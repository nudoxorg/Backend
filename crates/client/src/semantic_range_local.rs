//! Locald transport and durable admission for selected semantic byte ranges.

use crate::{ClientError, MAX_FRAME};
use backend_engine::cluster_transport::{
    Endpoint, EndpointAddr, RemoteIndexCapability, RemoteIndexChannel, RemoteIndexOutcome,
    RemoteIndexRequest, RemoteIndexSession, RemoteIndexSessionHello, SecretKey, TransportError,
    bind_direct, connect_remote_index, remote_index_now,
};
use backend_replication::{
    AdaptiveIrResidency, ByteRange, DurableSemanticRangeStore, FileSemanticRangeStore,
    HydrationCredits, IrHydrationCursor, IrHydrationError, IrHydrationPoll, IrHydrationRequest,
    IrHydrationTerminal, IrResidencyDeltaHop, IrResidencyError, IrResidencyMetrics,
    IrResidencyPath, LOCAL_CONTROL_MAX_CURSOR, LOCAL_CONTROL_MAX_ERROR, LocalControlClient,
    LocalControlError, LocalControlLimits, LocalControlRequest, LocalControlResponse,
    LocalSemanticGeneration, PreparedIrResidencyDeltaRoute, SelectedGenerationSource,
    SelectedGenerationStamp, SelectedSemanticImageChunk, SelectedSemanticImageGet,
    SelectedSemanticPlane, SemanticCatalogChunk, SemanticCatalogGet, SemanticImageCacheError,
    SemanticManifestChunk, SemanticManifestGet, SemanticRangeChunk, SemanticRangeClientCheckpoint,
    SemanticRangeClientProgress, SemanticRangeGet, SemanticTargetKey, TransportLimits,
    VerifiedSemanticSegment, accept_semantic_range, admit_semantic_catalog,
    admit_semantic_manifest,
};
use backend_semantic::ir::{
    MappedSemanticImage, SemanticPlaneCatalog, SemanticPlaneImageKey, SemanticPlaneKind,
    SemanticPlaneManifest, SemanticPlaneSegment, SemanticSegmentId,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

const CONNECTION_FRAME_BUDGET: usize = 240;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_METADATA_PAGE_BYTES: u64 = 16 * 1024;
const MAX_METADATA_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEMANTIC_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
const SELECTED_IMAGE_RESIDENCE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SELECTED_IMAGE_RESIDENCE_MAX_ENTRIES: usize = 8;

/// Complete image identity within one exact selected authority frontier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SelectedImageResidenceKey {
    selected_stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
}

impl SelectedImageResidenceKey {
    const fn new(selected_stamp: SelectedGenerationStamp, image: SemanticPlaneImageKey) -> Self {
        Self {
            selected_stamp,
            image,
        }
    }
}

struct ResidentSemanticImage {
    image: Arc<MappedSemanticImage>,
    bytes: u64,
}

struct PinnedSemanticImage {
    key: SelectedImageResidenceKey,
    image: Weak<MappedSemanticImage>,
    bytes: u64,
}

/// Bounded owner-side LRU for complete images selected by this client.
///
/// `retired` contains weak leases for entries evicted while a caller still
/// holds an `Arc`. Those mappings remain charged until the last lease drops.
struct SelectedImageResidence {
    entries: hashlink::LruCache<SelectedImageResidenceKey, ResidentSemanticImage>,
    retired: Vec<PinnedSemanticImage>,
    resident_bytes: u64,
    pinned_bytes: u64,
    max_bytes: u64,
    max_entries: usize,
}

impl SelectedImageResidence {
    fn new() -> Self {
        Self::with_limits(
            SELECTED_IMAGE_RESIDENCE_MAX_BYTES,
            SELECTED_IMAGE_RESIDENCE_MAX_ENTRIES,
        )
    }

    fn with_limits(max_bytes: u64, max_entries: usize) -> Self {
        Self {
            entries: hashlink::LruCache::new(max_entries.max(1)),
            retired: Vec::new(),
            resident_bytes: 0,
            pinned_bytes: 0,
            max_bytes,
            max_entries: max_entries.max(1),
        }
    }

    fn get(&mut self, key: SelectedImageResidenceKey) -> Option<Arc<MappedSemanticImage>> {
        self.prune_retired();
        if let Some(entry) = self.entries.get(&key) {
            return Some(Arc::clone(&entry.image));
        }
        let pinned_index = self.retired.iter().position(|entry| entry.key == key)?;
        let image = self.retired[pinned_index].image.upgrade()?;
        let bytes = self.retired[pinned_index].bytes;
        self.retired.swap_remove(pinned_index);
        self.pinned_bytes = self.pinned_bytes.saturating_sub(bytes);
        self.make_room_for_entry();
        self.resident_bytes = self.resident_bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            ResidentSemanticImage {
                image: Arc::clone(&image),
                bytes,
            },
        );
        Some(image)
    }

    fn insert(
        &mut self,
        key: SelectedImageResidenceKey,
        image: MappedSemanticImage,
    ) -> Result<Arc<MappedSemanticImage>, MappedSemanticImage> {
        let Some(bytes) = u64::try_from(image.view().as_ref().len()).ok() else {
            return Err(image);
        };
        if bytes == 0 || bytes > self.max_bytes {
            return Err(image);
        }
        self.prune_retired();
        while self.entries.len().saturating_add(self.retired.len()) >= self.max_entries
            || self
                .resident_bytes
                .saturating_add(self.pinned_bytes)
                .saturating_add(bytes)
                > self.max_bytes
        {
            if !self.evict_lru() {
                return Err(image);
            }
        }
        let image = Arc::new(image);
        self.resident_bytes = self.resident_bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            ResidentSemanticImage {
                image: Arc::clone(&image),
                bytes,
            },
        );
        Ok(image)
    }

    fn make_room_for_entry(&mut self) {
        while self.entries.len() >= self.max_entries {
            if !self.evict_lru() {
                break;
            }
        }
    }

    fn evict_lru(&mut self) -> bool {
        let Some((key, entry)) = self.entries.remove_lru() else {
            return false;
        };
        self.resident_bytes = self.resident_bytes.saturating_sub(entry.bytes);
        if Arc::strong_count(&entry.image) > 1 {
            self.pinned_bytes = self.pinned_bytes.saturating_add(entry.bytes);
            self.retired.push(PinnedSemanticImage {
                key,
                image: Arc::downgrade(&entry.image),
                bytes: entry.bytes,
            });
        }
        true
    }

    fn prune_retired(&mut self) {
        let mut released_bytes = 0_u64;
        self.retired.retain(|entry| {
            if entry.image.strong_count() == 0 {
                released_bytes = released_bytes.saturating_add(entry.bytes);
                false
            } else {
                true
            }
        });
        self.pinned_bytes = self.pinned_bytes.saturating_sub(released_bytes);
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.entries.len() + self.retired.len()
    }

    #[cfg(test)]
    fn charged_bytes(&self) -> u64 {
        self.resident_bytes + self.pinned_bytes
    }
}

enum SemanticImageOwner {
    Owned(MappedSemanticImage),
    Shared(Arc<MappedSemanticImage>),
}

/// Result of ensuring one complete selected canonical NXFI is locally mapped.
pub struct SemanticImageFetch {
    image: SemanticImageOwner,
    transferred_bytes: u64,
    page_requests: usize,
}

impl SemanticImageFetch {
    /// Validated zero-copy semantic reader owner.
    #[must_use]
    pub fn image(&self) -> &MappedSemanticImage {
        match &self.image {
            SemanticImageOwner::Owned(image) => image,
            SemanticImageOwner::Shared(image) => image,
        }
    }

    /// Number of bytes fetched over the local control transport.
    #[must_use]
    pub const fn transferred_bytes(&self) -> u64 {
        self.transferred_bytes
    }

    /// Number of selected-image page requests sent over the local control transport.
    #[must_use]
    pub const fn page_requests(&self) -> usize {
        self.page_requests
    }

    fn owned(image: MappedSemanticImage, transferred_bytes: u64, page_requests: usize) -> Self {
        Self {
            image: SemanticImageOwner::Owned(image),
            transferred_bytes,
            page_requests,
        }
    }

    fn shared(
        image: Arc<MappedSemanticImage>,
        transferred_bytes: u64,
        page_requests: usize,
    ) -> Self {
        Self {
            image: SemanticImageOwner::Shared(image),
            transferred_bytes,
            page_requests,
        }
    }
}

/// Fully assembled canonical catalog tied to one observed selected frontier.
/// The stamp is a freshness token; callers must continue checking it through
/// `SelectedGenerationSource` before using image membership or fetching ranges.
#[derive(Clone, Debug)]
pub struct SemanticCatalogSnapshot {
    target: SemanticTargetKey,
    selected_stamp: SelectedGenerationStamp,
    catalog: Arc<SemanticPlaneCatalog>,
}

