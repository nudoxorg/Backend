//! The ecosystem taxonomy, split cleanly into two concerns the old `Language`
//! enum conflated:
//!
//! - [`Ecosystem`] — *which* language/registry world a thing belongs to. It is
//!   fieldless and `ConstParamTy`, so it can be lifted into a const generic
//!   (see the registry `Catalog`) and used as a plain map/filter key.
//! - [`Toolchain`] — the *specific* build environment a package was produced
//!   against (compiler/interpreter version, edition). This is what a producer
//!   records; it is never used as an identity or filter key.
//!
//! Separating them fixes the bug where you couldn't express "search all of
//! Rust" without inventing a compiler version.

use std::marker::ConstParamTy;

use semver::Version;
use serde::{Deserialize, Serialize};

/// A language/registry ecosystem. Fieldless on purpose: this is the identity
/// and filter dimension, liftable into a const generic.
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
)]
#[strum(serialize_all = "lowercase")]
pub enum Ecosystem {
	/// <https://rust-lang.org/> — crates.io.
	Rust,
	/// <https://www.typescriptlang.org/> — npm.
	Typescript,
	/// <https://python.org/> — PyPI.
	Python,
}

impl Ecosystem {
	/// The stable lowercase wire/storage token (`"rust"`, `"typescript"`,
	/// `"python"`). Used in object-store paths and ids, so it must never drift.
	pub const fn as_token(self) -> &'static str {
		match self {
			Ecosystem::Rust => "rust",
			Ecosystem::Typescript => "typescript",
			Ecosystem::Python => "python",
		}
	}
}

/// The Rust edition a crate was produced under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum Edition {
	E2015,
	E2018,
	E2021,
	E2024,
}

/// The concrete toolchain a package was built/analyzed against. Recorded by
/// producers for provenance and reproducibility; never an identity key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Toolchain {
	/// `rustc` version + edition.
	Rust { compiler: Version, edition: Edition },

	/// The `tsc` compiler version.
	Typescript { compiler: Version },

	/// The CPython (or compatible) interpreter version the type oracle ran as.
	Python { interpreter: Version },
}

impl Toolchain {
	/// The ecosystem this toolchain belongs to.
	pub const fn ecosystem(&self) -> Ecosystem {
		match self {
			Toolchain::Rust { .. } => Ecosystem::Rust,
			Toolchain::Typescript { .. } => Ecosystem::Typescript,
			Toolchain::Python { .. } => Ecosystem::Python,
		}
	}
}
