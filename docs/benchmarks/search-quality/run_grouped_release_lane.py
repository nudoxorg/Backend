#!/usr/bin/env python3
"""Measure release ordering inside production package-lineage search groups."""

from __future__ import annotations

import argparse
import json
import os
import platform
import random
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
import search_benchmark as benchmark  # noqa: E402

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def facet_is_yanked(facet: dict[str, Any]) -> bool:
    return facet.get("status") == "known" and any(value is True for value in facet.get("values", []))


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def grouped_rankings(response: dict[str, Any], query: dict[str, Any], data: dict[str, Any]) -> list[str]:
    if response.get("query_id") != query["query_id"]:
        raise ValueError(f"group search returned the wrong query id for {query['query_id']}")
    groups = response.get("groups")
    if not isinstance(groups, list):
        raise ValueError(f"group search omitted groups for {query['query_id']}")
    scope = set(query["scope"]["ecosystems"])
    ids = []
    for group in groups:
        if not isinstance(group, dict) or group.get("ecosystem") not in scope:
            continue
        group_ids = group.get("document_ids")
        if not isinstance(group_ids, list):
            raise ValueError(f"group search returned malformed release ids for {query['query_id']}")
        for document_id in group_ids:
            document = data["documents"].get(document_id)
            if document is None or document.get("ecosystem") not in scope:
                continue
            if query.get("exclude_yanked") and facet_is_yanked(
                document.get("fields", {}).get("yanked", {})
            ):
                continue
            ids.append(document_id)
    if len(ids) != len(set(ids)):
        raise ValueError(f"group search returned duplicate release ids for {query['query_id']}")
    return ids


def read_response(stream: Any, query_id: str) -> dict[str, Any]:
    line = stream.readline()
    if not line:
        raise RuntimeError(f"adapter exited before replying to {query_id}")
    response = json.loads(line)
    if not isinstance(response, dict) or response.get("error"):
        raise RuntimeError(f"grouped production search failed for {query_id}: {response}")
    return response


def summarize(samples: list[int]) -> dict[str, int | None]:
    return benchmark.percentiles(samples)


