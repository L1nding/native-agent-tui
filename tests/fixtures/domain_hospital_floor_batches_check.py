"""Continue one concrete floor batch from the actual current Scene IR.

Only keyboard feedback advances the Agent. Existing grants, source groups,
scene bytes and sealed facts are observer evidence, never manufactured input.
"""

import hashlib
import json
import re
import time
from pathlib import Path

from domain_hospital_mixed_check import business


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
            lambda s: (
                f"产物验证 valid=true · current=true · build_id={lineage['preview_id']}"
                in business(s)
            ),
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
            lambda s: (
                f"产物验证 valid=true · current=true · build_id={lineage['build_id']}"
                in business(s)
            ),
            1800,
        )
        finished()
        expect(
            groups()["scene_sha256"] == proposal["candidate_sha256"],
            "final group scan is stale",
        )
        checkpoint("built")

    report.step("Agent builds validates and regroups all remaining sources", build, tui)
