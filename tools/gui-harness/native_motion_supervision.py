"""One-run, fail-closed LaunchServices input supervision.

The socket is a private control channel, not an input transport. A permit
linearizes one native posting group. Cancellation waits for its Done reply;
the driver never releases ownership until a separate kernel process-exit
event identifies the exact peer it armed.
"""
from __future__ import annotations

import json
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
LOCAL_PEERPID = 0x002  # macOS SDK sys/un.h, SOL_LOCAL=0


class ProtocolError(RuntimeError):
    pass


class LineChannel:
    def __init__(self, connection: socket.socket):
        self.connection = connection
        self.buffer = bytearray()

    def receive(self, timeout: float) -> dict[str, Any]:
        self.connection.settimeout(timeout)
        while b"\n" not in self.buffer:
            try:
                block = self.connection.recv(1024)
            except socket.timeout:
                raise
            if not block:
                raise EOFError("native control channel closed")
            self.buffer.extend(block)
            if len(self.buffer) > MAX_MESSAGE:
                raise ProtocolError("native control message exceeds 4096 bytes")
        line, _, rest = self.buffer.partition(b"\n")
        self.buffer = bytearray(rest)
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


def serve(connection: socket.socket, policy: ControlPolicy, deadline: float,
          accepted: Callable[[dict[str, Any]], None]) -> dict[str, Any]:
    channel = LineChannel(connection)
    hello = channel.receive(max(0.1, deadline - time.monotonic()))
    accepted(hello)
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
        reply = policy.handle(row)
        transcript.append({"request": {key: value for key, value in row.items() if key != "nonce"},
                           "reply": reply})
        channel.send(kind=reply)
        if row["kind"] == "stopped":
            break
    return {"state": policy.state, "cancel_requested": policy.cancel_requested,
            "cancel_ack": policy.cancel_ack, "stop_ack": policy.stopped and not policy.held,
            "held_unreleased": policy.held, "transcript": transcript}
