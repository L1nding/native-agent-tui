"""Continue the actual prepared C04 after a provider-interrupted preview turn.

The original failed report remains immutable. Observers read provenance; only
keyboard input to the platform Agent can revalidate, approve, apply and build.
"""

import hashlib
import json
import os
from pathlib import Path

HISTORY_FILES = (
    "main-initial.txt", "main-cabinet-revision.txt", "main-partial-selection.txt",
    "main-stages.json", "main-lineage.json", "report.json", "failed-05-conversation.txt",
)


def interrupted_prefix(history, context_path, scene, directory):
    """Reject changed inputs, incomplete feedback or preexisting writer authority."""
    history = history.resolve()
    lineage = json.loads((history / "main-lineage.json").read_bytes())
    stages = json.loads((history / "main-stages.json").read_bytes())
    steps = json.loads((history / "report.json").read_bytes())
    if [s["status"] for s in steps] != ["PASS", "PASS", "PASS", "PASS", "FAIL", "PASS"]:
        raise ValueError("resume requires the preserved interrupted C04 prefix")
    if [s["stage"] for s in stages] != [
        "main-initial", "main-cabinet-revision", "main-partial-selection",
    ]:
        raise ValueError("resume requires all three original C04 feedback stages")
    if hashlib.sha256(context_path.read_bytes()).hexdigest() != lineage["context_sha256"]:
        raise ValueError("resume context changed")
    if hashlib.sha256(scene.read_bytes()).hexdigest() != lineage["baseline_sha256"]:
        raise ValueError("resume scene changed before approval")
    for filename, key in (("source.dxf", "source_sha256"), ("answer-key.json", "answer_sha256")):
        if hashlib.sha256((context_path.parent / filename).read_bytes()).hexdigest() != lineage[key]:
            raise ValueError("resume authored input changed")
    if list(directory.glob("authorization-*.json")) or list(directory.glob("operation-*.json")):
        raise ValueError("resume must start without writer authority or application")
    proposal = json.loads((directory / (stages[-1]["proposal_id"] + ".json")).read_bytes())
    if proposal["state"] != "prepared" or proposal["revision"] != 3 or proposal["pending"]:
        raise ValueError("resume requires the original prepared V3")
    preview_id = "main-preview-" + history.name[-8:]
    artifact = json.loads((directory / "artifacts" / preview_id / "build.json").read_bytes())
    if artifact["state"] != "built" or not artifact["candidate"] or artifact["proposal_id"] != proposal["proposal_id"]:
        raise ValueError("resume requires the existing built preview of this exact candidate")
    lineage["resumed_from"] = str(history)
    lineage["history_sha256"] = {
        name: hashlib.sha256((history / name).read_bytes()).hexdigest() for name in HISTORY_FILES
    }
    return lineage, proposal, preview_id


def run_partial_resume_check(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_HOME, CTRL_Y, ENTER, F6, expect
    from domain_hospital_mixed_check import business, artifact_validation_visible

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    history = Path(os.environ.get("MODELING_COMPLEX_RESUME_EVIDENCE", ""))
    state = {}

    def preflight():
        lineage, proposal, preview_id = interrupted_prefix(history, context_path, scene, directory)
        state.update(lineage=lineage, proposal=proposal, preview_id=preview_id)
        (evidence / "main-lineage.json").write_text(json.dumps(lineage, indent=2), encoding="utf-8")
        return "preserved interrupted prefix; unchanged authored source/base; no prior authority"

    if not report.step("C04 resume validates immutable interrupted-run provenance", preflight, tui):
        return

    def inspect():
        proposal = state["proposal"]
        tui.wait("C04 prepared resume ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        tui.type(
            "继续此前因provider错误中断的合成C04。只读describe_capabilities，inspect_change提案 "
            + proposal["proposal_id"]
            + "，核对仍为prepared的第3版，完整base_sha256、candidate_sha256与四个保留实例。"
              "此前桌、两椅和柜已采纳，柜高度已修订为1.6m；床、盆、坐便器与未决楼梯已经skipped。"
              "保持全部来源、定义、候选与处置。查询并验证已有候选预览 "
            + state["preview_id"]
            + "，最后一次领域调用必须validate_artifact该build_id。只读取已有结果，"
              "不创建方案或新build_id，不批准、不应用，不使用shell或其他模型。"
              "向用户展示可继续的具体版本和预览核验结果，等待具体批准。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        screen = fact("C04 existing candidate preview verified", lambda s:
                      artifact_validation_visible(s, state["preview_id"]), 900)
        finished()
        tui.keys(CTRL_HOME, 0.2)
        binding_screen = fact("C04 prepared binding visible", lambda s:
                              all(proposal[key] in business(s) for key in
                                  ("proposal_id", "base_sha256", "candidate_sha256"))
                              and "修订 3 · prepared" in business(s), 90)
        (evidence / "main-resume-binding.txt").write_text(binding_screen, encoding="utf-8")
        (evidence / "main-preview.txt").write_text(screen, encoding="utf-8")
        expect(hashlib.sha256(scene.read_bytes()).hexdigest() == state["lineage"]["baseline_sha256"],
               "resume observation wrote scene")
        expect(not list(directory.glob("authorization-*.json"))
               and not list(directory.glob("operation-*.json")), "resume observation gained authority")
        current = json.loads((directory / (proposal["proposal_id"] + ".json")).read_bytes())
        expect(current == proposal, "resume observation revised the prepared candidate")

    if not report.step("Platform re-reads exact C04 V3 and verifies its existing real preview", inspect, tui):
        return

    def apply():
        tui.keys(CTRL_Y, 1.0)
        fact("resumed C04 scene applied", lambda s: "场景应用 applied" in business(s), 900)
        def final_visible(screen):
            jobs = [json.loads(p.read_bytes()) for p in directory.glob("artifacts/*/build.json")]
            return any(job["state"] == "built" and not job["candidate"]
                       and job["proposal_id"] == state["proposal"]["proposal_id"]
                       and artifact_validation_visible(screen, job["build_id"]) for job in jobs)
        screen = fact("resumed C04 final artifact valid", final_visible, 900)
        finished()
        (evidence / "main-applied.txt").write_text(screen, encoding="utf-8")
        marker = json.loads((context_path.parent / "main-fixture.json").read_bytes())
        result = {"platform_turn":"completed", "final_proposal_id":state["proposal"]["proposal_id"],
                  "preview_id":state["preview_id"], "selected_labels":marker["selected_labels"],
                  "resumed_from":state["lineage"]["resumed_from"],
                  "ai_visual_review":"not_run", "human_visual_review":"not_run"}
        (evidence / "main-result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")

    report.step("Concrete UI approval applies and verifies four retained C04 instances", apply, tui)
