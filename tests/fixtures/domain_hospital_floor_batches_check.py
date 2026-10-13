"""Continue one concrete floor batch from the actual current Scene IR.

Only keyboard feedback advances the Agent. Existing grants, source groups,
scene bytes and sealed facts are observer evidence, never manufactured input.
"""

import hashlib
import json
import re
import time
from pathlib import Path

from domain_hospital_mixed_check import artifact_validation_visible, business


def run_hospital_floor_batch(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, F6, expect

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    scene = root / context["scene_path"]
    baseline = scene.read_bytes()
    baseline_sha = hashlib.sha256(baseline).hexdigest()
    marker = json.loads(
        (context_path.parent / "hospital-floor-acceptance.json").read_bytes()
    )
    stages = []
    known_facts = set()
    group_facts = []

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    def groups():
        for p in (directory / "facts").glob("*.json"):
            if p.name not in known_facts:
                known_facts.add(p.name)
                value = json.loads(p.read_bytes())
                if value["kind"] == "source_groups":
                    group_facts.append(value)
        return (
            max(group_facts, key=lambda g: g["fact_sequence"]) if group_facts else None
        )

    initial_grants = records("authorization-*.json")
    initial_operations = records("operation-*.json")
    lineage = {
        "context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
        "initial_scene_sha256": baseline_sha,
        "initial_authorizations": initial_grants,
        "initial_operations": initial_operations,
        "scope": "one continued batch; all remaining floor requests persist",
    }
    (evidence / "initial.scene.json").write_bytes(baseline)

    def save_lineage():
        (evidence / "hospital-floor-batch-lineage.json").write_text(
            json.dumps(lineage, ensure_ascii=False, indent=2), encoding="utf-8"
        )

    save_lineage()

    def checkpoint(name):
        screen = tui.screen()
        if "领域事实同步失败" in screen and " · Completed · " in screen:
            proposal_id = lineage.get("proposal_id")
            request = (
                f"先只读inspect_change查询当前提案 {proposal_id}；"
                if proposal_id
                else ""
            )
            send(
                "刚才工具查询出现同步提示，保持当前场景、授权、操作与所有产物字节。"
                + request
                + "仅用scan_source_groups重新读取当前事实，作为本轮最后一次领域调用。"
                "本轮不需要read_fact_view，不重新准备、批准、应用或构建，不运行shell。"
                "这是只读事实对账，不能重放writer。"
            )
            finished()
        pattern = (
            r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})"
        )
        screen = tui.wait(
            "full private Core checkpoint", lambda s: re.search(pattern, s), 90
        )
        kind, seq, size, digest = re.search(pattern, screen).groups()
        raw = (directory / "facts" / (digest + ".json")).read_bytes()
        expect(
            len(raw) == int(size) and hashlib.sha256(raw).hexdigest() == digest,
            "private Core bytes differ",
        )
        (evidence / "core-facts").mkdir(exist_ok=True)
        (evidence / "core-facts" / (digest + ".json")).write_bytes(raw)
        proposals = [
            json.loads(p.read_bytes())
            for p in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}", p.stem)
        ]
        row = {
            "stage": name,
            "time_ns": time.time_ns(),
            "scene_sha256": hashlib.sha256(scene.read_bytes()).hexdigest(),
            "core_fact": {
                "kind": kind,
                "sequence": int(seq),
                "bytes": int(size),
                "sha256": digest,
            },
            "proposals": proposals,
            "groups": groups(),
            "authorizations": records("authorization-*.json"),
            "operations": records("operation-*.json"),
            "artifacts": records("artifacts/*/build.json"),
        }
        if name in {"initial", "prepared", "previewed"}:
            expect(
                row["scene_sha256"] == baseline_sha
                and row["authorizations"] == initial_grants
                and row["operations"] == initial_operations,
                "authority changed before concrete approval",
            )
        stages.append(row)
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        (evidence / (name + ".scene.json")).write_bytes(scene.read_bytes())
        (evidence / "hospital-floor-batch-stages.json").write_text(
            json.dumps(stages, ensure_ascii=False, indent=2), encoding="utf-8"
        )

    def inspect():
        tui.wait(
            "floor continuation ready",
            lambda s: "建模" in s and "Initializing" not in s,
            180,
        )
        tui.type(
            "继续医院八层整层建模。先describe_capabilities和scan_source_groups，核对实际已建、"
            "剩余实例及每组提案诊断／来源依据。已有分类为assumed，必要宿主关系保留；"
            "当前先对账和展示全层剩余请求，不重复已建，不准备、不批准、不应用。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        fact(
            "current floor remaining groups displayed",
            lambda s: (
                groups() is not None
                and groups()["scene_sha256"] == baseline_sha
                and "全层来源分组" in business(s)
            ),
            1800,
        )
        finished()
        expect(
            groups()["total_objects"] == len(marker["expected_instances"]),
            "floor inventory incomplete",
        )
        checkpoint("initial")

    if not report.step(
        "Agent reconciles actual current floor without writes", inspect, tui
    ):
        return

    proposal = None

    def prepare():
        nonlocal proposal
        send(
            "继续所有剩余可执行组，为下一批最多100实例准备具体方案。沿用此前允许的assumed示意高度／"
            "细节，保持CAD平面、位置、旋转、镜像和全部已有实体。缺profile的落地家具／洁具提出明确"
            "数据定义；已有定义复用。宿主不全和碰撞实例保留实际问题，为其余实例继续准备。"
            "优先处理此前未完成的请求。只准备并展示，不预览、不批准、不应用。"
        )

        def prepared(screen):
            nonlocal proposal
            matches = [
                p
                for p in records("*.json")
                if "proposal_id" in p
                and p.get("state") == "prepared"
                and p["base_sha256"] == baseline_sha
                and p["proposal_id"] in business(screen)
            ]
            if len(matches) != 1:
                return False
            proposal = matches[0]
            return True

        fact("next current floor batch prepared", prepared, 1800)
        finished()
        expect(
            proposal and 1 <= len(proposal["component_plan"]["components"]) <= 100,
            "invalid concrete batch capacity",
        )
        built = {
            i["source_id"]
            for g in stages[0]["groups"]["groups"]
            for i in g["instances"]
            if i["state"] == "built"
        }
        ids = {
            c["source_instance_id"] for c in proposal["component_plan"]["components"]
        }
        expect(
            not ids & built
            and len(ids) == len(proposal["component_plan"]["components"]),
            "repeated built source",
        )
        expect(ids <= set(marker["expected_instances"]), "foreign floor source")
        lineage.update(
            proposal_id=proposal["proposal_id"],
            preview_id="floor-preview-"
            + proposal["proposal_id"]
            + "-"
            + evidence.name[-8:],
            build_id="floor-built-"
            + proposal["proposal_id"]
            + "-"
            + evidence.name[-8:],
        )
        save_lineage()
        checkpoint("prepared")

    if not report.step(
        "Agent prepares a bound batch from remaining floor requests", prepare, tui
    ):
        return

    def preview():
        args = {
            "proposal_id": proposal["proposal_id"],
            "expected_sha256": proposal["base_sha256"],
            "build_id": lineage["preview_id"],
        }
        send(
            "仅对当前具体版本调用preview_candidate，参数："
            + json.dumps(args, ensure_ascii=False)
            + "。inspect_artifact(wait_seconds=20)等终态并validate_artifact。只用领域工具，不批准、不应用。"
        )
        fact(
            "concrete next floor preview valid",
            lambda s: artifact_validation_visible(s, lineage["preview_id"]),
            1800,
        )
        finished()
        checkpoint("previewed")

    if not report.step(
        "Agent previews and validates exact next floor candidate", preview, tui
    ):
        return

    def apply():
        fact(
            "concrete next floor proposal visible",
            lambda s: (
                proposal["proposal_id"] in business(s) and "prepared" in business(s)
            ),
            120,
        )
        finished()
        tui.keys(CTRL_Y, 1)
        fact(
            "next floor batch actually applied",
            lambda s: "场景应用 applied" in business(s),
            1800,
        )
        finished()
        expect(
            hashlib.sha256(scene.read_bytes()).hexdigest()
            == proposal["candidate_sha256"],
            "applied candidate differs",
        )
        checkpoint("applied")

    if not report.step(
        "New concrete approval applies exactly this floor batch", apply, tui
    ):
        return

    def build():
        args = {
            "proposal_id": proposal["proposal_id"],
            "expected_sha256": proposal["candidate_sha256"],
            "build_id": lineage["build_id"],
        }
        send(
            "构建刚应用版本，build_scene参数："
            + json.dumps(args, ensure_ascii=False)
            + "。用inspect_artifact(wait_seconds=20)等待，validate_artifact后scan_source_groups。"
            "展示所有剩余请求，保留未决问题，不开始下一次批准。只用领域工具等待。"
        )
        fact(
            "next floor applied build valid",
            lambda s: artifact_validation_visible(s, lineage["build_id"]),
            1800,
        )
        finished()
        expect(
            groups()["scene_sha256"] == proposal["candidate_sha256"],
            "final group scan is stale",
        )
        checkpoint("built")

    report.step("Agent builds validates and regroups all remaining sources", build, tui)


