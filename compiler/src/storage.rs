use std::{fs, io, path::{Path, PathBuf}};

use lang_types::Language;
use tempfile::{Builder, TempDir};
use url::Url;

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

	pub fn repository_dir(&self, language: Language, source: &Url) -> PathBuf {
		self
			.repositories_dir
			.join(language.to_string().to_ascii_lowercase())
			.join(sanitize_repository_source(source))
	}

	pub fn prepare_repository_dir(
		&self,
		language: Language,
		slug: &str,
		source: &Url,
	) -> io::Result<PathBuf> {
		let shared = self.repository_dir(language, source);
		if shared.exists() {
			return Ok(shared);
		}

		let legacy = self.legacy_repository_dir(language, slug, source);
		if legacy.exists() {
			if let Some(parent) = shared.parent() {
				fs::create_dir_all(parent)?;
			}
			fs::rename(&legacy, &shared)?;
			return Ok(shared);
		}

		Ok(shared)
	}

	pub fn create_workspace(&self) -> io::Result<TempDir> {
		Builder::new().prefix("nudox-workspace-").tempdir()
	}

	pub fn sessions_dir(&self) -> &Path { &self.sessions_dir }

	pub fn packages_file(&self) -> &Path { &self.packages_file }

	pub fn root(&self) -> &Path { &self.root }

	fn legacy_repository_dir(&self, language: Language, slug: &str, source: &Url) -> PathBuf {
		self.repositories_dir.join(language.to_string().to_ascii_lowercase()).join(format!(
			"{}-{}",
			sanitize_repository_slug(slug),
			sanitize_repository_source(source)
		))
	}
}

fn sanitize_repository_slug(slug: &str) -> String {
	let sanitized = crate::util::slug::ascii_segment(slug);
	if sanitized.is_empty() { "package".to_owned() } else { sanitized }
}

fn sanitize_repository_source(source: &Url) -> String {
	let raw = if source.scheme() == "file" {
		source.path().to_owned()
	} else {
		let mut value = source.host_str().unwrap_or("source").to_owned();
		value.push_str(source.path());
		value
	};

	let sanitized = raw
		.chars()
		.map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '_' })
		.collect::<String>()
		.trim_matches('_')
		.to_owned();

	if sanitized.is_empty() { "source".to_owned() } else { sanitized }
}

#[cfg(test)]
mod tests {
	use std::fs;

	use lang_types::Language;
	use tempfile::tempdir;
	use url::Url;

	use super::StorageLayout;

	#[test]
	fn repository_cache_path_is_shared_by_source() {
		let layout = StorageLayout::new("/tmp/nudox-test");
		let source = Url::parse("https://github.com/bytecodealliance/wasmtime").unwrap();

		let one = layout.repository_dir(Language::Rust, &source);
		let two = layout.repository_dir(Language::Rust, &source);

		assert_eq!(one, two);
		assert!(one.ends_with("github_com_bytecodealliance_wasmtime"));
	}

	#[test]
	fn prepare_repository_dir_migrates_legacy_slug_cache() {
		let root = tempdir().unwrap();
		let layout = StorageLayout::new(root.path());
		layout.ensure().unwrap();
		let source = Url::parse("https://github.com/bytecodealliance/wasmtime").unwrap();

		let legacy =
			root.path().join("repositories/rust/cranelift-github_com_bytecodealliance_wasmtime");
		fs::create_dir_all(&legacy).unwrap();
		fs::write(legacy.join("marker"), "ok").unwrap();

		let shared = layout.prepare_repository_dir(Language::Rust, "cranelift", &source).unwrap();

		assert_eq!(shared, root.path().join("repositories/rust/github_com_bytecodealliance_wasmtime"));
		assert!(shared.join("marker").is_file());
		assert!(!legacy.exists());
	}
}
