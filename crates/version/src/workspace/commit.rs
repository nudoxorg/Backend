fn encode_commit(
    id: [u8; ID_BYTES],
    root: [u8; ID_BYTES],
    parents: &[[u8; ID_BYTES]],
    provenance: &UntrustedCommitProvenance,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&COMMIT_WIRE_MAGIC);
    out.push(WORKSPACE_WIRE_VERSION);
    push_digest(&mut out, id);
    push_digest(&mut out, root);
    push_count(&mut out, parents.len());
    for parent in parents {
        push_digest(&mut out, *parent);
    }
    append_wire_field(&mut out, &provenance.encode());
    out
}

/// History identity bound to an exact workspace state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    id: CommitId,
    root: WorkspaceRoot,
    parents: Vec<CommitId>,
    authority: ObjectClosure,
    transaction: ObjectClosure,
    provenance: Vec<u8>,
}

/// A structurally decoded commit awaiting state and provenance admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedCommit {
    id: [u8; ID_BYTES],
    root: [u8; ID_BYTES],
    parents: Vec<[u8; ID_BYTES]>,
    provenance: UntrustedCommitProvenance,
}

impl UntrustedCommit {
    /// Returns the untrusted commit identity claim.
    #[must_use]
    pub const fn id(&self) -> [u8; ID_BYTES] {
        self.id
    }

    /// Returns the untrusted workspace-root claim.
    #[must_use]
    pub const fn root(&self) -> [u8; ID_BYTES] {
        self.root
    }

    /// Returns the strictly ordered parent claims.
    #[must_use]
    pub fn parents(&self) -> &[[u8; ID_BYTES]] {
        &self.parents
    }

    /// Returns untrusted provenance claims.
    #[must_use]
    pub const fn provenance(&self) -> &UntrustedCommitProvenance {
        &self.provenance
    }

    /// Re-encodes this decoded commit in canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_commit(self.id, self.root, &self.parents, &self.provenance)
    }

    /// Admits the commit against a checked manifest and exact typed
    /// provenance.  The claimed root, parents, provenance bytes, and commit
    /// ID are all recomputed and compared.  A merge commit with parent claims
    /// must use [`Self::admit_with_parents`] so every parent is supplied as a
    /// checked identity.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] when the manifest, provenance, parent set, or
    /// claimed ID does not match recomputation.
    pub fn admit(
        self,
        manifest: &WorkspaceManifest,
        provenance: CommitProvenance,
    ) -> Result<Commit, CommitError> {
        if !self.parents.is_empty() {
            return Err(CommitError::UnverifiedParent);
        }
        self.admit_with_parents(manifest, Vec::new(), provenance)
    }

    /// Admits this commit with typed parent identities supplied by the
    /// caller.  Raw parent claims from the wire are compared to these exact
    /// identities before commit-ID recomputation.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] when the state, parent set, provenance, or
    /// claimed ID does not match recomputation.
    pub fn admit_with_parents(
        self,
        manifest: &WorkspaceManifest,
        parents: Vec<CommitId>,
        provenance: CommitProvenance,
    ) -> Result<Commit, CommitError> {
        if self.root != manifest.root().to_bytes() {
            return Err(CommitError::StateMismatch);
        }
        if self.provenance.encode() != provenance.encode() {
            return Err(CommitError::ProvenanceMismatch);
        }
        let canonical_parents = canonical_parent_list(parents.clone())?;
        if canonical_parents.len() != self.parents.len()
            || canonical_parents
                .iter()
                .zip(&self.parents)
                .any(|(parent, claim)| parent.to_bytes() != *claim)
        {
            return Err(CommitError::UnverifiedParent);
        }
        let commit = commit_checked(manifest, parents, provenance)?;
        if commit.id.to_bytes() != self.id {
            return Err(CommitError::IdMismatch);
        }
        Ok(commit)
    }

    /// Admits this commit by supplying the typed authority and transaction
    /// closures that prove the wire provenance.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] when the closure claims, manifest, parents, or
    /// commit ID do not match recomputation.
    pub fn admit_with_closures(
        self,
        manifest: &WorkspaceManifest,
        authority: ObjectClosure,
        transaction: ObjectClosure,
    ) -> Result<Commit, CommitError> {
        let provenance = self
            .provenance
            .clone()
            .admit(authority, transaction)
            .map_err(|_| CommitError::ProvenanceMismatch)?;
        self.admit(manifest, provenance)
    }

    /// Admits this commit with typed parent and closure identities.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] when any parent, closure, state, or ID claim
    /// does not match recomputation.
    pub fn admit_with_parents_and_closures(
        self,
        manifest: &WorkspaceManifest,
        parents: Vec<CommitId>,
        authority: ObjectClosure,
        transaction: ObjectClosure,
    ) -> Result<Commit, CommitError> {
        let provenance = self
            .provenance
            .clone()
            .admit(authority, transaction)
            .map_err(|_| CommitError::ProvenanceMismatch)?;
        self.admit_with_parents(manifest, parents, provenance)
    }

    /// Admits this commit and returns an opaque owner capability.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::admit`].
    pub fn admit_capability(
        self,
        manifest: &WorkspaceManifest,
        provenance: CommitProvenance,
    ) -> Result<CheckedCommit, CommitError> {
        self.admit(manifest, provenance)
            .map(CheckedCommit::from_commit)
    }

    /// Admits this commit with typed parents and returns an opaque capability.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::admit_with_parents`].
    pub fn admit_capability_with_parents(
        self,
        manifest: &WorkspaceManifest,
        parents: Vec<CommitId>,
        provenance: CommitProvenance,
    ) -> Result<CheckedCommit, CommitError> {
        self.admit_with_parents(manifest, parents, provenance)
            .map(CheckedCommit::from_commit)
    }
}

