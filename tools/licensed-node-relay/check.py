#!/usr/bin/env python3
"""Real-process integration check; uses only temporary credentials and state."""
import hashlib
import http.client
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import threading
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
        grant = {"tenant": "test", "node": "process", "token_sha256": hashlib.sha256(token.encode()).hexdigest(), "activation_id": os.urandom(16).hex(), "scope": "telemetry:write", "not_before": now - 60, "expires_at": now + 3600, "revoked": False}
        registry = root / "registry"

        def write_grants(values):
            staging = root / "grant.new"
            staging.write_text(json.dumps(values), encoding="utf-8")
            staging.chmod(0o600)
            os.replace(staging, registry)

        def write_grant(value):
            write_grants([value])

        expired_token, future_token = os.urandom(32).hex(), os.urandom(32).hex()
        expired = {**grant, "node":"expired", "token_sha256":hashlib.sha256(expired_token.encode()).hexdigest(), "activation_id":os.urandom(16).hex(), "not_before":now-600, "expires_at":now-1}
        future = {**grant, "node":"future", "token_sha256":hashlib.sha256(future_token.encode()).hexdigest(), "activation_id":os.urandom(16).hex(), "not_before":now+60}
        initial = [grant, expired, future]
        write_grants(initial)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"

        def request(path, body=None, bearer=token):
            req = urllib.request.Request(base + path, data=body, headers={"Authorization": "Bearer " + bearer})
            try:
                with urllib.request.urlopen(req, timeout=10) as response:
                    return response.status, response.read().decode()
            except urllib.error.HTTPError as error:
                return error.code, error.read().decode()

        def metrics():
            status, body = request("/metrics")
            return body if status == 200 else ""

        def counter(body, name):
            return int(next(line.split()[-1] for line in body.splitlines() if line.startswith(name + " ")))

        child = "import signal,sys,time;signal.signal(signal.SIGTERM,lambda *_:sys.exit(0));time.sleep(60)"
        sender = subprocess.Popen([sys.executable, str(HERE / "sender.py"), base, str(root / "token"), str(root / "sender"), "--grant-activation", grant["activation_id"], "--interval-seconds", "1", "--spool-capacity", "1", "--", sys.executable, "-c", child])
        relay = None
        group_mode = False

        def start_relay():
            process = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(registry), str(root / "relay" / "state.sqlite"), str(port)])
            try:
                wait_for(lambda: request("/health")[0] == 200)
                return process
            except BaseException:
                process.terminate()
                process.wait(timeout=5)
                raise

        try:
            # The live child generates real records while its relay is absent;
            # a one-record FIFO produces real, durably counted overflow.
            def overflow():
                if sender.poll() is not None:
                    raise AssertionError(f"sender exited before overflow: {sender.returncode}")
                with sqlite3.connect(root / "sender" / "sender.sqlite") as db:
                    row = db.execute("SELECT lost FROM meta").fetchone()
                    return row is not None and row[0] >= 2
            wait_for(overflow, seconds=40)
            # Capture the real sender's pending heartbeat and freeze its
            # supervisor so no later sequence can race this replay check.
            sender.send_signal(signal.SIGSTOP)
            os.waitpid(sender.pid, os.WUNTRACED)
            with sqlite3.connect(root / "sender" / "sender.sqlite") as db:
                exact = db.execute("SELECT body FROM queue ORDER BY seq LIMIT 1").fetchone()[0]
            relay = start_relay()
            wait_for(lambda: request("/ready")[0] == 200, seconds=40)
            with socket.socket() as other:
                other.bind(("127.0.0.1", 0))
                second_port = other.getsockname()[1]
            competing = subprocess.run([sys.executable, str(HERE / "relay.py"), str(registry), str(root / "relay" / "state.sqlite"), str(second_port)], capture_output=True, timeout=5)
            assert competing.returncode != 0
            wait_for(lambda: request("/ready")[0] == 200, seconds=40)
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
            sender = subprocess.Popen([sys.executable, str(HERE / "sender.py"), base, str(root / "token"), str(root / "sender"), "--grant-activation", grant["activation_id"], "--", sys.executable, "-c", crashing_child])
            wait_for(lambda: pid_file.exists())
            def newer_heartbeat():
                body = metrics()
                value = float(next(x.split()[-1] for x in body.splitlines() if x.startswith('nudox_node_heartbeat_seconds{')))
                return body if value > goodbye else None
            wait_for(newer_heartbeat)
            os.kill(int(pid_file.read_text()), signal.SIGKILL)
            assert sender.wait(timeout=10) == 137
            assert counter(metrics(), 'relay_records_total{kind="goodbye"}') == clean_goodbyes
            # Model systemd's control-group signal delivery with a dedicated
            # process group. The child detects a duplicated termination signal.
            group_pid = root / "group-child.pid"
            group_child = (
                "import os,signal,sys,time\n"
                "seen=False\n"
                "def stop(*_):\n"
                " global seen\n"
                " if seen: sys.exit(143)\n"
                " seen=True\n"
                " time.sleep(0.5)\n"
                " sys.exit(0)\n"
                "signal.signal(signal.SIGTERM,stop)\n"
                "open(" + repr(str(group_pid)) + ", 'w').write(str(os.getpid()))\n"
                "time.sleep(60)\n"
            )
            group_mode = True
            sender = subprocess.Popen([sys.executable, str(HERE / "sender.py"), base, str(root / "token"), str(root / "sender"), "--grant-activation", grant["activation_id"], "--signal-mode", "cgroup", "--", sys.executable, "-c", group_child], start_new_session=True)
            wait_for(lambda: group_pid.exists())
            os.killpg(sender.pid, signal.SIGTERM)
            assert sender.wait(timeout=10) == 0
            group_mode = False
            wait_for(lambda: counter(metrics(), 'relay_records_total{kind="goodbye"}') == clean_goodbyes + 1)
            assert request("/v1/records", b"{}", bearer="invalid")[0] == 401
            assert request("/v1/records", b"{}")[0] == 400
            assert request("/v1/records", b"x" * 1025)[0] == 413
            stale = json.dumps({"seq": 2**40, "kind": "heartbeat", "created_at": now - 600, "activation_id": grant["activation_id"]}).encode()
            assert request("/v1/records", stale)[0] == 409
            assert request("/v1/records", exact, bearer=expired_token)[0] == 401
            assert request("/v1/records", exact, bearer=future_token)[0] == 401
            before = counter(metrics(), "relay_rejected_total")
            # A live file rewrite does not change this immutable snapshot.
            registry.write_text("{", encoding="utf-8")
            assert request("/ready")[0] == 200
            relay.terminate()
            relay.wait(timeout=5)
            relay = start_relay()
            assert request("/v1/records", exact)[0] == 503
            relay.terminate()
            relay.wait(timeout=5)
            write_grants(initial)
            relay = start_relay()
            assert counter(metrics(), "relay_rejected_total") == before
            assert request("/ready")[0] == 200
            # Revocation is authoritative after service restart.
            relay.terminate()
            relay.wait(timeout=5)
            write_grants([{**grant, "revoked": True}, expired, future])
            relay = start_relay()
            assert request("/v1/records", exact)[0] == 401
            # After an orderly stop during an outage, a separate timer process
            # must deliver the actual queued goodbye without starting a node.
            relay.terminate()
            relay.wait(timeout=5)
            relay = None
            token2 = os.urandom(32).hex()
            (root / "token2").write_text(token2, encoding="ascii")
            (root / "token2").chmod(0o600)
            (root / "drain-state").mkdir(mode=0o700)
            grant2 = {**grant, "node": "drain", "token_sha256": hashlib.sha256(token2.encode()).hexdigest(), "activation_id": os.urandom(16).hex()}
            write_grant(grant2)
            pid_file = root / "drain-child.pid"
            draining_child = "import os,signal,sys,time;signal.signal(signal.SIGTERM,lambda *_:sys.exit(0));open(" + repr(str(pid_file)) + ",'w').write(str(os.getpid()));time.sleep(60)"
            drain_base = [sys.executable, str(HERE / "sender.py"), base, str(root / "token2"), str(root / "drain-state"), "--grant-activation", grant2["activation_id"]]
            sender = subprocess.Popen(drain_base + ["--", sys.executable, "-c", draining_child])
            wait_for(lambda: pid_file.exists())
            assert subprocess.run(drain_base + ["--drain-only", "true", "--"], timeout=10).returncode == 75
            sender.terminate()
            assert sender.wait(timeout=10) == 0
            relay = start_relay()
            assert subprocess.run(drain_base + ["--drain-only", "true", "--"], timeout=10).returncode == 0
            body = metrics()
            assert 'nudox_node_goodbye_seconds{tenant="test",node="drain"}' in body
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="drain"}' not in body
            # A hard-killed sender may leave a heartbeat queued. The timer has
            # no live observation and must count abandonment, not replay it.
            relay.terminate()
            relay.wait(timeout=5)
            relay = None
            token3 = os.urandom(32).hex()
            (root / "token3").write_text(token3, encoding="ascii")
            (root / "token3").chmod(0o600)
            (root / "killed-state").mkdir(mode=0o700)
            grant3 = {**grant, "node": "killed", "token_sha256": hashlib.sha256(token3.encode()).hexdigest(), "activation_id": os.urandom(16).hex()}
            write_grant(grant3)
            pid_file = root / "killed-child.pid"
            killed_child = "import os,time;open(" + repr(str(pid_file)) + ",'w').write(str(os.getpid()));time.sleep(60)"
            kill_base = [sys.executable, str(HERE / "sender.py"), base, str(root / "token3"), str(root / "killed-state"), "--grant-activation", grant3["activation_id"]]
            sender = subprocess.Popen(kill_base + ["--", sys.executable, "-c", killed_child])
            wait_for(lambda: pid_file.exists())
            def pending_heartbeat():
                with sqlite3.connect(root / "killed-state" / "sender.sqlite") as db:
                    return db.execute("SELECT COUNT(*) FROM queue").fetchone()[0] > 0
            wait_for(pending_heartbeat)
            sender.kill()
            sender.wait(timeout=5)
            os.kill(int(pid_file.read_text()), signal.SIGKILL)
            relay = start_relay()
            assert subprocess.run(kill_base + ["--drain-only", "true", "--"], timeout=10).returncode == 0
            body = metrics()
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="killed"}' not in body
            assert 'nudox_node_goodbye_seconds{tenant="test",node="killed"}' not in body
            assert counter(body, "relay_node_reported_dropped_total") >= 1
            print("real process overflow, diagnostic, graceful/group shutdown, crash, denial, restart/dedup and offline drains: passed")
        finally:
            if sender.poll() is None:
                sender.send_signal(signal.SIGCONT)
                if group_mode:
                    os.killpg(sender.pid, signal.SIGTERM)
                else:
                    sender.terminate()
                try:
                    sender.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    if group_mode:
                        os.killpg(sender.pid, signal.SIGKILL)
                    else:
                        sender.kill()
                    sender.wait()
            if relay is not None and relay.poll() is None:
                relay.terminate()
                relay.wait(timeout=5)


