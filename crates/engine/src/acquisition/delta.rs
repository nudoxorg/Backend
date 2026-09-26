use std::{fmt, sync::Arc};

use super::identity::{
    AcquisitionDeltaId, IdentityError, ManifestEntry, RawArchiveObjectId, SourceSnapshot,
    SourceSnapshotId, TreeManifest, canonical_text, frame,
};

/// One before/after source path transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeltaChange {
    /// Canonical path key.
    pub path: Arc<str>,
    /// Object expected in the base snapshot, if present.
    pub before: Option<RawArchiveObjectId>,
    /// Complete base row mode bits, if present.
    pub before_mode: Option<u32>,
    /// Object published in the target snapshot, if present.
    pub after: Option<RawArchiveObjectId>,
    /// Complete target row mode bits, if present.
    pub after_mode: Option<u32>,
}

/// Error applying a versioned acquisition delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeltaError {
    /// Changes were duplicated or did not have a canonical path key.
    InvalidChanges(IdentityError),
    /// The caller supplied a source root other than the bound base root.
    StaleBase {
        expected: SourceSnapshotId,
        actual: SourceSnapshotId,
    },
    /// A before value did not match the base manifest.
    BeforeMismatch,
    /// The computed target root did not match the bound target root.
    TargetMismatch,
}

impl fmt::Display for DeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "acquisition delta error: {self:?}")
    }
}
impl std::error::Error for DeltaError {}

pub(super) fn merge_manifest_entries(
    base_entries: &[ManifestEntry],
    changes: &[DeltaChange],
) -> Result<Vec<ManifestEntry>, DeltaError> {
    let mut entries = Vec::with_capacity(
        base_entries.len().saturating_add(
            changes
                .iter()
                .filter(|change| change.before.is_none())
                .count(),
        ),
    );
    let mut base_index = 0;
    let mut change_index = 0;
    while base_index < base_entries.len() || change_index < changes.len() {
        match (base_entries.get(base_index), changes.get(change_index)) {
            (Some(entry), Some(change)) if entry.path < change.path => {
                entries.push(entry.clone());
                base_index += 1;
            }
            (Some(entry), Some(change)) if entry.path == change.path => {
                if Some(entry.object) != change.before || Some(entry.mode) != change.before_mode {
                    return Err(DeltaError::BeforeMismatch);
                }
                if let Some(after) = change.after {
                    entries.push(ManifestEntry {
                        path: Arc::clone(&entry.path),
                        object: after,
                        mode: change.after_mode.unwrap_or_default(),
                    });
                }
                base_index += 1;
                change_index += 1;
            }
            (Some(_), Some(change)) => {
                if change.before.is_some() {
                    return Err(DeltaError::BeforeMismatch);
                }
                if let Some(after) = change.after {
                    entries.push(ManifestEntry {
                        path: Arc::clone(&change.path),
                        object: after,
                        mode: change.after_mode.unwrap_or_default(),
                    });
                }
                change_index += 1;
            }
            (Some(entry), None) => {
                entries.push(entry.clone());
                base_index += 1;
            }
            (None, Some(change)) => {
                if change.before.is_some() {
                    return Err(DeltaError::BeforeMismatch);
                }
                if let Some(after) = change.after {
                    entries.push(ManifestEntry {
                        path: Arc::clone(&change.path),
                        object: after,
                        mode: change.after_mode.unwrap_or_default(),
                    });
                }
                change_index += 1;
            }
            (None, None) => break,
        }
    }
    Ok(entries)
}

/// Immutable root-bound source delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionDelta {
    id: AcquisitionDeltaId,
    base: SourceSnapshotId,
    target: SourceSnapshotId,
    target_snapshot: Arc<SourceSnapshot>,
    changes: Arc<[DeltaChange]>,
}

