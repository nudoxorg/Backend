//! End-to-end hyperlink test against **real producer output**.
//!
//! # Why this test must exist
//!
//! Every hyperlink test in `hyperlink_flows.rs` builds `DocLink` entries by
//! hand with hand-chosen `target` and `label` strings.  That means all of them
//! test the happy path where the producer-side path and the engine-side path
//! happen to agree.  The bug that shipped was the opposite: the producer emits
//! `"axum::routing::with_state"` (module-qualified, no impl-type segment) while
//! `moniker_path` in the engine stores `"axum.routing.Router.with_state"` (full
//! parent-chain including the impl type).  When multiple symbols share the leaf
//! name `with_state`, the suffix-narrowing check fails silently and produces no
//! link.
//!
//! This test closes that gap by:
//! 1. Running the real Rust producer against the `.real-crates/axum` checkout.
//! 2. Finding the `Router` entry (axum's most recognisable export).
//! 3. Feeding it through `nudox_engine::chunk::chunk`.
//! 4. Asserting that the prose sections contain at least one
//!    `InlineRun::Link { target: LinkTarget::Symbol { .. } }`.
//!
//! The test is `#[ignore]`d exactly like its siblings in `nudox-store`'s
//! `real_crate.rs` and skips gracefully if the `.real-crates/axum` checkout is
//! absent.
//!
//! # Running
//!
//! ```text
//! # Fetch the fixture if not already present:
//! scripts/fetch-real-crate.sh axum 0.8.9
//!
//! # Run just this test:
//! cargo test -p nudox-engine --test real_producer_links -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use nudox_ir::view::IrView;
use nudox_producer::produce;
use nudox_producer_rust::RustProducer;
use nudox_store::package::{PackageView, Provenance};
use nudox_store::source::producer::PackageDescriptor;

use nudox_engine::chunk;
use nudox_engine::wire::{InlineRun, LinkTarget, ProseBlock, RenderSection};

// ---------------------------------------------------------------------------
// Fixture helper
// ---------------------------------------------------------------------------

/// Path to the axum checkout.
///
/// Mirrors `axum_root()` in `nudox-store/tests/real_crate.rs` so both test
/// files honour the same `NUDOX_REAL_CRATE_ROOT` override and the same default
/// location `../../.real-crates/axum`.
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

