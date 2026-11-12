//! Declaration discovery from the selected native SQL relation.
//! These rows are a name projection; detailed binding and source handles stay
//! on sql-query. Files rejected by the worker never produce declarations.
use super::super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, WorkspaceSnapshot};
use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlFact, SqlObjectKind};

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
    for (project, indexed) in &sources.projects {
        let Some(sql) = super::super::sql_catalog::load_project(snapshot, *project, &|| false)
            .map_err(BuiltinModelError)?
        else {
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
) -> Result<(), BuiltinModelError> {
    use backend_extension_trustfall::{
        SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation, SqlNativeQueryEvidence,
    };
    let mut projected_bytes = 0usize;
    for (project, indexed) in &sources.projects {
        super::super::check_search_preparation(cancelled, "sql-query-project")?;
        let Some(sql) = super::super::sql_catalog::load_project(snapshot, *project, &|| {
            cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire))
        })
        .map_err(BuiltinModelError)?
        else {
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
            let (coordinate, preimage, display, kind) =
                projection(&indexed.label, &host.path, *ordinal, definition);
            // Charge retained strings and a conservative fixed evidence/row
            // allowance before allocating each query fact.
            let row_bytes = coordinate
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(preimage.len()))
                .and_then(|n| n.checked_add(display.len()))
                .and_then(|n| n.checked_add(host.path.len()))
                .and_then(|n| n.checked_add(512))
                .ok_or_else(|| BuiltinModelError("SQL query bytes overflow".into()))?;
            projected_bytes = projected_bytes
                .checked_add(row_bytes)
                .filter(|n| *n <= super::super::MAX_REBUILD_BYTES)
                .ok_or_else(|| BuiltinModelError("SQL query facts exceed byte bound".into()))?;
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
