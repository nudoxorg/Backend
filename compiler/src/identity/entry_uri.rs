use std::fmt;

/// Canonical `Entry/{lang}/{package}/{path}` URI used as the TerminusDB
/// document `@id` and cross-store primary key.
///
/// This is the **only** place entry URIs are constructed. Using this type
/// everywhere ensures all stores (Terminus, blobs, Tantivy, Qdrant, SQLite)
/// agree on the identity of a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryUri {
	lang:    String,
	package: String,
	path:    String,
}

impl EntryUri {
	/// Construct a canonical entry URI. `lang` is trimmed and lowercased;
	/// `package` and `path` are taken as-is.
	pub fn new(lang: &str, package: &str, path: &str) -> Self {
		Self {
			lang:    lang.trim().to_ascii_lowercase(),
			package: package.to_owned(),
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
