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
import urllib.error
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
        grant = {"tenant": "test", "node": "observer", "token_sha256": hashlib.sha256(token.encode()).hexdigest(), "activation_id": os.urandom(16).hex(), "scope": "telemetry:write", "not_before": now - 60, "expires_at": now + 3600, "revoked": False}
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
            return [sys.executable, "-c", shim, str(HERE / "sender.py"), base, str(root / token_name), str(root / "sender"), "--grant-activation", grant["activation_id"], "--observe-unit", "backend-owner.service", "--systemctl", str(manager), *extra, "--"]

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
            # Create a genuine queued goodbye with a real legacy-mode child,
            # then restore the old SQLite shape to exercise the upgrade. A new
            # generation is active before the manager-aware drain starts.
            observer.terminate()
            assert observer.wait(timeout=5) == 0
            relay.terminate()
            relay.wait(timeout=5)
            relay = None
            token2 = os.urandom(32).hex()
            (root / "token2").write_text(token2, encoding="ascii")
            (root / "token2").chmod(0o600)
            (root / "legacy").mkdir(mode=0o700)
            migration_grant = {**grant, "node": "migration", "token_sha256": hashlib.sha256(token2.encode()).hexdigest(), "activation_id": os.urandom(16).hex()}
            (root / "registry").write_text(json.dumps([migration_grant]), encoding="utf-8")
            legacy_base = [sys.executable, str(HERE / "sender.py"), base, str(root / "token2"), str(root / "legacy"), "--grant-activation", migration_grant["activation_id"]]
            legacy = subprocess.run(legacy_base + ["--", sys.executable, "-c", "pass"], timeout=10)
            assert legacy.returncode == 0
            with sqlite3.connect(root / "legacy" / "sender.sqlite") as db:
                assert any(json.loads(row[0])["kind"] == "goodbye" for row in db.execute("SELECT body FROM queue"))
                db.execute("ALTER TABLE queue DROP COLUMN generation")
                db.execute("DROP TABLE observed")
            fourth, start4 = owner()
            write_status(fourth.pid, start4, "active")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(lambda: urllib.request.urlopen(base + "/ready", timeout=1).status == 200)
            migrated = [sys.executable, "-c", shim, str(HERE / "sender.py"), base, str(root / "token2"), str(root / "legacy"), "--grant-activation", migration_grant["activation_id"], "--drain-only", "true", "--observe-unit", "backend-owner.service", "--systemctl", str(manager), "--"]
            assert subprocess.run(migrated, timeout=10).returncode == 0
            with sqlite3.connect(root / "legacy" / "sender.sqlite") as db:
                assert "generation" in {row[1] for row in db.execute("PRAGMA table_info(queue)")}
                assert db.execute("SELECT COUNT(*) FROM queue").fetchone()[0] == 0
            body = metrics()
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="migration"}' not in body
            assert 'nudox_node_goodbye_seconds{tenant="test",node="migration"}' not in body
            # G1 really exits clean while relay is offline. Rotate the operator
            # grant before any G2 starts; neither replay nor drain can relabel
            # G1's queued goodbye into the new activation.
            relay.terminate()
            relay.wait(timeout=5)
            relay = None
            migration_observer = [sys.executable, "-c", shim, str(HERE / "sender.py"), base, str(root / "token2"), str(root / "legacy"), "--grant-activation", migration_grant["activation_id"], "--observe-unit", "backend-owner.service", "--systemctl", str(manager), "--interval-seconds", "1", "--"]
            observer = subprocess.Popen(migration_observer)
            def migration_pending(kind):
                with sqlite3.connect(root / "legacy" / "sender.sqlite") as db:
                    return any(json.loads(row[0])["kind"] == kind for row in db.execute("SELECT body FROM queue"))
            wait_for(lambda: migration_pending("heartbeat"))
            fourth.terminate()
            assert fourth.wait(timeout=5) == 0
            write_status(fourth.pid, start4, "inactive")
            wait_for(lambda: migration_pending("goodbye"))
            observer.terminate()
            assert observer.wait(timeout=5) == 0
            with sqlite3.connect(root / "legacy" / "sender.sqlite") as db:
                old_goodbye = next(row[0] for row in db.execute("SELECT body FROM queue") if json.loads(row[0])["kind"] == "goodbye")
            token3 = os.urandom(32).hex()
            (root / "token3").write_text(token3, encoding="ascii")
            (root / "token3").chmod(0o600)
            reactivated = {**migration_grant, "token_sha256":hashlib.sha256(token3.encode()).hexdigest(), "activation_id":os.urandom(16).hex(), "not_before":int(time.time()) + 2}
            (root / "registry").write_text(json.dumps([reactivated]), encoding="utf-8")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(lambda: urllib.request.urlopen(base + "/ready", timeout=1).status == 200)
            reject_before = counter(metrics(), "relay_rejected_total")
            req = urllib.request.Request(base + "/v1/records", data=old_goodbye, headers={"Authorization":"Bearer " + token3})
            try:
                urllib.request.urlopen(req, timeout=2)
                raise AssertionError("old activation accepted")
            except urllib.error.HTTPError as error:
                assert error.code == 409
            assert counter(metrics(), "relay_rejected_total") == reject_before + 1
            lost_before = counter(metrics(), "relay_node_reported_dropped_total")
            reissued_drain = [sys.executable, "-c", shim, str(HERE / "sender.py"), base, str(root / "token3"), str(root / "legacy"), "--grant-activation", reactivated["activation_id"], "--drain-only", "true", "--observe-unit", "backend-owner.service", "--systemctl", str(manager), "--"]
            assert subprocess.run(reissued_drain, timeout=10).returncode == 0
            body = metrics()
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="migration"}' not in body
            assert 'nudox_node_goodbye_seconds{tenant="test",node="migration"}' not in body
            assert 'nudox_node_telemetry_grant_expected_since_seconds{tenant="test",node="migration"}' in body
            assert counter(body, "relay_node_reported_dropped_total") > lost_before
            # Activation IDs/credentials cannot be reused on a later restart.
            relay.terminate()
            relay.wait(timeout=5)
            (root / "registry").write_text(json.dumps([migration_grant]), encoding="utf-8")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            def refused_ready():
                try:
                    urllib.request.urlopen(base + "/ready", timeout=1)
                    return False
                except urllib.error.HTTPError as error:
                    return error.code == 503
            wait_for(refused_ready)
            relay.terminate()
            relay.wait(timeout=5)
            mutated = {**reactivated, "not_before":reactivated["not_before"] + 1}
            (root / "registry").write_text(json.dumps([mutated]), encoding="utf-8")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(refused_ready)
            relay.terminate()
            relay.wait(timeout=5)
            # Failed rotations leave the prior durable activation intact.
            (root / "registry").write_text(json.dumps([reactivated]), encoding="utf-8")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(lambda: urllib.request.urlopen(base + "/ready", timeout=1).status == 200)
            relay.terminate()
            relay.wait(timeout=5)
            reused_token = {**reactivated, "activation_id":os.urandom(16).hex(), "not_before":reactivated["not_before"] + 1}
            (root / "registry").write_text(json.dumps([reused_token]), encoding="utf-8")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(refused_ready)
            # Restore the old relay schema around actual prior receipts; the
            # upgrade must retain counters but hide unbound liveness.
            relay.terminate()
            relay.wait(timeout=5)
            with sqlite3.connect(root / "relay" / "state") as db:
                old_receipts = db.execute("SELECT heartbeat,goodbye FROM node WHERE tenant='test' AND node='observer'").fetchone()
                assert old_receipts and old_receipts[0] is not None and old_receipts[1] is not None
                db.execute("DROP TABLE grant_history")
                db.execute("DROP TABLE grant_current")
                db.execute("ALTER TABLE node DROP COLUMN activation")
            legacy_relay_grant = {**grant, "activation_id":os.urandom(16).hex(), "token_sha256":hashlib.sha256(os.urandom(32)).hexdigest(), "not_before":int(time.time()) + 2}
            (root / "registry").write_text(json.dumps([legacy_relay_grant]), encoding="utf-8")
            relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(root / "registry"), str(root / "relay" / "state"), str(port)])
            wait_for(lambda: urllib.request.urlopen(base + "/ready", timeout=1).status == 200)
            body = metrics()
            assert 'nudox_node_telemetry_grant_expected_since_seconds{tenant="test",node="observer"}' in body
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="observer"}' not in body
            assert 'nudox_node_goodbye_seconds{tenant="test",node="observer"}' not in body
            print("simulated manager with real children: owner independence, generation/activation fences, reload, clean stop, crash and legacy migrations passed")
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
