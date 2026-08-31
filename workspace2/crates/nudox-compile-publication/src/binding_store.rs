//! Deterministically addressed immutable generation-to-manifest bindings.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use thiserror::Error;

use crate::binding::{
    COMPILATION_BINDING_BYTES, CompilationBindingError, CompilationBindingFacts,
    CompilationBindingView,
};

const DIRECTORY: &str = "bindings";
const EXTENSION: &str = ".binding";
const HEX: &[u8; 16] = b"0123456789abcdef";
const TEMP_ATTEMPTS: u8 = 16;

/// Exact physical phase while storing or reopening a generation-addressed binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingIoPhase {
    /// Create the binding directory.
    CreateDirectory,
    /// Open an existing deterministic binding path.
    OpenExisting,
    /// Read an existing binding.
    ReadExisting,
    /// Create a same-directory temporary binding.
    CreateTemporary,
    /// Write binding bytes to a temporary file.
    WriteTemporary,
    /// Sync temporary binding bytes.
    SyncTemporary,
    /// Rename the temporary binding to its deterministic generation address.
    PublishTemporary,
    /// Open the binding directory for a durability barrier.
    OpenDirectory,
    /// Sync the binding directory after a namespace transition.
    SyncDirectory,
    /// Remove a failed temporary binding.
    RemoveTemporary,
}

/// Failure while storing or loading an immutable generation-addressed binding.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BindingStoreError {
    /// One exact binding filesystem phase failed.
    #[error("compiler binding I/O failed during {phase:?}")]
    Io {
        /// Exact physical phase.
        phase: BindingIoPhase,
        /// Original filesystem cause.
        #[source]
        source: io::Error,
    },
    /// Binding write failure and cleanup failure both remain available.
    #[error(
        "compiler binding I/O failed during {phase:?}; cleanup failed during {cleanup_phase:?}"
    )]
    IoWithCleanup {
        /// Primary write phase.
        phase: BindingIoPhase,
        /// Primary filesystem cause.
        #[source]
        source: io::Error,
        /// Cleanup phase.
        cleanup_phase: BindingIoPhase,
        /// Cleanup filesystem cause.
        cleanup_source: io::Error,
    },
    /// Existing deterministic binding did not have the complete fixed length.
    #[error("generation binding has {observed} bytes, expected {expected}")]
    Length {
        /// Fixed binding byte length required by this format.
        expected: usize,
        /// Observed file byte length.
        observed: u64,
    },
    /// Existing binding could not pass the immutable binding grammar.
    #[error("stored generation binding failed validation")]
    Binding(#[source] CompilationBindingError),
    /// Existing binding named a different verified generation than its deterministic path.
    #[error("stored generation binding facts disagree with its deterministic generation path")]
    GenerationMismatch {
        /// Exact cold mismatch facts retained in one shared diagnostic owner.
        facts: Arc<BindingGenerationMismatch>,
    },
    /// Existing binding bytes had a different complete immutable identity or manifest fact.
    #[error("stored generation binding facts conflict with the requested immutable binding")]
    BindingMismatch {
        /// Exact cold mismatch facts retained in one shared diagnostic owner.
        facts: Arc<BindingFactsMismatch>,
    },
    /// Every bounded temporary binding name collided with a pre-existing temporary path.
    #[error("could not allocate a temporary compiler binding after {attempts} attempts")]
    TemporaryNamesExhausted {
        /// Fixed bounded collision attempts.
        attempts: u8,
        /// Last exact create-new collision cause.
        #[source]
        source: io::Error,
    },
}

/// Exact deterministic-address disagreement retained only on the cold binding error path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingGenerationMismatch {
    /// Generation facts derived from the deterministic path.
    pub expected: nudox_hydration::VerifiedGenerationFacts,
    /// Generation facts carried by immutable binding bytes.
    pub observed: nudox_hydration::VerifiedGenerationFacts,
}

/// Exact immutable binding conflict retained only on the cold storage error path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingFactsMismatch {
    /// Facts requested for the immutable generation address.
    pub expected: CompilationBindingFacts,
    /// Existing validated facts at that address.
    pub observed: CompilationBindingFacts,
}

