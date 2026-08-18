//! Tool result types, serialized directly from the wire vocabulary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::key::{PackageLineageDto, SymbolKeyDto};
use crate::wire::{HitRow, KindTag, PackageDiff, RenderSection, SymbolHead, Timeline, Visibility};

/// `skip_serializing_if` predicate for a `bool` that defaults to `false` —
/// absence *is* the false case, matching the boolean-omission discipline the
/// rest of this module's results already follow.
fn is_false(b: &bool) -> bool {
    !*b
}

/// What is known about the bytes a package's documentation was produced from.
///
/// Mirrors [`crate::Integrity`] into the tool schema rather than
/// re-exporting it, because an MCP result type must be `JsonSchema`-derivable
/// as a flat, self-describing shape an agent can read without a discriminated
/// union — and because collapsing the two cases to a boolean would lose
/// precisely the part that matters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// The result of `index`.
///
/// # Why this is three lists and not a `Result`
///
/// The batch is not one operation. Five targets are five fetches and five
/// producer runs, and by the time the third is discovered to be misspelled the
/// first two have already cost what they cost. A `Result` over the batch would
/// throw that work away and — worse — would say only *that* something failed,
/// leaving the caller to guess which of the five to correct.
///
/// So every target lands in exactly one of these lists, and each row names the
/// target it came from. Partial success is the normal case here, not an
/// exceptional one, which is why it has a shape rather than an error code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexResult {
    /// Packages now in the corpus and visible to every other tool.
    pub indexed: Vec<IndexedPackage>,
    /// Jobs still running when the deadline expired. Not a failure: call
    /// `index` again with the same targets to attach to them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub running: Vec<RunningTarget>,
    /// Targets that could not be indexed, one row each.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FailedTarget>,
    /// What could not be *learned*, as distinct from what failed.
    ///
    /// Carries the dependency scans that produced no edges for a reason other
    /// than "there are none" — a manifest format with no reader yet, a corrupt
    /// manifest. Empty and omitted in the ordinary case.
    ///
    /// This exists because the alternative is silence: a caller who asked for
    /// dependencies, got none, and was not told that nobody looked would
    /// conclude the package is standalone. That is the same defect as a
    /// reference graph that was never built reporting as a symbol with no
    /// callers, and it is worth a field to avoid.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_scanned: Vec<UnscannedDependencies>,
}

/// One package that reached the corpus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexedPackage {
    /// The target string this came from — the PURL or the path as the caller
    /// wrote it, or, for a dependency, the manifest declaration it was
    /// resolved from.
    pub target: String,
    /// The `ecosystem:name` lineage key — what `search`'s `packages` filter
    /// and `packages` accept.
    pub package: String,
    /// The version that was indexed.
    pub version: String,
    /// Public API symbols now searchable from this package.
    pub symbol_count: u64,
    /// The package's root symbol key, to pass straight to `read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_key: Option<String>,
    /// What is known about the bytes this documentation came from.
    pub integrity: IntegrityReport,
    /// True when the caller named this target; false when it was pulled in as
    /// a dependency of one.
    ///
    /// Not cosmetic: an agent that asked for one package and received three
    /// needs to know which one it was working on. Without this the caller
    /// cannot tell the package it opened from a transitive acquisition, and
    /// `packages` — which lists what is resident, not why — cannot tell it
    /// either.
    pub requested: bool,
}

/// One target whose job had not finished when the deadline expired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunningTarget {
    /// The target string, to pass back on the next call.
    pub target: String,
    /// What the job is doing right now: `resolving`, `downloading`,
    /// `verifying`, `extracting`, `producing`, or `cached`.
    pub stage: String,
    /// True when this call attached to a job an earlier call started.
    pub joined_existing_job: bool,
    /// True when the caller named this target directly.
    pub requested: bool,
}

/// One target that could not be indexed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FailedTarget {
    /// The target string exactly as the caller wrote it, so a typo is visible
    /// as a typo rather than as a canonicalised form the caller never typed.
    pub target: String,
    /// The failure, with whatever the acquisition layer knew about it.
    pub error: String,
    /// True when the caller named this target directly. A dependency that
    /// failed is a smaller problem than a named target that did.
    pub requested: bool,
}

