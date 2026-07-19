//! Multi-window guard (09c §1.1): an advisory file lock on the **mutable**
//! project shard directory.
//!
//! Two GUI windows (or a window plus a CLI) opening the same project must
//! not both write one Edge shard. The first opener takes an exclusive
//! advisory lock on `<shard_dir>/.nudox.lock` and holds it for the life of
//! the store; a second opener gets [`StoreError::Io`] with
//! [`ErrorKind::WouldBlock`] and a clear message instead of a corrupted WAL.
//!
//! Baked dep shards are read-only after install (09-vector §20.3) and take
//! no lock.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use vector_core::StoreError;

use crate::{backend_error, locked_error};

/// Lock-file name inside the shard directory.
pub const LOCK_FILE: &str = ".nudox.lock";

/// An exclusive advisory lock over one mutable shard directory.
///
/// The OS releases advisory locks on process death, so a crashed writer
/// never wedges the project. Dropping the guard unlocks explicitly.
#[derive(Debug)]
pub struct ShardLock {
	file: File,
	path: PathBuf,
}

impl ShardLock {
	/// Take the exclusive lock, creating the shard directory and lock file
	/// as needed. A held lock elsewhere → [`StoreError::Io`] with
	/// [`ErrorKind::WouldBlock`] (via [`crate::locked_error`]).
	pub fn acquire(shard_dir: &Path) -> Result<Self, StoreError> {
		fs::create_dir_all(shard_dir).map_err(backend_error)?;
		let path = shard_dir.join(LOCK_FILE);
		let file = OpenOptions::new()
			.create(true)
			.truncate(false)
			.write(true)
			.open(&path)
			.map_err(backend_error)?;

		match file.try_lock_exclusive() {
			Ok(()) => Ok(Self { file, path }),
			Err(err) if err.kind() == ErrorKind::WouldBlock => Err(locked_error(format!(
				"project shard at {} is already open in another nudox window or process; \
				 close it (or wait for it to exit) before opening this project for writing",
				shard_dir.display(),
			))),
			Err(err) => Err(backend_error(format!(
				"failed to lock {}: {err}",
				path.display(),
			))),
		}
	}

	/// The lock-file path (diagnostics).
	pub fn path(&self) -> &Path {
		&self.path
	}
}

impl Drop for ShardLock {
	fn drop(&mut self) {
		// Best effort; the OS also releases the lock when the fd closes.
		let _ = fs2::FileExt::unlock(&self.file);
	}
}
