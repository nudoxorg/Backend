//! Committed product selection and bounded CAS reads for versioned semantic planes.
//!
//! The workspace root selects the served generation. Turso's selected frontier
//! is a repairable projection and cannot override that root. Logical semantic segment IDs stay separate from physical
//! FileStore object IDs, so the same cursor works across storage layouts.

use super::{BuiltinModelError, SemanticAuthority};
use backend_engine::builtin::{ProductSemanticPublicationKey, SemanticPublicationClaim};
use backend_extension_turso::{
    ReopenedCompilerMetadata, SelectedGeneration, VERSIONED_PLANE_SEGMENT_SCHEMA,
    VersionedPlaneMetadata,
};
use backend_replication::{
    ByteRange, IrHydrationRequest, SelectedGenerationSource, SelectedGenerationStamp,
    SelectedNativeHistoryBinding, SelectedNativeHistoryImage, SelectedNativeImagePublicationFence,
    SelectedNativeImageSource, SelectedTypedV3HistoryError, SemanticCatalogChunk,
    SemanticCatalogGet, SemanticManifestChunk, SemanticManifestGet, SemanticTargetKey,
};
use backend_replication::{
    FileSemanticRangeStore, HistoryCommitId, HistoryRefName, TransportLimits,
};
use backend_semantic::ir::{
    JumboRopeLimits, SemanticImageIdentity, SemanticPlaneCatalog, SemanticPlaneImageKey,
    SemanticPlaneManifest, SemanticPlaneSegmentBoundaryPolicy, SemanticRangeRequest,
    SemanticTypedPlaneVerificationTierV2,
};
use backend_store::{ArtifactBudget, FileStore, UntrustedObjectId};
use core::fmt;
use hashlink::LruCache;
use std::cell::RefCell;
use std::sync::{Arc, Mutex, RwLockReadGuard};

const MAX_RANGE_BYTES: u64 = 16 * 1024;
const MAX_SEGMENT_BYTES: u64 = backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const VERIFIED_SEGMENT_CACHE_BYTES: usize = 32 * 1024 * 1024;
// Byte bounds alone do not bound a cache of tiny semantic segments: every
// entry also retains a hash-table entry, LRU links, Arc, and object key.
const VERIFIED_SEGMENT_CACHE_ENTRIES: usize = 4_096;
const OBJECT_READ_CHUNK_BYTES: usize = 16 * 1024;

/// A worker-safe view of one selected-closure image range route. It retains
/// only immutable storage handles and the existing bounded reader caches; the
/// mutable Turso authority remains on the owner thread.
pub(super) struct SelectedClosureImageRangeReader {
    store: FileStore,
    local_readers: Arc<super::selected_full_image::VerifiedLocalImageReaderCache>,
    remote: Option<Arc<dyn super::s3_publication::SelectedClosurePublisher>>,
}

impl SelectedClosureImageRangeReader {
    #[must_use]
    pub(super) fn new(
        store: FileStore,
        local_readers: Arc<super::selected_full_image::VerifiedLocalImageReaderCache>,
        remote: Option<Arc<dyn super::s3_publication::SelectedClosurePublisher>>,
    ) -> Self {
        Self {
            store,
            local_readers,
            remote,
        }
    }

    /// Fills one exact bounded range from the admitted local object or the
    /// selected closure's verified remote route.
    pub(super) fn read_range_into(
        &self,
        plan: &super::selected_full_image::SelectedFullImagePlan,
        range: ByteRange,
        output: &mut [u8],
    ) -> Result<usize, SelectedImageRangeReadError> {
        let output_len = u64::try_from(output.len()).map_err(|_| {
            SelectedImageRangeReadError::Refused(
                "selected image range buffer is too large".to_owned(),
            )
        })?;
        if range.len == 0
            || range.len > MAX_RANGE_BYTES
            || range.len != output_len
            || range.end().ok().is_none_or(|end| end > plan.total_length)
        {
            return Err(SelectedImageRangeReadError::Refused(
                "selected image range is empty, oversized, or outside its admitted plan".to_owned(),
            ));
        }

        match self
            .local_readers
            .read_range_into(&self.store, plan, range, output)
        {
            Ok(Some(count)) if count == output.len() => return Ok(count),
            Ok(Some(_)) => {
                return Err(SelectedImageRangeReadError::Refused(
                    "local selected image range returned the wrong byte count".to_owned(),
                ));
            }
            Ok(None) => {}
            Err(super::selected_full_image::SelectedImageLocalReadError::CorruptEnvelope(_)) => {}
            Err(
                super::selected_full_image::SelectedImageLocalReadError::UnsafePath(reason)
                | super::selected_full_image::SelectedImageLocalReadError::AuthorityMismatch(reason),
            ) => {
                return Err(SelectedImageRangeReadError::Refused(reason));
            }
            Err(super::selected_full_image::SelectedImageLocalReadError::Storage(reason)) => {
                return Err(SelectedImageRangeReadError::Deferred(reason));
            }
        }

        let Some(remote) = self.remote.as_deref() else {
            return Err(SelectedImageRangeReadError::Deferred(
                "selected semantic image is cold locally and no remote range route is configured"
                    .to_owned(),
            ));
        };
        let payload = remote
            .hydrate_object_range(
                &self.store,
                plan.remote_selection,
                UntrustedObjectId::from_bytes(plan.object_id),
                backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA,
                plan.total_length,
                range.start,
                range.len,
            )
            .map_err(SelectedImageRangeReadError::from_remote)?;
        if payload.len() != output.len() {
            return Err(SelectedImageRangeReadError::Refused(
                "selected remote image range returned the wrong byte count".to_owned(),
            ));
        }
        output.copy_from_slice(&payload);
        Ok(output.len())
    }
}

#[derive(Debug)]
pub(super) enum SelectedImageRangeReadError {
    Deferred(String),
    Refused(String),
}

impl SelectedImageRangeReadError {
    fn from_remote(error: super::s3_publication::PublicationError) -> Self {
        use super::s3_publication::PublicationError;

        let detail = format!("{error:?}");
        match error {
            PublicationError::Configuration | PublicationError::Receipt => Self::Refused(
                "selected remote image route failed its immutable admission".to_owned(),
            ),
            PublicationError::RemoteStore(error)
            | PublicationError::RemoteHydration {
                source: Some(error),
                ..
            } => {
                let detail = error.to_string();
                match error {
                    backend_store_s3::RemoteStoreError::Unavailable
                    | backend_store_s3::RemoteStoreError::Store(backend_store::StoreError::Io(_)) => {
                        Self::Deferred(format!(
                            "selected remote image range is unavailable: {detail}"
                        ))
                    }
                    backend_store_s3::RemoteStoreError::Capability
                    | backend_store_s3::RemoteStoreError::StaleFence
                    | backend_store_s3::RemoteStoreError::Bounds
                    | backend_store_s3::RemoteStoreError::Identity
                    | backend_store_s3::RemoteStoreError::Protocol
                    | backend_store_s3::RemoteStoreError::ExistingUnverified
                    | backend_store_s3::RemoteStoreError::Store(_) => Self::Refused(format!(
                        "selected remote image range failed immutable identity or scope validation: {detail}"
                    )),
                }
            }
            PublicationError::RemoteHydration {
                operation,
                source: None,
            } => Self::Refused(format!(
                "selected remote image range lost typed failure provenance during {operation:?}"
            )),
            PublicationError::Remote => Self::Refused(format!(
                "selected remote image route failed without a retryable store error: {detail}"
            )),
            PublicationError::Store | PublicationError::ReceiptIo => Self::Deferred(format!(
                "selected remote image range is temporarily unavailable: {detail}"
            )),
        }
    }
}

/// Bounded process-local cache of bytes admitted from immutable CAS objects.
///
/// The physical object ID is the reuse key: identical content is useful across
/// selected generations. A hit is still checked against the *current committed
/// product* selection and its exact logical segment identity before any bytes
/// escape.
#[derive(Debug, Default)]
pub(super) struct VerifiedSegmentCache {
    state: Mutex<VerifiedSegmentCacheState>,
}

#[derive(Debug)]
struct VerifiedSegmentCacheState {
    entries: LruCache<[u8; 32], VerifiedSegment>,
    resident_bytes: usize,
}

impl Default for VerifiedSegmentCacheState {
    fn default() -> Self {
        Self {
            entries: LruCache::new(VERIFIED_SEGMENT_CACHE_ENTRIES),
            resident_bytes: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct VerifiedSegment {
    bytes: Arc<[u8]>,
    segment_id: [u8; 32],
    plane: backend_semantic::ir::SemanticPlaneKind,
}

impl VerifiedSegmentCache {
    fn get(&self, object_id: [u8; 32]) -> Option<VerifiedSegment> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // LruCache promotes by hash lookup and linked-node splice. This clone
        // shares the resident payload Arc and occurs only on a cache hit.
        state.entries.get(&object_id).cloned()
    }

    fn admit(&self, object_id: [u8; 32], segment: VerifiedSegment) {
        let len = segment.bytes.len();
        if len > VERIFIED_SEGMENT_CACHE_BYTES {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Another request may have admitted the same content while this one
        // read it. Keep the first verified resident copy and avoid double charge.
        if state.entries.contains_key(&object_id) {
            return;
        }
        while state.resident_bytes.saturating_add(len) > VERIFIED_SEGMENT_CACHE_BYTES {
            let Some((_, evicted)) = state.entries.remove_lru() else {
                return;
            };
            state.resident_bytes -= evicted.bytes.len();
        }
        while state.entries.len() >= VERIFIED_SEGMENT_CACHE_ENTRIES {
            let Some((_, evicted)) = state.entries.remove_lru() else {
                return;
            };
            state.resident_bytes -= evicted.bytes.len();
        }
        state.resident_bytes += len;
        state.entries.insert(object_id, segment);
    }
}

/// Metadata-only view of one exact product-selected compiler generation.
#[derive(Clone, Debug)]
pub(super) struct SelectedVersionedPlanePublication {
    stamp: SelectedGenerationStamp,
    metadata: VersionedPlaneMetadata,
    environment: [u8; 32],
    target_platform: [u8; 32],
    storage_selection: Option<super::s3_publication::RemoteClosureSelection>,
}

impl SelectedVersionedPlanePublication {
    /// Binds exact Turso metadata to the key and selected compiler generation.
    pub(super) fn from_reopened(
        key: &ProductSemanticPublicationKey,
        selected: &SelectedGeneration,
        reopened: &ReopenedCompilerMetadata,
    ) -> Result<Self, VersionedPlaneServiceError> {
        if !reopened.envelope().matches_selected(selected)
            || selected.namespace().package() != key.package().as_str()
            || selected.namespace().source() != key.coordinate().as_str()
        {
            return Err(VersionedPlaneServiceError::SelectionMismatch);
        }
        let selected_catalog_root = selected
            .semantic_catalog_root()
            .copied()
            .ok_or(VersionedPlaneServiceError::MissingManifest)?;
        let versioned = reopened
            .metadata()
            .versioned_planes()
            .ok_or(VersionedPlaneServiceError::MissingManifest)?;
        if versioned.catalog_root().as_bytes() != &selected_catalog_root {
            return Err(VersionedPlaneServiceError::ManifestMismatch);
        }
        let mut selected_build = None;
        for artifact in versioned.artifacts() {
            let manifest = SemanticPlaneManifest::decode(artifact.manifest_bytes())
                .map_err(|error| VersionedPlaneServiceError::Manifest(error.to_string()))?;
            if manifest.root() != artifact.image_key().manifest_root()
                || manifest.semantic_generation() != artifact.image_key().semantic_generation()
                || manifest.build().profile() != key.profile()
            {
                return Err(VersionedPlaneServiceError::ManifestMismatch);
            }
            let build = (
                *manifest.build().environment(),
                *manifest.build().target_platform(),
            );
            if selected_build.is_some_and(|existing| existing != build) {
                return Err(VersionedPlaneServiceError::ManifestMismatch);
            }
            selected_build = Some(build);
        }
        let (environment, target_platform) =
            selected_build.ok_or(VersionedPlaneServiceError::MissingManifest)?;
        let stamp = SelectedGenerationStamp::checked(
            selected.namespace().namespace_id(),
            key.profile(),
            coordinate_identity(key.coordinate().as_str()),
            selected.generation(),
            *selected.target_root(),
            *selected.closure_id(),
            versioned.catalog_root(),
        )
        .map_err(|error| VersionedPlaneServiceError::Selection(error.to_string()))?;
        Ok(Self {
            stamp,
            metadata: versioned.clone(),
            environment,
            target_platform,
            storage_selection: Some(
                super::s3_publication::RemoteClosureSelection::from_selected(selected),
            ),
        })
    }

    /// Exact stamp built from the committed product selection.
    #[must_use]
    pub(super) const fn stamp(&self) -> SelectedGenerationStamp {
        self.stamp
    }

    /// Reopens the exact manifest named by one selected catalog image key.
    pub(super) fn manifest(
        &self,
        image: SemanticPlaneImageKey,
    ) -> Result<SemanticPlaneManifest, VersionedPlaneServiceError> {
        let artifact = self
            .metadata
            .artifact_for_image(image)
            .ok_or(VersionedPlaneServiceError::MissingImage)?;
        let manifest = SemanticPlaneManifest::decode(artifact.manifest_bytes())
            .map_err(|error| VersionedPlaneServiceError::Manifest(error.to_string()))?;
        if manifest.root() != image.manifest_root()
            || manifest.semantic_generation() != image.semantic_generation()
        {
            return Err(VersionedPlaneServiceError::ManifestMismatch);
        }
        Ok(manifest)
    }

    /// Exact logical segment ID to physical object references.
    #[must_use]
    pub(super) const fn metadata(&self) -> &VersionedPlaneMetadata {
        &self.metadata
    }
}

/// Live resolver for the selected semantic generation and its closure-backed
/// plane inventory.
pub(super) trait VersionedPlaneSelectionResolver: SelectedGenerationSource {
    /// Re-reads the committed product selection and its exact metadata.
    fn current_selected_plane(&mut self) -> Result<SelectedVersionedPlanePublication, Self::Error>;
}

/// Per-operation adapter from local Turso authority to versioned plane
/// selection and the replication freshness contract.
pub(super) struct SemanticAuthoritySelectionSource<'authority> {
    authority: &'authority SemanticAuthority,
    key: ProductSemanticPublicationKey,
}

/// Owned read-only view for background history publication. It shares the
/// exact selector lock used by the marker writer, but does not retain the
/// mutable Turso authority or any owner-thread state.
pub(super) struct OwnedSemanticAuthoritySelectionSource {
    loader: Arc<super::semantic_authority::SelectedClosureImageLoader>,
    store: FileStore,
    key: ProductSemanticPublicationKey,
}

impl OwnedSemanticAuthoritySelectionSource {
    #[must_use]
    pub(super) fn new(
        loader: Arc<super::semantic_authority::SelectedClosureImageLoader>,
        store: FileStore,
        key: ProductSemanticPublicationKey,
    ) -> Self {
        Self { loader, store, key }
    }

    fn target(&self) -> Result<SemanticTargetKey, BuiltinModelError> {
        SemanticTargetKey::new(
            self.key.package().as_str(),
            self.key.coordinate().as_str(),
            self.key.profile(),
        )
        .map_err(|error| BuiltinModelError(format!("admit semantic target: {error}")))
    }

    fn current_selection(
        &self,
    ) -> Result<
        (
            SemanticPublicationClaim,
            backend_extension_turso::SelectedGeneration,
            SelectedVersionedPlanePublication,
        ),
        OwnedSemanticAuthoritySelectionError,
    > {
        let selections = self.loader.acquire_publication_read()?;
        let Some((claim, selected)) =
            super::semantic_authority::SelectedClosureImageLoader::committed_pair_optional_in(
                &selections,
                &self.key,
            )?
        else {
            return Err(OwnedSemanticAuthoritySelectionError::StaleSelection);
        };
        let publication =
            SemanticAuthority::selected_plane_for_store(&self.store, &self.key, claim, &selected)?;
        Ok((claim, selected, publication))
    }
}

#[derive(Debug)]
pub(super) enum OwnedSemanticAuthoritySelectionError {
    Authority(BuiltinModelError),
    StaleSelection,
}

impl From<BuiltinModelError> for OwnedSemanticAuthoritySelectionError {
    fn from(error: BuiltinModelError) -> Self {
        Self::Authority(error)
    }
}

impl fmt::Display for OwnedSemanticAuthoritySelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority(error) => fmt::Display::fmt(error, formatter),
            Self::StaleSelection => {
                formatter.write_str("committed semantic product selection moved")
            }
        }
    }
}

