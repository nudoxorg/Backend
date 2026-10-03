#!/usr/bin/env python3
"""Bounded live registry-discovery, storage-closure, and restore canary.

The runner requires an explicit measurement-slot grant and prebuilt locald,
CLI, BLAKE3, and Turso executables. It never builds and never runs package
`add` or `index` build commands. It uses `index-search` only for retrieval.
Registry traffic is limited to the configured metadata discovery feeds; the
one forge request is pinned to a GitHub tag resolved to an exact commit before
``forge-add``. Independent OSV point queries are saved as reference evidence;
the owner has advisory acquisition disabled and does not ingest those replies.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import http.client
import json
import math
import os
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


MAX_BUILD_MANIFEST_BYTES = 1024 * 1024
MAX_LOCK_BYTES = 16 * 1024 * 1024
MAX_RUNNER_BYTES = 2 * 1024 * 1024
MAX_JOURNAL_BYTES = 1024 * 1024 * 1024
MAX_FRAME_BYTES = 32 * 1024 * 1024
MAX_RECENT_PAGE_BYTES = 16 * 1024 * 1024
MAX_SPARSE_BYTES = 16 * 1024 * 1024
MAX_NPM_BYTES = 32 * 1024 * 1024
MAX_PYPI_BYTES = 32 * 1024 * 1024
MAX_OSV_BYTES = 2 * 1024 * 1024
MAX_OSV_PAGES = 4
MAX_GITHUB_BYTES = 4 * 1024 * 1024
MAX_CLI_BYTES = 64 * 1024 * 1024
MAX_DB_DUMP_BYTES = 128 * 1024 * 1024
MAX_BACKUP_BYTES = 8 * 1024 * 1024 * 1024
MAX_DATABASE_COUNT = 32
MAX_WORKSPACE_ENTRIES = 200_000
MAX_TOTAL_DATABASE_DUMP_BYTES = 1024 * 1024 * 1024
MAX_TOTAL_SOURCE_BYTES = 128 * 1024 * 1024
MAX_SOURCE_REQUESTS = 160
MAX_SEARCH_PAGES = 64
MAX_CLI_OPERATIONS = 4096
MAX_OWNER_BATCH_FACTS = 4096
MAX_SOURCE_HASH_OPERATIONS = 4096 + 4096 + 2 * 256
MAX_B3_INPUT_BYTES = 32 * 1024 * 1024 + 128
MAX_B3_TOTAL_SOURCE_BYTES = 256 * 1024 * 1024
MAX_JOURNAL_TRANSACTIONS = 4096
MAX_B3_TIMEOUT_SECONDS = 10.0
MAX_STDERR_BYTES = 4 * 1024 * 1024
MAX_OWNER_LOG_BYTES = 64 * 1024 * 1024
MAX_PROCESS_DRAIN_SECONDS = 3.0
MAX_PROCESS_TIMEOUT_SECONDS = 3600.0
PROCESS_READ_CHUNK_BYTES = 64 * 1024
MAX_PROCESS_CENSUS_BYTES = 16 * 1024 * 1024
MAX_PROCESS_CENSUS_ROWS = 32768
BUILD_PROCESS_NAMES = (
    "cargo", "rustc", "rustdoc", "cc", "gcc", "g++", "clang", "clang++",
    "cc1", "cc1plus", "ld", "lld", "link", "cmake", "make", "ninja",
    "sccache", "meson", "bazel", "buck2",
)
MAX_OWNER_RUNTIME_SECONDS = 3600.0
MAX_CLI_TIMEOUT_SECONDS = 120.0
MAX_SOURCE_TIMEOUT_SECONDS = 30.0
MAX_SOURCE_BUDGET_SECONDS = 300.0
MAX_INGEST_WAIT_SECONDS = 180.0
MAX_INGEST_POLL_SECONDS = 10.0
MAX_CANARY_REPETITIONS = 8
MAX_NPM_VERSIONS_PER_PACKAGE = 4096
DISCOVERY_MAGIC = b"DISCOV01"
DISCOVERY_DOMAIN = b"backend.registry.discovery.transaction.v1\0"
DEFAULT_FORGE_REPOSITORY = "json-c/json-c"
DEFAULT_FORGE_TAG = "json-c-0.18-20240915"
DISCOVERY_SOURCES = (
    ("cargo", "https://crates.io"),
    ("npm", "https://replicate.npmjs.com/registry"),
    ("pypi", "https://pypi.org"),
)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="milliseconds")


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_regular_file(path: Path, maximum: int, label: str) -> bytes:
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        raise ValueError(f"{label} must be a single-link regular file: {path}")
    if before.st_size > maximum:
        raise ValueError(f"{label} exceeds the {maximum}-byte limit: {path}")
    with path.open("rb") as stream:
        payload = stream.read(maximum + 1)
    after = path.lstat()
    if (
        len(payload) != before.st_size
        or len(payload) > maximum
        or (before.st_dev, before.st_ino, before.st_size)
        != (after.st_dev, after.st_ino, after.st_size)
        or not stat.S_ISREG(after.st_mode)
        or after.st_nlink != 1
    ):
        raise ValueError(f"{label} changed during its bounded read: {path}")
    return payload


def write_new(path: Path, payload: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, mode)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            path.unlink()
        except OSError:
            pass
        raise


def write_json_new(path: Path, value: Any) -> None:
    payload = (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()
    write_new(path, payload)


class BoundedProcessError(RuntimeError):
    def __init__(self, message: str, receipt: dict[str, Any]) -> None:
        super().__init__(message)
        self.receipt = receipt


class BoundedProcess:
    """Drain a process's two pipes to bounded files inside an owned session."""

    def __init__(
        self,
        command: list[str],
        *,
        stdout_path: Path,
        stderr_path: Path,
        receipt_path: Path,
        max_stdout_bytes: int,
        max_stderr_bytes: int,
        timeout_seconds: float,
        cwd: Path | None = None,
        env: dict[str, str] | None = None,
        input_data: bytes | None = None,
    ) -> None:
        if not math.isfinite(timeout_seconds) or not 0 < timeout_seconds <= MAX_PROCESS_TIMEOUT_SECONDS:
            raise ValueError("bounded process timeout is outside its finite limit")
        if max_stdout_bytes < 0 or max_stderr_bytes < 0:
            raise ValueError("bounded process output limits must be non-negative")
        self.command = command
        self.stdout_path = stdout_path
        self.stderr_path = stderr_path
        self.receipt_path = receipt_path
        self.maxima = {"stdout": max_stdout_bytes, "stderr": max_stderr_bytes}
        self.timeout_seconds = timeout_seconds
        self.started_at_utc = utc_now()
        self.started_ns = time.perf_counter_ns()
        self.input_bytes = len(input_data) if input_data is not None else 0
        self.output_bytes = {"stdout": 0, "stderr": 0}
        self.stored_bytes = {"stdout": 0, "stderr": 0}
        self.failure_reason: str | None = None
        self._lock = threading.Lock()
        self._finalizing = False
        self._finished = False
        self._receipt: dict[str, Any] | None = None
        for path in (stdout_path, stderr_path, receipt_path):
            path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            if os.path.lexists(path):
                raise ValueError(f"bounded process evidence path already exists: {path}")
        write_new(stdout_path, b"")
        write_new(stderr_path, b"")
        try:
            self.process = subprocess.Popen(
                command,
                cwd=cwd,
                env=env,
                stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
        except BaseException as error:
            receipt = {
                "argv": command,
                "started_at_utc": self.started_at_utc,
                "elapsed_ns": time.perf_counter_ns() - self.started_ns,
                "exit_code": None,
                "start_error": f"{type(error).__name__}: {error}",
                "stdout_path": str(stdout_path),
                "stderr_path": str(stderr_path),
            }
            write_json_new(receipt_path, receipt)
            raise BoundedProcessError(f"could not start owned process; receipt: {receipt_path}", receipt) from error

        self.pid = self.process.pid
        self._reader_threads = [
            threading.Thread(target=self._drain, args=("stdout", self.process.stdout), daemon=True),
            threading.Thread(target=self._drain, args=("stderr", self.process.stderr), daemon=True),
        ]
        for thread in self._reader_threads:
            thread.start()
        self._writer_thread: threading.Thread | None = None
        if input_data is not None:
            self._writer_thread = threading.Thread(
                target=self._write_input, args=(input_data,), daemon=True
            )
            self._writer_thread.start()
        self._timer = threading.Timer(timeout_seconds, self._on_timeout)
        self._timer.daemon = True
        self._timer.start()

    @property
    def returncode(self) -> int | None:
        return self.process.poll()

    def poll(self) -> int | None:
        return self.process.poll()

    def send_signal(self, signum: int) -> None:
        if self.process.poll() is not None:
            return
        self.process.send_signal(signum)

    def terminate_owned_group(self) -> None:
        try:
            os.killpg(self.pid, signal.SIGKILL)
        except (AttributeError, ProcessLookupError, PermissionError):
            if self.process.poll() is None:
                self.process.kill()

    def _fail_and_kill(self, reason: str) -> None:
        with self._lock:
            if self._finalizing:
                return
            if self.failure_reason is None:
                self.failure_reason = reason
        self.terminate_owned_group()

    def _on_timeout(self) -> None:
        self._fail_and_kill(f"wall-time limit exceeded ({self.timeout_seconds:g}s)")

    def _drain(self, name: str, pipe: Any) -> None:
        path = self.stdout_path if name == "stdout" else self.stderr_path
        try:
            with path.open("ab", buffering=0) as output:
                while True:
                    block = pipe.read(PROCESS_READ_CHUNK_BYTES)
                    if not block:
                        break
                    with self._lock:
                        if self._finalizing:
                            continue
                        self.output_bytes[name] += len(block)
                        remaining = max(0, self.maxima[name] - self.stored_bytes[name])
                        kept = block[:remaining]
                        if kept:
                            output.write(kept)
                            self.stored_bytes[name] += len(kept)
                        exceeded = self.output_bytes[name] > self.maxima[name]
                    if exceeded:
                        self._fail_and_kill(f"{name} byte limit exceeded ({self.maxima[name]} bytes)")
        except (OSError, ValueError) as error:
            self._fail_and_kill(f"failed draining {name}: {error}")
        finally:
            try:
                pipe.close()
            except OSError:
                pass

    def _write_input(self, input_data: bytes) -> None:
        assert self.process.stdin is not None
        try:
            view = memoryview(input_data)
            for offset in range(0, len(view), PROCESS_READ_CHUNK_BYTES):
                self.process.stdin.write(view[offset : offset + PROCESS_READ_CHUNK_BYTES])
            self.process.stdin.flush()
        except (BrokenPipeError, OSError):
            pass
        finally:
            try:
                self.process.stdin.close()
            except OSError:
                pass

    def wait(
        self, timeout: float | None = None, *, force_on_timeout: bool = False
    ) -> dict[str, Any]:
        if self._finished:
            assert self._receipt is not None
            if self.failure_reason is not None:
                raise BoundedProcessError(
                    f"{self.failure_reason}; receipt: {self.receipt_path}", self._receipt
                )
            return self._receipt
        wait_limit = timeout if timeout is not None else self.timeout_seconds + MAX_PROCESS_DRAIN_SECONDS
        try:
            self.process.wait(timeout=wait_limit)
        except subprocess.TimeoutExpired:
            if timeout is not None and not force_on_timeout:
                raise
            self._fail_and_kill("owned process did not exit by its wall-time deadline")
            try:
                self.process.wait(timeout=MAX_PROCESS_DRAIN_SECONDS)
            except subprocess.TimeoutExpired:
                with self._lock:
                    if self.failure_reason is None:
                        self.failure_reason = "owned process remained alive after group kill"
        for thread in self._reader_threads:
            thread.join(MAX_PROCESS_DRAIN_SECONDS)
        if self._writer_thread is not None:
            self._writer_thread.join(MAX_PROCESS_DRAIN_SECONDS)
        if any(thread.is_alive() for thread in self._reader_threads):
            self._fail_and_kill("owned child kept an output pipe open after its parent exited")
            for thread in self._reader_threads:
                thread.join(MAX_PROCESS_DRAIN_SECONDS)
        drain_threads_incomplete = any(thread.is_alive() for thread in self._reader_threads)
        if drain_threads_incomplete:
            with self._lock:
                self._finalizing = True
                if self.failure_reason is None:
                    self.failure_reason = "output drain did not close before the evidence deadline"
            for pipe in (self.process.stdout, self.process.stderr):
                try:
                    pipe.close()
                except (AttributeError, OSError):
                    pass
            for thread in self._reader_threads:
                thread.join(0.1)
        if self._writer_thread is not None and self._writer_thread.is_alive():
            self._fail_and_kill("owned child did not close stdin before the drain deadline")
            self._writer_thread.join(MAX_PROCESS_DRAIN_SECONDS)
        self._timer.cancel()
        with self._lock:
            receipt = {
                "argv": self.command,
                "pid": self.pid,
                "process_group": self.pid,
                "isolated_process_group": True,
                "started_at_utc": self.started_at_utc,
                "elapsed_ns": time.perf_counter_ns() - self.started_ns,
                "exit_code": self.process.poll(),
                "failure_reason": self.failure_reason,
                "input_bytes": self.input_bytes,
                "stdout_path": str(self.stdout_path),
                "stderr_path": str(self.stderr_path),
                "stdout_bytes_observed": self.output_bytes["stdout"],
                "stdout_bytes_stored": self.stored_bytes["stdout"],
                "stdout_limit_bytes": self.maxima["stdout"],
                "stderr_bytes_observed": self.output_bytes["stderr"],
                "stderr_bytes_stored": self.stored_bytes["stderr"],
                "stderr_limit_bytes": self.maxima["stderr"],
                "drain_threads_stopped": not drain_threads_incomplete,
                "stdout_sha256": sha256_file(self.stdout_path),
                "stderr_sha256": sha256_file(self.stderr_path),
                "receipt_path": str(self.receipt_path),
            }
            write_json_new(self.receipt_path, receipt)
        self._receipt = receipt
        self._finished = True
        if self.failure_reason is not None:
            raise BoundedProcessError(
                f"{self.failure_reason}; partial output and receipt: {self.receipt_path}", receipt
            )
        return receipt


def run_bounded_process(
    command: list[str],
    *,
    stdout_path: Path,
    stderr_path: Path,
    receipt_path: Path,
    max_stdout_bytes: int,
    max_stderr_bytes: int,
    timeout_seconds: float,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    input_data: bytes | None = None,
    check: bool = True,
) -> dict[str, Any]:
    process = BoundedProcess(
        command,
        stdout_path=stdout_path,
        stderr_path=stderr_path,
        receipt_path=receipt_path,
        max_stdout_bytes=max_stdout_bytes,
        max_stderr_bytes=max_stderr_bytes,
        timeout_seconds=timeout_seconds,
        cwd=cwd,
        env=env,
        input_data=input_data,
    )
    receipt = process.wait()
    if check and receipt["exit_code"] != 0:
        raise BoundedProcessError(
            f"process exited {receipt['exit_code']}; receipt: {receipt_path}", receipt
        )
    return receipt


def executable_snapshot(path: Path) -> dict[str, Any]:
    resolved = path.resolve(strict=True)
    before = resolved.lstat()
    if not stat.S_ISREG(before.st_mode) or not os.access(resolved, os.X_OK):
        raise ValueError(f"not a runnable regular executable: {resolved}")
    digest = sha256_file(resolved)
    after = resolved.lstat()
    if (before.st_dev, before.st_ino, before.st_size) != (
        after.st_dev,
        after.st_ino,
        after.st_size,
    ):
        raise ValueError(f"executable changed while hashing: {resolved}")
    return {"path": str(resolved), "sha256": digest, "bytes": after.st_size}


def minimal_environment(home: Path, endpoint: Path) -> dict[str, str]:
    """Keep proxy/TLS/runtime essentials; remove inherited product policy/secrets."""
    allowed = {
        "PATH", "HOME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TZ",
        "SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE",
        "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
        "http_proxy", "https_proxy", "all_proxy", "no_proxy",
        "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH",
    }
    environment = {key: value for key, value in os.environ.items() if key in allowed}
    environment["HOME"] = str(home)
    environment["BACKEND_LOCALD_BIN"] = str(endpoint.parent / "do-not-autostart-locald")
    return environment


def normalize_pypi_name(value: str) -> str:
    return re.sub(r"[-_.]+", "-", value).lower()


def package_purl(ecosystem: str, name: str, version: str) -> str:
    if ecosystem == "cargo":
        return f"pkg:cargo/{name.lower()}@{version}"
    if ecosystem == "npm":
        return f"pkg:npm/{urllib.parse.quote(name, safe='/@')}@{version}"
    if ecosystem == "pypi":
        return f"pkg:pypi/{normalize_pypi_name(name)}@{version}"
    raise ValueError(f"unsupported source ecosystem: {ecosystem}")


def canonical_json_bytes(value: Any) -> bytes:
    # Discovery proofs serialize serde_json::Value's sorted maps without spaces.
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
        allow_nan=False,
    ).encode("utf-8")


