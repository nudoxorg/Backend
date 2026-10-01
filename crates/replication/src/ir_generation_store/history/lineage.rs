//! Typed lineage evidence attached to one exact typed-IR transition.
//!
//! This is evidence about possible continuity, not a declaration-identity
//! allocator. It never rewrites `DeclarationIdentity`, semantic rows, links,
//! or either generation root. V2 stores the exact bytes in its commit-rooted
//! locator; full verification still requires a history adapter that proves
//! the exact child/first-parent transition and supplies verified readers and
//! ancestry.

use std::cmp::Ordering;

use backend_semantic::ir::{
    DeclarationFamilyId, DeclarationIdentity, SemanticGenerationRootV2, VariantFingerprint,
};
use thiserror::Error;

const MAGIC: &[u8; 8] = b"IRLEDGE1";
const HEADER_BYTES: usize = 108;
const EDGE_BYTES: usize = 198;

/// Maximum canonical wire size of one lineage-edge set.
pub const MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of fixed-width edges admitted in one transition.
pub const MAX_TYPED_LINEAGE_EDGES_V1: usize = 65_536;
/// Maximum alternatives retained for one ambiguous candidate group.
pub const MAX_LINEAGE_CANDIDATES_PER_GROUP_V1: u16 = 4_096;

/// Verified exact V2 root carried by a lineage endpoint.
///
/// This wrapper can only be made from the semantic crate's admitted root.
/// Wire bytes remain untrusted until they match one of these values.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VerifiedLineageRootV2([u8; 32]);

impl VerifiedLineageRootV2 {
    /// Captures a root already computed by typed semantic verification.
    #[must_use]
    pub const fn from_verified_root(root: SemanticGenerationRootV2) -> Self {
        Self(*root.as_bytes())
    }

    /// Returns the fixed-width root bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Why a candidate relationship could not be resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum UnresolvedLineageReasonV1 {
    /// The available facts do not distinguish one candidate from others.
    InsufficientEvidence = 1,
    /// Independent sources disagree about the candidate relationship.
    ConflictingEvidence = 2,
    /// Required source, reader, or ancestor evidence is unavailable.
    EvidenceUnavailable = 3,
    /// The candidate search stopped at its explicit resource bound.
    SearchBudgetExhausted = 4,
}

impl UnresolvedLineageReasonV1 {
    fn from_code(code: u16) -> Option<Self> {
        match code {
            1 => Some(Self::InsufficientEvidence),
            2 => Some(Self::ConflictingEvidence),
            3 => Some(Self::EvidenceUnavailable),
            4 => Some(Self::SearchBudgetExhausted),
            _ => None,
        }
    }
}

/// Content-addressed reference to independently validated confirmation proof.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineageAttestationId([u8; 32]);

impl LineageAttestationId {
    /// Retains an untrusted proof-object identity; a configured authority
    /// verifier must still validate its contents for the exact edge.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact proof-object identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Domain-separated digest of the exact transition and confirmed edge, with
/// the attestation object ID omitted so a signature can be content-addressed
/// without a self-reference cycle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LineageConfirmationStatementV1([u8; 32]);

impl LineageConfirmationStatementV1 {
    /// Returns the exact statement digest that an authority proof must cover.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Stable identity of one explicitly recorded ambiguity group.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineageCandidateGroupIdV1([u8; 32]);

impl LineageCandidateGroupIdV1 {
    /// Creates a group id from its content-addressed claim.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact group claim.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Kind of candidate relationship recorded between typed generations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LineageKindV1 {
    /// A candidate re-keying across the immediate parent-child transition.
    Rename = 1,
    /// A candidate return after the exact identity was absent from the parent.
    Resurrection = 2,
}

impl LineageKindV1 {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Rename),
            2 => Some(Self::Resurrection),
            _ => None,
        }
    }
}

/// Epistemic status of one lineage candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineageStatusV1 {
    /// Only an independent authority attestation can justify this status.
    Confirmed {
        /// Content ID of the independently verified attestation.
        attestation: LineageAttestationId,
    },
    /// One member of a complete, explicitly bounded candidate group.
    Ambiguous {
        /// Identity shared by the complete candidate group.
        group: LineageCandidateGroupIdV1,
        /// Zero-based position of this candidate in the group.
        index: u16,
        /// Total candidates required for the group to be complete.
        count: u16,
    },
    /// A candidate or missing edge whose relationship remains unresolved.
    Unresolved {
        /// Why continuity could not be established.
        reason: UnresolvedLineageReasonV1,
        /// Optional nonzero digest of retained diagnostic evidence.
        evidence: Option<[u8; 32]>,
    },
}

/// One source endpoint. Rename sources must be the direct parent; resurrection
/// sources must be a strict, verified ancestor of that parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageSourceV1 {
    commit: super::HistoryCommitId,
    generation: VerifiedLineageRootV2,
    declaration: DeclarationIdentity,
}

impl LineageSourceV1 {
    /// Creates a source endpoint from an exact history commit, verified root,
    /// and canonical declaration identity.
    #[must_use]
    pub const fn new(
        commit: super::HistoryCommitId,
        generation: VerifiedLineageRootV2,
        declaration: DeclarationIdentity,
    ) -> Self {
        Self {
            commit,
            generation,
            declaration,
        }
    }

    /// Returns the exact history commit.
    #[must_use]
    pub const fn commit(self) -> super::HistoryCommitId {
        self.commit
    }

    /// Returns the exact semantic generation root.
    #[must_use]
    pub const fn generation(self) -> VerifiedLineageRootV2 {
        self.generation
    }

    /// Returns the original canonical identity without rewriting it.
    #[must_use]
    pub const fn declaration(self) -> DeclarationIdentity {
        self.declaration
    }
}

/// One immutable candidate edge. Its target generation is the edge set's
/// exact child root; its source root and commit are carried explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageEdgeV1 {
    kind: LineageKindV1,
    source: LineageSourceV1,
    target: DeclarationIdentity,
    status: LineageStatusV1,
}

impl LineageEdgeV1 {
    /// Creates a typed candidate without asserting that it is confirmed.
    #[must_use]
    pub const fn new(
        kind: LineageKindV1,
        source: LineageSourceV1,
        target: DeclarationIdentity,
        status: LineageStatusV1,
    ) -> Self {
        Self {
            kind,
            source,
            target,
            status,
        }
    }

    /// Returns the relationship kind.
    #[must_use]
    pub const fn kind(self) -> LineageKindV1 {
        self.kind
    }

    /// Returns the exact source endpoint.
    #[must_use]
    pub const fn source(self) -> LineageSourceV1 {
        self.source
    }

    /// Returns the child declaration identity exactly as recorded.
    #[must_use]
    pub const fn target(self) -> DeclarationIdentity {
        self.target
    }

    /// Returns the evidence status without changing either declaration id.
    #[must_use]
    pub const fn status(self) -> LineageStatusV1 {
        self.status
    }
}

/// Owned, bounded canonical wire for one parent-to-child lineage edge set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedTypedLineageEdgeSetV1 {
    bytes: Box<[u8]>,
}

