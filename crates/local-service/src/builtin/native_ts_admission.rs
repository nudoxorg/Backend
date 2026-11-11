//! Private native TypeScript source and compiler candidate admission.

use super::commands::index::AdmittedCapturePublication;
use super::{BuiltinModelError, BuiltinWorkspaceRelation, ProductSourceRecord, ingest};
use backend_engine::application::LocalCompilerClient;
use backend_engine::builtin::{
    ProductSemanticCaptureOutcome, SemanticSourceCapture, semantic_capture_relation,
};
use backend_engine::{Relation, WorkspaceSnapshot};
use backend_semantic::vocabulary::LanguageProfile;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Compact identity copied only from a typed workspace snapshot. It is a
/// binding to recheck, not a capability for reading workspace records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeTsBase {
    root: [u8; 32],
    sequence: u64,
    commit: [u8; 32],
    closure: [u8; 32],
    owner_epoch: u64,
    evidence_basis: super::profile::NativeEvidenceBasis,
}

impl NativeTsBase {
    fn from_snapshot(snapshot: &WorkspaceSnapshot) -> Result<Self, BuiltinModelError> {
        Ok(Self {
            root: snapshot.root().to_bytes(),
            sequence: snapshot.sequence(),
            commit: *snapshot.commit().id().as_bytes(),
            closure: *snapshot.closure().binding().closure().as_bytes(),
            owner_epoch: snapshot.owner_epoch(),
            evidence_basis: super::profile::native_evidence_basis_for_snapshot(snapshot)?,
        })
    }
    fn matches(&self, snapshot: &WorkspaceSnapshot) -> Result<bool, BuiltinModelError> {
        Ok(*self == Self::from_snapshot(snapshot)?)
    }
}

fn project_commitment(project: &ProductSourceRecord) -> Result<[u8; 32], BuiltinModelError> {
    let mut encoded = Vec::new();
    backend_engine::builtin::ProductSourceRelation::encode_value(project, &mut encoded);
    if encoded.len() > ProductSourceRecord::ROW_VALUE_CAPACITY {
        return Err(BuiltinModelError(
            "native source project exceeds its row bound".into(),
        ));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-typescript-project-record.v1\0");
    hash.update(&encoded);
    Ok(*hash.finalize().as_bytes())
}

fn charge_allocation(total: &mut usize, capacity: usize) -> Option<()> {
    if capacity != 0 {
        *total = total
            .checked_add(capacity)?
            .checked_add(2 * std::mem::size_of::<usize>())?;
    }
    Some(())
}

fn charge_row_path(
    total: &mut usize,
    row: &backend_engine::builtin::NativeSourceEvidence,
) -> Option<()> {
    charge_allocation(total, row.path.capacity())?;
    if let Some(lines) = &row.line_starts {
        charge_allocation(total, lines.capacity().checked_mul(std::mem::size_of::<u32>())?)?;
    }
    if let Some(headers) = &row.headers {
        charge_allocation(total, headers.entries.capacity().checked_mul(std::mem::size_of::<backend_engine::builtin::NativeHeaderEntry>())?)?;
    }
    Some(())
}

fn charge_change_paths(total: &mut usize, change: &NativeTsEvidenceChange) -> Option<()> {
    if let Some(expected) = &change.expected {
        charge_row_path(total, expected)?;
    }
    charge_row_path(total, &change.after)
}

/// Private candidate admission; it is not a selected workspace transition.
/// It retains a compact post-Pending binding, not a workspace snapshot Arc.
pub(super) struct NativeTsOwnerCandidate {
    base: NativeTsBase,
    project_commitment: [u8; 32],
    publication: AdmittedCapturePublication,
    pending: backend_engine::builtin::ProductSemanticCaptureRecord,
    revalidated: Arc<ingest::RevalidatedNativeTsCapture>,
    compiler_closure: [u8; 32],
    program_identity: [u8; 32],
    program_paths: Vec<String>,
    image_digests: Box<[[u8; 32]]>,
    evidence: Vec<backend_engine::builtin::NativeSourceEvidence>,
    evidence_basis: super::profile::NativeEvidenceBasis,
    evidence_changes: Vec<NativeTsEvidenceChange>,
    evidence_work: usize,
}

/// Exact positive replacement derived from the admitted base; this does not
/// authorize removal of other rows or an exhaustive inventory claim.
#[derive(Clone)]
pub(super) struct NativeTsEvidenceChange {
    key: [u8; 32],
    expected: Option<backend_engine::builtin::NativeSourceEvidence>,
    after: backend_engine::builtin::NativeSourceEvidence,
}

/// Exact selected positive evidence to retire. This is not a claim that the
/// host file was deleted or absent from discovery.
#[derive(Clone)]
pub(super) struct NativeTsEvidenceRetirement {
    key: [u8; 32],
    expected: backend_engine::builtin::NativeSourceEvidence,
}
impl NativeTsEvidenceRetirement {
    pub(super) fn key(&self) -> [u8; 32] { self.key }
    pub(super) fn expected(&self) -> &backend_engine::builtin::NativeSourceEvidence { &self.expected }
}

/// A bounded positive update plan. It does not authorize a target workspace
/// or claim complete discovery; publication and replay validate those separately.
pub(super) struct NativeTsEvidencePlan {
    base: NativeTsBase,
    delta_state: NativeTsDeltaState,
    candidates: Vec<Arc<NativeTsOwnerCandidate>>,
    changes: Vec<NativeTsEvidenceChange>,
    retirements: Vec<NativeTsEvidenceRetirement>,
    scan_work: usize,
    change_work: usize,
}

/// What the freshly admitted positive Program observed against the exact
/// selected evidence root. Neither case claims complete discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeTsDeltaState {
    /// At least one exact positive evidence row changes.
    Changed,
    /// A nonempty fresh positive set exactly equals the prior owner TS rows.
    Unchanged,
}

