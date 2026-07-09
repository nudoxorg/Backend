#!/usr/bin/env python3
"""Thin shim — delegates to ``nudox.commands.update``.

The full feature-propagation engine now lives in the ``nudox`` package.  This
entrypoint is preserved so existing invocations keep working:

    python3 build/third-party/tools/gen-registry.py
    python3 build/third-party/tools/gen-registry.py --dry-run
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from nudox.commands import update  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Regenerate registry.bzl with fresh crate metadata and unified features."
    )
    update.add_arguments(parser)
    args = parser.parse_args()
    return update.run(args)


if __name__ == "__main__":
    sys.exit(main())
