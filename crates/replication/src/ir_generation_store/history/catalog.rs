// Ref publication and admitted-generation bindings share one owner.
use super::codec::*;
use super::provenance::*;
use super::v3::load_typed_v3_history_locator;
use super::*;
use std::collections::HashSet;

const MAX_HISTORY_PAYLOAD_ROOT_WALK: usize = 262_144;

impl HistoryRefCatalog {
    pub(super) fn empty() -> Self {
        Self { refs: Vec::new() }
    }

    pub(super) fn get(
        &self,
        kind: HistoryRefKind,
        name: &HistoryRefName,
    ) -> Option<HistoryCommitId> {
        self.refs
            .iter()
            .find(|reference| reference.kind == kind && reference.name == *name)
            .map(|reference| reference.commit)
    }

    pub(super) fn set(
        &mut self,
        kind: HistoryRefKind,
        name: HistoryRefName,
        commit: Option<HistoryCommitId>,
    ) {
        self.refs
            .retain(|reference| reference.kind != kind || reference.name != name);
        if let Some(commit) = commit {
            self.refs.push(SelectedHistoryRef { kind, name, commit });
        }
        self.refs.sort_by(ref_order);
    }
}

pub(crate) fn read_history_catalog_snapshot(
    target_root: &Path,
) -> Result<(HistoryRefCatalog, [u8; 32]), String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok((HistoryRefCatalog::empty(), *blake3::hash(&[]).as_bytes()));
    }
    let catalog_path = history_root.join("refs.catalog");
    let bytes = read_optional_bounded(&catalog_path, MAX_HISTORY_REFS_BYTES)?
        .ok_or_else(|| "semantic history refs catalog is missing".to_owned())?;
    let digest = *blake3::hash(&bytes).as_bytes();
    Ok((decode_ref_catalog(&bytes)?, digest))
}

pub(super) fn validate_history_commit_node(
    target_root: &Path,
    target: &SemanticTargetKey,
    commits_root: &Path,
    identity: HistoryCommitId,
) -> Result<HistoryCommitRecord, String> {
    let record = load_history_commit(commits_root, identity)?;
    if record.target != *target {
        return Err("semantic history commit belongs to another target".to_owned());
    }
    super::v2::validate_typed_v2_locator_binding(target_root, &record)?;
    super::v3::validate_typed_v3_locator_binding(target_root, &record)?;
    let generation = load_record(target_root, record.generation, target)?;
    validate_commit_generation(&record, &generation)?;
    validate_parent_set(commits_root, &record)?;
    for parent in &record.parents {
        let parent_record = load_history_commit(commits_root, *parent)?;
        super::v2::validate_typed_v2_locator_binding(target_root, &parent_record)?;
        super::v3::validate_typed_v3_locator_binding(target_root, &parent_record)?;
        let parent_generation = load_record(target_root, parent_record.generation, target)?;
        validate_commit_generation(&parent_record, &parent_generation)?;
    }
    Ok(record)
}

fn validate_history_commit_node_for_typed_v2_residency(
    target_root: &Path,
    target: &SemanticTargetKey,
    commits_root: &Path,
    identity: HistoryCommitId,
) -> Result<HistoryCommitRecord, String> {
    let record = load_history_commit(commits_root, identity)?;
    if record.target != *target {
        return Err("semantic history commit belongs to another target".to_owned());
    }
    let generation = load_record(target_root, record.generation, target)?;
    validate_commit_generation(&record, &generation)?;

    // Preserve the direct-parent and generation-snapshot checks used by the
    // strict path. Only locator-body validation is delegated: the exact
    // resident entry binds the current commit's locator ID to the bytes and
    // semantic proof admitted earlier, and ancestors' locator bodies are not
    // inputs to replaying this immutable generation.
    let mut parents = Vec::new();
    parents
        .try_reserve_exact(record.parents.len())
        .map_err(|_| "semantic history parent validation allocation failed".to_owned())?;
    for parent_identity in &record.parents {
        let parent = load_history_commit(commits_root, *parent_identity)?;
        if parent.target != *target {
            return Err("semantic history parent belongs to another target".to_owned());
        }
        let parent_generation = load_record(target_root, parent.generation, target)?;
        validate_commit_generation(&parent, &parent_generation)?;
        parents.push(parent);
    }
    validate_parent_set_with_records(&record, &parents)?;
    Ok(record)
}

