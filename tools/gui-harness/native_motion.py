#!/usr/bin/env python3
"""Capture an existing *native* macOS desktop window with timed input and AX.

This complements `backend-desktop-gui-harness journey`, which runs the real
owner but draws into a headless GPUI window. A native capture proves compositor
pixels, window geometry and OS accessibility. A paired owner/read receipt is
still required before claiming that those pixels show a particular live index.

Example (after launching the exact built desktop binary and granting Screen
Recording and Accessibility to a dedicated team-signed or frozen ad-hoc recorder app):
  python3 tools/gui-harness/native_motion.py \
    --pid "$PID" --binary /path/to/backend-desktop \
    --source /path/to/frozen/candidate-checkout \
    --compiler-receipt /path/to/completed-build/receipt.json \
    --plan tools/gui-harness/plans/first-add.json \
    --input /path/to/project/Cargo.toml --input /path/to/project/Cargo.lock \
    --recorder /path/to/NudoxMotionRecorder.app/Contents/MacOS/NudoxMotionRecorder \
    --out /private/tmp/sol-native-first-add
  python3 tools/gui-harness/native_motion_matrix.py /private/tmp/sol-native-runs \
    --out /private/tmp/sol-native-runs/MATRIX.json

Build/sign the dedicated app before OS privacy setup; see RUN19-NATIVE-MOTION.md.
Its executable supports `list PID` when more than one window needs an exact ID.
An ad-hoc rebuild is a new permission identity and must use a new CDHash path.
Per-output unsigned compilation is not admitted.
The plan's action times are relative to the first captured native frame. Native
keycodes are macOS virtual keycodes; click coordinates are global screen points.
No route, index, owner gate, or component state is injected by this tool.

Active plans require --attest-plan-v1 with a newly signed recorder that hashes
the exact Data it decodes and writes a matching native receipt before capture.
The existing signed recorder remains usable for passive, explicitly unverified
transport captures. Active LaunchServices capture also requires --supervise-v1
with a recorder implementing the private control protocol; legacy mode stays
zero-input.

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
import re
import secrets
import signal
import shlex
import socket
import stat
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time
from typing import Any
import native_motion_supervision as supervision

ROOT = Path(__file__).resolve().parents[2]
SWIFT = Path(__file__).with_name("native_motion.swift")
MAX_DURATION_MS = 30_000
MAX_FRAMES = 1_800
MAX_INPUT_BYTES = 2_000_000_000
MAX_AX_SAMPLE_AGE_MS = 150
MAX_CLOCK_RESIDUAL_MS = 50
MAX_CALLBACK_OFFSET_MS = 100
MAX_PLAN_BYTES = 1_000_000
EXPECTED_PLAN_FLAG_V1 = "--expected-plan-sha256-v1"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def plan_file_observation(path: Path) -> tuple[dict[str, Any], bytes]:
    """Read one regular file and bind its raw bytes to its file identity."""
    path = path.resolve(strict=True)
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_PLAN_BYTES:
            raise ValueError(f"plan must be a regular file <= {MAX_PLAN_BYTES} bytes: {path}")
        with os.fdopen(fd, "rb", closefd=False) as stream:
            raw = stream.read(MAX_PLAN_BYTES + 1)
        after = os.fstat(fd)
    finally:
        os.close(fd)
    path_after = path.stat()
    def identity(row: os.stat_result) -> tuple[int, ...]:
        return (row.st_dev, row.st_ino, row.st_size, row.st_mtime_ns,
                row.st_ctime_ns, row.st_nlink)
    if len(raw) != before.st_size or identity(before) != identity(after) or identity(after) != identity(path_after):
        raise ValueError(f"plan changed during one file observation: {path}")
    return ({"path": str(path), "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest(),
             "device": after.st_dev, "inode": after.st_ino, "mtime_ns": after.st_mtime_ns,
             "ctime_ns": after.st_ctime_ns, "nlink": after.st_nlink}, raw)


def write_once_readonly(path: Path, raw: bytes) -> dict[str, Any]:
    """Create a fresh evidence file; later checks still detect possible edits."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(fd, "wb", closefd=False) as stream:
            stream.write(raw)
            stream.flush()
        os.fsync(fd)
        os.fchmod(fd, 0o444)
    finally:
        os.close(fd)
    observed, written = plan_file_observation(path)
    if written != raw or observed["nlink"] != 1:
        raise RuntimeError(f"fresh plan evidence changed after write: {path}")
    return observed


def plan_snapshot_check(expected: dict[str, Any]) -> dict[str, Any]:
    """A point-in-time check, not proof of which bytes another process read."""
    try:
        observed, _ = plan_file_observation(Path(expected["path"]))
        return {"state": "Stable" if observed == expected else "Changed", "observed": observed}
    except (OSError, ValueError) as error:
        return {"state": "Unavailable", "error": str(error)}


