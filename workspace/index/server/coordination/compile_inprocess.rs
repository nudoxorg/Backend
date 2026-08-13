//! In-process compile strategy: the non-Linux twin of
//! [`super::indexing::run_producer_in_cage`] (`// reconcile: in-process
//! (macOS) vs cage (linux)`).
//!
//! There is no ephemeral SmolvmCage available on macOS (no golden rootfs to
//! fork, no `libkrun`), so this module runs the matching `nudox-producer-*`
//! crate **directly on this host's CPU**, against the same materialized
//! source tree `execute_compile_phase` already writes out for the cage path.
//!
//! # What this does write
//!
//! For every live, public entry the producer's [`nudox_producer::Produced`]
//! table contains, this writes a real catalog `symbols_proj` row via
//! [`crate::catalog::GlobalStore::upsert_symbol`] — the exact projection
//! [`super::indexing`]'s vector outbox consumer
//! (`crate::server::poll::materialize_vector`) reads via
//! `GlobalStore::symbols_for` to embed and upsert into qdrant. That consumer
//! already runs unconditionally after every emit (`SinkKind::Vector` is
//! fanned out for every package), so writing these rows here is the whole
//! fix: no other wiring is needed for `/search` to return real hits for an
//! in-process-compiled package.
//!
//! # What this does NOT write
//!
//! It does not reconstruct the producer's wire-format `Vec<OwnedEntryPayload>`
//! IR section that the cage path's `ingest_ir_bytes` decodes from an
//! NdIrF1 stream. Doing so faithfully would require a full inverse of
//! `ir_vcs::lower` (semantic `ir::entry::Entry`/`Kind` → wire
//! `SymbolWire`/`KindWire`, all 13 discriminants) that nothing in this
//! codebase provides today — `lower.rs` only goes wire → semantic. Nothing
//! downstream currently decodes that section back out, either: grepping the
//! whole `index` crate for `upsert_symbol` callers before this file existed
//! turned up none, which means the IR blob has been write-only (staged to
//! CAS, never read back) since the cage path was written. Attaching empty
//! (but valid — `BlobBuilder::finalize` requires both sections present)
//! IR/reference sections is therefore honest: it doesn't fabricate wire
//! bytes nobody currently reads, and it doesn't block emit. Revisit once the
//! IR reverse-position index (INDEX-PLAN §5.5 / IP-7, see the `Graph` sink
//! comment in `poll.rs`) needs real bytes here.

use std::path::Path;

use heart::identity::EntryUri;
use smol_str::SmolStr;

use crate::server::error::{InternalError, ServerError, ServerResult};
use crate::server::registry::blob::creation::BlobBuilder;
use crate::server::registry::identity::PackageCoordinates;
use crate::server::registry::vector::EmbeddingModel;
use crate::server::SourceStores;

