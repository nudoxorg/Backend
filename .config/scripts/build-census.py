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
import errno
import json
import os
import pathlib
import re
import select
import selectors
import shutil
import stat
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
MAX_CARGO_BUILD_ENV_BYTES = 256 * 1024
MAX_CARGO_BUILD_ENVIRONMENT_READS = 32
MAX_ARG_COUNT = 512
MAX_SNAPSHOT_ARG_BYTES = 8 * 1024 * 1024
MAX_SNAPSHOT_ARG_PROCESSES = 512
MAX_REPORTED_ARGS = 64
MAX_REPORTED_CARGO_ENTRIES = 512
MAX_REPORTED_PROCESS_ISSUES = 64
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


class _LinuxProcRace(OSError):
    """A proc row changed without evidence that the pinned task exited."""

    def __init__(
        self,
        error_number: int,
        message: str,
        filename: str | bytes | None = None,
        *,
        start_ticks: str | None = None,
        cause_errno: int | None = None,
        pidfd_ready: bool | None = None,
    ) -> None:
        super().__init__(error_number, message, filename)
        self.start_ticks = start_ticks
        self.cause_errno = cause_errno
        self.pidfd_ready = pidfd_ready


class _LinuxProcessExitedDuringRead(Exception):
    """The exact task read from proc exited while its row was being captured."""

    def __init__(self, proof: "_LinuxProcExitProof") -> None:
        super().__init__(f"pid-exited-during-read:{proof.pid}")
        self.proof = proof


@dataclasses.dataclass(frozen=True, slots=True)
class _LinuxProcStat:
    state: str
    ppid: int
    pgid: int
    session: int
    flags: int
    start_ticks: str
    # Missing adapter evidence is never a sole-thread witness. Real proc
    # parsing always supplies field 20; older deterministic adapters may not.
    thread_count: int | None = None


# Linux include/linux/sched.h: PF_KTHREAD.  This is kernel-provided stat data,
# not a command-name or parent-process heuristic.
_LINUX_PF_KTHREAD = 0x00200000
_LINUX_PF_EXITING = 0x00000004
_LINUX_TERMINAL_STATES = frozenset({"Z", "X", "x"})


def _linux_proc_path_absence(error: OSError) -> bool:
    """Only ENOENT and ESRCH can establish that a proc path is absent."""
    return error.errno in {errno.ENOENT, errno.ESRCH}


def _linux_proc_display_path(pid: int, proc_path: pathlib.Path | None, filename: Any) -> str:
    prefix = pathlib.PurePosixPath("/proc") / str(pid)
    if filename is None:
        return str(prefix)
    value = os.fsdecode(filename)
    if proc_path is not None:
        try:
            relative = pathlib.Path(value).relative_to(proc_path)
        except ValueError:
            pass
        else:
            suffix = relative.as_posix()
            return str(prefix) if suffix == "." else str(prefix / suffix)
    if value == str(prefix) or value.startswith(f"{prefix}/"):
        return value
    return str(prefix / pathlib.PurePosixPath(value).name)


@dataclasses.dataclass(frozen=True, slots=True)
class _LinuxTerminalProcRow:
    pid: int
    start_ticks: str
    state: str
    ppid: int
    pgid: int
    session: int


@dataclasses.dataclass(frozen=True, slots=True)
class _LinuxProcExitProof:
    pid: int
    first_start_ticks: str | None
    last_start_ticks: str | None
    first_state: str | None
    last_state: str | None
    pidfd_ready: bool | None
    missing_path: str | None
    missing_filename: str | None
    identity_observed: bool = True
    evidence: str = "pidfd-ready-and-matching-start-identity"
    pidfd_open_errno: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "pid": self.pid,
            "first_identity": (
                {"pid": self.pid, "start_ticks": self.first_start_ticks}
                if self.first_start_ticks is not None
                else None
            ),
            "last_identity": (
                {"pid": self.pid, "start_ticks": self.last_start_ticks}
                if self.last_start_ticks is not None
                else None
            ),
            "first_state": self.first_state,
            "last_state": self.last_state,
            "pidfd_ready": self.pidfd_ready,
            "missing_path": self.missing_path,
            "missing_filename": self.missing_filename,
            "identity_observed": self.identity_observed,
            "evidence": self.evidence,
            "pidfd_open_errno": self.pidfd_open_errno,
        }


@dataclasses.dataclass(frozen=True, slots=True)
class _LinuxProcCapture:
    first: _LinuxProcStat
    last: _LinuxProcStat
    value: Any
    terminal_row: _LinuxTerminalProcRow | None = None


