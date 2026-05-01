use std::{fs, io, path::{Path, PathBuf}};

use tempfile::{TempDir, tempdir_in};

#[derive(Clone, Debug)]
pub struct StorageLayout {
	root:             PathBuf,
	repositories_dir: PathBuf,
	workspaces_dir:   PathBuf,
}

impl StorageLayout {
	pub fn new(root: impl Into<PathBuf>) -> Self {
		let root = root.into();
		let repositories_dir = root.join("repositories");
		let workspaces_dir = root.join("workspaces");
		Self { root, repositories_dir, workspaces_dir }
	}

	pub fn ensure(&self) -> io::Result<()> {
		fs::create_dir_all(&self.repositories_dir)?;
		fs::create_dir_all(&self.workspaces_dir)?;
		Ok(())
	}

	pub fn repository_dir(&self, package_id: u64) -> PathBuf {
		self.repositories_dir.join(package_id.to_string())
	}

	pub fn create_workspace(&self) -> io::Result<TempDir> { tempdir_in(&self.workspaces_dir) }

	pub fn root(&self) -> &Path { &self.root }
}
