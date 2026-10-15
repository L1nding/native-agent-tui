"""Verify the ordinary dsh entry with real project tools and private Core facts.

The ACP provider is deterministic; no model/account/network is involved. The modeler
supplies real CAD parsing and sealed facts. All scenes/sources are synthetic and unique.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import uuid

sys.dont_write_bytecode = True


def seed(project):
    import ezdxf

    project.mkdir(parents=True)
    source = project / "drawing.dxf"
    document = ezdxf.new()
    document.header["$INSUNITS"] = 4
    block = document.blocks.new("floors")
    labels = [block.add_text(title, dxfattribs={"insert": (index * 10000, 0)})
              for index, title in enumerate(["八层平面", "九层平面", "九层平面"])]
    root = document.modelspace().add_blockref("floors", (0, 0))
    document.saveas(source)
    (project / "project.yaml").write_text("sources:\n  - path: drawing.dxf\n", encoding="utf-8")
    config = {"source": str(source), "root_insert_handle": root.dxf.handle,
              "label_handle": labels[0].dxf.handle, "label_text": labels[0].dxf.text,
              "floor_level": 8, "crop_dxf": [0, 0, 8000, 6000], "origin_dxf": [0, 0],
              "wall_height": 3, "confirmed_story_height": 3.2,
              "preview_slab_boundary": [{"x": 0, "y": 0}, {"x": 8, "y": 0},
                                        {"x": 8, "y": 6}, {"x": 0, "y": 6}],
              "preview_slab_source_handles": [labels[0].dxf.handle]}
    (project / "floor-08-trial.json").write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    (project / "scene.json").write_bytes(b'{"scene":"must remain unchanged"}')


def provider(evidence):
    from blender_agent.integrations.domain_facts import FactStore
    from blender_agent.integrations.project_tools import ProjectTools

    tools = facts = None
    turn = 0
    lock = threading.Lock()

    def send(value):
        with lock:
            print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)

    def update(value):
        send({"method": "session/update", "params": {"sessionId": "project-session", "update": value}})

    def call(name, function, arguments):
        value = function(**arguments)
        reply = facts.model_reply(value)
        identifier = f"project-{turn}-{name}"
        update({"sessionUpdate": "tool_call", "toolCallId": identifier, "kind": "other",
                "title": f"mcp__modeling_domain__{name}", "rawInput": arguments})
        update({"sessionUpdate": "tool_call_update", "toolCallId": identifier, "status": "completed",
                "rawOutput": reply})
        (evidence / f"{identifier}.json").write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")
        return value

    for line in sys.stdin:
        request = json.loads(line)
        if "id" not in request:
            continue
        method = request.get("method")
        result = {}
        if method == "initialize":
            result = {"protocolVersion": 1, "agentCapabilities": {}}
        elif method == "session/new":
            server = next(item for item in request["params"]["mcpServers"] if item["name"] == "modeling_domain")
            assert "blender_agent.integrations.project_mcp" in server["args"]
            context = Path(server["args"][server["args"].index("--context") + 1])
            tools, facts = ProjectTools(context), None
            facts = FactStore(tools)
            (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
            result = {"sessionId": "project-session"}
        elif method == "zf/permission/set":
            result = {"presets": [{"value": "read-only", "name": "read-only"}], "current": "read-only"}
        elif method == "zf/permission/describe":
            result = {"presets": [{"value": "read-only", "name": "read-only"}], "current": "read-only"}
        elif method == "zf/skills/list":
            result = {"skills": []}
        elif method == "session/prompt":
            turn += 1
            text = " ".join(item.get("text", "") for item in request["params"]["prompt"])
            assert text == ("建模整栋" if turn == 1 else "采用 f09.b")
            project = call("inspect_project", tools.inspect, {})["project"]
            selected = call("select_project_target", tools.select, {
                "target": "整栋", "expected_catalog_id": project["catalog_id"],
                "choices": None if turn == 1 else ["f09.b"],
            })["selection"]
            assert selected["state"] == ("needs_selection" if turn == 1 else "selected")
            update({"sessionUpdate": "agent_message_chunk", "messageId": f"answer-{turn}",
                    "content": {"type": "text", "text": "请选择重复图纸" if turn == 1 else "已选择 f08 和 f09.b；场景尚未应用"}})
            result = {"stopReason": "end_turn"}
        send({"id": request["id"], "result": result})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/native-agent-tui.exe"))
    parser.add_argument("--modeler-repo", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this fixture requires Windows ConPTY")
    from conpty import Console
    repo = Path(__file__).resolve().parents[2]
    modeler = args.modeler_repo.resolve()
    python = modeler / ".venv/Scripts/python.exe"
    if not python.is_file():
        parser.error("modeler .venv Python is required")
    identifier = uuid.uuid4().hex[:8]
    evidence = repo / "target" / ("domain-project-" + identifier)
    evidence.mkdir()
    project = modeler / "outputs" / ("project-entry-fixture-" + identifier)
    subprocess.run([str(python), str(Path(__file__).resolve()), "--seed", str(project)], check=True, cwd=modeler)
    before = {path.name: path.read_bytes() for path in project.iterdir()}
    launcher = evidence / "provider.cmd"
    launcher.write_text("@echo off\n" + subprocess.list2cmdline([
        str(python), str(Path(__file__).resolve()), "--provider", str(evidence)]) + "\n", encoding="utf-8")
    previous = os.environ.get("NATIVE_AGENT_TUI_LANG")
    os.environ["NATIVE_AGENT_TUI_LANG"] = "en"
    console = Console([str(args.binary.resolve()), "--cwd", str(modeler), "--journal-dir", str(evidence / "journal"),
                       "--dsh", str(launcher), "--domain-project", str(project.relative_to(modeler))], repo)
    try:
        console.wait(lambda screen: "Ready" in screen, "ready")
        console.write("建模整栋\r")
        console.wait(lambda screen: "Completed" in screen, "first turn completed")
        if "建模 · " not in console.read():
            console.write("\x1b[17~")  # Domain sessions start with their registered view open.
        console.wait(lambda screen: "重复图纸待选择" in screen and "f09.a" in screen and "f09.b" in screen,
                     "candidate choice facts are privately verified and visible")
        pending = console.stable()
        assert "project_selection" in pending and "not_granted" in pending
        (evidence / "pending-screen.txt").write_text(pending, encoding="utf-8")
        console.write("采用 f09.b\r")
        console.wait(lambda screen: "目标已选择" in screen, "explicit duplicate choice")
        selected = console.stable()
        assert "project_selection" in selected and "not_granted" in selected and "not_run" in selected
        (evidence / "selected-screen.txt").write_text(selected, encoding="utf-8")
        assert {path.name: path.read_bytes() for path in project.iterdir()} == before, "project input or scene mutated"
        console.write("\x19")  # There is no displayed proposal to approve.
        unchanged = console.stable()
        assert "not_granted" in unchanged
        print(f"PASS: ordinary project target entry, duplicate choice, private facts, no scene write; screens: {evidence}")
        console.write("\x11")
    except BaseException:
        (evidence / "failure-screen.txt").write_text(console.read(), encoding="utf-8")
        raise
    finally:
        console.close()
        if previous is None:
            os.environ.pop("NATIVE_AGENT_TUI_LANG", None)
        else:
            os.environ["NATIVE_AGENT_TUI_LANG"] = previous


if __name__ == "__main__":
    if "--seed" in sys.argv:
        seed(Path(sys.argv[sys.argv.index("--seed") + 1]))
    elif "--provider" in sys.argv:
        provider(Path(sys.argv[sys.argv.index("--provider") + 1]))
    else:
        main()
