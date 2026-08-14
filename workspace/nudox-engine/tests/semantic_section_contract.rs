//! `SECTION_SEMANTIC` is empty **because there is no model**, and it must say so.
//!
//! # What this file used to say, and why it was rewritten rather than deleted
//!
//! Until 2026-08-09 this file pinned section 2 as *deliberately empty*, and its
//! central assertion was that the section is emitted and carries zero rows. Its
//! own header named the condition for rewriting it: "If a real bridge … has
//! landed, this test is the thing to rewrite — deliberately, with a relevance
//! judgment behind it — not to delete."
//!
//! A bridge has landed. `nudox_engine::semantic` defines an object-safe
//! `Embedder` port; `run_search` fans out to a real index over it; and the
//! index is built incrementally as packages load. The relevance judgement lives
//! in `workspace/registry/tests/vector/engine_relevance.rs`, where the real
//! model can be named — it is a labelled set over real `memchr`, asserting that
//! a relevant symbol outranks an irrelevant one for queries whose words do not
//! appear in the relevant symbol's name.
//!
//! So the *old* invariant is now wrong in a specific way worth being precise
//! about. Section 2 being empty was never the contract; it was a *consequence*
//! of there being nothing behind it. The contract is, and always was:
//!
//! > **Section 2 never claims to have searched something it did not.**
//!
//! "Empty" satisfied that only by accident, and it satisfied it ambiguously:
//! `Section { rows: [] }` is indistinguishable from "we searched everything and
//! there is no match", which is a claim about the corpus that an engine with no
//! model is in no position to make. That ambiguity is what
//! `SearchEvent::SectionState` removes, and it is what this file now pins.
//!
//! # What still holds, unchanged, from the old file
//!
//! Two of the three original conjuncts are still exactly right, and the
//! reasoning behind them is preserved here because it cost someone the work to
//! find it:
//!
//! * **sections 0/1 must return real hits** — otherwise every assertion about
//!   section 2 passes because search is broken rather than because section 2 is
//!   behaving. A test that goes green on a dead engine is not a test.
//! * **section 2 must be *emitted*** — `workspace/gui/src/stores/search.rs`
//!   keys its offline detection on the literal section index `2`, so dropping
//!   the section would silently renumber the protocol underneath the GUI.
//!
//! # And two facts about the other sections, kept from the old file
//!
//! **`SECTION_TYPE` now answers signature queries** (`return:Point`,
//! `param:&Path`) via `nudox_engine::typequery`, *and* still answers a bare
//! kind keyword the old way. The old note here — "emits nothing unless the
//! query carries a kind" — is therefore no longer the whole story, but it is
//! still true of a query that names neither a kind nor a facet, which is why
//! the tests below still pass an explicit `KindDiscriminant`.
//!
//! **`SECTION_TYPE`'s kind path is still a facet, not a text search.** Given a
//! kind filter it returns every symbol of that kind whatever the query text
//! says. So the zero-hit test below still deliberately passes NO kind filter.

use std::collections::BTreeMap;

use nudox_engine::search::{SECTION_NAME, SECTION_SEMANTIC, SECTION_TYPE};
use nudox_engine::wire::{Gen, KindDiscriminant, SearchEvent};
use nudox_engine::{Engine, EngineConfig, SearchQuery, SectionState, Unavailable};

/// Start an engine over the fixture corpus — no Rust toolchain required.
///
/// Deliberately **no embedder**: `EngineConfig::default()` leaves
/// `embedder: None`, which is the configuration every unattended build in this
/// repo runs in and the one whose honesty this file is about.
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

/// Everything one search said, per section: how many rows, and what state.
struct Observed {
    rows: BTreeMap<u8, usize>,
    states: BTreeMap<u8, SectionState>,
}

/// Drain a search, counting rows and recording states per section.
///
/// Counting `Section` **and** `Merge` matters for the reason the old file gave:
/// an implementation that seeded section 2 empty and then merged fabricated
/// rows into it would slip past a check that only looked at the first batch.
async fn observe(
    engine: &nudox_engine::EngineHandle,
    text: &str,
    kinds: Vec<KindDiscriminant>,
    generation: Gen,
) -> Observed {
    let (_stream, rx) = engine.search(
        SearchQuery {
            text: text.to_owned(),
            kinds,
            ..Default::default()
        },
        generation,
    );

    let mut rows = BTreeMap::new();
    let mut states = BTreeMap::new();
    while let Ok(event) = rx.recv_async().await {
        match event {
            SearchEvent::Section { section, rows: r, .. }
            | SearchEvent::Merge { section, rows: r, .. } => {
                *rows.entry(section.0).or_insert(0) += r.len();
            }
            SearchEvent::SectionState { section, state, .. } => {
                states.insert(section.0, state);
            }
            SearchEvent::Done { .. } => break,
            SearchEvent::Failed { error, .. } => panic!("search failed: {error:?}"),
            SearchEvent::Latency { .. } => {}
            // `SearchEvent` is `#[non_exhaustive]`, so a future row-bearing
            // variant would be silently ignored here and this file's central
            // assertion would keep passing while rows flowed past it. Fail
            // loudly instead — a variant this test cannot account for means it
            // no longer knows what it is measuring.
            other => panic!(
                "unhandled SearchEvent variant reached the section observer: \
                 {other:?}. If it carries rows, add it to the arm above."
            ),
        }
    }
    Observed { rows, states }
}

