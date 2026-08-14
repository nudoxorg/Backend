//! Adversarial coverage for the two `EngineError` enrichments this session
//! added: `PackageNotLoaded::attempted` and `SymbolNotFound::possibly_stale`.
//!
//! # Why these need a real multi-generation / multi-outcome `IrSource`
//!
//! Both fields only mean anything against a *history* the corpus itself does
//! not retain (a failed load, a generation that is no longer current). A
//! fixture-backed single-version engine can never observe either state, so —
//! following this crate's own `versions_adversarial.rs` convention — this
//! file hand-builds a `StaticSource` that replays exactly the sequence of
//! `LoadEvent`s needed, rather than asserting only that *some* error came
//! back (doctrine §4: "a test that would pass against a stub is not a
//! test").

use std::sync::Arc;

use nudox_engine::{
    Engine, EngineConfig,
    wire::{DocEvent, EngineError, Gen, KeyTierName},
};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Deprecation, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::Module,
    package::{Escalation, SealReport},
    view::IrView,
};
use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::{IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, Error},
};

// ---------------------------------------------------------------------------
// Minimal IR helpers (mirrors versions_adversarial.rs's local copies)
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn make_symbol(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::from("src/lib.rs"),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None::<Deprecation>,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn module_entry(sym: Symbol) -> Entry {
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

fn view_with(lid: &PackageLineageId, entries: Vec<(IntroId, Entry)>) -> IrView {
    let mut table = PristineIntroTable::new();
    for (id, entry) in entries {
        table.insert_live(id, entry, None);
    }
    IrView::with_package(lid.clone(), table)
}

// ---------------------------------------------------------------------------
// A source that replays a fixed, hand-authored sequence of LoadEvents
// ---------------------------------------------------------------------------

enum Outcome {
    Ready(PackageLineageId, Option<String>, Arc<PackageView>),
    /// The failure reason, as plain text — `Error` does not derive
    /// `Clone` upstream, so this stores the string and builds a fresh
    /// `Error::Internal` per replay instead.
    Failed(PackageLineageId, String),
}

struct ScriptedSource(Vec<Outcome>);

impl IrSource for ScriptedSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "error-enrichment-scripted".to_owned(),
            package_count_hint: Some(self.0.len() as u32),
        }
    }

    fn load(
        &self,
        _req: LoadRequest,
    ) -> futures::stream::BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, Error>> = self
            .0
            .iter()
            .flat_map(|outcome| match outcome {
                Outcome::Ready(lid, version, pkg) => vec![
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: version.clone(),
                        },
                    }),
                    Ok(LoadEvent::Ready {
                        package: Arc::clone(pkg),
                    }),
                ],
                Outcome::Failed(lid, reason) => vec![
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: None,
                        },
                    }),
                    Ok(LoadEvent::Failed {
                        lineage: lid.clone(),
                        error: Error::Internal(reason.clone()),
                    }),
                ],
            })
            .collect();
        futures::stream::iter(events).boxed()
    }
}

async fn open_and_drain(
    engine: &nudox_engine::EngineHandle,
    key: StableRef,
) -> Vec<DocEvent> {
    let (handle, rx) = engine.open_symbol(key, Gen(1));
    let mut events = Vec::new();
    while let Ok(ev) = rx.recv_async().await {
        let done = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
        events.push(ev);
        if done {
            break;
        }
    }
    drop(handle);
    events
}

// ---------------------------------------------------------------------------
// PackageNotLoaded::attempted
// ---------------------------------------------------------------------------

