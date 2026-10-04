//! Process-shared generation selection for the rebuildable SQL projection.
//!
//! A selected projection is mutable, so its generation name is never reused.
//! The fixed selector contains only a generation number, schema, and seed
//! witness; it never supplies a filesystem path. Replacements are populated
//! privately and selected by one atomic file rename. Old roots are retained
//! until the platform exposes an exact opened-child cleanup receipt.

use crate::ProjectionError;
use backend_platform::{
    CreatedDirectory, DirectoryCapability, DirectoryCreateFailure, DirectoryRenameError,
    EntryKind, FileIdentity,
};
use std::fs::{File, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const NAMESPACE_SUFFIX: &str = ".namespace-v1";
const GENERATIONS_NAME: &str = "generations";
const GATE_NAME: &str = "selection.lock";
const SELECTOR_NAME: &str = "selected";
const COUNTER_NAME: &str = "last-generation";
const LEASE_NAME: &str = ".lease";
const IDENTITY_NAME: &str = ".generation";
const SELECTOR_MAGIC: &[u8; 8] = b"BPTSEL01";
const COUNTER_MAGIC: &[u8; 8] = b"BPTSEQ01";
const GENERATION_MAGIC: &[u8; 8] = b"BPTGEN01";
const FORMAT_VERSION: u16 = 1;
const SELECTOR_BYTES: usize = 8 + 2 + 8 + 8 + 32 + 32;
const COUNTER_BYTES: usize = 8 + 2 + 8 + 32;
const CHECKSUM_BYTES: usize = 32;
const MAX_GENERATIONS: usize = 32;
const MAX_NAMESPACE_ENTRIES: usize = MAX_GENERATIONS + 4;
const LOCK_WAIT: Duration = Duration::from_secs(5);
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// One monotonically allocated, never-reused mutable projection generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectionGenerationId(u64);

impl ProjectionGenerationId {
    pub(crate) const fn from_raw(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }

    fn name(self) -> String {
        format!("g{:016x}", self.0)
    }

    fn parse_name(name: &str) -> Option<Self> {
        let digits = name.strip_prefix('g')?;
        if digits.len() != 16
            || !digits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        let value = u64::from_str_radix(digits, 16).ok()?;
        Self::from_raw(value)
    }
}

/// A selected immutable seed for one mutable SQL generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProjectionSelector {
    pub(crate) generation: ProjectionGenerationId,
    pub(crate) schema_version: i64,
    pub(crate) seed_witness: [u8; 32],
}