impl OwnedTypedLineageEdgeSetV1 {
    /// Encodes one canonical set. Input order is immaterial; the wire is sorted
    /// by exact endpoint tuple and then status. Duplicate endpoint pairs and
    /// incomplete ambiguity groups are rejected.
    pub fn encode(
        parent: super::HistoryCommitId,
        parent_generation: VerifiedLineageRootV2,
        child_generation: VerifiedLineageRootV2,
        edges: &[LineageEdgeV1],
    ) -> Result<Self, LineageEdgeSetErrorV1> {
        if edges.len() > MAX_TYPED_LINEAGE_EDGES_V1 {
            return Err(LineageEdgeSetErrorV1::TooManyEdges);
        }
        let wire_len = HEADER_BYTES
            .checked_add(
                edges
                    .len()
                    .checked_mul(EDGE_BYTES)
                    .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?,
            )
            .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?;
        if wire_len > MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES {
            return Err(LineageEdgeSetErrorV1::TooManyEdges);
        }
        let mut ordered = Vec::new();
        ordered
            .try_reserve_exact(edges.len())
            .map_err(|_| LineageEdgeSetErrorV1::Allocation)?;
        ordered.extend_from_slice(edges);
        ordered.sort_by(canonical_edge_cmp);
        validate_owned_edges(&ordered)?;

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(wire_len)
            .map_err(|_| LineageEdgeSetErrorV1::Allocation)?;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(parent.as_bytes());
        bytes.extend_from_slice(parent_generation.as_bytes());
        bytes.extend_from_slice(child_generation.as_bytes());
        bytes.extend_from_slice(
            &u32::try_from(ordered.len())
                .map_err(|_| LineageEdgeSetErrorV1::TooManyEdges)?
                .to_be_bytes(),
        );
        for edge in &ordered {
            encode_edge(edge, child_generation, &mut bytes);
        }
        if bytes.len() != wire_len {
            return Err(LineageEdgeSetErrorV1::LengthOverflow);
        }
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
        })
    }

    /// Borrows the canonical bytes without copying them.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Parses the owned wire as a zero-copy borrowed view.
    pub fn borrow(&self) -> Result<BorrowedTypedLineageEdgeSetV1<'_>, LineageEdgeSetErrorV1> {
        BorrowedTypedLineageEdgeSetV1::parse(&self.bytes)
    }
}

/// Parsed view over caller-owned canonical bytes. Fixed-width endpoint fields
/// are read in place; validation allocates only bounded offset scratch, never
/// a copy of the wire or semantic payloads.
#[derive(Clone, Copy, Debug)]
pub struct BorrowedTypedLineageEdgeSetV1<'wire> {
    bytes: &'wire [u8],
    parent_commit: super::HistoryCommitId,
    parent_generation: &'wire [u8; 32],
    child_generation: &'wire [u8; 32],
    count: usize,
}

impl<'wire> BorrowedTypedLineageEdgeSetV1<'wire> {
    /// Parses exact bounded framing without materializing edge records.
    pub fn parse(bytes: &'wire [u8]) -> Result<Self, LineageEdgeSetErrorV1> {
        if bytes.len() > MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES || bytes.len() < HEADER_BYTES {
            return Err(LineageEdgeSetErrorV1::InvalidLength);
        }
        if bytes.get(..8) != Some(MAGIC.as_slice()) {
            return Err(LineageEdgeSetErrorV1::InvalidMagic);
        }
        let parent_commit = super::HistoryCommitId::from_bytes(read_array::<32>(bytes, 8)?);
        let parent_generation = read_array_ref::<32>(bytes, 40)?;
        let child_generation = read_array_ref::<32>(bytes, 72)?;
        let count = usize::try_from(u32::from_be_bytes(read_array::<4>(bytes, 104)?))
            .map_err(|_| LineageEdgeSetErrorV1::LengthOverflow)?;
        if count > MAX_TYPED_LINEAGE_EDGES_V1 {
            return Err(LineageEdgeSetErrorV1::TooManyEdges);
        }
        let expected_len = count
            .checked_mul(EDGE_BYTES)
            .and_then(|edge_bytes| HEADER_BYTES.checked_add(edge_bytes))
            .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?;
        if bytes.len() != expected_len {
            return Err(LineageEdgeSetErrorV1::InvalidLength);
        }
        for index in 0..count {
            let offset = HEADER_BYTES
                .checked_add(
                    index
                        .checked_mul(EDGE_BYTES)
                        .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?,
                )
                .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?;
            decode_edge(bytes, offset)?;
        }
        Ok(Self {
            bytes,
            parent_commit,
            parent_generation,
            child_generation,
            count,
        })
    }

    /// Returns the borrowed canonical bytes.
    #[must_use]
    pub const fn as_bytes(self) -> &'wire [u8] {
        self.bytes
    }

    /// Returns the exact parent commit claim.
    #[must_use]
    pub const fn parent_commit(self) -> super::HistoryCommitId {
        self.parent_commit
    }

    /// Returns the claimed parent generation root as borrowed bytes.
    #[must_use]
    pub const fn parent_generation_claim(self) -> &'wire [u8; 32] {
        self.parent_generation
    }

    /// Returns the child-root claim as borrowed bytes.
    #[must_use]
    pub const fn child_generation_claim(self) -> &'wire [u8; 32] {
        self.child_generation
    }

    /// Computes the digest an authority must sign for one confirmed row.
    /// This accepts only a view borrowed from this exact edge set. The
    /// attestation ID is excluded, so a producer can first encode a
    /// nonzero placeholder, derive the statement, then replace it with the
    /// content-addressed proof ID without creating a cycle.
    pub fn confirmation_statement(
        self,
        edge: LineageEdgeViewV1<'wire>,
    ) -> Result<LineageConfirmationStatementV1, LineageEdgeSetErrorV1> {
        if !matches!(edge.status, LineageStatusViewV1::Confirmed { .. }) {
            return Err(LineageEdgeSetErrorV1::NotConfirmed);
        }
        let expected_offset = edge.offset;
        if expected_offset < HEADER_BYTES
            || (expected_offset - HEADER_BYTES) % EDGE_BYTES != 0
            || expected_offset
                .checked_add(EDGE_BYTES)
                .is_none_or(|end| end > self.bytes.len())
            || edge.canonical_record.as_ptr() != self.bytes[expected_offset..].as_ptr()
        {
            return Err(LineageEdgeSetErrorV1::EdgeSetMismatch);
        }
        Ok(confirmation_statement_digest(
            self.parent_commit,
            self.parent_generation,
            self.child_generation,
            edge,
        ))
    }

    /// Validates canonical ordering, duplicate endpoints, reserved fields,
    /// and complete ambiguity groups without asserting semantic membership.
    pub(crate) fn validate_structure(self) -> Result<(), LineageEdgeSetErrorV1> {
        self.verify_canonical_structure()
    }

    /// Checks roots, endpoint presence, ancestry, canonical ordering, candidate
    /// completeness, and any confirmation attestations. The returned marker
    /// certifies only this metadata under the supplied evidence provider; it
    /// does not authorize identity rewriting or owner selection.
    pub fn verify<C: LineageHistoryEvidenceV1, A: LineageAttestationVerifierV1>(
        self,
        context: &C,
        attestations: &A,
    ) -> Result<VerifiedTypedLineageEdgeSetV1<'wire>, LineageEdgeSetErrorV1> {
        if self.parent_commit != context.parent_commit()
            || self.parent_generation != context.parent_generation().as_bytes()
            || self.child_generation != context.child_generation().as_bytes()
        {
            return Err(LineageEdgeSetErrorV1::GenerationRootMismatch);
        }
        match context.is_direct_parent(context.child_commit(), self.parent_commit) {
            Some(true) => {}
            Some(false) => return Err(LineageEdgeSetErrorV1::NotDirectTransition),
            None => return Err(LineageEdgeSetErrorV1::EvidenceUnavailable),
        }
        self.verify_canonical_structure()?;
        for edge in self.edges() {
            verify_edge_context(edge, context, attestations)?;
        }
        Ok(VerifiedTypedLineageEdgeSetV1 {
            view: self,
            child_commit: context.child_commit(),
        })
    }

    fn verify_canonical_structure(self) -> Result<(), LineageEdgeSetErrorV1> {
        let mut previous = None;
        let mut ambiguous_offsets = Vec::new();
        ambiguous_offsets
            .try_reserve(self.count)
            .map_err(|_| LineageEdgeSetErrorV1::Allocation)?;
        for edge in self.edges() {
            if let Some(before) = previous {
                match canonical_view_cmp(before, edge) {
                    Ordering::Less => {}
                    Ordering::Equal => return Err(LineageEdgeSetErrorV1::DuplicateEdge),
                    Ordering::Greater => return Err(LineageEdgeSetErrorV1::NonCanonicalOrder),
                }
                if same_endpoint_pair(before, edge) {
                    return Err(LineageEdgeSetErrorV1::DuplicateEdge);
                }
            }
            if matches!(edge.status, LineageStatusViewV1::Ambiguous { .. }) {
                ambiguous_offsets.push(edge.offset);
            }
            previous = Some(edge);
        }
        validate_ambiguous_groups(self, &mut ambiguous_offsets)
    }

    /// Iterates typed edge records borrowing their evidence bytes from `self`.
    #[must_use]
    pub fn edges(self) -> LineageEdgeIterV1<'wire> {
        LineageEdgeIterV1 {
            bytes: self.bytes,
            next: 0,
            count: self.count,
        }
    }
}

