//! Regression test for the "lexical search is permanently empty for every
//! newly ingested package" defect.
//!
//! # Root cause
//!
//! The only Text outbox intent used to come from `CatalogOp::UpsertVersion`
//! (`store::apply::apply_run`), fired at package *registration* time — before
//! any symbol exists. `runtime::text::poll::Poller::poll_once` consumes that
//! intent, calls `GlobalStore::symbols_for`, gets `IndexError::NotFound`
//! (`store::lifecycle::version_record` finds the version row but
//! `symbols_for_version` is empty), maps that to a well-formed *empty* batch,
//! and — like any other intent — advances its durable watermark past it.
//! `store::lifecycle` deliberately does not re-signal Text on later phase
//! transitions (`set_version_lifecycle`'s doc comment), and
//! `coordination::Outbox::record_stored` used to only fan the terminal
//! `Stored` transition out to Vector/Graph, not Text. Net effect: the one
//! Text intent a freshly registered package will ever get is spent while its
//! symbol table is still empty, and nothing ever asks again — the package is
//! lexically unsearchable **forever**, silently (no error, `/readyz` still
//! green).
//!
//! This is the exact sequence `.config/scripts/local-backends.nu`'s `ingest`
//! command works around by re-POSTing `/packages` after `Stored` (see that
//! file's doc comment) — a workaround for this defect, not a fix.
//!
//! # The fix under test
//!
//! `coordination::Outbox::record_stored` now also emits a Text intent
//! (symmetric with the pre-existing Vector/Graph emits) when a generation
//! reaches `Stored` — i.e. once the pipeline has actually written the
//! version's symbols into `symbols_proj`. A freshly registered package now
//! gets two Text intents: the original registration-time one (still empty,
//! still harmless — it also matters for a metadata-only update that never
//! touches `Stored` again) and this new one, which arrives only after real
//! symbols exist and is the one that actually gets them indexed.
//!
//! # Why this is a fast, non-gated unit test rather than a live-stack test
//!
//! `tests/pipeline_end_to_end.rs::tracked_package_is_text_searchable` already
//! covers this end-to-end against a real compiled package and a live
//! qdrant/embeddings stack, gated behind `SERVER_TEST_BACKENDS=1` (see that
//! file). This test drives the exact same catalog-level sequence — register,
//! poll (before symbols exist), materialize symbols, reach `Stored`, poll
//! again — directly against an in-memory catalog engine and a scratch tantivy
//! directory, with no network/compile pipeline involved, so it runs in every
//! `cargo nextest run -p index` invocation and fails fast and deterministically
//! rather than after a 5-minute live-stack timeout.

mod common;

use std::{sync::Arc, time::Duration};

use common::{migrated_writer, stem_id, version_id};

use heart::content::ContentHash;
use index::{
    catalog::{GlobalStore, InstanceToken},
    coordination::Outbox,
    protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates},
    runtime::text::{Poller, TextIndex, query::TextQuery},
    store::{MetaStore, lifecycle::upsert_symbol_projection},
};

/// A symbol name distinctive enough that a substring/token hit could not be
/// an accident of some other fixture.
const SYMBOL_MONIKER: &str = "widgetfixture::make_widget";
const SYMBOL_PLAIN: &str = "make_widget";

