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
from collections import defaultdict
from collections.abc import Callable, Mapping
from typing import Any


GIB = 1024**3
FLEET_ADMISSION_SLOT_LIMIT = 16
MAX_CARGO_JOBS_PER_PROCESS = 4
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
        "python": "/root/nudox-corpus-20261006/tool-recovery/roots/fleet-census-python3-3.14.6/bin/python3",
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


def _index_cargo_entries(
    rows: Any,
    *,
    host: str,
    reasons: list[str],
) -> dict[int, list[Mapping[str, Any]]]:
    """Index raw Cargo rows without collapsing duplicate or malformed PIDs."""
    if not isinstance(rows, list):
        reasons.append(f"{host}:cargo-entry-list-missing")
        return {}
    by_pid: dict[int, list[Mapping[str, Any]]] = defaultdict(list)
    for row in rows:
        if not isinstance(row, Mapping):
            reasons.append(f"{host}:cargo-entry-shape-invalid")
            continue
        pid = row.get("pid")
        pgid = row.get("pgid")
        if type(pid) is not int or pid <= 0 or type(pgid) is not int or pgid <= 0:
            reasons.append(f"{host}:cargo-entry-identity-invalid")
            continue
        by_pid[pid].append(row)
    for pid, matches in by_pid.items():
        if len(matches) != 1:
            reasons.append(f"{host}:cargo-entry-pid-join-ambiguous")
    return by_pid


def _admission_slots_for_group(
    group: Mapping[str, Any],
    *,
    host: str,
    reasons: list[str],
) -> int:
    """Validate the sampler's explicit slot provenance, with a safe fallback."""
    raw_pids = group.get("pids")
    pids = raw_pids if isinstance(raw_pids, list) else []
    valid_group_pids = {
        pid for pid in pids if type(pid) is int and pid > 0
    }
    raw_cargo = group.get("cargo_pids")
    cargo_pids = raw_cargo if isinstance(raw_cargo, list) else []
    valid_cargo = [pid for pid in cargo_pids if type(pid) is int and pid > 0]
    cargo_valid = (
        isinstance(raw_cargo, list)
        and len(valid_cargo) == len(raw_cargo)
        and len(set(valid_cargo)) == len(valid_cargo)
        and (group.get("kind") != "cargo" or bool(valid_cargo))
        and set(valid_cargo).issubset(valid_group_pids)
        and (group.get("kind") == "cargo" or not valid_cargo)
    )

    provenance = group.get("admission_slot_provenance")
    valid = (
        isinstance(raw_pids, list)
        and len(valid_group_pids) == len(raw_pids)
        and len(valid_group_pids) == len(pids)
        and isinstance(provenance, Mapping)
        and cargo_valid
    )
    if valid:
        roots = provenance.get("cargo_root_pids")
        residual_count = provenance.get("residual_slot_count")
        residual_members = provenance.get("residual_member_pids")
        residual_reason = provenance.get("residual_reason")
        kind = group.get("kind")
        valid = (
            isinstance(roots, list)
            and all(type(pid) is int and pid > 0 for pid in roots)
            and roots == sorted(valid_cargo)
            and type(residual_count) is int
            and residual_count in {0, 1}
            and isinstance(residual_members, list)
            and all(type(pid) is int and pid > 0 for pid in residual_members)
            and len(set(residual_members)) == len(residual_members)
            and set(residual_members).issubset(valid_group_pids)
            and (
                (kind == "cargo" and (residual_count == 1) == bool(residual_members))
                or (kind != "cargo" and residual_count == 1 and bool(residual_members))
            )
            and residual_reason == (
                "unattributed-active-members" if kind == "cargo" and residual_count == 1
                else "non-cargo-process-group" if kind != "cargo"
                else None
            )
        )
    if valid:
        expected = len(valid_cargo) + residual_count if group.get("kind") == "cargo" else 1
        reported = group.get("admission_slot_count")
        valid = type(reported) is int and reported == expected
    if valid:
        return expected

    reasons.append(f"{host}:compiler-admission-slot-provenance-invalid")
    # A malformed record must never reduce occupancy. Count each identifiable
    # Cargo root plus one residual PGID slot; otherwise retain one slot per PGID.
    fallback_roots = max(1, len(set(valid_cargo)), len(valid_group_pids))
    return fallback_roots + (1 if group.get("kind") == "cargo" else 0)


