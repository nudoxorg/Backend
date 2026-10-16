#!/usr/bin/env python3
"""Focused controls for Linux's direct remote-store flake runner."""

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("linux-flake-check.py")
SPEC = importlib.util.spec_from_file_location("linux_flake_check", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class LinuxFlakeCheckTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.key = Path(self.tempdir.name) / "builder-key"
        self.key.write_text("test-only-placeholder")
        self.config = (
            "experimental-features = nix-command flakes\n"
            "builders = ssh-ng://nixbuilder@10.80.0.1 "
            f"x86_64-linux {self.key} 2 1 kvm,nixos-test\n"
        )

    def fake_run(
        self,
        calls,
        *,
        system="x86_64-linux",
        names='["control-plane", "formatting"]',
        codes=None,
    ):
        codes = codes or {}

        def run(args, *, capture=False):
            calls.append((args, capture))
            if args[:3] == ["nix", "eval", "--raw"]:
                return subprocess.CompletedProcess(args, codes.get("system", 0), system + "\n", "")
            if args[:3] == ["nix", "eval", "--json"]:
                return subprocess.CompletedProcess(
                    args, codes.get("inventory", 0), names, "inventory failed"
                )
            if "--no-build" in args:
                return subprocess.CompletedProcess(
                    args, codes.get("validation", 0), "", "validation failed"
                )
            return subprocess.CompletedProcess(args, codes.get("build", 0), "", "build failed")

        return run

    def test_local_call_keeps_existing_flake_check(self):
        calls = []
        with patch.dict(os.environ, {}, clear=True), patch.object(
            runner, "run", self.fake_run(calls)
        ):
            self.assertEqual(runner.main(), 0)
        self.assertEqual(
            calls,
            [(["nix", "flake", "check", "-L", "--keep-going", "path:."], False)],
        )

    def test_remote_build_uses_full_inventory_in_one_bounded_request(self):
        calls = []
        with patch.dict(os.environ, {"NIX_CONFIG": self.config}, clear=True), patch.object(
            runner, "run", self.fake_run(calls)
        ):
            self.assertEqual(runner.main(), 0)
        self.assertIn("--no-build", calls[0][0])
        self.assertEqual(len([call for call in calls if call[0][1:2] == ["build"]]), 1)
        build = calls[-1][0]
        self.assertEqual(build[build.index("--max-jobs") + 1], "1")
        self.assertEqual(
            build[-2:],
            [
                "path:.#checks.x86_64-linux.control-plane",
                "path:.#checks.x86_64-linux.formatting",
            ],
        )
        self.assertIn(f"?ssh-key={self.key}", build[build.index("--store") + 1])

    def test_failed_validation_stops_before_inventory_or_build(self):
        calls = []
        with patch.dict(os.environ, {"NIX_CONFIG": self.config}, clear=True), patch.object(
            runner, "run", self.fake_run(calls, codes={"validation": 1})
        ):
            self.assertEqual(runner.main(), 1)
        self.assertEqual(len(calls), 1)

    def test_empty_inventory_fails_closed(self):
        calls = []
        with patch.dict(os.environ, {"NIX_CONFIG": self.config}, clear=True), patch.object(
            runner, "run", self.fake_run(calls, names="[]")
        ):
            self.assertEqual(runner.main(), 1)
        self.assertFalse(any(call[0][1:2] == ["build"] for call in calls))

    def test_malformed_inventory_fails_closed(self):
        calls = []
        with patch.dict(os.environ, {"NIX_CONFIG": self.config}, clear=True), patch.object(
            runner, "run", self.fake_run(calls, names='["has.dot"]')
        ):
            self.assertEqual(runner.main(), 1)
        self.assertFalse(any(call[0][1:2] == ["build"] for call in calls))

    def test_inventory_evaluation_failure_stops_before_build(self):
        calls = []
        with patch.dict(os.environ, {"NIX_CONFIG": self.config}, clear=True), patch.object(
            runner, "run", self.fake_run(calls, codes={"inventory": 1})
        ):
            self.assertEqual(runner.main(), 1)
        self.assertFalse(any(call[0][1:2] == ["build"] for call in calls))

    def test_failed_build_is_not_reported_as_success(self):
        calls = []
        with patch.dict(os.environ, {"NIX_CONFIG": self.config}, clear=True), patch.object(
            runner, "run", self.fake_run(calls, codes={"build": 23})
        ):
            self.assertEqual(runner.main(), 23)

    def test_malformed_builder_config_fails_instead_of_falling_back(self):
        with patch.dict(
            os.environ,
            {"NIX_CONFIG": "builders = local x86_64-linux /tmp/key"},
            clear=True,
        ):
            with self.assertRaises(ValueError):
                runner.remote_store_from_config()

    def test_missing_builder_key_fails_instead_of_falling_back(self):
        config = self.config.replace(str(self.key), str(self.key) + ".missing")
        with patch.dict(os.environ, {"NIX_CONFIG": config}, clear=True):
            with self.assertRaises(ValueError):
                runner.remote_store_from_config()


if __name__ == "__main__":
    unittest.main()
