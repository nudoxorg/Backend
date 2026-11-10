//! Private live SQL capture and exact predecessor/after-image planning.
//! Parsed intent bytes never carry this authority; callers must also bind the
//! target Project row and prove selected closure membership at owner admission.
use super::sql_capture::SqlProjectProjection;
use backend_engine::builtin::{
    SqlAnalysisRecord, SqlAnalysisRelation, SqlAnalysisRootSchema, admit_sql_relation,
    sql_analysis_relation, sql_analysis_root_pointer_key,
};
use backend_version::{Relation, Schema, SchemaIdentity};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_ROWS: usize = 262_144;
const MAX_WORK_BYTES: usize = 64 * 1024 * 1024;

/// Snapshot identity is a predecessor witness, not the target Merkle root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SqlAnalysisBasis {
    pub(super) workspace_root: [u8; 32],
    pub(super) sequence: u64,
    pub(super) closure: [u8; 32],
    pub(super) analysis_root: Option<[u8; 32]>,
}
/// Private same-scan capability, distinct from serialized SQL row claims.
#[derive(Clone)]
pub(super) struct SqlLiveProof {
    pub(super) capture: Arc<SqlProjectProjection>,
    pub(super) project: backend_engine::ProductSourceRecord,
    pub(super) cancellation: Arc<AtomicBool>,
}
impl std::fmt::Debug for SqlLiveProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlLiveProof").finish_non_exhaustive()
    }
}
/// One exact SQL before/after claim; owner admission must recompute it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SqlAnalysisChange {
    pub(super) key: [u8; 32],
    pub(super) expected: Option<SqlAnalysisRecord>,
    pub(super) after: Option<SqlAnalysisRecord>,
}

