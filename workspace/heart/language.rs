//! The core language enumeration, shared across the compiler, registry,
//! runtime, and server.

use semver::Version;

/// A particular language variant, and its identifying information for the toolchain it was built on (so like rust 1.89 + edition 2024) Some languages will only need a version.
pub enum Language {
	/// https://rust-lang.org/
	Rust(Version),
	/// https://www.typescriptlang.org/
	Typescript(Version)
}
