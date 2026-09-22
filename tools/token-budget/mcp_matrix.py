#!/usr/bin/env python3
"""Audit exact MCP wire sizes for every advertised tool and detail level.

The Rust generator owns the fixtures and the serialized response envelope;
this script only supplies the pinned model tokenizer and checks in the
machine-readable result. Production request handling remains byte bounded;
token counts are an audit signal for context-window planning.
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
REPORT_PATH = FIXTURE_ROOT / "mcp-tool-budgets.json"


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"mcp-tool-budget gate: {message}")


def load_tokenizer(metadata: dict):
    root = Path(
        os.environ.get("TOKEN_BUDGET_TIKTOKEN_ROOT", str(ROOT / ".local" / "tiktoken-pinned"))
    )
    sys.path.insert(0, str(root))
    try:
        import tiktoken
    except ImportError as error:
        fail(f"pinned tiktoken environment is missing: {error}")
    if getattr(tiktoken, "__version__", None) != metadata["version"]:
        fail(
            "tiktoken version drift: "
            f"expected {metadata['version']}, got {tiktoken.__version__}"
        )
    encoding = tiktoken.get_encoding(metadata["encoding"])
    artifact = {
        "mergeable-ranks": {
            key.hex(): value for key, value in sorted(encoding._mergeable_ranks.items())
        },
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


def canonical_tool(binary: Path) -> list[dict]:
    try:
        process = subprocess.run(
            [str(binary), "--tools"],
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
        value = json.loads(process.stdout)
    except json.JSONDecodeError as error:
        fail(f"canonical generator did not emit JSON: {error}")
    if not isinstance(value, list):
        fail("canonical tool matrix is not an array")
    return value


def measure_row(row: dict, encoding, caps: dict) -> dict:
    required = {
        "tool",
        "detail",
        "fixture",
        "admitted",
        "payload",
        "rpc_bytes",
        "estimated_tokens_upper_bound",
    }
    if not required.issubset(row):
        fail(f"matrix row is missing fields: {sorted(required - set(row))}")
    payload = row["payload"]
    if not isinstance(payload, str):
        fail(f"{row['tool']}:{row['detail']} has no serialized payload")
    raw = payload.encode("utf-8")
    byte_count = len(raw)
    token_count = len(encoding.encode(payload))
    if row["rpc_bytes"] != byte_count:
        fail(f"{row['tool']}:{row['detail']} byte count disagrees with its payload")
    estimate = (byte_count + 3) // 4
    if row["estimated_tokens_upper_bound"] != estimate:
        fail(f"{row['tool']}:{row['detail']} estimate disagrees with its wire bytes")
    if byte_count > caps["default_bytes"]:
        fail(f"{row['tool']}:{row['detail']} exceeds the MCP byte budget")
    if token_count > caps["default_tokens"]:
        fail(f"{row['tool']}:{row['detail']} exceeds the MCP token budget")
    measured = {
        "tool": row["tool"],
        "detail": row["detail"],
        "fixture": row["fixture"],
        "admitted": bool(row["admitted"]),
        "structured_bytes": row["structured_bytes"],
        "tool_result_bytes": row["tool_result_bytes"],
        "rpc_bytes": byte_count,
        "estimated_tokens_upper_bound": row["estimated_tokens_upper_bound"],
        "real_tokens": token_count,
        "sha256": hashlib.sha256(raw).hexdigest(),
    }
    if row.get("refusal_bytes") is not None:
        measured["refusal_bytes"] = row["refusal_bytes"]
        if row["admitted"]:
            fail(f"{row['tool']}:{row['detail']} has a refusal and is admitted")
        if row["refusal_bytes"] <= caps["default_bytes"]:
            fail(f"{row['tool']}:{row['detail']} refused without crossing the byte budget")
    elif not row["admitted"]:
        fail(f"{row['tool']}:{row['detail']} is refused without an observed size")
    return measured


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="rewrite the checked-in report")
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
    actual = {(row.get("tool"), row.get("detail")) for row in generated}
    if len(generated) != len(actual):
        fail("matrix contains duplicate tool/detail rows")
    modes_by_tool: dict[str, set[str]] = {}
    for tool, detail in actual:
        if not isinstance(tool, str) or detail not in {"compact", "default", "full"}:
            fail("matrix contains an unknown tool or detail label")
        modes_by_tool.setdefault(tool, set()).add(detail)
    if any("default" not in modes for modes in modes_by_tool.values()):
        fail("every advertised tool must have its schema default measurement")

    caps = metadata["caps"]
    measurements = [measure_row(row, encoding, caps) for row in generated]
    measurements.sort(key=lambda row: (row["tool"], row["detail"]))
    report = {
        "schema": "backend-mcp.tool-budget/v1",
        "tokenizer": metadata,
        "caps": {
            "bytes": caps["default_bytes"],
            "model_tokens": caps["default_tokens"],
        },
        "tools": measurements,
    }

    if not args.write:
        if not REPORT_PATH.exists():
            fail(f"checked-in report is missing: {REPORT_PATH}")
        expected_report = json.loads(REPORT_PATH.read_text(encoding="utf-8"))
        if expected_report != report:
            fail("checked-in MCP tool budget report differs; regenerate with --write")
    else:
        REPORT_PATH.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
