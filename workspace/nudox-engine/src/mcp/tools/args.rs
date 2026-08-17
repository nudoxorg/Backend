//! Tool argument types, whose MCP schemas derive from these structs.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::key::{PackageLineageDto, SymbolKeyDto};

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

/// Output shape for compact symbol reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolFormat {
    /// Identity and rendered signature only.
    Signature,
    /// Exact declaration text, including a body when the frontend supplied one.
    #[default]
    Source,
}

/// Arguments to the batched symbol reader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetSymbolsArgs {
    /// Up to 32 keys from search or reference results.
    pub keys: Vec<SymbolKeyDto>,
    /// Select the compact representation once for the whole batch.
    #[serde(default)]
    pub format: SymbolFormat,
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

/// Arguments to `get_occurrences`.
///
/// The key identifies the symbol that owns the occurrences. Each returned row
/// is one exact reference inside that symbol; use `find_usages` when the
/// question is instead which symbols refer to a target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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

/// Arguments to `index_package`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
