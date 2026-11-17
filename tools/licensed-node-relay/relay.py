#!/usr/bin/env python3
"""Loopback-only dogfood telemetry relay. See README.md for the contract."""
import hashlib
import collections
import hmac
import http.client
import json
import os
import re
import sqlite3
import stat
import socket
import socketserver
import threading
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

MAX_BODY = 1024
MAX_IDENTITIES = 128
MAX_SKEW = 120
MAX_WORKERS = 16
REQUEST_DEADLINE = 3.0
# Bound stdlib header parsing before authentication, across all workers.
http.client._MAXLINE = 8192
http.client._MAXHEADERS = 32
LABEL = re.compile(r"^[a-zA-Z0-9_-]{1,48}$")
DIGEST = re.compile(r"^[0-9a-f]{64}$")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def load_identities(path):
    # Read a bounded, private operator registry afresh on each use. Atomic
    # replacement is supported; a systemd credential snapshot needs a restart.
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    with os.fdopen(fd, "rb") as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or st.st_mode & 0o077:
            raise ValueError("grant registry must be service-owned and private")
        raw = f.read(65537)
    if len(raw) > 65536:
        raise ValueError("grant registry too large")
    data = json.loads(raw, object_pairs_hook=unique_object)
    if not isinstance(data, list) or len(data) > MAX_IDENTITIES:
        raise ValueError("invalid grant count")
    identities = []
    labels = set()
    hashes = set()
    for item in data:
        if not isinstance(item, dict) or set(item) != {"tenant", "node", "token_sha256", "scope", "not_before", "expires_at", "revoked"}:
            raise ValueError("invalid grant")
        tenant, node, digest = item["tenant"], item["node"], item["token_sha256"]
        if not all(isinstance(v, str) for v in (tenant, node, digest)) or not LABEL.fullmatch(tenant) or not LABEL.fullmatch(node) or not DIGEST.fullmatch(digest):
            raise ValueError("invalid grant values")
        if item["scope"] != "telemetry:write" or type(item["not_before"]) is not int or type(item["expires_at"]) is not int or not 0 <= item["not_before"] < item["expires_at"] <= 2**63-1 or type(item["revoked"]) is not bool:
            raise ValueError("invalid grant policy")
        if (tenant, node) in labels or digest in hashes:
            raise ValueError("duplicate grant")
        labels.add((tenant, node))
        hashes.add(digest)
        identities.append((tenant, node, digest, item["not_before"], item["expires_at"], item["revoked"]))
    return identities


class RegistryError(Exception):
    """Operator registry is unavailable or malformed; fail closed as 503."""


def active_grants(path, now):
    try:
        grants = load_identities(path)
    except (OSError, ValueError, UnicodeError, RecursionError) as error:
        raise RegistryError from error
    return [(tenant, node, digest, start) for tenant, node, digest, start, end, revoked in grants if not revoked and start <= now < end]