def run_hospital_floor_batch_finalize(
    report, tui, send, finished, fact, context_path, evidence
):
    """Recover only the missing UI checkpoint of an already applied/built batch."""
    import os

    from dsh_tui_check import ENTER, F6, expect

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    parent = Path(os.environ["MODELING_FLOOR_BATCH_PARENT_EVIDENCE"]).resolve()
    parent_path = parent / "hospital-floor-batch-stages.json"
    parent_lineage = json.loads(
        (parent / "hospital-floor-batch-lineage.json").read_bytes()
    )
    stages = json.loads(parent_path.read_bytes())
    expect(
        [s["stage"] for s in stages] == ["initial", "prepared", "previewed", "applied"],
        "parent lacks actual pre-build checkpoints",
    )
    initial = scene.read_bytes()
    expect(
        hashlib.sha256(initial).hexdigest() == stages[-1]["scene_sha256"],
        "parent applied scene changed",
    )
    before_grants = [
        json.loads(p.read_bytes())
        for p in sorted(directory.glob("authorization-*.json"))
    ]
    before_ops = [
        json.loads(p.read_bytes()) for p in sorted(directory.glob("operation-*.json"))
    ]
    lineage = {
        "parent_evidence": str(parent),
        "parent_stages_sha256": hashlib.sha256(parent_path.read_bytes()).hexdigest(),
        "parent_report_sha256": hashlib.sha256(
            (parent / "report.json").read_bytes()
        ).hexdigest(),
        "context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
        "initial_scene_sha256": hashlib.sha256(initial).hexdigest(),
        "initial_authorizations": before_grants,
        "initial_operations": before_ops,
        "scope": "read-only post-build checkpoint recovery; original failure remains recorded",
    }
    (evidence / "hospital-floor-finalize-lineage.json").write_text(
        json.dumps(lineage, ensure_ascii=False, indent=2), encoding="utf-8"
    )

    def reconcile():
        tui.wait(
            "read-only floor finalize ready",
            lambda s: "建模" in s and "Initializing" not in s,
            180,
        )
        proposal = parent_lineage["proposal_id"]
        build = parent_lineage["build_id"]
        operation = next(
            o["operation_id"]
            for o in stages[-1]["operations"]
            if o["proposal_id"] == proposal
        )
        tui.type(
            f"恢复医院八层上一批的只读对账。提案 {proposal} 已实际应用，操作 {operation}；"
            f"已构建产物build_id={build}。本轮禁止重新准备、授权、应用或构建。"
            "只用领域工具依次查询describe_capabilities、inspect_change、inspect_operation、"
            "inspect_artifact、validate_artifact，最后调用scan_source_groups展示state_counts和剩余请求。"
            "最后一次领域调用必须是scan_source_groups；本轮不需要read_fact_view或重新分析线稿，"
            "Core会读取完整事实。仅查询已有数据，不运行shell，不请求额外写权限。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        fact(
            "reconciled current applied batch visible",
            lambda s: artifact_validation_visible(s, build),
            1800,
        )
        finished()
        pattern = (
            r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})"
        )
        screen = tui.wait(
            "reconciled full private Core checkpoint",
            lambda s: re.search(pattern, s),
            90,
        )
        kind, seq, size, digest = re.search(pattern, screen).groups()
        raw = (directory / "facts" / (digest + ".json")).read_bytes()
        expect(
            hashlib.sha256(raw).hexdigest() == digest and len(raw) == int(size),
            "reconciled Core bytes differ",
        )
        expect(scene.read_bytes() == initial, "readonly recovery wrote scene")
        grants = [
            json.loads(p.read_bytes())
            for p in sorted(directory.glob("authorization-*.json"))
        ]
        ops = [
            json.loads(p.read_bytes())
            for p in sorted(directory.glob("operation-*.json"))
        ]
        expect(
            grants == before_grants and ops == before_ops,
            "readonly recovery changed writer authority",
        )
        groups = [
            json.loads(p.read_bytes()) for p in (directory / "facts").glob("*.json")
        ]
        groups = [
            g
            for g in groups
            if g["kind"] == "source_groups"
            and g["scene_sha256"] == lineage["initial_scene_sha256"]
        ]
        expect(groups, "no current actual source regrouping")
        row = {
            "stage": "built",
            "time_ns": time.time_ns(),
            "scene_sha256": lineage["initial_scene_sha256"],
            "core_fact": {
                "kind": kind,
                "sequence": int(seq),
                "bytes": int(size),
                "sha256": digest,
            },
            "groups": max(groups, key=lambda g: g["fact_sequence"]),
            "proposals": [
                json.loads(p.read_bytes())
                for p in directory.glob("*.json")
                if re.fullmatch(r"[a-f0-9]{32}", p.stem)
            ],
            "authorizations": grants,
            "operations": ops,
            "artifacts": [
                json.loads(p.read_bytes())
                for p in sorted(directory.glob("artifacts/*/build.json"))
            ],
        }
        (evidence / "core-facts").mkdir(exist_ok=True)
        (evidence / "core-facts" / (digest + ".json")).write_bytes(raw)
        (evidence / "built.txt").write_text(screen, encoding="utf-8")
        (evidence / "built.scene.json").write_bytes(initial)
        (evidence / "hospital-floor-finalized-checkpoint.json").write_text(
            json.dumps(row, ensure_ascii=False, indent=2), encoding="utf-8"
        )

    report.step(
        "Agent reconciles actual prior batch without replaying writer or build",
        reconcile,
        tui,
    )
