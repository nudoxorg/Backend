use std::{fmt, hash, path::{Path, PathBuf}};

use triomphe::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageIdView<'a> {
	Path(&'a Path),
}

pub struct PackageId {
	repr: Repr,
}

impl PackageId {
	pub fn path(path: impl AsRef<Path>) -> Self { PackageId { repr: Repr::path(path.as_ref()) } }

	pub fn view(&self) -> PackageIdView<'_> { self.repr.view() }
}

impl Clone for PackageId {
	fn clone(&self) -> Self { PackageId { repr: Repr::clone(&self.repr) } }
}

impl PartialEq for PackageId {
	fn eq(&self, other: &Self) -> bool { self.view() == other.view() }
}

impl Eq for PackageId {}

impl fmt::Debug for PackageId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.view(), f) }
}

impl hash::Hash for PackageId {
	fn hash<H: hash::Hasher>(&self, state: &mut H) { hash::Hash::hash(&self.view(), state) }
}

impl serde::Serialize for PackageId {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		self.repr.inner.serialize(serializer)
	}
}

impl<'de> serde::Deserialize<'de> for PackageId {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		ReprInner::deserialize(deserializer)
			.map(|repr| PackageId { repr: Repr { inner: Arc::new(repr) } })
	}
}

#[derive(Clone)]
struct Repr {
	inner: Arc<ReprInner>,
}

#[derive(serde::Serialize, serde::Deserialize)]
enum ReprInner {
	Path(PathBuf),
}

impl Repr {
	fn path(path: &Path) -> Self { Repr { inner: Arc::new(ReprInner::Path(path.to_path_buf())) } }

	fn view(&self) -> PackageIdView<'_> {
		match self.inner.as_ref() {
			ReprInner::Path(path) => PackageIdView::Path(path),
		}
	}
}
