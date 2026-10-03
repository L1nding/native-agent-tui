"""Run repository checks and native fixtures; stop at the first failed command."""
import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys


REPO = Path(__file__).resolve().parents[1]


def command_text(command):
    return subprocess.list2cmdline(command) if os.name == "nt" else shlex.join(command)


def run(command, **kwargs):
    print(f"\n> {command_text(command)}", flush=True)
    return subprocess.run(command, cwd=REPO, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", help="Installed rustup toolchain, for example 1.96.0")
    parser.add_argument("--cargo-test", action="store_true", help="Use cargo test even when nextest is installed")
    parser.add_argument("--fixtures-only", action="store_true", help="Use existing release binaries; skip Rust checks/build")
    parser.add_argument("--live", action="store_true", help="Also run ignored tests requiring the supported local Codex")
    args = parser.parse_args()
    cargo = ["cargo"] + ([f"+{args.toolchain}"] if args.toolchain else [])
    binary = REPO / "target/release" / ("native-agent-tui.exe" if os.name == "nt" else "native-agent-tui")
    fixture = binary.parent / "examples" / ("observation_fixture.exe" if os.name == "nt" else "observation_fixture")
    nextest = False
    if not args.fixtures_only or args.live:
        nextest = not args.cargo_test and subprocess.run(
            cargo + ["nextest", "--version"], cwd=REPO, capture_output=True
        ).returncode == 0
    if not args.fixtures_only:
        for arguments in [
            ["fmt", "--all", "--", "--check"],
            ["check", "--locked", "--all-targets"],
            ["clippy", "--locked", "--all-targets"],
            ["nextest", "run", "--locked"] if nextest else ["test", "--locked", "--all-targets"],
            ["test", "--locked", "--doc"],
            ["build", "--locked", "--release", "--examples", "--bin", "native-agent-tui"],
        ]:
            run(cargo + arguments)
    if not binary.is_file() or not fixture.is_file():
        parser.error("release binaries are missing; run without --fixtures-only to build them")
    fixtures = REPO / "tests/fixtures"
    run([sys.executable, str(REPO / "scripts/check_protocol_schema.py")])
    run([sys.executable, str(fixtures / "startup_verifier_check.py")])
    run([sys.executable, str(fixtures / "compatibility_cli_check.py"), "--binary", str(binary)])
    for script in ["history_cli_check.py", "journal_replay_check.py"]:
        run([sys.executable, str(fixtures / script), "--fixture", str(fixture), "--binary", str(binary)])
    # Keep JSONL as bytes; shell pipelines can recode text and hide the producer exit code.
    generated = run([str(fixture)], stdout=subprocess.PIPE)
    run([sys.executable, str(fixtures / "observation_consumer.py")], input=generated.stdout)
    run([sys.executable, str(fixtures / "live_jsonl_check.py"), "--binary", str(binary)])
    if os.name == "nt":
        run([sys.executable, str(fixtures / "windows_terminal_input_check.py"), "--fixture", str(binary.parent / "examples/input_fixture.exe")])
        run([sys.executable, str(fixtures / "timeline_tui_check.py"), "--binary", str(binary)])
        run([sys.executable, str(fixtures / "process_identity_check.py")])
        run([sys.executable, str(fixtures / "process_ownership_check.py"), "--fixture", str(binary.parent / "examples/process_fixture.exe")])
    if args.live:
        arguments = ["nextest", "run", "--locked", "--run-ignored", "only", "--no-capture", "--no-fail-fast"] if nextest else [
            "test", "--locked", "--all-targets", "--", "--ignored", "--nocapture", "--test-threads=1"
        ]
        run(cargo + arguments, env=dict(os.environ, NATIVE_AGENT_TUI_PYTHON=sys.executable))
    print("\nRepository verification passed.", flush=True)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(f"Verification stopped: command exited with {error.returncode}.", file=sys.stderr)
        sys.exit(1)
    except OSError as error:
        print(f"Verification could not start: {error}", file=sys.stderr)
        sys.exit(1)
