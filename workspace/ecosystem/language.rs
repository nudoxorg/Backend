//! The [`Language`] enum — moved here from `heart::ecosystem` so the spec
//! crate can sit *below* `heart` (which re-exports it unchanged): `heart`'s
//! `PackageName` delegates its grammar to [`crate::spec`], so the spec crate
//! cannot also depend on `heart`.

use std::marker::ConstParamTy;

use serde::{Deserialize, Serialize};

/// The world the code belongs to.
///
/// The wire token for each variant is its lowercase name (`"rust"`,
/// `"typescript"`, etc.), matching the postgres `CHECK` domain.
/// `VariantNames::VARIANTS` is the single source the schema CHECK constraint is
/// derived from.
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
	strum::VariantNames,
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

	/// <https://dotnet.microsoft.com/>. .NET / NuGet packages, extracted from
	/// compiled assemblies (metadata) or source via a Roslyn oracle.
	CSharp,

	/// C and C++ (`"cpp"` covers both — RL-2). The registry-less, git-native
	/// ecosystem: identity is a normalized repository slug, versions come from
	/// tags with a Go-style pseudo-version fallback, and curated registries
	/// (vcpkg/Conan/Homebrew) are demoted to alias/version feeds.
	Cpp,
}

impl Language {
	/// The stable lowercase wire token (`"rust"` / `"typescript"` / `"python"`)
	/// as a `&'static str` — the form persisted in postgres and used as a codec
	/// token.
	///
	/// Delegates to `strum::IntoStaticStr` so the token set can never drift from
	/// the variants. (`AsRefStr` yields the same strings but borrowed from
	/// `self`, which can't satisfy the `'static` return the storage/codec paths
	/// need.)
	pub fn as_token(&self) -> &'static str { self.into() }

	/// Parse a user-facing language token, accepting the common aliases the
	/// search surface sees (`ts`/`js` → Typescript, `c#`/`cs`/`csharp` →
	/// CSharp, `golang` → Go). Case-insensitive. `None` for unknown tokens —
	/// callers treat those as free text, never as an error.
	pub fn from_token(raw: &str) -> Option<Self> {
		let folded = raw.to_ascii_lowercase();
		match folded.as_str() {
			"ts" | "js" | "javascript" => Some(Language::Typescript),
			"c#" | "cs" | "dotnet" => Some(Language::CSharp),
			"golang" => Some(Language::Go),
			"py" => Some(Language::Python),
			"c" | "c++" | "cxx" | "cc" => Some(Language::Cpp),
			token => token.parse().ok(),
		}
	}
}
