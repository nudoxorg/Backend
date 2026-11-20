#!/usr/bin/env python3
"""End-to-end acceptance of C# indexing through the public CLI and MCP routes.

One isolated owner per scenario, the real `backend-cli` and `backend-mcp` binaries,
and the fixtures under `tests/journeys/fixtures`. Nothing here imports product code.

Scenarios
  solution   a multi-project ASP.NET solution (.slnx) with a TypeScript client:
             publication, definitions, cross-file and cross-project references,
             top-level statements, exact source, mixed C# + TypeScript, cold reopen.
  single     a single top-level ASP.NET project.
  no-sdk     an owner without a .NET host: an honest gap, never an empty "complete".
  no-helper  a .NET host but no Roslyn helper: the same.
  broken     a project the compiler cannot bind (an unrestored package): an honest
             gap with an actionable diagnostic.

Binaries built in a debug profile skip the account gate, so a passing run is a
development-mode result.

  csharp_aspnet_acceptance.py --bin-dir DIR --fixtures DIR --output DIR \\
      --dotnet PATH --helper PATH [--scenario NAME ...]
"""
import argparse
import json
import pathlib
import re
import shutil
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from csharp_public_route import Journey  # noqa: E402

CHECKS = []
SPAN = re.compile(r"^(?P<file>.+?) \[bytes(?P<start>\d+)\.\.(?P<end>\d+)\)$")


def check(name, ok, detail=""):
    CHECKS.append({"name": name, "ok": bool(ok), "detail": str(detail)[:600]})
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  [{str(detail)[:300]}]"), flush=True)
    return bool(ok)


def spans(reply):
    """(file, start, end) for every reference row that carries an exact byte span."""
    found = set()
    for row in (reply or {}).get("records", []):
        for tag in row.get("tags", []):
            match = SPAN.match(tag) if isinstance(tag, str) else None
            if match:
                found.add((match["file"], int(match["start"]), int(match["end"])))
    return found


def span_text(project, span):
    return (project / span[0]).read_bytes()[span[1]:span[2]].decode("utf-8", "replace")


def declarations(reply, name, kinds, language):
    rows = []
    for record in (reply or {}).get("records", []):
        ident = record.get("identity", {})
        if ident.get("name") != name or ident.get("shape") == "external":
            continue
        if kinds and record.get("kind") not in kinds:
            continue
        if language and record.get("language") != language:
            continue
        rows.append(record)
    return rows


def coverage_word(reply):
    note = str((reply or {}).get("note", ""))
    match = re.search(r"Reference coverage: (\w+)", note)
    return match.group(1).lower() if match else ""


def env_for(args, with_sdk=True, with_helper=True):
    out = {}
    if with_sdk and args.dotnet:
        out["NUDOX_DOTNET"] = args.dotnet
    if with_sdk and with_helper and args.helper:
        out["NUDOX_ROSLYN_HELPER"] = args.helper
    return out


def find_declaration(j, project, label, name, kinds, language="csharp"):
    rc, found, raw = j.cli(f"{label}-search", ["search", name, "--limit", "50"], project)
    rows = declarations(found, name, kinds, language)
    check(f"{label}: search finds {name} ({language}, {sorted(kinds) if kinds else 'any kind'})",
          rc == 0 and len(rows) >= 1, raw[:300])
    return [r["identity"]["coordinate"] for r in rows]


def exercise(j, project, label, name, kinds, expect_files, snippet, language="csharp", phase="warm"):
    """Definition, references and exact source of one declaration, by CLI and MCP.

    Returns the set of reference spans so a later phase can compare."""
    coords = find_declaration(j, project, f"{label}-{phase}", name, kinds, language)
    if not coords:
        return None
    best = None
    for index, coord in enumerate(coords):
        rc, refs, raw = j.cli(f"{label}-{phase}-references-{index}", ["references", coord], project)
        if rc == 0 and isinstance(refs, dict):
            found = spans(refs)
            if best is None or len(found) > len(best[1]):
                best = (coord, found, refs)
    if not check(f"{label}-{phase}: references answer for {name}", best is not None, coords):
        return None
    coord, found, refs = best
    files = sorted({f for f, _, _ in found})
    missing = [f for f in expect_files if f not in files]
    check(f"{label}-{phase}: {name} references cover {expect_files}", not missing,
          f"missing {missing}; got {files}")
    wrong = [s for s in found if name not in span_text(project, s)]
    check(f"{label}-{phase}: every {name} reference span is exactly the identifier", not wrong, wrong[:3])
    check(f"{label}-{phase}: {name} references state their coverage and never claim complete",
          coverage_word(refs) in ("partial", "unknown"), refs.get("note"))
    rc, _, raw = j.cli(f"{label}-{phase}-source", ["source", coord], project)
    check(f"{label}-{phase}: exact source of {name} (CLI)", rc == 0 and snippet in raw, raw[:300])
    try:
        mcp_refs = j.tool("backend.references", {"coordinate": coord})
        mcp_spans = spans(mcp_refs.get("structuredContent", {}))
        check(f"{label}-{phase}: MCP references equal CLI for {name}",
              not mcp_refs.get("isError") and mcp_spans == found, f"cli {len(found)} mcp {len(mcp_spans)}")
        mcp_source = j.tool("backend.source", {"coordinate": coord})
        check(f"{label}-{phase}: exact source of {name} (MCP)",
              not mcp_source.get("isError") and snippet in json.dumps(mcp_source), str(mcp_source)[:200])
    except Exception as error:  # noqa: BLE001 - reported as a failed check
        check(f"{label}-{phase}: MCP calls for {name}", False, repr(error))
    return found


