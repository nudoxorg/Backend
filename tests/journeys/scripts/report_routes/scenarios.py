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


def _has(text, *needles):
    return all(needle in text for needle in needles)


def probe_packages(bin_dir, out):
    """The full-stack scaffold's package questions against a loopback registry fixture.

    The owner runs online against registry_fixture.Registry only (127.0.0.1); nothing reaches a
    real registry. The fixture serves fastapi and vite metadata, no archives."""
    import registry_fixture
    registry = registry_fixture.Registry()
    r = Route(bin_dir, out)
    results = {}
    try:
        proj = _project(r, "crm")
        r.start_owner(proj, registry=registry)
        r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        # The same name explore leaves empty is found by index-search; explore says why.
        code, explore, text = r.cli("explore-fastapi", ["explore", "fastapi"], proj)
        note = (explore or {}).get("note", "")
        results["explore_scope"] = {"pass": code == 0 and not _rows(explore) and _has(note, "index-search fastapi", "does not mean no such package"),
                                 "detail": note}
        # An online lookup returns the registry's observation, not "offline".
        code, pkg, text = r.cli("package-fastapi", ["package", "pkg:pypi/fastapi@0.162.0"], proj)
        results["package_metadata_online"] = {"pass": code == 0 and "offline" not in text and "discovered" in text,
                                 "detail": text[:600]}
        # The discovered releases of a package the project uses.
        code, versions, text = r.cli("versions-fastapi", ["package-versions", "pkg:pypi/fastapi@0.162.0"], proj)
        results["package_versions_discovered"] = {"pass": code == 0 and _has(text, "0.160.0", "0.161.0", "0.162.0", "None is acquired"),
                                 "detail": text[:900]}
        code, vite, text_vite = r.cli("versions-vite", ["package-versions", "pkg:npm/vite@6.0.0"], proj)
        results["package_versions_discovered"]["pass"] = results["package_versions_discovered"]["pass"] and code == 0 and _has(text_vite, "5.4.0", "6.0.0")
        # Dependencies before acquisition names the acquisition.
        code, deps, text = r.cli("dependencies-fastapi", ["dependencies", "pkg:pypi/fastapi@0.162.0"], proj)
        fault = (deps or {}).get("fault", {})
        results["dependencies_name_acquisition"] = {"pass": fault.get("shell") == "nudox add pkg:pypi/fastapi@0.162.0"
                                 and fault.get("call", {}).get("name") == "backend.index",
                                 "detail": fault}
        # An unversioned locator names the discovery route.
        code, vue, text = r.cli("package-vue-unversioned", ["package", "pkg:npm/vue"], proj)
        results["unversioned_locator_route"] = {"pass": code != 0 and _has(text, "exact version", "index-search"), "detail": text[:400]}
        # The same answers over MCP, including the acquisition route's tool.
        r.start_mcp(proj)
        explore_mcp = r.tool("backend.explore", {"query": "fastapi"})
        versions_mcp = r.tool("backend.package_versions", {"package": "pkg:pypi/fastapi@0.162.0"})
        deps_mcp = r.tool("backend.dependencies", {"package": "pkg:pypi/fastapi@0.162.0"}, allow_error=True)
        call = (deps_mcp.get("structuredContent", {}).get("fault", {}) or {}).get("call", {})
        results["mcp_acquisition_route"] = {"pass": call.get("name") == "backend.index"
                                 and call.get("arguments", {}).get("path") == "pkg:pypi/fastapi@0.162.0"
                                 and "pkg:pypi/fastapi@0.162.0" in json.dumps(versions_mcp)
                                 and "index-search fastapi" in json.dumps(explore_mcp),
                                 "detail": {"dependencies_call": call}}
        # The owner was configured only with the fixture's sources, so the fixture must have
        # seen requests and every one of them must have come from the loopback interface.
        results["registry_loopback_only"] = {"pass": bool(registry.requests) and bool(registry.clients)
                                             and all(c in ("127.0.0.1", "::1") for c in registry.clients),
                                             "detail": {"requests": registry.requests, "clients": sorted(set(registry.clients))}}
        return results
    finally:
        r.stop()
        registry.stop()


