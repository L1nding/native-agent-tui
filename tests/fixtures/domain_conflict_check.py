"""C11 interrupted applying plus a later placement edit: refuse recovery without replay."""

import hashlib
import json
import re
import subprocess
import time
from pathlib import Path

from domain_failure_check import business_job_section
from domain_recovery_check import kill_owned_dsh


def run_conflict_checks(report, tui, send, finished, fact, context_path, evidence, restart):
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
    edited = None
    protected_records = {}

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

    def key(kind, ok=True, **fields):
        payload = {"contract":"modeling-domain/v2", "run_id":context["run_id"],
                   "context_sha256":hashlib.sha256(context_path.read_bytes()).hexdigest(),
                   "kind":kind, "ok":ok, **fields}
        return hashlib.sha256(json.dumps(payload, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

    def validation_key(build_id):
        result = DomainTools(context_path).artifacts.validate(build_id)
        expect(result["valid"], "actual artifact did not validate")
        return key("artifact_validation", artifact=result)

    def operation_key():
        # The domain inspection is read-only; do not invoke the model-facing _result.
        receipt = DomainTools(context_path).domain.inspect_operation(operation_id)
        return key("operation", receipt=receipt)

    def rejected_key():
        from blender_agent.integrations.domain_tools import REPAIRS
        code = "uncertain application"
        diagnostic = {"code":code, "entity_id":None,
                      "message":"uncertain application: conflict; inspect required",
                      "evidence":{"scene_path":str(scene)},
                      "repair":REPAIRS.get(code, "Inspect the proposal and correct its typed plan.")}
        return key("diagnostic", ok=False, diagnostics=[diagnostic])

    def business(screen):
        view = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
        return "".join(line.strip("│ ").strip() for line in view.splitlines())

    def checkpoint(name, screen, **extra):
        if edited is not None:
            expect(scene.read_bytes() == edited, "platform recovery changed the later placement edit")
        expect(all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == h for p,h in protected_records.items()),
               "recovery changed the uncertain task, proposal, original grant or operation receipt")
        row = {"stage":name, "time_ns":time.time_ns(), "tui_pid":int(tui.console.process.pid),
               "scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),
               "source_hashes":{p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in sources},
               "proposals":proposals(), "authorizations":records("authorization-*.json"),
               "operations":records("operation-*.json"), "tasks":records("tasks/*.json"),
               "artifacts":records("artifacts/*/build.json"), "write_events":events(),
               "fact_sequences":sequences(), "protected_records":protected_records.copy(), **extra}
        expect(row["source_hashes"] == sources, "recovery run changed authoritative source bytes")
        if preview_hashes:
            expect(all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == h for p,h in preview_hashes.items()),
                   "disconnect/recovery changed the valid old preview")
        stages.append(row)
        (evidence / "conflict-stages.json").write_text(json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8")
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / (name + ".scene.json")).write_bytes(scene.read_bytes())
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
        nonlocal proposal
        send("只为 source.mixed.instance.84.80.7E 准备一张来源椅子，保留来源变换与稳定身份；宽深0.5米、高0.9米允许assumed。只准备并展示第1版，不批准、不应用、不预览。")
        screen = fact("conflict prepared version reviewed", lambda s:"修订 1 · prepared" in business(s), 300)
        finished()
        rows = proposals()
        expect(len(rows) == 1 and rows[0]["state"] == "prepared", "conflict preparation changed lineage")
        proposal = rows[0]
        row = checkpoint("prepared", screen)
        expect(row["scene_sha256"] == baseline and not row["authorizations"] and not row["operations"] and not row["artifacts"],
               "preparation approved/applied or launched a preview")

    if not report.step("Agent prepares the concrete version without approval or preview", prepare, tui):
        return

    def preview():
        nonlocal preview_hashes
        send("对同一已准备方案做候选预览，build_id=conflict-valid-preview，使用原base_sha256；查询终态并validate_artifact，展示真实验证结果。不新建方案、不批准、不应用。")
        screen = fact("actual valid conflict preview", lambda s:"产物验证 valid=true · current=true" in s
                      and "conflict-valid-preview" in business(s), 300)
        finished()
        expect(validation_key("conflict-valid-preview") in sequences()["facts"], "Agent did not validate the preview")
        record = read(directory / "artifacts/conflict-valid-preview/build.json")
        output = Path(record["output"])
        preview_hashes = {str(output.with_suffix(ext)):hashlib.sha256(output.with_suffix(ext).read_bytes()).hexdigest()
                          for ext in (".glb", ".blend", ".validation.json")}
        binding_screen = fact("conflict concrete version reviewed", lambda s:proposal["proposal_id"] in business(s)
                              and "修订 1 · prepared" in business(s), 120)
        row = checkpoint("reviewed", binding_screen, preview_hashes=preview_hashes)
        expect(row["scene_sha256"] == baseline and not row["authorizations"] and not row["operations"],
               "preview obtained approval or wrote authority")

    if not report.step("Agent validates the real old preview in a separate bounded turn", preview, tui):
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

    def external_edit():
        nonlocal edited, protected_records
        editor = root / "tests/fixtures/complex_conflict_editor.py"
        command = [str(root / ".venv/Scripts/python.exe"), str(editor), "--context", str(context_path),
                   "--expected-sha256", proposal["candidate_sha256"]]
        completed = subprocess.run(command, check=True, capture_output=True, text=True, encoding="utf-8")
        receipt = read(isolated / "post-interruption-edit.json")
        expect(json.loads(completed.stdout) == receipt, "independent editor stdout differs from its durable receipt")
        edited = scene.read_bytes()
        expect(hashlib.sha256(edited).hexdigest() not in {baseline, proposal["candidate_sha256"]},
               "later placement edit did not change authority")
        paths = [directory / (proposal["proposal_id"] + ".json"),
                 *directory.glob("authorization-*.json"), *directory.glob("operation-*.json"),
                 *directory.glob("tasks/*.json")]
        protected_records = {str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
        checkpoint("external-edit", tui.screen(), edit_receipt=receipt, editor_invocation={"command":command,
                   "returncode":completed.returncode, "stdout":json.loads(completed.stdout)})

    if not report.step("independent editor moves only the interrupted chair while preserving original receipts", external_edit, tui):
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
                      and "applying" in business(s) and "conflict" in business(s), 240)
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

    if not report.step("Agent discovers the uncertain operation and renders actual later-edit conflict without writing", inspect, tui):
        return

    def recover():
        send("请用原proposal、revision、prepare_token、base_sha256、authorization_id和operation_id调用一次apply_change尝试对账，展示领域工具实际结果。当前场景已有后续位置修改，若冲突必须保留它；不生成新授权、不另起操作、不回滚、不构建。随后inspect_operation，分别查询并验证旧预览，先停下。")
        screen = fact("actual domain conflict rejects original application", lambda s:"uncertain application" in business(s)
                      and "conflict" in business(s) and rejected_key() in sequences()["facts"], 300)
        finished()
        expect(operation_key() in sequences()["facts"], "Agent did not inspect the still-uncertain operation")
        old_screen = fact("old conflict preview keeps valid history", lambda s:"valid=true" in business_job_section(s, "conflict-valid-preview")
                          and "current=false" in business_job_section(s, "conflict-valid-preview"), 120)
        expect(validation_key("conflict-valid-preview") in sequences()["facts"], "Agent did not validate old preview after conflict")
        row = checkpoint("recovery-refused", screen, rejection_key=rejected_key(), old_screen=old_screen,
                         older_validation=DomainTools(context_path).artifacts.validate("conflict-valid-preview"))
        expect(len(row["authorizations"]) == len(row["operations"]) == len(row["artifacts"]) == 1,
               "conflict recovery duplicated approval/operation/build")
        expect(row["operations"][0]["state"] == "applying" and row["tasks"][0]["state"] == "checked",
               "conflict was falsely finalized")
        expect(sum(e["kind"] == "after-scene-write" for e in row["write_events"]) == 1
               and not any(e["kind"] == "forbidden-scene-write" for e in row["write_events"]), "conflict attempted writer replay")

    if not report.step("original granted apply is actually rejected and later placement survives", recover, tui):
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
        submit_ready("恢复 floor.mixed；只重新读取原提案、已有操作和旧预览并验证产物，展示实际冲突和当前事实。不应用、不构建、不批准。")
        screen = fact("reload restores actual current facts", lambda s:operation_id in business(s)
                      and "conflict" in business(s) and "applying" in business(s), 240)
        finished()
        row = checkpoint("reloaded-inspected", screen)
        expect(len(row["authorizations"]) == len(row["operations"]) == len(row["artifacts"]) == 1,
               "reload repeated approval/application/build")
        old_screen = fact("reload retains separately validated old preview", lambda s:"valid=true" in business_job_section(s, "conflict-valid-preview")
                          and "current=false" in business_job_section(s, "conflict-valid-preview"), 120)
        expect(validation_key("conflict-valid-preview") in sequences()["facts"], "reload did not validate old preview")
        checkpoint("reloaded-preserved", old_screen)

    report.step("dsh reload switches lifecycle and restores only freshly inspected business facts", reload, tui)