pub(super) fn validate_catalog_tips(
    target_root: &Path,
    target: &SemanticTargetKey,
    catalog: &HistoryRefCatalog,
) -> Result<(), String> {
    let commits_root = target_root.join("history").join("commits");
    for reference in &catalog.refs {
        validate_history_commit_node(target_root, target, &commits_root, reference.commit)?;
    }
    Ok(())
}

/// History generation records are retained once the append-only index exists.
/// Metadata reclamation is intentionally coupled to history GC, never to a
/// current/previous cache open that cannot prove the full ref closure.
pub(crate) fn may_prune_generation_records(
    target_root: &Path,
    _target: &SemanticTargetKey,
) -> Result<bool, String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok(true);
    }
    // `current()` and every commit consult this gate. Decode and checksum the
    // bounded ref catalog so corruption remains fail-closed, but do not open
    // every named tip on the hot path. Full tip/ancestry validation belongs
    // to ref reads, replay, and mark/sweep.
    let (catalog, _) = read_history_catalog_snapshot(target_root)?;
    let index_path = history_root.join("commit.index");
    let index_exists = match fs::symlink_metadata(&index_path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata.len() != 0,
        Ok(_) => return Err("semantic history index is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(display_io(error)),
    };
    Ok(catalog.refs.is_empty() && !index_exists)
}

impl LocalSemanticGenerationFiles {
    pub(crate) fn require_local_cache_head_alignment(
        &self,
        target: &SemanticTargetKey,
        cache_head: Option<super::super::LocalHead>,
        incoming_generation: LocalSemanticGenerationId,
        incoming_stamp: SelectedGenerationStamp,
        incoming_manifest_root: backend_semantic::ir::SemanticManifestRoot,
    ) -> Result<(), String> {
        let name = HistoryRefName::new("local-cache")?;
        let Some(reference) = self.history_ref(target, HistoryRefKind::Branch, &name)? else {
            return Ok(());
        };
        let selected = self.history_commit(target, reference.commit())?;
        let retries_durable_ref = selected.record.generation == incoming_generation
            && selected.record.stamp == incoming_stamp
            && selected.record.manifest_root == incoming_manifest_root;
        let follows_cache_head = cache_head.is_some_and(|head| {
            selected.record.generation == head.current && selected.record.stamp == head.stamp
        });
        if retries_durable_ref || follows_cache_head {
            Ok(())
        } else {
            Err(
                "local semantic history ref is ahead of or detached from cache HEAD; reconcile the ref before committing a new selected generation"
                    .to_owned(),
            )
        }
    }

    pub(crate) fn selected_history_payload_root(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<Option<HistoryPayloadRoot>, String> {
        let name = HistoryRefName::new("local-cache")?;
        let Some(reference) = self.history_ref(target, HistoryRefKind::Branch, &name)? else {
            return Ok(None);
        };
        self.history_payload_root(target, reference.commit)
    }

    pub(crate) fn history_payload_root(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
    ) -> Result<Option<HistoryPayloadRoot>, String> {
        let target_root = self.target_root(target);
        let _ = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            commit,
        )?;
        read_history_payload_root(&target_root, commit)
    }

