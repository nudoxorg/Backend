#!/usr/bin/env python3
"""Validate and run synthetic adversarial search holdout cases."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Any


HERE = Path(__file__).resolve().parent
SPEC_PATH = HERE / "cases.json"
SOURCE_ID = "benchmark-holdout:search-quality:v1"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def facet(status: str = "unknown", values: list[Any] | None = None) -> dict[str, Any]:
    return {
        "status": status,
        "values": values or [],
        "source_snapshot_id": SOURCE_ID if status == "known" else None,
    }


def make_document(
    ecosystem: str,
    name: str,
    version: str,
    document_id: str,
    alias: str | None = None,
) -> dict[str, Any]:
    alias_facet = facet("known", [alias]) if alias is not None else facet()
    return {
        "data_class": "adversarial-fixture",
        "document_id": document_id,
        "ecosystem": ecosystem,
        "name": name,
        "version": version,
        "coordinate": f"fixture:{ecosystem}/{name}@{version}",
        "source_kind": "registry-fixture",
        "source_snapshot_id": SOURCE_ID,
        "archive_url": None,
        "archive_digest": None,
        "fields": {
            "aliases": alias_facet,
            "keywords": facet(),
            "description": facet(),
            "readme": facet(),
            "dependencies": facet(),
            "advisories": facet(),
            "yanked": facet(),
        },
    }


def make_corpus() -> list[dict[str, Any]]:
    documents = [
        make_document(
            "pypi",
            "scope-target",
            "9.0.0",
            "fixture:holdout/pypi/scope-target@9.0.0",
            "ecosystem-overflow-sentinel",
        )
    ]
    for ordinal in range(257):
        name = f"scope-noise-{ordinal:03d}"
        documents.append(
            make_document(
                "npm",
                name,
                "1.0.0",
                f"fixture:holdout/npm/{name}@1.0.0",
                "ecosystem-overflow-sentinel",
            )
        )

    documents.extend(
        [
            make_document(
                "cargo",
                "shared-source-name",
                "1.0.0",
                "fixture:holdout/cargo/shared-source-name@1.0.0",
            ),
            make_document(
                "pypi",
                "shared-source-name",
                "1.0.0",
                "fixture:holdout/pypi/shared-source-name@1.0.0",
            ),
        ]
    )

    for version in ("1.0.0", "1.1.0"):
        documents.append(
            make_document(
                "cargo",
                "lineage-alpha",
                version,
                f"fixture:holdout/cargo/lineage-alpha@{version}",
                "lineage-holdout-alias",
            )
        )
    documents.append(
        make_document(
            "cargo",
            "lineage-beta",
            "1.0.0",
            "fixture:holdout/cargo/lineage-beta@1.0.0",
            "lineage-holdout-alias",
        )
    )

    for version in ("1.0.0", "2.0.0"):
        documents.append(
            make_document(
                "npm",
                "@holdout/retract-me",
                version,
                f"fixture:holdout/npm/@holdout/retract-me@{version}",
                "withdraw-holdout-alias",
            )
        )
    return sorted(documents, key=lambda row: row["document_id"])


def request(case: dict[str, Any], operation: str = "search", limit: int = 50) -> dict[str, Any]:
    return {
        "op": operation,
        "query_id": case["id"],
        "query": case["query"],
        "category": case["id"],
        "corpus": "adversarial",
        "scope": {"ecosystems": case["scope"]},
        "exclude_yanked": False,
        "limit": limit,
    }


def validate_group_response(response: dict[str, Any], expected: dict[str, list[str]]) -> list[str]:
    errors: list[str] = []
    groups = response.get("groups")
    if not isinstance(groups, list):
        return ["group response has no groups array"]
    observed: dict[str, list[str]] = {}
    identities: set[tuple[str, str, str]] = set()
    for group in groups:
        if not isinstance(group, dict):
            errors.append("group entry is not an object")
            continue
        identity = (
            str(group.get("ecosystem")),
            str(group.get("source_id")),
            str(group.get("lineage")),
        )
        if identity in identities:
            errors.append(f"duplicate source-scoped lineage: {identity}")
        identities.add(identity)
        lineage = str(group.get("lineage"))
        ids = group.get("document_ids")
        if not isinstance(ids, list) or any(not isinstance(value, str) for value in ids):
            errors.append(f"invalid document_ids for lineage {lineage}")
            continue
        if len(ids) != len(set(ids)):
            errors.append(f"duplicate release document in lineage {lineage}")
        if lineage in observed:
            errors.append(f"lineage emitted more than once: {lineage}")
        observed[lineage] = ids
    if {key: set(value) for key, value in observed.items()} != {
        key: set(value) for key, value in expected.items()
    }:
        errors.append(f"lineage membership mismatch: expected {expected}, observed {observed}")
    return errors


def validate_retraction_response(
    response: dict[str, Any], expected_ids: list[str], expected_standing: str
) -> list[str]:
    errors: list[str] = []
    ids = response.get("document_ids")
    if not isinstance(ids, list) or set(ids) != set(expected_ids) or len(ids) != len(set(ids)):
        errors.append(f"retracted package result ids differ: expected {expected_ids}, observed {ids}")
    standings = response.get("standing_by_document_id")
    if not isinstance(standings, dict):
        return errors + ["search response has no standing_by_document_id map"]
    for document_id in expected_ids:
        if standings.get(document_id) != expected_standing:
            errors.append(
                f"release {document_id} should be {expected_standing}, observed {standings.get(document_id)}"
            )
    return errors


def falsification_controls(spec: dict[str, Any]) -> dict[str, bool]:
    lineage_case = next(case for case in spec["cases"] if case["id"] == "duplicate-lineage-group")
    baseline_groups = [
        {
            "ecosystem": "cargo",
            "source_id": "source-a",
            "lineage": lineage,
            "document_ids": document_ids,
            "more_releases": False,
        }
        for lineage, document_ids in lineage_case["expected_groups"].items()
    ]
    duplicated = {"groups": baseline_groups + [copy.deepcopy(baseline_groups[0])]}
    duplicate_detected = bool(validate_group_response(duplicated, lineage_case["expected_groups"]))

    retraction_case = next(case for case in spec["cases"] if case["id"] == "npm-package-retraction")
    missed_retraction = {
        "document_ids": retraction_case["expected_document_ids"],
        "standing_by_document_id": {
            retraction_case["expected_document_ids"][0]: "withdrawn",
            retraction_case["expected_document_ids"][1]: "available",
        },
    }
    retract_detected = bool(
        validate_retraction_response(
            missed_retraction,
            retraction_case["expected_document_ids"],
            retraction_case["expected_retraction_standing"],
        )
    )
    return {
        "duplicate_lineage_mutation_detected": duplicate_detected,
        "missed_version_retraction_mutation_detected": retract_detected,
    }


def validate_fixture() -> dict[str, Any]:
    spec = json.loads(SPEC_PATH.read_text(encoding="utf-8"))
    rows = make_corpus()
    errors: list[str] = []
    ids = [row["document_id"] for row in rows]
    if len(ids) != len(set(ids)):
        errors.append("holdout document ids are not unique")
    if any(row["source_snapshot_id"] != spec["source_snapshot_id"] for row in rows):
        errors.append("fixture source snapshot labels disagree with the case manifest")
    if sum(row["ecosystem"] == "npm" and row["name"].startswith("scope-noise-") for row in rows) != 257:
        errors.append("scope overflow fixture must contain exactly 257 NPM distractors")
    controls = falsification_controls(spec)
    if not all(controls.values()):
        errors.append(f"falsification controls failed: {controls}")
    return {
        "holdout_id": spec["holdout_id"],
        "fixture_document_count": len(rows),
        "fixture_sha256": hashlib.sha256(
            "".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in rows).encode()
        ).hexdigest(),
        "falsification_controls": controls,
        "errors": errors,
    }


def command_json(command: list[str], input_value: dict[str, Any] | None = None) -> dict[str, Any]:
    completed = subprocess.run(
        command,
        input=json.dumps(input_value, separators=(",", ":")) if input_value is not None else None,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(f"command failed ({completed.returncode}): {shlex.join(command)}\n{completed.stderr[-3000:]}")
    if not completed.stdout.strip():
        return {}
    value = json.loads(completed.stdout.splitlines()[-1])
    if not isinstance(value, dict):
        raise ValueError(f"command returned non-object JSON: {shlex.join(command)}")
    if "error" in value:
        raise RuntimeError(f"adapter error for {shlex.join(command)}: {value['error']}")
    return value


def cold_search(adapter: list[str], index: Path, query: dict[str, Any], work: Path) -> dict[str, Any]:
    query_path = work / f"{query['query_id']}.json"
    query_path.write_text(json.dumps(query, separators=(",", ":")), encoding="utf-8")
    return command_json(
        adapter
        + [
            "search-cold",
            "--index",
            str(index),
            "--query-json",
            str(query_path),
            "--limit",
            str(query.get("limit", 50)),
        ]
    )


def read_service_line(process: subprocess.Popen[str]) -> dict[str, Any]:
    if process.stdout is None:
        raise RuntimeError("adapter service stdout is unavailable")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError("adapter service exited before responding")
    value = json.loads(line)
    if not isinstance(value, dict) or "error" in value:
        raise RuntimeError(f"adapter service response failed: {value}")
    return value


def send_service_request(process: subprocess.Popen[str], value: dict[str, Any]) -> dict[str, Any]:
    if process.stdin is None:
        raise RuntimeError("adapter service stdin is unavailable")
    process.stdin.write(json.dumps(value, separators=(",", ":")) + "\n")
    process.stdin.flush()
    return read_service_line(process)


def run_holdout(adapter_command: str, result_dir: Path) -> dict[str, Any]:
    if os.environ.get("BENCH_BUILD_SLOT_GRANTED") != "1":
        raise ValueError("refusing adapter build/search until the repository owner grants a build slot")
    adapter = shlex.split(adapter_command)
    if not adapter:
        raise ValueError("adapter command is empty")
    result_dir = result_dir.resolve()
    if not result_dir.is_dir():
        raise ValueError(f"result directory must already exist: {result_dir}")
    corpus_path = result_dir / "holdout-corpus.jsonl"
    index_path = result_dir / "holdout-index"
    output_path = result_dir / "holdout-results.json"
    if corpus_path.exists() or index_path.exists() or output_path.exists():
        raise FileExistsError("refusing to overwrite existing holdout evidence")

    spec = json.loads(SPEC_PATH.read_text(encoding="utf-8"))
    rows = make_corpus()
    corpus_path.write_text(
        "".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in rows),
        encoding="utf-8",
    )
    build = command_json(adapter + ["build", "--corpus", str(corpus_path), "--index", str(index_path)])
    work = result_dir / "queries"
    work.mkdir()
    checks: list[dict[str, Any]] = []

    overflow = next(case for case in spec["cases"] if case["id"] == "scope-before-pagination")
    response = cold_search(
        adapter,
        index_path,
        request(overflow, limit=10),
        work,
    )
    observed = response.get("document_ids", [])
    overflow_passed = observed == overflow["expected_document_ids"]
    checks.append(
        {
            "id": overflow["id"],
            "passed": overflow_passed,
            "expected_document_ids": overflow["expected_document_ids"],
            "observed_document_ids": observed,
            "assertion": overflow["assertion"],
        }
    )

    collision = next(case for case in spec["cases"] if case["id"] == "cross-source-exact-name")
    response = cold_search(adapter, index_path, request(collision, limit=10), work)
    observed = response.get("document_ids", [])
    collision_passed = set(observed) == set(collision["expected_document_ids"]) and len(observed) == len(set(observed))
    checks.append(
        {
            "id": collision["id"],
            "passed": collision_passed,
            "expected_document_ids": collision["expected_document_ids"],
            "observed_document_ids": observed,
            "assertion": collision["assertion"],
        }
    )

    lineage = next(case for case in spec["cases"] if case["id"] == "duplicate-lineage-group")
    group_query = request(lineage, operation="search-groups", limit=10)
    response = cold_search(adapter, index_path, group_query, work)
    group_errors = validate_group_response(response, lineage["expected_groups"])
    checks.append(
        {
            "id": lineage["id"],
            "passed": not group_errors,
            "errors": group_errors,
            "observed_groups": response.get("groups"),
            "assertion": lineage["assertion"],
        }
    )

    retraction = next(case for case in spec["cases"] if case["id"] == "npm-package-retraction")
    retraction_query = request(retraction, limit=10)
    retraction_query["include_standing"] = True
    service = subprocess.Popen(
        adapter + ["serve", "--index", str(index_path), "--limit", "10"],
        text=True,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=1,
    )
    try:
        before = send_service_request(service, retraction_query)
        before_errors = validate_retraction_response(
            before,
            retraction["expected_document_ids"],
            "available",
        )
        event = send_service_request(
            service,
            {
                "op": "retract-package",
                "package_name": retraction["package_name"],
            },
        )
        after = send_service_request(service, retraction_query)
        after_errors = validate_retraction_response(
            after,
            retraction["expected_document_ids"],
            retraction["expected_retraction_standing"],
        )
        retract_passed = not before_errors and not after_errors and event.get("updated_releases") == 2
        checks.append(
            {
                "id": retraction["id"],
                "passed": retract_passed,
                "before_errors": before_errors,
                "retraction_event": event,
                "after_errors": after_errors,
                "before_standings": before.get("standing_by_document_id"),
                "after_standings": after.get("standing_by_document_id"),
                "assertion": retraction["assertion"],
            }
        )
    finally:
        if service.stdin is not None:
            service.stdin.close()
        try:
            service.wait(timeout=10)
        except subprocess.TimeoutExpired:
            service.terminate()
            service.wait(timeout=5)

    result = {
        "holdout_id": spec["holdout_id"],
        "lane": spec["lane"],
        "case_manifest_sha256": sha256(SPEC_PATH),
        "runner_sha256": sha256(Path(__file__).resolve()),
        "fixture_sha256": sha256(corpus_path),
        "adapter_build_report": build,
        "checks": checks,
        "passed": all(check["passed"] for check in checks),
        "performance_claims": False,
        "quality_qrel_scored": False,
    }
    output_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate", help="validate generated holdout fixtures and mutation controls")
    run = subparsers.add_parser("run", help="build and check the synthetic holdout with an adapter")
    run.add_argument("--adapter-command", required=True)
    run.add_argument("--result-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "validate":
            result = validate_fixture()
            print(json.dumps(result, indent=2, sort_keys=True))
            return 0 if not result["errors"] else 1
        result = run_holdout(args.adapter_command, args.result_dir)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0 if result["passed"] else 1
    except (OSError, ValueError, RuntimeError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
