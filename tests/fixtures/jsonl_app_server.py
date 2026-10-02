"""Deterministic subprocess transport fixture; records only IDs and method names."""
import json
import os
from pathlib import Path
import subprocess
import sys

if "debug" in sys.argv and "models" in sys.argv:
    print(json.dumps({"models": [{"slug": "fixture-model", "tool_mode": "code"}]}))
    sys.exit(0)

root = Path(os.environ["NATIVE_JSONL_FIXTURE_ROOT"])
mode = os.environ.get("NATIVE_JSONL_FIXTURE_MODE", "success")
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
            send({"id": message["id"], "result": {}})
        elif method == "thread/start":
            send({"id": message["id"], "result": {"thread": {"id": "root"}, "model": "fixture-model"}})
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
