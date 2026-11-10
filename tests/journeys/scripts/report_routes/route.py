#!/usr/bin/env python3
"""Public CLI/MCP route runner for the customer-report findings.

Runs real `backend-cli`, `backend-mcp` and `backend-locald` processes from one
build directory against a synthetic fixture, with a private HOME. Every run is
labelled with the exact source commit and binary hashes.

Debug builds skip the account gate, so a run of this script on a debug build is
DEVELOPMENT MODE: it never proves authenticated behaviour. The receipt says so.
"""
import argparse, hashlib, json, os, pathlib, select, shutil, signal, subprocess, sys, tempfile, time

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fixtures  # noqa: E402


def sha(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


class Route:
    """One private HOME, one owner, any number of CLI and MCP calls."""

    def __init__(self, bin_dir, out, auto_start=False, path_extra=None, extra_env=None):
        self.bin = pathlib.Path(bin_dir)
        self.out = pathlib.Path(out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.root = pathlib.Path(tempfile.mkdtemp(prefix="ndx-route-"))
        os.chmod(self.root, 0o700)
        self.records = []
        self.seq = 0
        self.owner = None
        self.mcp = None
        self.pending = bytearray()
        self.auto_start = auto_start
        base_path = os.environ.get("PATH", "/usr/bin:/bin")
        env = {k: os.environ[k] for k in ("LANG", "LC_ALL", "TZ") if k in os.environ}
        env["PATH"] = path_extra if path_extra is not None else base_path
        for k in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                  "XDG_RUNTIME_DIR", "TMPDIR", "CARGO_HOME"):
            env[k] = str(self.root / k)
            (self.root / k).mkdir(mode=0o700, exist_ok=True)
        # The account gate is never configured here: development mode only.
        env.update(extra_env or {})
        self.env = env
        self.workspace = self.root / "state"
        self.workspace.mkdir(mode=0o700)
        self.endpoint = self.root / "owner.sock"

    # ---- owner ---------------------------------------------------------------------
    def start_owner(self, cwd):
        if self.auto_start:
            return
        secret = self.workspace / "authority.secret"
        secret.write_bytes(os.urandom(32))
        os.chmod(secret, 0o600)
        env = dict(self.env, BACKEND_LOCALD_AUTHORITY_SECRET_FILE=str(secret))
        log = open(self.out / "owner-stderr.log", "wb")
        self.owner = subprocess.Popen(
            [str(self.bin / "backend-locald"), "--endpoint", str(self.endpoint), "--workspace",
             str(self.workspace), "--authority-secret-file", str(secret), "--registry-offline",
             "--registry-discovery-offline", "--advisory-offline", "--forge-offline"],
            cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
        deadline = time.monotonic() + 420
        while not self.endpoint.exists():
            if self.owner.poll() is not None:
                raise RuntimeError(f"locald exited {self.owner.returncode} before endpoint")
            if time.monotonic() > deadline:
                raise RuntimeError("locald startup timed out")
            time.sleep(0.05)

    def save_records(self):
        (self.out / "records.json").write_text(json.dumps(self.records, indent=1))

    def stop(self):
        try:
            self.save_records()
        except Exception:
            pass
        for proc in (self.mcp, self.owner):
            if proc is None:
                continue
            try:
                if proc.stdin:
                    proc.stdin.close()
            except Exception:
                pass
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                    proc.wait(timeout=8)
                except Exception:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except Exception:
                        pass
        # An owner the CLI started itself is found by its unique private workspace path.
        subprocess.run(["pkill", "-TERM", "-f", str(self.workspace)], check=False)
        time.sleep(0.5)
        subprocess.run(["pkill", "-KILL", "-f", str(self.workspace)], check=False)
        shutil.rmtree(self.root, ignore_errors=True)

    # ---- CLI -----------------------------------------------------------------------
    def cli(self, label, args, project, timeout=1800, json_out=True):
        cmd = [str(self.bin / "backend-cli")]
        if json_out:
            cmd += ["--format", "json"]
        cmd += ["--project", str(project), "--workspace", str(self.workspace)]
        if not self.auto_start:
            cmd += ["--endpoint", str(self.endpoint)]
        cmd += list(args)
        proc = subprocess.run(cmd, cwd=project, env=self.env, capture_output=True, timeout=timeout)
        out = proc.stdout.decode("utf-8", "replace")
        err = proc.stderr.decode("utf-8", "replace")
        (self.out / f"{label}.stdout").write_text(out)
        (self.out / f"{label}.stderr").write_text(err)
        try:
            obj = json.loads(out)
        except ValueError:
            obj = None
        self.records.append({"label": label, "argv": cmd[1:], "exit": proc.returncode})
        return proc.returncode, obj, out + "\n" + err

    # ---- MCP -----------------------------------------------------------------------
    def start_mcp(self, project):
        argv = [str(self.bin / "backend-mcp"), "--project", str(project), "--workspace", str(self.workspace)]
        if not self.auto_start:
            argv += ["--endpoint", str(self.endpoint)]
        self.mcp = subprocess.Popen(argv, cwd=project, env=self.env, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=open(self.out / "mcp-stderr.log", "wb"),
                                    start_new_session=True, bufsize=0)
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": {"name": "report-routes", "version": "1"}})
        self.mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')

    def rpc(self, method, params, timeout=120, allow_error=False):
        self.seq += 1
        req = {"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params}
        self.mcp.stdin.write(json.dumps(req, separators=(",", ":")).encode() + b"\n")
        deadline = time.monotonic() + timeout
        while b"\n" not in self.pending:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([self.mcp.stdout], [], [], left)[0]:
                raise RuntimeError("MCP read deadline")
            chunk = os.read(self.mcp.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError("MCP stdout closed")
            self.pending.extend(chunk)
        raw, _, rest = bytes(self.pending).partition(b"\n")
        self.pending = bytearray(rest)
        reply = json.loads(raw)
        self.records.append({"label": "mcp:" + method, "tool": params.get("name"), "arguments": params.get("arguments")})
        (self.out / f"mcp-{self.seq:03d}-{(params.get('name') or method).replace('/', '_')}.json").write_text(
            json.dumps(reply, indent=1))
        if "error" in reply and not allow_error:
            raise RuntimeError(f"MCP error: {reply['error']}")
        return reply.get("result", reply)

    def tool(self, name, arguments, **kw):
        return self.rpc("tools/call", {"name": name, "arguments": arguments}, **kw)

    def mcp_index(self, path, budget=1500):
        # The first reply carries the executable continuation; poll sizes are kept for the
        # payload findings.
        result = self.tool("backend.index", {"path": str(path)})
        self.poll_sizes = [len(json.dumps(result))]
        deadline = time.monotonic() + budget
        while True:
            body = result.get("structuredContent", {})
            call = body.get("index_poll", {}).get("call")
            if not call:
                return result
            if time.monotonic() > deadline:
                raise RuntimeError("MCP index timed out")
            time.sleep(0.5)
            result = self.rpc("tools/call", call)
            self.poll_sizes.append(len(json.dumps(result)))


def receipt_header(bin_dir, source_commit, mode="development"):
    bins = {}
    for name in ("backend-cli", "backend-mcp", "backend-locald"):
        p = pathlib.Path(bin_dir) / name
        bins[name] = sha(p) if p.exists() else None
    return {
        "source_commit": source_commit,
        "binaries": bins,
        "mode": "development (debug build, account gate skipped; NUDOX_ACCOUNT_GATE unset)",
        "authenticated_account_exercised": False,
        "fixture_origin": "synthetic, written for this lane; no customer data",
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenario")
    ap.add_argument("--bin-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--source-commit", required=True)
    args = ap.parse_args()
    import scenarios  # noqa: E402
    fn = scenarios.SCENARIOS[args.scenario]
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    result = fn(args.bin_dir, out)
    receipt = receipt_header(args.bin_dir, args.source_commit)
    receipt["scenario"] = args.scenario
    receipt["findings"] = result
    (out / "receipt.json").write_text(json.dumps(receipt, indent=1))
    print(json.dumps(result, indent=1))
    sys.exit(0 if all(v.get("pass") for v in result.values()) else 3)


if __name__ == "__main__":
    main()
