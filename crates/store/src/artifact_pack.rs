//! Storage-neutral layout and streaming access for typed object-envelope packs.
//!
//! `ObjectId` identifies a logical typed object. `ArtifactPackId` and
//! `ArtifactLayoutId` identify the physical grouping and exact range map. The
//! pack writer streams into a caller-owned private file; it never retains pack
//! payload bytes in memory. Filesystem and object-store adapters own atomic
//! install, transport checksums, pins, and garbage collection.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    mem::size_of,
};

use crate::{
    ObjectId, RelationAdmissionRegistry, StoreError, UntrustedObjectId, VerifiedObjectEnvelope,
    admit_object_envelope,
};

#[cfg(test)]
use crate::{TypedObject, write_object_envelope};

const MAGIC: &[u8; 8] = b"BACKPK02";
const PAGE_MAGIC: &[u8; 8] = b"BACKPG01";
const VERSION: u16 = 2;
const PAGE_VERSION: u16 = 1;
const HEADER_BYTES: usize = 136;
const PAGE_DESCRIPTOR_BYTES: usize = 160;
const PAGE_HEADER_BYTES: usize = 16;
const EXTENT_ROW_BYTES: usize = 80;
/// Fixed object grouping keeps manifest lookup bounded independently of pack size.
pub const ARTIFACT_PACK_PAGE_OBJECTS: usize = 32;
/// Hard bound for one fetched range-directory page.
pub const MAX_ARTIFACT_PACK_PAGE_BYTES: usize =
    PAGE_HEADER_BYTES + ARTIFACT_PACK_PAGE_OBJECTS * EXTENT_ROW_BYTES;
const LEAF_DOMAIN: &[u8] = b"backend-store.artifact-pack.leaf.v1\0";
const PAGE_LAYOUT_DOMAIN: &[u8] = b"backend-store.artifact-pack.page-layout.v1\0";
const PAGE_OBJECT_ROOT_DOMAIN: &[u8] = b"backend-store.artifact-pack.page-objects.v1\0";
const OBJECT_ROOT_DOMAIN: &[u8] = b"backend-store.artifact-pack.objects.v2\0";
const NODE_DOMAIN: &[u8] = b"backend-store.artifact-pack.node.v1\0";
const EMPTY_DOMAIN: &[u8] = b"backend-store.artifact-pack.empty.v1\0";
const PACK_ID_DOMAIN: &[u8] = b"backend-store.artifact-pack.id.v1\0";
const LAYOUT_ID_DOMAIN: &[u8] = b"backend-store.artifact-pack.layout.v1\0";

/// Suggested target size for immutable locality-affine object packs.
pub const RECOMMENDED_ARTIFACT_PACK_BYTES: u64 = 8 * 1024 * 1024;
/// Hard bound for a single storage-neutral artifact pack. This accommodates
/// one 128 MiB canonical NXFI image while leaving the 8 MiB recommended pack
/// target suitable for ordinary locality-affine objects.
pub const MAX_ARTIFACT_PACK_BYTES: u64 = 160 * 1024 * 1024;
/// Maximum number of complete object envelopes in one pack.
pub const MAX_ARTIFACT_PACK_OBJECTS: usize = 4096;
/// Maximum bounded root-directory prefix size; extent rows live in small pages.
pub const MAX_ARTIFACT_PACK_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
/// Smallest valid root manifest: one fixed root header and one page descriptor.
pub const MIN_ARTIFACT_PACK_MANIFEST_BYTES: usize = HEADER_BYTES + PAGE_DESCRIPTOR_BYTES;

/// Conservative root-plus-page prefix length for an optional coalesced cold
/// read. The result uses the fixed maximum size of every directory page, so it
/// is guaranteed to include the complete authenticated directory when the
/// supplied canonical root length is valid. It may include a small suffix of
/// the first envelope. Return `None` for malformed or out-of-domain lengths.
#[must_use]
pub fn artifact_pack_directory_prefix_upper_bound(root_bytes: u32) -> Option<usize> {
    let root_bytes = usize::try_from(root_bytes).ok()?;
    let directory_descriptors = root_bytes.checked_sub(HEADER_BYTES)?;
    if root_bytes > MAX_ARTIFACT_PACK_MANIFEST_BYTES
        || directory_descriptors % PAGE_DESCRIPTOR_BYTES != 0
    {
        return None;
    }
    let page_count = directory_descriptors / PAGE_DESCRIPTOR_BYTES;
    let maximum_pages = MAX_ARTIFACT_PACK_OBJECTS.div_ceil(ARTIFACT_PACK_PAGE_OBJECTS);
    if page_count == 0 || page_count > maximum_pages {
        return None;
    }
    root_bytes.checked_add(page_count.checked_mul(MAX_ARTIFACT_PACK_PAGE_BYTES)?)
}

/// Physical identity of one logical set of immutable object envelopes.
///
/// This domain is separate from [`ObjectId`] and from the existing map-pack
/// [`crate::PackId`]. Repacking the same sorted object set keeps this ID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactPackId([u8; 32]);

impl ArtifactPackId {
    /// Wraps raw bytes as an unverified physical pack identity claim.
    ///
    /// Callers must compare this claim with a checked manifest before using it
    /// as evidence of stored content.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width physical pack identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Physical identity for one exact object-to-range map.
///
/// A layout can change while logical object IDs remain stable. It is distinct
/// from [`ArtifactPackId`] and [`ObjectId`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactLayoutId([u8; 32]);

impl ArtifactLayoutId {
    /// Wraps raw bytes as an unverified physical layout identity claim.
    ///
    /// Callers must compare this claim with a checked manifest before using it
    /// as evidence of an object-to-range map.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width physical layout identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One complete canonical object-envelope range in an artifact pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPackExtent {
    object_id: [u8; 32],
    offset: u64,
    length: u64,
    leaf_hash: [u8; 32],
}

/// Authenticated root-directory entry for one bounded object-index page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPackPageDescriptor {
    first_object_id: [u8; 32],
    last_object_id: [u8; 32],
    offset: u64,
    bytes: u32,
    object_count: u16,
    data_offset: u64,
    data_end: u64,
    page_hash: [u8; 32],
    object_root: [u8; 32],
}

impl ArtifactPackPageDescriptor {
    /// First sorted logical object ID in this page, used for bounded lookup.
    #[must_use]
    pub const fn first_object_id_bytes(&self) -> &[u8; 32] {
        &self.first_object_id
    }

    /// Last sorted logical object ID in this page.
    #[must_use]
    pub const fn last_object_id_bytes(&self) -> &[u8; 32] {
        &self.last_object_id
    }

    /// Absolute offset of the page bytes in the pack.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Exact encoded page length.
    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }

    /// Number of object rows in this page.
    #[must_use]
    pub const fn object_count(&self) -> u16 {
        self.object_count
    }

    /// First absolute object-envelope offset represented by this page.
    #[must_use]
    pub const fn data_offset(&self) -> u64 {
        self.data_offset
    }

    /// Exclusive end of the final object-envelope range represented here.
    #[must_use]
    pub const fn data_end(&self) -> u64 {
        self.data_end
    }

    /// BLAKE3 commitment to the canonical page bytes, including physical ranges.
    #[must_use]
    pub const fn page_hash(&self) -> &[u8; 32] {
        &self.page_hash
    }

    /// Content commitment to the sorted object leaves represented by this page.
    #[must_use]
    pub const fn object_root(&self) -> &[u8; 32] {
        &self.object_root
    }
}

