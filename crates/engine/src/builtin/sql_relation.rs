//! Canonical SQL analysis rows. Rows are claims until owner admission verifies
//! source/config bytes, selected roots, context and complete host terminals.
use crate::workspace::{WorkspacePreflightRelationHandle, WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_semantic::sql_analysis::{SqlCoverage, SqlDialect, SqlFact, SqliteLineageCoverage};
use backend_store::{StoreError, TypedObject};
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use serde::{Deserialize, Serialize};

#[cfg(test)]
thread_local! { static SQL_DECODE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

const MAGIC: &[u8; 4] = b"SQL1";
const ROOT_KEY: [u8; 32] = [0x53; 32];
/// Maximum encoded bytes for one SQL row.
pub const MAX_SQL_ROW_BYTES: usize = 32 * 1024;
/// Maximum facts one SQL host may own.
pub const MAX_SQL_FACTS_PER_HOST: u32 = 65_536;
/// Maximum selected SQL hosts per project.
pub const MAX_SQL_HOSTS: u32 = 256;
/// Dedicated SQL native-analysis relation (0x97/16).
pub struct SqlAnalysisRelation;
/// Typed selected root pointer for SQL analysis (0x97/17).
pub struct SqlAnalysisRootSchema;
impl Schema for SqlAnalysisRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 17;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}
/// Whether the selected source inventory was fully witnessed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlInventory {
    Complete,
    Incomplete,
}
/// A failed native attempt is never an empty successful result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlUnavailable {
    Read,
    /// Project selection or same-walk evidence was unavailable.
    SelectionUnavailable,
    InvalidUtf8,
    Limit,
    EngineNotAdmitted,
    Deadline,
    /// The caller cancelled this native attempt.
    Cancelled,
    WorkerFailure,
    UnsupportedDialect,
    Context,
}
/// Per-host native parse outcome; modeled capabilities remain partial or unknown.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlHostTerminal {
    Parsed {
        source: SqlCoverage,
        contextual: SqlCoverage,
        sqlite_lineage: Option<SqliteLineageCoverage>,
    },
    NativeError,
    Unavailable(SqlUnavailable),
}
/// Latest attempt and retained selection are distinct.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlAttemptOutcome {
    Observed,
    Unavailable(SqlUnavailable),
}
/// One SQL source/analysis row. Every digest is a claim until owner admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum SqlAnalysisRecord {
    /// Project selection bound to actual configuration bytes and the confined
    /// filesystem root used for capture. The owner's predecessor snapshot is
    /// a separate witness; this path is not a workspace Merkle root.
    Selection {
        project: [u8; 32],
        source_root: String,
        config_input: [u8; 32],
        config_source: [u8; 32],
        config_length: u32,
        discovery: Option<[u8; 32]>,
        policy_complete: bool,
        /// No settings file existed: every `.sql` file of the bounded walk was
        /// selected and each dialect was inferred from the file.
        implicit: bool,
        /// Where the analysis of this selection is known to be incomplete.
        gaps: backend_semantic::sql_analysis::SqlGapCounts,
        inventory: SqlInventory,
        inventory_digest: [u8; 32],
        host_count: u32,
    },
    /// Latest attempted selection, with the exact retained selection digest if any.
    Attempt {
        project: [u8; 32],
        outcome: SqlAttemptOutcome,
        served_selection: Option<[u8; 32]>,
    },
    /// One explicitly configured SQL file and its native attempt terminal.
    Host {
        project: [u8; 32],
        path: String,
        dialect: SqlDialect,
        input: Option<[u8; 32]>,
        source: Option<[u8; 32]>,
        length: Option<u64>,
        producer: Option<[u8; 32]>,
        settings: Option<[u8; 32]>,
        context: Option<[u8; 32]>,
        terminal: SqlHostTerminal,
        fact_count: u32,
        fact_digest: [u8; 32],
    },
    /// One bounded native model claim under an exact admitted host basis.
    Fact {
        host: [u8; 32],
        ordinal: u32,
        fact: SqlFact,
    },
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        && !path.contains('\\')
        && !path.bytes().any(|b| b.is_ascii_control())
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.contains('\0')
}
fn valid_entity(entity: &backend_semantic::sql_analysis::SqlEntityRef) -> bool {
    valid_path(&entity.path) && entity.ordinal < MAX_SQL_FACTS_PER_HOST
}
fn valid_range(range: &backend_semantic::sql_analysis::SqlByteRange) -> bool {
    range.start < range.end
}
fn valid_definition(definition: &backend_semantic::sql_analysis::SqlDefinition) -> bool {
    use backend_semantic::sql_analysis::SqlDefinitionAction;
    valid_name(&definition.name)
        && definition.schema.as_deref().is_none_or(valid_name)
        && definition.parent.as_deref().is_none_or(valid_name)
        && definition.parent_schema.as_deref().is_none_or(valid_name)
        && definition.detail.as_deref().is_none_or(|v| v.len() <= 512 && !v.contains('\0'))
        && valid_range(&definition.name_extent)
        && valid_range(&definition.extent)
        && definition.statement < MAX_SQL_FACTS_PER_HOST
        && definition.entity.as_ref().is_none_or(valid_entity)
        && definition.parent_entity.as_ref().is_none_or(valid_entity)
        && definition.head_name.as_deref().is_none_or(valid_name)
        && definition.head_parent.as_deref().is_none_or(valid_name)
        && match &definition.action {
            SqlDefinitionAction::Rename { to, to_extent } => valid_name(to) && valid_range(to_extent),
            _ => true,
        }
}
fn valid_reference(reference: &backend_semantic::sql_analysis::SqlReference) -> bool {
    use backend_semantic::sql_analysis::SqlResolution;
    valid_name(&reference.name)
        && reference.schema.as_deref().is_none_or(valid_name)
        && reference.qualifier.as_deref().is_none_or(valid_name)
        && valid_range(&reference.extent)
        && valid_range(&reference.full_extent)
        && reference.statement < MAX_SQL_FACTS_PER_HOST
        && reference.in_object.as_ref().is_none_or(|object| valid_name(&object.name))
        && match &reference.resolution {
            SqlResolution::Resolved { target, .. } => valid_entity(target),
            SqlResolution::Relation { relation, .. } => valid_entity(relation),
            SqlResolution::Expands { targets } => targets.len() <= 64 && targets.iter().all(valid_entity),
            SqlResolution::Ambiguous { candidates, .. } => {
                (2..=16).contains(&candidates.len()) && candidates.iter().all(valid_entity)
            }
            SqlResolution::Local | SqlResolution::Unresolved { .. } => true,
        }
}
fn outline_host_ok(terminal: &SqlHostTerminal) -> bool {
    matches!(terminal, SqlHostTerminal::Parsed { source: SqlCoverage::Partial, .. })
}
impl SqlAnalysisRecord {
    /// Validate local shape; this never proves project membership or producer authority.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_shape()?;
        super::stylo_projection::bounded_encode(self, MAX_SQL_ROW_BYTES - MAGIC.len())?;
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), &'static str> {
        match self {
            Self::Selection {
                project,
                source_root,
                config_input,
                config_source,
                config_length,
                host_count,
                ..
            } => {
                if *project == [0; 32]
                    || *config_input == [0; 32]
                    || *config_source == [0; 32]
                    || *config_length > 128 * 1024
                    || *host_count > MAX_SQL_HOSTS
                    || source_root.is_empty()
                    || source_root.len() > 4096
                    || source_root.contains('\0')
                {
                    return Err("invalid SQL selection");
                }
            }
            Self::Attempt { project, .. } if *project == [0; 32] => {
                return Err("invalid SQL attempt");
            }
            Self::Host {
                project,
                path,
                dialect,
                input,
                source,
                length,
                producer,
                settings,
                context,
                terminal,
                fact_count,
                ..
            } => {
                if *project == [0; 32] || !valid_path(path) || *fact_count > MAX_SQL_FACTS_PER_HOST {
                    return Err("invalid SQL host");
                }
                if [input, source, producer, settings, context]
                    .into_iter()
                    .any(|value| value.is_some_and(|digest| digest == [0; 32]))
                    || length.is_some_and(|value| value > 512 * 1024)
                {
                    return Err("invalid SQL host identity or extent");
                }
                if matches!(terminal, SqlHostTerminal::Parsed { .. })
                    && (input.is_none()
                        || source.is_none()
                        || length.is_none()
                        || producer.is_none()
                        || settings.is_none())
                {
                    return Err("parsed SQL host missing observed basis");
                }
                if matches!(terminal, SqlHostTerminal::Unavailable(_)) && *fact_count != 0 {
                    return Err("unavailable SQL host has facts");
                }
                if let SqlHostTerminal::Parsed {
                    contextual,
                    sqlite_lineage,
                    ..
                } = terminal
                {
                    if *contextual == SqlCoverage::Partial && context.is_none() {
                        return Err("contextual SQL host missing context");
                    }
                    if *dialect != SqlDialect::Sqlite && sqlite_lineage.is_some() {
                        return Err("SQLite lineage on another dialect");
                    }
                    if matches!(
                        sqlite_lineage,
                        Some(
                            SqliteLineageCoverage::NativeComplete
                                | SqliteLineageCoverage::NativePartial
                        )
                    ) && (*dialect != SqlDialect::Sqlite
                        || context.is_none()
                        || *contextual != SqlCoverage::Partial)
                    {
                        return Err("native SQLite lineage without contextual analysis");
                    }
                }
            }
            Self::Fact {
                host,
                ordinal,
                fact,
            } => {
                if *host == [0; 32] || *ordinal >= MAX_SQL_FACTS_PER_HOST {
                    return Err("invalid SQL fact host");
                }
                match fact {
                    SqlFact::SqliteRelation { name, .. } if !valid_name(name) => {
                        return Err("invalid SQL relation name");
                    }
                    SqlFact::SqliteOutputLineage {
                        output_name,
                        origin,
                        ..
                    } if !valid_name(output_name)
                        || origin.as_ref().is_some_and(|v| {
                            !valid_name(&v.relation) || !valid_name(&v.column)
                        }) =>
                    {
                        return Err("invalid SQL lineage");
                    }
                    SqlFact::Diagnostic {
                        extent: Some(range),
                        ..
                    } if range.start > range.end => {
                        return Err("invalid SQL diagnostic extent");
                    }
                    SqlFact::PostgresColumnRead {
                        relation_oid,
                        attribute,
                        extent,
                    } if *relation_oid == 0 || *attribute == 0 || extent.start >= extent.end => {
                        return Err("invalid PostgreSQL read claim");
                    }
                    SqlFact::Statement { extent, .. } if extent.start >= extent.end => {
                        return Err("invalid SQL statement extent");
                    }
                    SqlFact::Definition(definition) if !valid_definition(definition) => {
                        return Err("invalid SQL definition");
                    }
                    SqlFact::Reference(reference) if !valid_reference(reference) => {
                        return Err("invalid SQL reference");
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// Compare serialized chunks directly with the admitted bytes. This preserves
// canonical JSON checking without allocating a second copy of the row.
struct CanonicalJsonCompare<'a> {
    remaining: &'a [u8],
    mismatch: bool,
}
impl std::io::Write for CanonicalJsonCompare<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.remaining.starts_with(bytes) {
            self.remaining = &self.remaining[bytes.len()..];
        } else {
            self.mismatch = true;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Relation for SqlAnalysisRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 16;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = SqlAnalysisRecord;
    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        if out.is_empty() {
            let bytes: Box<[u8]> = Box::new(*value);
            *out = Vec::from(bytes);
        } else {
            out.extend_from_slice(value);
        }
    }
    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&serde_json::to_vec(value).expect("serializable SQL row"));
    }
}
impl CanonicalRelation for SqlAnalysisRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        #[cfg(test)]
        SQL_DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));
        if bytes.len() > MAX_SQL_ROW_BYTES || !bytes.starts_with(MAGIC) {
            return Err(RelationDecodeError::Malformed);
        }
        // Escaped strings use an explicitly sized caller-owned scratch buffer.
        // A decoded JSON string cannot be longer than its encoded source.
        let input = &bytes[MAGIC.len()..];
        let mut scratch = Box::new([0u8; MAX_SQL_ROW_BYTES]);
        let (value, consumed) = serde_json_core::from_slice_escaped::<super::sql_decode::DirectRow>(input, &mut scratch[..])
            .map_err(|_| RelationDecodeError::Malformed)?;
        if consumed != input.len() {
            return Err(RelationDecodeError::Malformed);
        }
        let value = value.0;
        value
            .validate_shape()
            .map_err(|_| RelationDecodeError::Malformed)?;
        if !Self::canonical_value_matches(&value, bytes) {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(value)
    }
    fn canonical_value_matches(value: &Self::Value, bytes: &[u8]) -> bool {
        let Some(json) = bytes.strip_prefix(MAGIC) else { return false; };
        let mut canonical = CanonicalJsonCompare { remaining: json, mismatch: false };
        serde_json::to_writer(&mut canonical, value).is_ok()
            && !canonical.mismatch && canonical.remaining.is_empty()
    }
}
/// Shape-derived upper bounds for one decoded SQL row's heap payload.
///
/// Inline row storage, raw node bytes, allocator bookkeeping and outer page
/// containers are separate charges. The decoder's unescape buffer is live
/// only during decoding; the owned payload can remain live afterward.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SqlRowDecodeBound {
    /// Sum of maximum requested capacities of the returned row's allocations.
    pub owned_payload_bytes: usize,
    /// Maximum number of allocations owned by the row.
    pub owned_allocations: usize,
    /// Requested bytes in the transient unescape buffer.
    pub scratch_payload_bytes: usize,
    /// Maximum simultaneous allocations in the unescape buffer.
    pub scratch_allocations: usize,
}

