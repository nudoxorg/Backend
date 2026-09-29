// Ref publication and admitted-generation bindings share one owner.
use super::codec::*;
use super::provenance::*;
use super::*;

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

pub(super) fn read_history_catalog_snapshot(
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
    let generation = load_record(target_root, record.generation, target)?;
    validate_commit_generation(&record, &generation)?;
    validate_parent_set(commits_root, &record)?;
    for parent in &record.parents {
        let parent_record = load_history_commit(commits_root, *parent)?;
        let parent_generation = load_record(target_root, parent_record.generation, target)?;
        validate_commit_generation(&parent_record, &parent_generation)?;
    }
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
pub(super) fn may_prune_generation_records(
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
    pub(super) fn require_local_cache_head_alignment(
        &self,
        target: &SemanticTargetKey,
        cache_head: Option<super::LocalHead>,
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
    ) -> Result<Vec<ClosureId>, String> {
        let target_root = self.target_root(target);
        let history_root = target_root.join("history");
        if !ensure_optional_directory(&history_root)? {
            return Ok(Vec::new());
        }
        let (catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let mut roots = Vec::with_capacity(catalog.refs.len());
        for reference in &catalog.refs {
            if let Some(payload) = read_history_payload_root(&target_root, reference.commit)? {
                roots.push(payload.closure);
            }
        }
        roots.sort_unstable();
        roots.dedup();
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
    ) -> Result<Option<(ObjectId, u64)>, String> {
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
        super::gc::advance_history_gc(&self.target_root(target), target)
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
        let commits_root = prepare_history_layout(&target_root)?;
        let generation = load_record(
            &target_root,
            proposal.record.generation,
            &proposal.record.target,
        )?;
        validate_commit_generation(&proposal.record, &generation)?;
        require_current(source, proposal.record.stamp, generation.image)?;
        validate_parent_set(&commits_root, &proposal.record)?;
        let bytes = encode_history_commit(&proposal.record, proposal.identity)?;
        let path = history_commit_path(&commits_root, proposal.identity);
        let created = match fs::read(&path) {
            Ok(existing) if existing == bytes => false,
            Ok(_) => return Err("immutable semantic history identity collision".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
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
        let target_root = self.target_root(target);
        let commits_root = prepare_history_layout(&target_root)?;
        let (mut catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let actual = catalog.get(kind, &name);
        if actual != expected {
            return Err("semantic history reference compare-and-swap failed".to_owned());
        }
        if let Some(identity) = next {
            let _ = validate_history_commit_node(&target_root, target, &commits_root, identity)?;
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
        payload_root: Option<HistoryPayloadRoot>,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let target = &generation.target;
        let target_root = self.target_root(target);
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
    Ok(LocalSemanticGeneration {
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
    })
}

pub(super) fn validate_commit_generation(
    commit: &HistoryCommitRecord,
    generation: &GenerationRecord,
) -> Result<(), String> {
    validate_record_selection(generation, commit.stamp)?;
    if generation.identity != commit.generation
        || HistoryGenerationRoot::NxfiV1(generation.image.semantic_generation())
            != commit.generation_root
        || generation.manifest.root() != commit.manifest_root
    {
        return Err("semantic history commit differs from its generation snapshot".to_owned());
    }
    Ok(())
}

pub(super) fn validate_commit_ancestry(
    record: &HistoryCommitRecord,
    records: &HashMap<HistoryCommitId, HistoryCommitRecord>,
) -> Result<(), String> {
    if record.parents.is_empty() {
        if record.first_parent_depth != 0 || !record.checkpoint {
            return Err("semantic history root has invalid checkpoint metadata".to_owned());
        }
        return Ok(());
    }
    let parent = records
        .get(&record.parents[0])
        .ok_or_else(|| "semantic history commit has a missing first parent".to_owned())?;
    if record.target != parent.target
        || record.first_parent_depth != parent.first_parent_depth.saturating_add(1)
        || record.checkpoint != (record.first_parent_depth % HISTORY_CHECKPOINT_INTERVAL == 0)
    {
        return Err("semantic history first-parent chain is inconsistent".to_owned());
    }
    for identity in record.parents.iter().skip(1) {
        let parent = records
            .get(identity)
            .ok_or_else(|| "semantic history commit has a missing merge parent".to_owned())?;
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
    if record.parents.is_empty() {
        return validate_commit_ancestry(record, &HashMap::new());
    }
    let mut parents = HashMap::new();
    for identity in &record.parents {
        let parent = load_history_commit(commits_root, *identity)?;
        if parent.target != record.target {
            return Err("semantic history parent belongs to another target".to_owned());
        }
        parents.insert(*identity, parent);
    }
    validate_commit_ancestry(record, &parents)
}
