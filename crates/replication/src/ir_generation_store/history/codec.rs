// Bounded, checksummed persistence codecs for history protocol records.
use super::gc::repair_history_index_tail;
use super::*;
pub(super) fn identify_history_record(
    mut record: HistoryCommitRecord,
) -> Result<(HistoryCommitRecord, HistoryCommitId), String> {
    let body = encode_history_body(&record)?;
    let identity = history_commit_identity(&body);
    record.identity = identity;
    Ok((record, identity))
}

pub(super) fn encode_history_body(record: &HistoryCommitRecord) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_COMMIT_BYTES - 32 - 32);
    writer.header(HISTORY_COMMIT_TAG)?;
    writer.target(&record.target)?;
    writer.u8(u8::try_from(record.parents.len())
        .map_err(|_| "semantic history parent count exceeds its bound".to_owned())?)?;
    for parent in &record.parents {
        writer.fixed(parent.as_bytes())?;
    }
    writer.fixed(&record.generation.0)?;
    record.generation_root.encode_root(&mut writer)?;
    writer.fixed(record.manifest_root.as_bytes())?;
    writer.stamp(record.stamp)?;
    writer.fixed(&record.provenance)?;
    writer.u32(record.first_parent_depth)?;
    writer.u8(u8::from(record.checkpoint))?;
    Ok(writer.finish())
}

pub(super) fn encode_history_commit(
    record: &HistoryCommitRecord,
    identity: HistoryCommitId,
) -> Result<Vec<u8>, String> {
    let body = encode_history_body(record)?;
    if history_commit_identity(&body) != identity || record.identity != identity {
        return Err("semantic history proposal identity is inconsistent".to_owned());
    }
    let mut output = Vec::with_capacity(32 + body.len() + 32);
    output.extend_from_slice(&identity.0);
    output.extend_from_slice(&body);
    let checksum = blake3::hash(&output);
    output.extend_from_slice(checksum.as_bytes());
    if output.len() > MAX_HISTORY_COMMIT_BYTES {
        return Err("semantic history commit exceeds its storage bound".to_owned());
    }
    Ok(output)
}

pub(super) fn decode_history_commit(bytes: &[u8]) -> Result<HistoryCommitRecord, String> {
    let body = checked_body(bytes, MAX_HISTORY_COMMIT_BYTES)?;
    if body.len() < 32 {
        return Err("semantic history commit is truncated".to_owned());
    }
    let identity = HistoryCommitId(
        body[..32]
            .try_into()
            .map_err(|_| "semantic history commit identity is truncated".to_owned())?,
    );
    let content = &body[32..];
    if history_commit_identity(content) != identity {
        return Err("semantic history commit identity does not match its record".to_owned());
    }
    let mut reader = Reader::new(content);
    reader.header(HISTORY_COMMIT_TAG)?;
    let target = reader.target()?;
    let parent_count = usize::from(reader.u8()?);
    if parent_count > MAX_HISTORY_PARENTS {
        return Err("semantic history commit exceeds its parent bound".to_owned());
    }
    if parent_count == 2 {
        return Err("semantic history merge payload closure is unsupported".to_owned());
    }
    let mut parents = Vec::with_capacity(parent_count);
    for _ in 0..parent_count {
        parents.push(HistoryCommitId(reader.fixed()?));
    }
    if parents.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("semantic history commit repeats a parent".to_owned());
    }
    let generation = LocalSemanticGenerationId(reader.fixed()?);
    let generation_root = HistoryGenerationRoot::decode_root(&mut reader)?;
    let manifest_root =
        backend_semantic::ir::SemanticManifestRoot::from_wire_claim(reader.fixed()?);
    let stamp = reader.stamp()?;
    let provenance = reader.fixed()?;
    let first_parent_depth = reader.u32()?;
    let checkpoint = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err("semantic history checkpoint marker is invalid".to_owned()),
    };
    reader.finish()?;
    let record = HistoryCommitRecord {
        identity,
        target,
        parents,
        generation,
        generation_root,
        manifest_root,
        stamp,
        provenance,
        first_parent_depth,
        checkpoint,
    };
    if encode_history_body(&record)? != content {
        return Err("semantic history commit is not canonically encoded".to_owned());
    }
    Ok(record)
}

pub(super) fn history_commit_identity(body: &[u8]) -> HistoryCommitId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HISTORY_COMMIT_DOMAIN);
    hasher.update(&(body.len() as u64).to_be_bytes());
    hasher.update(body);
    HistoryCommitId(*hasher.finalize().as_bytes())
}

