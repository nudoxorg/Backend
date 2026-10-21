"""Linux T2 owned-session retirement, before reaping its reserved leader PID.

Only /proc/self child links are inspected. No host process census, PID-name
matching or signal after the owned session anchor has been reaped is allowed.
"""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time

_lock = threading.RLock()
_children = set()
_stopping = False
_baseline = None
_active = False
_fallback_enabled = False
_fallback_seen = False
_fallback_children = set()


class _RetirementDeadline(RuntimeError):
    """The anchor is retained; session retirement has not completed."""


def _own_children():
    children = set()
    with os.scandir('/proc/self/task') as entries:
        tasks = []
        for entry in entries:
            tasks.append(Path(entry.path))
            if len(tasks) > 256:
                raise RuntimeError('owned-child task census exceeds256 threads')
    for task in tasks:
        try:
            with (task/'children').open('rb') as stream:
                raw = stream.read(65537)
            if len(raw) > 65536:
                raise RuntimeError('owned-child link record exceeds64KiB')
            children.update(map(int, raw.split()))
        except FileNotFoundError:
            continue  # A thread exited; a live child is reparented to a peer.
    if len(children) > 4096:
        raise RuntimeError('owned-child census exceeds4096 children')
    return children


def _identity(pid):
    # Parent, SID and start token are one procfs record, not ambient argv/env.
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return int(fields[1]), int(fields[3]), int(fields[19])


def _prepare():
    global _baseline
    if _baseline is not None:
        return
    if not all(hasattr(os, key) for key in ('waitid', 'WNOWAIT', 'pidfd_open')) or not hasattr(signal, 'pidfd_send_signal'):
        raise RuntimeError('T2 owned-session custody requires Linux waitid/pidfd support')
    if signal.getsignal(signal.SIGCHLD) not in (signal.SIG_DFL, None):
        raise RuntimeError('T2 custody requires no independent SIGCHLD reaper')
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    libc.prctl.restype = ctypes.c_int
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'cannot establish T2 child subreaper')
    baseline = {}
    for pid in _own_children():
        try:
            baseline[pid] = _identity(pid)
        except FileNotFoundError:
            pass
    _baseline = baseline


