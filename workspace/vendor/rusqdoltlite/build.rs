//! Build script for `rusqdoltlite`.
//!
//! Compiles the vendored DoltLite amalgamation
//! (`workspace/vendor/doltlite/doltlite.c`) into a static library and links it
//! into this crate. DoltLite is a SQLite fork whose B-tree pager is replaced by a
//! content-addressed prolly-tree engine; the single amalgamation file bundles the
//! SQLite core, the prolly engine, the BLAKE3 hasher, and every `dolt_*`
//! version-control function and virtual table.
//!
//! There is intentionally **no** `bindgen` invocation here: the raw FFI surface we
//! need is a small, stable subset of the SQLite C API and is hand-written in
//! `sys.rs`. That keeps the build hermetic (no network, no libclang) and the
//! binding auditable.
//!
//! # Why this script renames and localizes symbols
//!
//! DoltLite exposes the *ordinary* `sqlite3_*` C API — because it *is* SQLite,
//! forked. Compiled naively it exports 1154 global symbols, 291 of them
//! `sqlite3_*`, and it lands in binaries that also statically link a complete
//! stock SQLite (`index` depends on `rusqlite` with feature `bundled`, whose
//! `libsqlite3.a` exports 280 symbols — **every one of which DoltLite also
//! defines**). Two consequences, both silent:
//!
//! 1. With identical names, the linker satisfies *both* crates' references from
//!    whichever archive it scans first. One engine wins for the whole binary and
//!    which one is a link-order accident. If stock SQLite wins, `index`'s
//!    "sovereign versioned catalog" is plain SQLite with no `dolt_*` functions at
//!    all — the exact defect this script previously enabled.
//! 2. If both archive members are pulled in, the link fails with 280 duplicate
//!    symbols, nowhere near this file.
//!
//! So the engine is compiled with the API entry points this crate actually calls
//! **renamed** to a `doltlite_`-prefixed namespace (via `-Dsqlite3_x=doltlite_x`,
//! which the preprocessor applies consistently to the definition *and* every
//! internal caller inside the single translation unit), and every other global is
//! then **localized** with a partial link so it cannot participate in resolution
//! at all. The resulting archive exports exactly [`API_FUNCTIONS`] and nothing
//! else — verifiable with `nm`, and disjoint from stock SQLite by construction.
//!
//! That is what makes the substitution *impossible* rather than merely unlikely:
//! a `doltlite_open_v2` reference has no other definition anywhere in the
//! dependency graph.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The exact SQLite C API entry points `sys.rs` declares, minus their `sqlite3_`
/// prefix.
///
/// This list is the crate's entire C surface. It is load-bearing three times
/// over: it drives the `-D` renames handed to the compiler, it is the export
/// allow-list handed to the partial link, and it must stay in lockstep with the
/// `doltlite_api!` invocation in `sys.rs` (whose `#[link_name]`s are exactly
/// `doltlite_<entry>`). Adding an extern to `sys.rs` without adding it here
/// produces an undefined symbol at link time — loudly, and naming the symbol.
const API_FUNCTIONS: &[&str] = &[
    "open_v2",
    "close_v2",
    "prepare_v2",
    "step",
    "reset",
    "clear_bindings",
    "finalize",
    "column_count",
    "column_type",
    "column_int64",
    "column_double",
    "column_text",
    "column_blob",
    "column_bytes",
    "bind_null",
    "bind_int64",
    "bind_double",
    "bind_text",
    "bind_blob",
    "changes",
    "extended_errcode",
    "errmsg",
    "errstr",
    "free",
    "libversion",
];

/// Locate the vendored amalgamation directory relative to this crate.
///
/// Layout: `<workspace>/workspace/vendor/rusqdoltlite/build.rs` and the vendored
/// engine at `<workspace>/workspace/vendor/doltlite`. Both live under
/// `workspace/vendor/`, so we resolve the sibling `doltlite` from this crate's
/// manifest directory.
fn vendored_doltlite_directory() -> PathBuf {
    let manifest_directory = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"),
    );
    manifest_directory
        .parent()
        .expect("rusqdoltlite manifest directory has a parent (workspace/vendor/)")
        .join("doltlite")
}

