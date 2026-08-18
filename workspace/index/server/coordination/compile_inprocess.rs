//! In-process compile strategy: the non-Linux twin of
//! [`super::indexing::cage::run_producer_in_cage`] (`// reconcile: in-process
//! (macOS) vs cage (linux)`).
//!
//! There is no ephemeral SmolvmCage available on macOS (no golden rootfs to
//! fork, no `libkrun`), so this module runs the matching `nudox-languages`
//! crate **directly on this host's CPU**, against the same materialized
//! source tree `execute_compile_phase` already writes out for the cage path.
//!
//! # What this does write
//!
//! For every live, public entry the producer's [`nudox_languages::Produced`]
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
//! # The reference section (the Target::Usages fix)
//!
//! It also writes a **real** reference section: the cross-symbol usage graph,
//! recovered from the sealed table's own `Ref` edges via
//! [`super::indexing::build_reference_set_from_table`] — the in-process twin of
//! the cage path's `Bodies`-frame reference derivation, landing in the identical
//! blob `ReferenceSet` format. Before this, the non-Linux path attached an
//! *empty* reference section, so `Target::Usages` ("who uses this symbol")
//! returned nothing for every package indexed on a macOS serving node, across
//! all languages. Now it returns real edges.
//!
//! # What this does NOT write (the IR payload section)
//!
//! It does not reconstruct the producer's wire-format `Vec<OwnedEntryPayload>`
//! IR section that the cage path's `ingest_ir_bytes` decodes from an NdIrF1
//! stream. Doing so faithfully would require a full inverse of `ir_vcs::lower`
//! (semantic `ir::entry::Entry`/`Kind` → wire `SymbolWire`/`KindWire`, all 13
//! discriminants) that nothing in this codebase provides today — `lower.rs`
//! only goes wire → semantic, and the guest producer that emits the `Symbols`
//! stream is out-of-repo. Nothing downstream decodes that section as payloads
//! either: only its byte-integrity is audited (`crate::server::save::blobs`),
//! so the IR blob has been write-only (staged to CAS, never read back) since
//! the cage path was written. Attaching an empty (but valid —
//! `BlobBuilder::finalize` requires the section present) IR section is
//! therefore honest: it does not fabricate wire bytes nobody currently reads,
//! and it does not block emit. Revisit once a forward semantic→wire lowering
//! plus the IR reverse-position index (INDEX-PLAN §5.5 / IP-7, see the `Graph`
//! sink comment in `poll.rs`) need real payload bytes here.

use std::path::Path;

use heart::identity::EntryUri;
use smol_str::SmolStr;

use crate::server::SourceStores;
use crate::server::error::{InternalError, ServerError, ServerResult};
use crate::server::registry::blob::creation::BlobBuilder;
use crate::server::registry::identity::PackageCoordinates;
use crate::server::registry::vector::EmbeddingModel;

/// Run the matching language producer in-process over `source_root` and
/// stage the result: real catalog symbol rows (so the existing vector
/// outbox consumer embeds them into qdrant), a real reference section (the
/// cross-symbol usage graph derived from the sealed table, so `Target::Usages`
/// works for macOS-indexed packages), and an empty-but-valid IR payload section
/// on `builder` (see module docs for why the IR payload stays empty).
///
/// Returns the fully-qualified symbol names contributed — the same shape
/// [`super::indexing::ir_stream::ingest_ir_bytes`] returns for the cage path, used by
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
    let src = nudox_languages::PackageSource::new(source_root.to_path_buf(), name.clone(), version);

    // Producers are synchronous and can be CPU-heavy (ra_ap parsing, javac
    // doclet invocation, …) — run on the blocking pool exactly like the cage
    // path does, so heartbeats/other jobs keep flowing.
    let produce_name = name.clone();
    let produced =
        tokio::task::spawn_blocking(move || run_language_producer(language, &src, &lineage))
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
    // W1: how many rows this package's table actually *offered* for upsert
    // (passed the Public/kind/segments filters), independent of how many
    // succeeded. Distinguishes "this package legitimately has zero public
    // symbols" (attempted == 0, a clean success) from "the catalog rejected
    // every row we tried" (attempted > 0, identifiers stays empty — a real
    // breakage that must not look like a clean empty package).
    let mut attempted: usize = 0;

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

        attempted += 1;
        if let Err(error) = stores
            .global_store
            .upsert_symbol(symbol_id, package, &fully_qualified_name, kind, generation)
            .await
        {
            // Non-fatal per symbol: one bad row must not sink the whole
            // package's indexing job when the rest can still be served.
            // Whether *every* row failing should sink the job is decided
            // once, after the loop — see `upserts_are_degraded`.
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

    // W1: zero *successful* rows out of zero *attempted* rows is an honest
    // empty package (e.g. a crate with no public items) — fine, proceed.
    // Zero successful rows out of one-or-more *attempted* rows means the
    // catalog rejected every single upsert (e.g. it's unreachable), which is
    // producer/host breakage masquerading as an empty package. Fail the job
    // loudly instead of completing as a false "Stored, 0 symbols".
    if upserts_are_degraded(attempted, identifiers.len()) {
        return Err(ServerError::Internal(InternalError::InProcessCompile {
            package: name.clone(),
            reason: format!(
                "all {attempted} catalog symbol upserts failed; refusing to report this as an empty package"
            ),
        }));
    }

    // ── Reference section: the real cross-symbol usage graph ────────────────
    // Recover references from the sealed table's own `Ref` edges — the
    // in-process twin of the cage path's Bodies-frame derivation (see
    // `super::indexing::build_reference_set_from_table` for why the sealed
    // table, and not a reconstructed NdIrF1 stream, is the source here). This
    // is the whole point of the fix: it is what makes `Target::Usages` return
    // real edges for a package indexed on a macOS serving node.
    let owning_pkg = format!("{}:{}", lineage_ecosystem_tag(language), name);
    let ref_set = super::indexing::build_reference_set_from_table(
        table,
        source_root,
        Some(owning_pkg.as_str()),
    );
    let reference_edges: usize = ref_set.by_file.iter().map(|f| f.references.len()).sum();

    if let Err(error) = builder.set_references(&ref_set) {
        // W1: a dropped reference set is a silent loss of this snapshot's
        // cross-reference graph, not a cosmetic degrade — fail the job rather
        // than report a clean `Stored` with no usages (mirrors the cage path's
        // `set_references` handling in `ingest_ir_bytes`). Re-attach an empty
        // set first so the builder still has a valid section if a later stage
        // inspects it.
        let empty = crate::server::registry::blob::ReferenceSet {
            by_file: Vec::new(),
        };
        let _ = builder.set_references(&empty);
        return Err(ServerError::Internal(InternalError::InProcessCompile {
            package: name.clone(),
            reason: format!("staging in-process reference set failed: {error}"),
        }));
    }

    // ── IR payload section: empty-but-valid ─────────────────────────────────
    // There is no forward semantic→wire encoder in the workspace
    // (`ir_vcs::lower` is wire→semantic only, and the guest producer that emits
    // the NdIrF1 `Symbols` stream is out-of-repo), so a faithful
    // `Vec<OwnedEntryPayload>` cannot be rebuilt from the sealed table here.
    // Nothing decodes this section as payloads yet — only its byte-integrity is
    // audited (`crate::server::save::blobs`) — so it stays empty-but-valid; the
    // real reference graph above is the functional fix. Revisit when a
    // semantic→wire lowering + the reverse-`occ` index land (INDEX-PLAN §5.5).
    super::indexing::set_empty_ir_section(builder);

    tracing::info!(
        %package,
        language = language.as_token(),
        entries = table.len(),
        identifiers = identifiers.len(),
        reference_edges,
        "in-process compile produced catalog symbols + reference graph"
    );

    Ok(identifiers)
}

