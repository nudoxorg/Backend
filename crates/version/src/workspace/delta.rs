/// Workspace transition across checked relation deltas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDelta {
    base: WorkspaceRoot,
    target: WorkspaceRoot,
    relations: Vec<RelationTransition>,
    base_manifest: WorkspaceManifest,
    target_manifest: WorkspaceManifest,
}

/// A structurally decoded workspace transition with no manifest or relation
/// closure proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedWorkspaceDelta {
    base: [u8; ID_BYTES],
    target: [u8; ID_BYTES],
    relations: Vec<RelationTransition>,
}

/// Zero-copy structural view of a potentially large persisted transition.
///
/// Durable owners use this header to locate the authenticated base relation
/// before rebuilding the exact typed delta. Canonical change bytes stay
/// borrowed in the original store buffer and are compared only once during
/// admission, avoiding a second large allocation on restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedWorkspaceDeltaHeader {
    base: [u8; ID_BYTES],
    target: [u8; ID_BYTES],
    relations: Vec<RelationTransitionHeader>,
}

/// Fixed-size identity fields of one persisted relation transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationTransitionHeader {
    schema: SchemaIdentity,
    base: [u8; ID_BYTES],
    target: [u8; ID_BYTES],
    delta: [u8; ID_BYTES],
}

impl RelationTransitionHeader {
    /// Returns the untrusted relation schema claim.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the untrusted base-root claim.
    #[must_use]
    pub const fn base(self) -> [u8; ID_BYTES] {
        self.base
    }

    /// Returns the untrusted target-root claim.
    #[must_use]
    pub const fn target(self) -> [u8; ID_BYTES] {
        self.target
    }

    /// Returns the untrusted delta-identity claim.
    #[must_use]
    pub const fn delta(self) -> [u8; ID_BYTES] {
        self.delta
    }
}

impl UntrustedWorkspaceDeltaHeader {
    /// Returns the untrusted workspace base-root claim.
    #[must_use]
    pub const fn base(&self) -> [u8; ID_BYTES] {
        self.base
    }

    /// Returns the untrusted workspace target-root claim.
    #[must_use]
    pub const fn target(&self) -> [u8; ID_BYTES] {
        self.target
    }

    /// Returns the fixed relation headers without materializing changes.
    #[must_use]
    pub fn relations(&self) -> &[RelationTransitionHeader] {
        &self.relations
    }

    /// Admits the original canonical bytes against freshly checked relation
    /// transitions and manifests.
    ///
    /// # Errors
    ///
    /// Returns a structural or semantic error when any header field or any
    /// canonical change byte differs from the rebuilt checked transition.
    pub fn admit_encoded(
        self,
        encoded: &[u8],
        base: &WorkspaceManifest,
        target: &WorkspaceManifest,
        relations: Vec<RelationTransition>,
    ) -> Result<WorkspaceDelta, WorkspaceDecodeError> {
        if self.base != base.root().to_bytes()
            || self.target != target.root().to_bytes()
            || self.relations.len() != relations.len()
            || self.relations.iter().zip(&relations).any(|(claim, checked)| {
                claim.schema != checked.schema()
                    || claim.base != checked.base()
                    || claim.target != checked.target()
                    || claim.delta != checked.delta()
            })
        {
            return Err(WorkspaceDecodeError::Semantic(
                WorkspaceError::TransitionMismatch,
            ));
        }
        let admitted = WorkspaceDelta::new(base, target, relations)
            .map_err(WorkspaceDecodeError::Semantic)?;
        if admitted.encode() != encoded {
            return Err(WorkspaceDecodeError::Semantic(
                WorkspaceError::TransitionMismatch,
            ));
        }
        Ok(admitted)
    }
}

impl UntrustedWorkspaceDelta {
    /// Returns the untrusted base workspace root claim.
    #[must_use]
    pub const fn base(&self) -> [u8; ID_BYTES] {
        self.base
    }

    /// Returns the untrusted target workspace root claim.
    #[must_use]
    pub const fn target(&self) -> [u8; ID_BYTES] {
        self.target
    }