def unsigned_source_sequence(value: Any, label: str) -> int:
    if type(value) is int and value >= 0:
        return value
    if isinstance(value, str) and value.isascii() and value.isdecimal():
        return int(value)
    raise ValueError(f"{label} is not a non-negative integer sequence")


def parse_osv_query_page(value: Any) -> tuple[list[dict[str, Any]], str | None]:
    """Admit OSV's optional empty `vulns` page and explicit pagination shape."""
    if not isinstance(value, dict):
        raise ValueError("OSV response must be an object")
    if "vulns" in value:
        vulnerabilities = value["vulns"]
        if not isinstance(vulnerabilities, list) or any(
            not isinstance(row, dict)
            or not isinstance(row.get("id"), str)
            or not row["id"].strip()
            for row in vulnerabilities
        ):
            raise ValueError("OSV vulns field must be a list of records with non-empty IDs")
    else:
        vulnerabilities = []
    token = value.get("next_page_token")
    if "next_page_token" in value and (not isinstance(token, str) or not token):
        raise ValueError("OSV next_page_token must be a non-empty string when present")
    return vulnerabilities, token


def discovery_journal_content_signature(summary: dict[str, Any]) -> dict[str, Any]:
    """Compare journal content while excluding only its workspace-specific path."""
    return {key: value for key, value in summary.items() if key != "path"}


def download_zero_was_not_manufactured(target: dict[str, Any], projected: Any) -> bool:
    projected_zero = (
        isinstance(projected, dict)
        and projected.get("state") == "known"
        and type(projected.get("value")) is int
        and projected.get("value") == 0
    )
    source_exact_zero = (
        target.get("downloads_source_state") == "known-count"
        and target.get("downloads_source_scope") == "per-release"
        and type(target.get("downloads_source_raw")) is int
        and target.get("downloads_source_raw") == 0
    )
    return not projected_zero or source_exact_zero


class HashBudget:
    def __init__(self, max_operations: int, max_bytes: int) -> None:
        self.max_operations = max_operations
        self.max_bytes = max_bytes
        self.operations = 0
        self.bytes = 0

    def admit(self, payload: bytes) -> None:
        if len(payload) > MAX_B3_INPUT_BYTES:
            raise ValueError(f"one BLAKE3 input exceeds {MAX_B3_INPUT_BYTES} bytes")
        if self.operations + 1 > self.max_operations or self.bytes + len(payload) > self.max_bytes:
            raise ValueError("BLAKE3 operation or byte budget exceeded")
        self.operations += 1
        self.bytes += len(payload)


class CliOperationBudget:
    """One shared cap for all live and restore CLI subprocesses."""

    def __init__(self, maximum: int) -> None:
        if type(maximum) is not int or not 1 <= maximum <= MAX_CLI_OPERATIONS:
            raise ValueError(f"CLI operation budget must be in 1..{MAX_CLI_OPERATIONS}")
        self.maximum = maximum
        self.used = 0

    def admit(self, stage: str) -> int:
        if self.used >= self.maximum:
            raise ValueError(
                f"CLI operation budget exhausted before {stage!r}: "
                f"{self.used}/{self.maximum} commands already started"
            )
        self.used += 1
        return self.used


def evidence_stem(label: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9_.-]", "-", label).strip(".-")
    if not cleaned:
        raise ValueError("process evidence label must contain a safe filename character")
    return cleaned[:100]


def b3sum(
    tool: Path,
    payload: bytes,
    *,
    evidence_dir: Path,
    label: str,
    budget: HashBudget,
) -> str:
    budget.admit(payload)
    stem = evidence_stem(f"b3-{label}")
    stdout_path = evidence_dir / f"{stem}.stdout"
    stderr_path = evidence_dir / f"{stem}.stderr"
    receipt_path = evidence_dir / f"{stem}.receipt.json"
    receipt = run_bounded_process(
        [str(tool), "--no-names", "-"],
        stdout_path=stdout_path,
        stderr_path=stderr_path,
        receipt_path=receipt_path,
        max_stdout_bytes=1024,
        max_stderr_bytes=4096,
        timeout_seconds=MAX_B3_TIMEOUT_SECONDS,
        input_data=payload,
    )
    digest = stdout_path.read_bytes().strip().decode("ascii", errors="strict")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError(f"b3sum returned an invalid digest: {digest!r}")
    return digest


class HttpEvidence:
    def __init__(self, root: Path, timeout_seconds: float, total_seconds: float) -> None:
        self.root = root
        self.timeout_seconds = timeout_seconds
        self.deadline = time.monotonic() + total_seconds
        self.rows: list[dict[str, Any]] = []
        self.total_bytes = 0
        self.request_count = 0
        self.hash_budget = HashBudget(MAX_SOURCE_HASH_OPERATIONS, MAX_B3_TOTAL_SOURCE_BYTES)

    def request(
        self,
        name: str,
        url: str,
        *,
        maximum_bytes: int,
        accept: str = "application/json",
        method: str = "GET",
        body: bytes | None = None,
        extra_headers: dict[str, str] | None = None,
    ) -> bytes:
        if not re.fullmatch(r"[a-zA-Z0-9_.-]{1,100}", name):
            raise ValueError("HTTP evidence name must be a short filename-safe token")
        if self.request_count >= MAX_SOURCE_REQUESTS:
            raise ValueError(f"direct source evidence exceeded {MAX_SOURCE_REQUESTS} requests")
        if type(maximum_bytes) is not int or not 1 <= maximum_bytes <= MAX_TOTAL_SOURCE_BYTES:
            raise ValueError("HTTP response byte bound must be a positive admitted integer")
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("direct public-source evidence exceeded its total time budget")
        headers = {
            "Accept": accept,
            "User-Agent": "Nudox-live-registry-canary/1.0 (bounded source evidence)",
        }
        if body is not None:
            headers["Content-Type"] = "application/json"
        headers.update(extra_headers or {})
        request = urllib.request.Request(url, data=body, headers=headers, method=method)
        started = utc_now()
        began_ns = time.perf_counter_ns()
        self.request_count += 1
        request_body_path = None
        if body is not None:
            request_body_file = self.root / f"{name}.request"
            write_new(request_body_file, body)
            request_body_path = str(request_body_file)
        try:
            with urllib.request.urlopen(request, timeout=min(self.timeout_seconds, remaining)) as response:
                final_url = response.geturl()
                status = int(response.status)
                payload_parts = []
                payload_length = 0
                while payload_length <= maximum_bytes:
                    remaining = self.deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError("direct public-source evidence exceeded its total time budget")
                    stream = getattr(response, "fp", None)
                    raw = getattr(stream, "raw", None)
                    sock = getattr(raw, "_sock", None)
                    if sock is not None:
                        sock.settimeout(min(self.timeout_seconds, remaining))
                    read_chunk = getattr(response, "read1", response.read)
                    block = read_chunk(min(PROCESS_READ_CHUNK_BYTES, maximum_bytes + 1 - payload_length))
                    if not block:
                        break
                    payload_parts.append(block)
                    payload_length += len(block)
                if time.monotonic() > self.deadline:
                    raise TimeoutError("direct public-source evidence exceeded its total time budget")
                payload = b"".join(payload_parts)
                selected_headers = {
                    key.lower(): response.headers.get(key)
                    for key in (
                        "content-type", "content-encoding", "date", "etag",
                        "last-modified", "x-pypi-last-serial",
                    )
                    if response.headers.get(key) is not None
                }
        except (urllib.error.URLError, http.client.HTTPException, TimeoutError, OSError) as error:
            self.rows.append(
                {
                    "name": name,
                    "url": url,
                    "method": method,
                    "started_at_utc": started,
                    "elapsed_ns": time.perf_counter_ns() - began_ns,
                    "status": "unavailable",
                    "error": str(error),
                    "request_body_sha256": sha256_bytes(body) if body is not None else None,
                    "request_body_path": request_body_path,
                }
            )
            raise
        if not final_url.startswith("https://"):
            raise ValueError(f"public metadata request redirected outside HTTPS: {final_url}")
        if status < 200 or status >= 300:
            raise RuntimeError(f"public source returned HTTP {status}: {url}")
        if len(payload) > maximum_bytes:
            raise ValueError(f"{name} exceeded its {maximum_bytes}-byte response bound")
        if self.total_bytes + len(payload) > MAX_TOTAL_SOURCE_BYTES:
            raise ValueError(f"direct source evidence exceeded {MAX_TOTAL_SOURCE_BYTES} response bytes")
        self.total_bytes += len(payload)
        path = self.root / f"{name}.response"
        write_new(path, payload)
        self.rows.append(
            {
                "name": name,
                "url": url,
                "final_url": final_url,
                "method": method,
                "status": status,
                "started_at_utc": started,
                "finished_at_utc": utc_now(),
                "elapsed_ns": time.perf_counter_ns() - began_ns,
                "request_accept": accept,
                "request_body_sha256": sha256_bytes(body) if body is not None else None,
                "request_body_path": request_body_path,
                "response_headers": selected_headers,
                "response_path": str(path),
                "response_bytes": len(payload),
                "response_sha256": sha256_bytes(payload),
            }
        )
        return payload

    def persist(self) -> None:
        write_json_new(
            self.root / "source-response-manifest.json",
            {
                "request_count": self.request_count,
                "response_body_bytes": self.total_bytes,
                "response_body_limit_bytes": MAX_TOTAL_SOURCE_BYTES,
                "requests": self.rows,
            },
        )


def cargo_sparse_path(name: str) -> str:
    normalized = name.lower()
    if not re.fullmatch(r"[a-z0-9_-]{1,256}", normalized):
        raise ValueError(f"invalid crates.io name in source response: {name!r}")
    if len(normalized) == 1:
        return f"1/{normalized}"
    if len(normalized) == 2:
        return f"2/{normalized}"
    if len(normalized) == 3:
        return f"3/{normalized[0]}/{normalized}"
    return f"{normalized[:2]}/{normalized[2:4]}/{normalized}"


def cargo_input(http: HttpEvidence, b3_tool: Path) -> dict[str, Any]:
    page_url = "https://crates.io/api/v1/crates?page=1&per_page=16&sort=recent-updates"
    page_bytes = http.request("cargo-recent-page", page_url, maximum_bytes=MAX_RECENT_PAGE_BYTES)
    page = json.loads(page_bytes)
    crates = page.get("crates") if isinstance(page, dict) else None
    if not isinstance(crates, list) or len(crates) > 16:
        raise ValueError("crates.io recent-updates payload did not match the 16-row source bound")
    packages: dict[str, Any] = {}
    releases: list[dict[str, Any]] = []
    sparse_pages = []
    total_release_count = 0
    for ordinal, crate in enumerate(crates):
        if not isinstance(crate, dict) or not isinstance(crate.get("name"), str):
            raise ValueError(f"malformed crates.io recent row {ordinal}")
        name = crate["name"]
        index_url = f"https://index.crates.io/{cargo_sparse_path(name)}"
        sparse = http.request(
            f"cargo-sparse-{ordinal:02d}", index_url, maximum_bytes=MAX_SPARSE_BYTES
        )
        row_count = sum(1 for line in sparse.splitlines() if line)
        if row_count == 0:
            raise ValueError(f"crates.io sparse file contained no release records for {name}")
        total_release_count += row_count
        sparse_pages.append((ordinal, crate, name, index_url, sparse, row_count))

    if total_release_count > MAX_OWNER_BATCH_FACTS:
        packages = {
            name: {
                "sparse_url": index_url,
                "recent_row_sha256": sha256_bytes(canonical_json_bytes(crate)),
                "source_release_count": row_count,
                "release_records_decoded": False,
                "crate_level_downloads_value": crate.get("downloads"),
                "crate_level_recent_downloads_value": crate.get("recent_downloads"),
            }
            for _, crate, name, index_url, _, row_count in sparse_pages
        }
        return {
            "status": "captured",
            "endpoint": "https://crates.io",
            "source_request": page_url,
            "source_window": "first 16 crates ordered by recent-updates; 1 sparse file per name",
            "completeness": "windowed; mutable recent-updates page, not a global catch-up proof",
            "package_count": len(packages),
            "release_count": total_release_count,
            "owner_batch_fact_count": total_release_count,
            "owner_batch_fact_limit": MAX_OWNER_BATCH_FACTS,
            "owner_batch_fact_limit_exceeded": True,
            "proof_computation_skipped_due_to_owner_batch_limit": True,
            "packages": packages,
            "normal_target": None,
            "yanked_target": None,
        }

    for ordinal, crate, name, index_url, sparse, _ in sparse_pages:
        package_releases = []
        for raw_line in sparse.splitlines():
            if not raw_line:
                continue
            record = json.loads(raw_line)
            if not isinstance(record, dict) or str(record.get("name", "")).lower() != name.lower():
                raise ValueError(f"sparse index identity mismatch for {name}")
            version = record.get("vers")
            yanked = record.get("yanked")
            if not isinstance(version, str) or type(yanked) is not bool:
                raise ValueError(f"malformed sparse release row for {name}")
            row = {
                "ecosystem": "cargo",
                "name": name,
                "version": version,
                "coordinate": package_purl("cargo", name, version),
                "proof_blake3": b3sum(
                    b3_tool,
                    raw_line,
                    evidence_dir=http.root / "b3-evidence",
                    label=f"cargo-{ordinal:02d}-release-{len(package_releases):04d}",
                    budget=http.hash_budget,
                ),
                "yanked": yanked,
                "downloads_source": "not-reported-per-release",
                "downloads_source_state": "unknown-at-release-granularity",
                "crate_level_downloads_value": crate.get("downloads"),
                "crate_level_recent_downloads_value": crate.get("recent_downloads"),
                "crate_level_downloads_scope": "crate-level recent page; not this exact release",
                "advisories_source": "not-reported-by-crates-sparse-index",
                "dependencies_source_count": (
                    len(record["deps"]) if isinstance(record.get("deps"), list) else None
                ),
                "cargo_sparse_source_present": "deps" in record,
            }
            package_releases.append(row)
            releases.append(row)
        if not package_releases:
            raise ValueError(f"crates.io sparse file contained no release records for {name}")
        packages[name] = {
            "sparse_url": index_url,
            "recent_row_sha256": sha256_bytes(canonical_json_bytes(crate)),
            "release_count": len(package_releases),
            "yanked_release_count": sum(row["yanked"] for row in package_releases),
            "crate_level_downloads_value": crate.get("downloads"),
            "crate_level_recent_downloads_value": crate.get("recent_downloads"),
        }
    normal = next((row for row in reversed(releases) if not row["yanked"]), None)
    yanked = next((row for row in releases if row["yanked"]), None)
    return {
        "status": "captured",
        "endpoint": "https://crates.io",
        "source_request": page_url,
        "source_window": "first 16 crates ordered by recent-updates; 1 sparse file per name",
        "completeness": "windowed; mutable recent-updates page, not a global catch-up proof",
        "package_count": len(packages),
        "release_count": len(releases),
        "owner_batch_fact_count": len(releases),
        "owner_batch_fact_limit": MAX_OWNER_BATCH_FACTS,
        "owner_batch_fact_limit_exceeded": False,
        "packages": packages,
        "normal_target": normal,
        "yanked_target": yanked,
    }


