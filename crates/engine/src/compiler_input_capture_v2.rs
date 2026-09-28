//! Bounded capture and independent admission of complete compiler workspaces.
//!
//! The capture binds actual file bytes, the complete scanner inventory, its
//! named ignore policy and revision fence, and portable invocation facts into
//! typed CAS objects. A full workspace is fresh-only metadata; it does not
//! assert a complete compiler read set or authorize incremental reuse.

use crate::compiler_input_manifest_v2::{
    CompilerInputManifestV2, CompilerInputManifestV2Error, CompilerInputManifestV2Schema,
    CompilerInvocationRecipeV2, CompilerPackageTargetV2, CompilerReadFrontierStatusV2,
    validate_compiler_input_path_v2,
};
use crate::compiler_input_tree_v2::{
    CompilerInputMerklePageSchema, CompilerInputMerklePageV2, CompilerInputMerkleTreeV2,
    CompilerInputTreeKindV2, CompilerInputTreeRecordV2, CompilerInputTreeV2Error,
    CompilerWorkspaceFileRoleV2,
};
use backend_execution::compiler_full_workspace_transfer_work_id;
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ClosureId, FileStore, ObjectId,
    PinnedStoredClosureReceipt, StoreError, StreamingClosureBudget, TypedObject, UntrustedObjectId,
};
use backend_version::{
    ContentId, ContentPayloadHasher, ObjectKeyHasher, ObjectVersionHasher, Schema, SchemaIdentity,
    SourceFactDomain,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use thiserror::Error;

const MAX_WORKSPACE_ENTRIES_V2: usize = 1_000_000;
const MAX_WORKSPACE_OBJECTS_V2: u64 = (MAX_WORKSPACE_ENTRIES_V2 as u64 * 2) + 1;
const MAX_WORKSPACE_PAGE_BYTES_V2: usize = 256 * 1024 * 1024;
const MAX_WORKSPACE_BUILD_CHARGE_BYTES_V2: usize = 64 * 1024 * 1024;
const MAX_WORKSPACE_FILE_BYTES_V2: u64 = 1_u64 << 40;
const MAX_WORKSPACE_TOTAL_FILE_BYTES_V2: u64 = 64 * 1024 * 1024 * 1024;
const MAX_WORKSPACE_MANIFEST_BYTES_V2: u64 = 8 * 1024 * 1024;
const MAX_WORKSPACE_CAPTURE_PAYLOAD_BYTES_V2: u64 = MAX_WORKSPACE_TOTAL_FILE_BYTES_V2
    + MAX_WORKSPACE_PAGE_BYTES_V2 as u64
    + MAX_WORKSPACE_MANIFEST_BYTES_V2;
const MAX_CAPTURE_CHUNK_BYTES_V2: usize = 1024 * 1024;
const MANIFEST_SCHEMA_IDENTITY_V2: SchemaIdentity = SchemaIdentity::new(0xe7, 1, 2);
const FILE_SCHEMA_IDENTITY_V2: SchemaIdentity = SchemaIdentity::new(0xe7, 3, 2);

/// Typed schema for bytes belonging to one captured workspace regular file.
pub struct CompilerWorkspaceFileV2Schema;

impl Schema for CompilerWorkspaceFileV2Schema {
    const DOMAIN: u8 = 0xe7;
    const TYPE: u16 = 3;
    const VERSION: u8 = 2;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Complete-workspace directory or file entry kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerWorkspaceEntryKindV2 {
    /// A directory included by the exact scanner policy.
    Directory,
    /// A regular file, including a valid zero-byte file.
    File {
        /// Exact length in bytes at the captured scanner revision.
        byte_length: u64,
    },
}

/// One normalized root-relative path from a complete scanner inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerWorkspaceEntryV2 {
    path: Box<str>,
    kind: CompilerWorkspaceEntryKindV2,
    role: Option<CompilerWorkspaceFileRoleV2>,
}

impl CompilerWorkspaceEntryV2 {
    /// Creates an inventory directory. The workspace root itself is `""`.
    #[must_use]
    pub fn directory(path: impl Into<Box<str>>) -> Self {
        Self {
            path: path.into(),
            kind: CompilerWorkspaceEntryKindV2::Directory,
            role: None,
        }
    }

    /// Creates one regular-file entry with its captured length and semantic role.
    #[must_use]
    pub fn file(
        path: impl Into<Box<str>>,
        byte_length: u64,
        role: CompilerWorkspaceFileRoleV2,
    ) -> Self {
        Self {
            path: path.into(),
            kind: CompilerWorkspaceEntryKindV2::File { byte_length },
            role: Some(role),
        }
    }

    /// Root-relative normalized path; empty only for the root directory.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact complete-inventory entry kind.
    #[must_use]
    pub const fn kind(&self) -> CompilerWorkspaceEntryKindV2 {
        self.kind
    }

    /// Source/configuration/lock/other role, absent for directories.
    #[must_use]
    pub const fn role(&self) -> Option<CompilerWorkspaceFileRoleV2> {
        self.role
    }
}

/// Source scanner capability used to capture one immutable complete inventory.
///
/// Implementations must provide the full path/kind inventory under one exact,
/// versioned ignore policy. File streams must be confined and tied to the
/// revision returned by `fence_digest`; `revalidate` must fail if any included
/// entry changes or if the scan policy discovers a different inventory.
pub trait WorkspaceSnapshotSourceV2 {
    /// Returns every admitted directory and regular file in portable path order.
    fn entries(&self) -> &[CompilerWorkspaceEntryV2];

    /// Returns the exact versioned ignore/generated-file policy identity.
    fn policy_identity(&self) -> &str;

    /// Returns an opaque digest pairing this inventory with its captured revisions.
    fn fence_digest(&self) -> [u8; 32];

    /// Rewalks and validates the complete source inventory against its capture fence.
    fn revalidate(&self) -> Result<bool, String>;

    /// Streams one captured regular file through a bounded confined reader.
    fn stream_file(
        &self,
        path: &str,
        max_bytes: u64,
        chunk_bytes: usize,
        consume: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, String>;
}

/// Identity facts supplied by the package and toolchain authorities for capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureWorkspaceIdentityV2 {
    package_target: CompilerPackageTargetV2,
    package_lineage: [u8; 32],
    invocation_recipe: CompilerInvocationRecipeV2,
    source_provenance: [u8; 32],
    max_output_bytes: u64,
}

/// Stable key for the small in-memory cache of prior workspace page trees.
///
/// Source fence and workspace roots are deliberately absent: edits should find
/// the previous tree for the same exact package/compiler authority tuple.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompilerInputCaptureCacheKeyV2 {
    package_lineage: [u8; 32],
    target: [u8; 32],
    recipe: [u8; 32],
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl CompilerInputCaptureCacheKeyV2 {
    /// Package/compiler tuple used to locate an eligible retained page tree.
    #[must_use]
    pub const fn fields(self) -> ([u8; 32], [u8; 32], [u8; 32], [u8; 32], [u8; 32], [u8; 32]) {
        (
            self.package_lineage,
            self.target,
            self.recipe,
            self.toolchain,
            self.environment,
            self.target_platform,
        )
    }
}

/// Construction path selected while recapturing a workspace snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerInputCaptureUpdateModeV2 {
    /// A full canonical page tree was built from the fresh inventory.
    FullBuild,
    /// Same-key records were checked and changed pages were path-copied.
    PathCopy,
}

/// Work saved or performed while constructing a capture's workspace pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerInputCaptureUpdateStatsV2 {
    /// Full build or persistent same-key update.
    pub mode: CompilerInputCaptureUpdateModeV2,
    /// File records whose exact byte object or length changed under an existing path.
    pub changed_file_records: Option<usize>,
    /// Page nodes encoded while path-copying the changed search paths.
    pub path_copied_pages: usize,
    /// Source-file bytes read once for hashing and once for closure streaming.
    pub source_bytes_read: u64,
}

impl CaptureWorkspaceIdentityV2 {
    /// Binds a package cell, lineage, portable invocation recipe, and provenance.
    #[must_use]
    pub const fn new(
        package_target: CompilerPackageTargetV2,
        package_lineage: [u8; 32],
        invocation_recipe: CompilerInvocationRecipeV2,
        source_provenance: [u8; 32],
        max_output_bytes: u64,
    ) -> Self {
        Self {
            package_target,
            package_lineage,
            invocation_recipe,
            source_provenance,
            max_output_bytes,
        }
    }

