"""Recompile every remaining floor-standing source on the actual current baseline.

Only platform keyboard feedback prepares the diagnostic proposal. The harness
never prepares/applies directly and never resolves collisions by moving CAD input.
"""

import hashlib
import json
import re
from pathlib import Path


def remaining_sources(scene, floor_id, parent):
    """Subtract all actually persisted batches on this floor, not only the last batch."""
    floor = next(f for f in scene["floors"] if f["id"] == floor_id)
    built = {s.get("metadata", {}).get("source_instance_id") for s in floor.get("solids", [])}
    return [c for c in parent["component_plan"]["components"]
            if c["source_instance_id"] not in built]


def run_hospital_remaining_check(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_HOME, ENTER, F6, expect
    from domain_hospital_mixed_check import business

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    parent_id = "f0969ac45ee4424ca7fd2e2089a72c93"
    parent = json.loads((directory / (parent_id + ".json")).read_bytes())
    baseline = scene.read_bytes()
    baseline_sha = hashlib.sha256(baseline).hexdigest()

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    applications = [p for p in records("operation-*.json") if p.get("state") in {"applied", "built"}
                    and p.get("candidate_sha256") == baseline_sha and p.get("proposal_id")]
    expect(len(applications) == 1, "current floor must bind one persisted applied operation")
    applied_id = applications[0]["proposal_id"]
    candidates = remaining_sources(json.loads(baseline), context["scope"]["floor_id"], parent)
    expect(bool(candidates), "current floor has no remaining historical candidates")
    count = len(candidates)

    grants, operations = records("authorization-*.json"), records("operation-*.json")
    old_proposals = {p.stem for p in directory.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}", p.stem)}
    source_state = {name: (directory / name).read_bytes() if (directory / name).exists() else None
                    for name in ["source-interpretations.json", "source-dispositions.json"]}
    lineage = {"context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
               "scene_sha256": baseline_sha, "historical_parent_id": parent_id,
               "historical_parent_sha256": hashlib.sha256((directory / (parent_id + ".json")).read_bytes()).hexdigest(),
               "applied_proposal_id": applied_id, "expected_components": candidates,
               "initial_authorizations": grants, "initial_operations": operations,
               "source_state_hashes": {k: hashlib.sha256(v).hexdigest() if v is not None else None
                                       for k, v in source_state.items()}}
    (evidence / "remaining-lineage.json").write_text(json.dumps(lineage, indent=2), encoding="utf-8")
    (evidence / "remaining-baseline.scene.json").write_bytes(baseline)

    def assess():
        tui.wait("remaining floor assessment ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        selectors = [c["component_id"] for c in candidates]
        tui.type(
            "继续核查医院八层剩余来源。先describe_capabilities、scan_source_groups和inspect_change。"
            f"历史89实例提案{parent_id}在旧基线有32个碰撞实体；当前基线由已应用提案{applied_id}绑定。"
            f"本轮按当前Scene IR扣除所有已应用批次后，核查以下{count}个未建实例，不能把旧基线问题直接当作当前诊断："
            + json.dumps(selectors, ensure_ascii=False)
            + f"。读取历史提案component_plan，对这{count}个保留来源、平面位置、旋转、镜像、尺寸、"
            "高度、参数依据及定义。复用实际已应用定义；尚未落盘定义保留原参数依据并完整带入。"
            "用prepare_group_change按当前group_id及精确source_ids子集展开这批来源。"
            "definitions只带所需profiles和尚未落盘定义，components/openings/connections留空；"
            "placements引用历史template_id与arguments，让Python确定性展开CAD变换，不手抄坐标。"
            "保持placement_basis=source；姿态冲突需核对指定来源，不能改成user_request来绕过。"
            "传当前scan_id和当前scene_sha256，形成独立当前基线提案。"
            "不要移位、缩小、改高度、改类别、丢弃实例或修改来源处置来绕过碰撞。"
            "不预览、不批准、不应用、不构建；本轮只要完整的新阻塞诊断和可回答的处理选择。"
            "其余193个安装／关系／类别问题保留真实缺口，不能通过通用落地profile掩盖。"
            "准备后inspect_change查询新提案完整诊断，最后scan_source_groups展示全部剩余请求。"
            "仅用领域工具，不运行shell或额外模型。结束回复说明保持CAD位置时哪些对象无法新增，"
            "后续需要用户提供哪些安装资料或选择，不请求当前批准。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        finished()
        proposals = [p for p in records("*.json") if p.get("proposal_id") not in old_proposals
                     and p.get("base_sha256") == baseline_sha and p.get("state") == "blocked"]
        matches = [p for p in proposals if {c["component_id"] for c in p["component_plan"]["components"]} == set(selectors)]
        expect(len(matches) == 1, "no complete current diagnostic proposal for all remaining sources")
        proposal = matches[0]
        actual = {c["component_id"]: c for c in proposal["component_plan"]["components"]}
        fields = ["template_id", "origin", "rotation_z_deg", "scale", "arguments", "basis",
                  "source_ids", "source_instance_id", "required_relations", "placement_basis"]
        for expected in candidates:
            expect(all(actual[expected["component_id"]][field] == expected[field] for field in fields),
                   "current assessment altered source geometry or parameters")
        expect(len(proposal["issues"]) >= count and {i["code"] for i in proposal["issues"]} == {"collision"},
               "new diagnostics do not prove current collision constraints")
        expect(all(any(issue["entity_id"] == component_id or
                       issue["entity_id"].startswith(component_id + ".")
                       for issue in proposal["issues"]) for component_id in selectors),
               "current diagnostic proposal omits a remaining source's actual blocker")
        fact("current remaining diagnostic proposal visible", lambda s: proposal["proposal_id"] in business(s), 90)
        expect(scene.read_bytes() == baseline, "diagnostic assessment wrote scene")
        expect(records("authorization-*.json") == grants and records("operation-*.json") == operations,
               "diagnostic assessment acquired approval or replayed writer")
        for name, before in source_state.items():
            after = (directory / name).read_bytes() if (directory / name).exists() else None
            expect(after == before, "assessment silently changed source interpretation or disposition")
        tui.keys(CTRL_HOME, 0.2)
        pattern = r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})"
        screen = tui.wait("remaining complete Core fact", lambda s: re.search(pattern, s), 90)
        kind, seq, size, digest = re.search(pattern, screen).groups()
        raw = (directory / "facts" / (digest + ".json")).read_bytes()
        expect(hashlib.sha256(raw).hexdigest() == digest and len(raw) == int(size), "Core bytes differ")
        (evidence / "remaining-core-fact.json").write_bytes(raw)
        (evidence / "remaining-core-screen.txt").write_text(screen, encoding="utf-8")
        replies = []
        for _ in range(8):
            replies.append(tui.screen())
            tui.keys("\x1b[5;2~", 0.15)
        (evidence / "remaining-reply-pages.txt").write_text("\n\n".join(replies), encoding="utf-8")
        result = {"proposal_id": proposal["proposal_id"], "proposal": proposal,
                  "core_fact": {"kind": kind, "sequence": int(seq), "bytes": int(size), "sha256": digest},
                  "scope": "current-baseline diagnostic-only assessment; no resolved collisions or new build"}
        (evidence / "remaining-result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")

    report.step("Agent revalidates all remaining floor-standing sources without changing CAD or authority", assess, tui)
