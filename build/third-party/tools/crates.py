#!/usr/bin/env python3
"""crates — a cargo-like dispatcher for the Buck2 third-party tooling.

Subcommands:
    add     Seed a crate (and missing transitive deps) into registry.bzl
    update  Refresh registry.bzl from crates.io (feature propagation)
    check   Verify the dependency graph (stub)
    new     Scaffold a new workspace member (stub)
    build   Build a workspace member (stub)
    test    Test a workspace member (stub)

Runnable as `python3 crates.py <verb> ...` and importable as a module.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# Ensure the package is importable when invoked as a script.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from nudox.commands import add, check, new, update, verbs  # noqa: E402


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="crates",
        description="Cargo-like tooling for the Buck2 third-party crate registry.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    p_add = subparsers.add_parser("add", help="add a crate to the registry")
    add.add_arguments(p_add)
    p_add.set_defaults(func=add.run)

    p_update = subparsers.add_parser("update", help="refresh registry.bzl from crates.io")
    update.add_arguments(p_update)
    p_update.set_defaults(func=update.run)

    p_check = subparsers.add_parser("check", help="verify the dependency graph")
    check.add_arguments(p_check)
    p_check.set_defaults(func=check.run)

    p_new = subparsers.add_parser("new", help="scaffold a new workspace member")
    new.add_arguments(p_new)
    p_new.set_defaults(func=new.run)

    p_build = subparsers.add_parser("build", help="build a workspace member")
    verbs.add_arguments_build(p_build)
    p_build.set_defaults(func=verbs.run_build)

    p_test = subparsers.add_parser("test", help="test a workspace member")
    verbs.add_arguments_test(p_test)
    p_test.set_defaults(func=verbs.run_test)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
