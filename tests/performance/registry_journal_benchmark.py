#!/usr/bin/env python3
"""Measure production registry search against the retained Maven journal/QRELs.

The runner never builds binaries. It starts the supplied locald executable on
a private copy of a prepared workspace, drives the supplied CLI through the
real ``index-search`` command, follows every cursor, and checks every result
against labels recorded independently from the source PURLs.

The first pass measures the supplied workspace's first open. It may build a
projection only when the copied template did not already contain one; a
retained projection is reported as an existing-index open. A second pass
reopens the same workspace in a fresh locald process. Timings include one CLI
process and local transport round trip per page; they are end-to-end surface
timings, not claims about scorer-only latency.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any


DEFAULT_JOURNAL = Path(
    "/Users/mileswirht/Downloads/backend/.local/harness/f-data-all/data/"
    "registry-discovery/catalog.journal"
)
DEFAULT_LABELS = Path(
    "/Users/mileswirht/Downloads/backend/.local/live-maven-replay/"
    "20260930T054321Z/case/runtime/independent-labels.json"
)
DEFAULT_WORKSPACE = Path(
    "/Users/mileswirht/Downloads/backend/.local/live-maven-replay/"
    "20260930T054321Z/case/workspace"
)
DEFAULT_MAVEN_JOURNAL_SHA256 = "7b156608e0427b60a7fd394f1d4d0c4b68aa7d7df6d55f0b12a1da80f9f36899"
DEFAULT_MAVEN_LABELS_SHA256 = "16475bf6c658d380873e01c5555e888093a052cb372699690f4e19b9c7d927bd"
MAX_JOURNAL_BYTES = 128 * 1024 * 1024
MAX_LABEL_BYTES = 16 * 1024 * 1024
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_LOCK_BYTES = 16 * 1024 * 1024


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_frozen_file(path: Path, maximum_bytes: int, label: str) -> bytes:
    """Read one bounded regular file and reject links and mid-read replacement."""
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        raise ValueError(f"{label} must be a single-link regular file: {path}")
    if before.st_size > maximum_bytes:
        raise ValueError(f"{label} exceeds the {maximum_bytes}-byte bound: {path}")
    with path.open("rb") as stream:
        payload = stream.read(maximum_bytes + 1)
    after = path.lstat()
    if (
        len(payload) > maximum_bytes
        or len(payload) != before.st_size
        or (before.st_dev, before.st_ino, before.st_size)
        != (after.st_dev, after.st_ino, after.st_size)
        or not stat.S_ISREG(after.st_mode)
        or after.st_nlink != 1
    ):
        raise ValueError(f"{label} changed while it was being frozen: {path}")
    return payload


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def owner_environment(endpoint: Path) -> dict[str, str]:
    """Remove inherited service policy and prevent CLI daemon auto-start."""
    environment = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("BACKEND_")
    }
    environment["BACKEND_LOCALD_BIN"] = str(endpoint.parent / "no-autostart-locald")
    return environment


def tree_bytes(path: Path) -> int:
    total = 0
    for entry in path.rglob("*"):
        if entry.is_symlink():
            raise ValueError(f"refusing symlink in measured workspace: {entry}")
        if entry.is_file():
            total = total + entry.stat().st_size
    return total


def make_private_owner_file(path: Path) -> None:
    """Restrict one copied owner-state file without following links."""
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise ValueError(f"copied owner-state path is not a single-link file: {path}")
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise ValueError(f"copied owner-state path is not owned by this user: {path}")
    os.chmod(path, 0o600, follow_symlinks=False)
    verified = path.lstat()
    if (
        not stat.S_ISREG(verified.st_mode)
        or verified.st_nlink != 1
        or stat.S_IMODE(verified.st_mode) != 0o600
    ):
        raise ValueError(f"could not make copied owner-state file private: {path}")


def percentile(samples: list[int], percent: int) -> int | None:
    if not samples:
        return None
    ordered = sorted(samples)
    index = max(0, (percent * len(ordered) + 99) // 100 - 1)
    return ordered[index]


def latency_summary(samples: list[int]) -> dict[str, int | None]:
    return {
        "samples": len(samples),
        "p50_ms": round(percentile(samples, 50) / 1_000_000, 3)
        if samples
        else None,
        "p95_ms": round(percentile(samples, 95) / 1_000_000, 3)
        if samples
        else None,
        "p99_ms": round(percentile(samples, 99) / 1_000_000, 3)
        if samples
        else None,
    }


def process_rss_bytes(pid: int) -> int | None:
    # The saved macOS shell puts a restricted GUI-tools `ps` first on PATH;
    # it refuses RSS without entitlement. Prefer the system process tool
    # there, then retain PATH lookup for other platforms.
    commands = (
        [["/bin/ps", "-o", "rss=", "-p", str(pid)], ["ps", "-o", "rss=", "-p", str(pid)]]
        if platform.system() == "Darwin"
        else [["ps", "-o", "rss=", "-p", str(pid)]]
    )
    for command in commands:
        try:
            result = subprocess.run(
                command,
                text=True,
                capture_output=True,
                check=False,
            )
        except OSError:
            continue
        if result.returncode == 0 and result.stdout.strip():
            try:
                # macOS and BSD `ps` report RSS in KiB.
                return int(result.stdout.strip().splitlines()[-1]) * 1024
            except ValueError:
                continue
    return None


class RssSampler:
    def __init__(self, pid: int) -> None:
        self.pid = pid
        self.values: list[int] = []
        self.stop_event = threading.Event()
        self.thread = threading.Thread(target=self._sample, daemon=True)

    def _sample(self) -> None:
        while not self.stop_event.is_set():
            value = process_rss_bytes(self.pid)
            if value is not None:
                self.values.append(value)
            self.stop_event.wait(0.05)

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> int | None:
        self.stop_event.set()
        self.thread.join()
        return max(self.values, default=process_rss_bytes(self.pid))


def snapshot_executable(path: Path) -> dict[str, Any]:
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise ValueError(f"executable is not a runnable file: {resolved}")
    before = resolved.lstat()
    if not stat.S_ISREG(before.st_mode):
        raise ValueError(f"executable is not a regular file: {resolved}")
    digest = sha256(resolved)
    after = resolved.lstat()
    if (before.st_dev, before.st_ino, before.st_size) != (
        after.st_dev,
        after.st_ino,
        after.st_size,
    ) or not stat.S_ISREG(after.st_mode):
        raise ValueError(f"executable changed while its digest was measured: {resolved}")
    return {"path": str(resolved), "sha256": digest, "bytes": after.st_size}


def run_cli_page(
    cli: Path,
    project: Path,
    workspace: Path,
    endpoint: Path,
    query: str,
    limit: int,
    cursor: str | None,
    timeout_seconds: float,
) -> tuple[dict[str, Any], int]:
    command = [
        str(cli),
        "--json",
        "--detail",
        "full",
        "--project",
        str(project),
        "--workspace",
        str(workspace),
        "--endpoint",
        str(endpoint),
        "--limit",
        str(limit),
        "index-search",
        query,
    ]
    if cursor is not None:
        command.extend(("--cursor", cursor))
    environment = owner_environment(endpoint)
    started = time.perf_counter_ns()
    completed = subprocess.run(
        command,
        cwd=project,
        env=environment,
        text=True,
        capture_output=True,
        timeout=timeout_seconds,
        check=False,
    )
    elapsed = time.perf_counter_ns() - started
    if completed.returncode:
        raise RuntimeError(
            f"CLI failed ({completed.returncode}) for query {query!r}: "
            f"{completed.stderr[-3000:]} {completed.stdout[-1000:]}"
        )
    try:
        response = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ValueError(f"CLI did not return JSON for {query!r}: {completed.stdout[-1000:]}") from error
    if not isinstance(response, dict) or response.get("answer") != "product":
        raise ValueError(f"unexpected CLI answer for {query!r}: {response!r}")
    return response, elapsed


def search_query(
    cli: Path,
    project: Path,
    workspace: Path,
    endpoint: Path,
    query: str,
    limit: int,
    timeout_seconds: float,
) -> dict[str, Any]:
    cursor: str | None = None
    seen_cursors: set[str] = set()
    observed: list[str] = []
    page_latencies: list[int] = []
    page_sizes: list[int] = []
    snapshot: list[int] | None = None
    started = time.perf_counter_ns()
    for page_number in range(1, 10_001):
        response, elapsed = run_cli_page(
            cli,
            project,
            workspace,
            endpoint,
            query,
            limit,
            cursor,
            timeout_seconds,
        )
        page_latencies.append(elapsed)
        records = response.get("records")
        if not isinstance(records, list):
            raise ValueError(f"query {query!r} response has no records array")
        if any(not isinstance(record, dict) for record in records):
            raise ValueError(f"query {query!r} returned a malformed non-object record")
        operands = [record.get("operand") for record in records]
        if any(not isinstance(operand, str) for operand in operands):
            raise ValueError(f"query {query!r} returned a record without an exact operand")
        observed.extend(operands)
        page_sizes.append(len(operands))
        info = response.get("index_search_page")
        if not isinstance(info, dict):
            raise ValueError(f"query {query!r} response has no index-search page envelope")
        page_snapshot = info.get("snapshot")
        if (
            not isinstance(page_snapshot, list)
            or len(page_snapshot) != 32
            or any(not isinstance(value, int) or value < 0 or value > 255 for value in page_snapshot)
        ):
            raise ValueError(f"query {query!r} returned a malformed snapshot identity")
        if snapshot is None:
            snapshot = page_snapshot
        elif page_snapshot != snapshot:
            raise ValueError(f"query {query!r} cursor chain crossed index snapshots")
        next_cursor = info.get("next_cursor")
        if next_cursor is None:
            break
        if not isinstance(next_cursor, str) or not next_cursor or next_cursor in seen_cursors:
            raise ValueError(f"query {query!r} returned a malformed or repeating cursor")
        seen_cursors.add(next_cursor)
        cursor = next_cursor
    else:
        raise ValueError(f"query {query!r} exceeded the cursor page limit")

    if len(observed) != len(set(observed)):
        raise ValueError(f"query {query!r} returned duplicate operands across pages")
    return {
        "elapsed_ns": time.perf_counter_ns() - started,
        "page_elapsed_ns": page_latencies,
        "page_sizes": page_sizes,
        "observed": observed,
        "snapshot": snapshot,
    }


def wait_ready(
    locald: subprocess.Popen[str],
    cli: Path,
    project: Path,
    workspace: Path,
    endpoint: Path,
    timeout_seconds: float,
) -> tuple[int, dict[str, Any]]:
    started = time.perf_counter_ns()
    deadline = time.monotonic() + timeout_seconds
    last_error = ""
    environment = owner_environment(endpoint)
    while time.monotonic() < deadline:
        if locald.poll() is not None:
            raise RuntimeError(f"locald exited before readiness with code {locald.returncode}")
        try:
            completed = subprocess.run(
                [
                    str(cli),
                    "--json",
                    "--project",
                    str(project),
                    "--workspace",
                    str(workspace),
                    "--endpoint",
                    str(endpoint),
                    "health",
                ],
                cwd=project,
                env=environment,
                text=True,
                capture_output=True,
                timeout=min(10.0, timeout_seconds),
                check=False,
            )
            if completed.returncode == 0:
                value = json.loads(completed.stdout)
                if isinstance(value, dict) and value.get("answer") == "status":
                    return time.perf_counter_ns() - started, value
                last_error = f"health returned an unexpected answer: {value!r}"
            else:
                last_error = completed.stderr[-2000:] or completed.stdout[-1000:]
        except (subprocess.TimeoutExpired, json.JSONDecodeError, OSError) as error:
            last_error = str(error)
        time.sleep(0.1)
    raise TimeoutError(f"locald did not become query-ready: {last_error}")


def stop_owner(process: subprocess.Popen[str]) -> int | None:
    if process.poll() is None:
        process.terminate()
        try:
            return process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
    return process.wait(timeout=5)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--locald", type=Path, required=True, help="prebuilt backend-locald executable")
    parser.add_argument("--cli", type=Path, required=True, help="prebuilt backend-cli executable")
    parser.add_argument(
        "--build-manifest",
        type=Path,
        required=True,
        help="frozen runtime-build manifest for these exact binaries",
    )
    parser.add_argument("--journal", type=Path, default=DEFAULT_JOURNAL)
    parser.add_argument("--labels", type=Path, default=DEFAULT_LABELS)
    parser.add_argument("--workspace-template", type=Path, default=DEFAULT_WORKSPACE)
    parser.add_argument("--project-template", type=Path, default=None)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--allow-non-maven-dataset",
        action="store_true",
        help=(
            "allow a different frozen registry-discovery journal only when its "
            "independent labels and workspace template bind the exact same SHA-256"
        ),
    )
    parser.add_argument("--limit", type=int, default=10)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timeout-seconds", type=float, default=90.0)
    parser.add_argument(
        "--slot-granted",
        action="store_true",
        help="required before starting prebuilt locald/CLI; set only after root assigns the lane",
    )
    args = parser.parse_args()

    if not args.slot_granted:
        parser.error("refusing to run owner processes until the root grants a measurement slot")
    if args.limit < 1 or args.limit > 200 or args.repetitions < 2:
        parser.error("--limit must be 1..200 and --repetitions must be at least 2")
    journal = args.journal.resolve(strict=True)
    labels_path = args.labels.resolve(strict=True)
    workspace_template = args.workspace_template.resolve(strict=True)
    project_template = (args.project_template or workspace_template.parent / "project").resolve(strict=True)
    locald_path = args.locald.resolve(strict=True)
    cli_path = args.cli.resolve(strict=True)
    build_manifest_path = args.build_manifest.resolve(strict=True)
    script_path = Path(__file__).resolve(strict=True)
    repository = script_path.parents[2]
    lock_path = repository / "Cargo.lock"

    journal_bytes = read_frozen_file(journal, MAX_JOURNAL_BYTES, "registry journal")
    labels_bytes = read_frozen_file(labels_path, MAX_LABEL_BYTES, "independent labels")
    manifest_bytes = read_frozen_file(build_manifest_path, MAX_MANIFEST_BYTES, "build manifest")
    lock_bytes = read_frozen_file(lock_path, MAX_LOCK_BYTES, "Cargo.lock")
    runner_bytes = read_frozen_file(script_path, MAX_MANIFEST_BYTES, "benchmark runner")
    journal_digest = sha256_bytes(journal_bytes)
    labels_digest = sha256_bytes(labels_bytes)
    manifest_digest = sha256_bytes(manifest_bytes)
    lock_digest = sha256_bytes(lock_bytes)
    runner_digest = sha256_bytes(runner_bytes)
    labels = json.loads(labels_bytes)
    build_manifest = json.loads(manifest_bytes)
    if not isinstance(labels, dict) or not isinstance(build_manifest, dict):
        raise ValueError("labels and build manifest must each be JSON objects")
    if build_manifest.get("schema") != "nudox.runtime-build-manifest.v1":
        raise ValueError("runtime build manifest has an unsupported schema")
    source_manifest = build_manifest.get("source")
    if not isinstance(source_manifest, dict) or not source_manifest.get("clean"):
        raise ValueError("runtime build manifest must identify a clean source revision")
    executable_manifest = build_manifest.get("executables")
    if not isinstance(executable_manifest, dict):
        raise ValueError("runtime build manifest must contain an executable map")
    git_revision_result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repository, text=True, capture_output=True, check=False
    )
    if git_revision_result.returncode:
        raise RuntimeError(f"could not identify source revision: {git_revision_result.stderr[-2000:]}")
    git_revision = git_revision_result.stdout.strip()
    if source_manifest.get("commit") != git_revision:
        raise ValueError("runtime build manifest source commit does not match the benchmark source tree")
    git_status_result = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=repository,
        text=True,
        capture_output=True,
        check=False,
    )
    if git_status_result.returncode or git_status_result.stdout.strip():
        raise ValueError("benchmark source tree must be clean before measurement")
    if len(journal_bytes) > MAX_JOURNAL_BYTES:
        raise ValueError("registry journal exceeded its frozen read bound")
    if len(journal_bytes) != journal.stat().st_size:
        raise ValueError("registry journal size changed after its frozen read")
    for name, executable in (("backend-locald", locald_path), ("backend-cli", cli_path)):
        recorded = executable_manifest.get(name)
        current = snapshot_executable(executable)
        if not isinstance(recorded, dict) or any(
            recorded.get(key) != current.get(key) for key in ("path", "sha256", "bytes")
        ):
            raise ValueError(f"{name} does not match the frozen runtime build manifest")
    output = args.output.resolve(strict=False)
    if output.exists():
        parser.error(f"output already exists; choose a fresh path: {output}")
    queries = labels.get("queries")
    if not isinstance(queries, list) or not queries:
        raise ValueError("independent labels must contain a nonempty query list")
    dataset_kind = labels.get("dataset_kind", "retained-maven")
    if args.allow_non_maven_dataset and (
        not isinstance(labels.get("dataset_kind"), str)
        or not labels["dataset_kind"].strip()
        or not isinstance(labels.get("scope"), str)
        or not labels["scope"].strip()
    ):
        raise ValueError(
            "non-Maven labels must identify dataset_kind and an independently recorded scope"
        )
    for item in queries:
        if (
            not isinstance(item, dict)
            or not isinstance(item.get("query"), str)
            or not item["query"].strip()
            or not isinstance(item.get("expected"), list)
            or any(not isinstance(value, str) for value in item["expected"])
            or len(item["expected"]) != len(set(item["expected"]))
        ):
            raise ValueError(f"malformed independent query label: {item!r}")
    journal_size = len(journal_bytes)
    if labels.get("journal_sha256") != journal_digest:
        raise ValueError("independent labels were recorded against a different journal")
    if not args.allow_non_maven_dataset and labels_digest != DEFAULT_MAVEN_LABELS_SHA256:
        raise ValueError("default Maven labels do not match the frozen independent-label snapshot")
    if (
        not args.allow_non_maven_dataset
        and (journal_size != 722_663 or journal_digest != DEFAULT_MAVEN_JOURNAL_SHA256)
    ):
        raise ValueError(f"expected the retained 722,663-byte Maven journal, found {journal_size}")
    template_journal = workspace_template / "registry-discovery" / "catalog.journal"
    template_journal_bytes = read_frozen_file(
        template_journal, MAX_JOURNAL_BYTES, "workspace template discovery journal"
    )
    if sha256_bytes(template_journal_bytes) != journal_digest:
        raise ValueError("workspace template does not contain the exact label-bound journal")
    if not workspace_template.is_dir() or not project_template.is_dir():
        raise ValueError("workspace and project templates must be directories")
    workspace_template_bytes = tree_bytes(workspace_template)
    project_template_bytes = tree_bytes(project_template)

    executable_snapshots = {
        "backend-locald": snapshot_executable(locald_path),
        "backend-cli": snapshot_executable(cli_path),
    }

    def verify_frozen_inputs(stage: str) -> None:
        frozen_paths = (
            (journal, journal_bytes, MAX_JOURNAL_BYTES, "registry journal"),
            (labels_path, labels_bytes, MAX_LABEL_BYTES, "independent labels"),
            (build_manifest_path, manifest_bytes, MAX_MANIFEST_BYTES, "build manifest"),
            (lock_path, lock_bytes, MAX_LOCK_BYTES, "Cargo.lock"),
            (script_path, runner_bytes, MAX_MANIFEST_BYTES, "benchmark runner"),
        )
        for path, expected_bytes, maximum, label in frozen_paths:
            if read_frozen_file(path, maximum, label) != expected_bytes:
                raise ValueError(f"{label} changed {stage}: {path}")
        for name, executable in (("backend-locald", locald_path), ("backend-cli", cli_path)):
            if snapshot_executable(executable) != executable_snapshots[name]:
                raise ValueError(f"{name} binary changed {stage}: {executable}")
        revision = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=repository, text=True, capture_output=True, check=False
        )
        status = subprocess.run(
            ["git", "status", "--porcelain", "--untracked-files=all"],
            cwd=repository,
            text=True,
            capture_output=True,
            check=False,
        )
        if revision.returncode or revision.stdout.strip() != git_revision or status.returncode or status.stdout.strip():
            raise ValueError(f"source tree changed {stage}")

    verify_frozen_inputs("before preparing the private workspace")
    output.mkdir(parents=True, mode=0o700)
    work = output / "private-run"
    work.mkdir(mode=0o700)
    workspace = work / "workspace"
    project = work / "project"
    shutil.copytree(workspace_template, workspace, symlinks=True)
    shutil.copytree(project_template, project, symlinks=True)
    registry_directory = workspace / "registry"
    if not stat.S_ISDIR(registry_directory.lstat().st_mode):
        raise ValueError(f"copied registry state directory is not a real directory: {registry_directory}")
    make_private_owner_file(registry_directory / "advisory-authority.json")
    if sha256_bytes(
        read_frozen_file(
            workspace / "registry-discovery" / "catalog.journal",
            MAX_JOURNAL_BYTES,
            "private discovery journal",
        )
    ) != journal_digest:
        raise ValueError("private workspace copy changed the labeled journal")

    search_root = workspace / "registry-discovery" / "catalog-search-v1"
    if search_root.is_symlink():
        raise ValueError(f"refusing symlinked search projection in private workspace: {search_root}")
    if search_root.exists() and not search_root.is_dir():
        raise ValueError(f"search projection path is not a directory: {search_root}")
    projection_existed_before = search_root.is_dir()
    projection_bytes_before = tree_bytes(search_root) if projection_existed_before else 0

    socket_dir = Path(tempfile.mkdtemp(prefix="nx-rij-"))
    endpoint = socket_dir / "locald.sock"
    log = (output / "locald.log").open("w", encoding="utf-8")
    environment = owner_environment(endpoint)
    locald_command = [
        str(locald_path),
        "--workspace",
        str(workspace),
        "--endpoint",
        str(endpoint),
        "--profile",
        "builtin",
        "--registry-offline",
        "--registry-discovery-offline",
        "--advisory-offline",
        "--forge-offline",
        "--idle-timeout-ms",
        "0",
    ]
    first_owner_pid: int | None = None
    reopen_owner_pid: int | None = None
    owner_rss: int | None = None
    owner_rss_sample_count = 0
    startup_ns = 0
    passes: dict[str, list[dict[str, Any]]] = {"first_open": [], "reopen": []}
    projection_bytes_after_first_open: int | None = None
    workspace_bytes_before = tree_bytes(workspace)
    workspace_bytes_after_first_open: int | None = None
    first_exit: int | None = None
    first_health: dict[str, Any] | None = None
    reopen_health: dict[str, Any] | None = None

    def start_owner() -> tuple[subprocess.Popen[str], RssSampler]:
        process = subprocess.Popen(
            locald_command,
            cwd=project,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        sampler = RssSampler(process.pid)
        sampler.start()
        return process, sampler

    def measure_round(round_name: str, process: subprocess.Popen[str]) -> dict[str, Any]:
        query_rows: list[dict[str, Any]] = []
        round_snapshot: list[int] | None = None
        for item in labels["queries"]:
            result = search_query(
                cli_path,
                project,
                workspace,
                endpoint,
                item["query"],
                args.limit,
                args.timeout_seconds,
            )
            expected = item["expected"]
            observed = result["observed"]
            if round_snapshot is None:
                round_snapshot = result["snapshot"]
            elif result["snapshot"] != round_snapshot:
                raise ValueError(f"index snapshot changed between queries in {round_name}")
            query_rows.append(
                {
                    "query": item["query"],
                    "expected_count": len(expected),
                    "observed_count": len(observed),
                    "exact_membership": set(observed) == set(expected),
                    "missing": sorted(set(expected) - set(observed)),
                    "unexpected": sorted(set(observed) - set(expected)),
                    "result_order": observed,
                    "snapshot": result["snapshot"],
                    "page_sizes": result["page_sizes"],
                    "page_latency_ns": result["page_elapsed_ns"],
                    "page_latency_ms": [round(value / 1_000_000, 3) for value in result["page_elapsed_ns"]],
                    "chain_latency_ns": result["elapsed_ns"],
                    "chain_latency_ms": round(result["elapsed_ns"] / 1_000_000, 3),
                }
            )
        if process.poll() is not None:
            raise RuntimeError(f"locald exited during {round_name} with code {process.returncode}")
        return {"round": round_name, "queries": query_rows}

    process: subprocess.Popen[str] | None = None
    sampler: RssSampler | None = None
    try:
        verify_frozen_inputs("before first owner start")
        process, sampler = start_owner()
        first_owner_pid = process.pid
        startup_ns, first_health = wait_ready(
            process, cli_path, project, workspace, endpoint, args.timeout_seconds
        )
        for repetition in range(args.repetitions):
            passes["first_open"].append(measure_round(f"first-open-{repetition + 1}", process))
        workspace_bytes_after_first_open = tree_bytes(workspace)
        projection_bytes_after_first_open = tree_bytes(search_root) if search_root.is_dir() else 0
        owner_rss = sampler.stop()
        owner_rss_sample_count += len(sampler.values)
        first_exit = stop_owner(process)
        process = None
        sampler = None

        verify_frozen_inputs("before cold-reopen owner start")
        process, sampler = start_owner()
        reopen_owner_pid = process.pid
        reopen_start, reopen_health = wait_ready(
            process, cli_path, project, workspace, endpoint, args.timeout_seconds
        )
        first_reopen = measure_round("cold-reopen-1", process)
        first_reopen["owner_startup_ms"] = round(reopen_start / 1_000_000, 3)
        passes["reopen"].append(first_reopen)
        for repetition in range(max(1, args.repetitions - 1)):
            passes["reopen"].append(measure_round(f"warm-{repetition + 1}", process))
        owner_rss = max(owner_rss or 0, sampler.stop() or 0) or None
        owner_rss_sample_count += len(sampler.values)
        stop_owner(process)
        process = None
        sampler = None
    finally:
        if process is not None:
            stop_owner(process)
        if sampler is not None:
            owner_rss = max(owner_rss or 0, sampler.stop() or 0) or None
        log.close()
        shutil.rmtree(socket_dir, ignore_errors=True)

    verify_frozen_inputs("after all owner processes stopped")
    journal_after = sha256_bytes(read_frozen_file(journal, MAX_JOURNAL_BYTES, "registry journal"))
    copied_journal_after = sha256_bytes(
        read_frozen_file(
            workspace / "registry-discovery" / "catalog.journal",
            MAX_JOURNAL_BYTES,
            "private discovery journal",
        )
    )
    correctness = all(
        row["exact_membership"]
        for phase in passes.values()
        for round_row in phase
        for row in round_row["queries"]
    )
    first_pages = [
        round_row["queries"][index]["page_latency_ns"][0]
        for round_row in passes["first_open"][:1]
        for index in range(len(labels["queries"]))
    ]
    reopen_pages = [
        round_row["queries"][index]["page_latency_ns"][0]
        for round_row in passes["reopen"][:1]
        for index in range(len(labels["queries"]))
    ]
    report = {
        "schema": "nudox.registry-journal-search-benchmark.v1",
        "correct": correctness,
        "provenance": {
            "dataset_kind": dataset_kind,
            "journal_path": str(journal),
            "journal_sha256": journal_digest,
            "journal_bytes": journal_size,
            "workspace_template": str(workspace_template),
            "workspace_template_file_bytes": workspace_template_bytes,
            "project_template_file_bytes": project_template_bytes,
            "private_workspace": str(workspace),
            "private_journal_sha256_after": copied_journal_after,
            "source_journal_sha256_after": journal_after,
            "labels_path": str(labels_path),
            "labels_sha256": labels_digest,
            "independent_label_scope": labels.get("scope"),
            "locald": snapshot_executable(locald_path),
            "cli": snapshot_executable(cli_path),
            "binary_source_commit": build_manifest["source"]["commit"],
            "build_manifest": str(build_manifest_path),
            "build_manifest_sha256": manifest_digest,
            "build_toolchain": build_manifest.get("toolchain"),
            "benchmark_runner": {
                "path": str(script_path),
                "sha256": runner_digest,
            },
            "python": sys.version,
            "platform": platform.platform(),
            "machine": platform.machine(),
            "git_revision": git_revision,
            "nix_shell": os.environ.get("IN_NIX_SHELL"),
            "cargo_lock_path": str(lock_path),
            "cargo_lock_sha256": lock_digest,
            "cargo_lock_bytes": len(lock_bytes),
            "frozen_inputs_rechecked_before_and_after": True,
            "locald_argv": locald_command,
            "inherited_backend_environment": "all BACKEND_* variables removed; explicit offline flags supplied",
        },
        "configuration": {
            "non_maven_dataset_opt_in": args.allow_non_maven_dataset,
            "page_limit": args.limit,
            "repetitions": args.repetitions,
            "latency_boundary": "one backend-cli process + local socket roundtrip per page",
            "projection_path": str(workspace / "registry-discovery" / "catalog-search-v1"),
            "workspace_bytes_before_first_open": workspace_bytes_before,
            "workspace_bytes_after_first_open": workspace_bytes_after_first_open,
            "projection_present_before_first_open": projection_existed_before,
            "projection_state_before_first_open": (
                "preexisting-projection-open" if projection_existed_before else "absent-before-first-open"
            ),
            "projection_bytes_before_first_open": projection_bytes_before,
            "projection_bytes_after_first_open": projection_bytes_after_first_open,
            "locald_pid_first_open": first_owner_pid,
            "locald_pid_cold_reopen": reopen_owner_pid,
            "sampled_peak_locald_rss_bytes_across_processes": owner_rss,
            "sampled_locald_rss_samples": owner_rss_sample_count,
            "first_owner_exit": first_exit,
            "owner_startup_to_ready_ms": round(startup_ns / 1_000_000, 3),
            "health_first_open": first_health,
            "health_cold_reopen": reopen_health,
            "first_open_first_page_latency": latency_summary(first_pages),
            "cold_reopen_first_page_latency": latency_summary(reopen_pages),
        },
        "passes": passes,
    }
    args.output.mkdir(parents=True, exist_ok=True)
    report_path = args.output / "report.json"
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"correct": correctness, "report": str(report_path)}, sort_keys=True))
    return 0 if correctness and journal_after == journal_digest and copied_journal_after == journal_digest else 1


if __name__ == "__main__":
    raise SystemExit(main())