/// A package whose dependency declarations could not be read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnscannedDependencies {
    /// The package whose manifest went unread.
    pub target: String,
    /// Why — an unread manifest format, a corrupt file, or no manifest at all.
    pub reason: String,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// One [`HitRow`] plus its address (docs/MCP-SURFACE-PLAN.md §4), when the
/// resolver could build one.
///
/// `#[serde(flatten)]` keeps every existing `hits[]` field (`key`,
/// `display_name`, `sig_preview`, `kind`, `provenance`, `score`) at the same
/// top level a caller reading `SearchResult` today already expects —
/// `address` is a pure addition, not a reshape.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SearchHitDoc {
    /// The engine's hit row, unchanged.
    #[serde(flatten)]
    pub hit: HitRow,
    /// This hit's address: a readable `sym-path`, with `#hash` present only
    /// when the path alone would not resolve back to this exact declaration
    /// (see [`crate::mcp::address::render_address`]). `None` when the
    /// declaration has no physical path to render (falls back to `key`,
    /// still present above).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// True when semantic ranking (also) surfaced this hit, i.e. it may not
    /// have matched by name or kind at all. Omitted (false) for an ordinary
    /// structural match — the common case — so this costs nothing there.
    #[serde(default, skip_serializing_if = "is_false")]
    pub semantic: bool,
    /// Exact documentation text the semantic index matched against. Present
    /// only when `semantic` is true and the declaration has documentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
}

/// The result of `search`.
///
/// `hits` are [`HitRow`] values (via [`SearchHitDoc`]) serialised directly.
/// Every `key` field is the canonical `"ecosystem:name#introhex"` string;
/// pass either it or `address` straight to `read` or `refs`. Structural
/// (name/kind) and semantic ranking run in the same call; a hit found only
/// through semantic similarity carries `semantic: true`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SearchResult {
    /// Matching symbols, most relevant first.
    pub hits: Vec<SearchHitDoc>,
    /// Present only when semantic ranking is not complete: `building` with
    /// coverage, or `unavailable` with why. Omitted when it is complete — the
    /// common case — so this costs nothing there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticStatus>,
    /// Present only when the default scope held kinds back. Omitted when
    /// the caller chose the scope — nothing to disclose then.
    ///
    /// `search`'s default excludes members (`Field`, `Variant`, `Param`) so
    /// an unfiltered query returns the declarations a reader browses to,
    /// not the parameters and fields that outnumber them (see
    /// `mcp::tools::default_search_kinds`). A silently narrowed default is
    /// still a trap for a caller who *wants* a parameter — this is how they
    /// learn the scope exists and what to pass to `kinds` to widen it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_kinds: Option<Vec<KindTag>>,
    /// `true` when more results exist beyond this page. Equivalent to
    /// `next_cursor.is_some()`; kept as its own field so a caller that only
    /// wants to know "is this everything" does not have to inspect the cursor.
    pub truncated: bool,
    /// Pass this to `search`'s `cursor` argument to get the next page.
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CompactSymbolDoc {
    /// Stable symbol identity shared by every MCP navigation tool.
    pub key: SymbolKeyDto,
    /// This declaration's address (docs/MCP-SURFACE-PLAN.md §4): a readable
    /// `sym-path`, with `#hash` present only when the path alone would not
    /// resolve back to this exact declaration. `None` when there is no
    /// physical path to render — `key` above is always present regardless.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Fully-qualified declaration path.
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
    /// Source location in package-relative file/line form, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Exact declaration source, including the body when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Deprecation message, when the declaration is deprecated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation: Option<String>,
}

/// One first-class resolved link in a compact signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct CompactSymbolReference {
    /// The type text as it appears in the declaration signature.
    pub text: String,
    /// Stable key of the resolved referenced declaration.
    pub target: SymbolKeyDto,
}

/// Batched compact symbol result, preserving input order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SymbolsResult {
    /// Compact symbol records in the same order as the requested keys.
    pub symbols: Vec<CompactSymbolDoc>,
}

/// One symbol that references the symbol passed to `find_usages`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UsageRow {
    /// The referencing symbol's key; pass it to `get_symbol` to read it.
    pub key: SymbolKeyDto,
    /// This symbol's address (docs/MCP-SURFACE-PLAN.md §4), when one could be
    /// rendered. `None` falls back to `key`, still present above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Its unqualified name, retained for typed callers.
    pub name: String,
    /// Its kind label, retained for typed callers.
    pub kind: String,
    /// Its complete rendered declaration signature.
    pub signature: String,
    /// Fully-qualified path of the referencing declaration.
    pub path: String,
}