def _validate_cargo_group_joins(
    group: Mapping[str, Any],
    *,
    host: str,
    entry_by_pid: Mapping[int, list[Mapping[str, Any]]],
    group_coverage: dict[int, int],
    reasons: list[str],
    limitations: list[str],
) -> None:
    if group.get("kind") != "cargo":
        return
    pgid = group.get("pgid")
    cargo_pids = group.get("cargo_pids")
    if (
        not isinstance(cargo_pids, list)
        or not cargo_pids
        or any(type(pid) is not int or pid <= 0 for pid in cargo_pids)
        or len(set(cargo_pids)) != len(cargo_pids)
    ):
        reasons.append(f"{host}:cargo-pid-list-invalid")
        return

    per_pid_jobs: list[int | None] = []
    for pid in cargo_pids:
        group_coverage[pid] = group_coverage.get(pid, 0) + 1
        matches = entry_by_pid.get(pid, [])
        if len(matches) != 1:
            reasons.append(f"{host}:cargo-entry-pid-join-missing-or-ambiguous")
            continue
        row = matches[0]
        classification = row.get("classification")
        if row.get("pgid") != pgid or classification not in {"build", "unknown"}:
            reasons.append(f"{host}:cargo-entry-pid-join-mismatch")
            continue
        if classification == "build" and (
            row.get("identity_validated") is not True
            or not isinstance(row.get("start_token"), str)
            or not row.get("start_token")
        ):
            reasons.append(f"{host}:cargo-entry-process-identity-unvalidated")
        jobs = row.get("requested_cargo_jobs")
        if type(jobs) is int:
            per_pid_jobs.append(jobs)
            if jobs > MAX_CARGO_JOBS_PER_PROCESS:
                reasons.append(f"{host}:active-cargo-process-jobs-exceed-4")
        else:
            per_pid_jobs.append(None)
            limitations.append(f"{host}:active-cargo-process-jobs-unknown-counted-conservatively")

    expected_total = sum(per_pid_jobs) if per_pid_jobs and all(value is not None for value in per_pid_jobs) else None
    reported_total = group.get("requested_cargo_jobs")
    reported_known = group.get("job_limit_known")
    if reported_total != expected_total or reported_known is not (expected_total is not None):
        reasons.append(f"{host}:cargo-group-job-telemetry-mismatch")


