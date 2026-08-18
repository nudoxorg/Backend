//! `impl heart::surface::Serve<heart::surface::Symbols> for EngineHandle` —
//! the local engine answers the one local/remote search contract
//! (`docs/LOCAL-REMOTE-CONTRACT.md` §2, task S3a).
//!
//! # What this closes
//!
//! `EngineHandle::search(SearchQuery, Gen) -> (StreamHandle,
//! flume::Receiver<SearchEvent>)` is a completely different shape from
//! `heart::surface::Serve<Symbols>::serve(Query, Gen) -> Answer<Symbols>`,
//! carrying a completely different row type (`HitRow` vs. `Scored<SymbolHit>`).
//! This module is the adapter between the two — see
//! `tests/engine/serve_symbols.rs`'s module doc for the full design rationale,
//! in particular why the version has to be retained on `PackageView`
//! (`store::package::PackageView::with_version`) for `SymbolHit::package` to
//! be derivable at all.
//!
//! # Why this spawns on `self.runtime_handle()` rather than taking an
//! injected spawner
//!
//! `heart::surface::Federated<S>` takes a `spawn` closure at construction
//! because it must run under lindsey's non-Tokio GPUI executor as easily as
//! under a server's Tokio one — it has no runtime of its own to assume. This
//! adapter is different: the local engine already owns the process's one
//! Tokio runtime (LR-9), so there is nowhere else for the drain task below to
//! run and no host that would need a different executor here.
//! `EngineHandle::runtime_handle()`'s own doc comment makes exactly this case
//! for the MCP server (`mcp/index.rs:265` borrows it the same way), and the
//! reasoning transfers unchanged.
//!
//! # Requests this adapter cannot honour
//!
//! `Query::at` (point-in-time) and `Query::page.cursor` (resume token) both
//! fail the request with `Frame::Failed` rather than being silently ignored —
//! see `unsupported_reason`'s doc comment. `routing`/`session` are hints and
//! are dropped; `rank`/`mode` select behaviour the engine already applies
//! unconditionally, so ignoring them changes nothing observable.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use heart::query::{Query, StableReference, Target};
use heart::surface::{
    Answer, Emitter, Residence, SearchNote, Serve, Signature, SigToken as HeartSigToken,
    StreamHandle as SurfaceStreamHandle, Summary, SymbolHit, Symbols, answer_channel,
};
use heart::{Coordinates, Language, PackageVersion, RegistryOrigin, Score, Scored, SymbolKind};
use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName as LineagePackageName};
use nudox_ir::kind::KindDiscriminant;
use smol_str::SmolStr;

use crate::runtime::EngineHandle;
use crate::search::SearchQuery;
use crate::store::corpus::Corpus;
use crate::wire::{Gen as EngineGen, HitRow, KindTag, SearchEvent, SigToken as EngineSigToken};

/// The `Emitter` budget for one `serve` call — items and notes above this are
/// parked (not dropped: the drain loop below always uses the `_async` emit
/// paths, see `answer_channel`'s own doc comment for why that distinction
/// matters).
const ANSWER_CAPACITY: usize = 128;

// ---------------------------------------------------------------------------
// In-flight search bookkeeping
// ---------------------------------------------------------------------------

/// Count of `Serve<Symbols>::serve` calls whose drain task is still running —
/// i.e. has not yet observed the underlying engine search end, whether by
/// completing (`Done`/`Failed`) or by being cancelled (the channel closing
/// with neither).
static ACTIVE_SYMBOL_SEARCHES: AtomicUsize = AtomicUsize::new(0);

/// How many local `Serve<Symbols>` searches this process currently has in
/// flight.
///
/// Exists so a caller — a test, a status indicator — can observe that
/// dropping an `Answer` actually stopped the underlying engine work rather
/// than merely closing a channel nobody is reading from any more (see
/// `Answer::with_handle`'s doc comment on why a `StreamHandle` has to be
/// attached at all).
pub fn active_symbol_searches() -> usize {
    ACTIVE_SYMBOL_SEARCHES.load(Ordering::Acquire)
}

/// RAII guard held for the lifetime of one drain task — see
/// [`active_symbol_searches`].
struct SearchGuard;

