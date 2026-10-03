"""Check real Windows Core startup and cleanup without dispatching a model turn."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


REPO = Path(__file__).resolve().parents[1]
TEST = "client::tests::live_windows_core_reaches_ready_without_a_model_turn"
PREFIX = b"[startup-check] "
CARGO_PREFIX = b"test " + TEST.encode() + b" ... " + PREFIX
PHASES = {"Ready", "Failed", "Unknown", "Disconnected"}
BOOLEAN_FIELDS = {"ready", "cleanup_confirmed", "journal_confirmed", "shell_preflight_timeout"}
COUNT_FIELDS = {"root_turn_count", "root_start_requests"}


def trial_count(value):
    try:
        count = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected 1 to 100 startup trials") from error
    if not 1 <= count <= 100:
        raise argparse.ArgumentTypeError("expected 1 to 100 startup trials")
    return count


def startup_facts(output):
    lines = []
    for line in output.splitlines():
        for prefix in (PREFIX, CARGO_PREFIX):
            if line.startswith(prefix):
                lines.append(line[len(prefix):])
                break
    if len(lines) != 1:
        return None
    try:
        facts = json.loads(lines[0])
    except (ValueError, UnicodeError):
        return None
    if not isinstance(facts, dict) or set(facts) != BOOLEAN_FIELDS | COUNT_FIELDS | {"phase"}:
        return None
    if type(facts["phase"]) is not str or facts["phase"] not in PHASES:
        return None
    if any(type(facts[key]) is not bool for key in BOOLEAN_FIELDS):
        return None
    if any(type(facts[key]) is not int or facts[key] < 0 for key in COUNT_FIELDS):
        return None
    if facts["ready"] != (facts["phase"] == "Ready"):
        return None
    return facts


def emit(record):
    print(json.dumps(record, separators=(",", ":")), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trials", type=trial_count, default=1,
                        help="Independent startup attempts; stop at the first failure (default: 1)")
    parser.add_argument("--cargo-test", action="store_true", help="Use Cargo's test runner")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this check requires native Windows and the supported local Codex installation")
    nextest = not args.cargo_test and subprocess.run(
        ["cargo", "nextest", "--version"], cwd=REPO, capture_output=True
    ).returncode == 0
    command = (["cargo", "nextest", "run", "--locked", "--run-ignored", "only",
                "--retries", "0", "--no-tests", "fail", "--no-capture", "--", "--exact", TEST]
               if nextest else ["cargo", "test", "--locked", "--lib", TEST, "--",
                                "--ignored", "--exact", "--nocapture", "--test-threads=1"])
    passed = 0
    for trial in range(1, args.trials + 1):
        started = time.monotonic()
        result = subprocess.run(command, cwd=REPO, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        facts = startup_facts(result.stdout)
        healthy = facts is not None and (
            result.returncode == 0 and facts["ready"]
            and facts["root_turn_count"] == 0 and facts["root_start_requests"] == 0
            and facts["cleanup_confirmed"] and facts["journal_confirmed"]
            and not facts["shell_preflight_timeout"]
        )
        emit({"kind": "startup_trial", "trial": trial, "passed": healthy,
              "elapsed_seconds": round(time.monotonic() - started, 3),
              "runner_exit_code": result.returncode, "facts": facts,
              "summary_available": facts is not None})
        passed += healthy
        if not healthy:
            break
    emit({"kind": "startup_summary", "requested_trials": args.trials,
          "completed_trials": trial, "passed_trials": passed, "passed": passed == args.trials})
    return 0 if passed == args.trials else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError:
        print("Startup verification could not start its test runner.", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        print("Startup verification interrupted.", file=sys.stderr)
        sys.exit(130)
