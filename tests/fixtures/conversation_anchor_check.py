"""Keep a scrolled preview fixed when authority replaces it and appends text.

The provider waits for filesystem barriers, so terminal speed cannot race the test.
No account, model, or network is used. Screens are retained under target/.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
import uuid

sys.dont_write_bytecode = True


def provider():
    lock = threading.Lock()

    def send(value):
        with lock:
            print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)

    def stream(request):
        def append(start, end):
            send({"method": "session/update", "params": {
                "sessionId": "anchor-session", "update": {
                    "sessionUpdate": "agent_message_chunk", "messageId": "answer",
                    "content": {"type": "text", "text": "".join(f"ANCHOR_{i:03d}\n" for i in range(start, end))},
                }}})

        send({"method": "zf/session/event", "params": {
            "sessionId": "anchor-session", "event": {"type": "delta", "kind": "text",
                "block": 0, "text": "".join(f"ANCHOR_{i:03d}\n" for i in range(50))}}})
        while not Path("grow").exists():
            time.sleep(.01)
        send({"method": "zf/session/event", "params": {
            "sessionId": "anchor-session", "event": {"type": "delta-end", "committed": True}}})
        append(0, 70)
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})

    for line in sys.stdin:
        request = json.loads(line)
        if "id" not in request:
            continue
        method = request.get("method")
        if method == "session/prompt":
            threading.Thread(target=stream, args=(request,), daemon=True).start()
            continue
        result = {"protocolVersion": 1, "agentCapabilities": {}} if method == "initialize" else {}
        if method == "session/new":
            result = {"sessionId": "anchor-session"}
        send({"id": request["id"], "result": result})


def anchor_screen(console):
    """Capture stable visible anchor rows while the running header can animate."""
    until = time.monotonic() + 2
    previous, since = None, time.monotonic()
    while time.monotonic() < until:
        screen = console.read()
        markers = re.findall(r"ANCHOR_\d{3}(?!\d)", screen)
        now = time.monotonic()
        if markers and markers == previous:
            if now - since >= .075:
                return screen
        else:
            previous, since = markers, now
        time.sleep(.025)
    raise AssertionError("visible conversation anchor rows did not settle")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/native-agent-tui.exe"))
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this fixture requires Windows ConPTY")
    from conpty import Console
    repo = Path(__file__).resolve().parents[2]
    root = repo / "target" / ("conversation-anchor-" + uuid.uuid4().hex[:8])
    root.mkdir()
    launcher = root / "provider.cmd"
    launcher.write_text('@echo off\n' + subprocess.list2cmdline([
        sys.executable, str(Path(__file__).resolve()), "--provider"]) + '\n', encoding="utf-8")
    previous = os.environ.get("NATIVE_AGENT_TUI_LANG")
    os.environ["NATIVE_AGENT_TUI_LANG"] = "en"
    console = Console([str(args.binary.resolve()), "--cwd", str(root), "--journal-dir",
                       str(root / "journal"), "--dsh", str(launcher)], repo)
    try:
        console.wait(lambda screen: "Ready" in screen, "ready")
        console.write("anchor probe\r")
        console.wait(lambda screen: "ANCHOR_049" in screen, "initial output")
        console.write("\x1b[5~")
        console.wait(lambda screen: "ANCHOR_041" in screen and "ANCHOR_049" not in screen, "scroll up")
        before_screen = anchor_screen(console)
        before = re.findall(r"ANCHOR_\d+", before_screen)
        assert before
        (root / "before-screen.txt").write_text(before_screen, encoding="utf-8")
        (root / "grow").touch()
        # The ordered prompt result proves Core consumed the preceding text update.
        console.wait(lambda screen: "Completed" in screen, "growth and completion consumed by UI")
        # A header arrives before the remainder of the terminal diff. Read one
        # settled set of anchor rows. The running header can keep animating
        # while the conversation content is already stable.
        after_screen = anchor_screen(console)
        after = re.findall(r"ANCHOR_\d+", after_screen)
        (root / "after-screen.txt").write_text(after_screen, encoding="utf-8")
        # Completion can reclaim a status row; the same top content must stay fixed.
        count = min(len(before), len(after))
        assert count and after[:count] == before[:count], f"viewport drifted: {before} -> {after}; screens: {root}"
        console.write("\x1b[1;5F")
        console.wait(lambda screen: "ANCHOR_069" in screen, "return to tail")
        print(f"PASS: anchored preview-to-authority viewport and return to tail; screens: {root}")
        console.write("\x11")
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
    provider() if "--provider" in sys.argv else main()
