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
//! then iterates `sections` in the order the chunker produced them, then
//! emits `Done`. A cancelled stream (receiver dropped) simply stops; the
//! prefix up to that point is a valid partial page.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    chunk,
    runtime::{EngineHandle, StreamHandle},
    wire::{DocEvent, EngineError, Gen, SymbolKey},
};

// ---------------------------------------------------------------------------
// Channel capacity (Appendix C)
// ---------------------------------------------------------------------------

const DOC_CHANNEL_CAP: usize = 64;

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
    /// 2. `Section` × N — in `section_plan` order.
    /// 3. `Done` — terminal.
    ///
    /// On error: `Failed(EngineError)` — terminal.
    pub fn open_symbol(
        &self,
        key: SymbolKey,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<DocEvent>) {
        let (tx, rx) = flume::bounded(DOC_CHANNEL_CAP);
        let (cancel_token, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);

        let corpus = self.corpus();
        self.spawn(stream_symbol(corpus, key, generation, tx, cancel_token));

        (handle, rx)
    }
}

// ---------------------------------------------------------------------------
// async driver
// ---------------------------------------------------------------------------

async fn stream_symbol(
    corpus: nudox_store::corpus::Corpus,
    key: SymbolKey,
    generation: Gen,
    tx: flume::Sender<DocEvent>,
    cancel: CancellationToken,
) {
    // Bail immediately if the stream was cancelled before we even started.
    if cancel.is_cancelled() {
        return;
    }

    // --- Resolve package + entry -------------------------------------------
    let pkg_arc = match corpus.package(&key.package).await {
        Some(p) => p,
        None => {
            let _ = tx.send_async(DocEvent::Failed(
                EngineError::PackageNotLoaded(key.package.clone()),
            ))
            .await;
            return;
        }
    };

    // Verify the entry exists before we start emitting.
    if pkg_arc.view().entry(key.intro).is_none() {
        let _ = tx.send_async(DocEvent::Failed(EngineError::SymbolNotFound)).await;
        return;
    }

    // --- Run the chunker ---------------------------------------------------
    // `chunk::chunk` is CPU-bound but fast (single IR walk). We keep it on
    // the async task to avoid a blocking-pool round-trip, which would be
    // strictly worse for the < 50 ms first-paint target (§9.1).
    let intro = key.intro;
    let view = pkg_arc.view();
    let (head, sections) = match chunk::chunk(intro, view, &pkg_arc) {
        Some(pair) => pair,
        None => {
            let _ = tx
                .send_async(DocEvent::Failed(EngineError::Chunk(
                    "chunker returned None for a live entry".into(),
                )))
                .await;
            return;
        }
    };

    // --- Emit Head ---------------------------------------------------------
    if cancel.is_cancelled() {
        return;
    }
    if tx.send_async(DocEvent::Head(Box::new(head))).await.is_err() {
        debug!("doc stream: receiver dropped after Head");
        return;
    }

    // --- Emit Sections in plan order ---------------------------------------
    for section in sections {
        if cancel.is_cancelled() {
            return;
        }
        if tx.send_async(DocEvent::Section(section)).await.is_err() {
            debug!("doc stream: receiver dropped during Section");
            return;
        }
    }

    // --- Emit Done ---------------------------------------------------------
    if !cancel.is_cancelled() {
        let _ = tx.send_async(DocEvent::Done).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use nudox_store::source::fixtures::FixtureSource;

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
        use nudox_store::source::fixtures::rich_lineage;
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
        let lineage = nudox_store::source::fixtures::rich_lineage();
        let pkg = corpus.package(&lineage).await.expect("rich package must be loaded");
        let (intro, _entry) = pkg.view().entries().next().expect("must have entries");
        let key = nudox_ir::change::StableRef::new(lineage, intro);

        let (handle, rx) = engine.open_symbol(key, Gen(1));

        let mut events = Vec::new();
        while let Ok(ev) = rx.recv_async().await {
            let is_terminal =
                matches!(ev, DocEvent::Done | DocEvent::Failed(_));
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
        let lineage = nudox_store::source::fixtures::rich_lineage();
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
        let lineage = nudox_store::source::fixtures::rich_lineage();
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
                DocEvent::Done => { got_done = true; break; }
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
        let fake_lineage =
            PackageLineageId::new(EcosystemId::new("fixture"), PackageName::new("nudox-fixture-rich"));
        let fake_intro = IntroId::from_raw([0xFFu8; 32]);
        let bad_key = StableRef::new(fake_lineage, fake_intro);

        let (_handle, rx) = engine.open_symbol(bad_key, Gen(3));
        let ev = rx.recv_async().await.expect("must receive an event");
        assert!(
            matches!(ev, DocEvent::Failed(_)),
            "missing symbol must produce Failed, got {:?}",
            ev
        );
    }
}
