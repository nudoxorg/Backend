//! L14 — items behind `#[cfg(feature = "…")]` must reach the lowered IR.
//!
//! # Why this exists
//!
//! `workspace/compiler/languages/rust/src/ra/loaded.rs` builds the
//! `CargoConfig` rust-analyzer loads the workspace with. Before the fix this
//! file guards, that config never requested any Cargo feature — rust-analyzer
//! therefore evaluated every `cfg(feature = "…")` as false and pruned the item
//! *before* `lower_workspace` ever walked it. The item is not merely
//! unbadged; it is entirely absent — no entry, no id, unsearchable,
//! unlinkable. See `docs/LIMITATIONS.md` L14.
//!
//! # Two crates, two gated items
//!
//! `serde`'s `rc` feature is not in its `default` set, so `impl Serialize for
//! Weak<T>` (`result/serde-*/src/ser/impls.rs`, `#[cfg(all(feature =
//! "rc", …))]`) is present only when Cargo features are requested at all.
//!
//! `memchr` is the crate L14 was originally diagnosed against
//! (`memmem::FindIter::into_owned`, gated on `alloc`). For a long stretch it
//! could not prove anything, because two unrelated problems kept its dependency
//! graph from resolving at all — first a missing vendored
//! `rustc-std-workspace-core`, then `CargoFeatures::All` activating
//! `rustc-dep-of-std` and substituting an *empty* build of that same shim for
//! `core`. Both are now closed (see the per-test doc on
//! `memchr_alloc_gated_method_reaches_the_lowered_ir` for the full chain), so
//! both tests now assert presence and this file proves the fix on both crates
//! rather than documenting a hole in one of them.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-store --test feature_gated_api -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use nudox_producer::produce;
use nudox_producer_rust::RustProducer;
use nudox_store::{
    package::{PackageView, Provenance},
    source::producer::PackageDescriptor,
};

/// Where `result/<name>-<version>` (or `result/<name>` for the
/// no-version-suffix convention some fixtures use) lives.
fn crate_root(dir_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../result")
        .join(dir_name)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

/// Lower `root` under `name`/`version`, skipping gracefully if the checkout is
/// absent, and printing a `cost case=` line so the run is also a benchmark
/// (docs/AGENTS-DOCTRINE.md §4).
fn try_lower(root: &PathBuf, name: &str, version: &str) -> Option<PackageView> {
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no checkout at {} for {}-{}",
            root.display(),
            name,
            version
        );
        return None;
    }

    let descriptor = PackageDescriptor::cargo(root, name, version);
    let case = format!("lower/{name}-{version}");

    let (table, cost) = nudox_test_support::measured(&case, root, || {
        produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .unwrap_or_else(|err| {
            // Print the whole `#[source]` chain — the top-level Display is
            // deliberately terse and hides exactly the kind of cause this file
            // cares about (a metadata resolution failure vs. a lowering bug).
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                chain.push_str(&format!("\n  caused by: {source}"));
                cursor = source;
            }
            panic!("{name} must lower without error:\n{chain}");
        })
    .table});

    eprintln!(
        "lowered {} entries from {name}-{version} in {:.1}s",
        table.len(),
        cost.wall.as_secs_f64(),
    );

    let view = nudox_ir::view::IrView::with_package(descriptor.lineage.clone(), table);
    Some(PackageView::build(view, Provenance::TrustedLocal))
}

/// `serde::rc::Weak`'s `Serialize` impl — gated `#[cfg(feature = "rc")]`,
/// `rc` is not in serde's `default` feature set — must reach the lowered IR.
///
/// This is the proof the L14 fix works: `serde`'s full dependency graph
/// resolves offline in this checkout (unlike `memchr`'s — see the module
/// doc), so whether `active_features` reflects the crate's non-default
/// features is determined purely by what `CargoConfig.features` requests, not
/// by an unrelated registry gap. Before the fix (default-features-only), no
/// impl entry naming `Weak` exists at all — the whole `impl<T> Serialize for
/// Weak<T>` block is pruned as dead code before the walk sees it. After the
/// fix (`CargoFeatures::All`), it must be present.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace"]
fn serde_rc_gated_impl_reaches_the_lowered_ir() {
    let root = crate_root("serde-1.0.196");
    let Some(package) = try_lower(&root, "serde", "1.0.196") else {
        return;
    };

    let view = package.view();

    // Impl entries are named `impl Trait for Type`; the type reference is
    // rendered into the name, so an entry naming both `Serialize` and `Weak`
    // is unambiguous (RcWeak/ArcWeak are both `std`/`alloc::rc`/`sync::Weak`
    // under a local alias, so `Weak` — not the alias name — is what the
    // rendered self-type carries).
    let weak_serialize_impls: Vec<String> = view
        .entries()
        .filter(|(_, e)| {
            let n = &e.sym().name;
            n.starts_with("impl ") && n.contains("Serialize") && n.contains("Weak")
        })
        .map(|(_, e)| e.sym().name.clone())
        .collect();

    assert!(
        !weak_serialize_impls.is_empty(),
        "no `impl Serialize for Weak<T>` found in the lowered IR. This impl is \
         gated `#[cfg(feature = \"rc\")]`, and `rc` is not in serde's default \
         feature set — its absence is exactly the L14 failure mode: \
         `CargoConfig` requested no non-default Cargo features when loading \
         the workspace, so rust-analyzer pruned the whole impl block as dead \
         code before `lower_workspace` ever walked it. \
         Sample of impl entries actually present: {:?}",
        view.entries()
            .filter(|(_, e)| e.sym().name.starts_with("impl "))
            .map(|(_, e)| e.sym().name.clone())
            .take(15)
            .collect::<Vec<_>>()
    );

    eprintln!(
        "found {} Weak Serialize impl(s): {:?}",
        weak_serialize_impls.len(),
        weak_serialize_impls
    );
}

