#!/usr/bin/env python3
"""Capture an existing *native* macOS desktop window with timed input and AX.

This complements `backend-desktop-gui-harness journey`, which runs the real
owner but draws into a headless GPUI window. A native capture proves compositor
pixels, window geometry and OS accessibility. A paired owner/read receipt is
still required before claiming that those pixels show a particular live index.

Example (after launching the exact built desktop binary and granting Screen
Recording, Accessibility and Input Monitoring to the recorder's terminal):
  python3 tools/gui-harness/native_motion.py \
    --pid "$PID" --binary /path/to/backend-desktop \
    --source /path/to/frozen/candidate-checkout \
    --compiler-receipt /path/to/completed-build/receipt.json \
    --plan tools/gui-harness/plans/first-add.json \
    --input /path/to/project/Cargo.toml --input /path/to/project/Cargo.lock \
    --out /private/tmp/sol-native-first-add
  python3 tools/gui-harness/native_motion_matrix.py /private/tmp/sol-native-runs \
    --out /private/tmp/sol-native-runs/MATRIX.json

Compile the recorder separately with `swiftc -parse-as-library -O
 tools/gui-harness/native_motion.swift -o native-motion-recorder`, then use
`native-motion-recorder list PID` if more than one window needs an exact ID.
The plan's action times are relative to the first captured native frame. Native
keycodes are macOS virtual keycodes; click coordinates are global screen points.
No route, index, owner gate, or component state is injected by this tool.

For a copied executable or relocated frozen checkout, also pass
`--preservation-receipt copy.json`. Schema 1 binds the original compiler
receipt SHA, build_cwd, compiled_artifact_path/SHA to the capture
source path/HEAD/tree/Cargo.lock SHA and copied artifact path/SHA. A copied
.app also binds its Info.plist path/SHA and CFBundleIdentifier. The original
compiler receipt is never rewritten; a symlink or hard link is not a copy.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import math
import os
import plistlib
import shlex
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
SWIFT = Path(__file__).with_name("native_motion.swift")
MAX_DURATION_MS = 30_000
MAX_FRAMES = 1_800
MAX_INPUT_BYTES = 2_000_000_000
MAX_AX_SAMPLE_AGE_MS = 150
MAX_CLOCK_RESIDUAL_MS = 50
MAX_CALLBACK_OFFSET_MS = 100


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=root, text=True).strip()


def source_identity(root: Path) -> dict[str, str]:
    root = root.resolve(strict=True)
    if not root.is_dir() or Path(git(root, "rev-parse", "--show-toplevel")).resolve() != root:
        raise ValueError(f"source must be an exact Git checkout root: {root}")
    lock = root / "Cargo.lock"
    if not lock.is_file():
        raise ValueError(f"candidate source lacks Cargo.lock: {root}")
    return {"path": str(root), "head": git(root, "rev-parse", "HEAD"),
            "tree": git(root, "rev-parse", "HEAD^{tree}"),
            "status_porcelain": git(root, "status", "--porcelain"),
            "cargo_lock_sha256": sha256(lock)}


def tool_identity() -> dict[str, str]:
    return {"path": str(ROOT), "head": git(ROOT, "rev-parse", "HEAD"),
            "tree": git(ROOT, "rev-parse", "HEAD^{tree}"),
            "status_porcelain": git(ROOT, "status", "--porcelain"),
            "python_sha256": sha256(Path(__file__)), "swift_sha256": sha256(SWIFT)}


def symlink_in_artifact_path(path: Path) -> bool:
    """Reject bundle-internal links without treating macOS /var as an artifact link."""
    bundle = next((parent for parent in path.parents if parent.suffix == ".app"), None)
    if bundle is None:
        return path.is_symlink()
    current = path
    while current != bundle:
        if current.is_symlink():
            return True
        current = current.parent
    return bundle.is_symlink()


def bundle_identity(binary: Path) -> dict[str, str] | None:
    binary = binary.resolve(strict=True)
    bundle = next((parent for parent in binary.parents if parent.suffix == ".app"), None)
    if bundle is None:
        return None
    info = bundle / "Contents" / "Info.plist"
    if not info.is_file() or symlink_in_artifact_path(info):
        raise ValueError(f"copied app bundle lacks Info.plist: {bundle}")
    data = plistlib.loads(info.read_bytes())
    identifier = data.get("CFBundleIdentifier")
    executable = data.get("CFBundleExecutable")
    if not isinstance(identifier, str) or not identifier or executable != binary.name:
        raise ValueError(f"app bundle identity or executable name is invalid: {bundle}")
    return {"path": str(bundle), "info_path": str(info), "info_sha256": sha256(info),
            "identifier": identifier}


def process_executable(pid: int) -> Path:
    # libproc is the OS source of the *running* process image, not an argv
    # string or bundle label that another process could spoof.
    lib = ctypes.CDLL("/usr/lib/libproc.dylib")
    lib.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    lib.proc_pidpath.restype = ctypes.c_int
    buffer = ctypes.create_string_buffer(4096)
    length = lib.proc_pidpath(pid, buffer, len(buffer))
    if length <= 0:
        raise ValueError(f"no executable path for running PID {pid}")
    return Path(os.fsdecode(buffer.value)).resolve()


def require_plan(path: Path) -> dict[str, Any]:
    plan = json.loads(path.read_text())
    if not isinstance(plan, dict) or plan.get("schema") != 1:
        raise ValueError("plan must have schema 1")
    allowed_plan = {"schema", "name", "duration_ms", "max_frames", "window_id", "expected_window_frame_pt", "capture_fps",
                    "max_frame_gap_ms", "expected_reduce_motion", "capture_scope", "actions", "crops", "case"}
    if set(plan) - allowed_plan:
        raise ValueError(f"unknown plan fields: {sorted(set(plan) - allowed_plan)}")
    case = plan.get("case")
    if not isinstance(case, dict) or set(case) != {"id", "flow", "owner_phase", "motion", "transition", "viewport", "text_scale", "live_index"}:
        raise ValueError("case requires id, flow, owner_phase, motion, transition, viewport, text_scale, live_index")
    if case["flow"] not in {"add", "ask", "find", "drawer", "hand", "settings", "source", "failure_recovery"}:
        raise ValueError("unknown case flow")
    if case["owner_phase"] not in {"starting", "failed", "serving"} or case["motion"] not in {"full", "reduced"}:
        raise ValueError("invalid owner phase or motion")
    if case["transition"] not in {"first_open", "open_close", "resize_midflight", "text_scale_midflight", "failure_recovery", "settle", "retarget"}:
        raise ValueError("invalid transition")
    if case["viewport"] not in {"wide", "narrow", "mixed"} or case["text_scale"] not in {"100", "200", "mixed"}:
        raise ValueError("invalid viewport/text scale")
    if type(case["live_index"]) is not bool or not isinstance(case["id"], str) or not case["id"]:
        raise ValueError("case id and live_index must be typed")
    name = plan.get("name")
    if not isinstance(name, str) or not name or len(name) > 80:
        raise ValueError("plan.name must be 1..80 characters")
    duration = plan.get("duration_ms")
    frames = plan.get("max_frames", min(MAX_FRAMES, math.ceil(duration * 0.06) + 10) if type(duration) is int else 0)
    if type(duration) is not int or not 100 <= duration <= MAX_DURATION_MS:
        raise ValueError("duration_ms must be 100..30000")
    if type(frames) is not int or not 1 <= frames <= MAX_FRAMES:
        raise ValueError("max_frames must be 1..1800")
    window_id = plan.get("window_id")
    if window_id is not None and (type(window_id) is not int or window_id < 1):
        raise ValueError("window_id must be positive")
    expected_frame = plan.get("expected_window_frame_pt")
    if expected_frame is not None and (not isinstance(expected_frame, list) or len(expected_frame) != 4
            or any(not isinstance(value, (int, float)) or not math.isfinite(value) for value in expected_frame)
            or min(expected_frame[2:]) <= 0):
        raise ValueError("expected_window_frame_pt requires finite [x,y,width,height]")
    scope = plan.get("capture_scope", "window")
    if scope not in {"window", "app_display"}:
        raise ValueError("capture_scope must be window or app_display")
    fps = plan.get("capture_fps", 30)
    if type(fps) is not int or not 15 <= fps <= 60:
        raise ValueError("capture_fps must be 15..60")
    motion = plan.get("expected_reduce_motion")
    if motion is not None and type(motion) is not bool:
        raise ValueError("expected_reduce_motion must be boolean")
    if motion is None or motion != (case["motion"] == "reduced"):
        raise ValueError("case motion must match expected_reduce_motion")
    actions = plan.get("actions")
    if not isinstance(actions, list):
        raise ValueError("actions must be a list")
    allowed = {"key", "move", "click", "mouse_down", "mouse_up", "resize", "click_ax", "probe"}
    previous = -1
    button_down = False
    for index, action in enumerate(actions):
        if not isinstance(action, dict) or action.get("kind") not in allowed:
            raise ValueError(f"action {index}: unknown kind")
        allowed_action = {"at_ms", "kind", "label", "keycode", "modifiers", "x", "y",
                          "width", "height", "title", "role", "expect_visual_ms", "min_visual_fraction", "expect_focus_title", "expect_ax_title", "expect_ax_selected_title"}
        if set(action) - allowed_action:
            raise ValueError(f"action {index}: unknown fields {sorted(set(action) - allowed_action)}")
        at = action.get("at_ms")
        if type(at) is not int or not 0 <= at <= duration - 50 or at < previous:
            raise ValueError(f"action {index}: at_ms must be ordered and before capture end")
        previous = at
        if not isinstance(action.get("label"), str) or not action["label"]:
            raise ValueError(f"action {index}: label required")
        kind = action["kind"]
        if kind == "key":
            if type(action.get("keycode")) is not int or not 0 <= action["keycode"] <= 127:
                raise ValueError(f"action {index}: virtual keycode required")
            if any(m not in {"command", "shift", "option", "control"} for m in action.get("modifiers", [])):
                raise ValueError(f"action {index}: invalid modifier")
        if kind in {"move", "click", "mouse_down", "mouse_up"} and not all(isinstance(action.get(k), (int, float)) and math.isfinite(action[k]) for k in ("x", "y")):
            raise ValueError(f"action {index}: finite global point required")
        if kind == "resize" and not all(isinstance(action.get(k), (int, float)) and 320 <= action[k] <= 8000 for k in ("width", "height")):
            raise ValueError(f"action {index}: valid width/height required")
        if kind == "click_ax" and (not isinstance(action.get("title"), str) or not action["title"]):
            raise ValueError(f"action {index}: exact AX title required")
        if kind == "mouse_down":
            if button_down:
                raise ValueError(f"action {index}: nested mouse_down")
            button_down = True
        elif kind == "mouse_up":
            if not button_down:
                raise ValueError(f"action {index}: mouse_up without mouse_down")
            button_down = False
        elif button_down and kind not in {"move", "probe"}:
            raise ValueError(f"action {index}: only move/probe/mouse_up may follow held mouse_down")
        if "expect_visual_ms" in action and (type(action["expect_visual_ms"]) is not int or action["expect_visual_ms"] < 1 or at + action["expect_visual_ms"] > duration):
            raise ValueError(f"action {index}: expect_visual_ms outside capture")
        fraction = action.get("min_visual_fraction", 0.005)
        if not isinstance(fraction, (int, float)) or not math.isfinite(fraction) or not 0 < fraction <= 1:
            raise ValueError(f"action {index}: min_visual_fraction must be 0..1")
        if "expect_focus_title" in action and (not isinstance(action["expect_focus_title"], str) or not action["expect_focus_title"]):
            raise ValueError(f"action {index}: expect_focus_title must be a name")
        if "expect_ax_title" in action and (kind != "probe" or not isinstance(action["expect_ax_title"], str) or not action["expect_ax_title"]):
            raise ValueError(f"action {index}: expect_ax_title requires a named probe")
        if "expect_ax_selected_title" in action and (kind != "probe" or not isinstance(action["expect_ax_selected_title"], str) or not action["expect_ax_selected_title"]):
            raise ValueError(f"action {index}: expect_ax_selected_title requires a named probe")
    if button_down:
        raise ValueError("plan leaves mouse button down")
    crops = plan.get("crops", [])
    if not isinstance(crops, list):
        raise ValueError("crops must be a list")
    for index, crop in enumerate(crops):
        if not isinstance(crop, dict) or not isinstance(crop.get("label"), str):
            raise ValueError(f"crop {index}: label required")
        if type(crop.get("at_ms")) is not int or not 0 <= crop["at_ms"] <= duration:
            raise ValueError(f"crop {index}: at_ms outside capture")
        rect = crop.get("rect_px")
        if not isinstance(rect, list) or len(rect) != 4 or any(type(v) is not int or v < 0 for v in rect) or min(rect[2:]) < 1:
            raise ValueError(f"crop {index}: physical rect_px [left,top,width,height] required")
    coverage = plan.get("max_frame_gap_ms", 50)
    if type(coverage) is not int or not 1 <= coverage <= 1000:
        raise ValueError("max_frame_gap_ms must be 1..1000")
    plan["max_frames"] = frames
    plan["capture_fps"] = fps
    plan["capture_scope"] = scope
    plan["max_frame_gap_ms"] = coverage
    return plan


def cadence(times: list[float]) -> dict[str, Any]:
    if len(times) < 2:
        raise ValueError("at least two native frames required")
    deltas = [b - a for a, b in zip(times, times[1:])]
    if any(not math.isfinite(d) or d <= 0 for d in deltas):
        raise ValueError("native PTS must increase strictly")
    median = statistics.median(deltas)
    maximum_error = max(abs(delta - median) for delta in deltas)
    # Only an actually stable native cadence gets CFR. A content-driven
    # ScreenCaptureKit stream usually has irregular gaps and therefore VFR.
    constant = maximum_error <= max(0.5, median * 0.03)
    return {"encoding": "cfr" if constant else "vfr", "median_interval_ms": median,
            "max_interval_error_ms": maximum_error, "intervals_ms": deltas}


def changed_fraction(first: Path, second: Path) -> float:
    """Fraction of pixels with a visible RGB delta, from only two open PNGs."""
    from PIL import Image, ImageChops
    with Image.open(first) as source, Image.open(second) as target:
        a = source.convert("RGB")
        b = target.convert("RGB")
    if a.size != b.size:
        return 1.0
    gray = ImageChops.difference(a, b).convert("L")
    histogram = gray.histogram()
    return sum(histogram[9:]) / (a.width * a.height)


def clock_alignment(frames: list[dict[str, Any]], actions: list[dict[str, Any]]) -> dict[str, Any]:
    """Measure the SC PTS to monotonic host-clock offset, including callback jitter."""
    offsets = []
    first_pts = frames[0].get("pts_seconds")
    if not isinstance(first_pts, (int, float)) or not math.isfinite(first_pts):
        return {"covered": False, "reason": "first ScreenCaptureKit PTS is absent"}
    for frame in frames:
        host = frame.get("host_capture_ns")
        pts = frame.get("pts_seconds")
        if (type(host) is not int or not isinstance(pts, (int, float)) or not math.isfinite(pts)
                or abs(frame["time_ms"] - (pts - first_pts) * 1000) > 1):
            return {"covered": False, "reason": "frame host time/PTS pair is absent or inconsistent"}
        offsets.append(host / 1_000_000 - pts * 1000)
    for action in actions:
        if action.get("phase") == "action":
            start, end = action.get("dispatch_host_ns"), action.get("posted_host_ns")
            if type(start) is not int or type(end) is not int or end < start:
                return {"covered": False, "reason": "native input dispatch/post host interval is absent or invalid"}
    offset = statistics.median(offsets)
    residual = max(abs(value - offset) for value in offsets)
    offset_low, offset_high = min(offsets), max(offsets)
    covered = residual <= MAX_CLOCK_RESIDUAL_MS and -5 <= offset_low <= offset_high <= MAX_CALLBACK_OFFSET_MS
    return {"covered": covered,
            "reason": None if covered else "SC PTS/host callback offset or residual exceeds bound",
            "offset_host_minus_pts_ms": offset, "first_pts_seconds": first_pts,
            "offset_range_ms": [offset_low, offset_high], "max_residual_ms": residual,
            "residual_bound_ms": MAX_CLOCK_RESIDUAL_MS, "callback_offset_bound_ms": MAX_CALLBACK_OFFSET_MS,
            "frame_pairs": len(offsets)}


def aligned_action_ms(action: dict[str, Any], clock: dict[str, Any]) -> float:
    return round(action["dispatch_host_ns"] / 1_000_000
                 - clock["offset_host_minus_pts_ms"] - clock["first_pts_seconds"] * 1000, 6)


def action_time_bounds(action: dict[str, Any], clock: dict[str, Any]) -> tuple[float, float]:
    dispatch_ms = action["dispatch_host_ns"] / 1_000_000
    posted_ms = action["posted_host_ns"] / 1_000_000
    low, high = clock["offset_range_ms"]
    first_pts_ms = clock["first_pts_seconds"] * 1000
    return round(dispatch_ms - high - first_pts_ms, 6), round(posted_ms - low - first_pts_ms, 6)


def fresh_ax(snapshot: dict[str, Any], before_host_ns: int, after_host_ns: int | None = None) -> bool:
    if not isinstance(snapshot, dict):
        return False
    start = snapshot.get("sample_start_host_ns")
    end = snapshot.get("sample_end_host_ns")
    return (type(start) is int and type(end) is int and start <= end <= before_host_ns
            and (after_host_ns is None or start >= after_host_ns)
            and (before_host_ns - end) / 1_000_000 <= MAX_AX_SAMPLE_AGE_MS
            and (end - start) / 1_000_000 <= MAX_AX_SAMPLE_AGE_MS)


def ax_has_title(snapshot: dict[str, Any], title: str) -> bool:
    focused = snapshot.get("focused") or {}
    return title in {focused.get("title"), focused.get("description")}


def analyze_frames(out: Path, frames: list[dict[str, Any]], actions: list[dict[str, Any]], plan: dict[str, Any]) -> dict[str, Any]:
    """One-frame-at-a-time pixel/geometry/AX analysis; no RGBA frame list."""
    from PIL import Image, ImageChops, ImageStat
    changes: list[dict[str, Any]] = []
    sizes: list[dict[str, Any]] = []
    focus: list[dict[str, Any]] = []
    focus_outside_window: list[dict[str, Any]] = []
    window_geometry: list[dict[str, Any]] = []
    previous_window = None
    previous = None
    previous_size = None
    previous_focus = None
    fresh_ax_frames = 0
    clock = clock_alignment(frames, actions)
    failures = []
    if not clock["covered"]:
        failures.append("native frame/input clock alignment uncovered: " + clock["reason"])
    for frame in frames:
        with Image.open(out / frame["file"]) as opened:
            current = opened.convert("RGB")
        size = current.size
        if size != (frame["width_px"], frame["height_px"]):
            raise ValueError(f"PNG {frame['file']} differs from recorded native pixel dimensions")
        ax = frame.get("ax") or {}
        ax_current = fresh_ax(ax, frame.get("host_capture_ns", -1)) if clock["covered"] else False
        fresh_ax_frames += int(ax_current)
        if previous_size != size:
            sizes.append({"at_ms": frame["time_ms"], "size_px": list(size),
                          "ax_window_bounds_pt": (ax.get("window") or {}).get("bounds_pt") if ax_current else None})
            previous_size = size
        window_node = (ax.get("window") or {}) if ax_current else {}
        window_rect = window_node.get("bounds_pt")
        if window_rect != previous_window:
            window_geometry.append({"at_ms": frame["time_ms"], "window": window_node})
            previous_window = window_rect
        focused = (ax.get("focused") or {}) if ax_current else {}
        focus_rect = focused.get("bounds_pt")
        if focus_rect and window_rect:
            overlap = (focus_rect["x"] < window_rect["x"] + window_rect["width"] and
                       focus_rect["x"] + focus_rect["width"] > window_rect["x"] and
                       focus_rect["y"] < window_rect["y"] + window_rect["height"] and
                       focus_rect["y"] + focus_rect["height"] > window_rect["y"])
            if not overlap:
                focus_outside_window.append({"at_ms": frame["time_ms"], "focused": focused, "window": window_node})
        focus_key = (focused.get("role"), focused.get("title"), focused.get("description"))
        if focus_key != previous_focus:
            focus.append({"at_ms": frame["time_ms"], "focused": focused,
                          "ax_sample_end_host_ns": ax.get("sample_end_host_ns") if ax_current else None})
            previous_focus = focus_key
        if previous is not None:
            if previous.size == size:
                diff = ImageChops.difference(previous, current)
                box = diff.getbbox()
                mean_delta = statistics.mean(ImageStat.Stat(diff).mean)
            else:
                box = (0, 0, size[0], size[1])
                mean_delta = None
            changes.append({"at_ms": frame["time_ms"], "changed_bounds_px": list(box) if box else None,
                            "mean_rgb_delta": mean_delta, "geometry_changed": previous.size != size})
        previous = current
    actual_actions = [event for event in actions if event.get("phase") == "action"]
    action_results = []
    coverage_findings = []
    for index, (requested, actual) in enumerate(zip(plan["actions"], actual_actions)):
        at = aligned_action_ms(actual, clock) if clock["covered"] else actual["actual_ms"]
        action_low, action_high = action_time_bounds(actual, clock) if clock["covered"] else (at, at)
        next_mutation = next((action_time_bounds(event, clock)[0] if clock["covered"] else event["actual_ms"]
                              for event in actual_actions[index + 1:] if event.get("kind") != "probe"), None)
        visual_deadline = requested.get("expect_visual_ms")
        end = min(plan["duration_ms"], action_low + visual_deadline if visual_deadline is not None else plan["duration_ms"],
                  next_mutation if next_mutation is not None else float("inf"))
        before = [frame for frame in frames if frame["time_ms"] <= action_low]
        baseline = max(before, key=lambda frame: frame["time_ms"]) if before else None
        observed = [frame for frame in frames if action_high < frame["time_ms"] < end]
        baseline_fresh = baseline is not None and action_low - baseline["time_ms"] <= plan["max_frame_gap_ms"]
        tail_fresh = bool(observed) and end - observed[-1]["time_ms"] <= plan["max_frame_gap_ms"]
        interval_times = ([baseline["time_ms"]] if baseline is not None else []) + [frame["time_ms"] for frame in observed]
        gaps_covered = all(b - a <= plan["max_frame_gap_ms"] for a, b in zip(interval_times, interval_times[1:]))
        visual_covered = clock["covered"] and baseline_fresh and tail_fresh and gaps_covered
        min_fraction = requested.get("min_visual_fraction", 0.005)
        first_change = None
        first_fraction = None
        read_only = actual.get("posted", {}).get("disposition") == "ReadOnlyOutOfScope"
        if clock["covered"] and baseline_fresh and not read_only:
            for frame in observed:
                fraction = changed_fraction(out / baseline["file"], out / frame["file"])
                if fraction >= min_fraction:
                    first_change = frame["time_ms"]
                    first_fraction = fraction
                    break
        expected_title = requested.get("expect_focus_title")
        dispatch_host = actual.get("dispatch_host_ns", -1)
        before_ax = actual.get("before") or {}
        first_focus = None
        focus_already_present = bool(expected_title and fresh_ax(before_ax, dispatch_host)
                                     and ax_has_title(before_ax, expected_title))
        if focus_already_present:
            first_focus = at
        elif expected_title and clock["covered"]:
            first_focus = next((frame["time_ms"] for frame in observed
                                if fresh_ax(frame.get("ax") or {}, frame["host_capture_ns"], dispatch_host)
                                and ax_has_title(frame["ax"], expected_title)), None)
        if read_only:
            failures.append(f"{actual['label']}: read-only out-of-scope native input: {actual['posted'].get('reason')}")
        resize_bounds = actual.get("posted", {}).get("window_after_pt", {}) if requested["kind"] == "resize" else None
        if resize_bounds is not None:
            if abs(resize_bounds.get("width", -10000) - requested["width"]) > 3 or abs(resize_bounds.get("height", -10000) - requested["height"]) > 3:
                failures.append(f"{actual['label']}: AX window did not reach requested size")
        expected_ax = requested.get("expect_ax_title")
        probe_ax = actual.get("posted", {}).get("ax") or {}
        probe_fresh = fresh_ax(probe_ax, actual.get("posted_host_ns", -1), dispatch_host)
        if requested["kind"] == "probe" and not probe_fresh:
            failures.append(f"{actual['label']}: AX probe sample is absent or stale")
        ax_nodes = probe_ax.get("tree", []) if probe_fresh else []
        ax_found = any(expected_ax in {node.get("title"), node.get("description")} for node in ax_nodes) if expected_ax else None
        if expected_ax and not ax_found:
            failures.append(f"{actual['label']}: fresh AX probe did not contain {expected_ax!r}")
        selected_title = requested.get("expect_ax_selected_title")
        selected_found = any(selected_title in {node.get("title"), node.get("description")} and node.get("selected") is True
                             for node in ax_nodes) if selected_title else None
        if selected_title and not selected_found:
            failures.append(f"{actual['label']}: fresh AX probe did not select {selected_title!r}")
        prior_trees = [(0, event.get("ax", {}).get("tree")) for event in actions
                       if event.get("phase") == "initial"
                       and fresh_ax(event.get("ax") or {}, dispatch_host)]
        prior_trees += [(event.get("actual_ms", -1), event.get("posted", {}).get("ax", {}).get("tree"))
                        for event in actions if event.get("phase") == "action" and event.get("kind") == "probe"
                        and event.get("dispatch_host_ns", 1e20) < dispatch_host
                        and fresh_ax(event.get("posted", {}).get("ax") or {}, dispatch_host)]
        baseline_tree = max(prior_trees, default=(0, None), key=lambda pair: pair[0])[1]
        probes = [event for event in actions if event.get("phase") == "action" and event.get("kind") == "probe"
                  and event.get("dispatch_host_ns", -1) >= dispatch_host
                  and (not clock["covered"] or aligned_action_ms(event, clock) < end)
                  and fresh_ax(event.get("posted", {}).get("ax") or {},
                               event.get("posted_host_ns", -1), event.get("dispatch_host_ns", -1))]
        ax_changed = any(event.get("posted", {}).get("ax", {}).get("tree") != baseline_tree for event in probes) if baseline_tree is not None else None
        ax_pixel_divergence = bool(ax_changed and visual_deadline is not None and visual_covered and first_change is None)
        if visual_deadline is not None and first_change is None and not read_only:
            if visual_covered:
                failures.append(f"{actual['label']}: no native pixel change within attributable response interval")
            else:
                reason = f"{actual['label']}: native frame/clock coverage does not span attributable response interval"
                coverage_findings.append(reason)
                failures.append(reason)
        if expected_title and first_focus is None:
            failures.append(f"{actual['label']}: AX focus never became {expected_title!r}")
        action_results.append({"label": actual["label"], "kind": actual["kind"], "posted_at_ms": at,
                               "action_pts_bounds_ms": [action_low, action_high],
                               "response_interval_end_ms": end, "next_mutation_ms": next_mutation,
                               "visual_coverage": "covered" if visual_covered else "uncovered",
                               "first_visual_change_ms": first_change, "visual_latency_ms": None if first_change is None else first_change - at,
                               "first_visual_fraction": first_fraction, "min_visual_fraction": min_fraction,
                               "expected_focus_title": expected_title, "first_expected_focus_ms": first_focus,
                               "expected_focus_already_present": focus_already_present,
                               "expected_ax_title": expected_ax, "ax_found": ax_found,
                               "expected_ax_selected_title": selected_title, "ax_selected_found": selected_found,
                               "resize_window_after_pt": resize_bounds, "ax_tree_changed": ax_changed,
                               "ax_pixel_divergence": ax_pixel_divergence})
    if len(actual_actions) != len(plan["actions"]):
        failures.append("not every timed native action has a successful posted event")
    if fresh_ax_frames == 0:
        failures.append("native frame AX samples are absent or stale")
    gaps = [b["time_ms"] - a["time_ms"] for a, b in zip(frames, frames[1:])]
    max_gap = max(gaps, default=0)
    if max_gap > plan["max_frame_gap_ms"]:
        reason = f"native SC frame coverage gap {max_gap:.2f} ms exceeds {plan['max_frame_gap_ms']} ms; no GUI jank inference"
        coverage_findings.append(reason)
        failures.append(reason)
    if frames[-1]["time_ms"] < plan["duration_ms"] - plan["max_frame_gap_ms"]:
        failures.append("native frames stopped before the requested capture tail")
    jump_candidates = [change for change in changes if change["mean_rgb_delta"] is not None
                       and change["mean_rgb_delta"] > 30]
    return {"frame_count": len(frames), "last_frame_ms": frames[-1]["time_ms"],
            "max_frame_gap_ms": max_gap, "size_changes": sizes, "window_geometry_changes": window_geometry,
            "clock_alignment": clock, "coverage_findings": coverage_findings,
            "fresh_ax_frame_count": fresh_ax_frames,
            "focus_changes": focus, "focus_outside_window": focus_outside_window,
            "pixel_changes": changes, "visual_jump_candidates": jump_candidates,
            "actions": action_results, "failures": failures}


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def crop_frames(out: Path, plan: dict[str, Any], frames: list[dict[str, Any]]) -> list[dict[str, Any]]:
    if not plan.get("crops"):
        return []
    from PIL import Image
    directory = out / "crops"
    directory.mkdir(exist_ok=True)
    results = []
    for crop in plan["crops"]:
        frame = min(frames, key=lambda f: abs(f["time_ms"] - crop["at_ms"]))
        left, top, width, height = crop["rect_px"]
        if left + width > frame["width_px"] or top + height > frame["height_px"]:
            raise ValueError(f"crop {crop['label']}: rect outside actual {frame['width_px']}x{frame['height_px']} frame")
        path = directory / f"{len(results):03d}-{crop['label']}.png"
        with Image.open(out / frame["file"]) as image:
            if image.size != (frame["width_px"], frame["height_px"]):
                raise ValueError("recorded pixel size disagrees with PNG")
            image.crop((left, top, left + width, top + height)).save(path)
        results.append({"label": crop["label"], "requested_at_ms": crop["at_ms"],
                        "actual_at_ms": frame["time_ms"], "source": frame["file"],
                        "source_size_px": [frame["width_px"], frame["height_px"]],
                        "rect_px": crop["rect_px"], "file": str(path.relative_to(out)), "sha256": sha256(path)})
    return results


def encode(out: Path, frames: list[dict[str, Any]], timing: dict[str, Any], ffmpeg: str) -> dict[str, Any]:
    movie = out / "native-motion.mp4"
    if timing["encoding"] == "cfr":
        # Use measured native cadence, not the requested SCStream max rate.
        fps = 1000 / timing["median_interval_ms"]
        command = [ffmpeg, "-hide_banner", "-nostdin", "-y", "-framerate", f"{fps:.9f}",
                   "-i", str(out / "frames/frame-%06d.png")]
    else:
        concat = out / "frames.ffconcat"
        lines = ["ffconcat version 1.0"]
        for frame, next_frame in zip(frames, frames[1:]):
            lines += [f"file 'frames/{Path(frame['file']).name}'", "option framerate 1000",
                      f"duration {(next_frame['time_ms'] - frame['time_ms']) / 1000:.9f}"]
        # ffmpeg concat requires a final file after the last duration; the
        # last held frame is visible for the measured median interval.
        lines += [f"file 'frames/{Path(frames[-1]['file']).name}'", "option framerate 1000",
                  f"duration {timing['median_interval_ms'] / 1000:.9f}",
                  f"file 'frames/{Path(frames[-1]['file']).name}'", "option framerate 1000"]
        concat.write_text("\n".join(lines) + "\n")
        command = [ffmpeg, "-hide_banner", "-nostdin", "-y", "-safe", "0", "-f", "concat", "-i", str(concat), "-fps_mode", "vfr"]
    command += ["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2", "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(movie)]
    result = subprocess.run(command, cwd=out, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (out / "ffmpeg.log").write_text(result.stdout)
    if result.returncode:
        raise RuntimeError(f"ffmpeg exit {result.returncode}; see {out / 'ffmpeg.log'}")
    ffprobe = str(Path(ffmpeg).with_name("ffprobe"))
    if not Path(ffprobe).is_file():
        ffprobe = shutil.which("ffprobe") or ""
    if not ffprobe:
        raise RuntimeError("ffprobe is required to verify encoded movie PTS")
    probe = subprocess.run([ffprobe, "-v", "error", "-select_streams", "v:0", "-show_entries",
                            "frame=best_effort_timestamp_time", "-of", "csv=p=0", str(movie)],
                           text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if probe.returncode:
        raise RuntimeError(f"ffprobe could not verify native movie: {probe.stderr}")
    encoded_pts = [float(line.split(",")[0]) * 1000 for line in probe.stdout.splitlines() if line.strip()]
    expected_pts = [frame["time_ms"] - frames[0]["time_ms"] for frame in frames]
    if len(encoded_pts) < len(expected_pts) or any(abs(actual - expected) > 2 for actual, expected in zip(encoded_pts, expected_pts)):
        raise RuntimeError("encoded movie PTS diverged from native frame timestamps")
    return {"file": movie.name, "sha256": sha256(movie), "argv": command, "timing": timing,
            "encoded_frame_count": len(encoded_pts), "max_pts_error_ms": max(abs(actual - expected) for actual, expected in zip(encoded_pts, expected_pts)),
            "last_frame_hold_ms": timing["median_interval_ms"]}


def run19_compiler_admission(path: Path, receipt: dict[str, Any], binary: Path,
                             source: dict[str, str], preservation_path: Path | None) -> dict[str, Any]:
    """Admit the actual Run19 receipt and a separate proof of its copied .app."""
    reasons = []
    binary_link = symlink_in_artifact_path(binary)
    binary = binary.resolve(strict=True)
    bundle = bundle_identity(binary)
    binary_sha = sha256(binary)
    if source["status_porcelain"] or receipt.get("frozen_source_clean") is not True or \
            receipt.get("source_changed_during_build") is not False or receipt.get("tracked_status_after") != "":
        reasons.append("Run19 frozen source was not clean and unchanged")
    if (receipt.get("frozen_source_path"), receipt.get("frozen_source_head"),
            receipt.get("frozen_source_tree"), receipt.get("frozen_source_lock_sha256")) != \
            (source["path"], source["head"], source["tree"], source["cargo_lock_sha256"]):
        reasons.append("Run19 frozen source path/HEAD/tree/lock differs from capture checkout")
    if (receipt.get("head"), receipt.get("tree"), receipt.get("lock_sha256")) != \
            (source["head"], source["tree"], source["cargo_lock_sha256"]):
        reasons.append("Run19 producing worktree commit differs from frozen source")
    if receipt.get("exit_status") != 0 or receipt.get("census_complete") is not True or \
            type(receipt.get("max_conservative_local_occupancy_sampled")) is not int or \
            type(receipt.get("reserved_remote_builds")) is not int or \
            not 0 <= receipt["max_conservative_local_occupancy_sampled"] <= 3 or \
            receipt["reserved_remote_builds"] != 1 or \
            receipt["max_conservative_local_occupancy_sampled"] + receipt["reserved_remote_builds"] > 4:
        reasons.append("Run19 build/census/cap receipt is invalid")
    if binary_link or not binary.is_file() or binary.stat().st_nlink != 1 or \
            receipt.get("qa_binary_regular_file") is not True:
        reasons.append("Run19 capture artifact is not a regular single-link QA copy")
    if receipt.get("qa_binary_path") != str(binary) or receipt.get("qa_binary_sha256") != binary_sha or \
            bundle is None or receipt.get("qa_bundle_path") != (bundle or {}).get("path") or \
            receipt.get("qa_info_plist_sha256") != (bundle or {}).get("info_sha256") or \
            receipt.get("qa_bundle_identifier") != (bundle or {}).get("identifier"):
        reasons.append("Run19 QA binary/Info.plist/bundle identity differs")
    command_path = Path(receipt.get("command_path", ""))
    raw_path = Path(receipt.get("raw_log_path", ""))
    if command_path != path.with_name("command.sh") or not command_path.is_file() or \
            sha256(command_path) != receipt.get("command_sha256"):
        reasons.append("Run19 command.sh path or SHA differs")
    if raw_path != path.with_name("raw.log") or not raw_path.is_file() or \
            sha256(raw_path) != receipt.get("raw_log_sha256"):
        reasons.append("Run19 raw.log path or SHA differs")
    command = command_path.read_text() if command_path.is_file() else ""
    saved = [Path(parts[1]) for line in command.splitlines()
             if len(parts := shlex.split(line, comments=False, posix=True)) == 2 and parts[0] == "source"]
    if len(saved) != 1 or not saved[0].is_file() or \
            sha256(saved[0]) != receipt.get("development_environment_sha256"):
        reasons.append("Run19 saved development environment differs")
    provenance_path = Path(receipt.get("provenance_path", ""))
    provenance = {}
    if not provenance_path.is_file() or sha256(provenance_path) != receipt.get("provenance_sha256"):
        reasons.append("Run19 producer provenance path or SHA differs")
    else:
        provenance = json.loads(provenance_path.read_text())
        toolchain = provenance.get("toolchain", {})
        if provenance.get("schema") != 2 or provenance.get("run_id") != receipt.get("provenance_run_id") or \
                provenance.get("workspace_root") != receipt.get("worktree") or \
                provenance.get("git_head") != source["head"] or \
                provenance.get("cargo_lock_sha256") != source["cargo_lock_sha256"] or \
                provenance.get("cargo_lock_sha256_after") != source["cargo_lock_sha256"] or \
                provenance.get("cargo_exit_status") != 0 or \
                provenance.get("source_changed_during_build") is not False or \
                toolchain.get("capture_complete") is not True or toolchain.get("changed_during_build") is not False or \
                "rustc 1.97.1" not in toolchain.get("rustc", ""):
            reasons.append("Run19 producer provenance does not prove the frozen compiler run")
    binaries = receipt.get("binaries", {})
    matches = [(name, digest) for name, digest in binaries.items() if digest == binary_sha] if isinstance(binaries, dict) else []
    compiled_path = matches[0][0] if len(matches) == 1 else None
    if compiled_path is None or not any(
            str(Path(provenance.get("cargo_target_dir", "")) / item.get("path", "")) == compiled_path
            and item.get("sha256") == binary_sha for item in provenance.get("outputs", []) if isinstance(item, dict)):
        reasons.append("Run19 QA SHA does not identify exactly one recorded compiler output")
    canary_path = path.with_name("root-canary-receipt.json")
    canary = json.loads(canary_path.read_text()) if canary_path.is_file() else {}
    if canary.get("schema") != "root-candidate-verification.v1" or \
            canary.get("compiler_receipt_path") != str(path) or \
            canary.get("compiler_receipt_sha256") != sha256(path) or \
            (canary.get("head"), canary.get("tree"), canary.get("qa_binary"), canary.get("sha256")) != \
            (source["head"], source["tree"], str(binary), binary_sha) or \
            not isinstance(canary.get("checks"), dict) or not canary["checks"] or \
            any(value is not True for value in canary["checks"].values()):
        reasons.append("Run19 independent root canary is absent or failed")
    preservation = json.loads(preservation_path.read_text()) if preservation_path is not None else {}
    expected_preservation = {"schema": 2, "kind": "run19-qa-preservation",
        "compiler_receipt_sha256": sha256(path), "provenance_sha256": receipt.get("provenance_sha256"),
        "compiled_artifact_path": compiled_path, "compiled_artifact_sha256": binary_sha,
        "capture_source_path": source["path"], "source_head": source["head"],
        "source_tree": source["tree"], "cargo_lock_sha256": source["cargo_lock_sha256"],
        "capture_artifact_path": str(binary), "capture_artifact_sha256": binary_sha,
        "capture_bundle_info_path": (bundle or {}).get("info_path"),
        "capture_bundle_info_sha256": (bundle or {}).get("info_sha256"),
        "capture_bundle_identifier": (bundle or {}).get("identifier")}
    if preservation != expected_preservation:
        reasons.append("Run19 separate hash-bound QA preservation proof is absent or differs")
    return {"state": "VerifiedBuildReceipt" if not reasons else "UnprovenBinarySource",
            "reasons": reasons, "receipt_path": str(path), "receipt_sha256": sha256(path),
            "receipt_kind": receipt["kind"], "build_cwd": receipt.get("worktree"),
            "compiled_artifact_path": compiled_path, "capture_source_path": source["path"],
            "capture_artifact_path": str(binary), "capture_artifact_sha256": binary_sha,
            "capture_bundle": bundle, "provenance_path": str(provenance_path),
            "provenance_sha256": receipt.get("provenance_sha256"),
            "canary_path": str(canary_path), "canary_sha256": sha256(canary_path) if canary_path.is_file() else None,
            "preservation_receipt_path": str(preservation_path) if preservation_path else None,
            "preservation_receipt_sha256": sha256(preservation_path) if preservation_path else None,
            "command_sha256": receipt.get("command_sha256"), "raw_log_sha256": receipt.get("raw_log_sha256")}


def compiler_admission(receipt_path: Path | None, binary: Path, source: dict[str, str],
                       preservation_path: Path | None = None) -> dict[str, Any]:
    """Admit only the root build's completed, source-bound compiler receipt."""
    if receipt_path is None:
        return {"state": "UnprovenBinarySource", "reasons": ["no compiler receipt supplied"]}
    symlink = symlink_in_artifact_path(binary)
    binary = binary.resolve(strict=True)
    bundle = bundle_identity(binary)
    path = receipt_path.resolve(strict=True)
    receipt = json.loads(path.read_text())
    if receipt.get("kind") == "desktop-bins-build":
        return run19_compiler_admission(path, receipt, binary, source,
                                         preservation_path.resolve(strict=True) if preservation_path else None)
    reasons = []
    if source["status_porcelain"]:
        reasons.append("source checkout is not frozen clean")
    if receipt.get("head") != source["head"] or receipt.get("tree") != source["tree"]:
        reasons.append("receipt HEAD/tree differs from capture checkout")
    if receipt.get("lock_sha256") != source["cargo_lock_sha256"]:
        reasons.append("receipt Cargo.lock digest differs")
    if type(receipt.get("exit_status")) is not int or receipt["exit_status"] != 0 or receipt.get("frozen_preserved") is not True or receipt.get("capacity_abort") is not False:
        reasons.append("compiler run did not finish successfully on a frozen source")
    maximum = receipt.get("maximum_local_cargo")
    cap = receipt.get("local_cargo_cap")
    if receipt.get("census_valid") is not True or type(maximum) is not int or type(cap) is not int or maximum < 0 or cap < 1 or maximum > cap:
        reasons.append("compiler process census/cap is invalid")
    if not isinstance(receipt.get("rustc_version"), str) or "rustc " not in receipt["rustc_version"]:
        reasons.append("rustc toolchain identity missing")
    if symlink or not binary.is_file():
        reasons.append("capture binary must be a regular copied artifact, not a symlink")
    if preservation_path is not None and binary.stat().st_nlink != 1:
        reasons.append("preserved capture binary must not be hard-linked to a mutable build artifact")
    command_path = path.with_name("command.sh")
    command = command_path.read_text() if command_path.is_file() else ""
    if not command or hashlib.sha256(command.encode()).hexdigest() != receipt.get("command_sha256"):
        reasons.append("compiler command.sh digest differs")
    argv = receipt.get("build_argv")
    if not isinstance(argv, list) or len(argv) != 3 or argv[1] != "-c" or argv[2] != command or not all(isinstance(part, str) for part in argv):
        reasons.append("actual compiler argv does not contain the recorded command")
    raw_log = path.with_name("raw.log")
    if not raw_log.is_file() or sha256(raw_log) != receipt.get("raw_log_sha256"):
        reasons.append("compiler raw log digest differs")
    saved = []
    for line in command.splitlines():
        parts = shlex.split(line, comments=False, posix=True)
        if len(parts) == 2 and parts[0] == "source":
            saved.append(Path(parts[1]))
    if len(saved) != 1 or not saved[0].is_file() or sha256(saved[0]) != receipt.get("development_environment_sha256"):
        reasons.append("saved build environment digest differs")
    binaries = receipt.get("binaries")
    binary_sha = sha256(binary)
    matches = [(name, value) for name, value in binaries.items()
               if isinstance(name, str) and value == binary_sha] if isinstance(binaries, dict) else []
    preservation = None
    preservation_sha = None
    if preservation_path is not None:
        preservation_path = preservation_path.resolve(strict=True)
        preservation_sha = sha256(preservation_path)
        preservation = json.loads(preservation_path.read_text())
    compiled_path = None
    if preservation is not None:
        compiled_path = preservation.get("compiled_artifact_path")
        if (preservation.get("schema") != 1
                or preservation.get("compiler_receipt_sha256") != sha256(path)
                or preservation.get("build_cwd") != receipt.get("build_cwd")
                or preservation.get("capture_source_path") != source["path"]
                or preservation.get("source_head") != source["head"]
                or preservation.get("source_tree") != source["tree"]
                or preservation.get("cargo_lock_sha256") != source["cargo_lock_sha256"]
                or preservation.get("capture_artifact_path") != str(binary)
                or preservation.get("capture_artifact_sha256") != binary_sha
                or preservation.get("compiled_artifact_sha256") != binary_sha
                or (bundle is not None and
                    (preservation.get("capture_bundle_info_path") != bundle["info_path"]
                     or preservation.get("capture_bundle_info_sha256") != bundle["info_sha256"]
                     or preservation.get("capture_bundle_identifier") != bundle["identifier"]))
                or (compiled_path, binary_sha) not in matches):
            reasons.append("preservation receipt does not bind this source and copied compiler artifact")
    elif receipt.get("build_cwd") != source["path"] or not any(Path(name).resolve() == binary for name, _ in matches):
        reasons.append("distinct source or copied binary requires a hash-bound preservation receipt")
    if not matches:
        reasons.append("binary SHA is not the one produced by this compiler run")
    return {"state": "VerifiedBuildReceipt" if not reasons else "UnprovenBinarySource",
            "reasons": reasons, "receipt_path": str(path), "receipt_sha256": sha256(path),
            "build_cwd": receipt.get("build_cwd"), "compiled_artifact_path": compiled_path or
                (next((name for name, _ in matches if Path(name).resolve() == binary), None)),
            "capture_source_path": source["path"], "capture_artifact_path": str(binary),
            "capture_artifact_sha256": binary_sha,
            "capture_bundle": bundle,
            "preservation_receipt_path": str(preservation_path) if preservation_path else None,
            "preservation_receipt_sha256": preservation_sha,
            "command_sha256": receipt.get("command_sha256"), "raw_log_sha256": receipt.get("raw_log_sha256")}