/// Run the matching language producer in-process over `source_root` and
/// stage the result: real catalog symbol rows (so the existing vector
/// outbox consumer embeds them into qdrant) plus empty-but-valid IR/
/// reference sections on `builder` (see module docs for why they're empty).
///
/// Returns the fully-qualified symbol names contributed — the same shape
/// [`super::indexing::ingest_ir_bytes`] returns for the cage path, used by
/// `execute_emit_phase` for facet/keyword extraction.
pub(crate) async fn compile_in_process<M: EmbeddingModel>(
    stores: &SourceStores<M>,
    package: heart::PackageId,
    coordinates: &PackageCoordinates,
    builder: &mut BlobBuilder,
    source_root: &Path,
) -> ServerResult<Vec<String>> {
    let language = coordinates.ecosystem();
    let name = coordinates.name.canonical().to_owned();
    let version = coordinates.version.canonical().to_owned();

    let lineage = ir::change::PackageLineageId::new(
        ir::change::EcosystemId::new(lineage_ecosystem_tag(language)),
        ir::change::PackageName::new(name.clone()),
    );
    let src = nudox_producer::PackageSource::new(source_root.to_path_buf(), name.clone(), version);

    // Producers are synchronous and can be CPU-heavy (ra_ap parsing, javac
    // doclet invocation, …) — run on the blocking pool exactly like the cage
    // path does, so heartbeats/other jobs keep flowing.
    let produce_name = name.clone();
    let produced = tokio::task::spawn_blocking(move || run_language_producer(language, &src, &lineage))
        .await
        .map_err(|join| {
            ServerError::Internal(InternalError::InProcessCompile {
                package: produce_name.clone(),
                reason: format!("producer task panicked or was cancelled: {join}"),
            })
        })?
        .map_err(|reason| {
            ServerError::Internal(InternalError::InProcessCompile {
                package: name.clone(),
                reason,
            })
        })?;

    // ── Stage real symbol rows into the catalog's serving projection ──────────
    // `provisional_generation()` is a valid, deterministic `ContentHash` over
    // everything staged on `builder` so far (the extract phase's source
    // files); it is not the final emitted snapshot hash (that isn't known
    // until `execute_emit_phase` finalizes), but `symbols_for_version` filters
    // only on `version_id`, never on `gen_stamp` (see
    // `store::lifecycle::symbols_for_version`), so an interim generation
    // doesn't gate reads — it only occupies the `on_conflict` key.
    let generation = builder.provisional_generation();
    let table = &produced.table;
    let mut identifiers = Vec::new();

    for (intro, entry) in table.live_entries() {
        if entry.sym().visibility != ir::entry::Visibility::Public {
            continue;
        }
        let Some(discriminant) = entry.kind().discriminant() else {
            continue; // a Reexport/alias Reference entry, not an owned kind
        };
        let Some(kind) = map_symbol_kind(discriminant) else {
            continue; // Field / Param: structural children, not search targets
        };

        let segments = fully_qualified_path(table, intro);
        if segments.is_empty() {
            continue;
        }
        let fully_qualified_name = segments.join("::");

        let uri = EntryUri {
            package,
            path: segments.iter().map(|s| SmolStr::from(s.as_str())).collect(),
        };
        let symbol_id = stores.global_store.symbol_id(&uri);

        if let Err(error) = stores
            .global_store
            .upsert_symbol(symbol_id, package, &fully_qualified_name, kind, generation)
            .await
        {
            // Non-fatal per symbol: one bad row must not sink the whole
            // package's indexing job when the rest can still be served.
            tracing::warn!(
                %package,
                symbol = %fully_qualified_name,
                error = %error,
                "catalog symbol upsert failed; skipping this symbol"
            );
            continue;
        }
        identifiers.push(fully_qualified_name);
    }

    // ── Empty-but-valid IR/reference sections ──────────────────────────────
    // See module docs: nothing downstream decodes these yet.
    super::indexing::attach_empty_ir_sections(builder);

    tracing::info!(
        %package,
        language = language.as_token(),
        entries = table.len(),
        identifiers = identifiers.len(),
        "in-process compile produced catalog symbols"
    );

    Ok(identifiers)
}