pub(super) struct OwnedSemanticAuthorityPublicationFence<'a> {
    _selections: RwLockReadGuard<'a, super::semantic_authority::SelectedClosureSnapshot>,
    target: SemanticTargetKey,
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
}

impl SelectedNativeImagePublicationFence for OwnedSemanticAuthorityPublicationFence<'_> {
    fn selected_target(&self) -> &SemanticTargetKey {
        &self.target
    }

    fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.stamp
    }

    fn selected_image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    fn selected_image_identity(&self) -> SemanticImageIdentity {
        self.image_identity
    }
}

impl<'authority> SemanticAuthoritySelectionSource<'authority> {
    /// Binds one product target to the live local semantic authority.
    #[must_use]
    pub(super) fn new(
        authority: &'authority SemanticAuthority,
        key: ProductSemanticPublicationKey,
    ) -> Self {
        Self { authority, key }
    }

    /// Returns the canonical product target bound by this selected source.
    pub(super) fn target(&self) -> Result<SemanticTargetKey, BuiltinModelError> {
        SemanticTargetKey::new(
            self.key.package().as_str(),
            self.key.coordinate().as_str(),
            self.key.profile(),
        )
        .map_err(|error| BuiltinModelError(format!("admit semantic target: {error}")))
    }
}

impl SelectedGenerationSource for SemanticAuthoritySelectionSource<'_> {
    type Error = BuiltinModelError;

    fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
        self.current_selected_plane()
            .map(|publication| publication.stamp())
    }

    fn selected_image_is_current(
        &mut self,
        expected_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<bool, Self::Error> {
        let current = self.current_selected_plane()?;
        Ok(current.stamp() == expected_stamp
            && current.metadata().artifact_for_image(image).is_some())
    }
}

impl SelectedNativeImageSource for SemanticAuthoritySelectionSource<'_> {
    type PublicationFence<'fence>
        = SemanticAuthorityPublicationFence<'fence>
    where
        Self: 'fence;

    fn selected_semantic_target(&mut self) -> Result<SemanticTargetKey, Self::Error> {
        self.target()
    }

    fn selected_native_image_identity(
        &mut self,
        image: SemanticPlaneImageKey,
    ) -> Result<backend_semantic::ir::SemanticImageIdentity, Self::Error> {
        self.authority
            .selected_native_image_identity(&self.key, image)
    }

    fn acquire_publication_fence<'fence>(
        &'fence mut self,
        selected: &SelectedNativeHistoryImage<'_>,
    ) -> Result<Self::PublicationFence<'fence>, Self::Error> {
        let target = self.target()?;
        if selected.target() != &target {
            return Err(BuiltinModelError(
                "typed V3 history target differs from its committed product key".to_owned(),
            ));
        }
        let lease = self.authority.committed_selection_lease(&self.key)?;
        let publication = lease.selected_plane()?;
        let image = selected.image_key();
        if publication.stamp() != selected.selected_stamp()
            || publication.metadata().artifact_for_image(image).is_none()
        {
            return Err(BuiltinModelError(
                "typed V3 history image is no longer the committed product selection".to_owned(),
            ));
        }
        let image_identity = lease.selected_native_image_identity(image)?;
        if image_identity != selected.image_identity() {
            return Err(BuiltinModelError(
                "typed V3 history image identity differs from the committed product selection"
                    .to_owned(),
            ));
        }
        Ok(SemanticAuthorityPublicationFence {
            _lease: lease,
            target,
            stamp: publication.stamp(),
            image,
            image_identity,
        })
    }
}

pub(super) struct SemanticAuthorityPublicationFence<'a> {
    _lease: super::semantic_authority::CommittedSemanticSelectionLease<'a>,
    target: SemanticTargetKey,
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
}

impl SelectedNativeImagePublicationFence for SemanticAuthorityPublicationFence<'_> {
    fn selected_target(&self) -> &SemanticTargetKey {
        &self.target
    }

    fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.stamp
    }

    fn selected_image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    fn selected_image_identity(&self) -> SemanticImageIdentity {
        self.image_identity
    }
}

impl VersionedPlaneSelectionResolver for SemanticAuthoritySelectionSource<'_> {
    fn current_selected_plane(&mut self) -> Result<SelectedVersionedPlanePublication, Self::Error> {
        self.authority.resolve_current_selected(&self.key)
    }
}

impl SelectedGenerationSource for OwnedSemanticAuthoritySelectionSource {
    type Error = OwnedSemanticAuthoritySelectionError;

    fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
        self.current_selection()
            .map(|(_, _, publication)| publication.stamp())
    }

    fn selected_image_is_current(
        &mut self,
        expected_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<bool, Self::Error> {
        let (claim, selected, publication) = self.current_selection()?;
        if publication.stamp() != expected_stamp
            || publication.metadata().artifact_for_image(image).is_none()
        {
            return Ok(false);
        }
        SemanticAuthority::selected_native_image_identity_for_store(
            &self.store,
            &self.key,
            claim,
            &selected,
            image,
        )?;
        Ok(true)
    }
}

impl SelectedNativeImageSource for OwnedSemanticAuthoritySelectionSource {
    type PublicationFence<'fence>
        = OwnedSemanticAuthorityPublicationFence<'fence>
    where
        Self: 'fence;

    fn classify_selection_error(
        error: &Self::Error,
    ) -> backend_replication::SelectedNativeImageSourceFailure {
        match error {
            OwnedSemanticAuthoritySelectionError::StaleSelection => {
                backend_replication::SelectedNativeImageSourceFailure::StaleSelection
            }
            OwnedSemanticAuthoritySelectionError::Authority(_) => {
                backend_replication::SelectedNativeImageSourceFailure::Refused
            }
        }
    }

    fn selected_semantic_target(&mut self) -> Result<SemanticTargetKey, Self::Error> {
        self.target().map_err(Into::into)
    }

    fn selected_native_image_identity(
        &mut self,
        image: SemanticPlaneImageKey,
    ) -> Result<SemanticImageIdentity, Self::Error> {
        let (claim, selected, publication) = self.current_selection()?;
        if publication.metadata().artifact_for_image(image).is_none() {
            return Err(OwnedSemanticAuthoritySelectionError::StaleSelection);
        }
        SemanticAuthority::selected_native_image_identity_for_store(
            &self.store,
            &self.key,
            claim,
            &selected,
            image,
        )
        .map_err(Into::into)
    }

    fn acquire_publication_fence<'fence>(
        &'fence mut self,
        selected_image: &SelectedNativeHistoryImage<'_>,
    ) -> Result<Self::PublicationFence<'fence>, Self::Error> {
        let target = self
            .target()
            .map_err(OwnedSemanticAuthoritySelectionError::from)?;
        if selected_image.target() != &target {
            return Err(OwnedSemanticAuthoritySelectionError::Authority(
                BuiltinModelError(
                    "typed V3 history target differs from its committed product key".to_owned(),
                ),
            ));
        }
        let selections = self.loader.acquire_publication_read()?;
        let Some((claim, selected)) =
            super::semantic_authority::SelectedClosureImageLoader::committed_pair_optional_in(
                &selections,
                &self.key,
            )?
        else {
            return Err(OwnedSemanticAuthoritySelectionError::StaleSelection);
        };
        let publication =
            SemanticAuthority::selected_plane_for_store(&self.store, &self.key, claim, &selected)?;
        let image = selected_image.image_key();
        if publication.stamp() != selected_image.selected_stamp()
            || publication.metadata().artifact_for_image(image).is_none()
        {
            return Err(OwnedSemanticAuthoritySelectionError::StaleSelection);
        }
        let image_identity = SemanticAuthority::selected_native_image_identity_for_store(
            &self.store,
            &self.key,
            claim,
            &selected,
            image,
        )?;
        if image_identity != selected_image.image_identity() {
            return Err(OwnedSemanticAuthoritySelectionError::StaleSelection);
        }
        #[cfg(test)]
        self.loader.wait_at_native_history_fence_gate();
        Ok(OwnedSemanticAuthorityPublicationFence {
            _selections: selections,
            target,
            stamp: publication.stamp(),
            image,
            image_identity,
        })
    }
}

enum NativeHistoryPublicationError {
    Superseded,
    Deferred(String),
    Refused(String),
}

fn committed_native_history_pair(
    loader: &super::semantic_authority::SelectedClosureImageLoader,
    key: &ProductSemanticPublicationKey,
) -> Result<Option<(SemanticPublicationClaim, SelectedGeneration)>, NativeHistoryPublicationError> {
    let selections = loader.acquire_publication_read().map_err(|error| {
        NativeHistoryPublicationError::Refused(format!(
            "read committed semantic selection for native history: {error}"
        ))
    })?;
    super::semantic_authority::SelectedClosureImageLoader::committed_pair_optional_in(
        &selections,
        key,
    )
    .map_err(|error| {
        NativeHistoryPublicationError::Refused(format!(
            "validate committed semantic selection projection for native history: {error}"
        ))
    })
}

fn require_committed_native_history_pair(
    loader: &super::semantic_authority::SelectedClosureImageLoader,
    key: &ProductSemanticPublicationKey,
) -> Result<(SemanticPublicationClaim, SelectedGeneration), NativeHistoryPublicationError> {
    committed_native_history_pair(loader, key)?.ok_or(NativeHistoryPublicationError::Superseded)
}

fn ensure_native_history_selection(
    loader: &super::semantic_authority::SelectedClosureImageLoader,
    key: &ProductSemanticPublicationKey,
    expected_claim: SemanticPublicationClaim,
    expected_stamp: SelectedGenerationStamp,
) -> Result<(), NativeHistoryPublicationError> {
    let Some((claim, selected)) = committed_native_history_pair(loader, key)? else {
        return Err(NativeHistoryPublicationError::Superseded);
    };
    if claim != expected_claim {
        return Err(NativeHistoryPublicationError::Superseded);
    }
    let stamp = SemanticAuthority::selected_generation_stamp(key, &selected)
        .map_err(|error| NativeHistoryPublicationError::Refused(error.0))?;
    if stamp != expected_stamp {
        return Err(NativeHistoryPublicationError::Superseded);
    }
    Ok(())
}

fn map_typed_history_publication_error(
    error: SelectedTypedV3HistoryError,
) -> NativeHistoryPublicationError {
    match error {
        SelectedTypedV3HistoryError::StaleSelection => NativeHistoryPublicationError::Superseded,
        SelectedTypedV3HistoryError::RetryableAvailability { operation, detail } => {
            NativeHistoryPublicationError::Deferred(format!("{operation:?}: {detail}"))
        }
        SelectedTypedV3HistoryError::Refused {
            operation,
            cause,
            detail,
        } => NativeHistoryPublicationError::Refused(format!(
            "{operation:?} refused ({cause:?}): {detail}"
        )),
    }
}