impl SemanticCatalogSnapshot {
    /// Product target whose selected catalog was admitted.
    #[must_use]
    pub const fn target(&self) -> &SemanticTargetKey {
        &self.target
    }

    /// Exact authority stamp returned by the local owner.
    #[must_use]
    pub const fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.selected_stamp
    }

    /// Canonical catalog after the complete byte sequence matched its root.
    #[must_use]
    pub fn catalog(&self) -> &SemanticPlaneCatalog {
        &self.catalog
    }

    /// Exact selected logical root bound to this semantic catalog.
    #[must_use]
    pub const fn selected_root(&self) -> &[u8; 32] {
        self.selected_stamp.selected_root()
    }
}

fn control_limits() -> LocalControlLimits {
    LocalControlLimits {
        max_frame: MAX_FRAME,
        max_cursor: LOCAL_CONTROL_MAX_CURSOR,
        max_error: LOCAL_CONTROL_MAX_ERROR,
    }
}

fn map_control_error(error: LocalControlError) -> ClientError {
    crate::map_frame(error)
}

/// Client for fetching a bounded semantic range and admitting it into durable CAS.
///
/// The method returns `Complete` only after the exact range has been staged,
/// reconstructed as a complete segment, checked against the selected manifest,
/// committed to durable CAS, and read back. Partial ranges return a durable
/// checkpoint and never expose segment bytes.
#[cfg(any(unix, windows))]
pub struct LocalSemanticRangeTransport {
    client: Option<LocalControlClient<backend_replication::LocalStream>>,
    remote: Option<RemoteSemanticRangeConnection>,
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
    endpoint: Option<PathBuf>,
    frames_on_connection: usize,
    next_request_id: u64,
}

struct RemoteSemanticRangeConnection {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    owner_address: EndpointAddr,
    capability: RemoteIndexCapability,
    session: Option<RemoteIndexSession>,
}

