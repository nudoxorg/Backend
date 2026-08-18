//! Tool argument types, whose MCP schemas derive from these structs.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::key::{PackageLineageDto, SymbolKeyDto};

/// `skip_serializing_if` predicate for a `bool` that defaults to `false`.
///
/// Matches the "boolean-omission discipline" the rest of the MCP surface
/// already follows for result payloads (§2.10 of `docs/MCP-SURFACE-PLAN.md`):
/// a `false`/default value is never printed, so absence *is* the false case
/// rather than a redundant explicit one.
fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------------------
// Tool arguments
// ---------------------------------------------------------------------------

/// Arguments to `search_symbols`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchSymbolsArgs {
    /// The text to search for: a symbol name, a prefix, or a kind label such
    /// as `trait`. Matching is by name and by kind; it is not full-text over
    /// documentation.
    pub query: String,

    /// Restrict results to these kinds. Valid values are `Module`, `Record`,
    /// `Field`, `Function`, `Alias`, `Trait`, `Impl`, `Enum`, `Variant`,
    /// `Const`, `Static`, `Reexport` and `Param` (case-insensitive). Omit to
    /// search the default scope: every kind above *except* `Field`,
    /// `Variant` and `Param` — those three are members (only meaningful
    /// through the type that owns them) and vastly outnumber the
    /// declarations a reader is usually browsing for. The response's
    /// `excluded_kinds` names what was held back; pass any of those kinds
    /// here explicitly to search them, e.g. `["Param"]` to find where a
    /// parameter is used.
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GetSymbolArgs {
    /// The symbol to open, as `ecosystem:name#introhex` — exactly the `key`
    /// returned by `search_symbols`, `find_usages` or a `graph_query`.
    pub key: SymbolKeyDto,
}

/// Output shape for compact symbol reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolFormat {
    /// Identity and rendered signature only.
    Signature,
    /// Exact declaration text, including a body when the frontend supplied
    /// one — but only when the producer recorded a real file span for this
    /// declaration. Declarations that exist only after macro expansion
    /// (common in std-trait-impl-heavy crates, e.g. serde's `Deserialize`
    /// impls for built-in types) have no such span, so this silently
    /// degrades to the same text `Signature` would give.
    #[default]
    Source,
}

/// Arguments to the batched symbol reader.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GetSymbolsArgs {
    /// Up to 32 keys from search or reference results.
    pub keys: Vec<SymbolKeyDto>,
    /// Select the compact representation once for the whole batch.
    #[serde(default)]
    pub format: SymbolFormat,
}

/// Arguments to `find_usages`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// Arguments to `get_occurrences`.
///
/// The key identifies the symbol that owns the occurrences. Each returned row
/// is one exact reference inside that symbol; use `find_usages` when the
/// question is instead which symbols refer to a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GetOccurrencesArgs {
    /// The symbol whose body owns the occurrences, as `ecosystem:name#introhex`.
    pub key: SymbolKeyDto,

    /// Maximum number of occurrences. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Opaque pagination cursor from a previous `get_occurrences` call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `semantic_search`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SemanticSearchArgs {
    /// A natural-language concept or behavior to find in documented public APIs.
    pub query: String,

    /// Restrict results to these kinds. Omit to search all kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,

    /// Restrict results to these loaded packages. Omit to search every package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<String>>,

    /// Maximum number of results. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Opaque pagination cursor from a previous `semantic_search` call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `list_packages`.
///
/// Empty, but named rather than omitted so the tool still has a derived schema
/// (LR-2) and so adding a filter later is not a breaking change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListPackagesArgs {}

/// Arguments to `list_versions`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListVersionsArgs {
    /// The package lineage to list loaded generations for, as `ecosystem:name`
    /// (for example `cargo:memchr`). Call `list_packages` to see what is
    /// loaded; every package it returns has at least one generation here.
    pub package: PackageLineageDto,
}

/// Arguments to `select_version`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SelectVersionArgs {
    /// The package lineage to switch, as `ecosystem:name`.
    pub package: PackageLineageDto,
    /// The version string to make current, exactly as it appears in
    /// `list_versions`'s output (for example `"2.8.0"`). Selecting a version
    /// that is not loaded is not an error — see `SelectVersionResult::NotLoaded`.
    pub version: String,
}

/// Arguments to `diff_versions`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GraphSchemaArgs {
    /// Return the verbatim `schema.graphql` SDL instead of the compact card.
    /// The card is complete for writing a query and names what it omits;
    /// ask for the SDL only for one of those cases.
    #[serde(default, skip_serializing_if = "is_false")]
    pub full: bool,
}

// ---------------------------------------------------------------------------
// Consolidated tool arguments (docs/MCP-SURFACE-PLAN.md §5.1)
//
// These back the merged tool surface (`search`, `read`, `refs`, `packages`).
// The pre-merge types above (`SearchSymbolsArgs`, `SemanticSearchArgs`,
// `GetSymbolArgs`, `GetSymbolsArgs`, `FindUsagesArgs`, `GetOccurrencesArgs`,
// `ListPackagesArgs`, `ListVersionsArgs`) are kept: they remain the stable
// internal surface `NudoxTools::do_*` is built on (see `tests/mcp_dump_responses.rs`'s
// own doctrine — it is deliberately written against that layer, not the MCP
// tool names, so it stays comparable across a redesign), and the merged
// tools below delegate to them rather than duplicating their logic.
// ---------------------------------------------------------------------------

