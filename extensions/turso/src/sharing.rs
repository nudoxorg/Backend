//! The I/O backend every Turso database of this extension is opened through,
//! and the cross-process capability it must provide.
//!
//! Turso keeps a write-ahead log. Whether a second process may open the same
//! file while the first still has it open is a property of the I/O backend,
//! not of the database format:
//!
//! * A backend that can coordinate the log between processes through a
//!   memory-mapped `.tshm` file supports *shared* access: readers keep stable
//!   snapshots while one writer proceeds, and a crashed holder is recovered by
//!   process-liveness checks. Unix backends provide it. On Windows only the
//!   completion-port backend does; Windows' default backend refuses the mode.
//! * A backend without it can only offer *exclusive* access through an
//!   open-time advisory lock: the first process owns the file and every other
//!   process is refused.
//!
//! Every database this extension opens is reachable from more than one
//! process: the daemon, its compiler workers and the clients that observe a
//! workspace all open the authority and the projection. Exclusive access is
//! therefore not sufficient for the product, and it is deliberately not
//! offered: on Windows Turso detects neither an exclusive holder when a shared
//! opener arrives nor the reverse (verified with two real processes), so a
//! file opened in both ways would be written by two processes that each
//! believe they own it. The decision is made here, once, as a typed
//! capability: a platform that cannot coordinate a shared log is refused by
//! name instead of being quietly given a weaker mode.

use std::{fmt, sync::Arc};

use turso::core::IO;

/// The I/O backend a database is opened through.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IoBackendName {
    /// The operating system's default backend.
    Platform,
    /// Windows' completion-port backend, the only Windows backend that can
    /// coordinate a write-ahead log between processes.
    WindowsCompletionPort,
}

impl fmt::Display for IoBackendName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Platform => "platform",
            Self::WindowsCompletionPort => "windows completion port",
        })
    }
}

/// Why a database could not be opened with shared multiprocess write-ahead
/// logging.
#[derive(Debug)]
pub enum SharingRefusal {
    /// The backend cannot coordinate a write-ahead log between processes.
    SharedWalUnsupported {
        /// The backend that was found.
        backend: IoBackendName,
    },
    /// The backend that provides shared logging could not be created.
    BackendUnavailable {
        /// The backend that was needed.
        backend: IoBackendName,
        /// The operating system's or Turso's description of the failure.
        detail: String,
    },
    /// The volume that holds the database cannot host the shared coordination
    /// file (a network share, for example).
    UnsupportedVolume {
        /// Turso's description of the refused location.
        detail: String,
    },
}

impl fmt::Display for SharingRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SharedWalUnsupported { backend } => write!(
                formatter,
                "shared multiprocess write-ahead logging is not supported by the {backend} I/O backend"
            ),
            Self::BackendUnavailable { backend, detail } => {
                write!(
                    formatter,
                    "the {backend} I/O backend is unavailable: {detail}"
                )
            }
            Self::UnsupportedVolume { detail } => write!(
                formatter,
                "shared multiprocess write-ahead logging is not supported on this volume: {detail}"
            ),
        }
    }
}

impl std::error::Error for SharingRefusal {}

impl SharingRefusal {
    /// Recognises Turso's refusal to enable multiprocess logging for a
    /// location. Turso reports it only as a message, so the wording this
    /// recognises is pinned by tests (against the real library wherever the
    /// refusal can be provoked); any other error is not a sharing refusal and
    /// stays the caller's to report.
    pub(crate) fn from_open_error(backend: IoBackendName, error: &turso::Error) -> Option<Self> {
        let turso::Error::Error(message) = error else {
            return None;
        };
        if !message.contains("experimental multiprocess WAL is not supported") {
            return None;
        }
        Some(if message.contains("filesystem backing") {
            Self::UnsupportedVolume {
                detail: message.clone(),
            }
        } else {
            Self::SharedWalUnsupported { backend }
        })
    }
}

/// An I/O backend proven able to coordinate a write-ahead log between
/// processes.
#[derive(Clone)]
pub struct SharedWalBackend {
    name: IoBackendName,
    io: Arc<dyn IO>,
}

