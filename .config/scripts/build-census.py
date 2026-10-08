#!/usr/bin/env python3
"""Bounded, read-only census of live Cargo CLI processes.

The census starts from the host-visible PID/UID/command-name list and
classifies Cargo only after validating its executable and OS-provided argv;
`comm` is only a hint for inaccessible foreign-UID candidates. It does not
reserve a build slot, signal processes, or launch Cargo. A caller
that admits work must still synchronize that decision with its slot manager.
The process list is sampled over an interval, so the result is a snapshot
estimate rather than an atomic admission permit.
"""

from __future__ import annotations

import argparse
import ctypes
import dataclasses
import datetime as _datetime
import json
import os
import pathlib
import re
import selectors
import shutil
import struct
import subprocess
import sys
import time
from collections.abc import Iterable, Mapping
from typing import Any


MAX_PROCESS_COUNT = 32_768
MAX_PROCESS_TABLE_BYTES = 8 * 1024 * 1024
MAX_PROCESS_TABLE_LINE_BYTES = 4 * 1024
PROCESS_TABLE_TIMEOUT_SECONDS = 10.0
NATIVE_SCAN_TIMEOUT_SECONDS = 10.0
MAX_ARG_BYTES = 128 * 1024
MAX_ARG_COUNT = 512
MAX_SNAPSHOT_ARG_BYTES = 8 * 1024 * 1024
MAX_SNAPSHOT_ARG_PROCESSES = 512
MAX_REPORTED_ARGS = 64
MAX_REPORTED_CARGO_ENTRIES = 512
MAX_ANCESTRY_DEPTH = 64
# proc_pidpath rejects buffers above PROC_PIDPATHINFO_MAXSIZE (4*MAXPATHLEN).
MAX_PATH_BYTES = 4 * 1024
MAX_RESOURCE_OUTPUT_BYTES = 64 * 1024
TOTAL_BUILD_CAPACITY = 4
RESERVED_REMOTE_BUILDS = 1
GIB = 1024**3

KNOWN_BUILD_VERBS = frozenset({"build", "check", "test", "run", "bench", "install"})
KNOWN_QUERY_VERBS = frozenset(
    {"metadata", "tree", "locate-project", "read-manifest", "search", "help", "version"}
)
VERSION_FLAGS = frozenset({"--version", "-V"})
HELP_FLAGS = frozenset({"--help", "-h"})
QUERY_FLAGS = frozenset({"--list"})
GLOBAL_VALUE_OPTIONS = frozenset({"--config", "--color", "--explain"})


class CensusError(RuntimeError):
    """The process table could not be read within the tool's fixed bounds."""


@dataclasses.dataclass(frozen=True, slots=True)
class ProcessIdentity:
    pid: int
    start_token: str


@dataclasses.dataclass(frozen=True, slots=True)
class ProcessRecord:
    pid: int
    ppid: int
    pgid: int
    start_token: str | None
    executable: str | None
    argv: tuple[str, ...] | None
    argv_error: str | None = None
    state: str | None = None
    identity_validated: bool = True
    comm_candidate: bool = False
    compiler_candidate: bool = False
    runtime_owner_candidate: bool = False
    unknown_executable_candidate: bool = False


@dataclasses.dataclass(frozen=True, slots=True)
class ProcessSnapshot:
    records: Mapping[int, ProcessRecord]
    started_at_utc: str
    finished_at_utc: str
    complete: bool
    issue_counts: Mapping[str, int]
    race_counts: Mapping[str, int]
    effective_uid: int
    visible_process_count: int
    captured_argv_bytes: int
    captured_argv_processes: int
    argv_budget_exhausted: bool


@dataclasses.dataclass(frozen=True, slots=True)
class _PsRow:
    pid: int
    ppid: int
    pgid: int
    effective_uid: int | None
    started: str
    ucomm: str


@dataclasses.dataclass(slots=True)
class _ArgvBudget:
    """Bound retained Cargo argv data across the whole process sample."""

    captured_bytes: int = 0
    captured_processes: int = 0
    exhausted: bool = False

    def admit(self, byte_count: int) -> bool:
        if self.exhausted:
            return False
        if (
            self.captured_processes >= MAX_SNAPSHOT_ARG_PROCESSES
            or self.captured_bytes + byte_count > MAX_SNAPSHOT_ARG_BYTES
        ):
            self.exhausted = True
            return False
        self.captured_processes += 1
        self.captured_bytes += byte_count
        return True

    def exhaust(self) -> None:
        self.exhausted = True


def _parse_ps_row(raw_line: bytes | str) -> _PsRow | None:
    if isinstance(raw_line, bytes):
        if len(raw_line) > MAX_PROCESS_TABLE_LINE_BYTES:
            raise CensusError("process-table-line-limit")
        line = raw_line.decode("utf-8", errors="replace").strip()
    else:
        if len(raw_line.encode("utf-8")) > MAX_PROCESS_TABLE_LINE_BYTES:
            raise CensusError("process-table-line-limit")
        line = raw_line.strip()
    if not line:
        return None
    fields = line.split(None, 9)
    # PID, PPID, PGID, effective UID, five fixed-width lstart words, ucomm.
    if len(fields) != 10:
        raise CensusError("process-table-malformed-row")
    try:
        pid, ppid, pgid = (int(field, 10) for field in fields[:3])
        uid_number = int(fields[3], 10)
    except ValueError as error:
        raise CensusError("process-table-malformed-id") from error
    if pid <= 0 or ppid < 0 or pgid < 0 or (uid_number < 0 and uid_number != -2):
        raise CensusError("process-table-malformed-id")
    uid = None if uid_number == -2 else uid_number
    started = " ".join(fields[4:9])
    ucomm = fields[9].strip()
    if not started or not ucomm:
        raise CensusError("process-table-malformed-row")
    return _PsRow(pid, ppid, pgid, uid, started, ucomm)


def _parse_ps_rows(output: str) -> list[_PsRow]:
    rows = []
    for line in output.splitlines():
        row = _parse_ps_row(line)
        if row is not None:
            rows.append(row)
            if len(rows) > MAX_PROCESS_COUNT:
                raise CensusError("process-count-limit")
    if len({row.pid for row in rows}) != len(rows):
        raise CensusError("process-table-duplicate-pid")
    return rows


@dataclasses.dataclass(frozen=True, slots=True)
class CargoEntry:
    record: ProcessRecord
    classification: str
    verb: str | None
    reason: str | None
    requested_jobs: int | None


