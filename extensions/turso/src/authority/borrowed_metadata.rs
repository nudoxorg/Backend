//! Fully checked borrowed compiler metadata; bytes remain selected claims.
use super::*;
use super::super::versioned::borrowed_versioned::{BorrowedVersionedError, BorrowedVersionedMetadata};

#[derive(Debug)]
pub(super) enum BorrowedMetadataError<E> { Invalid(CompilerEnvelopeError), Stopped(E) }
impl<E> From<CompilerEnvelopeError> for BorrowedMetadataError<E> {
    fn from(error: CompilerEnvelopeError) -> Self { Self::Invalid(error) }
}
fn versioned_error<E>(error: BorrowedVersionedError<E>) -> BorrowedMetadataError<E> {
    match error { BorrowedVersionedError::Invalid(e) => BorrowedMetadataError::Invalid(CompilerEnvelopeError::VersionedPlane(e)), BorrowedVersionedError::Stopped(e) => BorrowedMetadataError::Stopped(e) }
}

pub(super) struct BorrowedCompilerMetadata<'a> {
    pub(super) binding_identity: AuthorityHash,
    pub(super) semantic_catalog_root: Option<AuthorityHash>,
    pub(super) native_program_sources: Option<CompilerProgramSourceMember>,
    pub(super) native_headers: Option<CompilerNativeHeadersMember>,
    image_rows: &'a [u8],
    versioned: Option<BorrowedVersionedMetadata<'a>>,
}

fn bitmap_mark<E>(cells: &[u8], bitmap: &mut [u8], id: &[u8;32], checkpoint: &mut impl FnMut(usize) -> Result<(), E>) -> Result<bool, BorrowedMetadataError<E>> {
    let count = cells.len()/32;
    let mut low = 0usize; let mut high = count;
    while low < high {
        checkpoint(32).map_err(BorrowedMetadataError::Stopped)?;
        let mid = low + (high-low)/2;
        match cells[mid*32..(mid+1)*32].cmp(id.as_slice()) {
            std::cmp::Ordering::Less => low=mid+1,
            std::cmp::Ordering::Greater => high=mid,
            std::cmp::Ordering::Equal => {
                let mask = 1u8 << (mid%8);
                let new = bitmap[mid/8] & mask == 0;
                bitmap[mid/8] |= mask;
                return Ok(new);
            }
        }
    }
    Err(CompilerEnvelopeError::MetadataMismatch.into())
}

fn marked<E>(cells: &[u8], bitmap: &[u8], id: &[u8;32], checkpoint: &mut impl FnMut(usize) -> Result<(), E>) -> Result<bool, BorrowedMetadataError<E>> {
    let mut low=0usize; let mut high=cells.len()/32;
    while low<high { checkpoint(32).map_err(BorrowedMetadataError::Stopped)?; let mid=low+(high-low)/2;
        match cells[mid*32..(mid+1)*32].cmp(id.as_slice()) {
            std::cmp::Ordering::Less=>low=mid+1, std::cmp::Ordering::Greater=>high=mid,
            std::cmp::Ordering::Equal=>return Ok(bitmap[mid/8] & (1u8 << (mid%8)) != 0),
        }
    }
    Ok(false)
}

