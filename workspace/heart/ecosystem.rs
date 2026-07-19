use semver::Version;
use serde::{Deserialize, Serialize};

/// [`Language`] moved to the `ecosystem` spec crate (ECOSYSTEM-PLAN P0) so the
/// per-ecosystem spec sits *below* heart — `PackageName` delegates its grammar
/// to `ecosystem::spec`, so the spec crate cannot also depend on heart.
/// Re-exported here so every `heart::Language` path compiles unchanged.
pub use ::ecosystem::Language;

/// The Rust edition a crate was produced under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum Edition {
	E2015,
	E2018,
	E2021,
	E2024,
}

/// The concrete toolchain a package was built/analyzed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Toolchain {
	/// `rustc` version + edition.
	Rust { compiler: Version, edition: Edition },

	/// The `tsc` compiler version.
	Typescript { compiler: Version },

	/// The CPython (or compatible) interpreter version the type oracle ran as.
	Python { interpreter: Version },

	/// The Go toolchain version (from `go version` or go.mod).
	Go { compiler: Version },

	/// The JDK/javac version used for extraction.
	Java { compiler: Version },

	/// The vendored Nix evaluator (snix) revision/version the flake was
	/// evaluated and statically analyzed against.
	Nix { evaluator: Version },

	/// The .NET SDK version whose Roslyn the oracle ran as.
	CSharp { sdk: Version },
}

impl From<&Toolchain> for Language {
	/// The ecosystem (language) this toolchain belongs to.
	fn from(toolchain: &Toolchain) -> Self {
		match toolchain {
			Toolchain::Rust { .. } => Language::Rust,
			Toolchain::Typescript { .. } => Language::Typescript,
			Toolchain::Python { .. } => Language::Python,
			Toolchain::Go { .. } => Language::Go,
			Toolchain::Java { .. } => Language::Java,
			Toolchain::Nix { .. } => Language::Nix,
			Toolchain::CSharp { .. } => Language::CSharp,
		}
	}
}
