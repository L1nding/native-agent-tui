"""Cancel an unmodified real Blender building job through the TUI's dsh Agent."""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import threading
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
    parser.add_argument(
        "--disconnect",
        action="store_true",
        help="interrupt this exact owned dsh while real Blender is live, then recover read-only",
    )
    parser.add_argument(
        "--controlled-pause",
        action="store_true",
        help="pause real Blender before its unchanged production script for this exact job only",
    )
    args = parser.parse_args()
    if args.disconnect and args.controlled_pause:
        parser.error("disconnect test uses unmodified timing")
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect
    from windows_process_identity import creation_time, kernel_api

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    project = root / json.loads(context.read_bytes())["project_path"]
    expect(
        project.parent == root / "outputs",
        "requires an isolated completed two-floor fixture",
    )
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    registry = json.loads((context.parent / "project/scopes.json").read_bytes())
    watched = [context, *(p for p in project.rglob("*") if p.is_file())]
    for scope in registry["scopes"].values():
        child_context = root / scope["context_path"]
        child = json.loads(child_context.read_bytes())
        changes = root / "outputs/domain" / child["run_id"] / "changes"
        watched.extend([child_context, root / child["scene_path"]])
        watched.extend(
            p
            for p in changes.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", p.stem)
        )
        watched.extend(p for p in (changes / "artifacts").rglob("*") if p.is_file())
    watched.extend(
        p for p in (context.parent / "project/building").rglob("*") if p.is_file()
    )
    before = {str(p): sha(p) for p in watched}
    evidence = (
        repo
        / "target"
        / (
            ("building-interrupt-live-" if args.disconnect else "building-cancel-live-")
            + uuid.uuid4().hex[:8]
        )
    )
    evidence.mkdir()
    build_id = evidence.name
    job_path = context.parent / "project/building" / build_id / "build.json"
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    old_executable = os.environ.get("BLENDER_EXECUTABLE")
    if args.controlled_pause:
        from blender_agent.blender.adapter import find_blender

        actual = find_blender()
        expect(
            actual is not None,
            "actual Blender is required for controlled live cancellation",
        )
        gate = repo / "tests/fixtures/blender_project_cancel_gate.py"
        settings = evidence / "cancel-gate.json"
        settings.write_text(
            json.dumps(
                {
                    "context_path": str(context),
                    "context_sha256": sha(context),
                    "build_id": build_id,
                }
            ),
            encoding="utf-8",
        )
        wrapper = evidence / "real-blender-gated.cmd"
        expect(
            not any(
                c in str(actual) + str(gate) + str(settings)
                for c in ['"', "%", "\r", "\n"]
            ),
            "unsupported control character in fixture path",
        )
        wrapper.write_text(
            '@echo off\nset "MODELING_PROJECT_CANCEL_GATE='
            + str(settings)
            + '"\n"'
            + actual
            + '" --background --factory-startup --python-exit-code 1 --python "'
            + str(gate)
            + '" %*\nexit /b %errorlevel%\n',
            encoding="utf-8",
        )
        os.environ["BLENDER_EXECUTABLE"] = str(wrapper)
    try:
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
    finally:
        if old_executable is None:
            os.environ.pop("BLENDER_EXECUTABLE", None)
        else:
            os.environ["BLENDER_EXECUTABLE"] = old_executable
    report = Report(evidence)
    kernel = kernel_api()
    handles, observed, sampler_errors = {}, [], []
    disconnects = []
    stop = threading.Event()
    tui_pid = int(tui.console.process.pid)

    def sample():
        while not stop.is_set():
            try:
                result = subprocess.run(
                    [
                        "powershell",
                        "-NoProfile",
                        "-Command",
                        "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Json -Compress",
                    ],
                    capture_output=True,
                    check=True,
                    encoding="utf-8",
                    timeout=20,
                )
                inventory = {row["ProcessId"]: row for row in json.loads(result.stdout)}
                for row in inventory.values():
                    if (
                        row["Name"].lower() != "blender.exe"
                        or row["ProcessId"] in handles
                    ):
                        continue
                    pid, ancestry = row["ProcessId"], []
                    for _ in range(20):
                        if pid == tui_pid or pid not in inventory:
                            break
                        ancestry.append(inventory[pid])
                        pid = inventory[pid]["ParentProcessId"]
                    if pid != tui_pid:
                        continue
                    handle = kernel.OpenProcess(0x00101000, False, row["ProcessId"])
                    if not handle:
                        continue
                    if kernel.WaitForSingleObject(handle, 0) != 258:
                        kernel.CloseHandle(handle)
                        continue
                    handles[row["ProcessId"]] = handle
                    observed.append(
                        {
                            "pid": row["ProcessId"],
                            "created": creation_time(kernel, handle),
                            "observed_ns": time.time_ns(),
                            "ancestry": ancestry,
                            "job_state": json.loads(job_path.read_bytes())["state"]
                            if job_path.exists()
                            else None,
                        }
                    )
                    if args.disconnect:
                        import ctypes

                        # This ancestry was obtained from the exact live Blender to this TUI.
                        host = next(
                            row
                            for row in reversed(ancestry)
                            if row["Name"].lower() in {"node.exe", "dsh.exe"}
                        )
                        host_handle = kernel.OpenProcess(
                            0x00101001, False, host["ProcessId"]
                        )
                        if not host_handle:
                            raise OSError("cannot hold the exact owned dsh host")
                        try:
                            kernel.TerminateProcess.argtypes = [
                                ctypes.c_void_p,
                                ctypes.c_uint,
                            ]
                            kernel.TerminateProcess.restype = ctypes.c_int
                            identity = creation_time(kernel, host_handle)
                            started = time.time_ns()
                            if not kernel.TerminateProcess(host_handle, 19):
                                raise OSError("owned dsh fault injection failed")
                            if kernel.WaitForSingleObject(host_handle, 5000) != 0:
                                raise OSError("owned dsh exit unconfirmed")
                            disconnects.append(
                                {
                                    "pid": host["ProcessId"],
                                    "created": identity,
                                    "started_ns": started,
                                    "confirmed_exit_ns": time.time_ns(),
                                }
                            )
                            stop.set()
                        finally:
                            kernel.CloseHandle(host_handle)
            except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
                sampler_errors.append(type(error).__name__ + ": " + str(error))
            stop.wait(0.1)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        tui.keys(CTRL_U)
        tui.type(
            "真实楼栋取消验收，加载building-modeling技能。inspect_project，select_project_target恢复原target="
            + selection["target"]
            + "、choices="
            + json.dumps([key for key in selection["selected_keys"] if "." in key])
            + "。原楼层及旧楼栋保持。新任务build_id="
            + build_id
            + "：调用assemble_project_building，"
            + (
                "接着inspect_project_building wait_seconds=20等待原任务。测试观察者会在真实Blender运行时注入本次dsh断连。"
                "不要主动cancel，不创建替代任务。"
                if args.disconnect
                else "紧接inspect_project_building wait_seconds=1一次，然后立即cancel_project_building同一ID，"
                "再inspect_project_building和validate_project_building读取实际终态。不要循环等待构建完成后才取消。"
                "若完成竞态已经built就如实报告，不改写为cancelled；只有cancelled且owner_tree_confirmed=true才能说取消确认。"
            )
            + "不要prepare、approve、apply、shell、render或委派。原楼层、授权和旧产物不变，最终回复200字以内。"
        )
        tui.keys(ENTER)
        tui.wait("cancellation running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "cancellation terminal",
            lambda s: (
                " · Completed · " in s
                or " · Failed · " in s
                or (args.disconnect and " · Disconnected · " in s)
            ),
            args.timeout,
        )
        stop.set()
        sampler.join(timeout=25)
        (evidence / "actual-blender-processes.json").write_text(
            json.dumps(
                {
                    "tui_pid": tui_pid,
                    "processes": observed,
                    "sampler_errors": sampler_errors,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        if args.disconnect:
            expect(
                disconnects and observed,
                "actual owned dsh was not interrupted during Blender",
            )
            expect(
                all(kernel.WaitForSingleObject(h, 5000) == 0 for h in handles.values()),
                "actual Blender survived owned dsh cleanup",
            )
            expect(
                all(sha(Path(p)) == digest for p, digest in before.items()),
                "disconnect changed prior business files",
            )
            (evidence / "dsh-disconnect.json").write_text(
                json.dumps(disconnects, indent=2), encoding="utf-8"
            )
            (evidence / "disconnected-screen.txt").write_text(
                tui.console.stable(), encoding="utf-8"
            )
            expect(tui.quit() == 0, "disconnected TUI did not exit cleanly")
            tui.close()
            saved = {str(p): sha(p) for p in job_path.parent.rglob("*") if p.is_file()}
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
            tui.wait(
                "recovery project ready", lambda s: "建模 · " in s and "Ready" in s, 120
            )
            tui.keys(CTRL_U)
            tui.type(
                "只读恢复上次楼栋构建断连，加载building-modeling技能。inspect_project，"
                "select_project_target恢复target="
                + selection["target"]
                + "、choices="
                + json.dumps([key for key in selection["selected_keys"] if "." in key])
                + "。从recovery_index检查原build_id="
                + build_id
                + "，只调用inspect_project_building、"
                "validate_project_building，原未终态且无owner应为unknown，不自动assemble、build、cancel或重放。"
                "不prepare、apply、shell或委派。报告实际未知结果和需要新请求的边界，最终回复200字以内。"
            )
            tui.keys(ENTER)
            tui.wait("recovery running", lambda s: " · Running · " in s, 60)
            tui.wait(
                "recovery terminal",
                lambda s: " · Completed · " in s or " · Failed · " in s,
                args.timeout,
            )
            expect(" · Failed · " not in tui.screen(), "read-only recovery failed")
            expect(
                {str(p): sha(p) for p in job_path.parent.rglob("*") if p.is_file()}
                == saved,
                "recovery replayed or rewrote original work",
            )
            expect(
                all(sha(Path(p)) == digest for p, digest in before.items()),
                "recovery changed prior business files",
            )
            facts = [
                json.loads(p.read_bytes())
                for p in (context.parent / "project/facts").glob("*.json")
            ]
            restored = [
                f
                for f in facts
                if f.get("kind") == "project_building"
                and f.get("project_building", {}).get("build_id") == build_id
                and f["project_building"]["state"] == "unknown"
                and f["project_building"].get("valid") is False
            ]
            expect(restored, "new TUI did not seal unknown invalid result")
            tui.keys(CTRL_HOME)
            for _ in range(100):
                screen = tui.console.stable()
                if "楼栋 " + build_id + " · unknown" in screen:
                    break
                tui.keys("\x1b[6~")
            else:
                raise AssertionError("Core unknown building projection absent")
            (evidence / "recovered-screen.txt").write_text(screen, encoding="utf-8")
            (evidence / "recovered-fact.json").write_text(
                json.dumps(restored[-1], indent=2), encoding="utf-8"
            )
            report.step(
                "actual unmodified Blender interrupted with owned dsh and read-only new TUI recovers unknown without replay",
                lambda: None,
                tui,
            )
            report.step(
                "recovered TUI exits cleanly",
                lambda: expect(tui.quit() == 0, "nonzero exit"),
                tui,
            )
            return
        if args.controlled_pause:
            marker = json.loads(
                (job_path.parent / "real-blender-cancel-gate.json").read_bytes()
            )
            expect(
                marker["controlled_pause"] is True and marker["pid"] in handles,
                "controlled gate did not run inside the observed actual Blender",
            )
        expect(" · Failed · " not in tui.screen(), "cancellation Agent failed")
        job = json.loads(job_path.read_bytes())
        (evidence / "cancel-receipt.json").write_text(
            json.dumps(job, indent=2), encoding="utf-8"
        )
        expect(
            job["state"] == "cancelled" and job.get("owner_tree_confirmed") is True,
            "actual job did not confirm cancellation; retain completion race or diagnostic",
        )
        expect(
            observed and any(p["job_state"] == "building" for p in observed),
            "no actual live Blender process was observed under this exact TUI",
        )
        expect(
            all(kernel.WaitForSingleObject(h, 5000) == 0 for h in handles.values()),
            "actual Blender remains alive",
        )
        expect(
            all(sha(Path(p)) == digest for p, digest in before.items()),
            "cancellation changed prior business files",
        )
        tui.wait(
            "cancellation facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if "楼栋 " + build_id + " · cancelled" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("Core cancelled building projection absent")
        (evidence / "cancelled-screen.txt").write_text(screen, encoding="utf-8")
        report.step(
            (
                "actual Blender paused before unchanged production script, then cancelled by TUI with confirmed tree exit"
                if args.controlled_pause
                else "actual unmodified Blender job cancelled by TUI with confirmed tree exit and original hashes unchanged"
            ),
            lambda: None,
            tui,
        )
        report.step(
            "building cancellation TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "building cancellation acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        stop.set()
        sampler.join(timeout=25)
        (evidence / "actual-blender-processes.json").write_text(
            json.dumps(
                {
                    "tui_pid": tui_pid,
                    "processes": observed,
                    "sampler_errors": sampler_errors,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        if disconnects:
            (evidence / "dsh-disconnect.json").write_text(
                json.dumps(disconnects, indent=2), encoding="utf-8"
            )
        for handle in handles.values():
            kernel.CloseHandle(handle)
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