def run(args: argparse.Namespace) -> Path:
    if sys.platform != "darwin":
        raise ValueError("native capture requires macOS")
    out = args.out.resolve()
    source_root = args.source.resolve(strict=True)
    if out in {ROOT, source_root} or ROOT in out.parents or source_root in out.parents:
        raise ValueError("native artifact output must be outside the tool and candidate source checkouts")
    if out.exists() and any(out.iterdir()):
        raise ValueError(f"output directory must be empty to prevent stale frame evidence: {out}")
    out.mkdir(parents=True, exist_ok=True)
    plan = require_plan(args.plan)
    if symlink_in_artifact_path(args.binary):
        raise ValueError("capture executable must be an owned regular file, not a symlink")
    expected = args.binary.resolve(strict=True)
    bundle = bundle_identity(expected)
    actual = process_executable(args.pid)
    if actual != expected:
        raise ValueError(f"PID {args.pid} runs {actual}, not {expected}")
    inputs = []
    for path in args.input:
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.stat().st_size > MAX_INPUT_BYTES:
            raise ValueError(f"input must be a regular file <= {MAX_INPUT_BYTES} bytes: {resolved}")
        inputs.append({"path": str(resolved), "bytes": resolved.stat().st_size, "sha256": sha256(resolved)})
    source = source_identity(source_root)
    tool = tool_identity()
    if tool["status_porcelain"]:
        raise ValueError("native recorder tool checkout is not frozen clean")
    admission = compiler_admission(args.compiler_receipt, expected, source, args.preservation_receipt)
    manifest: dict[str, Any] = {"schema": 1, "class": "native_window_compositor_and_ax",
        "live_owner_index_admission": "unverified; pair with a production owner/read receipt",
        "name": plan["name"], "case": plan["case"], "pid": args.pid, "binary": {"path": str(expected), "sha256": sha256(expected)},
        "bundle": bundle,
        "plan": {"path": str(args.plan.resolve()), "sha256": sha256(args.plan.resolve())},
        "tool": tool,
        "source": source, "inputs": inputs, "binary_source_admission": admission,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "assertions": ["native SCWindow PID/window identity", "native PTS strictly increasing", "AX and action sidecars", "full PNG dimensions and crop bounds"]}
    if args.owner_receipt:
        receipt = args.owner_receipt.resolve(strict=True)
        manifest["owner_receipt"] = {"path": str(receipt), "sha256": sha256(receipt)}
    (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    recorder = args.recorder.resolve() if args.recorder else out / "native-motion-recorder"
    if not args.recorder:
        compiler = shutil.which("swiftc")
        if not compiler:
            raise ValueError("swiftc unavailable; pass --recorder")
        result = subprocess.run([compiler, "-parse-as-library", "-O", str(SWIFT), "-o", str(recorder)],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        (out / "swiftc.log").write_text(result.stdout)
        if result.returncode:
            raise RuntimeError(f"Swift recorder compile failed; see {out / 'swiftc.log'}")
    manifest["recorder_sha256"] = sha256(recorder)
    plan_path = out / "resolved-plan.json"
    plan_path.write_text(json.dumps(plan, indent=2) + "\n")
    result = subprocess.run([str(recorder), str(args.pid), str(plan_path), str(out)],
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            timeout=plan["duration_ms"] / 1000 + 15)
    (out / "recorder.log").write_text(result.stdout)
    manifest["recorder_exit"] = result.returncode
    if result.returncode:
        (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise RuntimeError(f"native recorder exit {result.returncode}; see {out / 'recorder.log'}")
    frames = read_jsonl(out / "frames.jsonl")
    if not frames or any(f["index"] != i for i, f in enumerate(frames)):
        raise ValueError("native frames missing or out of order")
    timing = cadence([f["time_ms"] for f in frames])
    actual_files = []
    for frame in frames:
        path = out / frame["file"]
        if not path.is_file():
            raise ValueError(f"missing frame {path}")
        actual_files.append({"file": frame["file"], "at_ms": frame["time_ms"],
                             "size_px": [frame["width_px"], frame["height_px"]], "sha256": sha256(path),
                             "pts_seconds": frame.get("pts_seconds"), "host_capture_ns": frame.get("host_capture_ns"),
                             "ax_sample_start_host_ns": frame.get("ax", {}).get("sample_start_host_ns"),
                             "ax_sample_end_host_ns": frame.get("ax", {}).get("sample_end_host_ns"),
                             "focused": frame.get("ax", {}).get("focused")})
    result_row = read_jsonl(out / "result.jsonl")[0]
    if result_row["captured_frames"] != len(frames) or result_row.get("stream_failure"):
        raise ValueError("native stream failed or frame count mismatched")
    manifest["window"] = read_jsonl(out / "window.jsonl")[0]
    if bundle and manifest["window"].get("bundle_identifier") != bundle["identifier"]:
        raise ValueError("running PID bundle identifier differs from the copied bundle Info.plist")
    if plan.get("expected_reduce_motion") is not None and manifest["window"]["reduce_motion"] != plan["expected_reduce_motion"]:
        raise ValueError("macOS reduce-motion state did not match this capture plan")
    manifest["actions"] = read_jsonl(out / "actions.jsonl")
    final_motion = manifest["actions"][-1].get("reduce_motion") if manifest["actions"] else None
    if final_motion != manifest["window"]["reduce_motion"]:
        raise ValueError("macOS reduce-motion state changed during the native capture")
    manifest["frames"] = actual_files
    manifest["analysis"] = analyze_frames(out, frames, manifest["actions"], plan)
    (out / "ANALYSIS.json").write_text(json.dumps(manifest["analysis"], indent=2) + "\n")
    manifest["frame_count"] = len(frames)
    manifest["stream_dropped_frames"] = result_row["dropped_frames"]
    if result_row["dropped_frames"]:
        manifest["analysis"]["failures"].append(f"{result_row['dropped_frames']} native frames discarded at max_frames bound")
    manifest["crops"] = crop_frames(out, plan, frames)
    manifest["movie"] = encode(out, frames, timing, args.ffmpeg)
    if not manifest["window"]["ax_trusted"]:
        manifest["analysis"]["failures"].append("macOS Accessibility permission absent; AX proof unavailable")
    if admission["state"] != "VerifiedBuildReceipt":
        manifest["analysis"]["failures"].append("UnprovenBinarySource: " + "; ".join(admission["reasons"]))
    manifest["passed_native_checks"] = not manifest["analysis"]["failures"]
    stable = process_executable(args.pid) == expected and sha256(expected) == manifest["binary"]["sha256"]
    stable = stable and source_identity(source_root) == source and tool_identity() == tool
    stable = stable and bundle_identity(expected) == bundle
    stable = stable and sha256(args.plan.resolve()) == manifest["plan"]["sha256"]
    if args.compiler_receipt:
        stable = stable and sha256(args.compiler_receipt.resolve()) == admission["receipt_sha256"]
        stable = stable and compiler_admission(args.compiler_receipt, expected, source, args.preservation_receipt) == admission
    stable = stable and all(sha256(Path(item["path"])) == item["sha256"] for item in inputs)
    if args.owner_receipt:
        stable = stable and sha256(args.owner_receipt.resolve()) == manifest["owner_receipt"]["sha256"]
    manifest["capture_inputs_stable"] = stable
    if not stable:
        manifest["analysis"]["failures"].append("binary/candidate source/tool/selected input or owner receipt changed during capture")
        manifest["passed_native_checks"] = False
    manifest["completed_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if not manifest["passed_native_checks"]:
        raise RuntimeError("native capture has AX/coverage/action findings; see CAPTURE.json")
    return out / "CAPTURE.json"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, required=True, help="PID of the visible native desktop process")
    parser.add_argument("--binary", type=Path, required=True, help="exact executable that PID must be running")
    parser.add_argument("--source", type=Path, required=True, help="frozen candidate checkout named by the compiler receipt; independent of this recorder checkout")
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--input", type=Path, action="append", default=[], help="immutable live input file to hash; repeat")
    parser.add_argument("--owner-receipt", type=Path, help="independent production owner/read evidence to hash, not inferred from pixels")
    parser.add_argument("--compiler-receipt", type=Path, help="completed frozen-source build receipt; absent/mismatch is UnprovenBinarySource")
    parser.add_argument("--preservation-receipt", type=Path, help="hash-bound copy/source receipt required when candidate checkout or executable path differs from the original build")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--recorder", type=Path, help="precompiled Swift recorder; otherwise swiftc compiles into --out")
    parser.add_argument("--ffmpeg", default=shutil.which("ffmpeg") or "ffmpeg")
    args = parser.parse_args()
    try:
        print(run(args))
        return 0
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired) as exc:
        print(f"native-motion: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
