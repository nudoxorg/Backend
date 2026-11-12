//! Declaration discovery from the selected native SQL relation.
//! These rows are a name projection; detailed binding and source handles stay
//! on sql-query. Files rejected by the worker never produce declarations.
use super::super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, WorkspaceSnapshot};
use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlFact, SqlObjectKind};

/// Conservative byte precharge before projection allocates owned strings.
fn projection_bound(
    label: &str,
    path: &str,
    definition: &backend_semantic::sql_analysis::SqlDefinition,
) -> Result<usize, BuiltinModelError> {
    let name = definition.head_name.as_deref().unwrap_or(&definition.name);
    let parent = definition
        .head_parent
        .as_deref()
        .or(definition.parent.as_deref())
        .unwrap_or("");
    label
        .len()
        .checked_add(path.len())
        .and_then(|n| n.checked_add(name.len()))
        .and_then(|n| n.checked_add(parent.len()))
        .and_then(|n| n.checked_add(32))
        .and_then(|n| n.checked_mul(5))
        .and_then(|n| n.checked_add(512))
        .ok_or_else(|| BuiltinModelError("SQL projection byte overflow".into()))
}

/// Shared projection identity for the view row and native query evidence.
pub(super) fn projection(
    label: &str,
    path: &str,
    ordinal: u32,
    definition: &backend_semantic::sql_analysis::SqlDefinition,
) -> (String, String, String, &'static str) {
    let name = definition.head_name.as_deref().unwrap_or(&definition.name);
    let parent = definition
        .head_parent
        .as_deref()
        .or(definition.parent.as_deref());
    let display = if definition.kind == SqlObjectKind::Column {
        parent.map_or_else(|| name.to_owned(), |parent| format!("{parent}.{name}"))
    } else {
        name.to_owned()
    };
    let kind = match definition.kind {
        SqlObjectKind::Column => "column",
        SqlObjectKind::Table => "table",
        SqlObjectKind::View => "view",
        SqlObjectKind::Index => "index",
        SqlObjectKind::Trigger => "trigger",
        SqlObjectKind::Function => "function",
        SqlObjectKind::Schema => "schema",
        SqlObjectKind::Type => "type",
        SqlObjectKind::Sequence => "sequence",
    };
    let coordinate = if definition.kind == SqlObjectKind::Column {
        parent.map_or_else(
            || format!("{label}::{path}::{name}"),
            |parent| format!("{label}::{path}::{parent}::{name}"),
        )
    } else {
        format!("{label}::{path}::{name}")
    };
    let preimage = format!("{coordinate}\0sql:{ordinal}");
    (coordinate, preimage, display, kind)
}

pub(super) fn append(
    snapshot: &WorkspaceSnapshot,
    initial: &ViewRoot,
    sources: &IndexedSources,
    rows: &mut Vec<Row>,
) -> Result<(), BuiltinModelError> {
    let mut bytes = super::super::admitted_view_bytes(rows)?;
    let projects = sources.projects.keys().copied().collect();
    let (selected, catalog_bytes) = super::super::sql_catalog::load_projects_bounded(
        snapshot,
        &projects,
        &|| false,
        super::super::MAX_REBUILD_BYTES,
    )
    .map_err(BuiltinModelError)?;
    for (project, indexed) in &sources.projects {
        let Some(sql) = selected.get(project) else {
            continue;
        };
        for (host, ordinal, fact) in &sql.facts {
            let SqlFact::Definition(definition) = fact else {
                continue;
            };
            if !definition.live_at_head
                || !matches!(
                    definition.action,
                    SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn
                )
            {
                continue;
            }
            if rows.len() >= MAX_REBUILD_PACKAGES {
                return Err(BuiltinModelError(
                    "SQL declarations exceed the view row bound".into(),
                ));
            }
            let path = &sql.hosts[*host].path;
            let prospective = bytes
                .checked_add(catalog_bytes)
                .and_then(|n| {
                    n.checked_add(projection_bound(&indexed.label, path, definition).ok()?)
                })
                .ok_or_else(|| BuiltinModelError("SQL projection byte overflow".into()))?;
            super::super::finish_admitted_bytes(prospective)?;
            let (coordinate, preimage, display, kind) =
                projection(&indexed.label, path, *ordinal, definition);
            let row = Row::in_package(RowId::Symbol(backend_engine::symbol_key(&preimage)), initial.basis(), indexed.package, coordinate)
                .try_with_identity_preimage(&preimage)
                .map_err(|error| BuiltinModelError(format!("SQL discovery identity: {error}")))?
                .with_kind(if definition.kind == SqlObjectKind::Column { DeclarationKind::Field } else { DeclarationKind::Unknown })
                .with_signature(format!("SQL {kind} {display}"))
                .with_document(vec![Fragment::Text(format!("SQL {kind} in {path}; use sql-query for binding, migration history and checked source"))]);
            bytes = bytes
                .checked_add(
                    super::super::row_admission_bytes(&row)
                        .ok_or_else(|| BuiltinModelError("SQL discovery byte overflow".into()))?,
                )
                .ok_or_else(|| BuiltinModelError("SQL discovery byte overflow".into()))?;
            super::super::finish_admitted_bytes(bytes)?;
            rows.push(row);
        }
    }
    Ok(())
}

