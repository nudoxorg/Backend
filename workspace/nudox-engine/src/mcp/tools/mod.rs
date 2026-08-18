//! The MCP tools of §L6, and the argument/result types their schemas
//! are derived from.
//!
//! # LR-8: this is a view, not a second engine
//!
//! The read/query tools bottom out in these `EngineHandle` calls. Names below
//! are the `do_*` methods — the stable internal surface `tests/mcp_dump_responses.rs`
//! is deliberately written against — not the MCP tool names, which are the
//! consolidated nine of docs/MCP-SURFACE-PLAN.md §5.1 (`search`, `read`,
//! `refs`, `packages`, `select_version`, `diff`, `index`, `graph`, `schema`);
//! `server.rs` maps each tool onto one or more of these:
//!
//! | `do_*` method | Engine call |
//! |---|---|
//! | `do_search`, `do_unified_search` (`search`) | [`EngineHandle::search`] |
//! | `do_get_symbol_compact` (`read`)  | current package view through the shared chunk-head projection |
//! | `do_get_symbols` (`read`, batched) | the same compact projection, deduplicated inside one MCP call |
//! | `do_find_usages` (`refs`, `direction: in`) | [`EngineHandle::query`] with `crate::graph::queries::FIND_USAGES` |
//! | `do_list_packages` (`packages`) | [`EngineHandle::query`] with [`PACKAGES_QUERY`] |
//! | `do_list_versions` (`packages`) | [`EngineHandle::versions`] |
//! | `do_select_version` (`select_version`) | [`EngineHandle::select_version`], drained to completion |
//! | `do_graph_query` (`graph`) | [`EngineHandle::query`] with the caller's query |
//! | `graph_schema`'s body (`schema`) | [`crate::mcp::schema_card::SCHEMA_CARD`] by default, [`crate::mcp::SCHEMA_SDL`] verbatim when `full: true` (LR-7) |
//!
//! `find_usages` and `list_packages` reach the store's usage postings and the
//! corpus's package map the way §L6 asks — *through the engine* — by going over
//! the Trustfall plane rather than by taking a second handle on `Corpus`. That
//! is deliberate and it is the stronger reading of LR-8/LR-9: `EngineHandle`
//! does not expose `corpus()` publicly, the engine owns the `LocalSet` the
//! `!Send` Trustfall stream requires, and routing through `query` means these
//! two tools cannot drift away from what the graph plane says is true.
//!
//! # Streams are consumed to the tool's canonical projection
//!
//! Full-document tools drain to `Done`/`Failed`. Compact reads resolve the
//! current package view directly and use the shared head projection with no
//! section plan, timeline, markdown walk, channel, or cancellation race.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::semantic::SectionState;
use crate::wire::{
    DocEvent, Gen, HitRow, KindDiscriminant, KindTag, PackageDiff, QueryEvent, RenderSection,
    SearchEvent, SigToken, SourceLocation, SymbolHead, Timeline, VersionEvent,
};
use crate::{EngineHandle, GraphQuery, SearchQuery};
use futures::future::try_join_all;

use crate::mcp::error::McpError;
use crate::mcp::key::{PackageLineageDto, SymbolKeyDto};

mod args;
mod results;

pub use args::{
    DiffVersionsArgs, FindUsagesArgs, GetOccurrencesArgs, GetSymbolArgs, GetSymbolsArgs,
    GraphQueryArgs, GraphSchemaArgs, IndexArgs, IndexPackageArgs, KeysArg, ListPackagesArgs,
    ListVersionsArgs, PackagesArgs, ReadArgs, RefsArgs, RefsDirection, SearchSymbolsArgs,
    SelectVersionArgs, SemanticSearchArgs, SymbolFormat,
};
pub use results::{
    CompactSymbolDoc, CompactSymbolReference, DiffVersionsResult, EdgeCoverageNote, FailedTarget,
    GetOccurrencesResult, IndexPackageResult, IndexResult, IndexedPackage, IntegrityReport,
    ListVersionsResult, LoadedPackagesResult, OccurrenceRow, OccurrenceSymbol, PackageSummary,
    PackageWithVersions, PackagesResult, QueryResult, QueryResultRow, ReferenceCoverage,
    RefsResult, RunningTarget, SchemaResult, SearchHitDoc, SearchResult, SelectVersionResult,
    SemanticHitRow, SemanticSearchResult, SemanticStatus, SymbolDoc, SymbolsResult,
    UnscannedDependencies, UsageRow, UsagesResult, VersionSummary,
};

/// The Trustfall query behind `list_packages`.
///
/// Kept minimal on purpose: it asks only for the three scalar properties
/// `schema.graphql` guarantees on `Package`, so it cannot break when the
/// vertex gains edges.
pub const PACKAGES_QUERY: &str = r"
query {
    Packages {
        lineage @output
        name @output
        ecosystem @output
    }
}
";

/// The exact-occurrence query behind `get_occurrences`.
pub const GET_OCCURRENCES_QUERY: &str = r#"
{
    Symbols {
        key @filter(op: "=", value: ["$key"])
        key @output(name: "ownerKey")
        signature @output(name: "ownerSignature")
        path @output(name: "ownerPath")
        occurrencesOf {
            targetKey @output
            referenceKind @output
            confidence @output
            spanStart @output
            spanEnd @output
            target @optional {
                key @output(name: "targetResolvedKey")
                signature @output(name: "targetSignature")
                path @output(name: "targetPath")
            }
        }
    }
}
"#;

/// Edge pairs that answer the same relational question for disjoint sets of
/// languages — `(dead edge name, the `TypePosition` it reads, the edge that
/// answers instead)`. See [`NudoxTools::edge_coverage_note`]'s doc comment for
/// how this is used and its documented limitations.
///
/// `Trait.implementors` reads `TypePosition::ImplementedTrait`, which only
/// Rust's `impl` blocks ever populate; `subtypes` reads
/// `TypePosition::SuperType`, which every *other* supported language
/// populates and Rust never does (`schema.graphql`'s notes on both edges, and
/// `workspace/compiler/languages/tests/producer_capabilities.rs`, are the
/// authoritative source for this split — this list must stay consistent with
/// both, not redefine the mapping a third time).
const DEAD_EDGE_PAIRS: &[(&str, crate::store::package::TypePosition, &str)] = &[
    (
        "implementors",
        crate::store::package::TypePosition::ImplementedTrait,
        "subtypes",
    ),
    (
        "subtypes",
        crate::store::package::TypePosition::SuperType,
        "implementors",
    ),
];

/// Default number of rows a tool returns when the caller does not say.
const DEFAULT_LIMIT: usize = 50;

/// Hard ceiling on rows, regardless of what the caller asks for.
///
/// An MCP result is injected into a model's context window; an unbounded
/// answer is not a useful answer. Callers that need more should narrow the
/// query or paginate with `graph_query`.
const MAX_LIMIT: usize = 500;

/// How long `index_package` waits by default before reporting a job as still
/// running.
///
/// Two minutes covers a cold fetch and lowering of most single packages
/// (`serde` is well under it) without approaching the timeouts an HTTP client
/// or proxy is likely to impose.
const DEFAULT_INDEX_WAIT_SECONDS: u32 = 120;

/// Ceiling on how many dependency hops `index` will follow.
///
/// Three is already generous: the closure grows multiplicatively, and the
/// question one hop answers — "open the type this function returns" — is
/// answered at hop one in essentially every case. A caller that genuinely
/// wants a deep closure is better served naming what it wants, because it
/// then finds out which packages it got.
const MAX_DEPENDENCY_DEPTH: u32 = 3;

/// Ceiling on `index_package`'s wait, whatever the caller asks for.
///
/// Beyond ten minutes the request is a liability rather than a convenience: the
/// job runs to completion regardless, and polling is free.
const MAX_INDEX_WAIT_SECONDS: u32 = 600;

/// Flatten [`crate::Integrity`] into the tool's report shape.
///
/// The two variants must not collapse: `verified_against_published_digest` is
/// the whole distinction, and `detail` carries either the endpoint that
/// substantiates it or the specific reason none exists.
fn report_integrity(integrity: &crate::Integrity) -> IntegrityReport {
    match integrity {
        crate::Integrity::RegistryDigest {
            algorithm,
            digest,
            published_by,
        } => IntegrityReport {
            verified_against_published_digest: true,
            algorithm: algorithm.clone(),
            digest: digest.clone(),
            detail: format!("matched the digest published by {published_by}"),
        },
        crate::Integrity::TransportOnly { sha256, why } => IntegrityReport {
            verified_against_published_digest: false,
            algorithm: "sha256".to_owned(),
            digest: sha256.clone(),
            detail: format!(
                "computed locally; no published digest was available to compare against — {why}"
            ),
        },
        // A working tree has no digest worth reporting: it changes under the
        // reader, so any hash would be stale before it was rendered. The
        // honest report is the *provenance* — that this documentation
        // describes a checkout, which may contain code that exists in no
        // published release — rather than an unverified-looking digest field.
        crate::Integrity::Local { root } => IntegrityReport {
            verified_against_published_digest: false,
            algorithm: "none".to_owned(),
            digest: String::new(),
            detail: format!(
                "produced from the working tree at {root}; not a published \
                 release, so there is no digest to verify against"
            ),
        },
        // `Integrity` is `#[non_exhaustive]`: a further tier must not be
        // silently reported as verified.
        _ => IntegrityReport {
            verified_against_published_digest: false,
            algorithm: "unknown".to_owned(),
            digest: String::new(),
            detail: "this build does not recognise the integrity tier this fetch reported"
                .to_owned(),
        },
    }
}

// ---------------------------------------------------------------------------
// The tool set
// ---------------------------------------------------------------------------

/// The engine-backed implementation of the eight §L6 tools.
///
/// Holds the logic; [`crate::mcp::NudoxMcpServer`] holds the MCP surface that calls
/// it. The split keeps every tool body testable without standing up a
/// transport, and keeps `#[tool_router]`'s macro expansion off this file.
///
/// Cheap to clone: an `EngineHandle` is an `Arc` pair.
#[derive(Clone)]
pub struct NudoxTools {
    /// The one engine every tool goes through (LR-8).
    engine: EngineHandle,
    /// Monotonic generation counter. The engine tags every stream event with
    /// the `Gen` it answers; giving each tool call a fresh one keeps two
    /// concurrent MCP calls from reading each other's events.
    generation: std::sync::Arc<AtomicU64>,
    /// Index jobs in flight, keyed by canonical PURL — see [`crate::mcp::index`].
    ///
    /// On `NudoxTools` and not on the server, because rmcp builds one
    /// `NudoxMcpServer` per MCP *session*: a registry living there would let
    /// two agents (or one agent reconnecting) each start their own producer run
    /// for the same package.
    jobs: std::sync::Arc<crate::mcp::index::IndexJobs>,
}