/// Run the producer registered for `language` (mirrors
/// `crates/nudox-store/src/source/producer.rs`'s `ProducerRegistry`, but
/// self-contained: `index` owns these producer crates directly under its
/// `server` feature rather than depending on the GUI-local `nudox-store`
/// crate — see `workspace/index/Cargo.toml`).
///
/// Returns a human-readable reason string on failure (already flattened —
/// callers wrap it in a typed `ServerError`).
fn run_language_producer(
    language: heart::Language,
    src: &nudox_producer::PackageSource,
    lineage: &ir::change::PackageLineageId,
) -> Result<nudox_producer::Produced, String> {
    use heart::Language;

    let result = match language {
        Language::Rust => nudox_producer::produce(
            &nudox_producer_rust::RustProducer { direct_repo: false },
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Go => nudox_producer::produce(
            &nudox_producer_go::GoProducer,
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Java => nudox_producer::produce(
            &nudox_producer_java::JavaProducer::new(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::CSharp => nudox_producer::produce(
            &nudox_producer_csharp::CSharpProducer::from_env(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Typescript => nudox_producer::produce(
            &nudox_producer_typescript::TypescriptProducer::new(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Cpp => nudox_producer::produce(
            &nudox_producer_clang::ClangProducer::new(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        // No in-process producer is registered for these: Python only
        // contributes real declarations behind the `pyrefly` feature (not
        // wired into `index` — see `workspace/index/Cargo.toml`), and there
        // is no `nudox-producer-nix` crate in the workspace at all.
        Language::Python | Language::Nix => {
            return Err(format!(
                "no in-process producer available for language {language}"
            ));
        }
    };

    result.map_err(|err| error_chain(&err))
}

/// Render a `ProducerError` and every link of its `#[source]` chain
/// (AGENTS-DOCTRINE.md §8: never print only the top-level `Display`).
fn error_chain(err: &nudox_producer::ProducerError) -> String {
    std::iter::successors(Some(err as &dyn std::error::Error), |e| {
        std::error::Error::source(*e)
    })
    .map(|e| e.to_string())
    .collect::<Vec<_>>()
    .join(": ")
}

/// The ecosystem tag used for this producer run's [`ir::change::PackageLineageId`]
/// (an internal identity used for cross-package unlinked-reference bookkeeping
/// during lowering — not the wire `heart::RegistryOrigin`). Mirrors the tags
/// `nudox-store`'s `PackageDescriptor` named constructors use.
fn lineage_ecosystem_tag(language: heart::Language) -> &'static str {
    use heart::Language;
    match language {
        Language::Rust => "cargo",
        Language::Go => "go",
        Language::Typescript => "npm",
        Language::Java => "maven",
        Language::CSharp => "nuget",
        Language::Python => "pypi",
        Language::Cpp => "cpp",
        Language::Nix => "nix",
    }
}

/// Map an owned [`ir::kind::KindDiscriminant`] onto the coarser
/// [`heart::SymbolKind`] the serving projection stores. `None` for kinds that
/// are structural children rather than independently-searchable symbols
/// (`Field`, `Param`) — they still exist in the table and are still walked
/// for `fully_qualified_path`, they just don't get their own catalog row.
fn map_symbol_kind(discriminant: ir::kind::KindDiscriminant) -> Option<heart::SymbolKind> {
    use ir::kind::KindDiscriminant as K;
    Some(match discriminant {
        K::Module => heart::SymbolKind::Module,
        K::Record | K::Alias | K::Enum => heart::SymbolKind::Type,
        K::Function => heart::SymbolKind::Function,
        K::Trait => heart::SymbolKind::Trait,
        K::Impl => heart::SymbolKind::Impl,
        K::Const => heart::SymbolKind::Constant,
        K::Static => heart::SymbolKind::Variable,
        K::Reexport | K::Variant => heart::SymbolKind::Other,
        K::Field | K::Param => return None,
    })
}

/// Walk `intro`'s parent chain in `table` to build its `::`-joined
/// fully-qualified path (root-first), e.g. `["mycrate", "widgets", "Widget",
/// "new"]`. Used both for the catalog's display name and (via
/// [`EntryUri`]) for the deterministic symbol id — so two entries that
/// share a bare name in different modules never collide.
fn fully_qualified_path(table: &ir::apply::PristineIntroTable, intro: ir::change::IntroId) -> Vec<String> {
    let mut segments = Vec::new();
    let mut cursor = Some(intro);
    // Defensive cycle guard: `PristineIntroTable` never admits a parent
    // cycle (LR/seal invariants), but this walk must never hang if that
    // invariant is ever violated by a future producer bug.
    for _ in 0..256 {
        let Some(id) = cursor else { break };
        let Some(entry) = table.get(id) else { break };
        segments.push(entry.sym().name.clone());
        cursor = table.parent_of(id);
    }
    segments.reverse();
    segments
}
