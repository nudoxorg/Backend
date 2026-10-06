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

const LEGACY_MAGIC: &[u8; 4] = b"PSC1";
const MAGIC: &[u8; 4] = b"PSC2";
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
    compiler_failure: Option<backend_library::PackageCompilerFailure>,
    compiler_failure_bytes: Option<Box<[u8]>>,
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
        Self::new_with_compiler_failure(
            operation_key,
            request_identity,
            capture,
            base_workspace_root,
            base_workspace_sequence,
            source_workspace_root,
            source_workspace_sequence,
            source_commit,
            outcome,
            None,
        )
    }

    /// Admits a capture and, when semantic lowering refused a package, its
    /// bounded typed compiler-failure projection.
    ///
    /// # Errors
    /// Rejects the same invalid capture identities as [`Self::new`], failure
    /// JSON over the DTO's bound, or a compiler failure attached to a
    /// non-terminal capture state.
    pub fn new_with_compiler_failure(
        operation_key: Option<[u8; 32]>,
        request_identity: [u8; 32],
        capture: SemanticSourceCapture,
        base_workspace_root: [u8; 32],
        base_workspace_sequence: u64,
        source_workspace_root: [u8; 32],
        source_workspace_sequence: u64,
        source_commit: [u8; 32],
        outcome: ProductSemanticCaptureOutcome,
        compiler_failure: Option<backend_library::PackageCompilerFailure>,
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
        if compiler_failure.is_some()
            && !matches!(
                outcome,
                ProductSemanticCaptureOutcome::Unavailable { .. }
                    | ProductSemanticCaptureOutcome::Failed { .. }
            )
        {
            return Err("compiler failure is only valid on a terminal unavailable capture");
        }
        let compiler_failure_bytes = compiler_failure
            .as_ref()
            .map(|failure| {
                serde_json::to_vec(failure)
                    .ok()
                    .filter(|bytes| {
                        bytes.len() <= backend_library::PackageCompilerFailure::MAX_ENCODED_BYTES
                    })
                    .ok_or("compiler failure exceeds its canonical encoding bound")
            })
            .transpose()?
            .map(Vec::into_boxed_slice);
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
            compiler_failure,
            compiler_failure_bytes,
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

    /// Typed, bounded package compiler failure retained for this capture.
    #[must_use]
    pub fn compiler_failure(&self) -> Option<&backend_library::PackageCompilerFailure> {
        self.compiler_failure.as_ref()
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
        Self::new_with_compiler_failure(
            self.operation_key,
            self.request_identity,
            self.capture,
            self.base_workspace_root,
            self.base_workspace_sequence,
            self.source_workspace_root,
            self.source_workspace_sequence,
            self.source_commit,
            outcome,
            self.compiler_failure.clone(),
        )
    }

    /// Adds the compiler fault that explains an unavailable or failed
    /// semantic result while preserving the source-capture identity.
    ///
    /// # Errors
    /// Returns an error when the capture is pending/published or the typed
    /// payload exceeds its canonical byte bound.
    pub fn with_compiler_failure(
        &self,
        failure: backend_library::PackageCompilerFailure,
    ) -> Result<Self, &'static str> {
        Self::new_with_compiler_failure(
            self.operation_key,
            self.request_identity,
            self.capture,
            self.base_workspace_root,
            self.base_workspace_sequence,
            self.source_workspace_root,
            self.source_workspace_sequence,
            self.source_commit,
            self.outcome,
            Some(failure),
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
        match &value.compiler_failure_bytes {
            Some(bytes) => {
                output.push(1);
                let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                output.extend_from_slice(&length.to_be_bytes());
                output.extend_from_slice(bytes);
            }
            None => output.push(0),
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
        let (mut rest, has_failure_slot) = if let Some(rest) = bytes.strip_prefix(MAGIC) {
            (rest, true)
        } else if let Some(rest) = bytes.strip_prefix(LEGACY_MAGIC) {
            (rest, false)
        } else {
            return Err(RelationDecodeError::Malformed);
        };
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
        let (outcome, rest) = match tag {
            1 => {
                let (prior, rest) = decode_optional_prior(rest)?;
                (ProductSemanticCaptureOutcome::Pending { prior }, rest)
            }
            2 => {
                let (&reason, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
                (
                    ProductSemanticCaptureOutcome::Unavailable {
                        reason: decode_reason(reason)?,
                    },
                    rest,
                )
            }
            3 => {
                let (prior, rest) = decode_version(rest)?;
                let (&reason, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
                (
                    ProductSemanticCaptureOutcome::Failed {
                        prior,
                        reason: decode_reason(reason)?,
                    },
                    rest,
                )
            }
            4 => {
                let (coverage, rest) = decode_coverage(rest)?;
                let (claim_bytes, rest) = rest
                    .split_at_checked(super::semantic_relation::CLAIM_BYTES)
                    .ok_or(RelationDecodeError::Malformed)?;
                let claim = super::semantic_relation::decode_claim(claim_bytes)?;
                (
                    ProductSemanticCaptureOutcome::Published { coverage, claim },
                    rest,
                )
            }
            _ => return Err(RelationDecodeError::Malformed),
        };
        let compiler_failure = if has_failure_slot {
            let (&tag, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
            match tag {
                0 if rest.is_empty() => None,
                1 => {
                    let (length_bytes, rest) = rest
                        .split_at_checked(4)
                        .ok_or(RelationDecodeError::Malformed)?;
                    let length = u32::from_be_bytes(
                        length_bytes
                            .try_into()
                            .map_err(|_| RelationDecodeError::Malformed)?,
                    ) as usize;
                    if length > backend_library::PackageCompilerFailure::MAX_ENCODED_BYTES
                        || rest.len() != length
                    {
                        return Err(RelationDecodeError::Malformed);
                    }
                    let failure: backend_library::PackageCompilerFailure =
                        serde_json::from_slice(rest).map_err(|_| RelationDecodeError::Malformed)?;
                    let canonical =
                        serde_json::to_vec(&failure).map_err(|_| RelationDecodeError::Malformed)?;
                    if canonical.as_slice() != rest {
                        return Err(RelationDecodeError::Malformed);
                    }
                    Some(failure)
                }
                _ => return Err(RelationDecodeError::Malformed),
            }
        } else {
            if !rest.is_empty() {
                return Err(RelationDecodeError::Malformed);
            }
            None
        };
        ProductSemanticCaptureRecord::new_with_compiler_failure(
            operation_key,
            request_identity,
            capture,
            base_workspace_root,
            base_workspace_sequence,
            source_workspace_root,
            source_workspace_sequence,
            source_commit,
            outcome,
            compiler_failure,
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

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::interface::{CompilerFragmentFailure, SourceAuthority};
    use backend_semantic::ir::{BuildError, EntityId};
    use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};

    fn compiler_failure() -> backend_library::PackageCompilerFailure {
        let attempt = backend_library::CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source bytes"),
                byte_len: 12,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe bytes"),
        };
        let failure = CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
            owner: EntityId::new(7),
            start: 18,
            end: 24,
        });
        backend_library::PackageCompilerFailure::from_fragment_failure(
            "src/recovery.ts",
            attempt,
            &failure,
        )
        .expect("bounded typed compiler failure")
    }

    fn capture_record(
        failure: Option<backend_library::PackageCompilerFailure>,
    ) -> ProductSemanticCaptureRecord {
        let capture = SemanticSourceCapture::new(None, [5; 32], [6; 32], 1, 1)
            .expect("valid source capture");
        ProductSemanticCaptureRecord::new_with_compiler_failure(
            None,
            [1; 32],
            capture,
            [2; 32],
            0,
            [3; 32],
            1,
            [4; 32],
            ProductSemanticCaptureOutcome::Unavailable {
                reason: SemanticUnavailableReason::Rejected,
            },
            failure,
        )
        .expect("valid semantic capture record")
    }

    #[test]
    fn typed_compiler_refusal_is_canonical_and_source_bound_in_capture_row() {
        let failure = compiler_failure();
        let record = capture_record(Some(failure.clone()));
        let mut encoded = Vec::new();
        ProductSemanticCaptureRelation::encode_value(&record, &mut encoded);
        let decoded = ProductSemanticCaptureRelation::decode_value(&encoded)
            .expect("canonical typed capture row");
        assert_eq!(decoded.compiler_failure(), Some(&failure));

        let last = encoded.last_mut().expect("encoded failure payload");
        *last ^= 1;
        assert!(ProductSemanticCaptureRelation::decode_value(&encoded).is_err());
    }

    #[test]
    fn legacy_psc1_unavailable_rows_remain_readable() {
        let record = capture_record(None);
        let mut encoded = Vec::new();
        ProductSemanticCaptureRelation::encode_value(&record, &mut encoded);
        assert_eq!(encoded.get(..4), Some(MAGIC.as_slice()));
        encoded[..4].copy_from_slice(LEGACY_MAGIC);
        assert_eq!(encoded.pop(), Some(0));

        let decoded = ProductSemanticCaptureRelation::decode_value(&encoded)
            .expect("legacy PSC1 unavailable capture");
        assert_eq!(decoded.outcome(), record.outcome());
        assert!(decoded.compiler_failure().is_none());
    }
}
