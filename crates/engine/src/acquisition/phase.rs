use blake3::Hasher;
use std::io::{Read, Write};
use std::sync::Arc;

use backend_execution::{WorkKey, acquisition_work_key};

use super::delta::{AcquisitionDelta, DeltaChange};
use super::freshness::FactFreshness;
use super::identity::{
    AcquisitionDeltaId, AcquisitionReceiptId, CHUNK_BYTES, ID_BYTES, IdentityError,
    PublicationRootId, RawArchiveObjectId, ReleaseClaim, SourceSnapshot, SourceSnapshotId,
    TreeManifest, canonical_text, digest,
};
use super::lease::{CasAdmission, LeaseStore};
use super::outcome::{AcquisitionOutcome, CorruptReason, RejectReason, Unavailable};

/// Exact key for one acquisition effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionRequest {
    /// Source endpoint identity (credentials excluded).
    pub source: [u8; ID_BYTES],
    /// Canonical package coordinate.
    pub coordinate: Arc<str>,
    /// Optional expected archive identity. When present it is an assertion.
    pub artifact: Option<RawArchiveObjectId>,
    /// Adapter protocol schema version.
    pub schema: u16,
    /// Policy epoch bound to this request.
    pub policy_epoch: u64,
    /// Explicit freshness frontier for mutable registry facts.
    pub facts: FactFreshness,
}

impl AcquisitionRequest {
    /// Admits one exact request.
    pub fn new(
        source: [u8; ID_BYTES],
        coordinate: impl Into<String>,
        artifact: RawArchiveObjectId,
        schema: u16,
        policy_epoch: u64,
    ) -> Result<Self, IdentityError> {
        Ok(Self {
            source,
            coordinate: Arc::from(canonical_text(coordinate.into())?),
            artifact: Some(artifact),
            schema,
            policy_epoch,
            facts: FactFreshness::always(),
        })
    }

    /// Admits a coordinate lookup whose archive identity is not known yet.
    pub fn for_coordinate(
        source: [u8; ID_BYTES],
        coordinate: impl Into<String>,
        schema: u16,
        policy_epoch: u64,
    ) -> Result<Self, IdentityError> {
        Ok(Self {
            source,
            coordinate: Arc::from(canonical_text(coordinate.into())?),
            artifact: None,
            schema,
            policy_epoch,
            facts: FactFreshness::always(),
        })
    }

    /// Returns a request with an explicit facts freshness frontier.
    #[must_use]
    pub const fn with_fact_freshness(mut self, facts: FactFreshness) -> Self {
        self.facts = facts;
        self
    }

    /// Returns a request bound to a policy epoch.
    #[must_use]
    pub const fn with_policy_epoch(mut self, policy_epoch: u64) -> Self {
        self.policy_epoch = policy_epoch;
        self
    }

    /// Returns the exact process-local interner key.
    #[must_use]
    pub fn work_key(&self) -> WorkKey {
        let mut frontier = Hasher::new();
        frontier.update(b"backend.acquisition.fact-frontier.v1\0");
        frontier.update(&self.policy_epoch.to_be_bytes());
        frontier.update(&self.facts.max_age_millis.to_be_bytes());
        let key_epoch = u64::from_be_bytes(
            frontier.finalize().as_bytes()[..8]
                .try_into()
                .expect("fixed digest prefix"),
        )
        .max(1);
        let artifact = self.artifact.map_or_else(
            || digest(b"backend.acquisition.unknown-artifact.v1", &[]),
            RawArchiveObjectId::to_bytes,
        );
        acquisition_work_key(
            self.source,
            self.coordinate.as_bytes(),
            artifact,
            self.schema,
            key_epoch,
        )
    }
}

/// Resolve phase of the acquisition typestate machine.
#[derive(Clone, Debug)]
pub struct Resolve {
    request: AcquisitionRequest,
}

impl Resolve {
    /// Starts resolution for one request.
    #[must_use]
    pub const fn new(request: AcquisitionRequest) -> Self {
        Self { request }
    }
    /// Returns the request identity.
    #[must_use]
    pub const fn request(&self) -> &AcquisitionRequest {
        &self.request
    }
    /// Admits decoded metadata supplied by a registry adapter.
    pub fn metadata(self, record: MetadataRecord) -> Result<Metadata, AcquisitionOutcome<()>> {
        if record.claim.source != self.request.source
            || self
                .request
                .artifact
                .is_some_and(|artifact| record.claim.archive != artifact)
            || record.claim.coordinate.as_ref() != self.request.coordinate.as_ref()
        {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Protocol));
        }
        Ok(Metadata {
            request: self.request,
            record,
        })
    }
}

