//! Canonical SQL analysis rows. Rows are claims until owner admission verifies
//! source/config bytes, selected roots, context and complete host terminals.
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_semantic::sql_analysis::{SqlCoverage, SqlDialect, SqlFact, SqliteLineageCoverage};
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use serde::{Deserialize, Serialize};

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
                    return Err("invalid SQL selection".into());
                }
            }
            Self::Attempt { project, .. } if *project == [0; 32] => {
                return Err("invalid SQL attempt".into());
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
                    return Err("invalid SQL host".into());
                }
                if [input, source, producer, settings, context]
                    .into_iter()
                    .any(|value| value.is_some_and(|digest| digest == [0; 32]))
                    || length.is_some_and(|value| value > 512 * 1024)
                {
                    return Err("invalid SQL host identity or extent".into());
                }
                if matches!(terminal, SqlHostTerminal::Parsed { .. })
                    && (input.is_none()
                        || source.is_none()
                        || length.is_none()
                        || producer.is_none()
                        || settings.is_none())
                {
                    return Err("parsed SQL host missing observed basis".into());
                }
                if matches!(terminal, SqlHostTerminal::Unavailable(_)) && *fact_count != 0 {
                    return Err("unavailable SQL host has facts".into());
                }
                if let SqlHostTerminal::Parsed {
                    contextual,
                    sqlite_lineage,
                    ..
                } = terminal
                {
                    if *contextual == SqlCoverage::Partial && context.is_none() {
                        return Err("contextual SQL host missing context".into());
                    }
                    if *dialect != SqlDialect::Sqlite && sqlite_lineage.is_some() {
                        return Err("SQLite lineage on another dialect".into());
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
                        return Err("native SQLite lineage without contextual analysis".into());
                    }
                }
            }
            Self::Fact {
                host,
                ordinal,
                fact,
            } => {
                if *host == [0; 32] || *ordinal >= MAX_SQL_FACTS_PER_HOST {
                    return Err("invalid SQL fact host".into());
                }
                match fact {
                    SqlFact::SqliteRelation { name, .. } if !valid_name(name) => {
                        return Err("invalid SQL relation name".into());
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
                        return Err("invalid SQL lineage".into());
                    }
                    SqlFact::Diagnostic {
                        extent: Some(range),
                        ..
                    } if range.start > range.end => {
                        return Err("invalid SQL diagnostic extent".into());
                    }
                    SqlFact::PostgresColumnRead {
                        relation_oid,
                        attribute,
                        extent,
                    } if *relation_oid == 0 || *attribute == 0 || extent.start >= extent.end => {
                        return Err("invalid PostgreSQL read claim".into());
                    }
                    SqlFact::Statement { extent, .. } if extent.start >= extent.end => {
                        return Err("invalid SQL statement extent".into());
                    }
                    SqlFact::Definition(definition) if !valid_definition(definition) => {
                        return Err("invalid SQL definition".into());
                    }
                    SqlFact::Reference(reference) if !valid_reference(reference) => {
                        return Err("invalid SQL reference".into());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        super::stylo_projection::bounded_encode(self, MAX_SQL_ROW_BYTES - MAGIC.len())?;
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
        out.extend_from_slice(value);
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
        if bytes.len() > MAX_SQL_ROW_BYTES || !bytes.starts_with(MAGIC) {
            return Err(RelationDecodeError::Malformed);
        }
        let value: SqlAnalysisRecord = serde_json::from_slice(&bytes[MAGIC.len()..])
            .map_err(|_| RelationDecodeError::Malformed)?;
        value
            .validate()
            .map_err(|_| RelationDecodeError::Malformed)?;
        let mut canonical = Vec::new();
        Self::encode_value(&value, &mut canonical);
        if canonical != bytes {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(value)
    }
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
