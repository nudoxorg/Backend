#!/usr/bin/env python3
"""Create a content-bound receipt for a pinned macOS .NET runtime root."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from macho_relocation import RelocationInputError, create_dotnet_runtime_receipt, write_new_bytes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime-root", required=True, type=Path)
    parser.add_argument("--pin", required=True, type=Path, help="reviewed source/version/root-tree descriptor")
    parser.add_argument("--source-archive", type=Path, help="exact upstream archive when the pin kind is verified-archive")
    parser.add_argument("--target", choices=("aarch64-apple-darwin", "x86_64-apple-darwin"), required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.output.exists() or args.output.is_symlink():
        raise RelocationInputError(f"refusing to overwrite runtime receipt: {args.output}")
    receipt = create_dotnet_runtime_receipt(
        args.runtime_root.resolve(strict=True),
        args.pin.resolve(strict=True),
        args.target,
        args.source_archive.resolve(strict=True) if args.source_archive else None,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    receipt_bytes = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode("utf-8")
    write_new_bytes(args.output, receipt_bytes, label="runtime receipt")
    print(f"Created pinned .NET runtime receipt: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RelocationInputError, OSError) as error:
        print(f"runtime receipt refused: {error}", file=sys.stderr)
        sys.exit(2)