class _LinuxProcDirectory:
    """Read one proc PID through a directory FD and, where available, pidfd."""

    def __init__(self, pid: int, proc_root: pathlib.Path = pathlib.Path("/proc")) -> None:
        self.pid = pid
        self.proc_path = proc_root / str(pid)
        self.directory_fd: int | None = None
        self.pidfd: int | None = None
        self.pidfd_open_error: ProcessLookupError | None = None
        self.directory_open_error: OSError | None = None
        self._closed = False

    def __enter__(self) -> "_LinuxProcDirectory":
        # Pin the kernel task before resolving /proc/PID. If it exits and the
        # number is reused before the directory open, pidfd readiness below
        # makes that mismatch a refusal instead of silently adopting a new PID.
        self.pin_pidfd()
        try:
            self.directory_fd = os.open(
                self.proc_path,
                os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0),
            )
        except OSError as error:
            if not _linux_proc_path_absence(error):
                self.close()
                raise
            # Keep the pidfd open so the capture layer can distinguish a
            # task that disappeared between enumeration and /proc open from
            # a live process whose path is simply unreadable.
            self.directory_open_error = error
        except BaseException:
            self.close()
            raise
        return self

    def __exit__(self, _kind: object, _value: object, _traceback: object) -> None:
        self.close()

    def pin_pidfd(self) -> None:
        if self.pidfd is not None or self.pidfd_open_error is not None:
            return
        opener = getattr(os, "pidfd_open", None)
        if opener is None:
            return
        try:
            self.pidfd = opener(self.pid, 0)
        except ProcessLookupError as error:
            if error.errno != errno.ESRCH:
                raise
            # The kernel says this PID is already absent. Keep that exact
            # result so the capture layer can require two fresh path checks;
            # do not retry pidfd_open and accidentally pin a replacement PID.
            self.pidfd_open_error = error
        except OSError as error:
            # Old kernels may not implement pidfds. Ordinary stable reads can
            # still use the pinned proc-directory identity; a later ENOENT
            # cannot be called an exit without a ready pidfd.
            if error.errno in {errno.ENOSYS, errno.EINVAL, getattr(errno, "EOPNOTSUPP", errno.EINVAL)}:
                return
            raise

    def pidfd_ready(self) -> bool | None:
        if self.pidfd is None:
            return None
        poller = select.poll()
        poller.register(self.pidfd, select.POLLIN | select.POLLHUP | select.POLLERR)
        events = poller.poll(0)
        for descriptor, event in events:
            if descriptor != self.pidfd:
                raise OSError(errno.EBADF, "pidfd-poll-unexpected-descriptor")
            if event & (select.POLLNVAL | select.POLLERR):
                raise OSError(errno.EBADF, "pidfd-poll-error")
        # A pidfd reports POLLIN while its task is zombie and may report only
        # POLLHUP after the task is reaped. Both are exact terminal readiness;
        # later proof checks this pidfd and, before identity, re-resolves the
        # current PID path twice to reject a replacement.
        return any(event & (select.POLLIN | select.POLLHUP) for _, event in events)

    def read_bytes(self, relative_path: str, limit: int) -> bytes:
        descriptor = os.open(
            relative_path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0),
            dir_fd=self._directory_fd(),
        )
        try:
            with os.fdopen(descriptor, "rb", closefd=False) as source:
                return source.read(limit + 1)
        finally:
            os.close(descriptor)

    def read_environ_bounded(self, limit: int) -> bytes:
        """Read the single whitelisted proc environment file, bounded and nofollow."""
        descriptor = os.open(
            "environ",
            os.O_RDONLY | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0),
            dir_fd=self._directory_fd(),
        )
        try:
            output = bytearray()
            with os.fdopen(descriptor, "rb", closefd=False) as source:
                while True:
                    chunk = source.read(min(4096, limit + 1 - len(output)))
                    if not chunk:
                        break
                    output.extend(chunk)
                    if len(output) > limit:
                        raise CensusError("linux-proc-file-limit")
            return bytes(output)
        finally:
            os.close(descriptor)

    def read_text(self, relative_path: str, limit: int = MAX_RESOURCE_OUTPUT_BYTES) -> str:
        raw = self.read_bytes(relative_path, limit)
        if len(raw) > limit:
            raise CensusError("linux-proc-file-limit")
        return raw.decode("ascii")

    def readlink(self, relative_path: str) -> str:
        return os.readlink(relative_path, dir_fd=self._directory_fd())

    def stat_path(self, relative_path: str) -> os.stat_result:
        return os.stat(relative_path, dir_fd=self._directory_fd(), follow_symlinks=True)

    def read_stat(self) -> _LinuxProcStat:
        return _parse_linux_proc_stat(self.read_text("stat", 4 * 1024))

    def read_current_stat(self) -> _LinuxProcStat:
        """Re-resolve /proc/PID, detecting a replacement after a pidfd exit."""
        with (self.proc_path / "stat").open("rb") as source:
            raw = source.read(4 * 1024 + 1)
        if len(raw) > 4 * 1024:
            raise CensusError("linux-proc-file-limit")
        return _parse_linux_proc_stat(raw.decode("ascii"))

    def _directory_fd(self) -> int:
        if self.directory_fd is None:
            raise OSError(errno.EBADF, "proc-directory-not-open")
        return self.directory_fd

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            if self.pidfd is not None:
                os.close(self.pidfd)
        finally:
            if self.directory_fd is not None:
                os.close(self.directory_fd)


def _parse_linux_proc_stat(text: str) -> _LinuxProcStat:
    close = text.rfind(")")
    if close < 0:
        raise ValueError("malformed-stat")
    fields = text[close + 1 :].strip().split()
    if len(fields) <= 19:
        raise ValueError("malformed-stat-fields")
    return _LinuxProcStat(
        state=fields[0],
        ppid=int(fields[1]),
        pgid=int(fields[2]),
        session=int(fields[3]),
        flags=int(fields[6]),
        thread_count=int(fields[17]),
        start_ticks=fields[19],
    )


