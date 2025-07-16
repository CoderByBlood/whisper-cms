#!/usr/bin/env python3

import os
import sys
import json

def collect_elements_from_element(element, element_map):
    eid = element.get("id")
    name = element.get("name")
    if eid and name:
        element_map[eid] = name

    for container in element.get("containers", []):
        collect_elements_from_element(container, element_map)
    for component in element.get("components", []):
        collect_elements_from_element(component, element_map)

def collect_relationships_from_element(element, relationship_map):
    for rel in element.get("relationships", []):
        rel_id = rel.get("id")
        if rel_id:
            relationship_map[rel_id] = rel

    for container in element.get("containers", []):
        collect_relationships_from_element(container, relationship_map)
    for component in element.get("components", []):
        collect_relationships_from_element(component, relationship_map)

def main():
    if len(sys.argv) != 3:
        print("Usage: python generate-sequence-diagrams.py <workspace.json> <outputDir>")
        sys.exit(1)

    workspace_json_file = sys.argv[1]
    output_dir = sys.argv[2]
    os.makedirs(output_dir, exist_ok=True)

    with open(workspace_json_file) as f:
        workspace = json.load(f)

    # Build element ID -> Name map
    element_id_to_name = {}
    for ss in workspace.get("model", {}).get("softwareSystems", []):
        collect_elements_from_element(ss, element_id_to_name)
    for person in workspace.get("model", {}).get("people", []):
        collect_elements_from_element(person, element_id_to_name)

    # Build relationship ID -> Relationship object map
    relationship_map = {}
    for ss in workspace.get("model", {}).get("softwareSystems", []):
        collect_relationships_from_element(ss, relationship_map)
    for person in workspace.get("model", {}).get("people", []):
        collect_relationships_from_element(person, relationship_map)

    # Process dynamic views
    dynamic_views = workspace.get("views", {}).get("dynamicViews", [])
    if not dynamic_views:
        print("No dynamic views found in workspace.")
        return

    for view in dynamic_views:
        key = view.get("key", "")
        name = view.get("name", key)

        if not key.endswith("-Sequence"):
            continue

        print(f"Generating sequence diagram for: {key}")

        # Build participants in order of first appearance in messages
        ordered_participants = []
        seen = set()

        relationships_in_order = sorted(
            view.get("relationships", []),
            key=lambda r: int(r.get("order", "0"))
        )
        for rel in relationships_in_order:
            rel_id = rel.get("id")
            if not rel_id:
                continue
            model_rel = relationship_map.get(rel_id)
            if not model_rel:
                continue
            src = model_rel.get("sourceId")
            dst = model_rel.get("destinationId")
            for p in [src, dst]:
                if p and p not in seen:
                    seen.add(p)
                    ordered_participants.append(p)

        # NEW: Prefix output file with UML-
        output_path = os.path.join(output_dir, f"UML-{key}.puml")
        with open(output_path, "w") as out:
            out.write("@startuml\n")
            out.write(f"title {name}\n\n")

            # Write participants in order of appearance
            for p in ordered_participants:
                label = element_id_to_name.get(p, p)
                if label and label[0].islower():
                    out.write(f"actor \"{label}\" as {p}\n")
                else:
                    out.write(f"participant \"{label}\" as {p}\n")

            out.write("\n")

            # Messages in order
            for rel in relationships_in_order:
                rel_id = rel.get("id")
                if not rel_id:
                    continue
                model_rel = relationship_map.get(rel_id)
                if not model_rel:
                    continue
                src = model_rel.get("sourceId")
                dst = model_rel.get("destinationId")
                if not src or not dst:
                    continue
                desc = rel.get("description", "").replace("\n", " ")
                is_response = rel.get("response", False)
                if is_response:
                    out.write(f"{dst} -> {src} : {desc}\n")
                else:
                    out.write(f"{src} -> {dst} : {desc}\n")

            out.write("@enduml\n")

        print(f"✅ Wrote {output_path}")

    print("✅ All sequence diagrams generated.")

if __name__ == "__main__":
    main()