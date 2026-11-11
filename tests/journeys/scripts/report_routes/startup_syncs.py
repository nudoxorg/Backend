#!/usr/bin/env python3
"""How many synced operations does each owner startup phase perform?

Starts a real `backend-locald` under `strace -f` in a private workspace, waits for its endpoint,
stops it, then buckets the traced calls by the phase windows the owner itself wrote to
`owner-startup-timing-v1.json`.

  startup_syncs.py --bin-dir DIR --out DIR [--timeout S]

Output: <out>/syncs.json (per-phase counts and the time spent inside sync calls) and
<out>/trace.txt. Counting only; nothing here changes behaviour.
"""
import argparse, json, os, pathlib, re, signal, subprocess, sys, time

TRACE = "fsync,fdatasync,syncfs,sync_file_range,mkdirat,mkdir,rename,renameat,renameat2,unlinkat,openat"
LINE = re.compile(r"^(\d+)\s+(\d+\.\d+)\s+(\w+)\((.*)\)\s+=\s+(-?\d+|\?)(?:.*?)(?:<(\d+\.\d+)>)?\s*$")


def run(bin_dir, out, timeout):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / f"startup-syncs-{os.getpid()}"
    state = work / "state"
    state.mkdir(parents=True, mode=0o700)
    secret = work / "authority.secret"
    secret.write_bytes(os.urandom(32))
    secret.chmod(0o600)
    sock = work / "owner.sock"
    trace = out / "trace.txt"
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(work / "home"),
           "XDG_RUNTIME_DIR": str(work / "run"), "TMPDIR": str(work / "tmp"),
           "BACKEND_LOCALD_AUTHORITY_SECRET_FILE": str(secret)}
    for key in ("HOME", "XDG_RUNTIME_DIR", "TMPDIR"):
        pathlib.Path(env[key]).mkdir(parents=True, exist_ok=True, mode=0o700)
    cmd = ["strace", "-f", "-ttt", "-T", "-y", "-e", f"trace={TRACE}", "-o", str(trace),
           str(pathlib.Path(bin_dir) / "backend-locald"), "--endpoint", str(sock), "--workspace", str(state),
           "--authority-secret-file", str(secret), "--registry-offline", "--registry-discovery-offline",
           "--advisory-offline", "--forge-offline"]
    started = time.time()
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=open(out / "owner-stderr.log", "wb"),
                            start_new_session=True)
    deadline = time.monotonic() + timeout
    while not sock.exists():
        if proc.poll() is not None:
            raise SystemExit(f"locald exited {proc.returncode} before listening")
        if time.monotonic() > deadline:
            os.killpg(proc.pid, signal.SIGKILL)
            raise SystemExit("locald did not listen in time")
        time.sleep(0.2)
    listening = time.time() - started
    time.sleep(1.0)
    os.killpg(proc.pid, signal.SIGTERM)
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
    timing = json.loads((state / "owner-startup-timing-v1.json").read_text())
    return trace, timing, listening


def analyse(trace, timing):
    t0 = timing["composition_started_unix_millis"] / 1000.0
    edges, cursor = [], 0.0
    for phase in timing["phases"]:
        micros = phase.get("elapsed_micros")
        if micros is None:
            continue
        edges.append((phase["phase"], t0 + cursor, t0 + cursor + micros / 1e6))
        cursor += micros / 1e6
    buckets = {name: {"seconds": round((end - start), 3), "fsync": 0, "fdatasync": 0, "mkdir": 0,
                      "rename": 0, "created_files": 0, "sync_seconds": 0.0, "directory_fsync": 0}
               for name, start, end in edges}
    buckets["outside-phases"] = {"seconds": 0, "fsync": 0, "fdatasync": 0, "mkdir": 0, "rename": 0,
                                 "created_files": 0, "sync_seconds": 0.0, "directory_fsync": 0}
    fdpath = re.compile(r"^\d+<(.*)>")
    for raw in pathlib.Path(trace).read_text(errors="replace").splitlines():
        m = LINE.match(raw)
        if not m:
            continue
        _pid, when, call, args, result, took = m.groups()
        when = float(when)
        if result.startswith("-"):
            continue
        name = next((n for n, s, e in edges if s <= when < e), "outside-phases")
        b = buckets[name]
        if call in ("fsync", "fdatasync"):
            b[call] += 1
            b["sync_seconds"] += float(took or 0)
            target = fdpath.match(args)
            if target and not os.path.splitext(target.group(1))[1] and "." not in os.path.basename(target.group(1)):
                b["directory_fsync"] += 1
        elif call in ("mkdir", "mkdirat"):
            b["mkdir"] += 1
        elif call.startswith("rename"):
            b["rename"] += 1
        elif call == "openat" and "O_CREAT" in args:
            b["created_files"] += 1
    for b in buckets.values():
        b["sync_seconds"] = round(b["sync_seconds"], 3)
    return buckets


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--timeout", type=float, default=900)
    args = ap.parse_args()
    trace, timing, listening = run(args.bin_dir, args.out, args.timeout)
    buckets = analyse(trace, timing)
    total = {k: sum(b[k] for b in buckets.values()) for k in ("fsync", "fdatasync", "mkdir", "rename", "created_files", "sync_seconds")}
    report = {"seconds_to_listening": round(listening, 2), "phases": buckets, "total": total,
              "loadavg": pathlib.Path("/proc/loadavg").read_text().split()[:3]}
    (pathlib.Path(args.out) / "syncs.json").write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