impl RemoteSemanticRangeConnection {
    fn connect(
        secret: SecretKey,
        owner: backend_engine::cluster_transport::EndpointId,
        address: SocketAddr,
        capability: RemoteIndexCapability,
    ) -> Result<Self, ClientError> {
        if capability.claims.semantic.is_none() {
            return Err(ClientError::Protocol(
                "remote semantic transport needs a semantic-hydration capability".to_owned(),
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ClientError::Io(error.to_string()))?;
        let bind_address = match address {
            SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
            SocketAddr::V6(_) => SocketAddr::from(([0_u16; 8], 0)),
        };
        let endpoint = runtime
            .block_on(bind_direct(secret, bind_address))
            .map_err(|error| ClientError::Io(error.to_string()))?;
        capability
            .verify(
                owner,
                endpoint.id(),
                remote_index_now().map_err(map_transport_error)?,
            )
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let mut result = Self {
            runtime,
            endpoint,
            owner_address: EndpointAddr::new(owner).with_ip_addr(address),
            capability,
            session: None,
        };
        result.open()?;
        Ok(result)
    }

    fn open(&mut self) -> Result<(), ClientError> {
        let hello = RemoteIndexSessionHello::new(
            self.capability.clone(),
            RemoteIndexChannel::SemanticHydration,
        )
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let session = self
            .runtime
            .block_on(connect_remote_index(
                &self.endpoint,
                self.owner_address.clone(),
                hello,
            ))
            .map_err(map_remote_index_transport_error)?;
        self.session = Some(session);
        Ok(())
    }

    fn reconnect(&mut self) -> Result<(), ClientError> {
        self.session = None;
        self.open()
    }

    fn request(
        &mut self,
        request: &LocalControlRequest,
    ) -> Result<LocalControlResponse, ClientError> {
        let body = backend_replication::encode_request(request, control_limits())
            .map_err(map_control_error)?;
        let request_id = request.request_id();
        if self.session.is_none() {
            self.open()?;
        }
        let result = {
            let session = self.session.as_mut().ok_or_else(|| {
                ClientError::Io("remote semantic session was not opened".to_owned())
            })?;
            self.runtime.block_on(async {
                session
                    .send_request(&RemoteIndexRequest {
                        request_id,
                        body: body.into_boxed_slice(),
                    })
                    .await?;
                session.receive_response(request_id).await
            })
        };
        let response = match result {
            Ok(response) => Ok(response),
            Err(error) if retryable_remote_index_transport(&error) => {
                self.reconnect()?;
                let session = self.session.as_mut().ok_or_else(|| {
                    ClientError::Io("remote semantic session was not reopened".to_owned())
                })?;
                let body = backend_replication::encode_request(request, control_limits())
                    .map_err(map_control_error)?;
                self.runtime.block_on(async {
                    session
                        .send_request(&RemoteIndexRequest {
                            request_id,
                            body: body.into_boxed_slice(),
                        })
                        .await?;
                    session.receive_response(request_id).await
                })
            }
            Err(error) => return Err(map_remote_index_transport_error(error)),
        }
        .map_err(map_remote_index_transport_error)?;
        match response.outcome {
            RemoteIndexOutcome::Payload(body) => {
                backend_replication::decode_response(&body, control_limits())
                    .map_err(map_control_error)
            }
            RemoteIndexOutcome::StaleSemanticSelection => {
                Ok(LocalControlResponse::SemanticStaleSelection { request_id })
            }
            RemoteIndexOutcome::StaleProductRoot { expected, observed } => {
                Err(ClientError::StaleRemoteRoot { expected, observed })
            }
            RemoteIndexOutcome::StaleProductSnapshot { .. } => {
                Err(ClientError::StaleRemoteCapability)
            }
            RemoteIndexOutcome::Rejected(
                backend_engine::cluster_transport::RemoteIndexReject::StaleCapability,
            ) => Err(ClientError::StaleRemoteCapability),
            RemoteIndexOutcome::Rejected(
                backend_engine::cluster_transport::RemoteIndexReject::CapabilityRevoked,
            ) => Err(ClientError::RemoteCapabilityRevoked),
            RemoteIndexOutcome::Rejected(reason) => Err(ClientError::Protocol(format!(
                "remote semantic request was rejected: {reason:?}"
            ))),
        }
    }
}

fn map_transport_error(error: impl std::fmt::Display) -> ClientError {
    ClientError::Io(error.to_string())
}

fn retryable_remote_index_transport(error: &TransportError) -> bool {
    matches!(error, TransportError::Iroh(_) | TransportError::Io(_))
}

fn map_remote_index_transport_error(error: TransportError) -> ClientError {
    match error {
        TransportError::Iroh(_) | TransportError::Io(_) => {
            ClientError::Disconnected(std::io::ErrorKind::ConnectionReset)
        }
        TransportError::Frame(message) => ClientError::Protocol(message),
        other => ClientError::Protocol(other.to_string()),
    }
}

#[cfg(any(unix, windows))]
impl LocalSemanticRangeTransport {
    /// Connects to and authenticates the locald endpoint.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let endpoint = backend_replication::UnixEndpointRef::new(path.as_ref()).map_err(|_| {
            ClientError::Transport(backend_replication::ReplicationError::MessageTooLarge)
        })?;
        let path = endpoint.as_path();
        let stream = connect_stream(path)?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
            .map_err(crate::map_peer_authentication_error)?;
        Ok(Self {
            client: Some(LocalControlClient::new(stream, control_limits())),
            remote: None,
            peer: Some(peer),
            endpoint: Some(path.to_path_buf()),
            frames_on_connection: 0,
            next_request_id: 1,
        })
    }

    /// Wraps an already connected stream, primarily for embedded owners and tests.
    #[must_use]
    pub fn from_stream(stream: backend_replication::LocalStream) -> Self {
        configure_stream(&stream);
        Self {
            client: Some(LocalControlClient::new(stream, control_limits())),
            remote: None,
            peer: None,
            endpoint: None,
            frames_on_connection: 0,
            next_request_id: 1,
        }
    }

    /// Opens a direct remote semantic-hydration channel under an exact owner grant.
    pub fn connect_remote(
        client_secret: SecretKey,
        owner: backend_engine::cluster_transport::EndpointId,
        address: SocketAddr,
        capability: RemoteIndexCapability,
    ) -> Result<Self, ClientError> {
        let remote =
            RemoteSemanticRangeConnection::connect(client_secret, owner, address, capability)?;
        Ok(Self {
            client: None,
            remote: Some(remote),
            peer: None,
            endpoint: None,
            frames_on_connection: 0,
            next_request_id: 1,
        })
    }

    /// Returns the same-user proof bound to an authenticated local connection.
    #[must_use]
    pub const fn authenticated_peer(&self) -> Option<&backend_replication::AuthenticatedLocalPeer> {
        self.peer.as_ref()
    }

    /// Fetches the selected semantic image catalog in bounded pages and admits
    /// its canonical root only after the complete byte sequence is assembled.
    pub fn fetch_semantic_catalog<A: SelectedGenerationSource>(
        &mut self,
        target: SemanticTargetKey,
        source: &mut A,
    ) -> Result<SemanticCatalogSnapshot, ClientError> {
        let first_range = ByteRange::new(0, 1).map_err(map_wire_error)?;
        let first = SemanticCatalogGet {
            request_id: self.take_request_id()?,
            target: target.clone(),
            selected_stamp: None,
            catalog_root: None,
            total_length: None,
            byte_range: first_range,
        };
        let first_chunk = self.request_catalog_page(&first)?;
        let stamp = first_chunk.selected_stamp;
        let catalog_root = first_chunk.catalog_root;
        let total_length = first_chunk.total_length;
        if total_length == 0 || total_length > MAX_METADATA_TOTAL_BYTES {
            return Err(ClientError::Protocol(
                "semantic catalog length exceeds the client bound".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(usize::try_from(total_length).map_err(|_| {
                ClientError::Protocol("semantic catalog length exceeds address space".to_owned())
            })?)
            .map_err(|_| ClientError::Protocol("semantic catalog allocation failed".to_owned()))?;
        append_metadata_page(&mut bytes, 0, total_length, &first_chunk.payload)?;
        let mut offset = u64::try_from(bytes.len())
            .map_err(|_| ClientError::Protocol("semantic catalog offset overflow".to_owned()))?;
        self.ensure_stamp_current(source, stamp)?;

        while offset < total_length {
            let length = (total_length - offset).min(MAX_METADATA_PAGE_BYTES);
            let get = SemanticCatalogGet {
                request_id: self.take_request_id()?,
                target: target.clone(),
                selected_stamp: Some(stamp),
                catalog_root: Some(catalog_root),
                total_length: Some(total_length),
                byte_range: ByteRange::new(offset, length).map_err(map_wire_error)?,
            };
            let chunk = self.request_catalog_page(&get)?;
            append_metadata_page(&mut bytes, offset, total_length, &chunk.payload)?;
            offset = u64::try_from(bytes.len()).map_err(|_| {
                ClientError::Protocol("semantic catalog offset overflow".to_owned())
            })?;
            self.ensure_stamp_current(source, stamp)?;
        }
        if offset != total_length {
            return Err(ClientError::Protocol(
                "semantic catalog pages did not cover the declared length".to_owned(),
            ));
        }
        let catalog = admit_semantic_catalog(catalog_root, &bytes).map_err(map_wire_error)?;
        if stamp.catalog_root() != catalog.root() {
            return Err(ClientError::Protocol(
                "semantic catalog root differs from the selected stamp".to_owned(),
            ));
        }
        self.ensure_stamp_current(source, stamp)?;
        Ok(SemanticCatalogSnapshot {
            target,
            selected_stamp: stamp,
            catalog: Arc::new(catalog),
        })
    }

    /// Fetches and admits one exact catalog image manifest in bounded pages.
    /// No manifest bytes are returned until the image root and current selected
    /// head have both been rechecked.
    pub fn fetch_semantic_manifest<A: SelectedGenerationSource>(
        &mut self,
        snapshot: &SemanticCatalogSnapshot,
        image: SemanticPlaneImageKey,
        source: &mut A,
    ) -> Result<SemanticPlaneManifest, ClientError> {
        let entry = snapshot
            .catalog
            .entries()
            .iter()
            .find(|entry| entry.image() == image)
            .ok_or_else(|| {
                ClientError::Protocol(
                    "semantic image is absent from the admitted catalog".to_owned(),
                )
            })?;
        let total_length = u64::from(entry.manifest_length());
        if total_length == 0 || total_length > MAX_METADATA_TOTAL_BYTES {
            return Err(ClientError::Protocol(
                "semantic manifest length exceeds the client bound".to_owned(),
            ));
        }
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(usize::try_from(total_length).map_err(|_| {
                ClientError::Protocol("semantic manifest length exceeds address space".to_owned())
            })?)
            .map_err(|_| ClientError::Protocol("semantic manifest allocation failed".to_owned()))?;
        let mut offset = 0_u64;
        while offset < total_length {
            let length = (total_length - offset).min(MAX_METADATA_PAGE_BYTES);
            let get = SemanticManifestGet {
                request_id: self.take_request_id()?,
                target: snapshot.target.clone(),
                selected_stamp: snapshot.selected_stamp,
                catalog_root: snapshot.catalog.root(),
                image,
                total_length,
                byte_range: ByteRange::new(offset, length).map_err(map_wire_error)?,
            };
            get.validate_against_catalog(&snapshot.catalog)
                .map_err(map_wire_error)?;
            let chunk = self.request_manifest_page(&get)?;
            append_metadata_page(&mut bytes, offset, total_length, &chunk.payload)?;
            offset = u64::try_from(bytes.len()).map_err(|_| {
                ClientError::Protocol("semantic manifest offset overflow".to_owned())
            })?;
            self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        }
        if offset != total_length {
            return Err(ClientError::Protocol(
                "semantic manifest pages did not cover the declared length".to_owned(),
            ));
        }
        let manifest = admit_semantic_manifest(image, &bytes).map_err(map_wire_error)?;
        if manifest.build().profile() != snapshot.target.profile() {
            return Err(ClientError::Protocol(
                "semantic manifest profile differs from its catalog target".to_owned(),
            ));
        }
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        Ok(manifest)
    }

    /// Ensures the exact selected canonical NXFI is present in local immutable
    /// storage and returns its mmap-backed reader. Page transfer resumes from
    /// the last synced cursor; cache hits are independently verified by VCS
    /// generation, artifact identity, and complete reader admission.
    pub fn fetch_semantic_image<A: SelectedGenerationSource>(
        &mut self,
        snapshot: &SemanticCatalogSnapshot,
        image: SemanticPlaneImageKey,
        source: &mut A,
        store: &FileSemanticRangeStore,
        max_bytes: u64,
        max_pages: usize,
    ) -> Result<SemanticImageFetch, ClientError> {
        if snapshot
            .catalog
            .entries()
            .iter()
            .all(|entry| entry.image() != image)
        {
            return Err(ClientError::Protocol(
                "semantic image is absent from the admitted catalog".to_owned(),
            ));
        }
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        match store.find_semantic_image(&snapshot.target, image) {
            Ok(Some(mapped)) => {
                self.ensure_image_current(source, snapshot.selected_stamp, image)?;
                return Ok(SemanticImageFetch::owned(mapped, 0, 0));
            }
            Ok(None)
            | Err(SemanticImageCacheError::CorruptContent { .. })
            | Err(SemanticImageCacheError::SelectionMismatch { .. }) => {}
            Err(SemanticImageCacheError::TransferRejected(error)) => {
                return Err(ClientError::Protocol(error));
            }
            Err(SemanticImageCacheError::Storage(error)) => {
                return Err(ClientError::Io(error));
            }
        }

        let mut transferred_bytes = 0_u64;
        let mut page_requests = 0_usize;
        let mut resume = store
            .resume_semantic_image_transfer(&snapshot.target, image)
            .map_err(ClientError::Io)?;
        // A fresh owner page binds the content identity before even a complete
        // cold-resumed stage can replace or invalidate a content-cache entry.
        let get = SelectedSemanticImageGet {
            request_id: self.take_request_id()?,
            target: snapshot.target.clone(),
            selected_stamp: snapshot.selected_stamp,
            image,
            image_identity: None,
            total_length: None,
            byte_range: ByteRange::new(0, 1).map_err(map_wire_error)?,
        };
        ensure_image_budget(
            &get,
            &mut transferred_bytes,
            &mut page_requests,
            max_bytes,
            max_pages,
        )?;
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        let chunk = self.request_selected_image_page(&get)?;
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        let cached = match store.find_semantic_image_identity(
            &snapshot.target,
            chunk.image_identity,
            image.semantic_generation(),
        ) {
            Ok(cached) => cached,
            Err(SemanticImageCacheError::CorruptContent { .. }) => {
                store
                    .repair_corrupt_semantic_image(
                        &snapshot.target,
                        image,
                        snapshot.selected_stamp,
                        &chunk,
                    )
                    .map_err(map_image_cache_error)?;
                None
            }
            Err(error) => return Err(map_image_cache_error(error)),
        };
        if let Some(cached) = cached {
            if u64::try_from(cached.view().as_ref().len()).ok() != Some(chunk.total_length) {
                return Err(ClientError::Protocol(
                    "cached semantic image length differs from the selected page".to_owned(),
                ));
            }
            match store.bind_semantic_image_identity(&snapshot.target, image, chunk.image_identity)
            {
                Ok(mapped) => {
                    if resume.is_some() {
                        store
                            .discard_semantic_image_transfer(&snapshot.target, image)
                            .map_err(ClientError::Io)?;
                    }
                    self.ensure_image_current(source, snapshot.selected_stamp, image)?;
                    return Ok(SemanticImageFetch::owned(
                        mapped,
                        transferred_bytes,
                        page_requests,
                    ));
                }
                Err(SemanticImageCacheError::CorruptContent { .. }) => {
                    store
                        .repair_corrupt_semantic_image(
                            &snapshot.target,
                            image,
                            snapshot.selected_stamp,
                            &chunk,
                        )
                        .map_err(map_image_cache_error)?;
                }
                Err(error) => return Err(map_image_cache_error(error)),
            }
        }

        if resume.as_ref().is_some_and(|resume| {
            resume.identity() != chunk.image_identity || resume.total_length() != chunk.total_length
        }) {
            store
                .discard_semantic_image_transfer(&snapshot.target, image)
                .map_err(ClientError::Io)?;
            resume = None;
        }
        resume = Some(
            store
                .stage_semantic_image_page(
                    &snapshot.target,
                    image,
                    chunk.image_identity,
                    chunk.total_length,
                    chunk.byte_range,
                    &chunk.payload,
                )
                .map_err(ClientError::Io)?,
        );

        let mut resume = resume.ok_or_else(|| {
            ClientError::Protocol("semantic-image first page did not create a stage".to_owned())
        })?;
        if resume.image() != image
            || resume.total_length() == 0
            || resume.total_length() > MAX_SEMANTIC_IMAGE_BYTES
            || resume.next_offset() > resume.total_length()
        {
            return Err(ClientError::Protocol(
                "durable semantic-image cursor differs from the selected image".to_owned(),
            ));
        }
        while resume.next_offset() < resume.total_length() {
            let offset = resume.next_offset();
            let length = (resume.total_length() - offset).min(MAX_METADATA_PAGE_BYTES);
            let get = SelectedSemanticImageGet {
                request_id: self.take_request_id()?,
                target: snapshot.target.clone(),
                selected_stamp: snapshot.selected_stamp,
                image,
                image_identity: Some(resume.identity()),
                total_length: Some(resume.total_length()),
                byte_range: ByteRange::new(offset, length).map_err(map_wire_error)?,
            };
            ensure_image_budget(
                &get,
                &mut transferred_bytes,
                &mut page_requests,
                max_bytes,
                max_pages,
            )?;
            self.ensure_image_current(source, snapshot.selected_stamp, image)?;
            let chunk = self.request_selected_image_page(&get)?;
            self.ensure_image_current(source, snapshot.selected_stamp, image)?;
            resume = store
                .stage_semantic_image_page(
                    &snapshot.target,
                    image,
                    chunk.image_identity,
                    chunk.total_length,
                    chunk.byte_range,
                    &chunk.payload,
                )
                .map_err(ClientError::Io)?;
        }
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        let mapped = match store.finish_semantic_image_transfer(&snapshot.target, resume) {
            Ok(mapped) => mapped,
            Err(SemanticImageCacheError::CorruptContent { .. }) => {
                // The shared content-addressed object may have changed after
                // the earlier cache lookup. Recheck selected authority before
                // using the first-page witness to invalidate it, then admit
                // the already complete canonical stage.
                self.ensure_image_current(source, snapshot.selected_stamp, image)?;
                store
                    .repair_corrupt_semantic_image(
                        &snapshot.target,
                        image,
                        snapshot.selected_stamp,
                        &chunk,
                    )
                    .map_err(map_image_cache_error)?;
                store
                    .finish_semantic_image_transfer(&snapshot.target, resume)
                    .map_err(map_image_cache_error)?
            }
            Err(error) => return Err(map_image_cache_error(error)),
        };
        self.ensure_image_current(source, snapshot.selected_stamp, image)?;
        Ok(SemanticImageFetch::owned(
            mapped,
            transferred_bytes,
            page_requests,
        ))
    }

    fn read_selected_stamp(
        &mut self,
        target: &SemanticTargetKey,
    ) -> Result<SelectedGenerationStamp, ClientError> {
        let get = SemanticCatalogGet {
            request_id: self.take_request_id()?,
            target: target.clone(),
            selected_stamp: None,
            catalog_root: None,
            total_length: None,
            byte_range: ByteRange::new(0, 1).map_err(map_wire_error)?,
        };
        let chunk = self.request_catalog_page(&get)?;
        if chunk.selected_stamp.profile() != target.profile() {
            return Err(ClientError::Protocol(
                "selected semantic profile differs from the requested target".to_owned(),
            ));
        }
        Ok(chunk.selected_stamp)
    }

    /// Fetches one cursor-requested range and passes it through durable sparse
    /// CAS admission. The owner rechecks the selected Turso head before and
    /// after serving the range; the client rechecks its source around the
    /// network request and before acknowledging complete segment coverage.
    pub fn request_and_accept<A, C>(
        &mut self,
        target: backend_replication::SemanticTargetKey,
        cursor: &mut IrHydrationCursor<'_, '_>,
        source: &mut A,
        request: &IrHydrationRequest,
        store: &mut C,
        limits: TransportLimits,
    ) -> Result<SemanticRangeClientProgress, ClientError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
    {
        self.prepare_request()?;
        let request_id = self.take_request_id()?;
        let get = SemanticRangeGet::from_request(request_id, target, request, source)
            .map_err(map_hydration_error)?;
        let chunk = self.request(&get)?;
        accept_semantic_range(cursor, source, request, &get, &chunk, store, limits)
            .map_err(map_hydration_error)
    }

    fn request(&mut self, get: &SemanticRangeGet) -> Result<SemanticRangeChunk, ClientError> {
        let payload = get
            .encode()
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let response = self.request_control(&LocalControlRequest::SemanticRangeGet {
            request_id: get.request_id,
            payload: payload.into_boxed_slice(),
        })?;
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        let (request_id, payload) = match response {
            LocalControlResponse::SemanticRangeChunk {
                request_id,
                payload,
            } => (request_id, payload),
            LocalControlResponse::SemanticStaleSelection { request_id } => {
                check_response_id(get.request_id, request_id)?;
                return Err(ClientError::StaleSelection);
            }
            LocalControlResponse::Rejected { message, .. } => {
                return Err(ClientError::Protocol(message));
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "locald returned the wrong semantic-range response: {other:?}"
                )));
            }
        };
        if request_id != get.request_id {
            return Err(ClientError::RequestMismatch {
                expected: get.request_id,
                observed: request_id,
            });
        }
        let chunk = SemanticRangeChunk::decode(&payload)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        chunk
            .validate_against(get)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(chunk)
    }

    fn request_catalog_page(
        &mut self,
        get: &SemanticCatalogGet,
    ) -> Result<SemanticCatalogChunk, ClientError> {
        self.prepare_request()?;
        let payload = get.encode().map_err(map_wire_error)?;
        let response = self.request_control(&LocalControlRequest::SemanticMetadataGet {
            request_id: get.request_id,
            payload: payload.into_boxed_slice(),
        })?;
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        let (request_id, payload) = match response {
            LocalControlResponse::SemanticMetadataChunk {
                request_id,
                payload,
            } => (request_id, payload),
            LocalControlResponse::SemanticStaleSelection { request_id } => {
                check_response_id(get.request_id, request_id)?;
                return Err(ClientError::StaleSelection);
            }
            LocalControlResponse::Rejected { message, .. } => {
                return Err(ClientError::Protocol(message));
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "locald returned the wrong semantic catalog response: {other:?}"
                )));
            }
        };
        check_response_id(get.request_id, request_id)?;
        let chunk = SemanticCatalogChunk::decode(&payload).map_err(map_wire_error)?;
        chunk.validate_against(get).map_err(map_wire_error)?;
        Ok(chunk)
    }

    fn request_manifest_page(
        &mut self,
        get: &SemanticManifestGet,
    ) -> Result<SemanticManifestChunk, ClientError> {
        self.prepare_request()?;
        let payload = get.encode().map_err(map_wire_error)?;
        let response = self.request_control(&LocalControlRequest::SemanticMetadataGet {
            request_id: get.request_id,
            payload: payload.into_boxed_slice(),
        })?;
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        let (request_id, payload) = match response {
            LocalControlResponse::SemanticMetadataChunk {
                request_id,
                payload,
            } => (request_id, payload),
            LocalControlResponse::SemanticStaleSelection { request_id } => {
                check_response_id(get.request_id, request_id)?;
                return Err(ClientError::StaleSelection);
            }
            LocalControlResponse::Rejected { message, .. } => {
                return Err(ClientError::Protocol(message));
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "locald returned the wrong semantic manifest response: {other:?}"
                )));
            }
        };
        check_response_id(get.request_id, request_id)?;
        let chunk = SemanticManifestChunk::decode(&payload).map_err(map_wire_error)?;
        chunk.validate_against(get).map_err(map_wire_error)?;
        Ok(chunk)
    }

    fn request_selected_image_page(
        &mut self,
        get: &SelectedSemanticImageGet,
    ) -> Result<SelectedSemanticImageChunk, ClientError> {
        self.prepare_request()?;
        let payload = get.encode().map_err(map_wire_error)?;
        let response = self.request_control(&LocalControlRequest::SemanticMetadataGet {
            request_id: get.request_id,
            payload: payload.into_boxed_slice(),
        })?;
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        let (request_id, payload) = match response {
            LocalControlResponse::SemanticMetadataChunk {
                request_id,
                payload,
            } => (request_id, payload),
            LocalControlResponse::SemanticStaleSelection { request_id } => {
                check_response_id(get.request_id, request_id)?;
                return Err(ClientError::StaleSelection);
            }
            LocalControlResponse::Rejected { message, .. } => {
                return Err(ClientError::Protocol(message));
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "locald returned the wrong selected-image response: {other:?}"
                )));
            }
        };
        check_response_id(get.request_id, request_id)?;
        let chunk = SelectedSemanticImageChunk::decode(&payload).map_err(map_wire_error)?;
        chunk.validate_against(get).map_err(map_wire_error)?;
        Ok(chunk)
    }

    fn ensure_stamp_current<A: SelectedGenerationSource>(
        &self,
        source: &mut A,
        expected: SelectedGenerationStamp,
    ) -> Result<(), ClientError> {
        let current = source
            .current_selected_generation()
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        if current != expected {
            return Err(ClientError::StaleSelection);
        }
        Ok(())
    }

    fn ensure_image_current<A: SelectedGenerationSource>(
        &self,
        source: &mut A,
        expected: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<(), ClientError> {
        self.ensure_stamp_current(source, expected)?;
        let current = source
            .selected_image_is_current(expected, image)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        if !current {
            return Err(ClientError::StaleSelection);
        }
        Ok(())
    }

    fn take_request_id(&mut self) -> Result<u64, ClientError> {
        let request_id = self.next_request_id;
        if request_id == 0 {
            return Err(ClientError::Protocol(
                "semantic-range request identity overflowed".to_owned(),
            ));
        }
        self.next_request_id = request_id.checked_add(1).unwrap_or(0);
        Ok(request_id)
    }

    fn request_control(
        &mut self,
        request: &LocalControlRequest,
    ) -> Result<LocalControlResponse, ClientError> {
        if let Some(remote) = self.remote.as_mut() {
            return remote.request(request);
        }
        self.client
            .as_mut()
            .ok_or_else(|| ClientError::Io("semantic control channel is unavailable".to_owned()))?
            .request(request)
            .map_err(map_control_error)
    }

    fn prepare_request(&mut self) -> Result<(), ClientError> {
        if self.frames_on_connection < CONNECTION_FRAME_BUDGET {
            return Ok(());
        }
        if let Some(remote) = self.remote.as_mut() {
            remote.reconnect()?;
            self.frames_on_connection = 0;
            return Ok(());
        }
        let endpoint = self.endpoint.as_deref().ok_or_else(|| {
            ClientError::Io("semantic-range connection reached its bounded frame budget".to_owned())
        })?;
        let stream = connect_stream(endpoint)?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .map_err(crate::map_peer_authentication_error)?;
        self.client = Some(LocalControlClient::new(stream, control_limits()));
        self.peer = Some(peer);
        self.frames_on_connection = 0;
        Ok(())
    }
}