impl ProjectionSelector {
    pub(crate) fn encode(self) -> [u8; SELECTOR_BYTES] {
        let mut bytes = [0; SELECTOR_BYTES];
        bytes[..8].copy_from_slice(SELECTOR_MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes[10..18].copy_from_slice(&self.generation.get().to_be_bytes());
        bytes[18..26].copy_from_slice(&self.schema_version.to_be_bytes());
        bytes[26..58].copy_from_slice(&self.seed_witness);
        let checksum = checksum(b"backend.turso.projection-selector.v1", &bytes[..58]);
        bytes[58..].copy_from_slice(&checksum);
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, ProjectionError> {
        if bytes.len() != SELECTOR_BYTES
            || &bytes[..8] != SELECTOR_MAGIC
            || u16::from_be_bytes(bytes[8..10].try_into().map_err(|_| corrupt("selector"))?)
                != FORMAT_VERSION
            || bytes[58..]
                != checksum(b"backend.turso.projection-selector.v1", &bytes[..58])
        {
            return Err(corrupt("selector"));
        }
        let generation = u64::from_be_bytes(
            bytes[10..18]
                .try_into()
                .map_err(|_| corrupt("selector_generation"))?,
        );
        let generation = ProjectionGenerationId::from_raw(generation)
            .ok_or_else(|| corrupt("selector_generation"))?;
        let schema_version = i64::from_be_bytes(
            bytes[18..26]
                .try_into()
                .map_err(|_| corrupt("selector_schema"))?,
        );
        if schema_version <= 0 {
            return Err(corrupt("selector_schema"));
        }
        let mut seed_witness = [0; 32];
        seed_witness.copy_from_slice(&bytes[26..58]);
        Ok(Self {
            generation,
            schema_version,
            seed_witness,
        })
    }
}

/// An open projection namespace. Its root and gate are stable for its lifetime.
#[derive(Clone)]
pub(crate) struct ProjectionNamespace {
    path: PathBuf,
    directory: DirectoryCapability,
    generations_path: PathBuf,
    generations: DirectoryCapability,
    gate_identity: FileIdentity,
}

impl ProjectionNamespace {
    pub(crate) fn open(database_stem: &Path) -> Result<Self, ProjectionError> {
        let parent_path = database_stem
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file_name = database_stem
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ProjectionError::NonUtf8Path(database_stem.to_path_buf()))?;
        let namespace_name = format!("{file_name}{NAMESPACE_SUFFIX}");
        let parent = DirectoryCapability::open(parent_path).map_err(filesystem)?;
        let directory = open_or_create_private_child(&parent, &namespace_name)?;
        let generations = open_or_create_private_child(&directory, GENERATIONS_NAME)?;
        let gate = directory
            .open_private_file_read_write(GATE_NAME, true)
            .map_err(filesystem)?;
        let gate_identity = FileIdentity::of_file(&gate).map_err(filesystem)?;
        let path = parent_path.join(namespace_name);
        let generations_path = path.join(GENERATIONS_NAME);
        Ok(Self {
            path,
            directory,
            generations_path,
            generations,
            gate_identity,
        })
    }

    pub(crate) fn selected(&self) -> Result<Option<ProjectionSelector>, ProjectionError> {
        let _gate = self.lock_shared()?;
        self.selected_unlocked()
    }

    pub(crate) fn generation_directory(
        &self,
        id: ProjectionGenerationId,
    ) -> Result<DirectoryCapability, ProjectionError> {
        self.generations
            .open_private_dir(&id.name())
            .map_err(filesystem)
    }

    pub(crate) fn generation_path(&self, id: ProjectionGenerationId) -> PathBuf {
        self.generations_path.join(id.name())
    }

    pub(crate) fn database_path(&self, id: ProjectionGenerationId) -> Result<PathBuf, ProjectionError> {
        let path = self.generation_path(id).join("projection.turso");
        if path.to_str().is_none() {
            return Err(ProjectionError::NonUtf8Path(path));
        }
        Ok(path)
    }

    pub(crate) fn generation_marker_path(&self, id: ProjectionGenerationId) -> PathBuf {
        self.generation_path(id).join(IDENTITY_NAME)
    }

    pub(crate) fn generation_pin(
        &self,
        directory: &DirectoryCapability,
        create: bool,
    ) -> Result<File, ProjectionError> {
        let file = directory
            .open_private_file_read_write(LEASE_NAME, create)
            .map_err(filesystem)?;
        lock_file(file, true)
    }

    pub(crate) fn create_generation(
        &self,
        id: ProjectionGenerationId,
    ) -> Result<CreatedDirectory, ProjectionError> {
        match self.generations.create_private_dir_tracked(&id.name()) {
            Ok(directory) => Ok(directory),
            Err(DirectoryCreateFailure::NotCreated(source)) => Err(filesystem(source)),
            Err(DirectoryCreateFailure::CreatedButUnready { directory, source }) => {
                let cleanup = directory.remove_all(MAX_NAMESPACE_ENTRIES);
                match cleanup {
                    Ok(()) => Err(filesystem(source)),
                    Err(cleanup) => Err(filesystem(std::io::Error::other(format!(
                        "generation creation failed ({source}) and exact rollback failed ({cleanup})"
                    )))),
                }
            }
            Err(DirectoryCreateFailure::CreatedButUnpinned(source)) => Err(filesystem(
                std::io::Error::other(format!(
                    "generation was created but could not be pinned; refusing name-based cleanup: {source}"
                )),
            )),
        }
    }

    pub(crate) fn write_generation_identity(
        &self,
        directory: &DirectoryCapability,
        selector: ProjectionSelector,
    ) -> Result<FileIdentity, ProjectionError> {
        let bytes = encode_generation(selector);
        let mut file = directory
            .create_file_exclusive(IDENTITY_NAME)
            .map_err(filesystem)?;
        file.write_all(&bytes).map_err(filesystem)?;
        file.sync_all().map_err(filesystem)?;
        directory.sync_all().map_err(filesystem)?;
        FileIdentity::of_file(&file).map_err(filesystem)
    }

    pub(crate) fn verify_generation_identity(
        &self,
        directory: &DirectoryCapability,
        selector: ProjectionSelector,
        expected_identity: Option<FileIdentity>,
    ) -> Result<FileIdentity, ProjectionError> {
        let mut file = directory.open_file_read(IDENTITY_NAME).map_err(filesystem)?;
        let identity = FileIdentity::of_file(&file).map_err(filesystem)?;
        let bytes = read_exact_bounded(&mut file, SELECTOR_BYTES)?;
        let actual = decode_generation(&bytes)?;
        if actual != selector {
            return Err(corrupt("generation_identity"));
        }
        if expected_identity.is_some_and(|expected| expected != identity) {
            return Err(ProjectionError::NamespaceIdentity);
        }
        let named = FileIdentity::of_path_nofollow(&self.generation_marker_path(selector.generation))
            .map_err(filesystem)?;
        if named != identity {
            return Err(ProjectionError::NamespaceIdentity);
        }
        Ok(identity)
    }

    /// Allocates and persists a new sequence while holding the stable gate.
    /// IDs are monotonic; the bounded retained namespace is never name-reaped.
    pub(crate) fn allocate_generation(
        &self,
        expected: Option<ProjectionSelector>,
    ) -> Result<ProjectionGenerationId, ProjectionError> {
        let _gate = self.lock_exclusive()?;
        let selected = self.selected_unlocked()?;
        if selected != expected {
            return Err(ProjectionError::StaleTransition);
        }
        let entries = self
            .generations
            .entries(MAX_GENERATIONS.saturating_add(1))
            .map_err(filesystem)?;
        if entries.len() >= MAX_GENERATIONS {
            return Err(ProjectionError::GenerationLimit {
                maximum: MAX_GENERATIONS,
            });
        }
        let mut largest = selected.map_or(0, |selector| selector.generation.get());
        for entry in entries {
            if entry.kind != EntryKind::Directory {
                return Err(corrupt("generation_entry_kind"));
            }
            let name = entry
                .name
                .to_str()
                .ok_or_else(|| corrupt("generation_name"))?;
            let id = ProjectionGenerationId::parse_name(name)
                .ok_or_else(|| corrupt("generation_name"))?;
            largest = largest.max(id.get());
        }
        largest = largest.max(self.read_counter()?);
        let next = largest
            .checked_add(1)
            .and_then(ProjectionGenerationId::from_raw)
            .ok_or(ProjectionError::GenerationIdExhausted)?;
        self.write_counter(next.get())?;
        Ok(next)
    }

    pub(crate) fn publish(
        &self,
        expected: Option<ProjectionSelector>,
        next: ProjectionSelector,
    ) -> Result<PublishOutcome, ProjectionError> {
        let _gate = self.lock_exclusive()?;
        if self.selected_unlocked()? != expected {
            return Ok(PublishOutcome::Conflict(self.selected_unlocked()?));
        }
        let directory = self.generation_directory(next.generation)?;
        self.verify_generation_identity(&directory, next, None)?;
        let bytes = next.encode();
        self.replace_small_file(SELECTOR_NAME, &bytes)?;
        if self.selected_unlocked()? != Some(next) {
            return Err(ProjectionError::PublicationIndeterminate);
        }
        Ok(PublishOutcome::Published)
    }

    pub(crate) fn operation_guard(
        &self,
        selector: ProjectionSelector,
        marker_identity: FileIdentity,
    ) -> Result<ProjectionOperationGuard, ProjectionError> {
        let gate = self.lock_shared()?;
        if self.selected_unlocked()? != Some(selector) {
            return Err(ProjectionError::SupersededGeneration);
        }
        let directory = self.generation_directory(selector.generation)?;
        self.verify_generation_identity(&directory, selector, Some(marker_identity))?;
        Ok(ProjectionOperationGuard { _gate: gate })
    }

    fn selected_unlocked(&self) -> Result<Option<ProjectionSelector>, ProjectionError> {
        let mut file = match self.directory.open_file_read(SELECTOR_NAME) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(filesystem(error)),
        };
        let bytes = read_exact_bounded(&mut file, SELECTOR_BYTES)?;
        ProjectionSelector::decode(&bytes).map(Some)
    }