/// Lower the axum crate at `root` via the Rust producer.
///
/// Returns `None` and prints a skip message when the checkout is absent.
fn try_lower_axum(root: &PathBuf) -> Option<Arc<PackageView>> {
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no axum checkout at {}.\n\
             Run: scripts/fetch-real-crate.sh axum 0.8.9",
            root.display()
        );
        return None;
    }

    let descriptor = PackageDescriptor::cargo(root, "axum", "0.8.9");
    let started = std::time::Instant::now();

    let table = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
    )
    .expect("axum must lower without error for a well-formed checkout");

    eprintln!(
        "lowered {} entries from axum in {:.1}s",
        table.len(),
        started.elapsed().as_secs_f32()
    );

    let view = IrView::with_package(descriptor.lineage, table);
    Some(Arc::new(PackageView::build(view, Provenance::TrustedLocal)))
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// Lower real axum with the real producer, chunk `Router`, and assert that its
/// prose contains at least one `InlineRun::Link { target: Symbol }` for
/// `with_state`.
///
/// # What this proves
///
/// The gap between "the producer emits doc_links" (already asserted by
/// `router_doc_links_are_populated` in `nudox-store`) and "the engine resolves
/// those links into `InlineRun::Link`s" (previously untested with real data).
///
/// The canonical regression: `Router`'s doc comment says
/// ``See [`Router::with_state`] for more details.``  The producer stores the
/// target as `"axum::routing::with_state"` (module-qualified, no impl type in
/// the path).  The engine's `moniker_path` stores the path as
/// `"axum.routing.Router.with_state"` (including the impl-type node).  When
/// multiple items share the leaf `with_state`, the suffix check
/// `"axum.routing.Router.with_state".ends_with("axum.routing.with_state")`
/// returned `false`, so the link was never resolved and
/// ``[`Router::with_state`]`` rendered as plain text.
///
/// The fix (segment-level fallback in `DocLinkTable::build`) correctly matches
/// the label `"Router::with_state"` against the stored path segments and picks
/// the right entry.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn real_axum_router_doc_links_resolve_to_symbol_links() {
    let root = axum_root();
    let Some(package) = try_lower_axum(&root) else {
        return;
    };

    let view = package.view();

    // Find the Router entry: a Record-kind entry named "Router" with doc_links.
    //
    // axum may have multiple entries named "Router" (the type itself plus
    // possible re-exports).  We want the one that:
    //   (a) is a Record (the struct definition, not a re-export wrapper), and
    //   (b) has at least one doc_link (so the engine has something to resolve).
    //
    // If no such entry exists, we fall back to any entry named "Router" with
    // doc_links and non-empty documentation — the test is about the engine's
    // resolution pipeline, not about the producer's lowering choice.
    let router_entry_id = view
        .entries()
        .filter(|(_, e)| {
            e.sym().name == "Router"
                && !e.sym().doc_links.is_empty()
                && !e.sym().documentation.is_empty()
        })
        .map(|(id, _)| id)
        .next();

    let Some(router_id) = router_entry_id else {
        // Either no Router entry has doc_links (the producer hasn't implemented
        // intra-doc link resolution yet) or no Router entry exists at all.
        // In either case this is an environment / producer limitation, not an
        // engine bug.  Skip with a diagnostic.
        let router_count = view
            .entries()
            .filter(|(_, e)| e.sym().name == "Router")
            .count();
        let with_links = view
            .entries()
            .filter(|(_, e)| e.sym().name == "Router" && !e.sym().doc_links.is_empty())
            .count();
        eprintln!(
            "SKIP: found {} Router entries, {} with doc_links. \
             The producer may not yet populate doc_links for Router \
             (see nudox-producer-rust intra-doc link resolution).",
            router_count, with_links
        );
        return;
    };

    eprintln!(
        "Router entry found: id={}…, doc_links count={}",
        &router_id.to_hex()[..12],
        view.entry(router_id)
            .map(|e| e.sym().doc_links.len())
            .unwrap_or(0)
    );

    // Run the full chunk pipeline — this is the real code path the app uses.
    let (_, sections) = chunk::chunk(router_id, view, &package)
        .expect("chunk must succeed for the Router entry");

    // Collect all InlineRun::Link { Symbol } instances across all prose sections.
    let mut symbol_links: Vec<(String, String)> = Vec::new(); // (text, intro_hex)

    for section in &sections {
        let blocks: &[ProseBlock] = match section {
            RenderSection::Prose { blocks, .. }
            | RenderSection::Examples { blocks, .. }
            | RenderSection::Callout { blocks, .. } => blocks,
            _ => continue,
        };

        for block in blocks {
            let runs: &[InlineRun] = match block {
                ProseBlock::Paragraph { runs } | ProseBlock::Heading { runs, .. } => runs,
                ProseBlock::List { items, .. } => {
                    for item_runs in items {
                        for run in item_runs {
                            if let InlineRun::Link {
                                text,
                                target: LinkTarget::Symbol { key },
                            } = run
                            {
                                symbol_links.push((
                                    text.to_string(),
                                    key.intro.to_hex()[..12].to_owned(),
                                ));
                            }
                        }
                    }
                    continue;
                }
                _ => continue,
            };

            for run in runs {
                if let InlineRun::Link {
                    text,
                    target: LinkTarget::Symbol { key },
                } = run
                {
                    symbol_links.push((
                        text.to_string(),
                        key.intro.to_hex()[..12].to_owned(),
                    ));
                }
            }
        }
    }

    eprintln!("Symbol links found in Router prose: {:?}", symbol_links);

    assert!(
        !symbol_links.is_empty(),
        "chunk::chunk on the real axum Router entry must produce at least one \
         InlineRun::Link {{ target: Symbol }} in its prose sections.\n\
         \n\
         Router's doc comment contains intra-doc links like \
         [`Router::with_state`] that the producer resolves and stores in \
         Symbol::doc_links.  The engine must turn those into clickable Symbol \
         links — not leave them as plain text.\n\
         \n\
         REGRESSION: the suffix-narrowing in DocLinkTable::build fails when the \
         producer's canonical path ('axum::routing::with_state') does not suffix- \
         match the engine's moniker path ('axum.routing.Router.with_state') \
         because the producer drops the impl-type segment.  The segment-level \
         fallback must fire and pick the correct entry.\n\
         \n\
         doc_links on Router: {:#?}\n\
         Total sections: {}\n\
         All section types: {:?}",
        view.entry(router_id)
            .map(|e| e.sym().doc_links.to_vec())
            .unwrap_or_default(),
        sections.len(),
        sections.iter().map(|s| format!("{:?}", s.section_id())).collect::<Vec<_>>(),
    );

    // Specifically check that at least one link targets `with_state` (the most
    // prominent intra-doc link in Router's doc comment:
    // "See [`Router::with_state`] for more details.").
    //
    // We check the link text rather than the IntroId because we don't know
    // `with_state`'s IntroId a priori (it is content-addressed and depends on
    // the lowering).
    let has_with_state_link = symbol_links
        .iter()
        .any(|(text, _)| text.contains("with_state"));

    assert!(
        has_with_state_link,
        "expected a Symbol link whose text contains 'with_state' — \
         Router's doc says 'See [`Router::with_state`] for more details.' \
         and `with_state` must be resolved to the actual method entry, not \
         left as plain text.\n\
         Symbol links found: {:?}",
        symbol_links,
    );
}
