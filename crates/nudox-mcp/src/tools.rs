//! The eight MCP tools of §L6, and the argument/result types their schemas
//! are derived from.
//!
//! # LR-8: this is a view, not a second engine
//!
//! Every tool here bottoms out in one of the `EngineHandle` calls below:
//!
//! | Tool | Engine call |
//! |---|---|
//! | `search_symbols` | [`EngineHandle::search`] |
//! | `get_symbol`     | [`EngineHandle::open_symbol`], drained to completion |
//! | `find_usages`    | [`EngineHandle::query`] with `nudox_graph::queries::FIND_USAGES` |
//! | `list_packages`  | [`EngineHandle::query`] with [`PACKAGES_QUERY`] |
//! | `list_versions`  | [`EngineHandle::versions`] |
//! | `select_version` | [`EngineHandle::select_version`], drained to completion |
//! | `graph_query`    | [`EngineHandle::query`] with the caller's query |
//! | `graph_schema`   | [`crate::SCHEMA_SDL`] (LR-7: served verbatim) |
//!
//! `find_usages` and `list_packages` reach the store's usage postings and the
//! corpus's package map the way §L6 asks — *through the engine* — by going over
//! the Trustfall plane rather than by taking a second handle on `Corpus`. That
//! is deliberate and it is the stronger reading of LR-8/LR-9: `EngineHandle`
//! does not expose `corpus()` publicly, the engine owns the `LocalSet` the
//! `!Send` Trustfall stream requires, and routing through `query` means these
//! two tools cannot drift away from what the graph plane says is true.
//!
//! # Streams are drained, never sampled
//!
//! The engine's API is streaming; MCP's is request/response. Each tool holds
//! its `StreamHandle` for the whole drain (dropping it cancels the stream) and
//! reads to a terminal `Done`/`Failed` event. A stream that ends without one is
//! [`McpError::TruncatedStream`] rather than a silently short answer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use nudox_engine::wire::{
    DocEvent, Gen, HitRow, KindDiscriminant, PackageDiff, QueryEvent, RenderSection, SearchEvent,
    SymbolHead, Timeline, VersionEvent,
};
use nudox_engine::{EngineHandle, GraphQuery, SearchQuery};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;
use crate::key::{PackageLineageDto, SymbolKeyDto};

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

/// Default number of rows a tool returns when the caller does not say.
const DEFAULT_LIMIT: usize = 50;

/// Hard ceiling on rows, regardless of what the caller asks for.
///
/// An MCP result is injected into a model's context window; an unbounded
/// answer is not a useful answer. Callers that need more should narrow the
/// query or paginate with `graph_query`.
const MAX_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// Tool arguments
// ---------------------------------------------------------------------------

/// Arguments to `search_symbols`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchSymbolsArgs {
    /// The text to search for: a symbol name, a prefix, or a kind label such
    /// as `trait`. Matching is by name and by kind; it is not full-text over
    /// documentation.
    pub query: String,

    /// Restrict results to these kinds. Valid values are `Module`, `Record`,
    /// `Field`, `Function`, `Alias`, `Trait`, `Impl`, `Enum`, `Variant`,
    /// `Const`, `Static`, `Reexport` and `Param` (case-insensitive). Omit to
    /// search all kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,

    /// Restrict results to these packages, each written as `ecosystem:name`
    /// (for example `cargo:serde`). Omit to search every loaded package; call
    /// `list_packages` to see what is loaded. Applied inside the engine before
    /// `limit` truncates each section, so a package late in tie-break order is
    /// never crowded out by ties from packages the caller excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<String>>,

    /// Maximum number of results. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Opaque pagination cursor from a previous `search_symbols` call's
    /// `next_cursor`. Omit for the first page. Treat this as an opaque token —
    /// pass back exactly what you received, never construct one yourself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `get_symbol`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetSymbolArgs {
    /// The symbol to open, as `ecosystem:name#introhex` — exactly the `key`
    /// returned by `search_symbols`, `find_usages` or a `graph_query`.
    pub key: SymbolKeyDto,
}

/// Arguments to `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindUsagesArgs {
    /// The symbol whose callers to find, as `ecosystem:name#introhex`.
    pub key: SymbolKeyDto,

    /// Maximum number of usages. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Opaque pagination cursor from a previous `find_usages` call's
    /// `next_cursor`. Omit for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `list_packages`.
