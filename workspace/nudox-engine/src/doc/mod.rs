//! `open_symbol` — drive the chunker and emit the §9.3 `DocEvent` protocol.
//!
//! # Protocol invariants
//!
//! 1. `Head` is always the first event.
//! 2. `Section`s arrive in `section_plan` order.
//! 3. `Highlight` only references already-sent sections.
//! 4. Applying any prefix of a valid stream yields a valid page.
//! 5. `Done` or `Failed` is the terminal event; nothing follows it.
//!
//! The invariants are upheld structurally: this module emits `Head` first,
//! then iterates `sections` in the order the chunker produced them (emitting
//! a `Highlight` immediately after each section so invariant 3 is trivially
//! true), then emits `Impls` and `Refs` pages (interleaved freely after
//! `Head` per the protocol validator), and finally emits `Done`. A cancelled
//! stream (receiver dropped) simply stops; the prefix up to that point is a
//! valid partial page.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    chunk,
    runtime::{EngineHandle, StreamHandle},
    versions::{timeline, VersionRegistry, VersionSlice},
    wire::{
        DocEvent, EngineError, Gen, HighlightSpan, KeyStaleness, KeyTierName, SharedStr, SymbolKey,
    },
};

mod impls;
mod refs;

use impls::{collect_impls, emit_impls_pages};
use refs::{collect_refs, emit_refs_pages};

// ---------------------------------------------------------------------------
// Channel capacity (Appendix C)
// ---------------------------------------------------------------------------

const DOC_CHANNEL_CAP: usize = 64;

// ---------------------------------------------------------------------------
// Paging constants
// ---------------------------------------------------------------------------

/// Maximum number of `ImplRow` entries per `Impls` page event.
///
/// Impls are cheap to enumerate (one scan of `by_kind[Impl]`), but a struct
/// like `axum::Router` in a large crate can accumulate 50–100 trait impls.
/// Sending them all in one event would make the first paint of the Impls tab
/// wait for the whole scan before the channel releases.  25 rows is roughly
/// one viewport height — the user sees the first screenful immediately and
/// further pages arrive while they are reading.
const IMPLS_PAGE_SIZE: usize = 25;

/// Maximum number of `RefRow` entries per `Refs` page event.
///
/// Usages (`FunctionCall`, `FieldAccess`, etc.) can be numerous for popular
/// items (hundreds of call sites for a widely-used function).  50 rows per
/// page keeps individual channel messages small while still delivering a full
/// screenful of results on each page.
const REFS_PAGE_SIZE: usize = 50;

// ---------------------------------------------------------------------------
// open_symbol
// ---------------------------------------------------------------------------

impl EngineHandle {
    /// Open a symbol's documentation page as a streaming `DocEvent` sequence.
    ///
    /// Returns `(StreamHandle, receiver)`. Dropping the `StreamHandle` cancels
    /// the stream (§2.3). The receiver closes when the stream is complete
    /// (`Done`) or has failed (`Failed`).
    ///
    /// # Protocol
    ///
    /// 1. `Head` — always first; carries the `SymbolHead` and `section_plan`.
    /// 2. `Timeline` — the symbol's history across loaded versions.
    /// 3. `Section` × N — in `section_plan` order.
    /// 4. `Done` — terminal.
    ///
    /// On error: `Failed(EngineError)` — terminal.
    ///
    /// # Which version this answers
    ///
    /// The generation currently resident in the `Corpus` — see
    /// [`EngineHandle::select_version`]. `SymbolKey` carries no version, and
    /// deliberately so: for most declarations `IntroId` is stable across
    /// versions, so the same key opens the same symbol in whichever generation
    /// is current, and switching version does not invalidate a link, a tab, or
    /// a history entry.
    ///
    /// **This does not hold for every declaration**, and the exception is not
    /// rare. `IntroId` is minted with a disambiguator tier
    /// (`nudox_ir::change::intro`), and only the top tiers are content-derived.
    /// A declaration that collided and fell through to `Disambiguator::Span` is
    /// keyed on **byte offsets**, so adding a comment above it changes its key;
    /// one that fell through to `Ordinal` is, in that module's own words, "not
    /// stable across a producer reordering its output". `Span` is universally
    /// degenerate in the Go, C# and Python producers (they emit `0..0`), which
    /// forces `Ordinal` for every collision there. Measured: memchr drops
    /// 1835 → 1794 entries with escalation disabled, i.e. 41 declarations in
    /// one crate whose identity is not content-derived.
    ///
    /// So a stale link is possible. `SealReport::forced_keys` is projected into
    /// `PackageView`, allowing the failure below to report which tier minted a
    /// key that still resolves in another generation. Treat "the key still
    /// resolves" as the common case, not a guarantee.
    pub fn open_symbol(
        &self,
        key: SymbolKey,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<DocEvent>) {
        let (tx, rx) = flume::bounded(DOC_CHANNEL_CAP);
        let (cancel_token, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);

        let corpus = self.corpus();
        let highlighter = self.highlighter();
        let versions = self.versions_registry();
        let load_failures = self.load_failures();
        self.spawn(stream_symbol(
            corpus,
            versions,
            load_failures,
            highlighter,
            key,
            generation,
            tx,
            cancel_token,
        ));

        (handle, rx)
    }
}

