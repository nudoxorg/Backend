//! Exact owner-side receipts for completed remote compiler-result retirement.
//!
//! The receipt is an owner audit record, not a worker signature. Its canonical
//! encoding is a fixed binary format so durable-key replay compares every
//! admitted field rather than a serializer's incidental representation.

use crate::surface::{ProductAdmissionError, SemanticLanguageProfile};
use backend_semantic::vocabulary::Stage;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

const RECEIPT_DOMAIN: &[u8] = b"backend.library.compiler-result-completion.v1\0";
const KEY_DOMAIN: &[u8] = b"backend.library.compiler-result-completion-key.v1\0";

/// Maximum canonical receipt size persisted by the completion authority.
pub const MAX_COMPILER_RESULT_COMPLETION_BYTES: usize = 1024;

/// Maximum remote result closure admitted by the owner-side ACK journal.
pub const MAX_COMPILER_RESULT_COMPLETION_OBJECTS: u32 = 100_002;

/// Maximum remote result closure bytes admitted by the owner-side ACK journal.
pub const MAX_COMPILER_RESULT_COMPLETION_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Domain-separated stable identity for one selected remote compiler result.
///
/// The identity binds the owner, namespace, exact scheduler/Turso attempt, and
/// worker closure. It intentionally excludes source and selected-head payload
/// fields: a retry that reuses this identity with changed payload must conflict
/// with the previously stored canonical receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompilerResultCompletionKey([u8; 32]);

impl CompilerResultCompletionKey {
    /// Returns the fixed-width domain-separated key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Exact assigned request and Turso candidate attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerResultAssignmentCommitment {
    /// Owner Turso namespace.
    #[serde(with = "hex_bytes")]
    pub namespace_id: [u8; 16],
    /// Stable full-workspace transfer request family.
    #[serde(with = "hex_bytes")]
    pub work_id: [u8; 16],
    /// Scheduler attempt ordinal.
    pub attempt: u64,
    /// Scheduler fence for this exact attempt.
    #[serde(with = "hex_bytes")]
    pub fence: [u8; 32],
    /// Turso-minted candidate attempt nonce.
    #[serde(with = "hex_bytes")]
    pub turso_attempt_id: [u8; 16],
    /// Monotonic Turso attempt epoch.
    pub turso_attempt_epoch: u64,
}

/// Authenticated identity and exact worker result-closure receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerWorkerResultCommitment {
    /// Owner endpoint which issued the Stored acknowledgement.
    #[serde(with = "hex_bytes")]
    pub owner_endpoint_id: [u8; 32],
    /// Authenticated worker endpoint identity.
    #[serde(with = "hex_bytes")]
    pub worker_peer_id: [u8; 32],
    /// Content-addressed digest of the complete worker result closure.
    #[serde(with = "hex_bytes")]
    pub result_closure_id: [u8; 32],
    /// Objects in the authenticated worker result closure.
    pub object_count: u32,
    /// Exact payload bytes reported for that closure.
    pub payload_bytes: u64,
    /// Bytes reopened and verified by the owner.
    pub bytes_verified: u64,
}

/// Admitted full-workspace source, input, and compiler-profile commitments.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerResultInputCommitment {
    /// Exact package lineage in the admitted capture.
    #[serde(with = "hex_bytes")]
    pub package_lineage: [u8; 32],
    /// Compiler package target admitted for the request.
    #[serde(with = "hex_bytes")]
    pub target: [u8; 32],
    /// Compiler recipe commitment.
    #[serde(with = "hex_bytes")]
    pub recipe: [u8; 32],
    /// Exact source-observation and compiler input root.
    #[serde(with = "hex_bytes")]
    pub input_root: [u8; 32],
    /// Exact read-manifest commitment for the transferred workspace.
    #[serde(with = "hex_bytes")]
    pub read_manifest: [u8; 32],
    /// Identity of the captured workspace snapshot.
    #[serde(with = "hex_bytes")]
    pub workspace_snapshot_id: [u8; 32],
    /// Full-workspace input closure digest.
    #[serde(with = "hex_bytes")]
    pub input_closure_id: [u8; 32],
    /// Typed input manifest object digest.
    #[serde(with = "hex_bytes")]
    pub manifest_object_id: [u8; 32],
    /// Source scanner fence checked before semantic selection.
    #[serde(with = "hex_bytes")]
    pub source_fence_digest: [u8; 32],
    /// Closed language and dialect profile used for this compilation.
    pub profile: SemanticLanguageProfile,
    /// Compiler stage admitted by the trust grant.
    pub stage: u8,
    /// Exact compiler toolchain identity.
    #[serde(with = "hex_bytes")]
    pub toolchain: [u8; 32],
    /// Exact compiler environment identity.
    #[serde(with = "hex_bytes")]
    pub environment: [u8; 32],
    /// Exact target-platform identity.
    #[serde(with = "hex_bytes")]
    pub target_platform: [u8; 32],
}