def probe_vue(bin_dir, out):
    """The full-stack scaffold's App.vue script block through search, outline, source and status."""
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "crm")
        r.start_owner(proj)
        code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        host = (proj / "frontend/src/App.vue").read_text()
        _, found, text = r.cli("search-loadhealth", ["search", "loadHealth"], proj)
        coord = _coord(_rows(found), "loadHealth")
        row = next((x for x in _rows(found) if x.get("identity", {}).get("name") == "loadHealth"), {})
        line = host[:host.index("async function loadHealth")].count("\n") + 1
        _, onm, onm_text = r.cli("search-onmounted", ["search", "onMounted"], proj)
        _, status, status_text = r.cli("status", ["status"], proj)
        source_ok = False
        src_text = ""
        if coord:
            _, src, src_text = r.cli("source-loadhealth", ["source", coord], proj)
            source_ok = "async function loadHealth()" in src_text and "status.value = (await response.json()).status" in src_text
        outline_code, _, outline_text = r.cli("outline", ["outline", "--limit", "100"], proj)
        vue_excluded = [e for e in (status or {}).get("source_selection_scope", {}).get("extensions", []) if e.get("extension") == "vue"]
        path_ok = row.get("identity", {}).get("path") == "frontend/src/App.vue" and row.get("identity", {}).get("line") == line
        return {"vue_script_block_inventory": {"pass": bool(coord) and path_ok and source_ok and "App.vue" in outline_text
                                 and not any(e.get("excluded_files") for e in vue_excluded),
                                 "detail": {"coordinate": coord, "row_path": row.get("identity", {}).get("path"),
                                            "row_line": row.get("identity", {}).get("line"), "expected_line": line,
                                            "source_ok": source_ok, "onMounted_found": "onMounted" in onm_text and bool(_rows(onm)),
                                            "vue_status": vue_excluded}}}
    finally:
        r.stop()


def startup_missing_checker(bin_dir, out):
    """An explicitly configured optional checker that does not exist."""
    r = Route(bin_dir, out, auto_start=True, extra_env={"NUDOX_PYTHON": "/nonexistent/python3"})
    try:
        proj = _project(r, "tiny")
        code, obj, text = r.cli("cold-health", ["health"], proj, timeout=300)
        code2, obj2, text2 = r.cli("status", ["status"], proj, timeout=300)
        code3, obj3, text3 = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        ok = code == 0 and code2 == 0 and code3 == 0
        # A Python project meets the unusable interpreter on its own: its owner stays up and
        # the outcome is reported, not a daemon that never started.
        crm = _project(r, "crm")
        code4, obj4, text4 = r.cli("add-python", ["add", str(crm), "--wait", "--wait-timeout", "900"], crm)
        code5, obj5, text5 = r.cli("health-after", ["health"], proj, timeout=300)
        return {"missing_checker_startup": {"pass": ok and code5 == 0,
                               "detail": f"health={code} status={code2} add={code3}; python project add={code4} "
                                         f"health afterwards={code5}; {text[-200:]!r}; add-python tail={text4[-300:]!r}"}}
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


def _poll_kind(result):
    body = result.get("structuredContent", {})
    return (body.get("surface", {}) or {}).get("result")


