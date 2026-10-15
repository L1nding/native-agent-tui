"""Recover a completed isolated project through a fresh real TUI Agent, without replay."""

import argparse
import hashlib
import json
import re
import sys
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--timeout", type=int, default=600)
    args = parser.parse_args()
    from domain_project_images import admitted_images
    from dsh_tui_check import CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    modeler = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    bound = json.loads(context.read_bytes())
    project = modeler / bound["project_path"]
    expect(
        project.parent == modeler / "outputs"
        and project.name.startswith("project-live-fixture-"),
        "recovery requires a prior isolated project fixture",
    )
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    expect(
        set(registry["scopes"]) == {"f08", "f09.b"}, "completed two-floor scope missing"
    )

    def durable(directory):
        return [
            path
            for path in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
        ] + list(directory.glob("artifacts/*/build.json"))

    watched = list(project.iterdir()) + [context, registry_path]
    for record in registry["scopes"].values():
        child = json.loads((modeler / record["context_path"]).read_bytes())
        directory = modeler / "outputs/domain" / child["run_id"] / "changes"
        watched.append(modeler / child["scene_path"])
        watched.extend(durable(directory))
    buildings = sorted((context.parent / "project/building").glob("*/build.json"))
    for path in buildings:
        watched.extend(
            [path, path.parent / "input.scene.json", path.parent / "assembly.json"]
        )
        watched.extend(
            path.parent / ("model" + suffix)
            for suffix in [".glb", ".blend", ".validation.json"]
        )
    building_request = ""
    if buildings:
        building_request = (
            "另从inspect_project.recovery_index.buildings发现原楼栋任务；只调用inspect_project_building、"
            "validate_project_building核对原build_id，再render_project_building_review perspective并"
            "read_project_building_review_image读取实际楼栋PNG，request_id同上。"
            "禁止调用assemble_project_building或cancel_project_building，不重新装配或构建。"
        )
    # Private fact counters, new inspection receipts and image delivery are observations.
    baseline = {
        str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in watched
    }
    evidence = repo / "target" / ("project-recovery-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    report = Report(evidence)
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(modeler),
            "--domain-context",
            str(context),
            "--model",
            args.model,
            "--profile",
            args.profile,
            "--permission",
            "read-only",
            "--journal-dir",
            str(evidence / "journal"),
        ],
        repo,
        evidence,
    )
    request_id = evidence.name
    try:
        tui.wait(
            "fresh project session", lambda s: "建模 · " in s and "Ready" in s, 120
        )
        tui.keys(CTRL_U)
        tui.type(
            "恢复上次隔离项目的最终复核，只读；最终回复控制在300字，简述原身份、实际看见的内容和未检查项。"
            "加载building-modeling技能。inspect_project并"
            "select_project_target target=整栋、choices=[f09.b]，恢复原selection_id；"
            "prepare_project_floor f09.b读取原已应用结构，不重建。describe_capabilities读取已有"
            "operation_index和artifact_index；inspect_change、inspect_operation、inspect_artifact及"
            "validate_artifact核对原身份和实际产物。用原build_id重新render_artifact_review top，"
            "read_artifact_review_image读取实际PNG，request_id="
            + request_id
            + "。报告你实际看到的"
            "内容和无法判断的细节，人工复核保持not_run。不得authorize、apply_change、prepare_change、"
            "build_scene、preview_candidate或shell；不生成新operation_id/build_id，不重试副作用。"
            + building_request
        )
        tui.keys(ENTER)
        tui.wait("recovery turn running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "recovery turn complete",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "recovery turn failed")
        expect(
            "领域事实同步失败" not in tui.console.stable(),
            "recovery facts not synchronized",
        )
        images = []
        image_receipts = []
        for path in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(path.read_bytes())
            image = fact.get("artifact", {}).get("review_image", {})
            if (
                image.get("delivery_request_id") == request_id
                and image.get("delivery") == "attached_png"
            ):
                images.append(fact)
                image_receipts.append(path.stem)
        expect(
            images and all(f["project_scope"]["floor_key"] == "f09.b" for f in images),
            "recovered artifact PNG was not delivered in its owning scope",
        )
        building_images = []
        for path in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(path.read_bytes())
            image = fact.get("project_building", {}).get("review_image", {})
            if (
                image.get("delivery_request_id") == request_id
                and image.get("delivery") == "attached_png"
            ):
                building_images.append(fact)
                image_receipts.append(path.stem)
        if buildings:
            expect(building_images, "original building PNG was not read on recovery")
            expect(
                {path.parent.name for path in buildings}
                == {
                    path.parent.name
                    for path in (context.parent / "project/building").glob(
                        "*/build.json"
                    )
                },
                "recovery allocated a building job",
            )
        (evidence / "building-review-facts.json").write_text(
            json.dumps(building_images, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        expect(
            {
                str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in watched
            }
            == baseline,
            "recovery modified authority, proposal, grant, operation, build or project source",
        )
        for record in registry["scopes"].values():
            child = json.loads((modeler / record["context_path"]).read_bytes())
            directory = modeler / "outputs/domain" / child["run_id"] / "changes"
            actual = {str(path) for path in durable(directory)}
            expect(
                actual <= set(baseline),
                "recovery allocated a new proposal, grant, operation or build",
            )
        admitted = admitted_images(evidence / "journal", image_receipts)
        (evidence / "model-image-admission.json").write_text(
            json.dumps(admitted, indent=2), encoding="utf-8"
        )
        (evidence / "review-facts.json").write_text(
            json.dumps(images, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / "source-evidence.txt").write_text(
            str(args.evidence.resolve()), encoding="utf-8"
        )
        report.step(
            "fresh TUI Agent restores target, operations and actual PNG without replay",
            lambda: None,
            tui,
        )
        tui.keys("\x1b[17~")
        (evidence / "agent-observations.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        report.step(
            "recovery TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "project recovery", lambda error=error: (_ for _ in ()).throw(error), tui
        )
        raise
    finally:
        (evidence / "final-screen.txt").write_text(tui.screen(), encoding="utf-8")
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
