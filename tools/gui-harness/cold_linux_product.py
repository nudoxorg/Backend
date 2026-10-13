#!/usr/bin/env python3
"""Ten serial, fresh production GUI starts on a broker-owned Linux X11 display.

This collector does not create a display, build a binary, index a fixture, or
certify a semantic UI route. It records real product traces and native window
captures. GPUI test-support state/input checks are a separate evidence layer.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time


def digest(path):
    result = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def read_trace(path):
    records = []
    if path.exists():
        for line in path.read_text(errors="replace").splitlines():
            try:
                record = json.loads(line)
                if isinstance(record, dict) and isinstance(record.get("t_ms"), (int, float)):
                    records.append(record)
            except json.JSONDecodeError:
                # A concurrently appended final record may be incomplete.
                continue
    return records


def classify(records):
    events = [record for record in records if record.get("name") in {
        "root.bootstrap-entered", "root.bootstrap-failed", "root.bootstrap-admitted",
        "observation.waiting-initial-root", "observation.initial-root-admitted",
        "observation.certified", "observation.failed", "boot.window_opened",
    }]
    terminals = [event for event in events if event["name"] in {
        "observation.certified", "observation.failed",
    }]
    terminal = max(terminals, key=lambda event: event["t_ms"]) if terminals else None
    faults = [event for event in events if event["name"] == "root.bootstrap-failed"
              and str(event.get("detail", "")).startswith("fault:")]
    preceding = [event for event in faults if terminal and event["t_ms"] <= terminal["t_ms"]]
    fault = max(preceding, key=lambda event: event["t_ms"]) if preceding else None
    frames = [record for record in records if record.get("name") == "frame_lifecycle"
              and record.get("kind") == "drawn"]
    return {
        "status": "pending" if not terminal else (
            "certified" if terminal["name"] == "observation.certified" else "observation-failed"),
        "terminal": terminal,
        "bootstrap_fault_to_terminal_ms": (
            round(terminal["t_ms"] - fault["t_ms"], 3) if fault and terminal else None),
        "definitive_initial_root_failure": bool(terminal and
            terminal.get("detail") == "initial-root-failed"),
        "reading_header_faults": [event for event in faults
                                  if "ReadingHeader" in str(event.get("detail", ""))],
        "events": events,
        "drawn_frames": len(frames),
        "last_drawn_frame": max(frames, key=lambda event: event["t_ms"]) if frames else None,
        "semantic_route_acceptance": "UNRUN; native pixels and trace events are recorded separately",
    }


def command(argv, env, timeout=5):
    return subprocess.run(argv, env=env, capture_output=True, text=True,
                          timeout=timeout, check=False)


def tool(name):
    executable = shutil.which(name)
    if not executable:
        raise RuntimeError("missing X11/capture prerequisite: " + name)
    return executable


def find_window(pid, tools, env):
    found = command([tools["xdotool"], "search", "--onlyvisible", "--pid", str(pid)], env)
    for candidate in found.stdout.splitlines():
        if not candidate.isdigit():
            continue
        props = command([tools["xprop"], "-id", candidate, "_NET_WM_PID", "WM_CLASS", "WM_NAME"], env)
        if re.search(r"_NET_WM_PID\(CARDINAL\)\s*=\s*" + str(pid) + r"\b", props.stdout):
            return candidate, props.stdout
    return None, None


def capture(window, case, tools, env):
    raw, png = case / "window.xwd", case / "window.png"
    result = command([tools["xwd"], "-silent", "-id", window, "-out", str(raw)], env)
    if result.returncode:
        raise RuntimeError("native window capture failed: " + result.stderr.strip())
    convert = [tools["convert"], str(raw), str(png)]
    result = command(convert, env, timeout=10)
    if result.returncode:
        raise RuntimeError("XWD-to-PNG conversion failed: " + result.stderr.strip())
    result = command([tools["identify"], "-format", "%w %h %k", str(png)], env)
    dimensions = result.stdout.split()
    if result.returncode or len(dimensions) != 3:
        raise RuntimeError("captured PNG could not be inspected")
    width, height, colors = map(int, dimensions)
    return {"path": str(png), "sha256": digest(png), "width": width, "height": height,
            "distinct_colors": colors, "nonblank": width > 16 and height > 16 and colors > 1,
            "evidence": "actual X11 product window; no offscreen GPUI renderer used"}


def close_owned_process(proc, window, tools, env):
    if proc.poll() is None and window:
        try:
            command([tools["xdotool"], "windowclose", window], env)
        except (OSError, subprocess.SubprocessError):
            pass
    try:
        return proc.wait(timeout=5), "native-window-close"
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            return proc.wait(timeout=5), "owned-process-group-SIGTERM"
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            return proc.wait(timeout=5), "owned-process-group-SIGKILL"


def one_launch(index, args, tools, env, identity):
    case = args.output / ("cold-%02d" % index)
    case.mkdir(mode=0o700)
    for name in ["ambient", "data", "config", "cache"]:
        (case / name).mkdir(mode=0o700)
    child_env = dict(env)
    for name in ["BACKEND_PROJECT", "BACKEND_LOCALD_WORKSPACE", "BACKEND_LOCALD_ENDPOINT",
                 "BACKEND_LOCALD_AUTHORITY_SECRET_FILE", "NUDOX_DEBUG_PAGE", "WAYLAND_DISPLAY"]:
        child_env.pop(name, None)
    child_env.update(XDG_DATA_HOME=str(case / "data"), XDG_CONFIG_HOME=str(case / "config"),
                     XDG_CACHE_HOME=str(case / "cache"), NUDOX_TRACE=str(case / "trace.jsonl"))
    started = time.monotonic()
    proc = None
    window = None
    result = {"launch": index, "identity": identity, "case": str(case), "native_input": "UNRUN"}
    with open(case / "stdout.log", "wb") as stdout, open(case / "stderr.log", "wb") as stderr:
        try:
            proc = subprocess.Popen([str(args.binary)], cwd=case / "ambient", env=child_env,
                                    stdout=stdout, stderr=stderr, start_new_session=True)
            result["pid"] = proc.pid
            executable = Path("/proc") / str(proc.pid) / "exe"
            result["running_executable"] = str(executable.resolve(strict=True))
            result["running_executable_sha256"] = digest(executable)
            if result["running_executable_sha256"] != identity["binary_sha256"]:
                raise RuntimeError("running executable does not match the authenticated binary")
            terminal_seen = None
            props = None
            while proc.poll() is None and time.monotonic() - started < args.deadline:
                if not window:
                    window, props = find_window(proc.pid, tools, child_env)
                    if window:
                        # A private Xvfb server has no window manager to give
                        # the newly mapped product ordinary native focus.
                        # Preserve these actual input events separately from
                        # cold owner certification and semantic acceptance.
                        result["native_focus"] = []
                        for operation in ("windowraise", "windowfocus"):
                            began = time.monotonic_ns()
                            argv = [tools["xdotool"], operation, window]
                            native = command(argv, child_env)
                            result["native_focus"].append({
                                "argv": argv, "exit": native.returncode,
                                "stdout": native.stdout, "stderr": native.stderr,
                                "begin_monotonic_ns": began,
                                "end_monotonic_ns": time.monotonic_ns(),
                            })
                            if native.returncode:
                                raise RuntimeError("native product window focus failed")
                        focus = command([tools["xdotool"], "getwindowfocus", "-f"], child_env)
                        result["native_focus_window"] = focus.stdout.strip()
                        if focus.returncode or result["native_focus_window"] != window:
                            raise RuntimeError("native focus does not identify this product window")
                        result["native_input"] = "actual X11 raise/focus; semantic controls remain UNRUN"
                state = classify(read_trace(case / "trace.jsonl"))
                if state["status"] != "pending":
                    terminal_seen = terminal_seen or time.monotonic()
                    # Retain short post-terminal frame/observer activity; do not
                    # turn certification followed by immediate failure into a pass.
                    if window and time.monotonic() - terminal_seen >= 0.5:
                        break
                time.sleep(0.1)
            result.update(classify(read_trace(case / "trace.jsonl")))
            result["window_id"] = window
            if props:
                (case / "window-properties.txt").write_text(props)
            if window:
                result["capture"] = capture(window, case, tools, child_env)
            else:
                result["capture_error"] = "no current visible X11 window proved this product PID"
            result["pass"] = (result["status"] == "certified" and
                              result.get("capture", {}).get("nonblank", False))
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            result["collector_error"] = str(error)
            result["pass"] = False
        finally:
            if proc:
                result["process_exit"], result["close_method"] = close_owned_process(proc, window, tools, child_env)
                # Keep all final trace records, including orderly owner closure.
                write_json(case / "trace-analysis.json", classify(read_trace(case / "trace.jsonl")))
    result["wall_seconds"] = round(time.monotonic() - started, 3)
    write_json(case / "result.json", result)
    print(json.dumps({key: result.get(key) for key in ["launch", "pid", "status", "pass",
          "wall_seconds", "bootstrap_fault_to_terminal_ms", "collector_error"]}), flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--analyze-only", nargs="+", type=Path, metavar="TRACE")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--source-sha")
    parser.add_argument("--build-receipt", type=Path,
                        help="broker receipt with source_sha and binary_sha256 fields")
    parser.add_argument("--managed-receipt", type=Path,
                        help="existing broker managed-execution admission; its hash is retained")
    parser.add_argument("--display", help="existing private X11 display; this script does not start Xvfb")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--count", type=int, default=10)
    parser.add_argument("--deadline", type=float, default=80.0)
    args = parser.parse_args()
    if args.analyze_only:
        for path in args.analyze_only:
            print(json.dumps({"trace": str(path), **classify(read_trace(path))}, sort_keys=True))
        return 0
    if sys.platform != "linux":
        parser.error("live collection is Linux-only and must run through the remote sole broker")
    for name in ["binary", "source_sha", "build_receipt", "managed_receipt", "display", "output"]:
        if not getattr(args, name):
            parser.error("live collection requires --" + name.replace("_", "-"))
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_sha) or not 1 <= args.count <= 10:
        parser.error("require an exact source commit and between 1 and 10 serial launches")
    if not 1.0 <= args.deadline <= 90.0:
        parser.error("per-launch deadline must be between 1 and 90 seconds")
    args.binary = args.binary.resolve(strict=True)
    receipt = json.loads(args.build_receipt.read_text())
    binary_hash = digest(args.binary)
    if receipt.get("source_sha") != args.source_sha or receipt.get("binary_sha256") != binary_hash:
        parser.error("broker build receipt does not match the exact source and executable")
    managed_hash = digest(args.managed_receipt)
    env = dict(os.environ, DISPLAY=args.display)
    tools = {name: tool(name) for name in ["xdotool", "xprop", "xwd", "convert", "identify", "xdpyinfo"]}
    display = command([tools["xdpyinfo"], "-display", args.display], env)
    if display.returncode:
        parser.error("the admitted X11 display is unavailable")
    os.umask(0o077)
    args.output = args.output.absolute()
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    (args.output / "display.txt").write_text(display.stdout)
    identity = {"source_sha": args.source_sha, "binary_path": str(args.binary), "binary_sha256": binary_hash,
                "build_receipt_sha256": digest(args.build_receipt), "managed_receipt_sha256": managed_hash,
                "display": args.display, "tools": {name: {"path": path, "sha256": digest(path)}
                                                       for name, path in tools.items()}}
    write_json(args.output / "identity.json", identity)
    results = [one_launch(index, args, tools, env, identity) for index in range(1, args.count + 1)]
    summary = {"identity": identity, "count": len(results), "passed": sum(result["pass"] for result in results),
               "results": results, "evidence_scope": "production cold-root certification and actual X11 captures",
               "mounted_semantic_and_native_input_acceptance": "UNRUN; execute separate GPUI/native controls"}
    write_json(args.output / "summary.json", summary)
    return 0 if summary["passed"] == args.count else 1


if __name__ == "__main__":
    raise SystemExit(main())
