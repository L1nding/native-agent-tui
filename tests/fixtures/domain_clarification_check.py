"""Real platform dialogue and C07 source/provenance acceptance.

Only keyboard input advances the model workflow. Durable domain records are read
as independent evidence; this harness never prepares, authorizes or applies one.
"""
import hashlib
import json
import re
from pathlib import Path


SOURCE_ID = "source.mixed.instance.84.80.7E"
COMPONENT_ID = "component.mixed.84.80.7E"


def run_clarification_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_U, CTRL_Y, ENTER, Failed, expect

    binding = json.loads(context_path.read_text(encoding="utf-8"))
    repository = Path(binding["project_root"])
    scene = repository / binding["scene_path"]
    directory = repository / "outputs/domain" / binding["run_id"] / "changes"
    baseline = hashlib.sha256(scene.read_bytes()).hexdigest()
    source_hashes = {
        str(repository / source["path"]): hashlib.sha256(
            (repository / source["path"]).read_bytes()
        ).hexdigest()
        for source in binding["sources"]
    }
    stages = []
    question_mode = None

    def records():
        return sorted(
            [json.loads(path.read_text(encoding="utf-8")) for path in directory.glob("*.json")
             if re.fullmatch(r"[a-f0-9]{32}", path.stem)],
            key=lambda record: record["revision"],
        )

    def latest():
        rows = records()
        return rows[-1] if rows else None

    def current_prepared(screen, previous):
        proposal = latest()
        return bool(proposal and proposal["state"] == "prepared"
                    and proposal["revision"] > previous
                    and f"方案 {proposal['proposal_id']} · 修订 {proposal['revision']} · prepared" in screen)

    def checkpoint(name, screen):
        row = {
            "stage": name,
            "question_mode": question_mode if name in (
                "ambiguous-source", "missing-height", "conflicting-width"
            ) else None,
            "scene_sha256": hashlib.sha256(scene.read_bytes()).hexdigest(),
            "source_hashes": {
                path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                for path in source_hashes
            },
            "proposals": records(),
            "source_dispositions": json.loads((directory / "source-dispositions.json").read_text(
                encoding="utf-8"
            )) if (directory / "source-dispositions.json").exists() else {"requests": {}, "objects": {}},
            "authorization_ids": sorted(p.stem for p in directory.glob("authorization-*.json")),
            "operation_ids": sorted(p.stem for p in directory.glob("operation-*.json")),
        }
        stages.append(row)
        (evidence / "clarification-stages.json").write_text(
            json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        expect(row["source_hashes"] == source_hashes, "clarification modified authoritative CAD")
        if name != "applied":
            expect(row["scene_sha256"] == baseline, "unapproved clarification modified Scene IR")
            expect(not row["authorization_ids"] and not row["operation_ids"],
                   "clarification produced approval or application before the final UI decision")
        return row

    def is_question(screen):
        return "Needs your answer" in screen or (
            "User input" in screen and "awaiting your answer" in screen
        )

    def question(name):
        nonlocal question_mode
        def pending(screen):
            nonlocal question_mode
            if " · Failed · " in screen or " · Interrupted · " in screen:
                raise Failed("clarification turn failed or was interrupted")
            if is_question(screen):
                question_mode = "platform_request"
                return True
            # Some providers only expose ordinary conversation questions. The
            # domain page must show the current root reply before we answer it.
            if " · Completed · " not in screen or "Agent 回复" not in screen:
                return False
            reply = screen.split("Agent 回复", 1)[1].split("目标 / 自然语言反馈", 1)[0]
            if name == "ambiguous-source":
                subject = "椅" in reply and any(word in reply for word in ("哪", "选择", "选"))
            elif name == "missing-height":
                subject = "高度" in reply
            else:
                subject = "宽" in reply or "冲突" in reply
            if subject and any(word in reply for word in ("？", "?", "请提供", "请选", "请选择", "请确认")):
                question_mode = "conversation"
                return True
            raise Failed("the completed domain reply did not ask the required clarification")
        screen = tui.wait("visible clarification question", pending, 240)
        return checkpoint(name, screen)

    def answer(text):
        if question_mode == "platform_request":
            expect(is_question(tui.screen()), "no live platform question accepts this answer")
        else:
            expect(question_mode == "conversation", "no observed clarification accepts an answer")
            finished()
        tui.keys(CTRL_U)
        tui.type(text)
        tui.keys(ENTER, 1.0)

    def ambiguous():
        send("请给那张椅子建模。目前有多张待新增椅子，‘那张’尚未明确指哪一个。"
             "我还没有提供任何高度，也没允许示意尺寸。先澄清对象并等待我的回答，"
             "一次只问一个问题。不要生成可执行方案或写入场景。")
        row = question("ambiguous-source")
        expect(all(p["state"] != "prepared" for p in row["proposals"]),
               "ambiguous source was guessed into an executable proposal")
    if not report.step("real dsh asks which source instance the user means", ambiguous, tui):
        return

    def missing_height():
        answer(f"我指 {SOURCE_ID}，在 (12,7,0)，宽深遵照图纸的0.5米。高度仍未确认，"
               "也未授权示意值。请先把缺高度登记为待确认方案，再等我提供高度，"
               "不要用默认值进入可执行方案。")
        row = question("missing-height")
        proposal = row["proposals"][-1] if row["proposals"] else None
        disposition = row["source_dispositions"]["objects"].get(SOURCE_ID)
        expect((proposal and proposal["state"] == "blocked" and proposal["pending"])
               or (not proposal and disposition and disposition["action"] == "pending_confirmation"),
               "missing height lacks durable pending evidence or became executable")
    if not report.step("missing height stays unresolved while a real clarification waits", missing_height, tui):
        return

    def allow_height():
        previous = latest()["revision"] if latest() else 0
        answer("允许整体高度0.9米作为示意高度，必须保持assumed，不能标为实测或source。"
               "宽深沿用图纸0.5米，保持来源位置、旋转与镜像。重新准备当前候选，先不要应用。")
        screen = fact("clarified height proposal", lambda s: current_prepared(s, previous), 300)
        finished()
        checkpoint("height-allowed", screen)
    if not report.step("a real height answer prepares a source-backed assumed candidate", allow_height, tui):
        return

    def conflicting_width():
        send("我又确认椅子宽度为0.7米，但图纸显示0.5米，两项信息冲突，我尚未决定修正哪一项。"
             "请保留CAD与保护对象不变，先把当前候选设为待确认并记录这项冲突，再询问并"
             "等待我澄清。不要直接采用任一尺寸，不要应用旧候选。")
        row = question("conflicting-width")
        proposal = row["proposals"][-1]
        expect(proposal["state"] == "blocked" and proposal["pending"],
               "conflicting confirmed/source width remained executable")
        expect(all(p["state"] != "prepared" for p in row["proposals"]),
               "a previous executable version survived the unresolved width conflict")
    if not report.step("confirmed/source dimension conflict invalidates the old candidate", conflicting_width, tui):
        return

    def resolve_width():
        previous = latest()["revision"]
        answer("复核后撤回我刚才的0.7米说法，按CAD宽深0.5米建模，不修改图纸。"
               "整体高度0.9米仍是我允许的示意值，保留assumed。保持实例身份与来源变换，"
               "清除已解决的未决项，重新准备完整候选，等待我批准。")
        screen = fact("resolved source conflict", lambda s: current_prepared(s, previous), 300)
        finished()
        row = checkpoint("width-resolved", screen)
        proposal = row["proposals"][-1]
        expect(not proposal["pending"], "resolved proposal still has pending input")
        components = proposal["component_plan"]["components"]
        expect(len(components) == 1 and components[0]["component_id"] == COMPONENT_ID
               and components[0]["source_instance_id"] == SOURCE_ID,
               "clarification changed source identity or added unrelated instances")
    if not report.step("explicit source clarification creates a new executable revision", resolve_width, tui):
        return

    def apply_and_build():
        tui.keys(CTRL_Y, 1.0)
        fact("clarified applied scene", lambda s: "场景应用 applied" in s, 240)
        screen = fact("clarified validated artifact",
                      lambda s: "产物验证 valid=true · current=true" in s, 300)
        finished()
        checkpoint("applied", screen)
    report.step("concrete UI approval applies and validates only the clarified version", apply_and_build, tui)
