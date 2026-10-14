"""Keyboard-only six-category/two-chair main workflow on authored test sources.

Observers only read durable records; they never prepare, authorize or apply.
"""

import hashlib
import json
import re
from pathlib import Path


def run_main_check(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, expect

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    input_dir = context_path.parent
    answers = json.loads((input_dir / "answer-key.json").read_bytes())["objects"]
    labels = {row["label"]: row for row in answers}
    marker = json.loads((input_dir / "main-fixture.json").read_bytes())
    partial = marker["case"] == "C04-platform-partial"
    selected = {labels[label]["component_id"]: labels[label] for label in marker["selected_labels"]}
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    baseline = scene.read_bytes()
    lineage = {"context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
               "baseline_sha256": hashlib.sha256(baseline).hexdigest(),
               "source_sha256": hashlib.sha256((input_dir / "source.dxf").read_bytes()).hexdigest(),
               "answer_sha256": hashlib.sha256((input_dir / "answer-key.json").read_bytes()).hexdigest()}
    (evidence / "main-lineage.json").write_text(json.dumps(lineage, indent=2), encoding="utf-8")
    stages = []

    def proposals():
        return sorted([json.loads(p.read_bytes()) for p in directory.glob("*.json")
                       if re.fullmatch(r"[a-f0-9]{32}", p.stem)], key=lambda r: r["revision"])

    def ready(name, previous=0, status="prepared"):
        def visible(screen):
            rows = proposals()
            return bool(rows and rows[-1]["revision"] > previous
                        and rows[-1]["state"] == status
                        and f"方案 {rows[-1]['proposal_id']} · 修订 {rows[-1]['revision']} · {status}" in screen)
        screen = fact(name, visible, 900 if partial else 600)
        finished()
        proposal = proposals()[-1]
        expect(scene.read_bytes() == baseline, "unapproved feedback changed Scene IR")
        expect(not list(directory.glob("authorization-*.json"))
               and not list(directory.glob("operation-*.json")), "feedback acquired writer authority")
        expect(hashlib.sha256((input_dir / "source.dxf").read_bytes()).hexdigest()
               == lineage["source_sha256"], "main feedback changed CAD")
        stages.append({"stage": name, "proposal_id": proposal["proposal_id"],
                       "revision": proposal["revision"], "scene_sha256": lineage["baseline_sha256"]})
        (evidence / "main-stages.json").write_text(json.dumps(stages, indent=2), encoding="utf-8")
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        return proposal

    state = {}

    def prepare():
        requests = [{"source_id": a["source_id"], "category": a["category"],
                     "dimensions_m": a["dimensions_m"]} for a in selected.values()]
        pending_instruction = (
            "另在同一plan.pending保留楼梯待确认：target_id=" + labels[marker["pending_stair_label"]]["source_id"]
            + "，question固定为：Stair connected floors and landing evidence missing。"
              "楼梯没有必要的跨层输入，不生成楼梯网格或连接；它应使本版blocked，"
              "仍须在同一完整候选保留全部7个家具洁具实例和定义，不退回空方案。"
            if partial else ""
        )
        send("根据已扫描的独立合成楼层，仅准备以下7个来源实例的同一个方案："
             + json.dumps(requests, ensure_ascii=False)
             + "。包括床、桌、两张椅子、柜、盆、坐便器六类。平面尺寸、XY、旋转和镜像保留CAD依据；"
               "各高度由夹具作者允许作为assumed示意参数。按来源定义形状和新数据模板，不读取plan.json，"
               "不修改已建、疑似重复、匿名椅和门窗楼梯井道。仅prepare，不批准、不应用、不构建。"
             + pending_instruction)
        state["v1"] = ready("main-initial", status="blocked" if partial else "prepared")
        ids = {c["component_id"] for c in state["v1"]["component_plan"]["components"]}
        expect(ids == set(selected), "six-category candidate omitted or added source instances")

    if not report.step("Agent prepares seven source instances across six categories", prepare, tui):
        return

    def move():
        send("只把以下两张椅子各向北移动1米（Y增加1）："
             + ", ".join(labels[label]["component_id"] for label in ("chair.one", "chair.two"))
             + "。来源、尺寸、身份、旋转和镜像保持；床、桌、柜、盆、坐便器及共享定义完全保持。"
               "沿当前父提案用feedback修订完整候选；不要只准备两椅的新孤立方案。")
        state["v2"] = ready("main-two-chair-move", state["v1"]["revision"])

    if partial:
        def revise_cabinet():
            send("只修订柜子 " + labels["cabinet.new"]["component_id"]
                 + "：把整体示意高度由1.8m改为1.6m，仍为assumed，保持CAD宽深1.2×0.6m及位置／旋转／镜像。"
                   "可改该实例或其专用定义的既有合法高度参数；不要向未声明的参数塞h。"
                   "其他六个候选及共享定义完全保持，尤其plan.pending的楼梯target_id和question逐字保持。"
                   "沿当前父提案带feedback和柜component_id重新prepare，预期仍blocked；不要批准或应用。")
            state["v2"] = ready("main-cabinet-revision", state["v1"]["revision"], status="blocked")
        if not report.step("Only cabinet height is revised while the stair remains explicitly unresolved", revise_cabinet, tui):
            return
    else:
        if not report.step("Natural-language feedback moves exactly both chairs", move, tui):
            return

        def rotate():
            send("只将 " + labels["chair.one"]["component_id"]
                 + " 相对当前朝向再旋转45度。两椅位置保持，第二张椅子的朝向和镜像保持；"
                   "床、桌、柜、盆、坐便器及所有定义保持。沿当前父提案用feedback修订完整候选。")
            state["v3"] = ready("main-one-chair-rotation", state["v2"]["revision"])

        if not report.step("Second feedback rotates one chair and preserves the other six candidates", rotate, tui):
            return

    def select():
        cabinet = labels["cabinet.new"]["component_id"]
        if partial:
            keep = [labels[label]["component_id"] for label in ("table.new", "chair.one", "chair.two", "cabinet.new")]
            send("部分采纳：只保留桌、两椅和已修订的柜子：" + ", ".join(keep)
                 + "。跳过床、盆、坐便器以及未决楼梯；这些来源保持skipped，不记built。"
                   "沿当前父提案调用select_candidate_components，保持保留实例及定义完整。"
                   "移除楼梯pending时必须使用该选择工具的来源处置，不另行改分类或伪造关系。"
                   "只准备新版本，不批准、不应用；新版本应为prepared。")
            state["v4"] = ready("main-partial-selection", state["v2"]["revision"])
            expect({c["component_id"] for c in state["v4"]["component_plan"]["components"]} == set(keep),
                   "C04 subset omitted retained furniture")
            return
        send("部分采纳：跳过柜子 " + cabinet
             + "；保留床、桌、两椅、盆、坐便器六个来源实例及当前所有位置、朝向和参数依据。"
               "沿当前父提案调用select_candidate_components，准备依赖完整的新版本；"
               "柜子保留skipped及理由，不记built，所有保留定义保持。不要批准或应用。")
        state["v4"] = ready("main-partial-selection", state["v3"]["revision"])
        ids = {c["component_id"] for c in state["v4"]["component_plan"]["components"]}
        expect(ids == set(selected) - {cabinet}, "partial selection changed retained candidate identity")

    if not report.step("Partial acceptance prepares the dependency-complete furniture subset" if partial else "Partial acceptance keeps six instances and prunes only the skipped cabinet", select, tui):
        return

    def preview():
        proposal = state["v4"]
        build_id = "main-preview-" + evidence.name[-8:]
        send("只对当前具体提案 " + proposal["proposal_id"]
             + " 渲染候选预览，使用build_scene candidate=true，build_id=" + build_id
             + "，expected_sha256使用提案base_sha256；用inspect_artifact等待终态，再validate_artifact。"
               "仅领域工具，不运行shell，不应用场景，不生成新提案。")
        screen = fact("main preview valid", lambda s: "产物验证 valid=true · current=true" in s, 600)
        finished()
        expect(scene.read_bytes() == baseline, "candidate preview wrote scene")
        expect(not list(directory.glob("authorization-*.json")), "preview silently approved proposal")
        state["preview_id"] = build_id
        (evidence / "main-preview.txt").write_text(screen, encoding="utf-8")

    if not report.step("Current selected candidate has a real validated Blender preview", preview, tui):
        return

    def apply():
        tui.keys(CTRL_Y, 1.0)
        fact("main scene applied", lambda s: "场景应用 applied" in s, 600)
        screen = fact("main applied artifact valid", lambda s: "产物验证 valid=true · current=true" in s, 600)
        finished()
        (evidence / "main-applied.txt").write_text(screen, encoding="utf-8")
        result = {"platform_turn": "completed", "final_proposal_id": state["v4"]["proposal_id"],
                  "preview_id": state["preview_id"], "selected_labels": marker["selected_labels"],
                  "ai_visual_review": "not_run", "human_visual_review": "not_run"}
        (evidence / "main-result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")

    report.step("Concrete UI approval applies and validates four retained furniture instances" if partial else
                "Concrete UI approval applies and validates all six retained source instances", apply, tui)
