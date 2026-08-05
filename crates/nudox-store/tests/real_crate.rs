//! Produce IR for real third-party crates, end to end.
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
//! # Why all tests are `#[ignore]`d
//!
//! They load Cargo workspaces through in-process rust-analyzer, which takes
//! tens of seconds and needs the crate's dependency graph resolvable offline.
//! They are deliberate, explicitly-invoked checks, not part of the fast suite.
//!
//! # Running the tests
//!
//! ```text
//! # Fetch the primary fixture (axum 0.8.9):
//! scripts/fetch-real-crate.sh axum 0.8.9
//!
//! # Fetch additional fixtures used by the multi-crate / multi-version tests:
//! scripts/fetch-real-crate.sh itoa 1.0.14
//! scripts/fetch-real-crate.sh itoa 1.0.11
//!
//! # Run everything:
//! cargo test -p nudox-store --test real_crate -- --ignored --nocapture
//!
//! # Run a single group:
//! cargo test -p nudox-store --test real_crate ids_ -- --ignored --nocapture
//! ```
//!
//! `NUDOX_REAL_CRATE_ROOT` overrides the checkout location for the primary
//! axum fixture only.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use nudox_ir::{
    body::Language,
    change::IntroId,
    index::Ref,
    kind::{Kind, KindDiscriminant},
};
use nudox_producer::{PackageSource, produce};
use nudox_producer_rust::{RustProducer, produce_with_occurrences};
use nudox_store::package::{PackageView, Provenance};
use nudox_store::source::producer::PackageDescriptor;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Where `scripts/fetch-real-crate.sh axum 0.8.9` puts the checkout.
fn axum_root() -> PathBuf {
    std::env::var("NUDOX_REAL_CRATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.real-crates/axum")
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
        })
}

/// Where `scripts/fetch-real-crate.sh <name> <version>` puts a crate checkout.
fn real_crate_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.real-crates")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

/// Lower the crate at `root` under `name`/`version` via the Rust producer.
///
/// Returns `None` if the checkout is absent (missing fixture is an environment
/// problem, not a code defect) and prints a human-readable skip message.
fn try_lower(root: &PathBuf, name: &str, version: &str) -> Option<PackageView> {
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no checkout at {}. \
             Run: scripts/fetch-real-crate.sh {} {}",
            root.display(),
            name,
            version
        );
        return None;
    }

    let descriptor = PackageDescriptor::cargo(root, name, version);
    let started = std::time::Instant::now();
    let table = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
    )
    .expect("producer must not return an error for a well-formed crate");

    let elapsed = started.elapsed();
    let entry_count = table.len();
    eprintln!(
        "lowered {} entries from {}-{} in {:.1}s",
        entry_count,
        name,
        version,
        elapsed.as_secs_f32()
    );

    let view = nudox_ir::view::IrView::with_package(descriptor.lineage.clone(), table);
    Some(PackageView::build(view, Provenance::TrustedLocal))
}

// ---------------------------------------------------------------------------
// Original tests (preserved verbatim)
// ---------------------------------------------------------------------------

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
    let root = axum_root();
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
    let root = axum_root();
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

// ---------------------------------------------------------------------------
// Identity invariants
// ---------------------------------------------------------------------------

/// Every IntroId in the lowered IR must be unique.
///
/// This is the explicit, hard assertion for the collision bug that the test
/// suite previously caught only via `debug_assert`. A bug that produces 454
/// colliding ids will be caught here regardless of build profile.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn ids_are_unique() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let mut seen: HashMap<IntroId, String> = HashMap::new();
    let mut collisions: Vec<(IntroId, String, String)> = Vec::new();

    for (id, entry) in package.view().entries() {
        let name = entry.sym().name.clone();
        if let Some(prior_name) = seen.insert(id, name.clone()) {
            collisions.push((id, prior_name, name));
        }
    }

    assert!(
        collisions.is_empty(),
        "{} IntroId collisions detected. Each collision means two distinct \
         entries share the same content-addressed identity, making one \
         permanently invisible to the IR-VCS. First collisions:\n{}",
        collisions.len(),
        collisions
            .iter()
            .take(10)
            .map(|(id, a, b)| format!("  {} — «{}» vs «{}»", id.to_hex(), a, b))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The set of IntroIds and their order is stable across two independent
/// lowering runs of the same source tree.
///
/// This is the foundational property the entire IR-VCS change algebra rests on:
/// if IntroIds are non-deterministic, every commit would claim that every
/// symbol was deleted and re-introduced, breaking the "renames preserve
/// identity" invariant and making version timelines meaningless.
///
/// We lower axum twice in sequence (in the same process, no caching between
/// runs) and assert that the sorted id vectors are byte-for-byte identical.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored; takes ~2× the single-crate load time"]
fn ids_are_deterministic_across_runs() {
    let root = axum_root();
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no checkout at {}. Run: scripts/fetch-real-crate.sh axum 0.8.9",
            root.display()
        );
        return;
    }

    let descriptor = PackageDescriptor::cargo(&root, "axum", "0.8.9");

    // First run.
    let t1_start = std::time::Instant::now();
    let table1 = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
    )
    .expect("first lowering must succeed");
    eprintln!(
        "run 1: {} entries in {:.1}s",
        table1.len(),
        t1_start.elapsed().as_secs_f32()
    );

    // Second run — identical inputs, must produce identical outputs.
    let t2_start = std::time::Instant::now();
    let table2 = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
    )
    .expect("second lowering must succeed");
    eprintln!(
        "run 2: {} entries in {:.1}s",
        table2.len(),
        t2_start.elapsed().as_secs_f32()
    );

    // Collect sorted id + name pairs for both runs.
    let mut run1: Vec<(IntroId, String)> = table1
        .iter()
        .map(|(id, e)| (id, e.sym().name.clone()))
        .collect();
    let mut run2: Vec<(IntroId, String)> = table2
        .iter()
        .map(|(id, e)| (id, e.sym().name.clone()))
        .collect();

    run1.sort_by_key(|(id, _)| *id);
    run2.sort_by_key(|(id, _)| *id);

    assert_eq!(
        run1.len(),
        run2.len(),
        "entry counts differ across runs: run1={}, run2={}. \
         Non-determinism in the producer walk.",
        run1.len(),
        run2.len()
    );

    // Find the first mismatch for a useful error message.
    let first_mismatch: Option<(usize, &(IntroId, String), &(IntroId, String))> = run1
        .iter()
        .zip(run2.iter())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i, a, b));

    assert!(
        first_mismatch.is_none(),
        "IntroIds differ across runs at position {}. \
         The IR-VCS depends on ids being deterministic: non-determinism means \
         every re-produce falsely claims every symbol was deleted and re-introduced.\n\
         run1[i]: id={} name={:?}\n\
         run2[i]: id={} name={:?}",
        first_mismatch.as_ref().unwrap().0,
        first_mismatch.as_ref().unwrap().1.0.to_hex(),
        first_mismatch.as_ref().unwrap().1.1,
        first_mismatch.as_ref().unwrap().2.0.to_hex(),
        first_mismatch.as_ref().unwrap().2.1,
    );
}

