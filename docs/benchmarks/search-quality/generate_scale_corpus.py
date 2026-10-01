#!/usr/bin/env python3
"""Generate a separate deterministic synthetic scale lane from frozen record schemas."""

from __future__ import annotations

import argparse
import hashlib
import json
import random
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any


HERE = Path(__file__).resolve().parent
ECOSYSTEMS = ["cargo", "npm", "pypi", "go", "maven", "nuget"]
SEED_DEFAULT = 20260928
VERSION = "1.0.0"
DESCRIPTIONS = [
    "Synthetic package description for search index scale measurement",
    "Synthetic client library with deterministic benchmark metadata",
    "Synthetic command line utility generated from a pinned package schema",
    "Synthetic protocol adapter used only by the scale benchmark",
]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_rows(path: Path) -> list[dict[str, Any]]:
    with path.open(encoding="utf-8") as stream:
        return [json.loads(line) for line in stream if line.strip()]


def parse_sizes(value: str) -> list[int]:
    try:
        sizes = sorted({int(part) for part in value.split(",")})
    except ValueError as error:
        raise argparse.ArgumentTypeError("records must be comma-separated positive integers") from error
    if not sizes or sizes[0] <= 0:
        raise argparse.ArgumentTypeError("records must be comma-separated positive integers")
    return sizes


def package_name(ecosystem: str, ordinal: int) -> str:
    suffix = f"{ordinal:07d}"
    if ecosystem == "go":
        return f"example.invalid/searchbench/scale-go-{suffix}"
    return f"scale-{ecosystem}-{suffix}"


def canonical_coordinate(ecosystem: str, name: str) -> str:
    if ecosystem == "maven":
        return f"maven:org.searchbench:{name}@{VERSION}"
    return f"scale-synthetic:{ecosystem}/{name}@{VERSION}"


def document_id(ecosystem: str, name: str) -> str:
    return f"scale-synthetic:{ecosystem}/{name}@{VERSION}"


def create_document(
    ecosystem: str,
    ordinal: int,
    seed: int,
    rng: random.Random,
    template_fields: dict[str, Any],
) -> dict[str, Any]:
    name = package_name(ecosystem, ordinal)
    source_snapshot_id = f"scale-synthetic:seed-{seed}:schema-v1"
    alias = f"synthetic-alias-{ecosystem}-{ordinal:07d}"
    description = DESCRIPTIONS[rng.randrange(len(DESCRIPTIONS))]
    keywords = [f"scale-{ecosystem}", f"variant-{rng.randrange(8)}"]

    def facet(status: str, values: list[Any]) -> dict[str, Any]:
        return {
            "status": status,
            "values": values,
            "source_snapshot_id": source_snapshot_id if status == "known" else None,
        }

    fields = {}
    for field in template_fields:
        if field == "aliases":
            fields[field] = facet("known", [alias])
        elif field == "description":
            fields[field] = facet("known", [description])
        elif field == "keywords":
            fields[field] = facet("known", keywords)
        else:
            fields[field] = facet("unknown", [])
    return {
        "data_class": "scale-synthetic",
        "document_id": document_id(ecosystem, name),
        "ecosystem": ecosystem,
        "name": name,
        "version": VERSION,
        "coordinate": canonical_coordinate(ecosystem, name),
        "source_kind": "scale-synthetic-registry",
        "source_snapshot_id": source_snapshot_id,
        "archive_url": None,
        "archive_digest": None,
        "fields": fields,
    }