struct NativeHistoryPublicationReceipt {
    commit: HistoryCommitId,
    proof: backend_engine::SemanticHistoryPublicationProof,
}

pub(super) fn publish_native_history(
    work: super::semantic_authority::NativeHistoryPublicationWork,
) -> backend_engine::SemanticHistoryPublicationStatus {
    let selection_id = work.selection_id;
    match publish_native_history_commit(work) {
        Ok(commit) => backend_engine::SemanticHistoryPublicationStatus::Published {
            selection_id,
            commit: *commit.commit.as_bytes(),
            reference: "selected-native-v3".to_owned(),
            proof: commit.proof,
        },
        Err(NativeHistoryPublicationError::Superseded) => {
            backend_engine::SemanticHistoryPublicationStatus::Superseded { selection_id }
        }
        Err(NativeHistoryPublicationError::Deferred(reason)) => {
            backend_engine::SemanticHistoryPublicationStatus::Deferred {
                selection_id,
                reason,
            }
        }
        Err(NativeHistoryPublicationError::Refused(reason)) => {
            backend_engine::SemanticHistoryPublicationStatus::Refused {
                selection_id,
                reason,
            }
        }
    }
}

fn publish_native_history_commit(
    work: super::semantic_authority::NativeHistoryPublicationWork,
) -> Result<NativeHistoryPublicationReceipt, NativeHistoryPublicationError> {
    let (claim, selected) = require_committed_native_history_pair(&work.loader, &work.key)?;
    if claim != work.expected_claim {
        return Err(NativeHistoryPublicationError::Superseded);
    }
    let stamp = SemanticAuthority::selected_generation_stamp(&work.key, &selected)
        .map_err(|error| NativeHistoryPublicationError::Refused(error.0))?;
    if stamp != work.stamp {
        return Err(NativeHistoryPublicationError::Superseded);
    }
    let publication =
        SemanticAuthority::selected_plane_for_store(&work.store, &work.key, claim, &selected)
            .map_err(|error| NativeHistoryPublicationError::Refused(error.0))?;
    let metadata = publication.metadata();
    let catalog_len = usize::try_from(metadata.catalog_len()).map_err(|_| {
        NativeHistoryPublicationError::Refused("selected catalog length exceeds usize".to_owned())
    })?;
    const MAX_NATIVE_CATALOG_BYTES: usize = 4 * 1024 * 1024;
    if catalog_len == 0 || catalog_len > MAX_NATIVE_CATALOG_BYTES {
        return Err(NativeHistoryPublicationError::Refused(
            "selected image catalog exceeds the bounded publication limit".to_owned(),
        ));
    }
    let mut catalog_bytes = Vec::new();
    catalog_bytes.try_reserve_exact(catalog_len).map_err(|_| {
        NativeHistoryPublicationError::Refused("selected catalog allocation failed".to_owned())
    })?;
    catalog_bytes.resize(catalog_len, 0);
    let mut catalog_offset = 0_usize;
    while catalog_offset < catalog_len {
        let chunk_len = (catalog_len - catalog_offset).min(16 * 1024);
        let end = catalog_offset.checked_add(chunk_len).ok_or_else(|| {
            NativeHistoryPublicationError::Refused("selected catalog range overflow".to_owned())
        })?;
        let output = catalog_bytes.get_mut(catalog_offset..end).ok_or_else(|| {
            NativeHistoryPublicationError::Refused(
                "selected catalog range is outside its buffer".to_owned(),
            )
        })?;
        metadata
            .write_catalog_range(catalog_offset as u64, output)
            .map_err(|error| {
                NativeHistoryPublicationError::Refused(format!(
                    "read selected canonical catalog: {error}"
                ))
            })?;
        catalog_offset = end;
    }
    let catalog = SemanticPlaneCatalog::decode(&catalog_bytes).map_err(|error| {
        NativeHistoryPublicationError::Refused(format!(
            "decode selected canonical catalog: {error}"
        ))
    })?;
    if catalog.root() != metadata.catalog_root() || catalog.root() != stamp.catalog_root() {
        return Err(NativeHistoryPublicationError::Refused(
            "selected canonical catalog root differs from its marker stamp".to_owned(),
        ));
    }
    let artifacts = metadata.artifacts();
    if artifacts.len() != 1 {
        return Err(NativeHistoryPublicationError::Refused(
            "typed V3 publication requires exactly one selected semantic image".to_owned(),
        ));
    }
    let image_key = artifacts[0].image_key();
    let manifest = publication
        .manifest(image_key)
        .map_err(|error| NativeHistoryPublicationError::Refused(error.to_string()))?;
    let mut source = OwnedSemanticAuthoritySelectionSource::new(
        Arc::clone(&work.loader),
        work.store.clone(),
        work.key.clone(),
    );
    let binding =
        match SelectedNativeHistoryBinding::bind(&mut source, catalog, image_key, manifest) {
            Ok(binding) => binding,
            Err(reason) => {
                ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)?;
                return Err(NativeHistoryPublicationError::Refused(reason));
            }
        };
    if binding.selected_stamp() != work.stamp {
        return Err(NativeHistoryPublicationError::Superseded);
    }
    let branch = HistoryRefName::new("selected-native-v3")
        .map_err(NativeHistoryPublicationError::Refused)?;
    let mut limits = TransportLimits::default();
    limits.max_chunk = 16 * 1024;
    let history = FileSemanticRangeStore::open(work.store.clone(), limits)
        .map_err(NativeHistoryPublicationError::Refused)?;
    if let Some(commit) = history
        .selected_typed_v3_history_branch_current(&binding, &branch)
        .map_err(NativeHistoryPublicationError::Refused)?
    {
        let ancestry = history
            .history_ref_ancestry_proof(
                binding.target(),
                backend_replication::HistoryRefKind::Branch,
                &branch,
                commit,
            )
            .map_err(NativeHistoryPublicationError::Refused)?;
        let replay = history
            .replay_typed_v3_history(
                binding.target(),
                backend_replication::HistoryRefKind::Branch,
                &branch,
                commit,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .map_err(NativeHistoryPublicationError::Refused)?;
        if replay.commit().identity() != commit
            || replay.input_replay_status()
                != backend_replication::TypedV3HistoryInputReplayStatus::Unproven
            || ancestry.ancestor() != commit
        {
            return Err(NativeHistoryPublicationError::Refused(
                "cold typed V3 replay did not verify the exact selected commit as unproven input"
                    .to_owned(),
            ));
        }
        ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)?;
        let proof = native_history_publication_proof(
            binding.selected_stamp(),
            binding.image_key(),
            binding.image_identity(),
            ancestry.ref_tip(),
            ancestry.ancestor(),
            replay.commit().parents(),
        )?;
        return Ok(NativeHistoryPublicationReceipt { commit, proof });
    }
    let plan = SemanticAuthority::selected_full_image_plan_for_store(
        &work.store,
        &work.key,
        claim,
        image_key,
        &selected,
        work.stamp,
    )
    .map_err(|error| NativeHistoryPublicationError::Refused(error.0))?;
    if plan.total_length == 0 || plan.total_length > backend_replication::MAX_SEMANTIC_IMAGE_BYTES {
        return Err(NativeHistoryPublicationError::Refused(
            "selected image exceeds the 128 MiB publication bound".to_owned(),
        ));
    }
    ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)?;
    let max_image_bytes =
        usize::try_from(backend_replication::MAX_SEMANTIC_IMAGE_BYTES).map_err(|_| {
            NativeHistoryPublicationError::Refused(
                "selected image byte bound does not fit this process".to_owned(),
            )
        })?;
    let selection_error = RefCell::new(None);
    let (mapped_image, range_metrics) = backend_semantic::ir::load_semantic_image_mmap_from_ranges(
        plan.total_length,
        plan.identity,
        image_key.semantic_generation(),
        max_image_bytes,
        |offset, output| {
            if let Err(error) =
                ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)
            {
                *selection_error.borrow_mut() = Some(error);
                return Err(SelectedImageRangeReadError::Refused(
                    "failed to revalidate committed product selection during selected image read"
                        .to_owned(),
                ));
            }
            let range_len = u64::try_from(output.len()).map_err(|_| {
                SelectedImageRangeReadError::Refused(
                    "selected image range length exceeds u64".to_owned(),
                )
            })?;
            let range = ByteRange::new(offset, range_len).map_err(|error| {
                SelectedImageRangeReadError::Refused(format!(
                    "construct selected image range: {error}"
                ))
            })?;
            work.image_ranges.read_range_into(&plan, range, output)
        },
        || match ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp) {
            Ok(()) => false,
            Err(error) => {
                *selection_error.borrow_mut() = Some(error);
                true
            }
        },
    )
    .map_err(|error| match error {
        backend_semantic::ir::MappedSemanticImageRangeError::Cancelled => selection_error
            .borrow_mut()
            .take()
            .unwrap_or(NativeHistoryPublicationError::Superseded),
        backend_semantic::ir::MappedSemanticImageRangeError::Read { source, .. } => {
            if let Some(error) = selection_error.borrow_mut().take() {
                error
            } else {
                match ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp) {
                    Err(error) => error,
                    Ok(()) => match source {
                        SelectedImageRangeReadError::Deferred(reason) => {
                            NativeHistoryPublicationError::Deferred(reason)
                        }
                        SelectedImageRangeReadError::Refused(reason) => {
                            NativeHistoryPublicationError::Refused(reason)
                        }
                    },
                }
            }
        }
        backend_semantic::ir::MappedSemanticImageRangeError::ShortRead {
            offset,
            expected,
            observed,
        } => NativeHistoryPublicationError::Refused(format!(
            "selected image range at {offset} returned {observed} of {expected} bytes"
        )),
        backend_semantic::ir::MappedSemanticImageRangeError::Mapping(error) => {
            let detail = error.to_string();
            match error {
                backend_semantic::ir::MappedSemanticImageError::Io { .. } => {
                    NativeHistoryPublicationError::Deferred(format!(
                        "allocate or admit selected image mapping: {detail}"
                    ))
                }
                _ => NativeHistoryPublicationError::Refused(format!(
                    "admit mapped selected semantic image: {detail}"
                )),
            }
        }
    })?;
    if range_metrics.bytes_read != plan.total_length
        || range_metrics.identity_hash_bytes != plan.total_length
    {
        return Err(NativeHistoryPublicationError::Refused(
            "selected image mapping did not account for its exact byte extent".to_owned(),
        ));
    }
    let selected_image = match binding.bind_mapped_image(&mapped_image) {
        Ok(selected_image) => selected_image,
        Err(reason) => {
            ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)?;
            return Err(NativeHistoryPublicationError::Refused(reason));
        }
    };
    let policy = SemanticPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
        .map_err(|error| NativeHistoryPublicationError::Refused(error.to_string()))?;
    let policies = backend_replication::SemanticTypedPlaneBoundaryPoliciesV3::new(
        policy, policy, policy, policy, policy, policy, policy,
    );
    let provenance = selected_history_provenance(&selected_image);
    let receipt = match history.publish_selected_typed_v3_history_branch(
        &selected_image,
        branch,
        provenance,
        policies,
        SemanticTypedPlaneVerificationTierV2::Standard,
        JumboRopeLimits::default(),
        &mut source,
    ) {
        Ok(receipt) => receipt,
        Err(error) => return Err(map_typed_history_publication_error(error)),
    };
    ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)?;
    let commit = receipt.current().ok_or_else(|| {
        NativeHistoryPublicationError::Refused(
            "typed V3 branch publication returned no current commit".to_owned(),
        )
    })?;
    let ancestry = history
        .history_ref_ancestry_proof(
            selected_image.target(),
            backend_replication::HistoryRefKind::Branch,
            &HistoryRefName::new("selected-native-v3")
                .map_err(NativeHistoryPublicationError::Refused)?,
            commit,
        )
        .map_err(NativeHistoryPublicationError::Refused)?;
    if ancestry.ancestor() != commit {
        return Err(NativeHistoryPublicationError::Refused(
            "typed V3 branch ancestry did not prove its CAS commit".to_owned(),
        ));
    }
    ensure_native_history_selection(&work.loader, &work.key, claim, work.stamp)?;
    let parent_commits = receipt.previous().into_iter().collect::<Vec<_>>();
    let proof = native_history_publication_proof(
        selected_image.selected_stamp(),
        selected_image.image_key(),
        selected_image.image_identity(),
        ancestry.ref_tip(),
        ancestry.ancestor(),
        &parent_commits,
    )?;
    Ok(NativeHistoryPublicationReceipt { commit, proof })
}

fn native_history_publication_proof(
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    reference_tip: HistoryCommitId,
    reachable_commit: HistoryCommitId,
    parent_commits: &[HistoryCommitId],
) -> Result<backend_engine::SemanticHistoryPublicationProof, NativeHistoryPublicationError> {
    if parent_commits.len() > 2 {
        return Err(NativeHistoryPublicationError::Refused(
            "typed V3 history commit exceeds the bounded parent count".to_owned(),
        ));
    }
    Ok(backend_engine::SemanticHistoryPublicationProof {
        selection: backend_engine::SemanticHistorySelectionStamp {
            namespace: *stamp.namespace(),
            profile: backend_engine::SemanticLanguageProfile::new(stamp.profile()),
            source_coordinate: *stamp.source_coordinate(),
            selection_revision: stamp.selection_revision(),
            selected_root: *stamp.selected_root(),
            closure_id: *stamp.closure_id(),
            catalog_root: *stamp.catalog_root().as_bytes(),
        },
        image: backend_engine::SemanticHistoryImageIdentity {
            artifact_ordinal: image.artifact_ordinal(),
            semantic_generation: *image.semantic_generation().as_bytes(),
            manifest_root: *image.manifest_root().as_bytes(),
            image_identity: *image_identity.as_ref(),
        },
        reference_tip: *reference_tip.as_bytes(),
        reachable_commit: *reachable_commit.as_bytes(),
        parent_commits: parent_commits
            .iter()
            .map(|commit| *commit.as_bytes())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        input_replay_status: backend_engine::SemanticHistoryInputReplayStatus::Unproven,
    })
}

