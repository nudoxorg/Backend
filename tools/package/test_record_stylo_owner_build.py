"""Two-phase custody fixtures; no product or worker image is executed."""
import copy
import hashlib
import json
import os
import sys
import time
from pathlib import Path
from unittest.mock import patch

import record_stylo_owner_build as actor
from test_record_embedding_helper_build import HelperReceiptFixtures, producer


class StyloOwnerReceiptFixtures(HelperReceiptFixtures):
    def test_runpath_rejects_rpath_precedence_and_ambiguous_entries(self):
        command = (self.args.cargo, self.artifact, "$ORIGIN", time.monotonic() + 5, {})
        with patch.object(actor, "bounded_process", return_value=b"(RUNPATH) [$ORIGIN]"):
            actor.require_runpath(*command)
        for output in (b"(RPATH) [$ORIGIN]", b"(RPATH) [/foreign]\n(RUNPATH) [$ORIGIN]", b"(RUNPATH) [$ORIGIN]\n(RUNPATH) [$ORIGIN]"):
            with self.subTest(output=output), patch.object(actor, "bounded_process", return_value=output), self.assertRaises(ValueError):
                actor.require_runpath(*command)

    def test_owner_path_scan_catches_boundary_split(self):
        path = self.root / "owner-image"
        forbidden = self.root / "private-checkout"
        marker = os.fsencode(forbidden)
        path.write_bytes(b"x" * (1024 * 1024 - len(marker) // 2) + marker + b"z")
        with self.assertRaisesRegex(ValueError, "private build path"):
            actor.require_no_builder_paths(path, [forbidden])

    def test_bounded_tool_rejects_excess_output_and_deadline(self):
        self.assertEqual(actor.bounded_process([sys.executable, "-c", "print('ok')"], time.monotonic() + 5, dict(os.environ)), b"ok\n")
        with self.assertRaisesRegex(ValueError, "output limit"):
            actor.bounded_process([sys.executable, "-c", "print('x'*10000)"], time.monotonic() + 5, dict(os.environ), maximum=64)
        with self.assertRaisesRegex(ValueError, "deadline"):
            actor.bounded_process([sys.executable, "-c", "import time; time.sleep(10)"], time.monotonic() + 0.2, dict(os.environ))

    def prepare_actor(self, *, tamper=False):
        self.args.sqlite = False
        self.args.sqlite_desktop = False
        worker = self.artifact.with_name("backend-stylo-worker")
        self.artifact.rename(worker)
        self.args.worker = worker
        self.args.worker_receipt = self.root / "worker-receipt.json"
        worker_paths = [str(self.source["path"]), str(self.args.cargo_home), str(self.args.target_dir), str(self.args.build_dir)]
        worker_flags = "\x1f".join(f"--remap-path-prefix={value}=/nudox/{label}" for value, label in zip(worker_paths, ("source", "cargo-home", "target", "build")))
        worker_receipt = {"schema": "nudox.stylo-worker-build-receipt.v2", "remap_schema": "nudox.stylo-worker-remap.v2", "source_path": worker_paths[0],
            "source": {"commit": self.source["commit"], "tree": self.source["tree"],
                "cargo_lock_sha256": self.source["cargo_lock_sha256"], "clean_before": True, "clean_after": True},
            "argv": ["cargo", "build", "--release", "--locked", "-p", "backend-frontend-stylo", "--bin", "backend-stylo-worker"],
            "exit": 0, "execution": {"exit": 0, "process_group_retired": True},
            "environment": {"CARGO_BUILD_TARGET": producer.TARGET, "CARGO_NET_OFFLINE": "true", "CARGO_BUILD_JOBS": "2", "CARGO_INCREMENTAL": "0", "CARGO_CACHE_AUTO_CLEAN_FREQUENCY": "never", "CARGO_TERM_COLOR": "never", "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": "", "RUSTC": "/fixture/rustc", "CARGO_HOME": worker_paths[1], "CARGO_TARGET_DIR": worker_paths[2], "CARGO_BUILD_BUILD_DIR": worker_paths[3], "HOME": worker_paths[3], "TMPDIR": worker_paths[3], "RUSTFLAGS": "", "CARGO_ENCODED_RUSTFLAGS": worker_flags},
            "toolchain": {"unchanged": True, "rustc": {"path": "/fixture/rustc", "sha256": "e" * 64}, "cargo": {"path": "/fixture/cargo", "sha256": "e" * 64}},
            "build_helpers": {name: {"path": "/fixture/" + name, "sha256": "e" * 64} for name in ("cc", "make", "python3", "sh")},
            "artifact": {"bytes": worker.stat().st_size, "sha256": self.digest(worker), "architecture": "x86_64"}}
        self.args.worker_receipt.write_text(json.dumps(worker_receipt))
        cli = self.root / "tools/backend-cli"
        cli.write_bytes(b"source-receipted CLI fixture")
        cli.chmod(0o700)
        self.args.cli_manifest = self.root / "cli-manifest.json"
        self.args.cli_manifest.write_text(json.dumps({"executables": {"backend-cli": {
            "path": str(cli), "bytes": cli.stat().st_size, "sha256": self.digest(cli)}}}))
        for name in ("patchelf", "readelf"):
            path = self.root / "tools" / name
            path.write_bytes(name.encode())
            path.chmod(0o700)
            setattr(self.args, name, path)
            setattr(self.args, name + "_sha256", self.digest(path))
        owner = self.args.target_dir / producer.TARGET / "release/backend-locald"
        def execute(command, source, env, output, deadline, maximum):
            self.fixture_execution(command, source, env, output, deadline, maximum)
            built = (self.args.target_dir / producer.TARGET / "release/examples/stylo_identity_observer") if command == actor.OBSERVER_ARGV else owner
            built.parent.mkdir(parents=True, exist_ok=True)
            built.write_bytes(worker.read_bytes())
            built.chmod(0o700)
            return {"exit": 0, "pid": 42, "process_group_retired": True, "output_bytes": 21}
        self.fixtures(execution=execute)
        self.addCleanup(patch.stopall)
        patch.object(actor, "common", producer).start()
        patch.object(actor, "package", producer.package).start()
        patch.object(producer.package, "admit_receipt", return_value={}).start()
        patch.object(producer.package, "check_source", return_value=(self.source["commit"], self.source["tree"], self.source["cargo_lock_sha256"], "0.2.0")).start()
        loader = self.root / "tools/loader"
        loader.write_bytes(b"loader fixture")
        loader.chmod(0o700)
        self.args.source_loader = loader
        self.args.source_loader_sha256 = self.digest(loader)
        library = self.root / "tools/libgcc_s.so.1"
        library.write_bytes(worker.read_bytes())
        before = {"needed": ["libc.so.6", "libgcc_s.so.1"], "interpreter": str(loader), "required_glibc_versions": []}
        after = {**before, "interpreter": producer.package.INTERPRETER}
        lib_info = {"needed": ["libc.so.6"], "interpreter": None, "required_glibc_versions": []}
        self.elf_inspector = patch.object(producer.package, "elf_info", side_effect=[before, lib_info, lib_info, after]).start()
        patch.object(producer.package, "run", return_value="$ORIGIN/../lib").start()
        def bounded(command, deadline, env, maximum=65536):
            if "--list" in command:
                return f"libgcc_s.so.1 => {library} (0x1)\n".encode()
            if "-d" in command:
                return b"(SONAME) [libgcc_s.so.1]\n(RUNPATH) [$ORIGIN]" if command[-1].endswith("libgcc_s.so.1") else b"(RUNPATH) [$ORIGIN/../lib]"
            if "--print-rpath" in command:
                return b"$ORIGIN" if command[-1].endswith("libgcc_s.so.1") else b"$ORIGIN/../lib"
            return b""
        patch.object(actor, "bounded_process", side_effect=bounded).start()
        patch.object(actor.shutil, "which", return_value=str(self.args.cargo)).start()
        patch.object(producer.shared, "stable_tool_file", side_effect=lambda path, *args, **kwargs: {"path": str(path), "sha256": self.digest(path)}).start()
        def observe(cli_path, staged, dependency, deadline, env):
            if tamper:
                staged.write_bytes(staged.read_bytes() + b"changed")
            return "a" * 64
        patch.object(actor, "observe", side_effect=observe).start()
        return worker

    def test_owner_build_binds_observed_relocated_worker_to_compile_environment(self):
        self.prepare_actor()
        result = actor.capture(self.args)
        receipt = json.loads(result.read_bytes())
        self.assertEqual(self.execution_calls[1][0], actor.ARGV)
        self.assertEqual(self.execution_calls[1][2]["NUDOX_BUILD_STYLO_WORKER_ID"], "a" * 64)
        self.assertEqual(receipt["worker_anchor"], "a" * 64)
        self.assertEqual(receipt["environment"]["NUDOX_BUILD_STYLO_WORKER_ID"], receipt["worker_anchor"])
        self.assertEqual(receipt["schema"], actor.SCHEMA)
        cli = json.loads(self.args.cli_manifest.read_bytes())["executables"]["backend-cli"]
        worker_receipt_sha = hashlib.sha256(self.args.worker_receipt.read_bytes()).hexdigest()
        raw_worker = {"bytes": self.args.worker.stat().st_size, "sha256": self.digest(self.args.worker)}
        arguments = {"source": (self.source["commit"], self.source["tree"], self.source["cargo_lock_sha256"]),
                     "cli_record": cli, "cli_manifest_sha": self.digest(self.args.cli_manifest),
                     "raw_worker": raw_worker, "worker_receipt_sha": worker_receipt_sha,
                     "worker_private_paths": receipt["worker_private_paths"]}
        candidate = self.root / "mutated-owner-receipt.json"
        for mutation in ("version", "anchor", "flags", "source", "worker", "owner", "dependency", "observer", "observer_command", "observer_environment", "observer_exit_type", "c_schema", "cflags", "cxxflags", "loader", "needed", "malformed_needed", "tool"):
            changed = copy.deepcopy(receipt)
            if mutation == "version": changed["schema"] = "nudox.stylo-bound-owner-build-receipt.v1"
            if mutation == "anchor": changed["environment"]["NUDOX_BUILD_STYLO_WORKER_ID"] = "b" * 64
            if mutation == "flags": changed["environment"]["CARGO_ENCODED_RUSTFLAGS"] = ""
            if mutation == "source": changed["source"]["tree"] = "f" * 40
            if mutation == "worker": changed["worker"]["sha256"] = "f" * 64
            if mutation == "owner": changed["owner"]["sha256"] = "f" * 64
            if mutation == "dependency": changed["dependency"]["sha256"] = "f" * 64
            if mutation == "observer": changed["observer"]["sha256"] = "f" * 64
            if mutation == "observer_command": changed["observer_argv"].append("--features=untrusted")
            if mutation == "observer_environment": changed["observer_environment"]["RUSTFLAGS"] = "bad"
            if mutation == "observer_exit_type": changed["observer_execution"]["exit"] = False
            if mutation == "c_schema": changed["c_remap_schema"] = "other"
            if mutation == "cflags": changed["environment"]["CFLAGS"] = ""
            if mutation == "cxxflags": changed["environment"]["CXXFLAGS"] = ""
            if mutation == "loader": changed["source_loader"]["sha256"] = "f" * 64
            if mutation == "needed": changed["dependency_transformation"]["before"]["needed"].append("libforeign.so")
            if mutation == "malformed_needed": changed["transformation"]["before"]["needed"].append({"not": "a soname"})
            if mutation == "tool": changed["transformation"]["tool_sha256"]["patchelf"] = "invalid"
            candidate.write_text(json.dumps(changed))
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                producer.package.admit_stylo_bound_owner(candidate, **arguments)
        oversized = self.root / "oversized/libgcc_s.so.1"
        oversized.parent.mkdir()
        with oversized.open("wb") as stream:
            stream.truncate(producer.package.MAX_STYLO_LIBGCC_BYTES + 1)
        changed = copy.deepcopy(receipt)
        changed["dependency"].update(path=str(oversized), bytes=oversized.stat().st_size)
        candidate.write_text(json.dumps(changed))
        with self.assertRaisesRegex(ValueError, "byte bound"):
            producer.package.admit_stylo_bound_owner(candidate, **arguments)

    def test_sqlite_desktop_and_locald_share_observed_build_basis(self):
        worker = self.prepare_actor()
        self.args.sqlite = True
        self.args.sqlite_desktop = True
        original_execute = producer.execute.side_effect
        events = []
        def execute(command, source, env, output, deadline, maximum):
            events.append(tuple(command))
            result = original_execute(command, source, env, output, deadline, maximum)
            if command == actor.DESKTOP_ARGV:
                desktop = self.args.target_dir / producer.TARGET / "release/backend-desktop"
                desktop.write_bytes(worker.read_bytes())
                desktop.chmod(0o700)
            return result
        def prepare(**kwargs):
            events.append("sqlite-prepared")
            dependency = producer.artifact_identity(self.args.output_dir / "libgcc_s.so.1")
            return {"dependency": dependency, "owner_environment": {"NUDOX_BUILD_SQLITE_WORKER_ID": "b" * 64, "NUDOX_BUILD_SQLITE_BASIS_JSON": "basis"}}
        def verify(*args, **kwargs):
            events.append("sqlite-rechecked")
        with (patch.object(producer, "execute", side_effect=execute),
              patch.object(actor.sqlite_phase, "prepare", side_effect=prepare),
              patch.object(actor.sqlite_phase, "verify_after_owner", side_effect=verify),
              patch.object(producer.package, "admit_stylo_bound_owner", return_value=({}, "x"))):
            receipt = json.loads(actor.capture(self.args).read_bytes())
        self.assertLess(events.index("sqlite-prepared"), events.index(tuple(actor.ARGV)))
        self.assertLess(events.index(tuple(actor.DESKTOP_ARGV)), events.index("sqlite-rechecked"))
        for command, _, env, _, _ in self.execution_calls:
            if command in (actor.ARGV, actor.DESKTOP_ARGV):
                self.assertEqual(env["NUDOX_BUILD_STYLO_WORKER_ID"], "a" * 64)
                self.assertEqual(env["NUDOX_BUILD_SQLITE_WORKER_ID"], "b" * 64)
                self.assertEqual(env["NUDOX_BUILD_SQLITE_BASIS_JSON"], "basis")
        self.assertEqual(receipt["desktop"]["argv"], actor.DESKTOP_ARGV)

    def test_worker_change_during_observation_cannot_start_owner_build(self):
        self.prepare_actor(tamper=True)
        with self.assertRaisesRegex(ValueError, "worker closure changed during identity observation"):
            actor.capture(self.args)
        self.assertEqual([call[0] for call in self.execution_calls], [actor.OBSERVER_ARGV])
        self.assertFalse((self.args.output_dir / "stylo-bound-owner-build-receipt.json").exists())

    def test_unexpected_worker_dependency_cannot_start_owner_build(self):
        self.prepare_actor()
        self.elf_inspector.side_effect = [{"needed": ["libc.so.6", "libforeign.so"], "interpreter": "/fixture/loader", "required_glibc_versions": []}]
        with self.assertRaisesRegex(ValueError, "unexpected non-platform dependency"):
            actor.capture(self.args)
        self.assertEqual([call[0] for call in self.execution_calls], [actor.OBSERVER_ARGV])

    def test_unadmitted_source_loader_cannot_start_owner_build(self):
        self.prepare_actor()
        self.args.source_loader_sha256 = "f" * 64
        with self.assertRaisesRegex(ValueError, "explicitly admitted source loader"):
            actor.capture(self.args)
        self.assertEqual([call[0] for call in self.execution_calls], [actor.OBSERVER_ARGV])
