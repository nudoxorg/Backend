//! The crates.io corpus table shared by this crate's real-package tests.
//!
//! Two test binaries read it, at two different costs:
//!
//! * `corpus_manifest.rs` — milliseconds, no rust-analyzer. Checks that every
//!   entry below is really on disk and really is the package it claims to be,
//!   and that the producer refuses inputs that are not cargo packages at all.
//! * `corpus_sweep.rs` — ~1 minute per entry. Lowers every entry through the
//!   real `nudox_producer::produce` pipeline and asserts on its real API.
//!
//! Splitting them is the point: the cheap binary is the one that can say
//! "the corpus is not provisioned" in the pre-commit gate, so the expensive
//! one can never be quietly sweeping nothing.
//!
//! # Why the expectations are `(name, kind)` pairs and not counts
//!
//! docs/AGENTS-DOCTRINE.md §4: "a test that would pass against a stub is not a
//! test." A count, a `table.len() > 0`, or an `is_ok()` is satisfied by a
//! producer that emits one entry named `""`. Every [`Want`] below names a
//! symbol that was read out of the checkout's own source in this repository
//! before it was written down here, together with the IR kind the Rust
//! declaration that defines it must lower to — so `memchr` must be a
//! `Function`, `Memchr` a `Record`, `arch` a `Module`, `Mutex` an `Alias`.
//! Matching the name alone would accept a producer that lowered every item as
//! an opaque module; matching name *and* kind would not.
//!
//! Every symbol is one *declared in the package's own source*, never one it
//! merely re-exports from a dependency. `walk::lower_crate` declares items
//! from every module of the walked crate, public or private (see its comment
//! on why private declarations are unconditional), so a private `mod memchr`
//! containing `pub fn memchr` is in scope for these assertions; a
//! `pub use clap_builder::*` is not, because the item it names belongs to
//! another crate and lowers as an `EntryInner::Reference` with no kind.

#![allow(dead_code)]

use std::path::PathBuf;

use nudox_ir::kind::KindDiscriminant;

/// One symbol that must appear in a package's lowered table, with the IR kind
/// its Rust declaration must produce.
pub struct Want {
    pub name: &'static str,
    pub kind: KindDiscriminant,
}

const fn want(name: &'static str, kind: KindDiscriminant) -> Want {
    Want { name, kind }
}

/// One corpus entry: `result/<dir>`, the cargo package name (which must
/// be the `[package] name` in that checkout's manifest, because
/// `ra::documented_package_names` matches on it exactly), the pinned version,
/// and the symbols its lowering must contain.
pub struct Entry {
    pub dir: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub expect: &'static [Want],
}

use KindDiscriminant::{Alias, Const, Enum, Function, Module, Record, Trait};

