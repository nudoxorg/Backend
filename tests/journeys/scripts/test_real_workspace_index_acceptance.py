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


if __name__ == "__main__":
    unittest.main(verbosity=2)
