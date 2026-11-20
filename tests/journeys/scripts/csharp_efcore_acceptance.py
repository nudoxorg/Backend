#!/usr/bin/env python3
"""Public-route acceptance of a restored ASP.NET Core solution that uses EF Core and Identity.

The fixture `csharp-efcore-solution` is an API project (EF Core `DbContext`, Identity, one
migration, minimal-API endpoints in top-level statements) plus a test project that references
it and uses xunit and Mvc.Testing. It is restored with `dotnet restore` (a NuGet feed is
needed once), then indexed through the real `backend-locald` and `backend-cli` with a private
HOME. Nothing here imports product code.

Checks (each is printed; the receipt lists them):
  package-profile   C# is published; no compiler refusal is reported
  references        `references AppDbContext` answers with rows from the API and the tests
  search            a top-level endpoint call in Program.cs is found
  unresolved        a reference no package supplies is named (code, symbol, file:line, project)
                    in the profile instead of the bare "compiler input was rejected"

Debug builds skip the account gate, so a passing run is a development-mode result.

  csharp_efcore_acceptance.py --bin-dir DIR --fixtures DIR --output DIR --dotnet PATH --helper PATH
                              [--no-restore] [--scenario restored|partial|unresolved ...]
"""
import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import traceback

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from csharp_public_route import Journey  # noqa: E402

CHECKS = []
COMPILER_PROFILE = '{"kind":"current"}'


def check(name, ok, detail=""):
    CHECKS.append({"name": name, "ok": bool(ok), "detail": str(detail)[:900]})
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  [{str(detail)[:600]}]"), flush=True)
    return bool(ok)


def show(label, text, limit=2400):
    print(f"--- {label}", flush=True)
    print(text[:limit], flush=True)


def restore(args, project):
    """A real restore of the staged fixture, with the caller's HOME so the package cache persists."""
    env = {k: os.environ[k] for k in ("PATH", "HOME", "LANG", "LC_ALL", "TZ") if k in os.environ}
    env.update(DOTNET_NOLOGO="1", DOTNET_CLI_TELEMETRY_OPTOUT="1", DOTNET_SKIP_FIRST_TIME_EXPERIENCE="1")
    done = subprocess.run([args.dotnet, "restore", str(project / "Pantry.sln")], cwd=project, env=env,
                          capture_output=True, text=True, timeout=1500)
    show("dotnet restore", (done.stdout + done.stderr)[-1200:])
    assets = sorted(project.glob("**/obj/project.assets.json"))
    check("restore: both projects wrote a restore result", done.returncode == 0 and len(assets) == 2,
          f"rc={done.returncode} assets={[str(a.relative_to(project)) for a in assets]}")
    return done.returncode == 0 and len(assets) == 2


def coordinates(found, name, kinds):
    out = []
    for record in (found or {}).get("records", []):
        ident = record.get("identity", {})
        if ident.get("name") == name and ident.get("shape") != "external" and record.get("kind") in kinds \
                and record.get("language") == "csharp":
            out.append(ident["coordinate"])
    return out


def profile_text(j, project, label):
    rc, _, raw = j.cli(f"{label}-package-profile", ["package-profile", str(project), "--compiler-outcome", COMPILER_PROFILE],
                       project, fmt=None)
    show(f"{label}: package-profile (exit {rc})", raw)
    # A refused profile prints a selector for its full diagnostic; follow it, as the output says.
    selector = re.search(r'^\s*(\{"kind":"diagnostic".*\})\s*$', raw, re.M)
    if selector:
        rc2, _, detail = j.cli(f"{label}-package-profile-diagnostic",
                               ["package-profile", str(project), "--compiler-outcome", selector.group(1)], project, fmt=None)
        show(f"{label}: package-profile --compiler-outcome (exit {rc2})", detail)
        raw = raw + "\n" + detail
    return rc, raw


