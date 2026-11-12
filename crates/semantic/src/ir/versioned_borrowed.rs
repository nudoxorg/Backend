//! Allocation-free traversal of the canonical semantic-plane manifest.
//! The resulting bytes remain claims until a selected closure and witnesses
//! independently admit them.
use super::*;

/// Canonical failure or caller work/cancellation refusal.
#[derive(Debug)]
pub enum BorrowedSemanticManifestError<E> {
    Invalid(SemanticManifestError),
    Stopped(E),
}
impl<E> From<SemanticManifestError> for BorrowedSemanticManifestError<E> {
    fn from(value: SemanticManifestError) -> Self { Self::Invalid(value) }
}

/// Fully validated canonical wire borrowed for the lifetime of its scratch.
pub struct BorrowedSemanticManifest<'a> {
    bytes: &'a [u8],
    generation: GenerationId,
    root: SemanticManifestRoot,
    segments: usize,
}
impl<'a> BorrowedSemanticManifest<'a> {
    /// Validates every coverage, ordering, extent and plane-root invariant
    /// using the same primitive validators/hash feed as the owning decoder.
    /// `checkpoint` must debit finite work and can cancel before each record
    /// and before each 64 KiB manifest-hash chunk.
    pub fn parse<E>(bytes: &'a [u8], checkpoint: &mut impl FnMut(usize) -> Result<(), E>) -> Result<Self, BorrowedSemanticManifestError<E>> {
        let (generation, segments) = walk(bytes, checkpoint, &mut |_, _| Ok(()))?;
        let mut hash = manifest_hasher(bytes.len());
        for chunk in bytes.chunks(64 * 1024) {
            checkpoint(chunk.len()).map_err(BorrowedSemanticManifestError::Stopped)?;
            hash.update(chunk);
        }
        Ok(Self { bytes, generation, root: SemanticManifestRoot(*hash.finalize().as_bytes()), segments })
    }
    pub const fn semantic_generation(&self) -> GenerationId { self.generation }
    pub const fn root(&self) -> SemanticManifestRoot { self.root }
    pub const fn segment_count(&self) -> usize { self.segments }
    /// Rewalks the already validated immutable wire. Each yielded segment is
    /// a fixed-size untrusted claim; caller accounts for this second scan.
    pub fn visit_segments<E>(&self, checkpoint: &mut impl FnMut(usize) -> Result<(), E>, visit: &mut impl FnMut(SemanticPlaneKind, SemanticPlaneSegment) -> Result<(), E>) -> Result<(), BorrowedSemanticManifestError<E>> {
        walk(self.bytes, checkpoint, visit).map(|_| ())
    }
}

/// Incrementally hashes the exact canonical catalog encoding without a Vec.
pub struct BorrowedSemanticCatalogHasher {
    hash: blake3::Hasher,
    expected: usize,
    written: usize,
    previous: Option<u32>,
}
impl BorrowedSemanticCatalogHasher {
    pub fn new(count: usize) -> Result<Self, SemanticManifestError> {
        if count > MAX_CATALOG_IMAGES { return Err(SemanticManifestError::TooManyCatalogImages { observed: count, maximum: MAX_CATALOG_IMAGES }); }
        let len = CATALOG_HEADER_BYTES.checked_add(count.checked_mul(CATALOG_ENTRY_BYTES).ok_or(SemanticManifestError::CountOverflow)?).ok_or(SemanticManifestError::CountOverflow)?;
        let mut hash = blake3::Hasher::new_derive_key("backend.semantic.plane.catalog.v1");
        hash.update(&(len as u64).to_be_bytes());
        hash.update(CATALOG_MAGIC); hash.update(&(count as u32).to_be_bytes());
        Ok(Self { hash, expected: count, written: 0, previous: None })
    }
    pub fn push(&mut self, ordinal: u32, generation: GenerationId, root: SemanticManifestRoot, length: u32) -> Result<(), SemanticManifestError> {
        if self.written >= self.expected { return Err(SemanticManifestError::CatalogImageCountOverflow); }
        if self.previous.is_some_and(|previous| previous >= ordinal) { return Err(SemanticManifestError::CatalogImageOrder); }
        if length == 0 { return Err(SemanticManifestError::CatalogManifestLength); }
        self.hash.update(&ordinal.to_be_bytes()); self.hash.update(generation.as_bytes()); self.hash.update(root.as_bytes()); self.hash.update(&length.to_be_bytes());
        self.previous = Some(ordinal); self.written += 1; Ok(())
    }
    pub fn finish(self) -> Result<SemanticPlaneCatalogRoot, SemanticManifestError> {
        if self.written != self.expected { return Err(SemanticManifestError::CatalogImageCountOverflow); }
        Ok(SemanticPlaneCatalogRoot(*self.hash.finalize().as_bytes()))
    }
}

