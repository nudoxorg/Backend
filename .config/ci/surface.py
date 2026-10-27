#!/usr/bin/env python3
"""T2 surface tier: real product operations, snapshotted, with performance.

    python3 .config/ci/surface.py --bin-dir DIR [--case NAME ...] [--update]

For each case in tests/surface/cases.json the harness starts the exact
`backend-locald` it is given (normally from the T1 artifact, never rebuilt),
indexes the case's project through the CLI, then runs the case's CLI and MCP
operations against the same owner. Every answer is compared with the reviewed
snapshot in tests/surface/snapshots/<case>.json; a difference fails and prints
a diff for the author to explain. The same run records wall time per
operation and the owner's peak RSS and CPU time, compared with the reviewed
baseline in the snapshot under a tolerance band.

Projects live at fixed paths (/tmp/nudox-surface/<case>/project); private
owner state lives on the writable source/task volume. Project coordinates
and content-derived identities are identical on every worker
and snapshots need no path rewriting. --update rewrites snapshots and
baselines for review instead of comparing.

Exit 0 when every case matches, 1 otherwise.
"""

from __future__ import annotations

import argparse
import copy
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from contextvars import ContextVar
from contextlib import contextmanager
import difflib
import hashlib
import fcntl
import json
import math
import os
import re
import shutil
import signal
import stat
import subprocess
import process_custody
import sys
import tempfile
import time
import threading
from pathlib import Path
import perf_context

ROOT = Path(__file__).resolve().parents[2]
SURFACE = ROOT / "tests" / "surface"
SCRATCH = Path("/tmp/nudox-surface")
# A performance number may grow this much over its reviewed baseline before
# the case fails. Generous on purpose: CI workers are shared and noisy.
TOLERANCE = {"seconds": 1.5, "peak_rss_kib": 1.25, "cpu_seconds": 1.5}
# Below these floors noise dominates and a change is not a regression.
FLOOR = {"seconds": 2.0, "peak_rss_kib": 64 * 1024, "cpu_seconds": 2.0}
SEARCH_PREPARATION_DIAGNOSTICS_ENV = "NUDOX_SEARCH_PREPARATION_DIAGNOSTICS"
OWNER_USAGE_DIAGNOSTICS_ENV = "NUDOX_OWNER_USAGE_DIAGNOSTICS"
OWNER_USAGE_DIAGNOSTIC_LIMIT = 8192
SEARCH_PREPARATION_RECEIPT = "search-preparation-worker-v1.json"
SEARCH_PREPARATION_RECEIPT_LIMIT = 4096
SEARCH_PREPARATION_DIAGNOSTIC_LIMIT = 4096
# Values that legitimately change from run to run.
VOLATILE_KEYS = {"elapsed_ms", "duration_ms", "timestamp", "started_at", "finished_at", "generated_at", "pid"}


LOCALD_ENDPOINT_START_TIMEOUT_SECONDS = 30
LOCALD_ENDPOINT_DIAGNOSTIC_SECONDS = 25
LOCALD_ENDPOINT_START_POLL_SECONDS = 0.05
LOCALD_STARTUP_CHECKPOINT_NAME = "owner-startup-active-v1.json"
LOCALD_STARTUP_CHECKPOINT_MAX_BYTES = 4096
LOCALD_PROCESS_CONTEXT_STATUS_MAX_BYTES = 8192
LOCALD_PROCESS_CONTEXT_CGROUP_MAX_BYTES = 4096

# Continuation receipts are retained and written to the worker event stream.
# Apply a new 1 MiB complete-receipt cap to either transport and cap each
# proof chain at 64 MiB; page-count overrides remain available per case.
MAX_CONTINUATION_RECEIPT_BYTES = 1024 * 1024
MAX_CONTINUATION_CHAIN_BYTES = 64 * 1024 * 1024


def _startup_checkpoint(workspace, pid):
    """Return a bounded summary of this owner's last composition phase."""
    path = Path(workspace) / LOCALD_STARTUP_CHECKPOINT_NAME
    try:
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NONBLOCK", 0)
        descriptor = os.open(path, flags)
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode):
                return "unavailable:not-regular"
            if info.st_size > LOCALD_STARTUP_CHECKPOINT_MAX_BYTES:
                return "unavailable:oversized"
            with os.fdopen(descriptor, "rb", closefd=False) as stream:
                raw = stream.read(LOCALD_STARTUP_CHECKPOINT_MAX_BYTES + 1)
        finally:
            os.close(descriptor)
    except FileNotFoundError:
        return "unavailable:absent"
    except OSError:
        return "unavailable:unreadable"
    if len(raw) > LOCALD_STARTUP_CHECKPOINT_MAX_BYTES:
        return "unavailable:oversized"
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return "unavailable:malformed"
    if not isinstance(value, dict) or value.get("version") != 1 or value.get("kind") != "owner-composition-checkpoint":
        return "unavailable:unsupported"
    if value.get("process_id") != pid:
        return "unavailable:pid-mismatch"
    state = value.get("state")
    phase = value.get("phase")
    known_phases = {
        "configuration", "workspace-open", "directory-admission", "embedding-provision",
        "compiler-selection", "compiler-owner", "semantic-authority-open",
        "semantic-reconcile", "view-recovery", "view-restore", "view-publication",
        "sql-projection", "projection-watermarks", "cluster-owner", "command-owner",
        "query-warmup",
    }
    if state not in {"opening", "completed", "refused"} or phase not in known_phases:
        return "unavailable:invalid-fields"
    phase_elapsed = value.get("phase_elapsed_micros")
    elapsed = value.get("elapsed_micros")
    completed = value.get("completed_phases")
    if not isinstance(phase_elapsed, int) or isinstance(phase_elapsed, bool) or phase_elapsed < 0:
        phase_elapsed = "unknown"
    if not isinstance(elapsed, int) or isinstance(elapsed, bool) or elapsed < 0:
        elapsed = "unknown"
    if isinstance(completed, list) and len(completed) <= 16:
        completed_rows = []
        for row in completed:
            if not isinstance(row, dict):
                completed_rows.append("invalid")
                continue
            completed_phase = row.get("phase")
            outcome = row.get("outcome")
            if completed_phase not in known_phases or outcome not in {"completed", "refused", "unmeasured"}:
                completed_rows.append("invalid")
            else:
                completed_rows.append(f"{completed_phase}:{outcome}")
        completed_summary = ",".join(completed_rows)
    else:
        completed_summary = "unknown"
    return (
        f"state={state},phase={phase},phase_elapsed_us={phase_elapsed},"
        f"elapsed_us={elapsed},completed_phases={completed_summary}"
    )


def _bounded_proc_text(path, limit=512):
    try:
        with open(path, "rb") as stream:
            raw = stream.read(limit + 1)
    except OSError as error:
        return f"unsupported:{type(error).__name__}"
    suffix = ":truncated" if len(raw) > limit else ""
    return raw[:limit].decode("utf-8", errors="replace").strip() + suffix


def _proc_namespace_identity(path):
    """Return bounded namespace link and inode identity, or an explicit error."""
    try:
        target = os.readlink(path)
        info = os.stat(path)
    except OSError as error:
        return f"unsupported:{type(error).__name__}"
    return {"target": target[:128], "device": info.st_dev, "inode": info.st_ino}


def _proc_nspid(path):
    """Read only NSpid from a bounded proc status snapshot."""
    status = _bounded_proc_text(path, LOCALD_PROCESS_CONTEXT_STATUS_MAX_BYTES)
    if status.startswith("unsupported:"):
        return status
    for line in status.splitlines():
        if line.startswith("NSpid:"):
            values = line.split(":", 1)[1].split()
            if values and all(value.isdecimal() for value in values):
                return [int(value) for value in values]
            return "unsupported:malformed-NSpid"
    return "unsupported:missing-NSpid"


def _proc_starttime_ticks(stat_text):
    """Parse Linux /proc/PID/stat field 22 from the already bounded stat text."""
    if stat_text.startswith("unsupported:"):
        return stat_text
    try:
        fields = stat_text.rsplit(")", 1)[1].split()
        value = fields[19]  # fields begin at field 3 (state); starttime is field 22.
        return int(value) if value.isdecimal() else "unsupported:malformed-starttime"
    except (IndexError, ValueError):
        return "unsupported:missing-starttime"


def _locald_process_context(pid, base, proc_root):
    """Bind the child PID to this proc/PID namespace and boot, with bounded reads."""
    self_root = Path(proc_root) / "self"
    status_path = base / "status"
    cgroup_path = base / "cgroup"
    self_status_path = self_root / "status"
    self_cgroup_path = self_root / "cgroup"
    stat_text = _bounded_proc_text(base / "stat")
    return {
        "observer_pid": os.getpid(),
        "observer_nspid": _proc_nspid(self_status_path),
        "observer_pid_namespace": _proc_namespace_identity(self_root / "ns" / "pid"),
        "observer_mount_namespace": _proc_namespace_identity(self_root / "ns" / "mnt"),
        "observer_cgroup": _bounded_proc_text(self_cgroup_path, LOCALD_PROCESS_CONTEXT_CGROUP_MAX_BYTES),
        "child_pid": pid,
        "child_nspid": _proc_nspid(status_path),
        "child_starttime_ticks": _proc_starttime_ticks(stat_text),
        "child_pid_namespace": _proc_namespace_identity(base / "ns" / "pid"),
        "child_mount_namespace": _proc_namespace_identity(base / "ns" / "mnt"),
        "child_cgroup": _bounded_proc_text(cgroup_path, LOCALD_PROCESS_CONTEXT_CGROUP_MAX_BYTES),
        "boot_id": _bounded_proc_text(Path(proc_root) / "sys" / "kernel" / "random" / "boot_id", 128),
    }


def _locald_process_diagnostic(pid, case_root, workspace_root, executable, *, proc_root=Path("/proc")):
    """Bounded, path-filtered observation of one still-owned startup child."""
    allowed_roots = []
    for kind, path in (("case", case_root), ("workspace", workspace_root)):
        try:
            allowed_roots.append((kind, Path(path).resolve()))
        except (OSError, RuntimeError):
            allowed_roots.append((kind, None))
    executable = None if executable is None else Path(executable)
    try:
        artifact_stat = executable.stat() if executable is not None else None
        artifact_identity = (None if artifact_stat is None else
                             {"device": artifact_stat.st_dev, "inode": artifact_stat.st_ino, "size": artifact_stat.st_size})
        artifact_failure = "unavailable:no-argv-path" if executable is None else None
    except OSError as error:
        artifact_identity = None
        artifact_failure = f"unsupported:{type(error).__name__}"
    base = Path(proc_root) / str(pid)
    try:
        running_stat = os.stat(base / "exe")
        running_identity = {"device": running_stat.st_dev, "inode": running_stat.st_ino, "size": running_stat.st_size}
    except OSError as error:
        running_identity = f"unsupported:{type(error).__name__}"
    result = {"pid": pid, "artifact_executable_path": str(executable)[:512] if executable is not None else "unavailable:no-argv-path",
              "artifact_executable_identity": artifact_identity if artifact_failure is None else artifact_failure,
              "running_executable_identity": running_identity,
              "process_context": _locald_process_context(pid, base, proc_root)}
    for name in ("stat", "wchan", "syscall", "io"):
        result[name] = _bounded_proc_text(base / name)
    descriptors = []
    try:
        with os.scandir(base / "fd") as entries:
            names = []
            truncated = False
            for entry in entries:
                if len(names) == 16:
                    truncated = True
                    break
                names.append(entry.name)
        names.sort(key=lambda value: int(value) if value.isdigit() else 10**9)
    except OSError as error:
        result["fd_paths"] = f"unsupported:{type(error).__name__}"
    else:
        for name in names:
            try:
                target = os.readlink(base / "fd" / name)
            except OSError as error:
                descriptors.append({"fd": name, "target": f"unsupported:{type(error).__name__}"})
                continue
            try:
                target_stat = os.stat(base / "fd" / name)
                is_executable = (isinstance(running_identity, dict) and
                                 (target_stat.st_dev, target_stat.st_ino) ==
                                 (running_identity["device"], running_identity["inode"]))
            except OSError:
                is_executable = False
            if is_executable:
                label = "executable"
            else:
                label = "filtered"
                try:
                    resolved = Path(target).resolve(strict=False)
                    for kind, allowed_root in allowed_roots:
                        if allowed_root is None:
                            continue
                        try:
                            relative = resolved.relative_to(allowed_root)
                        except ValueError:
                            continue
                        label = f"{kind}:" + str(relative)[:160]
                        break
                except (OSError, RuntimeError):
                    pass
            descriptors.append({"fd": name, "target": label})
        result["fd_paths"] = descriptors
        result["fd_paths_truncated"] = truncated
    return json.dumps(result, sort_keys=True, separators=(",", ":"))


def _endpoint_start_error(pid, return_code, waited, endpoint_present, checkpoint, tail, process_diagnostic=None):
    state = "running" if return_code is None else "exited"
    exit_code = "none" if return_code is None else str(return_code)
    endpoint = "present" if endpoint_present else "missing"
    diagnostic = "" if process_diagnostic is None else f", process_diagnostic={process_diagnostic}"
    return RuntimeError(
        "backend-locald did not open its endpoint "
        f"(pid={pid}, state={state}, exit_code={exit_code}, "
        f"waited={waited:.3f}s, endpoint={endpoint}, startup_checkpoint={checkpoint}{diagnostic}):\n{tail}"
    )


def _wait_for_endpoint(endpoint, process, workspace, tail, *, monotonic=time.monotonic, sleep=time.sleep):
    started = monotonic()
    deadline = started + LOCALD_ENDPOINT_START_TIMEOUT_SECONDS
    diagnostic = None
    while not endpoint.exists():
        return_code = process.poll()
        if return_code is not None:
            now = monotonic()
            raise _endpoint_start_error(
                process.pid, return_code, now - started, endpoint.exists(),
                _startup_checkpoint(workspace, process.pid), tail(), diagnostic
            )
        now = monotonic()
        if diagnostic is None and now - started >= LOCALD_ENDPOINT_DIAGNOSTIC_SECONDS:
            # Observe only this owned child, once, and only while it is still live.
            if process.poll() is None:
                process_args = getattr(process, "args", None)
                executable = process_args[0] if isinstance(process_args, (list, tuple)) and process_args else None
                diagnostic = _locald_process_diagnostic(
                    process.pid, endpoint.parent, workspace, executable
                )
        if now > deadline:
            raise _endpoint_start_error(
                process.pid, None, now - started, endpoint.exists(),
                _startup_checkpoint(workspace, process.pid), tail(), diagnostic
            )
        sleep(LOCALD_ENDPOINT_START_POLL_SECONDS)


OUTPUT_LOCK = threading.Lock()
CI_SIGTERM = threading.Event()
CI_SIGTERM_AT = None
CURRENT_CASE = ContextVar("surface_case", default=None)
RUN_PROVENANCE = ContextVar("run_provenance", default=None)
CASE_DIAGNOSTICS = ContextVar("case_diagnostics", default=None)


def log(message: str) -> None:
    case = CURRENT_CASE.get()
    prefix = f"[{case}] " if case else ""
    if CI_SIGTERM.is_set():
        print(f"surface: {prefix}{message}", flush=True)
        return
    with OUTPUT_LOCK:
        print(f"surface: {prefix}{message}", flush=True)


def emit_performance(name: str, performance: dict) -> None:
    # Successful Concourse containers can expire after the next no-op poll.
    # Keep exact metrics in the retained event log as well as the report file.
    line = "SURFACE-PERFORMANCE " + json.dumps(
        {"schema": 1, "case": name, "performance": performance}, sort_keys=True)
    if CI_SIGTERM.is_set():
        print(line, flush=True)
        return
    with OUTPUT_LOCK:
        print(line, flush=True)


@contextmanager
def _context_value(variable: ContextVar, value):
    token = variable.set(value)
    try:
        yield
    finally:
        variable.reset(token)


def _validated_encoding(value: object) -> str:
    if not isinstance(value, str) or len(value.encode("utf-8")) > 65536:
        raise ValueError
    return value


def _write_diagnostic(stream, encoded: str) -> None:
    with OUTPUT_LOCK:
        print("CI-PERF-CONTEXT " + encoded, file=stream, flush=True)


def _emit_diagnostics(record: dict) -> None:
    encoded, failure = perf_context.observe(perf_context.encode, record)
    if failure is None:
        encoded, failure = perf_context.observe(_validated_encoding, encoded)
    if failure is not None:
        record["encoding"] = failure
        encoded, failure = perf_context.observe(perf_context.encode_identity, record)
        if failure is not None:
            return

    failure = perf_context.observe(_write_diagnostic, sys.stdout, encoded)[1]
    if failure is None:
        return
    record["output"] = failure
    fallback, failure = perf_context.observe(perf_context.encode_identity, record)
    if failure is None:
        perf_context.observe(_write_diagnostic, sys.stderr, fallback)