impl AcquisitionDelta {
    /// Constructs a root-bound delta with canonical sorted changes.
    pub fn new(
        base: &SourceSnapshot,
        target: Arc<SourceSnapshot>,
        mut changes: Vec<DeltaChange>,
    ) -> Result<Self, DeltaError> {
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        if changes
            .windows(2)
            .any(|window| window[0].path == window[1].path)
        {
            return Err(DeltaError::InvalidChanges(IdentityError::Duplicate));
        }
        if changes.iter().any(|change| {
            change.before.is_some() != change.before_mode.is_some()
                || change.after.is_some() != change.after_mode.is_some()
        }) {
            return Err(DeltaError::InvalidChanges(IdentityError::NonCanonicalText));
        }
        let mut canonical = Vec::new();
        for change in &changes {
            canonical_text(change.path.to_string()).map_err(DeltaError::InvalidChanges)?;
            frame(&mut canonical, change.path.as_bytes());
            canonical
                .extend_from_slice(&change.before.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.before.is_some()));
            canonical.extend_from_slice(&change.before_mode.unwrap_or_default().to_be_bytes());
            canonical
                .extend_from_slice(&change.after.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.after.is_some()));
            canonical.extend_from_slice(&change.after_mode.unwrap_or_default().to_be_bytes());
        }
        let id =
            AcquisitionDeltaId::derive(&[base.id().as_bytes(), target.id().as_bytes(), &canonical]);
        let delta = Self {
            id,
            base: base.id(),
            target: target.id(),
            target_snapshot: target,
            changes: Arc::from(changes),
        };
        // Bind the supplied change set to the target root at construction.
        // This prevents a forged delta from using the target-id fast path to
        // bypass before/mode validation later in `apply`.
        let entries = merge_manifest_entries(base.manifest().entries(), &delta.changes)?;
        let manifest =
            Arc::new(TreeManifest::from_sorted(entries).map_err(DeltaError::InvalidChanges)?);
        let computed = SourceSnapshot::new_with_frontier(
            delta.target_snapshot.source(),
            delta.target_snapshot.cursor(),
            delta.target_snapshot.policy_epoch(),
            delta.target_snapshot.facts_frontier(),
            manifest,
            delta.target_snapshot.claims().to_vec(),
        )
        .map_err(DeltaError::InvalidChanges)?;
        if computed.id() != delta.target {
            return Err(DeltaError::TargetMismatch);
        }
        Ok(delta)
    }

    /// Returns this delta's immutable identity.
    #[must_use]
    pub const fn id(&self) -> AcquisitionDeltaId {
        self.id
    }
    /// Returns the exact base root.
    #[must_use]
    pub const fn base(&self) -> SourceSnapshotId {
        self.base
    }
    /// Returns the exact target root.
    #[must_use]
    pub const fn target(&self) -> SourceSnapshotId {
        self.target
    }
    /// Returns canonical sorted changes.
    #[must_use]
    pub fn changes(&self) -> &[DeltaChange] {
        &self.changes
    }

    /// Returns the canonical sorted change encoding committed by `id`.
    #[must_use]
    pub fn canonical_changes(&self) -> Vec<u8> {
        let mut canonical = Vec::new();
        for change in self.changes.iter() {
            frame(&mut canonical, change.path.as_bytes());
            canonical
                .extend_from_slice(&change.before.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.before.is_some()));
            canonical.extend_from_slice(&change.before_mode.unwrap_or_default().to_be_bytes());
            canonical
                .extend_from_slice(&change.after.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.after.is_some()));
            canonical.extend_from_slice(&change.after_mode.unwrap_or_default().to_be_bytes());
        }
        canonical
    }

    /// Applies once, or returns the same immutable target for a repeated
    /// application. A stale base is a typed error and never mutates state.
    pub fn apply(&self, base: &SourceSnapshot) -> Result<Arc<SourceSnapshot>, DeltaError> {
        if base.id() == self.target {
            return Ok(Arc::clone(&self.target_snapshot));
        }
        if base.id() != self.base {
            return Err(DeltaError::StaleBase {
                expected: self.base,
                actual: base.id(),
            });
        }
        let entries = merge_manifest_entries(base.manifest().entries(), &self.changes)?;
        let manifest =
            Arc::new(TreeManifest::from_sorted(entries).map_err(DeltaError::InvalidChanges)?);
        let computed = SourceSnapshot::new_with_frontier(
            self.target_snapshot.source(),
            self.target_snapshot.cursor(),
            self.target_snapshot.policy_epoch(),
            self.target_snapshot.facts_frontier(),
            manifest,
            self.target_snapshot.claims().to_vec(),
        )
        .map_err(DeltaError::InvalidChanges)?;
        if computed.id() != self.target {
            return Err(DeltaError::TargetMismatch);
        }
        Ok(Arc::new(computed))
    }
}

pub(super) fn snapshot_changes(base: &SourceSnapshot, target: &SourceSnapshot) -> Vec<DeltaChange> {
    let before = base.manifest().entries();
    let after = target.manifest().entries();
    let mut changes = Vec::with_capacity(before.len().abs_diff(after.len()));
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() || after_index < after.len() {
        match (before.get(before_index), after.get(after_index)) {
            (Some(left), Some(right)) if left.path == right.path => {
                if left.object != right.object || left.mode != right.mode {
                    changes.push(DeltaChange {
                        path: Arc::clone(&left.path),
                        before: Some(left.object),
                        before_mode: Some(left.mode),
                        after: Some(right.object),
                        after_mode: Some(right.mode),
                    });
                }
                before_index += 1;
                after_index += 1;
            }
            (Some(left), Some(right)) if left.path < right.path => {
                changes.push(DeltaChange {
                    path: Arc::clone(&left.path),
                    before: Some(left.object),
                    before_mode: Some(left.mode),
                    after: None,
                    after_mode: None,
                });
                before_index += 1;
            }
            (Some(_), Some(right)) => {
                changes.push(DeltaChange {
                    path: Arc::clone(&right.path),
                    before: None,
                    before_mode: None,
                    after: Some(right.object),
                    after_mode: Some(right.mode),
                });
                after_index += 1;
            }
            (Some(left), None) => {
                changes.push(DeltaChange {
                    path: Arc::clone(&left.path),
                    before: Some(left.object),
                    before_mode: Some(left.mode),
                    after: None,
                    after_mode: None,
                });
                before_index += 1;
            }
            (None, Some(right)) => {
                changes.push(DeltaChange {
                    path: Arc::clone(&right.path),
                    before: None,
                    before_mode: None,
                    after: Some(right.object),
                    after_mode: Some(right.mode),
                });
                after_index += 1;
            }
            (None, None) => break,
        }
    }
    changes
}
