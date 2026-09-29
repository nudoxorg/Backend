#!/usr/bin/env python3
"""Deterministic source-only checks for row ordering and UTF-8 edit locality.

This companion model reuses the fixture planners in ir_delta_locality.py. It
measures canonical key-order invariance, an explicitly order-sensitive byte
stream, and a single valid UTF-8 documentation edit. It is not a compiler or
CAS benchmark; digest IDs use the fixture's SHA-256 stand-in.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import sys
from typing import Any, Callable

HERE = Path(__file__).resolve().parent
FIXTURE_PATH = HERE / "ir_delta_locality.py"
SPEC = importlib.util.spec_from_file_location("ir_delta_locality", FIXTURE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load locality fixture: {FIXTURE_PATH}")
FIXTURE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = FIXTURE
SPEC.loader.exec_module(FIXTURE)


def encounter_image(rows: list[Any]) -> bytes:
    """Serialize rows in supplied order as an order-sensitivity control."""
    result = bytearray(b"NXFI-encounter-order-locality-v1\0")
    result.extend(len(rows).to_bytes(8, "big"))
    for row in rows:
        result.extend(len(row.payload).to_bytes(4, "big"))
        result.extend(row.payload)
    return bytes(result)


def compare_plan(
    planner: Callable[[Any], Any],
    before: Any,
    after: Any,
) -> dict[str, Any]:
    before_plan = planner(before)
    after_plan = planner(after)
    change = FIXTURE.delta(before_plan, after_plan)
    return {
        **change,
        "planner_read_bytes_pair": before_plan.planner_read_bytes + after_plan.planner_read_bytes,
        "segment_hash_bytes_pair": before_plan.segment_hash_bytes + after_plan.segment_hash_bytes,
        "root_equal": before_plan.segments == after_plan.segments,
    }


def with_utf8_edit(rows: list[Any], target: int, value: str) -> list[Any]:
    encoded = value.encode("utf-8")
    edited = []
    for row in rows:
        payload = row.payload
        if row.logical_id == target:
            payload += FIXTURE.field_bytes("utf8-documentation", encoded)
        edited.append(FIXTURE.Row(row.logical_id, row.stable_key, payload))
    return FIXTURE.with_graph_facts(edited)


def run(count: int) -> dict[str, Any]:
    entities = [FIXTURE.make_row(index) for index in range(count)]
    rows = FIXTURE.with_graph_facts(entities)
    reordered = list(reversed(rows))

    canonical_before = FIXTURE.encode_image(rows)
    canonical_after = FIXTURE.encode_image(reordered)
    encounter_before = encounter_image(rows)
    encounter_after = encounter_image(reordered)

    target = count // 2
    utf8_before = with_utf8_edit(entities, target, "cafe")
    utf8_after = with_utf8_edit(entities, target, "café")
    before_keys = [row.stable_key for row in utf8_before]
    after_keys = [row.stable_key for row in utf8_after]

    return {
        "provenance": (
            "source-independent deterministic model built from ir_delta_locality.py; "
            "SHA-256 stand-in object IDs; no compiler, CAS, or network execution"
        ),
        "rows": count,
        "declaration_reorder": {
            "operation": "reverse declaration encounter order without changing rows",
            "stable_keys_unchanged": sorted(row.stable_key for row in rows)
            == sorted(row.stable_key for row in reordered),
            "canonical_image_bytes_before": len(canonical_before),
            "canonical_image_bytes_after": len(canonical_after),
            "canonical_image_equal": canonical_before == canonical_after,
            "encounter_stream_bytes_before": len(encounter_before),
            "encounter_stream_bytes_after": len(encounter_after),
            "encounter_order_ordinal": compare_plan(
                FIXTURE.ordinal_plan,
                encounter_before,
                encounter_after,
            ),
            "canonical_ordinal": compare_plan(
                FIXTURE.ordinal_plan,
                canonical_before,
                canonical_after,
            ),
            "stable_key_cdc": compare_plan(
                FIXTURE.cdc_plan, rows, reordered
            ),
            "stable_key_prefix_8bit": compare_plan(
                FIXTURE.prefix_plan, rows, reordered
            ),
            "interpretation": (
                "reordering is a no-op for the model's canonical serializer and stable-key "
                "planners; the encounter-order stream is a sensitivity control, not a claim "
                "about production NXFI ordering"
            ),
        },
        "utf8_documentation_edit": {
            "operation": "replace one valid UTF-8 documentation value at the middle row",
            "target_logical_id": target,
            "value_before": "cafe",
            "value_after": "café",
            "value_bytes_before": len("cafe".encode("utf-8")),
            "value_bytes_after": len("café".encode("utf-8")),
            "stable_keys_unchanged": before_keys == after_keys,
            "ordinal_1mib": compare_plan(
                FIXTURE.ordinal_plan,
                FIXTURE.encode_image(utf8_before),
                FIXTURE.encode_image(utf8_after),
            ),
            "stable_key_cdc": compare_plan(
                FIXTURE.cdc_plan, utf8_before, utf8_after
            ),
            "stable_key_prefix_8bit": compare_plan(
                FIXTURE.prefix_plan, utf8_before, utf8_after
            ),
            "interpretation": (
                "a documentation payload edit preserves the logical row key in this fixture; "
                "actual compiler reuse still requires a complete producer read-frontier proof"
            ),
        },
        "inherited_fixture_coverage": {
            "front_mid_tail_insert_delete": "ir_delta_locality.py rows_for_case/run",
            "random_repeated_periodic_bytes": "ir_delta_locality.py jumbo_case",
            "source_of_existing_8192_numbers": "ir_delta_locality_8192.json",
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rows", type=int, default=8192)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.rows < 2:
        parser.error("--rows must be at least 2")
    result = run(args.rows)
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output is None:
        print(rendered, end="")
    else:
        args.output.write_text(rendered, encoding="utf-8")


if __name__ == "__main__":
    main()
