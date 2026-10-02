"""Deterministic subprocess transport fixture; records only IDs and method names."""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(os.environ["NATIVE_JSONL_FIXTURE_ROOT"])
mode = os.environ.get("NATIVE_JSONL_FIXTURE_MODE", "success")
stage = "version" if "--version" in sys.argv else "catalog" if "debug" in sys.argv and "models" in sys.argv else "app-server"
with (root / "startup.jsonl").open("a") as record:
    record.write(json.dumps({"stage": stage}) + "\n")
if stage == "version":
    if mode == "version_hang":
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"],
                                 creationflags=subprocess.CREATE_NO_WINDOW) if os.name == "nt" else None
        (root / "pids.json").write_text(json.dumps([os.getpid(), child.pid] if child else [os.getpid()]))
        print("codex-cli 0.159.2", flush=True)
        try:
            import time
            time.sleep(120)
        finally:
            if os.name != "nt" and child:
                child.terminate()
                child.wait()
    if mode == "version_empty":
        sys.exit(0)
    if mode == "version_bad":
        print("codex-cli 0.159.20")
    elif mode == "version_private":
        print("PRIVATE_BANNER 0.159.2")
    elif mode == "version_large":
        print("PRIVATE_BANNER" * 1000)
    else:
        print("codex-cli 0.159.2")
    sys.exit(1 if mode == "version_failure" else 0)

if "debug" in sys.argv and "models" in sys.argv:
    print(json.dumps({"models": [{"slug": "fixture-model", "tool_mode": "code"}]}))
    sys.exit(0)

child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"],
                         creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
(root / "pids.json").write_text(json.dumps([os.getpid(), child.pid]))
turn_count = 0
turn_id = "turn"


def send(value):
    print(json.dumps(value, ensure_ascii=True), flush=True)


def terminal(status="completed"):
    send({"method": "turn/completed", "params": {"threadId": "root",
          "turn": {"id": turn_id, "status": status,
                   "error": {"message": "PRIVATE_ERROR"} if status == "failed" else None}}})


def approval(request_id):
    params = {"threadId": "root", "turnId": turn_id, "command": "PRIVATE_COMMAND"}
    if mode == "approval_no_decline":
        params["availableDecisions"] = ["accept"]
    send({"id": request_id, "method": "item/commandExecution/requestApproval",
          "params": params})


try:
    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        with (root / "rpc.jsonl").open("a") as record:
            record.write(json.dumps({"method": method, "id": message.get("id"),
                                     "decision": message.get("result", {}).get("decision")}) + "\n")
        if method == "initialize":
            response = json.loads((Path(__file__).parent / "codex-0.159.2/initialize.json").read_text())
            response["id"] = message["id"]
            if mode == "initialize_bad":
                response["result"] = {"userAgent": "PRIVATE_METADATA"}
            send(response)
        elif method == "thread/start":
            send({"id": message["id"], "result": {"thread": {"id": "root", "cliVersion": "PRIVATE_VERSION" if mode == "thread_version_bad" else "0.159.2"}, "model": "fixture-model"}})
        elif method == "command/exec":
            send({"id": message["id"], "result": {"exitCode": 0, "stdout": "native-agent-tui-shell-ok"}})
        elif method == "turn/start":
            turn_count += 1
            turn_id = f"turn-{turn_count}"
            send({"id": message["id"], "result": {"turn": {"id": turn_id}}})
            send({"method": "item/agentMessage/delta", "params": {"threadId": "root", "turnId": turn_id, "itemId": "message", "delta": "PRIVATE_OUTPUT 中文"}})
            if mode.startswith("approval"):
                approval(7)
            elif mode in ("input", "input_hang"):
                send({"id": "input-request", "method": "item/tool/requestUserInput",
                      "params": {"threadId": "root", "turnId": turn_id, "questions": [
                          {"id": "question", "header": "PRIVATE_HEADER", "question": "PRIVATE_QUESTION", "isSecret": True}]}})
            elif mode == "failure" or mode == "workflow" and turn_count == 1:
                terminal("failed")
            elif mode == "disconnect":
                break
            elif mode != "hold":
                terminal()
        elif method == "turn/interrupt":
            send({"id": message["id"], "result": {}})
            if mode != "input_hang":
                terminal("interrupted")
        elif not method and message.get("id") == 7:
            assert message["result"]["decision"] == "decline"
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": 7}})
            approval("approval-request")
        elif not method and message.get("id") == "approval-request":
            assert message["result"]["decision"] == "decline"
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": "approval-request"}})
            terminal()
finally:
    # Windows Job cleanup is responsible for the held descendant.
    if os.name != "nt":
        child.terminate()
        child.wait()
