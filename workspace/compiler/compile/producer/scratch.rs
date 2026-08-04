//! Writable scratch carved from an [`FsGrant`] (or a fresh temp dir).

use std::fs;
use std::path::{Path, PathBuf};

use sandbox::FsGrant;

use super::ProducerError;

/// RAII scratch directory — the only preferred way to get a producer-private RW tree.
#[derive(Debug)]
pub struct Scratch {
	dir: PathBuf,
	/// When we created a private subdir, remove it on drop.
	owned: bool,
}

impl Scratch {
	/// Create a unique child under the grant's primary scratch.
	pub fn from_grant(fs: &FsGrant) -> Result<Self, ProducerError> {
		let dir = fs.scratch.join(format!(
			"run-{}-{:x}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.map(|d| d.as_nanos())
				.unwrap_or(0)
		));
		fs::create_dir_all(&dir)?;
		Ok(Self { dir, owned: true })
	}

	/// Fresh system-temp scratch (when no grant is available yet).
	pub fn temp(label: &str) -> Result<Self, ProducerError> {
		let dir = tempfile::Builder::new()
			.prefix(&format!("nudox-{label}-"))
			.tempdir()
			.map_err(|e| ProducerError::Io(e.into()))?
			.keep();
		Ok(Self { dir, owned: true })
	}

	/// Borrow the directory path.
	pub fn path(&self) -> &Path {
		&self.dir
	}

	/// Join a child path under this scratch.
	pub fn child(&self, name: impl AsRef<Path>) -> PathBuf {
		self.dir.join(name)
	}
}

impl Drop for Scratch {
	fn drop(&mut self) {
		if self.owned {
			let _ = fs::remove_dir_all(&self.dir);
		}
	}
}

impl AsRef<Path> for Scratch {
	fn as_ref(&self) -> &Path {
		&self.dir
	}
}
