use semver::Version;
use serde::{Deserialize, Serialize};

/// The world the code belongs to.
///
/// Owned by `heart` (the shared vocabulary) so it can sit *below* every plane —
/// `ir`, `ir-vcs`, `registry`, `index`, and the `ecosystem` spec crate all read
/// `heart::Language`. The per-ecosystem spec (`ecosystem::spec`) dispatches on
/// this enum and therefore depends on `heart`, not the other way around.
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

	/// The C/C++ toolchain (clang/libclang oracle) version the sources were
	/// analyzed against. IR production for `cpp` is governed by the IR plane
	/// (RL-15); this records the analyzer version for provenance.
	Cpp { compiler: Version },
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
			Toolchain::Cpp { .. } => Language::Cpp,
		}
	}
}
