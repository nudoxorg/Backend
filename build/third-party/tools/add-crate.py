#!/usr/bin/env python3
"""Thin shim — delegates to ``nudox.commands.add``.

The full crates.io seed logic now lives in the ``nudox`` package.  This
entrypoint is preserved so existing invocations keep working:

    python3 build/third-party/tools/add-crate.py <name> [version] [--dry-run]

With no arguments it prints usage and exits non-zero (as before).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from nudox.commands import add  # noqa: E402


def main() -> int:
    raw = sys.argv[1:]
    positionals = [a for a in raw if not a.startswith("--")]
    if not positionals:
        print(
            f"Usage: {sys.argv[0]} <name> [version] [--dry-run]",
            file=sys.stderr,
        )
        return 1

    parser = argparse.ArgumentParser(
        description="Add a crate (and missing transitive deps) to registry.bzl."
    )
    add.add_arguments(parser)
    args = parser.parse_args(raw)
    return add.run(args)


if __name__ == "__main__":
    sys.exit(main())
