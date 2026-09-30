//! Bounded pages from one exact Turso-selected full semantic image.
//!
//! The selected catalog identifies the image, but every page still resolves
//! the live Turso selection before and after its payload read. The authority
//! source supplies bytes only after closure membership and immutable object
//! admission; this module binds those bytes to the client request.

use backend_engine::builtin::ProductSemanticPublicationKey;
use backend_replication::{
    ByteRange, MAX_SEMANTIC_IMAGE_BYTES, SelectedGenerationStamp, SelectedSemanticImageChunk,
    SelectedSemanticImageGet, SemanticTargetKey,
};
use backend_semantic::ir::{
    SemanticImageIdentity, SemanticPlaneCatalogRoot, SemanticPlaneImageKey,
};
use backend_store::StoreError;
use backend_store::{ArtifactBudget, ArtifactObjectReader, FileStore, UntrustedObjectId};
use core::fmt;
use std::sync::{Arc, Mutex};

pub(super) const MAX_SELECTED_IMAGE_RANGE_BYTES: u64 = 16 * 1024;
const VERIFIED_LOCAL_IMAGE_READER_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const VERIFIED_LOCAL_IMAGE_READER_CACHE_ENTRIES: usize = 8;
const SELECTED_FULL_IMAGE_PLAN_CACHE_ENTRIES: usize = 32;

/// Local CAS outcomes that decide whether selected-image reads may use S3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SelectedImageLocalReadError {
    /// The immutable object envelope or its authenticated identity is corrupt.
    CorruptEnvelope(String),
    /// The object path failed the store's regular-file or single-link checks.
    UnsafePath(String),
    /// A verified object disagrees with the selected authority metadata.
    AuthorityMismatch(String),
    /// A transient or otherwise unclassified local store operation failed.
    Storage(String),
}

impl From<StoreError> for SelectedImageLocalReadError {
    fn from(error: StoreError) -> Self {
        match error {
            corrupt @ StoreError::Corrupt => Self::CorruptEnvelope(format!("{corrupt:?}")),
            unsafe_path @ StoreError::UnsafePath => Self::UnsafePath(format!("{unsafe_path:?}")),
            error => Self::Storage(format!("{error:?}")),
        }
    }
}

impl SelectedImageLocalReadError {
    fn from_object_admission(error: StoreError) -> Self {
        match error {
            corrupt @ (StoreError::Corrupt | StoreError::Bounds) => {
                Self::CorruptEnvelope(format!("{corrupt:?}"))
            }
            error => Self::from(error),
        }
    }
}

/// Result of choosing a verified local page or the exact selected S3 route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SelectedImagePageError<E> {
    Local(SelectedImageLocalReadError),
    Remote(E),
}

/// Falls back only when the local object is absent or its authenticated
/// envelope is corrupt. Path safety and selected-authority failures deny the
/// read before the remote closure can run.
pub(super) fn read_local_or_remote<E>(
    local: Result<Option<Vec<u8>>, SelectedImageLocalReadError>,
    remote: impl FnOnce() -> Result<Vec<u8>, E>,
) -> Result<Vec<u8>, SelectedImagePageError<E>> {
    match local {
        Ok(Some(payload)) => Ok(payload),
        Ok(None) | Err(SelectedImageLocalReadError::CorruptEnvelope(_)) => {
            remote().map_err(SelectedImagePageError::Remote)
        }
        Err(error) => Err(SelectedImagePageError::Local(error)),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SelectedFullImagePlanCacheKey {
    namespace: [u8; 16],
    profile: backend_semantic::vocabulary::LanguageProfile,
    source_coordinate: [u8; 32],
    selection_revision: u64,
    selected_root: [u8; 32],
    closure_id: [u8; 32],
    catalog_root: SemanticPlaneCatalogRoot,
    image: SemanticPlaneImageKey,
}

impl SelectedFullImagePlanCacheKey {
    fn new(stamp: SelectedGenerationStamp, image: SemanticPlaneImageKey) -> Self {
        Self {
            namespace: *stamp.namespace(),
            profile: stamp.profile(),
            source_coordinate: *stamp.source_coordinate(),
            selection_revision: stamp.selection_revision(),
            selected_root: *stamp.selected_root(),
            closure_id: *stamp.closure_id(),
            catalog_root: stamp.catalog_root(),
            image,
        }
    }
}

/// Small bounded cache of already-admitted per-image plans. It contains no
/// compiler metadata blobs or image payloads. Every lookup is keyed by the
/// complete selected stamp, closure, and image tuple; cold misses are
/// serialized so concurrent first pages decode the selected catalog once.
#[derive(Debug)]
pub(super) struct SelectedFullImagePlanCache {
    entries: Mutex<hashlink::LruCache<SelectedFullImagePlanCacheKey, SelectedFullImagePlan>>,
}

impl Default for SelectedFullImagePlanCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(hashlink::LruCache::new(
                SELECTED_FULL_IMAGE_PLAN_CACHE_ENTRIES,
            )),
        }
    }
}