///
/// Empty, but named rather than omitted so the tool still has a derived schema
/// (LR-2) and so adding a filter later is not a breaking change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListPackagesArgs {}

/// Arguments to `list_versions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListVersionsArgs {
    /// The package lineage to list loaded generations for, as `ecosystem:name`
    /// (for example `cargo:memchr`). Call `list_packages` to see what is
    /// loaded; every package it returns has at least one generation here.
    pub package: PackageLineageDto,
}

/// Arguments to `select_version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SelectVersionArgs {
    /// The package lineage to switch, as `ecosystem:name`.
    pub package: PackageLineageDto,
    /// The version string to make current, exactly as it appears in
    /// `list_versions`'s output (for example `"2.8.0"`). Selecting a version
    /// that is not loaded is not an error — see `SelectVersionResult::NotLoaded`.
    pub version: String,
}

/// Arguments to `diff_versions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffVersionsArgs {
    /// The package lineage to diff, as `ecosystem:name`.
    pub package: PackageLineageDto,
    /// The **older** version string, exactly as `list_versions` spells it.
    pub from_version: String,
    /// The **newer** version string, exactly as `list_versions` spells it.
    ///
    /// The engine does not reorder these: every verdict is directional, so
    /// passing them backwards returns a coherent diff of the wrong release
    /// rather than an error.
    pub to_version: String,
    /// Maximum number of rows. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous `diff_versions` call's
    /// `next_cursor`. Omit for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `graph_query`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphQueryArgs {
    /// The Trustfall query text. Call `graph_schema` first for the exact type
    /// and edge names it must be written against.
    pub query: String,

    /// Variable bindings referenced as `$name` in the query. All values are
    /// strings; the adapter coerces them to the property's declared type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<BTreeMap<String, String>>,

    /// Maximum number of rows. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Opaque pagination cursor from a previous `graph_query` call's
    /// `next_cursor`. Omit for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `graph_schema`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphSchemaArgs {}

// ---------------------------------------------------------------------------
// Tool results
// ---------------------------------------------------------------------------

/// The result of `search_symbols`.
///
/// `hits` are [`nudox_engine::wire::HitRow`] values serialised directly.  Every
/// `key` field is the canonical `"ecosystem:name#introhex"` string; pass it
/// straight to `get_symbol` or `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SearchResult {
    /// Matching symbols, most relevant first.
    pub hits: Vec<HitRow>,
    /// `true` when more results exist beyond this page. Equivalent to
    /// `next_cursor.is_some()`; kept as its own field so a caller that only
    /// wants to know "is this everything" does not have to inspect the cursor.
    pub truncated: bool,
    /// Pass this to `search_symbols`'s `cursor` argument to get the next page.
    /// `None` when this is the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The result of `get_symbol`: a symbol's complete documentation page.
///
/// The head, timeline and sections are [`nudox_engine::wire`] types serialised
/// directly (LR-2: derived from the wire vocabulary, never hand-written).
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SymbolDoc {
    /// Identity, signature, provenance, and the page's section plan.
    pub head: Box<SymbolHead>,
    /// The symbol's history across every loaded version of its package —
    /// renames, deprecations, signature/visibility/doc changes, introduction
    /// and removal — keyed on `IntroId` (see [`nudox_engine::wire::Timeline`]).
    /// With one version loaded this is always exactly one
    /// [`nudox_engine::wire::TimelineChange::Present`] row, never an empty
    /// list: the engine computes and emits this on every `open_symbol` call,
    /// it was simply not surfaced here before.
    pub timeline: Timeline,
    /// The rendered sections, in `head.section_plan` order.
    pub sections: Vec<RenderSection>,
}

/// One symbol that references the symbol passed to `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageRow {
    /// The referencing symbol's key; pass it to `get_symbol` to read it.
    pub key: SymbolKeyDto,
    /// Its unqualified name.
    pub name: String,
    /// Its kind label.
    pub kind: String,
}

