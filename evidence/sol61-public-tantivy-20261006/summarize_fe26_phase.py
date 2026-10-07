#!/usr/bin/env python3
"""Summarize retained authentic public bytes without replacing raw evidence."""
import argparse
import hashlib
import json
from pathlib import Path


def fact(path):
    return {"path": str(path), "bytes": path.stat().st_size,
            "sha256": hashlib.file_digest(path.open("rb"), "sha256").hexdigest()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    parser.add_argument("phase")
    parser.add_argument("--tokens")
    args = parser.parse_args()
    phase = args.root / args.phase
    receipt = json.loads((phase / "receipt.json").read_text())
    summary = {"schema": "nudox.actual-public-phase-summary.v1", "phase": args.phase,
               "source": receipt["runtime_source"], "pass": receipt["all_phase_checks_pass"],
               "error": receipt.get("error"), "owner_pid": receipt["owner_pid"],
               "eligible_inputs": receipt["eligible_python_inputs"],
               "elapsed_seconds": receipt["elapsed_seconds"],
               "receipt": fact(phase / "receipt.json"), "wire": fact(phase / "wire.jsonl"),
               "old_token_new_owner_replays": receipt.get("old_token_new_owner_replays", []),
               "contract_negatives": len(receipt.get("public_contract_negatives", [])),
               "stale_root_refusals": len(receipt.get("stale_selected_token_refusals", [])),
               "surface_checks": len(receipt.get("default_search_and_graph_checks", [])),
               "issued_token_sets": receipt.get("issued_token_sets", [])}
    matrix = phase / "matrix.json"
    if matrix.exists():
        m = json.loads(matrix.read_text())
        sizes, faults = [], {}
        for case in m["cases"]:
            sizes.extend(case.get("token_sizes", []))
            for surface, ref in case.get("references", {}).items():
                for refusal in ref.get("typed_budget_refusals", []):
                    faults[(case["route"], case["query"], surface, refusal["credit"])] = refusal["fault"]
        summary["matrix"] = {"artifact": fact(matrix), "pass": m["all_pass"], "cases": len(m["cases"]),
                             "pages": sum(c.get("pages", 0) for c in m["cases"]),
                             "row_occurrences": sum(c.get("rows", 0) for c in m["cases"]),
                             "multi_page_cases": sum(c.get("pages", 0) > 1 for c in m["cases"]),
                             "unique_large_credit_refusals": [{"route": k[0], "query": k[1], "surface": k[2],
                                                               "credit": k[3], "fault": v} for k, v in faults.items()],
                             "maximum_token_sizes": {key: max((z[key] for z in sizes), default=0) for key in
                                                     ("public_token_bytes", "owner_token_bytes", "decoded_command_bytes")}}
    if args.tokens:
        issued = json.loads((args.root / args.tokens).read_text())
        token_map = {c["first"]["nextCursor"]: c for c in issued["cases"] if c["surface"] == "mcp"}
        observations = []
        with (phase / "wire.jsonl").open() as raw:
            for line in raw:
                event = json.loads(line)
                token = event.get("payload", {}).get("params", {}).get("arguments", {}).get("cursor")
                if token in token_map:
                    expiry = int(token.split("-", 2)[1])
                    observations.append({"route": token_map[token]["route"], "query": token_map[token]["text"],
                                         "issued_at_unix": issued["issued_at_unix"], "request_at_unix": event["at_unix"],
                                         "expires_at_unix": expiry, "before_expiry": event["at_unix"] < expiry,
                                         "age_seconds": event["at_unix"] - issued["issued_at_unix"]})
        summary["retained_mcp_token_expiry_observations"] = observations
        summary["token_file"] = fact(args.root / args.tokens)
    target = phase / "compact-summary.json"
    target.write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({"path": str(target), "sha256": fact(target)["sha256"], "pass": summary["pass"]}))


if __name__ == "__main__":
    main()