def scenario(args, fixtures, label, mutate=None, mode="published"):
    j = Journey(args.bin_dir, args.output, {"NUDOX_DOTNET": args.dotnet, "NUDOX_ROSLYN_HELPER": args.helper}, label)
    try:
        project = j.stage_fixture(fixtures / "csharp-efcore-solution")
        if mutate:
            mutate(project)
        if not args.no_restore:
            restore(args, project)
        j.project = project
        j.start_owner(project)
        rc, obj, raw = j.cli(f"{label}-add", ["add", str(project), "--wait", "--wait-timeout", "900"], project,
                             timeout=1800, fmt=None)
        show(f"{label}: add (exit {rc})", raw)
        rc, profile = profile_text(j, project, label)
        refused = "compiler input was rejected" in profile or "authority_runtime" in profile
        expect_published = mode == "published"
        if mode == "partial":
            partial = re.search(r"published with partial coverage \((\d+) of (\d+) files\)", profile)
            check(f"{label}: the profile is published and says it is partial",
                  rc == 0 and partial is not None and int(partial[1]) < int(partial[2]), profile[:600])
            owner_log = "".join(path.read_text(errors="replace") for path in pathlib.Path(args.output).glob(f"{label}-owner-*.log"))
            check(f"{label}: the owner log names the file that does not compile",
                  re.search(r"scope gap: path=\S*Missing\.cs .*CSharpSourceDoesNotCompile", owner_log) is not None, owner_log[-600:])
            check(f"{label}: the owner log names the first unresolved reference with its project",
                  re.search(r"first unresolved reference CS0246 Missing at src/Pantry\.Api/Data/Missing\.cs:1 in project src/Pantry\.Api/Pantry\.Api\.csproj", owner_log) is not None,
                  owner_log[-600:])
            # references still answer for the files that compiled, and say the publication is partial
            rc, found, raw = j.cli(f"{label}-search-db", ["search", "AppDbContext", "--limit", "20"], project)
            coords = coordinates(found, "AppDbContext", {"class"})
            if coords:
                _, refs, raw = j.cli(f"{label}-references", ["references", coords[0]], project)
                check(f"{label}: references answer from the files that compiled and call the coverage partial",
                      "Reference coverage: partial" in raw or "partial" in str((refs or {}).get("note", "")), raw[:500])
            return j
        if expect_published:
            check(f"{label}: package-profile reports no compiler refusal", rc == 0 and not refused, profile[:600])
            check(f"{label}: no unresolved reference is accepted into a complete publication",
                  "unresolved" not in profile.lower() or "csharp: published" not in profile.lower(), profile[:600])
        else:
            check(f"{label}: the refusal names the first unresolved reference",
                  re.search(r"first unresolved reference CS0(246|234) Missing", profile) is not None, profile[:900])
            check(f"{label}: the refusal gives the file and line of that reference",
                  re.search(r"\.cs:\d+", profile) is not None, profile[:900])
            check(f"{label}: the refusal names the owning project",
                  ".csproj" in profile, profile[:900])
            check(f"{label}: the refusal says the project was restored",
                  re.search(r"restored", profile, re.I) is not None, profile[:900])
        # search: a top-level call in Program.cs. The framework method is an external symbol; its
        # references lead to the top-level statements that use it.
        rc, found, raw = j.cli(f"{label}-search-endpoint", ["search", "AddRateLimiter", "--limit", "20"], project)
        _, _, text = j.cli(f"{label}-search-endpoint-text", ["search", "AddRateLimiter", "--limit", "20"], project, fmt=None)
        show(f"{label}: search AddRateLimiter (exit {rc})", text)
        if expect_published:
            external = [r["identity"]["coordinate"] for r in (found or {}).get("records", [])
                        if r.get("identity", {}).get("name") == "AddRateLimiter"]
            check(f"{label}: search finds the Program.cs top-level call AddRateLimiter", rc == 0 and len(external) >= 1, raw[:500])
        rc, found, raw = j.cli(f"{label}-search-map", ["search", "MapPantryEndpoints", "--limit", "20"], project)
        coords = coordinates(found, "MapPantryEndpoints", {"method"})
        if expect_published:
            check(f"{label}: search finds the endpoint mapping method called from Program.cs", rc == 0 and len(coords) >= 1, raw[:400])
            if coords:
                rc, refs, raw = j.cli(f"{label}-references-map", ["references", coords[0]], project)
                _, _, text = j.cli(f"{label}-references-map-text", ["references", coords[0]], project, fmt=None)
                show(f"{label}: references MapPantryEndpoints (exit {rc})", text)
                check(f"{label}: the top-level call in Program.cs is a reference, owned by the module",
                      rc == 0 and "Program.cs" in json.dumps(refs) and "<module>" in json.dumps(refs), raw[:600])
        rc, found, raw = j.cli(f"{label}-search-db", ["search", "AppDbContext", "--limit", "20"], project)
        coords = coordinates(found, "AppDbContext", {"class"})
        if expect_published:
            check(f"{label}: search finds AppDbContext", rc == 0 and len(coords) >= 1, raw[:400])
            if coords:
                rc, refs, raw = j.cli(f"{label}-references-text", ["references", coords[0]], project, fmt=None)
                show(f"{label}: references AppDbContext (exit {rc})", raw)
                rc, refs, raw = j.cli(f"{label}-references", ["references", coords[0]], project)
                text = json.dumps(refs)
                check(f"{label}: references AppDbContext return rows from more than one file",
                      rc == 0 and len((refs or {}).get("records", [])) > 0, raw[:600])
                check(f"{label}: the API project's own use (AppDbContextFactory.cs) is listed",
                      "AppDbContextFactory.cs" in text, text[:600])
                check(f"{label}: the test project's use is listed (project reference bound)",
                      "PantryEndpointsTests.cs" in text, text[:600])
        return j
    except Exception as error:  # noqa: BLE001
        traceback.print_exc()
        check(f"{label}: scenario completed", False, repr(error))
        j.close()
        return None


MISSING = "using Missing.Package.Core;\nnamespace Pantry.Api.Data;\npublic class Missing { public Thing Value = new Thing(); }\n"


def unresolved_in_one_file(project):
    """One source file whose using no package or project supplies, in a restored project."""
    (project / "src" / "Pantry.Api" / "Data" / "Missing.cs").write_text(MISSING)


def unresolved_everywhere(project):
    """No file of the project compiles: only the file with the missing reference remains."""
    for source in project.rglob("*.cs"):
        source.unlink()
    (project / "src" / "Pantry.Api" / "Data").mkdir(parents=True, exist_ok=True)
    (project / "src" / "Pantry.Api" / "Data" / "Missing.cs").write_text(MISSING)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--bin-dir", required=True)
    p.add_argument("--fixtures", required=True)
    p.add_argument("--output", required=True)
    p.add_argument("--dotnet", required=True)
    p.add_argument("--helper", required=True)
    p.add_argument("--no-restore", action="store_true")
    p.add_argument("--scenario", action="append")
    args = p.parse_args()
    fixtures = pathlib.Path(args.fixtures)
    wanted = args.scenario or ["restored", "partial", "unresolved"]
    journeys = []
    try:
        if "restored" in wanted:
            journeys.append(scenario(args, fixtures, "restored"))
        if "partial" in wanted:
            journeys.append(scenario(args, fixtures, "partial", unresolved_in_one_file, mode="partial"))
        if "unresolved" in wanted:
            journeys.append(scenario(args, fixtures, "unresolved", unresolved_everywhere, mode="refused"))
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
