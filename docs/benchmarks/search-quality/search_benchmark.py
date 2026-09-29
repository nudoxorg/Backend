#!/usr/bin/env python3
"""Validate, score, and measure the frozen multi-ecosystem search benchmark."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import platform
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from pathlib import Path
from typing import Any, TextIO


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
CORPUS_FILES = {
    "primary": HERE / "primary-corpus.jsonl",
    "adversarial": HERE / "adversarial-corpus.jsonl",
}
QUERY_FILE = HERE / "queries.jsonl"
QREL_FILE = HERE / "qrels.tsv"
PAIR_FILE = HERE / "version-pairs.tsv"
SOURCE_FILE = HERE / "source-snapshots.json"
MUTATION_FILE = HERE / "mutations.json"


def frozen_files() -> list[Path]:
    files = {
        path
        for path in HERE.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts and path.suffix != ".pyc"
    }
    if SOURCE_FILE.is_file():
        source_manifest = json.loads(SOURCE_FILE.read_text(encoding="utf-8"))
        for source in source_manifest.get("source_snapshots", []):
            for field in ("path", "local_file"):
                relative = source.get(field)
                if isinstance(relative, str):
                    files.add(ROOT / relative)
    return sorted(files)


FROZEN_FILES = frozen_files()
DEFAULT_CUTOFFS = [1, 5, 10]
DEFAULT_SEED = 20260928


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def workspace_build_lock_snapshot() -> dict[str, Any]:
    path = ROOT / "Cargo.lock"
    return {
        "path": "Cargo.lock",
        "sha256": sha256(path),
        "bytes": path.stat().st_size,
        "scope": "execution context only; excluded from frozen corpus and QREL evidence",
    }


def input_hashes() -> dict[str, str]:
    return {path.relative_to(ROOT).as_posix(): sha256(path) for path in FROZEN_FILES}


def executable_snapshot(adapter: list[str]) -> dict[str, str | None]:
    token = adapter[0]
    candidate = Path(token)
    if not candidate.is_absolute():
        located = shutil.which(token)
        candidate = Path(located) if located else ROOT / candidate
    try:
        resolved = candidate.resolve(strict=True)
    except OSError:
        return {"path": token, "sha256": None}
    if not resolved.is_file():
        return {"path": str(resolved), "sha256": None}
    return {"path": str(resolved), "sha256": sha256(resolved)}


def fingerprint(hashes: dict[str, str]) -> str:
    body = "".join(f"{digest}  {name}\n" for name, digest in sorted(hashes.items()))
    return hashlib.sha256(body.encode()).hexdigest()


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            if not line.strip():
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
            if not isinstance(value, dict):
                raise ValueError(f"{path}:{line_number}: expected one JSON object")
            rows.append(value)
    return rows


def load_tsv(path: Path) -> list[dict[str, str]]:
    with path.open(encoding="utf-8", newline="") as stream:
        return list(csv.DictReader(stream, delimiter="\t"))


def load_data() -> dict[str, Any]:
    corpora = {name: load_jsonl(path) for name, path in CORPUS_FILES.items()}
    queries = load_jsonl(QUERY_FILE)
    qrel_rows = load_tsv(QREL_FILE)
    pairs = load_tsv(PAIR_FILE)
    sources = json.loads(SOURCE_FILE.read_text(encoding="utf-8"))
    mutations = json.loads(MUTATION_FILE.read_text(encoding="utf-8"))
    documents = {row["document_id"]: row for rows in corpora.values() for row in rows}
    query_by_id = {row["query_id"]: row for row in queries}
    qrels: dict[str, dict[str, int]] = defaultdict(dict)
    for row in qrel_rows:
        qrels[row["query_id"]][row["document_id"]] = int(row["grade"])
    return {
        "corpora": corpora,
        "documents": documents,
        "queries": queries,
        "query_by_id": query_by_id,
        "qrel_rows": qrel_rows,
        "qrels": qrels,
        "pairs": pairs,
        "sources": sources,
        "mutations": mutations,
    }


def validate_data(data: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    documents = data["documents"]
    queries = data["queries"]
    query_by_id = data["query_by_id"]
    source_ids = {row["snapshot_id"] for row in data["sources"]["source_snapshots"]}
    if ROOT / "Cargo.lock" in FROZEN_FILES:
        errors.append("live root Cargo.lock must remain outside frozen benchmark inputs")
    lock_sources = [
        row for row in data["sources"]["source_snapshots"]
        if str(row.get("snapshot_id", "")).startswith("cargo-lock-snapshot:")
    ]
    if len(lock_sources) != 1:
        errors.append("expected exactly one immutable captured Cargo.lock evidence snapshot")
    for row in data["sources"]["source_snapshots"]:
        if row.get("path") == "Cargo.lock" or row.get("local_file") == "Cargo.lock":
            errors.append("live root Cargo.lock must not be a frozen source or QREL dependency")
    for row in lock_sources:
        relative = row.get("local_file")
        if not isinstance(relative, str):
            errors.append("captured Cargo.lock evidence has no local snapshot file")
            continue
        path = ROOT / relative
        if not path.is_file():
            errors.append(f"captured Cargo.lock evidence file is missing: {relative}")
        elif sha256(path) != row.get("sha256"):
            errors.append(f"captured Cargo.lock evidence hash mismatch: {relative}")

    all_ids = [row["document_id"] for rows in data["corpora"].values() for row in rows]
    if len(all_ids) != len(set(all_ids)):
        errors.append("document_id is not unique across the corpora")
    query_ids = [row["query_id"] for row in queries]
    if len(query_ids) != len(set(query_ids)):
        errors.append("query_id is not unique")

    for corpus_name, rows in data["corpora"].items():
        for row in rows:
            if row.get("data_class") == "adversarial-fixture" and corpus_name != "adversarial":
                errors.append(f"synthetic fixture leaked into primary corpus: {row.get('document_id')}")
            if row.get("data_class") != "adversarial-fixture" and corpus_name == "adversarial":
                errors.append(f"adversarial corpus row lacks explicit fixture label: {row.get('document_id')}")
            source_id = row.get("source_snapshot_id")
            if not source_id or not any(source_id == value or source_id.startswith(value + "#") for value in source_ids):
                errors.append(f"unresolved document source snapshot id: {row.get('document_id')} / {source_id}")
            if row.get("source_kind") == "forge" and row.get("ecosystem") != "forge":
                errors.append(f"forge record has non-forge ecosystem: {row.get('document_id')}")
            if row.get("source_kind") == "forge" and row.get("archive_sha256_sri") is None and row.get("archive_digest") is None:
                errors.append(f"forge record lacks a pinned source digest: {row.get('document_id')}")
            for forbidden in ("downloads", "monthly_downloads", "download_count"):
                if forbidden in row:
                    errors.append(f"synthetic download metric in {row.get('document_id')}: {forbidden}")
            for field_name, facet in row.get("fields", {}).items():
                if facet.get("status") not in {"known", "absent", "unknown"}:
                    errors.append(f"invalid tri-state {field_name} on {row.get('document_id')}")
                if facet.get("status") == "known" and not facet.get("source_snapshot_id"):
                    errors.append(f"known {field_name} has no source snapshot: {row.get('document_id')}")

    for query in queries:
        query_id = query.get("query_id")
        if query.get("corpus") not in data["corpora"]:
            errors.append(f"unknown corpus for query {query_id}")
        if not query.get("query", "").strip():
            errors.append(f"empty query text: {query_id}")
        grades = data["qrels"].get(query_id, {})
        if query.get("expected_empty"):
            if grades:
                errors.append(f"expected-empty query has qrels: {query_id}")
        elif not any(grade > 0 for grade in grades.values()):
            errors.append(f"nonempty query lacks a positive independent judgment: {query_id}")
        for document_id in grades:
            if document_id not in documents:
                errors.append(f"qrel references missing document: {query_id} / {document_id}")
            elif document_id not in {row["document_id"] for row in data["corpora"][query["corpus"]]}:
                errors.append(f"qrel crosses corpus lane: {query_id} / {document_id}")
            elif documents[document_id].get("ecosystem") not in query.get("scope", {}).get("ecosystems", []):
                errors.append(f"qrel document is outside query ecosystem scope: {query_id} / {document_id}")
        for row in data["qrel_rows"]:
            if row["query_id"] == query_id and row["source_snapshot_id"] not in source_ids:
                errors.append(f"qrel has unresolved source snapshot id: {query_id} / {row['source_snapshot_id']}")

    for pair in data["pairs"]:
        query_id = pair["query_id"]
        if query_id not in query_by_id:
            errors.append(f"version pair has unknown query: {query_id}")
        for column in ("newer_document_id", "older_document_id"):
            if pair[column] not in documents:
                errors.append(f"version pair references missing document: {query_id} / {pair[column]}")
        if data["qrels"].get(query_id, {}).get(pair["newer_document_id"], 0) <= data["qrels"].get(query_id, {}).get(pair["older_document_id"], 0):
            errors.append(f"version pair direction disagrees with its independent judgments: {query_id}")

    for mutation in data["mutations"]["checks"]:
        query_id = mutation.get("query_id")
        if query_id not in query_by_id:
            errors.append(f"mutation references unknown query: {mutation.get('id')}")
        target = mutation.get("document_id") or mutation.get("promote_document_id")
        if target and target not in documents:
            errors.append(f"mutation references missing document: {mutation.get('id')} / {target}")
    return errors


def verify_source_freeze() -> None:
    completed = subprocess.run(
        [sys.executable, str(HERE / "freeze_corpus.py"), "--check"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode:
        raise ValueError("source freeze check failed: " + (completed.stderr or completed.stdout).strip())

    sources = json.loads(SOURCE_FILE.read_text(encoding="utf-8"))
    for source in sources["source_snapshots"]:
        if "path" in source and "sha256" in source:
            source_path = ROOT / source["path"]
            if not source_path.is_file() or sha256(source_path) != source["sha256"]:
                raise ValueError(f"frozen source changed: {source['path']}")
        if "local_file" in source and "sha256" in source:
            local_path = ROOT / source["local_file"]
            if not local_path.is_file() or sha256(local_path) != source["sha256"]:
                raise ValueError(f"captured source snapshot changed: {source['local_file']}")


def parse_cutoffs(value: str) -> list[int]:
    try:
        values = sorted({int(part) for part in value.split(",")})
    except ValueError as error:
        raise argparse.ArgumentTypeError("cutoffs must be comma-separated positive integers") from error
    if not values or values[0] <= 0:
        raise argparse.ArgumentTypeError("cutoffs must be comma-separated positive integers")
    return values


def score_one(query_id: str, docs: list[str], qrels: dict[str, int], cutoffs: list[int]) -> dict[str, Any]:
    relevant_count = sum(grade > 0 for grade in qrels.values())
    first_relevant = next((rank for rank, document_id in enumerate(docs, 1) if qrels.get(document_id, 0) > 0), None)
    scores: dict[str, Any] = {"query_id": query_id, "result_count": len(docs), "mrr": 0.0 if first_relevant is None else 1.0 / first_relevant}
    for cutoff in cutoffs:
        top = docs[:cutoff]
        seen = sum(qrels.get(document_id, 0) > 0 for document_id in top)
        recall = seen / relevant_count if relevant_count else None
        judged = sum(document_id in qrels for document_id in top)
        explicit_false_positives = sum(
            document_id in qrels and qrels[document_id] == 0 for document_id in top
        )
        unjudged = len(top) - judged
        precision = seen / len(top) if top else None
        # The benchmark's closed-corpus scorer treats unjudged results as
        # grade 0. Keep explicit negative judgments and unjudged results
        # separately visible so a partial QREL file is not mistaken for an
        # exhaustive truth set.
        false_positive_rate = (len(top) - seen) / len(top) if top else None
        qrel_coverage = judged / len(top) if top else None
        dcg = sum((2**qrels.get(document_id, 0) - 1) / math.log2(rank + 1) for rank, document_id in enumerate(top, 1))
        ideal = sorted(qrels.values(), reverse=True)[:cutoff]
        ideal_dcg = sum((2**grade - 1) / math.log2(rank + 1) for rank, grade in enumerate(ideal, 1))
        ndcg = dcg / ideal_dcg if ideal_dcg else None
        scores[f"recall@{cutoff}"] = recall
        scores[f"precision@{cutoff}"] = precision
        scores[f"false_positive_rate@{cutoff}"] = false_positive_rate
        scores[f"explicit_false_positives@{cutoff}"] = explicit_false_positives
        scores[f"unjudged@{cutoff}"] = unjudged
        scores[f"qrel_coverage@{cutoff}"] = qrel_coverage
        scores[f"ndcg@{cutoff}"] = ndcg
    return scores


def score_rankings(data: dict[str, Any], rankings: dict[str, list[str]], cutoffs: list[int]) -> dict[str, Any]:
    per_query: list[dict[str, Any]] = []
    query_scores: dict[str, dict[str, Any]] = {}
    empty_pass = 0
    empty_total = 0
    stale_top1 = 0
    freshness_total = 0
    yanked_errors: dict[int, int] = {cutoff: 0 for cutoff in cutoffs}
    yanked_exposures: dict[int, int] = {cutoff: 0 for cutoff in cutoffs}

    for query in data["queries"]:
        query_id = query["query_id"]
        docs = rankings.get(query_id, [])
        result = score_one(query_id, docs, data["qrels"].get(query_id, {}), cutoffs)
        result["category"] = query["category"]
        result["expected_empty"] = bool(query.get("expected_empty"))
        per_query.append(result)
        query_scores[query_id] = result
        if query.get("expected_empty"):
            empty_total += 1
            empty_pass += not docs
        if query.get("category") == "freshness":
            freshness_total += 1
            expected = data["qrels"].get(query_id, {})
            stale_top1 += not docs or expected.get(docs[0], 0) <= 0
        if query.get("exclude_yanked"):
            scope = set(query.get("scope", {}).get("ecosystems", []))
            for cutoff in cutoffs:
                top = docs[:cutoff]
                yanked_exposures[cutoff] += len(top)
                yanked_errors[cutoff] += sum(
                    document_id in data["documents"]
                    and data["documents"][document_id].get("ecosystem") in scope
                    and data["documents"][document_id].get("fields", {}).get("yanked", {}).get("status") == "known"
                    and data["documents"][document_id]["fields"]["yanked"].get("values") == [True]
                    for document_id in top
                )

    valid = [item for item in per_query if not item["expected_empty"]]
    macro: dict[str, Any] = {}
    for metric in [
        "mrr",
        *(
            name
            for cutoff in cutoffs
            for name in (
                f"recall@{cutoff}",
                f"precision@{cutoff}",
                f"false_positive_rate@{cutoff}",
                f"qrel_coverage@{cutoff}",
                f"ndcg@{cutoff}",
            )
        ),
    ]:
        values = [item[metric] for item in valid if item.get(metric) is not None]
        macro[metric] = statistics.fmean(values) if values else None

    category_macro: dict[str, dict[str, float | None]] = {}
    categories = sorted({item["category"] for item in valid})
    for category in categories:
        rows = [item for item in valid if item["category"] == category]
        category_macro[category] = {
            metric: statistics.fmean([row[metric] for row in rows if row.get(metric) is not None]) if any(row.get(metric) is not None for row in rows) else None
            for metric in macro
        }

    pair_checks = []
    for pair in data["pairs"]:
        docs = rankings.get(pair["query_id"], [])
        positions = {document_id: index for index, document_id in enumerate(docs)}
        newer_rank = positions.get(pair["newer_document_id"])
        older_rank = positions.get(pair["older_document_id"])
        pair_checks.append(
            {
                "query_id": pair["query_id"],
                "lineage_id": pair["lineage_id"],
                "newer_document_id": pair["newer_document_id"],
                "older_document_id": pair["older_document_id"],
                "newer_precedes_older": newer_rank is not None and (older_rank is None or newer_rank < older_rank),
                "newer_rank_1based": None if newer_rank is None else newer_rank + 1,
                "older_rank_1based": None if older_rank is None else older_rank + 1,
            }
        )
    return {
        "macro": macro,
        "category_macro": category_macro,
        "per_query": per_query,
        "freshness": {
            "frozen_candidate_stale_top1": stale_top1,
            "frozen_candidate_queries": freshness_total,
            "frozen_candidate_stale_top1_rate": stale_top1 / freshness_total if freshness_total else None,
            "relative_version_pair_accuracy": sum(row["newer_precedes_older"] for row in pair_checks) / len(pair_checks) if pair_checks else None,
            "version_pairs": pair_checks,
        },
        "yank_errors": {
            f"@{cutoff}": {
                "known_yanked_results": yanked_errors[cutoff],
                "scoped_results": yanked_exposures[cutoff],
                "rate": yanked_errors[cutoff] / yanked_exposures[cutoff] if yanked_exposures[cutoff] else None,
            }
            for cutoff in cutoffs
        },
        "expected_empty": {
            "passed": empty_pass,
            "queries": empty_total,
            "pass_rate": empty_pass / empty_total if empty_total else None,
        },
    }


def load_ranked_tsv(path: Path, data: dict[str, Any]) -> dict[str, list[str]]:
    rankings: dict[str, list[tuple[int, str]]] = defaultdict(list)
    seen: set[tuple[str, int]] = set()
    with path.open(encoding="utf-8", newline="") as stream:
        reader = csv.DictReader(stream, delimiter="\t")
        if reader.fieldnames != ["query_id", "rank", "document_id"]:
            raise ValueError("ranked TSV header must be query_id<TAB>rank<TAB>document_id")
        for row in reader:
            query_id = row["query_id"]
            document_id = row["document_id"]
            rank = int(row["rank"])
            if query_id not in data["query_by_id"] or document_id not in data["documents"] or rank <= 0:
                raise ValueError(f"invalid result row: {row}")
            if (query_id, rank) in seen:
                raise ValueError(f"duplicate result rank: {query_id} / {rank}")
            seen.add((query_id, rank))
            rankings[query_id].append((rank, document_id))
    normalized: dict[str, list[str]] = {}
    for query_id, rows in rankings.items():
        rows.sort()
        if [rank for rank, _ in rows] != list(range(1, len(rows) + 1)):
            raise ValueError(f"result ranks must be contiguous from 1: {query_id}")
        if len({document_id for _, document_id in rows}) != len(rows):
            raise ValueError(f"duplicate result document: {query_id}")
        query = data["query_by_id"][query_id]
        corpus_ids = {row["document_id"] for row in data["corpora"][query["corpus"]]}
        if any(document_id not in corpus_ids for _, document_id in rows):
            raise ValueError(f"result includes a document outside the query corpus: {query_id}")
        normalized[query_id] = [document_id for _, document_id in rows]
    return normalized


def quantile_nearest_rank(samples: list[int], percentile: int) -> int | None:
    if not samples:
        return None
    ordered = sorted(samples)
    index = max(0, math.ceil((percentile / 100) * len(ordered)) - 1)
    return ordered[index]


def percentiles(samples: list[int]) -> dict[str, int | None]:
    return {f"p{p}": quantile_nearest_rank(samples, p) for p in (50, 95, 99)}


def query_request(query: dict[str, Any], limit: int) -> dict[str, Any]:
    return {
        "op": "search",
        "query_id": query["query_id"],
        "query": query["query"],
        "category": query["category"],
        "corpus": query["corpus"],
        "scope": query["scope"],
        "exclude_yanked": query["exclude_yanked"],
        "limit": limit,
    }


def read_service_line(stream: TextIO, timeout_seconds: float = 120.0) -> dict[str, Any]:
    import selectors

    selector = selectors.DefaultSelector()
    selector.register(stream, selectors.EVENT_READ)
    try:
        ready = selector.select(timeout_seconds)
    finally:
        selector.close()
    if not ready:
        raise TimeoutError("adapter serve did not respond before timeout")
    line = stream.readline()
    if not line:
        raise RuntimeError("adapter serve exited without a response")
    value = json.loads(line)
    if not isinstance(value, dict):
        raise ValueError("adapter response must be a JSON object per line")
    return value


def validate_adapter_response(response: dict[str, Any], query_id: str | None = None) -> list[str]:
    if query_id and response.get("query_id") not in {None, query_id}:
        raise ValueError(f"adapter returned wrong query_id: expected {query_id}, got {response.get('query_id')}")
    document_ids = response.get("document_ids")
    if not isinstance(document_ids, list) or any(not isinstance(value, str) for value in document_ids):
        raise ValueError("adapter response must include document_ids as a string array")
    if len(document_ids) != len(set(document_ids)):
        raise ValueError("adapter returned a duplicate document id")
    return document_ids


def write_corpus(path: Path, documents: list[dict[str, Any]]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in sorted(documents, key=lambda row: row["document_id"])), encoding="utf-8")


def merged_documents(data: dict[str, Any]) -> list[dict[str, Any]]:
    return [row for rows in data["corpora"].values() for row in rows]


def command_run(command: list[str], result_dir: Path, input_path: Path | None = None) -> dict[str, Any]:
    completed = subprocess.run(
        command,
        cwd=ROOT,
        input=input_path.read_text(encoding="utf-8") if input_path else None,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(f"adapter command failed ({completed.returncode}): {shlex.join(command)}\n{completed.stderr[-4000:]}")
    if not completed.stdout.strip():
        return {}
    try:
        value = json.loads(completed.stdout.splitlines()[-1])
    except json.JSONDecodeError as error:
        raise ValueError(f"adapter command must end with one JSON object: {shlex.join(command)}") from error
    if not isinstance(value, dict):
        raise ValueError(f"adapter returned non-object JSON: {shlex.join(command)}")
    return value


def run_mutations(
    adapter: list[str],
    base_index: Path,
    data: dict[str, Any],
    rankings: dict[str, list[str]],
    limit: int,
    work_dir: Path,
    cutoffs: list[int],
) -> dict[str, Any]:
    outcomes: list[dict[str, Any]] = []
    doc_map = data["documents"]
    query_map = data["query_by_id"]

    for case in data["mutations"]["checks"]:
        case_id = case["id"]
        if case_id in {"float-irrelevant-near-match", "ignore-yank-status"}:
            query_id = case["query_id"]
            baseline = rankings.get(query_id, [])
            corrupted = list(baseline)
            promoted = case["promote_document_id"]
            if promoted in corrupted:
                corrupted.remove(promoted)
            corrupted.insert(0, promoted)
            baseline_score = score_one(query_id, baseline, data["qrels"].get(query_id, {}), cutoffs)
            corrupted_score = score_one(query_id, corrupted, data["qrels"].get(query_id, {}), cutoffs)
            if case_id == "float-irrelevant-near-match":
                if data["qrels"].get(query_id, {}).get(promoted, 0) != 0:
                    raise ValueError("near-match mutation target is not independently judged irrelevant")
                worsened = corrupted_score["mrr"] < baseline_score["mrr"] and (corrupted_score.get("ndcg@10") or 0.0) < (baseline_score.get("ndcg@10") or 0.0)
            else:
                target = query_map[query_id]
                known_yanked = doc_map[promoted].get("fields", {}).get("yanked", {}).get("values") == [True]
                worsened = bool(target.get("exclude_yanked")) and known_yanked and corrupted[0] == promoted
            outcomes.append({"id": case_id, "kind": "scorer-falsification", "passed": worsened, "assertion": case["assertion"]})
            continue

        corpus_name = case["corpus"]
        mutated = [dict(row) for row in data["corpora"][corpus_name]]
        target_id = case.get("document_id")
        if case_id == "hide-forge-lane":
            hidden_kind = case["hide_source_kind"]
            mutated = [row for row in mutated if row.get("source_kind") != hidden_kind]
        else:
            target = next(row for row in mutated if row["document_id"] == target_id)
            field = case["operation"]["field"]
            target["fields"][field] = {
                "status": case["operation"]["status"],
                "values": case["operation"]["values"],
                "source_snapshot_id": target["fields"][field].get("source_snapshot_id"),
            }
        changed_docs = [row for name, rows in data["corpora"].items() for row in (mutated if name == corpus_name else rows)]
        case_dir = work_dir / case_id
        case_dir.mkdir(parents=True)
        corpus_path = case_dir / "corpus.jsonl"
        index_path = case_dir / "index"
        write_corpus(corpus_path, changed_docs)
        command_run(adapter + ["build", "--corpus", str(corpus_path), "--index", str(index_path)], case_dir)
        query = query_map[case["query_id"]]
        query_path = case_dir / "query.json"
        query_path.write_text(json.dumps(query_request(query, limit)), encoding="utf-8")
        response = command_run(adapter + ["search-cold", "--index", str(index_path), "--query-json", str(query_path), "--limit", str(limit)], case_dir)
        docs = validate_adapter_response(response, query["query_id"])
        target_document_id = case.get("document_id") or next(
            pair["document_id"] for pair in data["qrel_rows"] if pair["query_id"] == case["query_id"] and int(pair["grade"]) > 0
        )
        baseline_has_target = target_document_id in rankings.get(query["query_id"], [])
        passed = baseline_has_target and target_document_id not in docs
        outcomes.append({"id": case_id, "kind": "candidate-rebuild", "passed": passed, "baseline_target_present": baseline_has_target, "mutated_target_present": target_document_id in docs, "assertion": case["assertion"]})

    return {"checks": outcomes, "passed": all(row["passed"] for row in outcomes), "note": "Rebuild mutation checks exclude setup and query time from performance samples."}


def run_benchmark(args: argparse.Namespace, data: dict[str, Any]) -> int:
    if os.environ.get("BENCH_BUILD_SLOT_GRANTED") != "1":
        raise ValueError("refusing to invoke an adapter until the root grants a build slot; set BENCH_BUILD_SLOT_GRANTED=1 only after that grant")
    if args.samples < 101:
        raise ValueError("--samples must be at least 101 for nearest-rank p99")
    result_dir = args.result_dir.resolve()
    if not result_dir.is_dir():
        raise ValueError(f"result directory must already exist: {result_dir}")
    try:
        result_dir.relative_to(ROOT)
        inside_repo = True
    except ValueError:
        inside_repo = False
    if inside_repo:
        ignored = subprocess.run(["git", "check-ignore", "--quiet", "--", str(result_dir.relative_to(ROOT))], cwd=ROOT, check=False)
        if ignored.returncode != 0:
            raise ValueError("result directory inside the repository must already be git-ignored")
    if not args.adapter_command:
        raise ValueError("--adapter-command is required")
    adapter = shlex.split(args.adapter_command)
    if not adapter:
        raise ValueError("--adapter-command is empty")
    if not 1 <= args.limit <= 1000:
        raise ValueError("--limit must be between 1 and 1000")

    verify_source_freeze()
    hashes_before = input_hashes()
    workspace_lock_before = workspace_build_lock_snapshot()
    source_fingerprint_before = fingerprint(hashes_before)
    result_path = result_dir / args.run_name
    if result_path.exists():
        raise ValueError(f"refusing to overwrite existing evidence directory: {result_path}")
    result_path.mkdir()
    work_dir = result_path / "work"
    work_dir.mkdir()
    raw_path = result_path / "raw-latencies.jsonl"
    stderr_path = result_path / "adapter-stderr.log"
    combined_corpus = work_dir / "all-corpora.jsonl"
    write_corpus(combined_corpus, merged_documents(data))
    index_path = work_dir / "baseline-index"

    host_memory = None
    try:
        host_memory = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (AttributeError, ValueError, OSError):
        pass
    run_manifest = {
        "schema_version": 1,
        "benchmark_snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "seed": args.seed,
        "samples_per_query": args.samples,
        "limit": args.limit,
        "adapter_argv": adapter,
        "adapter_executable_snapshot": executable_snapshot(adapter),
        "adapter_contract": "build; search-cold; persistent serve with search and one-document update messages",
        "adapter_build_report": {},
        "latency_definitions": {
            "cold": "new adapter process, index open, and one query; operating-system page cache is not flushed",
            "warm": "persistent adapter process, request/response round trip after one per-query warmup",
            "incremental_update": "persistent adapter process; one document and one field toggled per request; setup excluded",
        },
        "host": {
            "os": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor(),
            "logical_cpus": os.cpu_count(),
            "memory_bytes": host_memory,
            "python": sys.version,
        },
        "input_sha256": hashes_before,
        "source_fingerprint_before": source_fingerprint_before,
        "workspace_build_lock_context": {
            "before": workspace_lock_before,
            "after": None,
        },
        "cargo_invocation_by_harness": "none; adapter build operations are gated until the root grants a build slot",
    }
    (result_path / "run-manifest.json").write_text(json.dumps(run_manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    build_started = time.perf_counter_ns()
    build_report = command_run(
        adapter + ["build", "--corpus", str(combined_corpus), "--index", str(index_path)],
        result_path,
    )
    build_ns = time.perf_counter_ns() - build_started
    if not index_path.exists():
        raise RuntimeError("adapter build succeeded but did not create the requested index directory")
    persistent_index_directory_bytes = sum(
        path.stat().st_size for path in index_path.rglob("*") if path.is_file()
    )
    reported_index_bytes = build_report.get("index_bytes")
    index_bytes = (
        reported_index_bytes
        if isinstance(reported_index_bytes, int) and reported_index_bytes >= 0
        else persistent_index_directory_bytes
    )
    run_manifest["adapter_build_report"] = build_report
    (result_path / "run-manifest.json").write_text(
        json.dumps(run_manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    rankings: dict[str, list[str]] = {}
    per_query_warm_ns: dict[str, list[int]] = defaultdict(list)
    per_query_cold_ns: dict[str, list[int]] = defaultdict(list)
    raw_rows: list[dict[str, Any]] = []
    rng = random.Random(args.seed)
    query_rows = list(data["queries"])
    stable_response: dict[str, list[str]] = {}

    with stderr_path.open("w", encoding="utf-8") as stderr_stream:
        service = subprocess.Popen(
            adapter + ["serve", "--index", str(index_path), "--limit", str(args.limit)],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr_stream,
            text=True,
            bufsize=1,
        )
        if service.stdin is None or service.stdout is None:
            raise RuntimeError("failed to open adapter serve pipes")
        try:
            for query in query_rows:
                request = query_request(query, args.limit)
                service.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
                service.stdin.flush()
                response = read_service_line(service.stdout)
                docs = validate_adapter_response(response, query["query_id"])
                stable_response[query["query_id"]] = docs
            for sample in range(args.samples):
                order = list(query_rows)
                rng.shuffle(order)
                for query in order:
                    request = query_request(query, args.limit)
                    start = time.perf_counter_ns()
                    service.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
                    service.stdin.flush()
                    response = read_service_line(service.stdout)
                    elapsed = time.perf_counter_ns() - start
                    docs = validate_adapter_response(response, query["query_id"])
                    query_id = query["query_id"]
                    if query_id in stable_response and stable_response[query_id] != docs:
                        raise RuntimeError(f"adapter produced nondeterministic results for {query_id}")
                    stable_response[query_id] = docs
                    rankings[query_id] = docs
                    per_query_warm_ns[query_id].append(elapsed)
                    raw_rows.append({"mode": "warm", "sample": sample, "query_id": query_id, "elapsed_ns": elapsed})
            for update_sample in range(args.samples):
                remove_alias = (update_sample % 2) == 0
                new_field = {
                    "field": "aliases",
                    "status": "absent" if remove_alias else "known",
                    "values": [] if remove_alias else ["net-client"],
                    "source_snapshot_id": "workspace-fixture:discovery-search-typed-facets",
                }
                update_request = {
                    "op": "update",
                    "batch_id": "alias-remove" if remove_alias else "alias-restore",
                    "updates": [{"document_id": "fixture:cargo/hyper-transport@0.4.0", "fields": {"aliases": new_field}}],
                }
                start = time.perf_counter_ns()
                service.stdin.write(json.dumps(update_request, separators=(",", ":")) + "\n")
                service.stdin.flush()
                response = read_service_line(service.stdout)
                elapsed = time.perf_counter_ns() - start
                if response.get("op") not in {None, "update"}:
                    raise ValueError("adapter returned a non-update response for an update request")
                raw_rows.append({"mode": "incremental_update", "sample": update_sample, "batch_id": update_request["batch_id"], "elapsed_ns": elapsed})
                per_query_warm_ns["__incremental_update__"].append(elapsed)
                query = data["query_by_id"]["alias.only-net-client"]
                check_request = query_request(query, args.limit)
                service.stdin.write(json.dumps(check_request, separators=(",", ":")) + "\n")
                service.stdin.flush()
                check_response = read_service_line(service.stdout)
                check_docs = validate_adapter_response(check_response, query["query_id"])
                target = "fixture:cargo/hyper-transport@0.4.0"
                expected_present = not remove_alias
                if (target in check_docs) != expected_present:
                    raise RuntimeError(f"incremental alias mutation failed after {update_request['batch_id']}")
            if args.samples % 2:
                restore_request = {
                    "op": "update",
                    "batch_id": "alias-restore-final",
                    "updates": [{"document_id": "fixture:cargo/hyper-transport@0.4.0", "fields": {"aliases": {"field": "aliases", "status": "known", "values": ["net-client"], "source_snapshot_id": "workspace-fixture:discovery-search-typed-facets"}}}],
                }
                service.stdin.write(json.dumps(restore_request, separators=(",", ":")) + "\n")
                service.stdin.flush()
                read_service_line(service.stdout)
        finally:
            service.stdin.close()
            try:
                service.wait(timeout=10)
            except subprocess.TimeoutExpired:
                service.terminate()
                service.wait(timeout=5)

    for query in query_rows:
        query_id = query["query_id"]
        query_path = work_dir / f"query-{query_id.replace('.', '_')}.json"
        query_path.write_text(json.dumps(query_request(query, args.limit)), encoding="utf-8")
        for sample in range(args.samples):
            command = adapter + ["search-cold", "--index", str(index_path), "--query-json", str(query_path), "--limit", str(args.limit)]
            start = time.perf_counter_ns()
            response = command_run(command, result_path)
            elapsed = time.perf_counter_ns() - start
            docs = validate_adapter_response(response, query_id)
            if stable_response.get(query_id) != docs:
                raise RuntimeError(f"cold and warm candidate results differ for {query_id}")
            per_query_cold_ns[query_id].append(elapsed)
            raw_rows.append({"mode": "cold_process_open", "sample": sample, "query_id": query_id, "elapsed_ns": elapsed})

    for query in query_rows:
        query_id = query["query_id"]
        rankings.setdefault(query_id, stable_response.get(query_id, []))
    ranked_path = result_path / "ranked-results.tsv"
    with ranked_path.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.writer(stream, delimiter="\t", lineterminator="\n")
        writer.writerow(["query_id", "rank", "document_id"])
        for query in query_rows:
            for rank, document_id in enumerate(rankings.get(query["query_id"], []), 1):
                writer.writerow([query["query_id"], rank, document_id])

    score = score_rankings(data, rankings, DEFAULT_CUTOFFS)
    metrics = {
        "index_bytes": index_bytes,
        "persistent_index_directory_bytes": persistent_index_directory_bytes,
        "adapter_build_report": build_report,
        "index_build_wall_ns": build_ns,
        "cold_process_open_latency_ns": percentiles([value for values in per_query_cold_ns.values() for value in values]),
        "warm_request_roundtrip_latency_ns": percentiles([value for key, values in per_query_warm_ns.items() if key != "__incremental_update__" for value in values]),
        "incremental_one_document_one_field_update_latency_ns": percentiles(per_query_warm_ns["__incremental_update__"]),
        "samples_per_query": args.samples,
        "relevance": score,
        "results_sha256": sha256(ranked_path),
        "measurement_limits": [
            "Cold process-open timing starts a new adapter process, but the OS page cache is not flushed.",
            "Warm timing includes local JSONL pipe round-trip overhead.",
            "Index bytes cover files under the adapter's requested index directory only.",
            "The primary corpus contains real pinned identities and source digests; synthetic adversarial records are reported in a separate corpus lane.",
        ],
    }
    mutation_results = run_mutations(adapter, index_path, data, rankings, args.limit, work_dir / "mutations", DEFAULT_CUTOFFS)
    metrics["mutation_checks"] = mutation_results
    (result_path / "metrics.json").write_text(json.dumps(metrics, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    with raw_path.open("w", encoding="utf-8") as stream:
        for row in raw_rows:
            stream.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")

    hashes_after = input_hashes()
    workspace_lock_after = workspace_build_lock_snapshot()
    after_fingerprint = fingerprint(hashes_after)
    run_manifest["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    run_manifest["source_fingerprint_after"] = after_fingerprint
    run_manifest["source_tree_stable"] = source_fingerprint_before == after_fingerprint
    run_manifest["workspace_build_lock_context"]["after"] = workspace_lock_after
    run_manifest["workspace_build_lock_context"]["stable_during_run"] = (
        workspace_lock_before["sha256"] == workspace_lock_after["sha256"]
    )
    (result_path / "run-manifest.json").write_text(json.dumps(run_manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if source_fingerprint_before != after_fingerprint:
        raise RuntimeError("benchmark inputs changed during measurement; all output is invalid")
    if not mutation_results["passed"]:
        raise RuntimeError("one or more independent mutation controls failed; inspect metrics.json")
    print(result_path)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate", help="verify corpus, qrels, source snapshots, and mutation fixtures")
    score_parser = subparsers.add_parser("score", help="score a ranked TSV against the committed independent QREL")
    score_parser.add_argument("results", type=Path)
    score_parser.add_argument("--cutoffs", type=parse_cutoffs, default=DEFAULT_CUTOFFS)
    score_parser.add_argument("--output", type=Path)
    run_parser = subparsers.add_parser("run", help="measure an adapter that implements the documented line protocol")
    run_parser.add_argument("--adapter-command", required=True, help="argv prefix for the search adapter")
    run_parser.add_argument("--result-dir", type=Path, required=True, help="existing evidence directory")
    run_parser.add_argument("--run-name", default="search-quality-run")
    run_parser.add_argument("--samples", type=int, default=101)
    run_parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    run_parser.add_argument("--limit", type=int, default=50)
    args = parser.parse_args()

    try:
        data = load_data()
        if args.command == "validate":
            verify_source_freeze()
            errors = validate_data(data)
            if errors:
                print("\n".join(errors), file=sys.stderr)
                return 1
            print(f"validated {len(data['documents'])} documents, {len(data['queries'])} queries, {len(data['qrel_rows'])} qrels, {len(data['pairs'])} version pairs")
            print(f"primary={sum(len(rows) for rows in data['corpora'].values()) - len(data['corpora']['adversarial'])} adversarial={len(data['corpora']['adversarial'])}; no ranked results or performance claims were generated")
            return 0
        if args.command == "score":
            errors = validate_data(data)
            if errors:
                raise ValueError("benchmark data invalid: " + "; ".join(errors))
            rankings = load_ranked_tsv(args.results, data)
            report = score_rankings(data, rankings, args.cutoffs)
            output = json.dumps(report, indent=2, sort_keys=True) + "\n"
            if args.output:
                args.output.write_text(output, encoding="utf-8")
            else:
                print(output, end="")
            return 0
        errors = validate_data(data)
        if errors:
            raise ValueError("benchmark data invalid: " + "; ".join(errors))
        return run_benchmark(args, data)
    except (OSError, ValueError, RuntimeError, TimeoutError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
