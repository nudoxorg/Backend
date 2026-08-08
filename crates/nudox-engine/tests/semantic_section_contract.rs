//! `SECTION_SEMANTIC` is empty **by design**, and that must stay checkable.
//!
//! # Why this file exists
//!
//! LIMITATIONS.md L41 records that the product advertises a Semantic search mode
//! with no engine behind it. The 2026-08-08 audit re-scoped it and found the gap
//! is blocked two independent ways — there is no Cargo edge from `nudox-engine`
//! to `workspace/registry`'s vector plane and `AGENTS-DOCTRINE.md` §1 forbids
//! adding one, and there is no model artifact in the tree at all (zero `.onnx`
//! files; `MockEmbedder` is the only embedder that runs unattended, and it marks
//! itself `durable_canonical: false` so its vectors can never be published).
//!
//! The decision was to bridge the GUI to `driver` — which already has working
//! semantic search — rather than build a second embedding pipeline inside the
//! offline-first engine. That is multi-week work gated on an artifact decision.
//! In the meantime section 2 keeps emitting empty, because the alternative is
//! fabricating matches, which is the doctrine §6 failure L41 was filed to avoid.
//!
//! # What makes this a test rather than a tautology
//!
//! Asserting "section 2 is empty" alone is worth nothing: it passes against a
//! search that returns nothing at all, against a search that has been deleted,
//! and against an engine that cannot start. Doctrine §4 — a test that would pass
//! against a stub is not a test.
//!
//! The invariant worth stating is the *conjunction*, and it is the one a reader
//! of the GUI actually needs to believe:
//!
//! > For a query that really matches symbols, sections 0 and 1 return hits, and
//! > section 2 is nevertheless emitted, and is nevertheless empty.
//!
//! Each conjunct kills a different way of being wrong:
//!
//! * sections 0/1 non-empty — the empty section 2 is a deliberate reservation,
//!   not the symptom of a broken search or an unseeded corpus;
//! * section 2 *emitted* — the protocol slot is still live, so `SearchSectionId`
//!   never renumbers behind the GUI's back (`stores/search.rs` keys its offline
//!   detection on the literal index 2) and the day a bridge lands there is a
//!   channel already carrying its shape;
//! * section 2 *empty* — nobody has quietly started fabricating rows.
//!
//! So this file goes red on three distinct regressions: semantic results
//! appearing from nowhere, the reserved section being dropped from the protocol,
//! and search itself breaking in a way that would otherwise make the "empty"
//! assertion pass for the wrong reason.
//!
//! # Two things about the other sections, learned by getting this wrong twice
//!
//! Both were my assumptions failing, not the engine, and both are worth writing
//! down because the next person will assume the same things.
//!
//! **`SECTION_TYPE` emits nothing unless the query carries a kind.** Either the
//! text is itself a kind keyword (`"fn"`, `"struct"`, `"trait"`, …, mapped by
//! `keyword_to_kinds`) or `SearchQuery::kinds` is non-empty; otherwise
//! `collect_type_hits` returns early, on the stated grounds that the name section
//! already covers everything. So the first test passes an explicit
//! `KindDiscriminant::Record` — without it section 1 is empty for *every* query
//! and the "sections 0 and 1 both return hits" conjunct is unsatisfiable.
//!
//! **`SECTION_TYPE` is a kind facet, not a text search.** Given a kind filter it
//! returns every symbol of that kind, whatever the query text says — its own
//! comment reads "Kind-facet, not text-relevance: a fixed base relevance". So the
//! zero-hit test deliberately passes NO kind filter: with one, a query matching
//! nothing still legitimately returns rows, and asserting otherwise would be
//! asserting against the design.

use std::collections::BTreeMap;

use nudox_engine::search::{SECTION_NAME, SECTION_SEMANTIC, SECTION_TYPE};
use nudox_engine::wire::{Gen, KindDiscriminant, SearchEvent};
use nudox_engine::{Engine, EngineConfig, SearchQuery};

/// Start an engine over the fixture corpus — no Rust toolchain required.
fn make_engine() -> nudox_engine::EngineHandle {
    Engine::start_with_fixtures(EngineConfig::default())
}

/// Block until the fixture corpus is seeded, mirroring `search_adversarial.rs`.
async fn wait_for_corpus(engine: &nudox_engine::EngineHandle) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(nudox_engine::PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => continue,
            // Closed channel means every package landed before we subscribed.
            Ok(Err(_)) => return,
            Err(_) => panic!("corpus never seeded within 5 s"),
        }
    }
}