def probe_queries(counts: Counter[str]) -> tuple[list[dict[str, Any]], dict[str, str]]:
    queries = []
    probe_targets: dict[str, str] = {}
    for ecosystem in ECOSYSTEMS:
        count = counts[ecosystem]
        if count == 0:
            continue
        for label, ordinal in (("middle", count // 2), ("last", count - 1)):
            name = package_name(ecosystem, ordinal)
            target_id = document_id(ecosystem, name)
            query_id = f"{ecosystem}.{label}-exact"
            queries.append(
                {
                    "query_id": query_id,
                    "query": name,
                    "category": "exact-name",
                    "corpus": "primary",
                    "scope": {"ecosystems": [ecosystem]},
                    "exclude_yanked": False,
                    "limit": 10,
                }
            )
            probe_targets[query_id] = target_id
        ordinal = count // 2
        name = package_name(ecosystem, ordinal)
        query_id = f"{ecosystem}.middle-alias"
        queries.append(
            {
                "query_id": query_id,
                "query": f"synthetic-alias-{ecosystem}-{ordinal:07d}",
                "category": "alias",
                "corpus": "primary",
                "scope": {"ecosystems": [ecosystem]},
                "exclude_yanked": False,
                "limit": 10,
            }
        )
        probe_targets[query_id] = document_id(ecosystem, name)

    cargo_count = counts["cargo"]
    if cargo_count:
        middle = cargo_count // 2
        name = package_name("cargo", middle)
        query_id = "cargo.update-target"
        probe_targets[query_id] = document_id("cargo", name)
        return queries, probe_targets
    return queries, probe_targets


def generate_size(output_dir: Path, records: int, seed: int, template_fields: dict[str, Any], source_info: dict[str, Any]) -> None:
    target_dir = output_dir / f"records-{records}"
    if target_dir.exists():
        raise FileExistsError(f"refusing to overwrite existing scale corpus: {target_dir}")
    target_dir.mkdir(parents=True)
    rng = random.Random(seed)
    counts: Counter[str] = Counter()
    corpus_path = target_dir / "corpus.jsonl"
    with corpus_path.open("w", encoding="utf-8", newline="") as stream:
        for row_index in range(records):
            ecosystem = ECOSYSTEMS[row_index % len(ECOSYSTEMS)]
            ordinal = counts[ecosystem]
            row = create_document(ecosystem, ordinal, seed, rng, template_fields)
            stream.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
            counts[ecosystem] += 1

    queries, probe_targets = probe_queries(counts)
    (target_dir / "queries.jsonl").write_text(
        "".join(json.dumps(query, sort_keys=True, separators=(",", ":")) + "\n" for query in queries),
        encoding="utf-8",
    )
    manifest = {
        "schema_version": 1,
        "lane": "synthetic-scale-only",
        "benchmark_snapshot_id": source_info["benchmark_snapshot_id"],
        "generated_at": source_info["captured_at"],
        "seed": seed,
        "synthetic_source_snapshot_id": f"scale-synthetic:seed-{seed}:schema-v1",
        "document_count": records,
        "ecosystem_counts": dict(sorted(counts.items())),
        "source_schema_sha256": source_info["schema_sha256"],
        "record_schema_source": "primary-corpus.jsonl, cloned field names and tri-state shape",
        "perturbations": [
            "unique deterministic names and canonical document ids",
            "unique per-document aliases used for exact update probes",
            "known synthetic descriptions and keywords with explicit scale-synthetic provenance",
            "unknown dependency, advisory, readme, and yank facets",
            "no source archive URLs, hashes, download counts, or live registry claims",
        ],
        "query_count": len(queries),
        "probe_targets": probe_targets,
        "files": {
            "corpus.jsonl": {"sha256": sha256(corpus_path), "bytes": corpus_path.stat().st_size},
            "queries.jsonl": {
                "sha256": sha256(target_dir / "queries.jsonl"),
                "bytes": (target_dir / "queries.jsonl").stat().st_size,
            },
        },
    }
    (target_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {records} synthetic scale records to {target_dir}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--records", type=parse_sizes, default=parse_sizes("100000,1000000"))
    parser.add_argument("--seed", type=int, default=SEED_DEFAULT)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    primary_path = HERE / "primary-corpus.jsonl"
    sources_path = HERE / "source-snapshots.json"
    verified = subprocess.run(
        [sys.executable, str(HERE / "freeze_corpus.py"), "--check"],
        cwd=HERE.parents[2],
        text=True,
        capture_output=True,
        check=False,
    )
    if verified.returncode:
        raise ValueError("refusing to generate scale inputs from an unfrozen primary corpus")
    primary_rows = load_rows(primary_path)
    template = next(row for row in primary_rows if row["ecosystem"] == "cargo")
    source_info = json.loads(sources_path.read_text(encoding="utf-8"))
    source_info["schema_sha256"] = sha256(primary_path)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for records in args.records:
        generate_size(args.output_dir, records, args.seed, template["fields"], source_info)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