/// Project the same selected SQL declarations into the evidence-gated query
/// corpus. The captured host basis is retained without compiler authority.
pub(super) fn append_query(
    snapshot: &WorkspaceSnapshot,
    sources: &IndexedSources,
    facts: &mut Vec<backend_extension_trustfall::SemanticQueryFact>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    remaining_bytes: usize,
) -> Result<(), BuiltinModelError> {
    use backend_extension_trustfall::{
        SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation, SqlNativeQueryEvidence,
    };
    let projects = sources.projects.keys().copied().collect();
    let (selected, catalog_bytes) = super::super::sql_catalog::load_projects_bounded(
        snapshot,
        &projects,
        &|| cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)),
        remaining_bytes,
    )
    .map_err(BuiltinModelError)?;
    let mut projected_bytes = catalog_bytes;
    let added_rows = selected.values().try_fold(0usize, |count, sql| {
        let rows = sql.facts.iter().filter(|(_, _, fact)| matches!(fact, SqlFact::Definition(definition) if definition.live_at_head && matches!(definition.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn))).count();
        count.checked_add(rows).ok_or_else(|| BuiltinModelError("SQL query row overflow".into()))
    })?;
    let needed = facts
        .len()
        .checked_add(added_rows)
        .filter(|n| *n <= MAX_REBUILD_PACKAGES)
        .ok_or_else(|| BuiltinModelError("SQL query facts exceed row bound".into()))?;
    if needed > facts.capacity() {
        // During reserve both old and new buffers can coexist; the previous
        // buffer was charged by the caller, so charge the entire new one.
        let new_buffer = needed
            .checked_mul(std::mem::size_of::<
                backend_extension_trustfall::SemanticQueryFact,
            >())
            .ok_or_else(|| BuiltinModelError("SQL query allocation overflow".into()))?;
        projected_bytes = projected_bytes
            .checked_add(new_buffer)
            .filter(|n| *n <= remaining_bytes)
            .ok_or_else(|| BuiltinModelError("SQL query facts exceed byte bound".into()))?;
        facts
            .try_reserve_exact(added_rows)
            .map_err(|_| BuiltinModelError("SQL query allocation failed".into()))?;
        if facts.capacity() > needed {
            let extra = (facts.capacity() - needed)
                .checked_mul(std::mem::size_of::<
                    backend_extension_trustfall::SemanticQueryFact,
                >())
                .ok_or_else(|| BuiltinModelError("SQL query allocation overflow".into()))?;
            projected_bytes = projected_bytes
                .checked_add(extra)
                .filter(|n| *n <= remaining_bytes)
                .ok_or_else(|| BuiltinModelError("SQL query facts exceed byte bound".into()))?;
        }
    }
    for (project, indexed) in &sources.projects {
        super::super::check_search_preparation(cancelled, "sql-query-project")?;
        let Some(sql) = selected.get(project) else {
            continue;
        };
        for (host_index, ordinal, fact) in &sql.facts {
            super::super::check_search_preparation(cancelled, "sql-query-fact")?;
            let SqlFact::Definition(definition) = fact else {
                continue;
            };
            if !definition.live_at_head
                || !matches!(
                    definition.action,
                    SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn
                )
            {
                continue;
            }
            if facts.len() >= MAX_REBUILD_PACKAGES {
                return Err(BuiltinModelError("SQL query facts exceed row bound".into()));
            }
            let host = &sql.hosts[*host_index];
            let (Some(input), Some(source), Some(producer), Some(settings)) =
                (host.input, host.source, host.producer, host.settings)
            else {
                return Err(BuiltinModelError(
                    "selected SQL fact lacks its native host basis".into(),
                ));
            };
            if !matches!(
                host.terminal,
                backend_engine::builtin::SqlHostTerminal::Parsed { .. }
            ) {
                return Err(BuiltinModelError(
                    "selected SQL fact lacks parsed host terminal".into(),
                ));
            }
            projected_bytes = projected_bytes
                .checked_add(projection_bound(&indexed.label, &host.path, definition)?)
                .filter(|n| *n <= remaining_bytes)
                .ok_or_else(|| BuiltinModelError("SQL query facts exceed byte bound".into()))?;
            let (coordinate, preimage, display, kind) =
                projection(&indexed.label, &host.path, *ordinal, definition);
            let row_id = RowId::Symbol(backend_engine::symbol_key(&preimage));
            let name = definition
                .head_name
                .as_deref()
                .unwrap_or(&definition.name)
                .to_owned();
            facts.push(SemanticQueryFact::new(
                SemanticQueryEvidence::SqlNative(SqlNativeQueryEvidence::new(indexed.package, host.key, input, source, producer, settings, host.fact_digest, *ordinal, preimage)),
                SemanticQueryPresentation { id: row_id.stable_key(), kind: if definition.kind == SqlObjectKind::Column { "field" } else { "unknown" }.to_owned(), coordinate, name,
                    signature: Some(format!("SQL {kind} {display}")), documentation: format!("SQL {kind} in {}; use sql-query for binding, migration history and checked source", host.path),
                    score: None, project: Some(super::identity::query_package_id(indexed.package)), parent: None, related: Box::new([]) },
            ));
        }
    }
    Ok(())
}
