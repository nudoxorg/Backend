#!/usr/bin/env python3
"""Export the frozen Cargo-only, common-field input for lib-rs-mirror evaluation.

This is an input adapter, not an implementation of the upstream index or ranker.
It does not access the network, compile Rust, or emit ranked results.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
import search_benchmark as benchmark  # noqa: E402

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
CONTRACT_PATH = HERE / "lib-rs-common-contract.json"


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def encoded_jsonl(rows: list[dict[str, Any]]) -> bytes:
    return "".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in rows).encode()


def facet_value(document: dict[str, Any], name: str) -> Any:
    facet = document.get("fields", {}).get(name, {})
    if facet.get("status") == "known":
        values = facet.get("values", [])
        return values if name == "keywords" else (values[0] if values else "")
    return None


def common_document(document: dict[str, Any]) -> dict[str, Any]:
    fields = document.get("fields", {})
    # Unknown and absent stay distinct in status metadata; neither is fabricated
    # as an empty keyword list or description supplied to the search index.
    return {
        "document_id": document["document_id"],
        "crate_name": document["name"],
        "keywords": facet_value(document, "keywords"),
        "description": facet_value(document, "description"),
        "field_status": {
            "crate_name": "known",
            "keywords": fields.get("keywords", {}).get("status", "unknown"),
            "description": fields.get("description", {}).get("status", "unknown"),
        },
    }


def build_export(data: dict[str, Any]) -> dict[str, bytes]:
    contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
    errors = benchmark.validate_data(data)
    if errors:
        raise ValueError("benchmark data invalid: " + "; ".join(errors))

    cargo_documents = [
        row for row in data["corpora"]["primary"] if row.get("ecosystem") == "cargo"
    ]
    common_queries = [row for row in data["queries"] if row.get("lib_rs_common")]
    if not cargo_documents or not common_queries:
        raise ValueError("common Cargo input is empty")
    query_ids = {row["query_id"] for row in common_queries}
    cargo_ids = {row["document_id"] for row in cargo_documents}
    qrel_rows = [
        row
        for row in data["qrel_rows"]
        if row["query_id"] in query_ids and row["document_id"] in cargo_ids
    ]
    if any(row["query_id"] not in query_ids for row in qrel_rows):
        raise ValueError("common QREL export contains an out-of-profile query")

    documents_payload = encoded_jsonl([common_document(row) for row in cargo_documents])
    queries_payload = encoded_jsonl(common_queries)
    writer_rows = []
    for row in qrel_rows:
        writer_rows.append(
            "\t".join(
                [row["query_id"], row["document_id"], row["grade"], row["judgment_basis"], row["source_snapshot_id"]]
            )
        )
    qrels_payload = (
        "query_id\tdocument_id\tgrade\tjudgment_basis\tsource_snapshot_id\n"
        + "\n".join(writer_rows)
        + "\n"
    ).encode()
    coverage = {
        "cargo_documents": len(cargo_documents),
        "queries": len(common_queries),
        "qrels": len(qrel_rows),
        "known_field_values": {
            field: sum(row["field_status"][field] == "known" for row in map(common_document, cargo_documents))
            for field in ("crate_name", "keywords", "description")
        },
    }
    input_paths = [
        HERE / "primary-corpus.jsonl",
        HERE / "queries.jsonl",
        HERE / "qrels.tsv",
        CONTRACT_PATH,
    ]
    manifest = {
        "schema_version": 1,
        "benchmark_snapshot_id": data["sources"]["benchmark_snapshot_id"],
        "upstream_snapshot_id": contract["upstream_snapshot_id"],
        "profile": "cargo-primary-common-fields",
        "input_sha256": {
            path.relative_to(ROOT).as_posix(): sha256_bytes(path.read_bytes()) for path in input_paths
        },
        "output_sha256": {
            "documents.jsonl": sha256_bytes(documents_payload),
            "queries.jsonl": sha256_bytes(queries_payload),
            "qrels.tsv": sha256_bytes(qrels_payload),
        },
        "coverage": coverage,
        "field_projection": ["crate_name", "keywords", "description"],
        "omitted_inputs": [
            "README text",
            "release freshness and version ordering",
            "dependencies",
            "advisories",
            "yank state",
            "downloads and traffic",
            "README structure, code size, owners, and other upstream ranking features",
        ],
        "execution_status": "input projection only; no upstream index or ranker was built or run",
        "network_access": "none",
    }
    manifest_payload = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    return {
        "documents.jsonl": documents_payload,
        "queries.jsonl": queries_payload,
        "qrels.tsv": qrels_payload,
        "manifest.json": manifest_payload,
    }


def write_or_verify(output_dir: Path, payloads: dict[str, bytes]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    mismatches = []
    for name, payload in payloads.items():
        path = output_dir / name
        if path.exists():
            if path.read_bytes() != payload:
                mismatches.append(name)
        else:
            path.write_bytes(payload)
    if mismatches:
        raise ValueError(
            "export directory has differing generated files: " + ", ".join(mismatches)
        )
    missing = [name for name in payloads if not (output_dir / name).is_file()]
    if missing:
        raise ValueError("failed to write export files: " + ", ".join(missing))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        benchmark.verify_source_freeze()
        data = benchmark.load_data()
        payloads = build_export(data)
        write_or_verify(args.output_dir.resolve(), payloads)
        print(args.output_dir.resolve())
        return 0
    except (OSError, ValueError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
