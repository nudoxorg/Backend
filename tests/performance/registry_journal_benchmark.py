#!/usr/bin/env python3
"""Measure production registry search against the retained Maven journal/QRELs.

The runner never builds binaries. It starts the supplied locald executable on
a private copy of a prepared workspace, drives the supplied CLI through the
real ``index-search`` command, follows every cursor, and checks every result
against labels recorded independently from the source PURLs.

The first pass includes creation of the durable search projection. A second
pass reopens the same workspace in a fresh locald process and measures the
cold durable open and warm repeated queries separately. Timings include one
CLI process and local transport round trip per page; they are end-to-end
surface timings, not claims about scorer-only latency.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
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


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def tree_bytes(path: Path) -> int:
    total = 0
    for entry in path.rglob("*"):
        if entry.is_symlink():
            raise ValueError(f"refusing symlink in measured workspace: {entry}")
        if entry.is_file():
            total = total + entry.stat().st_size
    return total


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
    try:
        result = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(pid)],
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError:
        return None
    if result.returncode or not result.stdout.strip():
        return None
    try:
        # macOS and BSD ``ps`` report RSS in KiB.
        return int(result.stdout.strip().splitlines()[-1]) * 1024
    except ValueError:
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
    return {"path": str(resolved), "sha256": sha256(resolved), "bytes": resolved.stat().st_size}


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
    environment = os.environ.copy()
    # A failed explicit owner must fail closed; the CLI may attach, but must
    # never quietly start a second daemon with a different binary.
    environment["BACKEND_LOCALD_BIN"] = str(endpoint.parent / "no-autostart-locald")
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
        operands = [record.get("operand") for record in records if isinstance(record, dict)]
        if any(not isinstance(operand, str) for operand in operands):
            raise ValueError(f"query {query!r} returned a record without an exact operand")
        observed.extend(operands)
        page_sizes.append(len(operands))
        info = response.get("index_search_page") or {}
        next_cursor = info.get("next_cursor") if isinstance(info, dict) else None
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
    environment = os.environ.copy()
    environment["BACKEND_LOCALD_BIN"] = str(endpoint.parent / "no-autostart-locald")
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
    parser.add_argument("--journal", type=Path, default=DEFAULT_JOURNAL)
    parser.add_argument("--labels", type=Path, default=DEFAULT_LABELS)
    parser.add_argument("--workspace-template", type=Path, default=DEFAULT_WORKSPACE)
    parser.add_argument("--project-template", type=Path, default=None)
    parser.add_argument("--output", type=Path, required=True)
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
    output = args.output.resolve()
    if output.exists():
        parser.error(f"output already exists; choose a fresh path: {output}")
    output.mkdir(parents=True, mode=0o700)

    labels = json.loads(labels_path.read_text(encoding="utf-8"))
    queries = labels.get("queries")
    if not isinstance(queries, list) or not queries:
        raise ValueError("independent labels must contain a nonempty query list")
    for item in queries:
        if (
            not isinstance(item, dict)
            or not isinstance(item.get("query"), str)
            or not isinstance(item.get("expected"), list)
            or any(not isinstance(value, str) for value in item["expected"])
            or len(item["expected"]) != len(set(item["expected"]))
        ):
            raise ValueError(f"malformed independent query label: {item!r}")
    journal_digest = sha256(journal)
    journal_size = journal.stat().st_size
    if labels.get("journal_sha256") != journal_digest:
        raise ValueError("independent labels were recorded against a different journal")
    if journal_size != 722_663:
        raise ValueError(f"expected the retained 722,663-byte Maven journal, found {journal_size}")
    template_journal = workspace_template / "registry-discovery" / "catalog.journal"
    if sha256(template_journal) != journal_digest:
        raise ValueError("workspace template does not contain the exact labeled Maven journal")
    work = output / "private-run"
    work.mkdir(mode=0o700)
    workspace = work / "workspace"
    project = work / "project"
    shutil.copytree(workspace_template, workspace, symlinks=True)
    shutil.copytree(project_template, project, symlinks=True)
    if sha256(workspace / "registry-discovery" / "catalog.journal") != journal_digest:
        raise ValueError("private workspace copy changed the labeled journal")

    socket_dir = Path(tempfile.mkdtemp(prefix="nx-rij-"))
    endpoint = socket_dir / "locald.sock"
    log = (output / "locald.log").open("w", encoding="utf-8")
    environment = os.environ.copy()
    locald_command = [
        str(locald_path),
        "--workspace",
        str(workspace),
        "--endpoint",
        str(endpoint),
        "--profile",
        "builtin",
        "--registry-discovery-offline",
        "--idle-timeout-ms",
        "0",
    ]
    first_owner_pid: int | None = None
    reopen_owner_pid: int | None = None
    owner_rss: int | None = None
    startup_ns = 0
    passes: dict[str, list[dict[str, Any]]] = {"initial": [], "reopen": []}
    projection_bytes_after_build: int | None = None
    workspace_bytes_before = tree_bytes(workspace)
    workspace_bytes_after_build: int | None = None
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
            query_rows.append(
                {
                    "query": item["query"],
                    "expected_count": len(expected),
                    "observed_count": len(observed),
                    "exact_membership": set(observed) == set(expected),
                    "missing": sorted(set(expected) - set(observed)),
                    "unexpected": sorted(set(observed) - set(expected)),
                    "result_order": observed,
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
        process, sampler = start_owner()
        first_owner_pid = process.pid
        startup_ns, first_health = wait_ready(
            process, cli_path, project, workspace, endpoint, args.timeout_seconds
        )
        for repetition in range(args.repetitions):
            passes["initial"].append(measure_round(f"first-open-{repetition + 1}", process))
        workspace_bytes_after_build = tree_bytes(workspace)
        search_root = workspace / "registry-discovery" / "catalog-search-v1"
        projection_bytes_after_build = tree_bytes(search_root) if search_root.is_dir() else 0
        first_exit = stop_owner(process)
        owner_rss = sampler.stop()
        process = None
        sampler = None

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

    journal_after = sha256(journal)
    copied_journal_after = sha256(workspace / "registry-discovery" / "catalog.journal")
    correctness = all(
        row["exact_membership"]
        for phase in passes.values()
        for round_row in phase
        for row in round_row["queries"]
    )
    first_pages = [
        round_row["queries"][index]["page_latency_ns"][0]
        for round_row in passes["initial"][:1]
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
            "journal_path": str(journal),
            "journal_sha256": journal_digest,
            "journal_bytes": journal_size,
            "workspace_template": str(workspace_template),
            "private_workspace": str(workspace),
            "private_journal_sha256_after": copied_journal_after,
            "source_journal_sha256_after": journal_after,
            "labels_path": str(labels_path),
            "labels_sha256": sha256(labels_path),
            "independent_label_scope": labels.get("scope"),
            "locald": snapshot_executable(locald_path),
            "cli": snapshot_executable(cli_path),
            "python": sys.version,
            "platform": platform.platform(),
            "machine": platform.machine(),
            "git_revision": subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=Path(__file__).resolve().parents[2],
                text=True,
                capture_output=True,
                check=False,
            ).stdout.strip(),
            "nix_shell": os.environ.get("IN_NIX_SHELL"),
            "cargo_lock_sha256": sha256(Path(__file__).resolve().parents[2] / "Cargo.lock"),
            "locald_argv": locald_command,
        },
        "configuration": {
            "page_limit": args.limit,
            "repetitions": args.repetitions,
            "latency_boundary": "one backend-cli process + local socket roundtrip per page",
            "projection_path": str(workspace / "registry-discovery" / "catalog-search-v1"),
            "workspace_bytes_before_first_open": workspace_bytes_before,
            "workspace_bytes_after_first_open": workspace_bytes_after_build,
            "projection_bytes_after_first_open": projection_bytes_after_build,
            "locald_pid_first_open": first_owner_pid,
            "locald_pid_cold_reopen": reopen_owner_pid,
            "sampled_peak_locald_rss_bytes_across_processes": owner_rss,
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
