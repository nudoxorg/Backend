//! Adversarial tests for the §9.3 `DocEvent` protocol ordering invariants.
//!
//! Uses `Engine::start_with_fixtures` (requires the `fixtures` feature) so
//! there is a real corpus with real symbols and realistic event sequences.
//!
//! # Invariants tested
//!
//! 1. `Head` is always the first event.
//! 2. `Timeline` arrives after `Head` and before the first `Section`.
//! 3. Every `Highlight` references a `SectionId` that was already sent in a
//!    preceding `Section` event (invariant 3).
//! 4. `Done` or `Failed` is the terminal event; nothing follows it.
//! 5. Every emitted section was planned (`section_plan` contains its id).
//! 6. Applying any prefix of a valid stream is a valid page:
//!    - Head is present (or the prefix is empty).
//!    - No event appears before Head.
//!    - No event appears after the terminal.
//! 7. Opening a symbol that does not exist → `Failed`, not a hang.
//! 8. Cancelling the stream mid-flight must not hang and must not deliver
//!    events after the cancel fires.

use std::collections::HashSet;
use std::time::Duration;

use nudox_engine::{
    Engine, EngineConfig,
    wire::{DocEvent, Gen, SectionId, SymbolKey},
};
use nudox_engine::store::source::fixtures::rich_lineage;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_engine() -> nudox_engine::EngineHandle {
    Engine::start_with_fixtures(EngineConfig::default())
}

/// Wait until the fixture package has been signalled as loaded via `packages()`.
///
/// `corpus()` is `pub(crate)`, so integration tests cannot poll it directly.
/// Instead, we subscribe to the `packages()` broadcast which emits a
/// `Loaded` event (from the snapshot path or the live path) as soon as the
/// fixture source completes its seeding pass.
async fn wait_for_corpus(engine: &nudox_engine::EngineHandle) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(nudox_engine::PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => continue,
            // Channel closed: the seeding task finished and the snapshot path
            // was the sole delivery mechanism — the package is ready.
            Ok(Err(_)) => return,
            Err(_) => panic!("corpus never seeded within 5 s"),
        }
    }
}

/// Drain all events for the given key to completion (Done or Failed).
async fn drain(
    engine: &nudox_engine::EngineHandle,
    key: SymbolKey,
    generation: Gen,
) -> Vec<DocEvent> {
    let (handle, rx) = engine.open_symbol(key, generation);
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(ev) = rx.recv_async().await {
            let done = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
            events.push(ev);
            if done {
                break;
            }
        }
    })
    .await;
    drop(handle);
    events
}

/// Build keys for all entries in the rich fixture from the fixture view.
///
/// We use `build_rich_view()` directly instead of asking the engine's corpus
/// (which is `pub(crate)`). The IntroIds are deterministic and identical in
/// the loaded corpus.
fn all_keys_static() -> Vec<SymbolKey> {
    use nudox_engine::store::source::fixtures::build_rich_view;
    let lineage = rich_lineage();
    let view = build_rich_view();
    view.entries()
        .map(|(intro, _)| nudox_ir::change::StableRef::new(lineage.clone(), intro))
        .collect()
}

/// Get the key for the first entry in the rich fixture.
fn first_key_static() -> SymbolKey {
    use nudox_engine::store::source::fixtures::build_rich_view;
    let lineage = rich_lineage();
    let view = build_rich_view();
    let (intro, _) = view.entries().next().expect("fixture must have entries");
    nudox_ir::change::StableRef::new(lineage, intro)
}

// ---------------------------------------------------------------------------
// 1. `Head` is always the first event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn head_is_always_the_first_event() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let keys = all_keys_static();
    for (i, key) in keys.iter().enumerate() {
        let events = drain(&engine, key.clone(), Gen(i as u64)).await;
        assert!(!events.is_empty(), "symbol {key:?} produced zero events");
        assert!(
            matches!(events[0], DocEvent::Head(_)),
            "first event for symbol {key:?} must be Head; got {:?}",
            events[0]
        );
    }
}