pub(super) fn prepare_history_layout(target_root: &Path) -> Result<PathBuf, String> {
    create_private_directory(target_root)?;
    set_private_directory(target_root)?;
    backend_platform::durable::sync_parent(target_root).map_err(display_io)?;
    let history_root = target_root.join("history");
    let created = match fs::create_dir(&history_root) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_directory(&history_root)?;
            false
        }
        Err(error) => return Err(display_io(error)),
    };
    set_private_directory(&history_root)?;
    if created {
        backend_platform::durable::sync_parent(&history_root).map_err(display_io)?;
    }
    let commits_root = history_root.join("commits");
    create_private_directory(&commits_root)?;
    set_private_directory(&commits_root)?;
    backend_platform::durable::sync_parent(&commits_root).map_err(display_io)?;
    for directory in ["indexed", "gc"] {
        let path = history_root.join(directory);
        create_private_directory(&path)?;
        set_private_directory(&path)?;
    }
    let refs_path = history_root.join("refs.catalog");
    if created {
        backend_platform::durable::write_private_atomic(
            &refs_path,
            &encode_ref_catalog(&HistoryRefCatalog::empty())?,
        )
        .map_err(display_io)?;
        backend_platform::durable::write_private_atomic(&history_root.join("commit.index"), &[])
            .map_err(display_io)?;
        backend_platform::durable::write_private_atomic(
            &history_root.join("tombstones.index"),
            &[],
        )
        .map_err(display_io)?;
    } else if read_optional_bounded(&refs_path, MAX_HISTORY_REFS_BYTES)?.is_none() {
        return Err("semantic history refs catalog is missing".to_owned());
    } else {
        let index_path = history_root.join("commit.index");
        if !index_path.exists() {
            return Err("semantic history commit index is missing".to_owned());
        }
        ensure_regular_file(&index_path)?;
        let tombstones_path = history_root.join("tombstones.index");
        if !tombstones_path.exists() {
            return Err("semantic history tombstone index is missing".to_owned());
        }
        ensure_regular_file(&tombstones_path)?;
    }
    Ok(commits_root)
}

pub(super) fn encode_ref_catalog(catalog: &HistoryRefCatalog) -> Result<Vec<u8>, String> {
    if catalog.refs.len() > MAX_HISTORY_REFS
        || catalog
            .refs
            .windows(2)
            .any(|pair| ref_order(&pair[0], &pair[1]).is_ge())
    {
        return Err(
            "semantic history refs catalog is not canonical or exceeds its bound".to_owned(),
        );
    }
    let mut writer = Writer::new(MAX_HISTORY_REFS_BYTES - 32);
    writer.header(HISTORY_REFS_TAG)?;
    writer.u32(
        u32::try_from(catalog.refs.len())
            .map_err(|_| "semantic history ref count exceeds its bound".to_owned())?,
    )?;
    for reference in &catalog.refs {
        writer.u8(reference.kind.wire())?;
        writer.sized_bytes(
            reference.name.as_str().as_bytes(),
            MAX_HISTORY_REF_NAME_BYTES,
        )?;
        writer.fixed(reference.commit.as_bytes())?;
    }
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    if bytes.len() > MAX_HISTORY_REFS_BYTES {
        return Err("semantic history refs catalog exceeds its storage bound".to_owned());
    }
    Ok(bytes)
}

pub(super) fn decode_ref_catalog(bytes: &[u8]) -> Result<HistoryRefCatalog, String> {
    let body = checked_body(bytes, MAX_HISTORY_REFS_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_REFS_TAG)?;
    let count = usize::try_from(reader.u32()?)
        .map_err(|_| "semantic history ref count exceeds address space".to_owned())?;
    if count > MAX_HISTORY_REFS {
        return Err("semantic history refs catalog exceeds its entry bound".to_owned());
    }
    let mut refs = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = HistoryRefKind::from_wire(reader.u8()?)?;
        let name = std::str::from_utf8(reader.sized_bytes(MAX_HISTORY_REF_NAME_BYTES)?)
            .map_err(|_| "semantic history ref name is not UTF-8".to_owned())?;
        let name = HistoryRefName::new(name)?;
        let commit = HistoryCommitId(reader.fixed()?);
        refs.push(SelectedHistoryRef { kind, name, commit });
    }
    reader.finish()?;
    if refs
        .windows(2)
        .any(|pair| ref_order(&pair[0], &pair[1]).is_ge())
    {
        return Err("semantic history refs catalog is unsorted or duplicated".to_owned());
    }
    Ok(HistoryRefCatalog { refs })
}

