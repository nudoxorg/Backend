//! Shared slug / segment sanitizers.
//!
//! One home for the string-normalization rules that were previously copy-pasted
//! across `storage`, `ingest`, `core::ts`, and `local_registry`.

/// Normalize a string into a lowercase ASCII identifier segment: ASCII
/// alphanumerics are kept (lowercased), every other character becomes `_`.
///
/// Used for on-disk repository slugs and Qdrant collection segments.
pub fn ascii_segment(value: &str) -> String {
	value
		.chars()
		.map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '_' })
		.collect()
}

/// npm-style package slug: lowercase, with scope separators (`/`) collapsed to
/// `__` (e.g. `@scope/pkg` → `@scope__pkg`).
pub fn npm(name: &str) -> String { name.to_ascii_lowercase().replace('/', "__") }

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ascii_segment_lowercases_and_replaces() {
		assert_eq!(ascii_segment("Tokio-Util 1.0"), "tokio_util_1_0");
		assert_eq!(ascii_segment(""), "");
	}

	#[test]
	fn npm_collapses_scope_separator() {
		assert_eq!(npm("@AWS-SDK/Client-S3"), "@aws-sdk__client-s3");
		assert_eq!(npm("zod"), "zod");
	}
}