def evaluate_fleet(
    samples: Mapping[str, Mapping[str, Any]],
    *,
    destination: str,
    requested_jobs: int,
    hosts: Mapping[str, Mapping[str, Any]] = DEFAULT_HOSTS,
    now: dt.datetime | None = None,
    max_age_seconds: int = MAX_SAMPLE_AGE_SECONDS,
    allow_local_over_cap_for_remote: bool = False,
    destination_memory_only_for_remote: bool = False,
) -> dict[str, Any]:
    """Fail closed on missing/stale/incomplete evidence and enforce fleet caps."""
    now = (now or dt.datetime.now(dt.timezone.utc)).astimezone(dt.timezone.utc)
    reasons: list[str] = []
    if set(samples) != set(hosts):
        reasons.append("fleet-host-set-incomplete-or-unexpected")
    if destination not in hosts:
        reasons.append("destination-unknown")
    if type(requested_jobs) is not int or not 1 <= requested_jobs <= MAX_CARGO_JOBS_PER_PROCESS:
        reasons.append("requested-cargo-jobs-outside-1-through-4")
    if type(max_age_seconds) is not int or not 1 <= max_age_seconds <= 300:
        reasons.append("invalid-sample-age-policy")

    host_summaries: dict[str, Any] = {}
    limitations: list[str] = []
    total_groups = 0
    total_admission_slots = 0
    destination_memory_only = destination_memory_only_for_remote and destination != "local"
    for name, config in hosts.items():
        sample = samples.get(name)
        if not isinstance(sample, Mapping) or sample.get("transport_complete") is not True:
            reasons.append(f"{name}:unreachable-or-invalid-native-census")
            host_summaries[name] = {
                "complete": False,
                "error": sample.get("error") if isinstance(sample, Mapping) else "missing-host-sample",
                "compiler_group_count": None,
                "compiler_admission_slot_count": None,
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
        entry_by_pid = _index_cargo_entries(
            census.get("entries"), host=name, reasons=reasons
        )
        seen_pgids: set[int] = set()
        active_groups: list[dict[str, Any]] = []
        group_coverage: dict[int, int] = {}
        admission_slots = 0
        for group in groups:
            if not isinstance(group, Mapping):
                reasons.append(f"{name}:compiler-group-shape-invalid")
                continue
            pgid = group.get("pgid")
            kind = group.get("kind")
            classification = group.get("classification")
            if type(pgid) is not int or pgid <= 0 or pgid in seen_pgids or kind not in {"cargo", "orphan-rustc", "runtime-owner", "unknown"}:
                reasons.append(f"{name}:compiler-group-identity-invalid")
                continue
            seen_pgids.add(pgid)
            if classification not in {"build", "unknown", "orphan-rustc", "runtime-owner"}:
                reasons.append(f"{name}:compiler-group-classification-invalid")
                continue
            valid_classification = {
                "cargo": {"build", "unknown"},
                "orphan-rustc": {"orphan-rustc", "unknown"},
                "runtime-owner": {"runtime-owner", "unknown"},
                "unknown": {"unknown"},
            }[kind]
            if classification not in valid_classification:
                reasons.append(f"{name}:compiler-group-kind-classification-mismatch")
            active_groups.append(dict(group))
            admission_slots += _admission_slots_for_group(
                group, host=name, reasons=reasons
            )
            _validate_cargo_group_joins(
                group,
                host=name,
                entry_by_pid=entry_by_pid,
                group_coverage=group_coverage,
                reasons=reasons,
                limitations=limitations,
            )
            if classification == "unknown":
                limitations.append(f"{name}:unresolved-compiler-group-counted-conservatively")
        for pid, rows in entry_by_pid.items():
            if len(rows) != 1:
                continue
            if rows[0].get("classification") in {"build", "unknown"} and group_coverage.get(pid) != 1:
                reasons.append(f"{name}:active-cargo-entry-group-join-missing-or-ambiguous")
        count = len(active_groups)
        total_groups += count
        total_admission_slots += admission_slots
        limit = config.get("limit")
        if type(limit) is not int or limit < 1:
            reasons.append(f"{name}:host-limit-invalid")
        elif admission_slots + (1 if name == destination else 0) > limit:
            if allow_local_over_cap_for_remote and name == "local" and destination != "local":
                limitations.append("local:over-host-cap-remote-admission-authorized")
            else:
                reasons.append(f"{name}:host-compiler-admission-slot-limit-reached")
        memory = resources.get("available_memory_bytes")
        memory_floor_applies = not destination_memory_only or name == destination
        if type(memory) is not int or memory < 0:
            reasons.append(f"{name}:available-memory-invalid-or-missing")
        elif memory_floor_applies and memory < MIN_AVAILABLE_MEMORY:
            reasons.append(f"{name}:available-memory-below-8-gib-or-missing")
        elif not memory_floor_applies and memory < MIN_AVAILABLE_MEMORY:
            limitations.append(f"{name}:below-memory-floor-destination-only-admission-authorized")
        host_summaries[name] = {
            "complete": complete,
            "sample_started_at_utc": census.get("sample_started_at_utc"),
            "sample_finished_at_utc": census.get("sample_finished_at_utc"),
            "resource_sampled_at_utc": resources.get("sampled_at_utc"),
            "age_seconds": round(max(ages), 3) if ages else None,
            "visible_process_count": census.get("visible_process_count"),
            "cargo_entries": len(census.get("entries", [])) if isinstance(census.get("entries"), list) else None,
            "compiler_group_count": count,
            "compiler_admission_slot_count": admission_slots,
            "compiler_groups": active_groups,
            "host_limit": limit,
            "host_admission_slot_limit": limit,
            "available_memory_bytes": memory,
            "memory_floor_applies": memory_floor_applies,
            "available_memory_source": resources.get("available_memory_source"),
            "root_disk_available_bytes": resources.get("root_disk_available_bytes"),
            "root_disk_reserved_bytes": resources.get("root_disk_reserved_bytes", 0),
            "snapshot_issue_counts": census.get("snapshot_issue_counts"),
            "snapshot_race_counts": census.get("snapshot_race_counts"),
            "census_sha256": sample.get("census_sha256"),
        }

    if total_admission_slots + (1 if destination in hosts else 0) > FLEET_ADMISSION_SLOT_LIMIT:
        reasons.append("fleet-compiler-admission-slot-limit-reached")
    if destination in host_summaries:
        disk = host_summaries[destination].get("root_disk_available_bytes")
        if type(disk) is not int or disk < MIN_DESTINATION_DISK:
            reasons.append("destination-root-disk-below-16-gib-or-missing")
        reserved = host_summaries[destination].get("root_disk_reserved_bytes")
        if type(reserved) is not int or reserved < 0:
            reasons.append("destination-root-disk-reserve-invalid")
        elif type(disk) is int and reserved and disk < reserved:
            reasons.append("destination-root-disk-below-ci-reserve")
    unique_reasons = list(dict.fromkeys(reasons))
    return {
        "schema": "compiler-capacity-admission.v5",
        "sampled_at_utc": now.isoformat(),
        "destination": destination,
        "allow_local_over_cap_for_remote": allow_local_over_cap_for_remote,
        "requested_cargo_jobs": requested_jobs,
        "current_fleet_compiler_group_count": total_groups,
        "current_fleet_compiler_admission_slot_count": total_admission_slots,
        # Keep old JSON keys for existing report readers. Their explicit
        # compatibility note below prevents treating PGID counts as admission
        # occupancy; advisory_allowed is computed only from admission slots.
        "fleet_compiler_group_limit": FLEET_ADMISSION_SLOT_LIMIT,
        "host_compiler_group_limits": {name: value.get("limit") for name, value in hosts.items()},
        "max_cargo_jobs_per_group": MAX_CARGO_JOBS_PER_PROCESS,
        "fleet_compiler_admission_slot_limit": FLEET_ADMISSION_SLOT_LIMIT,
        "host_compiler_admission_slot_limits": {name: value.get("limit") for name, value in hosts.items()},
        "max_cargo_jobs_per_cargo_process": MAX_CARGO_JOBS_PER_PROCESS,
        "capacity_compatibility_note": (
            "Legacy group-limit and jobs-per-group fields are compatibility aliases only; "
            "compiler_group_count reports raw PGID telemetry, advisory_allowed uses "
            "admission-slot counts, and the legacy jobs-per-group value means the "
            "per-Cargo-process limit."
        ),
        "memory_guard_scope": "destination" if destination_memory_only else "every-host",
        "destination_memory_only_for_remote": destination_memory_only,
        "minimum_available_memory_bytes_each_host": None if destination_memory_only else MIN_AVAILABLE_MEMORY,
        "minimum_available_memory_bytes_destination": MIN_AVAILABLE_MEMORY,
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
    parser.add_argument("--jobs", type=int, default=MAX_CARGO_JOBS_PER_PROCESS)
    parser.add_argument(
        "--allow-local-over-cap-for-remote", action="store_true",
        help="Permit remote admission despite local occupancy; all other fleet, host, job, freshness and memory limits still apply.",
    )
    parser.add_argument("--max-age-seconds", type=int, default=MAX_SAMPLE_AGE_SECONDS)
    parser.add_argument(
        "--destination-memory-only-for-remote", action="store_true",
        help="Authorized remote admission checks the destination memory floor; all hosts still need fresh complete samples and fleet/job limits. Local admission keeps every-host memory floors.",
    )
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
        allow_local_over_cap_for_remote=args.allow_local_over_cap_for_remote,
        destination_memory_only_for_remote=args.destination_memory_only_for_remote,
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
