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
import json
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
MAX_TOTAL_DATABASE_DUMP_BYTES = 1024 * 1024 * 1024
MAX_TOTAL_SOURCE_BYTES = 128 * 1024 * 1024
MAX_SOURCE_REQUESTS = 160
MAX_SEARCH_PAGES = 256
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


def b3sum(tool: Path, payload: bytes) -> str:
    result = subprocess.run(
        [str(tool), "--no-names", "-"],
        input=payload,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"b3sum failed: {result.stderr[-2000:].decode(errors='replace')}")
    digest = result.stdout.strip().decode("ascii", errors="strict")
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
                payload = response.read(maximum_bytes + 1)
                selected_headers = {
                    key.lower(): response.headers.get(key)
                    for key in (
                        "content-type", "content-encoding", "date", "etag",
                        "last-modified", "x-pypi-last-serial",
                    )
                    if response.headers.get(key) is not None
                }
        except (urllib.error.URLError, TimeoutError, OSError) as error:
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
    for ordinal, crate in enumerate(crates):
        if not isinstance(crate, dict) or not isinstance(crate.get("name"), str):
            raise ValueError(f"malformed crates.io recent row {ordinal}")
        name = crate["name"]
        index_url = f"https://index.crates.io/{cargo_sparse_path(name)}"
        sparse = http.request(
            f"cargo-sparse-{ordinal:02d}", index_url, maximum_bytes=MAX_SPARSE_BYTES
        )
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
                "proof_blake3": b3sum(b3_tool, raw_line),
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
        "owner_batch_fact_limit": 4096,
        "owner_batch_fact_limit_exceeded": len(releases) > 4096,
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
    all_releases: list[dict[str, Any]] = []
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
        package_releases = []
        selected_versions = sorted(versions)[:4096]
        for version in selected_versions:
            metadata = versions[version]
            if not isinstance(metadata, dict):
                raise ValueError(f"npm version body is not an object: {name}@{version}")
            deprecated = metadata.get("deprecated")
            row = {
                "ecosystem": "npm",
                "name": name,
                "version": version,
                "coordinate": package_purl("npm", name, version),
                "proof_blake3": b3sum(b3_tool, canonical_json_bytes(metadata)),
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
            "release_count": len(package_releases),
            "source_release_count": len(versions),
            "version_window_truncated": len(versions) > 4096,
            "deprecated_release_count": sum(row["deprecated"] is not None for row in package_releases),
        }
        owner_fact_count += len(package_releases)
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
        "owner_batch_fact_limit": 4096,
        "owner_batch_fact_count": owner_fact_count,
        "owner_batch_fact_limit_exceeded": owner_fact_count > 4096,
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
                "proof_blake3": b3sum(b3_tool, canonical_json_bytes(files)),
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
        "owner_batch_fact_limit": 4096,
        "owner_batch_fact_count_within_owner_version_limit": owner_batch_fact_count,
        "owner_batch_fact_limit_exceeded": owner_batch_fact_count > 4096,
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
        except (urllib.error.URLError, TimeoutError, OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
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
                page_vulnerabilities = response.get("vulns") if isinstance(response, dict) else None
                if not isinstance(page_vulnerabilities, list) or any(
                    not isinstance(row, dict) or not isinstance(row.get("id"), str)
                    for row in page_vulnerabilities
                ):
                    raise ValueError("OSV response omitted a well-formed vulns list")
                vulnerabilities.extend(page_vulnerabilities)
                page_count += 1
                page_token = response.get("next_page_token")
                if page_token is None:
                    break
                if not isinstance(page_token, str) or not page_token:
                    raise ValueError("OSV response returned a malformed page token")
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
        except (urllib.error.URLError, TimeoutError, OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
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
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    stderr_path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    timer_fired = threading.Event()
    with output.open("xb") as stdout, stderr_path.open("xb") as stderr:
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=stderr)
        timer = threading.Timer(timeout, lambda: (timer_fired.set(), process.kill()))
        timer.daemon = True
        timer.start()
        written = 0
        try:
            assert process.stdout is not None
            for block in iter(lambda: process.stdout.read(1024 * 1024), b""):
                written += len(block)
                if written > maximum:
                    process.kill()
                    raise ValueError(f"command output exceeded its {maximum}-byte limit")
                stdout.write(block)
            status = process.wait()
        finally:
            timer.cancel()
            if process.poll() is None:
                process.kill()
                process.wait()
    if timer_fired.is_set():
        raise TimeoutError(f"command exceeded {timeout} seconds: {command[0]}")
    if status:
        detail = stderr_path.read_bytes()[-4000:].decode(errors="replace")
        raise RuntimeError(f"command exited {status}: {command[0]}: {detail}")
    return {"bytes": written, "sha256": sha256_file(output), "exit_code": status}


def turso_command(
    tool: Path,
    database: Path,
    arguments: list[str],
    *,
    timeout_seconds: float,
    maximum_bytes: int = 64 * 1024 * 1024,
) -> bytes:
    command = [str(tool), "-m", "list", "--experimental-multiprocess-wal", str(database)] + arguments
    result = subprocess.run(command, capture_output=True, timeout=timeout_seconds, check=False)
    if result.returncode:
        raise RuntimeError(
            f"tursodb failed ({result.returncode}) for {database}: "
            f"{result.stderr[-3000:].decode(errors='replace')}"
        )
    if len(result.stdout) > maximum_bytes:
        raise ValueError(f"tursodb output exceeded {maximum_bytes} bytes for {database}")
    return result.stdout


def database_signature(
    tool: Path, database: Path, evidence_dir: Path, label: str, timeout_seconds: float
) -> dict[str, Any]:
    integrity = turso_command(
        tool, database, ["PRAGMA integrity_check;"], timeout_seconds=timeout_seconds
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
    for path in sorted(root.rglob("*")):
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
        digest = sha256_file(path)
        after = path.lstat()
        if (metadata.st_dev, metadata.st_ino, metadata.st_size) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
        ):
            raise ValueError(f"workspace file changed while inventorying: {relative}")
        total += after.st_size
        if total > MAX_BACKUP_BYTES:
            raise ValueError(f"non-Turso workspace files exceeded the {MAX_BACKUP_BYTES}-byte bound")
        rows.append(
            {
                "path": relative.as_posix(),
                "kind": "file",
                "mode": stat.S_IMODE(after.st_mode),
                "bytes": after.st_size,
                "sha256": digest,
            }
        )
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
    for path in sorted(root.rglob("*.turso")):
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError(f"Turso database is not a single-link regular file: {path}")
        result.append(path.relative_to(root))
    if not result:
        raise ValueError("fresh owner workspace has no Turso database to back up")
    if len(result) > MAX_DATABASE_COUNT:
        raise ValueError(f"owner workspace exceeded the {MAX_DATABASE_COUNT}-database backup bound")
    return result


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


def parse_discovery_journal(path: Path, b3_tool: Path) -> dict[str, Any]:
    if not path.exists():
        return {"path": str(path), "exists": False, "bytes": 0, "transaction_count": 0, "facts": 0}
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise ValueError(f"discovery journal is not a single-link regular file: {path}")
    if metadata.st_size > MAX_JOURNAL_BYTES:
        raise ValueError("discovery journal exceeded its 1 GiB scan bound")
    transactions = []
    offset = 0
    with path.open("rb") as stream:
        while offset < metadata.st_size:
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
            if b3sum(b3_tool, DISCOVERY_DOMAIN + payload) != checksum.hex():
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
    return {
        "path": str(path),
        "exists": True,
        "bytes": metadata.st_size,
        "sha256": sha256_file(path),
        "transaction_count": len(transactions),
        "fact_count": sum(row["fact_count"] for row in transactions),
        "transactions": transactions,
    }


def process_tree(pid: int) -> list[dict[str, Any]]:
    rows: dict[int, tuple[int, str]] = {}
    proc = Path("/proc")
    if proc.is_dir():
        for path in proc.glob("[0-9]*/stat"):
            try:
                content = path.read_text(encoding="utf-8")
                close = content.rfind(")")
                child_pid = int(content[: content.index(" ")])
                fields = content[close + 2 :].split()
                rows[child_pid] = (int(fields[1]), content[content.index("(") + 1 : close])
            except (OSError, ValueError, IndexError):
                continue
    else:
        command = "/bin/ps" if Path("/bin/ps").exists() else "ps"
        result = subprocess.run(
            [command, "-axo", "pid=,ppid=,command="],
            text=True,
            capture_output=True,
            check=False,
        )
        if result.returncode == 0:
            for line in result.stdout.splitlines():
                pieces = line.strip().split(None, 2)
                if len(pieces) == 3:
                    try:
                        rows[int(pieces[0])] = (int(pieces[1]), pieces[2])
                    except ValueError:
                        pass
    descendants = {pid}
    changed = True
    while changed:
        changed = False
        for child, (parent, _) in rows.items():
            if parent in descendants and child not in descendants:
                descendants.add(child)
                changed = True
    return [
        {
            "pid": child,
            "ppid": rows.get(child, (None, ""))[0],
            "command": rows.get(child, (None, "unknown"))[1],
        }
        for child in sorted(descendants)
    ]


def assert_no_build_children(pid: int, stage: str) -> list[dict[str, Any]]:
    rows = process_tree(pid)
    offenders = [
        row for row in rows
        if re.search(r"(^|[/ ])(cargo|rustc)([ /]|$)", row["command"], re.IGNORECASE)
    ]
    if offenders:
        raise RuntimeError(f"Cargo/rustc process observed during {stage}: {offenders!r}")
    return rows


def process_rss_bytes(pid: int) -> int | None:
    commands = (
        [["/bin/ps", "-o", "rss=", "-p", str(pid)], ["ps", "-o", "rss=", "-p", str(pid)]]
        if platform.system() == "Darwin"
        else [["ps", "-o", "rss=", "-p", str(pid)]]
    )
    for command in commands:
        try:
            result = subprocess.run(command, capture_output=True, text=True, check=False)
        except OSError:
            continue
        if result.returncode == 0 and result.stdout.strip():
            try:
                return int(result.stdout.strip().splitlines()[-1]) * 1024
            except ValueError:
                continue
    return None


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
    ) -> None:
        self.cli = cli
        self.project = project
        self.workspace = workspace
        self.endpoint = endpoint
        self.home = home
        self.output = output
        self.timeout_seconds = timeout_seconds
        self.ordinal = 0
        self.owner_pid: int | None = None

    def run(self, words: list[str], stage: str, limit: int | None = None) -> tuple[dict[str, Any], int, Path]:
        self.ordinal += 1
        stem = f"{self.ordinal:05d}-{re.sub(r'[^a-zA-Z0-9_.-]', '-', stage)[:48]}"
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
        completed = subprocess.run(
            argv,
            cwd=self.project,
            env=minimal_environment(self.home, self.endpoint),
            capture_output=True,
            timeout=self.timeout_seconds,
            check=False,
        )
        elapsed = time.perf_counter_ns() - started
        if len(completed.stdout) > MAX_CLI_BYTES or len(completed.stderr) > 4 * 1024 * 1024:
            raise ValueError(f"CLI response exceeded its evidence bound at {stage}")
        stdout_path = self.output / f"{stem}.stdout"
        write_new(stdout_path, completed.stdout)
        if completed.stderr:
            write_new(self.output / f"{stem}.stderr", completed.stderr)
        if completed.returncode:
            raise RuntimeError(
                f"CLI {words[0]} failed ({completed.returncode}); output: {stdout_path}; "
                f"stderr={completed.stderr[-2000:].decode(errors='replace')}"
            )
        response = json.loads(completed.stdout)
        if not isinstance(response, dict):
            raise ValueError(f"CLI returned a non-object JSON response at {stage}")
        write_json_new(
            self.output / f"{stem}.result.json",
            {
                "argv": argv,
                "elapsed_ns": elapsed,
                "stdout_path": str(stdout_path),
                "stdout_bytes": len(completed.stdout),
                "stdout_sha256": sha256_bytes(completed.stdout),
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
    projected_zero = (
        isinstance(downloads, dict)
        and downloads.get("state") == "known"
        and downloads.get("value") == 0
    )
    source_exact_zero = (
        target.get("downloads_source_state") == "known-count"
        and target.get("downloads_source_scope") == "per-release"
        and target.get("downloads_source_raw") == 0
    )
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
        "download_zero_was_not_manufactured": not projected_zero or source_exact_zero,
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


def wait_ready(locald: subprocess.Popen[bytes], cli: CliRunner, timeout_seconds: float) -> tuple[int, dict[str, Any]]:
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


def stop_owner(locald: subprocess.Popen[bytes]) -> int:
    if locald.poll() is None:
        locald.send_signal(signal.SIGTERM)
        try:
            return locald.wait(timeout=60)
        except subprocess.TimeoutExpired:
            locald.kill()
    return locald.wait(timeout=10)


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
    command: list[str], project: Path, home: Path, endpoint: Path, log_path: Path
) -> tuple[subprocess.Popen[bytes], RssSampler, Any]:
    log = log_path.open("xb")
    process = subprocess.Popen(
        command,
        cwd=project,
        env=minimal_environment(home, endpoint),
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    sampler = RssSampler(process.pid)
    sampler.start()
    return process, sampler, log


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
    locald: subprocess.Popen[bytes],
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
        turso_command(tursodb, source_db, [sql], timeout_seconds=timeout_seconds)
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
        workspace / "registry-discovery" / "catalog.journal", b3_tool
    )
    backup_discovery = parse_discovery_journal(
        tree / "registry-discovery" / "catalog.journal", b3_tool
    )
    if source_discovery != backup_discovery:
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
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repository, text=True, capture_output=True, check=False
    )
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=repository,
        text=True,
        capture_output=True,
        check=False,
    )
    if revision.returncode or revision.stdout.strip() != source_commit:
        raise ValueError(f"source revision changed {stage}")
    if status.returncode or status.stdout.strip():
        raise ValueError(f"source tree is not clean {stage}")


