"""Keyboard-only platform image delivery and visual-analysis acceptance.

This proves image delivery and an actual completed platform analysis turn. It does
not grade visual correctness or turn a schematic extrusion into detailed quality.
"""

import hashlib
import json
import os
import re
from pathlib import Path


def run_hospital_visual_check(report, tui, send, finished, fact, context_path, evidence):
    from dsh_tui_check import CTRL_HOME, ENTER, F6, expect

    context = json.loads(context_path.read_bytes())
    root = Path(context["project_root"])
    scene = root / context["scene_path"]
    directory = root / "outputs/domain" / context["run_id"] / "changes"
    build_id = os.environ["MODELING_VISUAL_BUILD_ID"]
    build = json.loads((directory / "artifacts" / build_id / "build.json").read_bytes())
    baseline = scene.read_bytes()
    expect(hashlib.sha256(baseline).hexdigest() == build["input_sha256"], "review build is not current")

    def records(pattern):
        return [json.loads(p.read_bytes()) for p in sorted(directory.glob(pattern))]

    grants, operations = records("authorization-*.json"), records("operation-*.json")
    source_hashes = {s["path"]: hashlib.sha256((root / s["path"]).read_bytes()).hexdigest()
                     for s in context["sources"]}
    prefix = "visual-" + evidence.name[-8:]
    expected = {
        prefix + "-top": ("top", None),
        prefix + "-perspective": ("perspective", None),
        prefix + "-local": ("top", [8.0, 2.0, 22.0, 10.0]),
    }
    lineage = {"build_id": build_id, "proposal_id": build["proposal_id"],
               "scene_sha256": hashlib.sha256(baseline).hexdigest(),
               "context_sha256": hashlib.sha256(context_path.read_bytes()).hexdigest(),
               "source_hashes": source_hashes, "initial_authorizations": grants,
               "initial_operations": operations, "artifact_hashes": build["artifact_hashes"],
               "delivery_requests": expected}
    (evidence / "visual-lineage.json").write_text(json.dumps(lineage, indent=2), encoding="utf-8")

    def review():
        tui.wait("visual domain ready", lambda s: "建模" in s and "Initializing" not in s, 180)
        instructions = [
            {"view": view, "focus_m": focus, "read_request_id": request}
            for request, (view, focus) in expected.items()
        ]
        tui.type(
            f"对医院八层当前已构建产物 {build_id} 做实际看图分析。提案{build['proposal_id']}。"
            "先describe_capabilities、inspect_change和validate_artifact。仅用领域工具，"
            "不准备、不批准、不应用、不重新构建，不运行shell或额外模型循环。"
            "调用render_artifact_review生成以下三幅图；每幅再调用read_artifact_review_image，"
            "使用实际回执image_id、image_sha256，并将request_id绑定下列read_request_id："
            + json.dumps(instructions, ensure_ascii=False)
            + "。必须读取工具返回的实际PNG后分析，不能只根据台账或文件名描述。"
            "给出视图中能看到的构件形态、缺失细节、遮挡或无法判断的区域；"
            "区分几何构建检查、AI视觉观察和人工复核，不把示意体说成精细家具或合规。"
            "最后scan_source_groups展示当前数量，并以实际图中观察到的问题结束回复。"
            "保留视觉质量未分级和人工not_run。不要仅重复总数。"
        )
        tui.keys(ENTER, 0.5)
        if "目标 / 自然语言反馈" not in tui.screen():
            tui.keys(F6, 0.5)
        finished()
        expect(scene.read_bytes() == baseline, "visual review wrote Scene IR")
        expect(records("authorization-*.json") == grants and records("operation-*.json") == operations,
               "visual review changed writer authority")
        delivered = {}
        for path in (directory / "facts").glob("*.json"):
            raw = path.read_bytes()
            value = json.loads(raw)
            image = value.get("artifact", {}).get("review_image", {})
            request = image.get("delivery_request_id")
            if request in expected and image.get("delivery") == "attached_png":
                expect(hashlib.sha256(raw).hexdigest() == path.stem, "delivered fact hash differs")
                artifact = value["artifact"]
                expect(artifact["build_id"] == build_id and artifact["valid"] and artifact["current"],
                       "wrong/stale build delivered")
                expect(image["visual_analysis"] == "not_run", "delivery falsely certified analysis")
                expect(image["request"]["input_sha256"] == lineage["scene_sha256"], "image input differs")
                expect((image["request"]["view"], image["request"]["focus_m"]) == expected[request],
                       "image view/focus differs")
                image_path = Path(image["path"])
                expect(hashlib.sha256(image_path.read_bytes()).hexdigest() == image["image_sha256"],
                       "actual image differs")
                delivered[request] = {"fact_sha256": path.stem, "fact_sequence": value["fact_sequence"],
                                      "image": image}
        expect(set(delivered) == set(expected), "Agent did not retrieve all three actual PNGs")
        pattern = r"Core事实 ([a-z_]+) · seq (\d+) · (\d+) B(?: · |\s+)SHA256 ([a-f0-9]{64})"
        tui.keys(CTRL_HOME, 0.2)
        screen = tui.wait("visual private Core fact", lambda s: re.search(pattern, s), 90)
        kind, seq, size, digest = re.search(pattern, screen).groups()
        raw = (directory / "facts" / (digest + ".json")).read_bytes()
        expect(len(raw) == int(size) and hashlib.sha256(raw).hexdigest() == digest, "Core bytes differ")
        (evidence / "visual-core-fact.json").write_bytes(raw)
        (evidence / "visual-core-screen.txt").write_text(screen, encoding="utf-8")
        replies = []
        for _ in range(8):
            replies.append(tui.screen())
            tui.keys("\x1b[5;2~", 0.15)
        (evidence / "visual-reply-pages.txt").write_text("\n\n".join(replies), encoding="utf-8")
        for suffix, expected_hash in build["artifact_hashes"].items():
            path = Path(build["output"]).with_suffix(suffix)
            expect(hashlib.sha256(path.read_bytes()).hexdigest() == expected_hash, "review altered build")
        expect(all(hashlib.sha256((root / path).read_bytes()).hexdigest() == digest
                   for path, digest in source_hashes.items()), "review altered CAD")
        result = {"image_delivery": "passed", "platform_turn": "completed",
                  "ai_visual_review": "executed_ungraded", "human_visual_review": "not_run",
                  "scene_and_authority_unchanged": True, "delivered": delivered,
                  "core_fact": {"kind": kind, "sequence": int(seq), "bytes": int(size), "sha256": digest},
                  "limitation": "Actual PNG delivery and completed Agent turn; visual findings remain ungraded"}
        (evidence / "visual-result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")

    report.step("Platform Agent receives actual bound images and completes visual analysis without writes",
                review, tui)