    fn lock_shared(&self) -> Result<File, ProjectionError> {
        self.lock(true)
    }

    fn lock_exclusive(&self) -> Result<File, ProjectionError> {
        self.lock(false)
    }

    fn lock(&self, shared: bool) -> Result<File, ProjectionError> {
        let file = self
            .directory
            .open_private_file_read_write(GATE_NAME, false)
            .map_err(filesystem)?;
        if FileIdentity::of_file(&file).map_err(filesystem)? != self.gate_identity {
            return Err(ProjectionError::NamespaceIdentity);
        }
        lock_file(file, shared)
    }

    fn read_counter(&self) -> Result<u64, ProjectionError> {
        let mut file = match self.directory.open_file_read(COUNTER_NAME) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(filesystem(error)),
        };
        let bytes = read_exact_bounded(&mut file, COUNTER_BYTES)?;
        if &bytes[..8] != COUNTER_MAGIC
            || u16::from_be_bytes(bytes[8..10].try_into().map_err(|_| corrupt("counter"))?)
                != FORMAT_VERSION
            || bytes[18..]
                != checksum(b"backend.turso.projection-counter.v1", &bytes[..18])
        {
            return Err(corrupt("counter"));
        }
        Ok(u64::from_be_bytes(
            bytes[10..18]
                .try_into()
                .map_err(|_| corrupt("counter"))?,
        ))
    }

    fn write_counter(&self, value: u64) -> Result<(), ProjectionError> {
        let mut bytes = [0; COUNTER_BYTES];
        bytes[..8].copy_from_slice(COUNTER_MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes[10..18].copy_from_slice(&value.to_be_bytes());
        let checksum = checksum(b"backend.turso.projection-counter.v1", &bytes[..18]);
        bytes[18..].copy_from_slice(&checksum);
        self.replace_small_file(COUNTER_NAME, &bytes)
    }

    fn replace_small_file(&self, destination: &str, bytes: &[u8]) -> Result<(), ProjectionError> {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = format!(".publish-{}-{sequence}.tmp", std::process::id());
        let mut file = self
            .directory
            .create_file_exclusive(&temporary)
            .map_err(filesystem)?;
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = result {
            let _ = self.directory.remove_file(&temporary);
            return Err(filesystem(error));
        }
        match self
            .directory
            .rename_with_outcome(&temporary, destination, true)
        {
            Ok(()) => Ok(()),
            Err(DirectoryRenameError::NotCommitted(error)) => {
                let _ = self.directory.remove_file(&temporary);
                Err(filesystem(error))
            }
            Err(DirectoryRenameError::CommittedButNotDurable(_)) => {
                Err(ProjectionError::PublicationIndeterminate)
            }
        }
    }
}

