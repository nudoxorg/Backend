#!/usr/bin/env python3
"""Record a two-phase, source-bound Linux Stylo companion and owner build.

The caller supplies the existing managed build scope and a root CLI build
receipt. This actor never installs or runs the worker. Its output establishes
build custody, not successful installed execution or dependency isolation.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import sys
import time

import linux_release_package as package
import record_embedding_helper_build as common


ARGV = ["cargo", "build", "--release", "--locked", "-p", "backend-locald", "--bin", "backend-locald"]
SCHEMA = "nudox.stylo-bound-owner-build-receipt.v4"
OBSERVER_ARGV = ["cargo", "build", "--release", "--locked", "-p", "backend-compile", "--example", "stylo_identity_observer"]
REMAP_SCHEMA = "nudox.stylo-owner-remap.v1"
C_REMAP_SCHEMA = "nudox.stylo-c-prefix-map.v1"
LIBGCC = "libgcc_s.so.1"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise common.shared.ReceiptError(message)


def bounded_process(command: list[str], deadline: float, env: dict[str, str], maximum: int = 65536) -> bytes:
    require(time.monotonic() < deadline, "bounded tool deadline expired")
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, env=env, start_new_session=True, close_fds=True)
    output = {"out": bytearray(), "err": bytearray()}
    try:
        with selectors.DefaultSelector() as pending:
            pending.register(process.stdout, selectors.EVENT_READ, "out")
            pending.register(process.stderr, selectors.EVENT_READ, "err")
            while pending.get_map():
                remaining = deadline - time.monotonic()
                require(remaining > 0, "bounded tool deadline expired")
                for key, _ in pending.select(min(0.1, remaining)):
                    remaining_bytes = maximum - len(output["out"]) - len(output["err"])
                    chunk = os.read(key.fd, min(65536, remaining_bytes + 1))
                    if not chunk:
                        pending.unregister(key.fileobj)
                        continue
                    require(len(chunk) <= remaining_bytes, "bounded tool exceeded its output limit")
                    output[key.data].extend(chunk)
        require(process.wait(timeout=max(0.001, deadline - time.monotonic())) == 0,
                "bounded tool failed")
        require(common.runtime.terminate_owned_group(process), "bounded tool process group did not retire")
        return bytes(output["out"])
    finally:
        common.runtime.terminate_owned_group(process)
        process.wait()
        process.stdout.close()
        process.stderr.close()


def observe(observer: Path, worker: Path, dependency: Path, deadline: float, env: dict[str, str]) -> str:
    raw = bounded_process([str(observer), str(worker), str(dependency)], deadline, env)
    value = package.parse_json_bytes(raw, "worker identity observation")
    identity = value.get("identity")
    require(value.get("state") == "observed-helper" and isinstance(identity, str)
            and re.fullmatch(r"[0-9a-f]{64}", identity) is not None and identity != "0" * 64,
            "worker typed identity observation is malformed")
    return identity


def require_runpath(readelf: Path, artifact: Path, expected: str, deadline: float, env: dict[str, str]) -> None:
    dynamic = bounded_process([str(readelf), "-d", str(artifact)], deadline, env).decode("utf-8")
    require(not re.findall(r"\(RPATH\).*\[(.*?)\]", dynamic)
            and re.findall(r"\(RUNPATH\).*\[(.*?)\]", dynamic) == [expected],
            "artifact must use exactly the expected DT_RUNPATH and no DT_RPATH")


def require_no_builder_paths(path: Path, private_paths: list[Path]) -> None:
    patterns = [os.fsencode(value) for value in private_paths]
    overlap = max(map(len, patterns), default=1) - 1
    tail = b""
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            block = tail + chunk
            require(not any(pattern in block for pattern in patterns), "owner contains a private build path")
            tail = block[-overlap:] if overlap else b""


def capture(args: argparse.Namespace) -> Path:
    source = common.clean_source_snapshot(args.source_checkout)
    require(source["clean"] and source["path"] == common.ROOT, "owner source must be this exact clean checkout")
    require(type(args.jobs) is int and 1 <= args.jobs <= 4, "jobs must be between one and four")
    require(0 < args.timeout_seconds <= 1800 and 0 < args.max_log_bytes <= 64 * 1024 * 1024,
            "build bounds exceed the supported finite envelope")
    deadline = time.monotonic() + args.timeout_seconds
    for name in ("cargo_home", "target_dir", "build_dir"):
        setattr(args, name, common.directory(getattr(args, name)))
    output = common.runtime.output_path(args.output_dir, "Stylo owner evidence directory")
    require(output.resolve(strict=False) == output, "owner evidence path must be canonical")
    require(all(path != source["path"] and source["path"] not in path.parents
                for path in (args.cargo_home, args.target_dir, args.build_dir, output)),
            "owner mutable directories must be outside source")
    require(len({args.cargo_home, args.target_dir, args.build_dir, output}) == 4
            and all(output not in path.parents and path not in output.parents
                    for path in (args.cargo_home, args.target_dir, args.build_dir)),
            "owner evidence must not overlap mutable build directories")
    manifest_bytes = package.read_regular_bytes(args.cli_manifest, 16 * 1024 * 1024, "CLI build manifest")
    build = package.parse_json_bytes(manifest_bytes, "CLI build manifest")
    root_receipt = package.admit_receipt(build)
    with package.clean_git_environment():
        revision, tree, lock_sha, _ = package.check_source(source["path"], build, root_receipt)
    require((revision, tree, lock_sha) == (source["commit"], source["tree"], source["cargo_lock_sha256"]),
            "CLI and owner are not from the exact same source")
    cli_record = build["executables"]["backend-cli"]
    cli = Path(cli_record.get("path", ""))
    require(cli.is_absolute() and not cli.is_symlink(), "CLI must be its absolute source-receipted artifact")
    cli_size, cli_sha = package.admit_file_digest(cli, package.MAX_BINARY_BYTES)
    require((cli_size, cli_sha) == (cli_record.get("bytes"), cli_record.get("sha256")) and os.access(cli, os.X_OK),
            "CLI differs from its root build receipt")
    require(args.worker.is_absolute() and not args.worker.is_symlink(), "worker must be an absolute regular artifact")
    worker, worker_receipt_sha = package.admit_stylo_worker(args.worker, args.worker_receipt, revision, tree, lock_sha)
    worker_receipt_bytes = package.read_regular_bytes(args.worker_receipt, 65536, "Stylo worker build receipt")
    require(hashlib.sha256(worker_receipt_bytes).hexdigest() == worker_receipt_sha, "worker receipt changed during admission")
    worker_receipt = package.parse_json_bytes(worker_receipt_bytes, "Stylo worker build receipt")
    worker_private_paths = [worker_receipt["source_path"], *[worker_receipt["environment"][key] for key in ("CARGO_HOME", "CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR")]]
    tools = {name: common.tool(args, name) for name in ("cargo", "rustc")}
    patchelf = package.require_tool(args.patchelf, "patchelf")
    readelf = package.require_tool(args.readelf, "readelf")
    admitted_loader = package.require_tool(args.source_loader, "source worker loader")
    tool_hashes = {"patchelf": package.sha256(patchelf), "readelf": package.sha256(readelf)}
    require(tool_hashes == {"patchelf": args.patchelf_sha256, "readelf": args.readelf_sha256},
            "ELF tools differ from their admitted digests")
    env = common.build_environment(args)
    allowed = {"PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "TZ", "RUSTC", "RUSTC_WRAPPER",
               "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_HOME",
               "CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "CARGO_BUILD_TARGET", "CARGO_NET_OFFLINE",
               "CARGO_BUILD_JOBS", "CARGO_INCREMENTAL", "CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "CARGO_TERM_COLOR", "CFLAGS", "CXXFLAGS"}
    env = {key: value for key, value in env.items() if key in allowed}
    # Build scripts use a private, recorded home and temporary directory;
    # neither may inherit the operator's personal home or ambient TMPDIR.
    env["HOME"] = str(args.build_dir)
    env["TMPDIR"] = str(args.build_dir)
    private_paths = [source["path"], args.cargo_home, args.target_dir, args.build_dir]
    require(all("\x1f" not in str(path) and "=" not in str(path) for path in private_paths), "build path contains a rustflag delimiter")
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(
        f"--remap-path-prefix={path}=/nudox/{label}"
        for path, label in zip(private_paths, ("source", "cargo-home", "target", "build")))
    require(all(not any(character.isspace() for character in str(path)) for path in private_paths),
            "C prefix-map paths cannot contain whitespace")
    cflags = " ".join(
        f"-{kind}-prefix-map={path}=/nudox/{label}"
        for path, label in zip(private_paths, ("source", "cargo-home", "target", "build"))
        for kind in ("ffile", "fmacro"))
    env["CFLAGS"] = cflags
    env["CXXFLAGS"] = cflags
    cc = shutil.which("cc", path=env["PATH"])
    require(cc is not None, "selected build PATH has no linker driver")
    linker = common.observe_build_tool(cc, "linker driver")
    helpers = {}
    for name in ("make", "python3", "sh"):
        path = shutil.which(name, path=env["PATH"])
        require(path is not None, f"selected build PATH has no {name}")
        helpers[name] = common.observe_build_tool(path, f"build helper {name}")
    output.mkdir(mode=0o700)
    try:
        observer_logs = output / "observer-build"
        observer_logs.mkdir(mode=0o700)
        observer_environment = dict(env)
        observer_execution = common.execute(OBSERVER_ARGV, source["path"], env, observer_logs, deadline, args.max_log_bytes)
        require(observer_execution["exit"] == 0 and observer_execution["process_group_retired"], "identity observer build failed")
        observer = args.target_dir / common.TARGET / "release/examples/stylo_identity_observer"
        observer_identity = common.artifact_identity(observer)
        staged = output / "backend-stylo-worker"
        package.copy_admitted_file(args.worker, staged, worker["sha256"], worker["bytes"], package.MAX_BINARY_BYTES)
        staged.chmod(0o700)
        old_run = package.run
        package.run = lambda command: bounded_process(command, deadline, env).decode("utf-8").strip()
        try:
            before = package.elf_info(readelf, staged)
            require(bool(before["interpreter"]), "worker has no ELF interpreter")
            require(set(before["needed"]) - package.GLIBC_SONAMES == {LIBGCC},
                    "worker has an unexpected non-platform dependency")
            loader = Path(before["interpreter"])
            require(loader.is_absolute() and loader.is_file(), "worker loader is unavailable")
            loader_identity = common.observe_build_tool(str(loader), "worker loader")
            require(Path(loader_identity["path"]) == admitted_loader and loader_identity["sha256"] == args.source_loader_sha256,
                    "worker loader differs from the explicitly admitted source loader")
            linkage = bounded_process([str(loader), "--list", str(args.worker)], deadline, env).decode("utf-8")
            matches = [path for name, path in package.ldd_dependencies(linkage) if name == LIBGCC]
            require(len(matches) == 1 and Path(matches[0]).is_absolute(), "worker libgcc resolution is ambiguous")
            source_library = Path(matches[0]).resolve(strict=True)
            lib_size, lib_sha = package.admit_file_digest(source_library, package.MAX_STYLO_LIBGCC_BYTES)
            dependency = output / LIBGCC
            package.copy_admitted_file(source_library, dependency, lib_sha, lib_size, package.MAX_STYLO_LIBGCC_BYTES)
            dependency.chmod(0o700)
            dependency_before = package.elf_info(readelf, dependency)
            require(set(dependency_before["needed"]).issubset(package.GLIBC_SONAMES),
                    "libgcc has an unexpected non-platform dependency")
            dynamic = bounded_process([str(readelf), "-d", str(dependency)], deadline, env).decode("utf-8")
            require(re.findall(r"\(SONAME\).*\[(.*?)\]", dynamic) == [LIBGCC], "libgcc SONAME differs")
            dependency_command = [str(patchelf), "--set-rpath", "$ORIGIN", str(dependency)]
            package.run(dependency_command)
            dependency_after = package.elf_info(readelf, dependency)
            dynamic_after = bounded_process([str(readelf), "-d", str(dependency)], deadline, env).decode("utf-8")
            require(dependency_after["needed"] == dependency_before["needed"]
                    and set(dependency_after["needed"]).issubset(package.GLIBC_SONAMES)
                    and re.findall(r"\(SONAME\).*\[(.*?)\]", dynamic_after) == [LIBGCC]
                    and package.run([str(patchelf), "--print-rpath", str(dependency)]) == "$ORIGIN",
                    "libgcc relocation changed its loader contract")
            require_runpath(readelf, dependency, "$ORIGIN", deadline, env)
            command = [str(patchelf), "--set-interpreter", package.INTERPRETER, "--set-rpath", "$ORIGIN/../lib", str(staged)]
            package.run(command)
            after = package.elf_info(readelf, staged)
            require(after["interpreter"] == package.INTERPRETER and after["needed"] == before["needed"]
                    and package.run([str(patchelf), "--print-rpath", str(staged)]) == "$ORIGIN/../lib",
                    "worker relocation changed its loader contract")
            require_runpath(readelf, staged, "$ORIGIN/../lib", deadline, env)
        finally:
            package.run = old_run
        relocated = common.artifact_identity(staged)
        dependency_identity = common.artifact_identity(dependency)
        require(dependency_identity["bytes"] <= package.MAX_STYLO_LIBGCC_BYTES, "relocated libgcc exceeds its private-lease bound")
        package.reject_private_paths(staged, worker_private_paths + [str(path) for path in private_paths], "relocated Stylo worker")
        package.reject_private_paths(dependency, worker_private_paths + [str(path) for path in private_paths], "relocated Stylo dependency")
        require(common.artifact_identity(staged) == relocated, "worker changed during privacy scan")
        require(common.artifact_identity(dependency) == dependency_identity, "dependency changed during privacy scan")
        anchor = observe(observer, staged, dependency, deadline, env)
        require(common.artifact_identity(staged) == relocated and common.artifact_identity(dependency) == dependency_identity, "worker closure changed during identity observation")
        env["NUDOX_BUILD_STYLO_WORKER_ID"] = anchor
        execution = common.execute(ARGV, source["path"], env, output, deadline, args.max_log_bytes)
        require(execution["exit"] == 0 and execution["process_group_retired"], "anchored owner build failed")
        owner = args.target_dir / common.TARGET / "release/backend-locald"
        owner_identity = common.artifact_identity(owner)
        require_no_builder_paths(owner, private_paths + [Path(path) for path in worker_private_paths])
        require(common.clean_source_snapshot(args.source_checkout) == source and source["clean"], "source changed during owner build")
        require({name: common.tool(args, name) for name in tools} == tools, "owner toolchain changed during build")
        require(common.observe_build_tool(cc, "linker driver") == linker,
                "owner linker driver changed during build")
        for name, identity in helpers.items():
            path = shutil.which(name, path=env["PATH"])
            require(path is not None and common.observe_build_tool(path, f"build helper {name}") == identity,
                    f"owner build helper {name} changed during build")
        require({"patchelf": package.sha256(patchelf), "readelf": package.sha256(readelf)} == tool_hashes,
                "ELF tools changed during owner build")
        require(package.admit_file_digest(cli, package.MAX_BINARY_BYTES) == (cli_size, cli_sha), "CLI changed during owner build")
        require(common.artifact_identity(observer) == observer_identity, "identity observer changed during owner build")
        require(common.artifact_identity(staged) == relocated and common.artifact_identity(dependency) == dependency_identity and observe(observer, staged, dependency, deadline, env) == anchor,
                "relocated worker changed during owner build")
        require(common.observe_build_tool(str(loader), "worker loader") == loader_identity, "worker loader changed during owner build")
        require(package.admit_file_digest(source_library, package.MAX_STYLO_LIBGCC_BYTES) == (lib_size, lib_sha), "source libgcc changed during owner build")
        require(package.admit_stylo_worker(args.worker, args.worker_receipt, revision, tree, lock_sha)[1] == worker_receipt_sha,
                "raw worker receipt changed during owner build")
        receipt = {"schema": SCHEMA, "source": {"commit": revision, "tree": tree, "cargo_lock_sha256": lock_sha,
                   "clean_before": True, "clean_after": True},
                   "cli_build_manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
                   "cli": {"path": str(cli), "bytes": cli_size, "sha256": cli_sha},
                   "observer": observer_identity, "observer_argv": OBSERVER_ARGV, "observer_execution": observer_execution, "observer_environment": observer_environment,
                   "worker_build_receipt_sha256": worker_receipt_sha, "worker_source": worker, "worker_private_paths": worker_private_paths,
                   "worker": relocated, "worker_anchor": anchor,
                   "transformation": {"argv": command, "tool_sha256": tool_hashes, "before": before, "after": after, "runpath_kind": "DT_RUNPATH"},
                   "dependency_source": {"path": str(source_library), "bytes": lib_size, "sha256": lib_sha},
                   "dependency": dependency_identity,
                   "source_loader": loader_identity,
                   "source_loader_admission": {"path": str(admitted_loader), "sha256": args.source_loader_sha256},
                   "dependency_transformation": {"argv": dependency_command, "before": dependency_before, "after": dependency_after, "soname": LIBGCC, "runpath_kind": "DT_RUNPATH"},
                   "argv": ARGV, "environment": env, "linker": linker, "build_helpers": helpers,
                   "remap_schema": REMAP_SCHEMA, "c_remap_schema": C_REMAP_SCHEMA, "source_path": str(source["path"]),
                   "exit": execution["exit"], "execution": execution,
                   "toolchain": {**tools, "unchanged": True}, "owner": owner_identity}
        candidate = output / "candidate.json"
        require(len(common.shared.canonical_json(receipt)) + 1 <= 65536, "owner receipt exceeds its bound")
        common.runtime.write_new_json(candidate, receipt)
        package.admit_stylo_bound_owner(candidate, source=(revision, tree, lock_sha),
            cli_record=cli_record, cli_manifest_sha=hashlib.sha256(manifest_bytes).hexdigest(),
            raw_worker=worker, worker_receipt_sha=worker_receipt_sha, worker_private_paths=worker_private_paths)
        require(common.artifact_identity(staged) == relocated and common.artifact_identity(dependency) == dependency_identity and common.artifact_identity(owner) == owner_identity,
                "worker or owner changed before receipt publication")
        require_no_builder_paths(owner, private_paths + [Path(path) for path in worker_private_paths])
        require(common.artifact_identity(owner) == owner_identity, "owner changed during final path scan")
        require(time.monotonic() < deadline, "owner build deadline expired")
        final = output / "stylo-bound-owner-build-receipt.json"
        os.link(candidate, final)
        candidate.unlink()
        common.sync_directory(output)
        return final
    except Exception as error:
        common.runtime.write_new_json(output / "FAILED.json", {"status": "failed", "error": type(error).__name__, "detail": str(error)[:2048]})
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-checkout", "cargo", "rustc", "cargo-home", "target-dir", "build-dir", "output-dir", "worker", "worker-receipt", "cli-manifest", "patchelf", "readelf", "source-loader"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("cargo", "rustc", "patchelf", "readelf", "source-loader"):
        parser.add_argument("--" + name + "-sha256", required=True)
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--timeout-seconds", type=float, default=1800)
    parser.add_argument("--max-log-bytes", type=int, default=64 * 1024 * 1024)
    try:
        print(capture(parser.parse_args()))
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Stylo owner build refused: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
