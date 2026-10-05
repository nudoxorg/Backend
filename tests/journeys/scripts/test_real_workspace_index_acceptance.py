#!/usr/bin/env python3
"""Source/parser and generic process-control tests for the real-project runner.

These tests exercise only the Python harness and disposable standard-library
child processes. They do not launch backend binaries or make an acceptance claim.
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT_DIRECTORY = Path(__file__).resolve().parent
REPOSITORY = SCRIPT_DIRECTORY.parents[2]
sys.path.insert(0, str(SCRIPT_DIRECTORY))


def load_runner():
    path = SCRIPT_DIRECTORY / "run-real-workspace-index-acceptance.py"
    spec = importlib.util.spec_from_file_location("workspace_index_acceptance", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load the acceptance script")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


runner = load_runner()
import runtime_build_receipt as receipt  # noqa: E402

producer_spec = importlib.util.spec_from_file_location(
    "record_runtime_build_manifest",
    SCRIPT_DIRECTORY / "record-runtime-build-manifest.py",
)
if producer_spec is None or producer_spec.loader is None:
    raise RuntimeError("could not load the build receipt producer")
producer = importlib.util.module_from_spec(producer_spec)
sys.modules[producer_spec.name] = producer
producer_spec.loader.exec_module(producer)


class SourceContractTests(unittest.TestCase):
    def test_checked_in_language_and_profile_contracts_are_complete(self) -> None:
        extensions, roles = runner.parse_language_contract(REPOSITORY)
        profiles = runner.parse_semantic_profile_codes(REPOSITORY)
        self.assertEqual(set(runner.PROFILE_LANGUAGE_VARIANT), set(profiles))
        self.assertIn(".rs", extensions)
        self.assertIn("NUDOX_RUSTC", roles)
        self.assertNotIn("NUDOX_DATA_ROOT", roles)
        self.assertEqual(profiles["javascript"], profiles["typescript"])

    def test_source_capacity_contract_reports_independent_limits(self) -> None:
        capacity = runner.source_capacity_contract(REPOSITORY)
        self.assertGreater(capacity["project_file_record_maximum"], 0)
        self.assertGreater(capacity["project_row_value_maximum_bytes"], 0)
        self.assertGreater(capacity["project_frontier_file_maximum"], 0)
        self.assertGreater(capacity["compiler_workspace_build_charge_maximum_bytes"], 0)
        self.assertGreater(capacity["compiler_workspace_total_file_maximum_bytes"], 0)

    def test_source_file_inventory_hashes_in_bounded_stream_reads(self) -> None:
        payload = b"actual source bytes\n" * 7000
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.rs"
            path.write_bytes(payload)
            size, digest, info = runner.read_source_file(path, runner.Deadline(5))
            self.assertEqual(size, len(payload))
            self.assertEqual(digest, hashlib.sha256(payload).digest())
            self.assertEqual(info.st_size, len(payload))

            linked = Path(directory) / "linked.rs"
            linked.symlink_to(path)
            with self.assertRaises(runner.AcceptanceError):
                runner.read_source_file(linked, runner.Deadline(5))

    def test_macho_architecture_parser_rejects_invalid_shapes(self) -> None:
        receipt.verify_architecture_parser_fixtures()

    def test_build_receipt_rejects_a_foreign_manifest_override(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cargo = Path(directory) / "cargo"
            cargo.write_bytes(b"stub")
            cargo.chmod(0o700)
            runner_path = Path(directory) / "pinned-wrapper.sh"
            runner_path.write_text("#!/bin/sh\nexec cargo-wrapped \"$@\"\n", encoding="utf-8")
            runner_path.chmod(0o700)
            runner_name = str(runner_path.resolve())
            cargo_name = str(cargo.resolve())
            direct = [runner_name, "build", "--locked", "--workspace"]
            self.assertTrue(receipt._is_build_command(direct, cargo_name, runner_name))
            bash = Path("/bin/bash").resolve(strict=True)
            interpreted = [str(bash), runner_name, "build", "--locked", "--workspace"]
            self.assertTrue(
                receipt._is_build_command(interpreted, cargo_name, runner_name)
            )
            self.assertFalse(
                receipt._is_build_command(
                    [
                        str(bash),
                        runner_name,
                        "build",
                        "--locked",
                        "--workspace",
                        "--manifest-path",
                        "/other/Cargo.toml",
                    ],
                    cargo_name,
                    runner_name,
                )
            )
            for selector in ("--exclude=backend-mcp", "--bin=backend-cli", "--lib"):
                self.assertFalse(
                    receipt._is_build_command(
                        [str(bash), runner_name, "build", "--locked", "--workspace", selector],
                        cargo_name,
                        runner_name,
                    ),
                    selector,
                )
            self.assertFalse(
                receipt._is_build_command(
                    [str(bash), "-c", "exec cargo-wrapped build --locked --workspace"],
                    cargo_name,
                    runner_name,
                ),
                "shell command evaluation must not stand in for the pinned runner",
            )

    def test_cli_surface_call_does_not_use_health_only_passive_flag(self) -> None:
        source = (REPOSITORY / "apps/cli/src/options.rs").read_text(encoding="utf-8")
        guard = source.index('"--passive"')
        self.assertIn('grammar.name() == "health"', source[guard : guard + 700])
        runner_source = (SCRIPT_DIRECTORY / "run-real-workspace-index-acceptance.py").read_text(
            encoding="utf-8"
        )
        cli_call = runner_source.split("def cli_call(", 1)[1].split("def mcp_call(", 1)[0]
        self.assertNotIn('"--passive"', cli_call)

    def test_output_inside_real_project_is_refused_before_directory_creation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / "real-project"
            project.mkdir()
            manifest_path = root / "corpus.json"
            manifest_path.write_bytes(
                runner.canonical_json(
                    {
                        "schema": runner.MANIFEST_SCHEMA,
                        "projects": [{"path": str(project)}],
                    }
                )
            )
            inside = project / "acceptance-output"
            with self.assertRaises(runner.Blocked):
                runner.preflight_output_disjoint_from_corpus(inside, manifest_path)
            self.assertFalse(inside.exists())

            outside = root / "acceptance-output"
            runner.preflight_output_disjoint_from_corpus(outside, manifest_path)
            self.assertFalse(outside.exists())


class BoundedCaptureTests(unittest.TestCase):
    def test_owner_stream_capture_retains_bounded_head_tail_and_full_hash(self) -> None:
        payload = bytes(range(256)) * 80
        capture = runner.BoundedCapture()
        capture.drain(io.BytesIO(payload))
        evidence = capture.evidence()
        self.assertEqual(evidence["bytes"], len(payload))
        self.assertEqual(evidence["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertTrue(evidence["truncated"])
        self.assertEqual(len(evidence["prefix_base64"]), 4 * ((4096 + 2) // 3))

    def test_client_evidence_has_a_byte_limit_as_well_as_an_item_limit(self) -> None:
        evidence = runner.ClientEvidence()
        item = {"label": "one", "detail": "bounded"}
        encoded_size = len(runner.canonical_json(item))
        with patch.object(runner, "MAX_CLIENT_EVIDENCE_BYTES", encoded_size):
            runner.append_client_evidence(evidence, item)
            with self.assertRaises(runner.Blocked):
                runner.append_client_evidence(evidence, item)
        self.assertEqual(len(evidence), 1)
        self.assertEqual(evidence.retained_bytes, encoded_size)

    def test_client_payload_evidence_keeps_hash_and_bounded_head_tail(self) -> None:
        payload = b"head" + b"x" * 4096 + b"tail"
        item = runner.bounded_client_payload_evidence(payload)
        self.assertEqual(item["bytes"], len(payload))
        self.assertEqual(item["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertTrue(item["truncated"])
        self.assertLessEqual(len(item["prefix_base64"]), 4 * 1024)
        self.assertLessEqual(len(item["tail_base64"]), 4 * 1024)

    def test_client_environment_refuses_to_launch_a_replacement_owner(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            absent = Path(directory) / "missing-locald"
            environment = {"BACKEND_LOCALD_BIN": str(absent)}
            runner.require_owner_spawn_disabled(environment, "test client")
            absent.write_bytes(b"must never run")
            with self.assertRaises(runner.Blocked):
                runner.require_owner_spawn_disabled(environment, "test client")
            absent.unlink()
            absent.symlink_to(Path(directory) / "other")
            with self.assertRaises(runner.Blocked):
                runner.require_owner_spawn_disabled(environment, "test client")


class BoundedProcessTests(unittest.TestCase):
    def test_build_capture_executes_the_exact_runner_argv(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "argv"
            runner_path = root / "runner"
            runner_path.write_text(
                "#!/bin/sh\n"
                f"printf '%s\\0' \"$@\" > {str(marker)!r}\n",
                encoding="utf-8",
            )
            runner_path.chmod(0o700)
            command = [str(runner_path), "build", "--locked", "--workspace"]
            exit_code, clean = producer.execute_build(command, root)
            self.assertEqual(exit_code, 0)
            self.assertTrue(clean)
            self.assertEqual(
                marker.read_bytes().split(b"\0")[:-1],
                [b"build", b"--locked", b"--workspace"],
            )

    def test_stdin_and_both_output_pipes_are_drained_concurrently(self) -> None:
        code = (
            "import sys\n"
            "while True:\n"
            " block = sys.stdin.buffer.read(4096)\n"
            " if not block: break\n"
            " sys.stdout.buffer.write(block); sys.stdout.buffer.flush()\n"
            " sys.stderr.buffer.write(block); sys.stderr.buffer.flush()\n"
        )
        payload = os.urandom(512 * 1024)
        stdout, stderr, status, _ = runner.run_bounded_process(
            [sys.executable, "-c", code],
            {"PATH": os.defpath},
            payload,
            runner.Deadline(15),
            "full-duplex-test",
        )
        self.assertEqual(status, 0)
        self.assertEqual(stdout, payload)
        self.assertEqual(stderr, payload)

    def test_deadline_terminates_the_owned_process_group(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "child-retired"
            process_code = (
                "import pathlib,signal,sys,time\n"
                f"marker = pathlib.Path({str(marker)!r})\n"
                "def stop(_signal, _frame):\n"
                " marker.write_text('retired', encoding='ascii')\n"
                " raise SystemExit(0)\n"
                "signal.signal(signal.SIGTERM, stop)\n"
                "while True: time.sleep(0.05)\n"
            )
            with self.assertRaises(runner.AcceptanceError):
                runner.run_bounded_process(
                    [sys.executable, "-c", process_code],
                    {"PATH": os.defpath},
                    None,
                    runner.Deadline(0.5),
                    "process-group-deadline-test",
                )
            self.assertTrue(marker.is_file(), "the child in the owned group received SIGTERM")
            self.assertEqual(marker.read_text(encoding="ascii"), "retired")


if __name__ == "__main__":
    unittest.main(verbosity=2)