def _record_owner_threads(diagnostics: dict | None, pid: int) -> None:
    if diagnostics is None or sys.exc_info()[0] is not None:
        return
    value, failure = perf_context.observe(perf_context.owner_threads, pid)
    diagnostics["owner_threads_end"] = failure if failure is not None else value


def _start_diagnostics(provenance: dict, case_name: str) -> dict:
    diagnostics = {"schema": 1, **provenance, "case": case_name}
    value, failure = perf_context.observe(perf_context.system)
    diagnostics["start"] = failure if failure is not None else value
    return diagnostics


def _finish_diagnostics(diagnostics: dict) -> None:
    value, failure = perf_context.observe(perf_context.system)
    diagnostics["end"] = failure if failure is not None else value
    _emit_diagnostics(diagnostics)


def _preparation_diagnostic_opt_in(provenance: dict | None, value: str | None = None) -> bool:
    """Enable diagnostic worker receipts only for an explicit CI request."""
    if value is None:
        value = os.environ.get(SEARCH_PREPARATION_DIAGNOSTICS_ENV)
    return (provenance is not None and isinstance(value, str)
            and value.casefold() in {"1", "true"})


def _diagnostic_hex(value: object, length: int = 64) -> bool:
    return isinstance(value, str) and re.fullmatch(rf"[0-9a-f]{{{length}}}", value) is not None


def _diagnostic_phase(value: object) -> dict | None:
    if not isinstance(value, dict):
        return None
    if value.get("state") == "not-run":
        return {"state": "not-run"}
    elapsed = value.get("elapsed_ns")
    if (value.get("state") != "observed" or type(elapsed) is not int
            or not 0 <= elapsed <= 86_400_000_000_000):
        return None
    return {"state": "observed", "elapsed_ns": elapsed}


def _diagnostic_selection(value: object) -> dict | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        return None
    result = {}
    for key in ("row_evidence_join_ns", "relation_state_ns", "document_state_ns",
                "projection_open_advance_ns"):
        item = value.get(key)
        if item is not None and (type(item) is not int or not 0 <= item <= 86_400_000_000_000):
            return None
        result[key] = item
    for key in ("view_rows", "evidence_facts", "lexical_documents", "selected_entities"):
        item = value.get(key)
        if item is not None and (type(item) is not int or not 0 <= item <= 1_000_000_000):
            return None
        result[key] = item
    for key in ("same_view", "same_workspace", "same_relation_root", "same_binding"):
        item = value.get(key)
        if item is not None and type(item) is not bool:
            return None
        result[key] = item
    maintenance = value.get("maintenance")
    if maintenance is not None and maintenance not in (
            "reused", "restored", "rebound", "revised", "rebuilt", "refused"):
        return None
    result["maintenance"] = maintenance
    return result


def _search_answer_identities(content: dict) -> dict | None:
    """Collect only identity fields the public answer explicitly exposes."""
    found = {}

    def add(target: str, value: object) -> bool:
        if not _diagnostic_hex(value) and not (target == "source.schema" and type(value) is int
                                                and 0 <= value <= 65535):
            return False
        if target in found and found[target] != value:
            return False
        found[target] = value
        return True

    for field in ("workspace", "view", "view_version"):
        if field in content and not add(field, content[field]):
            return None
    source_fields = ("root", "object", "branch", "log", "schema")
    for container in (content.get("basis"), content.get("source_basis")):
        if isinstance(container, dict):
            source = container.get("source", container)
            for field in ("workspace", "view", "view_version"):
                if field in container and not add(field, container[field]):
                    return None
            if isinstance(source, dict):
                for field in source_fields:
                    if field in source and not add(f"source.{field}", source[field]):
                        return None
    for container in (content, content.get("page")):
        if isinstance(container, dict):
            for external, internal in (("source_root", "root"), ("source_object", "object"),
                                       ("branch", "branch"), ("log", "log"), ("schema", "schema")):
                if external in container and not add(f"source.{internal}", container[external]):
                    return None
    return found


