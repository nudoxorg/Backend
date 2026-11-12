//! Dialect-neutral syntactic outline of one SQL source file.
//!
//! An outline is a claim by a native parser worker about what a file *writes*:
//! which objects it defines or changes, and which names it mentions, each with
//! the exact byte range of the written spelling. It does not bind a name to an
//! object. Binding (which table does this column belong to, which migration
//! created it) is performed by the owner over the outlines of every file in a
//! project, so that it can cross file boundaries.
//!
//! Every range is a half-open UTF-8 byte range into the request source and is
//! validated against the actual bytes by [`Outline::validate`] before any
//! consumer sees the response.
use crate::Extent;
use serde::{Deserialize, Serialize};

/// Maximum statements in one source file.
pub const MAX_OUTLINE_STATEMENTS: usize = 8192;
/// Maximum definitions, references and scope entries in one file.
pub const MAX_OUTLINE_ITEMS: usize = 60_000;
/// Maximum bytes of a normalized name or a detail string.
pub const MAX_OUTLINE_TEXT: usize = 512;

/// Whether every statement was fully extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutlineCoverage {
    /// Every statement is `StatementSupport::Full`.
    Complete,
    /// At least one statement is partial or unsupported.
    Partial,
}

/// The syntactic outline of one source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    pub coverage: OutlineCoverage,
    /// In source order; extents are strictly increasing and non-overlapping.
    pub statements: Vec<StatementOutline>,
}

/// Closed set of statement classes the owner reasons about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatementKind {
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
    /// A recognized statement the extractor deliberately does not model
    /// (transaction control, PRAGMA, SET, COMMENT, GRANT, ...).
    Other,
}

/// How completely the extractor modeled one statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatementSupport {
    /// Every construct in the statement was extracted.
    Full,
    /// The statement parsed but at least one construct was not extracted;
    /// references may be missing. Consumers must not treat absence as proof.
    Partial(Gap),
    /// Parsed, but nothing was extracted.
    Unsupported(Gap),
}

/// Why extraction is incomplete. Closed so gaps can be counted and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    /// Procedural or foreign-language body (plpgsql, SQL function string body, trigger body not walked).
    OpaqueBody,
    /// Dynamic SQL (`EXECUTE`, string-built identifiers).
    DynamicSql,
    /// A syntax form the extractor does not walk yet.
    UnmodeledConstruct,
    /// Statement class not modeled for this dialect.
    UnmodeledStatement,
    /// Item limits reached while extracting.
    Limit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementOutline {
    pub kind: StatementKind,
    /// The whole statement, from its first token to its last token (trailing `;` excluded).
    pub extent: Extent,
    pub support: StatementSupport,
    pub definitions: Vec<Definition>,
    pub references: Vec<Reference>,
    /// Names visible inside this statement's query bodies (FROM items, CTEs).
    pub scope: Vec<ScopeEntry>,
}

/// Kind of schema object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ObjectKind {
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

/// What the statement does to the defined object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefinitionAction {
    /// CREATE (including `IF NOT EXISTS`, `OR REPLACE`; those are recorded in `conditional`).
    Create,
    /// DROP of the object.
    Drop,
    /// ALTER TABLE ... ADD COLUMN (kind Column, `parent` = table).
    AddColumn,
    /// ALTER TABLE ... DROP COLUMN.
    DropColumn,
    /// ALTER ... RENAME ... TO `to`. The object keeps its identity; `name` is the old name.
    Rename { to: String, to_extent: Extent },
    /// ALTER of an existing object that changes neither name nor existence
    /// (type change, constraint, default). Recorded so impact can see it.
    Alter,
}

/// A schema object written by a statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub kind: ObjectKind,
    pub action: DefinitionAction,
    /// `IF NOT EXISTS`, `IF EXISTS`, `OR REPLACE`, or other execution-dependent forms.
    pub conditional: bool,
    /// Schema qualifier as normalized by the dialect, if written.
    pub schema: Option<String>,
    /// Name normalized for the dialect (SQLite: ASCII lowercase; PostgreSQL: unquoted folds to
    /// lowercase, quoted preserves case). The written spelling is `name_extent`.
    pub name: String,
    /// For Column: the owning table. For Index and Trigger: the indexed/attached table.
    pub parent: Option<String>,
    /// Schema qualifier of `parent`, if written.
    pub parent_schema: Option<String>,
    /// The name token only.
    pub name_extent: Extent,
    /// The whole definition: for a column its column-definition clause, for a table the
    /// whole statement, for a function the whole statement.
    pub extent: Extent,
    /// Column declared type text / function signature text / view or index column list text,
    /// copied verbatim from source (not normalized). Absent when not applicable.
    pub detail: Option<String>,
}

/// What kind of thing a reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RefKind {
    /// A table, view, or CTE name in a relation position.
    Relation,
    /// A column, possibly with a qualifier. `name == "*"` is a star expansion.
    Column,
    Function,
    Type,
    Schema,
    Index,
    Sequence,
    Trigger,
}

/// How the statement uses the named thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RefRole {
    /// Read: SELECT list, WHERE, JOIN ON, GROUP BY, ORDER BY, FROM, RETURNING, subquery.
    Read,
    /// Write target: INSERT INTO t(cols), UPDATE t SET col =, DELETE FROM t, MERGE target.
    Write,
    /// Appears in DDL as a dependency: FK target, index key column, view body source,
    /// CHECK/DEFAULT expression, trigger table, ALTER target, DROP target.
    Schema,
}

