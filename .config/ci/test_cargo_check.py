"""Verify telemetry cannot hide a failed compiler or discard diagnostics."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("cargo_check", Path(__file__).with_name("cargo-check.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CheckTests(unittest.TestCase):
    def run_cargo(self, exit_code):
        with tempfile.TemporaryDirectory() as root:
            cargo = Path(root) / "cargo"
            cargo.write_text(
                f"#!{sys.executable}\n"
                "import json, sys\n"
                "assert '--locked' in sys.argv and '--all-targets' in sys.argv\n"
                "assert '--message-format=json-render-diagnostics' in sys.argv\n"
                "print(json.dumps({'reason': 'compiler-artifact', 'fresh': True}))\n"
                "print(json.dumps({'reason': 'compiler-artifact', 'fresh': False}))\n"
                "print(json.dumps({'reason': 'compiler-message', 'message': {'rendered': 'error: broken fixture\\n'}}))\n"
                "print('non-JSON wrapper output', flush=True)\n"
                f"sys.exit({exit_code})\n"
            )
            cargo.chmod(0o755)
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                result = module.check("x86_64-unknown-linux-gnu", 4, str(cargo))
            metric = json.loads(next(line.removeprefix("CI-METRIC ") for line in output.getvalue().splitlines() if line.startswith("CI-METRIC ")))
            return result, metric, output.getvalue(), errors.getvalue()

    def test_compile_failure_keeps_exit_code_and_diagnostics(self):
        result, metric, output, errors = self.run_cargo(101)
        self.assertEqual(result, 101)
        self.assertEqual(metric["exit_code"], 101)
        self.assertIn("error: broken fixture", errors)
        self.assertIn("non-JSON wrapper output", output)

    def test_reuse_is_counted_separately_from_rebuilds(self):
        result, metric, _, _ = self.run_cargo(0)
        self.assertEqual(result, 0)
        self.assertEqual(metric["fresh_artifacts"], 1)
        self.assertEqual(metric["rebuilt_artifacts"], 1)
        self.assertGreaterEqual(metric["seconds"], 0)


if __name__ == "__main__":
    unittest.main()