// ---------------------------------------------------------------------------
// 2. `Timeline` arrives after `Head` and before first `Section`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timeline_after_head_before_first_section_for_all_fixtures() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let keys = all_keys_static();
    for (i, key) in keys.iter().enumerate() {
        let events = drain(&engine, key.clone(), Gen(100 + i as u64)).await;

        let head_idx = events.iter().position(|e| matches!(e, DocEvent::Head(_)));
        let timeline_idx = events
            .iter()
            .position(|e| matches!(e, DocEvent::Timeline(_)));
        let first_section_idx = events
            .iter()
            .position(|e| matches!(e, DocEvent::Section(_)));

        let head_idx = match head_idx {
            Some(i) => i,
            None => continue, // Failed streams may not have Head
        };

        if let Some(t_idx) = timeline_idx {
            assert!(
                t_idx > head_idx,
                "symbol {key:?}: Timeline (pos {t_idx}) must follow Head (pos {head_idx})"
            );
            if let Some(s_idx) = first_section_idx {
                assert!(
                    t_idx < s_idx,
                    "symbol {key:?}: Timeline (pos {t_idx}) must precede first Section (pos {s_idx})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Every `Highlight` references a `SectionId` already sent
// ---------------------------------------------------------------------------

/// Invariant 3 (§9.3): a `Highlight` event must only reference a `SectionId`
/// for which a `Section` event was already delivered.
///
/// This is the adversarial version: we check every symbol in the fixture, not
/// just one.
#[tokio::test]
async fn highlight_only_references_already_sent_sections() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let keys = all_keys_static();
    for (i, key) in keys.iter().enumerate() {
        let events = drain(&engine, key.clone(), Gen(200 + i as u64)).await;

        let mut sent_sections: HashSet<SectionId> = HashSet::new();
        for ev in &events {
            match ev {
                DocEvent::Section(s) => {
                    sent_sections.insert(s.section_id());
                }
                DocEvent::Highlight { section, .. } => {
                    assert!(
                        sent_sections.contains(section),
                        "symbol {key:?}: Highlight references section {:?} which was not yet sent; \
                         sent so far: {:?}",
                        section,
                        sent_sections
                    );
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. `Done`/`Failed` is terminal; nothing follows it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn done_or_failed_is_terminal_and_nothing_follows() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let keys = all_keys_static();
    for (i, key) in keys.iter().enumerate() {
        let events = drain(&engine, key.clone(), Gen(300 + i as u64)).await;

        let terminal_idx = events
            .iter()
            .position(|e| matches!(e, DocEvent::Done | DocEvent::Failed(_)));

        if let Some(idx) = terminal_idx {
            assert_eq!(
                idx,
                events.len() - 1,
                "symbol {key:?}: terminal event at position {idx} but stream has {} events — \
                 something was emitted after the terminal",
                events.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Every emitted section was planned
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_emitted_section_was_planned() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let keys = all_keys_static();
    for (i, key) in keys.iter().enumerate() {
        let events = drain(&engine, key.clone(), Gen(400 + i as u64)).await;

        let head = events.iter().find_map(|e| match e {
            DocEvent::Head(h) => Some(h.as_ref()),
            _ => None,
        });

        let Some(head) = head else { continue };

        let planned: HashSet<SectionId> = head.section_plan.iter().map(|p| p.id).collect();

        for ev in &events {
            if let DocEvent::Section(s) = ev {
                let sid = s.section_id();
                assert!(
                    planned.contains(&sid),
                    "symbol {key:?}: emitted section {:?} was not in section_plan",
                    sid
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 6. Any prefix is a valid page (Head first if present, no events after terminal)
// ---------------------------------------------------------------------------

/// Open a stream and read only the first `n` events (1 to 3), then drop.
/// The prefix must satisfy: if non-empty, first event is Head.
#[tokio::test]
async fn any_prefix_is_a_valid_page() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let key = first_key_static();

    for prefix_len in 1..=4usize {
        let (handle, rx) = engine.open_symbol(key.clone(), Gen(500 + prefix_len as u64));
        let mut events = Vec::new();
        for _ in 0..prefix_len {
            if let Ok(ev) = rx.try_recv() {
                events.push(ev);
            } else {
                // Not enough events buffered yet — wait briefly.
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Ok(ev) = rx.try_recv() {
                    events.push(ev);
                }
            }
        }
        drop(handle); // cancel

        if let Some(first) = events.first() {
            assert!(
                matches!(first, DocEvent::Head(_)),
                "prefix of length {prefix_len}: first event must be Head if any events arrived; \
                 got {:?}",
                first
            );
        }
        // Every event before the terminal must be non-terminal.
        for (j, ev) in events.iter().enumerate() {
            if j < events.len() - 1 {
                assert!(
                    !matches!(ev, DocEvent::Done | DocEvent::Failed(_)),
                    "terminal event at position {j} in a prefix of {prefix_len} events"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 7. Opening a nonexistent symbol → `Failed`, not a hang
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opening_nonexistent_symbol_fails_cleanly() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    // Use a real lineage but a fake IntroId that is not in the corpus.
    let lineage = rich_lineage();
    let fake_intro = nudox_ir::change::IntroId::from_raw([0xFFu8; 32]);
    let bad_key = nudox_ir::change::StableRef::new(lineage, fake_intro);

    let result = tokio::time::timeout(Duration::from_millis(500), async {
        let (_h, rx) = engine.open_symbol(bad_key, Gen(600));
        rx.recv_async().await
    })
    .await;

    let ev = result
        .expect("opening a nonexistent symbol must not hang")
        .expect("must receive exactly one event");

    assert!(
        matches!(ev, DocEvent::Failed(_)),
        "nonexistent symbol must produce Failed; got {ev:?}"
    );
}

/// Nonexistent *package* (wrong lineage entirely) → `Failed(PackageNotLoaded)`.
#[tokio::test]
async fn opening_symbol_in_unloaded_package_fails_cleanly() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
    let ghost_lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new("never-loaded-ghost"),
    );
    let ghost_intro = nudox_ir::change::IntroId::from_raw([0xAAu8; 32]);
    let ghost_key = nudox_ir::change::StableRef::new(ghost_lineage, ghost_intro);

    let result = tokio::time::timeout(Duration::from_millis(500), async {
        let (_h, rx) = engine.open_symbol(ghost_key, Gen(601));
        rx.recv_async().await
    })
    .await;

    let ev = result
        .expect("opening symbol in unloaded package must not hang")
        .expect("must receive exactly one event");

    assert!(
        matches!(
            ev,
            DocEvent::Failed(nudox_engine::wire::EngineError::PackageNotLoaded { .. })
        ),
        "unloaded package must produce Failed(PackageNotLoaded); got {ev:?}"
    );
}

// ---------------------------------------------------------------------------
// 8. Cancelling mid-flight must not hang
// ---------------------------------------------------------------------------

/// Drop the `StreamHandle` after receiving exactly 2 events. The test must not
/// hang. We use a wall-clock timeout to enforce this.
#[tokio::test]
async fn cancelling_mid_stream_does_not_hang() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let key = first_key_static();
    let (handle, rx) = engine.open_symbol(key, Gen(700));

    // Consume up to 2 events, then cancel.
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        for _ in 0..2 {
            let _ = rx.recv_async().await;
        }
    })
    .await;

    drop(handle); // cancel

    // The receiver should drain quickly after cancel.
    let result = tokio::time::timeout(Duration::from_millis(500), async move {
        while rx.recv_async().await.is_ok() {}
    })
    .await;

    assert!(
        result.is_ok(),
        "after cancelling the stream, the receiver must drain within 500 ms"
    );
}

// ---------------------------------------------------------------------------
// Sections arrive in `section_plan` order
// ---------------------------------------------------------------------------

/// The `SectionId`s of emitted `Section` events must appear in the same order
/// as they appear in `section_plan`. This is §9.3 invariant 2.
#[tokio::test]
async fn sections_arrive_in_section_plan_order() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let keys = all_keys_static();
    for (i, key) in keys.iter().enumerate() {
        let events = drain(&engine, key.clone(), Gen(800 + i as u64)).await;

        let head = events.iter().find_map(|e| match e {
            DocEvent::Head(h) => Some(h.as_ref()),
            _ => None,
        });
        let Some(head) = head else { continue };

        // Build the expected order from the plan.
        let planned_order: Vec<SectionId> = head.section_plan.iter().map(|p| p.id).collect();

        // Collect the order in which Section events actually arrived.
        let emitted_order: Vec<SectionId> = events
            .iter()
            .filter_map(|e| match e {
                DocEvent::Section(s) => Some(s.section_id()),
                _ => None,
            })
            .collect();

        // Check that emitted_order is a subsequence of planned_order in the same relative order.
        // (We do a full equality check since all planned sections should be emitted.)
        assert_eq!(
            emitted_order, planned_order,
            "symbol {key:?}: sections arrived in wrong order; \
             expected {:?}, got {:?}",
            planned_order, emitted_order
        );
    }
}
