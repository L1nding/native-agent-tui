"""Independent observer for exactly additive, source-bound component batches."""

import copy
import json


def verify_batch_geometry(original, current, proposal, allowed_sources):
    expected = copy.deepcopy(original)
    plan = proposal["component_plan"]
    placements = plan["components"]
    sources = [item["source_instance_id"] for item in placements]
    assert sources and len(set(sources)) == len(sources), "duplicate source instances"
    assert set(sources) <= set(allowed_sources), (
        "candidate includes another source category/state"
    )
    assert all(item["placement_basis"] == "source" for item in placements), (
        "CAD pose lock lost"
    )
    components = {item["component_id"] for item in placements}
    additions = {"solids": 0, "primitives": 0, "templates": 0}
    for operation in proposal["draft"]["operations"]:
        assert operation["op"] == "add", "component batch changed existing geometry"
        collection = operation["collection"]
        assert collection in additions, "unexpected component collection"
        if collection == "solids":
            assert any(
                operation["target_id"].startswith(identity + ".")
                for identity in components
            ), "solid escaped selected components"
            container = next(
                floor
                for floor in expected["floors"]
                if floor["id"] == operation["floor_id"]
            )
        else:
            container = expected
        value = json.loads(operation["value_json"])
        assert value["id"] == operation["target_id"], "operation identity mismatch"
        items = container.setdefault(collection, [])
        assert not any(item["id"] == value["id"] for item in items), (
            "add replaced existing identity"
        )
        metadata = value.setdefault("metadata", {})
        metadata.update(
            patch_id="patch." + proposal["task_id"],
            patch_reason=operation["reason"],
            value_basis=operation["basis"],
        )
        if operation.get("decision_id"):
            metadata["decision_id"] = operation["decision_id"]
        for source in operation["source_ids"]:
            if source not in value.setdefault("source_ids", []):
                value["source_ids"].append(source)
        items.append(value)
        additions[collection] += 1
    assert expected == current, (
        "authority differs from original plus the exact approved additions"
    )
    return {
        "instances": len(placements),
        "additions": additions,
        "original_geometry_preserved": True,
        "source_subset_verified": True,
    }
