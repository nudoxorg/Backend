//! Durable semantic-range storage over the existing sparse receiving CAS.
//!
//! Partial ranges are staged through `ReceivingCas` and its byte-free
//! `WireReceivingCheckpoint`; complete, admitted segments are committed as
//! ordinary immutable `FileStore` objects. A small atomic side index binds a
//! selected root/plane/segment tuple to the checked FileStore object identity.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use backend_semantic::ir::{
    FacetChange, GenerationId, MAX_SEMANTIC_SEGMENT_BYTES, MappedSemanticImage, SemanticDiff,
    SemanticEntityChange, SemanticImageIdentity, SemanticLinkChangeKind, SemanticPlaneCatalog,
    SemanticPlaneImageKey, SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneSegment,
    SemanticRangeRequest, SemanticSegmentId, SemanticSnapshot, UntrustedSemanticSegmentId,
};
use backend_store::{
    ArtifactBudget, ArtifactObjectReader, FileStore, ObjectId, TypedObject, UntrustedObjectId,
};
use backend_version::{
    ObjectKey, ObjectKeyHasher, ObjectVersion, ObjectVersionHasher, Schema, SchemaIdentity,
};

use crate::{
    AuthorityClaim, AuthorityEpoch, ByteRange, ChunkChain, ChunkParts, DurableSemanticRangeStore,
    DurableSemanticSegmentStore, Frame, ImmutableObjectSchema, ReceivingCas, ReceivingCasSink,
    ReplicationError, SelectedSemanticPlane, SemanticImageCacheError, SparseCoverage, StagedExtent,
    TransferId, TransportLimits, UnverifiedObjectRequest, WireReceivingCheckpoint,
    claim_schema_object_key, claim_schema_object_version,
};

use super::ir_hydration::SelectedGenerationStamp;
use super::ir_hydration_wire::{
    MAX_CAS_CHECKPOINT_BYTES, MAX_RANGE_BYTES, SelectedSemanticImageChunk, WireReader, WireWriter,
};

const RECORD_TAG: u8 = 4;
const MAP_TAG: u8 = 5;
const CONTENT_MAP_TAG: u8 = 6;
const MAX_RECORD_BYTES: usize = MAX_CAS_CHECKPOINT_BYTES + 16 * 1024;
const IO_BUFFER_BYTES: usize = 16 * 1024;
const MAX_SEMANTIC_OBJECT_BYTES: u64 = MAX_SEMANTIC_SEGMENT_BYTES as u64;
const MAX_SPARSE_SESSIONS: usize = 64;
const MAX_SPARSE_RESERVED_BYTES: u64 =
    MAX_SPARSE_SESSIONS as u64 * (MAX_SEMANTIC_OBJECT_BYTES + MAX_RECORD_BYTES as u64);
const SPARSE_SESSION_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_SEMANTIC_MAPPINGS: usize = 16_384;
const MAX_SEMANTIC_MAPPING_BYTES: u64 = 16 * 1024 * 1024;
const SEMANTIC_MAPPING_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const SEGMENT_SCHEMA_DOMAIN: u8 = 0x52;
const SEGMENT_SCHEMA_TYPE: u16 = 0xfffb;

/// Counts returned by the canonical IR-VCS entity and link iterators.
/// `None` from `semantic_diff_summary` means one complete local base image is
/// unavailable; it never estimates a diff from manifest segment identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalSemanticDiffSummary {
    /// Canonical VCS generation used as the before reader.
    pub before: GenerationId,
    /// Canonical VCS generation used as the after reader.
    pub after: GenerationId,
    /// Stable declaration identities present only in the after image.
    pub introduced_entities: usize,
    /// Stable declaration identities present only in the before image.
    pub deleted_entities: usize,
    /// Retained declaration identities with changed semantic evidence.
    pub changed_entities: usize,
    /// Stable graph relations present only in the after image.
    pub added_links: usize,
    /// Stable graph relations present only in the before image.
    pub removed_links: usize,
    /// Stable graph relations whose evidence changed.
    pub changed_links: usize,
    /// Image-level source/recipe/scope provenance change reported by IR-VCS.
    pub provenance: FacetChange,
}

/// Raw canonical bytes for one semantic segment in the local immutable CAS.
struct SemanticSegmentPayload;

impl Schema for SemanticSegmentPayload {
    const DOMAIN: u8 = SEGMENT_SCHEMA_DOMAIN;
    const TYPE: u16 = SEGMENT_SCHEMA_TYPE;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// A semantic-segment object whose complete FileStore envelope, physical ID,
/// runtime schema, typed key, and typed version were verified on open.
/// Payload access stays range-based so callers choose their own bounded
/// scratch or their own output allocation.
struct VerifiedMappedSemanticSegment {
    object_id: ObjectId,
    byte_length: u64,
    reader: ArtifactObjectReader,
}

impl VerifiedMappedSemanticSegment {
    fn object_id(&self) -> ObjectId {
        self.object_id
    }

    fn byte_length(&self) -> u64 {
        self.byte_length
    }

