"""C11 actual platform failure, prior valid artifact and subsequent external review."""

import hashlib
import json
import re
import subprocess
import time
from pathlib import Path


def business_job_section(screen, build_id):
    view = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
    flat = "".join(line.strip("│ ").strip() for line in view.splitlines())
    chunks = re.split(r"(?=Blender 产物 |产物验证 |构建任务 |产物位置 |AI/人工复核)", flat)
    identity = re.compile(r"build_id=" + re.escape(build_id) + r"(?=$|[^A-Za-z0-9_.-])")
    return "".join(chunk for chunk in chunks
                   if chunk.startswith(("Blender 产物 ", "产物验证 ")) and identity.search(chunk))


def run_failure_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, expect
    from windows_process_identity import record
    from blender_agent.integrations.domain_tools import DomainTools

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    baseline = hashlib.sha256(scene.read_bytes()).hexdigest()
    sources = {str(root / s["path"]): hashlib.sha256((root / s["path"]).read_bytes()).hexdigest()
               for s in context["sources"]}
    stages, preserved = [], {}
    proposal = operation = older = None

    def read(path):
        return json.loads(path.read_bytes())

    def records(pattern):
        return [read(path) for path in sorted(directory.glob(pattern))]

    def proposals():
        return [row for path in directory.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}", path.stem)
                for row in [read(path)]]

    def sequences():
        path = directory / "fact-sequences.json"
        return read(path) if path.exists() else {"facts": {}}

    def key(kind, ok=True, **fields):
        payload = {"contract":"modeling-domain/v2", "run_id":context["run_id"],
                   "context_sha256":hashlib.sha256(context_path.read_bytes()).hexdigest(),
                   "kind":kind, "ok":ok, **fields}
        return hashlib.sha256(json.dumps(payload, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

    def validation(build_id):
        # Only read production records; never emit a model-facing fact from the fixture.
        return DomainTools(context_path).artifacts.validate(build_id)

    def validation_key(build_id):
        result = validation(build_id)
        return key("artifact_validation", ok=result["valid"], artifact=result)

    def business(screen):
        view = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
        return "".join(line.strip("│ ").strip() for line in view.splitlines())

    def job_section(screen, build_id):
        return business_job_section(screen, build_id)

    def unchanged():
        expect(all(hashlib.sha256(Path(path).read_bytes()).hexdigest() == expected for path, expected in preserved.items()),
               "failed build or recovery changed a previous valid artifact")
        expect(all(hashlib.sha256(Path(path).read_bytes()).hexdigest() == expected for path, expected in sources.items()),
               "failure acceptance changed a source")

    def checkpoint(name, screen, **extra):
        unchanged()
        row = {"stage":name, "time_ns":time.time_ns(), "scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),
               "source_hashes":sources, "proposals":proposals(), "authorizations":records("authorization-*.json"),
               "operations":records("operation-*.json"), "artifacts":records("artifacts/*/build.json"),
               "fact_sequences":sequences(), "preserved_hashes":preserved.copy(), **extra}
        stages.append(row)
        (evidence / ("failure-" + name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / ("failure-" + name + ".scene.json")).write_bytes(scene.read_bytes())
        (evidence / "failure-stages.json").write_text(json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8")
        return row

    def scan():
        tui.wait("modeling session ready", lambda s:"建模" in s and "Initializing" not in s, 120)
        tui.type("开始 floor.mixed。先查询能力并扫描来源实例台账，只扫描和展示，先不准备、不批准、不应用。")
        tui.keys(ENTER, 0.5)
        screen = fact("failure source ledger reviewed", lambda s:"来源实例对账" in s, 300)
        finished()
        checkpoint("scan", screen)

    if not report.step("real Agent scans the isolated mixed floor without creating effects", scan, tui):
        return

    def prepare():
        nonlocal proposal
        send("只为 source.mixed.instance.84.80.7E 准备一张来源椅子，保留来源位置、旋转和镜像、稳定身份；宽深0.5米、高0.9米允许assumed。只准备并展示第1版，不批准、不应用、不预览。随后用户批准后请使用 failure-valid-build 独立构建并验证。")
        screen = fact("failure concrete version reviewed", lambda s:"修订 1 · prepared" in business(s), 300)
        finished()
        rows = proposals()
        expect(len(rows) == 1 and rows[0]["state"] == "prepared", "failure case changed preparation lineage")
        proposal = rows[0]
        expect(proposal["proposal_id"] in business(screen), "exact proposal was not shown")
        row = checkpoint("reviewed", screen)
        expect(row["scene_sha256"] == baseline and not row["operations"] and not row["authorizations"] and not row["artifacts"],
               "preparation created effects before concrete approval")

    if not report.step("real Agent scans and prepares the exact sourced chair before human approval", prepare, tui):
        return

    def approve():
        nonlocal older, operation, preserved
        tui.keys(CTRL_Y, 1)
        def completed(screen):
            nonlocal older
            built = [a for a in records("artifacts/*/build.json") if not a["candidate"] and a["state"] == "built"]
            if not built:
                return False
            expect(len(built) == 1, "initial approval duplicated the applied build")
            older = built[0]
            return ("valid=true" in job_section(screen, older["build_id"])
                    and validation_key(older["build_id"]) in sequences()["facts"])
        screen = fact("failure prior applied artifact valid", completed, 300)
        finished()
        ops = records("operation-*.json")
        expect(len(ops) == len(records("authorization-*.json")) == 1, "approval/application duplicated")
        operation = ops[0]
        expect(operation["state"] == "applied", "actual application did not complete")
        output = Path(older["output"])
        preserved = {str(output.with_suffix(ext)):hashlib.sha256(output.with_suffix(ext).read_bytes()).hexdigest()
                     for ext in (".glb", ".blend", ".validation.json")}
        checkpoint("valid-applied", screen, older_validation=validation(older["build_id"]))

    if not report.step("concrete keyboard approval applies once and validates a real old build", approve, tui):
        return

    def fail_build():
        send("对同一已应用方案再独立 build_scene，build_id=platform-failed-build，使用原 candidate_sha256。先启动并查询一次状态；正在building就结束这轮，等待我继续。不要重新准备、批准或应用。")
        gate_path = directory / "artifacts/platform-failed-build/live-blender-failure.json"
        screen = fact("failure second real Blender building", lambda s:gate_path.exists() and "Blender 产物 building" in job_section(s, "platform-failed-build"), 240)
        finished()
        gate = read(gate_path)
        record(evidence, [gate["pid"]])
        checkpoint("failure-building", screen, actual_blender=gate)
        (context_path.parent / "gate-controls/platform-failed-build.release").write_text("C11 actual Blender failure release", "utf-8")
        send("继续只查询 platform-failed-build 的终态并 validate_artifact。失败必须如实显示，并分别查询和验证原有效构建 " + older["build_id"] + "；不应用、不回滚、不重启任务。")
        def failed(screen):
            path = directory / "artifacts/platform-failed-build/build.json"
            return (path.exists() and read(path)["state"] == "failed"
                    and "valid=false" in job_section(screen, "platform-failed-build")
                    and validation_key("platform-failed-build") in sequences()["facts"]
                    and validation_key(older["build_id"]) in sequences()["facts"])
        screen = fact("failure second build failed without inherited validation", failed, 240)
        finished()
        expect("valid=true" not in job_section(screen, "platform-failed-build"), "failed job inherited old successful validation")
        # Capture old evidence in its own viewport as well; a long page may show only one job.
        old_screen = fact("failure old build separately remains valid", lambda s:"valid=true" in job_section(s, older["build_id"]), 120)
        checkpoint("failed-preserved", screen, older_validation=validation(older["build_id"]),
                   failed_validation=validation("platform-failed-build"), old_screen=old_screen)
        expect(hashlib.sha256(scene.read_bytes()).hexdigest() == proposal["candidate_sha256"], "build failure rolled back the applied scene")
        expect(len(records("operation-*.json")) == len(records("authorization-*.json")) == 1, "failure duplicated application")

    if not report.step("actual Blender failure preserves the applied scene and separately bound valid artifact", fail_build, tui):
        return

    def later_edit():
        editor = root / "tests/fixtures/complex_external_editor.py"
        subprocess.run([str(root / ".venv/Scripts/python.exe"), str(editor), "--context", str(context_path),
                        "--expected-sha256", proposal["candidate_sha256"]], check=True, capture_output=True)
        receipt = read(context_path.parent / "post-failure-edit.json")
        edited = scene.read_bytes()
        screen = tui.screen()
        checkpoint("external-edit", screen, external_edit=receipt)
        send("现在有后续独立审阅修改。先只读 inspect_change 和 inspect_operation，分别 validate_artifact(" + older["build_id"] + ") 与 platform-failed-build，明确旧有效产物的valid和current不同含义。然后只沿原 authorization_id、operation_id " + operation["operation_id"] + " 调用一次 apply_change 查询原应用回执，不得新建批准、回滚、修订或重放场景写入；如实说明原操作已应用但当前场景已有后续变化。不要启动新的构建。")
        tools = DomainTools(context_path)
        def inspected(screen):
            current = tools.domain.inspect_operation(operation["operation_id"])
            application_key = key("application", receipt=operation, current_authority_sha256=receipt["edited_sha256"], current=False)
            return (key("operation", receipt=current) in sequences()["facts"]
                    and application_key in sequences()["facts"]
                    and validation_key(older["build_id"]) in sequences()["facts"]
                    and "valid=true · current=false" in job_section(screen, older["build_id"]))
        screen = fact("failure later edit survives original operation inspection", inspected, 240)
        finished()
        expect(scene.read_bytes() == edited, "Agent recovery overwrote the later edit")
        expect(records("operation-*.json") == [operation], "idempotent original operation changed its durable receipt")
        expect(len(records("authorization-*.json")) == 1 and len(records("artifacts/*/build.json")) == 2,
               "inspection issued another approval or build")
        checkpoint("reconciled-later-edit", screen, external_edit=receipt,
                   older_validation=validation(older["build_id"]), failed_validation=validation("platform-failed-build"))

    report.step("later external edit survives real Agent inspection and idempotent original application", later_edit, tui)