pub(crate) enum PublishOutcome {
    Published,
    Conflict(Option<ProjectionSelector>),
}

/// RAII fence held across one bounded SQL read or writer transaction.
pub(crate) struct ProjectionOperationGuard {
    _gate: File,
}

fn open_or_create_private_child(
    parent: &DirectoryCapability,
    name: &str,
) -> Result<DirectoryCapability, ProjectionError> {
    match parent.open_private_dir(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match parent.create_private_dir(name) {
                Ok(directory) => Ok(directory),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    parent.open_private_dir(name).map_err(filesystem)
                }
                Err(error) => Err(filesystem(error)),
            }
        }
        Err(error) => Err(filesystem(error)),
    }
}

fn lock_file(mut file: File, shared: bool) -> Result<File, ProjectionError> {
    let started = Instant::now();
    loop {
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(TryLockError::WouldBlock) => return Err(ProjectionError::NamespaceBusy),
            Err(TryLockError::Error(error)) => return Err(filesystem(error)),
        }
    }
}

fn read_exact_bounded(file: &mut File, expected: usize) -> Result<Vec<u8>, ProjectionError> {
    if file.metadata().map_err(filesystem)?.len()
        != u64::try_from(expected).map_err(|_| corrupt("file_length"))?
    {
        return Err(corrupt("file_length"));
    }
    let limit = u64::try_from(expected)
        .map_err(|_| corrupt("file_length"))?
        .saturating_add(1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| ProjectionError::Allocation)?;
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(filesystem)?;
    if bytes.len() != expected {
        return Err(corrupt("file_length"));
    }
    Ok(bytes)
}

