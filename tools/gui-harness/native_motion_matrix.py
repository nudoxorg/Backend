#!/usr/bin/env python3
"""Inventory native motion artifacts against the required GUI flow matrix.

This reads immutable CAPTURE.json files; it never launches or controls the UI.
A native pass proves pixels/AX/action coverage only. Live owner/index admission
requires a separate product-side receipt and independent review.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import sys

CATALOG = Path(__file__).with_name("native_motion_matrix.json")


def assess(catalog: dict, manifests: list[dict]) -> dict:
    by_id = {}
    duplicates = []
    for manifest in manifests:
        ident = manifest.get("case", {}).get("id")
        if ident in by_id:
            duplicates.append(ident)
        by_id[ident] = manifest
    rows = []
    for required in catalog["required"]:
        manifest = by_id.get(required["id"])
        if manifest is None:
            rows.append({"id": required["id"], "status": "missing"})
            continue
        problems = []
        if manifest.get("case") != required:
            problems.append("typed case metadata differs from matrix")
        if manifest.get("class") != catalog["evidence_class"]:
            problems.append("wrong capture evidence class")
        if not manifest.get("passed_native_checks"):
            problems.append("native AX, action, pixel or frame coverage failed")
        if manifest.get("binary_source_admission", {}).get("state") != "VerifiedBuildReceipt":
            problems.append("UnprovenBinarySource")
        if required["live_index"] and (not manifest.get("inputs") or not manifest.get("owner_receipt")):
            problems.append("live index input/owner receipt not attached")
        rows.append({"id": required["id"], "status": "native_pass" if not problems else "failed",
                     "problems": problems, "capture": manifest.get("_path"),
                     "live_owner_index_admission": "unverified by native pixels"})
    return {"schema": 1, "rows": rows, "duplicates": duplicates,
            "native_coverage_complete": not duplicates and all(row["status"] == "native_pass" for row in rows),
            "live_index_acceptance": "requires independent product-side owner/read verification"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("captures", type=Path, help="directory containing native capture subdirectories")
    parser.add_argument("--catalog", type=Path, default=CATALOG)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    catalog = json.loads(args.catalog.read_text())
    if catalog.get("schema") != 1 or not isinstance(catalog.get("required"), list):
        parser.error("invalid matrix catalog")
    manifests = []
    for path in sorted(args.captures.rglob("CAPTURE.json")):
        manifest = json.loads(path.read_text())
        manifest["_path"] = str(path.resolve())
        manifests.append(manifest)
    report = assess(catalog, manifests)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{sum(row['status'] == 'native_pass' for row in report['rows'])}/{len(report['rows'])} native cases pass: {args.out}")
    return 0 if report["native_coverage_complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
