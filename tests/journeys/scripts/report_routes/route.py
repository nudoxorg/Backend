#!/usr/bin/env python3
"""Public CLI/MCP route runner for the customer-report findings.

Runs real `backend-cli`, `backend-mcp` and `backend-locald` processes from one
build directory against a synthetic fixture, with a private HOME. Every run is
labelled with the exact source commit and binary hashes.

Debug builds skip the account gate, so a run of this script on a debug build is
DEVELOPMENT MODE: it never proves authenticated behaviour. The receipt says so.
"""
import argparse, hashlib, json, os, pathlib, re, select, shutil, signal, stat, subprocess, sys, tempfile, time

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fixtures  # noqa: E402


def sha(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def _read_linux_process(pid):
    """Read process identity and private-environment evidence, or return None."""
    root = pathlib.Path("/proc") / str(pid)
    try:
        argv = [part.decode("utf-8", "strict") for part in (root / "cmdline").read_bytes().split(b"\0") if part]
        raw = (root / "stat").read_text()
        # comm is parenthesized and may itself contain spaces or parentheses.
        fields = raw[raw.rfind(")") + 2:].split()
        status = (root / "status").read_text()
        uid_row = next(line for line in status.splitlines() if line.startswith("Uid:"))
        uid = int(uid_row.split()[2])
        environment = {}
        for entry in (root / "environ").read_bytes().split(b"\0"):
            key, sep, value = entry.partition(b"=")
            if sep and key in (b"HOME", b"XDG_RUNTIME_DIR"):
                environment[key.decode()] = value.decode("utf-8", "strict")
        executable = os.readlink(root / "exe")
        return {"argv": argv, "start_ticks": fields[19], "pgrp": int(fields[2]),
                "session": int(fields[3]), "uid": uid,
                "environment": environment, "executable": executable}
    except (OSError, UnicodeError, IndexError, ValueError):
        return None


def _option_value(argv, option):
    for index, item in enumerate(argv):
        if item == option and index + 1 < len(argv):
            return argv[index + 1]
        if item.startswith(option + "="):
            return item[len(option) + 1:]
    return None


def _wait_pidfd(fd, timeout):
    poller = select.poll()
    poller.register(fd, select.POLLIN)
    return any(ready_fd == fd and mask & select.POLLIN
               for ready_fd, mask in poller.poll(max(0, int(timeout * 1000))))


def _private_default_endpoint(endpoint, root, runtime_dir):
    try:
        endpoint_path = pathlib.Path(endpoint).resolve(strict=False)
        root_path = pathlib.Path(root).resolve(strict=True)
        runtime_path = pathlib.Path(runtime_dir).resolve(strict=True)
        return (endpoint_path.parent == runtime_path
                and os.path.commonpath((str(root_path), str(endpoint_path))) == str(root_path)
                and re.fullmatch(r"backend-v2-[0-9a-f]{24}\.sock", endpoint_path.name) is not None)
    except (OSError, ValueError):
        return False


def _matches_detached_owner(current, expected_executable, workspace, root, home, runtime_dir):
    argv = current["argv"]
    if (not argv or argv[0] != expected_executable
            or current["executable"] != expected_executable
            or current["uid"] != os.geteuid()
            or current["environment"].get("HOME") != str(home)
            or current["environment"].get("XDG_RUNTIME_DIR") != str(runtime_dir)
            or _option_value(argv, "--workspace") != str(workspace)
            or _option_value(argv, "--authority-secret-file") != str(pathlib.Path(workspace) / "authority.secret")):
        return False
    endpoint = _option_value(argv, "--endpoint")
    return bool(endpoint and _private_default_endpoint(endpoint, root, runtime_dir))


def _retire_detached_owner(pid, descriptor, expected_executable, workspace, root,
                           home, runtime_dir, observed, timeout=8):
    """Retire a previously pinned autostart owner and prove its kernel exit."""
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return None
    try:
        if _wait_pidfd(descriptor, 0):
            return {"proof": "linux-pidfd-already-exited", "pid": pid,
                    "endpoint": observed["endpoint"]}
        current = _read_linux_process(pid)
        if (current is None or current["start_ticks"] != observed["start_ticks"]
                or _option_value(current["argv"], "--endpoint") != observed["endpoint"]
                or not _matches_detached_owner(current, expected_executable, workspace,
                                               root, home, runtime_dir)):
            return None
        try:
            signal.pidfd_send_signal(descriptor, signal.SIGTERM)
        except ProcessLookupError:
            pass
        if _wait_pidfd(descriptor, timeout):
            return {"proof": "linux-pidfd", "pid": pid, "signal": "TERM",
                    "endpoint": observed["endpoint"]}
        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
        if _wait_pidfd(descriptor, timeout):
            return {"proof": "linux-pidfd", "pid": pid, "signal": "KILL-after-TERM-timeout",
                    "endpoint": observed["endpoint"]}
        return None
    except (OSError, ProcessLookupError, PermissionError, TimeoutError):
        return None


def _owned_detached_owners(binary, workspace, root, home, runtime_dir):
    """Find exact same-user owner commands with private route environment."""
    proc_root = pathlib.Path("/proc")
    if not proc_root.is_dir() or not hasattr(os, "pidfd_open"):
        return None
    expected = str(pathlib.Path(binary).resolve(strict=True))
    owners = []
    for entry in proc_root.iterdir():
        if not entry.name.isdecimal():
            continue
        pid = int(entry.name)
        observed = _read_linux_process(pid)
        if observed is None:
            continue
        argv = observed["argv"]
        if (argv and argv[0] == expected
                and _option_value(argv, "--workspace") == str(workspace)
                and observed["uid"] == os.geteuid()
                and observed["environment"].get("HOME") == str(home)
                and observed["environment"].get("XDG_RUNTIME_DIR") == str(runtime_dir)):
            endpoint = _option_value(argv, "--endpoint")
            if endpoint and _private_default_endpoint(endpoint, root, runtime_dir):
                observed["endpoint"] = endpoint
                owners.append((pid, observed))
    return owners


def _linux_group_members(pgid):
    """Pin live members of the Popen-created private session before signalling."""
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return None
    members = []
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        pid = int(entry.name)
        first = _read_linux_process(pid)
        if first is None or first["pgrp"] != pgid or first["session"] != pgid:
            continue
        try:
            descriptor = os.pidfd_open(pid)
        except OSError:
            for _, owned_fd in members:
                os.close(owned_fd)
            return None
        second = _read_linux_process(pid)
        if (second is not None and second["start_ticks"] == first["start_ticks"]
                and second["pgrp"] == pgid and second["session"] == pgid):
            if not _wait_pidfd(descriptor, 0):
                members.append((pid, descriptor))
                continue
        os.close(descriptor)
    return members


def _retire_spawned_group(proc, identity, grace=2, timeout=8):
    """Retire a Popen-owned session without signalling a recycled numeric PGID."""
    if identity.get("unproved"):
        return None, "spawned process identity could not be proven; refusing to signal its process group"
    pgid = proc.pid
    leader_fd = identity.get("leader_fd")
    if leader_fd is None:
        # On hosts without pidfd, a live Popen leader proves its own freshly
        # created group. Once that leader has exited, retain the fixture if
        # descendants remain instead of signalling a possibly recycled PGID.
        if proc.returncode is None:
            try:
                os.killpg(pgid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            except OSError as error:
                return None, f"TERM failed: {error.__class__.__name__}"
            try:
                proc.wait(timeout=grace)
            except subprocess.TimeoutExpired:
                try:
                    proc.kill()
                    proc.wait(timeout=grace)
                except Exception:
                    return None, "owned leader did not exit"
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return {"proof": "owned-popen-child", "pid": pgid,
                    "exit_code": proc.returncode}, None
        except OSError as error:
            return None, f"group retirement unproved: {error.__class__.__name__}"
        return None, "leader exited but descendant retirement has no pidfd proof"

    if not _wait_pidfd(leader_fd, 0):
        # The retained pidfd proves that this group leader still exists, so the
        # numeric PGID cannot refer to an unrelated replacement group here.
        try:
            os.killpg(pgid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except OSError as error:
            return None, f"TERM failed: {error.__class__.__name__}"
        try:
            proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            try:
                signal.pidfd_send_signal(leader_fd, signal.SIGKILL)
                proc.wait(timeout=grace)
            except Exception:
                return None, "owned leader did not exit"

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        members = _linux_group_members(pgid)
        if members is None:
            return None, "session members could not be pinned"
        if not members:
            try:
                proc.wait(timeout=max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                return None, "owned leader did not exit"
            return {"proof": "owned-popen-session-pidfds", "pid": pgid,
                    "exit_code": proc.returncode}, None
        member_failure = None
        for pid, descriptor in members:
            try:
                signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                if not _wait_pidfd(descriptor, min(grace, max(0.1, deadline - time.monotonic()))):
                    signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                    if not _wait_pidfd(descriptor, max(0.1, deadline - time.monotonic())):
                        member_failure = f"session member {pid} did not retire"
            except ProcessLookupError:
                if not _wait_pidfd(descriptor, 0):
                    member_failure = f"session member {pid} identity disappeared without exit proof"
            except OSError as error:
                member_failure = f"session member retirement failed: {error.__class__.__name__}"
            finally:
                os.close(descriptor)
        if member_failure:
            return None, member_failure
    return None, "session descendants remained after TERM/KILL"


class RetirementUnproved(RuntimeError):
    """A cleanup failure retains structured ownership evidence for callers."""

    def __init__(self, failures):
        self.failures = failures
        super().__init__("route owner retirement was not proven: " + json.dumps(failures))


class Route:
    """One private HOME, one owner, any number of CLI and MCP calls."""

    def __init__(self, bin_dir, out, auto_start=False, path_extra=None, extra_env=None):
        self.bin = pathlib.Path(bin_dir)
        self.out = pathlib.Path(out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.root = pathlib.Path(tempfile.mkdtemp(prefix="ndx-route-", dir="/tmp")).resolve()
        os.chmod(self.root, 0o700)
        self.records = []
        self.seq = 0
        self.owner = None
        self.mcp = None
        self.cleanup_proofs = []
        self.spawned_processes = {}
        self.autostart_attempted = False
        self.autostart_unproved = False
        self.autostart_owners = {}
        self.pending = bytearray()
        self.auto_start = auto_start
        base_path = os.environ.get("PATH", "/usr/bin:/bin")
        env = {k: os.environ[k] for k in ("LANG", "LC_ALL", "TZ") if k in os.environ}
        env["PATH"] = path_extra if path_extra is not None else base_path
        for k in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                  "XDG_RUNTIME_DIR", "TMPDIR", "CARGO_HOME"):
            env[k] = str(self.root / k)
            (self.root / k).mkdir(mode=0o700, exist_ok=True)
        # Preserve an inherited account gate exactly. Debug builds still skip the
        # release gate, so this cannot by itself prove authenticated behaviour.
        if "NUDOX_ACCOUNT_GATE" in os.environ:
            env["NUDOX_ACCOUNT_GATE"] = os.environ["NUDOX_ACCOUNT_GATE"]
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
        self._ensure_authority_secret(secret)
        env = dict(self.env, BACKEND_LOCALD_AUTHORITY_SECRET_FILE=str(secret))
        with open(self.out / "owner-stderr.log", "wb") as log:
            self.owner = subprocess.Popen(
                [str(self.bin / "backend-locald"), "--endpoint", str(self.endpoint), "--workspace",
                 str(self.workspace), "--authority-secret-file", str(secret), "--registry-offline",
                 "--registry-discovery-offline", "--advisory-offline", "--forge-offline"],
                cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
        self._remember_spawned_process(self.owner)
        deadline = time.monotonic() + 420
        while not self.endpoint.exists():
            if self.owner.poll() is not None:
                raise RuntimeError(f"locald exited {self.owner.returncode} before endpoint")
            if time.monotonic() > deadline:
                raise RuntimeError("locald startup timed out")
            time.sleep(0.05)

    @staticmethod
    def _ensure_authority_secret(path):
        """Create the route authority once, or reuse only a safe 32-byte file."""
        required = (os.O_WRONLY | os.O_CREAT | os.O_EXCL
                    | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0))
        if not getattr(os, "O_NOFOLLOW", 0):
            raise RuntimeError("cannot safely create the route authority without O_NOFOLLOW")
        try:
            fd = os.open(path, required, 0o600)
        except FileExistsError:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
                         | getattr(os, "O_CLOEXEC", 0))
            try:
                info = os.fstat(fd)
                if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                        or info.st_uid != os.getuid() or info.st_mode & 0o077
                        or info.st_size != 32):
                    raise RuntimeError("existing route authority is not a private single-link 32-byte file")
                secret = os.read(fd, 33)
                if len(secret) != 32:
                    raise RuntimeError("existing route authority is not a private single-link 32-byte file")
            finally:
                os.close(fd)
            return
        secret = os.urandom(32)
        try:
            os.fchmod(fd, 0o600)
            view = memoryview(secret)
            while view:
                written = os.write(fd, view)
                if written <= 0:
                    raise OSError("short write creating route authority")
                view = view[written:]
            os.fsync(fd)
            info = os.fstat(fd)
            if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                    or info.st_uid != os.getuid() or info.st_mode & 0o077
                    or info.st_size != 32):
                raise RuntimeError("new route authority did not remain a private single-link 32-byte file")
        finally:
            os.close(fd)

    def _remember_autostart_owners(self):
        if not self.auto_start:
            return
        try:
            found = _owned_detached_owners(
                self.bin / "backend-locald", self.workspace, self.root,
                self.root / "HOME", self.root / "XDG_RUNTIME_DIR")
        except Exception:
            found = None
        if found is None:
            self.autostart_unproved = True
            return
        for pid, observed in found:
            identity = (pid, observed["start_ticks"])
            if identity in self.autostart_owners:
                continue
            try:
                descriptor = os.pidfd_open(pid)
                current = _read_linux_process(pid)
                if current is not None:
                    current["endpoint"] = _option_value(current["argv"], "--endpoint")
                if (current is None or current["start_ticks"] != observed["start_ticks"]
                        or current.get("endpoint") != observed["endpoint"]
                        or not _matches_detached_owner(
                            current, str((self.bin / "backend-locald").resolve(strict=True)),
                            self.workspace, self.root, self.root / "HOME",
                            self.root / "XDG_RUNTIME_DIR")):
                    os.close(descriptor)
                    self.autostart_unproved = True
                    continue
                current["endpoint"] = observed["endpoint"]
                self.autostart_owners[identity] = {
                    "pid": pid, "descriptor": descriptor, "observed": current,
                }
            except OSError:
                self.autostart_unproved = True

    def _remember_spawned_process(self, proc):
        identity = {"leader_fd": None, "start_ticks": None}
        if hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"):
            try:
                observed = _read_linux_process(proc.pid)
                if (observed is None or observed["pgrp"] != proc.pid
                        or observed["session"] != proc.pid):
                    raise RuntimeError("spawned child did not retain its private session identity")
                descriptor = os.pidfd_open(proc.pid)
                current = _read_linux_process(proc.pid)
                if (current is None or current["start_ticks"] != observed["start_ticks"]
                        or current["pgrp"] != proc.pid or current["session"] != proc.pid):
                    os.close(descriptor)
                    raise RuntimeError("spawned child identity changed before pidfd pin")
                identity.update(leader_fd=descriptor, start_ticks=current["start_ticks"])
            except (OSError, RuntimeError):
                identity["unproved"] = True
        self.spawned_processes[proc.pid] = identity

    def save_records(self):
        (self.out / "records.json").write_text(json.dumps(self.records, indent=1))

    def retire_for_reopen(self):
        """Retire this route's owners while keeping its private state for reuse."""
        unretired = []
        proofs = []
        for proc in (self.mcp, self.owner):
            if proc is None:
                continue
            try:
                if proc.stdin:
                    proc.stdin.close()
            except Exception:
                pass
            try:
                identity = self.spawned_processes.get(proc.pid)
                if identity is None:
                    proof, error = None, "no retained Popen session identity"
                else:
                    proof, error = _retire_spawned_group(proc, identity)
            except Exception as failure:
                proof, error = None, f"retirement failed: {failure.__class__.__name__}"
            if proof is None:
                unretired.append({"kind": "spawned-process-group", "pid": proc.pid,
                                  "reason": error or "retirement unproved"})
                continue
            proofs.append(proof)
            for stream_name in ("stdin", "stdout", "stderr"):
                stream = getattr(proc, stream_name, None)
                if stream is not None:
                    try:
                        stream.close()
                    except Exception:
                        pass
            identity = self.spawned_processes.pop(proc.pid, None)
            if identity and identity.get("leader_fd") is not None:
                try:
                    os.close(identity["leader_fd"])
                except OSError:
                    pass
            if self.mcp is proc:
                self.mcp = None
            if self.owner is proc:
                self.owner = None

        if self.auto_start:
            if self.autostart_attempted:
                self._remember_autostart_owners()
            if self.autostart_unproved:
                unretired.append({"kind": "autostart-owner",
                                  "reason": "process identity or kernel retirement was not proven"})
            if self.autostart_attempted and not self.autostart_owners:
                unretired.append({"kind": "autostart-owner",
                                  "reason": "no owned identity was pinned; empty discovery is not proof of retirement"})
            try:
                expected_executable = str((self.bin / "backend-locald").resolve(strict=True))
            except OSError:
                expected_executable = None
                if self.autostart_owners:
                    unretired.append({"kind": "autostart-owner", "reason": "owner executable identity unavailable"})
            for identity, record in list(self.autostart_owners.items()):
                proof = None
                if expected_executable is not None:
                    proof = _retire_detached_owner(
                        record["pid"], record["descriptor"], expected_executable,
                        self.workspace, self.root, self.root / "HOME",
                        self.root / "XDG_RUNTIME_DIR", record["observed"])
                if proof is None:
                    unretired.append({"kind": "autostart-owner", "pid": record["pid"],
                                      "endpoint": record["observed"].get("endpoint"),
                                      "reason": "identity changed or kernel retirement was not proven"})
                else:
                    proofs.append(proof)
                    try:
                        os.close(record["descriptor"])
                    except OSError:
                        pass
                    self.autostart_owners.pop(identity, None)

        self.cleanup_proofs.extend(proofs)
        if unretired:
            raise RetirementUnproved(unretired)
        return proofs

    def stop(self):
        unretired = []
        before = len(self.cleanup_proofs)
        try:
            self.retire_for_reopen()
        except RetirementUnproved as failure:
            unretired = failure.failures
        proofs = self.cleanup_proofs[before:]
        cleanup_record = {"label": "cleanup", "retired": proofs, "unretired": unretired,
                          "private_root": str(self.root), "root_removed": not unretired}
        self.records.append(cleanup_record)
        try:
            self.save_records()
        except Exception:
            unretired.append({"kind": "receipt", "reason": "cleanup receipt could not be saved"})
            cleanup_record["unretired"] = unretired
            cleanup_record["root_removed"] = False
        if not unretired:
            shutil.rmtree(self.root, ignore_errors=True)

    # ---- CLI -----------------------------------------------------------------------
    def cli(self, label, args, project, timeout=1800, json_out=True):
        if self.auto_start:
            self.autostart_attempted = True
        cmd = [str(self.bin / "backend-cli")]
        if json_out:
            cmd += ["--format", "json"]
        cmd += ["--project", str(project), "--workspace", str(self.workspace)]
        if not self.auto_start:
            cmd += ["--endpoint", str(self.endpoint)]
        cmd += list(args)
        try:
            proc = subprocess.run(cmd, cwd=project, env=self.env, capture_output=True, timeout=timeout)
        finally:
            if self.auto_start:
                self._remember_autostart_owners()
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
        if self.auto_start:
            self.autostart_attempted = True
        argv = [str(self.bin / "backend-mcp"), "--project", str(project), "--workspace", str(self.workspace)]
        if not self.auto_start:
            argv += ["--endpoint", str(self.endpoint)]
        with open(self.out / "mcp-stderr.log", "wb") as log:
            self.mcp = subprocess.Popen(argv, cwd=project, env=self.env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=log,
                                        start_new_session=True, bufsize=0)
        self._remember_spawned_process(self.mcp)
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": {"name": "report-routes", "version": "1"}})
        self.mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        if self.auto_start:
            self._remember_autostart_owners()

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


def receipt_header(bin_dir, source_commit, mode="development", effective_env=None):
    repo = HERE.parents[3]
    actual_commit = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], check=True,
        capture_output=True, text=True).stdout.strip()
    tree = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD^{tree}"], check=True,
        capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain"], check=True,
        capture_output=True, text=True).stdout
    if actual_commit != source_commit:
        raise RuntimeError(f"declared source commit {source_commit} differs from checkout HEAD {actual_commit}")
    if dirty:
        raise RuntimeError("source checkout is dirty; exact-head receipt refused")
    lockfile = repo / "Cargo.lock"
    gate_env = effective_env
    gate_value = gate_env.get("NUDOX_ACCOUNT_GATE") if gate_env is not None else None
    bins = {}
    for name in ("backend-cli", "backend-mcp", "backend-locald"):
        p = pathlib.Path(bin_dir) / name
        bins[name] = sha(p) if p.exists() else None
    return {
        "source_commit": source_commit,
        "source_tree": tree,
        "source_checkout_clean": True,
        "cargo_lock_sha256": sha(lockfile),
        "binaries": bins,
        "mode": "development (debug build; account-gate behavior not validated)",
        "account_gate_environment": (
            {"state": "preserved-in-subprocess", "value_sha256": hashlib.sha256(gate_value.encode()).hexdigest()}
            if gate_value is not None else
            {"state": "unset-in-subprocess"} if gate_env is not None else
            {"state": "not-recorded"}
        ),
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
