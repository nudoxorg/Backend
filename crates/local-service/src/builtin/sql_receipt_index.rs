//! Private planning of the paged project-to-receipt selection index. This
//! does not publish, authenticate replay or construct an issued receipt.
use super::{BuiltinModelError, sql_receipt::IssuedSqlReceipt};
use backend_engine::builtin::{SqlReceiptIndexRelation, SqlReceiptIndexRootSchema, SqlReceiptIndexRow, sql_receipt_index_relation, sql_receipt_index_root_object, sql_receipt_index_root_pointer_key};
use backend_engine::{RelationState, TreeChange, WorkspaceSnapshot};
use backend_store::TypedObject;
use backend_version::{Schema, SchemaIdentity};

pub(super) struct PreparedSqlReceiptIndex {
    pub(super) pointer: TypedObject,
    pub(super) node_objects: Vec<TypedObject>,
    pub(super) receipt: TypedObject,
}
/// Update exactly one project using an issuer-only receipt. Every other index
/// row is inherited by the canonical path-copy tree. The owner must revalidate
/// and install these exact objects together with the SQL target and source row.
pub(super) fn prepare_receipt_index(
    base: &WorkspaceSnapshot,
    receipt: &IssuedSqlReceipt,
) -> Result<PreparedSqlReceiptIndex, BuiltinModelError> {
    receipt.check_cancelled()?;
    let basis = super::sql_admission::sql_basis_for_snapshot(base).map_err(BuiltinModelError)?;
    if !receipt.matches_basis(basis) { return Err(BuiltinModelError("SQL receipt index predecessor mismatch".into())); }
    let object = receipt.object()?;
    let project = receipt.project();
    let row = SqlReceiptIndexRow { object_id: *object.id().as_bytes(), object_version: *object.version() };
    let selected = sql_receipt_index_relation(base).map_err(|e| BuiltinModelError(format!("open SQL receipt index: {e}")))?;
    let (pointer, node_objects) = if let Some(relation) = selected {
        let schema = SchemaIdentity::new(SqlReceiptIndexRootSchema::DOMAIN, SqlReceiptIndexRootSchema::TYPE, SqlReceiptIndexRootSchema::VERSION);
        let key = sql_receipt_index_root_pointer_key();
        let previous = base.closure().control_manifest().objects().iter()
            .find(|object| object.schema() == schema && object.key() == &key)
            .ok_or_else(|| BuiltinModelError("SQL receipt index pointer missing".into()))?;
        let root = relation.root_object().map_err(|e| BuiltinModelError(format!("SQL receipt index root: {e}")))?;
        if !base.closure().contains(previous.id()).map_err(|e| BuiltinModelError(format!("SQL receipt closure: {e:?}")))?
            || !base.closure().contains(root.id()).map_err(|e| BuiltinModelError(format!("SQL receipt closure: {e:?}")))? {
            return Err(BuiltinModelError("SQL receipt index outside predecessor closure".into()));
        }
        receipt.check_cancelled()?;
        let update = relation.prepare_update(&[TreeChange { key: project, after: Some(row) }])
            .map_err(|e| BuiltinModelError(format!("update SQL receipt index: {e}")))?;
        receipt.check_cancelled()?;
        let nodes = update.changed_nodes().iter().map(|node| {
            receipt.check_cancelled()?;
            TypedObject::from_state_root(node.commitment(), node)
                .map_err(|e| BuiltinModelError(format!("retain SQL receipt index: {e:?}")))
        })
            .collect::<Result<Vec<_>, _>>()?;
        (sql_receipt_index_root_object(update.target().root()), nodes)
    } else {
        let state = RelationState::<SqlReceiptIndexRelation>::from_entries(vec![(project, row)], super::admitted_coverage()?)
            .map_err(|e| BuiltinModelError(format!("initial SQL receipt index: {e:?}")))?;
        let mut closure = state.node_closure();
        let mut nodes = Vec::new();
        while let Some(node) = closure.try_next().map_err(|e| BuiltinModelError(format!("SQL receipt index nodes: {e:?}")))? {
            receipt.check_cancelled()?;
            nodes.push(TypedObject::from_state_root(node.state_root(), node.canonical()).map_err(|e| BuiltinModelError(format!("SQL receipt index node: {e:?}")))?);
        }
        (sql_receipt_index_root_object(state.root()), nodes)
    };
    receipt.check_cancelled()?;
    Ok(PreparedSqlReceiptIndex { pointer, node_objects, receipt: object })
}