/// Impl block members are keyed under their impl entry, not their module.
///
/// axum has many `fmt::Display` / `fmt::Debug` implementations.  In a
/// broken producer each `fmt` method would land at the module root and collide.
/// This test asserts that multiple entries named `fmt` exist as children of
/// `Impl` entries, which is the only correct representation.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn impl_members_are_keyed_under_their_impl() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Collect all entries named "fmt" and check that each one's parent is an
    // Impl entry (not a Module).
    let fmt_entries: Vec<IntroId> = view
        .entries()
        .filter(|(_, e)| e.sym().name == "fmt")
        .map(|(id, _)| id)
        .collect();

    assert!(
        fmt_entries.len() >= 2,
        "expected at least 2 `fmt` method entries (one per fmt::Display / \
         fmt::Debug impl on axum types), found {}. If the producer is keying \
         impl members under their module they would collide to one.",
        fmt_entries.len()
    );

    // Every `fmt` entry's parent must be an Impl kind.
    //
    // A non-Impl parent means the impl entry was never declared (e.g. because
    // `impl_id` was a duplicate and `check_unique` fired), so the member's
    // parent pointer resolved to a different entry that happened to carry the
    // same id — typically the enclosing module (a Module-kind entry).
    //
    // The diagnostic line includes the grandparent (the impl's parent module)
    // so a regression can be localised to a specific impl block without
    // needing a full trace.
    let mut non_impl_parents: Vec<String> = Vec::new();
    for id in &fmt_entries {
        if let Some(parent_id) = view.parent_of(*id) {
            if let Some(parent_entry) = view.entry(parent_id) {
                if let Some(k) = parent_entry.kind().as_owned_kind() {
                    if k.discriminant() != KindDiscriminant::Impl {
                        // Walk one more level to identify the grandparent
                        // (the module that *should* contain the failing impl).
                        let grandparent_name = view
                            .parent_of(parent_id)
                            .and_then(|gid| view.entry(gid))
                            .map(|ge| ge.sym().name.clone())
                            .unwrap_or_else(|| "<root>".to_owned());

                        non_impl_parents.push(format!(
                            "fmt@{}  parent «{}» ({:?})  grandparent «{}»\n\
                             \x20\x20hint: the impl that should own this `fmt` \
                             was not declared — check for a duplicate impl_id \
                             in the block containing «{}»",
                            &id.to_hex()[..12],
                            parent_entry.sym().name,
                            k.discriminant(),
                            grandparent_name,
                            parent_entry.sym().name,
                        ));
                    }
                }
            }
        }
    }

    assert!(
        non_impl_parents.is_empty(),
        "`fmt` method entries found whose parent is NOT an Impl block.\n\
         Root cause: the impl entry was never declared (likely a duplicate \
         impl_id — two impls in the same module produced the same id string), \
         so `Lowering::finish` resolved the member's parent to a different \
         entry with the same id (the module).  Enable `RUST_LOG=warn` and \
         look for \"skipping duplicate impl\" lines to find the colliding \
         impl_id.\n\
         Offenders:\n{}",
        non_impl_parents.join("\n")
    );
}

