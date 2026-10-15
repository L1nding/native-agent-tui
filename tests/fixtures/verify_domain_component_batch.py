"""Independently verify an already finished real TUI batch; never execute domain writes."""

import argparse
import hashlib
import json
from pathlib import Path

from domain_project_component_geometry import verify_batch_geometry
from domain_project_images import admitted_images


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(evidence, assessments):
    context = Path((evidence / "context-path.txt").read_text().strip())
    root = Path(json.loads(context.read_bytes())["project_root"])
    registry = json.loads((context.parent / "project/scopes.json").read_bytes())
    decision = json.loads((evidence / "test-decision.json").read_bytes())
    scope = registry["scopes"][registry["active"]]
    child = json.loads((root / scope["context_path"]).read_bytes())
    directory = root / "outputs/domain" / child["run_id"] / "changes"
    proposal = json.loads(
        (directory / (decision["proposal_id"] + ".json")).read_bytes()
    )
    for key in [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
    ]:
        assert proposal[key] == decision[key], "approved proposal binding changed"
    assert proposal["state"] == "applied", "candidate has not been applied"
    original = json.loads((evidence / "before-scene.json").read_bytes())
    scene_path = root / child["scene_path"]
    current = json.loads(scene_path.read_bytes())
    assert sha(scene_path) == proposal["candidate_sha256"], (
        "authority differs from approved candidate"
    )
    groups_before = json.loads((assessments / "assessed-groups.json").read_bytes())
    categories = (
        {"chair", "basin"}
        if decision["category"] == "remaining"
        else {decision["category"]}
    )
    allowed = {
        i["source_id"]
        for g in groups_before["groups"]
        if g.get("assessment", {}).get("category") in categories
        for i in g["instances"]
        if i["state"] == "pending_addition"
    }
    geometry = verify_batch_geometry(original, current, proposal, allowed)
    baseline = json.loads((evidence / "before-hashes.json").read_bytes())
    assert all(sha(Path(path)) == digest for path, digest in baseline.items()), (
        "original business file changed"
    )
    # The original assessment recovery bound the interpretation file before this batch.
    assessment_baseline = json.loads((assessments / "before-hashes.json").read_bytes())
    for path, digest in assessment_baseline.items():
        if Path(path).name in {
            "source-interpretations.json",
            "source-dispositions.json",
        }:
            assert sha(Path(path)) == digest, (
                "batch rewrote source assessment/disposition"
            )
    grants = [
        json.loads(path.read_bytes())
        for path in directory.glob("authorization-*.json")
        if str(path) not in baseline
    ]
    assert len(grants) == 1, "unexpected additional grants"
    assert all(
        grants[0][key] == decision[key]
        for key in [
            "proposal_id",
            "revision",
            "prepare_token",
            "base_sha256",
            "candidate_sha256",
        ]
    )
    operations = [
        json.loads(path.read_bytes())
        for path in directory.glob("operation-*.json")
        if str(path) not in baseline
    ]
    assert len(operations) == 1 and operations[0]["state"] == "applied", (
        "unexpected apply operations"
    )
    jobs = [
        json.loads(path.read_bytes())
        for path in directory.glob("artifacts/*/build.json")
        if str(path) not in baseline
    ]
    built = [
        job
        for job in jobs
        if job["proposal_id"] == proposal["proposal_id"] and job["state"] == "built"
    ]
    assert built, "actual built artifact missing"
    build_results = []
    # This reads the public deterministic build-plan seam; no Blender or writer is invoked.
    from blender_agent.modeling.plan import create_build_plan
    from blender_agent.scene_ir.io import load_scene

    expected_ids = {obj.id for obj in create_build_plan(load_scene(scene_path)).objects}
    for job in built:
        output = Path(job["output"])
        validation = json.loads(output.with_suffix(".validation.json").read_bytes())
        assert validation["valid"] is True
        phase_counts = {}
        for phase in ["built", "rebuilt", "glb_roundtrip"]:
            actual = [item["id"] for item in validation[phase]["objects"]]
            assert validation[phase]["valid"] is True
            assert len(actual) == len(set(actual)) and set(actual) == expected_ids, (
                "actual mesh identity mismatch"
            )
            phase_counts[phase] = len(actual)
        build_results.append(
            {"build_id": job["build_id"], "phase_mesh_counts": phase_counts}
        )
    facts = [
        (path, json.loads(path.read_bytes()))
        for path in (context.parent / "project/facts").glob("*.json")
    ]
    groups = [
        value
        for _, value in facts
        if value.get("kind") == "source_groups"
        and value.get("scene_sha256") == proposal["candidate_sha256"]
    ]
    assert groups, "post-application source reconciliation missing"
    final = max(groups, key=lambda value: value["fact_sequence"])
    assert (
        final["total_objects"] == 487
        and final["total_groups"] == 16
        and final["assessed_groups"] == 16
    )
    previous_sources = {
        item["metadata"]["source_instance_id"]
        for floor in original["floors"]
        for collection in floor.values()
        if isinstance(collection, list)
        for item in collection
        if isinstance(item, dict) and item.get("metadata", {}).get("source_instance_id")
    }
    selected_sources = {
        item["source_instance_id"] for item in proposal["component_plan"]["components"]
    }
    actual_built_sources = {
        item["source_id"]
        for group in final["groups"]
        for item in group["instances"]
        if item["state"] == "built"
    }
    assert actual_built_sources == previous_sources | selected_sources, (
        "source built identities differ from previous scene plus this batch"
    )
    assert final["state_counts"].get("built") == len(actual_built_sources), (
        "source built count mismatch"
    )
    images = [
        (path.stem, value["artifact"]["review_image"])
        for path, value in facts
        if value.get("artifact", {}).get("review_image", {}).get("delivery_request_id")
        == evidence.name
        and value["artifact"]["review_image"].get("delivery") == "attached_png"
    ]
    assert {image["request"]["view"] for _, image in images} >= {
        "top",
        "perspective",
    }, "both actual review views required"
    admission = admitted_images(
        evidence / "journal", [receipt for receipt, _ in images]
    )
    return {
        "status": "PASS",
        "proposal_id": proposal["proposal_id"],
        "geometry": geometry,
        "builds": build_results,
        "source_state_counts": final["state_counts"],
        "image_admission": admission,
        "human_review": "not_run",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--assessments", type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.evidence.resolve(), args.assessments.resolve())
    (args.evidence / "independent-batch-verification.json").write_text(
        json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(
        json.dumps(
            {key: value for key, value in result.items() if key != "image_admission"},
            ensure_ascii=False,
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
