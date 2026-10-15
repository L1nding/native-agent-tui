"""Recover an existing applied layer-choice proposal through a fresh actual TUI.

Independent observers validate pinned source dependencies, the actual Blender phases,
and unchanged business hashes. No preparation, approval, writing or building is replayed.
"""

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


def core_panel(rendered):
    """Read the business panel only; an Agent's assertion is not a Core projection."""
    start = rendered.find("┌方案与执行事实")
    end = rendered.find("└", start) if start >= 0 else -1
    return rendered[start : end + 1] if end > start else ""


def artifact_visible(rendered, build_id, validated=False):
    state = "产物验证 valid=true" if validated else "Blender 产物 built"
    return any(
        state in row and "build_id=" + build_id in row
        for row in core_panel(rendered).splitlines()
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args()
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    original = args.evidence.resolve()
    context = Path((original / "context-path.txt").read_text(encoding="utf-8").strip())
    expect(
        context.resolve().is_relative_to(root / "outputs/domain"),
        "context outside isolated outputs",
    )
    bound = json.loads(context.read_bytes())
    project = root / bound["project_path"]
    expect(project.parent == root / "outputs", "requires isolated authored fixture")
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    record = registry["scopes"]["f09.a"]
    child_context = root / record["context_path"]
    child = json.loads(child_context.read_bytes())
    stage = child_context.parent
    directory = root / "outputs/domain" / child["run_id"] / "changes"
    authority = root / child["scene_path"]
    proposal = json.loads(
        (directory / (record["bootstrap_proposal_id"] + ".json")).read_bytes()
    )
    expect(
        proposal["state"] == "applied",
        "existing proposal not applied; recovery cannot write",
    )
    expect(
        sha(authority) == proposal["candidate_sha256"],
        "authority not original candidate",
    )
    expect(
        len(list(directory.glob("authorization-*.json"))) == 1,
        "requires exact one old grant",
    )
    expect(
        len(list(directory.glob("operation-*.json"))) == 1,
        "requires exact one old operation",
    )
    source_choice = json.loads((stage / "location-source.json").read_bytes())
    choice = record["wall_layer_choice"]
    expect(source_choice["choice"] == choice, "registered and sealed choice differ")
    expect(
        choice["expected_location_id"] == source_choice["evidence"]["location_id"],
        "wrong location binding",
    )
    expect(
        all(
            Path(path).is_file() and sha(Path(path)) == digest
            for path, digest in proposal["source_dependencies"].items()
        ),
        "prepared source dependency changed",
    )
    build_id = original.name + "-build"
    operation_id = original.name + "-apply"
    build_path = directory / "artifacts" / build_id / "build.json"
    build = json.loads(build_path.read_bytes())
    expect(build["state"] == "built", "original build not successful")
    validation = json.loads(
        Path(build["output"]).with_suffix(".validation.json").read_bytes()
    )
    expect(validation["valid"], "original Blender result invalid")
    phases = {}
    for phase in ["built", "rebuilt", "glb_roundtrip"]:
        ids = [item["id"] for item in validation[phase]["objects"]]
        expect(ids and len(ids) == len(set(ids)), "missing/duplicate mesh identity")
        phases[phase] = sorted(ids)
    expect(
        phases["built"] == phases["rebuilt"] == phases["glb_roundtrip"],
        "phase identity changed",
    )
    expect(
        (project / "original-scene.json").read_bytes()
        == b'{"baseline":"never written"}',
        "isolated business marker changed",
    )
    inputs = original / "input-hashes.json"
    if inputs.is_file():
        expect(
            all(
                sha(Path(path)) == digest
                for path, digest in json.loads(inputs.read_bytes()).items()
            ),
            "initial fixture inputs changed",
        )

    def business_hashes():
        paths = [
            context,
            child_context,
            authority,
            *project.rglob("*"),
            *stage.rglob("*"),
        ]
        paths += [
            path
            for path in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
        ]
        paths += list((directory / "artifacts").rglob("*"))
        return {str(path): sha(path) for path in paths if path.is_file()}

    before = business_hashes()
    evidence = repo / "target" / ("project-location-recovery-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
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
        screen = tui.wait(
            "recovery task terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(
            " · Failed · " not in screen,
            "read-only recovery response failed; no replay",
        )
        tui.wait(
            "recovery facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )

    def screen(name, predicate):
        tui.keys(CTRL_HOME)
        for _ in range(100):
            rendered = tui.console.stable()
            if predicate(core_panel(rendered)):
                (evidence / (name + ".txt")).write_text(rendered, encoding="utf-8")
                return
            tui.keys("\x1b[6~")
        raise AssertionError("missing Core screen " + name)

    try:
        tui.wait("recovery ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        send(
            "上次隔离验收在实际批准/应用/构建成功后回复遇到upstream_http2_stream_error。"
            "本次严格只读恢复：加载building-modeling技能，inspect_project，select_project_target "
            "target=九层 choices=[f09.a]；这是原已授权测试选择。inspect_project_floor_location "
            "floor_key=f09.a selection_id="
            + selection["selection_id"]
            + "，然后activate_project_floor floor_key=f09.a selection_id="
            + selection["selection_id"]
            + "，inspect_change proposal_id="
            + proposal["proposal_id"]
            + "。简短报告原applied和绑定图层。"
            "本轮只这5个领域工具，不扫描/分页/准备/授权/应用/构建/渲染/shell，不创建新业务身份。"
        )
        screen(
            "bound-layer-choice",
            lambda s: "结构提案绑定的墙体图层" in s and "TRIAL-ENCLOSURE" in s,
        )
        screen(
            "applied-proposal",
            lambda s: "方案 " + proposal["proposal_id"] in s and "applied" in s,
        )
        expect(
            business_hashes() == before,
            "scope/proposal recovery changed business files",
        )
        report.step(
            "fresh actual TUI restores original source-bound layer choice and applied proposal",
            lambda: None,
            tui,
        )
        send(
            "继续严格只读恢复同一项目：inspect_operation operation_id="
            + operation_id
            + "，inspect_artifact build_id="
            + build_id
            + "、validate_artifact build_id="
            + build_id
            + "。只这3个工具，简短报告已有操作和Blender技术检查；不扫描/渲染/prepare/authorize/apply/build/shell。"
            "不生成新业务身份。人工/合规验收未运行，不能说建筑已完成。"
        )
        screen(
            "built-artifact",
            lambda s: artifact_visible(s, build_id),
        )
        screen(
            "validated-artifact",
            lambda s: artifact_visible(s, build_id, validated=True),
        )
        expect(
            business_hashes() == before,
            "artifact recovery modified or allocated business state",
        )
        (evidence / "independent-check.json").write_text(
            json.dumps(
                {
                    "original_evidence": original.name,
                    "proposal_id": proposal["proposal_id"],
                    "choice": choice,
                    "candidate_sha256": proposal["candidate_sha256"],
                    "operation_id": operation_id,
                    "build_id": build_id,
                    "mesh_ids": phases,
                    "original_source_dependencies_match": True,
                    "business_hashes_unchanged": True,
                    "human_compliance_review": "not_run",
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        report.step(
            "same old grant/operation/artifact restored with immutable hashes and real Blender phase identities",
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
            "floor location read-only recovery",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
    print(evidence, flush=True)


if __name__ == "__main__":
    main()
