// Durable indexed mark/sweep for commit ancestry.
use super::catalog::*;
use super::codec::*;
use super::provenance::*;
use super::*;
pub(super) fn history_gc_state_path(target_root: &Path) -> PathBuf {
    target_root.join("history").join("gc.state")
}

pub(super) fn encode_history_gc_state(state: HistoryGcState) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_GC_STATE_BYTES - 32);
    writer.header(HISTORY_GC_STATE_TAG)?;
    writer.fixed(&state.refs_digest)?;
    writer.u8(match state.phase {
        HistoryGcPhase::Mark => 1,
        HistoryGcPhase::Sweep => 2,
        HistoryGcPhase::Complete => 3,
    })?;
    writer.u64(state.tombstone_offset)?;
    writer.u64(state.sweep_offset)?;
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

pub(super) fn decode_history_gc_state(bytes: &[u8]) -> Result<HistoryGcState, String> {
    let body = checked_body(bytes, MAX_HISTORY_GC_STATE_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_GC_STATE_TAG)?;
    let refs_digest = reader.fixed()?;
    let phase = match reader.u8()? {
        1 => HistoryGcPhase::Mark,
        2 => HistoryGcPhase::Sweep,
        3 => HistoryGcPhase::Complete,
        _ => return Err("semantic history GC phase is invalid".to_owned()),
    };
    let tombstone_offset = reader.u64()?;
    let sweep_offset = reader.u64()?;
    reader.finish()?;
    if tombstone_offset % HISTORY_INDEX_ENTRY_BYTES != 0
        || sweep_offset % HISTORY_INDEX_ENTRY_BYTES != 0
        || (phase == HistoryGcPhase::Mark && sweep_offset != 0)
    {
        return Err("semantic history GC cursor is invalid".to_owned());
    }
    Ok(HistoryGcState {
        refs_digest,
        phase,
        tombstone_offset,
        sweep_offset,
    })
}

pub(crate) fn read_history_gc_state(target_root: &Path) -> Result<Option<HistoryGcState>, String> {
    read_optional_bounded(
        &history_gc_state_path(target_root),
        MAX_HISTORY_GC_STATE_BYTES,
    )?
    .as_deref()
    .map(decode_history_gc_state)
    .transpose()
}

pub(super) fn write_history_gc_state(
    target_root: &Path,
    state: HistoryGcState,
) -> Result<(), String> {
    let bytes = encode_history_gc_state(state)?;
    backend_platform::durable::write_private_atomic(&history_gc_state_path(target_root), &bytes)
        .map_err(display_io)
}

pub(crate) fn history_gc_epoch_root(target_root: &Path, digest: &[u8; 32]) -> PathBuf {
    target_root.join("history").join("gc").join(hex(digest))
}

pub(super) fn ensure_history_gc_epoch(
    target_root: &Path,
    digest: &[u8; 32],
) -> Result<PathBuf, String> {
    let epoch_root = history_gc_epoch_root(target_root, digest);
    create_private_directory(&epoch_root)?;
    set_private_directory(&epoch_root)?;
    for root in ["queue", "marks"] {
        let class_root = epoch_root.join(root);
        create_private_directory(&class_root)?;
        set_private_directory(&class_root)?;
        for class in ["live", "candidate"] {
            let path = class_root.join(class);
            create_private_directory(&path)?;
            set_private_directory(&path)?;
        }
    }
    Ok(epoch_root)
}

pub(super) fn marker_path(directory: &Path, identity: HistoryCommitId, suffix: &str) -> PathBuf {
    directory.join(format!("{}.{}", hex(identity.as_bytes()), suffix))
}

pub(super) fn ensure_marker(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_regular_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(path, &[]).map_err(display_io)
        }
        Err(error) => Err(display_io(error)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryReachabilityClass {
    Live,
    Candidate,
}

impl HistoryReachabilityClass {
    const fn directory(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Candidate => "candidate",
        }
    }
}

pub(super) fn history_gc_enqueue(
    epoch_root: &Path,
    identity: HistoryCommitId,
    class: HistoryReachabilityClass,
) -> Result<(), String> {
    ensure_marker(&marker_path(
        &epoch_root.join("queue").join(class.directory()),
        identity,
        "todo",
    ))
}

pub(super) fn initialize_history_gc(
    target_root: &Path,
    catalog: &HistoryRefCatalog,
    digest: [u8; 32],
) -> Result<HistoryGcState, String> {
    let epoch_root = ensure_history_gc_epoch(target_root, &digest)?;
    for reference in &catalog.refs {
        history_gc_enqueue(
            &epoch_root,
            reference.commit,
            HistoryReachabilityClass::Live,
        )?;
    }
    let state = HistoryGcState {
        refs_digest: digest,
        phase: HistoryGcPhase::Mark,
        tombstone_offset: 0,
        sweep_offset: 0,
    };
    write_history_gc_state(target_root, state)?;
    Ok(state)
}

