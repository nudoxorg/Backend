//! Native SQL source facts. Dialect and context remain part of every claim.
//! These values are not database execution, global catalog identities, or
//! proof of authored locations without separate selected-source admission.
use serde::{Deserialize, Serialize};

/// Explicit native dialect; SQL file extensions do not select one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlDialect {
    Sqlite,
    Postgres,
}
/// A bounded capability terminal, independent of enumeration completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlCoverage {
    Partial,
    Unavailable,
}
/// Native syntaqlite relation class under its exact proposed context.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqliteRelationKind {
    Table,
    View,
}
/// Native model lineage status; this is not completeness of source semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqliteLineageCoverage {
    Unavailable,
    NativeComplete,
    NativePartial,
}
/// A checked-offset claim; actual host byte verification is still mandatory.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlByteRange {
    pub start: u32,
    pub end: u32,
}
/// One SQLite analyzer result. Names are scoped to the selected context;
/// native lineage does not establish an exact source token or a global object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum SqlFact {
    /// Native FROM/JOIN relation, without an authored-occurrence claim.
    SqliteRelation {
        name: String,
        relation_kind: SqliteRelationKind,
    },
    /// Native output-column origin, not all referenced expressions.
    SqliteOutputLineage {
        output_name: String,
        ordinal: u32,
        origin: Option<SqliteColumnOrigin>,
    },
    /// Native diagnostic with optional host-relative byte coordinate claim.
    Diagnostic {
        code: SqlDiagnostic,
        extent: Option<SqlByteRange>,
    },
}
/// A native result-column origin in the exact proposed catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteColumnOrigin {
    pub relation: String,
    pub column: String,
}
/// Closed diagnostic categories; Other preserves an explicit native gap.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlDiagnostic {
    UnknownColumn,
    Other,
}
