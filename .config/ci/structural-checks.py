#!/usr/bin/env python3
"""Stream structural checks; retry only Nix's transient missing-input handoff."""

import re
import subprocess
import sys


def main() -> int:
    for attempt in range(2):
        missing_input = False
        with subprocess.Popen(
            ["nix", "build", "-L", "--keep-going", "--no-link", *sys.argv[1:]],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        ) as process:
            assert process.stdout is not None
            for line in process.stdout:
                sys.stdout.buffer.write(line)
                sys.stdout.buffer.flush()
                if re.search(rb"some dependencies of .* are missing", line):
                    missing_input = True
            code = process.wait()
        if code == 0:
            return 0
        if not missing_input:
            return code if code > 0 else 128 - code
        if attempt == 0:
            print("CI problem: Nix inputs vanished during handoff; retrying once", flush=True)
    print("CI problem: Nix input handoff failed twice; exit 75", flush=True)
    return 75


if __name__ == "__main__":
    sys.exit(main())
