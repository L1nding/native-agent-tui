"""Real TUI proof of CAD audit image, Agent judgement and concrete correction approval."""

import argparse
import hashlib
import json
import re
import sys
import time
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_geometry_delta(root, context, proposal, evidence):
    from blender_agent.modeling.plan import create_build_plan
    from blender_agent.scene_ir.io import load_scene

    scope = json.loads((context.parent / "project/scopes.json").read_bytes())["scopes"][
        "f08"
    ]
    child = json.loads((root / scope["context_path"]).read_bytes())
    directory = root / "outputs/domain" / child["run_id"] / "changes"
    jobs = [
        json.loads(path.read_bytes())
        for path in directory.glob("artifacts/*/build.json")
    ]
    initial = next(
        job for job in jobs if job["proposal_id"] == scope["bootstrap_proposal_id"]
    )
    final = next(job for job in jobs if job["proposal_id"] == proposal["proposal_id"])
    before = json.loads(Path(initial["scene_snapshot"]).read_bytes())
    after = json.loads((root / child["scene_path"]).read_bytes())
    assert {k: v for k, v in before.items() if k != "floors"} == {
        k: v for k, v in after.items() if k != "floors"
    }
    assert [floor["id"] for floor in before["floors"]] == [
        floor["id"] for floor in after["floors"]
    ]
    operations = proposal["draft"]["operations"]
    assert (
        len(operations) == 1
        and operations[0]["op"] == "set"
        and operations[0]["field"] == "thickness"
    )
    changes = []
    collections = {"walls", "slabs", "elements", "openings", "constraints", "solids"}
    for old_floor, new_floor in zip(before["floors"], after["floors"]):
        assert {k: v for k, v in old_floor.items() if k not in collections} == {
            k: v for k, v in new_floor.items() if k not in collections
        }
        for name in collections:
            old = {item["id"]: item for item in old_floor.get(name, [])}
            new = {item["id"]: item for item in new_floor.get(name, [])}
            assert old.keys() == new.keys()
            for identity, old_value in old.items():
                if old_value == new[identity]:
                    continue
                assert name == "walls" and identity == operations[0]["target_id"]
                assert (
                    abs(old_value["thickness"] - 0.25) < 1e-9
                    and new[identity]["thickness"] == 0.2
                )
                omitted = {"thickness", "metadata", "source_ids"}
                assert {k: v for k, v in old_value.items() if k not in omitted} == {
                    k: v for k, v in new[identity].items() if k not in omitted
                }
                assert (
                    new[identity]["metadata"]["decision_id"]
                    == operations[0]["decision_id"]
                )
                changes.append(
                    {
                        "id": identity,
                        "before_thickness_m": old_value["thickness"],
                        "after_thickness_m": new[identity]["thickness"],
                        "remaining_geometry_equal": True,
                    }
                )
    assert len(changes) == 1
    expected = {
        obj.id
        for obj in create_build_plan(load_scene(Path(final["scene_snapshot"]))).objects
    }
    validation = json.loads(
        Path(final["output"]).with_suffix(".validation.json").read_bytes()
    )
    phases = {}
    for phase in ["built", "rebuilt", "glb_roundtrip"]:
        ids = [obj["id"] for obj in validation[phase]["objects"]]
        assert (
            validation[phase]["valid"]
            and len(ids) == len(set(ids))
            and set(ids) == expected
        )
        phases[phase] = {
            "valid": True,
            "mesh_count": len(ids),
            "exact_expected_ids": True,
        }
    result = {
        "valid": True,
        "proposal_id": proposal["proposal_id"],
        "authority_sha256": sha(root / child["scene_path"]),
        "approved_candidate_matches": sha(root / child["scene_path"])
        == proposal["candidate_sha256"],
        "changes": changes,
        "mesh_phases": phases,
        "grant_count": len(list(directory.glob("authorization-*.json"))),
    }
    (evidence / "independent-geometry-check.json").write_text(
        json.dumps(result, indent=2), encoding="utf-8"
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--resume-evidence", type=Path)
    args = parser.parse_args()
    import ezdxf
    from domain_project_images import admitted_images
    from domain_project_live_check import seed
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    expect(
        args.binary.stat().st_mtime
        >= max(p.stat().st_mtime for p in (repo / "src").rglob("*.rs")),
        "release is stale",
    )
    token = uuid.uuid4().hex[:8]
    context = None
    if args.resume_evidence:
        context = Path((args.resume_evidence / "context-path.txt").read_text().strip())
        bound = json.loads(context.read_bytes())
        expect(Path(bound["project_root"]).resolve() == root, "recovery root mismatch")
        project = root / bound["project_path"]
        expect(
            project.parent == root / "outputs"
            and project.name.startswith("drawing-correction-fixture-"),
            "recovery must be isolated",
        )
        original = json.loads(
            (args.resume_evidence / "authored-test-decision.json").read_bytes()
        )
        expect(
            sha(project / "drawing.dxf") == original["source_sha256"],
            "original CAD changed before recovery",
        )
        drawing = project / "drawing.dxf"
    else:
        project = root / "outputs" / ("drawing-correction-fixture-" + token)
        seed(project)
        drawing = project / "drawing.dxf"
        document = ezdxf.readfile(drawing)
        block = document.blocks.get("floors")
        removed = 0
        for entity in list(block):
            if (
                entity.dxftype() == "LINE"
                and entity.dxf.layer == "WALL"
                and entity.dxf.start.x == 0
                and entity.dxf.end.x == 8000
                and entity.dxf.start.y in (0, 200)
            ):
                block.delete_entity(entity)
                removed += 1
        expect(removed == 2, "authored CAD fixture did not replace exact bottom wall")
        for a, b in [
            ((0, 100), (4000, 100)),
            ((0, 300), (4000, 300)),
            ((4000, 75), (8000, 75)),
            ((4000, 325), (8000, 325)),
        ]:
            block.add_line(a, b, dxfattribs={"layer": "WALL"})
        document.saveas(drawing)
    before = {p.name: sha(p) for p in project.iterdir()}
    evidence = repo / "target" / ("drawing-correction-live-" + token)
    evidence.mkdir()
    (evidence / "authored-test-decision.json").write_text(
        json.dumps(
            {
                "actor": "coding_agent_as_user_authorized_test_proxy",
                "real_project_human_confirmation": False,
                "reason": "authored CAD fixture has collinear 0.2m and 0.25m wall segments; trial correction chooses 0.2m",
                "authority": "concrete versions still require separate real TUI Ctrl+Y",
                "source_sha256": sha(drawing),
            },
            indent=2,
        ),
        encoding="utf-8",
    )
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(root),
            "--domain-context" if context else "--domain-project",
            str(context) if context else str(project.relative_to(root)),
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

    def find_context():
        for path in root.glob("outputs/domain/project-*/context.json"):
            if Path(
                json.loads(path.read_bytes()).get("project_path", "")
            ) == project.relative_to(root):
                return path
        return None

    def records():
        registry = json.loads((context.parent / "project/scopes.json").read_bytes())
        record = registry["scopes"]["f08"]
        child = json.loads((root / record["context_path"]).read_bytes())
        directory = root / "outputs/domain" / child["run_id"] / "changes"
        rows = [
            json.loads(p.read_bytes())
            for p in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}", p.stem)
        ]
        return child, directory, rows

    def send(prompt):
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("Agent running", lambda s: " · Running · " in s, 60)

    def finished():
        tui.wait(
            "Agent terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "Agent turn failed")

    def wait(label, predicate):
        end = time.monotonic() + args.timeout
        while time.monotonic() < end:
            try:
                if predicate():
                    return
            except (OSError, KeyError, json.JSONDecodeError):
                pass
            expect(tui.exited() is None, "TUI exited during " + label)
            expect(" · Failed · " not in tui.screen(), "Agent failed during " + label)
            time.sleep(0.25)
        (evidence / (label + "-timeout.txt")).write_text(tui.screen(), encoding="utf-8")
        raise AssertionError("timeout: " + label)

    def approve(proposal, expected_grants, label):
        tui.wait(
            "complete facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(150):
            screen = tui.console.stable()
            if "方案 " + proposal["proposal_id"] in screen and "修订 1" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("exact proposal not visible")
        expect(
            len(list(records()[1].glob("authorization-*.json"))) == expected_grants,
            "unexpected grant before decision",
        )
        (evidence / (label + "-before-approval.txt")).write_text(
            screen, encoding="utf-8"
        )
        (evidence / (label + "-test-decision.json")).write_text(
            json.dumps(
                {
                    key: proposal[key]
                    for key in (
                        "proposal_id",
                        "revision",
                        "prepare_token",
                        "base_sha256",
                        "candidate_sha256",
                    )
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        tui.keys(CTRL_Y)
        wait(
            label + "-applied",
            lambda: any(
                p["proposal_id"] == proposal["proposal_id"] and p["state"] == "applied"
                for p in records()[2]
            ),
        )
        wait(
            label + "-built",
            lambda: any(
                json.loads(p.read_bytes()).get("state") == "built"
                and json.loads(p.read_bytes()).get("proposal_id")
                == proposal["proposal_id"]
                for p in records()[1].glob("artifacts/*/build.json")
            ),
        )
        finished()

    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        context = context or find_context()
        expect(context is not None, "project context missing")
        (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
        if args.resume_evidence:
            registry = json.loads((context.parent / "project/scopes.json").read_bytes())
            scope = registry["scopes"]["f08"]
            selected = json.loads(
                (context.parent / "project/selection.json").read_bytes()
            )
            prior = json.loads((args.resume_evidence / "correction.json").read_bytes())
            structure = next(
                p
                for p in records()[2]
                if p["proposal_id"] == scope["bootstrap_proposal_id"]
            )
            expect(
                structure["state"] == "applied" and prior["state"] == "prepared",
                "recovery states differ; inspect original operations first",
            )
            judgements = [
                json.loads(p.read_bytes())
                for p in (records()[1] / "drawing").glob("*/judgement-*.json")
            ]
            expect(len(judgements) == 1, "recovery needs one original observation")
            observation = judgements[0]
            send(
                "用户授权的隔离修正测试只读续接。此前Core拒绝中文长判断的显示，原修正仍prepared、未批准。"
                "禁止重新prepare、记录新判断、授权、应用或构建。加载building-modeling；inspect_project，select_project_target target=八层恢复原选择 "
                + selected["selection_id"]
                + "，inspect_project_floor f08；audit_drawing读取原1/3判断。"
                "用原audit_id="
                + observation["audit_id"]
                + "、candidate_id="
                + observation["candidate_id"]
                + "、image_sha256="
                + observation["image_sha256"]
                + "、request_id="
                + observation["request_id"]
                + "调用read_drawing_candidate_image重新读取同一实际图片，并inspect_change proposal_id="
                + prior["proposal_id"]
                + "展示原修正具体版本与决策，等待TUI批准，不重做初始化。所有read_fact_view使用limit=20，文本按next_offset继续，不传字符数作为limit。最后再次inspect_change确认完整当前事实。"
            )
            finished()
            expect(
                len(list(records()[1].glob("authorization-*.json"))) == 1,
                "recovery replayed authorization",
            )
            expect(
                sha(root / records()[0]["scene_path"]) == structure["candidate_sha256"],
                "read-only recovery changed authority",
            )
            report.step(
                "original applied structure and unapproved correction recovered read-only",
                lambda: None,
                tui,
            )
        else:
            send(
                "用户授权编码Agent代做决策的隔离CAD审查测试。加载building-modeling技能。inspect_project并select_project_target target=八层，"
                "prepare_project_floor f08，只准备并展示具体结构版本，本轮不授权不应用不构建。示意参数沿原测试配置保持assumed。"
                "后续收到平台具体批准才由你应用、构建和独立验证，不shell不旧workflow。"
            )
            wait("structure", lambda: bool(records()[2]))
            finished()
            structure = records()[2][-1]
            expect(structure["state"] == "prepared", "structure blocked")
            approve(structure, 0, "structure")
            report.step(
                "source CAD structure approved and built through TUI Agent",
                lambda: None,
                tui,
            )
            send(
                "继续这份编码Agent代理测试的图纸审查。audit_drawing按next_offset读取候选，找thickness_mismatch；"
                "这是明确授权的试验决策：本作者测试图的同一直墙两段厚度为0.2m与0.25m，统一采用0.2m。"
                "先render_drawing_candidate、read_drawing_candidate_image读取实际PNG并检查来源和测量；"
                "确认该厚度候选后record_drawing_judgement，解释实际观察及此试验决策，把0.25m那段的thickness改为0.2m，"
                "fix_operations只能这一项set，保留原DXF、墙ID、起终点、标高和高度。若实际证据不符就停下报告。"
                "再prepare_drawing_correction，只准备并展示具体提案与决策草稿，等待TUI批准，不自行应用或授权。"
            )
            wait(
                "correction", lambda: any(p.get("drawing_review") for p in records()[2])
            )
            finished()
        correction = next(p for p in records()[2] if p.get("drawing_review"))
        expect(
            correction["state"] == "prepared",
            "correction blocked: " + json.dumps(correction["issues"]),
        )
        expect(
            sha(root / records()[0]["scene_path"]) == structure["candidate_sha256"],
            "unapproved correction changed authority",
        )
        decision_id = correction["decision_drafts"][0]["id"]
        decision = (
            (root / records()[0]["scene_path"]).parent.parent
            / "decisions"
            / (decision_id + ".md")
        )
        expect(not decision.exists(), "decision written before approval")
        (evidence / "correction.json").write_text(
            json.dumps(correction, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        approve(correction, 1, "correction")
        expect(decision.is_file(), "approved decision missing")
        expect(
            sha(root / records()[0]["scene_path"]) == correction["candidate_sha256"],
            "authority differs from approved correction",
        )
        expect(
            len(list(records()[1].glob("authorization-*.json"))) == 2, "grant replay"
        )
        expect(
            {p.name: sha(p) for p in project.iterdir()} == before,
            "original CAD/config changed",
        )
        for job in records()[1].glob("artifacts/*/build.json"):
            value = json.loads(job.read_bytes())
            if value["state"] == "built":
                validation = json.loads(
                    Path(value["output"]).with_suffix(".validation.json").read_bytes()
                )
                expect(validation["valid"], "artifact validation failed")
        verify_geometry_delta(root, context, correction, evidence)
        image_receipts = []
        deliveries = []
        for path in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(path.read_bytes())
            if fact.get("drawing_image", {}).get("delivery") == "attached_png":
                deliveries.append((fact["fact_sequence"], path.stem))
        if deliveries:
            image_receipts = [max(deliveries)[1]]
        expect(image_receipts, "actual candidate crop delivery missing")
        (evidence / "image-admission.json").write_text(
            json.dumps(admitted_images(evidence / "journal", image_receipts), indent=2),
            encoding="utf-8",
        )
        report.step(
            "actual candidate image, Agent judgement and separately approved decision-backed correction built with source unchanged",
            lambda: None,
            tui,
        )
        report.step(
            "correction TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "drawing correction acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        (evidence / "final-screen.txt").write_text(tui.screen(), encoding="utf-8")
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
