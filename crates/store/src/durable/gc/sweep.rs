//! Durable candidate indexes and quarantine helpers for the GC sweep.

use super::super::{Hash, StoreError, sync_directory};
use super::state_io::log_digest;
use super::{
    GC_CANDIDATE_DIGEST_DOMAIN, GC_CANDIDATE_RECORD_BYTES, GC_DIR, GC_MARK, GC_QUARANTINE,
    GC_QUEUE, GC_ROOTS, GC_STATE, MAX_QUEUE_ITEMS, io_error,
};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SweepKind {
    Pack,
    Closure,
    Object,
}

impl SweepKind {
    pub(super) const fn directory(self) -> &'static str {
        match self {
            Self::Pack => "packs",
            Self::Closure => "closures",
            Self::Object => "objects",
        }
    }

    const fn index_name(self) -> &'static str {
        match self {
            Self::Pack => "candidates.packs",
            Self::Closure => "candidates.closures",
            Self::Object => "candidates.objects",
        }
    }

    const fn index_temp_name(self) -> &'static str {
        match self {
            Self::Pack => ".candidates.packs.tmp",
            Self::Closure => ".candidates.closures.tmp",
            Self::Object => ".candidates.objects.tmp",
        }
    }

    const fn build_name(self) -> &'static str {
        match self {
            Self::Pack => ".candidate-build-packs",
            Self::Closure => ".candidate-build-closures",
            Self::Object => ".candidate-build-objects",
        }
    }
}

/// One physical immutable file admitted into a frozen sweep index.
///
/// The record contains the canonical hash and the physical variant.  Store
/// filenames are derived from this record, so the index never retains a
/// directory listing or an unbounded collection of path strings.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Candidate {
    Pack { id: Hash, tree: bool },
    Closure(Hash),
    Object(Hash),
}

impl Candidate {
    pub(super) fn encode(self) -> [u8; GC_CANDIDATE_RECORD_BYTES] {
        let mut bytes = [0; GC_CANDIDATE_RECORD_BYTES];
        match self {
            Self::Pack { id, tree: false } => {
                bytes[0] = 1;
                bytes[1..33].copy_from_slice(&id);
            }
            Self::Pack { id, tree: true } => {
                bytes[0] = 2;
                bytes[1..33].copy_from_slice(&id);
            }
            Self::Closure(id) => {
                bytes[0] = 3;
                bytes[1..33].copy_from_slice(&id);
            }
            Self::Object(id) => {
                bytes[0] = 4;
                bytes[1..33].copy_from_slice(&id);
            }
        }
        // Byte 33 is reserved so the record can grow without changing the
        // framing contract. It must remain zero in this format.
        bytes
    }

    pub(super) fn decode(bytes: &[u8; GC_CANDIDATE_RECORD_BYTES]) -> Result<Self, StoreError> {
        if bytes[33] != 0 {
            return Err(StoreError::Corrupt);
        }
        let id: Hash = bytes[1..33].try_into().map_err(|_| StoreError::Corrupt)?;
        match bytes[0] {
            1 => Ok(Self::Pack { id, tree: false }),
            2 => Ok(Self::Pack { id, tree: true }),
            3 => Ok(Self::Closure(id)),
            4 => Ok(Self::Object(id)),
            _ => Err(StoreError::Corrupt),
        }
    }

    pub(super) const fn kind(self) -> SweepKind {
        match self {
            Self::Pack { .. } => SweepKind::Pack,
            Self::Closure(_) => SweepKind::Closure,
            Self::Object(_) => SweepKind::Object,
        }
    }

    pub(super) const fn id(self) -> Hash {
        match self {
            Self::Pack { id, .. } | Self::Closure(id) | Self::Object(id) => id,
        }
    }

    pub(super) const fn mark_key(self) -> super::state::MarkKey {
        match self {
            Self::Pack { id, .. } => super::state::MarkKey::Pack(id),
            Self::Closure(id) => super::state::MarkKey::Closure(id),
            Self::Object(id) => super::state::MarkKey::Object(id),
        }
    }

