#!/usr/bin/env python3
"""Bounded, real-REST Qdrant capacity workload for the pinned local service."""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import statistics
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Iterable, Sequence


class Phase(str, Enum):
    CLEANUP = "cleanup"
    CREATE = "create"
    PAYLOAD_INDEX = "payload_index"
    UPSERT = "upsert"
    OPTIMIZE = "optimize"
    QUERY = "query"
    DELETE = "delete"
    REINSERT = "reinsert"
    READBACK = "readback"
    COLLECTION_INFO = "collection_info"
    TELEMETRY = "telemetry"
    RESTART_QUERY = "restart_query"


class FaultKind(str, Enum):
    HTTP = "http"
    TRANSPORT = "transport"
    TIMEOUT = "timeout"
    DECODE = "decode"
    QDRANT_STATUS = "qdrant_status"
    SHAPE = "shape"
    IDENTITY = "identity"


class QueryKind(str, Enum):
    APPROXIMATE = "approximate"
    EXACT = "exact"


class FilterKind(str, Enum):
    UNFILTERED = "unfiltered"
    TYPED_PAYLOAD = "typed_payload"


class StorageKind(str, Enum):
    IN_MEMORY = "in_memory"
    ON_DISK = "on_disk"


class QuantizationKind(str, Enum):
    NONE = "none"
    SCALAR_INT8 = "scalar_int8"


@dataclass(frozen=True)
class Scenario:
    label: str
    points: int
    dimension: int
    storage: StorageKind
    payload_on_disk: bool
    quantization: QuantizationKind
    hnsw_m: int
    ef_construct: int
    hnsw_ef: int


@dataclass(frozen=True)
class WorkloadConfig:
    endpoint: str
    output: Path
    storage_path: Path | None
    qdrant_pid: int | None
    batch_points: int
    query_samples: int
    multicore_concurrency: int
    timeout_seconds: float
    optimizer_timeout_seconds: float
    seed: int


@dataclass(frozen=True)
class HttpFault(Exception):
    phase: Phase
    category: FaultKind
    detail: str
    status: int | None = None

    def __str__(self) -> str:
        return f"{self.phase.value}:{self.category.value}:{self.detail}"


@dataclass(frozen=True)
class Timed:
    wall_ns: int
    json_encode_ns: int
    request_bytes: int
    response_bytes: int


@dataclass(frozen=True)
class ProcessFact:
    availability: str
    rss_bytes: int | None
    cpu_seconds: float | None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", default="http://127.0.0.1:26333")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--storage-path", type=Path)
    parser.add_argument("--qdrant-pid", type=int)
    parser.add_argument("--batch-points", type=int, default=128)
    parser.add_argument("--query-samples", type=int, default=48)
    parser.add_argument("--multicore-concurrency", type=int, default=4)
    parser.add_argument("--timeout-seconds", type=float, default=30.0)
    parser.add_argument("--optimizer-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--seed", type=int, default=0x4E55444F58)
    parser.add_argument(
        "--scenario-set",
        choices=("baseline-10k", "largest-100k", "lever-10k", "all"),
        default="baseline-10k",
    )
    parser.add_argument(
        "--restart-scenario",
        choices=("baseline-384d-10k", "baseline-768d-10k", "baseline-1536d-10k", "baseline-768d-100k", "on-disk-768d-10k", "scalar-int8-768d-10k", "hnsw-m32-768d-10k", "hnsw-efconstruct200-768d-10k", "hnsw-ef128-768d-10k"),
    )
    return parser.parse_args()


def endpoint_path(endpoint: str, path: str) -> str:
    return endpoint.rstrip("/") + path


