#!/usr/bin/env python3
"""Exercise Unix script checkout under Windows Git line-ending settings."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = (
    ".config/scripts/cargo-umask-linker.sh",
    ".config/scripts/nix-cargo-output-umask.sh",
    ".config/scripts/cargo-shared-cache.sh",
    ".config/direnv/entrypoint",
    ".config/scripts/fleet-census-recovery",
    "tools/package/linux_release_package.py",
    "tests/fuzz/libfuzzer-rustc",
)


class ScriptCheckoutTests(unittest.TestCase):
    def test_windows_checkout_keeps_unix_scripts_executable(self):
        with tempfile.TemporaryDirectory(prefix="nudox-script-checkout-") as directory:
            repository = Path(directory) / "repository"
            repository.mkdir()
            subprocess.run(["git", "init", "-q", str(repository)], check=True)
            for name in (".gitattributes", *SCRIPTS):
                destination = repository / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / name, destination)
            subprocess.run(["git", "add", "."], cwd=repository, check=True)
            checkout = Path(directory) / "checkout"
            checkout.mkdir()
            subprocess.run(
                ["git", "-c", "core.autocrlf=true", "checkout-index", "--all",
                 f"--prefix={checkout}{os.sep}"],
                cwd=repository, check=True,
            )
            for name in SCRIPTS:
                with self.subTest(script=name):
                    self.assertNotIn(b"\r", (checkout / name).read_bytes())

            # The kernel must accept the actual configured linker shebang;
            # running through `sh script` would hide the reported ENOENT.
            compiler = Path(directory) / "bin" / "cc"
            compiler.parent.mkdir()
            compiler.write_text("#!/bin/sh\nexit 37\n")
            compiler.chmod(0o755)
            environment = dict(os.environ, PATH=f"{compiler.parent}{os.pathsep}{os.environ['PATH']}")
            result = subprocess.run(
                [str(checkout / SCRIPTS[0]), "argument with spaces"], env=environment,
            )
            self.assertEqual(result.returncode, 37)


if __name__ == "__main__":
    unittest.main()