    pub(super) fn filename(self) -> String {
        let mut name = String::with_capacity(70);
        for byte in self.id() {
            name.push(hex_digit(byte >> 4));
            name.push(hex_digit(byte & 0x0f));
        }
        match self {
            Self::Pack { tree: false, .. } => name.push_str(".pack"),
            Self::Pack { tree: true, .. } => name.push_str(".tree"),
            Self::Closure(_) => name.push_str(".closure"),
            Self::Object(_) => name.push_str(".object"),
        }
        name
    }

    pub(super) fn path(self, root: &Path) -> PathBuf {
        root.join(self.kind().directory()).join(self.filename())
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + value - 10) as char,
        _ => '?',
    }
}

pub(super) struct GcPaths {
    pub(super) dir: PathBuf,
    pub(super) state: PathBuf,
    pub(super) roots: PathBuf,
    pub(super) queue: PathBuf,
    pub(super) mark: PathBuf,
    pub(super) quarantine: PathBuf,
}

impl GcPaths {
    pub(super) fn new(root: &Path) -> Self {
        let dir = root.join(GC_DIR);
        Self {
            state: dir.join(GC_STATE),
            roots: dir.join(GC_ROOTS),
            queue: dir.join(GC_QUEUE),
            mark: dir.join(GC_MARK),
            quarantine: dir.join(GC_QUARANTINE),
            dir,
        }
    }

    pub(super) fn candidate_index(&self, kind: SweepKind) -> PathBuf {
        self.dir.join(kind.index_name())
    }

    fn candidate_temp(&self, kind: SweepKind) -> PathBuf {
        self.dir.join(kind.index_temp_name())
    }

    fn candidate_build(&self, kind: SweepKind) -> PathBuf {
        self.dir.join(kind.build_name())
    }

    pub(super) fn candidate_files(&self) -> [PathBuf; 3] {
        [
            self.candidate_index(SweepKind::Pack),
            self.candidate_index(SweepKind::Closure),
            self.candidate_index(SweepKind::Object),
        ]
    }
}

pub(super) fn parse_candidate(name: &str, kind: SweepKind) -> Option<Candidate> {
    let extension = Path::new(name).extension()?.to_str()?;
    let tag = match (kind, extension) {
        (SweepKind::Pack, "pack") => 1,
        (SweepKind::Pack, "tree") => 2,
        (SweepKind::Closure, "closure") => 3,
        (SweepKind::Object, "object") => 4,
        _ => return None,
    };
    let id = parse_hex(Path::new(name).file_stem()?.to_str()?)?;
    let candidate = match tag {
        1 => Candidate::Pack { id, tree: false },
        2 => Candidate::Pack { id, tree: true },
        3 => Candidate::Closure(id),
        4 => Candidate::Object(id),
        _ => return None,
    };
    (candidate.filename() == name).then_some(candidate)
}

pub(super) fn parse_hex(value: &str) -> Option<Hash> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(bytes)
}

pub(super) fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Builds one durable, bucket-ordered candidate index by scanning the source
/// directory exactly once. The 256 temporary shards bound memory regardless
/// of directory size; their concatenation is the frozen cursor space for the
/// whole sweep phase.
pub(super) fn build_candidate_index(
    paths: &GcPaths,
    kind: SweepKind,
    source: &Path,
    limits: super::state::GcLimits,
) -> Result<Hash, StoreError> {
    let build = paths.candidate_build(kind);
    let temporary = paths.candidate_temp(kind);
    let _ = fs::remove_dir_all(&build);
    let _ = fs::remove_file(&temporary);
    fs::create_dir_all(&build).map_err(|error| io_error(&error))?;

    let result = build_candidate_index_inner(paths, kind, source, limits, &build, &temporary);
    if result.is_err() {
        let _ = fs::remove_dir_all(&build);
        let _ = fs::remove_file(&temporary);
        let _ = sync_directory(&paths.dir);
    }
    result
}