def _argv_command(argv: tuple[str, ...] | None) -> tuple[str, str | None, str | None]:
    """Return (classification, command verb, safe parse reason) from argv tokens."""
    if argv is None:
        return "unknown", None, "argv-unavailable"
    if len(argv) > MAX_ARG_COUNT:
        return "unknown", None, "argv-count-limit"
    if not argv:
        return "unknown", None, "argv-empty"

    # `cargo +toolchain verb` is Cargo's supported toolchain-selector form.
    index = 1
    while index < len(argv):
        token = argv[index]
        if token.startswith("+") and len(token) > 1:
            index += 1
            continue
        if token in VERSION_FLAGS:
            return "introspection", "version", None
        if token in HELP_FLAGS:
            return "introspection", "help", None
        if token in QUERY_FLAGS:
            return "introspection", "list", None
        if token in GLOBAL_VALUE_OPTIONS:
            if index + 1 >= len(argv):
                return "unknown", None, "global-option-value-missing"
            index += 2
            continue
        if token.startswith("--config=") or token.startswith("--color=") or token.startswith("--explain="):
            index += 1
            continue
        if token.startswith("-"):
            # An unfamiliar option before the verb makes the verb position
            # ambiguous; do not mistake a value such as "build" for a command.
            return "unknown", None, "unrecognized-option-before-verb"
        if token == "--":
            return "unknown", None, "separator-before-verb"
        verb = token
        if verb in KNOWN_BUILD_VERBS:
            return "build", verb, None
        if verb in KNOWN_QUERY_VERBS:
            return "introspection", verb, None
        return "unknown", verb, "unclassified-cargo-verb"

    # Invoking Cargo without a subcommand prints help. A dangling toolchain
    # selector is malformed and is kept unknown.
    if index == len(argv):
        if any(token.startswith("+") and len(token) > 1 for token in argv[1:]):
            return "introspection", None, "no-subcommand"
        return "introspection", None, "no-subcommand"
    return "unknown", None, "unclassified-argv"


def _looks_like_cargo(record: ProcessRecord) -> bool:
    if record.executable is None:
        return record.comm_candidate
    return _is_cargo_executable(record.executable)


def _is_cargo_executable(executable: str) -> bool:
    executable_name = pathlib.PurePath(executable).name
    if executable_name.endswith(" (deleted)"):
        executable_name = executable_name.removesuffix(" (deleted)")
    return executable_name in {"cargo", "cargo.exe"}


def _is_rustc_executable(executable: str) -> bool:
    executable_name = pathlib.PurePath(executable).name
    if executable_name.endswith(" (deleted)"):
        executable_name = executable_name.removesuffix(" (deleted)")
    return executable_name in {"rustc", "rustc.exe"}


def _is_runtime_owner_executable(executable: str) -> bool:
    executable_name = pathlib.PurePath(executable).name
    if executable_name.endswith(" (deleted)"):
        executable_name = executable_name.removesuffix(" (deleted)")
    return executable_name in {
        "backend-locald",
        "backend-locald.exe",
        "backend-desktop",
        "backend-desktop.exe",
        "locald",
        "locald.exe",
    }


def _requested_cargo_jobs(argv: tuple[str, ...] | None) -> int | None:
    """Read only an explicit positive Cargo `--jobs` setting from argv."""
    if argv is None:
        return None
    values: list[str] = []
    index = 1
    while index < len(argv):
        token = argv[index]
        if token == "--":
            break
        if token in {"--jobs", "-j"}:
            if index + 1 >= len(argv):
                return None
            values.append(argv[index + 1])
            index += 2
            continue
        if token.startswith("--jobs="):
            values.append(token.partition("=")[2])
        elif token.startswith("-j") and len(token) > 2:
            values.append(token[2:])
        index += 1
    if len(values) != 1 or not values[0].isascii() or not values[0].isdecimal():
        return None
    parsed = int(values[0], 10)
    return parsed if parsed > 0 else None


def _safe_argv(record: ProcessRecord, verb: str | None) -> dict[str, Any]:
    """Describe argv without persisting positional values, paths, or secrets."""
    if record.argv is None:
        return {"status": record.argv_error or "unavailable", "argc": None, "tokens": []}
    tokens: list[str] = []
    if record.argv:
        tokens.append(pathlib.PurePath(record.argv[0]).name[:128])
    verb_written = False
    emitted = record.argv[1 : 1 + max(0, MAX_REPORTED_ARGS - 2)]
    for argument in emitted:
        if argument.startswith("+") and len(argument) > 1:
            tokens.append("+<toolchain>")
        elif not verb_written and verb is not None and argument == verb:
            tokens.append(verb)
            verb_written = True
        elif argument.startswith("--"):
            option = argument.split("=", 1)[0]
            if any(word in option.lower() for word in ("auth", "credential", "key", "password", "secret", "token")):
                tokens.append("<sensitive-option>")
            elif "=" in argument:
                tokens.append(option[:128] + "=<redacted>")
            else:
                tokens.append(option[:128])
        elif argument.startswith("-"):
            if len(argument) > 2:
                tokens.append(argument[:2] + "<redacted>")
            else:
                tokens.append(argument)
        else:
            tokens.append("<arg>")
    truncated = len(record.argv) - 1 > len(emitted)
    if truncated:
        tokens.append("<truncated>")
    return {"status": "complete", "argc": len(record.argv), "truncated": truncated, "tokens": tokens}


def classify_cargo_process(record: ProcessRecord) -> CargoEntry | None:
    """Classify one process only when its actual executable is Cargo."""
    if not _looks_like_cargo(record):
        return None
    if record.state in {"Z", "X", "zombie", "exited"}:
        return CargoEntry(record, "exited", None, "process-not-running", None)
    if record.executable is None:
        return CargoEntry(record, "unknown", None, "cargo-candidate-details-unavailable", None)
    classification, verb, reason = _argv_command(record.argv)
    if record.argv_error:
        classification, verb, reason = "unknown", None, record.argv_error
    return CargoEntry(record, classification, verb, reason, _requested_cargo_jobs(record.argv))


def cargo_entries(snapshot: ProcessSnapshot) -> list[CargoEntry]:
    entries = [entry for record in snapshot.records.values() if (entry := classify_cargo_process(record))]
    entries.sort(key=lambda entry: (entry.record.pid, entry.record.start_token))
    return entries


