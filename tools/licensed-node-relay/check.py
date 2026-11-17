#!/usr/bin/env python3
"""Real-process integration check; uses only temporary credentials and state."""
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
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent


def wait_for(f, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = f()
            if result:
                return result
        except (OSError, ValueError, StopIteration, sqlite3.OperationalError):
            pass
        time.sleep(0.1)
    raise AssertionError("timed out waiting for expected process state")


def main():
    with tempfile.TemporaryDirectory(prefix="nudox-telemetry-check-") as tmp:
        root = Path(tmp)
        os.chmod(root, 0o700)
        for name in ("relay", "sender"):
            (root / name).mkdir(mode=0o700)
        token = os.urandom(32).hex()
        (root / "token").write_text(token, encoding="ascii")
        (root / "token").chmod(0o600)
        now = int(time.time())
        grant = {"tenant": "test", "node": "process", "token_sha256": hashlib.sha256(token.encode()).hexdigest(), "scope": "telemetry:write", "not_before": now - 60, "expires_at": now + 3600, "revoked": False}
        registry = root / "registry"

        def write_grant(value):
            staging = root / "grant.new"
            staging.write_text(json.dumps([value]), encoding="utf-8")
            staging.chmod(0o600)
            os.replace(staging, registry)

        write_grant(grant)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"

        def request(path, body=None, bearer=token):
            req = urllib.request.Request(base + path, data=body, headers={"Authorization": "Bearer " + bearer})
            try:
                with urllib.request.urlopen(req, timeout=1) as response:
                    return response.status, response.read().decode()
            except urllib.error.HTTPError as error:
                return error.code, error.read().decode()

        def metrics():
            status, body = request("/metrics")
            return body if status == 200 else ""

        def counter(body, name):
            return int(next(line.split()[-1] for line in body.splitlines() if line.startswith(name + " ")))

        child = "import signal,sys,time;signal.signal(signal.SIGTERM,lambda *_:sys.exit(0));time.sleep(60)"
        sender = subprocess.Popen([sys.executable, str(HERE / "sender.py"), base, str(root / "token"), str(root / "sender"), "--interval-seconds", "1", "--spool-capacity", "1", "--", sys.executable, "-c", child])
        relay = None

        def start_relay():
            process = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(registry), str(root / "relay" / "state.sqlite"), str(port)])
            wait_for(lambda: request("/health")[0] == 200)
            return process

        try:
            # The live child generates real records while its relay is absent;
            # a one-record FIFO produces real, durably counted overflow.
            def overflow():
                with sqlite3.connect(root / "sender" / "sender.sqlite") as db:
                    return db.execute("SELECT lost FROM meta").fetchone()[0] >= 2
            wait_for(overflow)
            # Capture the real sender's pending heartbeat and freeze its
            # supervisor so no later sequence can race this replay check.
            sender.send_signal(signal.SIGSTOP)
            os.waitpid(sender.pid, os.WUNTRACED)
            with sqlite3.connect(root / "sender" / "sender.sqlite") as db:
                exact = db.execute("SELECT body FROM queue ORDER BY seq LIMIT 1").fetchone()[0]
            relay = start_relay()
            assert request("/ready")[0] == 200
            assert request("/v1/records", exact)[0] == 202
            assert request("/v1/records", exact)[0] == 200
            relay.terminate()
            relay.wait(timeout=5)
            relay = start_relay()
            assert request("/v1/records", exact)[0] == 200
            sender.send_signal(signal.SIGCONT)
            wait_for(lambda: counter(metrics(), "relay_node_reported_dropped_total") >= 2)
            wait_for(lambda: 'nudox_node_heartbeat_seconds{tenant="test",node="process"}' in metrics())
            sender.send_signal(signal.SIGTERM)
            assert sender.wait(timeout=10) == 0
            text = wait_for(lambda: metrics() if 'nudox_node_goodbye_seconds{tenant="test",node="process"}' in metrics() else None)
            heartbeat = float(next(x.split()[-1] for x in text.splitlines() if x.startswith('nudox_node_heartbeat_seconds{')))
            goodbye = float(next(x.split()[-1] for x in text.splitlines() if x.startswith('nudox_node_goodbye_seconds{')))
            assert goodbye >= heartbeat
            clean_goodbyes = counter(text, 'relay_records_total{kind="goodbye"}')
            # A real child killed without orderly retirement must not cause a
            # goodbye. The later heartbeat must supersede the earlier goodbye.
            pid_file = root / "child.pid"
            crashing_child = "import os,time;open(" + repr(str(pid_file)) + ", 'w').write(str(os.getpid()));time.sleep(60)"
            sender = subprocess.Popen([sys.executable, str(HERE / "sender.py"), base, str(root / "token"), str(root / "sender"), "--", sys.executable, "-c", crashing_child])
            wait_for(lambda: pid_file.exists())
            def newer_heartbeat():
                body = metrics()
                value = float(next(x.split()[-1] for x in body.splitlines() if x.startswith('nudox_node_heartbeat_seconds{')))
                return body if value > goodbye else None
            wait_for(newer_heartbeat)
            os.kill(int(pid_file.read_text()), signal.SIGKILL)
            assert sender.wait(timeout=10) == 137
            assert counter(metrics(), 'relay_records_total{kind="goodbye"}') == clean_goodbyes
            assert request("/v1/records", b"{}", bearer="invalid")[0] == 401
            assert request("/v1/records", b"{}")[0] == 400
            assert request("/v1/records", b"x" * 1025)[0] == 413
            stale = json.dumps({"seq": 2**40, "kind": "heartbeat", "created_at": now - 600}).encode()
            assert request("/v1/records", stale)[0] == 409
            write_grant({**grant, "revoked": True})
            assert request("/v1/records", exact)[0] == 401
            write_grant({**grant, "expires_at": int(time.time()) - 1})
            assert request("/v1/records", exact)[0] == 401
            write_grant({**grant, "not_before": int(time.time()) + 60})
            assert request("/v1/records", exact)[0] == 401
            write_grant(grant)
            relay.terminate()
            relay.wait(timeout=5)
            relay = start_relay()
            print("real process overflow, diagnostic, goodbye, crash, denial and restart/dedup: passed")
        finally:
            if sender.poll() is None:
                sender.send_signal(signal.SIGCONT)
                sender.terminate()
                try:
                    sender.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    sender.kill()
                    sender.wait()
            if relay is not None and relay.poll() is None:
                relay.terminate()
                relay.wait(timeout=5)


if __name__ == "__main__":
    main()
