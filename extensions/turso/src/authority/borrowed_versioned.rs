//! Borrowed canonical versioned-plane claims. No closure authority is minted.
use super::*;
use backend_semantic::ir::{BorrowedSemanticCatalogHasher, BorrowedSemanticManifest, BorrowedSemanticManifestError};

#[derive(Debug)]
pub(crate) enum BorrowedVersionedError<E> { Invalid(VersionedPlaneError), Stopped(E) }
impl<E> From<VersionedPlaneError> for BorrowedVersionedError<E> {
    fn from(value: VersionedPlaneError) -> Self { Self::Invalid(value) }
}
fn semantic_error<E>(error: BorrowedSemanticManifestError<BorrowedVersionedError<E>>) -> BorrowedVersionedError<E> {
    match error { BorrowedSemanticManifestError::Invalid(e) => BorrowedVersionedError::Invalid(e.into()), BorrowedSemanticManifestError::Stopped(e) => e }
}

pub(crate) struct BorrowedVersionedMetadata<'a> {
    bytes: &'a [u8], catalog_root: SemanticPlaneCatalogRoot, artifact_count: usize, segment_count: usize,
}
impl<'a> BorrowedVersionedMetadata<'a> {
    /// Fully checks the canonical record and logical-to-physical bijection.
    /// `bitmap` is caller-owned, precharged scratch and is cleared per artifact.
    pub(crate) fn parse<E>(bytes: &'a [u8], bitmap: &mut [u8], checkpoint: &mut impl FnMut(usize) -> Result<(), E>) -> Result<Self, BorrowedVersionedError<E>> {
        let mut d = Decoder::new(bytes);
        checkpoint(0).map_err(BorrowedVersionedError::Stopped)?;
        if d.take(MAGIC.len())? != MAGIC { return Err(VersionedPlaneError::Magic.into()); }
        if d.u16()? != VERSION { return Err(VersionedPlaneError::Version.into()); }
        let expected_catalog = SemanticPlaneCatalogRoot::from_wire_claim(d.array::<32>()?);
        let artifacts = d.u32()? as usize;
        if artifacts == 0 || artifacts > MAX_ARTIFACTS { return Err(VersionedPlaneError::ArtifactCount.into()); }
        let mut catalog = BorrowedSemanticCatalogHasher::new(artifacts).map_err(VersionedPlaneError::from)?;
        let mut manifest_total = 0usize;
        let mut member_total = artifacts;
        let mut segment_total = 0usize;
        let mut payload_total = 0u64;
        for expected_ordinal in 0..artifacts {
            checkpoint(0).map_err(BorrowedVersionedError::Stopped)?;
            let ordinal = d.u32()?;
            if usize::try_from(ordinal).ok() != Some(expected_ordinal) { return Err(VersionedPlaneError::NonContiguousArtifactOrdinal.into()); }
            let generation = GenerationId::from_raw(d.array::<32>()?);
            let expected_root = SemanticManifestRoot::from_wire_claim(d.array::<32>()?);
            let physical = d.array::<32>()?;
            let manifest_len = d.u32()? as usize;
            if manifest_len == 0 || manifest_len > MAX_MANIFEST_BYTES { return Err(VersionedPlaneError::ManifestLength.into()); }
            manifest_total = manifest_total.checked_add(manifest_len).ok_or(VersionedPlaneError::ManifestLength)?;
            if manifest_total > MAX_TOTAL_MANIFEST_BYTES { return Err(VersionedPlaneError::ManifestLength.into()); }
            let manifest_bytes = d.take(manifest_len)?;
            let count = d.u32()? as usize;
            if count == 0 || count > MAX_SEGMENTS { return Err(VersionedPlaneError::MemberCount.into()); }
            segment_total = segment_total.checked_add(count).ok_or(VersionedPlaneError::MemberCount)?;
            member_total = member_total.checked_add(count).ok_or(VersionedPlaneError::MemberCount)?;
            if segment_total > MAX_SEGMENTS || member_total > MAX_SEGMENTS { return Err(VersionedPlaneError::MemberCount.into()); }
            let members = d.take(count.checked_mul(MEMBER_BYTES).ok_or(VersionedPlaneError::Truncated)?)?;
            let needed = count.checked_add(7).ok_or(VersionedPlaneError::MemberCount)? / 8;
            if bitmap.len() < needed { return Err(VersionedPlaneError::Allocation.into()); }
            checkpoint(needed).map_err(BorrowedVersionedError::Stopped)?;
            bitmap[..needed].fill(0);
            let mut previous: Option<[u8;32]> = None;
            for cell in members.chunks_exact(MEMBER_BYTES) {
                checkpoint(MEMBER_BYTES).map_err(BorrowedVersionedError::Stopped)?;
                let id: [u8;32] = cell[..32].try_into().map_err(|_| VersionedPlaneError::Truncated)?;
                let object: [u8;32] = cell[32..64].try_into().map_err(|_| VersionedPlaneError::Truncated)?;
                let length = u64::from_le_bytes(cell[64..72].try_into().map_err(|_| VersionedPlaneError::Truncated)?);
                if previous.is_some_and(|p| p >= id) { return Err(VersionedPlaneError::DuplicateSegment.into()); }
                previous = Some(id);
                if object == [0;32] || length == 0 || length > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64 { return Err(VersionedPlaneError::SegmentObject.into()); }
                payload_total = payload_total.checked_add(length).ok_or(VersionedPlaneError::PayloadBudget)?;
                if payload_total > MAX_TOTAL_SEGMENT_PAYLOAD_BYTES as u64 { return Err(VersionedPlaneError::PayloadBudget.into()); }
            }
            let id = backend_store::untrusted_object_id_for_encoded_value_checkpointed(VERSIONED_PLANE_MANIFEST_SCHEMA, manifest_bytes, checkpoint)
                .map_err(|e| match e { backend_store::IdentityHashError::Bounds => BorrowedVersionedError::Invalid(VersionedPlaneError::ManifestObject), backend_store::IdentityHashError::Stopped(e) => BorrowedVersionedError::Stopped(e) })?;
            if id.as_bytes() != &physical { return Err(VersionedPlaneError::ManifestObject.into()); }
            let work = std::cell::RefCell::new(&mut *checkpoint);
            let mut check = |n| (work.borrow_mut())(n).map_err(BorrowedVersionedError::Stopped);
            let view = BorrowedSemanticManifest::parse(manifest_bytes, &mut check).map_err(semantic_error)?;
            if view.semantic_generation() != generation || view.root() != expected_root { return Err(VersionedPlaneError::ArtifactImage.into()); }
            if view.segment_count() != count { return Err(VersionedPlaneError::MemberCount.into()); }
            view.visit_segments(&mut check, &mut |_, segment| {
                let id = segment.id_claim();
                let mut low = 0; let mut high = count;
                while low < high {
                    (work.borrow_mut())(32).map_err(BorrowedVersionedError::Stopped)?;
                    let mid = low + (high - low)/2;
                    let cell = &members[mid*MEMBER_BYTES..(mid+1)*MEMBER_BYTES];
                    match cell[..32].cmp(id.as_bytes()) {
                        std::cmp::Ordering::Less => low=mid+1,
                        std::cmp::Ordering::Greater => high=mid,
                        std::cmp::Ordering::Equal => {
                            let length = u64::from_le_bytes(cell[64..72].try_into().map_err(|_| BorrowedVersionedError::Invalid(VersionedPlaneError::Truncated))?);
                            let mask = 1u8 << (mid % 8);
                            if bitmap[mid/8] & mask != 0 || length != segment.byte_length() { return Err(BorrowedVersionedError::Invalid(VersionedPlaneError::ManifestMemberMismatch)); }
                            bitmap[mid/8] |= mask; return Ok(());
                        }
                    }
                }
                Err(BorrowedVersionedError::Invalid(VersionedPlaneError::ManifestMemberMismatch))
            }).map_err(semantic_error)?;
            catalog.push(ordinal, generation, expected_root, u32::try_from(manifest_len).map_err(|_| VersionedPlaneError::ManifestLength)?).map_err(VersionedPlaneError::from)?;
        }
        if !d.is_empty() { return Err(VersionedPlaneError::TrailingBytes.into()); }
        let catalog_len = 11usize.checked_add(artifacts.checked_mul(72).ok_or(VersionedPlaneError::CatalogLength)?).ok_or(VersionedPlaneError::CatalogLength)?;
        if manifest_total.checked_add(catalog_len).ok_or(VersionedPlaneError::CatalogLength)? > MAX_TOTAL_MANIFEST_BYTES { return Err(VersionedPlaneError::CatalogLength.into()); }
        if catalog.finish().map_err(VersionedPlaneError::from)? != expected_catalog { return Err(VersionedPlaneError::CatalogRoot.into()); }
        Ok(Self { bytes, catalog_root: expected_catalog, artifact_count: artifacts, segment_count: segment_total })
    }
    pub(crate) fn catalog_root(&self) -> SemanticPlaneCatalogRoot { self.catalog_root }
    pub(crate) fn artifact_count(&self) -> usize { self.artifact_count }
    pub(crate) fn segment_count(&self) -> usize { self.segment_count }
    /// Emits physical IDs, including legitimate duplicates; caller must dedup
    /// against its authenticated envelope inventory and track exact coverage.
    pub(crate) fn visit_physical<E>(&self, checkpoint: &mut impl FnMut(usize) -> Result<(), E>, visit: &mut impl FnMut([u8;32]) -> Result<(), E>) -> Result<(), BorrowedVersionedError<E>> {
        let mut d = Decoder::new(self.bytes); d.take(MAGIC.len()+2+32)?; let count=d.u32()? as usize;
        for _ in 0..count {
            checkpoint(0).map_err(BorrowedVersionedError::Stopped)?;
            d.take(4+32+32)?; let object=d.array::<32>()?; visit(object).map_err(BorrowedVersionedError::Stopped)?;
            let len=d.u32()? as usize; d.take(len)?; let members=d.u32()? as usize;
            for _ in 0..members { checkpoint(MEMBER_BYTES).map_err(BorrowedVersionedError::Stopped)?; d.take(32)?; let object=d.array::<32>()?; d.take(8)?; visit(object).map_err(BorrowedVersionedError::Stopped)?; }
        }
        Ok(())
    }
}
