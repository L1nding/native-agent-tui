"""Check reasoning, tool updates and restored history through ACP/Core/the Windows TUI.

Runs a deterministic local ACP provider; no model account or network is used.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import uuid

sys.dont_write_bytecode = True

HISTORY = [
    {"id": "shared", "turnId": "saved-turn", "role": "reasoning", "text": "Inspect the probe carefully", "truncated": False},
    {"id": "tool", "turnId": "saved-turn", "role": "tool", "text": "▸ done · echo STREAM_PROBE", "truncated": False},
    {"id": "failed-tool", "turnId": "saved-turn", "role": "tool", "text": "▸ failed · echo FAILED_PROBE", "truncated": False},
    {"id": "shared", "turnId": "saved-turn", "role": "assistant", "text": "Stream answer", "truncated": False},
]


def provider():
    lock = threading.Lock()
    created = 0

    def send(frame):
        with lock:
            print(json.dumps({"jsonrpc": "2.0", **frame}, ensure_ascii=False), flush=True)

    def stream(request):
        session = request["params"]["sessionId"]

        def update(value):
            send({"method": "session/update", "params": {"sessionId": session, "update": value}})

        update({"sessionUpdate": "agent_thought_chunk", "messageId": "shared", "content": {"type": "text", "text": "Inspect the probe"}})
        time.sleep(.7)
        update({"sessionUpdate": "tool_call", "toolCallId": "tool", "title": "pwsh", "kind": "other", "status": "in_progress", "rawInput": {"command": "echo STREAM_PROBE"}})
        time.sleep(.7)
        update({"sessionUpdate": "agent_thought_chunk", "messageId": "shared", "content": {"type": "text", "text": " carefully"}})
        update({"sessionUpdate": "tool_call_update", "toolCallId": "tool", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "STREAM_PROBE"}}]})
        update({"sessionUpdate": "tool_call", "toolCallId": "failed-tool", "title": "pwsh", "kind": "other", "status": "in_progress", "rawInput": {"command": "echo FAILED_PROBE"}})
        time.sleep(.7)
        update({"sessionUpdate": "tool_call_update", "toolCallId": "failed-tool", "status": "failed", "content": []})
        update({"sessionUpdate": "agent_message_chunk", "messageId": "shared", "content": {"type": "text", "text": "Stream answer"}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})

    for line in sys.stdin:
        request = json.loads(line)
        method = request.get("method")
        if "id" not in request:
            continue
        result = {}
        if method == "initialize":
            result = {"protocolVersion": 1, "agentCapabilities": {}}
        elif method == "session/new":
            created += 1
            result = {"sessionId": "stream-session" if created == 1 else "new-session"}
        elif method == "session/prompt":
            threading.Thread(target=stream, args=(request,), daemon=True).start()
            continue
        elif method == "zf/session/list":
            result = {"sessions": [{"id": "stream-session", "title": "Stream fixture", "updatedAt": 2}, {"id": "new-session", "title": "New fixture", "updatedAt": 1}]}
        elif method == "zf/session/history":
            result = {"sessionId": "stream-session", "messages": HISTORY, "omitted": 0}
        elif method == "zf/skills/list":
            result = {"skills": []}
        elif method == "zf/permission/describe":
            result = {"presets": [{"value": "read-only", "name": "read-only"}], "current": "read-only"}
        send({"id": request["id"], "result": result})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/native-agent-tui.exe"))
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this fixture requires Windows ConPTY")
    from conpty import Console
    repo = Path(__file__).resolve().parents[2]
    root = repo / "target" / ("conversation-stream-" + uuid.uuid4().hex[:8])
    root.mkdir()
    launcher = root / "provider.cmd"
    launcher.write_text('@echo off\n' + subprocess.list2cmdline([sys.executable, str(Path(__file__).resolve()), "--provider"]) + '\n', encoding="utf-8")
    previous = os.environ.get("NATIVE_AGENT_TUI_LANG")
    os.environ["NATIVE_AGENT_TUI_LANG"] = "en"
    console = Console([str(args.binary.resolve()), "--cwd", str(root), "--journal-dir", str(root / "journal"), "--dsh", str(launcher)], repo)
    try:
        console.wait(lambda screen: "Ready" in screen, "initial ready")
        console.write("probe\r")
        console.wait(lambda screen: "Thinking" in screen and "Inspect the probe" in screen, "server reasoning is visible")
        console.wait(lambda screen: "running · echo STREAM_PROBE" in screen, "tool call is visible while running")
        (root / "running-screen.txt").write_text(console.read(), encoding="utf-8")
        console.wait(lambda screen: "Stream answer" in screen and "done · echo STREAM_PROBE" in screen and "failed · echo FAILED_PROBE" in screen, "tool terminal states and answer are visible")
        assert "Inspect the probe carefully" in console.read()
        console.wait(lambda screen: "Completed" in screen, "turn completion is confirmed")
        (root / "completed-screen.txt").write_text(console.read(), encoding="utf-8")
        console.write("/new\r")
        console.wait(lambda screen: "New conversation started." in screen, "new session is confirmed")
        console.write("\x1b[D")
        console.wait(lambda screen: "Stream fixture" in screen, "old session is listed")
        console.write("\x1b[B\r")
        console.wait(lambda screen: "Resumed: Stream fixture" in screen, "old session is resumed")
        console.write("\x1b[C")
        console.wait(lambda screen: "Stream answer" in screen and "Inspect the probe carefully" in screen and "done · echo STREAM_PROBE" in screen and "failed · echo FAILED_PROBE" in screen, "restored conversation keeps reasoning and tool states")
        (root / "restored-screen.txt").write_text(console.read(), encoding="utf-8")
        print(f"PASS: reasoning, running/completed/failed tools and restored history; screens: {root}", flush=True)
        console.write("\x11")
        time.sleep(.3)
    except BaseException:
        (root / "failure-screen.txt").write_text(console.read(), encoding="utf-8")
        raise
    finally:
        console.close()
        if previous is None:
            os.environ.pop("NATIVE_AGENT_TUI_LANG", None)
        else:
            os.environ["NATIVE_AGENT_TUI_LANG"] = previous


if __name__ == "__main__":
    if "--provider" in sys.argv:
        provider()
    else:
        main()