impl ArtifactPackExtent {
    /// The untrusted ID claim stored in this physical directory row.
    #[must_use]
    pub const fn object_id_bytes(&self) -> &[u8; 32] {
        &self.object_id
    }

    /// Absolute offset of the complete envelope in the pack.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Exact complete-envelope length.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Number of sibling hashes carried separately for this extent.
    ///
    /// The bounded directory page carries every page leaf, so verification
    /// recomputes the page root and needs no separate siblings.
    #[must_use]
    pub fn proof_depth(&self) -> usize {
        0
    }
}

/// One checked page of object-to-range entries from an artifact-pack directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPackPage {
    page_index: usize,
    pack_id: ArtifactPackId,
    layout_id: ArtifactLayoutId,
    extents: Vec<ArtifactPackExtent>,
}

impl ArtifactPackPage {
    /// Zero-based directory page index.
    #[must_use]
    pub const fn page_index(&self) -> usize {
        self.page_index
    }

    /// Sorted object rows authenticated by this page.
    #[must_use]
    pub fn extents(&self) -> &[ArtifactPackExtent] {
        &self.extents
    }

    /// Finds one logical object ID within this bounded page.
    #[must_use]
    pub fn extent(&self, id: ObjectId) -> Option<(usize, &ArtifactPackExtent)> {
        self.extents
            .binary_search_by(|extent| extent.object_id.as_slice().cmp(id.as_bytes()))
            .ok()
            .map(|index| (index, &self.extents[index]))
    }

    /// Finds one logical object claim within this checked page. The returned
    /// row remains untrusted until its complete envelope is admitted.
    #[must_use]
    pub fn extent_claim(&self, id: UntrustedObjectId) -> Option<(usize, &ArtifactPackExtent)> {
        self.extents
            .binary_search_by(|extent| extent.object_id.as_slice().cmp(id.as_bytes()))
            .ok()
            .map(|index| (index, &self.extents[index]))
    }
}

/// Checked canonical mapping from logical object IDs to physical pack ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPackManifest {
    pack_id: ArtifactPackId,
    layout_id: ArtifactLayoutId,
    pack_bytes: u64,
    manifest_bytes: u32,
    page_root: [u8; 32],
    object_count: usize,
    data_start: u64,
    pages: Vec<ArtifactPackPageDescriptor>,
    // Present on a newly built manifest for closure membership inspection.
    // A cold root parse deliberately leaves this empty and loads one page on demand.
    extents: Vec<ArtifactPackExtent>,
}

impl ArtifactPackManifest {
    /// Physical pack identity committed by this checked manifest.
    #[must_use]
    pub const fn pack_id(&self) -> ArtifactPackId {
        self.pack_id
    }

    /// Exact physical range-layout identity committed by this manifest.
    #[must_use]
    pub const fn layout_id(&self) -> ArtifactLayoutId {
        self.layout_id
    }

    /// Complete packed file size including the manifest prefix.
    #[must_use]
    pub const fn pack_bytes(&self) -> u64 {
        self.pack_bytes
    }

    /// Exact canonical manifest prefix size.
    #[must_use]
    pub const fn manifest_bytes(&self) -> u32 {
        self.manifest_bytes
    }

    /// Content root over the ordered page roots and complete object leaves.
    #[must_use]
    pub const fn page_root(&self) -> &[u8; 32] {
        &self.page_root
    }