fn selected_history_provenance(
    selected: &backend_replication::SelectedNativeHistoryImage<'_>,
) -> [u8; 32] {
    let target = selected.target();
    let stamp = selected.selected_stamp();
    let image = selected.image_key();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.locald.selected-native-v3.provenance.v1\0");
    hasher.update(target.package().as_bytes());
    hasher.update(&[0]);
    hasher.update(target.coordinate().as_bytes());
    hasher.update(&<[u8; 2]>::from(target.profile()));
    hasher.update(stamp.namespace());
    hasher.update(&<[u8; 2]>::from(stamp.profile()));
    hasher.update(stamp.source_coordinate());
    hasher.update(&stamp.selection_revision().to_le_bytes());
    hasher.update(stamp.selected_root());
    hasher.update(stamp.closure_id());
    hasher.update(stamp.catalog_root().as_bytes());
    hasher.update(&image.artifact_ordinal().to_le_bytes());
    hasher.update(image.semantic_generation().as_bytes());
    hasher.update(image.manifest_root().as_bytes());
    hasher.update(selected.image_identity().as_ref());
    *hasher.finalize().as_bytes()
}

/// Reads bounded canonical semantic-plane byte ranges from the selected CAS.
#[derive(Clone)]
pub(super) struct VersionedPlaneService<'hydrator> {
    store: FileStore,
    verified_segments: Arc<VerifiedSegmentCache>,
    s3_hydrator: Option<&'hydrator dyn super::s3_publication::SelectedClosurePublisher>,
}

impl fmt::Debug for VersionedPlaneService<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VersionedPlaneService")
            .field("verified_segments", &self.verified_segments)
            .field("s3_configured", &self.s3_hydrator.is_some())
            .finish_non_exhaustive()
    }
}

impl VersionedPlaneService<'static> {
    /// Opens a service over the local immutable CAS.
    #[must_use]
    pub(super) fn new(store: FileStore) -> Self {
        Self::with_cache(store, Arc::new(VerifiedSegmentCache::default()))
    }

    /// Reuses admitted immutable segments across independent range requests.
    #[must_use]
    pub(super) fn with_cache(
        store: FileStore,
        verified_segments: Arc<VerifiedSegmentCache>,
    ) -> Self {
        Self {
            store,
            verified_segments,
            s3_hydrator: None,
        }
    }
}

impl<'hydrator> VersionedPlaneService<'hydrator> {
    /// Adds the configured S3 closure reader used after an exact local CAS miss.
    #[must_use]
    pub(super) fn with_cache_and_hydrator(
        store: FileStore,
        verified_segments: Arc<VerifiedSegmentCache>,
        s3_hydrator: Option<&'hydrator dyn super::s3_publication::SelectedClosurePublisher>,
    ) -> Self {
        Self {
            store,
            verified_segments,
            s3_hydrator,
        }
    }

