//! Regression test for docs/LIMITATIONS.md L4: a `[[bench]]` target that shares its
//! package's crate name must not stop the package from lowering.
//!
//! # Why this exists
//!
//! `bytes-1.11.0`'s `Cargo.toml` names its `benches/bytes.rs` target `"bytes"`
//! — identical to its `[lib]` target. rust-analyzer's crate graph therefore
//! contains two distinct `Crate`s that both answer `"bytes"` to
//! `display_name`. The producer's crate-selection walk
//! (`ra::mod::find_local_crates`) matches purely by that name, so it used to
//! walk both:
//!
//! * the library declares its own root module at id `"bytes"` and a genuine
//!   `Reexport` at `"bytes::Bytes"` (from `pub use crate::bytes::Bytes;`);
//! * the bench crate's root module *also* computes to id `"bytes"` (its
//!   display name is identical), and its plain `use bytes::Bytes;` gets swept
//!   into a bogus re-export at the same `"bytes::Bytes"` id by
//!   `lower_module`'s scope scan.
//!
//! Both id collisions bypass `id_of`/`check_unique` — not because either
//! declare site forgot to call it (every declare site in `item.rs` does), but
//! because the *second* declare happens in an entirely separate `LowerCtx`
//! (one per crate walked), so the duplicate is only ever visible to
//! `Lowering::finish`, which reported `Duplicate(["bytes", "bytes::Bytes"])`
//! and refused to lower the crate at all.
//!
//! The fix (`ra::walk::lower_crate`) skips walking any crate that depends on
//! another crate sharing its own display name: Rust forbids a crate from
//! depending on itself, so that signal can only describe a bench/test/example
//! target riding on the library's name, never the library itself.
//!
//! # What this test proves beyond "it no longer panics"
//!
//! A fix that caught the duplicate-declare error and silently dropped the
//! *second* declaration (rather than skipping the whole spurious crate
//! upstream) would also make this crate lower — but it would do so by luck of
//! declaration order, and it would leave the bench file's own items
//! (`#[bench] fn deref_unique`, …) declared as children of `bytes`'s real root
//! module, indistinguishable from the crate's genuine public API. This test
//! therefore asserts on content, not just success: `Bytes` must be present
//! exactly once, and no bench-only symbol may be present at all.

use std::path::PathBuf;

use nudox_ir::kind::Kind;
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::producer::PackageDescriptor;

/// Where `scripts/fetch-real-crate.sh bytes 1.11.0` (or this repo's
/// `result/` checkout) puts the checkout. `NUDOX_PKG_ROOT` overrides it,
/// matching the convention `real_package.rs` uses.
fn bytes_root() -> PathBuf {
    std::env::var("NUDOX_PKG_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../result/bytes-1.11.0")
        })
}

/// Symbols that exist *only* in `benches/bytes.rs` — the target whose crate
/// name collides with the library's. Their presence in the lowered package
/// would mean the bench crate was walked instead of skipped.
const BENCH_ONLY_NAMES: &[&str] = &["deref_unique", "deref_shared", "deref_static"];

#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace"]
fn bytes_lowers_despite_its_bench_target_matching_the_crate_name() {
    let root = bytes_root();
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no checkout at {}. Run: scripts/fetch-real-crate.sh bytes 1.11.0",
            root.display(),
        );
        return;
    }

    let descriptor = PackageDescriptor::cargo(&root, "bytes", "1.11.0");
    let case = "lower/bytes-1.11.0-bench-name-collision";

    let (view, cost) = heart::cost::measured(case, &root, || {
        let table = produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .unwrap_or_else(|err| {
            // Print the whole `#[source]` chain — the top-level Display of a
            // ProducerError is deliberately terse, and for this specific bug
            // the interesting text (`declared more than once: [...]`) lives
            // one level down.
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                chain.push_str(&format!("\n  caused by: {source}"));
                cursor = source;
            }
            panic!(
                "bytes must lower despite its bench target sharing its crate \
                 name (docs/LIMITATIONS.md L4):\n{chain}"
            );
        }).table;

        let ir = nudox_ir::view::IrView::with_package(descriptor.lineage.clone(), table);
        PackageView::build(ir, Provenance::TrustedLocal)
    });

    let entries: Vec<_> = view.view().entries().collect();

    // `Bytes` — the crate's headline type — must be declared, and declared
    // exactly once. Two would mean the duplicate survived under a different
    // id scheme; zero would mean the fix over-corrected and dropped the real
    // declaration along with the spurious one.
    let bytes_structs: Vec<_> = entries
        .iter()
        .filter(|(_, entry)| {
            entry.sym().name == "Bytes"
                && matches!(entry.kind().as_owned_kind(), Some(Kind::Record(_)))
        })
        .collect();
    assert_eq!(
        bytes_structs.len(),
        1,
        "expected exactly one `Bytes` struct entry, found {}: {:?}",
        bytes_structs.len(),
        bytes_structs
            .iter()
            .map(|(intro, _)| *intro)
            .collect::<Vec<_>>(),
    );

    for (intro, entry) in &entries {
        assert!(
            !BENCH_ONLY_NAMES.contains(&entry.sym().name.as_str()),
            "found bench-only symbol {:?} ({intro:?}) in the lowered package — \
             the bench target that shares bytes' crate name was walked instead \
             of skipped",
            entry.sym().name,
        );
    }

    eprintln!(
        "lowered {} entries from bytes-1.11.0 in {:.1}s (1 `Bytes` struct, 0 bench leaks)",
        entries.len(),
        cost.wall.as_secs_f64(),
    );
}
