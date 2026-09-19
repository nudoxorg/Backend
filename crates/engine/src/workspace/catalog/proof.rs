//! Checked derived-output proof and immutable catalog entry types.

use super::super::head::HeadExpectation;
use super::super::owner::WorkspaceError;
use backend_execution::{AuthorityVersion, OutputVersion, ResultReceipt, WorkKey};
use backend_replication::AttestationClass;
use backend_semantic::{DependencyManifest, DependencyManifestSchema};
use backend_store::{ObjectId, TypedObject};
use backend_version::{ObjectKey, ObjectVersion, Relation};
use std::sync::Arc;

/// A checked output publication proof owned by the engine.
///
/// The constructor is crate-private. Only dispatch code that already holds a
/// scheduler result receipt and an admitted semantic capability can mint it.
#[derive(Clone, Debug)]
pub(crate) struct DerivedOutputProof {
    pub(crate) key: WorkKey,
    pub(crate) output: OutputVersion,
    pub(crate) bytes: Arc<Vec<u8>>,
    pub(crate) dependency_manifest: DependencyManifest,
    pub(crate) coverage_identity: [u8; 32],
    pub(crate) scope: u64,
    pub(crate) read_manifest: [u8; 32],
    pub(crate) authority: AuthorityVersion,
    pub(crate) authority_epoch: u64,
    pub(crate) revocation_version: u64,
    pub(crate) dependency_generation: u64,
    pub(crate) authority_class: AttestationClass,
    pub(crate) request: [u8; 32],
}

impl DerivedOutputProof {
    /// Mints the proof from a trusted scheduler receipt and semantic witness.
    /// The supplied byte owner must point to the exact bytes admitted by the
    /// receipt; this lets local and remote completion share its allocation.
    pub(crate) fn from_receipt<R: Relation>(
        receipt: &ResultReceipt<R>,
        semantic: &crate::dispatch::CompleteSemanticCoverage,
        bytes: Arc<Vec<u8>>,
        dependency_generation: u64,
        authority_class: AttestationClass,
    ) -> Result<Self, WorkspaceError> {
        if dependency_generation == 0
            || !authority_class.is_publishable()
            || !semantic.binds(&receipt.identity())
            || bytes.as_slice() != receipt.canonical_bytes()
            || OutputVersion::from_value(bytes.as_slice()) != receipt.result()
        {
            return Err(WorkspaceError::Corrupt("derived output admission binding"));
        }
        let dependency_manifest = semantic
            .dependency_manifest()
            .ok_or(WorkspaceError::Corrupt(
                "derived output lacks dependency manifest",
            ))?
            .clone();
        if !dependency_manifest.is_reuse_ready()
            || semantic.authority_epoch().0 != receipt.authority_evidence().authority_epoch()
            || semantic.revocation_version().0 != receipt.authority_evidence().revocation_version()
            || receipt.authority_evidence().key() != receipt.key()
            || receipt.authority_evidence().output() != receipt.result()
        {
            return Err(WorkspaceError::Corrupt("derived output authority binding"));
        }
        let identity = receipt.identity();
        let request_input = RequestIdInput {
            key: receipt.key(),
            output: receipt.result(),
            manifest: &dependency_manifest,
            authority: receipt.authority_evidence().authority(),
            authority_epoch: receipt.authority_evidence().authority_epoch(),
            revocation_version: receipt.authority_evidence().revocation_version(),
            dependency_generation,
            coverage_identity: semantic.identity().to_bytes(),
            scope: semantic.scope(),
            read_manifest: semantic.read_manifest().to_bytes(),
            authority_class,
        };
        let request = request_id(&request_input);
        Ok(Self {
            key: receipt.key(),
            output: receipt.result(),
            bytes,
            dependency_manifest,
            coverage_identity: semantic.identity().to_bytes(),
            scope: semantic.scope(),
            read_manifest: semantic.read_manifest().to_bytes(),
            authority: identity.authority,
            authority_epoch: receipt.authority_evidence().authority_epoch(),
            revocation_version: receipt.authority_evidence().revocation_version(),
            dependency_generation,
            authority_class,
            request,
        })
    }

    pub(crate) const fn request(&self) -> [u8; 32] {
        self.request
    }

    /// Builds the two immutable payload objects carried by this proof. The
    /// objects are typed from the proof's canonical bytes, so staging cannot
    /// substitute a caller supplied object identity.
    pub(crate) fn payload_objects(&self) -> (TypedObject, TypedObject) {
        let output_bytes = self.bytes.as_slice();
        let manifest_bytes = self.dependency_manifest.canonical_bytes();
        let output_object = TypedObject::from_value(
            &ObjectKey::<super::DerivedOutputBytesSchema>::from_value(output_bytes),
            output_bytes,
        );
        let manifest_object = TypedObject::from_value(
            &ObjectKey::<super::DerivedOutputManifestSchema>::from_value(&manifest_bytes),
            &manifest_bytes,
        );
        (output_object, manifest_object)
    }

