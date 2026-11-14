//! Plan an exact native Stylo project replacement against an admitted snapshot.
//!
//! This is preparation only. The workspace owner must still authenticate the
//! selected predecessor, apply exactly these changes, prove closure membership
//! and bind the private live capture before any relation is published.
use super::ingest::{StyloAttemptCapture, StyloProjectCapture};
use backend_engine::builtin::{
    StyloAnalysisRecord, StyloAnalysisRelation, admit_stylo_relation, stylo_analysis_relation,
};
use backend_version::Relation;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};

/// Fingerprint of the listed native admission, capture and source-walk files.
/// This does not identify all supervisor, dependency or platform behavior; the
/// explicit v1 recipe and owner-admitted worker are separate inputs. Historical
/// replay uses the recorded value. Length framing prevents ambiguous input.
pub(super) fn stylo_adapter_fingerprint() -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-native-adapter.v1\0");
    // Installed worker selection and the Linux private-library lease are a
    // versioned admission policy, independent of their transient paths.
    hash.update(b"installed-artifact-bounded-selection-platform-closure.v3\0");
    let parts: &[&[u8]] = &[
        include_bytes!("stylo_admission.rs"),
        include_bytes!("stylo_capture.rs"),
        include_bytes!("stylo_worker.rs"),
        include_bytes!("stylo_worker_install.rs"),
        include_bytes!("profile.rs"),
        include_bytes!("../builtin.rs"),
        include_bytes!("commands/index.rs"),
        include_bytes!("commands/index_source_preparation.rs"),
        include_bytes!("ingest.rs"),
        include_bytes!("source_budget.rs"),
        include_bytes!("source_frontier.rs"),
        include_bytes!("../../../discovery/src/lib.rs"),
        include_bytes!("../../../engine/src/builtin/stylo_identity.rs"),
        include_bytes!("../../../engine/src/builtin/stylo_projection.rs"),
        include_bytes!("../../../engine/src/builtin/stylo_relation.rs"),
    ];
    for part in parts {
        hash.update(&(part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    *hash.finalize().as_bytes()
}

/// Live capability for a same-scan capture or an exact source retirement.
#[derive(Clone)]
pub(super) enum StyloLiveProof {
    Captured {
        capture: std::sync::Arc<StyloProjectCapture>,
        project: backend_engine::ProductSourceRecord,
        cancellation: std::sync::Arc<AtomicBool>,
    },
    Unavailable {
        attempt: std::sync::Arc<StyloAttemptCapture>,
        cancellation: std::sync::Arc<AtomicBool>,
    },
    Retirement {
        project: [u8; 32],
        cancellation: std::sync::Arc<AtomicBool>,
    },
}
impl std::fmt::Debug for StyloLiveProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StyloLiveProof").finish_non_exhaustive()
    }
}
impl PartialEq for StyloLiveProof {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Captured {
                    capture: a,
                    project: x,
                    cancellation: c,
                },
                Self::Captured {
                    capture: b,
                    project: y,
                    cancellation: d,
                },
            ) => std::sync::Arc::ptr_eq(a, b) && x == y && std::sync::Arc::ptr_eq(c, d),
            (
                Self::Unavailable {
                    attempt: a,
                    cancellation: c,
                },
                Self::Unavailable {
                    attempt: b,
                    cancellation: d,
                },
            ) => std::sync::Arc::ptr_eq(a, b) && std::sync::Arc::ptr_eq(c, d),
            (
                Self::Retirement {
                    project: a,
                    cancellation: c,
                },
                Self::Retirement {
                    project: b,
                    cancellation: d,
                },
            ) => a == b && std::sync::Arc::ptr_eq(c, d),
            _ => false,
        }
    }
}
impl Eq for StyloLiveProof {}