    pub(crate) fn history_payload_roots(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<Vec<ArtifactClosureClaim>, String> {
        let target_root = self.target_root(target);
        let history_root = target_root.join("history");
        if !ensure_optional_directory(&history_root)? {
            return Ok(Vec::new());
        }
        let (catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let mut roots = Vec::with_capacity(catalog.refs.len());
        let mut visited = HashSet::new();
        let commits_root = history_root.join("commits");
        for reference in &catalog.refs {
            if let Some(payload) = read_history_payload_root(&target_root, reference.commit)? {
                roots.push(payload.closure);
            }
            let mut next = Some(reference.commit);
            while let Some(identity) = next {
                if !visited.insert(identity) {
                    break;
                }
                if visited.len() > MAX_HISTORY_PAYLOAD_ROOT_WALK {
                    return Err(
                        "semantic history payload ancestry exceeds its collection bound".to_owned(),
                    );
                }
                let record =
                    validate_history_commit_node(&target_root, target, &commits_root, identity)?;
                match record.generation_root {
                    HistoryGenerationRoot::TypedV2(claim) => {
                        let payload = read_history_payload_root(&target_root, identity)?
                            .ok_or_else(|| {
                                "typed V2 history commit payload closure is missing".to_owned()
                            })?;
                        if payload.closure.as_bytes() != claim.closure.as_bytes() {
                            return Err(
                                "typed V2 history payload closure differs from its commit root"
                                    .to_owned(),
                            );
                        }
                        roots.push(claim.closure);
                    }
                    HistoryGenerationRoot::TypedV3(claim) => {
                        let payload = read_history_payload_root(&target_root, identity)?
                            .ok_or_else(|| {
                                "typed V3 history commit payload closure is missing".to_owned()
                            })?;
                        if payload.closure.as_bytes() != claim.closure().as_bytes() {
                            return Err(
                                "typed V3 history payload closure differs from its commit root"
                                    .to_owned(),
                            );
                        }
                        roots.push(claim.closure());
                    }
                    HistoryGenerationRoot::NxfiV1(_) => {}
                }
                next = record.parents.first().copied();
            }
        }
        roots.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        roots.dedup_by(|left, right| left.as_bytes() == right.as_bytes());
        Ok(roots)
    }

    pub(crate) fn persist_history_segment_mapping(
        &self,
        target: &SemanticTargetKey,
        segment: backend_semantic::ir::UntrustedSemanticSegmentId,
        object: ObjectId,
        byte_length: u64,
    ) -> Result<(), String> {
        let target_root = self.target_root(target);
        super::retention::recover_pending_delete(&target_root)?;
        let history_root = target_root.join("history");
        prepare_history_layout(&target_root)?;
        let directory = history_root.join("segment-map");
        create_private_directory(&directory)?;
        set_private_directory(&directory)?;
        let bytes = encode_history_segment_mapping(segment, object, byte_length)?;
        let path = directory.join(format!("{}.map", hex(segment.as_bytes())));
        match fs::read(&path) {
            Ok(existing) if existing == bytes => Ok(()),
            Ok(_) => Err(
                "semantic history segment mapping conflicts with its content identity".to_owned(),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                reserve_history_segment_mapping(&history_root, &directory)?;
                backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
            }
            Err(error) => Err(display_io(error)),
        }
    }

    pub(crate) fn history_segment_mapping(
        &self,
        target: &SemanticTargetKey,
        segment: backend_semantic::ir::UntrustedSemanticSegmentId,
    ) -> Result<Option<(UntrustedObjectId, u64)>, String> {
        let path = self
            .target_root(target)
            .join("history")
            .join("segment-map")
            .join(format!("{}.map", hex(segment.as_bytes())));
        match read_optional_bounded(&path, MAX_HISTORY_SEGMENT_MAP_BYTES)? {
            Some(bytes) => decode_history_segment_mapping(&bytes, segment).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn advance_history_gc(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<HistoryGcProgress, String> {
        let target_root = self.target_root(target);
        let mut progress = super::gc::advance_history_gc(&target_root, target)?;
        if progress.complete {
            let remaining = MAX_HISTORY_GC_BATCH_RECORDS.saturating_sub(progress.processed_records);
            let retention = super::retention::advance_retention(&target_root, target, remaining)?;
            progress.processed_records = progress
                .processed_records
                .checked_add(retention.processed_records)
                .ok_or_else(|| "semantic history GC work counter overflows".to_owned())?;
            progress.complete = retention.complete;
            progress.stats = retention.stats;
        } else if let Some(stats) = super::retention::read_stats(&target_root)? {
            progress.stats = stats;
        }
        Ok(progress)
    }

    pub(crate) fn propose_history_commit(
        &self,
        target: &SemanticTargetKey,
        parents: &[HistoryCommitId],
        provenance: [u8; 32],
    ) -> Result<UnpublishedHistoryProposal, HistoryProposalError> {
        if parents.len() > MAX_HISTORY_PARENTS {
            return Err(HistoryProposalError::Storage(
                "semantic history commit exceeds its parent bound".to_owned(),
            ));
        }
        if parents.len() == 2 {
            return Err(HistoryProposalError::UnsupportedMergePayloadClosure);
        }
        let generation = self
            .current(target)?
            .ok_or_else(|| "no admitted current generation is available for history".to_owned())?;
        let target_root = self.target_root(target);
        let commits_root = prepare_history_layout(&target_root)?;
        let mut depth = 0_u32;
        for (index, parent) in parents.iter().enumerate() {
            let record =
                validate_history_commit_node(&target_root, target, &commits_root, *parent)?;
            if index == 0 {
                depth = record
                    .first_parent_depth
                    .checked_add(1)
                    .ok_or_else(|| "semantic history depth overflows".to_owned())?;
            }
        }
        let record = HistoryCommitRecord {
            identity: HistoryCommitId([0; 32]),
            target: target.clone(),
            parents: parents.to_vec(),
            generation: generation.identity,
            generation_root: HistoryGenerationRoot::NxfiV1(generation.semantic_generation()),
            manifest_root: generation.manifest.root(),
            stamp: generation.selected_stamp,
            provenance,
            first_parent_depth: depth,
            checkpoint: parents.is_empty() || depth % HISTORY_CHECKPOINT_INTERVAL == 0,
        };
        let (record, identity) = identify_history_record(record)?;
        Ok(UnpublishedHistoryProposal { record, identity })
    }

    pub(crate) fn admit_history_proposal<S: SelectedGenerationSource>(
        &self,
        proposal: UnpublishedHistoryProposal,
        source: &mut S,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let target_root = self.target_root(&proposal.record.target);
        super::retention::recover_pending_delete(&target_root)?;
        let commits_root = prepare_history_layout(&target_root)?;
        let generation = load_record(
            &target_root,
            proposal.record.generation,
            &proposal.record.target,
        )?;
        validate_commit_generation(&proposal.record, &generation)?;
        super::v2::validate_typed_v2_locator_binding(&target_root, &proposal.record)?;
        require_current(source, proposal.record.stamp, generation.image)?;
        validate_parent_set(&commits_root, &proposal.record)?;
        let bytes = encode_history_commit(&proposal.record, proposal.identity)?;
        let path = history_commit_path(&commits_root, proposal.identity);
        let created = match fs::read(&path) {
            Ok(existing) if existing == bytes => false,
            Ok(_) => return Err("immutable semantic history identity collision".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bump_history_commit_epoch(&target_root.join("history"))?;
                backend_platform::durable::write_private_atomic(&path, &bytes)
                    .map_err(display_io)?;
                true
            }
            Err(error) => return Err(display_io(error)),
        };
        #[cfg(test)]
        if created {
            super::trip_history_test_fault(super::HistoryTestFault::AfterHistoryCommit)?;
        }
        let admitted = load_history_commit(&commits_root, proposal.identity)?;
        if admitted != proposal.record {
            return Err("admitted semantic history commit differs from its proposal".to_owned());
        }
        append_commit_index(&target_root, proposal.identity)?;
        Ok(HistoryAdmissionReceipt {
            commit: AdmittedHistoryCommit { record: admitted },
            created,
            _gc_pin: None,
            typed_v2_proof: None,
            typed_v3_proof: None,
        })
    }

    pub(crate) fn history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: &HistoryRefName,
    ) -> Result<Option<SelectedHistoryRef>, String> {
        let target_root = self.target_root(target);
        if !ensure_optional_directory(&target_root.join("history"))? {
            return Ok(None);
        }
        let (catalog, _) = read_history_catalog_snapshot(&target_root)?;
        let Some(commit) = catalog.get(kind, name) else {
            return Ok(None);
        };
        let _ = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            commit,
        )?;
        Ok(Some(SelectedHistoryRef {
            kind,
            name: name.clone(),
            commit,
        }))
    }

    /// Reads the current named-ref tip without reopening its commit graph.
    ///
    /// This deliberately narrow operation is for a live, process-local
    /// `HistoryRefAncestryProof`: that proof already records a validated
    /// first-parent chain and is valid exactly while the named ref still has
    /// the same content-addressed tip. Callers must still validate the proof
    /// binding and must load the exact requested commit independently.
    pub(crate) fn history_ref_tip(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: &HistoryRefName,
    ) -> Result<Option<HistoryCommitId>, String> {
        let target_root = self.target_root(target);
        if !ensure_optional_directory(&target_root.join("history"))? {
            return Ok(None);
        }
        let (catalog, _) = read_history_catalog_snapshot(&target_root)?;
        Ok(catalog.get(kind, name))
    }

    pub(crate) fn history_commit(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<AdmittedHistoryCommit, String> {
        let target_root = self.target_root(target);
        let record = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            identity,
        )?;
        Ok(AdmittedHistoryCommit { record })
    }

    /// Reloads a commit and its direct parent/generation bindings while
    /// delegating only typed V2 locator validation to the caller's exact live
    /// residency entry. Cache misses must still load and validate the current
    /// locator and cold-verify its complete payload closure.
    pub(crate) fn history_commit_for_typed_v2_residency(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<AdmittedHistoryCommit, String> {
        let target_root = self.target_root(target);
        let record = validate_history_commit_node_for_typed_v2_residency(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            identity,
        )?;
        Ok(AdmittedHistoryCommit { record })
    }

    pub(crate) fn history_generation(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<LocalSemanticGeneration, String> {
        let target_root = self.target_root(target);
        let record = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            identity,
        )?;
        if matches!(
            record.generation_root,
            HistoryGenerationRoot::TypedV2(_) | HistoryGenerationRoot::TypedV3(_)
        ) {
            return Err(
                "typed history requires proof-bearing cold replay, not V1 materialization"
                    .to_owned(),
            );
        }
        let generation = load_record(&target_root, record.generation, target)?;
        generation_from_record(generation, record.stamp)
    }

    pub(crate) fn compare_and_swap_history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: HistoryRefName,
        expected: Option<HistoryCommitId>,
        next: Option<HistoryCommitId>,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        self.compare_and_swap_history_ref_inner(target, kind, name, expected, next, None, None)
    }

    pub(crate) fn compare_and_swap_typed_v2_history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: HistoryRefName,
        expected: Option<HistoryCommitId>,
        next: HistoryCommitId,
        admission: &TypedV2HistoryPublicationAdmission<'_>,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        if admission.identity() != next {
            return Err("typed V2 publication receipt names another commit".to_owned());
        }
        self.compare_and_swap_history_ref_inner(
            target,
            kind,
            name,
            expected,
            Some(next),
            Some(admission),
            None,
        )
    }

    pub(crate) fn compare_and_swap_typed_v3_history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: HistoryRefName,
        expected: Option<HistoryCommitId>,
        next: HistoryCommitId,
        admission: &TypedV3HistoryPublicationAdmission<'_>,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        if admission.identity() != next {
            return Err("typed V3 publication receipt names another commit".to_owned());
        }
        self.compare_and_swap_history_ref_inner(
            target,
            kind,
            name,
            expected,
            Some(next),
            None,
            Some(admission),
        )
    }

    fn compare_and_swap_history_ref_inner(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: HistoryRefName,
        expected: Option<HistoryCommitId>,
        next: Option<HistoryCommitId>,
        typed_v2_admission: Option<&TypedV2HistoryPublicationAdmission<'_>>,
        typed_v3_admission: Option<&TypedV3HistoryPublicationAdmission<'_>>,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        let target_root = self.target_root(target);
        super::retention::recover_pending_delete(&target_root)?;
        let commits_root = prepare_history_layout(&target_root)?;
        let (mut catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let actual = catalog.get(kind, &name);
        if let Some(identity) = next {
            let record =
                validate_history_commit_node(&target_root, target, &commits_root, identity)?;
            if kind == HistoryRefKind::Branch
                && name.as_str() == "local-cache"
                && matches!(
                    record.generation_root,
                    HistoryGenerationRoot::TypedV2(_) | HistoryGenerationRoot::TypedV3(_)
                )
            {
                return Err(
                    "typed history cannot be published as the production local-cache ref"
                        .to_owned(),
                );
            }
            match record.generation_root {
                HistoryGenerationRoot::TypedV2(claim) => {
                    let admission = typed_v2_admission.ok_or_else(|| {
                        "typed V2 ref publication requires its live proof-bearing admission receipt"
                            .to_owned()
                    })?;
                    if typed_v3_admission.is_some() {
                        return Err(
                            "typed V3 publication receipt cannot publish a V2 commit".to_owned()
                        );
                    }
                    let content = admission.content();
                    if admission.identity() != identity
                        || admission.closure().as_bytes() != claim.closure().as_bytes()
                        || admission.locator() != claim.locator()
                        || !claim.content_root_claim().matches(content.content_root())
                        || !claim
                            .generation_root_claim()
                            .matches(content.generation_root())
                    {
                        return Err(
                            "typed V2 ref publication receipt differs from the immutable commit"
                                .to_owned(),
                        );
                    }
                    let payload =
                        read_history_payload_root(&target_root, identity)?.ok_or_else(|| {
                            "typed V2 history commit payload closure is missing".to_owned()
                        })?;
                    if payload.closure.as_bytes() != claim.closure().as_bytes() {
                        return Err(
                            "typed V2 history payload closure differs from its commit root"
                                .to_owned(),
                        );
                    }
                }
                HistoryGenerationRoot::TypedV3(claim) => {
                    let admission = typed_v3_admission.ok_or_else(|| {
                        "typed V3 ref publication requires its proof-bearing admission receipt"
                            .to_owned()
                    })?;
                    if typed_v2_admission.is_some() {
                        return Err(
                            "typed V2 publication receipt cannot publish a V3 commit".to_owned()
                        );
                    }
                    let content = admission.content();
                    if admission.identity() != identity
                        || admission.closure().as_bytes() != claim.closure().as_bytes()
                        || admission.locator() != claim.locator()
                        || !claim.content_root_claim().matches(content.content_root())
                        || !claim
                            .generation_root_claim()
                            .matches(content.generation_root())
                    {
                        return Err(
                            "typed V3 publication receipt differs from the immutable commit"
                                .to_owned(),
                        );
                    }
                    let locator =
                        load_typed_v3_history_locator(&target_root, identity, claim.locator())?;
                    let manifest = locator.validate()?;
                    if manifest.input_claim() != admission.input_claim() {
                        return Err(
                            "typed V3 publication input claim differs from its locator".to_owned()
                        );
                    }
                    let generation = load_record(&target_root, record.generation, target)?;
                    let selected_input = backend_semantic::ir::SemanticInputClaimV2::from_witness(
                        &generation.manifest.input(),
                    );
                    if manifest.build() != generation.manifest.build()
                        || selected_input != admission.input_claim()
                    {
                        return Err(
                            "typed V3 publication no longer matches the selected native generation"
                                .to_owned(),
                        );
                    }
                    let payload =
                        read_history_payload_root(&target_root, identity)?.ok_or_else(|| {
                            "typed V3 history commit payload closure is missing".to_owned()
                        })?;
                    if payload.closure.as_bytes() != claim.closure().as_bytes() {
                        return Err(
                            "typed V3 history payload closure differs from its commit root"
                                .to_owned(),
                        );
                    }
                }
                HistoryGenerationRoot::NxfiV1(_) => {
                    if typed_v2_admission.is_some() || typed_v3_admission.is_some() {
                        return Err(
                            "typed publication receipt cannot publish a V1 history commit"
                                .to_owned(),
                        );
                    }
                }
            }
        }
        if (typed_v2_admission.is_some() || typed_v3_admission.is_some()) && actual == next {
            return Ok(HistoryRefUpdateReceipt {
                previous: actual,
                current: next,
            });
        }
        if actual != expected {
            return Err("semantic history reference compare-and-swap failed".to_owned());
        }
        if let Some(previous) = actual {
            if Some(previous) != next {
                append_history_tombstone(&target_root, previous)?;
            }
        }
        catalog.set(kind, name, next);
        let encoded = encode_ref_catalog(&catalog)?;
        backend_platform::durable::write_private_atomic(
            &target_root.join("history").join("refs.catalog"),
            &encoded,
        )
        .map_err(display_io)?;
        #[cfg(test)]
        super::trip_history_test_fault(super::HistoryTestFault::AfterRefsCatalog)?;
        Ok(HistoryRefUpdateReceipt {
            previous: actual,
            current: next,
        })
    }

    pub(crate) fn rename_history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        old_name: &HistoryRefName,
        new_name: HistoryRefName,
        expected: HistoryCommitId,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        let target_root = self.target_root(target);
        super::retention::recover_pending_delete(&target_root)?;
        prepare_history_layout(&target_root)?;
        let (mut catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let actual = catalog.get(kind, old_name);
        if actual != Some(expected) {
            return Err("semantic history reference compare-and-swap failed".to_owned());
        }
        if catalog.get(kind, &new_name).is_some() {
            return Err("semantic history rename destination already exists".to_owned());
        }
        if kind == HistoryRefKind::Branch && new_name.as_str() == "local-cache" {
            let record = validate_history_commit_node(
                &target_root,
                target,
                &target_root.join("history").join("commits"),
                expected,
            )?;
            if matches!(
                record.generation_root,
                HistoryGenerationRoot::TypedV2(_) | HistoryGenerationRoot::TypedV3(_)
            ) {
                return Err(
                    "typed history cannot be renamed as the production local-cache ref".to_owned(),
                );
            }
        }
        catalog.set(kind, old_name.clone(), None);
        catalog.set(kind, new_name, Some(expected));
        let encoded = encode_ref_catalog(&catalog)?;
        backend_platform::durable::write_private_atomic(
            &target_root.join("history").join("refs.catalog"),
            &encoded,
        )
        .map_err(display_io)?;
        Ok(HistoryRefUpdateReceipt {
            previous: Some(expected),
            current: Some(expected),
        })
    }

    pub(crate) fn record_selected_history<S: SelectedGenerationSource>(
        &self,
        generation: &LocalSemanticGeneration,
        source: &mut S,
        payload_root: Option<AdmittedHistoryPayloadRoot>,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let target = &generation.target;
        let target_root = self.target_root(target);
        super::retention::recover_pending_delete(&target_root)?;
        prepare_history_layout(&target_root)?;
        let name = HistoryRefName::new("local-cache")?;
        let previous = self.history_ref(target, HistoryRefKind::Branch, &name)?;
        if let Some(reference) = &previous {
            let selected = self.history_commit(target, reference.commit)?;
            if selected.record.generation == generation.identity
                && selected.record.stamp == generation.selected_stamp
                && selected.record.manifest_root == generation.manifest.root()
            {
                require_current(source, generation.selected_stamp, generation.image)?;
                return Ok(HistoryAdmissionReceipt {
                    commit: selected,
                    created: false,
                    _gc_pin: None,
                    typed_v2_proof: None,
                    typed_v3_proof: None,
                });
            }
        }
        let parents = previous
            .iter()
            .map(SelectedHistoryRef::commit)
            .collect::<Vec<_>>();
        let provenance = selected_generation_provenance(generation);
        let first_parent_depth = if let Some(parent) = parents.first() {
            load_history_commit(&target_root.join("history").join("commits"), *parent)?
                .first_parent_depth
                .checked_add(1)
                .ok_or_else(|| "semantic history depth overflows".to_owned())?
        } else {
            0
        };
        let record = HistoryCommitRecord {
            identity: HistoryCommitId([0; 32]),
            target: target.clone(),
            parents: parents.clone(),
            generation: generation.identity,
            generation_root: HistoryGenerationRoot::NxfiV1(generation.semantic_generation()),
            manifest_root: generation.manifest.root(),
            stamp: generation.selected_stamp,
            provenance,
            first_parent_depth,
            checkpoint: parents.is_empty() || first_parent_depth % HISTORY_CHECKPOINT_INTERVAL == 0,
        };
        let (record, identity) = identify_history_record(record)?;
        let admission =
            self.admit_history_proposal(UnpublishedHistoryProposal { record, identity }, source)?;
        if let Some(payload_root) = payload_root {
            write_history_payload_root(&target_root, admission.commit.identity(), payload_root)?;
            #[cfg(test)]
            super::trip_history_test_fault(super::HistoryTestFault::AfterPayloadRoot)?;
        }
        self.compare_and_swap_selected_history_ref(
            target,
            previous.as_ref().map(SelectedHistoryRef::commit),
            admission.commit.identity(),
            source,
        )?;
        Ok(admission)
    }

    fn compare_and_swap_selected_history_ref<S: SelectedGenerationSource>(
        &self,
        target: &SemanticTargetKey,
        expected: Option<HistoryCommitId>,
        next: HistoryCommitId,
        source: &mut S,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        let target_root = self.target_root(target);
        let commits_root = prepare_history_layout(&target_root)?;
        let record = load_history_commit(&commits_root, next)?;
        let generation = load_record(&target_root, record.generation, target)?;
        validate_commit_generation(&record, &generation)?;
        require_current(source, record.stamp, generation.image)?;
        self.compare_and_swap_history_ref(
            target,
            HistoryRefKind::Branch,
            HistoryRefName::new("local-cache")?,
            expected,
            Some(next),
        )
    }
}

