"""Keyboard-only real hospital multi-category and missing-host acceptance."""

import hashlib
import json
import re
import time
from pathlib import Path


def business(screen):
    if "方案与执行事实" not in screen:
        return ""
    return "".join(
        line.strip("│ ")
        for line in screen.split("方案与执行事实", 1)[1].split("Agent 回复", 1)[0].splitlines()
    )


def fact_sync_failure_is_terminal(screen):
    # Core still inhibits approval; the Agent may correct a failed tool call in this turn.
    return "领域事实同步失败" in screen and " · Running · " not in screen


def classified_state_visible(screen, objects, marker):
    """Read Core totals plus persisted source bindings across paginated ledger rows."""
    shown = business(screen)
    return (
        "来源实例对账" in shown
        and all(
            text in shown
            for text in ("pending_addition 2", "capability_missing 1", "pending_confirmation 484")
        )
        and set(objects) == {a["source_id"] for a in marker["answers"]}
        and all(
            objects[a["source_id"]]["category"] == a["category"]
            and objects[a["source_id"]]["basis"] == "assumed"
            and objects[a["source_id"]]["source_sha256"] == marker["source_sha256"]
            for a in marker["answers"]
        )
    )


def run_hospital_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, F6, expect

    context = json.loads(context_path.read_bytes())
    marker = json.loads((context_path.parent / "hospital-acceptance.json").read_bytes())
    root = Path(context["project_root"])
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    scene = root / context["scene_path"]
    supported = [a for a in marker["answers"] if a["expected_state"] == "built"]
    unsupported = next(a for a in marker["answers"] if a["expected_state"] == "capability_missing")
    target_ids = {a["component_id"] for a in supported}
    stages = []
    proposal = None

    def records(pattern):
        return [json.loads(path.read_bytes()) for path in sorted(directory.glob(pattern))]

    def proposals():
        return [
            json.loads(path.read_bytes())
            for path in sorted(directory.glob("*.json"))
            if re.fullmatch(r"[a-f0-9]{32}", path.stem)
        ]

    def candidate():
        matches = [
            p
            for p in proposals()
            if p["state"] == "prepared"
            and {c["component_id"] for c in p["component_plan"].get("components", [])} == target_ids
        ]
        return matches[0] if len(matches) == 1 else None

    def checkpoint(name, screen):
        pattern = r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})"
        screen = tui.wait(
            "hospital Core private fact synchronized", lambda s: bool(re.search(pattern, s)), 20
        )
        kind, sequence, size, digest = re.search(pattern, screen).groups()
        raw = (directory / "facts" / (digest + ".json")).read_bytes()
        expect(
            len(raw) == int(size) and hashlib.sha256(raw).hexdigest() == digest,
            "Core fact differs from sealed bytes",
        )
        archived = evidence / "core-facts"
        archived.mkdir(exist_ok=True)
        (archived / (digest + ".json")).write_bytes(raw)
        scene_raw = scene.read_bytes()
        row = {
            "stage": name,
            "time_ns": time.time_ns(),
            "tui_pid": int(tui.console.process.pid),
            "scene_sha256": hashlib.sha256(scene_raw).hexdigest(),
            "source_sha256": hashlib.sha256(
                (root / context["sources"][0]["path"]).read_bytes()
            ).hexdigest(),
            "proposals": proposals(),
            "authorizations": records("authorization-*.json"),
            "operations": records("operation-*.json"),
            "artifacts": records("artifacts/*/build.json"),
            "core_fact": {
                "kind": kind,
                "sequence": int(sequence),
                "bytes": int(size),
                "sha256": digest,
            },
            "fact_sequences": records("fact-sequences.json")[0],
        }
        expect(row["source_sha256"] == marker["source_sha256"], "CAD source changed")
        if name != "applied":
            expect(
                row["scene_sha256"] == marker["baseline_scene_sha256"],
                "scene written before concrete approval",
            )
            expect(
                not row["authorizations"] and not row["operations"],
                "preapproval stage obtained authority",
            )
        stages.append(row)
        (evidence / "hospital-stages.json").write_text(
            json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / (name + ".scene.json")).write_bytes(scene_raw)

    def scan():
        tui.wait("hospital modeling ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        tui.type(
            "开始医院八层隔离副本建模。先扫描和对账，展示全层总数及未确认对象；本轮先不准备或写入。"
            "分类必须依据CAD图块与几何，不能将洗手器改成椅子。不使用shell。"
        )
        tui.keys(ENTER, 0.5)
        shown = False

        def ledger(screen):
            nonlocal shown
            if "Write a task" in screen and "目标 / 自然语言反馈" not in screen and not shown:
                tui.keys(F6, 0.5)
                shown = True
                return False
            return "来源实例对账" in business(screen) and "pending_confirmation" in business(screen)

        screen = fact("hospital unresolved source ledger", ledger, 900)
        finished()
        checkpoint("scan", screen)

    if not report.step(
        "Agent scans the real hospital floor and preserves unresolved ledger", scan, tui
    ):
        return

    def classified(screen):
        path = directory / "source-interpretations.json"
        return path.exists() and classified_state_visible(
            screen, json.loads(path.read_bytes())["objects"], marker
        )

    def classify():
        targets = "；".join(a["source_id"] + "（CAD图块" + a["block"] + "）" for a in supported)
        message = (
            "本轮仅评估 "
            + targets
            + "。根据图块和几何记录为chair与bed试验类别，分类basis保留assumed；"
            "医生凳允许0.45米示意总高，病床允许0.6米示意总高，均保留assumed。"
            "台下洗手盆 "
            + unsupported["source_id"]
            + " 应保留为undercounter_basin，所需counter_host关系未核对，"
            "记录能力缺口，不能改成椅子或落地洗手盆。解释后重新扫描展示三者状态；先不准备。"
        )
        send(message)
        screen = fact(
            "hospital assessed category states",
            classified,
            900,
        )
        finished()
        checkpoint("classified", screen)

    if not report.step(
        "Agent classifies stool and bed and retains missing counter-host request", classify, tui
    ):
        return

    def prepare():
        nonlocal proposal
        send(
            "只为刚才评估的医生凳和病床准备一个可审批候选。保留各自CAD来源位置、旋转、镜像与平面尺寸，"
            "非中心图块原点按局部CAD边界对齐；允许的示意高度与分类都保留assumed。台下洗手盆仍缺counter_host，"
            "保持能力缺口且不纳入可应用方案。保护全部既有构件。先展示依据和候选，不预览、不批准、不应用。"
        )

        def ready(screen):
            p = candidate()
            return p is not None and f"方案 {p['proposal_id']} · 修订 1 · prepared" in business(
                screen
            )

        screen = fact("hospital mixed source-bound candidate", ready, 900)
        finished()
        proposal = candidate()
        checkpoint("prepared", screen)

    if not report.step(
        "Agent prepares two supported CAD categories without the missing-host basin", prepare, tui
    ):
        return

    def preview():
        send(
            "只预览当前医生凳和病床候选，build_id=hospital-mixed-preview，查询终态并验证。保持权威场景不变，"
            "不批准、不应用；产物查询仅用领域工具，不读取文件或shell。"
        )
        screen = fact(
            "hospital mixed preview validated",
            lambda s: (
                "hospital-mixed-preview" in business(s)
                and "产物验证 valid=true · current=true" in business(s)
            ),
            900,
        )
        finished()
        checkpoint("preview", screen)

    if not report.step(
        "Agent previews and validates the real multi-category hospital candidate", preview, tui
    ):
        return

    def approve():
        fact(
            "hospital exact approved version shown",
            lambda s: proposal["proposal_id"] in business(s) and "修订 1 · prepared" in business(s),
            120,
        )
        finished()
        tui.keys(CTRL_Y, 1)
        fact("hospital mixed scene applied", lambda s: "场景应用 applied" in business(s), 900)
        finished()
        send(
            "沿刚才已批准应用的医生凳和病床方案构建，build_id=hospital-mixed-built，查询终态并验证。"
            "最后重扫来源，已建两个来源对象分别可追溯；台下洗手盆保持capability_missing，其他未评估对象保留。"
            "不要重复应用或擅自处理其他对象，不使用shell。"
        )
        screen = fact(
            "hospital mixed applied artifact validated",
            lambda s: (
                "hospital-mixed-built" in business(s)
                and "产物验证 valid=true · current=true" in business(s)
            ),
            900,
        )
        finished()
        checkpoint("applied", screen)

    report.step(
        "Concrete approval builds two hospital categories and retains unresolved requests",
        approve,
        tui,
    )