def npm_input(http: HttpEvidence, b3_tool: Path, max_pages: int) -> dict[str, Any]:
    root_url = "https://replicate.npmjs.com/registry/"
    root = json.loads(http.request("npm-root", root_url, maximum_bytes=1024 * 1024))
    if not isinstance(root, dict) or "update_seq" not in root:
        raise ValueError("npm replication root omitted update_seq")
    source_high = unsigned_source_sequence(root["update_seq"], "npm root update_seq")
    limit = max_pages * 32
    changes_url = f"https://replicate.npmjs.com/registry/_changes?since=0&limit={limit}"
    changes = json.loads(
        http.request("npm-changes", changes_url, maximum_bytes=MAX_NPM_BYTES)
    )
    rows = changes.get("results", changes.get("result")) if isinstance(changes, dict) else None
    if not isinstance(rows, list) or len(rows) > limit:
        raise ValueError("npm changes response exceeded the configured page bound")
    last_row_sequence = 0
    for ordinal, row in enumerate(rows):
        if not isinstance(row, dict) or "seq" not in row:
            raise ValueError(f"npm changes response contained a malformed sequence row {ordinal}")
        sequence = unsigned_source_sequence(row["seq"], f"npm changes row {ordinal} seq")
        if sequence <= last_row_sequence or sequence > source_high:
            raise ValueError("npm changes rows were not strictly increasing through the captured high watermark")
        last_row_sequence = sequence
    response_sequence = unsigned_source_sequence(
        changes.get("last_seq", last_row_sequence), "npm changes last_seq"
    )
    pending = unsigned_source_sequence(changes.get("pending", 0), "npm changes pending")
    if response_sequence < last_row_sequence or response_sequence > source_high:
        raise ValueError("npm changes last_seq was outside the captured source cursor bounds")
    caught_up = pending == 0 and response_sequence == source_high
    latest: dict[str, dict[str, Any]] = {}
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str):
            raise ValueError("npm changes response contained a malformed package row")
        latest[row["id"]] = row
    packages: dict[str, Any] = {}
    packument_rows: list[tuple[int, str, dict[str, Any], dict[str, Any], list[str]]] = []
    owner_fact_count = 0
    for ordinal, (name, change) in enumerate(sorted(latest.items())):
        if change.get("deleted") is True:
            packages[name] = {"deleted": True, "change": change}
            continue
        packument_url = f"https://registry.npmjs.org/{urllib.parse.quote(name, safe='')}"
        packument = json.loads(
            http.request(
                f"npm-packument-{ordinal:03d}", packument_url, maximum_bytes=MAX_NPM_BYTES
            )
        )
        versions = packument.get("versions") if isinstance(packument, dict) else None
        if not isinstance(versions, dict):
            raise ValueError(f"npm packument omitted versions for {name}")
        selected_versions = sorted(versions)[:MAX_NPM_VERSIONS_PER_PACKAGE]
        owner_fact_count += len(selected_versions)
        change_list = change.get("changes")
        revision = (
            change_list[0].get("rev")
            if isinstance(change_list, list) and change_list and isinstance(change_list[0], dict)
            else None
        )
        packages[name] = {
            "deleted": False,
            "sequence": change.get("seq"),
            "change_revision": revision,
            "packument_revision": packument.get("_rev"),
            "release_count": len(selected_versions),
            "source_release_count": len(versions),
            "version_window_truncated": len(versions) > MAX_NPM_VERSIONS_PER_PACKAGE,
        }
        packument_rows.append((ordinal, name, packument, selected_versions))
    if owner_fact_count > MAX_OWNER_BATCH_FACTS:
        return {
            "status": "captured",
            "endpoint": "https://replicate.npmjs.com/registry",
            "packument_endpoint": "https://registry.npmjs.org",
            "source_request": changes_url,
            "source_high_watermark": source_high,
            "page_last_sequence": response_sequence,
            "pending": pending,
            "caught_up_through_source_high_watermark": caught_up,
            "change_row_count": len(rows),
            "package_count": len(packages),
            "release_count": owner_fact_count,
            "owner_batch_fact_limit": MAX_OWNER_BATCH_FACTS,
            "owner_batch_fact_count": owner_fact_count,
            "owner_batch_fact_limit_exceeded": True,
            "proof_computation_skipped_due_to_owner_batch_limit": True,
            "completeness": "source metadata captured; owner batch refused before release proof computation",
            "packages": packages,
            "deprecated_target": None,
            "version_target": None,
        }

    all_releases: list[dict[str, Any]] = []
    for ordinal, name, packument, selected_versions in packument_rows:
        versions = packument["versions"]
        package_releases = []
        for version_index, version in enumerate(selected_versions):
            metadata = versions[version]
            if not isinstance(metadata, dict):
                raise ValueError(f"npm version body is not an object: {name}@{version}")
            deprecated = metadata.get("deprecated")
            row = {
                "ecosystem": "npm",
                "name": name,
                "version": version,
                "coordinate": package_purl("npm", name, version),
                "proof_blake3": b3sum(
                    b3_tool,
                    canonical_json_bytes(metadata),
                    evidence_dir=http.root / "b3-evidence",
                    label=f"npm-{ordinal:03d}-release-{version_index:04d}",
                    budget=http.hash_budget,
                ),
                "deprecated": deprecated if isinstance(deprecated, str) else None,
                "deprecated_source_state": (
                    "known" if isinstance(deprecated, str)
                    else "absent" if "deprecated" in metadata and deprecated is None
                    else "unknown"
                ),
                "yanked_source_state": "absent",
                "downloads_source_state": "absent",
                "advisories_source_state": "absent",
            }
            package_releases.append(row)
            all_releases.append(row)
        packages[name]["deprecated_release_count"] = sum(
            row["deprecated"] is not None for row in package_releases
        )
    return {
        "status": "captured",
        "endpoint": "https://replicate.npmjs.com/registry",
        "packument_endpoint": "https://registry.npmjs.org",
        "source_request": changes_url,
        "source_high_watermark": source_high,
        "page_last_sequence": response_sequence,
        "pending": pending,
        "caught_up_through_source_high_watermark": caught_up,
        "change_row_count": len(rows),
        "package_count": len(packages),
        "release_count": len(all_releases),
        "owner_batch_fact_limit": MAX_OWNER_BATCH_FACTS,
        "owner_batch_fact_count": owner_fact_count,
        "owner_batch_fact_limit_exceeded": False,
        "completeness": "complete only through the returned cursor; a nonzero pending count or lower cursor means not caught up",
        "packages": packages,
        "deprecated_target": next(
            (row for row in all_releases if row["deprecated"] is not None), None
        ),
        "version_target": next(iter(all_releases), None),
    }


def pypi_yanked(files: list[Any]) -> bool | None:
    values = [row.get("yanked") for row in files if isinstance(row, dict)]
    if values and all(value is True for value in values):
        return True
    if any(value is False for value in values):
        return False
    return None


def pypi_input(http: HttpEvidence, b3_tool: Path, max_pages: int) -> dict[str, Any]:
    simple_url = "https://pypi.org/simple/"
    simple = json.loads(
        http.request(
            "pypi-simple",
            simple_url,
            accept="application/vnd.pypi.simple.v1+json",
            maximum_bytes=MAX_NPM_BYTES,
        )
    )
    projects = simple.get("projects") if isinstance(simple, dict) else None
    if not isinstance(projects, list):
        raise ValueError("PyPI Simple API omitted its project list")
    if len(projects) > 2_000_000:
        raise ValueError("PyPI project list exceeded the source adapter's 2,000,000-name bound")
    names: dict[str, str] = {}
    for row in projects:
        if not isinstance(row, dict) or not isinstance(row.get("name"), str):
            raise ValueError("PyPI Simple API contained a malformed project row")
        names.setdefault(normalize_pypi_name(row["name"]), row["name"])
    selected = sorted(names.items())[: min(max_pages, 4)]
    packages: dict[str, Any] = {}
    all_releases: list[dict[str, Any]] = []
    owner_batch_fact_count = 0
    for ordinal, (canonical, source_name) in enumerate(selected):
        url = f"https://pypi.org/pypi/{urllib.parse.quote(source_name, safe='')}/json"
        project = json.loads(
            http.request(
                f"pypi-project-{ordinal:02d}", url, maximum_bytes=MAX_PYPI_BYTES
            )
        )
        info = project.get("info") if isinstance(project, dict) else None
        releases = project.get("releases") if isinstance(project, dict) else None
        if not isinstance(info, dict) or not isinstance(releases, dict):
            raise ValueError(f"PyPI project JSON was malformed for {source_name}")
        vulnerabilities = project.get("vulnerabilities")
        if vulnerabilities is not None and not isinstance(vulnerabilities, list):
            raise ValueError(f"PyPI vulnerabilities field was malformed for {source_name}")
        if isinstance(vulnerabilities, list) and (
            len(vulnerabilities) > 128
            or any(not isinstance(row, dict) or not isinstance(row.get("id"), str) for row in vulnerabilities)
        ):
            raise ValueError(f"PyPI vulnerabilities exceeded the owner's admitted shape for {source_name}")
        vulnerability_ids = (
            [row["id"] for row in vulnerabilities]
            if isinstance(vulnerabilities, list) else None
        )
        release_rows = []
        selected_versions = sorted(releases)[:256]
        latest_version = info.get("version")
        for version in selected_versions:
            files = releases[version]
            if not isinstance(files, list):
                raise ValueError(f"PyPI release files are malformed for {source_name} {version}")
            if not files:
                continue
            owner_batch_fact_count += 1
            row = {
                "ecosystem": "pypi",
                "name": canonical,
                "source_name": source_name,
                "version": version,
                "coordinate": package_purl("pypi", canonical, version),
                "proof_blake3": b3sum(
                    b3_tool,
                    canonical_json_bytes(files),
                    evidence_dir=http.root / "b3-evidence",
                    label=f"pypi-{ordinal:02d}-release-{len(release_rows):04d}",
                    budget=http.hash_budget,
                ),
                "yanked": pypi_yanked(files),
                "file_count": len(files),
                "downloads_source_state": (
                    "known-count" if type(info.get("downloads")) is int and info["downloads"] >= 0
                    else "reported-sentinel" if info.get("downloads") == -1
                    else "unknown"
                ),
                "downloads_source_raw": info.get("downloads"),
                "downloads_source_scope": "PyPI deprecated project-level field; not a release count",
                "advisories_source_state": (
                    "known-package-level-latest-only"
                    if isinstance(vulnerabilities, list)
                    else "unknown"
                ),
                "advisories_source_count": (
                    len(vulnerabilities) if isinstance(vulnerabilities, list) else None
                ),
                "advisories_source_ids": vulnerability_ids if version == latest_version else None,
                "latest_version": info.get("version"),
            }
            release_rows.append(row)
            all_releases.append(row)
        packages[canonical] = {
            "source_name": source_name,
            "latest_version": info.get("version"),
            "source_release_count": len(releases),
            "evidence_version_bound": len(selected_versions),
            "version_window_truncated": len(releases) > 256,
            "latest_version_observed_within_owner_bound": (
                isinstance(latest_version, str) and latest_version in selected_versions
            ),
            "yanked_release_count": sum(row["yanked"] is True for row in release_rows),
            "vulnerability_count": len(vulnerabilities) if isinstance(vulnerabilities, list) else None,
            "downloads_raw_info": info.get("downloads"),
        }
    return {
        "status": "captured",
        "endpoint": "https://pypi.org",
        "simple_request": simple_url,
        "simple_project_count": len(projects),
        "selected_projects": [name for _, name in selected],
        "project_count": len(packages),
        "release_count": len(all_releases),
        "source_version_rows_per_project_capped_at": 256,
        "owner_batch_fact_limit": MAX_OWNER_BATCH_FACTS,
        "owner_batch_fact_count": owner_batch_fact_count,
        "owner_batch_fact_limit_exceeded": owner_batch_fact_count > MAX_OWNER_BATCH_FACTS,
        "completeness": "windowed; first normalized project names; no global event cursor",
        "packages": packages,
        "normal_target": next((row for row in all_releases if row["yanked"] is False), None),
        "yanked_target": next((row for row in all_releases if row["yanked"] is True), None),
        "latest_target": next(
            (
                row for row in all_releases
                if row["version"] == packages[row["name"]]["latest_version"]
            ),
            None,
        ),
    }


def capture_registry_sources(
    output: Path, b3_tool: Path, timeout_seconds: float, max_pages: int, total_seconds: float
) -> tuple[HttpEvidence, dict[str, Any]]:
    evidence = output / "source-evidence"
    evidence.mkdir(mode=0o700)
    http = HttpEvidence(evidence, timeout_seconds, total_seconds)
    sources: dict[str, Any] = {}
    for ecosystem, capture in (
        ("cargo", lambda: cargo_input(http, b3_tool)),
        ("npm", lambda: npm_input(http, b3_tool, max_pages)),
        ("pypi", lambda: pypi_input(http, b3_tool, max_pages)),
    ):
        try:
            sources[ecosystem] = capture()
        except (
            urllib.error.URLError,
            http.client.HTTPException,
            TimeoutError,
            OSError,
            RuntimeError,
            ValueError,
            json.JSONDecodeError,
        ) as error:
            sources[ecosystem] = {
                "status": "unavailable-or-malformed",
                "error": str(error),
                "packages": {},
            }
    return http, sources


def source_targets(sources: dict[str, Any]) -> list[dict[str, Any]]:
    targets = []
    cargo = sources.get("cargo", {})
    for key in ("normal_target", "yanked_target"):
        row = cargo.get(key) if isinstance(cargo, dict) else None
        if isinstance(row, dict):
            targets.append(row)
    npm = sources.get("npm", {})
    for key in ("deprecated_target", "version_target"):
        row = npm.get(key) if isinstance(npm, dict) else None
        if isinstance(row, dict):
            targets.append(row)
    pypi = sources.get("pypi", {})
    for key in ("normal_target", "yanked_target", "latest_target"):
        row = pypi.get(key) if isinstance(pypi, dict) else None
        if isinstance(row, dict):
            targets.append(row)
    unique: dict[str, dict[str, Any]] = {}
    for row in targets:
        unique[row["coordinate"]] = row
    return list(unique.values())


def capture_osv(http: HttpEvidence, targets: list[dict[str, Any]]) -> list[dict[str, Any]]:
    output = []
    selected = []
    by_ecosystem: set[str] = set()
    for target in targets:
        if target.get("ecosystem") not in by_ecosystem:
            selected.append(target)
            by_ecosystem.add(target["ecosystem"])
    for ordinal, target in enumerate(selected[:4]):
        coordinate = target["coordinate"]
        try:
            vulnerabilities = []
            page_token = None
            page_count = 0
            while page_count < MAX_OSV_PAGES:
                request_payload = {"package": {"purl": coordinate}}
                if page_token is not None:
                    request_payload["page_token"] = page_token
                body = json.dumps(request_payload, sort_keys=True, separators=(",", ":")).encode()
                payload = http.request(
                    f"osv-query-{ordinal:02d}-page-{page_count + 1:02d}",
                    "https://api.osv.dev/v1/query",
                    method="POST",
                    body=body,
                    maximum_bytes=MAX_OSV_BYTES,
                )
                response = json.loads(payload)
                page_vulnerabilities, next_page_token = parse_osv_query_page(response)
                vulnerabilities.extend(page_vulnerabilities)
                page_count += 1
                page_token = next_page_token
                if page_token is None:
                    break
            output.append(
                {
                    "coordinate": coordinate,
                    "ecosystem": target["ecosystem"],
                    "status": "point-response" if page_token is None else "incomplete-page-chain",
                    "query_time_utc": utc_now(),
                    "vulnerability_count": len(vulnerabilities),
                    "vulnerability_ids": [
                        row["id"] for row in vulnerabilities
                    ],
                    "page_count": page_count,
                    "cursor_chain_closed": page_token is None,
                    "scope": "independent point query; not the local feed's coverage or freshness",
                }
            )
        except (
            urllib.error.URLError,
            http.client.HTTPException,
            TimeoutError,
            OSError,
            RuntimeError,
            ValueError,
            json.JSONDecodeError,
        ) as error:
            output.append(
                {
                    "coordinate": coordinate,
                    "ecosystem": target["ecosystem"],
                    "status": "unavailable",
                    "detail": str(error),
                    "scope": "unknown, not a clean result",
                }
            )
    return output


def github_tag_pin(
    http: HttpEvidence, repository: str, tag: str
) -> dict[str, Any]:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("forge repository must be owner/repository")
    if not tag or len(tag) > 256 or any(ord(char) < 0x20 for char in tag):
        raise ValueError("forge tag is invalid")
    root = f"https://api.github.com/repos/{repository}/git"
    headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"}
    reference = json.loads(
        http.request(
            "github-tag-reference",
            f"{root}/ref/tags/{urllib.parse.quote(tag, safe='')}",
            maximum_bytes=MAX_GITHUB_BYTES,
            extra_headers=headers,
        )
    )
    target = reference.get("object") if isinstance(reference, dict) else None
    if not isinstance(target, dict) or not isinstance(target.get("sha"), str):
        raise ValueError("GitHub tag reference omitted its target object")
    sha = target["sha"]
    kind = target.get("type")
    for depth in range(4):
        if kind == "commit":
            if not re.fullmatch(r"[0-9a-fA-F]{40}", sha):
                raise ValueError("GitHub tag resolved to an invalid commit object id")
            commit = sha.lower()
            return {
                "repository": repository,
                "tag": tag,
                "tag_reference_object": target,
                "resolved_commit": commit,
                "coordinate": f"https://github.com/{repository}@commit:{commit}",
            }
        if kind != "tag":
            raise ValueError(f"GitHub tag resolved to unsupported object type {kind!r}")
        tag_object = json.loads(
            http.request(
                f"github-annotated-tag-{depth:02d}",
                f"{root}/tags/{urllib.parse.quote(sha, safe='')}",
                maximum_bytes=MAX_GITHUB_BYTES,
                extra_headers=headers,
            )
        )
        target = tag_object.get("object") if isinstance(tag_object, dict) else None
        if not isinstance(target, dict) or not isinstance(target.get("sha"), str):
            raise ValueError("annotated GitHub tag omitted its target object")
        sha, kind = target["sha"], target.get("type")
    raise ValueError("GitHub annotated tag nesting exceeded four objects")