def index_published(j, project, label):
    rc, obj, raw = j.cli(f"{label}-add", ["add", str(project), "--wait", "--wait-timeout", "900"], project, timeout=1800)
    state = (((obj or {}).get("index_operation") or {}).get("detail") or {}).get("state", {}).get("state")
    return rc, state, raw


def scenario_solution(args, fixtures):
    j = Journey(args.bin_dir, args.output, env_for(args), "solution")
    try:
        project = j.stage_fixture(fixtures / "csharp-aspnet-solution")
        j.project = project
        j.start_owner(project)
        j.start_mcp(project)
        rc, state, raw = index_published(j, project, "solution")
        check("solution: index reaches the fully published terminal state", rc == 0 and state == "published",
              f"rc={rc} state={state} {raw[:300]}")
        check("solution: C# and TypeScript both published with complete coverage",
              raw.count("with complete coverage") >= 2, raw[:600])
        cases = [
            ("MissingFields", {"method"}, ["src/Ledger.Api/AccountEndpoints.cs", "tests/Ledger.Api.Tests/ValidatorChecks.cs"],
             "public static Dictionary<string, string[]> MissingFields"),
            ("ToAccount", {"method"}, ["src/Ledger.Api/AccountEndpoints.cs", "tests/Ledger.Api.Tests/ValidatorChecks.cs"],
             "public static Account ToAccount"),
            ("MapAccountEndpoints", {"method"}, ["src/Ledger.Api/Program.cs"],
             "public static RouteGroupBuilder MapAccountEndpoints"),
            ("CreateAccountRequest", {"class"}, ["tests/Ledger.Api.Tests/ValidatorChecks.cs"],
             "public record CreateAccountRequest"),
            ("Account", {"class"}, ["tests/Ledger.Api.Tests/ValidatorChecks.cs"], "public record Account(Guid Id"),
        ]
        warm = {}
        for name, kinds, files, snippet in cases:
            warm[name] = exercise(j, project, f"solution-{name}", name, kinds, files, snippet)
        # An interface used only in signatures and generic arguments: the compiler image
        # records no occurrence for those, and the answer must say so rather than "none".
        coords = find_declaration(j, project, "solution-IAccountStore", "IAccountStore", {"interface"})
        if coords:
            rc, refs, raw = j.cli("solution-IAccountStore-references", ["references", coords[0]], project)
            check("solution: a use the compiler does not record is never reported as a complete empty answer",
                  rc == 0 and coverage_word(refs) in ("partial", "unknown"), refs and refs.get("note"))
        # Mixed language: the TypeScript client resolves through the same routes.
        ts = exercise(j, project, "solution-listAccounts", "listAccounts", {"function"}, [],
                      "export async function listAccounts", language="typescript")
        check("solution: TypeScript declaration is found next to the C# ones", ts is not None)
        # The call in the top-level statements of Program.cs is attributed to the file's module owner.
        coords = find_declaration(j, project, "solution-top-level", "MapAccountEndpoints", {"method"})
        if coords:
            rc, refs, raw = j.cli("solution-top-level-references", ["references", coords[0]], project)
            check("solution: the top-level call in Program.cs is attributed to its module owner",
                  rc == 0 and any("<module>" in str(r.get("title", "")) for r in (refs or {}).get("records", [])),
                  raw[:300])
        # Cold reopen: a new owner process over the same durable workspace.
        j.stop_mcp()
        j.stop_owner()
        j.start_owner(project)
        j.start_mcp(project)
        for name, kinds, files, snippet in cases:
            cold = exercise(j, project, f"solution-{name}", name, kinds, files, snippet, phase="cold")
            check(f"solution-cold: {name} references are identical after reopen",
                  cold is not None and cold == warm[name], f"warm {warm[name]} cold {cold}")
        return j
    except Exception as error:  # noqa: BLE001
        check("solution: scenario completed", False, repr(error))
        j.close()
        return None


