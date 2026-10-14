"""Exercise the public tier command's failure and no-rebuild boundaries."""
import json
import os
from pathlib import Path
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
        for arguments in (["integration"], ["smoke", "--update"], ["deep", "--case", "polyglot"], ["smoke", "--jobs", "2"], ["integration", "--bin-dir", "/unused", "--jobs", "0"]):
            result = subprocess.run([sys.executable, str(RUNNER), *arguments], env=dict(os.environ, PATH=""), capture_output=True)
            self.assertEqual(result.returncode, 2, result.stderr)

    def test_plan_does_not_launch_tools_or_update_snapshots(self):
        result = subprocess.run([sys.executable, str(RUNNER), "integration", "--bin-dir", "/unused/bins", "--case", "polyglot", "--update", "--plan"], env=dict(os.environ, PATH=""), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--update", json.loads(result.stdout))


if __name__ == "__main__":
    unittest.main()
