"""A real approval panel must not be mistaken for the editable steering window."""

import unittest

from dsh_tui_check import steering_tool_window


class SteeringWindowTests(unittest.TestCase):
    def test_running_tool_awaiting_approval_does_not_offer_steering(self):
        screen = "● Running\n▸ running · ping\nCtrl+Y accept · Ctrl+N decline\nWaiting for action"
        self.assertFalse(steering_tool_window(screen))

    def test_running_tool_after_approval_accepts_guidance(self):
        self.assertTrue(steering_tool_window("● Running\n▸ running · ping\n/steer guide"))

    def test_completed_turn_does_not_receive_guidance(self):
        self.assertFalse(steering_tool_window("Completed\n▸ done · ping\nEnter send"))


if __name__ == "__main__":
    unittest.main()
