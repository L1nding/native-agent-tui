"""A running header must not prevent capture of stable conversation anchor rows."""

import unittest

from conversation_anchor_check import anchor_screen


class AnimatedHeader:
    def __init__(self):
        self.reads = 0

    def stable(self):
        raise AssertionError("terminal frame did not settle")

    def read(self):
        self.reads += 1
        markers = ["ANCHOR_001", "ANCHOR_002"] if self.reads == 1 else ["ANCHOR_002", "ANCHOR_003"]
        return f"Running timer {self.reads}\n" + "\n".join(markers) + "\nactive spinner"


class AnchorCaptureTests(unittest.TestCase):
    def test_captures_complete_stable_anchor_rows_while_header_keeps_changing(self):
        console = AnimatedHeader()
        screen = anchor_screen(console)
        self.assertGreaterEqual(console.reads, 3)
        self.assertNotIn("ANCHOR_001", screen)
        self.assertIn("ANCHOR_002\nANCHOR_003", screen)


if __name__ == "__main__":
    unittest.main()
