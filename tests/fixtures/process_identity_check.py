"""Verify cleanup evidence rejects living owned processes and distinguishes PID reuse."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
from windows_process_identity import check_clean, record


@unittest.skipUnless(os.name == "nt", "requires native Windows process identities")
class ProcessIdentityCheck(unittest.TestCase):
    def test_living_original_process_fails_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record(root, [os.getpid()])
            with self.assertRaisesRegex(AssertionError, "still alive"):
                check_clean(root)

    def test_reused_pid_cannot_be_mistaken_for_the_original_process(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record(root, [os.getpid()])
            path = root / "processes.jsonl"
            process = json.loads(path.read_text())
            process["created"] -= 1
            path.write_text(json.dumps(process) + "\n")
            check_clean(root)

    def test_exited_process_passes_and_missing_identity_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                       creationflags=subprocess.CREATE_NO_WINDOW)
            try:
                record(root, [process.pid])
            finally:
                process.terminate()
                process.wait(timeout=5)
            check_clean(root)
            (root / "processes.jsonl").write_text("")
            with self.assertRaisesRegex(AssertionError, "incomplete"):
                check_clean(root)


if __name__ == "__main__":
    unittest.main()