def _admission_slots(
    cargo: list[CargoEntry],
    members: list[ProcessRecord],
    records: Mapping[int, ProcessRecord],
) -> dict[str, Any]:
    """Count Cargo roots and any residual work not proven below those roots.

    PGID membership alone is not process-tree ownership. A Rust compiler or
    runtime in a Cargo group is folded into a Cargo root's slot only when its
    captured, start-token-qualified ancestry contains that exact Cargo process.
    Otherwise one residual slot covers the unproved members of the PGID.
    """
    roots = {
        ProcessIdentity(entry.record.pid, entry.record.start_token)
        for entry in cargo
        if entry.record.identity_validated and entry.record.start_token is not None
    }
    root_pids = sorted(entry.record.pid for entry in cargo)
    unattributed: list[int] = []
    for member in members:
        ancestry, _complete, _error = ancestry_for(member.pid, records)
        if not any(identity in roots for identity in ancestry):
            unattributed.append(member.pid)
    unattributed = sorted(set(unattributed))

    # Every live Cargo PID consumes one slot, even if nested under another
    # Cargo process. Non-Cargo compiler/runtime PGIDs always consume at least
    # one residual slot; Cargo PGIDs need one only for unproved members.
    residual_slots = 1 if (unattributed or not cargo) else 0
    return {
        "admission_slot_count": len(cargo) + residual_slots if cargo else 1,
        "admission_slot_provenance": {
            "cargo_root_pids": root_pids,
            "residual_slot_count": residual_slots,
            "residual_member_pids": unattributed if cargo else sorted({member.pid for member in members}),
            "residual_reason": (
                "unattributed-active-members" if cargo and unattributed
                else "non-cargo-process-group" if not cargo
                else None
            ),
        },
    }


def compiler_groups(snapshot: ProcessSnapshot, entries: Iterable[CargoEntry]) -> list[dict[str, Any]]:
    """Group compiler, unresolved, and known runtime-owner processes by OS process group."""
    by_pgid: dict[int, dict[str, Any]] = {}
    for entry in entries:
        if entry.classification not in {"build", "unknown"}:
            continue
        group = by_pgid.setdefault(
            entry.record.pgid,
            {"pgid": entry.record.pgid, "cargo": [], "rustc": [], "unknown_rustc": [], "runtime": [], "unknown_runtime": [], "unknown_executable": []},
        )
        group["cargo"].append(entry)
    for record in snapshot.records.values():
        if record.state in {"Z", "X", "zombie", "exited"}:
            continue
        if record.executable is not None and _is_rustc_executable(record.executable):
            by_pgid.setdefault(
                record.pgid,
                {"pgid": record.pgid, "cargo": [], "rustc": [], "unknown_rustc": [], "runtime": [], "unknown_runtime": [], "unknown_executable": []},
            )["rustc"].append(record)
        elif record.unknown_executable_candidate:
            by_pgid.setdefault(
                record.pgid,
                {"pgid": record.pgid, "cargo": [], "rustc": [], "unknown_rustc": [], "runtime": [], "unknown_runtime": [], "unknown_executable": []},
            )["unknown_executable"].append(record)
        elif record.compiler_candidate:
            by_pgid.setdefault(
                record.pgid,
                {"pgid": record.pgid, "cargo": [], "rustc": [], "unknown_rustc": [], "runtime": [], "unknown_runtime": [], "unknown_executable": []},
            )["unknown_rustc"].append(record)
        elif record.executable is not None and _is_runtime_owner_executable(record.executable):
            by_pgid.setdefault(
                record.pgid,
                {"pgid": record.pgid, "cargo": [], "rustc": [], "unknown_rustc": [], "runtime": [], "unknown_runtime": [], "unknown_executable": []},
            )["runtime"].append(record)
        elif record.runtime_owner_candidate:
            by_pgid.setdefault(
                record.pgid,
                {"pgid": record.pgid, "cargo": [], "rustc": [], "unknown_rustc": [], "runtime": [], "unknown_runtime": [], "unknown_executable": []},
            )["unknown_runtime"].append(record)

    result: list[dict[str, Any]] = []
    for pgid, group in sorted(by_pgid.items()):
        cargo: list[CargoEntry] = group["cargo"]
        rustc: list[ProcessRecord] = group["rustc"]
        unknown_rustc: list[ProcessRecord] = group["unknown_rustc"]
        runtime: list[ProcessRecord] = group["runtime"]
        unknown_runtime: list[ProcessRecord] = group["unknown_runtime"]
        unknown_executable: list[ProcessRecord] = group["unknown_executable"]
        members = [*rustc, *unknown_rustc, *runtime, *unknown_runtime, *unknown_executable]
        if not cargo and not rustc and not unknown_rustc and not runtime and not unknown_runtime and not unknown_executable:
            continue
        if cargo:
            unknown = any(entry.classification == "unknown" for entry in cargo) or bool(unknown_rustc or unknown_runtime or unknown_executable)
            jobs = [entry.requested_jobs for entry in cargo]
            requested_jobs = sum(jobs) if jobs and all(value is not None for value in jobs) else None
            admission = _admission_slots(cargo, members, snapshot.records)
            result.append(
                {
                    "pgid": pgid,
                    "kind": "cargo",
                    "classification": "unknown" if unknown else "build",
                    "pids": sorted(
                        [entry.record.pid for entry in cargo]
                        + [record.pid for record in rustc]
                        + [record.pid for record in unknown_rustc]
                        + [record.pid for record in runtime]
                        + [record.pid for record in unknown_runtime]
                        + [record.pid for record in unknown_executable]
                    ),
                    "cargo_pids": sorted(entry.record.pid for entry in cargo),
                    "runtime_owner_pids": sorted(record.pid for record in [*runtime, *unknown_runtime]),
                    "rustc_process_count": len(rustc) + len(unknown_rustc),
                    "requested_cargo_jobs": requested_jobs,
                    "job_limit_known": requested_jobs is not None,
                    **admission,
                }
            )
        elif rustc or unknown_rustc:
            admission = _admission_slots([], members, snapshot.records)
            result.append(
                {
                    "pgid": pgid,
                    "kind": "orphan-rustc",
                    "classification": "unknown" if unknown_rustc or unknown_runtime or unknown_executable else "orphan-rustc",
                    "pids": sorted(record.pid for record in [*rustc, *unknown_rustc, *runtime, *unknown_runtime, *unknown_executable]),
                    "cargo_pids": [],
                    "runtime_owner_pids": sorted(record.pid for record in [*runtime, *unknown_runtime]),
                    "rustc_process_count": len(rustc) + len(unknown_rustc),
                    "requested_cargo_jobs": None,
                    "job_limit_known": False,
                    **admission,
                }
            )
        elif runtime or unknown_runtime:
            admission = _admission_slots([], members, snapshot.records)
            result.append(
                {
                    "pgid": pgid,
                    "kind": "runtime-owner",
                    "classification": "unknown" if unknown_runtime or unknown_executable else "runtime-owner",
                    "pids": sorted(record.pid for record in [*runtime, *unknown_runtime, *unknown_executable]),
                    "cargo_pids": [],
                    "runtime_owner_pids": sorted(record.pid for record in [*runtime, *unknown_runtime]),
                    "rustc_process_count": 0,
                    "requested_cargo_jobs": None,
                    "job_limit_known": False,
                    **admission,
                }
            )
        else:
            admission = _admission_slots([], members, snapshot.records)
            result.append(
                {
                    "pgid": pgid,
                    "kind": "unknown",
                    "classification": "unknown",
                    "pids": sorted(record.pid for record in unknown_executable),
                    "cargo_pids": [],
                    "runtime_owner_pids": [],
                    "rustc_process_count": 0,
                    "requested_cargo_jobs": None,
                    "job_limit_known": False,
                    **admission,
                }
            )
    return result


