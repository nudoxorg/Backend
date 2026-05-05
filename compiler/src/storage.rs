use std::{fs, io, path::{Path, PathBuf}};

use tempfile::{Builder, TempDir};

#[derive(Clone, Debug)]
pub struct StorageLayout {
	root:             PathBuf,
	packages_file:    PathBuf,
	repositories_dir: PathBuf,
	sessions_dir:     PathBuf,
}

impl StorageLayout {
	pub fn new(root: impl Into<PathBuf>) -> Self {
		let root = root.into();
		let packages_file = root.join("packages.json");
		let repositories_dir = root.join("repositories");
		let sessions_dir = root.join("sessions");
		Self { root, packages_file, repositories_dir, sessions_dir }
	}

	pub fn ensure(&self) -> io::Result<()> {
		fs::create_dir_all(&self.repositories_dir)?;
		fs::create_dir_all(&self.sessions_dir)?;
		Ok(())
	}

	pub fn repository_dir(&self, package_id: u64) -> PathBuf {
		self.repositories_dir.join(package_id.to_string())
	}

	pub fn create_workspace(&self) -> io::Result<TempDir> {
		Builder::new().prefix("nudox-workspace-").tempdir()
	}

	pub fn sessions_dir(&self) -> &Path { &self.sessions_dir }

	pub fn packages_file(&self) -> &Path { &self.packages_file }

	pub fn root(&self) -> &Path { &self.root }
}
