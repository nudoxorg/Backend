//! Tool result types, serialized directly from the wire vocabulary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::key::{PackageLineageDto, SymbolKeyDto};
use crate::wire::{HitRow, PackageDiff, RenderSection, SymbolHead, Timeline};

/// What is known about the bytes a package's documentation was produced from.
///
/// Mirrors [`crate::Integrity`] into the tool schema rather than
/// re-exporting it, because an MCP result type must be `JsonSchema`-derivable
/// as a flat, self-describing shape an agent can read without a discriminated
/// union — and because collapsing the two cases to a boolean would lose
/// precisely the part that matters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrityReport {
    /// True only when the registry published a digest of these exact bytes on
    /// a separate endpoint and it matched what was downloaded.
    ///
    /// False does **not** mean a check failed — a failed check discards the
    /// download and returns an error. It means no digest was available to
    /// check against; read `detail` for which registry and why.
    pub verified_against_published_digest: bool,
    /// `"sha256"`, `"sha512"` or `"sha1"` when verified; the algorithm of the
    /// locally computed digest otherwise.
    pub algorithm: String,
    /// The digest value: the registry's when verified, ours when not.
    pub digest: String,
    /// Where the digest came from, or why none was available.
    pub detail: String,
}

/// The state of an `index_package` job.
///
/// # Why the `schemars(extend)` attribute is load-bearing
///
/// This is an internally-tagged enum, so schemars emits a bare `oneOf` with no
/// root `"type"`. rmcp validates every tool's `outputSchema` when the router is
/// *constructed*, so a missing root type does not fail this tool — it panics
/// `NudoxMcpServer::new`, taking down every endpoint test, every host-lifecycle
/// test, and the GUI screenshot harness (which starts `McpService` before the
/// first frame). The failure surfaces nowhere near its cause.
///
/// `"type": "object"` is a true statement schemars simply omits for tagged
/// enums: every variant serialises as an object carrying a `status` field.
///
/// `SelectVersionResult` and `DiffVersionsResult` were fixed for this exact
/// reason earlier the same day, and a guard test was added — but as a
/// hand-maintained list of types, so this enum, added hours later by another
/// track, was not in it. The guard passed while the server panicked. See
/// `tests/schemas.rs`, where the list is now derived rather than written out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[schemars(extend("type" = "object"))]
pub enum IndexPackageResult {
    /// The package is in the corpus and every other tool can now see it.
    Indexed {
        /// The canonical package URL that was indexed.
        purl: String,
        /// The `ecosystem:name` lineage key — what `search_symbols`'s
        /// `packages` filter and `list_versions` accept.
        package: String,
        /// The version that was indexed.
        version: String,
        /// Public API symbols now searchable from this package.
        symbol_count: u64,
        /// The package's root symbol key, to pass straight to `get_symbol`.
        #[serde(skip_serializing_if = "Option::is_none")]
        root_key: Option<String>,
        /// What is known about the bytes this documentation came from.
        integrity: IntegrityReport,
    },

    /// The job is still running. Call again with the same `purl` to keep
    /// waiting; it will attach to this job, not start another.
    Running {
        /// The canonical package URL being indexed.
        purl: String,
        /// What the job is doing right now: `resolving`, `downloading`,
        /// `verifying`, `extracting`, `producing`, or `cached`.
        stage: String,
        /// Bytes downloaded so far, once the download has started.
        #[serde(skip_serializing_if = "Option::is_none")]
        received_bytes: Option<u64>,
        /// Total bytes, when the registry advertised a length.
        #[serde(skip_serializing_if = "Option::is_none")]
        total_bytes: Option<u64>,
        /// How long the job has been running, in seconds.
        elapsed_seconds: u64,
        /// True when this call attached to a job an earlier call started.
        joined_existing_job: bool,
    },
}

// ---------------------------------------------------------------------------
// Tool results
// ---------------------------------------------------------------------------

/// The result of `search_symbols`.
///
/// `hits` are [`crate::wire::HitRow`] values serialised directly.  Every
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
/// The head, timeline and sections are [`crate::wire`] types serialised
/// directly (LR-2: derived from the wire vocabulary, never hand-written).
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SymbolDoc {
    /// Identity, signature, provenance, and the page's section plan.
    pub head: Box<SymbolHead>,
    /// The symbol's history across every loaded version of its package —
    /// renames, deprecations, signature/visibility/doc changes, introduction
    /// and removal — keyed on `IntroId` (see [`crate::wire::Timeline`]).
    /// With one version loaded this is always exactly one
    /// [`crate::wire::TimelineChange::Present`] row, never an empty
    /// list: the engine computes and emits this on every `open_symbol` call,
    /// it was simply not surfaced here before.
    pub timeline: Timeline,
    /// The rendered sections, in `head.section_plan` order.
    pub sections: Vec<RenderSection>,
}

/// Compact MCP projection of the canonical symbol model.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CompactSymbolDoc {
    pub key: SymbolKeyDto,
    pub path: String,
    /// Rendered signature. Omitted when exact source already carries it;
    /// retained as the fallback for declarations without source text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Canonical wire kind; unknown future discriminants remain representable.
    pub kind: crate::wire::KindTag,
    /// Canonical access vocabulary shared with the indexed declaration.
    #[schemars(with = "String")]
    pub visibility: Visibility,
    /// Resolved type links appearing in the signature, flattened and
    /// deduplicated instead of repeating the full token stream.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<CompactSymbolReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation: Option<String>,
}

/// One first-class resolved link in a compact signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct CompactSymbolReference {
    pub text: String,
    pub target: SymbolKeyDto,
}

/// Batched compact symbol result, preserving input order.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SymbolsResult {
    pub symbols: Vec<CompactSymbolDoc>,
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
    /// Fully-qualified path of the referencing declaration.
    pub path: String,
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