    /// Serves one bounded page of the metadata-only image catalog. The first
    /// page discovers the selected head; every continuation must re-present
    /// the exact stamp, aggregate root, and catalog length.
    pub(super) fn serve_catalog_get<R: VersionedPlaneSelectionResolver>(
        &self,
        resolver: &mut R,
        get: &SemanticCatalogGet,
        expected_environment: [u8; 32],
        expected_target_platform: [u8; 32],
    ) -> Result<SemanticCatalogChunk, VersionedPlaneServiceError> {
        if get.byte_range.len == 0 || get.byte_range.len > MAX_RANGE_BYTES {
            return Err(VersionedPlaneServiceError::RangeBounds);
        }
        let selected = resolver
            .current_selected_plane()
            .map_err(|error| VersionedPlaneServiceError::Authority(error.to_string()))?;
        if selected.environment != expected_environment
            || selected.target_platform != expected_target_platform
        {
            return Err(VersionedPlaneServiceError::PlatformOrEnvironmentMismatch);
        }
        let total_length = selected.metadata().catalog_len();
        if total_length == 0 || total_length > MAX_MANIFEST_BYTES {
            return Err(VersionedPlaneServiceError::CatalogLength);
        }
        if get
            .selected_stamp
            .is_some_and(|stamp| stamp != selected.stamp())
            || get
                .catalog_root
                .is_some_and(|root| root != selected.metadata().catalog_root())
            || get
                .total_length
                .is_some_and(|length| length != total_length)
        {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        validate_requested_range(get.byte_range, total_length)?;
        let mut payload = vec![
            0;
            usize::try_from(get.byte_range.len)
                .map_err(|_| VersionedPlaneServiceError::RangeBounds)?
        ];
        selected
            .metadata()
            .write_catalog_range(get.byte_range.start, &mut payload)
            .map_err(|error| VersionedPlaneServiceError::Manifest(error.to_string()))?;

        let still_current = resolver
            .current_selected_plane()
            .map_err(|error| VersionedPlaneServiceError::Authority(error.to_string()))?;
        if still_current.stamp() != selected.stamp()
            || still_current.metadata().catalog_root() != selected.metadata().catalog_root()
            || still_current.metadata().catalog_len() != total_length
        {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        let chunk = SemanticCatalogChunk {
            request_id: get.request_id,
            target: get.target.clone(),
            selected_stamp: selected.stamp(),
            catalog_root: selected.metadata().catalog_root(),
            total_length,
            byte_range: get.byte_range,
            payload,
        };
        chunk
            .validate_against(get)
            .map_err(|error| VersionedPlaneServiceError::Request(error.to_string()))?;
        Ok(chunk)
    }

    /// Serves one bounded page of the exact per-image canonical manifest.
    pub(super) fn serve_manifest_get<R: VersionedPlaneSelectionResolver>(
        &self,
        resolver: &mut R,
        get: &SemanticManifestGet,
        expected_environment: [u8; 32],
        expected_target_platform: [u8; 32],
    ) -> Result<SemanticManifestChunk, VersionedPlaneServiceError> {
        if get.byte_range.len == 0 || get.byte_range.len > MAX_RANGE_BYTES {
            return Err(VersionedPlaneServiceError::RangeBounds);
        }
        let selected = resolver
            .current_selected_plane()
            .map_err(|error| VersionedPlaneServiceError::Authority(error.to_string()))?;
        if selected.stamp() != get.selected_stamp
            || selected.metadata().catalog_root() != get.catalog_root
        {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        if selected.environment != expected_environment
            || selected.target_platform != expected_target_platform
        {
            return Err(VersionedPlaneServiceError::PlatformOrEnvironmentMismatch);
        }
        let artifact = selected
            .metadata()
            .artifact_for_image(get.image)
            .ok_or(VersionedPlaneServiceError::MissingImage)?;
        let total_length = u64::try_from(artifact.manifest_bytes().len())
            .map_err(|_| VersionedPlaneServiceError::ManifestLength)?;
        if total_length != get.total_length || get.image.manifest_root().as_bytes() == &[0; 32] {
            return Err(VersionedPlaneServiceError::ManifestMismatch);
        }
        validate_requested_range(get.byte_range, total_length)?;
        let mut payload = vec![
            0;
            usize::try_from(get.byte_range.len)
                .map_err(|_| VersionedPlaneServiceError::RangeBounds)?
        ];
        selected
            .metadata()
            .write_manifest_range(get.image, get.byte_range.start, &mut payload)
            .map_err(|error| VersionedPlaneServiceError::Manifest(error.to_string()))?;

        let still_current = resolver
            .current_selected_plane()
            .map_err(|error| VersionedPlaneServiceError::Authority(error.to_string()))?;
        let still_selected = still_current
            .metadata()
            .artifact_for_image(get.image)
            .is_some_and(|current| current.manifest_bytes().len() as u64 == total_length);
        if still_current.stamp() != selected.stamp()
            || still_current.metadata().catalog_root() != selected.metadata().catalog_root()
            || !still_selected
        {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        let chunk = SemanticManifestChunk {
            request_id: get.request_id,
            target: get.target.clone(),
            selected_stamp: selected.stamp(),
            catalog_root: selected.metadata().catalog_root(),
            image: get.image,
            total_length,
            byte_range: get.byte_range,
            payload,
        };
        chunk
            .validate_against(get)
            .map_err(|error| VersionedPlaneServiceError::Request(error.to_string()))?;
        Ok(chunk)
    }

    /// Serves one range requested by the storage-neutral IR/embedding cursor.
    ///
    /// Selection and range metadata are checked against fresh Turso state
    /// before reading. The typed CAS object and semantic segment identity are
    /// checked before slicing. Current selection is re-read immediately before
    /// bytes are exposed.
    pub(super) fn serve_range<R: VersionedPlaneSelectionResolver>(
        &self,
        resolver: &mut R,
        request: &IrHydrationRequest,
        expected_environment: [u8; 32],
        expected_target_platform: [u8; 32],
    ) -> Result<Vec<u8>, VersionedPlaneServiceError> {
        let selected_stamp = request.selection().stamp();
        let range_request = request
            .segment(resolver)
            .map_err(|error| VersionedPlaneServiceError::Request(error.to_string()))?;
        let byte_range = request
            .byte_range(resolver)
            .map_err(|error| VersionedPlaneServiceError::Request(error.to_string()))?
            .ok_or(VersionedPlaneServiceError::NoMissingRange)?;
        self.serve_range_claim(
            resolver,
            selected_stamp,
            request.selection().image(),
            range_request,
            byte_range,
            expected_environment,
            expected_target_platform,
        )
    }

    /// Serves one decoded wire range after rebinding it to current authority.
    ///
    /// This is the local RPC entry point. The wire's selected stamp is only a
    /// claim: the resolver reconstructs the committed selection and exact
    /// manifest before the request is admitted.
    pub(super) fn serve_range_claim<R: VersionedPlaneSelectionResolver>(
        &self,
        resolver: &mut R,
        requested_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
        range_request: SemanticRangeRequest,
        byte_range: ByteRange,
        expected_environment: [u8; 32],
        expected_target_platform: [u8; 32],
    ) -> Result<Vec<u8>, VersionedPlaneServiceError> {
        let selected = resolver
            .current_selected_plane()
            .map_err(|error| VersionedPlaneServiceError::Authority(error.to_string()))?;
        if selected.stamp() != requested_stamp {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        if selected.environment != expected_environment
            || selected.target_platform != expected_target_platform
        {
            return Err(VersionedPlaneServiceError::PlatformOrEnvironmentMismatch);
        }
        validate_requested_range(byte_range, range_request.byte_length)?;
        if range_request.manifest_root != image.manifest_root() {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        let manifest = selected.manifest(image)?;
        let artifact = selected
            .metadata()
            .artifact_for_image(image)
            .ok_or(VersionedPlaneServiceError::MissingImage)?;
        let plane = manifest
            .plane(range_request.plane)
            .ok_or(VersionedPlaneServiceError::MissingPlane)?;
        let segment = plane
            .segments()
            .iter()
            .find(|segment| segment.id_claim() == range_request.segment_id)
            .ok_or(VersionedPlaneServiceError::MissingSegment)?;
        if segment.first_key() != &range_request.first_key
            || segment.last_key() != &range_request.last_key
            || segment.byte_length() != range_request.byte_length
        {
            return Err(VersionedPlaneServiceError::RequestMismatch);
        }
        let member_position = artifact
            .members()
            .binary_search_by(|member| {
                member
                    .segment_id()
                    .as_bytes()
                    .cmp(range_request.segment_id.as_bytes())
            })
            .map_err(|_| VersionedPlaneServiceError::MissingMember)?;
        let member = artifact
            .members()
            .get(member_position)
            .copied()
            .ok_or(VersionedPlaneServiceError::MissingMember)?;
        if member.byte_length() != segment.byte_length() || member.byte_length() > MAX_SEGMENT_BYTES
        {
            return Err(VersionedPlaneServiceError::MemberMismatch);
        }

        // The immutable object is read and admitted once per resident cache
        // lifetime, while each 16 KiB page still checks the live selection.
        // A cache miss reads at most one complete 1 MiB segment; a hit shares
        // its verified bytes without another disk read or semantic hash.
        let object_id = *member.object_id();
        let start = usize::try_from(byte_range.start)
            .map_err(|_| VersionedPlaneServiceError::RangeBounds)?;
        let end = usize::try_from(
            byte_range
                .end()
                .map_err(|_| VersionedPlaneServiceError::RangeBounds)?,
        )
        .map_err(|_| VersionedPlaneServiceError::RangeBounds)?;
        let bytes = match self.verified_segments.get(object_id) {
            Some(verified) => {
                if u64::try_from(verified.bytes.len()).ok() != Some(member.byte_length()) {
                    return Err(VersionedPlaneServiceError::MemberMismatch);
                }
                // A physical payload object is keyed by the shared typed
                // segment schema and canonical bytes. The logical segment ID
                // also binds plane metadata, so identical bytes can be
                // referenced from another plane or generation. Re-admit that
                // uncommon association before exposing its page.
                if verified.plane != range_request.plane
                    || verified.segment_id != *range_request.segment_id.as_bytes()
                {
                    let admitted = segment
                        .admit(range_request.plane, &verified.bytes)
                        .map_err(|error| VersionedPlaneServiceError::Segment(error.to_string()))?;
                    if admitted.as_bytes() != range_request.segment_id.as_bytes() {
                        return Err(VersionedPlaneServiceError::MemberMismatch);
                    }
                }
                verified
                    .bytes
                    .get(start..end)
                    .ok_or(VersionedPlaneServiceError::RangeBounds)?
                    .to_vec()
            }
            None => {
                let object_claim = UntrustedObjectId::from_bytes(*member.object_id());
                let payload = match read_local_segment_payload(
                    &self.store,
                    object_claim,
                    member.byte_length(),
                )? {
                    Some(payload) => payload,
                    None => {
                        let hydrator = self.s3_hydrator.ok_or_else(|| {
                            VersionedPlaneServiceError::Store(
                                "selected CAS member is absent and S3 is not configured".to_owned(),
                            )
                        })?;
                        let selected_storage = selected.storage_selection.ok_or_else(|| {
                            VersionedPlaneServiceError::S3(
                                "selected generation has no exact S3 receipt identity".to_owned(),
                            )
                        })?;
                        hydrator
                            .hydrate_object(
                                &self.store,
                                selected_storage,
                                object_claim,
                                VERSIONED_PLANE_SEGMENT_SCHEMA,
                                member.byte_length(),
                            )
                            .map_err(|error| VersionedPlaneServiceError::S3(error.to_string()))?
                    }
                };
                if u64::try_from(payload.len()).ok() != Some(member.byte_length()) {
                    return Err(VersionedPlaneServiceError::MemberMismatch);
                }
                let admitted = segment
                    .admit(range_request.plane, &payload)
                    .map_err(|error| VersionedPlaneServiceError::Segment(error.to_string()))?;
                if admitted.as_bytes() != range_request.segment_id.as_bytes() {
                    return Err(VersionedPlaneServiceError::MemberMismatch);
                }
                let bytes = payload
                    .get(start..end)
                    .ok_or(VersionedPlaneServiceError::RangeBounds)?
                    .to_vec();
                // Transfer the just-verified allocation into the cache after
                // making the requested page. This avoids cloning its Arc on a
                // miss; cache hits are the only path that clones an Arc.
                self.verified_segments.admit(
                    object_id,
                    VerifiedSegment {
                        bytes: Arc::from(payload),
                        segment_id: *admitted.as_bytes(),
                        plane: range_request.plane,
                    },
                );
                bytes
            }
        };

        let still_current = resolver
            .current_selected_plane()
            .map_err(|error| VersionedPlaneServiceError::Authority(error.to_string()))?;
        if still_current.stamp() != selected.stamp()
            || still_current.storage_selection != selected.storage_selection
            || still_current.metadata().artifact_for_image(image).is_none()
        {
            return Err(VersionedPlaneServiceError::StaleSelection);
        }
        Ok(bytes)
    }
}

fn read_local_segment_payload(
    store: &FileStore,
    object_id: UntrustedObjectId,
    expected_payload_len: u64,
) -> Result<Option<Vec<u8>>, VersionedPlaneServiceError> {
    if expected_payload_len > MAX_SEGMENT_BYTES {
        return Err(VersionedPlaneServiceError::MemberMismatch);
    }
    let budget = ArtifactBudget::new(1, 1, MAX_SEGMENT_BYTES, OBJECT_READ_CHUNK_BYTES, 1);
    let Some(mut reader) = store
        .artifact_sink(budget)
        .open_object(object_id)
        .map_err(|error| VersionedPlaneServiceError::Store(format!("{error:?}")))?
    else {
        return Ok(None);
    };
    if reader.id().as_bytes() != object_id.as_bytes()
        || reader.schema() != VERSIONED_PLANE_SEGMENT_SCHEMA
        || reader.payload_len() != expected_payload_len
    {
        return Err(VersionedPlaneServiceError::MemberMismatch);
    }
    let payload_len = usize::try_from(expected_payload_len)
        .map_err(|_| VersionedPlaneServiceError::MemberMismatch)?;
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(payload_len)
        .map_err(|_| VersionedPlaneServiceError::Store("segment allocation failed".to_owned()))?;
    let mut buffer = [0_u8; OBJECT_READ_CHUNK_BYTES];
    let mut offset = 0_u64;
    while offset < expected_payload_len {
        let remaining = expected_payload_len - offset;
        let request_len = usize::try_from(remaining.min(
            u64::try_from(buffer.len()).map_err(|_| VersionedPlaneServiceError::MemberMismatch)?,
        ))
        .map_err(|_| VersionedPlaneServiceError::MemberMismatch)?;
        let read = reader
            .read_payload_range(offset, &mut buffer[..request_len])
            .map_err(|error| VersionedPlaneServiceError::Store(format!("{error:?}")))?;
        if read != request_len {
            return Err(VersionedPlaneServiceError::MemberMismatch);
        }
        payload.extend_from_slice(&buffer[..read]);
        offset = offset
            .checked_add(
                u64::try_from(read).map_err(|_| VersionedPlaneServiceError::MemberMismatch)?,
            )
            .ok_or(VersionedPlaneServiceError::MemberMismatch)?;
    }
    Ok(Some(payload))
}

fn validate_requested_range(
    range: ByteRange,
    segment_length: u64,
) -> Result<(), VersionedPlaneServiceError> {
    let end = range
        .end()
        .map_err(|_| VersionedPlaneServiceError::RangeBounds)?;
    if range.len == 0 || range.len > MAX_RANGE_BYTES || end > segment_length {
        return Err(VersionedPlaneServiceError::RangeBounds);
    }
    Ok(())
}

pub(super) fn coordinate_identity(coordinate: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.locald.versioned-plane.source-coordinate.v1\0");
    hasher.update(coordinate.as_bytes());
    *hasher.finalize().as_bytes()
}

pub(super) fn native_history_selection_id(
    key: &ProductSemanticPublicationKey,
    stamp: SelectedGenerationStamp,
    image: Option<SemanticPlaneImageKey>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.locald.native-history-selection.v1\0");
    hasher.update(key.package().as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(key.coordinate().as_str().as_bytes());
    hasher.update(&<[u8; 2]>::from(key.profile()));
    hasher.update(stamp.namespace());
    hasher.update(&<[u8; 2]>::from(stamp.profile()));
    hasher.update(stamp.source_coordinate());
    hasher.update(&stamp.selection_revision().to_le_bytes());
    hasher.update(stamp.selected_root());
    hasher.update(stamp.closure_id());
    hasher.update(stamp.catalog_root().as_bytes());
    match image {
        Some(image) => {
            hasher.update(&[1]);
            hasher.update(&image.artifact_ordinal().to_le_bytes());
            hasher.update(image.semantic_generation().as_bytes());
            hasher.update(image.manifest_root().as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    *hasher.finalize().as_bytes()
}

/// Selection, range, schema, or durable-CAS failure while serving a plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum VersionedPlaneServiceError {
    /// Turso and the product key do not identify one exact target.
    SelectionMismatch,
    /// The current selected compiler closure lacks versioned planes.
    MissingManifest,
    /// The requested exact image key is absent from the selected catalog.
    MissingImage,
    /// The manifest root or compiler profile differs from selected authority.
    ManifestMismatch,
    /// Failed to parse canonical manifest metadata.
    Manifest(String),
    /// The selected authority stamp could not be checked by replication.
    Selection(String),
    /// The caller's cursor names another current selection.
    StaleSelection,
    /// The requested IR or embedding plane is not in the selected manifest.
    MissingPlane,
    /// The requested logical segment is not in the selected plane.
    MissingSegment,
    /// The selected closure has no physical object ref for the logical segment.
    MissingMember,
    /// Physical object schema, length, or logical segment ID is inconsistent.
    MemberMismatch,
    /// The request range metadata differs from the selected manifest.
    RequestMismatch,
    /// The bounded range is empty, too large, overflows, or exceeds the segment.
    RangeBounds,
    /// The selected canonical manifest exceeds the bounded bootstrap size.
    ManifestLength,
    /// The selected canonical catalog exceeds its bound.
    CatalogLength,
    /// Caller already has all bytes and requested no range.
    NoMissingRange,
    /// Runtime environment or target platform differs from selected build.
    PlatformOrEnvironmentMismatch,
    /// The replication cursor rejected an out-of-date request.
    Request(String),
    /// Authority lookup or cold metadata reopen failed.
    Authority(String),
    /// Durable CAS read failed or the selected member is missing/corrupt.
    Store(String),
    /// Exact receipt lookup or proof-checked S3 hydration failed.
    S3(String),
    /// The segment payload did not satisfy its canonical semantic ID.
    Segment(String),
}

impl fmt::Display for VersionedPlaneServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelectionMismatch => formatter.write_str("selected compiler key mismatch"),
            Self::MissingManifest => {
                formatter.write_str("selected closure has no versioned plane manifest")
            }
            Self::MissingImage => {
                formatter.write_str("requested semantic image is absent from selected catalog")
            }
            Self::ManifestMismatch => {
                formatter.write_str("selected semantic manifest root or profile mismatch")
            }
            Self::Manifest(error) => write!(formatter, "decode semantic plane manifest: {error}"),
            Self::Selection(error) => {
                write!(formatter, "invalid selected generation stamp: {error}")
            }
            Self::StaleSelection => formatter.write_str("semantic plane selection is stale"),
            Self::MissingPlane => formatter.write_str("requested semantic plane is absent"),
            Self::MissingSegment => formatter.write_str("requested semantic segment is absent"),
            Self::MissingMember => {
                formatter.write_str("selected closure is missing the semantic segment object")
            }
            Self::MemberMismatch => {
                formatter.write_str("semantic segment object differs from the selected manifest")
            }
            Self::RequestMismatch => {
                formatter.write_str("semantic range metadata differs from the selected manifest")
            }
            Self::RangeBounds => {
                formatter.write_str("semantic byte range exceeds configured bounds")
            }
            Self::ManifestLength => {
                formatter.write_str("selected semantic manifest exceeds bootstrap bounds")
            }
            Self::CatalogLength => {
                formatter.write_str("selected semantic catalog exceeds bootstrap bounds")
            }
            Self::NoMissingRange => {
                formatter.write_str("semantic cursor has no missing byte range")
            }
            Self::PlatformOrEnvironmentMismatch => formatter
                .write_str("selected semantic plane targets another environment or platform"),
            Self::Request(error) => {
                write!(formatter, "invalid semantic hydration request: {error}")
            }
            Self::Authority(error) => {
                write!(formatter, "read selected semantic authority: {error}")
            }
            Self::Store(error) => write!(formatter, "read selected semantic CAS member: {error}"),
            Self::S3(error) => write!(formatter, "hydrate selected semantic S3 member: {error}"),
            Self::Segment(error) => write!(formatter, "admit selected semantic segment: {error}"),
        }
    }
}

impl std::error::Error for VersionedPlaneServiceError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use backend_extension_turso::VersionedPlanePublication;
    use backend_replication::{
        ByteRange, SemanticCatalogGet, SemanticManifestGet, SemanticTargetKey,
    };
    use backend_semantic::ir::{
        EmbeddingNormalization, EmbeddingPlaneIdentity, GenerationId, LanguageProfile, RustEdition,
        SemanticBuildIdentity, SemanticInputWitness, SemanticPlane, SemanticPlaneCatalog,
        SemanticPlaneCatalogEntry, SemanticPlaneImageKey, SemanticPlaneKind, SemanticPlaneManifest,
        SemanticPlaneSegment,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_store::{FileStore, TypedObject};
    use backend_version::{Coverage, ObjectKey, ScopeRoot};
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn native_history_range_status_keeps_transient_and_integrity_failures_distinct() {
        use super::super::s3_publication::{PublicationError, RemoteHydrationOperation};

        assert!(matches!(
            SelectedImageRangeReadError::from_remote(PublicationError::RemoteStore(
                backend_store_s3::RemoteStoreError::Unavailable,
            )),
            SelectedImageRangeReadError::Deferred(_),
        ));
        assert!(matches!(
            SelectedImageRangeReadError::from_remote(PublicationError::RemoteHydration {
                operation: RemoteHydrationOperation::OpenEnvelope,
                source: Some(backend_store_s3::RemoteStoreError::Identity),
            }),
            SelectedImageRangeReadError::Refused(_),
        ));
        assert!(matches!(
            SelectedImageRangeReadError::from_remote(PublicationError::RemoteHydration {
                operation: RemoteHydrationOperation::ReadPayload,
                source: None,
            }),
            SelectedImageRangeReadError::Refused(_),
        ));
    }

    #[test]
    fn typed_v3_publication_failures_keep_retryability_separate_from_refusal() {
        use backend_replication::{
            SelectedTypedV3HistoryOperation as Operation, SelectedTypedV3HistoryRefusal as Refusal,
        };

        assert!(matches!(
            map_typed_history_publication_error(SelectedTypedV3HistoryError::StaleSelection),
            NativeHistoryPublicationError::Superseded,
        ));
        assert!(matches!(
            map_typed_history_publication_error(
                SelectedTypedV3HistoryError::RetryableAvailability {
                    operation: Operation::CompareAndSwapRef,
                    detail: "temporary store outage".to_owned(),
                }
            ),
            NativeHistoryPublicationError::Deferred(reason)
                if reason.contains("CompareAndSwapRef")
        ));
        assert!(matches!(
            map_typed_history_publication_error(SelectedTypedV3HistoryError::Refused {
                operation: Operation::VerifyPayloadClosure,
                cause: Refusal::IntegrityFailure,
                detail: "invalid closure".to_owned(),
            }),
            NativeHistoryPublicationError::Refused(reason)
                if reason.contains("IntegrityFailure")
        ));
    }

    #[test]
    fn owned_selection_source_classifies_an_absent_committed_marker_as_stale() {
        let directory = path();
        let package = backend_engine::PackageReference::parse(
            "pkg:cargo/native-history-selection@1.0.0".to_owned(),
        )
        .expect("valid product package");
        let coordinate = backend_library::interface::PackageUrl::parse(
            "pkg:cargo/native-history-selection@1.0.0".to_owned(),
        )
        .expect("valid product coordinate");
        let key = ProductSemanticPublicationKey::new(
            package,
            coordinate,
            backend_semantic::ir::LanguageProfile::Rust(
                backend_semantic::ir::RustEdition::Rust2021,
            ),
        )
        .expect("valid product selection key");
        let authority = SemanticAuthority::open(&directory).expect("open semantic authority");
        let mut source = authority.owned_history_selection_source(key);

        let error = source
            .current_selected_generation()
            .expect_err("unselected product has no current history stamp");
        assert!(matches!(
            OwnedSemanticAuthoritySelectionSource::classify_selection_error(&error),
            backend_replication::SelectedNativeImageSourceFailure::StaleSelection,
        ));

        drop(source);
        drop(authority);
        std::fs::remove_dir_all(directory).expect("remove temporary authority workspace");
    }

    #[test]
    fn native_history_pair_distinguishes_absence_from_selector_lock_failure() {
        let directory = path();
        let package = backend_engine::PackageReference::parse(
            "pkg:cargo/native-history-pair-state@1.0.0".to_owned(),
        )
        .expect("valid product package");
        let coordinate = backend_library::interface::PackageUrl::parse(
            "pkg:cargo/native-history-pair-state@1.0.0".to_owned(),
        )
        .expect("valid product coordinate");
        let key = ProductSemanticPublicationKey::new(
            package,
            coordinate,
            backend_semantic::ir::LanguageProfile::Rust(
                backend_semantic::ir::RustEdition::Rust2021,
            ),
        )
        .expect("valid product selection key");
        let authority = SemanticAuthority::open(&directory).expect("open semantic authority");
        let loader = authority.native_history_loader_for_test();
        assert!(matches!(
            require_committed_native_history_pair(&loader, &key),
            Err(NativeHistoryPublicationError::Superseded),
        ));

        let poison_loader = Arc::clone(&loader);
        let poison = std::thread::spawn(move || {
            let _write = poison_loader
                .selections
                .write()
                .expect("acquire selector lock before poisoning it");
            panic!("intentional native-history selector poison");
        });
        assert!(
            poison.join().is_err(),
            "fault injection must poison the lock"
        );
        assert!(matches!(
            require_committed_native_history_pair(&loader, &key),
            Err(NativeHistoryPublicationError::Refused(_)),
        ));

        drop(authority);
        std::fs::remove_dir_all(directory).expect("remove temporary authority workspace");
    }

    struct FakeSelectedImageRangePublisher {
        expected_selection: super::super::s3_publication::RemoteClosureSelection,
        expected_object_id: [u8; 32],
        expected_payload: Vec<u8>,
        range_call: Mutex<
            Option<(
                super::super::s3_publication::RemoteClosureSelection,
                [u8; 32],
                backend_version::SchemaIdentity,
                u64,
                u64,
                u64,
            )>,
        >,
    }

    impl super::super::s3_publication::SelectedClosurePublisher for FakeSelectedImageRangePublisher {
        fn publish_closure(
            &self,
            _store: &FileStore,
            _closure: backend_store::ClosureId,
            _target_root: [u8; 32],
            _expected_count: u64,
            _budget: ArtifactBudget,
            _publication_fence: super::super::s3_publication::PublicationFence,
        ) -> Result<
            super::super::s3_publication::ExactS3ClosureReceipt,
            super::super::s3_publication::PublicationError,
        > {
            Err(super::super::s3_publication::PublicationError::Remote)
        }

        fn hydrate_object(
            &self,
            _store: &FileStore,
            _selected: super::super::s3_publication::RemoteClosureSelection,
            _object_id: UntrustedObjectId,
            _expected_schema: backend_version::SchemaIdentity,
            _expected_payload_len: u64,
        ) -> Result<Vec<u8>, super::super::s3_publication::PublicationError> {
            Err(super::super::s3_publication::PublicationError::Receipt)
        }

        fn hydrate_object_range(
            &self,
            _store: &FileStore,
            selected: super::super::s3_publication::RemoteClosureSelection,
            object_id: UntrustedObjectId,
            expected_schema: backend_version::SchemaIdentity,
            expected_payload_len: u64,
            offset: u64,
            length: u64,
        ) -> Result<Vec<u8>, super::super::s3_publication::PublicationError> {
            *self
                .range_call
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
                selected,
                *object_id.as_bytes(),
                expected_schema,
                expected_payload_len,
                offset,
                length,
            ));
            if selected != self.expected_selection
                || *object_id.as_bytes() != self.expected_object_id
                || expected_schema != backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA
                || expected_payload_len
                    != u64::try_from(self.expected_payload.len()).unwrap_or(u64::MAX)
            {
                return Err(super::super::s3_publication::PublicationError::Receipt);
            }
            let end = offset
                .checked_add(length)
                .and_then(|end| usize::try_from(end).ok())
                .ok_or(super::super::s3_publication::PublicationError::Receipt)?;
            let start = usize::try_from(offset)
                .map_err(|_| super::super::s3_publication::PublicationError::Receipt)?;
            self.expected_payload
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or(super::super::s3_publication::PublicationError::Receipt)
        }

        fn has_durable_selected_closure(
            &self,
            _store: &FileStore,
            _selected: super::super::s3_publication::RemoteClosureSelection,
        ) -> Result<bool, super::super::s3_publication::PublicationError> {
            Ok(false)
        }

        fn verified_remote_segments(
            &self,
            _store: &FileStore,
            _selected: &backend_extension_turso::SelectedGeneration,
        ) -> Result<
            Option<super::super::s3_publication::VerifiedRemoteSegmentSet>,
            super::super::s3_publication::PublicationError,
        > {
            Ok(None)
        }
    }

    #[test]
    fn selected_image_range_reader_hydrates_exact_remote_image_range_when_local_object_is_absent() {
        let fixture = fixture();
        let selection = remote_selection();
        let payload = b"remote-only-selected-image".to_vec();
        let object_id = [0xD7; 32];
        let range_start = 5;
        let range_len = 9;
        let publisher = Arc::new(FakeSelectedImageRangePublisher {
            expected_selection: selection,
            expected_object_id: object_id,
            expected_payload: payload.clone(),
            range_call: Mutex::new(None),
        });
        let reader = SelectedClosureImageRangeReader::new(
            fixture.store.clone(),
            Arc::new(super::super::selected_full_image::VerifiedLocalImageReaderCache::default()),
            Some(publisher.clone()),
        );
        let stamp = fixture.publication.stamp();
        let plan = super::super::selected_full_image::SelectedFullImagePlan {
            stamp,
            image: fixture.image,
            identity: SemanticImageIdentity::from_encoded_bytes(&payload),
            total_length: u64::try_from(payload.len()).expect("payload length fits u64"),
            object_id,
            closure_id: *stamp.closure_id(),
            remote_selection: selection,
            environment: fixture.expected_environment,
            target_platform: fixture.expected_target_platform,
        };
        let range = ByteRange {
            start: range_start,
            len: range_len,
        };
        let mut output = vec![0xEE; usize::try_from(range_len).expect("range fits usize")];

        let count = reader
            .read_range_into(&plan, range, &mut output)
            .expect("read one verified remote image range");

        assert_eq!(count, output.len());
        assert_eq!(
            output,
            payload[usize::try_from(range_start).expect("offset fits usize")
                ..usize::try_from(range_start + range_len).expect("end fits usize")]
        );
        assert_eq!(
            *publisher
                .range_call
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some((
                selection,
                object_id,
                backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA,
                u64::try_from(payload.len()).expect("payload length fits u64"),
                range_start,
                range_len,
            )),
            "fallback must request this selected closure member and exact bounded range"
        );
        let path = fixture.path.clone();
        drop(reader);
        drop(publisher);
        drop(fixture);
        fs::remove_dir_all(path).expect("remove selected image range fixture");
    }

    struct FakeS3Hydrator {
        expected_selection: super::super::s3_publication::RemoteClosureSelection,
        expected_object_id: [u8; 32],
        payload: Vec<u8>,
    }

    impl super::super::s3_publication::SelectedClosurePublisher for FakeS3Hydrator {
        fn publish_closure(
            &self,
            _store: &FileStore,
            _closure: backend_store::ClosureId,
            _target_root: [u8; 32],
            _expected_count: u64,
            _budget: ArtifactBudget,
            _publication_fence: super::super::s3_publication::PublicationFence,
        ) -> Result<
            super::super::s3_publication::ExactS3ClosureReceipt,
            super::super::s3_publication::PublicationError,
        > {
            Err(super::super::s3_publication::PublicationError::Remote)
        }

        fn hydrate_object(
            &self,
            _store: &FileStore,
            selected: super::super::s3_publication::RemoteClosureSelection,
            object_id: UntrustedObjectId,
            expected_schema: backend_version::SchemaIdentity,
            expected_payload_len: u64,
        ) -> Result<Vec<u8>, super::super::s3_publication::PublicationError> {
            if selected != self.expected_selection
                || *object_id.as_bytes() != self.expected_object_id
                || expected_schema != VERSIONED_PLANE_SEGMENT_SCHEMA
            {
                return Err(super::super::s3_publication::PublicationError::Receipt);
            }
            let _ = expected_payload_len;
            Ok(self.payload.clone())
        }

        // This fake exercises the bounded range hydration path only. It does
        // not model a durable owner receipt or GC residency proof, so it must
        // never authorize remote-backed collection roots.
        fn has_durable_selected_closure(
            &self,
            _store: &FileStore,
            _selected: super::super::s3_publication::RemoteClosureSelection,
        ) -> Result<bool, super::super::s3_publication::PublicationError> {
            Ok(false)
        }

        fn verified_remote_segments(
            &self,
            _store: &FileStore,
            _selected: &backend_extension_turso::SelectedGeneration,
        ) -> Result<
            Option<super::super::s3_publication::VerifiedRemoteSegmentSet>,
            super::super::s3_publication::PublicationError,
        > {
            Ok(None)
        }
    }

    struct TestResolver {
        publications: VecDeque<SelectedVersionedPlanePublication>,
    }

    impl SelectedGenerationSource for TestResolver {
        type Error = &'static str;

        fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
            self.current_selected_plane()
                .map(|publication| publication.stamp())
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: SelectedGenerationStamp,
            image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            let publication = self.current_selected_plane()?;
            Ok(publication.stamp() == expected_stamp
                && publication.metadata().artifact_for_image(image).is_some())
        }
    }

    impl VersionedPlaneSelectionResolver for TestResolver {
        fn current_selected_plane(
            &mut self,
        ) -> Result<SelectedVersionedPlanePublication, Self::Error> {
            match self.publications.len() {
                0 => Err("no test selection"),
                1 => self
                    .publications
                    .front()
                    .cloned()
                    .ok_or("no test selection"),
                _ => self.publications.pop_front().ok_or("no test selection"),
            }
        }
    }

    struct Fixture {
        path: PathBuf,
        store: FileStore,
        publication: SelectedVersionedPlanePublication,
        request: SemanticRangeRequest,
        image: SemanticPlaneImageKey,
        expected_environment: [u8; 32],
        expected_target_platform: [u8; 32],
        payload: Vec<u8>,
        object_id: [u8; 32],
    }

    fn path() -> PathBuf {
        let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-locald-versioned-plane-{}-{serial}",
            std::process::id()
        ))
    }

    fn fixture() -> Fixture {
        let path = path();
        let store = FileStore::open(&path, 8 * 1024 * 1024).expect("open test CAS");
        let kind = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [0x21; 32],
                [0x22; 32],
                [0x25; 32],
                4,
                EmbeddingNormalization::L2,
                [0x23; 32],
                [0x24; 32],
            )
            .expect("nonzero vector dimension"),
        );
        let payload = b"asymmetric-embedding-payload".to_vec();
        let segment = SemanticPlaneSegment::from_payload(kind, [0x31; 32], [0x32; 32], 1, &payload)
            .expect("checked embedding segment");
        let semantic_generation =
            GenerationId::from_canonical_bytes(b"exact canonical semantic image");
        let expected_environment = [0x51; 32];
        let expected_target_platform = [0x61; 32];
        let manifest = SemanticPlaneManifest::new(
            semantic_generation,
            SemanticBuildIdentity::new(
                [0x11; 32],
                [0x12; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                Stage::LowerIr,
                [0x13; 32],
                [0x14; 32],
                expected_environment,
                expected_target_platform,
            ),
            SemanticInputWitness::claimed([0x15; 32], ScopeRoot::from_bytes([0x16; 32])),
            vec![
                SemanticPlane::claimed(kind, vec![segment], Coverage::Partial)
                    .expect("claim-only plane"),
            ],
        )
        .expect("versioned embedding manifest");
        let manifest_bytes = manifest.encode().expect("encode manifest");
        let segment_id = segment.admit(kind, &payload).expect("admit segment bytes");
        let plane_publication = VersionedPlanePublication::from_payloads(
            &manifest_bytes,
            [(segment_id, payload.as_slice())],
        )
        .expect("build checked logical-to-physical refs");
        for object in plane_publication.objects() {
            store.write_object(object).expect("persist segment object");
        }
        let metadata = plane_publication.metadata().clone();
        let object_id = *metadata
            .members()
            .next()
            .expect("one segment ref")
            .object_id();
        let image = metadata.artifacts()[0].image_key();
        let stamp = SelectedGenerationStamp::checked(
            [0x80; 16],
            manifest.build().profile(),
            [0x90; 32],
            7,
            [0xA1; 32],
            [0xA2; 32],
            metadata.catalog_root(),
        )
        .expect("valid test selection stamp");
        let publication = SelectedVersionedPlanePublication {
            stamp,
            metadata,
            environment: expected_environment,
            target_platform: expected_target_platform,
            storage_selection: None,
        };
        let request = SemanticRangeRequest {
            manifest_root: manifest.root(),
            plane: kind,
            segment_id: segment.id_claim(),
            first_key: *segment.first_key(),
            last_key: *segment.last_key(),
            byte_length: segment.byte_length(),
        };
        Fixture {
            path,
            store,
            publication,
            request,
            image,
            expected_environment,
            expected_target_platform,
            payload,
            object_id,
        }
    }

