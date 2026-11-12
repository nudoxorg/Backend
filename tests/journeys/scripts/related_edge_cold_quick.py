#!/usr/bin/env python3
"""Check real RELATED cursor replay across a cold owner and reindex.

Requires a contemporaneous build receipt for the exact clean checkout and
matched binaries. Uses the same 5,000-caller fixture as the full journey, but
reads only two pages per surface. Failed private workspaces are retained.
"""
import argparse
import base64
import json
import pathlib
import time

from csharp_public_route import Journey
from related_edge_public_acceptance import COUNT, LIMIT, check, fixture, identity, index
from runtime_build_receipt import stable_file, verify_runtime_build_receipt


def owner_cursor(surface, token):
    check(isinstance(token, str) and token, f"{surface}: missing continuation")
    if surface == "cli":
        check(token.startswith("re1-"), "CLI continuation is not an owner token")
        return token
    # MCP's expiry and MAC can change while the embedded owner token is stable.
    check(token.startswith("mcp2-"), "MCP continuation has unexpected format")
    fields = token.split("-", 2)
    check(len(fields) == 3 and fields[1].isdigit(), "invalid MCP expiry")
    check(int(fields[1]) > int(time.time()), "MCP continuation expired before owner replay")
    body, separator, mac = fields[2].rpartition("-")
    check(separator and len(mac) == 32 and body.startswith("re1-"), "invalid MCP envelope")
    return body


def owner_scope(surface, token):
    encoded = owner_cursor(surface, token)[4:]
    return json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))["scope"]


def page(j, surface, alias, label, cursor=None):
    if surface == "cli":
        args = ["related", alias, "--limit", str(LIMIT)]
        if cursor is not None:
            args += ["--cursor", cursor]
        rc, data, raw = j.cli(label, args, timeout=900)
        check(rc == 0 and isinstance(data, dict), f"{label}: CLI failed {raw[:1000]}")
        edges = data.get("related_edges")
    else:
        args = {"coordinate": alias, "limit": LIMIT}
        if cursor is not None:
            args["cursor"] = cursor
        result = j.tool("backend.related", args)
        (j.out / f"{label}.json").write_text(json.dumps(result, indent=2) + "\n")
        check(not result.get("isError") and "rpc_error" not in result, f"{label}: MCP failed {result!r}")
        data = result.get("structuredContent")
        check(isinstance(data, dict), f"{label}: missing structured response")
        edges = data.get("edges")
    check(not data.get("related_evidence_withheld", False), f"{label}: evidence withheld")
    check(isinstance(edges, list) and edges, f"{label}: empty or malformed edges")
    keys = [identity(edge) for edge in edges]
    check(len(set(keys)) == len(keys), f"{label}: duplicate edges")
    token = data.get("nextCursor")
    owner_cursor(surface, token)
    return keys, token


