#!/usr/bin/env python3
"""Run workspace gates against one immutable clean Git candidate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


def git(repo: Path, *arguments: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(repo), *arguments],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return completed.stdout.strip()


def identity(repo: Path) -> dict[str, str]:
    return {
        "commit": git(repo, "rev-parse", "HEAD"),
        "tree": git(repo, "rev-parse", "HEAD^{tree}"),
        "status": git(repo, "status", "--porcelain=v1"),
    }


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def cargo_commands(manifest: str) -> tuple[tuple[str, ...], ...]:
    prefix = ("cargo",)
    selector = ("--manifest-path", manifest)
    return (
        prefix + ("fmt",) + selector + ("--all", "--", "--check"),
        prefix
        + ("test",)
        + selector
        + (
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--no-fail-fast",
            "--",
            "--test-threads=1",
        ),
        prefix
        + ("test",)
        + selector
        + ("--offline", "--locked", "--workspace", "--doc", "--no-fail-fast"),
        prefix
        + ("clippy",)
        + selector
        + ("--offline", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"),
        prefix
        + ("doc",)
        + selector
        + ("--offline", "--locked", "--workspace", "--no-deps"),
    )


def run_command(
    repo: Path,
    output: Path,
    environment: dict[str, str],
    index: int,
    command: tuple[str, ...],
) -> dict[str, Any]:
    log = output / f"{index:02d}.log"
    started = datetime.now(UTC).isoformat()
    with log.open("wb") as sink:
        completed = subprocess.run(
            command,
            cwd=repo,
            env=environment,
            stdout=sink,
            stderr=subprocess.STDOUT,
            check=False,
        )
    finished = datetime.now(UTC).isoformat()
    return {
        "argv": list(command),
        "started": started,
        "finished": finished,
        "exit": completed.returncode,
        "log": log.name,
        "log_bytes": log.stat().st_size,
        "log_sha256": digest(log),
        "identity_after": identity(repo),
    }


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--label", required=True)
    parser.add_argument("--manifest", action="append", required=True)
    parser.add_argument("--target-dir", type=Path)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    repo = arguments.repo.resolve(strict=True)
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    before = identity(repo)
    summary: dict[str, Any] = {
        "label": arguments.label,
        "repo": str(repo),
        "identity_before": before,
        "commands": [],
    }
    if before["status"]:
        summary["result"] = "DIRTY_BEFORE"
        (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        return 2

    environment = os.environ.copy()
    environment.pop("RUSTC_WRAPPER", None)
    if arguments.target_dir is not None:
        environment["CARGO_TARGET_DIR"] = str(arguments.target_dir.resolve())

    command_index = 0
    for manifest in arguments.manifest:
        for command in cargo_commands(manifest):
            command_index += 1
            result = run_command(repo, output, environment, command_index, command)
            summary["commands"].append(result)
            after = result["identity_after"]
            if result["exit"] != 0 or after != before:
                summary["result"] = "FAILED" if result["exit"] else "CANDIDATE_MOVED"
                summary["identity_after"] = after
                (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
                return 1

    summary["identity_after"] = identity(repo)
    summary["result"] = "PASS"
    summary_path = output / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"{arguments.label} PASS {before['commit']} {before['tree']} {digest(summary_path)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