def scenario_single(args, fixtures):
    j = Journey(args.bin_dir, args.output, env_for(args), "single")
    try:
        project = j.stage_fixture(fixtures / "csharp-aspnet-single")
        j.project = project
        j.start_owner(project)
        j.start_mcp(project)
        rc, state, raw = index_published(j, project, "single")
        check("single: index reaches the fully published terminal state", rc == 0 and state == "published",
              f"rc={rc} state={state} {raw[:300]}")
        exercise(j, project, "single-MapNoteEndpoints", "MapNoteEndpoints", {"method"}, ["Program.cs"],
                 "public static void MapNoteEndpoints")
        exercise(j, project, "single-Add", "Add", {"method"}, ["Notes/NoteEndpoints.cs"], "public Note Add(string text)")
        exercise(j, project, "single-Note", "Note", {"class"}, ["Notes/NoteStore.cs"], "public record Note(int Id")
        return j
    except Exception as error:  # noqa: BLE001
        check("single: scenario completed", False, repr(error))
        j.close()
        return None


def degraded(args, fixtures, label, with_sdk, mutate, expected_words, with_helper=True):
    j = Journey(args.bin_dir, args.output, env_for(args, with_sdk, with_helper), label)
    try:
        project = j.stage_fixture(fixtures / "csharp-aspnet-single")
        mutate(project)
        j.project = project
        j.start_owner(project)
        rc, state, raw = index_published(j, project, label)
        check(f"{label}: the index does not claim a full publication", state != "published", f"state={state}")
        lowered = raw.lower()
        for word in expected_words:
            check(f"{label}: the diagnostic says {word!r}", word.lower() in lowered, raw[:700])
        coords = find_declaration(j, project, label, "NoteStore", {"class"})
        if coords:
            rc, refs, raw = j.cli(f"{label}-references", ["references", coords[0]], project)
            check(f"{label}: references are never a complete empty answer",
                  rc != 0 or (not spans(refs) and coverage_word(refs) == "unknown"),
                  (refs or {}).get("note") or raw[:300])
            check(f"{label}: references name the missing C# publication",
                  rc != 0 or "no selected semantic publication" in str((refs or {}).get("note", "")), (refs or {}).get("note"))
        return j
    except Exception as error:  # noqa: BLE001
        check(f"{label}: scenario completed", False, repr(error))
        j.close()
        return None


def break_with_unrestored_package(project):
    (project / "Notes" / "Restored.cs").write_text(
        "using Missing.Package.Core;\nnamespace Notes;\npublic class Restored { public Thing Value = new Thing(); }\n")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--bin-dir", required=True)
    p.add_argument("--fixtures", required=True)
    p.add_argument("--output", required=True)
    p.add_argument("--dotnet")
    p.add_argument("--helper")
    p.add_argument("--scenario", action="append")
    args = p.parse_args()
    fixtures = pathlib.Path(args.fixtures)
    wanted = args.scenario or ["solution", "single", "no-sdk", "no-helper", "broken"]
    journeys = []
    try:
        if "solution" in wanted:
            journeys.append(scenario_solution(args, fixtures))
        if "single" in wanted:
            journeys.append(scenario_single(args, fixtures))
        if "no-sdk" in wanted:
            journeys.append(degraded(args, fixtures, "no-sdk", False, lambda project: None, ["NUDOX_DOTNET"]))
        if "no-helper" in wanted:
            journeys.append(degraded(args, fixtures, "no-helper", True, lambda project: None, ["NUDOX_ROSLYN_HELPER"],
                                     with_helper=False))
        if "broken" in wanted:
            journeys.append(degraded(args, fixtures, "broken", True, break_with_unrestored_package,
                                     ["references", "resolve"]))
    finally:
        for journey in journeys:
            if journey:
                journey.close()
    out = pathlib.Path(args.output)
    out.mkdir(parents=True, exist_ok=True)
    (out / "receipt.json").write_text(json.dumps({"mode": "development", "checks": CHECKS}, indent=2) + "\n")
    failed = [c for c in CHECKS if not c["ok"]]
    print(f"{len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
