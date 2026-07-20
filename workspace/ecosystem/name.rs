//! [`StructuredName`] — the common currency every ecosystem parses package
//! identifiers into and renders them from.
//!
//! Deliberately purl-shaped (authority / namespace segments / name /
//! qualifier) so it maps onto every ecosystem we have and every one we might
//! add. Downstream code (tantivy fields, `PackageSelector`, contains-bonus)
//! consumes this type, never raw strings.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{Language, LanguageExt};

/// A package identity decomposed into its ecosystem-meaningful parts.
///
/// Examples (authority / namespace / name):
/// - Rust   `serde_json`                      → (None, [], "serde-json")
/// - npm    `@types/node`                     → (None, ["types"], "node")
/// - PyPI   `typing-extensions`               → (None, [], "typing-extensions")
/// - Go     `github.com/gorilla/mux/v2`       → (Some("github.com"), ["gorilla"], "mux") + major=Some(2)
/// - Maven  `org.springframework:spring-core` → (None, ["org","springframework"], "spring-core")
/// - NuGet  `Newtonsoft.Json`                 → (None, ["newtonsoft"], "json")
/// - Nix    `NixOS/nixpkgs`                   → (None, ["nixos"], "nixpkgs")
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StructuredName {
	pub ecosystem: Language,

	/// Host/domain part, only where the grammar has one (Go module paths).
	pub authority: Option<SmolStr>,

	/// Ordered namespace segments (npm scope, Maven groupId parts, Go path
	/// dirs, NuGet dotted prefix). Already case-folded per the ecosystem's
	/// rules.
	pub namespace: Vec<SmolStr>,

	/// The final, most-specific segment. Case-folded per ecosystem rules.
	pub name: SmolStr,

	/// Go major-version suffix (`/v2`) and similar identity-relevant
	/// qualifiers.
	pub major: Option<u32>,

	/// The exact original input, preserved for display.
	pub original: SmolStr,
}

impl StructuredName {
	/// Identity string — feeds `PackageId` hashing. MUST equal what the legacy
	/// `canonicalize_*` functions produced so no stored `PackageId` changes.
	pub fn canonical(&self) -> String { self.ecosystem.spec().render_canonical(self) }

	/// The short, IDF-clean form for the search index: namespace + name,
	/// WITHOUT authority — `github`/`com` never become search tokens (R4).
	pub fn search_surface(&self) -> String {
		let mut surface = String::new();
		for segment in &self.namespace {
			surface.push_str(segment);
			surface.push(' ');
		}
		surface.push_str(&self.name);
		surface
	}

	/// The roots a fully-qualified *symbol* path may start with, for
	/// `PackageSelector` matching (Q2). Rust: `["serde_json"]`; Go: the full
	/// module path prefix; Java: the dotted groupId prefix; npm: both
	/// `@scope/name` and bare `name`.
	pub fn symbol_roots(&self) -> Vec<String> {
		match self.ecosystem {
			// Rust symbol paths use the underscore (crate-ident) form of a
			// hyphenated package name.
			Language::Rust => vec![self.name.replace('-', "_")],

			// npm symbols may be qualified by the scoped or the bare form.
			Language::Typescript => match self.namespace.first() {
				Some(scope) => {
					vec![format!("@{scope}/{}", self.name), self.name.to_string()]
				}
				None => vec![self.name.to_string()],
			},

			// Python modules use the underscore import form.
			Language::Python => vec![self.name.replace('-', "_")],

			// Go symbols are anchored at the full module path (major suffix
			// included when present).
			Language::Go => {
				let mut root = String::new();
				if let Some(authority) = &self.authority {
					root.push_str(authority);
				}
				for segment in &self.namespace {
					root.push('/');
					root.push_str(segment);
				}
				root.push('/');
				root.push_str(&self.name);
				if let Some(major) = self.major {
					root.push_str(&format!("/v{major}"));
				}
				vec![root]
			}

			// Java symbols are anchored at the dotted groupId (the artifactId
			// does not appear in a fully-qualified class name).
			Language::Java if !self.namespace.is_empty() => {
				vec![self.namespace.join(".")]
			}
			Language::Java => vec![self.name.to_string()],

			// C# namespaces follow the dotted package id.
			Language::CSharp => {
				let mut segments: Vec<&str> =
					self.namespace.iter().map(SmolStr::as_str).collect();
				segments.push(&self.name);
				vec![segments.join(".")]
			}

			// Nix flake outputs are addressed by the project slug.
			Language::Nix => vec![self.name.to_string()],

			// C/C++ has no language-level module path; the repo slug is the
			// identity, so the terminal name segment is the only sensible root.
			Language::Cpp => vec![self.name.to_string()],
		}
	}
}