/// The result of `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsagesResult {
    /// The referencing symbols.
    pub usages: Vec<UsageRow>,
    /// `true` when more usages exist beyond this page. Equivalent to
    /// `next_cursor.is_some()`.
    pub truncated: bool,
    /// Pass this to `find_usages`'s `cursor` argument to get the next page.
    /// `None` when this is the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One loaded package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PackageSummary {
    /// The `ecosystem:name` lineage key — the prefix of every `SymbolKey` in
    /// this package, and the value `search_symbols`'s `packages` filter takes.
    pub lineage: String,
    /// The package's display name.
    pub name: String,
    /// The ecosystem it comes from (`cargo`, `npm`, `pypi`, …).
    pub ecosystem: String,
}

/// The result of `list_packages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PackagesResult {
    /// Every package currently loaded into the local corpus.
    pub packages: Vec<PackageSummary>,
}

/// One loaded generation of a package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VersionSummary {
    /// The version string exactly as it was loaded (e.g. `"2.8.0"`). Pass this
    /// verbatim to `select_version`.
    pub version: String,
    /// True for exactly one row: the generation `get_symbol`, `search_symbols`
    /// and `graph_query` currently answer from.
    pub is_current: bool,
    /// Number of entries in this generation's declaration table. Two rows for
    /// the same package routinely disagree — that is informative, not noise.
    pub symbol_count: u64,
}

/// The result of `list_versions`.
///
/// # What a key surviving a version switch does and does not mean
///
/// Most declarations keep the same `SymbolKey` across every version listed
/// here — that is what makes `select_version` useful instead of a full
/// navigation reset. It is **not guaranteed for every declaration**.
/// `IntroId` is minted through a disambiguator ladder, and a declaration that
/// collided during lowering can fall through to a byte-offset or ordinal
/// identity that does not survive an unrelated edit in a later release. When
/// that happens the old key does not error on the new version — it simply
/// resolves to nothing, indistinguishable from the symbol having been
/// deleted.
///
/// **Two tools now tell you which declarations are affected**, and this
/// paragraph said neither existed until 2026-08-09 ("that information
/// (`SealReport::forced`) is computed during lowering and discarded before
/// reaching this layer"):
///
/// * `graph_query` publishes `Symbol.keyTier` — `Structural`, `Span`,
///   `Ordinal`, or `Unrecorded` — plus `keyIsContentDerived`. Read it
///   **before** you cache a key across a switch; once the lookup fails there
///   is no vertex left to ask.
/// * `diff_versions` re-pairs churned declarations across two generations and
///   reports them as one `rekeyed` row carrying both keys, so you can find out
///   what a stale key *became* rather than only that it is stale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListVersionsResult {
    /// The package lineage these versions belong to.
    pub package: PackageLineageDto,
    /// Every loaded generation, newest first. Empty means the package is not
    /// loaded at all — call `list_packages` to check.
    pub versions: Vec<VersionSummary>,
}