def prepare_frozen_inputs(args: argparse.Namespace) -> dict[str, Any]:
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
    git_revision = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repository, text=True, capture_output=True, check=True
    ).stdout.strip()
    git_status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=repository,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
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
    cli = CliRunner(cli_path, project, workspace, endpoint, home, cli_output, args.timeout_seconds)
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
        repository=frozen["repository"], paths=frozen["paths"], executables=frozen["executables"],
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
            "limit": value.get("owner_batch_fact_limit", 4096),
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
        repository=frozen["repository"], paths=frozen["paths"], executables=frozen["executables"],
        stage="before creating the owner workspace", source_commit=frozen["source_commit"],
    )
    search_root = workspace / "registry-discovery" / "catalog-search-v1"
    projection_before = {"exists": search_root.exists(), "bytes": 0}
    if search_root.exists() or search_root.is_symlink():
        raise ValueError("new private workspace unexpectedly contains a discovery search projection")
    report["projection_before_first_search"] = projection_before

    live_cmd = owner_command(locald, workspace, endpoint, live=True, max_pages=args.max_pages)
    live_log = output / "owner-live.log"
    process = None
    sampler = None
    log = None
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
            repository=frozen["repository"], paths=frozen["paths"], executables=frozen["executables"],
            stage="before live owner start", source_commit=frozen["source_commit"],
        )
        process, sampler, log = start_owner(live_cmd, project, home, endpoint, live_log)
        cli.owner_pid = process.pid
        live_startup_ns, live_health = wait_ready(process, cli, args.timeout_seconds)
        no_build_tree = assert_no_build_children(process.pid, "live owner initialization")
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
            forge_search = run_index_search(cli, "json-c", "live-forge-index-search", args.search_limit)
            all_searches["json-c"] = forge_search
            poll_rows.append({"forge_search": normalize_search_result(forge_search)})
        no_build_tree += assert_no_build_children(process.pid, "live registry/forge search")
        live_rss = sampler.stop()
        sampler = None
        if log is not None:
            log.flush()
            log.close()
            log = None
        report["owner_runs"].append(
            {
                "mode": "live-discovery-and-one-pinned-forge-source",
                "argv": live_cmd,
                "pid": process.pid,
                "ready_latency_ns": live_startup_ns,
                "health": live_health,
                "polls": poll_rows,
                "warm_search_rounds": live_warm_rounds,
                "package_version_dependency_advisory_diagnostics": live_diagnostics,
                "forge_add": forge_add_response,
                "forge_search": all_searches.get("json-c"),
                "rss": live_rss,
                "process_tree_census": no_build_tree,
                "process_job_census": {
                    "cargo_or_rustc_children_observed": False,
                    "index_jobs_started_by_runner": 0,
                    "historical_or_other_jobs_enumerated": False,
                    "limitation": "no CLI job-list endpoint; index_progress/index_await are ticket-scoped",
                },
                "no_package_add_or_index_build": True,
            }
        )

        queries_with_forge = list(queries)
        if forge_pin is not None and "json-c" not in queries_with_forge:
            queries_with_forge.append("json-c")
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
        no_build_tree += assert_no_build_children(process.pid, "pre-backup final search")
        report["owner_runs"][0]["process_tree_census"] = no_build_tree
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
                workspace / "registry-discovery" / "catalog.journal", b3_tool
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
            restore_cli_output, args.timeout_seconds,
        )
        restore_cmd = owner_command(
            locald, restore_workspace, restore_endpoint, live=False, max_pages=args.max_pages
        )
        verify_frozen_inputs(
            repository=frozen["repository"], paths=frozen["paths"], executables=frozen["executables"],
            stage="before offline restored owner start", source_commit=frozen["source_commit"],
        )
        restore_log = output / "owner-restore-offline.log"
        restore_process, restore_sampler, restore_log_stream = start_owner(
            restore_cmd, project, home, restore_endpoint, restore_log
        )
        restore_cli.owner_pid = restore_process.pid
        try:
            restore_startup_ns, restore_health = wait_ready(
                restore_process, restore_cli, args.timeout_seconds
            )
            restore_tree = assert_no_build_children(restore_process.pid, "offline restore startup")
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
            restore_tree += assert_no_build_children(restore_process.pid, "offline restored retrieval")
            restore_rss = restore_sampler.stop()
            restore_sampler = None
            restore_log_stream.flush()
            restore_log_stream.close()
            restore_log_stream = None
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
                    restore_workspace / "registry-discovery" / "catalog.journal", b3_tool
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
            if restore_log_stream is not None:
                restore_log_stream.close()
            if restore_exit_code != 0:
                raise RuntimeError(
                    "offline restore owner did not exit cleanly after SIGTERM "
                    f"(exit code {restore_exit_code})"
                )
        verify_frozen_inputs(
            repository=frozen["repository"], paths=frozen["paths"], executables=frozen["executables"],
            stage="after live and restore owners stopped", source_commit=frozen["source_commit"],
        )
    finally:
        if process is not None and process.poll() is None:
            stop_owner(process)
        if sampler is not None:
            sampler.stop()
        if log is not None:
            log.close()

    journal = parse_discovery_journal(
        workspace / "registry-discovery" / "catalog.journal", b3_tool
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
        "rss": "50 ms ps sampling of each owner process; peak bytes, null if host process metrics unavailable",
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
    parser.add_argument("--forge-repository", default=DEFAULT_FORGE_REPOSITORY)
    parser.add_argument("--forge-tag", default=DEFAULT_FORGE_TAG)
    parser.add_argument(
        "--slot-granted", action="store_true",
        help="required before public-source requests and any local owner process are started",
    )
    args = parser.parse_args()
    if not args.slot_granted:
        parser.error("refusing public-source access or owner startup until the root grants the canary slot")
    if args.max_pages not in (1, 2):
        parser.error("--max-pages is limited to 1..2 for this canary")
    if not 1 <= args.search_limit <= 200 or args.repetitions < 2:
        parser.error("--search-limit must be 1..200 and --repetitions must be at least 2")
    if args.ingest_wait_seconds <= 0 or args.ingest_poll_seconds <= 0:
        parser.error("ingest wait and poll durations must be positive")
    frozen = prepare_frozen_inputs(args)
    output_arg = args.output.expanduser()
    if not output_arg.name or output_arg.name in {".", ".."}:
        parser.error("--output must name a fresh directory")
    output = output_arg.parent.resolve(strict=True) / output_arg.name
    if os.path.lexists(output):
        parser.error(f"output already exists; choose a new private run path: {output}")
    output.mkdir(mode=0o700)
    try:
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
                "owner_log": str(output / "owner-live.log"),
                "restore_log": str(output / "owner-restore-offline.log"),
            },
        )
        raise


if __name__ == "__main__":
    raise SystemExit(main())
