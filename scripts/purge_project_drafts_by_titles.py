#!/usr/bin/env python3
"""
Delete GitHub Project draft items whose Title appears in a CSV.
- Safe by default: dry-run prints how many would be deleted.
- Use --delete to actually remove.

CSV must have a "Title" column.

Usage (preview only):
  python3 purge_project_drafts_by_titles.py --owner CoderByBlood --project-number 2 --csv whispercms_project_backlog.csv --dry-run

Delete them:
  python3 purge_project_drafts_by_titles.py --owner CoderByBlood --project-number 2 --csv whispercms_project_backlog.csv --delete
"""
import argparse, csv, json, subprocess, sys

def sh(cmd, check=True):
    res = subprocess.run(cmd, capture_output=True, text=True)
    if check and res.returncode != 0:
        raise RuntimeError(f"Command failed: {' '.join(cmd)}\nSTDERR:\n{res.stderr}")
    return res.stdout

def gql(query, **vars):
    args = []
    for k, v in vars.items():
        is_int = isinstance(v, int) or (isinstance(v, str) and v.isdigit())
        args += (["-F", f"{k}={v}"] if is_int else ["-f", f"{k}={v}"])
    return sh(["gh","api","graphql", *args, "-f", f"query={query}"])

def get_project(owner, number):
    q_org = """
    query($owner:String!, $number:Int!) {
      organization(login:$owner) { projectV2(number:$number) { id title number } }
    }"""
    data = json.loads(gql(q_org, owner=owner, number=number))
    proj = (data.get("data") or {}).get("organization", {}) or {}
    proj = proj.get("projectV2")
    if not proj:
        raise SystemExit("Could not find project. Check owner/number and permissions.")
    return proj["id"], proj["title"]

def list_items(project_id):
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
                ... on Issue { title url }
                ... on PullRequest { title url }
              }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
      }
    }"""
    items = []
    after = None
    while True:
        if after is None:
            data = json.loads(gql(q, id=project_id))
        else:
            data = json.loads(gql(q, id=project_id, after=after))
        page = data["data"]["node"]["items"]
        for n in page["nodes"]:
            c = n.get("content") or {}
            t = None
            if c.get("__typename") == "DraftIssue":
                t = c.get("title")
            elif c.get("__typename") in ("Issue","PullRequest"):
                t = c.get("title")
            items.append({"id": n["id"], "type": n["type"], "title": t})
        if not page["pageInfo"]["hasNextPage"]:
            break
        after = page["pageInfo"]["endCursor"]
    return items

def delete_item(project_id, item_id):
    m = """
    mutation($projectId:ID!, $itemId:ID!) {
      deleteProjectV2Item(input:{projectId:$projectId, itemId:$itemId}) { deletedItemId }
    }"""
    gql(m, projectId=project_id, itemId=item_id)

def main():
    ap = argparse.ArgumentParser(description="Delete Project draft items whose Title appears in CSV.")
    ap.add_argument("--owner", required=True)
    ap.add_argument("--project-number", type=int, required=True)
    ap.add_argument("--csv", required=True)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--delete", action="store_true")
    args = ap.parse_args()

    project_id, title = get_project(args.owner, args.project_number)
    print(f"Project: {title} ({project_id})")

    # titles from CSV
    wanted = set()
    with open(args.csv, newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            t = (row.get("Title") or "").strip()
            if t:
                wanted.add(t)

    items = list_items(project_id)
    to_delete = [it for it in items if it["type"] == "DRAFT_ISSUE" and it["title"] in wanted]

    print(f"Found {len(to_delete)} draft items matching CSV titles.")
    for it in to_delete[:12]:
        print(f"  - {it['title']} ({it['id']})")
    if len(to_delete) > 12:
        print(f"  … and {len(to_delete)-12} more")

    if args.delete:
        for it in to_delete:
            delete_item(project_id, it["id"])
        print(f"Deleted {len(to_delete)} draft items.")
    else:
        print("Dry-run only. Re-run with --delete to actually remove them.")

if __name__ == "__main__":
    main()