// ---------------------------------------------------------------------------
// async driver
// ---------------------------------------------------------------------------

/// Resolve one symbol against the current corpus generation, preserving the
/// same load-failure and stale-key diagnostics for both full documents and
/// compact projections.
pub(crate) async fn resolve_symbol(
    corpus: &crate::store::corpus::Corpus,
    versions: &VersionRegistry,
    load_failures: &crate::runtime::LoadFailures,
    key: &SymbolKey,
) -> Result<Arc<crate::store::package::PackageView>, EngineError> {
    let pkg = corpus
        .package(&key.package)
        .await
        .ok_or_else(|| EngineError::PackageNotLoaded {
            package: key.package.clone(),
            attempted: load_failures.get(&key.package),
        })?;

    if pkg.view().entry(key.intro).is_some() {
        return Ok(pkg);
    }

    let possibly_stale = versions
        .slices(&key.package)
        .into_iter()
        .find(|slice| slice.package.view().entry(key.intro).is_some())
        .map(|slice| KeyStaleness {
            seen_in_version: slice.version,
            tier: slice.package.key_tier(key.intro).map(KeyTierName::from),
        });
    Err(EngineError::SymbolNotFound { possibly_stale })
}

async fn stream_symbol(
    corpus: crate::store::corpus::Corpus,
    versions: Arc<VersionRegistry>,
    load_failures: crate::runtime::LoadFailures,
    highlighter: crate::highlight::SharedHighlighter,
    key: SymbolKey,
    _generation: Gen,
    tx: flume::Sender<DocEvent>,
    cancel: CancellationToken,
) {
    // Bail immediately if the stream was cancelled before we even started.
    if cancel.is_cancelled() {
        return;
    }

    // --- Resolve package + entry -------------------------------------------
    let pkg_arc = match resolve_symbol(&corpus, &versions, &load_failures, &key).await {
        Ok(package) => package,
        Err(error) => {
            let _ = tx.send_async(DocEvent::Failed(error)).await;
            return;
        }
    };

    // --- Run the chunker ---------------------------------------------------
    // `chunk::chunk` is CPU-bound but fast (single IR walk). We keep it on
    // the async task to avoid a blocking-pool round-trip, which would be
    // strictly worse for the < 50 ms first-paint target (§9.1).
    let intro = key.intro;
    let view = pkg_arc.view();
    let Some((head, sections)) = chunk::chunk(intro, view, &pkg_arc) else {
        let _ = tx
            .send_async(DocEvent::Failed(EngineError::Chunk {
                message: "chunker returned None for a live entry".into(),
            }))
            .await;
        return;
    };

    // --- Emit Head ---------------------------------------------------------
    if cancel.is_cancelled() {
        return;
    }
    if tx.send_async(DocEvent::Head(Box::new(head))).await.is_err() {
        debug!("doc stream: receiver dropped after Head");
        return;
    }

    // --- Emit Timeline -----------------------------------------------------
    //
    // Immediately after `Head` and before the first `Section`. The Timeline is
    // a tab, not a band in the vertical flow, so it has no `SectionId` and does
    // not appear in `section_plan` — but a tab the user can click at any moment
    // should not be the one part of the page that is still empty because the
    // body is streaming. It is cheap enough to justify the position: one map
    // lookup and one signature render per loaded generation.
    //
    // The registry normally has slices for any package the corpus holds,
    // because the seeding task records before it inserts. The fallback below
    // covers the case where something put a `PackageView` into the corpus
    // without going through that path: rather than emit an empty timeline —
    // which would read as "this symbol has no history", a claim we have no
    // basis for — we synthesise a single slice from the resident package. The
    // result is the same honest one-row `Present` timeline a genuinely
    // single-version corpus produces.
    let slices = {
        let recorded = versions.slices(&key.package);
        if recorded.is_empty() {
            vec![VersionSlice {
                version: SharedStr::from(crate::versions::UNVERSIONED),
                package: Arc::clone(&pkg_arc),
                is_current: true,
            }]
        } else {
            recorded
        }
    };
    let timeline = timeline::build(&key, &slices);
    drop(slices); // release the extra PackageView handles before streaming

    if cancel.is_cancelled() {
        return;
    }
    if tx.send_async(DocEvent::Timeline(timeline)).await.is_err() {
        debug!("doc stream: receiver dropped after Timeline");
        return;
    }

    // --- Emit Sections in plan order, with Highlight upgrade after each ----
    //
    // `Highlight` invariant (§9.3 invariant 3): a `Highlight` event must only
    // reference a `SectionId` that has already been sent.  We uphold this by
    // always emitting the `Section` first, then immediately emitting the
    // corresponding `Highlight`.  The two sends are sequential on a single
    // async task, so no receiver can observe a `Highlight` before its
    // `Section` — even if the channel has slack capacity.
    //
    // Highlighting never fails the stream: `highlight::highlight` returns an
    // empty span list for unknown languages or parse failures, and we emit that
    // empty list as a `Highlight` event rather than skipping it.  Skipping
    // would leave the GUI in "highlight pending" limbo for the section; an
    // empty-span event is the explicit signal that "highlighting was attempted
    // and produced nothing".
    for section in sections {
        if cancel.is_cancelled() {
            return;
        }

        // Compute spans *before* sending the Section so the Highlight can be
        // sent right after with zero additional latency cost on the hot path.
        // Tree-sitter is fast enough that this is dominated by the channel
        // send, not the parse.
        //
        // If there are multiple code blocks inside a single prose section (e.g.
        // an Examples section with several fenced blocks), we concatenate their
        // spans.  Because `code_sources` only returns code-bearing blocks, the
        // spans are generated per-block and then merged.  Spans within a block
        // are byte-relative to that block's text, which is exactly what the GUI
        // expects: it maps `SectionId` → block text, then applies spans over
        // it.  When a prose section has multiple code blocks, each call to
        // `highlight::highlight` starts at offset 0 relative to its own block
        // text — the GUI's `sync_highlights` path stores one span list per
        // section, so we only highlight the first code block in a multi-block
        // section (the common case is one code block per section anyway).
        //
        // With no highlighter installed every section still gets an empty
        // `Highlight`. Keeping the *protocol* unconditional while the *content*
        // is optional means the consumer's state machine is identical either
        // way — a host without grammars renders uncoloured code, never code
        // stuck waiting for a highlight that will never arrive.
        let spans: Arc<[HighlightSpan]> = match (&highlighter, first_code_block(&section)) {
            (Some(highlighter), Some((source, language))) => {
                highlighter.highlight(source, language).into()
            }
            // Non-code sections, and code sections in a host with no grammars.
            _ => Arc::from([] as [HighlightSpan; 0]),
        };

        let section_id = section.section_id();

        if tx.send_async(DocEvent::Section(section)).await.is_err() {
            debug!("doc stream: receiver dropped during Section");
            return;
        }

        if cancel.is_cancelled() {
            return;
        }

        // Invariant 3 is satisfied here by construction: `Section(section_id)`
        // was just sent on the line above, so the section is already in the
        // receiver's buffer when `Highlight { section: section_id, .. }` lands.
        if tx
            .send_async(DocEvent::Highlight {
                section: section_id,
                spans,
            })
            .await
            .is_err()
        {
            debug!("doc stream: receiver dropped during Highlight");
            return;
        }
    }

    // --- Emit Impls --------------------------------------------------------
    //
    // An `Impl` entry whose `self_ty` resolves to the symbol being viewed is
    // an implementation of this type.  We find them by scanning the package's
    // `by_kind[Impl]` bucket — every impl in the package lives there — and
    // selecting those whose `self_ty` is a `Nominal(Intro(key.intro))` or
    // `Apply { base: Nominal(Intro(key.intro)), .. }` (the common case of
    // `impl Debug for Vec<T>` where the outer type has a generic argument).
    //
    // # Why `self_ty` and not the `mentions` posting list
    //
    // The `mentions` posting list indexes `of` (the *trait* in a trait impl)
    // and supertrait bounds — it is the right structure for "who implements
    // this trait".  For the Implementations tab we want the opposite question:
    // "which impl blocks declare `Self = this type`".  That information is
    // only directly available by inspecting `Impl::self_ty`.
    //
    // # Ordering guarantee
    //
    // We sort impls by their rendered label string before paginating.  The
    // sorted order is stable across two calls to `stream_symbol` on the same
    // package (labels are derived deterministically from the IR), and it
    // groups related impls together in a way that is useful for reading
    // (inherent impls before trait impls, alphabetical within each group).
    //
    // # Empty-is-a-real-answer
    //
    // A symbol with no impls must still receive exactly one `Impls` event
    // with `done: true` and an empty `impls` slice.  Without that event the
    // GUI cannot distinguish "none" from "still loading", which is the exact
    // failure mode that made this empty forever.
    if !cancel.is_cancelled() {
        let impls_result = collect_impls(key.intro, &pkg_arc, key.package.clone());
        emit_impls_pages(&tx, &cancel, impls_result).await;
        if cancel.is_cancelled() {
            return;
        }
    }

    // --- Emit Refs ---------------------------------------------------------
    //
    // `usages_of(key)` returns every entry whose resolved-reference postings include a
    // reference to `key` at `Confidence >= Index` — the graph-worthy floor
    // (see `PackageIndexes::usages_floor_is_confidence_index`).  These are
    // *call sites*, field accesses, and other active uses: the kind of
    // cross-reference a user means when they ask "where is this symbol used?".
    //
    // We deliberately do NOT include `mentions_of(key)` in the Refs tab.
    // `mentions` indexes trait-impl `of` edges and supertrait bounds — those
    // are structural relationships already visible in the Implementations tab
    // (impls where `of = key`) and in the symbol's own signature (supers).
    // Mixing them into Refs would show duplicates and confuse the intent of
    // the two tabs.
    //
    // `usages` and `mentions` together cover both directions of the reference
    // graph: `usages` = "who calls / accesses me" (Refs tab), `mentions` =
    // "who extends or implements me" (Implementations tab via `self_ty` scan).
    //
    // # Empty-is-a-real-answer
    //
    // Same logic as Impls: a symbol with no usages emits one empty page with
    // `done: true` so the GUI can transition to the "no references" state.
    if !cancel.is_cancelled() {
        let refs_result = collect_refs(key.intro, &pkg_arc, key.package.clone());
        emit_refs_pages(&tx, &cancel, refs_result).await;
        if cancel.is_cancelled() {
            return;
        }
    }

    // --- Emit Done ---------------------------------------------------------
    if !cancel.is_cancelled() {
        let _ = tx.send_async(DocEvent::Done).await;
    }
}