    /// Encodes the complete catalog provenance needed to audit a staged
    /// output after the in-memory dispatcher has gone away.  The journal
    /// keeps this compact envelope beside the CAS object references; it is
    /// deliberately independent of the private proof layout so the replay
    /// grammar can evolve without exposing catalog mutation capabilities.
    pub(crate) fn journal_provenance(&self) -> Box<[u8]> {
        let manifest = self.dependency_manifest.canonical_bytes();
        let mut bytes = Vec::with_capacity(1 + 32 * 5 + 8 * 3 + manifest.len() + 1);
        bytes.push(1);
        bytes.extend_from_slice(&self.key.to_bytes());
        bytes.extend_from_slice(&self.output.to_bytes());
        bytes.extend_from_slice(&self.coverage_identity);
        bytes.extend_from_slice(&self.read_manifest);
        bytes.extend_from_slice(&self.authority.to_bytes());
        bytes.extend_from_slice(&self.scope.to_be_bytes());
        bytes.extend_from_slice(&self.authority_epoch.to_be_bytes());
        bytes.extend_from_slice(&self.revocation_version.to_be_bytes());
        bytes.extend_from_slice(&self.dependency_generation.to_be_bytes());
        bytes.push(class_tag(self.authority_class));
        bytes.extend_from_slice(&self.request);
        append_journal_bytes(&mut bytes, &manifest);
        bytes.into_boxed_slice()
    }
}

/// Owner-issued capability proving that derived-output payloads were staged
/// in the immutable store for one exact selected-head expectation. It carries
/// no publication method; only the workspace owner can consume it.
#[derive(Clone, Debug)]
pub(crate) struct StagedDerivedOutput {
    pub(crate) proof: DerivedOutputProof,
    pub(crate) base: HeadExpectation,
    pub(crate) owner_epoch: u64,
    pub(crate) output_object: ObjectId,
    pub(crate) manifest_object: ObjectId,
}

impl StagedDerivedOutput {
    /// Converts the owner-staged proof into the durable dispatch proof used
    /// for crash recovery.  Both immutable object IDs are included, so a
    /// restart can distinguish a fully staged result from an output digest
    /// that has no recoverable payload.
    pub(crate) fn accepted_result_proof(
        &self,
        accepted_at: u64,
    ) -> Result<crate::dispatch::AcceptedResultProof, WorkspaceError> {
        let proof = crate::dispatch::AcceptedResultProof::new(
            self.proof.output.to_bytes(),
            self.proof.bytes.as_slice().to_vec().into_boxed_slice(),
            accepted_at,
        )
        .map_err(|_| WorkspaceError::Corrupt("derived output journal proof bounds"))?;
        proof
            .with_staged_objects(
                *self.output_object.as_bytes(),
                *self.manifest_object.as_bytes(),
                self.proof.journal_provenance(),
            )
            .map_err(|_| WorkspaceError::Corrupt("derived output journal proof"))
    }
}

fn append_journal_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend_from_slice(bytes);
}

/// A compact catalog record. Its primary key carries the query identity; the
/// value carries only fixed-width provenance and immutable object references.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogRecord {
    pub(crate) key: [u8; 32],
    pub(crate) request: [u8; 32],
    pub(crate) output: [u8; 32],
    pub(crate) output_object: [u8; 32],
    pub(crate) manifest_object: [u8; 32],
    pub(crate) manifest_version: [u8; 32],
    pub(crate) coverage_identity: [u8; 32],
    pub(crate) scope: u64,
    pub(crate) read_manifest: [u8; 32],
    pub(crate) authority: [u8; 32],
    pub(crate) authority_epoch: u64,
    pub(crate) revocation_version: u64,
    pub(crate) dependency_generation: u64,
    pub(crate) authority_class: u8,
}

impl CatalogRecord {
    pub(crate) fn from_proof(
        proof: &DerivedOutputProof,
        output_object: &TypedObject,
        manifest_object: &TypedObject,
    ) -> Self {
        let manifest = proof.dependency_manifest.canonical_bytes();
        let manifest_version = manifest_version(&manifest);
        Self {
            key: proof.key.to_bytes(),
            request: proof.request,
            output: proof.output.to_bytes(),
            output_object: *output_object.id().as_bytes(),
            manifest_object: *manifest_object.id().as_bytes(),
            manifest_version,
            coverage_identity: proof.coverage_identity,
            scope: proof.scope,
            read_manifest: proof.read_manifest,
            authority: proof.authority.to_bytes(),
            authority_epoch: proof.authority_epoch,
            revocation_version: proof.revocation_version,
            dependency_generation: proof.dependency_generation,
            authority_class: class_tag(proof.authority_class),
        }
    }
}

/// A verified output recovered from a selected workspace closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedOutputEntry {
    pub(crate) key: WorkKey,
    pub(crate) output: OutputVersion,
    pub(crate) bytes: Arc<Vec<u8>>,
    pub(crate) dependency_manifest: Arc<[u8]>,
    pub(crate) dependency_manifest_version: [u8; 32],
    pub(crate) coverage_identity: [u8; 32],
    pub(crate) scope: u64,
    pub(crate) read_manifest: [u8; 32],
    pub(crate) authority: [u8; 32],
    pub(crate) authority_epoch: u64,
    pub(crate) revocation_version: u64,
    pub(crate) dependency_generation: u64,
    pub(crate) authority_class: AttestationClass,
}