/// Adapter-decoded metadata handoff. It contains no acquisition ownership.
#[derive(Clone, Debug)]
pub struct MetadataRecord {
    /// Authenticated release claim.
    pub claim: ReleaseClaim,
    /// Declared archive length.
    pub length: u64,
    /// Source cursor/proof digest.
    pub source_proof: [u8; ID_BYTES],
}

/// Metadata phase.
#[derive(Clone, Debug)]
pub struct Metadata {
    request: AcquisitionRequest,
    record: MetadataRecord,
}

impl Metadata {
    /// Admits an object staged by the transport adapter.
    pub fn object(self, object: RawArchiveObjectId) -> Result<Object, AcquisitionOutcome<()>> {
        if object != self.record.claim.archive {
            return Err(AcquisitionOutcome::Corrupt(CorruptReason::Integrity));
        }
        Ok(Object {
            request: self.request,
            record: self.record,
            object,
        })
    }
}

/// Object phase.
#[derive(Clone, Debug)]
pub struct Object {
    request: AcquisitionRequest,
    record: MetadataRecord,
    object: RawArchiveObjectId,
}

impl Object {
    /// Verifies an object using its streamed content identity.
    pub fn verified(
        self,
        actual: RawArchiveObjectId,
    ) -> Result<VerifiedObject, AcquisitionOutcome<()>> {
        if actual != self.object
            || self
                .request
                .artifact
                .is_some_and(|artifact| actual != artifact)
        {
            return Err(AcquisitionOutcome::Corrupt(CorruptReason::Integrity));
        }
        Ok(VerifiedObject {
            request: self.request,
            record: self.record,
            object: actual,
        })
    }
}

/// VerifiedObject phase.
#[derive(Clone, Debug)]
pub struct VerifiedObject {
    request: AcquisitionRequest,
    record: MetadataRecord,
    object: RawArchiveObjectId,
}

impl VerifiedObject {
    /// Moves to policy evaluation. Policy is explicit and cannot be bypassed.
    pub fn policy(self, allowed: bool) -> Result<Policy, AcquisitionOutcome<()>> {
        if !allowed {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Policy));
        }
        Ok(Policy { verified: self })
    }
    /// Returns the admitted archive identity.
    #[must_use]
    pub const fn object(&self) -> RawArchiveObjectId {
        self.object
    }
}

/// Policy phase.
#[derive(Clone, Debug)]
pub struct Policy {
    verified: VerifiedObject,
}

/// Immutable publication receipt pairing a delta with its root transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionReceipt {
    /// Receipt identity.
    pub id: AcquisitionReceiptId,
    /// Delta identity included in the receipt.
    pub delta: AcquisitionDeltaId,
    /// Prior source root.
    pub base: SourceSnapshotId,
    /// Newly published source root.
    pub target: SourceSnapshotId,
    /// Publication root identity.
    pub publication: PublicationRootId,
    /// Policy/advisory frontier that authenticated this publication.
    pub policy_epoch: u64,
}

impl AcquisitionReceipt {
    /// Returns the fixed canonical receipt preimage.
    #[must_use]
    pub fn canonical_bytes(&self) -> [u8; ID_BYTES * 4 + 8] {
        let mut encoded = [0_u8; ID_BYTES * 4 + 8];
        encoded[..ID_BYTES].copy_from_slice(self.delta.as_bytes());
        encoded[ID_BYTES..ID_BYTES * 2].copy_from_slice(self.base.as_bytes());
        encoded[ID_BYTES * 2..ID_BYTES * 3].copy_from_slice(self.target.as_bytes());
        encoded[ID_BYTES * 3..ID_BYTES * 4].copy_from_slice(self.publication.as_bytes());
        encoded[ID_BYTES * 4..].copy_from_slice(&self.policy_epoch.to_be_bytes());
        encoded
    }
}

