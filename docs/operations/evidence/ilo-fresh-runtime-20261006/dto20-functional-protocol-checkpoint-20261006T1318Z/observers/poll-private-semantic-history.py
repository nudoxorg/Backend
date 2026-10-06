#!/usr/bin/env python3
"""Observe eventual derived history on an immutable real-state copy."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("--run", required=True, type=Path)
parser.add_argument("--manifest", required=True, type=Path)
parser.add_argument("--budget-seconds", type=int, default=60)
args = parser.parse_args()
assert 1 <= args.budget_seconds <= 300

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def available_memory():
    return int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:")))

def owner_resources():
    status = (Path("/proc") / str(owner.pid) / "status").read_text()
    values = {line.split(":", 1)[0]: line.split(":", 1)[1].strip() for line in status.splitlines() if ":" in line}
    stat = (Path("/proc") / str(owner.pid) / "stat").read_text().rpartition(") ")[2].split()
    ticks = os.sysconf("SC_CLK_TCK")
    return {"rss_kib": int(values.get("VmRSS", "0 kB").split()[0]), "threads": int(values["Threads"]), "cpu_seconds": (int(stat[11]) + int(stat[12])) / ticks, "available_memory_kib": available_memory()}

setup = json.loads((args.run / "setup.json").read_text())
manifest = json.loads(args.manifest.read_text())
assert sha(manifest["root_receipt"]["path"]) == manifest["root_receipt"]["sha256"]
images = {}
for name, claim in manifest["executables"].items():
    path = Path(claim["path"])
    assert path.is_file() and not path.is_symlink()
    assert sha(path) == claim["sha256"] and path.stat().st_size == claim["bytes"]
    images[name] = str(path)
memory_start = available_memory()
assert memory_start >= 24 * 1024 * 1024
root = args.run.parent / (args.run.name + "-history-poll-" + uuid.uuid4().hex[:8])
root.mkdir(mode=0o700)
state, home, tmp = [root / name for name in ["state", "home", "tmp"]]
shutil.copytree(args.run / "state", state)
home.mkdir()
tmp.mkdir()
project = args.run / "project"
endpoint = "/tmp/nudox-history-poll-" + uuid.uuid4().hex[:16] + ".sock"
env = dict(setup["env"])
env.update(HOME=str(home), TMPDIR=str(tmp), XDG_CONFIG_HOME=str(home / ".config"), XDG_DATA_HOME=str(home / ".local/share"), XDG_STATE_HOME=str(home / ".local/state"))
record = {"schema": "nudox.actual-eventual-semantic-history.v1", "root": str(root), "original_run": str(args.run), "original_state_mutated": False, "manifest": manifest, "manifest_sha256": sha(args.manifest), "observer_sha256": sha(__file__), "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "budget_seconds": args.budget_seconds, "memory_start_kib": memory_start, "selected_head_before": sha(state / "objects/HEAD"), "commands": [], "whole_project_pass": False}
owner_out, owner_err = [(root / ("locald." + suffix)).open("wb") for suffix in ["stdout", "stderr"]]
owner_argv = [images["backend-locald"], "--workspace", str(state), "--endpoint", endpoint, "--idle-timeout-ms", "0"]
owner = subprocess.Popen(owner_argv, cwd=project, env=env, stdout=owner_out, stderr=owner_err, start_new_session=True)
record.update(owner_pid=owner.pid, owner_argv=owner_argv)

def cli(words, label):
    argv = [images["backend-cli"], "--json", "--detail", "full", "--project", str(project), "--workspace", str(state), "--endpoint", endpoint, *words]
    start = time.monotonic()
    process = subprocess.Popen(argv, cwd=project, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    try:
        out, err = process.communicate(timeout=30)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        out, err = process.communicate(timeout=5)
    (root / (label + ".stdout")).write_bytes(out)
    (root / (label + ".stderr")).write_bytes(err)
    try:
        payload = json.loads(out)
    except ValueError:
        payload = None
    row = {"label": label, "argv": argv, "exit": process.returncode, "seconds": time.monotonic() - start, "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "stdout_sha256": hashlib.sha256(out).hexdigest(), "stderr_sha256": hashlib.sha256(err).hexdigest(), "payload": payload}
    record["commands"].append(row)
    return row

try:
    ready = False
    for _ in range(600):
        if owner.poll() is not None:
            break
        connection = socket.socket(socket.AF_UNIX)
        try:
            connection.connect(endpoint)
            ready = True
            break
        except (FileNotFoundError, ConnectionRefusedError):
            pass
        finally:
            connection.close()
        time.sleep(.1)
    record["owner_ready"] = ready
    assert ready, "the private owner never accepted a connection"
    cli(["--passive", "health"], "cold-health")
    started = time.monotonic()
    deadline = started + args.budget_seconds
    poll = 0
    while time.monotonic() < deadline:
        poll += 1
        reply = cli(["semantic-versions", str(project)], "history-" + str(poll))
        statuses = [row.get("history_status") for row in (reply["payload"] or {}).get("records", []) if row.get("history_status")]
        elapsed = time.monotonic() - started
        reply["owner_resources"] = owner_resources()
        print(json.dumps({"root": str(root), "poll": poll, "elapsed_seconds": elapsed, "exit": reply["exit"], "history_statuses": statuses, "owner_resources": reply["owner_resources"]}), flush=True)
        record["latest_history_statuses"] = statuses
        if reply["exit"] != 0 or (statuses and all(row.get("state") != "pending" for row in statuses)):
            record["terminal_observed"] = True
            break
        time.sleep(min(2, max(0, deadline - time.monotonic())))
    record.update(polls=poll, elapsed_seconds=time.monotonic() - started)
    cli(["--passive", "health"], "after-poll-health")
except Exception as error:
    record["observer_error"] = {"type": type(error).__name__, "message": str(error)}
finally:
    if owner.poll() is None:
        os.killpg(owner.pid, signal.SIGTERM)
        owner.wait(timeout=20)
    owner_out.close()
    owner_err.close()
    record.update(owner_exit=owner.returncode, selected_head_unchanged=sha(state / "objects/HEAD") == record["selected_head_before"], memory_end_kib=available_memory(), owner_stderr_sha256=sha(root / "locald.stderr"), finished_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
    target = root / "history-poll-receipt.json"
    target.write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({"receipt": str(target), "sha256": sha(target), "terminal_observed": record.get("terminal_observed", False), "observer_error": record.get("observer_error"), "latest_history_statuses": record.get("latest_history_statuses")}), flush=True)
