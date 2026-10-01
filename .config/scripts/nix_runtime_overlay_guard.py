#!/usr/bin/env python3
"""Verify only the source and executable inputs used by a frozen runtime overlay."""

from __future__ import annotations

import hashlib
import os
import pathlib
import selectors
import stat
import subprocess
import time
from collections.abc import Mapping

RUNTIME_OVERLAY_SOURCE_INPUTS = frozenset(
    {
        ".config/nix/checks.nix",
        ".config/nix/corpus-env.nix",
        ".config/nix/default.nix",
        ".config/nix/shells.nix",
        ".config/nix/tools.nix",
        ".config/scripts/nix-host-runtime-contract.py",
        ".config/scripts/nix_runtime_overlay_guard.py",
        "crates/engine/src/application/host.rs",
        "frontends/csharp/src/legacy/helper/AuthorityImage.cs",
        "frontends/csharp/src/legacy/helper/DocComments.cs",
        "frontends/csharp/src/legacy/helper/Extractor.cs",
        "frontends/csharp/src/legacy/helper/Program.cs",
        "frontends/csharp/src/legacy/helper/SourceLoader.cs",
        "frontends/csharp/src/legacy/helper/TypeSigWriter.cs",
        "frontends/csharp/src/legacy/helper/oracle.csproj",
        "frontends/csharp/src/legacy/helper/packages.lock.json",
        "frontends/csharp/src/legacy/oracle.rs",
        "frontends/go/src/legacy/oracle.rs",
        "frontends/python/src/legacy/checker.rs",
        "frontends/typescript/src/legacy/checker.rs",
        "tests/fleet/run-fleet.sh",
        "tests/fleet/src/lib.rs",
    }
)
GIT_COMMAND_TIMEOUT_SECONDS = 5.0
GIT_COMMAND_MAX_OUTPUT_BYTES = 16 * 1024


class OverlayInputError(ValueError):
    """An overlay input no longer matches its captured source identity."""


def sha256_file(path: pathlib.Path) -> str:
    path = pathlib.Path(path)
    try:
        entry_before = path.stat(follow_symlinks=False)
    except OSError as error:
        raise OverlayInputError(f"cannot stat overlay input {path}: {error}") from error
    if not stat.S_ISREG(entry_before.st_mode):
        raise OverlayInputError(f"overlay input is not a regular file: {path}")

    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NONBLOCK", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise OverlayInputError(f"cannot open overlay input {path}: {error}") from error

    digest = hashlib.sha256()
    size = 0
    try:
        opened_before = os.fstat(descriptor)
        if not stat.S_ISREG(opened_before.st_mode):
            raise OverlayInputError(f"opened overlay input is not a regular file: {path}")
        if _stat_identity(entry_before) != _stat_identity(opened_before):
            raise OverlayInputError(f"overlay input changed before it was opened: {path}")
        while True:
            try:
                block = os.read(descriptor, 1024 * 1024)
            except OSError as error:
                raise OverlayInputError(f"cannot read overlay input {path}: {error}") from error
            if not block:
                break
            digest.update(block)
            size += len(block)

        opened_after = os.fstat(descriptor)
        if (
            _stat_identity(opened_before) != _stat_identity(opened_after)
            or size != opened_after.st_size
        ):
            raise OverlayInputError(f"overlay input changed while it was being hashed: {path}")
        try:
            entry_after = path.stat(follow_symlinks=False)
        except OSError as error:
            raise OverlayInputError(f"overlay input disappeared while hashing: {path}") from error
        if _stat_identity(opened_after) != _stat_identity(entry_after):
            raise OverlayInputError(f"overlay input path was replaced while hashing: {path}")
        return digest.hexdigest()
    finally:
        os.close(descriptor)