/// Exact Turso selected head and semantic binding reopened from that head.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerResultPublishedHeadCommitment {
    /// Monotonic selected generation in the exact Turso namespace.
    pub turso_generation: u64,
    /// Exact candidate identity in selected-generation history.
    #[serde(with = "hex_bytes")]
    pub candidate_id: [u8; 32],
    /// Exact selected logical target root.
    #[serde(with = "hex_bytes")]
    pub target_root: [u8; 32],
    /// Exact owner-selected closure containing the admitted semantic output.
    #[serde(with = "hex_bytes")]
    pub selected_closure_id: [u8; 32],
    /// Aggregate root of the selected semantic-plane manifests, if present.
    #[serde(with = "optional_hex_bytes")]
    pub semantic_plane_manifest_root: Option<[u8; 32]>,
    /// Source/input digest named by the selected generation.
    #[serde(with = "hex_bytes")]
    pub input_digest: [u8; 32],
    /// Immutable semantic generation identity from the reopened binding.
    #[serde(with = "hex_bytes")]
    pub semantic_generation: [u8; 32],
    /// Generation root from the reopened semantic binding.
    #[serde(with = "hex_bytes")]
    pub generation_root: [u8; 32],
    /// Dependency-set commitment from the reopened semantic binding.
    #[serde(with = "hex_bytes")]
    pub dependency_set: [u8; 32],
    /// Manifest commitment from the reopened semantic binding.
    #[serde(with = "hex_bytes")]
    pub manifest: [u8; 32],
}

/// Owner completion payload for one selected remote compiler result.
///
/// The local ACK path derives this value from the sealed journal row and the
/// exact selected-history proof. The public fields and serde representation
/// make it data, not a capability: durable admission rejoins selected
/// authority, and journal deletion additionally requires the private
/// network-applied receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerResultCompletionReceipt {
    /// Exact assigned request and Turso candidate attempt.
    pub assignment: CompilerResultAssignmentCommitment,
    /// Exact owner-private pending ACK row that authorized this completion.
    /// This is compared payload, not assignment identity; replay with a
    /// changed row conflicts under the same completion key.
    #[serde(with = "hex_bytes")]
    pub pending_journal_id: [u8; 32],
    /// Exact authenticated worker result closure.
    pub worker: CompilerWorkerResultCommitment,
    /// Exact captured source and compiler input admission.
    pub input: CompilerResultInputCommitment,
    /// Reopened immutable semantic publication selected by the owner.
    pub published: CompilerResultPublishedHeadCommitment,
}

impl CompilerResultCompletionReceipt {
    /// Validates fixed-shape commitments without deciding whether a selected
    /// semantic version matches them.
    pub fn validate_shape(&self) -> Result<(), ProductAdmissionError> {
        let assignment = self.assignment;
        let worker = self.worker;
        let input = self.input;
        let published = self.published;
        if assignment.namespace_id == [0; 16]
            || assignment.work_id == [0; 16]
            || assignment.attempt == 0
            || assignment.attempt > i64::MAX as u64
            || assignment.fence == [0; 32]
            || assignment.turso_attempt_id == [0; 16]
            || assignment.turso_attempt_epoch == 0
            || assignment.turso_attempt_epoch > i64::MAX as u64
            || assignment.attempt != assignment.turso_attempt_epoch
            || worker.owner_endpoint_id == [0; 32]
            || worker.worker_peer_id == [0; 32]
            || worker.worker_peer_id == worker.owner_endpoint_id
            || self.pending_journal_id == [0; 32]
            || worker.result_closure_id == [0; 32]
            || worker.object_count == 0
            || worker.object_count > MAX_COMPILER_RESULT_COMPLETION_OBJECTS
            || worker.payload_bytes == 0
            || worker.payload_bytes > MAX_COMPILER_RESULT_COMPLETION_PAYLOAD_BYTES
            || worker.bytes_verified < worker.payload_bytes
            || worker.bytes_verified > MAX_COMPILER_RESULT_COMPLETION_PAYLOAD_BYTES
            || input.package_lineage == [0; 32]
            || input.target == [0; 32]
            || input.recipe == [0; 32]
            || input.input_root == [0; 32]
            || input.read_manifest == [0; 32]
            || input.workspace_snapshot_id == [0; 32]
            || input.input_closure_id == [0; 32]
            || input.manifest_object_id == [0; 32]
            || input.source_fence_digest == [0; 32]
            || input.profile.profile().is_err()
            || Stage::try_from(input.stage).map_or(true, |stage| stage != Stage::LowerIr)
            || input.toolchain == [0; 32]
            || input.environment == [0; 32]
            || input.target_platform == [0; 32]
            || published.turso_generation == 0
            || published.turso_generation > i64::MAX as u64
            || published.candidate_id == [0; 32]
            || published.target_root == [0; 32]
            || published.selected_closure_id == [0; 32]
            || published.input_digest != input.input_root
            || published.semantic_generation == [0; 32]
            || published.generation_root == [0; 32]
            || published.dependency_set == [0; 32]
            || published.manifest == [0; 32]
            || published
                .semantic_plane_manifest_root
                .is_some_and(|digest| digest == [0; 32])
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }

    /// Stable key for the exact assigned remote result, independent of its
    /// mutable selected-head and input payload.
    #[must_use]
    pub fn completion_key(&self) -> CompilerResultCompletionKey {
        let mut hasher = blake3::Hasher::new();
        hasher.update(KEY_DOMAIN);
        hasher.update(&self.assignment.namespace_id);
        hasher.update(&self.assignment.work_id);
        hasher.update(&self.assignment.attempt.to_be_bytes());
        hasher.update(&self.assignment.fence);
        hasher.update(&self.assignment.turso_attempt_id);
        hasher.update(&self.assignment.turso_attempt_epoch.to_be_bytes());
        hasher.update(&self.worker.owner_endpoint_id);
        hasher.update(&self.worker.worker_peer_id);
        hasher.update(&self.worker.result_closure_id);
        CompilerResultCompletionKey(*hasher.finalize().as_bytes())
    }

    /// Canonical receipt bytes used by durable replay comparison.
    ///
    /// Byte format: `RECEIPT_DOMAIN`; assignment namespace, work, scheduler
    /// attempt, scheduler fence, Turso attempt ID, and Turso epoch; exact owner
    /// pending-journal identity; worker owner endpoint, peer, result closure,
    /// object count, payload bytes, and verified bytes; input package lineage,
    /// target, recipe, input root, read manifest, workspace snapshot, input
    /// closure, manifest object, source fence, two-byte language profile,
    /// stage, toolchain, environment, and target platform; then selected
    /// generation, candidate, target root,
    /// selected closure, optional plane-root tag/value, input digest, semantic
    /// generation, generation root, dependency set, and manifest. Integer
    /// widths are those of the corresponding fields and all are unsigned
    /// big-endian; identifiers and digests are raw bytes. The optional root
    /// is one `0`/`1` tag followed by 32 bytes for `Some`. The receipt key uses
    /// `KEY_DOMAIN` and only the identity fields documented on
    /// [`CompilerResultCompletionKey`].
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(1024);
        bytes.extend_from_slice(RECEIPT_DOMAIN);
        let assignment = self.assignment;
        bytes.extend_from_slice(&assignment.namespace_id);
        bytes.extend_from_slice(&assignment.work_id);
        bytes.extend_from_slice(&assignment.attempt.to_be_bytes());
        bytes.extend_from_slice(&assignment.fence);
        bytes.extend_from_slice(&assignment.turso_attempt_id);
        bytes.extend_from_slice(&assignment.turso_attempt_epoch.to_be_bytes());
        bytes.extend_from_slice(&self.pending_journal_id);

        let worker = self.worker;
        bytes.extend_from_slice(&worker.owner_endpoint_id);
        bytes.extend_from_slice(&worker.worker_peer_id);
        bytes.extend_from_slice(&worker.result_closure_id);
        bytes.extend_from_slice(&worker.object_count.to_be_bytes());
        bytes.extend_from_slice(&worker.payload_bytes.to_be_bytes());
        bytes.extend_from_slice(&worker.bytes_verified.to_be_bytes());