/// Canonical production client for one package target's selected semantic catalog,
/// manifests, and IR or embedding segment ranges.
///
/// It owns one transfer connection and one independent live-authority probe
/// connection. The latter reopens the selected catalog on every freshness
/// check; the admitted catalog root is used only to verify membership after
/// that current stamp matches exactly.
#[cfg(any(unix, windows))]
pub struct LocalSemanticIndexClient {
    target: SemanticTargetKey,
    transfer: LocalSemanticRangeTransport,
    authority: LocalSemanticRangeTransport,
    selected_catalog: Option<SemanticCatalogSnapshot>,
    selected_images: SelectedImageResidence,
    segment_residency: AdaptiveIrResidency,
}

#[cfg(any(unix, windows))]
impl LocalSemanticIndexClient {
    /// Opens authenticated locald connections for one exact package target.
    pub fn connect(path: impl AsRef<Path>, target: SemanticTargetKey) -> Result<Self, ClientError> {
        let transfer = LocalSemanticRangeTransport::connect(path.as_ref())?;
        let authority = LocalSemanticRangeTransport::connect(path)?;
        Ok(Self {
            target,
            transfer,
            authority,
            selected_catalog: None,
            selected_images: SelectedImageResidence::new(),
            segment_residency: AdaptiveIrResidency::default(),
        })
    }