def capture_search_preparation_diagnostic(owner, provenance: dict, answer_content: dict) -> dict:
    """Read one current owner's bounded receipt without making it an authority input."""
    def unavailable(reason: str) -> dict:
        return {"state": "unavailable", "reason": reason}

    try:
        if not isinstance(answer_content, dict):
            return unavailable("search-answer-identity-invalid")
        pid = owner.process.pid
        if type(pid) is not int or pid <= 0 or owner.process.poll() is not None:
            return unavailable("owner-not-live")
        if (not _diagnostic_hex(provenance.get("source_sha"), 40)
                or not _diagnostic_hex(provenance.get("artifact_manifest_sha256"))):
            return unavailable("ci-provenance-invalid")
        directory = Path(owner.workspace)
        directory_stat = directory.lstat()
        if (not stat.S_ISDIR(directory_stat.st_mode) or directory_stat.st_uid != os.getuid()
                or stat.S_IMODE(directory_stat.st_mode) & 0o077):
            return unavailable("owner-workspace-not-private")
        path = directory / SEARCH_PREPARATION_RECEIPT
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid():
            return unavailable("receipt-not-owned-regular-file")
        if before.st_size > SEARCH_PREPARATION_RECEIPT_LIMIT:
            return unavailable("receipt-over-limit")
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            if (not stat.S_ISREG(opened.st_mode) or opened.st_uid != os.getuid()
                    or (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
                    or opened.st_size > SEARCH_PREPARATION_RECEIPT_LIMIT):
                return unavailable("receipt-changed-before-read")
            raw = stream.read(SEARCH_PREPARATION_RECEIPT_LIMIT + 1)
            finished = os.fstat(stream.fileno())
        after = path.lstat()
        if (len(raw) > SEARCH_PREPARATION_RECEIPT_LIMIT or len(raw) != opened.st_size
                or (finished.st_dev, finished.st_ino, finished.st_size, finished.st_mtime_ns)
                != (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns)
                or (after.st_dev, after.st_ino) != (opened.st_dev, opened.st_ino)):
            return unavailable("receipt-changed-during-read")
        if owner.process.poll() is not None:
            return unavailable("owner-retired-during-read")
        receipt = json.loads(raw)
        if (not isinstance(receipt, dict)
                or receipt.get("schema") != "nudox.search-preparation-worker.v1"
                or type(receipt.get("process_id")) is not int
                or receipt.get("process_id") != pid
                or receipt.get("authority") != "worker-observation-only"):
            return unavailable("receipt-owner-or-schema-mismatch")
        answer_identities = _search_answer_identities(answer_content)
        if answer_identities is None:
            return unavailable("search-answer-identity-invalid")
        if any(not _diagnostic_hex(receipt.get(key)) for key in ("workspace", "view", "view_version")):
            return unavailable("receipt-workspace-identity-invalid")
        source = receipt.get("source")
        if (not isinstance(source, dict)
                or any(not _diagnostic_hex(source.get(key)) for key in ("root", "object", "branch", "log"))
                or type(source.get("schema")) is not int or not 0 <= source["schema"] <= 65535):
            return unavailable("receipt-source-identity-invalid")
        worker_identities = {
            "workspace": receipt["workspace"],
            "view": receipt["view"],
            "view_version": receipt["view_version"],
            **{f"source.{key}": source[key] for key in ("root", "object", "branch", "log", "schema")},
        }
        if any(worker_identities[key] != value for key, value in answer_identities.items()):
            return unavailable("search-answer-identity-mismatch")
        outcome = receipt.get("outcome")
        outcomes = {"prepared", "capture-refused", "projection-refused", "cancelled", "panicked", "stopped"}
        elapsed = receipt.get("elapsed_ns")
        if (outcome not in outcomes or type(elapsed) is not int
                or not 0 <= elapsed <= 86_400_000_000_000):
            return unavailable("receipt-outcome-invalid")
        phases = receipt.get("phases")
        phase_names = ("indexed_sources", "semantic_corpus", "lexical_projection")
        if not isinstance(phases, dict):
            return unavailable("receipt-phases-invalid")
        bounded_phases = {key: _diagnostic_phase(phases.get(key)) for key in phase_names}
        if any(value is None for value in bounded_phases.values()):
            return unavailable("receipt-phases-invalid")
        selection = _diagnostic_selection(receipt.get("selection"))
        if receipt.get("selection") is not None and selection is None:
            return unavailable("receipt-selection-invalid")
        return {
            "state": "captured",
            "source_sha": provenance["source_sha"],
            "artifact_manifest_sha256": provenance["artifact_manifest_sha256"],
            "receipt_sha256": hashlib.sha256(raw).hexdigest(),
            "owner_pid": pid,
            "workspace": receipt["workspace"],
            "view": receipt["view"],
            "view_version": receipt["view_version"],
            "source": {key: source[key] for key in ("root", "object", "branch", "log", "schema")},
            "binding_level": "owner-process-private-workspace",
            "verified_answer_fields": sorted(answer_identities),
            "unbound_worker_fields": sorted(set(worker_identities) - set(answer_identities)),
            "outcome": outcome,
            "elapsed_ns": elapsed,
            "phases": bounded_phases,
            "selection": selection,
        }
    except FileNotFoundError:
        return unavailable("receipt-not-found")
    except (OSError, ValueError, TypeError, json.JSONDecodeError):
        return unavailable("receipt-read-or-decode-failed")


def emit_search_preparation_diagnostic(diagnostic: dict, case_name: str) -> None:
    try:
        packet = {"schema": 1, "case": case_name, **diagnostic}
        encoded = json.dumps(packet, sort_keys=True, separators=(",", ":"))
        if len(encoded.encode("utf-8")) <= SEARCH_PREPARATION_DIAGNOSTIC_LIMIT:
            with OUTPUT_LOCK:
                print("SURFACE-SEARCH-PREPARATION " + encoded, flush=True)
    except (OSError, TypeError, ValueError):
        # Observation-only evidence must never replace the product verdict.
        pass


def stable(value):
    """Drops run-dependent fields so snapshots compare behaviour only."""
    if isinstance(value, dict):
        return {key: stable(item) for key, item in sorted(value.items()) if key not in VOLATILE_KEYS}
    if isinstance(value, list):
        return [stable(item) for item in value]
    return value


def _owner_usage_diagnostics_enabled(value: str | None = None) -> bool:
    if value is None:
        value = os.environ.get(OWNER_USAGE_DIAGNOSTICS_ENV)
    provenance = RUN_PROVENANCE.get()
    return (isinstance(provenance, dict)
            and _diagnostic_hex(provenance.get("source_sha"), 40)
            and _diagnostic_hex(provenance.get("artifact_manifest_sha256"))
            and value == "1")


def _owner_start_ticks(stat_text: str) -> int:
    # /proc/<pid>/stat fields after the final ')' begin with field 3 (state);
    # starttime is field 22, hence offset 19 in this tail.
    fields = stat_text.rsplit(")", 1)[1].split()
    return int(fields[19])


def _emit_owner_usage_diagnostic(case: str | None, event: str, **fields) -> None:
    provenance = RUN_PROVENANCE.get()
    if not _owner_usage_diagnostics_enabled():
        return
    # Diagnostics are observational. Encoding or output failures must never
    # replace the product verdict, and a malformed field cannot create an
    # unbounded CI event.
    try:
        packet = {**provenance, "case": case, "event": event,
                  "timestamp_ns": time.time_ns(), **fields}
        encoded = json.dumps(packet, sort_keys=True)
        if len(encoded.encode("utf-8")) > OWNER_USAGE_DIAGNOSTIC_LIMIT:
            encoded = json.dumps({
                **provenance, "case": case, "event": event,
                "error": "owner usage diagnostic exceeded its bound",
            }, sort_keys=True)
        if len(encoded.encode("utf-8")) > OWNER_USAGE_DIAGNOSTIC_LIMIT:
            return
        with OUTPUT_LOCK:
            print("CI-OWNER-USAGE " + encoded, flush=True)
    except Exception:
        return


class Owner:
    """One backend-locald for one case, started and stopped by the harness."""

    def __init__(self, bin_dir: Path, case_root: Path, cargo_vendor: Path | None = None):
        process_custody.activate()
        self.bin_dir = bin_dir
        # Keep public project/endpoint coordinates stable, but place durable
        # owner writes on the task volume, not its rootfs /tmp overlay. The
        # same file/directory durability barriers still run on this state.
        state_parent = ROOT / ".local"
        state_parent.mkdir(mode=0o700, exist_ok=True)
        self.state_directory = tempfile.TemporaryDirectory(prefix="surface-owner-", dir=state_parent)
        state = Path(self.state_directory.name)
        self.home = state / "home"
        self.workspace = state / "workspace"
        self.endpoint = case_root / "locald.sock"
        self.env = isolated_env(self.home, bin_dir)
        if _preparation_diagnostic_opt_in(RUN_PROVENANCE.get()):
            # isolated_env strips inherited BACKEND_* overrides; this one is
            # explicitly reintroduced only for a provenance-bound CI report.
            self.env["BACKEND_JOURNEY_SEARCH_PREPARATION"] = "1"
        if cargo_vendor is not None:
            # Pinned lockfile sources are outside the upstream project. Keep
            # the owner offline and give this case its own writable Cargo home.
            cargo_home = case_root / "cargo-home"
            cargo_home.mkdir(mode=0o700)
            (cargo_home / "config.toml").write_text(
                '[source.crates-io]\nreplace-with = "nudox-surface-vendor"\n'
                '[source.nudox-surface-vendor]\ndirectory = '
                + json.dumps(str(cargo_vendor)) + '\n[net]\noffline = true\n')
            self.env["NUDOX_CARGO_HOME"] = str(cargo_home)

        self.workspace.mkdir(parents=True, mode=0o700)
        authority = self.workspace / "authority.secret"
        self.env["BACKEND_LOCALD_WORKSPACE"] = str(self.workspace)
        self.env["BACKEND_LOCALD_AUTHORITY_SECRET_FILE"] = str(authority)
        authority.write_bytes(bytes([0x5A]) * 32)
        authority.chmod(0o600)
        self.log = open(case_root / "locald.log", "w")
        self.process = None
        try:
            self.process = process_custody.popen(
                [
                    bin_dir / "backend-locald",
                    "--endpoint", str(self.endpoint),
                    "--workspace", str(self.workspace),
                    "--profile", "builtin",
                    "--authority-secret-file", str(authority),
                    "--max-frame", "1048576",
                    "--timeout-ms", "180000",
                    "--registry-offline",
                    "--registry-discovery-offline",
                    "--advisory-offline",
                    "--forge-offline",
                    "--idle-timeout-ms", "5000",
                ],
                env=self.env,
                cwd=case_root,
                stdin=subprocess.DEVNULL,
                stdout=self.log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            _wait_for_endpoint(self.endpoint, self.process, self.workspace, self.tail)
        except BaseException as error:
            # A constructor failure never reaches run_case's finally block.
            # Reap this owner before removing its state or starting a peer.
            try:
                self.stop()
            except Exception as cleanup_error:
                raise RuntimeError(f"{error}\nowner cleanup failed: {cleanup_error}") from error
            raise

    def tail(self) -> str:
        self.log.flush()
        return Path(self.log.name).read_text(errors="replace")[-3000:]

    def usage(self) -> dict:
        """Peak RSS (VmHWM) and CPU time of the owner so far, from /proc."""
        raw = self.usage_raw()
        return {"peak_rss_kib": raw["peak_rss_kib"], "cpu_seconds": round(raw["cpu_seconds"], 2)}

    def usage_raw(self) -> dict:
        """Unrounded owner counters; optionally retain same-process sample identity."""
        if self.process.poll() is not None:
            raise RuntimeError("owner exited before its resource sample")
        pid = self.process.pid
        diagnostics = _owner_usage_diagnostics_enabled()
        stat_before = None
        diagnostic_error = None
        if diagnostics:
            try:
                stat_before = Path(f"/proc/{pid}/stat").read_text()
            except Exception as error:
                diagnostic_error = f"stat-before unavailable: {type(error).__name__}"
        status = Path(f"/proc/{pid}/status").read_text()
        stat_after = Path(f"/proc/{pid}/stat").read_text()
        if diagnostics:
            try:
                start_before = _owner_start_ticks(stat_before) if stat_before is not None else None
                start_after = _owner_start_ticks(stat_after)
                if start_before is not None and start_before != start_after:
                    diagnostic_error = (
                        "owner PID identity changed during usage sample: "
                        f"pid={pid} start_ticks_before={start_before} start_ticks_after={start_after}"
                    )
            except Exception as error:
                start_after = None
                diagnostic_error = f"sample identity unavailable: {type(error).__name__}"
        status_name = re.search(r"^Name:\s+(.+)$", status, re.MULTILINE)
        peak = int(re.search(r"VmHWM:\s+(\d+) kB", status).group(1))
        resident = int(re.search(r"VmRSS:\s+(\d+) kB", status).group(1))
        fields = stat_after.rsplit(")", 1)[1].split()
        if self.process.poll() is not None:
            raise RuntimeError("owner exited during its resource sample")
        ticks = os.sysconf("SC_CLK_TCK")
        cpu = (int(fields[11]) + int(fields[12])) / ticks
        if not math.isfinite(cpu) or cpu < 0:
            raise RuntimeError("owner CPU counter is not finite and nonnegative")
        result = {"peak_rss_kib": peak, "resident_rss_kib": resident, "cpu_seconds": cpu}
        if diagnostics:
            try:
                result.update({
                    "pid": pid,
                    "start_ticks": start_after,
                    "comm": status_name.group(1) if status_name else None,
                    "cpu_user_ticks": int(fields[11]),
                    "cpu_system_ticks": int(fields[12]),
                    "sample_wall_time_ns": time.time_ns(),
                    "sample_monotonic_ns": time.monotonic_ns(),
                })
            except Exception as error:
                diagnostic_error = f"sample detail unavailable: {type(error).__name__}"
            if diagnostic_error is not None:
                result["diagnostic_error"] = diagnostic_error
        return result

    def reset_peak_rss(self) -> dict:
        """Reset this locald's VmHWM and verify Linux accepted the reset."""
        case_name = getattr(self, "case_name", None)
        pid = self.process.pid
        before = None
        if _owner_usage_diagnostics_enabled():
            try:
                before = self.usage_raw()
            except Exception as error:
                _emit_owner_usage_diagnostic(
                    case_name, "clear-refs-before-sample-unavailable",
                    pid=pid, error=type(error).__name__)
        if before is not None:
            _emit_owner_usage_diagnostic(case_name, "clear-refs-write-requested",
                                         pid=pid, start_ticks=before.get("start_ticks"),
                                         before=before, value="5\n")
        written = Path(f"/proc/{pid}/clear_refs").write_text("5\n")
        reset = self.usage_raw()
        _emit_owner_usage_diagnostic(case_name, "clear-refs-write-completed",
                                     pid=pid, start_ticks=reset.get("start_ticks"),
                                     bytes_written=written, before=before, after=reset,
                                     value="5\n")
        if reset["peak_rss_kib"] != reset["resident_rss_kib"]:
            raise RuntimeError(
                "owner VmHWM reset did not take effect: "
                f"peak={reset['peak_rss_kib']} KiB resident={reset['resident_rss_kib']} KiB"
            )
        return reset

    def stop(self) -> None:
        try:
            if self.process is None:
                return
            if self.process.poll() is None:
                log("owner stop: waiting for idle retirement")
                try:
                    self.process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    log("owner stop: idle retirement timed out; sending SIGKILL")
                    process_custody.signal_session(self.process, signal.SIGKILL)
                    reap_started = time.monotonic()
                    log("owner stop: waiting to reap SIGKILLed process")
                    process_custody.retire(self.process, time.monotonic()+5)
                    log(f"owner stop: reaped SIGKILLed process after {time.monotonic() - reap_started:.3f}s")
                    raise RuntimeError("backend-locald failed to retire through its idle timeout")
                else:
                    log("owner stop: idle retirement completed")
            # poll observes exit without reaping the owned session anchor.
            self.process.wait(timeout=5)
            if self.process.returncode != 0:
                raise RuntimeError(f"backend-locald exited {self.process.returncode}")
        finally:
            cleanup_started = time.monotonic()
            log("owner stop: closing log and removing private state")
            self.log.close()
            self.state_directory.cleanup()
            log(f"owner stop: private state cleanup completed after {time.monotonic() - cleanup_started:.3f}s")


def isolated_env(home: Path, bin_dir: Path) -> dict[str, str]:
    env = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(("BACKEND_", "FORGEJO_", "NUDOX_WORKSPACE", "NUDOX_PROJECT"))
        and name not in {
            "NIX_BUILDER_KEY", SEARCH_PREPARATION_DIAGNOSTICS_ENV,
            OWNER_USAGE_DIAGNOSTICS_ENV,
        }
    }
    for name, sub in (
        ("HOME", ""),
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
        ("XDG_RUNTIME_DIR", "run"),
    ):
        path = home / sub
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        env[name] = str(path)
    # T2 verifies the linked native TSZ producer. The shared corpus shell also
    # exports standalone Compiler API tools; inheriting one would explicitly
    # select that different producer and suppress native project coverage.
    for name in ("NUDOX_TSC", "NUDOX_TYPESCRIPT_NODE", "NUDOX_TYPESCRIPT_MODULE_ROOT",
                 "NUDOX_TYPESCRIPT_REPORT_PROGRAM"):
        env.pop(name, None)
    env["BACKEND_LOCALD_BIN"] = str(bin_dir / "backend-locald")
    env["NO_COLOR"] = "1"
    return env


def run_cli(owner: Owner, project: Path, args: list[str], timeout: float) -> tuple[dict, float]:
    operation = f"cli {args[0]}" if args else "cli"
    before_log = time.monotonic()
    log(f"{operation}: started (timeout={timeout:g}s)")
    started = time.monotonic()
    outcome = "completed"
    try:
        result = process_custody.run(
            [owner.bin_dir / "backend-cli", "--endpoint", str(owner.endpoint), "--project", str(project), "--json", *args],
            env=owner.env,
            cwd=project,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        outcome = "timed out"
        raise
    except BaseException:
        outcome = "raised"
        raise
    finally:
        finished = time.monotonic()
        seconds = finished - started
        log(f"{operation}: {outcome} after {seconds:.3f}s")
        # Full continuation evidence shares the output lock. Its lock/flush
        # waits are runner work, outside the shipped CLI's measured lifetime.
        # Keep both intervals in CI diagnostics without changing any budget.
        try:
            diagnostics = CASE_DIAGNOSTICS.get()
            if args and args[0] == "add" and isinstance(diagnostics, dict):
                after_log = time.monotonic()
                diagnostics["cli_add_timing"] = {
                    "outcome": outcome,
                    "subprocess_seconds": seconds,
                    "start_log_seconds": started - before_log,
                    "completion_log_seconds": after_log - finished,
                    "old_log_inclusive_seconds": after_log - before_log,
                }
        except Exception:
            pass  # Observational retention cannot replace the CLI result.
    try:
        body = json.loads(result.stdout) if result.stdout.strip() else None
    except json.JSONDecodeError:
        body = {"unparsed_stdout": result.stdout[-4000:]}
    answer = {"exit": result.returncode, "json": body}
    if result.returncode != 0:
        answer["stderr"] = result.stderr.strip()[-2000:]
    return answer, seconds


def run_mcp(owner: Owner, project: Path, calls: list[dict], timeout: float) -> tuple[dict, float]:
    requests = [
        {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "nudox-t2-surface", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
    ]
    for index, call in enumerate(calls, start=2):
        requests.append({"jsonrpc": "2.0", "id": index, "method": "tools/call",
                         "params": {"name": call["tool"], "arguments": call.get("arguments", {})}})
    started = time.monotonic()
    result = process_custody.run(
        [owner.bin_dir / "backend-mcp", "--endpoint", str(owner.endpoint), "--project", str(project)],
        env=owner.env,
        cwd=project,
        input="".join(json.dumps(request) + "\n" for request in requests),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    seconds = time.monotonic() - started
    replies = {}
    for line in result.stdout.splitlines():
        if line.strip():
            message = json.loads(line)
            if "id" in message:
                replies[message["id"]] = message
    tools = sorted(tool["name"] for tool in replies.get(1, {}).get("result", {}).get("tools", []))
    answers = []
    for index, call in enumerate(calls, start=2):
        reply = replies.get(index, {})
        answers.append({"call": call, "reply": reply.get("result", reply.get("error"))})
    return {"tools": tools, "calls": answers, "exit": result.returncode}, seconds


def run_mcp_interactive(owner: Owner, project: Path, calls: list[dict], timeout: float, on_reply,
                       absolute_deadline: float | None = None) -> tuple[dict, float]:
    """Keep session-bound outline authority while dynamically consuming its token."""
    import selectors
    import tempfile

    started = time.monotonic()
    deadline = absolute_deadline if absolute_deadline is not None else started + timeout
    log(f"MCP interactive: started ({len(calls)} calls; timeout={timeout:g}s)")
    outcome = "completed"
    answers = []
    pending = b""
    continuation_id = 10000
    with tempfile.TemporaryFile() as errors:
        process = process_custody.popen(
            [owner.bin_dir / "backend-mcp", "--endpoint", str(owner.endpoint), "--project", str(project)],
            env=owner.env, cwd=project, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors,
        )
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        try:
            def send(request):
                process.stdin.write((json.dumps(request) + "\n").encode())
                process.stdin.flush()

            def receive(identifier, per_call):
                nonlocal pending
                until = min(deadline, time.monotonic() + per_call)
                while True:
                    if time.monotonic() >= until:
                        raise RuntimeError("interactive MCP: response deadline exceeded")
                    if b"\n" not in pending:
                        if not selector.select(max(0.0, until - time.monotonic())):
                            raise RuntimeError("interactive MCP: response deadline exceeded")
                        chunk = os.read(process.stdout.fileno(), 65536)
                        if not chunk:
                            raise RuntimeError("interactive MCP: closed before its response")
                        pending += chunk
                        if len(pending) > 1048576:
                            raise RuntimeError("interactive MCP: oversized response")
                        continue
                    line, pending = pending.split(b"\n", 1)
                    if not line.strip():
                        continue
                    message = json.loads(line)
                    if "id" not in message:
                        continue
                    if message["id"] != identifier:
                        raise RuntimeError("interactive MCP: unexpected response identity")
                    reply = message.get("result", message.get("error"))
                    if isinstance(reply, dict) and "backend/wireBudget" in reply.get("_meta", {}):
                        require_mcp_wire_budget(reply, len(line) + 1)
                    return reply

            send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "nudox-t2-surface", "version": "1"}}})
            receive(0, timeout)
            send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            send({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
            tools = sorted(tool["name"] for tool in receive(1, timeout).get("tools", []))

            def exchange(call, identifier, per_call):
                send({"jsonrpc": "2.0", "id": identifier, "method": "tools/call",
                      "params": {"name": call["tool"], "arguments": call.get("arguments", {})}})
                return receive(identifier, per_call)

            def continue_call(call, per_call=10):
                nonlocal continuation_id
                continuation_id += 1
                return exchange(call, continuation_id, per_call)

            continue_call.deadline = deadline

            def readiness_call(call, per_call):
                nonlocal continuation_id
                continuation_id += 1
                return exchange(call, continuation_id, per_call)

            readiness_call.remaining = lambda: max(0.0, deadline - time.monotonic())
            readiness_call.deadline = deadline

            for identifier, call in enumerate(calls, start=2):
                reply = exchange(call, identifier, timeout)
                answers.append({"call": call, "reply": reply})
                replacement = on_reply(call, reply, continue_call, readiness_call)
                if replacement is not None:
                    answers[-1]["reply"] = replacement
            process.stdin.close()
            process.wait(timeout=max(0.01, deadline - time.monotonic()))
            return {"tools": tools, "calls": answers, "exit": process.returncode}, time.monotonic() - started
        except BaseException:
            outcome = "raised"
            raise
        finally:
            selector.close()
            # Match process_custody.run(timeout=...) cleanup for this owned client.
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)  # Also retire descendants after root-first exit.
            process.stdout.close()
            if not process.stdin.closed:
                process.stdin.close()
            log(f"MCP interactive: {outcome} after {time.monotonic() - started:.3f}s")


def require_cli_success(name: str, answer: dict) -> None:
    if answer["exit"] != 0 or not isinstance(answer["json"], dict) or answer["json"].get("answer") == "fault":
        raise RuntimeError(f"{name} failed: {answer}")


def require_completed_index(answer: dict, profile_count: int) -> str:
    """Exit zero alone admits failed terminal operations; require native completion."""
    require_cli_success("cli add", answer)
    observation = answer["json"].get("index_operation", {})
    detail = observation.get("detail", {})
    terminal = detail.get("state", {})
    profiles = detail.get("source_capture", {}).get("profiles", [])
    if observation.get("state") != "known" or terminal.get("state") != "published":
        raise RuntimeError(f"index setup: expected a published operation receipt; observation={observation.get('state')}, terminal={terminal}")
    if len(profiles) != profile_count or any(
        p.get("state", {}).get("state") != "published"
        or p.get("state", {}).get("detail", {}).get("coverage") != {"coverage": "complete"}
        for p in profiles
    ):
        raise RuntimeError("index setup: every fixture profile must publish complete native coverage; profiles=" + json.dumps(profiles, sort_keys=True)[:4096])
    root = terminal.get("detail", {}).get("view_root")
    if not isinstance(root, str) or not re.fullmatch(r"[0-9a-f]{64}", root):
        raise RuntimeError("index setup: missing exact published view root")
    return root


def require_selected_cargo_target(answer: dict, target: str) -> None:
    """A scoped success must disclose the exact retained semantic denominator."""
    detail = answer["json"].get("index_operation", {}).get("detail", {})
    capture = detail.get("source_capture", {})
    scope = capture.get("cargo_target") or {}
    profiles = capture.get("profiles", [])
    if detail.get("cargo_target") != target or scope.get("name") != target:
        raise RuntimeError("selected Cargo target: request and captured target disagree")
    if scope.get("root") != "src/lib.rs" or scope.get("kinds") != ["lib"]:
        raise RuntimeError("selected Cargo target: Ryu library source root/kind disagree")
    count = scope.get("source_count")
    digest = scope.get("source_closure")
    if not isinstance(count, int) or count <= 0 or len(profiles) != 1 or profiles[0].get("source_count") != count:
        raise RuntimeError("selected Cargo target: coverage denominator disagrees with closure")
    if not isinstance(digest, list) or len(digest) != 32 or any(type(byte) is not int or not 0 <= byte <= 255 for byte in digest) or not any(digest):
        raise RuntimeError("selected Cargo target: missing exact source closure digest")


def require_index_identity(answer: dict, project: Path, expected: dict | None = None) -> dict:
    """Bind fresh-client observations to the exact admitted request, never just exit zero."""
    observation = (answer.get("json") or {}).get("index_operation", {})
    detail = observation.get("detail", {})
    if observation.get("state") != "known" or not isinstance(detail, dict):
        raise RuntimeError("index resumption: operation must be known")
    for field in ("operation_key", "request_digest"):
        value = detail.get(field)
        if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value) or value == "0" * 64:
            raise RuntimeError(f"index resumption: missing exact {field}")
    if detail.get("package") != {"kind": "local", "value": str(project)} or detail.get("execution_intent") != "background":
        raise RuntimeError("index resumption: wrong package or execution intent")
    if detail.get("state", {}).get("state") not in ("accepted", "active", "published"):
        raise RuntimeError("index resumption: missing, failed or uncertain operation is not success")
    if expected is not None:
        for field in ("operation_key", "request_digest", "package", "execution_intent"):
            if detail.get(field) != expected.get(field):
                raise RuntimeError(f"index resumption: changed {field}")
    return detail


def require_index_wait_stop(answer: dict, project: Path) -> dict:
    if answer.get("exit") != 75:
        raise RuntimeError(f"zero-wait index: expected incomplete-wait exit 75, got {answer}")
    detail = require_index_identity(answer, project)
    if detail["state"]["state"] not in ("accepted", "active"):
        raise RuntimeError("zero-wait index: fixture must still be pending when the first caller leaves")
    return detail


def require_same_index_receipt(answer: dict, project: Path, expected: dict, profile_count: int) -> None:
    require_completed_index(answer, profile_count)
    detail = require_index_identity(answer, project, expected)
    if detail != expected:
        raise RuntimeError("index resumption: terminal receipt or source capture changed")


def require_mcp_index_receipt(reply: dict, project: Path, expected: dict, profile_count: int) -> None:
    if not isinstance(reply, dict) or reply.get("isError", False):
        raise RuntimeError(f"MCP index lookup: expected successful tool result, got {reply}")
    body = reply.get("structuredContent", {})
    projection = body.get("surface", {})
    if projection.get("result") != "index-operation-status" or body.get("fault") is not None:
        raise RuntimeError(f"MCP index lookup: wrong typed surface projection: {body}")
    require_same_index_receipt({"exit": 0, "json": {"index_operation": projection.get("data")}},
                               project, expected, profile_count)


def run_mcp_wait_refusals(owner: Owner, project: Path, terminal: dict, profile_count: int) -> dict:
    # Each invalid call runs through the shipped MCP process, never a private parser.
    times = {}
    for field, value in (("wait", True), ("wait_timeout", 30), ("wait-timeout", 30)):
        session, seconds = run_mcp(owner, project,
            [{"tool": "backend.index", "arguments": {"path": str(project), field: value}},
             {"tool": "backend.index_progress", "arguments": {"operation_key": terminal["operation_key"]}}], 10)
        if session["exit"] != 0 or len(session["calls"]) != 2:
            raise RuntimeError(f"MCP {field} refusal: session failed")
        reply = session["calls"][0]["reply"]
        fault = reply.get("structuredContent") if isinstance(reply, dict) else None
        if (not isinstance(reply, dict) or reply.get("isError") is not True
            or not isinstance(fault, dict) or fault.get("answer") != "fault"
            or fault.get("slug") != "usage" or fault.get("cause") != "malformed"
            or fault.get("operand") != field
            or f"takes no argument called `{field}`" not in str(fault.get("detail", ""))):
            raise RuntimeError(f"MCP {field}: expected exact unknown-argument usage fault, got {reply}")
        # A valid call in the same session must still observe the original receipt.
        require_mcp_index_receipt(session["calls"][1]["reply"], project, terminal, profile_count)
        times[f"MCP refused CLI-only {field}"] = round(seconds, 3)
    log("MCP: three CLI-only wait spellings refused; same-session progress preserves complete receipt")
    return times