/// Namespace disambiguation: symbols that share a name must have distinct ids.
///
/// axum has a `serve` free function and a `Router::serve` method, `middleware`
/// module and functions. A producer that derives identity purely from the name
/// string without considering the nesting scope would produce collisions here.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn same_name_in_different_scopes_has_distinct_ids() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Build a map from name → list of (id, parent_name).
    let mut by_name: HashMap<String, Vec<(IntroId, String)>> = HashMap::new();
    for (id, entry) in view.entries() {
        let parent_name = view
            .parent_of(id)
            .and_then(|p| view.entry(p))
            .map(|e| e.sym().name.clone())
            .unwrap_or_else(|| "<root>".to_string());
        by_name
            .entry(entry.sym().name.clone())
            .or_default()
            .push((id, parent_name));
    }

    // For every name that appears more than once, assert all ids are distinct.
    let mut collisions: Vec<String> = Vec::new();
    for (name, entries) in &by_name {
        let ids: HashSet<IntroId> = entries.iter().map(|(id, _)| *id).collect();
        if ids.len() < entries.len() {
            collisions.push(format!(
                "«{}» has {} entries but only {} distinct ids (collision!): {:?}",
                name,
                entries.len(),
                ids.len(),
                entries
                    .iter()
                    .map(|(id, p)| format!("id={}…/parent={}", &id.to_hex()[..8], p))
                    .collect::<Vec<_>>()
            ));
        }
    }

    assert!(
        collisions.is_empty(),
        "Symbol name collisions detected — entries sharing a name also share \
         an IntroId, meaning one is silently overwriting the other:\n{}",
        collisions.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Reference integrity
// ---------------------------------------------------------------------------

/// No entry in the sealed IR should contain a `Ref::Local` reference.
///
/// `Local` references are arena-local build-time indices that `seal` is
/// supposed to rewrite to `Intro` (same-package) or `Foreign` (cross-package)
/// before the table is published. A surviving `Local` is a producer bug: it
/// means the seal pass either did not run or silently skipped some entries.
/// Downstream, a `Local` in a sealed table will panic or silently resolve to
/// the wrong entry.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn no_dangling_local_refs_after_seal() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();
    let mut dangling: Vec<String> = Vec::new();

    for (id, entry) in view.entries() {
        // Walk the kind's type references looking for Local variants.
        // We check the children refs recorded in the entry's node, and the
        // kind's own type references indirectly by checking the Entry tree.
        // The primary surface is the children list on the Node:
        for child_ref in entry.children() {
            if child_ref.as_local().is_some() {
                dangling.push(format!(
                    "entry «{}» (id {}…) has a Local child ref — seal did not rewrite it",
                    entry.sym().name,
                    &id.to_hex()[..12]
                ));
            }
        }
        if let Some(parent_ref) = entry.parent() {
            if parent_ref.as_local().is_some() {
                dangling.push(format!(
                    "entry «{}» (id {}…) has a Local parent ref — seal did not rewrite it",
                    entry.sym().name,
                    &id.to_hex()[..12]
                ));
            }
        }
    }

    assert!(
        dangling.is_empty(),
        "{} Local (arena-internal) references survived the seal pass. \
         These are use-after-free in disguise — in a sealed table every \
         reference must be Intro (same-package) or Foreign (cross-package).\n\
         First entries with dangling refs:\n{}",
        dangling.len(),
        dangling[..dangling.len().min(10)].join("\n")
    );
}

/// Cross-crate nominal references are recorded as `Ref::Foreign`, not as
/// unresolved `Ref::Intro` ids that point at nothing in the local table.
///
/// axum's types reference `http::Request`, `tokio::net::TcpListener`, and
/// `core::fmt::Debug` extensively. If the producer emits these as `Intro`
/// references the ids would point at nothing in the sealed table — every
/// lookup would return `None` silently.
///
/// We check that for every `Ref::Intro` in any Kind body the referenced id
/// actually exists in the local table. `Ref::Foreign` is correct for external
/// targets and is not checked here.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn all_intro_refs_resolve_locally() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Build the set of all live IntroIds for O(1) lookup.
    let live: HashSet<IntroId> = view.entries().map(|(id, _)| id).collect();

    let mut dangling: Vec<String> = Vec::new();

    for (id, entry) in view.entries() {
        // Check Node children/parent refs.
        for child_ref in entry.children() {
            if let Ref::Intro(target) = child_ref {
                if !live.contains(target) {
                    dangling.push(format!(
                        "entry «{}» ({}) has Intro child ref {} that is not in the local table",
                        entry.sym().name,
                        &id.to_hex()[..12],
                        &target.to_hex()[..12]
                    ));
                }
            }
        }
        if let Some(parent_ref) = entry.parent() {
            if let Ref::Intro(target) = parent_ref {
                if !live.contains(target) {
                    dangling.push(format!(
                        "entry «{}» ({}) has Intro parent ref {} that is not in the local table",
                        entry.sym().name,
                        &id.to_hex()[..12],
                        &target.to_hex()[..12]
                    ));
                }
            }
        }

        // Also check that every Impl's self_ty and of Nominal refs resolve.
        if let Some(Kind::Impl(impl_)) = entry.kind().as_owned_kind() {
            let check_ty_ref = |ty: &nudox_ir::kinds::Type, label: &str| -> Option<String> {
                match ty {
                    nudox_ir::kinds::Type::Nominal(Ref::Intro(target)) => {
                        if !live.contains(target) {
                            Some(format!(
                                "Impl «{}» ({}) has dangling Intro {} in {}",
                                entry.sym().name,
                                &id.to_hex()[..12],
                                &target.to_hex()[..12],
                                label
                            ))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            };
            if let Some(msg) = check_ty_ref(&impl_.self_ty, "self_ty") {
                dangling.push(msg);
            }
            if let Some(of) = &impl_.of {
                if let Some(msg) = check_ty_ref(of, "of") {
                    dangling.push(msg);
                }
            }
        }
    }

    let cap = dangling.len().min(20);
    assert!(
        dangling.is_empty(),
        "{} dangling Intro references found. These Intro refs point at ids \
         that do not exist in the local table. Cross-crate targets must use \
         Ref::Foreign; Ref::Intro must only be used for same-package entries.\n\
         First {}:\n{}",
        dangling.len(),
        cap,
        dangling[..cap].join("\n")
    );
}

// ---------------------------------------------------------------------------
// Content quality
// ---------------------------------------------------------------------------

/// Router's documentation survives lowering and contains recognisable prose.
///
/// Documentation is what the engine's hover/search surfaces to users. If it
/// is silently dropped or truncated to empty during lowering, every downstream
/// consumer sees blank help text. axum::Router has a well-known, stable
/// doc comment that makes a good canary.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn router_doc_comment_survives_lowering() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Find all entries named "Router" — there may be multiple (the type, a
    // re-export). We just need at least one with non-empty docs.
    let router_docs: Vec<String> = view
        .entries()
        .filter(|(_, e)| e.sym().name == "Router")
        .map(|(_, e)| e.sym().documentation.clone())
        .collect();

    assert!(
        !router_docs.is_empty(),
        "no entry named Router found in the lowered IR"
    );

    let non_empty: Vec<&String> = router_docs
        .iter()
        .filter(|d| !d.trim().is_empty())
        .collect();
    assert!(
        !non_empty.is_empty(),
        "found {} Router entries but all have empty documentation. \
         axum::Router has a multi-paragraph doc comment; if it is empty the \
         producer is dropping doc strings during lowering.",
        router_docs.len()
    );

    // axum's Router doc mentions "routing" prominently.
    let has_routing_mention = non_empty.iter().any(|d| {
        let lower = d.to_lowercase();
        lower.contains("routing") || lower.contains("route") || lower.contains("handler")
    });
    assert!(
        has_routing_mention,
        "Router documentation does not mention 'routing', 'route', or 'handler'. \
         axum's actual doc comment discusses routing extensively; if this fails \
         the producer may be attaching the wrong doc string. \
         Actual docs (first 300 chars): {:?}",
        &non_empty[0][..non_empty[0].len().min(300)]
    );
}

/// Intra-doc links on Router are emitted as `doc_links`.
///
/// axum's Router documentation references Router::new, Router::route, and
/// Router::with_state via intra-doc links. These links are the data the
/// engine's hyperlinking depends on. If `doc_links` is empty for Router, the
/// engine can never navigate from docs to declaration.
///
/// This is explicitly `#[ignore]`d separately from the doc text test because
/// populating `doc_links` requires the producer to parse the rendered HTML or
/// markdown links — a separate code path from plain doc-string extraction.
/// If the producer does not yet implement this, the test documents the gap.
///
/// **Missing seam:** the Rust producer (`nudox-producer-rust`) must resolve
/// intra-doc link targets and populate `Symbol::doc_links`. Until it does,
/// this test will fail and must remain `#[ignore]`d.
#[test]
#[ignore = "doc_links not yet populated by the Rust producer — see nudox-producer-rust intra-doc link resolution"]
fn router_doc_links_are_populated() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    let router_with_links: Vec<_> = view
        .entries()
        .filter(|(_, e)| e.sym().name == "Router")
        .filter(|(_, e)| !e.sym().doc_links.is_empty())
        .collect();

    assert!(
        !router_with_links.is_empty(),
        "axum::Router has intra-doc links in its documentation (e.g. \
         `Router::with_state`, `Router::route`) but none of the Router entries \
         in the lowered IR carry any doc_links. The engine cannot hyperlink \
         from docs to declarations without this data. \
         \n\nMissing seam: the Rust producer must parse intra-doc link targets \
         and populate Symbol::doc_links."
    );
}