        let input = self.input;
        bytes.extend_from_slice(&input.package_lineage);
        bytes.extend_from_slice(&input.target);
        bytes.extend_from_slice(&input.recipe);
        bytes.extend_from_slice(&input.input_root);
        bytes.extend_from_slice(&input.read_manifest);
        bytes.extend_from_slice(&input.workspace_snapshot_id);
        bytes.extend_from_slice(&input.input_closure_id);
        bytes.extend_from_slice(&input.manifest_object_id);
        bytes.extend_from_slice(&input.source_fence_digest);
        bytes.extend_from_slice(&input.profile.to_bytes());
        bytes.push(input.stage);
        bytes.extend_from_slice(&input.toolchain);
        bytes.extend_from_slice(&input.environment);
        bytes.extend_from_slice(&input.target_platform);

        let published = self.published;
        bytes.extend_from_slice(&published.turso_generation.to_be_bytes());
        bytes.extend_from_slice(&published.candidate_id);
        bytes.extend_from_slice(&published.target_root);
        bytes.extend_from_slice(&published.selected_closure_id);
        match published.semantic_plane_manifest_root {
            Some(root) => {
                bytes.push(1);
                bytes.extend_from_slice(&root);
            }
            None => bytes.push(0),
        }
        bytes.extend_from_slice(&published.input_digest);
        bytes.extend_from_slice(&published.semantic_generation);
        bytes.extend_from_slice(&published.generation_root);
        bytes.extend_from_slice(&published.dependency_set);
        bytes.extend_from_slice(&published.manifest);
        bytes
    }

    /// Reopens one exact canonical receipt, rejecting extra bytes and
    /// noncanonical discriminators before a persisted receipt is trusted.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ProductAdmissionError> {
        if bytes.len() > MAX_COMPILER_RESULT_COMPLETION_BYTES {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let mut cursor = CanonicalCursor::new(bytes);
        if cursor.take(RECEIPT_DOMAIN.len())? != RECEIPT_DOMAIN {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let assignment = CompilerResultAssignmentCommitment {
            namespace_id: cursor.array()?,
            work_id: cursor.array()?,
            attempt: cursor.u64()?,
            fence: cursor.array()?,
            turso_attempt_id: cursor.array()?,
            turso_attempt_epoch: cursor.u64()?,
        };
        let pending_journal_id = cursor.array()?;
        let worker = CompilerWorkerResultCommitment {
            owner_endpoint_id: cursor.array()?,
            worker_peer_id: cursor.array()?,
            result_closure_id: cursor.array()?,
            object_count: cursor.u32()?,
            payload_bytes: cursor.u64()?,
            bytes_verified: cursor.u64()?,
        };
        let profile_bytes = cursor.array::<2>()?;
        let profile = backend_semantic::vocabulary::LanguageProfile::try_from(profile_bytes)
            .map(SemanticLanguageProfile::new)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        let input = CompilerResultInputCommitment {
            package_lineage: cursor.array()?,
            target: cursor.array()?,
            recipe: cursor.array()?,
            input_root: cursor.array()?,
            read_manifest: cursor.array()?,
            workspace_snapshot_id: cursor.array()?,
            input_closure_id: cursor.array()?,
            manifest_object_id: cursor.array()?,
            source_fence_digest: cursor.array()?,
            profile,
            stage: cursor.u8()?,
            toolchain: cursor.array()?,
            environment: cursor.array()?,
            target_platform: cursor.array()?,
        };
        let turso_generation = cursor.u64()?;
        let candidate_id = cursor.array()?;
        let target_root = cursor.array()?;
        let selected_closure_id = cursor.array()?;
        let semantic_plane_manifest_root = match cursor.u8()? {
            0 => None,
            1 => Some(cursor.array()?),
            _ => return Err(ProductAdmissionError::SemanticVersionShape),
        };
        let published = CompilerResultPublishedHeadCommitment {
            turso_generation,
            candidate_id,
            target_root,
            selected_closure_id,
            semantic_plane_manifest_root,
            input_digest: cursor.array()?,
            semantic_generation: cursor.array()?,
            generation_root: cursor.array()?,
            dependency_set: cursor.array()?,
            manifest: cursor.array()?,
        };
        cursor.finish()?;
        let receipt = Self {
            assignment,
            pending_journal_id,
            worker,
            input,
            published,
        };
        receipt.validate_shape()?;
        if receipt.canonical_bytes().as_slice() != bytes {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(receipt)
    }
}

struct CanonicalCursor<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> CanonicalCursor<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], ProductAdmissionError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ProductAdmissionError::SemanticVersionShape)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProductAdmissionError::SemanticVersionShape)?;
        self.offset = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProductAdmissionError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)
    }

    fn u8(&mut self) -> Result<u8, ProductAdmissionError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, ProductAdmissionError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, ProductAdmissionError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn finish(self) -> Result<(), ProductAdmissionError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ProductAdmissionError::SemanticVersionShape)
        }
    }
}