def _capture_linux_proc_pid(
    pid: int,
    read_payload: Any,
    *,
    proc_factory: Any = _LinuxProcDirectory,
) -> _LinuxProcCapture:
    """Capture one proc row; tolerate only exits proved by its own pidfd.

    The pidfd is opened before resolving /proc/PID, then the directory identity
    is read twice before payload access. A stable terminal row is returned as
    a proc observation, not as a pidfd exit proof. Before a first identity is
    captured, a missing proc path is an exit only if the pinned pidfd is ready,
    or pidfd_open specifically returned ESRCH, and two fresh /proc/PID/stat
    reads are absent. Permission errors, live or replaced PID paths, and
    start-time changes remain hard races.
    """

    def prove_unidentified_exit(proc: Any, error: OSError) -> _LinuxProcessExitedDuringRead:
        # Without an initial /proc identity, pidfd readiness alone could name
        # an earlier PID incarnation. Require the current PID path to be
        # absent twice with the exact pidfd still terminal after each read.
        # If pidfd_open itself returned ESRCH, its kernel result plus repeated
        # path absence is the proof; a readable or inaccessible path still
        # refuses the snapshot.
        if not _linux_proc_path_absence(error):
            raise error
        pidfd_open_esrch = getattr(proc, "pidfd_open_error", None)
        pidfd_ready = proc.pidfd_ready()
        has_open_esrch = (
            isinstance(pidfd_open_esrch, ProcessLookupError)
            and pidfd_open_esrch.errno == errno.ESRCH
        )
        if pidfd_ready is not True and not (has_open_esrch and pidfd_ready is None):
            raise _LinuxProcRace(
                errno.ESTALE,
                "proc-path-missing-before-identity-without-pidfd-exit-proof",
                getattr(error, "filename", None),
            ) from error
        missing_filename = getattr(error, "filename", None)
        missing_path = _linux_proc_display_path(
            proc.pid, getattr(proc, "proc_path", None), missing_filename
        )
        for _ in range(2):
            try:
                proc.read_current_stat()
            except OSError as current_error:
                if not _linux_proc_path_absence(current_error):
                    raise
                current_ready = proc.pidfd_ready()
                if has_open_esrch and current_ready is not None:
                    raise _LinuxProcRace(
                        errno.ESTALE,
                        "pidfd-open-esrch-proof-changed-during-proc-absence-checks",
                        getattr(error, "filename", None),
                    ) from error
                if not has_open_esrch and current_ready is not True:
                    raise _LinuxProcRace(
                        errno.ESTALE,
                        "pidfd-exit-not-stable-across-proc-absence-checks",
                        getattr(error, "filename", None),
                    ) from error
                continue
            # Any reappearing PID, terminal or live, may be another
            # incarnation that was not represented by the admitted ps row.
            raise _LinuxProcRace(
                errno.ESTALE,
                "proc-path-reappeared-before-identity-was-captured",
                getattr(error, "filename", None),
            ) from error
        return _LinuxProcessExitedDuringRead(
            _LinuxProcExitProof(
                pid=proc.pid,
                first_start_ticks=None,
                last_start_ticks=None,
                first_state=None,
                last_state=None,
                pidfd_ready=None if has_open_esrch else True,
                missing_path=missing_path,
                missing_filename=missing_filename,
                identity_observed=False,
                evidence=(
                    "pidfd-open-esrch-and-current-proc-path-absent-twice"
                    if has_open_esrch
                    else "ready-pidfd-and-current-proc-path-absent-twice"
                ),
                pidfd_open_errno=errno.ESRCH if has_open_esrch else None,
            )
        )

    try:
        with proc_factory(pid) as proc:
            # Custom test adapters may not pin in __enter__; the real adapter
            # has already opened the pidfd before its /proc/PID directory.
            proc.pin_pidfd()
            pidfd_open_error = getattr(proc, "pidfd_open_error", None)
            if pidfd_open_error is not None:
                raise prove_unidentified_exit(proc, pidfd_open_error)
            directory_open_error = getattr(proc, "directory_open_error", None)
            if directory_open_error is not None:
                raise prove_unidentified_exit(proc, directory_open_error)
            try:
                first = proc.read_stat()
            except OSError as error:
                if not _linux_proc_path_absence(error):
                    raise
                raise prove_unidentified_exit(proc, error)
            pinned = proc.read_stat()
            if first.start_ticks != pinned.start_ticks:
                raise _LinuxProcRace(errno.ESTALE, "pid-start-changed-while-pinning")
            if first.state in _LINUX_TERMINAL_STATES and pinned.state not in _LINUX_TERMINAL_STATES:
                # A terminal proc task cannot become active again through this
                # held directory; don't reinterpret that identity as a new
                # live process even if its pidfd isn't readable yet.
                raise _LinuxProcRace(errno.ESTALE, "terminal-state-changed-while-pinning")
            if first.state in _LINUX_TERMINAL_STATES and pinned.state in _LINUX_TERMINAL_STATES:
                # The held proc directory and matching start ticks identify a
                # stable terminal row. Do not call this an exit of the task
                # behind pidfd: that descriptor may already refer to a reaped
                # task while this PID path names a later terminal task.
                terminal = _LinuxTerminalProcRow(
                    pid=pid,
                    start_ticks=pinned.start_ticks,
                    state=pinned.state,
                    ppid=pinned.ppid,
                    pgid=pinned.pgid,
                    session=pinned.session,
                )
                return _LinuxProcCapture(first=first, last=pinned, value=None, terminal_row=terminal)
            if proc.pidfd_ready() is True:
                # Since pidfd_open precedes the proc path lookup, an already
                # ready descriptor cannot be tied to this live stat row: the
                # PID may have been recycled between those operations.
                raise _LinuxProcRace(errno.ESTALE, "pidfd-ready-before-first-identity")

            def exited(
                missing_path: str | None,
                last: _LinuxProcStat | None = None,
            ) -> _LinuxProcessExitedDuringRead:
                proof = _LinuxProcExitProof(
                    pid=pid,
                    first_start_ticks=first.start_ticks,
                    last_start_ticks=last.start_ticks if last is not None else None,
                    first_state=first.state,
                    last_state=last.state if last is not None else None,
                    pidfd_ready=True,
                    missing_path=(
                        missing_path
                        if missing_path is None or str(missing_path).startswith("/proc/")
                        else f"/proc/{pid}/{missing_path}"
                    ),
                    missing_filename=missing_path,
                )
                return _LinuxProcessExitedDuringRead(proof)

            def prove_exit(error: OSError) -> _LinuxProcessExitedDuringRead:
                if not _linux_proc_path_absence(error):
                    raise error
                pidfd_ready = proc.pidfd_ready()
                if pidfd_ready is not True:
                    raise _LinuxProcRace(
                        errno.ESTALE,
                        "proc-path-missing-without-pidfd-exit-proof",
                        getattr(error, "filename", None),
                        start_ticks=first.start_ticks,
                        cause_errno=error.errno,
                        pidfd_ready=pidfd_ready,
                    ) from error
                try:
                    last = proc.read_stat()
                except OSError as last_error:
                    if not _linux_proc_path_absence(last_error):
                        raise
                    last = None
                if last is not None and last.start_ticks != first.start_ticks:
                    raise _LinuxProcRace(errno.ESTALE, "pid-start-changed-after-pidfd-exit") from error
                if last is not None and last.state not in _LINUX_TERMINAL_STATES:
                    raise _LinuxProcRace(errno.ESTALE, "pidfd-ready-for-active-proc-row") from error
                return exited(getattr(error, "filename", None), last)

            try:
                value = read_payload(proc, pinned)
                last = proc.read_stat()
            except OSError as error:
                if not _linux_proc_path_absence(error):
                    raise
                raise prove_exit(error)
            if last.start_ticks != first.start_ticks:
                raise _LinuxProcRace(errno.ESTALE, "pid-start-changed-during-proc-read")
            if proc.pidfd_ready() is True:
                if last.state not in _LINUX_TERMINAL_STATES:
                    try:
                        observed = proc.read_stat()
                    except OSError as observed_error:
                        if not _linux_proc_path_absence(observed_error):
                            raise
                        observed = None
                    if observed is not None and observed.start_ticks != first.start_ticks:
                        raise _LinuxProcRace(errno.ESTALE, "pid-start-changed-after-pidfd-exit")
                    if observed is not None and observed.state not in _LINUX_TERMINAL_STATES:
                        raise _LinuxProcRace(errno.ESTALE, "pidfd-ready-for-active-proc-row")
                    last = observed
                raise exited(None, last)
            return _LinuxProcCapture(first=first, last=last, value=value)
    except ProcessLookupError:
        # An injected adapter may raise before returning a context, leaving no
        # proc object on which to perform the repeated path checks. Keep that
        # observation incomplete instead of treating the exception as proof.
        raise


