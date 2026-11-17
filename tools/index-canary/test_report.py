#!/usr/bin/env python3
"""Synthetic report-contract controls; these are not product QA evidence."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

WRAPPER = Path(__file__).with_name("run.py")


class ReportControls(unittest.TestCase):
    def exercise(self, mutation):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            manifest = {"cases": [{"id": "one", "steps": [{"id": "index", "kind": "index"}]}]}
            manifest_path = root / "manifest.json"
            manifest_path.write_text(json.dumps(manifest))
            digest = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
            revision = "a" * 40
            bins = root / "runtime/bin"
            bins.mkdir(parents=True)
            (root / "runtime/share/nudox").mkdir(parents=True)
            (root / "runtime/share/nudox/source-revision.txt").write_text(revision + "\n")
            release = {"source_revision": revision, "manifest_sha256": digest,
                       "driver_sha256": "b" * 64, "binaries": {"backend-cli": "c" * 64}}
            (root / "release.json").write_text(json.dumps(release))
            report = {**release, "status": "pass", "authority_environment_sha256": "d" * 64,
                      "containment_policy": {"required": True, "root": {"Unix": list(os.fsencode(root / "cgroup"))}},
                      "cases": [{"id": "one", "status": "pass", "steps": [{"id": "index", "status": "pass", "duration_ms": 10}]}]}
            report = mutation(report)
            report_file = root / "synthetic-report.json"
            report_file.write_text(json.dumps(report))
            runner = root / "runner"
            runner.write_text("#!/usr/bin/env python3\nimport os,pathlib,shutil,sys\np=pathlib.Path(sys.argv[sys.argv.index('--evidence')+1]); p.mkdir(); shutil.copyfile(os.environ['TEST_REPORT'],p/'report.json')\n")
            runner.chmod(0o700)
            env = {**os.environ, "KEEL_CGROUP_ROOT": str(root / "cgroup"), "TEST_REPORT": str(report_file)}
            command = ["python3", str(WRAPPER), "--manifest", str(manifest_path), "--bin-dir", str(bins),
                       "--runner", str(runner), "--release-receipt", str(root / "release.json"),
                       "--state-dir", str(root / "state"), "--metrics", str(root / "metrics")]
            proc = subprocess.run(command, env=env, capture_output=True, text=True, timeout=10)
            run = json.loads((root / "state/last-run.json").read_text())
            return proc, run, (root / "metrics").read_text()

    def test_synthetic_valid_contract(self):
        proc, run, metrics = self.exercise(lambda report: report)
        self.assertEqual(proc.returncode, 0)
        self.assertTrue(run["success"])
        self.assertIn("canary_run_success 1", metrics)

    def test_malformed_reports_fail_with_metrics(self):
        mutations = [lambda _: [], lambda r: {**r, "cases": [1]},
                     lambda r: {**r, "cases": [{"id": "one", "steps": [1]}]},
                     lambda r: {**r, "containment_policy": {"required": True, "root": {"Unix": [47]}}},
                     lambda r: {**r, "manifest_sha256": "0" * 64}]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                proc, run, metrics = self.exercise(mutation)
                self.assertEqual(proc.returncode, 1)
                self.assertFalse(run["success"])
                self.assertEqual(run["failure"], "invalid-receipt")
                self.assertIn("canary_run_success 0", metrics)


if __name__ == "__main__":
    unittest.main()