pub(super) fn generation_from_record(
    record: GenerationRecord,
    stamp: SelectedGenerationStamp,
) -> Result<LocalSemanticGeneration, String> {
    validate_record_selection(&record, stamp)?;
    Ok(generation_from_validated_record(record, stamp))
}

pub(super) fn generation_from_validated_record(
    record: GenerationRecord,
    stamp: SelectedGenerationStamp,
) -> LocalSemanticGeneration {
    LocalSemanticGeneration {
        identity: record.identity,
        previous_identity: None,
        previous_image: None,
        previous_image_identity: None,
        target: record.target,
        selected_stamp: stamp,
        catalog: record.catalog,
        image: record.image,
        image_identity: record.image_identity,
        manifest: record.manifest,
    }
}

pub(crate) fn validate_commit_generation(
    commit: &HistoryCommitRecord,
    generation: &GenerationRecord,
) -> Result<(), String> {
    validate_record_selection(generation, commit.stamp)?;
    let root_matches_generation = match commit.generation_root {
        HistoryGenerationRoot::NxfiV1(root) => {
            HistoryGenerationRoot::NxfiV1(generation.image.semantic_generation())
                == HistoryGenerationRoot::NxfiV1(root)
        }
        HistoryGenerationRoot::TypedV2(_) | HistoryGenerationRoot::TypedV3(_) => true,
    };
    if generation.identity != commit.generation
        || !root_matches_generation
        || generation.manifest.root() != commit.manifest_root
    {
        return Err("semantic history commit differs from its generation snapshot".to_owned());
    }
    Ok(())
}

