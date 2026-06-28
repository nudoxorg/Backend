use std::{fmt, sync::Arc};

/// Canonical `Entry/{lang}/{package}/{path}` URI used as the TerminusDB
/// document `@id` and cross-store primary key.
///
/// This is the **only** place entry URIs are constructed. Using this type
/// everywhere ensures all stores (Terminus, blobs, Tantivy, Qdrant, SQLite)
/// agree on the identity of a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryUri {
	lang:    Arc<str>,
	package: Arc<str>,
	path:    String,
}

impl EntryUri {
	/// Construct a canonical entry URI. `lang` is trimmed and lowercased; for
	/// Rust the `package` segment is normalized once here (see
	/// [`canonical_rust_package`]) so every caller — ingestion and lookup alike —
	/// produces the same spelling and no downstream guessing is needed. For other
	/// languages `package` is taken as-is.
	pub fn new(lang: &str, package: &str, path: &str) -> Self {
		let lang = lang.trim().to_ascii_lowercase();
		let package =
			if lang == "rust" { canonical_rust_package(package, path) } else { package.to_owned() };
		Self {
			lang:    Arc::from(lang.as_str()),
			package: Arc::from(package.as_str()),
			path:    path.to_owned(),
		}
	}

	/// Parse `"Entry/{lang}/{package}/{path}"`. Returns `None` if the string
	/// does not start with `"Entry/"` or contains fewer than three segments.
	///
	/// Invariant: `EntryUri::parse(uri.to_string()) == Some(uri)` for any
	/// `uri` produced by `EntryUri::new`.
	pub fn parse(s: &str) -> Option<Self> {
		let rest = s.strip_prefix("Entry/")?;
		let (lang, rest) = rest.split_once('/')?;
		let (package, path) = rest.split_once('/')?;
		Some(Self::new(lang, package, path))
	}

	pub fn lang(&self) -> &str { &self.lang }

	pub fn package(&self) -> &str { &self.package }

	pub fn path(&self) -> &str { &self.path }
}

/// Reconcile a crates.io package name (hyphenated, e.g. `cranelift-object`)
/// with the underscored Rust crate identifier that appears as the path's crate
/// root (e.g. `cranelift_object`), collapsing to the single hyphenated crate
/// slug. Only applied when the package and the crate root agree modulo `-`/`_`;
/// otherwise the package (a re-exported or differently-named crate) is left
/// untouched.
pub(crate) fn canonical_rust_package(package: &str, path: &str) -> String {
	let crate_root = path.split("::").next().filter(|segment| !segment.is_empty()).unwrap_or(package);
	let crate_slug = crate_root.replace('_', "-");
	if package.replace('_', "-").eq_ignore_ascii_case(&crate_slug) {
		crate_slug
	} else {
		package.to_owned()
	}
}

impl fmt::Display for EntryUri {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Entry/{}/{}/{}", self.lang, self.package, self.path)
	}
}

impl From<EntryUri> for String {
	fn from(u: EntryUri) -> String { u.to_string() }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let cases = [
			("rust", "serde", "serde::Serialize"),
			("typescript", "zod", "ZodString"),
			("rust", "cranelift-object", "cranelift_object::ObjectModule"),
			("rust", "tokio", "tokio::sync/broadcast"),
		];
		for (lang, pkg, path) in cases {
			let uri = EntryUri::new(lang, pkg, path);
			let parsed = EntryUri::parse(&uri.to_string())
				.unwrap_or_else(|| panic!("parse failed for {lang}/{pkg}/{path}"));
			assert_eq!(uri, parsed);
		}
	}

	#[test]
	fn rust_package_segment_is_canonicalized_to_hyphenated_crate_slug() {
		// Underscored crates.io spelling collapses to the hyphenated crate slug…
		let a = EntryUri::new("rust", "cranelift_object", "cranelift_object::ObjectModule");
		assert_eq!(a.package(), "cranelift-object");
		assert_eq!(a.to_string(), "Entry/rust/cranelift-object/cranelift_object::ObjectModule");
		// …and the already-hyphenated spelling maps to the same URI (the fixed point
		// that makes lookup-time guessing unnecessary).
		let b = EntryUri::new("rust", "cranelift-object", "cranelift_object::ObjectModule");
		assert_eq!(a, b);
	}

	#[test]
	fn rust_package_left_untouched_when_it_differs_from_crate_root() {
		// A re-export whose package name doesn't match the path's crate root is
		// preserved rather than mangled.
		let uri = EntryUri::new("rust", "tokio-util", "futures_core::Stream");
		assert_eq!(uri.package(), "tokio-util");
	}

	#[test]
	fn non_rust_package_is_not_canonicalized() {
		let uri = EntryUri::new("typescript", "aws_sdk", "client::S3");
		assert_eq!(uri.package(), "aws_sdk");
	}

	#[test]
	fn lang_is_normalized() {
		let uri = EntryUri::new("Rust", "serde", "Serialize");
		assert_eq!(uri.lang(), "rust");
		assert_eq!(uri.to_string(), "Entry/rust/serde/Serialize");
	}

	#[test]
	fn lang_whitespace_is_stripped() {
		let uri = EntryUri::new("  rust  ", "serde", "Serialize");
		assert_eq!(uri.lang(), "rust");
	}

	#[test]
	fn parse_rejects_wrong_prefix() {
		assert!(EntryUri::parse("Kind/rust/serde/Serialize").is_none());
		assert!(EntryUri::parse("entry/rust/serde/Serialize").is_none());
	}

	#[test]
	fn parse_rejects_missing_path_segment() {
		assert!(EntryUri::parse("Entry/rust/serde").is_none());
	}
}
