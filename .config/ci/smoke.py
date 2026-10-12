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
from pathlib import Path


def fail(message: str) -> int:
    print(f"smoke: {message}", file=sys.stderr)
    return 1


def isolated_env(home: Path) -> dict[str, str]:
    env = dict(os.environ)
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
    # Never inherit an endpoint, project, credential, or harness from the
    # developer shell. Every surface runs from an empty temporary project.
    for name in list(env):
        if name.startswith(("BACKEND_", "NUDOX_")):
            del env[name]
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("DISPLAY", None)
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
    server = subprocess.Popen(
        [xvfb, "-displayfd", str(write_fd), "-screen", "0", "1600x1000x24", "-nolisten", "tcp"],
        pass_fds=(write_fd,),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
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
                    return fail("desktop did not shut down within 10s after Quit")
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
