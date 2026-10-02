"""Optional real Codex app-server + real CLI JSONL, with localhost-only model traffic."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--codex")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    binary = args.binary.resolve()
    codex = args.codex or os.environ.get("CODEX_BIN") or shutil.which("codex.cmd" if os.name == "nt" else "codex")
    assert codex, "Codex 0.159.2 is required"
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0

    with tempfile.TemporaryDirectory(prefix="live-codex-jsonl-", dir=repo / "target") as temp, ExitStack() as owners:
        root = Path(temp)
        provider = subprocess.Popen([sys.executable, "-u", str(repo / "tests/fixtures/gate_provider.py"), "--single-agent"],
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=flags)
        def cleanup_provider():
            provider.stdin.close()
            try:
                provider.wait(timeout=3)
            except subprocess.TimeoutExpired:
                provider.kill()
                provider.wait(timeout=3)
            provider.stdout.close()
            provider.stderr.close()
        owners.callback(cleanup_provider)
        port = int(provider.stdout.readline().strip())
        env = dict(os.environ, CODEX_HOME=str(root / "home"))
        env.pop("OPENAI_API_KEY", None)
        env.pop("CODEX_API_KEY", None)
        home = root / "home"
        home.mkdir()
        version = subprocess.run([codex, "--version"], env=env, capture_output=True, timeout=15, creationflags=flags)
        assert version.returncode == 0 and b"0.159.2" in version.stdout, "This fixture requires the pinned Codex 0.159.2 baseline"
        bundled = subprocess.run([codex, "debug", "models", "--bundled"], env=env, capture_output=True, timeout=15, creationflags=flags)
        assert bundled.returncode == 0, bundled.stderr.decode()
        catalog = json.loads(bundled.stdout)
        for model in catalog["models"]:
            model["use_responses_lite"] = False
        catalog_path = home / "models.json"
        catalog_path.write_text(json.dumps(catalog))
        config = f'''model = "gpt-6.1-sol"
model_provider = "jsonl_fixture"
model_catalog_json = {json.dumps(str(catalog_path))}
[model_providers.jsonl_fixture]
name = "Local JSONL fixture"
base_url = "http://127.0.0.1:{port}/v1"
wire_api = "responses"
requires_openai_auth = false
'''
        (home / "config.toml").write_text(config)
        command = [str(binary), "--codex", codex, "--cwd", str(repo), "--journal-dir", str(root / "journal"),
                   "--run", "PRIVATE_SINGLE_TASK", "--json-events", "--sandbox", "read-only"]
        if os.name == "nt":
            command += ["--windows-sandbox", "unelevated"]
        result = subprocess.run(command, env=env, cwd=repo, capture_output=True, timeout=45, creationflags=flags)
        assert result.returncode == 0, (result.returncode, result.stderr.decode())
        assert b"PRIVATE_" not in result.stdout + result.stderr
        records = [json.loads(line) for line in result.stdout.splitlines()]
        assert records[0]["kind"] == "snapshot" and records[0]["event_seq"] == 0
        assert records[-1]["kind"] == "snapshot"
        final = records[-1]["payload"]
        assert final["session_closed"] and final["cleanup_confirmed"] and final["execution_result"] == "completed"
        assert final["root_start_requests"] == 1
        root_turn = next(activity for activity in final["observation"]["activities"] if activity["scope"] == "turn")
        assert root_turn["output_bytes"] > 0 and root_turn["progress_seq"] > 0
        assert root_turn["provider_state"] is None
        assert [record["event_seq"] for record in records[:-1]] == list(range(records[-1]["event_seq"] + 1))
        session = records[0]["session_id"]
        replay = subprocess.run([str(binary), "--cwd", str(repo), "--codex", str(root / "never-execute"),
                                 "--journal-dir", str(root / "journal"), "--replay", session, "--json-events"],
                                env=env, capture_output=True, timeout=10, creationflags=flags)
        assert replay.returncode == 0 and not replay.stderr
        history = [json.loads(line) for line in replay.stdout.splitlines()]
        assert history[-2]["payload"] == final
        assert history[-1]["payload"]["execution_result"] == "completed" and not history[-1]["payload"]["live_attached"]
        print("Real Codex 0.159.2 CLI JSONL: passed (localhost model, one root, durable Completed, read-only replay)")


if __name__ == "__main__":
    main()
