//! `cpp` name grammar (REGISTRYLESS §3.1).
//!
//! Identity is a normalized repository slug. [`parse_name`] accepts, in order:
//! 1. anything [`crate::ecosystem::repo::normalize_repo_url`] accepts (URLs, SCP, host/path
//!    slugs, `owner/repo` shorthands);
//! 2. `system/<name>` toolchain-library stems (RL-11);
//! 3. `vcpkg/<port>` and `conan/<name>` feed-scoped fallback stems (RL-3).
//!
//! Bare single tokens (`zlib`) are deliberately **rejected** — they are aliases,
//! resolved to a slug before `parse_name` by the server query-normalization hook
//! (§9), never valid `cpp` names themselves.

use smol_str::SmolStr;

use crate::ecosystem::{Language, name::StructuredName, repo::normalize_repo_url};

/// The two feed-scoped authorities users normally never type; followers emit
/// them when an upstream repository is genuinely underivable (RL-3).
const FEED_SCOPED_AUTHORITIES: &[&str] = &["vcpkg", "conan"];

/// The toolchain/system-library authority (RL-11).
const SYSTEM_AUTHORITY: &str = "system";

/// Parse a raw `cpp` package identifier into a [`StructuredName`], or `None`
/// when the identifier is not a valid `cpp` name (REGISTRYLESS §3.1).
pub fn parse_name(raw: &str) -> Option<StructuredName> {
	let trimmed = raw.trim();
	if trimmed.is_empty() {
		return None;
	}

	// (2)/(3): the scoped-authority forms, checked before the URL normalizer so
	// `system/pthread` is not mistaken for a bare `owner/repo` github shorthand.
	if let Some((authority, rest)) = trimmed.split_once('/')
		&& (authority == SYSTEM_AUTHORITY || FEED_SCOPED_AUTHORITIES.contains(&authority))
		&& !rest.is_empty()
		&& !rest.contains('/')
	{
		return Some(scoped_name(authority, rest, trimmed));
	}

	// (1a): a bare, already-canonical `host.tld/owner/repo…` slug (no scheme, no
	// `@`). This is exactly the form `render_canonical` emits, so it MUST parse
	// for the round-trip law to hold. `normalize_repo_url` does not accept it —
	// it treats a dotted first segment as a host only when a scheme is present,
	// and rejects dotted-host bare paths as ambiguous — so handle it directly.
	if is_bare_host_slug(trimmed) {
		// Lowercase to match the `normalize_repo_url` slug convention (identity
		// is case-insensitive; `render_canonical` lowercases anyway).
		return structured_from_slug(&trimmed.to_ascii_lowercase(), trimmed);
	}

	// (1b): the primary plane — a normalized repository slug (URLs, SCP,
	// npm shorthands, bare `owner/repo`).
	let slug = normalize_repo_url(trimmed)?;
	structured_from_slug(slug.as_str(), trimmed)
}

/// Whether `raw` is already a bare canonical slug: no scheme, no userinfo, a
/// dotted host first segment, and at least an `owner/repo` tail. This is the
/// output shape of [`render_canonical`]; accepting it keeps the round-trip law
/// (`parse_name(render_canonical(n))` succeeds) intact.
fn is_bare_host_slug(raw: &str) -> bool {
	if raw.contains("://") || raw.contains('@') || raw.contains(' ') {
		return false;
	}
	let mut segments = raw.split('/');
	let Some(host) = segments.next() else { return false };
	// First segment must look like a host (contain a dot) and be non-empty.
	if !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
		return false;
	}
	// Need at least two more non-empty segments (owner + repo); every segment
	// must be a plausible slug segment (no empty middles, no `..`).
	let rest: Vec<&str> = segments.collect();
	rest.len() >= 2 && rest.iter().all(|s| !s.is_empty() && *s != "..")
}

/// Build a [`StructuredName`] for a scoped `authority/name` form
/// (`system/<name>`, `vcpkg/<port>`, `conan/<name>`).
fn scoped_name(authority: &str, name: &str, original: &str) -> StructuredName {
	StructuredName {
		ecosystem: Language::Cpp,
		authority: Some(SmolStr::from(authority)),
		namespace: Vec::new(),
		name: SmolStr::from(name.to_ascii_lowercase()),
		major: None,
		original: SmolStr::from(original),
	}
}

/// Decompose a normalized `host/path…/name` slug into a [`StructuredName`].
///
/// `authority` = host; `namespace` = the middle path segments; `name` = the
/// final segment. The slug is already lowercased by `normalize_repo_url`.
fn structured_from_slug(slug: &str, original: &str) -> Option<StructuredName> {
	let mut segments = slug.split('/');
	let host = segments.next()?;
	let rest: Vec<&str> = segments.collect();
	if rest.is_empty() {
		return None; // a bare host is not an identity (guarded upstream too)
	}
	let (namespace_segments, name) = rest.split_at(rest.len() - 1);
	Some(StructuredName {
		ecosystem: Language::Cpp,
		authority: Some(SmolStr::from(host)),
		namespace: namespace_segments.iter().map(|s| SmolStr::from(*s)).collect(),
		name: SmolStr::from(name[0]),
		major: None,
		original: SmolStr::from(original),
	})
}

/// Render the canonical identity string: the lowercase slug join
/// `authority "/" namespace… "/" name` (REGISTRYLESS §3.1).
pub fn render_canonical(name: &StructuredName) -> String {
	let mut string = String::new();
	if let Some(authority) = &name.authority {
		string.push_str(authority);
		string.push('/');
	}
	for segment in &name.namespace {
		string.push_str(segment);
		string.push('/');
	}
	string.push_str(&name.name);
	string.to_ascii_lowercase()
}