    /// Returns structurally decoded, still unadmitted relation descriptors.
    #[must_use]
    pub fn relations(&self) -> &[RelationTransition] {
        &self.relations
    }

    /// Admits this descriptor against checked manifests and exact typed
    /// relation transitions.
    ///
    /// Each supplied transition must have been produced by
    /// [`RelationTransition::admit_delta`] (or directly by a checked
    /// [`Delta`]).  The wire roots and all transition fields are compared
    /// before the checked workspace transition is returned.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when either manifest or any transition is
    /// unverified, mismatched, unordered, or incomplete.
    pub fn admit(
        self,
        base: &WorkspaceManifest,
        target: &WorkspaceManifest,
        relations: Vec<RelationTransition>,
    ) -> Result<WorkspaceDelta, WorkspaceError> {
        if self.base != base.root().to_bytes() || self.target != target.root().to_bytes() {
            return Err(WorkspaceError::TransitionMismatch);
        }
        if !same_transition_list(&self.relations, &relations) {
            return Err(WorkspaceError::TransitionMismatch);
        }
        WorkspaceDelta::new(base, target, relations)
    }

    /// Admits this descriptor and immediately wraps it in an opaque owner
    /// capability.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::admit`].
    pub fn admit_checked(
        self,
        base: &WorkspaceManifest,
        target: &WorkspaceManifest,
        relations: Vec<RelationTransition>,
    ) -> Result<CheckedWorkspaceTransition, WorkspaceError> {
        self.admit(base, target, relations)
            .map(WorkspaceDelta::into_checked)
    }
}