    fn remote_selection() -> super::super::s3_publication::RemoteClosureSelection {
        super::super::s3_publication::RemoteClosureSelection {
            closure: [0xA2; 32],
            target_root: [0xA1; 32],
            candidate_id: [0x84; 32],
            namespace_id: [0x80; 16],
            generation: 7,
            attempt_id: [0x81; 16],
            epoch: 9,
            attempt_fence: [0x82; 32],
            input_digest: [0x83; 32],
        }
    }

    fn fixture_with_remote_selection(fixture: &Fixture) -> SelectedVersionedPlanePublication {
        let mut publication = fixture.publication.clone();
        publication.storage_selection = Some(remote_selection());
        publication
    }

    fn changed_selection(
        publication: &SelectedVersionedPlanePublication,
        selected_root: [u8; 32],
        closure_id: [u8; 32],
        revision: u64,
    ) -> SelectedVersionedPlanePublication {
        let stamp = SelectedGenerationStamp::checked(
            *publication.stamp.namespace(),
            publication.stamp.profile(),
            *publication.stamp.source_coordinate(),
            revision,
            selected_root,
            closure_id,
            publication.stamp.catalog_root(),
        )
        .expect("valid changed selection stamp");
        SelectedVersionedPlanePublication {
            stamp,
            metadata: publication.metadata.clone(),
            environment: publication.environment,
            target_platform: publication.target_platform,
            storage_selection: publication.storage_selection,
        }
    }

