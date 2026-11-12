#!/usr/bin/env python3
"""Exercise related-edge continuations through real owner, CLI and MCP binaries.

Requires a matched development bundle. Creates a private synthetic TypeScript
project and workspace; records outputs under --output. This script does not
build or install anything. A failure is evidence, not an automatic retry of an
index mutation. Large graph publication may exceed a product budget; that is a
failed acceptance result, not a reason to shrink the fixture.
"""
import argparse
import json
import pathlib
import re

from csharp_public_route import Journey
from runtime_build_receipt import stable_file, verify_runtime_build_receipt

COUNT = 5000
LIMIT = 137
NAME = re.compile(r"related_caller_(\d{4})_")


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def fixture(project):
    src = project / "src"
    src.mkdir(parents=True)
    (project / "tsconfig.json").write_text(json.dumps({
        "compilerOptions": {"target": "ES2022", "module": "commonjs", "strict": True},
        "include": ["src/**/*.ts"]
    }))
    (src / "center.ts").write_text("export function related_hub(value: number): number { return value + 1; }\n")
    for group in range(50):
        lines = ['import { related_hub } from "./center";']
        for index in range(group * 100, (group + 1) * 100):
            suffix = "x" * (1 + index % 80)
            lines.append(f"export function related_caller_{index:04d}_{suffix}(value: number): number {{ return related_hub(value + {index}); }}")
        (src / f"callers_{group:02d}.ts").write_text("\n".join(lines) + "\n")


def identity(edge):
    check(isinstance(edge, dict), f"invalid edge: {edge!r}")
    for field in ("from", "to", "relation", "confidence"):
        check(field in edge, f"edge missing {field}: {edge!r}")
    return tuple(json.dumps(edge[field], sort_keys=True) for field in ("from", "to", "relation"))


def run_pages(j, surface, label, alias=None, first_cursor=None):
    if alias is None:
        alias = f"{j.project}::src/center.ts:1::related_hub"
    token = first_cursor
    seen_cursors, seen_edges = set(), set()
    callers = set()
    first_successor = None
    for page in range(10000):
        if surface == "cli":
            args = ["related", alias, "--limit", str(LIMIT)]
            if token:
                args += ["--cursor", token]
            rc, data, raw = j.cli(f"{label}-{page:04d}", args, timeout=900)
            check(rc == 0 and isinstance(data, dict), f"CLI page {page} failed: {raw[:1000]}")
            edges = data.get("related_edges", [])
        else:
            args = {"coordinate": alias, "limit": LIMIT}
            if token:
                args["cursor"] = token
            result = j.tool("backend.related", args)
            check(not result.get("isError"), f"MCP page {page} failed: {result!r}")
            (j.out / f"{label}-{page:04d}.json").write_text(json.dumps(result, indent=2) + "\n")
            data = result.get("structuredContent")
            check(isinstance(data, dict), f"MCP page {page} lacks structured content")
            edges = data.get("edges", [])
        check(not data.get("related_evidence_withheld", False), f"{surface} withheld related evidence")
        check(isinstance(edges, list), f"{surface} malformed edges")
        for edge in edges:
            key = identity(edge)
            check(key not in seen_edges, f"{surface} repeated edge {key}")
            seen_edges.add(key)
            match = NAME.search(edge["from"])
            if match and "related_hub" in edge["to"] and "call" in str(edge["relation"]).lower():
                callers.add(int(match.group(1)))
        token = data.get("nextCursor")
        if token is None:
            return seen_edges, callers, first_successor
        check(isinstance(token, str) and token and token not in seen_cursors, f"{surface} bad/repeated cursor")
        seen_cursors.add(token)
        if first_successor is None:
            first_successor = token
    raise AssertionError(f"{surface} exceeded page bound")


