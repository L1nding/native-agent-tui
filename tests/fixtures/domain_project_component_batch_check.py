"""Prepare, approve and build one real hospital source-group batch through the TUI."""

import argparse
import hashlib
import json
import re
import sys
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--assessments", type=Path, required=True)
    parser.add_argument(
        "--category", choices=["bed", "chair", "basin", "remaining"], default="bed"
    )
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--timeout", type=int, default=1800)
    args = parser.parse_args()
    from domain_project_component_geometry import verify_batch_geometry
    from domain_project_images import admitted_images
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    bound = json.loads(context.read_bytes())
    project = root / bound["project_path"]
    expect(project.parent == root / "outputs", "requires an isolated hospital copy")
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    scope = registry["scopes"][registry["active"]]
    child_context = root / scope["context_path"]
    child = json.loads(child_context.read_bytes())
    scene = root / child["scene_path"]
    original_scene = json.loads(scene.read_bytes())
    original_sha = sha(scene)
    changes = root / "outputs/domain" / child["run_id"] / "changes"
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    assessment = json.loads((args.assessments / "assessed-groups.json").read_bytes())
    categories = {"chair", "basin"} if args.category == "remaining" else {args.category}
    group_ids = [
        g["group_id"]
        for g in assessment["groups"]
        if g.get("assessment", {}).get("category") in categories
    ]
    expect(group_ids, "requested assessed category absent")
    allowed_sources = {
        instance["source_id"]
        for group in assessment["groups"]
        if group["group_id"] in group_ids
        for instance in group["instances"]
        if instance["state"] == "pending_addition"
    }
    evidence = repo / "target" / ("hospital-component-batch-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-scene.json").write_text(
        json.dumps(original_scene, ensure_ascii=False), encoding="utf-8"
    )
    durable = [context, child_context, *project.rglob("*")]
    durable.extend(
        changes / name
        for name in ["source-interpretations.json", "source-dispositions.json"]
        if (changes / name).is_file()
    )
    durable.extend(
        p
        for p in changes.glob("*.json")
        if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", p.stem)
    )
    durable.extend(p for p in (changes / "artifacts").rglob("*") if p.is_file())
    before = {str(p): sha(p) for p in durable if p.is_file()}
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    old_proposals = {
        p.name for p in changes.glob("*.json") if re.fullmatch(r"[a-f0-9]{32}", p.stem)
    }
    old_grants = {p.name for p in changes.glob("authorization-*.json")}
    old_jobs = {p.parent.name for p in changes.glob("artifacts/*/build.json")}
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(root),
            "--domain-context",
            str(context),
            "--profile",
            args.profile,
            "--model",
            args.model,
            "--permission",
            "read-only",
            "--journal-dir",
            str(evidence / "journal"),
        ],
        repo,
        evidence,
    )
    report = Report(evidence)
    try:
        tui.wait(
            "hospital project ready", lambda s: "建模 · " in s and "Ready" in s, 120
        )
        assumptions = {
            "bed": "床面顶高0.55m、床垫厚0.10m、床头最高0.90m、落地支腿为示意；移动床轮脚结构不得宣称已确认",
            "chair": "座面顶高0.45m、座面厚0.05m、靠背最高0.85m、支腿接地为示意",
            "basin": "独立柱式支承落地、盆沿顶高0.80m、深0.18m为示意；盆体按完整CAD平面轮廓设计",
        }
        assumptions["remaining"] = assumptions["chair"] + "；" + assumptions["basin"]
        prompt = (
            "继续原隔离医院487来源实例业务验收。编码Agent依据用户委托作测试决定，非真实医院人工验收。"
            "加载building-modeling技能，inspect_project并select_project_target恢复原target="
            + selection["target"]
            + "，activate_project_floor floor_key="
            + scope["floor_key"]
            + "、selection_id="
            + selection["selection_id"]
            + "，inspect_change读取原已应用结构。describe_capabilities、scan_source_instances与scan_source_groups"
            "先核对完整487实例、16组与既有评估，不重写解释。"
            "本批仅处理categories="
            + json.dumps(sorted(categories))
            + "且状态pending_addition的来源，group_id="
            + json.dumps(group_ids)
            + "。本次明确允许以下缺失竖向参数作为assumed示意："
            + assumptions[args.category]
            + "。CAD平面尺度、原点、旋转、镜像和source_id优先，不移动避碰，不将assumed改成user_confirmed。"
            "逐页读完整共享linework_m和实例；用prepare_group_change、definitions自行设计有意义的模板／部件，"
            "placements精确source_ids仅纳入pending_addition，不纳入疑似重复；单批最多100实例。"
            "保留所有既有结构与构件，声明材料／参数假设。碰撞或其他真实诊断不得忽略；有阻塞时保留对应来源未决，"
            "以source_ids子集准备最大可批准批次，不通过移动、改类别或缩小CAD足迹规避。"
            "准备后inspect_change展示该具体版本、纳入实例数、假设和剩余问题，等待本TUI具体Ctrl+Y批准。"
            "不要提前apply、build、authorize或shell，不委派其他Agent。获平台具体批准后，沿授权精确版本apply_change，"
            "build_scene、inspect_artifact等待并validate_artifact；再scan_source_groups/instances核对完整487与建成/未决。"
            "最后render_artifact_review top和perspective并read_artifact_review_image实际看两张图，request_id="
            + evidence.name
            + "，报告看见的布局与能力限制，人工复核仍not_run。最终回复400字以内。"
        )
        (evidence / "test-policy.json").write_text(
            json.dumps(
                {
                    "category": args.category,
                    "groups": group_ids,
                    "assumptions": assumptions[args.category],
                    "placement": "preserve CAD source transforms",
                    "approval": "exact displayed TUI candidate only",
                    "human_confirmation": False,
                },
                ensure_ascii=False,
                indent=2,
            ),
            encoding="utf-8",
        )
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("component preparation running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "component preparation terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(
            " · Failed · " not in tui.screen(), "component Agent failed before approval"
        )
        proposals = [
            json.loads(p.read_bytes())
            for p in changes.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}", p.stem) and p.name not in old_proposals
        ]
        ready = [
            p for p in proposals if p["state"] == "prepared" and p.get("component_plan")
        ]
        expect(
            len(ready) == 1,
            "requires one unambiguous valid concrete candidate; inspect diagnostic evidence",
        )
        proposal = ready[0]
        plan = proposal["component_plan"]
        candidates = plan.get("components", [])
        expect(0 < len(candidates) <= 100, "invalid batch count")
        expect(
            {candidate["source_instance_id"] for candidate in candidates}
            <= allowed_sources,
            "candidate includes another source category/state",
        )
        expect(
            all(c["placement_basis"] == "source" for c in candidates),
            "CAD placement lock lost",
        )
        expect(
            proposal["base_sha256"] == original_sha and sha(scene) == original_sha,
            "authority changed before approval",
        )
        expect(
            {p.name for p in changes.glob("authorization-*.json")} == old_grants,
            "unexpected grant before approval",
        )
        for path, digest in before.items():
            expect(
                sha(Path(path)) == digest,
                "original business file changed during preparation: " + path,
            )
        tui.wait(
            "component fact synchronization",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(150):
            screen = tui.console.stable()
            if (
                "方案 " + proposal["proposal_id"] in screen
                and "修订 " + str(proposal["revision"]) in screen
            ):
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("concrete component candidate not visible")
        (evidence / "before-approval.txt").write_text(screen, encoding="utf-8")
        decision = {
            key: proposal[key]
            for key in [
                "proposal_id",
                "revision",
                "prepare_token",
                "base_sha256",
                "candidate_sha256",
            ]
        }
        decision.update(
            actor="coding_agent_as_user_authorized_test_proxy",
            real_project_human_confirmation=False,
            category=args.category,
            instance_count=len(candidates),
        )
        (evidence / "test-decision.json").write_text(
            json.dumps(decision, indent=2), encoding="utf-8"
        )
        report.step(
            "TUI Agent prepares and Core displays one exact hospital component batch",
            lambda: None,
            tui,
        )
        tui.keys(CTRL_Y)
        tui.wait("approved batch running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "approved batch terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(
            " · Failed · " not in tui.screen(), "component Agent failed after approval"
        )
        expect(
            sha(scene) == proposal["candidate_sha256"],
            "authority differs from approved candidate",
        )
        grants = [
            json.loads(p.read_bytes())
            for p in changes.glob("authorization-*.json")
            if p.name not in old_grants
        ]
        expect(
            len(grants) == 1 and grants[0]["proposal_id"] == proposal["proposal_id"],
            "exact grant count mismatch",
        )
        for path, digest in before.items():
            expect(
                sha(Path(path)) == digest,
                "old proposal/input/grant/artifact changed: " + path,
            )
        current_scene = json.loads(scene.read_bytes())
        geometry = verify_batch_geometry(
            original_scene, current_scene, proposal, allowed_sources
        )
        (evidence / "independent-geometry-check.json").write_text(
            json.dumps(geometry, indent=2), encoding="utf-8"
        )
        jobs = [
            json.loads(p.read_bytes())
            for p in changes.glob("artifacts/*/build.json")
            if p.parent.name not in old_jobs
        ]
        built = [
            job
            for job in jobs
            if job["state"] == "built" and job["proposal_id"] == proposal["proposal_id"]
        ]
        expect(built, "approved candidate has no actual built artifact")
        for job in built:
            validation = json.loads(
                Path(job["output"]).with_suffix(".validation.json").read_bytes()
            )
            expect(validation["valid"], "actual validation invalid")
            for phase in ["built", "rebuilt", "glb_roundtrip"]:
                expect(validation[phase]["valid"], "invalid build phase " + phase)
        images = []
        latest_groups = []
        for p in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(p.read_bytes())
            image = fact.get("artifact", {}).get("review_image", {})
            if (
                image.get("delivery_request_id") == evidence.name
                and image.get("delivery") == "attached_png"
            ):
                images.append(p.stem)
            if (
                fact.get("kind") == "source_groups"
                and fact.get("scene_sha256") == proposal["candidate_sha256"]
            ):
                latest_groups.append(fact)
        expect(latest_groups, "post-application complete source groups missing")
        final = max(latest_groups, key=lambda f: f["fact_sequence"])
        expect(
            final["total_objects"] == 487 and final["total_groups"] == 16,
            "complete source coverage changed",
        )
        expect(
            final["state_counts"].get("built", 0) >= len(candidates),
            "batch sources not reconciled as built",
        )
        (evidence / "final-groups.json").write_text(
            json.dumps(final, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        expect(len(images) >= 2, "missing actual top/perspective PNG reads")
        admission = admitted_images(evidence / "journal", images)
        (evidence / "image-admission.json").write_text(
            json.dumps(admission, indent=2), encoding="utf-8"
        )
        (evidence / "final-screen.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        report.step(
            "exactly approved batch applied and real Blender/rebuild/GLB plus actual image admission verified",
            lambda: None,
            tui,
        )
        report.step(
            "hospital component TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "hospital component batch acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
