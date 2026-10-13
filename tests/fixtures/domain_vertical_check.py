"""Real C08 Agent: conflict, revision, stair subset, then incremental shaft addition.

Only terminal keyboard input advances the workflow. Persistence reads below audit
actual model calls; they never prepare, authorize, apply or synthesize a result fact.
"""
import hashlib
import json
import re
import time
from pathlib import Path



def business(screen):
    if "方案与执行事实" not in screen:
        return ""
    return "".join(line.strip("│ ").strip() for line in
        screen.split("方案与执行事实", 1)[1].split("Agent 回复", 1)[0].splitlines())


def source_ledger_visible(screen):
    view = business(screen)
    return "来源实例对账" in view and all(source in view for source in (
        "source.vertical.instance.34", "source.vertical.instance.3B"))

def run_vertical_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, F6, expect
    from blender_agent.integrations.domain_tools import DomainTools

    context=json.loads(context_path.read_bytes())
    root=Path(context["project_root"])
    scene=root/context["scene_path"]
    directory=root/"outputs/domain"/context["run_id"]/"changes"
    baseline=hashlib.sha256(scene.read_bytes()).hexdigest()
    source_hashes={str(root/s["path"]):hashlib.sha256((root/s["path"]).read_bytes()).hexdigest() for s in context["sources"]}
    stages=[]
    first=None
    corrected=None
    selected=None
    shaft=None
    old_hashes={}
    stair_scene=None
    question_mode=None

    def read(path):return json.loads(path.read_bytes())
    def records(pattern):return [read(p) for p in sorted(directory.glob(pattern))]
    def proposals():return [read(p) for p in sorted(directory.glob("*.json")) if re.fullmatch(r"[a-f0-9]{32}",p.stem)]
    def current(parent=None, ids=None):
        found=[p for p in proposals() if p["state"] in ("prepared","blocked") and (parent is None or p["parent_id"]==parent)
            and (ids is None or {c["component_id"] for c in p["component_plan"].get("connections",[])}==set(ids))]
        return found[0] if len(found)==1 else None
    def sequences():
        p=directory/"fact-sequences.json"
        return read(p) if p.exists() else {"facts":{}}
    def validated(build_id):
        actual=DomainTools(context_path).artifacts.validate(build_id)
        expect(actual["valid"],"actual vertical artifact did not validate")
        payload={"contract":"modeling-domain/v2","run_id":context["run_id"],"context_sha256":hashlib.sha256(context_path.read_bytes()).hexdigest(),
            "kind":"artifact_validation","ok":actual["valid"],"artifact":actual}
        key=hashlib.sha256(json.dumps(payload,sort_keys=True,ensure_ascii=False).encode()).hexdigest()
        expect(key in sequences()["facts"],"actual Agent validation fact missing")
        return actual,key
    def checkpoint(name,screen,**extra):
        row={"stage":name,"time_ns":time.time_ns(),"tui_pid":int(tui.console.process.pid),
            "scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),
            "source_hashes":{p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in source_hashes},
            "proposals":proposals(),"authorizations":records("authorization-*.json"),"operations":records("operation-*.json"),
            "tasks":records("tasks/*.json"),"artifacts":records("artifacts/*/build.json"),"fact_sequences":sequences(),**extra}
        expect(row["source_hashes"]==source_hashes,"Agent changed authoritative CAD")
        if len(stages)<7:
            expect(row["scene_sha256"]==baseline,"Agent wrote before concrete stair approval")
            expect(not row["authorizations"] and not row["operations"],"preapproval stage obtained authority")
        expect(all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in old_hashes.items()),"later addition changed the old stair artifact")
        stages.append(row)
        (evidence/"vertical-stages.json").write_text(json.dumps(stages,ensure_ascii=False,indent=2),encoding="utf-8")
        (evidence/(name+".txt")).write_text(screen,encoding="utf-8")
        (evidence/(name+".scene.json")).write_bytes(scene.read_bytes())
        return row
    def show_prepared(what,parent,ids):
        def ready(screen):
            p=current(parent,ids)
            return p is not None and p["state"]=="prepared" and f"方案 {p['proposal_id']} · 修订 {p['revision']} · prepared" in business(screen)
        screen=fact(what,ready,300)
        finished()
        return current(parent,ids),screen

    def scan():
        tui.wait("vertical modeling session ready",lambda s:"建模" in s and "Initializing" not in s,120)
        tui.type("开始 floor.lower 建模。先查询能力、扫描图纸楼梯和井道并展示来源与已有模型对账、楼层范围和关系缺口。先不要准备方案、预览、批准或写场景。用简短中文报告结论，不复述完整JSON。")
        tui.keys(ENTER,0.5)
        toggled=False
        def ledger(screen):
            nonlocal toggled
            if "Pending request" in screen and "Needs your approval" in screen:
                tui.keys(CTRL_Y,1.0);return False
            if "Write a task" in screen and not toggled:
                tui.keys(F6,1.0);toggled=True;return False
            return source_ledger_visible(screen)
        screen=fact("vertical source ledger",ledger,240)
        finished();checkpoint("scan",screen)
    if not report.step("Agent scans both vertical sources without modifying authority",scan,tui):return

    def conflict():
        nonlocal first,question_mode
        send("按图纸准备连接本层和相邻上层的楼梯、井道候选；从能力查询的只读邻层信息自行核对标高和已有板洞。我允许闭合示意台阶，先明确使用20步、步高0.16米，标记assumed，不自行调整。保留全部保护对象。请先实际调用领域准备记录这组输入；若标高冲突，展示阻塞并询问我调整意见，不申请批准，不预览。")
        def blocked(screen):
            p=current(ids=["component.vertical.34","component.vertical.3B"])
            return p is not None and p["state"]=="blocked" and any("vertical_elevation_conflict" in i["message"] for i in p["pending"]) and "vertical_elevation_conflict" in business(screen)
        screen=fact("actual vertical elevation conflict",blocked,300)
        first=current(ids=["component.vertical.34","component.vertical.3B"])
        def asked(s):
            nonlocal question_mode
            if "Needs your answer" in s or "awaiting your answer" in s:
                question_mode="platform_request"
                return True
            if " · Completed · " in s:
                question_mode="conversation"
                return True
            return False
        question=tui.wait("vertical conflict feedback channel ready",asked,600)
        (evidence/"conflict-question.txt").write_text(question,encoding="utf-8")
        checkpoint("elevation-conflict",screen,question_mode=question_mode)
    if not report.step("Agent records the actual 20 by 0.16 elevation conflict",conflict,tui):return

    def revise():
        nonlocal corrected
        prompt="我把楼梯步高改为0.15米，仍20步并保留assumed。沿刚才被阻塞的原候选和原基线修订，仅改变指定楼梯；井道、来源身份、两层既有洞口和保护对象保持。不新开方案，不批准，不预览。"
        if question_mode=="platform_request":
            tui.keys("\x15")
            tui.type(prompt)
            tui.keys(ENTER,0.5)
        else:
            send(prompt)
        corrected,screen=show_prepared("same chain vertical correction",first["proposal_id"],["component.vertical.34","component.vertical.3B"])
        checkpoint("corrected",screen)
    if not report.step("User feedback repairs the same proposal chain preserving the shaft",revise,tui):return

    def select():
        nonlocal selected
        send("本轮只采纳图纸中的楼梯，井道暂时跳过。请用部分采纳工具生成完整依赖子方案，保留楼梯身份、20步×0.15米和上层既有洞口；沿当前方案继续，不批准、不应用、不预览。")
        selected,screen=show_prepared("dependency complete stair subset",corrected["proposal_id"],["component.vertical.34"])
        checkpoint("stair-selected",screen)
        screen=fact("stair parts reviewed",lambda s:"component.vertical.34.step.01" in business(s) and "底标高 0" in business(s),120)
        checkpoint("stair-parts-reviewed",screen)
        screen=fact("stair dependencies reviewed",lambda s:"服务楼层" in business(s) and "slab.upper" in business(s) and "未检查" in business(s),120)
        checkpoint("stair-dependencies-reviewed",screen)
    if not report.step("Partial acceptance displays actual stair parts and read only dependencies",select,tui):return

    def preview():
        send("仅对当前楼梯子方案做真实候选预览，build_id=vertical-stair-preview；查询终态并validate_artifact。不要批准或应用，不新建或修订方案。")
        screen=fact("validated vertical stair preview",lambda s:"产物验证 valid=true · current=true" in business(s) and "vertical-stair-preview" in business(s),300)
        finished();actual,key=validated("vertical-stair-preview")
        checkpoint("stair-preview",screen,validation_key=key,validation=actual)
    if not report.step("Agent previews and validates the selected stair before approval",preview,tui):return

    def approve_stair():
        nonlocal old_hashes,stair_scene
        screen=fact("stair exact version visible",lambda s:selected["proposal_id"] in business(s) and f"修订 {selected['revision']} · prepared" in business(s),120)
        tui.keys(CTRL_Y,1.0)
        fact("stair applied by concrete grant",lambda s:"场景应用 applied" in business(s),300)
        # Bind verification to an explicit build ID in a separate completed turn.
        finished()
        send("沿已应用楼梯方案和真实candidate_sha256执行独立build_scene，build_id=vertical-stair-built；查询终态并validate_artifact，最后重新扫描展示楼梯已建和井道跳过。不再应用。")
        screen=fact("validated applied stair build",lambda s:"产物验证 valid=true · current=true" in business(s) and "vertical-stair-built" in business(s),300)
        finished();actual,key=validated("vertical-stair-built")
        record=read(directory/"artifacts/vertical-stair-built/build.json")
        output=Path(record["output"])
        old_hashes={str(output.with_suffix(ext)):hashlib.sha256(output.with_suffix(ext).read_bytes()).hexdigest() for ext in (".glb",".blend",".validation.json")}
        stair_scene=scene.read_bytes()
        checkpoint("stair-applied",screen,validation_key=key,validation=actual,old_artifact_hashes=old_hashes)
    if not report.step("Concrete stair approval applies once and validates its real build",approve_stair,tui):return

    def prepare_shaft():
        nonlocal shaft
        send("现在继续图纸中刚才跳过的井道。先重新扫描当前已建楼梯与跳过井道，然后基于当前场景只准备该井道的新方案，使用相邻两层既有井道洞口。楼梯已经建成，保留所有台阶和身份；不修订已应用楼梯，不重新建楼梯，不批准、不预览。")
        shaft,screen=show_prepared("incremental shaft after built stair",None,["component.vertical.3B"])
        expect(scene.read_bytes()==stair_scene,"shaft preparation changed the built stair")
        checkpoint("shaft-prepared",screen)
        screen=fact("shaft geometry reviewed",lambda s:"component.vertical.3B.wall." in business(s) and "高 2.8" in business(s),120)
        checkpoint("shaft-parts-reviewed",screen)
        screen=fact("shaft dependencies reviewed",lambda s:"服务楼层" in business(s) and "slab.lower" in business(s) and "slab.upper" in business(s) and "未检查" in business(s),120)
        checkpoint("shaft-dependencies-reviewed",screen)
    if not report.step("Agent prepares only the previously skipped shaft on the new baseline",prepare_shaft,tui):return

    def preview_shaft():
        send("对当前井道新方案做真实候选预览，build_id=vertical-shaft-preview；查询终态并validate_artifact。核对台阶保留与井道贯通，不批准、不应用。")
        screen=fact("validated incremental shaft preview",lambda s:"产物验证 valid=true · current=true" in business(s) and "vertical-shaft-preview" in business(s),300)
        finished();actual,key=validated("vertical-shaft-preview")
        expect(scene.read_bytes()==stair_scene,"shaft preview changed authority")
        checkpoint("shaft-preview",screen,validation_key=key,validation=actual)
    if not report.step("Agent validates the cumulative stair and shaft candidate without writing",preview_shaft,tui):return

    def approve_shaft():
        fact("shaft exact version visible",lambda s:shaft["proposal_id"] in business(s) and "修订 1 · prepared" in business(s),120)
        tui.keys(CTRL_Y,1.0)
        fact("shaft applied by concrete grant",lambda s:"场景应用 applied" in business(s),300)
        finished()
        send("基于已应用井道方案执行独立build_scene，build_id=vertical-all-built；查询终态并validate_artifact。不要再次应用或增添对象。")
        screen=fact("validated complete vertical build",lambda s:"产物验证 valid=true · current=true" in business(s) and "vertical-all-built" in business(s),300)
        finished();actual,key=validated("vertical-all-built")
        checkpoint("shaft-applied",screen,validation_key=key,validation=actual)
    if not report.step("Concrete shaft approval adds only four walls and validates the cumulative model",approve_shaft,tui):return

    def reconcile():
        send("最后重新扫描来源台账，核对楼梯与井道都已建、没有额外实例。再次validate_artifact核对vertical-stair-built和vertical-all-built，区分旧产物valid=true/current=false与当前产物valid=true/current=true；不准备、不应用、不重建。")
        screen=fact("final two vertical built instances",lambda s:"component.vertical.34" in business(s) and "component.vertical.3B" in business(s) and "built 2" in business(s),300)
        finished()
        old,old_key=validated("vertical-stair-built")
        latest,latest_key=validated("vertical-all-built")
        expect(old["current"] is False and latest["current"] is True,"old and current vertical builds were conflated")
        checkpoint("final-reconciled",screen,old_validation_key=old_key,old_validation=old,validation_key=latest_key,validation=latest)
    report.step("Agent reconciles both stable instances and validates old versus current artifacts",reconcile,tui)
