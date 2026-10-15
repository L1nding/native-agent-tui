"""Fresh real TUI reads approved cross-floor identities and reports without replay."""

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
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args()
    from domain_project_images import admitted_images
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo, root = Path(__file__).resolve().parents[2], args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    scoped = json.loads((args.evidence / "connection-scope.json").read_bytes())
    decision = json.loads((args.evidence / "test-decision.json").read_bytes())
    previous = json.loads((args.evidence / "building-report.json").read_bytes())
    project = root / json.loads(context.read_bytes())["project_path"]
    expect(project.parent == root / "outputs", "isolated project required")
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    expect(
        registry["active_scope_id"] == scoped["scope_id"],
        "recovery requires same active connection scope",
    )
    watched = [context, registry_path, *project.rglob("*")]
    changes_directories = []
    for record in [
        *registry["scopes"].values(),
        *registry["connection_scopes"].values(),
    ]:
        child_context = root / record["context_path"]
        child = json.loads(child_context.read_bytes())
        changes = root / "outputs/domain" / child["run_id"] / "changes"
        changes_directories.append(changes)
        watched.extend([child_context, root / child["scene_path"]])
        watched.extend(
            path
            for path in changes.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
        )
        watched.extend(
            path for path in (changes / "artifacts").rglob("*") if path.is_file()
        )
    watched.extend(
        path
        for path in (context.parent / "project/building").rglob("*")
        if path.is_file()
    )
    before = {str(path): sha(path) for path in watched if path.is_file()}
    identities = {
        str(directory): {
            path.name
            for path in directory.glob("*.json")
            if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
        }
        for directory in changes_directories
    }
    evidence = repo / "target" / ("project-connection-recovery-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    image_arguments = []
    seen_images = set()
    for path in (context.parent / "project/facts").glob("*.json"):
        value = json.loads(path.read_bytes()).get("project_building", {})
        image = value.get("review_image", {})
        if (
            value.get("build_id") == previous["build_id"]
            and image.get("image_id") not in seen_images
            and image
        ):
            seen_images.add(image["image_id"])
            image_arguments.append(
                {
                    "build_id": previous["build_id"],
                    "image_id": image["image_id"],
                    "image_sha256": image["image_sha256"],
                    "request_id": evidence.name,
                }
            )
    expect(len(image_arguments) == 2, "exact original two image identities required")
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
            "recovery project ready", lambda s: "建模 · " in s and "Ready" in s, 120
        )
        tui.keys(CTRL_U)
        tui.type(
            "只读恢复已批准项目跨层连接，不准备、不授权、不apply、不assemble、不build、不shell或委派。"
            "加载building-modeling技能。inspect_project、select_project_target恢复target="
            + selection["target"]
            + "、choices=[f09.b]。先inspect_project_building及validate_project_building原基线build_id="
            + scoped["assembly_binding"]["build_id"]
            + "，再inspect_project_connection_scope scope_id="
            + scoped["scope_id"]
            + "、selection_id="
            + selection["selection_id"]
            + "（当前作用域已激活，此工具严格只读，不要prepare）。describe_capabilities读取changes、operation/artifact索引；"
            "inspect_change原proposal_id="
            + decision["proposal_id"]
            + "，inspect_operation原已应用操作，"
            "inspect_artifact/validate_artifact原连接构建。inspect_project_building、validate_project_building和"
            "check_project_building_connections、read_project_building_report原连接楼栋build_id="
            + previous["build_id"]
            + "。原16级楼梯来源与孔洞仍绑定，但人工／净高／规范未执行。已有两张楼栋图片只调用"
            "read_project_building_review_image重读相同image_id/hash，request_id="
            + evidence.name
            + "，不要重新render；原图精确读取参数="
            + json.dumps(image_arguments)
            + "。最后报告实际观察、原授权/操作和连接报告，300字以内。"
        )
        tui.keys(ENTER)
        tui.wait("connection recovery running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "connection recovery terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "connection recovery Agent failed")
        expect(
            all(sha(Path(path)) == digest for path, digest in before.items()),
            "recovery changed original authority/grant/job/artifact",
        )
        for directory in changes_directories:
            actual = {
                path.name
                for path in directory.glob("*.json")
                if re.fullmatch(
                    r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem
                )
            }
            expect(
                actual == identities[str(directory)],
                "recovery allocated new business identities",
            )
        facts = [
            (path, json.loads(path.read_bytes()))
            for path in (context.parent / "project/facts").glob("*.json")
        ]
        images = [
            path.stem
            for path, value in facts
            if value.get("project_building", {}).get("build_id") == previous["build_id"]
            and value["project_building"]
            .get("review_image", {})
            .get("delivery_request_id")
            == evidence.name
            and value["project_building"]["review_image"].get("delivery")
            == "attached_png"
        ]
        expect(len(images) >= 2, "recovered original two-view image reads missing")
        admission = admitted_images(evidence / "journal", images)
        (evidence / "image-admission.json").write_text(
            json.dumps(admission, indent=2), encoding="utf-8"
        )
        tui.wait(
            "recovery Core synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if "连接检查 passed" in screen and "总报告" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("recovered connected report absent in Core")
        (evidence / "completed-screen.txt").write_text(screen, encoding="utf-8")
        report.step(
            "new TUI restores exact connection operation and source-bound passed report with all business hashes unchanged",
            lambda: None,
            tui,
        )
        report.step(
            "connection recovery exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "project connection recovery",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