impl std::fmt::Debug for NudoxTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NudoxTools").finish_non_exhaustive()
    }
}

impl NudoxTools {
    /// Build the tool set over an already-started engine.
    pub fn new(engine: EngineHandle) -> Self {
        Self {
            engine,
            generation: std::sync::Arc::new(AtomicU64::new(1)),
            jobs: std::sync::Arc::new(crate::mcp::index::IndexJobs::default()),
        }
    }

    /// The engine this tool set views.
    pub fn engine(&self) -> &EngineHandle {
        &self.engine
    }

    /// Take the next stream generation.
    fn next_gen(&self) -> Gen {
        Gen(self.generation.fetch_add(1, Ordering::Relaxed))
    }
}

// ---------------------------------------------------------------------------
// Tool bodies — called by the `#[tool]` methods in `crate::mcp::server`
// ---------------------------------------------------------------------------

impl NudoxTools {
    /// `search_symbols`, minus the MCP wrapping.
    pub async fn do_search(&self, args: SearchSymbolsArgs) -> Result<SearchResult, McpError> {
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let (kinds, exclude_kinds, packages, excluded_kinds) =
            resolve_search_scope(args.kinds.as_ref(), args.packages.as_ref())?;

        // Fetch one row past the page so we can tell whether another page
        // exists (`fetched_may_have_capped` below) without a second query.
        // `SearchQuery.limit` bounds *each section independently*, so this is
        // the effective per-section fetch depth, not the final page size.
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);

        let query = SearchQuery {
            text: args.query,
            kinds,
            exclude_kinds,
            packages,
            limit: fetch_limit,
        };
        let generation = self.next_gen();
        // Bind the handle: dropping it cancels the stream mid-drain.
        let (_stream, rx) = self.engine.search(query, generation);

        let mut rows: Vec<HitRow> = Vec::new();
        // True when any single section returned exactly `fetch_limit` rows —
        // i.e. that section's own truncation may have hidden more.
        let mut fetched_may_have_capped = false;
        let mut terminated = false;
        let mut local_sections_seen = [false; 2];
        while let Ok(event) = rx.recv_async().await {
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
                } if section != crate::search::SECTION_SEMANTIC => {
                    if batch.len() >= fetch_limit {
                        fetched_may_have_capped = true;
                    }
                    rows.extend(batch.iter().cloned());
                    if section.0 < 2 {
                        local_sections_seen[section.0 as usize] = true;
                    }
                    // `search_symbols` is deliberately the fast structural
                    // facet. The engine emits name/type before semantic, so
                    // returning as soon as both local sections arrive avoids
                    // making an exact lookup wait for an embedding model.
                    if local_sections_seen == [true, true] {
                        break;
                    }
                }
                SearchEvent::Latency { .. } => {}
                SearchEvent::Done { .. } => {
                    terminated = true;
                    break;
                }
                SearchEvent::Failed { error, .. } => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated && local_sections_seen != [true, true] {
            return Err(McpError::TruncatedStream);
        }

        // A symbol can hit in more than one search section (name *and* kind),
        // and the sections are ranked independently. Deduplicate on the key —
        // not with `dedup_by`, which only removes *adjacent* duplicates and
        // would leave a pair whose scores happened to differ. The key string
        // is kept alongside each row (rather than recomputed) so the final
        // sort below can use it as a deterministic tiebreak.
        let mut seen = std::collections::HashSet::with_capacity(rows.len());
        let mut deduped: Vec<(String, HitRow)> = Vec::with_capacity(rows.len());
        for h in rows {
            let key_str = format!(
                "{}:{}#{}",
                h.key.package.ecosystem.as_str(),
                h.key.package.name.as_str(),
                h.key.intro.to_hex(),
            );
            if seen.insert(key_str.clone()) {
                deduped.push((key_str, h));
            }
        }

        // The merged list must be re-sorted before it is paged, or the page
        // boundary would cut by section order rather than by relevance. Score
        // first, then the canonical key string: two rows can tie on score
        // (frequent — see `search::compare_candidates`'s own reasoning), and
        // without a total tiebreak here the merge order — which depends on
        // which section happened to emit a tied row first — would leak into
        // page boundaries, making `cursor` unstable across calls for the
        // identical query.
        deduped.sort_by(|(ka, a), (kb, b)| b.score.total_cmp(&a.score).then_with(|| ka.cmp(kb)));
        let rows: Vec<HitRow> = deduped.into_iter().map(|(_, h)| h).collect();