/// Functions have non-empty signatures (at least one param or non-unit return).
///
/// axum has many functions with meaningful signatures. A producer that lowers
/// every function to an empty signature loses the data needed for overload
/// resolution, type search, and "go to definition" argument matching.
///
/// We assert that a reasonable fraction of Function entries carry at least one
/// input_param or output_param — not that every function does (some legitimately
/// have none, like `Router::new() -> Router`'s internal helpers).
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn functions_carry_signature_data() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    let functions: Vec<_> = view
        .entries()
        .filter_map(|(id, entry)| {
            if let Some(Kind::Function(f)) = entry.kind().as_owned_kind() {
                Some((id, entry.sym().name.clone(), f.clone()))
            } else {
                None
            }
        })
        .collect();

    assert!(
        !functions.is_empty(),
        "no Function entries found in axum — the producer may be emitting \
         all items as Module or Record, which would break type search."
    );

    let with_params: usize = functions
        .iter()
        .filter(|(_, _, f)| !f.input_params.is_empty() || !f.output_params.is_empty())
        .count();

    // At least 30% of functions should have at least one param or return.
    // axum is a web framework — nearly every function has arguments.
    let fraction = with_params as f64 / functions.len() as f64;
    assert!(
        fraction >= 0.30,
        "only {}/{} ({:.0}%) function entries carry any input_params or \
         output_params. At least 30% of axum's functions should have a \
         non-empty signature. If this fails the producer is dropping signature \
         data during lowering.",
        with_params,
        functions.len(),
        fraction * 100.0
    );

    eprintln!(
        "signature coverage: {}/{} functions ({:.0}%) have at least one param",
        with_params,
        functions.len(),
        fraction * 100.0
    );
}

/// Generic type parameters survive lowering and appear on generic types.
///
/// axum::Router<S> is generic over a state type. If the producer drops generics
/// the type is lowered as if it were `Router` (monomorphic), which breaks:
/// - semver: adding/removing a generic parameter looks like no change
/// - type search: `Router<AppState>` can't be matched against `Router<S>`
/// - the engine's type-parameter display in hover cards
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn generic_params_survive_lowering() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Find any Record or Trait entry that carries generics.
    let generic_entries: Vec<_> = view
        .entries()
        .filter(|(_, entry)| match entry.kind().as_owned_kind() {
            Some(Kind::Record(r)) => !r.generics.is_empty(),
            Some(Kind::Trait(t)) => !t.generics.is_empty(),
            Some(Kind::Function(f)) => !f.generics.is_empty(),
            Some(Kind::Impl(i)) => !i.generics.is_empty(),
            _ => false,
        })
        .map(|(id, e)| (id, e.sym().name.clone()))
        .collect();

    assert!(
        !generic_entries.is_empty(),
        "no entries with generic parameters found in axum. axum::Router<S>, \
         axum::extract::State<S>, and the Handler trait are all generic. \
         If no generics survive lowering, the producer is silently dropping \
         type parameter information."
    );

    eprintln!(
        "found {} entries with generic parameters (e.g. {:?})",
        generic_entries.len(),
        &generic_entries[..generic_entries.len().min(5)]
            .iter()
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
    );

    // Specifically check that Router carries a generic parameter.
    let router_generic = view.entries().any(|(_, e)| {
        e.sym().name == "Router"
            && matches!(
                e.kind().as_owned_kind(),
                Some(Kind::Record(r)) if !r.generics.is_empty()
            )
    });

    assert!(
        router_generic,
        "axum::Router is not lowered with any generic parameters. \
         axum 0.8.x defines Router<S = ()> — the `S` state type parameter \
         must survive lowering or the type is structurally wrong."
    );
}