def decide_capacity(
    entries: Iterable[CargoEntry],
    *,
    snapshot_complete: bool,
    total_build_capacity: int = 4,
    reserved_remote_builds: int = 1,
) -> dict[str, Any]:
    """Evaluate a snapshot; unknown Cargo and process groups occupy a slot."""
    if total_build_capacity < 1 or reserved_remote_builds < 0 or reserved_remote_builds >= total_build_capacity:
        raise ValueError("capacity must leave at least one local build slot")
    counts = {"build": 0, "introspection": 0, "unknown": 0, "exited": 0}
    for entry in entries:
        counts[entry.classification] = counts.get(entry.classification, 0) + 1
    local_capacity = total_build_capacity - reserved_remote_builds
    conservative_occupancy = counts["build"] + counts["unknown"]
    allowed = snapshot_complete and conservative_occupancy + 1 <= local_capacity
    if not snapshot_complete:
        reason = "snapshot-incomplete"
    elif not allowed:
        reason = "local-build-capacity-full"
    else:
        reason = "local-build-capacity-available"
    return {
        "total_build_capacity": total_build_capacity,
        "reserved_remote_builds": reserved_remote_builds,
        "local_build_capacity": local_capacity,
        "known_build_processes": counts["build"],
        "introspection_processes": counts["introspection"],
        "unknown_cargo_processes": counts["unknown"],
        "exited_cargo_processes": counts["exited"],
        "conservative_local_occupancy": conservative_occupancy,
        "snapshot_allows_new_local_build": allowed,
        "decision_reason": reason,
    }


def ancestry_for(
    pid: int,
    records: Mapping[int, ProcessRecord],
    *,
    maximum_depth: int = MAX_ANCESTRY_DEPTH,
) -> tuple[list[ProcessIdentity], bool, str | None]:
    """Return parent identities up to init; incomplete chains are never owned."""
    current = records.get(pid)
    if current is None:
        return [], False, "process-not-in-snapshot"
    if not current.identity_validated or current.start_token is None:
        return [], False, "process-identity-unavailable"
    ancestry: list[ProcessIdentity] = []
    visited = {pid}
    for _ in range(maximum_depth):
        parent_pid = current.ppid
        if parent_pid <= 0:
            return ancestry, True, None
        if parent_pid in visited:
            return ancestry, False, "parent-cycle"
        parent = records.get(parent_pid)
        if parent is None:
            return ancestry, False, "missing-parent"
        if not parent.identity_validated or parent.start_token is None:
            return ancestry, False, "parent-identity-unavailable"
        ancestry.append(ProcessIdentity(parent.pid, parent.start_token))
        visited.add(parent_pid)
        current = parent
    return ancestry, False, "ancestry-depth-limit"


def is_owned_by(
    pid: int,
    owner: ProcessIdentity,
    records: Mapping[int, ProcessRecord],
) -> bool:
    """Prove a live descendant relation to the exact run-owner identity."""
    candidate = records.get(pid)
    if candidate is None or not candidate.identity_validated or candidate.start_token is None:
        return False
    if candidate.pid == owner.pid:
        return candidate.start_token == owner.start_token
    current = candidate
    visited = {candidate.pid}
    for _ in range(MAX_ANCESTRY_DEPTH):
        parent_pid = current.ppid
        parent = records.get(parent_pid)
        if parent is None or parent_pid in visited or not parent.identity_validated or parent.start_token is None:
            return False
        if parent.pid == owner.pid:
            return parent.start_token == owner.start_token
        visited.add(parent_pid)
        current = parent
    return False


def _snapshot_now() -> ProcessSnapshot:
    started = _datetime.datetime.now(_datetime.timezone.utc).isoformat()
    if not (sys.platform.startswith("linux") or sys.platform == "darwin"):
        raise CensusError("unsupported-process-api")
    effective_uid = os.geteuid()
    rows = _host_process_rows()
    argv_budget = _ArgvBudget()
    if sys.platform.startswith("linux"):
        records, issues, races = _linux_processes(effective_uid, rows, argv_budget)
    else:
        records, issues, races = _darwin_processes(effective_uid, rows, argv_budget)
    finished = _datetime.datetime.now(_datetime.timezone.utc).isoformat()
    return ProcessSnapshot(
        records,
        started,
        finished,
        not issues,
        issues,
        races,
        effective_uid,
        len(rows),
        argv_budget.captured_bytes,
        argv_budget.captured_processes,
        argv_budget.exhausted,
    )


