"""A structural failure must return red before starting cross compilation."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class FastTests(unittest.TestCase):
    def test_failed_structural_check_does_not_start_cargo(self):
        nu = shutil.which("nu")
        self.assertIsNotNone(nu, "the CI contract check requires Nushell")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            marker = root / "cargo-started"
            nix = root / "nix"
            nix.write_text(f"#!{sys.executable}\nimport sys\nif sys.argv[1] == 'eval':\n print('x86_64-linux')\nelse:\n print('structural check rejected', file=sys.stderr)\n sys.exit(1)\n")
            cargo = root / "cargo"
            cargo.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nPath({str(marker)!r}).touch()\n")
            # This checks scheduling, not GNU timeout. The parent test bounds
            # the entire invocation, including this platform-neutral shim.
            timeout = root / "timeout"
            timeout.write_text(f"#!{sys.executable}\nimport os, sys\nos.execvp(sys.argv[3], sys.argv[3:])\n")
            nix.chmod(0o755)
            cargo.chmod(0o755)
            timeout.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"], CI_FAST_CACHE_ROOT="", NUDOX_CROSS_CHECK_TARGETS="x86_64-unknown-linux-gnu")
            result = subprocess.run([nu, "--no-config-file", str(Path(__file__).with_name("fast.nu"))], env=env, capture_output=True, text=True, timeout=30)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("structural check rejected", result.stdout + result.stderr)
            self.assertFalse(marker.exists(), result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
