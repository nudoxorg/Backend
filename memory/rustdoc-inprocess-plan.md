---
name: rustdoc-inprocess-plan
description: "Rustdoc in-process driver: full Buck-only refactor landed 2026-07-08. Phase A (vendored librustdoc + no-JSON) blocked on running vendor script + sha256 fill-in."
metadata:
  node_type: memory
  type: project
  originSessionId: current
---

Goal: replace `cargo rustdoc --output-format json` → JSON file → serde_json with a driver that lowers `rustdoc_types::Crate → IR` in-process — no rustdoc JSON ever touches disk.

**Status as of 2026-07-08: fully Buck-native, no Cargo.toml anywhere, no JSON fallback.**

## Architecture

```
workspace/
  rust-lowering/          ← new Buck member (lib)
    BUCK
    lib.rs                ← pub mod context/error/function/generics/item/types
    context.rs
    error.rs              ← Parse + Package + ProcessFailure + MetadataError
    function.rs
    generics.rs
    item.rs
    types.rs
  rustdoc-driver/         ← new Buck member (bin)
    BUCK                  ← rust_bin, deps on ir + rust-lowering + librustdoc
    src/main.rs           ← Phase A: capture_crate → lower → write IR+sm JSON
  compiler/
    compile/rust/
      mod.rs              ← pub use rust_lowering::{context,error,...}; pub mod package,traversal
      package.rs          ← driver-only path, errors if rustdoc-driver not found
      traversal.rs        ← git version traversal (stays in compiler)
```

## Key Buck targets

- `//workspace/rust-lowering:rust-lowering` — shared lowering lib
- `//workspace/rustdoc-driver:rustdoc-driver` — Phase A driver binary
- `//workspace/compiler:compiler` — now depends on `rust-lowering`

Both `rust-lowering` and `rustdoc-driver` are in `build/rust.bzl` `_MEMBERS`.

## Env vars

- `RUSTC_BOOTSTRAP=1` in `flake.nix` devShell env (for `#![feature(rustc_private)]`)
- `RUSTC_BOOTSTRAP=1` also set on the `rustdoc-driver` rust_bin Buck target

## Phase A activation (in-process, zero-disk-JSON)

1. `bash scripts/vendor-librustdoc.sh` — sparse-clones librustdoc source
2. `patch -p1 -d build/third-party/vendor/librustdoc < build/third-party/patches/librustdoc/expose-json-crate.patch`
3. Fill in sha256 in `build/third-party/git.bzl` (PLACEHOLDER_FILL_IN…)
4. `buck2 build //workspace/rustdoc-driver:rustdoc-driver`

After step 4, `package.rs` will find the driver via `NUDOX_RUSTDOC_DRIVER` or PATH.

## What was removed

- All Cargo.toml files (workspace/rustdoc-driver/, rust-lowering/, compiler/intermediate-representation/)
- JSON fallback path in `package.rs` (`generate_ir_via_json`, `run_cargo_rustdoc`, `source_map_from_crate`)
- Genrule in `workspace/compiler/BUCK` that cargo-built the driver
- `workspace/rustdoc-driver/driver/` subtree

## Runtime discovery

Driver found via (in order):
1. `NUDOX_RUSTDOC_DRIVER` env var
2. Sibling binary next to current exe (Buck run layout)
3. `rustdoc-driver` on `$PATH`

Error if not found — no fallback.
