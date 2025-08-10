#!/usr/bin/env python3
"""
Import CSV rows as GitHub Projects (v2) draft items.
- Idempotent: if a draft with the same Title already exists, it updates fields instead of creating a duplicate.
- Requires GitHub CLI: gh auth login && gh auth refresh -s project,repo

CSV columns required: Title, Body, Epic, MVP, Priority, Status
Project fields expected:
  Epic (TEXT)
  MVP (SINGLE_SELECT: e.g., Yes/No)
  Priority (SINGLE_SELECT: e.g., P0/P1/P2)
  Status (SINGLE_SELECT: e.g., Backlog/Ready/In progress/In review/Done)

Usage (dry run first):
  python3 import_to_projects.py --owner CoderByBlood --project-number 2 --csv whispercms_project_backlog.csv --dry-run

Then actually import:
  python3 import_to_projects.py --owner CoderByBlood --project-number 2 --csv whispercms_project_backlog.csv

Optionally update Body for existing drafts:
  python3 import_to_projects.py --owner CoderByBlood --project-number 2 --csv whispercms_project_backlog.csv --update-body
"""
import argparse, csv, json, subprocess, sys, time
from pathlib import Path

# ----------------- utils -----------------
def sh(cmd, input=None, check=True):
    res = subprocess.run(cmd, input=input, capture_output=True, text=True)
    if check and res.returncode != 0:
        raise RuntimeError(f"Command failed: {' '.join(cmd)}\nSTDERR:\n{res.stderr}")
    return res.stdout

def gql(query, **vars):
    args = []
    for k, v in vars.items():
        is_int = isinstance(v, int) or (isinstance(v, str) and v.isdigit())
        args += (["-F", f"{k}={v}"] if is_int else ["-f", f"{k}={v}"])
    return sh(["gh","api","graphql", *args, "-f", f"query={query}"])

# ----------------- lookups -----------------
def get_project(owner, number):
    q_org = """
    query($owner:String!, $number:Int!) {
      organization(login:$owner) { projectV2(number:$number) { id title number } }
    }"""
    q_user = """
    query($owner:String!, $number:Int!) {
      user(login:$owner) { projectV2(number:$number) { id title number } }
    }"""
    # try org
    data = json.loads(gql(q_org, owner=owner, number=number))
    proj = (data.get("data") or {}).get("organization", {}) or {}
    proj = proj.get("projectV2")
    # fallback to user
    if not proj:
        data = json.loads(gql(q_user, owner=owner, number=number))
        proj = (data.get("data") or {}).get("user", {}) or {}
        proj = proj.get("projectV2")
    if not proj:
        raise SystemExit("Could not find project. Check owner/number and permissions.")
    return proj["id"], proj["title"]

def get_fields(project_id):
    q = """
    query($id:ID!) {
      node(id:$id) {
        ... on ProjectV2 {
          fields(first:100) {
            nodes {
              __typename
              ... on ProjectV2Field { id name dataType }
              ... on ProjectV2SingleSelectField { id name dataType options { id name } }
              ... on ProjectV2IterationField { id name dataType configuration { iterations { id title } } }
            }
          }
        }
      }
    }"""
    data = json.loads(gql(q, id=project_id))
    nodes = data["data"]["node"]["fields"]["nodes"]
    return { n["name"]: n for n in nodes if "name" in n }

def list_existing_draft_items_by_title(project_id):
    """Return {title: item_id} for DraftIssue items."""
    def page(after=None):
        q = """
        query($id:ID!, $after:String) {
          node(id:$id) {
            ... on ProjectV2 {
              items(first:100, after:$after) {
                nodes {
                  id
                  type
                  content {
                    __typename
                    ... on DraftIssue { title }
                    ... on Issue { title }
                    ... on PullRequest { title }
                  }
                }
                pageInfo { hasNextPage endCursor }
              }
            }
          }
        }"""
        if after is None:
            return json.loads(gql(q, id=project_id))
        else:
            return json.loads(gql(q, id=project_id, after=after))

    out = {}
    after = None
    while True:
        data = page(after)
        items = data["data"]["node"]["items"]
        for n in items["nodes"]:
            c = n.get("content") or {}
            if c.get("__typename") == "DraftIssue":
                t = (c.get("title") or "").strip()
                if t and t not in out:
                    out[t] = n["id"]
        if not items["pageInfo"]["hasNextPage"]:
            break
        after = items["pageInfo"]["endCursor"]
    return out

