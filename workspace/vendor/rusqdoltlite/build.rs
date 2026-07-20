//! Build script for `rusqdoltlite`.
//!
//! Compiles the vendored DoltLite amalgamation (`workspace/vendor/doltlite/doltlite.c`)
//! into a static library and links it into this crate. DoltLite is a SQLite fork
//! whose B-tree pager is replaced by a content-addressed prolly-tree engine; the
//! single amalgamation file bundles the SQLite core, the prolly engine, the BLAKE3
//! hasher, and every `dolt_*` version-control function and virtual table.
//!
//! There is intentionally **no** `bindgen` invocation here: the raw FFI surface we
//! need is a small, stable subset of the SQLite C API and is hand-written in
//! `sys.rs`. That keeps the build hermetic (no network, no libclang) and the
//! binding auditable.

use std::path::PathBuf;

/// Locate the vendored amalgamation directory relative to this crate.
///
/// Layout: `<workspace>/workspace/vendor/rusqdoltlite/build.rs` and the vendored
/// engine at `<workspace>/workspace/vendor/doltlite`. Both live under
/// `workspace/vendor/`, so we resolve the sibling `doltlite` from this crate's
/// manifest directory.
fn vendored_doltlite_directory() -> PathBuf {
    let manifest_directory =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"));
    manifest_directory
        .parent()
        .expect("rusqdoltlite manifest directory has a parent (workspace/vendor/)")
        .join("doltlite")
}

fn main() {
    let vendor_directory = vendored_doltlite_directory();
    let amalgamation_source = vendor_directory.join("doltlite.c");
    let amalgamation_header = vendor_directory.join("doltlite.h");

    // Rebuild if the vendored engine or this script changes.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", amalgamation_source.display());
    println!("cargo:rerun-if-changed={}", amalgamation_header.display());

    // Expose the include directory to downstream `-sys` consumers, if any.
    println!("cargo:include={}", vendor_directory.display());

    let mut build = cc::Build::new();
    build
        .file(&amalgamation_source)
        .include(&vendor_directory)
        // Prolly engine on: replaces the B-tree pager with the content-addressed
        // chunk store and links in every `dolt_*` function. Without this the fork
        // degrades to stock SQLite.
        .define("DOLTLITE_PROLLY", "1")
        // Thread-safe (serialized) mode. The catalog is single-writer, but the
        // safe layer is `Send` and connections may live on different threads.
        .define("SQLITE_THREADSAFE", "1")
        // Match the upstream release build's feature flags for a faithful engine:
        // math functions, FTS5, JSON, R-Tree, dbstat, and strict typing.
        .define("SQLITE_ENABLE_MATH_FUNCTIONS", None)
        .define("SQLITE_ENABLE_FTS5", None)
        .define("SQLITE_ENABLE_RTREE", None)
        .define("SQLITE_ENABLE_DBSTAT_VTAB", None)
        .define("SQLITE_ENABLE_COLUMN_METADATA", None)
        // The amalgamation intentionally keeps `/* ... */` markers that trigger
        // nested-comment warnings; they are cosmetic. Silence the noise so a real
        // warning is not lost in it.
        .flag_if_supported("-Wno-comment")
        .flag_if_supported("-Wno-unused-function")
        .warnings(false);

    build.compile("doltlite");

    // Runtime dependencies of the amalgamation.
    //
    // * `pthread` — SQLite's serialized threading mode uses POSIX mutexes.
    // * `zlib` is referenced only by the optional RBU `compress=` path and resolves
    //   weakly on macOS/Linux; we do **not** force-link it because the nix build
    //   environment on this machine ships no standalone `-lz`, and nothing on the
    //   catalog path needs it.
    if std::env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("unix") {
        println!("cargo:rustc-link-lib=pthread");
    }
    // On non-Windows platforms `libm` and `libdl` are pulled in transitively by the
    // C runtime; no explicit link directive is required for the catalog subset.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // mbedtls / server sockets need these; harmless for the embedded subset.
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=bcrypt");
    }
}
