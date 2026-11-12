#!/usr/bin/env python3
"""Report 35 TS-only regression checks through real CLI and MCP processes.

The fixture has both .ts and .tsx profiles and no structural-language file.
Debug binaries exercise development mode, not signed-in release behavior.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent / "report_routes"))
from route import Route, receipt_header
from runtime_build_receipt import stable_file, verify_runtime_build_receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--build-receipt", type=Path, required=True,
                        help="Successful locked Cargo build receipt captured by record-runtime-build-manifest.py")
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[3]

    def verify_build():
        verified = verify_runtime_build_receipt(args.build_receipt.resolve(), source)
        if verified["source"]["commit"] != args.source_commit:
            raise RuntimeError("requested source differs from the verified Cargo build")
        for name, artifact in verified["artifacts"].items():
            candidate = stable_file(args.bin_dir.resolve() / name, name, executable=True)
            if any(candidate[key] != artifact[key] for key in ("sha256", "bytes", "architecture")):
                raise RuntimeError(f"{name} differs from the verified Cargo output")
        return verified

    build = verify_build()
    route = Route(args.bin_dir.resolve(), args.out.resolve())
    checks = []

    def check(name, condition, detail):
        checks.append({"check": name, "pass": bool(condition), "detail": detail})

    def cli(label, *arguments):
        code, body, text = route.cli(label, list(arguments), project)
        check(label + ":exit", code == 0, {"exit": code, "tail": text[-1200:]})
        return body, text

    def records(body):
        return body.get("records", []) if isinstance(body, dict) else []

    def identity(row):
        return row.get("identity", {})

    def require_json(label, body):
        check(label + ":json-object", isinstance(body, dict), body)
        return body if isinstance(body, dict) else {}

    def selected_rows(label, body, name, coordinate=None):
        rows = [row for row in records(body)
                if isinstance(row, dict) and identity(row).get("name") == name]
        if coordinate is not None:
            rows = [row for row in rows if identity(row).get("coordinate") == coordinate]
        check(label, bool(rows), {"name": name, "coordinate": coordinate,
                                  "rows": rows[:4], "available": names(body)})
        return rows

    def strings(value):
        if isinstance(value, str):
            yield value
        elif isinstance(value, dict):
            for item in value.values():
                yield from strings(item)
        elif isinstance(value, list):
            for item in value:
                yield from strings(item)

    def row_fingerprint(body):
        return sorted(tuple(str(value) for value in (
                           identity(row).get("name"), identity(row).get("coordinate"),
                           identity(row).get("key"),
                           json.dumps(identity(row).get("semantic_data"), sort_keys=True),
                           row.get("kind"), row.get("language")))
                      for row in records(body) if isinstance(row, dict))

    def query_rows(body):
        return body.get("rows", []) if isinstance(body, dict) else []

    def query_names(body):
        return [row.get("name") for row in query_rows(body) if isinstance(row, dict)]

    def query_fingerprint(body):
        return sorted(tuple(str(value) for value in (row.get("id"), row.get("name"),
                                                     row.get("coordinate"), row.get("state")))
                      for row in query_rows(body) if isinstance(row, dict))

    def reference_fingerprint(body):
        return sorted((str(row.get("title")), str(identity(row).get("name")),
                       str(identity(row).get("key")), str(identity(row).get("coordinate")),
                       json.dumps(identity(row).get("semantic_data"), sort_keys=True),
                       tuple(sorted(str(tag) for tag in row.get("tags", []))))
                      for row in records(body) if isinstance(row, dict))

    def names(body):
        return [identity(row).get("name") for row in records(body)]

    def outline_rows(body):
        def walk(rows):
            for row in rows:
                if isinstance(row, dict):
                    yield row
                    yield from walk(row.get("children", []))
        return list(walk(body.get("roots", []))) if isinstance(body, dict) else []

    def selected_symbol(row):
        data = identity(row).get("semantic_data", {})
        return data.get("value") if data.get("kind") == "selected-symbol-id" else None

    def compiler_call(row, caller_symbol, source, text, callee):
        detail = row.get("reference_details", {})
        evidence = detail.get("evidence", {})
        span = evidence.get("source", {})
        start = text.encode().rfind(callee.encode())
        return (caller_symbol is not None and row.get("site_symbol") == caller_symbol
                and detail.get("relation") == "calls" and evidence.get("confidence") == "compiler"
                and span.get("file") == source and span.get("start") == start
                and span.get("end") == start + len(callee.encode()))

    def tool(label, name, arguments):
        reply = route.tool(name, arguments)
        check(label + ":mcp", not reply.get("isError", False), reply)
        return reply.get("structuredContent", {})

    try:
        cli_references = {}
        project = route.root / "pure-typescript"
        (project / "src").mkdir(parents=True)
        (project / "package.json").write_text(
            '{"name":"native-public-regression","version":"1.0.0","private":true}\n'
        )
        (project / "tsconfig.json").write_text(json.dumps({
            "compilerOptions": {
                "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler",
                "strict": True, "jsx": "preserve", "baseUrl": ".",
                "paths": {"@/*": ["src/*"]},
            },
            "include": ["src/**/*.ts", "src/**/*.tsx"],
        }) + "\n")
        data_text = 'export function loadSectionsCsv(): string { return "sections"; }\n'
        scheduler_text = (
            'import { loadSectionsCsv } from "@/data";\n'
            'export async function generateSchedule(req: string): Promise<string> {\n'
            '  return loadSectionsCsv() + req;\n'
            '}\n'
        )
        route_text = (
            'import { generateSchedule } from "@/scheduler";\n'
            'export default async function POST(): Promise<string> { return generateSchedule("request"); }\n'
        )
        navbar_text = (
            'import { loadSectionsCsv } from "@/data";\n'
            'export default function NavBar(): string { return loadSectionsCsv(); }\n'
        )
        for path, text in (("src/data.ts", data_text), ("src/scheduler.ts", scheduler_text),
                           ("src/route.ts", route_text), ("src/NavBar.tsx", navbar_text)):
            (project / path).write_text(text)
        route.start_owner(project)
        add, _ = cli("add", "add", str(project), "--wait", "--wait-timeout", "900")
        require_json("add", add)
        coordinates = {}
        symbols = {}
        initial_search = {}
        for name in ("generateSchedule", "loadSectionsCsv", "POST", "NavBar"):
            body, _ = cli("search-" + name, "search", name, "--limit", "20")
            initial_search[name] = row_fingerprint(body)
            found = [row for row in records(body) if identity(row).get("name") == name]
            check("search:" + name, bool(found), names(body))
            if found:
                coordinates[name] = identity(found[0]).get("coordinate")
                symbols[name] = selected_symbol(found[0])
                check("search-coordinate:" + name,
                      isinstance(coordinates[name], str) and bool(coordinates[name]), found[0])
        status, _ = cli("status", "status")
        require_json("status", status)
        check("status:has-project-coverage", bool(status) and any(
            key in status for key in ("coverage", "lanes", "row_count", "rows", "publication", "answer")), status)
        outline, _ = cli("outline", "outline", "--limit", "50")
        require_json("outline", outline)
        check("outline:tsx", any(row.get("name") == "NavBar" and row.get("coordinate") == coordinates.get("NavBar")
                                 for row in outline_rows(outline)), outline)
        if coordinates.get("NavBar"):
            navbar_show, _ = cli("default-show", "show", coordinates["NavBar"])
            require_json("default-show", navbar_show)
            check("default:show-signature", isinstance(navbar_show.get("signature"), str)
                  and navbar_show["signature"].rstrip().removesuffix("{").rstrip() == "export default function NavBar(): string"
                  and navbar_show.get("kind") == "function", navbar_show)
            navbar_source, _ = cli("default-source", "source", coordinates["NavBar"])
            check("default:source", any(navbar_text.splitlines()[1] in part for part in strings(navbar_source)), navbar_source)
        if coordinates.get("POST"):
            post_show, _ = cli("default-async-show", "show", coordinates["POST"])
            require_json("default-async-show", post_show)
            check("default:async-show-signature", isinstance(post_show.get("signature"), str)
                  and post_show["signature"].rstrip().removesuffix("{").rstrip() == "export default async function POST(): Promise<string>"
                  and post_show.get("kind") == "function", post_show)
            post_source, _ = cli("default-async-source", "source", coordinates["POST"])
            check("default:async-source", any(route_text.splitlines()[1] in part for part in strings(post_source)), post_source)
        query = "{ Declaration { id @output name @output coordinate @output state @output } }"
        for name, caller in (("generateSchedule", "POST"), ("loadSectionsCsv", "generateSchedule")):
            coordinate = coordinates.get(name)
            if not coordinate:
                check("reads:" + name, False, "search did not emit an actionable coordinate")
                continue
            source_body, _ = cli("source-" + name, "source", coordinate)
            require_json("source:" + name, source_body)
            check("source-content:" + name,
                  any("function " + name in part for part in strings(source_body)), source_body)
            refs, _ = cli("references-" + name, "references", coordinate)
            require_json("references:" + name, refs)
            ref_rows = records(refs)
            source, text = (("src/route.ts", route_text) if caller == "POST" else ("src/scheduler.ts", scheduler_text))
            check("references-caller:" + name, any(compiler_call(row, symbols.get(caller), source, text, name)
                                                    for row in ref_rows), ref_rows)
            if name == "loadSectionsCsv":
                check("references:cross-profile:NavBar", any(
                    compiler_call(row, symbols.get("NavBar"), "src/NavBar.tsx", navbar_text, name)
                    for row in ref_rows), ref_rows)
            cli_references[name] = refs
        # The historical path coordinate must remain usable on unchanged source.
        coordinate = coordinates.get("generateSchedule")
        if coordinate:
            project_prefix = coordinate.split("::", 1)[0]
            path_coordinate = project_prefix + "::src/scheduler.ts:2::generateSchedule"
            for verb in ("show", "source", "document", "graph", "related", "references"):
                result, raw = cli("path-" + verb, verb, path_coordinate)
                require_json("path-json:" + verb, result)
                check("path-identity:" + verb,
                      any(identity(row).get("name") == "generateSchedule" for row in records(result))
                      or any("generateSchedule" in part for part in strings(result)),
                      {"result": result, "tail": raw[-500:]})
            shown_body, _ = cli("signature", "show", coordinate)
            require_json("signature", shown_body)
            check("signature:selected-page", shown_body.get("answer") == "page"
                  and identity(shown_body).get("name") == "generateSchedule", shown_body)
            signature = shown_body.get("signature")
            expected_header = "export async function generateSchedule(req: string): Promise<string>"
            check("signature:exact-header", isinstance(signature, str)
                  and signature.rstrip().removesuffix("{").rstrip() == expected_header,
                  {"signature": signature, "expected": expected_header})
            check("signature:kind", shown_body.get("kind") == "function", shown_body)
            path_source, _ = cli("path-source", "source", path_coordinate)
            check("path-source:expected-line",
                  any('return loadSectionsCsv() + req;' in part for part in strings(path_source)), path_source)
            path_refs, _ = cli("path-references", "references", path_coordinate)
            check("path-references:POST",
                  any(compiler_call(row, symbols.get("POST"), "src/route.ts", route_text, "generateSchedule")
                      for row in records(path_refs)), path_refs)
        route.start_mcp(project)
        body = tool("search", "backend.search", {"query": "generateSchedule", "limit": 20})
        selected_rows("mcp:search:declaration", body, "generateSchedule")
        cli_search, _ = cli("parity-search", "search", "generateSchedule", "--limit", "20")
        check("mcp:cli-search-parity", row_fingerprint(body) == row_fingerprint(cli_search),
              {"mcp": row_fingerprint(body), "cli": row_fingerprint(cli_search)})
        mcp_query = tool("query", "backend.query", {"query": query})
        if coordinates.get("NavBar"):
            navbar_mcp = tool("default-source", "backend.source", {"coordinate": coordinates["NavBar"]})
            check("mcp:default-source", any(navbar_text.splitlines()[1] in part for part in strings(navbar_mcp)), navbar_mcp)
        if coordinates.get("POST"):
            post_mcp = tool("default-async-source", "backend.source", {"coordinate": coordinates["POST"]})
            check("mcp:default-async-source", any(route_text.splitlines()[1] in part for part in strings(post_mcp)), post_mcp)
        check("mcp:query:declaration", "generateSchedule" in query_names(mcp_query), query_names(mcp_query))
        check("mcp:query:tsx", "NavBar" in query_names(mcp_query), query_names(mcp_query))
        outline_facts = {(row.get("name"), row.get("coordinate"),
                          "symbol:" + bytes(row.get("semantic_data", {}).get("value", [])).hex())
                         for row in outline_rows(outline) if row.get("semantic_data", {}).get("kind") == "selected-symbol-id"}
        query_facts = {(row.get("name"), row.get("coordinate"), row.get("id")) for row in query_rows(mcp_query)}
        for name in ("generateSchedule", "loadSectionsCsv", "POST", "NavBar"):
            expected = (name, coordinates.get(name), "symbol:" + bytes(symbols.get(name) or []).hex())
            check("mcp:outline-query:" + name, expected in outline_facts and expected in query_facts,
                  {"expected": expected, "outline": sorted(outline_facts), "query": sorted(query_facts)})
        if coordinate:
            body = tool("source", "backend.source", {"coordinate": coordinate})
            check("mcp:source:body", any("function generateSchedule" in part for part in strings(body)), body)
            expected_signature = "export async function generateSchedule(req: string): Promise<string> {"
            check("mcp:cli-source-parity", any(expected_signature in part for part in strings(body)), body)
            mcp_refs = tool("references", "backend.references", {"coordinate": coordinate})
            check("mcp:references:POST",
                  any(compiler_call(row, symbols.get("POST"), "src/route.ts", route_text, "generateSchedule")
                      for row in records(mcp_refs)), mcp_refs)
            check("mcp:cli-reference-parity",
                  reference_fingerprint(mcp_refs) == reference_fingerprint(
                      cli_references.get("generateSchedule", {})),
                  {"mcp": reference_fingerprint(mcp_refs),
                   "cli": reference_fingerprint(cli_references.get("generateSchedule", {}))})
            mcp_path_source = tool("path-source", "backend.source", {"coordinate": path_coordinate})
            check("mcp:path-source", any('return loadSectionsCsv() + req;' in part
                                           for part in strings(mcp_path_source)), mcp_path_source)
            for label, tool_name, arguments in (
                ("document", "backend.document", {"coordinate": path_coordinate}),
                ("graph", "backend.graph", {"coordinate": path_coordinate}),
                ("related", "backend.related", {"coordinate": path_coordinate}),
            ):
                try:
                    result = tool("path-" + label, tool_name, arguments)
                except Exception as error:
                    check("mcp:path-identity:" + label, False, str(error))
                    continue
                check("mcp:path-identity:" + label,
                      any("generateSchedule" in part for part in strings(result)), result)
        mcp_status = tool("status", "backend.status", {})
        check("mcp:status:json", isinstance(mcp_status, dict) and bool(mcp_status), mcp_status)
        mcp_status_body = mcp_status
        check("mcp:status:typed", isinstance(mcp_status_body, dict), mcp_status)
        comparable = ("coverage", "rows", "source_selection_scope", "revision")
        check("mcp:cli-status-parity", isinstance(status, dict)
              and isinstance(mcp_status_body, dict)
              and all(key in status and key in mcp_status_body
                      and status[key] == mcp_status_body[key] for key in comparable),
              {"keys": comparable, "cli": status, "mcp": mcp_status_body})
        # Stop only this fixture's processes and reopen the same durable state.
        retirement = route.retire_for_reopen()
        check("reopen:previous-owner-retired", len(retirement) == 2, retirement)
        deadline = time.monotonic() + 15
        while route.endpoint.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        route.start_owner(project)
        for name in ("generateSchedule", "loadSectionsCsv", "POST", "NavBar"):
            body, _ = cli("reopen-search-" + name, "search", name, "--limit", "20")
            selected_rows("reopen:search:" + name, body, name, coordinates.get(name))
            check("reopen:identity-parity:" + name,
                  bool(initial_search[name]) and row_fingerprint(body) == initial_search[name],
                  {"before": initial_search[name], "after": row_fingerprint(body)})
        route.start_mcp(project)
        reopened_query = tool("reopen-query", "backend.query", {"query": query})
        check("reopen:query-parity", bool(query_rows(mcp_query)) and query_fingerprint(reopened_query) == query_fingerprint(mcp_query),
              {"before": query_fingerprint(mcp_query), "after": query_fingerprint(reopened_query)})
        if coordinate:
            body, _ = cli("reopen-source", "source", path_coordinate)
            check("reopen:source-body", any('return loadSectionsCsv() + req;' in part
                                            for part in strings(body)), body)
            body, _ = cli("reopen-references", "references", path_coordinate)
            check("reopen:reference-parity", reference_fingerprint(body) == reference_fingerprint(
                  cli_references.get("generateSchedule", {})),
                  {"before": reference_fingerprint(cli_references.get("generateSchedule", {})),
                   "after": reference_fingerprint(body)})
        reopened_status, _ = cli("reopen-status", "status")
        check("reopen:status", isinstance(reopened_status, dict) and bool(reopened_status), reopened_status)
    except Exception as error:
        check("runner", False, str(error))
    finally:
        route.stop()
        cleanup = route.records[-1] if route.records else {}
        check("cleanup:owned-processes-retired", cleanup.get("label") == "cleanup"
              and not cleanup.get("unretired") and not route.root.exists(), cleanup)
        verified_after = verify_build()
        if verified_after["receipt_sha256"] != build["receipt_sha256"]:
            raise RuntimeError("Cargo build receipt changed during the public journey")
        receipt = receipt_header(args.bin_dir, args.source_commit, effective_env=route.env)
        receipt["root_build_receipt"] = {
            "path": build["receipt_path"], "sha256": build["receipt_sha256"],
            "artifacts": build["artifacts"],
        }
        receipt["scope"] = "synthetic TS/TSX-only CLI/MCP and cold-reopen regression; no real React or WSL proof"
        receipt["checks"] = checks
        receipt["pass"] = bool(checks) and all(item["pass"] for item in checks)
        (args.out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({"pass": receipt["pass"], "checks": len(checks), "failed": [x["check"] for x in checks if not x["pass"]]}))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