# ----------------- item ops -----------------
def item_create(owner, project_number, title, body):
    out = sh([
        "gh","project","item-create",str(project_number),
        "--owner",owner,"--title",title,"--body",body,"--format","json"
    ])
    return json.loads(out)["id"]

def item_edit_body(item_id, project_id, body):
    sh(["gh","project","item-edit","--id",item_id,"--project-id",project_id,"--body",body])

def set_text(item_id, project_id, field_id, text):
    sh(["gh","project","item-edit","--id",item_id,"--project-id",project_id,
        "--field-id",field_id,"--text",text])

def set_single_select(item_id, project_id, field_node, value):
    opts = { o["name"]: o["id"] for o in field_node.get("options", []) }
    if not opts:
        raise RuntimeError(f"Field '{field_node['name']}' has no options.")
    option_name = value if value in opts else {k.lower():k for k in opts}.get(value.lower())
    if not option_name:
        raise RuntimeError(
            f"Value '{value}' not valid for field '{field_node['name']}'. Options: {list(opts.keys())}"
        )
    option_id = opts[option_name]
    sh([
        "gh","project","item-edit",
        "--id", item_id,
        "--project-id", project_id,
        "--field-id", field_node["id"],
        "--single-select-option-id", option_id
    ])

# ----------------- main -----------------
def main():
    ap = argparse.ArgumentParser(description="Import CSV as GitHub Project draft items (idempotent upsert).")
    ap.add_argument("--owner", required=True, help="Org or user owner (e.g., CoderByBlood)")
    ap.add_argument("--project-number", required=True, type=int)
    ap.add_argument("--csv", required=True, type=Path)
    ap.add_argument("--sleep", type=float, default=0.25, help="Delay between items (API friendly)")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--update-body", action="store_true", help="Update Body when Title already exists")
    args = ap.parse_args()

    project_id, proj_title = get_project(args.owner, args.project_number)
    print(f"Project: {proj_title} (id={project_id})")

    fields = get_fields(project_id)
    needed = ["Epic","MVP","Priority","Status"]
    missing = [n for n in needed if n not in fields]
    if missing:
        print(f"ERROR: Missing fields in project: {missing}")
        sys.exit(1)

    if fields["Epic"]["dataType"] != "TEXT":
        print("WARNING: Field 'Epic' should be TEXT.")
    for name in ["MVP","Priority","Status"]:
        if fields[name]["dataType"] != "SINGLE_SELECT":
            print(f"WARNING: Field '{name}' should be SINGLE_SELECT.")

    existing = list_existing_draft_items_by_title(project_id)
    print(f"Found {len(existing)} existing draft items by Title.")

    created = updated = 0
    with args.csv.open(newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for i, r in enumerate(reader, start=2):  # start=2 accounts for CSV header row
            title = (r.get("Title") or "").strip()
            body = (r.get("Body") or "").strip()
            epic = (r.get("Epic") or "").strip()
            mvp = (r.get("MVP") or "").strip()
            priority = (r.get("Priority") or "").strip()
            status = (r.get("Status") or "").strip()
            if not title:
                print(f"Row {i}: Skipping empty Title")
                continue

            upserting = "Updating" if title in existing else "Creating"
            print(f"• {upserting}: {title}")
            if args.dry_run:
                print(f"  (dry-run) Epic='{epic}', MVP='{mvp}', Priority='{priority}', Status='{status}'")
                continue

            try:
                if title in existing:
                    item_id = existing[title]
                    # update fields
                    set_text(item_id, project_id, fields["Epic"]["id"], epic)
                    set_single_select(item_id, project_id, fields["MVP"], mvp)
                    set_single_select(item_id, project_id, fields["Priority"], priority)
                    set_single_select(item_id, project_id, fields["Status"], status)
                    if args.update_body and body:
                        item_edit_body(item_id, project_id, body)
                    updated += 1
                else:
                    # create then set fields
                    item_id = item_create(args.owner, args.project_number, title, body)
                    set_text(item_id, project_id, fields["Epic"]["id"], epic)
                    set_single_select(item_id, project_id, fields["MVP"], mvp)
                    set_single_select(item_id, project_id, fields["Priority"], priority)
                    set_single_select(item_id, project_id, fields["Status"], status)
                    created += 1
                time.sleep(args.sleep)
            except Exception as e:
                print(f"  Row {i} ERROR: {e}")

    print(f"Done. Created {created}, Updated {updated} items.")

if __name__ == "__main__":
    main()