#!/usr/bin/env python3
"""Run the same bounded test tiers locally and in CI. Use --plan to inspect commands."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
CI = ROOT / ".config" / "ci"


def plan(args: argparse.Namespace) -> list[str]:
    if args.tier == "smoke":
        command = ["nu", "--no-config-file", str(CI / "t1.nu")]
        shell = "smoke"
    elif args.tier == "integration":
        if args.bin_dir is None:
            raise ValueError("integration requires --bin-dir: reuse the exact smoke/artifact binaries")
        command = ["python3", str(CI / "surface.py"), "--bin-dir", str(args.bin_dir.resolve())]
        for case in args.case or []:
            command += ["--case", case]
        if args.jobs is not None:
            command += ["--jobs", str(args.jobs)]
        if args.update:
            command.append("--update")
        if args.report:
            command += ["--report", str(args.report.resolve())]
        command.append("--print-missing")
        shell = "surface"
    else:
        command = ["nu", "--no-config-file", str(CI / "t3.nu")]
        shell = "smoke"
    if args.tier != "integration" and (args.bin_dir or args.case or args.update or args.report or args.jobs is not None):
        raise ValueError("--bin-dir, --case, --update, --report and --jobs apply only to integration")
    return command if args.in_shell else ["nix", "develop", "path:.#" + shell, "--command", *command]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tier", choices=["smoke", "integration", "deep", "list"])
    parser.add_argument("--bin-dir", type=Path)
    parser.add_argument("--case", action="append")
    parser.add_argument("--jobs", type=int, choices=range(1, 5), help="concurrent isolated integration cases (default: 2)")
    parser.add_argument("--update", action="store_true", help="explicitly rewrite selected integration snapshots for review")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--in-shell", action="store_true", help="CI: already inside the tier's declared Nix shell")
    parser.add_argument("--plan", action="store_true", help="print the command as JSON without building or running")
    args = parser.parse_args()
    if args.tier == "list":
        cases = json.loads((ROOT / "tests/surface/cases.json").read_text())["cases"]
        print(json.dumps({"smoke": ["cli", "mcp", "gui"],
                          "integration": [case["name"] for case in cases],
                          "deep": ["cli-index-wait-laws"]}, indent=2))
        return 0
    try:
        command = plan(args)
    except ValueError as error:
        parser.error(str(error))
    if args.plan:
        print(json.dumps(command))
        return 0
    # A local smoke verdict must never swallow a separate T2 failure. T2 has
    # its own explicit command, binaries and failure status.
    environment = dict(os.environ, CI_T2_EXTERNAL="1")
    return subprocess.run(command, cwd=ROOT, env=environment, check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