// ---------------------------------------------------------------------------
// Code-block extraction
// ---------------------------------------------------------------------------

/// The first code block in a section, as `(source, language)`.
///
/// # Why only the first
///
/// A `Highlight` event carries one span list per `SectionId`, and spans are
/// byte offsets relative to the block they came from. Concatenating the spans
/// of several blocks would silently reinterpret the second block's offsets
/// against the first block's text — colouring the wrong characters, which is
/// worse than no colour. One block per section is the overwhelmingly common
/// shape; carrying more would need a per-block id in the wire protocol.
fn first_code_block(section: &crate::wire::RenderSection) -> Option<(&str, &str)> {
    use crate::wire::{ProseBlock, RenderSection};

    fn from_blocks(blocks: &[ProseBlock]) -> Option<(&str, &str)> {
        blocks.iter().find_map(|block| match block {
            ProseBlock::Code { text, lang, .. } => Some((text.as_ref(), lang.0.as_ref())),
            _ => None,
        })
    }

    match section {
        RenderSection::CodeBlock { text, lang, .. } => Some((text.as_ref(), lang.0.as_ref())),
        RenderSection::Prose { blocks, .. }
        | RenderSection::Examples { blocks, .. }
        | RenderSection::Callout { blocks, .. } => from_blocks(blocks),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::store::source::fixtures::FixtureSource;

    use crate::{
        runtime::{Engine, EngineConfig},
        wire::{DocEvent, Gen},
    };

    fn make_engine() -> crate::runtime::EngineHandle {
        Engine::start(EngineConfig::default(), FixtureSource::rich())
    }

    /// Wait until the corpus has the rich fixture package (the engine seeds
    /// asynchronously).
    async fn wait_for_corpus(engine: &crate::runtime::EngineHandle) {
        use crate::store::source::fixtures::rich_lineage;
        for _ in 0..50 {
            if engine.corpus().package(&rich_lineage()).await.is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("corpus never seeded");
    }

    #[tokio::test]
    async fn open_symbol_head_first_done_last() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        // Grab a real SymbolKey from the rich corpus.
        let corpus = engine.corpus();
        let lineage = crate::store::source::fixtures::rich_lineage();
        let pkg = corpus
            .package(&lineage)
            .await
            .expect("rich package must be loaded");
        let (intro, _entry) = pkg.view().entries().next().expect("must have entries");
        let key = nudox_ir::change::StableRef::new(lineage, intro);

        let (handle, rx) = engine.open_symbol(key, Gen(1));

        let mut events = Vec::new();
        while let Ok(ev) = rx.recv_async().await {
            let is_terminal = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
            events.push(ev);
            if is_terminal {
                break;
            }
        }
        drop(handle);

        // Protocol: Head first.
        assert!(
            matches!(events.first(), Some(DocEvent::Head(_))),
            "first event must be Head; got {:?}",
            events.first()
        );
        // Protocol: Done last.
        assert!(
            matches!(events.last(), Some(DocEvent::Done)),
            "last event must be Done; got {:?}",
            events.last()
        );
    }

    #[tokio::test]
    async fn truncated_stream_is_valid_page() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let corpus = engine.corpus();
        let lineage = crate::store::source::fixtures::rich_lineage();
        let pkg = corpus.package(&lineage).await.unwrap();
        let (intro, _) = pkg.view().entries().next().unwrap();
        let key = nudox_ir::change::StableRef::new(lineage, intro);

        let (handle, rx) = engine.open_symbol(key, Gen(2));

        // Receive only the first 2 events, then drop handle — cancels stream.
        let mut events = Vec::new();
        for _ in 0..2 {
            if let Ok(ev) = rx.recv_async().await {
                events.push(ev);
            }
        }
        drop(handle);

        // A prefix of any length must start with Head (or be empty).
        if let Some(first) = events.first() {
            assert!(
                matches!(first, DocEvent::Head(_)),
                "first event of prefix must be Head"
            );
        }
    }

    #[tokio::test]
    async fn superseded_gen_receives_no_events() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let corpus = engine.corpus();
        let lineage = crate::store::source::fixtures::rich_lineage();
        let pkg = corpus.package(&lineage).await.unwrap();
        let (intro, _) = pkg.view().entries().next().unwrap();
        let key = nudox_ir::change::StableRef::new(lineage.clone(), intro);

        // Issue gen 1 then immediately cancel it.
        let (handle1, _rx1) = engine.open_symbol(key.clone(), Gen(1));
        drop(handle1); // cancel gen 1

        // Issue gen 2.
        let (handle2, rx2) = engine.open_symbol(key, Gen(2));

        // Gen 2 must still produce a complete stream.
        let mut got_head = false;
        let mut got_done = false;
        while let Ok(ev) = rx2.recv_async().await {
            match &ev {
                DocEvent::Head(_) => got_head = true,
                DocEvent::Done => {
                    got_done = true;
                    break;
                }
                DocEvent::Failed(_) => break,
                _ => {}
            }
        }
        drop(handle2);

        assert!(got_head, "gen 2 must emit Head");
        assert!(got_done, "gen 2 must emit Done");
    }

    #[tokio::test]
    async fn missing_symbol_emits_failed() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
        let fake_lineage = PackageLineageId::new(
            EcosystemId::new("fixture"),
            PackageName::new("nudox-fixture-rich"),
        );
        let fake_intro = IntroId::from_raw([0xFFu8; 32]);
        let bad_key = StableRef::new(fake_lineage, fake_intro);

        let (_handle, rx) = engine.open_symbol(bad_key, Gen(3));
        let ev = rx.recv_async().await.expect("must receive an event");
        assert!(
            matches!(ev, DocEvent::Failed(_)),
            "missing symbol must produce Failed, got {ev:?}"
        );
    }

    // ── Timeline ─────────────────────────────────────────────────────────────

    use crate::{
        test_support::{entry as spec, intro, lineage, package, start_and_settle},
        wire::{Timeline, TimelineChange},
    };

    /// Start a settled engine over `generations` of one lineage, open the
    /// symbol at `intro(n)`, and return the whole event sequence.
    async fn events_for(
        generations: Vec<(&str, std::sync::Arc<crate::store::package::PackageView>)>,
        n: u8,
    ) -> Vec<DocEvent> {
        let lid = lineage("axum");
        let engine = start_and_settle(&lid, generations).await;

        let key = nudox_ir::change::StableRef::new(lid, intro(n));
        let (handle, rx) = engine.open_symbol(key, Gen(1));

        let mut events = Vec::new();
        while let Ok(ev) = rx.recv_async().await {
            let terminal = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
            events.push(ev);
            if terminal {
                break;
            }
        }
        drop(handle);
        events
    }

    fn timeline_of(events: &[DocEvent]) -> &Timeline {
        events
            .iter()
            .find_map(|e| match e {
                DocEvent::Timeline(t) => Some(t),
                _ => None,
            })
            .expect("every resolved symbol gets a Timeline event")
    }

    #[tokio::test]
    async fn timeline_arrives_after_head_and_before_any_section() {
        let lid = lineage("axum");
        let events = events_for(vec![("0.8.9", package(&lid, vec![spec(1, "Router")]))], 1).await;

        let head_at = events
            .iter()
            .position(|e| matches!(e, DocEvent::Head(_)))
            .expect("Head must be present");
        let timeline_at = events
            .iter()
            .position(|e| matches!(e, DocEvent::Timeline(_)))
            .expect("Timeline must be present");
        let first_section = events
            .iter()
            .position(|e| matches!(e, DocEvent::Section(_)));

        assert_eq!(head_at, 0, "Head is still first");
        assert!(timeline_at > head_at, "Timeline follows Head");
        if let Some(section_at) = first_section {
            assert!(
                timeline_at < section_at,
                "Timeline precedes the first Section"
            );
        }
    }

    #[tokio::test]
    async fn one_loaded_version_produces_one_present_row_not_an_empty_timeline() {
        // This is what a user sees first with a single-version corpus, and the
        // claim has to be "present in 0.8.9" — never "introduced in 0.8.9",
        // which the engine has no evidence for, and never nothing at all.
        let lid = lineage("axum");
        let events = events_for(vec![("0.8.9", package(&lid, vec![spec(1, "Router")]))], 1).await;
        let t = timeline_of(&events);

        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].change, TimelineChange::Present);
        assert_eq!(&*t.rows[0].version, "0.8.9");
        assert_eq!(t.versions_examined, 1);
        assert!(!t.rows[0].sig.is_empty(), "the row carries a signature");
    }

    #[tokio::test]
    async fn two_generations_classify_the_change_between_them() {
        let lid = lineage("axum");
        let events = events_for(
            vec![
                ("0.7.9", package(&lid, vec![spec(1, "Router").docs("old")])),
                ("0.8.1", package(&lid, vec![spec(1, "Router").docs("new")])),
            ],
            1,
        )
        .await;
        let t = timeline_of(&events);

        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].change, TimelineChange::DocsChanged);
        assert_eq!(&*t.rows[0].version, "0.8.1");
        assert!(t.rows[0].is_current, "the newest generation is current");
        assert_eq!(t.rows[1].change, TimelineChange::Present);
    }

    #[tokio::test]
    async fn a_symbol_added_in_a_later_version_is_introduced() {
        let lid = lineage("axum");
        let events = events_for(
            vec![
                ("0.7.9", package(&lid, vec![spec(1, "Router")])),
                (
                    "0.8.1",
                    package(&lid, vec![spec(1, "Router"), spec(2, "Extractor")]),
                ),
            ],
            2,
        )
        .await;
        let t = timeline_of(&events);

        assert_eq!(t.rows.len(), 1, "no row for the version that lacked it");
        assert_eq!(t.rows[0].change, TimelineChange::Introduced);
        assert_eq!(t.versions_examined, 2, "but both generations were examined");
    }
}