fn stage_identity<E>(common: &[u8], version: u16, program: Option<CompilerProgramSourceMember>, coverage: Option<CompilerSourceCoverageMember>, snapshot: Option<CompilerTypeScriptSnapshotMember>, checkpoint: &mut impl FnMut(usize)->Result<(),E>) -> Result<[u8;32], BorrowedMetadataError<E>> {
    let mut program_wire=[0u8;68]; let mut coverage_wire=[0u8;68]; let mut snapshot_wire=[0u8;36];
    if let Some(p)=program {program_wire[..32].copy_from_slice(&p.object_id); program_wire[32..64].copy_from_slice(&p.program); program_wire[64..].copy_from_slice(&p.byte_length.to_le_bytes());}
    if let Some(c)=coverage {coverage_wire[..32].copy_from_slice(&c.object_id); coverage_wire[32..64].copy_from_slice(&c.identity); coverage_wire[64..].copy_from_slice(&c.byte_length.to_le_bytes());}
    if let Some(s)=snapshot {snapshot_wire[..32].copy_from_slice(&s.object_id); snapshot_wire[32..].copy_from_slice(&s.byte_length.to_le_bytes());}
    let version_bytes=version.to_le_bytes(); let yes=[1u8]; let no=[0u8]; let mut parts: [&[u8];10]=[&[];10]; let mut n=0usize;
    parts[n]=METADATA_MAGIC; n+=1; parts[n]=&version_bytes; n+=1; parts[n]=common; n+=1;
    if version==COVERAGE_METADATA_VERSION || version==TYPESCRIPT_SNAPSHOT_METADATA_VERSION { parts[n]=if program.is_some(){&yes}else{&no}; n+=1; }
    if program.is_some() {parts[n]=&program_wire; n+=1;}
    if version==TYPESCRIPT_SNAPSHOT_METADATA_VERSION {parts[n]=if coverage.is_some(){&yes}else{&no}; n+=1;}
    if coverage.is_some() {parts[n]=&coverage_wire; n+=1;}
    if snapshot.is_some() {parts[n]=&snapshot_wire; n+=1;}
    let schema=match version {METADATA_VERSION=>COMPILER_PUBLICATION_METADATA_SCHEMA,PROGRAM_METADATA_VERSION=>COMPILER_PROGRAM_METADATA_SCHEMA,COVERAGE_METADATA_VERSION=>COMPILER_COVERAGE_METADATA_SCHEMA,TYPESCRIPT_SNAPSHOT_METADATA_VERSION=>COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA,_=>return Err(CompilerEnvelopeError::MetadataVersion.into())};
    let id=backend_store::untrusted_object_id_for_encoded_parts_checkpointed(schema,&parts[..n],checkpoint).map_err(|e|match e {backend_store::IdentityHashError::Bounds=>BorrowedMetadataError::Invalid(CompilerEnvelopeError::ByteLengthOverflow),backend_store::IdentityHashError::Stopped(e)=>BorrowedMetadataError::Stopped(e)})?;
    Ok(*id.as_bytes())
}

