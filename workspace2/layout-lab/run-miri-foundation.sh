#!/bin/sh
set -eu

workspace_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace_root"

nix develop --impure --expr 'let nixpkgs=builtins.getFlake "nixpkgs"; rustOverlay=import (builtins.getFlake "github:oxalica/rust-overlay"); pkgs=import nixpkgs { system=builtins.currentSystem; overlays=[rustOverlay]; }; nightly=pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override { extensions=["rust-src" "miri"]; }); in pkgs.mkShell { packages=[nightly pkgs.clang]; }' -c sh -c 'MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-frame && MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-view'