def _host_process_rows() -> list[_PsRow]:
    """Read a bounded host-visible PID/UID/comm census, never command lines."""
    if sys.platform == "darwin":
        ps_path = "/bin/ps"
        command = [
            ps_path,
            "-ww",
            "-A",
            "-o",
            "pid=",
            "-o",
            "ppid=",
            "-o",
            "pgid=",
            "-o",
            "uid=",
            "-o",
            "lstart=",
            "-o",
            "ucomm=",
        ]
    elif sys.platform.startswith("linux"):
        ps_path = shutil.which("ps") or "/bin/ps"
        command = [
            ps_path,
            "-ww",
            "-e",
            "-o",
            "pid=",
            "-o",
            "ppid=",
            "-o",
            "pgid=",
            "-o",
            "euid=",
            "-o",
            "lstart=",
            "-o",
            "comm=",
        ]
    else:
        raise CensusError("unsupported-process-api")
    try:
        child = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            close_fds=True,
            env={"LC_ALL": "C"},
        )
    except OSError as error:
        raise CensusError(f"host-process-list-unavailable:{type(error).__name__}") from error
    assert child.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + PROCESS_TABLE_TIMEOUT_SECONDS
    total_bytes = 0
    pending = bytearray()
    rows: list[_PsRow] = []

    def parse_line(raw_line: bytes) -> None:
        row = _parse_ps_row(raw_line)
        if row is None:
            return
        rows.append(row)
        if len(rows) > MAX_PROCESS_COUNT:
            raise CensusError("process-count-limit")

    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise CensusError("host-process-list-timeout")
            events = selector.select(min(remaining, 0.25))
            if not events:
                if child.poll() is not None:
                    # Drain a final EOF notification through the normal read path.
                    events = selector.select(0)
                    if not events:
                        break
                else:
                    continue
            chunk = os.read(child.stdout.fileno(), 16 * 1024)
            if not chunk:
                break
            total_bytes += len(chunk)
            if total_bytes > MAX_PROCESS_TABLE_BYTES:
                raise CensusError("process-table-byte-limit")
            pending.extend(chunk)
            while True:
                newline = pending.find(b"\n")
                if newline < 0:
                    if len(pending) > MAX_PROCESS_TABLE_LINE_BYTES:
                        raise CensusError("process-table-line-limit")
                    break
                raw_line = bytes(pending[:newline])
                del pending[: newline + 1]
                parse_line(raw_line)
        if pending:
            parse_line(bytes(pending))
        remaining = max(0.0, deadline - time.monotonic())
        try:
            return_code = child.wait(timeout=remaining)
        except subprocess.TimeoutExpired as error:
            raise CensusError("host-process-list-timeout") from error
        if return_code != 0:
            raise CensusError("host-process-list-failed")
        if len({row.pid for row in rows}) != len(rows):
            raise CensusError("process-table-duplicate-pid")
        return rows
    except BaseException:
        # This is the exact child started above; never signal a process group.
        if child.poll() is None:
            child.kill()
            child.wait()
        raise
    finally:
        selector.close()
        child.stdout.close()


def _parse_proc_status_uids(status_text: str) -> tuple[int, ...]:
    uid_line = next((line for line in status_text.splitlines() if line.startswith("Uid:")), None)
    if uid_line is None:
        raise ValueError("process-status-missing-uid")
    values = tuple(int(value) for value in uid_line.split()[1:])
    if len(values) < 2:
        raise ValueError("process-status-malformed-uid")
    return values


def _linux_process_record(row: _PsRow, boot_id: str, argv_budget: _ArgvBudget) -> ProcessRecord | None:
    base = pathlib.Path("/proc") / str(row.pid)
    status_text = (base / "status").read_text(encoding="ascii")
    process_uids = _parse_proc_status_uids(status_text)
    if row.effective_uid is not None and process_uids[1] != row.effective_uid:
        # PID exited/reused after ps observed it; do not trust its old ucomm row.
        return None
    stat_text = (base / "stat").read_text(encoding="ascii")
    close = stat_text.rfind(")")
    if close < 0:
        raise ValueError("malformed-stat")
    fields = stat_text[close + 1 :].strip().split()
    state = fields[0]
    ppid = int(fields[1])
    pgid = int(fields[2])
    start_ticks = fields[19]
    executable = os.readlink(base / "exe")
    if _is_cargo_executable(executable):
        if argv_budget.exhausted:
            argv = None
            argv_error = "snapshot-argv-budget-exhausted"
        else:
            with (base / "cmdline").open("rb") as source:
                raw_argv = source.read(MAX_ARG_BYTES + 1)
            if len(raw_argv) > MAX_ARG_BYTES:
                argv_budget.exhaust()
                argv = None
                argv_error = "argv-byte-limit"
            elif not raw_argv:
                argv = None
                argv_error = "argv-empty-or-exited"
            elif not argv_budget.admit(len(raw_argv)):
                argv = None
                argv_error = "snapshot-argv-budget-exhausted"
            else:
                parts = raw_argv.rstrip(b"\0").split(b"\0")
                if len(parts) > MAX_ARG_COUNT:
                    argv_budget.exhaust()
                    argv = None
                    argv_error = "argv-count-limit"
                else:
                    argv = tuple(os.fsdecode(part) for part in parts)
                    argv_error = None
    else:
        if not _is_rustc_executable(executable) and not _is_runtime_owner_executable(executable):
            executable = None
        argv = None
        argv_error = None
    return ProcessRecord(
        pid=row.pid,
        ppid=ppid,
        pgid=pgid,
        start_token=f"linux:{boot_id}:{start_ticks}",
        executable=executable,
        argv=argv,
        argv_error=argv_error,
        state=state,
    )


def _unknown_cargo_candidate(row: _PsRow) -> ProcessRecord:
    return ProcessRecord(
        pid=row.pid,
        ppid=row.ppid,
        pgid=row.pgid,
        start_token=None,
        executable=None,
        argv=None,
        argv_error="cargo-candidate-details-unavailable",
        identity_validated=False,
        comm_candidate=True,
    )


def _unknown_compiler_candidate(row: _PsRow) -> ProcessRecord:
    return ProcessRecord(
        pid=row.pid,
        ppid=row.ppid,
        pgid=row.pgid,
        start_token=None,
        executable=None,
        argv=None,
        argv_error="compiler-candidate-details-unavailable",
        identity_validated=False,
        compiler_candidate=True,
    )


def _unknown_executable_candidate(row: _PsRow, info: _ProcBsdInfo) -> ProcessRecord:
    """Keep a validated process group occupied when Darwin hides its executable path."""
    return ProcessRecord(
        pid=row.pid,
        ppid=int(info.ppid),
        pgid=int(info.pgid),
        start_token=f"darwin:{int(info.start_sec)}:{int(info.start_usec):06d}",
        executable=None,
        argv=None,
        argv_error="executable-unavailable",
        state="Z" if info.status == 5 else str(info.status),
        identity_validated=True,
        unknown_executable_candidate=True,
    )


def _unknown_runtime_owner_candidate(row: _PsRow) -> ProcessRecord:
    return ProcessRecord(
        pid=row.pid,
        ppid=row.ppid,
        pgid=row.pgid,
        start_token=None,
        executable=None,
        argv=None,
        argv_error="runtime-owner-candidate-details-unavailable",
        identity_validated=False,
        runtime_owner_candidate=True,
    )


