"""Interrupt actual CAD preparation through an owned TUI dsh, then inspect partial state."""

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import time
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
    parser.add_argument("--timeout", type=int, default=600)
    args = parser.parse_args()
    from domain_recovery_check import kill_owned_dsh
    from dsh_tui_check import CTRL_U, ENTER, Report, Tui, expect
    from windows_process_identity import check_clean

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    original_context = Path((args.evidence / "context-path.txt").read_text().strip())
    original_project = root / json.loads(original_context.read_bytes())["project_path"]
    expect(
        original_project.parent == root / "outputs",
        "requires existing isolated hospital inputs",
    )
    identifier = uuid.uuid4().hex[:8]
    project = root / "outputs" / ("hospital-init-interrupt-" + identifier)
    project.mkdir()
    for source in original_project.iterdir():
        expect(source.is_file(), "hospital fixture input must contain files only")
        shutil.copy2(source, project / source.name)
    config_path = project / "floor-08-trial.json"
    config = json.loads(config_path.read_bytes())
    config["source"] = str((project / "drawing.dxf").relative_to(root))
    config_path.write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    before = {str(p): sha(p) for p in project.iterdir()}
    evidence = repo / "target" / ("project-init-interrupt-live-" + identifier)
    evidence.mkdir()
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    contexts_before = set((root / "outputs/domain").glob("project-*/context.json"))
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
    report = Report(evidence)
    context = None
    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        contexts = [
            p
            for p in (root / "outputs/domain").glob("project-*/context.json")
            if p not in contexts_before
            and json.loads(p.read_bytes())["project_path"]
            == str(project.relative_to(root))
        ]
        expect(len(contexts) == 1, "new isolated project context ambiguous")
        context = contexts[0]
        (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
        tui.keys(CTRL_U)
        tui.type(
            "隔离医院初始化中断验收。加载building-modeling技能，inspect_project，select_project_target target=八层，"
            "调用prepare_project_floor f08和当前selection_id准备结构；保留配置假设。只准备，"
            "不要apply、build、authorize、shell或委派。测试观察者会在真实准备过程中注入本次dsh断连。"
        )
        tui.keys(ENTER)
        tui.wait("preparation running", lambda s: " · Running · " in s, 60)
        deadline = time.monotonic() + args.timeout
        stage = None
        while time.monotonic() < deadline:
            stages = [
                p for p in (context.parent / "project").glob("floor-*") if p.is_dir()
            ]
            if stages and any(p.is_file() for p in stages[0].rglob("*")):
                stage = stages[0]
                break
            expect(tui.exited() is None, "TUI exited before partial initialization")
            time.sleep(0.05)
        expect(stage is not None, "actual initialization stage never appeared")
        inventory_result = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress",
            ],
            capture_output=True,
            check=True,
            encoding="utf-8",
            timeout=25,
        )
        inventory = json.loads(inventory_result.stdout)
        candidates = [
            row
            for row in inventory
            if row["Name"].lower() == "python.exe"
            and "blender_agent.integrations.project_mcp"
            in (row.get("CommandLine") or "")
            and str(context) in (row.get("CommandLine") or "")
        ]
        # Windows venv Python can redirect through a second Python process with
        # the same module/context arguments. Select the leaf of that exact chain.
        parent_ids = {row["ParentProcessId"] for row in candidates}
        leaves = [row for row in candidates if row["ProcessId"] not in parent_ids]
        (evidence / "matched-mcp-processes.json").write_text(
            json.dumps(
                [
                    {key: row[key] for key in ["ProcessId", "ParentProcessId", "Name"]}
                    for row in candidates
                ],
                indent=2,
            ),
            encoding="utf-8",
        )
        expect(len(leaves) == 1, "exact context MCP process chain ambiguous")
        registry_path = context.parent / "project/scopes.json"
        expect(
            not registry_path.exists(), "initialization finished before interruption"
        )
        partial_before = {
            str(p.relative_to(stage)): sha(p) for p in stage.rglob("*") if p.is_file()
        }
        kill_owned_dsh(tui, leaves[0]["ProcessId"], evidence)
        tui.wait("actual preparation EOF", lambda s: " · Disconnected · " in s, 90)
        expect(
            not registry_path.exists(),
            "interrupted initialization unexpectedly registered a complete scope",
        )
        expect(
            all(sha(Path(p)) == digest for p, digest in before.items()),
            "input changed during initialization",
        )
        (evidence / "partial-at-interruption.json").write_text(
            json.dumps(partial_before, indent=2), encoding="utf-8"
        )
        (evidence / "disconnected-screen.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        expect(tui.quit() == 0, "disconnected TUI did not exit cleanly")
        check_clean(evidence)
        tui.close()
        durable = {str(p): sha(p) for p in stage.rglob("*") if p.is_file()}
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
                str(evidence / "recovery-journal"),
            ],
            repo,
            evidence,
        )
        tui.wait("recovery ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        tui.keys(CTRL_U)
        tui.type(
            "恢复上次初始化中断。加载building-modeling技能，inspect_project读取recovery_index的incomplete_floor_scopes，"
            "恢复select_project_target target=八层。只读确认现有floor目录unknown，未登记scope不能收为成功。"
            "本测试明确要求仅一次prepare_project_floor f08验证保护性拒绝：已有部分目录必须project_floor_incomplete，"
            "不能重建、覆盖或自动采用。随后inspect_project复核unknown。不要清理目录、apply、build、shell或委派。"
            "最终报告未确认结果及不能重放初始化的边界，200字以内。"
        )
        tui.keys(ENTER)
        tui.wait("partial recovery running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "partial recovery terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "partial recovery Agent failed")
        expect(
            {str(p): sha(p) for p in stage.rglob("*") if p.is_file()} == durable,
            "recovery reconstructed or overwrote partial initialization",
        )
        expect(
            not registry_path.exists(),
            "recovery silently adopted incomplete initialization",
        )
        facts = [
            json.loads(p.read_bytes())
            for p in (context.parent / "project/facts").glob("*.json")
        ]
        incomplete = [
            f
            for f in facts
            if f.get("recovery_index", {})
            .get("incomplete_floor_scopes", {})
            .get("total")
            == 1
        ]
        diagnostics = [
            f
            for f in facts
            if "project_floor_incomplete" in json.dumps(f.get("diagnostics", []))
        ]
        expect(
            incomplete and diagnostics,
            "actual recovery facts did not show incomplete refusal",
        )
        expect(
            all(sha(Path(p)) == digest for p, digest in before.items()),
            "recovery changed copied inputs",
        )
        (evidence / "partial-after-recovery.json").write_text(
            json.dumps(durable, indent=2), encoding="utf-8"
        )
        (evidence / "recovered-screen.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        report.step(
            "actual CAD initialization interrupted and fresh TUI rejects replay without changing partial files",
            lambda: None,
            tui,
        )
        report.step(
            "initialization recovery TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "initialization interruption acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