    /// Stable cache key for an exact package/compiler authority tuple.
    #[must_use]
    pub fn cache_key(&self) -> CompilerInputCaptureCacheKeyV2 {
        let recipe = self.invocation_recipe;
        CompilerInputCaptureCacheKeyV2 {
            package_lineage: self.package_lineage,
            target: *self.package_target.target().as_ref(),
            recipe: *recipe.identity().as_ref(),
            toolchain: *recipe.toolchain().as_ref(),
            environment: recipe.environment(),
            target_platform: recipe.target_platform(),
        }
    }
}

/// Opaque evidence that a complete workspace was captured and admitted in local CAS.
#[derive(Clone, Debug)]
pub struct CapturedFullWorkspaceV2 {
    manifest: CompilerInputManifestV2,
    workspace_tree: CompilerInputMerkleTreeV2,
    closure: ClosureId,
    manifest_object_id: ObjectId,
    object_count: u64,
    payload_bytes: u64,
    _pin: Arc<PinnedStoredClosureReceipt>,
}

/// Untrusted persisted claims required to reopen one offered V2 capture.
///
/// These fields are expectations only. They cannot mint a capture handle; the
/// cold-reopen constructor checks them against the pinned CAS closure, decoded
/// manifest, independently admitted workspace tree, and a rederived work ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapturedFullWorkspaceV2Expectation {
    /// Exact offered input closure identity.
    pub input_closure_id: [u8; 32],
    /// Exact typed manifest object identity in the closure.
    pub manifest_object_id: [u8; 32],
    /// Exact number of objects in the full closure.
    pub object_count: u64,
    /// Independently rederived sum of all canonical closure payload bytes.
    pub payload_bytes: u64,
    /// Work ID persisted with the Offered reservation.
    pub work_id: [u8; 16],
    /// Package lineage bound to the assignment.
    pub package_lineage: [u8; 32],
    /// Compilation unit target bound to the assignment.
    pub target: [u8; 32],
    /// Portable invocation recipe bound to the assignment.
    pub recipe: [u8; 32],
    /// Full-workspace input root.
    pub input_root: [u8; 32],
    /// Workspace snapshot identity carried in the assignment read-manifest slot.
    pub read_manifest: [u8; 32],
    /// Exact workspace snapshot identity.
    pub workspace_snapshot_id: [u8; 32],
    /// Maximum output bytes accepted for this compile.
    pub max_output_bytes: u64,
    /// Scanner revision-fence digest bound by the manifest.
    pub source_fence_digest: [u8; 32],
    /// Exact portable profile discriminator from the current trust grant.
    pub profile: [u8; 2],
    /// Exact compiler stage discriminator from the current trust grant.
    pub stage: u8,
    /// Resolved toolchain identity from the current trust grant.
    pub toolchain: [u8; 32],
    /// Portable compiler environment identity from the current trust grant.
    pub environment: [u8; 32],
    /// Target platform/sysroot identity from the current trust grant.
    pub target_platform: [u8; 32],
}

impl CapturedFullWorkspaceV2 {
    /// Whether two handles retain the same admitted capture and GC lifetime.
    ///
    /// Equal closure IDs alone are insufficient here: another FileStore can
    /// contain identical bytes while this owner's selected candidate is pinned
    /// in a different physical store.
    #[must_use]
    pub(crate) fn same_capture(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self._pin, &other._pin)
    }

    /// The canonical typed V2 manifest admitted in the capture closure.
    #[must_use]
    pub const fn manifest(&self) -> &CompilerInputManifestV2 {
        &self.manifest
    }

    /// Exact sealed local CAS closure containing manifest, pages, and file bytes.
    #[must_use]
    pub const fn closure(&self) -> ClosureId {
        self.closure
    }

    /// Exact typed manifest object ID included in the closure.
    #[must_use]
    pub const fn manifest_object_id(&self) -> ObjectId {
        self.manifest_object_id
    }

    /// Portable typed workspace input-root digest.
    #[must_use]
    pub const fn input_root(&self) -> [u8; 32] {
        self.manifest.input_root()
    }

    /// Exact source scanner revision-fence digest bound by the manifest.
    #[must_use]
    pub const fn source_fence_digest(&self) -> [u8; 32] {
        self.manifest.source_fence_digest()
    }

    /// Number of exact objects in the sealed closure.
    #[must_use]
    pub const fn object_count(&self) -> u64 {
        self.object_count
    }

    /// Canonical payload byte total independently checked across every closure member.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Encoded workspace-page bytes retained in memory for a warm update.
    #[must_use]
    pub fn retained_tree_bytes(&self) -> usize {
        self.workspace_tree.encoded_bytes()
    }

    /// Exact compiler-authority cache key for looking up a prior workspace tree.
    #[must_use]
    pub fn cache_key(&self) -> CompilerInputCaptureCacheKeyV2 {
        CompilerInputCaptureCacheKeyV2 {
            package_lineage: self.manifest.package_lineage(),
            target: *self.manifest.package_target().target().as_ref(),
            recipe: *self.manifest.invocation_recipe().identity().as_ref(),
            toolchain: *self.manifest.invocation_recipe().toolchain().as_ref(),
            environment: self.manifest.environment(),
            target_platform: self.manifest.target_platform(),
        }
    }

    /// Returns the receipt whose lifetime keeps this exact closure pinned against GC.
    #[must_use]
    pub fn pinned_receipt(&self) -> &PinnedStoredClosureReceipt {
        self._pin.as_ref()
    }

    /// Reopens and independently verifies the full workspace closure in a store.
    pub fn verify_in_store(
        &self,
        store: &FileStore,
    ) -> Result<VerifiedFullWorkspaceClosureV2, CompilerInputCaptureV2Error> {
        let verified = verify_full_workspace_closure_v2(
            store,
            self.closure,
            self.manifest_object_id,
            Some(self.object_count),
        )?;
        if verified.payload_bytes != self.payload_bytes {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
        Ok(verified)
    }

    /// Reopens persisted Offered-reservation claims under a GC pin and admits
    /// them only after verifying the exact CAS closure and every assignment fact.
    pub fn reopen_pinned_in_store(
        store: &FileStore,
        expected: CapturedFullWorkspaceV2Expectation,
        budget: ArtifactBudget,
    ) -> Result<Self, CompilerInputCaptureV2Error> {
        validate_reopen_expectation(expected, budget)?;
        let bounded_budget = ArtifactBudget::new(
            budget.max_changes,
            usize::try_from(expected.object_count)
                .map_err(|_| CompilerInputCaptureV2Error::Limit)?,
            expected.payload_bytes,
            budget.max_chunk_bytes,
            budget.max_put_calls,
        );
        let pinned_receipt = Arc::new(
            store
                .reopen_pinned_stored_closure(
                    ArtifactClosureClaim::from_bytes(expected.input_closure_id),
                    bounded_budget,
                )
                .map_err(store_error)?,
        );
        let receipt = pinned_receipt.receipt();
        if receipt.closure().as_bytes() != &expected.input_closure_id
            || receipt.object_count() != expected.object_count
            || receipt.bytes_verified() != expected.payload_bytes
        {
            return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
        }

        let manifest_member = store
            .artifact_sink(bounded_budget)
            .verify_closure_member(
                receipt.closure(),
                UntrustedObjectId::from_bytes(expected.manifest_object_id),
            )
            .map_err(store_error)?
            .ok_or(CompilerInputCaptureV2Error::ExpectationMismatch)?;
        let manifest_envelope = manifest_member.object();
        if manifest_member.closure() != receipt.closure()
            || manifest_member.object_id().as_bytes() != &expected.manifest_object_id
            || manifest_envelope.schema() != MANIFEST_SCHEMA_IDENTITY_V2
            || manifest_envelope.payload_len() > MAX_WORKSPACE_MANIFEST_BYTES_V2
        {
            return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
        }
        let manifest_object = store
            .read_object_claim(UntrustedObjectId::from_bytes(expected.manifest_object_id))
            .map_err(store_error)?;
        if manifest_object.id().as_bytes() != &expected.manifest_object_id {
            return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
        }
        let manifest = CompilerInputManifestV2::from_typed_object(&manifest_object)?;
        validate_manifest_expectation(&manifest, expected)?;

        let verified = verify_full_workspace_closure_v2(
            store,
            receipt.closure(),
            manifest_object.id(),
            Some(expected.object_count),
        )?;
        if verified.payload_bytes != expected.payload_bytes
            || verified.manifest != manifest
            || verified.manifest_object_id.as_bytes() != &expected.manifest_object_id
        {
            return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
        }
        let payload_bytes = verified.payload_bytes;
        let workspace_tree = verified.tree;
        Ok(Self {
            manifest,
            workspace_tree,
            closure: receipt.closure(),
            manifest_object_id: manifest_object.id(),
            object_count: receipt.object_count(),
            payload_bytes,
            _pin: pinned_receipt,
        })
    }
}