fn main() {
    // `doltlite_engine_linked` is set below if and only if a real DoltLite
    // amalgamation was compiled and archived. `sys.rs` declares the C externs
    // only under it; `Connection::open` refuses without it.
    println!("cargo::rustc-check-cfg=cfg(doltlite_engine_linked)");

    let vendor_directory = vendored_doltlite_directory();
    let amalgamation_source = vendor_directory.join("doltlite.c");
    let amalgamation_header = vendor_directory.join("doltlite.h");

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed={}", amalgamation_source.display());
    println!("cargo::rerun-if-changed={}", amalgamation_header.display());

    // The error the crate reports when it has no engine names this exact path, so
    // the reader does not have to guess where the file was expected.
    println!(
        "cargo::rustc-env=RUSQDOLTLITE_AMALGAMATION_PATH={}",
        amalgamation_source.display()
    );

    // The ~12 MB generated amalgamation is not committed (see
    // workspace/vendor/doltlite/VENDORING.md), so it may be absent.
    //
    // Historically this emitted a warning and returned, and the crate went on to
    // declare plain `sqlite3_*` externs that resolved against whatever SQLite
    // happened to be in the binary. That produced a green build running the wrong
    // database engine. It no longer can: without `doltlite_engine_linked`,
    // `sys.rs` declares no externs and `Connection::open` returns
    // `EngineError::EngineNotLinked`. The warning below is a courtesy, not the
    // guard.
    if !amalgamation_source.exists() {
        println!(
            "cargo::warning=rusqdoltlite: `{}` is absent, so the DoltLite engine was NOT \
             compiled. This crate now refuses to open a connection at all \
             (`EngineError::EngineNotLinked`) rather than silently binding the stock SQLite that \
             `rusqlite`'s `bundled` feature links into the same binary. See \
             workspace/vendor/doltlite/VENDORING.md to obtain the amalgamation.",
            amalgamation_source.display()
        );
        return;
    }

    // Expose the include directory to downstream `-sys` consumers, if any.
    println!("cargo::metadata=include={}", vendor_directory.display());

    let object_files = compile_amalgamation(&amalgamation_source, &vendor_directory);
    let out_directory = PathBuf::from(
        std::env::var("OUT_DIR").expect("OUT_DIR is always set by cargo for build scripts"),
    );
    let archive = localize_and_archive(&object_files, &out_directory);

    println!(
        "cargo::rustc-link-search=native={}",
        archive
            .parent()
            .expect("archive path has a parent directory")
            .display()
    );
    println!("cargo::rustc-link-lib=static=doltlite");

    // Runtime dependencies of the amalgamation.
    //
    // * `pthread` — SQLite's serialized threading mode uses POSIX mutexes.
    // * `zlib` is referenced only by the optional RBU `compress=` path and resolves
    //   weakly on macOS/Linux; we do **not** force-link it because the nix build
    //   environment on this machine ships no standalone `-lz`, and nothing on the
    //   catalog path needs it.
    if std::env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("unix") {
        println!("cargo::rustc-link-lib=pthread");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo::rustc-link-lib=ws2_32");
        println!("cargo::rustc-link-lib=bcrypt");
    }

    println!("cargo::rustc-cfg=doltlite_engine_linked");
}

/// Compile `doltlite.c` into object files, with the public API entry points
/// renamed into the `doltlite_` namespace.
///
/// Returns the intermediate objects rather than a finished archive because the
/// export surface still has to be narrowed (see [`localize_and_archive`]).
fn compile_amalgamation(amalgamation_source: &Path, vendor_directory: &Path) -> Vec<PathBuf> {
    let mut build = cc::Build::new();
    build
        .file(amalgamation_source)
        .include(vendor_directory)
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

    // The rename. Applied by the preprocessor to the whole translation unit, so
    // the definition in `doltlite.c`, its declaration in `doltlite.h`, and every
    // internal call site all move together — there is no half-renamed state.
    for entry in API_FUNCTIONS {
        build.define(&format!("sqlite3_{entry}"), format!("doltlite_{entry}").as_str());
    }

    build.compile_intermediates()
}