    /// Opens the same hydration API over two signed direct remote channels.
    pub fn connect_remote(
        client_secret: SecretKey,
        owner: backend_engine::cluster_transport::EndpointId,
        address: SocketAddr,
        capability: RemoteIndexCapability,
        target: SemanticTargetKey,
    ) -> Result<Self, ClientError> {
        capability
            .verify(
                owner,
                client_secret.public(),
                remote_index_now().map_err(map_transport_error)?,
            )
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let scope = capability.claims.semantic.as_ref().ok_or_else(|| {
            ClientError::Protocol(
                "remote semantic client needs a semantic-hydration capability".to_owned(),
            )
        })?;
        if scope.package != target.package()
            || scope.coordinate != target.coordinate()
            || scope.profile != <[u8; 2]>::from(target.profile())
        {
            return Err(ClientError::StaleRemoteCapability);
        }
        let secret = client_secret.to_bytes();
        let transfer = LocalSemanticRangeTransport::connect_remote(
            SecretKey::from_bytes(&secret),
            owner,
            address,
            capability.clone(),
        )?;
        let authority = LocalSemanticRangeTransport::connect_remote(
            SecretKey::from_bytes(&secret),
            owner,
            address,
            capability,
        )?;
        Ok(Self {
            target,
            transfer,
            authority,
            selected_catalog: None,
            selected_images: SelectedImageResidence::new(),
            segment_residency: AdaptiveIrResidency::default(),
        })
    }

    /// Returns the target this client is permanently bound to.
    #[must_use]
    pub const fn target(&self) -> &SemanticTargetKey {
        &self.target
    }

    /// Returns the last exact selected stamp admitted by this client.
    #[must_use]
    pub fn selected_stamp(&self) -> Option<SelectedGenerationStamp> {
        self.selected_catalog
            .as_ref()
            .map(SemanticCatalogSnapshot::selected_stamp)
    }

    /// Fetches and admits the entire currently selected catalog. The previous
    /// snapshot is replaced only after every page, root, and freshness check passes.
    pub fn fetch_selected_catalog(&mut self) -> Result<SemanticCatalogSnapshot, ClientError> {
        let snapshot = {
            let mut source = LocalSemanticAuthoritySource {
                authority: &mut self.authority,
                target: &self.target,
                catalog: None,
            };
            self.transfer
                .fetch_semantic_catalog(self.target.clone(), &mut source)?
        };
        self.selected_catalog = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Fetches one exact image manifest listed by the last admitted selected catalog.
    pub fn fetch_selected_manifest(
        &mut self,
        image: SemanticPlaneImageKey,
    ) -> Result<SemanticPlaneManifest, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        self.transfer
            .fetch_semantic_manifest(snapshot, image, &mut source)
    }

    /// Fetches or reuses the complete selected NXFI image and returns its
    /// validated zero-copy mapping. The supplied budget applies only to pages
    /// newly read from the local owner; a verified cache hit costs no pages.
    pub fn fetch_selected_image(
        &mut self,
        image: SemanticPlaneImageKey,
        store: &FileSemanticRangeStore,
        max_bytes: u64,
        max_pages: usize,
    ) -> Result<SemanticImageFetch, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let key = SelectedImageResidenceKey::new(snapshot.selected_stamp, image);
        {
            let mut source = LocalSemanticAuthoritySource {
                authority: &mut self.authority,
                target: &self.target,
                catalog: Some(snapshot),
            };
            self.transfer
                .ensure_image_current(&mut source, snapshot.selected_stamp, image)?;
            if let Some(mapped) = self.selected_images.get(key) {
                return Ok(SemanticImageFetch::shared(mapped, 0, 0));
            }
        }

