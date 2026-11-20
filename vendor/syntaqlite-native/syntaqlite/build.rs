// Copyright 2025 The syntaqlite Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0.

//! Build script for `syntaqlite`.
//!
//! Re-emits the native library search paths from `syntaqlite-syntax` so that
//! `cargo test --doc` can link doc-test binaries against the C parser/tokenizer.

fn main() {
    // DEP_SYNTAQLITE_SYNTAX_* env vars are set by syntaqlite-syntax's build
    // script (via its `links = "syntaqlite_syntax"` key in Cargo.toml).
    // Re-emit them so rustdoc can find the native libraries when linking
    // doc-test binaries.
    let out_dir = std::env::var("DEP_SYNTAQLITE_SYNTAX_OUT_DIR").unwrap_or_default();
    if !out_dir.is_empty() {
        println!("cargo:rustc-link-search=native={out_dir}");
    }
}
