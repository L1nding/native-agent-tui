"""Exercise the real CLI/pipe/journal path without authentication or model calls."""
import argparse
import copy
from contextlib import ExitStack
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from windows_process_identity import check_clean


def records(output):
    return [json.loads(line) for line in output.splitlines()]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    repo = Path(__file__).resolve().parents[2]
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0

    def consume(output, expected):
        result = subprocess.run([sys.executable, str(repo / "tests/fixtures/live_event_consumer.py")],
                                input=output, capture_output=True, timeout=5, creationflags=flags)
        assert result.returncode == expected, (result.returncode, result.stderr.decode())
        assert b"PRIVATE_" not in result.stdout + result.stderr
        if expected != 2:
            return json.loads(result.stdout)
        assert not result.stdout and b"Traceback" not in result.stderr

    with tempfile.TemporaryDirectory(prefix="live-jsonl-", dir=repo / "target") as temp, ExitStack() as owners:
        temp = Path(temp)
        fake = temp / ("codex.cmd" if os.name == "nt" else "codex")
        script = repo / "tests/fixtures/jsonl_app_server.py"
        fake.write_text(f'@echo off\n"{sys.executable}" -u "{script}" %*\n' if os.name == "nt"
                        else f'#!/bin/sh\nexec "{sys.executable}" -u "{script}" "$@"\n')
        if os.name != "nt":
            fake.chmod(0o700)

        invalid = temp / "invalid-workflow.json"
        invalid.write_text(json.dumps({"PRIVATE_FIELD": "PRIVATE_PROMPT"}))
        journal = temp / "must-not-start-journal"
        common = [str(binary), "--cwd", str(repo), "--codex", str(fake),
                  "--journal-dir", str(journal), "--json-events"]
        for options in [["--workflow", str(invalid), "--headless"],
                        ["--run", "PRIVATE_PROMPT", "--sandbox", "PRIVATE_SETTING"]]:
            rejected = subprocess.run(common + options, capture_output=True, timeout=5, creationflags=flags)
            assert rejected.returncode == 2 and not rejected.stdout
            assert b"PRIVATE_" not in rejected.stderr
            assert not journal.exists()
        print("Live JSONL invalid commands: passed (redacted diagnostics, zero execution)")

        def start(mode):
            root = temp / mode
            root.mkdir()
            env = dict(os.environ, NATIVE_JSONL_FIXTURE_ROOT=str(root), NATIVE_JSONL_FIXTURE_MODE=mode.split("-")[0])
            command = [str(binary), "--cwd", str(repo), "--codex", str(fake), "--journal-dir", str(root / "journal"),
                       "--run", "PRIVATE_PROMPT", "--json-events"]
            if mode == "workflow":
                workflow = root / "workflow.json"
                workflow.write_text(json.dumps({"tasks": [{"text": "PRIVATE_FIRST"}, {"text": "PRIVATE_LAST"}]}))
                command = command[:-3] + ["--workflow", str(workflow), "--headless", "--json-events"]
            process = subprocess.Popen(command, cwd=repo, env=env, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, creationflags=flags)
            def cleanup():
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=3)
                for stream in (process.stdout, process.stderr):
                    stream.close()
            owners.callback(cleanup)
            return root, process

        def replay(root):
            cursor = next((root / "journal").glob("*.cursor"))
            session = json.loads(cursor.read_text())["session_id"]
            command = [str(binary), "--cwd", str(repo), "--codex", str(temp / "never-execute"),
                       "--journal-dir", str(root / "journal"), "--replay", session, "--json-events"]
            result = subprocess.run(command, capture_output=True, timeout=10, creationflags=flags)
            assert result.returncode == 0, result.stderr.decode()
            return records(result.stdout)

        for mode, expected_code, result in [("success", 0, "completed"), ("approval", 0, "completed"),
                                            ("approval_no_decline", 130, "interrupted"),
                                            ("input", 130, "interrupted"), ("input_hang", 4, "unknown"),
                                            ("failure", 1, "failed"), ("workflow", 1, "failed"),
                                            ("disconnect", 4, "unknown")]:
            root, process = start(mode)
            output, error = process.communicate(timeout=15)
            assert process.returncode == expected_code, (mode, process.returncode, error.decode())
            assert b"PRIVATE_" not in output + error
            live = records(output)
            assert live[0]["kind"] == "snapshot" and live[0]["event_seq"] == 0
            assert live[-1]["kind"] == "snapshot"
            assert live[-1]["payload"]["session_closed"] and live[-1]["payload"]["cleanup_confirmed"]
            assert live[-1]["payload"]["execution_result"] == result
            seq = [r["event_seq"] for r in live[:-1]]
            assert seq == list(range(seq[-1] + 1)), (mode, seq)
            assert all(r["schema_version"] == 2 and not r.get("historical") for r in live)
            summary = consume(output, expected_code)
            assert summary["stream_closed"] and summary["execution_result"] == result
            if mode == "workflow":
                assert [t["state"] for t in live[-1]["payload"]["tasks"]] == ["failed", "succeeded"]
                assert live[-1]["payload"]["root_start_requests"] == 2
            if mode == "success":
                truncated = b"\n".join(output.splitlines()[:-1]) + b"\n"
                assert not consume(truncated, 4)["stream_closed"]
                bad_records = []
                for field, value in [("schema_version", 99), ("event_seq", True), ("event_seq", 42),
                                     ("payload", None), ("payload", []),
                                     ("payload", {"session_closed": True, "cleanup_confirmed": "PRIVATE_SECRET"}),
                                     ("payload", {"session_closed": True, "cleanup_confirmed": True, "execution_result": ["PRIVATE_SECRET"]})]:
                    bad = copy.deepcopy(live)
                    bad[-1][field] = value
                    bad_records.append(bad)
                bad = copy.deepcopy(live)
                bad[-1]["payload"].pop("cleanup_confirmed")
                bad_records.append(bad)
                for bad in bad_records:
                    consume(b"\n".join(json.dumps(r).encode() for r in bad) + b"\n", 2)
                consume(b'"PRIVATE_SECRET"\n', 2)
                consume(b'{"PRIVATE_SECRET":', 2)
            history = replay(root)
            assert history[-1]["payload"]["execution_result"] == result
            assert history[-2]["payload"] == live[-1]["payload"]
            actions = [r["payload"].get("last_headless_action") for r in live]
            if mode == "approval":
                assert {a["request_id"] for a in actions if a} == {7, "approval-request"}
                assert all(a["action"] == "declineApproval" for a in actions if a)
            if mode.startswith("input"):
                assert any(a and a["action"] == "interruptForInput" and a["request_id"] == "input-request" for a in actions)
            if mode == "approval_no_decline":
                assert any(a and a["action"] == "interruptForApproval" and a["request_id"] == 7 for a in actions)
            check_clean(root)
            print(f"Live JSONL {mode}: passed (exit {expected_code}, durable {result})")

        root, process = start("success-slow")
        time.sleep(0.3)  # Brief full pipe; Core must still finish independently.
        output, error = process.communicate(timeout=15)
        assert process.returncode == 0, error.decode()
        assert records(output)[-1]["payload"]["execution_result"] == "completed"
        check_clean(root)
        print("Live JSONL temporary slow reader: passed (journal catch-up)")

        root, process = start("hold-broken")
        first = json.loads(process.stdout.readline())
        assert first["kind"] == "snapshot"
        process.stdout.close()
        process.wait(timeout=15)
        error = process.stderr.read()
        assert process.returncode == 4, error.decode()
        assert b"PRIVATE_" not in error
        history = replay(root)
        assert history[-1]["payload"]["execution_result"] == "unknown"
        assert history[-2]["payload"]["last_headless_action"]["action"] == "stopForOutput"
        assert history[-2]["payload"]["cleanup_confirmed"]
        check_clean(root)
        print("Live JSONL broken pipe: passed (owner stopped, durable Unknown, owned processes cleaned)")

        root, process = start("hold-stalled")
        # Keep the read handle open without reading. This tests native blocked writes.
        before = time.monotonic()
        process.wait(timeout=15)
        elapsed = time.monotonic() - before
        error = process.stderr.read()
        assert process.returncode == 4 and elapsed < 12, (elapsed, error.decode())
        process.stdout.close()
        history = replay(root)
        assert history[-1]["payload"]["execution_result"] == "unknown"
        assert history[-2]["payload"]["last_headless_action"]["action"] == "stopForOutput"
        assert history[-2]["payload"]["root_start_requests"] == 1
        assert history[-2]["payload"]["cleanup_confirmed"]
        check_clean(root)
        print("Live JSONL permanently slow reader: passed (bounded exit, independent Core, durable Unknown)")


if __name__ == "__main__":
    main()
