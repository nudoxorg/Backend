use std::marker::ConstParamTy;

use semver::Version;
use serde::{Deserialize, Serialize};

/// The world the code belongs to.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	PartialOrd,
	Ord,
	ConstParamTy,
	Serialize,
	Deserialize,
	strum::Display,
	strum::EnumString,
	strum::EnumIter,
	strum::AsRefStr,
	strum::IntoStaticStr,
)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Language {
	/// <https://rust-lang.org/>.io.
	Rust,

	/// <https://www.typescriptlang.org/>.
	Typescript,

	/// <https://python.org/>.
	Python,

	/// <https://go.dev/>.
	Go,

	/// <https://www.java.com/>.
	Java,

	/// <https://nix.dev/>. Flakes, packages, NixOS modules, and lib functions,
	/// acquired from FlakeHub and evaluated in-process.
	Nix,
}

impl Language {
	/// The stable lowercase wire token (`"rust"` / `"typescript"` / `"python"`) as
	/// a `&'static str` — the form persisted in postgres and used as a codec token.
	///
	/// Delegates to `strum::IntoStaticStr` so the token set can never drift from
	/// the variants. (`AsRefStr` yields the same strings but borrowed from `self`,
	/// which can't satisfy the `'static` return the storage/codec paths need.)
	pub fn as_token(&self) -> &'static str { self.into() }
}

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
		}
	}
}