/// Opaque capability proving one canonical commit identity.
///
/// This capability exposes only publication-safe serialization and closure
/// enumeration.  It has no root or commit-ID accessor, so a model cannot
/// substitute a raw digest for the checked owner result.
#[derive(Clone, Eq, PartialEq)]
pub struct CheckedCommit {
    commit: Commit,
}

impl CheckedCommit {
    fn from_commit(commit: Commit) -> Self {
        Self { commit }
    }

    /// Returns whether this capability is bound to the supplied exact
    /// checked workspace manifest.
    ///
    /// The manifest is validated before its root is compared, so an
    /// untrusted wire summary cannot satisfy this proof seam by reproducing
    /// a copied root.  No encode/decode round trip or wire parsing is needed.
    #[must_use]
    pub fn binds_target(&self, manifest: &WorkspaceManifest) -> bool {
        manifest.validate().is_ok() && self.commit.root == manifest.root()
    }

    /// Returns this checked history node's canonical identity.
    #[must_use]
    pub const fn id(&self) -> CommitId {
        self.commit.id
    }

    /// Encodes the checked commit for durable publication.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.commit.encode()
    }

    /// Returns the typed authority and transaction objects required by this
    /// commit's durable closure.
    #[must_use]
    pub fn closure_refs(&self) -> Vec<ClosureRef> {
        CommitProvenance {
            authority: self.commit.authority,
            transaction: self.commit.transaction,
            detail: self.commit.provenance.clone(),
        }
        .closure_refs()
    }

    /// Returns the number of parent links in the checked history node.
    #[must_use]
    pub fn parent_count(&self) -> usize {
        self.commit.parents.len()
    }
}

impl Commit {
    /// Returns this history node's canonical identity.
    #[must_use]
    pub const fn id(&self) -> CommitId {
        self.id
    }

    /// Returns the exact visible workspace state selected by this commit.
    #[must_use]
    pub const fn root(&self) -> WorkspaceRoot {
        self.root
    }

    /// Returns the parent identities in the caller-supplied canonical order.
    #[must_use]
    pub fn parents(&self) -> &[CommitId] {
        &self.parents
    }

    /// Returns the authority provenance bound to this history node.
    #[must_use]
    pub const fn authority(&self) -> ObjectClosure {
        self.authority
    }

    /// Returns the transaction provenance bound to this history node.
    #[must_use]
    pub const fn transaction(&self) -> ObjectClosure {
        self.transaction
    }

    /// Returns opaque producer provenance bytes.
    #[must_use]
    pub fn provenance(&self) -> &[u8] {
        &self.provenance
    }

    /// Encodes this checked commit in canonical bounded wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let provenance = CommitProvenance {
            authority: self.authority,
            transaction: self.transaction,
            detail: self.provenance.clone(),
        };
        let provenance = UntrustedCommitProvenance {
            authority: UntrustedClosureRef {
                schema: provenance.authority.schema,
                version: provenance.authority.root,
            },
            transaction: UntrustedClosureRef {
                schema: provenance.transaction.schema,
                version: provenance.transaction.root,
            },
            detail: provenance.detail,
        };
        let parents: Vec<_> = self
            .parents
            .iter()
            .map(|parent| parent.to_bytes())
            .collect();
        encode_commit(
            self.id.to_bytes(),
            self.root.to_bytes(),
            &parents,
            &provenance,
        )
    }

    /// Decodes a commit without trusting its state, parents, or provenance.
    ///
    /// # Errors
    ///
    /// Returns a bounded structural error for malformed envelopes, duplicate
    /// or unordered parents, unsupported wire versions, or trailing bytes.
    pub fn decode_untrusted(bytes: &[u8]) -> Result<UntrustedCommit, WorkspaceDecodeError> {
        let mut reader = WireReader::new(bytes)?;
        reader.magic(COMMIT_WIRE_MAGIC)?;
        let id = reader.digest()?;
        let root = reader.digest()?;
        let parent_count = reader.count()?;
        let mut parents = Vec::with_capacity(parent_count);
        for _ in 0..parent_count {
            parents.push(reader.digest()?);
        }
        if parents.windows(2).any(|window| window[0] >= window[1]) {
            if parents.windows(2).any(|window| window[0] == window[1]) {
                return Err(WorkspaceDecodeError::DuplicateParent);
            }
            return Err(WorkspaceDecodeError::UnorderedParents);
        }
        let provenance = CommitProvenance::decode_untrusted(reader.field()?)?;
        reader.finish()?;
        Ok(UntrustedCommit {
            id,
            root,
            parents,
            provenance,
        })
    }

    /// Converts an already checked commit into an opaque owner capability.
    #[must_use]
    pub fn into_checked(self) -> CheckedCommit {
        CheckedCommit::from_commit(self)
    }
}

