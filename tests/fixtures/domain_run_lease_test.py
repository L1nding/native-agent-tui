"""Only one keyboard acceptance process can own an isolated scene."""

import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from domain_run_lease import acquire


class DomainRunLeaseTest(unittest.TestCase):
    def test_same_scene_rejected_other_scene_independent_and_close_releases(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            first = acquire(root / "locks", root / "scene.json")
            try:
                with self.assertRaisesRegex(RuntimeError, "still owns"):
                    acquire(root / "locks", root / "scene.json")
                other = acquire(root / "locks", root / "other.json")
                other.close()
            finally:
                first.close()
            again = acquire(root / "locks", root / "scene.json")
            again.close()

    def test_process_death_releases_lease_without_removing_lock_path(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            script = "from domain_run_lease import acquire; import sys,time; handle=acquire(sys.argv[1],sys.argv[2]); print('owned',flush=True); time.sleep(30)"
            child = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    script,
                    str(root / "locks"),
                    str(root / "scene"),
                ],
                cwd=Path(__file__).parent,
                stdout=subprocess.PIPE,
                text=True,
            )
            try:
                self.assertEqual(child.stdout.readline().strip(), "owned")
                with self.assertRaisesRegex(RuntimeError, "still owns"):
                    acquire(root / "locks", root / "scene")
                child.kill()
                child.wait(timeout=10)
                deadline = time.monotonic() + 2
                while True:
                    try:
                        lease = acquire(root / "locks", root / "scene")
                        break
                    except RuntimeError:
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(0.01)
                lease.close()
                self.assertEqual(len(list((root / "locks").glob("*.lock"))), 1)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
                child.stdout.close()


if __name__ == "__main__":
    unittest.main()