fn walk<E>(bytes: &[u8], checkpoint: &mut impl FnMut(usize) -> Result<(), E>, visit: &mut impl FnMut(SemanticPlaneKind, SemanticPlaneSegment) -> Result<(), E>) -> Result<(GenerationId, usize), BorrowedSemanticManifestError<E>> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(SemanticManifestError::ManifestTooLarge { observed: bytes.len(), maximum: MAX_MANIFEST_BYTES }.into());
    }
    checkpoint(0).map_err(BorrowedSemanticManifestError::Stopped)?;
    let mut reader = Reader::new(bytes);
    if reader.take(4)? != MAGIC.as_slice() { return Err(SemanticManifestError::BadMagic.into()); }
    let version = reader.u8()?;
    if version != WIRE_VERSION { return Err(SemanticManifestError::UnsupportedVersion(version).into()); }
    let _build = decode_build(&mut reader)?;
    let generation = GenerationId::from_raw(reader.array32()?);
    let _input_root = reader.array32()?;
    let read_manifest = ScopeRoot::from_bytes(reader.array32()?);
    let coverage = decode_coverage(&mut reader)?;
    if coverage.scope != read_manifest {
        return Err(SemanticManifestError::InputScopeMismatch { expected: read_manifest, observed: coverage.scope }.into());
    }
    let planes = usize::from(reader.u16()?);
    if planes > MAX_PLANES { return Err(SemanticManifestError::TooManyPlanes { observed: planes, maximum: MAX_PLANES }.into()); }
    let mut previous_plane = None;
    let mut total = 0usize;
    for _ in 0..planes {
        checkpoint(0).map_err(BorrowedSemanticManifestError::Stopped)?;
        let kind = decode_plane_kind(&mut reader)?;
        if previous_plane.is_some_and(|previous| previous >= kind) { return Err(SemanticManifestError::PlaneOrder.into()); }
        previous_plane = Some(kind);
        let coverage = decode_coverage(&mut reader)?;
        let count = usize::try_from(reader.u32()?).map_err(|_| SemanticManifestError::CountOverflow)?;
        if count > MAX_SEGMENTS_PER_PLANE { return Err(SemanticManifestError::TooManySegments { observed: count, maximum: MAX_SEGMENTS_PER_PLANE }.into()); }
        let needed = count.checked_mul(SEGMENT_WIRE_BYTES).ok_or(SemanticManifestError::CountOverflow)?;
        if needed > reader.remaining() { return Err(SemanticManifestError::Truncated.into()); }
        total = total.checked_add(count).ok_or(SemanticManifestError::CountOverflow)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.semantic.ir.plane.v1\0");
        update_plane_kind_hash(&mut hash, kind);
        hash.update(&(count as u32).to_be_bytes());
        let mut previous_last = None;
        for _ in 0..count {
            checkpoint(SEGMENT_WIRE_BYTES).map_err(BorrowedSemanticManifestError::Stopped)?;
            let segment = SemanticPlaneSegment {
                first_key: reader.array32()?, last_key: reader.array32()?, row_count: reader.u32()?, byte_length: reader.u64()?,
                id_claim: UntrustedSemanticSegmentId(reader.array32()?), admitted_id: None, input: decode_input_witness(&mut reader)?,
            };
            validate_segment_metadata(&segment)?;
            if previous_last.is_some_and(|last| last >= segment.first_key) { return Err(SemanticManifestError::SegmentOrder.into()); }
            previous_last = Some(segment.last_key);
            update_plane_segment_hash(&mut hash, &segment);
            visit(kind, segment).map_err(BorrowedSemanticManifestError::Stopped)?;
        }
        let root = SemanticPlaneRoot(*hash.finalize().as_bytes());
        let schema = SchemaIdentity::new(SemanticPlaneCoverageScope::DOMAIN, SemanticPlaneCoverageScope::TYPE, SemanticPlaneCoverageScope::VERSION);
        let expected = ScopeRoot::from_bytes(backend_version::object_version_digest(schema, root.as_bytes()).map_err(|_| SemanticManifestError::CountOverflow)?);
        if coverage.scope != expected { return Err(SemanticManifestError::PlaneScopeMismatch { expected, observed: coverage.scope }.into()); }
    }
    if !reader.is_empty() { return Err(SemanticManifestError::TrailingBytes(reader.remaining()).into()); }
    Ok((generation, total))
}
