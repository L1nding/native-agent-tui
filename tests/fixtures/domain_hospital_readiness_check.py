"""Read-only real platform audit of current-floor data-profile availability."""

import hashlib
import json
import re
from pathlib import Path


def run_hospital_readiness_check(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_HOME, ENTER, F6, expect
    from domain_hospital_mixed_check import business

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    baseline = scene.read_bytes()

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    grants, operations = records("authorization-*.json"), records("operation-*.json")
    source_state = {name: hashlib.sha256((directory / name).read_bytes()).hexdigest()
                    if (directory / name).exists() else None
                    for name in ["source-interpretations.json", "source-dispositions.json"]}
    lineage = {"scene_sha256": hashlib.sha256(baseline).hexdigest(),
               "context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
               "initial_authorizations": grants, "initial_operations": operations,
               "source_state_hashes": source_state}
    (evidence / "readiness-lineage.json").write_text(json.dumps(lineage, indent=2), encoding="utf-8")

    def inspect():
        tui.wait("current floor readiness ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        tui.type(
            "只读核对医院八层当前来源状态。先describe_capabilities，读取capability_version和"
            "reusable_category_profiles，再scan_source_groups。只报告已建、可继续准备和真实关系／"
            "定义缺口，不准备、不批准、不应用、不构建，不修改解释或来源处置。"
            "已实际应用的落地类别应按复用数据定义解释，pending_addition不表示碰撞已解决；"
            "缺宿主不能通过profile省略，人工语义／视觉未复核保持未确认。"
            "最后一次领域调用必须scan_source_groups；不需要逐实例read_fact_view或另起shell。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        fact("current floor profile readiness counts visible",
             lambda s: all(value in business(s) for value in
                           ["built 265", "pending_addition 29", "capability_missing 193"]), 1800)
        finished()
        expect(scene.read_bytes() == baseline, "readiness check wrote scene")
        expect(records("authorization-*.json") == grants and records("operation-*.json") == operations,
               "readiness check changed writer authority")
        for name, expected in source_state.items():
            path = directory / name
            expect((hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None) == expected,
                   "readiness check reclassified or changed disposition")
        tui.keys(CTRL_HOME, 0.2)
        pattern = r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})"
        screen = tui.wait("readiness complete Core fact", lambda s: re.search(pattern, s), 90)
        kind, sequence, size, digest = re.search(pattern, screen).groups()
        raw = (directory / "facts" / (digest + ".json")).read_bytes()
        expect(hashlib.sha256(raw).hexdigest() == digest and len(raw) == int(size), "Core bytes differ")
        facts = records("facts/*.json")
        groups = max((f for f in facts if f["kind"] == "source_groups"
                      and f["scene_sha256"] == lineage["scene_sha256"]
                      and f["capability_version"] == "floor-components/6"), key=lambda f:f["fact_sequence"])
        expect(groups["state_counts"] == {"built":265,"pending_addition":29,"capability_missing":193},
               "actual whole-floor readiness counts differ")
        (evidence / "readiness-core-fact.json").write_bytes(raw)
        (evidence / "readiness-core-screen.txt").write_text(screen, encoding="utf-8")
        result = {"groups":groups,"core_fact":{"kind":kind,"sequence":int(sequence),
                  "bytes":int(size),"sha256":digest},"scene_and_authority_unchanged":True,
                  "scope":"source readiness only; no newly resolved geometry, relationships or approval"}
        (evidence / "readiness-result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")

    report.step("Platform reconciles applied profiles without misreporting readiness as construction", inspect, tui)
