"""Real floor-directed workflow: the Agent chooses groups, definitions and batches.

Only keyboard input advances workflow. Reading sealed facts is an observer, not a writer.
No source IDs, CAD category mapping or generated plan is passed to the Agent.
"""

import hashlib
import json
import re
import time
from pathlib import Path

from domain_hospital_mixed_check import business


def run_hospital_floor_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, F6, expect

    context = json.loads(context_path.read_bytes())
    marker = json.loads((context_path.parent / "hospital-floor-acceptance.json").read_bytes())
    root = Path(context["project_root"])
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    scene = root / context["scene_path"]
    total = len(marker["expected_instances"])
    stages = []
    completed_sources = set()
    seen_facts = set()
    group_facts = []

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    def proposals():
        return [
            json.loads(p.read_bytes())
            for p in sorted(directory.glob("*.json"))
            if re.fullmatch(r"[a-f0-9]{32}", p.stem)
        ]

    def latest_groups():
        for path in (directory / "facts").glob("*.json"):
            if path.name not in seen_facts:
                value = json.loads(path.read_bytes())
                seen_facts.add(path.name)
                if value["kind"] == "source_groups":
                    group_facts.append(value)
        return max(group_facts, key=lambda f: f["fact_sequence"]) if group_facts else None

    def prepared():
        matches = [p for p in proposals() if p["state"] == "prepared"]
        return max(matches, key=lambda p: p["proposal_id"]) if len(matches) == 1 else None

    def checkpoint(name):
        screen = tui.screen()
        match = re.search(
            r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B · SHA256 ([a-f0-9]{64})", screen
        )
        expect(match is not None, "checkpoint missing private Core fact header")
        kind, sequence, size, sha = match.groups()
        raw = (directory / "facts" / (sha + ".json")).read_bytes()
        expect(
            len(raw) == int(size) and hashlib.sha256(raw).hexdigest() == sha,
            "checkpoint private fact bytes differ",
        )
        (evidence / "core-facts").mkdir(exist_ok=True)
        (evidence / "core-facts" / (sha + ".json")).write_bytes(raw)
        scene_raw = scene.read_bytes()
        row = {
            "stage": name,
            "time_ns": time.time_ns(),
            "core_fact": {
                "kind": kind,
                "sequence": int(sequence),
                "bytes": int(size),
                "sha256": sha,
            },
            "scene_sha256": hashlib.sha256(scene_raw).hexdigest(),
            "groups": latest_groups(),
            "proposals": proposals(),
            "authorizations": records("authorization-*.json"),
            "operations": records("operation-*.json"),
            "artifacts": records("artifacts/*/build.json"),
            "fact_sequences": records("fact-sequences.json")[0],
        }
        if name == "assessed" or name == "batch-1-prepared":
            expect(
                row["scene_sha256"] == marker["baseline_scene_sha256"]
                and not row["authorizations"]
                and not row["operations"],
                "authority changed before concrete platform approval",
            )
        stages.append(row)
        (evidence / "hospital-floor-stages.json").write_text(
            json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / (name + ".scene.json")).write_bytes(scene_raw)

    def assess():
        tui.wait("floor modeling ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        tui.type(
            "建模医院八层。请先完成整层来源扫描、所有分组的识别分析和实例对账，展示各组数量、"
            "分类依据、可执行的构建请求、歧义和缺能力项。现在先不准备或应用方案，缺参数问题"
            "集中写在回复中，供我下一条反馈回答。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)

        def assessed(screen):
            groups = latest_groups()
            return (
                groups is not None
                and groups["total_objects"] == total
                and groups["assessed_groups"] == groups["total_groups"]
                and "全层来源分组" in business(screen)
                and f"已评估 {groups['total_groups']} 组" in business(screen)
            )

        fact("hospital all source groups assessed", assessed, 1800)
        finished()
        expect(
            all(g.get("assessment", {}).get("reason") for g in latest_groups()["groups"]),
            "group assessment lacks evidence or unresolved reason",
        )
        checkpoint("assessed")

    if not report.step(
        "Floor-only target makes Agent assess every hospital source group", assess, tui
    ):
        return

    for batch in (1, 2):
        proposal = None

        def prepare():
            nonlocal proposal
            if batch == 1:
                send(
                    "先为你评估出有明确依据、现有能力支持的类别分批准备建模。可以使用明确标记的"
                    "示意高度和内部细节，全部保留assumed；CAD位置、平面尺寸、旋转与镜像保留。"
                    "缺宿主、未确认类别和缺验证器的对象保留问题。按类别／区域分批，每批容量最多100个，"
                    "先展示第一批完整方案，保留后续批请求，保护全部既有实体；先不预览、不批准。"
                )
            else:
                send(
                    "继续整层建模：第一批已建对象保持，以新场景基线和扫描事实，为其余已评估且能力"
                    "支持的对象准备下一批。保留assumed和CAD平面依据，尽量包含此前未建类别；"
                    "不要重复上一批实例，展示这一批方案和全层剩余请求。先不预览、不批准。"
                )
            fact(
                f"hospital floor batch {batch} prepared",
                lambda s: prepared() is not None and prepared()["proposal_id"] in business(s),
                1800,
            )
            finished()
            proposal = prepared()
            expect(proposal is not None, "no unique prepared batch")
            instances = proposal["component_plan"].get("components", [])
            expect(len(instances) >= 10, "floor workflow shrank to a representative sample")
            ids = {c["source_instance_id"] for c in instances}
            expect(
                len(ids) == len(instances) and not ids & completed_sources,
                "batch repeats a source instance",
            )
            expect(ids <= set(marker["expected_instances"]), "batch has foreign source")
            checkpoint(f"batch-{batch}-prepared")

        if not report.step(
            f"Agent prepares hospital batch {batch} from its own group analysis", prepare, tui
        ):
            return

        def preview():
            build_id = f"hospital-floor-preview-{batch}"
            send(
                f"只预览当前批方案，build_id={build_id}，查询终态并验证。不批准、不应用；"
                "产物查询使用领域工具。"
            )
            fact(
                f"hospital floor batch {batch} preview validated",
                lambda s: (
                    build_id in business(s) and "产物验证 valid=true · current=true" in business(s)
                ),
                1800,
            )
            finished()
            checkpoint(f"batch-{batch}-preview")

        if not report.step(f"Agent previews and validates hospital batch {batch}", preview, tui):
            return

        def apply():
            fact(
                f"hospital floor batch {batch} version displayed",
                lambda s: proposal["proposal_id"] in business(s) and "prepared" in business(s),
                120,
            )
            finished()
            tui.keys(CTRL_Y, 1)
            fact(
                f"hospital floor batch {batch} applied",
                lambda s: "场景应用 applied" in business(s),
                1800,
            )
            finished()
            build_id = f"hospital-floor-built-{batch}"
            send(
                f"构建刚批准应用的批次，build_id={build_id}，查询终态并验证。之后重扫全层分组，"
                "展示已建、待处理、缺能力和歧义；后续批继续等待我的具体批准。"
            )
            fact(
                f"hospital floor batch {batch} build validated",
                lambda s: (
                    build_id in business(s) and "产物验证 valid=true · current=true" in business(s)
                ),
                1800,
            )
            finished()
            completed_sources.update(
                c["source_instance_id"] for c in proposal["component_plan"]["components"]
            )
            checkpoint(f"batch-{batch}-applied")

        if not report.step(
            f"Concrete approval applies and builds hospital batch {batch}", apply, tui
        ):
            return
