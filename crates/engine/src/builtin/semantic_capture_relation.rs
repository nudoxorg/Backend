//! Orthogonal, closure-bound capture facts for package semantic refreshes.
//!
//! This relation deliberately lives beside the selected workspace relations,
//! in the checked immutable closure.  A source scan can therefore publish its
//! structural frontier and a typed Pending capture without changing the v3
//! semantic publication grammar or replacing the last coherent selection.

use super::semantic_relation::{
    PartialSemanticCoverage, ProductSemanticPublicationKey, ProductSemanticPublicationRelation,
    SemanticPublicationClaim, SemanticPublicationCoverage, SemanticPublicationVersion,
    SemanticSourceCapture, SemanticUnavailableReason, decode_source_capture, decode_version,
    encode_claim, encode_coverage, encode_source_capture, encode_version,
};
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use core::num::NonZeroU32;

const MAGIC: &[u8; 4] = b"PSC1";
const ROOT_KEY: [u8; 32] = [0x53; 32];

/// Outcome retained independently from the selected semantic publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductSemanticCaptureOutcome {
    /// Structural facts committed; semantic work has not selected a result.
    Pending {
        /// The previous selected coherent generation, if present.
        prior: Option<SemanticPublicationVersion>,
    },
    /// Semantic work ended without a usable generation and there was no
    /// prior generation to preserve.
    Unavailable {
        /// Exact typed reason no generation was selected.
        reason: SemanticUnavailableReason,
    },
    /// Semantic work failed while the prior selected generation remains
    /// available as stale evidence.
    Failed {
        /// Previous coherent generation retained by the v3 relation.
        prior: SemanticPublicationVersion,
        /// Exact typed failure reason.
        reason: SemanticUnavailableReason,
    },
    /// A new selected generation completed this source capture.
    Published {
        /// Coverage of the selected generation.
        coverage: SemanticPublicationCoverage,
        /// Exact selected immutable generation claim.
        claim: SemanticPublicationClaim,
    },
}

/// Exact source and operation evidence for one package/profile capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductSemanticCaptureRecord {
    operation_key: Option<[u8; 32]>,
    request_identity: [u8; 32],
    capture: SemanticSourceCapture,
    base_workspace_root: [u8; 32],
    base_workspace_sequence: u64,
    source_workspace_root: [u8; 32],
    source_workspace_sequence: u64,
    source_commit: [u8; 32],
    outcome: ProductSemanticCaptureOutcome,
}

impl ProductSemanticCaptureRecord {
    /// Admits one capture bound to its exact keyed source transition.
    ///
    /// # Errors
    /// Rejects reserved identities, an incorrect next sequence, a capture
    /// carrying another operation key, or an inconsistent prior generation.
    pub fn new(
        operation_key: Option<[u8; 32]>,
        request_identity: [u8; 32],
        capture: SemanticSourceCapture,
        base_workspace_root: [u8; 32],
        base_workspace_sequence: u64,
        source_workspace_root: [u8; 32],
        source_workspace_sequence: u64,
        source_commit: [u8; 32],
        outcome: ProductSemanticCaptureOutcome,
    ) -> Result<Self, &'static str> {
        if operation_key.is_some_and(|key| key.iter().all(|byte| *byte == 0))
            || request_identity.iter().all(|byte| *byte == 0)
            || operation_key.as_ref() != capture.operation_key()
            || [base_workspace_root, source_workspace_root, source_commit]
                .iter()
                .any(|identity| identity.iter().all(|byte| *byte == 0))
            || base_workspace_sequence == u64::MAX
            || source_workspace_sequence == 0
            || source_workspace_sequence != base_workspace_sequence + 1
        {
            return Err("semantic capture operation or root identity is invalid");
        }
        Ok(Self {
            operation_key,
            request_identity,
            capture,
            base_workspace_root,
            base_workspace_sequence,
            source_workspace_root,
            source_workspace_sequence,
            source_commit,
            outcome,
        })
    }

    /// Caller-owned operation identity bound by the marker.
    #[must_use]
    pub const fn operation_key(&self) -> Option<&[u8; 32]> {
        self.operation_key.as_ref()
    }

    /// Deterministic request identity of the exact structural transition.
    #[must_use]
    pub const fn request_identity(&self) -> &[u8; 32] {
        &self.request_identity
    }

    /// Exact profile-specific input evidence.
    #[must_use]
    pub const fn capture(&self) -> SemanticSourceCapture {
        self.capture
    }

    /// Exact workspace root that immediately preceded structural capture.
    #[must_use]
    pub const fn base_workspace_root(&self) -> &[u8; 32] {
        &self.base_workspace_root
    }

    /// Sequence of the workspace base that preceded structural capture.
    #[must_use]
    pub const fn base_workspace_sequence(&self) -> u64 {
        self.base_workspace_sequence
    }

    /// Exact structural source root selected with the Pending capture.
    #[must_use]
    pub const fn source_workspace_root(&self) -> &[u8; 32] {
        &self.source_workspace_root
    }

    /// Sequence that selected the structural source root.
    #[must_use]
    pub const fn source_workspace_sequence(&self) -> u64 {
        self.source_workspace_sequence
    }

    /// Exact durable commit identity of the structural source transition.
    #[must_use]
    pub const fn source_commit(&self) -> &[u8; 32] {
        &self.source_commit
    }

    /// Typed semantic outcome associated with this source capture.
    #[must_use]
    pub const fn outcome(&self) -> ProductSemanticCaptureOutcome {
        self.outcome
    }

    /// Creates a terminal update while preserving the exact capture and root
    /// basis admitted with the structural source transition.
    ///
    /// # Errors
    /// Rejects a published outcome whose claim is not the selected coherent
    /// generation represented by its coverage value.
    pub fn with_outcome(
        &self,
        outcome: ProductSemanticCaptureOutcome,
    ) -> Result<Self, &'static str> {
        Self::new(
            self.operation_key,
            self.request_identity,
            self.capture,
            self.base_workspace_root,
            self.base_workspace_sequence,
            self.source_workspace_root,
            self.source_workspace_sequence,
            self.source_commit,
            outcome,
        )
    }
}