/// Whether an in-process compile's catalog-upsert results indicate producer/
/// catalog breakage rather than a legitimately empty package (W1).
///
/// `attempted` is how many rows this package's producer table offered for
/// upsert (after the Public/kind/segments filters); `succeeded` is how many
/// of those actually landed in the catalog. A package that genuinely has no
/// public symbols attempts zero rows — that is a clean success. A package
/// that attempted at least one row but landed none means every single
/// upsert was rejected (e.g. the catalog was unreachable for the whole
/// loop) — that must not be reported the same way as a clean empty package.
/// A partial success (some rows landed, some didn't) stays non-fatal per
/// existing per-row-resilience policy: the package is still usefully
/// searchable on the symbols that did land.
fn upserts_are_degraded(attempted: usize, succeeded: usize) -> bool {
    attempted > 0 && succeeded == 0
}

#[cfg(test)]
mod tests {
    use super::upserts_are_degraded;

    #[test]
    fn zero_attempted_is_a_clean_empty_package() {
        // A package with no public symbols at all: nothing was ever
        // attempted, so zero successes is the correct, honest outcome.
        assert!(!upserts_are_degraded(0, 0));
    }

    #[test]
    fn all_attempts_failing_is_degraded() {
        // Every attempted upsert was rejected: this must not look like a
        // clean empty package.
        assert!(upserts_are_degraded(5, 0));
    }

    #[test]
    fn partial_success_is_not_degraded() {
        // Existing per-row resilience: some symbols landed, so the package
        // is still usefully searchable — one bad row must not sink the job.
        assert!(!upserts_are_degraded(5, 3));
    }

    #[test]
    fn full_success_is_not_degraded() {
        assert!(!upserts_are_degraded(5, 5));
    }
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
    src: &nudox_languages::PackageSource,
    lineage: &ir::change::PackageLineageId,
) -> Result<nudox_languages::Produced, String> {
    use heart::Language;

    let result = match language {
        Language::Rust => nudox_languages::produce(
            &nudox_languages::rust::RustProducer { direct_repo: false },
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Go => nudox_languages::produce(
            &nudox_languages::go::GoProducer,
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Java => nudox_languages::produce(
            &nudox_languages::java::JavaProducer::new(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::CSharp => nudox_languages::produce(
            &nudox_languages::csharp::CSharpProducer::from_env(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Typescript => nudox_languages::produce(
            &nudox_languages::typescript::TypescriptProducer::new(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        Language::Cpp => nudox_languages::produce(
            &nudox_languages::clang::ClangProducer::new(),
            src,
            lineage,
            &ir::foreign::Unlinked,
        ),
        // No in-process producer is registered: Python only contributes real
        // declarations behind the `pyrefly` feature (not wired into `index` —
        // see `workspace/index/Cargo.toml`).
        Language::Python => {
            return Err(format!(
                "no in-process producer available for language {language}"
            ));
        }
    };

    result.map_err(|err| error_chain(&err))
}

/// Render a `ProducerError` and every link of its `#[source]` chain
/// (docs/AGENTS-DOCTRINE.md §8: never print only the top-level `Display`).
fn error_chain(err: &nudox_languages::ProducerError) -> String {
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
fn fully_qualified_path(
    table: &ir::apply::PristineIntroTable,
    intro: ir::change::IntroId,
) -> Vec<String> {
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
