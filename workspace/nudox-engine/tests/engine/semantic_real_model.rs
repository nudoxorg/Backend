#![cfg(feature = "fixtures")]

//! Product semantic-search smoke test against the pinned local ONNX model.
//!
//! This test deliberately has no skip path and never contacts a service. The
//! model directory is supplied by the reproducible Nix `semantic-model`
//! package through `NUDOX_EMBED_MODEL_DIR`.

use std::sync::Arc;

use futures::stream::BoxStream;
use nudox_engine::embed::load_from_env;
use nudox_engine::search::SECTION_SEMANTIC;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::{
    Error as SourceError, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor,
};
use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, SearchQuery, SectionState};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::entry::{Entry, Node, Symbol, Visibility};
use nudox_ir::index::RawRef;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::Module;
use nudox_ir::view::IrView;

fn small_lineage() -> PackageLineageId {
    PackageLineageId::new(
        EcosystemId::new("semantic-restart"),
        PackageName::new("small-fixture"),
    )
}

fn small_package() -> Arc<PackageView> {
    let lineage = small_lineage();
    let symbol = Symbol {
        name: "Coordinate".to_owned(),
        visibility: Visibility::Public,
        documentation: "A point in a two dimensional coordinate system.".to_owned(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut table = PristineIntroTable::new();
    table.insert_live(
        IntroId::from_raw([1; 32]),
        Entry::new(
            symbol,
            Node::build(None::<RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );
    Arc::new(PackageView::build(
        IrView::with_package(lineage, table),
        Provenance::TrustedLocal,
    ))
}

struct SmallSource;

impl IrSource for SmallSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "semantic-restart-fixture".to_owned(),
            package_count_hint: Some(1),
        }
    }

    fn load(&self, _request: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>> {
        let lineage = small_lineage();
        Box::pin(futures::stream::iter([
            Ok(LoadEvent::Discovered {
                lineage: lineage.clone(),
                hint: PackageHint {
                    display_name: "small-fixture".to_owned(),
                    ecosystem: "semantic-restart".to_owned(),
                    version: Some("1.0.0".to_owned()),
                },
            }),
            Ok(LoadEvent::Ready {
                package: small_package(),
            }),
        ]))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_local_model_returns_semantic_results() {
    let model_dir = std::env::var("NUDOX_EMBED_MODEL_DIR")
        .expect("NUDOX_EMBED_MODEL_DIR must point at the Nix-pinned local model");
    assert!(
        std::path::Path::new(&model_dir)
            .join("model.onnx")
            .is_file(),
        "pinned model is missing model.onnx: {model_dir}"
    );

    let embedder = load_from_env().expect("onnx feature plus model directory installs embedder");
    let engine = Engine::start_with_fixtures(EngineConfig {
        embedder: Some(embedder),
        ..EngineConfig::default()
    });

    let packages = engine.packages();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match tokio::time::timeout_at(deadline, packages.recv_async()).await {
            Ok(Ok(nudox_engine::PackageLoadEvent::Loaded { .. })) => break,
            Ok(Ok(_)) => {},
            Ok(Err(error)) => panic!("fixture loading failed: {error}"),
            Err(elapsed) => panic!("fixture corpus did not load within 30 seconds"),
        }
    }

    let mut state = None;
    let mut semantic_rows = 0;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let (_stream, events) = engine.search(
            SearchQuery {
                text: "a point in a two dimensional coordinate system".to_owned(),
                limit: 10,
                ..SearchQuery::default()
            },
            Gen(1),
        );
        let mut observed_state = None;
        let mut observed_rows = 0;
        while let Ok(event) = events.recv_async().await {
            match event {
                SearchEvent::SectionState {
                    section,
                    state: value,
                    ..
                } if section == SECTION_SEMANTIC => observed_state = Some(value),
                SearchEvent::Section { section, rows, .. }
                | SearchEvent::Merge { section, rows, .. }
                    if section == SECTION_SEMANTIC =>
                {
                    observed_rows += rows.len();
                }
                SearchEvent::Done { .. } => break,
                SearchEvent::Failed { error, .. } => {
                    panic!("semantic search failed: {error:?}")
                }
                _ => {}
            }
        }
        state = observed_state;
        semantic_rows = observed_rows;
        if state == Some(SectionState::Complete) && semantic_rows > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }

    assert_eq!(state, Some(SectionState::Complete));
    assert!(
        semantic_rows > 0,
        "the pinned local model produced no semantic results"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn persisted_semantic_index_survives_engine_restart() {
    let embedder = load_from_env().expect("the Nix model must install an embedder");
    let state = tempfile::tempdir().expect("semantic state directory");
    let index_file = state.path().join("semantic-index.json");

    {
        let engine = Engine::start(
            EngineConfig {
                embedder: Some(embedder),
                semantic_index_dir: Some(state.path().to_owned()),
                ..EngineConfig::default()
            },
            SmallSource,
        );
        let packages = engine.packages();
        while let Ok(event) = packages.recv_async().await {
            if matches!(event, PackageLoadEvent::Loaded { .. }) {
                break;
            }
        }

        // Wait on the durable write, not on repeated query embeddings. The
        // latter contend with the document embedder's single ORT session and
        // can starve the package that is making the write we need to prove.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            if index_file.is_file() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "initial semantic index was not persisted"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let (_stream, events) = engine.search(
            SearchQuery {
                text: "a point in a two dimensional coordinate system".to_owned(),
                limit: 10,
                ..SearchQuery::default()
            },
            Gen(10),
        );
        let mut rows = 0;
        while let Ok(event) = events.recv_async().await {
            match event {
                SearchEvent::Section {
                    section,
                    rows: batch,
                    ..
                }
                | SearchEvent::Merge {
                    section,
                    rows: batch,
                    ..
                } if section == SECTION_SEMANTIC => rows += batch.len(),
                SearchEvent::Done { .. } => break,
                _ => {}
            }
        }
        assert!(rows > 0, "initial semantic index produced no results");
    }

    assert!(
        index_file.is_file(),
        "successful indexing must persist {}",
        index_file.display()
    );

    let reopened = Engine::start(
        EngineConfig {
            embedder: Some(load_from_env().expect("the Nix model must install an embedder")),
            semantic_index_dir: Some(state.path().to_owned()),
            ..EngineConfig::default()
        },
        SmallSource,
    );
    let packages = reopened.packages();
    while let Ok(event) = packages.recv_async().await {
        if matches!(event, PackageLoadEvent::Loaded { .. }) {
            break;
        }
    }

    let (_stream, events) = reopened.search(
        SearchQuery {
            text: "a point in a two dimensional coordinate system".to_owned(),
            limit: 10,
            ..SearchQuery::default()
        },
        Gen(11),
    );
    let mut rows = 0;
    while let Ok(event) = events.recv_async().await {
        match event {
            SearchEvent::Section {
                section,
                rows: batch,
                ..
            }
            | SearchEvent::Merge {
                section,
                rows: batch,
                ..
            } if section == SECTION_SEMANTIC => {
                rows += batch.len();
            }
            SearchEvent::Done { .. } => break,
            SearchEvent::Failed { error, .. } => panic!("search failed: {error:?}"),
            _ => {}
        }
    }
    assert!(rows > 0, "reopened semantic index returned no results");
}

/// The application-facing configuration must be useful without manually
/// threading an embedder or persistence directory through the host.
///
/// The Nix semantic check provides the model directory. This test supplies an
/// isolated state directory through the same environment variable used by the
/// default configuration, then proves that a second default-config engine can
/// search the persisted vectors.
#[tokio::test(flavor = "multi_thread")]
async fn default_config_searches_after_restart() {
    let state = tempfile::tempdir().expect("semantic state directory");
    let index_file = state.path().join("semantic-index.json");
    unsafe {
        std::env::set_var("NUDOX_SEMANTIC_INDEX_DIR", state.path());
    }

    let engine = Engine::start(EngineConfig::default(), SmallSource);
    let packages = engine.packages();
    while let Ok(event) = packages.recv_async().await {
        if matches!(event, PackageLoadEvent::Loaded { .. }) {
            break;
        }
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    while !index_file.is_file() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "default configuration did not persist its semantic index"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    drop(engine);

    let reopened = Engine::start(EngineConfig::default(), SmallSource);
    let packages = reopened.packages();
    while let Ok(event) = packages.recv_async().await {
        if matches!(event, PackageLoadEvent::Loaded { .. }) {
            break;
        }
    }

    let (_stream, events) = reopened.search(
        SearchQuery {
            text: "a point in a two dimensional coordinate system".to_owned(),
            limit: 10,
            ..SearchQuery::default()
        },
        Gen(12),
    );
    let mut rows = 0;
    while let Ok(event) = events.recv_async().await {
        match event {
            SearchEvent::Section {
                section,
                rows: batch,
                ..
            }
            | SearchEvent::Merge {
                section,
                rows: batch,
                ..
            } if section == SECTION_SEMANTIC => rows += batch.len(),
            SearchEvent::Done { .. } => break,
            SearchEvent::Failed { error, .. } => panic!("search failed: {error:?}"),
            _ => {}
        }
    }
    assert!(
        rows > 0,
        "default configuration returned no semantic rows after restart"
    );
}
