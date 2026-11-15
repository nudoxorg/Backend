//! Cold reopen support for the SQL acceptance journey (tests only).
//!
//! Opens a persisted workspace with the production dispatcher, relation
//! registry and a receipt context bound to its own leased store, which makes the
//! owner replay every persisted transition, SQL receipts included, before any
//! row is read.
use backend_library::sql_query::{SqlQueryRequest, SqlQueryRole};
use backend_semantic::sql_analysis::{SqlDefinitionAction, SqlFact, SqlObjectKind};
use std::{error::Error, path::Path, sync::Arc};

/// The product statement authority the adapter fixture runs with.
const PRODUCT_SECRET: [u8; 32] = [0x67; 32];

pub(super) struct Reopened {
    /// Whether a selected SQL analysis exists in the reopened workspace.
    pub(super) analysis: bool,
    pub(super) tables: Vec<String>,
    pub(super) balance_dropped: bool,
    pub(super) sql_root_level: u16,
    /// Every row of the selected SQL relation, whichever project owns it.
    pub(super) sql_rows: usize,
    /// Hosts which are SQL embedded in application code.
    pub(super) embedded_hosts: usize,
}

/// Count the rows (and embedded hosts) of the selected SQL relation.
pub(super) fn count_rows(snapshot: &backend_engine::WorkspaceSnapshot) -> Result<(usize, usize), Box<dyn Error>> {
    use backend_engine::builtin::{SqlAnalysisRecord, sql_analysis_relation};
    let Some(relation) = sql_analysis_relation(snapshot).map_err(|e| e.to_string())? else { return Ok((0, 0)) };
    let (mut rows, mut embedded) = (0, 0);
    let mut after = None;
    loop {
        let page = relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS).map_err(|e| e.to_string())?;
        for (_, row) in page.entries() {
            rows += 1;
            if matches!(row, SqlAnalysisRecord::Host { path, .. } if path.ends_with(super::sql_embedded::VIRTUAL_SUFFIX)) {
                embedded += 1;
            }
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            _ => return Ok((rows, embedded)),
        }
    }
}

pub(super) fn reopen_and_query(workspace: &Path, project: &Path) -> Result<Reopened, Box<dyn Error>> {
    let profile = super::profile_descriptor(super::BuiltinProfile::Product)?;
    let dispatcher = super::builtin_dispatcher(
        Some(super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )?;
    let registry = super::product_relation_registry()?;
    let receipt = Arc::new(super::owner_receipt_context::OwnerReceiptContext::new(
        PRODUCT_SECRET,
        workspace.to_path_buf(),
    ));
    let daemon = crate::Locald::open_with_dispatcher_and_registry(
        workspace,
        super::BuiltinModel::with_receipt_context(Some(receipt)),
        super::genesis()?,
        dispatcher,
        backend_engine::DaemonConfig::default(),
        registry,
    )?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let (sql_rows, embedded_hosts) = count_rows(&snapshot)?;
    let package = backend_library::PackageReference::parse(project.to_str().ok_or("project path")?.to_owned())
        .map_err(|e| format!("package: {e:?}"))?;
    let page = |kind: SqlObjectKind| -> Result<Option<Vec<backend_library::sql_query::SqlQueryHit>>, Box<dyn Error>> {
        let mut request = SqlQueryRequest {
            package: package.clone(),
            role: SqlQueryRole::Definition,
            name: None,
            kind: Some(kind),
            parent: None,
            uses: None,
            page_rows: 128,
            output_bytes: 4_194_304,
            scan_rows: 262_144,
            scan_bytes: 64 * 1024 * 1024,
            continuation: None,
        };
        let mut hits = Vec::new();
        loop {
            let page = super::commands::sql_query::query(&snapshot, &PRODUCT_SECRET, &request)?;
            hits.extend(page.hits);
            match page.terminal {
                backend_library::sql_query::SqlQueryTerminal::More { continuation, .. } => request.continuation = Some(continuation),
                backend_library::sql_query::SqlQueryTerminal::Complete => return Ok(Some(hits)),
                backend_library::sql_query::SqlQueryTerminal::Unavailable => return Ok(None),
            }
        }
    };
    let Some(table_hits) = page(SqlObjectKind::Table)? else {
        return Ok(Reopened { analysis: false, tables: Vec::new(), balance_dropped: false, sql_root_level: 0, sql_rows, embedded_hosts });
    };
    // Exercise the actual selected catalog after a cold owner reopen. Tiny
    // credit and cancellation must not produce a partial catalog.
    let project_key = backend_library::package_key(package.as_str()).to_bytes();
    if super::sql_catalog::load_projects_bounded(&snapshot, std::iter::once(&project_key), &|| false, 0, 64).is_ok() {
        return Err("zero-credit SQL catalog unexpectedly admitted".into());
    }
    if super::sql_catalog::load_projects_bounded(&snapshot, std::iter::once(&project_key), &|| true, 64 * 1024 * 1024, 64).is_ok() {
        return Err("cancelled SQL catalog unexpectedly admitted".into());
    }
    let (catalog, _) = super::sql_catalog::load_projects_bounded(
        &snapshot, std::iter::once(&project_key), &|| false, 64 * 1024 * 1024, 64,
    )?;
    let selected = catalog.get(&project_key).ok_or("cold selected SQL catalog missing")?;
    let mut expected_live = table_hits.iter().filter_map(|hit| match &hit.fact {
        SqlFact::Definition(d) if d.live_at_head && matches!(d.action, SqlDefinitionAction::Create) => Some(d.head_name.as_deref().unwrap_or(&d.name).to_owned()),
        _ => None,
    }).collect::<Vec<_>>();
    let mut actual_live = selected.facts.iter().filter_map(|(host, _, fact)| match fact {
        SqlFact::Definition(d) if d.kind == SqlObjectKind::Table && d.live_at_head && matches!(d.action, SqlDefinitionAction::Create) => {
            let basis = &selected.hosts[*host];
            assert!(basis.input.is_some() && basis.source.is_some() && basis.producer.is_some() && basis.settings.is_some());
            Some(d.head_name.as_deref().unwrap_or(&d.name).to_owned())
        }
        _ => None,
    }).collect::<Vec<_>>();
    expected_live.sort();
    actual_live.sort();
    if expected_live != actual_live {
        return Err("cold selected catalog table identities differ from SQL query".into());
    }
    let mut tables = table_hits
        .into_iter()
        .filter_map(|hit| match hit.fact {
            SqlFact::Definition(d) if matches!(d.action, SqlDefinitionAction::Create) => d.head_name,
            _ => None,
        })
        .collect::<Vec<_>>();
    tables.sort();
    let balance_dropped = page(SqlObjectKind::Column)?.unwrap_or_default().into_iter().any(|hit| {
        matches!(hit.fact, SqlFact::Definition(d) if d.name == "balance" && matches!(d.action, SqlDefinitionAction::AddColumn | SqlDefinitionAction::Create) && !d.live_at_head)
    });
    let sql_root_level = backend_engine::builtin::sql_analysis_relation(&snapshot)?
        .ok_or("cold SQL relation missing")?.root_node()?.level();
    Ok(Reopened { analysis: true, tables, balance_dropped, sql_root_level, sql_rows, embedded_hosts })
}