def plan_phase_receipt(out: Path, phase: str, source: dict[str, Any],
                       source_copy: dict[str, Any], resolved: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    checks = {"source": plan_snapshot_check(source), "source_copy": plan_snapshot_check(source_copy),
              "resolved": plan_snapshot_check(resolved)}
    row = {"schema": 1, "phase": phase,
           "state": "Stable" if all(check["state"] == "Stable" for check in checks.values()) else "Changed",
           "expected": {"source": source, "source_copy": source_copy, "resolved": resolved}, "checks": checks,
           "observed_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    receipt = write_once_readonly(out / f"PLAN-{phase.upper()}.json",
                                  (json.dumps(row, indent=2, sort_keys=True) + "\n").encode())
    return row, receipt


def plan_consumption_qualification(plan: dict[str, Any], attest_v1: bool = False) -> dict[str, Any]:
    # The already-signed legacy recorder emits no digest of the Data it
    # decodes. Opt-in v1 stays pending until its native receipt is checked.
    return {"state": "PendingV1" if attest_v1 else "UnverifiedByCurrentRecorder",
            "exact_bytes_attested": False,
            "launch_admissible": attest_v1 or not bool(plan["actions"])}


def recorder_arguments(pid: int, plan_path: Path, out: Path,
                       expected_sha256: str | None = None) -> list[str]:
    args = [str(pid), str(plan_path), str(out)]
    if expected_sha256 is not None:
        if not re.fullmatch(r"[0-9a-f]{64}", expected_sha256):
            raise ValueError("versioned expected plan digest must be lowercase SHA-256")
        args.extend([EXPECTED_PLAN_FLAG_V1, expected_sha256])
    return args


def consumed_plan_receipt(out: Path, expected: dict[str, Any], recorder: Path,
                          identity: dict[str, str], pid: int) -> dict[str, Any]:
    """Admit only the signed recorder's v1 digest of the Data it decoded."""
    path = out / "plan-consumption.jsonl"
    if not path.exists():
        return {"state": "Missing", "verified": False, "exact_bytes_attested": False}
    try:
        if path.is_symlink() or path.stat().st_nlink != 1 or path.stat().st_mode & 0o222:
            raise ValueError("receipt is linked or writable")
        observation, raw = plan_file_observation(path)
        rows = [json.loads(line) for line in raw.splitlines() if line.strip()]
        if len(rows) != 1 or not isinstance(rows[0], dict):
            raise ValueError("expected exactly one typed receipt row")
        row = rows[0]
        required = {"schema": 1, "kind": "native-motion-plan-consumption-v1", "state": "MatchedV1",
                    "pid": pid, "plan_path": expected["path"], "consumed_bytes": expected["bytes"],
                    "consumed_sha256": expected["sha256"], "expected_sha256": expected["sha256"],
                    "recorder_bundle_identifier": identity["identifier"],
                    "recorder_executable": str(recorder),
                    "recorder_executable_sha256": identity["executable_sha256"],
                    "stream_output_callback_ready": True}
        mismatches = [key for key, value in required.items()
                      if type(row.get(key)) is not type(value) or row.get(key) != value]
        return {"state": "VerifiedV1" if not mismatches else "RejectedV1",
                "verified": not mismatches, "exact_bytes_attested": not mismatches,
                "mismatched_fields": mismatches,
                "file": observation, "reported": row}
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return {"state": "InvalidV1", "verified": False, "exact_bytes_attested": False,
                "error": str(error)}


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
            "python_sha256": sha256(Path(__file__)), "swift_sha256": sha256(SWIFT),
            "supervision_sha256": sha256(Path(supervision.__file__))}


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


