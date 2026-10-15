"""Exact refusal evidence can be old generic or current typed, never an Agent assertion."""

import unittest

from domain_component_edit_recovery_check import (
    current_image_receipts,
    removed_source_refusal,
)


class RemovedSourceEvidence(unittest.TestCase):
    def fact(self, code, message):
        return {
            "kind": "diagnostic",
            "ok": False,
            "diagnostics": [{"code": code, "message": message}],
        }

    def test_current_typed_refusal_is_admitted(self):
        self.assertTrue(
            removed_source_refusal(
                self.fact("source_removed_requires_reopen", "exact proof required")
            )
        )

    def test_old_generic_code_requires_exact_message_prefix(self):
        self.assertTrue(
            removed_source_refusal(
                self.fact(
                    "project_rejected",
                    "source_removed_requires_reopen: exact proof required",
                )
            )
        )
        self.assertFalse(
            removed_source_refusal(
                self.fact("project_rejected", "some other failed input")
            )
        )
        self.assertFalse(
            removed_source_refusal(
                self.fact(
                    "project_rejected", "Agent says source_removed_requires_reopen"
                )
            )
        )

    def test_non_diagnostic_or_successful_claim_is_not_refusal(self):
        fact = self.fact("source_removed_requires_reopen", "blocked")
        fact["ok"] = True
        self.assertFalse(removed_source_refusal(fact))
        fact["ok"] = False
        fact["kind"] = "proposal"
        self.assertFalse(removed_source_refusal(fact))


class ImageDeliveryEvidence(unittest.TestCase):
    def test_only_this_recovery_and_build_deliveries_are_counted(self):
        def fact(request, build="edited", delivery="attached_png"):
            return {
                "artifact": {
                    "build_id": build,
                    "review_image": {
                        "delivery": delivery,
                        "delivery_request_id": request,
                    },
                }
            }

        facts = [
            ("old", fact("previous-top")),
            ("other-build", fact("current-top", build="old-build")),
            ("reference", fact("current-top", delivery="reference_only")),
            ("top", fact("current-top")),
            ("perspective", fact("current-perspective")),
        ]
        self.assertEqual(
            current_image_receipts(facts, "edited", "current"),
            ["top", "perspective"],
        )


if __name__ == "__main__":
    unittest.main()
