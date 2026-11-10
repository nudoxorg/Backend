//! Version-neutral planning of claim-only C# receipt relation replacements.
//!
//! This module is not a publication capability. A later intent must bind an
//! exact selected predecessor, private same-attempt proof, compiler closure,
//! source frontier and cancellation before these planned changes may commit.

use backend_engine::WorkspaceSnapshot;
use backend_engine::builtin::{
    CSharpReceiptRecord, CSharpReceiptRelation, CSharpReceiptUnit,
    admit_csharp_receipt_relation_cancellable, csharp_receipt_relation,
};
use backend_version::Relation;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Exact predecessor needed by a future authenticated transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CSharpReceiptBasis {
    pub workspace_root: [u8; 32],
    pub sequence: u64,
    pub owner_epoch: u64,
    pub closure: [u8; 32],
    pub relation_root: Option<[u8; 32]>,
}

/// One exact before/after row. A plan alone grants no authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CSharpReceiptChange {
    pub key: [u8; 32],
    pub expected: Option<CSharpReceiptRecord>,
    pub after: Option<CSharpReceiptRecord>,
}

pub(super) fn csharp_receipt_basis(
    snapshot: &WorkspaceSnapshot,
) -> Result<CSharpReceiptBasis, String> {
    let root = csharp_receipt_relation(snapshot)
        .map_err(|error| error.to_string())?
        .map(|relation| *relation.root().as_bytes());
    Ok(CSharpReceiptBasis {
        workspace_root: *snapshot.root().as_bytes(),
        sequence: snapshot.sequence(),
        owner_epoch: snapshot.owner_epoch(),
        closure: *snapshot.closure().binding().closure().as_bytes(),
        relation_root: root,
    })
}

fn owns(row: &CSharpReceiptRecord, project: [u8; 32], unit: &CSharpReceiptUnit) -> bool {
    match row {
        CSharpReceiptRecord::Unit {
            project: owner,
            unit: selected,
            ..
        }
        | CSharpReceiptRecord::Member {
            project: owner,
            unit: selected,
            ..
        } => *owner == project && selected == unit,
    }
}

/// Plans an exact replacement from untrusted claims. The caller must not apply
/// it without independently proving capture, selected compiler closure and M0.
pub(super) fn plan_untrusted_csharp_replacement(
    base: &WorkspaceSnapshot,
    project: [u8; 32],
    unit: &CSharpReceiptUnit,
    replacement: &[([u8; 32], CSharpReceiptRecord)],
    cancelled: &AtomicBool,
) -> Result<Vec<CSharpReceiptChange>, String> {
    if project == [0; 32] {
        return Err("invalid C# receipt project".into());
    }
    let mut selected = BTreeMap::new();
    let mut work = 0usize;
    if let Some(relation) = csharp_receipt_relation(base).map_err(|error| error.to_string())? {
        let mut after = None;
        loop {
            let page = relation
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|error| error.to_string())?;
            for (key, row) in page.entries() {
                if cancelled.load(Ordering::Acquire) {
                    return Err("C# receipt planning cancelled".into());
                }
                let mut encoded = Vec::new();
                CSharpReceiptRelation::encode_value(row, &mut encoded);
                work = work
                    .checked_add(encoded.len())
                    .and_then(|n| n.checked_add(40))
                    .ok_or("C# receipt work overflow")?;
                if work > 32 * 1024 * 1024 || selected.len() >= 16384 {
                    return Err("C# receipt scan exceeds budget".into());
                }
                if selected.insert(*key, row.clone()).is_some() {
                    return Err("duplicate C# receipt row".into());
                }
            }
            match page.next().copied() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
    }
    let current = selected
        .iter()
        .map(|(key, row)| (*key, row))
        .collect::<Vec<_>>();
    admit_csharp_receipt_relation_cancellable(&current, cancelled).map_err(str::to_owned)?;
    let before = selected
        .iter()
        .filter(|(_, row)| owns(row, project, unit))
        .map(|(key, row)| (*key, row.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut previous = None;
    for (key, row) in replacement {
        if cancelled.load(Ordering::Acquire) {
            return Err("C# receipt planning cancelled".into());
        }
        if previous.is_some_and(|old| old >= *key) || !owns(row, project, unit) {
            return Err("C# replacement is unordered or belongs to another unit".into());
        }
        row.validate_shape().map_err(str::to_owned)?;
        let mut encoded = Vec::new();
        CSharpReceiptRelation::encode_value(row, &mut encoded);
        work = work
            .checked_add(encoded.len())
            .and_then(|n| n.checked_add(40))
            .ok_or("C# receipt work overflow")?;
        if work > 32 * 1024 * 1024 {
            return Err("C# receipt combined plan exceeds budget".into());
        }
        previous = Some(*key);
    }
    let requested = replacement
        .iter()
        .map(|(key, row)| (*key, row))
        .collect::<Vec<_>>();
    admit_csharp_receipt_relation_cancellable(&requested, cancelled).map_err(str::to_owned)?;
    let target_count = selected
        .len()
        .checked_sub(before.len())
        .and_then(|count| count.checked_add(replacement.len()))
        .ok_or("C# receipt target count overflow")?;
    if target_count > 16384 {
        return Err("C# receipt target exceeds row budget".into());
    }
    for key in before.keys() {
        if cancelled.load(Ordering::Acquire) {
            return Err("C# receipt planning cancelled".into());
        }
        selected.remove(key);
    }
    for (key, row) in replacement {
        if cancelled.load(Ordering::Acquire) {
            return Err("C# receipt planning cancelled".into());
        }
        if selected.insert(*key, row.clone()).is_some() {
            return Err("C# receipt key collides with another unit".into());
        }
    }
    let target = selected
        .iter()
        .map(|(key, row)| (*key, row))
        .collect::<Vec<_>>();
    admit_csharp_receipt_relation_cancellable(&target, cancelled).map_err(str::to_owned)?;
    let keys = before
        .keys()
        .chain(replacement.iter().map(|(key, _)| key))
        .copied()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for key in keys {
        if cancelled.load(Ordering::Acquire) {
            return Err("C# receipt planning cancelled".into());
        }
        let old = before.get(&key).cloned();
        let new = selected.get(&key).cloned();
        if old != new {
            changes.push(CSharpReceiptChange {
                key,
                expected: old,
                after: new,
            });
        }
    }
    Ok(changes)
}