pub(crate) fn load_history_commit(
    commits_root: &Path,
    identity: HistoryCommitId,
) -> Result<HistoryCommitRecord, String> {
    let path = history_commit_path(commits_root, identity);
    let bytes = read_optional_bounded(&path, MAX_HISTORY_COMMIT_BYTES)?
        .ok_or_else(|| "semantic history references a missing commit object".to_owned())?;
    let record = decode_history_commit(&bytes)?;
    if record.identity != identity {
        return Err("semantic history commit filename differs from its identity".to_owned());
    }
    Ok(record)
}

pub(crate) fn history_commit_path(commits_root: &Path, identity: HistoryCommitId) -> PathBuf {
    commits_root.join(format!("{}.commit", hex(&identity.0)))
}

pub(crate) fn history_payload_root_path(target_root: &Path, identity: HistoryCommitId) -> PathBuf {
    target_root
        .join("history")
        .join("payload-roots")
        .join(format!("{}.root", hex(identity.as_bytes())))
}

pub(super) fn write_history_payload_root(
    target_root: &Path,
    identity: HistoryCommitId,
    payload: AdmittedHistoryPayloadRoot,
) -> Result<(), String> {
    let directory = target_root.join("history").join("payload-roots");
    create_private_directory(&directory)?;
    set_private_directory(&directory)?;
    let bytes = encode_history_payload_root(identity, payload)?;
    let path = history_payload_root_path(target_root, identity);
    match fs::read(&path) {
        Ok(existing) if existing == bytes => Ok(()),
        Ok(_) => Err("immutable semantic history payload root changed".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
        }
        Err(error) => Err(display_io(error)),
    }
}

pub(super) fn read_history_payload_root(
    target_root: &Path,
    identity: HistoryCommitId,
) -> Result<Option<HistoryPayloadRoot>, String> {
    let path = history_payload_root_path(target_root, identity);
    let Some(bytes) = read_optional_bounded(&path, MAX_HISTORY_PAYLOAD_RECORD_BYTES)? else {
        return Ok(None);
    };
    decode_history_payload_root(&bytes, identity).map(Some)
}

pub(super) fn encode_history_payload_root(
    identity: HistoryCommitId,
    payload: AdmittedHistoryPayloadRoot,
) -> Result<Vec<u8>, String> {
    encode_history_payload_root_claim_inner(
        identity,
        ArtifactClosureClaim::from_id(payload.closure),
    )
}

#[cfg(test)]
pub(in crate::ir_generation_store) fn encode_history_payload_root_claim(
    identity: HistoryCommitId,
    payload: HistoryPayloadRoot,
) -> Result<Vec<u8>, String> {
    encode_history_payload_root_claim_inner(identity, payload.closure)
}

fn encode_history_payload_root_claim_inner(
    identity: HistoryCommitId,
    closure: ArtifactClosureClaim,
) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_PAYLOAD_RECORD_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_PAYLOAD_ROOT_TAG)?;
    writer.fixed(identity.as_bytes())?;
    writer.fixed(closure.as_bytes())?;
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

pub(super) fn decode_history_payload_root(
    bytes: &[u8],
    expected_identity: HistoryCommitId,
) -> Result<HistoryPayloadRoot, String> {
    let body = checked_body(bytes, MAX_HISTORY_PAYLOAD_RECORD_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_PAYLOAD_ROOT_TAG)?;
    let identity = HistoryCommitId(reader.fixed()?);
    if identity != expected_identity {
        return Err("semantic history payload root names another commit".to_owned());
    }
    let closure = ArtifactClosureClaim::from_bytes(reader.fixed()?);
    reader.finish()?;
    Ok(HistoryPayloadRoot { closure })
}

pub(super) fn reserve_history_segment_mapping(
    history_root: &Path,
    mapping_root: &Path,
) -> Result<(), String> {
    let count_path = history_root.join("segment-map.count");
    let count = match read_optional_bounded(&count_path, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)? {
        Some(bytes) => decode_history_segment_map_count(&bytes)?,
        None => {
            let mut count = 0_u32;
            for entry in fs::read_dir(mapping_root).map_err(display_io)? {
                let entry = entry.map_err(display_io)?;
                let path = entry.path();
                ensure_regular_file(&path)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| "semantic history segment-map count overflows".to_owned())?;
                if count > MAX_HISTORY_SEGMENT_MAPPINGS {
                    return Err(
                        "semantic history segment-map retention limit was exceeded".to_owned()
                    );
                }
            }
            write_history_segment_map_count(&count_path, count)?;
            count
        }
    };
    let next = count
        .checked_add(1)
        .filter(|next| *next <= MAX_HISTORY_SEGMENT_MAPPINGS)
        .ok_or_else(|| {
            "semantic history segment-map retention limit reached; commit needs hydration or map compaction"
                .to_owned()
        })?;
    bump_history_segment_map_epoch(history_root)?;
    write_history_segment_map_count(&count_path, next)
}