impl WorkspaceDelta {
    /// Constructs a workspace delta only when every changed relation binds the
    /// exact base and target manifests.
    ///
    /// The relation transition lane is deliberately complete: basis roots,
    /// authority, and coverage must remain unchanged unless a future API adds
    /// explicit transition descriptors for those lanes.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when manifests, relation sets, or transition
    /// descriptors are incomplete or inconsistent.
    pub fn new(
        base: &WorkspaceManifest,
        target: &WorkspaceManifest,
        relations: Vec<RelationTransition>,
    ) -> Result<Self, WorkspaceError> {
        base.validate()?;
        target.validate()?;
        if base.schema != target.schema {
            return Err(WorkspaceError::SchemaMismatch);
        }
        if base.relations.len() != target.relations.len()
            || base
                .relations
                .iter()
                .zip(&target.relations)
                .any(|(old, new)| old.schema != new.schema)
        {
            return Err(WorkspaceError::RelationSetMismatch);
        }
        if base.basis != target.basis {
            return Err(WorkspaceError::BasisMismatch);
        }
        if base.authority != target.authority {
            return Err(WorkspaceError::AuthorityMismatch);
        }
        if base.coverage != target.coverage {
            return Err(WorkspaceError::CoverageMismatch);
        }
        if relations
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceError::DuplicateRelation);
        }
        for transition in &relations {
            if !transition.checked {
                return Err(WorkspaceError::UnverifiedTransition);
            }
            if !transition.is_canonical() {
                return Err(WorkspaceError::InvalidTransition);
            }
            let old = base
                .relations
                .iter()
                .find(|binding| binding.schema == transition.schema)
                .ok_or(WorkspaceError::TransitionMismatch)?;
            let new = target
                .relations
                .iter()
                .find(|binding| binding.schema == transition.schema)
                .ok_or(WorkspaceError::TransitionMismatch)?;
            if old.root != transition.base || new.root != transition.target {
                return Err(WorkspaceError::TransitionMismatch);
            }
            if old.root == new.root {
                return Err(WorkspaceError::UnexpectedTransition);
            }
        }
        for old in &base.relations {
            let new = target
                .relations
                .iter()
                .find(|binding| binding.schema == old.schema);
            if new.map(|binding| binding.root) != Some(old.root)
                && !relations
                    .iter()
                    .any(|transition| transition.schema == old.schema)
            {
                return Err(WorkspaceError::MissingTransition);
            }
        }
        Ok(Self {
            base: base.root(),
            target: target.root(),
            relations,
            base_manifest: base.clone(),
            target_manifest: target.clone(),
        })
    }

    /// Encodes the checked workspace transition in canonical wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&DELTA_WIRE_MAGIC);
        out.push(WORKSPACE_WIRE_VERSION);
        push_digest(&mut out, self.base.to_bytes());
        push_digest(&mut out, self.target.to_bytes());
        push_count(&mut out, self.relations.len());
        for relation in &self.relations {
            append_wire_field(&mut out, &relation.encode());
        }
        out
    }

    /// Decodes a workspace transition without admitting its manifests or
    /// relation deltas.
    ///
    /// # Errors
    ///
    /// Returns a bounded structural or canonical-order error for malformed
    /// envelopes.  Decoded relation descriptors remain untrusted.
    pub fn decode_untrusted(bytes: &[u8]) -> Result<UntrustedWorkspaceDelta, WorkspaceDecodeError> {
        let mut reader = WireReader::new(bytes)?;
        reader.magic(DELTA_WIRE_MAGIC)?;
        let base = reader.digest()?;
        let target = reader.digest()?;
        let relation_count = reader.count()?;
        let mut relations = Vec::with_capacity(relation_count);
        for _ in 0..relation_count {
            let relation_bytes = reader.field()?;
            relations.push(RelationTransition::decode_untrusted(relation_bytes)?);
        }
        if relations
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceDecodeError::Semantic(
                WorkspaceError::DuplicateRelation,
            ));
        }
        reader.finish()?;
        Ok(UntrustedWorkspaceDelta {
            base,
            target,
            relations,
        })
    }

    /// Decodes only fixed transition identities from a caller-bounded
    /// durable envelope, retaining no copy of canonical change bytes.
    ///
    /// # Errors
    ///
    /// Returns a structural error for malformed, oversized, unordered, or
    /// trailing data. The resulting claims remain untrusted until
    /// [`UntrustedWorkspaceDeltaHeader::admit_encoded`] succeeds.
    pub fn decode_persisted_header(
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<UntrustedWorkspaceDeltaHeader, WorkspaceDecodeError> {
        let mut reader = WireReader::new_with_limit(bytes, max_bytes)?;
        reader.magic(DELTA_WIRE_MAGIC)?;
        let base = reader.digest()?;
        let target = reader.digest()?;
        let relation_count = reader.count()?;
        let mut relations = Vec::with_capacity(relation_count);
        for _ in 0..relation_count {
            let relation_bytes = reader.field()?;
            let mut relation = WireReader::new_with_limit(relation_bytes, max_bytes)?;
            relation.magic(TRANSITION_WIRE_MAGIC)?;
            let schema = read_schema(&mut relation)?;
            let base = relation.digest()?;
            let target = relation.digest()?;
            let delta = relation.digest()?;
            let _canonical_changes = relation.field()?;
            relation.finish()?;
            relations.push(RelationTransitionHeader {
                schema,
                base,
                target,
                delta,
            });
        }
        if relations
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceDecodeError::Semantic(
                WorkspaceError::DuplicateRelation,
            ));
        }
        reader.finish()?;
        Ok(UntrustedWorkspaceDeltaHeader {
            base,
            target,
            relations,
        })
    }

    /// Wraps a validated workspace delta in the opaque capability consumed by
    /// an owner when it publishes a state transition.
    #[must_use]
    pub fn into_checked(self) -> CheckedWorkspaceTransition {
        CheckedWorkspaceTransition { delta: self }
    }

    /// Borrows this validated transition as an opaque owner capability.
    #[must_use]
    pub fn checked(&self) -> CheckedWorkspaceTransition {
        self.clone().into_checked()
    }

    /// Returns typed durable-object references for the target workspace
    /// closure.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if the transition's target manifest cannot
    /// prove a complete typed closure.
    pub fn closure_refs(&self) -> Result<Vec<ClosureRef>, WorkspaceError> {
        self.target_manifest.closure_refs()
    }

    /// Returns typed durable-object references for the base workspace
    /// closure.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if the base manifest cannot prove a
    /// complete typed closure.
    pub fn base_closure_refs(&self) -> Result<Vec<ClosureRef>, WorkspaceError> {
        self.base_manifest.closure_refs()
    }

    /// Returns typed durable-object references for the target workspace
    /// closure.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if the target manifest cannot prove a
    /// complete typed closure.
    pub fn target_closure_refs(&self) -> Result<Vec<ClosureRef>, WorkspaceError> {
        self.target_manifest.closure_refs()
    }

    /// Returns the base workspace root.
    #[must_use]
    pub const fn base(&self) -> WorkspaceRoot {
        self.base
    }

    /// Returns the target workspace root.
    #[must_use]
    pub const fn target(&self) -> WorkspaceRoot {
        self.target
    }

    /// Returns checked relation transition descriptors in schema order.
    #[must_use]
    pub fn relations(&self) -> &[RelationTransition] {
        &self.relations
    }

    /// Returns the checked base manifest retained by this transition.
    #[must_use]
    pub const fn base_manifest(&self) -> &WorkspaceManifest {
        &self.base_manifest
    }

    /// Returns the checked target manifest retained by this transition.
    #[must_use]
    pub const fn target_manifest(&self) -> &WorkspaceManifest {
        &self.target_manifest
    }
}

