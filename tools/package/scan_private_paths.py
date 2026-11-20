#!/usr/bin/env python3
"""Fail when a shipped binary embeds the private filesystem layout of the machine that built it.

Rust embeds absolute source paths in panic locations, assertion messages and debug records.
Dependencies from a registry or vendored directory are the usual leak: their paths start with
the builder's CARGO_HOME or checkout. A release build must remap those prefixes
(`--remap-path-prefix=<private>=/nudox/<label>`, set through CARGO_ENCODED_RUSTFLAGS by the
build recorder); this gate checks the result instead of trusting the flags.

    scan_private_paths.py --forbid /root/builder --forbid /home/ci backend-cli backend-mcp

Exit status 0 when no forbidden prefix occurs in any file, 1 otherwise, 2 for bad usage.
"""
import argparse
import sys
from pathlib import Path

CHUNK = 1 << 20


def scan(path, prefixes):
    """Return {prefix: count} of the prefixes found in `path`, reading in bounded chunks."""
    needles = {prefix: prefix.encode() for prefix in prefixes}
    overlap = max((len(needle) for needle in needles.values()), default=1) - 1
    found = {prefix: 0 for prefix in prefixes}
    tail = b""
    with open(path, "rb") as handle:
        while True:
            block = handle.read(CHUNK)
            if not block:
                break
            window = tail + block
            for prefix, needle in needles.items():
                found[prefix] += window.count(needle) - tail.count(needle)
            tail = window[-overlap:] if overlap else b""
    return {prefix: count for prefix, count in found.items() if count}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--forbid", action="append", default=[], help="absolute path prefix that must not appear")
    parser.add_argument("files", nargs="+")
    args = parser.parse_args(argv)
    if not args.forbid or any(not prefix.startswith("/") or prefix == "/" for prefix in args.forbid):
        parser.error("give at least one absolute --forbid prefix other than /")
    failed = False
    for name in args.files:
        path = Path(name)
        if not path.is_file():
            print(f"{name}: not a file", file=sys.stderr)
            return 2
        hits = scan(path, args.forbid)
        for prefix, count in sorted(hits.items()):
            print(f"{name}: {count} occurrence(s) of private prefix {prefix}")
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