/// Auxiliary canonical relation for source-first semantic refresh facts.
#[derive(Debug)]
pub struct ProductSemanticCaptureRelation;

impl Relation for ProductSemanticCaptureRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 4;
    const VERSION: u8 = 1;

    type Key = ProductSemanticPublicationKey;
    type Value = ProductSemanticCaptureRecord;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        ProductSemanticPublicationRelation::encode_key(key, output);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        match value.operation_key {
            Some(key) => {
                output.push(1);
                output.extend_from_slice(&key);
            }
            None => output.push(0),
        }
        output.extend_from_slice(&value.request_identity);
        encode_source_capture(value.capture, output);
        output.extend_from_slice(&value.base_workspace_root);
        output.extend_from_slice(&value.base_workspace_sequence.to_be_bytes());
        output.extend_from_slice(&value.source_workspace_root);
        output.extend_from_slice(&value.source_workspace_sequence.to_be_bytes());
        output.extend_from_slice(&value.source_commit);
        match value.outcome {
            ProductSemanticCaptureOutcome::Pending { prior } => {
                output.push(1);
                encode_optional_prior(prior, output);
            }
            ProductSemanticCaptureOutcome::Unavailable { reason } => {
                output.push(2);
                output.push(reason as u8);
            }
            ProductSemanticCaptureOutcome::Failed { prior, reason } => {
                output.push(3);
                encode_version(prior, output);
                output.push(reason as u8);
            }
            ProductSemanticCaptureOutcome::Published { coverage, claim } => {
                output.push(4);
                encode_coverage(coverage, output);
                encode_claim(claim, output);
            }
        }
    }
}

impl CanonicalRelation for ProductSemanticCaptureRelation {
    fn encode_order_key(key: &Self::Key, output: &mut Vec<u8>) {
        ProductSemanticPublicationRelation::encode_order_key(key, output);
    }

    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let key = ProductSemanticPublicationRelation::decode_key(bytes)?;
        if !key.is_selected() {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(key)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        let mut rest = bytes
            .strip_prefix(MAGIC)
            .ok_or(RelationDecodeError::Malformed)?;
        let (&operation_key_tag, next) =
            rest.split_first().ok_or(RelationDecodeError::Malformed)?;
        rest = next;
        let operation_key = match operation_key_tag {
            0 => None,
            1 => {
                let (key, next) = take::<32>(rest)?;
                rest = next;
                Some(key)
            }
            _ => return Err(RelationDecodeError::Malformed),
        };
        let (request_identity, next) = take::<32>(rest)?;
        rest = next;
        let (capture, next) = decode_source_capture(rest)?;
        rest = next;
        let (base_workspace_root, next) = take::<32>(rest)?;
        rest = next;
        let (base_workspace_sequence, next) = take_u64(rest)?;
        rest = next;
        let (source_workspace_root, next) = take::<32>(rest)?;
        rest = next;
        let (source_workspace_sequence, next) = take_u64(rest)?;
        rest = next;
        let (source_commit, next) = take::<32>(rest)?;
        let (&tag, rest) = next.split_first().ok_or(RelationDecodeError::Malformed)?;
        let outcome = match tag {
            1 => {
                let (prior, rest) = decode_optional_prior(rest)?;
                if !rest.is_empty() {
                    return Err(RelationDecodeError::Malformed);
                }
                ProductSemanticCaptureOutcome::Pending { prior }
            }
            2 => {
                let [reason] = rest else {
                    return Err(RelationDecodeError::Malformed);
                };
                ProductSemanticCaptureOutcome::Unavailable {
                    reason: decode_reason(*reason)?,
                }
            }
            3 => {
                let (prior, rest) = decode_version(rest)?;
                let [reason] = rest else {
                    return Err(RelationDecodeError::Malformed);
                };
                ProductSemanticCaptureOutcome::Failed {
                    prior,
                    reason: decode_reason(*reason)?,
                }
            }
            4 => {
                let (coverage, rest) = decode_coverage(rest)?;
                let (claim_bytes, rest) = rest
                    .split_at_checked(super::semantic_relation::CLAIM_BYTES)
                    .ok_or(RelationDecodeError::Malformed)?;
                let claim = super::semantic_relation::decode_claim(claim_bytes)?;
                if !rest.is_empty() {
                    return Err(RelationDecodeError::Malformed);
                }
                ProductSemanticCaptureOutcome::Published { coverage, claim }
            }
            _ => return Err(RelationDecodeError::Malformed),
        };
        ProductSemanticCaptureRecord::new(
            operation_key,
            request_identity,
            capture,
            base_workspace_root,
            base_workspace_sequence,
            source_workspace_root,
            source_workspace_sequence,
            source_commit,
            outcome,
        )
        .map_err(|_| RelationDecodeError::Malformed)
    }
}