/// Independently admitted full-workspace closure for a worker or coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedFullWorkspaceClosureV2 {
    manifest: CompilerInputManifestV2,
    closure: ClosureId,
    manifest_object_id: ObjectId,
    tree: CompilerInputMerkleTreeV2,
    members: Vec<ObjectId>,
    source_identities: HashMap<[u8; 32], ContentId<SourceFactDomain>>,
    payload_bytes: u64,
}

impl VerifiedFullWorkspaceClosureV2 {
    /// Canonical manifest admitted from the exact closure member.
    #[must_use]
    pub const fn manifest(&self) -> &CompilerInputManifestV2 {
        &self.manifest
    }

    /// Exact complete closure identity.
    #[must_use]
    pub const fn closure(&self) -> ClosureId {
        self.closure
    }

    /// Exact manifest ObjectId in the closure.
    #[must_use]
    pub const fn manifest_object_id(&self) -> ObjectId {
        self.manifest_object_id
    }

    /// Independently derived sum of every unique closure member payload.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Fully checked ordered workspace inventory tree.
    #[must_use]
    pub const fn workspace_tree(&self) -> &CompilerInputMerkleTreeV2 {
        &self.tree
    }

    /// Exact closure members in ascending ObjectId order.
    pub fn member_ids(&self) -> impl ExactSizeIterator<Item = ObjectId> + '_ {
        self.members.iter().copied()
    }

    /// Deterministic postorder inventory records, including directories and empty files.
    pub fn workspace_records(&self) -> impl Iterator<Item = &CompilerInputTreeRecordV2> {
        self.tree.pages().map(|page| page.record())
    }

    /// Returns the verified source-content identity for one source-role file.
    ///
    /// The identity is derived while verifying the file object in bounded
    /// chunks, so callers can rederive source-specific compile recipes without
    /// materializing potentially large source files.
    #[must_use]
    pub fn source_identity(&self, path: &str) -> Option<ContentId<SourceFactDomain>> {
        match self.tree.lookup(path, 2)? {
            CompilerInputTreeRecordV2::File {
                role: CompilerWorkspaceFileRoleV2::Source,
                object_id,
                ..
            } => self.source_identities.get(object_id).copied(),
            _ => None,
        }
    }
}

