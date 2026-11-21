#!/usr/bin/env python3
"""Report observed local owner lifecycle to the dogfood relay."""
import fcntl
import http.client
import json
import os
import re
from pathlib import Path
import signal
import sqlite3
import stat
import subprocess
import sys
import time
from urllib.parse import urlsplit

CAPACITY = 256
MAX_AGE = 110


def main():
    args = sys.argv[1:]
    if "--" not in args:
        sys.exit("usage: sender.py URL TOKEN_FILE STATE_DIR --grant-activation HEX [--interval-seconds N] [--spool-capacity N] [--observe-unit UNIT --systemctl ABSOLUTE_PATH] [--signal-mode forward|cgroup] [--drain-only true] -- [COMMAND [ARGS...]]")
    separator = args.index("--")
    options, command = args[:separator], args[separator + 1:]
    if len(options) < 3 or (len(options) - 3) % 2:
        sys.exit("invalid sender arguments")
    url_arg, token_path, state_path = options[:3]
    interval, capacity, signal_mode, drain_only = 60.0, CAPACITY, "forward", False
    observe_unit, systemctl, grant_activation = None, None, None
    seen = set()
    for name, value in zip(options[3::2], options[4::2]):
        if name in seen:
            sys.exit("duplicate sender option")
        seen.add(name)
        try:
            if name == "--interval-seconds": interval = float(value)
            elif name == "--spool-capacity": capacity = int(value)
            elif name == "--signal-mode": signal_mode = value
            elif name == "--observe-unit": observe_unit = value
            elif name == "--grant-activation": grant_activation = value
            elif name == "--systemctl": systemctl = value
            elif name == "--drain-only" and value == "true": drain_only = True
            else: sys.exit("unknown sender option")
        except ValueError:
            sys.exit("invalid sender option")
    if not 1.0 <= interval <= 60.0 or not 1 <= capacity <= CAPACITY or signal_mode not in ("forward", "cgroup") or grant_activation is None or not re.fullmatch(r"[0-9a-f]{32}", grant_activation) or (drain_only and command) or (observe_unit is not None and (command or systemctl is None)) or (observe_unit is None and not drain_only and not command) or (systemctl is not None and (observe_unit is None or not os.path.isabs(systemctl))) or (observe_unit is not None and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.@-]{0,99}\.service", observe_unit)):
        sys.exit("sender options out of bounds")
    url = urlsplit(url_arg)
    if url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port or url.path not in ("", "/") or url.username or url.password or url.query or url.fragment:
        sys.exit("relay must be an explicit loopback HTTP URL")
    fd = os.open(token_path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(fd, "rb") as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or st.st_mode & 0o077:
            sys.exit("token must be service-owned and private")
        raw_token = f.read(258)
    if len(raw_token) > 257:
        sys.exit("invalid token format")
    try:
        token = raw_token.decode("ascii").strip()
    except UnicodeError:
        sys.exit("invalid token format")
    if not 32 <= len(token) <= 256 or not token.isascii() or not token.isalnum():
        sys.exit("invalid token format")
    state = Path(state_path).resolve()
    st = state.stat()
    if st.st_uid != os.geteuid() or st.st_mode & 0o077:
        sys.exit("state directory must be service-owned and private")
    os.umask(0o077)
    lock = open(state / "sender.lock", "a+")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        # A running sender already drains the same spool.
        return 75
    db = sqlite3.connect(state / "sender.sqlite")
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.executescript("""
      CREATE TABLE IF NOT EXISTS meta(id INTEGER PRIMARY KEY CHECK(id=1), seq INTEGER NOT NULL, lost INTEGER NOT NULL);
      INSERT OR IGNORE INTO meta VALUES(1,0,0);
      CREATE TABLE IF NOT EXISTS queue(seq INTEGER PRIMARY KEY, created INTEGER NOT NULL, body BLOB NOT NULL, generation TEXT NOT NULL DEFAULT '');
      CREATE TABLE IF NOT EXISTS observed(id INTEGER PRIMARY KEY CHECK(id=1), generation TEXT NOT NULL, ended INTEGER NOT NULL);
      INSERT OR IGNORE INTO observed VALUES(1,'',1);
    """)
    if "generation" not in {row[1] for row in db.execute("PRAGMA table_info(queue)")}:
        db.execute("ALTER TABLE queue ADD COLUMN generation TEXT NOT NULL DEFAULT ''")
    db.commit()

    if observe_unit is not None:
        boot = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
        if not re.fullmatch(r"[0-9a-f-]{36}", boot):
            sys.exit("invalid boot identity")
        started_at = int(time.monotonic() * 1_000_000)
        def inspect_owner():
            try:
                result = subprocess.run([systemctl, "show", observe_unit,
                    "--property=Id,LoadState,ActiveState,MainPID,ExecMainPID,ExecMainStartTimestampMonotonic,ExecMainCode,ExecMainStatus,Result"],
                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=2, check=True)
                if len(result.stdout) > 8192:
                    return None
                props = {}
                for line in result.stdout.decode("ascii").splitlines():
                    key, sep, value = line.partition("=")
                    if not sep or key in props:
                        return None
                    props[key] = value
                required = {"Id", "LoadState", "ActiveState", "MainPID", "ExecMainPID", "ExecMainStartTimestampMonotonic", "ExecMainCode", "ExecMainStatus", "Result"}
                if set(props) != required or props["Id"] != observe_unit or props["LoadState"] != "loaded":
                    return None
                main, pid = int(props["MainPID"]), int(props["ExecMainPID"])
                start = int(props["ExecMainStartTimestampMonotonic"])
                code, status = int(props["ExecMainCode"]), int(props["ExecMainStatus"])
                if start <= 0 or pid <= 1:
                    return None
                generation = f"{boot}:{start}:{pid}"
                active = props["ActiveState"] in ("active", "reloading") and main == pid
                terminal = props["ActiveState"] in ("inactive", "failed")
                clean = terminal and props["Result"] == "success" and code == 1 and status == 0
                return generation, start, active, terminal, clean
            except (OSError, ValueError, UnicodeError, subprocess.SubprocessError):
                return None
    else:
        inspect_owner = None

    def enqueue(kind, dropped=None, terminal_generation=None, generation=""):
        with db:
            if terminal_generation is not None:
                row = db.execute("SELECT generation,ended FROM observed WHERE id=1").fetchone()
                if row != (terminal_generation, 0):
                    return False
            # Reserve one bounded slot for orderly retirement. If an older
            # unacknowledged goodbye exists, replace it with the latest real
            # retirement and durably count the abandoned older record.
            if kind == "goodbye":
                for old_seq, old_body in db.execute("SELECT seq,body FROM queue").fetchall():
                    if json.loads(old_body)["kind"] == "goodbye":
                        db.execute("DELETE FROM queue WHERE seq=?", (old_seq,))
                        db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")
            limit = capacity + 1 if kind == "goodbye" else capacity
            if db.execute("SELECT COUNT(*) FROM queue").fetchone()[0] >= limit:
                db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")
                if terminal_generation is not None:
                    db.execute("UPDATE observed SET ended=1 WHERE id=1")
                return False
            seq = db.execute("UPDATE meta SET seq=seq+1 WHERE id=1 RETURNING seq").fetchone()[0]
            now = int(time.time())
            value = {"seq":seq,"kind":kind,"created_at":now,"activation_id":grant_activation}
            if kind == "diag": value["dropped"] = dropped
            body = json.dumps(value, separators=(",", ":")).encode()
            db.execute("INSERT INTO queue(seq,created,body,generation) VALUES(?,?,?,?)", (seq,now,body,generation))
            if kind == "diag": db.execute("UPDATE meta SET lost=lost-? WHERE id=1", (dropped,))
            if terminal_generation is not None:
                db.execute("UPDATE observed SET ended=1 WHERE id=1")
        return True

    def flush():
        row = db.execute("SELECT seq,created,body,generation FROM queue ORDER BY seq LIMIT 1").fetchone()
        if not row:
            lost = db.execute("SELECT lost FROM meta WHERE id=1").fetchone()[0]
            if lost:
                enqueue("diag", min(lost, 2**31-1))
            return
        seq, created, body, generation = row
        record = json.loads(body)
        if time.time() - created > MAX_AGE or record.get("activation_id") != grant_activation:
            # Never relabel an earlier or unbound activation. Preserve actual
            # abandonment, including loss counted by an older diagnostic.
            lost = record.get("dropped", 1)
            with db:
                db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                db.execute("UPDATE meta SET lost=lost+? WHERE id=1", (lost,))
            return
        if record["kind"] in ("heartbeat", "goodbye") and (generation or observe_unit):
            # Observer-tagged lifecycle records require a fresh manager sample.
            # Never send them from an unconfigured drain or during uncertainty.
            if not generation:
                with db:
                    db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                    db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")
                return
            sample = inspect_owner() if inspect_owner is not None else None
            if sample is None:
                return
            actual_generation, _, active, terminal, clean = sample
            valid = actual_generation == generation and (active if record["kind"] == "heartbeat" else terminal and clean)
            if not valid:
                with db:
                    db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                    db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")
                return
        try:
            conn = http.client.HTTPConnection("127.0.0.1", url.port, timeout=2)
            conn.request("POST", "/v1/records", body, {"Authorization":"Bearer " + token,"Content-Type":"application/json"})
            response = conn.getresponse()
            response.read(1024)
            status = response.status
            conn.close()
            if status in (200,202):
                with db: db.execute("DELETE FROM queue WHERE seq=?", (seq,))
        except (OSError, http.client.HTTPException):
            pass

    def abandon_heartbeats():
        # A stopped sender cannot attest that the node remains alive. Preserve
        # actual abandoned-record counts instead of replaying stale liveness.
        with db:
            for seq, body in db.execute("SELECT seq,body FROM queue").fetchall():
                if json.loads(body)["kind"] == "heartbeat":
                    db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                    db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")

    if drain_only:
        abandon_heartbeats()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pending = db.execute("SELECT 1 FROM queue LIMIT 1").fetchone()
            lost = db.execute("SELECT lost FROM meta WHERE id=1").fetchone()[0]
            if not pending and not lost:
                db.close()
                return 0
            flush()
            time.sleep(0.1)
        db.close()
        return 75  # Durable work remains for the next timer run.

    if observe_unit is not None:
        # This process is a separate systemd service, never the owner's parent
        # or MainPID. Telemetry failures cannot terminate or prevent the owner.
        stopping = False
        def stop_observer(_signum, _frame):
            nonlocal stopping
            stopping = True
        signal.signal(signal.SIGTERM, stop_observer)
        signal.signal(signal.SIGINT, stop_observer)
        # An old queued heartbeat cannot establish current owner liveness.
        abandon_heartbeats()
        next_check = next_heartbeat = 0.0
        observed_active = False
        while not stopping:
            now = time.monotonic()
            if now >= next_check:
                next_check = now + 1.0
                sample = inspect_owner()
                if sample is None:
                    observed_active = False
                    abandon_heartbeats()
                else:
                    generation, start, active, terminal, clean = sample
                    prior, ended = db.execute("SELECT generation,ended FROM observed WHERE id=1").fetchone()
                    if active:
                        if generation != prior or ended:
                            # An old queued goodbye cannot describe a newly
                            # started owner, even if the old exit was clean.
                            with db:
                                for seq, body, old_generation in db.execute("SELECT seq,body,generation FROM queue").fetchall():
                                    if json.loads(body)["kind"] in ("heartbeat", "goodbye") and old_generation != generation:
                                        db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                                        db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")
                            with db:
                                db.execute("UPDATE observed SET generation=?,ended=0 WHERE id=1", (generation,))
                            next_heartbeat = 0.0
                        observed_active = True
                        if now >= next_heartbeat:
                            enqueue("heartbeat", generation=generation)
                            next_heartbeat = now + interval
                    elif terminal:
                        observed_active = False
                        abandon_heartbeats()
                        # A never-observed old unit status is not a fresh
                        # goodbye. A generation begun during this observer's
                        # lifetime is also direct manager evidence.
                        if generation != prior and start >= started_at:
                            with db:
                                db.execute("UPDATE observed SET generation=?,ended=0 WHERE id=1", (generation,))
                            prior, ended = generation, 0
                        if generation == prior and not ended:
                            if clean:
                                enqueue("goodbye", terminal_generation=generation, generation=generation)
                            else:
                                with db:
                                    db.execute("UPDATE observed SET ended=1 WHERE id=1")
                    else:
                        observed_active = False
                        abandon_heartbeats()
            lost = db.execute("SELECT lost FROM meta WHERE id=1").fetchone()[0]
            queued = db.execute("SELECT COUNT(*) FROM queue").fetchone()[0]
            if lost and queued < capacity:
                enqueue("diag", min(lost, 2**31-1))
            # Never transmit a queued heartbeat without a recent active sample.
            row = db.execute("SELECT body FROM queue ORDER BY seq LIMIT 1").fetchone()
            if not row or json.loads(row[0])["kind"] != "heartbeat" or observed_active:
                flush()
            time.sleep(0.1)
        abandon_heartbeats()
        db.close()
        return 0

    child = subprocess.Popen(command)
    stopping = False
    def forward(signum, _frame):
        nonlocal stopping
        stopping = True
        if signal_mode == "forward":
            try: child.send_signal(signum)
            except ProcessLookupError: pass
    signal.signal(signal.SIGTERM, forward)
    signal.signal(signal.SIGINT, forward)
    next_heartbeat = 0.0
    while child.poll() is None:
        now = time.monotonic()
        lost = db.execute("SELECT lost FROM meta WHERE id=1").fetchone()[0]
        queued = db.execute("SELECT COUNT(*) FROM queue").fetchone()[0]
        if lost and queued < capacity:
            enqueue("diag", min(lost, 2**31-1))
        if not stopping and now >= next_heartbeat:
            enqueue("heartbeat")
            next_heartbeat = now + interval
        flush()
        time.sleep(0.1)
    status = child.returncode
    # Once the observed process ends, queued heartbeats must never be sent as
    # evidence that it is still alive. Count the records we abandon.
    abandon_heartbeats()
    if status == 0:
        enqueue("goodbye")
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and (db.execute("SELECT 1 FROM queue LIMIT 1").fetchone() or db.execute("SELECT lost FROM meta WHERE id=1").fetchone()[0]):
        flush()
        time.sleep(0.1)
    db.close()
    return status if status >= 0 else 128 - status

if __name__ == "__main__":
    sys.exit(main())
