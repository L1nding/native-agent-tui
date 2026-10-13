"""The C08 observer reads actual ledger fields and excludes conversational imitations."""
import unittest
from domain_vertical_check import source_ledger_visible


class VerticalViewTest(unittest.TestCase):
    def test_actual_core_source_rows_are_observed_without_component_ids(self):
        screen = """┌方案与执行事实┐
│来源实例对账 · 扫描 actual-sha│
│pending_confirmation 2│
│source.vertical.instance.34 · stair · pending_confirmation · 已有实例 []│
│source.vertical.instance.3B · shaft · pending_confirmation · 已有实例 []│
┌Agent 回复┐"""
        self.assertTrue(source_ledger_visible(screen))

    def test_agent_reply_cannot_supply_missing_core_ledger_rows(self):
        screen = """┌方案与执行事实┐
│等待平台Agent│
┌Agent 回复┐
来源实例对账 source.vertical.instance.34 source.vertical.instance.3B"""
        self.assertFalse(source_ledger_visible(screen))
        self.assertFalse(source_ledger_visible("来源实例对账 source.vertical.instance.34 source.vertical.instance.3B"))

    def test_one_source_row_does_not_prove_both_instances_were_displayed(self):
        self.assertFalse(source_ledger_visible("方案与执行事实 来源实例对账 source.vertical.instance.34 Agent 回复"))


if __name__ == "__main__":
    unittest.main()