/// Narrow the engine's export surface to exactly [`API_FUNCTIONS`] and archive it
/// as `libdoltlite.a`.
///
/// Every other global DoltLite defines — the remaining `sqlite3_*` API, the
/// BLAKE3 core, the prolly-tree internals, and ~800 unprefixed helpers such as
/// `advanceTreeCursor` — is demoted to a file-local symbol by a partial link, so
/// it can neither collide with nor be mistaken for another library's definition.
///
/// The check at the end is the point of the exercise: it *reads back* the
/// archive's actual export table and fails the build if it is not exactly the
/// expected set. A build script that only ran the tool and trusted it is how the
/// original defect survived.
fn localize_and_archive(object_files: &[PathBuf], out_directory: &Path) -> PathBuf {
    let exported_symbols: Vec<String> = API_FUNCTIONS
        .iter()
        .map(|entry| format!("{}doltlite_{entry}", symbol_prefix()))
        .collect();

    let localized_object = out_directory.join("doltlite-localized.o");
    let archive = out_directory.join("libdoltlite.a");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => {
            // ld64: `-r` performs a partial link; `-exported_symbols_list` keeps the
            // listed symbols global and demotes every other definition to
            // private-extern.
            let list = out_directory.join("doltlite-exported-symbols.txt");
            std::fs::write(&list, format!("{}\n", exported_symbols.join("\n")))
                .expect("write the export allow-list into OUT_DIR");
            let mut command = Command::new("ld");
            command
                .arg("-r")
                .arg("-exported_symbols_list")
                .arg(&list)
                .arg("-o")
                .arg(&localized_object)
                .args(object_files);
            run(command, "ld -r (partial link, macOS)");
        }
        "linux" | "android" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" => {
            // GNU ld cannot filter exports during `-r`, so do it in two steps:
            // merge, then have objcopy keep only the allow-list global.
            let mut link = Command::new("ld");
            link.arg("-r").arg("-o").arg(&localized_object).args(object_files);
            run(link, "ld -r (partial link, ELF)");

            let mut objcopy = Command::new("objcopy");
            for symbol in &exported_symbols {
                objcopy.arg(format!("--keep-global-symbol={symbol}"));
            }
            objcopy.arg(&localized_object);
            run(objcopy, "objcopy --keep-global-symbol");
        }
        other => panic!(
            "rusqdoltlite: no symbol-localization step is implemented for target OS `{other}`. \
             Without it, DoltLite exports 291 `sqlite3_*` symbols that collide, name for name, \
             with the stock SQLite that `rusqlite`'s `bundled` feature links into the same \
             binary — which either fails the link with ~280 duplicate symbols or, worse, \
             silently resolves both engines to whichever archive the linker scanned first. \
             Refusing to build rather than produce that."
        ),
    }

    let _ = std::fs::remove_file(&archive);
    let mut archiver = Command::new(archiver_program());
    archiver.arg("crs").arg(&archive).arg(&localized_object);
    run(archiver, "ar crs");

    verify_exported_symbols(&archive, &exported_symbols);
    archive
}

/// The leading character the platform's object format prepends to C symbol names
/// (`_` in Mach-O, nothing in ELF).
fn symbol_prefix() -> &'static str {
    match std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default().as_str() {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => "_",
        _ => "",
    }
}

/// The archiver to build `libdoltlite.a` with, honouring `AR` when set (nix and
/// cross builds both rely on that).
fn archiver_program() -> String {
    std::env::var("AR").unwrap_or_else(|_| "ar".to_owned())
}

/// Run a build step, failing the build with the tool's own diagnostics rather
/// than a bare exit code.
fn run(mut command: Command, description: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("rusqdoltlite: could not spawn `{description}`: {error}"));
    if !output.status.success() {
        panic!(
            "rusqdoltlite: `{description}` failed ({}).\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Assert that `libdoltlite.a` exports exactly `expected` and nothing more.
///
/// This is the build script proving its own claim. `nm -g -U` lists only globally
/// visible *defined* symbols; if the localization step silently did nothing (an
/// unexpected linker, a flag a future toolchain drops), the surplus shows up here
/// with the names that would have collided, instead of at the final link of some
/// downstream binary.
fn verify_exported_symbols(archive: &Path, expected: &[String]) {
    let output = Command::new("nm")
        .arg("-g")
        .arg("-U")
        .arg(archive)
        .output()
        .unwrap_or_else(|error| panic!("rusqdoltlite: could not run `nm` on the engine archive to verify its export surface: {error}"));
    if !output.status.success() {
        panic!(
            "rusqdoltlite: `nm -g -U {}` failed: {}",
            archive.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut actual: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .collect();
    actual.sort_unstable();
    actual.dedup();

    let mut expected_sorted: Vec<&str> = expected.iter().map(String::as_str).collect();
    expected_sorted.sort_unstable();

    if actual != expected_sorted {
        let surplus: Vec<&&str> = actual
            .iter()
            .filter(|symbol| !expected_sorted.contains(symbol))
            .collect();
        let missing: Vec<&&str> = expected_sorted
            .iter()
            .filter(|symbol| !actual.contains(symbol))
            .collect();
        panic!(
            "rusqdoltlite: the engine archive does not export exactly the declared FFI surface.\n\
             missing ({}): {:?}\n\
             surplus ({}): {:?}\n\
             A surplus means the symbol-localization step did not take effect, and this archive \
             would collide with (or silently substitute for) the stock SQLite in the same binary. \
             A missing symbol means `API_FUNCTIONS` in build.rs and the `doltlite_api!` externs in \
             sys.rs have drifted apart.",
            missing.len(),
            missing,
            surplus.len(),
            surplus.iter().take(20).collect::<Vec<_>>(),
        );
    }
}
