"""Exercise the ordinary project entry against a private copy of the hospital floor.

Only the TUI Agent prepares, applies and builds. The coding Agent approves the
specific deterministic structure as a trial; no hospital semantic completion is inferred.
"""

import argparse
import hashlib
import json
import re
import shutil
import sys
import time
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify_component_inventory(
    root, context, child, candidate_sha256, evidence, *, expected_state_counts=None
):
    import ezdxf

    # This observer walks raw CAD independently; it never invokes domain scanning tools.
    sys.path.insert(0, str(root / "scripts"))
    from hospital_source_evidence import audit_scan_pages, inventory

    spec = child["sources"][0]
    source = (root / spec["path"]).resolve()
    assert source.is_relative_to(root)
    expected = inventory(ezdxf.readfile(source), spec)
    facts = []
    for path in (context.parent / "project/facts").glob("*.json"):
        value = json.loads(path.read_bytes())
        if value.get("kind") == "source_ledger":
            facts.append(value)
    result = audit_scan_pages(
        facts,
        expected,
        candidate_sha256,
        sha(source),
        expected_state_counts=expected_state_counts,
    )
    result.update(
        valid=True, component_layers=spec["component_layers"], source_sha256=sha(source)
    )
    (evidence / "independent-component-inventory.json").write_text(
        json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--resume-evidence", type=Path)
    parser.add_argument(
        "--continue-approved",
        action="store_true",
        help="recover one exact durable Core grant from prior evidence; never reapprove",
    )
    parser.add_argument(
        "--component-source-context",
        type=Path,
        help="copy only the declared component layers/rules from this original context",
    )
    parser.add_argument("--expected-source-total", type=int)
    args = parser.parse_args()
    if args.resume_evidence and args.component_source_context:
        parser.error("resume uses its immutable original source policy")
    if args.continue_approved and not args.resume_evidence:
        parser.error("--continue-approved requires --resume-evidence")
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    root = args.modeler_repo.resolve()
    original = root / "projects/hospital-site"
    token = uuid.uuid4().hex[:8]
    context = None
    if args.resume_evidence:
        context = Path((args.resume_evidence / "context-path.txt").read_text().strip())
        bound = json.loads(context.read_bytes())
        expect(
            Path(bound["project_root"]).resolve() == root,
            "recovery belongs to another repository",
        )
        project = root / bound["project_path"]
        expect(
            project.parent == root / "outputs"
            and project.name.startswith("hospital-entry-fixture-"),
            "recovery must use an isolated hospital project",
        )
        prior = json.loads((args.resume_evidence / "original-source.json").read_bytes())
        source = Path(prior["path"])
        expect(
            sha(source) == prior["sha256"], "original source changed before recovery"
        )
        config = json.loads((project / "floor-08-trial.json").read_bytes())
        if args.expected_source_total is None:
            args.expected_source_total = prior.get("expected_component_total")
    else:
        project = root / "outputs" / ("hospital-entry-fixture-" + token)
        project.mkdir(parents=True)
        import yaml

        data = yaml.safe_load((original / "project.yaml").read_text(encoding="utf-8"))
        source = original / data["sources"][0]["path"]
        shutil.copy2(source, project / "drawing.dxf")
        (project / "project.yaml").write_text(
            "sources:\n  - path: drawing.dxf\n", encoding="utf-8"
        )
        config = json.loads((original / "floor-08-trial.json").read_bytes())
        config["source"] = str((project / "drawing.dxf").relative_to(root))
        (project / "floor-08-trial.json").write_text(
            json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        for name in ["floor-08-enclosure.json", "floor-08-reviewed-assemblies.json"]:
            shutil.copy2(original / name, project / name)
        if args.component_source_context:
            profile_path = args.component_source_context.resolve()
            expect(
                profile_path.is_relative_to(root),
                "component source profile must be in modeler repository",
            )
            profile = json.loads(profile_path.read_bytes())
            expect(
                len(profile["sources"]) == 1,
                "one original component source is required",
            )
            spec = profile["sources"][0]
            expect(
                (root / spec["path"]).resolve().is_relative_to(root),
                "component source outside repository",
            )
            expect(
                sha(root / spec["path"]) == sha(source),
                "component policy belongs to different CAD",
            )
            for key in ("root_insert_handle", "origin_dxf", "crop_dxf"):
                expect(
                    spec[key] == config[key],
                    "component policy floor binding mismatch: " + key,
                )
            (project / "project.yaml").write_text(
                yaml.safe_dump(
                    {
                        "sources": [{"path": "drawing.dxf"}],
                        "component_sources": {
                            "layers": spec["component_layers"],
                            "rules": spec.get("rules", []),
                        },
                    },
                    allow_unicode=True,
                ),
                encoding="utf-8",
            )
    baseline = {p.name: sha(p) for p in project.iterdir()}
    repo = Path(__file__).resolve().parents[2]
    evidence = repo / "target" / ("hospital-project-entry-" + token)
    evidence.mkdir()
    (evidence / "original-source.json").write_text(
        json.dumps(
            {
                "path": str(source),
                "sha256": sha(source),
                "label_handle": config["label_handle"],
                "test_decision": "trial CAD structure only; semantics and human review not_run",
                "expected_component_total": args.expected_source_total,
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(root),
            "--domain-context" if context else "--domain-project",
            str(context) if context else str(project.relative_to(root)),
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

    def wait(label, predicate, timeout=None):
        deadline = time.monotonic() + (timeout or args.timeout)
        while time.monotonic() < deadline:
            try:
                if predicate():
                    return
            except (OSError, KeyError, json.JSONDecodeError):
                pass
            if tui.exited() is not None:
                raise AssertionError("TUI exited during " + label)
            if " · Failed · " in tui.screen():
                raise AssertionError("Agent turn failed during " + label)
            if (
                label == "hospital-applied"
                and " · Completed · " in tui.screen()
                and any(
                    text in tui.screen()
                    for text in (
                        "批准结果超过大小上限",
                        "批准进程中断或超时",
                        "批准未通过版本核对",
                    )
                )
            ):
                raise AssertionError(
                    "Core approval response failed; inspect the original durable grant"
                )
            if label == "hospital-structure" and " · Completed · " in tui.screen():
                raise AssertionError(
                    "hospital preparation ended without a scope; inspect the retained Agent diagnostic"
                )
            time.sleep(0.3)
        (evidence / (label + "-timeout.txt")).write_text(
            tui.console.stable(), encoding="utf-8"
        )
        raise AssertionError("timeout: " + label)

    def state():
        path = context.parent / "project/scopes.json"
        return json.loads(path.read_bytes()) if path.exists() else {"scopes": {}}

    def records():
        scope = next(iter(state()["scopes"].values()))
        child = json.loads((root / scope["context_path"]).read_bytes())
        directory = root / "outputs/domain" / child["run_id"] / "changes"
        rows = [
            json.loads(path.read_bytes())
            for path in directory.glob("*.json")
            if re.fullmatch("[a-f0-9]{32}", path.stem)
        ]
        return scope, child, directory, rows

    def finished():
        tui.wait(
            "hospital turn terminal",
            lambda screen: " · Completed · " in screen or " · Failed · " in screen,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "hospital Agent turn failed")

    try:
        tui.wait(
            "project ready",
            lambda screen: "建模 · " in screen and "Ready" in screen,
            120,
        )
        if context is None:
            for path in root.glob("outputs/domain/project-*/context.json"):
                value = json.loads(path.read_bytes())
                if Path(value.get("project_path", "")) == project.relative_to(root):
                    context = path
                    break
        expect(context is not None, "hospital context missing")
        (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
        tui.keys(CTRL_U)
        prompt = (
            "本轮是用户授权编码Agent代做决策的医院隔离项目测试。加载building-modeling技能。"
            "从inspect_project、select_project_target target=八层进入。若有重复图纸，只采用label_handle="
            + config["label_handle"]
            + "的那份（明确测试选择）。prepare_project_floor准备对应楼层的确定性结构，"
            "保留复制配置中的assumed尺寸。仅准备并展示具体版本，不授权、不应用、不构建。"
            "若blocked或工具失败，报告真实诊断，不绕过验证，不改原DXF，不调用shell或旧workflow。"
            "本轮不要求解释全部构件，医院业务判断和人工复核仍not_run；收到平台具体授权后才应用、构建与独立验证。"
        )
        if args.resume_evidence:
            stored = json.loads(
                (context.parent / "project/selection.json").read_bytes()
            )
            prior_scope = next(iter(state()["scopes"].values()))
            prompt = (
                "用户授权编码Agent代做决策的医院隔离测试续接。此前prepare已完成但MCP返回超时；"
                "只读恢复原候选，禁止再prepare_project_floor或初始化。先inspect_project，再select_project_target "
                "target=八层恢复完全相同的原选择；然后inspect_project_floor floor_key="
                + prior_scope["floor_key"]
                + "、selection_id="
                + stored["selection_id"]
                + "，再inspect_change proposal_id="
                + prior_scope["bootstrap_proposal_id"]
                + "。展示同一具体版本，等待TUI批准；本轮不授权、不应用、不构建，不shell。"
                "后续收到平台批准才沿原身份应用并构建验证，医院构件业务判断和人工复核仍not_run。"
            )
        if args.expected_source_total is not None:
            prompt += (
                "批准后的构建／独立验证及实例分页完成后，再调用scan_source_groups并读取完整分组，"
                "核对当前明确配置图层的来源总数应为"
                + str(args.expected_source_total)
                + "。这一数量核对不能当构件业务已完成。所有read_fact_view limit=20。"
                "prepare若Request timed out，按技能先inspect_project.recovery_index再inspect_project_floor"
                "与inspect_change恢复原候选，禁止盲目重做初始化。"
            )
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("hospital turn running", lambda screen: " · Running · " in screen, 60)
        wait("hospital-structure", lambda: bool(state()["scopes"]))
        finished()
        scope, child, directory, rows = records()
        proposal = next(
            row for row in rows if row["proposal_id"] == scope["bootstrap_proposal_id"]
        )
        (evidence / "structure.json").write_text(
            json.dumps(proposal, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        expect(
            proposal["state"] == "prepared",
            "hospital structure blocked: "
            + json.dumps(proposal["issues"], ensure_ascii=False)[:1000],
        )
        expect(
            proposal.get("structural_source", {}).get("contract") == "cad-structure/v1",
            "structure proof missing",
        )
        grants = list(directory.glob("authorization-*.json"))
        grant = None
        if args.continue_approved:
            expect(
                len(grants) == 1, "recovery requires exactly one original Core grant"
            )
            decision = json.loads(
                (args.resume_evidence / "test-decision.json").read_bytes()
            )
            grant = json.loads(grants[0].read_bytes())
            for key in (
                "proposal_id",
                "revision",
                "prepare_token",
                "base_sha256",
                "candidate_sha256",
            ):
                expect(
                    grant[key] == proposal[key] == decision[key],
                    "original grant binding mismatch: " + key,
                )
            expect(
                grant["actor"] == "native-agent-tui:user",
                "recovery grant is not from Core",
            )
            expect(grant["scope"] == proposal["scope"], "original grant scope mismatch")
            expect(
                Path(grant["scene_path"]).resolve()
                == (root / child["scene_path"]).resolve(),
                "grant scene mismatch",
            )
            expect(
                sha(root / child["scene_path"]) == grant["base_sha256"],
                "original candidate already changed; inspect first",
            )
            (evidence / "recovered-grant.json").write_text(
                json.dumps(grant, indent=2), encoding="utf-8"
            )
        else:
            expect(not grants, "grant before test decision")
        tui.wait(
            "structure facts synchronized",
            lambda screen: (
                "领域事实同步失败" not in screen
                and "正在同步完整领域事实" not in screen
            ),
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(100):
            screen = tui.console.stable()
            if "方案 " + proposal["proposal_id"] in screen and "修订 1" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("concrete hospital proposal not visible")
        (evidence / "before-approval.txt").write_text(screen, encoding="utf-8")
        (evidence / "test-decision.json").write_text(
            json.dumps(
                {
                    "actor": "coding_agent_as_user_authorized_test_proxy",
                    "real_project_human_confirmation": False,
                    "proposal_id": proposal["proposal_id"],
                    "revision": 1,
                    "prepare_token": proposal["prepare_token"],
                    "base_sha256": proposal["base_sha256"],
                    "candidate_sha256": proposal["candidate_sha256"],
                    "scope": "deterministic structure only",
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        report.step(
            "hospital project prepares exact source-sealed CAD structure in ordinary TUI",
            lambda: None,
            tui,
        )
        if grant is None:
            tui.keys(CTRL_Y)
        else:
            binding = {
                key: proposal[key]
                for key in (
                    "proposal_id",
                    "revision",
                    "prepare_token",
                    "base_sha256",
                    "candidate_sha256",
                )
            }
            binding.update(run_id=bound["run_id"], context_sha256=sha(context))
            arguments = {
                key: grant[key]
                for key in (
                    "proposal_id",
                    "revision",
                    "prepare_token",
                    "authorization_id",
                )
            }
            arguments["expected_sha256"] = grant["base_sha256"]
            payload = {
                "user_action": "resume_existing_concrete_approval",
                "proposal": binding,
                "authorization_id": grant["authorization_id"],
                "apply_change_arguments": arguments,
            }
            tui.keys(CTRL_U)
            tui.type(
                "编码Agent已核对前次TUI Ctrl+Y写入的唯一原授权。前次只是授权回传超过消息上限，未发生应用。"
                "这是同一具体候选的原批准续接；不要再授权或prepare。使用以下原grant和绑定，由你沿稳定operation_id应用、"
                "构建并独立验证；完整来源对账按分页保留未决项，不把结构试验当医院语义完成。\n"
                + json.dumps(payload, ensure_ascii=False)
            )
            tui.keys(ENTER)
            tui.wait(
                "original approval continuation running",
                lambda screen: " · Running · " in screen,
                60,
            )
        wait(
            "hospital-applied",
            lambda: any(row["state"] == "applied" for row in records()[3]),
        )
        wait(
            "hospital-built",
            lambda: any(
                json.loads(path.read_bytes()).get("state") == "built"
                for path in directory.glob("artifacts/*/build.json")
            ),
        )
        finished()
        expect(
            sha(root / child["scene_path"]) == proposal["candidate_sha256"],
            "hospital authority differs from approved candidate",
        )
        expect(
            len(list(directory.glob("authorization-*.json"))) == 1,
            "hospital grant count",
        )
        for path in directory.glob("artifacts/*/build.json"):
            job = json.loads(path.read_bytes())
            if job["state"] != "built":
                continue
            output = Path(job["output"])
            validation = json.loads(output.with_suffix(".validation.json").read_bytes())
            expect(validation["valid"], "hospital Blender validation failed")
            for phase in ["built", "rebuilt", "glb_roundtrip"]:
                ids = [obj["id"] for obj in validation[phase]["objects"]]
                expect(
                    ids and len(ids) == len(set(ids)),
                    "hospital mesh IDs missing or duplicate",
                )
        expect(
            {p.name: sha(p) for p in project.iterdir()} == baseline,
            "hospital copied source/config changed",
        )
        expect(
            sha(source) == baseline["drawing.dxf"], "original hospital drawing changed"
        )
        if args.expected_source_total is not None:
            groups = []
            for path in (context.parent / "project/facts").glob("*.json"):
                fact = json.loads(path.read_bytes())
                if fact.get("kind") == "source_groups":
                    groups.append(fact)
            expect(groups, "full component source group fact missing")
            latest = max(groups, key=lambda fact: fact["fact_sequence"])
            expect(
                latest["total_objects"] == args.expected_source_total,
                "configured component source total differs",
            )
            expect(
                sum(group["instance_count"] for group in latest["groups"])
                == args.expected_source_total,
                "full group coverage differs from configured total",
            )
            (evidence / "component-source-groups.json").write_text(
                json.dumps(latest, ensure_ascii=False, indent=2), encoding="utf-8"
            )
            inventory = verify_component_inventory(
                root, context, child, proposal["candidate_sha256"], evidence
            )
            expect(
                inventory["independent_instances"] == args.expected_source_total,
                "raw CAD inventory differs from configured expected source count",
            )
        report.step(
            "hospital structure concretely approved, applied and independently built with sources unchanged",
            lambda: None,
            tui,
        )
        (evidence / "completed-screen.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        report.step(
            "hospital TUI exits cleanly",
            lambda: expect(tui.quit() == 0, "nonzero exit"),
            tui,
        )
    except Exception as error:
        report.step(
            "hospital project entry",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    main()