/// Iterator over borrowed fixed-width edges.
#[derive(Clone, Debug)]
pub struct LineageEdgeIterV1<'wire> {
    bytes: &'wire [u8],
    next: usize,
    count: usize,
}

impl<'wire> Iterator for LineageEdgeIterV1<'wire> {
    type Item = LineageEdgeViewV1<'wire>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.count {
            return None;
        }
        let offset = HEADER_BYTES.checked_add(self.next.checked_mul(EDGE_BYTES)?)?;
        self.next += 1;
        decode_edge(self.bytes, offset).ok()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for LineageEdgeIterV1<'_> {}

/// Borrowed decoded edge. `evidence_bytes` is the exact 32-byte status
/// evidence field used in the canonical statement supplied to attestations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageEdgeViewV1<'wire> {
    offset: usize,
    kind: LineageKindV1,
    status: LineageStatusViewV1<'wire>,
    source_commit: super::HistoryCommitId,
    source_generation: &'wire [u8; 32],
    source: DeclarationIdentity,
    target_generation: &'wire [u8; 32],
    target: DeclarationIdentity,
    evidence_bytes: &'wire [u8; 32],
    canonical_record: &'wire [u8],
}

impl<'wire> LineageEdgeViewV1<'wire> {
    /// Returns the typed relationship kind.
    #[must_use]
    pub const fn kind(self) -> LineageKindV1 {
        self.kind
    }

    /// Returns the exact source commit claim.
    #[must_use]
    pub const fn source_commit(self) -> super::HistoryCommitId {
        self.source_commit
    }

    /// Returns the claimed source generation root as borrowed bytes.
    #[must_use]
    pub const fn source_generation_claim(self) -> &'wire [u8; 32] {
        self.source_generation
    }

    /// Returns the original declaration identity.
    #[must_use]
    pub const fn source(self) -> DeclarationIdentity {
        self.source
    }

    /// Returns the child declaration identity without rewriting it.
    #[must_use]
    pub const fn target(self) -> DeclarationIdentity {
        self.target
    }

    /// Returns the exact generation root in which the target is claimed.
    #[must_use]
    pub const fn target_generation_claim(self) -> &'wire [u8; 32] {
        self.target_generation
    }

    /// Returns this edge's status.
    #[must_use]
    pub const fn status(self) -> LineageStatusViewV1<'wire> {
        self.status
    }

    /// Returns the exact canonical row bytes for independent attestation.
    #[must_use]
    pub const fn canonical_record(self) -> &'wire [u8] {
        self.canonical_record
    }
}

/// Borrowed status decoded from one wire row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineageStatusViewV1<'wire> {
    /// Requires an external authority verifier to accept the exact row.
    Confirmed {
        /// Claimed attestation object ID; verification has not run.
        attestation: &'wire [u8; 32],
    },
    /// Candidate group membership; all declared alternatives must be present.
    Ambiguous {
        /// Claimed candidate-group identity.
        group: &'wire [u8; 32],
        /// Claimed zero-based position in that group.
        index: u16,
        /// Claimed total group size.
        count: u16,
    },
    /// Non-authoritative unresolved evidence.
    Unresolved {
        /// Claimed reason the relation remains unresolved.
        reason: UnresolvedLineageReasonV1,
        /// Optional borrowed nonzero diagnostic digest.
        evidence: Option<&'wire [u8; 32]>,
    },
}

impl<'wire> LineageStatusViewV1<'wire> {
    fn tag(self) -> u8 {
        match self {
            Self::Confirmed { .. } => 1,
            Self::Ambiguous { .. } => 2,
            Self::Unresolved { .. } => 3,
        }
    }
}

/// Evidence needed to validate one transition. Implementations must resolve
/// exact immutable history records and complete semantic readers; `None`
/// means the fact is unavailable, not false. `is_direct_parent` must bind the
/// exact child commit. `is_strict_ancestor` must compare the named commit's
/// verified generation root as well as ancestry.
pub trait LineageHistoryEvidenceV1 {
    /// Exact admitted child commit whose locator carries this edge set.
    fn child_commit(&self) -> super::HistoryCommitId;
    /// Exact admitted parent commit.
    fn parent_commit(&self) -> super::HistoryCommitId;
    /// Exact already-verified parent V2 root.
    fn parent_generation(&self) -> VerifiedLineageRootV2;
    /// Exact already-verified child V2 root.
    fn child_generation(&self) -> VerifiedLineageRootV2;
    /// Whether `parent` is the direct first parent of this exact child commit.
    /// The edge wire omits its own child commit ID to avoid a hash cycle, so
    /// the adapter must bind this relation from the admitted history record.
    fn is_direct_parent(
        &self,
        child: super::HistoryCommitId,
        parent: super::HistoryCommitId,
    ) -> Option<bool>;
    /// Whether an identity exists in this exact verified historical reader.
    fn declaration_present(
        &self,
        commit: super::HistoryCommitId,
        generation: &[u8; 32],
        identity: DeclarationIdentity,
    ) -> Option<bool>;
    /// Whether an identity exists in the exact verified child reader.
    fn child_declaration_present(&self, identity: DeclarationIdentity) -> Option<bool>;
    /// Whether `(ancestor, generation)` is a strict ancestor of the exact
    /// parent. `None` means ancestry was not established.
    fn is_strict_ancestor(
        &self,
        ancestor: super::HistoryCommitId,
        generation: &[u8; 32],
    ) -> Option<bool>;
}

/// Validates an independently supplied proof over one exact transition
/// statement. The statement digest excludes the proof object's own ID, so
/// content-addressed signatures cannot be circular. Heuristic similarity
/// scores are not attestations.
pub trait LineageAttestationVerifierV1 {
    /// Returns true only when the referenced immutable proof verifies this
    /// exact statement digest under the transition's committed roots.
    fn verifies(
        &self,
        parent_commit: super::HistoryCommitId,
        parent_generation: &[u8; 32],
        child_generation: &[u8; 32],
        statement: LineageConfirmationStatementV1,
        attestation: &[u8; 32],
    ) -> bool;
}

/// Reject-all policy for callers that have no trusted confirmation authority.
#[derive(Clone, Copy, Debug, Default)]
pub struct RejectLineageConfirmationsV1;

impl LineageAttestationVerifierV1 for RejectLineageConfirmationsV1 {
    fn verifies(
        &self,
        _parent_commit: super::HistoryCommitId,
        _parent_generation: &[u8; 32],
        _child_generation: &[u8; 32],
        _statement: LineageConfirmationStatementV1,
        _attestation: &[u8; 32],
    ) -> bool {
        false
    }
}

/// Opaque result proving that one borrowed edge set passed the supplied
/// history and authority checks.
#[derive(Clone, Copy, Debug)]
pub struct VerifiedTypedLineageEdgeSetV1<'wire> {
    view: BorrowedTypedLineageEdgeSetV1<'wire>,
    child_commit: super::HistoryCommitId,
}

impl<'wire> VerifiedTypedLineageEdgeSetV1<'wire> {
    /// Returns the unchanged borrowed edge stream.
    #[must_use]
    pub fn edges(self) -> VerifiedLineageEdgeIterV1<'wire> {
        VerifiedLineageEdgeIterV1 {
            inner: self.view.edges(),
        }
    }

    /// Returns the exact parent commit and root claims this proof checked.
    #[must_use]
    pub fn parent(self) -> (super::HistoryCommitId, &'wire [u8; 32]) {
        (self.view.parent_commit, self.view.parent_generation)
    }

    /// Returns the exact child commit proven to directly descend from the
    /// parent for the checked transition.
    #[must_use]
    pub const fn child_commit(self) -> super::HistoryCommitId {
        self.child_commit
    }

    /// Returns the exact child root claim this proof checked.
    #[must_use]
    pub const fn child_generation(self) -> &'wire [u8; 32] {
        self.view.child_generation
    }
}