/// The 23 crates.io version entries from `nix/corpus.nix` (20 packages;
/// `log` carries two versions and `memchr` three, for lineage testing), in
/// manifest order.
///
/// Kept as a literal list rather than parsed from the TOML at test time for the
/// same reason `typescript/tests/real_npm_packages.rs` does: a missing or
/// renamed fixture must fail as one clearly-named entry, not as a parse error
/// that takes the whole file down. If the manifest's crates.io section changes,
/// update this list — `corpus_manifest.rs` will tell you it drifted.
pub const ENTRIES: &[Entry] = &[
    Entry {
        dir: "bytes-1.11.0",
        name: "bytes",
        version: "1.11.0",
        // src/bytes.rs:101, src/bytes_mut.rs:60, src/buf/buf_impl.rs:117,
        // src/lib.rs:79.
        expect: &[
            want("Bytes", Record),
            want("BytesMut", Record),
            want("Buf", Trait),
            want("buf", Module),
        ],
    },
    Entry {
        dir: "hashbrown-0.17.1",
        name: "hashbrown",
        version: "0.17.1",
        // src/map.rs:182, src/set.rs:111, src/lib.rs:163, src/lib.rs:72.
        expect: &[
            want("HashMap", Record),
            want("HashSet", Record),
            want("TryReserveError", Enum),
            want("hash_map", Module),
        ],
    },
    Entry {
        dir: "indexmap-2.13.0",
        name: "indexmap",
        version: "2.13.0",
        // src/map.rs:92, src/set.rs:84, src/lib.rs:120-121.
        expect: &[
            want("IndexMap", Record),
            want("IndexSet", Record),
            want("map", Module),
            want("set", Module),
        ],
    },
    Entry {
        dir: "itoa-1.0.18",
        name: "itoa",
        version: "1.0.18",
        // src/lib.rs:72, src/lib.rs:119.
        expect: &[want("Buffer", Record), want("Integer", Trait)],
    },
    Entry {
        dir: "lazy_static-1.4.0",
        name: "lazy_static",
        version: "1.4.0",
        // src/lib.rs:185, src/lib.rs:213.
        expect: &[want("LazyStatic", Trait), want("initialize", Function)],
    },
    Entry {
        dir: "libc-0.2.161",
        name: "libc",
        version: "0.2.161",
        // src/unix/mod.rs:10 and :19 — this test host is `cfg(unix)`, so the
        // `unix` module is the active one. Chosen deliberately: libc's API is
        // almost entirely `cfg`-gated, and a producer that dropped the active
        // `cfg` branch would still lower thousands of entries while losing
        // exactly these.
        expect: &[want("c_int", Alias), want("size_t", Alias)],
    },
    Entry {
        dir: "log-0.4.17",
        name: "log",
        version: "0.4.17",
        // src/lib.rs:425, :641, :1269, :874, :1465, :1407, :1383.
        //
        // `set_logger` and `set_boxed_logger` are the two items in this corpus
        // whose visibility depends on a *build script* rather than on a Cargo
        // feature: log 0.4.17 gates them on `#[cfg(atomic_cas)]`, a custom cfg
        // its `build.rs` emits (`println!("cargo:rustc-cfg=atomic_cas")`) for
        // every target that has atomic compare-exchange, which
        // aarch64-apple-darwin does. 0.4.33 replaced the build script with the
        // built-in `#[cfg(target_has_atomic = "ptr")]` and is otherwise the
        // same API — which is what makes this pair an A/B for build-script cfg
        // delivery rather than two arbitrary versions.
        expect: &[
            want("Level", Enum),
            want("LevelFilter", Enum),
            want("Log", Trait),
            want("Record", Record),
            want("set_logger", Function),
            want("set_boxed_logger", Function),
            want("max_level", Function),
        ],
    },
    Entry {
        dir: "log-0.4.33",
        name: "log",
        version: "0.4.33",
        // src/lib.rs:475, :636, :1275, :868, :1504, :1446, :1422 — the same
        // seven symbols at different lines. Two versions of one package are in
        // the corpus for lineage testing, so asserting the *same* API on both
        // is the point: a producer whose output drifts between two releases of
        // a crate that did not change its API is the failure this pair exists
        // to catch, and here it does catch one (see 0.4.17 above).
        expect: &[
            want("Level", Enum),
            want("LevelFilter", Enum),
            want("Log", Trait),
            want("Record", Record),
            want("set_logger", Function),
            want("set_boxed_logger", Function),
            want("max_level", Function),
        ],
    },
    Entry {
        dir: "memchr-2.7.6",
        name: "memchr",
        version: "2.7.6",
        expect: MEMCHR_API,
    },
    Entry {
        dir: "memchr-2.8.0",
        name: "memchr",
        version: "2.8.0",
        expect: MEMCHR_API,
    },
    Entry {
        dir: "memchr-2.8.3",
        name: "memchr",
        version: "2.8.3",
        expect: MEMCHR_API,
    },
    Entry {
        dir: "nom-5.1.3",
        name: "nom",
        version: "5.1.3",
        // src/internal.rs:13 and :55, src/lib.rs:485 and :489. nom 5 builds
        // most of its surface with `macro_rules!`, so a lowering that never
        // expanded macros would still produce these four and drop everything
        // else — they are the floor, not the ceiling.
        //
        // `be_u128` (src/number/complete.rs:156) is the build-script probe, the
        // same role `set_logger` plays for `log 0.4.17`: nom's `build.rs`
        // emits `cargo:rustc-cfg=stable_i128` on any compiler at or past 1.28,
        // and eight public parsers — `{be,le}_{u,i}128` in both `complete` and
        // `streaming` — are gated on it. They are part of nom 5's API on every
        // toolchain this repository can build with.
        expect: &[
            want("IResult", Alias),
            want("Err", Enum),
            want("combinator", Module),
            want("sequence", Module),
            want("be_u128", Function),
        ],
    },
    Entry {
        dir: "once_cell-1.20.2",
        name: "once_cell",
        version: "1.20.2",
        // src/lib.rs:383 (`pub mod unsync`), :863 (`pub mod sync`, gated on
        // `std`/`critical-section` — active under `CargoFeatures::All`), :411
        // and :714 inside them. `OnceCell` and `Lazy` are each declared twice,
        // once per module; one match is enough for the assertion.
        expect: &[
            want("unsync", Module),
            want("sync", Module),
            want("OnceCell", Record),
            want("Lazy", Record),
        ],
    },
    Entry {
        dir: "parking_lot-0.12.1",
        name: "parking_lot",
        version: "0.12.1",
        // src/condvar.rs:90, src/raw_mutex.rs:32, src/mutex.rs:87,
        // src/lib.rs:28. `Mutex` is an `Alias`, not a `Record`: parking_lot
        // defines it as `pub type Mutex<T> = lock_api::Mutex<RawMutex, T>`.
        // A producer that reported it as a struct would be inventing a
        // definition the crate does not have.
        expect: &[
            want("Condvar", Record),
            want("RawMutex", Record),
            want("Mutex", Alias),
            want("deadlock", Module),
        ],
    },
    Entry {
        dir: "regex-1.10.3",
        name: "regex",
        version: "1.10.3",
        // src/regex/string.rs:101, src/regexset/string.rs:132, src/lib.rs:1344
        // and :1332.
        expect: &[
            want("Regex", Record),
            want("RegexSet", Record),
            want("escape", Function),
            want("bytes", Module),
        ],
    },
    Entry {
        dir: "serde-1.0.196",
        name: "serde",
        version: "1.0.196",
        // src/ser/mod.rs:218 and :333, src/de/mod.rs:535, src/lib.rs:299-300.
        expect: &[
            want("Serialize", Trait),
            want("Serializer", Trait),
            want("Deserialize", Trait),
            want("de", Module),
            want("ser", Module),
        ],
    },
    Entry {
        dir: "serde_json-1.0.113",
        name: "serde_json",
        version: "1.0.113",
        // src/value/mod.rs:116, src/map.rs:26, src/ser.rs:2207,
        // src/lib.rs:407 and :401.
        expect: &[
            want("Value", Enum),
            want("Map", Record),
            want("to_string", Function),
            want("value", Module),
            want("map", Module),
        ],
    },
    Entry {
        dir: "syn-1.0.109",
        name: "syn",
        version: "1.0.109",
        // src/lib.rs:311 and :436 are plain modules. `DeriveInput`
        // (src/derive.rs:9) and `File` (src/file.rs:81) are emitted by syn's
        // own `ast_struct!` macro, so they are only reachable if the lowering
        // expanded declarative macros — a producer that walked syntax alone
        // would pass on the two modules and fail on the two types.
        expect: &[
            want("token", Module),
            want("punctuated", Module),
            want("DeriveInput", Record),
            want("File", Record),
        ],
    },
    Entry {
        dir: "thiserror-1.0.40",
        name: "thiserror",
        version: "1.0.40",
        // src/aserror.rs:4, src/display.rs:4, src/lib.rs:250. thiserror's
        // public face is a proc macro re-exported from `thiserror_impl`, which
        // is a different crate; everything this crate itself declares lives in
        // private modules, which is exactly what `walk::lower_crate`'s
        // unconditional private-declaration rule exists to reach.
        expect: &[
            want("AsDynError", Trait),
            want("DisplayAsDisplay", Trait),
            want("__private", Module),
        ],
    },
    Entry {
        dir: "tracing-0.1.40",
        name: "tracing",
        version: "0.1.40",
        // src/span.rs:348, src/instrument.rs:20, src/lib.rs:975 and :979.
        expect: &[
            want("Span", Record),
            want("Instrument", Trait),
            want("field", Module),
            want("span", Module),
        ],
    },
    Entry {
        dir: "unicode-width-0.1.11",
        name: "unicode-width",
        version: "0.1.11",
        // src/lib.rs:68 and :97, src/tables.rs:15. The package name has a
        // hyphen and the crate name has an underscore; `crate_name_matches`
        // is what has to bridge that, so this entry is also the one that
        // proves it does.
        expect: &[
            want("UnicodeWidthChar", Trait),
            want("UnicodeWidthStr", Trait),
            want("UNICODE_VERSION", Const),
        ],
    },
    Entry {
        dir: "clap-4.5.1",
        name: "clap",
        version: "4.5.1",
        // src/lib.rs:107-115. The `clap` crate itself declares almost nothing
        // but these documentation modules — `Command`, `Arg`, `Parser` and the
        // rest arrive through `pub use clap_builder::*` / `clap_derive::*` and
        // therefore belong to *those* crates, not this one. Asserting on them
        // here would be asserting that the producer lowers a dependency's API
        // into its dependent's table, which is the opposite of what it should
        // do. All five are gated on `unstable-doc`, which `CargoFeatures::All`
        // activates; under a `--no-deps` load every feature evaluates false and
        // all five vanish, so this entry doubles as a degraded-load detector.
        expect: &[
            want("_cookbook", Module),
            want("_derive", Module),
            want("_faq", Module),
            want("_features", Module),
            want("_tutorial", Module),
        ],
    },
    Entry {
        dir: "rayon-1.9.0",
        name: "rayon",
        version: "1.9.0",
        // src/iter/mod.rs:365, src/lib.rs:100, :102, :106.
        expect: &[
            want("ParallelIterator", Trait),
            want("iter", Module),
            want("prelude", Module),
            want("slice", Module),
        ],
    },
];

