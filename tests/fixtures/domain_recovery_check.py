"""C11 real dsh EOF after a durable scene write, then keyboard-only recovery."""

import ctypes
import hashlib
import json
import re
import subprocess
import time
from pathlib import Path


def kill_owned_dsh(tui, mcp_pid, evidence):
    from dsh_tui_check import expect
    from windows_process_identity import creation_time, kernel_api, record

    process_list = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Json -Compress"],
        capture_output=True, check=True, text=True, encoding="utf-8",
    )
    inventory = {row["ProcessId"]: row for row in json.loads(process_list.stdout)}
    tui_pid = int(tui.console.process.pid)
    ancestry, pid = [], mcp_pid
    for _ in range(20):
        if pid == tui_pid:
            break
        expect(pid in inventory, "owned MCP process ancestry is missing")
        ancestry.append(inventory[pid])
        pid = inventory[pid]["ParentProcessId"]
    expect(pid == tui_pid, "MCP process does not belong to this exact test TUI")
    hosts = [row for row in ancestry if row["Name"].lower() in {"node.exe", "dsh.exe"}]
    expect(bool(hosts), "owned ancestry has no actual dsh host")
    host = hosts[-1]
    record(evidence, [host["ProcessId"], mcp_pid])
    kernel = kernel_api()
    kernel.TerminateProcess.argtypes = [ctypes.c_void_p, ctypes.c_uint]
    kernel.TerminateProcess.restype = ctypes.c_int
    handle = kernel.OpenProcess(0x00101001, False, host["ProcessId"])
    expect(bool(handle), "cannot hold the exact owned dsh process")
    try:
        identity = creation_time(kernel, handle)
        started = time.time_ns()
        expect(kernel.TerminateProcess(handle, 19), "owned dsh termination failed")
        expect(kernel.WaitForSingleObject(handle, 5000) == 0, "owned dsh did not exit")
        result = {"tui_pid": tui_pid, "mcp_pid": mcp_pid, "dsh_pid": host["ProcessId"],
                  "created": identity, "started_ns": started, "confirmed_exit_ns": time.time_ns(),
                  "ancestry": ancestry}
        (evidence / "dsh-disconnect.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
        return result
    finally:
        kernel.CloseHandle(handle)


def run_recovery_checks(report, tui, send, finished, fact, context_path, evidence, restart):
    from dsh_tui_check import CTRL_R, CTRL_U, CTRL_Y, ENTER, ESC, F6, expect
    from windows_process_identity import check_clean
    from blender_agent.integrations.domain_tools import DomainTools

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    isolated = context_path.parent
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    baseline = hashlib.sha256(scene.read_bytes()).hexdigest()
    sources = {str(root / s["path"]): hashlib.sha256((root / s["path"]).read_bytes()).hexdigest()
               for s in context["sources"]}
    stages = []
    preview_hashes = {}
    proposal = None
    operation_id = None

    def read(path):
        return json.loads(path.read_bytes())

    def records(pattern):
        return [read(p) for p in sorted(directory.glob(pattern))]

    def proposals():
        return sorted([read(p) for p in directory.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}", p.stem)],
                      key=lambda row: row["revision"])

    def events():
        path = isolated / "write-events.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def sequences():
        path = directory / "fact-sequences.json"
        return read(path) if path.exists() else {"facts": {}}

    def key(kind, **fields):
        payload = {"contract":"modeling-domain/v2", "run_id":context["run_id"],
                   "context_sha256":hashlib.sha256(context_path.read_bytes()).hexdigest(),
                   "kind":kind, "ok":True, **fields}
        return hashlib.sha256(json.dumps(payload, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

    def validation_key(build_id):
        result = DomainTools(context_path).artifacts.validate(build_id)
        expect(result["valid"], "actual artifact did not validate")
        return key("artifact_validation", artifact=result)

    def operation_key():
        # The domain inspection is read-only; do not invoke the model-facing _result.
        receipt = DomainTools(context_path).domain.inspect_operation(operation_id)
        return key("operation", receipt=receipt)

    def business(screen):
        view = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
        return "".join(line.strip("│ ").strip() for line in view.splitlines())

    def checkpoint(name, screen, **extra):
        row = {"stage":name, "time_ns":time.time_ns(), "tui_pid":int(tui.console.process.pid),
               "scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),
               "source_hashes":{p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in sources},
               "proposals":proposals(), "authorizations":records("authorization-*.json"),
               "operations":records("operation-*.json"), "tasks":records("tasks/*.json"),
               "artifacts":records("artifacts/*/build.json"), "write_events":events(),
               "fact_sequences":sequences(), **extra}
        expect(row["source_hashes"] == sources, "recovery run changed authoritative source bytes")
        if preview_hashes:
            expect(all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == h for p,h in preview_hashes.items()),
                   "disconnect/recovery changed the valid old preview")
        stages.append(row)
        (evidence / "recovery-stages.json").write_text(json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8")
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        return row

    def submit_ready(prompt):
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER, 0.5)

    def scan():
        tui.wait("recovery modeling ready", lambda s:"建模" in s and " · Ready · " in s, 120)
        submit_ready("开始 floor.mixed。先查询能力和扫描来源台账；先不准备、不批准、不应用。")
        screen = fact("recovery source ledger", lambda s:"来源实例对账" in s, 240)
        finished()
        expect(any(e["kind"] == "mcp-startup" for e in events()), "real MCP timing hook is not installed")
        checkpoint("scan", screen)

    if not report.step("real Agent discovers recovery floor and starts the actual MCP", scan, tui):
        return

    def prepare():
        nonlocal proposal, preview_hashes
        send("只为 source.mixed.instance.84.80.7E 准备一张来源椅子，保留来源变换与稳定身份；宽深0.5米、高0.9米允许assumed。完成preview_candidate，build_id=recovery-valid-preview，查询终态并validate_artifact。只预览，不批准、不应用。")
        screen = fact("actual valid recovery preview", lambda s:"产物验证 valid=true · current=true" in s
                      and "recovery-valid-preview" in business(s), 300)
        finished()
        rows = proposals()
        expect(len(rows) == 1 and rows[0]["state"] == "prepared", "recovery preparation changed lineage")
        proposal = rows[0]
        expect(validation_key("recovery-valid-preview") in sequences()["facts"], "Agent did not validate the preview")
        record = read(directory / "artifacts/recovery-valid-preview/build.json")
        output = Path(record["output"])
        preview_hashes = {str(output.with_suffix(ext)):hashlib.sha256(output.with_suffix(ext).read_bytes()).hexdigest()
                          for ext in (".glb", ".blend", ".validation.json")}
        binding_screen = fact("recovery concrete version reviewed", lambda s:proposal["proposal_id"] in business(s)
                              and "修订 1 · prepared" in business(s), 120)
        row = checkpoint("reviewed", binding_screen, preview_hashes=preview_hashes)
        expect(row["scene_sha256"] == baseline and not row["authorizations"] and not row["operations"],
               "preview obtained approval or wrote authority")

    if not report.step("Agent prepares and validates a real old preview before human approval", prepare, tui):
        return

    def disconnect():
        nonlocal operation_id
        tui.keys(CTRL_Y, 0.5)
        screen = tui.wait("actual scene replaced before receipt completion",
                          lambda s:(isolated / "paused-after-scene.json").exists(), 240)
        paused = read(isolated / "paused-after-scene.json")
        rows = records("operation-*.json")
        expect(len(rows) == 1 and rows[0]["state"] == "applying", "durable applying receipt is missing")
        operation_id = rows[0]["operation_id"]
        row = checkpoint("after-scene", screen)
        expect(len(row["authorizations"]) == 1 and row["scene_sha256"] == proposal["candidate_sha256"],
               "actual scene replacement did not match the approved candidate")
        kill_owned_dsh(tui, paused["pid"], evidence)
        screen = tui.wait("actual dsh EOF reaches Core", lambda s:" · Disconnected · " in s, 90)
        checkpoint("disconnected", screen)
        check_clean(evidence)
        (isolated / "write-control.json").write_text(json.dumps({"mode":"forbid"}), encoding="utf-8")

    if not report.step("actual dsh transport disconnects after the real atomic scene replacement", disconnect, tui):
        return

    def start_again():
        previous_pid = int(tui.console.process.pid)
        previous_exit = restart()
        tui.wait("new TUI is ready on the existing context", lambda s:"建模" in s and " · Ready · " in s, 120)
        tui.keys(CTRL_Y, 0.5)
        screen = fact("fresh owner has no unseen approval binding",
                      lambda s:"当前尚无已展示的可批准方案" in business(s), 60)
        row = checkpoint("restarted-empty", screen, previous_exit=previous_exit)
        expect(row["tui_pid"] != previous_pid and len(row["authorizations"]) == len(row["operations"]) == 1,
               "restart reused owner identity or issued another grant")

    if not report.step("a new platform owner starts read-only and cannot approve unseen stale facts", start_again, tui):
        return

    def inspect():
        submit_ready("恢复 floor.mixed。上次执行断连。请先读取当前提案和已有操作回执，在建模页展示真实对账结果；这回合只读，不批准、不应用、不构建。")
        screen = fact("recovery actual operation is visible", lambda s:operation_id in business(s)
                      and "applying" in business(s) and "candidate_present" in business(s), 240)
        finished()
        expected = operation_key()
        expect(expected in sequences()["facts"], "Agent did not inspect the actual operation receipt")
        row = checkpoint("inspected", screen, operation_validation_key=expected)
        expect(row["operations"][0]["state"] == "applying", "read-only recovery changed persistent outcome")
        tui.keys(CTRL_Y, 0.5)
        screen = fact("uncertain operation cannot obtain another approval",
                      lambda s:any(message in business(s) for message in ("当前方案或执行状态不允许批准", "当前尚无已展示的可批准方案")), 60)
        row = checkpoint("reapproval-rejected", screen)
        expect(len(row["authorizations"]) == 1, "uncertain operation obtained a second grant")

    if not report.step("Agent discovers the old operation and renders candidate_present without writing", inspect, tui):
        return

    def recover():
        send("继续恢复刚才已对账的原操作，沿用原批准与操作身份；恢复后查询回执，独立build_scene，build_id=recovery-applied-build，查询终态再validate_artifact。不要新建提案或申请第二份批准。")
        screen = fact("reconciled operation and real applied build", lambda s:"场景应用 applied" in business(s)
                      and "产物验证 valid=true · current=true" in business(s)
                      and "recovery-applied-build" in business(s), 300)
        finished()
        expected = validation_key("recovery-applied-build")
        expect(expected in sequences()["facts"], "Agent did not validate the recovered applied artifact")
        row = checkpoint("recovered", screen, applied_validation_key=expected)
        expect(len(row["authorizations"]) == len(row["operations"]) == 1
               and row["operations"][0].get("recovery") == "reconciled_without_replay", "recovery changed approval/operation identity")
        expect(sum(e["kind"] == "after-scene-write" for e in row["write_events"]) == 1
               and not any(e["kind"] == "forbidden-scene-write" for e in row["write_events"]), "recovery replayed the writer")

    if not report.step("Agent reconciles the original grant without writer replay and validates real Blender", recover, tui):
        return

    def reload():
        tui.keys(F6, 0.5)
        tui.keys("/model" + ENTER, 0.8)
        tui.see("Ctrl+R reload", 30)
        tui.keys(CTRL_R, 1)
        tui.keys(ESC, 0.5)
        tui.keys(F6, 0.5)
        tui.wait("domain session reconfirms read-only after dsh reload", lambda s:"建模" in s and " · Ready · " in s, 120)
        tui.keys(CTRL_Y, 0.5)
        screen = fact("reload discards old rendered approval binding",
                      lambda s:"当前尚无已展示的可批准方案" in business(s), 60)
        checkpoint("reloaded-empty", screen)
        submit_ready("恢复 floor.mixed；只重新读取提案、已有操作和recovery-applied-build并验证产物，展示真实当前事实。不应用、不构建、不批准。")
        screen = fact("reload restores actual current facts", lambda s:operation_id in business(s)
                      and "not_required" in business(s) and "产物验证 valid=true · current=true" in business(s), 240)
        finished()
        row = checkpoint("reloaded-inspected", screen)
        expect(len(row["authorizations"]) == len(row["operations"]) == 1 and len(row["artifacts"]) == 2,
               "reload repeated approval/application/build")

    report.step("dsh reload switches lifecycle and restores only freshly inspected business facts", reload, tui)