/// A lineage edge yielded only from a fully verified edge set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedLineageEdgeViewV1<'wire> {
    view: LineageEdgeViewV1<'wire>,
}

impl<'wire> VerifiedLineageEdgeViewV1<'wire> {
    /// Returns the verified relationship kind.
    #[must_use]
    pub const fn kind(self) -> LineageKindV1 {
        self.view.kind
    }

    /// Returns the exact source history commit.
    #[must_use]
    pub const fn source_commit(self) -> super::HistoryCommitId {
        self.view.source_commit
    }

    /// Returns the exact source generation root.
    #[must_use]
    pub const fn source_generation_claim(self) -> &'wire [u8; 32] {
        self.view.source_generation
    }

    /// Returns the exact target generation root checked by the edge set.
    #[must_use]
    pub const fn target_generation_claim(self) -> &'wire [u8; 32] {
        self.view.target_generation
    }

    /// Returns the unchanged source declaration identity.
    #[must_use]
    pub const fn source(self) -> DeclarationIdentity {
        self.view.source
    }

    /// Returns the unchanged target declaration identity.
    #[must_use]
    pub const fn target(self) -> DeclarationIdentity {
        self.view.target
    }

    /// Returns an epistemic status that can be obtained only after history
    /// checks and any required confirmation authority have passed.
    #[must_use]
    pub const fn status(self) -> VerifiedLineageStatusV1<'wire> {
        match self.view.status {
            LineageStatusViewV1::Confirmed { attestation } => {
                VerifiedLineageStatusV1::Confirmed { attestation }
            }
            LineageStatusViewV1::Ambiguous {
                group,
                index,
                count,
            } => VerifiedLineageStatusV1::Ambiguous {
                group,
                index,
                count,
            },
            LineageStatusViewV1::Unresolved { reason, evidence } => {
                VerifiedLineageStatusV1::Unresolved { reason, evidence }
            }
        }
    }
}

/// Exact-size iterator over edges whose set passed the configured verifiers.
#[derive(Clone, Debug)]
pub struct VerifiedLineageEdgeIterV1<'wire> {
    inner: LineageEdgeIterV1<'wire>,
}

impl<'wire> Iterator for VerifiedLineageEdgeIterV1<'wire> {
    type Item = VerifiedLineageEdgeViewV1<'wire>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|view| VerifiedLineageEdgeViewV1 { view })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for VerifiedLineageEdgeIterV1<'_> {}

/// Status carried by a verified edge view. Its type is distinct from raw
/// decoded claims, especially the `Confirmed` state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifiedLineageStatusV1<'wire> {
    /// An external authority accepted the exact edge and transition proof.
    Confirmed {
        /// Attestation ID accepted by the configured authority verifier.
        attestation: &'wire [u8; 32],
    },
    /// Complete alternatives; no member was selected as the winner.
    Ambiguous {
        /// Identity of the verified complete candidate group.
        group: &'wire [u8; 32],
        /// Zero-based position in that group.
        index: u16,
        /// Verified total number of candidates in the group.
        count: u16,
    },
    /// The candidate remains unresolved after all known contradictions were
    /// rejected and unavailable facts were recorded.
    Unresolved {
        /// Reason this verified candidate remains unresolved.
        reason: UnresolvedLineageReasonV1,
        /// Optional borrowed nonzero diagnostic digest.
        evidence: Option<&'wire [u8; 32]>,
    },
}

/// Lineage bytes recovered from a commit-rooted V2 locator before historical
/// declaration membership, ancestry, and any confirmation authority have
/// been checked. Consumers may display these as unproven candidates only.
#[derive(Clone, Copy, Debug)]
pub struct UnprovenTypedLineageEdgeSetV1<'wire> {
    view: BorrowedTypedLineageEdgeSetV1<'wire>,
    child_commit: super::HistoryCommitId,
}

impl<'wire> UnprovenTypedLineageEdgeSetV1<'wire> {
    pub(crate) const fn root_bound(
        view: BorrowedTypedLineageEdgeSetV1<'wire>,
        child_commit: super::HistoryCommitId,
    ) -> Self {
        Self { view, child_commit }
    }

    /// Returns the exact committed parent-to-child edge stream.
    #[must_use]
    pub fn edges(self) -> LineageEdgeIterV1<'wire> {
        self.view.edges()
    }

    /// Returns the parent commit ID committed in the edge-set header.
    #[must_use]
    pub const fn parent_commit(self) -> super::HistoryCommitId {
        self.view.parent_commit
    }

    /// Returns the child history commit whose content-addressed locator
    /// carried the exact edge bytes.
    #[must_use]
    pub const fn child_commit(self) -> super::HistoryCommitId {
        self.child_commit
    }

    /// Returns the claimed parent semantic root.
    #[must_use]
    pub const fn parent_generation_claim(self) -> &'wire [u8; 32] {
        self.view.parent_generation
    }

    /// Returns the child semantic root checked against cold V2 replay.
    #[must_use]
    pub const fn child_generation_claim(self) -> &'wire [u8; 32] {
        self.view.child_generation
    }
}

/// Rejection from parsing, encoding, or validating a lineage edge set.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LineageEdgeSetErrorV1 {
    /// Wire magic does not name this schema.
    #[error("invalid typed lineage edge-set magic")]
    InvalidMagic,
    /// Wire size, count, or exact fixed-width framing is invalid.
    #[error("invalid typed lineage edge-set length")]
    InvalidLength,
    /// Checked byte-size arithmetic overflowed.
    #[error("typed lineage edge-set length overflow")]
    LengthOverflow,
    /// The edge or wire bound was exceeded.
    #[error("typed lineage edge-set exceeds its bound")]
    TooManyEdges,
    /// Bounded scratch or owned wire allocation failed.
    #[error("typed lineage edge-set allocation failed")]
    Allocation,
    /// An edge row contains an unknown tag or non-canonical reserved bytes.
    #[error("invalid typed lineage edge row")]
    InvalidEdge,
    /// Edge rows are not strictly sorted by canonical endpoint order.
    #[error("typed lineage edge rows are not in canonical order")]
    NonCanonicalOrder,
    /// One endpoint pair appears more than once, possibly with conflicting status.
    #[error("duplicate typed lineage endpoint pair")]
    DuplicateEdge,
    /// An ambiguous group omits or duplicates a declared candidate.
    #[error("ambiguous lineage candidate group is incomplete")]
    InvalidCandidateGroup,
    /// Parent or child generation claims do not match verified roots.
    #[error("typed lineage edge set names the wrong generation roots")]
    GenerationRootMismatch,
    /// The claimed parent is not the exact child's direct first parent.
    #[error("typed lineage edge set does not describe the direct history transition")]
    NotDirectTransition,
    /// A statement was requested for a decoded row from another edge set.
    #[error("typed lineage statement row does not belong to this edge set")]
    EdgeSetMismatch,
    /// A confirmation statement was requested for a non-confirmed row.
    #[error("typed lineage statement requires a confirmed edge")]
    NotConfirmed,
    /// A known history fact contradicts the edge's endpoint claim.
    #[error("typed lineage endpoint contradicts verified semantic contents")]
    EndpointMismatch,
    /// Required endpoint or ancestry evidence was unavailable for a resolved status.
    #[error("typed lineage endpoint evidence is unavailable")]
    EvidenceUnavailable,
    /// Resurrection source is not a strict ancestor of the immediate parent.
    #[error("resurrection source is not a verified strict ancestor")]
    NotStrictAncestor,
    /// Confirmation lacks an independently validated authority proof.
    #[error("lineage confirmation is not independently attested")]
    UnattestedConfirmation,
}