// One generic lower-case, fixed-width hex codec is used for every byte-array
// length in this contract. The wrappers only provide serde's field-level path.
mod hex_bytes {
    use super::{Deserializer, Serializer, de};
    use std::fmt;

    pub fn serialize<S, const N: usize>(bytes: &[u8; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut text = String::with_capacity(N * 2);
        for byte in bytes {
            use fmt::Write as _;
            write!(&mut text, "{byte:02x}").map_err(serde::ser::Error::custom)?;
        }
        serializer.serialize_str(&text)
    }

    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<[u8; N], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HexVisitor<const N: usize>;

        impl<const N: usize> de::Visitor<'_> for HexVisitor<N> {
            type Value = [u8; N];

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{} lowercase hexadecimal characters", N * 2)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() != N * 2 {
                    return Err(E::custom("fixed hexadecimal byte length"));
                }
                let mut output = [0_u8; N];
                for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
                    let high = lower_nibble(pair[0])
                        .ok_or_else(|| E::custom("fixed hexadecimal bytes must be lowercase"))?;
                    let low = lower_nibble(pair[1])
                        .ok_or_else(|| E::custom("fixed hexadecimal bytes must be lowercase"))?;
                    output[index] = (high << 4) | low;
                }
                Ok(output)
            }
        }

        deserializer.deserialize_str(HexVisitor::<N>)
    }

    fn lower_nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
}

mod optional_hex_bytes {
    use super::{Deserializer, Serializer, de, hex_bytes};

    pub fn serialize<S, const N: usize>(
        value: &Option<[u8; N]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(bytes) => serializer.serialize_some(&HexBytes(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<Option<[u8; N]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OptionVisitor<const N: usize>;

        impl<'de, const N: usize> de::Visitor<'de> for OptionVisitor<N> {
            type Value = Option<[u8; N]>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "an optional {}-byte lowercase hex string", N)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(None)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(None)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                hex_bytes::deserialize::<D, N>(deserializer).map(Some)
            }
        }

        deserializer.deserialize_option(OptionVisitor::<N>)
    }