/// Compact, private custody for the exact Stylo state selected by Pending.
/// It is minted only from the original same-scan proof after that selection.
pub(super) struct SelectedStyloFence {
    basis: super::profile::StyloAnalysisBasis,
    owner_epoch: u64,
    project: [u8; 32],
    project_commitment: [u8; 32],
    fence: super::ingest::CompilerRevisionFence,
    cancellation: std::sync::Arc<AtomicBool>,
    // Conservative checked charge, including this value, its Arc header and
    // the shared cancellation allocation. One such fence is retained per job.
    retained_bytes: usize,
}

#[derive(Clone)]
pub(super) enum StyloGrantProof {
    Live(StyloLiveProof),
    Selected(std::sync::Arc<SelectedStyloFence>),
}

fn selected_project_commitment(snapshot: &backend_engine::WorkspaceSnapshot, key: &[u8; 32]) -> Result<[u8; 32], String> {
    let relation = snapshot.relation::<backend_engine::ProductSourceRelation>().map_err(|e| e.to_string())?;
    let row = relation.lookup(key).map_err(|e| e.to_string())?.ok_or("Stylo selected Project missing")?;
    if row.project_fields().is_none() { return Err("Stylo selected source is not a Project".into()); }
    let mut bytes = Vec::new();
    <backend_engine::ProductSourceRelation as Relation>::encode_value(&row, &mut bytes);
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-selected-project.v1\0");
    hash.update(key);
    hash.update(&(bytes.len() as u64).to_be_bytes());
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}

impl SelectedStyloFence {
    pub(super) fn from_selected(snapshot: &backend_engine::WorkspaceSnapshot, proof: &StyloLiveProof, expected_cancellation: &std::sync::Arc<AtomicBool>) -> Result<Self, String> {
        let (project, fence, cancellation) = match proof {
            StyloLiveProof::Captured { capture, cancellation, .. } => {
                let StyloAnalysisRecord::Selection { project, .. } = capture.selection() else { return Err("Stylo selection missing".into()); };
                (*project, capture.revision_fence(), cancellation)
            }
            StyloLiveProof::Unavailable { attempt, cancellation } => {
                let StyloAnalysisRecord::Attempt { project, .. } = attempt.row() else { return Err("Stylo attempt missing".into()); };
                (*project, attempt.revision_fence(), cancellation)
            }
            StyloLiveProof::Retirement { .. } => return Err("Stylo retirement is not a mixed capture".into()),
        };
        if !std::sync::Arc::ptr_eq(cancellation, expected_cancellation) { return Err("Stylo cancellation custody changed".into()); }
        let retained = fence.checked_retained_bytes().and_then(|n| n.checked_add(std::mem::size_of::<Self>()))
            .and_then(|n| n.checked_add(5 * std::mem::size_of::<usize>())).ok_or("Stylo fence size overflow")?;
        if retained > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES { return Err("Stylo fence retained size exceeds bound".into()); }
        validate_live_stylo_noop_for_target(snapshot, snapshot, proof)?;
        let fence = fence.clone();
        let cancellation = std::sync::Arc::clone(cancellation);
        Ok(Self { basis: stylo_basis_for_snapshot(snapshot)?, owner_epoch: snapshot.owner_epoch(), project,
            project_commitment: selected_project_commitment(snapshot, &project)?, fence, cancellation, retained_bytes: retained })
    }

    pub(super) fn validate(&self, base: &backend_engine::WorkspaceSnapshot, target: &backend_engine::WorkspaceSnapshot) -> Result<(), String> {
        if self.retained_bytes > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES { return Err("Stylo fence retained size exceeds bound".into()); }
        if self.cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Stylo grant cancelled".into());
        }
        if base.owner_epoch() != self.owner_epoch || stylo_basis_for_snapshot(base)? != self.basis
            || stylo_basis_for_snapshot(target)?.analysis_root != self.basis.analysis_root
            || selected_project_commitment(base, &self.project)? != self.project_commitment
            || selected_project_commitment(target, &self.project)? != self.project_commitment
        { return Err("Stylo selected scope changed before grant".into()); }
        let current = super::ingest::compiler_revision_is_current_with_cancellation(&self.fence, Some(self.cancellation.as_ref()))?;
        if self.cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Stylo grant cancelled".into());
        }
        if !current { return Err("Stylo source or policy changed before grant".into()); }
        Ok(())
    }
}

