"""Local Responses fixture. No API keys, external requests, or payload logging."""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

releases = [threading.Event(), threading.Event()]
lock = threading.Lock()
stats = {"root_requests": 0, "child_requests": 0, "root_requests_while_child_held": 0}
tools = {}


def catalog(entries, namespace=None):
    for entry in entries:
        if entry.get("type") == "namespace":
            catalog(entry.get("tools", []), entry["name"])
        elif entry.get("type") == "function":
            tools[entry["name"]] = (namespace, entry)


def function(name, arguments, call_id):
    candidates = [(key, spec) for key, spec in tools.items() if key == name or key.endswith("." + name)]
    if len(candidates) != 1:
        raise RuntimeError("fixture function is unavailable: " + name)
    actual_name, (namespace, _) = candidates[0]
    item = {"type": "function_call", "id": "fc-" + call_id, "call_id": call_id,
            "name": actual_name, "arguments": json.dumps(arguments), "status": "completed"}
    if namespace:
        item["namespace"] = namespace
    return item


def message(text):
    return {"id": "msg-" + text, "type": "message", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}]}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path == "/stats":
            with lock:
                data = json.dumps(stats).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        else:
            self.send_error(404)

    def do_POST(self):
        try:
            self.respond()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            # turn/interrupt deliberately closes a held child's provider stream.
            return

    def respond(self):
        if self.path in ("/release/0", "/release/1"):
            releases[int(self.path[-1])].set()
            self.send_response(200)
            self.end_headers()
            return
        if self.path not in ("/responses", "/v1/responses"):
            self.send_error(404)
            return
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        catalog(body.get("tools", []))
        with lock:
            # Spawn messages can arrive as developer input; inspect content, not call arguments.
            child = any("CHILD_TASK" in json.dumps(item.get("content", [])) for item in body.get("input", []))
            counter = "child_requests" if child else "root_requests"
            step = stats[counter]
            stats[counter] += 1
            if not child and ((step >= 2 and not releases[0].is_set()) or (step >= 4 and not releases[1].is_set())):
                stats["root_requests_while_child_held"] += 1
        if child:
            if step >= len(releases) or not releases[step].wait(30):
                self.send_error(504)
                return
            output = message("CHILD_DONE_" + str(step))
        elif step == 0:
            output = function("spawn_agent", {"task_name": "fixture_child", "message": "CHILD_TASK: Return CHILD_DONE.", "fork_turns": "none", "model":"gpt-6.1-sol"}, "spawn")
        elif step == 1 or step == 3:
            output = function("wait_for_subagent_completion", {"targets": []}, "wait-" + str(step))
        elif step == 2:
            output = function("followup_task", {"target": "/root/fixture_child", "message": "CHILD_TASK: Return CHILD_DONE_1."}, "followup")
        else:
            output = message("GATE_DONE")
        response_id = "resp-" + ("child" if child else "root") + "-" + str(step)
        response = {"id": response_id, "object": "response", "created_at": 1,
                    "model": body.get("model", "gpt-6.1-sol"), "status": "in_progress", "output": []}
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def event(kind, **fields):
            payload = {"type": kind, **fields}
            self.wfile.write(("event: " + kind + "\ndata: " + json.dumps(payload) + "\n\n").encode())
            self.wfile.flush()

        event("response.created", response=response)
        added = dict(output)
        if output["type"] == "function_call":
            added["arguments"] = ""
        event("response.output_item.added", output_index=0, item=added)
        if output["type"] == "function_call":
            event("response.function_call_arguments.delta", item_id=output["id"], output_index=0, delta=output["arguments"])
            event("response.function_call_arguments.done", item_id=output["id"], output_index=0, arguments=output["arguments"])
        else:
            event("response.output_text.delta", item_id=output["id"], output_index=0, content_index=0, delta=output["content"][0]["text"])
        event("response.output_item.done", output_index=0, item=output)
        response.update(status="completed", output=[output], usage={"input_tokens": 1, "output_tokens": 1, "total_tokens": 2,
                                                                  "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}})
        event("response.completed", response=response)


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
print(server.server_port, flush=True)
try:
    for _ in sys.stdin:
        pass
finally:
    for release in releases:
        release.set()
    server.shutdown()
    server.server_close()