def bounded_stdout(
    command: list[str], output: Path, stderr_path: Path, maximum: int, timeout: float
) -> dict[str, Any]:
    receipt_path = output.with_name(output.name + ".process.json")
    receipt = run_bounded_process(
        command,
        stdout_path=output,
        stderr_path=stderr_path,
        receipt_path=receipt_path,
        max_stdout_bytes=maximum,
        max_stderr_bytes=MAX_STDERR_BYTES,
        timeout_seconds=timeout,
        check=False,
    )
    if receipt["exit_code"]:
        detail = read_regular_file(stderr_path, MAX_STDERR_BYTES, "bounded command stderr")[-4000:]
        raise RuntimeError(
            f"command exited {receipt['exit_code']}: {command[0]}: {detail.decode(errors='replace')}; "
            f"receipt: {receipt_path}"
        )
    return {
        "bytes": receipt["stdout_bytes_stored"],
        "sha256": receipt["stdout_sha256"],
        "exit_code": receipt["exit_code"],
        "stderr_path": str(stderr_path),
        "process_receipt_path": str(receipt_path),
    }


def turso_command(
    tool: Path,
    database: Path,
    arguments: list[str],
    *,
    evidence_dir: Path,
    label: str,
    timeout_seconds: float,
    maximum_bytes: int = 64 * 1024 * 1024,
) -> bytes:
    command = [str(tool), "-m", "list", "--experimental-multiprocess-wal", str(database)] + arguments
    stem = evidence_stem(f"turso-{label}")
    stdout_path = evidence_dir / f"{stem}.stdout"
    stderr_path = evidence_dir / f"{stem}.stderr"
    receipt_path = evidence_dir / f"{stem}.process.json"
    receipt = run_bounded_process(
        command,
        stdout_path=stdout_path,
        stderr_path=stderr_path,
        receipt_path=receipt_path,
        max_stdout_bytes=maximum_bytes,
        max_stderr_bytes=MAX_STDERR_BYTES,
        timeout_seconds=timeout_seconds,
        check=False,
    )
    if receipt["exit_code"]:
        raise RuntimeError(
            f"tursodb failed ({receipt['exit_code']}) for {database}: "
            f"{read_regular_file(stderr_path, MAX_STDERR_BYTES, 'tursodb stderr')[-3000:].decode(errors='replace')}; "
            f"receipt: {receipt_path}"
        )
    return read_regular_file(stdout_path, maximum_bytes, "tursodb stdout")


def database_signature(
    tool: Path, database: Path, evidence_dir: Path, label: str, timeout_seconds: float
) -> dict[str, Any]:
    integrity = turso_command(
        tool, database, ["PRAGMA integrity_check;"], evidence_dir=evidence_dir,
        label=f"{label}-integrity", timeout_seconds=timeout_seconds
    ).decode(errors="replace").strip()
    if integrity != "ok":
        raise ValueError(f"PRAGMA integrity_check failed for {database}: {integrity[:1000]}")
    schema = turso_command(
        tool,
        database,
        [
            "SELECT type||'|'||name||'|'||tbl_name FROM sqlite_master "
            "WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name;"
        ],
        evidence_dir=evidence_dir,
        label=f"{label}-schema",
        timeout_seconds=timeout_seconds,
    )
    dump_path = evidence_dir / f"{label}.dump.sql"
    dump_stderr = evidence_dir / f"{label}.dump.stderr"
    command = [str(tool), "--experimental-multiprocess-wal", str(database), ".dump"]
    dump_meta = bounded_stdout(
        command, dump_path, dump_stderr, MAX_DB_DUMP_BYTES, timeout_seconds
    )
    total_dump_bytes = sum(path.stat().st_size for path in evidence_dir.glob("*.dump.sql"))
    if total_dump_bytes > MAX_TOTAL_DATABASE_DUMP_BYTES:
        raise ValueError(
            f"Turso logical dump evidence exceeded {MAX_TOTAL_DATABASE_DUMP_BYTES} bytes"
        )
    inserts = []
    with dump_path.open("rb") as stream:
        for line in stream:
            if line.startswith(b"INSERT INTO "):
                inserts.append(line.rstrip(b"\n"))
    inserts.sort()
    rows_digest = hashlib.sha256(b"\n".join(inserts)).hexdigest()
    return {
        "path": str(database),
        "integrity_check": integrity,
        "main_bytes": database.stat().st_size,
        "main_sha256": sha256_file(database),
        "schema_identity_sha256": sha256_bytes(schema),
        "schema_identity_bytes": len(schema),
        "dump_bytes": dump_meta["bytes"],
        "dump_sha256": dump_meta["sha256"],
        "insert_statement_count": len(inserts),
        "insert_multiset_sha256": rows_digest,
    }


def is_turso_sidecar(relative: Path, database_paths: set[Path]) -> bool:
    text = relative.as_posix()
    for database in database_paths:
        base = database.as_posix()
        if text in {base + suffix for suffix in ("-wal", "-shm", "-tshm", "-journal")}:
            return True
    return False


def workspace_inventory(root: Path, database_paths: set[Path]) -> dict[str, Any]:
    rows = []
    total = 0
    visited = 0
    for path in root.rglob("*"):
        visited += 1
        if visited > MAX_WORKSPACE_ENTRIES:
            raise ValueError(
                f"workspace closure exceeded {MAX_WORKSPACE_ENTRIES} files and directories"
            )
        relative = path.relative_to(root)
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError(f"workspace backup closure contains a symlink: {relative}")
        if stat.S_ISDIR(metadata.st_mode):
            rows.append({"path": relative.as_posix(), "kind": "directory", "mode": stat.S_IMODE(metadata.st_mode)})
            continue
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError(f"workspace backup closure has a non-regular or linked file: {relative}")
        if relative in database_paths or is_turso_sidecar(relative, database_paths):
            continue
        if total + metadata.st_size > MAX_BACKUP_BYTES:
            raise ValueError(f"non-Turso workspace files exceeded the {MAX_BACKUP_BYTES}-byte bound")
        digest = sha256_file(path)
        after = path.lstat()
        if (metadata.st_dev, metadata.st_ino, metadata.st_size) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
        ):
            raise ValueError(f"workspace file changed while inventorying: {relative}")
        total += after.st_size
        rows.append(
            {
                "path": relative.as_posix(),
                "kind": "file",
                "mode": stat.S_IMODE(after.st_mode),
                "bytes": after.st_size,
                "sha256": digest,
            }
        )
    rows.sort(key=lambda row: row["path"])
    return {"entries": rows, "file_bytes": total, "entry_count": len(rows)}


def sidecar_inventory(root: Path, database_paths: set[Path]) -> list[dict[str, Any]]:
    rows = []
    for database in database_paths:
        for suffix in ("-wal", "-shm", "-tshm", "-journal"):
            sidecar = root / Path(str(database) + suffix)
            if not sidecar.exists():
                continue
            metadata = sidecar.lstat()
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise ValueError(f"unsafe Turso sidecar: {sidecar}")
            rows.append(
                {
                    "path": sidecar.relative_to(root).as_posix(),
                    "bytes": metadata.st_size,
                    "sha256": sha256_file(sidecar),
                    "excluded_from_copy": True,
                    "reason": "Turso VACUUM INTO captures the committed logical database",
                }
            )
    return rows


def remove_private_turso_sidecars(root: Path, database_paths: set[Path]) -> list[dict[str, Any]]:
    """Remove only known Turso sidecars from the runner's private restore tree."""
    rows = sidecar_inventory(root, database_paths)
    for row in rows:
        path = root / row["path"]
        current = path.lstat()
        if not stat.S_ISREG(current.st_mode) or current.st_nlink != 1:
            raise ValueError(f"refusing to remove a linked or non-regular Turso sidecar: {path}")
        if current.st_size != row["bytes"] or sha256_file(path) != row["sha256"]:
            raise ValueError(f"Turso sidecar changed before private cleanup: {path}")
        path.unlink()
    if sidecar_inventory(root, database_paths):
        raise ValueError(f"known Turso sidecars remain in the private restore tree: {root}")
    return rows


def copy_non_database_tree(source: Path, target: Path, database_paths: set[Path]) -> dict[str, Any]:
    target.mkdir(mode=0o700)
    inventory = workspace_inventory(source, database_paths)
    for row in inventory["entries"]:
        relative = Path(row["path"])
        src = source / relative
        dst = target / relative
        if row["kind"] == "directory":
            dst.mkdir(parents=True, exist_ok=True, mode=row["mode"])
            os.chmod(dst, row["mode"])
        else:
            dst.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            shutil.copy2(src, dst, follow_symlinks=False)
            if sha256_file(dst) != row["sha256"] or dst.stat().st_size != row["bytes"]:
                raise ValueError(f"non-Turso file copy verification failed: {relative}")
    copied = workspace_inventory(target, set())
    if copied != inventory:
        raise ValueError("copied non-Turso tree does not match the live owner inventory")
    return inventory


def database_paths(root: Path) -> list[Path]:
    result = []
    for path in root.rglob("*.turso"):
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError(f"Turso database is not a single-link regular file: {path}")
        result.append(path.relative_to(root))
        if len(result) > MAX_DATABASE_COUNT:
            raise ValueError(f"owner workspace exceeded the {MAX_DATABASE_COUNT}-database backup bound")
    if not result:
        raise ValueError("fresh owner workspace has no Turso database to back up")
    return sorted(result)


def file_set_digest(inventory: dict[str, Any]) -> str:
    payload = json.dumps(inventory, sort_keys=True, separators=(",", ":")).encode()
    return sha256_bytes(payload)


def inventory_breakdown(inventory: dict[str, Any]) -> dict[str, Any]:
    directories: dict[str, int] = {}
    journals = []
    cas_paths = []
    for row in inventory["entries"]:
        path = row["path"]
        parts = Path(path).parts
        if row["kind"] == "file":
            directories[parts[0] if parts else "."] = (
                directories.get(parts[0] if parts else ".", 0) + row["bytes"]
            )
            if path.endswith(".journal"):
                journals.append({"path": path, "bytes": row["bytes"], "sha256": row["sha256"]})
            if any(part.lower() in {"cas", "objects", "content"} for part in parts):
                cas_paths.append(path)
    return {
        "bytes_by_top_level_directory": directories,
        "journal_files": journals,
        "cas_or_content_paths": cas_paths,
    }


def parse_discovery_journal(
    path: Path, b3_tool: Path, *, evidence_dir: Path, label: str
) -> dict[str, Any]:
    if not path.exists():
        return {"path": str(path), "exists": False, "bytes": 0, "transaction_count": 0, "facts": 0}
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise ValueError(f"discovery journal is not a single-link regular file: {path}")
    if metadata.st_size > MAX_JOURNAL_BYTES:
        raise ValueError("discovery journal exceeded its 1 GiB scan bound")
    transactions = []
    offset = 0
    hash_budget = HashBudget(
        MAX_JOURNAL_TRANSACTIONS,
        MAX_JOURNAL_BYTES + MAX_JOURNAL_TRANSACTIONS * len(DISCOVERY_DOMAIN),
    )
    with path.open("rb") as stream:
        while offset < metadata.st_size:
            if len(transactions) >= MAX_JOURNAL_TRANSACTIONS:
                raise ValueError(
                    f"discovery journal exceeded the {MAX_JOURNAL_TRANSACTIONS}-transaction bound"
                )
            header = stream.read(14)
            if len(header) != 14 or header[:8] != DISCOVERY_MAGIC:
                raise ValueError(f"invalid or truncated discovery journal header at byte {offset}")
            version = int.from_bytes(header[8:10], "big")
            length = int.from_bytes(header[10:14], "big")
            if version != 1 or length > MAX_FRAME_BYTES:
                raise ValueError(f"unsupported discovery frame version/length at byte {offset}")
            payload = stream.read(length)
            checksum = stream.read(32)
            if len(payload) != length or len(checksum) != 32:
                raise ValueError(f"truncated discovery journal transaction at byte {offset}")
            if b3sum(
                b3_tool,
                DISCOVERY_DOMAIN + payload,
                evidence_dir=evidence_dir,
                label=f"{label}-offset-{offset:012d}",
                budget=hash_budget,
            ) != checksum.hex():
                raise ValueError(f"discovery journal checksum mismatch at byte {offset}")
            transaction = json.loads(payload)
            batch = transaction.get("batch") if isinstance(transaction, dict) else None
            source = batch.get("source") if isinstance(batch, dict) else None
            facts = batch.get("facts") if isinstance(batch, dict) else None
            if not isinstance(facts, list):
                raise ValueError(f"discovery transaction omitted its facts list at byte {offset}")
            ecosystem = source.get("ecosystem") if isinstance(source, dict) else None
            transactions.append(
                {
                    "version": version,
                    "offset": offset,
                    "frame_bytes": 14 + length + 32,
                    "ecosystem": ecosystem,
                    "fact_count": len(facts),
                    "completeness": batch.get("completeness"),
                    "caught_up": batch.get("caught_up"),
                }
            )
            offset += 14 + length + 32
    after = path.lstat()
    if (metadata.st_dev, metadata.st_ino, metadata.st_size) != (
        after.st_dev,
        after.st_ino,
        after.st_size,
    ) or not stat.S_ISREG(after.st_mode) or after.st_nlink != 1:
        raise ValueError(f"discovery journal changed during its bounded scan: {path}")
    return {
        "path": str(path),
        "exists": True,
        "bytes": metadata.st_size,
        "sha256": sha256_file(path),
        "transaction_count": len(transactions),
        "fact_count": sum(row["fact_count"] for row in transactions),
        "transactions": transactions,
    }


def process_tree(pid: int, *, evidence_dir: Path, label: str) -> list[dict[str, Any]]:
    rows: dict[int, dict[str, Any]] = {}
    proc = Path("/proc")
    if proc.is_dir():
        for path in proc.glob("[0-9]*/stat"):
            try:
                content = path.read_text(encoding="utf-8")
                close = content.rfind(")")
                child_pid = int(content[: content.index(" ")])
                fields = content[close + 2 :].split()
                process_dir = path.parent
                try:
                    executable = os.readlink(process_dir / "exe")
                except OSError:
                    executable = None
                try:
                    with (process_dir / "cmdline").open("rb") as stream:
                        raw_argv = stream.read(4097)
                    argv = [
                        part.decode("utf-8", errors="replace")
                        for part in raw_argv.split(b"\0")
                        if part
                    ]
                    argv_truncated = len(raw_argv) > 4096
                    argv = argv[:128]
                except OSError:
                    argv, argv_truncated = [], False
                if len(rows) >= MAX_PROCESS_CENSUS_ROWS:
                    raise RuntimeError(
                        f"host process table exceeded {MAX_PROCESS_CENSUS_ROWS} census rows"
                    )
                rows[child_pid] = {
                    "ppid": int(fields[1]),
                    "command": content[content.index("(") + 1 : close],
                    "executable": executable,
                    "argv0": argv[0] if argv else None,
                    "argv": argv,
                    "argv_truncated": argv_truncated,
                }
            except (OSError, ValueError, IndexError):
                continue
    else:
        command = "/bin/ps" if Path("/bin/ps").exists() else "ps"
        stem = evidence_stem(f"process-tree-{label}")
        stdout_path = evidence_dir / f"{stem}.stdout"
        stderr_path = evidence_dir / f"{stem}.stderr"
        receipt_path = evidence_dir / f"{stem}.process.json"
        receipt = run_bounded_process(
            [command, "-axo", "pid=,ppid=,command="],
            stdout_path=stdout_path,
            stderr_path=stderr_path,
            receipt_path=receipt_path,
            max_stdout_bytes=MAX_PROCESS_CENSUS_BYTES,
            max_stderr_bytes=MAX_STDERR_BYTES,
            timeout_seconds=MAX_PROCESS_DRAIN_SECONDS,
            check=False,
        )
        if receipt["exit_code"] == 0:
            stdout = read_regular_file(stdout_path, MAX_PROCESS_CENSUS_BYTES, "process census stdout")
            for line in stdout.decode("utf-8", errors="replace").splitlines():
                pieces = line.strip().split(None, 2)
                if len(pieces) == 3:
                    try:
                        command_text = pieces[2]
                        argv0 = command_text.split(None, 1)[0] if command_text else ""
                        if len(rows) >= MAX_PROCESS_CENSUS_ROWS:
                            raise RuntimeError(
                                f"host process table exceeded {MAX_PROCESS_CENSUS_ROWS} census rows"
                            )
                        rows[int(pieces[0])] = {
                            "ppid": int(pieces[1]),
                            "command": command_text,
                            "executable": argv0,
                            "argv0": argv0,
                            "argv": [],
                            "argv_truncated": True,
                        }
                    except ValueError:
                        pass
        else:
            raise RuntimeError(f"bounded process census failed; receipt: {receipt_path}")
    descendants = {pid}
    changed = True
    while changed:
        changed = False
        for child, row in rows.items():
            if row["ppid"] in descendants and child not in descendants:
                descendants.add(child)
                changed = True
    return [
        {
            "pid": child,
            **rows.get(
                child,
                {
                    "ppid": None,
                    "command": "unknown",
                    "executable": None,
                    "argv0": None,
                    "argv": [],
                    "argv_truncated": False,
                },
            ),
        }
        for child in sorted(descendants)
    ]