/// Authenticate and collect every retained project receipt in one bounded
/// source-relation pass and one bounded receipt-index pass. The caller must
/// provide the active owner's exact admitted snapshot and run on its worker.
/// The replacing project is excluded only when its SQL delta is present.
pub(super) fn collect_retained_receipts(
    base: &WorkspaceSnapshot,
    owner: &super::owner_receipt_context::OwnerReceiptContext,
    excluded: Option<[u8; 32]>,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<Vec<super::sql_receipt::VerifiedSqlReceipt>, BuiltinModelError> {
    use backend_engine::builtin::{SqlAnalysisRecord, SqlAnalysisRelation, admit_sql_relation, sql_analysis_relation};
    use backend_version::Relation;
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    let check = || if cancellation.load(Ordering::Acquire) { Err(BuiltinModelError("SQL receipt staging cancelled".into())) } else { Ok(()) };
    check()?;
    // This admits the selected SQL root pointer/root membership first.
    let sql_basis = super::sql_admission::sql_basis_for_snapshot(base).map_err(BuiltinModelError)?;
    let index = sql_receipt_index_relation(base).map_err(|e| BuiltinModelError(format!("open receipt index: {e}")))?;
    let sql = sql_analysis_relation(base).map_err(|e| BuiltinModelError(format!("open SQL selection: {e}")))?;
    if sql_basis.analysis_root.is_none() {
        if index.is_some() { return Err(BuiltinModelError("receipt index without SQL selection".into())); }
        return Ok(Vec::new());
    }
    let sql = sql.ok_or_else(|| BuiltinModelError("SQL selection unavailable".into()))?;
    let store = base.durable_store().ok_or_else(|| BuiltinModelError("SQL receipt staging requires durable store".into()))?;
    let mut rows = Vec::new();
    let mut after = None;
    let mut charged = 0usize;
    loop {
        check()?;
        let page = sql.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS).map_err(|e| BuiltinModelError(format!("page SQL selection: {e}")))?;
        for (key, row) in page.entries() {
            check()?;
            row.validate().map_err(BuiltinModelError)?;
            let mut encoded = Vec::new();
            SqlAnalysisRelation::encode_value(row, &mut encoded);
            charged = charged.checked_add(encoded.len()).and_then(|n| n.checked_add(40)).ok_or_else(|| BuiltinModelError("SQL receipt staging limit".into()))?;
            // Staging has a tighter row cap than the canonical relation: it
            // also holds a bounded host map and per-project reference groups.
            if charged > 64 * 1024 * 1024 || rows.len() >= 65_536 { return Err(BuiltinModelError("SQL receipt staging limit".into())); }
            rows.try_reserve(1).map_err(|_| BuiltinModelError("SQL receipt staging allocation".into()))?;
            rows.push((*key, row.clone()));
        }
        match page.next().copied() { Some(next) if after != Some(next) => after = Some(next), Some(_) => return Err(BuiltinModelError("SQL receipt staging page did not advance".into())), None => break }
    }
    let mut refs = Vec::new();
    refs.try_reserve_exact(rows.len()).map_err(|_| BuiltinModelError("SQL receipt reference allocation".into()))?;
    refs.extend(rows.iter().map(|(key, row)| (*key, row)));
    admit_sql_relation(&refs).map_err(BuiltinModelError)?;
    let mut groups: BTreeMap<[u8; 32], Vec<([u8; 32], &SqlAnalysisRecord)>> = BTreeMap::new();
    let mut hosts = BTreeMap::new();
    let mut has_selection = false;
    for (key, row) in &rows {
        check()?;
        match row {
            SqlAnalysisRecord::Selection { project, .. } => {
                has_selection = true;
                if Some(*project) != excluded { let group = groups.entry(*project).or_default(); group.try_reserve(1).map_err(|_| BuiltinModelError("SQL receipt group allocation".into()))?; group.push((*key, row)); }
            }
            // An attempt without a served selection is explicit unavailability;
            // it has no prior selected receipt to carry.
            SqlAnalysisRecord::Attempt { .. } => {},
            SqlAnalysisRecord::Host { project, .. } => {
                hosts.insert(*key, *project);
                if Some(*project) != excluded { let group = groups.entry(*project).or_default(); group.try_reserve(1).map_err(|_| BuiltinModelError("SQL receipt group allocation".into()))?; group.push((*key, row)); }
            }
            SqlAnalysisRecord::Fact { .. } => {}
        }
        if groups.len() > 4096 || hosts.len() > 65_536 { return Err(BuiltinModelError("SQL receipt staging project limit".into())); }
    }
    for (key, row) in &rows {
        check()?;
        if let SqlAnalysisRecord::Fact { host, .. } = row {
            let project = hosts.get(host).ok_or_else(|| BuiltinModelError("SQL receipt fact owner missing".into()))?;
            if Some(*project) != excluded { let group = groups.get_mut(project).ok_or_else(|| BuiltinModelError("SQL receipt project missing".into()))?; group.try_reserve(1).map_err(|_| BuiltinModelError("SQL receipt group allocation".into()))?; group.push((*key, row)); }
        }
    }
    let Some(index) = index else {
        if !has_selection && groups.is_empty() { return Ok(Vec::new()); }
        return Err(BuiltinModelError("SQL receipt index unavailable".into()));
    };
    let schema = SchemaIdentity::new(SqlReceiptIndexRootSchema::DOMAIN, SqlReceiptIndexRootSchema::TYPE, SqlReceiptIndexRootSchema::VERSION);
    let pointer_key = sql_receipt_index_root_pointer_key();
    let pointer = base.closure().control_manifest().objects().iter().find(|object| object.schema() == schema && object.key() == &pointer_key)
        .ok_or_else(|| BuiltinModelError("SQL receipt index pointer missing".into()))?;
    let root = index.root_object().map_err(|e| BuiltinModelError(format!("SQL receipt index root: {e}")))?;
    if !base.closure().contains(pointer.id()).map_err(|e| BuiltinModelError(format!("SQL receipt closure: {e:?}")))?
        || !base.closure().contains(root.id()).map_err(|e| BuiltinModelError(format!("SQL receipt closure: {e:?}")))? {
        return Err(BuiltinModelError("SQL receipt index outside selected closure".into()));
    }
    let mut retained = Vec::new();
    let mut index_after = None;
    let mut seen = 0usize;
    loop {
        check()?;
        let page = index.page(index_after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS).map_err(|e| BuiltinModelError(format!("page SQL receipt index: {e}")))?;
        for (project, row) in page.entries() {
            check()?;
            seen = seen.checked_add(1).ok_or_else(|| BuiltinModelError("SQL receipt index limit".into()))?;
            if seen > 4096 { return Err(BuiltinModelError("SQL receipt index limit".into())); }
            if Some(*project) == excluded { continue; }
            let mut project_rows = groups.remove(project).ok_or_else(|| BuiltinModelError("SQL receipt project rows missing".into()))?;
            project_rows.sort_unstable_by_key(|entry| entry.0);
            let digest = super::sql_capture::sql_project_rows_digest_refs(*project, &project_rows, cancellation).map_err(|e| BuiltinModelError(format!("SQL receipt project digest: {e:?}")))?;
            let verified = super::sql_receipt::selected_retained_receipt(base, store, owner, *project, digest)?;
            retained.try_reserve(1).map_err(|_| BuiltinModelError("SQL receipt staging allocation".into()))?;
            retained.push((row.object_id, verified));
        }
        match page.next().copied() { Some(next) if index_after != Some(next) => index_after = Some(next), Some(_) => return Err(BuiltinModelError("SQL receipt index page did not advance".into())), None => break }
    }
    if !groups.is_empty() { return Err(BuiltinModelError("selected SQL project has no receipt".into())); }
    retained.sort_unstable_by_key(|entry| entry.0);
    if retained.windows(2).any(|pair| pair[0].0 == pair[1].0) { return Err(BuiltinModelError("duplicate SQL receipt member".into())); }
    check()?;
    let mut result = Vec::new();
    result.try_reserve_exact(retained.len()).map_err(|_| BuiltinModelError("SQL receipt staging allocation".into()))?;
    result.extend(retained.into_iter().map(|(_, receipt)| receipt));
    Ok(result)
}
