"""Keyboard-only acceptance of the authorized proxy's real hospital decisions.

The observer never prepares, authorizes, applies or constructs business facts.
The real workflow Agent performs domain actions; Ctrl+Y approves displayed bindings.
"""

import hashlib
import json
import os
import re
from pathlib import Path

from domain_hospital_mixed_check import artifact_validation_visible, business


def applied_proxy_proposals(values, scene_sha256):
    return [v for v in values if isinstance(v.get("draft"),dict)
            and isinstance(v.get("component_plan"),dict)
            and v.get("candidate_sha256")==scene_sha256
            and v.get("state") in {"applied","built"}]


def run_hospital_proxy_check(report,tui,send,finished,fact,context_path,evidence):
    from dsh_tui_check import CTRL_Y,ENTER,ESC,F6,expect
    context=json.loads(context_path.read_bytes())
    root=Path(context["project_root"])
    scene=root/context["scene_path"]
    directory=root/"outputs/domain"/context["run_id"]/"changes"
    inputs=json.loads((context_path.parent/"proxy-test-inputs.json").read_bytes())
    package=json.loads(Path(inputs["decision_path"]).read_bytes())
    phase=os.environ.get("MODELING_PROXY_PHASE","batch")
    baseline=scene.read_bytes()
    base_sha=hashlib.sha256(baseline).hexdigest()
    source_hashes={s["path"]:hashlib.sha256((root/s["path"]).read_bytes()).hexdigest() for s in context["sources"]}
    lineage={"contract":"proxy-workflow-evidence/1","phase":phase,
        "decision_actor":"coding_agent_as_user_authorized_test_proxy",
        "real_project_human_confirmation":False,"decision_binding":inputs,
        "initial_scene_sha256":base_sha,"source_hashes":source_hashes,"stages":[]}

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    initial_grants=records("authorization-*.json")
    initial_operations=records("operation-*.json")
    lineage["initial_authorizations"]=initial_grants
    lineage["initial_operations"]=initial_operations

    def save():
        (evidence/"proxy-lineage.json").write_text(json.dumps(lineage,ensure_ascii=False,indent=2),encoding="utf-8")
    save()

    def groups():
        values=[json.loads(p.read_bytes()) for p in (directory/"facts").glob("*.json")]
        return max((g for g in values if g.get("kind")=="source_groups" and g["scene_sha256"]==hashlib.sha256(scene.read_bytes()).hexdigest()),
                   key=lambda g:g["fact_sequence"],default=None)

    def checkpoint(label):
        screen=tui.wait("verified Core proxy fact",lambda s:re.search(
            r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})",s),120)
        match=re.search(r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})",screen)
        kind,seq,size,digest=match.groups()
        raw=(directory/"facts"/(digest+".json")).read_bytes()
        expect(len(raw)==int(size) and hashlib.sha256(raw).hexdigest()==digest,"Core fact bytes differ")
        (evidence/"core-facts").mkdir(exist_ok=True)
        (evidence/"core-facts"/(digest+".json")).write_bytes(raw)
        (evidence/(label+".txt")).write_text(screen,encoding="utf-8")
        (evidence/(label+".scene.json")).write_bytes(scene.read_bytes())
        lineage["stages"].append({"stage":label,"scene_sha256":hashlib.sha256(scene.read_bytes()).hexdigest(),
            "core_fact":{"kind":kind,"sequence":int(seq),"bytes":int(size),"sha256":digest},
            "groups":groups(),"authorizations":records("authorization-*.json"),
            "operations":records("operation-*.json")})
        expect(Path(inputs["original_scene_path"]).read_bytes() and
            hashlib.sha256(Path(inputs["original_scene_path"]).read_bytes()).hexdigest()==inputs["original_scene_sha256"],
            "original hospital scene changed")
        expect(all(hashlib.sha256((root/path).read_bytes()).hexdigest()==digest for path,digest in source_hashes.items()),
               "proxy test changed CAD inputs")
        save()

    def inspect():
        tui.wait("proxy acceptance ready",lambda s:"建模" in s and "Initializing" not in s,180)
        tui.type("继续用户授权的隔离代理测试。先describe_capabilities、scan_source_instances、scan_source_groups，"
                 "再inspect_proxy_test_decisions，参数decision_path="+json.dumps(inputs["decision_path"])+
                 "，decision_sha256="+inputs["decision_sha256"]+"。当前仅核对和展示，不准备、不处置、不批准、不应用。"
                 "编码Agent为本轮测试代理；所有分类／高度保持assumed，医院真实人工确认仍false。"
                 "以scan_source_groups作为最后一次领域调用。")
        tui.keys(ENTER,.5)
        if "目标 / 自然语言反馈" not in tui.screen():tui.keys(F6,.5)
        fact("proxy current source groups visible",lambda s:groups() is not None and "全层来源分组" in business(s),1800)
        finished()
        expect(scene.read_bytes()==baseline and records("authorization-*.json")==initial_grants,"initial inspection changed scene/approval")
        checkpoint("initial")
    if not report.step("real workflow reads current source facts and authorized proxy package",inspect,tui):return

    if phase=="setup":
        decisions=sorted(package["remaining_candidate_decisions"],key=lambda d:d["source_id"])
        def ask():
            send("现在仅调用一次ask_user_question，29项问题按代理包remaining_candidate_decisions的source_id升序。"
                 "ID依次q01至q29，header依次Item01至Item29；question必须包含完整source_id及该项decision，"
                 "两个选项标签Execute和Defer，描述分别为按该项代理测试决定执行、保留待复核。"
                 "等待所有29项回答。回答返回后仅用一段分号分隔的回复逐项输出q01:Execute;q02:Execute;...;q29:Execute；最后将P,R,O,X,Y,B,A,T,C,H,A,C,K按顺序无分隔拼接，暂不调用领域写工具。")
            screen=tui.wait("actual 29 source questions",lambda s:"Enter answer" in s or " · Failed · " in s,900)
            expect("(1/29)" in screen and "Item01" in screen,"not one ordered 29-source request")
            (evidence/"proxy-questions-first.txt").write_text(screen,encoding="utf-8")
            tui.keys("\x1bOQ",.3)
            captured=[]
            for i in range(90):
                screen=tui.screen();captured.append(screen)
                compact=re.sub(r"[\s│]","","\n".join(captured))
                if all(d["source_id"] in compact for d in decisions) and all(f"id=q{i:02}" in compact for i in range(1,30)):break
                tui.keys("\x1b[6~"*8,.15)
            compact=re.sub(r"[\s│]","","\n".join(captured))
            expect(all(d["source_id"] in compact for d in decisions),"actual source IDs absent from question details")
            (evidence/"proxy-questions-details.txt").write_text("\n\n".join(captured),encoding="utf-8")
            tui.keys(ESC,.2)
            for i in range(1,30):
                expect(f"({i}/29)" in tui.screen(),"question answer ordering changed")
                (evidence/f"proxy-answer-{i:02}.txt").write_text(tui.screen(),encoding="utf-8")
                tui.type("1");tui.keys(ENTER,.15)
                if i<29:expect("Enter answer" in tui.screen(),"answers submitted early")
            finished()
            tui.keys("\x1b[5;2~"*8,.2)
            returned=[]
            for _ in range(35):
                screen=tui.screen();returned.append(screen)
                compact=re.sub(r"[\s│]","","\n".join(returned))
                if all(f"q{i:02}:Execute" in compact for i in range(1,30)) and "PROXYBATCHACK" in compact:break
                tui.keys("\x1b[6;2~",.15)
            expect(all(f"q{i:02}:Execute" in compact for i in range(1,30)) and "PROXYBATCHACK" in compact,"individual answers lost")
            (evidence/"proxy-answers-returned.txt").write_text("\n\n".join(returned),encoding="utf-8")
            lineage["question_decisions"]=[{"question_id":f"q{i:02}","source_id":d["source_id"],"decision":d["decision"],"answer":"Execute"} for i,d in enumerate(decisions,1)]
            save()
        if not report.step("one real request carries separate answers for 29 actual sources",ask,tui):return
        def record():
            send("根据已完整返回的29项Execute及用户授权的代理包，现在通过record_source_disposition把7个"
                 "skip_duplicate_addition_for_this_test来源分别记为skipped；保留来源身份与已建对象，reason含"
                 "agent_proxy_test_decision、decision_sha256和physical_duplicate_confirmation=false。"
                 "通过interpret_source_group记录16组接受现解释的测试意见；category保持当前值，reason含"
                 "agent_proxy_test_assumption和决策包哈希，不能改为user_confirmed。22项只记录处理意见，"
                 "本轮不准备、不批准、不应用。最后scan_source_groups。")
            fact("seven proxy skips and remaining requests visible",lambda s:groups() is not None and groups()["state_counts"].get("skipped")==7,1800)
            finished()
            interpretations=json.loads((directory/"source-interpretations.json").read_bytes())
            assessed=interpretations.get("group_assessments",{})
            expect(all(d["group_id"] in assessed and inputs["decision_sha256"] in assessed[d["group_id"]]["reason"] for d in package["group_decisions"]),"sixteen proxy group decisions not recorded")
            expect(scene.read_bytes()==baseline and records("authorization-*.json")==initial_grants and records("operation-*.json")==initial_operations,"setup wrote or approved geometry")
            checkpoint("proxy-decisions-recorded")
        report.step("workflow records 7 test skips and 16 assumed category choices",record,tui)
        return

    current=groups()
    rows={r["source_id"]:r for g in current["groups"] for r in g["instances"]}
    allowed={d["source_id"] for d in package["remaining_candidate_decisions"] if d["decision"]=="revise_candidate_layout_and_reprepare"}
    allowed.update(r["source_id"] for r in rows.values() if r["category"] in {d["category"] for d in package["installation_test_choices"]})
    source_ids=sorted(s for s in allowed if rows[s]["state"] not in {"built","skipped","rejected"})[:30]
    if phase=="batch":
        expect(source_ids,"no remaining sources; use finalize phase")
        proposal=None
        def prepare():
            nonlocal proposal
            parameters={"decision_path":inputs["decision_path"],"decision_sha256":inputs["decision_sha256"],
                        "source_ids":source_ids,"expected_sha256":base_sha,"expected_scan_id":current["scan_id"]}
            prior=[p for p in records("*.json") if p.get("state")=="blocked"
                   and p.get("base_sha256")==base_sha and p.get("component_plan")
                   and {c.get("source_instance_id") for c in p["component_plan"]["components"] if c.get("source_instance_id")}==set(source_ids)]
            if prior:
                parent=max(prior,key=lambda p:(p["revision"],p["proposal_id"]))
                parameters.update(parent_id=parent["proposal_id"],feedback={
                    "feedback_id":"proxy-reprepare-"+evidence.name[-8:],"action":"revise",
                    "text":"编码Agent代理复核：修复决策来源登记链；保留同批布局、CAD尺寸和未选宿主，重新准备具体版本。",
                    "object_ids":[]})
            send("本轮只用prepare_proxy_test_batch准备以下具体批次，参数"+json.dumps(parameters,ensure_ascii=False)+
                 "。该工具提供保留CAD的显式test_layout和typed installation，不能换为user_request或缩小。"
                 "只准备并inspect_change展示，不批准、不应用；若blocked读取诊断并停在该事实，不能绕过。")
            def prepared(screen):
                nonlocal proposal
                values=[v for v in records("*.json") if v.get("state")=="prepared" and v.get("base_sha256")==base_sha and v.get("proposal_id","") in business(screen)]
                if len(values)!=1:return False
                proposal=values[0];return True
            fact("concrete proxy batch prepared and shown",prepared,1800);finished()
            selected={c["source_instance_id"] for c in proposal["component_plan"]["components"] if c.get("source_instance_id")}
            expect(selected==set(source_ids),"prepared proxy source selection differs")
            expect(scene.read_bytes()==baseline and records("authorization-*.json")==initial_grants,"preparation wrote/approved scene")
            lineage["proposal"]=proposal;lineage["source_ids"]=source_ids;save();checkpoint("prepared")
        if not report.step("workflow prepares exact proxy source batch with typed provenance",prepare,tui):return
        preview_id="proxy-preview-"+evidence.name[-8:]
        build_id="proxy-built-"+evidence.name[-8:]
        def preview():
            send("只预览当前提案，preview_candidate参数"+json.dumps({"proposal_id":proposal["proposal_id"],"expected_sha256":base_sha,"build_id":preview_id})+
                 "。inspect_artifact(wait_seconds=20)等待并validate_artifact，不批准、不应用。")
            fact("actual proxy candidate preview validated",lambda s:artifact_validation_visible(s,preview_id),1800);finished();checkpoint("previewed")
            expect(scene.read_bytes()==baseline and records("authorization-*.json")==initial_grants,"preview wrote or approved scene")
        if not report.step("workflow previews and validates exact proxy candidate",preview,tui):return
        def apply():
            fact("validated proxy proposal visible",lambda s:proposal["proposal_id"] in business(s) and "prepared" in business(s),120)
            tui.keys(CTRL_Y,1)
            fact("proxy scene actually applied",lambda s:"场景应用 applied" in business(s),1800);finished()
            expect(hashlib.sha256(scene.read_bytes()).hexdigest()==proposal["candidate_sha256"],"applied proxy candidate hash differs")
            new_grants=[g for g in records("authorization-*.json") if g not in initial_grants]
            expect(len(new_grants)==1 and "coding_agent_as_user_authorized_test_proxy" in new_grants[0]["actor"],"proxy approval actor missing")
            checkpoint("applied")
        if not report.step("proxy keyboard approval applies only displayed concrete version",apply,tui):return
        def build():
            send("build_scene参数"+json.dumps({"proposal_id":proposal["proposal_id"],"expected_sha256":proposal["candidate_sha256"],"build_id":build_id})+
                 "。inspect_artifact(wait_seconds=20)等待，validate_artifact后scan_source_groups。不要开始下一提案。")
            fact("actual proxy build and GLB valid",lambda s:artifact_validation_visible(s,build_id),1800);finished()
            lineage["build_id"]=build_id;lineage["preview_id"]=preview_id;save();checkpoint("built")
            expect(all(r["state"]=="built" for g in groups()["groups"] for r in g["instances"] if r["source_id"] in source_ids),"applied sources not reconciled built")
        report.step("workflow builds actual Blender artifact and reconciles the batch",build,tui)
        return

    expect(phase=="finalize" and not source_ids,"finalize requires no supported pending source")
    expect(current["state_counts"]=={"built":480,"skipped":7},"final source outcomes differ")
    proposals=applied_proxy_proposals(records("*.json"),base_sha)
    expect(len(proposals)==1,"no unique current applied proposal")
    proposal=proposals[0]
    rebuild_id="proxy-rebuilt-"+evidence.name[-8:]
    def rebuild():
        send("完整代理测试当前应为480built、7skipped。用build_scene对最后应用提案独立重建，参数"+
             json.dumps({"proposal_id":proposal["proposal_id"],"expected_sha256":base_sha,"build_id":rebuild_id})+
             "。inspect_artifact(wait_seconds=20)等待，validate_artifact核对Blender、rebuilt和glb_roundtrip。"
             "然后render_artifact_review生成top、perspective及front三幅图，每幅read_artifact_review_image读取实际PNG。"
             "当前只生成可交付的新图；编码Agent随后自行作代理视觉判断。scene与来源不能改变，真实人工not_run。"
             "先scan_source_groups，再validate_artifact参数"+json.dumps({"build_id":rebuild_id})+
             "作为最后一次领域调用，保留可观察的当前产物验证；不要以图像回执替代构建验证。")
        fact("final full-floor rebuild GLB validated",lambda s:artifact_validation_visible(s,rebuild_id),1800);finished()
        lineage["proposal"]=proposal;lineage["rebuild_id"]=rebuild_id;save();checkpoint("rebuilt-and-rendered")
        images=[v for v in records("facts/*.json") if v.get("artifact",{}).get("build_id")==rebuild_id and v.get("artifact",{}).get("review_image",{}).get("delivery")=="attached_png"]
        expect({v["artifact"]["review_image"]["request"]["view"] for v in images} >= {"top","perspective","front"},"new artifact images not actually delivered")
        lineage["delivered_images"]=images;save()
        expect(scene.read_bytes()==baseline and records("authorization-*.json")==initial_grants and records("operation-*.json")==initial_operations,"final read-only build/review changed authority or scene")
    report.step("workflow rebuilds full scene validates GLB and delivers three real images",rebuild,tui)
