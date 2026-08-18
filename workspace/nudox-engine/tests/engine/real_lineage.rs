//! Real-corpus content lineage.
//!
//! This is intentionally a normal (non-ignored) gate.  The Nix corpus provides
//! the two `log` checkouts used here; a missing checkout is a fixture failure,
//! not a reason to silently skip the test.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use nudox_engine::{
    Engine, EngineConfig, EngineHandle, PackageHistorySpec, PackageVersionSpec, ProducerLanguage,
    SearchQuery,
    wire::{DocEvent, Gen, HitRow, SearchEvent, SymbolKey, VersionEvent},
};
use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};

const OLDER: &str = "0.4.17";
const NEWER: &str = "0.4.33";
const SYMBOL: &str = "STATIC_MAX_LEVEL";

fn corpus_root(package: &str, version: &str) -> PathBuf {
    let root = std::env::var_os("NUDOX_CORPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../result"));
    root.join(format!("{package}-{version}"))
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("log"))
}

fn history_spec() -> PackageHistorySpec {
    PackageHistorySpec {
        name: "log".to_owned(),
        language: ProducerLanguage::Rust,
        versions: vec![
            PackageVersionSpec {
                root: corpus_root("log", OLDER),
                version: OLDER.to_owned(),
            },
            PackageVersionSpec {
                root: corpus_root("log", NEWER),
                version: NEWER.to_owned(),
            },
        ],
    }
}

fn settle(engine: &EngineHandle, lid: &PackageLineageId) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let versions = engine.versions(lid);
        if versions.len() == 2 && versions.current().is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "real log lineage did not settle: {:?}",
            versions
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn search_exact(engine: &EngineHandle, name: &str) -> Vec<HitRow> {
    let (handle, rx) = engine.search(
        SearchQuery {
            text: name.to_owned(),
            ..Default::default()
        },
        Gen(1),
    );
    let mut hits = Vec::new();
    while let Ok(event) = rx.recv() {
        match event {
            SearchEvent::Section { rows, .. } | SearchEvent::Merge { rows, .. } => {
                hits.extend(
                    rows.iter()
                        .filter(|row| &*row.display_name == name)
                        .cloned(),
                );
            }
            SearchEvent::Done { .. } => break,
            SearchEvent::Failed { error, .. } => panic!("search failed: {error:?}"),
            _ => {}
        }
    }
    drop(handle);
    hits
}

fn open_and_drain(engine: &EngineHandle, key: SymbolKey, generation: Gen) -> Vec<DocEvent> {
    let (handle, rx) = engine.open_symbol(key, generation);
    let mut events = Vec::new();
    while let Ok(event) = rx.recv() {
        let done = matches!(event, DocEvent::Done | DocEvent::Failed(_));
        events.push(event);
        if done {
            break;
        }
    }
    drop(handle);
    events
}

fn source_excerpt(events: &[DocEvent]) -> &str {
    events
        .iter()
        .find_map(|event| match event {
            DocEvent::Head(head) => head.source_excerpt.as_deref(),
            _ => None,
        })
        .expect("real symbol must carry exact source content")
}

#[test]
fn real_version_selection_changes_symbol_source_content() {
    assert!(
        corpus_root("log", OLDER).join("Cargo.toml").is_file(),
        "Nix real corpus is missing log {OLDER}"
    );
    assert!(
        corpus_root("log", NEWER).join("Cargo.toml").is_file(),
        "Nix real corpus is missing log {NEWER}"
    );

    let lid = lineage();
    let engine = Engine::start_with_versions(EngineConfig::default(), vec![history_spec()]);
    settle(&engine, &lid);

    let versions = engine.versions(&lid);
    assert_eq!(
        versions
            .versions
            .iter()
            .map(|v| &*v.version)
            .collect::<Vec<_>>(),
        vec![NEWER, OLDER],
        "both real generations must be retained, newest first"
    );
    assert_eq!(&*versions.current().unwrap().version, NEWER);

    // The declaration exists in both real releases, but its exact source
    // differs (`STATIC_MAX_LEVEL` changed implementation).  Read the current
    // page before switching, then read it again after switching.
    let newer_hits = search_exact(&engine, SYMBOL);
    assert_eq!(
        newer_hits.len(),
        1,
        "newer real source must expose {SYMBOL}"
    );
    let newer_events = open_and_drain(&engine, newer_hits[0].key.clone(), Gen(1));
    assert!(
        !newer_events
            .iter()
            .any(|event| matches!(event, DocEvent::Failed(_))),
        "newer real symbol must resolve: {newer_events:?}"
    );
    let newer_source = source_excerpt(&newer_events).to_owned();

    let rx = engine.select_version(lid.clone(), OLDER, Gen(2));
    assert!(
        matches!(
            rx.recv().expect("select_version must emit one event"),
            VersionEvent::Switched {
                generation: Gen(2),
                ..
            }
        ),
        "selecting the loaded older generation must switch"
    );
    assert_eq!(&*engine.versions(&lid).current().unwrap().version, OLDER);

    let older_hits = search_exact(&engine, SYMBOL);
    assert_eq!(
        older_hits.len(),
        1,
        "older real source must expose {SYMBOL}"
    );
    let older_events = open_and_drain(&engine, older_hits[0].key.clone(), Gen(2));
    assert!(
        !older_events
            .iter()
            .any(|event| matches!(event, DocEvent::Failed(_))),
        "older real symbol must resolve: {older_events:?}"
    );
    let older_source = source_excerpt(&older_events);

    assert_ne!(
        newer_source, older_source,
        "select_version must change actual source content, not only version metadata; \
         newer={newer_source:?}, older={older_source:?}"
    );
}