/// One or more symbol keys/addresses. Deserializes from a JSON array (the
/// documented, schema-visible form) or from a bare string for a single read —
/// the latter is accepted for robustness but not advertised in the schema, so
/// a client reading the schema sees one clear shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct KeysArg(#[schemars(with = "Vec<SymbolKeyDto>")] pub Vec<SymbolKeyDto>);

impl From<Vec<SymbolKeyDto>> for KeysArg {
    fn from(keys: Vec<SymbolKeyDto>) -> Self {
        Self(keys)
    }
}

impl<'de> Deserialize<'de> for KeysArg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(SymbolKeyDto),
            Many(Vec<SymbolKeyDto>),
        }
        Ok(match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(key) => KeysArg(vec![key]),
            OneOrMany::Many(keys) => KeysArg(keys),
        })
    }
}

/// Arguments to `read` (merges `get_symbol` + `get_symbols`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// 1-32 keys or addresses to read, in one round trip.
    pub keys: KeysArg,
    /// Select the compact representation once for the whole batch. Defaults
    /// to `source`.
    #[serde(default)]
    pub format: SymbolFormat,
}

/// Which direction of reference `refs` follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefsDirection {
    /// Symbols that hold a resolved reference to `key` — "who calls this?".
    #[default]
    In,
    /// Exact references owned by `key`'s own declaration body.
    Out,
}

/// Arguments to `refs` (merges `find_usages` + `get_occurrences`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefsArgs {
    /// The symbol to follow references for, as a key or an address.
    pub key: SymbolKeyDto,
    /// `in` (default): symbols referencing `key`. `out`: exact references
    /// owned by `key`'s own body.
    #[serde(default)]
    pub direction: RefsDirection,
    /// Maximum number of rows. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous `refs` call's `next_cursor`.
    /// Omit for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Arguments to `packages` (merges `list_packages` + `list_versions`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackagesArgs {
    /// Narrow to one package's loaded generations, as `ecosystem:name`. Omit
    /// to list every loaded package, each with its loaded versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageLineageDto>,
}

/// Arguments to `index`.
///
/// # Why this supersedes [`IndexPackageArgs`] rather than extending it
///
/// `index_package` takes one `purl`, and a PURL is by construction a registry
/// coordinate. Three things followed from that single field that no additional
/// optional argument could have fixed:
///
/// * a package in a checkout could not be indexed at all — a local root
///   reached the corpus only through `Engine::start_with_producer`, at process
///   start, from `NUDOX_PACKAGE_ROOT`;
/// * indexing two packages meant two round trips, each paying the full
///   bounded-wait deadline;
/// * following a dependency edge was not expressible.
///
/// `IndexPackageArgs` is kept and still works: it remains the single-target
/// registry shape, and `do_index_package` still serves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexArgs {
    /// The packages to index. Each entry is **either**:
    ///
    /// * a package URL — `pkg:<type>/[<namespace>/]<name>@<version>`, with
    ///   `type` one of `cargo`, `npm`, `pypi`, `golang`, `maven`, `nuget`; or
    /// * a filesystem path to a package root — the directory holding
    ///   `Cargo.toml`, `package.json`, `go.mod`, `pom.xml`, `build.gradle`,
    ///   `pyproject.toml`, a `*.csproj`, `CMakeLists.txt` or
    ///   `compile_commands.json`. The language is read from which manifest is
    ///   there, so it does not have to be named.
    ///
    /// A target that fails is reported on its own row; the others still land.
    pub targets: Vec<String>,

    /// Also index the packages these targets declare as dependencies.
    ///
    /// **Off by default, deliberately.** Indexing costs seconds to minutes per
    /// package and a dependency closure is unbounded — a mid-sized npm package
    /// reaches several hundred. Following it on every call would make the
    /// cheap question impossible to ask.
    ///
    /// Dependencies declared by path (a cargo `path = "../lib"`, an npm
    /// `"file:../lib"` or `"workspace:*"`) resolve on this filesystem with no
    /// registry involved, which is what makes this affordable in a monorepo.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dependencies: bool,

    /// How many dependency hops to follow. Defaults to 1; capped at 3.
    ///
    /// Ignored unless `dependencies` is set. One hop is what answers "open the
    /// type this function returns"; the full closure is rarely what was meant
    /// and always what costs most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,

    /// How long to wait, in seconds, before returning with jobs still
    /// running. Defaults to 120, capped at 600.
    ///
    /// The deadline covers the whole batch, not each target. Returning early
    /// is not a failure: the jobs keep running, and calling `index` again with
    /// the same targets attaches to them rather than starting a second set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<u32>,
}

/// Arguments to `index_package`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexPackageArgs {
    /// A package URL: `pkg:<type>/[<namespace>/]<name>@<version>`.
    ///
    /// Types: `cargo`, `npm`, `pypi`, `golang`, `maven`, `nuget`. The version
    /// is required and must be the registry's own spelling — omit it and the
    /// error lists the versions that exist.
    pub purl: String,

    /// How long to wait, in seconds, before returning with the job still
    /// running. Defaults to 120, capped at 600.
    ///
    /// Returning early is not a failure: the job keeps running, and calling
    /// `index_package` again with the same `purl` attaches to it rather than
    /// starting a second one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<u32>,
}
