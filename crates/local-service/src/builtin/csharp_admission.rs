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

/// Ephemeral handoff from the selected-source worker. This borrows the actual
/// staged compiler output and cannot be reconstructed from a durable claim.
/// It proves source and attempt correlation only; SDK and execution custody
/// are still required before any publication grant.
pub(super) struct CSharpSelectedCompile<'a> {
    witness: std::sync::Arc<super::commands::index::CapturedCSharpUnitAdmission>,
    attempt: &'a backend_extension_turso::CandidateAttempt,
    staged: &'a backend_engine::application::StagedSemanticPackage,
    cancellation: std::sync::Arc<AtomicBool>,
}

impl<'a> CSharpSelectedCompile<'a> {
    pub(super) fn capture(
        base: &WorkspaceSnapshot,
        witness: std::sync::Arc<super::commands::index::CapturedCSharpUnitAdmission>,
        attempt: &'a backend_extension_turso::CandidateAttempt,
        staged: &'a backend_engine::application::StagedSemanticPackage,
        authority: &super::semantic_authority::SemanticAuthority,
        cancellation: &std::sync::Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let capture = witness.pending_capture().ok_or("C# Pending capture missing")?;
        let key = witness.key();
        let namespace = super::semantic_authority::SemanticAuthority::namespace(key.package(), key.coordinate(), key.profile())
            .map_err(|error| error.to_string())?;
        if !witness.shares_cancellation(cancellation) || cancellation.load(Ordering::Acquire)
            || witness.operation_key().is_none() || capture.operation_key().copied() != witness.operation_key()
            || capture.input_digest() != attempt.input_digest()
            || capture.observation_sequence() != attempt.observation().sequence()
            || attempt.namespace() != &namespace || attempt.observation().observation().namespace() != &namespace
            || authority.recover_candidate_attempt(&attempt.recovery_claim()).map_err(|error| error.to_string())? != *attempt
            || !witness.revalidate(base, cancellation).map_err(|error| error.to_string())?
            || !witness.matches_staged_invocation(staged, *attempt.input_digest())
            || !witness.revalidate(base, cancellation).map_err(|error| error.to_string())?
            || authority.recover_candidate_attempt(&attempt.recovery_claim()).map_err(|error| error.to_string())? != *attempt
            || cancellation.load(Ordering::Acquire)
        { return Err("C# staged invocation or selected attempt changed".into()); }
        Ok(Self { witness, attempt, staged, cancellation: std::sync::Arc::clone(cancellation) })
    }
}

/// Live source/attempt custody for one planned unit delta. It is intentionally
/// insufficient to authorize publication without the execution and SDK leases.
/// Serialized intent bytes cannot construct this value.
pub(super) struct CSharpUnitAdmission {
    witness: std::sync::Arc<super::commands::index::CapturedCSharpUnitAdmission>,
    attempt: backend_extension_turso::CandidateAttempt,
    basis: CSharpReceiptBasis,
    changes: Vec<CSharpReceiptChange>,
    cancellation: std::sync::Arc<AtomicBool>,
}

impl CSharpUnitAdmission {
    pub(super) fn capture(
        base: &WorkspaceSnapshot,
        selected: &CSharpSelectedCompile<'_>,
        authority: &super::semantic_authority::SemanticAuthority,
        replacement: &[([u8; 32], CSharpReceiptRecord)],
    ) -> Result<Self, String> {
        let witness = std::sync::Arc::clone(&selected.witness);
        let attempt = selected.attempt.clone();
        let cancellation = std::sync::Arc::clone(&selected.cancellation);
        if !witness.matches_staged_invocation(selected.staged, *attempt.input_digest()) {
            return Err("C# staged invocation changed".into());
        }
        let recovered = authority.recover_candidate_attempt(&attempt.recovery_claim()).map_err(|error| error.to_string())?;
        if recovered != attempt { return Err("C# candidate attempt changed".into()); }
        let capture = witness.pending_capture().ok_or("C# Pending capture missing")?;
        let key = witness.key();
        let namespace = super::semantic_authority::SemanticAuthority::namespace(key.package(), key.coordinate(), key.profile())
            .map_err(|error| error.to_string())?;
        if !witness.shares_cancellation(&cancellation)
            || cancellation.load(Ordering::Acquire)
            || witness.operation_key().is_none()
            || capture.operation_key().copied() != witness.operation_key()
            || capture.input_digest() != attempt.input_digest()
            || capture.observation_sequence() != attempt.observation().sequence()
            || attempt.namespace() != &namespace
            || attempt.observation().observation().namespace() != &namespace
            || base.with_persisted_transition(|transition, _| transition.request()).ok() != Some(witness.source_request())
            || !witness.revalidate(base, &cancellation).map_err(|error| error.to_string())?
        { return Err("C# selected source or attempt changed".into()); }
        let basis = csharp_receipt_basis(base)?;
        let unit = CSharpReceiptUnit::Project { path: witness.project_path().to_owned() };
        let changes = plan_untrusted_csharp_replacement(base, witness.project_key(), &unit, replacement, cancellation.as_ref())?;
        if changes.is_empty() { return Err("C# unit delta is empty".into()); }
        let extension = super::csharp_intent::CSharpReceiptIntentExtension { basis, changes: changes.clone() };
        extension.encode()?;
        if !witness.revalidate(base, &cancellation).map_err(|error| error.to_string())? {
            return Err("C# source changed while planning".into());
        }
        Ok(Self { witness, attempt, basis, changes, cancellation })
    }

    pub(super) fn revalidate(&self, base: &WorkspaceSnapshot, cancellation: &std::sync::Arc<AtomicBool>) -> Result<(), String> {
        if !std::sync::Arc::ptr_eq(&self.cancellation, cancellation)
            || csharp_receipt_basis(base)? != self.basis
            || !self.witness.revalidate(base, cancellation).map_err(|error| error.to_string())?
        { return Err("C# unit custody changed".into()); }
        let capture = self.witness.pending_capture().ok_or("C# Pending capture missing")?;
        if capture.operation_key().copied() != self.witness.operation_key()
            || capture.input_digest() != self.attempt.input_digest()
            || capture.observation_sequence() != self.attempt.observation().sequence()
        { return Err("C# attempt changed".into()); }
        Ok(())
    }

    /// Reopen the current authoritative attempt. This check must be repeated
    /// at the owner barrier when execution and SDK leases are available.
    pub(super) fn revalidate_attempt(&self, authority: &super::semantic_authority::SemanticAuthority) -> Result<(), String> {
        let recovered = authority.recover_candidate_attempt(&self.attempt.recovery_claim()).map_err(|error| error.to_string())?;
        if recovered != self.attempt || self.cancellation.load(Ordering::Acquire) {
            return Err("C# candidate attempt changed".into());
        }
        Ok(())
    }

    pub(super) fn project(&self) -> [u8; 32] { self.witness.project_key() }
    pub(super) fn operation_key(&self) -> Option<[u8; 32]> { self.witness.operation_key() }

    pub(super) fn extension(&self) -> super::csharp_intent::CSharpReceiptIntentExtension {
        super::csharp_intent::CSharpReceiptIntentExtension { basis: self.basis, changes: self.changes.clone() }
    }

    pub(super) fn retained_bytes(&self) -> Option<usize> {
        self.witness.retained_bytes()?.checked_add(self.extension().encode().ok()?.len())?
            .checked_add(std::mem::size_of::<Self>())
    }
}
