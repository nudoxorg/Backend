#!/usr/bin/env python3
"""Real-child observer control with an explicitly simulated systemd manager.

This does not prove actual systemd semantics; the NixOS VM must do that.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = Path(__file__).resolve().parent


def wait_for(f, seconds=12):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = f()
            if value:
                return value
        except (OSError, ValueError, StopIteration, sqlite3.OperationalError):
            pass
        time.sleep(0.1)
    raise AssertionError("timed out waiting for observer control")


def main():
    with tempfile.TemporaryDirectory(prefix="nudox-observer-check-") as tmp:
        root = Path(tmp)
        os.chmod(root, 0o700)
        for name in ("relay", "sender"):
            (root / name).mkdir(mode=0o700)
        token = os.urandom(32).hex()
        for name, value in (("token", token), ("bad", "bad")):
            (root / name).write_text(value, encoding="ascii")
            (root / name).chmod(0o600)
        now = int(time.time())
        grant = {"tenant": "test", "node": "observer", "token_sha256": hashlib.sha256(token.encode()).hexdigest(), "scope": "telemetry:write", "not_before": now - 60, "expires_at": now + 3600, "revoked": False}
        (root / "registry").write_text(json.dumps([grant]), encoding="utf-8")
        (root / "registry").chmod(0o600)
        manager = root / "simulated-systemctl"
        manager.write_text("#!" + sys.executable + "\nimport pathlib,sys\nsys.stdout.write(pathlib.Path(" + repr(str(root / "status")) + ").read_text())\n", encoding="utf-8")
        manager.chmod(0o700)
        # Only this test substitutes boot identity on non-Linux hosts; status
        # input is a deliberately simulated manager bound to actual children.
        shim = 'import pathlib,runpy,sys; orig=pathlib.Path.read_text; pathlib.Path.read_text=lambda self,*a,**k: "12345678-1234-1234-1234-123456789abc" if str(self)=="/proc/sys/kernel/random/boot_id" else orig(self,*a,**k); sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0],run_name="__main__")'
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        relay = observer = None
        owners = []

        def write_status(pid, start, state, clean=True):
            active = state in ("active", "reloading")
            values = {"Id": "backend-owner.service", "LoadState": "loaded", "ActiveState": state, "MainPID": pid if active else 0, "ExecMainPID": pid, "ExecMainStartTimestampMonotonic": start, "ExecMainCode": 0 if active else (1 if clean else 2), "ExecMainStatus": 0 if clean else 9, "Result": "success" if clean else "signal"}
            staging = root / "status.new"
            staging.write_text("".join(f"{k}={v}\n" for k, v in values.items()), encoding="ascii")
            os.replace(staging, root / "status")

        def owner():
            ready = root / f"ready-{len(owners)}"
            code = "import pathlib,signal,sys,time;signal.signal(signal.SIGTERM,lambda *_:sys.exit(0));pathlib.Path(" + repr(str(ready)) + ").touch();time.sleep(60)"
            started = int(time.monotonic() * 1_000_000)
            child = subprocess.Popen([sys.executable, "-c", code])
            owners.append(child)
            wait_for(ready.exists)
            return child, started

        def invoke(token_name, extra=()):
            return [sys.executable, "-c", shim, str(HERE / "sender.py"), base, str(root / token_name), str(root / "sender"), "--observe-unit", "backend-owner.service", "--systemctl", str(manager), *extra, "--"]

        def pending(kind):
            with sqlite3.connect(root / "sender" / "sender.sqlite") as db:
                return any(json.loads(row[0])["kind"] == kind for row in db.execute("SELECT body FROM queue"))

        def metrics():
            with urllib.request.urlopen(base + "/metrics", timeout=1) as response:
                return response.read().decode()

        def counter(body, name):
            return int(next(line.split()[-1] for line in body.splitlines() if line.startswith(name + " ")))

        try:
            first, start1 = owner()
            write_status(first.pid, start1, "active")
            bad = subprocess.run(invoke("bad"), capture_output=True, timeout=5)
            assert bad.returncode != 0 and first.poll() is None
            observer = subprocess.Popen(invoke("token", ("--interval-seconds", "1")))
            wait_for(lambda: pending("heartbeat"))
            first.terminate()
            assert first.wait(timeout=5) == 0
            write_status(first.pid, start1, "inactive")
            wait_for(lambda: pending("goodbye"))
            observer.terminate()
            assert observer.wait(timeout=5) == 0
            second, start2 = owner()
            write_status(second.pid, start2, "active")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(lambda: urllib.request.urlopen(base + "/ready", timeout=1).status == 200)
            drain = subprocess.run(invoke("token", ("--drain-only", "true")), timeout=10)
            assert drain.returncode == 0
            body = metrics()
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="observer"}' not in body
            assert 'nudox_node_goodbye_seconds{tenant="test",node="observer"}' not in body
            assert counter(body, "relay_node_reported_dropped_total") >= 1
            observer = subprocess.Popen(invoke("token", ("--interval-seconds", "1")))
            wait_for(lambda: 'nudox_node_heartbeat_seconds{tenant="test",node="observer"}' in metrics())
            observer.kill()
            observer.wait(timeout=5)
            assert second.poll() is None
            observer = subprocess.Popen(invoke("token", ("--interval-seconds", "1")))
            write_status(second.pid, start2, "reloading")
            baseline = counter(metrics(), 'relay_records_total{kind="heartbeat"}')
            wait_for(lambda: counter(metrics(), 'relay_records_total{kind="heartbeat"}') > baseline)
            second.terminate()
            assert second.wait(timeout=5) == 0
            write_status(second.pid, start2, "inactive")
            wait_for(lambda: 'nudox_node_goodbye_seconds{tenant="test",node="observer"}' in metrics())
            clean_count = counter(metrics(), 'relay_records_total{kind="goodbye"}')
            third, start3 = owner()
            write_status(third.pid, start3, "active")
            baseline = counter(metrics(), 'relay_records_total{kind="heartbeat"}')
            wait_for(lambda: counter(metrics(), 'relay_records_total{kind="heartbeat"}') > baseline)
            third.kill()
            assert third.wait(timeout=5) < 0
            write_status(third.pid, start3, "failed", clean=False)
            def ended():
                with sqlite3.connect(root / "sender" / "sender.sqlite") as db:
                    return db.execute("SELECT ended FROM observed").fetchone()[0] == 1
            wait_for(ended)
            assert counter(metrics(), 'relay_records_total{kind="goodbye"}') == clean_count
            print("simulated manager with real children: owner independence, generation recheck, reload, clean stop and crash passed")
        finally:
            if observer is not None and observer.poll() is None:
                observer.terminate()
                observer.wait(timeout=5)
            for child in owners:
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=5)
            if relay is not None and relay.poll() is None:
                relay.terminate()
                relay.wait(timeout=5)


if __name__ == "__main__":
    main()
