#!/usr/bin/env python3
"""Measure the clearly labeled synthetic scale lane without QREL scoring."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def workspace_build_lock_snapshot() -> dict[str, Any]:
    path = ROOT / "Cargo.lock"
    return {
        "path": "Cargo.lock",
        "sha256": sha256(path),
        "bytes": path.stat().st_size,
        "scope": "execution context only; excluded from frozen corpus and QREL evidence",
    }


def executable_snapshot(adapter: list[str]) -> dict[str, str | None]:
    token = adapter[0]
    candidate = Path(token)
    if not candidate.is_absolute():
        located = shutil.which(token)
        candidate = Path(located) if located else ROOT / candidate
    try:
        resolved = candidate.resolve(strict=True)
    except OSError:
        return {"path": token, "sha256": None}
    if not resolved.is_file():
        return {"path": str(resolved), "sha256": None}
    return {"path": str(resolved), "sha256": sha256(resolved)}


def verify_frozen_sources() -> None:
    for command in (
        [sys.executable, str(HERE / "freeze_corpus.py"), "--check"],
        [sys.executable, str(HERE / "search_benchmark.py"), "validate"],
    ):
        completed = subprocess.run(
            command,
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        if completed.returncode:
            detail = (completed.stderr or completed.stdout).strip()
            raise ValueError(f"frozen benchmark source validation failed: {detail}")


def source_evidence_hashes(adapter: list[str]) -> dict[str, str]:
    paths = {
        HERE / "primary-corpus.jsonl",
        HERE / "source-snapshots.json",
        HERE / "generate_scale_corpus.py",
        HERE / "scale_benchmark.py",
        HERE / "search_benchmark.py",
    }
    source_manifest = json.loads((HERE / "source-snapshots.json").read_text(encoding="utf-8"))
    for source in source_manifest.get("source_snapshots", []):
        for field in ("path", "local_file"):
            relative = source.get(field)
            if isinstance(relative, str):
                paths.add(ROOT / relative)
    executable = executable_snapshot(adapter)
    if executable.get("sha256") is not None:
        paths.add(Path(executable["path"]))
    return {str(path): sha256(path) for path in sorted(paths)}


def percentiles(samples: list[int]) -> dict[str, int]:
    if not samples:
        raise ValueError("percentiles require at least one sample")
    ordered = sorted(samples)

    def nearest_rank(percent: int) -> int:
        index = max(0, (percent * len(ordered) + 99) // 100 - 1)
        return ordered[index]

    return {
        "samples": len(ordered),
        "p50_ns": nearest_rank(50),
        "p95_ns": nearest_rank(95),
        "p99_ns": nearest_rank(99),
    }


def rss_bytes(pid: int) -> int | None:
    try:
        completed = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(pid)],
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError:
        return None
    if completed.returncode != 0 or not completed.stdout.strip():
        return None
    try:
        # BSD/macOS and procps report RSS in KiB for this `ps` format.
        return int(completed.stdout.strip().splitlines()[-1]) * 1024
    except ValueError:
        return None


def sample_rss(pid: int, stop: threading.Event, samples: list[int], interval_seconds: float = 0.05) -> None:
    while not stop.is_set():
        value = rss_bytes(pid)
        if value is not None:
            samples.append(value)
        stop.wait(interval_seconds)


def run_measured(command: list[str], cwd: Path) -> tuple[dict[str, Any], int, int | None]:
    started = time.perf_counter_ns()
    process = subprocess.Popen(command, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    rss_samples: list[int] = []
    stop = threading.Event()
    sampler = threading.Thread(target=sample_rss, args=(process.pid, stop, rss_samples), daemon=True)
    sampler.start()
    stdout, stderr = process.communicate()
    stop.set()
    sampler.join()
    elapsed = time.perf_counter_ns() - started
    if process.returncode:
        raise RuntimeError(f"command failed ({process.returncode}): {shlex.join(command)}\n{stderr[-4000:]}")
    try:
        response = json.loads(stdout.splitlines()[-1]) if stdout.strip() else {}
    except (IndexError, json.JSONDecodeError) as error:
        raise ValueError(f"command did not end with a JSON object: {shlex.join(command)}") from error
    if not isinstance(response, dict):
        raise ValueError(f"command returned non-object JSON: {shlex.join(command)}")
    peak = max(rss_samples, default=rss_bytes(process.pid))
    return response, elapsed, peak


def load_scale_case(path: Path) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, str]]:
    manifest_path = path / "manifest.json"
    corpus_path = path / "corpus.jsonl"
    queries_path = path / "queries.jsonl"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("lane") != "synthetic-scale-only":
        raise ValueError(f"not a synthetic scale lane: {manifest_path}")
    source_manifest = json.loads((HERE / "source-snapshots.json").read_text(encoding="utf-8"))
    if manifest.get("benchmark_snapshot_id") != source_manifest.get("benchmark_snapshot_id"):
        raise ValueError("scale input was generated from a different benchmark source snapshot")
    if manifest.get("source_schema_sha256") != sha256(HERE / "primary-corpus.jsonl"):
        raise ValueError("scale input schema hash does not match the frozen primary corpus")
    for filename, target in (("corpus.jsonl", corpus_path), ("queries.jsonl", queries_path)):
        expected = manifest.get("files", {}).get(filename, {}).get("sha256")
        if expected != sha256(target):
            raise ValueError(f"scale input hash does not match its manifest: {target}")
    queries = [json.loads(line) for line in queries_path.read_text(encoding="utf-8").splitlines() if line]
    targets = manifest.get("probe_targets")
    if not isinstance(targets, dict) or not targets:
        raise ValueError("scale manifest has no probe targets")
    return manifest, queries, targets


def read_response(stdout: Any) -> dict[str, Any]:
    line = stdout.readline()
    if not line:
        raise RuntimeError("adapter exited before answering a scale request")
    value = json.loads(line)
    if not isinstance(value, dict) or "error" in value:
        raise RuntimeError(f"adapter request failed: {value}")
    return value


def validate_hit(response: dict[str, Any], expected: str, request_id: str | None = None) -> None:
    ids = response.get("document_ids")
    if not isinstance(ids, list) or expected not in ids:
        raise ValueError(f"scale probe did not return its deterministic target {expected}: {ids}")
    if request_id is not None and response.get("request_id") != request_id:
        raise ValueError(f"parallel response id mismatch: expected {request_id}, got {response.get('request_id')}")


def query_request(query: dict[str, Any], request_id: str) -> dict[str, Any]:
    return {**query, "op": "search", "request_id": request_id}


def run_update_lane(
    adapter: list[str],
    index_path: Path,
    manifest: dict[str, Any],
    samples: int,
    limit: int,
    cwd: Path,
) -> tuple[list[int], int | None]:
    target_id = manifest["probe_targets"]["cargo.update-target"]
    name = target_id.split("/", 1)[1].rsplit("@", 1)[0]
    ordinal = int(name.rsplit("-", 1)[1])
    alias = f"synthetic-alias-cargo-{ordinal:07d}"
    query = {
        "op": "search",
        "query_id": "scale.update-alias",
        "query": alias,
        "category": "alias",
        "corpus": "primary",
        "scope": {"ecosystems": ["cargo"]},
        "exclude_yanked": False,
        "limit": limit,
    }
    process = subprocess.Popen(
        adapter + ["serve", "--index", str(index_path), "--limit", str(limit)],
        cwd=cwd,
        text=True,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=1,
    )
    if process.stdin is None or process.stdout is None:
        process.kill()
        raise RuntimeError("failed to open adapter update protocol pipes")
    rss_samples: list[int] = []
    stop = threading.Event()
    sampler = threading.Thread(target=sample_rss, args=(process.pid, stop, rss_samples), daemon=True)
    sampler.start()
    latencies: list[int] = []
    try:
        process.stdin.write(json.dumps(query, separators=(",", ":")) + "\n")
        process.stdin.flush()
        validate_hit(read_response(process.stdout), target_id)
        for sample in range(samples):
            remove_alias = sample % 2 == 0
            update = {
                "op": "update",
                "updates": [
                    {
                        "document_id": target_id,
                        "fields": {
                            "aliases": {
                                "status": "absent" if remove_alias else "known",
                                "values": [] if remove_alias else [alias],
                                "source_snapshot_id": manifest["synthetic_source_snapshot_id"],
                            }
                        },
                    }
                ],
            }
            started = time.perf_counter_ns()
            process.stdin.write(json.dumps(update, separators=(",", ":")) + "\n")
            process.stdin.flush()
            response = read_response(process.stdout)
            latencies.append(time.perf_counter_ns() - started)
            if response.get("updated") != 1:
                raise ValueError(f"incremental scale update was not applied: {response}")
            process.stdin.write(json.dumps(query, separators=(",", ":")) + "\n")
            process.stdin.flush()
            search_response = read_response(process.stdout)
            present = target_id in search_response.get("document_ids", [])
            if present == remove_alias:
                raise ValueError(f"scale alias update failed its post-update check at sample {sample}")
        if samples % 2:
            restore = {
                "op": "update",
                "updates": [
                    {
                        "document_id": target_id,
                        "fields": {
                            "aliases": {
                                "status": "known",
                                "values": [alias],
                                "source_snapshot_id": manifest["synthetic_source_snapshot_id"],
                            }
                        },
                    }
                ],
            }
            process.stdin.write(json.dumps(restore, separators=(",", ":")) + "\n")
            process.stdin.flush()
            if read_response(process.stdout).get("updated") != 1:
                raise ValueError("scale update lane could not restore its probe document")
    finally:
        process.stdin.close()
        process.wait(timeout=30)
        stop.set()
        sampler.join()
    if process.returncode:
        stderr = process.stderr.read() if process.stderr else ""
        raise RuntimeError(f"adapter update service failed: {stderr[-4000:]}")
    return latencies, max(rss_samples, default=None)


def run_parallel_lane(
    adapter: list[str],
    index_path: Path,
    queries: list[dict[str, Any]],
    targets: dict[str, str],
    workers: int,
    samples: int,
    limit: int,
    cwd: Path,
) -> tuple[list[int], float, int | None]:
    process = subprocess.Popen(
        adapter + ["serve-parallel", "--index", str(index_path), "--limit", str(limit), "--workers", str(workers)],
        cwd=cwd,
        text=True,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=1,
    )
    if process.stdin is None or process.stdout is None:
        process.kill()
        raise RuntimeError("failed to open adapter parallel search pipes")
    rss_samples: list[int] = []
    stop = threading.Event()
    sampler = threading.Thread(target=sample_rss, args=(process.pid, stop, rss_samples), daemon=True)
    sampler.start()
    exact_queries = [query for query in queries if query["category"] == "exact-name"]
    if not exact_queries:
        process.kill()
        raise ValueError("scale lane has no exact-name probe queries")
    latencies: list[int] = []
    warmup_query = exact_queries[0]
    warmup_id = f"r{workers}-warmup"
    process.stdin.write(
        json.dumps(query_request(warmup_query, warmup_id), separators=(",", ":")) + "\n"
    )
    process.stdin.flush()
    validate_hit(read_response(process.stdout), targets[warmup_query["query_id"]], warmup_id)
    started_all = time.perf_counter_ns()
    request_number = 0
    try:
        for sample in range(samples):
            pending: dict[str, tuple[int, dict[str, Any]]] = {}
            for worker in range(workers):
                query = exact_queries[(sample * workers + worker) % len(exact_queries)]
                request_id = f"r{workers}-s{sample}-w{worker}"
                request = query_request(query, request_id)
                pending[request_id] = (time.perf_counter_ns(), query)
                process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
                request_number += 1
            process.stdin.flush()
            for _ in range(workers):
                response = read_response(process.stdout)
                request_id = response.get("request_id")
                if request_id not in pending:
                    raise ValueError(f"parallel adapter returned an unknown request id: {request_id}")
                request_started, query = pending.pop(request_id)
                validate_hit(response, targets[query["query_id"]], request_id)
                latencies.append(time.perf_counter_ns() - request_started)
    finally:
        process.stdin.close()
        process.wait(timeout=30)
        stop.set()
        sampler.join()
    if process.returncode:
        stderr = process.stderr.read() if process.stderr else ""
        raise RuntimeError(f"adapter parallel service failed: {stderr[-4000:]}")
    elapsed = (time.perf_counter_ns() - started_all) / 1_000_000_000
    throughput = request_number / elapsed if elapsed > 0 else 0.0
    return latencies, throughput, max(rss_samples, default=None)


def run_case(
    case_dir: Path,
    result_dir: Path,
    adapter: list[str],
    run_name: str,
    samples: int,
    seed: int,
    parallelism: list[int],
    limit: int,
) -> dict[str, Any]:
    manifest, queries, targets = load_scale_case(case_dir)
    if manifest.get("seed") != seed:
        raise ValueError(f"scale seed {manifest.get('seed')} does not match requested seed {seed}")
    evidence = result_dir / run_name / case_dir.name
    if evidence.exists():
        raise FileExistsError(f"refusing to overwrite scale evidence: {evidence}")
    evidence.mkdir(parents=True)
    work_dir = evidence / "work"
    work_dir.mkdir()
    hashes_before = {
        name: sha256(case_dir / name) for name in ("manifest.json", "corpus.jsonl", "queries.jsonl")
    }
    implementation_hashes_before = source_evidence_hashes(adapter)
    workspace_lock_before = workspace_build_lock_snapshot()
    index_path = work_dir / "index"
    build_report, build_ns, build_peak_rss = run_measured(
        adapter + ["build", "--corpus", str(case_dir / "corpus.jsonl"), "--index", str(index_path)],
        ROOT,
    )
    if not index_path.is_dir():
        raise RuntimeError("adapter build did not create the scale index")
    persistent_bytes = sum(path.stat().st_size for path in index_path.rglob("*") if path.is_file())

    cold_query = next(query for query in queries if query["query_id"] == "cargo.middle-alias")
    cold_target = targets[cold_query["query_id"]]
    cold_request_path = work_dir / "cold-query.json"
    cold_request_path.write_text(json.dumps(query_request(cold_query, "cold")), encoding="utf-8")
    cold_samples: list[int] = []
    cold_peak_rss_values: list[int] = []
    for _ in range(samples):
        response, elapsed_ns, peak = run_measured(
            adapter + [
                "search-cold",
                "--index",
                str(index_path),
                "--query-json",
                str(cold_request_path),
                "--limit",
                str(limit),
            ],
            ROOT,
        )
        validate_hit(response, cold_target, "cold")
        cold_samples.append(elapsed_ns)
        if peak is not None:
            cold_peak_rss_values.append(peak)

    update_samples, update_peak_rss = run_update_lane(
        adapter,
        index_path,
        manifest,
        samples,
        limit,
        evidence,
    )
    reader_metrics = {}
    all_warm_rows = []
    for workers in parallelism:
        latencies, throughput, peak_rss = run_parallel_lane(
            adapter,
            index_path,
            queries,
            targets,
            workers,
            samples,
            limit,
            evidence,
        )
        all_warm_rows.extend(
            {"parallelism": workers, "sample": index, "elapsed_ns": elapsed}
            for index, elapsed in enumerate(latencies)
        )
        reader_metrics[str(workers)] = {
            "reader_count": workers,
            "request_samples": percentiles(latencies),
            "requests_per_second": throughput,
            "sampled_peak_rss_bytes": peak_rss,
        }

    raw_path = evidence / "raw-latencies.jsonl"
    with raw_path.open("w", encoding="utf-8") as stream:
        for mode, values in (
            ("cold_process_open", cold_samples),
            ("incremental_one_field_update", update_samples),
        ):
            for sample, elapsed in enumerate(values):
                stream.write(json.dumps({"mode": mode, "sample": sample, "elapsed_ns": elapsed}, sort_keys=True) + "\n")
        for row in all_warm_rows:
            stream.write(json.dumps({"mode": "warm_parallel_search", **row}, sort_keys=True) + "\n")

    hashes_after = {
        name: sha256(case_dir / name) for name in ("manifest.json", "corpus.jsonl", "queries.jsonl")
    }
    implementation_hashes_after = source_evidence_hashes(adapter)
    workspace_lock_after = workspace_build_lock_snapshot()
    metrics = {
        "lane": "synthetic-scale-only",
        "quality_qrel_scored": False,
        "record_count": manifest["document_count"],
        "ecosystem_counts": manifest["ecosystem_counts"],
        "seed": seed,
        "samples": samples,
        "index_bytes": build_report.get("index_bytes"),
        "persistent_index_directory_bytes": persistent_bytes,
        "adapter_build_report": build_report,
        "adapter_argv": adapter,
        "adapter_executable_snapshot": executable_snapshot(adapter),
        "benchmark_source_manifest_sha256": sha256(HERE / "source-snapshots.json"),
        "index_build_latency_ns": build_ns,
        "build_process_sampled_peak_rss_bytes": build_peak_rss,
        "cold_process_open_latency_ns": percentiles(cold_samples),
        "cold_process_sampled_peak_rss_bytes": max(cold_peak_rss_values, default=None),
        "incremental_one_document_one_field_update_latency_ns": percentiles(update_samples),
        "update_service_sampled_peak_rss_bytes": update_peak_rss,
        "warm_readers": reader_metrics,
        "sampling_limits": [
            "The OS page cache is not flushed; cold process-open includes recovery and production-index reconstruction from the local source journal.",
            "Warm round-trip latency includes JSONL pipes and reader-pool queueing.",
            "RSS is sampled with `ps -o rss=` every 50 ms and may miss shorter peaks.",
            "All generated records, aliases, keywords, and descriptions are synthetic perturbations of the frozen package-row schema; there are no archive URLs, hashes, downloads, or relevance judgments in this lane.",
        ],
        "source_input_sha256_before": hashes_before,
        "source_input_sha256_after": hashes_after,
        "implementation_source_sha256_before": implementation_hashes_before,
        "implementation_source_sha256_after": implementation_hashes_after,
        "source_tree_stable": hashes_before == hashes_after and implementation_hashes_before == implementation_hashes_after,
        "workspace_build_lock_context": {
            "before": workspace_lock_before,
            "after": workspace_lock_after,
            "stable_during_run": workspace_lock_before["sha256"] == workspace_lock_after["sha256"],
        },
        "host": {
            "os": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor(),
            "logical_cpus": os.cpu_count(),
        },
    }
    (evidence / "metrics.json").write_text(json.dumps(metrics, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if hashes_before != hashes_after or implementation_hashes_before != implementation_hashes_after:
        raise RuntimeError("synthetic scale inputs or implementation sources changed during measurement")
    return {"evidence": str(evidence), "metrics": metrics}


def parse_parallelism(value: str) -> list[int]:
    try:
        values = sorted({int(part) for part in value.split(",")})
    except ValueError as error:
        raise argparse.ArgumentTypeError("parallelism must be comma-separated positive integers") from error
    if not values or values[0] <= 0 or values[-1] > 64:
        raise argparse.ArgumentTypeError("parallelism values must be between 1 and 64")
    return values


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    run = subparsers.add_parser("run", help="measure scale cases without QREL scoring")
    run.add_argument("--corpus-dir", type=Path, required=True)
    run.add_argument("--adapter-command", required=True)
    run.add_argument("--result-dir", type=Path, required=True)
    run.add_argument("--run-name", default="synthetic-scale-v1")
    run.add_argument("--samples", type=int, default=101)
    run.add_argument("--seed", type=int, default=20260928)
    run.add_argument("--parallelism", type=parse_parallelism, default=parse_parallelism("1,4,16"))
    run.add_argument("--limit", type=int, default=50)
    args = parser.parse_args()

    try:
        if os.environ.get("BENCH_BUILD_SLOT_GRANTED") != "1":
            raise ValueError("refusing to invoke an adapter until the root grants a build slot")
        verify_frozen_sources()
        if args.samples < 101:
            raise ValueError("--samples must be at least 101 for nearest-rank p99")
        if not 1 <= args.limit <= 1000:
            raise ValueError("--limit must be between 1 and 1000")
        if not args.result_dir.is_dir():
            raise ValueError(f"result directory must already exist: {args.result_dir}")
        adapter = shlex.split(args.adapter_command)
        if not adapter:
            raise ValueError("--adapter-command is empty")
        args.result_dir = args.result_dir.resolve()
        try:
            relative_result = args.result_dir.relative_to(ROOT)
            inside_repository = True
        except ValueError:
            inside_repository = False
            relative_result = Path()
        if inside_repository:
            ignored = subprocess.run(
                ["git", "check-ignore", "--quiet", "--", str(relative_result)],
                cwd=ROOT,
                check=False,
            )
            if ignored.returncode != 0:
                raise ValueError("result directory inside the repository must already be git-ignored")
        results = []
        for case_dir in sorted(args.corpus_dir.glob("records-*"), key=lambda path: int(path.name.removeprefix("records-"))):
            results.append(
                run_case(
                    case_dir.resolve(),
                    args.result_dir,
                    adapter,
                    args.run_name,
                    args.samples,
                    args.seed,
                    args.parallelism,
                    args.limit,
                )
            )
        if not results:
            raise ValueError(f"no records-* scale corpora found in {args.corpus_dir}")
        print(json.dumps(results, indent=2, sort_keys=True))
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
