#!/usr/bin/env bash

stable_toolchain="1.97.1"
dylint_toolchain="nightly-2026-05-28"
export RUSTUP_HOME="$project_dir/target/nix-rustup"

register_pinned_toolchain() {
  local name="$1"
  local expected_root="$2"
  local host
  local toolchain_link
  local linked_root

  host="$("$expected_root/bin/rustc" -vV | sed -n 's/^host: //p')"
  toolchain_link="$RUSTUP_HOME/toolchains/$name-$host"
  mkdir -p "$RUSTUP_HOME/toolchains"
  if [[ ! -e "$toolchain_link" && ! -L "$toolchain_link" ]]; then
    ln -s "$expected_root" "$toolchain_link"
  fi
  linked_root="$(readlink "$toolchain_link" 2>/dev/null || true)"
  if [[ "$linked_root" != "$expected_root" ]]; then
    echo "$toolchain_link points to $linked_root instead of $expected_root" >&2
    exit 1
  fi
}

register_pinned_toolchain "$stable_toolchain" "$NUDOX_STABLE_TOOLCHAIN"
register_pinned_toolchain "$dylint_toolchain" "$NUDOX_DYLINT_TOOLCHAIN"

stable_cargo() {
  rustup run "$stable_toolchain" cargo "$@"
}

dylint_cargo() {
  rustup run "$dylint_toolchain" cargo "$@"
}