fn encode_edge(edge: &LineageEdgeV1, child_generation: VerifiedLineageRootV2, out: &mut Vec<u8>) {
    out.push(edge.kind as u8);
    match edge.status {
        LineageStatusV1::Confirmed { attestation } => {
            out.push(1);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(edge.source.commit.as_bytes());
            out.extend_from_slice(edge.source.generation.as_bytes());
            encode_identity(edge.source.declaration, out);
            // The target generation is the header's exact child root.
            out.extend_from_slice(child_generation.as_bytes());
            encode_identity(edge.target, out);
            out.extend_from_slice(attestation.as_bytes());
        }
        LineageStatusV1::Ambiguous {
            group,
            index,
            count,
        } => {
            out.push(2);
            out.extend_from_slice(&index.to_be_bytes());
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(edge.source.commit.as_bytes());
            out.extend_from_slice(edge.source.generation.as_bytes());
            encode_identity(edge.source.declaration, out);
            out.extend_from_slice(child_generation.as_bytes());
            encode_identity(edge.target, out);
            out.extend_from_slice(group.as_bytes());
        }
        LineageStatusV1::Unresolved { reason, evidence } => {
            out.push(3);
            out.extend_from_slice(&(reason as u16).to_be_bytes());
            out.extend_from_slice(&[0; 2]);
            out.extend_from_slice(edge.source.commit.as_bytes());
            out.extend_from_slice(edge.source.generation.as_bytes());
            encode_identity(edge.source.declaration, out);
            out.extend_from_slice(child_generation.as_bytes());
            encode_identity(edge.target, out);
            out.extend_from_slice(&evidence.unwrap_or([0; 32]));
        }
    }
}

fn encode_identity(identity: DeclarationIdentity, out: &mut Vec<u8>) {
    out.extend_from_slice(identity.family.as_bytes());
    out.extend_from_slice(identity.variant.as_bytes());
}

fn canonical_edge_cmp(left: &LineageEdgeV1, right: &LineageEdgeV1) -> Ordering {
    left.source
        .commit
        .as_bytes()
        .cmp(right.source.commit.as_bytes())
        .then_with(|| {
            left.source
                .generation
                .as_bytes()
                .cmp(right.source.generation.as_bytes())
        })
        .then_with(|| left.source.declaration.cmp(&right.source.declaration))
        .then_with(|| left.target.cmp(&right.target))
        .then_with(|| (left.kind as u8).cmp(&(right.kind as u8)))
        .then_with(|| status_key(left.status).cmp(&status_key(right.status)))
}

fn status_key(status: LineageStatusV1) -> (u8, [u8; 32], u16, u16) {
    match status {
        LineageStatusV1::Confirmed { attestation } => (1, *attestation.as_bytes(), 0, 0),
        LineageStatusV1::Ambiguous {
            group,
            index,
            count,
        } => (2, *group.as_bytes(), index, count),
        LineageStatusV1::Unresolved { reason, evidence } => {
            (3, evidence.unwrap_or([0; 32]), reason as u16, 0)
        }
    }
}

fn validate_owned_edges(edges: &[LineageEdgeV1]) -> Result<(), LineageEdgeSetErrorV1> {
    if edges.iter().any(|edge| {
        matches!(
            edge.status,
            LineageStatusV1::Confirmed { attestation }
                if attestation.as_bytes() == &[0; 32]
        ) || match edge.status {
            LineageStatusV1::Unresolved {
                evidence: Some(evidence),
                ..
            } => evidence == [0; 32],
            _ => false,
        }
    }) {
        return Err(LineageEdgeSetErrorV1::InvalidEdge);
    }
    for pair in edges.windows(2) {
        if same_owned_endpoint_pair(&pair[0], &pair[1]) {
            return Err(LineageEdgeSetErrorV1::DuplicateEdge);
        }
    }
    let mut ambiguous = Vec::new();
    ambiguous
        .try_reserve(edges.len())
        .map_err(|_| LineageEdgeSetErrorV1::Allocation)?;
    ambiguous.extend(
        edges
            .iter()
            .enumerate()
            .filter_map(|(index, edge)| match edge.status {
                LineageStatusV1::Ambiguous {
                    group,
                    index: candidate,
                    count,
                } => Some((group, candidate, count, edge.kind, index)),
                _ => None,
            }),
    );
    validate_owned_groups(&mut ambiguous)
}

fn validate_owned_groups(
    ambiguous: &mut [(LineageCandidateGroupIdV1, u16, u16, LineageKindV1, usize)],
) -> Result<(), LineageEdgeSetErrorV1> {
    ambiguous.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.4.cmp(&right.4))
    });
    let mut start = 0;
    while start < ambiguous.len() {
        let group = ambiguous[start].0;
        let declared = ambiguous[start].2;
        let kind = ambiguous[start].3;
        let mut end = start + 1;
        while end < ambiguous.len() && ambiguous[end].0 == group {
            end += 1;
        }
        if group.as_bytes() == &[0; 32]
            || declared < 2
            || declared > MAX_LINEAGE_CANDIDATES_PER_GROUP_V1
            || usize::from(declared) != end - start
        {
            return Err(LineageEdgeSetErrorV1::InvalidCandidateGroup);
        }
        for (expected, candidate) in ambiguous[start..end].iter().enumerate() {
            if candidate.1
                != u16::try_from(expected)
                    .map_err(|_| LineageEdgeSetErrorV1::InvalidCandidateGroup)?
                || candidate.2 != declared
                || candidate.3 != kind
            {
                return Err(LineageEdgeSetErrorV1::InvalidCandidateGroup);
            }
        }
        start = end;
    }
    Ok(())
}

fn validate_ambiguous_groups(
    view: BorrowedTypedLineageEdgeSetV1<'_>,
    offsets: &mut [usize],
) -> Result<(), LineageEdgeSetErrorV1> {
    offsets.sort_by(|left, right| {
        let left_edge = decode_edge(view.bytes, *left).ok();
        let right_edge = decode_edge(view.bytes, *right).ok();
        match (left_edge, right_edge) {
            (Some(left), Some(right)) => match (left.status, right.status) {
                (
                    LineageStatusViewV1::Ambiguous {
                        group: left_group,
                        index: left_index,
                        ..
                    },
                    LineageStatusViewV1::Ambiguous {
                        group: right_group,
                        index: right_index,
                        ..
                    },
                ) => left_group
                    .cmp(right_group)
                    .then_with(|| left_index.cmp(&right_index)),
                _ => Ordering::Equal,
            },
            _ => Ordering::Equal,
        }
    });
    let mut start = 0;
    while start < offsets.len() {
        let first = decode_edge(view.bytes, offsets[start])?;
        let LineageStatusViewV1::Ambiguous { group, count, .. } = first.status else {
            return Err(LineageEdgeSetErrorV1::InvalidCandidateGroup);
        };
        let mut end = start + 1;
        while end < offsets.len() {
            let edge = decode_edge(view.bytes, offsets[end])?;
            match edge.status {
                LineageStatusViewV1::Ambiguous { group: next, .. } if next == group => end += 1,
                _ => break,
            }
        }
        if group == &[0; 32]
            || count < 2
            || count > MAX_LINEAGE_CANDIDATES_PER_GROUP_V1
            || usize::from(count) != end - start
        {
            return Err(LineageEdgeSetErrorV1::InvalidCandidateGroup);
        }
        for (expected, offset) in offsets[start..end].iter().enumerate() {
            let edge = decode_edge(view.bytes, *offset)?;
            match edge.status {
                LineageStatusViewV1::Ambiguous {
                    index,
                    count: next_count,
                    ..
                } if index
                    == u16::try_from(expected)
                        .map_err(|_| LineageEdgeSetErrorV1::InvalidCandidateGroup)?
                    && next_count == count
                    && edge.kind == first.kind => {}
                _ => return Err(LineageEdgeSetErrorV1::InvalidCandidateGroup),
            }
        }
        start = end;
    }
    Ok(())
}

