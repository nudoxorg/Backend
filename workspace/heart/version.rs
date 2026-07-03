//! The generic versioned-object wrapper, shared across registry and runtime.
//!
//! Versions the payload with an ecosystem-appropriate [`PackageVersion`] rather
//! than a bare SemVer, so a Python object (PEP 440) is representable. Fields are
//! private with accessors so the invariant "the version describes this object"
//! stays intact.

use serde::{Deserialize, Serialize};

use crate::identity::PackageVersion;

/// A payload paired with the package version it corresponds to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioned<T> {
	version: PackageVersion,
	object: T,
}

impl<T> Versioned<T> {
	/// Pair an object with its version.
	pub const fn new(version: PackageVersion, object: T) -> Self { Self { version, object } }

	/// The version this object corresponds to.
	pub const fn version(&self) -> &PackageVersion { &self.version }

	/// A shared reference to the payload.
	pub const fn get(&self) -> &T { &self.object }

	/// Consume into the payload, dropping the version tag.
	pub fn into_inner(self) -> T { self.object }

	/// Map the payload while preserving the version.
	pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Versioned<U> {
		Versioned { version: self.version, object: f(self.object) }
	}
}