/// Stability of discovery-policy inputs beyond the captured walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativePolicyStability {
    /// Ambient discovery-policy inputs are not fully witnessed.
    Unknown,
}

/// Positive bytes observed at a walk, without a claim about current policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeTsSourceBasis {
    CapturedObserved {
        policy_stability: NativePolicyStability,
    },
}

/// A worker-refreshed plan ready for transient staging. It still cannot admit
/// a target transition; profile must validate the delta and closure.
pub(super) struct RefreshedNativeTsEvidencePlan {
    plan: Arc<NativeTsEvidencePlan>,
    refresh_read_bytes: usize,
    source_basis: NativeTsSourceBasis,
}

impl RefreshedNativeTsEvidencePlan {
    pub(super) fn matches_cancellation(&self, cancellation: &Arc<std::sync::atomic::AtomicBool>) -> bool {
        self.plan.candidates[0].revalidated.matches_cancellation(cancellation)
    }
    pub(super) fn source_basis(&self) -> NativeTsSourceBasis {
        self.source_basis
    }
    pub(super) fn plan(&self) -> &NativeTsEvidencePlan {
        &self.plan
    }
    pub(super) fn refresh_read_bytes(&self) -> usize {
        self.refresh_read_bytes
    }
    /// Includes the enclosing `Arc<Self>` header and the retained plan charge.
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        std::mem::size_of::<Self>()
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(self.plan.checked_retained_bytes()?)
    }
    pub(super) fn validate_base_before_commit(
        &self,
        current: &WorkspaceSnapshot,
    ) -> Result<(), BuiltinModelError> {
        self.plan.validate_base_before_commit(current)
    }
}

/// Rechecks the original source observation and exact admitted compiler
/// closures on a background worker. No source buffers or activated images are
/// retained by the returned proof.
pub(super) fn refresh_native_ts_evidence_plan(
    base: &WorkspaceSnapshot,
    plan: Arc<NativeTsEvidencePlan>,
    compiler: &LocalCompilerClient,
    semantic_authority: &super::semantic_authority::SemanticAuthority,
    bytes_read: &mut usize,
) -> Result<RefreshedNativeTsEvidencePlan, BuiltinModelError> {
    plan.validate_base_before_commit(base)?;
    let before = *bytes_read;
    plan.candidates[0]
        .revalidated
        .revalidate_again(bytes_read)
        .map_err(BuiltinModelError)?;
    let refresh_read_bytes = bytes_read
        .checked_sub(before)
        .ok_or_else(|| BuiltinModelError("native TypeScript refresh accounting overflow".into()))?;
    let loader = semantic_authority.capture_image_loader()?;
    for candidate in &plan.candidates {
        let selected =
            match loader.selection(candidate.publication.key(), candidate.publication.claim())? {
                super::generation_residence::SelectedSemanticImageSelection::Admitted(selected)
                    if selected.input_digest() == candidate.pending.capture().input_digest()
                        && selected.closure_id() == &candidate.compiler_closure =>
                {
                    selected
                }
                _ => {
                    return Err(BuiltinModelError(
                        "native TypeScript admitted compiler closure changed".into(),
                    ));
                }
            };
        let _selected = selected;
        let mut generations = super::generation_residence::SemanticGenerationResidence::default();
        generations.install_selected_loader(Arc::clone(&loader));
        let mut image_rows = super::view_build::ImageRowResidence::default();
        let activated = super::activate_semantic_publication(
            compiler,
            candidate.publication.key(),
            candidate.publication.claim(),
            &mut generations,
            &mut image_rows,
        )?;
        let program = activated
            .native_program_sources
            .as_ref()
            .ok_or_else(|| BuiltinModelError("native TypeScript program is unavailable".into()))?;
        if program.membership_identity() != candidate.program_identity
            || program_package_paths(program)? != candidate.program_paths
            || !activated
                .images()
                .iter()
                .map(|image| image.content_digest())
                .eq(candidate.image_digests.iter().copied())
            || candidate
                .revalidated
                .program_image_evidence(&activated, candidate.publication.native_headers())
                .map_err(BuiltinModelError)?
                != candidate.evidence
        {
            return Err(BuiltinModelError(
                "native TypeScript program or images changed during refresh".into(),
            ));
        }
        #[cfg(test)]
        super::ingest::test_observe_native_program(
            *candidate.publication.claim().binding().identity.as_ref(), program,
        ).map_err(BuiltinModelError)?;
    }
    plan.validate_base_before_commit(base)?;
    Ok(RefreshedNativeTsEvidencePlan {
        plan,
        refresh_read_bytes,
        source_basis: NativeTsSourceBasis::CapturedObserved {
            policy_stability: NativePolicyStability::Unknown,
        },
    })
}