def request_json(
    config: WorkloadConfig,
    phase: Phase,
    method: str,
    path: str,
    body: dict[str, Any] | None = None,
    require_qdrant_status: bool = True,
) -> tuple[dict[str, Any], Timed]:
    encode_started = time.perf_counter_ns()
    payload = b"" if body is None else json.dumps(body, separators=(",", ":")).encode()
    json_encode_ns = time.perf_counter_ns() - encode_started
    request = urllib.request.Request(
        endpoint_path(config.endpoint, path),
        data=payload if method != "GET" else None,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    started = time.perf_counter_ns()
    try:
        with urllib.request.urlopen(request, timeout=config.timeout_seconds) as response:
            raw = response.read()
    except urllib.error.HTTPError as error:
        detail = error.read(8_192).decode("utf-8", "replace")
        raise HttpFault(phase, FaultKind.HTTP, f"body={detail}", error.code) from error
    except urllib.error.URLError as error:
        raise HttpFault(phase, FaultKind.TRANSPORT, str(error.reason)) from error
    except TimeoutError as error:
        raise HttpFault(phase, FaultKind.TIMEOUT, f"seconds={config.timeout_seconds}") from error
    elapsed = time.perf_counter_ns() - started
    try:
        decoded = json.loads(raw)
    except json.JSONDecodeError as error:
        raise HttpFault(phase, FaultKind.DECODE, raw[:8_192].decode("utf-8", "replace")) from error
    if require_qdrant_status and decoded.get("status") != "ok":
        raise HttpFault(phase, FaultKind.QDRANT_STATUS, json.dumps(decoded, separators=(",", ":")))
    return decoded, Timed(elapsed, json_encode_ns, len(payload), len(raw))


def vector_value(point_id: int, coordinate: int, seed: int) -> float:
    state = (seed + point_id * 0x9E3779B185EBCA87 + coordinate * 0xC2B2AE3D27D4EB4F) & ((1 << 64) - 1)
    state ^= state >> 30
    state = (state * 0xBF58476D1CE4E5B9) & ((1 << 64) - 1)
    state ^= state >> 27
    state = (state * 0x94D049BB133111EB) & ((1 << 64) - 1)
    state ^= state >> 31
    candidate = ((state & 0x00FF_FFFF) / 8_388_607.5) - 1.0
    return struct.unpack(">f", struct.pack(">f", candidate))[0]


def vector(point_id: int, scenario: Scenario, seed: int) -> list[float]:
    return [vector_value(point_id, coordinate, seed) for coordinate in range(scenario.dimension)]


def payload(point_id: int) -> dict[str, Any]:
    return {
        "authority": "nudox-capacity-authority-v1",
        "snapshot": "capacity-snapshot-20260831",
        "model": "capacity-seeded-f32",
        "metric": "Cosine",
        "partition": point_id % 16,
    }


def points(ids: Iterable[int], scenario: Scenario, seed: int) -> Iterable[dict[str, Any]]:
    for point_id in ids:
        yield {"id": point_id, "vector": vector(point_id, scenario, seed), "payload": payload(point_id)}


def collection_name(scenario: Scenario) -> str:
    return (
        f"nudoxcap{scenario.dimension}d{scenario.points}n{scenario.storage.value[:2]}"
        f"{scenario.quantization.value[:2]}m{scenario.hnsw_m}e{scenario.ef_construct}"
    )


def process_fact(pid: int | None) -> ProcessFact:
    if pid is None:
        return ProcessFact("unavailable:no_pid", None, None)
    completed = subprocess.run(
        ["ps", "-o", "rss=", "-o", "time=", "-p", str(pid)],
        check=False,
        capture_output=True,
        text=True,
    )
    fields = completed.stdout.split()
    if completed.returncode != 0 or len(fields) != 2:
        return ProcessFact("unavailable:ps", None, None)
    try:
        rss_bytes = int(fields[0]) * 1024
        clock = [float(part) for part in fields[1].split(":")]
        if len(clock) == 2:
            cpu_seconds = clock[0] * 60 + clock[1]
        elif len(clock) == 3:
            cpu_seconds = clock[0] * 3600 + clock[1] * 60 + clock[2]
        else:
            return ProcessFact("unavailable:ps_clock", None, None)
    except ValueError:
        return ProcessFact("unavailable:ps_parse", None, None)
    return ProcessFact("measured", rss_bytes, cpu_seconds)


def directory_bytes(path: Path | None) -> int | None:
    if path is None or not path.is_dir():
        return None
    total = 0
    for entry in path.rglob("*"):
        if entry.is_file():
            total += entry.stat().st_size
    return total


def collection_directory_bytes(storage_path: Path | None, name: str) -> int | None:
    if storage_path is None:
        return None
    return directory_bytes(storage_path / "collections" / name)


def process_delta(before: ProcessFact, after: ProcessFact) -> dict[str, Any]:
    if before.cpu_seconds is None or after.cpu_seconds is None:
        cpu_seconds: float | None = None
    else:
        cpu_seconds = max(0.0, after.cpu_seconds - before.cpu_seconds)
    return {
        "cpu_seconds": cpu_seconds,
        "steady_rss_bytes": after.rss_bytes,
        "availability": after.availability,
    }


def collection_body(scenario: Scenario) -> dict[str, Any]:
    body: dict[str, Any] = {
        "vectors": {"size": scenario.dimension, "distance": "Cosine", "on_disk": scenario.storage is StorageKind.ON_DISK},
        "on_disk_payload": scenario.payload_on_disk,
        "hnsw_config": {"m": scenario.hnsw_m, "ef_construct": scenario.ef_construct, "on_disk": scenario.storage is StorageKind.ON_DISK},
        "optimizers_config": {"indexing_threshold": 1},
        "shard_number": 1,
    }
    if scenario.quantization is QuantizationKind.SCALAR_INT8:
        body["quantization_config"] = {"scalar": {"type": "int8", "quantile": 0.99, "always_ram": scenario.storage is StorageKind.IN_MEMORY}}
    return body


def create_collection(config: WorkloadConfig, scenario: Scenario) -> dict[str, Any]:
    name = collection_name(scenario)
    try:
        request_json(config, Phase.CLEANUP, "DELETE", f"/collections/{name}", None)
    except HttpFault as error:
        if error.category is not FaultKind.HTTP or error.status != 404:
            raise
    _, timed = request_json(config, Phase.CREATE, "PUT", f"/collections/{name}?wait=true", collection_body(scenario))
    index_timings: list[dict[str, Any]] = []
    for field, schema in (("authority", "keyword"), ("snapshot", "keyword"), ("model", "keyword"), ("metric", "keyword"), ("partition", "integer")):
        _, index_timed = request_json(
            config,
            Phase.PAYLOAD_INDEX,
            "PUT",
            f"/collections/{name}/index?wait=true",
            {"field_name": field, "field_schema": schema},
        )
        index_timings.append({"field": field, **asdict(index_timed)})
    return {"collection": name, "create": asdict(timed), "payload_indexes": index_timings}


def upsert(config: WorkloadConfig, scenario: Scenario, name: str) -> dict[str, Any]:
    request_bytes = 0
    response_bytes = 0
    vector_generate_ns = 0
    json_encode_ns = 0
    rest_request_ns = 0
    end_to_end_started = time.perf_counter_ns()
    batches = 0
    for first in range(0, scenario.points, config.batch_points):
        generated_started = time.perf_counter_ns()
        batch = {"points": list(points(range(first, min(first + config.batch_points, scenario.points)), scenario, config.seed))}
        vector_generate_ns += time.perf_counter_ns() - generated_started
        _, timed = request_json(config, Phase.UPSERT, "PUT", f"/collections/{name}/points?wait=true", batch)
        request_bytes += timed.request_bytes
        response_bytes += timed.response_bytes
        json_encode_ns += timed.json_encode_ns
        rest_request_ns += timed.wall_ns
        batches += 1
    end_to_end_ns = time.perf_counter_ns() - end_to_end_started
    return {
        "points": scenario.points,
        "batches": batches,
        "end_to_end_wall_ns": end_to_end_ns,
        "vector_generate_wall_ns": vector_generate_ns,
        "json_encode_wall_ns": json_encode_ns,
        "rest_request_wall_ns": rest_request_ns,
        "request_bytes": request_bytes,
        "response_bytes": response_bytes,
        "end_to_end_points_per_second": scenario.points * 1_000_000_000 / end_to_end_ns,
        "rest_request_points_per_second": scenario.points * 1_000_000_000 / rest_request_ns,
    }


def wait_optimizer(config: WorkloadConfig, scenario: Scenario, name: str) -> dict[str, Any]:
    started = time.perf_counter_ns()
    deadline = time.monotonic() + config.optimizer_timeout_seconds
    polls = 0
    while True:
        optimizations, _ = request_json(config, Phase.OPTIMIZE, "GET", f"/collections/{name}/optimizations?with=queued,completed")
        info, _ = request_json(config, Phase.COLLECTION_INFO, "GET", f"/collections/{name}")
        summary = optimizations["result"]["summary"]
        polls += 1
        indexed_vectors = info["result"].get("indexed_vectors_count")
        if (
            summary["queued_optimizations"] == 0
            and not optimizations["result"]["running"]
            and info["result"]["status"] == "green"
            and isinstance(indexed_vectors, int)
            and indexed_vectors >= scenario.points
        ):
            return {"wall_ns": time.perf_counter_ns() - started, "polls": polls, "optimizations": optimizations["result"], "collection": info["result"]}
        if time.monotonic() >= deadline:
            raise HttpFault(Phase.OPTIMIZE, FaultKind.TIMEOUT, json.dumps({"collection": name, "summary": summary, "status": info["result"]["status"], "indexed_vectors_count": indexed_vectors, "expected_indexed_vectors_count": scenario.points}, separators=(",", ":")))
        time.sleep(0.25)


def query_body(scenario: Scenario, query_kind: QueryKind, filter_kind: FilterKind, point_id: int) -> dict[str, Any]:
    body: dict[str, Any] = {
        "query": vector(point_id % scenario.points, scenario, 0x51554452414E54),
        "limit": 10,
        "with_payload": False,
        "with_vector": False,
        "params": {"exact": query_kind is QueryKind.EXACT, "hnsw_ef": scenario.hnsw_ef},
    }
    if filter_kind is FilterKind.TYPED_PAYLOAD:
        body["filter"] = {
            "must": [
                {"key": "authority", "match": {"value": "nudox-capacity-authority-v1"}},
                {"key": "snapshot", "match": {"value": "capacity-snapshot-20260831"}},
                {"key": "model", "match": {"value": "capacity-seeded-f32"}},
                {"key": "metric", "match": {"value": "Cosine"}},
                {"key": "partition", "match": {"value": point_id % 16}},
            ]
        }
    return body


def percentile_ns(values: Sequence[int], percentile: float) -> int:
    ordered = sorted(values)
    index = math.ceil(percentile * len(ordered)) - 1
    return ordered[max(0, min(index, len(ordered) - 1))]


def query_measurement(
    config: WorkloadConfig,
    scenario: Scenario,
    name: str,
    query_kind: QueryKind,
    filter_kind: FilterKind,
    concurrency: int,
) -> dict[str, Any]:
    def one(sample: int) -> Timed:
        response, timed = request_json(
            config,
            Phase.QUERY,
            "POST",
            f"/collections/{name}/points/query",
            query_body(scenario, query_kind, filter_kind, sample),
        )
        points_result = response.get("result", {}).get("points")
        if not isinstance(points_result, list):
            raise HttpFault(Phase.QUERY, FaultKind.SHAPE, json.dumps(response, separators=(",", ":")))
        return timed

    started = time.perf_counter_ns()
    if concurrency == 1:
        timings = [one(sample) for sample in range(config.query_samples)]
    else:
        with ThreadPoolExecutor(max_workers=concurrency) as executor:
            timings = list(executor.map(one, range(config.query_samples)))
    elapsed = time.perf_counter_ns() - started
    latency = [timed.wall_ns for timed in timings]
    return {
        "query_kind": query_kind.value,
        "filter_kind": filter_kind.value,
        "concurrency": concurrency,
        "samples": len(latency),
        "p50_ns": percentile_ns(latency, 0.50),
        "p95_ns": percentile_ns(latency, 0.95),
        "p99_ns": percentile_ns(latency, 0.99),
        "qps": len(latency) * 1_000_000_000 / elapsed,
        "mean_request_bytes": statistics.fmean(timed.request_bytes for timed in timings),
        "mean_response_bytes": statistics.fmean(timed.response_bytes for timed in timings),
    }


def mutation_check(config: WorkloadConfig, scenario: Scenario, name: str) -> dict[str, Any]:
    count = min(64, scenario.points)
    ids = list(range(scenario.points - count, scenario.points))
    _, deleted = request_json(config, Phase.DELETE, "POST", f"/collections/{name}/points/delete?wait=true", {"points": ids})
    reinsert_body = {"points": list(points(ids, scenario, config.seed))}
    _, reinserted = request_json(config, Phase.REINSERT, "PUT", f"/collections/{name}/points?wait=true", reinsert_body)
    readback, readback_timed = request_json(config, Phase.READBACK, "POST", f"/collections/{name}/points", {"ids": [ids[0]], "with_payload": True, "with_vector": False})
    result = readback.get("result")
    if not isinstance(result, list) or len(result) != 1 or result[0].get("id") != ids[0]:
        raise HttpFault(Phase.READBACK, FaultKind.IDENTITY, json.dumps(readback, separators=(",", ":")))
    return {"delete": asdict(deleted), "reinsert": asdict(reinserted), "readback": asdict(readback_timed), "points": count}


def scenarios(selected: str) -> tuple[Scenario, ...]:
    baseline = tuple(
        Scenario(f"baseline-{dimension}d-10k", 10_000, dimension, StorageKind.IN_MEMORY, False, QuantizationKind.NONE, 16, 100, 64)
        for dimension in (384, 768, 1536)
    )
    largest = (Scenario("baseline-768d-100k", 100_000, 768, StorageKind.IN_MEMORY, False, QuantizationKind.NONE, 16, 100, 64),)
    levers = (
        Scenario("on-disk-768d-10k", 10_000, 768, StorageKind.ON_DISK, True, QuantizationKind.NONE, 16, 100, 64),
        Scenario("scalar-int8-768d-10k", 10_000, 768, StorageKind.IN_MEMORY, False, QuantizationKind.SCALAR_INT8, 16, 100, 64),
        Scenario("hnsw-m32-768d-10k", 10_000, 768, StorageKind.IN_MEMORY, False, QuantizationKind.NONE, 32, 100, 64),
        Scenario("hnsw-efconstruct200-768d-10k", 10_000, 768, StorageKind.IN_MEMORY, False, QuantizationKind.NONE, 16, 200, 64),
        Scenario("hnsw-ef128-768d-10k", 10_000, 768, StorageKind.IN_MEMORY, False, QuantizationKind.NONE, 16, 100, 128),
    )
    sets = {"baseline-10k": baseline, "largest-100k": largest, "lever-10k": levers, "all": baseline + largest + levers}
    return sets[selected]


def write_results(output: Path, results: list[dict[str, Any]]) -> None:
    output.mkdir(parents=True, exist_ok=True)
    (output / "result.json").write_text(json.dumps({"schema": "nudox.qdrant-capacity.v1", "results": results}, indent=2) + "\n")
    with (output / "queries.csv").open("w", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=("scenario", "query_kind", "filter_kind", "concurrency", "samples", "p50_ns", "p95_ns", "p99_ns", "qps", "mean_request_bytes", "mean_response_bytes"))
        writer.writeheader()
        for result in results:
            for query in result.get("queries", []):
                writer.writerow({"scenario": result["scenario"]["label"], **query})


def find_scenario(label: str) -> Scenario:
    for scenario in scenarios("all"):
        if scenario.label == label:
            return scenario
    raise ValueError(label)


def write_restart_result(config: WorkloadConfig, label: str) -> int:
    scenario = find_scenario(label)
    name = collection_name(scenario)
    version, _ = request_json(config, Phase.COLLECTION_INFO, "GET", "/", require_qdrant_status=False)
    info, _ = request_json(config, Phase.COLLECTION_INFO, "GET", f"/collections/{name}")
    telemetry, _ = request_json(config, Phase.TELEMETRY, "GET", "/telemetry")
    before_process = process_fact(config.qdrant_pid)
    queries = [
        query_measurement(config, scenario, name, query_kind, filter_kind, concurrency)
        for query_kind in QueryKind
        for filter_kind in FilterKind
        for concurrency in (1, config.multicore_concurrency)
    ]
    after_process = process_fact(config.qdrant_pid)
    config.output.mkdir(parents=True, exist_ok=True)
    (config.output / "restart.json").write_text(
        json.dumps(
            {
                "schema": "nudox.qdrant-capacity.restart.v1",
                "scenario": asdict(scenario),
                "collection": info["result"],
                "qdrant": version,
                "telemetry": telemetry["result"],
                "queries": queries,
                "process_before": asdict(before_process),
                "process_after": asdict(after_process),
                "process_delta": process_delta(before_process, after_process),
            },
            indent=2,
        )
        + "\n"
    )
    return 0


def run(config: WorkloadConfig, selected: str) -> int:
    version, _ = request_json(config, Phase.COLLECTION_INFO, "GET", "/", require_qdrant_status=False)
    results: list[dict[str, Any]] = []
    for scenario in scenarios(selected):
        before_total_bytes = directory_bytes(config.storage_path)
        before_process = process_fact(config.qdrant_pid)
        lifecycle = create_collection(config, scenario)
        name = lifecycle["collection"]
        load = upsert(config, scenario, name)
        optimized = wait_optimizer(config, scenario, name)
        post_optimizer_collection_bytes = collection_directory_bytes(config.storage_path, name)
        queries = [
            query_measurement(config, scenario, name, query_kind, filter_kind, concurrency)
            for query_kind in QueryKind
            for filter_kind in FilterKind
            for concurrency in (1, config.multicore_concurrency)
        ]
        mutation = mutation_check(config, scenario, name)
        mutation_optimizer = wait_optimizer(config, scenario, name)
        telemetry, _ = request_json(config, Phase.TELEMETRY, "GET", "/telemetry")
        post_mutation_collection_bytes = collection_directory_bytes(config.storage_path, name)
        after_total_bytes = directory_bytes(config.storage_path)
        after_process = process_fact(config.qdrant_pid)
        logical_vector_bytes = scenario.points * scenario.dimension * 4
        total_storage_delta_bytes = None if before_total_bytes is None or after_total_bytes is None else after_total_bytes - before_total_bytes
        results.append({
            "scenario": asdict(scenario),
            "qdrant": version,
            "lifecycle": lifecycle,
            "load": load,
            "optimizer": optimized,
            "mutation_optimizer": mutation_optimizer,
            "queries": queries,
            "mutation": mutation,
            "telemetry": telemetry["result"],
            "logical_vector_bytes": logical_vector_bytes,
            "collection_post_optimizer_bytes": post_optimizer_collection_bytes,
            "collection_post_mutation_settled_bytes": post_mutation_collection_bytes,
            "collection_settled_bytes_per_logical_vector_byte": None if post_mutation_collection_bytes is None else post_mutation_collection_bytes / logical_vector_bytes,
            "storage_total_before_bytes": before_total_bytes,
            "storage_total_after_bytes": after_total_bytes,
            "storage_total_delta_bytes": total_storage_delta_bytes,
            "process_before": asdict(before_process),
            "process_after": asdict(after_process),
            "process_delta": process_delta(before_process, after_process),
        })
        write_results(config.output, results)
    return 0


def main() -> int:
    arguments = parse_args()
    if arguments.batch_points < 1 or arguments.query_samples < 1 or arguments.multicore_concurrency < 1:
        raise SystemExit("batch-points, query-samples, and multicore-concurrency must be positive")
    config = WorkloadConfig(
        endpoint=arguments.endpoint,
        output=arguments.output,
        storage_path=arguments.storage_path,
        qdrant_pid=arguments.qdrant_pid,
        batch_points=arguments.batch_points,
        query_samples=arguments.query_samples,
        multicore_concurrency=arguments.multicore_concurrency,
        timeout_seconds=arguments.timeout_seconds,
        optimizer_timeout_seconds=arguments.optimizer_timeout_seconds,
        seed=arguments.seed,
    )
    try:
        if arguments.restart_scenario is not None:
            return write_restart_result(config, arguments.restart_scenario)
        return run(config, arguments.scenario_set)
    except HttpFault as error:
        config.output.mkdir(parents=True, exist_ok=True)
        (config.output / "red.json").write_text(json.dumps({"schema": "nudox.qdrant-capacity.red.v1", "phase": error.phase.value, "category": error.category.value, "status": error.status, "detail": error.detail}, indent=2) + "\n")
        print(error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
