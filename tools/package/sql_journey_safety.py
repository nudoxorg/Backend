"""Owned process retirement and build provenance for private SQL journeys.

Linux uses a pinned pidfd and same-session process identities; other platforms
fail closed if a process group cannot be proved empty. This is test harness
plumbing, not an application runtime or a claim of public product proof.
"""
import ctypes
import errno
import hashlib
import os
from pathlib import Path
import pathlib
import select
import secrets
import shutil
import signal
import subprocess
import sys
import time
import threading

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
            if sep and key in (b"HOME", b"XDG_RUNTIME_DIR", b"NUDOX_SQL_JOURNEY_TOKEN"):
                environment[key.decode()] = value.decode("utf-8", "strict")
        executable = os.readlink(root / "exe")
        return {"argv": argv, "start_ticks": fields[19], "ppid": int(fields[1]), "pgrp": int(fields[2]),
                "session": int(fields[3]), "uid": uid,
                "environment": environment, "executable": executable}
    except (OSError, UnicodeError, IndexError, ValueError):
        return None

def _wait_pidfd(fd, timeout):
    poller = select.poll()
    poller.register(fd, select.POLLIN)
    return any(ready_fd == fd and mask & select.POLLIN
               for ready_fd, mask in poller.poll(max(0, int(timeout * 1000))))