def _linux_processes(
    effective_uid: int,
    rows: list[_PsRow],
    argv_budget: _ArgvBudget,
) -> tuple[dict[int, ProcessRecord], dict[str, int], dict[str, int]]:
    proc_root = pathlib.Path("/proc")
    try:
        boot_id = (proc_root / "sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
    except OSError as error:
        raise CensusError(f"linux-proc-unavailable:{type(error).__name__}") from error
    records: dict[int, ProcessRecord] = {}
    issues: dict[str, int] = {}
    races: dict[str, int] = {}
    deadline = time.monotonic() + NATIVE_SCAN_TIMEOUT_SECONDS
    for row in rows:
        if time.monotonic() >= deadline:
            issues["native-process-scan-timeout"] = 1
            break
        same_uid = row.effective_uid == effective_uid
        command_hint = pathlib.PurePath(row.ucomm).name
        cargo_hint = command_hint in {"cargo", "cargo.exe"}
        compiler_hint = command_hint in {"rustc", "rustc.exe"}
        runtime_hint = _is_runtime_owner_executable(command_hint)
        if not same_uid and not cargo_hint and not compiler_hint and not runtime_hint:
            continue
        try:
            record = _linux_process_record(row, boot_id, argv_budget)
            if record is not None:
                records[row.pid] = record
            elif same_uid:
                races["pid-uid-changed-during-scan"] = races.get("pid-uid-changed-during-scan", 0) + 1
                if cargo_hint:
                    records[row.pid] = _unknown_cargo_candidate(row)
                elif compiler_hint:
                    records[row.pid] = _unknown_compiler_candidate(row)
                elif runtime_hint:
                    records[row.pid] = _unknown_runtime_owner_candidate(row)
            elif cargo_hint:
                records[row.pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[row.pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[row.pid] = _unknown_runtime_owner_candidate(row)
        except FileNotFoundError:
            races["pid-exited-during-scan"] = races.get("pid-exited-during-scan", 0) + 1
            if cargo_hint:
                records[row.pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[row.pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[row.pid] = _unknown_runtime_owner_candidate(row)
        except PermissionError:
            if same_uid and (cargo_hint or compiler_hint or runtime_hint):
                records[row.pid] = (
                    _unknown_cargo_candidate(row)
                    if cargo_hint
                    else _unknown_compiler_candidate(row)
                    if compiler_hint
                    else _unknown_runtime_owner_candidate(row)
                )
            elif same_uid:
                issues["same-uid-process-unreadable"] = issues.get("same-uid-process-unreadable", 0) + 1
            elif cargo_hint:
                records[row.pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[row.pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[row.pid] = _unknown_runtime_owner_candidate(row)
        except (OSError, ValueError, IndexError):
            if same_uid and (cargo_hint or compiler_hint or runtime_hint):
                records[row.pid] = (
                    _unknown_cargo_candidate(row)
                    if cargo_hint
                    else _unknown_compiler_candidate(row)
                    if compiler_hint
                    else _unknown_runtime_owner_candidate(row)
                )
            elif same_uid:
                issues["same-uid-process-snapshot-incomplete"] = issues.get("same-uid-process-snapshot-incomplete", 0) + 1
            elif cargo_hint:
                records[row.pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[row.pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[row.pid] = _unknown_runtime_owner_candidate(row)
    return records, issues, races


class _ProcBsdInfo(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_uint32),
        ("status", ctypes.c_uint32),
        ("xstatus", ctypes.c_uint32),
        ("pid", ctypes.c_uint32),
        ("ppid", ctypes.c_uint32),
        ("uid", ctypes.c_uint32),
        ("gid", ctypes.c_uint32),
        ("ruid", ctypes.c_uint32),
        ("rgid", ctypes.c_uint32),
        ("svuid", ctypes.c_uint32),
        ("svgid", ctypes.c_uint32),
        ("reserved", ctypes.c_uint32),
        ("comm", ctypes.c_char * 16),
        ("name", ctypes.c_char * 32),
        ("nfiles", ctypes.c_uint32),
        ("pgid", ctypes.c_uint32),
        ("pjobc", ctypes.c_uint32),
        ("ttydev", ctypes.c_uint32),
        ("ttypgid", ctypes.c_uint32),
        ("nice", ctypes.c_int32),
        ("start_sec", ctypes.c_uint64),
        ("start_usec", ctypes.c_uint64),
    ]


def _darwin_processes(
    effective_uid: int,
    rows: list[_PsRow],
    argv_budget: _ArgvBudget,
) -> tuple[dict[int, ProcessRecord], dict[str, int], dict[str, int]]:
    try:
        proc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    except OSError as error:
        raise CensusError(f"darwin-process-api-unavailable:{type(error).__name__}") from error
    proc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    proc.proc_pidinfo.restype = ctypes.c_int
    proc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    proc.proc_pidpath.restype = ctypes.c_int
    libc.sysctl.argtypes = [
        ctypes.POINTER(ctypes.c_int),
        ctypes.c_uint,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_size_t),
        ctypes.c_void_p,
        ctypes.c_size_t,
    ]
    libc.sysctl.restype = ctypes.c_int

    records: dict[int, ProcessRecord] = {}
    issues: dict[str, int] = {}
    races: dict[str, int] = {}
    deadline = time.monotonic() + NATIVE_SCAN_TIMEOUT_SECONDS
    for row in rows:
        if time.monotonic() >= deadline:
            issues["native-process-scan-timeout"] = 1
            break
        pid = row.pid
        same_uid = row.effective_uid == effective_uid
        command_hint = pathlib.PurePath(row.ucomm).name
        cargo_hint = command_hint in {"cargo", "cargo.exe"}
        compiler_hint = command_hint in {"rustc", "rustc.exe"}
        runtime_hint = _is_runtime_owner_executable(command_hint)
        if not same_uid and not cargo_hint and not compiler_hint and not runtime_hint:
            continue
        info = _ProcBsdInfo()
        ctypes.set_errno(0)
        size = proc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
        if size != ctypes.sizeof(info):
            code = ctypes.get_errno()
            if same_uid:
                if code == 3:
                    races["pid-exited-during-scan"] = races.get("pid-exited-during-scan", 0) + 1
                    if cargo_hint:
                        records[pid] = _unknown_cargo_candidate(row)
                    elif compiler_hint:
                        records[pid] = _unknown_compiler_candidate(row)
                    elif runtime_hint:
                        records[pid] = _unknown_runtime_owner_candidate(row)
                else:
                    if cargo_hint:
                        records[pid] = _unknown_cargo_candidate(row)
                    elif compiler_hint:
                        records[pid] = _unknown_compiler_candidate(row)
                    elif runtime_hint:
                        records[pid] = _unknown_runtime_owner_candidate(row)
                    else:
                        issues["same-uid-process-unreadable"] = issues.get("same-uid-process-unreadable", 0) + 1
            elif cargo_hint:
                records[pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[pid] = _unknown_runtime_owner_candidate(row)
            continue
        if row.effective_uid is not None and int(info.uid) != row.effective_uid:
            if same_uid:
                races["pid-uid-changed-during-scan"] = races.get("pid-uid-changed-during-scan", 0) + 1
                if cargo_hint:
                    records[pid] = _unknown_cargo_candidate(row)
                elif compiler_hint:
                    records[pid] = _unknown_compiler_candidate(row)
                elif runtime_hint:
                    records[pid] = _unknown_runtime_owner_candidate(row)
            elif cargo_hint:
                records[pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[pid] = _unknown_runtime_owner_candidate(row)
            continue
        path_buffer = ctypes.create_string_buffer(MAX_PATH_BYTES)
        ctypes.set_errno(0)
        path_size = proc.proc_pidpath(pid, path_buffer, MAX_PATH_BYTES)
        if path_size <= 0 or path_size >= MAX_PATH_BYTES:
            if same_uid:
                if ctypes.get_errno() == 3:
                    races["pid-exited-during-scan"] = races.get("pid-exited-during-scan", 0) + 1
                    if cargo_hint:
                        records[pid] = _unknown_cargo_candidate(row)
                    elif compiler_hint:
                        records[pid] = _unknown_compiler_candidate(row)
                    elif runtime_hint:
                        records[pid] = _unknown_runtime_owner_candidate(row)
                else:
                    if cargo_hint:
                        records[pid] = _unknown_cargo_candidate(row)
                    elif compiler_hint:
                        records[pid] = _unknown_compiler_candidate(row)
                    elif runtime_hint:
                        records[pid] = _unknown_runtime_owner_candidate(row)
                    else:
                        # PID, UID, start time, parent, and process group are
                        # already validated. Keep the group as unknown occupancy
                        # so an unavailable/deleted executable cannot appear as
                        # free capacity or invalidate the entire census.
                        records[pid] = _unknown_executable_candidate(row, info)
            elif cargo_hint:
                records[pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[pid] = _unknown_runtime_owner_candidate(row)
            continue
        executable = os.fsdecode(path_buffer.raw[:path_size])
        if _is_cargo_executable(executable):
            if argv_budget.exhausted:
                argv = None
                argv_error = "snapshot-argv-budget-exhausted"
            else:
                try:
                    argv, argv_bytes = _darwin_argv(libc, pid)
                    if not argv_budget.admit(argv_bytes):
                        argv = None
                        argv_error = "snapshot-argv-budget-exhausted"
                    else:
                        argv_error = None
                except (CensusError, OSError) as error:
                    argv_budget.exhaust()
                    argv = None
                    argv_error = str(error) if isinstance(error, CensusError) else f"argv-read:{type(error).__name__}"
        else:
            if not _is_rustc_executable(executable) and not _is_runtime_owner_executable(executable):
                executable = None
            argv = None
            argv_error = None
        records[pid] = ProcessRecord(
            pid=pid,
            ppid=int(info.ppid),
            pgid=int(info.pgid),
            start_token=f"darwin:{int(info.start_sec)}:{int(info.start_usec):06d}",
            executable=executable,
            argv=argv,
            argv_error=argv_error,
            state="Z" if info.status == 5 else str(info.status),
        )
    return records, issues, races


def _darwin_argv(libc: Any, pid: int) -> tuple[tuple[str, ...], int]:
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2, pid
    required = ctypes.c_size_t(0)
    if libc.sysctl(mib, 3, None, ctypes.byref(required), None, 0) != 0:
        raise CensusError("argv-kern-procargs-size-failed")
    if required.value <= ctypes.sizeof(ctypes.c_int) or required.value > MAX_ARG_BYTES:
        raise CensusError("argv-byte-limit")
    buffer = ctypes.create_string_buffer(required.value)
    length = ctypes.c_size_t(required.value)
    if libc.sysctl(mib, 3, buffer, ctypes.byref(length), None, 0) != 0 or length.value > required.value:
        raise CensusError("argv-kern-procargs-read-failed")
    raw = buffer.raw[: length.value]
    if len(raw) < 4:
        raise CensusError("argv-kern-procargs-malformed")
    argc = struct.unpack_from("=i", raw, 0)[0]
    if argc < 1 or argc > MAX_ARG_COUNT:
        raise CensusError("argv-count-limit")
    path_end = raw.find(b"\0", 4)
    if path_end < 0:
        raise CensusError("argv-kern-procargs-malformed")
    cursor = path_end + 1
    while cursor < len(raw) and raw[cursor] == 0:
        cursor += 1
    argv: list[str] = []
    for _ in range(argc):
        end = raw.find(b"\0", cursor)
        if end < 0:
            raise CensusError("argv-kern-procargs-truncated")
        argv.append(os.fsdecode(raw[cursor:end]))
        cursor = end + 1
    if not argv[0]:
        raise CensusError("argv-empty-argv0")
    return tuple(argv), len(raw)


def _entry_json(entry: CargoEntry, records: Mapping[int, ProcessRecord], owner: ProcessIdentity | None) -> dict[str, Any]:
    record = entry.record
    ancestry, complete, ancestry_error = ancestry_for(record.pid, records)
    value: dict[str, Any] = {
        "pid": record.pid,
        "ppid": record.ppid,
        "pgid": record.pgid,
        "start_token": record.start_token,
        "identity_validated": record.identity_validated and record.start_token is not None,
        "executable": pathlib.PurePath(record.executable).name if record.executable else None,
        "argv_verb": entry.verb,
        "classification": entry.classification,
        "classification_reason": entry.reason,
        "requested_cargo_jobs": entry.requested_jobs,
        "argv": _safe_argv(record, entry.verb),
        "ancestry_complete": complete,
        "ancestry": [dataclasses.asdict(identity) for identity in ancestry],
    }
    if owner is not None:
        value["owned_by_run_owner"] = is_owned_by(record.pid, owner, records)
    if ancestry_error:
        value["ancestry_error"] = ancestry_error
    return value


def _host_resource_snapshot() -> dict[str, Any]:
    sampled_at = _datetime.datetime.now(_datetime.timezone.utc).isoformat()
    if sys.platform.startswith("linux"):
        text = pathlib.Path("/proc/meminfo").read_text(encoding="ascii")
        matches = re.findall(r"^MemAvailable:\s+(\d+) kB$", text, re.MULTILINE)
        if len(matches) != 1:
            raise CensusError("linux-memavailable-missing-or-ambiguous")
        available_bytes = int(matches[0]) * 1024
        memory_source = "linux-proc-meminfo-MemAvailable"
    elif sys.platform == "darwin":
        try:
            result = subprocess.run(
                ["/usr/bin/vm_stat"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=5,
                check=True,
                env={"LC_ALL": "C"},
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise CensusError(f"darwin-vm-stat-unavailable:{type(error).__name__}") from error
        if len(result.stdout) > MAX_RESOURCE_OUTPUT_BYTES:
            raise CensusError("darwin-vm-stat-output-limit")
        text = result.stdout.decode("ascii", errors="strict")
        page_sizes = re.findall(r"^Mach Virtual Memory Statistics: \(page size of (\d+) bytes\)$", text, re.MULTILINE)
        if len(page_sizes) != 1:
            raise CensusError("darwin-vm-stat-page-size-missing-or-ambiguous")
        page_values: dict[str, int] = {}
        for name, raw in re.findall(r"^(Pages free|Pages inactive|Pages speculative):\s+(\d+)\.$", text, re.MULTILINE):
            if name in page_values:
                raise CensusError("darwin-vm-stat-duplicate-page-count")
            page_values[name] = int(raw)
        if set(page_values) != {"Pages free", "Pages inactive", "Pages speculative"}:
            raise CensusError("darwin-vm-stat-page-count-missing")
        available_bytes = sum(page_values.values()) * int(page_sizes[0])
        memory_source = "darwin-vm-stat-free-inactive-speculative"
    else:
        raise CensusError("unsupported-resource-api")
    try:
        disk = shutil.disk_usage(os.path.abspath(os.sep))
    except OSError as error:
        raise CensusError(f"root-disk-usage-unavailable:{type(error).__name__}") from error
    reserve_file = pathlib.Path('/etc/nudox-ci-disk-reserve-gib')
    reserve_bytes = 0
    if reserve_file.exists():
        try:
            reserve_gib = int(reserve_file.read_text().strip())
            if reserve_gib < 1:
                raise ValueError('reserve must be positive')
            reserve_bytes = reserve_gib * 1024 ** 3
        except (OSError, ValueError) as error:
            raise CensusError(f'root-disk-reserve-invalid:{type(error).__name__}') from error
    return {
        "sampled_at_utc": sampled_at,
        "available_memory_bytes": available_bytes,
        "available_memory_source": memory_source,
        "root_disk_available_bytes": disk.free,
        "root_disk_reserved_bytes": reserve_bytes,
        "root_disk_total_bytes": disk.total,
        "root_disk_used_bytes": disk.used,
    }


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--owner-pid", type=int)
    parser.add_argument("--owner-start-token")
    parser.add_argument(
        "--fleet-resources",
        action="store_true",
        help="include a bounded memory/disk sample for the fleet admission advisor",
    )
    args = parser.parse_args(argv)
    if (args.owner_pid is None) != (args.owner_start_token is None):
        parser.error("--owner-pid and --owner-start-token must be provided together")
    return args


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    try:
        snapshot = _snapshot_now()
    except CensusError as error:
        print(json.dumps({"complete": False, "error": str(error)}, sort_keys=True))
        return 2
    entries = cargo_entries(snapshot)
    groups = compiler_groups(snapshot, entries)
    resource_snapshot = None
    resource_error = None
    if args.fleet_resources:
        try:
            resource_snapshot = _host_resource_snapshot()
        except (CensusError, OSError, UnicodeError) as error:
            resource_error = str(error) if isinstance(error, CensusError) else f"resource-snapshot:{type(error).__name__}"
    owner = (
        ProcessIdentity(args.owner_pid, args.owner_start_token)
        if args.owner_pid is not None and args.owner_start_token is not None
        else None
    )
    decision = decide_capacity(
        entries,
        snapshot_complete=snapshot.complete,
        total_build_capacity=TOTAL_BUILD_CAPACITY,
        reserved_remote_builds=RESERVED_REMOTE_BUILDS,
    )
    output = {
        "schema": "build-census.v1",
        "process_scope": "host-visible-process-table",
        "effective_uid": snapshot.effective_uid,
        "visible_process_count": snapshot.visible_process_count,
        "captured_argv_bytes": snapshot.captured_argv_bytes,
        "captured_argv_processes": snapshot.captured_argv_processes,
        "argv_budget_exhausted": snapshot.argv_budget_exhausted,
        "scope_note": "Foreign Cargo, rustc, and locald/backend-locald/backend-desktop-looking rows are inspected; inaccessible candidates count as occupied unknown process groups. Same-UID process executable paths are inspected, and paths that remain unavailable count as unknown occupied process groups. Cargo argv is read through native process APIs and redacted. Other foreign rows and processes hidden by OS namespaces or permissions are not validated, so this advisory cannot guarantee whole-machine capacity.",
        "sample_started_at_utc": snapshot.started_at_utc,
        "sample_finished_at_utc": snapshot.finished_at_utc,
        "snapshot_atomic": False,
        "complete": snapshot.complete,
        "snapshot_issue_counts": dict(sorted(snapshot.issue_counts.items())),
        "snapshot_race_counts": dict(sorted(snapshot.race_counts.items())),
        "entries": [
            _entry_json(entry, snapshot.records, owner)
            for entry in entries[:MAX_REPORTED_CARGO_ENTRIES]
        ],
        "entries_omitted_count": max(0, len(entries) - MAX_REPORTED_CARGO_ENTRIES),
        "compiler_groups": groups[:MAX_REPORTED_CARGO_ENTRIES],
        "compiler_groups_omitted_count": max(0, len(groups) - MAX_REPORTED_CARGO_ENTRIES),
        "capacity": decision,
        "slot_reserved": False,
        "processes_signaled": [],
    }
    if args.fleet_resources:
        output["resource_snapshot_complete"] = resource_snapshot is not None
        output["resource_snapshot"] = resource_snapshot
        if resource_error is not None:
            output["resource_snapshot_error"] = resource_error
    print(json.dumps(output, ensure_ascii=True, indent=2, sort_keys=True))
    if args.fleet_resources and resource_snapshot is None:
        return 2
    if args.fleet_resources:
        return 0
    return 0 if decision["snapshot_allows_new_local_build"] else 75


if __name__ == "__main__":
    raise SystemExit(main())