/// Plan removal of every selected SQL row owned by a deleted project. The
/// caller must separately prove the same transition removes that Project.
pub(super) fn plan_sql_retirement(
    base: &backend_engine::WorkspaceSnapshot,
    project: [u8; 32],
    cancelled: &AtomicBool,
) -> Result<Vec<SqlAnalysisChange>, String> {
    sql_basis_for_snapshot(base)?;
    let Some(relation) = sql_analysis_relation(base).map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    let mut selected = BTreeMap::new();
    let mut total = 0usize;
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|e| e.to_string())?;
        for (key, row) in page.entries() {
            if cancelled.load(Ordering::Acquire) {
                return Err("SQL retirement cancelled".into());
            }
            let mut bytes = Vec::new();
            SqlAnalysisRelation::encode_value(row, &mut bytes);
            total = total
                .checked_add(bytes.len())
                .ok_or("SQL retirement scan overflow")?;
            if total > MAX_WORK_BYTES
                || selected.len() >= MAX_ROWS
                || selected.insert(*key, row.clone()).is_some()
            {
                return Err("SQL retirement relation limit or duplicate".into());
            }
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            Some(_) => return Err("SQL retirement page did not advance".into()),
            None => break,
        }
    }
    admit_sql_relation(
        &selected
            .iter()
            .map(|(key, row)| (*key, row))
            .collect::<Vec<_>>(),
    )?;
    let hosts: BTreeSet<_> = selected
        .iter()
        .filter_map(|(key, row)| match row {
            SqlAnalysisRecord::Host { project: owner, .. } if *owner == project => Some(*key),
            _ => None,
        })
        .collect();
    let mut changes = Vec::new();
    for (key, row) in &selected {
        let owned = match row {
            SqlAnalysisRecord::Selection { project: owner, .. }
            | SqlAnalysisRecord::Attempt { project: owner, .. }
            | SqlAnalysisRecord::Host { project: owner, .. } => *owner == project,
            SqlAnalysisRecord::Fact { host, .. } => hosts.contains(host),
        };
        if owned {
            changes.push(SqlAnalysisChange {
                key: *key,
                expected: Some(row.clone()),
                after: None,
            });
        }
    }
    for change in &changes {
        selected.remove(&change.key);
    }
    admit_sql_relation(
        &selected
            .iter()
            .map(|(key, row)| (*key, row))
            .collect::<Vec<_>>(),
    )?;
    Ok(changes)
}
/// Open only a root whose typed pointer and root object both belong to the
/// authenticated snapshot closure.
pub(super) fn sql_basis_for_snapshot(
    snapshot: &backend_engine::WorkspaceSnapshot,
) -> Result<SqlAnalysisBasis, String> {
    let relation = sql_analysis_relation(snapshot).map_err(|e| e.to_string())?;
    if let Some(relation) = &relation {
        let schema = SchemaIdentity::new(
            SqlAnalysisRootSchema::DOMAIN,
            SqlAnalysisRootSchema::TYPE,
            SqlAnalysisRootSchema::VERSION,
        );
        let key = sql_analysis_root_pointer_key();
        let pointer = snapshot
            .closure()
            .control_manifest()
            .objects()
            .iter()
            .find(|object| object.schema() == schema && object.key() == &key)
            .ok_or("selected SQL pointer missing")?;
        if !snapshot
            .closure()
            .contains(pointer.id())
            .map_err(|e| format!("{e:?}"))?
            || !snapshot
                .closure()
                .contains(relation.root_object().map_err(|e| e.to_string())?.id())
                .map_err(|e| format!("{e:?}"))?
        {
            return Err("selected SQL pointer or root outside closure".into());
        }
    }
    Ok(SqlAnalysisBasis {
        workspace_root: *snapshot.root().as_bytes(),
        sequence: snapshot.sequence(),
        closure: *snapshot.closure().binding().closure().as_bytes(),
        analysis_root: relation.map(|relation| *relation.root().as_bytes()),
    })
}
/// Derive the exact project replacement from a revalidated private capture.
/// Every existing row is admitted first; untouched projects are preserved.
pub(super) fn plan_sql_update(
    base: &backend_engine::WorkspaceSnapshot,
    proof: &SqlLiveProof,
) -> Result<Vec<SqlAnalysisChange>, String> {
    if proof.cancellation.load(Ordering::Acquire) {
        return Err("SQL planning cancelled".into());
    }
    proof
        .capture
        .revalidate(&proof.cancellation)
        .map_err(|e| format!("SQL live source: {e:?}"))?;
    let project = proof.capture.project();
    let project_fields = proof
        .project
        .project_fields()
        .ok_or("SQL proof is not a source Project")?;
    if backend_engine::PackageKey::from_value(project_fields.label).to_bytes() != project {
        return Err("SQL proof belongs to another source Project".into());
    }
    let captured_root = proof
        .capture
        .rows()
        .iter()
        .find_map(|(_, row)| match row {
            SqlAnalysisRecord::Selection {
                project: owner,
                source_root,
                ..
            } if *owner == project => Some(source_root),
            _ => None,
        })
        .ok_or("SQL proof selection missing")?;
    let actual_root = std::path::Path::new(project_fields.label)
        .canonicalize()
        .map_err(|_| "SQL project root unavailable")?;
    if actual_root.as_os_str() != std::ffi::OsStr::new(project_fields.label)
        || actual_root.as_os_str() != std::ffi::OsStr::new(captured_root)
    {
        return Err("SQL proof root differs from selected project".into());
    }
    let _basis = sql_basis_for_snapshot(base)?;
    let mut selected = BTreeMap::new();
    let mut total = 0usize;
    if let Some(relation) = sql_analysis_relation(base).map_err(|e| e.to_string())? {
        let mut after = None;
        loop {
            let page = relation
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|e| e.to_string())?;
            for (key, row) in page.entries() {
                if proof.cancellation.load(Ordering::Acquire) {
                    return Err("SQL planning cancelled".into());
                }
                let mut bytes = Vec::new();
                SqlAnalysisRelation::encode_value(row, &mut bytes);
                total = total.checked_add(bytes.len()).ok_or("SQL scan overflow")?;
                if total > MAX_WORK_BYTES
                    || selected.len() >= MAX_ROWS
                    || selected.insert(*key, row.clone()).is_some()
                {
                    return Err("SQL selected relation limit or duplicate".into());
                }
            }
            match page.next().copied() {
                Some(next) if after != Some(next) => after = Some(next),
                Some(_) => return Err("SQL relation page did not advance".into()),
                None => break,
            }
        }
    }
    admit_sql_relation(&selected.iter().map(|(k, v)| (*k, v)).collect::<Vec<_>>())?;
    let owned_hosts: BTreeSet<_> = selected
        .iter()
        .filter_map(|(key, row)| match row {
            SqlAnalysisRecord::Host { project: owner, .. } if *owner == project => Some(*key),
            _ => None,
        })
        .collect();
    let before: BTreeMap<_, _> = selected
        .iter()
        .filter_map(|(key, row)| match row {
            SqlAnalysisRecord::Selection { project: owner, .. }
            | SqlAnalysisRecord::Attempt { project: owner, .. }
            | SqlAnalysisRecord::Host { project: owner, .. }
                if *owner == project =>
            {
                Some((*key, row.clone()))
            }
            SqlAnalysisRecord::Fact { host, .. } if owned_hosts.contains(host) => {
                Some((*key, row.clone()))
            }
            _ => None,
        })
        .collect();
    for key in before.keys() {
        selected.remove(key);
    }
    let mut after = BTreeMap::new();
    for (key, row) in proof.capture.rows() {
        if after.insert(*key, row.clone()).is_some() || selected.insert(*key, row.clone()).is_some()
        {
            return Err("SQL project key collision".into());
        }
    }
    if selected.len() > MAX_ROWS {
        return Err("SQL target row limit".into());
    }
    admit_sql_relation(&selected.iter().map(|(k, v)| (*k, v)).collect::<Vec<_>>())?;
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    Ok(keys
        .into_iter()
        .filter_map(|key| {
            let expected = before.get(&key).cloned();
            let after = after.get(&key).cloned();
            (expected != after).then_some(SqlAnalysisChange {
                key,
                expected,
                after,
            })
        })
        .collect())
}

