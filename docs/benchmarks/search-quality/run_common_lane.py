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
    status = subprocess.run(
        ["git", "-C", str(path), "status", "--porcelain", "--untracked-files=all"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    if status:
        raise ValueError("pinned lib.rs checkout must be clean, including untracked files")
    return {"revision": revision, "files_sha256": observed, "worktree_clean": True}


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    payload = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    try:
        with temporary.open("wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        temporary.unlink(missing_ok=True)


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
    resume: bool,
) -> dict[str, Any]:
    cargo_corpus_path = output / "nudox-cargo-primary.jsonl"
    benchmark.write_corpus(cargo_corpus_path, cargo_rows)
    index_path = output / "nudox-index"
    build_checkpoint_path = output / "nudox-index-build.json"
    build_input = {
        "corpus_sha256": sha256(cargo_corpus_path),
        "adapter_sha256": sha256(adapter.resolve()),
        "document_ids": sorted(row["document_id"] for row in cargo_rows),
        "row_count": len(cargo_rows),
    }
    if index_path.exists():
        if not resume or not build_checkpoint_path.is_file():
            raise ValueError("existing Nudox index has no compatible resumable build checkpoint")
        build_checkpoint = json.loads(build_checkpoint_path.read_text(encoding="utf-8"))
        if build_checkpoint.get("input") != build_input:
            raise ValueError("existing Nudox index was built from different inputs or adapter")
        persisted_corpus = index_path / "corpus.jsonl"
        manifest_path = index_path / "adapter-manifest.json"
        if (
            not persisted_corpus.is_file()
            or sha256(persisted_corpus) != build_input["corpus_sha256"]
            or not manifest_path.is_file()
        ):
            raise ValueError("existing Nudox index is missing or differs from its frozen corpus")
        build_report = build_checkpoint["build_report"]
        build_ns = build_checkpoint["build_elapsed_ns"]
        index_reused = True
    else:
        started = time.perf_counter_ns()
        build_report = benchmark.command_run(
            [str(adapter), "build", "--corpus", str(cargo_corpus_path), "--index", str(index_path)],
            output,
        )
        build_ns = time.perf_counter_ns() - started
        if not index_path.is_dir():
            raise RuntimeError("Nudox adapter build did not create the index directory")
        write_json(
            build_checkpoint_path,
            {"input": build_input, "build_report": build_report, "build_elapsed_ns": build_ns},
        )
        index_reused = False

    requests: dict[str, dict[str, Any]] = {}
    checkpoints: dict[str, dict[str, Any]] = {}
    for query in queries:
        qid = query["query_id"]
        requests[qid] = request_for(query, limit)
        request_path = output / f"nudox-request-{qid}.json"
        write_json(request_path, requests[qid])
        checkpoint_path = output / f"nudox-query-{qid}.json"
        request_hash = sha256(request_path)
        if resume and checkpoint_path.is_file():
            checkpoint = json.loads(checkpoint_path.read_text(encoding="utf-8"))
            if (
                checkpoint.get("request_sha256") != request_hash
                or checkpoint.get("adapter_sha256") != build_input["adapter_sha256"]
                or checkpoint.get("limit") != limit
            ):
                raise ValueError(f"Nudox query checkpoint is incompatible: {qid}")
            if len(checkpoint.get("warm_latency_ns", [])) > samples or len(checkpoint.get("cold_latency_ns", [])) > cold_samples:
                raise ValueError(f"Nudox query checkpoint exceeds requested sample counts: {qid}")
        else:
            checkpoint = {
                "query_id": qid,
                "request_sha256": request_hash,
                "adapter_sha256": build_input["adapter_sha256"],
                "limit": limit,
                "ranking": None,
                "warm_latency_ns": [],
                "cold_latency_ns": [],
            }
        checkpoints[qid] = checkpoint
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
        warm_order = list(queries)
        for query in warm_order:
            qid = query["query_id"]
            process.stdin.write(json.dumps(requests[qid], separators=(",", ":")) + "\n")
            process.stdin.flush()
            rankings[qid] = service_response(process.stdout, qid)["document_ids"]
        for query in warm_order:
            qid = query["query_id"]
            checkpoint = checkpoints[qid]
            if checkpoint["ranking"] is not None and checkpoint["ranking"] != rankings[qid]:
                raise RuntimeError(f"Nudox resumed ranking differs from checkpoint for {qid}")
            full_ranking = rankings[qid]
            if len(cargo_rows) > limit:
                full_request = request_for(query, len(cargo_rows))
                process.stdin.write(json.dumps(full_request, separators=(",", ":")) + "\n")
                process.stdin.flush()
                full_ranking = service_response(process.stdout, qid)["document_ids"]
            if checkpoint.get("full_candidate_ranking") is not None and checkpoint["full_candidate_ranking"] != full_ranking:
                raise RuntimeError(f"Nudox full-candidate ranking differs from checkpoint for {qid}")
            checkpoint["ranking"] = rankings[qid]
            checkpoint["full_candidate_ranking"] = full_ranking
            warm_ns[qid] = list(checkpoint["warm_latency_ns"])
            if len(warm_ns[qid]) < samples:
                for _ in range(len(warm_ns[qid]), samples):
                    started = time.perf_counter_ns()
                    process.stdin.write(json.dumps(requests[qid], separators=(",", ":")) + "\n")
                    process.stdin.flush()
                    response = service_response(process.stdout, qid)
                    elapsed = time.perf_counter_ns() - started
                    if response["document_ids"] != rankings[qid]:
                        raise RuntimeError(f"Nudox returned nondeterministic results for {qid}")
                    warm_ns[qid].append(elapsed)
                checkpoint["warm_latency_ns"] = warm_ns[qid]
                write_json(output / f"nudox-query-{qid}.json", checkpoint)
    finally:
        if process.stdin:
            process.stdin.close()
        process.wait(timeout=30)
        service_err.close()
        if process.returncode != 0:
            raise RuntimeError(f"Nudox serve exited with {process.returncode}")

    cold_ns: dict[str, list[int]] = {query["query_id"]: [] for query in queries}
    for query in queries:
        qid = query["query_id"]
        checkpoint = checkpoints[qid]
        cold_ns[qid] = list(checkpoint["cold_latency_ns"])
        request_path = output / f"nudox-request-{qid}.json"
        for _ in range(len(cold_ns[qid]), cold_samples):
            started = time.perf_counter_ns()
            response = benchmark.command_run(
                [
                    str(adapter),
                    "search-cold",
                    "--index",
                    str(index_path),
                    "--query-json",
                    str(request_path),
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
        checkpoint["cold_latency_ns"] = cold_ns[qid]
        write_json(output / f"nudox-query-{qid}.json", checkpoint)

    return {
        "ranker": "repository production DiscoverySearchIndex via search-quality-tantivy-adapter",
        "adapter_executable": str(adapter.resolve()),
        "adapter_sha256": sha256(adapter.resolve()),
        "build_report": build_report,
        "build_elapsed_ns": build_ns,
        "index_reused_on_resume": index_reused,
        "index_input": {
            "path": str(cargo_corpus_path),
            "sha256": sha256(cargo_corpus_path),
            "document_ids": sorted(row["document_id"] for row in cargo_rows),
            "rows": len(cargo_rows),
        },
        "rankings": rankings,
        "full_candidate_rankings": {
            query["query_id"]: checkpoints[query["query_id"]]["full_candidate_ranking"]
            for query in queries
        },
        "warm_latency_ns": {qid: values for qid, values in warm_ns.items()},
        "warm_latency_summary_ns": {qid: percentiles(values) for qid, values in warm_ns.items()},
        "cold_process_open_latency_ns": {qid: values for qid, values in cold_ns.items()},
        "cold_latency_summary_ns": {qid: percentiles(values) for qid, values in cold_ns.items()},
        "warm_latency_boundary": "persistent production adapter JSONL request/response round trip; queries measured query-major",
        "warm_query_order": [query["query_id"] for query in warm_order],
        "paired_candidate_ranking_boundary": "untimed full-candidate query used only when the requested limit is below the frozen Cargo row count; paired quality then filters exact upstream IDs and applies the requested cutoff",
        "cold_latency_boundary": "new production adapter process, index open/rebuild, and one request; OS page cache is not flushed",
        "persistent_index_components": {
            "corpus_jsonl_bytes": (index_path / "corpus.jsonl").stat().st_size,
            "catalog_journal_bytes": (index_path / "catalog.journal").stat().st_size,
            "adapter_manifest_bytes": (index_path / "adapter-manifest.json").stat().st_size,
        },
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
    resume: bool,
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
    if resume:
        command.append("resume")
    started = time.perf_counter_ns()
    completed = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    index_and_warm_process_elapsed = time.perf_counter_ns() - started
    if completed.returncode:
        raise RuntimeError(f"pinned lib.rs ranker failed ({completed.returncode}):\n{completed.stderr[-6000:]}")
    raw = json.loads(output_path.read_text(encoding="utf-8"))
    raw["cargo_compile_elapsed_ns"] = build_elapsed
    raw["index_and_warm_process_elapsed_ns"] = index_and_warm_process_elapsed
    raw["warm_latency_boundary"] = "in-process pinned CrateSearchIndex.search call; excludes JSONL and process startup"
    raw["cold_process_open_latency_boundary"] = "new pinned ranker process, Tantivy index open, and one search; OS page cache is not flushed"

    cold_checkpoint_path = output / "lib-rs-cold-checkpoint.json"
    cold_checkpoint: dict[str, list[int]] = {}
    if resume and cold_checkpoint_path.is_file():
        previous_cold = json.loads(cold_checkpoint_path.read_text(encoding="utf-8"))
        if (
            previous_cold.get("limit") != limit
            or previous_cold.get("cold_samples_per_query") != cold_samples
            or previous_cold.get("upstream_revision") != UPSTREAM_REVISION
        ):
            raise ValueError("pinned lib.rs cold checkpoints do not match this run")
        cold_checkpoint = previous_cold.get("cold_latency_ns", {})
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
        cold[qid] = list(cold_checkpoint.get(qid, []))
        if len(cold[qid]) > cold_samples:
            raise ValueError(f"pinned lib.rs cold checkpoint exceeds sample count: {qid}")
        for _ in range(len(cold[qid]), cold_samples):
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
        write_json(
            cold_checkpoint_path,
            {
                "limit": limit,
                "cold_samples_per_query": cold_samples,
                "upstream_revision": UPSTREAM_REVISION,
                "cold_latency_ns": cold,
            },
        )
    raw["cold_latency_ns"] = cold
    raw["cold_latency_summary_ns"] = {qid: percentiles(values) for qid, values in cold.items()}
    raw["binary_path"] = str(binary.resolve())
    raw["binary_sha256"] = sha256(binary)
    write_json(output_path, raw)
    return raw


def common_quality(
    data: dict[str, Any],
    query_rows: list[dict[str, Any]],
    rankings: dict[str, list[str]],
    candidate_ids: set[str] | None = None,
) -> dict[str, Any]:
    metric_names = [
        "mrr",
        "recall@1",
        "recall@5",
        "recall@10",
        "precision@1",
        "precision@5",
        "precision@10",
        "false_positive_rate@1",
        "false_positive_rate@5",
        "false_positive_rate@10",
        "qrel_coverage@1",
        "qrel_coverage@5",
        "qrel_coverage@10",
        "ndcg@1",
        "ndcg@5",
        "ndcg@10",
    ]
    per_query = {}
    qrel_rows = 0
    positive_qrel_rows = 0
    for query in query_rows:
        query_id = query["query_id"]
        qrels = data["qrels"].get(query_id, {})
        ranking = rankings.get(query_id, [])
        if candidate_ids is not None:
            qrels = {document_id: grade for document_id, grade in qrels.items() if document_id in candidate_ids}
            ranking = [document_id for document_id in ranking if document_id in candidate_ids]
        qrel_rows += len(qrels)
        positive_qrel_rows += sum(grade > 0 for grade in qrels.values())
        per_query[query_id] = benchmark.score_one(query_id, ranking, qrels, [1, 5, 10])

    macro: dict[str, float | None] = {}
    macro_defined_query_count: dict[str, int] = {}
    for name in metric_names:
        values = [row[name] for row in per_query.values() if row[name] is not None]
        macro[name] = statistics.fmean(values) if values else None
        macro_defined_query_count[name] = len(values)
    return {
        "candidate_scope": "shared candidate set" if candidate_ids is not None else "full candidate set",
        "candidate_document_ids": sorted(candidate_ids) if candidate_ids is not None else None,
        "candidate_document_count": len(candidate_ids) if candidate_ids is not None else None,
        "query_count": len(query_rows),
        "qrel_row_count": qrel_rows,
        "positive_qrel_row_count": positive_qrel_rows,
        "precision_denominator": "number of returned documents in top-k; empty top-k is undefined",
        "false_positive_rate_denominator": "number of returned documents in top-k; empty top-k is undefined",
        "qrel_coverage_denominator": "number of returned documents in top-k; empty top-k is undefined",
        "undefined_metric_policy": "null per query; omitted from that metric's macro mean; counts are reported separately",
        "macro": macro,
        "macro_defined_query_count": macro_defined_query_count,
        "per_query": per_query,
    }


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
    parser.add_argument("--resume", action="store_true", help="resume only a run with a matching durable manifest")
    parser.add_argument("--online", action="store_true", help="allow Cargo to contact configured package registries")
    args = parser.parse_args()
    started_utc = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

    if os.environ.get("BENCH_BUILD_SLOT_GRANTED") != "1":
        raise ValueError("refusing to build/run benchmark without BENCH_BUILD_SLOT_GRANTED=1 after root grants a slot")
    if args.samples < 101 or args.cold_samples < 101:
        raise ValueError("warm and cold sample counts must be >= 101 for p99")
    if os.environ.get("CARGO_BUILD_JOBS") != "1":
        raise ValueError("set CARGO_BUILD_JOBS=1 for the allocated benchmark slot")
    if not args.adapter.resolve().is_file():
        raise ValueError(f"production adapter binary not found: {args.adapter}")
    if not args.result_dir.exists() or not args.result_dir.is_dir():
        raise ValueError(f"result directory must already exist: {args.result_dir}")
    if any(args.result_dir.iterdir()) and not args.resume:
        raise ValueError(f"result directory must be empty: {args.result_dir}")
    if args.resume and not (args.result_dir / "run-manifest.json").is_file():
        raise ValueError("--resume requires a prior run-manifest.json")
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
    query_categories = {query["category"] for query in query_rows}
    required_categories = {"exact_name", "prefix", "typo", "description", "keyword"}
    if (
        not cargo_rows
        or not query_rows
        or not qrel_rows
        or not required_categories.issubset(query_categories)
    ):
        raise ValueError(
            "frozen common lane must contain Cargo documents, positive QRELs, and "
            "exact-name, prefix, typo, description, and keyword queries"
        )

    measurement_queries = list(query_rows)
    random.Random(args.seed).shuffle(measurement_queries)
    ordered_queries_path = input_dir / "queries-measurement-order.jsonl"
    ordered_queries_path.write_text(
        "".join(json.dumps(query, sort_keys=True, separators=(",", ":")) + "\n" for query in measurement_queries),
        encoding="utf-8",
    )

    upstream_root = args.lib_rs_mirror.resolve(strict=True)
    upstream = require_pinned_upstream(upstream_root)
    target_dir = args.target_dir.resolve()
    target_dir.mkdir(parents=True, exist_ok=True)
    os.environ["LIB_RS_MIRROR"] = str(upstream_root)
    run_identity = {
        "snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "source_manifest_sha256": sha256(HERE / "source-snapshots.json"),
        "benchmark_source_fingerprint": frozen_source_fingerprint,
        "benchmark_input_sha256": frozen_input_hashes,
        "seed": args.seed,
        "samples_per_query": args.samples,
        "cold_samples_per_query": args.cold_samples,
        "limit": args.limit,
        "inputs_manifest": json.loads((input_dir / "manifest.json").read_text(encoding="utf-8")),
        "measurement_query_ids": [query["query_id"] for query in measurement_queries],
        "measurement_queries_sha256": sha256(ordered_queries_path),
        "upstream": upstream,
        "adapter_sha256": sha256(args.adapter.resolve()),
        "target_dir": str(target_dir),
        "cargo_mode": "--offline --locked" if not args.online else "--locked with configured network allowed",
    }
    manifest_path = args.result_dir / "run-manifest.json"
    if args.resume:
        previous_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if previous_manifest.get("run_identity") != run_identity:
            raise ValueError("existing run manifest does not match inputs, sources, adapter, or sample configuration")
    else:
        write_json(
            manifest_path,
            {
                "status": "running",
                "started_utc": started_utc,
                "run_identity": run_identity,
                "host": {"os": platform.platform(), "machine": platform.machine(), "python": sys.version},
            },
        )
    upstream_raw = run_lib_rs(
        upstream_root,
        args.result_dir,
        input_dir / "documents.jsonl",
        ordered_queries_path,
        args.samples,
        args.cold_samples,
        args.limit,
        target_dir,
        offline=not args.online,
        resume=args.resume,
    )
    nudox_raw = run_nudox(
        args.adapter.resolve(),
        args.result_dir,
        cargo_rows,
        measurement_queries,
        args.samples,
        args.cold_samples,
        args.limit,
        resume=args.resume,
    )

    nudox_rankings = nudox_raw["rankings"]
    nudox_full_rankings = nudox_raw["full_candidate_rankings"]
    lib_rs_rankings = {
        row["query_id"]: [hit["document_id"] for hit in row["ranked"] if hit.get("document_id")]
        for row in upstream_raw["results"]
    }
    upstream_candidate_ids = set(upstream_raw["indexed_document_ids"])
    nudox_shared_rankings = {
        query_id: [document_id for document_id in ranking if document_id in upstream_candidate_ids][: args.limit]
        for query_id, ranking in nudox_full_rankings.items()
    }
    metrics = {
        "snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "source_manifest_sha256": sha256(HERE / "source-snapshots.json"),
        "benchmark_source_fingerprint": frozen_source_fingerprint,
        "benchmark_input_sha256": frozen_input_hashes,
        "upstream_snapshot": upstream,
        "common_lane": {
            "full_frozen_corpus_document_count": sum(len(rows) for rows in data["corpora"].values()),
            "primary_document_count": len(data["corpora"]["primary"]),
            "adversarial_document_count": len(data["corpora"]["adversarial"]),
            "primary_document_ids": sorted(row["document_id"] for row in cargo_rows),
            "query_ids": [row["query_id"] for row in query_rows],
            "queries": [
                {
                    "query_id": query["query_id"],
                    "query": query["query"],
                    "category": query["category"],
                    "scope": query["scope"],
                    "exclude_yanked": query["exclude_yanked"],
                }
                for query in query_rows
            ],
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
            "upstream_indexed_document_ids": sorted(upstream_candidate_ids),
            "upstream_unindexed_document_ids": sorted(
                set(row["document_id"] for row in cargo_rows) - upstream_candidate_ids
            ),
            "qrel_rows": len(qrel_rows),
            "field_coverage": upstream_raw["field_coverage"],
        },
        "production_nudox": {
            "quality_full_candidate_set": common_quality(data, query_rows, nudox_rankings),
            "quality_shared_upstream_candidates": common_quality(
                data, query_rows, nudox_shared_rankings, candidate_ids=upstream_candidate_ids
            ),
            "index_bytes": nudox_raw["build_report"].get("index_bytes"),
            "persistent_directory_bytes": nudox_raw["build_report"].get("persistent_index_directory_bytes"),
            "persistent_index_components": nudox_raw["persistent_index_components"],
            "index_bytes_boundary": "logical in-memory Tantivy managed files",
            "persistent_directory_bytes_boundary": "on-disk copied corpus, source journal, and adapter manifest",
            "index_build_elapsed_ns": nudox_raw["build_elapsed_ns"],
            "warm_latency_ns": percentiles([value for values in nudox_raw["warm_latency_ns"].values() for value in values]),
            "cold_latency_ns": percentiles([value for values in nudox_raw["cold_process_open_latency_ns"].values() for value in values]),
            "warm_throughput_queries_per_second": throughput(nudox_raw["warm_latency_ns"]),
            "warm_boundary": nudox_raw["warm_latency_boundary"],
            "cold_boundary": nudox_raw["cold_latency_boundary"],
        },
        "upstream_lib_rs": {
            "quality_indexed_candidate_set": common_quality(
                data, query_rows, lib_rs_rankings, candidate_ids=upstream_candidate_ids
            ),
            "index_bytes": upstream_raw["index_bytes"],
            "persistent_tantivy_directory_bytes": upstream_raw["persistent_tantivy_directory_bytes"],
            "persistent_data_directory_bytes": upstream_raw["persistent_data_directory_bytes"],
            "index_bytes_boundary": "on-disk Tantivy tantivy18 directory",
            "index_build_elapsed_ns": upstream_raw["index_build_elapsed_ns"],
            "cargo_compile_elapsed_ns": upstream_raw["cargo_compile_elapsed_ns"],
            "warm_latency_ns": percentiles([value for row in upstream_raw["results"] for value in row["warm_latency_ns"]]),
            "cold_latency_ns": percentiles([value for values in upstream_raw["cold_latency_ns"].values() for value in values]),
            "warm_throughput_queries_per_second": throughput(
                {row["query_id"]: row["warm_latency_ns"] for row in upstream_raw["results"]}
            ),
            "warm_boundary": upstream_raw["warm_latency_boundary"],
            "cold_boundary": upstream_raw["cold_process_open_latency_boundary"],
            "version_selection": upstream_raw["version_selection"],
        },
        "paired_ranking_comparison": {
            "candidate_document_ids": sorted(upstream_candidate_ids),
            "candidate_document_count": len(upstream_candidate_ids),
            "nudox_quality": common_quality(
                data, query_rows, nudox_shared_rankings, candidate_ids=upstream_candidate_ids
            ),
            "lib_rs_quality": common_quality(
                data, query_rows, lib_rs_rankings, candidate_ids=upstream_candidate_ids
            ),
            "interpretation": "ranker comparison is restricted to exactly the IDs indexed by pinned lib.rs; the separate full-candidate Nudox score includes its version-history capability",
        },
        "comparability": [
            f"Both rankers receive the same {len(query_rows)} raw query strings, {len(qrel_rows)} independent QREL rows, and configured result limit {args.limit}; Nudox also applies the Cargo/primary scope against its Cargo-only index.",
            f"The pinned lib.rs indexer keeps {len(upstream_candidate_ids)} highest-SemVer rows, while Nudox indexes {len(cargo_rows)} frozen Cargo releases. The report includes an exact same-candidate paired score plus Nudox's full-candidate capability score.",
            "Warm boundaries differ: Nudox includes JSONL pipes and service handling; upstream measures in-process search. Cold includes fresh-process startup, index open, and one search for both; OS page cache is not flushed.",
            "Index bytes keep implementation-specific boundaries separate: Nudox logical in-memory managed files and persistent corpus/journal/manifest bytes; upstream on-disk Tantivy bytes and full harness data-directory bytes.",
            "Upstream Cargo compilation, measured Tantivy index construction, and the child process wall time covering index plus warm search are reported separately.",
        ],
        "limitations": [
            f"This common lane has {len(query_rows)} queries, {len(qrel_rows)} QREL rows, and three package origins. It cannot support general quality or speed superiority claims.",
            "Captured crates.io API descriptions and keywords are known for the frozen current serde and bincode rows; other release fields remain unknown, and README content is unavailable.",
            "The 33-query/60-QREL production lane is measured separately through the standard benchmark command; the pinned lib.rs common projection does not support forge, other ecosystems, aliases, dependencies, advisories, yanks, or release-history search.",
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
    final_manifest = {
        "status": "complete",
        "started_utc": started_utc,
        "completed_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "run_identity": run_identity,
        "host": {"os": platform.platform(), "machine": platform.machine(), "python": sys.version},
    }
    write_json(manifest_path, final_manifest)
    print(args.result_dir / "common-comparison.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