pub(super) fn validate_commit_ancestry(
    record: &HistoryCommitRecord,
    parents: &[HistoryCommitRecord],
) -> Result<(), String> {
    if record.parents.is_empty() {
        if !parents.is_empty() || record.first_parent_depth != 0 || !record.checkpoint {
            return Err("semantic history root has invalid checkpoint metadata".to_owned());
        }
        return Ok(());
    }
    let parent = parents
        .iter()
        .find(|parent| parent.identity == record.parents[0])
        .ok_or_else(|| "semantic history commit has a missing first parent".to_owned())?;
    if record.target != parent.target
        || record.first_parent_depth != parent.first_parent_depth.saturating_add(1)
        || record.checkpoint != (record.first_parent_depth % HISTORY_CHECKPOINT_INTERVAL == 0)
    {
        return Err("semantic history first-parent chain is inconsistent".to_owned());
    }
    for (identity, parent) in record.parents.iter().skip(1).zip(parents.iter().skip(1)) {
        if parent.identity != *identity {
            return Err("semantic history commit has a missing merge parent".to_owned());
        }
        if parent.target != record.target {
            return Err("semantic history merge crosses target scope".to_owned());
        }
    }
    Ok(())
}

pub(super) fn validate_parent_set(
    commits_root: &Path,
    record: &HistoryCommitRecord,
) -> Result<(), String> {
    if record.parents.len() > MAX_HISTORY_PARENTS
        || record.parents.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err("semantic history commit has an invalid parent set".to_owned());
    }
    let mut parents = Vec::with_capacity(record.parents.len());
    for identity in &record.parents {
        parents.push(load_history_commit(commits_root, *identity)?);
    }
    validate_parent_set_with_records(record, &parents)
}

pub(super) fn validate_parent_set_with_records(
    record: &HistoryCommitRecord,
    parents: &[HistoryCommitRecord],
) -> Result<(), String> {
    if record.parents.len() > MAX_HISTORY_PARENTS
        || record.parents.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err("semantic history commit has an invalid parent set".to_owned());
    }
    if parents.len() != record.parents.len() {
        return Err(if record.parents.is_empty() {
            "semantic history root has invalid checkpoint metadata".to_owned()
        } else if record.parents.len() == 1 {
            "semantic history commit has a missing first parent".to_owned()
        } else {
            "semantic history commit has a missing merge parent".to_owned()
        });
    }
    for (identity, parent) in record.parents.iter().zip(parents) {
        if parent.identity != *identity {
            return Err(if record.parents.first() == Some(identity) {
                "semantic history commit has a missing first parent".to_owned()
            } else {
                "semantic history commit has a missing merge parent".to_owned()
            });
        }
        if parent.target != record.target {
            return Err("semantic history parent belongs to another target".to_owned());
        }
    }
    validate_commit_ancestry(record, parents)
}
