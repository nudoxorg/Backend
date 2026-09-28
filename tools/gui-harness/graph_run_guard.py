#!/usr/bin/env python3
"""Watchdog and continuous compiler-overlap audit; never stops external work.

Only process IDs and executable names/paths are read from ps. Unrelated process
arguments and environments are never read. Exit 125 means observed compiler
overlap; exit 124 means the owned command exceeded its wall-time budget.
"""
import argparse
import hashlib
import json
from pathlib import Path
import os
import signal
import subprocess
import time

COMPILERS = {"cargo", "rustc", "rust-lld", "clang", "clang++", "cc", "cc1",
             "cc1plus", "gcc", "g++", "ld", "ld64.lld", "lld", "sccache",
             "ccache", "swiftc", "swift-frontend", "ninja", "cmake"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--cwd", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--timeout", type=float, required=True)
    parser.add_argument("--interval", type=float, default=0.5)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--sha256", help="required frozen executable digest when --binary is used")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or args.timeout <= 0 or args.interval <= 0:
        parser.error("command, positive timeout and positive interval are required")
    args.out.mkdir(parents=True, exist_ok=True)
    def digest(path):
        with path.open("rb") as file:
            return hashlib.file_digest(file, "sha256").hexdigest()
    binary = args.binary.resolve() if args.binary else None
    if binary and (not args.sha256 or digest(binary) != args.sha256):
        parser.error("frozen binary digest absent or different; command was not started")
    provenance = {"command": command, "cwd": str(args.cwd.resolve()),
                  "binary": str(binary) if binary else None,
                  "binary_sha256": digest(binary) if binary else None,
                  "guard_sha256": digest(Path(__file__)),
                  "poll_interval_seconds": args.interval}
    started = time.monotonic()
    observations, compilers, errors = 0, {}, []
    timed_out = False
    with (args.out / "compiler-overlap.jsonl").open("w") as trace, \
            (args.out / "command.log").open("wb") as log:
        def observe():
            nonlocal observations
            wall = round(time.monotonic() - started, 6)
            try:
                scan = subprocess.run(["/bin/ps", "-axo", "pid=,comm="],
                                      text=True, capture_output=True, check=True, timeout=5)
                matches = []
                for line in scan.stdout.splitlines():
                    fields = line.strip().split(None, 1)
                    if len(fields) != 2 or Path(fields[1]).name not in COMPILERS:
                        continue
                    pid, executable = int(fields[0]), fields[1]
                    matches.append({"pid": pid, "executable": executable})
                    key = (pid, executable)
                    entry = compilers.setdefault(key, {"pid": pid, "executable": executable,
                                                       "first_seen_seconds": wall, "samples": 0})
                    entry["last_seen_seconds"] = wall
                    entry["samples"] += 1
                trace.write(json.dumps({"wall_seconds": wall, "compilers": matches}) + "\n")
                trace.flush()
                observations += 1
            except (subprocess.SubprocessError, OSError, ValueError) as error:
                errors.append({"wall_seconds": wall, "error": str(error)})
        observe()
        process = subprocess.Popen(command, cwd=args.cwd.resolve(), stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        deadline, next_poll = started + args.timeout, time.monotonic() + args.interval
        while process.poll() is None:
            now = time.monotonic()
            if now >= deadline:
                timed_out = True
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                break
            if now >= next_poll:
                observe()
                next_poll = time.monotonic() + args.interval
            time.sleep(min(0.05, max(0.001, deadline - time.monotonic())))
        exit_code = process.wait()
        observe()
    after_sha = digest(binary) if binary else None
    identity_changed = binary is not None and after_sha != provenance["binary_sha256"]
    result = {"provenance": provenance, "pid": process.pid, "exit_code": exit_code,
              "timed_out": timed_out, "wall_seconds": time.monotonic() - started,
              "compiler_observations": observations, "compiler_overlap": list(compilers.values()),
              "monitor_errors": errors, "binary_sha256_after": after_sha,
              "binary_identity_changed": identity_changed,
              "quiet": not compilers and not errors and not identity_changed,
              "limitations": ["Name-based 0.5-second sampling can miss shorter compiler activity.",
                              "No compiler observation proves compiler quiet only; other system load is not excluded.",
                              "Compiler executable paths are recorded; unrelated arguments and environments are not read."]}
    (args.out / "guard.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 124 if timed_out else (exit_code or (125 if compilers or errors or identity_changed else 0))


if __name__ == "__main__":
    raise SystemExit(main())
