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


    def test_upgrading_an_existing_crlf_linker_rewrites_its_shebang(self):
        with tempfile.TemporaryDirectory(prefix="nudox-script-upgrade-") as directory:
            repository = Path(directory)
            def git(*arguments):
                return subprocess.check_output(["git", *arguments], cwd=repository,
                                               stderr=subprocess.DEVNULL).strip()
            git("init", "-q")
            git("config", "core.autocrlf", "true")
            linker_name = SCRIPTS[0]
            linker = repository / linker_name
            linker.parent.mkdir(parents=True)
            # Exact pre-fix linker bytes: changing only attributes does not
            # cause Git to rewrite this already checked-out file on upgrade.
            old = (ROOT / "tests/cargo-linker-pre-lf-policy.txt").read_bytes()
            linker.write_bytes(old)
            linker.chmod(0o755)
            git("add", ".")
            git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "before")
            before = git("rev-parse", "HEAD").decode()
            shutil.copy2(ROOT / ".gitattributes", repository / ".gitattributes")
            shutil.copy2(ROOT / linker_name, linker)
            git("add", ".")
            git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "fixed")
            fixed = git("rev-parse", "HEAD").decode()
            git("checkout", "-q", before)
            self.assertIn(b"\r\n", linker.read_bytes(), "pre-fix Windows checkout reproduces CRLF")
            git("checkout", "-q", fixed)
            self.assertNotIn(b"\r", linker.read_bytes(), "normal upgrade repairs the existing shebang")
            # Kernel invocation succeeds on the upgraded checkout too.
            result = subprocess.run([str(linker), "--version"], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL)
            self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