/// The six symbols every corpus `memchr` must expose.
///
/// `src/memchr.rs:27/92/158/288` and `src/lib.rs:216/220`, identical across
/// 2.7.6, 2.8.0 and 2.8.3 — verified in all three checkouts. `memchr` the
/// function, `memchr` the private module and `memchr` the crate root all share
/// a name, so the `Function` kind is what makes this assertion mean the
/// function; a name-only check would be satisfied by the module.
const MEMCHR_API: &[Want] = &[
    want("memchr", Function),
    want("memchr2", Function),
    want("memchr3", Function),
    want("Memchr", Record),
    want("arch", Module),
    want("memmem", Module),
];

/// Root of the corpus checkout directory, resolved from this crate's manifest
/// so the tests work regardless of the invoking shell's cwd.
///
/// Deliberately not `canonicalize`d: a missing `result/` must surface as
/// a named per-entry failure from the test that looked for it, not as a panic
/// inside a path helper that names no package.
pub fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../result")
}

pub fn entry_root(entry: &Entry) -> PathBuf {
    corpus_root().join(entry.dir)
}

/// Render the whole `std::error::Error` source chain.
///
/// Every terse top-level `Display` in this pipeline hides the part that
/// matters: `ProducerError::OracleSpawn` renders as "oracle spawn failed" and
/// `RustProducerError::Load` as "workspace load failed", while the package
/// that could not resolve, or the crate that was not found, is one or two
/// `source()` hops down.
pub fn chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut cursor = err.source();
    while let Some(source) = cursor {
        out.push_str("\n      caused by: ");
        out.push_str(&source.to_string());
        cursor = source.source();
    }
    out
}