/// A mention of an object by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub kind: RefKind,
    pub role: RefRole,
    /// Schema qualifier of the named object, normalized, if written (`s.t`, `s.t.c`).
    pub schema: Option<String>,
    /// For columns: the table or alias written before the column (`t.c`), normalized.
    pub qualifier: Option<String>,
    /// Normalized name (see [`Definition::name`]); `*` for a star.
    pub name: String,
    /// The name token only (for a star, the `*` token).
    pub extent: Extent,
    /// The whole written reference including qualifiers.
    pub full_extent: Extent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeKind {
    /// A table or view named in FROM / JOIN / UPDATE / DELETE / INSERT; `relation` names it.
    Relation,
    /// A common table expression defined by this statement; `relation` is None.
    Cte,
    /// A derived table `(SELECT ...) alias`; its columns are opaque to the owner.
    Subquery,
    /// A table-valued function in FROM.
    Function,
    /// The pseudo-relations `NEW` / `OLD` inside a trigger body.
    TriggerRow,
}

/// One name made visible by a FROM item or CTE, valid inside `region`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeEntry {
    pub kind: ScopeKind,
    /// The name by which columns may be qualified: the alias if present, else the relation name.
    pub visible_as: String,
    /// The named relation when `kind == Relation`.
    pub relation: Option<String>,
    pub relation_schema: Option<String>,
    /// Explicit CTE / derived-table column names when written (`WITH t(a, b) AS`, `AS x(a,b)`).
    pub columns: Option<Vec<String>>,
    /// Source range inside which unqualified or qualified column references may use this entry
    /// (the enclosing query block, including correlated subqueries nested in it).
    pub region: Extent,
}

impl Outline {
    /// Every range this outline claims, for host-byte verification.
    pub fn extents(&self) -> impl Iterator<Item = &Extent> + '_ {
        self.statements.iter().flat_map(|s| {
            std::iter::once(&s.extent)
                .chain(s.definitions.iter().flat_map(|d| {
                    let rename = match &d.action {
                        DefinitionAction::Rename { to_extent, .. } => Some(to_extent),
                        _ => None,
                    };
                    [&d.name_extent, &d.extent].into_iter().chain(rename)
                }))
                .chain(
                    s.references
                        .iter()
                        .flat_map(|r| [&r.extent, &r.full_extent]),
                )
                .chain(s.scope.iter().map(|e| &e.region))
        })
    }

    /// Structural validation against the request source. A response that fails
    /// is rejected as a whole; there is no partial acceptance.
    pub fn validate(&self, source: &[u8]) -> bool {
        let Ok(text) = std::str::from_utf8(source) else {
            return false;
        };
        let in_text = |e: &Extent| e.start < e.end && text.get(e.start..e.end).is_some();
        let contains = |outer: &Extent, inner: &Extent| {
            outer.start <= inner.start && inner.end <= outer.end
        };
        let name_ok = |n: &str| !n.is_empty() && n.len() <= MAX_OUTLINE_TEXT && !n.contains('\0');
        if self.statements.len() > MAX_OUTLINE_STATEMENTS {
            return false;
        }
        let mut items = 0usize;
        let mut previous_end = 0usize;
        let mut all_full = true;
        for statement in &self.statements {
            let e = &statement.extent;
            if !in_text(e) || e.start < previous_end {
                return false;
            }
            previous_end = e.end;
            all_full &= matches!(statement.support, StatementSupport::Full);
            items = items
                .saturating_add(statement.definitions.len())
                .saturating_add(statement.references.len())
                .saturating_add(statement.scope.len());
            if items > MAX_OUTLINE_ITEMS {
                return false;
            }
            for d in &statement.definitions {
                let rename_ok = match &d.action {
                    DefinitionAction::Rename { to, to_extent } => {
                        name_ok(to) && in_text(to_extent) && contains(e, to_extent)
                    }
                    _ => true,
                };
                if !name_ok(&d.name)
                    || !in_text(&d.name_extent)
                    || !in_text(&d.extent)
                    || !contains(e, &d.extent)
                    || !contains(&d.extent, &d.name_extent)
                    || !rename_ok
                    || d.schema.as_deref().is_some_and(|v| !name_ok(v))
                    || d.parent.as_deref().is_some_and(|v| !name_ok(v))
                    || d.parent_schema.as_deref().is_some_and(|v| !name_ok(v))
                    || d.detail
                        .as_deref()
                        .is_some_and(|v| v.len() > MAX_OUTLINE_TEXT || v.contains('\0'))
                    || (matches!(d.kind, ObjectKind::Column) && d.parent.is_none())
                {
                    return false;
                }
            }
            for r in &statement.references {
                if !name_ok(&r.name)
                    || !in_text(&r.extent)
                    || !in_text(&r.full_extent)
                    || !contains(e, &r.full_extent)
                    || !contains(&r.full_extent, &r.extent)
                    || r.schema.as_deref().is_some_and(|v| !name_ok(v))
                    || r.qualifier.as_deref().is_some_and(|v| !name_ok(v))
                    || (r.name == "*" && (r.kind != RefKind::Column || &text[r.extent.start..r.extent.end] != "*"))
                {
                    return false;
                }
            }
            for s in &statement.scope {
                if !name_ok(&s.visible_as)
                    || !in_text(&s.region)
                    || !contains(e, &s.region)
                    || s.relation.as_deref().is_some_and(|v| !name_ok(v))
                    || s.relation_schema.as_deref().is_some_and(|v| !name_ok(v))
                    || s.columns.as_ref().is_some_and(|c| {
                        c.len() > 512 || c.iter().any(|n| !name_ok(n))
                    })
                    || (s.kind == ScopeKind::Relation) != s.relation.is_some()
                {
                    return false;
                }
            }
        }
        (self.coverage == OutlineCoverage::Complete) == all_full
    }
}