    fn service_range(
        fixture: &Fixture,
        resolver: &mut TestResolver,
    ) -> Result<Vec<u8>, VersionedPlaneServiceError> {
        VersionedPlaneService::new(fixture.store.clone()).serve_range_claim(
            resolver,
            fixture.publication.stamp(),
            fixture.image,
            fixture.request,
            ByteRange::new(3, 8).expect("bounded fixture range"),
            fixture.expected_environment,
            fixture.expected_target_platform,
        )
    }

    fn target() -> SemanticTargetKey {
        SemanticTargetKey::new(
            "pkg:cargo/widget",
            "registry:crates-io",
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("valid test target")
    }

    fn catalog_get(
        fixture: &Fixture,
        expected: Option<(SelectedGenerationStamp, u64)>,
        byte_range: ByteRange,
    ) -> SemanticCatalogGet {
        let (selected_stamp, total_length) = expected
            .map(|(stamp, length)| (Some(stamp), Some(length)))
            .unwrap_or((None, None));
        SemanticCatalogGet {
            request_id: 1,
            target: target(),
            selected_stamp,
            catalog_root: selected_stamp.map(|_| fixture.publication.metadata().catalog_root()),
            total_length,
            byte_range,
        }
    }

    fn manifest_get(fixture: &Fixture, byte_range: ByteRange) -> SemanticManifestGet {
        let manifest_length = fixture.publication.metadata().artifacts()[0]
            .manifest_bytes()
            .len() as u64;
        SemanticManifestGet {
            request_id: 2,
            target: target(),
            selected_stamp: fixture.publication.stamp(),
            catalog_root: fixture.publication.metadata().catalog_root(),
            image: fixture.image,
            total_length: manifest_length,
            byte_range,
        }
    }

    #[test]
    fn manifest_bootstrap_discovers_then_requires_exact_page_selection() {
        let fixture = fixture();
        let service = VersionedPlaneService::new(fixture.store.clone());
        let artifact = &fixture.publication.metadata().artifacts()[0];
        let catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(fixture.image, artifact.manifest_bytes().len() as u32)
                .expect("catalog manifest length"),
        ])
        .expect("canonical image catalog");
        let expected_bytes = catalog.encode().expect("catalog encoding");
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let first = service
            .serve_catalog_get(
                &mut resolver,
                &catalog_get(
                    &fixture,
                    None,
                    ByteRange::new(0, 16).expect("bounded catalog page"),
                ),
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("discover first catalog page");
        assert_eq!(first.payload, expected_bytes[..16]);
        assert_eq!(first.byte_range.start, 0);
        assert_eq!(first.selected_stamp, fixture.publication.stamp());
        assert_eq!(first.catalog_root, catalog.root());
        assert_eq!(first.total_length as usize, expected_bytes.len());

        let mut resolver = make_resolver([fixture.publication.clone()]);
        let next = service
            .serve_catalog_get(
                &mut resolver,
                &catalog_get(
                    &fixture,
                    Some((first.selected_stamp, first.total_length)),
                    ByteRange::new(16, 16).expect("bounded next catalog page"),
                ),
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("fetch next page from exact selected catalog");
        assert_eq!(next.payload, expected_bytes[16..32]);

        let changed = changed_selection(&fixture.publication, [0xB1; 32], [0xB2; 32], 8);
        let mut resolver = make_resolver([changed]);
        assert_eq!(
            service.serve_catalog_get(
                &mut resolver,
                &catalog_get(
                    &fixture,
                    Some((first.selected_stamp, first.total_length)),
                    ByteRange::new(16, 16).expect("bounded next catalog page"),
                ),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::StaleSelection),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn manifest_page_is_rejected_if_head_changes_during_read() {
        let fixture = fixture();
        let changed = changed_selection(&fixture.publication, [0xB1; 32], [0xB2; 32], 8);
        let mut resolver = make_resolver([fixture.publication.clone(), changed]);
        assert_eq!(
            VersionedPlaneService::new(fixture.store.clone()).serve_catalog_get(
                &mut resolver,
                &catalog_get(
                    &fixture,
                    None,
                    ByteRange::new(0, 16).expect("bounded catalog page"),
                ),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::StaleSelection),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn catalog_page_enforces_range_and_environment_bounds() {
        let fixture = fixture();
        let service = VersionedPlaneService::new(fixture.store.clone());
        let mut resolver = make_resolver([fixture.publication.clone()]);
        assert_eq!(
            service.serve_catalog_get(
                &mut resolver,
                &catalog_get(
                    &fixture,
                    None,
                    ByteRange::new(0, 16 * 1024 + 1).expect("overlarge range shape"),
                ),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::RangeBounds),
        );
        let mut resolver = make_resolver([fixture.publication.clone()]);
        assert_eq!(
            service.serve_catalog_get(
                &mut resolver,
                &catalog_get(
                    &fixture,
                    None,
                    ByteRange::new(0, 16).expect("bounded catalog page"),
                ),
                [0xFF; 32],
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::PlatformOrEnvironmentMismatch),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    fn object_path(store: &FileStore, id: &[u8; 32]) -> PathBuf {
        let mut encoded = String::with_capacity(64);
        for byte in id {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("write object ID");
        }
        store
            .root()
            .join("objects")
            .join(format!("{encoded}.object"))
    }

    fn make_resolver(
        publications: impl IntoIterator<Item = SelectedVersionedPlanePublication>,
    ) -> TestResolver {
        TestResolver {
            publications: publications.into_iter().collect(),
        }
    }

    #[test]
    fn serves_asymmetric_embedding_ranges_after_cold_cas_reopen() {
        let fixture = fixture();
        drop(fixture.store);
        let reopened_store =
            FileStore::open(&fixture.path, 8 * 1024 * 1024).expect("cold reopen CAS");
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let actual = VersionedPlaneService::new(reopened_store)
            .serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(3, 8).expect("bounded fixture range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("serve selected embedding range");
        assert_eq!(actual, fixture.payload[3..11]);
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn cold_restart_hydrates_missing_selected_segment_from_exact_s3_selection() {
        let fixture = fixture();
        let publication = fixture_with_remote_selection(&fixture);
        fs::remove_file(object_path(&fixture.store, &fixture.object_id))
            .expect("remove local segment before restart");
        drop(fixture.store);
        let reopened_store = FileStore::open(&fixture.path, 8 * 1024 * 1024)
            .expect("reopen local CAS after cold restart");
        let hydrator = FakeS3Hydrator {
            expected_selection: remote_selection(),
            expected_object_id: fixture.object_id,
            payload: fixture.payload.clone(),
        };
        let service = VersionedPlaneService::with_cache_and_hydrator(
            reopened_store,
            Arc::new(VerifiedSegmentCache::default()),
            Some(&hydrator),
        );
        let mut resolver = make_resolver([publication.clone()]);
        let actual = service
            .serve_range_claim(
                &mut resolver,
                publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(3, 8).expect("bounded fixture range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("hydrate exact selected segment from cold S3 pack");
        assert_eq!(actual, fixture.payload[3..11]);
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn rejects_tampered_and_short_cold_s3_segment_payloads() {
        for truncate in [false, true] {
            let fixture = fixture();
            let publication = fixture_with_remote_selection(&fixture);
            fs::remove_file(object_path(&fixture.store, &fixture.object_id))
                .expect("remove local segment");
            drop(fixture.store);
            let mut payload = fixture.payload.clone();
            if truncate {
                payload.pop();
            } else {
                payload[0] ^= 0x80;
            }
            let hydrator = FakeS3Hydrator {
                expected_selection: remote_selection(),
                expected_object_id: fixture.object_id,
                payload,
            };
            let service = VersionedPlaneService::with_cache_and_hydrator(
                FileStore::open(&fixture.path, 8 * 1024 * 1024).expect("cold reopen CAS"),
                Arc::new(VerifiedSegmentCache::default()),
                Some(&hydrator),
            );
            let mut resolver = make_resolver([publication.clone()]);
            let result = service.serve_range_claim(
                &mut resolver,
                publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(0, 4).expect("bounded fixture range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            );
            assert!(
                result.is_err(),
                "tampered or short S3 payload must fail closed"
            );
            fs::remove_dir_all(&fixture.path).expect("remove test CAS");
        }
    }

    #[test]
    fn cold_s3_hydration_rejects_the_wrong_selection_fence() {
        let fixture = fixture();
        let mut publication = fixture_with_remote_selection(&fixture);
        let mut wrong = remote_selection();
        wrong.attempt_fence = [0x84; 32];
        publication.storage_selection = Some(wrong);
        fs::remove_file(object_path(&fixture.store, &fixture.object_id))
            .expect("remove local segment");
        drop(fixture.store);
        let hydrator = FakeS3Hydrator {
            expected_selection: remote_selection(),
            expected_object_id: fixture.object_id,
            payload: fixture.payload.clone(),
        };
        let service = VersionedPlaneService::with_cache_and_hydrator(
            FileStore::open(&fixture.path, 8 * 1024 * 1024).expect("cold reopen CAS"),
            Arc::new(VerifiedSegmentCache::default()),
            Some(&hydrator),
        );
        let mut resolver = make_resolver([publication.clone()]);
        assert!(matches!(
            service.serve_range_claim(
                &mut resolver,
                publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(0, 4).expect("bounded fixture range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::S3(_)),
        ));
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn verified_segment_bytes_are_reused_across_pages_but_not_across_cold_reopen() {
        let fixture = fixture();
        let shared = Arc::new(VerifiedSegmentCache::default());
        let first_service =
            VersionedPlaneService::with_cache(fixture.store.clone(), shared.clone());
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let first = first_service
            .serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(0, 7).expect("first range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("admit first page from CAS");
        assert_eq!(first, fixture.payload[..7]);

        // A second operation has a new service instance, just as the local RPC
        // handler does. Once admitted, the immutable verified bytes remain a
        // usable process-local tier even if their disk copy disappears. A cold
        // reopen has no such proof and must reject the missing object.
        fs::remove_file(object_path(&fixture.store, &fixture.object_id))
            .expect("remove physical CAS object after admission");
        let warm_service = VersionedPlaneService::with_cache(fixture.store.clone(), shared);
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let warm = warm_service
            .serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(7, 9).expect("second range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("reuse admitted segment without another physical read");
        assert_eq!(warm, fixture.payload[7..16]);

        let changed = changed_selection(&fixture.publication, [0xA1; 32], [0xA2; 32], 9);
        let mut resolver = make_resolver([fixture.publication.clone(), changed]);
        assert_eq!(
            warm_service.serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(16, 4).expect("third range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::StaleSelection),
            "a warm cache hit must still recheck the live selected head"
        );

        let cold_service = VersionedPlaneService::new(
            FileStore::open(&fixture.path, 8 * 1024 * 1024).expect("cold reopen CAS"),
        );
        let mut resolver = make_resolver([fixture.publication.clone()]);
        assert!(matches!(
            cold_service.serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(7, 9).expect("same second range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::Store(_)),
        ));
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn verified_segment_cache_evicts_to_an_exact_resident_byte_budget() {
        let cache = VerifiedSegmentCache::default();
        let plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [1; 32],
                [2; 32],
                [3; 32],
                4,
                EmbeddingNormalization::L2,
                [4; 32],
                [5; 32],
            )
            .expect("valid embedding plane"),
        );
        for index in 0..=32_u8 {
            let mut object_id = [0; 32];
            object_id[0] = index;
            cache.admit(
                object_id,
                VerifiedSegment {
                    bytes: Arc::from(vec![index; 1024 * 1024]),
                    segment_id: [index; 32],
                    plane,
                },
            );
        }
        let state = cache.state.lock().expect("cache state");
        let mut last = [0; 32];
        last[0] = 32;
        assert_eq!(state.resident_bytes, VERIFIED_SEGMENT_CACHE_BYTES);
        assert_eq!(state.entries.len(), 32);
        assert!(state.entries.peek(&[0; 32]).is_none());
        assert!(state.entries.peek(&last).is_some());
    }

    #[test]
    fn verified_segment_cache_promotes_hits_and_bounds_tiny_segment_metadata() {
        let cache = VerifiedSegmentCache::default();
        let plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [1; 32],
                [2; 32],
                [3; 32],
                4,
                EmbeddingNormalization::L2,
                [4; 32],
                [5; 32],
            )
            .expect("valid embedding plane"),
        );
        for index in 0..VERIFIED_SEGMENT_CACHE_ENTRIES {
            let mut object_id = [0; 32];
            object_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
            cache.admit(
                object_id,
                VerifiedSegment {
                    bytes: Arc::from(vec![index as u8]),
                    segment_id: object_id,
                    plane,
                },
            );
        }
        let mut recently_used = [0; 32];
        recently_used[..8].copy_from_slice(&0_u64.to_be_bytes());
        assert!(cache.get(recently_used).is_some());

        let mut newest = [0; 32];
        newest[..8].copy_from_slice(&(VERIFIED_SEGMENT_CACHE_ENTRIES as u64).to_be_bytes());
        cache.admit(
            newest,
            VerifiedSegment {
                bytes: Arc::from(vec![0xFF]),
                segment_id: newest,
                plane,
            },
        );
        let state = cache.state.lock().expect("cache state");
        assert_eq!(state.entries.len(), VERIFIED_SEGMENT_CACHE_ENTRIES);
        let mut least_recently_used = [0; 32];
        least_recently_used[..8].copy_from_slice(&1_u64.to_be_bytes());
        assert!(state.entries.peek(&recently_used).is_some());
        assert!(state.entries.peek(&least_recently_used).is_none());
        assert!(state.entries.peek(&newest).is_some());
        assert_eq!(state.resident_bytes, VERIFIED_SEGMENT_CACHE_ENTRIES);
    }

    #[test]
    fn verified_segment_cache_keeps_the_first_value_for_a_duplicate_object_id() {
        let cache = VerifiedSegmentCache::default();
        let object_id = [0xA5; 32];
        let plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [1; 32],
                [2; 32],
                [3; 32],
                4,
                EmbeddingNormalization::L2,
                [4; 32],
                [5; 32],
            )
            .expect("valid embedding plane"),
        );
        let other_plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [11; 32],
                [12; 32],
                [13; 32],
                4,
                EmbeddingNormalization::L2,
                [14; 32],
                [15; 32],
            )
            .expect("valid other embedding plane"),
        );
        cache.admit(
            object_id,
            VerifiedSegment {
                bytes: Arc::from(vec![1, 2, 3]),
                segment_id: [0x11; 32],
                plane,
            },
        );
        cache.admit(
            object_id,
            VerifiedSegment {
                bytes: Arc::from(vec![1, 2, 3]),
                segment_id: [0x22; 32],
                plane: other_plane,
            },
        );

        let state = cache.state.lock().expect("cache state");
        let resident = state.entries.peek(&object_id).expect("resident segment");
        assert_eq!(resident.bytes.as_ref(), &[1, 2, 3]);
        assert_eq!(resident.segment_id, [0x11; 32]);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.resident_bytes, 3);
    }

    #[test]
    fn verified_segment_cache_re_admits_shared_object_for_another_plane_generation() {
        let cache = VerifiedSegmentCache::default();
        let payload = b"same physical bytes in two selected generations";
        let first_plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [1; 32],
                [2; 32],
                [3; 32],
                4,
                EmbeddingNormalization::L2,
                [4; 32],
                [5; 32],
            )
            .expect("valid first embedding plane"),
        );
        let second_plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [11; 32],
                [12; 32],
                [13; 32],
                4,
                EmbeddingNormalization::L2,
                [14; 32],
                [15; 32],
            )
            .expect("valid second embedding plane"),
        );
        let first_segment =
            SemanticPlaneSegment::from_payload(first_plane, [0x31; 32], [0x32; 32], 1, payload)
                .expect("first generation segment");
        let second_segment =
            SemanticPlaneSegment::from_payload(second_plane, [0x31; 32], [0x32; 32], 1, payload)
                .expect("second generation segment");
        assert_ne!(
            first_segment.id_claim().as_bytes(),
            second_segment.id_claim().as_bytes(),
            "logical identity binds plane metadata"
        );

        // The physical object ID binds the shared segment schema and payload,
        // so the same object can be referenced by either plane generation.
        let key =
            ObjectKey::<backend_semantic::ir::VersionedPlaneSegmentSchema>::from_value(payload);
        let object_id = *TypedObject::from_value(&key, payload).id().as_bytes();
        cache.admit(
            object_id,
            VerifiedSegment {
                bytes: Arc::<[u8]>::from(&payload[..]),
                segment_id: *first_segment.id_claim().as_bytes(),
                plane: first_plane,
            },
        );

        let shared = cache.get(object_id).expect("shared physical object hit");
        let admitted = second_segment
            .admit(second_plane, &shared.bytes)
            .expect("admit bytes under second generation's plane identity");
        assert_eq!(admitted.as_bytes(), second_segment.id_claim().as_bytes());
        assert_ne!(*admitted.as_bytes(), shared.segment_id);
        assert_ne!(shared.plane, second_plane);
        assert_eq!(shared.bytes.as_ref(), payload);
    }

    #[test]
    #[ignore = "manual microbenchmark; compares cache hit-order cost"]
    fn verified_segment_cache_hit_order_microbenchmark() {
        const ENTRY_COUNT: usize = VERIFIED_SEGMENT_CACHE_ENTRIES;
        const BATCH_COUNT: usize = 25;

        #[derive(Default)]
        struct LegacyCacheState {
            entries: BTreeMap<[u8; 32], VerifiedSegment>,
            oldest_first: VecDeque<[u8; 32]>,
        }

        let object_id_for = |index: usize| {
            let mut object_id = [0; 32];
            object_id[..8].copy_from_slice(
                &u64::try_from(index)
                    .expect("benchmark index fits u64")
                    .to_be_bytes(),
            );
            object_id
        };
        let trace: Vec<_> = (0..ENTRY_COUNT).rev().map(object_id_for).collect();
        let plane = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [1; 32],
                [2; 32],
                [3; 32],
                4,
                EmbeddingNormalization::L2,
                [4; 32],
                [5; 32],
            )
            .expect("valid benchmark plane"),
        );
        let shared_bytes: Arc<[u8]> = Arc::from(vec![0x5A]);
        let mut legacy_samples = Vec::with_capacity(BATCH_COUNT);
        let mut hashlink_samples = Vec::with_capacity(BATCH_COUNT);

        for batch in 0..BATCH_COUNT {
            let mut legacy_state = LegacyCacheState::default();
            let cache = VerifiedSegmentCache::default();
            for index in 0..ENTRY_COUNT {
                let object_id = object_id_for(index);
                let segment = VerifiedSegment {
                    bytes: Arc::clone(&shared_bytes),
                    segment_id: object_id,
                    plane,
                };
                legacy_state.entries.insert(object_id, segment.clone());
                legacy_state.oldest_first.push_back(object_id);
                cache.admit(object_id, segment);
            }
            let legacy_state = Mutex::new(legacy_state);

            // Reverse insertion order drives the legacy queue scan across
            // almost every node. Both implementations process the same 4,096
            // hits and clone the same small VerifiedSegment on each hit.
            let run_legacy = || {
                let started = Instant::now();
                for object_id in &trace {
                    let hit = {
                        let mut state = legacy_state.lock().expect("legacy cache state");
                        let hit = state
                            .entries
                            .get(object_id)
                            .cloned()
                            .expect("benchmark legacy hit");
                        let position = state
                            .oldest_first
                            .iter()
                            .position(|candidate| candidate == object_id)
                            .expect("benchmark LRU node");
                        state.oldest_first.remove(position);
                        state.oldest_first.push_back(*object_id);
                        hit
                    };
                    std::hint::black_box(hit);
                }
                started.elapsed().as_nanos()
            };
            let run_hashlink = || {
                let started = Instant::now();
                for object_id in &trace {
                    std::hint::black_box(cache.get(*object_id));
                }
                started.elapsed().as_nanos()
            };

            if batch % 2 == 0 {
                legacy_samples.push(run_legacy());
                hashlink_samples.push(run_hashlink());
            } else {
                hashlink_samples.push(run_hashlink());
                legacy_samples.push(run_legacy());
            }
        }

        let percentile = |samples: &mut [u128], percent: usize| {
            samples.sort_unstable();
            let rank = samples.len().saturating_mul(percent).div_ceil(100);
            samples[rank.saturating_sub(1)]
        };
        let legacy_median = percentile(&mut legacy_samples, 50);
        let legacy_p95 = percentile(&mut legacy_samples, 95);
        let hashlink_median = percentile(&mut hashlink_samples, 50);
        let hashlink_p95 = percentile(&mut hashlink_samples, 95);
        eprintln!(
            "verified segment cache hit-order benchmark, ns per batch ({BATCH_COUNT} batches x {ENTRY_COUNT} hits): legacy median={legacy_median} p95={legacy_p95}; hashlink median={hashlink_median} p95={hashlink_p95}"
        );
    }

    #[test]
    fn rechecks_selected_head_after_read_and_rejects_same_generation_new_head() {
        let fixture = fixture();
        let changed = changed_selection(&fixture.publication, [0xB1; 32], [0xB2; 32], 8);
        assert_ne!(fixture.publication.stamp(), changed.stamp());
        let mut resolver = make_resolver([fixture.publication.clone(), changed]);
        assert_eq!(
            service_range(&fixture, &mut resolver),
            Err(VersionedPlaneServiceError::StaleSelection),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn rejects_environment_or_platform_mismatch_before_read() {
        let fixture = fixture();
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let result = VersionedPlaneService::new(fixture.store.clone()).serve_range_claim(
            &mut resolver,
            fixture.publication.stamp(),
            fixture.image,
            fixture.request,
            ByteRange::new(0, 4).expect("bounded fixture range"),
            [0x52; 32],
            fixture.expected_target_platform,
        );
        assert_eq!(
            result,
            Err(VersionedPlaneServiceError::PlatformOrEnvironmentMismatch),
        );
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let result = VersionedPlaneService::new(fixture.store.clone()).serve_range_claim(
            &mut resolver,
            fixture.publication.stamp(),
            fixture.image,
            fixture.request,
            ByteRange::new(0, 4).expect("bounded fixture range"),
            fixture.expected_environment,
            [0x62; 32],
        );
        assert_eq!(
            result,
            Err(VersionedPlaneServiceError::PlatformOrEnvironmentMismatch),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn manifest_pages_bind_exact_image_and_recheck_catalog_head() {
        let fixture = fixture();
        let service = VersionedPlaneService::new(fixture.store.clone());
        let manifest_bytes = fixture.publication.metadata().artifacts()[0].manifest_bytes();
        let get = manifest_get(
            &fixture,
            ByteRange::new(4, 12).expect("bounded manifest range"),
        );
        let mut resolver = make_resolver([fixture.publication.clone()]);
        let chunk = service
            .serve_manifest_get(
                &mut resolver,
                &get,
                fixture.expected_environment,
                fixture.expected_target_platform,
            )
            .expect("serve exact image manifest page");
        assert_eq!(chunk.payload, manifest_bytes[4..16]);
        assert_eq!(chunk.image, fixture.image);

        let changed = changed_selection(&fixture.publication, [0xB1; 32], [0xB2; 32], 8);
        let mut resolver = make_resolver([changed]);
        assert_eq!(
            service.serve_manifest_get(
                &mut resolver,
                &get,
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::StaleSelection),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }

    #[test]
    fn missing_or_corrupt_selected_range_fails_closed() {
        let missing_fixture = fixture();
        fs::remove_file(object_path(
            &missing_fixture.store,
            &missing_fixture.object_id,
        ))
        .expect("remove selected CAS object");
        let mut resolver = make_resolver([missing_fixture.publication.clone()]);
        assert!(matches!(
            service_range(&missing_fixture, &mut resolver),
            Err(VersionedPlaneServiceError::Store(_)),
        ));
        fs::remove_dir_all(&missing_fixture.path).expect("remove missing-object test CAS");

        let corrupt_fixture = fixture();
        fs::write(
            object_path(&corrupt_fixture.store, &corrupt_fixture.object_id),
            b"corrupt",
        )
        .expect("corrupt selected CAS object");
        let mut resolver = make_resolver([corrupt_fixture.publication.clone()]);
        assert!(matches!(
            service_range(&corrupt_fixture, &mut resolver),
            Err(VersionedPlaneServiceError::Store(_)),
        ));
        fs::remove_dir_all(&corrupt_fixture.path).expect("remove corrupt-object test CAS");
    }

    #[test]
    fn rejects_oversized_and_out_of_segment_ranges() {
        let fixture = fixture();
        let service = VersionedPlaneService::new(fixture.store.clone());
        let mut resolver = make_resolver([fixture.publication.clone()]);
        assert_eq!(
            service.serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(0, MAX_RANGE_BYTES + 1).expect("non-overflowing range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::RangeBounds),
        );
        let mut resolver = make_resolver([fixture.publication.clone()]);
        assert_eq!(
            service.serve_range_claim(
                &mut resolver,
                fixture.publication.stamp(),
                fixture.image,
                fixture.request,
                ByteRange::new(fixture.request.byte_length, 1).expect("non-overflowing range"),
                fixture.expected_environment,
                fixture.expected_target_platform,
            ),
            Err(VersionedPlaneServiceError::RangeBounds),
        );
        fs::remove_dir_all(&fixture.path).expect("remove test CAS");
    }
}
