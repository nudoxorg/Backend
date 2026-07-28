//! Produce IR for a real third-party crate, end to end.
//!
//! # Why this test exists
//!
//! Every other test in this workspace runs against `FixtureSource`, a corpus we
//! wrote ourselves. That is the right default — it is fast, deterministic, and
//! it exercises every IR variant on purpose. But it shares an author with the
//! code it tests, so it cannot tell us whether the producer survives contact
//! with a crate nobody wrote for us: real generics, real trait bounds, real
//! `cfg`, real macro output, real re-exports, thousands of items.
//!
//! It is also the only test that would have caught the bug that
//! `ProducerRegistry::with_rust_pilot` registered `RustProducer` with an empty
//! package name, which made *every* package produced through the registry fail.
//! The fixture path never goes through a producer at all, so the whole
//! production pipeline was untested end to end.
//!
//! # Why it is `#[ignore]`d
//!
//! It loads a Cargo workspace through in-process rust-analyzer, which takes
//! tens of seconds and needs the crate's dependency graph resolvable offline.
//! It is a deliberate, explicitly-invoked check, not part of the fast suite:
//!
//! ```text
//! scripts/fetch-real-crate.sh axum 0.8.9
//! cargo test -p nudox-store --test real_crate -- --ignored --nocapture
//! ```
//!
//! `NUDOX_REAL_CRATE_ROOT` overrides the checkout location.

use std::path::PathBuf;

use nudox_ir::body::Language;
use nudox_producer::{PackageSource, produce};
use nudox_producer_rust::RustProducer;
use nudox_store::package::{PackageView, Provenance};
use nudox_store::source::producer::PackageDescriptor;

/// Where `scripts/fetch-real-crate.sh` puts the checkout.
fn crate_root() -> PathBuf {
    std::env::var("NUDOX_REAL_CRATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.real-crates/axum")
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
        })
}

/// The Rust producer must lower a real crate into a populated IR table.
///
/// The assertions are deliberately about *shape a reader would notice*, not
/// about counts that drift with every axum release:
///
/// * the table is not empty — the failure mode this catches is a producer that
///   "succeeds" with zero entries, which downstream looks exactly like a
///   package that simply has no public API;
/// * `Router` is present — axum's single most recognisable export. If the walk
///   silently skipped re-exports or `pub use`, this is what would go missing;
/// * names are non-empty — an entry with no name is unsearchable and renders as
///   a blank row, which is the "we're rendering nothing real" failure.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn lowers_a_real_crate() {
    let root = crate_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "no crate checkout at {}. Run scripts/fetch-real-crate.sh first.",
        root.display()
    );

    let descriptor = PackageDescriptor::cargo(&root, "axum", "0.8.9");
    assert_eq!(
        descriptor.language,
        Language::Rust,
        "a Cargo descriptor must route to the Rust producer"
    );

    let started = std::time::Instant::now();
    let table = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
    )
    .expect("axum must lower without error");
    let elapsed = started.elapsed();

    let view = nudox_ir::view::IrView::with_package(descriptor.lineage.clone(), table);
    let package = PackageView::build(view, Provenance::TrustedLocal);

    let names: Vec<String> = package
        .view()
        .entries()
        .map(|(_, entry)| entry.sym().name.to_string())
        .collect();

    eprintln!(
        "lowered {} entries from axum in {:.1}s",
        names.len(),
        elapsed.as_secs_f32()
    );

    assert!(
        names.len() > 200,
        "axum lowered only {} entries — a real crate this size should yield \
         hundreds. A producer that 'succeeds' with almost nothing is \
         indistinguishable downstream from a package with no public API.",
        names.len()
    );

    assert!(
        names.iter().any(|n| n == "Router"),
        "axum::Router is missing from the lowered IR. It is the crate's most \
         recognisable export; if it is absent the walk is dropping items (most \
         likely re-exports). Sample of what we did get: {:?}",
        &names[..names.len().min(20)]
    );

    let blank = names.iter().filter(|n| n.trim().is_empty()).count();
    assert_eq!(
        blank, 0,
        "{blank} entries have empty names. These are unsearchable and render \
         as blank rows — the exact 'looks populated but is not' failure."
    );
}

/// The name the producer lowers under must be the one the caller asked for.
///
/// This is the regression guard for the empty-name bug: `RustProducer` no
/// longer holds a name at all, so the only name available is the one on the
/// `PackageSource`. If someone reintroduces a producer-side name field, this
/// fails rather than silently producing a package called "".
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn the_lowered_package_is_named_after_its_source() {
    let root = crate_root();
    assert!(root.join("Cargo.toml").is_file(), "no crate checkout");

    let source = PackageSource::new(&root, "axum", "0.8.9");
    let lineage = PackageDescriptor::cargo(&root, "axum", "0.8.9").lineage;

    let table = produce(&RustProducer { direct_repo: false }, &source, &lineage)
        .expect("axum must lower without error");
    let view = nudox_ir::view::IrView::with_package(lineage.clone(), table);

    assert_eq!(
        view.package().name.as_str(),
        "axum",
        "the lowered package must carry the name its PackageSource declared"
    );
}