pub(super) fn stylo_basis_for_snapshot(
    snapshot: &backend_engine::WorkspaceSnapshot,
) -> Result<super::profile::StyloAnalysisBasis, String> {
    let analysis_root = stylo_analysis_relation(snapshot)
        .map_err(|error| error.to_string())?
        .map(|relation| *relation.root().as_bytes());
    Ok(super::profile::StyloAnalysisBasis {
        workspace_root: *snapshot.root().as_bytes(),
        sequence: snapshot.sequence(),
        closure: *snapshot.closure().binding().closure().as_bytes(),
        analysis_root,
    })
}

/// Recompute an exact project delta from the private live capability.
pub(super) fn plan_stylo_proof(
    base: &backend_engine::WorkspaceSnapshot,
    proof: &StyloLiveProof,
) -> Result<Vec<StyloAnalysisChange>, String> {
    match proof {
        StyloLiveProof::Captured {
            capture,
            cancellation,
            ..
        } => plan_stylo_project_update(base, capture, cancellation.as_ref()),
        StyloLiveProof::Unavailable {
            attempt,
            cancellation,
        } => plan_stylo_unavailable(base, attempt, cancellation.as_ref()),
        StyloLiveProof::Retirement {
            project,
            cancellation,
        } => plan_stylo_retirement(base, *project, cancellation.as_ref()),
    }
}

/// Revalidate a source no-op immediately before its owner acknowledgement.
/// This checks the private CSS/policy fence and exact selected project/rows;
/// a matching native root alone does not prove host bytes are still current.
pub(super) fn validate_stylo_noop(
    snapshot: &backend_engine::WorkspaceSnapshot,
    proof: &StyloGrantProof,
) -> Result<(), String> {
    validate_stylo_noop_for_target(snapshot, snapshot, proof)
}

/// Recheck an unchanged Stylo selection against its exact proposed Project.
pub(super) fn validate_stylo_noop_for_target(
    base: &backend_engine::WorkspaceSnapshot,
    target: &backend_engine::WorkspaceSnapshot,
    proof: &StyloGrantProof,
) -> Result<(), String> {
    match proof {
        StyloGrantProof::Live(proof) => validate_live_stylo_noop_for_target(base, target, proof),
        StyloGrantProof::Selected(proof) => proof.validate(base, target),
    }
}

fn validate_live_stylo_noop_for_target(
    base: &backend_engine::WorkspaceSnapshot,
    target: &backend_engine::WorkspaceSnapshot,
    proof: &StyloLiveProof,
) -> Result<(), String> {
    let (project, expected) = match proof {
        StyloLiveProof::Captured {
            capture, project, ..
        } => {
            let StyloAnalysisRecord::Selection { project: owner, .. } = capture.selection() else {
                return Err("Stylo no-op selection missing".into());
            };
            (*owner, project)
        }
        StyloLiveProof::Unavailable { attempt, .. } => {
            let StyloAnalysisRecord::Attempt { project: owner, .. } = attempt.row() else {
                return Err("Stylo no-op attempt missing".into());
            };
            (*owner, attempt.project())
        }
        StyloLiveProof::Retirement { .. } => {
            return Err("Stylo retirement cannot be a source no-op".into());
        }
    };
    let source = target
        .relation::<backend_engine::ProductSourceRelation>()
        .map_err(|error| error.to_string())?;
    let selected = source.lookup(&project).map_err(|error| error.to_string())?;
    if selected.as_ref() != Some(expected) {
        return Err("Stylo no-op source Project changed".into());
    }
    if !plan_stylo_proof(base, proof)?.is_empty() {
        return Err("Stylo no-op native selection changed".into());
    }
    Ok(())
}

/// Exact observed before-row and requested after-row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StyloAnalysisChange {
    pub(super) key: [u8; 32],
    pub(super) expected: Option<StyloAnalysisRecord>,
    pub(super) after: Option<StyloAnalysisRecord>,
}