def command_run(command: list[str], cwd: Path, input_path: Path | None = None) -> dict[str, Any]:
    completed = subprocess.run(
        command,
        cwd=cwd,
        input=input_path.read_text(encoding="utf-8") if input_path else None,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(
            f"adapter command failed ({completed.returncode}): {command}\n{completed.stderr[-4000:]}"
        )
    if not completed.stdout.strip():
        return {}
    value = json.loads(completed.stdout.splitlines()[-1])
    if not isinstance(value, dict) or value.get("error"):
        raise RuntimeError(f"adapter returned an invalid JSON response: {value}")
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--result-dir", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=101)
    parser.add_argument("--cold-samples", type=int, default=101)
    parser.add_argument("--limit", type=int, default=50)
    parser.add_argument("--seed", type=int, default=20260929)
    args = parser.parse_args()
    started_utc = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    if os.environ.get("BENCH_BUILD_SLOT_GRANTED") != "1":
        raise ValueError("set BENCH_BUILD_SLOT_GRANTED=1 after root grants a benchmark slot")
    if os.environ.get("CARGO_BUILD_JOBS") != "1":
        raise ValueError("set CARGO_BUILD_JOBS=1 for the allocated benchmark slot")
    if args.samples < 101 or args.cold_samples < 101:
        raise ValueError("warm and cold sample counts must be >= 101 for p99")
    adapter = args.adapter.resolve(strict=True)
    result_dir = args.result_dir.resolve()
    if not result_dir.is_dir() or any(result_dir.iterdir()):
        raise ValueError("result directory must already exist and be empty")

    benchmark.verify_source_freeze()
    data = benchmark.load_data()
    errors = benchmark.validate_data(data)
    if errors:
        raise ValueError("invalid frozen inputs: " + "; ".join(errors))
    queries = [
        row
        for row in data["queries"]
        if row.get("corpus") == "primary" and row.get("category") in {"freshness", "freshness_pair"}
    ]
    query_ids = {row["query_id"] for row in queries}
    pairs = [row for row in data["pairs"] if row["query_id"] in query_ids]
    if not queries or not pairs:
        raise ValueError("the frozen corpus has no grouped release-order queries")
    frozen_input_hashes = benchmark.input_hashes()
    frozen_input_fingerprint = benchmark.fingerprint(frozen_input_hashes)

    corpus_path = result_dir / "primary-corpus.jsonl"
    benchmark.write_corpus(corpus_path, data["corpora"]["primary"])
    index_path = result_dir / "production-index"
    build_started = time.perf_counter_ns()
    build_report = command_run(
        [str(adapter), "build", "--corpus", str(corpus_path), "--index", str(index_path)], ROOT
    )
    build_elapsed_ns = time.perf_counter_ns() - build_started

    requests = {
        query["query_id"]: {
            "op": "search-groups",
            "query_id": query["query_id"],
            "query": query["query"],
            "category": query["category"],
            "corpus": query["corpus"],
            "scope": query["scope"],
            "exclude_yanked": query["exclude_yanked"],
            "limit": args.limit,
            "measure_owner_time": True,
        }
        for query in queries
    }
    rankings: dict[str, list[str]] = {}
    warm_roundtrip_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    warm_owner_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    stderr_path = result_dir / "serve.stderr"
    with stderr_path.open("w", encoding="utf-8") as stderr:
        process = subprocess.Popen(
            [str(adapter), "serve", "--index", str(index_path), "--limit", str(args.limit)],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
            bufsize=1,
        )
        try:
            if process.stdin is None or process.stdout is None:
                raise RuntimeError("adapter service pipes were not created")
            for query in queries:
                query_id = query["query_id"]
                request = requests[query_id]
                process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
                process.stdin.flush()
                response = read_response(process.stdout, query_id)
                rankings[query_id] = grouped_rankings(response, query, data)
                warm_owner_ns[query_id].append(int(response.get("owner_search_nanos", 0)))
            rng = random.Random(args.seed)
            for _ in range(args.samples):
                order = list(queries)
                rng.shuffle(order)
                for query in order:
                    query_id = query["query_id"]
                    started = time.perf_counter_ns()
                    process.stdin.write(
                        json.dumps(requests[query_id], separators=(",", ":")) + "\n"
                    )
                    process.stdin.flush()
                    response = read_response(process.stdout, query_id)
                    warm_roundtrip_ns[query_id].append(time.perf_counter_ns() - started)
                    warm_owner_ns[query_id].append(int(response.get("owner_search_nanos", 0)))
                    if grouped_rankings(response, query, data) != rankings[query_id]:
                        raise RuntimeError(f"warm group ranking changed for {query_id}")
        finally:
            if process.stdin:
                process.stdin.close()
            process.wait(timeout=30)
        if process.returncode != 0:
            raise RuntimeError(f"adapter serve exited with {process.returncode}")

    cold_process_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    cold_owner_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    for query in queries:
        query_id = query["query_id"]
        request_path = result_dir / f"request-{query_id}.json"
        write_json(request_path, requests[query_id])
        for _ in range(args.cold_samples):
            started = time.perf_counter_ns()
            response = command_run(
                [
                    str(adapter),
                    "search-cold",
                    "--index",
                    str(index_path),
                    "--query-json",
                    str(request_path),
                    "--limit",
                    str(args.limit),
                ],
                ROOT,
            )
            cold_process_ns[query_id].append(time.perf_counter_ns() - started)
            cold_owner_ns[query_id].append(int(response.get("owner_search_nanos", 0)))
            if grouped_rankings(response, query, data) != rankings[query_id]:
                raise RuntimeError(f"cold/warm group ranking mismatch for {query_id}")

    per_query = {
        query["query_id"]: benchmark.score_one(
            query["query_id"],
            rankings[query["query_id"]],
            data["qrels"].get(query["query_id"], {}),
            [1, 5, 10],
        )
        for query in queries
    }
    pair_checks = []
    for pair in pairs:
        ranking = rankings[pair["query_id"]]
        newer = pair["newer_document_id"]
        older = pair["older_document_id"]
        newer_rank = ranking.index(newer) + 1 if newer in ranking else None
        older_rank = ranking.index(older) + 1 if older in ranking else None
        older_yanked = facet_is_yanked(
            data["documents"][older].get("fields", {}).get("yanked", {})
        )
        filtered_yanked = bool(
            data["query_by_id"][pair["query_id"]].get("exclude_yanked")
            and older_yanked
            and older_rank is None
        )
        pair_checks.append(
            {
                **pair,
                "newer_rank_1based": newer_rank,
                "older_rank_1based": older_rank,
                "older_filtered_as_yanked": filtered_yanked,
                "newer_precedes_older": None
                if newer_rank is None or older_rank is None
                else newer_rank < older_rank,
                "freshness_decision_correct": newer_rank is not None
                and (older_rank is None and filtered_yanked or newer_rank < older_rank),
            }
        )

    metrics = {
        "snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "source_manifest_sha256": benchmark.sha256(benchmark.SOURCE_FILE),
        "benchmark_input_fingerprint": frozen_input_fingerprint,
        "benchmark_input_sha256": frozen_input_hashes,
        "ranker": "repository production DiscoverySearchIndex.search_groups_after via Tantivy adapter",
        "adapter_path": str(adapter),
        "adapter_sha256": benchmark.sha256(adapter),
        "index": build_report,
        "index_build_elapsed_ns": build_elapsed_ns,
        "query_ids": [query["query_id"] for query in queries],
        "query_count": len(queries),
        "version_pair_qrels": pair_checks,
        "relative_version_pair_accuracy": sum(
            row["freshness_decision_correct"] for row in pair_checks
        )
        / len(pair_checks),
        "freshness_macro": {
            metric: statistics.fmean(row[metric] for row in per_query.values())
            for metric in ("mrr", "recall@1", "recall@5", "ndcg@1", "ndcg@5", "ndcg@10")
        },
        "per_query": per_query,
        "rankings": rankings,
        "warm_roundtrip_latency_ns": {
            "p50_p95_p99": summarize(
                [value for samples in warm_roundtrip_ns.values() for value in samples]
            ),
            "boundary": "persistent production adapter JSONL request/response, excluding initial index open",
        },
        "warm_owner_search_latency_ns": {
            "p50_p95_p99": summarize([value for samples in warm_owner_ns.values() for value in samples]),
            "boundary": "in-process production grouped-ranker call, recorded by the adapter",
        },
        "cold_process_open_latency_ns": {
            "p50_p95_p99": summarize([value for samples in cold_process_ns.values() for value in samples]),
            "boundary": "new adapter process, journal-to-Tantivy projection rebuild, and one grouped search; OS page cache is not flushed",
        },
        "cold_owner_search_latency_ns": {
            "p50_p95_p99": summarize([value for samples in cold_owner_ns.values() for value in samples]),
            "boundary": "in-process grouped-ranker call after each cold index rebuild",
        },
    }
    write_json(result_dir / "grouped-release-comparison.json", metrics)
    manifest = {
        "snapshot_id": metrics["snapshot_id"],
        "source_manifest_sha256": metrics["source_manifest_sha256"],
        "benchmark_input_fingerprint": metrics["benchmark_input_fingerprint"],
        "started_utc": started_utc,
        "samples_per_query": args.samples,
        "cold_samples_per_query": args.cold_samples,
        "result_limit": args.limit,
        "seed": args.seed,
        "host": {"os": platform.platform(), "machine": platform.machine(), "python": sys.version},
        "cargo_build_jobs": os.environ.get("CARGO_BUILD_JOBS"),
    }
    write_json(result_dir / "run-manifest.json", manifest)
    benchmark.verify_source_freeze()
    if benchmark.input_hashes() != frozen_input_hashes:
        raise ValueError("frozen benchmark inputs changed during the grouped release run")
    print(result_dir / "grouped-release-comparison.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
