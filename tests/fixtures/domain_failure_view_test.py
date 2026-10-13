"""Viewport evidence must bind status lines to the exact job, even at a border."""

import unittest

from domain_failure_check import business_job_section


class FailureViewTest(unittest.TestCase):
    def test_clipped_title_and_border_still_preserve_status_identity(self):
        screen = "方案与执行事实\n│Blender 产物 failed · current=unknown · build_id=b│\n│产物验证 valid=false · current=unknown · build_id=b│\n└────────┘\nAgent 回复 valid=true build_id=b"
        section = business_job_section(screen, "b")
        self.assertIn("valid=false", section)
        self.assertIn("Blender 产物 failed", section)
        self.assertNotIn("valid=true", section)

    def test_similar_id_and_agent_words_cannot_supply_another_jobs_validation(self):
        screen = "方案与执行事实\n构建任务 a\n│产物验证 valid=true · current=true · build_id=a-extra│\n└────────┘\nAgent 回复 valid=true build_id=a"
        self.assertEqual(business_job_section(screen, "a"), "")
        self.assertIn("valid=true", business_job_section(screen, "a-extra"))


if __name__ == "__main__":
    unittest.main()
