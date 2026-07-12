"""``crates new`` — scaffold a first-party workspace crate.

Creates the crate directory, a starter source file, and a BUCK file, then wires
the new member into ``build/rust.bzl``'s ``_MEMBERS`` dict and adds a top-level
alias in the root ``BUCK`` file.

    crates new workspace/foo            # library member //workspace/foo:foo
    crates new workspace/foo --bin      # binary member  //workspace/foo:foo
"""

from __future__ import annotations

import argparse
import sys

from .. import buckedit, paths


def add_arguments(sub: argparse.ArgumentParser) -> None:
    sub.add_argument("path", help="path of the new workspace member to create")
    sub.add_argument(
        "--bin",
        action="store_true",
        help="create a binary member instead of a library",
    )
    sub.add_argument(
        "--dry-run",
        action="store_true",
        help="print the actions that would be taken without writing anything",
    )


# ── BUCK templates ────────────────────────────────────────────────────────────

def _lib_buck(name: str) -> str:
    return (
        'load("//build:rust.bzl", "deps", "rust_crate")\n'
        "\n"
        f'rust_crate(name = "{name}", deps = deps())\n'
    )


def _bin_buck(name: str) -> str:
    return (
        'load("//build:rust.bzl", "deps", "rust_bin")\n'
        "\n"
        f'rust_bin(\n'
        f'    name = "{name}",\n'
        f'    srcs = ["main.rs"],\n'
        f'    crate_root = "main.rs",\n'
        f'    deps = deps(),\n'
        f")\n"
    )


def _member_present(member: str) -> bool:
    """True if ``member`` is already a key in rust.bzl's ``_MEMBERS`` dict."""
    import re

    text = paths.RUST_BZL.read_text(encoding="utf-8")
    m = re.search(r"_MEMBERS\s*=\s*\{(?P<body>.*?)\}", text, flags=re.DOTALL)
    body = m.group("body") if m else ""
    return bool(re.search(rf'"{re.escape(member)}"\s*:', body))


def run(args: argparse.Namespace) -> int:
    rel_path = args.path.strip("/")
    if not rel_path:
        print("new: a member path is required (e.g. workspace/foo)", file=sys.stderr)
        return 1

    member = rel_path.rsplit("/", 1)[-1]  # last path component
    target_label = f"//{rel_path}:{member}"
    crate_dir = paths.REPO_ROOT / rel_path
    dry_run = getattr(args, "dry_run", False)

    src_name = "main.rs" if args.bin else "lib.rs"
    src_body = "fn main() {}\n" if args.bin else "pub fn placeholder() {}\n"
    buck_body = _bin_buck(member) if args.bin else _lib_buck(member)

    # ── refusal conditions ────────────────────────────────────────────────────
    if crate_dir.exists():
        print(f"new: refusing — directory already exists: {crate_dir}", file=sys.stderr)
        return 1
    if _member_present(member):
        print(
            f"new: refusing — member '{member}' already exists in _MEMBERS",
            file=sys.stderr,
        )
        return 1

    src_path = crate_dir / src_name
    buck_path = crate_dir / "BUCK"

    if dry_run:
        print("[dry-run] would scaffold a new workspace member:")
        print(f"  create dir   {crate_dir}")
        print(f"  create file  {src_path}")
        print(f"  create file  {buck_path}")
        print(f"  rust.bzl     register _MEMBERS['{member}'] = '{target_label}'")
        print(f"  root BUCK    alias(name = '{member}', actual = '{target_label}')")
        print(f"\nBUCK contents:\n{buck_body}")
        return 0

    # ── write ─────────────────────────────────────────────────────────────────
    crate_dir.mkdir(parents=True, exist_ok=False)
    src_path.write_text(src_body, encoding="utf-8")
    buck_path.write_text(buck_body, encoding="utf-8")

    buckedit.ensure_member_in_rust_bzl(member, target_label)
    buckedit.ensure_alias_in_root_buck(member, target_label)

    print(f"Created workspace member '{member}' at {crate_dir}")
    print(f"  {src_path}")
    print(f"  {buck_path}")
    print("\nNext steps:")
    print(f"  buck2 build //{rel_path}/...")
    return 0