    /// Number of logical objects represented by the layout.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.object_count
    }

    /// Borrows the canonical sorted extents when this manifest was built
    /// locally. A cold root parse returns an empty slice and resolves entries
    /// only after `verify_page` checks the corresponding bounded page.
    #[must_use]
    pub fn extents(&self) -> &[ArtifactPackExtent] {
        &self.extents
    }

    /// Bounded root-directory entries; each one identifies one small range page.
    #[must_use]
    pub fn pages(&self) -> &[ArtifactPackPageDescriptor] {
        &self.pages
    }

    /// Selects the one page that could contain `id` using sorted first IDs.
    #[must_use]
    pub fn page_for_object(&self, id: ObjectId) -> Option<(usize, &ArtifactPackPageDescriptor)> {
        self.page_for_object_claim(UntrustedObjectId::from_bytes(*id.as_bytes()))
    }

    /// Selects the one page that could contain a raw object-ID claim. This
    /// method only narrows the directory search; it does not admit the ID.
    #[must_use]
    pub fn page_for_object_claim(
        &self,
        id: UntrustedObjectId,
    ) -> Option<(usize, &ArtifactPackPageDescriptor)> {
        let insertion = self
            .pages
            .partition_point(|page| page.first_object_id.as_slice() <= id.as_bytes());
        let index = insertion.checked_sub(1)?;
        self.pages.get(index).map(|page| (index, page))
    }

    /// Returns a range from a locally built manifest with materialized rows.
    /// Cold root parses do not have rows; use `page_for_object` and
    /// `verify_page` before resolving an extent.
    #[must_use]
    #[cfg(test)]
    fn extent(&self, id: ObjectId) -> Option<(usize, &ArtifactPackExtent)> {
        self.extents
            .binary_search_by(|extent| extent.object_id.as_slice().cmp(id.as_bytes()))
            .ok()
            .map(|index| (index, &self.extents[index]))
    }

    /// Checks an exact directory-page range before exposing its member rows.
    pub fn verify_page(
        &self,
        page_index: usize,
        bytes: &[u8],
    ) -> Result<ArtifactPackPage, StoreError> {
        let descriptor = self.pages.get(page_index).ok_or(StoreError::Corrupt)?;
        if bytes.len() != usize::try_from(descriptor.bytes).map_err(|_| StoreError::Bounds)?
            || page_layout_hash(bytes) != descriptor.page_hash
        {
            return Err(StoreError::Corrupt);
        }
        let mut cursor = Cursor::new(bytes);
        if cursor.take(PAGE_MAGIC.len())? != PAGE_MAGIC
            || cursor.u16()? != PAGE_VERSION
            || usize::from(cursor.u16()?) != page_index
            || usize::from(cursor.u16()?) != usize::from(descriptor.object_count)
            || cursor.u16()? != 0
        {
            return Err(StoreError::Corrupt);
        }
        let mut extents = Vec::with_capacity(usize::from(descriptor.object_count));
        for _ in 0..usize::from(descriptor.object_count) {
            extents.push(ArtifactPackExtent {
                object_id: cursor.array32()?,
                offset: cursor.u64()?,
                length: cursor.u64()?,
                leaf_hash: cursor.array32()?,
            });
        }
        if !cursor.is_empty()
            || extents.is_empty()
            || extents[0].object_id != descriptor.first_object_id
            || extents.last().map(|extent| extent.object_id) != Some(descriptor.last_object_id)
            || extents
                .iter()
                .any(|extent| extent.object_id == [0; 32] || extent.length == 0)
            || extents
                .windows(2)
                .any(|pair| pair[0].object_id >= pair[1].object_id)
        {
            return Err(StoreError::Corrupt);
        }
        let mut expected_offset = descriptor.data_offset;
        for extent in &extents {
            if extent.offset != expected_offset {
                return Err(StoreError::Corrupt);
            }
            expected_offset = extent
                .offset
                .checked_add(extent.length)
                .ok_or(StoreError::Bounds)?;
        }
        if expected_offset != descriptor.data_end
            || page_objects_root(&extents)? != descriptor.object_root
        {
            return Err(StoreError::Corrupt);
        }
        Ok(ArtifactPackPage {
            page_index,
            pack_id: self.pack_id,
            layout_id: self.layout_id,
            extents,
        })
    }

    /// Encodes the bounded canonical manifest prefix.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut output =
            Vec::with_capacity(usize::try_from(self.manifest_bytes).unwrap_or(usize::MAX));
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&VERSION.to_be_bytes());
        output.extend_from_slice(
            &u16::try_from(HEADER_BYTES)
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        output.extend_from_slice(&self.manifest_bytes.to_be_bytes());
        output.extend_from_slice(&count_u32(self.object_count).to_be_bytes());
        output.extend_from_slice(&count_u32(self.pages.len()).to_be_bytes());
        output.extend_from_slice(&self.pack_bytes.to_be_bytes());
        output.extend_from_slice(&self.data_start.to_be_bytes());
        output.extend_from_slice(&self.page_root);
        output.extend_from_slice(&self.layout_id.0);
        output.extend_from_slice(&self.pack_id.0);
        for page in &self.pages {
            encode_page_descriptor(page, &mut output);
        }
        output
    }

    /// Parses and verifies a bounded manifest prefix against the actual pack
    /// size. The result proves canonical structure and internal ID consistency;
    /// it grants no authority to publish or select this pack.
    pub fn parse(bytes: &[u8], actual_pack_bytes: u64) -> Result<Self, StoreError> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_ARTIFACT_PACK_MANIFEST_BYTES {
            return Err(StoreError::Bounds);
        }
        let mut cursor = Cursor::new(bytes);
        if cursor.take(MAGIC.len())? != MAGIC {
            return Err(StoreError::Corrupt);
        }
        if cursor.u16()? != VERSION || usize::from(cursor.u16()?) != HEADER_BYTES {
            return Err(StoreError::Corrupt);
        }
        let manifest_bytes = cursor.u32()?;
        let count = usize::try_from(cursor.u32()?).map_err(|_| StoreError::Bounds)?;
        let page_count = usize::try_from(cursor.u32()?).map_err(|_| StoreError::Bounds)?;
        let pack_bytes = cursor.u64()?;
        let data_start = cursor.u64()?;
        let page_root = cursor.array32()?;
        let layout_id = ArtifactLayoutId(cursor.array32()?);
        let pack_id = ArtifactPackId(cursor.array32()?);
        let expected_page_count = page_count_for_objects(count);
        let expected_manifest_bytes = root_manifest_len(expected_page_count)?;
        if count == 0
            || count > MAX_ARTIFACT_PACK_OBJECTS
            || page_count != expected_page_count
            || actual_pack_bytes == 0
            || actual_pack_bytes > MAX_ARTIFACT_PACK_BYTES
            || pack_bytes != actual_pack_bytes
            || usize::try_from(manifest_bytes).ok() != Some(bytes.len())
            || bytes.len() != expected_manifest_bytes
        {
            return Err(StoreError::Corrupt);
        }
        let mut pages = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            pages.push(decode_page_descriptor(&mut cursor)?);
        }
        if !cursor.is_empty() {
            return Err(StoreError::Corrupt);
        }
        let mut expected_page_offset = u64::from(manifest_bytes);
        let mut expected_data_offset = data_start;
        let mut expected_object_root = blake3::Hasher::new();
        expected_object_root.update(OBJECT_ROOT_DOMAIN);
        expected_object_root.update(&count_u32(count).to_be_bytes());
        expected_object_root.update(&count_u32(page_count).to_be_bytes());
        for (page_index, page) in pages.iter().enumerate() {
            let expected_objects = page_object_count(count, page_index);
            let expected_page_bytes = page_encoded_len(expected_objects)?;
            if page.first_object_id == [0; 32]
                || page.last_object_id < page.first_object_id
                || page.object_count as usize != expected_objects
                || page.offset != expected_page_offset
                || usize::try_from(page.bytes).ok() != Some(expected_page_bytes)
                || page.data_offset != expected_data_offset
                || page.data_end <= page.data_offset
                || page.data_end > pack_bytes
                || pages
                    .get(page_index + 1)
                    .is_some_and(|next| page.last_object_id >= next.first_object_id)
            {
                return Err(StoreError::Corrupt);
            }
            expected_page_offset = expected_page_offset
                .checked_add(u64::from(page.bytes))
                .ok_or(StoreError::Bounds)?;
            expected_data_offset = page.data_end;
            expected_object_root.update(&page.object_root);
        }
        if expected_page_offset != data_start
            || expected_data_offset != pack_bytes
            || *expected_object_root.finalize().as_bytes() != page_root
        {
            return Err(StoreError::Corrupt);
        }
        let manifest = Self {
            pack_id,
            layout_id,
            pack_bytes,
            manifest_bytes,
            page_root,
            object_count: count,
            data_start,
            pages,
            extents: Vec::new(),
        };
        if compute_pack_id(&manifest) != pack_id
            || compute_layout_id(&manifest) != layout_id
            || manifest.encode().as_slice() != bytes
        {
            return Err(StoreError::Corrupt);
        }
        Ok(manifest)
    }

    /// Verifies one complete envelope extent and its typed backend-store
    /// identity before returning checked metadata. `input` must begin at the
    /// first byte of the exact range; bytes outside that range are never read.
    #[cfg(test)]
    fn verify_object_envelope<R: Read>(
        &self,
        id: ObjectId,
        input: &mut R,
        registry: &RelationAdmissionRegistry,
    ) -> Result<VerifiedArtifactPackObject, StoreError> {
        let (index, extent) = self.extent(id).ok_or(StoreError::Corrupt)?;
        self.verify_extent_envelope(index, extent, input, registry)
    }

    /// Verifies one complete envelope using a previously checked directory page.
    pub fn verify_object_envelope_in_page<R: Read>(
        &self,
        page: &ArtifactPackPage,
        id: ObjectId,
        input: &mut R,
        registry: &RelationAdmissionRegistry,
    ) -> Result<VerifiedArtifactPackObject, StoreError> {
        self.verify_object_envelope_in_page_claim(
            page,
            UntrustedObjectId::from_bytes(*id.as_bytes()),
            input,
            registry,
        )
    }

    /// Verifies one complete envelope selected by an untrusted page-member
    /// claim. The returned checked envelope carries the admitted `ObjectId`.
    pub fn verify_object_envelope_in_page_claim<R: Read>(
        &self,
        page: &ArtifactPackPage,
        id: UntrustedObjectId,
        input: &mut R,
        registry: &RelationAdmissionRegistry,
    ) -> Result<VerifiedArtifactPackObject, StoreError> {
        if page.pack_id != self.pack_id || page.layout_id != self.layout_id {
            return Err(StoreError::Corrupt);
        }
        let (local_index, extent) = page.extent_claim(id).ok_or(StoreError::Corrupt)?;
        let index = page
            .page_index
            .checked_mul(ARTIFACT_PACK_PAGE_OBJECTS)
            .and_then(|base| base.checked_add(local_index))
            .ok_or(StoreError::Bounds)?;
        self.verify_extent_envelope(index, extent, input, registry)
    }

    fn verify_extent_envelope<R: Read>(
        &self,
        index: usize,
        extent: &ArtifactPackExtent,
        input: &mut R,
        registry: &RelationAdmissionRegistry,
    ) -> Result<VerifiedArtifactPackObject, StoreError> {
        let id = ObjectId::from_bytes(extent.object_id);
        let bounded = input.take(extent.length);
        let mut hashing = LeafReader::new(bounded, id, index, extent.length);
        let verified = admit_object_envelope(
            &mut hashing,
            usize::try_from(extent.length).map_err(|_| StoreError::Bounds)?,
            registry,
            UntrustedObjectId::from_bytes(extent.object_id),
        )?;
        if verified.id() != id || hashing.bytes_read != extent.length {
            return Err(StoreError::Corrupt);
        }
        let leaf_hash = hashing.finish();
        if leaf_hash != extent.leaf_hash {
            return Err(StoreError::Corrupt);
        }
        Ok(VerifiedArtifactPackObject {
            envelope: verified,
            pack_id: self.pack_id,
            layout_id: self.layout_id,
            offset: extent.offset,
            length: extent.length,
        })
    }

    /// Approximate bounded metadata retained by the parsed directory.
    #[must_use]
    pub fn retained_metadata_bytes(&self) -> usize {
        self.extents
            .capacity()
            .saturating_mul(size_of::<ArtifactPackExtent>())
            .saturating_add(
                self.pages
                    .capacity()
                    .saturating_mul(size_of::<ArtifactPackPageDescriptor>()),
            )
    }
}

