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
        if args.ci_report:
            if not args.source_sha or args.update:
                raise ValueError("--ci-report requires --source-sha and ordinary comparison (no --update)")
            command += ["--ci-report", str(args.ci_report.resolve()), "--source-sha", args.source_sha]
        elif args.source_sha:
            raise ValueError("--source-sha applies only to --ci-report")
        command.append("--print-missing")
        shell = "surface"
    else:
        command = ["nu", "--no-config-file", str(CI / "t3.nu")]
        shell = "smoke"
    if args.tier != "integration" and (args.bin_dir or args.case or args.update or args.report or args.jobs is not None or args.ci_report or args.source_sha):
        raise ValueError("--bin-dir, --case, --update, --report, --ci-report, --source-sha and --jobs apply only to integration")
    return command if args.in_shell else ["nix", "develop", "path:.#" + shell, "--command", *command]


def runner_environment(tier: str, has_ci_report: bool,
                       inherited: dict[str, str] | None = None) -> dict[str, str]:
    """Preserve explicit diagnostics while marking the separate T2 invocation."""
    environment = dict(os.environ if inherited is None else inherited)
    environment["CI_T2_EXTERNAL"] = "1"
    return environment


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tier", choices=["smoke", "integration", "deep", "list", "select"])
    parser.add_argument("--base", help="selection preview: compare from the merge base of this ref")
    parser.add_argument("--head", default="HEAD", help="selection preview: source commit (default HEAD)")
    parser.add_argument("--worktree", action="store_true", help="selection preview: include local tracked/untracked changes")
    parser.add_argument("--bin-dir", type=Path)
    parser.add_argument("--case", action="append")
    parser.add_argument("--jobs", type=int, choices=range(1, 5), help="concurrent isolated integration cases (default: 2)")
    parser.add_argument("--update", action="store_true", help="explicitly rewrite selected integration snapshots for review")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--ci-report", type=Path, help="exact-artifact case evidence; requires --source-sha")
    parser.add_argument("--source-sha", help="full source identity for --ci-report or source-bound list")
    parser.add_argument("--in-shell", action="store_true", help="CI: already inside the tier's declared Nix shell")
    parser.add_argument("--plan", action="store_true", help="print the command as JSON without building or running")
    args = parser.parse_args()
    if args.tier == "select":
        if not args.base:
            parser.error("select requires --base")
        if args.bin_dir or args.case or args.update or args.report or args.jobs is not None or args.in_shell or args.plan or args.ci_report or args.source_sha:
            parser.error("select only accepts --base, --head and --worktree")
        from selection import preview
        print(json.dumps(preview(ROOT, args.base, args.head, args.worktree), indent=2))
        return 0
    if args.base or args.head != "HEAD" or args.worktree:
        parser.error("--base, --head and --worktree apply only to select")
    if args.tier == "list":
        if args.bin_dir or args.case or args.update or args.report or args.ci_report or args.jobs is not None or args.in_shell or args.plan:
            parser.error("list does not execute or accept integration options")
        from catalog import read_catalog
        try:
            listed = read_catalog(ROOT, args.source_sha)
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            parser.error(str(error))
        print(json.dumps(listed, indent=2))
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
    environment = runner_environment(args.tier, args.ci_report is not None)
    return subprocess.run(command, cwd=ROOT, env=environment, check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
