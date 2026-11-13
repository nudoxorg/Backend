//! Private Vue project replacement planner. No public route or publication is
//! authorized by these rows alone; the workspace owner must validate its
//! predecessor and atomically bind the returned exact delta and live proof.
use super::ingest::{IndexSnapshot, VueProjectCapture};
use backend_engine::{
    ProductSourceRecord, WorkspaceSnapshot,
    builtin::{validate_vue_inventory_rows, vue_inventory_relation},
};
use backend_frontend_vue::{VueInventoryRecord, VueInventoryRelation};
use backend_version::Relation;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VueInventoryChange {
    pub(super) key: [u8; 32],
    pub(super) expected: Option<VueInventoryRecord>,
    pub(super) after: Option<VueInventoryRecord>,
}
/// Recompute one exact, bounded project replacement from a private live proof.
/// Truncated walks cannot retire old entries; refuse the replacement instead.
/// This private planner does not authenticate `selected_root` or prove that
/// `project` is the exact staged after-image of `scan`: the live workspace
/// owner must establish both before publication. The equal content version and
/// scan identity checks here are necessary but not sufficient for that proof.
pub(super) fn plan_vue_project_update(
    base: &WorkspaceSnapshot,
    project: &ProductSourceRecord,
    scan: &IndexSnapshot,
    selected_root: &std::path::Path,
    capture: &VueProjectCapture,
    cancellation: &AtomicBool,
) -> Result<Vec<VueInventoryChange>, String> {
    if !capture.belongs_to_scan(scan, selected_root) {
        return Err("Vue capture differs from selected scan or root".into());
    }
    capture.verify(cancellation)?;
    if !capture.walk_complete() {
        return Err("Vue source walk is truncated".into());
    }
    if project.project_fields().is_none_or(|fields| {
        fields.source_version != capture.project_source_version()
            || backend_library::package_key(fields.label).to_bytes() != capture.project()
    }) {
        return Err("Vue capture does not match selected source project".into());
    }
    let mut selected = BTreeMap::new();
    let mut work = 0usize;
    if let Some(relation) = vue_inventory_relation(base).map_err(|e| e.to_string())? {
        let mut after = None;
        loop {
            let page = relation
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|e| e.to_string())?;
            for (key, row) in page.entries() {
                if cancellation.load(Ordering::Acquire) {
                    return Err("Vue planning cancelled".into());
                }
                let mut encoded = Vec::new();
                VueInventoryRelation::encode_value(row, &mut encoded);
                work = work
                    .checked_add(encoded.len())
                    .ok_or("Vue planning byte overflow")?;
                if work > 64 * 1024 * 1024 || selected.len() >= 100_000 {
                    return Err("Vue planning budget exceeded".into());
                }
                if selected.insert(*key, row.clone()).is_some() {
                    return Err("duplicate Vue row".into());
                }
            }
            match page.next().copied() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
    }
    validate_vue_inventory_rows(
        &selected
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>(),
    )?;
    let before: BTreeMap<_, _> = selected
        .iter()
        .filter(|(_, row)| row.project() == capture.project())
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let after: BTreeMap<_, _> = capture.rows().iter().cloned().collect();
    for key in before.keys() {
        selected.remove(key);
    }
    for (key, row) in &after {
        if row.project() != capture.project() || selected.insert(*key, row.clone()).is_some() {
            return Err("Vue project owner/key collision".into());
        }
    }
    validate_vue_inventory_rows(&selected.into_iter().collect::<Vec<_>>())?;
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    Ok(keys
        .into_iter()
        .filter_map(|key| {
            let expected = before.get(&key).cloned();
            let after = after.get(&key).cloned();
            (expected != after).then_some(VueInventoryChange {
                key,
                expected,
                after,
            })
        })
        .collect())
}

