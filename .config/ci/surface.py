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
        if not name.startswith(("BACKEND_", "NUDOX_WORKSPACE", "NUDOX_PROJECT"))
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
        answer, seconds = run_cli(owner, project, ["add", str(project), "--wait"], timeout)
        require_cli_success("cli add", answer)
        # Indexing is setup: require success above, then snapshot the verdict.
        # The caller journal embeds fresh random operation/receipt identities;
        # search/outline/packages below snapshot the published product state.
        record["answers"]["cli add"] = {"exit": answer["exit"]}
        record["performance"]["seconds"]["cli add"] = round(seconds, 2)
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
