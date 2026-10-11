#!/usr/bin/env python3
"""Keep Cargo diagnostics visible and measure actual artifact reuse per target."""

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone


def check(target, jobs, cargo="cargo"):
    started_at = datetime.now(timezone.utc).isoformat()
    started = time.monotonic()
    fresh = rebuilt = 0
    with subprocess.Popen(
        [cargo, "check", "--locked", "--workspace", "--all-targets",
         "--target", target, "--jobs", str(jobs),
         "--message-format=json-render-diagnostics"],
        stdout=subprocess.PIPE,
        text=True,
    ) as process:
        for line in process.stdout:
            try:
                message = json.loads(line)
            except ValueError:
                print(line, end="", flush=True)
                continue
            if not isinstance(message, dict):
                print(line, end="", flush=True)
                continue
            if message.get("reason") == "compiler-artifact":
                if message.get("fresh") is True:
                    fresh += 1
                elif message.get("fresh") is False:
                    rebuilt += 1
            elif message.get("reason") == "compiler-message":
                rendered = message.get("message", {}).get("rendered")
                if rendered:
                    print(rendered, end="", file=sys.stderr, flush=True)
        code = process.wait()
    # These count emitted Cargo artifacts, not tests, bytes, or sccache hits.
    # A failed build can have partial counts; its exit code stays authoritative.
    metric = {
        "kind": "cargo-check",
        "head_sha": os.environ.get("CI_HEAD_SHA", ""),
        "target": target,
        "started_at": started_at,
        "seconds": round(time.monotonic() - started, 3),
        "exit_code": code,
        "fresh_artifacts": fresh,
        "rebuilt_artifacts": rebuilt,
    }
    print("CI-METRIC " + json.dumps(metric, sort_keys=True), flush=True)
    return code if code >= 0 else 128 - code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target")
    parser.add_argument("jobs", type=int)
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("jobs must be positive")
    return check(args.target, args.jobs)


if __name__ == "__main__":
    sys.exit(main())