/// Exact predecessor for a Vue auxiliary transition; the selected relation
/// root remains distinct even when the workspace manifest is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VueInventoryBasis {
    pub(super) workspace_root: [u8; 32],
    pub(super) sequence: u64,
    pub(super) closure: [u8; 32],
    pub(super) relation_root: Option<[u8; 32]>,
}
impl VueInventoryBasis {
    pub(super) fn from_snapshot(snapshot: &WorkspaceSnapshot) -> Result<Self, String> {
        Ok(Self {
            workspace_root: *snapshot.root().as_bytes(),
            sequence: snapshot.sequence(),
            closure: *snapshot.closure().binding().closure().as_bytes(),
            relation_root: vue_inventory_relation(snapshot)
                .map_err(|e| e.to_string())?
                .map(|r| *r.root().as_bytes()),
        })
    }
}
/// Bounded standalone BPI15 section. It is not a live authorization and is
/// not attached to the journal until the finalized BPI14 grammar is present.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VueInventoryDelta {
    pub(super) owner: [u8; 32],
    pub(super) basis: VueInventoryBasis,
    pub(super) changes: Vec<VueInventoryChange>,
}
impl VueInventoryDelta {
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    const MAX_CHANGES: usize = 100_000;
    /// Encode exact before/after rows with deterministic order and bounds.
    pub(super) fn encode(&self) -> Result<Vec<u8>, String> {
        if self.owner == [0; 32] || self.changes.len() > Self::MAX_CHANGES {
            return Err("invalid Vue delta owner or count".into());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"VUD1");
        out.extend_from_slice(&self.owner);
        out.extend_from_slice(&self.basis.workspace_root);
        out.extend_from_slice(&self.basis.sequence.to_be_bytes());
        out.extend_from_slice(&self.basis.closure);
        if let Some(root) = self.basis.relation_root {
            out.push(1);
            out.extend_from_slice(&root);
        } else {
            out.push(0);
        }
        out.extend_from_slice(&(self.changes.len() as u32).to_be_bytes());
        let mut previous = None;
        for change in &self.changes {
            if previous.is_some_and(|key| key >= change.key)
                || change.expected == change.after
                || change
                    .expected
                    .iter()
                    .chain(change.after.iter())
                    .any(|row| {
                        row.project() != self.owner
                            || row.key() != change.key
                            || row.validate().is_err()
                    })
            {
                return Err("invalid Vue delta key, owner or no-op".into());
            }
            previous = Some(change.key);
            out.extend_from_slice(&change.key);
            for row in [&change.expected, &change.after] {
                match row {
                    None => out.push(0),
                    Some(row) => {
                        out.push(1);
                        let mut encoded = Vec::new();
                        VueInventoryRelation::encode_value(row, &mut encoded);
                        out.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
                        out.extend_from_slice(&encoded);
                    }
                }
                if out.len() > Self::MAX_BYTES {
                    return Err("Vue delta byte limit".into());
                }
            }
        }
        Ok(out)
    }
    /// Decode canonical bounded bytes; this creates no live capture proof.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, String> {
        use backend_version::CanonicalRelation;
        if bytes.len() > Self::MAX_BYTES {
            return Err("Vue delta byte limit".into());
        }
        struct Reader<'a> {
            bytes: &'a [u8],
            at: usize,
        }
        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
                let end = self.at.checked_add(n).ok_or("Vue delta overflow")?;
                let result = self.bytes.get(self.at..end).ok_or("truncated Vue delta")?;
                self.at = end;
                Ok(result)
            }
            fn u32(&mut self) -> Result<u32, String> {
                Ok(u32::from_be_bytes(
                    self.take(4)?.try_into().map_err(|_| "Vue delta width")?,
                ))
            }
            fn array(&mut self) -> Result<[u8; 32], String> {
                self.take(32)?
                    .try_into()
                    .map_err(|_| "Vue delta width".into())
            }
            fn row(&mut self) -> Result<Option<VueInventoryRecord>, String> {
                match self.take(1)?[0] {
                    0 => Ok(None),
                    1 => {
                        let n = self.u32()? as usize;
                        if n > 30 * 1024 {
                            return Err("Vue delta row limit".into());
                        }
                        Ok(Some(
                            VueInventoryRelation::decode_value(self.take(n)?)
                                .map_err(|_| "invalid Vue row")?,
                        ))
                    }
                    _ => Err("invalid Vue row tag".into()),
                }
            }
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take(4)? != b"VUD1" {
            return Err("invalid Vue delta magic".into());
        }
        let owner = r.array()?;
        let workspace_root = r.array()?;
        let sequence = u64::from_be_bytes(r.take(8)?.try_into().map_err(|_| "Vue delta width")?);
        let closure = r.array()?;
        let relation_root = match r.take(1)?[0] {
            0 => None,
            1 => Some(r.array()?),
            _ => return Err("invalid Vue root tag".into()),
        };
        let count = r.u32()? as usize;
        if count > Self::MAX_CHANGES || count > bytes.len().saturating_sub(r.at) / 34 {
            return Err("Vue delta count limit".into());
        }
        let mut changes = Vec::with_capacity(count);
        for _ in 0..count {
            changes.push(VueInventoryChange {
                key: r.array()?,
                expected: r.row()?,
                after: r.row()?,
            });
        }
        if r.at != bytes.len() {
            return Err("trailing Vue delta bytes".into());
        }
        let result = Self {
            owner,
            basis: VueInventoryBasis {
                workspace_root,
                sequence,
                closure,
                relation_root,
            },
            changes,
        };
        if result.encode()?.as_slice() != bytes {
            return Err("noncanonical Vue delta".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vue_delta_is_canonical_owner_bound_and_rejects_forgery() {
        let row = VueInventoryRecord::Selection {
            project: [1; 32],
            source_root: "/tmp/vue".into(),
            basis: [2; 32],
            producer: backend_frontend_vue::support_fingerprint(),
            discovered: 0,
            inventory_digest: backend_frontend_vue::vue_path_inventory_digest(&[]).unwrap(),
            unavailable: 0,
            walk_truncated: false,
        };
        let delta = VueInventoryDelta {
            owner: [1; 32],
            basis: VueInventoryBasis {
                workspace_root: [3; 32],
                sequence: 7,
                closure: [4; 32],
                relation_root: None,
            },
            changes: vec![VueInventoryChange {
                key: row.key(),
                expected: None,
                after: Some(row),
            }],
        };
        let bytes = delta.encode().unwrap();
        assert_eq!(VueInventoryDelta::decode(&bytes).unwrap(), delta);
        let mut bad = delta.clone();
        bad.changes[0].key = [9; 32];
        assert!(bad.encode().is_err());
        let mut bad = delta.clone();
        bad.owner = [8; 32];
        assert!(bad.encode().is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(VueInventoryDelta::decode(&trailing).is_err());
    }
}
