#!/usr/bin/env python3
"""T1 smoke checks: does each product surface come alive?

    python3 .config/ci/smoke.py cli|mcp|gui --bin-dir DIR

Each check starts one built binary in a throwaway HOME and proves the most
basic thing it is for, nothing more. Correctness belongs to T2.

- cli: `backend-cli --help` exits 0 and prints usage.
- mcp: `backend-mcp` on stdio answers `initialize` and `tools/list`. Both are
  local protocol operations, so no index owner is needed.
- gui: `backend-desktop` under Xvfb with Mesa's software Vulkan draws at least
  one frame, read from its own NUDOX_TRACE frame records.

Exit 0 on success, 1 on failure, with the reason on stderr.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from collections.abc import Mapping
from pathlib import Path


def fail(message: str) -> int:
    print(f"smoke: {message}", file=sys.stderr)
    return 1


_RUNTIME_PATH_ENV = (
    "PATH",
    "LD_LIBRARY_PATH",
    "FONTCONFIG_FILE",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "LIBGL_DRIVERS_PATH",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
)
_LOCALE_ENV = (
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_ADDRESS",
    "LC_IDENTIFICATION",
    "LC_MEASUREMENT",
    "LC_NAME",
    "LC_PAPER",
    "LC_TELEPHONE",
)


def isolated_env(home: Path, source: Mapping[str, str] | None = None) -> dict[str, str]:
    """Build a child environment from runtime paths/locales, never ambient secrets."""
    source = os.environ if source is None else source
    env = {
        name: source[name]
        for name in (*_RUNTIME_PATH_ENV, *_LOCALE_ENV)
        if name in source
    }
    for name, sub in (
        ("HOME", ""),
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
        ("XDG_RUNTIME_DIR", "run"),
        ("TMPDIR", "tmp"),
    ):
        path = home / sub
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        env[name] = str(path)
    return env


def stop(process: subprocess.Popen) -> None:
    """Ends a child we started, and its process group, without waiting long."""
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=10)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


def smoke_cli(bin_dir: Path, home: Path) -> int:
    result = subprocess.run(
        [bin_dir / "backend-cli", "--help"],
        env=isolated_env(home),
        cwd=home,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr
    if result.returncode != 0:
        return fail(f"backend-cli --help exited {result.returncode}:\n{output[-2000:]}")
    if "usage" not in output.lower():
        return fail(f"backend-cli --help printed no usage:\n{output[-2000:]}")
    print(f"cli: --help ok ({len(output.splitlines())} lines)")
    return 0


def smoke_mcp(bin_dir: Path, home: Path) -> int:
    project = home / "project"
    project.mkdir()
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "nudox-t1-smoke", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    stdin = "".join(json.dumps(request) + "\n" for request in requests)
    result = subprocess.run(
        [bin_dir / "backend-mcp", "--project", str(project)],
        env=isolated_env(home),
        cwd=project,
        input=stdin,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if result.returncode != 0:
        return fail(f"backend-mcp exited {result.returncode}:\n{result.stderr[-2000:]}")
    replies = {}
    for line in result.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            return fail(f"backend-mcp wrote a non-JSON line: {line[:200]}")
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            return fail(f"backend-mcp wrote an invalid JSON-RPC reply: {line[:200]}")
        if "id" in message:
            if message["id"] in replies:
                return fail(f"backend-mcp returned duplicate id {message['id']}")
            replies[message["id"]] = message
    initialize = replies.get(1, {})
    if "result" not in initialize:
        return fail(f"no initialize result (exit {result.returncode}):\n{result.stdout[-1500:]}\n{result.stderr[-1500:]}")
    info = initialize["result"]
    if not isinstance(info, dict) or not isinstance(info.get("serverInfo"), dict):
        return fail("initialize returned no serverInfo object")
    server = info["serverInfo"].get("name")
    if not isinstance(server, str) or not server or "tools" not in info.get("capabilities", {}):
        return fail("initialize returned no server name or tools capability")
    tools = replies.get(2, {}).get("result", {}).get("tools")
    if not isinstance(tools, list) or not tools:
        return fail(f"tools/list returned no tools:\n{json.dumps(replies.get(2))[:1500]}")
    print(f"mcp: initialize ok (server {server}), tools/list ok ({len(tools)} tools)")
    return 0


def drew_a_frame(trace: Path) -> bool:
    if not trace.exists():
        return False
    for line in trace.read_text(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        # A collection containing frame ticks/link events proves no paint.
        if isinstance(event, dict) and event.get("name") == "frame":
            return True
    return False


def _proc_text(path: Path, limit: int = 1024) -> str:
    try:
        with path.open("rb") as stream:
            data = stream.read(limit + 1)
    except OSError as error:
        return f"unavailable ({error.__class__.__name__})"
    truncated = len(data) > limit
    text = data[:limit].decode(errors="replace").strip()
    return text + ("…" if truncated else "")


def _gui_process_diagnostics(pid: int, proc_root: Path = Path("/proc")) -> str:
    """Read a small, secret-free /proc snapshot before failure cleanup.

    File reads are byte-capped; the 0.5s budget stops further sampling when
    possible, but is best-effort because an individual procfs read has no
    portable timeout.
    """
    deadline = time.monotonic() + 0.5
    proc = proc_root / str(pid)
    try:
        child_text = _proc_text(proc / "task" / str(pid) / "children", 256)
        child_pids = [int(value) for value in child_text.split() if value.isdecimal()]
    except (OSError, ValueError):
        child_pids = []
    pids = [pid, *child_pids[:3]]
    details = []
    threads = []
    cwd_details = []
    for child_pid in pids:
        if time.monotonic() >= deadline:
            details.append("process snapshot budget elapsed (0.5s best effort)")
            break
        try:
            cwd = os.readlink(proc_root / str(child_pid) / "cwd")[:512]
        except OSError as error:
            cwd = f"unavailable ({error.__class__.__name__})"
        cwd_details.append(f"pid={child_pid} cwd={cwd!r}")
        task_dir = proc_root / str(child_pid) / "task"
        try:
            task_entries = []
            with os.scandir(task_dir) as entries:
                for entry in entries:
                    if entry.name.isdecimal():
                        task_entries.append(int(entry.name))
                    if len(task_entries) >= 128:
                        break
        except OSError:
            task_entries = [child_pid]
        for tid in task_entries:
            if time.monotonic() >= deadline:
                break
            thread = task_dir / str(tid)
            comm = _proc_text(thread / "comm", 128)
            priority = int(any(name in comm.casefold() for name in ("owner", "compiler", "locald")))
            threads.append((priority, child_pid, tid, comm, thread))
    threads.sort(key=lambda item: item[0], reverse=True)
    for _, child_pid, tid, comm, thread in threads[:32]:
        if time.monotonic() >= deadline:
            details.append("thread snapshot budget elapsed (0.5s best effort)")
            break
        wchan = _proc_text(thread / "wchan", 128)
        stack = _proc_text(thread / "stack", 512)
        details.append(
            f"pid={child_pid} tid={tid} comm={comm!r} wchan={wchan!r}\nstack:\n{stack}"
        )
    if len(threads) > 32:
        details.append(f"thread sample capped at 32 of {len(threads)} discovered threads")
    details[:0] = cwd_details
    return "\n".join(details)


def _tail(path: Path, limit: int = 3000) -> str:
    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - limit))
            data = stream.read(limit)
    except OSError as error:
        return f"unavailable ({error.__class__.__name__})"
    text = data.decode(errors="replace")
    return ("…" if size > limit else "") + text


def smoke_gui(bin_dir: Path, home: Path, budget: float) -> int:
    xvfb = shutil.which("Xvfb")
    xdotool = shutil.which("xdotool")
    if xvfb is None or xdotool is None:
        return fail("Xvfb/xdotool are not on PATH (run inside the `.#smoke` shell)")
    # Xvfb selects a free display atomically. A PID-derived number collides
    # when several shards start at once, or can find someone else's socket.
    read_fd, write_fd = os.pipe()
    env = isolated_env(home)
    trace = home / "trace.jsonl"
    env["NUDOX_TRACE"] = str(trace)
    log = open(home / "desktop.log", "w")
    server_log = open(home / "xvfb.log", "w")
    server = subprocess.Popen(
        [xvfb, "-displayfd", str(write_fd), "-screen", "0", "1600x1000x24", "-nolisten", "tcp"],
        env=env,
        pass_fds=(write_fd,),
        stdout=subprocess.DEVNULL,
        stderr=server_log,
        start_new_session=True,
    )
    os.close(write_fd)
    app = None
    try:
        import select

        ready, _, _ = select.select([read_fd], [], [], 10)
        if not ready:
            return fail("Xvfb did not publish a display within 10s")
        display_number = os.read(read_fd, 64).decode().strip()
        if not display_number.isdecimal() or server.poll() is not None:
            return fail("Xvfb did not start")
        env["DISPLAY"] = f":{display_number}"
        started = time.monotonic()
        app = subprocess.Popen(
            [bin_dir / "backend-desktop"],
            env=env,
            cwd=home,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        while time.monotonic() - started < budget:
            if drew_a_frame(trace):
                first_frame = time.monotonic() - started
                windows = subprocess.run(
                    [xdotool, "search", "--onlyvisible", "--pid", str(app.pid)],
                    env=env, capture_output=True, text=True, timeout=5,
                )
                if windows.returncode != 0 or not windows.stdout.strip():
                    return fail("desktop drew a frame but mapped no visible X11 window")
                window = windows.stdout.splitlines()[0]
                # Exercise the app's public Quit shortcut. Signals below are
                # failure cleanup of this task's children, never the proof.
                subprocess.run(
                    [xdotool, "windowfocus", "--sync", window, "key", "--clearmodifiers", "super+q"],
                    env=env, check=True, capture_output=True, timeout=5,
                )
                try:
                    code = app.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    log.flush()
                    server_log.flush()
                    details = "\n".join(
                        f"{name}:\n{_tail(home / name)}"
                        for name in ("desktop.log", "xvfb.log", "trace.jsonl")
                        if (home / name).is_file()
                    )
                    process_details = _gui_process_diagnostics(app.pid)
                    return fail(
                        "desktop did not shut down within 10s after Quit\n"
                        f"{details}\nprocesses:\n{process_details}"
                    )
                if code != 0:
                    return fail(f"desktop quit with exit {code}")
                print(f"gui: visible window and first frame after {first_frame:.1f}s; clean Quit")
                return 0
            if app.poll() is not None:
                break
            time.sleep(0.2)
        log.flush()
        tail = (home / "desktop.log").read_text(errors="replace")[-3000:]
        state = f"exited {app.returncode}" if app.poll() is not None else f"no frame within {budget:.0f}s"
        return fail(f"backend-desktop {state}:\n{tail}")
    finally:
        if app is not None:
            stop(app)
        stop(server)
        os.close(read_fd)
        log.close()
        server_log.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("surface", choices=["cli", "mcp", "gui"])
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--gui-budget", type=float, default=45.0, help="seconds to wait for the first frame")
    args = parser.parse_args()
    args.bin_dir = args.bin_dir.resolve()
    with tempfile.TemporaryDirectory(prefix=f"nudox-smoke-{args.surface}-") as scratch:
        home = Path(scratch)
        if args.surface == "cli":
            return smoke_cli(args.bin_dir, home)
        if args.surface == "mcp":
            return smoke_mcp(args.bin_dir, home)
        return smoke_gui(args.bin_dir, home, args.gui_budget)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, subprocess.SubprocessError, ValueError, TypeError) as error:
        sys.exit(fail(str(error)))
