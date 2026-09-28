//! Locald transport and durable admission for selected semantic byte ranges.

use crate::{ClientError, MAX_FRAME};
use backend_replication::{
    ByteRange, DurableSemanticRangeStore, HydrationCredits, IrHydrationCursor, IrHydrationError,
    IrHydrationPoll, IrHydrationRequest, IrHydrationTerminal, LOCAL_CONTROL_MAX_CURSOR,
    LOCAL_CONTROL_MAX_ERROR, LocalControlClient, LocalControlError, LocalControlLimits,
    LocalControlRequest, LocalControlResponse, SelectedGenerationSource, SelectedGenerationStamp,
    SelectedSemanticPlane, SemanticCatalogChunk, SemanticCatalogGet, SemanticManifestChunk,
    SemanticManifestGet, SemanticRangeChunk, SemanticRangeClientCheckpoint,
    SemanticRangeClientProgress, SemanticRangeGet, SemanticTargetKey, TransportLimits,
    VerifiedSemanticSegment, accept_semantic_range, admit_semantic_catalog,
    admit_semantic_manifest,
};
use backend_semantic::ir::{
    SemanticPlaneCatalog, SemanticPlaneImageKey, SemanticPlaneKind, SemanticPlaneManifest,
    SemanticRangeRequest, SemanticSegmentId,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CONNECTION_FRAME_BUDGET: usize = 240;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_METADATA_PAGE_BYTES: u64 = 16 * 1024;
const MAX_METADATA_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

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
    client: LocalControlClient<backend_replication::LocalStream>,
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
    endpoint: Option<PathBuf>,
    frames_on_connection: usize,
    next_request_id: u64,
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
            client: LocalControlClient::new(stream, control_limits()),
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
            client: LocalControlClient::new(stream, control_limits()),
            peer: None,
            endpoint: None,
            frames_on_connection: 0,
            next_request_id: 1,
        }
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
        let response = self
            .client
            .request(&LocalControlRequest::SemanticRangeGet {
                request_id: get.request_id,
                payload: payload.into_boxed_slice(),
            })
            .map_err(map_control_error)?;
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
        let response = self
            .client
            .request(&LocalControlRequest::SemanticMetadataGet {
                request_id: get.request_id,
                payload: payload.into_boxed_slice(),
            })
            .map_err(map_control_error)?;
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
        let response = self
            .client
            .request(&LocalControlRequest::SemanticMetadataGet {
                request_id: get.request_id,
                payload: payload.into_boxed_slice(),
            })
            .map_err(map_control_error)?;
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

    fn prepare_request(&mut self) -> Result<(), ClientError> {
        if self.frames_on_connection < CONNECTION_FRAME_BUDGET {
            return Ok(());
        }
        let endpoint = self.endpoint.as_deref().ok_or_else(|| {
            ClientError::Io("semantic-range connection reached its bounded frame budget".to_owned())
        })?;
        let stream = connect_stream(endpoint)?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .map_err(crate::map_peer_authentication_error)?;
        self.client = LocalControlClient::new(stream, control_limits());
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
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
        };
        self.transfer
            .fetch_semantic_manifest(&snapshot, image, &mut source)
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
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
        };
        IrHydrationCursor::new_for_image(manifest, &mut source, image, kind, have_ids, limits)
            .map_err(map_hydration_error)
    }

    /// Reads locally complete segments and returns only IDs whose bytes still
    /// admit against the exact selected manifest plane.
    pub fn verified_local_segments<C: DurableSemanticRangeStore>(
        &mut self,
        manifest: &SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        store: &mut C,
    ) -> Result<Vec<SemanticSegmentId>, ClientError> {
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
        };
        let selection = SelectedSemanticPlane::select(&mut source, manifest, image, kind)
            .map_err(map_hydration_error)?;
        let plane = manifest.plane(kind).ok_or_else(|| {
            ClientError::Protocol("requested semantic plane is absent from the manifest".to_owned())
        })?;
        let mut have = Vec::new();
        have.try_reserve(plane.segments().len()).map_err(|_| {
            ClientError::Protocol("local semantic segment index allocation failed".to_owned())
        })?;
        for segment in plane.segments() {
            let request = SemanticRangeRequest {
                manifest_root: manifest.root(),
                plane: kind,
                segment_id: segment.id_claim(),
                first_key: *segment.first_key(),
                last_key: *segment.last_key(),
                byte_length: segment.byte_length(),
            };
            let payload = store
                .read_complete_segment(selection, request)
                .map_err(|error| ClientError::Io(error.to_string()))?;
            if let Some(payload) = payload {
                have.push(segment.admit(kind, &payload).map_err(|error| {
                    ClientError::Protocol(format!(
                        "local semantic segment failed admission: {error}"
                    ))
                })?);
            }
        }
        Ok(have)
    }

    /// Polls the next bounded range using a fresh live selected-stamp read.
    pub fn next_request(
        &mut self,
        cursor: &mut IrHydrationCursor<'_, '_>,
        partial: Option<&backend_replication::SparseSegmentCoverage>,
        credits: HydrationCredits,
    ) -> Result<IrHydrationPoll, ClientError> {
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
        };
        cursor
            .next_request(&mut source, partial, credits)
            .map_err(map_hydration_error)
    }

    /// Returns the exact outstanding transfer byte count after rechecking selection.
    pub fn requested_bytes(&mut self, request: &IrHydrationRequest) -> Result<u64, ClientError> {
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
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
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
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
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
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
        let snapshot = self.require_catalog()?.clone();
        let mut source = LocalSemanticAuthoritySource {
            authority: &mut self.authority,
            target: &self.target,
            catalog: Some(&snapshot),
        };
        checkpoint
            .resume(manifest, &mut source, have_ids, limits, store)
            .map_err(map_hydration_error)
    }

    fn require_catalog(&self) -> Result<&SemanticCatalogSnapshot, ClientError> {
        self.selected_catalog.as_ref().ok_or_else(|| {
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

#[cfg(any(unix, windows))]
fn map_hydration_error(error: IrHydrationError) -> ClientError {
    match error {
        IrHydrationError::Terminal(IrHydrationTerminal::Stale) => ClientError::StaleSelection,
        error => ClientError::Protocol(error.to_string()),
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
        GenerationId, SemanticManifestRoot, SemanticPlaneCatalogEntry, SemanticPlaneCatalogRoot,
    };
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
    use std::os::unix::net::UnixStream;
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
