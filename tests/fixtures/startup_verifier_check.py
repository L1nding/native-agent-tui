"""Verify fail-fast startup reporting without requiring Codex or starting a model."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("startup_verifier", Path(__file__).resolve().parents[2] / "scripts/check_startup.py")
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


def healthy():
    return {"phase": "Ready", "ready": True, "root_turn_count": 0, "root_start_requests": 0,
            "cleanup_confirmed": True, "journal_confirmed": True, "shell_preflight_timeout": False}


def response(facts, code=0):
    return subprocess.CompletedProcess([], code, b"PRIVATE_RUNNER_TEXT\n" + verifier.PREFIX + json.dumps(facts).encode() + b"\n")


class StartupVerifierTests(unittest.TestCase):
    def run_trials(self, outcomes, *, cargo_test=False):
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            if command == ["cargo", "nextest", "--version"]:
                return subprocess.CompletedProcess(command, 0)
            return outcomes.pop(0)

        output = io.StringIO()
        arguments = ["check_startup.py", "--trials", "3"] + (["--cargo-test"] if cargo_test else [])
        with patch.object(verifier.sys, "argv", arguments), patch.object(verifier.os, "name", "nt"), \
                patch.object(verifier.subprocess, "run", side_effect=run), contextlib.redirect_stdout(output):
            code = verifier.main()
        self.assertNotIn("PRIVATE_RUNNER_TEXT", output.getvalue())
        records = [json.loads(line) for line in output.getvalue().splitlines()]
        return code, records, [command for command in calls if command != ["cargo", "nextest", "--version"]]

    def test_success_runs_only_requested_zero_retry_startup_test(self):
        code, records, calls = self.run_trials([response(healthy()) for _ in range(3)])
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 3)
        self.assertEqual(records[-1]["passed_trials"], 3)
        for command in calls:
            self.assertEqual(command[-1], verifier.TEST)
            self.assertEqual(command[-3:], ["--", "--exact", verifier.TEST])
            self.assertEqual(command[command.index("--retries") + 1], "0")
            self.assertEqual(command[command.index("--no-tests") + 1], "fail")
            self.assertEqual(command[command.index("--run-ignored") + 1], "only")

    def test_timeout_stops_before_another_startup_attempt(self):
        facts = healthy() | {"phase": "Unknown", "ready": False, "shell_preflight_timeout": True}
        code, records, calls = self.run_trials([response(facts, 100)])
        self.assertEqual((code, len(calls)), (1, 1))
        self.assertEqual(records[-1]["completed_trials"], 1)
        self.assertTrue(records[0]["facts"]["shell_preflight_timeout"])

    def test_missing_summary_cannot_pass_even_when_runner_returns_zero(self):
        code, records, calls = self.run_trials([subprocess.CompletedProcess([], 0, b"PRIVATE_RUNNER_TEXT")])
        self.assertEqual((code, len(calls)), (1, 1))
        self.assertFalse(records[0]["summary_available"])

    def test_execution_or_uncertain_cleanup_cannot_pass(self):
        for change in [{"root_start_requests": 1}, {"root_turn_count": 1},
                       {"cleanup_confirmed": False}, {"journal_confirmed": False}]:
            with self.subTest(change=change):
                code, _, calls = self.run_trials([response(healthy() | change)])
                self.assertEqual((code, len(calls)), (1, 1))

    def test_failed_runner_cannot_pass_with_healthy_facts(self):
        code, _, calls = self.run_trials([response(healthy(), 100)])
        self.assertEqual((code, len(calls)), (1, 1))

    def test_cargo_fallback_filters_exactly_one_ignored_test(self):
        code, _, calls = self.run_trials([response(healthy()) for _ in range(3)], cargo_test=True)
        self.assertEqual(code, 0)
        for command in calls:
            self.assertEqual(command[:4], ["cargo", "test", "--locked", "--lib"])
            self.assertEqual(command[4], verifier.TEST)
            self.assertIn("--exact", command)
            self.assertIn("--test-threads=1", command)

    def test_cargo_harness_prefix_preserves_verified_facts(self):
        encoded = json.dumps(healthy()).encode()
        self.assertEqual(verifier.startup_facts(verifier.CARGO_PREFIX + encoded), healthy())
        self.assertIsNone(verifier.startup_facts(b"PRIVATE_RUNNER_TEXT " + verifier.PREFIX + encoded))

    def test_malformed_or_private_summary_is_discarded(self):
        for facts in [healthy() | {"phase": "PRIVATE_PHASE"}, healthy() | {"extra": "PRIVATE_METADATA"},
                      healthy() | {"ready": "true"}, healthy() | {"root_start_requests": False}]:
            with self.subTest():
                self.assertIsNone(verifier.startup_facts(verifier.PREFIX + json.dumps(facts).encode()))
        self.assertIsNone(verifier.startup_facts(verifier.PREFIX + b"{}\n" + verifier.PREFIX + b"{}"))


if __name__ == "__main__":
    unittest.main()