impl NativeTsEvidencePlan {
    pub(super) fn delta_state(&self) -> NativeTsDeltaState {
        self.delta_state
    }
    pub(super) fn owner(&self) -> [u8; 32] {
        self.candidates[0].revalidated.project()
    }
    /// Conservative queue-retention charge, independent of encoded intent
    /// bytes and source rewalk I/O. Shared candidate/capture Arcs are counted
    /// for every reference, so shared ownership can only increase the charge.
    /// Includes the enclosing `Arc<Self>` header.
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        let mut total =
            std::mem::size_of::<Self>().checked_add(2 * std::mem::size_of::<usize>())?;
        charge_allocation(
            &mut total,
            self.candidates
                .capacity()
                .checked_mul(std::mem::size_of::<Arc<NativeTsOwnerCandidate>>())?,
        )?;
        charge_allocation(
            &mut total,
            self.changes
                .capacity()
                .checked_mul(std::mem::size_of::<NativeTsEvidenceChange>())?,
        )?;
        for change in &self.changes {
            charge_change_paths(&mut total, change)?;
        }
        charge_allocation(&mut total, self.retirements.capacity()
            .checked_mul(std::mem::size_of::<NativeTsEvidenceRetirement>())?)?;
        for retirement in &self.retirements {
            charge_row_path(&mut total, &retirement.expected)?;
        }
        for candidate in &self.candidates {
            total = total.checked_add(candidate.checked_retained_bytes()?)?;
        }
        Some(total)
    }
    pub(super) fn base(&self) -> NativeTsBase {
        self.base
    }
    pub(super) fn candidates(&self) -> &[Arc<NativeTsOwnerCandidate>] {
        &self.candidates
    }
    pub(super) fn evidence_basis(&self) -> super::profile::NativeEvidenceBasis {
        self.candidates[0].evidence_basis
    }
    pub(super) fn changes(&self) -> &[NativeTsEvidenceChange] {
        &self.changes
    }
    pub(super) fn retirements(&self) -> &[NativeTsEvidenceRetirement] {
        &self.retirements
    }
    pub(super) fn scan_work(&self) -> usize {
        self.scan_work
    }
    /// The shared original rewalk is charged once even with two profiles.
    pub(super) fn rewalk_bytes(&self) -> usize {
        self.candidates[0].rewalk_bytes()
    }
    /// Conservative framing charge; an absent before-row can overcount by four bytes.
    pub(super) fn change_work(&self) -> usize {
        self.change_work
    }
    pub(super) fn validate_base_before_commit(
        &self,
        current: &WorkspaceSnapshot,
    ) -> Result<(), BuiltinModelError> {
        for candidate in &self.candidates {
            candidate.validate_base_before_commit(current)?;
        }
        Ok(())
    }
}

/// Merges the same source observed in multiple programs without promoting a
/// source whose identity or captured line map differs.
fn merge_program_source_claim(
    prior: &mut backend_engine::builtin::NativeSourceEvidence,
    row: &backend_engine::builtin::NativeSourceEvidence,
) -> Result<(), BuiltinModelError> {
    let mut comparable = row.clone();
    comparable.kind = prior.kind;
    comparable.headers = prior.headers.clone();
    if *prior != comparable {
        return Err(BuiltinModelError(
            "native TypeScript candidates disagree on a source".into(),
        ));
    }
    if prior.admits_image() && row.admits_image() && prior.headers != row.headers {
        return Err(BuiltinModelError(
            "native TypeScript candidates disagree on image headers".into(),
        ));
    }
    if row.admits_image() && !prior.admits_image() {
        prior.kind = row.kind;
        prior.headers.clone_from(&row.headers);
    }
    Ok(())
}

#[cfg(test)]
mod program_source_claim_tests {
    use super::merge_program_source_claim;
    use backend_engine::builtin::{NativeSourceEvidence, NativeSourceEvidenceKind};
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    #[test]
    fn image_claim_wins_only_for_the_same_captured_member() {
        let member = NativeSourceEvidence {
            project: [1; 32], profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            path: "a.ts".into(), input_digest: [2; 32], source_digest: [3; 32],
            byte_length: 5, kind: NativeSourceEvidenceKind::ProgramMember,
            line_starts: Some(vec![0, 3]),
            headers: None,
        };
        let mut image = member.clone();
        image.kind = NativeSourceEvidenceKind::Image;
        image.headers = Some(backend_engine::builtin::NativeHeaderEvidence {
            image_identity: [4; 32], image_byte_len: 1, producer_recipe: [5; 32], entries: Vec::new(),
        });
        let mut merged = member.clone();
        merge_program_source_claim(&mut merged, &image).expect("same source");
        assert!(merged.admits_image());
        assert_eq!(merged.headers, image.headers);
        merge_program_source_claim(&mut merged, &member).expect("same source in reverse order");
        assert!(merged.admits_image());
        assert_eq!(merged.headers, image.headers);
        let mut conflicting_image = image.clone();
        conflicting_image.headers.as_mut().unwrap().image_identity[0] ^= 1;
        assert!(merge_program_source_claim(&mut merged, &conflicting_image).is_err());
        let mut different = member;
        different.source_digest[0] ^= 1;
        assert!(merge_program_source_claim(&mut merged, &different).is_err());
    }
}