        let fetched = {
            let mut source = LocalSemanticAuthoritySource {
                authority: &mut self.authority,
                target: &self.target,
                catalog: Some(snapshot),
            };
            self.transfer.fetch_semantic_image(
                snapshot,
                image,
                &mut source,
                store,
                max_bytes,
                max_pages,
            )?
        };
        let SemanticImageFetch {
            image,
            transferred_bytes,
            page_requests,
        } = fetched;
        match image {
            SemanticImageOwner::Owned(mapped) => match self.selected_images.insert(key, mapped) {
                Ok(mapped) => Ok(SemanticImageFetch::shared(
                    mapped,
                    transferred_bytes,
                    page_requests,
                )),
                Err(mapped) => Ok(SemanticImageFetch::owned(
                    mapped,
                    transferred_bytes,
                    page_requests,
                )),
            },
            SemanticImageOwner::Shared(mapped) => Ok(SemanticImageFetch::shared(
                mapped,
                transferred_bytes,
                page_requests,
            )),
        }
    }

    /// Borrows one admitted local segment under the current selected frontier.
    ///
    /// Repeated reads stay in the single-owner hot cache. A bounded, admitted
    /// IR-VCS delta route may instead reuse the exact segment from an earlier
    /// CAS binding; all other reads use this selection's pristine CAS entry.
    /// The callback cannot retain the borrowed bytes beyond this call.
    pub fn with_selected_segment<C, R>(
        &mut self,
        manifest: &SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        segment: &SemanticPlaneSegment,
        delta_chain: &[IrResidencyDeltaHop<'_>],
        store: &mut C,
        read: impl FnOnce(&[u8]) -> R,
    ) -> Result<(IrResidencyPath, R), ClientError>
    where
        C: DurableSemanticRangeStore,
    {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        let selection = SelectedSemanticPlane::select(&mut source, manifest, image, kind)
            .map_err(map_hydration_error)?;
        self.segment_residency
            .with_segment(
                &mut source,
                selection,
                manifest,
                segment,
                delta_chain,
                store,
                read,
            )
            .map_err(map_residency_error)
    }

    /// Checks one IR-VCS manifest chain once for a sequence of selected
    /// segment reads. The plan borrows the manifests and stores only bounded
    /// reusable descriptor indices; each later read still rechecks the live
    /// selection and the exact requested descriptor.
    pub fn prepare_selected_delta_route<'manifest>(
        &mut self,
        manifest: &'manifest SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        chain: &[IrResidencyDeltaHop<'manifest>],
    ) -> Result<PreparedIrResidencyDeltaRoute<'manifest>, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        let selection = SelectedSemanticPlane::select(&mut source, manifest, image, kind)
            .map_err(map_hydration_error)?;
        Ok(self
            .segment_residency
            .prepare_delta_route(selection, manifest, chain))
    }

    /// Borrows one exact selected segment using a previously checked route.
    /// No route actions are rescanned, and the borrowed payload cannot escape
    /// `read`.
    pub fn with_selected_segment_prepared<C, R>(
        &mut self,
        manifest: &SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        segment: &SemanticPlaneSegment,
        route: &PreparedIrResidencyDeltaRoute<'_>,
        store: &mut C,
        read: impl FnOnce(&[u8]) -> R,
    ) -> Result<(IrResidencyPath, R), ClientError>
    where
        C: DurableSemanticRangeStore,
    {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        let selection = SelectedSemanticPlane::select(&mut source, manifest, image, kind)
            .map_err(map_hydration_error)?;
        self.segment_residency
            .with_segment_prepared(
                &mut source,
                selection,
                manifest,
                segment,
                route,
                store,
                read,
            )
            .map_err(map_residency_error)
    }

    /// Current hot-owner and route-selection counters for this client.
    #[must_use]
    pub fn segment_residency_metrics(&self) -> IrResidencyMetrics {
        self.segment_residency.metrics()
    }

    /// Starts a cursor for one exact selected IR or embedding plane.
    pub fn new_cursor<'manifest, 'have>(
        &mut self,
        manifest: &'manifest SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        have_ids: &'have [SemanticSegmentId],
        limits: TransportLimits,
    ) -> Result<IrHydrationCursor<'manifest, 'have>, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        IrHydrationCursor::new_for_image(manifest, &mut source, image, kind, have_ids, limits)
            .map_err(map_hydration_error)
    }

    /// Reads locally complete segments and returns only IDs whose bytes still
    /// admit against the exact selected manifest plane.
    pub fn verified_local_segments(
        &mut self,
        manifest: &SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        store: &mut FileSemanticRangeStore,
    ) -> Result<Vec<SemanticSegmentId>, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let previous_generation = store
            .current_local_generation(&self.target)
            .map_err(ClientError::Io)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        let selection = SelectedSemanticPlane::select(&mut source, manifest, image, kind)
            .map_err(map_hydration_error)?;
        let chain = previous_generation
            .as_ref()
            .filter(|generation| generation.image() != image)
            .and_then(|generation| {
                generation.historical_plane_binding(kind).map(|binding| {
                    IrResidencyDeltaHop::from_historical(generation.manifest(), manifest, binding)
                })
            })
            .into_iter()
            .collect::<Vec<_>>();
        let route = self
            .segment_residency
            .prepare_delta_route(selection, manifest, &chain);
        let plane = manifest.plane(kind).ok_or_else(|| {
            ClientError::Protocol("requested semantic plane is absent from the manifest".to_owned())
        })?;
        let mut have = Vec::new();
        have.try_reserve(plane.segments().len()).map_err(|_| {
            ClientError::Protocol("local semantic segment index allocation failed".to_owned())
        })?;
        self.segment_residency
            .verify_segments_prepared(
                &mut source,
                selection,
                manifest,
                plane.segments(),
                &route,
                store,
                &mut have,
            )
            .map_err(map_residency_error)?;
        Ok(have)
    }

    /// Persists the selected image metadata and moves the local generation
    /// head after every segment in `kind` is present and re-admitted from CAS.
    pub fn commit_local_generation(
        &mut self,
        image: SemanticPlaneImageKey,
        manifest: &SemanticPlaneManifest,
        kind: SemanticPlaneKind,
        store: &mut FileSemanticRangeStore,
    ) -> Result<LocalSemanticGeneration, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        let selection = SelectedSemanticPlane::select(&mut source, manifest, image, kind)
            .map_err(map_hydration_error)?;
        store
            .commit_local_generation(
                &self.target,
                snapshot.catalog(),
                image,
                manifest,
                selection,
                &mut source,
            )
            .map_err(ClientError::Io)
    }

    /// Polls the next bounded range using a fresh live selected-stamp read.
    pub fn next_request(
        &mut self,
        cursor: &mut IrHydrationCursor<'_, '_>,
        partial: Option<&backend_replication::SparseSegmentCoverage>,
        credits: HydrationCredits,
    ) -> Result<IrHydrationPoll, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        cursor
            .next_request(&mut source, partial, credits)
            .map_err(map_hydration_error)
    }

    /// Returns the exact outstanding transfer byte count after rechecking selection.
    pub fn requested_bytes(&mut self, request: &IrHydrationRequest) -> Result<u64, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        request
            .byte_range(&mut source)
            .map_err(map_hydration_error)?
            .map(|range| range.len)
            .ok_or_else(|| {
                ClientError::Protocol("verification request has no transfer range".to_owned())
            })
    }

    /// Fetches the exact cursor range and stages or admits it in caller-owned durable CAS.
    pub fn request_and_accept<C: DurableSemanticRangeStore>(
        &mut self,
        cursor: &mut IrHydrationCursor<'_, '_>,
        request: &IrHydrationRequest,
        store: &mut C,
        limits: TransportLimits,
    ) -> Result<SemanticRangeClientProgress, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        self.transfer.request_and_accept(
            self.target.clone(),
            cursor,
            &mut source,
            request,
            store,
            limits,
        )
    }

    /// Admits a segment already complete in local sparse CAS, then commits and
    /// reads it back before the cursor counts it as present.
    pub fn verify_local_segment<C: DurableSemanticRangeStore>(
        &mut self,
        cursor: &mut IrHydrationCursor<'_, '_>,
        request: &IrHydrationRequest,
        store: &mut C,
    ) -> Result<VerifiedSemanticSegment, ClientError> {
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        let claim = request.segment(&mut source).map_err(map_hydration_error)?;
        let payload = store
            .read_complete_segment(request.selection(), claim)
            .map_err(|error| ClientError::Io(error.to_string()))?
            .ok_or_else(|| {
                ClientError::Protocol("cursor requested a segment absent from local CAS".to_owned())
            })?;
        let pending = cursor
            .verify_payload(&mut source, request, &payload)
            .map_err(map_hydration_error)?;
        cursor
            .commit_admitted_segment(&mut source, request, pending, store)
            .map_err(map_hydration_error)
    }

    /// Restores one persisted sparse range checkpoint after checking its exact
    /// selected stamp against a fresh locald authority read.
    pub fn resume_semantic_range<'manifest, 'have, C: DurableSemanticRangeStore>(
        &mut self,
        checkpoint: &SemanticRangeClientCheckpoint,
        manifest: &'manifest SemanticPlaneManifest,
        have_ids: &'have [SemanticSegmentId],
        limits: TransportLimits,
        store: &mut C,
    ) -> Result<
        (
            IrHydrationCursor<'manifest, 'have>,
            backend_replication::SparseSegmentCoverage,
            IrHydrationPoll,
        ),
        ClientError,
    > {
        if checkpoint.target() != &self.target {
            return Err(ClientError::StaleSelection);
        }
        let snapshot = Self::require_catalog(&self.selected_catalog)?;
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(snapshot),
        };
        checkpoint
            .resume(manifest, &mut source, have_ids, limits, store)
            .map_err(map_hydration_error)
    }

    fn require_catalog(
        catalog: &Option<SemanticCatalogSnapshot>,
    ) -> Result<&SemanticCatalogSnapshot, ClientError> {
        catalog.as_ref().ok_or_else(|| {
            ClientError::Protocol(
                "fetch the selected semantic catalog before using an image".to_owned(),
            )
        })
    }
}