/// SQL node and pointer delta prepared only after a live proof is checked.
pub(super) struct PreparedSqlRelationUpdate {
    pub(super) node_objects: Vec<backend_store::TypedObject>,
    pub(super) pointer: Option<backend_store::TypedObject>,
}
/// Private-constructed proof that the exact SQL delta was rederived from live
/// source and the selected snapshot. It expires when those borrows end.
pub(super) struct ValidatedSqlDelta<'a> {
    base: &'a backend_engine::WorkspaceSnapshot,
    changes: &'a [SqlAnalysisChange],
    _proof: &'a SqlLiveProof,
}
impl ValidatedSqlDelta<'_> {
    /// Re-read the confined configuration and hosts at the owner grant barrier.
    /// The caller must also recheck the target Project after-image at that barrier.
    pub(super) fn revalidate_before_grant(&self) -> Result<(), String> {
        if plan_sql_update(self.base, self._proof)?.as_slice() != self.changes {
            return Err("SQL live delta changed before owner grant".into());
        }
        Ok(())
    }
}
/// Check the exact source Project after-image and recompute the complete SQL
/// delta from the private source witness before the owner prepares a target.
pub(super) fn validate_sql_live<'a>(
    base: &'a backend_engine::WorkspaceSnapshot,
    proof: &'a SqlLiveProof,
    basis: SqlAnalysisBasis,
    changes: &'a [SqlAnalysisChange],
    selected_project_after: Option<&backend_engine::ProductSourceRecord>,
) -> Result<ValidatedSqlDelta<'a>, String> {
    if selected_project_after != Some(&proof.project) {
        return Err("SQL source Project after-image mismatch".into());
    }
    if sql_basis_for_snapshot(base)? != basis {
        return Err("SQL selected predecessor changed".into());
    }
    let recomputed = plan_sql_update(base, proof)?;
    if recomputed.as_slice() != changes {
        return Err("SQL live delta differs from private capture".into());
    }
    Ok(ValidatedSqlDelta {
        base,
        changes,
        _proof: proof,
    })
}
/// Prepare exact canonical SQL relation nodes after `validate_sql_live`.
/// This does not itself commit, validate replay, or authorize a workspace.
pub(super) fn prepare_sql_relation_update(
    validated: &ValidatedSqlDelta<'_>,
) -> Result<PreparedSqlRelationUpdate, String> {
    use backend_engine::builtin::sql_analysis_root_object;
    use backend_engine::{RelationState, TreeChange};
    use backend_store::TypedObject;
    let base = validated.base;
    let changes = validated.changes;
    if changes.is_empty() {
        return Ok(PreparedSqlRelationUpdate {
            node_objects: Vec::new(),
            pointer: None,
        });
    }
    if changes.len() > MAX_ROWS
        || changes.windows(2).any(|pair| pair[0].key >= pair[1].key)
        || changes.iter().any(|change| change.expected == change.after)
    {
        return Err("invalid SQL change ordering".into());
    }
    let selected = sql_analysis_relation(base).map_err(|e| e.to_string())?;
    let updates = changes
        .iter()
        .map(|change| TreeChange {
            key: change.key,
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    if let Some(relation) = selected {
        for change in changes {
            if relation.lookup(&change.key).map_err(|e| e.to_string())? != change.expected {
                return Err("SQL predecessor row mismatch".into());
            }
        }
        let update = relation
            .prepare_update(&updates)
            .map_err(|e| e.to_string())?;
        let node_objects = update
            .changed_nodes()
            .iter()
            .map(|node| {
                TypedObject::from_state_root(node.commitment(), node).map_err(|e| format!("{e:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedSqlRelationUpdate {
            node_objects,
            pointer: Some(sql_analysis_root_object(update.target().root())),
        })
    } else {
        if changes
            .iter()
            .any(|change| change.expected.is_some() || change.after.is_none())
        {
            return Err("invalid initial SQL relation delta".into());
        }
        let entries = changes
            .iter()
            .filter_map(|change| change.after.clone().map(|row| (change.key, row)))
            .collect::<Vec<_>>();
        let state = RelationState::<SqlAnalysisRelation>::from_entries(
            entries,
            super::admitted_coverage().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut closure = state.node_closure();
        let mut node_objects = Vec::new();
        while let Some(node) = closure.try_next().map_err(|e| format!("{e:?}"))? {
            node_objects.push(
                TypedObject::from_state_root(node.state_root(), node.canonical())
                    .map_err(|e| format!("{e:?}"))?,
            );
        }
        Ok(PreparedSqlRelationUpdate {
            node_objects,
            pointer: Some(sql_analysis_root_object(state.root())),
        })
    }
}

/// Before acknowledging an unchanged source index, revalidate the private SQL
/// capture and exact selected source Project; an unchanged SQL root is not by
/// itself proof that source and configuration bytes remain current.
pub(super) fn validate_sql_noop(
    base: &backend_engine::WorkspaceSnapshot,
    proof: &SqlLiveProof,
) -> Result<(), String> {
    let selected = base
        .relation::<backend_engine::ProductSourceRelation>()
        .map_err(|e| e.to_string())?
        .lookup(&proof.capture.project())
        .map_err(|e| e.to_string())?;
    if selected.as_ref() != Some(&proof.project) {
        return Err("SQL no-op Project changed".into());
    }
    if !plan_sql_update(base, proof)?.is_empty() {
        return Err("SQL no-op native selection changed".into());
    }
    Ok(())
}
