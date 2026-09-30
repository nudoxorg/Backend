//! Bounded reclamation of semantic history bridge maps and generation records.
//!
//! The history catalog GC establishes the live commit marks. This module
//! consumes those marks and pages over the existing commit index and metadata
//! directories without building an in-memory archive-sized set. Each page is
//! persisted before returning, and unlink operations use a durable intent so
//! counters and the segment-map capacity sidecar recover together.

use std::{
    fs::{self, OpenOptions},
    io::Seek,
    path::{Path, PathBuf},
};

use backend_semantic::ir::UntrustedSemanticSegmentId;

use super::{
    HistoryCommitId, HistoryGcStats, HistoryReachabilityClass, LocalSemanticGenerationId,
    MAX_HISTORY_COMMIT_BYTES, MAX_HISTORY_INDEX_INTENT_BYTES, MAX_HISTORY_SEGMENT_MAP_BYTES,
    SemanticTargetKey, decode_history_index_intent, decode_history_segment_mapping,
    ensure_regular_file, history_commit_path, history_gc_epoch_root, history_gc_marked,
    history_index_id_at, history_index_intent_path, history_payload_root_path, load_history_commit,
    read_history_catalog_snapshot, read_history_gc_state, read_optional_bounded, remove_file,
    validate_commit_generation, write_history_segment_map_count,
};

const RETENTION_STATE_TAG: u8 = 10;
const RETENTION_DELETE_INTENT_TAG: u8 = 11;
const MAX_RETENTION_STATE_BYTES: usize = 512;
const MAX_RETENTION_DELETE_INTENT_BYTES: usize = 512;
const RETENTION_DELETE_INTENT_NAME_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetentionPhase {
    Mark,
    SweepCommits,
    ScanCommitObjects,
    ScanMaps,
    SweepMaps,
    ScanGenerations,
    SweepGenerations,
    CompactIndex,
    Cleanup,
    Complete,
}

impl RetentionPhase {
    const fn wire(self) -> u8 {
        match self {
            Self::Mark => 1,
            Self::SweepCommits => 2,
            Self::ScanMaps => 3,
            Self::SweepMaps => 4,
            Self::ScanGenerations => 5,
            Self::SweepGenerations => 6,
            Self::CompactIndex => 7,
            Self::Cleanup => 8,
            Self::Complete => 9,
            Self::ScanCommitObjects => 10,
        }
    }

