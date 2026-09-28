#!/usr/bin/env python3
"""Run the real Nudox and pinned lib.rs rankers on their shared Cargo lane.

This invokes a prebuilt Nudox adapter and builds the upstream ranker. It is
deliberately guarded by BENCH_BUILD_SLOT_GRANTED because running this script
consumes a Cargo build slot and index benchmark time.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import random
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
import lib_rs_adapter as common_export
import search_benchmark as benchmark

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
UPSTREAM_REVISION = "4642a01664e14f4ae30a3804a55556b0770119d9"
UPSTREAM_FILES = {
    "search_index/src/lib_search_index.rs": "24705598c93b49933013125e69e7cfdb39e7b4f47d4cdaa68ae4c47367f6e301",
    "ranking/src/lib_ranking.rs": "45e2b9fd0de65db22e6c3067f57be6e1c788c482571124992a162bd732fafcda",
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def percentile(values: list[int], pct: int) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(pct * len(ordered) / 100) - 1)]


def percentiles(values: list[int]) -> dict[str, int | None]:
    return {f"p{pct}": percentile(values, pct) for pct in (50, 95, 99)}


def require_pinned_upstream(path: Path) -> dict[str, Any]:
    result = subprocess.run(
        ["git", "-C", str(path), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    )
    revision = result.stdout.strip()
    if revision != UPSTREAM_REVISION:
        raise ValueError(f"lib.rs checkout is {revision}; expected {UPSTREAM_REVISION}")
    observed = {}
    for relative, expected in UPSTREAM_FILES.items():
        source = path / relative
        actual = sha256(source)
        if actual != expected:
            raise ValueError(f"pinned lib.rs file hash mismatch: {relative}: {actual}")
        observed[relative] = actual
    return {"revision": revision, "files_sha256": observed}


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def request_for(query: dict[str, Any], limit: int) -> dict[str, Any]:
    return {
        "op": "search",
        "query_id": query["query_id"],
        "query": query["query"],
        "category": query["category"],
        "corpus": query["corpus"],
        "scope": query["scope"],
        "exclude_yanked": query["exclude_yanked"],
        "limit": limit,
    }


def service_response(stream: Any, query_id: str) -> dict[str, Any]:
    value = benchmark.read_service_line(stream)
    ids = benchmark.validate_adapter_response(value, query_id)
    value["document_ids"] = ids
    return value


def run_nudox(
    adapter: Path,
    output: Path,
    cargo_rows: list[dict[str, Any]],
    queries: list[dict[str, Any]],
    samples: int,
    cold_samples: int,
    limit: int,
    seed: int,
) -> dict[str, Any]:
    cargo_corpus_path = output / "nudox-cargo-primary.jsonl"
    benchmark.write_corpus(cargo_corpus_path, cargo_rows)
    index_path = output / "nudox-index"
    started = time.perf_counter_ns()
    build_report = benchmark.command_run(
        [str(adapter), "build", "--corpus", str(cargo_corpus_path), "--index", str(index_path)],
        output,
    )
    build_ns = time.perf_counter_ns() - started
    if not index_path.is_dir():
        raise RuntimeError("Nudox adapter build did not create the index directory")

    requests: dict[str, dict[str, Any]] = {}
    for query in queries:
        requests[query["query_id"]] = request_for(query, limit)
    rankings: dict[str, list[str]] = {}
    warm_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    service_err = (output / "nudox-serve.stderr").open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(adapter), "serve", "--index", str(index_path), "--limit", str(limit)],
        cwd=ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=service_err,
        text=True,
        bufsize=1,
    )
    try:
        if process.stdin is None or process.stdout is None:
            raise RuntimeError("Nudox serve pipes were not created")
        for query in queries:
            qid = query["query_id"]
            process.stdin.write(json.dumps(requests[qid], separators=(",", ":")) + "\n")
            process.stdin.flush()
            rankings[qid] = service_response(process.stdout, qid)["document_ids"]
        rng = random.Random(seed)
        for sample in range(samples):
            order = list(queries)
            rng.shuffle(order)
            for query in order:
                qid = query["query_id"]
                started = time.perf_counter_ns()
                process.stdin.write(json.dumps(requests[qid], separators=(",", ":")) + "\n")
                process.stdin.flush()
                response = service_response(process.stdout, qid)
                elapsed = time.perf_counter_ns() - started
                if response["document_ids"] != rankings[qid]:
                    raise RuntimeError(f"Nudox returned nondeterministic results for {qid}")
                warm_ns[qid].append(elapsed)
    finally:
        if process.stdin:
            process.stdin.close()
        process.wait(timeout=30)
        service_err.close()
        if process.returncode != 0:
            raise RuntimeError(f"Nudox serve exited with {process.returncode}")

    request_paths: dict[str, Path] = {}
    for query in queries:
        qid = query["query_id"]
        request_path = output / f"nudox-request-{qid}.json"
        write_json(request_path, requests[qid])
        request_paths[qid] = request_path
    cold_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    for query in queries:
        qid = query["query_id"]
        for _ in range(cold_samples):
            started = time.perf_counter_ns()
            response = benchmark.command_run(
                [
                    str(adapter),
                    "search-cold",
                    "--index",
                    str(index_path),
                    "--query-json",
                    str(request_paths[qid]),
                    "--limit",
                    str(limit),
                ],
                output,
            )
            elapsed = time.perf_counter_ns() - started
            ids = benchmark.validate_adapter_response(response, qid)
            if ids != rankings[qid]:
                raise RuntimeError(f"Nudox cold/warm result mismatch for {qid}")
            cold_ns[qid].append(elapsed)

    return {
        "ranker": "repository production DiscoverySearchIndex via search-quality-tantivy-adapter",
        "adapter_executable": str(adapter.resolve()),
        "adapter_sha256": sha256(adapter.resolve()),
        "build_report": build_report,
        "build_elapsed_ns": build_ns,
        "index_input": {
            "path": str(cargo_corpus_path),
            "sha256": sha256(cargo_corpus_path),
            "document_ids": sorted(row["document_id"] for row in cargo_rows),
            "rows": len(cargo_rows),
        },
        "rankings": rankings,
        "warm_latency_ns": {qid: values for qid, values in warm_ns.items()},
        "warm_latency_summary_ns": {qid: percentiles(values) for qid, values in warm_ns.items()},
        "cold_process_open_latency_ns": {qid: values for qid, values in cold_ns.items()},
        "cold_latency_summary_ns": {qid: percentiles(values) for qid, values in cold_ns.items()},
        "warm_latency_boundary": "persistent production adapter JSONL request/response round trip",
        "cold_latency_boundary": "new production adapter process, index open/rebuild, and one request; OS page cache is not flushed",
    }


def run_lib_rs(
    upstream_root: Path,
    output: Path,
    documents_path: Path,
    queries_path: Path,
    samples: int,
    cold_samples: int,
    limit: int,
    cargo_target_dir: Path,
    offline: bool,
) -> dict[str, Any]:
    project_source = HERE / "lib-rs-common"
    project = output / "lib-rs-project"
    if project.exists():
        shutil.rmtree(project)
    shutil.copytree(project_source, project, ignore=shutil.ignore_patterns("target", "__pycache__"))
    (project / "upstream").symlink_to(upstream_root, target_is_directory=True)
    index_dir = output / "lib-rs-index"
    output_path = output / "lib-rs-results.json"
    binary = cargo_target_dir / "release" / "lib-rs-common-bench"
    build_args = [
        "cargo",
        "build",
        "--release",
        "--locked",
        "--manifest-path",
        str(project / "Cargo.toml"),
        "--target-dir",
        str(cargo_target_dir),
    ]
    if offline:
        build_args.insert(3, "--offline")
    build_started = time.perf_counter_ns()
    completed = subprocess.run(build_args, cwd=ROOT, text=True, capture_output=True, check=False)
    build_elapsed = time.perf_counter_ns() - build_started
    if completed.returncode:
        raise RuntimeError(
            f"pinned lib.rs build failed ({completed.returncode}):\n{completed.stderr[-6000:]}"
        )
    if not binary.is_file():
        raise RuntimeError(f"expected upstream benchmark binary was not built: {binary}")

    command = [
        str(binary),
        str(documents_path),
        str(queries_path),
        str(index_dir),
        str(output_path),
        str(limit),
        str(samples),
    ]
    started = time.perf_counter_ns()
    completed = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    index_elapsed = time.perf_counter_ns() - started
    if completed.returncode:
        raise RuntimeError(f"pinned lib.rs ranker failed ({completed.returncode}):\n{completed.stderr[-6000:]}")
    raw = json.loads(output_path.read_text(encoding="utf-8"))
    raw["build_elapsed_ns"] = build_elapsed
    raw["index_and_warm_query_elapsed_ns"] = index_elapsed
    raw["warm_latency_boundary"] = "in-process pinned CrateSearchIndex.search call; excludes JSONL and process startup"
    raw["cold_process_open_latency_boundary"] = "new pinned ranker process, Tantivy index open, and one search; OS page cache is not flushed"

    cold: dict[str, list[int]] = {}
    query_rows = [
        json.loads(line)
        for line in queries_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    for query in query_rows:
        if not query.get("lib_rs_common"):
            continue
        qid = query["query_id"]
        cold[qid] = []
        for _ in range(cold_samples):
            started = time.perf_counter_ns()
            result = subprocess.run(
                [str(binary), "search-once", str(index_dir), query["query"], str(limit)],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            elapsed = time.perf_counter_ns() - started
            if result.returncode:
                raise RuntimeError(f"pinned lib.rs cold search failed: {result.stderr[-4000:]}")
            actual = json.loads(result.stdout)["ranked_document_ids"]
            expected = next(row for row in raw["results"] if row["query_id"] == qid)
            expected_ids = [hit["document_id"] for hit in expected["ranked"] if hit.get("document_id")]
            if actual != expected_ids:
                raise RuntimeError(f"pinned lib.rs cold/warm result mismatch for {qid}")
            cold[qid].append(elapsed)
    raw["cold_latency_ns"] = cold
    raw["cold_latency_summary_ns"] = {qid: percentiles(values) for qid, values in cold.items()}
    raw["binary_path"] = str(binary.resolve())
    raw["binary_sha256"] = sha256(binary)
    return raw


def common_quality(
    data: dict[str, Any],
    query_rows: list[dict[str, Any]],
    rankings: dict[str, list[str]],
) -> dict[str, Any]:
    per_query = {
        query["query_id"]: benchmark.score_one(
            query["query_id"], rankings.get(query["query_id"], []), data["qrels"].get(query["query_id"], {}), [1, 5, 10]
        )
        for query in query_rows
    }
    metric_names = ["mrr", "recall@1", "recall@5", "recall@10", "ndcg@1", "ndcg@5", "ndcg@10"]
    macro = {name: statistics.fmean(row[name] for row in per_query.values()) for name in metric_names}
    return {"macro": macro, "per_query": per_query}


def throughput(samples_by_query: dict[str, list[int]]) -> float | None:
    flat = [sample for values in samples_by_query.values() for sample in values]
    elapsed = sum(flat)
    return len(flat) * 1_000_000_000 / elapsed if elapsed else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter", type=Path, required=True, help="prebuilt production benchmark adapter")
    parser.add_argument("--lib-rs-mirror", type=Path, required=True, help="local checkout of the pinned upstream mirror")
    parser.add_argument("--target-dir", type=Path, required=True, help="isolated Cargo target directory allocated by root")
    parser.add_argument("--result-dir", type=Path, required=True, help="new ignored directory for raw evidence")
    parser.add_argument("--samples", type=int, default=1001)
    parser.add_argument("--cold-samples", type=int, default=101)
    parser.add_argument("--limit", type=int, default=150)
    parser.add_argument("--seed", type=int, default=20260928)
    parser.add_argument("--online", action="store_true", help="allow Cargo to contact configured package registries")
    args = parser.parse_args()
    started_utc = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

    if os.environ.get("BENCH_BUILD_SLOT_GRANTED") != "1":
        raise ValueError("refusing to build/run benchmark without BENCH_BUILD_SLOT_GRANTED=1 after root grants a slot")
    if args.samples < 101 or args.cold_samples < 101:
        raise ValueError("warm and cold sample counts must be >= 101 for p99")
    if os.environ.get("CARGO_BUILD_JOBS") != "2":
        raise ValueError("set CARGO_BUILD_JOBS=2 for the allocated benchmark slot")
    if not args.adapter.resolve().is_file():
        raise ValueError(f"production adapter binary not found: {args.adapter}")
    if not args.result_dir.exists() or not args.result_dir.is_dir():
        raise ValueError(f"result directory must already exist: {args.result_dir}")
    if any(args.result_dir.iterdir()):
        raise ValueError(f"result directory must be empty: {args.result_dir}")
    try:
        relative = args.result_dir.resolve().relative_to(ROOT)
    except ValueError:
        relative = None
    if relative is not None:
        if subprocess.run(["git", "check-ignore", "--quiet", "--", relative.as_posix()], cwd=ROOT).returncode:
            raise ValueError("result directory inside the checkout must be git-ignored")

    benchmark.verify_source_freeze()
    data = benchmark.load_data()
    errors = benchmark.validate_data(data)
    if errors:
        raise ValueError("invalid frozen benchmark inputs: " + "; ".join(errors))
    input_payloads = common_export.build_export(data)
    frozen_input_hashes = benchmark.input_hashes()
    frozen_source_fingerprint = benchmark.fingerprint(frozen_input_hashes)
    input_dir = args.result_dir / "common-input"
    common_export.write_or_verify(input_dir, input_payloads)
    cargo_rows = [row for row in data["corpora"]["primary"] if row.get("ecosystem") == "cargo"]
    query_rows = [row for row in data["queries"] if row.get("lib_rs_common") is True]
    qrel_rows = [
        row
        for row in data["qrel_rows"]
        if row["query_id"] in {query["query_id"] for query in query_rows}
        and row["document_id"] in {doc["document_id"] for doc in cargo_rows}
    ]
    if len(cargo_rows) != 7 or len(query_rows) != 2 or len(qrel_rows) != 4:
        raise ValueError(f"frozen common lane changed: {len(cargo_rows)} docs, {len(query_rows)} queries, {len(qrel_rows)} QRELs")

    upstream_root = args.lib_rs_mirror.resolve(strict=True)
    upstream = require_pinned_upstream(upstream_root)
    target_dir = args.target_dir.resolve()
    target_dir.mkdir(parents=True, exist_ok=True)
    os.environ["LIB_RS_MIRROR"] = str(upstream_root)
    upstream_raw = run_lib_rs(
        upstream_root,
        args.result_dir,
        input_dir / "documents.jsonl",
        input_dir / "queries.jsonl",
        args.samples,
        args.cold_samples,
        args.limit,
        target_dir,
        offline=not args.online,
    )
    nudox_raw = run_nudox(
        args.adapter.resolve(),
        args.result_dir,
        cargo_rows,
        query_rows,
        args.samples,
        args.cold_samples,
        args.limit,
        args.seed,
    )

    nudox_rankings = nudox_raw["rankings"]
    lib_rs_rankings = {
        row["query_id"]: [hit["document_id"] for hit in row["ranked"] if hit.get("document_id")]
        for row in upstream_raw["results"]
    }
    metrics = {
        "snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "source_manifest_sha256": sha256(HERE / "source-snapshots.json"),
        "benchmark_source_fingerprint": frozen_source_fingerprint,
        "benchmark_input_sha256": frozen_input_hashes,
        "upstream_snapshot": upstream,
        "common_lane": {
            "primary_document_ids": sorted(row["document_id"] for row in cargo_rows),
            "query_ids": [row["query_id"] for row in query_rows],
            "qrels": [
                {
                    "query_id": row["query_id"],
                    "document_id": row["document_id"],
                    "grade": int(row["grade"]),
                    "judgment_basis": row["judgment_basis"],
                    "source_snapshot_id": row["source_snapshot_id"],
                }
                for row in qrel_rows
            ],
            "primary_release_rows": len(cargo_rows),
            "upstream_indexed_documents": upstream_raw["indexed_documents"],
            "qrel_rows": len(qrel_rows),
            "field_coverage": upstream_raw["field_coverage"],
        },
        "production_nudox": {
            "quality": common_quality(data, query_rows, nudox_rankings),
            "index_bytes": nudox_raw["build_report"].get("index_bytes"),
            "persistent_directory_bytes": nudox_raw["build_report"].get("persistent_index_directory_bytes"),
            "warm_latency_ns": percentiles([value for values in nudox_raw["warm_latency_ns"].values() for value in values]),
            "cold_latency_ns": percentiles([value for values in nudox_raw["cold_process_open_latency_ns"].values() for value in values]),
            "warm_throughput_queries_per_second": throughput(nudox_raw["warm_latency_ns"]),
            "warm_boundary": nudox_raw["warm_latency_boundary"],
            "cold_boundary": nudox_raw["cold_latency_boundary"],
        },
        "upstream_lib_rs": {
            "quality": common_quality(data, query_rows, lib_rs_rankings),
            "index_bytes": upstream_raw["index_bytes"],
            "warm_latency_ns": percentiles([value for row in upstream_raw["results"] for value in row["warm_latency_ns"]]),
            "cold_latency_ns": percentiles([value for values in upstream_raw["cold_latency_ns"].values() for value in values]),
            "warm_throughput_queries_per_second": throughput(
                {row["query_id"]: row["warm_latency_ns"] for row in upstream_raw["results"]}
            ),
            "warm_boundary": upstream_raw["warm_latency_boundary"],
            "cold_boundary": upstream_raw["cold_process_open_latency_boundary"],
            "version_selection": upstream_raw["version_selection"],
        },
        "comparability": [
            "Both rankers use the same seven Cargo primary document IDs, two exact-name query IDs, four independently hand-authored QREL rows, and a result limit of 150.",
            "The pinned lib.rs indexer keeps one highest-SemVer document per Crates.io origin; Nudox keeps all seven frozen release rows. This changes the candidate set for historical serde QRELs and is reported as a version-history capability difference.",
            "Warm latency boundaries differ: Nudox includes JSONL pipes and service request handling; lib.rs measures only the in-process search call. Cold for both includes a fresh process, opening the index, and one search, but neither flushes the OS page cache.",
            "Tantivy bytes are reported from each implementation's own index directory/storage boundary; these values are not normalized storage comparisons.",
        ],
        "limitations": [
            "This common lane has only two exact-name queries, four QREL judgments, and three package origins. It cannot support general quality or speed superiority claims.",
            "All seven Cargo rows have unknown keywords, descriptions, and README content. The richer-field lane is unavailable, so the shared-field run uses package name only.",
            "The larger 25-query/39-QREL production lane is measured separately through the standard benchmark command; the pinned lib.rs common projection does not support forge, other ecosystems, aliases, dependencies, advisories, yanks, or release-history search.",
            "The lib.rs add API requires numeric monthly downloads. The harness passes 0 because the query-relevance sort path ignores that field; its status is explicitly unknown and the value is not used as ranking evidence.",
        ],
    }
    write_json(args.result_dir / "nudox-common-raw.json", nudox_raw)
    write_json(args.result_dir / "lib-rs-common-raw.json", upstream_raw)
    benchmark.verify_source_freeze()
    if benchmark.input_hashes() != frozen_input_hashes:
        raise ValueError("frozen benchmark inputs changed during the comparison")
    if require_pinned_upstream(upstream_root) != upstream:
        raise ValueError("pinned lib.rs source changed during the comparison")
    write_json(args.result_dir / "common-comparison.json", metrics)
    run_manifest = {
        "snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "source_manifest_sha256": metrics["source_manifest_sha256"],
        "benchmark_source_fingerprint": frozen_source_fingerprint,
        "benchmark_input_sha256": frozen_input_hashes,
        "started_utc": started_utc,
        "seed": args.seed,
        "samples_per_query": args.samples,
        "cold_samples_per_query": args.cold_samples,
        "limit": args.limit,
        "inputs": json.loads((input_dir / "manifest.json").read_text(encoding="utf-8")),
        "upstream": upstream,
        "adapter_sha256": sha256(args.adapter.resolve()),
        "target_dir": str(target_dir),
        "host": {"os": platform.platform(), "machine": platform.machine(), "python": sys.version},
        "cargo_mode": "--offline --locked" if not args.online else "--locked with configured network allowed",
    }
    write_json(args.result_dir / "run-manifest.json", run_manifest)
    print(args.result_dir / "common-comparison.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