    fn read_range(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, String> {
        self.reader
            .read_payload_range(offset, output)
            .map_err(|error| format!("read mapped semantic object payload: {error:?}"))
    }

    /// Streams a mapped payload through both the semantic segment admission
    /// and FileStore's typed object identity checks using caller-owned scratch.
    fn verify_segment(
        &mut self,
        plane: SemanticPlaneKind,
        segment: &SemanticPlaneSegment,
        scratch: &mut [u8],
    ) -> Result<SemanticSegmentId, String> {
        if segment.byte_length() != self.byte_length {
            return Err("mapped semantic object has the wrong length".to_owned());
        }
        if scratch.is_empty() && self.byte_length != 0 {
            return Err("mapped semantic object scratch buffer is empty".to_owned());
        }
        let capacity = usize::try_from(self.byte_length)
            .map_err(|_| "mapped semantic object is too large".to_owned())?;
        let schema = SchemaIdentity::new(
            SemanticSegmentPayload::DOMAIN,
            SemanticSegmentPayload::TYPE,
            SemanticSegmentPayload::VERSION,
        );
        let mut key_hasher = ObjectKeyHasher::new(schema, capacity).map_err(display_error)?;
        let mut version_hasher =
            ObjectVersionHasher::new(schema, capacity).map_err(display_error)?;
        let mut semantic_verifier = segment.streaming_admission(plane);
        let mut offset = 0_u64;
        while offset < self.byte_length {
            let remaining = usize::try_from(self.byte_length - offset)
                .map_err(|_| "mapped semantic object offset overflows".to_owned())?;
            let take = remaining.min(scratch.len());
            let read = self.read_range(offset, &mut scratch[..take])?;
            if read != take {
                return Err("mapped semantic object payload is truncated".to_owned());
            }
            let chunk = &scratch[..read];
            key_hasher.update(chunk).map_err(display_error)?;
            version_hasher.update(chunk).map_err(display_error)?;
            semantic_verifier.update(chunk).map_err(display_error)?;
            offset = offset
                .checked_add(u64::try_from(read).map_err(display_error)?)
                .ok_or_else(|| "mapped semantic object offset overflows".to_owned())?;
        }
        let key = key_hasher
            .finish_key::<SemanticSegmentPayload>()
            .map_err(display_error)?;
        let version = version_hasher
            .finish_version::<SemanticSegmentPayload>()
            .map_err(display_error)?;
        if key.as_bytes() != self.reader.key() || version.as_bytes() != self.reader.version() {
            return Err("mapped semantic payload identity changed during read".to_owned());
        }
        semantic_verifier.finish().map_err(display_error)
    }

    fn read_payload(&mut self) -> Result<Box<[u8]>, String> {
        let capacity = usize::try_from(self.byte_length)
            .map_err(|_| "mapped semantic object is too large".to_owned())?;
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(capacity)
            .map_err(|_| "mapped semantic object allocation failed".to_owned())?;
        payload.resize(capacity, 0);
        let schema = SchemaIdentity::new(
            SemanticSegmentPayload::DOMAIN,
            SemanticSegmentPayload::TYPE,
            SemanticSegmentPayload::VERSION,
        );
        let mut key_hasher = ObjectKeyHasher::new(schema, capacity).map_err(display_error)?;
        let mut version_hasher =
            ObjectVersionHasher::new(schema, capacity).map_err(display_error)?;
        let mut offset = 0_u64;
        while offset < self.byte_length {
            let start = usize::try_from(offset)
                .map_err(|_| "mapped semantic object offset overflows".to_owned())?;
            let take = (capacity - start).min(IO_BUFFER_BYTES);
            let read = self.read_range(offset, &mut payload[start..start + take])?;
            if read != take {
                return Err("mapped semantic object payload is truncated".to_owned());
            }
            key_hasher
                .update(&payload[start..start + read])
                .map_err(display_error)?;
            version_hasher
                .update(&payload[start..start + read])
                .map_err(display_error)?;
            offset = offset
                .checked_add(u64::try_from(read).map_err(|_| "read size overflow".to_owned())?)
                .ok_or_else(|| "mapped semantic object offset overflows".to_owned())?;
        }
        let key = key_hasher
            .finish_key::<SemanticSegmentPayload>()
            .map_err(display_error)?;
        let version = version_hasher
            .finish_version::<SemanticSegmentPayload>()
            .map_err(display_error)?;
        if key.as_bytes() != self.reader.key() || version.as_bytes() != self.reader.version() {
            return Err("mapped semantic payload identity changed during read".to_owned());
        }
        Ok(payload.into_boxed_slice())
    }

    /// Compares a complete payload through caller-owned bounded scratch while
    /// recomputing its typed identities from the bytes actually read.
    fn matches_payload(&mut self, expected: &[u8], scratch: &mut [u8]) -> Result<bool, String> {
        if u64::try_from(expected.len()).map_err(display_error)? != self.byte_length {
            return Ok(false);
        }
        if scratch.is_empty() && self.byte_length != 0 {
            return Err("mapped semantic object scratch buffer is empty".to_owned());
        }
        let capacity = usize::try_from(self.byte_length)
            .map_err(|_| "mapped semantic object is too large".to_owned())?;
        let schema = SchemaIdentity::new(
            SemanticSegmentPayload::DOMAIN,
            SemanticSegmentPayload::TYPE,
            SemanticSegmentPayload::VERSION,
        );
        let mut key_hasher = ObjectKeyHasher::new(schema, capacity).map_err(display_error)?;
        let mut version_hasher =
            ObjectVersionHasher::new(schema, capacity).map_err(display_error)?;
        let mut offset = 0_u64;
        let mut matches = true;
        while offset < self.byte_length {
            let start = usize::try_from(offset)
                .map_err(|_| "mapped semantic object offset overflows".to_owned())?;
            let take = (capacity - start).min(scratch.len());
            let read = self.read_range(offset, &mut scratch[..take])?;
            if read != take {
                return Err("mapped semantic object payload is truncated".to_owned());
            }
            key_hasher.update(&scratch[..read]).map_err(display_error)?;
            version_hasher
                .update(&scratch[..read])
                .map_err(display_error)?;
            matches &= scratch[..read] == expected[start..start + read];
            offset = offset
                .checked_add(u64::try_from(read).map_err(|_| "read size overflow".to_owned())?)
                .ok_or_else(|| "mapped semantic object offset overflows".to_owned())?;
        }
        let key = key_hasher
            .finish_key::<SemanticSegmentPayload>()
            .map_err(display_error)?;
        let version = version_hasher
            .finish_version::<SemanticSegmentPayload>()
            .map_err(display_error)?;
        if key.as_bytes() != self.reader.key() || version.as_bytes() != self.reader.version() {
            return Err("mapped semantic payload identity changed during read".to_owned());
        }
        Ok(matches)
    }
}

/// Durable FileStore-backed sparse receiver for versioned semantic segments.
///
/// It requires the negotiated transfer chunk size to be exactly 16 KiB. This
/// keeps the receiving-CAS extent count bounded by the published segment byte
/// limit while matching the local range protocol's maximum payload.
pub struct FileSemanticRangeStore {
    store: FileStore,
    state_root: PathBuf,
    content_root: PathBuf,
    generations: super::ir_generation_store::LocalSemanticGenerationFiles,
    images: super::ir_image_store::LocalSemanticImageFiles,
    limits: TransportLimits,
}

impl fmt::Debug for FileSemanticRangeStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileSemanticRangeStore")
            .field("root", &self.state_root)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl FileSemanticRangeStore {
    /// Opens the semantic sparse-transfer state adjacent to the FileStore.
    /// The FileStore continues to own all completed immutable object bytes.
    pub fn open(store: FileStore, limits: TransportLimits) -> Result<Self, String> {
        let limits = limits.validate().map_err(display_error)?;
        if limits.max_chunk != MAX_RANGE_BYTES || limits.max_frame < MAX_RANGE_BYTES + 192 {
            return Err("semantic range CAS requires 16 KiB chunks and frame headroom".to_owned());
        }
        let state_root = store.root().join("semantic-hydration");
        let sessions = state_root.join("sessions");
        let mappings = state_root.join("mappings");
        let content_root = state_root.join("segments");
        fs::create_dir_all(&sessions).map_err(display_io)?;
        fs::create_dir_all(&mappings).map_err(display_io)?;
        fs::create_dir_all(&content_root).map_err(display_io)?;
        validate_directory(&state_root)?;
        validate_directory(&sessions)?;
        validate_directory(&mappings)?;
        validate_directory(&content_root)?;
        set_private_directory(&state_root)?;
        set_private_directory(&sessions)?;
        set_private_directory(&mappings)?;
        set_private_directory(&content_root)?;
        let generations =
            super::ir_generation_store::LocalSemanticGenerationFiles::open(&state_root)?;
        let images = super::ir_image_store::LocalSemanticImageFiles::open(&state_root)?;
        let store = Self {
            store,
            state_root,
            content_root,
            generations,
            images,
            limits,
        };
        let _state_lock = store.acquire_state_lock()?;
        store.images.recover_stages()?;
        store.prune_sparse_state()?;
        prune_mapping_state(&store.state_root.join("mappings"), None)?;
        prune_mapping_state(&store.content_root, None)?;
        Ok(store)
    }

    fn acquire_state_lock(&self) -> Result<File, String> {
        let path = self.state_root.join("state.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(display_io)?;
        ensure_regular_file(&path)?;
        set_private_file(&path)?;
        file.lock().map_err(display_io)?;
        Ok(file)
    }

    fn prune_sparse_state(&self) -> Result<(), String> {
        prune_sparse_state(&self.state_root.join("sessions"))
    }

    fn session_path(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> PathBuf {
        self.state_root
            .join("sessions")
            .join(format!("{}.sparse", hex(&logical_key(selection, request))))
    }

    fn record_path(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> PathBuf {
        self.state_root.join("sessions").join(format!(
            "{}.checkpoint",
            hex(&logical_key(selection, request))
        ))
    }

    fn mapping_path(
        &self,
        selection: SelectedSemanticPlane,
        segment: UntrustedSemanticSegmentId,
    ) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.semantic.range-map-path.v1\0");
        hash_stamp(&mut hasher, selection.stamp());
        hash_image(&mut hasher, selection.image());
        hash_plane(&mut hasher, selection.kind());
        hasher.update(segment.as_bytes());
        self.state_root
            .join("mappings")
            .join(format!("{}.map", hasher.finalize().to_hex()))
    }

    fn content_mapping_path(&self, segment: UntrustedSemanticSegmentId) -> PathBuf {
        self.content_root
            .join(format!("{}.map", hex(segment.as_bytes())))
    }

    fn transfer_fence(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<(TransferId, AuthorityClaim), String> {
        let identity = logical_key(selection, request);
        let mut transfer_bytes = [0_u8; 8];
        transfer_bytes.copy_from_slice(&identity[..8]);
        let transfer_value = u64::from_be_bytes(transfer_bytes) | 1;
        let transfer = TransferId::new(transfer_value).map_err(display_error)?;
        let authority_key = ObjectKey::<ImmutableObjectSchema>::from_value(identity.as_slice());
        let authority = AuthorityClaim::from_typed(
            &authority_key,
            AuthorityEpoch(selection.stamp().selection_revision()),
        );
        Ok((transfer, authority))
    }

    fn unverified_request(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<UnverifiedObjectRequest<SemanticSegmentPayload>, String> {
        let (transfer, _) = self.transfer_fence(selection, request)?;
        let placeholder = b"semantic-segment-unverified-routing-claim".as_slice();
        let key = ObjectKey::<SemanticSegmentPayload>::from_value(placeholder);
        let version = ObjectVersion::<SemanticSegmentPayload>::from_value(placeholder);
        UnverifiedObjectRequest::new(
            transfer,
            claim_schema_object_key(key).map_err(display_error)?,
            claim_schema_object_version(version).map_err(display_error)?,
            request.byte_length,
            self.limits,
        )
        .map_err(display_error)
    }

    fn max_extents(&self, request: SemanticRangeRequest) -> Result<usize, String> {
        usize::try_from(request.byte_length.div_ceil(MAX_RANGE_BYTES as u64))
            .map_err(|_| "semantic segment extent budget overflows usize".to_owned())
    }

    fn read_record(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<Option<SparseRecord>, String> {
        let path = self.record_path(selection, request);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(display_io(error)),
        };
        if bytes.len() > MAX_RECORD_BYTES {
            return Err("semantic sparse checkpoint exceeds its bound".to_owned());
        }
        let record = decode_sparse_record(&bytes, self.limits, self.max_extents(request)?)?;
        if record.selected_stamp != selection.stamp()
            || record.image != selection.image()
            || record.request != request
            || record.checkpoint.transfer != self.transfer_fence(selection, request)?.0
            || record.checkpoint.len != request.byte_length
        {
            return Err("semantic sparse checkpoint is bound to another selection".to_owned());
        }
        Ok(Some(record))
    }

    fn persist_record(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        checkpoint: &WireReceivingCheckpoint<SemanticSegmentPayload>,
    ) -> Result<(), String> {
        let bytes = encode_sparse_record(
            selection,
            request,
            checkpoint,
            self.limits,
            self.max_extents(request)?,
        )?;
        backend_platform::durable::write_private_atomic(
            &self.record_path(selection, request),
            &bytes,
        )
        .map_err(display_io)
    }

    fn open_mapped_object(
        &self,
        selection: SelectedSemanticPlane,
        segment: UntrustedSemanticSegmentId,
    ) -> Result<Option<VerifiedMappedSemanticSegment>, String> {
        let selected_path = self.mapping_path(selection, segment);
        let content_path = self.content_mapping_path(segment);
        let (path, bytes, mapped_segment, claim, byte_length) = match fs::read(&selected_path) {
            Ok(bytes) => {
                let (mapped_segment, claim, byte_length) = decode_mapping(
                    &bytes,
                    selection.stamp(),
                    selection.image(),
                    selection.kind(),
                )?;
                (selected_path, bytes, mapped_segment, claim, byte_length)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let bytes = match fs::read(&content_path) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(None);
                    }
                    Err(error) => return Err(display_io(error)),
                };
                let (mapped_segment, claim, byte_length) = decode_content_mapping(&bytes)?;
                (content_path, bytes, mapped_segment, claim, byte_length)
            }
            Err(error) => return Err(display_io(error)),
        };
        if mapped_segment != segment {
            return Err("semantic object mapping names another segment".to_owned());
        }
        if byte_length == 0 || byte_length > MAX_SEMANTIC_OBJECT_BYTES {
            return Err("mapped semantic object exceeds its published bound".to_owned());
        }
        let sink = self.store.artifact_sink(ArtifactBudget::new(
            1,
            1,
            MAX_SEMANTIC_OBJECT_BYTES,
            IO_BUFFER_BYTES,
            1,
        ));
        let Some(object) = sink
            .open_object_limited(claim, MAX_SEMANTIC_OBJECT_BYTES)
            .map_err(|error| format!("verify mapped semantic object: {error:?}"))?
        else {
            // FileStore GC does not treat this convenience map as a root. A
            // vanished immutable member therefore means the local have entry
            // is stale and the caller should fetch the segment again. Compare
            // before removing so a concurrently replaced mapping survives.
            remove_mapping_if_unchanged(&path, &bytes)?;
            return Ok(None);
        };
        if object.id().as_bytes() != claim.as_bytes()
            || object.schema()
                != SchemaIdentity::new(
                    SemanticSegmentPayload::DOMAIN,
                    SemanticSegmentPayload::TYPE,
                    SemanticSegmentPayload::VERSION,
                )
            || object.payload_len() != byte_length
        {
            return Err(
                "mapped FileStore object does not match semantic payload schema".to_owned(),
            );
        }
        touch_mapping(&path);
        Ok(Some(VerifiedMappedSemanticSegment {
            object_id: object.id(),
            byte_length,
            reader: object,
        }))
    }

    /// Reopens the last durable local generation record for this exact target.
    /// The returned stamp is the authority observation saved with the head;
    /// callers still need a live authority check before freshness-sensitive use.
    pub fn current_local_generation(
        &self,
        target: &crate::SemanticTargetKey,
    ) -> Result<Option<super::ir_generation_store::LocalSemanticGeneration>, String> {
        let _state_lock = self.acquire_state_lock()?;
        self.generations.current(target)
    }

    /// Streams a selected segment already present in immutable local CAS and
    /// returns its admitted ID without materializing the payload.
    ///
    /// The caller must freshly obtain `selection` from the authority before
    /// this check. `None` means no mapped immutable CAS object was present;
    /// sparse receiving checkpoints are not read or admitted by this probe.
    pub fn verify_present_segment(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        segment: &SemanticPlaneSegment,
    ) -> Result<Option<SemanticSegmentId>, String> {
        self.validate_selection_request(selection, request)?;
        if request.segment_id.as_bytes() != segment.id_claim().as_bytes()
            || request.first_key != *segment.first_key()
            || request.last_key != *segment.last_key()
            || request.byte_length != segment.byte_length()
        {
            return Err("semantic request differs from its manifest segment".to_owned());
        }
        let mapped = {
            let _state_lock = self.acquire_state_lock()?;
            self.open_mapped_object(selection, request.segment_id)?
        };
        let Some(mut mapped) = mapped else {
            return Ok(None);
        };
        let mut scratch = [0_u8; IO_BUFFER_BYTES];
        let admitted = mapped.verify_segment(request.plane, segment, &mut scratch)?;
        if admitted.as_bytes() != request.segment_id.as_bytes() {
            return Err("mapped semantic segment differs from its selected claim".to_owned());
        }
        let object_id = mapped.object_id();
        let _state_lock = self.acquire_state_lock()?;
        self.persist_content_mapping(request.segment_id, object_id, request.byte_length)?;
        self.persist_selected_mapping(
            selection,
            request.segment_id,
            object_id,
            request.byte_length,
        )?;
        Ok(Some(admitted))
    }

    /// Reuses a complete local NXFI by canonical VCS generation after
    /// independently verifying its content identity and full grammar.
    pub fn find_semantic_image(
        &self,
        target: &crate::SemanticTargetKey,
        image: SemanticPlaneImageKey,
    ) -> Result<Option<MappedSemanticImage>, SemanticImageCacheError> {
        let _state_lock = self
            .acquire_state_lock()
            .map_err(SemanticImageCacheError::Storage)?;
        self.images.find_generation(target, image)
    }

    /// Reopens a known content-addressed NXFI after a selected page has bound
    /// its exact identity and canonical generation.
    pub fn find_semantic_image_identity(
        &self,
        target: &crate::SemanticTargetKey,
        identity: SemanticImageIdentity,
        generation: GenerationId,
    ) -> Result<Option<MappedSemanticImage>, SemanticImageCacheError> {
        let _state_lock = self
            .acquire_state_lock()
            .map_err(SemanticImageCacheError::Storage)?;
        self.images.find_identity(target, identity, generation)
    }

    /// Binds an exact selected image key to already verified content-addressed
    /// bytes after its first authority-checked page supplied the identity.
    pub fn bind_semantic_image_identity(
        &self,
        target: &crate::SemanticTargetKey,
        image: SemanticPlaneImageKey,
        identity: SemanticImageIdentity,
    ) -> Result<MappedSemanticImage, SemanticImageCacheError> {
        let _state_lock = self
            .acquire_state_lock()
            .map_err(SemanticImageCacheError::Storage)?;
        let mut retained = Vec::with_capacity(2);
        if let Some(current) = self
            .generations
            .current(target)
            .map_err(SemanticImageCacheError::Storage)?
        {
            retained.push((current.image(), current.image_identity()));
            if let (Some(previous_image), Some(previous_identity)) =
                (current.previous_image(), current.previous_image_identity())
            {
                retained.push((previous_image, previous_identity));
            }
        }
        self.images
            .bind_selected_key(target, image, identity, &retained)
    }

    /// Invalidates a corrupt content-addressed image after the selected owner
    /// has returned an exact initial page for this target, image, and stamp.
    /// The object is reopened under the state lock before deletion; healthy
    /// objects, generation mismatches, symlinks, and I/O failures are preserved.
    pub fn repair_corrupt_semantic_image(
        &self,
        target: &crate::SemanticTargetKey,
        image: SemanticPlaneImageKey,
        selected_stamp: SelectedGenerationStamp,
        owner_page: &SelectedSemanticImageChunk,
    ) -> Result<(), SemanticImageCacheError> {
        let _state_lock = self
            .acquire_state_lock()
            .map_err(SemanticImageCacheError::Storage)?;
        self.images
            .repair_corrupt_identity(target, image, selected_stamp, owner_page)
    }

    /// Runs the canonical IR-VCS entity and stable-link iterators over two
    /// verified mmap-backed complete NXFI readers. Missing images remain an
    /// explicit `None`; no manifest-segment comparison is substituted.
    pub fn semantic_diff_summary(
        &self,
        target: &crate::SemanticTargetKey,
        before_key: SemanticPlaneImageKey,
        after_key: SemanticPlaneImageKey,
    ) -> Result<Option<LocalSemanticDiffSummary>, String> {
        let before = self
            .find_semantic_image(target, before_key)
            .map_err(|error| error.to_string())?;
        let Some(before) = before else {
            return Ok(None);
        };
        let after = self
            .find_semantic_image(target, after_key)
            .map_err(|error| error.to_string())?;
        let Some(after) = after else {
            return Ok(None);
        };
        let before_view = before.view();
        let after_view = after.view();
        let diff = SemanticDiff::between(
            SemanticSnapshot {
                generation: before.generation(),
                reader: &before_view,
            },
            SemanticSnapshot {
                generation: after.generation(),
                reader: &after_view,
            },
        );
        let provenance = diff.entities.provenance;
        let mut summary = LocalSemanticDiffSummary {
            before: before.generation(),
            after: after.generation(),
            introduced_entities: 0,
            deleted_entities: 0,
            changed_entities: 0,
            added_links: 0,
            removed_links: 0,
            changed_links: 0,
            provenance,
        };
        for change in diff.entities {
            match change {
                SemanticEntityChange::Introduced { .. } => {
                    summary.introduced_entities = summary.introduced_entities.saturating_add(1);
                }
                SemanticEntityChange::Deleted { .. } => {
                    summary.deleted_entities = summary.deleted_entities.saturating_add(1);
                }
                SemanticEntityChange::Retained { .. } => {
                    summary.changed_entities = summary.changed_entities.saturating_add(1);
                }
            }
        }
        for change in diff.links {
            match change.kind {
                SemanticLinkChangeKind::Added => {
                    summary.added_links = summary.added_links.saturating_add(1);
                }
                SemanticLinkChangeKind::Removed => {
                    summary.removed_links = summary.removed_links.saturating_add(1);
                }
                SemanticLinkChangeKind::EvidenceChanged => {
                    summary.changed_links = summary.changed_links.saturating_add(1);
                }
            }
        }
        Ok(Some(summary))
    }

    /// Restores a sequential full-image page transfer after validating its
    /// bounded checksummed cursor and exact selected image key.
    pub fn resume_semantic_image_transfer(
        &self,
        target: &crate::SemanticTargetKey,
        image: SemanticPlaneImageKey,
    ) -> Result<Option<super::ir_image_store::SemanticImageResume>, String> {
        let _state_lock = self.acquire_state_lock()?;
        let retained = self.retained_image_keys(target)?;
        self.images.resume(target, image, &retained)
    }

    /// Discards one staged image transfer after a fresh selected owner page
    /// proves its saved identity or total length has changed.
    pub fn discard_semantic_image_transfer(
        &self,
        target: &crate::SemanticTargetKey,
        image: SemanticPlaneImageKey,
    ) -> Result<(), String> {
        let _state_lock = self.acquire_state_lock()?;
        self.images.discard_stage(target, image)
    }

    /// Durably stages one checked 16 KiB-or-smaller full-image page.
    pub fn stage_semantic_image_page(
        &self,
        target: &crate::SemanticTargetKey,
        image: SemanticPlaneImageKey,
        identity: SemanticImageIdentity,
        total_length: u64,
        byte_range: ByteRange,
        payload: &[u8],
    ) -> Result<super::ir_image_store::SemanticImageResume, String> {
        let _state_lock = self.acquire_state_lock()?;
        let retained = self.retained_image_keys(target)?;
        self.images.stage_page(
            target,
            image,
            identity,
            total_length,
            byte_range,
            payload,
            &retained,
        )
    }

    /// Reopens a complete staged NXFI, verifies it, then atomically admits its
    /// immutable image and canonical-generation locator into the local CAS.
    pub fn finish_semantic_image_transfer(
        &self,
        target: &crate::SemanticTargetKey,
        resume: super::ir_image_store::SemanticImageResume,
    ) -> Result<MappedSemanticImage, SemanticImageCacheError> {
        let _state_lock = self
            .acquire_state_lock()
            .map_err(SemanticImageCacheError::Storage)?;
        let mut retained = Vec::with_capacity(2);
        if let Some(current) = self
            .generations
            .current(target)
            .map_err(SemanticImageCacheError::Storage)?
        {
            retained.push((current.image(), current.image_identity()));
            if let (Some(image), Some(identity)) =
                (current.previous_image(), current.previous_image_identity())
            {
                retained.push((image, identity));
            }
        }
        self.images.finish(target, resume, &retained)
    }

    fn retained_image_keys(
        &self,
        target: &crate::SemanticTargetKey,
    ) -> Result<Vec<SemanticPlaneImageKey>, String> {
        let Some(current) = self.generations.current(target)? else {
            return Ok(Vec::new());
        };
        let mut retained = Vec::with_capacity(2);
        retained.push(current.image());
        if let Some(previous) = current.previous_image() {
            retained.push(previous);
        }
        Ok(retained)
    }

    /// Commits an immutable selected catalog/image generation and advances its
    /// local head only after every requested-plane segment is present in CAS,
    /// re-admitted against this manifest, and the same live authority stamp is
    /// observed immediately before the atomic head write.
    pub fn commit_local_generation<S: crate::SelectedGenerationSource>(
        &mut self,
        target: &crate::SemanticTargetKey,
        catalog: &SemanticPlaneCatalog,
        image: SemanticPlaneImageKey,
        manifest: &SemanticPlaneManifest,
        selection: SelectedSemanticPlane,
        source: &mut S,
    ) -> Result<super::ir_generation_store::LocalSemanticGeneration, String> {
        if selection.image() != image
            || selection.image().manifest_root() != manifest.root()
            || target.profile() != selection.stamp().profile()
        {
            return Err("local semantic generation selection differs from its manifest".to_owned());
        }
        let stamp = selection.stamp();
        let full_image = {
            let _state_lock = self.acquire_state_lock()?;
            self.images
                .find_generation(target, image)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "complete canonical NXFI is not present in local storage".to_owned()
                })?
        };
        let image_identity = full_image.identity();
        let plane = selection.kind();
        let plane_manifest = manifest
            .plane(plane)
            .ok_or_else(|| "selected semantic plane is absent from its manifest".to_owned())?;
        for segment in plane_manifest.segments() {
            let claim = segment.id_claim();
            let mapped = {
                let _state_lock = self.acquire_state_lock()?;
                self.open_mapped_object(selection, claim)?
            };
            let Some(mut mapped) = mapped else {
                return Err(
                    "selected semantic plane is missing a verified local segment".to_owned(),
                );
            };
            let mut scratch = [0_u8; IO_BUFFER_BYTES];
            let admitted = mapped.verify_segment(plane, segment, &mut scratch)?;
            let request = SemanticRangeRequest {
                manifest_root: manifest.root(),
                plane,
                segment_id: claim,
                first_key: *segment.first_key(),
                last_key: *segment.last_key(),
                byte_length: segment.byte_length(),
            };
            if admitted.as_bytes() != claim.as_bytes() {
                return Err("local semantic segment identity changed during head commit".to_owned());
            }
            let _state_lock = self.acquire_state_lock()?;
            self.publish_selected_mapping(selection, request, mapped.object_id())?;
        }
        let committed = {
            let _state_lock = self.acquire_state_lock()?;
            let current_image = self
                .images
                .find_generation(target, image)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "complete canonical NXFI disappeared during local commit".to_owned()
                })?;
            if current_image.identity() != image_identity {
                return Err("canonical NXFI identity changed during local commit".to_owned());
            }
            let committed = self.generations.commit(
                target,
                stamp,
                catalog,
                image,
                image_identity,
                manifest,
                source,
            )?;
            let mut retained = vec![(committed.image(), committed.image_identity())];
            if let (Some(image), Some(identity)) = (
                committed.previous_image(),
                committed.previous_image_identity(),
            ) {
                retained.push((image, identity));
            }
            self.images.prune(target, retained)?;
            committed
        };
        Ok(committed)
    }

