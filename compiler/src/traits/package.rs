use ir::entry::Entry;
use semver::Version;

pub trait Package: Send + Sync + Sized {
	type Error: std::error::Error + Send + Sync;

	/// All of the published versions.
	fn get_available_versions(&self) -> Result<Vec<Version>, Self::Error>;

	/// All of the available flags.
	fn flags(&self) -> Result<Option<Vec<String>>, Self::Error>;

	/// A provided description as to the purpose of this package.
	fn description(&self) -> Result<Option<String>, Self::Error>;

	/// Any other packages that this package relies on (none is an empty vec).
	fn dependencies(&self) -> Result<Vec<Self>, Self::Error>;

	/// Any other packages that rely on this package (none is an empty vec).
	fn dependents(&self) -> Result<Vec<Self>, Self::Error>;

	/// Generate the IR entries for the given version and optional feature flags.
	fn retrieve(
		&self,
		version: Version,
		flags: Option<Vec<String>>,
	) -> Result<Vec<Entry>, Self::Error>;
}