impl SelectedFullImagePlanCache {
    pub(super) fn get_or_resolve<E>(
        &self,
        stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
        resolve: impl FnOnce() -> Result<SelectedFullImagePlan, E>,
    ) -> Result<SelectedFullImagePlan, E> {
        let key = SelectedFullImagePlanCacheKey::new(stamp, image);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(plan) = entries.get(&key) {
            return Ok(plan.clone());
        }
        let plan = resolve()?;
        entries.insert(key, plan.clone());
        Ok(plan)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct VerifiedLocalImageReaderKey {
    closure_id: [u8; 32],
    object_id: [u8; 32],
    schema_domain: u8,
    schema_type: u16,
    schema_version: u8,
    payload_len: u64,
}

struct VerifiedLocalImageReader {
    reader: Arc<Mutex<ArtifactObjectReader>>,
    payload_len: u64,
}

struct VerifiedLocalImageReaderCacheState {
    entries: hashlink::LruCache<VerifiedLocalImageReaderKey, VerifiedLocalImageReader>,
    resident_bytes: u64,
}

/// Bounded cache of fully verified open local CAS readers. It retains file
/// handles and fixed metadata, never image payload allocations.
#[derive(Default)]
pub(super) struct VerifiedLocalImageReaderCache {
    state: Mutex<VerifiedLocalImageReaderCacheState>,
}

impl Default for VerifiedLocalImageReaderCacheState {
    fn default() -> Self {
        Self {
            entries: hashlink::LruCache::new(VERIFIED_LOCAL_IMAGE_READER_CACHE_ENTRIES),
            resident_bytes: 0,
        }
    }
}

impl VerifiedLocalImageReaderCache {
    /// Opens and verifies one selected object once, then seeks bounded pages
    /// through its retained file descriptor. `None` means the object is cold
    /// in local CAS and may be supplied by the selected S3 route.
    pub(super) fn read_range(
        &self,
        store: &FileStore,
        plan: &SelectedFullImagePlan,
        range: ByteRange,
    ) -> Result<Option<Vec<u8>>, SelectedImageLocalReadError> {
        let schema = backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA;
        let key = VerifiedLocalImageReaderKey {
            closure_id: plan.closure_id,
            object_id: plan.object_id,
            schema_domain: schema.domain(),
            schema_type: schema.ty(),
            schema_version: schema.version(),
            payload_len: plan.total_length,
        };
        let mut state = self.state.lock().map_err(|_| {
            SelectedImageLocalReadError::Storage(
                "selected image reader cache is poisoned".to_owned(),
            )
        })?;
        let reader = if let Some(cached) = state.entries.get(&key) {
            Arc::clone(&cached.reader)
        } else {
            let budget = ArtifactBudget::new(
                1,
                1,
                plan.total_length,
                usize::try_from(MAX_SELECTED_IMAGE_RANGE_BYTES).map_err(|_| {
                    SelectedImageLocalReadError::Storage(
                        "selected image page bound does not fit usize".to_owned(),
                    )
                })?,
                1,
            );
            let claim = UntrustedObjectId::from_bytes(plan.object_id);
            let Some(reader) = store
                .artifact_sink(budget)
                .open_object_limited(claim, plan.total_length)
                .map_err(SelectedImageLocalReadError::from_object_admission)?
            else {
                return Ok(None);
            };
            if reader.id().as_bytes() != &plan.object_id
                || reader.schema() != schema
                || reader.payload_len() != plan.total_length
            {
                return Err(SelectedImageLocalReadError::AuthorityMismatch(
                    "selected semantic image object differs from Turso metadata".to_owned(),
                ));
            }
            let reader = Arc::new(Mutex::new(reader));
            if plan.total_length <= VERIFIED_LOCAL_IMAGE_READER_CACHE_BYTES {
                while state.resident_bytes.saturating_add(plan.total_length)
                    > VERIFIED_LOCAL_IMAGE_READER_CACHE_BYTES
                    || state.entries.len() >= VERIFIED_LOCAL_IMAGE_READER_CACHE_ENTRIES
                {
                    let Some((_, evicted)) = state.entries.remove_lru() else {
                        break;
                    };
                    state.resident_bytes = state.resident_bytes.saturating_sub(evicted.payload_len);
                }
                if state.resident_bytes.saturating_add(plan.total_length)
                    <= VERIFIED_LOCAL_IMAGE_READER_CACHE_BYTES
                    && state.entries.len() < VERIFIED_LOCAL_IMAGE_READER_CACHE_ENTRIES
                {
                    state.resident_bytes += plan.total_length;
                    state.entries.insert(
                        key,
                        VerifiedLocalImageReader {
                            reader: Arc::clone(&reader),
                            payload_len: plan.total_length,
                        },
                    );
                }
            }
            reader
        };
        // The LRU lock protects only admission and recency. Each retained
        // reader has its own lock, so independent images can read concurrently.
        drop(state);

        let count = usize::try_from(range.len).map_err(|_| {
            SelectedImageLocalReadError::Storage(
                "selected semantic image page does not fit usize".to_owned(),
            )
        })?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(count).map_err(|_| {
            SelectedImageLocalReadError::Storage(
                "selected semantic image page allocation failed".to_owned(),
            )
        })?;
        payload.resize(count, 0);
        let read = reader
            .lock()
            .map_err(|_| {
                SelectedImageLocalReadError::Storage(
                    "selected image object reader is poisoned".to_owned(),
                )
            })?
            .read_payload_range(range.start, &mut payload)
            .map_err(SelectedImageLocalReadError::from)?;
        if read != count {
            return Err(SelectedImageLocalReadError::Storage(
                "selected semantic image page was truncated".to_owned(),
            ));
        }
        Ok(Some(payload))
    }
}

/// Exact Turso-selected image facts needed for one bounded storage read.
///
/// The object ID remains an owner-side implementation detail and never enters
/// the transport. The selected closure and physical object identity bind a
/// local or S3 read to the same immutable member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SelectedFullImagePlan {
    pub(super) stamp: SelectedGenerationStamp,
    pub(super) image: SemanticPlaneImageKey,
    pub(super) identity: SemanticImageIdentity,
    pub(super) total_length: u64,
    pub(super) object_id: [u8; 32],
    pub(super) closure_id: [u8; 32],
    pub(super) remote_selection: super::s3_publication::RemoteClosureSelection,
    pub(super) environment: [u8; 32],
    pub(super) target_platform: [u8; 32],
}

/// Live selected authority and storage operations used by the page service.
///
/// Production methods resolve the exact committed product selection on every
/// `current_image` call. Tests use an independent source that can flip
/// selection or corrupt its backing image between the pre-read and post-read
/// checks.
pub(super) trait SelectedFullImageAuthority {
    /// Resolves the exact currently selected full-image metadata.
    fn current_image(
        &mut self,
        key: &ProductSemanticPublicationKey,
        image: SemanticPlaneImageKey,
        expected_stamp: SelectedGenerationStamp,
    ) -> Result<SelectedFullImagePlan, SelectedFullImageError>;

