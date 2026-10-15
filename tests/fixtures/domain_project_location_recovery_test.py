"""Acceptance cannot confuse an Agent reply or another build with Core evidence."""

import unittest

from domain_project_location_recovery_check import artifact_visible, core_panel


def screen(rows, reply=""):
    return (
        "┌方案与执行事实─┐\n"
        + rows
        + "\n└────────┘\n┌Agent 回复─┐\n"
        + reply
        + "\n└────┘"
    )


class CoreArtifactAdmission(unittest.TestCase):
    def test_agent_claim_outside_core_panel_is_not_artifact_evidence(self):
        rendered = screen(
            "│Blender 产物 not_run│", "│Blender 产物 built · build_id=target│"
        )
        self.assertFalse(artifact_visible(rendered, "target"))
        self.assertNotIn("build_id", core_panel(rendered))

    def test_core_built_and_validated_rows_are_admitted_separately(self):
        rendered = screen("│Blender 产物 built · build_id=target│")
        self.assertTrue(artifact_visible(rendered, "target"))
        self.assertFalse(artifact_visible(rendered, "target", validated=True))
        rendered = screen("│产物验证 valid=true · current=true · build_id=target│")
        self.assertTrue(artifact_visible(rendered, "target", validated=True))
        self.assertFalse(artifact_visible(rendered, "target"))

    def test_another_builds_success_cannot_validate_the_target(self):
        rendered = screen(
            "│产物验证 valid=true · build_id=other│\n│产物验证 valid=false · build_id=target│"
        )
        self.assertFalse(artifact_visible(rendered, "target", validated=True))
        self.assertTrue(artifact_visible(rendered, "other", validated=True))

    def test_business_panel_filter_is_idempotent_for_composed_screen_predicates(self):
        rendered = screen("│产物验证 valid=true · build_id=target│", "untrusted reply")
        panel = core_panel(rendered)
        self.assertEqual(core_panel(panel), panel)
        self.assertTrue(artifact_visible(panel, "target", validated=True))

    def test_missing_or_incomplete_business_panel_is_not_admitted(self):
        for rendered in [
            "Blender 产物 built · build_id=target",
            "┌方案与执行事实\nBlender 产物 built · build_id=target",
        ]:
            self.assertFalse(artifact_visible(rendered, "target"))


if __name__ == "__main__":
    unittest.main()