        let (page, has_more) = paginate_rows(rows, offset, limit, fetched_may_have_capped);
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        let hits = self.attach_addresses(page, &HashSet::new()).await;
        Ok(SearchResult {
            hits,
            semantic: None,
            excluded_kinds,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `search`, the merged form of `search_symbols` + `semantic_search`
    /// (docs/MCP-SURFACE-PLAN.md §5.1): one `engine.search()` call, every
    /// section collected rather than stopping once the two structural
    /// sections arrive, each hit labelled with whether semantic ranking
    /// (also) contributed to it. `do_search` and `do_semantic_search` above
    /// are unchanged and remain the direct structural-only / semantic-only
    /// entry points other callers (including `tests/mcp_dump_responses.rs`'s
    /// stable-surface harness) already depend on.
    pub async fn do_unified_search(
        &self,
        args: SearchSymbolsArgs,
    ) -> Result<SearchResult, McpError> {
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;
        let (kinds, exclude_kinds, packages, excluded_kinds) =
            resolve_search_scope(args.kinds.as_ref(), args.packages.as_ref())?;
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);
        let query = SearchQuery {
            text: args.query,
            kinds,
            exclude_kinds,
            packages,
            limit: fetch_limit,
        };
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.search(query, generation);

        let mut rows: Vec<HitRow> = Vec::new();
        let mut semantic_keys: HashSet<String> = HashSet::new();
        let mut fetched_may_have_capped = false;
        let mut terminated = false;
        let mut semantic_status_val: Option<SemanticStatus> = None;

        while let Ok(event) = rx.recv_async().await {
            match event {
                SearchEvent::SectionState { section, state, .. }
                    if section == crate::search::SECTION_SEMANTIC =>
                {
                    semantic_status_val = Some(semantic_status(state));
                }
                SearchEvent::Section {
                    section,
                    rows: batch,
                    ..
                }
                | SearchEvent::Merge {
                    section,
                    rows: batch,
                    ..
                } => {
                    if batch.len() >= fetch_limit {
                        fetched_may_have_capped = true;
                    }
                    if section == crate::search::SECTION_SEMANTIC {
                        semantic_keys.extend(batch.iter().map(hit_key_string));
                    }
                    rows.extend(batch.iter().cloned());
                }
                SearchEvent::Latency { .. } => {}
                SearchEvent::Done { .. } => {
                    terminated = true;
                    break;
                }
                SearchEvent::Failed { error, .. } => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        // Same dedup/tiebreak discipline as `do_search` (§ its own doc
        // comment): a symbol can hit in more than one section, sections are
        // ranked independently, and the merged, re-sorted order must be
        // deterministic so `cursor` stays stable across calls.
        let mut seen = HashSet::with_capacity(rows.len());
        let mut deduped: Vec<(String, HitRow)> = Vec::with_capacity(rows.len());
        for h in rows {
            let key_str = hit_key_string(&h);
            if seen.insert(key_str.clone()) {
                deduped.push((key_str, h));
            }
        }
        deduped.sort_by(|(ka, a), (kb, b)| b.score.total_cmp(&a.score).then_with(|| ka.cmp(kb)));
        let rows: Vec<HitRow> = deduped.into_iter().map(|(_, h)| h).collect();

        let (page, has_more) = paginate_rows(rows, offset, limit, fetched_may_have_capped);
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        let hits = self.attach_addresses(page, &semantic_keys).await;
        Ok(SearchResult {
            hits,
            // `Ready` is the common case; omitting it there is what keeps a
            // plain structural search from paying a token for a status that
            // is always the same value.
            semantic: semantic_status_val.filter(|s| !matches!(s, SemanticStatus::Ready)),
            excluded_kinds,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// Compute an address (§4) for each hit, caching one [`PackageView`] fetch
    /// per distinct package lineage in the page rather than per row — a page
    /// is usually dominated by one or two packages, and `Corpus::package` is
    /// an `Arc` clone behind an `RwLock` read, cheap but not free to repeat
    /// per row. `semantic_keys` marks which hits (by canonical key string)
    /// semantic ranking contributed to; documentation evidence is fetched
    /// only for those, matching what `do_semantic_search` already did.
    async fn attach_addresses(
        &self,
        hits: Vec<HitRow>,
        semantic_keys: &HashSet<String>,
    ) -> Vec<crate::mcp::tools::SearchHitDoc> {
        let corpus = self.engine.corpus();
        let mut cache: PackageCache = std::collections::HashMap::new();
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            let lineage = hit.key.package.clone();
            let pkg = self.cached_package(&corpus, &mut cache, &lineage).await;
            let address = pkg
                .as_deref()
                .and_then(|pkg| {
                    crate::mcp::address::render_address(pkg, &lineage, None, hit.key.intro)
                })
                .map(|a| a.to_string());
            let semantic = semantic_keys.contains(&hit_key_string(&hit));
            let documentation = semantic
                .then(|| {
                    pkg.as_deref()
                        .and_then(|pkg| pkg.view().entry(hit.key.intro))
                        .map(|entry| entry.sym().documentation.clone())
                        .filter(|documentation| !documentation.is_empty())
                })
                .flatten();
            out.push(crate::mcp::tools::SearchHitDoc {
                hit,
                address,
                semantic,
                documentation,
            });
        }
        out
    }

    /// Fetch and cache one [`PackageView`] per distinct lineage across a
    /// batch of rows, so repeated rows from the same package (the common
    /// case for `refs`/search pages) do not each pay a separate
    /// `Corpus::package` lookup.
    async fn cached_package(
        &self,
        corpus: &crate::store::corpus::Corpus,
        cache: &mut PackageCache,
        lineage: &crate::wire::PackageLineageId,
    ) -> Option<std::sync::Arc<crate::store::package::PackageView>> {
        match cache.get(lineage) {
            Some(cached) => cached.clone(),
            None => {
                let fetched = corpus.package(lineage).await;
                cache.insert(lineage.clone(), fetched.clone());
                fetched
            }
        }
    }

    /// Render one declaration's address, reusing an already-open
    /// [`PackageCache`] — the shared building block [`Self::attach_addresses`]
    /// and `do_find_usages`/`do_get_occurrences` (Job 1: address-bearing
    /// reference rows) all use so a batch of rows from the same package pays
    /// for exactly one `Corpus::package` fetch.
    async fn cached_address(
        &self,
        corpus: &crate::store::corpus::Corpus,
        cache: &mut PackageCache,
        key: &crate::wire::SymbolKey,
    ) -> Option<String> {
        let pkg = self.cached_package(corpus, cache, &key.package).await;
        pkg.as_deref()
            .and_then(|pkg| crate::mcp::address::render_address(pkg, &key.package, None, key.intro))
            .map(|a| a.to_string())
    }

    /// Decode a `key`-shaped tool argument that may be either the legacy
    /// `ecosystem:name#introhex` string or an address (§4).
    ///
    /// The legacy form is tried first and, when it matches, decoded exactly
    /// as before — `SymbolKeyDto::to_wire`'s contract does not change for any
    /// caller already passing one. Anything else is parsed as an address and
    /// resolved against the live corpus; only [`ResolveOutcome::Resolved`]
    /// succeeds; every other outcome becomes a structured
    /// [`McpError::AddressUnresolved`] carrying exactly why (ambiguous,
    /// not found, wrong version, …) rather than collapsing to one generic
    /// "invalid key" message.
    async fn resolve_key_or_address(
        &self,
        raw: &SymbolKeyDto,
    ) -> Result<crate::wire::SymbolKey, McpError> {
        if let Ok(key) = raw.to_wire() {
            return Ok(key);
        }
        let corpus = self.engine.corpus();
        let versions = self.engine.versions_registry();
        match crate::mcp::address::resolve_str(&corpus, &versions, &raw.0).await {
            crate::mcp::address::ResolveOutcome::Resolved { key, .. } => Ok(key),
            outcome => Err(McpError::AddressUnresolved { outcome }),
        }
    }

    /// `get_symbol`, minus the MCP wrapping.
    pub async fn do_get_symbol(&self, args: GetSymbolArgs) -> Result<SymbolDoc, McpError> {
        let key = self.resolve_key_or_address(&args.key).await?;
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.open_symbol(key, generation);

        let mut head: Option<Box<SymbolHead>> = None;
        let mut timeline: Option<Timeline> = None;
        let mut sections: Vec<RenderSection> = Vec::new();
        let mut terminated = false;

        while let Ok(event) = rx.recv_async().await {
            match event {
                DocEvent::Head(h) => head = Some(h),
                // §9.3 invariant 5: emitted at most once, after `Head` and
                // before the first `Section` — the version-lineage feature
                // (renames, deprecations, signature/visibility/doc changes,
                // introduction, removal) an agent otherwise has no way to see.
                // This was previously the one event this loop silently dropped
                // in its wildcard arm below: fully computed by the engine on
                // every `open_symbol` call, and discarded before any caller —
                // GUI or MCP — could read it.
                DocEvent::Timeline(t) => timeline = Some(t),
                DocEvent::Section(s) => sections.push(s),
                // Highlight spans are a GUI-only progressive upgrade and carry
                // no information an agent can use; Refs/Impls pages are served
                // by `find_usages` and `graph_query` instead.
                DocEvent::Highlight { .. } | DocEvent::Refs { .. } | DocEvent::Impls { .. } => {}
                DocEvent::Done => {
                    terminated = true;
                    break;
                }
                DocEvent::Failed(error) => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        // Protocol invariant §9.3.1: `Head` precedes everything. A `Done`
        // without one is a broken engine stream, not an empty symbol.
        let head = head.ok_or(McpError::TruncatedStream)?;
        // Protocol invariant §9.3.5: `Timeline` is emitted for every resolved
        // symbol (`doc.rs` substitutes a single-generation slice when the
        // version registry has nothing recorded), so its absence here is the
        // same class of broken-stream signal as a missing `Head`.
        let timeline = timeline.ok_or(McpError::TruncatedStream)?;
        Ok(SymbolDoc {
            head,
            timeline,
            sections,
        })
    }

    /// Read one symbol through the compact MCP projection.
    pub async fn do_get_symbol_compact(
        &self,
        args: GetSymbolArgs,
        format: SymbolFormat,
    ) -> Result<CompactSymbolDoc, McpError> {
        let key = self.resolve_key_or_address(&args.key).await?;
        self.compact_symbol_for_key(key, format).await
    }

    /// The compact projection for an already-resolved [`SymbolKey`].
    ///
    /// Split out of [`Self::do_get_symbol_compact`] so [`Self::do_get_symbols`]
    /// can share this half without paying for [`Self::resolve_key_or_address`]
    /// a second time on a key it just finished resolving in its own pre-flight
    /// pass.
    async fn compact_symbol_for_key(
        &self,
        key: crate::wire::SymbolKey,
        format: SymbolFormat,
    ) -> Result<CompactSymbolDoc, McpError> {
        let corpus = self.engine.corpus();
        let versions = self.engine.versions_registry();
        let failures = self.engine.load_failures();
        let package = crate::doc::resolve_symbol(&corpus, &versions, &failures, &key)
            .await
            .map_err(McpError::Engine)?;
        let entry = package
            .view()
            .entry(key.intro)
            .expect("resolve_symbol guarantees a live entry");
        let head = crate::chunk::head::projection(key.intro, entry, package.view(), &package);
        Ok(compact_symbol(&head, format, &package))
    }

    /// Read several symbols in one MCP round trip.
    ///
    /// Each element of `args.keys` accepts exactly what [`Self::do_get_symbol`]
    /// accepts for its single `key` — the legacy `ecosystem:name#introhex`
    /// string or an address (§4) — via the same [`Self::resolve_key_or_address`].
    /// `SearchResult`'s doc comment promises a caller may "pass either it or
    /// `address` straight to `read` or `refs`"; before this, `read`'s batched
    /// path enforced only the legacy spelling, so an address that worked for a
    /// single-symbol `read` or for `refs` was rejected here with a format
    /// complaint the caller was never shown as a constraint.
    pub async fn do_get_symbols(&self, args: GetSymbolsArgs) -> Result<SymbolsResult, McpError> {
        if args.keys.is_empty() || args.keys.len() > 32 {
            return Err(McpError::InvalidArgument {
                argument: "keys",
                reason: "must contain between 1 and 32 symbol keys".to_owned(),
            });
        }
        // Resolve — and thereby reject a malformed or unresolvable batch —
        // before opening any streams; otherwise a bad late element computes
        // earlier symbols and then discards the entire response. Resolution
        // is the pre-flight step now, not a `to_wire` format check: the two
        // used to be the same operation because the legacy spelling was the
        // only accepted input, but accepting addresses means "well-formed"
        // and "resolves to exactly one declaration" are no longer the same
        // fact, and it is the second one this loop must have settled before
        // any `do_get_symbol_compact` call below can be allowed to run.
        //
        // Build a stable unique work list at the same time: callers may repeat
        // a key, or name the same declaration once as a key and once as an
        // address, but the engine must never repeat its index read for either.
        // Dedup therefore keys on the *resolved* `SymbolKey`, not the raw
        // input string — deduping on the string would let two spellings of one
        // symbol slip past as two distinct work items.
        let mut unique_by_key = BTreeMap::<crate::wire::SymbolKey, usize>::new();
        let mut unique = Vec::new();
        let mut order = Vec::with_capacity(args.keys.len());
        for raw in &args.keys {
            let key = self.resolve_key_or_address(raw).await?;
            let index = match unique_by_key.get(&key) {
                Some(&index) => index,
                None => {
                    let index = unique.len();
                    unique_by_key.insert(key.clone(), index);
                    unique.push(key);
                    index
                }
            };
            order.push(index);
        }
        let unique_symbols = try_join_all(
            unique
                .into_iter()
                .map(|key| self.compact_symbol_for_key(key, args.format)),
        )
        .await?;
        let symbols = order
            .into_iter()
            .map(|index| unique_symbols[index].clone())
            .collect();
        Ok(SymbolsResult { symbols })
    }

    /// `find_usages`, minus the MCP wrapping.
    ///
    /// # Job 1 — accepts an address
    ///
    /// The caller's `key` is resolved at *this* MCP layer, not passed
    /// raw into the Trustfall binding: [`Self::resolve_key_or_address`]
    /// accepts either the legacy `ecosystem:name#introhex` string or an
    /// address (§4), and only the canonical resolved key string ever reaches
    /// [`crate::graph::queries::FIND_USAGES`]. A non-`Resolved` outcome
    /// surfaces as [`McpError::AddressUnresolved`] carrying the candidates or
    /// refine hint — never a silent empty result.
    pub async fn do_find_usages(&self, args: FindUsagesArgs) -> Result<UsagesResult, McpError> {
        let key = self.resolve_key_or_address(&args.key).await?;
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let mut bindings = BTreeMap::new();
        bindings.insert("key".to_owned(), SymbolKeyDto::from_wire(&key).0);

        let (page, columns, has_more) = self
            .run_query_page(
                GraphQuery {
                    query: crate::graph::queries::FIND_USAGES.to_owned(),
                    args: bindings,
                },
                offset,
                limit,
            )
            .await?;

        let corpus = self.engine.corpus();
        let mut cache: PackageCache = std::collections::HashMap::new();
        let mut usages = Vec::with_capacity(page.len());
        for row in &page {
            let key_dto = SymbolKeyDto(column(&columns, row, "key").unwrap_or_default());
            let address = match key_dto.to_wire() {
                Ok(wire_key) => self.cached_address(&corpus, &mut cache, &wire_key).await,
                Err(_) => None,
            };
            usages.push(UsageRow {
                key: key_dto,
                address,
                name: column(&columns, row, "name").unwrap_or_default(),
                kind: column(&columns, row, "kind").unwrap_or_default(),
                signature: column(&columns, row, "signature").unwrap_or_default(),
                path: column(&columns, row, "path").unwrap_or_default(),
            });
        }

        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(UsagesResult {
            usages,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `get_occurrences`, minus the MCP wrapping.
    ///
    /// # Job 1 — accepts an address
    ///
    /// See [`Self::do_find_usages`]'s docs — the same resolve-then-bind
    /// discipline applies here, and every owner/target row also carries an
    /// `address` so a caller can chain without a second lookup.
    pub async fn do_get_occurrences(
        &self,
        args: GetOccurrencesArgs,
    ) -> Result<GetOccurrencesResult, McpError> {
        let key = self.resolve_key_or_address(&args.key).await?;
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;
        let mut bindings = BTreeMap::new();
        bindings.insert("key".to_owned(), SymbolKeyDto::from_wire(&key).0);

        let (page, columns, has_more) = self
            .run_query_page(
                GraphQuery {
                    query: GET_OCCURRENCES_QUERY.to_owned(),
                    args: bindings,
                },
                offset,
                limit,
            )
            .await?;

        let corpus = self.engine.corpus();
        let mut cache: PackageCache = std::collections::HashMap::new();
        let mut occurrences = Vec::with_capacity(page.len());
        for row in &page {
            let owner_key = SymbolKeyDto(column(&columns, row, "ownerKey").unwrap_or_default());
            let owner_address = match owner_key.to_wire() {
                Ok(wire_key) => self.cached_address(&corpus, &mut cache, &wire_key).await,
                Err(_) => None,
            };
            let owner = OccurrenceSymbol {
                key: owner_key,
                address: owner_address,
                signature: column(&columns, row, "ownerSignature").unwrap_or_default(),
                path: column(&columns, row, "ownerPath"),
            };
            let target = match column(&columns, row, "targetResolvedKey")
                .filter(|key| !key.is_empty())
            {
                Some(raw) => {
                    let target_key = SymbolKeyDto(raw);
                    let target_address = match target_key.to_wire() {
                        Ok(wire_key) => self.cached_address(&corpus, &mut cache, &wire_key).await,
                        Err(_) => None,
                    };
                    Some(OccurrenceSymbol {
                        key: target_key,
                        address: target_address,
                        signature: column(&columns, row, "targetSignature").unwrap_or_default(),
                        path: column(&columns, row, "targetPath"),
                    })
                }
                None => None,
            };
            occurrences.push(OccurrenceRow {
                target_key: column(&columns, row, "targetKey").unwrap_or_default(),
                target,
                owner,
                reference_kind: column(&columns, row, "referenceKind").unwrap_or_default(),
                confidence: column(&columns, row, "confidence").unwrap_or_default(),
                span_start: parse_occurrence_offset(
                    &column(&columns, row, "spanStart"),
                    "spanStart",
                )?,
                span_end: parse_occurrence_offset(&column(&columns, row, "spanEnd"), "spanEnd")?,
            });
        }
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(GetOccurrencesResult {
            // Canonical form, even when the caller's `args.key` was an
            // address rather than a legacy key string — the response echoes
            // back what was actually resolved.
            owner: SymbolKeyDto::from_wire(&key),
            occurrences,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `refs` (docs/MCP-SURFACE-PLAN.md §5.1): merges `find_usages` +
    /// `get_occurrences` behind a `direction` argument. Both directions
    /// inherit Job 1's address acceptance from the methods above.
    pub async fn do_refs(&self, args: RefsArgs) -> Result<RefsResult, McpError> {
        // Resolved once, ahead of the direction split, so both branches
        // report on the same package the query actually ran against — and
        // so an address argument (as opposed to the already-canonical
        // `ecosystem:name#introhex` form) is only ever parsed once.
        let resolved = self.resolve_key_or_address(&args.key).await?;
        // `Recorded` is the common case; collapsing it to `None` here is what
        // keeps an ordinarily-answered `refs` call from paying a token for a
        // status that is always the same value — the same collapse
        // `do_unified_search` applies to `SemanticStatus::Ready` above.
        let coverage = Some(self.reference_coverage(&resolved).await)
            .filter(|c| !matches!(c, ReferenceCoverage::Recorded));
        match args.direction {
            RefsDirection::In => {
                let result = self
                    .do_find_usages(FindUsagesArgs {
                        key: args.key,
                        limit: args.limit,
                        cursor: args.cursor,
                    })
                    .await?;
                Ok(RefsResult::In {
                    usages: result.usages,
                    truncated: result.truncated,
                    next_cursor: result.next_cursor,
                    coverage,
                })
            }
            RefsDirection::Out => {
                let result = self
                    .do_get_occurrences(GetOccurrencesArgs {
                        key: args.key,
                        limit: args.limit,
                        cursor: args.cursor,
                    })
                    .await?;
                Ok(RefsResult::Out {
                    owner: result.owner,
                    occurrences: result.occurrences,
                    truncated: result.truncated,
                    next_cursor: result.next_cursor,
                    coverage,
                })
            }
        }
    }

    /// Whether `key`'s package ever recorded a reference graph — the fact
    /// `do_refs` attaches as `RefsResult`'s `coverage` field so an empty page
    /// cannot be misread as "no references" when the true reason is "never
    /// indexed" (see [`ReferenceCoverage`]'s doc comment).
    ///
    /// Deliberately reads a fact the package itself carries
    /// (`PackageIndexes::occurrences_recorded`, set once at load time from
    /// whether the producer ever called `record_occurrence`) rather than
    /// matching on the package's `ProducerLanguage` here. A language match in
    /// this layer would be a second source of truth for exactly the fact
    /// `occurrences_recorded` exists to hold, and it would silently go stale
    /// the day a producer starts recording — this reads true again the same
    /// day, with no second edit required.
    ///
    /// Returns `NotRecorded` when `key`'s package cannot be found through
    /// `corpus.entry` at all — this should not happen on this path, since
    /// `key` was just resolved against the same corpus, but `is_some_and`
    /// defaults a `None` to `false` (not recorded) rather than `true`. That
    /// keeps this helper's failure mode aligned with
    /// `PackageIndexes::occurrences_recorded`'s own documented direction:
    /// under-claim into a coverage note nobody strictly needed, never
    /// over-claim into a silently untrustworthy empty page.
    async fn reference_coverage(&self, key: &crate::wire::SymbolKey) -> ReferenceCoverage {
        let corpus = self.engine.corpus();
        let recorded = corpus
            .entry(key)
            .await
            .is_some_and(|entry| entry.package.indexes().occurrences_recorded);
        if recorded {
            ReferenceCoverage::Recorded
        } else {
            ReferenceCoverage::NotRecorded {
                language: key.package.ecosystem.as_str().to_owned(),
            }
        }
    }

    /// `read` (docs/MCP-SURFACE-PLAN.md §5.1): merges `get_symbol` +
    /// `get_symbols` — always plural, and accepts a bare string or an array
    /// for `keys` (see [`KeysArg`]).
    pub async fn do_read(&self, args: ReadArgs) -> Result<SymbolsResult, McpError> {
        self.do_get_symbols(GetSymbolsArgs {
            keys: args.keys.0,
            format: args.format,
        })
        .await
    }

    /// `packages` (docs/MCP-SURFACE-PLAN.md §5.1): merges `list_packages` +
    /// `list_versions` — one question, "what is loaded, at what versions".
    pub async fn do_packages(&self, args: PackagesArgs) -> Result<LoadedPackagesResult, McpError> {
        match args.package {
            Some(requested) => {
                let versions = self
                    .do_list_versions(ListVersionsArgs { package: requested })
                    .await?;
                let lineage = versions.package.to_wire()?;
                Ok(LoadedPackagesResult {
                    packages: vec![PackageWithVersions {
                        lineage: versions.package.0,
                        name: lineage.name.as_str().to_owned(),
                        ecosystem: lineage.ecosystem.as_str().to_owned(),
                        versions: versions.versions,
                    }],
                })
            }
            None => {
                let all = self.do_list_packages().await?;
                let mut packages = Vec::with_capacity(all.packages.len());
                for summary in all.packages {
                    let versions = self
                        .do_list_versions(ListVersionsArgs {
                            package: PackageLineageDto(summary.lineage.clone()),
                        })
                        .await?;
                    packages.push(PackageWithVersions {
                        lineage: summary.lineage,
                        name: summary.name,
                        ecosystem: summary.ecosystem,
                        versions: versions.versions,
                    });
                }
                Ok(LoadedPackagesResult { packages })
            }
        }
    }

    /// `semantic_search`, minus the MCP wrapping.
    pub async fn do_semantic_search(
        &self,
        args: SemanticSearchArgs,
    ) -> Result<SemanticSearchResult, McpError> {
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;
        let (kinds, packages) = parse_search_filters(args.kinds.as_ref(), args.packages.as_ref())?;
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);
        let query = SearchQuery {
            text: args.query.clone(),
            kinds,
            // `semantic_search` ranks by documentation similarity, not by name,
            // so the member-noise problem the default scope exists to solve
            // does not arise here: a parameter with no documentation of its own
            // is not a nearest neighbour of anything. Excluding kinds by
            // default would only hide real matches.
            exclude_kinds: Vec::new(),
            packages,
            limit: fetch_limit,
        };
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.search(query, generation);
        let mut rows = Vec::new();
        let mut status = None;
        let mut fetched_may_have_capped = false;
        let mut terminated = false;

        while let Ok(event) = rx.recv_async().await {
            match event {
                SearchEvent::SectionState { section, state, .. }
                    if section == crate::search::SECTION_SEMANTIC =>
                {
                    status = Some(semantic_status(state));
                }
                SearchEvent::Section {
                    section,
                    rows: batch,
                    ..
                }
                | SearchEvent::Merge {
                    section,
                    rows: batch,
                    ..
                } if section == crate::search::SECTION_SEMANTIC => {
                    if batch.len() >= fetch_limit {
                        fetched_may_have_capped = true;
                    }
                    rows.extend(batch.iter().cloned());
                }
                SearchEvent::Done { .. } => {
                    terminated = true;
                    break;
                }
                SearchEvent::Failed { error, .. } => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        let mut seen = HashSet::with_capacity(rows.len());
        rows.retain(|row| {
            let key = format!(
                "{}:{}#{}",
                row.key.package.ecosystem.as_str(),
                row.key.package.name.as_str(),
                row.key.intro.to_hex()
            );
            seen.insert(key)
        });
        let (page, has_more) = paginate_rows(rows, offset, limit, fetched_may_have_capped);
        let corpus = self.engine.corpus();
        let mut hits = Vec::with_capacity(page.len());
        for hit in page {
            let documentation = corpus
                .package(&hit.key.package)
                .await
                .and_then(|package| {
                    package
                        .view()
                        .entry(hit.key.intro)
                        .map(|entry| entry.sym().documentation.clone())
                })
                .filter(|documentation| !documentation.is_empty());
            hits.push(SemanticHitRow {
                key: SymbolKeyDto::from_wire(&hit.key),
                display_name: hit.display_name.to_string(),
                signature: hit.sig_preview.iter().map(signature_text).collect(),
                kind: hit.kind,
                score: hit.score,
                documentation,
            });
        }
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(SemanticSearchResult {
            query: args.query,
            // Not one of `Unavailable`'s three labels, because it is not one
            // of its states: the section is required to emit a status and did
            // not. Saying so — rather than borrowing the nearest real reason —
            // keeps a protocol gap from being read as a configuration problem
            // the reader could go and fix.
            status: status.unwrap_or_else(|| SemanticStatus::Unavailable {
                reason: "semantic section emitted no status".to_owned(),
                remedy: "this is an engine defect, not a configuration \
                         problem — the search stream ended without the \
                         semantic section reporting its state; check the log"
                    .to_owned(),
            }),
            hits,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `diff_versions`, minus the MCP wrapping.
    ///
    /// # Why this is not a `graph_query`
    ///
    /// `graph_query` resolves against the corpus, which holds exactly one
    /// resident generation per lineage. No query it can express sees two. The
    /// engine's version registry holds them both already, so this is a direct
    /// engine call rather than a query — see `crate::diff` for the two
    /// designs that were rejected.
    ///
    /// # Pagination pages the *rows*, not the counts
    ///
    /// `unchanged`, `from_symbol_count` and `to_symbol_count` describe the
    /// whole diff on every page. Only `rows` is sliced. A caller that paged to
    /// the end and summed the pages would otherwise get a different total from
    /// a caller that read page one, and both would be right.
    pub async fn do_diff_versions(
        &self,
        args: DiffVersionsArgs,
    ) -> Result<DiffVersionsResult, McpError> {
        let lineage = args.package.to_wire()?;
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let Some(full) = self
            .engine
            .diff_versions(&lineage, &args.from_version, &args.to_version)
        else {
            let loaded = self
                .engine
                .versions(&lineage)
                .versions
                .iter()
                .map(|v| v.version.to_string())
                .collect();
            return Ok(DiffVersionsResult::NotLoaded {
                package: PackageLineageDto::from_wire(&lineage),
                from_version: args.from_version,
                to_version: args.to_version,
                loaded,
            });
        };

        let total = full.rows.len();
        let page: Vec<_> = full.rows.iter().skip(offset).take(limit).cloned().collect();
        let has_more = offset.saturating_add(page.len()) < total;
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));

        Ok(DiffVersionsResult::Diff {
            diff: Box::new(PackageDiff {
                rows: page.into(),
                ..full
            }),
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `list_packages`, minus the MCP wrapping.
    pub async fn do_list_packages(&self) -> Result<PackagesResult, McpError> {
        let result = self
            .run_query(
                GraphQuery {
                    query: PACKAGES_QUERY.to_owned(),
                    args: BTreeMap::new(),
                },
                MAX_LIMIT,
            )
            .await?;

        let packages = result
            .rows
            .iter()
            .map(|row| PackageSummary {
                lineage: column(&result.columns, row, "lineage").unwrap_or_default(),
                name: column(&result.columns, row, "name").unwrap_or_default(),
                ecosystem: column(&result.columns, row, "ecosystem").unwrap_or_default(),
            })
            .collect();

        Ok(PackagesResult { packages })
    }

    /// `index_package`, minus the MCP wrapping.
    ///
    /// # Why this returns before the work does
    ///
    /// Every other tool in this file answers from data that is already
    /// resident, so "drain the stream to a terminal event" is bounded by
    /// milliseconds. This one runs a network fetch and a language producer —
    /// seconds at best. A tool that blocked for the whole of it would stall the
    /// agent's turn and hand every HTTP proxy on the path a vote on how long an
    /// index is allowed to take.
    ///
    /// So the wait is bounded by an argument the *caller* chooses, and a job
    /// still running when it expires is reported as
    /// [`IndexPackageResult::Running`] with its current stage — a true answer,
    /// not a timeout error. Calling again joins the same job (see
    /// [`crate::mcp::index`]), which is what makes polling safe rather than a way to
    /// start N producer runs.
    pub async fn do_index_package(
        &self,
        args: IndexPackageArgs,
    ) -> Result<IndexPackageResult, McpError> {
        use crate::mcp::index::JobProgress;

        let purl = crate::Purl::parse(&args.purl).map_err(|reason| McpError::Index {
            // Parse failures are `Error::MalformedPurl`, so an agent sees
            // one taxonomy for every way indexing can fail rather than a
            // parse-shaped error here and an index-shaped one everywhere else.
            error: Box::new(crate::packages::acquire::Error::from(reason)),
        })?;
        let rendered = purl.render();

        let wait = std::time::Duration::from_secs(
            args.wait_seconds
                .unwrap_or(DEFAULT_INDEX_WAIT_SECONDS)
                .min(MAX_INDEX_WAIT_SECONDS)
                .into(),
        );

        let (progress, joined) = self
            .jobs
            .run(
                &self.engine,
                crate::mcp::index::IndexTarget::Registry(purl),
                self.next_gen(),
                wait,
            )
            .await;

        match progress {
            JobProgress::Indexed(done) => Ok(IndexPackageResult::Indexed {
                purl: done.purl,
                package: format!("{}:{}", done.ecosystem, done.name),
                version: done.version,
                symbol_count: done.symbol_count,
                root_key: done.root.map(|k| SymbolKeyDto::from_wire(&k).0),
                integrity: report_integrity(&done.integrity),
            }),
            JobProgress::Failed(error) => Err(McpError::Index {
                error: Box::new(error),
            }),
            JobProgress::Running {
                stage,
                received,
                total,
            } => Ok(IndexPackageResult::Running {
                stage: stage.to_string(),
                // Zero received bytes before the download starts is absence,
                // not a measurement — reporting `0` would read as "the download
                // has produced nothing", which is a different claim.
                received_bytes: (received > 0).then_some(received),
                total_bytes: total,
                elapsed_seconds: self
                    .jobs
                    .elapsed(&rendered)
                    .map(|d| d.as_secs())
                    .unwrap_or_default(),
                joined_existing_job: joined,
                purl: rendered,
            }),
        }
    }

    /// `index`: several targets, local roots, and opt-in dependency following.
    ///
    /// # Why the deadline covers the batch rather than each target
    ///
    /// Per-target deadlines multiply: five targets at the default 120 s could
    /// keep the caller waiting ten minutes for a call it asked to bound at
    /// two. The jobs are independent and already run to completion whether or
    /// not anyone is waiting (see [`crate::mcp::index`]), so a batch deadline
    /// costs nothing but a second call for the stragglers — which joins the
    /// running jobs rather than restarting them.
    ///
    /// # Why dependency following is breadth-first
    ///
    /// A depth-first walk would spend the whole deadline on one chain and
    /// return with the caller's *direct* dependencies still unstarted, which
    /// is the opposite of what one hop was asked for. Level by level, the
    /// nearest edges are always the ones that land first.
    pub async fn do_index(&self, args: IndexArgs) -> Result<IndexResult, McpError> {
        use crate::mcp::index::JobProgress;
        use std::collections::BTreeSet;

        if args.targets.is_empty() {
            return Err(McpError::InvalidArgument {
                argument: "targets",
                reason: "must name at least one package URL or package root path".to_owned(),
            });
        }

        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(
                args.wait_seconds
                    .unwrap_or(DEFAULT_INDEX_WAIT_SECONDS)
                    .min(MAX_INDEX_WAIT_SECONDS)
                    .into(),
            );
        let hops = if args.dependencies {
            args.depth.unwrap_or(1).min(MAX_DEPENDENCY_DEPTH)
        } else {
            0
        };

        let mut result = IndexResult {
            indexed: Vec::new(),
            running: Vec::new(),
            failed: Vec::new(),
            not_scanned: Vec::new(),
        };
        // Keyed on the resolved target, not the caller's spelling: a package
        // reachable as a dependency of two others must be indexed once.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        // One frontier per hop. `requested` rides along because it is a fact
        // about how a package was *reached*, and it is lost the moment the
        // frontier is flattened.
        let mut frontier: Vec<(String, bool)> =
            args.targets.iter().map(|t| (t.clone(), true)).collect();

        for hop in 0..=hops {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<(String, bool)> = Vec::new();

            for (spelling, requested) in std::mem::take(&mut frontier) {
                let target = match self.parse_target(&spelling) {
                    Ok(target) => target,
                    Err(error) => {
                        result.failed.push(FailedTarget {
                            target: spelling,
                            error,
                            requested,
                        });
                        continue;
                    }
                };
                if !seen.insert(target.key()) {
                    continue;
                }

                // A deadline already past still gives every remaining target a
                // job — started, joined on the next call — rather than
                // silently dropping it. `Duration::ZERO` returns the job's
                // current state immediately, which for a job just started is
                // `Running`.
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let (progress, joined) = self
                    .jobs
                    .run(&self.engine, target.clone(), self.next_gen(), remaining)
                    .await;

                // A local package's manifest is readable whether or not its
                // producer has finished — the file is on disk either way. So
                // the dependency scan is keyed on the target having *started*
                // rather than having *landed*: a caller who asked for
                // dependencies and hit the deadline still gets the edges
                // enqueued, instead of a result that silently followed none.
                // A failed target is excluded, because the most common failure
                // is a root that is not there to read.
                let scan = hop < hops && !matches!(progress, JobProgress::Failed(_));

                match progress {
                    JobProgress::Indexed(done) => result.indexed.push(IndexedPackage {
                        target: spelling,
                        package: format!("{}:{}", done.ecosystem, done.name),
                        version: done.version,
                        symbol_count: done.symbol_count,
                        root_key: done.root.map(|k| SymbolKeyDto::from_wire(&k).0),
                        integrity: report_integrity(&done.integrity),
                        requested,
                    }),
                    JobProgress::Running { stage, .. } => result.running.push(RunningTarget {
                        target: spelling,
                        stage: stage.to_string(),
                        joined_existing_job: joined,
                        requested,
                    }),
                    JobProgress::Failed(error) => result.failed.push(FailedTarget {
                        target: spelling,
                        error: error.to_string(),
                        requested,
                    }),
                }

                if scan {
                    self.collect_dependencies(&target, &mut next, &mut result);
                }
            }

            frontier = next;
        }

        Ok(result)
    }

    /// Decide whether a target string names a registry package or a directory.
    ///
    /// The two are unambiguous by construction — a package URL always begins
    /// `pkg:`, and no filesystem path does — so this never has to guess. That
    /// is why the tool takes one `targets` list rather than two arguments the
    /// caller has to sort their input into.
    fn parse_target(&self, spelling: &str) -> Result<crate::mcp::index::IndexTarget, String> {
        use crate::mcp::index::IndexTarget;

        if spelling.starts_with("pkg:") {
            return crate::Purl::parse(spelling)
                .map(IndexTarget::Registry)
                .map_err(|reason| crate::packages::acquire::Error::from(reason).to_string());
        }

        let root = std::path::PathBuf::from(spelling);
        if !root.is_dir() {
            return Err(format!(
                "{spelling} is neither a package URL (those begin \"pkg:\") \
                 nor a readable directory"
            ));
        }
        let Some(language) = crate::packages::local::language_at(&root) else {
            return Err(format!(
                "{spelling} holds no manifest any producer recognises \
                 (Cargo.toml, package.json, go.mod, pom.xml, build.gradle, \
                 pyproject.toml, *.csproj, CMakeLists.txt, compile_commands.json)"
            ));
        };
        let name = crate::packages::local::name_at(&root, language);
        Ok(IndexTarget::Local {
            root,
            language,
            name,
        })
    }

    /// Read `target`'s declared dependencies onto the next frontier.
    ///
    /// Only *local* dependencies are followed. A registry dependency carries a
    /// requirement (`"^1.2"`), not a version, and turning one into an
    /// indexable PURL means asking a registry to solve it — a network call per
    /// edge, with a version-solving policy this layer has no business
    /// choosing. Those are reported on `not_scanned` so the caller can name
    /// the ones it wants explicitly, which is both cheaper and correct.
    fn collect_dependencies(
        &self,
        target: &crate::mcp::index::IndexTarget,
        next: &mut Vec<(String, bool)>,
        result: &mut IndexResult,
    ) {
        use crate::mcp::index::IndexTarget;
        use crate::packages::local::{Dependency, DependencyScan, declared_dependencies};

        let IndexTarget::Local { root, language, .. } = target else {
            // A fetched package's sources are in the cache and its manifest is
            // readable, but every edge in it is a registry requirement — see
            // this function's docs. Saying so beats scanning to find nothing.
            result.not_scanned.push(UnscannedDependencies {
                target: target.key(),
                reason: "dependencies of a registry package are version \
                         requirements, not coordinates; name them explicitly"
                    .to_owned(),
            });
            return;
        };

        match declared_dependencies(root, *language) {
            DependencyScan::Read(found) => {
                let mut skipped = Vec::new();
                for dependency in found {
                    match dependency {
                        Dependency::Local { root, .. } => {
                            next.push((root.display().to_string(), false));
                        }
                        Dependency::Registry { name, requirement } => {
                            skipped.push(format!("{name} {requirement}"));
                        }
                    }
                }
                if !skipped.is_empty() {
                    result.not_scanned.push(UnscannedDependencies {
                        target: target.key(),
                        reason: format!(
                            "{} registry dependenc{} declared as version \
                             requirements rather than resolved versions, so \
                             they were not indexed: {}",
                            skipped.len(),
                            if skipped.len() == 1 { "y" } else { "ies" },
                            skipped.join(", "),
                        ),
                    });
                }
            }
            DependencyScan::Unsupported { language, manifest } => {
                result.not_scanned.push(UnscannedDependencies {
                    target: target.key(),
                    reason: format!(
                        "no reader for {language:?}'s {manifest} yet — this is \
                         \"nobody looked\", not \"no dependencies\""
                    ),
                });
            }
            DependencyScan::Malformed { manifest, reason } => {
                result.not_scanned.push(UnscannedDependencies {
                    target: target.key(),
                    reason: format!("{} could not be parsed: {reason}", manifest.display()),
                });
            }
            DependencyScan::NoManifest { root } => {
                result.not_scanned.push(UnscannedDependencies {
                    target: target.key(),
                    reason: format!("{} holds no manifest to read", root.display()),
                });
            }
        }
    }

    /// `list_versions`, minus the MCP wrapping.
    ///
    /// [`EngineHandle::versions`] is itself synchronous (the version registry
    /// is fully resident in memory, see `crate::mcp::versions`'s module docs) so
    /// there is no stream to drain here, unlike every other tool in this file.
    pub async fn do_list_versions(
        &self,
        args: ListVersionsArgs,
    ) -> Result<ListVersionsResult, McpError> {
        let lineage = args.package.to_wire()?;
        let list = self.engine.versions(&lineage);

        let versions = list
            .versions
            .iter()
            .map(|v| VersionSummary {
                version: v.version.to_string(),
                is_current: v.is_current,
                symbol_count: v.symbol_count,
            })
            .collect();

        Ok(ListVersionsResult {
            package: PackageLineageDto::from_wire(&list.package),
            versions,
        })
    }

    /// `select_version`, minus the MCP wrapping.
    ///
    /// Drains [`EngineHandle::select_version`]'s single-event receiver to
    /// completion before returning, so — unlike a GUI frame, which polls
    /// `versions()` separately from issuing the switch — this call's own
    /// response is only produced once the corpus has actually been repointed.
    /// See [`SelectVersionResult`]'s docs for what that guarantee does and
    /// does not cover.
    pub async fn do_select_version(
        &self,
        args: SelectVersionArgs,
    ) -> Result<SelectVersionResult, McpError> {
        let lineage = args.package.to_wire()?;
        let generation = self.next_gen();
        let rx = self
            .engine
            .select_version(lineage, args.version.clone(), generation);

        match rx.recv_async().await {
            Ok(VersionEvent::Switched {
                package, version, ..
            }) => {
                // The registry was updated synchronously as part of producing
                // this event (see `VersionRegistry::set_current`), so this read
                // is guaranteed to see the generation we just switched to.
                let symbol_count = self
                    .engine
                    .versions(&package)
                    .versions
                    .iter()
                    .find(|v| v.version == version)
                    .map(|v| v.symbol_count)
                    .unwrap_or(0);
                Ok(SelectVersionResult::Switched {
                    package: PackageLineageDto::from_wire(&package),
                    version: version.to_string(),
                    symbol_count,
                })
            }
            Ok(VersionEvent::NotLoaded {
                package, version, ..
            }) => Ok(SelectVersionResult::NotLoaded {
                package: PackageLineageDto::from_wire(&package),
                version: version.to_string(),
            }),
            // `VersionEvent` is `#[non_exhaustive]` precisely so a future
            // variant does not fail to compile here; only `Switched` and
            // `NotLoaded` exist today. A third variant needs a real
            // translation added above — until then this is reported rather
            // than silently mapped into either existing outcome.
            Ok(other) => {
                tracing::warn!(
                    ?other,
                    "select_version: engine emitted a VersionEvent variant this tool set does not \
                     yet translate"
                );
                Err(McpError::TruncatedStream)
            }
            // Capacity-1 channel, exactly one event ever sent (§the engine's
            // own docs on `select_version`); an empty receiver here means the
            // spawned task never got to send, which is the same broken-stream
            // signal every other drain loop in this file treats as such.
            Err(_) => Err(McpError::TruncatedStream),
        }
    }

    /// `graph_query`, minus the MCP wrapping.
    pub async fn do_graph_query(&self, args: GraphQueryArgs) -> Result<QueryResult, McpError> {
        if args.query.trim().is_empty() {
            return Err(McpError::InvalidArgument {
                argument: "query",
                reason: "must not be empty; call graph_schema for the queryable types".to_owned(),
            });
        }
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;
        // Borrowed before `args.query` moves into `GraphQuery` below — the
        // coverage check below needs the query text, not just its result.
        let query_text = args.query.clone();

        let (page, columns, has_more) = self
            .run_query_page(
                GraphQuery {
                    query: args.query,
                    args: args.args.unwrap_or_default(),
                },
                offset,
                limit,
            )
            .await?;

        // Only a genuinely empty page needs checking — a page that came back
        // with rows already answered, whatever edge it traversed.
        let edge_coverage = if page.is_empty() {
            self.edge_coverage_note(&query_text).await
        } else {
            None
        };

        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(QueryResult {
            columns,
            rows: page,
            truncated: next_cursor.is_some(),
            next_cursor,
            edge_coverage,
        })
    }

    /// Whether an empty `graph_query` page is empty because the query
    /// traversed an edge that is **empty by construction** for every package
    /// currently loaded — see [`EdgeCoverageNote`]'s doc comment for the
    /// incident this closes.
    ///
    /// # Why a substring check on the query text, not a parsed query plan
    ///
    /// The query text is Trustfall (a GraphQL subset). Knowing *for certain*
    /// which edges a query traverses means parsing it and walking the
    /// selection set — this crate already does that once, inside
    /// `trustfall_core::frontend::parse`, and duplicating that walk here to
    /// answer a narrower question is out of scope for what this closes. A
    /// substring check on the two edge names this pair is about is a
    /// conservative stand-in: it can only over-fire (an edge name appearing
    /// in, say, a string literal `@filter` value that happens to spell
    /// `"implementors"`), never under-fire on an actual traversal, and an
    /// over-fire here only *adds* a note to a result that was already empty —
    /// it never removes information a caller would otherwise have had. If a
    /// third edge pair with this problem ever shows up, extend
    /// [`DEAD_EDGE_PAIRS`] rather than reaching for a real query-plan walk;
    /// the day this substring check produces a wrong note in practice is the
    /// day it's worth replacing.
    ///
    /// # Why "packages currently loaded", not "packages this query touched"
    ///
    /// `graph_query` (unlike `refs`, which resolves one key's own package)
    /// has no package argument to read scope from — a query runs over the
    /// whole resident corpus unless its own `@filter` narrows it, and parsing
    /// that filter is the same out-of-scope problem as above. Every package
    /// in the corpus is therefore treated as "in scope": if literally none of
    /// them ever records the position the dead edge reads, the edge is empty
    /// by construction regardless of which packages this particular query
    /// happened to touch, and the note fires correctly. The only imprecision
    /// this can introduce is firing when a *typed but package-filtered* query
    /// would have been correctly empty for the excluded package's language
    /// alone — again a case where the note is added noise, not a wrong claim.
    async fn edge_coverage_note(&self, query_text: &str) -> Option<EdgeCoverageNote> {
        let corpus = self.engine.corpus();
        let packages = corpus.packages().await;
        for (edge, position, alternative) in DEAD_EDGE_PAIRS {
            if !query_text.contains(edge) {
                continue;
            }
            let any_package_records = packages
                .iter()
                .any(|pkg| pkg.indexes().records_type_position(*position));
            if !any_package_records {
                return Some(EdgeCoverageNote {
                    edge: (*edge).to_owned(),
                    answers_instead: (*alternative).to_owned(),
                    reason: format!(
                        "no package currently loaded records a `{edge}` relationship for any \
                         symbol — it is empty by construction for these languages, not \
                         because this particular question has no answer; `{alternative}` is \
                         the edge that carries it here"
                    ),
                });
            }
        }
        None
    }

    /// Drive one Trustfall query through the engine and collect up to `limit`
    /// rows.
    ///
    /// Shared by `find_usages`, `list_packages`, `graph_query` (via
    /// [`Self::run_query_page`]) so all agree on column handling and
    /// truncation semantics. Returns the internal [`RawQueryResult`] rather
    /// than the public [`QueryResult`] because `list_packages` has no cursor
    /// and the two paginated callers need one more decision (`paginate_rows`)
    /// made on top of this before they know their own `next_cursor`.
    async fn run_query(&self, q: GraphQuery, limit: usize) -> Result<RawQueryResult, McpError> {
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.query(q, generation);

        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<QueryResultRow> = Vec::new();
        let mut truncated = false;
        let mut terminated = false;

        while let Ok(event) = rx.recv_async().await {
            match event {
                QueryEvent::Columns { columns: cols, .. } => {
                    columns = cols.iter().map(|c| c.to_string()).collect();
                }
                QueryEvent::Rows { rows: batch, .. } => {
                    for row in batch.iter() {
                        if rows.len() >= limit {
                            truncated = true;
                            break;
                        }
                        rows.push(QueryResultRow {
                            cells: row.cells.iter().map(|c| c.to_string()).collect(),
                        });
                    }
                }
                QueryEvent::Done { .. } => {
                    terminated = true;
                    break;
                }
                QueryEvent::Failed { error, .. } => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        Ok(RawQueryResult {
            columns,
            rows,
            truncated,
        })
    }

    /// Run `q`, fetching enough rows to cover one page at `offset`/`limit`,
    /// and slice that page out locally.
    ///
    /// Returns `(page_rows, columns, has_more)`. See [`paginate_rows`] for
    /// what `has_more` means when the fetch itself may have been capped
    /// before the true end of the result set.
    async fn run_query_page(
        &self,
        q: GraphQuery,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<QueryResultRow>, Vec<String>, bool), McpError> {
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);
        let raw = self.run_query(q, fetch_limit).await?;
        let (page, has_more) = paginate_rows(raw.rows, offset, limit, raw.truncated);
        Ok((page, raw.columns, has_more))
    }
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Ceiling on how far a cursor may page into a result set.
///
/// Every paginated tool here answers a page by re-running its underlying
/// query with a larger internal fetch depth and slicing the page out locally
/// (see [`encode_cursor`]), so the cost of serving a page grows with its
/// offset. This bounds that cost: an agent that wants row 50,000 of "every
/// deprecated symbol in tokio" should narrow the query (a `kinds`/`packages`
/// filter, a more specific Trustfall query), not page 100 requests deep to
/// reach it.
const MAX_OFFSET: usize = 20_000;

/// Encode a page offset as an opaque pagination cursor.
///
/// # Why an offset, not a real streaming cursor
///
/// None of the three paginated tools keep state alive across calls — each
/// call independently re-runs its underlying query and re-sorts the full
/// result under a **total**, deterministic order (search:
/// `search::compare_candidates`'s score/name/key order, reproduced in
/// `NudoxTools::do_search`; `graph_query`/`find_usages`: Trustfall's own row
/// order for a fixed query against a fixed corpus). Given that determinism, a
/// numeric offset into the re-sorted result is exactly as stable a location
/// as a real cursor would be, without needing to keep a result set alive
/// between calls. It is still returned as an opaque string rather than a bare
/// integer in the schema — a caller must pass back exactly what it received,
/// never construct or parse one itself — so a future revision could replace
/// the encoding with a real streaming cursor without a breaking schema
/// change. If you ever find yourself tempted to accept a bare offset `u32`
/// argument instead of this, don't: that would commit the schema to the
/// current encoding being permanent, which is exactly what "opaque" is here
/// to avoid.
fn compact_symbol(
    head: &crate::chunk::head::SymbolProjection,
    format: SymbolFormat,
    package: &crate::store::package::PackageView,
) -> CompactSymbolDoc {
    let mut path_parts = head
        .breadcrumb
        .iter()
        .map(|crumb| crumb.label.to_string())
        .collect::<Vec<_>>();
    path_parts.push(head.name.to_string());
    // §4.9 of docs/MCP-SURFACE-PLAN.md: the separator is the ecosystem's own
    // idiomatic one, not a global `"::"` — that hardcoding is what made every
    // Java path render with Rust's separator. `PathStyle` is the shared
    // decision `nudox_ir::reflect::moniker_path_styled` also defers to.
    let path = path_parts.join(
        crate::wire::PathStyle::for_ecosystem(head.key.package.ecosystem.as_str()).separator(),
    );
    let rendered_signature = || {
        head.signature
            .iter()
            .map(signature_text)
            .collect::<String>()
    };
    let mut references = Vec::new();
    let mut seen_references = HashSet::new();
    for token in &head.signature {
        let SigToken::Ty {
            text,
            target: Some(target),
        } = token
        else {
            continue;
        };
        let reference = CompactSymbolReference {
            text: text.to_string(),
            target: SymbolKeyDto::from_wire(target),
        };
        if seen_references.insert(reference.clone()) {
            references.push(reference);
        }
    }
    let location = match &head.source {
        SourceLocation::Declared { file, start, .. } => {
            Some(format!("{}:{}:{}", file, start.line, start.column))
        }
        SourceLocation::BytesOnly { file, bytes } => {
            Some(format!("{}#bytes={}-{}", file, bytes[0], bytes[1]))
        }
        SourceLocation::Unlocated { .. } => None,
    };
    let source = matches!(format, SymbolFormat::Source)
        .then(|| head.source_excerpt.as_ref().map(ToString::to_string))
        .flatten();
    let signature = if matches!(format, SymbolFormat::Signature) || source.is_none() {
        Some(rendered_signature())
    } else {
        None
    };
    let address =
        crate::mcp::address::render_address(package, &head.key.package, None, head.key.intro)
            .map(|a| a.to_string());
    CompactSymbolDoc {
        key: SymbolKeyDto::from_wire(&head.key),
        address,
        path,
        signature,
        kind: head.kind,
        visibility: head.visibility,
        references,
        location,
        source,
        deprecation: head.deprecation.as_ref().map(ToString::to_string),
    }
}

/// Render one signature token without exposing the GUI token vocabulary at the
/// MCP boundary. The Markdown projection uses this exact formatter so an
/// agent sees the same signature text as the compact symbol reader.
pub(crate) fn signature_text(token: &SigToken) -> &str {
    match token {
        SigToken::Kw(text) | SigToken::Punct(text) => text,
        SigToken::Ident(text)
        | SigToken::Ty { text, .. }
        | SigToken::Generic(text)
        | SigToken::Lifetime(text) => text.as_ref(),
        SigToken::Ws => " ",
        _ => "",
    }
}

fn encode_cursor(offset: usize) -> String {
    offset.to_string()
}

/// Decode a pagination cursor back into a page offset. `None` (the first
/// page) decodes to `0`.
fn decode_cursor(cursor: Option<&str>) -> Result<usize, McpError> {
    let Some(raw) = cursor else {
        return Ok(0);
    };
    let offset: usize = raw.parse().map_err(|_| McpError::InvalidArgument {
        argument: "cursor",
        reason: format!("{raw:?} is not a cursor this server issued"),
    })?;
    if offset > MAX_OFFSET {
        return Err(McpError::InvalidArgument {
            argument: "cursor",
            reason: format!(
                "cursor offset {offset} exceeds the {MAX_OFFSET}-row pagination ceiling; \
                 narrow the query instead of paging this far into it"
            ),
        });
    }
    Ok(offset)
}

/// Slice one page out of an already-fetched, already-deterministically-ordered
/// row set, and report whether more rows exist beyond this page.
///
/// `fetch_may_have_capped` is `true` when the underlying fetch itself might
/// have stopped short of the true result set (its own internal limit was
/// reached). In that case `has_more` is unconditionally `true`: there is no
/// way from here to distinguish "exactly this many rows exist" from "more
/// rows exist past where the fetch stopped looking" without fetching deeper —
/// which is exactly what requesting the next page does. This never *hides* a
/// real next page; the one failure mode it can produce is an honest empty
/// final page when the fetch boundary happened to land exactly on the true
/// end of the result set, which is a wasted call, not a wrong answer.
fn paginate_rows<T>(
    rows: Vec<T>,
    offset: usize,
    limit: usize,
    fetch_may_have_capped: bool,
) -> (Vec<T>, bool) {
    if offset >= rows.len() {
        return (Vec::new(), fetch_may_have_capped);
    }
    let end = (offset + limit).min(rows.len());
    let remainder_within_fetch = end < rows.len();
    let mut rows = rows;
    let page = rows.drain(offset..end).collect();
    (page, remainder_within_fetch || fetch_may_have_capped)
}

/// The raw result of draining one Trustfall query stream, before either the
/// non-paginated caller (`list_packages`, which has no cursor) or the two
/// paginated ones (`find_usages`, `graph_query`, via
/// [`NudoxTools::run_query_page`]) decide what their own `next_cursor` is.
struct RawQueryResult {
    /// Column names, in the order the query's `@output` directives declared them.
    columns: Vec<String>,
    /// Every row collected, up to the fetch limit passed to `run_query`.
    rows: Vec<QueryResultRow>,
    /// `true` when the stream had more rows than the fetch limit allowed us
    /// to collect.
    truncated: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One [`PackageView`] fetch per distinct lineage, shared across a batch of
/// rows by [`NudoxTools::cached_package`]/[`NudoxTools::cached_address`].
type PackageCache = std::collections::HashMap<
    crate::wire::PackageLineageId,
    Option<std::sync::Arc<crate::store::package::PackageView>>,
>;

/// The canonical `"ecosystem:name#introhex"` string for a hit's key, used as
/// the dedup/tiebreak/section-membership key across `do_search`,
/// `do_semantic_search` and `do_unified_search` — kept as one function so the
/// three cannot drift on what "the same row" means.
fn hit_key_string(hit: &HitRow) -> String {
    format!(
        "{}:{}#{}",
        hit.key.package.ecosystem.as_str(),
        hit.key.package.name.as_str(),
        hit.key.intro.to_hex(),
    )
}

/// Apply the default and the ceiling to a caller-supplied limit.
fn clamp_limit(requested: Option<u32>) -> usize {
    match requested {
        None => DEFAULT_LIMIT,
        // 0 is treated as "unlimited" by `SearchQuery`, which is exactly what
        // MAX_LIMIT exists to prevent; clamp it up to the ceiling instead.
        Some(0) => MAX_LIMIT,
        Some(n) => (n as usize).min(MAX_LIMIT),
    }
}

/// Parse the shared name/type/semantic search filters once for every search
/// facet. Keeping package filtering inside `SearchQuery` is important: the
/// engine truncates each section before returning it, so filtering afterward
/// can erase a package that should have occupied a page.
fn parse_search_filters(
    kinds: Option<&Vec<String>>,
    packages: Option<&Vec<String>>,
) -> Result<(Vec<KindDiscriminant>, Vec<crate::wire::PackageLineageId>), McpError> {
    let kinds = match kinds {
        None => Vec::new(),
        Some(names) => {
            let mut out = Vec::with_capacity(names.len());
            for name in names {
                out.push(parse_kind(name).ok_or_else(|| McpError::InvalidArgument {
                    argument: "kinds",
                    reason: format!(
                        "{name:?} is not a known kind; expected one of {}",
                        known_kind_names().join(", ")
                    ),
                })?);
            }
            out
        }
    };

    let packages = match packages {
        None => Vec::new(),
        Some(names) => {
            if names.is_empty() {
                return Err(McpError::InvalidArgument {
                    argument: "packages",
                    reason: "must contain at least one 'ecosystem:name' entry, or be omitted"
                        .to_owned(),
                });
            }
            let mut out = Vec::with_capacity(names.len());
            for name in names {
                let lineage = PackageLineageDto(name.clone()).to_wire().map_err(|e| {
                    let reason = match e {
                        McpError::MalformedPackage { reason, .. } => reason,
                        _ => "not 'ecosystem:name'",
                    };
                    McpError::InvalidArgument {
                        argument: "packages",
                        reason: format!("{name:?} is not 'ecosystem:name': {reason}"),
                    }
                })?;
                out.push(lineage);
            }
            out
        }
    };
    Ok((kinds, packages))
}

/// Resolve `kinds` for `search`/`search_symbols`, and what a defaulted scope
/// held back — for `SearchResult::excluded_kinds`.
///
/// `kinds: None` is not "no filter" here, unlike `packages: None` two lines
/// above it in the same args struct: an omitted kind list is treated as the
/// caller not yet knowing that members (`Field`, `Variant`, `Param`) exist,
/// vastly outnumber declarations in a real package, and would need naming to
/// reach — not as a request to see everything. See
/// `tests/mcp/search_member_noise.rs` and `default_search_kinds` below for why
/// that default is a scope rather than a bigger discount on `kind_weight`.
///
/// `kinds: Some(_)` is untouched, and always will be: naming a kind is
/// answering a different question ("where is this parameter used") than an
/// exploratory query, and the default must never become a filter the caller
/// cannot turn off.
/// Returns `(kinds, exclude_kinds, packages, disclosed)`.
///
/// The default scope is expressed as an **exclusion**, never as a filled-in
/// `kinds` list. See [`crate::SearchQuery::exclude_kinds`]: a non-empty `kinds`
/// switches the kind-facet section on, which would make every query — a
/// nonsense one included — return every declaration in the corpus.
fn resolve_search_scope(
    kinds: Option<&Vec<String>>,
    packages: Option<&Vec<String>>,
) -> Result<
    (
        Vec<KindDiscriminant>,
        Vec<KindDiscriminant>,
        Vec<crate::wire::PackageLineageId>,
        Option<Vec<KindTag>>,
    ),
    McpError,
> {
    let explicit = kinds.is_some();
    let (parsed_kinds, packages) = parse_search_filters(kinds, packages)?;
    if explicit {
        return Ok((parsed_kinds, Vec::new(), packages, None));
    }
    let excluded = default_excluded_kinds();
    let disclosed = excluded.iter().copied().map(KindTag::from).collect();
    Ok((Vec::new(), excluded, packages, Some(disclosed)))
}

/// Map the engine's honest semantic coverage state to the MCP result model.
fn semantic_status(state: SectionState) -> SemanticStatus {
    match state {
        SectionState::Complete => SemanticStatus::Ready,
        SectionState::Building { covered, total } => SemanticStatus::Building { covered, total },
        // The remedy is carried, not just the label. `Unavailable::remedy` is
        // the engine's own answer to "what do I do about this"; re-deriving it
        // here from the label would be a second place for that text to live.
        SectionState::Unavailable { reason } => SemanticStatus::Unavailable {
            reason: format!("{reason:?}"),
            remedy: reason.remedy().to_owned(),
        },
    }
}

/// Parse a scalar span from the graph plane without turning a corrupt row into
/// an apparently valid zero-length occurrence.
fn parse_occurrence_offset(value: &Option<String>, field: &'static str) -> Result<u32, McpError> {
    let value = value.as_deref().ok_or_else(|| McpError::InvalidArgument {
        argument: field,
        reason: "the graph row omitted a required occurrence offset".to_owned(),
    })?;
    value.parse::<u32>().map_err(|_| McpError::InvalidArgument {
        argument: field,
        reason: format!("the graph row contained a non-integer offset {value:?}"),
    })
}

/// Look a cell up by column name rather than by position.
///
/// Trustfall does not promise a column order across queries, so the tools that
/// build typed rows from a generic result address cells by name.
fn column(columns: &[String], row: &QueryResultRow, name: &str) -> Option<String> {
    let idx = columns.iter().position(|c| c == name)?;
    row.cells.get(idx).cloned()
}

/// Every kind discriminant this binary knows, by name.
///
/// Derived from `KindDiscriminant::from_u16` rather than hand-listed, so a new
/// IR kind becomes searchable without touching this crate.
fn known_kinds() -> impl Iterator<Item = KindDiscriminant> {
    (0..=u16::from(u8::MAX)).filter_map(KindDiscriminant::from_u16)
}

/// `search`'s default scope: every kind `search::is_member_kind` does not
/// mark a member, i.e. declarations a reader can navigate straight to rather
/// than parts of one.
///
/// # Why a scope, not a bigger reweight
///
/// `search`'s ranking already discounts members through `kind_weight`
/// (`search/hits.rs`) — `Param` at 0.7, `Field`/`Variant` at 0.85 — but that
/// discount is multiplicative over text relevance, so it only *reorders* hits
/// of comparable relevance. An exact-name match on a parameter still
/// outranks a prefix match on the function that declares it: a 12.6k-line
/// package indexes 3,039 symbols, most of them `Param`, and a bigger
/// discount only changes which of those 3,039 win, not whether one of them
/// wins. Excluding members from consideration before ranking runs is a
/// different claim than scoring them lower, and it is the one that keeps a
/// query for a common word like `connection` from paging entirely through
/// parameters — see `tests/mcp/search_member_noise.rs`, which pins exactly
/// this: a reweight that pushed `Param` under `Function` would still satisfy
/// `kind_weight`'s own tests and still fail this crate's.
///
/// # Field and Variant, not just Param
///
/// The motivating complaint was `Param`, but `Field` and `Variant` are held
/// back too, on `kind_weight`'s own partition (`is_member_kind` is literally
/// "scores below `1.0`" — see that function). A struct's fields are exactly
/// as much "reachable only through the owner, not a destination in their own
/// right" as a function's parameters: a type with a dozen fields would still
/// flood an exploratory query the way a function's parameters do today, just
/// scaled to fewer languages. Drawing the default-scope line narrower than
/// the ranking line — excluding only `Param` — would need its own
/// justification that "browse this package's API" does not supply, and would
/// leave `symbol_count`-style noise for any type with many fields or a large
/// enum. Naming `Field` or `Variant` in `kinds` still reaches them; nothing
/// here removes them from the index, only from the unfiltered page.
/// # Why this returns what to EXCLUDE rather than what to search
///
/// The complement — "every non-member kind" — is the same set, and putting it
/// in `SearchQuery::kinds` is not the same request: a non-empty `kinds` turns
/// on the kind-facet section, which emits every symbol of those kinds whether
/// or not the query matched anything. Expressed that way the default scope
/// made a nonsense query return the entire corpus. See
/// [`crate::SearchQuery::exclude_kinds`].
fn default_excluded_kinds() -> Vec<KindDiscriminant> {
    known_kinds()
        .filter(|k| crate::search::is_member_kind(*k))
        .collect()
}

/// The names accepted by the `kinds` argument.
pub(crate) fn known_kind_names() -> Vec<String> {
    known_kinds().map(|k| format!("{k:?}")).collect()
}

/// Parse a kind name case-insensitively.
fn parse_kind(name: &str) -> Option<KindDiscriminant> {
    known_kinds().find(|k| format!("{k:?}").eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_cover_the_frozen_discriminants() {
        let names = known_kind_names();
        for expected in [
            "Module", "Record", "Field", "Function", "Alias", "Trait", "Impl", "Enum", "Variant",
            "Const", "Static", "Reexport", "Param",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing kind {expected}"
            );
        }
    }

    #[test]
    fn kind_parsing_is_case_insensitive_and_rejects_junk() {
        assert_eq!(parse_kind("function"), parse_kind("Function"));
        assert!(parse_kind("Function").is_some());
        assert!(parse_kind("  trait  ").is_some());
        assert!(parse_kind("not-a-kind").is_none());
    }

    #[test]
    fn limits_default_and_clamp() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(u32::MAX)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(0)), MAX_LIMIT, "0 must not mean unlimited");
    }

    #[test]
    fn columns_are_addressed_by_name_not_position() {
        let columns = vec!["name".to_owned(), "key".to_owned()];
        let row = QueryResultRow {
            cells: vec!["Deserializer".into(), "cargo:serde#ab".into()],
        };
        assert_eq!(
            column(&columns, &row, "key").as_deref(),
            Some("cargo:serde#ab")
        );
        assert_eq!(
            column(&columns, &row, "name").as_deref(),
            Some("Deserializer")
        );
        assert_eq!(column(&columns, &row, "absent"), None);
    }

    #[test]
    fn packages_query_parses_against_the_real_schema() {
        // Guards the one Trustfall query this crate authors itself.
        assert!(PACKAGES_QUERY.contains("Packages"));
        assert!(PACKAGES_QUERY.contains("lineage @output"));
    }
}
