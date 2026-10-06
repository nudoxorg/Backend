#!/usr/bin/env python3
"""Read real retained private stores; preserve originals and raw client output."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("--run", type=Path, required=True)
parser.add_argument("--observer", type=Path, required=True)
parser.add_argument("--mode", choices=["membership", "cold-search", "selected-read"], required=True)
parser.add_argument("--query", required=True)
parser.add_argument("--coordinate", help="Copy an actual retained resolved coordinate verbatim")
args = parser.parse_args()

def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

assert digest(args.observer) == "491f4ab662812bb98a2c29af31382c34e9b7ab189645b56f408aad63070250f2"
sys.path.insert(0, str(args.observer.parent))
spec = importlib.util.spec_from_file_location("frozen_surface_observer", args.observer)
observer = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = observer
spec.loader.exec_module(observer)
original = json.loads((args.run / "summary.json").read_text())
assert not original["owned_daemons_remaining"]
manifest_path = Path(original["runtime_manifest"])
assert digest(manifest_path) == original["runtime_manifest_sha256"]
manifest = json.loads(manifest_path.read_text())
assert digest(manifest["root_receipt"]["path"]) == manifest["root_receipt"]["sha256"]
binaries = {}
for name, claim in manifest["executables"].items():
    identity = observer.executable_identity(name, Path(claim["path"]))
    assert identity.sha256 == claim["sha256"]
    binaries[name] = identity

mem = int(next(row.split()[1] for row in Path("/proc/meminfo").read_text().splitlines() if row.startswith("MemAvailable:")))
disk = shutil.disk_usage(args.run).free
assert mem >= 24 * 1024 * 1024 and disk >= 24 * 1024 ** 3
root = args.run.parent / (args.run.name + "-" + args.mode + "-" + uuid.uuid4().hex[:8])
root.mkdir(mode=0o700)
state, home, tmp = [root / name for name in ["state", "home", "tmp"]]
shutil.copytree(args.run / "state", state)
home.mkdir(mode=0o700)
tmp.mkdir(mode=0o700)
project = args.run / "project"
env = dict(original["env"])
env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"), XDG_DATA_HOME=str(home / ".local/share"), XDG_STATE_HOME=str(home / ".local/state"), TMPDIR=str(tmp), BACKEND_LOCALD_BIN=str(root / "disabled-auto-owner"))
deadline = observer.Deadline(600)
receipt = {"schema": "nudox.private-functional-surface-read.v1", "original_run": str(args.run), "original_state_mutated": False, "original_source_mutated": False, "root": str(root), "candidate_manifest": manifest, "candidate_manifest_sha256": digest(manifest_path), "source_tree_sha256": original["source_after"]["sha256"], "env": env, "observer_sha256": digest(args.observer), "helper_sha256": digest(Path(__file__)), "resource_before": {"memory_available_kib": mem, "disk_free_bytes": disk}, "commands": [], "owners": [], "whole_package_pass": False}
owner = None
endpoint = None

def start_owner(label, same_endpoint=False):
    global owner, endpoint
    if not same_endpoint:
        endpoint = Path("/tmp/nudox-read-" + uuid.uuid4().hex[:18] + ".sock")
    argv = ["--workspace", str(state), "--endpoint", str(endpoint), "--idle-timeout-ms", "0"]
    owner = observer.Owner(binaries["backend-locald"], argv, env, deadline, label)
    owner.start()
    start = time.monotonic()
    until = start + 60
    while True:
        owner.require_running("before real AF_UNIX connection")
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            connection.connect(str(endpoint))
            break
        except (FileNotFoundError, ConnectionRefusedError):
            if time.monotonic() >= until:
                raise TimeoutError("owner did not accept a real connection within60s")
        finally:
            connection.close()
        time.sleep(.05)
    receipt["owners"].append({"label": label, "pid": owner.process.pid, "argv": [str(binaries["backend-locald"].path), *argv], "live_readiness_seconds": time.monotonic() - start})

def stop_owner():
    global owner
    if owner is not None:
        owner.stop()
        receipt["owners"][-1]["retirement"] = owner.log_evidence
        owner = None

def call(label, words=None, tool=None, arguments=None):
    common = ["--workspace", str(state), "--project", str(project), "--endpoint", str(endpoint)]
    request = None
    if tool:
        argv = [str(binaries["backend-mcp"].path), *common]
        request = b"".join(json.dumps(row).encode() + b"\n" for row in [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "ordinary-functional-read", "version": "1"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "backend." + tool, "arguments": {"detail": "full", **(arguments or {})}}}])
        (root / (label + ".request")).write_bytes(request)
    else:
        argv = [str(binaries["backend-cli"].path), "--json", "--detail", "full", *common, *(words or [])]
    stdout, stderr, code, elapsed = observer.run_bounded_process(argv, env, request, deadline, label)
    (root / (label + ".stdout")).write_bytes(stdout)
    (root / (label + ".stderr")).write_bytes(stderr)
    row = {"label": label, "argv": argv, "exit": code, "seconds": elapsed, "stdout_bytes": len(stdout), "stdout_sha256": hashlib.sha256(stdout).hexdigest(), "stderr_sha256": hashlib.sha256(stderr).hexdigest()}
    receipt["commands"].append(row)
    print(json.dumps(row), flush=True)
    if tool:
        correlated = [json.loads(line) for line in stdout.splitlines() if line and json.loads(line).get("id") == 2]
        assert len(correlated) == 1
        return correlated[0]
    return json.loads(stdout)

def dto(reply):
    return reply.get("result", {}).get("structuredContent", {})

class Session:
    def __init__(self):
        self.argv = [str(binaries["backend-mcp"].path), "--workspace", str(state), "--project", str(project), "--endpoint", str(endpoint)]
        self.stderr = (root / "persistent-mcp.stderr").open("xb")
        self.proc = subprocess.Popen(self.argv, env=env, cwd=project, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, start_new_session=True)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)
        self.sequence, self.buffer = 0, b""
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "persistent-ordinary-cold-read", "version": "1"}})
        self.rpc("notifications/initialized", {}, notify=True)

    def rpc(self, method, params, notify=False):
        self.sequence += 1
        label = "persistent-" + str(self.sequence)
        request = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notify:
            request["id"] = self.sequence
        wire = json.dumps(request).encode() + b"\n"
        (root / (label + ".request")).write_bytes(wire)
        self.proc.stdin.write(wire)
        self.proc.stdin.flush()
        if notify:
            return None
        start = time.monotonic()
        while b"\n" not in self.buffer:
            if not self.selector.select(max(0, 120 - (time.monotonic() - start))):
                raise TimeoutError("persistent ordinary MCP reply")
            block = os.read(self.proc.stdout.fileno(), 65536)
            if not block:
                raise RuntimeError("persistent MCP EOF")
            self.buffer += block
            assert len(self.buffer) <= 1024 * 1024
        line, self.buffer = self.buffer.split(b"\n", 1)
        (root / (label + ".stdout")).write_bytes(line + b"\n")
        reply = json.loads(line)
        assert reply.get("id") == self.sequence
        receipt["commands"].append({"label": label, "argv": self.argv, "mcp_pid": self.proc.pid, "seconds": time.monotonic() - start, "request": request, "stdout_sha256": hashlib.sha256(line + b"\n").hexdigest()})
        return reply

    def tool(self, arguments):
        return self.rpc("tools/call", {"name": "backend.search", "arguments": {"detail": "full", **arguments}})

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

session = None
try:
    start_owner("initial-clone-read")
    call("initial-health", ["--passive", "health"])
    baseline = call("cli-search", ["search", args.query, "--limit", "10"])
    call("mcp-search", tool="search", arguments={"query": args.query, "limit": 10})
    if args.mode == "selected-read":
        assert args.coordinate
        receipt["source_presentations"] = []
        for cli_name, mcp_name in [("source", "source"), ("show", "document"), ("references", "references"), ("graph", "graph")]:
            left = call("cli-selected-" + cli_name, [cli_name, args.coordinate])
            right = call("mcp-selected-" + mcp_name, tool=mcp_name, arguments={"coordinate": args.coordinate})
            receipt["source_presentations"].append({"route": mcp_name, "exact_cli_mcp_parity": left == dto(right)})
    elif args.mode == "membership":
        request = {"package": {"kind": "local", "value": str(project)}, "limit": 16}
        pages = []
        for index in range(100):
            page = call("membership-" + str(index), tool="package_source_membership", arguments={"request": request})
            pages.append(page)
            current = dto(page).get("package_source_membership_page", {})
            cursor = current.get("next")
            if not cursor:
                break
            request = {**request, "cursor": cursor, "expected_source_relation_root": current["source_relation_root"], "expected_source_version": current["source_version"]}
        receipt["membership_pages"] = pages
    else:
        session = Session()
        first = session.tool({"query": args.query, "limit": 1})
        cursor = dto(first).get("nextCursor")
        assert cursor, "this real query did not produce a continuation"
        stop_owner()
        start_owner("after-kernel-exit-cold-owner", same_endpoint=True)
        call("cold-first-health", ["--passive", "health"])
        rows = dto(first).get("records", [])
        for index in range(20):
            page = session.tool({"query": args.query, "limit": 1, "cursor": cursor})
            if "error" in page or page.get("result", {}).get("isError"):
                receipt["cold_continuation_failure"] = page
                break
            rows.extend(dto(page).get("records", []))
            cursor = dto(page).get("nextCursor")
            if not cursor:
                break
        actual = [row.get("identity", {}).get("coordinate") for row in rows]
        expected = [row.get("identity", {}).get("coordinate") for row in baseline.get("records", [])]
        receipt["cold_paging"] = {"coordinates": actual, "cli_coordinates": expected, "exact_coordinate_order_parity": actual == expected, "duplicate_coordinates": len(actual) != len(set(actual)), "unresolved_cursor": cursor}
    call("final-health", ["--passive", "health"])
except Exception as error:
    receipt["error"] = {"class": type(error).__name__, "detail": str(error)}
finally:
    try:
        if session is not None:
            session.close()
    except Exception as error:
        receipt["client_cleanup_error"] = {"class": type(error).__name__, "detail": str(error)}
    finally:
        stop_owner()
    (root / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({"root": str(root), "receipt_sha256": digest(root / "receipt.json"), "error": receipt.get("error"), "whole_package_pass": False}), flush=True)