    fn publish_selected_mapping(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        object_id: ObjectId,
    ) -> Result<(), String> {
        let segment = request.segment_id;
        self.persist_content_mapping(segment, object_id, request.byte_length)?;
        self.persist_selected_mapping(selection, segment, object_id, request.byte_length)
    }

    fn persist_content_mapping(
        &self,
        segment: UntrustedSemanticSegmentId,
        object_id: ObjectId,
        byte_length: u64,
    ) -> Result<(), String> {
        persist_content_mapping(
            &self.content_mapping_path(segment),
            segment,
            object_id,
            byte_length,
        )
    }

    fn persist_selected_mapping(
        &self,
        selection: SelectedSemanticPlane,
        segment: UntrustedSemanticSegmentId,
        object_id: ObjectId,
        byte_length: u64,
    ) -> Result<(), String> {
        let encoded = encode_mapping(
            selection.stamp(),
            selection.image(),
            selection.kind(),
            segment,
            object_id,
            byte_length,
        )
        .map_err(display_error)?;
        let path = self.mapping_path(selection, segment);
        match fs::read(&path) {
            Ok(existing) => {
                let (observed_segment, observed_object, observed_len) = decode_mapping(
                    &existing,
                    selection.stamp(),
                    selection.image(),
                    selection.kind(),
                )?;
                if observed_segment != segment
                    || observed_object.as_bytes() != object_id.as_bytes()
                    || observed_len != byte_length
                {
                    return Err("semantic selection mapping conflicts with verified CAS".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                prune_mapping_state(
                    &self.state_root.join("mappings"),
                    Some((&path, encoded.len() as u64)),
                )?;
                backend_platform::durable::write_private_atomic(&path, &encoded)
                    .map_err(display_io)?;
            }
            Err(error) => return Err(display_io(error)),
        }
        Ok(())
    }

    fn full_coverage(&self, request: SemanticRangeRequest) -> Result<SparseCoverage, String> {
        let mut coverage = SparseCoverage::new(self.limits.max_ranges).map_err(display_error)?;
        coverage
            .insert(ByteRange::new(0, request.byte_length).map_err(display_error)?)
            .map_err(display_error)?;
        Ok(coverage)
    }

    fn create_or_resume(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        record: Option<SparseRecord>,
    ) -> Result<
        (
            ReceivingCas<FileSparseSink, SemanticSegmentPayload>,
            FileSparseSink,
        ),
        String,
    > {
        let mut sink = self.file_sink(selection, request)?;
        if record.is_none() {
            remove_if_present(&sink.session_path)?;
            self.ensure_sparse_capacity(request, &sink.session_path)?;
        }
        let (_transfer, authority) = self.transfer_fence(selection, request)?;
        let max_extents = self.max_extents(request)?;
        let receiving = if let Some(record) = record {
            let expected = self.unverified_request(selection, request)?;
            ReceivingCas::resume_unverified(
                expected,
                authority,
                self.limits,
                max_extents,
                record.checkpoint,
                &mut sink,
            )
        } else {
            let request = self.unverified_request(selection, request)?;
            // A byte file without its atomic checkpoint cannot contribute to
            // coverage. Discard it before opening a fresh receiving session.
            ReceivingCas::new_unverified(request, authority, self.limits, max_extents, &mut sink)
        };
        receiving
            .map(|receiving| (receiving, sink))
            .map_err(display_error)
    }

    fn file_sink(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<FileSparseSink, String> {
        Ok(FileSparseSink {
            store: self.store.clone(),
            session_path: self.session_path(selection, request),
            mapping_path: self.mapping_path(selection, request.segment_id),
            content_mapping_path: self.content_mapping_path(request.segment_id),
            expected_transfer: self.transfer_fence(selection, request)?.0,
            selected_stamp: selection.stamp(),
            image: selection.image(),
            selected_plane: selection.kind(),
            range_request: request,
        })
    }

    fn cleanup_session(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<(), String> {
        remove_if_present(&self.session_path(selection, request))?;
        remove_if_present(&self.record_path(selection, request))
    }

    fn ensure_sparse_capacity(
        &self,
        request: SemanticRangeRequest,
        requested_path: &Path,
    ) -> Result<(), String> {
        self.prune_sparse_state()?;
        if request.byte_length == 0 || request.byte_length > MAX_SEMANTIC_OBJECT_BYTES {
            return Err("semantic sparse request exceeds its published bound".to_owned());
        }
        let usage = sparse_usage(&self.state_root.join("sessions"))?;
        if usage
            .iter()
            .any(|session| session.sparse_path.as_path() == requested_path)
        {
            return Ok(());
        }
        let count = usage.len().saturating_add(1);
        let requested_reservation = request
            .byte_length
            .checked_add(MAX_RECORD_BYTES as u64)
            .ok_or_else(|| "semantic sparse reservation overflows".to_owned())?;
        let reserved = usage
            .iter()
            .try_fold(requested_reservation, |total, session| {
                total.checked_add(session.reserved_bytes)
            })
            .ok_or_else(|| "semantic sparse reservation overflows".to_owned())?;
        if count > MAX_SPARSE_SESSIONS || reserved > MAX_SPARSE_RESERVED_BYTES {
            return Err("semantic sparse-transfer retention quota is full".to_owned());
        }
        Ok(())
    }
}

impl DurableSemanticSegmentStore for FileSemanticRangeStore {
    type Error = String;

    fn commit_and_read(
        &mut self,
        selection: SelectedSemanticPlane,
        segment: SemanticSegmentId,
        payload: &[u8],
        admit: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<Box<[u8]>, Self::Error> {
        let _state_lock = self.acquire_state_lock()?;
        let segment_claim = UntrustedSemanticSegmentId::from_raw(*segment.as_bytes());
        if let Some(mut mapped_object) = self.open_mapped_object(selection, segment_claim)? {
            let object_id = mapped_object.object_id();
            let mapped = mapped_object.read_payload()?;
            if mapped.as_ref() != payload {
                return Err("mapped semantic object differs from admitted payload".to_owned());
            }
            admit(&mapped).map_err(display_error)?;
            self.persist_content_mapping(
                segment_claim,
                object_id,
                u64::try_from(mapped.len()).map_err(display_error)?,
            )?;
            self.persist_selected_mapping(
                selection,
                segment_claim,
                object_id,
                u64::try_from(mapped.len()).map_err(display_error)?,
            )?;
            return Ok(mapped);
        }
        let request = self
            .read_record_for_segment(selection, segment_claim)?
            .ok_or_else(|| "complete semantic bytes have no sparse CAS checkpoint".to_owned())?;
        if request.byte_length != payload.len() as u64 {
            return Err("admitted semantic payload length changed".to_owned());
        }
        let record = self
            .read_record(selection, request)?
            .ok_or_else(|| "semantic sparse checkpoint disappeared".to_owned())?;
        if !record.checkpoint.coverage.is_complete(request.byte_length) {
            return Err("semantic sparse CAS is not complete".to_owned());
        }
        let (receiving, mut sink) = self.create_or_resume(selection, request, Some(record))?;
        let object_id = match receiving.finish_unverified_with_admission(&mut sink, |staged| {
            if staged != payload {
                return Err(ReplicationError::IdentityMismatch);
            }
            admit(staged)
        }) {
            Ok(object_id) => object_id,
            Err(error) => {
                self.cleanup_session(selection, request)?;
                return Err(display_error(error));
            }
        };
        let mut mapped_object = self
            .open_mapped_object(selection, segment_claim)?
            .ok_or_else(|| "durable semantic object mapping was not published".to_owned())?;
        let mapped_id = mapped_object.object_id();
        let mapped = mapped_object.read_payload()?;
        if mapped_id != object_id || mapped.as_ref() != payload {
            return Err("FileStore read-back differs from admitted semantic bytes".to_owned());
        }
        admit(&mapped).map_err(display_error)?;
        self.cleanup_session(selection, request)?;
        Ok(mapped)
    }
}

impl DurableSemanticRangeStore for FileSemanticRangeStore {
    type RangeError = String;

    fn stage_durable_range(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        byte_range: ByteRange,
        payload: &[u8],
    ) -> Result<SparseCoverage, Self::RangeError> {
        self.validate_selection_request(selection, request)?;
        let _state_lock = self.acquire_state_lock()?;
        if byte_range.len != payload.len() as u64 || byte_range.len > MAX_RANGE_BYTES as u64 {
            return Err("semantic range size is invalid".to_owned());
        }
        if let Some(mapped) = self.open_mapped_object(selection, request.segment_id)? {
            if mapped.byte_length() != request.byte_length {
                return Err("mapped semantic object has the wrong length".to_owned());
            }
            return self.full_coverage(request);
        }
        let existing = self.read_record(selection, request)?;
        let prior_coverage = existing
            .as_ref()
            .map(|record| record.checkpoint.coverage.clone())
            .unwrap_or(SparseCoverage::new(self.limits.max_ranges).map_err(display_error)?);
        let (sequence, previous_chain) = existing
            .as_ref()
            .map(|record| {
                (
                    record.checkpoint.extents.len() as u64,
                    record
                        .checkpoint
                        .extents
                        .last()
                        .map(|extent| extent.chain)
                        .unwrap_or(ChunkChain([0; 32])),
                )
            })
            .unwrap_or((0, ChunkChain([0; 32])));
        let (mut receiving, mut sink) = self.create_or_resume(selection, request, existing)?;
        if prior_coverage.covers(byte_range) {
            let retained = sink.read_range(byte_range)?;
            if retained.as_slice() != payload {
                return Err("duplicate semantic range conflicts with durable bytes".to_owned());
            }
            return Ok(prior_coverage);
        }
        let missing = prior_coverage
            .missing(request.byte_length, self.limits.max_ranges)
            .map_err(display_error)?;
        let first = missing
            .first()
            .copied()
            .ok_or_else(|| "semantic segment is already complete".to_owned())?;
        let expected_range = ByteRange::new(first.start, first.len.min(MAX_RANGE_BYTES as u64))
            .map_err(display_error)?;
        if byte_range != expected_range {
            return Err("semantic range is dropped, duplicated, or out of order".to_owned());
        }
        let (transfer, authority) = self.transfer_fence(selection, request)?;
        let placeholder = b"semantic-segment-unverified-routing-claim".as_slice();
        let staging_key = ObjectKey::<SemanticSegmentPayload>::from_value(placeholder);
        let staging_version = ObjectVersion::<SemanticSegmentPayload>::from_value(placeholder);
        let frame = Frame::<SemanticSegmentPayload>::new(
            transfer,
            staging_key,
            staging_version,
            ChunkParts {
                object_len: request.byte_length,
                offset: byte_range.start,
                sequence,
                previous_chain,
                payload: payload.to_vec(),
            },
            authority,
        )
        .map_err(display_error)?
        .admit(self.limits)
        .map_err(display_error)?;
        receiving.stage(&mut sink, frame).map_err(display_error)?;
        let durable_checkpoint = receiving.checkpoint_wire().map_err(display_error)?;
        self.persist_record(selection, request, &durable_checkpoint)?;
        Ok(receiving.receipt().coverage)
    }

    fn read_complete_segment(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<Option<Box<[u8]>>, Self::RangeError> {
        self.validate_selection_request(selection, request)?;
        let _state_lock = self.acquire_state_lock()?;
        if let Some(mut mapped) = self.open_mapped_object(selection, request.segment_id)? {
            if mapped.byte_length() != request.byte_length {
                return Err("mapped semantic object has the wrong length".to_owned());
            }
            return Ok(Some(mapped.read_payload()?));
        }
        let Some(record) = self.read_record(selection, request)? else {
            return Ok(None);
        };
        if !record.checkpoint.coverage.is_complete(request.byte_length) {
            return Ok(None);
        }
        let payload = self
            .file_sink(selection, request)?
            .read_full(request.byte_length)?;
        Ok(Some(payload.into_boxed_slice()))
    }

    fn checkpoint_sparse_segment(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<Vec<u8>, Self::RangeError> {
        self.validate_selection_request(selection, request)?;
        let record = self
            .read_record(selection, request)?
            .ok_or_else(|| "semantic sparse checkpoint is missing".to_owned())?;
        record
            .checkpoint
            .encode_bounded(
                MAX_CAS_CHECKPOINT_BYTES,
                self.limits,
                self.max_extents(request)?,
            )
            .map_err(display_error)
    }

    fn resume_sparse_segment(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        checkpoint: &[u8],
    ) -> Result<SparseCoverage, Self::RangeError> {
        self.validate_selection_request(selection, request)?;
        let _state_lock = self.acquire_state_lock()?;
        let record = self
            .read_record(selection, request)?
            .ok_or_else(|| "semantic sparse checkpoint is missing".to_owned())?;
        let supplied = WireReceivingCheckpoint::<SemanticSegmentPayload>::decode_bounded(
            checkpoint,
            MAX_CAS_CHECKPOINT_BYTES,
            self.limits,
            self.max_extents(request)?,
        )
        .map_err(display_error)?;
        if supplied != record.checkpoint {
            return Err("semantic sparse checkpoint differs from durable state".to_owned());
        }
        if self
            .open_mapped_object(selection, request.segment_id)?
            .is_some()
        {
            return self.full_coverage(request);
        }
        let (receiving, _sink) = self.create_or_resume(selection, request, Some(record))?;
        Ok(receiving.receipt().coverage)
    }

    fn discard_sparse_segment(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<(), Self::RangeError> {
        self.validate_selection_request(selection, request)?;
        let _state_lock = self.acquire_state_lock()?;
        self.cleanup_session(selection, request)
    }
}

impl FileSemanticRangeStore {
    fn validate_selection_request(
        &self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<(), String> {
        if selection.kind() != request.plane
            || selection.image().manifest_root() != request.manifest_root
            || request.byte_length == 0
            || request.byte_length > self.limits.max_object
            || request.byte_length > MAX_SEMANTIC_SEGMENT_BYTES as u64
            || request.first_key > request.last_key
            || request.segment_id.as_bytes() == &[0; 32]
        {
            return Err("semantic sparse request does not match selected plane".to_owned());
        }
        Ok(())
    }

    fn read_record_for_segment(
        &self,
        selection: SelectedSemanticPlane,
        segment: UntrustedSemanticSegmentId,
    ) -> Result<Option<SemanticRangeRequest>, String> {
        let path = self.state_root.join("sessions");
        let prefix = hex(&logical_key_prefix(selection, segment));
        let record_path = path.join(format!("{prefix}.checkpoint"));
        let bytes = match fs::read(record_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(display_io(error)),
        };
        let mut reader = checked_reader(&bytes)?;
        reader.header(RECORD_TAG).map_err(display_error)?;
        let stamp = reader.stamp().map_err(display_error)?;
        let image = reader.image().map_err(display_error)?;
        let request = reader.range_request().map_err(display_error)?;
        let _checkpoint = reader
            .sized_bytes(MAX_CAS_CHECKPOINT_BYTES)
            .map_err(display_error)?;
        reader.finish().map_err(display_error)?;
        if stamp != selection.stamp()
            || image != selection.image()
            || request.manifest_root != image.manifest_root()
            || request.segment_id != segment
        {
            return Err("semantic checkpoint does not match segment".to_owned());
        }
        Ok(Some(request))
    }
}

struct SparseRecord {
    selected_stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    request: SemanticRangeRequest,
    checkpoint: WireReceivingCheckpoint<SemanticSegmentPayload>,
}

struct SparseFileSession {
    transfer: TransferId,
    len: u64,
    file: File,
}

struct FileSparseSink {
    store: FileStore,
    session_path: PathBuf,
    mapping_path: PathBuf,
    content_mapping_path: PathBuf,
    expected_transfer: TransferId,
    selected_stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    selected_plane: SemanticPlaneKind,
    range_request: SemanticRangeRequest,
}

impl FileSparseSink {
    fn begin_file(
        &self,
        transfer: TransferId,
        len: u64,
    ) -> Result<SparseFileSession, ReplicationError> {
        if transfer != self.expected_transfer || len != self.range_request.byte_length {
            return Err(ReplicationError::StaleFence);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&self.session_path)
            .map_err(|error| map_file_error(&error))?;
        file.set_len(len).map_err(|error| map_file_error(&error))?;
        file.sync_all().map_err(|error| map_file_error(&error))?;
        backend_platform::durable::sync_parent(&self.session_path)
            .map_err(|error| map_file_error(&error))?;
        Ok(SparseFileSession {
            transfer,
            len,
            file,
        })
    }

    fn resume_file(
        &self,
        transfer: TransferId,
        len: u64,
    ) -> Result<SparseFileSession, ReplicationError> {
        if transfer != self.expected_transfer || len != self.range_request.byte_length {
            return Err(ReplicationError::StaleFence);
        }
        ensure_regular_file(&self.session_path).map_err(|_| ReplicationError::CorruptFrame)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.session_path)
            .map_err(|error| map_file_error(&error))?;
        if file
            .metadata()
            .map_err(|error| map_file_error(&error))?
            .len()
            != len
        {
            return Err(ReplicationError::CorruptFrame);
        }
        Ok(SparseFileSession {
            transfer,
            len,
            file,
        })
    }

    fn read_range(&self, range: ByteRange) -> Result<Vec<u8>, String> {
        if range.end().map_err(display_error)? > self.range_request.byte_length {
            return Err("semantic range exceeds sparse segment".to_owned());
        }
        ensure_regular_file(&self.session_path)?;
        let mut file = File::open(&self.session_path).map_err(display_io)?;
        file.seek(SeekFrom::Start(range.start))
            .map_err(display_io)?;
        let length = usize::try_from(range.len).map_err(|_| "range too large".to_owned())?;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes).map_err(display_io)?;
        Ok(bytes)
    }

    fn read_full(&self, len: u64) -> Result<Vec<u8>, String> {
        if len != self.range_request.byte_length {
            return Err("semantic sparse segment length changed".to_owned());
        }
        ensure_regular_file(&self.session_path)?;
        let mut file = File::open(&self.session_path).map_err(display_io)?;
        if file.metadata().map_err(display_io)?.len() != len {
            return Err("semantic sparse file length changed".to_owned());
        }
        let capacity = usize::try_from(len).map_err(|_| "segment is too large".to_owned())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| "segment allocation failed".to_owned())?;
        file.read_to_end(&mut bytes).map_err(display_io)?;
        if bytes.len() != capacity {
            return Err("semantic sparse file is truncated".to_owned());
        }
        Ok(bytes)
    }
}

impl ReceivingCasSink<SemanticSegmentPayload> for FileSparseSink {
    type Session = SparseFileSession;
    type Receipt = ObjectId;

    fn begin(
        &mut self,
        transfer: TransferId,
        _key: ObjectKey<SemanticSegmentPayload>,
        _version: ObjectVersion<SemanticSegmentPayload>,
        len: u64,
    ) -> Result<Self::Session, ReplicationError> {
        self.begin_file(transfer, len)
    }

    fn begin_unverified(
        &mut self,
        transfer: TransferId,
        len: u64,
    ) -> Result<Self::Session, ReplicationError> {
        self.begin_file(transfer, len)
    }

    fn resume(
        &mut self,
        transfer: TransferId,
        _key: ObjectKey<SemanticSegmentPayload>,
        _version: ObjectVersion<SemanticSegmentPayload>,
        len: u64,
        _checkpoint: &crate::ReceivingCheckpoint<SemanticSegmentPayload>,
    ) -> Result<Self::Session, ReplicationError> {
        self.resume_file(transfer, len)
    }

    fn resume_unverified(
        &mut self,
        transfer: TransferId,
        len: u64,
        _checkpoint: &WireReceivingCheckpoint<SemanticSegmentPayload>,
    ) -> Result<Self::Session, ReplicationError> {
        self.resume_file(transfer, len)
    }

    fn write(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        bytes: Arc<[u8]>,
    ) -> Result<(), ReplicationError> {
        if session.transfer != self.expected_transfer
            || extent.len != bytes.len() as u64
            || extent.range()?.end()? > session.len
        {
            return Err(ReplicationError::Range);
        }
        session
            .file
            .seek(SeekFrom::Start(extent.offset))
            .map_err(|error| map_file_error(&error))?;
        session
            .file
            .write_all(&bytes)
            .map_err(|error| map_file_error(&error))?;
        session
            .file
            .sync_all()
            .map_err(|error| map_file_error(&error))
    }

    fn read_extent(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        visitor: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<(), ReplicationError> {
        if session.transfer != self.expected_transfer || extent.range()?.end()? > session.len {
            return Err(ReplicationError::Range);
        }
        session
            .file
            .seek(SeekFrom::Start(extent.offset))
            .map_err(|error| map_file_error(&error))?;
        let mut remaining = extent.len;
        let mut buffer = [0_u8; IO_BUFFER_BYTES];
        while remaining > 0 {
            let take = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| ReplicationError::Overflow)?;
            session
                .file
                .read_exact(&mut buffer[..take])
                .map_err(|error| map_file_error(&error))?;
            visitor(&buffer[..take])?;
            remaining -= take as u64;
        }
        Ok(())
    }

    fn commit(
        &mut self,
        session: Self::Session,
        key: ObjectKey<SemanticSegmentPayload>,
        version: ObjectVersion<SemanticSegmentPayload>,
        len: u64,
        digest: [u8; 32],
    ) -> Result<Self::Receipt, ReplicationError> {
        if session.transfer != self.expected_transfer || len != session.len {
            return Err(ReplicationError::StaleFence);
        }
        let bytes = self
            .read_full(len)
            .map_err(|_| ReplicationError::Disconnected)?;
        let actual_key = ObjectKey::<SemanticSegmentPayload>::from_value(&bytes);
        let actual_version = ObjectVersion::<SemanticSegmentPayload>::from_value(&bytes);
        if actual_key != key || actual_version != version || version.as_bytes() != &digest {
            return Err(ReplicationError::IdentityMismatch);
        }
        let object = TypedObject::from_value(&key, bytes.as_slice());
        let object_id = self
            .store
            .write_object(&object)
            .map_err(|_| ReplicationError::Disconnected)?;
        persist_content_mapping(
            &self.content_mapping_path,
            self.range_request.segment_id,
            object_id,
            len,
        )
        .map_err(|_| ReplicationError::Disconnected)?;
        let mapping = encode_mapping(
            self.selected_stamp,
            self.image,
            self.selected_plane,
            self.range_request.segment_id,
            object_id,
            len,
        )
        .map_err(|_| ReplicationError::InvalidWire)?;
        match fs::read(&self.mapping_path) {
            Ok(existing) => {
                let (mapped_segment, mapped_object, mapped_len) = decode_mapping(
                    &existing,
                    self.selected_stamp,
                    self.image,
                    self.selected_plane,
                )
                .map_err(|_| ReplicationError::CorruptFrame)?;
                if mapped_segment != self.range_request.segment_id
                    || mapped_object.as_bytes() != object_id.as_bytes()
                    || mapped_len != len
                {
                    return Err(ReplicationError::ReplayConflict);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = self
                    .mapping_path
                    .parent()
                    .ok_or(ReplicationError::InvalidWire)?;
                prune_mapping_state(parent, Some((&self.mapping_path, mapping.len() as u64)))
                    .map_err(|_| ReplicationError::Disconnected)?;
                backend_platform::durable::write_private_atomic(&self.mapping_path, &mapping)
                    .map_err(|_| ReplicationError::Disconnected)?;
            }
            Err(_) => return Err(ReplicationError::Disconnected),
        }
        drop(session.file);
        remove_if_present(&self.session_path).map_err(|_| ReplicationError::Disconnected)?;
        Ok(object_id)
    }

    fn abort(&mut self, session: Self::Session) {
        drop(session);
        let _ = remove_if_present(&self.session_path);
    }
}

fn encode_sparse_record(
    selection: SelectedSemanticPlane,
    request: SemanticRangeRequest,
    checkpoint: &WireReceivingCheckpoint<SemanticSegmentPayload>,
    limits: TransportLimits,
    max_extents: usize,
) -> Result<Vec<u8>, String> {
    let cas = checkpoint
        .encode_bounded(MAX_CAS_CHECKPOINT_BYTES, limits, max_extents)
        .map_err(display_error)?;
    let mut writer = WireWriter::new(MAX_RECORD_BYTES);
    writer.header(RECORD_TAG).map_err(display_error)?;
    writer.stamp(selection.stamp()).map_err(display_error)?;
    writer.image(selection.image()).map_err(display_error)?;
    writer.range_request(request).map_err(display_error)?;
    writer
        .sized_bytes(&cas, MAX_CAS_CHECKPOINT_BYTES)
        .map_err(display_error)?;
    append_checksum(writer.finish())
}

fn decode_sparse_record(
    bytes: &[u8],
    limits: TransportLimits,
    max_extents: usize,
) -> Result<SparseRecord, String> {
    let mut reader = checked_reader(bytes)?;
    reader.header(RECORD_TAG).map_err(display_error)?;
    let selected_stamp = reader.stamp().map_err(display_error)?;
    let image = reader.image().map_err(display_error)?;
    let request = reader.range_request().map_err(display_error)?;
    let cas_bytes = reader
        .sized_bytes(MAX_CAS_CHECKPOINT_BYTES)
        .map_err(display_error)?;
    reader.finish().map_err(display_error)?;
    let checkpoint = WireReceivingCheckpoint::<SemanticSegmentPayload>::decode_bounded(
        &cas_bytes,
        MAX_CAS_CHECKPOINT_BYTES,
        limits,
        max_extents,
    )
    .map_err(display_error)?;
    if request.manifest_root != image.manifest_root() || request.byte_length != checkpoint.len {
        return Err("semantic sparse record has inconsistent selected identity".to_owned());
    }
    Ok(SparseRecord {
        selected_stamp,
        image,
        request,
        checkpoint,
    })
}

fn encode_mapping(
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    plane: SemanticPlaneKind,
    segment: UntrustedSemanticSegmentId,
    object_id: ObjectId,
    byte_length: u64,
) -> Result<Vec<u8>, ReplicationError> {
    let mut writer = WireWriter::new(512);
    writer.header(MAP_TAG)?;
    writer.stamp(stamp)?;
    writer.image(image)?;
    writer.plane(plane)?;
    writer.fixed(segment.as_bytes())?;
    writer.fixed(object_id.as_bytes())?;
    writer.u64(byte_length)?;
    append_checksum(writer.finish()).map_err(|_| ReplicationError::InvalidWire)
}

fn encode_content_mapping(
    segment: UntrustedSemanticSegmentId,
    object_id: ObjectId,
    byte_length: u64,
) -> Result<Vec<u8>, ReplicationError> {
    let mut writer = WireWriter::new(128);
    writer.header(CONTENT_MAP_TAG)?;
    writer.fixed(segment.as_bytes())?;
    writer.fixed(object_id.as_bytes())?;
    writer.u64(byte_length)?;
    append_checksum(writer.finish()).map_err(|_| ReplicationError::InvalidWire)
}

fn decode_content_mapping(
    bytes: &[u8],
) -> Result<(UntrustedSemanticSegmentId, UntrustedObjectId, u64), String> {
    let mut reader = checked_reader(bytes)?;
    reader.header(CONTENT_MAP_TAG).map_err(display_error)?;
    let segment = UntrustedSemanticSegmentId::from_raw(reader.fixed().map_err(display_error)?);
    let object_id = UntrustedObjectId::from_bytes(reader.fixed().map_err(display_error)?);
    let byte_length = reader.u64().map_err(display_error)?;
    reader.finish().map_err(display_error)?;
    if segment.as_bytes() == &[0; 32]
        || object_id.as_bytes() == &[0; 32]
        || byte_length == 0
        || byte_length > MAX_SEMANTIC_OBJECT_BYTES
    {
        return Err("semantic content mapping has an invalid identity or length".to_owned());
    }
    Ok((segment, object_id, byte_length))
}

fn persist_content_mapping(
    path: &Path,
    segment: UntrustedSemanticSegmentId,
    object_id: ObjectId,
    byte_length: u64,
) -> Result<(), String> {
    let encoded = encode_content_mapping(segment, object_id, byte_length).map_err(display_error)?;
    match fs::read(path) {
        Ok(existing) => {
            let (observed_segment, observed_object, observed_len) =
                decode_content_mapping(&existing)?;
            if observed_segment != segment
                || observed_object.as_bytes() != object_id.as_bytes()
                || observed_len != byte_length
            {
                return Err("semantic content mapping conflicts with verified CAS".to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| "semantic content mapping has no parent directory".to_owned())?;
            prune_mapping_state(parent, Some((path, encoded.len() as u64)))?;
            backend_platform::durable::write_private_atomic(path, &encoded).map_err(display_io)?;
        }
        Err(error) => return Err(display_io(error)),
    }
    Ok(())
}

fn decode_mapping(
    bytes: &[u8],
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    plane: SemanticPlaneKind,
) -> Result<(UntrustedSemanticSegmentId, UntrustedObjectId, u64), String> {
    let mut reader = checked_reader(bytes)?;
    reader.header(MAP_TAG).map_err(display_error)?;
    let mapped_stamp = reader.stamp().map_err(display_error)?;
    let mapped_image = reader.image().map_err(display_error)?;
    let mapped_plane = reader.plane().map_err(display_error)?;
    let segment = UntrustedSemanticSegmentId::from_raw(reader.fixed().map_err(display_error)?);
    let object_id = UntrustedObjectId::from_bytes(reader.fixed().map_err(display_error)?);
    let byte_length = reader.u64().map_err(display_error)?;
    reader.finish().map_err(display_error)?;
    if mapped_stamp != stamp
        || mapped_image != image
        || mapped_plane != plane
        || segment.as_bytes() == &[0; 32]
        || byte_length == 0
    {
        return Err("semantic object mapping has a mismatched root or plane".to_owned());
    }
    // The claim becomes a checked ObjectId only after FileStore verifies the
    // complete object envelope in `open_mapped_object`.
    Ok((segment, object_id, byte_length))
}

fn checked_reader(bytes: &[u8]) -> Result<WireReader<'_>, String> {
    if bytes.len() < 32 || bytes.len() > MAX_RECORD_BYTES {
        return Err("semantic state record has an invalid length".to_owned());
    }
    let checksum_at = bytes.len() - 32;
    if blake3::hash(&bytes[..checksum_at]).as_bytes() != &bytes[checksum_at..] {
        return Err("semantic state record checksum failed".to_owned());
    }
    Ok(WireReader::new(&bytes[..checksum_at]))
}

fn append_checksum(mut bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_RECORD_BYTES.saturating_sub(32) {
        return Err("semantic state record exceeds its bound".to_owned());
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn remove_mapping_if_unchanged(path: &Path, observed: &[u8]) -> Result<(), String> {
    match fs::read(path) {
        Ok(current) if current == observed => remove_if_present(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(display_io(error)),
    }
}

fn touch_mapping(path: &Path) {
    if let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

#[derive(Clone)]
struct SparsePart {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

#[derive(Default)]
struct SparsePair {
    sparse: Option<SparsePart>,
    checkpoint: Option<SparsePart>,
}

struct SparseUsage {
    sparse_path: PathBuf,
    reserved_bytes: u64,
    last_modified: SystemTime,
}

fn prune_sparse_state(directory: &Path) -> Result<(), String> {
    let now = SystemTime::now();
    let mut sessions = scan_sparse_sessions(directory)?;
    for pair in sessions.values() {
        let keep = match (&pair.sparse, &pair.checkpoint) {
            (Some(sparse), Some(checkpoint)) => {
                let latest = sparse.modified.max(checkpoint.modified);
                now.duration_since(latest).unwrap_or_default() < SPARSE_SESSION_RETENTION
            }
            _ => false,
        };
        if !keep {
            if let Some(part) = &pair.sparse {
                remove_if_present(&part.path)?;
            }
            if let Some(part) = &pair.checkpoint {
                remove_if_present(&part.path)?;
            }
        }
    }
    sessions = scan_sparse_sessions(directory)?;
    let mut usage = sparse_usage_from_sessions(sessions)?;
    usage.sort_by_key(|session| session.last_modified);
    while usage.len() > MAX_SPARSE_SESSIONS
        || usage
            .iter()
            .try_fold(0_u64, |total, session| {
                total.checked_add(session.reserved_bytes)
            })
            .unwrap_or(u64::MAX)
            > MAX_SPARSE_RESERVED_BYTES
    {
        let oldest = usage
            .first()
            .ok_or_else(|| "semantic sparse-session quota accounting failed".to_owned())?;
        let checkpoint = oldest.sparse_path.with_extension("checkpoint");
        remove_if_present(&oldest.sparse_path)?;
        remove_if_present(&checkpoint)?;
        usage.remove(0);
    }
    Ok(())
}

fn scan_sparse_sessions(directory: &Path) -> Result<BTreeMap<String, SparsePair>, String> {
    let mut sessions = BTreeMap::<String, SparsePair>::new();
    for entry in fs::read_dir(directory).map_err(display_io)? {
        let entry = entry.map_err(display_io)?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic sparse state filename is not UTF-8".to_owned())?;
        if name.ends_with(".tmp") {
            ensure_regular_file(&path)?;
            remove_if_present(&path)?;
            continue;
        }
        let (stem, checkpoint) = if let Some(stem) = name.strip_suffix(".sparse") {
            (stem, false)
        } else if let Some(stem) = name.strip_suffix(".checkpoint") {
            (stem, true)
        } else {
            return Err("semantic sparse state directory contains an unknown member".to_owned());
        };
        if !is_hex_digest(stem) {
            return Err("semantic sparse state filename is malformed".to_owned());
        }
        ensure_regular_file(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(display_io)?;
        let len = metadata.len();
        if (checkpoint && len > MAX_RECORD_BYTES as u64)
            || (!checkpoint && len > MAX_SEMANTIC_OBJECT_BYTES)
        {
            return Err("semantic sparse state member exceeds its bound".to_owned());
        }
        let part = SparsePart {
            path,
            len,
            modified: metadata.modified().map_err(display_io)?,
        };
        let pair = sessions.entry(stem.to_owned()).or_default();
        let slot = if checkpoint {
            &mut pair.checkpoint
        } else {
            &mut pair.sparse
        };
        if slot.replace(part).is_some() {
            return Err("semantic sparse state contains duplicate members".to_owned());
        }
        if sessions.len() > MAX_SPARSE_SESSIONS * 4 {
            return Err("semantic sparse state scan exceeds its bound".to_owned());
        }
    }
    Ok(sessions)
}

fn sparse_usage(directory: &Path) -> Result<Vec<SparseUsage>, String> {
    sparse_usage_from_sessions(scan_sparse_sessions(directory)?)
}

fn sparse_usage_from_sessions(
    sessions: BTreeMap<String, SparsePair>,
) -> Result<Vec<SparseUsage>, String> {
    let mut usage = Vec::with_capacity(sessions.len());
    for pair in sessions.into_values() {
        let (Some(sparse), Some(checkpoint)) = (pair.sparse, pair.checkpoint) else {
            return Err("semantic sparse state contains an incomplete session".to_owned());
        };
        let reserved_bytes = sparse
            .len
            .checked_add(MAX_RECORD_BYTES as u64)
            .ok_or_else(|| "semantic sparse reservation overflows".to_owned())?;
        usage.push(SparseUsage {
            sparse_path: sparse.path,
            reserved_bytes,
            last_modified: sparse.modified.max(checkpoint.modified),
        });
    }
    Ok(usage)
}

struct MappingFile {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

fn prune_mapping_state(directory: &Path, additional: Option<(&Path, u64)>) -> Result<(), String> {
    let now = SystemTime::now();
    let mut files = scan_mapping_files(directory)?;
    for file in &files {
        if now.duration_since(file.modified).unwrap_or_default() >= SEMANTIC_MAPPING_RETENTION {
            remove_if_present(&file.path)?;
        }
    }
    files.retain(|file| {
        now.duration_since(file.modified).unwrap_or_default() < SEMANTIC_MAPPING_RETENTION
    });
    files.sort_by_key(|file| file.modified);
    loop {
        let (additional_count, additional_bytes) = additional
            .map(|(path, len)| {
                if files.iter().any(|file| file.path.as_path() == path) {
                    (0_usize, 0_u64)
                } else {
                    (1_usize, len)
                }
            })
            .unwrap_or((0, 0));
        let bytes = files
            .iter()
            .try_fold(additional_bytes, |total, file| total.checked_add(file.len))
            .ok_or_else(|| "semantic mapping quota accounting overflows".to_owned())?;
        if files.len().saturating_add(additional_count) <= MAX_SEMANTIC_MAPPINGS
            && bytes <= MAX_SEMANTIC_MAPPING_BYTES
        {
            return Ok(());
        }
        if additional_count == 1 && additional_bytes > MAX_SEMANTIC_MAPPING_BYTES {
            return Err("semantic object mapping exceeds its retention quota".to_owned());
        }
        let remove_index = files
            .iter()
            .position(|file| additional.is_none_or(|(path, _)| file.path.as_path() != path))
            .ok_or_else(|| "semantic object mapping retention quota is full".to_owned())?;
        let oldest = files.remove(remove_index);
        remove_if_present(&oldest.path)?;
    }
}

fn scan_mapping_files(directory: &Path) -> Result<Vec<MappingFile>, String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).map_err(display_io)? {
        let entry = entry.map_err(display_io)?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic mapping filename is not UTF-8".to_owned())?;
        if name.ends_with(".tmp") {
            ensure_regular_file(&path)?;
            remove_if_present(&path)?;
            continue;
        }
        let stem = name
            .strip_suffix(".map")
            .ok_or_else(|| "semantic mapping directory contains an unknown member".to_owned())?;
        if !is_hex_digest(stem) {
            return Err("semantic mapping filename is malformed".to_owned());
        }
        ensure_regular_file(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(display_io)?;
        if metadata.len() > 512 {
            return Err("semantic mapping exceeds its fixed record bound".to_owned());
        }
        files.push(MappingFile {
            path,
            len: metadata.len(),
            modified: metadata.modified().map_err(display_io)?,
        });
        if files.len() > MAX_SEMANTIC_MAPPINGS * 2 {
            return Err("semantic mapping scan exceeds its bound".to_owned());
        }
    }
    Ok(files)
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn logical_key(selection: SelectedSemanticPlane, request: SemanticRangeRequest) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.sparse-session.v1\0");
    hash_stamp(&mut hasher, selection.stamp());
    hash_plane(&mut hasher, selection.kind());
    hash_image(&mut hasher, selection.image());
    hasher.update(request.manifest_root.as_bytes());
    hasher.update(request.segment_id.as_bytes());
    *hasher.finalize().as_bytes()
}

fn logical_key_prefix(
    selection: SelectedSemanticPlane,
    segment: UntrustedSemanticSegmentId,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.sparse-session.v1\0");
    hash_stamp(&mut hasher, selection.stamp());
    hash_plane(&mut hasher, selection.kind());
    hash_image(&mut hasher, selection.image());
    hasher.update(selection.image().manifest_root().as_bytes());
    hasher.update(segment.as_bytes());
    *hasher.finalize().as_bytes()
}

fn hash_stamp(hasher: &mut blake3::Hasher, stamp: SelectedGenerationStamp) {
    let mut writer = WireWriter::new(256);
    if writer.stamp(stamp).is_ok() {
        hasher.update(&writer.finish());
    } else {
        hasher.update(b"invalid-selected-stamp");
    }
}

fn hash_plane(hasher: &mut blake3::Hasher, plane: SemanticPlaneKind) {
    let mut writer = WireWriter::new(128);
    if writer.plane(plane).is_ok() {
        hasher.update(&writer.finish());
    } else {
        hasher.update(b"invalid-semantic-plane");
    }
}

fn hash_image(hasher: &mut blake3::Hasher, image: SemanticPlaneImageKey) {
    let mut writer = WireWriter::new(128);
    if writer.image(image).is_ok() {
        hasher.update(&writer.finish());
    } else {
        hasher.update(b"invalid-semantic-image");
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("semantic CAS state path is not a directory".to_owned());
    }
    Ok(())
}

fn set_private_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path).map_err(display_io)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions).map_err(display_io)?;
    }
    Ok(())
}

fn set_private_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path).map_err(display_io)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).map_err(display_io)?;
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("semantic sparse CAS member is not a regular file".to_owned());
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => backend_platform::durable::sync_parent(path).map_err(display_io),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(display_io(error)),
    }
}

fn map_file_error(error: &std::io::Error) -> ReplicationError {
    match error.kind() {
        std::io::ErrorKind::NotFound => ReplicationError::Disconnected,
        std::io::ErrorKind::AlreadyExists => ReplicationError::ReplayConflict,
        _ => ReplicationError::Disconnected,
    }
}

fn display_io(error: std::io::Error) -> String {
    error.to_string()
}

fn display_error(error: impl fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-semantic-range-retention-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create retention fixture directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn verified_mapped_segment_supports_bounded_range_and_identity_checks() {
        let directory = TestDirectory::create();
        let store =
            FileStore::open(directory.0.join("cas"), 4 * 1024 * 1024).expect("open semantic CAS");
        let payload = vec![0x5a; IO_BUFFER_BYTES + 73];
        let key = ObjectKey::<SemanticSegmentPayload>::from_value(payload.as_slice());
        let object = TypedObject::from_value(&key, payload.as_slice());
        let object_id = store.write_object(&object).expect("write semantic object");
        let claim = UntrustedObjectId::from_bytes(*object_id.as_bytes());
        let sink = store.artifact_sink(ArtifactBudget::new(
            1,
            1,
            MAX_SEMANTIC_OBJECT_BYTES,
            IO_BUFFER_BYTES,
            1,
        ));
        let reader = sink
            .open_object_limited(claim, MAX_SEMANTIC_OBJECT_BYTES)
            .expect("verify semantic object")
            .expect("semantic object exists");
        let mut mapped = VerifiedMappedSemanticSegment {
            object_id,
            byte_length: payload.len() as u64,
            reader,
        };

        let mut range = [0_u8; 41];
        assert_eq!(
            mapped.read_range(19, &mut range).expect("read range"),
            range.len()
        );
        assert_eq!(range.as_slice(), &payload[19..19 + range.len()]);

        let mut scratch = [0_u8; 257];
        let plane = SemanticPlaneKind::Ir(backend_semantic::ir::SemanticIrPlane::Core);
        let segment = SemanticPlaneSegment::from_payload(plane, [3; 32], [3; 32], 1, &payload)
            .expect("semantic segment claim");
        assert_eq!(
            mapped
                .verify_segment(plane, &segment, &mut scratch)
                .expect("admit mapped bytes through bounded scratch"),
            segment.admitted_id().expect("producer-admitted segment ID")
        );

        let mut changed = payload.clone();
        changed[IO_BUFFER_BYTES] ^= 1;
        let wrong_claim = SemanticPlaneSegment::from_payload(plane, [3; 32], [3; 32], 1, &changed)
            .expect("same-length different semantic claim");
        assert!(
            mapped
                .verify_segment(plane, &wrong_claim, &mut scratch)
                .expect_err("mapped content does not match different claim")
                .contains("segment content ID differs")
        );

        assert!(
            mapped
                .matches_payload(&payload, &mut scratch)
                .expect("compare payload through bounded scratch")
        );
        let mut changed = payload.clone();
        changed[IO_BUFFER_BYTES] ^= 1;
        assert!(
            !mapped
                .matches_payload(&changed, &mut scratch)
                .expect("detect different payload")
        );
        assert_eq!(
            mapped
                .read_payload()
                .expect("materialize on demand")
                .as_ref(),
            payload
        );
    }

    #[test]
    fn stale_mapping_invalidation_preserves_a_concurrent_replacement() {
        let directory = TestDirectory::create();
        let path = directory.0.join("mapping.map");
        fs::write(&path, b"old mapping").expect("write old mapping");
        remove_mapping_if_unchanged(&path, b"older observation")
            .expect("a replaced mapping is left alone");
        assert_eq!(
            fs::read(&path).expect("replacement remains"),
            b"old mapping"
        );
        remove_mapping_if_unchanged(&path, b"old mapping").expect("remove stale mapping");
        assert!(!path.exists());
    }

    #[test]
    fn sparse_quota_reserves_checkpoint_headroom_and_expires_abandoned_sessions() {
        let directory = TestDirectory::create();
        let stem = format!("{:064x}", 7);
        let sparse = directory.0.join(format!("{stem}.sparse"));
        let checkpoint = directory.0.join(format!("{stem}.checkpoint"));
        fs::write(&sparse, [1]).expect("write sparse segment");
        fs::write(&checkpoint, [2]).expect("write sparse checkpoint");
        let sparse_file = File::open(&sparse).expect("open sparse segment");
        let checkpoint_file = File::open(&checkpoint).expect("open sparse checkpoint");
        sparse_file
            .set_modified(UNIX_EPOCH)
            .expect("age sparse segment");
        checkpoint_file
            .set_modified(UNIX_EPOCH)
            .expect("age sparse checkpoint");
        prune_sparse_state(&directory.0).expect("expire abandoned sparse session");
        assert!(!sparse.exists());
        assert!(!checkpoint.exists());

        fs::write(&sparse, [1]).expect("write active sparse segment");
        fs::write(&checkpoint, [2]).expect("write active sparse checkpoint");
        let usage = sparse_usage(&directory.0).expect("measure sparse retention usage");
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].reserved_bytes, 1 + MAX_RECORD_BYTES as u64);
        assert!(usage[0].reserved_bytes <= MAX_SPARSE_RESERVED_BYTES);
    }

    #[test]
    fn expired_mapping_metadata_is_reclaimed_by_retention_policy() {
        let directory = TestDirectory::create();
        let path = directory.0.join(format!("{:064x}.map", 11));
        fs::write(&path, [3]).expect("write stale map");
        File::open(&path)
            .expect("open stale map")
            .set_modified(UNIX_EPOCH)
            .expect("age stale map");
        prune_mapping_state(&directory.0, None).expect("prune expired mapping metadata");
        assert!(!path.exists());
    }
}
