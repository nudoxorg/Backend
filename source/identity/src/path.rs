//! Canonical conversions from [`ir::entry::NudoxPath`] to the string forms used
//! in entry URIs and fully-qualified names.
//!
//! These are the **single source of truth** for how a `NudoxPath` is rendered.
//! The Terminus emitter, the parse-once projection, and any cross-store lookup
//! must all go through here so every store agrees on a symbol's identity.

use ir::entry::NudoxPath;

/// Render a path as the `{...}` tail of an `Entry/{lang}/{package}/{...}` URI.
///
/// Backslashes are normalised to forward slashes; external paths are prefixed
/// with their owning dependency.
pub fn nudox_path_to_str(path: &NudoxPath) -> String {
	match path {
		NudoxPath::Local(p) => p.to_string_lossy().replace('\\', "/"),
		NudoxPath::External { path, dependency } => {
			format!("{}/{}", dependency, path.to_string_lossy().replace('\\', "/"))
		}
	}
}

/// Split a path into its individual segments (dependency-prefixed for
/// externals).
pub fn path_segments(path: &NudoxPath) -> Vec<String> {
	match path {
		NudoxPath::Local(local_path) => {
			local_path.iter().map(|segment| segment.to_string_lossy().into_owned()).collect()
		}
		NudoxPath::External { path, dependency } => std::iter::once(dependency.clone())
			.chain(path.iter().map(|segment| segment.to_string_lossy().into_owned()))
			.collect(),
	}
}

/// The fully-qualified name (`a::b::c`) for a path.
pub fn fq_name(path: &NudoxPath) -> String { path_segments(path).join("::") }