/// Plans a bounded exact replacement for the captured owning project. Existing
/// rows from other projects remain outside the returned change set.
///
/// This does not authorize a fresh transition on its own. Callers must carry
/// the private capture proof to owner admission and recheck its source fence.
pub(super) fn plan_stylo_project_update(
    base: &backend_engine::WorkspaceSnapshot,
    capture: &StyloProjectCapture,
    cancellation: &AtomicBool,
) -> Result<Vec<StyloAnalysisChange>, String> {
    capture
        .verify(cancellation)
        .map_err(|error| format!("Stylo capture proof: {error:?}"))?;
    let StyloAnalysisRecord::Selection { project, .. } = capture.selection() else {
        return Err("Stylo capture selection missing".into());
    };
    let mut selected = BTreeMap::new();
    let mut encoded_work = 0usize;
    if let Some(relation) = stylo_analysis_relation(base).map_err(|error| error.to_string())? {
        let mut after = None;
        loop {
            let page = relation
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|error| error.to_string())?;
            for (key, row) in page.entries() {
                if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                    return Err("Stylo planning cancelled".into());
                }
                let mut encoded = Vec::new();
                StyloAnalysisRelation::encode_value(row, &mut encoded);
                encoded_work = encoded_work
                    .checked_add(encoded.len())
                    .ok_or("Stylo planning work overflow")?;
                if encoded_work > 64 * 1024 * 1024 || selected.len() >= 262_144 {
                    return Err("Stylo planning exceeds relation scan budget".into());
                }
                if selected.insert(*key, row.clone()).is_some() {
                    return Err("duplicate Stylo relation row".into());
                }
            }
            match page.next().copied() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
    }
    let selected_refs: Vec<_> = selected.iter().map(|(key, row)| (*key, row)).collect();
    admit_stylo_relation(&selected_refs)?;
    let owned_hosts: BTreeSet<_> = selected
        .iter()
        .filter_map(|(key, row)| match row {
            StyloAnalysisRecord::Host { project: owner, .. } if owner == project => Some(*key),
            _ => None,
        })
        .collect();
    let before: BTreeMap<_, _> = selected
        .iter()
        .filter(|(_, row)| match row {
            StyloAnalysisRecord::Selection { project: owner, .. }
            | StyloAnalysisRecord::Attempt { project: owner, .. }
            | StyloAnalysisRecord::Host { project: owner, .. } => owner == project,
            StyloAnalysisRecord::Fact { host, .. } => owned_hosts.contains(host),
        })
        .map(|(key, row)| (*key, row.clone()))
        .collect();
    let after: BTreeMap<_, _> = capture
        .rows()
        .into_iter()
        .map(|(key, row)| (key, row.clone()))
        .collect();
    // Bound the merged target, not just each project's captured replacement.
    // A new row must never replace an unrelated project's row on key collision.
    for key in before.keys() {
        selected.remove(key);
    }
    for (key, row) in &after {
        if cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Stylo planning cancelled".into());
        }
        if selected.insert(*key, row.clone()).is_some() {
            return Err("Stylo project key collides with another project".into());
        }
    }
    if selected.len() > 262_144 {
        return Err("Stylo target exceeds relation row budget".into());
    }
    let mut target_bytes = 0usize;
    for row in selected.values() {
        if cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Stylo planning cancelled".into());
        }
        let mut encoded = Vec::new();
        StyloAnalysisRelation::encode_value(row, &mut encoded);
        target_bytes = target_bytes
            .checked_add(encoded.len())
            .ok_or("Stylo target work overflow")?;
        if target_bytes > 64 * 1024 * 1024 {
            return Err("Stylo target exceeds relation byte budget".into());
        }
    }
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    Ok(keys
        .into_iter()
        .filter_map(|key| {
            let expected = before.get(&key).cloned();
            let after = after.get(&key).cloned();
            (expected != after).then_some(StyloAnalysisChange {
                key,
                expected,
                after,
            })
        })
        .collect())
}