/// The selected Program's mapped paths are separate from its image inventory;
/// member-only evidence never establishes an emitted declaration.
fn program_package_paths(
    program: &backend_semantic::ir::NativeProgramSources,
) -> Result<Vec<String>, BuiltinModelError> {
    let mut paths = Vec::new();
    let mut bytes = 0usize;
    for row in program.sources() {
        let Some(path) = row.package_path else { continue };
        bytes = bytes.checked_add(path.len()).ok_or_else(|| BuiltinModelError("native Program path work overflow".into()))?;
        if paths.len() >= backend_engine::builtin::MAX_NATIVE_EVIDENCE_PROJECT_ROWS
            || bytes > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES
        {
            return Err(BuiltinModelError("native Program paths exceed their bound".into()));
        }
        paths.push(path.to_owned());
    }
    paths.sort_unstable();
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(BuiltinModelError("duplicate native Program package path".into()));
    }
    Ok(paths)
}

/// Joins fresh profiles and plans exact positive replacements and selected
/// evidence retirement. Call on the bounded background worker.
pub(super) fn plan_native_ts_evidence(
    base: &WorkspaceSnapshot,
    candidates: Vec<Arc<NativeTsOwnerCandidate>>,
) -> Result<NativeTsEvidencePlan, BuiltinModelError> {
    use backend_engine::builtin::{
        MAX_NATIVE_EVIDENCE_BYTES, MAX_NATIVE_EVIDENCE_PROJECT_ROWS, MAX_NATIVE_EVIDENCE_ROWS,
        NativeSourceEvidenceRelation,
    };
    if candidates.is_empty() || candidates.len() > 2 {
        return Err(BuiltinModelError(
            "native TypeScript profile count is unsupported".into(),
        ));
    }
    let first = &candidates[0];
    let admitted_base = NativeTsBase::from_snapshot(base)?;
    let owner = first.revalidated.project();
    let mut read_budget = MAX_NATIVE_EVIDENCE_BYTES;
    for candidate in &candidates {
        let retained = candidate.checked_retained_bytes().ok_or_else(|| BuiltinModelError("native candidate resident overflow".into()))?;
        read_budget = read_budget.checked_sub(retained).ok_or_else(|| BuiltinModelError("native candidate resident budget exceeded".into()))?;
    }
    let mut profiles = BTreeSet::new();
    let mut positive = BTreeMap::<[u8; 32], backend_engine::builtin::NativeSourceEvidence>::new();
    let mut changes = BTreeMap::<[u8; 32], NativeTsEvidenceChange>::new();
    let mut positive_work = 0usize;
    let mut change_work = 0usize;
    let max_change_work = MAX_NATIVE_EVIDENCE_BYTES
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(MAX_NATIVE_EVIDENCE_PROJECT_ROWS * 42))
        .ok_or_else(|| BuiltinModelError("native evidence change budget overflow".into()))?;
    for candidate in &candidates {
        if candidate.base != admitted_base
            || candidate.revalidated.project() != owner
            || !Arc::ptr_eq(&candidate.revalidated, &first.revalidated)
            || candidate.project_commitment != first.project_commitment
            || !profiles.insert(candidate.publication.key().profile())
        {
            return Err(BuiltinModelError(
                "native TypeScript candidate bases or profiles differ".into(),
            ));
        }
        for row in &candidate.evidence {
            if first.revalidated.cancelled() {
                return Err(BuiltinModelError(
                    "native TypeScript evidence planning was cancelled".into(),
                ));
            }
            let key = row.key().map_err(|error| BuiltinModelError(error.into()))?;
            if let Some(prior) = positive.get_mut(&key) {
                merge_program_source_claim(prior, row)?;
                continue;
            }
            let mut bytes = Vec::new();
            NativeSourceEvidenceRelation::encode_value(row, &mut bytes);
            positive_work = positive_work
                .checked_add(40)
                .and_then(|work| work.checked_add(bytes.len()))
                .ok_or_else(|| {
                    BuiltinModelError("native positive evidence work overflow".into())
                })?;
            if positive.len() >= MAX_NATIVE_EVIDENCE_PROJECT_ROWS
                || positive_work > MAX_NATIVE_EVIDENCE_BYTES
            {
                return Err(BuiltinModelError(
                    "native positive evidence exceeds its bound".into(),
                ));
            }
            let clone_bytes = row.checked_heap_bytes().and_then(|n| n.checked_add(std::mem::size_of::<backend_engine::builtin::NativeSourceEvidence>() + 256))
                .ok_or_else(|| BuiltinModelError("native positive clone overflow".into()))?;
            read_budget = read_budget.checked_sub(clone_bytes).ok_or_else(|| BuiltinModelError("native positive clone budget exceeded".into()))?;
            positive.insert(key, row.clone());
        }
        for change in &candidate.evidence_changes {
            if first.revalidated.cancelled() {
                return Err(BuiltinModelError(
                    "native TypeScript evidence planning was cancelled".into(),
                ));
            }
            if let Some(prior) = changes.get(&change.key) {
                if prior.expected != change.expected {
                    return Err(BuiltinModelError(
                        "native TypeScript candidates disagree on an evidence delta".into(),
                    ));
                }
                continue;
            }
            let mut charge = 42usize;
            for row in change.expected.iter().chain(std::iter::once(&change.after)) {
                let mut bytes = Vec::new();
                NativeSourceEvidenceRelation::encode_value(row, &mut bytes);
                charge = charge.checked_add(bytes.len()).ok_or_else(|| {
                    BuiltinModelError("native evidence change work overflow".into())
                })?;
            }
            change_work = change_work
                .checked_add(charge)
                .ok_or_else(|| BuiltinModelError("native evidence change work overflow".into()))?;
            if changes.len() >= MAX_NATIVE_EVIDENCE_PROJECT_ROWS || change_work > max_change_work {
                return Err(BuiltinModelError(
                    "native evidence change work exceeds its bound".into(),
                ));
            }
            let mut clone_bytes = std::mem::size_of::<NativeTsEvidenceChange>() + 256;
            charge_change_paths(&mut clone_bytes, change).ok_or_else(|| BuiltinModelError("native change clone overflow".into()))?;
            read_budget = read_budget.checked_sub(clone_bytes).ok_or_else(|| BuiltinModelError("native change clone budget exceeded".into()))?;
            changes.insert(change.key, change.clone());
        }
    }
    // Different profile programs can name the same input while only one
    // emits its image. Publish the strongest matching claim once, comparing
    // against the same selected before-row used by each candidate.
    for (key, change) in &mut changes {
        let replacement = positive.get(key).ok_or_else(|| BuiltinModelError(
            "native evidence delta has no positive program member".into(),
        ))?;
        let clone_bytes = replacement.checked_heap_bytes().and_then(|n| n.checked_add(128))
            .ok_or_else(|| BuiltinModelError("native replacement clone overflow".into()))?;
        read_budget = read_budget.checked_sub(clone_bytes).ok_or_else(|| BuiltinModelError("native replacement clone budget exceeded".into()))?;
        change.after = replacement.clone();
    }
    changes.retain(|_, change| change.expected.as_ref() != Some(&change.after));
    let relation = backend_engine::builtin::native_source_evidence_relation_budgeted(base, &mut read_budget)
        .map_err(|error| BuiltinModelError(format!("open selected native evidence: {error}")))?;
    let mut scan_work = 0usize;
    let mut count = 0usize;
    let mut owner_count = 0usize;
    let mut previous = None;
    let mut retirements = Vec::new();
    let selected_root_exists = relation.is_some();
    let mut matched_positive = 0usize;
    let mut unchanged_rows = true;
    if let Some(relation) = relation {
        let mut after = None;
        loop {
            if first.revalidated.cancelled() {
                return Err(BuiltinModelError(
                    "native TypeScript rewalk was cancelled".into(),
                ));
            }
            let page = backend_engine::builtin::native_source_evidence_page_budgeted(&relation, after.as_ref(), &mut read_budget)
                .map_err(|error| {
                    BuiltinModelError(format!("read selected native evidence: {error}"))
                })?;
            for (key, row) in page.entries() {
                if first.revalidated.cancelled() {
                    return Err(BuiltinModelError(
                        "native TypeScript evidence planning was cancelled".into(),
                    ));
                }
                if row.key().ok() != Some(*key) || previous.is_some_and(|prior| prior >= *key) {
                    return Err(BuiltinModelError(
                        "selected native evidence is not canonical".into(),
                    ));
                }
                previous = Some(*key);
                count = count
                    .checked_add(1)
                    .ok_or_else(|| BuiltinModelError("native evidence count overflow".into()))?;
                if row.project == owner {
                    owner_count = owner_count.checked_add(1).ok_or_else(|| {
                        BuiltinModelError("native evidence project count overflow".into())
                    })?;
                }
                let mut bytes = Vec::new();
                NativeSourceEvidenceRelation::encode_value(row, &mut bytes);
                scan_work = scan_work
                    .checked_add(40)
                    .and_then(|work| work.checked_add(bytes.len()))
                    .ok_or_else(|| BuiltinModelError("native evidence scan overflow".into()))?;
                if count > MAX_NATIVE_EVIDENCE_ROWS
                    || owner_count > MAX_NATIVE_EVIDENCE_PROJECT_ROWS
                    || scan_work > MAX_NATIVE_EVIDENCE_BYTES
                {
                    return Err(BuiltinModelError(
                        "selected native evidence exceeds its bound".into(),
                    ));
                }
                if row.project == owner && matches!(row.profile, LanguageProfile::TypeScript(_)) {
                    if let Some(expected) = positive.get(key) {
                        matched_positive = matched_positive.checked_add(1).ok_or_else(||
                            BuiltinModelError("native positive match count overflow".into()))?;
                        unchanged_rows &= row == expected;
                    } else {
                        let candidate = candidates.iter().find(|candidate|
                            candidate.publication.key().profile() == row.profile
                        ).ok_or_else(|| BuiltinModelError(
                            "native evidence retirement requires a fresh same-profile Program".into(),
                        ))?;
                        if !candidate.evidence.iter().any(|positive| positive.profile == row.profile)
                            || candidate.program_paths.binary_search(&row.path).is_ok()
                        {
                            return Err(BuiltinModelError(
                                "native evidence retirement lacks Program omission".into(),
                            ));
                        }
                        let charge = 42usize.checked_add(bytes.len()).ok_or_else(||
                            BuiltinModelError("native retirement work overflow".into()))?;
                        change_work = change_work.checked_add(charge).ok_or_else(||
                            BuiltinModelError("native retirement work overflow".into()))?;
                        if changes.len().saturating_add(retirements.len()) >= MAX_NATIVE_EVIDENCE_PROJECT_ROWS
                            || change_work > max_change_work
                        {
                            return Err(BuiltinModelError("native retirement exceeds its bound".into()));
                        }
                        let clone_bytes = row.checked_heap_bytes().and_then(|n| n.checked_add(std::mem::size_of::<NativeTsEvidenceRetirement>() + 128))
                            .ok_or_else(|| BuiltinModelError("native retirement clone overflow".into()))?;
                        read_budget = read_budget.checked_sub(clone_bytes).ok_or_else(|| BuiltinModelError("native retirement clone budget exceeded".into()))?;
                        retirements.push(NativeTsEvidenceRetirement { key: *key, expected: row.clone() });
                    }
                }
            }
            match page.next().copied() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
    }
    if first.revalidated.cancelled() {
        return Err(BuiltinModelError(
            "native TypeScript rewalk was cancelled".into(),
        ));
    }
    let delta_state = if changes.is_empty() && retirements.is_empty() {
        if !selected_root_exists || positive.is_empty() || !unchanged_rows
            || matched_positive != positive.len()
        {
            return Err(BuiltinModelError(
                "native unchanged evidence lacks an exact nonempty selected row set".into(),
            ));
        }
        NativeTsDeltaState::Unchanged
    } else {
        NativeTsDeltaState::Changed
    };
    Ok(NativeTsEvidencePlan {
        base: admitted_base,
        delta_state,
        candidates,
        changes: changes.into_values().collect(),
        retirements,
        scan_work,
        change_work,
    })
}