def isolation():
    with tempfile.TemporaryDirectory(prefix="nudox-telemetry-isolation-") as tmp:
        root = Path(tmp)
        os.chmod(root, 0o700)
        for name in ("relay", "one", "two"):
            (root / name).mkdir(mode=0o700)
        tokens = [os.urandom(32).hex(), os.urandom(32).hex()]
        now = int(time.time())
        grants = []
        for i, node in enumerate(("one", "two")):
            path = root / ("token" + str(i))
            path.write_text(tokens[i], encoding="ascii")
            path.chmod(0o600)
            grants.append({"tenant": "test", "node": node, "token_sha256": hashlib.sha256(tokens[i].encode()).hexdigest(), "activation_id": os.urandom(16).hex(), "scope": "telemetry:write", "not_before": now - 600, "expires_at": now + 3600, "revoked": False})
        registry = root / "registry"
        registry.write_text(json.dumps(grants), encoding="utf-8")
        registry.chmod(0o600)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        relay = subprocess.Popen([sys.executable, str(HERE / "relay.py"), str(registry), str(root / "relay" / "state"), str(port)])
        senders = []
        slow = []
        child = "import signal,sys,time;signal.signal(signal.SIGTERM,lambda *_:sys.exit(0));time.sleep(60)"
        def get(path):
            with urllib.request.urlopen(base + path, timeout=1) as response:
                return response.read().decode()
        def post(token):
            req = urllib.request.Request(base + "/v1/records", data=b"{}", headers={"Authorization": "Bearer " + token})
            try:
                with urllib.request.urlopen(req, timeout=2) as response:
                    return response.status
            except urllib.error.HTTPError as error:
                error.read()
                return error.code
            except (urllib.error.URLError, http.client.RemoteDisconnected, TimeoutError, ConnectionError):
                # A full bounded admission pool may shed a connection before
                # an HTTP response; that is not a received rejection.
                return None
        def counter(body, name):
            return int(next(line.split()[-1] for line in body.splitlines() if line.startswith(name + " ")))
        def heartbeat(body, node):
            marker = f'nudox_node_heartbeat_seconds{{tenant="test",node="{node}"}} '
            return float(next(line[len(marker):] for line in body.splitlines() if line.startswith(marker)))
        try:
            wait_for(lambda: get("/ready"))
            def start(i):
                senders.append(subprocess.Popen([sys.executable, str(HERE / "sender.py"), base, str(root / ("token" + str(i))), str(root / ("one" if i == 0 else "two")), "--grant-activation", grants[i]["activation_id"], "--interval-seconds", "1", "--", sys.executable, "-c", child]))
            start(0)
            first = wait_for(lambda: heartbeat(get("/metrics"), "one"))
            body = get("/metrics")
            assert 'nudox_node_telemetry_grant_expected_since_seconds{tenant="test",node="two"}' in body
            assert 'nudox_node_heartbeat_seconds{tenant="test",node="two"}' not in body
            start(1)
            second = wait_for(lambda: heartbeat(get("/metrics"), "two"))
            # Hold incomplete header and incomplete authenticated body open;
            # other requests must proceed and both sockets must be closed by an
            # absolute deadline even if the peer never finishes its request.
            header = socket.create_connection(("127.0.0.1", port), timeout=1)
            header.sendall(b"GET /health HTTP/1.1\r\nHost: local\r\nX-Slow:")
            slow.append(header)
            partial = socket.create_connection(("127.0.0.1", port), timeout=1)
            partial.sendall(("POST /v1/records HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer " + tokens[0] + "\r\nContent-Length: 1000\r\n\r\n{").encode())
            slow.append(partial)
            closed_after = []
            started = time.monotonic()
            def trickle(sock):
                while time.monotonic() - started < 5:
                    try:
                        sock.sendall(b"x")
                    except OSError:
                        break
                    time.sleep(0.2)
                closed_after.append(time.monotonic() - started)
            tricklers = [threading.Thread(target=trickle, args=(sock,), daemon=True) for sock in slow]
            for thread in tricklers:
                thread.start()
            assert get("/health") == "ok\n"
            assert get("/ready") == "ready\n"
            assert heartbeat(get("/metrics"), "two") >= second
            # Keep a bounded invalid-client flood active across multiple real
            # heartbeat intervals, and observe both nodes advance during it.
            def flood(token, results, stop):
                while not stop.is_set():
                    results.append(post(token))
                    time.sleep(0.02)
            rejected_before = counter(get("/metrics"), "relay_rejected_total")
            statuses = []
            stop = threading.Event()
            invalid_flood = threading.Thread(target=flood, args=("invalid", statuses, stop), daemon=True)
            invalid_flood.start()
            wait_for(lambda: heartbeat(get("/metrics"), "one") > first, seconds=8)
            wait_for(lambda: heartbeat(get("/metrics"), "two") > second, seconds=8)
            wait_for(lambda: 429 in statuses, seconds=20)
            stop.set()
            invalid_flood.join(timeout=5)
            assert not invalid_flood.is_alive(), "invalid flood did not stop"
            assert 401 in statuses and 429 in statuses, "invalid grant quota was not exercised"
            assert all(code in (401,429,None) for code in statuses), "unexpected invalid grant response"
            responded = sum(code is not None for code in statuses)
            # Exhaust grant one independently while grant two keeps reporting.
            second = heartbeat(get("/metrics"), "two")
            statuses = []
            stop = threading.Event()
            grant_flood = threading.Thread(target=flood, args=(tokens[0], statuses, stop), daemon=True)
            grant_flood.start()
            wait_for(lambda: heartbeat(get("/metrics"), "two") > second, seconds=8)
            wait_for(lambda: 429 in statuses, seconds=20)
            stop.set()
            grant_flood.join(timeout=5)
            assert not grant_flood.is_alive(), "grant flood did not stop"
            assert 429 in statuses, "grant quota was not exercised"
            assert all(code in (400,429,None) for code in statuses), "unexpected valid grant response"
            responded += sum(code is not None for code in statuses)
            assert counter(get("/metrics"), "relay_rejected_total") >= rejected_before + responded
            for thread in tricklers:
                thread.join(timeout=6)
                assert not thread.is_alive()
            assert len(closed_after) == 2 and all(elapsed < 5 for elapsed in closed_after)
            for sock in slow:
                sock.settimeout(5)
                received = bytearray()
                while True:
                    try:
                        chunk = sock.recv(1024)
                    except ConnectionResetError:
                        break
                    if not chunk:
                        break
                    received.extend(chunk)
                    assert len(received) <= 4096
            # Saturate the complete bounded worker pool with slow connections.
            # Shedding is permitted, but all slots must recover and both real
            # senders must resume once the absolute deadline expires.
            for sock in slow:
                sock.close()
            slow.clear()
            previous = (heartbeat(get("/metrics"), "one"), heartbeat(get("/metrics"), "two"))
            for _ in range(16):
                sock = socket.create_connection(("127.0.0.1", port), timeout=1)
                sock.sendall(b"GET /health HTTP/1.1\r\nHost: local\r\nX-Slow:")
                slow.append(sock)
            time.sleep(3.5)
            for sock in slow:
                sock.settimeout(5)
                try:
                    assert sock.recv(1024) == b""
                except ConnectionResetError:
                    pass
            wait_for(lambda: get("/ready"), seconds=8)
            wait_for(lambda: heartbeat(get("/metrics"), "one") > previous[0], seconds=8)
            wait_for(lambda: heartbeat(get("/metrics"), "two") > previous[1], seconds=8)
            print("two-grant quota isolation, expected-since, slow-client deadline and pool recovery: passed")
        finally:
            for sock in slow:
                sock.close()
            for sender in senders:
                if sender.poll() is None:
                    sender.terminate()
            for sender in senders:
                if sender.poll() is None:
                    sender.wait(timeout=10)
            relay.terminate()
            relay.wait(timeout=5)


if __name__ == "__main__":
    main()
    isolation()