def query_output(bin_dir, out):
    """Query and output behaviour, through real CLI and MCP processes."""
    checks = {}

    def ok(finding, condition, detail):
        entry = checks.setdefault(finding, {"pass": True, "detail": []})
        entry["pass"] = entry["pass"] and bool(condition)
        entry["detail"].append(("ok: " if condition else "FAIL: ") + str(detail))

    r = Route(bin_dir, out)
    try:
        proj = _project(r, "budget")
        r.start_owner(proj)
        r.start_mcp(proj)
        # Index through MCP and classify every poll by size and kind.
        result = r.tool("backend.index", {"path": str(proj)})
        polls = [("index", len(json.dumps(result)))]
        deadline = time.monotonic() + 1500
        while True:
            call = result.get("structuredContent", {}).get("index_poll", {}).get("call")
            if not call:
                break
            if time.monotonic() > deadline:
                raise RuntimeError("MCP index timed out")
            time.sleep(0.5)
            result = r.rpc("tools/call", call)
            polls.append((_poll_kind(result) or "terminal", len(json.dumps(result))))
        (out / "poll-sizes.json").write_text(json.dumps(polls))
        unchanged = [size for kind, size in polls if kind == "index-progress-unchanged"]
        changed = [size for kind, size in polls[1:] if kind != "index-progress-unchanged"]
        ok("poll_size", unchanged and max(unchanged) < 1000, f"unchanged polls {unchanged} bytes")
        ok("poll_size", changed and max(changed) < 8000, f"changed polls {changed} bytes")
        ok("poll_size", not result.get("isError"), "final poll is not an error")
        r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)

        # packages names the languages and their file counts.
        _, pk, _ = r.cli("packages", ["packages"], proj)
        profiles = (((pk or {}).get("projects") or [{}])[0]).get("source_profiles") or []
        by = {p["profile"]: p for p in profiles}
        ok("shelf_languages", "csharp" in by and by["csharp"]["files"] > 0, f"source_profiles={profiles}")
        ok("shelf_languages", any(k in by for k in ("typescript", "tsx")), "a TypeScript profile is listed")
        _, md, text = r.cli("packages-text", ["--format", "markdown", "packages"], proj, json_out=False)
        ok("shelf_languages", "csharp" in text and "file(s)" in text, f"markdown packages: {text[:300]!r}")

        # The layout tag is named as such and the revision follows the edit.
        _, _, before = r.cli("status-before", ["--format", "markdown", "status"], proj, json_out=False)
        api = proj / "frontend/lib/api.ts"
        api.write_text(api.read_text() + "\nexport const rqEdited = 1;\n")
        r.cli("re-add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        _, _, after = r.cli("status-after", ["--format", "markdown", "status"], proj, json_out=False)
        import re
        rev = lambda t: (re.search(r"revision ([0-9a-f]{8})", t) or [None, None])[1]
        lay = lambda t: (re.search(r"store layout ([0-9a-f]{8})", t) or [None, None])[1]
        ok("status_layout", rev(before) and rev(after) and rev(before) != rev(after), f"revision {rev(before)} -> {rev(after)}")
        ok("status_layout", lay(before) and lay(before) == lay(after), f"store layout {lay(before)} -> {lay(after)}, constant")
        ok("status_layout", " source " not in after.splitlines()[0], "the status line no longer calls the layout tag `source`")

        # declarations_only on CLI and MCP.
        _, allrows, _ = r.cli("search-all", ["search", "Button", "--limit", "50"], proj)
        _, only, _ = r.cli("search-decl", ["search", "Button", "--limit", "50", "--declarations-only"], proj)
        kinds = lambda o: [row.get("kind") for row in _rows(o)]
        ok("declarations_only", "import" in kinds(allrows), f"unfiltered search returns imports: {kinds(allrows)}")
        ok("declarations_only", "import" not in kinds(only) and only.get("hidden_non_declarations", 0) > 0,
           f"filtered kinds={kinds(only)} hidden={only.get('hidden_non_declarations')}")
        ok("declarations_only", len(kinds(only)) + only.get("hidden_non_declarations", 0) == len(kinds(allrows)),
           "no row is lost: kept + hidden == all")
        mcp = r.tool("backend.search", {"query": "Button", "limit": 50, "declarations_only": True})
        sc = mcp.get("structuredContent", {})
        ok("declarations_only", not mcp.get("isError") and "import" not in [x.get("kind") for x in sc.get("records", [])]
           and sc.get("hidden_non_declarations", 0) > 0, f"MCP kinds={[x.get('kind') for x in sc.get('records', [])]}")

        # Outline kind filter on CLI and MCP.
        _, full, _ = r.cli("outline-all", ["outline", "--limit", "200"], proj)
        _, slim, _ = r.cli("outline-slim", ["outline", "--limit", "200", "--exclude-kind", "variable"], proj)

        def walk(nodes):
            for node in nodes:
                yield node
                yield from walk(node.get("children", []))
        ok("outline_kind_filter", any(n.get("kind") == "variable" for n in walk(full.get("roots", []))), "unfiltered outline lists variables (parameters/locals)")
        ok("outline_kind_filter", not any(n.get("kind") == "variable" for n in walk(slim.get("roots", []))) and slim.get("filtered_out", 0) > 0,
           f"filtered outline has no variables, filtered_out={slim.get('filtered_out')}")
        bad = r.cli("outline-badkind", ["outline", "--kind", "klass"], proj)
        ok("outline_kind_filter", bad[0] != 0 and "variable" in bad[2], "an unknown kind is refused with the closed list")
        m = r.tool("backend.outline", {"limit": 200, "exclude_kind": "variable"})
        msc = m.get("structuredContent", {})
        ok("outline_kind_filter", not m.get("isError") and not any(n.get("kind") == "variable" for n in walk(msc.get("roots", []))) and msc.get("filtered_out", 0) > 0,
           f"MCP outline filtered_out={msc.get('filtered_out')} isError={m.get('isError')}")

        # A limit-1 MCP search cursor is a short handle that resumes the next page.
        first = r.tool("backend.search", {"query": "Button", "limit": 1})
        fsc = first.get("structuredContent", {})
        cursor = fsc.get("nextCursor")
        ok("cursor_handle", cursor and len(cursor) < 120, f"nextCursor is {len(cursor or '')} bytes; result {len(json.dumps(first))} bytes")
        second = r.tool("backend.search", {"query": "Button", "limit": 1, "cursor": cursor})
        ssc = second.get("structuredContent", {})
        ok("cursor_handle", not second.get("isError") and ssc.get("records") and ssc["records"][0]["identity"]["coordinate"] != fsc["records"][0]["identity"]["coordinate"],
           "the handle resumes the following page")
        wrong = r.tool("backend.search", {"query": "AppShell", "limit": 1, "cursor": cursor}, allow_error=True)
        ok("cursor_handle", wrong.get("isError") or wrong.get("code"), f"the handle does not resume a different call: {json.dumps(wrong)[:200]}")
        # A restarted server no longer holds the handle and says so.
        r.mcp.stdin.close(); r.mcp.wait(timeout=10); r.pending = bytearray()
        r.start_mcp(proj)
        stale = r.tool("backend.search", {"query": "Button", "limit": 1, "cursor": cursor}, allow_error=True)
        text = json.dumps(stale)
        ok("cursor_handle", "short-lived" in text and "without a cursor" in text, f"expired handle is refused honestly: {text[:240]}")
        return checks
    finally:
        r.stop()


def probe_owner_retirement(bin_dir, out):
    """What does a client see when its change races the owner's idle retirement?

    The detached owner retires itself after its idle window and takes a while to drain. A change
    submitted inside that drain must either be served or be reported as not accepted; it must
    never be accepted by the still-open listener and then dropped. The window is shrunk to 1.5 s and the submit time is swept across the retirement.
    """
    import re
    results = []
    for offset_ms in range(1200, 2400, 100):
        o = out / f"offset-{offset_ms}"
        r = Route(bin_dir, o, owner_args=["--idle-timeout-ms", "1500"])
        try:
            proj = _project(r, "tiny")
            r.start_owner(proj)
            started = time.monotonic()
            wait = max(0.0, offset_ms / 1000 - (time.monotonic() - started))
            time.sleep(wait)
            code, obj, text = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "120"], proj, timeout=240)
            lost = bool(re.search(r"in flight|outcome is unknown|broken pipe|connection (was )?lost", text, re.I))
            results.append({"offset_ms": offset_ms, "exit": code, "lost_in_flight": lost,
                            "tail": text[-240:].replace("\n", " ")})
        finally:
            r.stop()
    (out / "sweep.json").write_text(json.dumps(results, indent=1))
    lost = [x for x in results if x["lost_in_flight"]]
    return {"owner_retirement": {"pass": not lost,
                         "detail": f"{len(results)} submits across the retirement window; {len(lost)} lost in flight: "
                                   f"{[x['offset_ms'] for x in lost]}"}}