/// The result of `find_usages`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UsagesResult {
    /// The referencing symbols.
    pub usages: Vec<UsageRow>,
    /// `true` when more usages exist beyond this page. Equivalent to
    /// `next_cursor.is_some()`.
    pub truncated: bool,
    /// Pass this to `find_usages`'s `cursor` argument to get the next page.
    /// `None` when this is the last page.
    pub next_cursor: Option<String>,
}

/// One exact reference owned by a symbol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OccurrenceRow {
    /// The stable key of the referenced symbol. The target may be unloaded.
    pub target_key: String,
    /// The resolved target's declaration context, when it remains loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<OccurrenceSymbol>,
    /// The declaration that owns this occurrence.
    pub owner: OccurrenceSymbol,
    /// Producer-specific reference category, such as `call` or `type`.
    pub reference_kind: String,
    /// Producer confidence label.
    pub confidence: String,
    /// Byte offset relative to the owning symbol's span start.
    pub span_start: u32,
    /// Exclusive byte offset relative to the owning symbol's span start.
    pub span_end: u32,
}

/// The type-bearing identity attached to an occurrence target or owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OccurrenceSymbol {
    /// Stable key for following the declaration.
    pub key: SymbolKeyDto,
    /// This declaration's address (docs/MCP-SURFACE-PLAN.md §4), when one
    /// could be rendered. `None` falls back to `key`, still present above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Complete rendered declaration signature.
    pub signature: String,
    /// Package-relative declaration path, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Exact occurrences owned by one symbol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GetOccurrencesResult {
    /// The symbol whose body owns these occurrences.
    pub owner: SymbolKeyDto,
    /// One row per exact reference.
    pub occurrences: Vec<OccurrenceRow>,
    /// True when more rows exist beyond this page.
    pub truncated: bool,
    /// Opaque cursor for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The result of `refs` (merges `find_usages` + `get_occurrences`,
/// docs/MCP-SURFACE-PLAN.md §5.1), tagged by which direction was asked for.
///
/// `In` is `find_usages`'s question — symbols holding a resolved reference to
/// `key` — and `Out` is `get_occurrences`'s — exact references owned by
/// `key`'s own body. The two are structurally different relations (one row
/// per referencing symbol vs. one row per exact reference with span offsets),
/// so they stay distinct variants rather than being forced into one shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "direction", rename_all = "snake_case")]
#[schemars(extend("type" = "object"))]
pub enum RefsResult {
    /// Symbols referencing the requested key.
    In {
        /// The referencing symbols.
        usages: Vec<UsageRow>,
        /// `true` when more rows exist beyond this page.
        truncated: bool,
        /// Pass this to `refs`'s `cursor` argument to get the next page.
        #[serde(skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
        /// Present only when the page is not authoritative. Omitted in the
        /// ordinary case, so it costs nothing there.
        #[serde(skip_serializing_if = "Option::is_none")]
        coverage: Option<ReferenceCoverage>,
    },
    /// Exact references owned by the requested key's body.
    Out {
        /// The symbol whose body owns these occurrences, resolved to its
        /// canonical key even when the request named an address.
        owner: SymbolKeyDto,
        /// One row per exact reference.
        occurrences: Vec<OccurrenceRow>,
        /// `true` when more rows exist beyond this page.
        truncated: bool,
        /// Pass this to `refs`'s `cursor` argument to get the next page.
        #[serde(skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
        /// Present only when the page is not authoritative. Omitted in the
        /// ordinary case, so it costs nothing there.
        #[serde(skip_serializing_if = "Option::is_none")]
        coverage: Option<ReferenceCoverage>,
    },
}

