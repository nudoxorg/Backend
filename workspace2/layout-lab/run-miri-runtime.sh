#!/bin/sh
set -eu

workspace_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace_root"

nix develop --impure --expr 'let nixpkgs=builtins.getFlake "nixpkgs"; rustOverlay=import (builtins.getFlake "github:oxalica/rust-overlay"); pkgs=import nixpkgs { system=builtins.currentSystem; overlays=[rustOverlay]; }; nightly=pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override { extensions=["rust-src" "miri"]; }); in pkgs.mkShell { packages=[nightly pkgs.clang]; }' -c sh -c '
  MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-runtime local_payload_prefix_guard_drops_partial_initialization_during_unwind &&
  MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-runtime terminal_retention_is_in_place_and_inline_or_local_storage_needs_no_heap_owner &&
  MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-runtime payload_cell_lifecycle_covers_cancel_terminal_reuse_and_drop &&
  cargo test --doc -p nudox-runtime
'