/// Compute a SQL row's shape bound before decoding it.
///
/// Strings occupy at most the input's JSON bytes in aggregate. The only
/// variable vector is allocated with capacity 64; at most one definition or
/// reference box exists. A reference has at most 64 entity paths plus name,
/// schema, qualifier and enclosing object name (68 strings), one vector and
/// one box. The decoder uses one fixed-size unescape scratch allocation.
///
/// `None` means the envelope cannot be a SQL row or arithmetic overflowed.
#[must_use]
pub fn sql_row_decode_bound(encoded_len: usize) -> Option<SqlRowDecodeBound> {
    use backend_semantic::sql_analysis::{SqlDefinition, SqlEntityRef, SqlReference};
    let json_len = encoded_len.checked_sub(MAGIC.len())?;
    if encoded_len > MAX_SQL_ROW_BYTES { return None; }
    let vector_bytes = 64usize.checked_mul(std::mem::size_of::<SqlEntityRef>())?;
    let boxed_bytes = std::mem::size_of::<SqlDefinition>().max(std::mem::size_of::<SqlReference>());
    Some(SqlRowDecodeBound {
        owned_payload_bytes: json_len.checked_add(vector_bytes)?.checked_add(boxed_bytes)?,
        owned_allocations: 70,
        scratch_payload_bytes: MAX_SQL_ROW_BYTES,
        scratch_allocations: 1,
    })
}

/// Bound one-at-a-time SQL decoding across a borrowed leaf's values.
///
/// The caller must keep this reservation while a decoded row is in flight,
/// transfer its retained charge before reusing it, and separately charge raw
/// nodes, typed keys, path state, containers and allocator bookkeeping. This
/// function does not authenticate the leaf or validate the values.
#[must_use]
pub fn sql_leaf_sequential_decode_bound<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Option<SqlRowDecodeBound> {
    let mut maximum = SqlRowDecodeBound {
        owned_payload_bytes: 0, owned_allocations: 0,
        scratch_payload_bytes: 0, scratch_allocations: 0,
    };
    for value in values {
        let bound = sql_row_decode_bound(value.len())?;
        maximum.owned_payload_bytes = maximum.owned_payload_bytes.max(bound.owned_payload_bytes);
        maximum.owned_allocations = maximum.owned_allocations.max(bound.owned_allocations);
        maximum.scratch_payload_bytes = maximum.scratch_payload_bytes.max(bound.scratch_payload_bytes);
        maximum.scratch_allocations = maximum.scratch_allocations.max(bound.scratch_allocations);
    }
    Some(maximum)
}

/// Failure while reserving or decoding one SQL row's requested heap payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlRowBudgetError {
    /// The caller's payload budget is too small for the conservative peak.
    Exhausted,
    /// The input is not a canonical SQL row.
    Malformed,
}