/// Failure while capturing or independently admitting a V2 full workspace.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CompilerInputCaptureV2Error {
    /// The scanner inventory is incomplete, unordered, or has no root directory.
    #[error("compiler workspace inventory is incomplete or noncanonical")]
    InvalidInventory,
    /// A bounded source file read failed or disagreed with its fence.
    #[error("compiler workspace source fence failed: {0}")]
    Source(String),
    /// A streaming closure operation failed.
    #[error("compiler workspace CAS capture failed: {0:?}")]
    Store(StoreError),
    /// A content digest could not be derived from its declared payload size.
    #[error("compiler workspace payload identity is invalid")]
    PayloadIdentity,
    /// The typed manifest could not be constructed or admitted.
    #[error(transparent)]
    Manifest(#[from] CompilerInputManifestV2Error),
    /// A page tree could not be constructed or admitted.
    #[error(transparent)]
    Tree(#[from] CompilerInputTreeV2Error),
    /// The sealed closure differs from the exact V2 manifest, page tree, or files.
    #[error("compiler workspace closure members do not exactly match its inventory")]
    ClosureMismatch,
    /// Persisted Offered-reservation claims differ from the independently verified capture.
    #[error("persisted compiler capture claims do not match its verified closure")]
    ExpectationMismatch,
    /// The requested closure exceeds one of the capture hard bounds.
    #[error("compiler workspace capture exceeds a fixed bound")]
    Limit,
}

/// Captures one complete source snapshot as a fresh V2 compiler input closure.
pub fn capture_full_workspace_v2(
    source: &impl WorkspaceSnapshotSourceV2,
    identity: CaptureWorkspaceIdentityV2,
    store: &FileStore,
    budget: StreamingClosureBudget,
) -> Result<CapturedFullWorkspaceV2, CompilerInputCaptureV2Error> {
    capture_full_workspace_v2_with_prior(source, identity, store, budget, None)
        .map(|(capture, _stats)| capture)
}

/// Captures a fresh full workspace, path-copying a compatible retained page tree when possible.
///
/// This accelerates Merkle-page construction only. It still scans and rehashes every included
/// file, captures a full closure, and never authorizes compiler-result reuse. The current V2
/// manifest's compiler read frontier remains `Unproven`; changed keys or authority identity
/// always take the full-build path.
pub fn capture_full_workspace_v2_with_prior(
    source: &impl WorkspaceSnapshotSourceV2,
    identity: CaptureWorkspaceIdentityV2,
    store: &FileStore,
    budget: StreamingClosureBudget,
    prior: Option<&CapturedFullWorkspaceV2>,
) -> Result<(CapturedFullWorkspaceV2, CompilerInputCaptureUpdateStatsV2), CompilerInputCaptureV2Error>
{
    let entries = source.entries();
    validate_inventory(entries)?;
    if entries.len() > MAX_WORKSPACE_ENTRIES_V2
        || budget.max_chunk_bytes == 0
        || budget.max_object_bytes == 0
        || !source
            .revalidate()
            .map_err(CompilerInputCaptureV2Error::Source)?
    {
        return Err(CompilerInputCaptureV2Error::InvalidInventory);
    }
    let policy_identity = source.policy_identity().to_owned();
    let source_fence_digest = source.fence_digest();
    if source_fence_digest == [0; 32]
        || policy_identity.is_empty()
        || policy_identity.len() > 512
        || !policy_identity.is_ascii()
        || identity.package_lineage == [0; 32]
        || identity.source_provenance == [0; 32]
        || identity.max_output_bytes == 0
        || identity.package_target.encode().is_err()
    {
        return Err(CompilerInputCaptureV2Error::InvalidInventory);
    }
    let prior_tree = prior.filter(|capture| {
        prior_capture_is_compatible(capture, &identity, &policy_identity)
            && workspace_inventory_matches_tree(entries, &capture.workspace_tree)
    });
    let total_file_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        let CompilerWorkspaceEntryKindV2::File { byte_length } = entry.kind else {
            return Some(total);
        };
        total.checked_add(byte_length)
    });
    if total_file_bytes.is_none_or(|bytes| {
        bytes > MAX_WORKSPACE_TOTAL_FILE_BYTES_V2 || bytes > budget.max_payload_bytes
    }) {
        return Err(CompilerInputCaptureV2Error::Limit);
    }
    let source_bytes_read = total_file_bytes
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(CompilerInputCaptureV2Error::Limit)?;

    let mut builder = store.begin_streaming_closure(budget).map_err(store_error)?;
    let chunk_bytes = budget.max_chunk_bytes.min(MAX_CAPTURE_CHUNK_BYTES_V2);
    let mut records = Vec::new();
    records
        .try_reserve_exact(entries.len())
        .map_err(|_| CompilerInputCaptureV2Error::Limit)?;

    for entry in entries {
        match entry.kind {
            CompilerWorkspaceEntryKindV2::Directory => {
                records.push(CompilerInputTreeRecordV2::Directory {
                    path: entry.path.clone(),
                });
            }
            CompilerWorkspaceEntryKindV2::File { byte_length } => {
                let length =
                    usize::try_from(byte_length).map_err(|_| CompilerInputCaptureV2Error::Limit)?;
                if byte_length > MAX_WORKSPACE_FILE_BYTES_V2
                    || byte_length > u64::try_from(budget.max_object_bytes).unwrap_or(u64::MAX)
                {
                    return Err(CompilerInputCaptureV2Error::Limit);
                }
                let (key, version) =
                    hash_file_payload(source, entry.path(), byte_length, length, chunk_bytes)?;
                let claim =
                    ArtifactObjectClaim::new(FILE_SCHEMA_IDENTITY_V2, key, version, byte_length);
                let mut stream = builder.begin_object(claim).map_err(store_error)?;
                let streamed = source
                    .stream_file(entry.path(), byte_length, chunk_bytes, &mut |chunk| {
                        stream.write(chunk).map_err(|error| format!("{error:?}"))?;
                        Ok(())
                    })
                    .map_err(CompilerInputCaptureV2Error::Source)?;
                if streamed != byte_length {
                    return Err(CompilerInputCaptureV2Error::Source(
                        "streamed file length differs from captured inventory".to_owned(),
                    ));
                }
                let object_id = stream.finish().map_err(store_error)?;
                records.push(CompilerInputTreeRecordV2::File {
                    path: entry.path.clone(),
                    role: entry.role.unwrap_or(CompilerWorkspaceFileRoleV2::Other),
                    object_id: *object_id.as_bytes(),
                    length: byte_length,
                });
            }
        }
    }
    records.sort_by(|left, right| left.path().as_bytes().cmp(right.path().as_bytes()));
    let (tree, update_stats) = match prior_tree {
        Some(prior_capture) => {
            let (tree, changed_file_records) =
                apply_retained_tree_update(&prior_capture.workspace_tree, &records);
            match tree {
                Some((tree, path_copied_pages)) => (
                    tree,
                    CompilerInputCaptureUpdateStatsV2 {
                        mode: CompilerInputCaptureUpdateModeV2::PathCopy,
                        changed_file_records,
                        path_copied_pages,
                        source_bytes_read,
                    },
                ),
                None => (
                    CompilerInputMerkleTreeV2::from_sorted_records(
                        CompilerInputTreeKindV2::Workspace,
                        records,
                    )?,
                    CompilerInputCaptureUpdateStatsV2 {
                        mode: CompilerInputCaptureUpdateModeV2::FullBuild,
                        changed_file_records,
                        path_copied_pages: 0,
                        source_bytes_read,
                    },
                ),
            }
        }
        None => (
            CompilerInputMerkleTreeV2::from_sorted_records(
                CompilerInputTreeKindV2::Workspace,
                records,
            )?,
            CompilerInputCaptureUpdateStatsV2 {
                mode: CompilerInputCaptureUpdateModeV2::FullBuild,
                changed_file_records: None,
                path_copied_pages: 0,
                source_bytes_read,
            },
        ),
    };
    if tree.page_count() > MAX_WORKSPACE_ENTRIES_V2 {
        return Err(CompilerInputCaptureV2Error::Limit);
    }
    let required_objects = builder
        .object_count()
        .checked_add(tree.page_count())
        .and_then(|count| count.checked_add(1))
        .ok_or(CompilerInputCaptureV2Error::Limit)?;
    if required_objects > budget.max_objects {
        return Err(CompilerInputCaptureV2Error::Limit);
    }

    for page in tree.pages() {
        let object = page.typed_object();
        let id = stream_typed_object(&mut builder, &object, chunk_bytes)?;
        if *id.as_bytes() != page.id() {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
    }

    let manifest = CompilerInputManifestV2::new(
        identity.package_target,
        identity.package_lineage,
        identity.invocation_recipe,
        identity.source_provenance,
        policy_identity,
        source_fence_digest,
        tree.root(),
        identity.max_output_bytes,
    )?;
    let manifest_object = manifest.typed_object()?;
    let manifest_object_id = manifest_object.id();
    let streamed_manifest_id = stream_typed_object(&mut builder, &manifest_object, chunk_bytes)?;
    if streamed_manifest_id != manifest_object_id {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    if !source
        .revalidate()
        .map_err(CompilerInputCaptureV2Error::Source)?
        || source.fence_digest() != source_fence_digest
    {
        return Err(CompilerInputCaptureV2Error::Source(
            "workspace changed while its immutable closure was being captured".to_owned(),
        ));
    }

    let pinned_receipt = Arc::new(builder.seal_pinned().map_err(store_error)?);
    let receipt = pinned_receipt.receipt();
    let mut captured = CapturedFullWorkspaceV2 {
        manifest,
        workspace_tree: tree,
        closure: receipt.closure(),
        manifest_object_id,
        object_count: receipt.object_count(),
        payload_bytes: receipt.payload_bytes(),
        _pin: pinned_receipt,
    };
    let verified = captured.verify_in_store(store)?;
    if verified.members.len() as u64 != captured.object_count
        || verified.payload_bytes != captured.payload_bytes
    {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    captured.workspace_tree = verified.tree;
    Ok((captured, update_stats))
}

fn prior_capture_is_compatible(
    prior: &CapturedFullWorkspaceV2,
    identity: &CaptureWorkspaceIdentityV2,
    policy_identity: &str,
) -> bool {
    let manifest = &prior.manifest;
    manifest.package_target() == &identity.package_target
        && manifest.package_lineage() == identity.package_lineage
        && manifest.invocation_recipe() == identity.invocation_recipe
        && manifest.max_output_bytes() == identity.max_output_bytes
        && manifest.policy_identity() == policy_identity
        && manifest.identity_claim().is_ok()
        && manifest.workspace_root() == prior.workspace_tree.root()
        && manifest.read_manifest() == manifest.workspace_snapshot_id().to_bytes()
        && manifest.read_frontier_status() == CompilerReadFrontierStatusV2::Unproven
        && prior.workspace_tree.kind() == CompilerInputTreeKindV2::Workspace
}

fn workspace_inventory_matches_tree(
    entries: &[CompilerWorkspaceEntryV2],
    tree: &CompilerInputMerkleTreeV2,
) -> bool {
    if entries.len() != tree.page_count() || tree.kind() != CompilerInputTreeKindV2::Workspace {
        return false;
    }
    let mut records = tree.records();
    entries.iter().all(|entry| {
        let Some(record) = records.next() else {
            return false;
        };
        match (entry.kind, record) {
            (
                CompilerWorkspaceEntryKindV2::Directory,
                CompilerInputTreeRecordV2::Directory { path },
            ) => entry.path() == path.as_ref(),
            (
                CompilerWorkspaceEntryKindV2::File { .. },
                CompilerInputTreeRecordV2::File { path, role, .. },
            ) => entry.path() == path.as_ref() && entry.role == Some(*role),
            _ => false,
        }
    }) && records.next().is_none()
}

fn apply_retained_tree_update(
    base: &CompilerInputMerkleTreeV2,
    records: &[CompilerInputTreeRecordV2],
) -> (Option<(CompilerInputMerkleTreeV2, usize)>, Option<usize>) {
    const MAX_PATH_COPY_RECORDS: usize = 4_096;

    if base.kind() != CompilerInputTreeKindV2::Workspace || records.len() != base.page_count() {
        return (None, None);
    }
    let mut replacements = Vec::with_capacity(MAX_PATH_COPY_RECORDS.min(records.len()));
    let mut changed_file_records = 0_usize;
    for (previous, current) in base.records().zip(records) {
        match (previous, current) {
            (
                CompilerInputTreeRecordV2::Directory { path: old_path },
                CompilerInputTreeRecordV2::Directory { path: new_path },
            ) if old_path == new_path => {}
            (
                CompilerInputTreeRecordV2::File {
                    path: old_path,
                    role: old_role,
                    ..
                },
                CompilerInputTreeRecordV2::File {
                    path: new_path,
                    role: new_role,
                    ..
                },
            ) if old_path == new_path && old_role == new_role => {
                if previous != current {
                    changed_file_records = match changed_file_records.checked_add(1) {
                        Some(count) => count,
                        None => return (None, None),
                    };
                    if changed_file_records <= MAX_PATH_COPY_RECORDS {
                        replacements.push(
                            crate::compiler_input_tree_v2::CompilerInputTreeReplacementV2::new(
                                current.clone(),
                            ),
                        );
                    }
                }
            }
            _ => return (None, None),
        }
    }
    if changed_file_records > MAX_PATH_COPY_RECORDS {
        return (None, Some(changed_file_records));
    }
    let Ok((tree, stats)) = base.apply_replacements_with_stats(replacements) else {
        return (None, Some(changed_file_records));
    };
    if stats.replacement_records != changed_file_records {
        return (None, Some(changed_file_records));
    }
    (
        Some((tree, stats.path_copied_pages)),
        Some(changed_file_records),
    )
}

/// Reopens a typed manifest and proves its complete exact closure membership.
pub fn verify_full_workspace_closure_v2(
    store: &FileStore,
    closure_id: ClosureId,
    manifest_object_id: ObjectId,
    expected_object_count: Option<u64>,
) -> Result<VerifiedFullWorkspaceClosureV2, CompilerInputCaptureV2Error> {
    let closure = store.read_closure_index(closure_id).map_err(store_error)?;
    let closure_object_count = closure.object_count();
    if closure_object_count == 0
        || closure_object_count > MAX_WORKSPACE_OBJECTS_V2
        || expected_object_count.is_some_and(|expected| expected != closure_object_count)
    {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    let manifest_object = store.read_object(manifest_object_id).map_err(store_error)?;
    let manifest = CompilerInputManifestV2::from_typed_object(&manifest_object)?;
    if manifest_object.id() != manifest_object_id {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    let mut payload_bytes = u64::try_from(manifest_object.bytes().len())
        .map_err(|_| CompilerInputCaptureV2Error::Limit)?;

    let verification_sink = store.artifact_sink(ArtifactBudget::new(
        1,
        usize::try_from(MAX_WORKSPACE_OBJECTS_V2)
            .map_err(|_| CompilerInputCaptureV2Error::Limit)?,
        MAX_WORKSPACE_FILE_BYTES_V2,
        64 * 1024,
        1,
    ));
    let root_claim = UntrustedObjectId::from_bytes(manifest.workspace_root());
    let mut pending = vec![root_claim];
    let mut page_ids = HashSet::new();
    let mut file_ids = HashSet::new();
    let mut source_identities = HashMap::new();
    let mut pages = Vec::new();
    let mut encoded_page_bytes = 0_usize;
    let mut total_file_bytes = 0_u64;
    while let Some(page_claim) = pending.pop() {
        let object = store.read_object_claim(page_claim).map_err(store_error)?;
        let page_id = object.id();
        page_ids
            .try_reserve(1)
            .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
        if !page_ids.insert(page_id) {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
        if page_ids.len() > MAX_WORKSPACE_ENTRIES_V2 {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
        let page = CompilerInputMerklePageV2::from_typed_object(&object)?;
        if page.kind() != CompilerInputTreeKindV2::Workspace || page.id() != *page_id.as_bytes() {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
        encoded_page_bytes = encoded_page_bytes
            .checked_add(page.bytes().len())
            .ok_or(CompilerInputCaptureV2Error::Limit)?;
        if encoded_page_bytes > MAX_WORKSPACE_PAGE_BYTES_V2 {
            return Err(CompilerInputCaptureV2Error::Limit);
        }
        payload_bytes = payload_bytes
            .checked_add(
                u64::try_from(page.bytes().len())
                    .map_err(|_| CompilerInputCaptureV2Error::Limit)?,
            )
            .ok_or(CompilerInputCaptureV2Error::Limit)?;
        if let CompilerInputTreeRecordV2::File {
            object_id,
            length,
            role,
            ..
        } = page.record()
        {
            total_file_bytes = total_file_bytes
                .checked_add(*length)
                .ok_or(CompilerInputCaptureV2Error::Limit)?;
            if total_file_bytes > MAX_WORKSPACE_TOTAL_FILE_BYTES_V2 {
                return Err(CompilerInputCaptureV2Error::Limit);
            }
            let mut file_object = verification_sink
                .open_object(UntrustedObjectId::from_bytes(*object_id))
                .map_err(store_error)?
                .ok_or(CompilerInputCaptureV2Error::ClosureMismatch)?;
            let file_id = file_object.id();
            if file_object.schema() != FILE_SCHEMA_IDENTITY_V2
                || file_object.payload_len() != *length
            {
                return Err(CompilerInputCaptureV2Error::ClosureMismatch);
            }
            let (key_matches, source_identity) = verify_file_key(
                &mut file_object,
                *role == CompilerWorkspaceFileRoleV2::Source,
            )?;
            if !key_matches {
                return Err(CompilerInputCaptureV2Error::ClosureMismatch);
            }
            if let Some(source_identity) = source_identity {
                match source_identities.get(object_id).copied() {
                    Some(previous) if previous != source_identity => {
                        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
                    }
                    Some(_) => {}
                    None => {
                        source_identities
                            .try_reserve(1)
                            .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
                        source_identities.insert(*object_id, source_identity);
                    }
                }
            }
            // Multiple paths may name identical bytes and therefore one CAS
            // member; the tree still binds both paths and exact roles.
            file_ids
                .try_reserve(1)
                .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
            if file_ids.insert(file_id) {
                payload_bytes = payload_bytes
                    .checked_add(*length)
                    .ok_or(CompilerInputCaptureV2Error::Limit)?;
            }
        }
        if let Some(left) = page.left() {
            pending
                .try_reserve(1)
                .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
            pending.push(UntrustedObjectId::from_bytes(left));
        }
        if let Some(right) = page.right() {
            pending
                .try_reserve(1)
                .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
            pending.push(UntrustedObjectId::from_bytes(right));
        }
        pages
            .try_reserve(1)
            .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
        pages.push(page);
    }
    let tree = CompilerInputMerkleTreeV2::from_pages(
        CompilerInputTreeKindV2::Workspace,
        manifest.workspace_root(),
        pages,
    )?;
    let mut members = Vec::new();
    let expected_member_count = page_ids
        .len()
        .checked_add(file_ids.len())
        .and_then(|count| count.checked_add(1))
        .ok_or(CompilerInputCaptureV2Error::Limit)?;
    if expected_member_count as u64 != closure_object_count {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    members
        .try_reserve_exact(expected_member_count)
        .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
    members.extend(page_ids);
    members.extend(file_ids);
    members.push(manifest_object_id);
    members.sort_unstable();
    if members.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    let mut cursor = None;
    let mut verified_count = 0_usize;
    loop {
        let page = closure.page_ids(cursor, 4_096).map_err(store_error)?;
        let observed = page.object_ids();
        let end = verified_count
            .checked_add(observed.len())
            .ok_or(CompilerInputCaptureV2Error::Limit)?;
        if members.get(verified_count..end) != Some(observed) {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
        verified_count = end;
        match page.next() {
            Some(next) if !observed.is_empty() && observed.last() == Some(&next) => {
                cursor = Some(next);
            }
            Some(_) => return Err(CompilerInputCaptureV2Error::ClosureMismatch),
            None => break,
        }
    }
    if verified_count != members.len() {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    Ok(VerifiedFullWorkspaceClosureV2 {
        manifest,
        closure: closure_id,
        manifest_object_id,
        tree,
        members,
        source_identities,
        payload_bytes,
    })
}

fn validate_reopen_expectation(
    expected: CapturedFullWorkspaceV2Expectation,
    budget: ArtifactBudget,
) -> Result<(), CompilerInputCaptureV2Error> {
    let expected_object_count =
        usize::try_from(expected.object_count).map_err(|_| CompilerInputCaptureV2Error::Limit)?;
    if expected.input_closure_id == [0; 32]
        || expected.manifest_object_id == [0; 32]
        || expected.object_count < 2
        || expected.object_count > MAX_WORKSPACE_OBJECTS_V2
        || expected.payload_bytes == 0
        || expected.payload_bytes > MAX_WORKSPACE_CAPTURE_PAYLOAD_BYTES_V2
        || expected.work_id == [0; 16]
        || expected.package_lineage == [0; 32]
        || expected.target == [0; 32]
        || expected.recipe == [0; 32]
        || expected.input_root == [0; 32]
        || expected.read_manifest == [0; 32]
        || expected.workspace_snapshot_id == [0; 32]
        || expected.max_output_bytes == 0
        || expected.source_fence_digest == [0; 32]
        || expected.toolchain == [0; 32]
        || expected.environment == [0; 32]
        || expected.target_platform == [0; 32]
        || expected.read_manifest != expected.workspace_snapshot_id
        || expected_object_count > budget.max_closure_objects
        || expected.payload_bytes > budget.max_payload_bytes
    {
        return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
    }
    let expected_work_id = compiler_full_workspace_transfer_work_id(
        expected.package_lineage,
        expected.target,
        expected.recipe,
        expected.read_manifest,
        expected.input_root,
        expected.input_closure_id,
        expected.manifest_object_id,
        expected.max_output_bytes,
    );
    if expected_work_id != expected.work_id {
        return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
    }
    Ok(())
}

fn validate_manifest_expectation(
    manifest: &CompilerInputManifestV2,
    expected: CapturedFullWorkspaceV2Expectation,
) -> Result<(), CompilerInputCaptureV2Error> {
    // This also validates all canonical typed identities and rederives the
    // manifest's workspace root and snapshot identity before comparing claims.
    manifest.identity_claim()?;
    let target = *manifest.package_target().target().as_ref();
    if manifest.package_lineage() != expected.package_lineage
        || target != expected.target
        || manifest.recipe() != expected.recipe
        || manifest.input_root() != expected.input_root
        || manifest.read_manifest() != expected.read_manifest
        || manifest.workspace_snapshot_id().to_bytes() != expected.workspace_snapshot_id
        || manifest.max_output_bytes() != expected.max_output_bytes
        || manifest.source_fence_digest() != expected.source_fence_digest
        || <[u8; 2]>::from(manifest.profile()) != expected.profile
        || u8::from(manifest.stage()) != expected.stage
        || manifest.toolchain() != expected.toolchain
        || manifest.environment() != expected.environment
        || manifest.target_platform() != expected.target_platform
    {
        return Err(CompilerInputCaptureV2Error::ExpectationMismatch);
    }
    Ok(())
}

fn validate_inventory(
    entries: &[CompilerWorkspaceEntryV2],
) -> Result<(), CompilerInputCaptureV2Error> {
    if entries.is_empty() || entries.len() > MAX_WORKSPACE_ENTRIES_V2 {
        return Err(CompilerInputCaptureV2Error::InvalidInventory);
    }
    if !matches!(entries.first(), Some(entry) if entry.path.is_empty() && entry.kind == CompilerWorkspaceEntryKindV2::Directory)
    {
        return Err(CompilerInputCaptureV2Error::InvalidInventory);
    }
    let mut prior: Option<&str> = None;
    let mut build_charge = 0_usize;
    let mut portable_paths = HashSet::new();
    portable_paths
        .try_reserve(entries.len())
        .map_err(|_| CompilerInputCaptureV2Error::Limit)?;
    for entry in entries {
        let entry_charge = entry
            .path
            .len()
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(256))
            .ok_or(CompilerInputCaptureV2Error::Limit)?;
        build_charge = build_charge
            .checked_add(entry_charge)
            .ok_or(CompilerInputCaptureV2Error::Limit)?;
        if build_charge > MAX_WORKSPACE_BUILD_CHARGE_BYTES_V2 {
            return Err(CompilerInputCaptureV2Error::Limit);
        }
        if prior.is_some_and(|path| path.as_bytes() >= entry.path.as_bytes())
            || match entry.kind {
                CompilerWorkspaceEntryKindV2::Directory => entry.role.is_some(),
                CompilerWorkspaceEntryKindV2::File { .. } => entry.role.is_none(),
            }
        {
            return Err(CompilerInputCaptureV2Error::InvalidInventory);
        }
        if entry.path.is_empty() {
            if entry.kind != CompilerWorkspaceEntryKindV2::Directory {
                return Err(CompilerInputCaptureV2Error::InvalidInventory);
            }
        } else {
            validate_compiler_input_path_v2(entry.path())
                .map_err(|_| CompilerInputCaptureV2Error::InvalidInventory)?;
            if !portable_paths.insert(entry.path().to_lowercase()) {
                return Err(CompilerInputCaptureV2Error::InvalidInventory);
            }

            // The complete ancestry is checked before capture can call `stream_file`.
            // Prefixes sort before descendants, so each directory must already be in this
            // ordered inventory; an intervening file cannot stand in for a directory.
            let mut descendant = entry.path();
            while let Some((ancestor, _)) = descendant.rsplit_once('/') {
                let Ok(index) = entries.binary_search_by(|candidate| {
                    candidate.path().as_bytes().cmp(ancestor.as_bytes())
                }) else {
                    return Err(CompilerInputCaptureV2Error::InvalidInventory);
                };
                if entries[index].kind != CompilerWorkspaceEntryKindV2::Directory {
                    return Err(CompilerInputCaptureV2Error::InvalidInventory);
                }
                descendant = ancestor;
            }
        }
        prior = Some(entry.path());
    }
    Ok(())
}

fn hash_file_payload(
    source: &impl WorkspaceSnapshotSourceV2,
    path: &str,
    byte_length: u64,
    length: usize,
    chunk_bytes: usize,
) -> Result<([u8; 32], [u8; 32]), CompilerInputCaptureV2Error> {
    let mut key = ObjectKeyHasher::new(FILE_SCHEMA_IDENTITY_V2, length)
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?;
    let mut version = ObjectVersionHasher::new(FILE_SCHEMA_IDENTITY_V2, length)
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?;
    let streamed = source
        .stream_file(path, byte_length, chunk_bytes, &mut |chunk| {
            key.update(chunk).map_err(|error| format!("{error:?}"))?;
            version
                .update(chunk)
                .map_err(|error| format!("{error:?}"))?;
            Ok(())
        })
        .map_err(CompilerInputCaptureV2Error::Source)?;
    if streamed != byte_length {
        return Err(CompilerInputCaptureV2Error::Source(
            "hashed file length differs from captured inventory".to_owned(),
        ));
    }
    let key = key
        .finish_key::<CompilerWorkspaceFileV2Schema>()
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?
        .to_bytes();
    let version = version
        .finish_version::<CompilerWorkspaceFileV2Schema>()
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?
        .to_bytes();
    Ok((key, version))
}

fn stream_typed_object(
    builder: &mut backend_store::StreamingClosureBuilder,
    object: &TypedObject,
    chunk_bytes: usize,
) -> Result<ObjectId, CompilerInputCaptureV2Error> {
    let length =
        u64::try_from(object.bytes().len()).map_err(|_| CompilerInputCaptureV2Error::Limit)?;
    let claim = ArtifactObjectClaim::new(object.schema(), *object.key(), *object.version(), length);
    let mut stream = builder.begin_object(claim).map_err(store_error)?;
    for chunk in object.bytes().chunks(chunk_bytes) {
        stream.write(chunk).map_err(store_error)?;
    }
    let id = stream.finish().map_err(store_error)?;
    if id != object.id() {
        return Err(CompilerInputCaptureV2Error::ClosureMismatch);
    }
    Ok(id)
}

fn verify_file_key(
    reader: &mut backend_store::ArtifactObjectReader,
    derive_source_identity: bool,
) -> Result<(bool, Option<ContentId<SourceFactDomain>>), CompilerInputCaptureV2Error> {
    let length =
        usize::try_from(reader.payload_len()).map_err(|_| CompilerInputCaptureV2Error::Limit)?;
    let mut hasher = ObjectKeyHasher::new(FILE_SCHEMA_IDENTITY_V2, length)
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?;
    let mut source_hasher = derive_source_identity
        .then(|| ContentPayloadHasher::<SourceFactDomain>::new(reader.payload_len()));
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    while offset < reader.payload_len() {
        let read = reader
            .read_payload_range(offset, &mut buffer)
            .map_err(store_error)?;
        if read == 0 {
            return Err(CompilerInputCaptureV2Error::ClosureMismatch);
        }
        hasher
            .update(&buffer[..read])
            .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?;
        if let Some(source_hasher) = &mut source_hasher {
            source_hasher
                .push_chunk(&buffer[..read])
                .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?;
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| CompilerInputCaptureV2Error::Limit)?)
            .ok_or(CompilerInputCaptureV2Error::Limit)?;
    }
    let key = hasher
        .finish_key::<CompilerWorkspaceFileV2Schema>()
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?
        .to_bytes();
    let source_identity = source_hasher
        .map(|hasher| hasher.finish())
        .transpose()
        .map_err(|_| CompilerInputCaptureV2Error::PayloadIdentity)?;
    Ok((&key == reader.key(), source_identity))
}

fn store_error(error: StoreError) -> CompilerInputCaptureV2Error {
    CompilerInputCaptureV2Error::Store(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{
        LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::ToolchainDomain;
    use std::{cell::Cell, collections::HashMap, path::PathBuf};

    struct MemorySnapshot {
        entries: Vec<CompilerWorkspaceEntryV2>,
        files: HashMap<String, Vec<u8>>,
        fence: [u8; 32],
    }

    impl WorkspaceSnapshotSourceV2 for MemorySnapshot {
        fn entries(&self) -> &[CompilerWorkspaceEntryV2] {
            &self.entries
        }

        fn policy_identity(&self) -> &str {
            "test-policy-v1"
        }

        fn fence_digest(&self) -> [u8; 32] {
            self.fence
        }

        fn revalidate(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn stream_file(
            &self,
            path: &str,
            max_bytes: u64,
            chunk_bytes: usize,
            consume: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<u64, String> {
            let bytes = self
                .files
                .get(path)
                .ok_or_else(|| "missing fixture file".to_owned())?;
            if u64::try_from(bytes.len()).map_err(|_| "fixture size".to_owned())? > max_bytes {
                return Err("fixture file exceeds byte budget".to_owned());
            }
            for chunk in bytes.chunks(chunk_bytes) {
                consume(chunk)?;
            }
            u64::try_from(bytes.len()).map_err(|_| "fixture size".to_owned())
        }
    }

    fn workspace_fixture(
        lib: &[u8],
        other: &[u8],
        missing: Option<&[u8]>,
        fence: [u8; 32],
    ) -> MemorySnapshot {
        let mut entries = vec![
            CompilerWorkspaceEntryV2::directory(""),
            CompilerWorkspaceEntryV2::file("Cargo.lock", 4, CompilerWorkspaceFileRoleV2::Lock),
            CompilerWorkspaceEntryV2::file(
                "Cargo.toml",
                9,
                CompilerWorkspaceFileRoleV2::Configuration,
            ),
            CompilerWorkspaceEntryV2::directory("src"),
            CompilerWorkspaceEntryV2::file(
                "src/lib.rs",
                u64::try_from(lib.len()).expect("fixture length"),
                CompilerWorkspaceFileRoleV2::Source,
            ),
            CompilerWorkspaceEntryV2::file(
                "src/other.rs",
                u64::try_from(other.len()).expect("fixture length"),
                CompilerWorkspaceFileRoleV2::Source,
            ),
        ];
        let mut files = HashMap::from([
            ("Cargo.lock".to_owned(), b"lock".to_vec()),
            ("Cargo.toml".to_owned(), b"[package]".to_vec()),
            ("src/lib.rs".to_owned(), lib.to_vec()),
            ("src/other.rs".to_owned(), other.to_vec()),
        ]);
        if let Some(missing) = missing {
            entries.push(CompilerWorkspaceEntryV2::file(
                "src/missing.rs",
                u64::try_from(missing.len()).expect("fixture length"),
                CompilerWorkspaceFileRoleV2::Source,
            ));
            files.insert("src/missing.rs".to_owned(), missing.to_vec());
        }
        entries[5..].sort_by(|left, right| left.path().as_bytes().cmp(right.path().as_bytes()));
        MemorySnapshot {
            entries,
            files,
            fence,
        }
    }

    fn capture_identity(
        source_provenance: [u8; 32],
        environment: [u8; 32],
        toolchain_name: &[u8],
    ) -> CaptureWorkspaceIdentityV2 {
        let package = PackageUrl::parse("pkg:cargo/path-copy-fixture@1.0.0".to_owned())
            .expect("valid test package");
        let invocation = CompilerInvocationRecipeV2::new(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            ContentId::<ToolchainDomain>::from_canonical_bytes(toolchain_name),
            environment,
            [3; 32],
            [4; 32],
        )
        .expect("valid invocation recipe");
        CaptureWorkspaceIdentityV2::new(
            CompilerPackageTargetV2::for_package(package),
            [5; 32],
            invocation,
            source_provenance,
            1024,
        )
    }

    struct InvalidPathSnapshot {
        entries: Vec<CompilerWorkspaceEntryV2>,
        streamed: Cell<bool>,
    }

    impl WorkspaceSnapshotSourceV2 for InvalidPathSnapshot {
        fn entries(&self) -> &[CompilerWorkspaceEntryV2] {
            &self.entries
        }

        fn policy_identity(&self) -> &str {
            "test-policy-v1"
        }

        fn fence_digest(&self) -> [u8; 32] {
            [1; 32]
        }

        fn revalidate(&self) -> Result<bool, String> {
            Ok(true)
        }

        fn stream_file(
            &self,
            _path: &str,
            _max_bytes: u64,
            _chunk_bytes: usize,
            _consume: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<u64, String> {
            self.streamed.set(true);
            Err("invalid path must be rejected before reading".to_owned())
        }
    }

    struct TestStoreDirectory(PathBuf);

    impl TestStoreDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "backend-engine-compiler-capture-v2-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock is after unix epoch")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create test CAS directory");
            Self(path)
        }
    }

    impl Drop for TestStoreDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn large_aggregate_paths_fail_preflight_before_capture_allocations() {
        let prefix = "b".repeat(4_080);
        let mut entries = Vec::with_capacity(5_400);
        entries.push(CompilerWorkspaceEntryV2::directory(""));
        for index in 0..5_399 {
            entries.push(CompilerWorkspaceEntryV2::file(
                format!("{prefix}{index:06}.rs"),
                0,
                CompilerWorkspaceFileRoleV2::Other,
            ));
        }
        assert_eq!(
            validate_inventory(&entries),
            Err(CompilerInputCaptureV2Error::Limit)
        );
    }

    #[test]
    fn invalid_path_is_rejected_before_any_workspace_file_is_streamed() {
        let source = InvalidPathSnapshot {
            entries: vec![
                CompilerWorkspaceEntryV2::directory(""),
                CompilerWorkspaceEntryV2::file(
                    "../secret.rs",
                    1,
                    CompilerWorkspaceFileRoleV2::Source,
                ),
            ],
            streamed: Cell::new(false),
        };
        let package = PackageUrl::parse("pkg:cargo/capture-fixture@1.0.0".to_owned())
            .expect("valid test package");
        let invocation = CompilerInvocationRecipeV2::new(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"test-rustc"),
            [2; 32],
            [3; 32],
            [4; 32],
        )
        .expect("valid invocation recipe");
        let identity = CaptureWorkspaceIdentityV2::new(
            CompilerPackageTargetV2::for_package(package),
            [5; 32],
            invocation,
            [6; 32],
            1024,
        );
        let directory = TestStoreDirectory::new();
        let store = FileStore::open(&directory.0, 1024 * 1024).expect("open test CAS");
        let budget = StreamingClosureBudget::new(8, 1024, 1024, 1024, 8, 4096);

        assert!(matches!(
            capture_full_workspace_v2(&source, identity, &store, budget),
            Err(CompilerInputCaptureV2Error::InvalidInventory)
        ));
        assert!(!source.streamed.get());
    }

    #[test]
    fn production_capture_path_copies_same_keys_and_rebuilds_for_new_paths_or_recipe() {
        let directory = TestStoreDirectory::new();
        let store = FileStore::open(&directory.0, 32 * 1024 * 1024).expect("open test CAS");
        let budget = StreamingClosureBudget::new(
            64,
            8 * 1024 * 1024,
            2 * 1024 * 1024,
            4096,
            4096,
            2 * 1024 * 1024,
        );
        let initial_source =
            workspace_fixture(b"pub fn one() {}", b"pub fn two() {}", None, [8; 32]);
        let (initial, initial_stats) = capture_full_workspace_v2_with_prior(
            &initial_source,
            capture_identity([6; 32], [2; 32], b"path-copy-rustc"),
            &store,
            budget,
            None,
        )
        .expect("capture initial workspace");
        assert_eq!(
            initial_stats.mode,
            CompilerInputCaptureUpdateModeV2::FullBuild
        );

        let edited_source = workspace_fixture(
            b"pub fn one() { let _ = 1; }",
            b"pub fn two() { let _ = 2; }",
            None,
            [9; 32],
        );
        let (edited, edit_stats) = capture_full_workspace_v2_with_prior(
            &edited_source,
            capture_identity([7; 32], [2; 32], b"path-copy-rustc"),
            &store,
            budget,
            Some(&initial),
        )
        .expect("path-copy two source edits");
        assert_eq!(edit_stats.mode, CompilerInputCaptureUpdateModeV2::PathCopy);
        assert_eq!(edit_stats.changed_file_records, Some(2));
        assert!(edit_stats.path_copied_pages > 0);
        assert_eq!(
            edit_stats.source_bytes_read,
            u64::try_from(edited_source.files.values().map(Vec::len).sum::<usize>())
                .expect("source bytes")
                * 2
        );
        assert_eq!(
            edited.manifest().read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );
        assert_eq!(initial.cache_key(), edited.cache_key());

        let (no_op, no_op_stats) = capture_full_workspace_v2_with_prior(
            &edited_source,
            capture_identity([7; 32], [2; 32], b"path-copy-rustc"),
            &store,
            budget,
            Some(&edited),
        )
        .expect("recapture unchanged workspace");
        assert_eq!(no_op_stats.mode, CompilerInputCaptureUpdateModeV2::PathCopy);
        assert_eq!(no_op_stats.changed_file_records, Some(0));
        assert_eq!(no_op_stats.path_copied_pages, 0);
        assert!(no_op_stats.source_bytes_read > 0);
        assert_eq!(
            no_op.manifest().workspace_root(),
            edited.manifest().workspace_root()
        );

        let (cold, cold_stats) = capture_full_workspace_v2_with_prior(
            &edited_source,
            capture_identity([7; 32], [2; 32], b"path-copy-rustc"),
            &store,
            budget,
            None,
        )
        .expect("independent cold full capture");
        assert_eq!(cold_stats.mode, CompilerInputCaptureUpdateModeV2::FullBuild);
        assert_eq!(
            edited.manifest().workspace_root(),
            cold.manifest().workspace_root()
        );
        assert_eq!(edited.manifest().input_root(), cold.manifest().input_root());

        let new_import_source = workspace_fixture(
            b"pub fn one() { let _ = 1; }",
            b"pub fn two() { let _ = 2; }",
            Some(b"pub fn newly_created() {}"),
            [10; 32],
        );
        let (new_import, path_change_stats) = capture_full_workspace_v2_with_prior(
            &new_import_source,
            capture_identity([8; 32], [2; 32], b"path-copy-rustc"),
            &store,
            budget,
            Some(&edited),
        )
        .expect("newly created missing import forces full capture");
        assert_eq!(
            path_change_stats.mode,
            CompilerInputCaptureUpdateModeV2::FullBuild
        );
        assert_ne!(
            new_import.manifest().workspace_root(),
            edited.manifest().workspace_root()
        );
        assert_eq!(
            new_import.manifest().read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );

        let (toolchain_change, recipe_stats) = capture_full_workspace_v2_with_prior(
            &edited_source,
            capture_identity([7; 32], [22; 32], b"path-copy-rustc"),
            &store,
            budget,
            Some(&edited),
        )
        .expect("changed environment forces full build");
        assert_eq!(
            recipe_stats.mode,
            CompilerInputCaptureUpdateModeV2::FullBuild
        );
        assert_ne!(
            toolchain_change.manifest().recipe(),
            edited.manifest().recipe()
        );

        let (_, toolchain_stats) = capture_full_workspace_v2_with_prior(
            &edited_source,
            capture_identity([7; 32], [2; 32], b"different-rustc"),
            &store,
            budget,
            Some(&edited),
        )
        .expect("changed toolchain forces full build");
        assert_eq!(
            toolchain_stats.mode,
            CompilerInputCaptureUpdateModeV2::FullBuild
        );
    }

    #[test]
    fn cold_reopen_revalidates_persisted_capture_and_retains_its_pin() {
        let directory = TestStoreDirectory::new();
        let store = FileStore::open(&directory.0, 1024 * 1024).expect("open test CAS");
        let bytes = b"pub fn captured() -> u8 { 7 }\n".to_vec();
        let source = MemorySnapshot {
            entries: vec![
                CompilerWorkspaceEntryV2::directory(""),
                CompilerWorkspaceEntryV2::directory("src"),
                CompilerWorkspaceEntryV2::file(
                    "src/lib.rs",
                    u64::try_from(bytes.len()).expect("fixture length"),
                    CompilerWorkspaceFileRoleV2::Source,
                ),
            ],
            files: HashMap::from([("src/lib.rs".to_owned(), bytes)]),
            fence: [8; 32],
        };
        let package = PackageUrl::parse("pkg:cargo/cold-reopen-fixture@1.0.0".to_owned())
            .expect("valid test package");
        let invocation = CompilerInvocationRecipeV2::new(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"cold-reopen-rustc"),
            [2; 32],
            [3; 32],
            [4; 32],
        )
        .expect("valid invocation recipe");
        let identity = CaptureWorkspaceIdentityV2::new(
            CompilerPackageTargetV2::for_package(package),
            [5; 32],
            invocation,
            [6; 32],
            1024,
        );
        let budget =
            StreamingClosureBudget::new(32, 1024 * 1024, 1024 * 1024, 4096, 512, 1024 * 1024);
        let capture = capture_full_workspace_v2(&source, identity, &store, budget)
            .expect("capture full fixture");
        let manifest = capture.manifest();
        let work_id = compiler_full_workspace_transfer_work_id(
            manifest.package_lineage(),
            *manifest.package_target().target().as_ref(),
            manifest.recipe(),
            manifest.read_manifest(),
            manifest.input_root(),
            *capture.closure().as_bytes(),
            *capture.manifest_object_id().as_bytes(),
            manifest.max_output_bytes(),
        );
        let expected = CapturedFullWorkspaceV2Expectation {
            input_closure_id: *capture.closure().as_bytes(),
            manifest_object_id: *capture.manifest_object_id().as_bytes(),
            object_count: capture.object_count(),
            payload_bytes: capture.payload_bytes(),
            work_id,
            package_lineage: manifest.package_lineage(),
            target: *manifest.package_target().target().as_ref(),
            recipe: manifest.recipe(),
            input_root: manifest.input_root(),
            read_manifest: manifest.read_manifest(),
            workspace_snapshot_id: manifest.workspace_snapshot_id().to_bytes(),
            max_output_bytes: manifest.max_output_bytes(),
            source_fence_digest: manifest.source_fence_digest(),
            profile: <[u8; 2]>::from(manifest.profile()),
            stage: u8::from(manifest.stage()),
            toolchain: manifest.toolchain(),
            environment: manifest.environment(),
            target_platform: manifest.target_platform(),
        };
        drop(capture);
        let recovered = CapturedFullWorkspaceV2::reopen_pinned_in_store(
            &store,
            expected,
            ArtifactBudget::new(
                1,
                usize::try_from(expected.object_count).expect("object count fits usize"),
                expected.payload_bytes,
                64 * 1024,
                1,
            ),
        )
        .expect("reopen exact capture");
        assert_eq!(recovered.closure().as_bytes(), &expected.input_closure_id);
        assert_eq!(
            recovered.manifest_object_id().as_bytes(),
            &expected.manifest_object_id
        );
        assert_eq!(recovered.object_count(), expected.object_count);
        assert_eq!(recovered.payload_bytes(), expected.payload_bytes);
        assert_eq!(recovered.input_root(), expected.input_root);
        assert_eq!(recovered.manifest().recipe(), expected.recipe);
        assert!(recovered.verify_in_store(&store).is_ok());

        let mut mismatched = expected;
        mismatched.recipe[0] ^= 1;
        assert!(matches!(
            CapturedFullWorkspaceV2::reopen_pinned_in_store(
                &store,
                mismatched,
                ArtifactBudget::new(
                    1,
                    usize::try_from(expected.object_count).expect("object count fits usize"),
                    expected.payload_bytes,
                    64 * 1024,
                    1,
                ),
            ),
            Err(CompilerInputCaptureV2Error::ExpectationMismatch)
        ));

        let mut mismatched_toolchain = expected;
        mismatched_toolchain.toolchain[0] ^= 1;
        assert!(matches!(
            CapturedFullWorkspaceV2::reopen_pinned_in_store(
                &store,
                mismatched_toolchain,
                ArtifactBudget::new(
                    1,
                    usize::try_from(expected.object_count).expect("object count fits usize"),
                    expected.payload_bytes,
                    64 * 1024,
                    1,
                ),
            ),
            Err(CompilerInputCaptureV2Error::ExpectationMismatch)
        ));
    }
}