    /// Reads one range after verifying closure membership and object identity.
    fn read_image_range(
        &mut self,
        plan: &SelectedFullImagePlan,
        byte_range: ByteRange,
    ) -> Result<Vec<u8>, String>;
}

impl SelectedFullImageAuthority for super::semantic_authority::SemanticAuthority {
    fn current_image(
        &mut self,
        key: &ProductSemanticPublicationKey,
        image: SemanticPlaneImageKey,
        expected_stamp: SelectedGenerationStamp,
    ) -> Result<SelectedFullImagePlan, SelectedFullImageError> {
        self.selected_full_image_plan(key, image, expected_stamp)
    }

    fn read_image_range(
        &mut self,
        plan: &SelectedFullImagePlan,
        byte_range: ByteRange,
    ) -> Result<Vec<u8>, String> {
        self.read_selected_full_image_range(plan, byte_range)
            .map_err(|error| error.0)
    }
}

/// Failure while selecting, reading, or correlating a selected image page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SelectedFullImageError {
    /// The selected head or image tuple changed while the page was read.
    StaleSelection,
    /// The selected image belongs to another runtime environment/platform.
    PlatformOrEnvironmentMismatch,
    /// Image identity or the admitted total length differs from the request.
    ImageMismatch,
    /// The requested byte range is empty, oversized, overflowing, or out of bounds.
    RangeBounds,
    /// Turso selection or immutable storage could not admit the request.
    Authority(String),
    /// Returned bytes do not exactly fill the requested range.
    PayloadLength,
    /// The typed response failed correlation or identity validation.
    Response(String),
}

