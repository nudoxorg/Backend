#!/usr/bin/env python3
"""Contract tests for pinned macOS app-build runners and receipts."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import shlex
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch


PACKAGE_DIR = Path(__file__).resolve().parent
if str(PACKAGE_DIR) not in sys.path:
    sys.path.insert(0, str(PACKAGE_DIR))


def load_module(filename: str, name: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, PACKAGE_DIR / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


builder = load_module("build-macos-investor-app.py", "macos_app_builder")
bundle = load_module("macos-investor-bundle.py", "macos_investor_bundle")


class RunnerContractTests(unittest.TestCase):
    def _write_executable(self, path: Path, contents: str) -> Path:
        path.write_text(contents, encoding="utf-8")
        path.chmod(0o755)
        return path

    def _direct_runner(self, root: Path, *, wrapper: bool = False, rustdoc: bool = True) -> tuple[Path, dict[str, str]]:
        cargo = self._write_executable(root / "cargo", "#!/bin/sh\nexit 0\n")
        rustc = self._write_executable(root / "rustc", "#!/bin/sh\nexit 0\n")
        rustdoc_path = self._write_executable(root / "rustdoc", "#!/bin/sh\nexit 0\n")
        wrapper_path = self._write_executable(root / "rustc-wrapper", "#!/bin/sh\nexit 0\n")
        snapshot = root / "environment.sh"
        values = {"PATH": "/usr/bin:/bin", "RUSTC": str(rustc), "CARGO_BUILD_JOBS": "1"}
        if rustdoc:
            values["RUSTDOC"] = str(rustdoc_path)
        if wrapper:
            values["RUSTC_WRAPPER"] = str(wrapper_path)
        snapshot.write_text(
            "".join(f"export {name}={shlex.quote(value)}\n" for name, value in values.items()),
            encoding="utf-8",
        )
        runner = root / "direct-runner.sh"
        runner.write_text(
            "#!/bin/sh\nset -eu\n. "
            + str(snapshot)
            + "\n"
            + ("unset RUSTC_WRAPPER\n" if not wrapper else "")
            + f'exec {cargo} "$@"\n',
            encoding="utf-8",
        )
        return runner, values

    def test_literal_environment_snapshot_decodes_escaped_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = Path(temporary) / "environment.sh"
            snapshot.write_text("export PATH='/nix/store/a path/bin:/usr/bin'\n", encoding="utf-8")
            self.assertEqual(
                builder._static_environment(snapshot),
                {"PATH": "/nix/store/a path/bin:/usr/bin"},
            )

    def test_literal_environment_rejects_shell_hook_and_expansion(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = Path(temporary) / "environment.sh"
            for contents in (
                "eval \"$shellHook\"\n",
                "export PATH=\"$HOME/bin\"\n",
            ):
                snapshot.write_text(contents, encoding="utf-8")
                with self.subTest(contents=contents), self.assertRaises(builder.BuildError):
                    builder._static_environment(snapshot)

    def test_direct_runner_accepts_missing_wrapper_and_pins_three_tools(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runner, _ = self._direct_runner(Path(temporary))
            identity = builder.inspect_runner(runner, builder.sha256(runner))
            self.assertEqual(identity["execution_kind"], "direct-cargo")
            assets = identity["referenced_asset_sha256"]
            self.assertIn("runner.tool.RUSTC", assets)
            self.assertIn("runner.tool.RUSTDOC", assets)
            self.assertIn("runner.exec", assets)
            self.assertNotIn("runner.tool.RUSTC_WRAPPER", assets)

    def test_direct_runner_rejects_unbounded_cargo_jobs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runner, _ = self._direct_runner(Path(temporary))
            snapshot = Path(temporary) / "environment.sh"
            snapshot.write_text(snapshot.read_text(encoding="utf-8").replace("CARGO_BUILD_JOBS=1", "CARGO_BUILD_JOBS=8"), encoding="utf-8")
            with self.assertRaises(builder.BuildError):
                builder.inspect_runner(runner, builder.sha256(runner))

    def test_direct_runner_pins_selected_wrapper_and_requires_rustdoc(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runner, _ = self._direct_runner(Path(temporary), wrapper=True)
            identity = builder.inspect_runner(runner, builder.sha256(runner))
            self.assertIn("runner.tool.RUSTC_WRAPPER", identity["referenced_asset_sha256"])
        with tempfile.TemporaryDirectory() as temporary:
            runner, _ = self._direct_runner(Path(temporary), rustdoc=False)
            with self.assertRaises(builder.BuildError):
                builder.inspect_runner(runner, builder.sha256(runner))

    def test_direct_cargo_line_parser_streams_only_rendered_diagnostics_and_artifacts(self) -> None:
        rendered, artifact = builder._parse_direct_cargo_line(
            b'{"reason":"compiler-message","message":{"rendered":"error: sample\\n"}}\n'
        )
        self.assertEqual(rendered, "error: sample\n")
        self.assertIsNone(artifact)
        rendered, artifact = builder._parse_direct_cargo_line(
            b'{"reason":"compiler-artifact","target":{"name":"backend-mcp"},'
            b'"executable":"/work/target/backend-mcp"}\n'
        )
        self.assertIsNone(rendered)
        self.assertEqual(artifact, ("backend-mcp", "/work/target/backend-mcp"))
        rendered, artifact = builder._parse_direct_cargo_line(b"native linker diagnostic\n")
        self.assertEqual(rendered, "native linker diagnostic\n")
        self.assertIsNone(artifact)

    def test_direct_environment_excludes_hostile_ambient_rust_overrides(self) -> None:
        runner = {
            "_environment": {
                "PATH": "/nix/store/bin",
                "CARGO_BUILD_JOBS": "1",
                "RUSTC": "/nix/store/rustc/bin/rustc",
                "RUSTDOC": "/nix/store/rustc/bin/rustdoc",
            },
            "_tool_paths": {
                "rustc": "/nix/store/rustc/bin/rustc",
                "rustdoc": "/nix/store/rustc/bin/rustdoc",
            },
        }
        with patch.dict(
            os.environ,
            {
                "HOME": "/Users/operator",
                "TMPDIR": "/private/tmp",
                "RUSTFLAGS": "-C target-cpu=native",
                "RUSTC_WRAPPER": "/tmp/unpinned-wrapper",
                "CARGO_PROFILE_RELEASE_LTO": "true",
                "CARGO_HOME": "/tmp/unpinned-cargo-home",
            },
            clear=True,
        ):
            environment = builder._direct_build_environment(
                runner, Path("/source"), Path("/source/.local/target")
            )
        self.assertNotIn("RUSTFLAGS", environment)
        self.assertNotIn("RUSTC_WRAPPER", environment)
        self.assertNotIn("CARGO_PROFILE_RELEASE_LTO", environment)
        self.assertEqual(environment["CARGO_HOME"], "/Users/operator/.cargo")
        self.assertEqual(environment["CARGO_BUILD_JOBS"], "1")

    def test_existing_wrapper_runner_keeps_dynamic_environment_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env_script = root / "development.sh"
            env_script.write_text('export PATH="$HOME/bin"\n', encoding="utf-8")
            rustc = self._write_executable(root / "rustc", "#!/bin/sh\nexit 0\n")
            wrapper = self._write_executable(root / "rustc-wrapper", "#!/bin/sh\nexit 0\n")
            cargo_wrapper = self._write_executable(root / "cargo-wrapped", "#!/bin/sh\nexit 0\n")
            runner = root / "wrapper-runner.sh"
            runner.write_text(
                f"#!/bin/sh\nsource {env_script}\nexport RUSTC={rustc}\n"
                f"export RUSTC_WRAPPER={wrapper}\nexec {cargo_wrapper} \"$@\"\n",
                encoding="utf-8",
            )
            identity = builder.inspect_runner(runner, builder.sha256(runner))
            self.assertEqual(identity["execution_kind"], "wrapper")
            self.assertIn("runner.tool.RUSTC_WRAPPER", identity["referenced_asset_sha256"])


class DirectProvenanceContractTests(unittest.TestCase):
    def _valid_provenance(self) -> tuple[dict[str, Any], dict[str, str], dict[str, Any]]:
        target = "aarch64-apple-darwin"
        source = {
            "git_revision": "1" * 40,
            "git_tree": "2" * 40,
            "cargo_lock_sha256": "3" * 64,
            "working_tree": "clean",
        }
        assets = {
            "runner": "4" * 64,
            "runner.exec": "5" * 64,
            "runner.tool.CARGO": "5" * 64,
            "runner.tool.RUSTC": "6" * 64,
            "runner.tool.RUSTDOC": "7" * 64,
            "runner.environment.snapshot": "8" * 64,
        }
        runner = {
            "sha256": assets["runner"],
            "environment_sha256": assets["runner.environment.snapshot"],
            "environment_variables": ["CARGO_BUILD_JOBS", "PATH", "RUSTC", "RUSTDOC"],
            "effective_environment_sha256": "a" * 64,
            "effective_environment_variables": [
                "CARGO_BUILD_JOBS", "CARGO_HOME", "CARGO_TARGET_DIR", "HOME", "PATH", "PWD", "RUSTC", "RUSTDOC", "TMPDIR"
            ],
            "referenced_asset_sha256": assets,
        }
        source_fingerprint = hashlib.sha256(
            json.dumps(source, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        provenance = {
            "schema": 3,
            "kind": "direct-cargo",
            "run_id": "c" * 32,
            "git_head": source["git_revision"],
            "git_tree": source["git_tree"],
            "git_head_after": source["git_revision"],
            "git_tree_after": source["git_tree"],
            "source_fingerprint_sha256": source_fingerprint,
            "source_fingerprint_sha256_after": source_fingerprint,
            "source_unchanged": True,
            "cargo_lock_sha256": source["cargo_lock_sha256"],
            "cargo_exit_status": 0,
            "cargo_child_pid": 4242,
            "cargo_output_log": {
                "path": f"direct-{'c' * 32}.cargo.log",
                "sha256": "d" * 64,
                "size_bytes": 4096,
            },
            "features": {
                "features": [],
                "all_features": False,
                "no_default_features": False,
                "targets": [target],
            },
            "target": target,
            "profile": "release",
            "locked": True,
            "started_at_utc": "2026-10-04T12:00:00+00:00",
            "finished_at_utc": "2026-10-04T12:01:00+00:00",
            "elapsed_ns": 60_000_000_000,
            "command_sha256": "b" * 64,
            "runner_sha256": runner["sha256"],
            "runner_sha256_after": runner["sha256"],
            "runner_asset_sha256": assets,
            "runner_asset_sha256_after": assets,
            "environment_snapshot_sha256": runner["environment_sha256"],
            "environment_variables": runner["environment_variables"],
            "effective_environment_sha256": runner["effective_environment_sha256"],
            "effective_environment_variables": runner["effective_environment_variables"],
            "command": [
                "$CARGO", "build", "--locked", "--manifest-path", "$SOURCE_ROOT/Cargo.toml",
                "--target-dir", "$SOURCE_ROOT/.local/target", "--target", target,
                "--profile", "release", "-p", "backend-desktop", "-p", "backend-mcp",
                "-p", "backend-locald", "--message-format=json-render-diagnostics",
            ],
            "toolchain": {
                "cargo": {"version": "cargo 1.97.1", "sha256": assets["runner.exec"]},
                "rustc": {"version": "rustc 1.97.1", "sha256": assets["runner.tool.RUSTC"]},
                "rustdoc": {"version": "rustdoc 1.97.1", "sha256": assets["runner.tool.RUSTDOC"]},
            },
            "toolchain_after": {
                "cargo": {"version": "cargo 1.97.1", "sha256": assets["runner.exec"]},
                "rustc": {"version": "rustc 1.97.1", "sha256": assets["runner.tool.RUSTC"]},
                "rustdoc": {"version": "rustdoc 1.97.1", "sha256": assets["runner.tool.RUSTDOC"]},
            },
            "toolchain_unchanged": True,
            "rustc_wrapper_sha256": None,
            "outputs": [
                {"path": f"{target}/release/{name}", "sha256": "9" * 64, "size_bytes": 1}
                for name in ("backend-desktop", "backend-mcp", "backend-locald")
            ],
        }
        return provenance, source, runner

    def test_schema_three_contract_binds_runner_source_and_all_outputs(self) -> None:
        provenance, source, runner = self._valid_provenance()
        outputs = bundle.validate_direct_cargo_provenance(provenance, source, "aarch64-apple-darwin", runner)
        self.assertEqual(len(outputs), 3)
        self.assertEqual(
            set(outputs),
            {
                "aarch64-apple-darwin/release/backend-desktop",
                "aarch64-apple-darwin/release/backend-mcp",
                "aarch64-apple-darwin/release/backend-locald",
            },
        )

    def test_schema_three_rejects_wrong_kind_source_runner_or_missing_output(self) -> None:
        provenance, source, runner = self._valid_provenance()
        mutations = (
            (lambda value: value.update(kind="wrapper"), source, runner),
            (lambda value: value.update(source_fingerprint_sha256="a" * 64), source, runner),
            (lambda value: value.update(runner_sha256="b" * 64), source, runner),
            (lambda value: value["outputs"].pop(), source, runner),
            (lambda value: value.pop("cargo_child_pid"), source, runner),
            (lambda value: value["cargo_output_log"].update(sha256="z" * 64), source, runner),
            (lambda value: value["cargo_output_log"].update(path="unrelated.log"), source, runner),
        )
        for mutate, test_source, test_runner in mutations:
            candidate = json.loads(json.dumps(provenance))
            mutate(candidate)
            with self.subTest(mutation=mutate), self.assertRaises(bundle.PackageError):
                bundle.validate_direct_cargo_provenance(candidate, test_source, "aarch64-apple-darwin", test_runner)

    def test_schema_three_optional_wrapper_is_bound_to_runner_and_toolchain(self) -> None:
        provenance, source, runner = self._valid_provenance()
        wrapper_digest = "a" * 64
        runner["referenced_asset_sha256"]["runner.tool.RUSTC_WRAPPER"] = wrapper_digest
        runner["environment_variables"].append("RUSTC_WRAPPER")
        runner["environment_variables"].sort()
        provenance["runner_asset_sha256"] = runner["referenced_asset_sha256"]
        provenance["environment_variables"] = runner["environment_variables"]
        provenance["toolchain"]["rustc_wrapper"] = {"version": "sccache 0.10.0", "sha256": wrapper_digest}
        provenance["toolchain_after"]["rustc_wrapper"] = {
            "version": "sccache 0.10.0",
            "sha256": wrapper_digest,
        }
        provenance["rustc_wrapper_sha256"] = wrapper_digest
        bundle.validate_direct_cargo_provenance(provenance, source, "aarch64-apple-darwin", runner)
        provenance["rustc_wrapper_sha256"] = "b" * 64
        with self.assertRaises(bundle.PackageError):
            bundle.validate_direct_cargo_provenance(provenance, source, "aarch64-apple-darwin", runner)


if __name__ == "__main__":
    unittest.main()