/// Whether a `refs` page can be trusted as an answer, or only reflects that
/// the package's producer never recorded a reference graph in the first
/// place.
///
/// An empty `usages`/`occurrences` page means two entirely different things
/// depending on which of these holds, and nothing else on the response tells
/// them apart: "this symbol genuinely has no callers" (an ordinary, useful
/// answer) versus "this package's producer never calls `record_occurrence`,
/// so *every* symbol in it looks uncalled" (today: every language but Rust).
/// See `PackageIndexes::occurrences_recorded`'s doc comment for how the
/// underlying fact is computed and why a genuinely reference-free package is
/// deliberately folded into `NotRecorded` rather than given its own state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[schemars(extend("type" = "object"))]
pub enum ReferenceCoverage {
    /// This package's producer recorded a reference graph. Never actually
    /// serialized — `RefsResult`'s `coverage` field collapses this variant to
    /// `None` at the call site, the same way `SearchResult::semantic`
    /// collapses `SemanticStatus::Ready`, so an ordinarily-answered `refs`
    /// call costs nothing for a status that is always the same value.
    Recorded,
    /// It did not, so an empty page means "not indexed", not "no callers".
    NotRecorded {
        /// The ecosystem this key's package was loaded under (e.g. `"npm"`,
        /// `"cargo"`) — carried straight from the resolved key rather than
        /// looked up through a hand-maintained ecosystem→language table, so
        /// this label can never drift from the package it is describing.
        language: String,
    },
}

/// Why semantic search is or is not complete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[schemars(extend("type" = "object"))]
pub enum SemanticStatus {
    /// Every package in scope was embedded.
    Ready,
    /// Only the covered packages contributed to the current ranking.
    Building {
        /// Number of packages already embedded.
        covered: u32,
        /// Number of packages in the search scope.
        total: u32,
    },
    /// No semantic ranking was produced.
    Unavailable {
        /// The machine state, as a compact label (`NoModelConfigured`,
        /// `EmptyCorpus`, `ModelFailed`).
        reason: String,
        /// What to do about it.
        ///
        /// # Why the label is not enough on its own
        ///
        /// This field exists because `reason` is a *name for a state*, and a
        /// name is only actionable to a reader who already knows how this
        /// binary was built. `~sem:unavailable(NoEmbedder)` — the exact string
        /// this surface used to emit — is what sent a user looking for a
        /// broken embedder when the truth was that no runtime had been
        /// compiled in at all.
        ///
        /// Splitting `NoEmbedder` into `NoRuntime` and `NoModelConfigured`
        /// made the two states *distinguishable*; carrying the remedy is what
        /// makes them *actionable*. `NoRuntime` was later removed outright —
        /// the ONNX runtime became a plain, non-optional dependency of
        /// `nudox-engine` (no `onnx` cargo feature to be absent), so the state
        /// it named stopped being reachable — leaving `NoModelConfigured` as
        /// the only configuration gap this field can name.
        remedy: String,
    },
}

/// One semantic-search hit plus the exact documentation that was indexed.
///
/// This is an internal projection rather than a wire schema: MCP transports
/// render it as Markdown, while the typed fields keep ranking and evidence
/// explicit inside the tools layer.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHitRow {
    /// Stable symbol identity for follow-up calls.
    pub key: SymbolKeyDto,
    /// The leaf display name emitted by the search pipeline.
    pub display_name: String,
    /// Complete rendered declaration signature.
    pub signature: String,
    /// Canonical symbol kind, retained for typed callers and fallback output.
    pub kind: KindTag,
    /// Relevance score retained for optional diagnostics, not default output.
    pub score: f32,
    /// Exact documentation text used by the semantic corpus, when present.
    pub documentation: Option<String>,
}

/// A semantic-search result, kept separate from name/type hits.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticSearchResult {
    /// The original natural-language query.
    pub query: String,
    /// Whether the ranking is complete, partial, or unavailable.
    pub status: SemanticStatus,
    /// Ranked semantic hits.
    pub hits: Vec<SemanticHitRow>,
    /// True when more rows exist beyond this page.
    pub truncated: bool,
    /// Opaque cursor for the next page.
    pub next_cursor: Option<String>,
}

/// One loaded package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackagesResult {
    /// Every package currently loaded into the local corpus.
    pub packages: Vec<PackageSummary>,
}

/// One loaded generation of a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListVersionsResult {
    /// The package lineage these versions belong to.
    pub package: PackageLineageDto,
    /// Every loaded generation, newest first. Empty means the package is not
    /// loaded at all — call `list_packages` to check.
    pub versions: Vec<VersionSummary>,
}

