#!/usr/bin/env python3
"""T2 surface tier: real product operations, snapshotted, with performance.

    python3 .config/ci/surface.py --bin-dir DIR [--case NAME ...] [--update]

For each case in tests/surface/cases.json the harness starts the exact
`backend-locald` it is given (normally from the T1 artifact, never rebuilt),
indexes the case's project through the CLI, then runs the case's CLI and MCP
operations against the same owner. Every answer is compared with the reviewed
snapshot in tests/surface/snapshots/<case>.json; a difference fails and prints
a diff for the author to explain. The same run records wall time per
operation and the owner's peak RSS and CPU time, compared with the reviewed
baseline in the snapshot under a tolerance band.

Projects and workspaces live at fixed paths (/tmp/nudox-surface/<case>/...),
so coordinates and content-derived identities are identical on every worker
and snapshots need no path rewriting. --update rewrites snapshots and
baselines for review instead of comparing.

Exit 0 when every case matches, 1 otherwise.
"""

from __future__ import annotations

import argparse
import difflib
import fcntl
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SURFACE = ROOT / "tests" / "surface"
SCRATCH = Path("/tmp/nudox-surface")
# A performance number may grow this much over its reviewed baseline before
# the case fails. Generous on purpose: CI workers are shared and noisy.
TOLERANCE = {"seconds": 1.5, "peak_rss_kib": 1.25, "cpu_seconds": 1.5}
# Below these floors noise dominates and a change is not a regression.
FLOOR = {"seconds": 2.0, "peak_rss_kib": 64 * 1024, "cpu_seconds": 2.0}
# Values that legitimately change from run to run.
VOLATILE_KEYS = {"elapsed_ms", "duration_ms", "timestamp", "started_at", "finished_at", "generated_at", "pid"}


def log(message: str) -> None:
    print(f"surface: {message}", flush=True)


def stable(value):
    """Drops run-dependent fields so snapshots compare behaviour only."""
    if isinstance(value, dict):
        return {key: stable(item) for key, item in sorted(value.items()) if key not in VOLATILE_KEYS}
    if isinstance(value, list):
        return [stable(item) for item in value]
    return value


class Owner:
    """One backend-locald for one case, started and stopped by the harness."""

    def __init__(self, bin_dir: Path, case_root: Path):
        self.bin_dir = bin_dir
        self.home = case_root / "home"
        self.workspace = case_root / "workspace"
        self.endpoint = case_root / "locald.sock"
        self.env = isolated_env(self.home, bin_dir)
        self.workspace.mkdir(parents=True, mode=0o700)
        authority = self.workspace / "authority.secret"
        self.env["BACKEND_LOCALD_WORKSPACE"] = str(self.workspace)
        self.env["BACKEND_LOCALD_AUTHORITY_SECRET_FILE"] = str(authority)
        authority.write_bytes(bytes([0x5A]) * 32)
        authority.chmod(0o600)
        self.log = open(case_root / "locald.log", "w")
        self.process = subprocess.Popen(
            [
                bin_dir / "backend-locald",
                "--endpoint", str(self.endpoint),
                "--workspace", str(self.workspace),
                "--profile", "builtin",
                "--authority-secret-file", str(authority),
                "--max-frame", "1048576",
                "--timeout-ms", "180000",
                "--registry-offline",
                "--registry-discovery-offline",
                "--advisory-offline",
                "--forge-offline",
                "--idle-timeout-ms", "5000",
            ],
            env=self.env,
            cwd=case_root,
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        deadline = time.monotonic() + 30
        while not self.endpoint.exists():
            if self.process.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError(f"backend-locald did not open its endpoint:\n{self.tail()}")
            time.sleep(0.05)

    def tail(self) -> str:
        self.log.flush()
        return Path(self.log.name).read_text(errors="replace")[-3000:]

    def usage(self) -> dict:
        """Peak RSS (VmHWM) and CPU time of the owner so far, from /proc."""
        pid = self.process.pid
        status = Path(f"/proc/{pid}/status").read_text()
        peak = int(re.search(r"VmHWM:\s+(\d+) kB", status).group(1))
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        ticks = os.sysconf("SC_CLK_TCK")
        cpu = (int(fields[11]) + int(fields[12])) / ticks
        return {"peak_rss_kib": peak, "cpu_seconds": round(cpu, 2)}

    def stop(self) -> None:
        if self.process.poll() is None:
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait()
                raise RuntimeError("backend-locald failed to retire through its idle timeout")
        self.log.close()
        if self.process.returncode != 0:
            raise RuntimeError(f"backend-locald exited {self.process.returncode}")


def isolated_env(home: Path, bin_dir: Path) -> dict[str, str]:
    env = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(("BACKEND_", "FORGEJO_", "NUDOX_WORKSPACE", "NUDOX_PROJECT"))
        and name != "NIX_BUILDER_KEY"
    }
    for name, sub in (
        ("HOME", ""),
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
        ("XDG_RUNTIME_DIR", "run"),
    ):
        path = home / sub
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        env[name] = str(path)
    env["BACKEND_LOCALD_BIN"] = str(bin_dir / "backend-locald")
    env["NO_COLOR"] = "1"
    return env