class OwnedPopen(subprocess.Popen):
    def __init__(self, *args, **kwargs):
        self.custody_lock = threading.RLock()
        self.retired = False
        super().__init__(*args, **kwargs)

    def poll(self):
        # Observe only. returncode is an observed root status until wait()
        # retires its session. The unreaped child reserves its PID and SID.
        with self.custody_lock:
            if self.returncode is None:
                info = os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                if info is not None:
                    self.returncode = info.si_status if info.si_code == os.CLD_EXITED else -info.si_status
            return self.returncode

    def _internal_poll(self, _deadstate=None):
        # Popen.__del__ must not bypass the waitable-anchor contract either.
        return self.poll()

    def _try_wait(self, wait_flags):
        # No inherited private helper may independently reap the anchor.
        raise RuntimeError('owned anchor requires session retirement, not _try_wait')

    def _wait(self, timeout):
        # CPython wait(), communicate() and __exit__() call this directly,
        # including their short KeyboardInterrupt grace waits. Inherit public
        # wait() so CPython still resumes KeyboardInterrupt after that grace.
        deadline = time.monotonic() + (5 if timeout is None else timeout)
        # An ordinary indefinite wait retains its old execution lifetime;
        # only retirement after an observed root exit receives a5s bound.
        while self.poll() is None:
            if timeout is not None and time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired(self.args, timeout)
            time.sleep(.01)
        if timeout is None:
            deadline = time.monotonic() + 5
        try:
            return self.retire(deadline)
        except _RetirementDeadline:
            if timeout is not None:
                # CPython catches precisely TimeoutExpired while preserving
                # KeyboardInterrupt. Keep the unresolved anchor for cleanup.
                raise subprocess.TimeoutExpired(self.args, timeout) from None
            raise

    def retire(self, deadline):
        with self.custody_lock, _lock:
            if self.retired:
                return self.returncode
            # Even if poll observed exit, this leader is STILL waitable. Never
            # replace this with Popen.poll/try_wait before signalling the SID.
            info = os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            try:
                os.killpg(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass  # Signal-attempt result only; census/reap decides closure.
            while True:
                if time.monotonic() >= deadline:
                    raise _RetirementDeadline('owned session retirement incomplete at existing deadline')
                found = []
                active_roots = {child.pid for child in _children if not child.retired}
                for pid in _own_children():
                    if pid == self.pid or pid in active_roots:
                        continue
                    try:
                        identity = _identity(pid)
                    except FileNotFoundError:
                        continue
                    if identity == _baseline.get(pid):
                        continue  # Pre-existing unrelated child, preserved.
                    if identity[1] != self.pid:
                        # Other still-owned sessions are handled by their own
                        # anchors. Never signal an unproved/new escaped SID.
                        if identity[1] in active_roots:
                            continue
                        raise RuntimeError('unproved adopted child/session; custody is not closed')
                    fd = os.pidfd_open(pid)
                    try:
                        if _identity(pid) != identity:
                            raise RuntimeError('owned adopted child changed identity')
                        signal.pidfd_send_signal(fd, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    finally:
                        os.close(fd)
                    found.append(pid)
                for pid in found:
                    # These are exact adopted children, never a waitpid(-1)
                    # that might consume an unrelated sentinel's status.
                    try:
                        os.waitpid(pid, os.WNOHANG)
                    except ChildProcessError:
                        raise RuntimeError('owned adopted child was independently reaped')
                if not found:
                    info = os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    if info is not None:
                        _, status = os.waitpid(self.pid, 0)  # Anchor is LAST.
                        self._handle_exitstatus(status)
                        self.retired = True
                        return self.returncode
                time.sleep(.01)

    def send_signal(self, sig):
        with self.custody_lock:
            if not self.retired:
                # The PID is unreaped even after observed exit; it cannot name
                # an unrelated replacement. Group cleanup belongs to retire.
                os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                try:
                    os.kill(self.pid, sig)
                except ProcessLookupError:
                    pass


def activate():
    # Called once by the T2 case coordinator. Standalone CLI-contract tools
    # retain their existing cross-platform subprocess behavior.
    global _active, _fallback_enabled
    with _lock:
        _active = sys.platform == 'linux'
        _fallback_enabled = not _active


def popen(*args, **kwargs):
    global _fallback_seen
    if not _active:
        if not _fallback_enabled:
            return subprocess.Popen(*args, **kwargs)
        with _lock:
            if _stopping:
                raise RuntimeError('surface interruption: new child admission is closed')
            _fallback_seen = True
            # Admission and registration share the interruption snapshot lock;
            # stop_before_exit cannot miss a constructor already admitted here.
            child = subprocess.Popen(*args, **kwargs)
            child._nudox_new_session = kwargs.get('start_new_session') is True
            _fallback_children.add(child)
            return child
    with _lock:
        if _stopping:
            raise RuntimeError('surface interruption: new child admission is closed')
        _prepare()
        kwargs['start_new_session'] = True
        child = OwnedPopen(*args, **kwargs)
        _children.add(child)
        return child


def signal_session(child, sig, deadline=None):
    if not isinstance(child, OwnedPopen):
        # Preserve ordinary POSIX Popen cleanup. Its waitpid lock prevents a
        # concurrent poll/wait from reaping this PID before the signal; an
        # already-reaped root can never authorize a numeric group signal.
        if signal.getsignal(signal.SIGCHLD) not in (signal.SIG_DFL, None):
            raise RuntimeError('ordinary Popen signal refused with an independent SIGCHLD reaper')
        while not child._waitpid_lock.acquire(False):
            if deadline is None or time.monotonic() >= deadline:
                raise RuntimeError('ordinary Popen wait owns the anchor; unsafe concurrent signal refused')
            time.sleep(min(.01, max(0, deadline-time.monotonic())))
        try:
            if signal.getsignal(signal.SIGCHLD) not in (signal.SIG_DFL, None):
                raise RuntimeError('ordinary Popen signal refused with an independent SIGCHLD reaper')
            if child.returncode is None:
                send = os.killpg if getattr(child, '_nudox_new_session', False) is True else os.kill
                try:send(child.pid, sig)
                except ProcessLookupError:pass
        finally:
            child._waitpid_lock.release()
        return
    with child.custody_lock:
        if not child.retired:
            os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            try:
                os.killpg(child.pid, sig)
            except ProcessLookupError:
                pass


def retire(child, deadline):
    if isinstance(child, OwnedPopen):
        return child.retire(deadline)
    # Ordinary Popen has no retire method; this is root wait, NOT a claim
    # that non-Linux descendant sessions have been proved empty.
    return child.wait(timeout=max(0, deadline-time.monotonic()))


def stop(child):
    signal_session(child, signal.SIGTERM)
    try:
        child.wait(timeout=10)
    except (subprocess.TimeoutExpired, RuntimeError):
        if not isinstance(child, OwnedPopen):
            signal_session(child, signal.SIGKILL)
        retire(child, time.monotonic()+5)


def _run_ordinary(*args, input=None, capture_output=False, timeout=None, check=False, **kwargs):
    if input is not None:
        if kwargs.get('stdin') is not None:
            raise ValueError('stdin and input arguments may not both be used.')
        kwargs['stdin'] = subprocess.PIPE
    if capture_output:
        if kwargs.get('stdout') is not None or kwargs.get('stderr') is not None:
            raise ValueError('stdout and stderr arguments may not be used with capture_output.')
        kwargs.update(stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    child = popen(*args, **kwargs)  # Atomic admission and ordinary-root registration.
    try:
        deadline = None if timeout is None else time.monotonic()+timeout
        pending_input = input
        while True:
            remaining = _communicate_timeout(deadline, pending_input, time.monotonic())
            try:
                stdout, stderr = child.communicate(pending_input, timeout=remaining)
                break
            except subprocess.TimeoutExpired as error:
                # CPython cannot resume a partial input write on an
                # input=None retry. Finite-deadline input is sent in one call;
                # unbounded input retains short polling for prompt stop, so a
                # delayed reader may still outlast its first communication slice.
                pending_input = None
                if deadline is not None and time.monotonic() >= deadline:
                    raise subprocess.TimeoutExpired(child.args, timeout,
                        output=error.output, stderr=error.stderr) from None
        result = subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)
        if check:
            result.check_returncode()
        return result
    except BaseException:
        # No ordinary context-manager __exit__/unbounded wait: cleanup keeps
        # its own existing5s bound, exact-root signal lock and unreaped identity.
        cleanup_deadline = time.monotonic()+5
        signal_session(child, signal.SIGKILL, cleanup_deadline)
        retire(child, cleanup_deadline)
        raise
    finally:
        for stream in (child.stdin, child.stdout, child.stderr):
            if stream is not None:
                stream.close()


def _communicate_timeout(deadline, pending_input, now):
    """Choose a bounded poll interval, keeping finite input sends intact."""
    if deadline is None:
        return .1
    remaining = max(0, deadline-now)
    if pending_input:
        return remaining
    return min(.1, remaining)


def run(*args, input=None, capture_output=False, timeout=None, check=False, **kwargs):
    global _fallback_seen
    if not _active:
        if _fallback_enabled:
            return _run_ordinary(*args, input=input, capture_output=capture_output,
                                 timeout=timeout, check=check, **kwargs)
        return subprocess.run(*args, input=input, capture_output=capture_output,
                              timeout=timeout, check=check, **kwargs)
    if input is not None:
        if kwargs.get('stdin') is not None:
            raise ValueError('stdin and input arguments may not both be used')
        kwargs['stdin'] = subprocess.PIPE
    if capture_output:
        if kwargs.get('stdout') is not None or kwargs.get('stderr') is not None:
            raise ValueError('stdout and stderr arguments may not be used with capture_output')
        kwargs.update(stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    child = popen(*args, **kwargs)
    try:
        stdout, stderr = child.communicate(input, timeout=timeout)
        result = subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)
        if check:
            result.check_returncode()
        return result
    except BaseException:
        child.retire(time.monotonic()+5)  # Timeout also retires descendants.
        raise
    finally:
        for stream in (child.stdin, child.stdout, child.stderr):
            if stream is not None:
                stream.close()


def stop_before_exit():
    global _stopping
    with _lock:
        _stopping = True
        children = list(_fallback_children if _fallback_enabled else _children)
        unproved_fallback = _fallback_enabled and _fallback_seen
    if unproved_fallback:
        deadline = time.monotonic()+5
        for child in children:
            signal_session(child, signal.SIGTERM, deadline)
        while time.monotonic() < deadline and any(child.poll() is None for child in children):
            time.sleep(.01)
        deadline = time.monotonic()+5
        for child in children:
            if child.returncode is None:
                signal_session(child, signal.SIGKILL, deadline)
            retire(child, deadline)
        raise RuntimeError('ordinary Popen roots cleaned; non-Linux descendant custody remains unproved; refusing early exit')
    for child in children:
        signal_session(child, signal.SIGTERM)
    deadline = time.monotonic()+5
    while time.monotonic() < deadline and any(child.poll() is None for child in children):
        time.sleep(.01)
    deadline = time.monotonic()+5
    for child in children:
        retire(child, deadline)
    return dict(owned_roots=len(children), owned_sessions_retired=True,
                anchors_reaped_last=True, new_child_admission_closed=True)