#[cfg(any(unix, windows))]
struct LocalSemanticAuthoritySource<'a> {
    authority: &'a mut LocalSemanticRangeTransport,
    target: &'a SemanticTargetKey,
    catalog: Option<&'a SemanticCatalogSnapshot>,
}

#[cfg(any(unix, windows))]
impl SelectedGenerationSource for LocalSemanticAuthoritySource<'_> {
    type Error = ClientError;

    fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
        self.authority.read_selected_stamp(self.target)
    }

    fn selected_image_is_current(
        &mut self,
        expected_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<bool, Self::Error> {
        let current = self.current_selected_generation()?;
        if current != expected_stamp {
            return Ok(false);
        }
        let Some(catalog) = self.catalog else {
            return Ok(false);
        };
        Ok(catalog.selected_stamp == expected_stamp
            && catalog.catalog.root() == expected_stamp.catalog_root()
            && catalog
                .catalog
                .entries()
                .iter()
                .any(|entry| entry.image() == image))
    }
}

#[cfg(any(unix, windows))]
fn append_metadata_page(
    output: &mut Vec<u8>,
    expected_offset: u64,
    total_length: u64,
    page: &[u8],
) -> Result<(), ClientError> {
    let observed_offset = u64::try_from(output.len())
        .map_err(|_| ClientError::Protocol("semantic metadata offset overflow".to_owned()))?;
    let page_length = u64::try_from(page.len())
        .map_err(|_| ClientError::Protocol("semantic metadata page is too large".to_owned()))?;
    let end = expected_offset
        .checked_add(page_length)
        .ok_or_else(|| ClientError::Protocol("semantic metadata page range overflow".to_owned()))?;
    if observed_offset != expected_offset || page.is_empty() || end > total_length {
        return Err(ClientError::Protocol(
            "semantic metadata pages are noncontiguous or exceed their declared length".to_owned(),
        ));
    }
    output.extend_from_slice(page);
    Ok(())
}

#[cfg(any(unix, windows))]
fn ensure_image_budget(
    get: &SelectedSemanticImageGet,
    transferred_bytes: &mut u64,
    page_requests: &mut usize,
    max_bytes: u64,
    max_pages: usize,
) -> Result<(), ClientError> {
    let next_bytes = transferred_bytes
        .checked_add(get.byte_range.len)
        .ok_or_else(|| {
            ClientError::Protocol("semantic-image byte accounting overflow".to_owned())
        })?;
    let next_pages = page_requests.checked_add(1).ok_or_else(|| {
        ClientError::Protocol("semantic-image page accounting overflow".to_owned())
    })?;
    if next_bytes > max_bytes || next_pages > max_pages {
        return Err(ClientError::Protocol(
            "complete semantic image exceeds the configured transfer budget".to_owned(),
        ));
    }
    *transferred_bytes = next_bytes;
    *page_requests = next_pages;
    Ok(())
}

#[cfg(any(unix, windows))]
fn check_response_id(expected: u64, observed: u64) -> Result<(), ClientError> {
    if expected == observed {
        Ok(())
    } else {
        Err(ClientError::RequestMismatch { expected, observed })
    }
}

#[cfg(any(unix, windows))]
fn map_wire_error(error: backend_replication::ReplicationError) -> ClientError {
    ClientError::Transport(error)
}

fn map_image_cache_error(error: SemanticImageCacheError) -> ClientError {
    match error {
        SemanticImageCacheError::Storage(detail) => ClientError::Io(detail),
        SemanticImageCacheError::CorruptContent { detail, .. }
        | SemanticImageCacheError::SelectionMismatch { detail, .. } => {
            ClientError::Protocol(detail)
        }
        SemanticImageCacheError::TransferRejected(detail) => ClientError::Protocol(detail),
    }
}

#[cfg(any(unix, windows))]
fn map_hydration_error(error: IrHydrationError) -> ClientError {
    match error {
        IrHydrationError::Terminal(IrHydrationTerminal::Stale) => ClientError::StaleSelection,
        error => ClientError::Protocol(error.to_string()),
    }
}