def index(j, project, label):
    rc, obj, raw = j.cli(label, ["add", str(project), "--wait", "--wait-timeout", "900"], project, timeout=1800)
    state = (((obj or {}).get("index_operation") or {}).get("detail") or {}).get("state", {}).get("state")
    check(rc == 0 and state == "published", f"index did not publish: rc={rc} state={state} {raw[:1000]}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", required=True, help="directory containing matched backend-cli, backend-mcp and backend-locald")
    parser.add_argument("--output", required=True, help="new directory for transcripts and result")
    parser.add_argument("--build-receipt", required=True, help="successful exact Cargo build receipt")
    parser.add_argument("--source-commit", required=True, help="exact checked-out source commit")
    args = parser.parse_args()
    source = pathlib.Path(__file__).resolve().parents[3]
    output = pathlib.Path(args.output)
    if output.exists() or output.is_symlink():
        raise RuntimeError("output directory must be new")

    def verify_build():
        verified = verify_runtime_build_receipt(pathlib.Path(args.build_receipt).resolve(), source)
        if verified["source"]["commit"] != args.source_commit:
            raise RuntimeError("requested source differs from the verified Cargo build")
        for name, artifact in verified["artifacts"].items():
            candidate = stable_file(pathlib.Path(args.bin_dir).resolve() / name, name, executable=True)
            if any(candidate[key] != artifact[key] for key in ("sha256", "bytes", "architecture")):
                raise RuntimeError(f"{name} differs from the verified Cargo build")
        return verified

    build = verify_build()
    j = Journey(args.bin_dir, args.output, label="related-edge")
    result = {"status": "failed", "expected_callers": COUNT,
              "build_receipt_sha256": build["receipt_sha256"],
              "source_commit": args.source_commit, "artifacts": build["artifacts"]}
    try:
        project = j.root / "project"
        fixture(project)
        j.project = project
        alias = f"{project}::src/center.ts:1::related_hub"
        caller_alias = f"{project}::src/callers_00.ts:2::related_caller_0000_x"
        j.start_owner(project)
        j.start_mcp(project)
        index(j, project, "initial-index")
        cli, cli_callers, cli_cursor = run_pages(j, "cli", "cli")
        mcp, mcp_callers, mcp_cursor = run_pages(j, "mcp", "mcp")
        expected = set(range(COUNT))
        check(cli_callers == expected, f"CLI callers {len(cli_callers)}/{COUNT}, missing {sorted(expected-cli_callers)[:20]}")
        check(mcp_callers == expected, f"MCP callers {len(mcp_callers)}/{COUNT}, missing {sorted(expected-mcp_callers)[:20]}")
        check(cli == mcp, f"CLI/MCP edge sets differ: {len(cli-mcp)} / {len(mcp-cli)}")
        check(cli_cursor and mcp_cursor, "fixture did not produce continuation")
        outgoing_cli, _, _ = run_pages(j, "cli", "outgoing-cli", alias=caller_alias)
        outgoing_mcp, _, _ = run_pages(j, "mcp", "outgoing-mcp", alias=caller_alias)
        def has_caller_edge(edges):
            return any("related_caller_0000_x" in json.loads(edge[0])
                       and "related_hub" in json.loads(edge[1])
                       and "call" in json.loads(edge[2]).lower() for edge in edges)
        check(outgoing_cli == outgoing_mcp and has_caller_edge(outgoing_cli),
              "caller-centered CLI/MCP related pages omitted the outgoing call")
        j.stop_mcp()
        j.stop_owner()
        j.start_owner(project)
        j.start_mcp(project)
        cold, cold_callers, _ = run_pages(j, "cli", "cold-cli")
        check(cold == cli and cold_callers == expected, "cold reopen changed related edges")
        cold_mcp, cold_mcp_callers, mcp_cursor = run_pages(j, "mcp", "cold-mcp")
        check(cold_mcp == cli and cold_mcp_callers == expected and mcp_cursor,
              "cold MCP reopen changed related edges or omitted continuation")
        cold_out_cli, _, _ = run_pages(j, "cli", "cold-outgoing-cli", alias=caller_alias)
        cold_out_mcp, _, _ = run_pages(j, "mcp", "cold-outgoing-mcp", alias=caller_alias)
        check(cold_out_cli == outgoing_cli and cold_out_mcp == outgoing_mcp,
              "cold reopen changed caller-centered outgoing edges")
        # A cursor issued before restart must resume against the same owner scope.
        tail, _, _ = run_pages(j, "cli", "cold-resume", first_cursor=cli_cursor)
        check(tail and tail.issubset(cli), "cold resumed frontier did not retain prior edges")
        rc, data, raw = j.cli("wrong-credit", ["related", alias, "--limit", str(LIMIT - 1), "--cursor", cli_cursor])
        check(rc != 0, f"CLI accepted changed issued credit: {data!r} {raw[:500]}")
        with (project / "src" / "center.ts").open("a") as source:
            source.write("\nexport const related_revision_marker = 1;\n")
        index(j, project, "changed-index")
        rc, data, raw = j.cli("stale-cli", ["related", alias, "--limit", str(LIMIT), "--cursor", cli_cursor])
        check(rc != 0, f"stale CLI cursor was accepted: {data!r} {raw[:500]}")
        stale = j.tool("backend.related", {"coordinate": alias, "limit": LIMIT, "cursor": mcp_cursor}, allow_error=True)
        check(stale.get("isError") or "rpc_error" in stale, f"stale MCP cursor was accepted: {stale!r}")
        result.update(status="passed", cli_edges=len(cli), mcp_edges=len(mcp), callers=len(cli_callers), records=j.records)
    except Exception as error:
        result.update(error=repr(error), records=j.records)
        raise
    finally:
        j.close()
        try:
            if any("cleanup_error" in record for record in j.records) or j.root.exists():
                raise RuntimeError("journey-owned processes or private workspace were not retired")
            verified_after = verify_build()
            if verified_after["receipt_sha256"] != build["receipt_sha256"]:
                raise RuntimeError("Cargo build receipt changed during the journey")
        except Exception as error:
            result.update(status="failed", verification_error=repr(error), records=j.records)
            (output / "related-edge-result.json").write_text(json.dumps(result, indent=2) + "\n")
            raise
        result["records"] = j.records
        (output / "related-edge-result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