impl<'a> BorrowedCompilerMetadata<'a> {
    /// Validates canonical semantics against an authenticated sorted envelope.
    /// Bitmap is caller-owned scratch; both metadata and member cells must
    /// remain alive while this view is used. It grants no source authority.
    pub(super) fn parse<E>(bytes: &'a [u8], metadata_id: [u8;32], members: &[u8], bitmap: &mut [u8], checkpoint: &mut impl FnMut(usize) -> Result<(), E>) -> Result<Self, BorrowedMetadataError<E>> {
        if members.len()%32 != 0 || members.len()/32 > MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::MemberCount.into()); }
        let member_count = members.len()/32;
        let needed = member_count.checked_add(7).ok_or(CompilerEnvelopeError::MemberCount)?/8;
        if bitmap.len() < needed { return Err(CompilerEnvelopeError::MemberCount.into()); }
        let mut prior = None;
        for cell in members.chunks_exact(32) {
            checkpoint(32).map_err(BorrowedMetadataError::Stopped)?;
            let id: [u8;32] = cell.try_into().map_err(|_| CompilerEnvelopeError::Truncated)?;
            if prior.is_some_and(|p| p >= id) { return Err(CompilerEnvelopeError::MemberOrder.into()); }
            prior = Some(id);
        }
        let mut d=Decoder::new(bytes);
        if d.take(METADATA_MAGIC.len())? != METADATA_MAGIC { return Err(CompilerEnvelopeError::MetadataMagic.into()); }
        let version=d.u16()?;
        if !matches!(version, METADATA_VERSION | PROGRAM_METADATA_VERSION | COVERAGE_METADATA_VERSION | TYPESCRIPT_SNAPSHOT_METADATA_VERSION | CSHARP_EVIDENCE_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION) { return Err(CompilerEnvelopeError::MetadataVersion.into()); }
        let manifest_len=d.u32()? as usize;
        if manifest_len < SEMANTIC_MANIFEST_HEADER_BYTES || manifest_len > MAX_MANIFEST_BYTES { return Err(CompilerEnvelopeError::ManifestFacts.into()); }
        let manifest=d.take(manifest_len)?;
        if &manifest[..8] != SEMANTIC_MANIFEST_MAGIC || manifest[10..12] != [0,0] { return Err(CompilerEnvelopeError::ManifestFacts.into()); }
        let format=CompilationManifestFormat::from_version(u16::from_le_bytes([manifest[8],manifest[9]])).filter(|f| f.is_semantic()).ok_or(CompilerEnvelopeError::ManifestFacts)?;
        let count=u32::from_le_bytes(manifest[12..16].try_into().map_err(|_| CompilerEnvelopeError::ManifestFacts)?) as usize;
        if count == 0 || count > MAX_MEMBER_IDS || SEMANTIC_MANIFEST_HEADER_BYTES.checked_add(count.checked_mul(SEMANTIC_MANIFEST_ENTRY_BYTES).ok_or(CompilerEnvelopeError::ManifestFacts)?) != Some(manifest_len) { return Err(CompilerEnvelopeError::ManifestFacts.into()); }
        let mut hasher=backend_version::ArtifactHasher::<IrManifestEncoding, IrManifestDomain>::new();
        for chunk in manifest.chunks(64*1024) { checkpoint(chunk.len()).map_err(BorrowedMetadataError::Stopped)?; hasher.write_chunk(chunk); }
        let manifest_identity: AuthorityHash = *hasher.finalize().as_ref();
        let binding=d.array::<COMPILATION_BINDING_BYTES>()?;
        let binding_identity=validate_binding(&binding, &manifest_identity)?;
        let pinned: AuthorityHash=binding[12..44].try_into().map_err(|_| CompilerEnvelopeError::Binding)?;
        let dependencies: AuthorityHash=binding[44..76].try_into().map_err(|_| CompilerEnvelopeError::Binding)?;
        GenerationId::try_from(pinned).map_err(|_| CompilerEnvelopeError::Binding)?;
        ContentId::<DependencySetDomain>::try_from(dependencies).map_err(|_| CompilerEnvelopeError::Binding)?;
        let images=d.u32()? as usize;
        if images != count || images == 0 { return Err(CompilerEnvelopeError::ManifestFacts.into()); }
        let image_rows=d.take(images.checked_mul(72).ok_or(CompilerEnvelopeError::MemberCount)?)?;
        let bitmap_need=images.checked_add(7).ok_or(CompilerEnvelopeError::MemberCount)?/8;
        if bitmap.len() < bitmap_need { return Err(CompilerEnvelopeError::MemberCount.into()); }
        checkpoint(bitmap_need).map_err(BorrowedMetadataError::Stopped)?;
        bitmap[..bitmap_need].fill(0);
        let mut previous=None;
        for row in image_rows.chunks_exact(72) {
            checkpoint(72).map_err(BorrowedMetadataError::Stopped)?;
            let ordinal=u32::from_le_bytes(row[..4].try_into().map_err(|_| CompilerEnvelopeError::Truncated)?) as usize;
            let object: [u8;32]=row[4..36].try_into().map_err(|_| CompilerEnvelopeError::Truncated)?;
            let identity: [u8;32]=row[36..68].try_into().map_err(|_| CompilerEnvelopeError::Truncated)?;
            let length=u32::from_le_bytes(row[68..72].try_into().map_err(|_| CompilerEnvelopeError::Truncated)?);
            if previous.is_some_and(|p| p >= object) || ordinal >= images { return Err(CompilerEnvelopeError::MemberOrder.into()); }
            previous=Some(object);
            if length==0 { return Err(CompilerEnvelopeError::ImageLength.into()); }
            SemanticImageIdentity::try_from(identity).map_err(|_| CompilerEnvelopeError::ImageIdentity)?;
            let mask=1u8 << (ordinal%8); if bitmap[ordinal/8] & mask != 0 { return Err(CompilerEnvelopeError::MemberOrder.into()); } bitmap[ordinal/8] |= mask;
            let entry=SEMANTIC_MANIFEST_HEADER_BYTES + ordinal*SEMANTIC_MANIFEST_ENTRY_BYTES;
            let id_at=entry+SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET; let len_at=entry+SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET;
            if manifest[id_at..id_at+32] != identity || u32::from_le_bytes(manifest[len_at..len_at+4].try_into().map_err(|_| CompilerEnvelopeError::ManifestFacts)?) != length { return Err(CompilerEnvelopeError::MetadataMismatch.into()); }
        }
        let versioned=match d.u8()? {
            0 => None,
            1 => { let len=d.u32()? as usize; let wire=d.take(len)?; let view=BorrowedVersionedMetadata::parse(wire,bitmap,checkpoint).map_err(versioned_error)?; if view.artifact_count()!=images { return Err(CompilerEnvelopeError::VersionedPlaneMetadata.into()); } Some(view) },
            _ => return Err(CompilerEnvelopeError::VersionedPlaneMetadata.into()),
        };
        let common_end=d.offset;
        let has_program=if matches!(version,COVERAGE_METADATA_VERSION|TYPESCRIPT_SNAPSHOT_METADATA_VERSION|NATIVE_HEADERS_METADATA_VERSION) { match d.u8()? {0=>false,1=>true,_=>return Err(CompilerEnvelopeError::NativeProgramSources.into())} } else { version==PROGRAM_METADATA_VERSION };
        let program=if has_program { let m=CompilerProgramSourceMember {object_id:d.array::<32>()?,program:d.array::<32>()?,byte_length:d.u32()?}; if m.byte_length==0 || m.byte_length as usize>backend_semantic::ir::MAX_NATIVE_PROGRAM_MANIFEST_BYTES {return Err(CompilerEnvelopeError::NativeProgramSources.into());} Some(m) } else {None};
        let has_coverage=match version { COVERAGE_METADATA_VERSION=>true, TYPESCRIPT_SNAPSHOT_METADATA_VERSION|NATIVE_HEADERS_METADATA_VERSION=>match d.u8()? {0=>false,1=>true,_=>return Err(CompilerEnvelopeError::SourceCoverage.into())}, _=>false };
        let coverage=if has_coverage {let m=CompilerSourceCoverageMember {object_id:d.array::<32>()?,identity:d.array::<32>()?,byte_length:d.u32()?}; if m.byte_length==0 || m.byte_length as usize>backend_semantic::ir::MAX_SOURCE_COVERAGE_BYTES {return Err(CompilerEnvelopeError::SourceCoverage.into());} Some(m)} else {None};
        let has_snapshot=match version { TYPESCRIPT_SNAPSHOT_METADATA_VERSION=>true,NATIVE_HEADERS_METADATA_VERSION=>match d.u8()? {0=>false,1=>true,_=>return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness.into())},_=>false};
        let snapshot=if has_snapshot {let m=CompilerTypeScriptSnapshotMember {object_id:d.array::<32>()?,byte_length:d.u32()?}; if m.byte_length==0 || m.byte_length as usize>backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES {return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness.into());} Some(m)} else {None};
        let csharp=if version==CSHARP_EVIDENCE_METADATA_VERSION {let m=CompilerCSharpEvidenceMember {object_id:d.array::<32>()?,byte_length:d.u32()?}; if m.byte_length==0 || m.byte_length as usize>backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES {return Err(CompilerEnvelopeError::CSharpInvocationEvidence.into());} Some(m)} else {None};
        let headers=if version==NATIVE_HEADERS_METADATA_VERSION {let m=CompilerNativeHeadersMember {object_id:d.array::<32>()?,byte_length:d.u32()?}; if m.byte_length==0 || m.byte_length as usize>MAX_COMPILER_NATIVE_HEADERS_BYTES {return Err(CompilerEnvelopeError::NativeHeaders.into());} Some(m)} else {None};
        if !d.is_empty() { return Err(CompilerEnvelopeError::TrailingBytes.into()); }
        if (coverage.is_some() || snapshot.is_some() || headers.is_some()) && program.is_none() { return Err(CompilerEnvelopeError::NativeProgramSources.into()); }
        let schema=match version { METADATA_VERSION=>COMPILER_PUBLICATION_METADATA_SCHEMA, PROGRAM_METADATA_VERSION=>COMPILER_PROGRAM_METADATA_SCHEMA, COVERAGE_METADATA_VERSION=>COMPILER_COVERAGE_METADATA_SCHEMA, TYPESCRIPT_SNAPSHOT_METADATA_VERSION=>COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA, CSHARP_EVIDENCE_METADATA_VERSION=>COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA, NATIVE_HEADERS_METADATA_VERSION=>COMPILER_NATIVE_HEADERS_METADATA_SCHEMA, _=>return Err(CompilerEnvelopeError::MetadataVersion.into()) };
        let id=backend_store::untrusted_object_id_for_encoded_value_checkpointed(schema,bytes,checkpoint).map_err(|e| match e {backend_store::IdentityHashError::Bounds=>BorrowedMetadataError::Invalid(CompilerEnvelopeError::ByteLengthOverflow),backend_store::IdentityHashError::Stopped(e)=>BorrowedMetadataError::Stopped(e)})?;
        if id.as_bytes()!=&metadata_id { return Err(CompilerEnvelopeError::MetadataMismatch.into()); }
        checkpoint(needed).map_err(BorrowedMetadataError::Stopped)?;
        bitmap[..needed].fill(0);
        let mut unique_versioned=0usize;
        if let Some(planes)=&versioned {
            planes.visit_physical(&mut |_| Ok::<(), BorrowedMetadataError<E>>(()), &mut |id| {
                if bitmap_mark(members,bitmap,&id,checkpoint)? {unique_versioned+=1;} Ok(())
            }).map_err(|e| match e {BorrowedVersionedError::Invalid(e)=>BorrowedMetadataError::Invalid(CompilerEnvelopeError::VersionedPlane(e)),BorrowedVersionedError::Stopped(e)=>e})?;
        }
        if images.checked_add(unique_versioned).and_then(|v|v.checked_add(1)).ok_or(CompilerEnvelopeError::MemberCount)? > MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::MemberCount.into()); }
        let mut base_unique=unique_versioned;
        for row in image_rows.chunks_exact(72) {let id: [u8;32]=row[4..36].try_into().map_err(|_| CompilerEnvelopeError::Truncated)?; if bitmap_mark(members,bitmap,&id,checkpoint)? {base_unique+=1;} }
        // The legacy builder checks its current metadata object identity
        // before each optional attachment. Recompute each intermediate wire
        // identity without allocating; physical collisions are counted exactly.
        let common=&bytes[METADATA_MAGIC.len()+2..common_end];
        let mut previous: [Option<[u8;32]>;4]=[None;4]; let mut previous_len=0usize;
        for (present, stage, optional) in [
            (program.is_some(), METADATA_VERSION, program.map(|v|v.object_id)),
            (coverage.is_some(), PROGRAM_METADATA_VERSION, coverage.map(|v|v.object_id)),
            (snapshot.is_some(), if coverage.is_some(){COVERAGE_METADATA_VERSION}else{PROGRAM_METADATA_VERSION}, snapshot.map(|v|v.object_id)),
            (csharp.is_some(), METADATA_VERSION, csharp.map(|v|v.object_id)),
            (headers.is_some(), if snapshot.is_some(){TYPESCRIPT_SNAPSHOT_METADATA_VERSION}else if coverage.is_some(){COVERAGE_METADATA_VERSION}else{PROGRAM_METADATA_VERSION}, headers.map(|v|v.object_id)),
        ] {
            if !present {continue;}
            let stage_program=if stage==METADATA_VERSION {None}else{program};
            let stage_coverage=if stage==COVERAGE_METADATA_VERSION || stage==TYPESCRIPT_SNAPSHOT_METADATA_VERSION {coverage}else{None};
            let stage_snapshot=if stage==TYPESCRIPT_SNAPSHOT_METADATA_VERSION {snapshot}else{None};
            let stage_id=stage_identity(common,stage,stage_program,stage_coverage,stage_snapshot,checkpoint)?;
            let mut count=base_unique + usize::from(!marked(members,bitmap,&stage_id,checkpoint)?);
            for i in 0..previous_len { if let Some(id)=previous[i] {
                if id != stage_id && !marked(members,bitmap,&id,checkpoint)? && !previous[..i].contains(&Some(id)) {count+=1;}
            }}
            if count>=MAX_MEMBER_IDS {return Err(CompilerEnvelopeError::MemberCount.into());}
            if previous_len < previous.len() {previous[previous_len]=optional; previous_len+=1;}
        }
        let mut unique=base_unique;
        if bitmap_mark(members,bitmap,&metadata_id,checkpoint)? { unique+=1; }
        for optional in [program.map(|v|v.object_id),coverage.map(|v|v.object_id),snapshot.map(|v|v.object_id),csharp.map(|v|v.object_id),headers.map(|v|v.object_id)].into_iter().flatten() {
            if bitmap_mark(members,bitmap,&optional,checkpoint)? {unique+=1;}
        }
        if unique!=member_count {return Err(CompilerEnvelopeError::MetadataMismatch.into());}
        let _ = format;
        Ok(Self { binding_identity, semantic_catalog_root: versioned.as_ref().map(|v|*v.catalog_root().as_bytes()), native_program_sources: program, native_headers: headers, image_rows, versioned })
    }
    pub(super) fn visit_image_ids<E>(&self, visit: &mut impl FnMut([u8;32])->Result<(),E>) -> Result<(),E> {
        for row in self.image_rows.chunks_exact(72) { let mut id=[0;32]; id.copy_from_slice(&row[4..36]); visit(id)?; } Ok(())
    }
    pub(super) fn versioned(&self) -> Option<&BorrowedVersionedMetadata<'a>> { self.versioned.as_ref() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CompilerPublicationMetadata {
        let image_identity = *SemanticImageIdentity::from_encoded_bytes(b"borrowed metadata image").as_ref();
        let image = CompilerImageMember { artifact_ordinal: 0, object_id: [0x45;32], semantic_image_identity: image_identity, byte_length: 7 };
        let mut manifest = vec![0u8; SEMANTIC_MANIFEST_HEADER_BYTES + SEMANTIC_MANIFEST_ENTRY_BYTES];
        manifest[..8].copy_from_slice(SEMANTIC_MANIFEST_MAGIC);
        manifest[8..10].copy_from_slice(&CompilationManifestFormat::SemanticV2.wire_version().to_le_bytes());
        manifest[12..16].copy_from_slice(&1u32.to_le_bytes());
        let id_at = SEMANTIC_MANIFEST_HEADER_BYTES + SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET;
        let len_at = SEMANTIC_MANIFEST_HEADER_BYTES + SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET;
        manifest[id_at..id_at+32].copy_from_slice(&image_identity);
        manifest[len_at..len_at+4].copy_from_slice(&7u32.to_le_bytes());
        let manifest_id = ManifestIdentity::from_encoded_bytes(&manifest);
        let root = GenerationId::from_canonical_bytes(b"borrowed compiler root");
        let dependencies = ContentId::<DependencySetDomain>::from_canonical_bytes(b"borrowed dependency set");
        let mut binding = [0u8; COMPILATION_BINDING_BYTES];
        binding[..8].copy_from_slice(b"NUDXCPB\0"); binding[8..10].copy_from_slice(&1u16.to_le_bytes());
        binding[12..44].copy_from_slice(root.as_ref()); binding[44..76].copy_from_slice(dependencies.as_ref());
        binding[76..108].copy_from_slice(manifest_id.as_ref());
        CompilerPublicationMetadata::new(&manifest, binding, vec![image]).unwrap()
    }

    #[test]
    fn borrowed_v9_optional_combinations_and_cancellation() {
        let program = CompilerProgramSourceMember { object_id: [0x11;32], program: [0x22;32], byte_length: 8 };
        let coverage = CompilerSourceCoverageMember { object_id: [0x33;32], identity: [0x44;32], byte_length: 8 };
        let snapshot = CompilerTypeScriptSnapshotMember { object_id: [0x55;32], byte_length: 8 };
        let headers = CompilerNativeHeadersMember { object_id: [0x77;32], byte_length: 8 };
        for has_coverage in [false,true] { for has_snapshot in [false,true] {
            let mut metadata = base().with_native_program_sources(program).unwrap();
            if has_coverage { metadata = metadata.with_source_coverage(coverage).unwrap(); }
            if has_snapshot { metadata = metadata.with_typescript_snapshot_witness(snapshot).unwrap(); }
            metadata = metadata.with_native_headers(headers).unwrap();
            let bytes = metadata.canonical_bytes();
            let ids = metadata_member_ids_checked(&metadata).unwrap();
            let mut members = Vec::new(); for id in &ids { members.extend_from_slice(id); }
            let mut bitmap = vec![0u8; (MAX_MEMBER_IDS + 7)/8];
            let mut calls = 0usize;
            let mut result = None;
            let allocation = allocation_counter::measure(|| {
                result = Some(BorrowedCompilerMetadata::parse(&bytes, metadata.object_id(), &members, &mut bitmap, &mut |_| { calls += 1; Ok::<(), ()>(()) }).unwrap());
            });
            assert_eq!(allocation.bytes_max, 0);
            let view = result.unwrap();
            assert_eq!(view.binding_identity, *metadata.binding_identity());
            assert_eq!(view.native_program_sources, Some(program));
            assert_eq!(view.native_headers, Some(headers));
            assert_eq!(CompilerPublicationMetadata::decode(&bytes).unwrap(), metadata);
            for cutoff in [1, calls/2, calls.saturating_sub(1)] {
                let mut seen = 0usize;
                let stopped = BorrowedCompilerMetadata::parse(&bytes, metadata.object_id(), &members, &mut bitmap, &mut |_| { seen += 1; if seen == cutoff { Err(()) } else { Ok(()) } });
                assert!(matches!(stopped, Err(BorrowedMetadataError::Stopped(()))));
            }
            let mut corrupt = bytes.clone(); corrupt[METADATA_MAGIC.len()+2] ^= 0x80;
            assert!(BorrowedCompilerMetadata::parse(&corrupt, metadata.object_id(), &members, &mut bitmap, &mut |_| Ok::<(), ()>(())).is_err());
            let mut incomplete = members.clone(); incomplete.truncate(incomplete.len()-32);
            assert!(BorrowedCompilerMetadata::parse(&bytes, metadata.object_id(), &incomplete, &mut bitmap, &mut |_| Ok::<(), ()>(())).is_err());
        }}
    }
}
