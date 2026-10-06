#!/usr/bin/env python3
"""Count distinct packages from hash-bound runtime evidence, never acquisition.

Language workers append one immutable attempt record per package. This reader
selects the latest attempt for one exact candidate manifest and checks the
referenced acceptance result before granting credit. Older builds, alternate
versions and retries cannot inflate a checkpoint's package count.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
from collections import Counter
from contextlib import contextmanager
from dataclasses import dataclass, replace
from pathlib import Path, PurePosixPath
from typing import Any

ATTEMPT_SCHEMA = "nudox.language-corpus-attempt.v1"
RESULT_SCHEMA = "nudox.real-workspace-index-acceptance-result.v1"
MAX_LINE_BYTES = 64 * 1024
MAX_RESULT_BYTES = 32 * 1024 * 1024
MAX_JOURNAL_BYTES = 256 * 1024 * 1024
MAX_ATTEMPT_RECORDS = 500_000
MAX_DISTINCT_PACKAGES = 100_000
LANGUAGES = ("typescript", "python", "go")
PREFIX = {"typescript": "npm:", "python": "pypi:", "go": "go:"}
REQUIRED = {
    "schema", "language", "package", "version", "source_sha256",
    "candidate_manifest_sha256", "attempt", "setup", "result_path", "result_sha256",
}


class InvalidEvidence(ValueError):
    """Evidence cannot establish a counted runtime pass."""


def digest(value: Any) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise InvalidEvidence("expected an exact lowercase SHA-256")
    return value


def object_without_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise InvalidEvidence("duplicate JSON field")
        value[key] = item
    return value


def decode(raw: bytes) -> Any:
    try:
        return json.loads(raw, object_pairs_hook=object_without_duplicates)
    except (ValueError, UnicodeError) as error:
        raise InvalidEvidence("malformed evidence JSON") from error


def read_result(path: Path) -> bytes:
    """Read one stable regular receipt without blocking on a special file."""
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC
    descriptor = os.open(path, flags)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise InvalidEvidence("result must be a single-link regular file")
        if before.st_size > MAX_RESULT_BYTES:
            raise InvalidEvidence("result exceeded its byte bound")
        raw = stream.read(MAX_RESULT_BYTES + 1)
        after = os.fstat(stream.fileno())
        selected = os.stat(path, follow_symlinks=False)
    identity = lambda value: (value.st_dev, value.st_ino, value.st_size,
                              value.st_mtime_ns, value.st_ctime_ns, value.st_nlink)
    if (identity(before) != identity(after) or identity(after) != identity(selected)
            or not stat.S_ISREG(selected.st_mode) or len(raw) != after.st_size):
        raise InvalidEvidence("result changed while being read")
    return raw


@contextmanager
def open_journal(path: Path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(descriptor, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise InvalidEvidence("journal must be a single-link regular file")
        if info.st_size > MAX_JOURNAL_BYTES:
            raise InvalidEvidence("journal exceeded its byte bound")
        yield stream


def verify_package_binding(attempt: Attempt, project: dict[str, Any]) -> None:
    provenance = project.get("package_provenance")
    if (not isinstance(provenance, dict)
            or provenance.get("verification") != "verified-source-inventory-v1"):
        raise InvalidEvidence("pass has no independently verified package source inventory")
    if provenance.get("origin_verification") != "verified-registry-artifact-v1":
        raise InvalidEvidence("pass has no verified registry artifact identity and tree membership")
    origin_evidence = provenance.get("origin_evidence")
    if not isinstance(origin_evidence, dict):
        raise InvalidEvidence("pass omits the retained registry artifact evidence")
    for field in ("metadata_sha256", "archive_sha256", "archive_membership_sha256"):
        digest(origin_evidence.get(field))
    expected = {"ecosystem": PREFIX[attempt.language][:-1],
                "id": attempt.package[len(PREFIX[attempt.language]):], "version": attempt.version}
    origin = provenance.get("package")
    if provenance.get("target_package") != expected:
        raise InvalidEvidence("pass belongs to another package or pinned version")
    if digest(provenance.get("source_tree_sha256")) != attempt.source_sha256:
        raise InvalidEvidence("pass belongs to another acquired source tree")
    digest(provenance.get("acquired_inventory_sha256"))
    digest(provenance.get("corpus_manifest_sha256"))
    if (digest(provenance.get("target_root_identity_sha256"))
            != digest(project.get("root_identity_sha256"))):
        raise InvalidEvidence("pass names another runtime target root")
    target = provenance.get("target_subdir")
    if not isinstance(target, str) or not target or len(target.encode()) > 4096:
        raise InvalidEvidence("pass omits its canonical package target subdirectory")
    if target != ".":
        parts = PurePosixPath(target)
        if (parts.is_absolute() or str(parts) != target or "\\" in target
                or any(part in {".", ".."} for part in parts.parts)):
            raise InvalidEvidence("pass names a noncanonical package target subdirectory")
    if attempt.language == "go":
        if (not isinstance(origin, dict) or set(origin) != {"ecosystem", "id", "version"}
                or origin.get("ecosystem") != "go" or origin.get("version") != attempt.version
                or not isinstance(origin.get("id"), str) or not origin["id"]
                or origin["id"].endswith("/")):
            raise InvalidEvidence("Go pass omits its exact module origin")
        import_path = origin["id"] + ("/" + target if target != "." else "")
        if import_path != expected["id"]:
            raise InvalidEvidence("Go package target differs from module/subdirectory identity")
    elif origin != expected:
        raise InvalidEvidence("package target differs from its acquired registry origin")


def verify_setup_binding(attempt: Attempt, run: dict[str, Any]) -> None:
    setup = run.get("runtime_setup")
    witnesses = run.get("environment_witnesses")
    if not isinstance(setup, dict) or not isinstance(witnesses, dict):
        raise InvalidEvidence("pass has no observed runtime setup evidence")
    if setup.get("mode") != attempt.setup:
        raise InvalidEvidence("attempt setup label differs from the executed setup")
    if (digest(setup.get("owner_environment_sha256"))
            != digest(witnesses.get("owner_allowlisted_environment_sha256"))):
        raise InvalidEvidence("setup belongs to another owner environment")
    overrides = setup.get("compiler_override_keys")
    injected = setup.get("compiler_snapshot_injected")
    if (type(injected) is not bool or not isinstance(overrides, list)
            or any(not isinstance(key, str) or not re.fullmatch(r"[A-Z][A-Z0-9_]{0,127}", key)
                   for key in overrides) or overrides != sorted(set(overrides))):
        raise InvalidEvidence("setup omits its exact compiler override policy")
    if attempt.setup == "stock" and (injected or overrides):
        raise InvalidEvidence("configured compiler setup cannot count as a stock pass")


@dataclass(frozen=True)
class Attempt:
    language: str
    package: str
    version: str
    source_sha256: str
    candidate_manifest_sha256: str
    attempt: int
    setup: str
    result_path: str | None
    result_sha256: str | None

    @classmethod
    def parse(cls, value: Any) -> Attempt:
        if not isinstance(value, dict) or set(value) != REQUIRED or value["schema"] != ATTEMPT_SCHEMA:
            raise InvalidEvidence("unknown attempt schema or fields")
        language = value["language"]
        package = value["package"]
        if language not in LANGUAGES or not isinstance(package, str):
            raise InvalidEvidence("unknown language or package identity")
        if (not package.startswith(PREFIX[language]) or len(package) > 1024
                or any(ord(c) < 33 or ord(c) > 126 for c in package)):
            raise InvalidEvidence("package must use a canonical ecosystem identity")
        name = package[len(PREFIX[language]):]
        if not name or (language == "python" and re.sub(r"[-_.]+", "-", name).lower() != name):
            raise InvalidEvidence("noncanonical package name")
        if language == "typescript" and name.lower() != name:
            raise InvalidEvidence("npm package name is not canonical")
        version = value["version"]
        if not isinstance(version, str) or not version or len(version) > 256:
            raise InvalidEvidence("missing or oversized pinned version")
        if type(value["attempt"]) is not int or value["attempt"] <= 0:
            raise InvalidEvidence("attempt ordinal must be a positive integer")
        if value["setup"] not in {"stock", "configured"}:
            raise InvalidEvidence("setup must distinguish stock from configured")
        path, result_hash = value["result_path"], value["result_sha256"]
        if (path is None) != (result_hash is None):
            raise InvalidEvidence("result path and hash must be paired")
        if path is not None:
            if not isinstance(path, str) or not Path(path).is_absolute() or len(path) > 4096:
                raise InvalidEvidence("result path must be absolute and bounded")
            digest(result_hash)
        return cls(language, package, version, digest(value["source_sha256"]),
                   digest(value["candidate_manifest_sha256"]), value["attempt"],
                   value["setup"], path, result_hash)


def runtime_status(attempt: Attempt) -> tuple[str, str]:
    if attempt.result_path is None:
        return "pending", "runtime acceptance has not completed"
    path = Path(attempt.result_path)
    raw = read_result(path)
    if len(raw) > MAX_RESULT_BYTES or hashlib.sha256(raw).hexdigest() != attempt.result_sha256:
        raise InvalidEvidence("result exceeded its bound or changed after publication")
    run = decode(raw)
    if not isinstance(run, dict) or run.get("schema") != RESULT_SCHEMA:
        raise InvalidEvidence("result is not a real-workspace runtime receipt")
    scope = run.get("scope", {})
    if scope.get("kind") != "language-corpus" or scope.get("language") != attempt.language:
        raise InvalidEvidence("runtime result belongs to another acceptance scope")
    if run.get("status") != "PASS":
        status = run.get("status")
        if status not in {"RUNNING", "FAIL", "BLOCKED"}:
            raise InvalidEvidence("unknown runtime result status")
        return status.lower(), str(run.get("reason", status))[:1024]
    source = run.get("source", {})
    if source.get("build_manifest_sha256") != attempt.candidate_manifest_sha256:
        raise InvalidEvidence("runtime pass belongs to another candidate manifest")
    operations = run.get("operations")
    queries = run.get("queries")
    projects = run.get("projects")
    if not isinstance(projects, list) or len(projects) != 1:
        raise InvalidEvidence("a counted package needs one isolated project receipt")
    project_id = projects[0].get("id")
    if not project_id or projects[0].get("source_tree_unchanged") is not True:
        raise InvalidEvidence("package source changed during acceptance")
    verify_package_binding(attempt, projects[0])
    verify_setup_binding(attempt, run)
    if (not isinstance(operations, list) or len(operations) != 1
            or operations[0].get("project_id") != project_id
            or operations[0].get("terminal_state") != "published"
            or operations[0].get("cold_status_same_receipt") is not True
            or operations[0].get("same_key_replay_same_receipt") is not True):
        raise InvalidEvidence("pass omits exact publication or cold replay evidence")
    digest(operations[0].get("publication_receipt_sha256"))
    if not isinstance(queries, list) or not queries:
        raise InvalidEvidence("pass omits expected semantic query results")
    eligible = {"typescript", "tsx"} if attempt.language == "typescript" else {attempt.language}
    if not any(q.get("profile") in eligible for q in queries if isinstance(q, dict)):
        raise InvalidEvidence("pass has no semantic query for its counted language")
    for query in queries:
        if (not isinstance(query, dict) or query.get("project_id") != project_id
                or type(query.get("matched_records")) is not int or query["matched_records"] <= 0):
            raise InvalidEvidence("empty or foreign semantic query cannot count")
        digest(query.get("identity_sha256"))
    before = run.get("semantic_versions_before_restart")
    after = run.get("semantic_versions_after_restart")
    if not isinstance(before, dict) or not before or before != after:
        raise InvalidEvidence("pass omits stable selected semantic history after restart")
    return "passed_" + attempt.setup, "verified publication, query and restart receipt"


def summarize(journals: list[Path], candidate: str, target: int = 10_000) -> dict[str, Any]:
    digest(candidate)
    if type(target) is not int or target <= 0:
        raise InvalidEvidence("target must be positive")
    latest: dict[tuple[str, str], Attempt] = {}
    other_builds = 0
    incomplete_tails = 0
    records = 0
    for journal in journals:
        consumed = 0
        with open_journal(journal) as stream:
            while raw := stream.readline(MAX_LINE_BYTES + 1):
                consumed += len(raw)
                records += 1
                if consumed > MAX_JOURNAL_BYTES or records > MAX_ATTEMPT_RECORDS:
                    raise InvalidEvidence("attempt history exceeded its byte or record bound")
                if len(raw) > MAX_LINE_BYTES:
                    raise InvalidEvidence("attempt record exceeded its byte bound")
                if not raw.endswith(b"\n"):
                    incomplete_tails += 1
                    break  # A crash during append cannot manufacture a pass.
                attempt = Attempt.parse(decode(raw))
                if attempt.candidate_manifest_sha256 != candidate:
                    other_builds += 1
                    continue
                key = (attempt.language, attempt.package)
                prior = latest.get(key)
                if prior is None and len(latest) >= MAX_DISTINCT_PACKAGES:
                    raise InvalidEvidence("distinct package history exceeded its bound")
                if prior is None or attempt.attempt > prior.attempt:
                    latest[key] = attempt
                elif attempt.attempt == prior.attempt and attempt != prior:
                    identity = replace(attempt, result_path=None, result_sha256=None)
                    if identity != replace(prior, result_path=None, result_sha256=None):
                        raise InvalidEvidence("conflicting evidence for the same package attempt")
                    if prior.result_path is None:
                        latest[key] = attempt  # Complete the exact pending attempt.
                    elif attempt.result_path is not None:
                        raise InvalidEvidence("two completed results for the same package attempt")
    counts = {language: Counter() for language in LANGUAGES}
    failures: dict[str, Counter[str]] = {language: Counter() for language in LANGUAGES}
    for attempt in latest.values():
        try:
            status, reason = runtime_status(attempt)
        except (InvalidEvidence, OSError, TypeError, AttributeError) as error:
            status, reason = "invalid_evidence", str(error)[:1024]
        counts[attempt.language][status] += 1
        if status not in {"passed_stock", "passed_configured"}:
            failures[attempt.language][reason] += 1
    return {
        "schema": "nudox.language-corpus-summary.v1",
        "candidate_manifest_sha256": candidate,
        "target_distinct_packages_per_language": target,
        "other_build_attempts_excluded": other_builds,
        "incomplete_journal_tails": incomplete_tails,
        "languages": {
            language: {"counts": dict(counts[language]),
                       "remaining_stock_passes": max(0, target - counts[language]["passed_stock"]),
                       "failure_families": dict(failures[language].most_common(30))}
            for language in LANGUAGES
        },
        "target_met": all(counts[language]["passed_stock"] >= target for language in LANGUAGES),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--journal", action="append", type=Path, required=True)
    parser.add_argument("--candidate-manifest-sha256", required=True)
    parser.add_argument("--target", type=int, default=10_000)
    args = parser.parse_args()
    try:
        summary = summarize(args.journal, args.candidate_manifest_sha256, args.target)
    except (InvalidEvidence, OSError) as error:
        parser.exit(2, f"cannot establish corpus counts: {error}\n")
    print(json.dumps(summary, sort_keys=True, indent=2))
    return 0 if summary["target_met"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
