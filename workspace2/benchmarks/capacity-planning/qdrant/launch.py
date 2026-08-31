#!/usr/bin/env python3
"""Launch the pinned Qdrant binary and preserve a reproducible REST benchmark record."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Sequence


class LaunchPhase(str, Enum):
    CONFIGURE = "configure"
    START_COLD = "start_cold"
    RUN_WORKLOAD = "run_workload"
    STOP_COLD = "stop_cold"
    START_WARM = "start_warm"
    RUN_RESTART = "run_restart"
    STOP_WARM = "stop_warm"


class LaunchFaultKind(str, Enum):
    CONFIGURATION = "configuration"
    PROCESS = "process"
    READINESS = "readiness"
    COMMAND = "command"


@dataclass(frozen=True)
class LaunchFault(Exception):
    phase: LaunchPhase
    kind: LaunchFaultKind
    detail: str

    def __str__(self) -> str:
        return f"{self.phase.value}:{self.kind.value}:{self.detail}"


@dataclass(frozen=True)
class LaunchConfig:
    output: Path
    project: Path
    qdrant_binary: Path
    runner: Path
    http_port: int
    grpc_port: int
    scenario_set: str
    restart_scenario: str | None
    batch_points: int
    query_samples: int
    multicore_concurrency: int
    timeout_seconds: float
    optimizer_timeout_seconds: float
    seed: int


@dataclass(frozen=True)
class ServiceRun:
    phase: LaunchPhase
    readiness_wall_ns: int
    qdrant_pid: int
    time_wrapper_pid: int
    command: tuple[str, ...]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--qdrant-binary", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--http-port", type=int, default=26335)
    parser.add_argument("--grpc-port", type=int, default=26336)
    parser.add_argument("--scenario-set", choices=("baseline-10k", "largest-100k", "lever-10k", "all"), default="baseline-10k")
    parser.add_argument(
        "--restart-scenario",
        choices=("baseline-384d-10k", "baseline-768d-10k", "baseline-1536d-10k", "baseline-768d-100k", "on-disk-768d-10k", "scalar-int8-768d-10k", "hnsw-m32-768d-10k", "hnsw-efconstruct200-768d-10k", "hnsw-ef128-768d-10k"),
    )
    parser.add_argument("--batch-points", type=int, default=128)
    parser.add_argument("--query-samples", type=int, default=48)
    parser.add_argument("--multicore-concurrency", type=int, default=4)
    parser.add_argument("--timeout-seconds", type=float, default=30.0)
    parser.add_argument("--optimizer-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--seed", type=int, default=0x4E55444F58)
    return parser.parse_args()


def checked_command(phase: LaunchPhase, command: Sequence[str]) -> str:
    try:
        completed = subprocess.run(command, check=False, capture_output=True, text=True)
    except OSError as error:
        raise LaunchFault(phase, LaunchFaultKind.COMMAND, f"command={tuple(command)!r} source={error}") from error
    if completed.returncode != 0:
        raise LaunchFault(
            phase,
            LaunchFaultKind.COMMAND,
            f"command={tuple(command)!r} exit={completed.returncode} stderr={completed.stderr[:8_192]}",
        )
    return completed.stdout.strip()


def command_fact(command: Sequence[str]) -> dict[str, Any]:
    try:
        return {"command": list(command), "output": checked_command(LaunchPhase.CONFIGURE, command), "availability": "measured"}
    except LaunchFault as error:
        return {"command": list(command), "availability": "unavailable", "error": asdict(error)}


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            block = source.read(1 << 20)
            if not block:
                return digest.hexdigest()
            digest.update(block)


def source_digest(project: Path) -> dict[str, str]:
    paths = (
        project / "benchmarks/capacity-planning/qdrant/runner.py",
        project / "benchmarks/capacity-planning/qdrant/launch.py",
        project / "benchmarks/capacity-planning/qdrant/run-local-nix.sh",
    )
    return {str(path.relative_to(project)): file_sha256(path) for path in paths}


def read_ready(phase: LaunchPhase, endpoint: str, timeout_seconds: float) -> None:
    request = urllib.request.Request(f"{endpoint}/readyz", method="GET")
    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            body = response.read()
    except urllib.error.URLError as error:
        raise LaunchFault(phase, LaunchFaultKind.READINESS, str(error.reason)) from error
    if response.status != 200 or body != b"all shards are ready":
        raise LaunchFault(phase, LaunchFaultKind.READINESS, f"status={response.status} body={body[:8_192]!r}")


def qdrant_child_pid(phase: LaunchPhase, time_wrapper_pid: int) -> int:
    output = checked_command(phase, ("pgrep", "-P", str(time_wrapper_pid)))
    lines = output.splitlines()
    if len(lines) != 1:
        raise LaunchFault(phase, LaunchFaultKind.PROCESS, f"time_wrapper_pid={time_wrapper_pid} children={lines!r}")
    try:
        return int(lines[0])
    except ValueError as error:
        raise LaunchFault(phase, LaunchFaultKind.PROCESS, f"child_pid={lines[0]!r}") from error


def start_service(config: LaunchConfig, phase: LaunchPhase, endpoint: str) -> tuple[subprocess.Popen[bytes], ServiceRun]:
    storage = config.output / "storage"
    log_path = config.output / f"qdrant-{phase.value}.log"
    time_path = config.output / f"qdrant-{phase.value}.time.txt"
    environment = os.environ.copy()
    environment.update(
        {
            "QDRANT__SERVICE__HTTP_PORT": str(config.http_port),
            "QDRANT__SERVICE__GRPC_PORT": str(config.grpc_port),
            "QDRANT__STORAGE__STORAGE_PATH": str(storage),
        }
    )
    command = ("/usr/bin/time", "-l", str(config.qdrant_binary), "--disable-telemetry")
    with log_path.open("wb") as log, time_path.open("wb") as process_time:
        process = subprocess.Popen(command, cwd=storage.parent, env=environment, stdout=log, stderr=process_time)
    started = time.perf_counter_ns()
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        if process.poll() is not None:
            detail = time_path.read_text(errors="replace")[-8_192:]
            raise LaunchFault(phase, LaunchFaultKind.PROCESS, f"exit={process.returncode} process_time={detail}")
        try:
            read_ready(phase, endpoint, 0.25)
            child = qdrant_child_pid(phase, process.pid)
            return process, ServiceRun(phase, time.perf_counter_ns() - started, child, process.pid, command)
        except LaunchFault as error:
            if error.kind is not LaunchFaultKind.READINESS:
                process.terminate()
                process.wait(timeout=10)
                raise LaunchFault(phase, error.kind, error.detail) from error
        time.sleep(0.1)
    process.terminate()
    process.wait(timeout=10)
    raise LaunchFault(phase, LaunchFaultKind.READINESS, "timeout_seconds=30")


def stop_service(phase: LaunchPhase, process: subprocess.Popen[bytes], service: ServiceRun) -> dict[str, Any]:
    try:
        os.kill(service.qdrant_pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=30)
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.wait(timeout=10)
        raise LaunchFault(phase, LaunchFaultKind.PROCESS, "qdrant did not stop after SIGTERM") from error
    return {"exit_code": process.returncode, "time_wrapper_pid": service.time_wrapper_pid, "qdrant_pid": service.qdrant_pid}


def runner_command(config: LaunchConfig, endpoint: str, qdrant_pid: int, restart: bool) -> tuple[str, ...]:
    command: list[str] = [
        sys.executable,
        str(config.runner),
        "--endpoint",
        endpoint,
        "--output",
        str(config.output),
        "--storage-path",
        str(config.output / "storage"),
        "--qdrant-pid",
        str(qdrant_pid),
        "--batch-points",
        str(config.batch_points),
        "--query-samples",
        str(config.query_samples),
        "--multicore-concurrency",
        str(config.multicore_concurrency),
        "--timeout-seconds",
        str(config.timeout_seconds),
        "--optimizer-timeout-seconds",
        str(config.optimizer_timeout_seconds),
        "--seed",
        str(config.seed),
    ]
    if restart:
        if config.restart_scenario is None:
            raise LaunchFault(LaunchPhase.RUN_RESTART, LaunchFaultKind.CONFIGURATION, "restart scenario is missing")
        command.extend(("--restart-scenario", config.restart_scenario))
    else:
        command.extend(("--scenario-set", config.scenario_set))
    return tuple(command)


def run_runner(phase: LaunchPhase, command: tuple[str, ...]) -> None:
    try:
        completed = subprocess.run(command, check=False)
    except OSError as error:
        raise LaunchFault(phase, LaunchFaultKind.COMMAND, f"command={command!r} source={error}") from error
    if completed.returncode != 0:
        raise LaunchFault(phase, LaunchFaultKind.COMMAND, f"command={command!r} exit={completed.returncode}")


def time_facts(output: Path, phase: LaunchPhase) -> dict[str, Any]:
    path = output / f"qdrant-{phase.value}.time.txt"
    text = path.read_text(errors="replace")
    maximum_rss_bytes: int | None = None
    for line in text.splitlines():
        if "maximum resident set size" not in line:
            continue
        first = line.strip().split(maxsplit=1)[0]
        try:
            maximum_rss_bytes = int(first)
        except ValueError:
            maximum_rss_bytes = None
    return {"raw_file": path.name, "maximum_resident_set_bytes": maximum_rss_bytes, "raw": text}


def ensure_output(config: LaunchConfig) -> None:
    if config.output.exists() and any(config.output.iterdir()):
        raise LaunchFault(LaunchPhase.CONFIGURE, LaunchFaultKind.CONFIGURATION, f"output_not_empty={config.output}")
    if config.http_port < 1 or config.http_port > 65535 or config.grpc_port < 1 or config.grpc_port > 65535:
        raise LaunchFault(LaunchPhase.CONFIGURE, LaunchFaultKind.CONFIGURATION, "port_out_of_range")
    if config.batch_points < 1 or config.query_samples < 1 or config.multicore_concurrency < 1:
        raise LaunchFault(LaunchPhase.CONFIGURE, LaunchFaultKind.CONFIGURATION, "batch/query/concurrency must be positive")
    config.output.mkdir(parents=True, exist_ok=False)
    (config.output / "storage").mkdir()


def provenance(config: LaunchConfig) -> dict[str, Any]:
    return {
        "schema": "nudox.qdrant-capacity.provenance.v1",
        "config": asdict(config),
        "commands": {
            "qdrant_version": command_fact((str(config.qdrant_binary), "--version")),
            "rustc_version": command_fact(("rustc", "--version", "--verbose")),
            "uname": command_fact(("uname", "-a")),
            "macos_version": command_fact(("sw_vers",)),
            "cpu_brand": command_fact(("sysctl", "-n", "machdep.cpu.brand_string")),
            "physical_cores": command_fact(("sysctl", "-n", "hw.physicalcpu")),
            "memory_bytes": command_fact(("sysctl", "-n", "hw.memsize")),
            "source_head": command_fact(("git", "-C", str(config.project), "rev-parse", "HEAD")),
        },
        "qdrant_binary_sha256": file_sha256(config.qdrant_binary),
        "benchmark_source_sha256": source_digest(config.project),
        "base_binary_includes_embedding_runtime": False,
        "embedding_inference": "unmeasured_external_service",
    }


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(json.dumps(value, indent=2, default=str) + "\n")


def run(config: LaunchConfig) -> int:
    ensure_output(config)
    write_json(config.output / "provenance.json", provenance(config))
    endpoint = f"http://127.0.0.1:{config.http_port}"
    cold_process, cold_service = start_service(config, LaunchPhase.START_COLD, endpoint)
    try:
        run_runner(LaunchPhase.RUN_WORKLOAD, runner_command(config, endpoint, cold_service.qdrant_pid, False))
    finally:
        cold_stop = stop_service(LaunchPhase.STOP_COLD, cold_process, cold_service)
    lifecycle: dict[str, Any] = {
        "schema": "nudox.qdrant-capacity.lifecycle.v1",
        "cold": {"service": asdict(cold_service), "stop": cold_stop, "process_time": time_facts(config.output, LaunchPhase.START_COLD)},
    }
    if config.restart_scenario is not None:
        warm_process, warm_service = start_service(config, LaunchPhase.START_WARM, endpoint)
        try:
            run_runner(LaunchPhase.RUN_RESTART, runner_command(config, endpoint, warm_service.qdrant_pid, True))
        finally:
            warm_stop = stop_service(LaunchPhase.STOP_WARM, warm_process, warm_service)
        lifecycle["warm"] = {"service": asdict(warm_service), "stop": warm_stop, "process_time": time_facts(config.output, LaunchPhase.START_WARM)}
    write_json(config.output / "lifecycle.json", lifecycle)
    return 0


def main() -> int:
    arguments = parse_args()
    config = LaunchConfig(
        output=arguments.output.resolve(),
        project=arguments.project.resolve(),
        qdrant_binary=arguments.qdrant_binary.resolve(),
        runner=arguments.runner.resolve(),
        http_port=arguments.http_port,
        grpc_port=arguments.grpc_port,
        scenario_set=arguments.scenario_set,
        restart_scenario=arguments.restart_scenario,
        batch_points=arguments.batch_points,
        query_samples=arguments.query_samples,
        multicore_concurrency=arguments.multicore_concurrency,
        timeout_seconds=arguments.timeout_seconds,
        optimizer_timeout_seconds=arguments.optimizer_timeout_seconds,
        seed=arguments.seed,
    )
    try:
        return run(config)
    except LaunchFault as error:
        if config.output.exists():
            write_json(config.output / "launch-red.json", {"schema": "nudox.qdrant-capacity.launch-red.v1", "phase": error.phase.value, "kind": error.kind.value, "detail": error.detail})
        print(error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
