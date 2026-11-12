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
#[serde(tag = "fact", deny_unknown_fields)]
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
    /// Native pgrust SELECT-column read. OID and attribute are local to the
    /// exact project-proposed catalog; they are not deployed database IDs.
    /// The extent is a native scanner/parser correspondence claim that still
    /// requires verification against the selected host bytes.
    PostgresColumnRead {
        relation_oid: u32,
        attribute: u16,
        extent: SqlByteRange,
    },
    /// Native diagnostic with optional host-relative byte coordinate claim.
    Diagnostic {
        code: SqlDiagnostic,
        extent: Option<SqlByteRange>,
    },
    /// One statement of the file and how completely it was modeled. Later
    /// facts of the same statement follow it in ordinal order.
    Statement {
        class: SqlStatementKind,
        extent: SqlByteRange,
        support: SqlStatementSupport,
    },
    /// A schema object written by a statement (outline claim plus the
    /// owner's ordered-schema identity for it).
    Definition(Box<SqlDefinition>),
    /// A mention of an object by name, with the owner's binding result.
    Reference(Box<SqlReference>),
}
/// Where the selected project's SQL analysis is known to be incomplete. A zero
/// in every field, with a complete inventory, is what lets an empty result mean
/// "none in the declared scope" rather than "unknown".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SqlGapCounts {
    /// Selected files that could not be read, parsed by an admitted worker, or fit limits.
    pub unavailable_hosts: u32,
    /// Selected files the native parser rejected.
    pub native_error_hosts: u32,
    /// Statements parsed but not fully extracted.
    pub partial_statements: u32,
    /// Statements parsed with nothing extracted.
    pub unsupported_statements: u32,
}
/// Statement classes the owner reasons about.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlStatementKind {
    CreateTable,
    CreateView,
    CreateIndex,
    CreateTrigger,
    CreateFunction,
    CreateType,
    CreateSchema,
    CreateSequence,
    AlterTable,
    Drop,
    Select,
    Insert,
    Update,
    Delete,
    Merge,
    Other,
}
/// Why a statement or construct was not fully extracted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlGap {
    OpaqueBody,
    DynamicSql,
    UnmodeledConstruct,
    UnmodeledStatement,
    Limit,
}
/// Extraction completeness of one statement. Absence of a reference in a
/// `Partial` or `Unsupported` statement proves nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlStatementSupport {
    Full,
    Partial(SqlGap),
    Unsupported(SqlGap),
}
/// Kinds of schema object.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum SqlObjectKind {
    Schema,
    Table,
    View,
    Column,
    Index,
    Trigger,
    Function,
    Type,
    Sequence,
}
/// What a definition statement does to its object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlDefinitionAction {
    Create,
    Drop,
    AddColumn,
    DropColumn,
    Rename { to: String, to_extent: SqlByteRange },
    Alter,
}
/// Stable name of one schema object: the fact that created it, as the host
/// path and fact ordinal. Renames keep it; drop and re-create does not.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct SqlEntityRef {
    pub path: String,
    pub ordinal: u32,
}
/// A definition outline claim bound to its entity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SqlDefinition {
    pub kind: SqlObjectKind,
    pub action: SqlDefinitionAction,
    pub conditional: bool,
    pub schema: Option<String>,
    pub name: String,
    pub parent: Option<String>,
    pub parent_schema: Option<String>,
    pub name_extent: SqlByteRange,
    pub extent: SqlByteRange,
    pub detail: Option<String>,
    /// The ordinal of the owning `Statement` fact in this host.
    pub statement: u32,
    /// The entity this definition creates or changes. `None` when the object
    /// being altered or dropped is not defined anywhere in the project.
    pub entity: Option<SqlEntityRef>,
    /// The owning relation entity for a column, when known.
    pub parent_entity: Option<SqlEntityRef>,
    /// Whether the entity exists after the last ordered statement of the
    /// project's declared source order.
    pub live_at_head: bool,
    /// The entity's name after every rename in the declared order (equal to
    /// `name` for an unrenamed object), and its owning relation's name then.
    pub head_name: Option<String>,
    pub head_parent: Option<String>,
}
/// The schema object a statement defines, named for display.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SqlObjectRef {
    pub kind: SqlObjectKind,
    pub name: String,
}
/// Reference classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum SqlRefKind {
    Relation,
    Column,
    Function,
    Type,
    Schema,
    Index,
    Sequence,
    Trigger,
}
/// How a statement uses the named object.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum SqlRefRole {
    Read,
    Write,
    Schema,
}
/// Why a reference stayed unbound.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlUnresolved {
    /// No object of that name is defined by any project source (a builtin,
    /// an extension, or something outside the project).
    NoSuchObject,
    /// The relation is defined but has no live column of that name.
    NoSuchColumn,
    /// The statement made no relation visible to this name.
    NoScope,
    /// A derived table or table function hides its column list.
    OpaqueScope,
    /// The statement was only partly extracted; binding is not attempted.
    PartialStatement,
}
/// The owner's binding of one reference within the project's ordered schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlResolution {
    /// Exactly this entity. `live` is whether it exists at the head of the
    /// declared source order.
    Resolved { target: SqlEntityRef, live: bool },
    /// A star expanding to every column of these relations.
    Expands { targets: Vec<SqlEntityRef> },
    /// The relation is known but the member (a view column, say) is not.
    Relation { relation: SqlEntityRef, live: bool },
    /// More than one candidate; `more` is set when the list was cut.
    Ambiguous { candidates: Vec<SqlEntityRef>, more: bool },
    /// A statement-local name: CTE, alias, derived table, trigger row.
    Local,
    Unresolved { reason: SqlUnresolved },
}
/// A reference outline claim bound by the owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SqlReference {
    pub kind: SqlRefKind,
    pub role: SqlRefRole,
    pub schema: Option<String>,
    pub qualifier: Option<String>,
    pub name: String,
    pub extent: SqlByteRange,
    pub full_extent: SqlByteRange,
    pub statement: u32,
    /// Class of the statement the mention sits in.
    pub statement_kind: SqlStatementKind,
    /// The view, trigger, function, index or table the statement defines, when
    /// it defines one: the object that depends on whatever this names.
    pub in_object: Option<SqlObjectRef>,
    pub resolution: SqlResolution,
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
