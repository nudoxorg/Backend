//! Bounded frozen metadata for a native, immutable semantic source reference.

use super::{SemanticSourceDocument, SemanticSourceError, SemanticSourceIdentity};
use crate::{
    PackageCoordinate, PackageReference, SelectedProjectSourceFrontier, SemanticGenerationId,
    SemanticHistoryImagePublicationProof, SemanticHistoryInputReplayStatus,
    SemanticHistoryPublicationProof, SemanticHistoryPublicationStatus,
    SemanticHistorySelectionStamp, SemanticLanguageProfile, SemanticVersionFreshness,
    SemanticVersionRecord,
};
use serde::{Deserialize, Serialize};

/// Exact compiler and query-time source metadata, without an unbounded history
/// image catalog. These are frozen observations, never re-read mutable labels.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceMetadata {
    /// Exact product package that owns the captured generation.
    pub package: PackageReference,
    /// Full compiler package coordinate, including its captured version.
    pub coordinate: PackageCoordinate,
    /// Closed source language and dialect profile.
    pub profile: SemanticLanguageProfile,
    /// Exact immutable compiler binding identity.
    pub generation: SemanticGenerationId,
    /// Verified compiler generation root.
    pub generation_root: [u8; 32],
    /// Verified dependency-set commitment.
    pub dependency_set: [u8; 32],
    /// Exact semantic manifest commitment.
    pub manifest: [u8; 32],
    /// Complete native image count committed by the manifest.
    pub artifacts: u32,
    /// Canonical compiler manifest extent, not image payload bytes.
    pub semantic_bytes: u32,
    /// Compiler coverage of the complete declared scope.
    pub complete: bool,
    /// Selection observed when this source was captured.
    pub selected: bool,
    /// Frozen source-input observation, independent of current selection.
    pub freshness: SemanticVersionFreshness,
    /// Exact Project membership frontier observed for this source.
    pub selected_source_frontier: Option<SelectedProjectSourceFrontier>,
}
impl From<&SemanticVersionRecord> for SemanticSourceMetadata {
    fn from(record: &SemanticVersionRecord) -> Self {
        Self {
            package: record.package.clone(),
            coordinate: record.coordinate.clone(),
            profile: record.profile,
            generation: record.generation,
            generation_root: record.generation_root,
            dependency_set: record.dependency_set,
            manifest: record.manifest,
            artifacts: record.artifacts,
            semantic_bytes: record.semantic_bytes,
            complete: record.complete,
            selected: record.selected,
            freshness: record.freshness,
            selected_source_frontier: record.selected_source_frontier.clone(),
        }
    }
}
impl SemanticSourceMetadata {
    fn record(&self, history_status: SemanticHistoryPublicationStatus) -> SemanticVersionRecord {
        SemanticVersionRecord {
            package: self.package.clone(),
            coordinate: self.coordinate.clone(),
            profile: self.profile,
            generation: self.generation,
            generation_root: self.generation_root,
            dependency_set: self.dependency_set,
            manifest: self.manifest,
            artifacts: self.artifacts,
            semantic_bytes: self.semantic_bytes,
            complete: self.complete,
            selected: self.selected,
            freshness: self.freshness,
            history_status,
            selected_source_frontier: self.selected_source_frontier.clone(),
        }
    }
}

