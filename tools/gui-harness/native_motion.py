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
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import math
import os
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


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


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
    allowed_plan = {"schema", "name", "duration_ms", "max_frames", "window_id", "capture_fps",
                    "max_frame_gap_ms", "expected_reduce_motion", "capture_scope", "actions", "crops", "case"}
    if set(plan) - allowed_plan:
        raise ValueError(f"unknown plan fields: {sorted(set(plan) - allowed_plan)}")
    case = plan.get("case")
    if not isinstance(case, dict) or set(case) != {"id", "flow", "owner_phase", "motion", "transition", "viewport", "text_scale", "live_index"}:
        raise ValueError("case requires id, flow, owner_phase, motion, transition, viewport, text_scale, live_index")
    if case["flow"] not in {"add", "ask", "hand", "settings", "source", "failure_recovery"}:
        raise ValueError("unknown case flow")
    if case["owner_phase"] not in {"starting", "failed", "serving"} or case["motion"] not in {"full", "reduced"}:
        raise ValueError("invalid owner phase or motion")
    if case["transition"] not in {"first_open", "open_close", "resize_midflight", "text_scale_midflight", "failure_recovery"}:
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
    allowed = {"key", "move", "click", "resize", "click_ax", "probe"}
    previous = -1
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
        if kind in {"move", "click"} and not all(isinstance(action.get(k), (int, float)) and math.isfinite(action[k]) for k in ("x", "y")):
            raise ValueError(f"action {index}: finite global point required")
        if kind == "resize" and not all(isinstance(action.get(k), (int, float)) and 320 <= action[k] <= 8000 for k in ("width", "height")):
            raise ValueError(f"action {index}: valid width/height required")
        if kind == "click_ax" and (not isinstance(action.get("title"), str) or not action["title"]):
            raise ValueError(f"action {index}: exact AX title required")
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
    for frame in frames:
        with Image.open(out / frame["file"]) as opened:
            current = opened.convert("RGB")
        size = current.size
        if size != (frame["width_px"], frame["height_px"]):
            raise ValueError(f"PNG {frame['file']} differs from recorded native pixel dimensions")
        if previous_size != size:
            sizes.append({"at_ms": frame["time_ms"], "size_px": list(size),
                          "ax_window_bounds_pt": frame.get("ax", {}).get("window", {}).get("bounds_pt")})
            previous_size = size
        window_node = frame.get("ax", {}).get("window") or {}
        window_rect = window_node.get("bounds_pt")
        if window_rect != previous_window:
            window_geometry.append({"at_ms": frame["time_ms"], "window": window_node})
            previous_window = window_rect
        focused = frame.get("ax", {}).get("focused") or {}
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
            focus.append({"at_ms": frame["time_ms"], "focused": focused})
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
    failures = []
    for requested, actual in zip(plan["actions"], actual_actions):
        at = actual["actual_ms"]
        baseline = max((frame for frame in frames if frame["time_ms"] <= at),
                       key=lambda frame: frame["time_ms"], default=frames[0])
        min_fraction = requested.get("min_visual_fraction", 0.005)
        first_change = None
        first_fraction = None
        for frame in frames:
            if frame["time_ms"] <= at:
                continue
            fraction = changed_fraction(out / baseline["file"], out / frame["file"])
            if fraction >= min_fraction:
                first_change = frame["time_ms"]
                first_fraction = fraction
                break
        focus_after = [entry for entry in focus if entry["at_ms"] >= at]
        expected_title = requested.get("expect_focus_title")
        first_focus = next((entry["at_ms"] for entry in focus_after
                            if expected_title in {entry["focused"].get("title"), entry["focused"].get("description")}), None) if expected_title else None
        resize_bounds = actual.get("posted", {}).get("window_after_pt", {}) if requested["kind"] == "resize" else None
        if resize_bounds is not None:
            if abs(resize_bounds.get("width", -10000) - requested["width"]) > 3 or abs(resize_bounds.get("height", -10000) - requested["height"]) > 3:
                failures.append(f"{actual['label']}: AX window did not reach requested size")
        expected_ax = requested.get("expect_ax_title")
        ax_nodes = actual.get("posted", {}).get("ax", {}).get("tree", [])
        ax_found = any(expected_ax in {node.get("title"), node.get("description")} for node in ax_nodes) if expected_ax else None
        if expected_ax and not ax_found:
            failures.append(f"{actual['label']}: AX tree did not contain {expected_ax!r}")
        selected_title = requested.get("expect_ax_selected_title")
        selected_found = any(selected_title in {node.get("title"), node.get("description")} and node.get("selected") is True
                             for node in ax_nodes) if selected_title else None
        if selected_title and not selected_found:
            failures.append(f"{actual['label']}: AX tree did not select {selected_title!r}")
        visual_deadline = requested.get("expect_visual_ms")
        prior_trees = [(0, event.get("ax", {}).get("tree")) for event in actions if event.get("phase") == "initial"]
        prior_trees += [(event.get("actual_ms", -1), event.get("posted", {}).get("ax", {}).get("tree"))
                        for event in actions if event.get("phase") == "action" and event.get("kind") == "probe" and event.get("actual_ms", 1e12) < at]
        baseline_tree = max(prior_trees, default=(0, None), key=lambda pair: pair[0])[1]
        probes = [event for event in actions if event.get("phase") == "action" and event.get("kind") == "probe"
                  and event.get("actual_ms", -1) >= at and (visual_deadline is None or event.get("actual_ms", 1e12) <= at + visual_deadline)]
        ax_changed = any(event.get("posted", {}).get("ax", {}).get("tree") != baseline_tree for event in probes) if baseline_tree is not None else None
        ax_pixel_divergence = bool(ax_changed and visual_deadline is not None and (first_change is None or first_change > at + visual_deadline))
        if visual_deadline is not None and (first_change is None or first_change > at + visual_deadline):
            failures.append(f"{actual['label']}: no native pixel change within {visual_deadline} ms")
        if expected_title and first_focus is None:
            failures.append(f"{actual['label']}: AX focus never became {expected_title!r}")
        action_results.append({"label": actual["label"], "kind": actual["kind"], "posted_at_ms": at,
                               "first_visual_change_ms": first_change, "visual_latency_ms": None if first_change is None else first_change - at,
                               "first_visual_fraction": first_fraction, "min_visual_fraction": min_fraction,
                               "expected_focus_title": expected_title, "first_expected_focus_ms": first_focus,
                               "expected_ax_title": expected_ax, "ax_found": ax_found,
                               "expected_ax_selected_title": selected_title, "ax_selected_found": selected_found,
                               "resize_window_after_pt": resize_bounds, "ax_tree_changed": ax_changed,
                               "ax_pixel_divergence": ax_pixel_divergence})
    if len(actual_actions) != len(plan["actions"]):
        failures.append("not every timed native action has a successful posted event")
    gaps = [b["time_ms"] - a["time_ms"] for a, b in zip(frames, frames[1:])]
    max_gap = max(gaps, default=0)
    if max_gap > plan["max_frame_gap_ms"]:
        failures.append(f"native frame gap {max_gap:.2f} ms exceeds {plan['max_frame_gap_ms']} ms")
    if frames[-1]["time_ms"] < plan["duration_ms"] - plan["max_frame_gap_ms"]:
        failures.append("native frames stopped before the requested capture tail")
    jump_candidates = [change for change in changes if change["mean_rgb_delta"] is not None
                       and change["mean_rgb_delta"] > 30]
    return {"frame_count": len(frames), "last_frame_ms": frames[-1]["time_ms"],
            "max_frame_gap_ms": max_gap, "size_changes": sizes, "window_geometry_changes": window_geometry,
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


def compiler_admission(receipt_path: Path | None, binary: Path, source: dict[str, str]) -> dict[str, Any]:
    """Admit only the root build's completed, source-bound compiler receipt."""
    if receipt_path is None:
        return {"state": "UnprovenBinarySource", "reasons": ["no compiler receipt supplied"]}
    path = receipt_path.resolve(strict=True)
    receipt = json.loads(path.read_text())
    reasons = []
    if source["status_porcelain"]:
        reasons.append("source checkout is not frozen clean")
    if receipt.get("head") != source["head"] or receipt.get("tree") != source["tree"]:
        reasons.append("receipt HEAD/tree differs from capture checkout")
    if receipt.get("lock_sha256") != source["cargo_lock_sha256"]:
        reasons.append("receipt Cargo.lock digest differs")
    if receipt.get("exit_status") != 0 or receipt.get("frozen_preserved") is not True or receipt.get("capacity_abort") is not False:
        reasons.append("compiler run did not finish successfully on a frozen source")
    if receipt.get("census_valid") is not True or type(receipt.get("maximum_local_cargo")) is not int or receipt["maximum_local_cargo"] > receipt.get("local_cargo_cap", -1):
        reasons.append("compiler process census/cap is invalid")
    if not isinstance(receipt.get("rustc_version"), str) or "rustc " not in receipt["rustc_version"]:
        reasons.append("rustc toolchain identity missing")
    if receipt.get("build_cwd") != str(ROOT.resolve()):
        reasons.append("actual compiler working directory differs")
    command_path = path.with_name("command.sh")
    command = command_path.read_text() if command_path.is_file() else ""
    if not command or hashlib.sha256(command.encode()).hexdigest() != receipt.get("command_sha256"):
        reasons.append("compiler command.sh digest differs")
    argv = receipt.get("build_argv")
    if not isinstance(argv, list) or len(argv) < 3 or argv[-1] != command or not all(isinstance(part, str) for part in argv):
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
    matched = False
    if isinstance(binaries, dict):
        matched = any(Path(name).resolve() == binary and value == sha256(binary) for name, value in binaries.items())
    if not matched:
        reasons.append("binary SHA is not the one produced by this compiler run")
    return {"state": "VerifiedBuildReceipt" if not reasons else "UnprovenBinarySource",
            "reasons": reasons, "receipt_path": str(path), "receipt_sha256": sha256(path),
            "command_sha256": receipt.get("command_sha256"), "raw_log_sha256": receipt.get("raw_log_sha256")}


def run(args: argparse.Namespace) -> Path:
    if sys.platform != "darwin":
        raise ValueError("native capture requires macOS")
    out = args.out.resolve()
    if out == ROOT or ROOT in out.parents:
        raise ValueError("native artifact output must be outside the source checkout")
    if out.exists() and any(out.iterdir()):
        raise ValueError(f"output directory must be empty to prevent stale frame evidence: {out}")
    out.mkdir(parents=True, exist_ok=True)
    plan = require_plan(args.plan)
    expected = args.binary.resolve(strict=True)
    actual = process_executable(args.pid)
    if actual != expected:
        raise ValueError(f"PID {args.pid} runs {actual}, not {expected}")
    inputs = []
    for path in args.input:
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.stat().st_size > MAX_INPUT_BYTES:
            raise ValueError(f"input must be a regular file <= {MAX_INPUT_BYTES} bytes: {resolved}")
        inputs.append({"path": str(resolved), "bytes": resolved.stat().st_size, "sha256": sha256(resolved)})
    source = {"head": git("rev-parse", "HEAD"), "tree": git("rev-parse", "HEAD^{tree}"),
              "status_porcelain": git("status", "--porcelain"), "cargo_lock_sha256": sha256(ROOT / "Cargo.lock")}
    admission = compiler_admission(args.compiler_receipt, expected, source)
    manifest: dict[str, Any] = {"schema": 1, "class": "native_window_compositor_and_ax",
        "live_owner_index_admission": "unverified; pair with a production owner/read receipt",
        "name": plan["name"], "case": plan["case"], "pid": args.pid, "binary": {"path": str(expected), "sha256": sha256(expected)},
        "plan": {"path": str(args.plan.resolve()), "sha256": sha256(args.plan.resolve())},
        "tool": {"python_sha256": sha256(Path(__file__)), "swift_sha256": sha256(SWIFT)},
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
                             "focused": frame.get("ax", {}).get("focused")})
    result_row = read_jsonl(out / "result.jsonl")[0]
    if result_row["captured_frames"] != len(frames) or result_row.get("stream_failure"):
        raise ValueError("native stream failed or frame count mismatched")
    manifest["window"] = read_jsonl(out / "window.jsonl")[0]
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
    stable = stable and git("rev-parse", "HEAD") == source["head"] and git("rev-parse", "HEAD^{tree}") == source["tree"]
    stable = stable and git("status", "--porcelain") == source["status_porcelain"]
    stable = stable and sha256(ROOT / "Cargo.lock") == source["cargo_lock_sha256"]
    stable = stable and sha256(args.plan.resolve()) == manifest["plan"]["sha256"]
    stable = stable and sha256(Path(__file__)) == manifest["tool"]["python_sha256"] and sha256(SWIFT) == manifest["tool"]["swift_sha256"]
    if args.compiler_receipt:
        stable = stable and sha256(args.compiler_receipt.resolve()) == admission["receipt_sha256"]
        stable = stable and compiler_admission(args.compiler_receipt, expected, source)["state"] == admission["state"]
    stable = stable and all(sha256(Path(item["path"])) == item["sha256"] for item in inputs)
    if args.owner_receipt:
        stable = stable and sha256(args.owner_receipt.resolve()) == manifest["owner_receipt"]["sha256"]
    manifest["capture_inputs_stable"] = stable
    if not stable:
        manifest["analysis"]["failures"].append("binary/source/lock/selected input or owner receipt changed during capture")
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
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--input", type=Path, action="append", default=[], help="immutable live input file to hash; repeat")
    parser.add_argument("--owner-receipt", type=Path, help="independent production owner/read evidence to hash, not inferred from pixels")
    parser.add_argument("--compiler-receipt", type=Path, help="completed frozen-source build receipt; absent/mismatch is UnprovenBinarySource")
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
