//! Create, load, and publish generation-addressed compilation bindings.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

use crate::publication::binding::{COMPILATION_BINDING_BYTES, CompilationBindingView};

use super::*;

impl GenerationBindingStore {
    pub(crate) fn existing(parent: &Path) -> Self {
        Self {
            directory: parent.join(DIRECTORY),
            next_temporary: initial_nonce(),
        }
    }

    /// Opens or creates the binding directory without creating a selected generation.
    #[allow(
        clippy::result_large_err,
        reason = "cold exact binding conflicts retain both immutable facts without allocation or source erasure"
    )]
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
    #[allow(
        clippy::result_large_err,
        reason = "cold exact binding conflicts retain both immutable facts without allocation or source erasure"
    )]
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
                    expected: **binding,
                    observed: existing,
                });
            }
            None => {}
        }
        self.publish_missing(path, binding)
    }

    /// Loads the exact binding addressable only from journal-published generation facts.
    #[allow(
        clippy::result_large_err,
        reason = "cold exact binding conflicts retain both immutable facts without allocation or source erasure"
    )]
    pub(crate) fn load(
        &self,
        generation: backend_store::hydration::VerifiedGenerationFacts,
    ) -> Result<Option<StoredBinding>, BindingStoreError> {
        let path = self.path_for(generation);
        self.read_existing(&path, generation)
            .map(|facts| facts.map(|facts| StoredBinding { facts, path }))
    }

    #[allow(
        clippy::result_large_err,
        reason = "cold exact binding conflicts retain both immutable facts without allocation or source erasure"
    )]
    fn read_existing(
        &self,
        path: &Path,
        expected_generation: backend_store::hydration::VerifiedGenerationFacts,
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
                expected: expected_generation,
                observed: binding.generation,
            });
        }
        Ok(Some(*binding))
    }

    #[allow(
        clippy::result_large_err,
        reason = "cold exact binding conflicts retain both immutable facts without allocation or source erasure"
    )]
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

    fn path_for(&self, generation: backend_store::hydration::VerifiedGenerationFacts) -> PathBuf {
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
