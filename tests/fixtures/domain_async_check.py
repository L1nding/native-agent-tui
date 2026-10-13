"""C09 keyboard-only live Agent revisions with real late Blender completions."""

import hashlib
import json
import re
import time
from pathlib import Path


def run_async_checks(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_Y, ENTER, F6, expect

    context = json.loads(context_path.read_text(encoding="utf-8"))
    project = Path(context["project_root"])
    isolated = context_path.parent
    scene = project / context["scene_path"]
    records_dir = project / "outputs/domain" / context["run_id"] / "changes"
    baseline = hashlib.sha256(scene.read_bytes()).hexdigest()
    sources = {
        str(project / s["path"]): hashlib.sha256(
            (project / s["path"]).read_bytes()
        ).hexdigest()
        for s in context["sources"]
    }
    states = []
    versions = []
    observed_validations = {}

    def sequences():
        path = records_dir / "fact-sequences.json"
        return json.loads(path.read_bytes()) if path.exists() else {"facts": {}}

    def validation_key(build_id):
        # Read-only audit; never emits a domain result or manufactures a sequence.
        from blender_agent.integrations.domain_tools import DomainTools

        validated = DomainTools(context_path).artifacts.validate(build_id)
        expect(validated["valid"], "actual artifact failed read-only validation")
        payload = {
            "contract": "modeling-domain/v2",
            "run_id": context["run_id"],
            "context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
            "kind": "artifact_validation",
            "ok": True,
            "artifact": validated,
        }
        return hashlib.sha256(
            json.dumps(payload, sort_keys=True, ensure_ascii=False).encode()
        ).hexdigest()

    def records():
        return sorted(
            [
                json.loads(p.read_bytes())
                for p in records_dir.glob("*.json")
                if re.fullmatch(r"[a-f0-9]{32}", p.stem)
            ],
            key=lambda p: p["revision"],
        )

    def artifact(build_id):
        path = records_dir / "artifacts" / build_id / "build.json"
        return json.loads(path.read_bytes()) if path.exists() else None

    def gate(build_id):
        path = records_dir / "artifacts" / build_id / "live-blender-gate.json"
        if not path.exists():
            return None
        try:
            return json.loads(path.read_bytes())
        except ValueError:
            return None

    def checkpoint(name, screen):
        row = {
            "stage": name,
            "time_ns": time.time_ns(),
            "scene_sha256": hashlib.sha256(scene.read_bytes()).hexdigest(),
            "source_hashes": {
                p: hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in sources
            },
            "proposals": records(),
            "authorization_ids": sorted(
                p.stem for p in records_dir.glob("authorization-*.json")
            ),
            "operation_ids": sorted(
                p.stem for p in records_dir.glob("operation-*.json")
            ),
            "artifacts": {
                bid: artifact(bid)
                for bid in ("async-preview-v1", "async-preview-v2", "async-preview-v3")
            },
            "gates": {
                bid: gate(bid)
                for bid in ("async-preview-v1", "async-preview-v2", "async-preview-v3")
            },
        }
        row["fact_sequences"] = sequences()
        row["observed_validations"] = dict(observed_validations)
        states.append(row)
        (evidence / "async-stages.json").write_text(
            json.dumps(states, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / (name + ".txt")).write_text(screen, encoding="utf-8")
        expect(row["source_hashes"] == sources, "async run changed authoritative CAD")
        if name != "applied":
            expect(
                row["scene_sha256"] == baseline
                and not row["authorization_ids"]
                and not row["operation_ids"],
                "async revision/preview wrote scene or obtained approval",
            )
        return row

    def scan():
        tui.wait(
            "async modeling ready",
            lambda s: "建模" in s and "Initializing" not in s,
            120,
        )
        tui.type(
            "开始 floor.mixed。先查询 modeling_domain 能力并扫描来源台账；先不要准备、应用或请求批准。"
        )
        tui.keys(ENTER, 0.5)
        requested = False

        def ledger(screen):
            nonlocal requested
            if "Pending request" in screen and "Needs your approval" in screen:
                tui.keys(CTRL_Y, 1)
                return False
            if "Write a task" in screen and not requested:
                tui.keys(F6, 1)
                requested = True
                return False
            return "来源实例对账" in screen

        screen = fact("async source ledger", ledger, 240)
        finished()
        checkpoint("scan", screen)

    if not report.step("real Agent scans the async isolated floor", scan, tui):
        return

    def revision(number, prompt):
        send(prompt)

        def prepared(screen):
            rows = records()
            return (
                bool(rows)
                and rows[-1]["revision"] == number
                and rows[-1]["state"] == "prepared"
                and f"方案 {rows[-1]['proposal_id']} · 修订 {number} · prepared"
                in screen
                and gate(f"async-preview-v{number}") is not None
            )

        screen = fact(f"async revision {number} started", prepared, 300)
        finished()
        current = records()[-1]
        expect(
            len(records()) == number,
            "Agent branched or duplicated async proposal chain",
        )
        if number > 1:
            expect(
                current["parent_id"] == versions[-1]["proposal_id"],
                "async revision changed parent chain",
            )
        versions.append(current)
        bid = f"async-preview-v{number}"
        expect(
            artifact(bid)["state"] == "building" and "released_ns" not in gate(bid),
            "preview was not actually held in Blender",
        )
        expect(
            artifact("async-preview-v1")["state"] == "building",
            "first preview did not remain in flight",
        )
        checkpoint(f"v{number}-running", screen)

    if not report.step(
        "Agent prepares V1 and leaves its real Blender preview running",
        lambda: revision(
            1,
            "只为 source.mixed.instance.84.80.7E 准备一张来源椅子，保留来源XY及稳定身份，宽深0.5米、高0.9米允许为assumed示意值。方案prepared后调用preview_candidate，build_id必须为async-preview-v1，expected_sha256使用base_sha256。工具返回building后就结束本回合，暂不轮询或等待，不申请批准、不应用。",
        ),
        tui,
    ):
        return
    if not report.step(
        "Agent revises V2 while V1 remains in real Blender",
        lambda: revision(
            2,
            "V1预览还在运行。先inspect_change读取完整component_plan，沿当前提案链只把这张椅子向北移动1米，原计划其余字段完整保留（含scan_id、source_instance_id、来源旋转与镜像、共享定义），placement_basis=user_request。带parent_id、base_sha256和明确反馈准备V2，再启动preview_candidate，build_id=async-preview-v2。返回building后结束本回合，不等待V1或V2、不批准、不应用。",
        ),
        tui,
    ):
        return

    def release_and_inspect(number, historical=False):
        bid = f"async-preview-v{number}"
        (isolated / "gate-controls" / (bid + ".release")).write_text(
            str(time.time_ns()), encoding="utf-8"
        )
        tui.wait(
            f"actual Blender {bid} completes",
            lambda s: artifact(bid)["state"] in {"built", "failed"},
            90,
        )
        expect(artifact(bid)["state"] == "built", f"actual {bid} build failed")
        expected_key = validation_key(bid)
        send(
            f"请只调用inspect_artifact检查{bid}，再validate_artifact核验；这是{'历史V1预览，不能作为当前V3或批准依据' if historical else '当前候选预览'}。不要再准备方案、不要重新启动任何作业、不要批准或应用。明确build_id、proposal_id、revision、valid与current。"
        )
        finished()
        current = versions[-1]
        stage_name = f"v{number}-completed{'-historical' if historical else ''}"
        current_bid = f"async-preview-v{current['revision']}"

        def visible_artifact(screen):
            business = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
            flat = "".join(line.strip("│ ").strip() for line in business.splitlines())
            return (
                "产物验证 valid=true · current=true" in business
                and current_bid in flat
                and expected_key in sequences()["facts"]
            )

        screen = fact(f"async preview {number} inspected", visible_artifact, 120)
        observed_validations[bid] = expected_key
        binding_screen = fact(
            f"async current binding after preview {number}",
            lambda s: (
                f"方案 {current['proposal_id']} · 修订 {current['revision']} · prepared"
                in s
            ),
            120,
        )
        (evidence / (stage_name + "-binding.txt")).write_text(
            binding_screen, encoding="utf-8"
        )
        if historical:
            business = screen.split("方案与执行事实", 1)[-1].split("Agent 回复", 1)[0]
            flat = "".join(line.strip("│ ").strip() for line in business.splitlines())
            expect(
                current["revision"] == 3 and "async-preview-v1" not in flat,
                "late V1 became current business artifact",
            )
        checkpoint(f"v{number}-completed{'-historical' if historical else ''}", screen)

    if not report.step(
        "V2 actually completes and validates before V1",
        lambda: release_and_inspect(2),
        tui,
    ):
        return
    if not report.step(
        "Agent revises V3 while V1 still runs",
        lambda: revision(
            3,
            "V2已验证，V1仍运行。先inspect_change读取完整component_plan，沿原链只把这张椅子相对当前朝向旋转45度；完整保留scan_id和其余计划字段，位置、身份、来源和参数依据不变；带parent_id和明确反馈准备V3并启动preview_candidate，build_id=async-preview-v3。返回building后结束回合，不等V1，不批准、不应用。",
        ),
        tui,
    ):
        return
    if not report.step(
        "V3 actually completes and validates before V1",
        lambda: release_and_inspect(3),
        tui,
    ):
        return
    if not report.step(
        "late V1 stays historical and cannot replace current V3",
        lambda: release_and_inspect(1, historical=True),
        tui,
    ):
        return

    def approve():
        current = versions[-1]
        artifact_screen = fact(
            "async final V3 artifact reviewed",
            lambda s: (
                "产物验证 valid=true · current=true" in s
                and "async-preview-v3"
                in "".join(
                    line.strip("│ ").strip()
                    for line in s.split("方案与执行事实", 1)[-1]
                    .split("Agent 回复", 1)[0]
                    .splitlines()
                )
            ),
            120,
        )
        (evidence / "v3-reviewed-artifact.txt").write_text(
            artifact_screen, encoding="utf-8"
        )
        screen = fact(
            "async final concrete version reviewed",
            lambda s: f"方案 {current['proposal_id']} · 修订 3 · prepared" in s,
            120,
        )
        checkpoint("v3-reviewed", screen)
        tui.keys(CTRL_Y, 1)
        fact("async V3 applied", lambda s: "场景应用 applied" in s, 240)

        def applied_artifact(screen):
            builds = [
                json.loads(p.read_bytes())
                for p in (records_dir / "artifacts").glob("*/build.json")
            ]
            completed = [
                b for b in builds if not b["candidate"] and b["state"] == "built"
            ]
            if not completed:
                return False
            expect(
                len(completed) == 1
                and completed[0]["proposal_id"] == current["proposal_id"],
                "applied build changed version or duplicated",
            )
            return (
                "产物验证 valid=true · current=true" in screen
                and "场景应用 applied" in screen
                and validation_key(completed[0]["build_id"]) in sequences()["facts"]
            )

        screen = fact("async V3 applied artifact valid", applied_artifact, 300)
        finished()
        checkpoint("applied", screen)

    report.step(
        "concrete approval applies only V3 and validates its actual build", approve, tui
    )