def recorder_identity(recorder: Path) -> dict[str, str]:
    """Admit a team-stable app or one frozen, content-addressed ad-hoc app."""
    if symlink_in_artifact_path(recorder):
        raise ValueError("recorder app executable must not be a symlink")
    bundle = bundle_identity(recorder)
    if bundle is None or not (bundle["identifier"] == "dev.nudox.audit.motion-recorder" or
                              bundle["identifier"].startswith("dev.nudox.audit.motion-recorder.")):
        raise ValueError("recorder must be a dedicated dev.nudox.audit.motion-recorder .app")
    info = plistlib.loads(Path(bundle["info_path"]).read_bytes())
    if not isinstance(info.get("NSScreenCaptureUsageDescription"), str) or not info["NSScreenCaptureUsageDescription"].strip():
        raise ValueError("recorder app needs NSScreenCaptureUsageDescription")
    verified = subprocess.run(["codesign", "--verify", "--strict", "--verbose=2", bundle["path"]],
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if verified.returncode:
        raise ValueError("recorder app code signature verification failed: " + verified.stdout.strip())
    signing = subprocess.run(["codesign", "--display", "--verbose=4", bundle["path"]],
                             text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    fields = dict(line.split("=", 1) for line in signing.stdout.splitlines() if "=" in line)
    if signing.returncode:
        raise ValueError("recorder code-signing identity could not be read")
    cdhash = fields.get("CDHash", "").lower()
    if not re.fullmatch(r"[0-9a-f]{40,64}", cdhash):
        raise ValueError("recorder has no valid CodeDirectory hash")
    team = fields.get("TeamIdentifier", "")
    authority = [line for line in signing.stdout.splitlines()
                 if line.startswith(("Authority=Apple Development:", "Authority=Developer ID Application:"))]
    requirement = subprocess.run(["codesign", "--display", "--requirements", "-", bundle["path"]],
                                 text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if requirement.returncode or "designated =>" not in requirement.stdout:
        raise ValueError("recorder has no verifiable designated requirement")
    if fields.get("Signature") == "adhoc":
        # Apple DTS confirms ad-hoc permission is per build. The app path and
        # code directory therefore name one immutable version; a new build
        # needs its own path and a fresh grant through the OS UI.
        if team not in {"", "not set"} or bundle["path"] != str(
                Path(bundle["path"]).with_name(f"NudoxMotionRecorder-{cdhash}.app")):
            raise ValueError("ad-hoc recorder needs its own CDHash-versioned app path")
        if f'cdhash h"{cdhash}"' not in requirement.stdout.lower():
            raise ValueError("ad-hoc designated requirement does not bind the recorded CDHash")
        kind = "AdHocFrozenContentAddressed"
    else:
        if not team or team == "not set" or not authority or "anchor apple" not in requirement.stdout:
            raise ValueError("recorder needs an Apple team signature or frozen ad-hoc content address")
        kind = "TeamSignedStableAcrossBuilds"
    return {**bundle, "kind": kind, "team_identifier": team, "cdhash": cdhash,
            "designated_requirement": requirement.stdout.strip(), "executable_sha256": sha256(recorder)}


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


def require_plan(path: Path, raw_bytes: bytes | None = None) -> dict[str, Any]:
    # A runner passes its one observed byte snapshot here. Standalone callers
    # can still validate a plan path without preparing a capture.
    plan = json.loads(raw_bytes if raw_bytes is not None else path.read_bytes())
    if not isinstance(plan, dict) or plan.get("schema") != 1:
        raise ValueError("plan must have schema 1")
    allowed_plan = {"schema", "name", "duration_ms", "max_frames", "window_id", "expected_window_frame_pt", "capture_fps",
                    "max_frame_gap_ms", "expected_reduce_motion", "capture_scope", "require_frontmost", "foreground_required",
                    "actions", "crops", "case"}
    if set(plan) - allowed_plan:
        raise ValueError(f"unknown plan fields: {sorted(set(plan) - allowed_plan)}")
    case = plan.get("case")
    if not isinstance(case, dict) or set(case) != {"id", "flow", "owner_phase", "motion", "transition", "viewport", "text_scale", "live_index"}:
        raise ValueError("case requires id, flow, owner_phase, motion, transition, viewport, text_scale, live_index")
    if case["flow"] not in {"add", "ask", "find", "drawer", "shelf", "hand", "settings", "source", "failure_recovery"}:
        raise ValueError("unknown case flow")
    if case["owner_phase"] not in {"starting", "failed", "serving"} or case["motion"] not in {"full", "reduced"}:
        raise ValueError("invalid owner phase or motion")
    if case["transition"] not in {"first_open", "open_close", "resize_midflight", "text_scale_midflight", "failure_recovery", "settle", "retarget", "hide_reveal", "underlay_retirement"}:
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
    if "require_frontmost" in plan and type(plan["require_frontmost"]) is not bool:
        raise ValueError("require_frontmost must be boolean")
    allowed = {"key", "move", "click", "mouse_down", "mouse_up", "resize", "click_ax", "probe"}
    previous = -1
    button_down = False
    retirement = case["flow"] == "drawer" and case["transition"] == "underlay_retirement"
    held_keys: list[tuple[int, tuple[str, ...]]] = []
    retirement_pairs = 0
    for index, action in enumerate(actions):
        if not isinstance(action, dict) or action.get("kind") not in allowed:
            raise ValueError(f"action {index}: unknown kind")
        allowed_action = {"at_ms", "kind", "label", "keycode", "modifiers", "x", "y",
                          "width", "height", "title", "role", "expect_visual_ms", "min_visual_fraction", "expect_focus_title", "expect_ax_title", "expect_ax_selected_title", "expect_ax_unselected_title"}
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
            if retirement and retirement_pairs:
                raise ValueError(f"action {index}: underlay retirement permits one held gesture")
            button_down = True
        elif kind == "mouse_up":
            if not button_down:
                raise ValueError(f"action {index}: mouse_up without mouse_down")
            if retirement and held_keys != [(42, ("command",)), (53, ())]:
                raise ValueError(f"action {index}: underlay retirement requires held ⌘\\ then Escape before Up")
            button_down = False
            retirement_pairs += 1
        elif button_down and kind == "key" and retirement:
            chord = (action["keycode"], tuple(action.get("modifiers", [])))
            allowed_held = [(42, ("command",)), (53, ())]
            if len(held_keys) >= len(allowed_held) or chord != allowed_held[len(held_keys)]:
                raise ValueError(f"action {index}: held keyboard cover only admits ⌘\\ then Escape")
            held_keys.append(chord)
        elif button_down and kind not in ({"probe"} if retirement else {"move", "probe"}):
            raise ValueError(f"action {index}: only scoped held-input actions may follow mouse_down")
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
        if "expect_ax_unselected_title" in action and (kind != "probe" or not isinstance(action["expect_ax_unselected_title"], str) or not action["expect_ax_unselected_title"]):
            raise ValueError(f"action {index}: expect_ax_unselected_title requires a named probe")
    if button_down:
        raise ValueError("plan leaves mouse button down")
    if case["transition"] == "underlay_retirement" and (not retirement or retirement_pairs != 1):
        raise ValueError("underlay retirement requires one drawer held keyboard cover gesture")
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
    # Probes are read-only. The Swift recorder decodes this typed admission
    # and independently checks it against the action list before any input.
    foreground_required = plan.get("require_frontmost", False) or any(action["kind"] != "probe" for action in actions)
    if "foreground_required" in plan and plan["foreground_required"] is not foreground_required:
        raise ValueError("foreground_required must match posting actions")
    plan["foreground_required"] = foreground_required
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


def ax_uniquely_unselected(nodes: list[dict[str, Any]], title: str) -> bool:
    matches = [node for node in nodes if title in {node.get("title"), node.get("description")}]
    return len(matches) == 1 and matches[0].get("selected") is False


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
        unselected_title = requested.get("expect_ax_unselected_title")
        unselected_found = ax_uniquely_unselected(ax_nodes, unselected_title) if unselected_title else None
        if unselected_title and not unselected_found:
            failures.append(f"{actual['label']}: fresh AX probe did not uniquely leave {unselected_title!r} unselected")
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
                               "expected_ax_unselected_title": unselected_title, "ax_unselected_found": unselected_found,
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


def failed_recorder_evidence(out: Path) -> dict[str, Any]:
    """Preserve typed pre-input evidence without promoting it to a capture."""
    evidence: dict[str, Any] = {"state": "RecorderFailedBeforeAnalysis"}
    for name, key in [("preflight.jsonl", "preflight"), ("window.jsonl", "window"),
                      ("stream-state.jsonl", "stream_state"),
                      ("plan-consumption.jsonl", "plan_consumption")]:
        path = out / name
        if path.is_file():
            try:
                rows = read_jsonl(path)
                if rows:
                    evidence[key] = rows[0]
            except (OSError, ValueError) as error:
                evidence[key + "_parse_error"] = str(error)
    for name, key in [("frames.jsonl", "frame_rows"), ("actions.jsonl", "action_rows")]:
        path = out / name
        evidence[key] = len(read_jsonl(path)) if path.is_file() else 0
    return evidence


def launch_services_command(bundle: Path, pid: int, plan_path: Path, out: Path,
                            expected_sha256: str | None = None,
                            control: tuple[Path, Path] | None = None) -> list[str]:
    """Launch the recorder as its own app, with separate native output files."""
    recorder_args = recorder_arguments(pid, plan_path, out, expected_sha256)
    if control is not None:
        if expected_sha256 is None:
            raise ValueError("supervised launch requires an expected digest")
        recorder_args.extend(["--control-v1", str(control[0]), str(control[1])])
    return ["/usr/bin/open", "-n", "-g", "-W", "-a", str(bundle),
            "--stdout", str(out / "recorder.stdout"),
            "--stderr", str(out / "recorder.stderr"),
            "--args", *recorder_args]


def require_launch_bundle(bundle_arg: Path, recorder: Path, identity: dict[str, str],
                          plan: dict[str, Any], supervised: bool = False) -> Path:
    # A timed-out `open -W` can leave its LaunchServices app alive. Admit only
    # zero-input capture until the launcher has an exact process-stop protocol.
    if plan["actions"] and not supervised:
        raise ValueError("LaunchServices mode currently admits zero-input captures only")
    if symlink_in_artifact_path(bundle_arg):
        raise ValueError("LaunchServices recorder bundle must not be a symlink")
    bundle = bundle_arg.resolve(strict=True)
    if bundle != Path(identity["path"]) or recorder != bundle / "Contents/MacOS" / recorder.name:
        raise ValueError("LaunchServices bundle does not match the verified recorder executable")
    return bundle


def launch_services_supervised(bundle: Path, pid: int, plan_path: Path, out: Path,
                               plan: dict[str, Any], recorder: Path,
                               identity: dict[str, str], expected: dict[str, Any]) -> dict[str, Any]:
    """An exact native peer must stop before input ownership can be released."""
    nonce = secrets.token_hex(32)
    socket_path = None
    nonce_path = None
    listener = None
    argv: list[str] = []
    deadline = time.monotonic() + plan["duration_ms"] / 1000 + 15
    process = None
    connection = None
    watcher = None
    recorder_pid = None
    native = None
    failures: list[str] = []
    exact_exit = False
    socket_eof = False
    launcher_output = ""
    launcher_exit = None
    try:
        socket_path, listener = supervision.private_socket()
        nonce_path = socket_path.parent / "nonce"
        nonce_fd = os.open(nonce_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            raw_nonce = nonce.encode()
            offset = 0
            while offset < len(raw_nonce):
                written = os.write(nonce_fd, raw_nonce[offset:])
                if written <= 0:
                    raise OSError("private control nonce write failed")
                offset += written
            os.fsync(nonce_fd)
        finally:
            os.close(nonce_fd)
        argv = launch_services_command(bundle, pid, plan_path, out, expected["sha256"],
                                       (socket_path, nonce_path))
        for name in ("recorder.stdout", "recorder.stderr"):
            fd = os.open(out / name, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            os.close(fd)
        process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   text=True, start_new_session=True)
        listener.settimeout(min(10.0, max(0.1, deadline - time.monotonic())))
        connection, _ = listener.accept()
        recorder_pid = supervision.peer_pid(connection)
        if recorder_pid <= 0 or process_executable(recorder_pid) != recorder:
            raise supervision.ProtocolError("control peer is not the exact running recorder")
        watcher = supervision.ExactProcessExit(recorder_pid)
        if process_executable(recorder_pid) != recorder:
            raise supervision.ProtocolError("control peer changed before arm")
        policy = supervision.ControlPolicy(plan["actions"])

        def accept_hello(row: dict[str, Any]) -> None:
            required = {"kind": "Hello", "nonce": nonce, "recorder_pid": recorder_pid,
                        "target_pid": pid, "recorder_executable": str(recorder),
                        "recorder_executable_sha256": identity["executable_sha256"],
                        "recorder_bundle_identifier": identity["identifier"],
                        "consumed_sha256": expected["sha256"]}
            if any(type(row.get(key)) is not type(value) or row.get(key) != value
                   for key, value in required.items()):
                raise supervision.ProtocolError("native Hello identity, target or consumed digest mismatch")
            consumed = consumed_plan_receipt(out, expected, recorder, identity, pid)
            if not consumed["verified"] or recorder_identity(recorder) != identity:
                raise supervision.ProtocolError("native plan receipt or recorder signature changed before arm")

        native = supervision.serve(connection, policy, deadline, accept_hello)
        if native["held_unreleased"]:
            failures.append("native held input was not released")
        exact_exit = watcher.wait(3.0)
        if not exact_exit:
            failures.append("kernel NOTE_EXIT absent for exact armed recorder")
        elif connection is not None:
            socket_eof = supervision.peer_eof_after_exit(connection)
            if not socket_eof:
                failures.append("control socket EOF absent after exact recorder exit")
        if process is not None:
            try:
                launcher_output, _ = process.communicate(timeout=2.0)
                launcher_exit = process.returncode
            except subprocess.TimeoutExpired:
                failures.append("open -W did not return after native stop")
    except (OSError, ValueError, supervision.ProtocolError, TimeoutError, EOFError,
            socket.timeout) as error:
        failures.append(str(error))
        if watcher is not None:
            try:
                exact_exit = watcher.wait(2.0)
                if exact_exit and connection is not None:
                    socket_eof = supervision.peer_eof_after_exit(connection)
            except OSError as watch_error:
                failures.append(f"kernel exit watch failed: {watch_error}")
        if process is not None and process.poll() is not None:
            launcher_output, _ = process.communicate(timeout=1.0)
            launcher_exit = process.returncode
    finally:
        if connection is not None:
            connection.close()  # EOF revokes any future native permit.
        if listener is not None:
            listener.close()
        if watcher is not None:
            try:
                watcher.close()
            except OSError as watch_error:
                failures.append(f"kernel exit watch close failed: {watch_error}")
        if socket_path is not None:
            try:
                socket_path.unlink(missing_ok=True)
                if nonce_path is not None:
                    nonce_path.unlink(missing_ok=True)
                socket_path.parent.rmdir()
            except OSError:
                pass
        if process is not None and process.poll() is None:
            # Only the launcher is our child. Do not signal the target GUI or
            # an unverified recorder PID; this does not prove recorder exit.
            try:
                process.terminate()
            except OSError as error:
                failures.append(f"launcher termination failed: {error}")
            try:
                launcher_output, _ = process.communicate(timeout=2.0)
                launcher_exit = process.returncode
            except subprocess.TimeoutExpired:
                failures.append("launcher did not terminate; recorder stop remains unproven")
        (out / "launcher.log").write_text(launcher_output or "")
    native_stopped = native is not None and native["state"] == "Stopped"
    stop_path = out / "native-control-stop.jsonl"
    stop_receipt = None
    if stop_path.is_file():
        try:
            if stop_path.is_symlink() or stop_path.stat().st_nlink != 1 or stop_path.stat().st_mode & 0o222:
                raise ValueError("native stop receipt is linked or writable")
            stop_file, raw = plan_file_observation(stop_path)
            rows = [json.loads(line) for line in raw.splitlines() if line.strip()]
            if len(rows) != 1 or not isinstance(rows[0], dict):
                raise ValueError("native stop receipt requires one typed row")
            stop_receipt = {"file": stop_file, "row": rows[0]}
        except (OSError, ValueError, json.JSONDecodeError) as error:
            failures.append(f"native stop receipt invalid: {error}")
    else:
        failures.append("native stop receipt absent")
    stop_row = stop_receipt["row"] if stop_receipt is not None else {}
    if (stop_row.get("schema") != 1 or stop_row.get("kind") != "native-motion-control-stop-v1"
            or stop_row.get("recorder_pid") != recorder_pid or stop_row.get("held_unreleased") is not False
            or stop_row.get("controller_reply") != "StopAck"):
        failures.append("native stop acknowledgment does not bind released exact recorder")
    if native is not None and native["cancel_requested"] and not native["cancel_ack"]:
        failures.append("native cancellation was not acknowledged")
    ownership_release = native_stopped and exact_exit and socket_eof and not failures
    if not ownership_release:
        failures.append("exclusive input ownership unresolved until native stop and exact kernel exit")
    logs = {name: ({"path": str(out / name), "size": (out / name).stat().st_size,
                    "sha256": sha256(out / name)} if (out / name).is_file() else {"state": "Missing"})
            for name in ("recorder.stdout", "recorder.stderr")}
    receipt = {"schema": 1, "kind": "native-motion-supervision-v1",
               "state": "StoppedVerified" if ownership_release else "Unresolved",
               "recorder_pid": recorder_pid, "peer_pid_verified": recorder_pid is not None,
               "exact_process_exit": exact_exit, "control_socket_eof_observed": socket_eof,
               "native_stop_ack": native_stopped,
               "held_unreleased": bool(native and native["held_unreleased"]),
               "input_ownership_release_admitted": ownership_release,
               "native": native, "native_stop_receipt": stop_receipt, "failures": failures,
               "nonce_sha256": hashlib.sha256(nonce.encode()).hexdigest(),
               "socket_path": str(socket_path) if socket_path is not None else None,
               "launcher_exit_code": launcher_exit, "launcher_log_sha256": sha256(out / "launcher.log"),
               "recorder_exit": "KernelNOTE_EXIT" if exact_exit else None}
    receipt_file = write_once_readonly(out / "SUPERVISION.json",
                                       (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode())
    if not ownership_release:
        write_once_readonly(out / "INPUT-OWNERSHIP-UNRESOLVED.json",
            (json.dumps({"schema": 1, "state": "Unresolved",
                "supervision_receipt_sha256": receipt_file["sha256"],
                "exact_recorder_pid": recorder_pid,
                "reason": "native stop/ack or kernel NOTE_EXIT was not proven; retain exclusive input handoff"},
                sort_keys=True) + "\n").encode())
    return {"mode": "SupervisedLaunchServicesApp", "argv": argv,
            "timed_out": not ownership_release, "recorder_exit": receipt["recorder_exit"],
            "recorder_may_continue": not exact_exit, "launcher_exit_code": launcher_exit,
            "logs": logs, "launcher_log_sha256": receipt["launcher_log_sha256"],
            "supervision": receipt, "supervision_file": receipt_file}


def launch_services_wait(bundle: Path, pid: int, plan_path: Path, out: Path,
                         timeout_seconds: float, expected_sha256: str | None = None) -> dict[str, Any]:
    """Wait on the launcher only; app completion requires native sidecars."""
    for name in ["recorder.stdout", "recorder.stderr"]:
        path = out / name
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(fd)
    argv = launch_services_command(bundle, pid, plan_path, out, expected_sha256)
    then = time.monotonic()
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, start_new_session=True)
    timed_out = False
    try:
        launcher_output, _ = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            launcher_output, _ = process.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            launcher_output, _ = process.communicate(timeout=2)
    logs = {}
    for name in ["recorder.stdout", "recorder.stderr"]:
        path = out / name
        if path.is_symlink() or not path.is_file() or path.stat().st_nlink != 1:
            raise RuntimeError(f"LaunchServices output is not a fresh regular file: {path}")
        logs[name] = {"path": str(path), "size": path.stat().st_size, "sha256": sha256(path)}
    (out / "launcher.log").write_text(launcher_output or "")
    return {"mode": "LaunchServicesApp", "argv": argv, "launcher_exit_code": process.returncode,
            "timed_out": timed_out, "elapsed_seconds": round(time.monotonic() - then, 3),
            "logs": logs, "launcher_log_sha256": sha256(out / "launcher.log"),
            # open -W is our child, not the recorder. Its exit cannot rule
            # out a still-live app, even when the launcher returned zero.
            "recorder_exit": None, "recorder_may_continue": True,
            "tcc_responsibility": "Unverified; inspect OS TCC attribution for launched PID"}


def native_sidecar_bytes(path: Path) -> tuple[bytes | None, str | None]:
    """Read one small native sidecar without following links or trusting EOF."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None, None
    except OSError as error:
        return None, str(error)
    try:
        details = os.fstat(fd)
        if not stat.S_ISREG(details.st_mode) or details.st_nlink != 1:
            return None, "sidecar must be a regular singly linked file"
        return os.read(fd, supervision.MAX_MESSAGE + 1), None
    except OSError as error:
        return None, str(error)
    finally:
        os.close(fd)


def native_preflight_classification(out: Path, recorder: Path, identity: dict[str, str],
                                    pid: int, plan: dict[str, Any]) -> dict[str, Any]:
    raw, error = native_sidecar_bytes(out / "preflight.jsonl")
    classification = supervision.classify_preflight(raw, recorder=str(recorder),
        identifier=identity["identifier"], pid=pid, foreground_required=plan["foreground_required"])
    if error is not None:
        classification.update(state="Invalid", diagnostic=error)
    return classification


def launch_services_completion(out: Path, launch: dict[str, Any], recorder: Path,
                               identity: dict[str, str], pid: int,
                               plan: dict[str, Any]) -> tuple[bool, dict[str, Any] | None, list[str], dict[str, Any]]:
    """Native completion evidence and reported preflight admission are separate.

    This checks sidecars, not an exact kernel exit or TCC process attribution.
    Supervised input still needs its independent StopAck/NOTE_EXIT/EOF gate.
    """
    failures = []
    if launch["timed_out"]:
        failures.append("LaunchServices wait timed out; recorder may still be running")
    if launch["launcher_exit_code"] != 0:
        failures.append("LaunchServices launcher exited nonzero; recorder exit is unknown")
    raw_result, result_error = native_sidecar_bytes(out / "result.jsonl")
    result, parse_error = supervision.single_sidecar_record(raw_result)
    if raw_result is None and result_error is None:
        failures.append("native result sidecar absent; recorder completion unproven")
    elif result_error or parse_error:
        failures.append(f"native result sidecar invalid; completion unproven: {result_error or parse_error}")
    elif result is not None and (any(type(result.get(key)) is not int or result[key] < 0
                                   for key in ("captured_frames", "dropped_frames"))
                                or result.get("stream_failure") is not None):
        failures.append("native result counters or stream failure prevent completion admission")
    classification = native_preflight_classification(out, recorder, identity, pid, plan)
    if classification["state"] == "Rejected":
        failures.append("native preflight rejected: " + ", ".join(classification["native_failure_codes"]))
    elif classification["state"] != "Admitted":
        failures.append(f"native preflight {classification['state']}: {classification['diagnostic']}")
    return not failures, classification["row"], failures, classification


def recorder_failure_message(manifest: dict[str, Any], out: Path) -> str:
    details = manifest.get("launcher", {}).get("completion_failures", [])
    classification = manifest.get("preflight_classification", {})
    if not details and classification.get("state") == "Rejected":
        details = ["native preflight rejected: " + ", ".join(classification["native_failure_codes"])]
    prefix = "; ".join(details) + "; " if details else ""
    return prefix + f"native recorder or plan snapshots did not prove completion; see {out / 'CAPTURE.json'}"


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
    source_plan, source_plan_bytes = plan_file_observation(args.plan)
    plan = require_plan(args.plan, source_plan_bytes)
    attest_plan_v1 = getattr(args, "attest_plan_v1", False)
    supervise_v1 = getattr(args, "supervise_v1", False)
    if supervise_v1 and (not args.launch_bundle or not attest_plan_v1):
        raise ValueError("--supervise-v1 requires --launch-bundle and --attest-plan-v1")
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
        "plan": {"path": source_plan["path"], "sha256": source_plan["sha256"],
                 "source_snapshot": source_plan, "attestation_requested": "v1" if attest_plan_v1 else None},
        "tool": tool,
        "source": source, "inputs": inputs, "binary_source_admission": admission,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "assertions": ["native SCWindow PID/window identity", "native PTS strictly increasing", "AX and action sidecars", "full PNG dimensions and crop bounds"]}
    if args.owner_receipt:
        receipt = args.owner_receipt.resolve(strict=True)
        manifest["owner_receipt"] = {"path": str(receipt), "sha256": sha256(receipt)}
    (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if not args.recorder:
        raise ValueError("pass --recorder with a dedicated signed native QA recorder .app executable")
    if symlink_in_artifact_path(args.recorder):
        raise ValueError("recorder app executable must not be a symlink")
    recorder = args.recorder.resolve(strict=True)
    manifest["recorder_identity"] = recorder_identity(recorder)
    manifest["recorder_sha256"] = sha256(recorder)
    launch_bundle = (require_launch_bundle(args.launch_bundle, recorder, manifest["recorder_identity"], plan,
                                           supervised=supervise_v1)
                     if args.launch_bundle else None)
    source_plan_copy = write_once_readonly(out / "source-plan.snapshot.json", source_plan_bytes)
    if source_plan_copy["sha256"] != source_plan["sha256"]:
        raise RuntimeError("original plan snapshot differs from parsed plan bytes")
    manifest["plan"]["source_copy_snapshot"] = source_plan_copy
    plan_path = out / "resolved-plan.json"
    resolved_plan = write_once_readonly(plan_path, (json.dumps(plan, indent=2) + "\n").encode())
    manifest["plan"]["resolved_snapshot"] = resolved_plan
    snapshot = {"schema": 1, "state": "PreparedNotProvenConsumed", "source": source_plan,
                "source_copy": source_plan_copy,
                "resolved": resolved_plan, "recorder_read_sha256": None}
    manifest["plan"]["snapshot_receipt"] = write_once_readonly(
        out / "PLAN-SNAPSHOT.json", (json.dumps(snapshot, indent=2, sort_keys=True) + "\n").encode())
    before, before_receipt = plan_phase_receipt(out, "prelaunch", source_plan,
                                                source_plan_copy, resolved_plan)
    manifest["plan"]["prelaunch_receipt"] = before_receipt
    manifest["plan"]["prelaunch_receipt_checks"] = {
        "snapshot": plan_snapshot_check(manifest["plan"]["snapshot_receipt"]),
        "prelaunch": plan_snapshot_check(before_receipt)}
    if before["state"] != "Stable" or any(
            check["state"] != "Stable" for check in manifest["plan"]["prelaunch_receipt_checks"].values()):
        manifest["plan"]["state"] = "ChangedBeforeLaunch"
        (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise RuntimeError("plan source or resolved bytes changed before recorder launch; see CAPTURE.json")
    manifest["plan"]["recorder_consumption"] = plan_consumption_qualification(plan, attest_plan_v1)
    if not manifest["plan"]["recorder_consumption"]["launch_admissible"]:
        manifest["plan"]["state"] = "ActiveLaunchBlockedWithoutConsumedBytesAttestation"
        (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise RuntimeError("active plan needs exact Swift-consumed bytes attestation before native input; see CAPTURE.json")
    (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    recorder_finished = False
    expected_consumed_sha256 = resolved_plan["sha256"] if attest_plan_v1 else None
    try:
        if launch_bundle:
            if supervise_v1:
                launch = launch_services_supervised(launch_bundle, args.pid, plan_path, out,
                    plan, recorder, manifest["recorder_identity"], resolved_plan)
                manifest["supervision"] = launch["supervision"]
                manifest["supervision_file"] = launch["supervision_file"]
            else:
                launch = launch_services_wait(launch_bundle, args.pid, plan_path, out,
                    timeout_seconds=plan["duration_ms"] / 1000 + 15,
                    expected_sha256=expected_consumed_sha256)
            manifest["launcher"] = launch
            manifest["recorder_exit"] = launch["recorder_exit"]
            (out / "recorder.log").write_bytes((out / "recorder.stderr").read_bytes())
            recorder_finished, preflight, launch_failures, classification = launch_services_completion(
                out, launch, recorder, manifest["recorder_identity"], args.pid, plan)
            manifest["preflight_classification"] = classification
            if supervise_v1 and not launch["supervision"]["input_ownership_release_admitted"]:
                launch_failures.append("supervised recorder input ownership remains unresolved")
                recorder_finished = False
            manifest["launcher"]["completion_failures"] = launch_failures
            if preflight is not None:
                manifest["preflight"] = preflight
        else:
            result = subprocess.run([str(recorder), *recorder_arguments(
                                    args.pid, plan_path, out, expected_consumed_sha256)],
                                    text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    timeout=plan["duration_ms"] / 1000 + 15)
            (out / "recorder.log").write_text(result.stdout)
            manifest["recorder_exit"] = result.returncode
            recorder_finished = result.returncode == 0
    finally:
        after, after_receipt = plan_phase_receipt(out, "postlaunch", source_plan,
                                                  source_plan_copy, resolved_plan)
        manifest["plan"]["postlaunch_receipt"] = after_receipt
        manifest["plan"]["postlaunch_receipt_checks"] = {
            "snapshot": plan_snapshot_check(manifest["plan"]["snapshot_receipt"]),
            "prelaunch": plan_snapshot_check(before_receipt),
            "postlaunch": plan_snapshot_check(after_receipt)}
        postlaunch_stable = after["state"] == "Stable" and all(
            check["state"] == "Stable" for check in manifest["plan"]["postlaunch_receipt_checks"].values())
        manifest["plan"]["state"] = "StableAtChecks" if postlaunch_stable else "ChangedDuringLaunch"
        (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    recorder_finished = recorder_finished and postlaunch_stable
    if attest_plan_v1:
        consumption = consumed_plan_receipt(out, resolved_plan, recorder,
                                            manifest["recorder_identity"], args.pid)
        manifest["plan"]["recorder_consumption"] = consumption
        recorder_finished = recorder_finished and consumption["verified"]
        (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    try:
        manifest["recorder_identity_stable"] = recorder_identity(recorder) == manifest["recorder_identity"]
    except (ValueError, OSError) as exc:
        manifest["recorder_identity_stable"] = False
        manifest["recorder_identity_change"] = str(exc)
    if not recorder_finished or not manifest["recorder_identity_stable"]:
        if "preflight_classification" not in manifest:
            manifest["preflight_classification"] = native_preflight_classification(
                out, recorder, manifest["recorder_identity"], args.pid, plan)
        manifest["failed_recorder_evidence"] = failed_recorder_evidence(out)
        (out / "CAPTURE.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise RuntimeError(recorder_failure_message(manifest, out))
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
        manifest["analysis"]["failures"].append(f"{result_row['dropped_frames']} complete screen samples were not saved as PNG evidence")
    manifest["crops"] = crop_frames(out, plan, frames)
    manifest["movie"] = encode(out, frames, timing, args.ffmpeg)
    if not manifest["window"]["ax_trusted"]:
        manifest["analysis"]["failures"].append("macOS Accessibility permission absent; AX proof unavailable")
    if admission["state"] != "VerifiedBuildReceipt":
        manifest["analysis"]["failures"].append("UnprovenBinarySource: " + "; ".join(admission["reasons"]))
    if plan["actions"] and not manifest["plan"]["recorder_consumption"]["exact_bytes_attested"]:
        manifest["analysis"]["failures"].append("RecorderPlanConsumptionUnverified: active plan bytes were not attested by the Swift recorder")
    manifest["passed_native_checks"] = not manifest["analysis"]["failures"]
    stable = process_executable(args.pid) == expected and sha256(expected) == manifest["binary"]["sha256"]
    stable = stable and source_identity(source_root) == source and tool_identity() == tool
    stable = stable and manifest["recorder_identity_stable"]
    stable = stable and bundle_identity(expected) == bundle
    final_plan_checks = {"source": plan_snapshot_check(source_plan),
                         "source_copy": plan_snapshot_check(source_plan_copy),
                         "resolved": plan_snapshot_check(resolved_plan),
                         "snapshot_receipt": plan_snapshot_check(manifest["plan"]["snapshot_receipt"]),
                         "prelaunch_receipt": plan_snapshot_check(before_receipt),
                         "postlaunch_receipt": plan_snapshot_check(after_receipt)}
    if supervise_v1:
        final_plan_checks["supervision_receipt"] = plan_snapshot_check(manifest["supervision_file"])
        native_stop = manifest["supervision"].get("native_stop_receipt")
        if native_stop is not None:
            final_plan_checks["native_stop_receipt"] = plan_snapshot_check(native_stop["file"])
    if attest_plan_v1 and manifest["plan"]["recorder_consumption"]["verified"]:
        final_plan_checks["consumption_receipt"] = plan_snapshot_check(
            manifest["plan"]["recorder_consumption"]["file"])
    manifest["plan"]["final_checks"] = final_plan_checks
    stable = stable and all(check["state"] == "Stable" for check in final_plan_checks.values())
    if args.compiler_receipt:
        stable = stable and sha256(args.compiler_receipt.resolve()) == admission["receipt_sha256"]
        stable = stable and compiler_admission(args.compiler_receipt, expected, source, args.preservation_receipt) == admission
    stable = stable and all(sha256(Path(item["path"])) == item["sha256"] for item in inputs)
    if args.owner_receipt:
        stable = stable and sha256(args.owner_receipt.resolve()) == manifest["owner_receipt"]["sha256"]
    manifest["capture_inputs_stable"] = stable
    if not stable:
        manifest["analysis"]["failures"].append("binary/recorder/candidate source/tool/plan snapshots/selected input or owner receipt changed during capture")
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
    parser.add_argument("--recorder", type=Path, required=True,
                        help="dedicated team-signed or frozen CDHash-addressed ad-hoc recorder .app executable")
    parser.add_argument("--launch-bundle", type=Path,
                        help="opt-in LaunchServices launch of this exact recorder .app; active plans also require --supervise-v1")
    parser.add_argument("--attest-plan-v1", action="store_true",
                        help="opt in to a new recorder's exact decoded-plan SHA-256 handshake; legacy signed recorders fail closed")
    parser.add_argument("--supervise-v1", action="store_true",
                        help="new recorder protocol: exact peer, live permits, cancellation ack and kernel NOTE_EXIT before input ownership release")
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
