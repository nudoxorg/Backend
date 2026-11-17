#!/usr/bin/env python3
"""Supervise a real node process and send its lifecycle to the dogfood relay."""
import fcntl
import http.client
import json
import os
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
        sys.exit("usage: sender.py URL TOKEN_FILE STATE_DIR [--interval-seconds N] [--spool-capacity N] [--signal-mode forward|cgroup] -- COMMAND [ARGS...]")
    separator = args.index("--")
    options, command = args[:separator], args[separator + 1:]
    if len(options) < 3 or not command or (len(options) - 3) % 2:
        sys.exit("invalid sender arguments")
    url_arg, token_path, state_path = options[:3]
    interval, capacity, signal_mode = 60.0, CAPACITY, "forward"
    seen = set()
    for name, value in zip(options[3::2], options[4::2]):
        if name in seen:
            sys.exit("duplicate sender option")
        seen.add(name)
        try:
            if name == "--interval-seconds": interval = float(value)
            elif name == "--spool-capacity": capacity = int(value)
            elif name == "--signal-mode": signal_mode = value
            else: sys.exit("unknown sender option")
        except ValueError:
            sys.exit("invalid sender option")
    if not 1.0 <= interval <= 60.0 or not 1 <= capacity <= CAPACITY or signal_mode not in ("forward", "cgroup"):
        sys.exit("sender options out of bounds")
    url = urlsplit(url_arg)
    if url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port or url.path not in ("", "/") or url.username or url.password or url.query or url.fragment:
        sys.exit("relay must be an explicit loopback HTTP URL")
    fd = os.open(token_path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
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
        sys.exit("sender already running")
    db = sqlite3.connect(state / "sender.sqlite")
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.executescript("""
      CREATE TABLE IF NOT EXISTS meta(id INTEGER PRIMARY KEY CHECK(id=1), seq INTEGER NOT NULL, lost INTEGER NOT NULL);
      INSERT OR IGNORE INTO meta VALUES(1,0,0);
      CREATE TABLE IF NOT EXISTS queue(seq INTEGER PRIMARY KEY, created INTEGER NOT NULL, body BLOB NOT NULL);
    """)
    db.commit()

    def enqueue(kind, dropped=None):
        with db:
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
                return False
            seq = db.execute("UPDATE meta SET seq=seq+1 WHERE id=1 RETURNING seq").fetchone()[0]
            now = int(time.time())
            value = {"seq":seq,"kind":kind,"created_at":now}
            if kind == "diag": value["dropped"] = dropped
            body = json.dumps(value, separators=(",", ":")).encode()
            db.execute("INSERT INTO queue VALUES(?,?,?)", (seq,now,body))
            if kind == "diag": db.execute("UPDATE meta SET lost=lost-? WHERE id=1", (dropped,))
        return True

    def flush():
        row = db.execute("SELECT seq,created,body FROM queue ORDER BY seq LIMIT 1").fetchone()
        if not row:
            lost = db.execute("SELECT lost FROM meta WHERE id=1").fetchone()[0]
            if lost:
                enqueue("diag", min(lost, 2**31-1))
            return
        seq, created, body = row
        if time.time() - created > MAX_AGE:
            # An unreceived record cannot be replayed as current liveness. Preserve
            # its actual loss, including any loss counted by an expired diagnostic.
            record = json.loads(body)
            lost = record.get("dropped", 1)
            with db:
                db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                db.execute("UPDATE meta SET lost=lost+? WHERE id=1", (lost,))
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
    with db:
        queued = db.execute("SELECT seq,body FROM queue").fetchall()
        for seq, body in queued:
            if json.loads(body)["kind"] == "heartbeat":
                db.execute("DELETE FROM queue WHERE seq=?", (seq,))
                db.execute("UPDATE meta SET lost=lost+1 WHERE id=1")
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