impl fmt::Debug for SharedWalBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedWalBackend")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl SharedWalBackend {
    /// Selects and creates this platform's shared-logging backend.
    ///
    /// # Errors
    ///
    /// Returns a [`SharingRefusal`] naming the missing capability when the
    /// platform has no backend that can coordinate a log between processes, or
    /// when that backend cannot be created.
    pub fn detect() -> Result<Self, SharingRefusal> {
        let (name, io) = platform_shared_wal_io()?;
        if io.supports_shared_wal_coordination() {
            Ok(Self { name, io })
        } else {
            Err(SharingRefusal::SharedWalUnsupported { backend: name })
        }
    }

    /// The backend databases are opened through.
    #[must_use]
    pub const fn name(&self) -> IoBackendName {
        self.name
    }

    /// A local-database builder configured for shared access through this
    /// backend.
    pub(crate) fn builder(&self, path: &str) -> turso::Builder {
        turso::Builder::new_local(path)
            .experimental_multiprocess_wal(true)
            .with_io_impl(Arc::clone(&self.io))
    }

    /// Builds the database, naming a sharing refusal instead of passing
    /// Turso's message through.
    pub(crate) async fn open_database(
        &self,
        builder: turso::Builder,
    ) -> Result<turso::Database, OpenFailure> {
        builder.build().await.map_err(|error| {
            match SharingRefusal::from_open_error(self.name, &error) {
                Some(refusal) => OpenFailure::Sharing(refusal),
                None => OpenFailure::Database(error),
            }
        })
    }
}

/// Why opening a database failed, split into the sharing capability and
/// everything else.
#[derive(Debug)]
pub(crate) enum OpenFailure {
    /// The shared-logging capability is missing for this database.
    Sharing(SharingRefusal),
    /// Turso failed for any other reason.
    Database(turso::Error),
}

#[cfg(windows)]
fn platform_shared_wal_io() -> Result<(IoBackendName, Arc<dyn IO>), SharingRefusal> {
    let backend = IoBackendName::WindowsCompletionPort;
    let io =
        turso::core::WindowsIOCP::new().map_err(|error| SharingRefusal::BackendUnavailable {
            backend,
            detail: error.to_string(),
        })?;
    Ok((backend, Arc::new(io)))
}

#[cfg(not(windows))]
fn platform_shared_wal_io() -> Result<(IoBackendName, Arc<dyn IO>), SharingRefusal> {
    let backend = IoBackendName::Platform;
    let io =
        turso::core::PlatformIO::new().map_err(|error| SharingRefusal::BackendUnavailable {
            backend,
            detail: error.to_string(),
        })?;
    Ok((backend, Arc::new(io)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn this_platform_provides_a_shared_wal_backend() {
        let backend = SharedWalBackend::detect().expect("shared WAL backend");
        if cfg!(windows) {
            assert_eq!(backend.name(), IoBackendName::WindowsCompletionPort);
        } else {
            assert_eq!(backend.name(), IoBackendName::Platform);
        }
    }

    #[test]
    fn the_platform_default_backend_is_not_what_provides_sharing_on_windows() {
        // The reason the backend is chosen explicitly: Windows' default backend
        // refuses the mode, so relying on it makes every open fail.
        let default = turso::core::PlatformIO::new().expect("default backend");
        assert_eq!(default.supports_shared_wal_coordination(), cfg!(unix));
    }

    #[test]
    fn an_unrelated_error_is_not_a_sharing_refusal() {
        for error in [
            turso::Error::Busy("database is locked".to_owned()),
            turso::Error::Error("Locking error: Failed locking file".to_owned()),
            turso::Error::Corrupt("bad page".to_owned()),
        ] {
            assert!(
                SharingRefusal::from_open_error(IoBackendName::Platform, &error).is_none(),
                "{error}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_real_windows_default_backend_refusal_is_recognised_by_name() {
        // This is the failure every open had before the completion-port backend
        // was selected explicitly, provoked through the real library.
        let path = std::env::temp_dir().join(format!(
            "backend-turso-default-backend-{}.db",
            std::process::id()
        ));
        let error = futures_executor::block_on(
            turso::Builder::new_local(path.to_str().expect("utf-8 path"))
                .experimental_multiprocess_wal(true)
                .build(),
        )
        .expect_err("the default Windows backend cannot share a log");
        assert!(matches!(
            SharingRefusal::from_open_error(IoBackendName::Platform, &error),
            Some(SharingRefusal::SharedWalUnsupported {
                backend: IoBackendName::Platform
            })
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_backend_and_volume_wordings_are_told_apart() {
        let backing = turso::Error::Error(
            "experimental multiprocess WAL is not supported on the filesystem backing 'Z:\\x.db'"
                .to_owned(),
        );
        assert!(matches!(
            SharingRefusal::from_open_error(IoBackendName::Platform, &backing),
            Some(SharingRefusal::UnsupportedVolume { .. })
        ));
        let backend = turso::Error::Error(
            "experimental multiprocess WAL is not supported by the active IO backend for 'x.db'"
                .to_owned(),
        );
        assert!(matches!(
            SharingRefusal::from_open_error(IoBackendName::Platform, &backend),
            Some(SharingRefusal::SharedWalUnsupported {
                backend: IoBackendName::Platform
            })
        ));
    }
}
