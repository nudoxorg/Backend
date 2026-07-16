use std::{fmt, path::{Path, PathBuf}};

use triomphe::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageIdView<'a> {
	Path(&'a Path),
}

#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PackageId {
	repr: Repr,
}

impl PackageId {
	pub fn path(path: impl AsRef<Path>) -> Self { PackageId { repr: Repr::path(path.as_ref()) } }

	pub fn view(&self) -> PackageIdView<'_> { self.repr.view() }
}

impl fmt::Debug for PackageId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.view(), f) }
}

#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Repr {
	inner: Arc<ReprInner>,
}

#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
