"""Read-only platform reconciliation after an actually successful C04 application."""

import hashlib
import json
import os
from pathlib import Path

APPLICATION_FILES = ("main-preview.txt", "main-applied.txt", "main-result.json", "main-lineage.json", "report.json")


def run_partial_reconcile_check(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import ENTER, F6, expect
    from domain_hospital_mixed_check import business

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    application = Path(os.environ.get("MODELING_COMPLEX_APPLICATION_EVIDENCE", "")).resolve()
    state = {}

    def authorities():
        return {p.name:p.read_bytes() for pattern in ("authorization-*.json","operation-*.json")
                for p in directory.glob(pattern)}

    def preflight():
        steps = json.loads((application / "report.json").read_bytes())
        expect(len(steps) == 4 and all(s["status"] == "PASS" for s in steps),
               "reconciliation must follow the actual successful application continuation")
        result = json.loads((application / "main-result.json").read_bytes())
        proposal = json.loads((directory / (result["final_proposal_id"] + ".json")).read_bytes())
        expect(proposal["state"] in {"applied","built"}, "C04 candidate was never applied")
        expect(hashlib.sha256(scene.read_bytes()).hexdigest() == proposal["candidate_sha256"],
               "C04 current scene changed since its actual application")
        before = authorities()
        expect(sum(n.startswith("authorization-") for n in before) == 1
               and sum(n.startswith("operation-") for n in before) == 1,
               "C04 authority count changed before reconciliation")
        lineage = json.loads((application / "main-lineage.json").read_bytes())
        lineage["application_from"] = str(application)
        lineage["application_sha256"] = {n:hashlib.sha256((application / n).read_bytes()).hexdigest()
                                          for n in APPLICATION_FILES}
        (evidence / "main-lineage.json").write_text(json.dumps(lineage,indent=2),encoding="utf-8")
        state.update(result=result, lineage=lineage, before=before, scene=scene.read_bytes())

    if not report.step("C04 reconciliation binds the successful application without new authority", preflight, tui):
        return

    def reconcile():
        tui.wait("applied C04 ledger ready", lambda s:"建模" in s and "Initializing" not in s,180)
        tui.type(
            "只读核对已应用的合成C04。调用describe_capabilities并分页scan_source_instances读完全部14个来源；"
            "最后一次领域调用必须scan_source_instances，完整台账应为built5（含原有1）、"
            "pending_addition1、pending_confirmation3、skipped4、suspected_duplicate1。"
            "已应用的是桌、两椅和高度1.6m的柜；床、盆、坐便器、未决楼梯skipped。"
            "核对当前场景、既有提案与来源链接，保留原类别／参数依据。"
            "不准备、不批准、不apply、不构建，不修改来源解释或处置，不使用shell或其他模型。"
            "直接展示对账结果与未决对象。"
        )
        tui.keys(ENTER,0.5)
        if "目标 / 自然语言反馈" not in tui.screen(): tui.keys(F6,0.5)
        values = ("built 5","pending_addition 1","pending_confirmation 3","skipped 4","suspected_duplicate 1")
        screen = fact("C04 applied full source reconciliation visible", lambda s:
                      "来源实例对账" in business(s) and all(v in business(s) for v in values),900)
        finished()
        expect(scene.read_bytes() == state["scene"],"reconciliation changed applied scene")
        expect(authorities() == state["before"],"reconciliation created or changed authority")
        (evidence / "main-final-ledger.txt").write_text(screen,encoding="utf-8")
        result = {**state["result"],"application_from":str(application)}
        (evidence / "main-result.json").write_text(json.dumps(result,indent=2),encoding="utf-8")

    report.step("Platform seals complete post-application C04 ledger without replaying writer", reconcile, tui)
