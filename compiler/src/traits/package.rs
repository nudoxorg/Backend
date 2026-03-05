use semver::Version;

use crate::{core::rust, error::NewDocsError};

pub trait Package: Send + Sync {
	// These are all async due to possible network requests/file IO

	// All of the published versions.
	fn get_available_versions(&self) -> Result<Vec<Version>, NewDocsError>;

	// All of the available flags.
	fn flags(&self) -> Result<Option<Vec<String>>, NewDocsError>;

	// A provided description as to the purpose of this package
	fn description(&self) -> Result<Option<String>, NewDocsError>;

	// Any other packages that this package relies on (none is an empty array)
	fn dependencies(&self) -> Result<Vec<AnyPackage>, NewDocsError>;

	// Any other packages that rely on this package (none is an empty array)
	fn dependents(&self) -> Result<Vec<AnyPackage>, NewDocsError>;

	fn retrieve(&self, version: Version, flags: Option<Vec<String>>) -> Result<String, NewDocsError>;
}

pub enum AnyPackage {
	Rust(rust::Package),
}
