"""Read a current or legacy building report through a real TUI, without replay."""

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
    parser.add_argument("--build-id", default="building-assembled-01")
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args()
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    bound = json.loads(context.read_bytes())
    project = root / bound["project_path"]
    expect(
        project.parent == root / "outputs", "report requires an isolated test project"
    )
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    building = context.parent / "project/building" / args.build_id
    expect((building / "build.json").is_file(), "original job is missing")
    watched = [context, *project.iterdir(), *building.rglob("*")]
    registry = json.loads((context.parent / "project/scopes.json").read_bytes())
    for scope in registry["scopes"].values():
        child_context = root / scope["context_path"]
        child = json.loads(child_context.read_bytes())
        changes = root / "outputs/domain" / child["run_id"] / "changes"
        watched.extend([child_context, root / child["scene_path"]])
        watched.extend(
            path
            for path in changes.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
        )
        watched.extend(changes.glob("artifacts/*/build.json"))
    before = {str(path): sha(path) for path in watched if path.is_file()}
    evidence = repo / "target" / ("building-report-live-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
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
    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        tui.keys(CTRL_U)
        tui.type(
            "隔离楼栋总报告只读验收。加载building-modeling技能。inspect_project，再select_project_target恢复原target="
            + selection["target"]
            + "及choices="
            + json.dumps([key for key in selection["selected_keys"] if "." in key])
            + "。只读取原build_id="
            + args.build_id
            + "，调用check_project_building_connections与read_project_building_report。"
            "read_fact_view limit=20按next_offset读完整报告的来源、图纸、连接和未执行项。"
            "不要prepare、授权、apply、assemble、build或shell，不委派其他Agent。"
            "若旧快照缺连接来源证明，保留明确attention；相邻楼层没有已验证连接不得当成功。"
            "最后inspect_project_building检查同一任务，报告当前／历史、产物有效性、总报告与未决原因，人工复核仍not_run。"
        )
        tui.keys(ENTER)
        tui.wait("report running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "report terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "report Agent failed")
        rows = []
        for path in (context.parent / "project/facts").glob("*.json"):
            value = json.loads(path.read_bytes())
            record = value.get("project_building", {})
            if record.get("build_id") == args.build_id and record.get("report"):
                rows.append((value["fact_sequence"], path.stem, record))
        expect(rows, "no sealed total report was produced")
        _, receipt, record = max(rows, key=lambda row: row[0])
        body = record["report"]
        expect(
            body["human_review"] == "not_run" and body["state"] == "attention",
            "report promoted human completion",
        )
        expect(
            record["connection_check"] == "attention",
            "unconnected test building was called connected",
        )
        expect(
            body["connections"]["verified_count"] == 0,
            "test has no verified declared connections",
        )
        expect(
            all(sha(Path(path)) == digest for path, digest in before.items()),
            "report changed original inputs, authority, grants or built artifacts",
        )
        (evidence / "building-report.json").write_text(
            json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / "report-receipt.txt").write_text(receipt, encoding="utf-8")
        tui.wait(
            "report facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if "总报告 attention" in screen and "技术 attention" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("Core total report is not displayed")
        (evidence / "completed-screen.txt").write_text(screen, encoding="utf-8")
        report.step(
            "legacy building checked and sealed report displayed with explicit missing connections and original hashes unchanged",
            lambda: None,
            tui,
        )
        report.step(
            "building report TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "building report acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
