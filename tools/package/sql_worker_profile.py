#!/usr/bin/env python3
"""Time the SQLite worker on realistic SQL files, outside the owner.

    sql_worker_profile.py WORKER [--repeat N]

Generates migration-shaped SQL files of several sizes, sends each to the
worker the way the owner does (one request on stdin, one response on stdout),
and prints per-file wall time, CPU time (user + system, from the kernel) and
response size. It also times a plain SHA-256 read of the worker file, which is
the lower bound of what a checked launch spends admitting the executable.

Development mode: nothing here touches the owner, a database or the network.
"""
import argparse, hashlib, json, os, resource, statistics, subprocess, sys, time

p = argparse.ArgumentParser()
p.add_argument("worker")
p.add_argument("--repeat", type=int, default=5)
a = p.parse_args()
worker = os.path.abspath(a.worker)


def run(source: bytes, basis):
    request = json.dumps({"version": 4, "expected_basis": basis, "source": list(source),
                          "settings": "ProcessDefaults", "context": None}).encode()
    t0 = time.perf_counter()
    proc = subprocess.Popen([worker], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    out, err = proc.communicate(request)
    wall = time.perf_counter() - t0
    # communicate() has reaped the child; its usage is in RUSAGE_CHILDREN deltas.
    return wall, out, err


def cpu():
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime + usage.ru_stime


def table(index: int) -> str:
    return (
        f"CREATE TABLE t{index} (\n  id INTEGER PRIMARY KEY,\n  parent_id INTEGER REFERENCES t{max(index - 1, 0)}(id),\n"
        f"  name TEXT NOT NULL,\n  email TEXT UNIQUE,\n  balance INTEGER NOT NULL DEFAULT 0,\n  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\n);\n"
        f"CREATE INDEX t{index}_by_name ON t{index}(name);\n"
        f"CREATE VIEW v{index} AS SELECT a.id, a.name, SUM(b.balance) AS total FROM t{index} a JOIN t{max(index - 1, 0)} b ON b.id = a.parent_id GROUP BY a.id, a.name;\n"
        f"SELECT name, balance FROM t{index} WHERE id = ?1;\n"
        f"UPDATE t{index} SET balance = balance - ?2 WHERE id = ?1;\n"
        f"INSERT INTO t{index} (name, email) VALUES (?1, ?2);\n"
    )


def sized(target: int) -> bytes:
    out, i = "", 0
    while len(out) < target:
        out += table(i)
        i += 1
    return out.encode()


# 1. A probe: the worker reports the basis it will accept.
_, out, err = run(b"SELECT 1", None)
try:
    basis = json.loads(out)["basis"]
except Exception:
    sys.exit(f"worker gave no basis: {out[:300]!r} {err[:300]!r}")

size = os.path.getsize(worker)
digests = []
for _ in range(a.repeat):
    t0 = time.perf_counter()
    h = hashlib.sha256()
    with open(worker, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    digests.append(time.perf_counter() - t0)
print(f"worker {worker} {size / 1e6:.1f} MB; sha256 read: median {statistics.median(digests) * 1000:.0f} ms")
print(f"load average: {' '.join(f'{x:.1f}' for x in os.getloadavg())}")
print(f"{'file':>10} {'wall ms (median)':>17} {'wall max':>9} {'cpu ms':>8} {'resp KB':>8}  parsed")
for label, target in (("tiny", 200), ("2 KB", 2_000), ("20 KB", 20_000), ("100 KB", 100_000), ("250 KB", 250_000), ("500 KB", 500_000)):
    source = sized(target)
    walls, cpus = [], []
    parsed = None
    for _ in range(a.repeat):
        before = cpu()
        wall, out, err = run(source, basis)
        cpus.append(cpu() - before)
        walls.append(wall)
        try:
            parsed = "Parsed" in json.dumps(json.loads(out).get("syntax"))
        except Exception:
            parsed = False
    print(f"{label:>10} {statistics.median(walls) * 1000:>17.0f} {max(walls) * 1000:>9.0f} {statistics.median(cpus) * 1000:>8.0f} {len(out) / 1024:>8.0f}  {parsed} ({len(source) / 1024:.0f} KB source)")