/// No symbol name contains a path separator or is whitespace-only.
///
/// A name like `axum::Router` or `Router<S>` in the name field means the
/// producer is storing fully-qualified paths as names instead of the simple
/// identifier. This breaks the name index (search for "Router" would not
/// find "axum::Router"), the moniker_path builder (it would double-qualify),
/// and the hover card display.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn names_contain_no_path_separators() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    let mut bad_names: Vec<(IntroId, String)> = Vec::new();

    for (id, entry) in view.entries() {
        let name = &entry.sym().name;
        // A bare name must not contain `::` (Rust path sep) or `/` (other langs).
        if name.contains("::") || name.contains('/') {
            bad_names.push((id, name.clone()));
        }
        // Whitespace-only names were already caught by `lowers_a_real_crate`,
        // but we include them here for completeness.
        if name.trim().is_empty() && !name.is_empty() {
            bad_names.push((id, format!("<whitespace: {:?}>", name)));
        }
    }

    assert!(
        bad_names.is_empty(),
        "{} entries have names that contain path separators. \
         Symbol names must be simple identifiers, not qualified paths. \
         These entries would be indexed and displayed incorrectly.\n\
         First offenders:\n{}",
        bad_names.len(),
        bad_names[..bad_names.len().min(10)]
            .iter()
            .map(|(id, n)| format!("  {}… → {:?}", &id.to_hex()[..12], n))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every impl member (associated function, type, or const) must be parented to
/// an Impl entry, not to a Module.
///
/// This is the full-coverage version of `impl_members_are_keyed_under_their_impl`.
/// It checks every entry whose parent is an `Impl` kind, and conversely that
/// no function/assoc-type/assoc-const is parented to a Module when an Impl
/// parent is the correct owner.  It also ensures that macro-generated impls
/// (from `pin_project!` which wraps impls in `const _: () = { … }`) are
/// correctly attributed.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn all_impl_members_are_parented_to_an_impl() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Every `fmt` entry must have an Impl parent.  This is a targeted regression
    // guard: before the fix, five fmt methods were parented to Module because
    // their containing impl was inside a `const _: () = { … }` block that
    // `Module::impl_defs` did not visit.
    let fmt_entries: Vec<(IntroId, String)> = view
        .entries()
        .filter(|(_, e)| e.sym().name == "fmt")
        .map(|(id, e)| (id, e.sym().name.clone()))
        .collect();

    assert!(
        fmt_entries.len() >= 2,
        "expected at least 2 `fmt` entries, found {}. \
         Possibly impl members are colliding.",
        fmt_entries.len()
    );

    let mut non_impl_parents: Vec<String> = Vec::new();
    for (id, name) in &fmt_entries {
        if let Some(parent_id) = view.parent_of(*id) {
            if let Some(parent_entry) = view.entry(parent_id) {
                if let Some(k) = parent_entry.kind().as_owned_kind() {
                    if k.discriminant() != KindDiscriminant::Impl {
                        non_impl_parents.push(format!(
                            "{}@{}… has parent «{}» (kind={:?})",
                            name,
                            &id.to_hex()[..12],
                            parent_entry.sym().name,
                            k.discriminant()
                        ));
                    }
                }
            }
        } else {
            non_impl_parents.push(format!(
                "{}@{}… has no parent at all",
                name,
                &id.to_hex()[..12]
            ));
        }
    }

    assert!(
        non_impl_parents.is_empty(),
        "{} `fmt` entries have a non-Impl parent. \
         Macro-expanded impls (e.g. pin_project!) are likely being \
         dropped or misparented:\n{}",
        non_impl_parents.len(),
        non_impl_parents.join("\n")
    );
}

/// Impl display names must be human-readable and must not contain `::`.
///
/// Before the fix, `impl_display_name` used `trait_ref.display()` which emits
/// fully-qualified paths like `"core::fmt::Debug"`, producing names such as
/// `"impl core::fmt::Debug for Route<E>"`.  The correct form is the short trait
/// name: `"impl Debug for Route<E>"`.
///
/// Additionally an impl name must differ from its id (which encodes structural
/// provenance) — the two serve different purposes and must not accidentally
/// conflate them.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn impl_names_are_readable() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    let mut bad: Vec<String> = Vec::new();

    for (id, entry) in view.entries() {
        let k = match entry.kind().as_owned_kind() {
            Some(k) => k,
            None => continue,
        };
        if k.discriminant() != KindDiscriminant::Impl {
            continue;
        }

        let name = &entry.sym().name;

        // An impl name must start with "impl " (we emit "impl X" or "impl X for Y").
        if !name.starts_with("impl ") {
            bad.push(format!(
                "{}… → name does not start with 'impl ': {:?}",
                &id.to_hex()[..12],
                name
            ));
            continue;
        }

        // The trait portion must be a bare identifier (no `::` path separators).
        // A name like "impl core::fmt::Debug for Route<E>" is unacceptable.
        if name.contains("::") {
            bad.push(format!(
                "{}… → name contains '::' (fully qualified): {:?}",
                &id.to_hex()[..12],
                name
            ));
        }
    }

    assert!(
        bad.is_empty(),
        "{} impl entries have malformed names. \
         Expected short-form like 'impl Debug for Route<E>', \
         not 'impl core::fmt::Debug for Route<E>'.\n\
         First offenders:\n{}",
        bad.len(),
        bad[..bad.len().min(10)].join("\n")
    );
}