/// `memchr::memmem::*::into_owned` — gated `#[cfg(feature = "alloc")]` — must
/// reach the lowered IR.
///
/// # This test used to assert the opposite
///
/// It was written as a canary that *expected absence* and panicked if the item
/// ever appeared, because two separate things stopped `memchr` resolving:
///
///  1. `result/memchr-*` had no vendored copy of
///     `rustc-std-workspace-core`, which `memchr` declares as an unconditional
///     optional dependency. `cargo metadata` must resolve every optional
///     dependency's version to build a valid lockfile whether or not a feature
///     activates it, so it failed outright and `ra_ap_project_model` silently
///     retried with `--no-deps` — no `resolve` section, so every feature
///     including `default` evaluated false. The fixtures are now vendored and
///     `cargo metadata --offline` succeeds.
///  2. Once it *did* resolve, `CargoFeatures::All` activated `rustc-dep-of-std`,
///     which pulls that same shim into the crate graph under the name `core`,
///     shadowing the real `core`. For `memchr` 2.7.6, whose lockfile pins the
///     `1.0.0` shim — a literally empty `lib.rs` — that silently destroyed name
///     resolution for the whole package. `loaded.rs` now excludes any feature
///     that would enable a `rustc-std-workspace-*` dependency, so the shim never
///     enters the graph.
///
/// With both closed, `memchr` is no longer the awkward crate this file's module
/// doc once described: it resolves, `alloc` activates, and `into_owned` is
/// present. The canary fired exactly as designed and this is the flip it asked
/// for.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace"]
fn memchr_alloc_gated_method_reaches_the_lowered_ir() {
    let root = crate_root("memchr-2.8.3");
    let Some(package) = try_lower(&root, "memchr", "2.8.3") else {
        return;
    };

    let view = package.view();

    let into_owned_owners: Vec<String> = view
        .entries()
        .filter(|(_, e)| e.sym().name == "into_owned")
        .filter_map(|(id, e)| {
            let parent_name = view
                .parent_of(id)
                .and_then(|p| view.entry(p))
                .map(|pe| pe.sym().name.clone());
            parent_name.map(|p| format!("{p}::{}", e.sym().name))
        })
        .collect();

    assert!(
        !into_owned_owners.is_empty(),
        "no `into_owned` found in memchr's lowered IR. Every one of them is \
         gated `#[cfg(feature = \"alloc\")]`, and `alloc` is not in memchr's \
         default feature set, so an empty result means Cargo features were not \
         active during the load — either `cargo metadata` fell back to \
         `--no-deps` (check for `ra_load metadata_degraded=true` above), or the \
         std-integration feature exclusion in `loaded.rs` over-matched and \
         dropped `alloc` along with `rustc-dep-of-std`. \
         Sample of entries actually present: {:?}",
        view.entries()
            .filter(|(_, e)| e.sym().name.starts_with("impl "))
            .map(|(_, e)| e.sym().name.clone())
            .take(15)
            .collect::<Vec<_>>()
    );

    // Assert on *content*, not just non-emptiness: the `memmem` iterator/finder
    // family is where `into_owned` actually lives, and a hit somewhere else
    // (say `CowBytes`) would satisfy a bare `is_empty` check while the API this
    // test names stayed missing.
    assert!(
        into_owned_owners
            .iter()
            .any(|owner| owner.contains("FindIter") || owner.contains("Finder")),
        "`into_owned` exists but not on memchr's memmem finder/iterator types; \
         found only: {into_owned_owners:?}"
    );

    eprintln!("found {} into_owned: {:?}", into_owned_owners.len(), into_owned_owners);
}
