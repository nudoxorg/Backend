#!/usr/bin/env python3
"""Describe retained real CLI/MCP receipts; never grant whole-package credit."""
import argparse
import ast
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--run", required=True, type=Path)
parser.add_argument("--output", required=True, type=Path)
args = parser.parse_args()
assert not args.output.exists(), "preserve previous observations"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


summary = json.loads((args.run / "summary.json").read_text())
assert not summary["owned_daemons_remaining"], "wait for owned processes to stop"
wire = [json.loads(line) for line in (args.run / "wire.jsonl").read_text().splitlines()]
commands = [row for row in wire if row["kind"] == "command"]
options = summary["journey_options"]
witness_path, witness_line = options.get("witness_path"), options.get("witness_line")
versions = {}
if witness_path:
    original = Path(summary["source"]) / witness_path
    versions[0] = original.read_bytes()
    versions[1] = (args.run / "project" / witness_path).read_bytes()

source_checks, parities, reference_rows = [], [], []
last_command = {}
pending_call = None
for index, row in enumerate(wire):
    if row["kind"] == "command":
        argv = row["argv"]
        if "--endpoint" not in argv:
            continue
        endpoint_index = argv.index("--endpoint")
        words = argv[endpoint_index + 2:]
        if len(words) >= 2 and words[0] in ["source", "show", "references", "graph"]:
            tool = "document" if words[0] == "show" else words[0]
            last_command[(tool, words[1])] = row
        value = row.get("payload") or {}
        source = value.get("source")
        if isinstance(source, dict) and source.get("path") == witness_path:
            version = 0 if row["label"].startswith("query-") else 1
            raw = versions[version]
            source_lines = raw.decode().splitlines()
            body = "\n".join(source["lines"])
            nodes = [node for node in ast.walk(ast.parse(raw)) if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Assign, ast.AnnAssign)) and node.lineno == source["line"]]
            actual_lines = nodes[0].end_lineno - nodes[0].lineno + 1 if len(nodes) == 1 else None
            column = nodes[0].col_offset if len(nodes) == 1 else 0
            expected_tail = "\n".join([source_lines[source["line"] - 1][column:], *source_lines[source["line"]:]])
            exact_declaration = ast.get_source_segment(raw.decode(), nodes[0]) if len(nodes) == 1 else None
            source_checks.append({"label": row["label"], "source_version": version, "path": source["path"], "line": source["line"], "source_sha256": hashlib.sha256(raw).hexdigest(), "source_node_kind": type(nodes[0]).__name__ if len(nodes) == 1 else None, "returned_lines": len(source["lines"]), "returned_body_bytes": len(body.encode()), "actual_declaration_lines": actual_lines, "extent": source["extent"], "exact_source_prefix": expected_tail.startswith(body), "exact_complete_declaration": body == exact_declaration if source["extent"] == "complete" and exact_declaration is not None else None, "stdout_sha256": row["stdout_sha256"]})
        if len(words) >= 2 and words[0] == "references":
            reference_rows.append({"label": row["label"], "coordinate": words[1], "exit": row["exit"], "records": value.get("records", []), "stdout_sha256": row["stdout_sha256"]})
    elif row["kind"] == "mcp-request" and row["payload"].get("method") == "tools/call":
        pending_call = row["payload"]
    elif row["kind"] == "mcp-reply" and pending_call and row["payload"].get("id") == pending_call.get("id"):
        params = pending_call["params"]
        tool = params["name"].removeprefix("backend.")
        coordinate = params["arguments"].get("coordinate")
        prior = last_command.get((tool, coordinate))
        if prior:
            actual = row["payload"].get("result", {}).get("structuredContent")
            parities.append({"cli_label": prior["label"], "tool": tool, "coordinate": coordinate, "exact_parity": actual == prior.get("payload"), "cli_stdout_sha256": prior["stdout_sha256"], "mcp_reply_wire_line": index + 1})
        pending_call = None

selected = []
for row in commands:
    if row["label"].endswith("health") or row["label"] == "cold-health-explicit-retry":
        value = row.get("payload") or {}
        selected.append({"label": row["label"], "exit": row["exit"], "seconds": row["seconds"], "sequence": value.get("sequence"), "rows": value.get("rows"), "fault": value if value.get("answer") == "fault" else None})
resolve = next((row.get("payload") for row in commands if row["label"] == "query-resolve"), None) or {}
largest = sorted([{"wire_line": i + 1, "bytes": row.get("bytes", 0), "estimated_tokens": row.get("estimated_tokens")} for i, row in enumerate(wire) if row["kind"] == "mcp-reply"], key=lambda row: row["bytes"], reverse=True)[:5]
observation = {"schema": "nudox.retained-functional-journey-observation.v1", "run": str(args.run), "manifest_sha256": summary["runtime_manifest_sha256"], "summary_sha256": sha(args.run / "summary.json"), "wire_sha256": sha(args.run / "wire.jsonl"), "observer_sha256": sha(Path(__file__)), "requested_witness": {"path": witness_path, "line": witness_line}, "requested_witness_present_in_resolution": any(row["identity"].get("path") == witness_path and (not witness_line or row["identity"].get("line") == witness_line) for row in resolve.get("records", [])), "healths": selected, "command_timings": [{"label": row["label"], "exit": row["exit"], "seconds": row["seconds"], "stdout_bytes": row["stdout_bytes"]} for row in commands], "source_checks": source_checks, "cli_mcp_parities": parities, "references": reference_rows, "largest_mcp_replies": largest, "source_edits": [row for row in wire if row["kind"] == "source-edit"], "pagination": [row for row in wire if row["kind"] == "pagination-observation"], "cancellation": [row for row in wire if row["kind"] in ["job-ticket-discovery", "initial-cancel-ticket"]], "journey_error": summary.get("journey_error"), "whole_package_pass": False, "original_acquired_source_mutated": False, "source_witnesses_are_not_compiler_bindings": True}
args.output.write_text(json.dumps(observation, indent=2) + "\n")
print(json.dumps({"path": str(args.output), "sha256": sha(args.output), "source_checks": len(source_checks), "source_prefix_matches": sum(row["exact_source_prefix"] for row in source_checks), "parities": len(parities), "exact_parities": sum(row["exact_parity"] for row in parities), "requested_witness_present": observation["requested_witness_present_in_resolution"]}))
