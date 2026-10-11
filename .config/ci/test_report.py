"""Prevent repeated log exports and failed checks from inflating green timings."""

import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("report", Path(__file__).with_name("report.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReportTests(unittest.TestCase):
    def test_duplicate_exports_and_failures_are_not_success_samples(self):
        base = {"kind": "cargo-check", "head_sha": "abc", "target": "linux", "started_at": "first", "seconds": 4, "exit_code": 0, "fresh_artifacts": 9, "rebuilt_artifacts": 1}
        failed = dict(base, started_at="second", seconds=1, exit_code=101)
        log = "CI-METRIC " + json.dumps(base) + "\nCI-METRIC " + json.dumps(failed)
        report = module.summarize([log, log])
        self.assertEqual(len(report["attempts"]), 2)
        green = next(row for row in report["summaries"] if row["outcome"] == "passed")
        self.assertEqual(green["attempts"], 1)
        self.assertEqual(green["mean_seconds"], 4)
        self.assertEqual(green["artifact_reuse_fraction"], .9)
        self.assertIsNone(green["observed_p99_seconds"])


if __name__ == "__main__":
    unittest.main()