/// File-backed deterministic writer for one bounded artifact pack.
///
/// The caller owns creation, atomic installation, and cleanup of `output`.
/// The writer reserves the bounded manifest prefix, then streams each complete
/// envelope directly into the file. It retains only bounded extents and
/// Merkle metadata, never the pack payload.
pub struct ArtifactPackWriter {
    output: File,
    expected_count: usize,
    maximum_bytes: u64,
    manifest_bytes: u32,
    page_count: usize,
    data_start: u64,
    written_bytes: u64,
    last_id: Option<[u8; 32]>,
    extents: Vec<ArtifactPackExtent>,
    registry: RelationAdmissionRegistry,
    failed: bool,
}

impl ArtifactPackWriter {
    /// Starts writing a new pack into a caller-owned private file.
    pub fn new(
        output: File,
        expected_count: usize,
        maximum_bytes: u64,
    ) -> Result<Self, StoreError> {
        Self::new_with_registry(
            output,
            expected_count,
            maximum_bytes,
            RelationAdmissionRegistry::default(),
        )
    }

    /// Starts a pack writer with the relation grammars admitted by this store.
    pub fn new_with_registry(
        mut output: File,
        expected_count: usize,
        maximum_bytes: u64,
        registry: RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        if expected_count == 0
            || expected_count > MAX_ARTIFACT_PACK_OBJECTS
            || maximum_bytes == 0
            || maximum_bytes > MAX_ARTIFACT_PACK_BYTES
        {
            return Err(StoreError::Bounds);
        }
        let page_count = page_count_for_objects(expected_count);
        let manifest_size = root_manifest_len(page_count)?;
        let manifest_bytes = u32::try_from(manifest_size).map_err(|_| StoreError::Bounds)?;
        let directory_bytes = (0..page_count).try_fold(0_u64, |total, page_index| {
            total
                .checked_add(
                    u64::try_from(page_encoded_len(page_object_count(
                        expected_count,
                        page_index,
                    ))?)
                    .map_err(|_| StoreError::Bounds)?,
                )
                .ok_or(StoreError::Bounds)
        })?;
        let data_start = u64::from(manifest_bytes)
            .checked_add(directory_bytes)
            .ok_or(StoreError::Bounds)?;
        if data_start >= maximum_bytes {
            return Err(StoreError::Bounds);
        }
        output.set_len(0).map_err(io_error)?;
        output.seek(SeekFrom::Start(data_start)).map_err(io_error)?;
        Ok(Self {
            output,
            expected_count,
            maximum_bytes,
            manifest_bytes,
            page_count,
            data_start,
            written_bytes: 0,
            last_id: None,
            extents: Vec::with_capacity(expected_count),
            registry,
            failed: false,
        })
    }

