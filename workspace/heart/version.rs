//! The generic versioned-object wrapper, shared across registry and runtime.

use semver::Version;

/// A versioned representation of a particular object
pub struct Versioned<T> {
	// TODO: Support different kinds of versioning
	version: Version,
	object: T,
}