fn build_candidate_index_inner(
    paths: &GcPaths,
    kind: SweepKind,
    source: &Path,
    limits: super::state::GcLimits,
    build: &Path,
    temporary: &Path,
) -> Result<Hash, StoreError> {
    let mut shards: [Option<File>; 256] = std::array::from_fn(|_| None);
    let mut count = 0u64;
    for entry in fs::read_dir(source).map_err(|error| io_error(&error))? {
        let entry = entry.map_err(|error| io_error(&error))?;
        if !entry
            .file_type()
            .map_err(|error| io_error(&error))?
            .is_file()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(candidate) = parse_candidate(&name, kind) else {
            continue;
        };
        count = count.checked_add(1).ok_or(StoreError::Bounds)?;
        if count > limits.max_queue_items || count > MAX_QUEUE_ITEMS {
            return Err(StoreError::Bounds);
        }
        let bucket = usize::from(candidate.id()[0]);
        if shards[bucket].is_none() {
            let path = build.join(format!("bucket-{bucket:02x}"));
            shards[bucket] = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|error| io_error(&error))?,
            );
        }
        shards[bucket]
            .as_mut()
            .ok_or(StoreError::Corrupt)?
            .write_all(&candidate.encode())
            .map_err(|error| io_error(&error))?;
    }
    for file in shards.iter_mut().flatten() {
        file.sync_all().map_err(|error| io_error(&error))?;
    }
    drop(shards);
    sync_directory(build)?;

    let mut index = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(temporary)
        .map_err(|error| io_error(&error))?;
    for bucket in 0u16..=u8::MAX.into() {
        let shard = build.join(format!("bucket-{bucket:02x}"));
        let Ok(mut shard_file) = File::open(&shard) else {
            continue;
        };
        let length = shard_file
            .metadata()
            .map_err(|error| io_error(&error))?
            .len();
        if !length.is_multiple_of(
            u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?,
        ) {
            return Err(StoreError::Corrupt);
        }
        io::copy(&mut shard_file, &mut index).map_err(|error| io_error(&error))?;
    }
    index.sync_all().map_err(|error| io_error(&error))?;
    sync_directory(&paths.dir)?;
    #[cfg(test)]
    if super::take_test_fault(2) {
        return Err(StoreError::Io(
            "injected GC failure after candidate index sync".to_owned(),
        ));
    }
    fs::rename(temporary, paths.candidate_index(kind)).map_err(|error| io_error(&error))?;
    sync_directory(&paths.dir)?;
    #[cfg(test)]
    if super::take_test_fault(3) {
        return Err(StoreError::Io(
            "injected GC failure after candidate index rename".to_owned(),
        ));
    }
    fs::remove_dir_all(build).map_err(|error| io_error(&error))?;
    sync_directory(&paths.dir)?;
    log_digest(
        &paths.candidate_index(kind),
        GC_CANDIDATE_RECORD_BYTES,
        GC_CANDIDATE_DIGEST_DOMAIN,
    )
}

pub(super) fn validate_candidate_index(
    path: &Path,
    kind: SweepKind,
    limits: super::state::GcLimits,
) -> Result<u64, StoreError> {
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    let length = file.metadata().map_err(|error| io_error(&error))?.len();
    let record_bytes = u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if !length.is_multiple_of(record_bytes)
        || length / record_bytes > limits.max_queue_items
        || length / record_bytes > MAX_QUEUE_ITEMS
    {
        return Err(StoreError::Corrupt);
    }
    let mut previous_bucket = 0u8;
    let mut first = true;
    let mut bytes = [0; GC_CANDIDATE_RECORD_BYTES];
    let mut offset = 0u64;
    while offset < length {
        file.read_exact(&mut bytes)
            .map_err(|error| io_error(&error))?;
        let candidate = Candidate::decode(&bytes)?;
        if candidate.kind() != kind {
            return Err(StoreError::Corrupt);
        }
        let bucket = candidate.id()[0];
        if !first && bucket < previous_bucket {
            return Err(StoreError::Corrupt);
        }
        first = false;
        previous_bucket = bucket;
        offset = offset.checked_add(record_bytes).ok_or(StoreError::Bounds)?;
    }
    Ok(length)
}