    /// Streams one complete typed envelope. Calls must be strictly ordered by
    /// `ObjectId`; the callback writes directly to the bounded file-backed
    /// sink and returns the number of bytes it emitted.
    pub fn push_with<F>(&mut self, id: ObjectId, write_envelope: F) -> Result<(), StoreError>
    where
        F: FnOnce(&mut dyn Write) -> Result<u64, StoreError>,
    {
        if self.failed || self.extents.len() >= self.expected_count {
            return Err(StoreError::Bounds);
        }
        let object_id = *id.as_bytes();
        if object_id == [0; 32] || self.last_id.is_some_and(|last| last >= object_id) {
            self.failed = true;
            return Err(StoreError::Corrupt);
        }
        let index = self.extents.len();
        let offset = self
            .data_start
            .checked_add(self.written_bytes)
            .ok_or(StoreError::Bounds)?;
        let maximum_object_bytes = self
            .maximum_bytes
            .checked_sub(offset)
            .ok_or(StoreError::Bounds)?;
        let mut sink = PackObjectSink::new(&mut self.output, maximum_object_bytes);
        let reported = match write_envelope(&mut sink) {
            Ok(reported) => reported,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        let (length, content_digest) = sink.finish();
        if length == 0 || reported != length {
            self.failed = true;
            return Err(StoreError::Corrupt);
        }
        let end = offset.checked_add(length).ok_or(StoreError::Bounds)?;
        if end > self.maximum_bytes {
            self.failed = true;
            return Err(StoreError::Bounds);
        }
        self.failed = true;
        self.output.flush().map_err(io_error)?;
        self.output
            .seek(SeekFrom::Start(offset))
            .map_err(io_error)?;
        let verified = {
            let mut bounded = (&mut self.output).take(length);
            match admit_object_envelope(
                &mut bounded,
                usize::try_from(length).map_err(|_| StoreError::Bounds)?,
                &self.registry,
                UntrustedObjectId::from_bytes(object_id),
            ) {
                Ok(verified) => verified,
                Err(error) => return Err(error),
            }
        };
        if verified.id() != id {
            self.failed = true;
            return Err(StoreError::Corrupt);
        }
        self.output.seek(SeekFrom::Start(end)).map_err(io_error)?;
        let leaf_hash = leaf_hash(index, object_id, length, &content_digest);
        self.extents.push(ArtifactPackExtent {
            object_id,
            offset,
            length,
            leaf_hash,
        });
        self.last_id = Some(object_id);
        self.written_bytes = self
            .written_bytes
            .checked_add(length)
            .ok_or(StoreError::Bounds)?;
        self.failed = false;
        Ok(())
    }

    /// Finalizes the proof directory and writes its bounded canonical prefix.
    pub fn finish(mut self) -> Result<(File, ArtifactPackManifest), StoreError> {
        if self.failed || self.extents.len() != self.expected_count {
            return Err(StoreError::Corrupt);
        }
        let pack_bytes = self
            .data_start
            .checked_add(self.written_bytes)
            .ok_or(StoreError::Bounds)?;
        if pack_bytes > self.maximum_bytes {
            return Err(StoreError::Bounds);
        }
        let mut pages = Vec::with_capacity(self.page_count);
        let mut page_offset = u64::from(self.manifest_bytes);
        for page_index in 0..self.page_count {
            let start = page_index
                .checked_mul(ARTIFACT_PACK_PAGE_OBJECTS)
                .ok_or(StoreError::Bounds)?;
            let end = start
                .checked_add(ARTIFACT_PACK_PAGE_OBJECTS)
                .unwrap_or(usize::MAX)
                .min(self.extents.len());
            let extents = self.extents.get(start..end).ok_or(StoreError::Corrupt)?;
            let encoded = encode_page(page_index, extents)?;
            let data_offset = extents.first().ok_or(StoreError::Corrupt)?.offset;
            let final_extent = extents.last().ok_or(StoreError::Corrupt)?;
            let data_end = final_extent
                .offset
                .checked_add(final_extent.length)
                .ok_or(StoreError::Bounds)?;
            let descriptor = ArtifactPackPageDescriptor {
                first_object_id: extents[0].object_id,
                last_object_id: final_extent.object_id,
                offset: page_offset,
                bytes: u32::try_from(encoded.len()).map_err(|_| StoreError::Bounds)?,
                object_count: u16::try_from(extents.len()).map_err(|_| StoreError::Bounds)?,
                data_offset,
                data_end,
                page_hash: page_layout_hash(&encoded),
                object_root: page_objects_root(extents)?,
            };
            self.output
                .seek(SeekFrom::Start(page_offset))
                .map_err(io_error)?;
            self.output.write_all(&encoded).map_err(io_error)?;
            page_offset = page_offset
                .checked_add(u64::from(descriptor.bytes))
                .ok_or(StoreError::Bounds)?;
            pages.push(descriptor);
        }
        if page_offset != self.data_start {
            return Err(StoreError::Corrupt);
        }
        let page_root = compute_page_set_root(self.expected_count, &pages);
        let mut manifest = ArtifactPackManifest {
            pack_id: ArtifactPackId([0; 32]),
            layout_id: ArtifactLayoutId([0; 32]),
            pack_bytes,
            manifest_bytes: self.manifest_bytes,
            page_root,
            object_count: self.expected_count,
            data_start: self.data_start,
            pages,
            extents: self.extents,
        };
        manifest.pack_id = compute_pack_id(&manifest);
        manifest.layout_id = compute_layout_id(&manifest);
        let encoded = manifest.encode();
        if encoded.len() != usize::try_from(self.manifest_bytes).unwrap_or(usize::MAX) {
            return Err(StoreError::Corrupt);
        }
        self.output.seek(SeekFrom::Start(0)).map_err(io_error)?;
        self.output.write_all(&encoded).map_err(io_error)?;
        self.output.flush().map_err(io_error)?;
        let actual_bytes = self.output.metadata().map_err(io_error)?.len();
        if actual_bytes != pack_bytes {
            return Err(StoreError::Corrupt);
        }
        Ok((self.output, manifest))
    }
}

struct PackObjectSink<'a> {
    output: &'a mut File,
    maximum_bytes: u64,
    written: u64,
    hasher: blake3::Hasher,
}

impl<'a> PackObjectSink<'a> {
    fn new(output: &'a mut File, maximum_bytes: u64) -> Self {
        Self {
            output,
            maximum_bytes,
            written: 0,
            hasher: blake3::Hasher::new(),
        }
    }

    fn finish(self) -> (u64, [u8; 32]) {
        (self.written, *self.hasher.finalize().as_bytes())
    }
}

impl Write for PackObjectSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = self
            .maximum_bytes
            .checked_sub(self.written)
            .ok_or_else(|| io::Error::other("artifact pack byte limit exceeded"))?;
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "artifact pack byte limit exceeded",
            ));
        }
        let count = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let written = self.output.write(&bytes[..count])?;
        self.hasher.update(&bytes[..written]);
        self.written = self
            .written
            .checked_add(u64::try_from(written).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("artifact pack byte count overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

/// Typed checks for one verified complete envelope extent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedArtifactPackObject {
    envelope: VerifiedObjectEnvelope,
    pack_id: ArtifactPackId,
    layout_id: ArtifactLayoutId,
    offset: u64,
    length: u64,
}

impl VerifiedArtifactPackObject {
    /// Checked logical typed object identity.
    #[must_use]
    pub const fn id(self) -> ObjectId {
        self.envelope.id()
    }

    /// Physical pack identity from the checked manifest.
    #[must_use]
    pub const fn pack_id(self) -> ArtifactPackId {
        self.pack_id
    }

    /// Exact physical range layout identity.
    #[must_use]
    pub const fn layout_id(self) -> ArtifactLayoutId {
        self.layout_id
    }

    /// Absolute start of the verified complete envelope in the pack.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Exact byte length of the verified complete envelope.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// The store-verified envelope facts.
    #[must_use]
    pub const fn envelope(self) -> VerifiedObjectEnvelope {
        self.envelope
    }
}

struct LeafReader<R> {
    input: R,
    hasher: blake3::Hasher,
    bytes_read: u64,
    id: ObjectId,
    index: usize,
    length: u64,
}

impl<R: Read> LeafReader<R> {
    fn new(input: R, id: ObjectId, index: usize, length: u64) -> Self {
        Self {
            input,
            hasher: blake3::Hasher::new(),
            bytes_read: 0,
            id,
            index,
            length,
        }
    }

    fn finish(self) -> [u8; 32] {
        leaf_hash(
            self.index,
            *self.id.as_bytes(),
            self.length,
            self.hasher.finalize().as_bytes(),
        )
    }
}

impl<R: Read> Read for LeafReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(output)?;
        if count != 0 {
            self.hasher.update(&output[..count]);
            self.bytes_read = self
                .bytes_read
                .checked_add(u64::try_from(count).map_err(io::Error::other)?)
                .ok_or_else(|| io::Error::other("artifact pack read count overflow"))?;
        }
        Ok(count)
    }
}

fn root_manifest_len(page_count: usize) -> Result<usize, StoreError> {
    let bytes = HEADER_BYTES
        .checked_add(
            page_count
                .checked_mul(PAGE_DESCRIPTOR_BYTES)
                .ok_or(StoreError::Bounds)?,
        )
        .ok_or(StoreError::Bounds)?;
    if bytes > MAX_ARTIFACT_PACK_MANIFEST_BYTES {
        return Err(StoreError::Bounds);
    }
    Ok(bytes)
}

fn page_count_for_objects(object_count: usize) -> usize {
    object_count.div_ceil(ARTIFACT_PACK_PAGE_OBJECTS)
}

fn page_object_count(object_count: usize, page_index: usize) -> usize {
    let start = page_index.saturating_mul(ARTIFACT_PACK_PAGE_OBJECTS);
    object_count
        .saturating_sub(start)
        .min(ARTIFACT_PACK_PAGE_OBJECTS)
}

fn page_encoded_len(object_count: usize) -> Result<usize, StoreError> {
    if object_count == 0 || object_count > ARTIFACT_PACK_PAGE_OBJECTS {
        return Err(StoreError::Bounds);
    }
    PAGE_HEADER_BYTES
        .checked_add(
            object_count
                .checked_mul(EXTENT_ROW_BYTES)
                .ok_or(StoreError::Bounds)?,
        )
        .ok_or(StoreError::Bounds)
}