def _capture_gui_supervisor_process(
    pid: int,
    *,
    proc_factory: Any = _LinuxProcDirectory,
) -> tuple[dict[str, Any] | None, _LinuxProcExitProof | None]:
    """The GUI supervisor's exact status/ns/mnt/root observation, injectable in tests."""

    def read_fields(proc: Any, _stat: _LinuxProcStat) -> dict[str, Any]:
        uids = _parse_proc_status_uids(proc.read_text("status"))
        if len(uids) != 4:
            raise ValueError("gui-supervisor-uid-tuple-mismatch")
        mount_namespace = proc.stat_path("ns/mnt")
        root = proc.stat_path("root")
        return {
            "uids": list(uids),
            "ns": [mount_namespace.st_dev, mount_namespace.st_ino],
            "root": [root.st_dev, root.st_ino],
        }

    try:
        capture = _capture_linux_proc_pid(pid, read_fields, proc_factory=proc_factory)
    except _LinuxProcessExitedDuringRead as stopped:
        return None, stopped.proof
    if capture.terminal_row is not None:
        terminal = capture.terminal_row
        return {
            "pid": terminal.pid,
            "state": terminal.state,
            "ppid": terminal.ppid,
            "pgid": terminal.pgid,
            "sid": terminal.session,
            "start": int(terminal.start_ticks),
            "terminal_proc_observation": True,
        }, None
    row = {
        "pid": pid,
        "state": capture.last.state,
        "ppid": capture.last.ppid,
        "pgid": capture.last.pgid,
        "sid": capture.last.session,
        "start": int(capture.last.start_ticks),
        **capture.value,
    }
    return row, None


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
    environment_cargo_jobs: int | None = None


@dataclasses.dataclass(frozen=True, slots=True)
class _LinuxKernelThreadWithoutExecutable:
    start_ticks: str


@dataclasses.dataclass(frozen=True, slots=True)
class _LinuxExitingWithoutExecutable:
    """A sole exiting task with no mm, retained as occupied, never as dead."""

    stat: _LinuxProcStat
    uids: tuple[int, ...]


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
    issue_details: tuple[Mapping[str, Any], ...] = ()
    issue_details_omitted_count: int = 0


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