/// Complete bounded header of a published proof. The immutable V3 tip owns the
/// omitted catalog; `image_count` is its exact whole extent, not a preview count.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistoryProofReference {
    /// Exact native owner selection stamp that published this proof.
    pub selection: SemanticHistorySelectionStamp,
    /// Exact native history target package label.
    pub target_package: String,
    /// Exact native history target coordinate.
    pub target_coordinate: String,
    /// Commitment of the complete ordered image set and selection.
    pub package_identity: [u8; 32],
    /// Exact count of all image proofs in the immutable catalog.
    pub image_count: u32,
    /// Immutable V3 tip retained for this source.
    pub reference_tip: [u8; 32],
    /// Last image commit admitted at that exact tip.
    pub reachable_commit: [u8; 32],
    /// Persisted input claim authority; no compiler read authority is recreated.
    pub input_replay_status: SemanticHistoryInputReplayStatus,
}
impl SemanticHistoryProofReference {
    fn capture(proof: &SemanticHistoryPublicationProof) -> Result<Self, SemanticSourceError> {
        Ok(Self {
            selection: proof.selection.clone(),
            target_package: proof.target_package.clone(),
            target_coordinate: proof.target_coordinate.clone(),
            package_identity: proof.package_identity,
            image_count: u32::try_from(proof.images.len())
                .map_err(|_| SemanticSourceError::Range)?,
            reference_tip: proof.reference_tip,
            reachable_commit: proof.reachable_commit,
            input_replay_status: proof.input_replay_status,
        })
    }
    fn reconstruct(
        &self,
        images: Box<[SemanticHistoryImagePublicationProof]>,
    ) -> Result<SemanticHistoryPublicationProof, SemanticSourceError> {
        if u32::try_from(images.len()).ok() != Some(self.image_count) {
            return Err(SemanticSourceError::Identity);
        }
        let proof = SemanticHistoryPublicationProof {
            selection: self.selection.clone(),
            target_package: self.target_package.clone(),
            target_coordinate: self.target_coordinate.clone(),
            package_identity: self.package_identity,
            images,
            reference_tip: self.reference_tip,
            reachable_commit: self.reachable_commit,
            input_replay_status: self.input_replay_status,
        };
        if proof.recompute_package_identity() != self.package_identity {
            return Err(SemanticSourceError::Digest);
        }
        Ok(proof)
    }
}

/// Frozen history availability and full immutable proof locator. No omitted
/// image list is represented as an empty or complete source object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticSourceHistory {
    /// This version was not the committed selected generation.
    NotSelected,
    /// Derived history had not been requested.
    NotRequested {
        /// Exact selection identity whose history observation is frozen.
        selection_id: [u8; 32],
    },
    /// Derived history was being produced.
    Pending {
        /// Exact selection identity whose history observation is frozen.
        selection_id: [u8; 32],
    },
    /// History publication was waiting for a worker slot.
    Deferred {
        /// Exact selection identity whose history observation is frozen.
        selection_id: [u8; 32],
        /// Frozen owner diagnostic, subject to the existing history detail bound.
        reason: String,
    },
    /// The complete ordered native image catalog was durably published.
    Published {
        /// Exact selection identity whose history observation is frozen.
        selection_id: [u8; 32],
        /// Immutable published history commit.
        commit: [u8; 32],
        /// Native branch name observed when the source was captured.
        reference: String,
        /// Complete bounded proof locator; the image catalog is retrieved natively.
        proof: SemanticHistoryProofReference,
    },
    /// Derived history publication was refused.
    Refused {
        /// Exact selection identity whose history observation is frozen.
        selection_id: [u8; 32],
        /// Frozen owner diagnostic, subject to the existing history detail bound.
        reason: String,
    },
    /// The publication marker advanced during derived history work.
    Superseded {
        /// Exact selection identity whose history observation is frozen.
        selection_id: [u8; 32],
    },
}
impl SemanticSourceHistory {
    fn capture(status: &SemanticHistoryPublicationStatus) -> Result<Self, SemanticSourceError> {
        Ok(match status {
            SemanticHistoryPublicationStatus::NotSelected => Self::NotSelected,
            SemanticHistoryPublicationStatus::NotRequested { selection_id } => Self::NotRequested {
                selection_id: *selection_id,
            },
            SemanticHistoryPublicationStatus::Pending { selection_id } => Self::Pending {
                selection_id: *selection_id,
            },
            SemanticHistoryPublicationStatus::Deferred {
                selection_id,
                reason,
            } => Self::Deferred {
                selection_id: *selection_id,
                reason: reason.clone(),
            },
            SemanticHistoryPublicationStatus::Published {
                selection_id,
                commit,
                reference,
                proof,
            } => Self::Published {
                selection_id: *selection_id,
                commit: *commit,
                reference: reference.clone(),
                proof: SemanticHistoryProofReference::capture(proof)?,
            },
            SemanticHistoryPublicationStatus::Refused {
                selection_id,
                reason,
            } => Self::Refused {
                selection_id: *selection_id,
                reason: reason.clone(),
            },
            SemanticHistoryPublicationStatus::Superseded { selection_id } => Self::Superseded {
                selection_id: *selection_id,
            },
        })
    }
    fn reconstruct(
        &self,
        images: Box<[SemanticHistoryImagePublicationProof]>,
    ) -> Result<SemanticHistoryPublicationStatus, SemanticSourceError> {
        if !matches!(self, Self::Published { .. }) && !images.is_empty() {
            return Err(SemanticSourceError::Identity);
        }
        Ok(match self {
            Self::NotSelected => SemanticHistoryPublicationStatus::NotSelected,
            Self::NotRequested { selection_id } => SemanticHistoryPublicationStatus::NotRequested {
                selection_id: *selection_id,
            },
            Self::Pending { selection_id } => SemanticHistoryPublicationStatus::Pending {
                selection_id: *selection_id,
            },
            Self::Deferred {
                selection_id,
                reason,
            } => SemanticHistoryPublicationStatus::Deferred {
                selection_id: *selection_id,
                reason: reason.clone(),
            },
            Self::Published {
                selection_id,
                commit,
                reference,
                proof,
            } => SemanticHistoryPublicationStatus::Published {
                selection_id: *selection_id,
                commit: *commit,
                reference: reference.clone(),
                proof: proof.reconstruct(images)?,
            },
            Self::Refused {
                selection_id,
                reason,
            } => SemanticHistoryPublicationStatus::Refused {
                selection_id: *selection_id,
                reason: reason.clone(),
            },
            Self::Superseded { selection_id } => SemanticHistoryPublicationStatus::Superseded {
                selection_id: *selection_id,
            },
        })
    }
}