/// Total rows delivered per section across `Section` and `Merge` events.
///
/// Counting both matters: a future implementation that seeded section 2 empty
/// and then merged fabricated rows into it would slip past a check that only
/// looked at the initial `Section` batch.
async fn rows_per_section(
    engine: &nudox_engine::EngineHandle,
    text: &str,
    kinds: Vec<KindDiscriminant>,
    generation: Gen,
) -> BTreeMap<u8, usize> {
    let (_stream, rx) = engine.search(
        SearchQuery {
            text: text.to_owned(),
            kinds,
            ..Default::default()
        },
        generation,
    );

    let mut counts = BTreeMap::new();
    while let Ok(event) = rx.recv_async().await {
        match event {
            SearchEvent::Section { section, rows, .. }
            | SearchEvent::Merge { section, rows, .. } => {
                *counts.entry(section.0).or_insert(0) += rows.len();
            }
            SearchEvent::Done { .. } => break,
            SearchEvent::Failed { error, .. } => panic!("search failed: {error:?}"),
            SearchEvent::Latency { .. } => {}
            // `SearchEvent` is `#[non_exhaustive]` so new row-bearing variants
            // can be added without breaking consumers. That is a hazard for this
            // file specifically: a future `SearchEvent::Append`-style variant
            // carrying rows would be silently ignored here, and the semantic
            // assertion would keep passing while fabricated rows flowed past it.
            // Fail loudly instead — a variant this test cannot account for means
            // this test no longer knows what it is measuring.
            other => panic!(
                "unhandled SearchEvent variant reached the section counter: \
                 {other:?}. If it carries rows, add it to the arm above — an \
                 ignored row-bearing variant makes this file's central assertion \
                 unsound."
            ),
        }
    }
    counts
}

#[tokio::test]
async fn semantic_section_is_empty_by_design_while_the_others_are_not() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    // A query the fixture corpus genuinely matches. If this stops matching, the
    // first two assertions below fail loudly rather than letting the third pass
    // for the wrong reason.
    let counts = rows_per_section(&engine, "point", vec![KindDiscriminant::Record], Gen(1)).await;

    let name = counts.get(&SECTION_NAME.0).copied().unwrap_or(0);
    let type_ = counts.get(&SECTION_TYPE.0).copied().unwrap_or(0);

    assert!(
        name > 0,
        "SECTION_NAME returned no rows for a query the fixture corpus contains. \
         The semantic assertion below would then pass because search is broken, \
         not because section 2 is reserved. Fix search first; this file cannot \
         say anything useful until it works. Sections seen: {counts:?}"
    );
    assert!(
        type_ > 0,
        "SECTION_TYPE returned no rows for a matching query — same problem as \
         above. Sections seen: {counts:?}"
    );

    assert!(
        counts.contains_key(&SECTION_SEMANTIC.0),
        "SECTION_SEMANTIC (id {}) was not emitted at all. The slot is reserved, \
         not optional: `workspace/gui/src/stores/search.rs` keys its offline \
         detection on the literal section index 2, and dropping the section here \
         would silently renumber the protocol underneath it. Emit it empty. \
         Sections seen: {counts:?}",
        SECTION_SEMANTIC.0
    );

    assert_eq!(
        counts.get(&SECTION_SEMANTIC.0).copied(),
        Some(0),
        "SECTION_SEMANTIC delivered rows. There is no embedding store on this \
         plane and no model artifact in this repository (zero .onnx files; the \
         only unattended embedder is MockEmbedder, which sets \
         durable_canonical: false precisely so its vectors cannot be published). \
         So any row arriving here is fabricated, which is the doctrine §6 \
         failure LIMITATIONS.md L41 exists to prevent. If a real bridge to \
         `driver` has landed, this test is the thing to rewrite — deliberately, \
         with a relevance judgment behind it — not to delete. Sections: {counts:?}"
    );
}

#[tokio::test]
async fn a_query_matching_nothing_still_emits_every_reserved_section() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    // The empty-result path is where a section is most likely to be skipped by
    // an early return. The protocol shape must not depend on hit count.
    let counts = rows_per_section(&engine, "zzzz-no-such-symbol-anywhere", Vec::new(), Gen(2)).await;

    for section in [SECTION_NAME, SECTION_TYPE, SECTION_SEMANTIC] {
        assert!(
            counts.contains_key(&section.0),
            "section {} was skipped for a zero-hit query. Every section must be \
             emitted for every query — the GUI renders a per-section empty state \
             and cannot distinguish 'no hits' from 'never arrived' if a section \
             goes missing. Sections seen: {counts:?}",
            section.0
        );
        assert_eq!(
            counts.get(&section.0).copied(),
            Some(0),
            "section {} returned rows for a query that matches nothing. \
             Sections seen: {counts:?}",
            section.0
        );
    }
}
