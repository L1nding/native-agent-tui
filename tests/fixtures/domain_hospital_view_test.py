"""Hospital classification uses Core facts even when rows are paginated."""

import copy
import unittest

from domain_hospital_mixed_check import (
    classified_state_visible, fact_sync_failure_is_terminal, artifact_validation_visible,
)


class HospitalViewTest(unittest.TestCase):
    def setUp(self):
        self.marker = {
            "source_sha256": "cad-sha",
            "answers": [
                {"source_id": "stool", "category": "chair"},
                {"source_id": "bed", "category": "bed"},
                {"source_id": "basin", "category": "undercounter_basin"},
            ],
        }
        self.objects = {
            a["source_id"]: {
                "category": a["category"],
                "basis": "assumed",
                "source_sha256": "cad-sha",
            }
            for a in self.marker["answers"]
        }
        self.screen = """┌方案与执行事实┐
│来源实例对账 · 扫描 actual-scan│
│capability_missing 1 · pending_addition 2 · pending_confirmation 484│
│bed · pending_addition│
┌Agent 回复┐"""

    def test_current_artifact_validation_survives_wrapping_at_build_identifier(self):
        build_id = "floor-built-8351deef91f143fda63ab0f72e98d3a1-430bb6bb"
        screen = f"""┌方案与执行事实┐
│产物验证 valid=true · current=true · │
│build_id={build_id}│
┌Agent 回复┐"""
        self.assertTrue(artifact_validation_visible(screen, build_id))
        for wrong in (screen.replace("current=true", "current=false"),
                      screen.replace("valid=true", "valid=false"),
                      screen.replace(build_id, "foreign-build")):
            self.assertFalse(artifact_validation_visible(wrong, build_id))
        self.assertFalse(artifact_validation_visible(
            f"┌方案与执行事实┐\n│产物验证 not_run│\n┌Agent 回复┐\n"
            f"产物验证 valid=true · current=true · build_id={build_id}", build_id))

    def test_running_tool_error_remains_recoverable_until_actual_terminal_state(self):
        message = "领域事实同步失败：领域工具未成功返回事实回执"
        self.assertFalse(fact_sync_failure_is_terminal("建模 · Running · 平台模型\n" + message))
        self.assertTrue(fact_sync_failure_is_terminal("建模 · Completed · 平台模型\n" + message))
        self.assertTrue(fact_sync_failure_is_terminal("建模 · Failed · 平台模型\n" + message))
        self.assertFalse(fact_sync_failure_is_terminal("建模 · Completed · 平台模型\nCore事实"))

    def test_one_visible_category_with_bound_core_totals_is_enough(self):
        self.assertTrue(classified_state_visible(self.screen, self.objects, self.marker))

    def test_conversational_totals_cannot_replace_core_ledger(self):
        self.assertFalse(
            classified_state_visible(
                "方案与执行事实 等待平台Agent Agent 回复 " + self.screen,
                self.objects,
                self.marker,
            )
        )

    def test_category_basis_and_source_must_match_for_every_instance(self):
        for field, wrong in (
            ("category", "chair"),
            ("basis", "user_confirmed"),
            ("source_sha256", "foreign-cad"),
        ):
            with self.subTest(field=field):
                objects = copy.deepcopy(self.objects)
                objects["basin"][field] = wrong
                self.assertFalse(classified_state_visible(self.screen, objects, self.marker))

    def test_missing_or_extra_interpretations_do_not_satisfy_the_stage(self):
        for objects in ({"bed": self.objects["bed"]}, {**self.objects, "extra": {}}):
            self.assertFalse(classified_state_visible(self.screen, objects, self.marker))


if __name__ == "__main__":
    unittest.main()