/// PublishedDelta phase payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedDelta {
    /// Exact root-bound delta.
    pub delta: Arc<AcquisitionDelta>,
    /// Immutable publication receipt.
    pub receipt: Arc<AcquisitionReceipt>,
}
impl Policy {
    /// Builds and publishes an immutable target delta.
    pub fn publish(
        self,
        base: &SourceSnapshot,
        target: Arc<SourceSnapshot>,
        changes: Vec<DeltaChange>,
    ) -> AcquisitionOutcome<PublishedDelta> {
        let delta = match AcquisitionDelta::new(base, Arc::clone(&target), changes) {
            Ok(delta) => Arc::new(delta),
            Err(_) => return AcquisitionOutcome::Rejected(RejectReason::Protocol),
        };
        let publication = PublicationRootId::derive(&[
            delta.base().as_bytes(),
            delta.target().as_bytes(),
            delta.id().as_bytes(),
            &target.policy_epoch().to_be_bytes(),
        ]);
        let receipt_id = AcquisitionReceiptId::derive(&[
            &self.verified.request.source,
            delta.id().as_bytes(),
            publication.as_bytes(),
            &target.policy_epoch().to_be_bytes(),
        ]);
        let receipt = Arc::new(AcquisitionReceipt {
            id: receipt_id,
            delta: delta.id(),
            base: delta.base(),
            target: delta.target(),
            publication,
            policy_epoch: target.policy_epoch(),
        });
        AcquisitionOutcome::Hit(PublishedDelta { delta, receipt })
    }

    /// Returns the verified archive identity used by policy.
    #[must_use]
    pub const fn object(&self) -> RawArchiveObjectId {
        self.verified.object
    }
}
/// Marker for the six phases in the typed acquisition state machine.
pub trait AcquisitionPhase: sealed::Sealed {}
mod sealed {
    pub trait Sealed {}
}
impl sealed::Sealed for Resolve {}
impl sealed::Sealed for Metadata {}
impl sealed::Sealed for Object {}
impl sealed::Sealed for VerifiedObject {}
impl sealed::Sealed for Policy {}
impl sealed::Sealed for PublishedDelta {}
impl AcquisitionPhase for Resolve {}
impl AcquisitionPhase for Metadata {}
impl AcquisitionPhase for Object {}
impl AcquisitionPhase for VerifiedObject {}
impl AcquisitionPhase for Policy {}
impl AcquisitionPhase for PublishedDelta {}

/// Type-level owner of one acquisition phase. The concrete phase structs
/// above expose the useful transition APIs; this alias makes generic bounds
/// available to orchestration code without a runtime enum.
pub type AcquisitionState<S> = S;

/// Streams an archive into a private temp object and admits its identity.
pub fn admit_archive(
    source: &mut impl Read,
    store: &LeaseStore,
    maximum: u64,
) -> Result<(RawArchiveObjectId, CasAdmission), AcquisitionOutcome<()>> {
    let mut temp = store.temp("archive").map_err(|_| {
        AcquisitionOutcome::Unavailable(Unavailable {
            source: [0; ID_BYTES],
        })
    })?;
    let mut hasher = Hasher::new();
    hasher.update(b"backend.acquisition.archive.v1\0");
    let mut buffer = [0_u8; CHUNK_BYTES];
    let mut length = 0_u64;
    loop {
        let read = source.read(&mut buffer).map_err(|_| {
            AcquisitionOutcome::Unavailable(Unavailable {
                source: [0; ID_BYTES],
            })
        })?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or(AcquisitionOutcome::Corrupt(CorruptReason::Integrity))?;
        if length > maximum {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Bounds));
        }
        hasher.update(&buffer[..read]);
        temp.write_all(&buffer[..read]).map_err(|_| {
            AcquisitionOutcome::Unavailable(Unavailable {
                source: [0; ID_BYTES],
            })
        })?;
    }
    let digest = *hasher.finalize().as_bytes();
    let mut payload = Vec::with_capacity(ID_BYTES + 8);
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&digest);
    let object = RawArchiveObjectId::derive(&[&payload]);
    let admission = store.cas_admit(&mut temp, object).map_err(|_| {
        AcquisitionOutcome::Unavailable(Unavailable {
            source: [0; ID_BYTES],
        })
    })?;
    Ok((object, admission))
}