/// Decode one SQL row after reserving its shape-derived peak payload.
///
/// On success, the budget remains charged for the row's actual owned payload;
/// scratch and unused reserve are refunded. On failure, it is unchanged.
/// This covers requested payload bytes only: callers must independently
/// account for allocator bookkeeping, inline storage, raw nodes and containers.
/// This function does not itself authorize a relation node or source fact.
pub fn sql_decode_row_with_payload_budget(
    bytes: &[u8], budget: &mut usize,
) -> Result<SqlAnalysisRecord, SqlRowBudgetError> {
    let bound = sql_row_decode_bound(bytes.len()).ok_or(SqlRowBudgetError::Malformed)?;
    let reserve = bound.owned_payload_bytes.checked_add(bound.scratch_payload_bytes)
        .ok_or(SqlRowBudgetError::Exhausted)?;
    let original = *budget;
    let remaining = budget.checked_sub(reserve).ok_or(SqlRowBudgetError::Exhausted)?;
    *budget = remaining;
    let result = (|| {
        let value = SqlAnalysisRelation::decode_value(bytes).map_err(|_| SqlRowBudgetError::Malformed)?;
        let owned = sql_row_owned_payload_bytes(&value).ok_or(SqlRowBudgetError::Exhausted)?;
        if owned > bound.owned_payload_bytes { return Err(SqlRowBudgetError::Exhausted); }
        Ok((value, owned))
    })();
    match result {
        Ok((value, owned)) => { *budget = original - owned; Ok(value) }
        Err(error) => { *budget = original; Err(error) }
    }
}

/// Sum the capacities of heap allocations owned by an already decoded SQL row.
///
/// This counts requested allocation payloads only. It excludes the inline row,
/// allocator metadata, parser scratch and any copies. It is not a preflight
/// bound and must not be used alone to authorize a budgeted read.
#[must_use]
pub fn sql_row_owned_payload_bytes(row: &SqlAnalysisRecord) -> Option<usize> {
    use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlEntityRef, SqlResolution};
    fn add(total: &mut usize, bytes: usize) -> Option<()> {
        *total = total.checked_add(bytes)?;
        Some(())
    }
    fn string(total: &mut usize, value: &Option<String>) -> Option<()> {
        add(total, value.as_ref().map_or(0, String::capacity))
    }
    fn entity(total: &mut usize, value: &SqlEntityRef) -> Option<()> {
        add(total, value.path.capacity())
    }
    fn optional_entity(total: &mut usize, value: &Option<SqlEntityRef>) -> Option<()> {
        if let Some(value) = value { entity(total, value)?; }
        Some(())
    }
    let mut total = 0usize;
    match row {
        SqlAnalysisRecord::Selection { source_root, .. } => add(&mut total, source_root.capacity())?,
        SqlAnalysisRecord::Host { path, .. } => add(&mut total, path.capacity())?,
        SqlAnalysisRecord::Attempt { .. } => {},
        SqlAnalysisRecord::Fact { fact, .. } => match fact {
            SqlFact::SqliteRelation { name, .. } => add(&mut total, name.capacity())?,
            SqlFact::SqliteOutputLineage { output_name, origin, .. } => {
                add(&mut total, output_name.capacity())?;
                if let Some(origin) = origin {
                    add(&mut total, origin.relation.capacity())?;
                    add(&mut total, origin.column.capacity())?;
                }
            }
            SqlFact::Definition(value) => {
                add(&mut total, std::mem::size_of_val(value.as_ref()))?;
                add(&mut total, value.name.capacity())?;
                for name in [&value.schema, &value.parent, &value.parent_schema, &value.detail, &value.head_name, &value.head_parent] {
                    string(&mut total, name)?;
                }
                if let SqlDefinitionAction::Rename { to, .. } = &value.action { add(&mut total, to.capacity())?; }
                optional_entity(&mut total, &value.entity)?;
                optional_entity(&mut total, &value.parent_entity)?;
            }
            SqlFact::Reference(value) => {
                add(&mut total, std::mem::size_of_val(value.as_ref()))?;
                add(&mut total, value.name.capacity())?;
                string(&mut total, &value.schema)?;
                string(&mut total, &value.qualifier)?;
                if let Some(object) = &value.in_object { add(&mut total, object.name.capacity())?; }
                match &value.resolution {
                    SqlResolution::Resolved { target, .. } => entity(&mut total, target)?,
                    SqlResolution::Relation { relation, .. } => entity(&mut total, relation)?,
                    SqlResolution::Expands { targets } | SqlResolution::Ambiguous { candidates: targets, .. } => {
                        add(&mut total, targets.capacity().checked_mul(std::mem::size_of::<SqlEntityRef>())?)?;
                        for target in targets { entity(&mut total, target)?; }
                    }
                    SqlResolution::Local | SqlResolution::Unresolved { .. } => {},
                }
            }
            SqlFact::PostgresColumnRead { .. } | SqlFact::Diagnostic { .. } | SqlFact::Statement { .. } => {},
        }
    }
    Some(total)
}
/// Count the live allocation payloads owned by a decoded SQL row.
///
/// Together with [`sql_row_owned_payload_bytes`], this lets a caller apply
/// its own allocator-bookkeeping policy. It excludes inline and caller-owned
/// containers and does not itself reserve memory.
#[must_use]
pub fn sql_row_owned_allocation_count(row: &SqlAnalysisRecord) -> Option<usize> {
    use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlEntityRef, SqlResolution};
    fn present(value: &String) -> usize { usize::from(value.capacity() != 0) }
    fn optional(value: &Option<String>) -> usize { value.as_ref().map_or(0, present) }
    fn entity(value: &SqlEntityRef) -> usize { present(&value.path) }
    let mut count = 0usize;
    match row {
        SqlAnalysisRecord::Selection { source_root, .. } => count += present(source_root),
        SqlAnalysisRecord::Host { path, .. } => count += present(path),
        SqlAnalysisRecord::Attempt { .. } => {},
        SqlAnalysisRecord::Fact { fact, .. } => match fact {
            SqlFact::SqliteRelation { name, .. } => count += present(name),
            SqlFact::SqliteOutputLineage { output_name, origin, .. } => {
                count += present(output_name);
                if let Some(origin) = origin { count += present(&origin.relation) + present(&origin.column); }
            }
            SqlFact::Definition(value) => {
                count += 1 + present(&value.name);
                for name in [&value.schema, &value.parent, &value.parent_schema, &value.detail, &value.head_name, &value.head_parent] {
                    count += optional(name);
                }
                if let SqlDefinitionAction::Rename { to, .. } = &value.action { count += present(to); }
                if let Some(value) = &value.entity { count += entity(value); }
                if let Some(value) = &value.parent_entity { count += entity(value); }
            }
            SqlFact::Reference(value) => {
                count += 1 + present(&value.name) + optional(&value.schema) + optional(&value.qualifier);
                if let Some(object) = &value.in_object { count += present(&object.name); }
                match &value.resolution {
                    SqlResolution::Resolved { target, .. } => count += entity(target),
                    SqlResolution::Relation { relation, .. } => count += entity(relation),
                    SqlResolution::Expands { targets } | SqlResolution::Ambiguous { candidates: targets, .. } => {
                        count += usize::from(targets.capacity() != 0);
                        for target in targets { count = count.checked_add(entity(target))?; }
                    }
                    SqlResolution::Local | SqlResolution::Unresolved { .. } => {},
                }
            }
            SqlFact::PostgresColumnRead { .. } | SqlFact::Diagnostic { .. } | SqlFact::Statement { .. } => {},
        }
    }
    Some(count)
}

