"""Real project-mode stair proposal, exact TUI approval, build, report and visual review."""

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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--timeout", type=int, default=1200)
    parser.add_argument(
        "--resume-prepared",
        action="store_true",
        help="recover the one original unapproved connection scope, without prepare",
    )
    args = parser.parse_args()
    from domain_project_images import admitted_images
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    repo, root = Path(__file__).resolve().parents[2], args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    project = root / json.loads(context.read_bytes())["project_path"]
    expect(project.parent == root / "outputs", "isolated project required")
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    base_path = context.parent / "project/building/building-assembled-01/build.json"
    base = json.loads(base_path.read_bytes())
    base_scene = json.loads((base_path.parent / "input.scene.json").read_bytes())
    inventory = json.loads((base_path.parent / "connection-sources.json").read_bytes())
    expect(
        len(inventory) == 1 and inventory[0]["category"] == "stair",
        "source stair missing",
    )
    source = inventory[0]
    lower, upper = sorted(base_scene["floors"], key=lambda floor: floor["elevation"])
    expect(
        len(upper["slabs"][0]["holes"]) == 1,
        "existing CAD-derived upper opening missing",
    )
    protected_files = [context, *project.rglob("*"), *base_path.parent.rglob("*")]
    for scope in registry["scopes"].values():
        child_context = root / scope["context_path"]
        child = json.loads(child_context.read_bytes())
        changes = root / "outputs/domain" / child["run_id"] / "changes"
        protected_files.extend([child_context, root / child["scene_path"]])
        protected_files.extend(
            path
            for path in changes.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
        )
        protected_files.extend(changes.glob("artifacts/*/build.json"))
    before = {str(path): sha(path) for path in protected_files if path.is_file()}
    evidence = repo / "target" / ("project-connection-live-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    build_id = evidence.name + "-building"
    spec = {
        "component_id": source["component_id"],
        "source_instance_id": source["source_id"],
        "kind": "stair",
        "served_floors": [lower["id"], upper["id"]],
        "openings": [
            {
                "floor_id": upper["id"],
                "slab_id": upper["slabs"][0]["id"],
                "hole_index": 0,
            }
        ],
        "steps": 16,
        "riser_m": 0.2,
        "dimension_basis": "assumed",
        "reason": "user-authorized isolated source staircase; 3.2m rise, existing CAD hole",
    }
    (evidence / "test-connection-spec.json").write_text(
        json.dumps(spec, indent=2), encoding="utf-8"
    )
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
    report = Report(evidence)

    def send(text):
        tui.keys(CTRL_U)
        tui.type(text)
        tui.keys(ENTER)
        tui.wait("connection turn running", lambda s: " · Running · " in s, 60)

    def finished():
        tui.wait(
            "connection turn terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "connection Agent failed")

    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        prompt = (
            "编码Agent受用户委托作隔离项目跨层验收决定，不是建筑合规或医院人工确认。加载building-modeling技能。"
            "inspect_project，select_project_target恢复target="
            + selection["target"]
            + "、choices=[f09.b]。"
            "先inspect_project_building与validate_project_building原build_id=building-assembled-01，必须built/valid/current。"
            "本批只在楼栋连接作用域增加一处CAD来源楼梯，所有原楼层、上层和楼板只读；已有孔洞不能编辑。"
            "prepare_project_connection_change build_id=building-assembled-01、lower_floor_key=f08、selection_id="
            + selection["selection_id"]
            + "、expected_input_binding="
            + base["input_binding"]
            + "，plan={summary:'CAD来源跨层楼梯，原楼层与楼板只读',connections:["
            + json.dumps(spec)
            + "]}，其他候选列表空。"
            "返回连接scope后describe_capabilities、scan_source_instances核对原来源身份，inspect_change完整读取具体方案和假设。"
            "请核对16步*0.2m=3.2m与上下标高，不推断栏杆、净高、荷载或规范；参数维持assumed。"
            "只准备展示后等待TUI具体Ctrl+Y，不提前apply/build/authorize，不shell或委派。"
            "获具体批准后apply_change沿精确授权，build_scene新build_id="
            + evidence.name
            + "-artifact，"
            "inspect_artifact等待并validate_artifact；再assemble_project_building selection_id用原值、build_id="
            + build_id
            + "、connection_scope_id用这次scope_id。inspect/validate新楼栋并check_project_building_connections、"
            "read_project_building_report。然后render_project_building_review perspective和top，"
            "read_project_building_review_image读取两张实际图，request_id="
            + evidence.name
            + "。报告只证明source-bound massing与已有孔洞几何，人工与规范未执行。"
        )
        if args.resume_prepared:
            existing = list(registry.get("connection_scopes", {}).values())
            expect(len(existing) == 1, "one original unapproved scope required")
            old_scope = existing[0]
            prefix = (
                "只读恢复前次因界面分页夹具中止的同一连接候选，禁止prepare或另建提案。加载building-modeling。"
                "inspect_project、select_project_target target=整栋、choices=[f09.b]恢复原选择；"
                "inspect_project_building和validate_project_building原base build_id=building-assembled-01；"
                "inspect_project_connection_scope scope_id="
                + old_scope["scope_id"]
                + "、selection_id="
                + selection["selection_id"]
                + "，describe_capabilities和inspect_change原proposal_id="
                + old_scope["bootstrap_proposal_id"]
                + "。读取全部16级楼梯操作、来源／上层孔洞和assumed参数，"
                "保持原版本，等待本TUI单独Ctrl+Y；此前无授权无应用。不要shell或委派。"
            )
            prompt = prefix + prompt[prompt.index("获具体批准后") :]
        send(prompt)
        finished()
        registry = json.loads(registry_path.read_bytes())
        records = list(registry.get("connection_scopes", {}).values())
        expect(len(records) == 1, "exactly one connection scope expected")
        scoped = records[0]
        child_context = root / scoped["context_path"]
        child = json.loads(child_context.read_bytes())
        directory = root / "outputs/domain" / child["run_id"] / "changes"
        proposal = json.loads(
            (directory / (scoped["bootstrap_proposal_id"] + ".json")).read_bytes()
        )
        expect(proposal["state"] == "prepared", "connection proposal blocked")
        expect(
            not list(directory.glob("authorization-*.json")),
            "grant before exact approval",
        )
        expect(
            sha(root / child["scene_path"]) == base["input_sha256"],
            "connection authority mutated before approval",
        )
        expect(
            all(sha(Path(path)) == digest for path, digest in before.items()),
            "preparation changed original inputs",
        )
        tui.wait(
            "connection scope synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        binding_screen = None
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if "楼栋连接作用域" in screen:
                binding_screen = screen
                break
            tui.keys("\x1b[6~")
        expect(
            binding_screen is not None,
            "Core did not display project connection binding",
        )
        (evidence / "scope-binding-screen.txt").write_text(
            binding_screen, encoding="utf-8"
        )
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if (
                "方案 " + proposal["proposal_id"] in screen
                and "修订 " + str(proposal["revision"]) in screen
            ):
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError(
                "Core did not display exact project connection candidate"
            )
        (evidence / "before-approval.txt").write_text(screen, encoding="utf-8")
        (evidence / "connection-scope.json").write_text(
            json.dumps(scoped, indent=2), encoding="utf-8"
        )
        decision = {
            key: proposal[key]
            for key in [
                "proposal_id",
                "revision",
                "prepare_token",
                "base_sha256",
                "candidate_sha256",
            ]
        }
        decision.update(
            actor="coding_agent_as_user_authorized_test_proxy", human_confirmation=False
        )
        (evidence / "test-decision.json").write_text(
            json.dumps(decision, indent=2), encoding="utf-8"
        )
        report.step(
            "exact source stair candidate and read-only upper reference displayed by Core",
            lambda: None,
            tui,
        )
        tui.keys(CTRL_Y)
        tui.wait("approved connection running", lambda s: " · Running · " in s, 60)
        finished()
        expect(
            sha(root / child["scene_path"]) == proposal["candidate_sha256"],
            "authority differs from approved candidate",
        )
        expect(
            all(sha(Path(path)) == digest for path, digest in before.items()),
            "connection execution changed original floor or building",
        )
        grants = list(directory.glob("authorization-*.json"))
        operations = list(directory.glob("operation-*.json"))
        expect(
            len(grants) == len(operations) == 1,
            "connection grant/operation count mismatch",
        )
        current_scene = json.loads((root / child["scene_path"]).read_bytes())
        expect(
            current_scene["floors"][1] == base_scene["floors"][1], "upper floor changed"
        )
        for name, value in base_scene["floors"][0].items():
            if name != "elements":
                expect(
                    current_scene["floors"][0][name] == value,
                    "original lower geometry changed: " + name,
                )
        old_elements = {
            value["id"]: value for value in base_scene["floors"][0]["elements"]
        }
        new_elements = {
            value["id"]: value for value in current_scene["floors"][0]["elements"]
        }
        expect(
            all(new_elements[key] == value for key, value in old_elements.items()),
            "old lower elements changed",
        )
        expected_parts = {
            spec["component_id"] + ".step." + str(index).zfill(2)
            for index in range(1, 17)
        }
        expect(
            set(new_elements) - set(old_elements) == expected_parts,
            "stair part identities differ",
        )
        final_path = context.parent / "project/building" / build_id / "build.json"
        built = json.loads(final_path.read_bytes())
        expect(built["state"] == "built", "connected building not built")
        expect(
            built["inputs"]["connection_authority"]["scope_id"] == scoped["scope_id"],
            "building omitted approved connection authority",
        )
        expect(
            sha(final_path.parent / "input.scene.json") == proposal["candidate_sha256"],
            "building snapshot differs from authority",
        )
        from blender_agent.modeling.plan import create_build_plan
        from blender_agent.scene_ir.io import load_scene

        expected_meshes = {
            obj.id
            for obj in create_build_plan(load_scene(root / child["scene_path"])).objects
        }
        validation = json.loads(
            (final_path.parent / "model.validation.json").read_bytes()
        )
        for phase in ["built", "rebuilt", "glb_roundtrip"]:
            actual = [row["id"] for row in validation[phase]["objects"]]
            expect(
                validation[phase]["valid"]
                and set(actual) == expected_meshes
                and len(actual) == len(set(actual)),
                "actual connected building mesh phase mismatch: " + phase,
            )
        facts = [
            (path, json.loads(path.read_bytes()))
            for path in (context.parent / "project/facts").glob("*.json")
        ]
        reports = [
            value["project_building"]
            for _, value in facts
            if value.get("kind") == "project_building"
            and value["project_building"].get("build_id") == build_id
            and value["project_building"].get("report")
        ]
        expect(reports, "sealed connected report missing")
        final_report = reports[-1]
        expect(
            final_report["valid"]
            and final_report["current"]
            and final_report["connection_check"] == "passed",
            "source-bound connection check did not pass",
        )
        expect(
            final_report["report"]["connections"]["verified_count"] == 1
            and final_report["report"]["human_review"] == "not_run",
            "connection report falsely promoted completion",
        )
        images = [
            path.stem
            for path, value in facts
            if value.get("project_building", {}).get("build_id") == build_id
            and value["project_building"]
            .get("review_image", {})
            .get("delivery_request_id")
            == evidence.name
            and value["project_building"]["review_image"].get("delivery")
            == "attached_png"
        ]
        expect(len(images) >= 2, "two actual connected building image reads required")
        admission = admitted_images(evidence / "journal", images)
        (evidence / "image-admission.json").write_text(
            json.dumps(admission, indent=2), encoding="utf-8"
        )
        (evidence / "building-report.json").write_text(
            json.dumps(final_report, indent=2), encoding="utf-8"
        )
        (evidence / "independent-check.json").write_text(
            json.dumps(
                {
                    "original_hashes_unchanged": True,
                    "added_part_ids": sorted(expected_parts),
                    "mesh_ids": sorted(expected_meshes),
                    "grant_count": 1,
                    "operation_count": 1,
                    "current_authority_sha256": proposal["candidate_sha256"],
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        tui.wait(
            "final connection facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if "连接检查 passed" in screen and "总报告" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("Core connected building report not displayed")
        (evidence / "completed-screen.txt").write_text(screen, encoding="utf-8")
        report.step(
            "exact approval applied, real building mesh phases and actual PNG admission verified without original floor writes",
            lambda: None,
            tui,
        )
        report.step(
            "connection TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "project connection acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
