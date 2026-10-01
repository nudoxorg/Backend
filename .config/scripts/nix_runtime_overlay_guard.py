#!/usr/bin/env python3
"""Verify only the source and executable inputs used by a frozen runtime overlay."""

from __future__ import annotations

import hashlib
import pathlib
import subprocess
from collections.abc import Mapping


class OverlayInputError(ValueError):
    """An overlay input no longer matches its captured source identity."""


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _git(git: str, workspace: pathlib.Path, *arguments: str) -> str:
    result = subprocess.run(
        [git, "-C", str(workspace), *arguments],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        message = result.stderr.strip() or result.stdout.strip()
        raise OverlayInputError(f"git {' '.join(arguments)} failed: {message}")
    return result.stdout.strip()


def verify_source_snapshot(
    git: str,
    workspace: pathlib.Path,
    snapshot: Mapping[str, object],
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
    if not isinstance(files, Mapping) or not files:
        raise OverlayInputError("source snapshot has no contract-file hashes")

    relative_paths: list[str] = []
    expected_hashes: dict[str, str] = {}
    for relative, expected in files.items():
        if not isinstance(relative, str) or not isinstance(expected, str):
            raise OverlayInputError("source snapshot contains an invalid file entry")
        path = pathlib.PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or not path.parts:
            raise OverlayInputError(f"source path is not workspace-relative: {relative}")
        normalized = path.as_posix()
        relative_paths.append(normalized)
        expected_hashes[normalized] = expected

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

    current_head = _git(git, root, "rev-parse", "HEAD")
    return current_head


def verify_provider_identity(name: str, identity: Mapping[str, object]) -> pathlib.Path:
    """Verify an exact path, resolved path, and SHA-256 for a runtime provider."""

    path_value = identity.get("path")
    resolved_value = identity.get("resolved_path")
    expected_hash = identity.get("sha256")
    if not all(isinstance(value, str) and value for value in (path_value, resolved_value, expected_hash)):
        raise OverlayInputError(f"runtime provider identity is incomplete: {name}")
    path = pathlib.Path(path_value)
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise OverlayInputError(f"cannot resolve runtime provider {name}: {error}") from error
    if str(resolved) != resolved_value:
        raise OverlayInputError(f"runtime provider path changed: {name}")
    if not resolved.is_file() or sha256_file(resolved) != expected_hash:
        raise OverlayInputError(f"runtime provider content changed: {name}")
    return path
