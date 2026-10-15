"""Switch existing floors A/B/A through a real TUI without replaying domain writes."""

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
    args = parser.parse_args()
    from dsh_tui_check import CTRL_HOME, CTRL_U, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    root = args.modeler_repo.resolve()
    context = Path((args.evidence / "context-path.txt").read_text().strip())
    bound = json.loads(context.read_bytes())
    project = root / bound["project_path"]
    expect(project.parent == root / "outputs", "requires an isolated prior project")
    registry_path = context.parent / "project/scopes.json"
    registry = json.loads(registry_path.read_bytes())
    keys = sorted(registry["scopes"])
    expect(len(keys) == 2, "requires two registered floors")
    selection = json.loads((context.parent / "project/selection.json").read_bytes())
    scopes_before = registry["scopes"]
    changes = []
    inputs = [context, *(p for p in project.rglob("*") if p.is_file())]
    for scope in scopes_before.values():
        child_context = root / scope["context_path"]
        child = json.loads(child_context.read_bytes())
        inputs.extend([child_context, root / child["scene_path"]])
        changes.append(root / "outputs/domain" / child["run_id"] / "changes")

    def business_hashes():
        watched = list(inputs)
        for directory in changes:
            watched.extend(
                p
                for p in directory.glob("*.json")
                if re.fullmatch(r"[a-f0-9]{32}|authorization-.+|operation-.+", p.stem)
            )
            watched.extend(
                p for p in (directory / "artifacts").rglob("*") if p.is_file()
            )
        watched.extend(
            p for p in (context.parent / "project/building").rglob("*") if p.is_file()
        )
        return {str(p): sha(p) for p in watched}

    before = business_hashes()
    evidence = repo / "target" / ("project-activation-live-" + uuid.uuid4().hex[:8])
    evidence.mkdir()
    (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
    (evidence / "before-hashes.json").write_text(
        json.dumps(before, indent=2), encoding="utf-8"
    )
    facts_dir = context.parent / "project/facts"
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
    receipts = []
    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        initial = (
            "这是已有隔离两层项目的楼层激活验收。加载building-modeling技能，inspect_project，"
            "select_project_target恢复target="
            + selection["target"]
            + "、choices="
            + json.dumps([key for key in selection["selected_keys"] if "." in key])
            + "。"
        )
        for step, key in enumerate([keys[0], keys[1], keys[0]]):
            tui.keys(CTRL_U)
            tui.type(
                (initial if step == 0 else "继续同一楼层激活验收。")
                + "只调用activate_project_floor floor_key="
                + key
                + "、selection_id="
                + selection["selection_id"]
                + "，随后inspect_project_floor核对同一"
                "当前scope。用inspect_change读取该scope原bootstrap_proposal_id。保持原提案和授权。"
                "不要prepare、apply、build、assemble、来源解释、shell或委派。"
                "最终回复150字以内，报告floor_key、activation_revision和原提案状态。"
            )
            tui.keys(ENTER)
            tui.wait("activation running", lambda s: " · Running · " in s, 60)
            tui.wait(
                "activation terminal",
                lambda s: " · Completed · " in s or " · Failed · " in s,
                args.timeout,
            )
            expect(" · Failed · " not in tui.screen(), "activation Agent failed")
            current = json.loads(registry_path.read_bytes())
            expect(current["active"] == key, "backend activated the wrong floor")
            expect(
                current["scopes"] == scopes_before,
                "activation changed registered scope records",
            )
            revision = current.get("activation_revision")
            expect(type(revision) is int and revision > 0, "activation revision absent")
            matching = []
            for path in facts_dir.glob("*.json"):
                fact = json.loads(path.read_bytes())
                scope = fact.get("project_scope", {})
                if (
                    fact.get("kind") == "project_scope"
                    and scope.get("floor_key") == key
                    and scope.get("activation_revision") == revision
                ):
                    matching.append((fact["fact_sequence"], path.stem, fact))
            expect(matching, "TUI did not seal the new activation fact")
            sequence, receipt, fact = max(matching, key=lambda item: item[0])
            expect(
                fact["proposal"]["proposal_id"]
                == scopes_before[key]["bootstrap_proposal_id"],
                "activation substituted another proposal",
            )
            expect(
                fact["proposal"]["state"] == "applied", "original applied state lost"
            )
            tui.wait(
                "activation facts synchronized",
                lambda s: (
                    "领域事实同步失败" not in s and "正在同步完整领域事实" not in s
                ),
                60,
            )
            tui.keys(CTRL_HOME)
            for _ in range(100):
                screen = tui.console.stable()
                if (
                    "当前楼层 " + key + " · " in screen
                    and "作用域 " + scopes_before[key]["scope_id"] in screen
                ):
                    break
                tui.keys("\x1b[6~")
            else:
                raise AssertionError(
                    "Core did not display the activated floor and original scope"
                )
            expect(
                business_hashes() == before,
                "activation changed original business files or identities",
            )
            if receipts:
                expect(
                    revision == receipts[-1]["activation_revision"] + 1,
                    "switch did not advance exactly one activation",
                )
                expect(
                    sequence > receipts[-1]["fact_sequence"],
                    "scope receipt sequence regressed",
                )
            receipts.append(
                {
                    "floor_key": key,
                    "activation_revision": revision,
                    "fact_sequence": sequence,
                    "fact_sha256": receipt,
                }
            )
            (evidence / (str(step) + "-" + key + "-screen.txt")).write_text(
                screen, encoding="utf-8"
            )
        (evidence / "activation-receipts.json").write_text(
            json.dumps(receipts, indent=2), encoding="utf-8"
        )
        report.step(
            "A-B-A scope facts displayed with increasing activation and unchanged proposals, grants and builds",
            lambda: None,
            tui,
        )
        report.step(
            "activation TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "floor activation acceptance",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