/// One loaded package, with every generation of it that is loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageWithVersions {
    /// The `ecosystem:name` lineage key — the prefix of every `SymbolKey` in
    /// this package, and the value `search`'s `packages` filter takes.
    pub lineage: String,
    /// The package's display name.
    pub name: String,
    /// The ecosystem it comes from (`cargo`, `npm`, `pypi`, …).
    pub ecosystem: String,
    /// Every loaded generation, newest first.
    pub versions: Vec<VersionSummary>,
}

/// The result of `packages` (merges `list_packages` + `list_versions`,
/// docs/MCP-SURFACE-PLAN.md §5.1): what is loaded, and at what versions.
///
/// # What a key surviving a version switch does and does not mean
///
/// Most declarations keep the same `SymbolKey` across every version listed
/// here — that is what makes `select_version` useful instead of a full
/// navigation reset. It is **not guaranteed for every declaration**:
/// `IntroId` is minted through a disambiguator ladder, and a declaration that
/// collided during lowering can fall through to a byte-offset- or
/// ordinal-derived identity that does not survive an unrelated edit in a
/// later release. When that happens the old key does not error on the new
/// version — it simply resolves to nothing, indistinguishable from the
/// symbol having been deleted. `graph_query`'s `Symbol.keyTier` (read it
/// *before* caching a key across a switch) and `diff_versions` (re-pairs a
/// churned declaration's old and new key) both tell you which declarations
/// are affected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoadedPackagesResult {
    /// Every loaded package matching the request, each with its loaded
    /// versions. Requesting one `package` that is not loaded still returns
    /// one entry with an empty `versions` list, not an error.
    pub packages: Vec<PackageWithVersions>,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResultRow {
    /// Cell values, positionally aligned with [`QueryResult::columns`].
    pub cells: Vec<String>,
}

/// The result of `graph_query`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Present only when [`Self::rows`] is empty *and* the reason is that a
    /// traversed edge is empty by construction for every package currently
    /// loaded — not because the question genuinely has no answer. Omitted
    /// otherwise (the common case, including an ordinary empty result), so
    /// this costs nothing there. See [`EdgeCoverageNote`]'s doc comment for
    /// the incident this closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_coverage: Option<EdgeCoverageNote>,
}

/// A traversed graph edge that cannot answer for the packages currently
/// loaded, paired with the edge that does.
///
/// # The problem this closes
///
/// `Trait.implementors` and `subtypes` are the same question — "what
/// satisfies this type?" — asked of two different data shapes: Rust states
/// implementation through `impl` blocks (`Trait.implementors`, reading
/// `Impl.of`); every other supported language names its supertypes on the
/// record itself (`subtypes`, reading `Record.super_types`). A query against
/// the wrong one of the pair for the languages in scope returns `[]`, which
/// is the same shape a correct "no matches" answer has. An agent that reads
/// `[]` cannot tell "this trait genuinely has no implementors" from "this
/// edge cannot express implementation for this language, ask the other one" —
/// and the field report this test is named for spent two wrong graph queries
/// finding that out by hand (`graph_edge_language_fit.rs`'s module docs carry
/// the full incident).
///
/// This is the same discipline `RefsResult::coverage`
/// (`ReferenceCoverage::NotRecorded`) already applies to `refs`, and
/// `SearchResult::excluded_kinds` applies to a defaulted scope: the degraded
/// case gets a shape of its own rather than borrowing the ordinary case's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeCoverageNote {
    /// The edge the query traversed that is empty by construction for every
    /// package currently loaded.
    pub edge: String,
    /// The edge that DOES carry this relationship for those packages — the
    /// caller's next query should use this one, not guess again.
    pub answers_instead: String,
    /// Why, in prose: which relationship the dead edge cannot express for
    /// these languages.
    pub reason: String,
}

/// The result of `graph_schema`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SchemaResult {
    /// By default (`full: false`), the compact reference card
    /// ([`crate::mcp::SCHEMA_CARD`]) — complete for writing a `graph_query`.
    /// When the request set `full: true`, the verbatim `schema.graphql` SDL
    /// that `graph_query` queries are actually checked against.
    pub schema: String,

    /// True when [`Self::schema`] is the complete, verbatim SDL rather than
    /// the compact card. Lets a client distinguish the two without matching
    /// on content.
    #[serde(default, skip_serializing_if = "is_false")]
    pub full: bool,
}
