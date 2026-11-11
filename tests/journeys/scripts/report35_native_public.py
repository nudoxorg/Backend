#!/usr/bin/env python3
"""Report 35 TS-only regression checks through real CLI and MCP processes.

The fixture has both .ts and .tsx profiles and no structural-language file.
Debug binaries exercise development mode, not signed-in release behavior.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent / "report_routes"))
from route import Route, receipt_header


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    args = parser.parse_args()
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

    def names(body):
        return [identity(row).get("name") for row in records(body)]

    def tool(label, name, arguments):
        reply = route.tool(name, arguments)
        check(label + ":mcp", not reply.get("isError", False), reply)
        return reply.get("structuredContent", {})

    try:
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
        (project / "src/data.ts").write_text(
            'export function loadSectionsCsv(): string { return "sections"; }\n'
        )
        (project / "src/scheduler.ts").write_text(
            'import { loadSectionsCsv } from "@/data";\n'
            'export async function generateSchedule(req: string): Promise<string> {\n'
            '  return loadSectionsCsv() + req;\n'
            '}\n'
        )
        (project / "src/route.ts").write_text(
            'import { generateSchedule } from "@/scheduler";\n'
            'export async function POST(): Promise<string> { return generateSchedule("request"); }\n'
        )
        (project / "src/NavBar.tsx").write_text(
            'export function NavBar(): string { return "navigation"; }\n'
        )
        route.start_owner(project)
        cli("add", "add", str(project), "--wait", "--wait-timeout", "900")
        coordinates = {}
        for name in ("generateSchedule", "loadSectionsCsv", "POST", "NavBar"):
            body, _ = cli("search-" + name, "search", name, "--limit", "20")
            found = [row for row in records(body) if identity(row).get("name") == name]
            check("search:" + name, bool(found), names(body))
            if found:
                coordinates[name] = identity(found[0]).get("coordinate")
        cli("status", "status")
        outline, _ = cli("outline", "outline", "--limit", "50")
        check("outline:tsx", "NavBar" in json.dumps(outline), outline)
        query = "{ Declaration { name } }"
        body, text = cli("declarations", "query", query)
        check("query:declaration", "generateSchedule" in text, body)
        for name, caller in (("generateSchedule", "POST"), ("loadSectionsCsv", "generateSchedule")):
            coordinate = coordinates.get(name)
            if not coordinate:
                check("reads:" + name, False, "search did not emit an actionable coordinate")
                continue
            _, source = cli("source-" + name, "source", coordinate)
            check("source:" + name, "function " + name in source, source[-1200:])
            body, text = cli("references-" + name, "references", coordinate)
            check("references:" + name, caller in text, body)
        # The historical path coordinate must remain usable on unchanged source.
        coordinate = coordinates.get("generateSchedule")
        if coordinate:
            project_prefix = coordinate.split("::", 1)[0]
            path_coordinate = project_prefix + "::src/scheduler.ts:2::generateSchedule"
            for verb in ("show", "source", "document", "graph", "related", "references"):
                cli("path-" + verb, verb, path_coordinate)
            _, shown = cli("signature", "show", coordinate)
            check("signature:modifiers", "export async" in shown, shown[-2000:])
            check("signature:return", "Promise<string>" in shown, shown[-2000:])
        route.start_mcp(project)
        body = tool("search", "backend.search", {"query": "generateSchedule", "limit": 20})
        check("mcp:search:declaration", "generateSchedule" in names(body), body)
        if coordinate:
            body = tool("source", "backend.source", {"coordinate": coordinate})
            check("mcp:source:body", "loadSectionsCsv" in json.dumps(body), body)
        # Stop only this fixture's processes and reopen the same durable state.
        for process in (route.mcp, route.owner):
            if process is not None and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=15)
        route.mcp = None
        route.owner = None
        deadline = time.monotonic() + 15
        while route.endpoint.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        route.start_owner(project)
        body, _ = cli("reopen-search", "search", "generateSchedule", "--limit", "20")
        check("reopen:search", "generateSchedule" in names(body), body)
    except Exception as error:
        check("runner", False, str(error))
    finally:
        route.stop()
        receipt = receipt_header(args.bin_dir, args.source_commit)
        receipt["scope"] = "synthetic TS/TSX-only CLI/MCP and cold-reopen regression; no real React or WSL proof"
        receipt["checks"] = checks
        receipt["pass"] = bool(checks) and all(item["pass"] for item in checks)
        (args.out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({"pass": receipt["pass"], "checks": len(checks), "failed": [x["check"] for x in checks if not x["pass"]]}))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