@dataclasses.dataclass(slots=True)
class _CargoEnvironmentBudget:
    """Bound optional Cargo environment captures across one process scan."""

    reads: int = 0

    def admit(self) -> bool:
        if self.reads >= MAX_CARGO_BUILD_ENVIRONMENT_READS:
            return False
        self.reads += 1
        return True


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


def _positive_job_count(value: str) -> int | None:
    """Parse a positive ASCII decimal Cargo job count without leaking errors."""
    if not value or not value.isascii() or not value.isdecimal():
        return None
    try:
        parsed = int(value, 10)
    except ValueError:
        return None
    return parsed if parsed > 0 else None


def _cargo_jobs_option_present(argv: tuple[str, ...] | None) -> bool:
    """Whether argv contains any jobs option, valid or malformed, before `--`."""
    if argv is None:
        return False
    for token in argv[1:]:
        if token == "--":
            return False
        if token in {"--jobs", "-j"} or token.startswith("--jobs="):
            return True
        if token.startswith("-j") and len(token) > 2:
            return True
    return False


def _cargo_build_jobs_from_environ(raw: bytes) -> int | None:
    """Extract only one positive CARGO_BUILD_JOBS value from bounded environ bytes."""
    if not raw or len(raw) > MAX_CARGO_BUILD_ENV_BYTES or raw[-1:] != b"\0":
        return None
    matches: list[bytes] = []
    for entry in raw[:-1].split(b"\0"):
        if not entry:
            return None
        name, separator, value = entry.partition(b"=")
        if not separator or not name:
            return None
        if name == b"CARGO_BUILD_JOBS":
            matches.append(value)
    if len(matches) != 1:
        return None
    try:
        text = matches[0].decode("ascii")
    except UnicodeDecodeError:
        return None
    count = _positive_job_count(text)
    if count is None or count > sys.maxsize:
        return None
    return count


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
    if len(values) != 1:
        return None
    return _positive_job_count(values[0])


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
    requested_jobs = _requested_cargo_jobs(record.argv)
    if record.argv is not None and not _cargo_jobs_option_present(record.argv):
        requested_jobs = record.environment_cargo_jobs
    return CargoEntry(record, classification, verb, reason, requested_jobs)


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
    # Cargo process. A non-Cargo PGID containing rustc processes has no
    # verified Cargo job limit or ancestry owner, so count each rustc
    # independently. If other active members share that PGID, retain one
    # additional residual slot for that unclassified work. Runtime-only and
    # other non-rustc groups continue to consume one residual slot per PGID.
    rustc_members = [
        member for member in members
        if member.compiler_candidate
        or (member.executable is not None and _is_rustc_executable(member.executable))
    ]
    residual_slots = (
        1 if unattributed else 0
    ) if cargo else max(1, len(rustc_members)) + int(bool(rustc_members) and len(members) > len(rustc_members))
    return {
        "admission_slot_count": len(cargo) + residual_slots if cargo else residual_slots,
        "admission_slot_provenance": {
            "cargo_root_pids": root_pids,
            "residual_slot_count": residual_slots,
            "residual_member_pids": unattributed if cargo else sorted({member.pid for member in members}),
            "rustc_member_pids": sorted(member.pid for member in rustc_members),
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
    issue_details: list[dict[str, Any]] = []
    if sys.platform.startswith("linux"):
        records, issues, races = _linux_processes(
            effective_uid,
            rows,
            argv_budget,
            issue_details=issue_details,
        )
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
        tuple(issue_details),
        max(0, sum(issues.values()) - len(issue_details)) if sys.platform.startswith("linux") else 0,
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
        # `ps` can include itself in its own table. This is the exact child
        # started above, and wait() has now reaped that child successfully. Its
        # row is therefore an expected transient observer, not an unrelated
        # PID that vanished while the proc snapshot was being captured.
        return [row for row in rows if row.pid != child.pid]
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


def _linux_exiting_witness_kernel_supported() -> bool:
    """Enable this source-derived proof only for the exact audited release.

    Suffixes, newer kernels and unavailable/malformed release data require a
    separate audit; they retain the existing strict executable refusal.
    """
    try:
        release = os.uname().release
    except (AttributeError, OSError):
        return False
    return type(release) is str and release == "6.18.38"


def _exiting_status_uids(status_text: str, row: _PsRow, stat: _LinuxProcStat) -> tuple[int, ...]:
    """Require one complete status payload tying no-mm to its sole task.

    Linux 6.18.38 proc_pid_status acquires mm before task_sig reads Threads
    under the task's sighand lock. A live sibling keeps that count >=2;
    releasing an old exec leader clears sighand with the same lock, so that
    old task reports 0, never a false sole-thread witness. Raw stat flags and
    num_threads alone are not an atomic pair across a non-leader exec.
    """
    if len(status_text) > MAX_RESOURCE_OUTPUT_BYTES or not status_text.endswith("\n") or "\0" in status_text:
        raise ValueError("exiting-status-incomplete-or-over-limit")
    fields: dict[str, str] = {}
    for line in status_text.splitlines():
        key, separator, value = line.partition(":")
        if not separator or re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key) is None or key in fields:
            raise ValueError("exiting-status-malformed-or-duplicate-field")
        fields[key] = value.strip()
    required = {
        "Name", "State", "Pid", "Tgid", "PPid", "Uid", "Kthread", "Threads",
        "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches",
    }
    if not required.issubset(fields):
        raise ValueError("exiting-status-required-field-missing")
    # Both MMU and NOMMU task_mem output, plus the mm-only status fields.
    # Zero memory values still prove an mm was present and are not absence.
    memory_fields = {"Mem", "Slack", "Shared", "HugetlbPages", "CoreDumping", "THP_enabled", "untag_mask"}
    if any(key.startswith(("Vm", "Rss")) or key in memory_fields for key in fields):
        raise ValueError("exiting-status-still-has-mm")

    def number(key: str) -> int:
        value = fields[key]
        if not value.isascii() or not value.isdecimal():
            raise ValueError("exiting-status-invalid-integer")
        return int(value)

    if any(not value.isascii() or not value.isdecimal() for value in fields["Uid"].split()):
        raise ValueError("exiting-status-invalid-uid")
    uids = _parse_proc_status_uids(status_text)
    if (
        len(uids) != 4
        or any(uid < 0 for uid in uids)
        or row.effective_uid is None
        or uids[1] != row.effective_uid
        or number("Pid") != row.pid
        or number("Tgid") != row.pid
        or number("PPid") != stat.ppid
        or number("Threads") != 1
        or number("Kthread") != 0
        or fields["State"].split()[0] != stat.state
        or stat.state in _LINUX_TERMINAL_STATES
        or stat.thread_count != 1
        or not (stat.flags & _LINUX_PF_EXITING)
        or stat.flags & _LINUX_PF_KTHREAD
        or (row.ppid, row.pgid) != (stat.ppid, stat.pgid)
    ):
        raise ValueError("exiting-status-identity-or-task-state-mismatch")
    # Require the ordinary tail to be present and well formed, rather than
    # interpreting a truncated status prefix as evidence that mm fields omit.
    number("voluntary_ctxt_switches")
    number("nonvoluntary_ctxt_switches")
    return uids


def _linux_process_record(
    row: _PsRow,
    boot_id: str,
    argv_budget: _ArgvBudget,
    *,
    proc_factory: Any = _LinuxProcDirectory,
    environment_budget: _CargoEnvironmentBudget | None = None,
) -> ProcessRecord | _LinuxKernelThreadWithoutExecutable | _LinuxTerminalProcRow | None:
    if environment_budget is None:
        environment_budget = _CargoEnvironmentBudget()

    def read_process(
        proc: _LinuxProcDirectory, stat: _LinuxProcStat
    ) -> dict[str, Any] | _LinuxExitingWithoutExecutable | None:
        status_text = proc.read_text("status")
        process_uids = _parse_proc_status_uids(status_text)
        if row.effective_uid is not None and process_uids[1] != row.effective_uid:
            # PID exited/reused after ps observed it; do not trust its old ucomm row.
            return None
        try:
            executable = proc.readlink("exe")
        except FileNotFoundError as missing:
            if stat.flags & _LINUX_PF_KTHREAD:
                return {
                    "kernel_thread_without_executable": True,
                    "start_ticks": stat.start_ticks,
                    "stat_flags": stat.flags,
                }
            if (
                missing.errno == errno.ENOENT
                and stat.flags & _LINUX_PF_EXITING
                and _linux_exiting_witness_kernel_supported()
            ):
                if proc.pidfd_ready() is not False:
                    raise missing
                try:
                    before_uids = _exiting_status_uids(status_text, row, stat)
                    after_uids = _exiting_status_uids(proc.read_text("status"), row, stat)
                except (ValueError, IndexError):
                    # Missing proof retains the original strict exe-absence
                    # refusal and its diagnostics; this is never an exit.
                    raise missing
                if before_uids == after_uids and proc.pidfd_ready() is False:
                    return _LinuxExitingWithoutExecutable(stat, before_uids)
            raise
        if _is_cargo_executable(executable):
            environment_cargo_jobs = None
            if argv_budget.exhausted:
                argv = None
                argv_error = "snapshot-argv-budget-exhausted"
            else:
                raw_argv = proc.read_bytes("cmdline", MAX_ARG_BYTES)
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
                        if (
                            not _cargo_jobs_option_present(argv)
                            and environment_budget.admit()
                        ):
                            read_environment = getattr(proc, "read_environ_bounded", None)
                            if callable(read_environment):
                                try:
                                    environment = read_environment(MAX_CARGO_BUILD_ENV_BYTES)
                                except (OSError, CensusError):
                                    # Environment availability is optional and
                                    # may never make an otherwise readable
                                    # Cargo process disappear from the census.
                                    pass
                                else:
                                    environment_cargo_jobs = _cargo_build_jobs_from_environ(
                                        environment
                                    )
        else:
            if not _is_rustc_executable(executable) and not _is_runtime_owner_executable(executable):
                executable = None
            argv = None
            argv_error = None
            environment_cargo_jobs = None
        return {
            "ppid": stat.ppid,
            "pgid": stat.pgid,
            "start_ticks": stat.start_ticks,
            "state": stat.state,
            "executable": executable,
            "argv": argv,
            "argv_error": argv_error,
            "environment_cargo_jobs": environment_cargo_jobs,
        }

    capture = _capture_linux_proc_pid(row.pid, read_process, proc_factory=proc_factory)
    if capture.terminal_row is not None:
        return capture.terminal_row
    if capture.value is None:
        return None
    details = capture.value
    if isinstance(details, _LinuxExitingWithoutExecutable):
        if any(
            sample != details.stat
            or sample.thread_count != 1
            or not (sample.flags & _LINUX_PF_EXITING)
            or sample.flags & _LINUX_PF_KTHREAD
            for sample in (capture.first, capture.last)
        ):
            raise _LinuxProcRace(errno.ESTALE, "exiting-without-executable-identity-not-stable")
        hint = pathlib.PurePath(row.ucomm).name
        cargo_hint = hint in {"cargo", "cargo.exe"}
        compiler_hint = hint in {"rustc", "rustc.exe"}
        runtime_hint = _is_runtime_owner_executable(hint)
        return ProcessRecord(
            pid=row.pid, ppid=details.stat.ppid, pgid=details.stat.pgid,
            start_token=f"linux:{boot_id}:{details.stat.start_ticks}",
            executable=None, argv=None, argv_error="sole-exiting-task-without-mm-or-executable",
            state=details.stat.state, identity_validated=True,
            comm_candidate=cargo_hint, compiler_candidate=compiler_hint,
            runtime_owner_candidate=runtime_hint,
            unknown_executable_candidate=not (cargo_hint or compiler_hint or runtime_hint),
        )
    if details.get("kernel_thread_without_executable"):
        if (
            details["start_ticks"] != capture.first.start_ticks
            or details["start_ticks"] != capture.last.start_ticks
            or not (capture.first.flags & _LINUX_PF_KTHREAD)
            or not (details["stat_flags"] & _LINUX_PF_KTHREAD)
            or not (capture.last.flags & _LINUX_PF_KTHREAD)
        ):
            raise _LinuxProcRace(errno.ESTALE, "kernel-thread-identity-not-stable")
        return _LinuxKernelThreadWithoutExecutable(details["start_ticks"])
    return ProcessRecord(
        pid=row.pid,
        ppid=details["ppid"],
        pgid=details["pgid"],
        start_token=f"linux:{boot_id}:{details['start_ticks']}",
        executable=details["executable"],
        argv=details["argv"],
        argv_error=details["argv_error"],
        state=details["state"],
        environment_cargo_jobs=details["environment_cargo_jobs"],
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


def _linux_process_issue_detail(
    row: _PsRow,
    boot_id: str,
    category: str,
    error: BaseException,
) -> dict[str, Any]:
    filename = getattr(error, "filename", None)
    basename: str | None = None
    if isinstance(filename, (str, bytes, os.PathLike)):
        basename = pathlib.PurePosixPath(os.fsdecode(filename)).name
    path_classes = {
        "cmdline": ("read-command-line", "proc-command-line"),
        "exe": ("read-executable-link", "proc-executable-link"),
        "stat": ("read-process-stat", "proc-stat"),
        "status": ("read-process-status", "proc-status"),
    }
    if basename is None:
        stage, path_class = "capture-process-record", None
    else:
        stage, path_class = path_classes.get(basename, ("capture-process-record", "other-proc-path"))
    start_ticks = getattr(error, "start_ticks", None)
    start_token = None
    if start_ticks is not None and str(start_ticks).isdigit():
        start_token = f"linux:{boot_id}:{start_ticks}"
    cause = error.__cause__
    cause_errno = getattr(error, "cause_errno", None)
    if cause_errno is None:
        cause_errno = getattr(cause, "errno", None)
    reason = getattr(error, "strerror", None) or "non-os-error"
    return {
        "category": category,
        "pid": row.pid,
        "ps_started": row.started,
        "start_token": start_token,
        "stage": stage,
        "path_class": path_class,
        "error_kind": type(error).__name__,
        "errno": getattr(error, "errno", None),
        "errno_name": errno.errorcode.get(getattr(error, "errno", None)),
        "cause_kind": type(cause).__name__ if isinstance(cause, OSError) else None,
        "cause_errno": cause_errno,
        "cause_errno_name": errno.errorcode.get(cause_errno),
        "pidfd_ready": getattr(error, "pidfd_ready", None),
        "reason": str(reason)[:160],
    }


def _linux_processes(
    effective_uid: int,
    rows: list[_PsRow],
    argv_budget: _ArgvBudget,
    *,
    proc_root: pathlib.Path = pathlib.Path("/proc"),
    proc_factory: Any = _LinuxProcDirectory,
    issue_details: list[dict[str, Any]] | None = None,
) -> tuple[dict[int, ProcessRecord], dict[str, int], dict[str, int]]:
    try:
        boot_id = (proc_root / "sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
    except OSError as error:
        raise CensusError(f"linux-proc-unavailable:{type(error).__name__}") from error
    records: dict[int, ProcessRecord] = {}
    issues: dict[str, int] = {}
    races: dict[str, int] = {}
    environment_budget = _CargoEnvironmentBudget()

    def record_issue(category: str, row: _PsRow, error: BaseException) -> None:
        issues[category] = issues.get(category, 0) + 1
        if issue_details is not None and len(issue_details) < MAX_REPORTED_PROCESS_ISSUES:
            issue_details.append(_linux_process_issue_detail(row, boot_id, category, error))

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
            record = _linux_process_record(
                row,
                boot_id,
                argv_budget,
                proc_factory=proc_factory,
                environment_budget=environment_budget,
            )
            if isinstance(record, (_LinuxKernelThreadWithoutExecutable, _LinuxTerminalProcRow)):
                continue
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
        except _LinuxProcessExitedDuringRead as stopped:
            if stopped.proof.identity_observed:
                race = "pid-exited-during-scan"
            elif stopped.proof.pidfd_open_errno == errno.ESRCH:
                race = "pidfd-open-esrch-and-proc-absent-twice"
            else:
                race = "pid-exited-before-identity"
            races[race] = races.get(race, 0) + 1
        except FileNotFoundError as error:
            if cargo_hint:
                records[row.pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[row.pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[row.pid] = _unknown_runtime_owner_candidate(row)
            elif same_uid:
                record_issue("same-uid-process-snapshot-incomplete", row, error)
        except PermissionError as error:
            if same_uid and (cargo_hint or compiler_hint or runtime_hint):
                records[row.pid] = (
                    _unknown_cargo_candidate(row)
                    if cargo_hint
                    else _unknown_compiler_candidate(row)
                    if compiler_hint
                    else _unknown_runtime_owner_candidate(row)
                )
            elif same_uid:
                record_issue("same-uid-process-unreadable", row, error)
            elif cargo_hint:
                records[row.pid] = _unknown_cargo_candidate(row)
            elif compiler_hint:
                records[row.pid] = _unknown_compiler_candidate(row)
            elif runtime_hint:
                records[row.pid] = _unknown_runtime_owner_candidate(row)
        except (OSError, ValueError, IndexError) as error:
            if same_uid and (cargo_hint or compiler_hint or runtime_hint):
                records[row.pid] = (
                    _unknown_cargo_candidate(row)
                    if cargo_hint
                    else _unknown_compiler_candidate(row)
                    if compiler_hint
                    else _unknown_runtime_owner_candidate(row)
                )
            elif same_uid:
                record_issue("same-uid-process-snapshot-incomplete", row, error)
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


def _disk_volume_snapshot(path: pathlib.Path) -> dict[str, int]:
    """Bind the free-space observation to one canonical directory/device."""
    before = path.lstat()
    if not stat.S_ISDIR(before.st_mode) or path.resolve(strict=True) != path:
        raise ValueError(f'disk path is not a canonical nonsymlink directory: {path}')
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        opened = os.fstat(fd)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise ValueError(f'disk path changed before open: {path}')
        usage = os.fstatvfs(fd)
        after = path.lstat()
        if (after.st_dev, after.st_ino) != (opened.st_dev, opened.st_ino) or path.resolve(strict=True) != path:
            raise ValueError(f'disk path changed during observation: {path}')
        unit = usage.f_frsize
        return dict(device=opened.st_dev, available_bytes=usage.f_bavail * unit,
                    total_bytes=usage.f_blocks * unit, used_bytes=(usage.f_blocks - usage.f_bfree) * unit)
    finally:
        os.close(fd)


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
        disk = _disk_volume_snapshot(pathlib.Path('/'))
    except (OSError, ValueError, RuntimeError) as error:
        raise CensusError(f"root-disk-usage-unavailable:{type(error).__name__}") from error
    reserve_file = pathlib.Path('/etc/nudox-ci-disk-reserve-gib')
    reserve_bytes = 0
    try:
        reserve_file.lstat()
    except FileNotFoundError:
        pass
    except OSError as error:
        raise CensusError(f'root-disk-reserve-invalid:{type(error).__name__}') from error
    else:
        try:
            reserve_gib = int(reserve_file.read_text().strip())
            if reserve_gib < 1:
                raise ValueError('reserve must be positive')
            reserve_bytes = reserve_gib * 1024 ** 3
        except (OSError, ValueError) as error:
            raise CensusError(f'root-disk-reserve-invalid:{type(error).__name__}') from error
    ci_path = pathlib.Path('/srv/ci')
    try:
        # resolve(strict=False) also rejects a missing leaf under a symlinked
        # parent; only a genuinely absent literal path has the legacy fallback.
        if ci_path.resolve(strict=False) != ci_path:
            raise ValueError('CI disk path has a symlink component')
        try:
            ci_path.lstat()
        except FileNotFoundError:
            if ci_path.resolve(strict=False) != ci_path:
                raise ValueError('absent CI disk path changed to a symlink component')
            try:
                ci_path.lstat()
            except FileNotFoundError:
                pass
            else:
                raise ValueError('CI disk path appeared during observation')
            ci_path = pathlib.Path('/')
            ci_disk = disk
        else:
            ci_disk = _disk_volume_snapshot(ci_path)
    except (OSError, ValueError, RuntimeError) as error:
        raise CensusError(f'ci-disk-usage-invalid:{type(error).__name__}') from error
    return {
        "sampled_at_utc": sampled_at,
        "available_memory_bytes": available_bytes,
        "available_memory_source": memory_source,
        "root_disk_available_bytes": disk['available_bytes'],
        "root_disk_reserved_bytes": reserve_bytes if ci_disk['device'] == disk['device'] else 0,
        "root_disk_total_bytes": disk['total_bytes'],
        "root_disk_used_bytes": disk['used_bytes'],
        "root_disk_device": disk['device'],
        "ci_disk_path": str(ci_path),
        "ci_disk_device": ci_disk['device'],
        "ci_disk_available_bytes": ci_disk['available_bytes'],
        "ci_disk_reserved_bytes": reserve_bytes,
        "ci_disk_reserve_file": str(reserve_file),
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
    if sys.platform.startswith("linux"):
        output["snapshot_issue_details"] = [dict(detail) for detail in snapshot.issue_details]
        output["snapshot_issue_details_omitted_count"] = snapshot.issue_details_omitted_count
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
