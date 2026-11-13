"""Scenario functions: each returns {finding_id: {"pass": bool, "detail": ...}}."""
import json, os, pathlib, tempfile, time

import fixtures
from route import Route


def _project(route, kind):
    proj = route.root / kind
    proj.mkdir()
    fixtures.BUILDERS[kind](proj)
    return proj


def _rows(obj):
    return (obj or {}).get("records", []) if isinstance(obj, dict) else []


def _coord(rows, name, kinds=None, shape=None):
    for row in rows:
        ident = row.get("identity", {})
        if ident.get("name") != name:
            continue
        if kinds and row.get("kind") not in kinds:
            continue
        if shape and ident.get("shape") != shape:
            continue
        return ident.get("coordinate")
    return None


def probe_budget(bin_dir, out):
    """Observation only: dump the public output of every read route on the budget fixture."""
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "budget")
        r.start_owner(proj)
        code, obj, text = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        for label, args in [
            ("status", ["status"]), ("health", ["health"]), ("packages", ["packages"]),
            ("search-button", ["search", "Button", "--limit", "10"]),
            ("search-button-1", ["search", "Button", "--limit", "1"]),
            ("search-api", ["search", "api", "--limit", "10"]),
            ("search-bankaccount", ["search", "BankAccount", "--limit", "10"]),
            ("search-addbank", ["search", "AddBankAccounts"]),
            ("search-multiword", ["search", "where is the balance validated"]),
            ("search-onmounted", ["search", "onMounted"]),
            ("resolve-fielderrors", ["resolve", "FieldErrors"]),
            ("resolve-request", ["resolve", "request"]),
            ("outline", ["outline", "--limit", "50"]),
        ]:
            r.cli(label, args, proj)
        # follow-ups that need coordinates
        _, found, _ = r.cli("find-button", ["search", "Button", "--limit", "50"], proj)
        coords = {}
        for name, kinds in [("Button", {"function"}), ("AppShell", {"function"}), ("AuthCard", {"function"}),
                            ("ApiError", {"class"}), ("listBankAccounts", {"function"}),
                            ("AppDbContext", {"class"}), ("MapAuthEndpoints", {"function", "method"}),
                            ("MissingFields", {"function", "method"}), ("AuthEndpointsTests", {"class"}),
                            ("FieldErrors", {"type"})]:
            _, f, _ = r.cli(f"find-{name}", ["search", name, "--limit", "50"], proj)
            coords[name] = _coord(_rows(f), name, None, "semantic") or _coord(_rows(f), name)
        (out / "coords.json").write_text(json.dumps(coords, indent=1))
        for name, coord in coords.items():
            if not coord:
                continue
            r.cli(f"references-{name}", ["references", coord], proj)
            r.cli(f"graph-{name}", ["graph", coord], proj)
            r.cli(f"source-{name}", ["source", coord], proj)
            r.cli(f"read-{name}", ["read", coord], proj)
        # re-index after an edit
        (proj / "frontend/lib/api.ts").write_text(
            (proj / "frontend/lib/api.ts").read_text() + "\nexport const added = 1;\n")
        code2, obj2, text2 = r.cli("add-again", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        r.cli("search-added", ["search", "added"], proj)
        r.cli("profile", ["package-profile", str(proj)], proj)
        return {"observation": {"pass": True, "detail": f"add exit={code} re-add exit={code2}"}}
    finally:
        r.stop()


def startup_path(bin_dir, out):
    """STARTUP-01/02, STATUS-01: a regular file on PATH must not stop the daemon or any tool.

    The CLI starts the owner itself (no pre-started daemon), so this is the cold path the
    report hit. The PATH keeps a working python3 first and appends a file-valued entry,
    which is what WSL does with a Windows PATH entry that names an executable.
    """
    root = pathlib.Path(tempfile.mkdtemp(prefix="ndx-startup-"))
    win = root / "win"
    win.mkdir()
    (win / "dotnet.exe").write_bytes(b"MZ not a directory")
    path = os.environ.get("PATH", "/usr/bin:/bin") + os.pathsep + str(win / "dotnet.exe")
    r = Route(bin_dir, out, auto_start=True, path_extra=path)
    results = {}
    try:
        proj = _project(r, "tiny")
        # Exit 75 is the documented "startup is pending, nothing was submitted, retry the same
        # command"; a loaded host can need several tries. Only that code is retried.
        attempts = []
        for attempt in range(12):
            code, obj, text = r.cli(f"cold-health-{attempt}", ["health"], proj, timeout=300)
            attempts.append(code)
            if code != 75:
                break
            time.sleep(10)
        results["STARTUP-01"] = {"pass": code == 0, "detail": f"cold health exits={attempts}; tail={text[-300:]!r}"}
        for attempt in range(12):
            code, obj, text = r.cli(f"status-{attempt}", ["status"], proj)
            if code != 75:
                break
            time.sleep(10)
        status_ok = code == 0
        code2, obj2, text2 = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        code3, obj3, text3 = r.cli("search", ["search", "greet"], proj)
        results["STARTUP-01"]["pass"] = results["STARTUP-01"]["pass"] and status_ok and code2 == 0 and code3 == 0
        results["STARTUP-01"]["detail"] += f"; status exit={code}; add exit={code2}; search exit={code3}"
        r.start_mcp(proj)
        mcp = r.tool("backend.status", {})
        results["STARTUP-01"]["mcp_status_is_error"] = bool(mcp.get("isError"))
        results["STARTUP-01"]["pass"] = results["STARTUP-01"]["pass"] and not mcp.get("isError")
        return results
    finally:
        r.stop()
        import shutil
        shutil.rmtree(root, ignore_errors=True)


def _consumers(refs):
    """path -> number of reference rows, from the `path [bytesA..B)` tags."""
    import re
    seen = {}
    for row in _rows(refs):
        for tag in row.get("tags", []):
            m = re.match(r"(.+?) \[bytes\d+\.\.\d+\)", tag)
            if m:
                seen[m.group(1)] = seen.get(m.group(1), 0) + 1
    return seen


def probe_refs(bin_dir, out, kind="refs"):
    """TSX-01 matrix: which cross-file uses does `references` find, by import style."""
    r = Route(bin_dir, out)
    summary = {}
    try:
        proj = _project(r, kind)
        r.start_owner(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        for name in ["relativeTarget", "aliasTarget", "barrelTarget", "SharedShape", "Widget", "Page"]:
            _, found, _ = r.cli(f"find-{name}", ["search", name, "--limit", "50"], proj)
            coord = _coord(_rows(found), name, None, "semantic") or _coord(_rows(found), name)
            if not coord:
                summary[name] = "no declaration"
                continue
            c, refs, text = r.cli(f"references-{name}", ["references", coord], proj)
            summary[name] = {"consumers": _consumers(refs), "note": (refs or {}).get("note", "")[:160], "exit": c}
        (out / "summary.json").write_text(json.dumps(summary, indent=1))
        return {"observation": {"pass": True, "detail": json.dumps(summary)}}
    finally:
        r.stop()


def probe_edits(bin_dir, out):
    """INDEX-04/05, SOURCE-04: what the product says once files move on disk after indexing."""
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "budget")
        r.start_owner(proj)
        r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        _, found, _ = r.cli("find-Button", ["search", "Button", "--limit", "20"], proj)
        coord = _coord(_rows(found), "Button", {"function"}, "declaration")
        _, found, _ = r.cli("find-api", ["search", "listBankAccounts", "--limit", "20"], proj)
        api = _coord(_rows(found), "listBankAccounts", {"function"}, "declaration")
        # edits on disk, no re-index
        api_path = proj / "frontend/lib/api.ts"
        api_path.write_text(api_path.read_text().replace("/api/bank-accounts", "/api/accounts") + "\nexport function brandNewHelper() { return 1; }\n")
        btn = proj / "frontend/components/Button.tsx"
        btn.write_text("// two\n// inserted lines\n" + btn.read_text())
        r.cli("search-new-symbol", ["search", "brandNewHelper"], proj)
        r.cli("source-edited-api", ["source", api], proj)
        r.cli("read-edited-api", ["read", api], proj)
        r.cli("source-moved-button", ["source", coord], proj)
        short = "components/Button.tsx:3::Button"
        r.cli("source-moved-short", ["source", short], proj)
        r.cli("show-moved-short", ["show", short], proj)
        r.cli("references-moved-short", ["references", short], proj)
        # re-index, then use the pre-edit short coordinate and compare the status source tag
        r.cli("status-before-reindex", ["--format", "markdown", "status"], proj, json_out=False)
        r.cli("re-add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        r.cli("status-after-reindex", ["--format", "markdown", "status"], proj, json_out=False)
        r.cli("source-old-line-after-reindex", ["source", short], proj)
        r.cli("source-old-line-fullpath", ["source", "frontend/components/Button.tsx:3::Button"], proj)
        _, found, _ = r.cli("find-long", ["search", "LongRegistry", "--limit", "5"], proj)
        long_coord = _coord(_rows(found), "LongRegistry", {"class"}, "declaration")
        if long_coord:
            r.cli("source-long", ["source", long_coord], proj)
            r.cli("source-long-bytes", ["source", long_coord, "--byte-range"], proj)
            r.cli("source-long-full", ["--detail", "full", "source", long_coord], proj)
        return {"observation": {"pass": True, "detail": "see receipts"}}
    finally:
        r.stop()


def probe_churn(bin_dir, out):
    """INDEX-01: build output rewritten while an index runs."""
    import threading
    r = Route(bin_dir, out)
    stop = threading.Event()
    try:
        proj = _project(r, "budget")
        for d in ("backend/Budget.Api/bin/Debug/net10.0", "backend/Budget.Api/obj/Debug", "frontend/.next/cache"):
            (proj / d).mkdir(parents=True, exist_ok=True)

        def churn():
            n = 0
            while not stop.is_set():
                n += 1
                for d in ("backend/Budget.Api/bin/Debug/net10.0", "backend/Budget.Api/obj/Debug", "frontend/.next/cache"):
                    (proj / d / f"out{n % 5}.bin").write_bytes(os.urandom(64))
                    (proj / d / f"fresh{n}.tmp").write_bytes(b"x")
                    sub = proj / d / f"dir{n % 3}"
                    sub.mkdir(exist_ok=True)
                    (sub / f"f{n}.txt").write_text(str(n))
                time.sleep(0.05)

        t = threading.Thread(target=churn, daemon=True)
        r.start_owner(proj)
        t.start()
        code, obj, text = r.cli("add-under-churn", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        stop.set()
        t.join(timeout=5)
        rejected = "changed before product selection" in text or "rejected" in text.lower()
        return {"INDEX-01": {"pass": code in (0,) or ("partially" in text and not rejected),
                             "detail": f"exit={code} rejected={rejected}; tail={text[-400:]!r}"}}
    finally:
        stop.set()
        r.stop()


def probe_mcp(bin_dir, out):
    """INDEX-03 and payload sizes through MCP."""
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "budget")
        r.start_owner(proj)
        r.start_mcp(proj)
        res = r.mcp_index(proj)
        (out / "mcp-index-final.json").write_text(json.dumps(res, indent=1))
        sizes = {"index_final_isError": bool(res.get("isError")), "poll_sizes": getattr(r, "poll_sizes", [])}
        found = r.tool("backend.search", {"query": "Button", "limit": 1})
        sizes["search_limit1_bytes"] = len(json.dumps(found))
        sizes["search_isError"] = bool(found.get("isError"))
        st = r.tool("backend.status", {})
        sizes["status_bytes"] = len(json.dumps(st))
        pk = r.tool("backend.packages", {})
        sizes["packages"] = json.dumps(pk.get("structuredContent"))[:400]
        return {"observation": {"pass": True, "detail": json.dumps(sizes)}}
    finally:
        r.stop()


def probe_react_variants(bin_dir, out):
    """TSX-02: which `children: ReactNode` shapes still lose their file to a source-scope gap."""
    import re
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "react_variants")
        r.start_owner(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        log = (out / "owner-stderr.log").read_text(errors="replace") if (out / "owner-stderr.log").exists() else ""
        gaps = sorted(set(re.findall(r"scope gap: path=(\S+) .*?cause=(\w+)", log)))
        failing = sorted({p for p, _ in gaps})
        expected = sorted(f"src/{name}.tsx" for name in fixtures.REACT_VARIANTS)
        r.cli("profile", ["package-profile", str(proj)], proj)
        detail = {"failing_files": failing, "all_files": expected, "add_exit": code}
        return {"TSX-02": {"pass": not failing, "detail": json.dumps(detail)}}
    finally:
        r.stop()


def probe_refs_nested(bin_dir, out):
    return probe_refs(bin_dir, out, "refs_nested")


def probe_refs_mixed(bin_dir, out):
    return probe_refs(bin_dir, out, "refs_mixed")


def _budget_refs(bin_dir, out, kind):
    r = Route(bin_dir, out)
    summary = {}
    try:
        proj = _project(r, kind)
        r.start_owner(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        for name in ["Button", "listBankAccounts", "ApiError", "AuthCard"]:
            _, found, _ = r.cli(f"find-{name}", ["search", name, "--limit", "50"], proj)
            coord = _coord(_rows(found), name, None, "declaration") or _coord(_rows(found), name)
            if not coord:
                summary[name] = "no declaration"
                continue
            c, refs, _t = r.cli(f"references-{name}", ["references", coord], proj)
            summary[name] = {"consumers": _consumers(refs), "exit": c}
        (out / "summary.json").write_text(json.dumps(summary, indent=1))
        return {"observation": {"pass": True, "detail": json.dumps(summary)}}
    finally:
        r.stop()


def probe_budget_no_csharp(bin_dir, out):
    return _budget_refs(bin_dir, out, "budget_no_csharp")


def probe_budget_no_appshell(bin_dir, out):
    return _budget_refs(bin_dir, out, "budget_no_appshell")


def probe_budget_no_next(bin_dir, out):
    return _budget_refs(bin_dir, out, "budget_no_next")


def probe_refs_gap(bin_dir, out):
    return probe_refs(bin_dir, out, "refs_gap")


def probe_reindex_refusal(bin_dir, out):
    """INDEX-06/08: re-index a project whose refused file was edited; what is served afterwards."""
    r = Route(bin_dir, out)
    results = {}
    try:
        proj = _project(r, "react_variants")
        r.start_owner(proj)
        code0, _, _ = r.cli("add-1", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        target = proj / "src/v10_arrow_component.tsx"
        rejected_any = False
        for n in (2, 3):
            target.write_text(target.read_text() + f"\nexport const extra{n} = {n};\n")
            code, obj, text = r.cli(f"add-{n}", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
            rejected = "does not match captured source" in text or "rejected" in text.lower()
            rejected_any |= rejected
            _, found, _ = r.cli(f"search-extra{n}", ["search", f"extra{n}"], proj)
            results[f"add-{n}"] = {"exit": code, "rejected": rejected, "extra_found": bool(_rows(found))}
        results["INDEX-06"] = {"pass": not rejected_any, "detail": json.dumps(results)}
        return results
    finally:
        r.stop()


def probe_budget_diag(bin_dir, out):
    """TSX-02: owner diagnostics for the files whose children type still cannot be projected."""
    r = Route(bin_dir, out, extra_env={"NUDOX_TYPESCRIPT_TYPE_PROJECTION_DIAGNOSTICS": "1"})
    try:
        proj = _project(r, "budget")
        r.start_owner(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        r.cli("profile", ["package-profile", str(proj)], proj)
        return {"observation": {"pass": True, "detail": f"add exit={code}"}}
    finally:
        r.stop()


def probe_crm(bin_dir, out):
    """INVENTORY-01/02, PACKAGE-*, STATUS: the CRM scaffold (FastAPI + Vue) through every public read."""
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "crm")
        r.start_owner(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        for label, args in [
            ("search-onmounted", ["search", "onMounted"]),
            ("search-health", ["search", "health"]),
            ("search-loadhealth", ["search", "loadHealth"]),
            ("outline", ["outline"]),
            ("status", ["status"]),
            ("packages", ["packages"]),
            ("explore-fastapi", ["explore", "fastapi"]),
            ("index-search-fastapi", ["index-search", "fastapi"]),
            ("index-search-purl", ["index-search", "pkg:pypi/fastapi"]),
            ("package-fastapi", ["package", "pkg:pypi/fastapi@0.162.0"]),
            ("dependencies-fastapi", ["dependencies", "pkg:pypi/fastapi@0.162.0"]),
            ("versions-vite", ["package-versions", "pkg:npm/vite@6.0.0"]),
            ("package-vue-unversioned", ["package", "pkg:npm/vue"]),
            ("advisory-fastapi", ["advisory", "pkg:pypi/fastapi@0.162.0"]),
            ("timing-note", ["health"]),
        ]:
            r.cli(label, args, proj)
        r.cli("status-md", ["--format", "markdown", "status"], proj, json_out=False)
        r.cli("search-onmounted-md", ["--format", "markdown", "search", "onMounted"], proj, json_out=False)
        return {"observation": {"pass": True, "detail": f"add exit={code}"}}
    finally:
        r.stop()


def startup_missing_checker(bin_dir, out):
    """STARTUP-02/STATUS-01: an explicitly configured optional checker that does not exist."""
    r = Route(bin_dir, out, auto_start=True, extra_env={"NUDOX_PYTHON": "/nonexistent/python3"})
    try:
        proj = _project(r, "tiny")
        code, obj, text = r.cli("cold-health", ["health"], proj, timeout=300)
        code2, obj2, text2 = r.cli("status", ["status"], proj, timeout=300)
        code3, obj3, text3 = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        ok = code == 0 and code2 == 0 and code3 == 0
        return {"STARTUP-02": {"pass": ok, "detail": f"health={code} status={code2} add={code3}; {text[-300:]!r}"}}
    finally:
        r.stop()


def probe_refusals(bin_dir, out):
    """INDEX-06/07/08: small TypeScript projects that make the compiler lane refuse in different ways."""
    import re
    r = Route(bin_dir, out)
    results = {}
    try:
        base = r.root / "refusals"
        base.mkdir()
        variants = {
            "bad_tsconfig_json": lambda d: (fixtures.write(d, "tsconfig.json", '{"compilerOptions": {"strict": true,,}\n'),),
            "tsconfig_extends_missing": lambda d: (fixtures.write(d, "tsconfig.json", '{"extends":"./nope.json"}\n'),),
            "non_utf8_source": lambda d: (fixtures.write(d, "src/bad.ts", "x"), (d / "src/bad.ts").write_bytes(b"export const a = '\xff\xfe';\n")),
            "oversized_source": lambda d: (fixtures.write(d, "src/big.ts", "export const big = '" + "x" * (3 * 1024 * 1024) + "';\n"),),
            "syntax_error": lambda d: (fixtures.write(d, "src/broken.ts", "export function ( { return ;\n"),),
        }
        r.start_owner(base)
        for name, mutate in variants.items():
            d = base / name
            fixtures.tiny_ts_project(d)
            mutate(d)
            code, obj, text = r.cli(f"add-{name}", ["add", str(d), "--wait", "--wait-timeout", "900"], d)
            (d / "src/definition.ts").write_text((d / "src/definition.ts").read_text() + "\nexport const edited = 1;\n")
            code2, obj2, text2 = r.cli(f"re-add-{name}", ["add", str(d), "--wait", "--wait-timeout", "900"], d)
            c3, found, _ = r.cli(f"search-{name}", ["search", "edited"], d)
            results[name] = {
                "add": code, "re_add": code2,
                "identity_mismatch": "source identity does not match" in (text + text2),
                "state_add": re.findall(r"(partially published|published|failed|rejected)", text2)[-1:] ,
                "edited_found": bool(_rows(found)),
            }
        mismatch = [k for k, v in results.items() if v["identity_mismatch"]]
        (out / "summary.json").write_text(json.dumps(results, indent=1))
        return {"INDEX-06": {"pass": not mismatch, "detail": json.dumps(results)}}
    finally:
        r.stop()


def _lane(obj, name):
    for entry in (obj or {}).get("coverage", []):
        if entry.get("lane") == name:
            return entry.get("state")
    return None


def verify_budget(bin_dir, out):
    """Pass/fail checks for the budget-report findings, through the CLI and MCP of real processes."""
    r = Route(bin_dir, out)
    res = {}

    def put(key, ok, detail):
        res[key] = {"pass": bool(ok), "detail": str(detail)[:600]}

    try:
        proj = _project(r, "budget")
        r.start_owner(proj)
        r.start_mcp(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)

        def find(name, kinds=None, shape="declaration"):
            _, f, _ = r.cli(f"find-{name}", ["search", name, "--limit", "50"], proj)
            return _coord(_rows(f), name, kinds, shape) or _coord(_rows(f), name), f

        # SOURCE-03 / SOURCE-02 / SOURCE-01: modifiers, leading comment, honest extent, paging
        button, _ = find("Button", {"function"})
        c, src, _t = r.cli("source-Button", ["source", button], proj)
        lines = (src or {}).get("source", {}).get("lines", [])
        put("SOURCE-03", lines and lines[0].lstrip().startswith("export function Button"),
            f"exit={c} first line={lines[:1]} extent={(src or {}).get('source', {}).get('extent')}")
        long_coord, _ = find("LongRegistry", {"class"})
        if not long_coord:
            put("SOURCE-02", False, "LongRegistry not found by search")
            put("SOURCE-01", False, "LongRegistry not found by search")
        else:
            c, src, _t = r.cli("source-long", ["source", long_coord], proj)
            lines = (src or {}).get("source", {}).get("lines", [])
            put("SOURCE-02", lines and lines[0].startswith("/** Registry of numbered operations")
                and any(l.startswith("export class LongRegistry") for l in lines[:3]),
                f"exit={c} head={lines[:2]}")
            extent = (src or {}).get("source", {}).get("extent")
            hint = (src or {}).get("source", {}).get("continue_with", "")
            c2, page, _t = r.cli("source-long-bytes", ["source", long_coord, "--byte-range"], proj)
            text, guard = "", 0
            while page and guard < 80:
                guard += 1
                rng = page.get("declaration_source_range") or {}
                text += (rng.get("source") or {}).get("text", "")
                cursor = rng.get("nextCursor")
                if not cursor:
                    break
                c2, page, _t = r.cli(f"source-long-bytes-{guard}", ["source", long_coord, "--byte-range", "--cursor", cursor], proj)
            full = (proj / "frontend/lib/longModule.ts").read_text()
            body = full[full.index("class LongRegistry"):]
            put("SOURCE-01", extent == "truncated" and "--byte-range" in hint and body.strip() == text.strip(),
                f"default extent={extent}; hint={hint[:90]!r}; paged bytes={len(text)} of {len(body)}; pages={guard}")

        # SEARCH-02 / SEARCH-03 / SEARCH-06
        _, rf, _ = r.cli("resolve-FieldErrors", ["resolve", "FieldErrors"], proj)
        names = [(x["identity"]["name"], x.get("kind")) for x in _rows(rf)]
        put("SEARCH-02", names and names[0] == ("FieldErrors", "type"), names)
        _, rr, _ = r.cli("resolve-request", ["resolve", "request"], proj)
        shapes = [x["identity"]["shape"] for x in _rows(rr)]
        put("SEARCH-03", "external" not in shapes[:1] and shapes[:1] == ["declaration"],
            [(x["identity"]["name"], x.get("kind"), x["identity"]["shape"]) for x in _rows(rr)])
        _, sb, _ = r.cli("search-bankaccount", ["search", "BankAccount", "--limit", "20"], proj)
        put("SEARCH-06", any(x["identity"]["name"] == "AddBankAccounts" for x in _rows(sb)),
            [x["identity"]["name"] for x in _rows(sb)])

        # STATUS-01 / STATUS-03 / REFERENCES-01 / GRAPH-01
        c, st, text = r.cli("status", ["status"], proj)
        summary = ((st or {}).get("capabilities") or {}).get("summary", "")
        put("STATUS-01", "optional compiler-backed checkers" in summary, summary)
        scope = ((st or {}).get("source_selection_scope") or {}).get("extensions", [])
        put("STATUS-03", any(e.get("extension") == "vue" and e.get("excluded_files") == 0 and e.get("projects_observed") == 1 for e in scope),
            scope)
        c, refs, _t = r.cli("references-Button", ["references", button], proj)
        note = (refs or {}).get("note", "")
        put("REFERENCES-01", note.startswith("Reference coverage: partial") or note.startswith("Reference coverage: unknown"), note[:200])
        titles = [row.get("title", "") for row in _rows(refs)]
        put("REFERENCES-03", all("::semantic::" not in t for t in titles), titles)
        c, graph, _t = r.cli("graph-Button", ["graph", button], proj)
        put("GRAPH-01", _lane(graph, "graph") != "complete", (graph or {}).get("coverage"))

        # TSX-01: every cross-file use through the @/ alias, JSX and re-exports must be listed
        expectations = {
            "Button": {"frontend/components/AppShell.tsx", "frontend/components/AuthForm.tsx", "frontend/components/BankAccounts.tsx"},
            "listBankAccounts": {"frontend/components/BankAccounts.tsx"},
            "ApiError": {"frontend/components/AuthForm.tsx", "frontend/components/BankAccounts.tsx"},
            "AppShell": {"frontend/components/BankAccounts.tsx"},
            "AuthCard": {"frontend/components/AuthForm.tsx"},
        }
        misses = {}
        for name, want in expectations.items():
            coord, _ = find(name, {"function", "class"})
            c, refs, _t = r.cli(f"references-{name}", ["references", coord], proj) if coord else (None, None, "")
            got = set(_consumers(refs))
            if not want <= got:
                misses[name] = sorted(want - got)
        put("TSX-01", not misses, f"missing consumers: {misses}")

        # MCP: the same questions
        m = r.tool("backend.search", {"query": "BankAccount", "limit": 20})
        mnames = [x["identity"]["name"] for x in m.get("structuredContent", {}).get("records", [])]
        put("SEARCH-06-mcp", "AddBankAccounts" in mnames, mnames)
        ms = r.tool("backend.source", {"coordinate": button})
        mlines = ms.get("structuredContent", {}).get("source", {}).get("lines", [])
        put("SOURCE-03-mcp", mlines and mlines[0].lstrip().startswith("export function Button"), mlines[:1])
        listed = r.rpc("tools/list", {})
        search_tool = next((t for t in listed.get("tools", []) if t.get("name") == "backend.search"), {})
        desc = search_tool.get("description", "")
        put("SEARCH-04-mcp", "semantic_search status" in desc and "distinctive name" in desc, desc[:300])
        put("CAPABILITY-01-mcp", "published_names_only" in desc, desc[:300])
        _, multi, _ = r.cli("search-multiword", ["search", "where is the balance validated"], proj)
        put("SEARCH-04", "several words" in (multi or {}).get("empty_reason", "") and (multi or {}).get("semantic_search", {}).get("status") == "unavailable",
            (multi or {}).get("empty_reason", "")[:300])
        mst = r.tool("backend.status", {})
        put("STATUS-01-mcp", "optional compiler-backed checkers" in json.dumps(mst.get("structuredContent", {})), "")

        # INDEX-05 / INDEX-04 / SOURCE-04: edit on disk, then re-index
        api = proj / "frontend/lib/api.ts"
        api.write_text(api.read_text() + "\nexport function brandNewHelper() { return 1; }\n")
        _, empty, _ = r.cli("search-new-symbol", ["search", "brandNewHelper"], proj)
        put("INDEX-05", "not searchable until it is re-indexed" in (empty or {}).get("empty_reason", ""),
            (empty or {}).get("empty_reason", "")[:300])
        lb, _ = find("listBankAccounts", {"function"})
        c, stale, _t = r.cli("source-stale", ["source", lb], proj)
        put("SOURCE-04", c == 2 and "changed on disk" in json.dumps(stale), json.dumps(stale)[:300])
        btn = proj / "frontend/components/Button.tsx"
        btn.write_text("// one\n// two\n" + btn.read_text())
        r.cli("re-add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        c, moved, _t = r.cli("source-moved-short", ["source", "components/Button.tsx:3::Button"], proj)
        put("INDEX-04", c == 0 and "Button" in json.dumps(moved), f"exit={c} {json.dumps(moved)[:200]}")
        c, found, _t = r.cli("search-new-after-reindex", ["search", "brandNewHelper"], proj)
        res["INDEX-05"]["detail"] += f" | after re-index found={bool(_rows(found))}"
        return res
    finally:
        r.stop()


def gate_on_signed_out(bin_dir, out):
    """Which routes still answer when the account gate is ON and nobody is signed in.

    This is not an authenticated run; it records which findings can only be exercised with an
    account. NUDOX_ACCOUNT_GATE=on only makes the build stricter, it bypasses nothing.
    """
    r = Route(bin_dir, out, auto_start=True, extra_env={"NUDOX_ACCOUNT_GATE": "on"})
    seen = {}
    try:
        proj = _project(r, "tiny")
        for label, args in [("health", ["health"]), ("status", ["status"]), ("packages", ["packages"]),
                            ("add", ["add", str(proj), "--wait", "--wait-timeout", "300"]),
                            ("search", ["search", "greet"]), ("outline", ["outline"])]:
            code, obj, text = r.cli(label, args, proj, timeout=600)
            seen[label] = {"exit": code, "slug": (obj or {}).get("slug") if isinstance(obj, dict) else None,
                           "operand": (obj or {}).get("operand") if isinstance(obj, dict) else None}
        (out / "summary.json").write_text(json.dumps(seen, indent=1))
        return {"observation": {"pass": True, "detail": json.dumps(seen)}}
    finally:
        r.stop()


SCENARIOS = {"gate_on_signed_out": gate_on_signed_out, "verify_budget": verify_budget, "probe_budget_no_csharp": probe_budget_no_csharp, "probe_budget_no_appshell": probe_budget_no_appshell, "probe_budget_no_next": probe_budget_no_next, "probe_refusals": probe_refusals, "probe_refs_mixed": probe_refs_mixed, "startup_missing_checker": startup_missing_checker, "probe_crm": probe_crm, "probe_budget_diag": probe_budget_diag, "probe_reindex_refusal": probe_reindex_refusal, "probe_refs_nested": probe_refs_nested, "probe_refs_gap": probe_refs_gap, "probe_react_variants": probe_react_variants, "probe_budget": probe_budget, "startup_path": startup_path, "probe_refs": probe_refs,
             "probe_edits": probe_edits, "probe_churn": probe_churn, "probe_mcp": probe_mcp}
