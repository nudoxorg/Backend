#!/usr/bin/env python3
"""Write a compact provenance record around one wrapped Cargo invocation."""

from __future__ import annotations

import datetime as _datetime
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import time


def _sha256_path(path: pathlib.Path) -> str | None:
    try:
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()
    except OSError:
        return None


def _git(root: pathlib.Path, *args: str) -> bytes:
    result = subprocess.run(
        [os.environ.get("NUDOX_PROVENANCE_GIT") or shutil.which("git") or "git", "-C", str(root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.stdout if result.returncode == 0 else b""


def _git_stream_digest(root: pathlib.Path, *args: str) -> bytes:
    digest = hashlib.sha256()
    process = subprocess.Popen(
        [os.environ.get("NUDOX_PROVENANCE_GIT") or shutil.which("git") or "git", "-C", str(root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    assert process.stdout is not None
    for chunk in iter(lambda: process.stdout.read(1024 * 1024), b""):
        digest.update(chunk)
    process.stdout.close()
    process.wait()
    return digest.digest() if process.returncode == 0 else b""


def _dirty_digest(root: pathlib.Path) -> str:
    digest = hashlib.sha256()
    digest.update(_git(root, "status", "--porcelain=v1", "-z", "--untracked-files=all"))
    digest.update(_git_stream_digest(root, "diff", "--binary", "HEAD", "--"))
    untracked = _git(root, "ls-files", "--others", "--exclude-standard", "-z")
    for raw_path in untracked.split(b"\0"):
        if not raw_path:
            continue
        relative = pathlib.Path(os.fsdecode(raw_path))
        path = root / relative
        digest.update(b"\0untracked\0")
        digest.update(raw_path)
        try:
            details = path.lstat()
            digest.update(str(stat.S_IFMT(details.st_mode)).encode("ascii"))
            if stat.S_ISREG(details.st_mode):
                with path.open("rb") as source:
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        digest.update(chunk)
            elif stat.S_ISLNK(details.st_mode):
                digest.update(os.fsencode(os.readlink(path)))
        except OSError as error:
            digest.update(type(error).__name__.encode("ascii"))
    return digest.hexdigest()


def _version(command: str, *args: str) -> str | None:
    try:
        result = subprocess.run(
            [command, *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    value = result.stdout.strip()
    return value if result.returncode == 0 else None


def _command_path(command: str | None) -> pathlib.Path | None:
    if not command:
        return None
    path = pathlib.Path(command)
    if path.is_absolute():
        return path
    resolved = shutil.which(command)
    return pathlib.Path(resolved) if resolved else None


def _features(arguments: list[str]) -> dict[str, object]:
    values: list[str] = []
    all_features = False
    no_default_features = False
    targets: list[str] = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument in ("--features", "-F") and index + 1 < len(arguments):
            values.append(arguments[index + 1])
            index += 1
        elif argument.startswith("--features="):
            values.append(argument.split("=", 1)[1])
        elif argument.startswith("-F") and len(argument) > 2:
            values.append(argument[2:])
        elif argument == "--all-features":
            all_features = True
        elif argument == "--no-default-features":
            no_default_features = True
        elif argument == "--target" and index + 1 < len(arguments):
            targets.append(arguments[index + 1])
            index += 1
        elif argument.startswith("--target="):
            targets.append(argument.split("=", 1)[1])
        index += 1
    return {
        "features": values,
        "all_features": all_features,
        "no_default_features": no_default_features,
        "targets": targets,
    }


def _atomic_json(path: pathlib.Path, value: dict[str, object]) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temporary = path.with_name(path.name + f".tmp-{os.getpid()}")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(value, output, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def _begin(arguments: list[str]) -> int:
    if len(arguments) < 8:
        return 64
    root, build_dir, target_dir, runtime_wrapper, source_wrapper, cargo, rustc, rustc_wrapper, *cargo_args = arguments
    workspace = pathlib.Path(root)
    now_ns = time.time_ns()
    run_id = f"{now_ns}-{os.getpid()}"
    lockfile = workspace / "Cargo.lock"
    value: dict[str, object] = {
        "schema": 1,
        "run_id": run_id,
        "started_at_utc": _datetime.datetime.now(_datetime.timezone.utc).isoformat(),
        "started_at_ns": now_ns,
        "workspace_root": str(workspace),
        "git_head": _git(workspace, "rev-parse", "HEAD").decode("ascii", "replace").strip() or None,
        "source_dirty_sha256": _dirty_digest(workspace),
        "cargo_lock_sha256": _sha256_path(lockfile),
        "cargo_build_dir": build_dir,
        "cargo_target_dir": target_dir,
        "features": _features(cargo_args),
        "toolchain": {
            "cargo": _version(cargo, "--version"),
            "rustc": _version(rustc, "--version", "--verbose"),
            "rustc_path": rustc,
        },
        "wrapper": {
            "runtime_path": runtime_wrapper,
            "runtime_sha256": _sha256_path(pathlib.Path(runtime_wrapper)),
            "source_path": source_wrapper,
            "source_sha256": _sha256_path(pathlib.Path(source_wrapper)),
            "rustc_path": rustc_wrapper,
            "rustc_sha256": _sha256_path(_command_path(rustc_wrapper)) if _command_path(rustc_wrapper) else None,
        },
        "outputs": [],
    }
    start_path = pathlib.Path(build_dir) / f".nudox-provenance-start-{run_id}.json"
    _atomic_json(start_path, value)
    print(start_path)
    return 0


def _output_hashes(target_dir: pathlib.Path, started_at_ns: int) -> list[dict[str, object]]:
    outputs: list[dict[str, object]] = []
    if not target_dir.is_dir():
        return outputs
    for directory, subdirectories, filenames in os.walk(target_dir, followlinks=False):
        subdirectories[:] = [name for name in subdirectories if name != ".nudox-provenance"]
        for filename in filenames:
            path = pathlib.Path(directory) / filename
            try:
                details = path.lstat()
            except OSError:
                continue
            if not stat.S_ISREG(details.st_mode) or not details.st_mode & 0o111:
                continue
            if details.st_mtime_ns < started_at_ns:
                continue
            output_hash = _sha256_path(path)
            if output_hash is not None:
                outputs.append(
                    {
                        "path": path.relative_to(target_dir).as_posix(),
                        "sha256": output_hash,
                        "size_bytes": details.st_size,
                    }
                )
    outputs.sort(key=lambda item: str(item["path"]))
    return outputs


def _finish(arguments: list[str]) -> int:
    if len(arguments) != 2:
        return 64
    start_path = pathlib.Path(arguments[0])
    cargo_status = int(arguments[1])
    with start_path.open("r", encoding="utf-8") as source:
        value = json.load(source)
    root = pathlib.Path(str(value["workspace_root"]))
    target_dir = pathlib.Path(str(value["cargo_target_dir"]))
    lockfile = root / "Cargo.lock"
    value["finished_at_utc"] = _datetime.datetime.now(_datetime.timezone.utc).isoformat()
    value["source_dirty_sha256_after"] = _dirty_digest(root)
    value["source_changed_during_build"] = (
        value["source_dirty_sha256_after"] != value["source_dirty_sha256"]
    )
    value["cargo_lock_sha256_after"] = _sha256_path(lockfile)
    value["cargo_exit_status"] = cargo_status
    value["outputs"] = _output_hashes(target_dir, int(value["started_at_ns"]))
    manifest_dir = target_dir / ".nudox-provenance"
    manifest_path = manifest_dir / f"{value['run_id']}.json"
    _atomic_json(manifest_path, value)
    start_path.unlink(missing_ok=True)
    print(manifest_path)
    return 0


def main() -> int:
    if len(sys.argv) < 2:
        return 64
    if sys.argv[1] == "begin":
        return _begin(sys.argv[2:])
    if sys.argv[1] == "finish":
        return _finish(sys.argv[2:])
    return 64


if __name__ == "__main__":
    raise SystemExit(main())
