//! Process-shared generation selection for the rebuildable SQL projection.
//!
//! A selected projection is mutable, so its generation name is never reused.
//! The fixed selector contains only a generation number, schema, and seed
//! witness; it never supplies a filesystem path. Replacements are populated
//! privately and selected by one atomic file rename. Old roots are retained
//! until the platform exposes an exact opened-child cleanup receipt.

use crate::ProjectionError;
use backend_library::{CheckedPackageGraphFacts, ViewRoot};
use backend_platform::{
    CreatedDirectory, DirectoryCapability, DirectoryCreateFailure, DirectoryRenameError, EntryKind,
    FileIdentity,
};
use std::fs::{File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(test)]
std::thread_local! {
    static AFTER_GATE_LOCK_BLOCKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_after_gate_lock_blocked_test_hook(hook: impl FnOnce() + 'static) {
    AFTER_GATE_LOCK_BLOCKED.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_after_gate_lock_blocked_test_hook() {
    let hook = AFTER_GATE_LOCK_BLOCKED.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

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
const SELECTOR_BYTES: usize = 8 + 2 + 8 + 8 + 32 + 32 + 1 + 32 + 32;
const COUNTER_BYTES: usize = 8 + 2 + 8 + 32;
const CHECKSUM_BYTES: usize = 32;
const SELECTOR_PAYLOAD_BYTES: usize = SELECTOR_BYTES - CHECKSUM_BYTES;
const MAX_GENERATIONS: usize = 32;
const MAX_NAMESPACE_ENTRIES: usize = MAX_GENERATIONS + 24;
const MAX_STAGE_CLEANUP_ENTRIES: usize = 256;
const LOCK_WAIT: Duration = Duration::from_secs(5);
const NAMESPACE_BOOTSTRAP_RETRY: Duration = Duration::from_millis(10);
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// One monotonically allocated, never-reused mutable projection generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectionGenerationId(u64);

impl ProjectionGenerationId {
    pub(crate) const fn from_raw(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the stable positive sequence value.
    #[must_use]
    pub const fn get(self) -> u64 {
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

/// Dependency-fact completeness supplied when a selected generation must be
/// initialized or rebuilt.
#[derive(Clone, Copy, Debug)]
pub enum ProjectionGraphSeed<'a> {
    /// A checked complete fact set, including a checked empty set when the
    /// producer proved that no source facts exist.
    Checked(&'a CheckedPackageGraphFacts),
    /// No checked graph snapshot is available. The projection records no graph
    /// metadata, so graph reads fail closed instead of claiming known-empty.
    Unavailable,
}

/// Complete immutable inputs required before publishing the first generation.
#[derive(Clone, Copy, Debug)]
pub struct ProjectionSeed<'a> {
    /// Complete view root whose rows seed the projection.
    pub view: &'a ViewRoot,
    /// Explicit graph-fact completeness for this seed.
    pub graph: ProjectionGraphSeed<'a>,
}

impl<'a> ProjectionSeed<'a> {
    /// Constructs a complete projection seed.
    #[must_use]
    pub const fn new(view: &'a ViewRoot, graph: ProjectionGraphSeed<'a>) -> Self {
        Self { view, graph }
    }
}

/// The immutable first-seed witness for one mutable SQL generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProjectionSelector {
    pub(crate) generation: ProjectionGenerationId,
    pub(crate) schema_version: i64,
    pub(crate) seed_view_root: [u8; 32],
    pub(crate) seed_view_version: [u8; 32],
    pub(crate) graph_state: u8,
    pub(crate) graph_witness: [u8; 32],
}

impl ProjectionSelector {
    pub(crate) fn for_seed(generation: ProjectionGenerationId, seed: ProjectionSeed<'_>) -> Self {
        let (graph_state, graph_witness) = match seed.graph {
            ProjectionGraphSeed::Checked(facts) => (1, facts.witness()),
            ProjectionGraphSeed::Unavailable => (0, [0; 32]),
        };
        Self {
            generation,
            schema_version: crate::schema::SCHEMA_VERSION,
            seed_view_root: *seed.view.root().as_bytes(),
            seed_view_version: *seed.view.version().as_bytes(),
            graph_state,
            graph_witness,
        }
    }

    pub(crate) fn encode(self) -> [u8; SELECTOR_BYTES] {
        let mut bytes = [0; SELECTOR_BYTES];
        bytes[..8].copy_from_slice(SELECTOR_MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes[10..18].copy_from_slice(&self.generation.get().to_be_bytes());
        bytes[18..26].copy_from_slice(&self.schema_version.to_be_bytes());
        bytes[26..58].copy_from_slice(&self.seed_view_root);
        bytes[58..90].copy_from_slice(&self.seed_view_version);
        bytes[90] = self.graph_state;
        bytes[91..123].copy_from_slice(&self.graph_witness);
        let checksum = checksum(
            "backend.turso.projection-selector.v1",
            &bytes[..SELECTOR_PAYLOAD_BYTES],
        );
        bytes[SELECTOR_PAYLOAD_BYTES..].copy_from_slice(&checksum);
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, ProjectionError> {
        if bytes.len() != SELECTOR_BYTES
            || &bytes[..8] != SELECTOR_MAGIC
            || u16::from_be_bytes(bytes[8..10].try_into().map_err(|_| corrupt("selector"))?)
                != FORMAT_VERSION
            || bytes[SELECTOR_PAYLOAD_BYTES..]
                != checksum(
                    "backend.turso.projection-selector.v1",
                    &bytes[..SELECTOR_PAYLOAD_BYTES],
                )
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
        let mut seed_view_root = [0; 32];
        seed_view_root.copy_from_slice(&bytes[26..58]);
        let mut seed_view_version = [0; 32];
        seed_view_version.copy_from_slice(&bytes[58..90]);
        let graph_state = bytes[90];
        let mut graph_witness = [0; 32];
        graph_witness.copy_from_slice(&bytes[91..123]);
        if graph_state > 1 || (graph_state == 0 && graph_witness != [0; 32]) {
            return Err(corrupt("selector_graph_state"));
        }
        Ok(Self {
            generation,
            schema_version,
            seed_view_root,
            seed_view_version,
            graph_state,
            graph_witness,
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
    /// Opens an already initialized namespace without creating or repairing
    /// any path. Plain `TursoProjection::open` uses this path.
    pub(crate) fn open_existing(database_stem: &Path) -> Result<Self, ProjectionError> {
        Self::open_until_ready(database_stem, false)
    }

    /// Opens a namespace for the only API allowed to initialize a generation.
    /// A missing namespace is created once; an existing partial namespace is
    /// refused rather than silently repaired.
    pub(crate) fn open_for_seed(database_stem: &Path) -> Result<Self, ProjectionError> {
        Self::open_until_ready(database_stem, true)
    }

    fn open_until_ready(
        database_stem: &Path,
        create_namespace: bool,
    ) -> Result<Self, ProjectionError> {
        let started = Instant::now();
        loop {
            match Self::open_impl(database_stem, create_namespace) {
                Err(ProjectionError::CorruptNamespace {
                    field: "missing_gate" | "missing_generations" | "namespace_incomplete",
                }) if started.elapsed() < LOCK_WAIT => {
                    std::thread::sleep(NAMESPACE_BOOTSTRAP_RETRY);
                }
                Err(ProjectionError::CorruptNamespace {
                    field: "missing_gate" | "missing_generations" | "namespace_incomplete",
                }) => return Err(corrupt("namespace_initialization_timeout")),
                result => return result,
            }
        }
    }

    fn open_impl(database_stem: &Path, create_namespace: bool) -> Result<Self, ProjectionError> {
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
        let (directory, fresh, receipt) = match parent.open_private_dir(&namespace_name) {
            Ok(directory) => (directory, false, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_namespace => {
                // Namespace bootstrap is serialized by the parent's
                // exclusive child creation. On Unix the platform must reopen
                // the name after `mkdirat` to obtain its capability; a hostile
                // same-UID replacement in that pre-pin window is outside this
                // API's guarantee. This protocol coordinates cooperating
                // workspace processes, not mutually hostile processes running
                // as the same user. Once the gate is pinned, all selector
                // admission and publication is fenced through its identity.
                match parent.create_private_dir_tracked(&namespace_name) {
                    Ok(created) => (created.capability().clone(), true, Some(created)),
                    Err(DirectoryCreateFailure::NotCreated(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        (
                            parent.open_private_dir(&namespace_name).map_err(|error| {
                                if error.kind() == std::io::ErrorKind::NotFound {
                                    corrupt("namespace_inventory")
                                } else {
                                    filesystem(error)
                                }
                            })?,
                            false,
                            None,
                        )
                    }
                    Err(DirectoryCreateFailure::NotCreated(error))
                    | Err(DirectoryCreateFailure::CreatedButUnpinned(error)) => {
                        return Err(filesystem(error));
                    }
                    Err(DirectoryCreateFailure::CreatedButUnready { directory, source }) => {
                        let cleanup = directory.remove_all(MAX_STAGE_CLEANUP_ENTRIES);
                        return Err(match cleanup {
                            Ok(()) => filesystem(source),
                            Err(cleanup) => ProjectionError::StageCleanup {
                                cause: source.to_string(),
                                cleanup,
                            },
                        });
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProjectionError::NeedsSeed);
            }
            Err(error) => return Err(filesystem(error)),
        };
        let built = (|| {
            let generations = if fresh {
                directory
                    .create_private_dir(GENERATIONS_NAME)
                    .map_err(filesystem)?
            } else {
                directory
                    .open_private_dir(GENERATIONS_NAME)
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            corrupt("missing_generations")
                        } else {
                            filesystem(error)
                        }
                    })?
            };
            let gate = directory
                .open_private_file_read_write(GATE_NAME, fresh)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        corrupt("missing_gate")
                    } else {
                        filesystem(error)
                    }
                })?;
            let gate_identity = FileIdentity::of_file(&gate).map_err(filesystem)?;
            if fresh {
                // `open_private_file_read_write(..., true)` creates the gate
                // but does not flush its directory entry. Persist the complete
                // initial namespace scaffold before returning a capability
                // that can allocate or publish a generation.
                directory.sync_all().map_err(filesystem)?;
            }
            let path = parent_path.join(namespace_name);
            let generations_path = path.join(GENERATIONS_NAME);
            let namespace = Self {
                path,
                directory,
                generations_path,
                generations,
                gate_identity,
            };
            namespace.validate_inventory()?;
            Ok(namespace)
        })();
        match built {
            Ok(namespace) => {
                drop(receipt);
                Ok(namespace)
            }
            Err(error) => {
                if let Some(receipt) = receipt {
                    match receipt.remove_all(MAX_STAGE_CLEANUP_ENTRIES) {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(ProjectionError::StageCleanup {
                            cause: error.to_string(),
                            cleanup,
                        }),
                    }
                } else {
                    Err(error)
                }
            }
        }
    }

    fn validate_inventory(&self) -> Result<(), ProjectionError> {
        let entries = self
            .directory
            .entries(MAX_NAMESPACE_ENTRIES)
            .map_err(filesystem)?;
        let mut saw_generations = false;
        let mut saw_gate = false;
        let mut temporary_count = 0_usize;
        for entry in entries {
            let Some(name) = entry.name.to_str() else {
                return Err(corrupt("namespace_name"));
            };
            match name {
                GENERATIONS_NAME if entry.kind == EntryKind::Directory => saw_generations = true,
                GATE_NAME if entry.kind == EntryKind::File => saw_gate = true,
                SELECTOR_NAME | COUNTER_NAME if entry.kind == EntryKind::File => {}
                _ if is_temporary_name(name) && entry.kind == EntryKind::File => {
                    temporary_count = temporary_count.saturating_add(1);
                    if temporary_count > 16 {
                        return Err(corrupt("temporary_entry_limit"));
                    }
                }
                _ => return Err(corrupt("namespace_inventory")),
            }
        }
        if !saw_generations || !saw_gate {
            return Err(corrupt("namespace_incomplete"));
        }
        let entries = self
            .generations
            .entries(MAX_GENERATIONS.saturating_add(1))
            .map_err(filesystem)?;
        if entries.len() > MAX_GENERATIONS {
            return Err(ProjectionError::GenerationLimit {
                maximum: MAX_GENERATIONS,
            });
        }
        for entry in entries {
            if entry.kind != EntryKind::Directory
                || entry
                    .name
                    .to_str()
                    .and_then(ProjectionGenerationId::parse_name)
                    .is_none()
            {
                return Err(corrupt("generation_inventory"));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_selected_schema_for_test(
        &self,
        schema_version: i64,
    ) -> Result<(), ProjectionError> {
        if schema_version <= 0 || schema_version == crate::schema::SCHEMA_VERSION {
            return Err(ProjectionError::StaleTransition);
        }
        let _gate = self.lock_exclusive()?;
        let current = self
            .selected_unlocked()?
            .ok_or(ProjectionError::NeedsSeed)?;
        let downgraded = ProjectionSelector {
            schema_version,
            ..current
        };
        let directory = self.generation_directory(current.generation)?;
        let mut marker = directory
            .open_private_file_read_write(IDENTITY_NAME, false)
            .map_err(filesystem)?;
        marker.set_len(0).map_err(filesystem)?;
        marker.seek(SeekFrom::Start(0)).map_err(filesystem)?;
        marker
            .write_all(&encode_generation(downgraded))
            .map_err(filesystem)?;
        marker.sync_all().map_err(filesystem)?;
        directory.sync_all().map_err(filesystem)?;
        self.replace_small_file(SELECTOR_NAME, &downgraded.encode())
    }

    #[cfg(test)]
    pub(crate) fn generation_count_for_test(&self) -> Result<usize, ProjectionError> {
        self.generations
            .entries(MAX_GENERATIONS.saturating_add(1))
            .map(|entries| entries.len())
            .map_err(filesystem)
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

    pub(crate) fn database_path(
        &self,
        id: ProjectionGenerationId,
    ) -> Result<PathBuf, ProjectionError> {
        let path = self.generation_path(id).join(crate::FILE_NAME);
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
        // `id` was durably reserved while holding the stable namespace gate.
        // No cooperating process can stage the same generation name; the
        // directory receipt is still required for any pre-publication cleanup.
        match self.generations.create_private_dir_tracked(&id.name()) {
            Ok(directory) => Ok(directory),
            Err(DirectoryCreateFailure::NotCreated(source)) => Err(filesystem(source)),
            Err(DirectoryCreateFailure::CreatedButUnready { directory, source }) => {
                let cleanup = directory.remove_all(MAX_NAMESPACE_ENTRIES);
                match cleanup {
                    Ok(()) => Err(filesystem(source)),
                    Err(cleanup) => Err(ProjectionError::StageCleanup {
                        cause: source.to_string(),
                        cleanup,
                    }),
                }
            }
            Err(DirectoryCreateFailure::CreatedButUnpinned(source)) => {
                Err(filesystem(std::io::Error::other(format!(
                    "generation was created but could not be pinned; refusing name-based cleanup: {source}"
                ))))
            }
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
        let mut file = directory
            .open_file_read(IDENTITY_NAME)
            .map_err(filesystem)?;
        let identity = FileIdentity::of_file(&file).map_err(filesystem)?;
        let bytes = read_exact_bounded(&mut file, SELECTOR_BYTES)?;
        let actual = decode_generation(&bytes)?;
        if actual != selector {
            return Err(corrupt("generation_identity"));
        }
        if expected_identity.is_some_and(|expected| expected != identity) {
            return Err(ProjectionError::NamespaceIdentity);
        }
        let named =
            FileIdentity::of_path_nofollow(&self.generation_marker_path(selector.generation))
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
        let current = self.selected_unlocked()?;
        if current != expected {
            return Ok(PublishOutcome::Conflict(current));
        }
        if next.schema_version != crate::schema::SCHEMA_VERSION
            || current.is_some_and(|selected| next.generation <= selected.generation)
        {
            return Err(ProjectionError::StaleTransition);
        }
        let directory = self.generation_directory(next.generation)?;
        self.verify_generation_identity(&directory, next, None)?;
        let bytes = next.encode();
        self.replace_small_file(SELECTOR_NAME, &bytes)?;
        // The rename is already committed when this read begins. Any failure
        // to prove the new selector is visible must preserve its generation:
        // the caller cannot safely clean a stage that may now be selected.
        match self.selected_unlocked() {
            Ok(Some(selected)) if selected == next => {}
            Ok(_) | Err(_) => return Err(ProjectionError::PublicationIndeterminate),
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
        self.verify_gate_identity(&file)?;
        let locked = lock_file(file, shared)?;
        // The lock may have waited behind another process. Verify both the
        // pinned handle and its current name after acquisition so a replaced
        // gate cannot authorize work under the old inode.
        self.verify_gate_identity(&locked)?;
        Ok(locked)
    }

    fn verify_gate_identity(&self, file: &File) -> Result<(), ProjectionError> {
        if FileIdentity::of_file(file).map_err(filesystem)? != self.gate_identity {
            return Err(ProjectionError::NamespaceIdentity);
        }
        if FileIdentity::of_path_nofollow(&self.path.join(GATE_NAME)).map_err(filesystem)?
            != self.gate_identity
        {
            return Err(ProjectionError::NamespaceIdentity);
        }
        Ok(())
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
            || bytes[18..] != checksum("backend.turso.projection-counter.v1", &bytes[..18])
        {
            return Err(corrupt("counter"));
        }
        Ok(u64::from_be_bytes(
            bytes[10..18].try_into().map_err(|_| corrupt("counter"))?,
        ))
    }

    fn write_counter(&self, value: u64) -> Result<(), ProjectionError> {
        let mut bytes = [0; COUNTER_BYTES];
        bytes[..8].copy_from_slice(COUNTER_MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes[10..18].copy_from_slice(&value.to_be_bytes());
        let checksum = checksum("backend.turso.projection-counter.v1", &bytes[..18]);
        bytes[18..].copy_from_slice(&checksum);
        self.replace_small_file(COUNTER_NAME, &bytes)
    }

    fn replace_small_file(&self, destination: &str, bytes: &[u8]) -> Result<(), ProjectionError> {
        let mut created = None;
        for _ in 0..8 {
            let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0_u128, |duration| duration.as_nanos());
            let temporary = format!(
                ".publish-{}-{nanos:032x}-{sequence:016x}.tmp",
                std::process::id()
            );
            match self.directory.create_file_exclusive(&temporary) {
                Ok(file) => {
                    created = Some((temporary, file));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(filesystem(error)),
            }
        }
        let (temporary, mut file) = created.ok_or(corrupt("temporary_name_exhausted"))?;
        file.write_all(bytes).map_err(filesystem)?;
        file.sync_all().map_err(filesystem)?;
        drop(file);
        match self
            .directory
            .rename_with_outcome(&temporary, destination, true)
        {
            Ok(()) => {
                #[cfg(test)]
                if destination == SELECTOR_NAME
                    && std::env::var_os("BACKEND_TURSO_TEST_HOLD_AFTER_SELECTOR_RENAME").is_some()
                {
                    // The selector rename is committed and the parent
                    // directory was flushed. A process test can now kill this
                    // writer before the post-rename readback, exercising the
                    // ambiguous-return recovery boundary.
                    crate::process_harness::report("SELECTOR-RENAMED");
                    if crate::process_harness::commands().next().as_deref() != Some("GO") {
                        loop {
                            std::thread::park();
                        }
                    }
                }
                Ok(())
            }
            Err(DirectoryRenameError::NotCommitted(error)) => Err(filesystem(error)),
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

fn lock_file(mut file: File, shared: bool) -> Result<File, ProjectionError> {
    let started = Instant::now();
    #[cfg(test)]
    let mut reported_blocked = false;
    loop {
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => {
                #[cfg(test)]
                if !reported_blocked {
                    reported_blocked = true;
                    run_after_gate_lock_blocked_test_hook();
                }
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
    let checksum = checksum(
        "backend.turso.projection-generation.v1",
        &bytes[..SELECTOR_PAYLOAD_BYTES],
    );
    bytes[SELECTOR_PAYLOAD_BYTES..].copy_from_slice(&checksum);
    bytes
}

fn decode_generation(bytes: &[u8]) -> Result<ProjectionSelector, ProjectionError> {
    if bytes.len() != SELECTOR_BYTES
        || &bytes[..8] != GENERATION_MAGIC
        || u16::from_be_bytes(bytes[8..10].try_into().map_err(|_| corrupt("generation"))?)
            != FORMAT_VERSION
        || bytes[SELECTOR_PAYLOAD_BYTES..]
            != checksum(
                "backend.turso.projection-generation.v1",
                &bytes[..SELECTOR_PAYLOAD_BYTES],
            )
    {
        return Err(corrupt("generation"));
    }
    let mut selector = bytes.to_vec();
    selector[..8].copy_from_slice(SELECTOR_MAGIC);
    let checksum = checksum(
        "backend.turso.projection-selector.v1",
        &selector[..SELECTOR_PAYLOAD_BYTES],
    );
    selector[SELECTOR_PAYLOAD_BYTES..].copy_from_slice(&checksum);
    ProjectionSelector::decode(&selector)
}

fn checksum(domain: &'static str, bytes: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = blake3::Hasher::new_derive_key(domain);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn corrupt(field: &'static str) -> ProjectionError {
    ProjectionError::CorruptNamespace { field }
}

fn filesystem(error: std::io::Error) -> ProjectionError {
    ProjectionError::Filesystem(error)
}

fn is_temporary_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(".publish-") else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(".tmp") else {
        return false;
    };
    let mut fields = rest.split('-');
    let (Some(pid), Some(time), Some(sequence), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && time.len() == 32
        && time.bytes().all(|byte| byte.is_ascii_hexdigit())
        && sequence.len() == 16
        && sequence.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_and_generation_names_are_canonical_and_bounded() {
        let selector = ProjectionSelector {
            generation: ProjectionGenerationId::from_raw(17).expect("nonzero generation"),
            schema_version: 8,
            seed_view_root: [0x11; 32],
            seed_view_version: [0x22; 32],
            graph_state: 1,
            graph_witness: [0xA5; 32],
        };
        let encoded = selector.encode();
        assert!(matches!(
            ProjectionSelector::decode(&encoded),
            Ok(decoded) if decoded == selector
        ));
        assert!(matches!(
            decode_generation(&encode_generation(selector)),
            Ok(decoded) if decoded == selector
        ));
        assert_eq!(selector.generation.name(), "g0000000000000011");
        assert_eq!(
            ProjectionGenerationId::parse_name("g0000000000000011"),
            Some(selector.generation)
        );
        assert_eq!(
            ProjectionGenerationId::parse_name("g000000000000001A"),
            None
        );
        assert_eq!(ProjectionGenerationId::parse_name("../selected"), None);
        let mut trailing = encoded.to_vec();
        trailing.push(0);
        assert!(ProjectionSelector::decode(&trailing).is_err());
        let mut corrupt = encoded;
        corrupt[25] ^= 1;
        assert!(ProjectionSelector::decode(&corrupt).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn gate_replacement_while_waiting_is_refused_after_lock_acquisition() {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let stem = std::env::temp_dir().join(format!(
            "backend-turso-gate-race-{}-{sequence}.db",
            std::process::id()
        ));
        let namespace_path = stem.with_file_name(format!(
            "{}{NAMESPACE_SUFFIX}",
            stem.file_name()
                .and_then(|name| name.to_str())
                .expect("utf8 temp stem")
        ));
        let namespace = ProjectionNamespace::open_for_seed(&stem).expect("create namespace");
        let owner = namespace.lock_exclusive().expect("hold old gate");
        let (blocked_sender, blocked_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let waiting_namespace = namespace.clone();
        let waiter = std::thread::spawn(move || {
            set_after_gate_lock_blocked_test_hook(move || {
                blocked_sender.send(()).expect("report blocked lock");
                release_receiver.recv().expect("release blocked lock");
            });
            waiting_namespace.lock_shared()
        });

        blocked_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("waiter reached the OS lock wait");
        namespace
            .replace_small_file(GATE_NAME, b"replacement gate")
            .expect("replace gate pathname while old inode is locked");
        release_sender.send(()).expect("resume waiter");
        drop(owner);

        assert!(matches!(
            waiter.join().expect("join gate waiter"),
            Err(ProjectionError::NamespaceIdentity)
        ));
        drop(namespace);
        std::fs::remove_dir_all(namespace_path).expect("remove test namespace");
    }
}