pub(super) fn candidate_index_length(path: &Path) -> Result<u64, StoreError> {
    let length = File::open(path)
        .map_err(|error| io_error(&error))?
        .metadata()
        .map_err(|error| io_error(&error))?
        .len();
    if !length
        .is_multiple_of(u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?)
    {
        return Err(StoreError::Corrupt);
    }
    Ok(length)
}

pub(super) fn read_candidate(path: &Path, offset: u64) -> Result<Option<Candidate>, StoreError> {
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    let length = file.metadata().map_err(|error| io_error(&error))?.len();
    let record_bytes = u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if !length.is_multiple_of(record_bytes) || !offset.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    if offset == length {
        return Ok(None);
    }
    if offset > length || length - offset < record_bytes {
        return Err(StoreError::Corrupt);
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error(&error))?;
    let mut bytes = [0; GC_CANDIDATE_RECORD_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|error| io_error(&error))?;
    Candidate::decode(&bytes).map(Some)
}

pub(super) fn quarantine_file(
    paths: &GcPaths,
    kind: SweepKind,
    name: &str,
    source: &Path,
) -> Result<(), StoreError> {
    let destination_dir = paths.quarantine.join(kind.directory());
    fs::create_dir_all(&destination_dir).map_err(|error| io_error(&error))?;
    let destination = destination_dir.join(name);
    fs::rename(source, &destination).map_err(|error| io_error(&error))?;
    sync_directory(source.parent().ok_or(StoreError::Bounds)?)?;
    sync_directory(&destination_dir)?;
    sync_directory(&paths.quarantine)?;
    sync_directory(&paths.dir)
}

pub(super) fn restore_quarantine(paths: &GcPaths) -> Result<(), StoreError> {
    if !paths.quarantine.exists() {
        return Ok(());
    }
    for kind in [SweepKind::Pack, SweepKind::Closure, SweepKind::Object] {
        let directory = paths.quarantine.join(kind.directory());
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(|error| io_error(&error))?;
            let name = entry.file_name();
            let destination = paths
                .dir
                .parent()
                .ok_or(StoreError::Bounds)?
                .join(kind.directory())
                .join(&name);
            if destination.exists() {
                return Err(StoreError::Corrupt);
            }
            fs::rename(entry.path(), &destination).map_err(|error| io_error(&error))?;
            sync_directory(destination.parent().ok_or(StoreError::Bounds)?)?;
        }
    }
    remove_quarantine(paths)
}

pub(super) fn remove_quarantine(paths: &GcPaths) -> Result<(), StoreError> {
    match fs::remove_dir_all(&paths.quarantine) {
        Ok(()) => sync_directory(&paths.dir),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(&error)),
    }
}

pub(super) fn remove_gc_files(paths: &GcPaths) -> Result<(), StoreError> {
    for path in [
        paths.state.clone(),
        paths.roots.clone(),
        paths.queue.clone(),
        paths.mark.clone(),
        paths.mark.with_file_name("mark.index"),
        paths.mark.with_file_name("queue.index"),
        paths.mark.with_file_name("mark.index.tmp"),
        paths.mark.with_file_name("queue.index.tmp"),
        paths.mark.with_file_name("mark.freeze.tmp"),
        paths.candidate_index(SweepKind::Pack),
        paths.candidate_index(SweepKind::Closure),
        paths.candidate_index(SweepKind::Object),
        paths.candidate_temp(SweepKind::Pack),
        paths.candidate_temp(SweepKind::Closure),
        paths.candidate_temp(SweepKind::Object),
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&error)),
        }
    }
    for kind in [SweepKind::Pack, SweepKind::Closure, SweepKind::Object] {
        match fs::remove_dir_all(paths.candidate_build(kind)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&error)),
        }
    }
    sync_directory(&paths.dir)
}