fn owner_key(kind: &[u8], project: &[u8; 32], suffix: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"nudox.sql-analysis.v1\0");
    h.update(&(kind.len() as u64).to_be_bytes());
    h.update(kind);
    h.update(project);
    h.update(&(suffix.len() as u64).to_be_bytes());
    h.update(suffix);
    *h.finalize().as_bytes()
}
/// Deterministic key for the project SQL selection.
pub fn sql_selection_key(project: [u8; 32]) -> [u8; 32] {
    owner_key(b"selection", &project, b"")
}
/// Deterministic key for the project's latest SQL attempt.
pub fn sql_attempt_key(project: [u8; 32]) -> [u8; 32] {
    owner_key(b"attempt", &project, b"")
}
/// Derive the host key from a canonical project-relative path.
pub fn sql_host_key(project: [u8; 32], path: &str) -> Result<[u8; 32], String> {
    if project == [0; 32] || !valid_path(path) {
        return Err("invalid SQL host path".into());
    }
    Ok(owner_key(b"host", &project, path.as_bytes()))
}
/// Bind a native fact to exact host, source, producer, settings, context and ordinal.
pub fn sql_fact_key(
    host: [u8; 32],
    dialect: SqlDialect,
    input: [u8; 32],
    source: [u8; 32],
    producer: [u8; 32],
    settings: [u8; 32],
    context: Option<[u8; 32]>,
    ordinal: u32,
) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"nudox.sql-analysis.v1.fact\0");
    h.update(&host);
    h.update(&[match dialect {
        SqlDialect::Sqlite => 1,
        SqlDialect::Postgres => 2,
    }]);
    h.update(&input);
    h.update(&source);
    h.update(&producer);
    h.update(&settings);
    match context {
        Some(value) => {
            h.update(&[1]);
            h.update(&value);
        }
        None => {
            h.update(&[0]);
        }
    }
    h.update(&ordinal.to_be_bytes());
    *h.finalize().as_bytes()
}
/// Key of the typed selected SQL root pointer.
pub fn sql_analysis_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<SqlAnalysisRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
/// Construct a typed pointer for a separately admitted SQL root.
pub fn sql_analysis_root_object(root: StateRoot<SqlAnalysisRelation>) -> TypedObject {
    let key = ObjectKey::<SqlAnalysisRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Open the selected SQL relation; absence is unknown, never known empty.
pub fn sql_analysis_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<SqlAnalysisRelation>>, WorkspaceRelationError> {
    let key = ObjectKey::<SqlAnalysisRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<SqlAnalysisRelation>(
        SchemaIdentity::new(
            SqlAnalysisRootSchema::DOMAIN,
            SqlAnalysisRootSchema::TYPE,
            SqlAnalysisRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

/// Select an auxiliary SQL pointer without decoding its root. Every node read
/// through the returned handle must use a SQL preflight gate.
pub fn sql_analysis_preflight_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspacePreflightRelationHandle<SqlAnalysisRelation>>, WorkspaceRelationError> {
    let key = ObjectKey::<SqlAnalysisRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation_preflight::<SqlAnalysisRelation>(
        SchemaIdentity::new(SqlAnalysisRootSchema::DOMAIN, SqlAnalysisRootSchema::TYPE, SqlAnalysisRootSchema::VERSION),
        *key.as_bytes(),
    )
}

/// Reservations for sequential SQL row decoding in an authenticated node.
///
/// The caller supplies the per-allocation bookkeeping charge required by its
/// allocator policy; zero means only requested allocation payloads are metered.
/// The workspace streaming wrapper separately reserves node envelope and path
/// storage. Retained rows, maps, digests and all other caller allocations must
/// be charged separately before retention. Gate reservations must stay held
/// until the streaming operation has returned and all temporary nodes drop.
#[derive(Debug)]
struct SqlNodeDecodeReservations {
    allocation_bookkeeping_bytes: usize,
    held: usize,
}
impl SqlNodeDecodeReservations {
    /// Create an initially empty gate ledger.
    #[must_use]
    const fn new(allocation_bookkeeping_bytes: usize) -> Self {
        Self { allocation_bookkeeping_bytes, held: 0 }
    }

    /// Precharge one authenticated wire node before any SQL value decoding.
    /// Every leaf value is considered, including values the caller will skip.
    /// The borrowed wire fields are claims, not source authority.
    fn preflight(
        &mut self, node: &backend_version::CanonicalNodeView<'_>, budget: &mut usize,
    ) -> Result<(), StoreError> {
        let mut row_peak = 0usize;
        if node.is_leaf() {
            for field in node.leaf_field_iter().map_err(|_| StoreError::Corrupt)? {
                let (key, value) = field.map_err(|_| StoreError::Corrupt)?;
                if key.len() != 32 || !value.starts_with(MAGIC) { return Err(StoreError::Corrupt); }
                let bound = sql_row_decode_bound(value.len()).ok_or(StoreError::Corrupt)?;
                let allocations = bound.owned_allocations.checked_add(bound.scratch_allocations).ok_or(StoreError::Bounds)?;
                let payload = bound.owned_payload_bytes.checked_add(bound.scratch_payload_bytes).ok_or(StoreError::Bounds)?;
                let cost = allocations.checked_mul(self.allocation_bookkeeping_bytes)
                    .and_then(|n| n.checked_add(payload)).ok_or(StoreError::Bounds)?;
                row_peak = row_peak.max(cost);
            }
        } else {
            for child in node.branch_children().map_err(|_| StoreError::Corrupt)? {
                let child = child.map_err(|_| StoreError::Corrupt)?;
                if child.first_key.len() != 32 { return Err(StoreError::Corrupt); }
            }
        }
        // Canonical key re-encoding uses one exact 32-byte allocation; it is
        // sequential to value decoding but charged separately for clarity.
        let key_peak = 32usize.checked_add(self.allocation_bookkeeping_bytes).ok_or(StoreError::Bounds)?;
        // The checked node may coexist with its retained root clone while a
        // subsequent node is opened. Charge two exact wire-length payloads
        // for every gated node; charges stay held until the operation ends.
        let node_copies = node.encoded_len().checked_mul(2)
            .and_then(|n| self.allocation_bookkeeping_bytes.checked_mul(2).and_then(|h| n.checked_add(h)))
            .ok_or(StoreError::Bounds)?;
        let charge = row_peak.max(key_peak).checked_add(node_copies).ok_or(StoreError::Bounds)?;
        let held = self.held.checked_add(charge).ok_or(StoreError::Bounds)?;
        let remaining = budget.checked_sub(charge).ok_or(StoreError::Bounds)?;
        self.held = held;
        *budget = remaining;
        Ok(())
    }

    /// Refund temporary node decode reservations after the operation ends.
    fn release(&mut self, budget: &mut usize) -> Result<(), StoreError> {
        *budget = budget.checked_add(self.held).ok_or(StoreError::Bounds)?;
        self.held = 0;
        Ok(())
    }
}

/// Visit SQL rows through authenticated, preflighted nodes without constructing
/// a sibling or result vector. The visitor must charge retained row payload,
/// allocation bookkeeping and its containers before storing each row. On any
/// error the caller must discard all results from this operation.
///
/// A returned key is the last row delivered. The caller must continue until a
/// subsequent visit returns `None` to establish exhaustion.
pub fn sql_visit_preflighted<V>(
    relation: &WorkspacePreflightRelationHandle<SqlAnalysisRelation>,
    after: Option<&[u8; 32]>, limit: usize, budget: &mut usize,
    allocation_bookkeeping_bytes: usize, visit: &mut V,
) -> Result<Option<[u8; 32]>, WorkspaceRelationError>
where V: FnMut(&[u8; 32], SqlAnalysisRecord, &mut usize) -> Result<(), StoreError> {
    let mut reservations = SqlNodeDecodeReservations::new(allocation_bookkeeping_bytes);
    let result = relation.visit(after, limit, 32, budget, &mut |node, budget| reservations.preflight(node, budget), visit);
    reservations.release(budget).map_err(WorkspaceRelationError::Store)?;
    result
}

/// Look up one SQL row with authenticated preflight and charge its actual
/// retained requested payload and allocation bookkeeping before returning it.
/// Inline result and any caller-owned container are separate charges.
pub fn sql_lookup_preflighted(
    relation: &WorkspacePreflightRelationHandle<SqlAnalysisRelation>,
    key: &[u8; 32], budget: &mut usize, allocation_bookkeeping_bytes: usize,
) -> Result<Option<SqlAnalysisRecord>, WorkspaceRelationError> {
    let mut reservations = SqlNodeDecodeReservations::new(allocation_bookkeeping_bytes);
    let result = relation.lookup(key, 32, budget, &mut |node, budget| reservations.preflight(node, budget));
    let result = result.and_then(|row| {
        if let Some(value) = &row {
            let payload = sql_row_owned_payload_bytes(value).ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
            let count = sql_row_owned_allocation_count(value).ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
            let charge = count.checked_mul(allocation_bookkeeping_bytes).and_then(|n| n.checked_add(payload))
                .ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
            *budget = budget.checked_sub(charge).ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
        }
        Ok(row)
    });
    reservations.release(budget).map_err(WorkspaceRelationError::Store)?;
    result
}

/// Digest of a sorted, explicit selected SQL host inventory. This is not a
/// discovery-completeness proof; the caller must separately witness the walk.
pub fn sql_inventory_digest(hosts: &[(String, SqlDialect)]) -> Result<[u8; 32], String> {
    if hosts.len() > MAX_SQL_HOSTS as usize {
        return Err("SQL inventory limit".into());
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql-analysis.v1.inventory\0");
    let mut previous: Option<&str> = None;
    for (path, dialect) in hosts {
        if !valid_path(path) || previous.is_some_and(|value| value >= path.as_str()) {
            return Err("invalid SQL inventory ordering".into());
        }
        hash.update(&(path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update(&[match dialect {
            SqlDialect::Sqlite => 1,
            SqlDialect::Postgres => 2,
        }]);
        previous = Some(path);
    }
    hash.update(&(hosts.len() as u64).to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}
/// Digest facts in contiguous native ordinal order, not Merkle-key order.
pub fn sql_host_fact_digest(facts: &[([u8; 32], &SqlAnalysisRecord)]) -> Result<[u8; 32], String> {
    if facts.len() > MAX_SQL_FACTS_PER_HOST as usize {
        return Err("SQL host fact limit".into());
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql.sqlite.host-facts.v1\0");
    let mut seen = std::collections::BTreeSet::new();
    for (index, (key, row)) in facts.iter().enumerate() {
        if !seen.insert(*key)
            || !matches!(row, SqlAnalysisRecord::Fact { ordinal, .. } if *ordinal as usize == index)
        {
            return Err("invalid SQL host fact ordering".into());
        }
        row.validate()?;
        let encoded =
            super::stylo_projection::bounded_encode(row, MAX_SQL_ROW_BYTES - MAGIC.len())?;
        hash.update(key);
        hash.update(&(encoded.len() as u64).to_be_bytes());
        hash.update(&encoded);
    }
    hash.update(&(facts.len() as u32).to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}
/// Content digest of one canonical project selection, used by its latest attempt.
pub fn sql_selection_digest(
    project: [u8; 32],
    row: &SqlAnalysisRecord,
) -> Result<[u8; 32], String> {
    if !matches!(row, SqlAnalysisRecord::Selection { project: owner, .. } if *owner == project) {
        return Err("wrong SQL selection owner".into());
    }
    row.validate()?;
    let encoded = super::stylo_projection::bounded_encode(row, MAX_SQL_ROW_BYTES - MAGIC.len())?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql-analysis.v1.selection\0");
    hash.update(&sql_selection_key(project));
    hash.update(&(encoded.len() as u64).to_be_bytes());
    hash.update(&encoded);
    Ok(*hash.finalize().as_bytes())
}

/// Check a complete bounded SQL relation's keys, ownership and partitions.
/// This does not authenticate a workspace closure, producer, settings bytes,
/// project membership, host bytes, or diagnostic UTF-8 boundaries.
pub fn admit_sql_relation(rows: &[([u8; 32], &SqlAnalysisRecord)]) -> Result<(), String> {
    use std::collections::BTreeMap;
    if rows.len() > 262_144 {
        return Err("SQL relation row limit".into());
    }
    let mut prior = None;
    let mut work = 0usize;
    let mut selections = BTreeMap::new();
    let mut attempts = BTreeMap::new();
    let mut hosts = BTreeMap::new();
    let mut facts: BTreeMap<[u8; 32], Vec<([u8; 32], &SqlAnalysisRecord)>> = BTreeMap::new();
    for (key, row) in rows {
        if prior.is_some_and(|p| p >= *key) {
            return Err("SQL relation keys not strictly ordered".into());
        }
        prior = Some(*key);
        row.validate()?;
        let encoded =
            super::stylo_projection::bounded_encode(row, MAX_SQL_ROW_BYTES - MAGIC.len())?;
        work = work
            .checked_add(encoded.len() + MAGIC.len())
            .ok_or("SQL relation work overflow")?;
        if work > 64 * 1024 * 1024 {
            return Err("SQL relation work limit".into());
        }
        match row {
            SqlAnalysisRecord::Selection {
                project,
                policy_complete,
                discovery,
                ..
            } => {
                if *key != sql_selection_key(*project)
                    || (*policy_complete && discovery.is_none())
                    || selections.insert(*project, *row).is_some()
                {
                    return Err("invalid SQL selection key".into());
                }
            }
            SqlAnalysisRecord::Attempt { project, .. } => {
                if *key != sql_attempt_key(*project) || attempts.insert(*project, *row).is_some() {
                    return Err("invalid SQL attempt key".into());
                }
            }
            SqlAnalysisRecord::Host { project, path, .. } => {
                if *key != sql_host_key(*project, path)? || hosts.insert(*key, *row).is_some() {
                    return Err("invalid SQL host key".into());
                }
            }
            SqlAnalysisRecord::Fact { host, .. } => {
                facts.entry(*host).or_default().push((*key, *row))
            }
        }
    }
    for (project, attempt) in &attempts {
        let SqlAnalysisRecord::Attempt {
            outcome,
            served_selection,
            ..
        } = attempt
        else {
            unreachable!()
        };
        let selected = selections
            .get(project)
            .map(|row| sql_selection_digest(*project, row))
            .transpose()?;
        if *served_selection != selected
            || (*outcome == SqlAttemptOutcome::Observed && selected.is_none())
        {
            return Err("SQL attempt does not bind the served selection".into());
        }
    }
    let mut inventories: BTreeMap<[u8; 32], Vec<(String, SqlDialect)>> = BTreeMap::new();
    for host in hosts.values() {
        if let SqlAnalysisRecord::Host {
            project,
            path,
            dialect,
            ..
        } = host
        {
            let inventory = inventories.entry(*project).or_default();
            if inventory.len() >= MAX_SQL_HOSTS as usize {
                return Err("SQL inventory limit".into());
            }
            inventory.push((path.clone(), *dialect));
        }
    }
    for (project, selection) in &selections {
        if !attempts.contains_key(project) {
            return Err("SQL selection missing latest attempt".into());
        }
        let SqlAnalysisRecord::Selection {
            host_count,
            inventory_digest,
            ..
        } = selection
        else {
            unreachable!()
        };
        let mut inventory = inventories.remove(project).unwrap_or_default();
        inventory.sort_by(|a, b| a.0.cmp(&b.0));
        if inventory.len() != *host_count as usize
            || sql_inventory_digest(&inventory)? != *inventory_digest
        {
            return Err("SQL selected inventory mismatch".into());
        }
    }
    if !inventories.is_empty() {
        return Err("SQL hosts without selection".into());
    }
    for (host_key, host) in &hosts {
        let SqlAnalysisRecord::Host {
            project,
            dialect,
            input,
            source,
            length,
            producer,
            settings,
            context,
            terminal,
            fact_count,
            fact_digest,
            ..
        } = host
        else {
            unreachable!()
        };
        if !selections.contains_key(project) {
            return Err("SQL host without selection".into());
        }
        let mut owned = facts.remove(host_key).unwrap_or_default();
        owned.sort_by_key(|(_, row)| match row {
            SqlAnalysisRecord::Fact { ordinal, .. } => *ordinal,
            _ => u32::MAX,
        });
        if owned.len() != *fact_count as usize || sql_host_fact_digest(&owned)? != *fact_digest {
            return Err("SQL host fact partition mismatch".into());
        }
        for (key, row) in owned {
            let SqlAnalysisRecord::Fact { ordinal, fact, .. } = row else {
                unreachable!()
            };
            let (Some(input), Some(source), Some(producer), Some(settings), Some(length)) =
                (input, source, producer, settings, length)
            else {
                return Err("SQL fact missing observed host basis".into());
            };
            if key
                != sql_fact_key(
                    *host_key, *dialect, *input, *source, *producer, *settings, *context, *ordinal,
                )
            {
                return Err("SQL fact key does not bind host basis".into());
            }
            match fact {
                SqlFact::SqliteRelation { .. } | SqlFact::SqliteOutputLineage { .. }
                    if *dialect != SqlDialect::Sqlite
                        || !matches!(
                            terminal,
                            SqlHostTerminal::Parsed {
                                contextual: SqlCoverage::Partial,
                                ..
                            }
                        ) =>
                {
                    return Err("SQL model fact without SQLite contextual analysis".into());
                }
                SqlFact::Diagnostic {
                    extent: Some(range),
                    ..
                } if u64::from(range.end) > *length => {
                    return Err("SQL diagnostic extent outside host claim".into());
                }
                SqlFact::Statement { extent, .. }
                    if !outline_host_ok(terminal) || u64::from(extent.end) > *length =>
                {
                    return Err("SQL statement lacks an outline host or exact extent".into());
                }
                SqlFact::Definition(definition)
                    if !outline_host_ok(terminal)
                        || [definition.name_extent, definition.extent]
                            .iter()
                            .any(|range| u64::from(range.end) > *length) =>
                {
                    return Err("SQL definition lacks an outline host or exact extent".into());
                }
                SqlFact::Reference(reference)
                    if !outline_host_ok(terminal)
                        || [reference.extent, reference.full_extent]
                            .iter()
                            .any(|range| u64::from(range.end) > *length) =>
                {
                    return Err("SQL reference lacks an outline host or exact extent".into());
                }
                SqlFact::PostgresColumnRead { extent, .. }
                    if *dialect != SqlDialect::Postgres
                        || context.is_none()
                        || !matches!(
                            terminal,
                            SqlHostTerminal::Parsed {
                                source: SqlCoverage::Partial,
                                contextual: SqlCoverage::Partial,
                                sqlite_lineage: None
                            }
                        )
                        || u64::from(extent.end) > *length =>
                {
                    return Err("PostgreSQL read lacks contextual host or exact extent".into());
                }
                _ => {}
            }
        }
    }
    if !facts.is_empty() {
        return Err("SQL fact without selected host".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::sql_analysis::{SqlByteRange, SqlDiagnostic};
    #[test]
    fn postgres_read_is_a_context_scoped_nonempty_native_extent() {
        let host = [1; 32];
        let fact = |relation_oid, attribute, start, end| SqlAnalysisRecord::Fact {
            host,
            ordinal: 0,
            fact: SqlFact::PostgresColumnRead {
                relation_oid,
                attribute,
                extent: SqlByteRange { start, end },
            },
        };
        assert!(fact(7, 1, 2, 5).validate().is_ok());
        assert!(fact(0, 1, 2, 5).validate().is_err());
        assert!(fact(7, 0, 2, 5).validate().is_err());
        assert!(fact(7, 1, 2, 2).validate().is_err());
        let mut bytes = Vec::new();
        SqlAnalysisRelation::encode_value(&fact(7, 1, 2, 5), &mut bytes);
        assert_eq!(
            SqlAnalysisRelation::decode_value(&bytes).unwrap(),
            fact(7, 1, 2, 5)
        );
    }
    #[test]
    fn postgres_read_requires_the_exact_contextual_host_and_in_bounds_extent() {
        let project = [1; 32];
        let host_key = sql_host_key(project, "a.sql").unwrap();
        let context = Some([6; 32]);
        let key = sql_fact_key(
            host_key,
            SqlDialect::Postgres,
            [2; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            context,
            0,
        );
        let fact = SqlAnalysisRecord::Fact {
            host: host_key,
            ordinal: 0,
            fact: SqlFact::PostgresColumnRead {
                relation_oid: 7,
                attribute: 1,
                extent: SqlByteRange { start: 1, end: 3 },
            },
        };
        let mut host = SqlAnalysisRecord::Host {
            project,
            path: "a.sql".into(),
            dialect: SqlDialect::Postgres,
            input: Some([2; 32]),
            source: Some([3; 32]),
            length: Some(8),
            producer: Some([4; 32]),
            settings: Some([5; 32]),
            context,
            terminal: SqlHostTerminal::Parsed {
                source: SqlCoverage::Partial,
                contextual: SqlCoverage::Partial,
                sqlite_lineage: None,
            },
            fact_count: 1,
            fact_digest: sql_host_fact_digest(&[(key, &fact)]).unwrap(),
        };
        let selection = SqlAnalysisRecord::Selection {
            project,
            source_root: "/fixture".into(),
            config_input: [7; 32],
            config_source: [8; 32],
            config_length: 32,
            discovery: None,
            policy_complete: false,
            implicit: false,
            gaps: Default::default(),
            inventory: SqlInventory::Incomplete,
            inventory_digest: sql_inventory_digest(&[("a.sql".into(), SqlDialect::Postgres)])
                .unwrap(),
            host_count: 1,
        };
        let attempt = SqlAnalysisRecord::Attempt {
            project,
            outcome: SqlAttemptOutcome::Observed,
            served_selection: Some(sql_selection_digest(project, &selection).unwrap()),
        };
        let admit = |host: &SqlAnalysisRecord| {
            let mut rows = vec![
                (sql_selection_key(project), &selection),
                (sql_attempt_key(project), &attempt),
                (host_key, host),
                (key, &fact),
            ];
            rows.sort_by_key(|row| row.0);
            admit_sql_relation(&rows)
        };
        assert!(admit(&host).is_ok());
        if let SqlAnalysisRecord::Host { length, .. } = &mut host {
            *length = Some(2);
        }
        assert!(admit(&host).is_err());
        if let SqlAnalysisRecord::Host {
            length, terminal, ..
        } = &mut host
        {
            *length = Some(8);
            *terminal = SqlHostTerminal::Parsed {
                source: SqlCoverage::Unavailable,
                contextual: SqlCoverage::Partial,
                sqlite_lineage: None,
            };
        }
        assert!(admit(&host).is_err());
    }
    #[test]
    fn owned_payload_counts_capacity_not_string_length() {
        let mut name = String::with_capacity(128);
        name.push_str("table");
        let capacity = name.capacity();
        let row = SqlAnalysisRecord::Fact {
            host: [1; 32], ordinal: 0,
            fact: SqlFact::SqliteRelation {
                name, relation_kind: backend_semantic::sql_analysis::SqliteRelationKind::Table,
            },
        };
        assert_eq!(sql_row_owned_payload_bytes(&row), Some(capacity));
    }

    #[test]
    fn owned_payload_counts_box_vector_and_nested_paths() {
        use backend_semantic::sql_analysis::{SqlByteRange, SqlEntityRef, SqlRefKind, SqlRefRole, SqlReference, SqlResolution, SqlStatementKind};
        let mut path = String::with_capacity(25);
        path.push_str("a.sql");
        let path_capacity = path.capacity();
        let mut targets = Vec::with_capacity(64);
        targets.push(SqlEntityRef { path, ordinal: 0 });
        let vector_capacity = targets.capacity();
        let mut name = String::with_capacity(30);
        name.push('x');
        let name_capacity = name.capacity();
        let row = SqlAnalysisRecord::Fact {
            host: [1; 32], ordinal: 0,
            fact: SqlFact::Reference(Box::new(SqlReference {
                kind: SqlRefKind::Column, role: SqlRefRole::Read, schema: None,
                qualifier: None, name, extent: SqlByteRange { start: 0, end: 1 },
                full_extent: SqlByteRange { start: 0, end: 1 }, statement: 0,
                statement_kind: SqlStatementKind::Select, in_object: None,
                resolution: SqlResolution::Expands { targets },
            })),
        };
        assert_eq!(sql_row_owned_payload_bytes(&row), Some(
            std::mem::size_of::<SqlReference>() + name_capacity + path_capacity
                + vector_capacity * std::mem::size_of::<SqlEntityRef>()
        ));
        assert_eq!(sql_row_owned_allocation_count(&row), Some(4));
    }

    #[test]
    fn streamed_sql_rows_and_lookup_preserve_canonical_root() {
        use backend_store::{FileStore, RelationAdmissionRegistry};
        use backend_version::{CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope};
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let registry = RelationAdmissionRegistry::new().with_relation::<SqlAnalysisRelation>().unwrap();
        let store = FileStore::open_with_registry(directory.path(), 1024 * 1024, registry).unwrap();
        let rows = (0..200u16).map(|i| {
            let mut key = [0; 32]; key[..2].copy_from_slice(&i.to_be_bytes());
            (key, SqlAnalysisRecord::Fact { host: [1; 32], ordinal: u32::from(i), fact: SqlFact::SqliteRelation {
                name: format!("table_{i:03}{}", "x".repeat(230)), relation_kind: backend_semantic::sql_analysis::SqliteRelationKind::Table,
            }})
        }).collect::<Vec<_>>();
        let tree = backend_version::PersistentTree::<SqlAnalysisRelation>::from_sorted_items(&rows).unwrap();
        assert!(tree.root().level() > 0, "fixture must contain multiple leaves");
        let coverage = CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(ScopeRoot::from_u64(1)));
        let state = RelationState::<SqlAnalysisRelation>::from_entries(rows.clone(), coverage).unwrap();
        let root = state.root();
        store.write_relation_state(&state).unwrap();
        let handle = WorkspacePreflightRelationHandle::<SqlAnalysisRelation>::new(Arc::new(store), *root.as_bytes()).unwrap();
        let mut available = 16 * 1024 * 1024;
        let mut collected = Vec::with_capacity(rows.len());
        let mut cursor = None;
        loop {
            cursor = sql_visit_preflighted(&handle, cursor.as_ref(), 7, &mut available, 64, &mut |key, value, budget| {
                let count = sql_row_owned_allocation_count(&value).ok_or(StoreError::Bounds)?;
                let payload = sql_row_owned_payload_bytes(&value).ok_or(StoreError::Bounds)?;
                let charge = count.checked_mul(64).and_then(|v| v.checked_add(payload)).ok_or(StoreError::Bounds)?;
                *budget = budget.checked_sub(charge).ok_or(StoreError::Bounds)?;
                collected.push((*key, value));
                Ok(())
            }).unwrap();
            if cursor.is_none() { break; }
        }
        assert_eq!(collected, rows);
        assert_eq!(sql_lookup_preflighted(&handle, &rows[70].0, &mut available, 64).unwrap(), Some(rows[70].1.clone()));
        let rebuilt = RelationState::<SqlAnalysisRelation>::from_entries(collected, coverage).unwrap();
        assert_eq!(rebuilt.root(), root);
    }

    #[test]
    fn streamed_sql_child_denial_precedes_every_leaf_decode() {
        use backend_store::{FileStore, RelationAdmissionRegistry};
        use backend_version::{CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope};
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let registry = RelationAdmissionRegistry::new().with_relation::<SqlAnalysisRelation>().unwrap();
        let store = FileStore::open_with_registry(directory.path(), 1024 * 1024, registry).unwrap();
        let rows = (0..200u16).map(|i| {
            let mut key = [0; 32]; key[..2].copy_from_slice(&i.to_be_bytes());
            (key, SqlAnalysisRecord::Fact { host: [1; 32], ordinal: u32::from(i), fact: SqlFact::SqliteRelation {
                name: format!("table_{i:03}{}", "x".repeat(230)), relation_kind: backend_semantic::sql_analysis::SqliteRelationKind::Table,
            }})
        }).collect::<Vec<_>>();
        let tree = backend_version::PersistentTree::<SqlAnalysisRelation>::from_sorted_items(&rows).unwrap();
        assert!(tree.root().level() > 0);
        let coverage = CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(ScopeRoot::from_u64(1)));
        let state = RelationState::<SqlAnalysisRelation>::from_entries(rows, coverage).unwrap();
        store.write_relation_state(&state).unwrap();
        let handle = WorkspacePreflightRelationHandle::<SqlAnalysisRelation>::new(Arc::new(store), *state.root().as_bytes()).unwrap();
        SQL_DECODE_CALLS.with(|calls| calls.set(0));
        let mut budget = 16 * 1024 * 1024;
        let mut gate = SqlNodeDecodeReservations::new(64);
        let mut seen_leaf = false;
        let result = handle.visit(None, 7, 32, &mut budget, &mut |node, budget| {
            if node.is_leaf() { seen_leaf = true; return Err(StoreError::Bounds); }
            gate.preflight(node, budget)
        }, &mut |_, _, _| panic!("denied leaf must not yield rows"));
        assert!(matches!(result, Err(WorkspaceRelationError::Store(StoreError::Bounds))));
        assert!(seen_leaf);
        SQL_DECODE_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    }

    #[test]
    fn whole_leaf_preflight_charges_unretained_row_before_decode() {
        use backend_version::{CanonicalNodeView, PersistentTree};
        let rows = vec![
            ([1; 32], SqlAnalysisRecord::Attempt { project: [1; 32], outcome: SqlAttemptOutcome::Observed, served_selection: None }),
            ([2; 32], SqlAnalysisRecord::Fact { host: [2; 32], ordinal: 0, fact: SqlFact::SqliteRelation {
                name: "q\"".repeat(100), relation_kind: backend_semantic::sql_analysis::SqliteRelationKind::Table,
            }}),
        ];
        let tree = PersistentTree::<SqlAnalysisRelation>::from_sorted_items(&rows).unwrap();
        let view = CanonicalNodeView::parse(tree.root().as_bytes(), SchemaIdentity::of_relation::<SqlAnalysisRelation>()).unwrap();
        assert!(view.is_leaf());
        let lengths: Vec<_> = view.leaf_field_iter().unwrap().map(|f| f.unwrap().1.len()).collect();
        let max = sql_row_decode_bound(*lengths.iter().max().unwrap()).unwrap();
        let overhead = 64usize;
        let expected = max.owned_payload_bytes + max.scratch_payload_bytes
            + (max.owned_allocations + max.scratch_allocations) * overhead
            + 2 * view.encoded_len() + 2 * overhead;
        let mut insufficient = expected - 1;
        let mut gate = SqlNodeDecodeReservations::new(overhead);
        assert!(matches!(gate.preflight(&view, &mut insufficient), Err(StoreError::Bounds)));
        assert_eq!(insufficient, expected - 1);
        for initial in [expected, expected + 1] {
            let mut available = initial;
            let mut gate = SqlNodeDecodeReservations::new(overhead);
            gate.preflight(&view, &mut available).unwrap();
            assert_eq!(available, initial - expected);
            gate.release(&mut available).unwrap();
            assert_eq!(available, initial);
        }
    }

    #[test]
    fn sequential_leaf_bound_includes_unretained_values() {
        let short = vec![0; 20];
        let unrelated = vec![0; 9000];
        let bound = sql_leaf_sequential_decode_bound([short.as_slice(), unrelated.as_slice()]).unwrap();
        assert_eq!(bound, sql_row_decode_bound(unrelated.len()).unwrap());
        assert!(bound.owned_payload_bytes > sql_row_decode_bound(short.len()).unwrap().owned_payload_bytes);
        let oversized = vec![0; MAX_SQL_ROW_BYTES + 1];
        assert!(sql_leaf_sequential_decode_bound([short.as_slice(), oversized.as_slice()]).is_none());
        let empty = sql_leaf_sequential_decode_bound(std::iter::empty()).unwrap();
        assert_eq!(empty.owned_payload_bytes, 0);
        assert_eq!(empty.scratch_payload_bytes, 0);
    }

    #[test]
    fn row_payload_budget_reserves_before_decode_and_refunds_scratch() {
        let row = SqlAnalysisRecord::Fact {
            host: [1; 32], ordinal: 0,
            fact: SqlFact::SqliteRelation { name: "a\"b".into(), relation_kind: backend_semantic::sql_analysis::SqliteRelationKind::Table },
        };
        let mut encoded = Vec::new();
        SqlAnalysisRelation::encode_value(&row, &mut encoded);
        let bound = sql_row_decode_bound(encoded.len()).unwrap();
        let required = bound.owned_payload_bytes + bound.scratch_payload_bytes;
        let mut too_small = required - 1;
        assert_eq!(sql_decode_row_with_payload_budget(&encoded, &mut too_small), Err(SqlRowBudgetError::Exhausted));
        assert_eq!(too_small, required - 1);
        for initial in [required, required + 1] {
            let mut budget = initial;
            let decoded = sql_decode_row_with_payload_budget(&encoded, &mut budget).unwrap();
            assert_eq!(decoded, row);
            assert_eq!(budget, initial - sql_row_owned_payload_bytes(&decoded).unwrap());
        }
        let mut malformed = encoded.clone();
        malformed[0] ^= 1;
        let mut budget = required;
        assert_eq!(sql_decode_row_with_payload_budget(&malformed, &mut budget), Err(SqlRowBudgetError::Malformed));
        assert_eq!(budget, required);
    }

    #[test]
    fn dense_escaped_reference_decode_stays_within_its_shape_bound() {
        use backend_semantic::sql_analysis::{SqlByteRange, SqlEntityRef, SqlRefKind, SqlRefRole, SqlReference, SqlResolution, SqlStatementKind};
        let row = SqlAnalysisRecord::Fact {
            host: [1; 32], ordinal: 0,
            fact: SqlFact::Reference(Box::new(SqlReference {
                kind: SqlRefKind::Column, role: SqlRefRole::Read,
                schema: Some("s\"".repeat(100)), qualifier: Some("q\"".repeat(100)),
                name: "n\"".repeat(100), extent: SqlByteRange { start: 0, end: 1 },
                full_extent: SqlByteRange { start: 0, end: 1 }, statement: 0,
                statement_kind: SqlStatementKind::Select, in_object: None,
                resolution: SqlResolution::Expands {
                    targets: (0..64).map(|ordinal| SqlEntityRef { path: format!("{}.sql", "p\"".repeat(80)), ordinal }).collect(),
                },
            })),
        };
        let mut encoded = Vec::new();
        SqlAnalysisRelation::encode_value(&row, &mut encoded);
        let bound = sql_row_decode_bound(encoded.len()).unwrap();
        let allocation = allocation_counter::measure(|| {
            let decoded = SqlAnalysisRelation::decode_value(std::hint::black_box(&encoded)).unwrap();
            assert_eq!(decoded, row);
            assert!(sql_row_owned_payload_bytes(&decoded).unwrap() <= bound.owned_payload_bytes);
            std::hint::black_box(decoded);
        });
        assert!(allocation.bytes_max <= (bound.owned_payload_bytes + bound.scratch_payload_bytes) as u64, "{allocation:?}");
        assert!(allocation.count_max <= (bound.owned_allocations + bound.scratch_allocations) as u64, "{allocation:?}");
    }

    #[test]
    fn canonical_check_rejects_equivalent_noncanonical_json() {
        let row = SqlAnalysisRecord::Attempt {
            project: [1; 32],
            outcome: SqlAttemptOutcome::Observed,
            served_selection: None,
        };
        let mut canonical = Vec::new();
        SqlAnalysisRelation::encode_value(&row, &mut canonical);
        assert_eq!(SqlAnalysisRelation::decode_value(&canonical).unwrap(), row);
        assert!(SqlAnalysisRelation::canonical_value_matches(&row, &canonical));
        let mut spaced = MAGIC.to_vec();
        spaced.extend_from_slice(b" ");
        spaced.extend_from_slice(&canonical[MAGIC.len()..]);
        assert!(SqlAnalysisRelation::decode_value(&spaced).is_err());
        assert!(!SqlAnalysisRelation::canonical_value_matches(&row, &spaced));
        let mut trailing = canonical.clone();
        trailing.push(b' ');
        assert!(SqlAnalysisRelation::decode_value(&trailing).is_err());
    }

    #[test]
    fn strict_rows_and_identity_bind_the_full_analysis_basis() {
        let host = sql_host_key([1; 32], "queries/a.sql").unwrap();
        let fact = SqlAnalysisRecord::Fact {
            host,
            ordinal: 0,
            fact: SqlFact::Diagnostic {
                code: SqlDiagnostic::UnknownColumn,
                extent: Some(SqlByteRange { start: 7, end: 14 }),
            },
        };
        let mut bytes = Vec::new();
        SqlAnalysisRelation::encode_value(&fact, &mut bytes);
        assert_eq!(SqlAnalysisRelation::decode_value(&bytes).unwrap(), fact);
        let mut invalid: serde_json::Value = serde_json::from_slice(&bytes[4..]).unwrap();
        invalid["extra"] = serde_json::json!(true);
        let mut forged = MAGIC.to_vec();
        forged.extend(serde_json::to_vec(&invalid).unwrap());
        assert!(SqlAnalysisRelation::decode_value(&forged).is_err());
        assert!(sql_host_key([1; 32], "../escape.sql").is_err());
        let key = |input, context| {
            sql_fact_key(
                host,
                SqlDialect::Sqlite,
                input,
                [3; 32],
                [4; 32],
                [5; 32],
                context,
                0,
            )
        };
        assert_ne!(key([2; 32], Some([6; 32])), key([9; 32], Some([6; 32])));
        assert_ne!(key([2; 32], Some([6; 32])), key([2; 32], Some([7; 32])));
        assert_ne!(key([2; 32], Some([6; 32])), key([2; 32], None));
        assert_ne!(
            key([2; 32], None),
            sql_fact_key(
                host,
                SqlDialect::Postgres,
                [2; 32],
                [3; 32],
                [4; 32],
                [5; 32],
                None,
                0
            )
        );
        assert_ne!(sql_selection_key([1; 32]), sql_attempt_key([1; 32]));
    }
    #[test]
    fn unavailable_or_missing_context_never_becomes_successful_facts() {
        let mut host = SqlAnalysisRecord::Host {
            project: [1; 32],
            path: "a.sql".into(),
            dialect: SqlDialect::Sqlite,
            input: Some([2; 32]),
            source: Some([3; 32]),
            length: Some(8),
            producer: Some([4; 32]),
            settings: Some([5; 32]),
            context: None,
            terminal: SqlHostTerminal::Parsed {
                source: SqlCoverage::Unavailable,
                contextual: SqlCoverage::Partial,
                sqlite_lineage: Some(SqliteLineageCoverage::NativeComplete),
            },
            fact_count: 0,
            fact_digest: [6; 32],
        };
        assert!(host.validate().is_err());
        if let SqlAnalysisRecord::Host { terminal, .. } = &mut host {
            *terminal = SqlHostTerminal::Parsed {
                source: SqlCoverage::Unavailable,
                contextual: SqlCoverage::Unavailable,
                sqlite_lineage: Some(SqliteLineageCoverage::NativeComplete),
            };
        }
        assert!(host.validate().is_err());
        if let SqlAnalysisRecord::Host {
            terminal,
            fact_count,
            ..
        } = &mut host
        {
            *terminal = SqlHostTerminal::Unavailable(SqlUnavailable::EngineNotAdmitted);
            *fact_count = 1;
        }
        assert!(host.validate().is_err());
    }

    #[test]
    fn relation_admission_rejects_missing_terminal_forged_basis_and_orphans() {
        let project = [1; 32];
        let host_key = sql_host_key(project, "a.sql").unwrap();
        let fact_key = sql_fact_key(
            host_key,
            SqlDialect::Sqlite,
            [2; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            Some([6; 32]),
            0,
        );
        let fact = SqlAnalysisRecord::Fact {
            host: host_key,
            ordinal: 0,
            fact: SqlFact::Diagnostic {
                code: SqlDiagnostic::UnknownColumn,
                extent: Some(SqlByteRange { start: 0, end: 2 }),
            },
        };
        let host = SqlAnalysisRecord::Host {
            project,
            path: "a.sql".into(),
            dialect: SqlDialect::Sqlite,
            input: Some([2; 32]),
            source: Some([3; 32]),
            length: Some(8),
            producer: Some([4; 32]),
            settings: Some([5; 32]),
            context: Some([6; 32]),
            terminal: SqlHostTerminal::Parsed {
                source: SqlCoverage::Unavailable,
                contextual: SqlCoverage::Partial,
                sqlite_lineage: Some(SqliteLineageCoverage::NativeComplete),
            },
            fact_count: 1,
            fact_digest: sql_host_fact_digest(&[(fact_key, &fact)]).unwrap(),
        };
        let selection = SqlAnalysisRecord::Selection {
            project,
            source_root: "/fixture".into(),
            config_input: [7; 32],
            config_source: [8; 32],
            config_length: 32,
            discovery: None,
            policy_complete: false,
            implicit: false,
            gaps: Default::default(),
            inventory: SqlInventory::Incomplete,
            inventory_digest: sql_inventory_digest(&[("a.sql".into(), SqlDialect::Sqlite)])
                .unwrap(),
            host_count: 1,
        };
        let attempt = SqlAnalysisRecord::Attempt {
            project,
            outcome: SqlAttemptOutcome::Observed,
            served_selection: Some(sql_selection_digest(project, &selection).unwrap()),
        };
        let mut rows = vec![
            (sql_selection_key(project), &selection),
            (sql_attempt_key(project), &attempt),
            (host_key, &host),
            (fact_key, &fact),
        ];
        rows.sort_by_key(|row| row.0);
        assert!(admit_sql_relation(&rows).is_ok());
        assert!(
            admit_sql_relation(
                &rows
                    .iter()
                    .copied()
                    .filter(|(_, row)| !matches!(row, SqlAnalysisRecord::Attempt { .. }))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
        assert!(
            admit_sql_relation(
                &rows
                    .iter()
                    .copied()
                    .filter(|(_, row)| !matches!(row, SqlAnalysisRecord::Host { .. }))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
        let wrong_key = sql_fact_key(
            host_key,
            SqlDialect::Postgres,
            [2; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            Some([6; 32]),
            0,
        );
        let mut forged = vec![
            (sql_selection_key(project), &selection),
            (sql_attempt_key(project), &attempt),
            (host_key, &host),
            (wrong_key, &fact),
        ];
        forged.sort_by_key(|row| row.0);
        assert!(admit_sql_relation(&forged).is_err());
    }
}
