"""Resume provenance must not turn failed history or earlier authority into success."""

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from domain_partial_resume_check import HISTORY_FILES, interrupted_prefix


class PartialResumeTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.history = self.root / "dsh-e2e-12345678"
        self.history.mkdir()
        self.context = self.root / "context.json"
        self.context.write_bytes(b"context")
        self.scene = self.root / "scene.json"
        self.scene.write_bytes(b"scene")
        (self.root / "source.dxf").write_bytes(b"authored CAD")
        (self.root / "answer-key.json").write_bytes(b"authored answers")
        self.directory = self.root / "changes"
        self.directory.mkdir()
        def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()
        self.write(self.history / "main-lineage.json", {
            "context_sha256":sha(self.context), "baseline_sha256":sha(self.scene),
            "source_sha256":sha(self.root / "source.dxf"),
            "answer_sha256":sha(self.root / "answer-key.json"),
        })
        stages = [{"stage":name,"proposal_id":"v3"} for name in
                  ("main-initial","main-cabinet-revision","main-partial-selection")]
        self.write(self.history / "main-stages.json", stages)
        self.write(self.history / "report.json", [{"status":s} for s in
                   ("PASS","PASS","PASS","PASS","FAIL","PASS")])
        for name in HISTORY_FILES:
            if not (self.history / name).exists(): (self.history / name).write_bytes(b"actual screen")
        self.write(self.directory / "v3.json", {
            "proposal_id":"v3","revision":3,"state":"prepared","pending":[],
        })
        job = self.directory / "artifacts/main-preview-12345678"
        job.mkdir(parents=True)
        self.write(job / "build.json", {"state":"built","candidate":True,"proposal_id":"v3"})

    @staticmethod
    def write(path, value):
        path.write_text(json.dumps(value), encoding="utf-8")

    def inspect(self):
        return interrupted_prefix(self.history, self.context, self.scene, self.directory)

    def test_preflight_preserves_failed_history_and_digests_every_original_stage(self):
        lineage, proposal, preview = self.inspect()
        self.assertEqual(proposal["proposal_id"], "v3")
        self.assertEqual(preview, "main-preview-12345678")
        self.assertEqual(set(lineage["history_sha256"]), set(HISTORY_FILES))
        self.assertEqual(lineage["resumed_from"], str(self.history.resolve()))
        self.assertEqual(json.loads((self.history / "report.json").read_bytes())[4]["status"], "FAIL")
        self.assertFalse(list(self.directory.glob("authorization-*.json")))
        self.assertFalse(list(self.directory.glob("operation-*.json")))

    def test_input_edits_and_earlier_authority_reject_resume_before_any_platform_input(self):
        for name in ("context.json", "scene.json", "source.dxf", "answer-key.json"):
            path = self.root / name
            original = path.read_bytes()
            path.write_bytes(original + b"later edit")
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "changed"):
                self.inspect()
            path.write_bytes(original)
        for name in ("authorization-earlier.json", "operation-earlier.json"):
            path = self.directory / name
            path.write_bytes(b"{}")
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "writer authority"):
                self.inspect()
            path.unlink()

    def test_missing_feedback_unprepared_candidate_and_wrong_preview_reject_resume(self):
        stages = json.loads((self.history / "main-stages.json").read_bytes())
        self.write(self.history / "main-stages.json", stages[:-1])
        with self.assertRaisesRegex(ValueError, "feedback stages"): self.inspect()
        self.write(self.history / "main-stages.json", stages)
        proposal = json.loads((self.directory / "v3.json").read_bytes())
        self.write(self.directory / "v3.json", {**proposal,"state":"blocked"})
        with self.assertRaisesRegex(ValueError, "prepared V3"): self.inspect()
        self.write(self.directory / "v3.json", proposal)
        job = self.directory / "artifacts/main-preview-12345678/build.json"
        self.write(job, {"state":"built","candidate":False,"proposal_id":"v3"})
        with self.assertRaisesRegex(ValueError, "existing built preview"): self.inspect()

    def test_interrupted_preview_cannot_be_relabelled_as_passed_history(self):
        report = json.loads((self.history / "report.json").read_bytes())
        report[4]["status"] = "PASS"
        self.write(self.history / "report.json", report)
        with self.assertRaisesRegex(ValueError, "preserved interrupted"): self.inspect()


if __name__ == "__main__":
    unittest.main()