/// Portable, restart-usable native source reference. The durable owner seal
/// authenticates the exact captured metadata and roots; the content version
/// admits complete source bytes independently of the readable descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceReference {
    /// Canonical full-record identity and complete byte extent.
    pub source: SemanticSourceIdentity,
    /// Native source relation root captured with this observation.
    pub source_relation_root: [u8; 32],
    /// Native compiler publication relation root captured with this observation.
    pub semantic_relation_root: [u8; 32],
    /// Complete bounded compiler identities and frozen source observations.
    pub metadata: SemanticSourceMetadata,
    /// Frozen history availability and exact immutable proof locator.
    pub history: SemanticSourceHistory,
    /// Domain-separated native owner statement over the complete reference.
    pub seal: [u8; 32],
}

/// Scoped native-owner admission of a portable source reference. Only owner
/// seal verification constructs this capability; raw parsed metadata grants
/// no source lookup, root authority, retention, or shape proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedSemanticSourceReference<'a> {
    reference: &'a SemanticSourceReference,
}
#[derive(Serialize)]
struct SourceOwnerStatement<'a>(
    &'a SemanticSourceIdentity,
    [u8; 32],
    [u8; 32],
    &'a SemanticSourceMetadata,
    &'a SemanticSourceHistory,
);
impl crate::NativeOwnerStatement for SourceOwnerStatement<'_> {
    const DOMAIN: &'static [u8] = b"backend.semantic.source-reference.v1\0";
}
impl SemanticSourceReference {
    /// Captures one complete admitted record with an already selected native
    /// root pair. The caller must retain the exact V3 proof tip before returning
    /// a published reference; sealing alone creates no retention or shape proof.
    pub fn capture(
        record: &SemanticVersionRecord,
        source_relation_root: [u8; 32],
        semantic_relation_root: [u8; 32],
        authority: &[u8; 32],
    ) -> Result<Self, SemanticSourceError> {
        if source_relation_root == [0; 32] || semantic_relation_root == [0; 32] {
            return Err(SemanticSourceError::Identity);
        }
        let source = SemanticSourceIdentity::from_record(record)?;
        let mut reference = Self {
            source,
            source_relation_root,
            semantic_relation_root,
            metadata: record.into(),
            history: SemanticSourceHistory::capture(&record.history_status)?,
            seal: [0; 32],
        };
        reference.seal = crate::seal_native_owner_statement(authority, &reference.statement())
            .map_err(|_| SemanticSourceError::Record)?;
        Ok(reference)
    }
    fn statement(&self) -> SourceOwnerStatement<'_> {
        SourceOwnerStatement(
            &self.source,
            self.source_relation_root,
            self.semantic_relation_root,
            &self.metadata,
            &self.history,
        )
    }
    /// Re-admits the portable reference under the same durable native owner.
    pub fn verify_owner(
        &self,
        authority: &[u8; 32],
    ) -> Result<VerifiedSemanticSourceReference<'_>, SemanticSourceError> {
        crate::verify_native_owner_statement(authority, &self.statement(), &self.seal)
            .map_err(|_| SemanticSourceError::Identity)?;
        Ok(VerifiedSemanticSourceReference { reference: self })
    }
}
impl VerifiedSemanticSourceReference<'_> {
    /// Borrows the exact owner-authenticated metadata, roots, and byte identity.
    pub const fn reference(&self) -> &SemanticSourceReference {
        self.reference
    }
    /// Reconstructs the source from its unchanged header and an independently
    /// admitted complete V3 proof catalog. No mutable metadata is re-observed.
    pub fn reconstruct(
        &self,
        images: Box<[SemanticHistoryImagePublicationProof]>,
    ) -> Result<SemanticSourceDocument, SemanticSourceError> {
        let reference = self.reference;
        let record = reference
            .metadata
            .record(reference.history.reconstruct(images)?);
        let document = SemanticSourceDocument::new(&record)?;
        if document.source() != &reference.source {
            return Err(SemanticSourceError::Digest);
        }
        Ok(document)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn capture() -> SemanticVersionRecord {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../present/fixtures/semantic-versions-semver-public.json"
        ))
        .expect("untouched complete real capture");
        serde_json::from_value(packet["surface"]["data"][0].clone()).expect("source DTO")
    }

    #[test]
    fn exact_owner_reference_survives_roundtrip_and_metadata_cannot_rebind_it() {
        let record = capture();
        let reference = SemanticSourceReference::capture(&record, [2; 32], [3; 32], &[7; 32])
            .expect("owner captured exact complete source");
        let bytes = serde_json::to_vec(&reference).expect("portable reference");
        assert!(
            bytes.len()
                < serde_json::to_vec(&record.history_status)
                    .expect("full history proof extent")
                    .len()
        );
        let reopened: SemanticSourceReference =
            serde_json::from_slice(&bytes).expect("cold reference");
        assert_eq!(reopened, reference);
        reopened
            .verify_owner(&[7; 32])
            .expect("same durable owner secret after restart");
        assert_eq!(
            reopened.verify_owner(&[8; 32]),
            Err(SemanticSourceError::Identity)
        );
        for field in 0..5 {
            let mut changed = reopened.clone();
            match field {
                0 => changed.source_relation_root[0] ^= 1,
                1 => changed.semantic_relation_root[0] ^= 1,
                2 => changed.metadata.generation_root[0] ^= 1,
                3 => changed.metadata.freshness = SemanticVersionFreshness::Unverified,
                _ => changed.source.version[0] ^= 1,
            }
            assert_eq!(
                changed.verify_owner(&[7; 32]),
                Err(SemanticSourceError::Identity)
            );
        }
    }

    #[test]
    fn captured_proof_catalog_reconstructs_every_original_source_byte_and_refuses_partial_reordered_sets()
     {
        let record = capture();
        let reference = SemanticSourceReference::capture(&record, [2; 32], [3; 32], &[7; 32])
            .expect("exact source");
        let SemanticHistoryPublicationStatus::Published { proof, .. } = &record.history_status
        else {
            panic!("actual published capture");
        };
        assert_eq!(proof.images.len(), 42);
        let admitted = reference
            .verify_owner(&[7; 32])
            .expect("native owner admission");
        let document = admitted
            .reconstruct(proof.images.clone())
            .expect("full native proof catalog");
        assert_eq!(document.source(), &reference.source);
        assert_eq!(
            document.bytes(),
            super::super::canonical_semantic_source(&record).expect("original canonical bytes")
        );
        assert!(
            admitted
                .reconstruct(proof.images[..41].to_vec().into_boxed_slice())
                .is_err()
        );
        let mut reordered = proof.images.to_vec();
        reordered.swap(0, 1);
        assert!(admitted.reconstruct(reordered.into_boxed_slice()).is_err());
        let mut foreign = proof.images.to_vec();
        foreign[0].history_commit[0] ^= 1;
        assert!(admitted.reconstruct(foreign.into_boxed_slice()).is_err());
    }

    #[test]
    fn published_source_proof_rejects_a_cross_family_selection_profile() {
        let mut record = capture();
        let crate::SemanticHistoryPublicationStatus::Published { proof, .. } =
            &mut record.history_status
        else {
            panic!("actual published capture");
        };
        let original = proof.selection.profile;
        let foreign =
            crate::SemanticLanguageProfile::from_name("python").expect("closed Python profile");
        assert_ne!(
            original, foreign,
            "fixture uses a different compiler family"
        );
        proof.selection.profile = foreign;
        assert_eq!(
            record.admit(),
            Err(crate::ProductAdmissionError::SemanticVersionShape),
            "a captured source proof cannot be rebound to another language family"
        );
    }
}