class Relay(socketserver.ThreadingMixIn, HTTPServer):
    daemon_threads = True
    request_queue_size = 32
    def __init__(self, state, registry, port):
        self.registry = registry
        self.workers = threading.BoundedSemaphore(MAX_WORKERS)
        self.db_lock = threading.RLock()
        self.quota_lock = threading.Lock()
        self.quotas = collections.OrderedDict()
        os.umask(0o077)
        self.db = sqlite3.connect(state, check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS node(tenant TEXT NOT NULL, node TEXT NOT NULL,
            seq INTEGER NOT NULL, digest TEXT NOT NULL, heartbeat REAL, goodbye REAL,
            PRIMARY KEY(tenant,node));
          CREATE TABLE IF NOT EXISTS counters(kind TEXT PRIMARY KEY, n INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS ready_check(id INTEGER PRIMARY KEY, n INTEGER NOT NULL);
          INSERT OR IGNORE INTO ready_check VALUES(1,0);
          CREATE TABLE IF NOT EXISTS wall_clock(id INTEGER PRIMARY KEY, latest REAL NOT NULL);
          INSERT OR IGNORE INTO wall_clock VALUES(1,0);
        """)
        self.db.commit()
        super().__init__(("127.0.0.1", port), Handler)

    def checked_time(self, commit=False):
        now = time.time()
        latest = self.db.execute("SELECT latest FROM wall_clock WHERE id=1").fetchone()[0]
        if now < latest:
            raise RuntimeError("wall clock regressed")
        self.db.execute("UPDATE wall_clock SET latest=? WHERE id=1", (now,))
        if commit:
            self.db.commit()
        return now

    def reject(self):
        with self.db_lock:
            self.count("rejected")
            self.db.commit()

    def count(self, kind):
        self.db.execute("INSERT INTO counters VALUES(?,1) ON CONFLICT(kind) DO UPDATE SET n=n+1", (kind,))

    def allow(self, identity):
        # Invalid traffic has its own bucket; valid identities cannot consume
        # each other's quota. Keys come from validated, bounded registry labels.
        key = identity if identity is not None else ("", "")
        with self.quota_lock:
            now = time.monotonic()
            tokens, last = self.quotas.pop(key, (20.0, now))
            tokens = min(20.0, tokens + (now - last) * 5.0)
            allowed = tokens >= 1
            if allowed:
                tokens -= 1
            self.quotas[key] = (tokens, now)
            while len(self.quotas) > MAX_IDENTITIES + 1:
                self.quotas.popitem(last=False)
            return allowed

    def process_request(self, request, client_address):
        # Connections outside this bounded pool are shed before HTTP admission.
        if not self.workers.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.workers.release()
            raise

    def process_request_thread(self, request, client_address):
        def expire():
            try:
                request.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        timer = threading.Timer(REQUEST_DEADLINE, expire)
        timer.daemon = True
        timer.start()
        try:
            super().process_request_thread(request, client_address)
        finally:
            timer.cancel()
            self.workers.release()

    def handle_error(self, request, client_address):
        # No request or credential data enters logs on disconnect or timeout.
        pass

    def get_request(self):
        conn, addr = super().get_request()
        conn.settimeout(REQUEST_DEADLINE)
        return conn, addr


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def setup(self):
        super().setup()
        self.deadline = time.monotonic() + REQUEST_DEADLINE

    def log_message(self, *args):
        pass  # Never log paths, request bodies, or credentials.

    def reply(self, status, body=b"", content_type="text/plain; charset=utf-8"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_GET(self):
        if self.path == "/health":
            return self.reply(200, b"ok\n")
        if self.path not in ("/ready", "/metrics"):
            return self.reply(404)
        try:
            with self.server.db_lock, self.server.db:
                now = self.server.checked_time(commit=True)
                grants = active_grants(self.server.registry, now)
                if self.path == "/ready":
                    if not grants:
                        status, body = 503, b"not ready\n"
                    else:
                        self.server.db.execute("BEGIN IMMEDIATE")
                        self.server.db.execute("UPDATE ready_check SET n = 1 - n WHERE id = 1")
                        self.server.db.commit()
                        status, body = 200, b"ready\n"
                else:
                    rows = {(a,b):(c,d) for a,b,c,d in self.server.db.execute("SELECT tenant,node,heartbeat,goodbye FROM node")}
                    counters = dict(self.server.db.execute("SELECT kind,n FROM counters"))
                    out = ["# TYPE nudox_node_heartbeat_seconds gauge", "# TYPE nudox_node_goodbye_seconds gauge", "# TYPE nudox_node_telemetry_grant_expected_since_seconds gauge"]
                    for tenant, node, _, start in grants:
                        hb, bye = rows.get((tenant,node), (None,None))
                        labels = f'tenant="{tenant}",node="{node}"'
                        out.append(f"nudox_node_telemetry_grant_expected_since_seconds{{{labels}}} {start}")
                        if hb is not None: out.append(f"nudox_node_heartbeat_seconds{{{labels}}} {hb!r}")
                        if bye is not None: out.append(f"nudox_node_goodbye_seconds{{{labels}}} {bye!r}")
                    out.append("# TYPE relay_records_total counter")
                    for kind in ("heartbeat", "goodbye", "diag"):
                        out.append(f'relay_records_total{{kind="{kind}"}} {counters.get(kind,0)}')
                    out += ["# TYPE relay_rejected_total counter", f"relay_rejected_total {counters.get('rejected',0)}", "# TYPE relay_node_reported_dropped_total counter", f"relay_node_reported_dropped_total {counters.get('reported_dropped',0)}"]
                    status, body = 200, ("\n".join(out)+"\n").encode()
            return self.reply(status, body)
        except (sqlite3.Error, OSError, ValueError, UnicodeError, RuntimeError, RecursionError, RegistryError):
            with self.server.db_lock:
                self.server.db.rollback()
            return self.reply(503)

    def do_POST(self):
        if self.path != "/v1/records":
            return self.reply(404)
        try:
            authorization = self.headers.get("Authorization", "")
            token = authorization.removeprefix("Bearer ") if authorization.startswith("Bearer ") else ""
            digest = hashlib.sha256(token.encode()).hexdigest() if token and len(token) <= 256 and token.isascii() else ""
            # Initial lookup is deliberately outside the database lock so an
            # invalid local client cannot consume any valid grant's bucket.
            grants = active_grants(self.server.registry, time.time())
            identity = next(((t,n) for t,n,h,_ in grants if digest and hmac.compare_digest(digest,h)), None)
            if not self.server.allow(identity):
                self.server.reject()
                return self.reply(429)
            if identity is None:
                self.server.reject()
                return self.reply(401)
            if self.headers.get("Transfer-Encoding") is not None:
                self.server.reject()
                return self.reply(400)
            raw_length = self.headers.get("Content-Length", "")
            if not raw_length.isdecimal() or not 1 <= int(raw_length) <= MAX_BODY:
                self.server.reject()
                return self.reply(413)
            raw = self.rfile.read(int(raw_length))
            if len(raw) != int(raw_length):
                self.server.reject()
                return self.reply(400)
            record = json.loads(raw, object_pairs_hook=unique_object)
            if not isinstance(record,dict) or record.get("kind") not in ("heartbeat","goodbye","diag") or set(record) != ({"seq","kind","created_at","dropped"} if record.get("kind") == "diag" else {"seq","kind","created_at"}) or type(record["seq"]) is not int or not 1 <= record["seq"] <= 2**63-1 or type(record["created_at"]) is not int or (record["kind"] == "diag" and (type(record["dropped"]) is not int or not 1 <= record["dropped"] <= 2**31-1)):
                raise ValueError("schema")
            fingerprint = hashlib.sha256(raw).hexdigest()
            tenant,node = identity
            with self.server.db_lock, self.server.db:
                db = self.server.db
                db.execute("BEGIN IMMEDIATE")
                prior = db.execute("SELECT seq,digest,heartbeat,goodbye FROM node WHERE tenant=? AND node=?", identity).fetchone()
                received = self.server.checked_time()
                still_allowed = any(t == tenant and n == node and hmac.compare_digest(digest,h)
                                    for t,n,h,_ in active_grants(self.server.registry, received))
                if time.monotonic() >= self.deadline:
                    db.rollback()
                    status, body = 503, b""
                elif not still_allowed:
                    db.rollback()
                    self.server.reject()
                    status, body = 401, b""
                elif prior and record["seq"] == prior[0] and hmac.compare_digest(fingerprint, prior[1]):
                    db.rollback()
                    status, body = 200, b"duplicate\n"
                elif (prior and record["seq"] <= prior[0]) or abs(received - record["created_at"]) > MAX_SKEW:
                    db.rollback()
                    self.server.reject()
                    status, body = 409, b""
                elif not prior and db.execute("SELECT COUNT(*) FROM node").fetchone()[0] >= MAX_IDENTITIES:
                    db.rollback()
                    self.server.reject()
                    status, body = 429, b""
                elif prior and any(value is not None and received <= value for value in prior[2:]):
                    db.rollback()
                    status, body = 503, b""
                else:
                    db.execute("INSERT INTO node(tenant,node,seq,digest) VALUES(?,?,?,?) ON CONFLICT(tenant,node) DO UPDATE SET seq=excluded.seq,digest=excluded.digest", (tenant,node,record["seq"],fingerprint))
                    if record["kind"] in ("heartbeat", "goodbye"):
                        db.execute(f"UPDATE node SET {record['kind']}=? WHERE tenant=? AND node=?", (received,tenant,node))
                    self.server.count(record["kind"])
                    if record["kind"] == "diag":
                        db.execute("INSERT INTO counters VALUES('reported_dropped',?) ON CONFLICT(kind) DO UPDATE SET n=n+excluded.n", (record["dropped"],))
                    db.commit()
                    status, body = 202, b"accepted\n"
            return self.reply(status, body)
        except (ValueError, TypeError, KeyError, json.JSONDecodeError, UnicodeError, RecursionError):
            with self.server.db_lock:
                self.server.db.rollback()
            self.server.reject()
            return self.reply(400)
        except (sqlite3.Error, OSError, RuntimeError, RegistryError):
            with self.server.db_lock:
                self.server.db.rollback()
            return self.reply(503)


def main():
    if len(sys.argv) != 4:
        sys.exit("usage: relay.py IDENTITIES_JSON STATE_DB LOOPBACK_PORT")
    load_identities(sys.argv[1])
    parent = Path(sys.argv[2]).resolve().parent
    if os.stat(parent).st_uid != os.geteuid() or os.stat(parent).st_mode & 0o077:
        sys.exit("state directory must be service-owned and private")
    server = Relay(sys.argv[2], sys.argv[1], int(sys.argv[3]))
    server.serve_forever()

if __name__ == "__main__":
    main()
