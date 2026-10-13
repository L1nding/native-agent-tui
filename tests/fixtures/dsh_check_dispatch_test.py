import importlib.util
import json
import os
from types import SimpleNamespace
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


class DomainDispatchTest(unittest.TestCase):
    def test_domain_only_runs_domain_checks_and_cleanup(self):
        path = Path(__file__).with_name("dsh_tui_check.py")
        spec = importlib.util.spec_from_file_location("dsh_check_dispatch", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            fake_script = Path(directory) / "tests" / "fixtures" / "dsh_tui_check.py"
            with patch.object(module, "__file__", str(fake_script)), \
                 patch("sys.argv", [str(path), "--binary", "fake.exe", "--model", "test/model", "--only", "domain"]), \
                 patch.object(module, "domain_checks") as domain, \
                 patch.object(module, "tui_checks") as tui, \
                 patch.object(module, "cli_checks") as cli, \
                 patch.object(module.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "")), \
                 patch.object(module.time, "sleep"), \
                 self.assertRaises(SystemExit) as exited:
                module.main()
            self.assertEqual(exited.exception.code, 0)
            domain.assert_called_once()
            tui.assert_not_called()
            cli.assert_not_called()

    def test_full_domain_cases_refuse_a_nonisolated_fixture_scene(self):
        path = Path(__file__).with_name("dsh_tui_check.py")
        spec = importlib.util.spec_from_file_location("dsh_check_isolation", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for case in ("chair", "mixed", "blocked", "clarify", "hosted", "async", "recovery", "failure", "cancel", "conflict", "vertical", "hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch", "hospital-floor-finalize"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                context = root / "context.json"
                context.write_text(json.dumps({"scene_path": "tests/fixtures/scene.json"}), encoding="utf-8")
                report = module.Report(root)
                args = SimpleNamespace(domain_case=case)
                with patch.dict(os.environ, {"MODELING_COMPLEX_CONTEXT": str(context), "MODELING_COMPLEX_ROOT": str(root)}), \
                     patch.object(module, "Tui") as tui:
                    module.domain_checks(report, Path("fake.exe"), [], args, root, root)
                tui.assert_not_called()
                self.assertEqual(len(report.failed()), 1)
                self.assertIn("isolated scene", report.rows[0][0])

    def test_second_scene_owner_is_rejected_before_any_tui_or_model_process(self):
        from domain_run_lease import acquire
        path = Path(__file__).with_name("dsh_tui_check.py")
        spec = importlib.util.spec_from_file_location("dsh_check_owner", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            context = root / "context.json"
            context.write_text(json.dumps({"scene_path": "outputs/scene.json"}), encoding="utf-8")
            fake_script = root / "tests/fixtures/dsh_tui_check.py"
            lease = acquire(root / "target/domain-run-locks", root / "outputs/scene.json")
            report = module.Report(root)
            args = SimpleNamespace(domain_case="hospital-floor-resume", model="test/model", dsh="test", profile="test")
            try:
                with patch.object(module, "__file__", str(fake_script)), \
                     patch.dict(os.environ, {"MODELING_COMPLEX_CONTEXT":str(context), "MODELING_COMPLEX_ROOT":str(root)}), \
                     patch.object(module, "Tui") as tui:
                    module.domain_checks(report, Path("fake.exe"), [], args, root, root)
                tui.assert_not_called()
                self.assertEqual(len(report.failed()), 1)
                self.assertIn("still owns", report.rows[0][2])
            finally:
                lease.close()


if __name__ == "__main__":
    unittest.main()
