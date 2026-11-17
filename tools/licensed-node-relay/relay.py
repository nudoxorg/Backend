#!/usr/bin/env python3
"""Loopback-only dogfood telemetry relay. See README.md for the contract."""
import hashlib
import hmac
import json
import os
import re
import sqlite3
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

MAX_BODY = 1024
MAX_IDENTITIES = 128
MAX_SKEW = 120
LABEL = re.compile(r"^[a-zA-Z0-9_-]{1,48}$")
DIGEST = re.compile(r"^[0-9a-f]{64}$")


def private_file(path):
    st = os.stat(path)
    if st.st_uid != os.geteuid() or st.st_mode & 0o077:
        raise ValueError("identity file must be owned by the service user and private")


def load_identities(path):
    private_file(path)
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, list) or not 1 <= len(data) <= MAX_IDENTITIES:
        raise ValueError("invalid identity count")
    identities = []
    labels = set()
    hashes = set()
    for item in data:
        if not isinstance(item, dict) or set(item) != {"tenant", "node", "token_sha256"}:
            raise ValueError("invalid identity")
        tenant, node, digest = item["tenant"], item["node"], item["token_sha256"]
        if not all(isinstance(v, str) for v in (tenant, node, digest)) or not LABEL.fullmatch(tenant) or not LABEL.fullmatch(node) or not DIGEST.fullmatch(digest):
            raise ValueError("invalid identity values")
        if (tenant, node) in labels or digest in hashes:
            raise ValueError("duplicate identity")
        labels.add((tenant, node))
        hashes.add(digest)
        identities.append((tenant, node, digest))
    return identities


