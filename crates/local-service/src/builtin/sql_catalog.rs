//! Bounded read of one project's selected SQL analysis.
//!
//! Gathers the selection, hosts and facts of a project from the selected SQL
//! relation in one pass. Anything that is not a clean, complete read comes back
//! as an error or as gaps; callers must never treat a partial read as empty.
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{
        SqlAnalysisRecord, SqlHostTerminal, SqlInventory, sql_analysis_relation, sql_selection_key,
    },
};
use backend_semantic::sql_analysis::{SqlDialect, SqlFact, SqlGapCounts};

const MAX_ROWS: usize = 262_144;

pub(super) struct SqlHost {
    pub(super) key: [u8; 32],
    pub(super) path: String,
    pub(super) dialect: SqlDialect,
    pub(super) source: Option<[u8; 32]>,
    pub(super) length: Option<u64>,
    pub(super) terminal: SqlHostTerminal,
}

pub(super) struct ProjectSql {
    /// Canonical project directory the selection was captured under.
    pub(super) root: String,
    pub(super) inventory_complete: bool,
    pub(super) implicit: bool,
    pub(super) gaps: SqlGapCounts,
    pub(super) hosts: Vec<SqlHost>,
    /// `(host index, ordinal, fact)` in host then ordinal order.
    pub(super) facts: Vec<(usize, u32, SqlFact)>,
}

/// Read the selected SQL analysis of `project`. `Ok(None)` means the snapshot
/// holds no SQL selection for it, which is unknown, not "no SQL".
pub(super) fn load_project(
    snapshot: &WorkspaceSnapshot,
    project: [u8; 32],
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<ProjectSql>, String> {
    let Some(relation) = sql_analysis_relation(snapshot).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let Some(SqlAnalysisRecord::Selection {
        project: owner,
        source_root,
        inventory,
        implicit,
        gaps,
        ..
    }) = relation
        .lookup(&sql_selection_key(project))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    if owner != project {
        return Err("SQL selection belongs to another project".into());
    }
    let mut hosts: Vec<SqlHost> = Vec::new();
    let mut raw: Vec<([u8; 32], u32, SqlFact)> = Vec::new();
    let mut after = None;
    let mut rows = 0usize;
    loop {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|e| e.to_string())?;
        for (key, row) in page.entries() {
            rows += 1;
            if rows > MAX_ROWS {
                return Err("SQL relation exceeds the read bound".into());
            }
            match row {
                SqlAnalysisRecord::Host {
                    project: p,
                    path,
                    dialect,
                    source,
                    length,
                    terminal,
                    ..
                } if *p == project => {
                    hosts.push(SqlHost {
                        key: *key,
                        path: path.clone(),
                        dialect: *dialect,
                        source: *source,
                        length: *length,
                        terminal: *terminal,
                    });
                }
                SqlAnalysisRecord::Fact {
                    host,
                    ordinal,
                    fact,
                } => {
                    raw.push((*host, *ordinal, fact.clone()));
                }
                _ => {}
            }
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            Some(_) => return Err("SQL relation page did not advance".into()),
            None => break,
        }
    }
    hosts.sort_by(|a, b| a.path.cmp(&b.path));
    let index_of: std::collections::BTreeMap<[u8; 32], usize> =
        hosts.iter().enumerate().map(|(i, h)| (h.key, i)).collect();
    let mut facts: Vec<(usize, u32, SqlFact)> = raw
        .into_iter()
        .filter_map(|(host, ordinal, fact)| index_of.get(&host).map(|i| (*i, ordinal, fact)))
        .collect();
    facts.sort_by_key(|(host, ordinal, _)| (*host, *ordinal));
    Ok(Some(ProjectSql {
        root: source_root,
        inventory_complete: matches!(inventory, SqlInventory::Complete),
        implicit,
        gaps,
        hosts,
        facts,
    }))
}

/// Constant-size selected native host terminals for health. This says what the
/// selected snapshot analysed, never what a later installation might admit.
pub(super) fn progress(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<backend_library::SqlAnalysisProgress>, String> {
    let Some(relation) = sql_analysis_relation(snapshot).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let mut summary = backend_library::SqlAnalysisProgress {
        analysed_files: 0,
        no_worker_files: 0,
        unsupported_dialect_files: 0,
        rejected_files: 0,
        other_unavailable_files: 0,
    };
    let mut after = None;
    let mut rows = 0usize;
    let mut has_hosts = false;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|e| e.to_string())?;
        for (_, row) in page.entries() {
            rows += 1;
            if rows > MAX_ROWS {
                return Err("SQL status exceeds the read bound".to_owned());
            }
            let SqlAnalysisRecord::Host { terminal, .. } = row else {
                continue;
            };
            has_hosts = true;
            let count = match terminal {
                SqlHostTerminal::Parsed { .. } => &mut summary.analysed_files,
                SqlHostTerminal::NativeError => &mut summary.rejected_files,
                SqlHostTerminal::Unavailable(
                    backend_engine::builtin::SqlUnavailable::EngineNotAdmitted,
                ) => &mut summary.no_worker_files,
                SqlHostTerminal::Unavailable(
                    backend_engine::builtin::SqlUnavailable::UnsupportedDialect,
                ) => &mut summary.unsupported_dialect_files,
                SqlHostTerminal::Unavailable(_) => &mut summary.other_unavailable_files,
            };
            *count = count
                .checked_add(1)
                .ok_or_else(|| "SQL status count overflow".to_owned())?;
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            Some(_) => return Err("SQL status page did not advance".to_owned()),
            None => break,
        }
    }
    Ok(has_hosts.then_some(summary))
}
