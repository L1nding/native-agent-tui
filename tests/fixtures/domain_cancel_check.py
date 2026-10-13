"""C11 keyboard-only Agent cancellation, durable discovery, reload and same-ID replay."""

import hashlib
import json
import re
import subprocess
import time
from pathlib import Path

from domain_failure_check import business_job_section


def run_cancel_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_R, CTRL_U, CTRL_Y, ENTER, ESC, F6, expect
    from windows_process_identity import creation_time, kernel_api, record
    from blender_agent.integrations.domain_tools import DomainTools

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    baseline = scene.read_bytes()
    sources = {str(root / s["path"]): hashlib.sha256((root / s["path"]).read_bytes()).hexdigest()
               for s in context["sources"]}
    stages, preserved, cancelled_hashes, handles = [], {}, {}, {}
    kernel = kernel_api()
    proposal = None
    ancestry = []

    def read(path):
        return json.loads(path.read_bytes())

    def records(pattern):
        return [read(path) for path in sorted(directory.glob(pattern))]

    def proposals():
        return [read(p) for p in directory.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}", p.stem)]

    def sequences():
        path = directory / "fact-sequences.json"
        return read(path) if path.exists() else {"facts": {}}

    def semantic_key(kind, ok=True, **fields):
        payload = {"contract":"modeling-domain/v2", "run_id":context["run_id"],
                   "context_sha256":hashlib.sha256(context_path.read_bytes()).hexdigest(),
                   "kind":kind, "ok":ok, **fields}
        return hashlib.sha256(json.dumps(payload, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

    def validation(build_id):
        # Read deterministic validation only. The fixture never calls _result or emits a fact.
        return DomainTools(context_path).artifacts.validate(build_id)

    def validation_key(build_id):
        value = validation(build_id)
        return semantic_key("artifact_validation", ok=value["valid"], artifact=value)

    def business(screen):
        view = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
        return "".join(line.strip("│ ").strip() for line in view.splitlines())

    def job(screen, build_id):
        return business_job_section(screen, build_id)

    def unchanged():
        expect(scene.read_bytes() == baseline, "preview cancellation changed Scene IR")
        expect(not records("authorization-*.json") and not records("operation-*.json"),
               "read-only preview scenario approved or applied a change")
        for path, expected in {**sources, **preserved, **cancelled_hashes}.items():
            expect(hashlib.sha256(Path(path).read_bytes()).hexdigest() == expected,
                   "cancellation/reload/replay changed a protected source or artifact")

    def checkpoint(name, screen, **extra):
        unchanged()
        row = {"stage":name, "time_ns":time.time_ns(), "scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),
               "source_hashes":sources, "proposals":proposals(), "authorizations":records("authorization-*.json"),
               "operations":records("operation-*.json"), "artifacts":records("artifacts/*/build.json"),
               "fact_sequences":sequences(), "preserved_hashes":preserved.copy(),
               "cancelled_hashes":cancelled_hashes.copy(), **extra}
        stages.append(row)
        (evidence / ("cancel-" + name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / ("cancel-" + name + ".scene.json")).write_bytes(scene.read_bytes())
        (evidence / "cancel-stages.json").write_text(json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8")
        return row

    def scan():
        tui.wait("modeling session ready", lambda s:"建模" in s and "Initializing" not in s, 120)
        tui.type("开始 floor.mixed。先查询能力并扫描来源实例台账，只扫描和展示，先不准备、不批准、不应用。")
        tui.keys(ENTER, 0.5)
        screen = fact("cancel source ledger reviewed", lambda s:"来源实例对账" in s, 300)
        finished()
        checkpoint("scan", screen)

    def prepare():
        nonlocal proposal, preserved
        send("只为 source.mixed.instance.84.80.7E 准备一张来源椅子，保留来源位置、旋转和镜像、稳定身份；宽深0.5米、高0.9米允许assumed。准备并展示第1版，然后用 build_id=cancel-valid-preview 做候选预览，查询终态并validate_artifact。不批准、不应用。")
        screen = fact("cancel old preview independently valid", lambda s:"valid=true" in job(s, "cancel-valid-preview")
                      and validation_key("cancel-valid-preview") in sequences()["facts"], 300)
        finished()
        rows = proposals()
        expect(len(rows) == 1 and rows[0]["state"] == "prepared", "preview changed preparation lineage")
        proposal = rows[0]
        older = read(directory / "artifacts/cancel-valid-preview/build.json")
        expect(older["candidate"] and older["state"] == "built", "old candidate was not actually built")
        output = Path(older["output"])
        preserved = {str(output.with_suffix(ext)):hashlib.sha256(output.with_suffix(ext).read_bytes()).hexdigest()
                     for ext in (".glb", ".blend", ".validation.json")}
        checkpoint("valid-preview", screen, older_validation=validation("cancel-valid-preview"))

    def start_preview():
        nonlocal ancestry
        send("对同一已准备方案启动第二次候选预览，build_id=cancel-preview，使用原base_sha256。启动后查询一次状态；正在building就结束这轮，等我继续。不重新准备、不批准、不应用。")
        path = directory / "artifacts/cancel-preview/live-blender-cancel.json"
        screen = fact("cancel second actual Blender building", lambda s:path.exists()
                      and "Blender 产物 building" in job(s, "cancel-preview"), 240)
        finished()
        gate = read(path)
        inventory_result = subprocess.run([
            "powershell", "-NoProfile", "-Command",
            "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Json -Compress"
        ], capture_output=True, check=True, text=True, encoding="utf-8")
        inventory = {r["ProcessId"]: r for r in json.loads(inventory_result.stdout)}
        pid, tui_pid = gate["pid"], int(tui.console.process.pid)
        ancestry = []
        for _ in range(20):
            if pid == tui_pid:
                break
            expect(pid in inventory, "actual Blender ancestry is incomplete")
            row = dict(inventory[pid])
            handle = kernel.OpenProcess(0x00101000, False, pid)
            expect(bool(handle), "cannot retain actual live process identity")
            handles[pid] = handle
            row["created"] = creation_time(kernel, handle)
            expect(kernel.WaitForSingleObject(handle, 0) == 258, "observed ancestor already exited")
            ancestry.append(row)
            pid = row["ParentProcessId"]
        expect(pid == tui_pid, "live Blender does not belong to this exact TUI")
        expect(Path(ancestry[0]["Name"]).name.lower() == "blender.exe", "gate was not real Blender")
        record(evidence, [r["ProcessId"] for r in ancestry])
        checkpoint("building", screen, actual_blender=gate, ancestry=ancestry, tui_pid=tui_pid)

    def cancel():
        nonlocal cancelled_hashes
        send("取消正在构建的预览，保留已完成的预览和方案，先停下。请查询并验证取消结果和旧预览，不批准、不应用、不启动新作业。")
        path = directory / "artifacts/cancel-preview/build.json"
        screen = fact("Agent confirms actual cancellation", lambda s:read(path)["state"] == "cancelled"
                      and "Blender 产物 cancelled" in job(s, "cancel-preview")
                      and "valid=false" in job(s, "cancel-preview")
                      and validation_key("cancel-preview") in sequences()["facts"], 240)
        finished()
        receipt = read(path)
        expect(receipt.get("owner_tree_confirmed") is True, "cancelled tree was not confirmed")
        owner = next((r for r in ancestry if r["ProcessId"] == receipt["owner_pid"]), None)
        expect(owner and owner["created"] == receipt["owner_created"], "receipt lost actual live owner identity")
        expect(kernel.WaitForSingleObject(handles[receipt["owner_pid"]], 0) == 0, "actual owner remains alive")
        expect(kernel.WaitForSingleObject(handles[ancestry[0]["ProcessId"]], 0) == 0, "actual Blender remains alive")
        cancelled_hashes = {str(p):hashlib.sha256(p.read_bytes()).hexdigest()
                            for p in path.parent.iterdir() if p.is_file()}
        old_screen = fact("cancel old preview remains independently valid", lambda s:"valid=true" in job(s, "cancel-valid-preview"), 120)
        checkpoint("cancelled", screen, cancel_receipt=receipt, old_screen=old_screen,
                   failed_validation=validation("cancel-preview"), older_validation=validation("cancel-valid-preview"),
                   cancelled_validation_key=validation_key("cancel-preview"), confirmed_exit_ns=time.time_ns())

    def reload():
        tui.keys(F6, 0.5)
        tui.keys("/model" + ENTER, 0.8)
        tui.see("Ctrl+R reload", 30)
        tui.keys(CTRL_R, 1)
        tui.keys(ESC, 0.5)
        tui.keys(F6, 0.5)
        tui.wait("cancel session reconfirms read-only after reload", lambda s:"建模" in s and " · Ready · " in s, 120)
        tui.keys(CTRL_Y, 0.5)
        screen = fact("cancel reload discards old approval binding", lambda s:"当前尚无已展示的可批准方案" in business(s), 60)
        checkpoint("reloaded-empty", screen)
        tui.keys(CTRL_U)
        tui.type("恢复 floor.mixed。只读取和展示上次方案与已有预览，并验证产物；不批准、不应用、不重新构建。")
        tui.keys(ENTER, 0.5)
        screen = fact("cancel reload discovers durable receipt", lambda s:"Blender 产物 cancelled" in job(s, "cancel-preview")
                      and "valid=false" in job(s, "cancel-preview"), 240)
        finished()
        old_screen = fact("cancel reload restores old valid preview", lambda s:"valid=true" in job(s, "cancel-valid-preview"), 120)
        checkpoint("reloaded-inspected", screen, old_screen=old_screen)

    def replay():
        send("用原 build_id=cancel-preview 再调用一次同一方案的preview_candidate，expected_sha256沿用原base_sha256；查询并验证实际回执。保留方案和旧预览，不批准、不应用、不另起任务。")
        screen = fact("cancel same ID replay stays terminal", lambda s:"Blender 产物 cancelled" in job(s, "cancel-preview")
                      and "valid=false" in job(s, "cancel-preview"), 240)
        finished()
        expect(len(records("artifacts/*/build.json")) == 2 and len(proposals()) == 1, "replay created another proposal/build")
        checkpoint("replayed", screen)

    try:
        for title, action in [
            ("real Agent scans without effects", scan),
            ("real Agent prepares and validates the old candidate preview", prepare),
            ("second actual Blender preview is running under the observed TUI owner", start_preview),
            ("natural-language cancellation confirms tree exit and preserves old exports", cancel),
            ("dsh reload discovers durable preview receipts without stale approval", reload),
            ("original cancelled build identity reads the same receipt without replay", replay),
        ]:
            if not report.step(title, action, tui):
                return
    finally:
        for handle in handles.values():
            kernel.CloseHandle(handle)