/// Opaque capability proving one exact checked workspace transition.
///
/// This type deliberately has no root or digest accessors.  An owner can use
/// it to enumerate the durable closure, encode the transition, or publish a
/// checked commit, while a model cannot return a caller-selected raw target
/// root as if it were the result of the transition.
#[derive(Clone, Eq, PartialEq)]
pub struct CheckedWorkspaceTransition {
    delta: WorkspaceDelta,
}

impl CheckedWorkspaceTransition {
    /// Returns whether this capability was built for the supplied exact
    /// checked target manifest.
    ///
    /// This comparison is proof preserving and allocation free; callers do
    /// not need to encode the capability and decode it merely to inspect its
    /// target binding.
    #[must_use]
    pub fn binds_target(&self, manifest: &WorkspaceManifest) -> bool {
        self.delta.target_manifest == *manifest
    }

    /// Returns whether this capability was built from the supplied exact
    /// checked base manifest.
    #[must_use]
    pub fn binds_base(&self, manifest: &WorkspaceManifest) -> bool {
        self.delta.base_manifest == *manifest
    }

    /// Returns the durable closure selected by the transition's target state.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if the target manifest is not fully checked
    /// or has incomplete coverage.
    pub fn closure_refs(&self) -> Result<Vec<ClosureRef>, WorkspaceError> {
        self.delta.closure_refs()
    }

    /// Returns the base closure selected by this checked capability.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if the base manifest cannot prove a
    /// complete typed closure.
    pub fn base_closure_refs(&self) -> Result<Vec<ClosureRef>, WorkspaceError> {
        self.delta.base_closure_refs()
    }

    /// Encodes the checked transition for a durable publication record.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.delta.encode()
    }

    /// Returns the number of relation lanes in this transition.
    #[must_use]
    pub fn relation_count(&self) -> usize {
        self.delta.relations.len()
    }

    /// Binds this transition's target state to typed commit provenance and
    /// returns an opaque commit capability.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] if the provenance or parent set is not valid
    /// for the target manifest.
    pub fn commit(
        self,
        parents: Vec<CommitId>,
        provenance: CommitProvenance,
    ) -> Result<CheckedCommit, CommitError> {
        commit_checked(&self.delta.target_manifest, parents, provenance)
            .map(CheckedCommit::from_commit)
    }
}

/// Constructs a checked workspace transition.
///
/// # Errors
///
/// Returns the validation errors from [`WorkspaceDelta::new`].
pub fn workspace_delta(
    base: &WorkspaceManifest,
    target: &WorkspaceManifest,
    relations: Vec<RelationTransition>,
) -> Result<WorkspaceDelta, WorkspaceError> {
    WorkspaceDelta::new(base, target, relations)
}