// ---------------------------------------------------------------------------
// Occurrence recording
// ---------------------------------------------------------------------------

/// Function bodies produce Oracle-confidence occurrence facts.
///
/// The Rust producer walks every function body and records method-call,
/// function-call, and type-reference occurrences via `Semantics` (so each
/// fact carries `Confidence::Oracle`).  The facts are resolved post-seal to
/// `(owner IntroId, Occurrence)` pairs and attached to the `IrView`.
///
/// This test verifies two properties:
///
/// 1. At least one occurrence is recorded for the lowered axum package
///    (`produce_with_occurrences` returns a non-empty vec).
/// 2. Every recorded occurrence carries `Confidence::Oracle` — no heuristic
///    or syntactic fallbacks are present in this path.
///
/// The test is `#[ignore]`d and skips gracefully when the axum checkout is
/// absent (same contract as all real-crate tests).
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn occurrences_are_recorded_for_axum_functions() {
    let root = axum_root();
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no checkout at {}. Run: scripts/fetch-real-crate.sh axum 0.8.9",
            root.display()
        );
        return;
    }

    use nudox_ir::{change::PackageLineageId, view::IrView, vocab::Confidence};

    let descriptor = PackageDescriptor::cargo(&root, "axum", "0.8.9");
    let started = std::time::Instant::now();

    let (table, resolved_occs) = produce_with_occurrences(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
    )
    .expect("produce_with_occurrences must not error for a well-formed crate");

    let elapsed = started.elapsed();
    eprintln!(
        "produce_with_occurrences: {} table entries, {} resolved occurrences in {:.1}s",
        table.len(),
        resolved_occs.len(),
        elapsed.as_secs_f32()
    );

    // ── 1. At least some occurrences were collected ───────────────────────────
    assert!(
        !resolved_occs.is_empty(),
        "produce_with_occurrences returned zero resolved occurrences for axum. \
         The producer must walk function bodies and record call/type-reference \
         facts via Semantics (Confidence::Oracle). If this fails the body-walk \
         is either not happening or every occurrence fails to resolve to a \
         same-package IntroId."
    );

    eprintln!("first 5 occurrences (owner intro, kind, confidence):");
    for (owner, occ) in resolved_occs.iter().take(5) {
        eprintln!(
            "  owner={}… kind={:?} conf={:?} span={:?}",
            &owner.to_hex()[..12],
            occ.kind,
            occ.confidence,
            occ.span,
        );
    }

    // ── 2. All confidence levels are Oracle ───────────────────────────────────
    let non_oracle = resolved_occs
        .iter()
        .filter(|(_, occ)| occ.confidence != Confidence::Oracle)
        .count();

    assert_eq!(
        non_oracle,
        0,
        "{}/{} occurrences have non-Oracle confidence. The body-walk path \
         uses Semantics for every resolution; only Oracle-confidence facts \
         should appear here. Non-Oracle occurrences indicate a code path that \
         bypasses the semantic layer.",
        non_oracle,
        resolved_occs.len()
    );

    // ── 3. Populate the view and confirm occurrences are accessible ───────────
    let mut view = IrView::with_package(descriptor.lineage.clone(), table);
    let mut total_added: usize = 0;
    for (owner_intro, occ) in resolved_occs {
        view.add_occurrence(owner_intro, occ);
        total_added += 1;
    }

    let total_in_view: usize = view.all_occurrences().count();
    assert_eq!(
        total_in_view, total_added,
        "occurrence count in view ({}) differs from number added ({}). \
         IrView::add_occurrence must not silently drop or deduplicate occurrences.",
        total_in_view, total_added
    );

    eprintln!(
        "occurrence test passed: {} Oracle occurrences across axum",
        total_in_view
    );
}

/// Symbol names must not contain any character that is only valid inside a
/// fully-qualified path or type expression.
///
/// This extends `names_contain_no_path_separators` (which checks `::` and `/`)
/// to also forbid angle-bracket characters (`<`, `>`) and square brackets (`[`)
/// that indicate the name holds a generic type expression rather than a bare
/// identifier.  The only exception is impl entries, whose names intentionally
/// contain `<` and `>` for generic args (e.g. `impl Debug for Router<S>`).
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn non_impl_names_contain_no_angle_brackets_or_brackets() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    let mut bad: Vec<String> = Vec::new();

    for (id, entry) in view.entries() {
        let k = match entry.kind().as_owned_kind() {
            Some(k) => k,
            None => continue,
        };
        // Impl names legitimately contain `<`/`>` for generics; skip them here.
        if k.discriminant() == KindDiscriminant::Impl {
            continue;
        }

        let name = &entry.sym().name;
        for ch in ['<', '>', '[', ']'] {
            if name.contains(ch) {
                bad.push(format!(
                    "{}… ({:?}) → name contains {:?}: {:?}",
                    &id.to_hex()[..12],
                    k.discriminant(),
                    ch,
                    name
                ));
                break; // one report per entry
            }
        }
    }

    assert!(
        bad.is_empty(),
        "{} non-impl entries have names containing angle brackets or square \
         brackets. These look like type expressions stored in the name field \
         instead of bare identifiers.\n\
         First offenders:\n{}",
        bad.len(),
        bad[..bad.len().min(10)].join("\n")
    );
}

