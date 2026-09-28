#!/usr/bin/env python3
"""Score a frozen query judgment file against ranked result TSV rows."""

from __future__ import annotations

import argparse
import csv
import math
import sys
from collections import defaultdict
from pathlib import Path


def read_tsv(path: Path) -> list[dict[str, str]]:
    with path.open("r", encoding="utf-8", newline="") as stream:
        return list(csv.DictReader(stream, delimiter="\t"))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="TSV with query_id, rank, document_id")
    parser.add_argument(
        "--queries",
        type=Path,
        default=Path(__file__).with_name("search-queries.tsv"),
    )
    parser.add_argument(
        "--qrels",
        type=Path,
        default=Path(__file__).with_name("search-qrels.tsv"),
    )
    parser.add_argument("--k", default="1,5,10", help="comma-separated positive cutoffs")
    args = parser.parse_args()

    cutoffs = sorted({int(value) for value in args.k.split(",")})
    if not cutoffs or cutoffs[0] <= 0:
        parser.error("--k must contain positive integers")

    queries = read_tsv(args.queries)
    qrels = read_tsv(args.qrels)
    results = read_tsv(args.results)
    query_ids = {row["query_id"] for row in queries}
    if len(query_ids) != len(queries):
        parser.error("query_id must be unique in the query file")

    judgments: dict[str, dict[str, int]] = defaultdict(dict)
    for row in qrels:
        query_id = row["query_id"]
        document_id = row["document_id"]
        grade = int(row["grade"])
        if query_id not in query_ids or not 0 <= grade <= 3:
            parser.error(f"invalid qrel row: {row}")
        if document_id in judgments[query_id]:
            parser.error(f"duplicate qrel: {query_id} / {document_id}")
        judgments[query_id][document_id] = grade

    ranked: dict[str, list[str]] = defaultdict(list)
    seen_rows: set[tuple[str, int]] = set()
    for row in results:
        query_id = row["query_id"]
        rank = int(row["rank"])
        document_id = row["document_id"]
        if query_id not in query_ids or rank <= 0 or (query_id, rank) in seen_rows:
            parser.error(f"invalid result row: {row}")
        seen_rows.add((query_id, rank))
        ranked[query_id].append((rank, document_id))

    print("query_id\tk\trecall\tmrr\tndcg")
    for query_id in sorted(query_ids):
        rows = sorted(ranked[query_id])
        if [rank for rank, _ in rows] != list(range(1, len(rows) + 1)):
            parser.error(f"result ranks must be contiguous from 1 for {query_id}")
        docs = [document_id for _, document_id in rows]
        grades = judgments[query_id]
        relevant_count = sum(grade > 0 for grade in grades.values())
        for k in cutoffs:
            top = docs[:k]
            relevant_seen = sum(grades.get(document_id, 0) > 0 for document_id in top)
            recall = relevant_seen / relevant_count if relevant_count else math.nan
            reciprocal_rank = next(
                (1.0 / index for index, document_id in enumerate(docs, start=1) if grades.get(document_id, 0) > 0),
                0.0,
            )
            dcg = sum(
                (2**grades.get(document_id, 0) - 1) / math.log2(index + 1)
                for index, document_id in enumerate(top, start=1)
            )
            ideal = sorted(grades.values(), reverse=True)[:k]
            ideal_dcg = sum(
                (2**grade - 1) / math.log2(index + 1)
                for index, grade in enumerate(ideal, start=1)
            )
            ndcg = dcg / ideal_dcg if ideal_dcg else math.nan
            print(f"{query_id}\t{k}\t{recall:.6f}\t{reciprocal_rank:.6f}\t{ndcg:.6f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