/// With no embedder, section 2 is empty **and says why** — while sections 0 and
/// 1 return real hits.
///
/// The conjunction is what makes this a test rather than a tautology, and the
/// third conjunct is the one that changed: it is no longer "section 2 is
/// empty", which a broken engine also satisfies, but "section 2 reports
/// `Unavailable { NoEmbedder }`", which only a section that ran its
/// availability check and found no model can produce.
#[tokio::test]
async fn a_build_with_no_model_reports_unavailable_rather_than_no_results() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    // A query the fixture corpus genuinely matches.
    let seen = observe(&engine, "point", vec![KindDiscriminant::Record], Gen(1)).await;

    let name = seen.rows.get(&SECTION_NAME.0).copied().unwrap_or(0);
    let type_ = seen.rows.get(&SECTION_TYPE.0).copied().unwrap_or(0);

    assert!(
        name > 0,
        "SECTION_NAME returned no rows for a query the fixture corpus contains. \
         Every assertion below would then pass because search is broken, not \
         because section 2 is behaving. Fix search first. Rows: {:?}",
        seen.rows
    );
    assert!(
        type_ > 0,
        "SECTION_TYPE returned no rows for a matching query — same problem. \
         Rows: {:?}",
        seen.rows
    );

    assert!(
        seen.rows.contains_key(&SECTION_SEMANTIC.0),
        "SECTION_SEMANTIC (id {}) was not emitted at all. The slot is reserved, \
         not optional: `workspace/gui/src/stores/search.rs` keys its offline \
         detection on the literal section index 2, so dropping the section here \
         renumbers the protocol underneath it. Rows: {:?}",
        SECTION_SEMANTIC.0,
        seen.rows
    );
    assert_eq!(
        seen.rows.get(&SECTION_SEMANTIC.0).copied(),
        Some(0),
        "SECTION_SEMANTIC delivered rows with no embedder installed. There is \
         nothing to have produced them, so they are fabricated — the doctrine \
         §6 failure docs/LIMITATIONS.md L41 exists to prevent. Rows: {:?}",
        seen.rows
    );

    // The conjunct that replaces "is empty". `Complete` here would be a lie:
    // it claims the corpus was searched and contains no match for "point",
    // which the *name* section has just disproved.
    assert_eq!(
        seen.states.get(&SECTION_SEMANTIC.0),
        Some(&SectionState::Unavailable {
            reason: Unavailable::NoEmbedder
        }),
        "with no embedder the semantic section must report why it is empty. \
         `Complete` would assert that we searched and found nothing — a claim \
         about the corpus that an engine with no model cannot make, and one the \
         name section contradicts. States: {:?}",
        seen.states
    );
}

/// The local sections claim completeness, and are entitled to.
///
/// Stated separately because it is the other half of the same contract: if
/// *every* section reported `Unavailable`, the assertion above would pass while
/// meaning nothing. Sections 0 and 1 read the resident corpus directly, so
/// `Complete` is exactly true for them, and the difference between the two
/// answers is the whole information content of `SectionState`.
#[tokio::test]
async fn the_local_sections_report_complete() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let seen = observe(&engine, "point", vec![KindDiscriminant::Record], Gen(2)).await;

    for section in [SECTION_NAME, SECTION_TYPE] {
        assert_eq!(
            seen.states.get(&section.0),
            Some(&SectionState::Complete),
            "section {} searches the resident corpus synchronously, so it is \
             complete by construction and must say so. A section that only \
             reports state when the news is bad forces the consumer to infer \
             the good case from silence. States: {:?}",
            section.0,
            seen.states
        );
    }
}

/// Every section is emitted, with a state, for a query that matches nothing.
///
/// The empty-result path is where a section is most likely to be skipped by an
/// early return, and the GUI cannot distinguish "no hits" from "never arrived"
/// if one goes missing.
#[tokio::test]
async fn a_query_matching_nothing_still_emits_every_reserved_section_and_its_state() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let seen = observe(&engine, "zzzz-no-such-symbol-anywhere", Vec::new(), Gen(3)).await;

    for section in [SECTION_NAME, SECTION_TYPE, SECTION_SEMANTIC] {
        assert!(
            seen.rows.contains_key(&section.0),
            "section {} was skipped for a zero-hit query. Rows: {:?}",
            section.0,
            seen.rows
        );
        assert_eq!(
            seen.rows.get(&section.0).copied(),
            Some(0),
            "section {} returned rows for a query that matches nothing. \
             Rows: {:?}",
            section.0,
            seen.rows
        );
        assert!(
            seen.states.contains_key(&section.0),
            "section {} delivered rows but no state. An unstated state is the \
             ambiguity this event exists to remove — the consumer is back to \
             guessing what zero rows means. States: {:?}",
            section.0,
            seen.states
        );
    }

    // And the two kinds of emptiness really are distinguishable, which is the
    // single claim the whole mechanism has to support.
    assert_ne!(
        seen.states.get(&SECTION_NAME.0),
        seen.states.get(&SECTION_SEMANTIC.0),
        "section 0 (searched, found nothing) and section 2 (never searched, no \
         model) both delivered zero rows and must NOT be reporting the same \
         state — if they do, a reader cannot tell 'there is no such symbol' \
         from 'this build cannot answer that'. States: {:?}",
        seen.states
    );
}