def _cpu_seconds(pid):
    """utime+stime of a live process, from /proc: stable under host load, unlike wall time."""
    try:
        fields = pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    except (OSError, IndexError, ValueError):
        return None


def perf_timing(bin_dir, out):
    """How long does indexing take, in wall time and in CPU time, with the host load beside it."""
    rows = []
    for kind in ("tiny", "crm", "budget"):
        o = out / kind
        r = Route(bin_dir, o)
        try:
            proj = _project(r, kind)
            t = time.monotonic()
            r.start_owner(proj)
            startup = time.monotonic() - t
            load0 = pathlib.Path("/proc/loadavg").read_text().split()[:3]
            t = time.monotonic()
            code, _, _ = r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
            cold = time.monotonic() - t
            cpu_cold = _cpu_seconds(r.owner.pid)
            t = time.monotonic()
            code2, _, _ = r.cli("add-again", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
            warm = time.monotonic() - t
            cpu_warm = _cpu_seconds(r.owner.pid)
            load1 = pathlib.Path("/proc/loadavg").read_text().split()[:3]
            files = sum(1 for p in proj.rglob("*") if p.is_file() and "node_modules" not in p.parts)
            rows.append({"project": kind, "files": files, "owner_startup_s": round(startup, 2),
                         "first_add_wall_s": round(cold, 2), "first_add_exit": code,
                         "owner_cpu_s_after_first_add": cpu_cold,
                         "second_add_wall_s": round(warm, 2), "second_add_exit": code2,
                         "owner_cpu_s_after_second_add": cpu_warm,
                         "loadavg_before": load0, "loadavg_after": load1,
                         "cores": os.cpu_count()})
        finally:
            r.stop()
    (out / "timings.json").write_text(json.dumps(rows, indent=1))
    return {"index_timing": {"pass": all(x["first_add_exit"] == 0 for x in rows), "detail": json.dumps(rows)}}


def references_nested_calls(bin_dir, out):
    """Three calls in nested callables of one component must be three rows, not six."""
    import re
    r = Route(bin_dir, out)
    try:
        proj = _project(r, "refs_nested_calls")
        r.start_owner(proj)
        r.cli("add", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        _, found, _ = r.cli("find-aliasTarget", ["search", "aliasTarget", "--limit", "50"], proj)
        coord = _coord(_rows(found), "aliasTarget", None, "semantic") or _coord(_rows(found), "aliasTarget")
        if not coord:
            return {"nested_calls": {"pass": False, "detail": "aliasTarget not found"}}
        _, refs, text = r.cli("references-aliasTarget", ["references", coord], proj)
        sites = []
        for row in _rows(refs):
            tags = row.get("tags", [])
            rel = tags[0] if tags else ""
            for tag in tags:
                m = re.match(r"(.+?) \[bytes(\d+)\.\.(\d+)\)", tag)
                if m:
                    sites.append((m.group(1), m.group(2), m.group(3), rel))
        nested = [x for x in sites if x[0].endswith("nested.tsx")]
        calls = [x for x in nested if x[3] == "calls"]
        distinct = len(set((x[0], x[1], x[2]) for x in nested))
        ok = len(calls) == 3 and distinct == len(nested)
        return {"nested_calls": {"pass": ok, "detail": f"nested.tsx rows={len(nested)} calls={len(calls)} distinct sites={distinct}; all rows={sites}"}}
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


def _long_ts_project(root, files, tag):
    """A TypeScript project big enough that its index outlasts many status reads."""
    root = pathlib.Path(root)
    fixtures.write(root, "package.json", '{"name":"long-%s","version":"1.0.0","private":true}\n' % tag)
    fixtures.write(root, "tsconfig.json", '{"compilerOptions":{"strict":true,"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","noEmit":true},"include":["src"]}\n')
    for index in range(files):
        previous = f'import {{ step{index - 1} }} from "./m{index - 1}";\n' if index else ""
        chain = f"step{index - 1}(n) + " if index else ""
        body = "".join(f"export function helper{index}_{k}(a: number, b: string): string {{ return b + String(a * {k + 1}); }}\n" for k in range(12))
        fixtures.write(root, f"src/m{index}.ts",
                       f"{previous}export function step{index}(n: number): number {{ return {chain}n + {index}; }}\n{body}"
                       f"export interface Shape{index} {{ id: number; label: string; next?: Shape{max(index - 1, 0)}; }}\n")
    return root


FRAME_BAD = ("disconnect", "broken pipe", "connection reset", "endpoint closed", "mutation outcome", "unknown outcome")


def long_index_frame_budget(bin_dir, out):
    """Does a long index make the real CLI and MCP clients fail on the owner's frame budget?

    The owner is started with BACKEND_LOCALD_MAX_FRAMES_PER_CONNECTION lowered (only the owner sees it, so
    the clients still believe in the default 256 and must recover from the retirement after the fact).
    Set FRAME_BUDGET to 256 to leave the owner untouched and rely on the index outlasting 256 frames.
    """
    budget = os.environ.get("FRAME_BUDGET", "8")
    files = int(os.environ.get("LONG_FILES", "300"))
    owner_env = {} if budget == "256" else {"BACKEND_LOCALD_MAX_FRAMES_PER_CONNECTION": budget}
    r = Route(bin_dir, out, owner_env=owner_env)
    detail = {"frame_budget": budget, "files": files}
    ok = True
    try:
        cli_proj = _long_ts_project(r.root / "cli_proj", files, "cli")
        mcp_proj = _long_ts_project(r.root / "mcp_proj", files, "mcp")
        r.start_owner(cli_proj)
        started = time.monotonic()
        code, obj, text = r.cli("add", ["add", str(cli_proj), "--wait", "--wait-timeout", "900"], cli_proj)
        detail["cli_seconds"] = round(time.monotonic() - started, 1)
        detail["cli_exit"] = code
        detail["cli_bad_words"] = [w for w in FRAME_BAD if w in text.lower()]
        detail["cli_state"] = json.dumps(obj)[:300] if obj is not None else text[:300]
        ok &= code == 0 and not detail["cli_bad_words"]
        r.start_mcp(mcp_proj)
        started = time.monotonic()
        result = r.tool("backend.index", {"path": str(mcp_proj)})
        polls, errors = 1, int(bool(result.get("isError")))
        deadline = time.monotonic() + 900
        while True:
            call = (result.get("structuredContent") or {}).get("index_poll", {}).get("call")
            if not call:
                break
            if time.monotonic() > deadline:
                raise RuntimeError("MCP index timed out")
            time.sleep(0.02)
            result = r.rpc("tools/call", call)
            polls += 1
            errors += int(bool(result.get("isError")))
        detail.update({"mcp_seconds": round(time.monotonic() - started, 1), "mcp_polls": polls, "mcp_error_results": errors,
                       "mcp_final_isError": bool(result.get("isError"))})
        text = json.dumps(result).lower()
        detail["mcp_bad_words"] = [w for w in FRAME_BAD if w in text]
        ok &= errors == 0 and not result.get("isError") and not detail["mcp_bad_words"]
        found = r.tool("backend.search", {"query": "step7", "limit": 3})
        detail["mcp_search_isError"] = bool(found.get("isError"))
        ok &= not found.get("isError")
        (out / "summary.json").write_text(json.dumps(detail, indent=1))
        return {"long_index_frame_budget": {"pass": bool(ok), "detail": json.dumps(detail)}}
    finally:
        r.stop()


TS_VIEW = "export function Greeting() {\n  return '%s';\n}\n"
CSHARP_PROGRAM = "public class OrderService { public int Count() { return 1; } }\n"


def _served_source(r, proj, label, name):
    """The source text the public route serves for the declaration named `name` (None when none is served)."""
    _, found, _ = r.cli(f"search-{label}", ["search", name, "--limit", "20"], proj)
    texts = []
    for row in _rows(found):
        ident = row.get("identity", {})
        if ident.get("name") != name or not ident.get("coordinate"):
            continue
        code, _, text = r.cli(f"source-{label}-{len(texts)}", ["source", ident["coordinate"]], proj, json_out=False)
        if code == 0:
            texts.append(text)
    return texts


def ts_gains_second_language(bin_dir, out):
    """A TypeScript project indexed through native evidence gains C#: the whole index must not fail,
    TypeScript must be served from the current files, and a cold reopen must serve the same."""
    r = Route(bin_dir, out)
    steps = {}
    ok = True
    try:
        proj = r.root / "mixed_proj"
        fixtures.write(proj, "tsconfig.json", '{"compilerOptions":{"strict":true},"include":["*.ts"]}\n')
        fixtures.write(proj, "view.ts", TS_VIEW % "before")
        fixtures.write(proj, "consumer.ts", "import { Greeting } from './view';\nGreeting();\n")
        r.start_owner(proj)
        code, obj, text = r.cli("add-ts-only", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        steps["ts_only_add_exit"] = code
        first = _served_source(r, proj, "ts-only", "Greeting")
        steps["ts_only_served"] = [("before" in t) for t in first]
        ok &= code == 0 and any("return 'before'" in t for t in first)
        # The project gains C# beside its TypeScript, and the TypeScript changes.
        fixtures.write(proj, "Program.cs", CSHARP_PROGRAM)
        fixtures.write(proj, "view.ts", TS_VIEW % "after")
        code, obj, text = r.cli("add-mixed", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        steps["mixed_add_exit"] = code
        steps["mixed_add_refused"] = "cannot admit mixed" in text or "already has selected native" in text
        ok &= code == 0 and not steps["mixed_add_refused"]
        ts = _served_source(r, proj, "mixed-ts", "Greeting")
        cs = _served_source(r, proj, "mixed-cs", "OrderService")
        steps["mixed_ts_current"] = any("return 'after'" in t for t in ts)
        steps["mixed_ts_stale"] = any("return 'before'" in t for t in ts)
        steps["mixed_cs_served"] = any("OrderService" in t for t in cs)
        ok &= steps["mixed_ts_current"] and not steps["mixed_ts_stale"] and steps["mixed_cs_served"]
        r.cli("status-mixed", ["--format", "markdown", "status"], proj, json_out=False)
        # Cold reopen: same answers from a new owner process.
        r.restart_owner(proj)
        ts2 = _served_source(r, proj, "reopen-ts", "Greeting")
        cs2 = _served_source(r, proj, "reopen-cs", "OrderService")
        steps["reopen_ts_current"] = any("return 'after'" in t for t in ts2) and not any("return 'before'" in t for t in ts2)
        steps["reopen_cs_served"] = any("OrderService" in t for t in cs2)
        ok &= steps["reopen_ts_current"] and steps["reopen_cs_served"]
        # Dropping the second language again keeps the project indexable.
        (proj / "Program.cs").unlink()
        code, obj, text = r.cli("add-back", ["add", str(proj), "--wait", "--wait-timeout", "900"], proj)
        steps["back_add_exit"] = code
        back = _served_source(r, proj, "back-ts", "Greeting")
        steps["back_ts_current"] = any("return 'after'" in t for t in back) and not any("return 'before'" in t for t in back)
        ok &= code == 0 and steps["back_ts_current"]
        (out / "summary.json").write_text(json.dumps(steps, indent=1))
        return {"ts_gains_second_language": {"pass": bool(ok), "detail": json.dumps(steps)}}
    finally:
        r.stop()


SCENARIOS = {"probe_packages": probe_packages, "probe_vue": probe_vue, "references_nested_calls": references_nested_calls, "perf_timing": perf_timing, "probe_owner_retirement": probe_owner_retirement, "query_output": query_output, "ts_gains_second_language": ts_gains_second_language, "long_index_frame_budget": long_index_frame_budget, "gate_on_signed_out": gate_on_signed_out, "verify_budget": verify_budget, "probe_budget_no_csharp": probe_budget_no_csharp, "probe_budget_no_appshell": probe_budget_no_appshell, "probe_budget_no_next": probe_budget_no_next, "probe_refusals": probe_refusals, "probe_refs_mixed": probe_refs_mixed, "startup_missing_checker": startup_missing_checker, "probe_crm": probe_crm, "probe_budget_diag": probe_budget_diag, "probe_reindex_refusal": probe_reindex_refusal, "probe_refs_nested": probe_refs_nested, "probe_refs_gap": probe_refs_gap, "probe_react_variants": probe_react_variants, "probe_budget": probe_budget, "startup_path": startup_path, "probe_refs": probe_refs,
             "probe_edits": probe_edits, "probe_churn": probe_churn, "probe_mcp": probe_mcp}
