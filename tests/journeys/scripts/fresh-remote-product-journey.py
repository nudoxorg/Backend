#!/usr/bin/env python3
"""Run real CLI/MCP journeys in a new user/state/project namespace.

This is supplemental product evidence, not a replacement for the canonical
source-origin acceptance runner and not an installer certification.
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import selectors
import shutil
import signal
import subprocess
import time
import uuid


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def tickets(value):
    if isinstance(value, dict):
        if {"id", "owner_epoch", "package"}.issubset(value):
            yield value
        for child in value.values():
            yield from tickets(child)
    elif isinstance(value, list):
        for child in value:
            yield from tickets(child)


def tree(root):
    rows = []
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink() and "node_modules" not in path.parts:
            rows.append([path.relative_to(root).as_posix(), path.stat().st_size, digest(path)])
    return {"files": rows, "sha256": hashlib.sha256(json.dumps(rows, separators=(",", ":")).encode()).hexdigest()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--images", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--origin", type=Path)
    parser.add_argument("--runs", type=Path, required=True)
    parser.add_argument("--language", choices=["typescript", "python", "go"], required=True)
    parser.add_argument("--mode", choices=["stock-host", "stock-tools", "guided-tools", "configured"], required=True)
    parser.add_argument("--compiler-variable", action="append", default=[], help="Public compiler variable explicitly configured in guided-tools mode")
    parser.add_argument("--compiler-snapshot", type=Path, required=True)
    parser.add_argument("--transport-only", action="store_true")
    parser.add_argument("--http", action="store_true")
    parser.add_argument("--query", action="append")
    parser.add_argument("--edit-file")
    parser.add_argument("--replace-from")
    parser.add_argument("--replace-to")
    parser.add_argument("--keep-edited-source", action="store_true", help="Keep the private edited project through the cold read, binding the current frontier to source version 1")
    parser.add_argument("--cancel-before-first-add", action="store_true")
    parser.add_argument("--skip-update-cancel", action="store_true", help="Keep the unchanged retry independent of a preceding background job/cancellation")
    parser.add_argument("--first-publish-only", action="store_true", help="Observe one initial add and sequential CLI/MCP reads; omit retries and cold boundaries")
    parser.add_argument("--witness-path", help="Prioritize a real source witness path among resolved declarations")
    parser.add_argument("--witness-line", type=int, help="Prioritize the exact real declaration line within a witness file")
    parser.add_argument("--resolve-detail", choices=["full", "summary"], default="full", help="Ordinary resolve presentation budget; subsequent source/document reads remain full")
    args = parser.parse_args()
    root = args.runs / (time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + args.language + "-" + args.mode + "-" + uuid.uuid4().hex[:8])
    root.mkdir(parents=True, mode=0o700)
    project, home, state, tmp = [root / name for name in ["project", "home", "state", "tmp"]]
    for path in [home, tmp]:
        path.mkdir(mode=0o700)
    shutil.copytree(args.source, project, ignore=shutil.ignore_patterns(".git", "__pycache__", ".pytest_cache", "*.tsbuildinfo", ".nudox", "target"), symlinks=True)
    endpoint = "/tmp/nudox-fresh-" + uuid.uuid4().hex[:20] + ".sock"
    snapshot = json.loads(args.compiler_snapshot.read_text())
    compiler_paths = {row["variable"]: bytes(row["path"]["units"]).decode() for row in snapshot["paths"]}
    base_path = "/run/current-system/sw/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin"
    tool_dirs = sorted({str(Path(path).parent) for name, path in compiler_paths.items() if name != "NUDOX_TYPESCRIPT_MODULE_ROOT" and Path(path).is_file()})
    env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"), "XDG_DATA_HOME": str(home / ".local/share"), "XDG_STATE_HOME": str(home / ".local/state"), "TMPDIR": str(tmp), "PATH": base_path if args.mode == "stock-host" else ":".join(tool_dirs + [base_path]), "LANG": "C.UTF-8", "USER": "root", "LOGNAME": "root"}
    if args.mode == "configured":
        env["BACKEND_LOCALD_COMPILER_ENVIRONMENT"] = json.dumps(snapshot, separators=(",", ":"))
    if args.mode == "guided-tools":
        assert args.compiler_variable, "guided-tools requires an explicit public setup variable"
        for variable in args.compiler_variable:
            assert variable in compiler_paths and variable.startswith("NUDOX_")
            env[variable] = compiler_paths[variable]
    else:
        assert not args.compiler_variable, "public variables must be labelled guided-tools"
    binaries = {name: args.images / name for name in ["backend-cli", "backend-locald", "backend-mcp"]}
    for name, path in binaries.items():
        assert path.is_file() and not path.is_symlink(), (name, path)
    build_manifest = json.loads(args.manifest.read_text())
    assert build_manifest["schema"] == "nudox.runtime-build-manifest.v1"
    for name, path in binaries.items():
        claimed = build_manifest["executables"][name]
        assert claimed == {"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}, (name, claimed)
    receipt = build_manifest["root_receipt"]
    assert digest(receipt["path"]) == receipt["sha256"], "build receipt changed"
    common = ["--project", str(project), "--workspace", str(state), "--endpoint", endpoint]
    wire = (root / "wire.jsonl").open("w")
    owners, clients = set(), []
    checks = []
    initial_cancel_ticket = None

    def record(kind, **data):
        row = {"kind": kind, "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **data}
        wire.write(json.dumps(row, ensure_ascii=False) + "\n")
        wire.flush()
        return row

    def owner_rows():
        rows = []
        for proc in Path("/proc").glob("[0-9]*"):
            try:
                words = (proc / "cmdline").read_bytes().split(b"\0")
                if words[0].decode() == str(binaries["backend-locald"]) and str(state).encode() in words and endpoint.encode() in words:
                    rows.append((int(proc.name), [word.decode() for word in words if word]))
            except (FileNotFoundError, ProcessLookupError, PermissionError, UnicodeError):
                pass
        return rows

    def remember_owner(label):
        rows = owner_rows()
        for pid, words in rows:
            owners.add(pid)
            record("owner", label=label, pid=pid, argv=words, executable=os.readlink("/proc/" + str(pid) + "/exe"))
        return rows

    def stop_owner(label):
        for pid, words in owner_rows():
            assert pid in owners, "unowned process must not be terminated"
            os.kill(pid, signal.SIGTERM)
            deadline = time.monotonic() + 20
            while any(row[0] == pid for row in owner_rows()):
                if time.monotonic() > deadline:
                    raise TimeoutError("owned locald did not retire")
                time.sleep(.05)
            owners.discard(pid)
            record("owner-stopped", label=label, pid=pid, argv=words)

    def run(argv, label, timeout=120):
        start = time.monotonic()
        proc = subprocess.Popen(argv, cwd=project, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            out, err = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                out, err = proc.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                out, err = proc.communicate()
        (root / (label + ".stdout")).write_bytes(out)
        (root / (label + ".stderr")).write_bytes(err)
        try:
            payload = json.loads(out)
        except (ValueError, UnicodeError):
            payload = None
        row = record("command", label=label, argv=argv, exit=proc.returncode, seconds=round(time.monotonic() - start, 6), stdout_bytes=len(out), stderr_bytes=len(err), stdout_sha256=hashlib.sha256(out).hexdigest(), stderr_sha256=hashlib.sha256(err).hexdigest(), payload=payload, stderr=err.decode(errors="replace")[:16384])
        print(json.dumps({key: row[key] for key in ["label", "exit", "seconds", "stdout_bytes"]} | {"fault": payload if isinstance(payload, dict) and payload.get("answer") == "fault" else None}), flush=True)
        return row

    def cli(words, label, timeout=120, detail="full"):
        return run([str(binaries["backend-cli"]), "--json", "--detail", detail, *common, *words], label, timeout)

    class Stdio:
        def __init__(self, label):
            self.stderr = (root / (label + "-mcp.stderr")).open("wb")
            self.proc = subprocess.Popen([str(binaries["backend-mcp"]), *common], env=env, cwd=project, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, start_new_session=True)
            clients.append(self)
            self.selector = selectors.DefaultSelector()
            self.selector.register(self.proc.stdout, selectors.EVENT_READ)
            self.sequence, self.buffer = 0, b""

        def rpc(self, method, params=None, notify=False):
            self.sequence += 1
            request = {"jsonrpc": "2.0", "method": method, "params": params or {}}
            if not notify:
                request["id"] = self.sequence
            record("mcp-request", payload=request)
            self.proc.stdin.write((json.dumps(request) + "\n").encode())
            self.proc.stdin.flush()
            if notify:
                return None
            start, deadline = time.monotonic(), time.monotonic() + 120
            while b"\n" not in self.buffer:
                if not self.selector.select(max(0, deadline - time.monotonic())):
                    raise TimeoutError("MCP " + method)
                block = os.read(self.proc.stdout.fileno(), 65536)
                if not block:
                    raise RuntimeError("MCP EOF")
                self.buffer += block
                if len(self.buffer) > 4 * 1024 * 1024:
                    raise RuntimeError("MCP output exceeded 4MiB capture ceiling")
            line, self.buffer = self.buffer.split(b"\n", 1)
            reply = json.loads(line)
            assert reply.get("id") == self.sequence, reply
            record("mcp-reply", method=method, seconds=time.monotonic() - start, bytes=len(line) + 1, estimated_tokens=math.ceil((len(line) + 1) / 4), payload=reply)
            return reply

        def tool(self, name, arguments=None):
            return self.rpc("tools/call", {"name": "backend." + name, "arguments": {"detail": "full", **(arguments or {})}})

        def close(self):
            if self.proc.poll() is None:
                self.proc.stdin.close()
                try:
                    self.proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(self.proc.pid, signal.SIGTERM)
                    self.proc.wait(timeout=10)
            self.selector.close()
            self.stderr.close()
            clients.remove(self)

    def setup_mcp(label):
        mcp = Stdio(label)
        mcp.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "fresh-real-remote-acceptance", "version": "1"}})
        mcp.rpc("notifications/initialized", notify=True)
        listing = mcp.rpc("tools/list")
        tools = listing.get("result", {}).get("tools", [])
        record("tool-enumeration", names=[tool["name"] for tool in tools], tool_count=len(tools), compact_json_bytes=len(json.dumps(tools, separators=(",", ":")).encode()), estimated_tokens=math.ceil(len(json.dumps(tools, separators=(",", ":")).encode()) / 4))
        return mcp

    def http_probe():
        token = "private-acceptance-" + uuid.uuid4().hex
        http_env = {**env, "BACKEND_MCP_TOKEN": token}
        stderr_path = root / "http-mcp.stderr"
        stderr = stderr_path.open("wb")
        proc = subprocess.Popen([str(binaries["backend-mcp"]), *common, "--http", "127.0.0.1:0"], env=http_env, cwd=project, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=stderr, start_new_session=True)
        try:
            deadline = time.monotonic() + 15
            ready = None
            while time.monotonic() < deadline and proc.poll() is None:
                for line in stderr_path.read_text(errors="replace").splitlines():
                    if "backend-mcp: ready " in line:
                        ready = json.loads(line.split("backend-mcp: ready ", 1)[1])
                if ready:
                    break
                time.sleep(.05)
            if ready is None:
                raise RuntimeError("HTTP no ready line")
            from urllib.parse import urlparse
            url = urlparse(ready["url"])
            record("http-ready", metadata={key: value for key, value in ready.items() if key != "authorization"}, bearer_token_printed_to_stderr=token in stderr_path.read_text())

            def post(label, request, authorization=True, session=None, content_type="application/json", accept="application/json, text/event-stream"):
                body = request if isinstance(request, bytes) else json.dumps(request).encode()
                headers = {"Content-Type": content_type, "Accept": accept}
                if authorization:
                    headers["Authorization"] = "Bearer " + (token if authorization is True else "deliberately-wrong-private-token")
                if session:
                    headers["Mcp-Session-Id"] = session
                conn = http.client.HTTPConnection(url.hostname, url.port, timeout=30)
                start = time.monotonic()
                conn.request("POST", url.path, body, headers)
                response = conn.getresponse()
                out = response.read(4 * 1024 * 1024 + 1)
                assert len(out) <= 4 * 1024 * 1024
                try:
                    payload = json.loads(out)
                except ValueError:
                    payload = out.decode(errors="replace")
                reply_headers = dict(response.getheaders())
                record("http-reply", label=label, http_status=response.status, headers=reply_headers, bytes=len(out), seconds=time.monotonic() - start, payload=payload)
                conn.close()
                return response.status, reply_headers, payload

            initialize = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "fresh-real-http", "version": "1"}}}
            post("missing-token", initialize, authorization=False)
            post("wrong-token", initialize, authorization="wrong")
            status, headers, payload = post("initialize", initialize)
            session = next((value for key, value in headers.items() if key.lower() == "mcp-session-id"), None)
            post("initialized", {"jsonrpc": "2.0", "method": "notifications/initialized"}, session=session)
            post("tools-list", {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, session=session)
            post("status", {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "backend.status", "arguments": {}}}, session=session)
            post("invalid-media", {"jsonrpc": "2.0", "id": 4, "method": "ping"}, session=session, content_type="text/plain", accept="text/plain")
            post("unknown-session", {"jsonrpc": "2.0", "id": 5, "method": "ping"}, session="not-a-real-session")
            post("malformed-json", b"{not-json", session=session)
            # A request just above the advertised bound can be rejected before
            # the body has been sent; record that real transport outcome too.
            try:
                post("oversized-request", b" " * (ready["maxRequestBytes"] + 1), session=session)
            except (BrokenPipeError, ConnectionResetError) as error:
                record("http-early-rejection", label="oversized-request", error_type=type(error).__name__, message=str(error))
            conn = http.client.HTTPConnection(url.hostname, url.port, timeout=20)
            conn.request("DELETE", url.path, headers={"Authorization": "Bearer " + token, "Mcp-Session-Id": session})
            response = conn.getresponse()
            record("http-delete", http_status=response.status, bytes=len(response.read()))
            conn.close()
            post("deleted-session", {"jsonrpc": "2.0", "id": 6, "method": "ping"}, session=session)
            post("reinitialize", initialize)
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                proc.wait(timeout=10)
            stderr.close()
            # Token is private test data, but shipped readiness logs disclose it.
            stderr_path.write_text(stderr_path.read_text().replace(token, "[private-test-token-redacted]"))

    before = tree(project)
    metadata = {"schema": "nudox.fresh-real-remote-runtime.v1", "root": str(root), "language": args.language, "mode": args.mode, "source": str(args.source), "source_origin": json.loads(args.origin.read_text()) if args.origin else None, "source_before": before, "runtime_manifest": str(args.manifest), "runtime_manifest_sha256": digest(args.manifest), "runtime_manifest_content": json.loads(args.manifest.read_text()), "binaries": {name: {"path": str(path), "sha256": digest(path)} for name, path in binaries.items()}, "env": env, "compiler_snapshot_sha256": digest(args.compiler_snapshot), "workspace_preexisting": state.exists(), "endpoint_preexisting": Path(endpoint).exists(), "fresh_home_entries": list(home.iterdir()), "installer_tested": False, "dependency_closure_copied": (project / "node_modules").exists(), "accepted_project_pass": False}
    metadata["journey_options"] = {"skip_update_cancel": args.skip_update_cancel, "cancel_before_first_add": args.cancel_before_first_add, "transport_only": args.transport_only, "public_compiler_variables": args.compiler_variable, "requested_queries": args.query, "edit_file": args.edit_file, "keep_edited_source": args.keep_edited_source, "first_publish_only": args.first_publish_only, "witness_path": args.witness_path, "witness_line": args.witness_line, "resolve_detail": args.resolve_detail}
    (root / "setup.json").write_text(json.dumps(metadata, indent=2, default=str) + "\n")
    record("setup", **metadata)
    try:
        for tool in ["node", "npm", "tsc", "python3", "pyrefly", "go"]:
            selected = shutil.which(tool, path=env["PATH"])
            record("external-tool", name=tool, path=selected, realpath=str(Path(selected).resolve()) if selected else None, sha256=digest(Path(selected).resolve()) if selected else None)
            if selected:
                run([selected, "version" if tool == "go" else "--version"], "tool-" + tool, 20)
        health = cli(["health"], "fresh-health")
        remember_owner("fresh-auto-start")
        if args.first_publish_only:
            assert not args.edit_file and not args.cancel_before_first_add and not args.http and not args.transport_only
            queries = args.query or []
            assert queries, "first-publication exploration needs a real source witness query"
            added = cli(["add", str(project)], "fresh-add", 600)
            remember_owner("after-add")
            selected_health = cli(["health"], "post-add-health")
            cli(["packages"], "post-add-packages")
            payload = selected_health.get("payload") or {}
            published = added["exit"] == 0 and payload.get("sequence", 0) > 0 and payload.get("rows", 0) > 0
            metadata["first_publication_observed"] = published
            record("first-publication-observation", observed=published, selected_health=payload, add_reply=added.get("payload"), complete_runtime_acceptance=False)
            coordinates = []
            if published:
                cli(["semantic-versions", str(project)], "post-add-semantic-versions")
                for query in queries:
                    cli(["search", query, "--limit", "10"], "query-search-" + query.replace("/", "_"))
                resolve_limit = 200 if args.witness_path else 10
                resolved = cli(["resolve", queries[0], "--limit", str(resolve_limit)], "query-resolve", detail=args.resolve_detail)
                rows = (resolved.get("payload") or {}).get("records", [])
                if args.witness_path:
                    rows.sort(key=lambda row: (row.get("identity", {}).get("path") != args.witness_path, bool(args.witness_line) and row.get("identity", {}).get("line") != args.witness_line))
                    record("requested-source-witness-observation", path=args.witness_path, line=args.witness_line, matched=any(row.get("identity", {}).get("path") == args.witness_path and (not args.witness_line or row.get("identity", {}).get("line") == args.witness_line) for row in rows), resolve_limit=resolve_limit, more=(resolved.get("payload") or {}).get("more"), complete_runtime_acceptance=False)
                for row in rows[:2]:
                    coordinate = row.get("identity", {}).get("coordinate")
                    if coordinate:
                        coordinates.append(coordinate)
                        for command in ["source", "show", "references", "graph"]:
                            cli([command, coordinate], "query-" + command + "-" + hashlib.sha256(coordinate.encode()).hexdigest()[:8])
            # Clients are sequential: two private cases use at most four product
            # processes (two owners plus one CLI or MCP client per owner).
            mcp = setup_mcp("warm")
            mcp.tool("status")
            if published:
                for query in queries:
                    reply = mcp.tool("search", {"query": query, "limit": 10})
                    checks.append({"query": query, "mcp_search": reply})
                mcp.tool("resolve", {"query": queries[0], "limit": resolve_limit, "detail": args.resolve_detail})
                for coordinate in coordinates:
                    for tool in ["source", "document", "references", "graph"]:
                        mcp.tool(tool, {"coordinate": coordinate})
            mcp.close()
            return
        mcp = setup_mcp("warm")
        mcp.tool("status")
        mcp.rpc("resources/list")
        mcp.rpc("resources/read", {"uri": "backend://workspace/current"})
        if args.cancel_before_first_add:
            started = mcp.tool("index_start", {"package": str(project), "execution_intent": "background"})
            ticket = next(tickets(started), None)
            initial_cancel_ticket = ticket
            record("initial-cancel-ticket", ticket=ticket, reply=started)
            if ticket:
                mcp.tool("index_cancel", {"ticket": ticket})
                for observation in range(80):
                    reply = mcp.tool("index_progress", {"ticket": ticket})
                    if any(word in json.dumps(reply).lower() for word in ["cancelled", "published", "refused", "failed"]):
                        break
                    time.sleep(.2)
                cli(["health"], "after-initial-cancel-health")
        if args.http:
            http_probe()
        queries = args.query or {"typescript": ["getHello", "AppService", "src/app.service.ts"], "python": ["NoAppException", "Quart", "src/quart/app.py"], "go": ["NewRoute", "Router", "route.go"]}[args.language]
        lifecycle_coordinates = []
        if not args.transport_only:
            added = cli(["add", str(project)], "fresh-add", 600)
            remember_owner("after-add")
            cli(["health"], "post-add-health")
            cli(["packages"], "post-add-packages")
            cli(["semantic-versions", str(project)], "post-add-semantic-versions")
            for query in queries:
                cli(["search", query, "--limit", "10"], "query-search-" + query.replace("/", "_"))
                reply = mcp.tool("search", {"query": query, "limit": 10})
                checks.append({"query": query, "mcp_search": reply})
            resolve_limit = 200 if args.witness_path else 10
            resolved = cli(["resolve", queries[0], "--limit", str(resolve_limit)], "query-resolve", detail=args.resolve_detail)
            mcp.tool("resolve", {"query": queries[0], "limit": resolve_limit, "detail": args.resolve_detail})
            cli(["outline"], "query-outline-bare")
            cli(["outline", "."], "query-outline-dot")
            cli(["outline", str(project)], "query-outline-absolute")
            for count in [1, 200, 65535]:
                mcp.tool("outline", {"limit": count})
            paged = mcp.tool("search", {"query": queries[0], "limit": 1})
            page_records, page_count = [], 0
            while True:
                page_count += 1
                dto = paged.get("result", {}).get("structuredContent", {})
                page_records.extend(dto.get("records", []))
                cursor = dto.get("nextCursor")
                if not cursor or page_count >= 20:
                    break
                paged = mcp.tool("search", {"query": queries[0], "limit": 1, "cursor": cursor})
            coordinates = [row.get("identity", {}).get("coordinate") for row in page_records]
            record("pagination-observation", query=queries[0], pages=page_count, records=len(page_records), coordinates=coordinates, duplicate_coordinates=len(coordinates) != len(set(coordinates)), unresolved_cursor=cursor)
            mcp.tool("search", {"query": queries[0], "limit": 0})
            mcp.tool("search", {"query": queries[0], "limit": 65536})
            mcp.tool("search", {"query": queries[0], "limit": 1, "cursor": "bad-cursor"})
            rows = (resolved.get("payload") or {}).get("records", [])
            if args.edit_file:
                rows.sort(key=lambda row: row.get("identity", {}).get("path") != args.edit_file)
            if args.witness_path:
                rows.sort(key=lambda row: (row.get("identity", {}).get("path") != args.witness_path, bool(args.witness_line) and row.get("identity", {}).get("line") != args.witness_line))
            lifecycle_coordinates = [row["identity"]["coordinate"] for row in rows[:3] if row.get("identity", {}).get("coordinate")]
            for row in rows[:3]:
                coordinate = row.get("identity", {}).get("coordinate")
                if coordinate:
                    for command in ["source", "show", "references", "graph", "related"]:
                        cli([command, coordinate], "query-" + command + "-" + hashlib.sha256(coordinate.encode()).hexdigest()[:8])
                    for tool in ["source", "document", "references", "graph"]:
                        mcp.tool(tool, {"coordinate": coordinate})
            if not args.skip_update_cancel:
                # The job ticket is copied from the actual owner reply, never invented.
                started = mcp.tool("index_start", {"package": str(project), "execution_intent": "background"})
                ticket = next(tickets(started), None)
                record("job-ticket-discovery", ticket=ticket, reply=started)
                if ticket:
                    mcp.tool("index_progress", {"ticket": ticket})
                    mcp.tool("index_cancel", {"ticket": ticket})
                    for observation in range(80):
                        reply = mcp.tool("index_progress", {"ticket": ticket})
                        text = json.dumps(reply).lower()
                        if any(word in text for word in ["cancelled", "published", "refused", "failed"]):
                            break
                        time.sleep(.2)
                    cli(["health"], "after-job-cancel-health")
            cli(["add", str(project)], "retry-add", 600)
            cli(["health"], "retry-health")
            mcp.tool("status")
            for query in queries:
                cli(["search", query, "--limit", "10"], "retry-search-" + query.replace("/", "_"))
                mcp.tool("search", {"query": query, "limit": 10})
            cli(["resolve", queries[0], "--limit", str(resolve_limit)], "retry-resolve", detail=args.resolve_detail)
            if args.edit_file:
                assert args.replace_from and args.replace_to
                target = project / args.edit_file
                assert target.resolve().is_relative_to(project.resolve())
                original_bytes = target.read_bytes()
                old, new = args.replace_from.encode(), args.replace_to.encode()
                assert original_bytes.count(old) == 1, "edit must bind one existing source occurrence"
                changed = original_bytes.replace(old, new, 1)
                (root / "source-version0.bin").write_bytes(original_bytes)
                (root / "source-version1.bin").write_bytes(changed)
                try:
                    target.write_bytes(changed)
                    record("source-edit", path=args.edit_file, before_sha256=hashlib.sha256(original_bytes).hexdigest(), after_sha256=hashlib.sha256(changed).hexdigest(), tree_after=tree(project))
                    cli(["add", str(project)], "edit-add", 600)
                    cli(["health"], "edit-health")
                    mcp.tool("search", {"query": queries[0], "limit": 10})
                    edited_resolution = cli(["resolve", queries[0], "--limit", str(resolve_limit)], "edit-resolve", detail=args.resolve_detail)
                    edited_rows = (edited_resolution.get("payload") or {}).get("records", [])
                    edited_rows.sort(key=lambda row: row.get("identity", {}).get("path") != args.edit_file)
                    if args.witness_path:
                        edited_rows.sort(key=lambda row: (row.get("identity", {}).get("path") != args.witness_path, bool(args.witness_line) and row.get("identity", {}).get("line") != args.witness_line))
                    for row in edited_rows[:3]:
                        coordinate = row.get("identity", {}).get("coordinate")
                        if coordinate:
                            cli(["source", coordinate], "edit-source-" + hashlib.sha256(coordinate.encode()).hexdigest()[:8])
                            cli(["show", coordinate], "edit-document-" + hashlib.sha256(coordinate.encode()).hexdigest()[:8])
                    cli(["remove", str(project)], "remove-project")
                    cli(["health"], "removed-health")
                    mcp.tool("search", {"query": queries[0], "limit": 10})
                    # These are real coordinates returned before removal.
                    # Preserve the typed absent/stale replies for all read routes.
                    for coordinate in lifecycle_coordinates:
                        suffix = hashlib.sha256(coordinate.encode()).hexdigest()[:8]
                        for command in ["source", "show", "references", "graph"]:
                            cli([command, coordinate], "removed-" + command + "-" + suffix)
                        for tool in ["source", "document", "references", "graph"]:
                            mcp.tool(tool, {"coordinate": coordinate})
                    cli(["add", str(project)], "readd-project", 600)
                    cli(["health"], "readded-health")
                    for coordinate in lifecycle_coordinates:
                        suffix = hashlib.sha256(coordinate.encode()).hexdigest()[:8]
                        for command in ["source", "show", "references", "graph"]:
                            cli([command, coordinate], "readded-" + command + "-" + suffix)
                        for tool in ["source", "document", "references", "graph"]:
                            mcp.tool(tool, {"coordinate": coordinate})
                finally:
                    if args.keep_edited_source:
                        record("private-source-version1-retained", path=args.edit_file, sha256=hashlib.sha256(target.read_bytes()).hexdigest(), original_acquired_source_mutated=False)
                    else:
                        target.write_bytes(original_bytes)
                        record("source-restored", path=args.edit_file, sha256=hashlib.sha256(target.read_bytes()).hexdigest())
        mcp.close()
        stop_owner("explicit-cold-boundary")
        cold_health = cli(["health"], "cold-health")
        if (cold_health.get("payload") or {}).get("cause") == "unreachable":
            cli(["health"], "cold-health-explicit-retry")
        remember_owner("cold-auto-start")
        mcp = setup_mcp("cold")
        mcp.tool("status")
        if initial_cancel_ticket:
            mcp.tool("index_progress", {"ticket": initial_cancel_ticket})
        for query in queries:
            cli(["search", query, "--limit", "10"], "cold-search-" + query.replace("/", "_"))
            mcp.tool("search", {"query": query, "limit": 10})
        for coordinate in lifecycle_coordinates:
            suffix = hashlib.sha256(coordinate.encode()).hexdigest()[:8]
            for command in ["source", "show", "references", "graph"]:
                cli([command, coordinate], "cold-" + command + "-" + suffix)
            for tool in ["source", "document", "references", "graph"]:
                mcp.tool(tool, {"coordinate": coordinate})
        remember_owner("before-live-reconnect")
        stop_owner("live-client-owner-boundary")
        mcp.tool("status")
        remember_owner("live-client-reconnected")
        mcp.close()
    except Exception as error:
        record("journey-error", error_type=type(error).__name__, message=str(error))
        metadata["journey_error"] = {"type": type(error).__name__, "message": str(error)}
    finally:
        for client in list(clients):
            try:
                client.close()
            except Exception as error:
                record("cleanup-error", message=str(error))
        remember_owner("cleanup-census")
        try:
            stop_owner("final-owned-cleanup")
        except Exception as error:
            record("cleanup-error", message=str(error))
        metadata.update(source_after=tree(project), checks=checks, owned_daemons_remaining=owner_rows(), endpoint_remaining=Path(endpoint).exists())
        metadata["source_unchanged"] = before == metadata["source_after"]
        (root / "summary.json").write_text(json.dumps(metadata, indent=2, default=str) + "\n")
        wire.close()
        print(json.dumps({"root": str(root), "source_unchanged": metadata["source_unchanged"], "accepted_project_pass": False, "remaining_owned_daemons": metadata["owned_daemons_remaining"], "journey_error": metadata.get("journey_error")}), flush=True)


if __name__ == "__main__":
    main()