/// Plans one unavailable attempt while retaining any selected native facts.
pub(super) fn plan_stylo_unavailable(
    base: &backend_engine::WorkspaceSnapshot,
    attempt: &StyloAttemptCapture,
    cancellation: &AtomicBool,
) -> Result<Vec<StyloAnalysisChange>, String> {
    use backend_engine::builtin::{
        StyloAttemptOutcome, stylo_attempt_key, stylo_rows_digest, stylo_selection_key,
    };
    attempt
        .verify(cancellation)
        .map_err(|error| format!("Stylo attempt proof: {error:?}"))?;
    let StyloAnalysisRecord::Attempt {
        project,
        served_selection,
        outcome: StyloAttemptOutcome::Unavailable(_),
        ..
    } = attempt.row()
    else {
        return Err("unavailable Stylo attempt required".into());
    };
    if attempt.key() != stylo_attempt_key(*project) {
        return Err("invalid Stylo attempt key".into());
    }
    let relation = stylo_analysis_relation(base).map_err(|error| error.to_string())?;
    let before = relation
        .as_ref()
        .map(|relation| relation.lookup(&attempt.key()))
        .transpose()
        .map_err(|error| error.to_string())?
        .flatten();
    let selected = relation
        .as_ref()
        .map(|relation| relation.lookup(&stylo_selection_key(*project)))
        .transpose()
        .map_err(|error| error.to_string())?
        .flatten();
    let digest = selected
        .as_ref()
        .map(|selection| stylo_rows_digest(&[(stylo_selection_key(*project), selection)]))
        .transpose()?;
    if *served_selection != digest {
        return Err("Stylo unavailable attempt changed its retained selection".into());
    }
    if before.as_ref() == Some(attempt.row()) {
        return Ok(Vec::new());
    }
    Ok(vec![StyloAnalysisChange {
        key: attempt.key(),
        expected: before,
        after: Some(attempt.row().clone()),
    }])
}

/// Plans exact deletion of every selected native row owned by one project.
/// The caller must additionally prove that the same transition deletes the
/// source Project; retirement does not assert new producer observations.
pub(super) fn plan_stylo_retirement(
    base: &backend_engine::WorkspaceSnapshot,
    project: [u8; 32],
    cancellation: &AtomicBool,
) -> Result<Vec<StyloAnalysisChange>, String> {
    let Some(relation) = stylo_analysis_relation(base).map_err(|error| error.to_string())? else {
        return Ok(Vec::new());
    };
    let mut rows = BTreeMap::new();
    let mut after = None;
    let mut bytes = 0usize;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| error.to_string())?;
        for (key, row) in page.entries() {
            if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                return Err("Stylo retirement cancelled".into());
            }
            let mut encoded = Vec::new();
            StyloAnalysisRelation::encode_value(row, &mut encoded);
            bytes = bytes
                .checked_add(encoded.len())
                .ok_or("Stylo retirement overflow")?;
            if bytes > 64 * 1024 * 1024 || rows.len() >= 262_144 {
                return Err("Stylo retirement exceeds relation budget".into());
            }
            rows.insert(*key, row.clone());
        }
        match page.next().copied() {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let refs = rows
        .iter()
        .map(|(key, row)| (*key, row))
        .collect::<Vec<_>>();
    admit_stylo_relation(&refs)?;
    let hosts = rows
        .iter()
        .filter_map(|(key, row)| match row {
            StyloAnalysisRecord::Host { project: owner, .. } if *owner == project => Some(*key),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    Ok(rows
        .into_iter()
        .filter_map(|(key, row)| {
            let owned = match &row {
                StyloAnalysisRecord::Selection { project: owner, .. }
                | StyloAnalysisRecord::Attempt { project: owner, .. }
                | StyloAnalysisRecord::Host { project: owner, .. } => *owner == project,
                StyloAnalysisRecord::Fact { host, .. } => hosts.contains(host),
            };
            owned.then_some(StyloAnalysisChange {
                key,
                expected: Some(row),
                after: None,
            })
        })
        .collect())
}