class Relay(HTTPServer):
    def __init__(self, state, identities, port):
        self.identities = identities
        self.tokens = 20.0
        self.last = time.monotonic()
        os.umask(0o077)
        self.db = sqlite3.connect(state)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS node(tenant TEXT NOT NULL, node TEXT NOT NULL,
            seq INTEGER NOT NULL, digest TEXT NOT NULL, heartbeat REAL, goodbye REAL,
            PRIMARY KEY(tenant,node));
          CREATE TABLE IF NOT EXISTS counters(kind TEXT PRIMARY KEY, n INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS ready_check(id INTEGER PRIMARY KEY, n INTEGER NOT NULL);
          INSERT OR IGNORE INTO ready_check VALUES(1,0);
        """)
        self.db.commit()
        super().__init__(("127.0.0.1", port), Handler)

    def reject(self):
        self.count("rejected")
        self.db.commit()

    def count(self, kind):
        self.db.execute("INSERT INTO counters VALUES(?,1) ON CONFLICT(kind) DO UPDATE SET n=n+1", (kind,))

    def allow(self):
        now = time.monotonic()
        self.tokens = min(20.0, self.tokens + (now - self.last) * 5)
        self.last = now
        if self.tokens < 1:
            return False
        self.tokens -= 1
        return True

    def get_request(self):
        conn, addr = super().get_request()
        conn.settimeout(5)
        return conn, addr


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

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
        if self.path == "/ready":
            try:
                self.server.db.execute("BEGIN IMMEDIATE")
                self.server.db.execute("UPDATE ready_check SET n = 1 - n WHERE id = 1")
                self.server.db.commit()
                return self.reply(200, b"ready\n")
            except sqlite3.Error:
                self.server.db.rollback()
                return self.reply(503, b"not ready\n")
        if self.path != "/metrics":
            return self.reply(404)
        try:
            rows = {(a,b):(c,d) for a,b,c,d in self.server.db.execute("SELECT tenant,node,heartbeat,goodbye FROM node")}
            counters = dict(self.server.db.execute("SELECT kind,n FROM counters"))
            out = ["# TYPE nudox_node_heartbeat_seconds gauge", "# TYPE nudox_node_goodbye_seconds gauge"]
            for tenant, node, _ in self.server.identities:
                hb, bye = rows.get((tenant,node), (None,None))
                labels = f'tenant="{tenant}",node="{node}"'
                if hb is not None: out.append(f"nudox_node_heartbeat_seconds{{{labels}}} {hb:.6f}")
                if bye is not None: out.append(f"nudox_node_goodbye_seconds{{{labels}}} {bye:.6f}")
            out.append("# TYPE relay_records_total counter")
            for kind in ("heartbeat", "goodbye", "diag"):
                out.append(f'relay_records_total{{kind="{kind}"}} {counters.get(kind,0)}')
            out += ["# TYPE relay_rejected_total counter", f"relay_rejected_total {counters.get('rejected',0)}", "# TYPE relay_node_reported_dropped_total counter", f"relay_node_reported_dropped_total {counters.get('reported_dropped',0)}"]
            return self.reply(200, ("\n".join(out)+"\n").encode())
        except sqlite3.Error:
            return self.reply(503)

    def do_POST(self):
        if self.path != "/v1/records": return self.reply(404)
        try:
            if not self.server.allow():
                self.server.reject()
                return self.reply(429)
            authorization = self.headers.get("Authorization", "")
            token = authorization.removeprefix("Bearer ") if authorization.startswith("Bearer ") else ""
            if not token or len(token) > 256 or not token.isascii():
                self.server.reject()
                return self.reply(401)
            digest = hashlib.sha256(token.encode()).hexdigest()
            identity = next(((t,n) for t,n,h in self.server.identities if hmac.compare_digest(digest,h)), None)
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
            record = json.loads(raw)
            if not isinstance(record,dict) or record.get("kind") not in ("heartbeat","goodbye","diag") or set(record) != ({"seq","kind","created_at","dropped"} if record.get("kind") == "diag" else {"seq","kind","created_at"}) or type(record["seq"]) is not int or not 1 <= record["seq"] <= 2**63-1 or type(record["created_at"]) is not int or (record["kind"] == "diag" and (type(record["dropped"]) is not int or not 1 <= record["dropped"] <= 2**31-1)):
                raise ValueError("schema")
            fingerprint = hashlib.sha256(raw).hexdigest()
            tenant,node = identity
            db = self.server.db
            db.execute("BEGIN IMMEDIATE")
            prior = db.execute("SELECT seq,digest FROM node WHERE tenant=? AND node=?", identity).fetchone()
            if prior and record["seq"] == prior[0] and hmac.compare_digest(fingerprint, prior[1]):
                db.rollback()
                return self.reply(200, b"duplicate\n")
            if (prior and record["seq"] <= prior[0]) or abs(time.time() - record["created_at"]) > MAX_SKEW:
                db.rollback()
                self.server.reject()
                return self.reply(409)
            received = time.time()
            db.execute("INSERT INTO node(tenant,node,seq,digest) VALUES(?,?,?,?) ON CONFLICT(tenant,node) DO UPDATE SET seq=excluded.seq,digest=excluded.digest", (tenant,node,record["seq"],fingerprint))
            if record["kind"] in ("heartbeat", "goodbye"):
                column = record["kind"]
                db.execute(f"UPDATE node SET {column}=? WHERE tenant=? AND node=?", (received,tenant,node))
            self.server.count(record["kind"])
            if record["kind"] == "diag":
                db.execute("INSERT INTO counters VALUES('reported_dropped',?) ON CONFLICT(kind) DO UPDATE SET n=n+excluded.n", (record["dropped"],))
            db.commit()
            return self.reply(202, b"accepted\n")
        except (ValueError, TypeError, KeyError, json.JSONDecodeError, UnicodeError):
            self.server.db.rollback()
            self.server.reject()
            return self.reply(400)
        except sqlite3.Error:
            self.server.db.rollback()
            return self.reply(503)


def main():
    if len(sys.argv) != 4:
        sys.exit("usage: relay.py IDENTITIES_JSON STATE_DB LOOPBACK_PORT")
    identities = load_identities(sys.argv[1])
    parent = Path(sys.argv[2]).resolve().parent
    if os.stat(parent).st_uid != os.geteuid() or os.stat(parent).st_mode & 0o077:
        sys.exit("state directory must be service-owned and private")
    server = Relay(sys.argv[2], identities, int(sys.argv[3]))
    server.serve_forever()

if __name__ == "__main__":
    main()