impl fmt::Display for SelectedFullImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleSelection => formatter.write_str("selected semantic image is stale"),
            Self::PlatformOrEnvironmentMismatch => formatter
                .write_str("selected semantic image targets another environment or platform"),
            Self::ImageMismatch => {
                formatter.write_str("selected semantic image identity or length differs")
            }
            Self::RangeBounds => {
                formatter.write_str("selected semantic image range exceeds bounds")
            }
            Self::Authority(error) => {
                write!(formatter, "selected semantic image authority: {error}")
            }
            Self::PayloadLength => {
                formatter.write_str("selected semantic image range has the wrong payload length")
            }
            Self::Response(error) => write!(formatter, "selected semantic image response: {error}"),
        }
    }
}

/// Serves one bounded range only while the exact requested image remains selected.
pub(super) fn serve_selected_image_get<A: SelectedFullImageAuthority>(
    authority: &mut A,
    key: &ProductSemanticPublicationKey,
    get: &SelectedSemanticImageGet,
    expected_environment: [u8; 32],
    expected_target_platform: [u8; 32],
) -> Result<SelectedSemanticImageChunk, SelectedFullImageError> {
    if get.request_id == 0 {
        return Err(SelectedFullImageError::ImageMismatch);
    }
    let first_page = get.image_identity.is_none() && get.total_length.is_none();
    if get.image_identity.is_some() != get.total_length.is_some()
        || (first_page && (get.byte_range.start != 0 || get.byte_range.len != 1))
    {
        return Err(SelectedFullImageError::ImageMismatch);
    }
    if get.byte_range.len == 0 || get.byte_range.len > MAX_SELECTED_IMAGE_RANGE_BYTES {
        return Err(SelectedFullImageError::RangeBounds);
    }

    let selected = authority.current_image(key, get.image, get.selected_stamp)?;
    validate_selected_plan(
        &selected,
        get,
        expected_environment,
        expected_target_platform,
    )?;
    let end = get
        .byte_range
        .end()
        .map_err(|_| SelectedFullImageError::RangeBounds)?;
    if end > selected.total_length {
        return Err(SelectedFullImageError::RangeBounds);
    }

    let payload = authority
        .read_image_range(&selected, get.byte_range)
        .map_err(SelectedFullImageError::Authority)?;
    if u64::try_from(payload.len()).ok() != Some(get.byte_range.len) {
        return Err(SelectedFullImageError::PayloadLength);
    }

    let still_selected = authority.current_image(key, get.image, selected.stamp)?;
    if still_selected != selected {
        return Err(SelectedFullImageError::StaleSelection);
    }

    let chunk = SelectedSemanticImageChunk {
        request_id: get.request_id,
        target: get.target.clone(),
        selected_stamp: selected.stamp,
        image: selected.image,
        image_identity: selected.identity,
        total_length: selected.total_length,
        byte_range: get.byte_range,
        payload,
    };
    chunk
        .validate_against(get)
        .map_err(|error| SelectedFullImageError::Response(error.to_string()))?;
    Ok(chunk)
}