impl NativeTsEvidenceChange {
    pub(super) fn key(&self) -> [u8; 32] {
        self.key
    }
    pub(super) fn expected(&self) -> Option<&backend_engine::builtin::NativeSourceEvidence> {
        self.expected.as_ref()
    }
    pub(super) fn after(&self) -> &backend_engine::builtin::NativeSourceEvidence {
        &self.after
    }
}

impl NativeTsOwnerCandidate {
    fn checked_retained_bytes(&self) -> Option<usize> {
        let mut total =
            std::mem::size_of::<Self>().checked_add(2 * std::mem::size_of::<usize>())?;
        let label = match self.publication.key().package() {
            backend_engine::PackageReference::Local(label) => label,
            _ => return None,
        };
        charge_allocation(&mut total, label.retained_capacity())?;
        // PackageUrl retains an exact Box<str>, whose payload length is exact.
        charge_allocation(
            &mut total,
            self.publication.key().coordinate().as_str().len(),
        )?;
        charge_allocation(&mut total, self.program_paths.capacity()
            .checked_mul(std::mem::size_of::<String>())?)?;
        for path in &self.program_paths { charge_allocation(&mut total, path.capacity())?; }
        charge_allocation(
            &mut total,
            self.image_digests
                .len()
                .checked_mul(std::mem::size_of::<[u8; 32]>())?,
        )?;
        charge_allocation(
            &mut total,
            self.evidence.capacity().checked_mul(std::mem::size_of::<
                backend_engine::builtin::NativeSourceEvidence,
            >())?,
        )?;
        for row in &self.evidence {
            charge_row_path(&mut total, row)?;
        }
        charge_allocation(
            &mut total,
            self.evidence_changes
                .capacity()
                .checked_mul(std::mem::size_of::<NativeTsEvidenceChange>())?,
        )?;
        for change in &self.evidence_changes {
            charge_change_paths(&mut total, change)?;
        }
        // Count both retained private custodies, even if shared by another plan.
        total
            .checked_add(self.publication.native_headers().map_or(0, |headers| headers.retained_bytes()))?
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(self.revalidated.checked_retained_bytes()?)
    }
    pub(super) fn base(&self) -> NativeTsBase {
        self.base
    }
    pub(super) fn project_commitment(&self) -> [u8; 32] {
        self.project_commitment
    }
    pub(super) fn publication(&self) -> &AdmittedCapturePublication {
        &self.publication
    }
    pub(super) fn pending(&self) -> &backend_engine::builtin::ProductSemanticCaptureRecord {
        &self.pending
    }
    pub(super) fn evidence(&self) -> &[backend_engine::builtin::NativeSourceEvidence] {
        &self.evidence
    }
    pub(super) fn evidence_basis(&self) -> super::profile::NativeEvidenceBasis {
        self.evidence_basis
    }
    pub(super) fn evidence_changes(&self) -> &[NativeTsEvidenceChange] {
        &self.evidence_changes
    }
    pub(super) fn evidence_work(&self) -> usize {
        self.evidence_work
    }
    pub(super) fn rewalk_bytes(&self) -> usize {
        self.revalidated.bytes_read()
    }
    /// Checks only the retained base and source fence. Exact target delta,
    /// relation closure and replay admission remain separate requirements.
    pub(super) fn validate_base_before_commit(
        &self,
        current: &WorkspaceSnapshot,
    ) -> Result<(), BuiltinModelError> {
        if self.revalidated.cancelled()
            || !self.base.matches(current)?
            || !self
                .revalidated
                .fence_is_current()
                .map_err(BuiltinModelError)?
            || self.revalidated.cancelled()
        {
            return Err(BuiltinModelError(
                "native TypeScript source or workspace changed before publication".into(),
            ));
        }
        let relation = current
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("open native source project: {error}")))?;
        let project = relation
            .lookup(&self.revalidated.project())
            .map_err(|error| BuiltinModelError(format!("read native source project: {error}")))?
            .ok_or_else(|| BuiltinModelError("native source project is absent".into()))?;
        if project_commitment(&project)? != self.project_commitment {
            return Err(BuiltinModelError(
                "native TypeScript project changed before publication".into(),
            ));
        }
        let captures = semantic_capture_relation(current)
            .map_err(|error| BuiltinModelError(format!("open native source captures: {error}")))?
            .ok_or_else(|| BuiltinModelError("native source capture relation is absent".into()))?;
        if captures
            .lookup(self.publication.key())
            .map_err(|error| BuiltinModelError(format!("read native source capture: {error}")))?
            .as_ref()
            != Some(&self.pending)
        {
            return Err(BuiltinModelError(
                "native TypeScript pending capture changed before publication".into(),
            ));
        }
        if self.revalidated.cancelled()
            || !self
                .revalidated
                .fence_is_current()
                .map_err(BuiltinModelError)?
            || self.revalidated.cancelled()
        {
            return Err(BuiltinModelError(
                "native TypeScript source changed or publication was cancelled".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn admit_native_ts_owner_candidate(
    base: &WorkspaceSnapshot,
    revalidated: Arc<ingest::RevalidatedNativeTsCapture>,
    publication: AdmittedCapturePublication,
    expected_capture: SemanticSourceCapture,
    compiler: &LocalCompilerClient,
    semantic_authority: &super::semantic_authority::SemanticAuthority,
) -> Result<NativeTsOwnerCandidate, BuiltinModelError> {
    if !publication.is_fresh_completion()
        || !publication.key().is_selected()
        || !matches!(publication.key().profile(), LanguageProfile::TypeScript(_))
        || !matches!(
            publication.key().package(),
            backend_engine::PackageReference::Local(_)
        )
        || publication.key().package_key().to_bytes() != revalidated.project()
    {
        return Err(BuiltinModelError(
            "native TypeScript publication belongs to another project or profile".into(),
        ));
    }
    let relation = base
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open native source project: {error}")))?;
    let project = relation
        .lookup(&revalidated.project())
        .map_err(|error| BuiltinModelError(format!("read native source project: {error}")))?
        .ok_or_else(|| BuiltinModelError("native source project is absent".into()))?;
    let fields = project
        .project_fields()
        .ok_or_else(|| BuiltinModelError("native source project row is not a project".into()))?;
    if revalidated.canonical_root().to_str() != Some(fields.label)
        || backend_engine::PackageKey::from_value(fields.label).to_bytes() != revalidated.project()
        || fields.source_version != revalidated.project_source_version()
        || expected_capture.source_version() != &fields.source_version
    {
        return Err(BuiltinModelError(
            "native TypeScript project differs from the captured source basis".into(),
        ));
    }
    // The first slice requires no structural files. Reject from the Project
    // row itself rather than hydrating a potentially large membership tree.
    if fields.files.file_count() != 0 || !fields.files.page_keys().is_empty() {
        return Err(BuiltinModelError(
            "native TypeScript candidate requires an empty structural frontier".into(),
        ));
    }
    let captures = semantic_capture_relation(base)
        .map_err(|error| BuiltinModelError(format!("open native source captures: {error}")))?
        .ok_or_else(|| BuiltinModelError("native source capture relation is absent".into()))?;
    let pending = captures
        .lookup(publication.key())
        .map_err(|error| BuiltinModelError(format!("read native source capture: {error}")))?
        .ok_or_else(|| BuiltinModelError("native source capture is absent".into()))?;
    if pending.capture() != expected_capture
        || pending.cargo_target().is_some()
        || pending.compiler_failure().is_some()
        || !matches!(
            pending.outcome(),
            ProductSemanticCaptureOutcome::Pending { .. }
        )
        || pending.source_workspace_root() != base.root().as_bytes()
        || pending.source_workspace_sequence() != base.sequence()
        || pending.source_commit() != base.commit().id().as_bytes()
    {
        return Err(BuiltinModelError(
            "native TypeScript pending capture differs from the selected source workspace".into(),
        ));
    }
    // Resolve the exact admitted compiler attempt and activate from the same
    // immutable loader snapshot; a source-only hash cannot bind its config.
    let loader = semantic_authority.capture_image_loader()?;
    let selected = match loader.selection(publication.key(), publication.claim())? {
        super::generation_residence::SelectedSemanticImageSelection::Admitted(selected)
            if selected.input_digest() == expected_capture.input_digest() =>
        {
            selected
        }
        _ => {
            return Err(BuiltinModelError(
                "native TypeScript compiler input differs from the pending capture".into(),
            ));
        }
    };
    let mut generations = super::generation_residence::SemanticGenerationResidence::default();
    generations.install_selected_loader(loader);
    let mut image_rows = super::view_build::ImageRowResidence::default();
    let activated = super::activate_semantic_publication(
        compiler,
        publication.key(),
        publication.claim(),
        &mut generations,
        &mut image_rows,
    )?;
    let evidence = revalidated
        .program_image_evidence(&activated, publication.native_headers())
        .map_err(BuiltinModelError)?;
    let program = activated.native_program_sources.as_ref()
        .ok_or_else(|| BuiltinModelError("native TypeScript program is unavailable".into()))?;
    let program_identity = program.membership_identity();
    let program_paths = program_package_paths(program)?;
    let image_digests = activated
        .images()
        .iter()
        .map(|image| image.content_digest())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut read_budget = backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES;
    let evidence_basis = super::profile::native_evidence_basis_for_snapshot_budgeted(base, &mut read_budget)?;
    let selected_evidence = backend_engine::builtin::native_source_evidence_relation_budgeted(base, &mut read_budget)
        .map_err(|error| BuiltinModelError(format!("open selected native evidence: {error}")))?;
    let mut evidence_changes = Vec::with_capacity(evidence.len());
    let mut evidence_work = 0usize;
    for row in &evidence {
        row.validate()
            .map_err(|error| BuiltinModelError(error.into()))?;
        let key = row.key().map_err(|error| BuiltinModelError(error.into()))?;
        let expected = selected_evidence
            .as_ref()
            .map(|relation| backend_engine::builtin::native_source_evidence_lookup_budgeted(relation, &key, &mut read_budget))
            .transpose()
            .map_err(|error| BuiltinModelError(format!("read selected native evidence: {error}")))?
            .flatten();
        if expected.as_ref().is_some_and(|before| {
            before.project != revalidated.project() || before.key().ok() != Some(key)
        }) {
            return Err(BuiltinModelError(
                "selected native evidence has the wrong owner or key".into(),
            ));
        }
        if expected.as_ref() == Some(row) {
            continue;
        }
        let mut charge = 32usize + 2 + 8;
        for value in expected.iter().chain(std::iter::once(row)) {
            let mut bytes = Vec::new();
            backend_engine::builtin::NativeSourceEvidenceRelation::encode_value(value, &mut bytes);
            charge = charge
                .checked_add(bytes.len())
                .ok_or_else(|| BuiltinModelError("native evidence work overflow".into()))?;
        }
        evidence_work = evidence_work
            .checked_add(charge)
            .ok_or_else(|| BuiltinModelError("native evidence work overflow".into()))?;
        let max = backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES
            .checked_mul(2)
            .and_then(|value| {
                value.checked_add(backend_engine::builtin::MAX_NATIVE_EVIDENCE_PROJECT_ROWS * 42)
            })
            .ok_or_else(|| BuiltinModelError("native evidence budget overflow".into()))?;
        if evidence_work > max {
            return Err(BuiltinModelError(
                "native evidence work exceeds its bound".into(),
            ));
        }
        let clone_bytes = row.checked_heap_bytes().and_then(|n| n.checked_add(std::mem::size_of::<NativeTsEvidenceChange>() + 128))
            .ok_or_else(|| BuiltinModelError("native evidence clone overflow".into()))?;
        read_budget = read_budget.checked_sub(clone_bytes).ok_or_else(|| BuiltinModelError("native evidence clone budget exceeded".into()))?;
        evidence_changes.push(NativeTsEvidenceChange {
            key,
            expected,
            after: row.clone(),
        });
    }
    evidence_changes.sort_unstable_by_key(|change| change.key);
    if evidence_changes
        .windows(2)
        .any(|pair| pair[0].key == pair[1].key)
    {
        return Err(BuiltinModelError("duplicate native evidence key".into()));
    }
    let base_binding = NativeTsBase::from_snapshot(base)?;
    let project_commitment = project_commitment(&project)?;
    Ok(NativeTsOwnerCandidate {
        base: base_binding,
        project_commitment,
        publication,
        pending,
        revalidated,
        compiler_closure: *selected.closure_id(),
        program_identity,
        program_paths,
        image_digests,
        evidence,
        evidence_basis,
        evidence_changes,
        evidence_work,
    })
}