#[cfg(any(unix, windows))]
fn map_residency_error(error: IrResidencyError) -> ClientError {
    match error {
        IrResidencyError::StaleSelection => ClientError::StaleSelection,
        IrResidencyError::Frontier(detail) | IrResidencyError::Storage(detail) => {
            ClientError::Io(detail)
        }
        other => ClientError::Protocol(other.to_string()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use backend_replication::{
        LocalControlLimits, LocalControlRequest, LocalControlResponse, SemanticCatalogChunk,
        SemanticCatalogGet, decode_request, encode_response, read_frame, write_frame,
    };
    use backend_semantic::ir::{
        GenerationId, SemanticImageIdentity, SemanticManifestRoot, SemanticPlaneCatalogEntry,
        SemanticPlaneCatalogRoot, load_semantic_image_mmap,
    };
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
    use std::fs;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    struct Source {
        stamp: SelectedGenerationStamp,
        changed_stamp: SelectedGenerationStamp,
        reads: usize,
        stale_after: Option<usize>,
    }

    impl SelectedGenerationSource for Source {
        type Error = &'static str;

        fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
            self.reads += 1;
            if self.stale_after.is_some_and(|limit| self.reads >= limit) {
                Ok(self.changed_stamp)
            } else {
                Ok(self.stamp)
            }
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: SelectedGenerationStamp,
            _image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            Ok(expected_stamp == self.stamp)
        }
    }

    struct MembershipSource {
        stamp: SelectedGenerationStamp,
        selected: bool,
    }

    impl SelectedGenerationSource for MembershipSource {
        type Error = &'static str;

        fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
            Ok(self.stamp)
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: SelectedGenerationStamp,
            _image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            Ok(self.selected && expected_stamp == self.stamp)
        }
    }

    fn catalog_fixture() -> (
        SemanticTargetKey,
        SemanticPlaneCatalog,
        SelectedGenerationStamp,
    ) {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let target = SemanticTargetKey::new(
            "pkg:cargo/local-bootstrap@1.0.0",
            "pkg:cargo/local-bootstrap@1.0.0",
            profile,
        )
        .expect("target");
        let entries = (0..256_u32)
            .map(|ordinal| {
                let identity = u8::try_from(ordinal % 250 + 1).expect("nonzero fixture byte");
                let image = SemanticPlaneImageKey::new(
                    ordinal,
                    GenerationId::from_raw([identity; 32]),
                    SemanticManifestRoot::from_wire_claim([identity.wrapping_add(1); 32]),
                );
                SemanticPlaneCatalogEntry::new(image, 1).expect("catalog entry")
            })
            .collect();
        let catalog = SemanticPlaneCatalog::new(entries).expect("catalog");
        let stamp = SelectedGenerationStamp::checked(
            [1; 16],
            profile,
            [2; 32],
            1,
            [3; 32],
            [4; 32],
            catalog.root(),
        )
        .expect("selected stamp");
        (target, catalog, stamp)
    }

    fn changed_stamp(stamp: SelectedGenerationStamp) -> SelectedGenerationStamp {
        SelectedGenerationStamp::checked(
            *stamp.namespace(),
            stamp.profile(),
            *stamp.source_coordinate(),
            stamp.selection_revision().saturating_add(1),
            *stamp.selected_root(),
            *stamp.closure_id(),
            SemanticPlaneCatalogRoot::from_wire_claim([9; 32]),
        )
        .expect("changed selected stamp")
    }

    fn mapped_image_fixture() -> MappedSemanticImage {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
        let ir = backend_semantic::ir::IrBuilder::new()
            .finish()
            .expect("empty canonical IR builds");
        let length =
            backend_semantic::ir::full_semantic_image_len(&ir).expect("canonical image length");
        let mut bytes = vec![0; length];
        backend_semantic::ir::encode_full_semantic_image(&ir, &mut bytes)
            .expect("canonical image encodes");
        let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
        let generation = GenerationId::from_canonical_bytes(&bytes);
        let path = std::env::temp_dir().join(format!(
            "backend-client-image-residence-{}-{}.nxf",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::write(&path, bytes).expect("write canonical image fixture");
        let mapped = load_semantic_image_mmap(&path, identity, generation, length)
            .expect("map and admit canonical image");
        fs::remove_file(path).expect("remove fixture after anonymous mapping");
        mapped
    }

    fn residence_key(stamp: SelectedGenerationStamp, ordinal: u32) -> SelectedImageResidenceKey {
        let identity = u8::try_from(ordinal).expect("small test image ordinal");
        SelectedImageResidenceKey::new(
            stamp,
            SemanticPlaneImageKey::new(
                ordinal,
                GenerationId::from_raw([identity.wrapping_add(1); 32]),
                SemanticManifestRoot::from_wire_claim([identity.wrapping_add(2); 32]),
            ),
        )
    }

    fn serve_catalog_pages(
        mut stream: UnixStream,
        catalog: SemanticPlaneCatalog,
        stamp: SelectedGenerationStamp,
        stop_after_first: bool,
    ) {
        let limits = LocalControlLimits::default();
        let bytes = catalog.encode().expect("catalog bytes");
        loop {
            let request_payload = read_frame(&mut stream, limits).expect("client page frame");
            let request = decode_request(&request_payload, limits).expect("decode page request");
            let LocalControlRequest::SemanticMetadataGet {
                request_id,
                payload,
            } = request
            else {
                panic!("expected metadata request")
            };
            let get = SemanticCatalogGet::decode(&payload).expect("decode catalog page request");
            assert_eq!(request_id, get.request_id);
            let start = usize::try_from(get.byte_range.start).expect("start");
            let end = usize::try_from(get.byte_range.end().expect("end")).expect("end usize");
            let chunk = SemanticCatalogChunk {
                request_id,
                target: get.target.clone(),
                selected_stamp: stamp,
                catalog_root: catalog.root(),
                total_length: bytes.len() as u64,
                byte_range: get.byte_range,
                payload: bytes[start..end].to_vec(),
            };
            chunk.validate_against(&get).expect("valid catalog chunk");
            let payload = chunk.encode().expect("encode catalog chunk");
            let response = LocalControlResponse::SemanticMetadataChunk {
                request_id,
                payload: payload.into_boxed_slice(),
            };
            let response_payload = encode_response(&response, limits).expect("encode response");
            write_frame(&mut stream, &response_payload, limits).expect("send catalog chunk");
            if end == bytes.len() || stop_after_first {
                return;
            }
        }
    }

    #[test]
    fn cold_client_assembles_and_admits_a_mult_page_catalog() {
        let (target, catalog, stamp) = catalog_fixture();
        let (client_stream, server_stream) = UnixStream::pair().expect("stream pair");
        let server_catalog = catalog.clone();
        let server =
            thread::spawn(move || serve_catalog_pages(server_stream, server_catalog, stamp, false));
        let mut client = LocalSemanticRangeTransport::from_unix_stream(client_stream);
        let mut source = Source {
            stamp,
            changed_stamp: changed_stamp(stamp),
            reads: 0,
            stale_after: None,
        };
        let snapshot = client
            .fetch_semantic_catalog(target.clone(), &mut source)
            .expect("fetch selected catalog");
        assert_eq!(snapshot.target(), &target);
        assert_eq!(snapshot.selected_stamp(), stamp);
        assert_eq!(snapshot.catalog(), &catalog);
        assert!(catalog.encode().expect("catalog wire").len() > MAX_METADATA_PAGE_BYTES as usize);
        server.join().expect("server completion");
    }

    #[test]
    fn client_rejects_catalog_page_when_head_moves_after_first_page() {
        let (target, catalog, stamp) = catalog_fixture();
        let (client_stream, server_stream) = UnixStream::pair().expect("stream pair");
        let server_catalog = catalog.clone();
        let server =
            thread::spawn(move || serve_catalog_pages(server_stream, server_catalog, stamp, true));
        let mut client = LocalSemanticRangeTransport::from_unix_stream(client_stream);
        let mut source = Source {
            stamp,
            changed_stamp: changed_stamp(stamp),
            reads: 0,
            stale_after: Some(1),
        };
        let error = client
            .fetch_semantic_catalog(target, &mut source)
            .expect_err("stale catalog must be rejected");
        assert_eq!(error, ClientError::StaleSelection);
        server.join().expect("server completion");
    }

    #[test]
    fn selected_image_residence_reuses_the_exact_stamp_and_image_arc() {
        let (_, _, stamp) = catalog_fixture();
        let key = residence_key(stamp, 1);
        let mapped = mapped_image_fixture();
        let mut residence = SelectedImageResidence::with_limits(
            u64::try_from(mapped.view().as_ref().len()).expect("image length"),
            2,
        );
        let first = match residence.insert(key, mapped) {
            Ok(image) => image,
            Err(_) => panic!("bounded image should be retained"),
        };
        let second = residence.get(key).expect("exact key is hot");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(residence.entry_count(), 1);
        assert_eq!(
            residence.charged_bytes(),
            u64::try_from(first.view().as_ref().len()).expect("image length")
        );

        let changed_key = residence_key(changed_stamp(stamp), 1);
        assert!(residence.get(changed_key).is_none());
    }

    #[test]
    fn selected_image_residence_keeps_evicted_leases_in_its_budget() {
        let (_, _, stamp) = catalog_fixture();
        let first_image = mapped_image_fixture();
        let image_bytes = u64::try_from(first_image.view().as_ref().len()).expect("image length");
        let mut residence = SelectedImageResidence::with_limits(image_bytes * 2, 2);
        let first_key = residence_key(stamp, 1);
        let second_key = residence_key(stamp, 2);
        let third_key = residence_key(stamp, 3);
        let first = match residence.insert(first_key, first_image) {
            Ok(image) => image,
            Err(_) => panic!("first bounded image should be retained"),
        };
        let second = match residence.insert(second_key, mapped_image_fixture()) {
            Ok(image) => image,
            Err(_) => panic!("second bounded image should be retained"),
        };

        assert!(residence.insert(third_key, mapped_image_fixture()).is_err());
        assert_eq!(residence.entry_count(), 2);
        assert_eq!(residence.charged_bytes(), image_bytes * 2);
        assert!(
            residence
                .get(first_key)
                .is_some_and(|hot| Arc::ptr_eq(&hot, &first))
        );

        drop(first);
        drop(second);
        assert!(residence.get(third_key).is_none());
        assert_eq!(residence.charged_bytes(), image_bytes);
        assert!(residence.insert(third_key, mapped_image_fixture()).is_ok());
    }

    #[test]
    fn selected_image_residence_bypasses_images_over_its_byte_limit() {
        let (_, _, stamp) = catalog_fixture();
        let mapped = mapped_image_fixture();
        let mut residence = SelectedImageResidence::with_limits(1, 2);

        assert!(residence.insert(residence_key(stamp, 1), mapped).is_err());
        assert_eq!(residence.entry_count(), 0);
        assert_eq!(residence.charged_bytes(), 0);
    }

    #[test]
    fn image_current_check_rejects_a_stale_cached_selection() {
        let (_, catalog, stamp) = catalog_fixture();
        let image = catalog.entries()[0].image();
        let (client_stream, _server_stream) = UnixStream::pair().expect("stream pair");
        let transport = LocalSemanticRangeTransport::from_unix_stream(client_stream);
        let mut source = Source {
            stamp,
            changed_stamp: changed_stamp(stamp),
            reads: 0,
            stale_after: Some(1),
        };

        assert_eq!(
            transport.ensure_image_current(&mut source, stamp, image),
            Err(ClientError::StaleSelection)
        );
        assert_eq!(source.reads, 1);
    }

    #[test]
    fn image_current_check_rejects_changed_image_membership() {
        let (_, catalog, stamp) = catalog_fixture();
        let image = catalog.entries()[0].image();
        let (client_stream, _server_stream) = UnixStream::pair().expect("stream pair");
        let transport = LocalSemanticRangeTransport::from_unix_stream(client_stream);
        let mut source = MembershipSource {
            stamp,
            selected: false,
        };

        assert_eq!(
            transport.ensure_image_current(&mut source, stamp, image),
            Err(ClientError::StaleSelection)
        );
    }
}

#[cfg(any(unix, windows))]
fn connect_stream(path: &Path) -> Result<backend_replication::LocalStream, ClientError> {
    let stream = backend_replication::LocalStream::connect(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound
            || error.kind() == std::io::ErrorKind::ConnectionRefused
        {
            ClientError::Disconnected(error.kind())
        } else {
            ClientError::Io(error.to_string())
        }
    })?;
    configure_stream(&stream);
    Ok(stream)
}

#[cfg(any(unix, windows))]
fn configure_stream(stream: &backend_replication::LocalStream) {
    let _ = stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(REQUEST_TIMEOUT)));
}

#[cfg(unix)]
impl LocalSemanticRangeTransport {
    /// Uses a connected Unix stream for local RPC tests.
    #[must_use]
    pub fn from_unix_stream(stream: std::os::unix::net::UnixStream) -> Self {
        Self::from_stream(stream)
    }
}