/// A package whose load was attempted and failed reports *why*, distinct from
/// a lineage the engine never saw at all — the "not loaded vs. failed to
/// lower vs. does not exist" trio the task brief calls out. This engine
/// cannot ever fully separate the third case from the first (it has no
/// external registry), and the test below only asserts the two it can.
#[tokio::test]
async fn package_not_loaded_distinguishes_a_failed_load_from_a_never_requested_one() {
    let broken = lineage("broken-crate");
    let ok = lineage("ok-crate");
    let ok_view = view_with(&ok, vec![(intro(1), module_entry(make_symbol("Thing")))]);
    let ok_pkg = Arc::new(PackageView::build(ok_view, Provenance::TrustedLocal));

    let source = ScriptedSource(vec![
        Outcome::Failed(broken.clone(), "scripted failure: oracle exited 1".to_owned()),
        Outcome::Ready(ok.clone(), Some("1.0.0".to_owned()), ok_pkg),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);

    // Wait for the healthy package to land — proof the engine finished
    // processing both scripted events, including the failure.
    for _ in 0..200 {
        if !engine.versions(&ok).versions.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !engine.versions(&ok).versions.is_empty(),
        "scripted source never settled"
    );

    // Case 1: a lineage whose load was attempted and failed.
    let events = open_and_drain(&engine, StableRef::new(broken.clone(), intro(1))).await;
    match events.last() {
        Some(DocEvent::Failed(EngineError::PackageNotLoaded { package, attempted })) => {
            assert_eq!(package, &broken);
            let attempted = attempted
                .as_ref()
                .expect("a package whose load failed must report why, not a bare not-loaded");
            assert!(
                attempted.contains("scripted failure"),
                "attempted reason must carry the real failure text, got {attempted:?}"
            );
        }
        other => panic!("expected PackageNotLoaded with attempted=Some(..), got {other:?}"),
    }

    // Case 2: a lineage the engine never saw at all.
    let never_seen = lineage("totally-unrelated-package");
    let events = open_and_drain(&engine, StableRef::new(never_seen.clone(), intro(1))).await;
    match events.last() {
        Some(DocEvent::Failed(EngineError::PackageNotLoaded { package, attempted })) => {
            assert_eq!(package, &never_seen);
            assert!(
                attempted.is_none(),
                "a lineage that was never requested must not claim a load was attempted; \
                 got {attempted:?}"
            );
        }
        other => panic!("expected PackageNotLoaded with attempted=None, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// SymbolNotFound::possibly_stale
// ---------------------------------------------------------------------------

/// A key minted at a non-`Structural` tier that stops resolving after a
/// version switch reports the generation and tier it last resolved at,
/// instead of a bare not-found indistinguishable from deletion.
#[tokio::test]
async fn symbol_not_found_reports_the_tier_and_version_where_a_stale_key_still_resolves() {
    let lid = lineage("churny-crate");

    // v1: intro(1) exists, minted at KeyTier::Ordinal (the seal report records
    // it as an escalated/forced key).
    let v1_view = view_with(&lid, vec![(intro(1), module_entry(make_symbol("Thing")))]);
    let v1_report = SealReport {
        forced_keys: vec![(intro(1), Escalation::Ordinal)],
        ..Default::default()
    };
    let v1_pkg = Arc::new(PackageView::build_sealed(
        v1_view,
        Provenance::TrustedLocal,
        &v1_report,
    ));

    // v2: intro(1) is gone (a producer reordering could have moved an
    // Ordinal-tiered declaration to a different id entirely); intro(2) is the
    // only entry, so v2 is non-empty and genuinely current.
    let v2_view = view_with(&lid, vec![(intro(2), module_entry(make_symbol("OtherThing")))]);
    let v2_pkg = Arc::new(PackageView::build(v2_view, Provenance::TrustedLocal));

    let source = ScriptedSource(vec![
        Outcome::Ready(lid.clone(), Some("1.0.0".to_owned()), v1_pkg),
        Outcome::Ready(lid.clone(), Some("2.0.0".to_owned()), v2_pkg),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);

    // Wait for both generations to be recorded and v2.0.0 (newest) current.
    for _ in 0..200 {
        let list = engine.versions(&lid);
        if list.versions.len() == 2 && list.versions.iter().any(|v| v.is_current) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let list = engine.versions(&lid);
    assert_eq!(list.versions.len(), 2, "both generations must be recorded");

    // intro(1) does not resolve in the current (2.0.0) generation.
    let events = open_and_drain(&engine, StableRef::new(lid.clone(), intro(1))).await;
    match events.last() {
        Some(DocEvent::Failed(EngineError::SymbolNotFound { possibly_stale })) => {
            let stale = possibly_stale
                .as_ref()
                .expect("intro(1) resolves under 1.0.0; the lookup must say so, not just fail");
            assert_eq!(&*stale.seen_in_version, "1.0.0");
            assert_eq!(
                stale.tier,
                Some(KeyTierName::Ordinal),
                "the tier reported must be the one the seal report actually recorded"
            );
        }
        other => panic!("expected SymbolNotFound with possibly_stale=Some(..), got {other:?}"),
    }

    // A key that never existed in *any* loaded generation gets no stale hint.
    let events = open_and_drain(&engine, StableRef::new(lid.clone(), intro(99))).await;
    match events.last() {
        Some(DocEvent::Failed(EngineError::SymbolNotFound { possibly_stale })) => {
            assert!(
                possibly_stale.is_none(),
                "a key absent from every loaded generation must not claim staleness; got {possibly_stale:?}"
            );
        }
        other => panic!("expected SymbolNotFound with possibly_stale=None, got {other:?}"),
    }
}