def run_resumable_index(owner: Owner, project: Path, profile_count: int, timeout: float) -> tuple[dict, dict]:
    started = time.monotonic()
    # Exercise the installed alias and a caller deadline without terminating the owner.
    first, seconds = run_cli(owner, project,
        ["index", str(project), "--execution-intent", "background", "--wait", "--wait-timeout", "0"], timeout)
    initial = require_index_wait_stop(first, project)
    log(f"background zero-wait: {initial['state']['state']} after {seconds:.3f}s")
    times = {"background index zero-wait": round(seconds, 3)}
    key = initial["operation_key"]
    progress, seconds = run_cli(owner, project, ["index_progress", "--operation-key", key], 10)
    require_cli_success("index progress after caller deadline", progress)
    require_index_identity(progress, project, initial)
    times["index progress after caller deadline"] = round(seconds, 3)
    completed, seconds = run_cli(owner, project,
        ["add", str(project), "--execution-intent", "background", "--wait"], timeout)
    log(f"resumed wait: exit={completed['exit']}; checking complete native publication")
    require_completed_index(completed, profile_count)
    terminal = require_index_identity(completed, project, initial)
    times["resumed index wait"] = round(seconds, 3)
    # Preserve the existing setup budget, now covering start + observation + resumed wait.
    times["cli add"] = round(time.monotonic() - started, 2)
    for name, args in (
        ("completed CLI operation lookup", ["index_progress", "--operation-key", key]),
        ("completed CLI keyed replay", ["index_start", str(project), "--execution-intent", "background", "--operation-key", key]),
    ):
        answer, seconds = run_cli(owner, project, args, 10)
        log(f"{name}: checking terminal receipt")
        require_same_index_receipt(answer, project, terminal, profile_count)
        times[name] = round(seconds, 3)
    session, seconds = run_mcp(owner, project,
        [{"tool": "backend.index_progress", "arguments": {"operation_key": key}}], 10)
    if session["exit"] != 0 or len(session["calls"]) != 1:
        raise RuntimeError("completed MCP operation lookup: session failed")
    reply = session["calls"][0]["reply"]
    log("completed MCP operation lookup: checking typed terminal receipt")
    require_mcp_index_receipt(reply, project, terminal, profile_count)
    times["completed MCP operation lookup"] = round(seconds, 3)
    times.update(run_mcp_wait_refusals(owner, project, terminal, profile_count))
    log("background index: zero-wait caller left; fresh alias/add clients retained one key; explicit keyed replay; CLI/MCP terminal receipts agree")
    return completed, times


def require_live_health(summary: dict, full: dict, witness: dict, published_root: str | None = None) -> None:
    """Whole-owner CLI and MCP status must agree on public identity and coverage."""
    if published_root is not None and (summary.get("readiness") != "ready" or summary.get("revision") != published_root):
        raise RuntimeError("completed index: health must certify the published receipt as ready")
    for name, detail, value in (("summary", "summary", summary), ("full", "full", full), ("MCP", "full", witness)):
        if value.get("answer") != "status" or value.get("detail") != detail:
            raise RuntimeError(f"health {name}: wrong answer/detail: {value}")
        if value.get("availability") != "available" or value.get("publication") != "populated":
            raise RuntimeError(f"health {name}: indexed owner is not available/populated: {value}")
        if not isinstance(value.get("readiness"), str) or not value["readiness"]:
            raise RuntimeError(f"health {name}: missing truthful readiness")
        if not isinstance(value.get("coverage"), list) or not isinstance(value.get("capabilities"), dict):
            raise RuntimeError(f"health {name}: missing public coverage/capabilities")
        if not isinstance(value.get("rows"), int) or value["rows"] <= 0:
            raise RuntimeError(f"health {name}: indexed owner has no visible rows")
        revision = value.get("revision", "")
        if not isinstance(revision, str) or not re.fullmatch(r"[a-f0-9]{64}", revision):
            raise RuntimeError(f"health {name}: revision is not an exact identity")
        if value.get("revision_tag") != revision[:8] or revision != witness.get("revision"):
            raise RuntimeError(f"health {name}: revision {revision} disagrees with MCP status {witness.get('revision')}")
        for field in ("readiness", "coverage", "rows", "publication"):
            if value.get(field) != witness.get(field):
                raise RuntimeError(f"health {name}: {field} disagrees with the same-scope MCP status")
    if "source" in summary or "source_tag" in summary:
        raise RuntimeError("health summary exposed full-only source fields")
    for name, value in (("full", full), ("MCP", witness)):
        source = value.get("source", "")
        if not isinstance(source, str) or not re.fullmatch(r"[a-f0-9]{64}", source):
            raise RuntimeError(f"health {name}: source is not an exact identity")
        if value.get("source_tag") != source[:8] or source != witness.get("source"):
            raise RuntimeError(f"health {name}: source {source} disagrees with MCP status {witness.get('source')}")


def require_bounded_search(reference: dict, bounded: dict, limit: int) -> None:
    if reference.get("answer") != "records" or bounded.get("answer") != "records":
        raise RuntimeError("bounded search: expected records answers")
    rows = reference.get("records")
    if not isinstance(rows, list) or len(rows) < 2 or reference.get("more") is not False:
        raise RuntimeError("bounded search: reference must contain multiple complete results")
    if bounded.get("query") != reference.get("query"):
        raise RuntimeError("bounded search: changed query")
    if bounded.get("records") != rows[:limit]:
        raise RuntimeError(f"bounded search: limit {limit} lost rank order or returned the wrong rows")
    if bounded.get("more") is not (len(rows) > limit):
        raise RuntimeError(f"bounded search: limit {limit} has an untruthful completeness flag")


def walk_search_pages(reference: dict, fetch_page, limit: int = 1) -> tuple[int, str]:
    """Consume opaque cursors through real clients; refuse loss, repetition or drift."""
    expected = reference.get("records")
    if reference.get("more") is not False or not isinstance(expected, list) or len(expected) < 2:
        raise RuntimeError("continuation: expected a complete multi-result reference")
    cursor = None
    first_cursor = None
    seen = set()
    offset = 0
    started = time.monotonic()
    for count in range(1, len(expected) + 2):
        if time.monotonic() - started > 30:
            raise RuntimeError("continuation: exceeded the traversal deadline")
        page = fetch_page(cursor)
        if time.monotonic() - started > 30:
            raise RuntimeError("continuation: exceeded the traversal deadline")
        if page.get("answer") != "records":
            raise RuntimeError("continuation: expected a records page")
        for field in ("query", "coverage", "readiness"):
            if page.get(field) != reference.get(field):
                raise RuntimeError(f"continuation: changed {field} between pages")
        rows = page.get("records")
        if not isinstance(rows, list) or not rows or rows != expected[offset:offset + limit]:
            raise RuntimeError("continuation: missing, duplicated, reordered or unbounded rows")
        offset += len(rows)
        more = offset < len(expected)
        if page.get("more") is not more:
            raise RuntimeError("continuation: incorrect completeness flag")
        token = offered_cursor(page)
        if not more:
            if token is not None:
                raise RuntimeError("continuation: exhausted page still offers a cursor")
            return count, first_cursor
        if not isinstance(token, str) or not token or token in seen:
            raise RuntimeError("continuation: missing or repeated cursor")
        seen.add(token)
        first_cursor = first_cursor or token
        cursor = token
    raise RuntimeError("continuation: did not exhaust the reference")


def require_malformed_cursor_refusal(cli: dict, mcp: dict) -> None:
    fault = cli.get("json")
    if (cli.get("exit") == 0 or not isinstance(fault, dict)
        or fault.get("answer") != "fault" or fault.get("slug") != "protocol"
        or fault.get("cause") != "unproven"
        or str(fault.get("operand", "")).lstrip("-") != "cursor"
        or "continuation token" not in str(fault.get("detail", "")).lower()):
        raise RuntimeError(f"malformed CLI cursor: expected a typed cursor refusal, got {cli}")
    fault = mcp.get("structuredContent") if isinstance(mcp, dict) else None
    if (not isinstance(mcp, dict) or mcp.get("isError") is not True
        or not isinstance(fault, dict) or fault.get("answer") != "fault"
        or fault.get("slug") != "usage" or fault.get("cause") != "malformed"
        or fault.get("operand") != "arguments"
        or "cursor" not in str(fault.get("detail", "")).lower()):
        raise RuntimeError(f"malformed MCP cursor: expected a structured usage refusal, got {mcp}")


def run_live_contracts(owner: Owner, project: Path, config: dict, published_root: str) -> dict:
    times = {}

    def query(name: str, arguments: list[str]) -> dict:
        answer, seconds = run_cli(owner, project, arguments, 10)
        require_cli_success(name, answer)
        times[name] = round(seconds, 3)
        return answer["json"]

    summary = query("live health summary", ["--passive", "health"])
    full = query("live health full", ["--passive", "--detail", "full", "health"])
    mcp, seconds = run_mcp(owner, project, [{"tool": "backend.status", "arguments": {"detail": "full"}}], 10)
    times["live health MCP status"] = round(seconds, 3)
    if mcp["exit"] != 0 or len(mcp["calls"]) != 1:
        raise RuntimeError(f"MCP live status failed: {mcp}")
    reply = mcp["calls"][0]["reply"]
    if not isinstance(reply, dict) or reply.get("isError", False) or "structuredContent" not in reply:
        raise RuntimeError(f"MCP live status failed: {reply}")
    require_live_health(summary, full, reply["structuredContent"], published_root)
    print("surface-contract: live health summary/full agree with MCP status identity and coverage", flush=True)
    text = config["bounded_search_query"]
    reference = query("bounded search reference", ["search", text, "--limit", "200"])
    # The fixture authors two lowercase language methods at these public paths.
    methods = {
        (row.get("identity", {}).get("path"), row.get("language"))
        for row in reference.get("records", [])
        if row.get("identity", {}).get("name") == "language" and row.get("kind") == "method"
    }
    if not {("src/Duke.java", "java"), ("src/main.cpp", "cpp")}.issubset(methods):
        raise RuntimeError(f"bounded search: reference lost authored Java/C++ language methods: {methods}")
    for limit in (1, 3):
        bounded = query(f"bounded search limit {limit}", ["--limit", str(limit), "search", text])
        require_bounded_search(reference, bounded, limit)
        print(f"surface-contract: search limit {limit} preserves rank/completeness", flush=True)

    def cli_page(cursor):
        args = ["--limit", "1", "search", text]
        if cursor is not None:
            args += ["--cursor", cursor]
        answer, _ = run_cli(owner, project, args, 10)
        require_cli_success("CLI search continuation", answer)
        return answer["json"]

    def mcp_page(cursor):
        arguments = {"query": text, "limit": 1}
        if cursor is not None:
            arguments["cursor"] = cursor
        session, _ = run_mcp(owner, project, [{"tool": "backend.search", "arguments": arguments}], 10)
        if session["exit"] != 0 or len(session["calls"]) != 1:
            raise RuntimeError("MCP search continuation: session failed")
        reply = session["calls"][0]["reply"]
        if not isinstance(reply, dict) or reply.get("isError", False) or not isinstance(reply.get("structuredContent"), dict):
            raise RuntimeError(f"MCP search continuation failed: {reply}")
        return reply["structuredContent"]

    for name, fetch_page in (("CLI", cli_page), ("MCP", mcp_page)):
        started = time.monotonic()
        count, _ = walk_search_pages(reference, fetch_page)
        times[f"{name} search continuation"] = round(time.monotonic() - started, 3)
        print(f"surface-contract: {name} search exhausted {count} pages in rank order, no lost/duplicate rows", flush=True)

    refused_cli, seconds = run_cli(owner, project, ["--limit", "1", "search", text, "--cursor", "pc1-not-hex"], 10)
    times["malformed CLI cursor"] = round(seconds, 3)
    refused_mcp, seconds = run_mcp(owner, project, [{"tool": "backend.search", "arguments": {"query": text, "limit": 1, "cursor": "mcp1-invalid"}}], 10)
    times["malformed MCP cursor"] = round(seconds, 3)
    if refused_mcp["exit"] != 0 or len(refused_mcp["calls"]) != 1:
        raise RuntimeError("malformed MCP cursor: session failed instead of refusing invalid parameters")
    require_malformed_cursor_refusal(refused_cli, refused_mcp["calls"][0]["reply"])
    require_live_health(query("health after cursor refusals", ["--passive", "health"]), full, full, published_root)
    print("surface-contract: malformed CLI/MCP cursors refused; owner remains ready at published revision", flush=True)
    return times


def offered_cursor(answer: dict) -> str | None:
    """Read either public cursor representation, rejecting contradictory proofs."""
    direct = answer.get("nextCursor")
    executable = answer.get("continuation_call")
    advertised = None
    if executable is not None:
        if not isinstance(executable, dict) or not isinstance(executable.get("arguments"), dict):
            raise RuntimeError("snapshot continuation: malformed executable call")
        advertised = executable["arguments"].get("cursor")
    if direct is not None and advertised is not None and direct != advertised:
        raise RuntimeError("snapshot continuation: contradictory offered tokens")
    token = direct if direct is not None else advertised
    if token is not None and (not isinstance(token, str) or not token):
        raise RuntimeError("snapshot continuation: invalid offered token")
    return token


def require_initial_cursor_shape(answer: dict) -> str | None:
    """Require the public more flag and cursor presence to agree on first pages."""
    if not isinstance(answer, dict):
        raise RuntimeError("snapshot continuation: malformed initial answer")
    kind = answer.get("answer")
    if kind == "records":
        more = answer.get("more")
    elif kind == "outline":
        page = answer.get("page")
        if not isinstance(page, dict):
            raise RuntimeError("snapshot continuation: malformed initial outline page")
        more = page.get("more")
    else:
        return None
    token = offered_cursor(answer)
    if type(more) is not bool or more != (token is not None):
        raise RuntimeError("snapshot continuation: initial more flag disagrees with its cursor")
    return token


def require_continuation_reply(first: dict, following: dict, published_root: str) -> None:
    """Prove an offered token works through a fresh public client before normalizing it."""
    token = offered_cursor(first)
    if not isinstance(token, str) or not token or following.get("answer") != first.get("answer"):
        raise RuntimeError("snapshot continuation: missing token or changed answer kind")
    if offered_cursor(following) == token:
        raise RuntimeError("snapshot continuation: repeated cursor")
    if first["answer"] == "records":
        if first.get("more") is not True:
            raise RuntimeError("snapshot continuation: token on an exhausted search")
        for field in ("query", "coverage", "readiness", "detail", "semantic_search", "search_scope",
                      "full_search", "limit", "source_root", "source_object", "branch", "log",
                      "schema", "sequence", "revision"):
            if following.get(field) != first.get(field):
                raise RuntimeError(f"snapshot continuation: changed {field}")
        before, after = first.get("records"), following.get("records")
        if not isinstance(before, list) or not before or not isinstance(after, list) or not after:
            raise RuntimeError("snapshot continuation: empty records page")
        old = [_search_record_identity(row) for row in before]
        new = [_search_record_identity(row) for row in after]
        if (len(old) != len(set(old)) or len(new) != len(set(new))
                or set(old) & set(new)):
            raise RuntimeError("snapshot continuation: duplicate emitted search identity across pages")
        more = following.get("more")
    elif first["answer"] == "outline":
        page, next_page = first.get("page", {}), following.get("page", {})
        if following.get("package") != first.get("package"):
            raise RuntimeError("snapshot continuation: changed outline package")
        if following.get("detail") != first.get("detail"):
            raise RuntimeError("snapshot continuation: changed outline detail")
        if page.get("more") is not True:
            raise RuntimeError("snapshot continuation: token on an exhausted outline")
        if page.get("source_root") != published_root or next_page.get("source_root") != published_root:
            raise RuntimeError("snapshot continuation: changed published source root")
        for field in ("branch", "log", "schema", "sequence", "limit", "source_object", "coverage"):
            if field not in page or next_page.get(field) != page[field]:
                raise RuntimeError(f"snapshot continuation: changed outline {field}")
        if not following.get("roots") or following["roots"] == first.get("roots") or following.get("declarations", 0) <= 0:
            raise RuntimeError("snapshot continuation: empty or repeated outline page")
        more = next_page.get("more")
    else:
        raise RuntimeError("snapshot continuation: unsupported answer")
    next_token = offered_cursor(following)
    if not isinstance(more, bool) or (more and (not isinstance(next_token, str) or not next_token)) or (not more and next_token is not None):
        raise RuntimeError("snapshot continuation: dishonest continuation flag")