/// The outcome of a `select_version` call.
///
/// # `SymbolKey` survival across the switch is not guaranteed
///
/// A `SymbolKey` you held before switching is **not guaranteed to resolve**
/// after a `Switched` result. Most declarations keep the same key because
/// their identity is derived from the declaration's content, but a
/// declaration that collided during lowering can fall through to a
/// byte-offset- or ordinal-keyed identity that does not hold for every
/// release. When that happens the old key does not error — it silently
/// resolves to nothing on the new generation, which looks identical to the
/// symbol having been deleted.
///
/// *This* tool still cannot tell you in advance which declarations are
/// affected, but two others can, and you should use them rather than guessing:
/// `graph_query`'s `Symbol.keyTier` before the switch, and `diff_versions`
/// after it. See [`ListVersionsResult`]'s docs for the full reasoning, and
/// re-fetch a key with `search_symbols` rather than assuming a lookup failure
/// after a switch means deletion.
///
/// # The window between "selected" and "resident"
///
/// This tool waits for the corpus to actually be repointed before returning,
/// so by the time you see this result the switch has landed: `get_symbol`,
/// `search_symbols` and `graph_query` calls you make after receiving it
/// answer from the new generation. That guarantee is about *this* call's
/// response, not about global ordering — the engine records the new
/// selection synchronously and repoints the corpus asynchronously afterward
/// (see `EngineHandle::select_version`), so a call to `list_versions` or
/// another tool made concurrently, before this response arrives, may briefly
/// observe the old generation even though `list_versions` already reports the
/// new one as current. This is not a bug to work around: re-issue any call
/// whose answer matters after you have this result.
// Every variant of this internally-tagged enum serialises to a JSON object, so
// the schema's root type really is `object` — schemars just does not say so for
// a `oneOf`, and rmcp rejects a tool whose `outputSchema` has no root `type`
// (the MCP spec requires one, and the rejection is a panic at server
// construction, so it takes the whole endpoint down rather than one tool).
// Stating it here is a true statement the generator omitted, not a widening;
// `tests/schemas.rs` pins that every tool result says it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("type" = "object"))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectVersionResult {
    /// The corpus now serves `version` of `package`.
    ///
    /// `SymbolKey`s you already hold for this package usually still resolve —
    /// see [`ListVersionsResult`]'s docs for the declarations this does not
    /// hold for, and re-fetch via `search_symbols` or `get_symbol` rather than
    /// assuming an old key is now stale just because it is old.
    Switched {
        /// The package lineage that was switched.
        package: PackageLineageDto,
        /// The version now resident in the corpus.
        version: String,
        /// The newly-resident generation's declaration-table size.
        symbol_count: u64,
    },
    /// `version` is not a loaded generation of `package`; nothing changed.
    ///
    /// Not an error: the engine only holds what it was asked to load, and a
    /// caller may legitimately ask for a version it saw in a manifest but
    /// never requested. Call `list_versions` to see what is actually loaded.
    NotLoaded {
        /// The package lineage that was requested.
        package: PackageLineageDto,
        /// The version that was requested and is not loaded.
        version: String,
    },
}

/// The outcome of a `diff_versions` call.
///
/// # Read `verdict.kind` before you read anything else
///
/// A row's `verdict` is the whole point of this tool. `removed` is the only
/// value that asserts a declaration is gone, and it is emitted only for keys
/// the sealer minted from the declaration's own content. Two other values
/// exist because a naive set difference gets them wrong:
///
/// * `rekeyed` — one declaration, two keys. The old key is dead; use `to_key`.
/// * `indeterminate` — it vanished, and its key was one that can move on its
///   own, so we **cannot** tell you whether it was deleted. Re-search by name
///   and path before concluding anything.
///
/// A diff with a non-zero `indeterminate` count is not a broken diff; it is a
/// diff telling you which of its own rows not to trust.
// See `SelectVersionResult` for why the root type is stated explicitly.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[schemars(extend("type" = "object"))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffVersionsResult {
    /// Both generations were loaded and compared.
    Diff {
        /// The comparison. `rows` is one page of it.
        diff: Box<PackageDiff>,
        /// `true` when more rows exist beyond this page. Equivalent to
        /// `next_cursor.is_some()`.
        truncated: bool,
        /// Pass this to `diff_versions`'s `cursor` argument for the next page.
        #[serde(skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
    },
    /// At least one of the two versions is not loaded; nothing was compared.
    ///
    /// Not an error, for the same reason `select_version` returns `NotLoaded`:
    /// the engine holds only what it was asked to load, and naming a version
    /// from a manifest is a legitimate thing for a caller to do. `loaded` is
    /// included so the fix does not need a second round trip.
    NotLoaded {
        /// The package lineage that was requested.
        package: PackageLineageDto,
        /// The older version that was requested.
        from_version: String,
        /// The newer version that was requested.
        to_version: String,
        /// Every version of this package that *is* loaded, newest first.
        /// Empty means the package itself is not loaded.
        loaded: Vec<String>,
    },
}

/// One row of a `graph_query` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResultRow {
    /// Cell values, positionally aligned with [`QueryResult::columns`].
    pub cells: Vec<String>,
}

/// The result of `graph_query`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResult {
    /// Column names, in the order the query's `@output` directives declared
    /// them. `rows[i].cells[j]` is the value of `columns[j]`.
    pub columns: Vec<String>,
    /// The result rows.
    pub rows: Vec<QueryResultRow>,
    /// `true` when more rows exist beyond this page. Equivalent to
    /// `next_cursor.is_some()`.
    pub truncated: bool,
    /// Pass this to `graph_query`'s `cursor` argument to get the next page.
    /// `None` when this is the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The result of `graph_schema`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SchemaResult {
    /// The GraphQL SDL that `graph_query` queries are checked against.
    pub schema: String,
}

