"""Run a real TUI dsh Agent from project target to approved CAD floors and real Blender.

The coding Agent makes test decisions via prompts and Ctrl+Y. Only the TUI Agent
executes tools. Durable observers never prepare, authorize, apply or build.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path

sys.dont_write_bytecode = True


def seed(project, connections=False):
    import ezdxf

    project.mkdir(parents=True)
    source = project / "drawing.dxf"
    document = ezdxf.new()
    document.header["$INSUNITS"] = 4
    block = document.blocks.new("floors")
    labels = []
    for index, title in enumerate(["八层平面", "九层平面", "九层平面"]):
        x = index * 250000
        labels.append(block.add_text(title, dxfattribs={"insert": (x, 0)}))
        block.add_text(
            f"{24.0 if index == 0 else 27.2:.3f}",
            dxfattribs={"insert": (x + 300, 300), "layer": "FL-AXIS"},
        )
        for cx, cy in [(1000, 1000), (7000, 1000), (1000, 5000), (7000, 5000)]:
            block.add_lwpolyline(
                [
                    (x + cx - 120, cy - 120),
                    (x + cx + 120, cy - 120),
                    (x + cx + 120, cy + 120),
                    (x + cx - 120, cy + 120),
                ],
                close=True,
                dxfattribs={"layer": "COLUMN"},
            )
        for a, b in [
            ((0, 0), (8000, 0)),
            ((0, 200), (8000, 200)),
            ((0, 5800), (8000, 5800)),
            ((0, 6000), (8000, 6000)),
            ((0, 0), (0, 6000)),
            ((200, 0), (200, 6000)),
            ((7800, 0), (7800, 6000)),
            ((8000, 0), (8000, 6000)),
        ]:
            block.add_line(
                (a[0] + x, a[1]), (b[0] + x, b[1]), dxfattribs={"layer": "WALL"}
            )
    axis = document.blocks.new("_AXISO")
    axis.add_attdef("A", insert=(0, 0), dxfattribs={"height": 100})
    for index in range(3):
        x = index * 250000
        for name, point in [
            ("1", (x + 1000, -1000)),
            ("1", (x + 1000, 5000)),
            ("2", (x + 7000, -1000)),
            ("2", (x + 7000, 5000)),
            ("A", (x - 1000, 1000)),
            ("A", (x + 9000, 1000)),
            ("B", (x - 1000, 5000)),
            ("B", (x + 9000, 5000)),
        ]:
            ref = block.add_blockref("_AXISO", point)
            ref.add_auto_attribs({"A": name})
    holes = {}
    if connections:
        stair = document.blocks.new("TEST_STAIR")
        stair.add_lwpolyline([(0, 0), (3000, 0), (3000, 1200), (0, 1200)], close=True)
        block.add_blockref("TEST_STAIR", (2000, 2000), dxfattribs={"layer": "VERTICAL"})
        for index in [1, 2]:
            x = index * 250000
            hole = block.add_lwpolyline(
                [
                    (x + 2000, 2000),
                    (x + 5000, 2000),
                    (x + 5000, 3200),
                    (x + 2000, 3200),
                ],
                close=True,
                dxfattribs={"layer": "SLAB-VOID"},
            )
            holes[index] = hole.dxf.handle
    root = document.modelspace().add_blockref("floors", (0, 0))
    document.saveas(source)
    (project / "project.yaml").write_text(
        "sources:\n  - path: drawing.dxf\n", encoding="utf-8"
    )
    if connections:
        (project / "project.yaml").write_text(
            "sources:\n  - path: drawing.dxf\ncomponent_sources:\n  layers: [VERTICAL]\n"
            "  rules:\n    - block: TEST_STAIR\n      category: stair\n"
            "      reason: authored source-bound staircase for isolated acceptance\n",
            encoding="utf-8",
        )
    for index, label in enumerate(labels):
        x = index * 250000
        key = ["08", "09-a", "09-b"][index]
        config = {
            "source": str(source),
            "root_insert_handle": root.dxf.handle,
            "label_handle": label.dxf.handle,
            "label_text": label.dxf.text,
            "floor_level": 8 if index == 0 else 9,
            "crop_dxf": [x, 0, x + 8000, 6000],
            "origin_dxf": [x, 0],
            "wall_height": 3,
            "confirmed_story_height": 3.2,
            "preview_slab_thickness": 0.2,
            "preview_slab_boundary": [
                {"x": 0, "y": 0},
                {"x": 8, "y": 0},
                {"x": 8, "y": 6},
                {"x": 0, "y": 6},
            ],
            "preview_slab_source_handles": [label.dxf.handle],
        }
        if connections:
            config["observation_layers"] = ["COLUMN", "SLAB-VOID"]
            config["slab_opening_source_ids"] = (
                ["dxf." + root.dxf.handle + "." + holes[index]]
                if index in holes
                else []
            )
        (project / f"floor-{key}-trial.json").write_text(
            json.dumps(config, ensure_ascii=False), encoding="utf-8"
        )
        (project / f"floor-{key}-enclosure.json").write_text(
            json.dumps(
                {
                    "dimension_source": "test delegate authorizes schematic dimensions, assumed",
                    "window_sill": 0.9,
                    "window_height": 1.2,
                    "door_height": 2.1,
                    "bands": [
                        {
                            "name": "test-floor",
                            "bounds": [-1, -1, 9, 7],
                            "reason": "test authored boundary",
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
    (project / "original-scene.json").write_bytes(b'{"baseline":"never written"}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary", type=Path, default=Path("target/release/native-agent-tui.exe")
    )
    parser.add_argument("--modeler-repo", type=Path, required=True)
    parser.add_argument("--model", default="hi/gpt-6.1-sol")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument(
        "--connections",
        action="store_true",
        help="seed source stair and explicit upper CAD slab hole",
    )
    args = parser.parse_args()
    newest_source = max(
        path.stat().st_mtime
        for path in Path(__file__).resolve().parents[2].joinpath("src").rglob("*.rs")
    )
    if args.binary.stat().st_mtime < newest_source:
        parser.error(
            "release binary is older than Rust sources; rebuild it before live verification"
        )
    from domain_project_images import admitted_images
    from dsh_tui_check import CTRL_HOME, CTRL_U, CTRL_Y, ENTER, Report, Tui, expect

    repo = Path(__file__).resolve().parents[2]
    modeler = args.modeler_repo.resolve()
    token = uuid.uuid4().hex[:8]
    evidence = repo / "target" / ("project-live-" + token)
    evidence.mkdir()
    project = modeler / "outputs" / ("project-live-fixture-" + token)
    python = modeler / ".venv/Scripts/python.exe"
    subprocess.run(
        [str(python), str(Path(__file__).resolve()), "--seed", str(project)]
        + (["--connections"] if args.connections else []),
        check=True,
        cwd=modeler,
    )
    baseline = {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest()
        for path in project.iterdir()
    }
    tui = Tui(
        [
            str(args.binary.resolve()),
            "--cwd",
            str(modeler),
            "--domain-project",
            str(project.relative_to(modeler)),
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
    report = Report(evidence)
    context = None

    def find_context():
        for path in modeler.glob("outputs/domain/project-*/context.json"):
            value = json.loads(path.read_bytes())
            if Path(value.get("project_path", "")) == project.relative_to(modeler):
                return path
        return None

    def state():
        path = context.parent / "project/scopes.json"
        return json.loads(path.read_bytes()) if path.exists() else {"scopes": {}}

    def child(key):
        registry = state()
        record = registry["scopes"][key]
        value = json.loads((modeler / record["context_path"]).read_bytes())
        directory = modeler / "outputs/domain" / value["run_id"] / "changes"
        return record, value, directory

    def rows(key):
        _, _, directory = child(key)
        return [
            json.loads(path.read_bytes())
            for path in directory.glob("*.json")
            if re.fullmatch("[a-f0-9]{32}", path.stem)
        ]

    def wait(what, predicate, timeout=None):
        until = time.monotonic() + (timeout or args.timeout)
        while time.monotonic() < until:
            try:
                if predicate():
                    return
            except (FileNotFoundError, KeyError, json.JSONDecodeError):
                pass
            if tui.exited() is not None:
                raise AssertionError("TUI exited during " + what)
            time.sleep(0.25)
        (evidence / (what + "-timeout.txt")).write_text(tui.screen(), encoding="utf-8")
        raise AssertionError("timeout: " + what)

    def send(prompt):
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("new domain turn running", lambda s: " · Running · " in s, 60)

    def business_state():
        paths = [context, context.parent / "project/scopes.json"]
        for key in state()["scopes"]:
            _, bound, directory = child(key)
            paths.append(modeler / bound["scene_path"])
            paths.extend(
                path
                for path in directory.glob("*.json")
                if re.fullmatch(
                    r"[a-f0-9]{32}|authorization-.+|operation-.+", path.stem
                )
            )
            paths.extend(directory.glob("artifacts/*/build.json"))
        return {
            str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths
        }

    def finished():
        tui.wait(
            "confirmed idle domain turn",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        time.sleep(0.4)
        if " · Failed · " not in tui.screen():
            return
        # A confirmed build is not replayed because its model response was lost.
        # Only the observed upstream transport failure permits a new read-only task.
        tui.keys("\x1b[17~")
        failure = tui.console.stable()
        expect(
            "upstream_http2_stream_error" in failure,
            "Agent turn failed for an unhandled reason",
        )
        tui.keys("\x1b[17~")
        identifier = uuid.uuid4().hex[:8]
        (evidence / ("provider-failure-" + identifier + ".txt")).write_text(
            failure, encoding="utf-8"
        )
        before = business_state()
        send(
            "上轮模型回复因upstream_http2_stream_error中断。只读续接当前楼层：describe_capabilities，"
            "inspect_change读取原提案，按已有索引inspect_operation／inspect_artifact／validate_artifact，"
            "完成来源对账并简洁报告已有事实。若已有复核图片，读取同一build_id的top和perspective后报告实际观察。"
            "尚未应用的方案只检查并展示，等待原具体审批。不得调用prepare、authorize、apply_change、"
            "build_scene或preview_candidate；不得改变原提案、授权、操作、构建及权威场景，不生成新业务身份。"
        )
        tui.wait(
            "read-only response recovery",
            lambda s: " · Completed · " in s or " · Failed · " in s,
            args.timeout,
        )
        expect(" · Failed · " not in tui.screen(), "read-only response recovery failed")
        expect(
            business_state() == before,
            "response recovery modified or allocated business state",
        )
        (evidence / ("response-recovery-" + identifier + ".json")).write_text(
            json.dumps(
                {
                    "reason": "upstream_http2_stream_error",
                    "action": "new_read_only_task",
                    "business_state_unchanged": True,
                }
            ),
            encoding="utf-8",
        )

    def approve(key, proposal):
        if "领域事实同步失败" in tui.screen():
            send(
                "重新调用inspect_change，proposal_id="
                + proposal["proposal_id"]
                + "，只核对当前方案以恢复平台事实同步；不要批准、应用或构建，不需要再次分页读取。"
            )
            finished()
        tui.wait(
            "private fact synchronization restored",
            lambda s: "领域事实同步失败" not in s and "正在同步完整领域事实" not in s,
            60,
        )
        tui.keys(CTRL_HOME)
        for _ in range(64):
            screen = tui.console.stable()
            if (
                f"方案 {proposal['proposal_id']}" in screen
                and f"修订 {proposal['revision']}" in screen
            ):
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError(
                "exact structure not visible after paging the business view"
            )
        (evidence / (key + "-before-approval.txt")).write_text(screen, encoding="utf-8")
        _, _, directory = child(key)
        expect(
            not list(directory.glob("authorization-*.json")),
            "unexpected pre-test grant",
        )
        (evidence / (key + "-test-decision.json")).write_text(
            json.dumps(
                {
                    "actor": "coding-agent-test-delegate",
                    "user_authorized": True,
                    "action": "approve_concrete_version",
                    "proposal_id": proposal["proposal_id"],
                    "revision": proposal["revision"],
                    "prepare_token": proposal["prepare_token"],
                    "base_sha256": proposal["base_sha256"],
                    "candidate_sha256": proposal["candidate_sha256"],
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        tui.keys(CTRL_Y)

    try:
        tui.wait("project ready", lambda s: "建模 · " in s and "Ready" in s, 120)
        context = find_context()
        expect(context is not None, "bound project context missing")
        (evidence / "context-path.txt").write_text(str(context), encoding="utf-8")
        send(
            "本轮为编码Agent代做决策的隔离测试。加载building-modeling技能。建模整栋：先inspect_project，"
            "select_project_target target=整栋。重复九层明确采用f09.b（这是已授权测试选择），保留唯一八层。"
            "读取完整候选依据，choices只含f09.b，随后prepare_project_floor f08，selection_id用当前返回值。"
            "允许测试配置的示意尺寸保持assumed。只准备具体结构候选，展示方案版本；此轮不要批准、应用或构建。"
            "后续收到平台具体授权后由你调用apply_change、build_scene、inspect_artifact、validate_artifact，"
            "不调用shell或旧Python workflow。"
        )
        wait("first-structure", lambda: "f08" in state()["scopes"])
        finished()
        first = rows("f08")[-1]
        expect(first["state"] == "prepared", "first structure blocked")
        approve("f08", first)
        wait(
            "first-applied",
            lambda: any(row["state"] == "applied" for row in rows("f08")),
        )
        wait(
            "first-built",
            lambda: any(
                json.loads(path.read_bytes()).get("state") == "built"
                for path in child("f08")[2].glob("artifacts/*/build.json")
            ),
        )
        finished()
        report.step(
            "real Agent selected building, prepared and concretely approved first CAD floor",
            lambda: None,
            tui,
        )
        send(
            "第一层已应用且构建。继续目标中的f09.b：prepare_project_floor使用原selection_id、floor_key=f09.b；"
            "只准备第二层具体结构方案并展示。不要重做第一层、不要自动授权，也不要构建未经批准的第二层。"
        )
        wait("second-structure", lambda: "f09.b" in state()["scopes"])
        finished()
        second = rows("f09.b")[-1]
        expect(second["state"] == "prepared", "second structure blocked")
        approve("f09.b", second)
        wait(
            "second-applied",
            lambda: any(row["state"] == "applied" for row in rows("f09.b")),
        )
        wait(
            "second-built",
            lambda: any(
                json.loads(path.read_bytes()).get("state") == "built"
                for path in child("f09.b")[2].glob("artifacts/*/build.json")
            ),
        )
        finished()
        report.step(
            "same TUI Agent prepares and executes second concretely approved floor",
            lambda: None,
            tui,
        )
        send(
            "复核当前f09.b实际已构建产物：用已有build_id调用validate_artifact，再render_artifact_review取得top和perspective，"
            "按每份回执调用read_artifact_review_image读取实际PNG，然后报告你实际看见的轮廓、墙与楼板、未覆盖细节。"
            "图像生成或送达不等于复核通过，人工复核保持not_run。不要创建新提案、授权、应用或构建。"
        )
        finished()
        tui.wait(
            "review facts synchronized",
            lambda screen: (
                "领域事实同步失败" not in screen
                and "正在同步完整领域事实" not in screen
            ),
            60,
        )
        review_facts = []
        review_receipts = []
        for path in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(path.read_bytes())
            image = fact.get("artifact", {}).get("review_image", {})
            if (
                fact.get("project_scope", {}).get("floor_key") == "f09.b"
                and image.get("delivery") == "attached_png"
            ):
                review_facts.append(fact)
                review_receipts.append(path.stem)
        expect(
            {
                fact["artifact"]["review_image"]["request"]["view"]
                for fact in review_facts
            }
            >= {"top", "perspective"},
            "actual PNG delivery for both views missing",
        )
        admitted = admitted_images(evidence / "journal", review_receipts)
        (evidence / "model-image-admission.json").write_text(
            json.dumps(admitted, indent=2), encoding="utf-8"
        )
        (evidence / "review-facts.json").write_text(
            json.dumps(review_facts, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / "agent-review-screen.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        selection_value = json.loads(
            (context.parent / "project/selection.json").read_bytes()
        )
        send(
            "现在调用assemble_project_building，selection_id使用当前项目选择返回值 "
            + selection_value["selection_id"]
            + "，build_id=building-assembled-01。只合并已应用的f08和f09.b，核对每层授权、来源、轴网和绝对标高；"
            "这是异步任务，用inspect_project_building等待原build_id直到终态，再validate_project_building。"
            "返回楼栋Scene、assembly证据、GLB哈希和未决项。用render_project_building_review perspective"
            "并read_project_building_review_image读取实际楼栋PNG，报告实际观察与未覆盖的连接检查。"
            "同时调用当前楼层audit_drawing并按next_offset完整读取候选，单独报告候选与AI判断限制。"
            "不要修改任何楼层权威场景，不创建新的楼层提案或授权，不重复启动楼栋构建。"
        )
        finished()
        building_record = (
            context.parent / "project/building/building-assembled-01/assembly.json"
        )
        expect(building_record.is_file(), "building assembly evidence missing")
        building = json.loads(building_record.read_bytes())
        expect(
            [row["floor_key"] for row in building["floors"]] == ["f08", "f09.b"],
            "building floor identity mismatch",
        )
        expect(
            (
                context.parent / "project/building/building-assembled-01/model.glb"
            ).is_file(),
            "building GLB missing",
        )
        building_directory = context.parent / "project/building/building-assembled-01"
        built = json.loads((building_directory / "build.json").read_bytes())
        expect(built["state"] == "built", "building not terminal-success")
        expect(
            built["checks"]
            == {key: "passed" for key in ["built", "rebuilt", "glb_roundtrip"]},
            "building checks missing",
        )
        expect(
            built["connection_check"] == "not_run",
            "building connection review falsely marked complete",
        )
        building_images = []
        building_receipts = []
        audits = []
        for path in (context.parent / "project/facts").glob("*.json"):
            fact = json.loads(path.read_bytes())
            record = fact.get("project_building", {})
            if (
                record.get("build_id") == "building-assembled-01"
                and record.get("review_image", {}).get("delivery") == "attached_png"
            ):
                building_images.append(fact)
                building_receipts.append(path.stem)
            if fact.get("kind") == "drawing_audit":
                audits.append(fact)
        expect(building_images, "building image delivery missing")
        expect(audits, "current floor drawing audit missing")
        building_admission = admitted_images(evidence / "journal", building_receipts)
        (evidence / "building-image-admission.json").write_text(
            json.dumps(building_admission, indent=2), encoding="utf-8"
        )
        (evidence / "building-review-facts.json").write_text(
            json.dumps(building_images, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        (evidence / "drawing-audit-facts.json").write_text(
            json.dumps(audits, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        tui.wait(
            "building facts synchronized",
            lambda screen: (
                "领域事实同步失败" not in screen
                and "正在同步完整领域事实" not in screen
            ),
            60,
        )
        # Read the actual Core projection by scrolling, then retain its rendered state.
        tui.keys(CTRL_HOME)
        for _ in range(64):
            screen = tui.console.stable()
            if "楼栋 building-assembled-01" in screen and "built" in screen:
                break
            tui.keys("\x1b[6~")
        else:
            raise AssertionError("current building fact not displayed in F6")
        (evidence / "building-core-screen.txt").write_text(screen, encoding="utf-8")
        report.step(
            "same TUI Agent assembles selected applied floors with absolute placement evidence",
            lambda: None,
            tui,
        )
        report.step(
            "same TUI Agent reads build-bound top and perspective PNGs and reports visual limits",
            lambda: None,
            tui,
        )
        # Independent observers verify exact applied candidate hashes and actual mesh phases.
        for key in ["f08", "f09.b"]:
            record, value, directory = child(key)
            proposal = next(
                row
                for row in rows(key)
                if row["proposal_id"] == record["bootstrap_proposal_id"]
            )
            expect(
                hashlib.sha256((modeler / value["scene_path"]).read_bytes()).hexdigest()
                == proposal["candidate_sha256"],
                "authority hash mismatch",
            )
            grants = list(directory.glob("authorization-*.json"))
            expect(len(grants) == 1, "wrong grant count")
            artifacts = [
                json.loads(path.read_bytes())
                for path in directory.glob("artifacts/*/build.json")
            ]
            for artifact in artifacts:
                if artifact["state"] != "built":
                    continue
                validation = json.loads(
                    Path(artifact["output"])
                    .with_suffix(".validation.json")
                    .read_bytes()
                )
                expect(validation["valid"], "invalid Blender result")
                for phase in ["built", "rebuilt", "glb_roundtrip"]:
                    ids = [obj["id"] for obj in validation[phase]["objects"]]
                    expect(
                        ids and len(ids) == len(set(ids)),
                        "missing/duplicate mesh identity",
                    )
            (evidence / (key + "-scope.json")).write_text(
                json.dumps(record, indent=2), encoding="utf-8"
            )
        expect(
            {
                path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                for path in project.iterdir()
            }
            == baseline,
            "project sources changed",
        )
        report.step(
            "independent hashes, grants, real Blender/rebuild/GLB and untouched sources",
            lambda: None,
            tui,
        )
        tui.keys("\x1b[17~")
        (evidence / "agent-observations.txt").write_text(
            tui.console.stable(), encoding="utf-8"
        )
        report.step(
            "TUI exits cleanly", lambda: expect(tui.quit() == 0, "nonzero exit"), tui
        )
    except Exception as error:
        report.step(
            "live project workflow",
            lambda error=error: (_ for _ in ()).throw(error),
            tui,
        )
        raise
    finally:
        (evidence / "final-screen.txt").write_text(tui.screen(), encoding="utf-8")
        tui.close()
        print("Evidence: " + str(evidence), flush=True)


if __name__ == "__main__":
    if "--seed" in sys.argv:
        seed(Path(sys.argv[sys.argv.index("--seed") + 1]), "--connections" in sys.argv)
    else:
        main()
