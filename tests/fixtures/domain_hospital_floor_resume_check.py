"""Resume an actual approved hospital batch through the platform Agent only.

The parent run and human grant are read-only evidence. No domain method is
called here; keyboard feedback carries the original concrete authorization.
"""

import hashlib
import json
import os
import re
import time
from pathlib import Path

from domain_hospital_mixed_check import business


def run_hospital_floor_resume(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, F6, expect

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    scene = root / context["scene_path"]
    parent = Path(os.environ["MODELING_FLOOR_PARENT_EVIDENCE"]).resolve()
    parent_stages_path = parent / "hospital-floor-stages.json"
    parent_stages = json.loads(parent_stages_path.read_bytes())
    ready = next(s for s in parent_stages if s["stage"] == "batch-1-prepared")
    original = next(p for p in ready["proposals"] if p["state"] == "prepared")
    proposal_id = original["proposal_id"]
    grants = [json.loads(p.read_bytes()) for p in directory.glob("authorization-*.json")]
    grants = [g for g in grants if g["proposal_id"] == proposal_id]
    expect(len(grants) == 1, "resume requires exactly one existing concrete human grant")
    grant = grants[0]
    binding = ("proposal_id", "revision", "prepare_token", "base_sha256", "candidate_sha256")
    expect(all(grant[k] == original[k] for k in binding), "existing grant differs from parent proposal")
    expect(grant["actor"] == "native-agent-tui:user", "original approval is not platform human")
    expect(not list(directory.glob("operation-*.json")), "resume baseline already has an application")
    expect(hashlib.sha256(scene.read_bytes()).hexdigest() == original["base_sha256"], "resume scene changed")
    stages = []

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    def checkpoint(name):
        screen = tui.screen()
        match = re.search(r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B · SHA256 ([a-f0-9]{64})", screen)
        expect(match is not None, "resume lacks private Core fact header")
        kind, seq, size, sha = match.groups()
        raw = (directory / "facts" / (sha + ".json")).read_bytes()
        expect(hashlib.sha256(raw).hexdigest() == sha and len(raw) == int(size), "resume Core bytes differ")
        (evidence / "core-facts").mkdir(exist_ok=True)
        (evidence / "core-facts" / (sha + ".json")).write_bytes(raw)
        proposals = [json.loads(p.read_bytes()) for p in directory.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}", p.stem)]
        row = {"stage": name, "time_ns": time.time_ns(),
               "core_fact": {"kind": kind, "sequence": int(seq), "bytes": int(size), "sha256": sha},
               "scene_sha256": hashlib.sha256(scene.read_bytes()).hexdigest(),
               "proposals": proposals, "authorizations": records("authorization-*.json"),
               "operations": records("operation-*.json"), "artifacts": records("artifacts/*/build.json")}
        stages.append(row)
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / (name + ".scene.json")).write_bytes(scene.read_bytes())
        (evidence / "hospital-floor-resume-stages.json").write_text(json.dumps(stages,ensure_ascii=False,indent=2), encoding="utf-8")

    lineage = {"context": str(context_path.resolve()), "context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
               "parent_evidence": str(parent), "parent_stages_sha256": hashlib.sha256(parent_stages_path.read_bytes()).hexdigest(),
               "original_proposal_id": proposal_id, "original_authorization_id": grant["authorization_id"],
               "original_base_sha256": original["base_sha256"], "scope": "continue original approved floor batch; not full-floor completion"}
    (evidence / "hospital-floor-resume-lineage.json").write_text(json.dumps(lineage,ensure_ascii=False,indent=2),encoding="utf-8")

    def inspect():
        tui.wait("resume modeling ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        tui.type(f"恢复医院八层现有运行。只查询能力、来源分组与提案 {proposal_id}，核对当前场景基线和已有有效预览。保持已有分组解释，本轮不重新识别，不新建提案，不申请授权，不应用、不构建。展示具体版本和剩余请求。")
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        fact("resume original approved version displayed",lambda s: proposal_id in business(s) and "prepared" in business(s),1800)
        finished()
        expect(hashlib.sha256(scene.read_bytes()).hexdigest() == original["base_sha256"], "inspection wrote scene")
        expect(not list(directory.glob("operation-*.json")), "inspection created application")
        checkpoint("original-approved-inspected")

    if not report.step("New platform owner inspects original hospital approval without writes", inspect, tui):
        return

    def apply_original():
        args = {k: original[k] for k in ("proposal_id","revision","prepare_token")}
        args.update(expected_sha256=original["base_sha256"], authorization_id=grant["authorization_id"], operation_id="hospital-floor-resume-batch-1")
        send("继续此前Core具体批准的原版本；不是新批准。原批准记录仍有效，先核对绑定，再使用以下apply_change参数执行一次原方案；不修订或重新授权。参数中的expected_sha256是base_sha256，候选哈希仅用于核对应用结果：" + json.dumps(args,ensure_ascii=False))
        fact("original approved hospital batch applied",lambda s: "场景应用 applied" in business(s),1800)
        finished()
        expect(hashlib.sha256(scene.read_bytes()).hexdigest() == original["candidate_sha256"], "original candidate not applied")
        expect(len(records("authorization-*.json")) == 1 and len(records("operation-*.json")) == 1, "original grant replayed or replaced")
        checkpoint("original-approved-applied")

    if not report.step("Agent applies original concrete hospital grant once", apply_original, tui):
        return

    def build_original():
        send("构建刚恢复应用的原批次，build_id=hospital-floor-resumed-built-1，查询终态并验证；然后重新scan_source_groups展示全层状态与剩余请求。已有解释和CAD依据保持。先不准备下一批。")
        fact("resumed hospital batch actual build validated",lambda s: "hospital-floor-resumed-built-1" in business(s) and "产物验证 valid=true · current=true" in business(s),1800)
        finished()
        checkpoint("original-approved-built")

    if not report.step("Agent builds validates and reconciles resumed hospital batch", build_original, tui):
        return

    next_proposal = None
    def prepare_next():
        nonlocal next_proposal
        send("继续整层剩余可执行组，以新场景和scan_id准备下一批。优先包含此前未建类别；只有缺profile的落地家具／洁具可提出明确的数据定义，必要宿主关系仍保留。使用此前允许的assumed示意高度与细节，保留CAD尺寸、位置、旋转、镜像和全部既有实体。个别实例受阻时保留问题，为其他实例准备无冲突批次；不重复已建，不预览、不批准。")
        def prepared(screen):
            nonlocal next_proposal
            matches = [p for p in records("*.json") if "proposal_id" in p and p.get("state") == "prepared"]
            if len(matches) != 1:
                return False
            next_proposal = matches[0]
            return next_proposal["proposal_id"] in business(screen)
        fact("next hospital batch prepared after resumed apply",prepared,1800)
        finished()
        expect(next_proposal is not None and len(next_proposal["component_plan"]["components"]) >= 10, "next batch reduced to sample")
        old_ids = {c["source_instance_id"] for c in original["component_plan"]["components"]}
        new_ids = {c["source_instance_id"] for c in next_proposal["component_plan"]["components"]}
        expect(not old_ids & new_ids, "next batch duplicates resumed instances")
        checkpoint("next-batch-prepared")

    if not report.step("Agent prepares next floor batch without repeating built sources",prepare_next,tui):
        return

    def preview_next():
        send("只预览当前下一批，build_id=hospital-floor-resumed-preview-2，查询终态并验证；先不批准、不应用。")
        fact("next hospital batch preview validated",lambda s: "hospital-floor-resumed-preview-2" in business(s) and "产物验证 valid=true · current=true" in business(s),1800)
        finished()
        checkpoint("next-batch-preview")

    if not report.step("Agent previews next hospital batch",preview_next,tui):
        return

    def approve_next():
        fact("next concrete hospital version displayed",lambda s: next_proposal["proposal_id"] in business(s) and "prepared" in business(s),120)
        finished()
        tui.keys(CTRL_Y,1)
        fact("next concrete hospital batch applied",lambda s: "场景应用 applied" in business(s),1800)
        finished()
        send("构建新批准批次，build_id=hospital-floor-resumed-built-2，查询终态并验证，然后重扫全层分组展示每组已建、未决和剩余请求。本次批次成功仍不代表整层完成。")
        fact("next hospital batch build validated",lambda s: "hospital-floor-resumed-built-2" in business(s) and "产物验证 valid=true · current=true" in business(s),1800)
        finished()
        checkpoint("next-batch-applied-built")

    report.step("Concrete new approval applies builds and reconciles next hospital batch",approve_next,tui)