fn validate_selected_plan(
    selected: &SelectedFullImagePlan,
    get: &SelectedSemanticImageGet,
    expected_environment: [u8; 32],
    expected_target_platform: [u8; 32],
) -> Result<(), SelectedFullImageError> {
    if selected.stamp != get.selected_stamp || selected.image != get.image {
        return Err(SelectedFullImageError::StaleSelection);
    }
    if get.target.profile() != selected.stamp.profile() {
        return Err(SelectedFullImageError::StaleSelection);
    }
    if selected.environment != expected_environment
        || selected.target_platform != expected_target_platform
    {
        return Err(SelectedFullImageError::PlatformOrEnvironmentMismatch);
    }
    if selected.total_length == 0 || selected.total_length > MAX_SEMANTIC_IMAGE_BYTES {
        return Err(SelectedFullImageError::ImageMismatch);
    }
    if get
        .image_identity
        .is_some_and(|identity| identity != selected.identity)
        || get
            .total_length
            .is_some_and(|length| length != selected.total_length)
    {
        return Err(SelectedFullImageError::ImageMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use backend_engine::PackageReference;
    use backend_library::interface::PackageUrl;
    use backend_replication::{SelectedGenerationStamp, SemanticTargetKey};
    use backend_semantic::ir::{
        GenerationId, RustEdition, SemanticImageIdentity, SemanticManifestRoot,
        SemanticPlaneCatalogRoot,
    };
    use backend_semantic::vocabulary::LanguageProfile;
    use backend_store::TypedObject;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_CAS: AtomicU64 = AtomicU64::new(0);

    struct ScratchCas(PathBuf);

    impl ScratchCas {
        fn new() -> Self {
            let sequence = NEXT_CAS.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "backend-selected-image-cas-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create selected-image CAS fixture");
            Self(root)
        }
    }

    impl Drop for ScratchCas {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn put_image_object(
        root: &Path,
        payload: &[u8],
    ) -> (FileStore, SelectedFullImagePlan, PathBuf) {
        use backend_version::{ObjectKey, Schema};

        struct ImageSchema;
        impl Schema for ImageSchema {
            const DOMAIN: u8 = backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA.domain();
            const TYPE: u16 = backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA.ty();
            const VERSION: u8 = backend_extension_turso::COMPILER_SEMANTIC_IMAGE_SCHEMA.version();
            type Value = [u8];

            fn encode(value: &Self::Value, output: &mut Vec<u8>) {
                output.extend_from_slice(value);
            }
        }

        let store = FileStore::open(root, 8 * 1024 * 1024).expect("open selected-image CAS");
        let object_key = ObjectKey::<ImageSchema>::from_value(payload);
        let object = TypedObject::from_value(&object_key, payload);
        let id = store
            .write_object(&object)
            .expect("write selected image envelope");
        let mut plan = plan(payload, 1);
        plan.object_id = *id.as_bytes();
        let filename = id
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let object_path = root.join("objects").join(format!("{filename}.object"));
        (store, plan, object_path)
    }

    struct FixtureAuthority {
        plan: SelectedFullImagePlan,
        plan_cache: SelectedFullImagePlanCache,
        next_plan: Option<SelectedFullImagePlan>,
        bytes: Vec<u8>,
        corrupt: bool,
        reads: usize,
        head_checks: usize,
        metadata_resolves: usize,
    }

    impl SelectedFullImageAuthority for FixtureAuthority {
        fn current_image(
            &mut self,
            _key: &ProductSemanticPublicationKey,
            image: SemanticPlaneImageKey,
            expected_stamp: SelectedGenerationStamp,
        ) -> Result<SelectedFullImagePlan, SelectedFullImageError> {
            self.head_checks += 1;
            let plan = self.plan.clone();
            if plan.stamp != expected_stamp {
                return Err(SelectedFullImageError::StaleSelection);
            }
            let image_is_selected = plan.image == image;
            let metadata_resolves = &mut self.metadata_resolves;
            self.plan_cache.get_or_resolve(plan.stamp, image, || {
                *metadata_resolves += 1;
                if !image_is_selected {
                    return Err(SelectedFullImageError::Authority(
                        "image absent from selected fixture".to_owned(),
                    ));
                }
                Ok(plan)
            })
        }

        fn read_image_range(
            &mut self,
            plan: &SelectedFullImagePlan,
            byte_range: ByteRange,
        ) -> Result<Vec<u8>, String> {
            self.reads += 1;
            if self.corrupt
                || SemanticImageIdentity::from_encoded_bytes(&self.bytes) != plan.identity
                || GenerationId::from_canonical_bytes(&self.bytes)
                    != plan.image.semantic_generation()
            {
                return Err("fixture object failed image identity verification".to_owned());
            }
            let start = usize::try_from(byte_range.start).map_err(|_| "range start")?;
            let end = usize::try_from(byte_range.end().map_err(|_| "range end")?)
                .map_err(|_| "range end")?;
            let payload = self
                .bytes
                .get(start..end)
                .ok_or_else(|| "range outside fixture image".to_owned())?
                .to_vec();
            if let Some(next) = self.next_plan.take() {
                self.plan = next;
            }
            Ok(payload)
        }
    }

    fn target() -> SemanticTargetKey {
        SemanticTargetKey::new(
            "pkg:cargo/full-image-fixture@1.0.0",
            "pkg:cargo/full-image-fixture@1.0.0",
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("valid fixture target")
    }

    fn key() -> ProductSemanticPublicationKey {
        let package = PackageReference::parse("pkg:cargo/full-image-fixture@1.0.0")
            .expect("valid fixture package");
        let coordinate = PackageUrl::parse("pkg:cargo/full-image-fixture@1.0.0")
            .expect("valid fixture coordinate");
        ProductSemanticPublicationKey::new(
            package,
            coordinate,
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("valid fixture product key")
    }

    fn bytes() -> Vec<u8> {
        (0..(MAX_SELECTED_IMAGE_RANGE_BYTES as usize * 3 + 29))
            .map(|index| u8::try_from((index * 37 + index / 13) % 251).expect("byte fits"))
            .collect()
    }

    fn plan(bytes: &[u8], revision: u64) -> SelectedFullImagePlan {
        let identity = SemanticImageIdentity::from_encoded_bytes(bytes);
        let image = SemanticPlaneImageKey::new(
            2,
            GenerationId::from_canonical_bytes(bytes),
            SemanticManifestRoot::from_wire_claim([0x35; 32]),
        );
        SelectedFullImagePlan {
            stamp: SelectedGenerationStamp::checked(
                [0x11; 16],
                target().profile(),
                [0x22; 32],
                revision,
                [0x33; 32],
                [0x44; 32],
                SemanticPlaneCatalogRoot::from_wire_claim([0x55; 32]),
            )
            .expect("valid fixture stamp"),
            image,
            identity,
            total_length: u64::try_from(bytes.len()).expect("fixture length fits"),
            object_id: [0x66; 32],
            closure_id: [0x44; 32],
            remote_selection: super::super::s3_publication::RemoteClosureSelection {
                closure: [0x44; 32],
                target_root: [0x33; 32],
                candidate_id: [0x77; 32],
                namespace_id: [0x11; 16],
                generation: revision,
                attempt_id: [0x88; 16],
                epoch: revision,
                attempt_fence: [0x99; 32],
                input_digest: [0xaa; 32],
            },
            environment: [0xbb; 32],
            target_platform: [0xcc; 32],
        }
    }

    fn get(
        plan: &SelectedFullImagePlan,
        request_id: u64,
        start: u64,
        len: u64,
    ) -> SelectedSemanticImageGet {
        SelectedSemanticImageGet {
            request_id,
            target: target(),
            selected_stamp: plan.stamp,
            image: plan.image,
            image_identity: Some(plan.identity),
            total_length: Some(plan.total_length),
            byte_range: ByteRange::new(start, len).expect("valid fixture range"),
        }
    }

    #[test]
    fn pages_read_nonuniform_ranges_from_one_selected_image() {
        let bytes = bytes();
        let plan = plan(&bytes, 1);
        let mut authority = FixtureAuthority {
            plan: plan.clone(),
            plan_cache: SelectedFullImagePlanCache::default(),
            next_plan: None,
            bytes: bytes.clone(),
            corrupt: false,
            reads: 0,
            head_checks: 0,
            metadata_resolves: 0,
        };
        let ranges = [
            ByteRange::new(0, 1).expect("first byte"),
            ByteRange::new(1, MAX_SELECTED_IMAGE_RANGE_BYTES).expect("full page"),
            ByteRange::new(
                MAX_SELECTED_IMAGE_RANGE_BYTES + 1,
                MAX_SELECTED_IMAGE_RANGE_BYTES - 5,
            )
            .expect("nonuniform page"),
            ByteRange::new(
                MAX_SELECTED_IMAGE_RANGE_BYTES * 2 - 4,
                MAX_SELECTED_IMAGE_RANGE_BYTES,
            )
            .expect("cross-page boundary"),
            ByteRange::new(plan.total_length - 11, 11).expect("tail page"),
        ];
        for (index, range) in ranges.into_iter().enumerate() {
            let mut request = get(
                &plan,
                u64::try_from(index + 1).expect("id fits"),
                range.start,
                range.len,
            );
            if index == 0 {
                request.image_identity = None;
                request.total_length = None;
            }
            let chunk =
                serve_selected_image_get(&mut authority, &key(), &request, [0xbb; 32], [0xcc; 32])
                    .expect("serve selected image page");
            let start = usize::try_from(range.start).expect("start fits");
            let end = usize::try_from(range.end().expect("range end")).expect("end fits");
            assert_eq!(chunk.payload, bytes[start..end]);
            assert_eq!(chunk.total_length, plan.total_length);
            assert_eq!(chunk.image_identity, plan.identity);
        }
        assert_eq!(authority.reads, 5);
        assert_eq!(
            authority.head_checks, 10,
            "each page checks the live head twice"
        );
        assert_eq!(
            authority.metadata_resolves, 1,
            "multi-page reads resolve selected compiler metadata only once"
        );
    }

    #[test]
    fn selection_flip_after_payload_discards_the_page() {
        let bytes = bytes();
        let first = plan(&bytes, 1);
        let next = plan(&bytes, 2);
        let request = get(&first, 41, 9, 23);
        let mut authority = FixtureAuthority {
            plan: first,
            plan_cache: SelectedFullImagePlanCache::default(),
            next_plan: Some(next),
            bytes,
            corrupt: false,
            reads: 0,
            head_checks: 0,
            metadata_resolves: 0,
        };
        assert_eq!(
            serve_selected_image_get(&mut authority, &key(), &request, [0xbb; 32], [0xcc; 32]),
            Err(SelectedFullImageError::StaleSelection)
        );
        assert_eq!(authority.reads, 1);
    }

    #[test]
    fn corrupt_selected_payload_never_becomes_a_chunk() {
        let bytes = bytes();
        let plan = plan(&bytes, 1);
        let request = get(&plan, 42, 3, 17);
        let mut authority = FixtureAuthority {
            plan,
            plan_cache: SelectedFullImagePlanCache::default(),
            next_plan: None,
            bytes,
            corrupt: true,
            reads: 0,
            head_checks: 0,
            metadata_resolves: 0,
        };
        assert!(matches!(
            serve_selected_image_get(&mut authority, &key(), &request, [0xbb; 32], [0xcc; 32]),
            Err(SelectedFullImageError::Authority(_))
        ));
        assert_eq!(authority.reads, 1);
    }

    #[test]
    fn corrupt_local_cas_envelope_uses_the_selected_remote_page() {
        use std::io::Write;

        let scratch = ScratchCas::new();
        let payload = bytes();
        let (store, plan, object_path) = put_image_object(&scratch.0, &payload);
        let mut permissions = fs::metadata(&object_path)
            .expect("read immutable object permissions")
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&object_path, permissions).expect("make fixture writable");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&object_path)
            .expect("open envelope for corruption");
        file.write_all(&[0]).expect("corrupt envelope header");
        file.sync_all().expect("sync corrupt envelope");

        let range = ByteRange::new(19, 23).expect("valid selected range");
        let local = VerifiedLocalImageReaderCache::default().read_range(&store, &plan, range);
        assert!(matches!(
            &local,
            Err(SelectedImageLocalReadError::CorruptEnvelope(_))
        ));
        let expected = payload[19..42].to_vec();
        let mut requested_remote = false;
        let page = read_local_or_remote(local, || {
            requested_remote = true;
            Ok::<_, &'static str>(expected.clone())
        })
        .expect("corrupt local envelope may use selected remote hydration");
        assert!(requested_remote);
        assert_eq!(page, expected);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_local_cas_links_deny_the_remote_fallback() {
        use std::os::unix::fs::symlink;

        let scratch = ScratchCas::new();
        let payload = bytes();
        let (store, plan, object_path) = put_image_object(&scratch.0, &payload);
        let range = ByteRange::new(0, 8).expect("valid selected range");
        let readers = VerifiedLocalImageReaderCache::default();
        let alias = scratch.0.join("objects").join("unsafe-alias.object");
        fs::hard_link(&object_path, &alias).expect("create hostile hard link");
        let local = readers.read_range(&store, &plan, range);
        assert!(matches!(
            local,
            Err(SelectedImageLocalReadError::UnsafePath(_))
        ));
        fs::remove_file(&alias).expect("remove hostile hard link");

        let target = scratch.0.join("objects").join("image-target.object");
        fs::rename(&object_path, &target).expect("move backing object for symlink fixture");
        symlink(&target, &object_path).expect("replace image path with symlink");
        let local = readers.read_range(&store, &plan, range);
        let mut requested_remote = false;
        let result = read_local_or_remote(local, || {
            requested_remote = true;
            Ok::<_, &'static str>(payload[..8].to_vec())
        });
        assert!(matches!(
            result,
            Err(SelectedImagePageError::Local(
                SelectedImageLocalReadError::UnsafePath(_)
            ))
        ));
        assert!(!requested_remote);
    }

    #[cfg(not(unix))]
    #[test]
    fn non_regular_local_cas_path_denies_the_remote_fallback() {
        let scratch = ScratchCas::new();
        let payload = bytes();
        let (store, plan, object_path) = put_image_object(&scratch.0, &payload);
        fs::remove_file(&object_path).expect("remove regular object fixture");
        fs::create_dir(&object_path).expect("replace object path with a directory");

        let local = VerifiedLocalImageReaderCache::default().read_range(
            &store,
            &plan,
            ByteRange::new(0, 8).expect("valid selected range"),
        );
        let mut requested_remote = false;
        let result = read_local_or_remote(local, || {
            requested_remote = true;
            Ok::<_, &'static str>(payload[..8].to_vec())
        });
        assert!(matches!(result, Err(SelectedImagePageError::Local(_))));
        assert!(!requested_remote);
    }

    #[test]
    fn absent_local_image_without_s3_returns_an_error() {
        let result = read_local_or_remote(Ok(None), || {
            Err::<Vec<u8>, _>(
                "selected semantic image is absent or corrupt in local CAS and S3 is unavailable",
            )
        });
        assert!(matches!(
            result,
            Err(SelectedImagePageError::Remote(
                "selected semantic image is absent or corrupt in local CAS and S3 is unavailable"
            ))
        ));
    }

    #[test]
    fn corrupt_local_image_with_stale_remote_receipt_is_denied() {
        let local = Err(SelectedImageLocalReadError::CorruptEnvelope(
            "authenticated envelope failed identity verification".to_owned(),
        ));
        let result = read_local_or_remote(local, || {
            Err::<Vec<u8>, _>("selected S3 receipt does not match the live authority")
        });
        assert!(matches!(
            result,
            Err(SelectedImagePageError::Remote(
                "selected S3 receipt does not match the live authority"
            ))
        ));
    }
}