impl SearchGuard {
    fn start() -> Self {
        ACTIVE_SYMBOL_SEARCHES.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for SearchGuard {
    fn drop(&mut self) {
        ACTIVE_SYMBOL_SEARCHES.fetch_sub(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------
// Serve<Symbols>
// ---------------------------------------------------------------------------

impl Serve<Symbols> for EngineHandle {
    fn serve(&self, request: Query, generation: heart::surface::Gen) -> Answer<Symbols> {
        if let Some(reason) = unsupported_reason(&request) {
            return Answer::failed(generation, heart::WireError::BadRequest(reason));
        }

        let search_query = to_search_query(&request);
        let engine_generation = EngineGen(generation.0);
        let (engine_handle, rx) = self.search(search_query, engine_generation);
        let corpus = self.corpus();

        let (emitter, answer) = answer_channel::<Symbols>(ANSWER_CAPACITY, generation);

        // The engine owns the runtime this drains on — see this module's
        // doc comment for why no spawner is injected.
        self.runtime_handle().spawn(drain(rx, corpus, emitter));

        // `engine_handle` (the engine's own `crate::StreamHandle`) is not
        // `Clone`; wrapping it in an `Arc` lets the closure below call
        // `.cancel()` (an `&self` method, and idempotent — it wraps a
        // `CancellationToken`) without needing ownership, and keeps it alive
        // for exactly as long as the returned `Answer`'s attached
        // `StreamHandle` is.
        let engine_handle = Arc::new(engine_handle);
        let stream_handle = SurfaceStreamHandle::new(generation, move || {
            engine_handle.cancel();
        });

        answer.with_handle(stream_handle)
    }
}

// ---------------------------------------------------------------------------
// Request validation and conversion
// ---------------------------------------------------------------------------

/// Why `request` cannot be honoured, if it cannot be.
///
/// # `at` and `page.cursor` must fail, not be ignored
///
/// `at: Some(_)` asks for a point-in-time snapshot; the engine has no `AsOf`
/// mechanism at all, so answering with current data would be a confidently
/// wrong answer presented as a historical one, with nothing to tell the
/// caller apart from a genuine point-in-time answer.
///
/// `page.cursor: Some(_)` asks to resume a previous page; `collect_name_hits`
/// (the engine's own search collector) has a top-`N` limit and no offset
/// concept, so silently ignoring the cursor would return page one labelled as
/// whatever page the caller asked to resume — silently, forever.
///
/// Both are worse than refusing outright, and neither is detectable by the
/// caller unless this adapter refuses. `target != Symbols` is the one
/// remaining runtime-checkable mismatch `Query` (shared across all three
/// search surfaces) cannot rule out at the type level.
fn unsupported_reason(request: &Query) -> Option<String> {
    if !matches!(request.target, Target::Symbols) {
        return Some(format!(
            "the local engine's Symbols adapter cannot answer target {:?}",
            request.target
        ));
    }
    if request.at.is_some() {
        return Some(
            "the local engine has no AsOf mechanism; answering a point-in-time request with \
             current data would be a confidently wrong answer"
                .to_owned(),
        );
    }
    if request.page.cursor.is_some() {
        return Some(
            "the local engine's search has a top-N limit with no offset/cursor concept; \
             honouring a cursor by ignoring it would silently return page one labelled as a \
             later page"
                .to_owned(),
        );
    }
    None
}

/// Convert the shared `heart::query::Query` into the engine's own
/// `SearchQuery`.
///
/// Supported: `text` and `page.limit`. `scope.packages`/`scope.ecosystems`
/// are translated into `SearchQuery.packages` only when cheap to do so
/// honestly — see the comment inline. `routing`/`session` are hints with no
/// local analogue and are dropped; `rank`/`mode` select behaviour the engine
/// already applies unconditionally (there is exactly one ranking formula and
/// one query mode locally), so ignoring them changes nothing observable.
fn to_search_query(request: &Query) -> SearchQuery {
    let mut search_query = SearchQuery {
        text: request.text.clone(),
        limit: request.page.limit as usize,
        ..SearchQuery::default()
    };

    // A `PackageLineageId` needs both an ecosystem and a name. `Query::scope`
    // carries them as two independent lists (`packages: Vec<String>`,
    // `ecosystems: Vec<Language>`), with no declared pairing between them.
    // Only when exactly one ecosystem is named is the pairing unambiguous —
    // every package stem belongs to that one ecosystem. Two or more
    // ecosystems for N stems has no natural cross product a caller actually
    // meant, so it is left unfiltered here rather than guessed; guessing
    // wrong would silently narrow results to packages that were never asked
    // to be excluded.
    if !request.scope.packages.is_empty()
        && let [only] = request.scope.ecosystems.as_slice()
    {
        let tag = only.lineage_tag();
        search_query.packages = request
            .scope
            .packages
            .iter()
            .map(|name| PackageLineageId::new(EcosystemId::new(tag), LineagePackageName::new(name.clone())))
            .collect();
    }

    search_query
}

// ---------------------------------------------------------------------------
// Drain: SearchEvent -> Frame<Symbols>
// ---------------------------------------------------------------------------

/// Drain the engine's `SearchEvent` stream onto `emitter`, converting each
/// `HitRow` into a `Scored<SymbolHit>` along the way.
///
/// Uses the `_async` emit paths throughout (`item_async`/`note_async`), which
/// park for room instead of dropping a frame on a full channel — this task
/// has a runtime to park on (it is itself spawned on one), so real
/// backpressure is strictly better than `push`'s drop-and-report (see
/// `heart::surface::answer_channel`'s doc comment on the two emit paths).
async fn drain(rx: flume::Receiver<SearchEvent>, corpus: Corpus, emitter: Emitter<Symbols>) {
    let _guard = SearchGuard::start();
    let mut delivered: u64 = 0;

    while let Ok(event) = rx.recv_async().await {
        match event {
            SearchEvent::Section { rows, .. } | SearchEvent::Merge { rows, .. } => {
                for row in rows.iter() {
                    let Some(scored) = build_symbol_hit(&corpus, row).await else {
                        continue;
                    };
                    if emitter.item_async(scored, Residence::Local).await.is_err() {
                        // The `Answer` side was dropped — the consumer is
                        // gone, so there is no point continuing to drain.
                        return;
                    }
                    delivered += 1;
                }
            }
            SearchEvent::Latency { elapsed, .. } => {
                let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
                let _ = emitter.note_async(SearchNote::Latency { millis }).await;
            }
            // `SectionState` (Complete/Building/Unavailable per section) has
            // no honest `SearchNote` analogue: `SearchNote::Coverage` needs
            // counts this event does not carry, and `SearchNote` generalises
            // what `SectionState`/`Latency` mean rather than mirroring every
            // local-only event 1:1 (see `SearchNote`'s own doc comment).
            // Emitting nothing is more honest than inventing a lossy mapping.
            SearchEvent::SectionState { .. } => {}
            SearchEvent::Done { .. } => {
                let _ = emitter.end(Summary::complete(delivered));
                return;
            }
            SearchEvent::Failed { error, .. } => {
                let _ = emitter.failed(heart::WireError::Internal(error.to_string()));
                return;
            }
        }
    }
    // The channel closed with no terminal frame — the search was cancelled
    // (dropping the `Answer` fired the attached `StreamHandle`'s canceller,
    // which cancelled the engine's own search task, which dropped its
    // `SearchEvent` sender). Nothing further to report: `emitter` is about to
    // be dropped without ever having sent a terminal frame, and
    // `Answer::recv` already treats a disconnect exactly like a terminal
    // frame for a consumer still reading (see its own doc comment) — there
    // is no consumer left here regardless, since cancellation is what
    // produced this state.
}

/// Convert one `HitRow` into a `Scored<SymbolHit>`, or `None` (logged) when
/// the conversion cannot be done honestly.
///
/// # The version is the one genuine blocker
///
/// `SymbolHit::package` is a `PackageId` whose hash seed includes the
/// concrete version (`heart/package/coordinates.rs`). A package whose
/// resident version is unknown, whose lineage names an ecosystem this build
/// does not map to a registry, or whose version string does not parse under
/// that ecosystem's own grammar cannot be given a *correct* `PackageId` —
/// never guess one; a wrong id breaks cross-plane dedup silently, which is
/// the exact failure class this adapter exists to remove. Each of those three
/// cases skips the hit and logs why, rather than fabricating an id.
async fn build_symbol_hit(corpus: &Corpus, row: &HitRow) -> Option<Scored<SymbolHit>> {
    let lineage = &row.key.package;

    let package_view = match corpus.package(lineage).await {
        Some(view) => view,
        None => {
            tracing::warn!(
                package = %lineage,
                "search hit named a package no longer resident in the corpus; skipping"
            );
            return None;
        }
    };

    let version = match package_view.version() {
        Some(version) => version,
        None => {
            tracing::warn!(
                package = %lineage,
                "package has no resident version recorded; cannot mint a cross-plane \
                 PackageId, skipping hit"
            );
            return None;
        }
    };

    let language = match Language::from_lineage_tag(lineage.ecosystem.as_str()) {
        Some(language) => language,
        None => {
            tracing::warn!(
                ecosystem = lineage.ecosystem.as_str(),
                "unknown lineage ecosystem tag; skipping hit"
            );
            return None;
        }
    };

    let package_version = match PackageVersion::try_from((language, version)) {
        Ok(version) => version,
        Err(error) => {
            tracing::warn!(
                package = %lineage,
                version,
                %error,
                "package version does not parse under its ecosystem's grammar; skipping hit"
            );
            return None;
        }
    };

    let name_str = lineage.name.as_str();
    let package_id = Coordinates {
        origin: RegistryOrigin::default_for(language),
        name: heart::package::PackageName::from_canonical(language, name_str, name_str),
        version: package_version,
    }
    .id();

    // O(1) precomputed lookup — not `qualified_display_name`'s output, which
    // collapses segments and conditionally prefixes the package name for
    // *display*; `path` is half of `Symbols::key`, so it must be the raw
    // fully-qualified moniker, not a display string.
    let path = package_view
        .indexes()
        .path_of(row.key.intro)
        .map_or_else(
            || SmolStr::new(&*row.display_name),
            |p| SmolStr::new(p.as_ref()),
        );

    let display_name = SmolStr::new(&*row.display_name);
    let kind = map_symbol_kind(row.kind);
    let signature = if row.sig_preview.is_empty() {
        None
    } else {
        Some(Signature::new(
            row.sig_preview.iter().map(convert_sig_token).collect(),
        ))
    };
    let reference = stable_reference(&row.key.package, row.key.intro);

    let score = match Score::try_new(row.score) {
        Ok(score) => score,
        Err(error) => {
            tracing::warn!(%error, "search hit carried a non-finite score; skipping");
            return None;
        }
    };

    Some(Scored::new(
        SymbolHit {
            package: package_id,
            path,
            display_name,
            ecosystem: language,
            kind,
            signature,
            reference,
        },
        score,
    ))
}

/// `F:<eco>/<pkg>#<hex>` — the frozen `StableReference` grammar
/// (`heart/query/mod.rs`). **Not** the engine's own `serialize_symbol_key`
/// spelling (`eco:pkg#hex`, `wire/mod.rs`) — the two must not be confused.
fn stable_reference(package: &PackageLineageId, intro: nudox_ir::change::IntroId) -> Option<StableReference> {
    StableReference::parse(&format!(
        "F:{}/{}#{}",
        package.ecosystem.as_str(),
        package.name.as_str(),
        intro.to_hex(),
    ))
    .ok()
}

/// `KindTag` (13 known `KindDiscriminant`s, plus `Unknown`) -> the coarser,
/// 8-variant `SymbolKind`.
///
/// Mirrors `index/server/coordination/compile_inprocess.rs::map_symbol_kind`
/// **except** for `Field`/`Param`: that precedent returns `None` (drops them)
/// because it builds a catalog that does not want member kinds at all. Local
/// search deliberately returns them — `search/hits.rs`'s
/// `is_member_kind`/`kind_weight` score them down rather than exclude them —
/// so dropping them here would silently narrow what local search returns
/// today. They map to `SymbolKind::Other`, the same as `KindTag::Unknown`,
/// which is what that variant is for.
fn map_symbol_kind(kind: KindTag) -> SymbolKind {
    match kind {
        KindTag::Known(disc) => match disc {
            KindDiscriminant::Module => SymbolKind::Module,
            KindDiscriminant::Record | KindDiscriminant::Alias | KindDiscriminant::Enum => {
                SymbolKind::Type
            }
            KindDiscriminant::Function => SymbolKind::Function,
            KindDiscriminant::Trait => SymbolKind::Trait,
            KindDiscriminant::Impl => SymbolKind::Impl,
            KindDiscriminant::Const => SymbolKind::Constant,
            KindDiscriminant::Static => SymbolKind::Variable,
            KindDiscriminant::Reexport | KindDiscriminant::Variant => SymbolKind::Other,
            KindDiscriminant::Field | KindDiscriminant::Param => SymbolKind::Other,
        },
        KindTag::Unknown(_) => SymbolKind::Other,
    }
}

/// `nudox_engine::wire::SigToken` -> `heart::surface::SigToken`. The two
/// enums are variant-for-variant identical (7 each); only `Ty`'s link target
/// needs real conversion (`Option<SymbolKey>` -> `Option<StableReference>`),
/// since a `SymbolKey` is local-engine-internal and must never cross the wire
/// as identity (`LOCAL-REMOTE-CONTRACT.md` §0.7).
fn convert_sig_token(token: &EngineSigToken) -> HeartSigToken {
    match token {
        EngineSigToken::Kw(s) => HeartSigToken::Kw(SmolStr::new(*s)),
        EngineSigToken::Ident(s) => HeartSigToken::Ident(SmolStr::new(&**s)),
        EngineSigToken::Ty { text, target } => HeartSigToken::Ty {
            text: SmolStr::new(&**text),
            target: target
                .as_ref()
                .and_then(|key| stable_reference(&key.package, key.intro)),
        },
        EngineSigToken::Punct(s) => HeartSigToken::Punct(SmolStr::new(*s)),
        EngineSigToken::Ws => HeartSigToken::Ws,
        EngineSigToken::Generic(s) => HeartSigToken::Generic(SmolStr::new(&**s)),
        EngineSigToken::Lifetime(s) => HeartSigToken::Lifetime(SmolStr::new(&**s)),
    }
}