def assert_no_build_children(pid: int, stage: str, evidence_dir: Path) -> dict[str, Any]:
    rows = process_tree(pid, evidence_dir=evidence_dir, label=stage)
    expected_names = set(BUILD_PROCESS_NAMES)
    def process_name(row: dict[str, Any], field: str) -> str | None:
        value = row.get(field)
        if not isinstance(value, str) or not value:
            return None
        return Path(value.split(None, 1)[0]).name.lower()

    offenders = [
        row for row in rows
        if any(
            process_name(row, field) in expected_names
            for field in ("executable", "argv0", "command")
        )
    ]
    if offenders:
        raise RuntimeError(f"build-tool process observed during {stage}: {offenders!r}")
    return {
        "stage": stage,
        "observed_at_utc": utc_now(),
        "root_pid": pid,
        "processes": rows,
        "recognized_build_process_names": sorted(expected_names),
        "build_processes_observed": [],
        "scope": "one point-in-time descendant snapshot; not continuous monitoring and not a historical job census",
    }


def process_rss_bytes(pid: int) -> int | None:
    if platform.system() == "Linux":
        status = Path(f"/proc/{pid}/status")
        try:
            with status.open("rb") as stream:
                payload = stream.read(65537)
        except OSError:
            return None
        if len(payload) > 65536:
            return None
        for line in payload.splitlines():
            if line.startswith(b"VmRSS:"):
                pieces = line.split()
                if len(pieces) >= 2 and pieces[1].isdigit():
                    return int(pieces[1]) * 1024
        return None
    commands = (
        [["/bin/ps", "-o", "rss=", "-p", str(pid)], ["ps", "-o", "rss=", "-p", str(pid)]]
        if platform.system() == "Darwin"
        else [["ps", "-o", "rss=", "-p", str(pid)]]
    )
    for command in commands:
        process: subprocess.Popen[bytes] | None = None
        timer: threading.Timer | None = None
        try:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            timer = threading.Timer(1.0, kill_process_if_running, args=(process,))
            timer.daemon = True
            timer.start()
            assert process.stdout is not None
            raw = process.stdout.read(65)
            if len(raw) > 64:
                kill_process_group_if_present(process.pid)
            status = process.wait(timeout=1.0)
        except (OSError, subprocess.TimeoutExpired, ProcessLookupError):
            if process is not None and process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=1.0)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            continue
        finally:
            if timer is not None:
                timer.cancel()
            if process is not None and process.stdout is not None:
                process.stdout.close()
        if status == 0 and len(raw) <= 64 and raw.strip():
            try:
                return int(raw.decode("ascii").strip().splitlines()[-1]) * 1024
            except ValueError:
                continue
    return None


def kill_process_group_if_present(pid: int) -> None:
    try:
        os.killpg(pid, signal.SIGKILL)
    except (AttributeError, ProcessLookupError, PermissionError):
        pass