def run_cli(owner: Owner, project: Path, args: list[str], timeout: float) -> tuple[dict, float]:
    started = time.monotonic()
    result = subprocess.run(
        [owner.bin_dir / "backend-cli", "--endpoint", str(owner.endpoint), "--project", str(project), "--json", *args],
        env=owner.env,
        cwd=project,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    seconds = time.monotonic() - started
    try:
        body = json.loads(result.stdout) if result.stdout.strip() else None
    except json.JSONDecodeError:
        body = {"unparsed_stdout": result.stdout[-4000:]}
    answer = {"exit": result.returncode, "json": body}
    if result.returncode != 0:
        answer["stderr"] = result.stderr.strip()[-2000:]
    return answer, seconds


def run_mcp(owner: Owner, project: Path, calls: list[dict], timeout: float) -> tuple[dict, float]:
    requests = [
        {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "nudox-t2-surface", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
    ]
    for index, call in enumerate(calls, start=2):
        requests.append({"jsonrpc": "2.0", "id": index, "method": "tools/call",
                         "params": {"name": call["tool"], "arguments": call.get("arguments", {})}})
    started = time.monotonic()
    result = subprocess.run(
        [owner.bin_dir / "backend-mcp", "--endpoint", str(owner.endpoint), "--project", str(project)],
        env=owner.env,
        cwd=project,
        input="".join(json.dumps(request) + "\n" for request in requests),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    seconds = time.monotonic() - started
    replies = {}
    for line in result.stdout.splitlines():
        if line.strip():
            message = json.loads(line)
            if "id" in message:
                replies[message["id"]] = message
    tools = sorted(tool["name"] for tool in replies.get(1, {}).get("result", {}).get("tools", []))
    answers = []
    for index, call in enumerate(calls, start=2):
        reply = replies.get(index, {})
        answers.append({"call": call, "reply": reply.get("result", reply.get("error"))})
    return {"tools": tools, "calls": answers, "exit": result.returncode}, seconds


def require_cli_success(name: str, answer: dict) -> None:
    if answer["exit"] != 0 or not isinstance(answer["json"], dict) or answer["json"].get("answer") == "fault":
        raise RuntimeError(f"{name} failed: {answer}")


def require_completed_index(answer: dict, profile_count: int) -> str:
    """Exit zero alone admits failed terminal operations; require native completion."""
    require_cli_success("cli add", answer)
    observation = answer["json"].get("index_operation", {})
    detail = observation.get("detail", {})
    terminal = detail.get("state", {})
    profiles = detail.get("source_capture", {}).get("profiles", [])
    if observation.get("state") != "known" or terminal.get("state") != "published":
        raise RuntimeError(f"index setup: expected a published operation receipt; observation={observation.get('state')}, terminal={terminal}")
    if len(profiles) != profile_count or any(
        p.get("state", {}).get("state") != "published"
        or p.get("state", {}).get("detail", {}).get("coverage") != {"coverage": "complete"}
        for p in profiles
    ):
        raise RuntimeError("index setup: every fixture profile must publish complete native coverage")
    root = terminal.get("detail", {}).get("view_root")
    if not isinstance(root, str) or not re.fullmatch(r"[0-9a-f]{64}", root):
        raise RuntimeError("index setup: missing exact published view root")
    return root


def require_index_identity(answer: dict, project: Path, expected: dict | None = None) -> dict:
    """Bind fresh-client observations to the exact admitted request, never just exit zero."""
    observation = (answer.get("json") or {}).get("index_operation", {})
    detail = observation.get("detail", {})
    if observation.get("state") != "known" or not isinstance(detail, dict):
        raise RuntimeError("index resumption: operation must be known")
    for field in ("operation_key", "request_digest"):
        value = detail.get(field)
        if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value) or value == "0" * 64:
            raise RuntimeError(f"index resumption: missing exact {field}")
    if detail.get("package") != {"kind": "local", "value": str(project)} or detail.get("execution_intent") != "background":
        raise RuntimeError("index resumption: wrong package or execution intent")
    if detail.get("state", {}).get("state") not in ("accepted", "active", "published"):
        raise RuntimeError("index resumption: missing, failed or uncertain operation is not success")
    if expected is not None:
        for field in ("operation_key", "request_digest", "package", "execution_intent"):
            if detail.get(field) != expected.get(field):
                raise RuntimeError(f"index resumption: changed {field}")
    return detail


def require_index_wait_stop(answer: dict, project: Path) -> dict:
    if answer.get("exit") != 75:
        raise RuntimeError(f"zero-wait index: expected incomplete-wait exit 75, got {answer}")
    detail = require_index_identity(answer, project)
    if detail["state"]["state"] not in ("accepted", "active"):
        raise RuntimeError("zero-wait index: fixture must still be pending when the first caller leaves")
    return detail


def require_same_index_receipt(answer: dict, project: Path, expected: dict, profile_count: int) -> None:
    require_completed_index(answer, profile_count)
    detail = require_index_identity(answer, project, expected)
    if detail != expected:
        raise RuntimeError("index resumption: terminal receipt or source capture changed")


def require_mcp_index_receipt(reply: dict, project: Path, expected: dict, profile_count: int) -> None:
    if not isinstance(reply, dict) or reply.get("isError", False):
        raise RuntimeError(f"MCP index lookup: expected successful tool result, got {reply}")
    body = reply.get("structuredContent", {})
    projection = body.get("surface", {})
    if projection.get("result") != "index-operation-status" or body.get("fault") is not None:
        raise RuntimeError(f"MCP index lookup: wrong typed surface projection: {body}")
    require_same_index_receipt({"exit": 0, "json": {"index_operation": projection.get("data")}},
                               project, expected, profile_count)


def run_mcp_wait_refusals(owner: Owner, project: Path, terminal: dict, profile_count: int) -> dict:
    # Each invalid call runs through the shipped MCP process, never a private parser.
    times = {}
    for field, value in (("wait", True), ("wait_timeout", 30), ("wait-timeout", 30)):
        session, seconds = run_mcp(owner, project,
            [{"tool": "backend.index", "arguments": {"path": str(project), field: value}},
             {"tool": "backend.index_progress", "arguments": {"operation_key": terminal["operation_key"]}}], 10)
        if session["exit"] != 0 or len(session["calls"]) != 2:
            raise RuntimeError(f"MCP {field} refusal: session failed")
        reply = session["calls"][0]["reply"]
        fault = reply.get("structuredContent") if isinstance(reply, dict) else None
        if (not isinstance(reply, dict) or reply.get("isError") is not True
            or not isinstance(fault, dict) or fault.get("answer") != "fault"
            or fault.get("slug") != "usage" or fault.get("cause") != "malformed"
            or fault.get("operand") != field
            or f"takes no argument called `{field}`" not in str(fault.get("detail", ""))):
            raise RuntimeError(f"MCP {field}: expected exact unknown-argument usage fault, got {reply}")
        # A valid call in the same session must still observe the original receipt.
        require_mcp_index_receipt(session["calls"][1]["reply"], project, terminal, profile_count)
        times[f"MCP refused CLI-only {field}"] = round(seconds, 3)
    log("MCP: three CLI-only wait spellings refused; same-session progress preserves complete receipt")
    return times


def run_resumable_index(owner: Owner, project: Path, profile_count: int, timeout: float) -> tuple[dict, dict]:
    started = time.monotonic()
    # Exercise the installed alias and a caller deadline without terminating the owner.
    first, seconds = run_cli(owner, project,
        ["index", str(project), "--execution-intent", "background", "--wait", "--wait-timeout", "0"], timeout)
    initial = require_index_wait_stop(first, project)
    log(f"background zero-wait: {initial['state']['state']} after {seconds:.3f}s")
    times = {"background index zero-wait": round(seconds, 3)}
    key = initial["operation_key"]
    progress, seconds = run_cli(owner, project, ["index_progress", "--operation-key", key], 10)
    require_cli_success("index progress after caller deadline", progress)
    require_index_identity(progress, project, initial)
    times["index progress after caller deadline"] = round(seconds, 3)
    completed, seconds = run_cli(owner, project,
        ["add", str(project), "--execution-intent", "background", "--wait"], timeout)
    log(f"resumed wait: exit={completed['exit']}; checking complete native publication")
    require_completed_index(completed, profile_count)
    terminal = require_index_identity(completed, project, initial)
    times["resumed index wait"] = round(seconds, 3)
    # Preserve the existing setup budget, now covering start + observation + resumed wait.
    times["cli add"] = round(time.monotonic() - started, 2)
    for name, args in (
        ("completed CLI operation lookup", ["index_progress", "--operation-key", key]),
        ("completed CLI keyed replay", ["index_start", str(project), "--execution-intent", "background", "--operation-key", key]),
    ):
        answer, seconds = run_cli(owner, project, args, 10)
        log(f"{name}: checking terminal receipt")
        require_same_index_receipt(answer, project, terminal, profile_count)
        times[name] = round(seconds, 3)
    session, seconds = run_mcp(owner, project,
        [{"tool": "backend.index_progress", "arguments": {"operation_key": key}}], 10)
    if session["exit"] != 0 or len(session["calls"]) != 1:
        raise RuntimeError("completed MCP operation lookup: session failed")
    reply = session["calls"][0]["reply"]
    log("completed MCP operation lookup: checking typed terminal receipt")
    require_mcp_index_receipt(reply, project, terminal, profile_count)
    times["completed MCP operation lookup"] = round(seconds, 3)
    times.update(run_mcp_wait_refusals(owner, project, terminal, profile_count))
    log("background index: zero-wait caller left; fresh alias/add clients retained one key; explicit keyed replay; CLI/MCP terminal receipts agree")
    return completed, times


def require_live_health(summary: dict, full: dict, witness: dict, published_root: str | None = None) -> None:
    """Whole-owner CLI and MCP status must agree on public identity and coverage."""
    if published_root is not None and (summary.get("readiness") != "ready" or summary.get("revision") != published_root):
        raise RuntimeError("completed index: health must certify the published receipt as ready")
    for name, detail, value in (("summary", "summary", summary), ("full", "full", full), ("MCP", "full", witness)):
        if value.get("answer") != "status" or value.get("detail") != detail:
            raise RuntimeError(f"health {name}: wrong answer/detail: {value}")
        if value.get("availability") != "available" or value.get("publication") != "populated":
            raise RuntimeError(f"health {name}: indexed owner is not available/populated: {value}")
        if not isinstance(value.get("readiness"), str) or not value["readiness"]:
            raise RuntimeError(f"health {name}: missing truthful readiness")
        if not isinstance(value.get("coverage"), list) or not isinstance(value.get("capabilities"), dict):
            raise RuntimeError(f"health {name}: missing public coverage/capabilities")
        if not isinstance(value.get("rows"), int) or value["rows"] <= 0:
            raise RuntimeError(f"health {name}: indexed owner has no visible rows")
        revision = value.get("revision", "")
        if not isinstance(revision, str) or not re.fullmatch(r"[a-f0-9]{64}", revision):
            raise RuntimeError(f"health {name}: revision is not an exact identity")
        if value.get("revision_tag") != revision[:8] or revision != witness.get("revision"):
            raise RuntimeError(f"health {name}: revision {revision} disagrees with MCP status {witness.get('revision')}")
        for field in ("readiness", "coverage", "rows", "publication"):
            if value.get(field) != witness.get(field):
                raise RuntimeError(f"health {name}: {field} disagrees with the same-scope MCP status")
    if "source" in summary or "source_tag" in summary:
        raise RuntimeError("health summary exposed full-only source fields")
    for name, value in (("full", full), ("MCP", witness)):
        source = value.get("source", "")
        if not isinstance(source, str) or not re.fullmatch(r"[a-f0-9]{64}", source):
            raise RuntimeError(f"health {name}: source is not an exact identity")
        if value.get("source_tag") != source[:8] or source != witness.get("source"):
            raise RuntimeError(f"health {name}: source {source} disagrees with MCP status {witness.get('source')}")


def require_bounded_search(reference: dict, bounded: dict, limit: int) -> None:
    if reference.get("answer") != "records" or bounded.get("answer") != "records":
        raise RuntimeError("bounded search: expected records answers")
    rows = reference.get("records")
    if not isinstance(rows, list) or len(rows) < 2 or reference.get("more") is not False:
        raise RuntimeError("bounded search: reference must contain multiple complete results")
    if bounded.get("query") != reference.get("query"):
        raise RuntimeError("bounded search: changed query")
    if bounded.get("records") != rows[:limit]:
        raise RuntimeError(f"bounded search: limit {limit} lost rank order or returned the wrong rows")
    if bounded.get("more") is not (len(rows) > limit):
        raise RuntimeError(f"bounded search: limit {limit} has an untruthful completeness flag")


def walk_search_pages(reference: dict, fetch_page, limit: int = 1) -> tuple[int, str]:
    """Consume opaque cursors through real clients; refuse loss, repetition or drift."""
    expected = reference.get("records")
    if reference.get("more") is not False or not isinstance(expected, list) or len(expected) < 2:
        raise RuntimeError("continuation: expected a complete multi-result reference")
    cursor = None
    first_cursor = None
    seen = set()
    offset = 0
    started = time.monotonic()
    for count in range(1, len(expected) + 2):
        if time.monotonic() - started > 30:
            raise RuntimeError("continuation: exceeded the traversal deadline")
        page = fetch_page(cursor)
        if time.monotonic() - started > 30:
            raise RuntimeError("continuation: exceeded the traversal deadline")
        if page.get("answer") != "records":
            raise RuntimeError("continuation: expected a records page")
        for field in ("query", "coverage", "readiness"):
            if page.get(field) != reference.get(field):
                raise RuntimeError(f"continuation: changed {field} between pages")
        rows = page.get("records")
        if not isinstance(rows, list) or not rows or rows != expected[offset:offset + limit]:
            raise RuntimeError("continuation: missing, duplicated, reordered or unbounded rows")
        offset += len(rows)
        more = offset < len(expected)
        if page.get("more") is not more:
            raise RuntimeError("continuation: incorrect completeness flag")
        token = page.get("nextCursor")
        if not more:
            if token is not None:
                raise RuntimeError("continuation: exhausted page still offers a cursor")
            return count, first_cursor
        if not isinstance(token, str) or not token or token in seen:
            raise RuntimeError("continuation: missing or repeated cursor")
        seen.add(token)
        first_cursor = first_cursor or token
        cursor = token
    raise RuntimeError("continuation: did not exhaust the reference")


def require_malformed_cursor_refusal(cli: dict, mcp: dict) -> None:
    fault = cli.get("json")
    if (cli.get("exit") == 0 or not isinstance(fault, dict)
        or fault.get("answer") != "fault" or fault.get("slug") != "protocol"
        or fault.get("cause") != "unproven"
        or str(fault.get("operand", "")).lstrip("-") != "cursor"
        or "continuation token" not in str(fault.get("detail", "")).lower()):
        raise RuntimeError(f"malformed CLI cursor: expected a typed cursor refusal, got {cli}")
    fault = mcp.get("structuredContent") if isinstance(mcp, dict) else None
    if (not isinstance(mcp, dict) or mcp.get("isError") is not True
        or not isinstance(fault, dict) or fault.get("answer") != "fault"
        or fault.get("slug") != "usage" or fault.get("cause") != "malformed"
        or fault.get("operand") != "arguments"
        or "cursor" not in str(fault.get("detail", "")).lower()):
        raise RuntimeError(f"malformed MCP cursor: expected a structured usage refusal, got {mcp}")


def run_live_contracts(owner: Owner, project: Path, config: dict, published_root: str) -> dict:
    times = {}

    def query(name: str, arguments: list[str]) -> dict:
        answer, seconds = run_cli(owner, project, arguments, 10)
        require_cli_success(name, answer)
        times[name] = round(seconds, 3)
        return answer["json"]

    summary = query("live health summary", ["--passive", "health"])
    full = query("live health full", ["--passive", "--detail", "full", "health"])
    mcp, seconds = run_mcp(owner, project, [{"tool": "backend.status", "arguments": {"detail": "full"}}], 10)
    times["live health MCP status"] = round(seconds, 3)
    if mcp["exit"] != 0 or len(mcp["calls"]) != 1:
        raise RuntimeError(f"MCP live status failed: {mcp}")
    reply = mcp["calls"][0]["reply"]
    if not isinstance(reply, dict) or reply.get("isError", False) or "structuredContent" not in reply:
        raise RuntimeError(f"MCP live status failed: {reply}")
    require_live_health(summary, full, reply["structuredContent"], published_root)
    print("surface-contract: live health summary/full agree with MCP status identity and coverage", flush=True)
    text = config["bounded_search_query"]
    reference = query("bounded search reference", ["search", text, "--limit", "200"])
    # The fixture authors two lowercase language methods at these public paths.
    methods = {
        (row.get("identity", {}).get("path"), row.get("language"))
        for row in reference.get("records", [])
        if row.get("identity", {}).get("name") == "language" and row.get("kind") == "function"
    }
    if not {("src/Duke.java", "java"), ("src/main.cpp", "cpp")}.issubset(methods):
        raise RuntimeError(f"bounded search: reference lost authored Java/C++ language methods: {methods}")
    for limit in (1, 3):
        bounded = query(f"bounded search limit {limit}", ["--limit", str(limit), "search", text])
        require_bounded_search(reference, bounded, limit)
        print(f"surface-contract: search limit {limit} preserves rank/completeness", flush=True)

    def cli_page(cursor):
        args = ["--limit", "1", "search", text]
        if cursor is not None:
            args += ["--cursor", cursor]
        answer, _ = run_cli(owner, project, args, 10)
        require_cli_success("CLI search continuation", answer)
        return answer["json"]

    def mcp_page(cursor):
        arguments = {"query": text, "limit": 1}
        if cursor is not None:
            arguments["cursor"] = cursor
        session, _ = run_mcp(owner, project, [{"tool": "backend.search", "arguments": arguments}], 10)
        if session["exit"] != 0 or len(session["calls"]) != 1:
            raise RuntimeError("MCP search continuation: session failed")
        reply = session["calls"][0]["reply"]
        if not isinstance(reply, dict) or reply.get("isError", False) or not isinstance(reply.get("structuredContent"), dict):
            raise RuntimeError(f"MCP search continuation failed: {reply}")
        return reply["structuredContent"]

    for name, fetch_page in (("CLI", cli_page), ("MCP", mcp_page)):
        started = time.monotonic()
        count, _ = walk_search_pages(reference, fetch_page)
        times[f"{name} search continuation"] = round(time.monotonic() - started, 3)
        print(f"surface-contract: {name} search exhausted {count} pages in rank order, no lost/duplicate rows", flush=True)

    refused_cli, seconds = run_cli(owner, project, ["--limit", "1", "search", text, "--cursor", "pc1-not-hex"], 10)
    times["malformed CLI cursor"] = round(seconds, 3)
    refused_mcp, seconds = run_mcp(owner, project, [{"tool": "backend.search", "arguments": {"query": text, "limit": 1, "cursor": "mcp1-invalid"}}], 10)
    times["malformed MCP cursor"] = round(seconds, 3)
    if refused_mcp["exit"] != 0 or len(refused_mcp["calls"]) != 1:
        raise RuntimeError("malformed MCP cursor: session failed instead of refusing invalid parameters")
    require_malformed_cursor_refusal(refused_cli, refused_mcp["calls"][0]["reply"])
    require_live_health(query("health after cursor refusals", ["--passive", "health"]), full, full, published_root)
    print("surface-contract: malformed CLI/MCP cursors refused; owner remains ready at published revision", flush=True)
    return times


def run_case(case: dict, bin_dir: Path) -> dict:
    case_root = SCRATCH / case["name"]
    if case_root.exists():
        shutil.rmtree(case_root)
    case_root.mkdir(parents=True, mode=0o700)
    project = case_root / "project"
    shutil.copytree(ROOT / case["project"], project)
    timeout = case.get("timeout_seconds", 300)
    owner = Owner(bin_dir, case_root)
    record = {"answers": {}, "performance": {"seconds": {}}}
    try:
        answer, index_times = run_resumable_index(owner, project, len(case["languages"]), timeout)
        published_root = require_completed_index(answer, len(case["languages"]))
        log(f"index terminal receipt: published {len(case['languages'])} complete native profiles; view {published_root}")
        # Indexing is setup: require success above, then snapshot the verdict.
        # The caller journal embeds fresh random operation/receipt identities;
        # search/outline/packages below snapshot the published product state.
        record["answers"]["cli add"] = {"exit": answer["exit"]}
        record["performance"]["seconds"].update(index_times)
        for args in case.get("cli", []):
            key = "cli " + " ".join(args)
            answer, seconds = run_cli(owner, project, args, timeout)
            require_cli_success(key, answer)
            if args[0] == "search" and not answer["json"].get("records"):
                raise RuntimeError(f"{key}: expected fixture symbol, got no records")
            record["answers"][key] = answer
            record["performance"]["seconds"][key] = round(seconds, 2)
        if case.get("mcp"):
            answer, seconds = run_mcp(owner, project, case["mcp"], timeout)
            if answer["exit"] != 0 or not answer["tools"]:
                raise RuntimeError(f"MCP session failed: {answer}")
            for result in answer["calls"]:
                reply = result["reply"]
                if not isinstance(reply, dict) or reply.get("isError", False) or "code" in reply:
                    raise RuntimeError(f"MCP call failed: {result}")
                if result["call"]["tool"] == "backend.search" and not reply.get("structuredContent", {}).get("records"):
                    raise RuntimeError(f"MCP search returned no fixture symbols: {result}")
            record["answers"]["mcp"] = answer
            record["performance"]["seconds"]["mcp"] = round(seconds, 2)
        if case.get("live_contracts"):
            record["performance"]["seconds"].update(
                run_live_contracts(owner, project, case["live_contracts"], published_root)
            )
        record["performance"].update(owner.usage())
    finally:
        owner.stop()
    record["answers"] = stable(record["answers"])
    return record


def regressions(baseline: dict, measured: dict) -> list[str]:
    found = []

    def check(metric: str, name: str, old: float, new: float) -> None:
        if new > max(old * TOLERANCE[metric], FLOOR[metric]):
            found.append(f"{name}: {new} vs baseline {old} (allowed x{TOLERANCE[metric]})")

    for name, old in baseline.get("seconds", {}).items():
        if name in measured["seconds"]:
            check("seconds", f"seconds[{name}]", old, measured["seconds"][name])
    for metric in ("peak_rss_kib", "cpu_seconds"):
        if metric in baseline and metric in measured:
            check(metric, metric, baseline[metric], measured[metric])
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--case", action="append", help="run only these cases (default: all)")
    parser.add_argument("--update", action="store_true", help="rewrite snapshots and baselines for review")
    parser.add_argument("--report", type=Path, help="write measured performance here as JSON")
    parser.add_argument(
        "--print-missing",
        action="store_true",
        help="print the would-be snapshot of a case that has none, for review in CI logs",
    )
    args = parser.parse_args()
    # Fixed paths require exclusive ownership; another local run must never
    # remove an active case workspace. Concourse tasks have separate /tmp.
    SCRATCH.mkdir(parents=True, exist_ok=True, mode=0o700)
    lock = open(SCRATCH / ".lock", "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        parser.error("another surface run owns the fixed scratch paths")
    cases = json.loads((SURFACE / "cases.json").read_text())["cases"]
    if any(not re.fullmatch(r"[a-z0-9][a-z0-9_-]*", case["name"]) for case in cases):
        parser.error("case names must be simple scratch directory names")
    if args.case:
        unknown = set(args.case) - {case["name"] for case in cases}
        if unknown:
            parser.error(f"unknown cases: {sorted(unknown)}")
        cases = [case for case in cases if case["name"] in args.case]
    failed = []
    report = {}
    for case in cases:
        log(f"case {case['name']}")
        started = time.monotonic()
        if case.get("kind") == "cli-contract":
            from cli_contract import run_cases

            try:
                report[case["name"]] = run_cases(args.bin_dir.resolve())
                log(f"case {case['name']} passed ({time.monotonic() - started:.1f}s)")
            except Exception as error:
                log(f"case {case['name']} failed: {error}")
                failed.append(case["name"])
            continue
        try:
            record = run_case(case, args.bin_dir.resolve())
        except Exception as error:  # noqa: BLE001 - one broken case must not hide the rest
            log(f"case {case['name']} could not run: {error}")
            failed.append(case["name"])
            continue
        report[case["name"]] = record["performance"]
        snapshot_path = SURFACE / "snapshots" / f"{case['name']}.json"
        if args.update:
            snapshot_path.parent.mkdir(parents=True, exist_ok=True)
            snapshot_path.write_text(json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
            log(f"case {case['name']} snapshot written ({time.monotonic() - started:.1f}s)")
            continue
        if not snapshot_path.exists():
            log(f"case {case['name']} has no reviewed snapshot; run with --update and commit it")
            if args.print_missing:
                print(f"SURFACE-SNAPSHOT-BEGIN {case['name']}")
                print(json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False))
                print(f"SURFACE-SNAPSHOT-END {case['name']}")
            failed.append(case["name"])
            continue
        reviewed = json.loads(snapshot_path.read_text())
        problems = []
        if reviewed["answers"] != record["answers"]:
            old = json.dumps(reviewed["answers"], indent=2, sort_keys=True, ensure_ascii=False).splitlines()
            new = json.dumps(record["answers"], indent=2, sort_keys=True, ensure_ascii=False).splitlines()
            diff = "\n".join(difflib.unified_diff(old, new, "reviewed", "this change", lineterm="", n=2))
            problems.append(f"answers changed; explain or update the snapshot:\n{diff[:20000]}")
        problems += regressions(reviewed.get("performance", {}), record["performance"])
        if problems:
            failed.append(case["name"])
            for problem in problems:
                log(f"case {case['name']}: {problem}")
        else:
            log(f"case {case['name']} matches ({time.monotonic() - started:.1f}s, "
                f"peak {record['performance'].get('peak_rss_kib', 0) // 1024} MiB)")
    if args.report:
        args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    if failed:
        log(f"failed: {', '.join(failed)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
