#!/usr/bin/env python3
"""Source/parser and generic process-control tests for the real-project runner.

These tests exercise only the Python harness and disposable standard-library
child processes. They do not launch backend binaries or make an acceptance claim.
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import tarfile
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
        self.assertGreater(capacity["project_frontier_file_conservative_maximum"], 0)
        absolute = capacity["project_frontier_absolute_inline_maximum"]
        self.assertEqual(absolute, capacity["project_row_value_maximum_bytes"] // 32)
        self.assertEqual(capacity["large_project_candidate_census_minimum"], absolute + 1)
        self.assertGreater(absolute, capacity["project_frontier_file_conservative_maximum"])
        self.assertGreater(capacity["compiler_workspace_build_charge_maximum_bytes"], 0)
        self.assertGreater(capacity["compiler_workspace_total_file_maximum_bytes"], 0)


class DerivedHistoryAwaitTests(unittest.TestCase):
    def reply(self, state, selection=7, generation="generation", reason=None):
        history = {"state": state, "selection_id": [selection] * 32}
        if reason is not None:
            history["reason"] = reason
        return {"answer": "product", "heading": "semantic-versions",
                "selected_source_frontier": {"version": "fixed"},
                "records": [{"compiler_profile": [1, 0], "operand": generation,
                             "tags": ["selected", "complete", "current source input"],
                             "history_status": history}]}

    def wait(self, replies, maximum_wait=10):
        clock = [0.0]
        observations = []
        iterator = iter(replies)
        case = runner.ProjectCase("case", Path("/project"), False, 0,
                                  ({"profile": "typescript"},))
        with patch.object(runner.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(runner.time, "sleep", side_effect=lambda delay: clock.__setitem__(0, clock[0] + delay)):
            value = runner.await_selected_history(lambda: next(iterator), case,
                {"typescript": (1, 0)}, runner.Deadline(100), observations, "await",
                maximum_wait_seconds=maximum_wait)
        return value, observations

    def test_pending_publication_is_awaited_without_changing_selection(self):
        values = [self.reply("not_requested"), self.reply("deferred", reason="worker slot"),
                  self.reply("pending"), self.reply("published")]
        last, observed = self.wait(values)
        self.assertEqual(last, values[-1])
        self.assertEqual([entry["reply"] for entry in observed], values)
        self.assertEqual([entry["elapsed_seconds"] for entry in observed], [0, 1, 2, 3])

    def test_typed_refusal_is_terminal_and_retained(self):
        refused = self.reply("refused", reason="exact native image refused")
        value, observations = self.wait([self.reply("pending"), refused])
        self.assertEqual(value, refused)
        self.assertEqual(len(observations), 2)

    def test_new_selection_or_generation_does_not_count_as_old_job(self):
        for advanced in (self.reply("published", selection=8),
                         self.reply("published", generation="replacement")):
            with self.subTest(advanced=advanced), self.assertRaisesRegex(runner.AcceptanceError, "changed"):
                self.wait([self.reply("pending"), advanced])

    def test_pending_does_not_become_success_after_timeout(self):
        with self.assertRaisesRegex(runner.AcceptanceError, "remained in flight"):
            self.wait([self.reply("pending")] * 3, maximum_wait=2)

    def test_wrong_route_and_invalid_selection_are_not_polled(self):
        for invalid in ({"answer": "surface"}, self.reply("pending", selection=True)):
            with self.subTest(invalid=invalid), self.assertRaises(runner.AcceptanceError):
                self.wait([invalid])


class SnapshotToolchainAdmissionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.extensions = {"javascript": ".js", "typescript": ".ts", "tsx": ".tsx"}
        self.extension_languages = {
            extension: "TypeScript" for extension in self.extensions.values()
        }

    def executable(self, name: str) -> str:
        path = self.root / name
        path.write_text("static toolchain admission fixture\n", encoding="utf-8")
        path.chmod(0o755)
        return str(path)

    def regular_file(self, name: str) -> str:
        path = self.root / name
        path.write_text("static path-kind fixture\n", encoding="utf-8")
        path.chmod(0o644)
        return str(path)

    def directory(self, name: str) -> str:
        path = self.root / name
        path.mkdir()
        return str(path)

    def admit(self, selected: dict[str, str], profile: str) -> dict[str, list[str]]:
        extension = self.extensions[profile]
        case = runner.ProjectCase(
            project_id="typescript-fixture",
            path=self.root,
            large=False,
            min_candidates=0,
            symbols=(
                {
                    "profile": profile,
                    "path": f"src/app{extension}",
                    "name": "app",
                },
            ),
        )
        return runner.validate_snapshot_toolchains(
            selected, [case], self.extension_languages
        )

    def test_typescript_authority_requires_compiler_and_one_complete_program_route(self) -> None:
        tsc = self.executable("tsc")
        report = self.executable("typescript-report")
        node = self.executable("node")
        module_root = self.directory("node_modules")

        accepted = (
            {"NUDOX_TSC": tsc, "NUDOX_TYPESCRIPT_REPORT_PROGRAM": report},
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_NODE": node,
                "NUDOX_TYPESCRIPT_MODULE_ROOT": module_root,
            },
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": report,
                "NUDOX_TYPESCRIPT_NODE": node,
                "NUDOX_TYPESCRIPT_MODULE_ROOT": module_root,
            },
        )
        for selected in accepted:
            for profile in self.extensions:
                with self.subTest(profile=profile, selected=tuple(sorted(selected))):
                    self.assertEqual(self.admit(selected, profile), {profile: []})

        incomplete = (
            {"NUDOX_TSC": tsc},
            {"NUDOX_TYPESCRIPT_REPORT_PROGRAM": report},
            {"NUDOX_TYPESCRIPT_NODE": node},
            {"NUDOX_TYPESCRIPT_MODULE_ROOT": module_root},
            {"NUDOX_TYPESCRIPT_NODE": node, "NUDOX_TYPESCRIPT_MODULE_ROOT": module_root},
            {"NUDOX_TSC": tsc, "NUDOX_TYPESCRIPT_NODE": node},
            {"NUDOX_TSC": tsc, "NUDOX_TYPESCRIPT_MODULE_ROOT": module_root},
        )
        for selected in incomplete:
            for profile in self.extensions:
                with self.subTest(profile=profile, selected=tuple(sorted(selected))):
                    with self.assertRaises(runner.Blocked):
                        self.admit(selected, profile)

    def test_selected_typescript_roles_enforce_executable_file_and_directory_kinds(self) -> None:
        tsc = self.executable("tsc")
        report = self.executable("typescript-report")
        node = self.executable("node")
        module_root = self.directory("node_modules")
        invalid = (
            {
                "NUDOX_TSC": self.regular_file("not-executable"),
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": report,
            },
            {
                "NUDOX_TSC": self.directory("tsc-directory"),
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": report,
            },
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": self.regular_file("report-file"),
            },
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": self.directory("report-directory"),
            },
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_NODE": self.directory("node-directory"),
                "NUDOX_TYPESCRIPT_MODULE_ROOT": module_root,
            },
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_NODE": node,
                "NUDOX_TYPESCRIPT_MODULE_ROOT": self.regular_file("module-root-file"),
            },
            {
                "NUDOX_TSC": tsc,
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": report,
                "NUDOX_TYPESCRIPT_NODE": node,
                "NUDOX_TYPESCRIPT_MODULE_ROOT": self.regular_file("unused-invalid-root"),
            },
        )
        for selected in invalid:
            for profile in self.extensions:
                with self.subTest(profile=profile, selected=tuple(sorted(selected))):
                    with self.assertRaises(runner.Blocked):
                        self.admit(selected, profile)


class SelectedProjectFrontierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.capacity = runner.source_capacity_contract(REPOSITORY)
        self.absolute_inline_maximum = self.capacity[
            "project_frontier_absolute_inline_maximum"
        ]
        self.large_candidate_minimum = self.capacity[
            "large_project_candidate_census_minimum"
        ]
        self.case = runner.ProjectCase(
            project_id="large-fixture",
            path=Path("/real/project").resolve(),
            large=True,
            min_candidates=self.large_candidate_minimum,
            symbols=(),
        )
        self.maximum_files = 100_000

    def frontier(self, count: int, root_byte: int = 1) -> dict[str, object]:
        return {
            "package": {"kind": "local", "value": str(self.case.path)},
            "source_relation_root": [root_byte] * 32,
            "source_version": [2] * 32,
            "file_count": count,
        }

    def test_large_project_must_exceed_absolute_old_inline_upper_bound(self) -> None:
        with self.assertRaises(runner.Blocked):
            runner.assert_selected_source_frontier(
                {"selected_source_frontier": self.frontier(self.absolute_inline_maximum)},
                self.case,
                self.absolute_inline_maximum,
                self.maximum_files,
                "large accepted count",
            )
        admitted = runner.assert_selected_source_frontier(
            {
                "selected_source_frontier": self.frontier(
                    self.absolute_inline_maximum + 1
                )
            },
            self.case,
            self.absolute_inline_maximum,
            self.maximum_files,
            "large accepted count",
        )
        self.assertEqual(admitted["file_count"], self.absolute_inline_maximum + 1)

    def test_missing_or_malformed_frontier_never_counts_candidates_as_members(self) -> None:
        with self.assertRaises(runner.Blocked):
            runner.assert_selected_source_frontier(
                {"recognized_source_candidates": 33196},
                self.case,
                self.absolute_inline_maximum,
                self.maximum_files,
                "missing membership",
            )
        malformed = self.frontier(self.large_candidate_minimum)
        malformed["source_relation_root"] = [0] * 31
        with self.assertRaises(runner.AcceptanceError):
            runner.assert_selected_source_frontier(
                {"selected_source_frontier": malformed},
                self.case,
                self.absolute_inline_maximum,
                self.maximum_files,
                "malformed membership",
            )

    def test_zero_or_over_limit_membership_is_rejected(self) -> None:
        for count, expected in ((0, runner.Blocked), (100_001, runner.AcceptanceError)):
            with self.subTest(count=count), self.assertRaises(expected):
                runner.assert_selected_source_frontier(
                    {"selected_source_frontier": self.frontier(count)},
                    self.case,
                    self.absolute_inline_maximum,
                    self.maximum_files,
                    "bounded membership",
                )

    def test_cold_replay_identity_includes_exact_root_and_count(self) -> None:
        before = runner.assert_selected_source_frontier(
            {"selected_source_frontier": self.frontier(self.large_candidate_minimum)},
            self.case,
            self.absolute_inline_maximum,
            self.maximum_files,
            "before restart",
        )
        after_same = runner.assert_selected_source_frontier(
            {"selected_source_frontier": self.frontier(self.large_candidate_minimum)},
            self.case,
            self.absolute_inline_maximum,
            self.maximum_files,
            "after restart",
        )
        after_changed = runner.assert_selected_source_frontier(
            {
                "selected_source_frontier": self.frontier(
                    self.large_candidate_minimum + 1, root_byte=3
                )
            },
            self.case,
            self.absolute_inline_maximum,
            self.maximum_files,
            "changed restart",
        )
        self.assertEqual(before, after_same)
        self.assertNotEqual(before, after_changed)


class CorpusManifestCandidateFloorTests(unittest.TestCase):
    def test_pypa_src_build_package_is_admitted_but_build_artifacts_are_not(self) -> None:
        # Exact production paths from pypa/build's ProjectBuilder source witness.
        extensions, _ = runner.parse_language_contract(REPOSITORY)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / "package"
            sources = {
                "src/build/_builder.py": "class ProjectBuilder:\n    pass\n",
                "src/build/__main__.py": "builder = ProjectBuilder(source_dir)\n",
                "src/build/util.py": "builder = ProjectBuilder(source_dir)\n",
                "build/lib/build/_builder.py": "class ProjectBuilder:\n    pass\n",
                "src/build/dist/artifact.py": "class GeneratedArtifact:\n    pass\n",
            }
            for relative, content in sources.items():
                path = project / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            manifest = root / "manifest.json"
            value = {"schema": runner.MANIFEST_SCHEMA, "projects": [{
                "id": "pypa-build", "path": str(project), "large": False,
                "minimum_source_candidates": 0,
                "symbols": [{"profile": "python", "path": "src/build/_builder.py",
                             "name": "ProjectBuilder"}],
            }]}
            manifest.write_text(json.dumps(value))
            cases, _ = runner.validate_corpus_manifest(
                manifest, root / "evidence", extensions, 2046, "python")
            census = runner.project_census(project)
            self.assertEqual(census.files, 3)
            self.assertEqual(census.extension_counts, {".py": 3})
            self.assertEqual(cases[0].symbols[0]["path"], "src/build/_builder.py")
            for excluded in ["build/lib/build/_builder.py", "src/build/dist/artifact.py"]:
                value["projects"][0]["symbols"][0]["path"] = excluded
                manifest.write_text(json.dumps(value))
                with self.assertRaisesRegex(runner.Blocked, "noncanonical relative source path"):
                    runner.validate_corpus_manifest(
                        manifest, root / "evidence", extensions, 2046, "python")

    def test_language_shards_are_explicit_and_do_not_weaken_default_gates(self) -> None:
        extensions, _ = runner.parse_language_contract(REPOSITORY)
        floor = runner.source_capacity_contract(REPOSITORY)["large_project_candidate_census_minimum"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / "package"
            project.mkdir()
            output = root / "evidence"
            manifest = root / "manifest.json"
            for language, extension in [("typescript", ".ts"), ("python", ".py"), ("go", ".go")]:
                (project / ("source" + extension)).write_text("FixtureSymbol\n")
                manifest.write_text(json.dumps({
                    "schema": runner.MANIFEST_SCHEMA,
                    "projects": [{
                        "id": "package", "path": str(project), "large": False,
                        "minimum_source_candidates": 0,
                        "symbols": [{"profile": language, "path": "source" + extension,
                                     "name": "FixtureSymbol"}],
                    }],
                }))
                cases, _ = runner.validate_corpus_manifest(manifest, output, extensions, floor, language)
                self.assertEqual(len(cases), 1)
                self.assertFalse(cases[0].large)
                with self.assertRaises(runner.Blocked):
                    runner.validate_corpus_manifest(manifest, output, extensions, floor)
                with self.assertRaises(runner.Blocked):
                    runner.validate_corpus_manifest(manifest, output, extensions, floor, "unsupported")
                other = "go" if language != "go" else "python"
                with self.assertRaises(runner.Blocked):
                    runner.validate_corpus_manifest(manifest, output, extensions, floor, other)

    def test_javascript_only_shard_cannot_count_as_typescript_coverage(self) -> None:
        extensions, _ = runner.parse_language_contract(REPOSITORY)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / "package"
            project.mkdir()
            (project / "index.js").write_text("function FixtureSymbol() {}\n")
            manifest = root / "manifest.json"
            value = {
                "schema": runner.MANIFEST_SCHEMA,
                "projects": [{
                    "id": "js-package", "path": str(project), "large": False,
                    "minimum_source_candidates": 0,
                    "symbols": [{"profile": "javascript", "path": "index.js", "name": "FixtureSymbol"}],
                }],
            }
            manifest.write_text(json.dumps(value))
            with self.assertRaisesRegex(runner.Blocked, "no TypeScript symbol"):
                runner.validate_corpus_manifest(manifest, root / "evidence", extensions, 2046, "typescript")
            (project / "index.tsx").write_text("function FixtureSymbol() {}\n")
            value["projects"][0]["symbols"].append({
                "profile": "tsx", "path": "index.tsx", "name": "FixtureSymbol",
            })
            manifest.write_text(json.dumps(value))
            cases, _ = runner.validate_corpus_manifest(manifest, root / "evidence", extensions, 2046, "typescript")
            self.assertEqual(len(cases[0].symbols), 2)

    def test_large_candidate_floor_is_source_derived_and_manifest_can_raise_it(self) -> None:
        extension_languages, _ = runner.parse_language_contract(REPOSITORY)
        source_capacity = runner.source_capacity_contract(REPOSITORY)
        derived_floor = source_capacity["large_project_candidate_census_minimum"]
        profiles = runner.PROFILE_LANGUAGE_VARIANT

        with tempfile.TemporaryDirectory() as directory:
            temporary_root = Path(directory)
            project = temporary_root / "actual-shaped-project"
            project.mkdir()
            evidence = temporary_root / "evidence"
            evidence.mkdir()
            symbols: list[dict[str, str]] = []
            for profile in profiles:
                language, _ = profiles[profile]
                extension = next(
                    ext
                    for ext in runner.PROFILE_EXTENSIONS[profile]
                    if extension_languages.get(ext) == language
                )
                relative = f"src/fixture-{profile}{extension}"
                source_file = project / relative
                source_file.parent.mkdir(parents=True, exist_ok=True)
                source_file.write_text("fixture source\n", encoding="utf-8")
                symbols.append(
                    {"profile": profile, "path": relative, "name": "FixtureSymbol"}
                )

            manifest_path = temporary_root / "corpus.json"
            manifest = {
                "schema": runner.MANIFEST_SCHEMA,
                "projects": [
                    {
                        "id": "large-fixture",
                        "path": str(project),
                        "large": True,
                        "minimum_source_candidates": 0,
                        "symbols": symbols,
                    }
                ],
            }
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            cases, _ = runner.validate_corpus_manifest(
                manifest_path,
                evidence,
                extension_languages,
                derived_floor,
            )
            self.assertEqual(cases[0].min_candidates, derived_floor)
            self.assertEqual(
                derived_floor,
                source_capacity["project_frontier_absolute_inline_maximum"] + 1,
            )

            with self.assertRaises(runner.Blocked):
                runner.validate_corpus_manifest(
                    manifest_path,
                    evidence,
                    extension_languages,
                    0,
                )

            manifest["projects"][0]["minimum_source_candidates"] = derived_floor + 189
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            raised_cases, _ = runner.validate_corpus_manifest(
                manifest_path,
                evidence,
                extension_languages,
                derived_floor,
            )
            self.assertEqual(raised_cases[0].min_candidates, derived_floor + 189)


class HarnessContractTests(unittest.TestCase):
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

    def test_output_inside_source_checkout_is_refused_before_directory_creation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "checkout"
            source.mkdir()
            inside = source / "acceptance-output"
            with self.assertRaises(runner.Blocked):
                runner.preflight_output_disjoint_from_source(inside, source)
            self.assertFalse(inside.exists())

            outside = Path(directory) / "acceptance-output"
            runner.preflight_output_disjoint_from_source(outside, source)
            self.assertFalse(outside.exists())


class RuntimeToolFileAdmissionTests(unittest.TestCase):
    def test_installed_immutable_nix_shell_hardlink_is_admitted(self) -> None:
        candidates = Path("/nix/store").glob("*-bash-*/bin/bash")
        shell = next(
            (path for path in candidates if not path.is_symlink() and path.stat().st_nlink > 1),
            None,
        )
        if shell is None:
            self.skipTest("no installed immutable hardlinked Nix shell is available")
        identity = receipt.stable_interpreter_file(shell)
        self.assertEqual(identity["path"], str(shell))
        self.assertEqual(identity["sha256"], hashlib.sha256(shell.read_bytes()).hexdigest())
        self.assertGreater(identity["bytes"], 0)
        with self.assertRaises(receipt.ReceiptError):
            receipt.stable_file(shell, "pinned runner interpreter", executable=True)
        with self.assertRaises(receipt.ReceiptError):
            receipt.stable_tool_file(shell, "bash")

    def test_shell_admission_preserves_hardlink_path_and_name_rejections(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "shell-source"
            source.write_bytes(b"not an immutable shell")
            source.chmod(0o555)
            owned = root / "bash"
            os.link(source, owned)
            fake_bin = root / "nix" / "store" / ("0" * 32 + "-bash-5.3") / "bin"
            fake_bin.mkdir(parents=True)
            fake = fake_bin / "bash"
            os.link(source, fake)
            alias = root / "sh"
            alias.symlink_to(owned)
            for path in (owned, fake, alias, root / "python3"):
                with self.subTest(path=str(path)):
                    with self.assertRaises(receipt.ReceiptError):
                        receipt.stable_interpreter_file(path)

    def test_installed_immutable_nix_cargo_and_rustc_hardlinks_are_admitted(self) -> None:
        store = Path("/nix/store")
        if not store.is_dir():
            self.skipTest("this host has no Nix store")

        hardlinked_pair = None
        for cargo in store.glob("*-rust-*-with-components-*/bin/cargo"):
            rustc = cargo.with_name("rustc")
            try:
                cargo_info = cargo.lstat()
                rustc_info = rustc.lstat()
            except OSError:
                continue
            if cargo_info.st_nlink > 1 and rustc_info.st_nlink > 1:
                hardlinked_pair = (cargo, rustc)
                break
        if hardlinked_pair is None:
            self.skipTest("no installed hardlinked Nix Cargo/rustc pair is available")

        for path, label in zip(hardlinked_pair, ("Cargo", "rustc"), strict=True):
            with self.subTest(tool=label, path=str(path)):
                identity = receipt.stable_tool_file(path, label)
                self.assertEqual(identity["path"], str(path))
                self.assertEqual(len(identity["sha256"]), 64)
                self.assertGreater(identity["bytes"], 0)
                self.assertGreater(path.lstat().st_nlink, 1)
                with self.assertRaises(receipt.ReceiptError):
                    receipt.stable_file(path, label, executable=True)
                with self.assertRaises(receipt.ReceiptError):
                    receipt.stable_tool_file(path, "backend-cli")

    def test_user_owned_hardlinked_tools_are_rejected_even_when_read_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for mode in (0o755, 0o555):
                with self.subTest(mode=oct(mode)):
                    source = root / f"cargo-{mode:o}"
                    linked = root / f"cargo-linked-{mode:o}"
                    source.write_bytes(b"executable fixture")
                    source.chmod(mode)
                    os.link(source, linked)
                    self.assertEqual(source.lstat().st_uid, os.geteuid())
                    self.assertEqual(source.lstat().st_nlink, 2)
                    with self.assertRaises(receipt.ReceiptError):
                        receipt.stable_tool_file(linked, "Cargo")

    def test_fake_nix_shaped_path_does_not_gain_the_hardlink_exception(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fake_store = Path(directory) / "nix" / "store"
            fake_package = fake_store / ("0" * 32 + "-rust-1.97.1")
            fake_bin = fake_package / "bin"
            fake_bin.mkdir(parents=True)
            cargo = fake_bin / "cargo"
            cargo.write_bytes(b"not a Nix store executable")
            cargo.chmod(0o555)
            linked = fake_bin / "cargo-copy"
            os.link(cargo, linked)
            for directory_path in (fake_store, fake_package, fake_bin):
                directory_path.chmod(0o555)
            with self.assertRaises(receipt.ReceiptError):
                receipt.stable_tool_file(cargo, "Cargo")

    def test_artifacts_receipts_and_source_lockfiles_keep_single_link_admission(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "backend-cli"
            source.write_bytes(b"artifact fixture")
            source.chmod(0o755)
            linked = root / "backend-cli-linked"
            os.link(source, linked)

            with self.assertRaises(receipt.ReceiptError):
                receipt.stable_file(linked, "backend-cli", executable=True)
            with self.assertRaises(receipt.ReceiptError):
                receipt.read_regular(linked, 1024, "build receipt")
            with self.assertRaises(receipt.ReceiptError):
                receipt.stable_file(linked, "Cargo.lock")


class PackageProvenanceMetadataTests(unittest.TestCase):
    def metadata(self, ecosystem: str, name: str) -> dict:
        return {"ecosystem": ecosystem, "id": name, "version": "1.2.3",
                "provenance": {"kind": "archive-sha256", "sha256": "a" * 64}}

    def test_language_scoped_package_identity_keeps_the_declared_artifact_hash_domain(self) -> None:
        for language, ecosystem, name in [("typescript", "npm", "@scope/package"),
                                           ("python", "pypi", "requests"),
                                           ("go", "go", "github.com/gorilla/mux")]:
            with self.subTest(language=language):
                value = self.metadata(ecosystem, name)
                self.assertEqual(runner.validate_package_metadata(value, language), value)
                value["provenance"]["kind"] = "source-tree-sha256"
                self.assertEqual(runner.validate_package_metadata(value, language), value)

    def test_declared_metadata_cannot_assert_verification_or_change_the_default_all_profile_scope(self) -> None:
        value = self.metadata("npm", "typescript")
        with self.assertRaises(runner.Blocked):
            runner.validate_package_metadata(value, None)
        with self.assertRaises(runner.Blocked):
            runner.validate_package_metadata(value, "python")
        value["provenance"]["verified"] = True
        with self.assertRaises(runner.Blocked):
            runner.validate_package_metadata(value, "typescript")

    def test_malformed_hash_and_noncanonical_package_names_are_refused(self) -> None:
        for kind, digest in [("recognized-source-census", "a" * 64),
                             ("archive-sha256", "a" * 40),
                             ("source-tree-sha256", "A" * 64)]:
            value = self.metadata("npm", "typescript")
            value["provenance"] = {"kind": kind, "sha256": digest}
            with self.subTest(provenance=value["provenance"]):
                with self.assertRaises(runner.Blocked):
                    runner.validate_package_metadata(value, "typescript")
        for language, ecosystem, name in [("typescript", "npm", "TypeScript"),
                                           ("python", "pypi", "Some_Package")]:
            with self.assertRaises(runner.Blocked):
                runner.validate_package_metadata(self.metadata(ecosystem, name), language)

    def test_manifest_retains_declared_metadata_without_relaxing_profile_admission(self) -> None:
        extensions, _ = runner.parse_language_contract(REPOSITORY)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            project = root / "package"
            project.mkdir()
            (project / "index.ts").write_text("export class Program {}\n")
            package = self.metadata("npm", "typescript")
            manifest = root / "manifest.json"
            value = {"schema": runner.MANIFEST_SCHEMA, "projects": [{
                "id": "typescript", "path": str(project), "large": False,
                "minimum_source_candidates": 0, "package": package,
                "symbols": [{"profile": "typescript", "path": "index.ts", "name": "Program"}],
            }]}
            manifest.write_text(json.dumps(value))
            cases, raw = runner.validate_corpus_manifest(
                manifest, root / "evidence", extensions, 2046, "typescript")
            self.assertEqual(cases[0].package, package)
            self.assertEqual(raw, manifest.read_bytes())
            with self.assertRaises(runner.Blocked):
                runner.validate_corpus_manifest(manifest, root / "evidence", extensions, 2046)
            value["projects"][0].pop("package")
            manifest.write_text(json.dumps(value))
            with self.assertRaisesRegex(runner.Blocked, "missing supported-profile"):
                runner.validate_corpus_manifest(manifest, root / "evidence", extensions, 2046)


class RuntimeSetupBoundaryTests(unittest.TestCase):
    def test_stock_environment_omits_ambient_and_snapshot_compiler_pins(self) -> None:
        with patch.dict(os.environ, {"PATH": "/usr/bin:/bin", "NUDOX_TSC": "/hidden/tsc",
                                    "BACKEND_LOCALD_COMPILER_ENVIRONMENT": "ambient"}, clear=True):
            stock, digest = runner.minimal_environment(
                compiler_snapshot="closed-snapshot", compiler_key="BACKEND_LOCALD_COMPILER_ENVIRONMENT",
                setup_mode="stock", temp_root=Path("/owned/tmp"))
        proof = runner.runtime_setup_receipt("stock", stock, digest,
                                            "BACKEND_LOCALD_COMPILER_ENVIRONMENT", ["NUDOX_TSC"])
        self.assertEqual(proof, {"mode": "stock", "compiler_snapshot_injected": False,
                                 "compiler_override_keys": [], "owner_environment_sha256": digest})
        self.assertEqual(stock, {"PATH": "/usr/bin:/bin", "TMPDIR": "/owned/tmp"})
        stock["NUDOX_TSC"] = "/hidden/tsc"
        with self.assertRaises(runner.AcceptanceError):
            runner.runtime_setup_receipt("stock", stock, digest,
                                         "BACKEND_LOCALD_COMPILER_ENVIRONMENT", ["NUDOX_TSC"])

    def test_configured_snapshot_is_explicit_and_client_spawn_guard_is_not_a_compiler_override(self) -> None:
        environment, digest = runner.minimal_environment(
            compiler_snapshot="closed-snapshot", compiler_key="BACKEND_LOCALD_COMPILER_ENVIRONMENT",
            client=True, temp_root=Path("/owned/tmp"))
        proof = runner.runtime_setup_receipt("configured", environment, digest,
                                            "BACKEND_LOCALD_COMPILER_ENVIRONMENT", ["NUDOX_TSC"])
        self.assertEqual(proof["compiler_override_keys"], ["BACKEND_LOCALD_COMPILER_ENVIRONMENT"])
        self.assertTrue(proof["compiler_snapshot_injected"])
        self.assertIn("BACKEND_LOCALD_BIN", environment)
        del environment["BACKEND_LOCALD_COMPILER_ENVIRONMENT"]
        with self.assertRaises(runner.AcceptanceError):
            runner.runtime_setup_receipt("configured", environment, digest,
                                         "BACKEND_LOCALD_COMPILER_ENVIRONMENT", ["NUDOX_TSC"])


@unittest.skipUnless(hasattr(os, "mkfifo"), "Unix descriptor boundary")
class SourceDescriptorRaceTests(unittest.TestCase):
    def test_regular_to_fifo_replacement_is_refused_without_a_blocking_open(self) -> None:
        # Both old implementations block here. The test child has its own deadline.
        code = """