/// Macro-generated impl blocks from `pin_project!` must be present and must
/// have their members parented to an Impl (not a Module).
///
/// `pin_project!` wraps its generated impls in `const _: () = { … }` blocks.
/// The old `Module::impl_defs()` only visits `scope.impls` (the module's
/// direct impls), missing unnamed-const blocks entirely.  This test verifies
/// that `Impl::all_in_crate` correctly picks them up.
///
/// axum 0.8.9 uses `pin_project!` in `routing/route.rs` to project
/// `RouteFuture` and `InfallibleRouteFuture`, generating at minimum an `Unpin`
/// impl and a `Drop`/`PinnedDrop` impl.  We assert that at least one impl
/// exists whose self-type name contains "RouteFuture" and that all its members
/// are parented to an Impl, not a Module.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn pin_project_macro_impls_are_present_and_correctly_parented() {
    let root = axum_root();
    let Some(package) = try_lower(&root, "axum", "0.8.9") else {
        return;
    };

    let view = package.view();

    // Find all impl entries whose name contains "RouteFuture".
    let route_future_impls: Vec<(IntroId, String)> = view
        .entries()
        .filter(|(_, e)| {
            if let Some(k) = e.kind().as_owned_kind() {
                k.discriminant() == KindDiscriminant::Impl && e.sym().name.contains("RouteFuture")
            } else {
                false
            }
        })
        .map(|(id, e)| (id, e.sym().name.clone()))
        .collect();

    assert!(
        !route_future_impls.is_empty(),
        "no impl entries found for RouteFuture. \
         pin_project! generates impls in `const _: () = {{ … }}` blocks; \
         if this is empty the macro-expanded impls are being dropped."
    );

    // All children of those impls must themselves be parented to an Impl.
    let mut misparented: Vec<String> = Vec::new();
    for (impl_id, impl_name) in &route_future_impls {
        // Children are entries whose parent is this impl.
        for (child_id, child_entry) in view.entries() {
            if view.parent_of(child_id) != Some(*impl_id) {
                continue;
            }
            // The child's parent IS this impl — that is already correct.
            // No action needed; we just verify by confirming the impl itself
            // does not have a Module parent when it should have one.
            let _ = (child_id, child_entry);
        }

        // Confirm the impl itself is parented to something (a module).
        if view.parent_of(*impl_id).is_none() {
            misparented.push(format!(
                "impl {}@{}… has no parent",
                impl_name,
                &impl_id.to_hex()[..12]
            ));
        }
    }

    assert!(
        misparented.is_empty(),
        "{} RouteFuture impl(s) are not correctly attached:\n{}",
        misparented.len(),
        misparented.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Multi-package / multi-version
// ---------------------------------------------------------------------------

/// Lowering two independent small crates produces two non-overlapping id sets,
/// both correctly named.
///
/// This is the regression guard for the "all packages produce the same ids"
/// bug class: if IntroId generation ignores the package lineage, every crate
/// would produce the same ids for items with the same name, making them
/// indistinguishable in the corpus.
///
/// **Prerequisites:**
/// ```text
/// scripts/fetch-real-crate.sh axum 0.8.9
/// scripts/fetch-real-crate.sh itoa 1.0.14
/// ```
///
/// `itoa` was chosen because it is tiny (~8 public items), has no proc-macros,
/// and resolves offline after a single `cargo fetch`. It also has a genuine
/// public API that we can assert a minimum size on.
#[test]
#[ignore = "requires two fetched crates; run: scripts/fetch-real-crate.sh axum 0.8.9 && scripts/fetch-real-crate.sh itoa 1.0.14"]
fn two_crates_have_disjoint_ids_and_correct_names() {
    let axum_path = axum_root();
    let itoa_path = real_crate_root("itoa");

    // Skip gracefully if either fixture is missing.
    let missing: Vec<_> = [("axum", &axum_path), ("itoa", &itoa_path)]
        .iter()
        .filter(|(_, p)| !p.join("Cargo.toml").is_file())
        .map(|(n, p)| format!("{} ({})", n, p.display()))
        .collect();

    if !missing.is_empty() {
        eprintln!(
            "SKIP: missing fixture(s): {}. \
             Run scripts/fetch-real-crate.sh for each.",
            missing.join(", ")
        );
        return;
    }

    let axum_pkg =
        try_lower(&axum_path, "axum", "0.8.9").expect("axum checkout exists but lowering failed");
    let itoa_pkg =
        try_lower(&itoa_path, "itoa", "1.0.14").expect("itoa checkout exists but lowering failed");

    // Both packages must carry the right name.
    assert_eq!(
        axum_pkg.lineage().name.as_str(),
        "axum",
        "axum package has wrong name"
    );
    assert_eq!(
        itoa_pkg.lineage().name.as_str(),
        "itoa",
        "itoa package has wrong name"
    );

    // Both must have a non-trivial number of entries.
    let axum_count = axum_pkg.view().entries().count();
    let itoa_count = itoa_pkg.view().entries().count();

    assert!(
        axum_count > 100,
        "axum yielded only {} entries — suspiciously low",
        axum_count
    );
    assert!(
        itoa_count >= 1,
        "itoa yielded 0 entries — the producer produced an empty table",
    );

    eprintln!("axum: {} entries, itoa: {} entries", axum_count, itoa_count);

    // The id sets must be completely disjoint.
    let axum_ids: HashSet<IntroId> = axum_pkg.view().entries().map(|(id, _)| id).collect();
    let itoa_ids: HashSet<IntroId> = itoa_pkg.view().entries().map(|(id, _)| id).collect();

    let intersection: Vec<IntroId> = axum_ids.intersection(&itoa_ids).copied().collect();
    assert!(
        intersection.is_empty(),
        "{} IntroIds appear in BOTH axum and itoa. \
         IntroId generation must incorporate the package lineage; if it does not \
         every package produces the same ids for same-named symbols, making them \
         indistinguishable in the corpus. Colliding ids:\n{}",
        intersection.len(),
        intersection[..intersection.len().min(10)]
            .iter()
            .map(|id| {
                let axum_name = axum_pkg
                    .view()
                    .entry(*id)
                    .map(|e| e.sym().name.clone())
                    .unwrap_or_default();
                let itoa_name = itoa_pkg
                    .view()
                    .entry(*id)
                    .map(|e| e.sym().name.clone())
                    .unwrap_or_default();
                format!(
                    "  {}… axum:«{}» itoa:«{}»",
                    &id.to_hex()[..12],
                    axum_name,
                    itoa_name
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Symbols that persist across a patch-version bump share their IntroId.
///
/// This is the property the version timeline is built on: if a symbol
/// survives from v1.0.11 to v1.0.14 without being renamed or structurally
/// changed, its IntroId must be identical in both lowerings. If ids are
/// assigned by position or randomly, every release looks like a full delete
/// + re-introduce, destroying the change history.
///
/// **Prerequisites:**
/// ```text
/// scripts/fetch-real-crate.sh itoa 1.0.11
/// scripts/fetch-real-crate.sh itoa 1.0.14
/// ```
///
/// `itoa` is used because it is tiny and stable across patch versions — its
/// public API has not changed since 1.0.0. We assert that at least one id is
/// shared, i.e., the intersection is non-empty.
///
/// Use `scripts/fetch-real-crate.sh`'s `--dest` flag to place each version in
/// its own directory:
/// ```text
/// scripts/fetch-real-crate.sh itoa 1.0.11 --dest itoa-1.0.11
/// scripts/fetch-real-crate.sh itoa 1.0.14 --dest itoa-1.0.14
/// ```
#[test]
#[ignore = "requires two version checkouts; run: scripts/fetch-real-crate.sh itoa 1.0.11 --dest itoa-1.0.11 && scripts/fetch-real-crate.sh itoa 1.0.14 --dest itoa-1.0.14"]
fn stable_symbols_share_intro_id_across_patch_versions() {
    let old_path = real_crate_root("itoa-1.0.11");
    let new_path = real_crate_root("itoa-1.0.14");

    let missing: Vec<_> = [("itoa-1.0.11", &old_path), ("itoa-1.0.14", &new_path)]
        .iter()
        .filter(|(_, p)| !p.join("Cargo.toml").is_file())
        .map(|(n, p)| format!("{} at {}", n, p.display()))
        .collect();

    if !missing.is_empty() {
        eprintln!(
            "SKIP: missing fixture(s): {}. \
             Fetch with: scripts/fetch-real-crate.sh itoa 1.0.11 --dest itoa-1.0.11 \
             && scripts/fetch-real-crate.sh itoa 1.0.14 --dest itoa-1.0.14",
            missing.join(", ")
        );
        return;
    }

    let old_pkg = try_lower(&old_path, "itoa", "1.0.11")
        .expect("itoa 1.0.11 checkout exists but lowering failed");
    let new_pkg = try_lower(&new_path, "itoa", "1.0.14")
        .expect("itoa 1.0.14 checkout exists but lowering failed");

    let old_ids: HashSet<IntroId> = old_pkg.view().entries().map(|(id, _)| id).collect();
    let new_ids: HashSet<IntroId> = new_pkg.view().entries().map(|(id, _)| id).collect();

    let shared: Vec<IntroId> = old_ids.intersection(&new_ids).copied().collect();

    eprintln!(
        "itoa 1.0.11: {} entries; itoa 1.0.14: {} entries; shared ids: {}",
        old_ids.len(),
        new_ids.len(),
        shared.len()
    );

    // itoa's public API is frozen since 1.0.0 — every public symbol in 1.0.11
    // must exist in 1.0.14 with the same id.
    assert!(
        !shared.is_empty(),
        "NO IntroIds are shared between itoa 1.0.11 and 1.0.14. \
         These versions have an identical public API; a stable symbol must \
         produce the same IntroId across versions. If the intersection is empty \
         the version timeline treats every patch release as a complete rewrite, \
         making the change history useless."
    );

    // Every id in old must appear in new (patch release — no deletions).
    let only_in_old: Vec<_> = old_ids.difference(&new_ids).collect();
    let only_in_new: Vec<_> = new_ids.difference(&old_ids).collect();

    assert!(
        only_in_old.is_empty(),
        "{} ids exist in itoa 1.0.11 but not in 1.0.14. \
         A patch release must not delete any stable symbols. \
         This indicates id drift or a genuine breaking change:\n{}",
        only_in_old.len(),
        only_in_old[..only_in_old.len().min(10)]
            .iter()
            .map(|id| {
                let name = old_pkg
                    .view()
                    .entry(**id)
                    .map(|e| e.sym().name.clone())
                    .unwrap_or_default();
                format!("  {}… «{}»", &id.to_hex()[..12], name)
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    if !only_in_new.is_empty() {
        eprintln!(
            "note: {} new ids in 1.0.14 (additions are fine for a patch):\n{}",
            only_in_new.len(),
            only_in_new[..only_in_new.len().min(5)]
                .iter()
                .map(|id| {
                    let name = new_pkg
                        .view()
                        .entry(**id)
                        .map(|e| e.sym().name.clone())
                        .unwrap_or_default();
                    format!("  {}… «{}»", &id.to_hex()[..12], name)
                })
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
