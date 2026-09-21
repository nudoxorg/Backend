#!/usr/bin/env python3
"""Recompute exact MCP payload budgets from canonical Rust projections.

The tokenizer is intentionally a dev/test input. Production request handling
uses the bounded typed encoder and its byte admission gate; this script is the
reproducible exact-token audit run beside the checked-in fixtures.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = ROOT / "crates" / "present" / "fixtures"
METADATA_PATH = Path(__file__).with_name("tokenizer.json")

PAYLOAD_FILES = {
    "common_empty_records_summary": "common-empty-records-summary.json",
    "worst_200_full_records": "worst-200-full-records.json",
    "unicode_rtl_records": "unicode-rtl-records.json",
    "long_page_document_source": "long-page-document-source.json",
    "continuation_envelope": "continuation-envelope.json",
    "shelf_summary": "shelf-summary.json",
    "outline_summary": "outline-summary.json",
    "status_summary": "status-summary.json",
    "product_summary": "product-summary.json",
    "query_page": "query-page.json",
    "error_fault": "error-fault.json",
    "oversized_fault": "oversized-fault.json",
    "tool_schema": "mcp-tool-schema.json",
}
HARD_BUDGET_FIXTURES = {"worst_200_full_records"}


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"token-budget gate: {message}")


def load_tokenizer(metadata: dict):
    root = os.environ.get("TOKEN_BUDGET_TIKTOKEN_ROOT", str(ROOT / ".local" / "tiktoken-pinned"))
    sys.path.insert(0, root)
    try:
        import tiktoken
    except ImportError as error:
        fail(
            "pinned tiktoken environment is missing; set TOKEN_BUDGET_TIKTOKEN_ROOT "
            f"to a site-packages directory ({error})"
        )

    expected_version = metadata["version"]
    if getattr(tiktoken, "__version__", None) != expected_version:
        fail(f"tiktoken version drift: expected {expected_version}, got {tiktoken.__version__}")
    encoding = tiktoken.get_encoding(metadata["encoding"])
    artifact = {
        "mergeable-ranks": {key.hex(): value for key, value in sorted(encoding._mergeable_ranks.items())},
        "special-tokens": dict(sorted(encoding._special_tokens.items())),
        "pattern": encoding._pat_str,
    }
    digest = hashlib.sha256(
        json.dumps(artifact, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    if digest != metadata["artifact_sha256"]:
        fail(
            "tokenizer artifact drift: "
            f"expected {metadata['artifact_sha256']}, got {digest}"
        )
    return encoding


def canonical_tool(binary: Path) -> dict:
    try:
        process = subprocess.run(
            [str(binary)],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        fail(f"canonical generator is missing: {binary}; build backend-token-budget first")
    except subprocess.CalledProcessError as error:
        fail(f"canonical generator failed with status {error.returncode}: {error.stderr.strip()}")
    try:
        return json.loads(process.stdout)
    except json.JSONDecodeError as error:
        fail(f"canonical generator did not emit JSON: {error}")


def check_payload(name: str, generated: dict, encoding, metadata: dict, writing: bool = False) -> dict:
    if not generated.get("admitted"):
        fail(f"{name} was unexpectedly refused by the canonical encoder")
    payload = generated.get("payload")
    if not isinstance(payload, str):
        fail(f"{name} has no canonical UTF-8 payload")
    raw = payload.encode("utf-8")
    bytes_count = len(raw)
    token_count = len(encoding.encode(payload))
    if generated.get("bytes") != bytes_count:
        fail(f"{name} byte measurement disagrees with its canonical payload")
    path = FIXTURE_ROOT / PAYLOAD_FILES[name]
    if not path.exists() and not writing:
        fail(f"{name} checked-in payload is missing: {path}")
    if path.exists() and path.read_bytes() != raw:
        fail(f"{name} checked-in payload differs from canonical typed DTO output")
    caps = metadata["caps"]
    byte_cap = caps["hard_bytes"] if name in HARD_BUDGET_FIXTURES else caps["default_bytes"]
    token_cap = caps["hard_tokens"] if name in HARD_BUDGET_FIXTURES else caps["default_tokens"]
    if bytes_count > byte_cap:
        fail(f"{name} exceeds its {byte_cap} byte cap")
    if token_count > token_cap:
        fail(f"{name} exceeds its {token_cap} token cap")
    return {
        "bytes": bytes_count,
        "estimated_tokens": generated["estimated_tokens"],
        "real_tokens": token_count,
        "allocation_count": generated["allocations"]["count_total"],
        "allocation_bytes": generated["allocations"]["bytes_total"],
        "budget_bytes": byte_cap,
        "fixture": str(path.relative_to(ROOT)),
        "sha256": hashlib.sha256(raw).hexdigest(),
    }


def check_refusal(generated: dict, worst: dict, encoding, metadata: dict) -> dict:
    if generated.get("admitted"):
        fail("atomic oversized refusal was admitted")
    refusal = generated.get("refusal")
    if not isinstance(refusal, dict) or refusal.get("partial_bytes") != 0:
        fail("oversized refusal reported partial serialized bytes")
    if refusal.get("budget_bytes") != metadata["caps"]["default_bytes"]:
        fail("oversized refusal crossed the default byte budget")
    if refusal.get("observed_bytes") != worst["bytes"]:
        fail("oversized refusal no longer measures the complete worst payload")
    return {
        "bytes": refusal["observed_bytes"],
        "estimated_tokens": generated["estimated_tokens"],
        "real_tokens": worst["real_tokens"],
        "allocation_count": generated["allocations"]["count_total"],
        "allocation_bytes": generated["allocations"]["bytes_total"],
        "budget_bytes": refusal["budget_bytes"],
        "admitted": False,
        "partial_bytes": refusal["partial_bytes"],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="rewrite checked-in payload fixtures and measurements")
    parser.add_argument(
        "--binary",
        type=Path,
        default=ROOT / "target" / "debug" / "backend-token-budget",
        help="canonical Rust fixture generator",
    )
    args = parser.parse_args()
    metadata = json.loads(METADATA_PATH.read_text(encoding="utf-8"))
    encoding = load_tokenizer(metadata)
    generated = canonical_tool(args.binary)
    expected_names = set(PAYLOAD_FILES) | {"atomic_oversized_refusal"}
    if set(generated) != expected_names:
        fail(f"fixture set drift: expected {sorted(expected_names)}, got {sorted(generated)}")

    expected_report = None
    report_path = FIXTURE_ROOT / "payload-budgets.json"
    if not args.write:
        if not report_path.exists():
            fail(f"checked-in machine-readable report is missing: {report_path}")
        expected_report = json.loads(report_path.read_text(encoding="utf-8"))
        if expected_report.get("schema") != "backend-present.payload-budget/v2":
            fail("checked-in report schema is stale; regenerate with --write")
        if expected_report.get("tokenizer") != metadata:
            fail("checked-in tokenizer metadata differs from tokenizer.json")

    measurements = {}
    for name in PAYLOAD_FILES:
        measurements[name] = check_payload(name, generated[name], encoding, metadata, args.write)
    measurements["atomic_oversized_refusal"] = check_refusal(
        generated["atomic_oversized_refusal"], measurements["worst_200_full_records"], encoding, metadata
    )

    if expected_report is not None:
        expected_fixtures = expected_report.get("fixtures", {})
        if set(expected_fixtures) != expected_names:
            fail("checked-in report fixture set differs from the canonical generator")
        for name, measured in measurements.items():
            expected = expected_fixtures[name]
            for field in ("bytes", "estimated_tokens", "real_tokens"):
                if expected.get(field) != measured.get(field):
                    fail(f"checked-in {name}.{field} disagrees with recomputation")
            if measured.get("admitted", True) and expected.get("sha256") != measured.get("sha256"):
                fail(f"checked-in {name}.sha256 disagrees with canonical bytes")
            if expected.get("allocation_bytes", 0) <= 0:
                fail(f"checked-in {name} has no allocation measurement")

    if args.write:
        for name, filename in PAYLOAD_FILES.items():
            (FIXTURE_ROOT / filename).write_bytes(generated[name]["payload"].encode("utf-8"))

    result = {
        "schema": "backend-present.payload-budget/v2",
        "tokenizer": metadata,
        "fixtures": dict(sorted(measurements.items())),
    }
    result_text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.write:
        (FIXTURE_ROOT / "payload-budgets.json").write_text(result_text, encoding="utf-8")
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
