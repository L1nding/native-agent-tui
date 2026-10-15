"""Assess every configured source group through the TUI Agent without scene writes."""

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


def business_files(directory):
    return [
        path
        for path in directory.glob("*.json")
        if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem)
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--recover-assessments", action="store_true")
    args = parser.parse_args()
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    bound = json.loads(context.read_bytes())
    project = root / bound["project_path"]
    expect(project.parent == root / "outputs", "assessment requires isolated inputs")
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    scope = registry["scopes"][registry["active"]]
    child_context = root / scope["context_path"]
    child = json.loads(child_context.read_bytes())
    directory = root / "outputs/domain" / child["run_id"] / "changes"
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    initial = json.loads((args.evidence / "component-source-groups.json").read_bytes())
    watched = [
        context,
        registry_path,
        child_context,
        root / child["scene_path"],
        *project.iterdir(),
        *business_files(directory),
    ]
    for path in directory.glob("artifacts/*/build.json"):
        watched.append(path)
        output = Path(json.loads(path.read_bytes())["output"])
        watched.extend(
            output.with_suffix(suffix)
            for suffix in [".glb", ".blend", ".validation.json"]
        )
    if args.recover_assessments:
        metadata = directory / "source-interpretations.json"
        expect(metadata.is_file(), "original assessment metadata missing")
        original = json.loads(metadata.read_bytes())
        expect(
            len(original.get("group_assessments", {})) == initial["total_groups"],
            "original assessment is incomplete",
        )
        watched.append(metadata)
    before = {str(path): sha(path) for path in watched if path.is_file()}
    ids = {path.name for path in business_files(directory)}
    evidence = repo / "target" / ("source-assessment-live-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
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
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        tui.keys(CTRL_U)
        prompt = (
            "用户授权编码Agent代做决策的医院隔离来源评估。加载building-modeling技能。"
            "本轮由当前TUI Agent直接执行领域工具，不委派其他Agent，不shell、不旧workflow。"
            "仅解释来源、记录assumed分组评估及未决项，不prepare、授权、apply、build、preview或修改场景。"
            "inspect_project，select_project_target target="
            + selection["target"]
            + "恢复原选择；"
            "inspect_project_floor floor_key="
            + scope["floor_key"]
            + "、selection_id="
            + selection["selection_id"]
            + "。"
            "describe_capabilities与scan_source_groups读取当前明确配置图层的全部"
            + str(initial["total_groups"])
            + "组、"
            + str(initial["total_objects"])
            + "实例。"
            "逐组读取linework_m、local_bounds_m、来源测量、类别和实例上下文，按read_fact_view limit=20"
            "及next_offset读完省略的必要几何证据；不要仅按图块名字分类。"
            "每组用interpret_source_group记录你自己的类别或category=null及具体reason。"
            "有证据才能选类别；无法确认时保留null并解释，尺寸与安装依据缺失仍未决，不代替真实人确认。"
            "每组采用唯一稳定request_id，串行解释，使用上一工具返回的当前scan_id，不能并发沿旧scan_id提交。"
            "解释过的原组只读取不重写；最终再scan_source_groups核对每组assessment，487项全量实例"
            "身份保持原值。完成当前实例分页对账，并inspect_change核对原已应用结构。"
            "最后报告类别分布、可准备与缺能力/关系/未决状态，不把评估当已建或语义验收通过。"
        )
        if args.recover_assessments:
            prompt = (
                "已完成医院来源评估的只读恢复。加载building-modeling技能。此前16组、487实例已有"
                "原assumed分组判断，原场景未变；本轮不得解释或重写原判断。不要interpret、prepare、授权、"
                "apply、build、preview或shell，不委派其他Agent。inspect_project，select_project_target target="
                + selection["target"]
                + "恢复原选择；inspect_project_floor floor_key="
                + scope["floor_key"]
                + "、selection_id="
                + selection["selection_id"]
                + "。scan_source_groups读取原完整16组评估，"
                "只读各组assessment及类别/状态，不重复展开原线稿。read_fact_view limit=20，只读必要省略的"
                "assessment。随后scan_source_instances limit=1读一个最新分页，核对同一来源状态摘要。"
                "最后inspect_change读取原已应用结构，并简洁报告16组是否齐全、类别和待确认/待新增/缺能力"
                "的全量数量。保留assumed、真实语义未验收、人工not_run；不能把评估当建成。"
            )
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("assessment running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "assessment terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "assessment Agent failed")
        groups = []
        for path in (context.parent / "project/facts").glob("*.json"):
            value = json.loads(path.read_bytes())
            if value.get("kind") == "source_groups":
                groups.append(value)
        latest = max(groups, key=lambda value: value["fact_sequence"])
        expect(
            latest["total_objects"] == initial["total_objects"]
            and latest["total_groups"] == initial["total_groups"],
            "source range changed",
        )
        expect(
            latest["assessed_groups"] == initial["total_groups"],
            "some source groups remain unassessed",
        )
        expect(
            {g["group_id"] for g in latest["groups"]}
            == {g["group_id"] for g in initial["groups"]},
            "group identities changed",
        )
        for group in latest["groups"]:
            expect(
                group["assessment"] is not None and group["assessment"]["reason"],
                "empty assessment",
            )
        expect(
            all(sha(Path(path)) == digest for path, digest in before.items()),
            "assessment changed a business input or artifact",
        )
        expect(
            {path.name for path in business_files(directory)} == ids,
            "assessment allocated a proposal, grant or operation",
        )
        (evidence / "assessed-groups.json").write_text(
            json.dumps(latest, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        tui.wait(
            "assessment facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(120):
            screen = tui.console.stable()
            if "已评估 " + str(initial["total_groups"]) + " 组" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError(
                "Core did not display the complete group assessment count"
            )
        (evidence / "completed-screen.txt").write_text(screen, encoding="utf-8")
        report.step(
            "all source groups assessed by TUI Agent with original scene and business identities unchanged",
            lambda: None,
            tui,
        )
        report.step(
            "source assessment TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "source assessment acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
