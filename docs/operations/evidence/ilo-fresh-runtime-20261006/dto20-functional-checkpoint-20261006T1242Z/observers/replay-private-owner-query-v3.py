#!/usr/bin/env python3
"""Capture locald startup/reindex diagnostics from a copied failing store."""
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
parser.add_argument("--run", type=Path, required=True)
parser.add_argument("--state-source", type=Path, help="Clone a retained state using the original source/setup binding")
parser.add_argument("--skip-repeat", action="store_true")
parser.add_argument("--clear-cache", action="store_true")
parser.add_argument("--cache-source", type=Path)
parser.add_argument("--query", action="append", default=[])
parser.add_argument("--manifest", type=Path)
parser.add_argument("--resolve-query")
parser.add_argument("--witness-path")
parser.add_argument("--witness-line", type=int)
parser.add_argument("--resolve-limit", type=int, default=32)
args = parser.parse_args()
original = json.loads((args.run / "setup.json").read_text())
manifest_path = args.manifest or Path(original["runtime_manifest"])
manifest = json.loads(manifest_path.read_text())
def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()
assert digest(manifest["root_receipt"]["path"]) == manifest["root_receipt"]["sha256"]
binaries = {}
for name in ["backend-locald", "backend-cli", "backend-mcp"]:
    claim = manifest["executables"][name]
    image = Path(claim["path"])
    assert image.is_file() and not image.is_symlink()
    assert image.stat().st_size == claim["bytes"] and digest(image) == claim["sha256"]
    binaries[name] = str(image)
root = args.run.parent / (args.run.name + "-diagnostic-" + uuid.uuid4().hex[:8])
root.mkdir(mode=0o700)
state, home, tmp = [root / word for word in ["state", "home", "tmp"]]
state_source = args.state_source or args.run / "state"
shutil.copytree(state_source, state)
assert not (args.clear_cache and args.cache_source)
cache = state / "search-index-v2"
if args.clear_cache or args.cache_source:
    shutil.rmtree(cache)
if args.cache_source:
    shutil.copytree(args.cache_source, cache)
home.mkdir()
tmp.mkdir()
env = dict(original["env"])
env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"), XDG_DATA_HOME=str(home / ".local/share"), XDG_STATE_HOME=str(home / ".local/state"), TMPDIR=str(tmp))
project = args.run / "project"
endpoint = "/tmp/nudox-diagnostic-" + uuid.uuid4().hex[:16] + ".sock"
common = ["--project", str(project), "--workspace", str(state), "--endpoint", endpoint]
out, err = (root / "locald.stdout").open("wb"), (root / "locald.stderr").open("wb")
argv = [binaries["backend-locald"], "--workspace", str(state), "--endpoint", endpoint, "--idle-timeout-ms", "0"]
proc = subprocess.Popen(argv, cwd=project, env=env, stdout=out, stderr=err, start_new_session=True)
receipt = {"schema": "nudox.private-owner-clone-diagnostic.v1", "original_run": str(args.run), "original_state_mutated": False, "cache_cleared_in_clone": args.clear_cache, "authentic_cache_transplant_source": str(args.cache_source) if args.cache_source else None, "diagnostic_root": str(root), "owner_argv": argv, "owner_pid": proc.pid, "env": env, "commands": [], "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
receipt.update(candidate_manifest_path=str(manifest_path), candidate_manifest_sha256=digest(manifest_path), candidate_manifest=manifest, original_project_source_hash=original["source_before"]["sha256"])
receipt["cloned_state_source"] = str(state_source)
try:
    owner_ready = False
    for attempt in range(600):
        if proc.poll() is not None:
            break
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            connection.connect(endpoint)
            owner_ready = True
            break
        except (FileNotFoundError, ConnectionRefusedError):
            pass
        finally:
            connection.close()
        time.sleep(.1)
    receipt["owner_socket_ready_before_cli"] = owner_ready
    if owner_ready:
        commands = [("clone-health", ["--passive", "health"])]
        if not args.skip_repeat:
            commands.append(("clone-repeat-add", ["add", str(project)]))
        commands.extend(("clone-search-" + query.replace("/", "_"), ["search", query, "--limit", "10"]) for query in args.query)
        commands.append(("clone-after-health", ["--passive", "health"]))
        def cli(label, words, detail="full"):
            cli_argv = [binaries["backend-cli"], "--json", "--detail", detail, *common, *words]
            start = time.monotonic()
            client = subprocess.run(cli_argv, env=env, cwd=project, capture_output=True, timeout=120, start_new_session=True)
            (root / (label + ".stdout")).write_bytes(client.stdout)
            (root / (label + ".stderr")).write_bytes(client.stderr)
            receipt["commands"].append({"label": label, "argv": cli_argv, "exit": client.returncode, "seconds": time.monotonic() - start, "stdout_sha256": hashlib.sha256(client.stdout).hexdigest(), "stderr_sha256": hashlib.sha256(client.stderr).hexdigest(), "stdout": client.stdout.decode(errors="replace"), "stderr": client.stderr.decode(errors="replace")})
            try:
                return json.loads(client.stdout)
            except ValueError:
                return None
        for label, words in commands:
            cli(label, words)
        if args.resolve_query:
            cli("clone-full-resolve", ["resolve", args.resolve_query, "--limit", "200"])
            assert 1 <= args.resolve_limit <= 65535
            summary = cli("clone-summary-resolve", ["resolve", args.resolve_query, "--limit", str(args.resolve_limit)], "summary") or {}
            matches = [row for row in summary.get("records", []) if row.get("identity", {}).get("path") == args.witness_path and (not args.witness_line or row.get("identity", {}).get("line") == args.witness_line)]
            receipt["resolved_source_witnesses"] = matches
            if len(matches) == 1:
                coordinate = matches[0]["identity"]["coordinate"]
                for command in ["source", "show", "references", "graph"]:
                    cli("clone-summary-selected-" + command, [command, coordinate])
            else:
                receipt["summary_resolution_witness_refused"] = {"matches": len(matches), "requested_path": args.witness_path, "requested_line": args.witness_line}
finally:
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(timeout=20)
    receipt["owner_exit"] = proc.returncode
    out.close()
    err.close()
    receipt["owner_stderr"] = (root / "locald.stderr").read_text(errors="replace")
    receipt["owner_stderr_sha256"] = hashlib.sha256((root / "locald.stderr").read_bytes()).hexdigest()
    receipt["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    (root / "diagnostic.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({key: receipt[key] for key in ["diagnostic_root", "owner_exit", "owner_stderr", "owner_stderr_sha256"]}), flush=True)
