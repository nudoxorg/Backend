#!/usr/bin/env python3
"""Public-route harness for C# repositories.

Starts an isolated owner (`backend-locald`) with a private workspace and drives
the real `backend-cli` and `backend-mcp` binaries against a copy of a fixture.
No product internals are imported; everything is observed through the same
routes a user has. The harness owns every process it starts and removes its
scratch directory.

This module only supplies the plumbing (`Journey`). The scenarios live in
`csharp_aspnet_acceptance.py`.
"""
import json
import os
import pathlib
import shutil
import signal
import subprocess
import tempfile
import time

MAX_OUTPUT = 16 * 1024 * 1024


class Journey:
    def __init__(self, bin_dir, output, env_extra=None, label="journey"):
        self.bin = pathlib.Path(bin_dir).resolve(strict=True)
        for name in ("backend-cli", "backend-mcp", "backend-locald"):
            path = self.bin / name
            if not (path.is_file() and os.access(path, os.X_OK)):
                raise SystemExit(f"missing executable {path}")
        self.out = pathlib.Path(output).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.label = label
        self.root = pathlib.Path(tempfile.mkdtemp(prefix="ndx-cs-", dir=os.environ.get("TMPDIR", "/tmp")))
        os.chmod(self.root, 0o700)
        self.env = {k: os.environ[k] for k in ("PATH", "LANG", "LC_ALL", "TZ", "NUDOX_ACCOUNT_GATE") if k in os.environ}
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "TMPDIR"):
            self.env[key] = str(self.root / key)
            pathlib.Path(self.env[key]).mkdir(mode=0o700)
        self.env.update(env_extra or {})
        self.workspace = self.root / "state"
        self.workspace.mkdir(mode=0o700)
        self.secret = self.workspace / "authority.secret"
        self.secret.write_bytes(os.urandom(32))
        os.chmod(self.secret, 0o600)
        self.endpoint = self.root / "owner.sock"
        self.owner = None
        self.mcp = None
        self.records = []
        self.seq = 0
        self.project = None
        self.counter = 0

    # -- fixture -----------------------------------------------------------
    def stage_fixture(self, source, name="project"):
        target = self.root / name
        shutil.copytree(source, target)
        return target

    # -- owner -------------------------------------------------------------
    def start_owner(self, cwd):
        env = dict(self.env)
        env["BACKEND_LOCALD_AUTHORITY_SECRET_FILE"] = str(self.secret)
        if self.endpoint.exists():
            self.endpoint.unlink()
        self.counter += 1
        log = (self.out / f"{self.label}-owner-{self.counter}.log").open("wb")
        self.owner = subprocess.Popen(
            [str(self.bin / "backend-locald"), "--endpoint", str(self.endpoint),
             "--workspace", str(self.workspace), "--authority-secret-file", str(self.secret),
             "--registry-offline", "--registry-discovery-offline", "--advisory-offline", "--forge-offline"],
            cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True)
        deadline = time.monotonic() + float(os.environ.get("CSHARP_ROUTE_OWNER_START_SECONDS", "900"))
        while not self.endpoint.exists():
            if self.owner.poll() is not None:
                raise RuntimeError(f"owner exited {self.owner.returncode} before its endpoint appeared")
            if time.monotonic() > deadline:
                raise RuntimeError("owner startup timed out")
            time.sleep(0.05)

    def stop_owner(self):
        self._stop(self.owner)
        self.owner = None

    def _stop(self, proc):
        if proc is None:
            return
        try:
            if proc.stdin:
                try:
                    proc.stdin.close()
                except OSError:
                    pass
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    proc.wait(timeout=10)
        except Exception as error:  # noqa: BLE001 - cleanup must not mask the result
            self.records.append({"cleanup_error": repr(error)})

    # -- CLI ---------------------------------------------------------------
    def cli(self, label, args, project=None, timeout=900):
        project = project or self.project
        cmd = [str(self.bin / "backend-cli"), "--format", "json", "--project", str(project),
               "--workspace", str(self.workspace), "--endpoint", str(self.endpoint), *args]
        # A read the owner did not answer in time is documented as repeatable; a busy
        # host (a build farm) makes that common, so repeat it a bounded number of times.
        for attempt in range(6):
            done = subprocess.run(cmd, cwd=project, env=self.env, capture_output=True, timeout=timeout)
            raw = done.stdout.decode("utf-8", "replace")
            err = done.stderr.decode("utf-8", "replace")
            try:
                obj = json.loads(raw)
            except ValueError:
                obj = None
            repeatable = (done.returncode != 0 and isinstance(obj, dict) and obj.get("slug") == "endpoint"
                          and obj.get("cause") == "unreachable" and "can be repeated" in str(obj.get("detail", "")))
            if not repeatable:
                break
            time.sleep(10)
        (self.out / f"{self.label}-{label}.stdout").write_text(raw)
        (self.out / f"{self.label}-{label}.stderr").write_text(err)
        self.records.append({"label": label, "argv": cmd[1:], "exit": done.returncode})
        return done.returncode, obj, raw + "\n" + err

    # -- MCP ---------------------------------------------------------------
    def start_mcp(self, project=None):
        project = project or self.project
        self.mcp = subprocess.Popen(
            [str(self.bin / "backend-mcp"), "--project", str(project), "--workspace", str(self.workspace),
             "--endpoint", str(self.endpoint)],
            cwd=project, env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=open(self.out / f"{self.label}-mcp-{self.counter}.stderr", "wb"), start_new_session=True)
        self.seq = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": {"name": "csharp-public-route", "version": "1"}})
        self.mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.mcp.stdin.flush()

    def stop_mcp(self):
        self._stop(self.mcp)
        self.mcp = None

    def rpc(self, method, params, allow_error=False, timeout=300):
        self.seq += 1
        request = {"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params}
        self.mcp.stdin.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        self.mcp.stdin.flush()
        deadline = time.monotonic() + timeout
        import select
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.mcp.stdout], [], [], remaining)[0]:
                raise RuntimeError(f"MCP deadline for {method}")
            line = self.mcp.stdout.readline()
            if not line:
                raise RuntimeError("MCP closed its output")
            reply = json.loads(line)
            if reply.get("id") == self.seq:
                break
        self.records.append({"label": "mcp:" + method, "tool": params.get("name") if isinstance(params, dict) else None})
        if "error" in reply:
            if allow_error:
                return {"rpc_error": reply["error"]}
            raise RuntimeError("MCP RPC failure: " + json.dumps(reply)[:500])
        return reply["result"]

    def tool(self, name, arguments, allow_error=False):
        result = self.rpc("tools/call", {"name": name, "arguments": arguments}, allow_error=allow_error)
        return result

    # -- shutdown ------------------------------------------------------------
    def close(self):
        self.stop_mcp()
        self.stop_owner()
        shutil.rmtree(self.root, ignore_errors=True)
