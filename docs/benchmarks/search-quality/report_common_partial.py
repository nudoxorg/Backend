#!/usr/bin/env python3
"""Summarize saved upstream common-lane raw output without running a search."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run_common_lane as common
import search_benchmark as benchmark


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw", type=Path, required=True, help="saved lib-rs-results.json")
    parser.add_argument("--output", type=Path, required=True, help="new JSON summary path")
    args = parser.parse_args()
    data = benchmark.load_data()
    query_rows = [query for query in data["queries"] if query.get("lib_rs_common") is True]
    raw = json.loads(args.raw.read_text(encoding="utf-8"))
    rankings = {
        row["query_id"]: [hit["document_id"] for hit in row["ranked"] if hit.get("document_id")]
        for row in raw.get("results", [])
    }
    candidate_ids = raw.get("indexed_document_ids")
    if candidate_ids is None:
        id_map_path = args.raw.parent / "lib-rs-index" / "ids-by-coordinate.json"
        candidate_ids = list(json.loads(id_map_path.read_text(encoding="utf-8")).values())
    summary = {
        "status": "partial-upstream-only",
        "raw_file": str(args.raw.resolve()),
        "completed_query_rows": len(raw.get("results", [])),
        "expected_query_rows": len(query_rows),
        "warm_sample_count_by_query": {
            row["query_id"]: len(row.get("warm_latency_ns", [])) for row in raw.get("results", [])
        },
        "cold_samples_present": bool(raw.get("cold_latency_ns")),
        "indexed_document_ids": sorted(candidate_ids),
        "quality": common.common_quality(data, query_rows, rankings, candidate_ids=set(candidate_ids)),
        "warm_latency_ns": common.percentiles(
            [sample for row in raw.get("results", []) for sample in row.get("warm_latency_ns", [])]
        ),
        "interpretation": "saved partial evidence check only; no Nudox comparison, cold latency, or superiority claim",
    }
    common.write_json(args.output, summary)
    print(args.output.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
