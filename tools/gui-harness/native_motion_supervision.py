"""One-run, fail-closed LaunchServices input supervision.

The socket is a private control channel, not an input transport. A permit
linearizes one native posting group. Cancellation waits for its Done reply;
the driver never releases ownership until a separate kernel process-exit
event identifies the exact peer it armed.
"""
from __future__ import annotations

import json
import math
import os
import secrets
import select
import socket
import struct
import tempfile
import time
from pathlib import Path
from typing import Any, Callable

MAX_MESSAGE = 4096
MAX_SIDECAR_DEPTH = 64
LOCAL_PEERPID = 0x002  # macOS SDK sys/un.h, SOL_LOCAL=0


class ProtocolError(RuntimeError):
    pass


def single_sidecar_record(raw: bytes | None, *, limit: int = MAX_MESSAGE) -> tuple[dict[str, Any] | None, str | None]:
    """A bounded, newline-terminated record, never a process-lifetime proof."""
    if raw is None:
        return None, "absent"
    if len(raw) > limit:
        return None, f"record exceeds {limit} bytes"
    if not raw.endswith(b"\n") or len(raw.splitlines()) != 1:
        return None, "record requires one complete newline-terminated row"
    # json.loads(bytes) auto-detects UTF16/32 and strips a UTF8 BOM. Decode
    # explicitly so structural-byte scanning and JSON share a UTF8 contract.
    try:
        text = raw.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        return None, f"invalid UTF8: {error}"
    # Bound nesting before allocating decoder containers. Braces inside
    # authored/escaped strings do not consume the structural depth budget.
    depth, quoted, escaped = 0, False, False
    for byte in raw:
        if quoted:
            if escaped:
                escaped = False
            elif byte == 92:
                escaped = True
            elif byte == 34:
                quoted = False
        elif byte == 34:
            quoted = True
        elif byte in (91, 123):
            depth += 1
            if depth > MAX_SIDECAR_DEPTH:
                return None, "JSON nesting exceeds 64 levels"
        elif byte in (93, 125):
            depth -= 1

    def unique_fields(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        fields: dict[str, Any] = {}
        for key, value in pairs:
            if key in fields:
                raise ValueError("duplicate JSON field")
            fields[key] = value
        return fields

    def reject_constant(value: str) -> None:
        raise ValueError(f"non-JSON constant {value}")

    def finite_float(value: str) -> float:
        number = float(value)
        if not math.isfinite(number):
            raise ValueError("JSON number exceeds finite float range")
        return number

    try:
        row = json.loads(text, object_pairs_hook=unique_fields,
                         parse_constant=reject_constant, parse_float=finite_float)
    except (ValueError, RecursionError) as error:
        return None, f"invalid JSON: {error}"
    if not isinstance(row, dict):
        return None, "record must be an object"
    return row, None


def classify_preflight(raw: bytes | None, *, recorder: str, identifier: str,
                       pid: int, foreground_required: bool) -> dict[str, Any]:
    """Classify recorder-reported admission; metadata is not TCC attribution.

    Refusal and completion are independent: even an identity-bound rejection
    does not prove that the recorder stopped, nor release input ownership.
    """
    row, error = single_sidecar_record(raw)
    result: dict[str, Any] = {"state": "Absent" if error == "absent" else "Invalid",
                              "identity_bound": False, "native_failure_codes": [],
                              "row": row, "diagnostic": error}
    if error is not None:
        return result
    assert row is not None
    if type(row.get("schema")) is not int or row["schema"] != 1:
        result["diagnostic"] = "native preflight schema must be integer 1"
        return result
    required = {"recorder_executable": recorder, "recorder_bundle_identifier": identifier,
                "pid": pid, "foreground_required": foreground_required}
    mismatches = [key for key, value in required.items()
                  if type(row.get(key)) is not type(value) or row.get(key) != value]
    if mismatches:
        result.update(state="IdentityMismatch", diagnostic="mismatched " + ", ".join(mismatches))
        return result
    codes = row.get("failures")
    flags = ("screen_recording_preflight_granted", "ax_trusted", "stream_output_callback_ready")
    if (any(type(row.get(key)) is not bool for key in flags)
            or type(row.get("frontmost_pid")) is not int
            or not isinstance(codes, list)
            or any(type(code) is not str or not code or len(code) > 128 for code in codes)
            or len(set(codes)) != len(codes)):
        result["diagnostic"] = "native preflight requires typed flags, frontmost PID and unique failure codes"
        return result
    checks = {"ScreenRecordingPreflightDenied": not row[flags[0]],
              "AccessibilityTrustDenied": not row[flags[1]],
              "SCStreamOutputCallbackUnavailable": not row[flags[2]],
              "TargetNotFrontmost": foreground_required and row["frontmost_pid"] != pid}
    if any((code in codes) != failed for code, failed in checks.items()):
        result["diagnostic"] = "native preflight failure codes contradict admission flags"
        return result
    state = row.get("state")
    if state not in ("Admitted", "Rejected") or (state == "Admitted") != (not codes):
        result["diagnostic"] = "native preflight state contradicts failure codes"
        return result
    result.update(state=state, identity_bound=True, native_failure_codes=codes, diagnostic=None)
    return result


class LineChannel:
    def __init__(self, connection: socket.socket):
        self.connection = connection
        self.buffer = bytearray()
        self.message_deadline: float | None = None

    def receive(self, timeout: float) -> dict[str, Any]:
        # One deadline for the complete line. A peer may not extend a permit
        # window indefinitely by sending one byte before each recv timeout.
        candidate = time.monotonic() + timeout
        self.message_deadline = min(self.message_deadline, candidate) if self.message_deadline is not None else candidate
        deadline = self.message_deadline
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                if self.buffer:
                    raise ProtocolError("partial native control message exceeded its first-byte deadline")
                self.message_deadline = None
                raise socket.timeout("native control message deadline elapsed")
            self.connection.settimeout(remaining)
            try:
                block = self.connection.recv(1024)
            except socket.timeout as error:
                if self.buffer and time.monotonic() >= deadline:
                    raise ProtocolError("partial native control message exceeded its first-byte deadline") from error
                raise
            if not block:
                raise EOFError("native control channel closed")
            self.buffer.extend(block)
            if len(self.buffer) > MAX_MESSAGE:
                raise ProtocolError("native control message exceeds 4096 bytes")
        if time.monotonic() >= deadline:
            raise ProtocolError("native control message completed after its first-byte deadline")
        line, _, rest = self.buffer.partition(b"\n")
        self.buffer = bytearray(rest)
        self.message_deadline = None
        try:
            row = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ProtocolError("invalid native control JSON") from error
        if not isinstance(row, dict) or row.get("schema") != 1:
            raise ProtocolError("invalid native control schema")
        return row

    def send(self, **fields: Any) -> None:
        raw = (json.dumps({"schema": 1, **fields}, separators=(",", ":")) + "\n").encode()
        if len(raw) > MAX_MESSAGE:
            raise ProtocolError("native control reply exceeds 4096 bytes")
        self.connection.sendall(raw)


class ControlPolicy:
    """Pure typed transition table; the sender's plan and results are checked."""
    def __init__(self, actions: list[dict[str, Any]]):
        self.actions = actions
        self.state = "Armed"
        self.held = False
        self.expected_up: int | None = None
        self.pending: int | None = None
        self.previous = -1
        self.cancel_requested = False
        self.stopped = False
        self.cancel_ack = False

    def cancel(self) -> None:
        self.cancel_requested = True
        self.state = "HeldAwaitRelease" if self.held else "Cancelling"

    def handle(self, row: dict[str, Any]) -> str:
        kind = row.get("kind")
        if kind == "check":
            if row.get("held") is not self.held:
                raise ProtocolError("native held state changed without Done")
            return "HeldAwaitRelease" if self.cancel_requested and self.held else (
                "Cancel" if self.cancel_requested else "Continue")
        if kind == "permit":
            index = row.get("index")
            if type(index) is not int or not 0 <= index < len(self.actions) or \
                    index <= self.previous or self.pending is not None or \
                    row.get("action_kind") != self.actions[index]["kind"] or \
                    row.get("held") is not self.held:
                raise ProtocolError("native permit does not match ordered plan/action state")
            if not self.cancel_requested and index != self.previous + 1:
                raise ProtocolError("native recorder skipped an ordinary planned action")
            if self.cancel_requested and (not self.held or index != self.expected_up or
                                          self.actions[index]["kind"] != "mouse_up"):
                return "HeldAwaitRelease" if self.held else "Cancel"
            self.pending = index
            self.previous = index
            return "PermitRelease" if self.cancel_requested else "Permit"
        if kind == "done":
            index = row.get("index")
            if type(index) is not int or index != self.pending or \
                    row.get("action_kind") != self.actions[index]["kind"] or \
                    type(row.get("posted")) is not bool or type(row.get("held")) is not bool:
                raise ProtocolError("native Done has no exact outstanding Permit")
            action_kind = self.actions[index]["kind"]
            expected_held = self.held
            if row["posted"] and action_kind == "mouse_down":
                expected_held = True
                self.expected_up = next((at for at in range(index + 1, len(self.actions))
                    if self.actions[at]["kind"] == "mouse_up"), None)
                if self.expected_up is None:
                    raise ProtocolError("held native Down has no planned Up")
            elif row["posted"] and action_kind == "mouse_up":
                if not self.held or index != self.expected_up:
                    raise ProtocolError("native Up is not the matched release")
                expected_held = False
                self.expected_up = None
            if row["held"] is not expected_held:
                raise ProtocolError("native held state contradicts posted operation")
            self.held = expected_held
            self.pending = None
            self.state = "HeldAwaitRelease" if self.cancel_requested and self.held else (
                "Cancelling" if self.cancel_requested else "Armed")
            return "HeldAwaitRelease" if self.cancel_requested and self.held else (
                "Cancel" if self.cancel_requested else "Continue")
        if kind == "stopped":
            if self.pending is not None or row.get("held") is not self.held:
                raise ProtocolError("native stopped while posting or with inconsistent held state")
            self.stopped = True
            self.cancel_ack = self.cancel_requested and not self.held
            self.state = "StoppedHeldUnreleased" if self.held else "Stopped"
            return "StopAck" if not self.held else "HeldUnreleased"
        raise ProtocolError("unknown native control message")


class ExactProcessExit:
    """EVFILT_PROC registration pins the peer process, not a reusable PID."""
    def __init__(self, pid: int):
        self.pid = pid
        self.queue = select.kqueue()
        event = select.kevent(pid, filter=select.KQ_FILTER_PROC,
                              flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_ONESHOT,
                              fflags=select.KQ_NOTE_EXIT)
        self.queue.control([event], 0, 0)
        self.exited = False

    def wait(self, seconds: float) -> bool:
        if not self.exited:
            self.exited = any(event.fflags & select.KQ_NOTE_EXIT
                              for event in self.queue.control(None, 1, max(0, seconds)))
        return self.exited

    def close(self) -> None:
        self.queue.close()


def private_socket() -> tuple[Path, socket.socket]:
    directory = Path(tempfile.mkdtemp(prefix="nm-", dir="/private/tmp"))
    os.chmod(directory, 0o700)
    path = directory / "control.sock"
    if len(os.fsencode(path)) >= 104:  # Darwin sockaddr_un.sun_path[104].
        raise ProtocolError("native control socket path exceeds Darwin bound")
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(path))
    os.chmod(path, 0o600)
    listener.listen(1)
    return path, listener