def stale(j, surface, alias, cursor):
    if surface == "cli":
        rc, obj, raw = j.cli("stale-cli", ["related", alias, "--limit", str(LIMIT), "--cursor", cursor], timeout=900)
        check(rc != 0 and isinstance(obj, dict) and obj.get("slug") == "cursor-mismatch", f"CLI did not report cursor mismatch: {raw[:1000]}")
    else:
        result = j.tool("backend.related", {"coordinate": alias, "limit": LIMIT, "cursor": cursor}, allow_error=True)
        (j.out / "stale-mcp.json").write_text(json.dumps(result, indent=2) + "\n")
        check(result.get("isError") or "rpc_error" in result, f"MCP accepted stale cursor: {result!r}")
        structured = result.get("structuredContent", {})
        check(isinstance(structured, dict) and structured.get("slug") == "cursor-mismatch",
              f"MCP did not report owner cursor mismatch: {result!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--build-receipt", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--trace-related", action="store_true")
    args = parser.parse_args()
    source = pathlib.Path(__file__).resolve().parents[3]
    output = pathlib.Path(args.output)
    if output.exists() or output.is_symlink():
        raise RuntimeError("output directory must be new")

    def verify_build():
        verified = verify_runtime_build_receipt(pathlib.Path(args.build_receipt).resolve(), source)
        if verified["source"]["commit"] != args.source_commit:
            raise RuntimeError("requested source differs from verified Cargo build")
        for name, artifact in verified["artifacts"].items():
            candidate = stable_file(pathlib.Path(args.bin_dir).resolve() / name, name, executable=True)
            if any(candidate[key] != artifact[key] for key in ("sha256", "bytes", "architecture")):
                raise RuntimeError(f"{name} differs from verified Cargo build")
        return {"receipt_sha256": verified["receipt_sha256"], "source_commit": verified["source"]["commit"], "artifacts": verified["artifacts"]}

    build = verify_build()
    j = Journey(args.bin_dir, args.output, env_extra={"NUDOX_RELATED_TRACE": "1"} if args.trace_related else None, label="related-cold")
    result = {"status": "failed", "expected_callers": COUNT, "build_before": build}
    try:
        project = j.root / "project"
        fixture(project)
        j.project = project
        alias = f"{project}::src/center.ts:1::related_hub"
        j.start_owner(project)
        j.start_mcp(project)
        index(j, project, "initial-index")
        baseline = {}
        for surface in ("cli", "mcp"):
            first_edges, first_cursor = page(j, surface, alias, f"{surface}-first")
            second_edges, second_cursor = page(j, surface, alias, f"{surface}-second", first_cursor)
            check(set(first_edges).isdisjoint(second_edges), f"{surface}: frontier repeated an edge")
            baseline[surface] = (first_edges, first_cursor, second_edges, owner_cursor(surface, second_cursor))
        # Adaptive output limits may yield different surface page boundaries.
        # Compare only when the sealed delivery scopes actually match.
        if owner_scope("cli", baseline["cli"][1]) == owner_scope("mcp", baseline["mcp"][1]):
            check(baseline["cli"][0] == baseline["mcp"][0], "CLI/MCP equal-scope first pages disagree")
            check(owner_cursor("cli", baseline["cli"][1]) == owner_cursor("mcp", baseline["mcp"][1]),
                  "CLI/MCP equal-scope first frontiers disagree")
        j.stop_mcp()
        j.stop_owner()
        j.start_owner(project)
        j.start_mcp(project)
        for surface in ("cli", "mcp"):
            _, first_cursor, expected_edges, expected_next = baseline[surface]
            actual_edges, actual_next = page(j, surface, alias, f"cold-{surface}-second", first_cursor)
            check(actual_edges == expected_edges, f"{surface}: cold continuation edges changed")
            check(owner_cursor(surface, actual_next) == expected_next, f"{surface}: cold continuation frontier changed")
        with (project / "src" / "center.ts").open("a") as stream:
            stream.write("\nexport const related_revision_marker = 1;\n")
        index(j, project, "changed-index")
        for surface in ("cli", "mcp"):
            if surface == "mcp":
                owner_cursor("mcp", baseline[surface][1])  # exclude an expired envelope
            stale(j, surface, alias, baseline[surface][1])
        result.update(status="passed", first_page_edges={key: len(value[0]) for key, value in baseline.items()})
    except Exception as error:
        result["error"] = repr(error)
        raise
    finally:
        if result["status"] == "passed":
            j.close()
        else:
            j.stop_mcp()
            j.stop_owner()
            result["preserved_private_workspace"] = str(j.root)
        try:
            if any("cleanup_error" in record for record in j.records) or (result["status"] == "passed" and j.root.exists()):
                raise RuntimeError("journey process retirement or cleanup failed")
            after = verify_build()
            result["build_after"] = after
            if after != build:
                raise RuntimeError("build receipt, source, or binaries changed during journey")
        except Exception as error:
            result.update(status="failed", verification_error=repr(error))
            raise
        finally:
            result["records"] = j.records
            (output / "related-cold-result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