/// Root pointer object retained in the same checked closure as the capture
/// relation nodes. It is separate from `WorkspaceManifest`, preserving the
/// existing v3 selected relation set while keeping source evidence durable.
pub struct ProductSemanticCaptureRootSchema;

impl Schema for ProductSemanticCaptureRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 5;
    const VERSION: u8 = 1;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Creates the canonical closure object that points at one checked capture
/// relation root.
#[must_use]
pub fn semantic_capture_root_object(
    root: StateRoot<ProductSemanticCaptureRelation>,
) -> TypedObject {
    let key = ObjectKey::<ProductSemanticCaptureRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}

/// Opens the auxiliary capture relation selected by this exact workspace
/// closure. A missing pointer is the legacy state and contains no captures.
/// The pointed root and every relation node are re-admitted by the workspace
/// snapshot's registered store before a handle is returned.
pub fn semantic_capture_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<ProductSemanticCaptureRelation>>, WorkspaceRelationError>
{
    let key = ObjectKey::<ProductSemanticCaptureRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<ProductSemanticCaptureRelation>(
        SchemaIdentity::new(
            ProductSemanticCaptureRootSchema::DOMAIN,
            ProductSemanticCaptureRootSchema::TYPE,
            ProductSemanticCaptureRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

fn encode_optional_prior(prior: Option<SemanticPublicationVersion>, output: &mut Vec<u8>) {
    match prior {
        Some(prior) => {
            output.push(1);
            encode_version(prior, output);
        }
        None => output.push(0),
    }
}

fn decode_optional_prior(
    bytes: &[u8],
) -> Result<(Option<SemanticPublicationVersion>, &[u8]), RelationDecodeError> {
    match bytes.split_first().ok_or(RelationDecodeError::Malformed)? {
        (0, rest) => Ok((None, rest)),
        (1, rest) => decode_version(rest).map(|(prior, rest)| (Some(prior), rest)),
        _ => Err(RelationDecodeError::Malformed),
    }
}

fn decode_coverage(
    bytes: &[u8],
) -> Result<(SemanticPublicationCoverage, &[u8]), RelationDecodeError> {
    let (&tag, rest) = bytes.split_first().ok_or(RelationDecodeError::Malformed)?;
    match tag {
        1 => Ok((SemanticPublicationCoverage::Complete, rest)),
        2 => {
            let (completed, rest) = take_u32(rest)?;
            let (total, rest) = take_u32(rest)?;
            let completed = NonZeroU32::new(completed).ok_or(RelationDecodeError::Malformed)?;
            let total = NonZeroU32::new(total).ok_or(RelationDecodeError::Malformed)?;
            let coverage = PartialSemanticCoverage::new(completed, total)
                .map_err(|_| RelationDecodeError::Malformed)?;
            Ok((SemanticPublicationCoverage::Partial(coverage), rest))
        }
        _ => Err(RelationDecodeError::Malformed),
    }
}

fn decode_reason(tag: u8) -> Result<SemanticUnavailableReason, RelationDecodeError> {
    match tag {
        1 => Ok(SemanticUnavailableReason::Toolchain),
        2 => Ok(SemanticUnavailableReason::ProjectAuthority),
        3 => Ok(SemanticUnavailableReason::Cancelled),
        4 => Ok(SemanticUnavailableReason::Rejected),
        _ => Err(RelationDecodeError::Malformed),
    }
}

fn take<const N: usize>(bytes: &[u8]) -> Result<([u8; N], &[u8]), RelationDecodeError> {
    let (value, rest) = bytes
        .split_at_checked(N)
        .ok_or(RelationDecodeError::Malformed)?;
    Ok((
        value
            .try_into()
            .map_err(|_| RelationDecodeError::Malformed)?,
        rest,
    ))
}

fn take_u32(bytes: &[u8]) -> Result<(u32, &[u8]), RelationDecodeError> {
    let (value, rest) = take::<4>(bytes)?;
    Ok((u32::from_be_bytes(value), rest))
}

fn take_u64(bytes: &[u8]) -> Result<(u64, &[u8]), RelationDecodeError> {
    let (value, rest) = take::<8>(bytes)?;
    Ok((u64::from_be_bytes(value), rest))
}