#[tokio::test]
async fn text_sink_recovers_after_symbols_land_post_registration() {
    let writer = Arc::new(migrated_writer());

    let stem = stem_id(1);
    let version = version_id(1);

    // Register the package/version. `apply_ops`'s `UpsertVersion` arm emits
    // the *first* Text intent right here, before any symbol exists
    // (store/apply.rs's `CatalogOp::UpsertVersion` arm).
    writer
        .apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem,
                    ecosystem: heart::Language::Rust,
                    name_struct: "pkg:cargo/widgetfixture".into(),
                    name_canonical: "widgetfixture".into(),
                    name_original: "widgetfixture".into(),
                },
                repo_url: None,
            },
            CatalogOp::UpsertVersion {
                coordinates: VersionCoordinates {
                    version_id: version,
                    stem_id: stem,
                    version_canonical: "1.0.0".into(),
                    version_original: "1.0.0".into(),
                },
                published_at: None,
                toolchain: None,
                license: None,
                edges: index::protocol::EdgeSnapshot::unobserved(),
                facets: FacetWire::default(),
                source: None,
            },
        ])
        .expect("register package/version");

    let outbox = Outbox::new(Arc::clone(&writer));
    let global = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/text-outbox").expect("valid instance token"),
    );

    let index_dir = tempfile::tempdir().expect("tempdir");
    let text_index = TextIndex::open_or_create(index_dir.path()).expect("open text index");
    let poller = Poller::new(
        outbox.clone(),
        global.clone(),
        index_dir.path(),
        Duration::from_hours(1),
    );

    // Poll now, exactly as `text_index_poller` would shortly after
    // registration: the package has no symbols yet (compilation has not run).
    // `symbols_for` answers `NotFound`; `poll_once` maps that to an empty
    // batch and still advances the watermark past this intent.
    let after_first_poll = poller.poll_once(&text_index).await.expect("first poll");
    assert_eq!(
        after_first_poll.sequence, 1,
        "the registration-time intent was consumed (watermark advanced past it) \
         even though it carried zero symbols — this is expected/correct, not the bug"
    );
    let no_hit_yet: Vec<_> = {
        use futures::TryStreamExt;
        text_index
            .search(&TextQuery::new(SYMBOL_PLAIN), 10.try_into().unwrap(), None)
            .try_collect()
            .await
            .expect("search answers on an empty index")
    };
    assert!(
        no_hit_yet.is_empty(),
        "the symbol cannot be searchable before it has been written anywhere"
    );

    // Compilation "finishes": the pipeline writes the version's real symbols
    // into the serving projection (this is what `execute_compile_phase` /
    // `ir_stream::ingest_ir_bytes` do in the real pipeline, well before
    // `Stored` is recorded).
    let gen_stamp = [7u8; 32];
    let intro_id = [9u8; 32];
    // `symbols_for` (called by `poll_once` below) parses this token back into
    // a `heart::SymbolKind` via `FromStr` — it must be one of that enum's
    // variant names (`Function`, `Type`, `Module`, `Constant`, `Variable`,
    // `Trait`, `Impl`, `Other`), not an arbitrary shorthand.
    upsert_symbol_projection(
        writer.engine(),
        &intro_id,
        version,
        &gen_stamp,
        SYMBOL_MONIKER,
        "Function",
    )
    .expect("project the real symbol");

    // The terminal `Stored` transition. This is the fix under test: with it,
    // `record_stored` emits a fresh Text intent now that symbols genuinely
    // exist; without it, Text has nothing left to consume ever again (its
    // watermark already passed the only intent it will ever receive).
    outbox
        .record_stored(
            &global,
            version,
            ContentHash::of_bytes(b"fixture-snapshot"),
            None,
        )
        .await
        .expect("record stored");

    // Poll again. Without the fix: no new Text intent exists, so this is a
    // no-op and the symbol is never indexed — the assertion below fails.
    // With the fix: this consumes the new Stored-time intent, indexes the
    // real symbol, and the search below finds it.
    let after_second_poll = poller.poll_once(&text_index).await.expect("second poll");
    assert!(
        after_second_poll.sequence > after_first_poll.sequence,
        "a Stored-time Text intent must exist and be consumed by the next poll \
         (defect: Text was never re-signalled after the registration-time intent, \
         so this would stay stuck at {})",
        after_first_poll.sequence
    );

    let hits: Vec<_> = {
        use futures::TryStreamExt;
        text_index
            .search(&TextQuery::new(SYMBOL_PLAIN), 10.try_into().unwrap(), None)
            .try_collect()
            .await
            .expect("search answers")
    };
    assert!(
        hits.iter().any(|hit| hit.value.package == version),
        "the package's real symbol must be lexically searchable once its \
         generation reaches Stored — got hits: {:?}",
        hits.iter()
            .map(|h| h.value.name.fully_qualified.clone())
            .collect::<Vec<_>>()
    );
}