/// Failure while constructing a bound commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitError {
    /// The workspace manifest was an untrusted/raw descriptor.
    UnverifiedManifest,
    /// The authority provenance did not match the manifest authority object.
    AuthorityMismatch,
    /// Two parent identities were identical.
    DuplicateParent,
    /// A caller supplied a parent list that was not in canonical order.
    UnorderedParents,
    /// A legacy root/byte-only commit omitted typed provenance.
    MissingProvenance,
    /// The commit's root did not match the admitted manifest.
    StateMismatch,
    /// Decoded provenance did not match supplied typed closure objects.
    ProvenanceMismatch,
    /// Decoded parent claims were not admitted against typed parent commits.
    UnverifiedParent,
    /// The claimed commit ID did not match recomputation.
    IdMismatch,
    /// Provenance detail exceeded the bounded wire budget.
    OversizedProvenance,
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid workspace commit: {self:?}")
    }
}

impl std::error::Error for CommitError {}

fn canonical_parent_list(mut parents: Vec<CommitId>) -> Result<Vec<CommitId>, CommitError> {
    parents.sort();
    if parents.windows(2).any(|window| window[0] == window[1]) {
        return Err(CommitError::DuplicateParent);
    }
    Ok(parents)
}

fn commit_digest(
    root: WorkspaceRoot,
    parents: &[CommitId],
    provenance: &CommitProvenance,
) -> [u8; ID_BYTES] {
    let mut parent_bytes = Vec::with_capacity(parents.len().saturating_mul(ID_BYTES));
    for parent in parents {
        parent_bytes.extend_from_slice(parent.as_bytes());
    }
    let mut authority = Vec::new();
    append_closure(&mut authority, provenance.authority);
    let mut transaction = Vec::new();
    append_closure(&mut transaction, provenance.transaction);
    digest(
        0x43,
        0,
        2,
        CANONICAL_VERSION,
        &[
            root.as_bytes(),
            &parent_bytes,
            &authority,
            &transaction,
            &provenance.detail,
        ],
    )
}

fn append_closure(out: &mut Vec<u8>, closure: ObjectClosure) {
    append_field(out, |inner| {
        inner.push(closure.schema.domain());
        inner.extend_from_slice(&closure.schema.ty().to_be_bytes());
        inner.push(closure.schema.version());
        inner.extend_from_slice(&closure.root);
    });
}

/// Creates a history identity bound to a checked workspace manifest, typed
/// authority/transaction provenance, and a canonical parent set.
///
/// Parent IDs are sorted before hashing so equivalent merge inputs converge;
/// duplicate parents are rejected.  The manifest's authority closure must be
/// exactly the one carried by `provenance`.
///
/// # Errors
///
/// Returns [`CommitError`] when the manifest closure is unverified, the
/// authority provenance differs, or a parent is duplicated.
pub fn commit_checked(
    manifest: &WorkspaceManifest,
    parents: Vec<CommitId>,
    provenance: CommitProvenance,
) -> Result<Commit, CommitError> {
    if !manifest.is_checked() {
        return Err(CommitError::UnverifiedManifest);
    }
    if manifest.authority_closure() != Some(provenance.authority) {
        return Err(CommitError::AuthorityMismatch);
    }
    if provenance.detail.len() > MAX_WIRE_DETAIL_BYTES {
        return Err(CommitError::OversizedProvenance);
    }
    let parents = canonical_parent_list(parents)?;
    let root = manifest.root();
    let id = commit_id_from_digest(commit_digest(root, &parents, &provenance));
    Ok(Commit {
        id,
        root,
        parents,
        authority: provenance.authority,
        transaction: provenance.transaction,
        provenance: provenance.detail,
    })
}

/// Creates an opaque owner commit capability from checked inputs.
///
/// # Errors
///
/// Returns [`CommitError`] when the manifest closure, provenance, or parent
/// list is invalid.
pub fn commit_capability(
    manifest: &WorkspaceManifest,
    parents: Vec<CommitId>,
    provenance: CommitProvenance,
) -> Result<CheckedCommit, CommitError> {
    commit_checked(manifest, parents, provenance).map(CheckedCommit::from_commit)
}

/// Legacy byte-only entry point retained as an explicit failing adapter.
///
/// A root and opaque bytes do not establish the authority/transaction closure
/// required for a durable commit.  Call [`commit_checked`] instead.
///
/// # Errors
///
/// Always returns [`CommitError::MissingProvenance`].
pub fn commit(
    _root: WorkspaceRoot,
    _parents: Vec<CommitId>,
    _provenance: &[u8],
) -> Result<Commit, CommitError> {
    Err(CommitError::MissingProvenance)
}