def peer_pid(connection: socket.socket) -> int:
    raw = connection.getsockopt(0, LOCAL_PEERPID, struct.calcsize("i"))
    return struct.unpack("i", raw)[0]


def peer_eof_after_exit(connection: socket.socket) -> bool:
    """A separate bounded transport observation after kernel NOTE_EXIT."""
    previous = connection.gettimeout()
    try:
        connection.settimeout(0.5)
        return connection.recv(1) == b""
    except socket.timeout:
        return False
    finally:
        connection.settimeout(previous)


def serve(connection: socket.socket, policy: ControlPolicy, deadline: float,
          accepted: Callable[[dict[str, Any]], None]) -> dict[str, Any]:
    channel = LineChannel(connection)
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("native control deadline elapsed before Hello")
    hello = channel.receive(remaining)
    if time.monotonic() >= deadline:
        raise TimeoutError("native control Hello arrived after deadline")
    accepted(hello)
    if time.monotonic() >= deadline:
        raise TimeoutError("native identity admission exceeded deadline")
    channel.send(kind="Arm")
    transcript: list[dict[str, Any]] = []
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            policy.cancel()
        try:
            row = channel.receive(min(0.25, max(0.1, remaining)))
        except socket.timeout:
            if remaining < -2.0:
                raise TimeoutError("native recorder did not acknowledge cancellation")
            continue
        # The line may complete after the deadline, including on the same
        # recv that supplies its newline. Decide cancellation before Permit.
        if time.monotonic() >= deadline:
            policy.cancel()
        reply = policy.handle(row)
        transcript.append({"request": {key: value for key, value in row.items() if key != "nonce"},
                           "reply": reply})
        channel.send(kind=reply)
        if row["kind"] == "stopped":
            if channel.buffer:
                raise ProtocolError("native control bytes followed Stopped")
            break
    return {"state": policy.state, "cancel_requested": policy.cancel_requested,
            "cancel_ack": policy.cancel_ack, "stop_ack": policy.stopped and not policy.held,
            "held_unreleased": policy.held, "transcript": transcript}