def _linux_group_members(pgid):
    """Pin live members of the Popen-created private session before signalling."""
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return None
    members = []
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        pid = int(entry.name)
        try:
            raw = (entry / "stat").read_text()
            fields = raw[raw.rfind(")") + 2:].split()
            state, group, session = fields[0], int(fields[2]), int(fields[3])
        except (OSError, ValueError, IndexError):
            # A disappearing process is harmless; an unreadable live one
            # cannot establish that the owned session is empty.
            if entry.exists():
                for _, owned_fd, _ in members:
                    os.close(owned_fd)
                return None
            continue
        if group != pgid or session != pgid or state == "Z":
            continue
        first = _read_linux_process(pid)
        if first is None:
            if entry.exists():
                for _, owned_fd, _ in members:
                    os.close(owned_fd)
                return None
            continue
        try:
            descriptor = os.pidfd_open(pid)
        except OSError:
            for _, owned_fd, _ in members:
                os.close(owned_fd)
            return None
        second = _read_linux_process(pid)
        if (second is not None and second["start_ticks"] == first["start_ticks"]
                and second["pgrp"] == pgid and second["session"] == pgid):
            if not _wait_pidfd(descriptor, 0):
                members.append((pid, descriptor, first["start_ticks"]))
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
                proc.terminate()
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
        # A pidfd binds the kernel identity even if the leader exits between
        # the observation and signal. Never signal a numeric process group.
        try:
            signal.pidfd_send_signal(leader_fd, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except OSError as error:
            return None, f"TERM failed: {error.__class__.__name__}"
        if not _wait_pidfd(leader_fd, grace):
            try:
                signal.pidfd_send_signal(leader_fd, signal.SIGKILL)
                if not _wait_pidfd(leader_fd, grace):
                    return None, "owned leader did not exit"
            except OSError:
                return None, "owned leader did not exit"

    # Do not reap the leader until the private session is empty: its zombie
    # retains the numeric PID while descendant identities are pinned.
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
        for pid, descriptor, _ in members:
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


class OwnedProcesses:
    """Track every spawned child and refuse cleanup when retirement is unproved."""
    def __init__(self):
        self.owned = {}
        self.proofs = []
        self.token = secrets.token_hex(32)
        self.known_descendants = {}
        self.subreaper = False
        if sys.platform.startswith("linux"):
            if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
                raise RuntimeError("Linux pidfds are required for descendant retirement")
            if self._children():
                raise RuntimeError("journey process must start without pre-existing child processes")
            libc = ctypes.CDLL(None, use_errno=True)
            if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
                raise RuntimeError(f"could not establish private child subreaper: errno {ctypes.get_errno()}")
            state = ctypes.c_int()
            if libc.prctl(37, ctypes.byref(state), 0, 0, 0) != 0 or state.value != 1:  # PR_GET_CHILD_SUBREAPER
                raise RuntimeError("child subreaper could not be verified")
            self.subreaper = True

    @staticmethod
    def _children():
        try:
            text = (Path("/proc/self/task") / str(os.getpid()) / "children").read_text()
            return {int(value) for value in text.split()}
        except (OSError, ValueError) as error:
            raise RuntimeError("kernel child inventory unavailable") from error

    def _retire_adopted(self):
        """Pin and retire only reparented, token-matched private descendants."""
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            adopted = self._children() - set(self.owned)
            if not adopted:
                return
            for pid in sorted(adopted):
                first = _read_linux_process(pid)
                if first is None:
                    try:
                        raw = (Path("/proc") / str(pid) / "stat").read_text()
                        fields = raw[raw.rfind(")") + 2:].split()
                        if fields[0] == "Z" and int(fields[1]) == os.getpid() and self.known_descendants.get(pid) == fields[19]:
                            os.waitpid(pid, 0)
                            self.known_descendants.pop(pid, None)
                            self.proofs.append({"proof": "known-adopted-zombie", "pid": pid})
                            continue
                    except (OSError, ValueError, IndexError, ChildProcessError):
                        pass
                    raise RuntimeError(f"untracked child {pid} has no exact private descendant proof")
                if first["ppid"] != os.getpid() or first["uid"] != os.geteuid() or first["environment"].get("NUDOX_SQL_JOURNEY_TOKEN") != self.token:
                    raise RuntimeError(f"untracked child {pid} has no exact private descendant proof")
                try:
                    fd = os.pidfd_open(pid)
                except OSError as error:
                    raise RuntimeError(f"cannot pin adopted child {pid}") from error
                try:
                    second = _read_linux_process(pid)
                    if (second is None or second["start_ticks"] != first["start_ticks"]
                        or second["ppid"] != os.getpid() or second["uid"] != os.geteuid()
                        or second["environment"].get("NUDOX_SQL_JOURNEY_TOKEN") != self.token):
                        raise RuntimeError(f"adopted child {pid} identity changed")
                    if not _wait_pidfd(fd, 0):
                        try:
                            signal.pidfd_send_signal(fd, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                        if not _wait_pidfd(fd, 2):
                            signal.pidfd_send_signal(fd, signal.SIGKILL)
                            if not _wait_pidfd(fd, 2):
                                raise RuntimeError(f"adopted child {pid} did not exit")
                    try:
                        os.waitpid(pid, 0)
                    except ChildProcessError as error:
                        raise RuntimeError(f"adopted child {pid} could not be reaped") from error
                    self.proofs.append({"proof": "adopted-token-pidfd", "pid": pid})
                finally:
                    os.close(fd)
        raise RuntimeError("adopted descendants did not drain")

    def spawn(self, argv, **kwargs):
        if kwargs.pop("start_new_session", True) is not True:
            raise ValueError("owned processes require an exclusive session")
        env = dict(kwargs.pop("env", os.environ))
        env["NUDOX_SQL_JOURNEY_TOKEN"] = self.token
        proc = subprocess.Popen(argv, start_new_session=True, env=env, **kwargs)
        identity = {"leader_fd": None, "start_ticks": None}
        # Retain before attempting identity pinning: even a pin failure must
        # be reported, and never authorize deletion of its private state.
        self.owned[proc.pid] = (proc, identity)
        if hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"):
            try:
                observed = _read_linux_process(proc.pid)
                if observed is None or observed["pgrp"] != proc.pid or observed["session"] != proc.pid:
                    raise RuntimeError("private session identity unavailable")
                fd = os.pidfd_open(proc.pid)
                current = _read_linux_process(proc.pid)
                if current is None or current["start_ticks"] != observed["start_ticks"] or current["pgrp"] != proc.pid or current["session"] != proc.pid:
                    os.close(fd)
                    raise RuntimeError("private session identity changed")
                identity.update(leader_fd=fd, start_ticks=current["start_ticks"])
            except (OSError, RuntimeError):
                identity["unproved"] = True
                raise RuntimeError(f"could not pin spawned child {proc.pid}; retain private state")
        return proc

    def retire(self, proc, *, crash=False):
        entry = self.owned.get(proc.pid)
        if entry is None or entry[0] is not proc:
            raise RuntimeError("process is not an owned child")
        identity = entry[1]
        if identity.get("unproved"):
            raise RuntimeError(f"child {proc.pid} identity is unproved; refusing to signal")
        if self.subreaper:
            observed = _linux_group_members(proc.pid)
            if observed is None:
                raise RuntimeError("owned session identities could not be pinned before retirement")
            for pid, fd, start_ticks in observed:
                if pid != proc.pid:
                    self.known_descendants[pid] = start_ticks
                os.close(fd)
        if crash:
            # Signal only a still-live, retained leader; never a numeric group
            # after exit. The retirement routine subsequently proves descendants.
            fd = identity.get("leader_fd")
            if fd is not None:
                if _wait_pidfd(fd, 0):
                    raise RuntimeError("cold owner already exited; crash not established")
                signal.pidfd_send_signal(fd, signal.SIGKILL)
                if not _wait_pidfd(fd, 10):
                    raise RuntimeError("cold owner did not exit after SIGKILL")
            elif proc.poll() is None:
                proc.kill()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired as error:
                    raise RuntimeError("cold owner did not exit after SIGKILL") from error
            else:
                raise RuntimeError("cold owner already exited; crash not established")
        proof, error = _retire_spawned_group(proc, identity)
        if proof is None:
            raise RuntimeError(f"child {proc.pid} retirement unproved: {error}")
        if identity.get("leader_fd") is not None:
            os.close(identity["leader_fd"])
        self.owned.pop(proc.pid)
        self.proofs.append(proof)
        if crash and self.subreaper:
            if self.owned:
                raise RuntimeError("cannot prove cold-owner descendants while other owned processes remain")
            self._retire_adopted()
        return proof

    def exited(self, proc):
        entry = self.owned.get(proc.pid)
        if entry is None or entry[0] is not proc:
            raise RuntimeError("process is not an owned child")
        fd = entry[1].get("leader_fd")
        return _wait_pidfd(fd, 0) if fd is not None else proc.poll() is not None

    def run(self, argv, *, timeout, **kwargs):
        proc = self.spawn(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)
        fd = self.owned[proc.pid][1].get("leader_fd")
        if fd is None:
            try:
                stdout, stderr = proc.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.retire(proc)
                raise
            self.retire(proc)
            return subprocess.CompletedProcess(argv, proc.returncode, stdout, stderr)
        outputs = [bytearray(), bytearray()]
        overflows = []
        def read_stream(stream, sink):
            try:
                while True:
                    chunk = stream.read(65536)
                    if not chunk:
                        break
                    if len(sink) + len(chunk) > 32 * 1024 * 1024:
                        overflows.append(True)
                        break
                    sink.extend(chunk)
            finally:
                stream.close()
        readers = [threading.Thread(target=read_stream, args=(stream, sink), daemon=True)
                   for stream, sink in zip((proc.stdout, proc.stderr), outputs)]
        for reader in readers:
            reader.start()
        finished = _wait_pidfd(fd, timeout)
        self.retire(proc)
        for reader in readers:
            reader.join(timeout=10)
        if any(reader.is_alive() for reader in readers):
            raise RuntimeError("owned CLI output stream did not close after retirement")
        if not finished:
            raise subprocess.TimeoutExpired(argv, timeout)
        if overflows:
            raise RuntimeError("owned CLI output exceeds the harness byte bound")
        return subprocess.CompletedProcess(argv, proc.returncode, bytes(outputs[0]), bytes(outputs[1]))

    def retire_all(self):
        failures = []
        for proc, _ in list(self.owned.values()):
            try:
                self.retire(proc)
            except Exception as error:
                failures.append({"pid": proc.pid, "error": str(error)})
        if not failures and not self.owned:
            if not self.subreaper:
                failures.append({"kind": "descendant-proof", "error": "complete descendant retirement is not available on this platform"})
            else:
                try:
                    self._retire_adopted()
                except Exception as error:
                    failures.append({"kind": "descendant-proof", "error": str(error)})
        return failures


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def verified_build(receipt, checkout, bundle, script):
    """Verify Root's contemporaneous build receipt and exact invoked binaries."""
    checkout = Path(checkout).resolve(strict=True)
    script = Path(script).resolve(strict=True)
    if script != (checkout / "tools/package" / script.name).resolve(strict=True):
        raise RuntimeError("harness must run from the exact receipt source checkout")
    if Path(__file__).resolve(strict=True) != (checkout / "tools/package/sql_journey_safety.py").resolve(strict=True):
        raise RuntimeError("lifecycle helper must belong to the exact receipt source checkout")
    scripts = checkout / "tests/journeys/scripts"
    sys.path.insert(0, str(scripts))
    from runtime_build_receipt import verify_runtime_build_receipt
    verified = verify_runtime_build_receipt(Path(receipt), Path(checkout))
    bundle = Path(bundle).resolve(strict=True)
    for name, item in verified["artifacts"].items():
        target = (bundle / name).resolve(strict=True)
        if not target.is_file() or digest(target) != item["sha256"]:
            raise RuntimeError(f"bundle {name} differs from Root build receipt")
    return {"receipt": verified["receipt_path"], "receipt_sha256": verified["receipt_sha256"],
            "source": verified["source"], "artifacts": verified["artifacts"],
            "harness_sha256": digest(script), "helper_sha256": digest(__file__)}


def remove_private_root(root, identity):
    root = Path(root)
    current = root.lstat()
    if root.is_symlink() or not root.is_dir() or (current.st_dev, current.st_ino) != identity:
        raise RuntimeError("private root changed identity")
    shutil.rmtree(root)
    if root.exists() or root.is_symlink():
        raise RuntimeError("private root cleanup was not verified")