fn encode_page(page_index: usize, extents: &[ArtifactPackExtent]) -> Result<Vec<u8>, StoreError> {
    let page_len = page_encoded_len(extents.len())?;
    let mut output = Vec::with_capacity(page_len);
    output.extend_from_slice(PAGE_MAGIC);
    output.extend_from_slice(&PAGE_VERSION.to_be_bytes());
    output.extend_from_slice(
        &u16::try_from(page_index)
            .map_err(|_| StoreError::Bounds)?
            .to_be_bytes(),
    );
    output.extend_from_slice(
        &u16::try_from(extents.len())
            .map_err(|_| StoreError::Bounds)?
            .to_be_bytes(),
    );
    output.extend_from_slice(&0_u16.to_be_bytes());
    for extent in extents {
        output.extend_from_slice(&extent.object_id);
        output.extend_from_slice(&extent.offset.to_be_bytes());
        output.extend_from_slice(&extent.length.to_be_bytes());
        output.extend_from_slice(&extent.leaf_hash);
    }
    if output.len() != page_len {
        return Err(StoreError::Corrupt);
    }
    Ok(output)
}

fn encode_page_descriptor(page: &ArtifactPackPageDescriptor, output: &mut Vec<u8>) {
    output.extend_from_slice(&page.first_object_id);
    output.extend_from_slice(&page.last_object_id);
    output.extend_from_slice(&page.offset.to_be_bytes());
    output.extend_from_slice(&page.bytes.to_be_bytes());
    output.extend_from_slice(&page.object_count.to_be_bytes());
    output.extend_from_slice(&0_u16.to_be_bytes());
    output.extend_from_slice(&page.data_offset.to_be_bytes());
    output.extend_from_slice(&page.data_end.to_be_bytes());
    output.extend_from_slice(&page.page_hash);
    output.extend_from_slice(&page.object_root);
}

fn decode_page_descriptor(
    cursor: &mut Cursor<'_>,
) -> Result<ArtifactPackPageDescriptor, StoreError> {
    let first_object_id = cursor.array32()?;
    let last_object_id = cursor.array32()?;
    let offset = cursor.u64()?;
    let bytes = cursor.u32()?;
    let object_count = cursor.u16()?;
    if cursor.u16()? != 0 {
        return Err(StoreError::Corrupt);
    }
    Ok(ArtifactPackPageDescriptor {
        first_object_id,
        last_object_id,
        offset,
        bytes,
        object_count,
        data_offset: cursor.u64()?,
        data_end: cursor.u64()?,
        page_hash: cursor.array32()?,
        object_root: cursor.array32()?,
    })
}

fn page_layout_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PAGE_LAYOUT_DOMAIN);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn page_objects_root(extents: &[ArtifactPackExtent]) -> Result<[u8; 32], StoreError> {
    let leaves = extents
        .iter()
        .map(|extent| extent.leaf_hash)
        .collect::<Vec<_>>();
    merkle_root(&leaves)
}

fn compute_page_set_root(object_count: usize, pages: &[ArtifactPackPageDescriptor]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(OBJECT_ROOT_DOMAIN);
    hasher.update(&count_u32(object_count).to_be_bytes());
    hasher.update(&count_u32(pages.len()).to_be_bytes());
    for page in pages {
        hasher.update(&page.object_root);
    }
    *hasher.finalize().as_bytes()
}

fn leaf_hash(
    index: usize,
    object_id: [u8; 32],
    length: u64,
    content_digest: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LEAF_DOMAIN);
    hasher.update(&u64::try_from(index).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(&object_id);
    hasher.update(&length.to_be_bytes());
    hasher.update(content_digest);
    *hasher.finalize().as_bytes()
}

fn compute_pack_id(manifest: &ArtifactPackManifest) -> ArtifactPackId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PACK_ID_DOMAIN);
    hasher.update(&count_u32(manifest.object_count).to_be_bytes());
    hasher.update(&manifest.page_root);
    ArtifactPackId(*hasher.finalize().as_bytes())
}

fn compute_layout_id(manifest: &ArtifactPackManifest) -> ArtifactLayoutId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LAYOUT_ID_DOMAIN);
    hasher.update(&manifest.manifest_bytes.to_be_bytes());
    hasher.update(&manifest.pack_bytes.to_be_bytes());
    hasher.update(&manifest.data_start.to_be_bytes());
    hasher.update(&count_u32(manifest.object_count).to_be_bytes());
    hasher.update(&count_u32(manifest.pages.len()).to_be_bytes());
    hasher.update(&manifest.page_root);
    for page in &manifest.pages {
        hasher.update(&page.first_object_id);
        hasher.update(&page.last_object_id);
        hasher.update(&page.offset.to_be_bytes());
        hasher.update(&page.bytes.to_be_bytes());
        hasher.update(&page.object_count.to_be_bytes());
        hasher.update(&page.data_offset.to_be_bytes());
        hasher.update(&page.data_end.to_be_bytes());
        hasher.update(&page.page_hash);
        hasher.update(&page.object_root);
    }
    ArtifactLayoutId(*hasher.finalize().as_bytes())
}

fn merkle_root(leaves: &[[u8; 32]]) -> Result<[u8; 32], StoreError> {
    if leaves.is_empty() || leaves.len() > ARTIFACT_PACK_PAGE_OBJECTS {
        return Err(StoreError::Bounds);
    }
    let count = leaves.len();
    let padded = count.next_power_of_two();
    let mut level = Vec::with_capacity(padded);
    level.extend_from_slice(leaves);
    for index in count..padded {
        level.push(empty_hash(count, index));
    }
    while level.len() > 1 {
        let mut parent = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks_exact(2) {
            parent.push(node_hash(&pair[0], &pair[1]));
        }
        level = parent;
    }
    let root = level.first().copied().ok_or(StoreError::Corrupt)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(PAGE_OBJECT_ROOT_DOMAIN);
    hasher.update(&count_u32(count).to_be_bytes());
    hasher.update(&root);
    Ok(*hasher.finalize().as_bytes())
}

fn empty_hash(count: usize, index: usize) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EMPTY_DOMAIN);
    hasher.update(&count_u64(count).to_be_bytes());
    hasher.update(&count_u64(index).to_be_bytes());
    *hasher.finalize().as_bytes()
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

fn io_error(error: io::Error) -> StoreError {
    StoreError::Io(error.to_string())
}

