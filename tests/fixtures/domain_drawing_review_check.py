"""Review an existing isolated project's real CAD candidate through a fresh TUI Agent.

Observers compare durable identities and read actual image admission metadata. They
never prepare, authorize, apply, build, render or judge through domain APIs.
"""

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
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--recover-judgement", action="store_true")
    parser.add_argument("--all-candidates", action="store_true")
    args = parser.parse_args()
    from domain_project_images import admitted_images
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    bound = json.loads(context.read_bytes())
    project = root / bound["project_path"]
    expect(
        project.parent == root / "outputs", "review must use isolated fixture inputs"
    )
    newest = max(p.stat().st_mtime for p in (repo / "src").rglob("*.rs"))
    expect(args.binary.stat().st_mtime >= newest, "rebuild release before real review")
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    active = registry["scopes"][registry["active"]]
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    child_context = root / active["context_path"]
    child = json.loads(child_context.read_bytes())
    directory = root / "outputs/domain" / child["run_id"] / "changes"
    watched = [
        context,
        registry_path,
        child_context,
        root / child["scene_path"],
        *project.iterdir(),
    ]
    for path in directory.glob("*.json"):
        if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem):
            watched.append(path)
    for path in directory.glob("artifacts/*/build.json"):
        watched.append(path)
        output = Path(json.loads(path.read_bytes())["output"])
        watched.extend(
            output.with_suffix(suffix)
            for suffix in [".glb", ".blend", ".validation.json"]
        )
    if args.all_candidates:
        watched.extend((directory / "drawing").glob("*/judgement-*.json"))
        watched.extend((directory / "drawing").glob("*/*.png"))
    before = {str(path): sha(path) for path in watched if path.is_file()}
    identities = {path.name for path in directory.glob("*.json")}
    evidence = repo / "target" / ("drawing-review-live-" + uuid.uuid4().hex[:8])
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
    request_id = "drawing_observation_" + uuid.uuid4().hex[:12]
    expected_requests = {request_id}
    all_requests = None
    if args.all_candidates:
        audits = [
            json.loads(path.read_bytes())
            for path in (directory / "drawing").glob("*/audit.json")
        ]
        audit = next(
            a
            for a in audits
            if a["binding"]["scene_sha256"] == sha(root / child["scene_path"])
        )
        previous = {
            row["candidate_id"]: row
            for row in (
                json.loads(path.read_bytes())
                for path in (directory / "drawing").glob("*/judgement-*.json")
            )
            if row["audit_id"] == audit["audit_id"]
        }
        all_requests = {
            candidate["id"]: previous[candidate["id"]]["request_id"]
            if candidate["id"] in previous
            else "whole_floor_"
            + hashlib.sha256(candidate["id"].encode()).hexdigest()[:16]
            for candidate in audit["candidates"]
        }
        expected_requests = set(all_requests.values())
    recovery = None
    if args.recover_judgement:
        previous = [
            json.loads(path.read_bytes())
            for path in (directory / "drawing").glob("*/judgement-*.json")
        ]
        expect(
            len(previous) == 1, "recovery requires one original immutable observation"
        )
        recovery = previous[0]
        request_id = recovery["request_id"]
        expected_requests = {request_id}
    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        tui.keys(CTRL_U)
        prompt = (
            "用户授权编码Agent代做决策的隔离图纸复核测试。加载building-modeling技能。"
            "本轮只读恢复当前项目与楼层，记录你的实际看图判断；不要prepare、授权、apply、build或shell。"
            "先inspect_project，select_project_target恢复原target="
            + selection["target"]
            + "、selection_id应等于"
            + selection["selection_id"]
            + "（原selected_keys="
            + json.dumps(selection["selected_keys"])
            + "）；"
            "再inspect_project_floor floor_key="
            + active["floor_key"]
            + "，selection_id用原值。"
            "audit_drawing limit=1 offset=0读取第一候选，核对候选ID与来源；render_drawing_candidate，"
            "再read_drawing_candidate_image读取实际PNG，request_id=" + request_id + "。"
            "根据真实图像和证据用record_drawing_judgement记录error/doubt/not_error及你的解释，"
            "本轮fix_operations保持空，不能只凭检测器名称确认错误；未能看清时记doubt。"
            "最后报告实际可见线条、检测测量、判断和未覆盖项；人工复核及医院语义完成仍not_run。"
        )
        if recovery:
            prompt = (
                "原图纸判断的只读恢复测试。加载building-modeling；inspect_project，select_project_target target="
                + selection["target"]
                + "恢复原选择，inspect_project_floor floor_key="
                + active["floor_key"]
                + "、selection_id="
                + selection["selection_id"]
                + "。audit_drawing读取当前原判断。"
                "调用read_drawing_candidate_image重新读取原PNG：audit_id="
                + recovery["audit_id"]
                + "，candidate_id="
                + recovery["candidate_id"]
                + "，image_sha256="
                + recovery["image_sha256"]
                + "，request_id="
                + recovery["request_id"]
                + "。报告原判断和你实际见到的图片。"
                "禁止record_drawing_judgement、prepare、授权、apply、build或shell；原观察保持不可变。"
            )
        if all_requests is not None:
            prompt = (
                "医院隔离项目的全量图纸候选审查。加载building-modeling技能。"
                "本轮仅审查并记录语义观察，不prepare、不授权、不apply、不build、不shell。"
                "inspect_project，select_project_target target="
                + selection["target"]
                + "恢复原选择，inspect_project_floor floor_key="
                + active["floor_key"]
                + "、selection_id="
                + selection["selection_id"]
                + "。audit_drawing分页核对全部候选，原audit_id="
                + audit["audit_id"]
                + "。对每项render_drawing_candidate，再read_drawing_candidate_image读取实际PNG，"
                "逐项依据图片和测量独立判断error/doubt/not_error，解释实际可见内容和不确定处，fix_operations保持空。"
                "原有判断不可重写，只重新读取同一图片；其余候选用record_drawing_judgement记录你的观察。"
                "不能仅按检测器名称批量同判，也不能把图像送达当看图完成。使用以下candidate_id到request_id映射："
                + json.dumps(all_requests, ensure_ascii=False)
                + "。所有read_fact_view limit=20，按next_offset读取。最后再audit_drawing读取完整当前计数并报告每种"
                "verdict数量及未执行修正、人工复核、医院构件语义等边界。"
            )
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("review running", lambda s: " · Running · " in s, 60)
        tui.wait(
            "review terminal",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "review Agent failed")
        judgements = list((directory / "drawing").glob("*/judgement-*.json"))
        matched = [
            json.loads(path.read_bytes())
            for path in judgements
            if json.loads(path.read_bytes())["request_id"] in expected_requests
        ]
        expect(
            len(matched) == len(expected_requests),
            "expected bound Agent observations were not recorded",
        )
        judgement = matched[0]
        for item in matched:
            expect(
                item["human_review"] == "not_run"
                and item["scene_application"] == "not_run"
                and item["judgement"]["fix_operations"] == [],
                "an observation promoted a correction or approval",
            )
        if all_requests is not None:
            expect(
                {item["candidate_id"] for item in matched} == set(all_requests),
                "candidate coverage differs from original audit",
            )
        expect(
            judgement["human_review"] == "not_run"
            and judgement["scene_application"] == "not_run",
            "observation promoted approval",
        )
        latest = {}
        for path in (context.parent / "project/facts").glob("*.json"):
            value = json.loads(path.read_bytes())
            image = value.get("drawing_image", {})
            request = image.get("request_id")
            if request in expected_requests and image.get("delivery") == "attached_png":
                sequence = value["fact_sequence"]
                if request not in latest or latest[request][0] < sequence:
                    latest[request] = (sequence, path.stem)
        receipts = [item[1] for item in latest.values()]
        expect(
            set(latest) == expected_requests, "actual CAD crop delivery facts missing"
        )
        admission = admitted_images(evidence / "journal", receipts)
        if all_requests is not None:
            (evidence / "agent-judgements.json").write_text(
                json.dumps(matched, ensure_ascii=False, indent=2), encoding="utf-8"
            )
        (evidence / "image-admission.json").write_text(
            json.dumps(admission, indent=2), encoding="utf-8"
        )
        (evidence / "agent-judgement.json").write_text(
            json.dumps(judgement, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        tui.wait(
            "drawing facts synchronized",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(80):
            screen = tui.console.stable()
            if f"已记录 {len(expected_requests)}/" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError(
                "Core did not display the persisted drawing judgement count"
            )
        if recovery:
            expect(
                judgement == recovery, "read-only recovery changed original observation"
            )
        (evidence / "completed-screen.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        expect(
            all(sha(Path(path)) == original for path, original in before.items()),
            "durable business input or artifact changed",
        )
        after_ids = {path.name for path in directory.glob("*.json")}
        expect(
            after_ids == identities,
            "review changed proposal/grant/operation identities",
        )
        report.step(
            "actual source-bound crop admitted and Agent judgement persisted without business writes",
            lambda: None,
            tui,
        )
        report.step(
            "drawing review TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "drawing review acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