    fn from_wire(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Mark),
            2 => Ok(Self::SweepCommits),
            3 => Ok(Self::ScanMaps),
            4 => Ok(Self::SweepMaps),
            5 => Ok(Self::ScanGenerations),
            6 => Ok(Self::SweepGenerations),
            7 => Ok(Self::CompactIndex),
            8 => Ok(Self::Cleanup),
            9 => Ok(Self::Complete),
            10 => Ok(Self::ScanCommitObjects),
            _ => Err("semantic history retention phase is invalid".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetentionState {
    input_digest: [u8; 32],
    phase: RetentionPhase,
    mark_offset: u64,
    commit_sweep_offset: u64,
    map_scan_offset: u64,
    map_scan_count: u64,
    generation_scan_offset: u64,
    compact_input_offset: u64,
    compact_output_offset: u64,
    stats: HistoryGcStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeleteKind {
    Map,
    Commit,
    Generation,
}

impl DeleteKind {
    const fn wire(self) -> u8 {
        match self {
            Self::Map => 1,
            Self::Commit => 2,
            Self::Generation => 3,
        }
    }

    fn from_wire(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Map),
            2 => Ok(Self::Commit),
            3 => Ok(Self::Generation),
            _ => Err("semantic history retention delete kind is invalid".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetentionDeleteIntent {
    kind: DeleteKind,
    name: String,
    byte_length: u64,
    old_reclaimed_count: u64,
    old_reclaimed_bytes: u64,
    old_map_count: Option<u32>,
}

pub(super) struct RetentionBatch {
    pub(super) processed_records: usize,
    pub(super) complete: bool,
    pub(super) stats: HistoryGcStats,
}

fn retention_state_path(target_root: &Path) -> PathBuf {
    target_root.join("history").join("retention.state")
}

fn retention_delete_intent_path(target_root: &Path) -> PathBuf {
    target_root.join("history").join("retention.delete.intent")
}

fn retention_work_root(target_root: &Path, digest: &[u8; 32]) -> PathBuf {
    target_root
        .join("history")
        .join("gc")
        .join(format!("retention-{}", super::hex(digest)))
}

fn ensure_work_layout(target_root: &Path, digest: &[u8; 32]) -> Result<PathBuf, String> {
    let root = retention_work_root(target_root, digest);
    super::create_private_directory(&root)?;
    for directory in [
        "commit-seen",
        "commit-output",
        "commit-candidates",
        "generation-live",
        "map-live",
        "map-candidates",
        "generation-candidates",
    ] {
        let path = root.join(directory);
        super::create_private_directory(&path)?;
        super::set_private_directory(&path)?;
    }
    Ok(root)
}

fn retention_input_digest(target_root: &Path) -> Result<[u8; 32], String> {
    let (_, refs_digest) = read_history_catalog_snapshot(target_root)?;
    let history_root = target_root.join("history");
    let head = read_optional_bounded(&target_root.join("HEAD"), super::super::MAX_HEAD_BYTES)?;
    let index_intent = read_optional_bounded(
        &history_index_intent_path(target_root),
        MAX_HISTORY_INDEX_INTENT_BYTES,
    )?;
    if let Some(bytes) = &index_intent {
        let _ = decode_history_index_intent(bytes)?;
    }
    let index_path = history_root.join("commit.index");
    let index_length = match fs::symlink_metadata(&index_path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata.len(),
        Ok(_) => return Err("semantic history commit index is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(super::display_io(error)),
    };
    let map_epoch = read_optional_bounded(
        &history_root.join("segment-map.epoch"),
        super::MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES,
    )?;
    let commit_epoch = read_optional_bounded(
        &history_root.join("commits.epoch"),
        super::MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES,
    )?;
    let record_epoch = read_optional_bounded(
        &target_root.join("records.epoch"),
        super::MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES,
    )?;
    if let Some(bytes) = &map_epoch {
        validate_epoch(bytes, super::HISTORY_SEGMENT_MAP_EPOCH_TAG)?;
    }
    if let Some(bytes) = &commit_epoch {
        validate_epoch(bytes, super::HISTORY_COMMIT_EPOCH_TAG)?;
    }
    if let Some(bytes) = &record_epoch {
        validate_epoch(bytes, super::super::RECORDS_EPOCH_TAG)?;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.history-retention-input.v2\0");
    hasher.update(&refs_digest);
    if let Some(head) = head {
        hasher.update(&(head.len() as u64).to_be_bytes());
        hasher.update(&head);
    } else {
        hasher.update(&0_u64.to_be_bytes());
    }
    if let Some(intent) = index_intent {
        hasher.update(&(intent.len() as u64).to_be_bytes());
        hasher.update(&intent);
    } else {
        hasher.update(&0_u64.to_be_bytes());
    }
    hasher.update(&index_length.to_be_bytes());
    for epoch in [map_epoch, commit_epoch, record_epoch] {
        if let Some(epoch) = epoch {
            hasher.update(&(epoch.len() as u64).to_be_bytes());
            hasher.update(&epoch);
        } else {
            hasher.update(&0_u64.to_be_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn validate_epoch(bytes: &[u8], tag: u8) -> Result<(), String> {
    let body = super::checked_body(bytes, super::MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?;
    let mut reader = super::Reader::new(body);
    reader.header(tag)?;
    let _ = reader.u64()?;
    reader.finish()
}

fn encode_state(state: RetentionState) -> Result<Vec<u8>, String> {
    let mut writer = super::Writer::new(MAX_RETENTION_STATE_BYTES - 32);
    writer.header(RETENTION_STATE_TAG)?;
    writer.fixed(&state.input_digest)?;
    writer.u8(state.phase.wire())?;
    writer.u64(state.mark_offset)?;
    writer.u64(state.commit_sweep_offset)?;
    writer.u64(state.map_scan_offset)?;
    writer.u64(state.map_scan_count)?;
    writer.u64(state.generation_scan_offset)?;
    writer.u64(state.compact_input_offset)?;
    writer.u64(state.compact_output_offset)?;
    encode_stats(&mut writer, state.stats)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    if bytes.len() > MAX_RETENTION_STATE_BYTES {
        return Err("semantic history retention state exceeds its storage bound".to_owned());
    }
    Ok(bytes)
}

fn decode_state(bytes: &[u8]) -> Result<RetentionState, String> {
    let body = super::checked_body(bytes, MAX_RETENTION_STATE_BYTES)?;
    let mut reader = super::Reader::new(body);
    reader.header(RETENTION_STATE_TAG)?;
    let input_digest = reader.fixed()?;
    let phase = RetentionPhase::from_wire(reader.u8()?)?;
    let mark_offset = reader.u64()?;
    let commit_sweep_offset = reader.u64()?;
    let map_scan_offset = reader.u64()?;
    let map_scan_count = reader.u64()?;
    let generation_scan_offset = reader.u64()?;
    let compact_input_offset = reader.u64()?;
    let compact_output_offset = reader.u64()?;
    let stats = decode_stats(&mut reader)?;
    reader.finish()?;
    let before_map_scan = matches!(
        phase,
        RetentionPhase::Mark | RetentionPhase::SweepCommits | RetentionPhase::ScanCommitObjects
    );
    let before_generation_scan = matches!(
        phase,
        RetentionPhase::Mark
            | RetentionPhase::SweepCommits
            | RetentionPhase::ScanCommitObjects
            | RetentionPhase::ScanMaps
            | RetentionPhase::SweepMaps
    );
    let before_compaction = matches!(
        phase,
        RetentionPhase::Mark
            | RetentionPhase::SweepCommits
            | RetentionPhase::ScanCommitObjects
            | RetentionPhase::ScanMaps
            | RetentionPhase::SweepMaps
            | RetentionPhase::ScanGenerations
            | RetentionPhase::SweepGenerations
    );
    if mark_offset % super::HISTORY_INDEX_ENTRY_BYTES != 0
        || commit_sweep_offset % super::HISTORY_INDEX_ENTRY_BYTES != 0
        || phase == RetentionPhase::Mark
            && (commit_sweep_offset != 0
                || map_scan_offset != 0
                || map_scan_count != 0
                || generation_scan_offset != 0)
        || phase == RetentionPhase::SweepCommits && map_scan_offset != 0
        || before_map_scan && map_scan_count != 0
        || before_generation_scan && generation_scan_offset != 0
        || !matches!(
            phase,
            RetentionPhase::CompactIndex | RetentionPhase::Cleanup | RetentionPhase::Complete
        ) && compact_input_offset != 0
        || before_compaction && compact_output_offset != 0
        || compact_input_offset % super::HISTORY_INDEX_ENTRY_BYTES != 0
        || compact_output_offset % super::HISTORY_INDEX_ENTRY_BYTES != 0
        || compact_output_offset > compact_input_offset
        || !before_map_scan && map_scan_count > map_scan_offset
        || phase == RetentionPhase::Complete
            && map_scan_offset < stats.live_maps.saturating_add(stats.reclaimed_maps)
    {
        return Err("semantic history retention cursors are invalid".to_owned());
    }
    Ok(RetentionState {
        input_digest,
        phase,
        mark_offset,
        commit_sweep_offset,
        map_scan_offset,
        map_scan_count,
        generation_scan_offset,
        compact_input_offset,
        compact_output_offset,
        stats,
    })
}

fn encode_stats(writer: &mut super::Writer, stats: HistoryGcStats) -> Result<(), String> {
    for value in [
        stats.live_maps,
        stats.reclaimed_maps,
        stats.live_map_bytes,
        stats.reclaimed_map_bytes,
        stats.live_commits,
        stats.reclaimed_commits,
        stats.live_commit_bytes,
        stats.reclaimed_commit_bytes,
        stats.live_generation_records,
        stats.reclaimed_generation_records,
        stats.live_generation_record_bytes,
        stats.reclaimed_generation_record_bytes,
    ] {
        writer.u64(value)?;
    }
    Ok(())
}

fn decode_stats(reader: &mut super::Reader<'_>) -> Result<HistoryGcStats, String> {
    Ok(HistoryGcStats {
        live_maps: reader.u64()?,
        reclaimed_maps: reader.u64()?,
        live_map_bytes: reader.u64()?,
        reclaimed_map_bytes: reader.u64()?,
        live_commits: reader.u64()?,
        reclaimed_commits: reader.u64()?,
        live_commit_bytes: reader.u64()?,
        reclaimed_commit_bytes: reader.u64()?,
        live_generation_records: reader.u64()?,
        reclaimed_generation_records: reader.u64()?,
        live_generation_record_bytes: reader.u64()?,
        reclaimed_generation_record_bytes: reader.u64()?,
    })
}

fn read_state(target_root: &Path) -> Result<Option<RetentionState>, String> {
    read_optional_bounded(
        &retention_state_path(target_root),
        MAX_RETENTION_STATE_BYTES,
    )?
    .as_deref()
    .map(decode_state)
    .transpose()
}

fn validate_index_cursors(target_root: &Path, state: &RetentionState) -> Result<(), String> {
    // Call only after selecting a state whose input digest matches the current
    // retention inputs. A mismatched state is reset before its old file bounds
    // are compared to a changed index; published compaction is already moved
    // to Cleanup by `recover_published_compaction`.
    if matches!(
        state.phase,
        RetentionPhase::Cleanup | RetentionPhase::Complete
    ) {
        return Ok(());
    }
    let index_path = target_root.join("history").join("commit.index");
    ensure_regular_file(&index_path)?;
    let length = fs::metadata(&index_path).map_err(super::display_io)?.len();
    if length % super::HISTORY_INDEX_ENTRY_BYTES != 0 {
        return Err("semantic history commit index length is invalid".to_owned());
    }
    if state.mark_offset > length {
        return Err("semantic history mark cursor exceeds the commit index".to_owned());
    }
    if state.phase != RetentionPhase::Mark && state.mark_offset != length {
        return Err("semantic history mark cursor does not cover the commit index".to_owned());
    }
    if state.commit_sweep_offset > length {
        return Err("semantic history sweep cursor exceeds the commit index".to_owned());
    }
    if matches!(
        state.phase,
        RetentionPhase::ScanCommitObjects
            | RetentionPhase::ScanMaps
            | RetentionPhase::SweepMaps
            | RetentionPhase::ScanGenerations
            | RetentionPhase::SweepGenerations
            | RetentionPhase::CompactIndex
    ) && state.commit_sweep_offset != length
    {
        return Err("semantic history sweep cursor does not cover the commit index".to_owned());
    }
    if state.phase == RetentionPhase::CompactIndex
        && (state.compact_input_offset > length
            || state.compact_output_offset > state.compact_input_offset)
    {
        return Err("semantic history compaction cursor exceeds the commit index".to_owned());
    }
    Ok(())
}

fn write_state(target_root: &Path, state: RetentionState) -> Result<(), String> {
    let bytes = encode_state(state)?;
    backend_platform::durable::write_private_atomic(&retention_state_path(target_root), &bytes)
        .map_err(super::display_io)
}

fn encode_delete_intent(intent: &RetentionDeleteIntent) -> Result<Vec<u8>, String> {
    validate_delete_name(intent.kind, &intent.name)?;
    let mut writer = super::Writer::new(MAX_RETENTION_DELETE_INTENT_BYTES - 32);
    writer.header(RETENTION_DELETE_INTENT_TAG)?;
    writer.u8(intent.kind.wire())?;
    writer.sized_bytes(intent.name.as_bytes(), RETENTION_DELETE_INTENT_NAME_BYTES)?;
    writer.u64(intent.byte_length)?;
    writer.u64(intent.old_reclaimed_count)?;
    writer.u64(intent.old_reclaimed_bytes)?;
    match intent.old_map_count {
        Some(count) => {
            writer.u8(1)?;
            writer.u32(count)?;
        }
        None => writer.u8(0)?,
    }
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    if bytes.len() > MAX_RETENTION_DELETE_INTENT_BYTES {
        return Err("semantic history delete intent exceeds its storage bound".to_owned());
    }
    Ok(bytes)
}

fn decode_delete_intent(bytes: &[u8]) -> Result<RetentionDeleteIntent, String> {
    let body = super::checked_body(bytes, MAX_RETENTION_DELETE_INTENT_BYTES)?;
    let mut reader = super::Reader::new(body);
    reader.header(RETENTION_DELETE_INTENT_TAG)?;
    let kind = DeleteKind::from_wire(reader.u8()?)?;
    let name = std::str::from_utf8(reader.sized_bytes(RETENTION_DELETE_INTENT_NAME_BYTES)?)
        .map_err(|_| "semantic history delete-intent name is not UTF-8".to_owned())?
        .to_owned();
    let byte_length = reader.u64()?;
    let old_reclaimed_count = reader.u64()?;
    let old_reclaimed_bytes = reader.u64()?;
    let old_map_count = match reader.u8()? {
        0 => None,
        1 => Some(reader.u32()?),
        _ => return Err("semantic history delete-intent map count is invalid".to_owned()),
    };
    reader.finish()?;
    validate_delete_name(kind, &name)?;
    if (kind == DeleteKind::Map && name.ends_with(".map")) != old_map_count.is_some() {
        return Err("semantic history delete intent is inconsistent".to_owned());
    }
    Ok(RetentionDeleteIntent {
        kind,
        name,
        byte_length,
        old_reclaimed_count,
        old_reclaimed_bytes,
        old_map_count,
    })
}

fn validate_delete_name(kind: DeleteKind, name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err("semantic history delete-intent name is unsafe".to_owned());
    }
    let suffix = match kind {
        DeleteKind::Map if name.ends_with(".map") => ".map",
        DeleteKind::Commit if name.ends_with(".commit") => ".commit",
        DeleteKind::Map | DeleteKind::Commit if name.ends_with(".tmp") => ".tmp",
        DeleteKind::Generation if name.ends_with(".record") => ".record",
        DeleteKind::Generation if name.ends_with(".tmp") => ".tmp",
        _ => {
            return Err("semantic history delete-intent name has an invalid suffix".to_owned());
        }
    };
    let stem = name
        .strip_suffix(suffix)
        .ok_or_else(|| "semantic history delete-intent suffix is invalid".to_owned())?;
    if suffix == ".tmp" {
        if !name.starts_with('.')
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err("semantic history temporary filename is invalid".to_owned());
        }
        return Ok(());
    }
    if stem.len() != 64 {
        return Err("semantic history delete-intent identity is malformed".to_owned());
    }
    let _ = decode_hex_digest(stem)?;
    Ok(())
}

fn read_delete_intent(target_root: &Path) -> Result<Option<RetentionDeleteIntent>, String> {
    read_optional_bounded(
        &retention_delete_intent_path(target_root),
        MAX_RETENTION_DELETE_INTENT_BYTES,
    )?
    .as_deref()
    .map(decode_delete_intent)
    .transpose()
}

fn write_delete_intent(target_root: &Path, intent: &RetentionDeleteIntent) -> Result<(), String> {
    let bytes = encode_delete_intent(intent)?;
    backend_platform::durable::write_private_atomic(
        &retention_delete_intent_path(target_root),
        &bytes,
    )
    .map_err(super::display_io)
}

fn delete_target_path(target_root: &Path, intent: &RetentionDeleteIntent) -> PathBuf {
    let history_root = target_root.join("history");
    match intent.kind {
        DeleteKind::Map => history_root.join("segment-map").join(&intent.name),
        DeleteKind::Commit => history_root.join("commits").join(&intent.name),
        DeleteKind::Generation => target_root.join("records").join(&intent.name),
    }
}

fn reclaimed_counters_mut(stats: &mut HistoryGcStats, kind: DeleteKind) -> (&mut u64, &mut u64) {
    match kind {
        DeleteKind::Map => (&mut stats.reclaimed_maps, &mut stats.reclaimed_map_bytes),
        DeleteKind::Commit => (
            &mut stats.reclaimed_commits,
            &mut stats.reclaimed_commit_bytes,
        ),
        DeleteKind::Generation => (
            &mut stats.reclaimed_generation_records,
            &mut stats.reclaimed_generation_record_bytes,
        ),
    }
}

fn recover_delete_intent(target_root: &Path, state: &mut RetentionState) -> Result<bool, String> {
    let Some(intent) = read_delete_intent(target_root)? else {
        return Ok(false);
    };
    let path = delete_target_path(target_root, &intent);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure_regular_file(&path)?;
            if metadata.len() != intent.byte_length {
                return Err("semantic history file changed during retention sweep".to_owned());
            }
            remove_file(&path)?;
            if intent.kind == DeleteKind::Map {
                #[cfg(test)]
                super::super::trip_history_test_fault(
                    super::super::HistoryTestFault::AfterHistoryMapUnlink,
                )?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(super::display_io(error)),
    }
    if intent.kind == DeleteKind::Commit && intent.name.ends_with(".commit") {
        let identity = intent
            .name
            .strip_suffix(".commit")
            .ok_or_else(|| "semantic history commit delete name is invalid".to_owned())?;
        if identity.len() != 64 {
            return Err("semantic history commit delete identity is invalid".to_owned());
        }
        let raw = decode_hex_digest(identity)?;
        let commit = HistoryCommitId::from_bytes(raw);
        remove_file(&history_payload_root_path(target_root, commit))?;
        remove_file(
            &target_root
                .join("history")
                .join("indexed")
                .join(format!("{identity}.indexed")),
        )?;
    }
    if let Some(old_count) = intent.old_map_count {
        let count_path = target_root.join("history").join("segment-map.count");
        let current = super::decode_history_segment_map_count(
            &read_optional_bounded(&count_path, super::MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?
                .ok_or_else(|| "semantic history segment-map count is missing".to_owned())?,
        )?;
        let next = old_count
            .checked_sub(1)
            .ok_or_else(|| "semantic history segment-map count underflows".to_owned())?;
        if current == old_count {
            write_history_segment_map_count(&count_path, next)?;
        } else if current != next {
            return Err(
                "semantic history segment-map delete intent conflicts with its count".to_owned(),
            );
        }
    }
    if !intent.name.ends_with(".tmp") {
        let (reclaimed_count, reclaimed_bytes) =
            reclaimed_counters_mut(&mut state.stats, intent.kind);
        let next_count = intent
            .old_reclaimed_count
            .checked_add(1)
            .ok_or_else(|| "semantic history reclaimed-record counter overflows".to_owned())?;
        let next_bytes = intent
            .old_reclaimed_bytes
            .checked_add(intent.byte_length)
            .ok_or_else(|| "semantic history reclaimed-byte counter overflows".to_owned())?;
        if *reclaimed_count == intent.old_reclaimed_count
            && *reclaimed_bytes == intent.old_reclaimed_bytes
        {
            *reclaimed_count = next_count;
            *reclaimed_bytes = next_bytes;
            write_state(target_root, *state)?;
        } else if *reclaimed_count != next_count || *reclaimed_bytes != next_bytes {
            return Err(
                "semantic history delete intent conflicts with retention counters".to_owned(),
            );
        }
    }
    remove_candidate_marker(target_root, state, intent.kind, &intent.name)?;
    remove_file(&retention_delete_intent_path(target_root))?;
    Ok(true)
}

fn candidate_directory(root: &Path, kind: DeleteKind) -> PathBuf {
    root.join(match kind {
        DeleteKind::Map => "map-candidates",
        DeleteKind::Commit => "commit-candidates",
        DeleteKind::Generation => "generation-candidates",
    })
}

fn candidate_marker_name(name: &str) -> String {
    format!(
        "{}.candidate",
        super::hex(blake3::hash(name.as_bytes()).as_bytes())
    )
}

fn ensure_candidate(root: &Path, kind: DeleteKind, name: &str) -> Result<(), String> {
    let path = candidate_directory(root, kind).join(candidate_marker_name(name));
    match fs::read(&path) {
        Ok(existing) if existing == name.as_bytes() => Ok(()),
        Ok(_) => Err("semantic history candidate marker conflicts with its name".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&path, name.as_bytes())
                .map_err(super::display_io)
        }
        Err(error) => Err(super::display_io(error)),
    }
}

fn first_candidate(root: &Path, kind: DeleteKind) -> Result<Option<(PathBuf, String)>, String> {
    let directory = candidate_directory(root, kind);
    for entry in fs::read_dir(&directory).map_err(super::display_io)? {
        let entry = entry.map_err(super::display_io)?;
        let path = entry.path();
        ensure_regular_file(&path)?;
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic history candidate name is not UTF-8".to_owned())?;
        let stem = file_name.strip_suffix(".candidate").ok_or_else(|| {
            "semantic history candidate directory contains an unknown member".to_owned()
        })?;
        if stem.len() != 64 {
            return Err("semantic history candidate marker name is malformed".to_owned());
        }
        let name = String::from_utf8(fs::read(&path).map_err(super::display_io)?)
            .map_err(|_| "semantic history candidate value is not UTF-8".to_owned())?;
        if candidate_marker_name(&name) != file_name {
            return Err("semantic history candidate marker differs from its name".to_owned());
        }
        return Ok(Some((path, name)));
    }
    Ok(None)
}

fn remove_candidate_marker(
    target_root: &Path,
    state: &RetentionState,
    kind: DeleteKind,
    name: &str,
) -> Result<(), String> {
    let root = retention_work_root(target_root, &state.input_digest);
    remove_file(&candidate_directory(&root, kind).join(candidate_marker_name(name)))
}

fn insert_marker_once(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_regular_file(path)?;
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(path, &[])
                .map_err(super::display_io)?;
            Ok(true)
        }
        Err(error) => Err(super::display_io(error)),
    }
}

fn marker_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_regular_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(super::display_io(error)),
    }
}

fn read_segment_mapping_if_present(
    target_root: &Path,
    segment: UntrustedSemanticSegmentId,
) -> Result<Option<u64>, String> {
    let path = target_root
        .join("history")
        .join("segment-map")
        .join(format!("{}.map", super::hex(segment.as_bytes())));
    let Some(bytes) = read_optional_bounded(&path, MAX_HISTORY_SEGMENT_MAP_BYTES)? else {
        return Ok(None);
    };
    let (_, byte_length) = decode_history_segment_mapping(&bytes, segment)?;
    Ok(Some(byte_length))
}

fn mark_generation(
    target_root: &Path,
    work_root: &Path,
    generation: &super::GenerationRecord,
) -> Result<(), String> {
    let live_generation = work_root.join("generation-live").join(format!(
        "{}.mark",
        super::hex(generation.identity.as_bytes())
    ));
    let _ = insert_marker_once(&live_generation)?;
    for plane in generation.manifest.planes() {
        for segment in plane.segments() {
            let claim = segment.id_claim();
            if let Some(byte_length) = read_segment_mapping_if_present(target_root, claim)? {
                if byte_length != segment.byte_length() {
                    return Err(
                        "semantic history bridge length differs from its manifest".to_owned()
                    );
                }
                let map_mark = work_root
                    .join("map-live")
                    .join(format!("{}.mark", super::hex(claim.as_bytes())));
                let _ = insert_marker_once(&map_mark)?;
            }
        }
    }
    Ok(())
}

fn mark_root_generation(
    target_root: &Path,
    work_root: &Path,
    identity: LocalSemanticGenerationId,
    target: &SemanticTargetKey,
) -> Result<(), String> {
    let record = super::load_record(target_root, identity, target)?;
    mark_generation(target_root, work_root, &record)
}

fn page_directory(
    directory: &Path,
    offset: u64,
    budget: usize,
) -> Result<(Vec<fs::DirEntry>, u64, bool), String> {
    let mut entries = fs::read_dir(directory).map_err(super::display_io)?;
    let mut skipped = 0_u64;
    while skipped < offset {
        match entries.next() {
            Some(entry) => {
                entry.map_err(super::display_io)?;
                skipped = skipped
                    .checked_add(1)
                    .ok_or_else(|| "semantic history directory cursor overflows".to_owned())?;
            }
            None => return Err("semantic history directory cursor exceeds its snapshot".to_owned()),
        }
    }
    let mut page = Vec::with_capacity(budget);
    while page.len() < budget {
        match entries.next() {
            Some(entry) => page.push(entry.map_err(super::display_io)?),
            None => break,
        }
    }
    let reached_end = match entries.next() {
        None => true,
        Some(entry) => {
            let _ = entry.map_err(super::display_io)?;
            false
        }
    };
    let next = offset
        .checked_add(page.len() as u64)
        .ok_or_else(|| "semantic history directory cursor overflows".to_owned())?;
    Ok((page, next, reached_end))
}

fn indexed_page(
    path: &Path,
    offset: u64,
    budget: usize,
    domain: &[u8],
) -> Result<(Vec<HistoryCommitId>, u64, bool), String> {
    ensure_regular_file(path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(super::display_io)?;
    let length = index.metadata().map_err(super::display_io)?.len();
    if length % super::HISTORY_INDEX_ENTRY_BYTES != 0 || offset > length {
        return Err("semantic history index cursor or length is invalid".to_owned());
    }
    let mut ids = Vec::with_capacity(budget);
    let mut cursor = offset;
    while cursor < length && ids.len() < budget {
        ids.push(history_index_id_at(&mut index, cursor, domain)?);
        cursor = cursor
            .checked_add(super::HISTORY_INDEX_ENTRY_BYTES)
            .ok_or_else(|| "semantic history index cursor overflows".to_owned())?;
    }
    Ok((ids, cursor, cursor == length))
}

fn process_live_commit(
    target_root: &Path,
    target: &SemanticTargetKey,
    work_root: &Path,
    identity: HistoryCommitId,
    stats: &mut HistoryGcStats,
) -> Result<(), String> {
    let path = history_commit_path(&target_root.join("history").join("commits"), identity);
    ensure_regular_file(&path)?;
    let length = fs::metadata(&path).map_err(super::display_io)?.len();
    if length > MAX_HISTORY_COMMIT_BYTES as u64 {
        return Err("semantic history commit exceeds its storage bound".to_owned());
    }
    let record = load_history_commit(&target_root.join("history").join("commits"), identity)?;
    let generation = super::load_record(target_root, record.generation, target)?;
    validate_commit_generation(&record, &generation)?;
    stats.live_commits = stats
        .live_commits
        .checked_add(1)
        .ok_or_else(|| "semantic history live-commit counter overflows".to_owned())?;
    stats.live_commit_bytes = stats
        .live_commit_bytes
        .checked_add(length)
        .ok_or_else(|| "semantic history live-commit bytes overflow".to_owned())?;
    mark_generation(target_root, work_root, &generation)
}

fn complete_delete(
    target_root: &Path,
    state: &mut RetentionState,
    kind: DeleteKind,
    name: String,
    byte_length: u64,
) -> Result<(), String> {
    let old_map_count = if kind == DeleteKind::Map && name.ends_with(".map") {
        let count_path = target_root.join("history").join("segment-map.count");
        let bytes = read_optional_bounded(&count_path, super::MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?
            .ok_or_else(|| "semantic history segment-map count is missing".to_owned())?;
        let count = super::decode_history_segment_map_count(&bytes)?;
        Some(count)
    } else {
        None
    };
    let (reclaimed_count, reclaimed_bytes) = reclaimed_counters_mut(&mut state.stats, kind);
    let intent = RetentionDeleteIntent {
        kind,
        name,
        byte_length,
        old_reclaimed_count: *reclaimed_count,
        old_reclaimed_bytes: *reclaimed_bytes,
        old_map_count,
    };
    write_delete_intent(target_root, &intent)?;
    let _ = recover_delete_intent(target_root, state)?;
    Ok(())
}

fn sweep_one_candidate(
    target_root: &Path,
    state: &mut RetentionState,
    kind: DeleteKind,
) -> Result<bool, String> {
    let work_root = retention_work_root(target_root, &state.input_digest);
    let Some((_, name)) = first_candidate(&work_root, kind)? else {
        return Ok(false);
    };
    let path = delete_target_path(
        target_root,
        &RetentionDeleteIntent {
            kind,
            name: name.clone(),
            byte_length: 0,
            old_reclaimed_count: 0,
            old_reclaimed_bytes: 0,
            old_map_count: None,
        },
    );
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure_regular_file(&path)?;
            let length = metadata.len();
            complete_delete(target_root, state, kind, name, length)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            complete_delete(target_root, state, kind, name, 0)?;
        }
        Err(error) => return Err(super::display_io(error)),
    }
    Ok(true)
}

fn add_live_generation_stats(state: &mut RetentionState, record_path: &Path) -> Result<(), String> {
    let length = fs::metadata(record_path).map_err(super::display_io)?.len();
    state.stats.live_generation_records = state
        .stats
        .live_generation_records
        .checked_add(1)
        .ok_or_else(|| "semantic history live-generation counter overflows".to_owned())?;
    state.stats.live_generation_record_bytes = state
        .stats
        .live_generation_record_bytes
        .checked_add(length)
        .ok_or_else(|| "semantic history live-generation bytes overflow".to_owned())?;
    Ok(())
}

fn mark_directory_path(root: &Path, kind: &str, identity: &[u8; 32]) -> PathBuf {
    root.join(kind)
        .join(format!("{}.mark", super::hex(identity)))
}

fn validate_record_file(
    target_root: &Path,
    target: &SemanticTargetKey,
    path: &Path,
) -> Result<Option<LocalSemanticGenerationId>, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "semantic generation filename is not UTF-8".to_owned())?;
    let Some(stem) = name.strip_suffix(".record") else {
        return Ok(None);
    };
    if stem.len() != 64 {
        return Err("semantic generation filename is malformed".to_owned());
    }
    let identity = LocalSemanticGenerationId(decode_hex_digest(stem)?);
    let record = super::load_record(target_root, identity, target)?;
    if record.identity != identity {
        return Err("semantic generation filename differs from its identity".to_owned());
    }
    Ok(Some(identity))
}

fn decode_hex_digest(value: &str) -> Result<[u8; 32], String> {
    super::decode_hex_digest(value)
}

fn init_state(digest: [u8; 32]) -> RetentionState {
    RetentionState {
        input_digest: digest,
        phase: RetentionPhase::Mark,
        mark_offset: 0,
        commit_sweep_offset: 0,
        map_scan_offset: 0,
        map_scan_count: 0,
        generation_scan_offset: 0,
        compact_input_offset: 0,
        compact_output_offset: 0,
        stats: HistoryGcStats::default(),
    }
}

fn recover_published_compaction(
    target_root: &Path,
    state: &mut RetentionState,
) -> Result<bool, String> {
    if state.phase != RetentionPhase::CompactIndex
        || state.compact_output_offset >= state.compact_input_offset
    {
        return Ok(false);
    }
    let old_work_root = retention_work_root(target_root, &state.input_digest);
    let staging_path = old_work_root.join("commit.index.compact");
    match fs::symlink_metadata(&staging_path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(super::display_io(error)),
    }
    let index_path = target_root.join("history").join("commit.index");
    ensure_regular_file(&index_path)?;
    if fs::metadata(&index_path).map_err(super::display_io)?.len() != state.compact_output_offset {
        return Ok(false);
    }
    super::compact_history_tombstones(target_root)?;
    state.input_digest = retention_input_digest(target_root)?;
    state.phase = RetentionPhase::Cleanup;
    write_state(target_root, *state)?;
    Ok(true)
}

fn remove_one_tree_member(directory: &Path, depth: usize) -> Result<bool, String> {
    if depth > 6 {
        return Err("semantic history GC work directory is unexpectedly deep".to_owned());
    }
    for entry in fs::read_dir(directory).map_err(super::display_io)? {
        let entry = entry.map_err(super::display_io)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(super::display_io)?;
        if metadata.file_type().is_dir() {
            if remove_one_tree_member(&path, depth + 1)? {
                return Ok(true);
            }
            fs::remove_dir(&path).map_err(super::display_io)?;
            backend_platform::durable::sync_parent(&path).map_err(super::display_io)?;
            return Ok(true);
        }
        ensure_regular_file(&path)?;
        remove_file(&path)?;
        return Ok(true);
    }
    Ok(false)
}

fn cleanup_one_stale_gc_item(
    target_root: &Path,
    current_retention_root: &Path,
    current_history_gc_root: &Path,
) -> Result<bool, String> {
    let gc_root = target_root.join("history").join("gc");
    if !super::ensure_optional_directory(&gc_root)? {
        return Ok(false);
    }
    for entry in fs::read_dir(&gc_root).map_err(super::display_io)? {
        let entry = entry.map_err(super::display_io)?;
        let path = entry.path();
        if path == current_retention_root || path == current_history_gc_root {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic history GC work name is not UTF-8".to_owned())?;
        let valid = if let Some(stem) = name.strip_prefix("retention-") {
            stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
        } else {
            name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        };
        if !valid {
            return Err("semantic history GC root contains an unknown work directory".to_owned());
        }
        let metadata = fs::symlink_metadata(&path).map_err(super::display_io)?;
        if !metadata.file_type().is_dir() {
            return Err("semantic history GC work entry is not a directory".to_owned());
        }
        if remove_one_tree_member(&path, 0)? {
            return Ok(true);
        }
        fs::remove_dir(&path).map_err(super::display_io)?;
        backend_platform::durable::sync_parent(&path).map_err(super::display_io)?;
        return Ok(true);
    }
    Ok(false)
}

/// Advances map and metadata retention after the catalog reachability pass.
/// `budget` is the remaining unit budget in the enclosing GC call.
pub(super) fn advance_retention(
    target_root: &Path,
    target: &SemanticTargetKey,
    budget: usize,
) -> Result<RetentionBatch, String> {
    let history_root = target_root.join("history");
    if !super::ensure_optional_directory(&history_root)? {
        return Ok(RetentionBatch {
            processed_records: 0,
            complete: true,
            stats: HistoryGcStats::default(),
        });
    }
    let (_, refs_digest) = read_history_catalog_snapshot(target_root)?;
    let history_gc = read_history_gc_state(target_root)?
        .ok_or_else(|| "semantic history GC state is missing".to_owned())?;
    if history_gc.refs_digest != refs_digest || history_gc.phase != super::HistoryGcPhase::Complete
    {
        return Err(
            "semantic history retention started before its catalog mark completed".to_owned(),
        );
    }
    let mut prior_state = read_state(target_root)?;
    if let Some(state) = &mut prior_state {
        let _ = recover_delete_intent(target_root, state)?;
    } else if read_delete_intent(target_root)?.is_some() {
        return Err("semantic history delete intent has no retention state".to_owned());
    }
    let intent_bytes = read_optional_bounded(
        &history_index_intent_path(target_root),
        MAX_HISTORY_INDEX_INTENT_BYTES,
    )?;
    if let Some(bytes) = intent_bytes.as_deref() {
        let _ = decode_history_index_intent(bytes)?;
        super::recover_history_index_intent(target_root)?;
    }
    let digest = retention_input_digest(target_root)?;
    let mut state = if let Some(mut prior) = prior_state {
        if prior.input_digest == digest || recover_published_compaction(target_root, &mut prior)? {
            prior
        } else {
            init_state(digest)
        }
    } else {
        init_state(digest)
    };
    validate_index_cursors(target_root, &state)?;
    let work_root = ensure_work_layout(target_root, &digest)?;
    let _ = recover_delete_intent(target_root, &mut state)?;
    let mut processed = 0_usize;
    while processed < budget {
        match state.phase {
            RetentionPhase::Mark => {
                let index_path = history_root.join("commit.index");
                let (ids, _next, done) = indexed_page(
                    &index_path,
                    state.mark_offset,
                    budget - processed,
                    super::HISTORY_INDEX_DOMAIN,
                )?;
                for identity in ids {
                    let seen_marker = work_root
                        .join("commit-seen")
                        .join(format!("{}.seen", super::hex(identity.as_bytes())));
                    let _ = insert_marker_once(&seen_marker)?;
                    let live = history_gc_marked(
                        &history_gc_epoch_root(target_root, &refs_digest),
                        identity,
                        HistoryReachabilityClass::Live,
                    )?;
                    if live {
                        process_live_commit(
                            target_root,
                            target,
                            &work_root,
                            identity,
                            &mut state.stats,
                        )?;
                    } else {
                        let commit_path =
                            history_commit_path(&history_root.join("commits"), identity);
                        match fs::symlink_metadata(&commit_path) {
                            Ok(_) => {
                                let record =
                                    load_history_commit(&history_root.join("commits"), identity)?;
                                if record.target != *target {
                                    return Err(
                                        "semantic history commit belongs to another target"
                                            .to_owned(),
                                    );
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(super::display_io(error)),
                        }
                    }
                    state.mark_offset = state
                        .mark_offset
                        .checked_add(super::HISTORY_INDEX_ENTRY_BYTES)
                        .ok_or_else(|| "semantic history mark cursor overflows".to_owned())?;
                    processed += 1;
                }
                if done {
                    if let Some(head_bytes) = read_optional_bounded(
                        &target_root.join("HEAD"),
                        super::super::MAX_HEAD_BYTES,
                    )? {
                        let head = super::super::decode_head(&head_bytes)?;
                        for identity in [Some(head.current), head.previous].into_iter().flatten() {
                            mark_root_generation(target_root, &work_root, identity, target)?;
                        }
                    }
                    state.phase = RetentionPhase::SweepCommits;
                    state.commit_sweep_offset = 0;
                }
                write_state(target_root, state)?;
                if done {
                    continue;
                }
            }
            RetentionPhase::SweepCommits => {
                let index_path = history_root.join("commit.index");
                let (ids, _next, done) = indexed_page(
                    &index_path,
                    state.commit_sweep_offset,
                    budget - processed,
                    super::HISTORY_INDEX_DOMAIN,
                )?;
                for identity in ids {
                    let live = history_gc_marked(
                        &history_gc_epoch_root(target_root, &refs_digest),
                        identity,
                        HistoryReachabilityClass::Live,
                    )?;
                    if !live {
                        let name = format!("{}.commit", super::hex(identity.as_bytes()));
                        ensure_candidate(&work_root, DeleteKind::Commit, &name)?;
                    }
                    state.commit_sweep_offset = state
                        .commit_sweep_offset
                        .checked_add(super::HISTORY_INDEX_ENTRY_BYTES)
                        .ok_or_else(|| "semantic history sweep cursor overflows".to_owned())?;
                    processed += 1;
                }
                if done {
                    state.phase = RetentionPhase::ScanCommitObjects;
                    state.map_scan_offset = 0;
                }
                write_state(target_root, state)?;
                if done {
                    continue;
                }
            }
            RetentionPhase::ScanCommitObjects => {
                let commits_root = history_root.join("commits");
                if !super::ensure_optional_directory(&commits_root)? {
                    state.phase = RetentionPhase::ScanMaps;
                    write_state(target_root, state)?;
                    continue;
                }
                let (entries, _next, done) =
                    page_directory(&commits_root, state.map_scan_offset, budget - processed)?;
                for entry in entries {
                    let path = entry.path();
                    ensure_regular_file(&path)?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "semantic history commit filename is not UTF-8".to_owned())?;
                    if name.ends_with(".commit") {
                        validate_delete_name(DeleteKind::Commit, &name)?;
                        let stem = name.strip_suffix(".commit").ok_or_else(|| {
                            "semantic history commit filename is malformed".to_owned()
                        })?;
                        let identity = HistoryCommitId(decode_hex_digest(stem)?);
                        let record = load_history_commit(&commits_root, identity)?;
                        if record.target != *target {
                            return Err(
                                "semantic history commit belongs to another target".to_owned()
                            );
                        }
                        let seen_marker = work_root
                            .join("commit-seen")
                            .join(format!("{}.seen", super::hex(identity.as_bytes())));
                        if !marker_exists(&seen_marker)? {
                            ensure_candidate(&work_root, DeleteKind::Commit, &name)?;
                        }
                    } else if name.ends_with(".tmp") {
                        validate_delete_name(DeleteKind::Commit, &name)?;
                        ensure_candidate(&work_root, DeleteKind::Commit, &name)?;
                    } else {
                        return Err(
                            "semantic history commit directory contains an unknown member"
                                .to_owned(),
                        );
                    }
                    state.map_scan_offset =
                        state.map_scan_offset.checked_add(1).ok_or_else(|| {
                            "semantic history commit scan cursor overflows".to_owned()
                        })?;
                    processed += 1;
                }
                if done {
                    state.phase = RetentionPhase::ScanMaps;
                    state.map_scan_offset = 0;
                }
                write_state(target_root, state)?;
                if done {
                    continue;
                }
            }
            RetentionPhase::ScanMaps => {
                let map_root = history_root.join("segment-map");
                if !super::ensure_optional_directory(&map_root)? {
                    super::create_private_directory(&map_root)?;
                }
                let (entries, _next, done) =
                    page_directory(&map_root, state.map_scan_offset, budget - processed)?;
                for entry in entries {
                    let path = entry.path();
                    ensure_regular_file(&path)?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "semantic history map filename is not UTF-8".to_owned())?;
                    let length = fs::metadata(&path).map_err(super::display_io)?.len();
                    if name.ends_with(".map") {
                        state.map_scan_count =
                            state.map_scan_count.checked_add(1).ok_or_else(|| {
                                "semantic history map scan count overflows".to_owned()
                            })?;
                        validate_delete_name(DeleteKind::Map, &name)?;
                        let stem = name.strip_suffix(".map").ok_or_else(|| {
                            "semantic history map filename is malformed".to_owned()
                        })?;
                        let segment =
                            UntrustedSemanticSegmentId::from_raw(decode_hex_digest(stem)?);
                        let bytes = read_optional_bounded(&path, MAX_HISTORY_SEGMENT_MAP_BYTES)?
                            .ok_or_else(|| {
                                "semantic history map disappeared during scan".to_owned()
                            })?;
                        let _ = decode_history_segment_mapping(&bytes, segment)?;
                        let live = marker_exists(
                            &work_root
                                .join("map-live")
                                .join(format!("{}.mark", super::hex(segment.as_bytes()))),
                        )?;
                        if live {
                            state.stats.live_maps =
                                state.stats.live_maps.checked_add(1).ok_or_else(|| {
                                    "semantic history live-map counter overflows".to_owned()
                                })?;
                            state.stats.live_map_bytes =
                                state.stats.live_map_bytes.checked_add(length).ok_or_else(
                                    || "semantic history live-map bytes overflow".to_owned(),
                                )?;
                        } else {
                            ensure_candidate(&work_root, DeleteKind::Map, &name)?;
                        }
                    } else if name.ends_with(".tmp") {
                        validate_delete_name(DeleteKind::Map, &name)?;
                        ensure_candidate(&work_root, DeleteKind::Map, &name)?;
                    } else {
                        return Err(
                            "semantic history map directory has an unknown member".to_owned()
                        );
                    }
                    state.map_scan_offset = state
                        .map_scan_offset
                        .checked_add(1)
                        .ok_or_else(|| "semantic history map scan cursor overflows".to_owned())?;
                    processed += 1;
                }
                if done {
                    let count = u32::try_from(state.map_scan_count)
                        .map_err(|_| "semantic history segment-map count overflows".to_owned())?;
                    write_history_segment_map_count(
                        &history_root.join("segment-map.count"),
                        count,
                    )?;
                    state.phase = RetentionPhase::SweepMaps;
                }
                write_state(target_root, state)?;
                if done {
                    continue;
                }
            }
            RetentionPhase::SweepMaps => {
                if sweep_one_candidate(target_root, &mut state, DeleteKind::Commit)? {
                    processed += 1;
                    write_state(target_root, state)?;
                    continue;
                }
                if sweep_one_candidate(target_root, &mut state, DeleteKind::Map)? {
                    processed += 1;
                    write_state(target_root, state)?;
                    continue;
                }
                state.phase = RetentionPhase::ScanGenerations;
                write_state(target_root, state)?;
            }
            RetentionPhase::ScanGenerations => {
                let records_root = target_root.join("records");
                if !super::ensure_optional_directory(&records_root)? {
                    state.phase = RetentionPhase::SweepGenerations;
                    write_state(target_root, state)?;
                    continue;
                }
                let (entries, _next, done) = page_directory(
                    &records_root,
                    state.generation_scan_offset,
                    budget - processed,
                )?;
                for entry in entries {
                    let path = entry.path();
                    ensure_regular_file(&path)?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "semantic generation filename is not UTF-8".to_owned())?;
                    if name.ends_with(".record") {
                        let Some(identity) = validate_record_file(target_root, target, &path)?
                        else {
                            return Err("semantic generation record name is invalid".to_owned());
                        };
                        let live_marker =
                            mark_directory_path(&work_root, "generation-live", identity.as_bytes());
                        if marker_exists(&live_marker)? {
                            add_live_generation_stats(&mut state, &path)?;
                        } else {
                            ensure_candidate(&work_root, DeleteKind::Generation, &name)?;
                        }
                    } else if name.ends_with(".tmp") {
                        ensure_candidate(&work_root, DeleteKind::Generation, &name)?;
                    } else {
                        return Err(
                            "semantic generation directory contains an unknown member".to_owned()
                        );
                    }
                    state.generation_scan_offset = state
                        .generation_scan_offset
                        .checked_add(1)
                        .ok_or_else(|| "semantic generation scan cursor overflows".to_owned())?;
                    processed += 1;
                }
                if done {
                    state.phase = RetentionPhase::SweepGenerations;
                }
                write_state(target_root, state)?;
                if done {
                    continue;
                }
            }
            RetentionPhase::SweepGenerations => {
                if sweep_one_candidate(target_root, &mut state, DeleteKind::Generation)? {
                    processed += 1;
                    write_state(target_root, state)?;
                    continue;
                }
                state.phase = RetentionPhase::CompactIndex;
                state.compact_input_offset = 0;
                state.compact_output_offset = 0;
                write_state(target_root, state)?;
            }
            RetentionPhase::CompactIndex => {
                let index_path = history_root.join("commit.index");
                ensure_regular_file(&index_path)?;
                let source_length = fs::metadata(&index_path).map_err(super::display_io)?.len();
                if state.compact_input_offset > source_length
                    || state.compact_input_offset % super::HISTORY_INDEX_ENTRY_BYTES != 0
                    || state.compact_output_offset % super::HISTORY_INDEX_ENTRY_BYTES != 0
                {
                    return Err("semantic history index compaction cursor is invalid".to_owned());
                }
                let staging_path = work_root.join("commit.index.compact");
                let staging_exists = match fs::symlink_metadata(&staging_path) {
                    Ok(metadata) if metadata.file_type().is_file() => true,
                    Ok(_) => {
                        return Err(
                            "semantic history compacted index staging path is not a regular file"
                                .to_owned(),
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(super::display_io(error)),
                };
                if !staging_exists
                    && state.compact_input_offset == source_length
                    && state.compact_output_offset == source_length
                {
                    // The input and output lengths are equal, so the filtered
                    // index was already identical and its staging file was
                    // removed before the last state write.
                    super::compact_history_tombstones(target_root)?;
                    state.phase = RetentionPhase::Cleanup;
                    state.input_digest = retention_input_digest(target_root)?;
                    write_state(target_root, state)?;
                    continue;
                }
                if !staging_exists
                    && (state.compact_input_offset != 0 || state.compact_output_offset != 0)
                {
                    return Err(
                        "semantic history compacted index staging file is missing".to_owned()
                    );
                }
                let mut staging = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&staging_path)
                    .map_err(super::display_io)?;
                // If the process stopped after appending a page but before
                // persisting its cursor, trim those bytes before replaying it.
                staging
                    .set_len(state.compact_output_offset)
                    .map_err(super::display_io)?;
                staging
                    .seek(std::io::SeekFrom::Start(state.compact_output_offset))
                    .map_err(super::display_io)?;
                let (ids, _next, done) = indexed_page(
                    &index_path,
                    state.compact_input_offset,
                    budget - processed,
                    super::HISTORY_INDEX_DOMAIN,
                )?;
                let live_epoch = history_gc_epoch_root(target_root, &refs_digest);
                for identity in ids {
                    let live =
                        history_gc_marked(&live_epoch, identity, HistoryReachabilityClass::Live)?;
                    if live {
                        super::append_history_index_entry(
                            &staging_path,
                            identity,
                            super::HISTORY_INDEX_DOMAIN,
                        )?;
                        state.compact_output_offset = state
                            .compact_output_offset
                            .checked_add(super::HISTORY_INDEX_ENTRY_BYTES)
                            .ok_or_else(|| {
                                "semantic history compacted index length overflows".to_owned()
                            })?;
                    }
                    state.compact_input_offset = state
                        .compact_input_offset
                        .checked_add(super::HISTORY_INDEX_ENTRY_BYTES)
                        .ok_or_else(|| "semantic history compaction cursor overflows".to_owned())?;
                    processed += 1;
                }
                if done {
                    if state.compact_input_offset != source_length {
                        return Err(
                            "semantic history compaction ended before the input index".to_owned()
                        );
                    }
                    if state.compact_output_offset == source_length {
                        remove_file(&staging_path)?;
                    } else {
                        staging.sync_all().map_err(super::display_io)?;
                        drop(staging);
                        fs::rename(&staging_path, &index_path).map_err(super::display_io)?;
                        backend_platform::durable::sync_parent(&index_path)
                            .map_err(super::display_io)?;
                        #[cfg(test)]
                        super::super::trip_history_test_fault(
                            super::super::HistoryTestFault::AfterHistoryIndexCompactRename,
                        )?;
                    }
                    super::compact_history_tombstones(target_root)?;
                    state.phase = RetentionPhase::Cleanup;
                    state.input_digest = retention_input_digest(target_root)?;
                }
                write_state(target_root, state)?;
                if done {
                    continue;
                }
            }
            RetentionPhase::Cleanup => {
                let current_retention_root = ensure_work_layout(target_root, &state.input_digest)?;
                let current_history_gc_root = history_gc_epoch_root(target_root, &refs_digest);
                if cleanup_one_stale_gc_item(
                    target_root,
                    &current_retention_root,
                    &current_history_gc_root,
                )? {
                    processed += 1;
                    write_state(target_root, state)?;
                    continue;
                }
                state.phase = RetentionPhase::Complete;
                write_state(target_root, state)?;
            }
            RetentionPhase::Complete => {
                return Ok(RetentionBatch {
                    processed_records: processed,
                    complete: true,
                    stats: state.stats,
                });
            }
        }
        if processed >= budget {
            write_state(target_root, state)?;
            return Ok(RetentionBatch {
                processed_records: processed,
                complete: state.phase == RetentionPhase::Complete,
                stats: state.stats,
            });
        }
    }
    write_state(target_root, state)?;
    Ok(RetentionBatch {
        processed_records: processed,
        complete: state.phase == RetentionPhase::Complete,
        stats: state.stats,
    })
}

pub(super) fn read_stats(target_root: &Path) -> Result<Option<HistoryGcStats>, String> {
    let Some(state) = read_state(target_root)? else {
        return Ok(None);
    };
    if state.input_digest != retention_input_digest(target_root)? {
        return Ok(None);
    }
    Ok(Some(state.stats))
}

pub(super) fn recover_pending_delete(target_root: &Path) -> Result<(), String> {
    let Some(mut state) = read_state(target_root)? else {
        if read_delete_intent(target_root)?.is_some() {
            return Err("semantic history delete intent has no retention state".to_owned());
        }
        return Ok(());
    };
    let _ = recover_delete_intent(target_root, &mut state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-semantic-history-retention-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create unique retention fixture");
            super::super::super::set_private_directory(&path)
                .expect("make retention fixture private");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mark_cursor_resumes_at_aligned_offset_and_rejects_misaligned_or_unbounded_state() {
        let directory = TestDirectory::create();
        let history_root = directory.0.join("history");
        fs::create_dir_all(&history_root).expect("create history directory");
        let index_length = super::super::HISTORY_INDEX_ENTRY_BYTES * 4;
        fs::write(
            history_root.join("commit.index"),
            vec![0; usize::try_from(index_length).expect("small index fixture")],
        )
        .expect("write bounded commit index fixture");

        let mut state = init_state([0x53; 32]);
        state.mark_offset = super::super::HISTORY_INDEX_ENTRY_BYTES * 2;
        fs::write(
            retention_state_path(&directory.0),
            encode_state(state).expect("encode resumable Mark state"),
        )
        .expect("persist in-progress mark cursor");
        let reopened = read_state(&directory.0)
            .expect("cold-read persisted retention cursor")
            .expect("retention cursor exists");
        assert_eq!(reopened, state);
        validate_index_cursors(&directory.0, &reopened)
            .expect("aligned cursor within index resumes safely");

        let mut misaligned = state;
        misaligned.mark_offset += 1;
        let bytes = encode_state(misaligned).expect("encode deliberately corrupt cursor");
        assert!(
            decode_state(&bytes)
                .expect_err("misaligned index cursor is corrupt")
                .contains("cursors are invalid")
        );

        let mut out_of_bounds = state;
        out_of_bounds.mark_offset = index_length + super::super::HISTORY_INDEX_ENTRY_BYTES;
        let bytes = encode_state(out_of_bounds).expect("encode out-of-range cursor");
        let reopened = decode_state(&bytes).expect("alignment is structurally valid");
        assert!(
            validate_index_cursors(&directory.0, &reopened)
                .expect_err("cursor beyond current index must fail closed")
                .contains("mark cursor exceeds")
        );

        let mut completed_with_temp_reclamation = init_state([0x54; 32]);
        completed_with_temp_reclamation.phase = RetentionPhase::Complete;
        completed_with_temp_reclamation.map_scan_offset = 1;
        completed_with_temp_reclamation.stats.reclaimed_maps = 1;
        assert_eq!(
            decode_state(
                &encode_state(completed_with_temp_reclamation)
                    .expect("encode completed map-temporary cleanup")
            )
            .expect("directory cursor includes reclaimed temporary map entries"),
            completed_with_temp_reclamation
        );
        completed_with_temp_reclamation.map_scan_offset = 0;
        assert!(
            decode_state(
                &encode_state(completed_with_temp_reclamation)
                    .expect("encode incomplete completed-state cursor")
            )
            .expect_err("completed state must account for every reclaimed entry")
            .contains("cursors are invalid")
        );
    }

    fn map_delete_fixture(map_count: u32, include_map: bool) -> (TestDirectory, RetentionState) {
        let directory = TestDirectory::create();
        let history_root = directory.0.join("history");
        let map_root = history_root.join("segment-map");
        fs::create_dir_all(&map_root).expect("create segment-map directory");
        super::super::super::set_private_directory(&history_root)
            .expect("make history fixture private");
        super::super::super::set_private_directory(&map_root)
            .expect("make segment-map fixture private");
        let gc_root = history_root.join("gc");
        fs::create_dir(&gc_root).expect("create retention GC root");
        super::super::super::set_private_directory(&gc_root)
            .expect("make retention GC root private");
        let name = format!("{}.map", "ab".repeat(32));
        if include_map {
            fs::write(map_root.join(&name), b"checked map fixture")
                .expect("write known map fixture");
        }
        write_history_segment_map_count(&history_root.join("segment-map.count"), map_count)
            .expect("write map count fixture");

        let mut state = init_state([0x31; 32]);
        state.phase = RetentionPhase::SweepMaps;
        let work_root = ensure_work_layout(&directory.0, &state.input_digest)
            .expect("create retention work layout");
        ensure_candidate(&work_root, DeleteKind::Map, &name)
            .expect("write exact map candidate marker");
        write_state(&directory.0, state).expect("persist interrupted retention state");
        write_delete_intent(
            &directory.0,
            &RetentionDeleteIntent {
                kind: DeleteKind::Map,
                name,
                byte_length: b"checked map fixture".len() as u64,
                old_reclaimed_count: 0,
                old_reclaimed_bytes: 0,
                old_map_count: Some(1),
            },
        )
        .expect("persist map deletion intent");
        (directory, state)
    }

    #[test]
    fn map_count_recovers_after_unlink_before_counter_write() {
        let (directory, mut state) = map_delete_fixture(1, true);
        super::super::super::arm_history_test_fault(
            super::super::super::HistoryTestFault::AfterHistoryMapUnlink,
        );
        assert!(
            recover_delete_intent(&directory.0, &mut state)
                .expect_err("injected stop follows unlink")
                .contains("injected semantic history interruption")
        );

        let history_root = directory.0.join("history");
        let map_root = history_root.join("segment-map");
        assert_eq!(fs::read_dir(&map_root).expect("enumerate map").count(), 0);
        assert_eq!(
            super::super::decode_history_segment_map_count(
                &fs::read(history_root.join("segment-map.count")).expect("read old count")
            )
            .expect("decode old count"),
            1,
            "the durable intent bridges the unlink and counter write"
        );

        let mut reopened_state = read_state(&directory.0)
            .expect("read retention state after restart")
            .expect("retention state remains durable");
        assert!(
            recover_delete_intent(&directory.0, &mut reopened_state)
                .expect("idempotently finish interrupted deletion")
        );
        assert_eq!(
            super::super::decode_history_segment_map_count(
                &fs::read(history_root.join("segment-map.count")).expect("read repaired count")
            )
            .expect("decode repaired count"),
            0
        );
        assert_eq!(reopened_state.stats.reclaimed_maps, 1);
        assert_eq!(reopened_state.stats.reclaimed_map_bytes, 19);
        assert!(
            read_delete_intent(&directory.0)
                .expect("read completed delete intent")
                .is_none()
        );
    }

    #[test]
    fn map_count_recovers_when_counter_was_written_before_unlink() {
        let (directory, mut state) = map_delete_fixture(0, true);
        let map_root = directory.0.join("history").join("segment-map");
        assert_eq!(fs::read_dir(&map_root).expect("enumerate map").count(), 1);

        assert!(
            recover_delete_intent(&directory.0, &mut state)
                .expect("finish deletion after counter-first crash")
        );
        assert_eq!(
            fs::read_dir(&map_root)
                .expect("enumerate swept map")
                .count(),
            0
        );
        assert_eq!(
            super::super::decode_history_segment_map_count(
                &fs::read(directory.0.join("history").join("segment-map.count"))
                    .expect("read already-decremented count")
            )
            .expect("decode already-decremented count"),
            0
        );
        assert_eq!(state.stats.reclaimed_maps, 1);
        assert_eq!(state.stats.reclaimed_map_bytes, 19);
    }
}