fn count_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_version::{ObjectKey, Schema};
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_PACK: AtomicU64 = AtomicU64::new(0);

    struct BytesSchema;

    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf5;
        const TYPE: u16 = 29;
        const VERSION: u8 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct TempPack(PathBuf);

    impl TempPack {
        fn new() -> (Self, File) {
            let ordinal = NEXT_PACK.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-store-artifact-pack-{}-{ordinal}.tmp",
                std::process::id()
            ));
            let _ = fs::remove_file(&path);
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .expect("create private pack file");
            (Self(path), file)
        }
    }

    impl Drop for TempPack {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn object(bytes: &[u8]) -> TypedObject {
        let key = ObjectKey::<BytesSchema>::from_value(bytes);
        TypedObject::from_value(&key, bytes)
    }

    #[test]
    fn writer_rejects_an_envelope_whose_id_disagrees_with_its_extent() {
        let (_temp, file) = TempPack::new();
        let claimed = object(b"logical object A");
        let emitted = object(b"different object B");
        let mut writer =
            ArtifactPackWriter::new(file, 1, 1024 * 1024).expect("create bounded pack writer");
        let result = writer.push_with(claimed.id(), |output| {
            write_object_envelope(&emitted, output, 4096)
        });
        assert!(matches!(result, Err(StoreError::Corrupt)));
    }

    #[test]
    fn verified_pack_ranges_reject_tampering_and_manifest_mutation() {
        let (temp, file) = TempPack::new();
        let item = object(b"stable pack vector payload");
        let registry = RelationAdmissionRegistry::default();
        let mut writer =
            ArtifactPackWriter::new_with_registry(file, 1, 1024 * 1024, registry.clone())
                .expect("create bounded pack writer");
        writer
            .push_with(item.id(), |output| {
                write_object_envelope(&item, output, 4096)
            })
            .expect("stream checked typed envelope");
        let (mut pack, manifest) = writer.finish().expect("finish immutable pack");
        let encoded = manifest.encode();
        assert_eq!(
            ArtifactPackManifest::parse(&encoded, manifest.pack_bytes())
                .expect("cold parse checked manifest")
                .pack_id(),
            manifest.pack_id()
        );

        let extent = manifest.extents().first().expect("one object extent");
        pack.seek(SeekFrom::Start(extent.offset()))
            .expect("seek to complete object range");
        let verified = manifest
            .verify_object_envelope(item.id(), &mut pack, &registry)
            .expect("verify complete pack range and typed envelope");
        assert_eq!(verified.id(), item.id());
        assert_eq!(verified.pack_id(), manifest.pack_id());
        assert_eq!(verified.layout_id(), manifest.layout_id());

        let mut mutated_manifest = encoded;
        mutated_manifest[HEADER_BYTES - 1] ^= 1;
        assert!(ArtifactPackManifest::parse(&mutated_manifest, manifest.pack_bytes()).is_err());

        pack.seek(SeekFrom::Start(extent.offset()))
            .expect("seek to first packed envelope");
        let mut byte = [0_u8; 1];
        pack.read_exact(&mut byte).expect("read tamper byte");
        pack.seek(SeekFrom::Start(extent.offset()))
            .expect("rewind to tamper byte");
        pack.write_all(&[byte[0] ^ 1])
            .expect("tamper complete range");
        pack.seek(SeekFrom::Start(extent.offset()))
            .expect("seek to corrupted complete range");
        assert!(
            manifest
                .verify_object_envelope(item.id(), &mut pack, &registry)
                .is_err()
        );
        drop(pack);
        drop(temp);
    }

    #[test]
    fn cold_paged_layout_matches_an_independent_raw_file_oracle() {
        let (temp, file) = TempPack::new();
        let mut candidates = (0..512)
            .map(|index| {
                let payload_len = match index % 3 {
                    0 => 32,
                    1 => 1024,
                    _ => 64 * 1024,
                };
                let mut payload =
                    vec![u8::try_from(index % 251).expect("bounded byte"); payload_len];
                payload[..8]
                    .copy_from_slice(&u64::try_from(index).expect("fixture index").to_be_bytes());
                object(&payload)
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.id().as_bytes().cmp(right.id().as_bytes()));
        let boundary_start = candidates
            .windows(4)
            .enumerate()
            .skip(30)
            .find_map(|(index, window)| {
                let mut lengths = window
                    .iter()
                    .map(|item| item.bytes().len())
                    .collect::<Vec<_>>();
                lengths.sort_unstable();
                lengths.dedup();
                (lengths == [32, 1024, 64 * 1024]).then_some(index)
            })
            .expect("candidate set has all payload classes near a page boundary");
        assert!(boundary_start + 34 <= candidates.len());
        let items = candidates[boundary_start - 30..boundary_start + 34].to_vec();
        let mut boundary_payload_lengths = items[30..34]
            .iter()
            .map(|item| item.bytes().len())
            .collect::<Vec<_>>();
        boundary_payload_lengths.sort_unstable();
        boundary_payload_lengths.dedup();
        assert_eq!(boundary_payload_lengths, [32, 1024, 64 * 1024]);

        let expected_envelopes = items
            .iter()
            .map(|item| {
                let mut bytes = Vec::new();
                write_object_envelope(item, &mut bytes, 128 * 1024)
                    .expect("write independent canonical envelope oracle");
                bytes
            })
            .collect::<Vec<_>>();
        let registry = RelationAdmissionRegistry::default();
        let mut writer = ArtifactPackWriter::new_with_registry(
            file,
            items.len(),
            4 * 1024 * 1024,
            registry.clone(),
        )
        .expect("create paged pack writer");
        for item in &items {
            writer
                .push_with(item.id(), |output| {
                    write_object_envelope(item, output, 128 * 1024)
                })
                .expect("write sorted typed object");
        }
        let (mut pack, built) = writer.finish().expect("finish paged pack");
        assert_eq!(built.pages().len(), 2);
        assert!(built.manifest_bytes() < 1024);
        let directory_upper_bound =
            artifact_pack_directory_prefix_upper_bound(built.manifest_bytes())
                .expect("valid root length has a conservative directory bound");
        let exact_directory_end = built
            .pages()
            .last()
            .expect("root has a final page")
            .offset()
            + u64::from(built.pages().last().expect("root has a final page").bytes());
        assert!(u64::try_from(directory_upper_bound).unwrap() >= exact_directory_end);
        assert!(artifact_pack_directory_prefix_upper_bound(0).is_none());
        assert!(artifact_pack_directory_prefix_upper_bound(u32::MAX).is_none());

        // Independently parse fixed fields and descriptors from bytes on disk.
        // No expected range below is derived from the Rust manifest decoder.
        let mut fixed_header = [0_u8; HEADER_BYTES];
        pack.seek(SeekFrom::Start(0))
            .expect("seek to raw root header");
        pack.read_exact(&mut fixed_header)
            .expect("read fixed root header");
        assert_eq!(&fixed_header[..8], MAGIC);
        assert_eq!(
            u16::from_be_bytes(fixed_header[8..10].try_into().unwrap()),
            VERSION
        );
        assert_eq!(
            u16::from_be_bytes(fixed_header[10..12].try_into().unwrap()) as usize,
            HEADER_BYTES
        );
        let root_len = u32::from_be_bytes(fixed_header[12..16].try_into().unwrap()) as usize;
        let object_count = u32::from_be_bytes(fixed_header[16..20].try_into().unwrap()) as usize;
        let page_count = u32::from_be_bytes(fixed_header[20..24].try_into().unwrap()) as usize;
        let pack_bytes = u64::from_be_bytes(fixed_header[24..32].try_into().unwrap());
        let data_start = u64::from_be_bytes(fixed_header[32..40].try_into().unwrap());
        assert_eq!(object_count, items.len());
        assert_eq!(page_count, 2);
        assert_eq!(root_len, HEADER_BYTES + page_count * PAGE_DESCRIPTOR_BYTES);
        assert_eq!(pack_bytes, pack.metadata().expect("pack metadata").len());

        let mut root_bytes = vec![0_u8; root_len];
        pack.seek(SeekFrom::Start(0)).expect("rewind to full root");
        pack.read_exact(&mut root_bytes).expect("read full root");
        let mut expected_page_offset = u64::try_from(root_len).expect("root length fits u64");
        let mut expected_data_offset = data_start;
        let mut raw_pages = Vec::with_capacity(page_count);
        for page_index in 0..page_count {
            let descriptor_offset = HEADER_BYTES + page_index * PAGE_DESCRIPTOR_BYTES;
            let descriptor =
                &root_bytes[descriptor_offset..descriptor_offset + PAGE_DESCRIPTOR_BYTES];
            let page_offset = u64::from_be_bytes(descriptor[64..72].try_into().unwrap());
            let page_bytes = u32::from_be_bytes(descriptor[72..76].try_into().unwrap()) as usize;
            let page_objects = u16::from_be_bytes(descriptor[76..78].try_into().unwrap()) as usize;
            let page_data_offset = u64::from_be_bytes(descriptor[80..88].try_into().unwrap());
            let page_data_end = u64::from_be_bytes(descriptor[88..96].try_into().unwrap());
            assert_eq!(page_offset, expected_page_offset);
            assert_eq!(page_objects, ARTIFACT_PACK_PAGE_OBJECTS);
            assert_eq!(page_data_offset, expected_data_offset);
            let mut bytes = vec![0_u8; page_bytes];
            pack.seek(SeekFrom::Start(page_offset))
                .expect("seek raw page");
            pack.read_exact(&mut bytes).expect("read raw page");
            assert_eq!(&bytes[..8], PAGE_MAGIC);
            assert_eq!(
                u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
                PAGE_VERSION
            );
            assert_eq!(
                u16::from_be_bytes(bytes[10..12].try_into().unwrap()) as usize,
                page_index
            );
            assert_eq!(
                u16::from_be_bytes(bytes[12..14].try_into().unwrap()) as usize,
                page_objects
            );
            assert_eq!(u16::from_be_bytes(bytes[14..16].try_into().unwrap()), 0);

            let first_object = page_index * ARTIFACT_PACK_PAGE_OBJECTS;
            let mut current_data_offset = page_data_offset;
            for row in 0..page_objects {
                let row_offset = PAGE_HEADER_BYTES + row * EXTENT_ROW_BYTES;
                let row_bytes = &bytes[row_offset..row_offset + EXTENT_ROW_BYTES];
                let global_object_index = first_object + row;
                assert_eq!(&row_bytes[..32], items[global_object_index].id().as_bytes());
                let object_offset = u64::from_be_bytes(row_bytes[32..40].try_into().unwrap());
                let object_len = u64::from_be_bytes(row_bytes[40..48].try_into().unwrap());
                assert_eq!(object_offset, current_data_offset);
                assert_eq!(
                    object_len,
                    u64::try_from(expected_envelopes[global_object_index].len())
                        .expect("envelope length fits u64")
                );
                let mut actual_envelope =
                    vec![0_u8; usize::try_from(object_len).expect("bounded test envelope")];
                pack.seek(SeekFrom::Start(object_offset))
                    .expect("seek raw complete envelope");
                pack.read_exact(&mut actual_envelope)
                    .expect("read raw complete envelope");
                assert_eq!(actual_envelope, expected_envelopes[global_object_index]);
                current_data_offset = current_data_offset
                    .checked_add(object_len)
                    .expect("test object range does not overflow");
            }
            assert_eq!(current_data_offset, page_data_end);
            expected_page_offset = expected_page_offset
                .checked_add(u64::try_from(page_bytes).expect("page length fits u64"))
                .expect("page offset does not overflow");
            expected_data_offset = page_data_end;
            raw_pages.push(bytes);
        }
        assert_eq!(expected_page_offset, data_start);
        assert_eq!(expected_data_offset, pack_bytes);

        let cold = ArtifactPackManifest::parse(&root_bytes, built.pack_bytes())
            .expect("reopen root without loading object rows");
        assert_eq!(cold.object_count(), items.len());
        assert!(cold.extents().is_empty());
        assert_eq!(cold.pages().len(), 2);
        for (page_index, page_bytes) in raw_pages.iter().enumerate() {
            let page = cold
                .verify_page(page_index, page_bytes)
                .expect("verify page commitment and exact map");
            let first = page_index * ARTIFACT_PACK_PAGE_OBJECTS;
            for item in &items[first..first + ARTIFACT_PACK_PAGE_OBJECTS] {
                let extent = page.extent(item.id()).expect("page maps exact object ID").1;
                pack.seek(SeekFrom::Start(extent.offset()))
                    .expect("seek to complete envelope");
                let verified = cold
                    .verify_object_envelope_in_page(&page, item.id(), &mut pack, &registry)
                    .expect("verify complete object against checked page");
                assert_eq!(verified.id(), item.id());
            }

            if page_index == 0 {
                let mut tampered_page = page_bytes.to_vec();
                tampered_page[PAGE_HEADER_BYTES + 32] ^= 1;
                assert!(cold.verify_page(page_index, &tampered_page).is_err());
                assert!(
                    cold.verify_page(page_index, &page_bytes[..page_bytes.len() - 1])
                        .is_err()
                );
            }
        }

        let mut changed_page_hash = root_bytes.clone();
        changed_page_hash[HEADER_BYTES + 96] ^= 1;
        assert!(ArtifactPackManifest::parse(&changed_page_hash, pack_bytes).is_err());

        let mut changed_data_offset = root_bytes.clone();
        let first_data_offset = u64::from_be_bytes(
            changed_data_offset[HEADER_BYTES + 80..HEADER_BYTES + 88]
                .try_into()
                .unwrap(),
        );
        changed_data_offset[HEADER_BYTES + 80..HEADER_BYTES + 88]
            .copy_from_slice(&first_data_offset.saturating_add(1).to_be_bytes());
        assert!(ArtifactPackManifest::parse(&changed_data_offset, pack_bytes).is_err());

        let mut missing_page = root_bytes[..root_bytes.len() - PAGE_DESCRIPTOR_BYTES].to_vec();
        let missing_length = u32::try_from(missing_page.len()).expect("root length fits u32");
        missing_page[12..16].copy_from_slice(&missing_length.to_be_bytes());
        missing_page[20..24].copy_from_slice(&1_u32.to_be_bytes());
        assert!(ArtifactPackManifest::parse(&missing_page, built.pack_bytes()).is_err());

        let mut extra_page = root_bytes.clone();
        extra_page.extend_from_slice(&[0_u8; PAGE_DESCRIPTOR_BYTES]);
        let extra_length = u32::try_from(extra_page.len()).expect("root length fits u32");
        extra_page[12..16].copy_from_slice(&extra_length.to_be_bytes());
        extra_page[20..24].copy_from_slice(&3_u32.to_be_bytes());
        assert!(ArtifactPackManifest::parse(&extra_page, built.pack_bytes()).is_err());

        drop(pack);
        drop(temp);
    }
}

fn count_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Cursor<'bytes> {
    fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], StoreError> {
        let end = self.offset.checked_add(length).ok_or(StoreError::Bounds)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(StoreError::Corrupt)?;
        self.offset = end;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, StoreError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().map_err(|_| StoreError::Corrupt)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, StoreError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| StoreError::Corrupt)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, StoreError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| StoreError::Corrupt)?,
        ))
    }

    fn array32(&mut self) -> Result<[u8; 32], StoreError> {
        self.take(32)?.try_into().map_err(|_| StoreError::Corrupt)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
