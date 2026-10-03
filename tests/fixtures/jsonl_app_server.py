"""Deterministic subprocess transport fixture; records only IDs and method names."""
import json
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
from windows_process_identity import record as record_processes

root = Path(os.environ["NATIVE_JSONL_FIXTURE_ROOT"])
mode = os.environ.get("NATIVE_JSONL_FIXTURE_MODE", "success")
stage = "version" if "--version" in sys.argv else "catalog" if "debug" in sys.argv and "models" in sys.argv else "app-server"
with (root / "startup.jsonl").open("a") as record:
    record.write(json.dumps({"stage": stage}) + "\n")
stages = [json.loads(line)['stage'] for line in (root / 'startup.jsonl').read_text().splitlines()]
is_peer = stage == 'app-server' and stages.count('app-server') > 1
if stage == "version":
    if mode == "version_hang":
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"],
                                 creationflags=subprocess.CREATE_NO_WINDOW) if os.name == "nt" else None
        record_processes(root, [os.getpid(), child.pid] if child else [os.getpid()])
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
    if mode == "peer_version_bad" and stages.count('version') > 1:
        print("PRIVATE_PEER_BANNER")
    elif mode == "version_bad":
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
record_processes(root, [os.getpid(), child.pid])
turn_count = 0
turn_id = "turn"
detail_stage = 0
file_done = False
input_sent = False
input_paste_answered = False


def send(value):
    print(json.dumps(value, ensure_ascii=True), flush=True)


def terminal(status="completed"):
    send({"method": "turn/completed", "params": {"threadId": "root",
          "turn": {"id": turn_id, "status": status,
                   "error": {"message": "PRIVATE_ERROR"} if status == "failed" else None}}})


def approval(request_id):
    params = {"threadId": "root", "turnId": turn_id, "command": "PRIVATE_COMMAND"}
    if mode == "request_details":
        params.update(itemId="shell-detail", kind="command", startedAtMs=123,
                      cwd="PRIVATE_COMMAND_DIRECTORY", reason="Review command 中文👋",
                      availableDecisions=["accept", "decline", "acceptForSession"],
                      additionalPermissions={"network": {"enabled": True}})
    if mode == "approval_no_decline":
        params["availableDecisions"] = ["accept"]
    if mode == "approval_cancel":
        params.update(itemId="cancel-command", cwd="PRIVATE_COMMAND_DIRECTORY",
                      availableDecisions=["accept", "cancel"])
    send({"id": request_id, "method": "item/commandExecution/requestApproval",
          "params": params})


def details_input():
    global input_sent
    if detail_stage == 2 and file_done and not input_sent:
        input_sent = True
        send({"id": "input-request", "method": "item/tool/requestUserInput", "params": {
            "threadId": "root", "turnId": turn_id, "itemId": "input-detail", "questions": [
                {"id": "question", "header": "Secret", "question": "Enter fixture secret 中文👋",
                 "isSecret": True, "options": [{"label": "Fixture", "description": "PRIVATE_OPTION_DETAIL"}]}]}})


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
            if mode == "initialize_bad" or is_peer and mode == "peer_initialize_bad":
                response["result"] = {"userAgent": "PRIVATE_METADATA"}
            send(response)
        elif method == "thread/start":
            send({"id": message["id"], "result": {"thread": {"id": "root", "cliVersion": "PRIVATE_VERSION" if mode == "thread_version_bad" else "0.159.2"}, "model": "fixture-model"}})
        elif method == "command/exec":
            send({"id": message["id"], "result": {"exitCode": 1 if mode == "peer_shell_bad" and is_peer else 0, "stdout": "native-agent-tui-shell-ok"}})
        elif method == "turn/start":
            turn_count += 1
            turn_id = f"turn-{turn_count}"
            send({"id": message["id"], "result": {"turn": {"id": turn_id}}})
            send({"method": "item/agentMessage/delta", "params": {"threadId": "root", "turnId": turn_id, "itemId": "message", "delta": "PRIVATE_OUTPUT 中文"}})
            if mode.startswith("approval") or mode == "request_details":
                approval(7)
            elif mode in ("input", "input_hang", "input_paste"):
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
        elif not method and mode == "approval_cancel" and message.get("id") == 7:
            assert message["result"]["decision"] == "cancel"
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": 7}})
            terminal("interrupted")
        elif not method and mode == "request_details" and message.get("id") == 7:
            assert message["result"]["decision"] in ("accept", "decline")
            detail_stage += 1
            assert detail_stage <= 2
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": 7}})
            if detail_stage == 1:
                send({"method": "item/started", "params": {"threadId": "root", "turnId": turn_id, "item": {
                    "id": "file-detail", "type": "fileChange", "status": "inProgress", "changes": [
                        {"path": "PRIVATE_FILE_中文.rs", "kind": {"type": "update", "move_path": None},
                         "diff": "-PRIVATE_BEFORE\n+PRIVATE_AFTER 中文👋\n"}]}}})
                send({"id": "file-detail", "method": "item/fileChange/requestApproval", "params": {
                    "threadId": "root", "turnId": turn_id, "itemId": "file-detail", "startedAtMs": 124,
                    "reason": "Review file change", "grantRoot": "PRIVATE_PROPOSED_ROOT"}})
                approval(7)  # Same RPC ID and turn; this is a new accepted delivery.
            details_input()
        elif not method and mode == "request_details" and message.get("id") == "file-detail":
            assert message["result"]["decision"] in ("accept", "decline")
            assert not file_done
            file_done = True
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": "file-detail"}})
            details_input()
        elif not method and mode == "request_details" and message.get("id") == "input-request":
            answers = message["result"]["answers"]
            (root / "answers.json").write_text(json.dumps({"valid_fixture_answer": answers == {
                "question": {"answers": ["秘密回答中文👋"]}}}))
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": "input-request"}})
        elif not method and mode == "input_paste" and message.get("id") == "input-request":
            assert not input_paste_answered
            input_paste_answered = True
            valid = message["result"]["answers"] == {"question": {"answers": ["秘密回答中文👋\n第二行\n第三行"]}}
            (root / "answers.json").write_text(json.dumps({"valid_fixture_answer": valid, "answer_count": 1}))
            assert valid
            send({"method": "serverRequest/resolved", "params": {"threadId": "root", "requestId": "input-request"}})
            terminal()
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
