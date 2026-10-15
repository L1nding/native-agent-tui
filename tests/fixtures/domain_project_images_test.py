"""Image review needs a durable image block and a later response of the same turn."""

import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from domain_project_images import admitted_images


@unittest.skipUnless(shutil.which("node"), "requires Node's installed zstd support")
class ModelImageAdmission(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.journal = self.root / "journal"
        self.journal.mkdir()
        self.session = "image-fixture"
        self.store = self.root / "sessions/workdir" / self.session
        self.store.mkdir(parents=True)
        (self.journal / "fixture.jsonl").write_text(
            json.dumps(
                {
                    "payload": {
                        "tasks": [
                            {
                                "kind": "rootTurn",
                                "external": {"thread_id": self.session},
                            }
                        ]
                    }
                }
            ),
            encoding="utf-8",
        )
        self.receipt = "a" * 64
        self.text = {
            "type": "text",
            "text": json.dumps(
                {
                    "contract": "modeling-fact-receipt/v1",
                    "fact_sha256": self.receipt,
                    "agent_view": {"delivery": "attached_png"},
                }
            ),
        }
        self.image = {
            "type": "image",
            "attachment": {
                "attachmentId": "sha256:" + "b" * 64,
                "mediaType": "image/webp",
                "width": 16,
                "height": 8,
            },
        }

    def read(self, content, *, response_turn="turn", response_step=3):
        rows = [
            {"type": "request/header", "seq": 1},
            {
                "type": "tool/result",
                "seq": 10,
                "data": {"turn": "turn", "step": 2, "message": {"content": content}},
            },
            {
                "type": "assistant/message",
                "seq": 13,
                "data": {"turn": response_turn, "step": response_step},
            },
        ]
        script = "const fs=require('node:fs'),z=require('node:zlib');const rows=JSON.parse(process.argv[1]);fs.writeFileSync(process.argv[2],Buffer.concat(rows.map(r=>z.zstdCompressSync(Buffer.from(JSON.stringify(r)+'\\n')))));"
        subprocess.run(
            [
                "node",
                "-e",
                script,
                json.dumps(rows),
                str(self.store / "session.v4.jsonl.zstd"),
            ],
            check=True,
            capture_output=True,
        )
        return admitted_images(
            self.journal, [self.receipt], session_root=self.root / "sessions"
        )

    def test_same_turn_model_response_uses_an_admitted_image_without_a_second_header(
        self,
    ):
        value = self.read([self.text, self.image])
        self.assertEqual(value["admitted"][0]["subsequent_model_step"], 3)
        self.assertEqual(value["admitted"][0]["images"][0]["mediaType"], "image/webp")

    def test_attached_png_receipt_and_unavailable_text_are_insufficient(self):
        with self.assertRaisesRegex(AssertionError, "image unavailable"):
            self.read(
                [
                    self.text,
                    {
                        "type": "text",
                        "text": "[image unavailable: model does not declare image input]",
                    },
                ]
            )

    def test_offloaded_image_is_not_treated_as_visible(self):
        with self.assertRaisesRegex(AssertionError, "not admitted"):
            self.read([self.text, {**self.image, "offloaded": True}])

    def test_another_turn_response_cannot_prove_review(self):
        with self.assertRaisesRegex(AssertionError, "not admitted"):
            self.read([self.text, self.image], response_turn="other")

    def test_same_step_does_not_prove_a_model_call_after_the_image(self):
        with self.assertRaisesRegex(AssertionError, "not admitted"):
            self.read([self.text, self.image], response_step=2)


if __name__ == "__main__":
    unittest.main()
