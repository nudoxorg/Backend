//! Declaration discovery from the selected native SQL relation.
//! These rows are a name projection; detailed binding and source handles stay
//! on sql-query. Files rejected by the worker never produce declarations.
use super::super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use backend_engine::{DeclarationKind, Row, RowId, ViewRoot, WorkspaceSnapshot};
use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlFact, SqlObjectKind};

// Requested-allocation quota allowance per allocation. This is not a bound
// on allocator overhead or process RSS.
const SQL_ALLOCATION_QUOTA_PER_ALLOCATION: usize = 64;

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
        // Cover the projection and the subsequently retained presentation
        // strings, including formatting growth and allocation quota credits.
        .and_then(|n| n.checked_mul(16))
        .and_then(|n| n.checked_add(1024))
        .ok_or_else(|| BuiltinModelError("SQL projection byte overflow".into()))
}

/// Shared projection identity for the view row and native query evidence.
pub(super) fn projection(
    label: &str,
    path: &str,
    ordinal: u32,
    definition: &backend_semantic::sql_analysis::SqlDefinition,
) -> Result<(String, String, String, &'static str), BuiltinModelError> {
    let name = definition.head_name.as_deref().unwrap_or(&definition.name);
    let parent = definition
        .head_parent
        .as_deref()
        .or(definition.parent.as_deref());
    let column_parent = (definition.kind == SqlObjectKind::Column).then_some(parent).flatten();
    let display_len = name.len().checked_add(column_parent.map_or(0, |p| p.len().saturating_add(1)))
        .ok_or_else(|| BuiltinModelError("SQL projection byte overflow".into()))?;
    let mut display = String::with_capacity(display_len);
    if let Some(parent) = column_parent { display.push_str(parent); display.push('.'); }
    display.push_str(name);
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
    let coordinate_len = label.len().checked_add(path.len()).and_then(|n| n.checked_add(name.len()))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(column_parent.map_or(0, |p| p.len().saturating_add(2))))
        .ok_or_else(|| BuiltinModelError("SQL projection byte overflow".into()))?;
    let mut coordinate = String::with_capacity(coordinate_len);
    coordinate.push_str(label); coordinate.push_str("::"); coordinate.push_str(path); coordinate.push_str("::");
    if let Some(parent) = column_parent { coordinate.push_str(parent); coordinate.push_str("::"); }
    coordinate.push_str(name);
    let mut preimage = String::with_capacity(coordinate_len.checked_add(15)
        .ok_or_else(|| BuiltinModelError("SQL projection byte overflow".into()))?);
    preimage.push_str(&coordinate); preimage.push_str("\0sql:");
    use std::fmt::Write as _;
    write!(&mut preimage, "{ordinal}").map_err(|_| BuiltinModelError("SQL projection formatting failed".into()))?;
    Ok((coordinate, preimage, display, kind))
}

