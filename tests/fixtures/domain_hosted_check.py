"""C08 advances the real platform Agent through keyboard input only."""
import hashlib
import json
import re
from pathlib import Path


def run_hosted_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_U, CTRL_Y, ENTER, F6, Failed, expect

    binding=json.loads(context_path.read_text(encoding="utf-8"))
    root=Path(binding["project_root"])
    scene=root/binding["scene_path"]
    directory=root/"outputs/domain"/binding["run_id"]/"changes"
    baseline=hashlib.sha256(scene.read_bytes()).hexdigest()
    hashes={str(root/source["path"]):hashlib.sha256((root/source["path"]).read_bytes()).hexdigest() for source in binding["sources"]}
    stages=[]
    question_mode=None

    def records():
        return sorted([json.loads(p.read_text(encoding="utf-8")) for p in directory.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}",p.stem)],key=lambda p:p["revision"])

    def checkpoint(name,screen):
        row={"stage":name,"question_mode":question_mode if name=="ambiguous-host" else None,
            "scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),"source_hashes":{p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in hashes},
            "proposals":records(),"authorization_ids":sorted(p.stem for p in directory.glob("authorization-*.json")),"operation_ids":sorted(p.stem for p in directory.glob("operation-*.json"))}
        stages.append(row)
        (evidence/"hosted-stages.json").write_text(json.dumps(stages,ensure_ascii=False,indent=2),encoding="utf-8")
        (evidence/(name+".txt")).write_text(screen,encoding="utf-8")
        expect(row["source_hashes"]==hashes,"authoritative hosted CAD changed")
        if name!="applied":
            expect(row["scene_sha256"]==baseline,"hosted scene changed before concrete approval")
            expect(not row["authorization_ids"] and not row["operation_ids"],"unapproved hosted phase obtained write authority")
        return row

    def scan():
        tui.wait("hosted modeling session ready",lambda s:"建模" in s and "Initializing" not in s,120)
        tui.type("开始 floor.hosted 建模。先扫描来源，展示图纸门窗与已有模型的对账、范围和关系能力；先不要准备方案或写入场景。")
        tui.keys(ENTER,0.5)
        requested=False
        def ledger(screen):
            nonlocal requested
            if "Pending request" in screen and "Needs your approval" in screen:
                tui.keys(CTRL_Y,1.0)
                return False
            if "Write a task" in screen and not requested:
                tui.keys(F6,1.0)
                requested=True
                return False
            return "来源实例对账" in screen and "pending_confirmation" in screen
        screen=fact("hosted source ledger",ledger,240)
        finished()
        checkpoint("scan",screen)
    if not report.step("real dsh scans CAD door and window relation inputs",scan,tui): return

    def ambiguous():
        nonlocal question_mode
        send("按图纸只准备门窗框和真实墙洞，暂不添加门扇、玻璃或其他填充资产。门高2.1米、门槛0；窗高1.2米、窗台1.0米，我允许这些竖向示意值，保留assumed。宽度和位置按图纸。宿主有歧义时先登记当前候选未决项，问我并等回答；不要自行猜宿主或申请写入。")
        def waiting(screen):
            nonlocal question_mode
            if " · Failed · " in screen or " · Interrupted · " in screen: raise Failed("host clarification turn failed")
            rows=records()
            if not rows or rows[-1]["state"]!="blocked" or not any("host_wall_ambiguous" in p["message"] for p in rows[-1]["pending"]): return False
            if "Needs your answer" in screen or "awaiting your answer" in screen:
                question_mode="platform_request"
                return True
            if " · Completed · " in screen and "Agent 回复" in screen:
                reply=screen.split("Agent 回复",1)[1].split("目标 / 自然语言反馈",1)[0]
                if ("宿主" in reply or ("wall.host" in reply and "wall.ambiguous" in reply)) and any(w in reply for w in ("？","?","请选","请选择","请确认","请分别指定","请明确","请回复")):
                    question_mode="conversation"
                    return True
                raise Failed("concrete host question was hidden or absent")
            return False
        screen=tui.wait("visible host ambiguity question",waiting,300)
        checkpoint("ambiguous-host",screen)
    if not report.step("real dsh blocks ambiguous host and asks the user",ambiguous,tui): return

    def resolve():
        previous=records()[-1]
        if question_mode=="conversation": finished()
        tui.keys(CTRL_U)
        tui.type("门的宿主我明确选择 wall.host；窗也使用 wall.host。保护 wall.ambiguous 和既有洞口，保持来源位置及宽度、示意高度和窗台依据。沿当前候选修订，先不要应用。")
        tui.keys(ENTER,1.0)
        def prepared(screen):
            rows=records()
            if not rows: return False
            current=rows[-1]
            return (current["state"]=="prepared" and current["revision"]>previous["revision"]
                and current["parent_id"]==previous["proposal_id"]
                and f"方案 {current['proposal_id']} · 修订 {current['revision']} · prepared" in screen)
        screen=fact("source-bound hosted proposal",prepared,300)
        finished()
        checkpoint("prepared",screen)
        expected_ids=[o["component_id"] for o in records()[-1]["component_plan"]["openings"]]
        def reviewed(s):
            business=s.split("方案与执行事实",1)[-1].split("Agent 回复",1)[0]
            return (all(i in business for i in expected_ids)
                and business.count("孔洞尺寸(m)")>=len(expected_ids)
                and business.count("参数依据")>=len(expected_ids)
                and "source" in business and "assumed" in business)
        review=fact("hosted parameters reviewed",reviewed,120)
        (evidence/"hosted-review.txt").write_text(review,encoding="utf-8")
    if not report.step("real host answer prepares the same source-bound proposal chain",resolve,tui): return

    def approve():
        tui.keys(CTRL_Y,1.0)
        fact("hosted applied scene",lambda s:"场景应用 applied" in s,240)
        screen=fact("validated hosted artifact",lambda s:"产物验证 valid=true · current=true" in s,300)
        finished()
        checkpoint("applied",screen)
    report.step("concrete hosted approval builds and validates real door/window apertures",approve,tui)