import importlib.util, os, pathlib, sys
directory = pathlib.Path(sys.argv[1])
sys.path.insert(0, sys.argv[2])
spec = importlib.util.spec_from_file_location('race_runner', pathlib.Path(sys.argv[2]) / 'run-real-workspace-index-acceptance.py')
r = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = r
spec.loader.exec_module(r)
path = directory / 'source'
path.write_bytes(b'original regular source')
if sys.argv[3] == 'bounded':
    original_lstat = pathlib.Path.lstat
    def replace_after_inspection(self, *args, **kwargs):
        info = original_lstat(self, *args, **kwargs)
        if self == path and path.is_file():
            path.unlink()
            os.mkfifo(path)
        return info
    pathlib.Path.lstat = replace_after_inspection
    operation = lambda: r.read_bounded_regular(path, 1024, 'source')
else:
    assert path.is_file()
    path.unlink()
    os.mkfifo(path)
    operation = lambda: r.read_source_file(path)
try:
    operation()
except (r.Blocked, r.AcceptanceError):
    print('refused')
else:
    raise AssertionError('FIFO was accepted as regular source')
"""
        for boundary in ("bounded", "streaming"):
            with self.subTest(boundary=boundary), tempfile.TemporaryDirectory() as directory:
                result = subprocess.run([sys.executable, "-c", code, directory, str(SCRIPT_DIRECTORY), boundary],
                                        capture_output=True, timeout=2, check=True)
                self.assertEqual(result.stdout, b"refused\n")


class PairedSurfaceObligationTests(unittest.TestCase):
    def fixture(self, root: Path) -> tuple:
        source = b"class Session:\n    pass\n\ndef session():\n    return Session()\n"
        (root / "sessions.py").write_bytes(source)
        declaration = b"class Session:\n    pass"
        use_start = source.rindex(b"Session")
        contract = {"kind": "class", "source": {
            "file_sha256": hashlib.sha256(source).hexdigest(), "start": 0,
            "end": len(declaration), "slice_sha256": hashlib.sha256(declaration).hexdigest()},
            "references": [{"path": "sessions.py", "start": use_start, "end": use_start + 7,
                "file_sha256": hashlib.sha256(source).hexdigest(),
                "slice_sha256": hashlib.sha256(b"Session").hexdigest(),
                "relation": "calls", "confidence": "compiler"}],
            "graph": [{"label": "neighbor", "path": "sessions.py", "name": "session"}]}
        identity = {"coordinate": "exact-session-coordinate", "name": "Session", "path": "sessions.py",
                    "project": str(root), "key": "01234567", "line": 1, "shape": "symbol",
                    "trail": "sessions.py::Session", "segments": ["Session"]}
        records = {"answer": "records", "readiness": "ready", "coverage": [{"state": "complete"}],
                   "more": False, "records": [{"identity": identity, "kind": "class", "language": "python"}]}
        page = {"answer": "page", "identity": {**identity, "key": "89abcdef"}, "language": "python"}
        source_page = {**page, "source": {"path": "sessions.py", "line": 1,
                         "lines": ["class Session:", "    pass"], "extent": "complete"}}
        graph = {"answer": "records", "query": identity["coordinate"], "more": False,
                 "readiness": "ready", "records": [{"identity":
                    {"project": str(root), "path": "sessions.py", "name": "session"}}]}
        raw_refs = {"answer": "surface", "surface": {"result": "references", "data": {
            "target": identity["coordinate"], "references": [{"site": "exact-factory-coordinate",
                "target": {"scope": "local", "declaration": {"family": [1] * 16, "variant": [2] * 16}},
                "relation": "calls", "evidence": {"confidence": "compiler", "source": {
                    "file": "sessions.py", "start": use_start, "end": use_start + 7}}}]}}}
        read = {"answer": "surface", "surface": {"result": "read", "data": [{
            "label": identity["coordinate"], "stable_id": [3] * 32, "signature": "class Session"}]}}
        values = {"search": records, "resolve": records, "document": page, "source": source_page,
                  "references": {"answer": "product", "heading": "references", "records": []},
                  "graph": graph, "read": read, "raw-references": raw_refs}
        case = runner.ProjectCase("requests", root, False, 0, ({"profile": "python", "path": "sessions.py",
                                 "name": "Session", "surface_contract": contract},))
        return case, values, identity

    def test_paired_routes_check_source_body_required_semantics_and_cold_old_coordinate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case, values, identity = self.fixture(Path(directory).resolve())
            cli_commands, mcp_commands = [], []
            def cli(*args):
                command = args[4]
                cli_commands.append(command)
                operation = command[2] if command[0] == "--limit" else command[0]
                return values["document" if operation == "show" else operation]
            def mcp(*args):
                tool, arguments = args[4:6]
                mcp_commands.append((tool, arguments))
                if tool == "backend.surface":
                    return values["read" if arguments["command"]["operation"] == "read" else "raw-references"]
                return {**values[tool.removeprefix("backend.")], "budget": {"bytes": 1}}
            with patch.object(runner, "cli_call", side_effect=cli), patch.object(runner, "mcp_call", side_effect=mcp):
                binaries = {"backend-cli": None, "backend-mcp": None}
                warm = runner.run_surface_contracts(case, binaries, case.path, case.path / "socket", {}, runner.Deadline(1), [], "warm")
                cold = runner.run_surface_contracts(case, binaries, case.path, case.path / "socket", {}, runner.Deadline(1), [], "cold", warm)
            self.assertTrue(cold[0]["cold_old_coordinate_verified"])
            self.assertEqual(cold[0]["row_stable_id"], [3] * 32)
            self.assertEqual(cold[0]["public_key_abbreviation"], "01234567")
            self.assertEqual(cold[0]["references"]["required_sites"], 1)
            self.assertEqual(cold[0]["graph"]["required_neighbors"], 1)
            self.assertEqual(cold[0]["page_identity"]["key"], "89abcdef")
            for command in cli_commands:
                if command[0] in {"show", "source", "references", "graph"}:
                    self.assertEqual(command[1], identity["coordinate"])
            self.assertEqual(len(mcp_commands), 16)
            values["read"]["surface"]["data"][0]["stable_id"] = [4] * 32
            with patch.object(runner, "cli_call", side_effect=cli), patch.object(runner, "mcp_call", side_effect=mcp):
                with self.assertRaisesRegex(runner.AcceptanceError, "retained row or semantic evidence"):
                    runner.run_surface_contracts(case, binaries, case.path, case.path / "socket", {}, runner.Deadline(1), [], "cold", warm)

    def test_header_only_wrong_target_and_missing_graph_edges_never_satisfy_obligations(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case, values, identity = self.fixture(Path(directory).resolve())
            contract = case.symbols[0]["surface_contract"]
            witness = runner.expected_source_span(case.path, "sessions.py", contract["source"])
            values["source"]["source"]["lines"] = ["class Session:"]
            with self.assertRaisesRegex(runner.AcceptanceError, "complete declaration body"):
                runner.assert_source_body(values["source"], identity, witness, "source")
            values["raw-references"]["surface"]["data"]["target"] = "another-coordinate"
            with self.assertRaisesRegex(runner.AcceptanceError, "exact target"):
                runner.assert_reference_obligations(values["raw-references"], identity["coordinate"], contract["references"])
            values["raw-references"]["surface"]["data"]["target"] = identity["coordinate"]
            values["raw-references"]["surface"]["data"]["references"][0]["target"] = {
                "scope": "foreign", "declaration": [1] * 16, "variant": None}
            with self.assertRaisesRegex(runner.AcceptanceError, "required semantic use"):
                runner.assert_reference_obligations(values["raw-references"], identity["coordinate"], contract["references"])
            values["graph"]["records"] = []
            with self.assertRaisesRegex(runner.AcceptanceError, "required declaration edge"):
                runner.assert_graph_obligations(values["graph"], identity, contract["graph"])
            with self.assertRaisesRegex(runner.AcceptanceError, "semantic edge label"):
                runner.assert_graph_obligations(values["graph"], identity, [{**contract["graph"][0], "label": "calls"}])

    def test_empty_required_sets_remain_explicit_obligations_not_completeness_claims(self) -> None:
        raw = {"answer": "surface", "surface": {"result": "references", "data": {"target": "exact", "references": []}}}
        proof = runner.assert_reference_obligations(raw, "exact", [])
        self.assertEqual(proof["required_sites"], 0)
        self.assertIn("not the complete reference universe", proof["coverage_claim"])

    def test_changed_source_and_utf8_split_spans_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            case, _, _ = self.fixture(root)
            contract = case.symbols[0]["surface_contract"]
            (root / "sessions.py").write_text("class AnotherSession: pass\n")
            with self.assertRaises(runner.AcceptanceError):
                runner.validate_surface_contract(contract, root, "sessions.py")
            content = "é\n".encode()
            (root / "utf8.py").write_bytes(content)
            span = {"start": 1, "end": 2, "file_sha256": hashlib.sha256(content).hexdigest(),
                    "slice_sha256": hashlib.sha256(content[1:2]).hexdigest()}
            with self.assertRaisesRegex(runner.Blocked, "UTF-8 boundaries"):
                runner.expected_source_span(root, "utf8.py", span)


class AcquiredSourceInventoryTests(unittest.TestCase):
    def test_inventory_hash_domain_is_independent_of_object_field_insertion_order(self) -> None:
        left = [{"path": "é.py", "bytes": 3, "sha256": "a" * 64}]
        right = [{"sha256": "a" * 64, "path": "é.py", "bytes": 3}]
        expected = hashlib.sha256(json.dumps(left, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        self.assertEqual(runner.source_inventory_sha256(left), expected)
        self.assertEqual(runner.source_inventory_sha256(right), expected)
        self.assertNotEqual(runner.canonical_json(left), runner.canonical_json(right))

    def fixture(self, root: Path, *, subdir: str = ".") -> tuple:
        source = root / "acquired"
        target = source if subdir == "." else source / subdir
        target.mkdir(parents=True)
        (target / "source.py").write_text("class Session:\n    pass\n")
        (source / "LICENSE").write_text("License fixture\n")
        files = [{"path": path.relative_to(source).as_posix(), "bytes": path.stat().st_size,
                  "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                 for path in sorted(source.rglob("*")) if path.is_file()]
        inventory = {"schema": "nudox.acquired-package-source-inventory.v1",
                     "package": {"ecosystem": "pypi", "id": "requests", "version": "2.34.2"},
                     "source_root": str(source), "files": files}
        inventory_path = root / "inventory.json"
        inventory_path.write_bytes(runner.canonical_json(inventory))
        tree_sha = hashlib.sha256(json.dumps(files, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        package = {**inventory["package"], "provenance": {"kind": "source-tree-sha256", "sha256": tree_sha}}
        case = runner.ProjectCase("requests", target, False, 0, (), package, {
            "inventory_path": str(inventory_path),
            "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
            "target_subdir": subdir,
        })
        return case, inventory, tree_sha

    def test_exact_acquired_tree_and_runtime_subdirectory_are_independently_hash_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case, _, tree_sha = self.fixture(Path(directory).resolve(), subdir="src")
            proof = runner.verify_acquired_source_inventory(case, "b" * 64)
            self.assertEqual(proof["verification"], "verified-source-inventory-v1")
            self.assertEqual(proof["source_tree_sha256"], tree_sha)
            self.assertEqual(proof["package"], {"ecosystem": "pypi", "id": "requests", "version": "2.34.2"})
            self.assertEqual(proof["target_root_identity_sha256"], hashlib.sha256(str(case.path).encode()).hexdigest())
            self.assertEqual(proof["target_subdir"], "src")
            # Git administrative metadata is not part of acquired package source.
            (case.path.parent / ".git").mkdir()
            (case.path.parent / ".git" / "index").write_bytes(b"Git metadata")
            self.assertEqual(runner.verify_acquired_source_inventory(case, "b" * 64), proof)

    def test_missing_extra_changed_or_symlink_files_cannot_reuse_a_verified_inventory(self) -> None:
        for mutation in ["missing", "extra", "changed", "symlink"]:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                case, _, _ = self.fixture(Path(directory).resolve())
                if mutation == "missing":
                    (case.path / "source.py").unlink()
                elif mutation == "extra":
                    (case.path / "extra.txt").write_bytes(b"not recognized as source but must be verified")
                elif mutation == "changed":
                    (case.path / "LICENSE").write_text("Changed license\n")
                else:
                    (case.path / "link.py").symlink_to(case.path / "source.py")
                with self.assertRaises(runner.AcceptanceError):
                    runner.verify_acquired_source_inventory(case, "b" * 64)

    def test_wrong_package_target_and_declared_tree_digest_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case, _, _ = self.fixture(Path(directory).resolve(), subdir="src")
            for field, bad_value in [("id", "other-package"), ("version", "0.0.0")]:
                original = case.package[field]
                case.package[field] = bad_value
                with self.assertRaisesRegex(runner.AcceptanceError, "another package or version"):
                    runner.verify_acquired_source_inventory(case, "b" * 64)
                case.package[field] = original
            case.acquisition["target_subdir"] = "."
            with self.assertRaisesRegex(runner.AcceptanceError, "target subdirectory"):
                runner.verify_acquired_source_inventory(case, "b" * 64)
            case.acquisition["target_subdir"] = "src"
            case.package["provenance"]["sha256"] = "a" * 64
            with self.assertRaisesRegex(runner.AcceptanceError, "declared source-tree hash"):
                runner.verify_acquired_source_inventory(case, "b" * 64)

    def test_noncanonical_inventory_paths_and_changed_inventory_digest_are_refused(self) -> None:
        for value in [".", "../escape.py", "/absolute.py", "a//b.py", "./a.py", "a\\b.py"]:
            with self.subTest(value=value), self.assertRaises(runner.Blocked):
                runner.canonical_inventory_relative(value)
        with tempfile.TemporaryDirectory() as directory:
            case, _, _ = self.fixture(Path(directory).resolve())
            Path(case.acquisition["inventory_path"]).write_bytes(b"{}")
            with self.assertRaisesRegex(runner.AcceptanceError, "declared digest"):
                runner.verify_acquired_source_inventory(case, "b" * 64)


class RegistryArtifactOriginTests(unittest.TestCase):
    def test_npm_exact_release_and_encoded_scope_bind_archive_identity(self) -> None:
        from registry_artifact_origin import OriginError, verify_registry_metadata
        import base64
        archive = b"retained archive"
        package = {"ecosystem": "npm", "id": "@types/estree", "version": "1.2.3"}
        archive_url = "https://registry.npmjs.org/@types/estree/-/estree-1.2.3.tgz"
        release = {"name": "@types/estree", "version": "1.2.3", "dist": {
            "tarball": archive_url, "integrity": "sha512-" + base64.b64encode(hashlib.sha512(archive).digest()).decode()}}
        special = {"package.json": json.dumps({"name": package["id"], "version": package["version"]}).encode()}
        for endpoint in ("/%40types%2Festree/latest", "/%40types%2Festree/1.2.3", "/@types/estree/1.2.3"):
            with self.subTest(endpoint=endpoint):
                verify_registry_metadata(package, release, "https://registry.npmjs.org" + endpoint, archive, archive_url, special)
        verify_registry_metadata(package, {"name": package["id"], "versions": {"1.2.3": release}}, "https://registry.npmjs.org/%40types%2Festree", archive, archive_url, special)
        for endpoint in ("/%40types%2Fcounterfeit/latest", "/%40types%2Festree/2.0.0", "/%2540types%252Festree/latest", "/@types/estree/latest/"):
            with self.subTest(endpoint=endpoint), self.assertRaises(OriginError):
                verify_registry_metadata(package, release, "https://registry.npmjs.org" + endpoint, archive, archive_url, special)
        with self.assertRaisesRegex(OriginError, "another package or version"):
            verify_registry_metadata(package, {**release, "version": "2.0.0"}, "https://registry.npmjs.org/%40types%2Festree/latest", archive, archive_url, special)
        with self.assertRaisesRegex(OriginError, "archive belongs"):
            verify_registry_metadata(package, release, "https://registry.npmjs.org/%40types%2Festree/latest", archive, archive_url, {"package.json": b'{"name":"@types/counterfeit","version":"1.2.3"}'})

    def test_pypi_official_project_url_accepts_normalized_names_only(self) -> None:
        from registry_artifact_origin import OriginError, verify_registry_metadata
        archive = b"retained archive"
        package = {"ecosystem": "pypi", "id": "fb-messenger", "version": "0.3.0"}
        archive_url = "https://files.pythonhosted.org/packages/fb_messenger-0.3.0.tar.gz"
        metadata = {"info": {"name": "fb_messenger"}, "releases": {"0.3.0": [{
            "packagetype": "sdist", "url": archive_url, "size": len(archive),
            "digests": {"sha256": hashlib.sha256(archive).hexdigest()}}]}}
        special = {"PKG-INFO": b"Name: fb_messenger\nVersion: 0.3.0\n"}
        for segment in ("fb_messenger", "FB.Messenger", "fb%5Fmessenger"):
            with self.subTest(segment=segment):
                verify_registry_metadata(package, metadata, "https://pypi.org/pypi/" + segment + "/json", archive, archive_url, special)
        for url in ("http://pypi.org/pypi/fb_messenger/json", "https://pypi.org.evil/pypi/fb_messenger/json",
                    "https://pypi.org/pypi/counterfeit/json", "https://pypi.org/pypi/fb%2Fmessenger/json",
                    "https://pypi.org/pypi/fb_messenger/json/", "https://pypi.org/pypi/fb_messenger/json?query=1"):
            with self.subTest(url=url), self.assertRaises(OriginError):
                verify_registry_metadata(package, metadata, url, archive, archive_url, special)

    def fixture(self, root: Path, ecosystem="pypi") -> tuple:
        source = root / "source"
        source.mkdir()
        package = {"ecosystem": ecosystem, "id": "requests" if ecosystem == "pypi" else "typescript", "version": "1.2.3"}
        source_files = {"source.py": b"class Session: pass\n"}
        identity_file = "PKG-INFO" if ecosystem == "pypi" else "package.json"
        source_files[identity_file] = (b"Name: Requests\nVersion: 1.2.3\n" if ecosystem == "pypi"
            else runner.canonical_json({"name": package["id"], "version": package["version"]}))
        prefix = "requests-1.2.3/" if ecosystem == "pypi" else "package/"
        archive_stream = io.BytesIO()
        with tarfile.open(fileobj=archive_stream, mode="w:gz") as archive:
            for name, content in sorted(source_files.items()):
                (source / name).write_bytes(content)
                member = tarfile.TarInfo(prefix + name)
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))
        archive_bytes = archive_stream.getvalue()
        archive_path = root / "archive.tar.gz"
        archive_path.write_bytes(archive_bytes)
        archive_sha = hashlib.sha256(archive_bytes).hexdigest()
        archive_url = ("https://files.pythonhosted.org/packages/archive.tar.gz" if ecosystem == "pypi"
                       else "https://registry.npmjs.org/typescript/-/typescript-1.2.3.tgz")
        if ecosystem == "pypi":
            metadata = {"info": {"name": "Requests"}, "releases": {"1.2.3": [{
                "packagetype": "sdist", "url": archive_url, "size": len(archive_bytes),
                "digests": {"sha256": archive_sha}}]}}
            metadata_url = "https://pypi.org/pypi/requests/json"
        else:
            import base64
            metadata = {"name": "typescript", "versions": {"1.2.3": {
                "name": "typescript", "version": "1.2.3", "dist": {"tarball": archive_url,
                "integrity": "sha512-" + base64.b64encode(hashlib.sha512(archive_bytes).digest()).decode()}}}}
            metadata_url = "https://registry.npmjs.org/typescript"
        metadata_path = root / "metadata.json"
        metadata_path.write_bytes(runner.canonical_json(metadata))
        origin = {"schema": "nudox.registry-artifact-origin.v1", "package": package,
                  "registry_metadata": {"path": str(metadata_path), "sha256": hashlib.sha256(metadata_path.read_bytes()).hexdigest(), "url": metadata_url},
                  "archive": {"path": str(archive_path), "sha256": archive_sha, "url": archive_url},
                  "unpack": {"strip_prefix": prefix}}
        origin_path = root / "origin.json"
        origin_path.write_bytes(runner.canonical_json(origin))
        binding = {"receipt_path": str(origin_path), "receipt_sha256": hashlib.sha256(origin_path.read_bytes()).hexdigest()}
        files = [{"path": name, "bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}
                 for name, content in sorted(source_files.items())]
        return package, files, binding, origin, metadata

    def test_registry_metadata_archive_identity_and_exact_file_membership_are_required(self) -> None:
        for ecosystem in ("pypi", "npm"):
            with self.subTest(ecosystem=ecosystem), tempfile.TemporaryDirectory() as directory:
                package, files, binding, _, _ = self.fixture(Path(directory).resolve(), ecosystem)
                proof = runner.verify_registry_origin(binding, package, files, runner.Deadline(1))
                self.assertEqual(proof["origin_verification"], "verified-registry-artifact-v1")
                self.assertEqual(proof["origin_evidence"]["archive_membership_sha256"], hashlib.sha256(json.dumps(files, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest())
                files.pop()
                with self.assertRaisesRegex(runner.AcceptanceError, "actual acquired source tree"):
                    runner.verify_registry_origin(binding, package, files, runner.Deadline(1))

    def test_inventory_origin_and_declared_archive_are_one_exact_binding(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            package, files, origin_binding, origin, _ = self.fixture(root)
            source = root / "source"
            inventory = {"schema": "nudox.acquired-package-source-inventory.v1",
                         "package": package, "source_root": str(source), "files": files}
            inventory_path = root / "inventory.json"
            inventory_path.write_bytes(runner.canonical_json(inventory))
            case = runner.ProjectCase("requests", source, False, 0, (),
                {**package, "provenance": {"kind": "archive-sha256", "sha256": origin["archive"]["sha256"]}},
                {"inventory_path": str(inventory_path), "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
                 "target_subdir": ".", "origin": origin_binding})
            proof = runner.verify_acquired_source_inventory(case, "a" * 64)
            self.assertEqual(proof["origin_verification"], "verified-registry-artifact-v1")
            self.assertEqual(proof["source_tree_sha256"], proof["origin_evidence"]["archive_membership_sha256"])
            case.package["provenance"]["sha256"] = "b" * 64
            with self.assertRaisesRegex(runner.AcceptanceError, "declared package archive"):
                runner.verify_acquired_source_inventory(case, "a" * 64)

    def test_relabelled_package_cannot_reuse_real_metadata_and_archive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            package, files, binding, origin, _ = self.fixture(Path(directory).resolve())
            package["id"] = "counterfeit"
            origin["package"] = package
            origin["registry_metadata"]["url"] = "https://pypi.org/pypi/counterfeit/json"
            path = Path(binding["receipt_path"])
            path.write_bytes(runner.canonical_json(origin))
            binding["receipt_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
            with self.assertRaisesRegex(runner.AcceptanceError, "another project"):
                runner.verify_registry_origin(binding, package, files, runner.Deadline(1))
            metadata_path = Path(origin["registry_metadata"]["path"])
            metadata = json.loads(metadata_path.read_bytes())
            metadata["info"]["name"] = "counterfeit"
            metadata_path.write_bytes(runner.canonical_json(metadata))
            origin["registry_metadata"]["sha256"] = hashlib.sha256(metadata_path.read_bytes()).hexdigest()
            path.write_bytes(runner.canonical_json(origin))
            binding["receipt_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
            with self.assertRaisesRegex(runner.AcceptanceError, "sdist package metadata"):
                runner.verify_registry_origin(binding, package, files, runner.Deadline(1))

    def test_links_traversal_duplicate_members_and_uncompressed_limits_are_refused(self) -> None:
        for bad_name, bad_type in [("package/../escape", tarfile.REGTYPE),
                                    ("package/link", tarfile.SYMTYPE),
                                    ("package/file", tarfile.REGTYPE)]:
            archive_stream = io.BytesIO()
            with tarfile.open(fileobj=archive_stream, mode="w") as archive:
                for index in range(2 if bad_name == "package/file" else 1):
                    member = tarfile.TarInfo(bad_name)
                    member.type = bad_type
                    member.size = 1 if bad_type == tarfile.REGTYPE else 0
                    archive.addfile(member, io.BytesIO(b"x") if member.size else None)
            with self.subTest(name=bad_name), self.assertRaises(runner.OriginError):
                runner.archive_members(archive_stream.getvalue(), "package/", 10, 1024, 1024)
        with tempfile.TemporaryDirectory() as directory:
            _, _, _, origin, _ = self.fixture(Path(directory).resolve())
            with self.assertRaises(runner.OriginError):
                runner.archive_members(Path(origin["archive"]["path"]).read_bytes(), origin["unpack"]["strip_prefix"], 10, 1, 1024)


class OperationObservationBoundaryTests(unittest.TestCase):
    """The actual 80543 TypeScript pilot used these two different envelopes."""

    key = "6bf72b553ff267091039bfc8afe35983a53278970e48f079ff8ae2fbbe16af64"
    package = Path(tempfile.gettempdir()) / "nudox-operation-contract-fixture"

    def observation(self) -> dict:
        return {
            "state": "known",
            "detail": {
                "operation_key": self.key,
                "request_digest": "9a9deef6819f8dcb28c1d79d483067d58cb0acb4750d00c127344e581b2381c5",
                "package": {"kind": "local", "value": str(self.package)},
                "execution_intent": "interactive",
                "state": {
                    "state": "active",
                    "detail": {
                        "stage": "scanning",
                        "ticket": {
                            "id": 1,
                            "owner_epoch": [36, 103, 93, 243, 49, 77, 198, 160, 118, 172, 126, 210, 108, 187, 207, 240],
                            "package": {"kind": "local", "value": str(self.package)},
                        },
                    },
                },
            },
        }

    def surface(self, observation: dict) -> dict:
        return {"answer": "surface", "detail": "summary", "surface": {
            "result": "index-operation-status", "data": observation,
        }}

    def test_actual_cli_and_mcp_wrappers_admit_the_same_typed_active_observation(self) -> None:
        observation = self.observation()
        cli = {"answer": "product", "heading": "index-operation", "index_operation": observation}
        self.assertEqual(runner.operation_state(cli, self.key, self.package), ("active", None, None))
        self.assertEqual(runner.operation_state(self.surface(observation), self.key, self.package),
                         runner.operation_state(cli, self.key, self.package))

    def test_typed_refusal_facts_survive_both_wrappers_without_human_promotion(self) -> None:
        # Same existing DTO family; cause admission belongs to the shared Rust
        # client. This test exercises observer retention, not compiler success.
        failure = {"relative_path": "classes/comparator.d.ts", "source_identity": "ab" * 32,
                   "source_byte_len": 11, "recipe_identity": None, "phase": "setup",
                   "kind_tag": "toolchain_configuration_mismatch",
                   "cause": {"family": "toolchain", "fault": {"language": "type_script",
                       "stage": "lower-ir", "selected": "type_script_compiler", "configured": None}},
                   "detail": "tsc is selected but not configured", "detail_truncated": False}
        observation = self.observation()
        detail = {"reason": "refused", "detail": "short human message", "compiler_failure": failure}
        observation["detail"]["state"] = {"state": "failed", "detail": detail}
        cli = {"answer": "product", "heading": "index-operation", "index_operation": observation}
        for wrapped in [cli, self.surface(observation)]:
            self.assertEqual(runner.operation_state(wrapped, self.key, self.package),
                             ("failed", None, detail))
        detail.pop("compiler_failure")
        detail["detail"] = json.dumps(failure)
        self.assertEqual(runner.operation_state(self.surface(observation), self.key, self.package),
                         ("failed", None, detail))
        for state, reason, facts in [("unresolved", "restarted", failure),
                                     ("failed", "worker-failed", failure),
                                     ("failed", "refused", json.dumps(failure)),
                                     ("failed", "refused", {"opaque": "x" * 8192})]:
            with self.subTest(state=state, reason=reason, facts_type=type(facts)):
                observation["detail"]["state"] = {"state": state, "detail": {
                    "reason": reason, "detail": "human detail", "compiler_failure": facts}}
                with self.assertRaises(runner.AcceptanceError):
                    runner.operation_state(self.surface(observation), self.key, self.package)

    def test_raw_surface_still_checks_the_exact_caller_key_package_and_request_digest(self) -> None:
        for field, value in [
            ("operation_key", "1" * 64),
            ("package", {"kind": "local", "value": "/another/package"}),
            ("request_digest", "not-a-digest"),
        ]:
            with self.subTest(field=field):
                observation = self.observation()
                observation["detail"][field] = value
                with self.assertRaises(runner.AcceptanceError):
                    runner.operation_state(self.surface(observation), self.key, self.package)

    def test_another_surface_result_and_valid_json_in_human_text_are_not_operations(self) -> None:
        wrong_result = self.surface(self.observation())
        wrong_result["surface"]["result"] = "references"
        text_only = {"answer": "fault", "detail": json.dumps(self.observation())}
        untagged = {"answer": "surface", "surface": {"data": self.observation()}}
        for value in [wrong_result, text_only, untagged]:
            with self.subTest(value=value):
                with self.assertRaises(runner.AcceptanceError):
                    runner.operation_state(value, self.key, self.package)

    def test_mcp_status_refuses_a_start_reply_even_with_the_exact_same_operation_binding(self) -> None:
        wrong_route = self.surface(self.observation())
        wrong_route["surface"]["result"] = "index-operation-started"
        self.assertEqual(runner.operation_state(wrong_route, self.key, self.package),
                         ("active", None, None))
        case = runner.ProjectCase("fixture", self.package, False, 0, ())
        with patch.object(runner, "mcp_call", return_value=wrong_route):
            with self.assertRaisesRegex(runner.AcceptanceError, "another result route"):
                runner.mcp_operation_status(None, self.package, self.package / "endpoint",
                                            case, self.key, {}, runner.Deadline(1), [], "status")


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


class GoRegistryArtifactOriginTests(unittest.TestCase):
    def fixture(self, root: Path) -> tuple:
        from registry_artifact_origin import go_hash1
        import zipfile
        source = root / "source"
        (source / "pkg").mkdir(parents=True)
        package = {"ecosystem": "go", "id": "example.org/module", "version": "v1.2.3"}
        contents = {"go.mod": b"module example.org/module\n\ngo 1.20\n", "pkg/main.go": b"package pkg\nfunc NewRoute() {}\n"}
        files = [{"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()} for name, data in sorted(contents.items())]
        for name, data in contents.items():
            (source / name).write_bytes(data)
        def bind(name, value):
            data = value if isinstance(value, bytes) else runner.canonical_json(value)
            path = root / name
            path.write_bytes(data)
            return {"path": str(path), "sha256": hashlib.sha256(data).hexdigest()}
        prefix = package["id"] + "@" + package["version"] + "/"
        archive_stream = io.BytesIO()
        with zipfile.ZipFile(archive_stream, "w") as archive:
            for name, data in contents.items():
                archive.writestr(prefix + name, data)
        info = bind("info.json", {"Version": package["version"]})
        archive = bind("module.zip", archive_stream.getvalue())
        mod = bind("module.mod", contents["go.mod"])
        archive_sum = go_hash1(files, prefix)
        mod_sum = go_hash1([{"path": "go.mod", "sha256": hashlib.sha256(contents["go.mod"]).hexdigest()}])
        lookup = bind("sumdb.txt", ("1\nexample.org/module v1.2.3 " + archive_sum + "\nexample.org/module v1.2.3/go.mod " + mod_sum + "\n\n— sum.golang.org retained-signature\n").encode())
        download = bind("download.json", {"Path": package["id"], "Version": package["version"],
            "Sum": archive_sum, "GoModSum": mod_sum, "Info": info["path"], "Zip": archive["path"], "GoMod": mod["path"]})
        executable = bind("go", b"unit-test executable witness; no runtime proof")
        snapshot = bind("snapshot.json", {})
        process = {"schema": "nudox.go-module-download-process.v1", "request": {
            "registry_request_path": package["id"], "resolved_version": package["version"], "query": "latest"},
            "executable": {**executable, "snapshot_path": snapshot["path"], "snapshot_sha256": snapshot["sha256"]},
            "argv": [executable["path"], "mod", "download", "-json", package["id"] + "@latest"],
            "environment": {"GOPROXY": "https://proxy.golang.org", "GOSUMDB": "sum.golang.org"},
            "exit_code": 0, "stdout": download, "stderr": bind("stderr", b""),
            "timeout_pid_file": bind("pid", b"123\n"), "acquisition_status": bind("status", b"status=downloaded\n"),
            "driver_files": [bind("driver.sh", b"original acquisition driver")], "proxy_artifacts": {
                "go_mod": mod, "sumdb_lookup": lookup, "info": info, "zip": archive,
                "ziphash": bind("ziphash", archive_sum.encode()), "sum": archive_sum, "go_mod_sum": mod_sum}}
        listing = {"ImportPath": "example.org/module/pkg", "Dir": str(source / "pkg"),
                   "Module": {"Path": package["id"], "Dir": str(source), "GoMod": str(source / "go.mod"), "Main": True}}
        receipt = {"schema": "nudox.registry-artifact-origin.v1", "package": package,
            "registry_metadata": {**info, "url": "https://proxy.golang.org/example.org/module/@v/v1.2.3.info"},
            "archive": {**archive, "url": "https://proxy.golang.org/example.org/module/@v/v1.2.3.zip"},
            "unpack": {"strip_prefix": prefix}, "module": {"go_mod": mod, "sumdb_lookup": lookup,
                "download": download, "process": bind("process.json", process), "go_list": bind("go-list.json", listing)}}
        binding = bind("origin.json", receipt)
        return package, files, {"receipt_path": binding["path"], "receipt_sha256": binding["sha256"]}, receipt, source, process, listing

    def test_go_original_authenticated_acquisition_and_target_binding(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            package, files, binding, receipt, source, process, listing = self.fixture(Path(directory).resolve())
            def verify():
                return runner.verify_registry_origin(binding, package, files, runner.Deadline(1),
                    source_root=source, target_root=source / "pkg", target_subdir="pkg")
            proof = verify()
            self.assertEqual(proof["target_package"]["id"], "example.org/module/pkg")
            self.assertIn("no independent Python signature verification", proof["origin_evidence"]["authentication_source"])
            for value in ("example.org/counterfeit/pkg", "example.org/module/other"):
                listing["ImportPath"] = value
                path = Path(receipt["module"]["go_list"]["path"])
                path.write_bytes(runner.canonical_json(listing))
                receipt["module"]["go_list"]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                path = Path(binding["receipt_path"])
                path.write_bytes(runner.canonical_json(receipt))
                binding["receipt_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                with self.assertRaisesRegex(runner.AcceptanceError, "actual runtime package"):
                    verify()

    def test_go_content_sum_and_original_process_cannot_be_relabelled(self) -> None:
        for mutation in ("sum", "version", "argv", "sumdb", "module"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                package, files, binding, receipt, source, process, listing = self.fixture(Path(directory).resolve())
                if mutation in {"sum", "version"}:
                    path = Path(receipt["module"]["download"]["path"])
                    data = json.loads(path.read_bytes());data["Sum" if mutation == "sum" else "Version"] = "counterfeit"
                    path.write_bytes(runner.canonical_json(data));receipt["module"]["download"]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                elif mutation in {"argv", "sumdb"}:
                    if mutation == "argv":process["argv"][-1] = "example.org/module@v2.0.0"
                    else:process["environment"]["GOSUMDB"] = "off"
                    path = Path(receipt["module"]["process"]["path"]);path.write_bytes(runner.canonical_json(process));receipt["module"]["process"]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                else:
                    path = Path(receipt["module"]["go_mod"]["path"]);path.write_bytes(b"module counterfeit.org/module\n");receipt["module"]["go_mod"]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                path = Path(binding["receipt_path"]);path.write_bytes(runner.canonical_json(receipt));binding["receipt_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                with self.assertRaises(runner.AcceptanceError):
                    runner.verify_registry_origin(binding, package, files, runner.Deadline(1), source_root=source, target_root=source / "pkg", target_subdir="pkg")


if __name__ == "__main__":
    unittest.main(verbosity=2)
