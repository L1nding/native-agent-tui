"""Restore one already applied selected edit through actual TUI without replaying writes."""

import argparse
import hashlib
import json
import re
import sys
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def removed_source_refusal(fact):
    if fact.get("kind") != "diagnostic" or fact.get("ok") is not False:
        return False
    return any(
        diagnostic.get("code") == "source_removed_requires_reopen"
        or (
            diagnostic.get("code") == "project_rejected"
            and diagnostic.get("message", "").startswith(
                "source_removed_requires_reopen:"
            )
        )
        for diagnostic in fact.get("diagnostics", [])
    )


def current_image_receipts(facts, build_id, request_prefix):
    requests = {request_prefix + "-top", request_prefix + "-perspective"}
    return [
        digest
        for digest, fact in facts
        if (artifact := fact.get("artifact", {})).get("build_id") == build_id
        and (image := artifact.get("review_image", {})).get("delivery")
        == "attached_png"
        and image.get("delivery_request_id") in requests
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--model", default="hi/gpt-6-sol")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--review-missing-images", action="store_true")
    args = parser.parse_args()
    from domain_project_location_recovery_check import artifact_visible, core_panel
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    original = args.evidence.resolve()
    context = Path((original / "context-path.txt").read_text(encoding="utf-8").strip())
    expect(
        context.is_relative_to(root / "outputs/domain"),
        "context outside isolated outputs",
    )
    project = root / json.loads(context.read_bytes())["project_path"]
    expect(project.parent == root / "outputs", "requires isolated fixture")
    record = json.loads((context.parent / "project/scopes.json").read_bytes())[
        "scopes"
    ]["f08"]
    bound = json.loads((root / record["context_path"]).read_bytes())
    authority = root / bound["scene_path"]
    directory = root / "outputs/domain" / bound["run_id"] / "changes"
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    proposals = [
        json.loads(p.read_bytes())
        for p in directory.glob("*.json")
        if re.fullmatch(r"[a-f0-9]{32}", p.stem)
    ]
    edits = [
        p for p in proposals if p.get("component_edit") and p["state"] == "applied"
    ]
    expect(len(edits) == 1, "requires exactly one applied edit; recovery cannot write")
    edit = edits[0]
    initial = next(
        p for p in proposals if p.get("component_plan", {}).get("components")
    )
    intent = json.loads((original / "edit-intent.json").read_bytes())
    before_scene = json.loads((original / "before-edit.scene.json").read_bytes())
    scene = json.loads(authority.read_bytes())
    first = intent["plan"]["replacements"]["components"][0]["component_id"]
    removed = intent["plan"]["remove_component_ids"][0]
    old_parts = {p["id"]: p for p in before_scene["floors"][0]["solids"]}
    new_parts = {p["id"]: p for p in scene["floors"][0]["solids"]}
    untouched = next(
        p["metadata"]["component_id"]
        for p in old_parts.values()
        if p["metadata"]["component_id"] not in {first, removed}
    )
    expect(
        set(new_parts) == set(old_parts) - {removed + ".body"},
        "changed part IDs outside exact selected removal",
    )
    expect(
        new_parts[untouched + ".body"] == old_parts[untouched + ".body"],
        "unselected component changed",
    )
    expect(
        new_parts[first + ".body"]["origin"] == [5.4, 1.6, 0]
        and new_parts[first + ".body"]["arguments"] == {"h": 0.9},
        "movement/parameters not applied",
    )
    expect(
        sha(authority) == edit["candidate_sha256"],
        "authority differs from original approved candidate",
    )
    for key in ["walls", "slabs", "elements", "openings", "constraints"]:
        expect(
            scene["floors"][0][key] == before_scene["floors"][0][key],
            "protected geometry changed",
        )
    for key in ["primitives", "templates"]:
        new = {p["id"]: p for p in scene[key]}
        expect(
            all(new.get(p["id"]) == p for p in before_scene[key]),
            "existing shared definition changed",
        )
    inputs = json.loads((original / "input-hashes.json").read_bytes())
    expect(
        all(sha(Path(path)) == digest for path, digest in inputs.items()),
        "fixture source/business files changed",
    )
    expect(
        len(list(directory.glob("authorization-*.json"))) == 3, "unexpected grant count"
    )
    expect(
        len(list(directory.glob("operation-*.json"))) == 3, "unexpected operation count"
    )
    expect(
        all(
            sha(Path(path)) == digest
            for path, digest in edit["source_dependencies"].items()
        ),
        "original edit dependency changed",
    )
    builds = [
        json.loads(p.read_bytes())
        for p in (directory / "artifacts").glob("*/build.json")
    ]
    build = next(
        b
        for b in builds
        if b["proposal_id"] == edit["proposal_id"] and not b["candidate"]
    )
    expect(
        build["state"] == "built", "edited artifact not built; recovery cannot rebuild"
    )
    validation = json.loads(
        Path(build["output"]).with_suffix(".validation.json").read_bytes()
    )
    expect(validation["valid"], "actual Blender validation invalid")
    phases = {}
    for phase in ["built", "rebuilt", "glb_roundtrip"]:
        objects = validation[phase]["objects"]
        ids = [p["id"] for p in objects]
        expect(
            len(ids) == len(set(ids))
            and removed + ".body" not in ids
            and first + ".body" in ids,
            "mesh IDs disagree with selected removal",
        )
        shape = next(p for p in objects if p["id"] == first + ".body")
        expect(
            abs(shape["volume"] - 1.35) < 0.0001,
            "actual hollow-body parameter volume differs",
        )
        phases[phase] = sorted(ids)
    expect(
        phases["built"] == phases["rebuilt"] == phases["glb_roundtrip"],
        "mesh phase identity differs",
    )
    operation = next(
        json.loads(p.read_bytes())
        for p in directory.glob("operation-*.json")
        if json.loads(p.read_bytes())["proposal_id"] == edit["proposal_id"]
    )

    def business_hashes():
        paths = [context, root / record["context_path"], authority, *project.rglob("*")]
        paths += [
            p
            for p in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", p.stem)
        ]
        # Rendered PNG review cache is derived; pre-existing business artifacts remain byte-identical.
        paths += [
            p
            for p in (directory / "artifacts").rglob("*")
            if "review-images" not in p.parts
        ]
        return {str(p): sha(p) for p in paths if p.is_file()}

    evidence = repo / "target" / ("component-edit-recovery-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    before = business_hashes()
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    report = Report(evidence)
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(root),
            "--domain-context",
            str(context),
            "--profile",
            args.profile,
            "--model",
            args.model,
            "--permission",
            "read-only",
            "--journal-dir",
            str(evidence / "journal"),
        ],
        repo,
        evidence,
    )

    def send(prompt):
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("recovery task running", lambda s: " · Running · " in s, 60)
        rendered = tui.wait(
            "recovery task terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(
            " · Failed · " not in rendered,
            "read-only recovery reply failed; do not replay writes",
        )
        tui.wait(
            "private facts synced",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )

    def screen(name, predicate):
        tui.keys(CTRL_HOME)
        for _ in range(180):
            rendered = tui.console.stable()
            if predicate(core_panel(rendered)):
                (evidence / (name + ".txt")).write_text(rendered, encoding="utf-8")
                return
            tui.keys("\x1b[6~")
        raise AssertionError("missing Core panel " + name)

    try:
        tui.wait("recovery ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        send(
            "这是原隔离编辑的严格只读恢复。加载building-modeling技能，inspect_project，select_project_target target=八层，"
            "activate_project_floor floor_key=f08 selection_id="
            + selection["selection_id"]
            + "，inspect_change proposal_id="
            + edit["proposal_id"]
            + "。仅这4个领域工具，简洁报告原applied编辑与整组件删除及来源仍待确认。不得prepare/authorize/apply/build/render/shell或新业务身份。"
        )
        screen(
            "applied-edit-proposal",
            lambda s: "方案 " + edit["proposal_id"] in s and "applied" in s,
        )
        screen(
            "selected-edit-intent",
            lambda s: "已应用组件编辑" in s and "整组件删除 1" in s,
        )
        screen(
            "removed-source-disclosure", lambda s: "来源仍待确认" in s and removed in s
        )
        expect(business_hashes() == before, "proposal recovery modified business state")
        report.step(
            "fresh TUI restores original applied selected edit and exact removal disclosure",
            lambda: None,
            tui,
        )
        send(
            "继续严格只读：inspect_operation operation_id="
            + operation["operation_id"]
            + "；inspect_artifact build_id="
            + build["build_id"]
            + "；validate_artifact同build_id；scan_source_instances当前来源台账。仅4个工具，报告已建2/批准删除待确认1和技术检查，"
            "不prepare/authorize/apply/build/render/shell，不改变disposition。"
        )
        screen("built-artifact", lambda s: artifact_visible(s, build["build_id"]))
        screen(
            "validated-artifact", lambda s: artifact_visible(s, build["build_id"], True)
        )
        facts = [
            json.loads(p.read_bytes())
            for p in (context.parent / "project/facts").glob("*.json")
        ]
        ledger = max(
            (
                f
                for f in facts
                if f.get("kind") == "source_ledger"
                and f.get("scene_sha256") == sha(authority)
            ),
            key=lambda f: f["fact_sequence"],
        )
        states = {row["component_id"]: row["state"] for row in ledger["objects"]}
        expect(
            states
            == {first: "built", removed: "pending_confirmation", untouched: "built"},
            "source reconciliation falsely completed deletion",
        )
        marker = next(
            row["approved_removal"]
            for row in ledger["objects"]
            if row["component_id"] == removed
        )
        expect(
            marker["proposal_id"] == edit["proposal_id"]
            and marker["lineage"] == "confirmed",
            "wrong removal authority",
        )
        expect(
            business_hashes() == before,
            "artifact/ledger recovery modified business state",
        )
        report.step(
            "same old grant/operation/build and immutable source states restored with real Blender phase identities",
            lambda: None,
            tui,
        )
        cache = json.loads((directory / "source-scan-cache.json").read_bytes())["scan"]
        source = next(c for c in cache["candidates"] if c["component_id"] == removed)
        add = {
            "summary": "controlled removed-source guard test",
            "scan_id": intent["plan"]["replacements"]["scan_id"],
            "components": [
                {
                    "component_id": removed,
                    "template_id": initial["component_plan"]["templates"][0]["id"],
                    "origin": [2, 4.1, 0],
                    "source_ids": [source["source_id"]],
                    "source_instance_id": source["source_id"],
                    "reason": "negative test only",
                }
            ],
        }
        send(
            "已授权隔离负例：仅调用一次prepare_change plan="
            + json.dumps(add)
            + " request=证明删除来源守卫 expected_sha256="
            + sha(authority)
            + "。预期source_removed_requires_reopen拒绝；不得record_source_disposition、换身份、prepare_component_edit、apply/build/shell。"
            "然后inspect_change原edit proposal_id="
            + edit["proposal_id"]
            + "并简短报告拒绝，未知不能重试。"
        )
        facts = [
            json.loads(p.read_bytes())
            for p in (context.parent / "project/facts").glob("*.json")
        ]
        expect(
            any(removed_source_refusal(fact) for fact in facts),
            "actual guard refusal absent",
        )
        expect(business_hashes() == before, "guard refusal allocated business state")
        report.step(
            "actual TUI receives removed-source refusal and creates no new proposal/grant/operation/build",
            lambda: None,
            tui,
        )
        admission = None
        if args.review_missing_images:
            send(
                "只复核已有编辑产物：render_artifact_review build_id="
                + build["build_id"]
                + "，read_artifact_review_image读取实际top与perspective两张PNG；"
                "request_id分别="
                + evidence.name
                + "-top及"
                + evidence.name
                + "-perspective。简短报告实际看到的床数、移动带孔床与未覆盖区域，"
                "图片未准入不得声称看过。不得prepare/authorize/apply/build/shell；人工/合规仍not_run。"
            )
            from domain_project_images import admitted_images

            receipts = current_image_receipts(
                (
                    (p.stem, json.loads(p.read_bytes()))
                    for p in (context.parent / "project/facts").glob("*.json")
                ),
                build["build_id"],
                evidence.name,
            )
            expect(len(receipts) == 2, "exact two PNG deliveries missing")
            admission = admitted_images(evidence / "journal", receipts)
            (evidence / "image-admission.json").write_text(
                json.dumps(admission, indent=2), encoding="utf-8"
            )
            report.step(
                "actual TUI Agent reads same applied artifact top/perspective PNGs without rebuilding",
                lambda: None,
                tui,
            )
        expect(
            business_hashes() == before,
            "recovery changed original proposal/grant/operation/artifact hashes",
        )
        (evidence / "independent-check.json").write_text(
            json.dumps(
                {
                    "original_evidence": original.name,
                    "edit_proposal_id": edit["proposal_id"],
                    "operation_id": operation["operation_id"],
                    "authorization_id": operation["authorization_id"],
                    "build_id": build["build_id"],
                    "candidate_sha256": edit["candidate_sha256"],
                    "mesh_ids": phases,
                    "selected_body_volume_m3": 1.35,
                    "source_states": states,
                    "inputs_unchanged": True,
                    "business_hashes_unchanged": True,
                    "human_review": "not_run",
                    "images_admitted": admission is not None,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        report.step(
            "recovery TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "selected component read-only recovery",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
    print(evidence, flush=True)


if __name__ == "__main__":
    main()
