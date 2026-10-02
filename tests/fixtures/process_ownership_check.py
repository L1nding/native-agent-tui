"""Verify creation-time Windows job membership and owner crash cleanup."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this fixture requires Windows")
    fixture = args.fixture.resolve()
    repo = Path(__file__).resolve().parents[2]
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    kernel.OpenProcess.restype = ctypes.c_void_p
    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel.TerminateProcess.argtypes = [ctypes.c_void_p, ctypes.c_uint]

    with tempfile.TemporaryDirectory(prefix="process-ownership-", dir=repo / "target") as temporary:
        base = Path(temporary)
        for mode in ["suspended", "reject", "running", "graceful", "wait-cancelled"]:
            root = base / mode
            root.mkdir()
            owner = subprocess.Popen([str(fixture), mode], cwd=repo, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     env=dict(os.environ, NATIVE_PROCESS_FIXTURE_ROOT=str(root)),
                                     creationflags=subprocess.CREATE_NO_WINDOW)
            handles = []
            try:
                response = queue.Queue()
                reader = threading.Thread(target=lambda: response.put(owner.stdout.readline()), daemon=True)
                reader.start()
                report = json.loads(response.get(timeout=10))
                assert report["in_job"], report
                pids = [report["pid"]]
                if mode in ["running", "graceful", "wait-cancelled"]:
                    pids.append(report["descendant_pid"])
                    deadline = time.monotonic() + 5
                    while not (root / "grandchild.json").exists() and time.monotonic() < deadline:
                        time.sleep(0.02)
                    descendant = json.loads((root / "grandchild.json").read_text())
                    assert descendant["in_job"] and descendant["pid"] == pids[1]
                for pid in pids:
                    handle = kernel.OpenProcess(0x00100001, False, pid)
                    assert handle and kernel.WaitForSingleObject(handle, 0) == 258, pid
                    handles.append(handle)
                if mode in ["suspended", "running"]:
                    owner.kill()  # TerminateProcess: no Rust destructor or async cleanup runs.
                    owner.wait(timeout=5)
                else:
                    owner.stdin.write(b"stop\n")
                    owner.stdin.flush()
                    owner.stdin.close()
                    owner.wait(timeout=5)
                    assert owner.returncode == 0, owner.stderr.read().decode()
                assert all(kernel.WaitForSingleObject(handle, 3000) == 0 for handle in handles)
                if mode in ["suspended", "reject"]:
                    assert not (root / "child.json").exists(), "suspended child ran user code"
                print(f"Windows process {mode}: passed (job membership before resume, owned handles confirm exit)")
            finally:
                if owner.poll() is None:
                    owner.kill()
                    owner.wait(timeout=5)
                for handle in handles:
                    # Clean up exact held process identities even if the regression fails.
                    if kernel.WaitForSingleObject(handle, 0) == 258:
                        kernel.TerminateProcess(handle, 87)
                        kernel.WaitForSingleObject(handle, 3000)
                    kernel.CloseHandle(handle)
                for stream in (owner.stdin, owner.stdout, owner.stderr):
                    if not stream.closed:
                        stream.close()
        values = ["", "中文 🦀", "space value", "trailing\\", 'model_catalog_json="C:\\space & path\\models.json"',
                  "%NATIVE_PROCESS_PRIVATE%", "!NATIVE_PROCESS_PRIVATE!", "a^&b|c<d>e", 'embedded"quote', "$(literal)"]
        for mode in ["executable", "batch", "null-input", "exit-code", "creation-failures", "invalid-input"]:
            root = base / f"{mode} 中文 & %NATIVE_PROCESS_PRIVATE%"
            root.mkdir()
            (root / "wrapper.cmd").write_text(f'@echo off\n"{fixture}" %*\n', encoding="utf-8")
            result = subprocess.run([str(fixture), mode, *values], cwd=repo, capture_output=True, timeout=10,
                                    env=dict(os.environ, NATIVE_PROCESS_FIXTURE_ROOT=str(root), NATIVE_PROCESS_PRIVATE="DO_NOT_EXPAND"),
                                    creationflags=subprocess.CREATE_NO_WINDOW)
            assert result.returncode == 0, result.stderr.decode()
            report = json.loads(result.stdout)
            if mode == "invalid-input":
                assert report["rejected"] == 5, report
                assert not (root / "child.json").exists()
                print("Windows invalid command: passed (NUL, batch newlines, executable/batch length limits)")
                continue
            if mode == "creation-failures":
                assert report["failures"] == 100 and report["after"] <= report["before"] + 4, report
                assert not (root / "child.json").exists()
                print("Windows failed process creation: passed (100 invalid images, bounded handle count)")
                continue
            assert report["in_job"] and report["arguments"] == values, report
            assert Path(report["cwd"]).resolve() == root.resolve()
            if mode == "null-input":
                assert report["stdin_bytes"] == 0
            print(f"Windows process {mode}: passed (Unicode/quoted/literal args, cwd, inherited job, exit code)")


if __name__ == "__main__":
    main()