pub(crate) fn write_history_segment_map_count(path: &Path, count: u32) -> Result<(), String> {
    let mut writer = Writer::new(MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_SEGMENT_MAP_COUNT_TAG)?;
    writer.u32(count)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    backend_platform::durable::write_private_atomic(path, &bytes).map_err(display_io)
}

pub(crate) fn decode_history_segment_map_count(bytes: &[u8]) -> Result<u32, String> {
    let body = checked_body(bytes, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_SEGMENT_MAP_COUNT_TAG)?;
    let count = reader.u32()?;
    reader.finish()?;
    Ok(count)
}

pub(super) fn bump_history_segment_map_epoch(history_root: &Path) -> Result<(), String> {
    let path = history_root.join("segment-map.epoch");
    let current = match read_optional_bounded(&path, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)? {
        Some(bytes) => {
            let body = checked_body(&bytes, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?;
            let mut reader = Reader::new(body);
            reader.header(HISTORY_SEGMENT_MAP_EPOCH_TAG)?;
            let epoch = reader.u64()?;
            reader.finish()?;
            epoch
        }
        None => 0,
    };
    let next = current
        .checked_add(1)
        .ok_or_else(|| "semantic history segment-map epoch overflows".to_owned())?;
    let mut writer = Writer::new(MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_SEGMENT_MAP_EPOCH_TAG)?;
    writer.u64(next)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
}

pub(super) fn bump_history_commit_epoch(history_root: &Path) -> Result<(), String> {
    let path = history_root.join("commits.epoch");
    let current = match read_optional_bounded(&path, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)? {
        Some(bytes) => {
            let body = checked_body(&bytes, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?;
            let mut reader = Reader::new(body);
            reader.header(HISTORY_COMMIT_EPOCH_TAG)?;
            let epoch = reader.u64()?;
            reader.finish()?;
            epoch
        }
        None => 0,
    };
    let next = current
        .checked_add(1)
        .ok_or_else(|| "semantic history commit epoch overflows".to_owned())?;
    let mut writer = Writer::new(MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_COMMIT_EPOCH_TAG)?;
    writer.u64(next)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
}

pub(in crate::ir_generation_store) fn encode_history_segment_mapping(
    segment: backend_semantic::ir::UntrustedSemanticSegmentId,
    object: ObjectId,
    byte_length: u64,
) -> Result<Vec<u8>, String> {
    if byte_length == 0 {
        return Err("semantic history segment mapping has an empty payload".to_owned());
    }
    let mut writer = Writer::new(MAX_HISTORY_SEGMENT_MAP_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_SEGMENT_MAP_TAG)?;
    writer.fixed(segment.as_bytes())?;
    writer.fixed(object.as_bytes())?;
    writer.u64(byte_length)?;
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

pub(crate) fn decode_history_segment_mapping(
    bytes: &[u8],
    expected_segment: backend_semantic::ir::UntrustedSemanticSegmentId,
) -> Result<(UntrustedObjectId, u64), String> {
    let body = checked_body(bytes, MAX_HISTORY_SEGMENT_MAP_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_SEGMENT_MAP_TAG)?;
    let segment: [u8; 32] = reader.fixed()?;
    if segment != *expected_segment.as_bytes() {
        return Err("semantic history segment-map filename differs from its claim".to_owned());
    }
    let object = UntrustedObjectId::from_bytes(reader.fixed()?);
    let byte_length = reader.u64()?;
    if byte_length == 0 {
        return Err("semantic history segment mapping has an empty payload".to_owned());
    }
    reader.finish()?;
    Ok((object, byte_length))
}

pub(super) fn append_commit_index(
    target_root: &Path,
    identity: HistoryCommitId,
) -> Result<(), String> {
    let history_root = target_root.join("history");
    recover_history_index_intent(target_root)?;
    let indexed_path = history_root
        .join("indexed")
        .join(format!("{}.indexed", hex(identity.as_bytes())));
    match fs::symlink_metadata(&indexed_path) {
        Ok(_) => {
            ensure_regular_file(&indexed_path)?;
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(display_io(error)),
    }
    let index_path = history_root.join("commit.index");
    ensure_regular_file(&index_path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&index_path)
        .map_err(display_io)?;
    let offset = repair_history_index_tail(&index_path, &mut index)?;
    let intent = HistoryIndexIntent { identity, offset };
    backend_platform::durable::write_private_atomic(
        &history_index_intent_path(target_root),
        &encode_history_index_intent(intent)?,
    )
    .map_err(display_io)?;
    append_history_index_entry(&index_path, identity, HISTORY_INDEX_DOMAIN)?;
    #[cfg(test)]
    super::trip_history_test_fault(super::HistoryTestFault::AfterHistoryIndex)?;
    backend_platform::durable::write_private_atomic(&indexed_path, &[]).map_err(display_io)?;
    remove_file(&history_index_intent_path(target_root))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct HistoryIndexIntent {
    identity: HistoryCommitId,
    offset: u64,
}

pub(crate) fn history_index_intent_path(target_root: &Path) -> PathBuf {
    target_root.join("history").join("commit.index.intent")
}

fn encode_history_index_intent(intent: HistoryIndexIntent) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_INDEX_INTENT_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_INDEX_INTENT_TAG)?;
    writer.fixed(intent.identity.as_bytes())?;
    writer.u64(intent.offset)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    Ok(bytes)
}

pub(super) fn decode_history_index_intent(bytes: &[u8]) -> Result<HistoryIndexIntent, String> {
    let body = checked_body(bytes, MAX_HISTORY_INDEX_INTENT_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_INDEX_INTENT_TAG)?;
    let intent = HistoryIndexIntent {
        identity: HistoryCommitId(reader.fixed()?),
        offset: reader.u64()?,
    };
    reader.finish()?;
    if intent.offset % HISTORY_INDEX_ENTRY_BYTES != 0 {
        return Err("semantic history index intent has a misaligned offset".to_owned());
    }
    Ok(intent)
}

pub(crate) fn recover_history_index_intent(target_root: &Path) -> Result<(), String> {
    let intent_path = history_index_intent_path(target_root);
    let Some(bytes) = read_optional_bounded(&intent_path, MAX_HISTORY_INDEX_INTENT_BYTES)? else {
        return Ok(());
    };
    let intent = decode_history_index_intent(&bytes)?;
    let index_path = target_root.join("history").join("commit.index");
    ensure_regular_file(&index_path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&index_path)
        .map_err(display_io)?;
    let length = repair_history_index_tail(&index_path, &mut index)?;
    let expected_end = intent
        .offset
        .checked_add(HISTORY_INDEX_ENTRY_BYTES)
        .ok_or_else(|| "semantic history index intent overflows".to_owned())?;
    if length == intent.offset {
        append_history_index_entry(&index_path, intent.identity, HISTORY_INDEX_DOMAIN)?;
    } else if length == expected_end {
        let observed = history_index_id_at(&mut index, intent.offset, HISTORY_INDEX_DOMAIN)?;
        if observed != intent.identity {
            return Err(
                "semantic history index intent conflicts with its durable entry".to_owned(),
            );
        }
    } else {
        return Err("semantic history index intent is inconsistent with the append log".to_owned());
    }
    let indexed_path = target_root
        .join("history")
        .join("indexed")
        .join(format!("{}.indexed", hex(intent.identity.as_bytes())));
    match fs::symlink_metadata(&indexed_path) {
        Ok(_) => ensure_regular_file(&indexed_path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&indexed_path, &[])
                .map_err(display_io)?;
        }
        Err(error) => return Err(display_io(error)),
    }
    remove_file(&intent_path)
}

pub(super) fn append_history_tombstone(
    target_root: &Path,
    identity: HistoryCommitId,
) -> Result<(), String> {
    append_history_index_entry(
        &target_root.join("history").join("tombstones.index"),
        identity,
        HISTORY_TOMBSTONE_DOMAIN,
    )
}

pub(crate) fn append_history_index_entry(
    index_path: &Path,
    identity: HistoryCommitId,
    domain: &[u8],
) -> Result<(), String> {
    ensure_regular_file(index_path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .write(true)
        .append(true)
        .open(index_path)
        .map_err(display_io)?;
    let length = index.metadata().map_err(display_io)?.len();
    let tail = length % HISTORY_INDEX_ENTRY_BYTES;
    if tail != 0 {
        index.set_len(length - tail).map_err(display_io)?;
        index.sync_all().map_err(display_io)?;
    }
    let mut entry = Vec::with_capacity(HISTORY_INDEX_ENTRY_BYTES as usize);
    entry.extend_from_slice(identity.as_bytes());
    entry.extend_from_slice(&history_index_checksum(identity, domain));
    index.write_all(&entry).map_err(display_io)?;
    index.sync_all().map_err(display_io)
}

pub(super) fn history_index_checksum(identity: HistoryCommitId, domain: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(identity.as_bytes());
    *hasher.finalize().as_bytes()
}