def _exact_symbol_id(dto: dict) -> str | None:
    """Return the full selected-symbol ID, never its display key/coordinate."""
    proof = dto.get("semantic_data")
    if not isinstance(proof, dict) or proof.get("kind") != "selected-symbol-id":
        return None
    value = proof.get("value")
    if (not isinstance(value, list) or len(value) != 32
            or any(type(byte) is not int or not 0 <= byte <= 255 for byte in value)
            or not any(value)):
        raise RuntimeError("snapshot continuation: malformed selected-symbol ID")
    return bytes(value).hex()


def _search_record_identity(row: dict) -> str:
    """Use the full symbol ID, falling back to the same structural tuple as traversal."""
    identity = row.get("identity") if isinstance(row, dict) else None
    if not isinstance(identity, dict):
        raise RuntimeError("snapshot continuation: search row has no identity DTO")
    exact = _exact_symbol_id(identity)
    if exact is not None:
        return exact
    return "unresolved:" + json.dumps(
        [identity.get("coordinate"), identity.get("key"), identity.get("name"),
         identity.get("path"), identity.get("line"), identity.get("segments"),
         row.get("kind"), row.get("language"), row.get("state")], sort_keys=True,
        separators=(",", ":"), ensure_ascii=False)


def _outline_page_identities(page: dict, previously_seen: set[str]) -> tuple[set[str], int]:
    roots = page.get("roots")
    if not isinstance(roots, list):
        raise RuntimeError("snapshot continuation: malformed outline roots")
    page_ids = set()
    node_count = 0

    def visit(row):
        nonlocal node_count
        if not isinstance(row, dict):
            raise RuntimeError("snapshot continuation: malformed outline row")
        node_count += 1
        identity = _exact_symbol_id(row)
        if identity is None:
            # An unresolved DTO has no exact selector. Its complete public
            # structural tuple is a conservative fallback, never the short key alone.
            identity = "unresolved:" + json.dumps(
                [row.get("coordinate"), row.get("key"), row.get("kind"), row.get("name"),
                 row.get("semantic_data")],
                sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        if identity in page_ids:
            raise RuntimeError("snapshot continuation: duplicate outline identity within a page")
        page_ids.add(identity)
        children = row.get("children", [])
        if not isinstance(children, list):
            raise RuntimeError("snapshot continuation: malformed outline children")
        new_descendants = sum(visit(child) for child in children)
        is_new = identity not in previously_seen
        if not is_new and new_descendants == 0:
            raise RuntimeError("snapshot continuation: duplicate emitted outline identity across pages")
        # A repeated ancestor with new descendants is structural page context;
        # the exact selected-symbol ID keeps it distinct from a repeated row.
        return int(is_new) + new_descendants

    new_count = sum(visit(root) for root in roots)
    declarations = page.get("declarations")
    if type(declarations) is not int or declarations != node_count:
        raise RuntimeError("snapshot continuation: outline declaration count disagrees with its rows")
    if new_count == 0:
        raise RuntimeError("snapshot continuation: outline page made no identity progress")
    return page_ids, new_count


def _emit_continuation_page(evidence: list, transport: str, tool: str,
                            page_index: int, request: dict | list[str],
                            response: dict, input_cursor: str | None,
                            byte_budget: dict | None = None,
                            max_page_bytes: int = MAX_CONTINUATION_RECEIPT_BYTES,
                            max_total_bytes: int = MAX_CONTINUATION_CHAIN_BYTES) -> int:
    """Retain each public page outside goldens without logging opaque cursors."""
    payload = json.loads(json.dumps(response))
    offered = offered_cursor(payload)
    payload.pop("nextCursor", None)
    executable = payload.get("continuation_call")
    if isinstance(executable, dict) and isinstance(executable.get("arguments"), dict):
        executable["arguments"].pop("cursor", None)
    receipt = {
        "transport": transport,
        "tool": tool,
        "page": page_index,
        "request": request,
        "input_cursor_sha256": hashlib.sha256(input_cursor.encode()).hexdigest() if input_cursor else None,
        "offered_cursor_sha256": hashlib.sha256(offered.encode()).hexdigest() if offered else None,
        "response": payload,
    }
    encoded = json.dumps(receipt, sort_keys=True, separators=(",", ":"),
                         ensure_ascii=False).encode("utf-8")
    receipt_bytes = len(encoded)
    if receipt_bytes > max_page_bytes:
        raise RuntimeError("snapshot continuation: complete page receipt exceeded its byte bound")
    if byte_budget is not None:
        retained = byte_budget.get("bytes", 0)
        if retained + receipt_bytes > max_total_bytes:
            raise RuntimeError("snapshot continuation: complete receipt chain exceeded its byte bound")
        byte_budget["bytes"] = retained + receipt_bytes
    evidence.append(receipt)
    with OUTPUT_LOCK:
        print("SURFACE-CONTINUATION-PAGE " + encoded.decode("utf-8"), flush=True)
    return receipt_bytes



def continuation_report_references(evidence: list[dict]) -> list[dict]:
    """Index every full SURFACE-CONTINUATION-PAGE log record by its exact hash.

    DTOs remain in the case record and log. The bounded CI verdict packet must
    not duplicate them: one real-project walk can exceed its entire byte cap.
    """
    references = []
    for receipt in evidence:
        encoded = json.dumps(receipt, sort_keys=True, separators=(",", ":"),
                             ensure_ascii=False).encode("utf-8")
        references.append({
            "transport": receipt["transport"],
            "tool": receipt["tool"],
            "page": receipt["page"],
            "receipt_sha256": hashlib.sha256(encoded).hexdigest(),
            "receipt_bytes": len(encoded),
        })
    return references


def walk_outline_pages(first: dict, fetch_page, published_root: str,
                       max_pages: int = 256, timeout: float = 60.0,
                       deadline: float | None = None, on_page=None) -> tuple[int, int]:
    """Exhaust a real outline cursor chain and reject holes, overlap or basis drift."""
    if max_pages < 1 or timeout <= 0:
        raise ValueError("outline continuation bounds must be positive")
    started = time.monotonic()
    deadline = deadline if deadline is not None else started + timeout
    current = first
    seen_tokens = set()
    seen_rows = set()
    pages = 0
    declarations = 0
    incoming_token = None
    while True:
        if time.monotonic() >= deadline:
            raise RuntimeError("snapshot continuation: outline traversal deadline exceeded")
        pages += 1
        if pages > max_pages:
            raise RuntimeError("snapshot continuation: outline traversal exceeded its page bound")
        page_ids, new_count = _outline_page_identities(current, seen_rows)
        seen_rows.update(page_ids)
        declarations += new_count
        token = offered_cursor(current)
        page_meta = current.get("page", {})
        more = page_meta.get("more") if isinstance(page_meta, dict) else None
        if not isinstance(more, bool) or more != (token is not None):
            raise RuntimeError("snapshot continuation: outline more flag disagrees with its cursor")
        if token is not None and token in seen_tokens:
            raise RuntimeError("snapshot continuation: repeated outline cursor")
        if on_page is not None:
            on_page(pages, current, incoming_token)
        if token is None:
            return pages, declarations
        seen_tokens.add(token)
        following = fetch_page(current, token)
        if time.monotonic() >= deadline:
            raise RuntimeError("snapshot continuation: outline traversal deadline exceeded")
        require_continuation_reply(current, following, published_root)
        incoming_token = token
        current = following


def walk_record_pages(first: dict, fetch_page, max_pages: int = 256,
                      timeout: float = 60.0, deadline: float | None = None,
                      on_page=None) -> tuple[int, int]:
    """Exhaust search cursors and verify stable scope plus a unique row union."""
    if max_pages < 1 or timeout <= 0:
        raise ValueError("search continuation bounds must be positive")
    started = time.monotonic()
    deadline = deadline if deadline is not None else started + timeout
    current = first
    seen_tokens = set()
    seen_rows = set()
    pages = 0
    rows_seen = 0
    reference = first
    effective_call = None
    effective_tool = None
    incoming_token = None
    while True:
        if time.monotonic() >= deadline:
            raise RuntimeError("snapshot continuation: search traversal deadline exceeded")
        pages += 1
        if pages > max_pages:
            raise RuntimeError("snapshot continuation: search traversal exceeded its page bound")
        if on_page is not None:
            on_page(pages, current, incoming_token)
        rows = current.get("records")
        if not isinstance(rows, list) or not rows:
            raise RuntimeError("snapshot continuation: empty search page")
        identities = []
        for row in rows:
            identities.append(_search_record_identity(row))
        if len(identities) != len(set(identities)) or seen_rows.intersection(identities):
            raise RuntimeError("snapshot continuation: duplicate emitted search identity across pages")
        seen_rows.update(identities)
        rows_seen += len(identities)
        token = offered_cursor(current)
        more = current.get("more")
        if not isinstance(more, bool) or more != (token is not None):
            raise RuntimeError("snapshot continuation: search more flag disagrees with its cursor")
        executable = current.get("continuation_call")
        if executable is not None:
            if not isinstance(executable, dict) or not isinstance(executable.get("arguments"), dict):
                raise RuntimeError("snapshot continuation: malformed search continuation operands")
            name = executable.get("name")
            arguments = dict(executable["arguments"])
            arguments.pop("cursor", None)
            if effective_tool is None:
                effective_tool, effective_call = name, arguments
            elif name != effective_tool or arguments != effective_call:
                raise RuntimeError("snapshot continuation: changed effective search tool or operands")
        if token is None:
            return pages, rows_seen
        if token in seen_tokens:
            raise RuntimeError("snapshot continuation: repeated search cursor")
        seen_tokens.add(token)
        following = fetch_page(current, token)
        if time.monotonic() >= deadline:
            raise RuntimeError("snapshot continuation: search traversal deadline exceeded")
        require_continuation_reply(current, following, "")
        for field in ("query", "coverage", "readiness", "detail", "semantic_search", "search_scope",
                      "full_search", "limit", "source_root", "source_object", "branch", "log",
                      "schema", "sequence", "revision"):
            if field in reference or field in following:
                if following.get(field) != reference.get(field):
                    raise RuntimeError(f"snapshot continuation: changed search {field}")
        incoming_token = token
        current = following


def mcp_continuation_call(call: dict, first: dict) -> dict:
    token = offered_cursor(first)
    if token is None:
        raise RuntimeError("snapshot continuation: no offered token")
    executable = first.get("continuation_call")
    if executable is not None:
        if executable.get("name") != call["tool"] or executable["arguments"].get("cursor") != token:
            raise RuntimeError("snapshot continuation: executable call changed tool or token")
        arguments = dict(executable["arguments"])
        if call["tool"] == "backend.outline":
            expected = {"path": first.get("package", {}).get("project"),
                        "limit": first.get("page", {}).get("limit"),
                        "detail": first.get("detail"), "cursor": token}
            if arguments != expected:
                raise RuntimeError("snapshot continuation: executable outline changed path, effective limit, detail or cursor")
        else:
            requested = dict(call.get("arguments", {}))
            actual = dict(arguments)
            actual.pop("cursor", None)
            requested.pop("cursor", None)
            requested_limit = requested.pop("limit", None)
            actual_limit = actual.pop("limit", None)
            if (arguments.get("cursor") != token or actual != requested
                    or type(actual_limit) is not int or not 1 <= actual_limit <= 200
                    or (type(requested_limit) is int and actual_limit > requested_limit)):
                raise RuntimeError("snapshot continuation: executable search changed query scope or effective limit")
        return {"tool": executable["name"], "arguments": arguments}
    arguments = dict(call.get("arguments", {}), cursor=token)
    if call["tool"] == "backend.outline":
        # MCP can reduce the first page to fit its response budget. Its
        # continuation is bound to that effective limit, not the omitted or
        # original request limit. Copy the public continuation operands.
        limit = first.get("page", {}).get("limit")
        path = first.get("package", {}).get("project")
        detail = first.get("detail")
        if type(limit) is not int or limit <= 0 or not isinstance(path, str) or not path or detail not in ("summary", "full"):
            raise RuntimeError("snapshot continuation: missing effective outline operands")
        arguments.update(path=path, limit=limit, detail=detail)
    return {"tool": call["tool"], "arguments": arguments}


def normalize_continuations(value, proven_tokens: set[str]):
    """Compare public content, not opaque proof bytes, only after real token consumption."""
    if isinstance(value, list):
        return [normalize_continuations(item, proven_tokens) for item in value]
    if not isinstance(value, dict):
        return value
    result = {key: normalize_continuations(item, proven_tokens) for key, item in value.items()}
    token = offered_cursor(value)
    if token is not None:
        if not isinstance(token, str) or token not in proven_tokens:
            raise RuntimeError("snapshot continuation: unconsumed token cannot be normalized")
        prefix = token.split("-", 1)[0]
        if not re.fullmatch(r"(?:pc|mcp)[0-9]+", prefix):
            raise RuntimeError("snapshot continuation: unknown token family")
        budget = value.get("budget", {})
        size, width, estimated = (budget.get(key) for key in ("bytes", "bytesPerToken", "estimatedTokens"))
        if any(type(number) is not int for number in (size, width, estimated)) or width <= 0 or size < len(token.encode()) or estimated != (size + width - 1) // width:
            raise RuntimeError("snapshot continuation: invalid encoded-byte budget")
        advertised = value.get("continuation_call", {}).get("arguments", {}).get("cursor")
        copies = int(value.get("nextCursor") == token) + int(advertised == token)
        payload_size = size - copies * len(token.encode())
        if payload_size < 0:
            raise RuntimeError("snapshot continuation: cursor copies exceed encoded-byte budget")
        result["budget"] = dict(result["budget"])
        result["budget"].pop("bytes")
        result["budget"].pop("estimatedTokens")
        result["budget"].update(bytesExcludingCursor=payload_size,
                                estimatedTokensExcludingCursor=(payload_size + width - 1) // width)
        placeholder = prefix + "-<live-validated>"
        if value.get("nextCursor") is not None:
            result["nextCursor"] = placeholder
        if advertised == token:
            result["continuation_call"]["arguments"]["cursor"] = placeholder
    return result


def require_mcp_wire_budget(reply: dict, frame_size: int) -> None:
    budget = reply.get("_meta", {}).get("backend/wireBudget", {})
    size, width, estimated, max_bytes, max_tokens = (budget.get(key) for key in
        ("bytes", "bytesPerToken", "estimatedTokens", "limitBytes", "limitTokens"))
    if any(type(number) is not int for number in (size, width, estimated, max_bytes, max_tokens)) or width <= 0 or max_bytes <= 0 or max_tokens <= 0:
        raise RuntimeError("snapshot continuation: missing MCP wire accounting")
    if budget.get("scope") != "complete_jsonrpc_line" or size != frame_size or estimated != (size + width - 1) // width or size > max_bytes:
        raise RuntimeError("snapshot continuation: dishonest MCP wire accounting")


def normalize_surface_answers(answers: dict, proven_answers: dict[str, set[str]]) -> dict:
    """Normalize each answer only with cursor proofs from that exact request."""
    normalized = {}
    for name, answer in answers.items():
        if name != "mcp" or not isinstance(answer, dict):
            normalized[name] = normalize_continuations(
                answer, proven_answers.get(f"cli:{name}", set()))
            continue
        normalized_mcp = {}
        for field, value in answer.items():
            if field != "calls" or not isinstance(value, list):
                normalized_mcp[field] = normalize_continuations(value, set())
                continue
            normalized_mcp[field] = [
                normalize_continuations(call, proven_answers.get(f"mcp:{index}", set()))
                for index, call in enumerate(value)
            ]
        normalized[name] = normalized_mcp
    # The MCP whole-line counter includes the opaque token as well as the
    # inner answer budget. Validate it against the actual compact JSON-RPC
    # envelope, then compare the normalized envelope without its own counters.
    raw_calls = answers.get("mcp", {}).get("calls", [])
    for identifier, (raw, projected) in enumerate(zip(raw_calls, normalized.get("mcp", {}).get("calls", [])), start=2):
        reply = raw["reply"]
        if offered_cursor(reply.get("structuredContent", {})) is None:
            continue
        budget = reply.get("_meta", {}).get("backend/wireBudget", {})
        # The interactive driver already validated the raw line length.
        # Recheck its arithmetic/limits before projecting the opaque proof.
        require_mcp_wire_budget(reply, budget.get("bytes"))
        width = budget["bytesPerToken"]

        def frame_bytes(body):
            envelope = {"jsonrpc": "2.0", "id": identifier, "result": body}
            return len(json.dumps(envelope, separators=(",", ":"), ensure_ascii=False).encode()) + 1

        projected_budget = projected["reply"]["_meta"]["backend/wireBudget"]
        projected_budget.pop("bytes")
        projected_budget.pop("estimatedTokens")
        payload_size = frame_bytes(projected["reply"])
        projected_budget.update(normalizedFrameBytesExcludingCounters=payload_size,
                                normalizedEstimatedTokensExcludingCounters=(payload_size + width - 1) // width)
    return normalized


def _assert_published_root(answer: dict, published_root: str, label: str) -> None:
    """Reject a replay that answers from a different immutable publication."""
    if answer.get("source_root") is not None and answer["source_root"] != published_root:
        raise RuntimeError(f"{label}: replay changed the published source root")
    page = answer.get("page")
    if isinstance(page, dict) and page.get("source_root") is not None and page["source_root"] != published_root:
        raise RuntimeError(f"{label}: replay changed the published source root")


def _validate_owner_usage(usage: dict, label: str) -> None:
    for field in ("peak_rss_kib", "resident_rss_kib"):
        value = usage.get(field)
        if type(value) is not int or value < 0:
            raise RuntimeError(f"owner {field} counter is invalid {label}")
    cpu = usage.get("cpu_seconds")
    if type(cpu) not in (int, float) or not math.isfinite(cpu) or cpu < 0:
        raise RuntimeError(f"owner CPU counter is invalid {label}")


def _replay_comparison_projection(value):
    """Drop only declared volatile fields; retain every answer and basis field."""
    if isinstance(value, dict):
        return {key: _replay_comparison_projection(item)
                for key, item in sorted(value.items()) if key not in VOLATILE_KEYS}
    if isinstance(value, list):
        return [_replay_comparison_projection(item) for item in value]
    return value


def _assert_same_initial_answer(measured: dict, replayed: dict,
                                measured_tokens: set[str], replay_tokens: set[str],
                                label: str) -> None:
    """Compare immutable first-page content after each offered token was proven."""
    left = _replay_comparison_projection(normalize_continuations(measured, measured_tokens))
    right = _replay_comparison_projection(normalize_continuations(replayed, replay_tokens))
    if left != right:
        raise RuntimeError(f"{label}: proof replay initial answer differs from the measured answer")


def _mcp_structured(reply: dict, label: str) -> dict:
    if (not isinstance(reply, dict) or reply.get("isError", False)
            or not isinstance(reply.get("structuredContent"), dict)):
        raise RuntimeError(f"{label}: MCP refused the offered cursor: {reply}")
    return reply["structuredContent"]


def _single_mcp_hop(owner: Owner, project: Path, call: dict, first: dict,
                    deadline: float, label: str, published_root: str) -> dict:
    """Measure one offered page using a fresh session and the current absolute deadline."""
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RuntimeError(f"{label}: MCP continuation deadline exceeded")
    continuation = mcp_continuation_call(call, first)
    continued, _seconds = run_mcp(owner, project, [continuation], min(10, remaining))
    if continued["exit"] != 0 or len(continued["calls"]) != 1:
        raise RuntimeError(f"{label}: MCP continuation session failed")
    following = _mcp_structured(continued["calls"][0]["reply"], label)
    require_continuation_reply(first, following, published_root)
    return following


def _replay_cli_cursor(owner: Owner, project: Path, args: list[str], key: str,
                       measured: dict, case: dict, published_root: str,
                       evidence: list[dict], deadline: float) -> float:
    """Replay one selected CLI request after measurement, then exhaust only its proof chain."""
    started = time.monotonic()
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RuntimeError(f"{key}: original query deadline expired before proof replay")
    replayed, _seconds = run_cli(owner, project, args, remaining)
    require_cli_success(key + " proof replay", replayed)
    current = replayed["json"]
    if args[0] == "search" and args[-1] in case.get("authored_symbols", {}):
        current, _frames = resolve_partial_authored_search(
            replayed,
            lambda remaining: run_cli(owner, project, args, remaining)[0],
            deadline,
            transport="cli",
            query=args[-1],
            expected={args[-1]: case["authored_symbols"][args[-1]]},
            language=case["languages"][0],
            request=args,
        )
        current = current["json"]
    _assert_published_root(current, published_root, key)
    replay_token = require_initial_cursor_shape(current)
    measured_token = require_initial_cursor_shape(measured["json"])
    if (replay_token is None) != (measured_token is None):
        raise RuntimeError(f"{key}: proof replay changed cursor availability")
    proven = set()
    if replay_token is not None:
        receipt_budget = {"bytes": 0}
        def fetch(current_page, cursor):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(f"{key}: proof replay deadline exceeded")
            if current_page.get("answer") == "outline":
                following, _elapsed = run_cli(owner, project, args + ["--cursor", cursor], min(10, remaining))
                require_cli_success(key + " proof continuation", following)
                return following["json"]
            following, _elapsed = run_cli(owner, project, args + ["--cursor", cursor], min(10, remaining))
            require_cli_success(key + " proof continuation", following)
            return following["json"]

        def retain(page_number, page, input_cursor):
            kind = "outline" if current.get("answer") == "outline" else "search"
            _emit_continuation_page(evidence, "cli", kind, page_number,
                                    {"arguments": args}, page, input_cursor,
                                    byte_budget=receipt_budget)

        if current.get("answer") == "outline":
            walk_outline_pages(current, fetch, published_root,
                               max_pages=case.get("outline_max_pages", 256),
                               deadline=deadline, on_page=retain)
        elif current.get("answer") == "records":
            walk_record_pages(current, fetch, deadline=deadline, on_page=retain)
        else:
            raise RuntimeError(f"{key}: proof replay has an unsupported answer")
        proven.add(replay_token)
    _assert_same_initial_answer(measured["json"], current,
                                {measured_token} if measured_token else set(), proven, key)
    return time.monotonic() - started


def _replay_mcp_calls(owner: Owner, project: Path, calls: list[dict], measured: dict,
                      case: dict, published_root: str, evidence: list[dict],
                      measured_proofs: dict[str, set[str]], deadline: float) -> float:
    """Replay selected MCP calls and exhaust their cursor chains in one proof session."""
    started = time.monotonic()
    if deadline <= started:
        raise RuntimeError("MCP original session deadline expired before proof replay")
    timeout = deadline - started
    answer_index = 0

    def prove(call, reply, exchange, readiness_exchange):
        nonlocal answer_index
        index = answer_index
        answer_index += 1
        if index >= len(measured.get("calls", [])):
            raise RuntimeError("MCP proof replay returned an extra initial answer")
        measured_call = measured["calls"][index]
        if measured_call.get("call") != call:
            raise RuntimeError("MCP proof replay changed the selected request order or operands")
        measured_reply = measured_call.get("reply")
        query = call.get("arguments", {}).get("query")
        if call["tool"] == "backend.search" and query in case.get("authored_symbols", {}):
            replay_reply, _frames = resolve_partial_authored_search(
                reply,
                lambda remaining: readiness_exchange(call, remaining),
                readiness_exchange.deadline,
                transport="mcp",
                query=query,
                expected={query: case["authored_symbols"][query]},
                language=case["languages"][0],
                request=call,
            )
        else:
            replay_reply = reply
        replay_first = _mcp_structured(replay_reply, "MCP proof replay initial request")
        measured_first = _mcp_structured(measured_reply, "MCP measured initial request")
        _assert_published_root(replay_first, published_root, "MCP proof replay")
        replay_token = require_initial_cursor_shape(replay_first)
        measured_token = require_initial_cursor_shape(measured_first)
        if (replay_token is None) != (measured_token is None):
            raise RuntimeError("MCP proof replay changed cursor availability")
        replay_proven = set()
        if replay_token is not None:
            page_requests = {}
            receipt_budget = {"bytes": 0}
            def fetch(current, cursor):
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("MCP cursor proof replay deadline exceeded")
                continuation = mcp_continuation_call(call, current)
                actual_arguments = dict(continuation["arguments"])
                actual_arguments.pop("cursor", None)
                page_requests[cursor] = {"arguments": actual_arguments}
                following = exchange(continuation, min(10, remaining))
                return _mcp_structured(following, "MCP cursor proof replay")

            def retain(page_number, page, input_cursor):
                _emit_continuation_page(evidence, "mcp", call["tool"], page_number,
                                        page_requests.get(input_cursor,
                                                          {"arguments": dict(call.get("arguments", {}))}), page,
                                        input_cursor, byte_budget=receipt_budget)

            if replay_first.get("answer") == "outline":
                walk_outline_pages(replay_first, fetch, published_root,
                                   max_pages=case.get("outline_max_pages", 256),
                                   deadline=deadline, on_page=retain)
            elif replay_first.get("answer") == "records":
                walk_record_pages(replay_first, fetch, deadline=deadline, on_page=retain)
            else:
                raise RuntimeError("MCP proof replay has an unsupported answer")
            replay_proven.add(replay_token)
        _assert_same_initial_answer(
            measured_first, replay_first,
            measured_proofs.get(f"mcp:{index}", set()), replay_proven,
            f"MCP call {index} proof replay",
        )
        return replay_reply if replay_reply is not reply else None

    replay, _seconds = run_mcp_interactive(
        owner, project, calls, timeout, prove, absolute_deadline=deadline)
    if replay.get("exit") != 0 or not replay.get("tools"):
        raise RuntimeError("MCP proof replay session failed")
    if answer_index != len(calls):
        raise RuntimeError("MCP proof replay omitted an initial selected request")
    return time.monotonic() - started


def require_authored_symbols(answer: dict, expected: dict, language: str) -> None:
    for query, declaration in expected.items():
        # Search text need not be the compiler's canonical identity. Java,
        # for example, finds a type by its short name but reports its FQN.
        if isinstance(declaration, str):
            name, filename = query, declaration
        elif (isinstance(declaration, dict) and set(declaration) == {"name", "path"}
              and all(isinstance(value, str) and value for value in declaration.values())):
            name, filename = declaration["name"], declaration["path"]
        else:
            raise RuntimeError(f"real corpus: invalid authored declaration for {query}")
        if not any(row.get("identity", {}).get("name") == name
                   and row.get("identity", {}).get("path") == filename and row.get("language") == language
                   and row.get("state") == "ready"
                   for row in answer.get("records", [])):
            observed = [{"name": row.get("identity", {}).get("name"), "path": row.get("identity", {}).get("path"),
                         "language": row.get("language")} for row in answer.get("records", [])[:20]]
            raise RuntimeError(f"real corpus: missing authored {language} symbol {name} in {filename}; observed {observed}; first records {json.dumps(answer.get("records", [])[:2], sort_keys=True)}")


def _search_frame_content(frame: dict, transport: str, query: str) -> dict:
    if transport == "cli":
        require_cli_success(f"cli search {query}", frame)
        content = frame["json"]
    elif transport == "mcp":
        if not isinstance(frame, dict) or frame.get("isError", False) or "code" in frame:
            raise RuntimeError(f"MCP search {query} failed: {frame}")
        content = frame.get("structuredContent")
    else:
        raise ValueError(f"unknown search transport: {transport}")
    if not isinstance(content, dict) or content.get("answer") != "records":
        raise RuntimeError(f"{transport} search {query} returned no records page: {frame}")
    if content.get("query") != query:
        raise RuntimeError(f"{transport} search changed query {query!r}: {content.get('query')!r}")
    return content


def _require_same_search_basis(first: dict, current: dict, transport: str, query: str) -> None:
    # Public summary pages do not always serialize the view commitment. When
    # they do, keep every exposed basis field fixed across the retry. The
    # caller also reuses the same owner, project, and exact request operands.
    for field in ("basis", "view_root", "viewRoot", "revision", "revision_tag", "source_root"):
        if field in first or field in current:
            if first.get(field) != current.get(field):
                raise RuntimeError(f"{transport} search {query}: changed {field} during readiness retry")


def _emit_search_readiness_frame(transport: str, query: str, attempt: int,
                                 request: dict | list[str], response: dict) -> None:
    # Retain complete raw product frames in CI logs, outside the reviewed
    # snapshot payload, so run-dependent readiness does not enter goldens.
    with OUTPUT_LOCK:
        print("SURFACE-READINESS-FRAME " + json.dumps({
            "transport": transport, "query": query, "attempt": attempt,
            "request": request, "response": response,
        }, sort_keys=True, separators=(",", ":"), ensure_ascii=False), flush=True)


def resolve_partial_authored_search(first: dict, retry, deadline: float, *,
                                    transport: str, query: str, expected: dict,
                                    language: str, request: dict | list[str],
                                    now=time.monotonic, sleep=time.sleep) -> tuple[dict, list[dict]]:
    """Keep the useful name-stage observation, then require full readiness.

    The retry callback must use the same owner and exact request operands. Its
    timeout is always the remaining time to the original query/session
    deadline; retries cannot restart that deadline or turn a refusal into a
    partial success.
    """
    first_content = _search_frame_content(first, transport, query)
    frames = [first]
    readiness = first_content.get("readiness")
    scope = first_content.get("search_scope")
    full_search = first_content.get("full_search")
    if readiness == "ready" and scope != "published_names_only" and full_search in (None, "ready"):
        require_authored_symbols(first_content, expected, language)
        return first, frames
    if (readiness != "partial" or scope != "published_names_only"
            or full_search != "preparing"):
        raise RuntimeError(
            f"{transport} search {query}: expected an explicit useful preparing-stage reply "
            f"or a steady ready reply; readiness={readiness!r}, scope={scope!r}, full_search={full_search!r}"
        )
    _emit_search_readiness_frame(transport, query, 0, request, first)
    require_authored_symbols(first_content, expected, language)
    delay = 0.05
    attempt = 0
    while True:
        remaining = deadline - now()
        if remaining <= 0:
            raise RuntimeError(f"{transport} search {query}: full-search readiness deadline exceeded")
        sleep(min(delay, remaining))
        remaining = deadline - now()
        if remaining <= 0:
            raise RuntimeError(f"{transport} search {query}: full-search readiness deadline exceeded")
        attempt += 1
        current = retry(remaining)
        frames.append(current)
        _emit_search_readiness_frame(transport, query, attempt, request, current)
        if now() >= deadline:
            raise RuntimeError(f"{transport} search {query}: full-search readiness deadline exceeded")
        content = _search_frame_content(current, transport, query)
        _require_same_search_basis(first_content, content, transport, query)
        readiness = content.get("readiness")
        scope = content.get("search_scope")
        full_search = content.get("full_search")
        if readiness == "ready" and scope != "published_names_only" and full_search in (None, "ready"):
            require_authored_symbols(content, expected, language)
            return current, frames
        if (readiness != "partial" or scope != "published_names_only"
                or full_search != "preparing"):
            raise RuntimeError(
                f"{transport} search {query}: readiness retry left the admitted preparing state "
                f"without a ready full-search reply; readiness={readiness!r}, scope={scope!r}, "
                f"full_search={full_search!r}"
            )
        require_authored_symbols(content, expected, language)
        delay = min(delay * 2, 0.5)


def make_case_directories_writable(root: Path) -> None:
    """Only our copied directories need write permission for a repeat run.

    Nix source directories are read-only. copytree preserves those modes;
    do not change the pinned source or follow a link out of the case tree.
    """
    for directory, _, _ in os.walk(root, followlinks=False):
        path = Path(directory)
        metadata = path.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid():
            raise RuntimeError("surface case cleanup requires owned directories")
        path.chmod(stat.S_IMODE(metadata.st_mode) | stat.S_IWUSR | stat.S_IXUSR)


def emit_index_failure_diagnostic(owner: Owner, project: Path) -> None:
    """Retain discovery and copied exact reads without replacing the setup failure."""
    try:
        deadline = time.monotonic() + 10
        reply, _ = run_cli(owner, project, [
            "package-profile", str(project),
            "--compiler-outcome", '{"kind":"current"}',
        ], 10)
        print("SURFACE-INDEX-DIAGNOSTIC " + json.dumps({
            "project": str(project), "reply": reply,
        }, sort_keys=True), flush=True)
        outcomes = (reply.get("json") or {}).get("compiler_outcomes") or {}
        if reply.get("exit") != 0 or outcomes.get("state") != "summary":
            return
        reads = [entry.get("read") for entry in outcomes.get("coverage") or []
                 if entry.get("state") == "retained"]
        reads.extend((entry.get("diagnostic") or {}).get("read")
                     for entry in outcomes.get("refusals") or []
                     if (entry.get("diagnostic") or {}).get("state") == "retained")
        seen = set()
        # One shared deadline and at most 16 bounded pages/diagnostics. Selectors
        # come from this owner; never reconstruct a basis or infer missing rows.
        for _ in range(16):
            if not reads:
                break
            read = reads.pop(0)
            if not isinstance(read, dict) or read.get("kind") not in ("coverage", "diagnostic"):
                continue
            encoded = json.dumps(read, sort_keys=True, separators=(",", ":"))
            if encoded in seen:
                continue
            seen.add(encoded)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            reply, _ = run_cli(owner, project, [
                "package-profile", str(project), "--compiler-outcome", encoded,
            ], remaining)
            print("SURFACE-INDEX-DIAGNOSTIC " + json.dumps({
                "project": str(project), "read": read, "reply": reply,
            }, sort_keys=True), flush=True)
            outcome = (reply.get("json") or {}).get("compiler_outcomes") or {}
            if reply.get("exit") != 0 or outcome.get("state") in ("stale", "unavailable"):
                break
            if outcome.get("state") == "coverage":
                following = (outcome.get("page") or {}).get("next")
                if following is not None:
                    reads.append(following)
    except Exception as error:
        # A diagnostic cannot replace the original setup failure or pass a case.
        log(f"index failure diagnostic unavailable: {error}")


def run_case(case: dict, bin_dir: Path) -> dict:
    case_root = SCRATCH / case["name"]
    if case_root.exists():
        make_case_directories_writable(case_root)
        shutil.rmtree(case_root)
    case_root.mkdir(parents=True, mode=0o700)
    project = case_root / "project"
    if "project_env" in case:
        source = os.environ.get(case["project_env"])
        if not source or not Path(source).is_absolute():
            raise RuntimeError(f"real corpus: missing absolute pinned project authority {case['project_env']}")
        shutil.copytree(source, project)
    else:
        shutil.copytree(ROOT / case["project"], project)
    make_case_directories_writable(project)
    timeout = case.get("timeout_seconds", 300)
    outline_max_pages = case.get("outline_max_pages", 256)
    if (isinstance(outline_max_pages, bool) or not isinstance(outline_max_pages, int)
            or outline_max_pages < 1):
        raise RuntimeError("outline continuation page bound must be a positive integer")
    cargo_vendor = None
    if "cargo_vendor_env" in case:
        vendor = os.environ.get(case["cargo_vendor_env"], "")
        cargo_vendor = Path(vendor)
        if not cargo_vendor.is_absolute() or not cargo_vendor.is_dir() or not str(cargo_vendor.resolve()).startswith("/nix/store/"):
            raise RuntimeError("real corpus: missing pinned Cargo dependency vendor directory")
    owner = Owner(bin_dir, case_root, cargo_vendor)
    owner.case_name = case["name"]

    record = {"answers": {}, "performance": {"seconds": {}}, "continuation_evidence": []}
    proven_answers: dict[str, set[str]] = {}
    cli_proofs: list[tuple[list[str], str, dict, float]] = []
    preparation_diagnostic_captured = False

    def capture_first_full_search_diagnostic(answer_content: dict) -> None:
        nonlocal preparation_diagnostic_captured
        provenance = RUN_PROVENANCE.get()
        if preparation_diagnostic_captured or not _preparation_diagnostic_opt_in(provenance):
            return
        preparation_diagnostic_captured = True
        diagnostic = capture_search_preparation_diagnostic(owner, provenance, answer_content)
        record["search_preparation_diagnostic"] = diagnostic
        emit_search_preparation_diagnostic(diagnostic, case["name"])
    try:
        try:
            if case.get("index_mode", "resumable") == "resumable":
                answer, index_times = run_resumable_index(owner, project, len(case["languages"]), timeout)
            elif case["index_mode"] == "wait":
                target_args = ["--cargo-target", case["cargo_target"]] if case.get("cargo_target") else []
                answer, seconds = run_cli(owner, project, ["add", str(project), *target_args, "--wait"], timeout)
                require_completed_index(answer, len(case["languages"]))
                index_times = {"cli add": round(seconds, 2)}
            else:
                raise RuntimeError(f"unknown index mode: {case['index_mode']}")
            published_root = require_completed_index(answer, len(case["languages"]))
            if case.get("cargo_target"):
                require_selected_cargo_target(answer, case["cargo_target"])
                operation = answer["json"]["index_operation"]["detail"]
                session, _ = run_mcp(owner, project, [{"tool": "backend.index_progress", "arguments": {"operation_key": operation["operation_key"]}}], 10)
                if session["exit"] != 0 or len(session["calls"]) != 1:
                    raise RuntimeError("selected Cargo target: MCP receipt session failed")
                reply = session["calls"][0]["reply"]
                body = reply.get("structuredContent", {})
                projection = body.get("surface", {})
                if reply.get("isError", False) or body.get("fault") is not None or projection.get("result") != "index-operation-status" or projection.get("data") != answer["json"]["index_operation"]:
                    raise RuntimeError("selected Cargo target: CLI/MCP exact terminal receipts disagree")

        except Exception:
            emit_index_failure_diagnostic(owner, project)
            raise
        log(f"index terminal receipt: published {len(case['languages'])} complete native profiles; view {published_root}")
        # Indexing is setup: require success above, then snapshot the verdict.
        # The caller journal embeds fresh random operation/receipt identities;
        # search/outline/packages below snapshot the published product state.
        record["answers"]["cli add"] = {"exit": answer["exit"]}
        record["performance"]["seconds"].update(index_times)
        for args in case.get("cli", []):
            key = "cli " + " ".join(args)
            query_started = time.monotonic()
            query_deadline = query_started + timeout
            answer, seconds = run_cli(owner, project, args, timeout)
            require_cli_success(key, answer)
            if args[0] == "search" and not answer["json"].get("records"):
                raise RuntimeError(f"{key}: expected fixture symbol, got no records")
            if args[0] == "search" and args[-1] in case.get("authored_symbols", {}):
                answer, _frames = resolve_partial_authored_search(
                    answer,
                    lambda remaining: run_cli(owner, project, args, remaining)[0],
                    query_deadline,
                    transport="cli",
                    query=args[-1],
                    expected={args[-1]: case["authored_symbols"][args[-1]]},
                    language=case["languages"][0],
                    request=args,
                )
                seconds = time.monotonic() - query_started
                capture_first_full_search_diagnostic(answer["json"])
            token = require_initial_cursor_shape(answer["json"])
            if token is not None:
                remaining = query_deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError(f"{key}: measured first-hop deadline exceeded")
                following, _ = run_cli(owner, project, args + ["--cursor", token], min(10, remaining))
                require_cli_success(key + " first continuation", following)
                require_continuation_reply(answer["json"], following["json"], published_root)
                proven_answers.setdefault(f"cli:{key}", set()).add(token)
                cli_proofs.append((list(args), key, copy.deepcopy(answer), query_deadline))
            record["answers"][key] = answer
            record["performance"]["seconds"][key] = round(seconds, 2)
        if case.get("mcp"):
            def prove_session_outline(call, reply, exchange, readiness_exchange, answer_index):
                query = call.get("arguments", {}).get("query")
                if (call["tool"] == "backend.search" and query in case.get("authored_symbols", {})):
                    ready, _frames = resolve_partial_authored_search(
                        reply,
                        lambda remaining: readiness_exchange(call, remaining),
                        readiness_exchange.deadline,
                        transport="mcp",
                        query=query,
                        expected={query: case["authored_symbols"][query]},
                        language=case["languages"][0],
                        request=call,
                    )
                    replacement = ready
                else:
                    replacement = None
                first = reply.get("structuredContent", {}) if isinstance(reply, dict) else {}
                if (call["tool"] == "backend.search" and query in case.get("authored_symbols", {})):
                    first = ready.get("structuredContent", {})
                token = require_initial_cursor_shape(first)
                if call["tool"] != "backend.outline" or token is None:
                    return replacement
                remaining = exchange.deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("backend.outline first-hop deadline exceeded")
                continuation = mcp_continuation_call(call, first)
                following = _mcp_structured(
                    exchange(continuation, min(10, remaining)),
                    "backend.outline first continuation",
                )
                require_continuation_reply(first, following, published_root)
                proven_answers.setdefault(f"mcp:{answer_index}", set()).add(token)
                log("backend.outline: measured one offered continuation in its issuing session")
                return replacement

            mcp_answer_index = 0
            def track_mcp_reply(call, reply, exchange, readiness_exchange):
                nonlocal mcp_answer_index
                answer_index = mcp_answer_index
                mcp_answer_index += 1
                return prove_session_outline(call, reply, exchange, readiness_exchange, answer_index)

            mcp_deadline = time.monotonic() + timeout
            answer, seconds = run_mcp_interactive(owner, project, case["mcp"], timeout,
                                                   track_mcp_reply, absolute_deadline=mcp_deadline)
            if answer["exit"] != 0 or not answer["tools"]:
                raise RuntimeError(f"MCP session failed: {answer}")
            for answer_index, result in enumerate(answer["calls"]):
                reply = result["reply"]
                if not isinstance(reply, dict) or reply.get("isError", False) or "code" in reply:
                    raise RuntimeError(f"MCP call failed: {result}")
                if result["call"]["tool"] == "backend.search" and not reply.get("structuredContent", {}).get("records"):
                    raise RuntimeError(f"MCP search returned no fixture symbols: {result}")
                text = result["call"].get("arguments", {}).get("query")
                if result["call"]["tool"] == "backend.search" and text in case.get("authored_symbols", {}):
                    require_authored_symbols(reply["structuredContent"], {text: case["authored_symbols"][text]}, case["languages"][0])
                first = reply.get("structuredContent", {})
                token = require_initial_cursor_shape(first)
                if token is not None and token not in proven_answers.get(f"mcp:{answer_index}", set()):
                    _single_mcp_hop(owner, project, result["call"], first, mcp_deadline,
                                    result["call"]["tool"] + " first continuation", published_root)
                    proven_answers.setdefault(f"mcp:{answer_index}", set()).add(token)
                    log(f"{result['call']['tool']}: measured one offered continuation in a fresh MCP session")
            record["answers"]["mcp"] = answer
            record["performance"]["seconds"]["mcp"] = round(seconds, 2)
        # Freeze the historical selected-call workload before cursor proof:
        # initial replies plus exactly one offered continuation, with MCP
        # issuing sessions fully closed as in the original measurement.
        preproof_usage = owner.usage_raw()
        _validate_owner_usage(preproof_usage, "before cursor proof")
        with OUTPUT_LOCK:
            print("CI-RESOURCE-PHASE " + json.dumps({
                **(RUN_PROVENANCE.get() or {}), "case": case["name"], "phase": "preproof",
                "owner": preproof_usage,
            }, sort_keys=True), flush=True)
        # Reissue initial requests and exhaust full chains while the measured
        # publication is still current. Each proof uses the original absolute
        # query deadline, never a renewed deadline.
        proof_started = time.monotonic()
        for args, key, measured, deadline in cli_proofs:
            _replay_cli_cursor(owner, project, args, key, measured, case,
                               published_root, record["continuation_evidence"], deadline)
        if case.get("mcp"):
            has_mcp_cursor = any(require_initial_cursor_shape(
                (item.get("reply") or {}).get("structuredContent", {})) is not None
                for item in record["answers"]["mcp"].get("calls", []))
            if has_mcp_cursor:
                _replay_mcp_calls(owner, project, case["mcp"], record["answers"]["mcp"],
                                  case, published_root, record["continuation_evidence"],
                                  proven_answers, mcp_deadline)
        proof_usage = owner.usage_raw()
        _validate_owner_usage(proof_usage, "after cursor proof")
        if proof_usage["cpu_seconds"] < preproof_usage["cpu_seconds"]:
            raise RuntimeError("owner CPU counter decreased during cursor proof phase")
        if _owner_usage_diagnostics_enabled():
            _emit_owner_usage_diagnostic(
                case["name"], "proof-sample-before-reset",
                preproof=preproof_usage, proof=proof_usage,
                hwm_snapshot_decreased=(proof_usage["peak_rss_kib"]
                                        < preproof_usage["peak_rss_kib"]),
                reset_state="not-issued",
            )
        has_cursor_proof = any(require_initial_cursor_shape(item[2]["json"]) is not None
                               for item in cli_proofs)
        if case.get("mcp"):
            has_cursor_proof = has_cursor_proof or any(
                require_initial_cursor_shape((item.get("reply") or {}).get("structuredContent", {})) is not None
                for item in record["answers"]["mcp"].get("calls", [])
            )
        reset_usage = owner.reset_peak_rss() if has_cursor_proof else proof_usage
        _validate_owner_usage(reset_usage, "after VmHWM reset")
        proof_elapsed = time.monotonic() - proof_started
        phase_record = {
            "preproof": preproof_usage,
            "postproof": proof_usage,
            "post_reset": reset_usage,
            # Linux documents the /proc RSS/HWM fields as unlocked snapshots.
            # Preserve a conservative envelope instead of requiring two
            # independently sampled VmHWM values to be monotonic.
            "proof_peak_rss_kib": max(
                preproof_usage["peak_rss_kib"], proof_usage["peak_rss_kib"]),
            "proof_cpu_seconds": proof_usage["cpu_seconds"] - preproof_usage["cpu_seconds"],
            "proof_elapsed_seconds": round(proof_elapsed, 3),
            "peak_reset_method": ("locald /proc clear_refs=5; VmHWM verified equals VmRSS"
                                  if has_cursor_proof else "not needed; no selected cursor proof"),
            "peak_reset_performed": has_cursor_proof,
        }
        record["resource_phase_evidence"] = phase_record
        with OUTPUT_LOCK:
            print("CI-RESOURCE-PHASE " + json.dumps({
                **(RUN_PROVENANCE.get() or {}), "case": case["name"], "phase": "proof-complete",
                **phase_record,
            }, sort_keys=True), flush=True)
        record["performance"].update({
            "cpu_seconds": round(preproof_usage["cpu_seconds"], 2),
            "peak_rss_kib": preproof_usage["peak_rss_kib"],
        })
        if case.get("live_contracts"):
            record["performance"]["seconds"].update(
                run_live_contracts(owner, project, case["live_contracts"], published_root)
            )
        if case.get("gui"):
            from gui_contract import run_gui
            answer, usage, evidence = run_gui(owner, project, case["gui"])
            record["answers"]["gui"] = answer
            record["performance"]["seconds"].update(usage.pop("seconds"))
            record["performance"].update(usage)
            with OUTPUT_LOCK:
                print("CI-GUI-EVIDENCE " + json.dumps({**(RUN_PROVENANCE.get() or {}), **evidence}, sort_keys=True), flush=True)
        final_usage = owner.usage_raw()
        _validate_owner_usage(final_usage, "after live/GUI phases")
        if final_usage["cpu_seconds"] < proof_usage["cpu_seconds"]:
            raise RuntimeError("owner CPU counter decreased across cursor proof and live/GUI phases")
        if (_owner_usage_diagnostics_enabled()
                and final_usage["peak_rss_kib"] < reset_usage["peak_rss_kib"]):
            _emit_owner_usage_diagnostic(
                case["name"], "post-reset-hwm-snapshot-decreased",
                reset=reset_usage, final=final_usage,
                retained_peak=max(reset_usage["peak_rss_kib"],
                                  final_usage["peak_rss_kib"]),
                reset_state="issued" if has_cursor_proof else "not-needed",
            )
        postproof_cpu = final_usage["cpu_seconds"] - proof_usage["cpu_seconds"]
        if postproof_cpu < 0:
            raise RuntimeError("owner post-proof CPU delta is negative")
        phase_record["post_reset_final"] = final_usage
        phase_record["post_reset_peak_rss_kib"] = max(
            reset_usage["peak_rss_kib"], final_usage["peak_rss_kib"])
        with OUTPUT_LOCK:
            print("CI-RESOURCE-PHASE " + json.dumps({
                **(RUN_PROVENANCE.get() or {}), "case": case["name"], "phase": "final",
                **phase_record,
            }, sort_keys=True), flush=True)
        record["performance"]["cpu_seconds"] = round(preproof_usage["cpu_seconds"] + postproof_cpu, 2)
        record["performance"]["peak_rss_kib"] = max(
            preproof_usage["peak_rss_kib"], phase_record["post_reset_peak_rss_kib"])
    finally:
        case_error = sys.exc_info()[1]
        try:
            diagnostics = CASE_DIAGNOSTICS.get()
            _record_owner_threads(diagnostics, owner.process.pid)
        finally:
            try:
                owner.stop()
            except Exception as cleanup_error:
                if case_error is not None:
                    raise RuntimeError(f"{case_error}\nowner cleanup failed: {cleanup_error}") from case_error
                raise
    record["answers"] = stable(normalize_surface_answers(record["answers"], proven_answers))
    return record


def regressions(baseline: dict, measured: dict) -> list[str]:
    found = []

    def check(metric: str, name: str, old: float, new: float) -> None:
        if new > max(old * TOLERANCE[metric], FLOOR[metric]):
            found.append(f"{name}: {new} vs baseline {old} (allowed x{TOLERANCE[metric]})")

    for name, old in baseline.get("seconds", {}).items():
        if name in measured["seconds"]:
            check("seconds", f"seconds[{name}]", old, measured["seconds"][name])
    for metric in ("peak_rss_kib", "cpu_seconds", "gui_peak_rss_kib", "gui_cpu_seconds"):
        if metric in baseline and metric in measured:
            policy = metric.removeprefix("gui_")
            check(policy, metric, baseline[metric], measured[metric])
    return found


def print_candidate(name: str, record: dict) -> None:
    with OUTPUT_LOCK:
        print(f"SURFACE-SNAPSHOT-BEGIN {name}\n"
              + json.dumps({"answers": record["answers"], "performance": record["performance"]},
                           indent=2, sort_keys=True, ensure_ascii=False)
              + f"\nSURFACE-SNAPSHOT-END {name}", flush=True)


def execute_case(case: dict, bin_dir: Path) -> tuple[dict | None, float, str | None]:
    case_name = case["name"]
    with _context_value(CURRENT_CASE, case_name), _context_value(CASE_DIAGNOSTICS, None):
        started = time.monotonic()
        log(f"case {case_name}")
        provenance = RUN_PROVENANCE.get()
        diagnostics = (_start_diagnostics(provenance, case_name)
                       if provenance is not None else None)
        try:
            with _context_value(CASE_DIAGNOSTICS, diagnostics):
                try:
                    if case.get("kind") == "cli-contract":
                        from cli_contract import run_cases
                        # Shared scratch paths and direct stdout in this small runner.
                        with OUTPUT_LOCK:
                            record = {"performance": run_cases(bin_dir)}
                    else:
                        record = run_case(case, bin_dir)
                    result = (record, time.monotonic() - started, None)
                except Exception as error:  # one broken case must not hide the others
                    result = (None, time.monotonic() - started, str(error))
            return result
        finally:
            # Preserve a control-flow exception already unwinding from the
            # product case. Otherwise ordinary diagnostic failures are recorded
            # by the shared observer, while cancellation is allowed to escape.
            if diagnostics is not None and sys.exc_info()[0] is None:
                _finish_diagnostics(diagnostics)


def _invoke_case(case: dict, bin_dir: Path, provenance: dict | None) -> tuple[dict | None, float, str | None]:
    with _context_value(RUN_PROVENANCE, provenance):
        return execute_case(case, bin_dir)


def bounded_text(value: str, limit: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= limit:
        return value
    return encoded[:limit - 60].decode("utf-8", errors="ignore") + "\n… full detail retained in CI logs"


def evidence_artifact(bin_dir: Path, source_sha: str) -> str:
    """Admit the actual artifact bytes and checkout before attaching source identity."""
    from t1_artifacts import TARGET, digest, verify
    from source_identity import verify_checkout
    verify_checkout(ROOT, source_sha)
    directory = bin_dir.absolute()
    if directory.name != "bin":
        raise ValueError("CI evidence requires the verified artifact's bin directory")
    verify(directory.parent, source_sha, TARGET)
    return digest(directory.parent / "manifest.json")


def write_ci_evidence(path: Path, source_sha: str, artifact_digest: str,
                      cases: dict, filtered: bool) -> None:
    report = {"schema": 1, "source_sha": source_sha,
              "artifact_manifest_sha256": artifact_digest,
              "selection": ("Explicit case filter; see the case list." if filtered else
                            "All catalog cases; no product-path narrowing."),
              "cases": cases}
    # A reader must never observe a partially written source-bound document.
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".ci-evidence-", delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(report, stream, indent=2, sort_keys=True, ensure_ascii=False)
            stream.write("\n")
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def drain_task_volume() -> None:
    """Finish Linux runtime-import writes before measuring product operations.

    T2's Nix runtime/tool import dirties the same ext4 filesystem as the
    private owner workspace. Its journal transaction can hold the first
    owner's directory fsync beyond the startup bound. Drain that setup I/O
    once, outside case measurements but inside the unchanged CI deadline.
    Subsequent product writes retain every real durability barrier.
    """
    if sys.platform != "linux":
        return
    started = time.monotonic()
    try:
        result = process_custody.run(
            ["sync", "--file-system", str(ROOT)],
            capture_output=True, text=True, timeout=60,
        )
    except subprocess.TimeoutExpired as error:
        seconds = time.monotonic() - started
        print(f"CI-TIMING lane=t2 step=filesystem-drain seconds={seconds:.3f} exit=timeout", flush=True)
        raise RuntimeError("T2 infrastructure: runtime-import filesystem drain exceeded 60 seconds") from error
    seconds = time.monotonic() - started
    print(f"CI-TIMING lane=t2 step=filesystem-drain seconds={seconds:.3f} exit={result.returncode}", flush=True)
    if result.returncode != 0:
        raise RuntimeError(f"T2 infrastructure: cannot drain runtime-import writes: {result.stderr.strip()}")


def compare_cases(args: argparse.Namespace, parser: argparse.ArgumentParser) -> int:
    cases = json.loads((SURFACE / "cases.json").read_text())["cases"]
    if any(not re.fullmatch(r"[a-z0-9][a-z0-9_-]*", case["name"]) for case in cases):
        parser.error("case names must be simple scratch directory names")
    if not cases or len({case["name"] for case in cases}) != len(cases):
        parser.error("case catalog must be nonempty with unique scratch directory names")
    if args.case:
        unknown = set(args.case) - {case["name"] for case in cases}
        if unknown:
            parser.error(f"unknown cases: {sorted(unknown)}")
        cases = [case for case in cases if case["name"] in args.case]
    failed = []
    report = {}
    evidence = {}
    provenance = ({"source_sha": args.source_sha,
                   "artifact_manifest_sha256": args.artifact_digest}
                  if args.ci_report else None)
    global CI_SIGTERM_AT
    CI_SIGTERM.clear()
    CI_SIGTERM_AT = None
    interrupted = CI_SIGTERM
    started: set[str] = set()
    completed_at: dict[str, float] = {}
    start_lock = threading.Lock()
    previous_sigterm = None
    if args.ci_report:
        previous_sigterm = signal.getsignal(signal.SIGTERM)

        def handle_sigterm(_signum, _frame):
            global CI_SIGTERM_AT
            if CI_SIGTERM_AT is None:
                CI_SIGTERM_AT = time.monotonic()
            interrupted.set()

        signal.signal(signal.SIGTERM, handle_sigterm)

    def invoke_tracked(case: dict) -> tuple[dict | None, float, str | None]:
        with start_lock:
            if interrupted.is_set():
                return None, 0.0, "not run: CI runner received SIGTERM before the case started"
            started.add(case["name"])
        try:
            return _invoke_case(case, args.bin_dir.resolve(), provenance)
        finally:
            with start_lock:
                completed_at[case["name"]] = time.monotonic()

    def emit_evidence(case: dict, entry: dict) -> None:
        if args.ci_report:
            print("SURFACE-CASE-EVIDENCE " + json.dumps({
                **provenance, "case": case["name"], "verdict": entry,
            }, sort_keys=True, separators=(",", ":")), flush=True)

    def record_result(case: dict, future) -> None:
        record, elapsed, error = future.result()
        entry = {"state": "failed", "seconds": {}}
        evidence[case["name"]] = entry
        try:
            if error is not None:
                with start_lock:
                    case_started = case["name"] in started
                    finished = completed_at.get(case["name"])
                if interrupted.is_set() and case_started and (
                    finished is None or CI_SIGTERM_AT is None or finished > CI_SIGTERM_AT
                ):
                    entry["failure_kind"] = "sigterm-interrupted"
                    entry["detail"] = "case started but completed with an error after the CI runner received SIGTERM; no product verdict"
                    log(f"case {case['name']} returned an error after SIGTERM; classified as interrupted")
                    failed.append(case["name"])
                    return
                if not case_started:
                    entry["failure_kind"] = "infrastructure-not-run"
                    entry["detail"] = "not run: CI runner received SIGTERM before this case started; no product verdict"
                else:
                    entry["detail"] = bounded_text(error, 4096)
                log(f"case {case['name']} could not run: {error}")
                failed.append(case["name"])
                return
            with start_lock:
                finished = completed_at.get(case["name"])
            if interrupted.is_set() and finished is not None and (
                CI_SIGTERM_AT is None or finished > CI_SIGTERM_AT
            ):
                entry["failure_kind"] = "sigterm-interrupted"
                entry["detail"] = "case returned after the CI runner received SIGTERM; no complete pre-deadline verdict"
                failed.append(case["name"])
                log(f"case {case['name']} completed after SIGTERM; classified as interrupted")
                return
            entry.update(record["performance"])
            if "search_preparation_diagnostic" in record:
                entry["search_preparation_diagnostic"] = record["search_preparation_diagnostic"]
            if args.ci_report and record.get("continuation_evidence"):
                entry["continuation_evidence"] = continuation_report_references(record["continuation_evidence"])
            if case.get("kind") == "cli-contract":
                entry["state"] = "passed"
                report[case["name"]] = record["performance"]
                emit_performance(case["name"], record["performance"])
                log(f"case {case['name']} passed ({elapsed:.1f}s)")
                return
            report[case["name"]] = record["performance"]
            emit_performance(case["name"], record["performance"])
            snapshot_path = SURFACE / "snapshots" / f"{case['name']}.json"
            if args.update:
                snapshot_path.parent.mkdir(parents=True, exist_ok=True)
                snapshot_path.write_text(json.dumps(
                    {"answers": record["answers"], "performance": record["performance"]},
                    indent=2, sort_keys=True, ensure_ascii=False) + "\n")
                log(f"case {case['name']} snapshot written ({elapsed:.1f}s)")
                return
            if not snapshot_path.exists():
                entry["detail"] = "No reviewed snapshot; explicit review/update required."
                log(f"case {case['name']} has no reviewed snapshot; run with --update and commit it")
                if args.print_missing:
                    print_candidate(case["name"], record)
                failed.append(case["name"])
                return
            reviewed = json.loads(snapshot_path.read_text())
            baseline = reviewed.get("performance", {}).get("seconds", {}).get("cli add")
            indexed = record["performance"].get("seconds", {}).get("cli add")
            if isinstance(baseline, (int, float)) and baseline > 0 and isinstance(indexed, (int, float)):
                entry["index_delta_percent"] = round((indexed / baseline - 1) * 100, 2)
            problems = []
            if reviewed["answers"] != record["answers"]:
                old = json.dumps(reviewed["answers"], indent=2, sort_keys=True, ensure_ascii=False).splitlines()
                new = json.dumps(record["answers"], indent=2, sort_keys=True, ensure_ascii=False).splitlines()
                diff = "\n".join(difflib.unified_diff(old, new, "reviewed", "this change", lineterm="", n=2))
                entry["snapshot_diff"] = bounded_text(diff, 8192)
                problems.append(f"answers changed; explain or update the snapshot:\n{diff[:20000]}")
            problems += regressions(reviewed.get("performance", {}), record["performance"])
            if problems:
                entry["detail"] = bounded_text("\n".join(problem.split("\n", 1)[0] for problem in problems), 4096)
                failed.append(case["name"])
                for problem in problems:
                    log(f"case {case['name']}: {problem}")
                if args.print_missing and reviewed["answers"] != record["answers"]:
                    print_candidate(case["name"], record)
            else:
                entry["state"] = "passed"
                log(f"case {case['name']} matches ({elapsed:.1f}s, "
                    f"peak {record['performance'].get('peak_rss_kib', 0) // 1024} MiB)")
        finally:
            emit_evidence(case, entry)

    process_custody.activate()
    import cpu_placement
    slots = (cpu_placement.slot_partitions(set(os.sched_getaffinity(0)), args.jobs)
             if hasattr(os, "sched_getaffinity") else [])
    print("SURFACE-CPU-SLOTS " + json.dumps(slots), flush=True)
    free_slots = list(slots)
    slot_lock = threading.Lock()

    def pin_worker() -> None:
        # Each worker thread owns one slot for its lifetime; case processes
        # inherit it, so concurrent cases never share a core.
        if slots:
            with slot_lock:
                slot = free_slots.pop(0)
            cpu_placement.pin_current_thread(slot)

    executor = ThreadPoolExecutor(max_workers=args.jobs, thread_name_prefix="surface",
                                  initializer=pin_worker)
    futures = {executor.submit(invoke_tracked, case): case for case in cases}
    pending = set(futures)
    processed = set()
    was_interrupted = False
    try:
        while pending:
            done, _ = wait(pending, timeout=0.1, return_when=FIRST_COMPLETED)
            for future in done:
                case = futures[future]
                record_result(case, future)
                pending.remove(future)
                processed.add(future)
            if interrupted.is_set():
                was_interrupted = True
                break
    except BaseException:
        executor.shutdown(wait=not interrupted.is_set(), cancel_futures=interrupted.is_set())
        if previous_sigterm is not None and not interrupted.is_set():
            signal.signal(signal.SIGTERM, previous_sigterm)
        raise

    was_interrupted = was_interrupted or interrupted.is_set()
    executor.shutdown(wait=not was_interrupted, cancel_futures=was_interrupted)
    if was_interrupted:
        with start_lock:
            started_at_signal = set(started)
        # Include results that finished as the signal arrived, then label only
        # genuinely unfinished or never-started work as interruption evidence.
        for future, case in futures.items():
            if future in processed:
                continue
            if case["name"] in started_at_signal and future.done() and not future.cancelled():
                record_result(case, future)
            elif case["name"] in started_at_signal:
                entry = {"state": "failed", "seconds": {}, "failure_kind": "sigterm-interrupted",
                         "detail": "case started but did not complete before the CI runner received SIGTERM; outer exit status determines the deadline outcome"}
                evidence[case["name"]] = entry
                failed.append(case["name"])
                log(f"case {case['name']} interrupted by SIGTERM before completion")
                emit_evidence(case, entry)
            else:
                entry = {"state": "failed", "seconds": {}, "failure_kind": "infrastructure-not-run",
                         "detail": "not run: CI runner received SIGTERM before this case started; no product verdict"}
                evidence[case["name"]] = entry
                failed.append(case["name"])
                log(f"case {case['name']} not run: SIGTERM arrived before it started")
                emit_evidence(case, entry)
    if args.report:
        args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    if args.ci_report:
        write_ci_evidence(args.ci_report, args.source_sha, args.artifact_digest, evidence, bool(args.case))
    if was_interrupted or interrupted.is_set():
        if previous_sigterm is not None:
            final_packet = {
                "schema": 1,
                "source_sha": args.source_sha,
                "artifact_manifest_sha256": args.artifact_digest,
                "selection": "All catalog cases; no product-path narrowing." if not args.case else
                             "Explicit case filter; see the case list.",
                "cases": evidence,
                "packet_kind": "harness-interruption-summary",
                "verification": "source/artifact admission was checked before execution; this final log packet was not reread or adapter-reverified",
            }
            print("SURFACE-CI-EVIDENCE " + json.dumps(final_packet, sort_keys=True,
                                                        separators=(",", ":")), flush=True)
            sys.stdout.flush()
            sys.stderr.flush()
            # Reporting cannot release live detached owner/client custody.
            cleanup = process_custody.stop_before_exit()
            print('SURFACE-CHILD-CUSTODY ' + json.dumps(cleanup, sort_keys=True), flush=True)
            os._exit(128 + signal.SIGTERM)
    if previous_sigterm is not None:
        signal.signal(signal.SIGTERM, previous_sigterm)
    if failed:
        log(f"failed: {', '.join(failed)}")
        return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--case", action="append", help="run only these cases (default: all)")
    parser.add_argument("--jobs", type=int, choices=range(1, 5), default=2,
                        help="concurrent isolated cases, 1–4 (default: 2)")
    parser.add_argument("--update", action="store_true", help="rewrite snapshots and baselines for review")
    parser.add_argument("--report", type=Path, help="write measured performance here as JSON")
    parser.add_argument("--ci-report", type=Path, help="write versioned exact-artifact case verdicts/diffs/performance")
    parser.add_argument("--source-sha", help="CI report: required exact artifact and checkout source SHA")
    parser.add_argument(
        "--print-missing",
        action="store_true",
        help="print missing or changed candidate snapshots for review in CI logs",
    )
    args = parser.parse_args()
    if args.ci_report:
        if not args.source_sha or args.update:
            parser.error("--ci-report requires --source-sha and ordinary comparison (no --update)")
        if args.report and args.report.absolute() == args.ci_report.absolute():
            parser.error("--report and --ci-report require different output files")
        try:
            args.artifact_digest = evidence_artifact(args.bin_dir, args.source_sha)
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            parser.error(f"CI evidence artifact admission failed: {error}")
    elif args.source_sha:
        parser.error("--source-sha applies only to --ci-report")
    # Fixed paths require exclusive ownership; another local run must never
    # remove an active case workspace. Concourse tasks have separate /tmp.
    SCRATCH.mkdir(parents=True, exist_ok=True, mode=0o700)
    with open(SCRATCH / ".lock", "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error("another surface run owns the fixed scratch paths")
        if args.ci_report:
            # Retire the pre-case filesystem-drain command under the same
            # owned-session policy as product subprocesses.
            process_custody.activate()
            drain_task_volume()
        from cpu_placement import measurement_placement
        with measurement_placement() as placement:
            print("SURFACE-CPU-PLACEMENT " + json.dumps(placement, sort_keys=True), flush=True)
            return compare_cases(args, parser)


if __name__ == "__main__":
    sys.exit(main())
