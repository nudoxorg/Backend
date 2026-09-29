#!/usr/bin/env python3
"""Measure real live-journey CLI and direct Turso query latency."""

from __future__ import annotations

import argparse
import json
import math
import statistics
import subprocess
import time
from pathlib import Path


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    rank = max(1, math.ceil(fraction * len(ordered)))
    return ordered[rank - 1]


def run_command(argv: list[str]) -> tuple[float, str]:
    began = time.perf_counter_ns()
    completed = subprocess.run(argv, capture_output=True, text=True, check=False)
    elapsed_ms = (time.perf_counter_ns() - began) / 1_000_000
    if completed.returncode != 0:
        raise RuntimeError(
            f"query failed (exit {completed.returncode}): {argv!r}\n"
            f"{(completed.stdout + completed.stderr)[-4000:]}"
        )
    return elapsed_ms, completed.stdout


def cli_command(cli: str, workspace: str, endpoint: str, *query: str) -> list[str]:
    return [
        cli,
        "--workspace",
        workspace,
        "--endpoint",
        endpoint,
        "--json",
        *query,
    ]


def result_rows(output: str) -> int:
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"query returned non-JSON output: {output[:1000]!r}") from error
    records = payload.get("records")
    if isinstance(records, list):
        return len(records)
    return 0


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", required=True)
    parser.add_argument("--turso", required=True)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--projection", required=True)
    parser.add_argument("--serde-purl", required=True)
    parser.add_argument("--dependent-purl", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--samples", type=int, default=20)
    args = parser.parse_args()
    if not 0 <= args.warmups <= 10 or not 1 <= args.samples <= 100:
        parser.error("warmups must be 0..10 and samples must be 1..100")

    graph_forward = (
        "SELECT target_ecosystem||'|'||target_name||'|'||requirement "
        "FROM backend_projection_package_edges "
        f"WHERE source='{args.serde_purl}' ORDER BY edge_id LIMIT 64;"
    )
    graph_reverse = (
        "SELECT source||'|'||requirement FROM backend_projection_package_edges "
        "WHERE target_name='serde' ORDER BY source,edge_id LIMIT 64;"
    )
    queries: dict[str, list[str]] = {
        "tantivy_package_search": cli_command(
            args.cli, args.workspace, args.endpoint, "--limit", "20", "search", "serde"
        ),
        "registry_index_search": cli_command(
            args.cli, args.workspace, args.endpoint, "--limit", "20", "index-search", "serde"
        ),
        "forward_dependencies": cli_command(
            args.cli,
            args.workspace,
            args.endpoint,
            "dependencies",
            args.dependent_purl,
        ),
        "reverse_dependents": cli_command(
            args.cli,
            args.workspace,
            args.endpoint,
            "dependents",
            args.serde_purl,
        ),
        "direct_graph_forward": [
            args.turso,
            "-m",
            "list",
            "--experimental-multiprocess-wal",
            args.projection,
            graph_forward,
        ],
        "direct_graph_reverse": [
            args.turso,
            "-m",
            "list",
            "--experimental-multiprocess-wal",
            args.projection,
            graph_reverse,
        ],
    }

    measurements: dict[str, dict[str, object]] = {}
    for name, argv in queries.items():
        for _ in range(args.warmups):
            run_command(argv)
        latencies: list[float] = []
        rows: list[int] = []
        byte_counts: list[int] = []
        for _ in range(args.samples):
            elapsed_ms, output = run_command(argv)
            latencies.append(elapsed_ms)
            byte_counts.append(len(output.encode("utf-8")))
            rows.append(
                len(output.splitlines())
                if name.startswith("direct_graph_")
                else result_rows(output)
            )
        measurements[name] = {
            "measurement": "new CLI/Turso process per sample; process startup included",
            "warmup_calls": args.warmups,
            "sample_calls": args.samples,
            "row_count_per_sample": rows[0] if len(set(rows)) == 1 else None,
            "row_count_min": min(rows),
            "row_count_max": max(rows),
            "row_counts_per_sample": rows,
            "output_bytes_median": int(statistics.median(byte_counts)),
            "latency_ms": {
                "p50": round(statistics.median(latencies), 3),
                "p95": round(percentile(latencies, 0.95), 3),
                "min": round(min(latencies), 3),
                "max": round(max(latencies), 3),
            },
        }

    result = {
        "kind": "live_turso_cold_restore_query_benchmark",
        "backend_cli": str(Path(args.cli)),
        "turso_cli": str(Path(args.turso)),
        "projection_database": str(Path(args.projection)),
        "package_search_path": "locald package search surface (Tantivy-backed)",
        "registry_index_search_path": "locald registry/discovery index search surface",
        "latency_note": "Each sample launches a fresh process; daemon is warm after 3 warmup requests.",
        "measurements": measurements,
    }
    output_path = Path(args.output)
    output_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    output_path.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