fn verify_edge_context<C: LineageHistoryEvidenceV1, A: LineageAttestationVerifierV1>(
    edge: LineageEdgeViewV1<'_>,
    context: &C,
    attestations: &A,
) -> Result<(), LineageEdgeSetErrorV1> {
    let resolved = !matches!(edge.status, LineageStatusViewV1::Unresolved { .. });
    let parent_commit = context.parent_commit();
    let parent_generation = context.parent_generation();
    let child_generation = context.child_generation();
    let parent_root = parent_generation.as_bytes();
    let child_root = child_generation.as_bytes();
    if edge.target_generation != child_root {
        return Err(LineageEdgeSetErrorV1::GenerationRootMismatch);
    }
    match edge.kind {
        LineageKindV1::Rename => {
            if edge.source_commit != parent_commit
                || edge.source_generation != parent_root
                || edge.source == edge.target
            {
                return Err(LineageEdgeSetErrorV1::EndpointMismatch);
            }
            require_presence(
                context.declaration_present(parent_commit, parent_root, edge.source),
                true,
                resolved,
            )?;
            require_presence(
                context.child_declaration_present(edge.source),
                false,
                resolved,
            )?;
            require_presence(
                context.declaration_present(parent_commit, parent_root, edge.target),
                false,
                resolved,
            )?;
            require_presence(
                context.child_declaration_present(edge.target),
                true,
                resolved,
            )?;
        }
        LineageKindV1::Resurrection => {
            if edge.source_commit == parent_commit || edge.source_generation == parent_root {
                return Err(LineageEdgeSetErrorV1::NotStrictAncestor);
            }
            if edge.source != edge.target {
                return Err(LineageEdgeSetErrorV1::EndpointMismatch);
            }
            match context.is_strict_ancestor(edge.source_commit, edge.source_generation) {
                Some(true) => {}
                Some(false) => return Err(LineageEdgeSetErrorV1::NotStrictAncestor),
                None if !resolved => {}
                None => return Err(LineageEdgeSetErrorV1::EvidenceUnavailable),
            }
            require_presence(
                context.declaration_present(
                    edge.source_commit,
                    edge.source_generation,
                    edge.source,
                ),
                true,
                resolved,
            )?;
            require_presence(
                context.declaration_present(parent_commit, parent_root, edge.source),
                false,
                resolved,
            )?;
            require_presence(
                context.declaration_present(parent_commit, parent_root, edge.target),
                false,
                resolved,
            )?;
            require_presence(
                context.child_declaration_present(edge.target),
                true,
                resolved,
            )?;
        }
    }
    if let LineageStatusViewV1::Confirmed { attestation } = edge.status {
        let statement = confirmation_statement_digest(parent_commit, parent_root, child_root, edge);
        if !attestations.verifies(
            parent_commit,
            parent_root,
            child_root,
            statement,
            attestation,
        ) {
            return Err(LineageEdgeSetErrorV1::UnattestedConfirmation);
        }
    }
    Ok(())
}

fn confirmation_statement_digest(
    parent_commit: super::HistoryCommitId,
    parent_generation: &[u8; 32],
    child_generation: &[u8; 32],
    edge: LineageEdgeViewV1<'_>,
) -> LineageConfirmationStatementV1 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.lineage-confirmation-statement.v1\0");
    hasher.update(parent_commit.as_bytes());
    hasher.update(parent_generation);
    hasher.update(child_generation);
    // The fixed endpoint/status prefix includes `Confirmed` but deliberately
    // excludes the trailing attestation ID in bytes 166..198.
    hasher.update(&edge.canonical_record[..166]);
    LineageConfirmationStatementV1(*hasher.finalize().as_bytes())
}

fn require_presence(
    observed: Option<bool>,
    expected: bool,
    resolved: bool,
) -> Result<(), LineageEdgeSetErrorV1> {
    match observed {
        Some(value) if value == expected => Ok(()),
        Some(_) => Err(LineageEdgeSetErrorV1::EndpointMismatch),
        None if resolved => Err(LineageEdgeSetErrorV1::EvidenceUnavailable),
        None => Ok(()),
    }
}

fn same_owned_endpoint_pair(left: &LineageEdgeV1, right: &LineageEdgeV1) -> bool {
    left.source.commit == right.source.commit
        && left.source.generation == right.source.generation
        && left.source.declaration == right.source.declaration
        && left.target == right.target
}

fn same_endpoint_pair(left: LineageEdgeViewV1<'_>, right: LineageEdgeViewV1<'_>) -> bool {
    left.source_commit == right.source_commit
        && left.source_generation == right.source_generation
        && left.source == right.source
        && left.target == right.target
}

fn canonical_view_cmp(left: LineageEdgeViewV1<'_>, right: LineageEdgeViewV1<'_>) -> Ordering {
    left.source_commit
        .as_bytes()
        .cmp(right.source_commit.as_bytes())
        .then_with(|| left.source_generation.cmp(right.source_generation))
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.target_generation.cmp(right.target_generation))
        .then_with(|| left.target.cmp(&right.target))
        .then_with(|| (left.kind as u8).cmp(&(right.kind as u8)))
        .then_with(|| left.status.tag().cmp(&right.status.tag()))
        .then_with(|| left.evidence_bytes.cmp(right.evidence_bytes))
        .then_with(|| left.canonical_record[2..6].cmp(&right.canonical_record[2..6]))
}

fn decode_edge(
    bytes: &[u8],
    offset: usize,
) -> Result<LineageEdgeViewV1<'_>, LineageEdgeSetErrorV1> {
    let record = bytes
        .get(
            offset
                ..offset
                    .checked_add(EDGE_BYTES)
                    .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?,
        )
        .ok_or(LineageEdgeSetErrorV1::InvalidLength)?;
    let kind = LineageKindV1::from_code(record[0]).ok_or(LineageEdgeSetErrorV1::InvalidEdge)?;
    let status_tag = record[1];
    let metadata = &record[2..6];
    let source_commit = super::HistoryCommitId::from_bytes(read_array::<32>(record, 6)?);
    let source_generation = read_array_ref::<32>(record, 38)?;
    let source = decode_identity(record, 70)?;
    let target_generation = read_array_ref::<32>(record, 102)?;
    let target = decode_identity(record, 134)?;
    let evidence_bytes = read_array_ref::<32>(record, 166)?;
    let status = match status_tag {
        1 if metadata == [0; 4] && evidence_bytes != &[0; 32] => LineageStatusViewV1::Confirmed {
            attestation: evidence_bytes,
        },
        2 => {
            let index = u16::from_be_bytes([metadata[0], metadata[1]]);
            let count = u16::from_be_bytes([metadata[2], metadata[3]]);
            if evidence_bytes == &[0; 32]
                || count < 2
                || count > MAX_LINEAGE_CANDIDATES_PER_GROUP_V1
                || index >= count
            {
                return Err(LineageEdgeSetErrorV1::InvalidEdge);
            }
            LineageStatusViewV1::Ambiguous {
                group: evidence_bytes,
                index,
                count,
            }
        }
        3 => {
            let reason_code = u16::from_be_bytes([metadata[0], metadata[1]]);
            if metadata[2..4] != [0; 2] {
                return Err(LineageEdgeSetErrorV1::InvalidEdge);
            }
            let reason = UnresolvedLineageReasonV1::from_code(reason_code)
                .ok_or(LineageEdgeSetErrorV1::InvalidEdge)?;
            LineageStatusViewV1::Unresolved {
                reason,
                evidence: (evidence_bytes != &[0; 32]).then_some(evidence_bytes),
            }
        }
        _ => return Err(LineageEdgeSetErrorV1::InvalidEdge),
    };
    Ok(LineageEdgeViewV1 {
        offset,
        kind,
        status,
        source_commit,
        source_generation,
        source,
        target_generation,
        target,
        evidence_bytes,
        canonical_record: record,
    })
}

fn decode_identity(
    bytes: &[u8],
    offset: usize,
) -> Result<DeclarationIdentity, LineageEdgeSetErrorV1> {
    let family = read_array::<16>(bytes, offset)?;
    let variant = read_array::<16>(bytes, offset + 16)?;
    Ok(DeclarationIdentity {
        family: DeclarationFamilyId::from_raw(family),
        variant: VariantFingerprint::from_raw(variant),
    })
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; N], LineageEdgeSetErrorV1> {
    let source = bytes
        .get(
            offset
                ..offset
                    .checked_add(N)
                    .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?,
        )
        .ok_or(LineageEdgeSetErrorV1::InvalidLength)?;
    let mut result = [0; N];
    result.copy_from_slice(source);
    Ok(result)
}

