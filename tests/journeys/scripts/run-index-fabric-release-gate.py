#!/usr/bin/env python3
"""Run the checked-in index-fabric journeys and emit one JSONL receipt per suite.

The runner deliberately invokes existing real-process journeys instead of
recreating their assertions. A passing Rust test binary is recorded as PASS;
capability gaps reported by the live matrix or listed below remain
UNSUPPORTED and prevent an overall PASS. No registry, worker, or S3 fake is
created by this runner.

Run from any directory with:

    python3 tests/journeys/scripts/run-index-fabric-release-gate.py

The default is hermetic (`cargo --locked --offline`). Network-backed native
registry journeys are described in the report but remain an explicit opt-in.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
LOG_DIR = ROOT / "target" / "index-fabric-release-gate"


def cargo_test(package: str, target: str, test_filter: str | None = None) -> list[str]:
    command = ["cargo", "test", "--locked", "--offline", "-p", package]
    if target == "lib":
        command.append("--lib")
    elif target.startswith("test:"):
        command.extend(["--test", target.removeprefix("test:")])
    else:
        raise ValueError(f"unknown Cargo target selector: {target}")
    if test_filter:
        command.extend([test_filter, "--", "--nocapture"])
    else:
        command.append("--")
        command.append("--nocapture")
    return command


SUITES: list[dict[str, Any]] = [
    {
        "id": "nine-language-live-surface-matrix",
        "command": cargo_test("backend-journeys", "test:surface_matrix"),
        "evidence": "real locald, CLI, MCP, desktop runtime, nine independent language fixtures, SIGKILL/cold restart",
    },
    {
        "id": "package-index-advisory-and-input-tentpole",
        "command": cargo_test("backend-journeys", "test:index_tentpole"),
        "evidence": "real registry archive packages, exact/lineage search, changed dependencies, ignored JS tree, dynamic compiler read, advisory stale after restart",
    },
    {
        "id": "gui-shell-and-persisted-restart",
        "command": cargo_test("backend-journeys", "test:restart_persistence"),
        "evidence": "GUI harness, real locald process, persisted project shelf, live subscription, graceful and SIGKILL recovery",
    },
    {
        "id": "remote-worker-selection-and-stored-ack-recovery",
        "command": cargo_test("backend-journeys", "test:cluster_source_journey"),
        "evidence": "real owner and worker processes, persisted identities/trust, compiler result, Turso selection, crash/replay/Stored ACK",
    },
    {
        "id": "s3-selected-cold-restart-and-range-hydration",
        "command": cargo_test("backend-journeys", "test:s3_cold_selected_journey"),
        "evidence": "real locald process, conditional S3 pack PUT, exact Turso selection, remote segment GC, cold restart, CLI queries, and selected IR range GET",
    },
    {
        "id": "registry-unknown-yank-and-cold-reopen",
        "command": cargo_test("backend-journeys", "test:registry_discovery"),
        "evidence": "loopback native feed protocols, typed unknown coverage, yanked standing, process restart and offline historical answers",
    },
    {
        "id": "forge-process-and-offline-reference",
        "command": cargo_test("backend-journeys", "test:new_user_forge"),
        "evidence": "real locald/CLI/MCP against a loopback Git HTTP service and cold offline reference reads",
    },
    {
        "id": "desktop-keyboard-and-shell-input",
        "command": cargo_test("backend-desktop", "lib", "shell::tests"),
        "evidence": "mounted GPUI shell input, keyboard focus and click actions in the desktop test harness",
    },
    {
        "id": "versioned-plane-authority-and-embedding",
        "command": cargo_test("backend-local-service", "lib", "versioned_planes"),
        "evidence": "selected catalog/image authority, cold storage reads, exact embedding plane identity and bounded ranges",
    },
    {
        "id": "semantic-client-range-hydration",
        "command": cargo_test("backend-client", "lib", "semantic_range_local"),
        "evidence": "canonical catalog/manifest paging, selected-head movement, and bounded client hydration transport",
    },
    {
        "id": "replication-cold-resume-and-corruption",
        "command": cargo_test("backend-replication", "lib", "ir_hydration"),
        "evidence": "durable sparse CAS admission, cold resume, stale selection and corrupted range rejection",
    },
    {
        "id": "file-store-streaming-and-bounded-reads",
        "command": cargo_test("backend-store", "test:artifact_stream"),
        "evidence": "local FileStore artifact closure streaming and cold reopen",
    },
    {
        "id": "file-store-bounded-object-reads",
        "command": cargo_test("backend-store", "test:bounded_object_read"),
        "evidence": "bounded local object reads and byte accounting",
    },
    {
        "id": "s3-compatible-pack-protocol",
        "command": cargo_test("backend-store-s3", "lib"),
        "evidence": "production S3 client and immutable pack protocol against the crate's loopback HTTP protocol fixture",
    },
    {
        "id": "s3-selection-ordering-unit-contract",
        "command": cargo_test("backend-local-service", "lib", "s3_publication"),
        "evidence": "owner S3 configuration, receipt, and selection-ordering unit contracts",
    },
]


STATIC_UNSUPPORTED = [
    {
        "id": "same-closure-local-versus-s3-process-comparison",
        "reason": "The checked-in journeys do not yet compare one canonical logical closure, selected root, client-visible planes, and query answers through both FileStore-only and S3-backed locald processes. S3 pack and owner selection suites are reported separately below.",
    },
    {
        "id": "s3-client-mid-transfer-cold-resume-and-complete-embedding",
        "reason": "The production CLI semantic-hydrate path has cold-daemon coverage, and the S3 journey hydrates selected IR after restart. No journey kills the client after partial S3 range admission and resumes its durable checkpoint in a fresh client process; the stock locald fixture also has no compiler embedding runtime, so complete embedding bytes are not exercised through cold S3 hydration.",
    },
    {
        "id": "live-native-registry-network-corpus",
        "reason": "The live_registry journeys are ignored opt-in tests requiring NUDOX_LIVE_REGISTRY=1, network access, and external toolchains. This hermetic runner does not claim that network lane passed.",
    },
]


def json_line(value: dict[str, Any]) -> None:
    print(json.dumps(value, sort_keys=True, separators=(",", ":")), flush=True)


def unsupported_from_output(text: str) -> list[dict[str, str]]:
    unsupported: list[dict[str, str]] = []
    for line in text.splitlines():
        if line.startswith("index-fabric-evidence "):
            payload = line.removeprefix("index-fabric-evidence ")
            try:
                item = json.loads(payload)
            except json.JSONDecodeError:
                continue
            if item.get("state") in {"UNSUPPORTED", "PARTIAL"}:
                unsupported.append(
                    {
                        "source": "surface_matrix",
                        "field": str(item.get("field", "unknown")),
                        "language": str(item.get("language", "all")),
                        "reason": str(item.get("reason", "some fixture lanes lack this field")),
                    }
                )
        elif "state=unavailable" in line:
            unsupported.append(
                {
                    "source": "surface_matrix capability rollup",
                    "field": "capability",
                    "language": "all",
                    "reason": line.strip(),
                }
            )
        elif "surface-matrix result=PASS_WITH_UNAVAILABLE" in line:
            unsupported.append(
                {
                    "source": "surface_matrix aggregate",
                    "field": "capability",
                    "language": "all",
                    "reason": line.strip(),
                }
            )
    # De-duplicate the aggregate result line and per-lane explanations while
    # preserving the first exact observation of each capability field.
    unique: dict[tuple[str, str, str, str], dict[str, str]] = {}
    for item in unsupported:
        key = (item["source"], item["field"], item["language"], item["reason"])
        unique[key] = item
    return list(unique.values())


def evidence_from_output(text: str) -> list[dict[str, Any]]:
    evidence: list[dict[str, Any]] = []
    for line in text.splitlines():
        if not line.startswith("index-fabric-evidence "):
            continue
        try:
            item = json.loads(line.removeprefix("index-fabric-evidence "))
        except json.JSONDecodeError:
            continue
        if isinstance(item, dict):
            evidence.append(item)
    return evidence


def tail(text: str, lines: int = 30) -> str:
    return "\n".join(text.splitlines()[-lines:])


def run_suite(
    suite: dict[str, Any],
    timeout_seconds: int,
    verbose: bool,
) -> dict[str, Any]:
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    try:
        completed = subprocess.run(
            suite["command"],
            cwd=ROOT,
            env=os.environ.copy(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            timeout=timeout_seconds,
            check=False,
        )
        stdout = completed.stdout
        stderr = completed.stderr
        exit_code = completed.returncode
        timed_out = False
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else (error.stdout or "")
        stderr = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else (error.stderr or "")
        exit_code = None
        timed_out = True

    duration_ms = round((time.monotonic() - started) * 1000)
    combined = stdout + "\n" + stderr
    log_prefix = LOG_DIR / suite["id"]
    log_prefix.with_suffix(".stdout.log").write_text(stdout, encoding="utf-8")
    log_prefix.with_suffix(".stderr.log").write_text(stderr, encoding="utf-8")
    unsupported = unsupported_from_output(combined)
    evidence = evidence_from_output(combined)
    if timed_out or exit_code != 0:
        status = "FAIL"
    elif unsupported:
        status = "UNSUPPORTED"
    else:
        status = "PASS"
    result = {
        "record": "suite",
        "id": suite["id"],
        "status": status,
        "command": suite["command"],
        "exit_code": exit_code,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "stdout_bytes": len(stdout.encode("utf-8")),
        "stderr_bytes": len(stderr.encode("utf-8")),
        "log_prefix": str(log_prefix.relative_to(ROOT)),
        "evidence": suite["evidence"],
        "unsupported": unsupported,
        "oracle_fields": [item for item in evidence if item.get("kind") == "oracle"],
        "mutation_checks": [item for item in evidence if item.get("kind") == "mutation"],
    }
    if status == "FAIL":
        result["diagnostic_tail"] = {
            "stdout": tail(stdout),
            "stderr": tail(stderr),
        }
    if verbose:
        print(f"\n===== {suite['id']} ({status}) =====", file=sys.stderr)
        if stdout:
            print(stdout, end="" if stdout.endswith("\n") else "\n", file=sys.stderr)
        if stderr:
            print(stderr, end="" if stderr.endswith("\n") else "\n", file=sys.stderr)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=1800,
        help="per-suite timeout (default: 1800 seconds)",
    )
    parser.add_argument(
        "--allow-unsupported",
        action="store_true",
        help="return success when suites pass but required end-to-end capabilities remain unsupported",
    )
    parser.add_argument("--verbose", action="store_true", help="copy suite output to stderr")
    args = parser.parse_args()
    if args.timeout_seconds < 1:
        parser.error("--timeout-seconds must be positive")

    json_line(
        {
            "record": "release_gate",
            "schema": "backend.index-fabric-gate.v1",
            "root": str(ROOT),
            "mode": "offline-hermetic",
            "suite_count": len(SUITES),
        }
    )
    results: list[dict[str, Any]] = []
    for suite in SUITES:
        result = run_suite(suite, args.timeout_seconds, args.verbose)
        results.append(result)
        json_line(result)

    failures = [item["id"] for item in results if item["status"] == "FAIL"]
    unsupported = [item["id"] for item in results if item["status"] == "UNSUPPORTED"]
    if STATIC_UNSUPPORTED:
        unsupported.extend(item["id"] for item in STATIC_UNSUPPORTED)
    if failures:
        overall = "FAIL"
    elif unsupported:
        overall = "UNSUPPORTED"
    else:
        overall = "PASS"
    summary = {
        "record": "summary",
        "status": overall,
        "pass": [item["id"] for item in results if item["status"] == "PASS"],
        "unsupported": unsupported,
        "fail": failures,
        "known_gaps": STATIC_UNSUPPORTED,
        "exit_policy": "FAIL=1, UNSUPPORTED=2 unless --allow-unsupported, PASS=0",
    }
    json_line(summary)
    if failures:
        return 1
    if unsupported and not args.allow_unsupported:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
