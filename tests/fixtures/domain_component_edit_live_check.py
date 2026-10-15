"""Actual dsh TUI Agent applies selected furniture movement, shape/parameters and removal.

Seeding defines isolated author inputs only. Observers read hashes, CAD/cache identities,
approved operations and actual Blender validations, never preparing or writing domain state.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def seed(project):
    import ezdxf
    from domain_project_live_check import seed as seed_project

    seed_project(project)
    source = project / "drawing.dxf"
    document = ezdxf.readfile(source)
    bed = document.blocks.new("TEST_BED")
    bed.add_lwpolyline(
        [(-450, -1000), (450, -1000), (450, 1000), (-450, 1000)], close=True
    )
    block = document.blocks["floors"]
    inserts = [
        block.add_blockref("TEST_BED", position, dxfattribs={"layer": "FURNITURE"})
        for position in [(2000, 1600), (2000, 4100), (5400, 4100)]
    ]
    document.saveas(source)
    (project / "project.yaml").write_text(
        "sources:\n  - path: drawing.dxf\ncomponent_sources:\n  layers: [FURNITURE]\n"
        "  rules:\n    - block: TEST_BED\n      category: bed\n"
        "      reason: authored isolated source-bound trial beds\n",
        encoding="utf-8",
    )
    (project / "edit-answer.json").write_text(
        json.dumps(
            {
                "instances": [
                    {
                        "handle": item.dxf.handle,
                        "position_mm": list(item.dxf.insert)[:2],
                    }
                    for item in inserts
                ],
                "source_xy_m": [0.9, 2.0],
                "height_status": "assumed",
                "edited_origin_m": [5.4, 1.6, 0],
                "edited_height_m": 0.9,
                "edited_volume_m3": 1.35,
            },
            indent=2,
        ),
        encoding="utf-8",
    )


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--timeout", type=int, default=1200)
    args = parser.parse_args()
    from domain_component_edit_recovery_check import removed_source_refusal
    from domain_project_location_recovery_check import artifact_visible, core_panel
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    token = uuid.uuid4().hex[:8]
    evidence = repo / "target" / ("component-edit-live-" + token)
    evidence.mkdir()
    project = root / "outputs" / ("component-edit-fixture-" + token)
    subprocess.run(
        [
            str(root / ".venv/Scripts/python.exe"),
            str(Path(__file__).resolve()),
            "--seed",
            str(project),
        ],
        cwd=root,
        check=True,
    )
    inputs = {str(p): sha(p) for p in project.rglob("*") if p.is_file()}
    (evidence / "input-hashes.json").write_text(
        json.dumps(inputs, indent=2), encoding="utf-8"
    )
    report = Report(evidence)
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(root),
            "--domain-project",
            str(project.relative_to(root)),
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
    context = None

    def send(prompt):
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("task running", lambda s: " · Running · " in s, 60)

    def finished():
        rendered = tui.wait(
            "task terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(
            " · Failed · " not in rendered,
            "Agent reply failed; inspect existing state, never replay writes",
        )
        tui.wait(
            "private facts synced",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )

    def wait(what, predicate):
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            try:
                if predicate():
                    return
            except (FileNotFoundError, KeyError, json.JSONDecodeError):
                pass
            expect(tui.exited() is None, "TUI exited during " + what)
            time.sleep(0.3)
        raise AssertionError("timeout " + what)

    def registry():
        path = context.parent / "project/scopes.json"
        return json.loads(path.read_bytes()) if path.exists() else {"scopes": {}}

    def child():
        record = registry()["scopes"]["f08"]
        bound = json.loads((root / record["context_path"]).read_bytes())
        return record, bound, root / "outputs/domain" / bound["run_id"] / "changes"

    def proposals():
        return [
            json.loads(p.read_bytes())
            for p in child()[2].glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}", p.stem)
        ]

    def screen(name, predicate):
        tui.keys(CTRL_HOME)
        for _ in range(160):
            rendered = tui.console.stable()
            if predicate(core_panel(rendered)):
                (evidence / (name + ".txt")).write_text(rendered, encoding="utf-8")
                return
            tui.keys("\x1b[6~")
        raise AssertionError("missing exact Core panel " + name)

    def approve(proposal, name):
        screen(
            name + "-before-approval",
            lambda s: (
                "方案 " + proposal["proposal_id"] in s
                and "修订 " + str(proposal["revision"]) in s
                and "prepared" in s
            ),
        )
        (evidence / (name + "-decision.json")).write_text(
            json.dumps(
                {
                    "actor": "coding-agent-user-authorized-test-delegate",
                    "proposal_id": proposal["proposal_id"],
                    "revision": proposal["revision"],
                    "prepare_token": proposal["prepare_token"],
                    "base_sha256": proposal["base_sha256"],
                    "candidate_sha256": proposal["candidate_sha256"],
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        tui.keys(CTRL_Y)
        wait(
            name + " applied",
            lambda: any(
                p["proposal_id"] == proposal["proposal_id"] and p["state"] == "applied"
                for p in proposals()
            ),
        )

    def build(build_id):
        path = child()[2] / "artifacts" / build_id / "build.json"
        wait(
            "terminal build " + build_id,
            lambda: (
                json.loads(path.read_bytes())["state"]
                in ["built", "failed", "cancelled", "unknown"]
            ),
        )
        result = json.loads(path.read_bytes())
        expect(result["state"] == "built", "Blender build failed")
        validation = json.loads(
            Path(result["output"]).with_suffix(".validation.json").read_bytes()
        )
        expect(validation["valid"], "Blender result invalid")
        return result, validation

    def business_hashes():
        _, bound, directory = child()
        files = [context, root / bound["scene_path"], *project.rglob("*")]
        files += [
            p
            for p in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", p.stem)
        ]
        files += list((directory / "artifacts").rglob("*"))
        return {str(p): sha(p) for p in files if p.is_file()}

    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        for path in root.glob("outputs/domain/project-*/context.json"):
            if Path(
                json.loads(path.read_bytes()).get("project_path", "")
            ) == project.relative_to(root):
                context = path
                break
        expect(context is not None, "project context missing")
        (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
        send(
            "本轮为已授权编码Agent代做决策的隔离测试。加载building-modeling技能，inspect_project，"
            "select_project_target target=八层，仅f08；prepare_project_floor用当前selection_id。"
            "只准备结构候选等待具体批准，不应用/构建。收到平台具体授权后仅apply_change operation_id="
            + evidence.name
            + "-structure，随后报告，暂不构建或准备家具。没有shell或旧workflow。"
        )
        wait("structure prepared", lambda: "f08" in registry()["scopes"])
        finished()
        structural = next(
            p
            for p in proposals()
            if p["proposal_id"] == child()[0]["bootstrap_proposal_id"]
        )
        expect(structural["state"] == "prepared", "structure blocked")
        approve(structural, "structure")
        finished()
        send(
            "同一隔离测试，scan_source_instances与describe_capabilities。仅FURNITURE内TEST_BED的3个来源，"
            "CAD宽0.9深2米，源中心分别(2,1.6),(2,4.1),(5.4,4.1)，全部示意高度0.6米明确assumed。"
            "prepare_change建立3实例，component_id和source_instance_id用实际扫描的稳定ID，scan_id用当前扫描。"
            "新primitive.acceptancebed：parameters w=0.9,d=2,h=0.6且parameter_basis全部assumed；body extrude "
            "profile rect width=w depth=d direction=[0,0,h]。template.acceptancebed category=bed，仅body部件，"
            "role=furniture，primitive_id=primitive.acceptancebed；无template参数。源姿态与source_ids保持。"
            "只准备具体提案等待批准。批准后apply_change operation_id="
            + evidence.name
            + "-initial，"
            "build_scene build_id="
            + evidence.name
            + "-initial-build，然后inspect_artifact等终态并validate_artifact。"
            "此轮不要批准、不要shell、不要旧workflow。"
        )
        wait(
            "three-bed proposal",
            lambda: any(
                p.get("component_plan", {}).get("components") for p in proposals()
            ),
        )
        finished()
        initial = next(
            p for p in proposals() if p.get("component_plan", {}).get("components")
        )
        expect(
            len(initial["component_plan"]["components"]) == 3,
            "initial source count differs",
        )
        approve(initial, "initial")
        initial_build, _initial_validation = build(evidence.name + "-initial-build")
        finished()
        _, bound, directory = child()
        authority = root / bound["scene_path"]
        original = json.loads(authority.read_bytes())
        (evidence / "before-edit.scene.json").write_bytes(authority.read_bytes())
        scan = json.loads((directory / "source-scan-cache.json").read_bytes())["scan"]
        candidates = sorted(
            scan["candidates"], key=lambda c: (c["origin_m"][0], c["origin_m"][1])
        )
        expect(len(candidates) == 3, "unexpected CAD source scope")
        first, removed, untouched = candidates
        replacement = {
            "summary": "source-bound hollow bed replacement",
            "scan_id": initial["component_plan"]["scan_id"],
            "primitives": [
                {
                    "id": "primitive.acceptanceframe",
                    "parameters": {"h": 0.8},
                    "parameter_basis": {"h": "assumed"},
                    "body": {
                        "kind": "extrude",
                        "profile": {
                            "kind": "polygon",
                            "outer": [[-0.45, -1], [0.45, -1], [0.45, 1], [-0.45, 1]],
                            "holes": [
                                [[-0.15, -0.5], [-0.15, 0.5], [0.15, 0.5], [0.15, -0.5]]
                            ],
                        },
                        "direction": [0, 0, "h"],
                    },
                }
            ],
            "templates": [
                {
                    "id": "template.acceptanceframe",
                    "category": "bed",
                    "parameters": {"height": 0.8},
                    "parameter_basis": {"height": "assumed"},
                    "parts": [
                        {
                            "name": "body",
                            "role": "furniture",
                            "primitive_id": "primitive.acceptanceframe",
                            "arguments": {"h": "height"},
                        }
                    ],
                }
            ],
            "components": [
                {
                    "component_id": first["component_id"],
                    "source_instance_id": first["source_id"],
                    "source_ids": [first["source_id"]],
                    "template_id": "template.acceptanceframe",
                    "origin": [5.4, 1.6, 0],
                    "placement_basis": "user_request",
                    "arguments": {"height": 0.9},
                    "reason": "explicit isolated user movement and hollow-body edit",
                }
            ],
        }
        plan = {
            "summary": "move and reshape first applied bed, remove second, preserve third",
            "selected_component_ids": [first["component_id"], removed["component_id"]],
            "replacements": replacement,
            "remove_component_ids": [removed["component_id"]],
        }
        feedback = {
            "feedback_id": evidence.name + "-edit-feedback",
            "action": "revise",
            "text": plan["summary"],
            "object_ids": plan["selected_component_ids"],
        }
        (evidence / "edit-intent.json").write_text(
            json.dumps({"plan": plan, "feedback": feedback}, indent=2), encoding="utf-8"
        )
        send(
            "继续已授权隔离测试，先inspect_existing_components核对当前完整组件和scene_sha256。"
            "明确用户编辑意图：第一张源床移至(5.4,1.6,0)，保留CAD XY，改为带真实内孔的hollow实体并参数高度0.9 assumed；"
            "第二张源床整组件删除，第三张及全部共享旧定义/结构不变。prepare_component_edit expected_sha256用当前权威，"
            "plan="
            + json.dumps(plan)
            + "，feedback="
            + json.dumps(feedback)
            + "，request="
            + plan["summary"]
            + "。仅准备提案，等待具体批准，不应用/构建。批准后apply_change operation_id="
            + evidence.name
            + "-edit，"
            "build_scene build_id="
            + evidence.name
            + "-edited-build，再inspect_artifact等终态、validate_artifact、scan_source_instances。"
            "删源必须保持pending_confirmation，不record_source_disposition、不重新增补它，不shell。"
        )
        wait("edit prepared", lambda: any(p.get("component_edit") for p in proposals()))
        finished()
        edited = next(p for p in proposals() if p.get("component_edit"))
        expect(edited["state"] == "prepared", "edit blocked: " + str(edited["issues"]))
        expect(
            json.loads(authority.read_bytes()) == original,
            "preparation changed authority",
        )
        screen(
            "selected-edit-intent",
            lambda s: "已应用组件编辑" in s and "整组件删除 1" in s,
        )
        screen(
            "removed-source-disclosure",
            lambda s: "来源仍待确认" in s and removed["component_id"] in s,
        )
        screen(
            "shape-parameter-disclosure",
            lambda s: "primitive.acceptanceframe" in s and "0.9" in s,
        )
        approve(edited, "edit")
        edited_build, edited_validation = build(evidence.name + "-edited-build")
        finished()
        after = json.loads(authority.read_bytes())
        before_parts = {p["id"]: p for p in original["floors"][0]["solids"]}
        after_parts = {p["id"]: p for p in after["floors"][0]["solids"]}
        changed_id = first["component_id"] + ".body"
        removed_id = removed["component_id"] + ".body"
        untouched_id = untouched["component_id"] + ".body"
        expect(
            set(after_parts) == set(before_parts) - {removed_id},
            "part IDs changed beyond selected removal",
        )
        expect(
            after_parts[untouched_id] == before_parts[untouched_id],
            "unselected component changed",
        )
        expect(
            after_parts[changed_id]["origin"] == [5.4, 1.6, 0]
            and after_parts[changed_id]["arguments"] == {"h": 0.9},
            "pose or parameter edit missing",
        )
        for field in ["walls", "slabs", "elements", "openings", "constraints"]:
            expect(
                after["floors"][0][field] == original["floors"][0][field],
                "protected collection changed: " + field,
            )
        for field in ["primitives", "templates"]:
            old = {item["id"]: item for item in original[field]}
            new = {item["id"]: item for item in after[field]}
            expect(
                all(new[key] == item for key, item in old.items()),
                "shared definition changed",
            )
        phases = {}
        for phase in ["built", "rebuilt", "glb_roundtrip"]:
            objects = edited_validation[phase]["objects"]
            ids = [obj["id"] for obj in objects]
            expect(
                len(ids) == len(set(ids))
                and removed_id not in ids
                and changed_id in ids
                and untouched_id in ids,
                "Blender identity mismatch",
            )
            changed = next(obj for obj in objects if obj["id"] == changed_id)
            expect(
                abs(changed["volume"] - 1.35) < 0.0001,
                "hollow shape/parameter volume not reflected",
            )
            phases[phase] = sorted(ids)
        expect(
            phases["built"] == phases["rebuilt"] == phases["glb_roundtrip"],
            "phase ID mismatch",
        )
        source_ledger = [
            json.loads(p.read_bytes())
            for p in (context.parent / "project/facts").glob("*.json")
        ]
        ledgers = [
            fact
            for fact in source_ledger
            if fact.get("kind") == "source_ledger"
            and fact.get("scene_sha256") == sha(authority)
        ]
        expect(ledgers, "current source ledger absent")
        ledger = max(ledgers, key=lambda f: f["fact_sequence"])
        states = {row["source_id"]: row["state"] for row in ledger["objects"]}
        expect(
            states
            == {
                first["source_id"]: "built",
                removed["source_id"]: "pending_confirmation",
                untouched["source_id"]: "built",
            },
            "removal source state misrepresented",
        )
        expect(
            all(sha(Path(path)) == digest for path, digest in inputs.items()),
            "CAD/business inputs changed",
        )
        expect(
            len(list(directory.glob("authorization-*.json"))) == 3
            and len(list(directory.glob("operation-*.json"))) == 3,
            "wrong exact approval/application counts",
        )
        screen(
            "edited-core-artifact",
            lambda s: artifact_visible(
                s, evidence.name + "-edited-build", validated=True
            ),
        )
        (evidence / "independent-check.json").write_text(
            json.dumps(
                {
                    "initial_proposal_id": initial["proposal_id"],
                    "edit_proposal_id": edited["proposal_id"],
                    "edit_operation_id": evidence.name + "-edit",
                    "initial_build_id": initial_build["build_id"],
                    "edited_build_id": edited_build["build_id"],
                    "edited_component_id": first["component_id"],
                    "removed_component_id": removed["component_id"],
                    "unselected_component_id": untouched["component_id"],
                    "selected_body_volume_m3": 1.35,
                    "mesh_ids": phases,
                    "source_states": states,
                    "source_inputs_unchanged": True,
                    "human_review": "not_run",
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        report.step(
            "actual TUI approves applied pose/shape/parameter replacement and complete removal with real Blender phases",
            lambda: None,
            tui,
        )
        before = business_hashes()
        restore = {
            "summary": "prove removed-source guard",
            "scan_id": replacement["scan_id"],
            "components": [
                {
                    "component_id": removed["component_id"],
                    "source_instance_id": removed["source_id"],
                    "source_ids": [removed["source_id"]],
                    "template_id": "template.acceptancebed",
                    "origin": [2, 4.1, 0],
                    "reason": "isolated explicit guard test only",
                }
            ],
        }
        send(
            "隔离负例：仅调用一次prepare_change plan="
            + json.dumps(restore)
            + " request=证明删除来源守卫 "
            "expected_sha256="
            + sha(authority)
            + "。预期source_removed_requires_reopen拒绝；不要record_source_disposition、"
            "不要换身份/绕过、不要apply/build/shell。然后inspect_change原edit proposal_id="
            + edited["proposal_id"]
            + "，简短报告拒绝。"
        )
        finished()
        expect(
            business_hashes() == before,
            "denied re-addition allocated or modified business state",
        )
        facts = [
            json.loads(p.read_bytes())
            for p in (context.parent / "project/facts").glob("*.json")
        ]
        expect(
            any(removed_source_refusal(fact) for fact in facts),
            "actual denied guard diagnostic missing",
        )
        report.step(
            "same TUI cannot silently rebuild deleted CAD source; no new proposal/grant/operation/build",
            lambda: None,
            tui,
        )
        send(
            "只复核已有编辑产物：render_artifact_review build_id="
            + evidence.name
            + "-edited-build，"
            "然后read_artifact_review_image实际读取top和perspective两张PNG，request_id分别="
            + evidence.name
            + "-top及"
            + evidence.name
            + "-perspective。简短报告实际观察到的两张床、"
            "移动后的带孔床、已删除对象不再出现及看不清的细节；没有实际图片不得说看过。"
            "不prepare/authorize/apply/build/shell；人工和合规仍not_run。"
        )
        finished()
        from domain_project_images import admitted_images

        image_facts = []
        image_receipts = []
        for path in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(path.read_bytes())
            artifact = fact.get("artifact", {})
            if (
                artifact.get("build_id") == evidence.name + "-edited-build"
                and artifact.get("review_image", {}).get("delivery") == "attached_png"
            ):
                image_facts.append(fact)
                image_receipts.append(path.stem)
        expect(len(image_facts) == 2, "actual two build-bound PNG deliveries missing")
        admission = admitted_images(evidence / "journal", image_receipts)
        (evidence / "image-admission.json").write_text(
            json.dumps(admission, indent=2), encoding="utf-8"
        )
        report.step(
            "same TUI Agent receives actual edited top/perspective PNGs and reports visual limits",
            lambda: None,
            tui,
        )
        report.step(
            "editing TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "applied furniture editing acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
    print(evidence, flush=True)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--seed":
        seed(Path(sys.argv[2]))
    else:
        main()
