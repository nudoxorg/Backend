#!/usr/bin/env python3
"""Record a real offline CPU helper build inside an already admitted managed scope.

This actor allocates no permit or resource scope. Its caller supplies the existing
fleet admission, finite kernel scope, private target/build roots and cache charge.
It never runs the helper or installs a model. Successful Cargo cache reuse is valid.
"""
from __future__ import annotations

import argparse
import importlib.util
import math
import os
from pathlib import Path
import platform
import selectors
import shutil
import signal
import subprocess
import sys
import time
import tomllib


ROOT = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


shared = load("runtime_build_receipt", ROOT / "tests/journeys/scripts/runtime_build_receipt.py")
runtime = load("helper_runtime_recorder", ROOT / "tests/journeys/scripts/record-runtime-build-manifest.py")
package = load("helper_linux_package", ROOT / "tools/package/linux_release_package.py")
ARGV = ["cargo", "build", "--release", "--locked", "--no-default-features", "--features", "cpu", "-p", "backend-embedding-candle"]
TARGET = "x86_64-unknown-linux-gnu"
NAME = "backend-embedding-candle"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise shared.ReceiptError(message)


def directory(path: Path) -> Path:
    require(path.is_absolute() and not path.is_symlink() and path.is_dir(), "build inputs require existing absolute non-symlink directories")
    require(path.resolve(strict=True) == path, "build directories must use canonical paths without symlink ancestors")
    return path


def build_environment(args: argparse.Namespace) -> dict[str, str]:
    env = dict(os.environ)
    # External Git metadata belongs to the recorder, never Cargo/build scripts.
    for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
        env.pop(key, None)
    env.update({
        "PATH": str(args.cargo.parent) + os.pathsep + env.get("PATH", ""),
        "RUSTC": str(args.rustc), "CARGO_HOME": str(args.cargo_home),
        "CARGO_TARGET_DIR": str(args.target_dir), "CARGO_BUILD_BUILD_DIR": str(args.build_dir),
        "CARGO_BUILD_TARGET": TARGET, "CARGO_NET_OFFLINE": "true",
        "CARGO_BUILD_JOBS": str(args.jobs), "CARGO_INCREMENTAL": "0",
        "CARGO_CACHE_AUTO_CLEAN_FREQUENCY": "never", "CARGO_TERM_COLOR": "never",
        "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": "",
        "RUSTFLAGS": "", "CARGO_ENCODED_RUSTFLAGS": "",
    })
    require(shutil.which("cargo", path=env["PATH"]) == str(args.cargo), "PATH does not resolve the pinned Cargo executable")
    return env


def execute(command: list[str], source: Path, env: dict[str, str], output: Path, deadline: float, maximum: int) -> dict:
    """Bound actual pipe output and time, then retire this invocation's process group."""
    require(time.monotonic() < deadline, "helper build deadline expired before Cargo launch")
    process = subprocess.Popen(command, cwd=source, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True, close_fds=True)
    previous = {}
    total = 0

    def interrupted(_signum, _frame):
        raise shared.ReceiptError("helper build interrupted")

    try:
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            previous[signum] = signal.signal(signum, interrupted)
        private = lambda path, flags: os.open(path, flags, 0o600)
        with selectors.DefaultSelector() as selector, open(output / "cargo.stdout", "xb", opener=private) as stdout, open(output / "cargo.stderr", "xb", opener=private) as stderr:
            selector.register(process.stdout, selectors.EVENT_READ, stdout)
            selector.register(process.stderr, selectors.EVENT_READ, stderr)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                require(remaining > 0, "helper build exceeded its finite deadline")
                for key, _ in selector.select(min(0.1, remaining)):
                    chunk = os.read(key.fileobj.fileno(), min(65536, maximum - total + 1))
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    allowed = min(len(chunk), maximum - total)
                    key.data.write(chunk[:allowed])
                    total += allowed
                    require(len(chunk) == allowed, "helper build exceeded its combined output bound")
            code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        clean = runtime.terminate_owned_group(process)
        require(clean, "helper build process group did not retire")
        return {"exit": code, "pid": process.pid, "process_group_retired": clean, "output_bytes": total}
    finally:
        # Also covers signals, log overflow, timeout and pipe/launch failures.
        runtime.terminate_owned_group(process)
        try:
            process.wait(timeout=2)
        finally:
            try:
                process.stdout.close()
                process.stderr.close()
            finally:
                for signum, handler in previous.items():
                    signal.signal(signum, handler)


def tool(args: argparse.Namespace, name: str) -> dict:
    path = getattr(args, name)
    value = shared.stable_tool_file(path, name, executable=True)
    require(value["sha256"] == getattr(args, name + "_sha256"), f"{name} differs from its admitted SHA256")
    require(value["path"] == str(path), f"{name} must be selected by its resolved executable path")
    return {**value, "version": shared.tool_version(path, name)}


def artifact_identity(path: Path) -> dict:
    before = path.lstat()
    size, digest = package.admit_file_digest(path, package.MAX_BINARY_BYTES)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        opened = os.fstat(stream.fileno())
        header = stream.read(4096)
    after = path.lstat()
    revision = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns, value.st_mode)
    require(revision(before) == revision(opened) == revision(after) and os.access(path, os.X_OK), "helper ELF changed or is not executable")
    require(header.startswith(b"\x7fELF") and shared.architecture_from_binary_header(header, size, "x86_64") == "x86_64", "helper is not an x86_64 ELF")
    return {"path": str(path), "bytes": size, "sha256": digest, "architecture": "x86_64", "device": after.st_dev, "inode": after.st_ino}


def sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def capture(args: argparse.Namespace) -> Path:
    require(platform.system() == "Linux" and shared.normalize_arch(platform.machine()) == "x86_64", "CPU helper producer requires Linux x86_64")
    require(type(args.jobs) is int and 1 <= args.jobs <= 4, "jobs must be between one and four")
    require(0 < args.timeout_seconds <= 1800 and 0 < args.max_log_bytes <= 64 * 1024 * 1024, "build bounds exceed the supported finite envelope")
    deadline = time.monotonic() + args.timeout_seconds
    if args.deadline_monotonic is not None:
        require(math.isfinite(args.deadline_monotonic) and args.deadline_monotonic > 0, "managed scope deadline must be finite and positive")
        deadline = min(deadline, args.deadline_monotonic)
    require(deadline > time.monotonic(), "managed scope deadline has expired")
    source = shared.source_snapshot(args.source_checkout)
    require(source["clean"], "refusing to build from a dirty source checkout")
    source_path = source["path"]
    require(source_path == ROOT, "run the producer from the exact source checkout being built")
    for name in ("cargo_home", "target_dir", "build_dir"):
        setattr(args, name, directory(getattr(args, name)))
    output = runtime.output_path(args.output_dir, "helper evidence directory")
    require(output.resolve(strict=False) == output, "helper evidence must use a canonical path without symlink ancestors")
    paths = [args.cargo_home, args.target_dir, args.build_dir, output.resolve(strict=False)]
    require(len(set(paths)) == len(paths), "helper evidence, cache, target and build directories must be distinct")
    require(all(path != source_path and source_path not in path.parents for path in paths), "helper mutable directories must be outside source")
    require(all(output.resolve(strict=False) not in path.parents and path not in output.resolve(strict=False).parents for path in paths[:-1]), "helper evidence must not overlap mutable build directories")
    manifest = tomllib.loads(shared.read_regular(source_path / "tools/embedding-candle/Cargo.toml", 65536, "helper manifest").decode("utf-8"))
    require(manifest.get("package", {}).get("name") == NAME and [item.get("name") for item in manifest.get("bin", [])] == [NAME], "source does not declare the fixed helper binary")
    tools = {name: tool(args, name) for name in ("cargo", "rustc")}
    env = build_environment(args)
    output.mkdir(mode=0o700)
    started = runtime.utc_now()
    try:
        execution = execute(ARGV, source_path, env, output, deadline, args.max_log_bytes)
        require(execution["exit"] == 0 and execution["process_group_retired"], "Cargo helper build did not succeed with a retired process group")
        source_after = shared.source_snapshot(args.source_checkout)
        require(source_after == source and source_after["clean"], "helper source changed during the actual build")
        require({name: tool(args, name) for name in tools} == tools, "helper toolchain changed during the actual build")
        artifact = args.target_dir / TARGET / "release" / NAME
        identity = artifact_identity(artifact)
        logs = {name: shared.stable_file(output / f"cargo.{name}", f"Cargo {name}") for name in ("stdout", "stderr")}
        require(sum(value["bytes"] for value in logs.values()) <= args.max_log_bytes, "Cargo logs exceed their admitted combined bound")
        receipt = {
            "schema": "nudox.embedding-helper-build-receipt.v1", "preset": "candle-minilm-cpu-v1",
            "source": {"commit": source["commit"], "tree": source["tree"], "cargo_lock_sha256": source["cargo_lock_sha256"], "clean_before": True, "clean_after": True},
            "argv": ARGV, "exit": execution["exit"], "artifact": identity,
            "toolchain": {**tools, "unchanged": True}, "execution": execution,
            "started_utc": started, "finished_utc": runtime.utc_now(), "logs": logs,
            "environment": {key: env[key] for key in ("RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_HOME", "CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "CARGO_BUILD_TARGET", "CARGO_NET_OFFLINE", "CARGO_BUILD_JOBS", "CARGO_INCREMENTAL", "CARGO_CACHE_AUTO_CLEAN_FREQUENCY")},
            "bounds": {"timeout_seconds": args.timeout_seconds, "deadline_monotonic": deadline, "combined_output_bytes": args.max_log_bytes},
        }
        candidate = output / "candidate.json"
        require(len(shared.canonical_json(receipt)) + 1 <= 65536, "helper receipt exceeds consumer bound")
        runtime.write_new_json(candidate, receipt)
        package.admit_embedding_helper(artifact, candidate, source["commit"], source["tree"], source["cargo_lock_sha256"])
        require(artifact_identity(artifact) == identity, "helper changed before receipt publication")
        require(time.monotonic() < deadline, "helper proof exceeded its finite deadline")
        final = output / "embedding-helper-build-receipt.json"
        os.link(candidate, final)  # No overwrite, after real consumer admission.
        try:
            candidate.unlink()
            sync_directory(output)
        except OSError:
            final.unlink()  # Only our successful exclusive link, never an existing name.
            raise
        return final
    except Exception as error:
        runtime.write_new_json(output / "FAILED.json", {"status": "failed", "error": type(error).__name__, "detail": str(error)[:2048]})
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-checkout", "cargo", "rustc", "cargo-home", "target-dir", "build-dir", "output-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("cargo", "rustc"):
        parser.add_argument("--" + name + "-sha256", required=True)
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--timeout-seconds", type=float, default=1800)
    parser.add_argument("--deadline-monotonic", type=float)
    parser.add_argument("--max-log-bytes", type=int, default=64 * 1024 * 1024)
    try:
        print(capture(parser.parse_args()))
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"helper build refused: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
