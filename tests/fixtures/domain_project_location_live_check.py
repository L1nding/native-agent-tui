"""Actual TUI Agent proposes non-default CAD wall layers, obtains approval and builds.

Only fixture seeding and independent reading run outside TUI. Recovery observes the same
proposal/grant/operation/build and never reconstructs or replays writes.
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
    block = document.blocks["floors"]
    for entity in list(block.query("LINE")):
        if entity.dxf.layer != "WALL" or not 240000 <= entity.dxf.start.x <= 270000:
            continue
        a, b = entity.dxf.start, entity.dxf.end
        block.delete_entity(entity)
        for index in range(3):
            block.add_line(
                a.lerp(b, index / 3),
                a.lerp(b, (index + 1) / 3),
                dxfattribs={"layer": "TRIAL-ENCLOSURE"},
            )
    document.saveas(source)
    config = project / "floor-09-a-trial.json"
    if config.parent.resolve() != project.resolve():
        raise AssertionError("fixture config outside isolated project")
    config.unlink()


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary", type=Path, default=Path("target/release/native-agent-tui.exe")
    )
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args()
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    binary = args.binary.resolve()
    expect(
        binary.stat().st_mtime
        >= max(p.stat().st_mtime for p in (repo / "src").rglob("*.rs")),
        "release binary predates sources",
    )
    token = uuid.uuid4().hex[:8]
    evidence = repo / "target" / ("project-location-live-" + token)
    evidence.mkdir()
    project = root / "outputs" / ("project-location-fixture-" + token)
    subprocess.run(
        [
            str(root / ".venv/Scripts/python.exe"),
            str(Path(__file__).resolve()),
            "--seed",
            str(project),
        ],
        check=True,
        cwd=root,
    )
    baseline = {str(p): sha(p) for p in project.rglob("*") if p.is_file()}
    (evidence / "input-hashes.json").write_text(
        json.dumps(baseline, indent=2), encoding="utf-8"
    )
    report = Report(evidence)
    tui = None
    context = None

    def launch(restored=False):
        command = [
            str(binary),
            "--cwd",
            str(root),
            "--profile",
            args.profile,
            "--model",
            args.model,
            "--permission",
            "read-only",
            "--journal-dir",
            str(evidence / ("recovery-journal" if restored else "journal")),
        ]
        command += (
            ["--domain-context", str(context)]
            if restored
            else ["--domain-project", str(project.relative_to(root))]
        )
        return Tui(command, repo, evidence)

    def send(prompt):
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("task running", lambda s: " · Running · " in s, 60)

    def finished():
        screen = tui.wait(
            "task terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in screen, "Agent turn failed; no replay")
        tui.wait(
            "private facts synchronized",
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
        return json.loads((context.parent / "project/scopes.json").read_bytes())

    def child():
        record = registry()["scopes"]["f09.a"]
        bound = json.loads((root / record["context_path"]).read_bytes())
        return record, bound, root / "outputs/domain" / bound["run_id"] / "changes"

    def proposal():
        record, _, directory = child()
        return json.loads(
            (directory / (record["bootstrap_proposal_id"] + ".json")).read_bytes()
        )

    def find_screen(name, predicate):
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if predicate(screen):
                (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
                return screen
            tui.keys("\x1b[6~")
        raise AssertionError("missing Core screen " + name)

    def business_hashes():
        _, bound, directory = child()
        paths = [context, root / bound["scene_path"], *project.rglob("*")]
        paths += [
            p
            for p in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", p.stem)
        ]
        paths += list((directory / "artifacts").rglob("*"))
        paths += list((root / bound["scene_path"]).parent.rglob("*"))
        return {str(p): sha(p) for p in paths if p.is_file()}

    try:
        tui = launch()
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        for path in root.glob("outputs/domain/project-*/context.json"):
            if json.loads(path.read_bytes()).get("project_path") == str(
                project.relative_to(root)
            ):
                context = path
                break
        expect(context is not None, "project context missing")
        (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
        send(
            "这是编码Agent代做已授权隔离测试决策。加载building-modeling技能，inspect_project，"
            "select_project_target target=九层、choices=[f09.a]，测试选择仅f09.a。"
            "调用inspect_project_floor_location，用完整逐图层证据解释默认图层为何无法定位；"
            "可用read_fact_view分页。只读这一阶段，不prepare、应用、构建或shell，报告定位身份与图层。"
        )
        finished()
        location_facts = [
            json.loads(p.read_bytes())
            for p in (context.parent / "project/facts").glob("*.json")
        ]
        location = next(
            f["project_location"]
            for f in location_facts
            if f.get("kind") == "project_location"
        )
        expect(location["status"] == "needs_wall_layers", "did not exercise fallback")
        expect(
            any(
                row == {"layer": "TRIAL-ENCLOSURE", "line_entities": 24}
                for row in location["layer_statistics"]
            ),
            "missing actual source line counts",
        )
        expect(
            not list((context.parent / "project").glob("floor-*")),
            "read initialized a scope",
        )
        find_screen(
            "location-evidence",
            lambda s: "楼层定位 f09.a" in s and "needs_wall_layers" in s,
        )
        find_screen(
            "layer-counts", lambda s: "TRIAL-ENCLOSURE" in s and "24 线实体" in s
        )
        report.step(
            "actual TUI reads source-bound non-default layer evidence without initialization",
            lambda: None,
            tui,
        )
        selection = json.loads((context.parent / "project/selection.json").read_bytes())
        reason = "isolated authored paired CAD enclosure; assumed heights retained"
        choice = {
            "expected_location_id": location["location_id"],
            "wall_layers": ["TRIAL-ENCLOSURE"],
            "reason": reason,
        }
        send(
            "继续同一隔离测试：prepare_project_floor floor_key=f09.a、selection_id="
            + selection["selection_id"]
            + "、allow_assumptions=true、wall_layer_choice="
            + json.dumps(choice)
            + "。只准备结构方案并等待具体批准，不批准、不应用、不构建。"
            "后续收到平台具体授权后才apply_change operation_id="
            + evidence.name
            + "-apply，"
            "build_scene build_id="
            + evidence.name
            + "-build，inspect_artifact等待终态，validate_artifact。"
            "不要旧workflow或shell，不更改源文件，不额外生成新的提案或身份。"
        )
        wait("registered proposal", lambda: "f09.a" in registry()["scopes"])
        finished()
        prepared = proposal()
        expect(prepared["state"] == "prepared", "structure proposal not ready")
        expect(
            location["location_id"] in prepared["draft"]["summary"],
            "choice absent from exact summary",
        )
        _, bound, directory = child()
        empty = (root / bound["scene_path"]).read_bytes()
        expect(
            not list(directory.glob("authorization-*.json")),
            "proposal already authorized",
        )
        find_screen(
            "bound-layer-choice",
            lambda s: "结构提案绑定的墙体图层" in s and "TRIAL-ENCLOSURE" in s,
        )
        find_screen(
            "before-approval",
            lambda s: "方案 " + prepared["proposal_id"] in s and "修订 1" in s,
        )
        (evidence / "test-decision.json").write_text(
            json.dumps(
                {
                    "actor": "coding-agent-test-delegate",
                    "user_authorized": True,
                    "proposal_id": prepared["proposal_id"],
                    "revision": 1,
                    "prepare_token": prepared["prepare_token"],
                    "choice": choice,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        tui.keys(CTRL_Y)
        wait("applied original proposal", lambda: proposal()["state"] == "applied")
        build_path = directory / "artifacts" / (evidence.name + "-build") / "build.json"
        wait(
            "real Blender terminal",
            lambda: (
                json.loads(build_path.read_bytes())["state"]
                in ["built", "failed", "unknown", "cancelled"]
            ),
        )
        finished()
        build = json.loads(build_path.read_bytes())
        expect(build["state"] == "built", "real build unsuccessful")
        authority = root / bound["scene_path"]
        expect(
            authority.read_bytes() != empty
            and sha(authority) == prepared["candidate_sha256"],
            "candidate not applied exactly",
        )
        expect(
            len(list(directory.glob("authorization-*.json"))) == 1, "wrong grant count"
        )
        expect(
            len(list(directory.glob("operation-*.json"))) == 1, "wrong operation count"
        )
        validation = json.loads(
            Path(build["output"]).with_suffix(".validation.json").read_bytes()
        )
        expect(validation["valid"], "Blender validation invalid")
        phases = {}
        for phase in ["built", "rebuilt", "glb_roundtrip"]:
            ids = [obj["id"] for obj in validation[phase]["objects"]]
            expect(ids and len(ids) == len(set(ids)), "duplicate or absent meshes")
            phases[phase] = sorted(ids)
        expect(
            phases["built"] == phases["rebuilt"] == phases["glb_roundtrip"],
            "mesh identities changed",
        )
        expect(
            all(sha(Path(p)) == h for p, h in baseline.items()),
            "source/business inputs changed",
        )
        (evidence / "independent-check.json").write_text(
            json.dumps(
                {
                    "proposal_id": prepared["proposal_id"],
                    "choice": choice,
                    "build_id": build["build_id"],
                    "authority_sha256": sha(authority),
                    "valid": True,
                    "mesh_ids": phases,
                    "source_inputs_unchanged": True,
                    "human_compliance_review": "not_run",
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        report.step(
            "exact TUI approval applies one layer-bound structure and real Blender build/rebuild/GLB",
            lambda: None,
            tui,
        )
        expect(tui.quit() == 0, "initial TUI did not exit cleanly")
        tui.close()
        before = business_hashes()
        tui = launch(True)
        tui.wait("recovery ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        send(
            "只读恢复同一隔离项目：加载building-modeling技能，inspect_project，"
            "select_project_target target=九层 choices=[f09.a]，inspect_project_floor_location f09.a "
            "selection_id="
            + selection["selection_id"]
            + "。然后activate_project_floor f09.a "
            "selection_id="
            + selection["selection_id"]
            + "，inspect_change proposal_id="
            + prepared["proposal_id"]
            + "，inspect_operation operation_id="
            + evidence.name
            + "-apply，"
            "inspect_artifact和validate_artifact build_id=" + evidence.name + "-build。"
            "只报告原已应用方案、定位绑定和产物结果。不得prepare、authorize、apply、build、preview、shell，"
            "不得生成新业务身份，未知结果不重放。"
        )
        finished()
        find_screen(
            "recovered-bound-choice",
            lambda s: "结构提案绑定的墙体图层" in s and "TRIAL-ENCLOSURE" in s,
        )
        find_screen(
            "recovered-proposal",
            lambda s: "方案 " + prepared["proposal_id"] in s and "applied" in s,
        )
        expect(
            business_hashes() == before,
            "recovery changed business files or identity sets",
        )
        report.step(
            "fresh actual TUI restores the same location/proposal/grant/operation/build without replay",
            lambda: None,
            tui,
        )
        report.step(
            "recovery TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "floor-location live acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        if tui is not None:
            tui.close()
    print(evidence, flush=True)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--seed":
        seed(Path(sys.argv[2]))
    else:
        main()