pub(super) fn first_history_gc_todo(
    epoch_root: &Path,
) -> Result<Option<(PathBuf, HistoryCommitId, HistoryReachabilityClass)>, String> {
    for class in [
        HistoryReachabilityClass::Live,
        HistoryReachabilityClass::Candidate,
    ] {
        let queue = epoch_root.join("queue").join(class.directory());
        for entry in fs::read_dir(&queue).map_err(display_io)? {
            let entry = entry.map_err(display_io)?;
            let path = entry.path();
            ensure_regular_file(&path)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "semantic history GC queue name is not UTF-8".to_owned())?;
            let stem = name
                .strip_suffix(".todo")
                .ok_or_else(|| "semantic history GC queue contains an unknown member".to_owned())?;
            if stem.len() != 64 {
                return Err("semantic history GC queue identity is malformed".to_owned());
            }
            return Ok(Some((
                path,
                HistoryCommitId(decode_hex_digest(stem)?),
                class,
            )));
        }
    }
    Ok(None)
}

pub(crate) fn history_gc_marked(
    epoch_root: &Path,
    identity: HistoryCommitId,
    class: HistoryReachabilityClass,
) -> Result<bool, String> {
    let path = marker_path(
        &epoch_root.join("marks").join(class.directory()),
        identity,
        "mark",
    );
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            ensure_regular_file(&path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(display_io(error)),
    }
}

pub(super) fn repair_history_index_tail(
    index_path: &Path,
    index: &mut File,
) -> Result<u64, String> {
    ensure_regular_file(index_path)?;
    let length = index.metadata().map_err(display_io)?.len();
    let tail = length % HISTORY_INDEX_ENTRY_BYTES;
    if tail != 0 {
        index.set_len(length - tail).map_err(display_io)?;
        index.sync_all().map_err(display_io)?;
    }
    Ok(length - tail)
}

pub(crate) fn history_index_id_at(
    index: &mut File,
    offset: u64,
    domain: &[u8],
) -> Result<HistoryCommitId, String> {
    index.seek(SeekFrom::Start(offset)).map_err(display_io)?;
    let mut entry = [0; HISTORY_INDEX_ENTRY_BYTES as usize];
    index.read_exact(&mut entry).map_err(display_io)?;
    let identity = HistoryCommitId(
        entry[..32]
            .try_into()
            .map_err(|_| "semantic history index identity is truncated".to_owned())?,
    );
    if entry[32..] != history_index_checksum(identity, domain) {
        return Err("semantic history commit index checksum is invalid".to_owned());
    }
    Ok(identity)
}

