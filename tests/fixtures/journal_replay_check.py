"""End-to-end durable fixture, Python consumer, and read-only CLI checks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    fixture_binary, binary = args.fixture.resolve(), args.binary.resolve()
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0

    def run(command, **kwargs):
        return subprocess.run(command, cwd=repo, capture_output=True, timeout=30,
                              creationflags=flags, **kwargs)

    def files(root):
        return {p.name: (hashlib.sha256(p.read_bytes()).hexdigest(), p.stat().st_mtime_ns)
                for p in root.iterdir()}

    with tempfile.TemporaryDirectory(prefix="journal-validation-", dir=repo / "target") as temp:
        temp = Path(temp)
        root = temp / "history"
        generated = run([str(fixture_binary), "--journal-dir", str(root)])
        assert generated.returncode == 0, generated.stderr.decode()
        records = [json.loads(line) for line in generated.stdout.splitlines()]
        session = records[0]["session_id"]
        consumer = run([sys.executable, str(repo / "tests/fixtures/journal_consumer.py")],
                       input=generated.stdout)
        assert consumer.returncode == 0, consumer.stderr.decode()
        print(consumer.stdout.decode().strip())
        # Executing this selected Codex would leave a marker even if launch then failed.
        poison = temp / ("poison.cmd" if os.name == "nt" else "poison")
        marker = temp / "executed.marker"
        poison.write_text('@echo off\necho launched>"%~dp0executed.marker"\nexit /b 83\n'
                          if os.name == "nt" else '#!/bin/sh\ntouch "$(dirname "$0")/executed.marker"\nexit 83\n')
        if os.name != "nt":
            poison.chmod(0o700)
        common = [str(binary), "--cwd", str(repo), "--journal-dir", str(root), "--codex", str(poison)]
        before = files(root)
        replay = run(common + ["--replay", session, "--json-events"])
        assert replay.returncode == 0 and not replay.stderr, replay.stderr.decode()
        assert replay.stdout == generated.stdout
        assert files(root) == before and not marker.exists()
        cursor = run(common + ["--replay", session, "--since", "1", "--json-events"])
        assert cursor.returncode == 0
        cursor_records = [json.loads(line) for line in cursor.stdout.splitlines()]
        assert [r["event_seq"] for r in cursor_records] == [1, 2, 3, 3, 3]
        assert cursor_records[0]["kind"] == "snapshot"
        assert run(common + ["--sessions"]).returncode == 0
        for suffix in [["--replay", "../escape"], ["--replay", "missing"],
                       ["--replay", session, "--since", "4"],
                       ["--run", "PRIVATE_PROMPT", "--since", "0", "--json-events"]]:
            rejected = run(common + suffix)
            assert rejected.returncode == 2 and not rejected.stdout
        assert files(root) == before and not marker.exists()
        log = next(root.glob("*.jsonl"))
        with log.open("ab") as stream:
            stream.write(b'{"partial":')
        torn_before = files(root)
        torn = run(common + ["--replay", session, "--json-events"])
        assert torn.returncode == 0
        end = json.loads(torn.stdout.splitlines()[-1])["payload"]
        assert end["uncommitted_tail"] and end["needs_recovery"]
        assert not end["live_attached"] and end["execution_result"] == "unknown"
        assert files(root) == torn_before and not marker.exists()
        print("CLI journal replay: passed (zero execution, zero writes, pinned cursor, torn tail)")


if __name__ == "__main__":
    main()
