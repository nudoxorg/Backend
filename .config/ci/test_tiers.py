"""Exercise the public tier command's failure and no-rebuild boundaries."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name("test.py")


class TierCommandTests(unittest.TestCase):
    def test_child_failure_is_the_tier_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "invocation.json"
            nix = root / "nix"
            nix.write_text(f"#!{sys.executable}\nimport json,os,sys\n"
                           "json.dump([sys.argv[1:], os.environ.get('CI_T2_EXTERNAL')], open(os.environ['TIER_MARKER'], 'w'))\n"
                           "sys.exit(23)\n")
            nix.chmod(0o700)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ.get("PATH", ""), TIER_MARKER=str(marker))
            result = subprocess.run([sys.executable, str(RUNNER), "integration", "--bin-dir", str(root / "binaries"), "--case", "cli-usage", "--jobs", "2"], env=env, capture_output=True)
            self.assertEqual(result.returncode, 23, result.stderr)
            arguments, isolated = json.loads(marker.read_text())
            self.assertIn("cli-usage", arguments)
            self.assertIn(str((root / "binaries").resolve()), arguments)
            self.assertNotIn("cargo", arguments)
            self.assertEqual(arguments[arguments.index("--jobs") + 1], "2")
            self.assertEqual(isolated, "1")

    def test_missing_binaries_and_cross_tier_options_fail_before_nix(self):
        for arguments in (["integration"], ["smoke", "--update"], ["deep", "--case", "polyglot"], ["smoke", "--jobs", "2"], ["integration", "--bin-dir", "/unused", "--jobs", "0"], ["smoke", "--ci-report", "/unused"], ["list", "--ci-report", "/unused"], ["integration", "--bin-dir", "/unused", "--ci-report", "/unused"], ["integration", "--bin-dir", "/unused", "--source-sha", "a" * 40], ["integration", "--bin-dir", "/unused", "--ci-report", "/unused", "--source-sha", "a" * 40, "--case", "cli-usage"]):
            result = subprocess.run([sys.executable, str(RUNNER), *arguments], env=dict(os.environ, PATH=""), capture_output=True)
            self.assertEqual(result.returncode, 2, result.stderr)

    def test_ci_evidence_plan_forwards_identity_without_running_tools(self):
        result = subprocess.run([sys.executable, str(RUNNER), "integration", "--bin-dir", "/unused/bin",
                                 "--ci-report", "/unused/report.json", "--source-sha", "a" * 40, "--plan"],
                                env=dict(os.environ, PATH=""), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        arguments = json.loads(result.stdout)
        self.assertEqual(arguments[arguments.index("--source-sha") + 1], "a" * 40)
        self.assertEqual(arguments[arguments.index("--ci-report") + 1], "/unused/report.json")

    def test_selection_preview_falls_back_without_launching_build_tools(self):
        result = subprocess.run([sys.executable, str(RUNNER), "select", "--base", "missing-ref"],
                                env=dict(os.environ, PATH=""), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        selected = json.loads(result.stdout)
        self.assertEqual(selected["tier"], "T1+T2")
        self.assertFalse(selected["diff_complete"])
        self.assertFalse(selected["executed"])
        self.assertTrue(selected["integration_cases"])

    def test_selection_options_cannot_be_ignored_by_other_tiers(self):
        for arguments in (["select"], ["smoke", "--base", "HEAD"],
                          ["integration", "--bin-dir", "/unused", "--worktree"],
                          ["select", "--base", "HEAD", "--update"]):
            result = subprocess.run([sys.executable, str(RUNNER), *arguments],
                                    env=dict(os.environ, PATH=""), capture_output=True)
            self.assertEqual(result.returncode, 2, result.stderr)

    def test_plan_does_not_launch_tools_or_update_snapshots(self):
        result = subprocess.run([sys.executable, str(RUNNER), "integration", "--bin-dir", "/unused/bins", "--case", "polyglot", "--update", "--plan"], env=dict(os.environ, PATH=""), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--update", json.loads(result.stdout))

    def test_complete_tier_keeps_legacy_evidence_after_vm_failure(self):
        self._assert_complete_tier_failure(vm_exit=19, legacy_exit=0)

    def test_complete_tier_keeps_legacy_failure_after_vm_success(self):
        self._assert_complete_tier_failure(vm_exit=0, legacy_exit=23)

    def _assert_complete_tier_failure(self, vm_exit, legacy_exit):
        # These real child processes control only orchestration failures. They
        # make no claim that a native worker or a VM was qualified.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ci = root / ".config" / "ci"
            ci.mkdir(parents=True)
            entry = ci / "integration.py"
            shutil.copyfile(RUNNER.with_name("integration.py"), entry)
            (ci / "keel-vm.py").write_text(
                "import json,pathlib,sys\n"
                "pathlib.Path('vm-invocation.json').write_text(json.dumps(sys.argv[1:]))\n"
                f"sys.exit({vm_exit})\n"
            )
            (ci / "surface.py").write_text(
                "import json,pathlib,sys\n"
                "args=sys.argv[1:]\n"
                "for name in ['--report','--ci-report']:\n"
                " if name in args:\n"
                "  p=pathlib.Path(args[args.index(name)+1]);p.parent.mkdir(parents=True,exist_ok=True)\n"
                "  p.write_text(json.dumps({'control':'legacy evidence retained'}))\n"
                f"sys.exit({legacy_exit})\n"
            )
            bundle = root / "artifact"
            (bundle / "bin").mkdir(parents=True)
            (bundle / "manifest.json").write_text('{}\n')
            report = root / "results" / "legacy.json"
            evidence = root / "results" / "legacy.evidence.json"
            completed = subprocess.run(
                [sys.executable, str(entry), "--bin-dir", str(bundle / "bin"),
                 "--source-sha", "a" * 40, "--report", str(report), "--ci-report", str(evidence)],
                text=True, capture_output=True,
            )
            self.assertEqual(completed.returncode, vm_exit or legacy_exit, completed.stderr)
            self.assertEqual(json.loads(evidence.read_text()), {"control": "legacy evidence retained"})
            self.assertTrue(report.is_file())
            summary = json.loads((report.parent / "legacy.keel-summary.json").read_text())
            self.assertEqual(summary["status"], "failed")
            self.assertEqual(summary["keel"]["exit"], vm_exit)
            self.assertEqual(summary["legacy"]["exit"], legacy_exit)
            invocation = json.loads((root / "vm-invocation.json").read_text())
            self.assertEqual(invocation[invocation.index("--source-sha") + 1], "a" * 40)


if __name__ == "__main__":
    unittest.main()