fn read_array_ref<const N: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<&[u8; N], LineageEdgeSetErrorV1> {
    bytes
        .get(
            offset
                ..offset
                    .checked_add(N)
                    .ok_or(LineageEdgeSetErrorV1::LengthOverflow)?,
        )
        .ok_or(LineageEdgeSetErrorV1::InvalidLength)?
        .try_into()
        .map_err(|_| LineageEdgeSetErrorV1::InvalidLength)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Clone)]
    struct ModelHistory {
        child_commit: super::super::HistoryCommitId,
        parent_commit: super::super::HistoryCommitId,
        direct_parent: bool,
        parent_generation: VerifiedLineageRootV2,
        child_generation: VerifiedLineageRootV2,
        parent_rows: BTreeSet<DeclarationIdentity>,
        child_rows: BTreeSet<DeclarationIdentity>,
        snapshots:
            BTreeMap<super::super::HistoryCommitId, ([u8; 32], BTreeSet<DeclarationIdentity>)>,
        ancestors: BTreeSet<(super::super::HistoryCommitId, [u8; 32])>,
    }

    impl LineageHistoryEvidenceV1 for ModelHistory {
        fn child_commit(&self) -> super::super::HistoryCommitId {
            self.child_commit
        }
        fn parent_commit(&self) -> super::super::HistoryCommitId {
            self.parent_commit
        }
        fn parent_generation(&self) -> VerifiedLineageRootV2 {
            self.parent_generation
        }
        fn child_generation(&self) -> VerifiedLineageRootV2 {
            self.child_generation
        }
        fn is_direct_parent(
            &self,
            child: super::super::HistoryCommitId,
            parent: super::super::HistoryCommitId,
        ) -> Option<bool> {
            Some(child == self.child_commit && parent == self.parent_commit && self.direct_parent)
        }
        fn declaration_present(
            &self,
            commit: super::super::HistoryCommitId,
            root: &[u8; 32],
            identity: DeclarationIdentity,
        ) -> Option<bool> {
            if commit == self.parent_commit && root == self.parent_generation.as_bytes() {
                return Some(self.parent_rows.contains(&identity));
            }
            self.snapshots
                .get(&commit)
                .filter(|(snapshot_root, _)| snapshot_root == root)
                .map(|(_, rows)| rows.contains(&identity))
        }
        fn child_declaration_present(&self, identity: DeclarationIdentity) -> Option<bool> {
            Some(self.child_rows.contains(&identity))
        }
        fn is_strict_ancestor(
            &self,
            commit: super::super::HistoryCommitId,
            root: &[u8; 32],
        ) -> Option<bool> {
            Some(
                self.ancestors.contains(&(commit, *root))
                    && self
                        .snapshots
                        .get(&commit)
                        .is_some_and(|(snapshot_root, _)| snapshot_root == root),
            )
        }
    }

    struct TestAuthority {
        attestation: Option<[u8; 32]>,
        statement: Option<LineageConfirmationStatementV1>,
    }
    impl LineageAttestationVerifierV1 for TestAuthority {
        fn verifies(
            &self,
            parent_commit: super::super::HistoryCommitId,
            parent_generation: &[u8; 32],
            child_generation: &[u8; 32],
            statement: LineageConfirmationStatementV1,
            attestation: &[u8; 32],
        ) -> bool {
            self.attestation == Some(*attestation)
                && parent_commit == commit(1)
                && parent_generation == &[2; 32]
                && child_generation == &[3; 32]
                && self.statement == Some(statement)
        }
    }

    fn root(byte: u8) -> VerifiedLineageRootV2 {
        VerifiedLineageRootV2([byte; 32])
    }
    fn commit(byte: u8) -> super::super::HistoryCommitId {
        super::super::HistoryCommitId::from_bytes([byte; 32])
    }
    fn id(family: u8, variant: u8) -> DeclarationIdentity {
        DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([family; 16]),
            variant: VariantFingerprint::from_raw([variant; 16]),
        }
    }
    fn context(
        parent_rows: &[DeclarationIdentity],
        child_rows: &[DeclarationIdentity],
    ) -> ModelHistory {
        ModelHistory {
            child_commit: commit(3),
            parent_commit: commit(1),
            direct_parent: true,
            parent_generation: root(2),
            child_generation: root(3),
            parent_rows: parent_rows.iter().copied().collect(),
            child_rows: child_rows.iter().copied().collect(),
            snapshots: BTreeMap::new(),
            ancestors: BTreeSet::new(),
        }
    }

    #[test]
    fn rejects_a_skipped_intermediate_transition() {
        let old = id(5, 1);
        let new = id(5, 2);
        let mut history = context(&[old], &[new]);
        history.direct_parent = false;
        let edge = LineageEdgeV1::new(
            LineageKindV1::Rename,
            LineageSourceV1::new(commit(1), root(2), old),
            new,
            LineageStatusV1::Unresolved {
                reason: UnresolvedLineageReasonV1::InsufficientEvidence,
                evidence: None,
            },
        );
        assert_eq!(
            make_wire(&[edge])
                .borrow()
                .expect("parse")
                .verify(&history, &RejectLineageConfirmationsV1)
                .unwrap_err(),
            LineageEdgeSetErrorV1::NotDirectTransition
        );
    }
    fn make_wire(edges: &[LineageEdgeV1]) -> OwnedTypedLineageEdgeSetV1 {
        OwnedTypedLineageEdgeSetV1::encode(commit(1), root(2), root(3), edges)
            .expect("encode canonical edge set")
    }

    #[test]
    fn rejects_wrong_parent_or_child_root_claims() {
        let old = id(4, 1);
        let new = id(4, 2);
        let history = context(&[old], &[new]);
        let edge = LineageEdgeV1::new(
            LineageKindV1::Rename,
            LineageSourceV1::new(commit(1), root(2), old),
            new,
            LineageStatusV1::Unresolved {
                reason: UnresolvedLineageReasonV1::InsufficientEvidence,
                evidence: None,
            },
        );
        let wire = make_wire(&[edge]);
        let parsed = wire.borrow().expect("parse");
        assert_eq!(
            parsed
                .verify(&history, &RejectLineageConfirmationsV1)
                .expect("exact roots")
                .edges()
                .count(),
            1
        );

        let mut forged = wire.as_bytes().to_vec();
        forged[40] ^= 0x80;
        let parsed =
            BorrowedTypedLineageEdgeSetV1::parse(&forged).expect("well framed stale claim");
        assert_eq!(
            parsed
                .verify(&history, &RejectLineageConfirmationsV1)
                .unwrap_err(),
            LineageEdgeSetErrorV1::GenerationRootMismatch
        );

        let mut forged_child = wire.as_bytes().to_vec();
        forged_child[72] ^= 0x40;
        let parsed =
            BorrowedTypedLineageEdgeSetV1::parse(&forged_child).expect("framed child claim");
        assert_eq!(
            parsed
                .verify(&history, &RejectLineageConfirmationsV1)
                .unwrap_err(),
            LineageEdgeSetErrorV1::GenerationRootMismatch
        );
    }

    #[test]
    fn ambiguity_requires_the_full_candidate_group_and_never_selects_a_winner() {
        let old = id(8, 1);
        let first = id(8, 2);
        let second = id(8, 3);
        let group = LineageCandidateGroupIdV1::from_bytes([0x55; 32]);
        let make = |target, index| {
            LineageEdgeV1::new(
                LineageKindV1::Rename,
                LineageSourceV1::new(commit(1), root(2), old),
                target,
                LineageStatusV1::Ambiguous {
                    group,
                    index,
                    count: 2,
                },
            )
        };
        let history = context(&[old], &[first, second]);
        let all = make_wire(&[make(first, 0), make(second, 1)]);
        assert_eq!(
            all.borrow()
                .expect("parse")
                .verify(&history, &RejectLineageConfirmationsV1)
                .expect("complete alternatives")
                .edges()
                .count(),
            2
        );
        assert_eq!(
            OwnedTypedLineageEdgeSetV1::encode(commit(1), root(2), root(3), &[make(first, 0)]),
            Err(LineageEdgeSetErrorV1::InvalidCandidateGroup)
        );
        assert_eq!(
            OwnedTypedLineageEdgeSetV1::encode(
                commit(1),
                root(2),
                root(3),
                &[make(first, 0), make(first, 1)]
            ),
            Err(LineageEdgeSetErrorV1::DuplicateEdge)
        );
        let conflicting_status = LineageEdgeV1::new(
            LineageKindV1::Rename,
            LineageSourceV1::new(commit(1), root(2), old),
            first,
            LineageStatusV1::Unresolved {
                reason: UnresolvedLineageReasonV1::InsufficientEvidence,
                evidence: None,
            },
        );
        assert_eq!(
            OwnedTypedLineageEdgeSetV1::encode(
                commit(1),
                root(2),
                root(3),
                &[make(first, 0), make(second, 1), conflicting_status],
            ),
            Err(LineageEdgeSetErrorV1::DuplicateEdge)
        );

        // Two independent candidate sets with a colliding group id cannot be
        // merged: the observed group count no longer matches either claim.
        let collided = [
            make(first, 0),
            make(second, 1),
            LineageEdgeV1::new(
                LineageKindV1::Rename,
                LineageSourceV1::new(commit(1), root(2), id(9, 1)),
                id(9, 2),
                LineageStatusV1::Ambiguous {
                    group,
                    index: 0,
                    count: 2,
                },
            ),
            LineageEdgeV1::new(
                LineageKindV1::Rename,
                LineageSourceV1::new(commit(1), root(2), id(9, 1)),
                id(9, 3),
                LineageStatusV1::Ambiguous {
                    group,
                    index: 1,
                    count: 2,
                },
            ),
        ];
        assert_eq!(
            OwnedTypedLineageEdgeSetV1::encode(commit(1), root(2), root(3), &collided),
            Err(LineageEdgeSetErrorV1::InvalidCandidateGroup)
        );
    }

    #[test]
    fn resurrection_requires_a_strict_ancestor_and_exact_origin_root() {
        let returning = id(9, 1);
        let origin_commit = commit(7);
        let origin_root = root(6);
        let mut history = context(&[], &[returning]);
        history.snapshots.insert(
            origin_commit,
            (*origin_root.as_bytes(), [returning].into_iter().collect()),
        );
        history
            .ancestors
            .insert((origin_commit, *origin_root.as_bytes()));
        let edge = LineageEdgeV1::new(
            LineageKindV1::Resurrection,
            LineageSourceV1::new(origin_commit, origin_root, returning),
            returning,
            LineageStatusV1::Unresolved {
                reason: UnresolvedLineageReasonV1::InsufficientEvidence,
                evidence: None,
            },
        );
        let wire = make_wire(&[edge]);
        assert!(
            wire.borrow()
                .expect("parse")
                .verify(&history, &RejectLineageConfirmationsV1)
                .is_ok()
        );

        history.ancestors.clear();
        assert_eq!(
            wire.borrow()
                .expect("parse")
                .verify(&history, &RejectLineageConfirmationsV1)
                .unwrap_err(),
            LineageEdgeSetErrorV1::NotStrictAncestor
        );

        history
            .ancestors
            .insert((origin_commit, *origin_root.as_bytes()));
        let forged_edge = LineageEdgeV1::new(
            LineageKindV1::Resurrection,
            LineageSourceV1::new(origin_commit, root(5), returning),
            returning,
            LineageStatusV1::Unresolved {
                reason: UnresolvedLineageReasonV1::EvidenceUnavailable,
                evidence: None,
            },
        );
        let forged = make_wire(&[forged_edge]);
        assert_eq!(
            forged
                .borrow()
                .expect("parse forged origin root")
                .verify(&history, &RejectLineageConfirmationsV1)
                .unwrap_err(),
            LineageEdgeSetErrorV1::NotStrictAncestor
        );
    }

    #[test]
    fn heuristic_confirmation_is_rejected_without_independent_attestation() {
        let old = id(10, 1);
        let new = id(10, 2);
        let history = context(&[old], &[new]);
        let edge = LineageEdgeV1::new(
            LineageKindV1::Rename,
            LineageSourceV1::new(commit(1), root(2), old),
            new,
            LineageStatusV1::Confirmed {
                attestation: LineageAttestationId::from_bytes([0x33; 32]),
            },
        );
        let wire = make_wire(&[edge]);
        let borrowed = wire.borrow().expect("parse");
        let first_row = borrowed.edges().next().expect("edge");
        let statement = borrowed
            .confirmation_statement(first_row)
            .expect("derive statement before proof ID is known");
        assert_eq!(
            statement,
            confirmation_statement_digest(commit(1), &[2; 32], &[3; 32], first_row)
        );
        let authority = TestAuthority {
            attestation: Some([0x33; 32]),
            statement: Some(statement),
        };
        assert_eq!(
            wire.borrow()
                .expect("parse")
                .verify(&history, &RejectLineageConfirmationsV1)
                .unwrap_err(),
            LineageEdgeSetErrorV1::UnattestedConfirmation
        );
        let verified = wire
            .borrow()
            .expect("parse")
            .verify(&history, &authority)
            .expect("exact transition authority");
        assert_eq!(verified.child_commit(), commit(3));
        assert!(matches!(
            verified.edges().next().expect("verified edge").status(),
            VerifiedLineageStatusV1::Confirmed { attestation } if attestation == &[0x33; 32]
        ));

        let other_id_edge = LineageEdgeV1::new(
            LineageKindV1::Rename,
            LineageSourceV1::new(commit(1), root(2), old),
            new,
            LineageStatusV1::Confirmed {
                attestation: LineageAttestationId::from_bytes([0x44; 32]),
            },
        );
        let other_id_wire = make_wire(&[other_id_edge]);
        let second_row = other_id_wire
            .borrow()
            .expect("parse other attestation id")
            .edges()
            .next()
            .expect("edge");
        assert_eq!(
            other_id_wire
                .borrow()
                .expect("parse other attestation id")
                .confirmation_statement(second_row)
                .expect("same statement with a different proof ID"),
            statement
        );
        assert_eq!(
            other_id_wire
                .borrow()
                .expect("parse other attestation id")
                .confirmation_statement(first_row),
            Err(LineageEdgeSetErrorV1::EdgeSetMismatch)
        );

        // The same row proof is not portable across a different transition
        // root, even when that transition happens to contain the same rows.
        let mut other_transition = history.clone();
        other_transition.child_generation = root(4);
        let transplanted = OwnedTypedLineageEdgeSetV1::encode(commit(1), root(2), root(4), &[edge])
            .expect("encode same candidate under another transition");
        assert_eq!(
            transplanted
                .borrow()
                .expect("parse transplanted proof")
                .verify(&other_transition, &authority)
                .unwrap_err(),
            LineageEdgeSetErrorV1::UnattestedConfirmation
        );

        assert_eq!(
            OwnedTypedLineageEdgeSetV1::encode(
                commit(1),
                root(2),
                root(3),
                &[LineageEdgeV1::new(
                    LineageKindV1::Rename,
                    LineageSourceV1::new(commit(1), root(2), old),
                    new,
                    LineageStatusV1::Confirmed {
                        attestation: LineageAttestationId::from_bytes([0; 32]),
                    },
                )],
            ),
            Err(LineageEdgeSetErrorV1::InvalidEdge)
        );

        assert_eq!(
            OwnedTypedLineageEdgeSetV1::encode(
                commit(1),
                root(2),
                root(3),
                &[LineageEdgeV1::new(
                    LineageKindV1::Rename,
                    LineageSourceV1::new(commit(1), root(2), old),
                    new,
                    LineageStatusV1::Unresolved {
                        reason: UnresolvedLineageReasonV1::InsufficientEvidence,
                        evidence: Some([0; 32]),
                    },
                )],
            ),
            Err(LineageEdgeSetErrorV1::InvalidEdge)
        );

        let unresolved = make_wire(&[LineageEdgeV1::new(
            LineageKindV1::Rename,
            LineageSourceV1::new(commit(1), root(2), old),
            new,
            LineageStatusV1::Unresolved {
                reason: UnresolvedLineageReasonV1::InsufficientEvidence,
                evidence: None,
            },
        )]);
        let unresolved_view = unresolved.borrow().expect("parse unresolved row");
        assert_eq!(
            unresolved_view
                .confirmation_statement(unresolved_view.edges().next().expect("unresolved edge")),
            Err(LineageEdgeSetErrorV1::NotConfirmed)
        );
    }
}