def _stat_identity(metadata: os.stat_result) -> tuple[int, int, int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def _git(git: str, workspace: pathlib.Path, *arguments: str) -> str:
    try:
        process = subprocess.Popen(
            [git, "-C", str(workspace), *arguments],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise OverlayInputError(f"could not start git {' '.join(arguments)}: {error}") from error

    outputs = {process.stdout: bytearray(), process.stderr: bytearray()}
    selector = selectors.SelectSelector()
    for pipe in outputs:
        assert pipe is not None
        selector.register(pipe, selectors.EVENT_READ)
    deadline = time.monotonic() + GIT_COMMAND_TIMEOUT_SECONDS
    total = 0
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                _terminate_git(process)
                raise OverlayInputError(f"git {' '.join(arguments)} exceeded its time limit")
            ready = selector.select(remaining)
            if not ready:
                if process.poll() is None:
                    _terminate_git(process)
                    raise OverlayInputError(f"git {' '.join(arguments)} exceeded its time limit")
                continue
            for key, _ in ready:
                pipe = key.fileobj
                remaining_bytes = GIT_COMMAND_MAX_OUTPUT_BYTES - total
                block = os.read(pipe.fileno(), min(4096, remaining_bytes + 1))
                if not block:
                    selector.unregister(pipe)
                    pipe.close()
                    continue
                total += len(block)
                if total > GIT_COMMAND_MAX_OUTPUT_BYTES:
                    _terminate_git(process)
                    raise OverlayInputError(f"git {' '.join(arguments)} exceeded its output limit")
                outputs[pipe].extend(block)

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            _terminate_git(process)
            raise OverlayInputError(f"git {' '.join(arguments)} exceeded its time limit")
        returncode = process.wait(timeout=remaining)
    except subprocess.TimeoutExpired as error:
        _terminate_git(process)
        raise OverlayInputError(f"git {' '.join(arguments)} exceeded its time limit") from error
    finally:
        selector.close()
        for pipe in outputs:
            if pipe is not None and not pipe.closed:
                pipe.close()
        if process.poll() is None:
            _terminate_git(process)

    stdout = bytes(outputs[process.stdout]).decode("utf-8", errors="replace")
    stderr = bytes(outputs[process.stderr]).decode("utf-8", errors="replace")
    if returncode:
        message = stderr.strip() or stdout.strip()
        raise OverlayInputError(f"git {' '.join(arguments)} failed: {message}")
    return stdout.strip()


def _terminate_git(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        process.kill()
        process.wait()


def verify_source_snapshot(
    git: str,
    workspace: pathlib.Path,
    snapshot: Mapping[str, object],
    allowed_sources: frozenset[str],
) -> str:
    """Verify the captured source files and return the current commit.

    The captured commit is retained as provenance. A later unrelated commit is
    accepted when every tracked contract input is still byte-identical. Dirty
    contract paths, untracked replacements, missing files, and changed bytes
    are refused.
    """

    root = workspace.resolve(strict=True)
    captured_head = snapshot.get("git_head")
    files = snapshot.get("files_sha256")
    if not isinstance(captured_head, str) or not captured_head:
        raise OverlayInputError("source snapshot has no captured Git commit")
    if not allowed_sources:
        raise OverlayInputError("source guard has no closed contract-file allowlist")
    if not isinstance(files, Mapping) or not files:
        raise OverlayInputError("source snapshot has no contract-file hashes")

    relative_paths: list[str] = []
    expected_hashes: dict[str, str] = {}
    for relative, expected in files.items():
        if not isinstance(relative, str) or not isinstance(expected, str):
            raise OverlayInputError("source snapshot contains an invalid file entry")
        if len(expected) != 64 or any(character not in "0123456789abcdef" for character in expected):
            raise OverlayInputError(f"source snapshot has an invalid SHA-256 for {relative}")
        path = pathlib.PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or not path.parts:
            raise OverlayInputError(f"source path is not workspace-relative: {relative}")
        normalized = path.as_posix()
        relative_paths.append(normalized)
        expected_hashes[normalized] = expected

    if set(expected_hashes) != allowed_sources:
        missing = sorted(allowed_sources - set(expected_hashes))
        unexpected = sorted(set(expected_hashes) - allowed_sources)
        raise OverlayInputError(
            f"source snapshot input set changed: missing={missing}, unexpected={unexpected}"
        )

    pathspecs = [f":(literal){relative}" for relative in relative_paths]
    tracked = _git(git, root, "ls-files", "--error-unmatch", "--", *pathspecs)
    if set(tracked.splitlines()) != set(relative_paths):
        raise OverlayInputError("one or more captured contract files are not tracked")
    dirty = _git(
        git,
        root,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--",
        *pathspecs,
    )
    if dirty:
        raise OverlayInputError("a captured contract source path has uncommitted changes")

    for relative, expected in expected_hashes.items():
        path = root.joinpath(*pathlib.PurePosixPath(relative).parts)
        try:
            resolved = path.resolve(strict=True)
            resolved.relative_to(root)
        except (OSError, ValueError) as error:
            raise OverlayInputError(f"cannot resolve contract source {relative}: {error}") from error
        if not resolved.is_file():
            raise OverlayInputError(f"contract source is not a file: {relative}")
        if sha256_file(resolved) != expected:
            raise OverlayInputError(f"contract source changed: {relative}")
        try:
            if path.resolve(strict=True) != resolved:
                raise OverlayInputError(f"contract source path changed while hashing: {relative}")
        except OSError as error:
            raise OverlayInputError(f"contract source disappeared while hashing: {relative}") from error

    current_head = _git(git, root, "rev-parse", "HEAD")
    return current_head


def verify_provider_identity(name: str, identity: Mapping[str, object]) -> pathlib.Path:
    """Verify an exact path, resolved path, and SHA-256 for a runtime provider."""

    path_value = identity.get("path")
    resolved_value = identity.get("resolved_path")
    expected_hash = identity.get("sha256")
    if not all(isinstance(value, str) and value for value in (path_value, resolved_value, expected_hash)):
        raise OverlayInputError(f"runtime provider identity is incomplete: {name}")
    if len(expected_hash) != 64 or any(character not in "0123456789abcdef" for character in expected_hash):
        raise OverlayInputError(f"runtime provider SHA-256 is invalid: {name}")
    path = pathlib.Path(path_value)
    try:
        entry_before = path.stat(follow_symlinks=False)
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise OverlayInputError(f"cannot resolve runtime provider {name}: {error}") from error
    if str(resolved) != resolved_value:
        raise OverlayInputError(f"runtime provider path changed: {name}")
    if not resolved.is_file() or sha256_file(resolved) != expected_hash:
        raise OverlayInputError(f"runtime provider content changed: {name}")
    try:
        entry_after = path.stat(follow_symlinks=False)
        resolved_after = path.resolve(strict=True)
    except OSError as error:
        raise OverlayInputError(f"runtime provider changed while hashing: {name}") from error
    if _stat_identity(entry_before) != _stat_identity(entry_after) or resolved_after != resolved:
        raise OverlayInputError(f"runtime provider path changed while hashing: {name}")
    return path