    struct HexBytes<'a, const N: usize>(&'a [u8; N]);

    impl<const N: usize> Serialize for HexBytes<'_, N> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            hex_bytes::serialize(self.0, serializer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};

    fn receipt() -> CompilerResultCompletionReceipt {
        CompilerResultCompletionReceipt {
            assignment: CompilerResultAssignmentCommitment {
                namespace_id: [1; 16],
                work_id: [2; 16],
                attempt: 3,
                fence: [4; 32],
                turso_attempt_id: [5; 16],
                turso_attempt_epoch: 3,
            },
            pending_journal_id: [33; 32],
            worker: CompilerWorkerResultCommitment {
                owner_endpoint_id: [6; 32],
                worker_peer_id: [7; 32],
                result_closure_id: [8; 32],
                object_count: 9,
                payload_bytes: 10,
                bytes_verified: 11,
            },
            input: CompilerResultInputCommitment {
                package_lineage: [12; 32],
                target: [13; 32],
                recipe: [14; 32],
                input_root: [15; 32],
                read_manifest: [16; 32],
                workspace_snapshot_id: [17; 32],
                input_closure_id: [18; 32],
                manifest_object_id: [19; 32],
                source_fence_digest: [20; 32],
                profile: SemanticLanguageProfile::new(LanguageProfile::Rust(RustEdition::Rust2024)),
                stage: u8::from(Stage::LowerIr),
                toolchain: [21; 32],
                environment: [22; 32],
                target_platform: [23; 32],
            },
            published: CompilerResultPublishedHeadCommitment {
                turso_generation: 24,
                candidate_id: [25; 32],
                target_root: [26; 32],
                selected_closure_id: [27; 32],
                semantic_plane_manifest_root: Some([28; 32]),
                input_digest: [15; 32],
                semantic_generation: [29; 32],
                generation_root: [30; 32],
                dependency_set: [31; 32],
                manifest: [32; 32],
            },
        }
    }

    #[test]
    fn canonical_receipt_is_complete_and_key_is_stable_across_payload_changes() {
        let original = receipt();
        original.validate_shape().expect("valid receipt shape");
        let round_trip: CompilerResultCompletionReceipt =
            serde_json::from_slice(&serde_json::to_vec(&original).expect("serialize receipt"))
                .expect("strict receipt roundtrip");
        assert_eq!(round_trip, original);
        assert_eq!(round_trip.canonical_bytes(), original.canonical_bytes());
        assert_eq!(
            CompilerResultCompletionReceipt::from_canonical_bytes(&original.canonical_bytes())
                .expect("canonical byte roundtrip"),
            original
        );

        let key = original.completion_key();
        let mut changed = original;
        changed.input.read_manifest[0] ^= 1;
        assert_eq!(changed.completion_key(), key);
        assert_ne!(changed.canonical_bytes(), original.canonical_bytes());

        changed = original;
        changed.pending_journal_id[0] ^= 1;
        assert_eq!(changed.completion_key(), key);
        assert_ne!(changed.canonical_bytes(), original.canonical_bytes());

        changed = original;
        changed.worker.owner_endpoint_id[0] ^= 1;
        assert_ne!(changed.completion_key(), key);
    }

    #[test]
    fn canonical_receipt_option_tag_and_generation_are_committed() {
        let some = receipt();
        let mut none = some;
        none.published.semantic_plane_manifest_root = None;
        assert_ne!(some.canonical_bytes(), none.canonical_bytes());
        assert_eq!(some.completion_key(), none.completion_key());

        let mut missing_journal_identity = some;
        missing_journal_identity.pending_journal_id = [0; 32];
        assert!(missing_journal_identity.validate_shape().is_err());
        assert!(
            CompilerResultCompletionReceipt::from_canonical_bytes(
                &missing_journal_identity.canonical_bytes()
            )
            .is_err()
        );

        let mut different_generation = some;
        different_generation.published.turso_generation += 1;
        assert_eq!(some.completion_key(), different_generation.completion_key());
        assert_ne!(
            some.canonical_bytes(),
            different_generation.canonical_bytes()
        );
    }

    #[test]
    fn canonical_receipt_and_identity_match_independent_golden_vectors() {
        let value = receipt();
        let bytes = value.canonical_bytes();
        assert_eq!(bytes.len(), 974);
        assert_eq!(
            blake3::hash(&bytes).to_hex().as_str(),
            "53c3c10221ddd64c34ea383b97b334277834cb460ecb9bff2786c4b843aba0f6"
        );
        assert_eq!(
            blake3::hash(&bytes).to_hex().as_str(),
            "e3af80d9587c166e8e77e570a8d52d60084d7de67de990cfe5b60ca2c66dae6c"
        );
        assert_eq!(
            value.completion_key().as_bytes(),
            &hex_bytes_for_test::<32>(
                "ad8ea575478df97d7a2892c90d590a78615a75afa62ff14d4fab2d1efb65b6bc",
            )
        );
    }

    #[test]
    fn canonical_reopen_rejects_truncation_extra_bytes_and_unknown_option_tags() {
        let value = receipt();
        let bytes = value.canonical_bytes();
        assert!(
            CompilerResultCompletionReceipt::from_canonical_bytes(&bytes[..bytes.len() - 1])
                .is_err()
        );
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(CompilerResultCompletionReceipt::from_canonical_bytes(&extra).is_err());
        let option_offset = RECEIPT_DOMAIN.len() + 96 + 32 + 116 + 387 + 8 + 96;
        let mut unknown_option = bytes;
        unknown_option[option_offset] = 2;
        assert!(CompilerResultCompletionReceipt::from_canonical_bytes(&unknown_option).is_err());
    }

    fn hex_bytes_for_test<const N: usize>(hex: &str) -> [u8; N] {
        assert_eq!(hex.len(), N * 2);
        let mut bytes = [0; N];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let high = lower_nibble_for_test(pair[0]);
            let low = lower_nibble_for_test(pair[1]);
            bytes[index] = (high << 4) | low;
        }
        bytes
    }

    fn lower_nibble_for_test(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("invalid hard-coded golden vector"),
        }
    }

    #[test]
    fn digest_hex_codec_is_fixed_width_lowercase() {
        let value = receipt();
        let encoded = serde_json::to_value(value).expect("serialize receipt");
        assert_eq!(encoded["assignment"]["namespace_id"], "01".repeat(16));
        let upper = serde_json::to_string(&encoded)
            .expect("serialize value")
            .replace("0101", "0A01");
        assert!(serde_json::from_str::<CompilerResultCompletionReceipt>(&upper).is_err());
    }
}