/// Location and validated facts of a generation-addressed immutable binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredBinding {
    /// Validated complete immutable binding facts.
    pub facts: CompilationBindingFacts,
    /// Deterministic path derived only from the bound generation root and dependency set.
    pub path: PathBuf,
}

/// Single-owner store for deterministic `{pinned_root, dep_set}` binding paths.
#[derive(Debug)]
pub(crate) struct GenerationBindingStore {
    directory: PathBuf,
    next_temporary: u64,
}

impl GenerationBindingStore {
    pub(crate) fn existing(parent: &Path) -> Self {
        Self {
            directory: parent.join(DIRECTORY),
            next_temporary: initial_nonce(),
        }
    }

    /// Opens or creates the binding directory without creating a selected generation.
    pub(crate) fn new(parent: &Path) -> Result<Self, BindingStoreError> {
        fs::create_dir_all(parent).map_err(|source| BindingStoreError::Io {
            phase: BindingIoPhase::CreateDirectory,
            source,
        })?;
        let directory = parent.join(DIRECTORY);
        match fs::create_dir(&directory) {
            Ok(()) => sync_directory(&directory)
                .map_err(|(phase, source)| BindingStoreError::Io { phase, source })?,
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(BindingStoreError::Io {
                    phase: BindingIoPhase::CreateDirectory,
                    source,
                });
            }
        }
        Ok(Self {
            directory,
            next_temporary: initial_nonce(),
        })
    }

    /// Persists a fully validated binding at its deterministic generation address before journal
    /// submission. Existing bytes are independently validated and reused without rewriting.
    pub(crate) fn ensure(
        &mut self,
        binding: &CompilationBindingView<'_>,
    ) -> Result<StoredBinding, BindingStoreError> {
        let path = self.path_for(binding.generation);
        match self.read_existing(&path, binding.generation)? {
            Some(existing) if existing == **binding => {
                return Ok(StoredBinding {
                    facts: existing,
                    path,
                });
            }
            Some(existing) => {
                return Err(BindingStoreError::BindingMismatch {
                    facts: Arc::new(BindingFactsMismatch {
                        expected: **binding,
                        observed: existing,
                    }),
                });
            }
            None => {}
        }
        self.publish_missing(path, binding)
    }

    /// Loads the exact binding addressable only from journal-published generation facts.
    pub(crate) fn load(
        &self,
        generation: nudox_hydration::VerifiedGenerationFacts,
    ) -> Result<Option<StoredBinding>, BindingStoreError> {
        let path = self.path_for(generation);
        self.read_existing(&path, generation)
            .map(|facts| facts.map(|facts| StoredBinding { facts, path }))
    }

    fn read_existing(
        &self,
        path: &Path,
        expected_generation: nudox_hydration::VerifiedGenerationFacts,
    ) -> Result<Option<CompilationBindingFacts>, BindingStoreError> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(BindingStoreError::Io {
                    phase: BindingIoPhase::OpenExisting,
                    source,
                });
            }
        };
        let length = file
            .metadata()
            .map_err(|source| BindingStoreError::Io {
                phase: BindingIoPhase::ReadExisting,
                source,
            })?
            .len();
        if length != COMPILATION_BINDING_BYTES as u64 {
            return Err(BindingStoreError::Length {
                expected: COMPILATION_BINDING_BYTES,
                observed: length,
            });
        }
        let mut bytes = [0; COMPILATION_BINDING_BYTES];
        file.read_exact(&mut bytes)
            .map_err(|source| BindingStoreError::Io {
                phase: BindingIoPhase::ReadExisting,
                source,
            })?;
        let mut extra = [0_u8; 1];
        if file
            .read(&mut extra)
            .map_err(|source| BindingStoreError::Io {
                phase: BindingIoPhase::ReadExisting,
                source,
            })?
            != 0
        {
            let observed = file
                .metadata()
                .map_err(|source| BindingStoreError::Io {
                    phase: BindingIoPhase::ReadExisting,
                    source,
                })?
                .len();
            return Err(BindingStoreError::Length {
                expected: COMPILATION_BINDING_BYTES,
                observed,
            });
        }
        let post_read = file
            .metadata()
            .map_err(|source| BindingStoreError::Io {
                phase: BindingIoPhase::ReadExisting,
                source,
            })?
            .len();
        if post_read != COMPILATION_BINDING_BYTES as u64 {
            return Err(BindingStoreError::Length {
                expected: COMPILATION_BINDING_BYTES,
                observed: post_read,
            });
        }
        let binding =
            CompilationBindingView::validate(&bytes).map_err(BindingStoreError::Binding)?;
        if binding.generation != expected_generation {
            return Err(BindingStoreError::GenerationMismatch {
                facts: Arc::new(BindingGenerationMismatch {
                    expected: expected_generation,
                    observed: binding.generation,
                }),
            });
        }
        Ok(Some(*binding))
    }

    fn publish_missing(
        &mut self,
        final_path: PathBuf,
        binding: &CompilationBindingView<'_>,
    ) -> Result<StoredBinding, BindingStoreError> {
        let mut remaining = TEMP_ATTEMPTS;
        let (temporary_path, mut temporary) = loop {
            let temporary_path = self.temporary_path(&final_path);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
            {
                Ok(temporary) => break (temporary_path, temporary),
                Err(source) if source.kind() == ErrorKind::AlreadyExists => {
                    remaining -= 1;
                    if remaining == 0 {
                        return Err(BindingStoreError::TemporaryNamesExhausted {
                            attempts: TEMP_ATTEMPTS,
                            source,
                        });
                    }
                }
                Err(source) => {
                    return Err(BindingStoreError::Io {
                        phase: BindingIoPhase::CreateTemporary,
                        source,
                    });
                }
            }
        };
        if let Err(source) = temporary.write_all(binding.as_ref()) {
            return Err(self.with_cleanup(BindingIoPhase::WriteTemporary, source, &temporary_path));
        }
        if let Err(source) = temporary.sync_all() {
            return Err(self.with_cleanup(BindingIoPhase::SyncTemporary, source, &temporary_path));
        }
        drop(temporary);
        fs::rename(&temporary_path, &final_path).map_err(|source| {
            self.with_cleanup(BindingIoPhase::PublishTemporary, source, &temporary_path)
        })?;
        sync_directory(&self.directory)
            .map_err(|(phase, source)| BindingStoreError::Io { phase, source })?;
        Ok(StoredBinding {
            facts: **binding,
            path: final_path,
        })
    }

    fn with_cleanup(
        &self,
        phase: BindingIoPhase,
        source: io::Error,
        temporary: &Path,
    ) -> BindingStoreError {
        match fs::remove_file(temporary) {
            Ok(()) => match sync_directory(&self.directory) {
                Ok(()) => BindingStoreError::Io { phase, source },
                Err((cleanup_phase, cleanup_source)) => BindingStoreError::IoWithCleanup {
                    phase,
                    source,
                    cleanup_phase,
                    cleanup_source,
                },
            },
            Err(cleanup_source) => BindingStoreError::IoWithCleanup {
                phase,
                source,
                cleanup_phase: BindingIoPhase::RemoveTemporary,
                cleanup_source,
            },
        }
    }

    fn path_for(&self, generation: nudox_hydration::VerifiedGenerationFacts) -> PathBuf {
        self.directory.join(format!(
            "{}-{}{}",
            hexadecimal(generation.pinned_root.as_ref()),
            hexadecimal(generation.dep_set.as_ref()),
            EXTENSION,
        ))
    }

    fn temporary_path(&mut self, final_path: &Path) -> PathBuf {
        let nonce = self.next_temporary;
        self.next_temporary = self.next_temporary.wrapping_add(1);
        final_path.with_extension(format!("binding.tmp.{nonce}"))
    }
}

fn sync_directory(directory: &Path) -> Result<(), (BindingIoPhase, io::Error)> {
    let file = File::open(directory).map_err(|source| (BindingIoPhase::OpenDirectory, source))?;
    file.sync_all()
        .map_err(|source| (BindingIoPhase::SyncDirectory, source))
}

fn hexadecimal(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[usize::from(*byte >> 4)] as char);
        output.push(HEX[usize::from(*byte & 0x0f)] as char);
    }
    output
}

fn initial_nonce() -> u64 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        });
    time ^ u64::from(std::process::id()).rotate_left(17)
}