fn encode_generation(selector: ProjectionSelector) -> [u8; SELECTOR_BYTES] {
    let mut bytes = selector.encode();
    bytes[..8].copy_from_slice(GENERATION_MAGIC);
    let checksum = checksum(b"backend.turso.projection-generation.v1", &bytes[..58]);
    bytes[58..].copy_from_slice(&checksum);
    bytes
}

fn decode_generation(bytes: &[u8]) -> Result<ProjectionSelector, ProjectionError> {
    if bytes.len() != SELECTOR_BYTES
        || &bytes[..8] != GENERATION_MAGIC
        || u16::from_be_bytes(bytes[8..10].try_into().map_err(|_| corrupt("generation"))?)
            != FORMAT_VERSION
        || bytes[58..]
            != checksum(b"backend.turso.projection-generation.v1", &bytes[..58])
    {
        return Err(corrupt("generation"));
    }
    let mut selector = bytes.to_vec();
    selector[..8].copy_from_slice(SELECTOR_MAGIC);
    let checksum = checksum(b"backend.turso.projection-selector.v1", &selector[..58]);
    selector[58..].copy_from_slice(&checksum);
    ProjectionSelector::decode(&selector)
}

fn checksum(domain: &'static [u8], bytes: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = blake3::Hasher::new_derive_key(std::str::from_utf8(domain).unwrap_or("backend.turso.projection.invalid"));
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn corrupt(field: &'static str) -> ProjectionError {
    ProjectionError::CorruptNamespace { field }
}

fn filesystem(error: std::io::Error) -> ProjectionError {
    ProjectionError::Filesystem(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_and_generation_names_are_canonical_and_bounded() {
        let selector = ProjectionSelector {
            generation: ProjectionGenerationId::from_raw(17).expect("nonzero generation"),
            schema_version: 8,
            seed_witness: [0xA5; 32],
        };
        let encoded = selector.encode();
        assert_eq!(ProjectionSelector::decode(&encoded), Ok(selector));
        assert_eq!(decode_generation(&encode_generation(selector)), Ok(selector));
        assert_eq!(selector.generation.name(), "g0000000000000011");
        assert_eq!(ProjectionGenerationId::parse_name("g0000000000000011"), Some(selector.generation));
        assert_eq!(ProjectionGenerationId::parse_name("g000000000000001A"), None);
        assert_eq!(ProjectionGenerationId::parse_name("../selected"), None);
        let mut trailing = encoded.to_vec();
        trailing.push(0);
        assert!(ProjectionSelector::decode(&trailing).is_err());
        let mut corrupt = encoded;
        corrupt[25] ^= 1;
        assert!(ProjectionSelector::decode(&corrupt).is_err());
    }
}
