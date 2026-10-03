"""Deterministic Responses provider for real Codex request round trips on localhost.

Never records request bodies or authentication data. Only fixed counters and
fixture-answer booleans are exposed by /stats.
"""
import argparse
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


parser = argparse.ArgumentParser()
parser.add_argument("--scenario", choices=("approvals", "cancel", "input"), required=True)
args = parser.parse_args()
lock = threading.Lock()
stats = {"requests": 0, "fixture_answer_received": False, "fixture_error": False, "accepted_command_output_count": 0}


def catalog(entries, namespace=None):
    result = {}
    for entry in entries:
        if entry.get("type") == "namespace":
            result.update(catalog(entry.get("tools", []), entry["name"]))
        elif entry.get("type") in ("function", "custom"):
            result[entry["name"]] = (namespace, entry)
    return result


def tool(tools, name, payload, call_id):
    candidates = [(key, spec) for key, spec in tools.items() if key == name or key.endswith("." + name)]
    if len(candidates) != 1:
        raise ValueError("required fixture tool missing")
    actual, (namespace, spec) = candidates[0]
    item = {"id": "fc-" + call_id, "call_id": call_id, "name": actual, "status": "completed"}
    if namespace:
        item["namespace"] = namespace
    if spec["type"] == "custom":
        item.update(type="custom_tool_call", input=payload)
    else:
        item.update(type="function_call", arguments=json.dumps(payload if isinstance(payload, dict) else {"input": payload}))
    return item


def output(body, step):
    tools = catalog(body.get("tools", []))
    for item in body.get("input", []):
        if item.get("type") == "function_call_output" and item.get("call_id") == "command-1":
            with lock:
                stats["accepted_command_output_count"] = json.dumps(item.get("output", ""), ensure_ascii=False).count("SHELL_CANARY")
    if args.scenario == "approvals" and step == 1:
        return tool(tools, "exec_command", {"cmd": "[Console]::Write('SHELL_CANARY')", "yield_time_ms": 1000}, "command-1")
    if args.scenario == "cancel" or args.scenario == "approvals" and step == 2:
        name = "command-declined.txt" if args.scenario == "cancel" else "command-accepted.txt"
        command = "[System.IO.File]::AppendAllText('" + name + "', 'SHELL_CANARY')"
        return tool(tools, "exec_command", {"cmd": command, "yield_time_ms": 1000}, "command-" + str(step))
    if args.scenario == "approvals" and step in (0, 3):
        accepted = step == 3
        name = "file-accepted.txt" if accepted else "file-declined.txt"
        patch = ("*** Begin Patch\n*** Update File: " + name + "\n@@\n BASE\n+FILE_CANARY 中文\n*** End Patch\n"
                 if accepted else "*** Begin Patch\n*** Add File: " + name + "\n+FILE_CANARY 中文\n*** End Patch\n")
        return tool(tools, "apply_patch", patch, "file-" + str(step))
    if args.scenario == "input" and step == 0:
        return tool(tools, "request_user_input", {"questions": [{"id": "choice", "header": "Fixture", "question": "Choose a fixture response 中文👋", "options": [
            {"label": "Proceed (Recommended)", "description": "Continue the fixture."},
            {"label": "Stop", "description": "Stop the fixture."}]}]}, "input-0")
    if args.scenario == "input":
        for item in body.get("input", []):
            if item.get("type") == "function_call_output" and item.get("call_id") == "input-0":
                try:
                    value = json.loads(item.get("output", ""))
                    with lock:
                        stats["fixture_answer_received"] = value.get("answers") == {"choice": {"answers": ["测试回答中文👋"]}}
                except (TypeError, ValueError):
                    pass
    return {"id": "msg-done", "type": "message", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": "REQUESTS_DONE", "annotations": []}]}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path != "/stats":
            self.send_error(404)
            return
        with lock:
            data = json.dumps(stats).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        if self.path not in ("/responses", "/v1/responses"):
            self.send_error(404)
            return
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with lock:
                step = stats["requests"]
                stats["requests"] += 1
            item = output(body, step)
        except (KeyError, TypeError, ValueError):
            with lock:
                stats["fixture_error"] = True
            self.send_error(422, "fixture schema unavailable")
            return
        response = {"id": "resp-" + str(step), "object": "response", "created_at": 1,
                    "model": body.get("model", "gpt-6.1-sol"), "status": "in_progress", "output": []}
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def event(kind, **fields):
            self.wfile.write(("event: " + kind + "\ndata: " + json.dumps({"type": kind, **fields}) + "\n\n").encode())
            self.wfile.flush()

        try:
            event("response.created", response=response)
            event("response.output_item.added", output_index=0, item=item)
            if item["type"] == "message":
                event("response.output_text.delta", item_id=item["id"], output_index=0, content_index=0, delta=item["content"][0]["text"])
            event("response.output_item.done", output_index=0, item=item)
            response.update(status="completed", output=[item], usage={"input_tokens": 1, "output_tokens": 1, "total_tokens": 2,
                            "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}})
            event("response.completed", response=response)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            return


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
print(server.server_port, flush=True)
try:
    for _ in sys.stdin:
        pass
finally:
    server.shutdown()
    server.server_close()