pub(super) fn append(
    snapshot: &WorkspaceSnapshot,
    initial: &ViewRoot,
    sources: &IndexedSources,
    rows: &mut Vec<Row>,
) -> Result<(), BuiltinModelError> {
    use super::super::row_admission::{ExactText, ProjectedRowPlan, ProjectedRowTexts, RequestedLayoutBudget};
    let prior = super::super::admitted_view_bytes(rows)?
        .checked_add(rows.capacity().saturating_sub(rows.len()).checked_mul(std::mem::size_of::<Row>())
            .ok_or_else(|| BuiltinModelError("SQL view row buffer overflow".into()))?)
        .and_then(|n| n.checked_add(if rows.capacity() == 0 { 0 } else { SQL_ALLOCATION_QUOTA_PER_ALLOCATION }))
        .ok_or_else(|| BuiltinModelError("SQL view quota overflow".into()))?;
    let mut budget = RequestedLayoutBudget::new(prior, super::super::MAX_REBUILD_BYTES)
        .map_err(|error| BuiltinModelError(error.into()))?;
    let remaining = super::super::MAX_REBUILD_BYTES.checked_sub(prior)
        .ok_or_else(|| BuiltinModelError("SQL view exceeds byte bound".into()))?;
    let (selected, catalog_bytes) = super::super::sql_catalog::load_projects_bounded(
        snapshot, sources.projects.keys(), &|| false, remaining, SQL_ALLOCATION_QUOTA_PER_ALLOCATION,
    ).map_err(|error| BuiltinModelError(error.to_string()))?;
    let added = selected.values().try_fold(0usize, |n, sql| {
        let count = sql.facts.iter().filter(|(_, _, fact)| matches!(fact,
            SqlFact::Definition(d) if d.live_at_head && matches!(d.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn))).count();
        n.checked_add(count).ok_or_else(|| BuiltinModelError("SQL row count overflow".into()))
    })?;
    let needed = rows.len().checked_add(added).filter(|n| *n <= MAX_REBUILD_PACKAGES)
        .ok_or_else(|| BuiltinModelError("SQL declarations exceed the view row bound".into()))?;
    if needed > rows.capacity() {
        // The old buffer remains charged while the complete requested new
        // buffer is admitted. Keeping both charges afterwards is conservative.
        let new_buffer = needed.checked_mul(std::mem::size_of::<Row>())
            .and_then(|n| n.checked_add(SQL_ALLOCATION_QUOTA_PER_ALLOCATION))
            .ok_or_else(|| BuiltinModelError("SQL row buffer overflow".into()))?;
        budget.retain(new_buffer, catalog_bytes).map_err(|e| BuiltinModelError(e.into()))?;
        rows.try_reserve_exact(added).map_err(|_| BuiltinModelError("SQL row buffer allocation failed".into()))?;
    }
    for (project, indexed) in &sources.projects {
        let Some(sql) = selected.get(project) else { continue; };
        for (host, ordinal, fact) in &sql.facts {
            let SqlFact::Definition(definition) = fact else { continue; };
            if !definition.live_at_head || !matches!(definition.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn) { continue; }
            let path = &sql.hosts[*host].path;
            let name = definition.head_name.as_deref().unwrap_or(&definition.name);
            let parent = if definition.kind == SqlObjectKind::Column {
                definition.head_parent.as_deref().or(definition.parent.as_deref())
            } else { None };
            let prefix = parent.unwrap_or("");
            let coordinate_sep = if parent.is_some() { "::" } else { "" };
            let display_sep = if parent.is_some() { "." } else { "" };
            let kind = match definition.kind {
                SqlObjectKind::Column => "column", SqlObjectKind::Table => "table", SqlObjectKind::View => "view",
                SqlObjectKind::Index => "index", SqlObjectKind::Trigger => "trigger", SqlObjectKind::Function => "function",
                SqlObjectKind::Schema => "schema", SqlObjectKind::Type => "type", SqlObjectKind::Sequence => "sequence",
            };
            let label = ExactText::measure(format_args!("{}::{path}::{prefix}{coordinate_sep}{name}", indexed.label)).map_err(|e| BuiltinModelError(e.into()))?;
            let identity = ExactText::measure(format_args!("{}::{path}::{prefix}{coordinate_sep}{name}\0sql:{ordinal}", indexed.label)).map_err(|e| BuiltinModelError(e.into()))?;
            let signature = ExactText::measure(format_args!("SQL {kind} {prefix}{display_sep}{name}")).map_err(|e| BuiltinModelError(e.into()))?;
            let document = ExactText::measure(format_args!("SQL {kind} in {path}; use sql-query for binding, migration history and checked source")).map_err(|e| BuiltinModelError(e.into()))?;
            let plan = ProjectedRowPlan::new(label.len(), identity.len(), signature.len(), document.len())
                .map_err(|e| BuiltinModelError(e.into()))?;
            let retained = plan.retained_payload_requested_bytes().checked_add(
                plan.allocation_count().checked_mul(SQL_ALLOCATION_QUOTA_PER_ALLOCATION)
                    .ok_or_else(|| BuiltinModelError("SQL row quota overflow".into()))?)
                .ok_or_else(|| BuiltinModelError("SQL row quota overflow".into()))?;
            budget.retain(retained, catalog_bytes).map_err(|e| BuiltinModelError(e.into()))?;
            let label = label.allocate(format_args!("{}::{path}::{prefix}{coordinate_sep}{name}", indexed.label)).map_err(|e| BuiltinModelError(e.into()))?;
            let identity = identity.allocate(format_args!("{}::{path}::{prefix}{coordinate_sep}{name}\0sql:{ordinal}", indexed.label)).map_err(|e| BuiltinModelError(e.into()))?;
            let signature = signature.allocate(format_args!("SQL {kind} {prefix}{display_sep}{name}")).map_err(|e| BuiltinModelError(e.into()))?;
            let document = document.allocate(format_args!("SQL {kind} in {path}; use sql-query for binding, migration history and checked source")).map_err(|e| BuiltinModelError(e.into()))?;
            let id = RowId::Symbol(backend_engine::symbol_key(&identity));
            let row = plan.build(id, initial.basis(), indexed.package,
                if definition.kind == SqlObjectKind::Column { DeclarationKind::Field } else { DeclarationKind::Unknown },
                ProjectedRowTexts { label, identity, signature, document }).map_err(|e| BuiltinModelError(e.into()))?;
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
    let (selected, catalog_bytes) = super::super::sql_catalog::load_projects_bounded(
        snapshot,
        sources.projects.keys(),
        &|| cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)),
        remaining_bytes,
        SQL_ALLOCATION_QUOTA_PER_ALLOCATION,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))?;
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
                projection(&indexed.label, &host.path, *ordinal, definition)?;
            let row_id = RowId::Symbol(backend_engine::symbol_key(&preimage));
            let name = definition
                .head_name
                .as_deref()
                .unwrap_or(&definition.name)
                .to_owned();
            facts.push(SemanticQueryFact::new(
                SemanticQueryEvidence::SqlNative(SqlNativeQueryEvidence::new(indexed.package, sql.selection_digest, sql.inventory_digest, host.key, input, source, producer, settings, host.fact_digest, *ordinal, preimage)),
                SemanticQueryPresentation { id: row_id.stable_key(), kind: if definition.kind == SqlObjectKind::Column { "field" } else { "unknown" }.to_owned(), coordinate, name,
                    signature: Some(format!("SQL {kind} {display}")), documentation: format!("SQL {kind} in {}; use sql-query for binding, migration history and checked source", host.path),
                    score: None, project: Some(super::identity::query_package_id(indexed.package)), parent: None, related: Box::new([]) },
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::sql_analysis::{SqlByteRange, SqlDefinition};

    #[test]
    fn long_parent_projection_fits_precharged_quota() {
        let definition = SqlDefinition {
            kind: SqlObjectKind::Column,
            action: SqlDefinitionAction::Create,
            conditional: false,
            schema: None,
            name: "x".into(),
            parent: Some("p".repeat(4096)),
            parent_schema: None,
            name_extent: SqlByteRange { start: 0, end: 1 },
            extent: SqlByteRange { start: 0, end: 1 },
            detail: None,
            statement: 0,
            entity: None,
            parent_entity: None,
            live_at_head: true,
            head_name: None,
            head_parent: None,
        };
        let bound = projection_bound("p", "s", &definition).unwrap();
        let (coordinate, preimage, display, _) = projection("p", "s", u32::MAX, &definition).unwrap();
        let requested = coordinate.capacity() + preimage.capacity() + display.capacity()
            + 3 * SQL_ALLOCATION_QUOTA_PER_ALLOCATION;
        assert!(requested <= bound, "projection requested {requested}, quota {bound}");
        assert!(preimage.ends_with("\0sql:4294967295"));
    }

    #[test]
    fn long_path_query_presentation_fits_precharged_quota() {
        use backend_extension_trustfall::{SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation, SqlNativeQueryEvidence};
        let definition = SqlDefinition {
            kind: SqlObjectKind::Column, action: SqlDefinitionAction::Create,
            conditional: false, schema: None, name: "x".into(), parent: Some("t".into()),
            parent_schema: None, name_extent: SqlByteRange { start: 0, end: 1 },
            extent: SqlByteRange { start: 0, end: 1 }, detail: None, statement: 0,
            entity: None, parent_entity: None, live_at_head: true, head_name: None, head_parent: None,
        };
        let path = vec!["d".repeat(250); 12].join("/");
        let bound = projection_bound("p", &path, &definition).unwrap();
        let (coordinate, preimage, display, kind) = projection("p", &path, 4, &definition).unwrap();
        let package = backend_engine::package_key("p");
        let id = RowId::Symbol(backend_engine::symbol_key(&preimage)).stable_key();
        let fact = SemanticQueryFact::new(
            SemanticQueryEvidence::SqlNative(SqlNativeQueryEvidence::new(package, [8;32], [9;32], [1;32], [2;32], [3;32], [4;32], [5;32], [6;32], 4, preimage)),
            SemanticQueryPresentation { id, kind: "field".into(), coordinate, name: "x".into(),
                signature: Some(format!("SQL {kind} {display}")),
                documentation: format!("SQL {kind} in {path}; use sql-query for binding, migration history and checked source"),
                score: None, project: None, parent: None, related: Box::new([]) },
        );
        let measured = backend_extension_trustfall::measure_semantic_query_facts(
            &[fact], 1, backend_extension_trustfall::Limits::default(),
        ).unwrap();
        assert!(measured + display.capacity() <= bound, "presentation quota {bound}, measured {measured}");
    }

    #[test]
    fn row_buffer_exact_growth_requests_precharged_layout() {
        let (initial, _) = super::super::super::initial_view().unwrap();
        let mut rows = Vec::with_capacity(1);
        rows.push(Row::new(RowId::Symbol(backend_engine::symbol_key("row")), initial.basis(), "row"));
        let needed = rows.len() + 7;
        let measured = allocation_counter::measure(|| rows.try_reserve_exact(7).unwrap());
        assert!(rows.capacity() >= needed);
        assert_eq!(measured.count_total, 1);
        assert_eq!(measured.bytes_total, (needed * std::mem::size_of::<Row>()) as u64);
    }
}