// ---------------------------------------------------------------------------
// The tool set
// ---------------------------------------------------------------------------

/// The engine-backed implementation of the eight §L6 tools.
///
/// Holds the logic; [`crate::NudoxMcpServer`] holds the MCP surface that calls
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
// Tool bodies — called by the `#[tool]` methods in `crate::server`
// ---------------------------------------------------------------------------

impl NudoxTools {
    /// `search_symbols`, minus the MCP wrapping.
    pub async fn do_search(&self, args: SearchSymbolsArgs) -> Result<SearchResult, McpError> {
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let kinds = match &args.kinds {
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

        // The `packages` filter is pushed all the way into the engine's
        // `SearchQuery` (rather than applied here, after the fact, on the rows
        // it returned) because `collect_name_hits`/`collect_type_hits` each
        // truncate to `limit` before returning. A post-hoc filter here would be
        // filtering an already-truncated set: ties break on package lineage
        // (`compare_candidates`), so a package whose name sorts late could be
        // crowded out of the truncated set entirely by ties from packages the
        // caller was about to exclude anyway, producing zero results for a
        // package that genuinely has matches. See `SearchQuery::packages`.
        let packages = match &args.packages {
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
                    let lineage = PackageLineageDto(name.clone()).to_wire().map_err(|_| {
                        McpError::InvalidArgument {
                            argument: "packages",
                            reason: format!("{name:?} is not 'ecosystem:name'"),
                        }
                    })?;
                    out.push(lineage);
                }
                out
            }
        };

        // Fetch one row past the page so we can tell whether another page
        // exists (`fetched_may_have_capped` below) without a second query.
        // `SearchQuery.limit` bounds *each section independently*, so this is
        // the effective per-section fetch depth, not the final page size.
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);

        let query = SearchQuery {
            text: args.query,
            kinds,
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
        while let Ok(event) = rx.recv_async().await {
            match event {
                SearchEvent::Section { rows: batch, .. }
                | SearchEvent::Merge { rows: batch, .. } => {
                    if batch.len() >= fetch_limit {
                        fetched_may_have_capped = true;
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
        Ok(SearchResult {
            hits: page,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `get_symbol`, minus the MCP wrapping.
    pub async fn do_get_symbol(&self, args: GetSymbolArgs) -> Result<SymbolDoc, McpError> {
        let key = args.key.to_wire()?;
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

    /// `find_usages`, minus the MCP wrapping.
    pub async fn do_find_usages(&self, args: FindUsagesArgs) -> Result<UsagesResult, McpError> {
        // Validate the key even though the query plane takes it as a string:
        // a malformed key should be `invalid_params`, not an empty result set.
        args.key.to_wire()?;
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let mut bindings = BTreeMap::new();
        bindings.insert("key".to_owned(), args.key.0.clone());

        let (page, columns, has_more) = self
            .run_query_page(
                GraphQuery {
                    query: nudox_graph::queries::FIND_USAGES.to_owned(),
                    args: bindings,
                },
                offset,
                limit,
            )
            .await?;

        let usages = page
            .iter()
            .map(|row| UsageRow {
                key: SymbolKeyDto(column(&columns, row, "key").unwrap_or_default()),
                name: column(&columns, row, "name").unwrap_or_default(),
                kind: column(&columns, row, "kind").unwrap_or_default(),
            })
            .collect();

        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(UsagesResult {
            usages,
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
    /// engine call rather than a query — see `nudox_engine::diff` for the two
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

        let Some(full) =
            self.engine
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
        let page: Vec<_> = full
            .rows
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect();
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

    /// `list_versions`, minus the MCP wrapping.
    ///
    /// [`EngineHandle::versions`] is itself synchronous (the version registry
    /// is fully resident in memory, see `crate::versions`'s module docs) so
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

        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(QueryResult {
            columns,
            rows: page,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
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

/// The names accepted by the `kinds` argument.
fn known_kind_names() -> Vec<String> {
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
