#!/usr/bin/env python3
"""One-sample-per-host advisory census for bounded compiler admission.

This tool reads the local, ILO, and h16001mac process/resource snapshots once
per invocation. It never reserves a slot, starts a compiler, or signals a
process. An allowed result is only a fresh fleet-capacity observation; the
caller must still acquire its existing managed permit for the destination.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import time
from collections.abc import Callable, Mapping
from typing import Any


GIB = 1024**3
FLEET_GROUP_LIMIT = 16
MAX_CARGO_JOBS = 4
MIN_AVAILABLE_MEMORY = 8 * GIB
MIN_DESTINATION_DISK = 16 * GIB
MAX_SAMPLE_AGE_SECONDS = 60
MAX_CENSUS_OUTPUT_BYTES = 8 * 1024 * 1024
MAX_CENSUS_ERROR_BYTES = 64 * 1024
CENSUS_TIMEOUT_SECONDS = 22
SSH_CONNECT_TIMEOUT_SECONDS = 8

DEFAULT_HOSTS = {
    "local": {"limit": 5, "ssh_target": None, "python": None},
    "ilo": {
        "limit": 8,
        "ssh_target": "root@95.217.56.147",
        "python": "/nix/store/xkz9p0a8m6p08l79mciidsgc8irvy86n-python3-3.14.6-env/bin/python3",
    },
    "h16001mac": {
        "limit": 8,
        "ssh_target": "h16001mac",
        "python": "/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3",
    },
}


def _utc(value: Any) -> dt.datetime | None:
    if not isinstance(value, str):
        return None
    try:
        result = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if result.tzinfo is None or result.utcoffset() is None:
        return None
    return result.astimezone(dt.timezone.utc)


def _safe_remote_value(value: str, *, target: bool) -> bool:
    pattern = r"[A-Za-z0-9._@:-]+" if target else r"/[A-Za-z0-9._+/-]+"
    return re.fullmatch(pattern, value) is not None


def _sample_host(name: str, host: Mapping[str, Any], sampler_source: bytes) -> dict[str, Any]:
    """Invoke the native census exactly once for one configured host."""
    if name == "local":
        command = [sys.executable, "-B", str(pathlib.Path(__file__).with_name("build-census.py")), "--fleet-resources"]
        command_input = None
    else:
        target = host.get("ssh_target")
        python = host.get("python")
        if not isinstance(target, str) or not _safe_remote_value(target, target=True):
            raise ValueError(f"{name}: invalid SSH target")
        if not isinstance(python, str) or not _safe_remote_value(python, target=False):
            raise ValueError(f"{name}: invalid remote Python path")
        remote_command = f"{python} - --fleet-resources"
        command = [
            "ssh",
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            f"ConnectTimeout={SSH_CONNECT_TIMEOUT_SECONDS}",
            "-o",
            "ServerAliveInterval=2",
            "-o",
            "ServerAliveCountMax=2",
            "--",
            target,
            remote_command,
        ]
        command_input = sampler_source
    started = dt.datetime.now(dt.timezone.utc)
    try:
        result = subprocess.run(
            command,
            input=command_input,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=CENSUS_TIMEOUT_SECONDS,
            check=False,
            env={**os.environ, "LC_ALL": "C"},
        )
    except subprocess.TimeoutExpired:
        return {"host": name, "transport_complete": False, "error": "native-census-timeout"}
    except OSError as error:
        return {
            "host": name,
            "transport_complete": False,
            "error": f"native-census-launch:{type(error).__name__}",
        }
    finished = dt.datetime.now(dt.timezone.utc)
    if len(result.stdout) > MAX_CENSUS_OUTPUT_BYTES or len(result.stderr) > MAX_CENSUS_ERROR_BYTES:
        return {"host": name, "transport_complete": False, "error": "native-census-output-limit"}
    try:
        census = json.loads(result.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return {
            "host": name,
            "transport_complete": False,
            "error": "native-census-invalid-json",
            "return_code": result.returncode,
            "stderr_bytes": len(result.stderr),
        }
    if not isinstance(census, dict):
        return {"host": name, "transport_complete": False, "error": "native-census-invalid-shape"}
    return {
        "host": name,
        "transport_complete": True,
        "collector_started_at_utc": started.isoformat(),
        "collector_finished_at_utc": finished.isoformat(),
        "return_code": result.returncode,
        "stderr_bytes": len(result.stderr),
        "census_sha256": hashlib.sha256(result.stdout).hexdigest(),
        "census": census,
    }


def collect_once(
    hosts: Mapping[str, Mapping[str, Any]],
    sampler_source: bytes,
    *,
    sample_host: Callable[[str, Mapping[str, Any], bytes], dict[str, Any]] = _sample_host,
) -> dict[str, dict[str, Any]]:
    """Sample each host exactly once; partial failures are retained as refusals."""
    names = tuple(hosts)
    if not names:
        return {}
    samples: dict[str, dict[str, Any]] = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(names)) as pool:
        futures = {
            name: pool.submit(sample_host, name, hosts[name], sampler_source)
            for name in names
        }
        for name, future in futures.items():
            try:
                samples[name] = future.result()
            except Exception as error:
                samples[name] = {
                    "host": name,
                    "transport_complete": False,
                    "error": f"native-census-worker:{type(error).__name__}",
                }
    return samples


def evaluate_fleet(
    samples: Mapping[str, Mapping[str, Any]],
    *,
    destination: str,
    requested_jobs: int,
    hosts: Mapping[str, Mapping[str, Any]] = DEFAULT_HOSTS,
    now: dt.datetime | None = None,
    max_age_seconds: int = MAX_SAMPLE_AGE_SECONDS,
) -> dict[str, Any]:
    """Fail closed on missing/stale/incomplete evidence and enforce fleet caps."""
    now = (now or dt.datetime.now(dt.timezone.utc)).astimezone(dt.timezone.utc)
    reasons: list[str] = []
    if set(samples) != set(hosts):
        reasons.append("fleet-host-set-incomplete-or-unexpected")
    if destination not in hosts:
        reasons.append("destination-unknown")
    if type(requested_jobs) is not int or not 1 <= requested_jobs <= MAX_CARGO_JOBS:
        reasons.append("requested-cargo-jobs-outside-1-through-4")
    if type(max_age_seconds) is not int or not 1 <= max_age_seconds <= 300:
        reasons.append("invalid-sample-age-policy")

    host_summaries: dict[str, Any] = {}
    limitations: list[str] = []
    total_groups = 0
    for name, config in hosts.items():
        sample = samples.get(name)
        if not isinstance(sample, Mapping) or sample.get("transport_complete") is not True:
            reasons.append(f"{name}:unreachable-or-invalid-native-census")
            host_summaries[name] = {
                "complete": False,
                "error": sample.get("error") if isinstance(sample, Mapping) else "missing-host-sample",
                "compiler_group_count": None,
            }
            continue
        census = sample.get("census")
        if not isinstance(census, Mapping):
            reasons.append(f"{name}:native-census-shape-invalid")
            host_summaries[name] = {"complete": False, "error": "invalid-census-shape"}
            continue
        start = _utc(census.get("sample_started_at_utc"))
        finish = _utc(census.get("sample_finished_at_utc"))
        resources = census.get("resource_snapshot")
        resources = resources if isinstance(resources, Mapping) else {}
        resource_at = _utc(resources.get("sampled_at_utc"))
        ages = [
            (now - stamp).total_seconds()
            for stamp in (start, finish, resource_at)
            if stamp is not None
        ]
        time_valid = (
            start is not None
            and finish is not None
            and resource_at is not None
            and len(ages) == 3
            and start <= finish <= resource_at
            and all(-5 <= age <= max_age_seconds for age in ages)
        )
        complete = (
            sample.get("return_code") == 0
            and census.get("schema") == "build-census.v1"
            and census.get("complete") is True
            and census.get("resource_snapshot_complete") is True
            and census.get("entries_omitted_count") == 0
            and census.get("compiler_groups_omitted_count") == 0
            and time_valid
        )
        if not complete:
            reasons.append(f"{name}:incomplete-or-stale-native-census")

        groups_raw = census.get("compiler_groups")
        groups = groups_raw if isinstance(groups_raw, list) else []
        if not isinstance(groups_raw, list):
            reasons.append(f"{name}:compiler-group-list-missing")
        seen_pgids: set[int] = set()
        active_groups: list[dict[str, Any]] = []
        for group in groups:
            if not isinstance(group, Mapping):
                reasons.append(f"{name}:compiler-group-shape-invalid")
                continue
            pgid = group.get("pgid")
            kind = group.get("kind")
            classification = group.get("classification")
            if type(pgid) is not int or pgid <= 0 or pgid in seen_pgids or kind not in {"cargo", "orphan-rustc", "runtime-owner"}:
                reasons.append(f"{name}:compiler-group-identity-invalid")
                continue
            seen_pgids.add(pgid)
            if classification not in {"build", "unknown", "orphan-rustc", "runtime-owner"}:
                reasons.append(f"{name}:compiler-group-classification-invalid")
                continue
            active_groups.append(dict(group))
            if classification == "unknown":
                limitations.append(f"{name}:unresolved-compiler-group-counted-conservatively")
            if kind == "cargo":
                jobs = group.get("requested_cargo_jobs")
                if group.get("job_limit_known") is not True or type(jobs) is not int:
                    limitations.append(f"{name}:active-cargo-jobs-unknown-group-counted-conservatively")
                elif jobs > MAX_CARGO_JOBS:
                    reasons.append(f"{name}:active-cargo-jobs-exceed-4")
        count = len(active_groups)
        total_groups += count
        limit = config.get("limit")
        if type(limit) is not int or limit < 1:
            reasons.append(f"{name}:host-limit-invalid")
        elif count + (1 if name == destination else 0) > limit:
            reasons.append(f"{name}:host-compiler-group-limit-reached")
        memory = resources.get("available_memory_bytes")
        if type(memory) is not int or memory < MIN_AVAILABLE_MEMORY:
            reasons.append(f"{name}:available-memory-below-8-gib-or-missing")
        host_summaries[name] = {
            "complete": complete,
            "sample_started_at_utc": census.get("sample_started_at_utc"),
            "sample_finished_at_utc": census.get("sample_finished_at_utc"),
            "resource_sampled_at_utc": resources.get("sampled_at_utc"),
            "age_seconds": round(max(ages), 3) if ages else None,
            "visible_process_count": census.get("visible_process_count"),
            "cargo_entries": len(census.get("entries", [])) if isinstance(census.get("entries"), list) else None,
            "compiler_group_count": count,
            "compiler_groups": active_groups,
            "host_limit": limit,
            "available_memory_bytes": memory,
            "available_memory_source": resources.get("available_memory_source"),
            "root_disk_available_bytes": resources.get("root_disk_available_bytes"),
            "snapshot_issue_counts": census.get("snapshot_issue_counts"),
            "snapshot_race_counts": census.get("snapshot_race_counts"),
            "census_sha256": sample.get("census_sha256"),
        }

    if total_groups + (1 if destination in hosts else 0) > FLEET_GROUP_LIMIT:
        reasons.append("fleet-compiler-group-limit-reached")
    if destination in host_summaries:
        disk = host_summaries[destination].get("root_disk_available_bytes")
        if type(disk) is not int or disk < MIN_DESTINATION_DISK:
            reasons.append("destination-root-disk-below-16-gib-or-missing")
    unique_reasons = list(dict.fromkeys(reasons))
    return {
        "schema": "compiler-capacity-admission.v5",
        "sampled_at_utc": now.isoformat(),
        "destination": destination,
        "requested_cargo_jobs": requested_jobs,
        "current_fleet_compiler_group_count": total_groups,
        "fleet_compiler_group_limit": FLEET_GROUP_LIMIT,
        "host_compiler_group_limits": {name: value.get("limit") for name, value in hosts.items()},
        "max_cargo_jobs_per_group": MAX_CARGO_JOBS,
        "minimum_available_memory_bytes_each_host": MIN_AVAILABLE_MEMORY,
        "minimum_destination_root_disk_bytes": MIN_DESTINATION_DISK,
        "max_sample_age_seconds": max_age_seconds,
        "advisory_allowed": not unique_reasons,
        "reasons": unique_reasons,
        "limitations": list(dict.fromkeys(limitations)),
        "slot_reserved": False,
        "managed_host_permit_required": True,
        "processes_signaled": [],
        "hosts": host_summaries,
        "raw_samples": dict(samples),
    }


def _write_exclusive(path: pathlib.Path, report: Mapping[str, Any]) -> None:
    encoded = (json.dumps(report, ensure_ascii=True, sort_keys=True, indent=2) + "\n").encode()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(encoded)
            output.flush()
            os.fsync(output.fileno())
    except BaseException:
        try:
            path.unlink()
        except OSError:
            pass
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", choices=tuple(DEFAULT_HOSTS), required=True)
    parser.add_argument("--jobs", type=int, default=MAX_CARGO_JOBS)
    parser.add_argument("--max-age-seconds", type=int, default=MAX_SAMPLE_AGE_SECONDS)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--ilo-target", default=DEFAULT_HOSTS["ilo"]["ssh_target"])
    parser.add_argument("--ilo-python", default=DEFAULT_HOSTS["ilo"]["python"])
    parser.add_argument("--mac-target", default=DEFAULT_HOSTS["h16001mac"]["ssh_target"])
    parser.add_argument("--mac-python", default=DEFAULT_HOSTS["h16001mac"]["python"])
    args = parser.parse_args(argv)
    hosts = {name: dict(value) for name, value in DEFAULT_HOSTS.items()}
    hosts["ilo"].update(ssh_target=args.ilo_target, python=args.ilo_python)
    hosts["h16001mac"].update(ssh_target=args.mac_target, python=args.mac_python)
    census_path = pathlib.Path(__file__).with_name("build-census.py")
    try:
        source = census_path.read_bytes()
    except OSError as error:
        print(json.dumps({"schema": "compiler-capacity-admission.v5", "advisory_allowed": False, "reasons": [f"native-sampler-read:{type(error).__name__}"], "slot_reserved": False}))
        return 2
    started = dt.datetime.now(dt.timezone.utc)
    started_monotonic = time.monotonic()
    samples = collect_once(hosts, source)
    report = evaluate_fleet(
        samples,
        destination=args.destination,
        requested_jobs=args.jobs,
        hosts=hosts,
        max_age_seconds=args.max_age_seconds,
    )
    report["collector_started_at_utc"] = started.isoformat()
    report["sampler_path"] = str(census_path.resolve())
    report["sampler_sha256"] = hashlib.sha256(source).hexdigest()
    report["collector_elapsed_seconds"] = round(time.monotonic() - started_monotonic, 3)
    if args.output is not None:
        report["evidence_path"] = str(args.output.resolve())
        try:
            _write_exclusive(args.output, report)
        except OSError as error:
            report["advisory_allowed"] = False
            report["reasons"] = [*report["reasons"], f"evidence-write:{type(error).__name__}"]
    print(json.dumps(report, ensure_ascii=True, sort_keys=True, indent=2))
    if not report["advisory_allowed"]:
        return 75
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