pub(super) fn advance_history_gc(
    target_root: &Path,
    target: &SemanticTargetKey,
) -> Result<HistoryGcProgress, String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok(HistoryGcProgress {
            processed_records: 0,
            complete: true,
            stats: HistoryGcStats::default(),
        });
    }
    let (catalog, digest) = read_history_catalog_snapshot(target_root)?;
    validate_catalog_tips(target_root, target, &catalog)?;
    let state_path = history_gc_state_path(target_root);
    let mut state = match read_history_gc_state(target_root)? {
        Some(state) if state.refs_digest == digest => state,
        _ => initialize_history_gc(target_root, &catalog, digest)?,
    };
    let epoch_root = ensure_history_gc_epoch(target_root, &digest)?;
    let mut processed = 0;
    loop {
        match state.phase {
            HistoryGcPhase::Mark => {
                if let Some((todo_path, identity, class)) = first_history_gc_todo(&epoch_root)? {
                    if history_gc_marked(&epoch_root, identity, class)? {
                        remove_file(&todo_path)?;
                        processed += 1;
                    } else {
                        let commits_root = history_root.join("commits");
                        let record = validate_history_commit_node(
                            target_root,
                            target,
                            &commits_root,
                            identity,
                        )?;
                        for parent in &record.parents {
                            history_gc_enqueue(&epoch_root, *parent, class)?;
                        }
                        ensure_marker(&marker_path(
                            &epoch_root.join("marks").join(class.directory()),
                            identity,
                            "mark",
                        ))?;
                        remove_file(&todo_path)?;
                        processed += 1;
                    }
                } else {
                    let tombstones_path = history_root.join("tombstones.index");
                    ensure_regular_file(&tombstones_path)?;
                    let mut tombstones = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&tombstones_path)
                        .map_err(display_io)?;
                    let length = repair_history_index_tail(&tombstones_path, &mut tombstones)?;
                    if state.tombstone_offset > length {
                        return Err(
                            "semantic history GC cursor exceeds its tombstone index".to_owned()
                        );
                    }
                    if state.tombstone_offset < length {
                        let identity = history_index_id_at(
                            &mut tombstones,
                            state.tombstone_offset,
                            HISTORY_TOMBSTONE_DOMAIN,
                        )?;
                        history_gc_enqueue(
                            &epoch_root,
                            identity,
                            HistoryReachabilityClass::Candidate,
                        )?;
                        state.tombstone_offset = state
                            .tombstone_offset
                            .checked_add(HISTORY_INDEX_ENTRY_BYTES)
                            .ok_or_else(|| {
                                "semantic history GC tombstone cursor overflows".to_owned()
                            })?;
                        processed += 1;
                    } else {
                        state.phase = HistoryGcPhase::Sweep;
                        state.sweep_offset = 0;
                        write_history_gc_state(target_root, state)?;
                        continue;
                    }
                }
                if processed >= MAX_HISTORY_GC_BATCH_RECORDS {
                    write_history_gc_state(target_root, state)?;
                    return Ok(HistoryGcProgress {
                        processed_records: processed,
                        complete: false,
                        stats: HistoryGcStats::default(),
                    });
                }
            }
            HistoryGcPhase::Sweep => {
                let (current_catalog, current_digest) = read_history_catalog_snapshot(target_root)?;
                if current_digest != state.refs_digest {
                    let restarted =
                        initialize_history_gc(target_root, &current_catalog, current_digest)?;
                    return Ok(HistoryGcProgress {
                        processed_records: processed,
                        complete: restarted.phase == HistoryGcPhase::Complete,
                        stats: HistoryGcStats::default(),
                    });
                }
                validate_catalog_tips(target_root, target, &current_catalog)?;
                let tombstones_path = history_root.join("tombstones.index");
                ensure_regular_file(&tombstones_path)?;
                let mut tombstones = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&tombstones_path)
                    .map_err(display_io)?;
                let tombstones_length =
                    repair_history_index_tail(&tombstones_path, &mut tombstones)?;
                if state.tombstone_offset > tombstones_length {
                    return Err("semantic history GC cursor exceeds its tombstone index".to_owned());
                }
                if state.tombstone_offset < tombstones_length {
                    state.phase = HistoryGcPhase::Mark;
                    write_history_gc_state(target_root, state)?;
                    continue;
                }
                let index_path = history_root.join("commit.index");
                ensure_regular_file(&index_path)?;
                let mut index = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&index_path)
                    .map_err(display_io)?;
                let length = repair_history_index_tail(&index_path, &mut index)?;
                if state.sweep_offset > length {
                    return Err("semantic history GC cursor exceeds its commit index".to_owned());
                }
                if state.sweep_offset == length {
                    state.phase = HistoryGcPhase::Complete;
                    write_history_gc_state(target_root, state)?;
                    return Ok(HistoryGcProgress {
                        processed_records: processed,
                        complete: true,
                        stats: HistoryGcStats::default(),
                    });
                }
                while state.sweep_offset < length && processed < MAX_HISTORY_GC_BATCH_RECORDS {
                    let identity =
                        history_index_id_at(&mut index, state.sweep_offset, HISTORY_INDEX_DOMAIN)?;
                    if history_gc_marked(
                        &epoch_root,
                        identity,
                        HistoryReachabilityClass::Candidate,
                    )? && !history_gc_marked(
                        &epoch_root,
                        identity,
                        HistoryReachabilityClass::Live,
                    )? {
                        remove_file(&history_commit_path(
                            &history_root.join("commits"),
                            identity,
                        ))?;
                        remove_file(&history_payload_root_path(target_root, identity))?;
                        super::v2::remove_typed_v2_locator_for_commit(target_root, identity)?;
                    }
                    state.sweep_offset = state
                        .sweep_offset
                        .checked_add(HISTORY_INDEX_ENTRY_BYTES)
                        .ok_or_else(|| "semantic history GC cursor overflows".to_owned())?;
                    processed += 1;
                }
                if state.sweep_offset == length {
                    state.phase = HistoryGcPhase::Complete;
                }
                write_history_gc_state(target_root, state)?;
                return Ok(HistoryGcProgress {
                    processed_records: processed,
                    complete: state.phase == HistoryGcPhase::Complete,
                    stats: HistoryGcStats::default(),
                });
            }
            HistoryGcPhase::Complete => {
                let tombstones_path = history_root.join("tombstones.index");
                ensure_regular_file(&tombstones_path)?;
                let mut tombstones = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&tombstones_path)
                    .map_err(display_io)?;
                let tombstones_length =
                    repair_history_index_tail(&tombstones_path, &mut tombstones)?;
                if state.tombstone_offset < tombstones_length {
                    state.phase = HistoryGcPhase::Mark;
                    write_history_gc_state(target_root, state)?;
                    continue;
                }
                let index_path = history_root.join("commit.index");
                ensure_regular_file(&index_path)?;
                let mut index = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&index_path)
                    .map_err(display_io)?;
                let length = repair_history_index_tail(&index_path, &mut index)?;
                if state.sweep_offset < length {
                    state.phase = HistoryGcPhase::Sweep;
                    write_history_gc_state(target_root, state)?;
                    continue;
                }
                return Ok(HistoryGcProgress {
                    processed_records: processed,
                    complete: true,
                    stats: HistoryGcStats::default(),
                });
            }
        }
    }
}
