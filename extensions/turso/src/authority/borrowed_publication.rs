//! Bounded selected-publication wire transport for replay consumers.
//!
//! Physical and selected-closure checks here do not grant semantic Program,
//! image, source or header authority. Consumers must canonically admit each
//! borrowed payload and bind it to their exact replay state before using it.
use super::*;

/// Borrowed claims from one already-selected immutable compiler closure.
/// Program/image bytes still need their respective canonical semantic decoders.
pub enum SelectedPublicationWire<'a> {
    Program { member: CompilerProgramSourceMember, schema: SchemaIdentity, bytes: &'a [u8] },
    Image { member: CompilerImageMember, bytes: &'a [u8] },
    Headers { member: CompilerNativeHeadersMember, bytes: &'a [u8] },
    Lines { member: CompilerNativeLinesMember, bytes: &'a [u8] },
}

/// Verifies the selected envelope, exact metadata/member inventory and physical
/// object identities under one GC pin and one caller-owned ledger, then lends
/// each Program/image/header/line wire to `visit`. The borrowed slices cannot escape the
/// callback; any retained copies require a separate ledger reservation.
///
/// The callback must separately reserve and validate every retained Program,
/// image, source and auxiliary allocation against `budget` before allocating;
/// it must use the live request's cancellation/deadline for semantic decoding.
/// The callback must build a detached plan and discard it on any error; later
/// objects have not yet been authenticated when an earlier callback executes.
/// A closure-membership work token represents one bounded worst-shape node,
/// while payload scan tokens represent bounded chunks; they are not byte counts.
/// Successful return means wire
/// transport only; no semantic decode or source authority is asserted here.
/// `max_payload_bytes` and `max_images` apply to declared Program+image
/// inventory before any of those payloads are read.
pub fn visit_selected_publication_wire<F>(
    store: &FileStore, selected: &SelectedGeneration, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool, max_payload_bytes: usize, max_images: usize, mut visit: F,
) -> Result<(), String>
where F: for<'a> FnMut(SelectedPublicationWire<'a>, &mut SelectedNativeHeaderBudget) -> Result<(), String> {
    let frame_before=budget.memory;
    header_checkpoint(&budget.work,cancelled).map_err(|e|format!("selected publication stopped: {e:?}"))?;
    let pin=store.pin_garbage_collection_bounded(&mut budget.memory, |_|header_checkpoint(&budget.work,cancelled)).map_err(|e|format!("pin selected compiler closure: {e:?}"))?;
    let closure=backend_store::ClosureId::from_bytes(*selected.closure_id());
    let count=header_member_bounded(store,&pin,closure,*selected.candidate_id(),budget,cancelled)?;
    if count==0 || count>(MAX_MEMBER_IDS as u64+1){return Err("selected compiler closure count exceeds limit".into());}
    let envelope_max=MAGIC.len()+2+5*4+4*4096+MAX_PROFILE_BYTES+1+16+8+32*6+1+4+MAX_MEMBER_IDS*32;
    let (envelope_wire,schema,key)=header_object_bounded(store,&pin,*selected.candidate_id(),&[COMPILER_PUBLICATION_ENVELOPE_SCHEMA],envelope_max,budget,cancelled)?;
    verified_payload_key_checkpointed(schema,&envelope_wire,&key,budget,cancelled)?;
    let envelope=borrowed_selected_envelope(&envelope_wire,selected,selected.candidate_id(),&||header_checkpoint(&budget.work,cancelled).map_err(|_|CompilerEnvelopeError::Stopped)).map_err(|e|e.to_string())?;
    if count!=u64::try_from(envelope.member_wire.len()/32).map_err(|_|"member count overflow")?+1{return Err("selected compiler closure count differs".into());}
    let metadata_id=envelope.metadata_id;
    header_member_bounded(store,&pin,closure,metadata_id,budget,cancelled)?;
    let metadata_max=MAX_MANIFEST_BYTES+64*1024*1024+MAX_MEMBER_IDS*72+1024;
    let schemas=[COMPILER_PUBLICATION_METADATA_SCHEMA,COMPILER_PROGRAM_METADATA_SCHEMA,COMPILER_COVERAGE_METADATA_SCHEMA,COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA,COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA,COMPILER_NATIVE_HEADERS_METADATA_SCHEMA,COMPILER_NATIVE_LINES_METADATA_SCHEMA];
    let (metadata_wire,schema,key)=header_object_bounded(store,&pin,metadata_id,&schemas,metadata_max,budget,cancelled)?;
    verified_payload_key_checkpointed(schema,&metadata_wire,&key,budget,cancelled)?;
    let version_at=METADATA_MAGIC.len();
    let raw=metadata_wire.get(version_at..version_at+2).ok_or("truncated metadata version")?;
    let version=u16::from_le_bytes([raw[0],raw[1]]);
    let expected=match version { METADATA_VERSION=>COMPILER_PUBLICATION_METADATA_SCHEMA,PROGRAM_METADATA_VERSION=>COMPILER_PROGRAM_METADATA_SCHEMA,COVERAGE_METADATA_VERSION=>COMPILER_COVERAGE_METADATA_SCHEMA,TYPESCRIPT_SNAPSHOT_METADATA_VERSION=>COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA,CSHARP_EVIDENCE_METADATA_VERSION=>COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA,NATIVE_HEADERS_METADATA_VERSION=>COMPILER_NATIVE_HEADERS_METADATA_SCHEMA,NATIVE_LINES_METADATA_VERSION=>COMPILER_NATIVE_LINES_METADATA_SCHEMA,_=>return Err("invalid compiler metadata version".into()) };
    if schema!=expected{return Err("compiler metadata schema differs".into());}
    let bitmap_len=(MAX_MEMBER_IDS+7)/8;
    let mut bitmap=backend_store::try_allocate_exact_zeroed_bytes_budgeted(bitmap_len,&mut budget.memory).map_err(|e|format!("allocate metadata bitmap: {e:?}"))?;
    let metadata=borrowed_metadata::BorrowedCompilerMetadata::parse(&metadata_wire,metadata_id,envelope.member_wire,&mut bitmap,&mut |_|header_checkpoint(&budget.work,cancelled)).map_err(|e|format!("selected metadata validation: {e:?}"))?;
    if envelope.logical_generation!=metadata.binding_identity || envelope.semantic_root!=metadata.semantic_catalog_root{return Err("selected compiler metadata differs from envelope".into());}
    drop(bitmap);budget.memory=budget.memory.checked_add(bitmap_len).ok_or("metadata bitmap refund overflow")?;
    if metadata.image_count()>max_images{return Err("selected image count exceeds residence limit".into());}
    let mut payload_total=metadata.native_program_sources.map_or(0usize,|member|member.byte_length() as usize);
    let mut images=metadata.images();
    loop {header_checkpoint(&budget.work,cancelled).map_err(|e|format!("selected inventory stopped: {e:?}"))?;let Some(image)=images.next() else {break;};
        payload_total=payload_total.checked_add(image.byte_length() as usize).ok_or("selected payload length overflow")?;
        if payload_total>max_payload_bytes{return Err("selected payload exceeds residence limit".into());}
    }
    drop(images);
    if payload_total>max_payload_bytes{return Err("selected payload exceeds residence limit".into());}
    for id in envelope.members(){if header_member_bounded(store,&pin,closure,id,budget,cancelled)?!=count{return Err("selected closure count changed".into());}}
    let frame_charge=frame_before.checked_sub(budget.memory).ok_or("selected frame charge underflow")?;
    if let Some(member)=metadata.native_program_sources {
        let before=budget.memory;
        let (wire,schema,key)=header_object_bounded(store,&pin,*member.object_id(),&[NATIVE_PROGRAM_SOURCE_MANIFEST_SCHEMA,NATIVE_PROGRAM_FOREST_SCHEMA],member.byte_length() as usize,budget,cancelled)?;
        let temporary=before.checked_sub(budget.memory).ok_or("program temporary underflow")?;
        if wire.len()!=member.byte_length() as usize{return Err("selected Program extent differs".into());}
        verified_payload_key_checkpointed(schema,&wire,&key,budget,cancelled)?;
        visit(SelectedPublicationWire::Program{member,schema,bytes:&wire},budget)?;
        drop(wire);budget.memory=budget.memory.checked_add(temporary).ok_or("program refund overflow")?;
    }
    let mut images=metadata.images();
    loop {header_checkpoint(&budget.work,cancelled).map_err(|e|format!("selected image scan stopped: {e:?}"))?;let Some(member)=images.next() else {break;};
        let before=budget.memory;
        let (wire,schema,key)=header_object_bounded(store,&pin,*member.object_id(),&[COMPILER_SEMANTIC_IMAGE_SCHEMA],member.byte_length() as usize,budget,cancelled)?;
        let temporary=before.checked_sub(budget.memory).ok_or("image temporary underflow")?;
        if wire.len()!=member.byte_length() as usize{return Err("selected image extent differs".into());}
        verified_payload_key_checkpointed(schema,&wire,&key,budget,cancelled)?;
        let mut identity=backend_version::ArtifactHasher::<IrSemanticImageEncoding,IrSemanticImageDomain>::new();
        for chunk in wire.chunks(64*1024){header_checkpoint(&budget.work,cancelled).map_err(|e|format!("selected image identity stopped: {e:?}"))?;identity.write_chunk(chunk);}
        if identity.finalize().as_ref()!=member.semantic_image_identity(){return Err("selected image identity differs".into());}
        visit(SelectedPublicationWire::Image{member,bytes:&wire},budget)?;
        drop(wire);budget.memory=budget.memory.checked_add(temporary).ok_or("image refund overflow")?;
    }
    drop(images);
    if let Some(member)=metadata.native_headers {
        let before=budget.memory;
        let (wire,schema,key)=header_object_bounded(store,&pin,*member.object_id(),&[COMPILER_NATIVE_HEADERS_SCHEMA],member.byte_length() as usize,budget,cancelled)?;
        let temporary=before.checked_sub(budget.memory).ok_or("header temporary underflow")?;
        if wire.len()!=member.byte_length() as usize{return Err("selected header extent differs".into());}
        verified_payload_key_checkpointed(schema,&wire,&key,budget,cancelled)?;
        backend_semantic::native_ts_headers::NativeTsHeaderAggregateView::parse_canonical_checkpointed(&wire,&mut |_|header_checkpoint(&budget.work,cancelled)).map_err(|e|format!("selected header scan: {e:?}"))?;
        visit(SelectedPublicationWire::Headers{member,bytes:&wire},budget)?;
        drop(wire);budget.memory=budget.memory.checked_add(temporary).ok_or("header refund overflow")?;
    }
    if let Some(member)=metadata.native_lines {
        let before=budget.memory;
        let (wire,schema,key)=header_object_bounded(store,&pin,*member.object_id(),&[COMPILER_NATIVE_LINES_SCHEMA],member.byte_length() as usize,budget,cancelled)?;
        let temporary=before.checked_sub(budget.memory).ok_or("line-map temporary underflow")?;
        if wire.len()!=member.byte_length() as usize{return Err("selected line-map extent differs".into());}
        verified_payload_key_checkpointed(schema,&wire,&key,budget,cancelled)?;
        backend_semantic::native_source_lines::NativeSourceLineAggregateView::parse(&wire,&mut |_|header_checkpoint(&budget.work,cancelled)).map_err(|e|format!("selected line-map scan: {e:?}"))?;
        visit(SelectedPublicationWire::Lines{member,bytes:&wire},budget)?;
        drop(wire);budget.memory=budget.memory.checked_add(temporary).ok_or("line-map refund overflow")?;
    }
    drop(metadata);drop(metadata_wire);drop(envelope);drop(envelope_wire);drop(pin);
    budget.memory=budget.memory.checked_add(frame_charge).ok_or("selected frame refund overflow")?;
    Ok(())
}