impl DerivedOutputEntry {
    /// Returns the exact work key.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the canonical output version.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }

    /// Returns the canonical output bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// Returns the shared canonical output owner.
    #[must_use]
    pub fn bytes_arc(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.bytes)
    }

    /// Returns the exact canonical dependency manifest bytes.
    #[must_use]
    pub fn dependency_manifest_bytes(&self) -> &[u8] {
        &self.dependency_manifest
    }

    /// Returns the dependency manifest content identity.
    #[must_use]
    pub const fn dependency_manifest_version(&self) -> [u8; 32] {
        self.dependency_manifest_version
    }

    /// Returns the semantic coverage witness identity.
    #[must_use]
    pub const fn coverage_identity(&self) -> [u8; 32] {
        self.coverage_identity
    }

    /// Returns the exact admitted semantic scope.
    #[must_use]
    pub const fn scope(&self) -> u64 {
        self.scope
    }

    /// Returns the exact read-manifest identity.
    #[must_use]
    pub const fn read_manifest(&self) -> [u8; 32] {
        self.read_manifest
    }

    /// Returns the authority identity.
    #[must_use]
    pub const fn authority(&self) -> [u8; 32] {
        self.authority
    }

    /// Returns the authority epoch.
    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    /// Returns the revocation observation.
    #[must_use]
    pub const fn revocation_version(&self) -> u64 {
        self.revocation_version
    }

    /// Returns the semantic registration generation.
    #[must_use]
    pub const fn dependency_generation(&self) -> u64 {
        self.dependency_generation
    }

    /// Returns the verifier-owned authority class that admitted this output.
    #[must_use]
    pub const fn authority_class(&self) -> AttestationClass {
        self.authority_class
    }
}

/// Result of atomically publishing a derived output under the workspace head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DerivedOutputPublication {
    /// A new closure/head generation was selected by the store.
    Published {
        /// The verified output catalog entry.
        entry: DerivedOutputEntry,
        /// The selected workspace generation.
        sequence: u64,
        /// Diagnostic acknowledgements that remain pending.
        status: super::super::publication::PublicationStatus,
    },
    /// The exact entry was already selected; no second generation was minted.
    AlreadyPresent {
        /// The existing verified output catalog entry.
        entry: DerivedOutputEntry,
        /// The selected workspace generation.
        sequence: u64,
    },
}

pub(super) fn class_tag(class: AttestationClass) -> u8 {
    match class {
        AttestationClass::LocallyVerifiable => 1,
        AttestationClass::TrustedSigned => 2,
        AttestationClass::QuorumAudited => 3,
        AttestationClass::UntrustedMemo => 4,
    }
}

pub(super) fn class_from_tag(tag: u8) -> Result<AttestationClass, WorkspaceError> {
    match tag {
        1 => Ok(AttestationClass::LocallyVerifiable),
        2 => Ok(AttestationClass::TrustedSigned),
        3 => Ok(AttestationClass::QuorumAudited),
        4 => Ok(AttestationClass::UntrustedMemo),
        _ => Err(WorkspaceError::Corrupt("derived output authority class")),
    }
}

pub(super) fn manifest_version(bytes: &[u8]) -> [u8; 32] {
    let value = bytes.to_vec();
    ObjectVersion::<DependencyManifestSchema>::from_value(&value).to_bytes()
}

#[derive(Clone, Copy)]
struct RequestIdInput<'a> {
    key: WorkKey,
    output: OutputVersion,
    manifest: &'a DependencyManifest,
    authority: AuthorityVersion,
    authority_epoch: u64,
    revocation_version: u64,
    dependency_generation: u64,
    coverage_identity: [u8; 32],
    scope: u64,
    read_manifest: [u8; 32],
    authority_class: AttestationClass,
}

fn request_id(input: &RequestIdInput<'_>) -> [u8; 32] {
    let RequestIdInput {
        key,
        output,
        manifest,
        authority,
        authority_epoch,
        revocation_version,
        dependency_generation,
        coverage_identity,
        scope,
        read_manifest,
        authority_class,
    } = *input;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.derived-output.request.v1\0");
    hasher.update(&key.to_bytes());
    hasher.update(&output.to_bytes());
    let manifest_bytes = manifest.canonical_bytes();
    hasher.update(
        &u64::try_from(manifest_bytes.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(&manifest_bytes);
    hasher.update(&authority.to_bytes());
    hasher.update(&authority_epoch.to_be_bytes());
    hasher.update(&revocation_version.to_be_bytes());
    hasher.update(&dependency_generation.to_be_bytes());
    hasher.update(&coverage_identity);
    hasher.update(&scope.to_be_bytes());
    hasher.update(&read_manifest);
    hasher.update(&[class_tag(authority_class)]);
    *hasher.finalize().as_bytes()
}
