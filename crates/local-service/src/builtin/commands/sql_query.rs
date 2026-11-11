//! Root-bound SQL native claim queries. Page completion is not semantic or
//! source-occurrence completeness. This module is not a publication authority.
use super::super::{BuiltinModelError, BuiltinWorkspaceRelation};
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{
        MAX_SQL_ROW_BYTES, SqlAnalysisRecord, SqlAnalysisRelation, SqlAttemptOutcome,
        SqlHostTerminal, SqlInventory, sql_analysis_relation, sql_attempt_key, sql_fact_key,
        sql_selection_digest, sql_selection_key,
    },
};
use backend_library::sql_query::*;
use backend_semantic::sql_analysis::{
    SqlDefinition, SqlDefinitionAction, SqlEntityRef, SqlFact, SqlObjectKind, SqlResolution,
};
use backend_version::Relation;
use std::path::Path;

type RelationHandle = backend_engine::workspace::WorkspaceRelationHandle<SqlAnalysisRelation>;
fn error(message: impl std::fmt::Display) -> BuiltinModelError {
    BuiltinModelError(format!("native SQL query: {message}"))
}
fn failure(value: backend_engine::builtin::SqlUnavailable) -> SqlFailure {
    use backend_engine::builtin::SqlUnavailable as U;
    match value {
        U::Read => SqlFailure::Read,
        U::SelectionUnavailable => SqlFailure::SelectionUnavailable,
        U::InvalidUtf8 => SqlFailure::InvalidUtf8,
        U::Limit => SqlFailure::Limit,
        U::EngineNotAdmitted => SqlFailure::EngineNotAdmitted,
        U::Deadline => SqlFailure::Deadline,
        U::Cancelled => SqlFailure::Cancelled,
        U::WorkerFailure => SqlFailure::WorkerFailure,
        U::UnsupportedDialect => SqlFailure::UnsupportedDialect,
        U::Context => SqlFailure::Context,
    }
}
struct Work {
    rows: usize,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
}
impl Work {
    fn charge(&mut self, row: Option<&SqlAnalysisRecord>) -> Result<(), BuiltinModelError> {
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| error("row work overflow"))?;
        if let Some(row) = row {
            let mut bytes = Vec::new();
            SqlAnalysisRelation::encode_value(row, &mut bytes);
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| error("byte work overflow"))?;
        }
        if self.rows > self.max_rows || self.bytes > self.max_bytes {
            return Err(error("SQL work budget exceeded"));
        }
        Ok(())
    }
    fn can_scan(&self) -> bool {
        self.max_rows.saturating_sub(self.rows) >= 3
            && self.max_bytes.saturating_sub(self.bytes) >= 3 * MAX_SQL_ROW_BYTES
    }
}
fn lookup(
    relation: &RelationHandle,
    key: &[u8; 32],
    work: &mut Work,
) -> Result<Option<SqlAnalysisRecord>, BuiltinModelError> {
    let row = relation.lookup(key).map_err(error)?;
    work.charge(row.as_ref())?;
    Ok(row)
}
/// Entities selected by a `uses` filter: the named object, plus every column it owns.
struct UsesSet {
    entities: std::collections::BTreeSet<SqlEntityRef>,
}
impl UsesSet {
    fn mentions(&self, resolution: &SqlResolution) -> bool {
        match resolution {
            SqlResolution::Resolved { target, .. } => self.entities.contains(target),
            SqlResolution::Relation { relation, .. } => self.entities.contains(relation),
            SqlResolution::Expands { targets } => targets.iter().any(|target| self.entities.contains(target)),
            SqlResolution::Ambiguous { candidates, .. } => candidates.iter().any(|candidate| self.entities.contains(candidate)),
            SqlResolution::Local | SqlResolution::Unresolved { .. } => false,
        }
    }
}
/// Resolve `table`, `schema.table`, `table.column` or `schema.table.column` against
/// the definitions of the selected project, by head name (after every rename).
fn resolve_uses(
    relation: &RelationHandle,
    project: [u8; 32],
    spec: &str,
    work: &mut Work,
) -> Result<UsesSet, BuiltinModelError> {
    let mut paths = std::collections::BTreeMap::<[u8; 32], String>::new();
    let mut definitions = Vec::<([u8; 32], u32, Box<SqlDefinition>)>::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(error)?;
        for (key, row) in page.entries() {
            work.charge(Some(row))?;
            match row {
                SqlAnalysisRecord::Host { project: owner, path, .. } if *owner == project => {
                    paths.insert(*key, path.clone());
                }
                SqlAnalysisRecord::Fact { host, ordinal, fact: SqlFact::Definition(definition) } => {
                    definitions.push((*host, *ordinal, definition.clone()));
                }
                _ => {}
            }
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            Some(_) => return Err(error("SQL scan page did not advance")),
            None => break,
        }
    }
    let parts: Vec<String> = spec.split('.').map(|part| part.to_ascii_lowercase()).collect();
    let lower = |value: &Option<String>| value.as_deref().map(str::to_ascii_lowercase);
    let creates = |definition: &SqlDefinition| {
        matches!(definition.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn)
    };
    let mut entities = std::collections::BTreeSet::new();
    for (host, ordinal, definition) in &definitions {
        let Some(path) = paths.get(host) else { continue };
        if !creates(definition) {
            continue;
        }
        let name = lower(&definition.head_name);
        let parent = lower(&definition.head_parent);
        let schema = lower(&definition.schema);
        let parent_schema = lower(&definition.parent_schema);
        let is_column = definition.kind == SqlObjectKind::Column;
        let hit = match (is_column, parts.len()) {
            (false, 1) => name.as_deref() == Some(parts[0].as_str()),
            (false, 2) => schema.as_deref() == Some(parts[0].as_str()) && name.as_deref() == Some(parts[1].as_str()),
            (true, 2) => parent.as_deref() == Some(parts[0].as_str()) && name.as_deref() == Some(parts[1].as_str()),
            (true, 3) => {
                parent.as_deref() == Some(parts[1].as_str())
                    && name.as_deref() == Some(parts[2].as_str())
                    && (schema.as_deref() == Some(parts[0].as_str()) || parent_schema.as_deref() == Some(parts[0].as_str()))
            }
            _ => false,
        };
        if hit {
            entities.insert(SqlEntityRef { path: path.clone(), ordinal: *ordinal });
        }
    }
    // A relation also stands for the columns it owns.
    let owners = entities.clone();
    for (host, ordinal, definition) in &definitions {
        let Some(path) = paths.get(host) else { continue };
        if creates(definition)
            && definition.kind == SqlObjectKind::Column
            && definition.parent_entity.as_ref().is_some_and(|parent| owners.contains(parent))
        {
            entities.insert(SqlEntityRef { path: path.clone(), ordinal: *ordinal });
        }
    }
    Ok(UsesSet { entities })
}
fn matches_filter(fact: &SqlFact, request: &SqlQueryRequest, uses: Option<&UsesSet>) -> bool {
    use backend_semantic::sql_analysis::{SqlObjectKind as K, SqlRefKind as R, SqlStatementSupport};
    let lowered = |value: &str| value.to_ascii_lowercase();
    let name_is = |names: &[&str]| {
        request.name.as_ref().is_none_or(|wanted| names.iter().any(|name| lowered(name) == lowered(wanted)))
    };
    match (request.role, fact) {
        (SqlQueryRole::Any, _) => true,
        (SqlQueryRole::Relation, SqlFact::SqliteRelation { name, .. }) => {
            request.name.as_ref().is_none_or(|n| n == name)
        }
        (SqlQueryRole::OutputLineage, SqlFact::SqliteOutputLineage { output_name, .. }) => {
            request.name.as_ref().is_none_or(|n| n == output_name)
        }
        (SqlQueryRole::Diagnostic, SqlFact::Diagnostic { .. }) => true,
        (SqlQueryRole::Diagnostic, SqlFact::Reference(reference)) => {
            matches!(reference.kind, R::Column | R::Relation)
                && matches!(reference.resolution, SqlResolution::Unresolved { .. })
        }
        (SqlQueryRole::ColumnRead, SqlFact::PostgresColumnRead { .. }) => true,
        (SqlQueryRole::Statement, SqlFact::Statement { .. }) => true,
        (SqlQueryRole::Gap, SqlFact::Statement { support, .. }) => {
            !matches!(support, SqlStatementSupport::Full)
        }
        (SqlQueryRole::Definition, SqlFact::Definition(definition)) => {
            let head = definition.head_name.as_deref().unwrap_or(&definition.name);
            name_is(&[definition.name.as_str(), head])
                && request.kind.is_none_or(|kind| kind == definition.kind)
                && request.parent.as_ref().is_none_or(|wanted| {
                    [definition.parent.as_deref(), definition.head_parent.as_deref()]
                        .into_iter()
                        .flatten()
                        .any(|parent| lowered(parent) == lowered(wanted))
                })
        }
        (SqlQueryRole::Reference, SqlFact::Reference(reference)) => {
            name_is(&[reference.name.as_str()])
                && request.kind.is_none_or(|kind| match (kind, reference.kind) {
                    (K::Table | K::View, R::Relation)
                    | (K::Column, R::Column)
                    | (K::Function, R::Function)
                    | (K::Type, R::Type)
                    | (K::Schema, R::Schema)
                    | (K::Index, R::Index)
                    | (K::Sequence, R::Sequence)
                    | (K::Trigger, R::Trigger) => true,
                    _ => false,
                })
                && uses.is_none_or(|set| set.mentions(&reference.resolution))
        }
        _ => false,
    }
}
fn cursor(
    request: &SqlQueryRequest,
    secret: &[u8; 32],
    manifest_root: [u8; 32],
    sql_root: [u8; 32],
    project: [u8; 32],
    after: [u8; 32],
) -> Result<SqlQueryCursor, BuiltinModelError> {
    SqlQueryCursor::issue(
        SqlCursorClaims {
            manifest_root,
            sql_root,
            project,
            role: request.role,
            name: request.name.clone(),
            kind: request.kind,
            parent: request.parent.clone(),
            uses: request.uses.clone(),
            page_rows: request.page_rows,
            output_bytes: request.output_bytes,
            scan_rows: request.scan_rows,
            scan_bytes: request.scan_bytes,
            after,
        },
        secret,
    )
    .map_err(error)
}
/// The exact written byte range a fact points at, if it has one.
fn fact_extent(fact: &SqlFact) -> Option<backend_semantic::sql_analysis::SqlByteRange> {
    match fact {
        SqlFact::Diagnostic { extent, .. } => *extent,
        SqlFact::PostgresColumnRead { extent, .. } | SqlFact::Statement { extent, .. } => Some(*extent),
        SqlFact::Definition(definition) => Some(definition.name_extent),
        SqlFact::Reference(reference) => Some(reference.extent),
        SqlFact::SqliteRelation { .. } | SqlFact::SqliteOutputLineage { .. } => None,
    }
}
fn empty(
    manifest_root: [u8; 32],
    sql_root: Option<[u8; 32]>,
    project: [u8; 32],
    attempt: SqlAttemptState,
) -> SqlQueryPage {
    SqlQueryPage {
        host_gaps: Vec::new(),
        basis: SqlQueryBasis {
            manifest_root,
            sql_root,
            project,
            attempt,
            host_health: SqlHostHealth::Unknown,
            inventory_complete: false,
            policy_complete: false,
            source_occurrences_complete: false,
            gaps: Default::default(),
            implicit_selection: false,
            catalog_is_deployed: false,
        },
        hits: Vec::new(),
        terminal: SqlQueryTerminal::Unavailable,
    }
}
/// Query a selected snapshot with a durable owner sealing authority.
pub(in crate::builtin) fn query(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    request: &SqlQueryRequest,
) -> Result<SqlQueryPage, BuiltinModelError> {
    request.validate().map_err(error)?;
    let project = backend_library::package_key(request.package.as_str()).to_bytes();
    let manifest_root = *snapshot.root().as_bytes();
    let source = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(error)?;
    let project_present = source
        .lookup(&project)
        .map_err(error)?
        .as_ref()
        .and_then(|row| row.project_fields())
        .is_some_and(|p| p.label == request.package.as_str());
    if !project_present {
        if request.continuation.is_some() {
            return Err(error("continuation expired: project unavailable"));
        }
        return Ok(empty(
            manifest_root,
            None,
            project,
            SqlAttemptState::ProjectNotSelected,
        ));
    }
    let selected = super::super::sql_admission::sql_basis_for_snapshot(snapshot).map_err(error)?;
    let Some(relation) = sql_analysis_relation(snapshot).map_err(error)? else {
        if request.continuation.is_some() {
            return Err(error("continuation expired: SQL root unavailable"));
        }
        return Ok(empty(
            manifest_root,
            None,
            project,
            SqlAttemptState::NoSelectedAnalysis,
        ));
    };
    let sql_root = *relation.root().as_bytes();
    if selected.analysis_root != Some(sql_root) {
        return Err(error("SQL root selection changed"));
    }
    let mut work = Work {
        rows: 0,
        bytes: 0,
        max_rows: request.scan_rows as usize,
        max_bytes: request.scan_bytes as usize,
    };
    let selection = lookup(&relation, &sql_selection_key(project), &mut work)?;
    let attempt = lookup(&relation, &sql_attempt_key(project), &mut work)?;
    let selected_digest = selection
        .as_ref()
        .map(|row| sql_selection_digest(project, row))
        .transpose()
        .map_err(error)?;
    let attempt_state = match attempt {
        None if selection.is_none() => SqlAttemptState::NoSelectedAnalysis,
        Some(SqlAnalysisRecord::Attempt {
            project: owner,
            outcome,
            served_selection,
        }) if owner == project && served_selection == selected_digest => {
            match (outcome, selection.is_some()) {
                (SqlAttemptOutcome::Observed, true) => SqlAttemptState::LatestObserved,
                (SqlAttemptOutcome::Unavailable(reason), true) => {
                    SqlAttemptState::RetainedAfterUnavailable {
                        reason: failure(reason),
                    }
                }
                (SqlAttemptOutcome::Unavailable(reason), false) => SqlAttemptState::Unavailable {
                    reason: failure(reason),
                },
                _ => return Err(error("invalid SQL attempt")),
            }
        }
        _ => return Err(error("SQL selection or attempt mismatch")),
    };
    let (inventory_complete, policy_complete, gaps, implicit_selection) = match &selection {
        Some(SqlAnalysisRecord::Selection {
            project: owner,
            inventory,
            policy_complete,
            gaps,
            implicit,
            ..
        }) if *owner == project => (
            matches!(inventory, SqlInventory::Complete),
            *policy_complete,
            *gaps,
            *implicit,
        ),
        None => (false, false, Default::default(), false),
        _ => return Err(error("invalid SQL selection")),
    };
    let basis = SqlQueryBasis {
        manifest_root,
        sql_root: Some(sql_root),
        project,
        attempt: attempt_state,
        host_health: SqlHostHealth::Unknown,
        inventory_complete,
        policy_complete,
        source_occurrences_complete: false,
        gaps,
        implicit_selection,
        catalog_is_deployed: false,
    };
    if selection.is_none() {
        if request.continuation.is_some() {
            return Err(error("continuation expired: SQL selection unavailable"));
        }
        return Ok(SqlQueryPage {
            host_gaps: Vec::new(),
            basis,
            hits: Vec::new(),
            terminal: SqlQueryTerminal::Unavailable,
        });
    }
    let mut after = match &request.continuation {
        None => None,
        Some(token) => {
            let claims = token.verify(secret).map_err(error)?;
            if claims.manifest_root != manifest_root
                || claims.sql_root != sql_root
                || claims.project != project
                || claims.role != request.role
                || claims.name != request.name
                || claims.kind != request.kind
                || claims.parent != request.parent
                || claims.uses != request.uses
                || claims.page_rows != request.page_rows
                || claims.output_bytes != request.output_bytes
                || claims.scan_rows != request.scan_rows
                || claims.scan_bytes != request.scan_bytes
            {
                return Err(error("continuation basis or filter changed"));
            }
            Some(claims.after)
        }
    };
    let uses = match &request.uses {
        Some(spec) => Some(resolve_uses(&relation, project, spec, &mut work)?),
        None => None,
    };
    let mut hits = Vec::new();
    let mut host_gaps = Vec::new();
    let mut output = serde_json::to_vec(&basis).map_err(error)?.len() + 2048;
    loop {
        if !work.can_scan() {
            let after = after.ok_or_else(|| error("SQL work budget cannot progress"))?;
            return Ok(SqlQueryPage {
                host_gaps,
                basis,
                hits,
                terminal: SqlQueryTerminal::More {
                    continuation: cursor(request, secret, manifest_root, sql_root, project, after)?,
                    work_limited: true,
                },
            });
        }
        let page = relation.page(after.as_ref(), 1).map_err(error)?;
        let Some((key, row)) = page.entries().first() else {
            return Ok(SqlQueryPage {
                host_gaps,
                basis,
                hits,
                terminal: SqlQueryTerminal::Complete,
            });
        };
        work.charge(Some(row))?;
        let lookahead = relation.page(Some(key), 1).map_err(error)?;
        let is_last = if let Some((_, next)) = lookahead.entries().first() {
            work.charge(Some(next))?;
            false
        } else {
            true
        };
        let host_gap = match row {
            SqlAnalysisRecord::Host { project: owner, path, dialect, terminal, .. }
                if *owner == project && matches!(request.role, SqlQueryRole::Gap | SqlQueryRole::Diagnostic) => {
                let reason = match terminal {
                    SqlHostTerminal::Unavailable(reason) => Some(SqlHostGapReason::Unavailable { reason: failure(*reason) }),
                    SqlHostTerminal::NativeError => Some(SqlHostGapReason::NativeError),
                    SqlHostTerminal::Parsed { .. } => None,
                };
                reason.map(|reason| SqlHostGap { path: path.clone(), dialect: *dialect, reason })
            }
            _ => None,
        };
        if let Some(gap) = host_gap {
            let size = serde_json::to_vec(&gap).map_err(error)?.len().saturating_add(1);
            if output.saturating_add(size) > request.output_bytes as usize {
                let prior = after.ok_or_else(|| error("SQL first gap exceeds output budget"))?;
                return Ok(SqlQueryPage { basis, hits, host_gaps, terminal: SqlQueryTerminal::More {
                    continuation: cursor(request, secret, manifest_root, sql_root, project, prior)?,
                    work_limited: false,
                }});
            }
            output += size;
            host_gaps.push(gap);
        }
        let candidate = if let SqlAnalysisRecord::Fact {
            host,
            ordinal,
            fact,
        } = row
        {
            if matches_filter(fact, request, uses.as_ref()) {
                let host_row = lookup(&relation, host, &mut work)?
                    .ok_or_else(|| error("SQL fact host missing"))?;
                let SqlAnalysisRecord::Host {
                    project: owner,
                    path,
                    dialect,
                    input: Some(input),
                    source: Some(source),
                    length: Some(length),
                    producer: Some(producer),
                    settings: Some(settings),
                    context,
                    terminal,
                    ..
                } = host_row
                else {
                    return Err(error("SQL fact host lacks admitted basis"));
                };
                if owner == project {
                    if backend_engine::builtin::sql_host_key(project, &path).map_err(error)?
                        != *host
                    {
                        return Err(error("SQL host key differs from its owner and path"));
                    }
                    if sql_fact_key(
                        *host, dialect, input, source, producer, settings, context, *ordinal,
                    ) != *key
                    {
                        return Err(error("SQL fact key differs from its host basis"));
                    }
                    let terminal = match terminal {
                        SqlHostTerminal::Parsed {
                            source,
                            contextual,
                            sqlite_lineage,
                        } => SqlNativeTerminal::Parsed {
                            source,
                            contextual,
                            sqlite_lineage,
                        },
                        SqlHostTerminal::NativeError => SqlNativeTerminal::NativeError,
                        SqlHostTerminal::Unavailable(_) => {
                            return Err(error("SQL fact has unavailable host"));
                        }
                    };
                    let source_ref = if fact_extent(fact).is_some() {
                        Some(
                            SqlSourceReference::issue(
                                SqlSourceClaims {
                                    manifest_root,
                                    sql_root,
                                    project,
                                    host: *host,
                                    fact: *key,
                                },
                                secret,
                            )
                            .map_err(error)?,
                        )
                    } else {
                        None
                    };
                    Some(SqlQueryHit {
                        id: *key,
                        path,
                        dialect,
                        policy: SqlSourcePolicy::Unknown,
                        host_basis: SqlHostBasis {
                            input,
                            source,
                            length,
                            producer,
                            settings,
                            proposed_context: context,
                            terminal,
                        },
                        fact: fact.clone(),
                        source: source_ref,
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(hit) = candidate {
            let size = serde_json::to_vec(&hit)
                .map_err(error)?
                .len()
                .saturating_add(1);
            if output.saturating_add(size) > request.output_bytes as usize {
                let prior = after.ok_or_else(|| error("SQL first hit exceeds output budget"))?;
                return Ok(SqlQueryPage {
                    host_gaps,
                    basis,
                    hits,
                    terminal: SqlQueryTerminal::More {
                        continuation: cursor(
                            request,
                            secret,
                            manifest_root,
                            sql_root,
                            project,
                            prior,
                        )?,
                        work_limited: false,
                    },
                });
            }
            output += size;
            hits.push(hit);
        }
        after = Some(*key);
        if is_last {
            return Ok(SqlQueryPage {
                host_gaps,
                basis,
                hits,
                terminal: SqlQueryTerminal::Complete,
            });
        }
        if hits.len() + host_gaps.len() >= usize::from(request.page_rows) {
            return Ok(SqlQueryPage {
                host_gaps,
                basis,
                hits,
                terminal: SqlQueryTerminal::More {
                    continuation: cursor(request, secret, manifest_root, sql_root, project, *key)?,
                    work_limited: false,
                },
            });
        }
    }
}

/// Verify exact selected source and both content domains for a native diagnostic.
pub(super) fn read_source(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    reference: &SqlSourceReference,
) -> Result<SqlSourceRead, BuiltinModelError> {
    use super::super::ingest::{SelectedSourceReadError, read_stylo_selected_source};
    use backend_compile::{SourceVerificationError, VerifiedSourceText};
    let claim = reference.verify(secret).map_err(error)?;
    if *snapshot.root().as_bytes() != claim.manifest_root {
        return Ok(SqlSourceRead::Expired);
    }
    let basis = super::super::sql_admission::sql_basis_for_snapshot(snapshot).map_err(error)?;
    if basis.analysis_root != Some(claim.sql_root) {
        return Ok(SqlSourceRead::Expired);
    }
    let Some(relation) = sql_analysis_relation(snapshot).map_err(error)? else {
        return Ok(SqlSourceRead::Expired);
    };
    let source = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(error)?;
    if source
        .lookup(&claim.project)
        .map_err(error)?
        .as_ref()
        .and_then(|r| r.project_fields())
        .is_none()
    {
        return Ok(SqlSourceRead::Expired);
    }
    let Some(SqlAnalysisRecord::Selection {
        project,
        source_root,
        ..
    }) = relation
        .lookup(&sql_selection_key(claim.project))
        .map_err(error)?
    else {
        return Ok(SqlSourceRead::Expired);
    };
    if project != claim.project {
        return Err(error("SQL selection owner mismatch"));
    }
    let Some(SqlAnalysisRecord::Host {
        project: owner,
        path,
        dialect,
        input: Some(input),
        source: Some(source_id),
        length: Some(length),
        producer: Some(producer),
        settings: Some(settings),
        context,
        ..
    }) = relation.lookup(&claim.host).map_err(error)?
    else {
        return Err(error("SQL source host unavailable"));
    };
    if owner != project {
        return Err(error("SQL source host owner mismatch"));
    }
    if backend_engine::builtin::sql_host_key(project, &path).map_err(error)? != claim.host {
        return Err(error("SQL source host key mismatch"));
    }
    let Some(SqlAnalysisRecord::Fact { host, ordinal, fact }) = relation.lookup(&claim.fact).map_err(error)?
    else {
        return Err(error("SQL source fact unavailable"));
    };
    let Some(extent) = fact_extent(&fact) else {
        return Err(error("SQL source fact has no exact extent"));
    };
    if host != claim.host
        || sql_fact_key(
            host, dialect, input, source_id, producer, settings, context, ordinal,
        ) != claim.fact
        || u64::from(extent.end) > length
    {
        return Err(error("SQL source fact ownership mismatch"));
    }
    let length = usize::try_from(length).map_err(error)?;
    let bytes = match read_stylo_selected_source(Path::new(&source_root), &path, length) {
        Ok(bytes) => bytes,
        Err(SelectedSourceReadError::Vanished | SelectedSourceReadError::Unavailable) => {
            return Ok(SqlSourceRead::BytesNotRetained);
        }
        Err(SelectedSourceReadError::TooLarge) => return Ok(SqlSourceRead::SourceChanged),
        Err(SelectedSourceReadError::Invalid) => return Err(error("SQL source path invalid")),
    };
    if bytes.len() != length {
        return Ok(SqlSourceRead::SourceChanged);
    }
    let actual = match VerifiedSourceText::capture(&bytes) {
        Ok(value) => value,
        Err(SourceVerificationError::NonUtf8) => return Ok(SqlSourceRead::SourceChanged),
        Err(error) => return Err(self::error(error)),
    };
    let actual_source = actual.extent(0..bytes.len()).map_err(error)?.source();
    if *actual_source.as_ref() != source_id {
        return Ok(SqlSourceRead::SourceChanged);
    }
    let verified = match VerifiedSourceText::admit_stored(&bytes, input, actual_source) {
        Ok(value) => value,
        Err(
            SourceVerificationError::InputRevisionMismatch
            | SourceVerificationError::SourceIdentityMismatch
            | SourceVerificationError::NonUtf8,
        ) => return Ok(SqlSourceRead::SourceChanged),
        Err(error) => return Err(self::error(error)),
    };
    let line_1 = u32::try_from(
        bytes[..extent.start as usize]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1,
    )
    .map_err(error)?;
    let text = verified
        .extent(extent.start as usize..extent.end as usize)
        .map_err(error)?
        .text()
        .to_owned();
    Ok(SqlSourceRead::Available {
        reference: reference.clone(),
        policy: SqlSourcePolicy::Unknown,
        served_root: source_root,
        path,
        start: extent.start,
        end: extent.end,
        line_1,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::PackageReference;
    use backend_semantic::sql_analysis::SqliteRelationKind;
    #[test]
    fn native_names_are_exact_and_diagnostics_do_not_match_relation_filters() {
        let mut request = SqlQueryRequest {
            package: PackageReference::parse("/tmp/project").unwrap(),
            role: SqlQueryRole::Relation,
            name: Some("Users".into()),
            kind: None,
            parent: None,
            uses: None,
            page_rows: 1,
            output_bytes: 131_072,
            scan_rows: 7,
            scan_bytes: 229_376,
            continuation: None,
        };
        let fact = SqlFact::SqliteRelation {
            name: "users".into(),
            relation_kind: SqliteRelationKind::Table,
        };
        assert!(!matches_filter(&fact, &request, None));
        request.name = Some("users".into());
        assert!(matches_filter(&fact, &request, None));
        request.role = SqlQueryRole::Diagnostic;
        request.name = None;
        assert!(!matches_filter(&fact, &request, None));
    }
}