def kill_process_if_running(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        kill_process_group_if_present(process.pid)


class RssSampler:
    def __init__(self, pid: int) -> None:
        self.pid = pid
        self.values: list[int] = []
        self.stop_event = threading.Event()
        self.thread = threading.Thread(target=self._sample, daemon=True)

    def _sample(self) -> None:
        while not self.stop_event.is_set():
            value = process_rss_bytes(self.pid)
            if value is not None:
                self.values.append(value)
            self.stop_event.wait(0.05)

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> dict[str, Any]:
        self.stop_event.set()
        self.thread.join()
        return {
            "sample_count": len(self.values),
            "max_bytes": max(self.values, default=process_rss_bytes(self.pid)),
            "sampling_interval_ms": 50,
        }


def process_io(pid: int) -> dict[str, int] | None:
    path = Path(f"/proc/{pid}/io")
    try:
        rows = path.read_text(encoding="ascii").splitlines()
    except OSError:
        return None
    output = {}
    for row in rows:
        key, separator, value = row.partition(":")
        if separator and key in {"rchar", "wchar", "read_bytes", "write_bytes", "syscr", "syscw"}:
            try:
                output[key] = int(value.strip())
            except ValueError:
                return None
    return output or None


def io_delta(before: dict[str, int] | None, after: dict[str, int] | None) -> dict[str, int] | None:
    if before is None or after is None or before.keys() != after.keys():
        return None
    return {key: after[key] - before[key] for key in before}


def percentile(values: list[int], percent: int) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, (percent * len(ordered) + 99) // 100 - 1)
    return ordered[index]


def latency_summary(values: list[int]) -> dict[str, Any]:
    return {
        "samples": len(values),
        "p50_ms": round(percentile(values, 50) / 1_000_000, 3) if values else None,
        "p95_ms": round(percentile(values, 95) / 1_000_000, 3) if values else None,
        "p99_ms": round(percentile(values, 99) / 1_000_000, 3) if values else None,
    }


def search_latency_summary(results: dict[str, dict[str, Any]]) -> dict[str, Any]:
    page_latencies = [
        value
        for result in results.values()
        for value in result["page_latencies_ns"]
    ]
    chain_latencies = [result["chain_latency_ns"] for result in results.values()]
    return {
        "page_round_trip": latency_summary(page_latencies),
        "cursor_chain": latency_summary(chain_latencies),
    }


class CliRunner:
    def __init__(
        self,
        cli: Path,
        project: Path,
        workspace: Path,
        endpoint: Path,
        home: Path,
        output: Path,
        timeout_seconds: float,
        operation_budget: CliOperationBudget,
    ) -> None:
        self.cli = cli
        self.project = project
        self.workspace = workspace
        self.endpoint = endpoint
        self.home = home
        self.output = output
        self.timeout_seconds = timeout_seconds
        self.operation_budget = operation_budget
        self.ordinal = 0
        self.owner_pid: int | None = None

    def run(self, words: list[str], stage: str, limit: int | None = None) -> tuple[dict[str, Any], int, Path]:
        self.ordinal += 1
        stem = f"{self.ordinal:05d}-{re.sub(r'[^a-zA-Z0-9_.-]', '-', stage)[:48]}"
        operation_number = self.operation_budget.admit(stage)
        argv = [
            str(self.cli), "--json", "--detail", "full",
            "--project", str(self.project), "--workspace", str(self.workspace),
            "--endpoint", str(self.endpoint),
        ]
        if limit is not None:
            argv.extend(("--limit", str(limit)))
        argv.extend(words)
        before_io = process_io(self.owner_pid) if self.owner_pid is not None else None
        started = time.perf_counter_ns()
        stdout_path = self.output / f"{stem}.stdout"
        stderr_path = self.output / f"{stem}.stderr"
        process_receipt_path = self.output / f"{stem}.process.json"
        completed = run_bounded_process(
            argv,
            stdout_path=stdout_path,
            stderr_path=stderr_path,
            receipt_path=process_receipt_path,
            max_stdout_bytes=MAX_CLI_BYTES,
            max_stderr_bytes=MAX_STDERR_BYTES,
            timeout_seconds=self.timeout_seconds,
            cwd=self.project,
            env=minimal_environment(self.home, self.endpoint),
            check=False,
        )
        elapsed = time.perf_counter_ns() - started
        stdout_bytes = read_regular_file(stdout_path, MAX_CLI_BYTES, f"CLI stdout at {stage}")
        stderr_bytes = read_regular_file(stderr_path, MAX_STDERR_BYTES, f"CLI stderr at {stage}")
        if completed["exit_code"]:
            raise RuntimeError(
                f"CLI {words[0]} failed ({completed['exit_code']}); output: {stdout_path}; "
                f"receipt: {process_receipt_path}; "
                f"stderr={stderr_bytes[-2000:].decode(errors='replace')}"
            )
        response = json.loads(stdout_bytes)
        if not isinstance(response, dict):
            raise ValueError(f"CLI returned a non-object JSON response at {stage}")
        write_json_new(
            self.output / f"{stem}.result.json",
            {
                "argv": argv,
                "operation_number": operation_number,
                "operation_limit": self.operation_budget.maximum,
                "elapsed_ns": elapsed,
                "stdout_path": str(stdout_path),
                "stdout_bytes": len(stdout_bytes),
                "stdout_sha256": sha256_bytes(stdout_bytes),
                "stderr_path": str(stderr_path),
                "stderr_bytes": len(stderr_bytes),
                "process_receipt_path": str(process_receipt_path),
                "owner_process_io_delta": (
                    io_delta(before_io, process_io(self.owner_pid))
                    if self.owner_pid is not None else None
                ),
                "response": response,
            },
        )
        return response, elapsed, stdout_path


def snapshot_from_page(response: dict[str, Any], query: str) -> list[int]:
    page = response.get("index_search_page")
    snapshot = page.get("snapshot") if isinstance(page, dict) else None
    if not isinstance(snapshot, list) or len(snapshot) != 32 or any(
        type(byte) is not int or not 0 <= byte <= 255 for byte in snapshot
    ):
        raise ValueError(f"index-search returned a malformed generation snapshot for {query!r}")
    return snapshot


def text_coordinate(value: Any) -> str | None:
    if isinstance(value, str) and value.startswith("pkg:"):
        return value
    if isinstance(value, dict):
        for key in ("value", "coordinate", "purl"):
            nested = value.get(key)
            if isinstance(nested, str) and nested.startswith("pkg:"):
                return nested
    return None


def proof_hex(value: Any) -> str | None:
    if isinstance(value, str) and re.fullmatch(r"[0-9a-fA-F]{64}", value):
        return value.lower()
    if isinstance(value, list) and len(value) == 32 and all(
        type(byte) is int and 0 <= byte <= 255 for byte in value
    ):
        return bytes(value).hex()
    return None


def discovery_candidates(response: dict[str, Any]) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []

    def visit(value: Any) -> None:
        if isinstance(value, dict):
            if (
                text_coordinate(value.get("coordinate")) is not None
                and proof_hex(value.get("proof")) is not None
                and "metadata" in value
            ):
                found.append(value)
            for nested in value.values():
                if isinstance(nested, (dict, list)):
                    visit(nested)
        elif isinstance(value, list):
            for nested in value:
                if isinstance(nested, (dict, list)):
                    visit(nested)

    visit(response.get("records", []))
    unique = {}
    for row in found:
        key = (
            text_coordinate(row.get("coordinate")),
            proof_hex(row.get("proof")),
            json.dumps(row.get("source"), sort_keys=True),
        )
        unique[key] = row
    return list(unique.values())


def run_index_search(
    cli: CliRunner, query: str, stage: str, page_limit: int
) -> dict[str, Any]:
    cursor: str | None = None
    cursors: set[str] = set()
    pages = []
    candidates = []
    operands = []
    latencies = []
    snapshot = None
    started = time.perf_counter_ns()
    for number in range(1, MAX_SEARCH_PAGES + 1):
        command = ["index-search", query]
        if cursor is not None:
            command.extend(("--cursor", cursor))
        response, elapsed, _ = cli.run(command, f"{stage}-page-{number:03d}", page_limit)
        if response.get("answer") != "product":
            raise ValueError(f"index-search returned a non-product response for {query!r}")
        records = response.get("records")
        page = response.get("index_search_page")
        if not isinstance(records, list) or any(not isinstance(row, dict) for row in records):
            raise ValueError(f"index-search contained malformed records for {query!r}")
        if not isinstance(page, dict):
            raise ValueError(f"index-search omitted page envelope for {query!r}")
        page_snapshot = snapshot_from_page(response, query)
        if snapshot is None:
            snapshot = page_snapshot
        elif page_snapshot != snapshot:
            raise ValueError(f"cursor chain crossed generations for {query!r}")
        page_operands = [row.get("operand") for row in records]
        if any(not isinstance(value, str) for value in page_operands):
            raise ValueError(f"index-search returned a record without an exact operand for {query!r}")
        operands.extend(page_operands)
        candidates.extend(discovery_candidates(response))
        next_cursor = page.get("next_cursor")
        pages.append(
            {
                "number": number,
                "snapshot": page_snapshot,
                "record_count": len(records),
                "operands": page_operands,
                "next_cursor": next_cursor,
                "result_count": page.get("result_count"),
                "records": response.get("records"),
            }
        )
        latencies.append(elapsed)
        if next_cursor is None:
            break
        if not isinstance(next_cursor, str) or not next_cursor or next_cursor in cursors:
            raise ValueError(f"index-search returned a malformed or repeating cursor for {query!r}")
        cursors.add(next_cursor)
        cursor = next_cursor
    else:
        raise ValueError(f"index-search did not close its cursor within {MAX_SEARCH_PAGES} pages")
    if len(operands) != len(set(operands)):
        raise ValueError(f"index-search returned duplicate operands across pages for {query!r}")
    return {
        "query": query,
        "snapshot": snapshot,
        "pages": pages,
        "cursor_chain_closed": pages[-1]["next_cursor"] is None,
        "page_latencies_ns": latencies,
        "chain_latency_ns": time.perf_counter_ns() - started,
        "operands": operands,
        "discovery_candidates": candidates,
    }


def facet_state(value: Any) -> str:
    if isinstance(value, dict) and isinstance(value.get("state"), str):
        return value["state"]
    return "not-present-in-reply"


def match_target(target: dict[str, Any], result: dict[str, Any]) -> dict[str, Any]:
    coordinate = target["coordinate"]
    matches = [
        candidate for candidate in result["discovery_candidates"]
        if text_coordinate(candidate.get("coordinate")) == coordinate
    ]
    proof = target.get("proof_blake3")
    verified = [row for row in matches if proof_hex(row.get("proof")) == proof]
    projected = verified[0].get("metadata") if verified else None
    projected = projected if isinstance(projected, dict) else {}
    standing = verified[0].get("standing") if verified else None
    actual_standing = standing.get("value") if isinstance(standing, dict) else standing
    downloads = projected.get("downloads")
    advisories = projected.get("advisories")
    yanked = projected.get("yanked")
    projected_advisory_rows = (
        advisories.get("value")
        if isinstance(advisories, dict) and advisories.get("state") == "known"
        else None
    )
    projected_advisory_ids = (
        [row.get("id") for row in projected_advisory_rows if isinstance(row, dict)]
        if isinstance(projected_advisory_rows, list) else None
    )
    source_advisory_ids = target.get("advisories_source_ids")
    expected_standing = None
    if target.get("yanked") is True:
        expected_standing = "yanked"
    elif target.get("ecosystem") == "npm" or target.get("yanked") is False:
        expected_standing = "published"
    return {
        "coordinate": coordinate,
        "ecosystem": target["ecosystem"],
        "source_proof_blake3": proof,
        "candidate_count": len(matches),
        "proof_match_count": len(verified),
        "source_standing_expected": expected_standing,
        "projected_standing": actual_standing,
        "standing_matches_source": expected_standing is None or actual_standing == expected_standing,
        "projected_metadata": [row.get("metadata") for row in verified],
        "source_downloads_value_or_state": target.get(
            "downloads_source_state", target.get("downloads_source")
        ),
        "source_downloads_raw": target.get(
            "downloads_source_raw",
            {
                "crate_level_total": target.get("crate_level_downloads_value"),
                "crate_level_recent": target.get("crate_level_recent_downloads_value"),
            } if target.get("ecosystem") == "cargo" else None,
        ),
        "source_downloads_scope": target.get(
            "downloads_source_scope", target.get("crate_level_downloads_scope")
        ),
        "projected_downloads_state": facet_state(downloads),
        "projected_downloads_value": downloads.get("value") if isinstance(downloads, dict) else None,
        "download_zero_was_not_manufactured": download_zero_was_not_manufactured(target, downloads),
        "projected_advisories_state": facet_state(advisories),
        "source_advisories_state": target.get("advisories_source_state"),
        "source_advisories_count": target.get("advisories_source_count"),
        "source_advisory_ids": source_advisory_ids,
        "projected_advisory_ids": projected_advisory_ids,
        "advisory_ids_match_source": (
            projected_advisory_ids == source_advisory_ids
            if isinstance(source_advisory_ids, list) else None
        ),
        "projected_yanked_state": facet_state(yanked),
        "projected_yanked_value": yanked.get("value") if isinstance(yanked, dict) else None,
        "source_deprecation": target.get("deprecated"),
        "deprecation_projection": (
            "not-returned-by-current-registry-discovery-candidate"
            if target.get("ecosystem") == "npm" else "not-applicable"
        ),
        "source_cargo_sparse_present": target.get("cargo_sparse_source_present"),
        "cargo_sparse_projection": (
            "not-returned-by-current-registry-discovery-candidate"
            if target.get("ecosystem") == "cargo" else "not-applicable"
        ),
        "freshness": [row.get("freshness") for row in verified],
        "completeness": [row.get("completeness") for row in verified],
        "caught_up": [row.get("caught_up") for row in verified],
        "verified": bool(verified),
    }


def strip_freshness(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: strip_freshness(nested)
            for key, nested in value.items()
            if key != "freshness"
        }
    if isinstance(value, list):
        return [strip_freshness(nested) for nested in value]
    return value


def normalize_search_result(result: dict[str, Any]) -> dict[str, Any]:
    return {
        "query": result["query"],
        "snapshot": result["snapshot"],
        "pages": [
            {
                "snapshot": page["snapshot"],
                "operands": page["operands"],
                "records": strip_freshness(page["records"]),
                "next_cursor": page["next_cursor"],
                "result_count": strip_freshness(page["result_count"]),
            }
            for page in result["pages"]
        ],
    }


def wait_ready(locald: BoundedProcess, cli: CliRunner, timeout_seconds: float) -> tuple[int, dict[str, Any]]:
    started = time.perf_counter_ns()
    deadline = time.monotonic() + timeout_seconds
    last_error = ""
    while time.monotonic() < deadline:
        if locald.poll() is not None:
            raise RuntimeError(f"backend-locald exited before readiness ({locald.returncode})")
        try:
            response, _, _ = cli.run(["health"], "health-probe")
            if response.get("answer") == "status":
                return time.perf_counter_ns() - started, response
            last_error = f"unexpected health response: {response!r}"
        except (RuntimeError, ValueError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
            last_error = str(error)
        time.sleep(0.2)
    raise TimeoutError(f"backend-locald was not ready before deadline: {last_error}")


def stop_owner(locald: BoundedProcess) -> int:
    if locald.poll() is None:
        locald.send_signal(signal.SIGTERM)
        try:
            receipt = locald.wait(timeout=60)
        except subprocess.TimeoutExpired:
            locald._fail_and_kill("owner did not stop after SIGTERM within the 60-second grace")
            receipt = locald.wait(timeout=10, force_on_timeout=True)
    else:
        receipt = locald.wait(timeout=10)
    exit_code = receipt["exit_code"]
    if type(exit_code) is not int:
        raise RuntimeError(f"owned backend process has no final exit code; receipt: {locald.receipt_path}")
    return exit_code


def owner_command(
    locald: Path, workspace: Path, endpoint: Path, *, live: bool, max_pages: int
) -> list[str]:
    command = [str(locald), "--workspace", str(workspace), "--endpoint", str(endpoint), "--profile", "builtin"]
    for ecosystem, url in DISCOVERY_SOURCES:
        command.extend(("--registry-discovery-source", f"{ecosystem}={url}"))
    command.extend(("--registry-discovery-max-pages", str(max_pages), "--advisory-offline"))
    command.extend(
        (
            "--forge-max-archive-bytes", str(64 * 1024 * 1024),
            "--forge-max-metadata-bytes", str(2 * 1024 * 1024),
            "--forge-max-readme-bytes", str(256 * 1024),
            "--forge-max-entries", "20000",
            "--forge-max-tree-bytes", str(128 * 1024 * 1024),
            "--forge-max-path-bytes", "4096",
            "--forge-max-entry-bytes", str(4 * 1024 * 1024),
            "--idle-timeout-ms", "0",
        )
    )
    if not live:
        command.extend(
            ("--registry-offline", "--registry-discovery-offline", "--forge-offline")
        )
    return command


def start_owner(
    command: list[str],
    project: Path,
    home: Path,
    endpoint: Path,
    log_path: Path,
    runtime_seconds: float,
) -> tuple[BoundedProcess, RssSampler, dict[str, Path]]:
    stdout_path = log_path.with_name(log_path.stem + ".stdout.log")
    stderr_path = log_path.with_name(log_path.stem + ".stderr.log")
    receipt_path = log_path.with_name(log_path.stem + ".process.json")
    process = BoundedProcess(
        command,
        stdout_path=stdout_path,
        stderr_path=stderr_path,
        receipt_path=receipt_path,
        max_stdout_bytes=MAX_OWNER_LOG_BYTES // 2,
        max_stderr_bytes=MAX_OWNER_LOG_BYTES // 2,
        timeout_seconds=runtime_seconds,
        cwd=project,
        env=minimal_environment(home, endpoint),
    )
    sampler = RssSampler(process.pid)
    sampler.start()
    return process, sampler, {
        "stdout": stdout_path,
        "stderr": stderr_path,
        "receipt": receipt_path,
    }


def pick_queries(targets: list[dict[str, Any]]) -> list[str]:
    queries = []
    for row in targets:
        name, version = row.get("name"), row.get("version")
        if isinstance(name, str) and isinstance(version, str):
            query = f"{name} {version}"
            if query not in queries:
                queries.append(query)
    return queries or ["serde"]


def search_set(
    cli: CliRunner, queries: list[str], stage: str, limit: int
) -> dict[str, dict[str, Any]]:
    return {
        query: run_index_search(cli, query, f"{stage}-{ordinal:02d}", limit)
        for ordinal, query in enumerate(queries, start=1)
    }


def source_matches(
    targets: list[dict[str, Any]], results: dict[str, dict[str, Any]]
) -> list[dict[str, Any]]:
    by_query = {result["query"]: result for result in results.values()}
    output = []
    for target in targets:
        query = f"{target['name']} {target['version']}"
        output.append(match_target(target, by_query[query]))
    return output


def target_diagnostics(
    cli: CliRunner, targets: list[dict[str, Any]], stage: str
) -> dict[str, Any]:
    diagnostics = {}
    by_ecosystem = {}
    for row in targets:
        by_ecosystem.setdefault(row["ecosystem"], row)
    for ecosystem, target in by_ecosystem.items():
        coordinate = target["coordinate"]
        records = {}
        for command in ("package-versions", "dependencies", "advisory"):
            response, elapsed, path = cli.run(
                [command, coordinate], f"{stage}-{ecosystem}-{command}"
            )
            records[command] = {
                "elapsed_ns": elapsed,
                "response_path": str(path),
                "response": response,
            }
        diagnostics[ecosystem] = {"coordinate": coordinate, "commands": records}
    return diagnostics


def compare_diagnostics(left: dict[str, Any], right: dict[str, Any]) -> dict[str, Any]:
    comparisons = {}
    for ecosystem in sorted(set(left) | set(right)):
        comparisons[ecosystem] = {}
        left_commands = left.get(ecosystem, {}).get("commands", {})
        right_commands = right.get(ecosystem, {}).get("commands", {})
        for command in sorted(set(left_commands) | set(right_commands)):
            a = left_commands.get(command, {}).get("response")
            b = right_commands.get(command, {}).get("response")
            comparisons[ecosystem][command] = strip_freshness(a) == strip_freshness(b)
    return comparisons


def create_consistent_backup(
    *,
    workspace: Path,
    backup_root: Path,
    tursodb: Path,
    b3_tool: Path,
    locald: BoundedProcess,
    timeout_seconds: float,
) -> dict[str, Any]:
    """Copy immutable non-DB state and VACUUM every Turso DB before stopping owner.

    No CLI call may run concurrently with this function. After SIGTERM, every
    copied source file and every logical database signature is rechecked.
    """
    started = time.perf_counter_ns()
    relative_databases = database_paths(workspace)
    database_set = set(relative_databases)
    source_inventory_before = workspace_inventory(workspace, database_set)
    sidecars_before = sidecar_inventory(workspace, database_set)
    closure_bytes = source_inventory_before["file_bytes"] + sum(
        (workspace / relative).stat().st_size for relative in relative_databases
    )
    if closure_bytes > MAX_BACKUP_BYTES:
        raise ValueError(f"owner workspace exceeded the {MAX_BACKUP_BYTES}-byte backup bound")
    tree = backup_root / "workspace"
    copied_inventory = copy_non_database_tree(workspace, tree, database_set)
    if workspace_inventory(workspace, database_set) != source_inventory_before:
        raise ValueError("non-Turso workspace state changed while copying the backup closure")

    snapshots_root = backup_root / "turso-vacuum-into"
    snapshots_root.mkdir(mode=0o700)
    database_results = []
    before_signatures = {}
    for ordinal, relative in enumerate(relative_databases, start=1):
        source_db = workspace / relative
        signature = database_signature(
            tursodb, source_db, backup_root / "database-dumps", f"live-before-{ordinal:03d}", timeout_seconds
        )
        before_signatures[relative.as_posix()] = signature
        snapshot = snapshots_root / relative
        snapshot.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        sql_path = backup_root / f"vacuum-{ordinal:03d}.sql"
        quoted = str(snapshot).replace("'", "''")
        write_new(sql_path, f"VACUUM INTO '{quoted}';\n".encode())
        sql = f"VACUUM INTO '{quoted}';"
        turso_command(
            tursodb,
            source_db,
            [sql],
            evidence_dir=backup_root / "turso-process-evidence",
            label=f"vacuum-{ordinal:03d}",
            timeout_seconds=timeout_seconds,
        )
        if not snapshot.is_file() or snapshot.is_symlink():
            raise ValueError(f"VACUUM INTO did not create a regular snapshot for {relative}")
        shutil.copy2(snapshot, tree / relative)
        database_results.append(
            {
                "relative_path": relative.as_posix(),
                "source_before_vacuum": signature,
                "vacuum_snapshot_path": str(snapshot),
                "vacuum_snapshot_bytes": snapshot.stat().st_size,
                "vacuum_snapshot_sha256": sha256_file(snapshot),
                "copied_main_bytes": (tree / relative).stat().st_size,
                "copied_main_sha256": sha256_file(tree / relative),
            }
        )
        current_payload = source_inventory_before["file_bytes"] + sum(
            row["copied_main_bytes"] + row["vacuum_snapshot_bytes"]
            for row in database_results
        )
        if current_payload > MAX_BACKUP_BYTES:
            raise ValueError(f"backup artifact payload exceeded the {MAX_BACKUP_BYTES}-byte bound")

    exit_code = stop_owner(locald)
    if exit_code != 0:
        raise ValueError(
            f"live owner did not exit cleanly after SIGTERM (exit code {exit_code}); "
            "refusing to label the workspace backup consistent"
        )
    sidecars_after = sidecar_inventory(workspace, database_set)
    source_inventory_after = workspace_inventory(workspace, database_set)
    if source_inventory_after != source_inventory_before:
        raise ValueError("non-Turso workspace files changed during owner shutdown; backup is not atomic")
    after_signatures = {}
    backup_signatures = {}
    for ordinal, relative in enumerate(relative_databases, start=1):
        source_db = workspace / relative
        copied_db = tree / relative
        after = database_signature(
            tursodb, source_db, backup_root / "database-dumps", f"live-after-stop-{ordinal:03d}", timeout_seconds
        )
        copied = database_signature(
            tursodb, copied_db, backup_root / "database-dumps", f"backup-copy-{ordinal:03d}", timeout_seconds
        )
        before = before_signatures[relative.as_posix()]
        logical_keys = ("schema_identity_sha256", "insert_statement_count", "insert_multiset_sha256")
        if any(before[key] != after[key] or before[key] != copied[key] for key in logical_keys):
            raise ValueError(f"Turso logical data drifted across running backup for {relative}")
        if database_results[ordinal - 1]["copied_main_sha256"] != copied_db_hash_before_validation(
            database_results[ordinal - 1], copied_db
        ):
            raise ValueError(f"restored Turso main file changed during validation: {relative}")
        after_signatures[relative.as_posix()] = after
        backup_signatures[relative.as_posix()] = copied

    backup_sidecars_removed = remove_private_turso_sidecars(tree, database_set)
    copied_after = workspace_inventory(tree, database_set)
    if copied_after != copied_inventory:
        raise ValueError("Turso validation changed or added a copied non-DB file")
    source_discovery = parse_discovery_journal(
        workspace / "registry-discovery" / "catalog.journal",
        b3_tool,
        evidence_dir=backup_root / "journal-hash-evidence",
        label="source-after-stop",
    )
    backup_discovery = parse_discovery_journal(
        tree / "registry-discovery" / "catalog.journal",
        b3_tool,
        evidence_dir=backup_root / "journal-hash-evidence",
        label="backup-copy",
    )
    journal_content_parity = (
        discovery_journal_content_signature(source_discovery)
        == discovery_journal_content_signature(backup_discovery)
    )
    if not journal_content_parity:
        raise ValueError("discovery journal differs between stopped owner and backup copy")
    copied_main_bytes = sum((tree / relative).stat().st_size for relative in relative_databases)
    snapshot_bytes = sum(row["vacuum_snapshot_bytes"] for row in database_results)
    logical_restore_bytes = copied_after["file_bytes"] + copied_main_bytes
    return {
        "created_at_utc": utc_now(),
        "elapsed_ns": time.perf_counter_ns() - started,
        "owner_sigterm_exit_code": exit_code,
        "non_database_source_inventory": source_inventory_before,
        "non_database_source_inventory_sha256": file_set_digest(source_inventory_before),
        "non_database_backup_inventory": copied_after,
        "non_database_backup_inventory_sha256": file_set_digest(copied_after),
        "non_database_inventory_breakdown": inventory_breakdown(copied_after),
        "turso_sidecars_observed_before_copy_and_excluded": sidecars_before,
        "turso_sidecars_observed_after_stop_and_excluded": sidecars_after,
        "turso_sidecars_removed_from_private_backup_after_validation": backup_sidecars_removed,
        "databases": database_results,
        "copied_database_main_bytes": copied_main_bytes,
        "database_snapshot_bytes": snapshot_bytes,
        "closed_restore_workspace_bytes": logical_restore_bytes,
        "backup_artifact_payload_bytes": logical_restore_bytes + snapshot_bytes,
        "stopped_source_signatures": after_signatures,
        "backup_logical_signatures": backup_signatures,
        "discovery_journal_source": source_discovery,
        "discovery_journal_backup": backup_discovery,
        "discovery_journal_content_parity_ignoring_location_only": journal_content_parity,
        "registry_and_cas_closure": "all regular non-Turso files under the private workspace copied and SHA-256 verified",
        "closure_valid": True,
    }


def copied_main_db_hash_before_validation(database_result: dict[str, Any], copied_db: Path) -> str:
    # The pre-validation hash is the VACUUM INTO output hash; the copied file
    # must match it before tursodb opens the backup copy.
    expected = database_result["copied_main_sha256"]
    if sha256_file(copied_db) != expected:
        raise ValueError("Turso backup copy SHA-256 did not match the VACUUM INTO file")
    return expected


def validate_workspace_copy(source_tree: Path, restore_tree: Path, db_paths: set[Path]) -> None:
    source = workspace_inventory(source_tree, db_paths)
    copied = workspace_inventory(restore_tree, db_paths)
    if source != copied:
        raise ValueError("restore workspace non-Turso files differ from the validated backup tree")
    for relative in db_paths:
        if sha256_file(source_tree / relative) != sha256_file(restore_tree / relative):
            raise ValueError(f"restore workspace Turso file differs from backup: {relative}")


def bounded_git_output(repository: Path, evidence_dir: Path, label: str, words: list[str]) -> bytes:
    stem = evidence_stem(f"git-{label}")
    stdout_path = evidence_dir / f"{stem}.stdout"
    stderr_path = evidence_dir / f"{stem}.stderr"
    receipt_path = evidence_dir / f"{stem}.process.json"
    receipt = run_bounded_process(
        ["git", *words],
        cwd=repository,
        stdout_path=stdout_path,
        stderr_path=stderr_path,
        receipt_path=receipt_path,
        max_stdout_bytes=MAX_PROCESS_CENSUS_BYTES,
        max_stderr_bytes=MAX_STDERR_BYTES,
        timeout_seconds=30.0,
        check=False,
    )
    if receipt["exit_code"] != 0:
        detail = read_regular_file(stderr_path, MAX_STDERR_BYTES, "git stderr")[-3000:]
        raise RuntimeError(
            f"bounded git command failed ({receipt['exit_code']}): {words!r}: "
            f"{detail.decode(errors='replace')}; receipt: {receipt_path}"
        )
    return read_regular_file(stdout_path, MAX_PROCESS_CENSUS_BYTES, "git stdout")


def source_availability(sources: dict[str, Any]) -> dict[str, Any]:
    return {
        ecosystem: {
            "status": value.get("status"),
            "package_count": value.get("package_count") if value.get("status") == "captured" else None,
            "release_count": value.get("release_count") if value.get("status") == "captured" else None,
            "completeness": value.get("completeness"),
            "error": value.get("error"),
        }
        for ecosystem, value in sources.items()
    }


def verify_frozen_inputs(
    *,
    repository: Path,
    evidence_dir: Path,
    paths: dict[str, tuple[Path, bytes, int]],
    executables: dict[str, tuple[Path, dict[str, Any]]],
    stage: str,
    source_commit: str,
) -> None:
    for label, (path, expected, maximum) in paths.items():
        if read_regular_file(path, maximum, label) != expected:
            raise ValueError(f"frozen {label} changed {stage}: {path}")
    for label, (path, expected) in executables.items():
        if executable_snapshot(path) != expected:
            raise ValueError(f"{label} binary changed {stage}: {path}")
    suffix = re.sub(r"[^A-Za-z0-9_.-]", "-", stage)[:48]
    revision = bounded_git_output(repository, evidence_dir, f"revision-{suffix}", ["rev-parse", "HEAD"])
    status = bounded_git_output(
        repository,
        evidence_dir,
        f"status-{suffix}",
        ["status", "--porcelain", "--untracked-files=all"],
    )
    if revision.decode("ascii", errors="replace").strip() != source_commit:
        raise ValueError(f"source revision changed {stage}")
    if status.strip():
        raise ValueError(f"source tree is not clean {stage}")


def prepare_frozen_inputs(args: argparse.Namespace, evidence_dir: Path) -> dict[str, Any]:
    script = Path(__file__).resolve(strict=True)
    repository = script.parents[2]
    locald = args.locald.resolve(strict=True)
    cli = args.cli.resolve(strict=True)
    tursodb = args.tursodb.resolve(strict=True)
    b3_tool = args.b3sum.resolve(strict=True)
    manifest_path = args.build_manifest.resolve(strict=True)
    lock_path = repository / "Cargo.lock"
    paths = {
        "runner": (script, read_regular_file(script, MAX_RUNNER_BYTES, "runner"), MAX_RUNNER_BYTES),
        "build_manifest": (
            manifest_path,
            read_regular_file(manifest_path, MAX_BUILD_MANIFEST_BYTES, "build manifest"),
            MAX_BUILD_MANIFEST_BYTES,
        ),
        "Cargo.lock": (lock_path, read_regular_file(lock_path, MAX_LOCK_BYTES, "Cargo.lock"), MAX_LOCK_BYTES),
    }
    manifest = json.loads(paths["build_manifest"][1])
    if not isinstance(manifest, dict) or manifest.get("schema") != "nudox.runtime-build-manifest.v1":
        raise ValueError("build manifest must use nudox.runtime-build-manifest.v1")
    source = manifest.get("source")
    executables_manifest = manifest.get("executables")
    if not isinstance(source, dict) or not source.get("clean") or not isinstance(source.get("commit"), str):
        raise ValueError("build manifest must identify a clean source commit")
    if not isinstance(executables_manifest, dict):
        raise ValueError("build manifest has no executable map")
    git_revision = bounded_git_output(
        repository, evidence_dir, "prepare-revision", ["rev-parse", "HEAD"]
    ).decode("ascii", errors="replace").strip()
    git_status = bounded_git_output(
        repository,
        evidence_dir,
        "prepare-status",
        ["status", "--porcelain", "--untracked-files=all"],
    ).decode("utf-8", errors="replace").strip()
    if git_revision != source["commit"] or git_status:
        raise ValueError("source tree must be clean and match the build manifest commit")
    executables = {
        "backend-locald": (locald, executable_snapshot(locald)),
        "backend-cli": (cli, executable_snapshot(cli)),
        "tursodb": (tursodb, executable_snapshot(tursodb)),
        "b3sum": (b3_tool, executable_snapshot(b3_tool)),
    }
    for name in ("backend-locald", "backend-cli"):
        recorded = executables_manifest.get(name)
        current = executables[name][1]
        if not isinstance(recorded, dict) or any(
            recorded.get(field) != current[field] for field in ("path", "sha256", "bytes")
        ):
            raise ValueError(f"{name} does not match the exact build-manifest entry")
    return {
        "repository": repository,
        "source_commit": git_revision,
        "paths": paths,
        "executables": executables,
        "build_manifest": manifest,
        "build_manifest_sha256": sha256_bytes(paths["build_manifest"][1]),
        "cargo_lock_sha256": sha256_bytes(paths["Cargo.lock"][1]),
        "cargo_lock_bytes": len(paths["Cargo.lock"][1]),
        "runner_sha256": sha256_bytes(paths["runner"][1]),
    }


def validate_canary_parameters(args: argparse.Namespace) -> None:
    integer_bounds = (
        ("max_pages", args.max_pages, 1, 2),
        ("search_limit", args.search_limit, 1, 200),
        ("repetitions", args.repetitions, 2, MAX_CANARY_REPETITIONS),
        ("max_operations", args.max_operations, 1, MAX_CLI_OPERATIONS),
    )
    for name, value, minimum, maximum in integer_bounds:
        if type(value) is not int or not minimum <= value <= maximum:
            raise ValueError(f"--{name.replace('_', '-')} must be in {minimum}..{maximum}")
    duration_bounds = (
        ("ingest_wait_seconds", args.ingest_wait_seconds, MAX_INGEST_WAIT_SECONDS),
        ("ingest_poll_seconds", args.ingest_poll_seconds, MAX_INGEST_POLL_SECONDS),
        ("timeout_seconds", args.timeout_seconds, MAX_CLI_TIMEOUT_SECONDS),
        ("source_timeout_seconds", args.source_timeout_seconds, MAX_SOURCE_TIMEOUT_SECONDS),
        ("source_budget_seconds", args.source_budget_seconds, MAX_SOURCE_BUDGET_SECONDS),
        ("owner_runtime_seconds", args.owner_runtime_seconds, MAX_OWNER_RUNTIME_SECONDS),
    )
    for name, value, maximum in duration_bounds:
        if not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 < value <= maximum:
            raise ValueError(f"--{name.replace('_', '-')} must be finite and in (0, {maximum:g}]")
    if args.ingest_poll_seconds > args.ingest_wait_seconds:
        raise ValueError("--ingest-poll-seconds cannot exceed --ingest-wait-seconds")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.forge_repository):
        raise ValueError("--forge-repository must be an owner/repository pair")
    if not args.forge_tag or len(args.forge_tag) > 256 or any(ord(char) < 0x20 for char in args.forge_tag):
        raise ValueError("--forge-tag must contain 1..256 characters and no control bytes")


def run_experiment(args: argparse.Namespace, frozen: dict[str, Any], output: Path) -> dict[str, Any]:
    locald = args.locald.resolve(strict=True)
    cli_path = args.cli.resolve(strict=True)
    tursodb = args.tursodb.resolve(strict=True)
    b3_tool = args.b3sum.resolve(strict=True)
    workspace = output / "owner-workspace"
    project = output / "empty-project"
    home = output / "empty-home"
    endpoint_dir = output / "endpoint"
    for directory in (workspace, project, home, endpoint_dir):
        directory.mkdir(mode=0o700)
    endpoint = endpoint_dir / "locald.sock"
    if len(os.fsencode(endpoint)) > 100:
        raise ValueError("private socket path is too long for a local Unix-domain endpoint")
    cli_output = output / "cli-evidence"
    cli_output.mkdir(mode=0o700)
    cli_operation_budget = CliOperationBudget(args.max_operations)
    cli = CliRunner(
        cli_path,
        project,
        workspace,
        endpoint,
        home,
        cli_output,
        args.timeout_seconds,
        cli_operation_budget,
    )
    report: dict[str, Any] = {
        "schema": "nudox.live-registry-discovery-canary.v1",
        "started_at_utc": utc_now(),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": sys.version,
            "hostname": platform.node(),
        },
        "input_sources": {},
        "owner_runs": [],
        "backup": None,
        "restore": None,
        "provenance": {
            "source_commit": frozen["source_commit"],
            "build_manifest_sha256": frozen["build_manifest_sha256"],
            "build_toolchain": frozen["build_manifest"].get("toolchain"),
            "cargo_lock_sha256": frozen["cargo_lock_sha256"],
            "cargo_lock_bytes": frozen["cargo_lock_bytes"],
            "runner_sha256": frozen["runner_sha256"],
            "executables": {
                label: snapshot for label, (_, snapshot) in frozen["executables"].items()
            },
            "frozen_inputs_rechecked_before_each_owner_and_after_stop": True,
            "cli_operation_limit": args.max_operations,
            "owner_runtime_seconds_limit": args.owner_runtime_seconds,
            "backend_environment": "all inherited BACKEND_* variables are removed; only explicit owner flags set source policy",
            "advisory_policy": "owner advisory acquisition is disabled; standalone OSV point queries are reference evidence only and are not owner input",
            "preserved_environment_names": sorted(
                key for key in os.environ
                if key in {
                    "PATH", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TZ",
                    "SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE",
                    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
                    "http_proxy", "https_proxy", "all_proxy", "no_proxy",
                    "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH",
                }
            ),
        },
    }
    verify_frozen_inputs(
        repository=frozen["repository"],
        evidence_dir=output / "provenance-command-evidence",
        paths=frozen["paths"],
        executables=frozen["executables"],
        stage="before independent source requests", source_commit=frozen["source_commit"],
    )
    http, sources = capture_registry_sources(
        output, b3_tool, args.source_timeout_seconds, args.max_pages, args.source_budget_seconds
    )
    targets = source_targets(sources)
    osv = capture_osv(http, targets)
    forge_pin = None
    forge_error = None
    try:
        forge_pin = github_tag_pin(http, args.forge_repository, args.forge_tag)
    except (urllib.error.URLError, TimeoutError, OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        forge_error = str(error)
    http.persist()
    write_json_new(output / "registry-source-inputs.json", sources)
    write_json_new(output / "osv-point-observations.json", osv)
    write_json_new(output / "forge-tag-pin.json", forge_pin or {"status": "unavailable", "error": forge_error})
    report["input_sources"] = {
        "registry": source_availability(sources),
        "targets": targets,
        "osv_point_observations": osv,
        "osv_scope": "independent reference evidence only; owner starts with advisory acquisition disabled",
        "forge_tag_pin": forge_pin,
        "forge_tag_error": forge_error,
        "source_response_manifest": str(output / "source-evidence" / "source-response-manifest.json"),
        "max_pages": args.max_pages,
        "completeness_warning": "these independent responses are time-stamped point/window observations; endpoint data may change before owner fetch",
    }
    batch_limits = {
        ecosystem: {
            "source_status": value.get("status"),
            "fact_count": (
                value.get("owner_batch_fact_count", value.get("release_count"))
                if value.get("status") == "captured" else None
            ),
            "limit": value.get("owner_batch_fact_limit", MAX_OWNER_BATCH_FACTS),
            "exceeded": (
                value.get("owner_batch_fact_limit_exceeded")
                if value.get("status") == "captured" else None
            ),
        }
        for ecosystem, value in sources.items()
    }
    all_source_counts_measured = all(row["source_status"] == "captured" for row in batch_limits.values())
    report["input_sources"]["owner_batch_preflight"] = {
        "limits": batch_limits,
        "within_all_owner_batch_limits": (
            not any(row["exceeded"] is True for row in batch_limits.values())
            if all_source_counts_measured else None
        ),
        "exceeded_sources": [name for name, row in batch_limits.items() if row["exceeded"] is True],
    }
    write_json_new(output / "pre-owner-input-evidence.json", report["input_sources"])
    if report["input_sources"]["owner_batch_preflight"]["exceeded_sources"]:
        raise ValueError(
            "bounded source sample exceeds the existing 4096-fact owner batch limit; "
            "captured evidence is retained and the owner will not be started"
        )

    verify_frozen_inputs(
        repository=frozen["repository"],
        evidence_dir=output / "provenance-command-evidence",
        paths=frozen["paths"],
        executables=frozen["executables"],
        stage="before creating the owner workspace", source_commit=frozen["source_commit"],
    )
    search_root = workspace / "registry-discovery" / "catalog-search-v1"
    projection_before = {"exists": search_root.exists(), "bytes": 0}
    if search_root.exists() or search_root.is_symlink():
        raise ValueError("new private workspace unexpectedly contains a discovery search projection")
    report["projection_before_first_search"] = projection_before

    live_cmd = owner_command(locald, workspace, endpoint, live=True, max_pages=args.max_pages)
    live_log = output / "owner-live.log"
    process: BoundedProcess | None = None
    sampler: RssSampler | None = None
    log_paths: dict[str, Path] | None = None
    all_searches: dict[str, dict[str, Any]] = {}
    target_results: list[dict[str, Any]] = []
    live_diagnostics: dict[str, Any] = {}
    forge_add_response = None
    live_health = None
    live_startup_ns = None
    live_rss = None
    queries = pick_queries(targets)
    poll_rows = []
    try:
        verify_frozen_inputs(
            repository=frozen["repository"],
            evidence_dir=output / "provenance-command-evidence",
            paths=frozen["paths"],
            executables=frozen["executables"],
            stage="before live owner start", source_commit=frozen["source_commit"],
        )
        process, sampler, log_paths = start_owner(
            live_cmd,
            project,
            home,
            endpoint,
            live_log,
            args.owner_runtime_seconds,
        )
        cli.owner_pid = process.pid
        live_startup_ns, live_health = wait_ready(process, cli, args.timeout_seconds)
        no_build_tree = [
            assert_no_build_children(
                process.pid, "live-owner-initialization", output / "process-census-evidence"
            )
        ]
        deadline = time.monotonic() + args.ingest_wait_seconds
        poll = 0
        while time.monotonic() < deadline:
            poll += 1
            results = search_set(cli, queries, f"ingest-poll-{poll:03d}", args.search_limit)
            matches = source_matches(targets, results) if targets else []
            poll_rows.append(
                {
                    "poll": poll,
                    "elapsed_utc": utc_now(),
                    "snapshots": {query: result["snapshot"] for query, result in results.items()},
                    "page_latencies_ns": {
                        query: result["page_latencies_ns"] for query, result in results.items()
                    },
                    "chain_latencies_ns": {
                        query: result["chain_latency_ns"] for query, result in results.items()
                    },
                    "matches": matches,
                }
            )
            all_searches = results
            by_source = {row["ecosystem"]: row["verified"] for row in matches}
            required = {
                ecosystem for ecosystem, value in sources.items()
                if value.get("status") == "captured" and value.get("release_count", 0) > 0
            }
            if required and all(by_source.get(ecosystem, False) for ecosystem in required):
                break
            time.sleep(args.ingest_poll_seconds)
        live_warm_rounds = []
        for repetition in range(args.repetitions):
            live_warm_rounds.append(
                search_set(cli, queries, f"live-warm-{repetition + 1:02d}", args.search_limit)
            )
        all_searches = live_warm_rounds[-1]
        target_results: list[dict[str, Any]] = []
        live_diagnostics = target_diagnostics(cli, targets, "live") if targets else {}
        if forge_pin is not None:
            forge_add_response, _, forge_stdout = cli.run(
                ["forge-add", forge_pin["coordinate"]], "live-forge-add"
            )
            if forge_pin["resolved_commit"] not in forge_stdout.read_text(encoding="utf-8", errors="replace"):
                raise ValueError("forge-add reply did not bind the resolved GitHub commit")
            forge_search_query = forge_pin["repository"]
            forge_search = run_index_search(
                cli, forge_search_query, "live-forge-index-search", args.search_limit
            )
            all_searches[forge_search_query] = forge_search
            poll_rows.append({"forge_search": normalize_search_result(forge_search)})
        no_build_tree.append(
            assert_no_build_children(
                process.pid, "live-registry-forge-search", output / "process-census-evidence"
            )
        )
        live_rss = sampler.stop()
        sampler = None
        report["owner_runs"].append(
            {
                "mode": "live-discovery-and-one-pinned-forge-source",
                "argv": live_cmd,
                "pid": process.pid,
                "process_output_files": (
                    {key: str(path) for key, path in log_paths.items()}
                    if log_paths is not None else None
                ),
                "ready_latency_ns": live_startup_ns,
                "health": live_health,
                "polls": poll_rows,
                "warm_search_rounds": live_warm_rounds,
                "package_version_dependency_advisory_diagnostics": live_diagnostics,
                "forge_add": forge_add_response,
                "forge_search_query": forge_pin["repository"] if forge_pin else None,
                "forge_search": all_searches.get(forge_pin["repository"]) if forge_pin else None,
                "rss": live_rss,
                "process_tree_census": no_build_tree,
                "process_job_census": {
                    "build_processes_observed_in_sampled_snapshots": any(
                        row["build_processes_observed"] for row in no_build_tree
                    ),
                    "sampled_snapshots": len(no_build_tree),
                    "index_build_commands_started_by_runner": 0,
                    "historical_or_other_jobs_enumerated": False,
                    "limitation": "compiler names are checked at listed point-in-time descendant snapshots only; no continuous monitoring or CLI-wide job list exists",
                },
                "no_package_add_or_index_build": True,
            }
        )

        queries_with_forge = list(queries)
        forge_search_query = forge_pin["repository"] if forge_pin is not None else None
        if forge_search_query is not None and forge_search_query not in queries_with_forge:
            queries_with_forge.append(forge_search_query)
        backup_root = output / "consistent-backup"
        backup_root.mkdir(mode=0o700)
        all_searches = search_set(cli, queries_with_forge, "pre-backup-final", args.search_limit)
        target_results = source_matches(targets, all_searches) if targets else []
        report["owner_runs"][0]["source_proof_matches"] = target_results
        report["owner_runs"][0]["pre_backup_final_searches"] = all_searches
        report["owner_runs"][0]["rss"] = live_rss
        report["owner_runs"][0]["warm_search_latency_summary"] = {
            f"round_{index + 1}": search_latency_summary(rows)
            for index, rows in enumerate(live_warm_rounds)
        }
        no_build_tree.append(
            assert_no_build_children(
                process.pid, "pre-backup-final-search", output / "process-census-evidence"
            )
        )
        report["owner_runs"][0]["process_tree_census"] = no_build_tree
        report["owner_runs"][0]["process_job_census"]["sampled_snapshots"] = len(no_build_tree)
        baseline_searches = {
            query: normalize_search_result(result)
            for query, result in all_searches.items()
            if query in queries_with_forge
        }
        baseline_forge_reference = None
        if forge_pin is not None:
            baseline_forge_reference, _, _ = cli.run(
                ["forge-reference", forge_pin["coordinate"]], "live-forge-reference-before-backup"
            )
        report["live_generation_root"] = {
            "search_snapshots": {query: result["snapshot"] for query, result in all_searches.items()},
            "discovery_journal": parse_discovery_journal(
                workspace / "registry-discovery" / "catalog.journal",
                b3_tool,
                evidence_dir=output / "journal-hash-evidence",
                label="live-pre-backup",
            ),
            "workspace_inventory_before_backup": workspace_inventory(
                workspace, set(database_paths(workspace))
            ),
        }
        # All CLI traffic ends before backup begins. The discovery workers may
        # finish fetches, but only owner-thread CLI dispatch applies their batches.
        backup_manifest = create_consistent_backup(
            workspace=workspace,
            backup_root=backup_root,
            tursodb=tursodb,
            b3_tool=b3_tool,
            locald=process,
            timeout_seconds=args.timeout_seconds,
        )
        process = None
        if sampler is not None:
            sampler.stop()
            sampler = None
        report["backup"] = backup_manifest
        db_set = {Path(row["relative_path"]) for row in backup_manifest["databases"]}
        restore_workspace = output / "restored-owner-workspace"
        shutil.copytree(backup_root / "workspace", restore_workspace, symlinks=False)
        restore_sidecars_removed = remove_private_turso_sidecars(restore_workspace, db_set)
        if sidecar_inventory(restore_workspace, db_set):
            raise ValueError("restored workspace contains Turso sidecars before offline cold open")
        validate_workspace_copy(backup_root / "workspace", restore_workspace, db_set)
        restore_endpoint_dir = output / "restore-endpoint"
        restore_endpoint_dir.mkdir(mode=0o700)
        restore_endpoint = restore_endpoint_dir / "locald.sock"
        restore_cli_output = output / "restore-cli-evidence"
        restore_cli_output.mkdir(mode=0o700)
        restore_cli = CliRunner(
            cli_path, project, restore_workspace, restore_endpoint, home,
            restore_cli_output, args.timeout_seconds, cli_operation_budget,
        )
        restore_cmd = owner_command(
            locald, restore_workspace, restore_endpoint, live=False, max_pages=args.max_pages
        )
        verify_frozen_inputs(
            repository=frozen["repository"],
            evidence_dir=output / "provenance-command-evidence",
            paths=frozen["paths"],
            executables=frozen["executables"],
            stage="before offline restored owner start", source_commit=frozen["source_commit"],
        )
        restore_log = output / "owner-restore-offline.log"
        restore_process, restore_sampler, restore_log_paths = start_owner(
            restore_cmd,
            project,
            home,
            restore_endpoint,
            restore_log,
            args.owner_runtime_seconds,
        )
        restore_cli.owner_pid = restore_process.pid
        try:
            restore_startup_ns, restore_health = wait_ready(
                restore_process, restore_cli, args.timeout_seconds
            )
            restore_tree = [
                assert_no_build_children(
                    restore_process.pid,
                    "offline-restore-startup",
                    output / "process-census-evidence",
                )
            ]
            cold_searches = search_set(
                restore_cli, queries_with_forge, "restore-process-cold", args.search_limit
            )
            cold_diagnostics = target_diagnostics(restore_cli, targets, "restore") if targets else {}
            restore_forge_reference = None
            if forge_pin is not None:
                restore_forge_reference, _, _ = restore_cli.run(
                    ["forge-reference", forge_pin["coordinate"]], "restore-forge-reference"
                )
            warm_searches = []
            for repetition in range(max(1, args.repetitions - 1)):
                warm_searches.append(
                    search_set(
                        restore_cli, queries_with_forge,
                        f"restore-warm-{repetition + 1:02d}", args.search_limit,
                    )
                )
            restore_tree.append(
                assert_no_build_children(
                    restore_process.pid,
                    "offline-restored-retrieval",
                    output / "process-census-evidence",
                )
            )
            restore_rss = restore_sampler.stop()
            restore_sampler = None
            parity = {
                query: normalize_search_result(cold_searches[query]) == baseline_searches.get(query)
                for query in queries_with_forge
            }
            forge_parity = (
                strip_freshness(restore_forge_reference)
                == strip_freshness(baseline_forge_reference)
                if forge_pin is not None
                else None
            )
            diag_parity = compare_diagnostics(live_diagnostics, cold_diagnostics)
            report["restore"] = {
                "argv": restore_cmd,
                "pid": restore_process.pid,
                "process_output_files": {
                    key: str(path) for key, path in restore_log_paths.items()
                },
                "owner_startup_ns": restore_startup_ns,
                "health": restore_health,
                "cold_process_first_touch": True,
                "os_page_cache_flushed": False,
                "searches": cold_searches,
                "warm_searches": warm_searches,
                "cold_search_latency_summary": search_latency_summary(cold_searches),
                "warm_search_latency_summaries": [
                    search_latency_summary(rows) for rows in warm_searches
                ],
                "structural_search_parity_by_query": parity,
                "package_version_dependency_advisory_diagnostics": cold_diagnostics,
                "diagnostic_parity_by_ecosystem_command": diag_parity,
                "forge_reference": restore_forge_reference,
                "forge_reference_parity": forge_parity,
                "rss": restore_rss,
                "process_tree_census": restore_tree,
                "restored_discovery_journal": parse_discovery_journal(
                    restore_workspace / "registry-discovery" / "catalog.journal",
                    b3_tool,
                    evidence_dir=output / "journal-hash-evidence",
                    label="restore-cold-after-search",
                ),
                "sidecars_copied": False,
                "sidecars_removed_before_cold_open": restore_sidecars_removed,
                "search_projection_initial_state": projection_before,
            }
        finally:
            restore_exit_code = stop_owner(restore_process)
            report.setdefault("restore", {})["owner_sigterm_exit_code"] = restore_exit_code
            if restore_sampler is not None:
                restore_sampler.stop()
            if restore_exit_code != 0:
                raise RuntimeError(
                    "offline restore owner did not exit cleanly after SIGTERM "
                    f"(exit code {restore_exit_code})"
                )
        verify_frozen_inputs(
            repository=frozen["repository"],
            evidence_dir=output / "provenance-command-evidence",
            paths=frozen["paths"],
            executables=frozen["executables"],
            stage="after live and restore owners stopped", source_commit=frozen["source_commit"],
        )
    finally:
        if process is not None:
            stop_owner(process)
        if sampler is not None:
            sampler.stop()

    journal = parse_discovery_journal(
        workspace / "registry-discovery" / "catalog.journal",
        b3_tool,
        evidence_dir=output / "journal-hash-evidence",
        label="live-report-final",
    )
    source_capture_ok = all(
        report["input_sources"]["registry"].get(ecosystem, {}).get("status") == "captured"
        and (report["input_sources"]["registry"].get(ecosystem, {}).get("release_count") or 0) > 0
        for ecosystem in ("cargo", "npm", "pypi")
    )
    matched_ecosystems = {row["ecosystem"] for row in target_results if row["verified"]}
    matches_ok = (
        source_capture_ok
        and matched_ecosystems == {"cargo", "npm", "pypi"}
        and all(
            row["verified"]
            and row["standing_matches_source"]
            and row["advisory_ids_match_source"] is not False
            and row["download_zero_was_not_manufactured"]
            for row in target_results
        )
    )
    restore_parity = report.get("restore", {}).get("structural_search_parity_by_query", {})
    diagnostic_parity = report.get("restore", {}).get(
        "diagnostic_parity_by_ecosystem_command", {}
    )
    forge_parity = report.get("restore", {}).get("forge_reference_parity")
    requested_shapes = {
        "cargo_published_release": isinstance(sources.get("cargo", {}).get("normal_target"), dict),
        "cargo_yanked_release": isinstance(sources.get("cargo", {}).get("yanked_target"), dict),
        "npm_release": isinstance(sources.get("npm", {}).get("version_target"), dict),
        "npm_deprecation_source_field": isinstance(sources.get("npm", {}).get("deprecated_target"), dict),
        "pypi_release": isinstance(sources.get("pypi", {}).get("normal_target"), dict),
        "pypi_yanked_release": isinstance(sources.get("pypi", {}).get("yanked_target"), dict),
        "osv_point_response_for_all_three_ecosystems": {
            row.get("ecosystem") for row in osv if row.get("status") == "point-response"
        } == {"cargo", "npm", "pypi"},
        "one_commit_pinned_forge_repository": forge_pin is not None and forge_add_response is not None,
    }
    report["correctness"] = {
        "all_three_live_registry_sources_captured": source_capture_ok,
        "live_source_proof_matches": matches_ok,
        "requested_evidence_shapes": requested_shapes,
        "all_requested_evidence_shapes_observed": all(requested_shapes.values()),
        "all_search_cursor_chains_closed": all(
            result["cursor_chain_closed"] for result in all_searches.values()
        ),
        "backup_closure_valid": bool(report.get("backup", {}).get("closure_valid")),
        "restore_search_parity": bool(restore_parity) and all(restore_parity.values()),
        "diagnostic_parity": bool(diagnostic_parity) and all(
            all(commands.values()) for commands in diagnostic_parity.values()
        ),
        "forge_reference_parity": forge_parity,
        "discovery_journal_transactions": journal.get("transaction_count", 0),
    }
    report["performance_scope"] = {
        "latency": "one new backend-cli process plus one Unix-socket command per search page; p50/p95/p99 are emitted for saved samples",
        "warm": "repeat CLI query chains after the owner, source worker result set, and search projection have been opened",
        "cold": "first request after a new owner process opens the restored workspace; OS file/page cache remains warm",
        "rss": "50 ms owner-process RSS sampling from /proc on Linux and bounded one-PID ps output on macOS; peak bytes, null if unavailable",
        "io": "Linux /proc parent-process counters when available; null on hosts without /proc",
        "cache_policy": "fresh private owner workspace and empty HOME; package-manager caches are not used; host filesystem and OS page caches are not reset",
        "allocations": "not instrumented",
        "body_reuse": "owner response-body reuse is not instrumented; direct HTTPS evidence is stored separately and does not prove owner transport cache hits",
        "old_new_index_comparison": "not measured by this single-binary live canary",
        "scale_claim": "bounded canary only; cannot establish production-scale throughput or capacity",
    }
    report["storage_closure"] = {
        "workspace": str(workspace),
        "turso_databases": [row["relative_path"] for row in report["backup"]["databases"]],
        "non_turso_tree": "all single-link regular files and directories below owner workspace, SHA-256 verified before and after owner shutdown",
        "excluded_sidecars": "only known Turso WAL/SHM/journal sidecars; their pre-copy hashes/bytes are recorded; committed logical rows are checked against VACUUM INTO snapshots",
        "CAS_and_registry_files": "included by full workspace traversal, with registry/discovery/forge paths retained in the per-file manifest",
        "backup_directory": str(output / "consistent-backup"),
        "restore_directory": str(output / "restored-owner-workspace"),
        "closed_restore_workspace_bytes": report["backup"]["closed_restore_workspace_bytes"],
        "backup_artifact_payload_bytes": report["backup"]["backup_artifact_payload_bytes"],
    }
    report["completed_at_utc"] = utc_now()
    report["cli_operations"] = {
        "used": cli_operation_budget.used,
        "limit": cli_operation_budget.maximum,
        "shared_across_live_and_restore": True,
    }
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--locald", type=Path, required=True)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--tursodb", type=Path, required=True)
    parser.add_argument("--b3sum", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="fresh private artifact directory; never reused")
    parser.add_argument("--max-pages", type=int, default=1, help="bounded source page budget (1..2; Cargo remains 16 rows)")
    parser.add_argument("--search-limit", type=int, default=50)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--ingest-wait-seconds", type=float, default=60.0)
    parser.add_argument("--ingest-poll-seconds", type=float, default=1.0)
    parser.add_argument("--timeout-seconds", type=float, default=90.0)
    parser.add_argument("--source-timeout-seconds", type=float, default=10.0)
    parser.add_argument("--source-budget-seconds", type=float, default=180.0)
    parser.add_argument("--owner-runtime-seconds", type=float, default=1200.0)
    parser.add_argument("--max-operations", type=int, default=2048)
    parser.add_argument("--forge-repository", default=DEFAULT_FORGE_REPOSITORY)
    parser.add_argument("--forge-tag", default=DEFAULT_FORGE_TAG)
    parser.add_argument(
        "--slot-granted", action="store_true",
        help="required before public-source requests and any local owner process are started",
    )
    args = parser.parse_args()
    try:
        validate_canary_parameters(args)
    except ValueError as error:
        parser.error(str(error))
    if not args.slot_granted:
        parser.error("refusing public-source access or owner startup until the root grants the canary slot")
    output_arg = args.output.expanduser()
    if not output_arg.name or output_arg.name in {".", ".."}:
        parser.error("--output must name a fresh directory")
    output = output_arg.parent.resolve(strict=True) / output_arg.name
    if os.path.lexists(output):
        parser.error(f"output already exists; choose a new private run path: {output}")
    output.mkdir(mode=0o700)
    try:
        frozen = prepare_frozen_inputs(args, output / "provenance-command-evidence")
        report = run_experiment(args, frozen, output)
        write_json_new(output / "report.json", report)
        checks = report["correctness"]
        return 0 if (
            checks["backup_closure_valid"]
            and report.get("backup", {}).get("owner_sigterm_exit_code") == 0
            and checks["restore_search_parity"]
            and report.get("restore", {}).get("owner_sigterm_exit_code") == 0
            and checks["all_search_cursor_chains_closed"]
            and checks["all_three_live_registry_sources_captured"]
            and checks["live_source_proof_matches"]
            and checks["all_requested_evidence_shapes_observed"]
            and checks["diagnostic_parity"]
            and checks["forge_reference_parity"] is True
        ) else 2
    except BaseException as error:
        write_json_new(
            output / "failure-report.json",
            {
                "schema": "nudox.live-registry-discovery-canary-failure.v1",
                "failed_at_utc": utc_now(),
                "error_type": type(error).__name__,
                "error": str(error),
                "traceback": traceback.format_exc(),
                "output_directory": str(output),
                "owner_log_stdout": str(output / "owner-live.stdout.log"),
                "owner_log_stderr": str(output / "owner-live.stderr.log"),
                "owner_process_receipt": str(output / "owner-live.process.json"),
                "restore_log_stdout": str(output / "owner-restore-offline.stdout.log"),
                "restore_log_stderr": str(output / "owner-restore-offline.stderr.log"),
                "restore_process_receipt": str(output / "owner-restore-offline.process.json"),
            },
        )
        raise


if __name__ == "__main__":
    raise SystemExit(main())
