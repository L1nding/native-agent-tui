"""Native read-only recovery and export checks; Codex must never be launched."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    repo = Path(__file__).resolve().parents[2]
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0

    def run(command):
        return subprocess.run(command, cwd=repo, capture_output=True, timeout=15, creationflags=flags)

    def contents(root):
        return {p.name: (hashlib.sha256(p.read_bytes()).hexdigest(), p.stat().st_mtime_ns)
                for p in root.iterdir()}

    with tempfile.TemporaryDirectory(prefix="history-cli-", dir=repo / "target") as temp:
        root = Path(temp)
        journal = root / "journal"
        generated = run([str(fixture), "--journal-dir", str(journal)])
        assert generated.returncode == 0, generated.stderr.decode()
        source = [json.loads(line) for line in generated.stdout.splitlines()]
        session = source[0]["session_id"]
        before = contents(journal)
        poison = root / ("poison.cmd" if os.name == "nt" else "poison")
        marker = root / "executed.marker"
        poison.write_text('@echo off\necho launched>"%~dp0executed.marker"\nexit /b 83\n'
                          if os.name == "nt" else '#!/bin/sh\ntouch "$(dirname "$0")/executed.marker"\nexit 83\n')
        if os.name != "nt":
            poison.chmod(0o700)
        common = [str(binary), "--cwd", str(repo), "--journal-dir", str(journal), "--codex", str(poison)]
        preview = run(common + ["--export", session])
        assert preview.returncode == 0, preview.stderr.decode()
        assert b"stable_aliases" in preview.stdout and b"PRIVATE_" not in preview.stdout
        assert session.encode() not in preview.stdout
        for since in [0, 1, 3]:
            output = root / f"中文 export-{since}.jsonl"
            result = run(common + ["--export", session, "--since", str(since), "--output", str(output)])
            assert result.returncode == 0, result.stderr.decode()
            text = output.read_text(encoding="utf-8")
            assert "PRIVATE_" not in text and session not in text
            values = [json.loads(line) for line in text.splitlines()]
            manifest, records = values[0], values[1:]
            assert manifest["export_version"] == 1 and manifest["source_schema_version"] == 2
            assert manifest["since"] == since and manifest["high_watermark"] == 3
            assert manifest["needs_recovery"] and manifest["execution_result"] == "unknown"
            assert manifest["identities"] == "stable_aliases" and not manifest["reverse_mapping_included"]
            assert manifest["content_not_retained"] == ["prompts", "messages", "commands", "questions", "answers", "configuration", "raw_errors"]
            assert [r["event_seq"] for r in records] == [since, *range(since + 1, 4), 3, 3]
            assert all(r["historical"] and r["schema_version"] == 2 for r in records)
            assert len({r["session_id"] for r in records}) == 1
            assert records[-1]["payload"]["needs_recovery"] and not records[-1]["payload"]["live_attached"]
            assert records[-2]["payload"]["execution_result"] == "unknown"
            activities = records[-2]["payload"]["observation"]["activities"]
            assert all(a["freshness"] != "current" for a in activities)
            assert len(activities) == len(source[-2]["payload"]["observation"]["activities"])
            # Relations survive aliasing. Match root by its retained task identity.
            root_activity = next(a for a in activities if a["identity"]["task_id"] == 1 and a["scope"] == "turn")
            targets = root_activity["wait_targets"]
            assert len(targets) == 2 and all(t["outcome"] is None for t in targets)
            child_ids = {a["identity"]["thread_id"] for a in activities if a["scope"] == "turn"}
            assert all(t["thread_id"] in child_ids for t in targets)
            assert [t["silence_ms"] for t in targets] == [0, 131000]
            content = output.read_bytes()
            rejected = run(common + ["--export", session, "--output", str(output)])
            assert rejected.returncode == 2 and output.read_bytes() == content
        for options in [["--export", session, "--output", str(journal / "inside.jsonl")],
                        ["--export", session, "--since", "4"], ["--export", "missing"],
                        ["--history", session], ["--export", session, "--json-events"]]:
            rejected = run(common + options)
            assert rejected.returncode == 2, (options, rejected.stderr.decode())
        assert contents(journal) == before and not marker.exists()
        assert not list(root.glob(".native-agent-export-*.tmp"))
        # A schema 1 journal is still exportable without silently upgrading its records.
        log = next(journal.glob("*.jsonl"))
        records = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
        for record in records:
            record["schema_version"] = 1
            record["payload"].pop("last_headless_action", None)
        legacy = "".join(json.dumps(record, separators=(",", ":")) + "\n" for record in records).encode()
        log.write_bytes(legacy)
        cursor = next(journal.glob("*.cursor"))
        metadata = json.loads(cursor.read_text())
        metadata.update(schema_version=1, committed_bytes=len(legacy))
        cursor.write_text(json.dumps(metadata))
        before = contents(journal)
        output = root / "legacy.jsonl"
        exported = run(common + ["--export", session, "--since", "1", "--output", str(output)])
        assert exported.returncode == 0, exported.stderr.decode()
        values = [json.loads(line) for line in output.read_text().splitlines()]
        assert values[0]["source_schema_version"] == 1
        assert all(r["schema_version"] == 1 for r in values[1:])
        assert contents(journal) == before and not marker.exists()
        print("CLI history/export: passed (aliased identities, preserved waiting evidence, range preview, no clobber, zero execution/writes to journal)")


if __name__ == "__main__":
    main()
