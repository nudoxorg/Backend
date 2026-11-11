//! Declaration discovery from the selected native SQL relation.
//! These rows are a name projection; detailed binding and source handles stay
//! on sql-query. Files rejected by the worker never produce declarations.
use super::super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, WorkspaceSnapshot};
use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlFact, SqlObjectKind};

pub(super) fn append(
    snapshot: &WorkspaceSnapshot,
    initial: &ViewRoot,
    sources: &IndexedSources,
    rows: &mut Vec<Row>,
) -> Result<(), BuiltinModelError> {
    for (project, indexed) in &sources.projects {
        let Some(sql) = super::super::sql_catalog::load_project(snapshot, *project, &|| false)
            .map_err(BuiltinModelError)? else { continue };
        for (host, ordinal, fact) in &sql.facts {
            let SqlFact::Definition(definition) = fact else { continue };
            if !definition.live_at_head || !matches!(definition.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn) {
                continue;
            }
            if rows.len() >= MAX_REBUILD_PACKAGES {
                return Err(BuiltinModelError("SQL declarations exceed the view row bound".into()));
            }
            let name = definition.head_name.as_deref().unwrap_or(&definition.name);
            let parent = definition.head_parent.as_deref().or(definition.parent.as_deref());
            let display = if definition.kind == SqlObjectKind::Column {
                parent.map_or_else(|| name.to_owned(), |parent| format!("{parent}.{name}"))
            } else { name.to_owned() };
            let kind = match definition.kind {
                SqlObjectKind::Column => "column", SqlObjectKind::Table => "table",
                SqlObjectKind::View => "view", SqlObjectKind::Index => "index",
                SqlObjectKind::Trigger => "trigger", SqlObjectKind::Function => "function",
                SqlObjectKind::Schema => "schema", SqlObjectKind::Type => "type",
                SqlObjectKind::Sequence => "sequence",
            };
            let path = &sql.hosts[*host].path;
            // The source ordinal is an admitted SQL fact identity. It keeps
            // same-name definitions distinct without inventing source lines.
            let coordinate = format!("{}::{path}::sql:{ordinal}::{display}", indexed.label);
            rows.push(Row::in_package(RowId::Symbol(backend_engine::symbol_key(&coordinate)), initial.basis(), indexed.package, coordinate)
                .with_kind(if definition.kind == SqlObjectKind::Column { DeclarationKind::Field } else { DeclarationKind::Unknown })
                .with_signature(format!("SQL {kind} {display}"))
                .with_document(vec![Fragment::Text(format!("SQL {kind} in {path}; use sql-query for binding, migration history and checked source"))]));
        }
    }
    Ok(())
}
