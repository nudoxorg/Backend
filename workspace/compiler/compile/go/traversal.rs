//! Version resolution over git tags for Go modules.
//!
//! Go modules version through git tags with strict-semver names and a
//! module-path coupling that this module implements as *pure* functions
//! (the git walking itself reuses the compiler's shared VCS layer —
//! mirror of `python::traversal`, whose network paths live apart from
//! the pure selection logic):
//!
//! * a version tag is `vMAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]` — the
//!   leading `v` is mandatory in the Go ecosystem;
//! * major versions ≥ 2 must be declared in the module path as a `/vN`
//!   suffix (`github.com/user/repo/v2`), and only tags of that major
//!   satisfy the module;
//! * a module in a repository *subdirectory* is tagged with the relative
//!   directory as a tag prefix (`sub/dir/v1.2.3`);
//! * repositories that never adopted modules may carry `+incompatible`
//!   v2+ versions: for a module path *without* a `/vN` suffix, tags with
//!   major ≥ 2 are treated as incompatible-only candidates — selected
//!   only when explicitly requested (we approximate the real rule, which
//!   also checks for the absence of a go.mod at that tag).
//!
//! Selection follows the semver 2.0 precedence rules, preferring stable
//! releases over prereleases exactly like `python::traversal`'s
//! stable-before-prerelease policy.

use std::cmp::Ordering;

use version::{Constraint, TagContext, VersionGrammar, VersionRequest, resolve_from_tags};

use super::error::{GoError, Result};

/// A parsed strict-semver version (Go module flavor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoVersion {
	pub major: u64,
	pub minor: u64,
	pub patch: u64,

	/// Dot-separated prerelease identifiers (`rc.1` → `["rc", "1"]`).
	/// Empty for a stable release.
	pub prerelease: Vec<String>,

	/// Build metadata (ignored for precedence, kept for fidelity).
	pub build: Option<String>,
}

impl GoVersion {
	/// Whether this is a prerelease version.
	pub fn is_prerelease(&self) -> bool {
		!self.prerelease.is_empty()
	}

	/// Render back to the canonical `vX.Y.Z[-pre][+build]` form.
	pub fn to_tag(&self) -> String {
		let mut out = format!("v{}.{}.{}", self.major, self.minor, self.patch);
		if !self.prerelease.is_empty() {
			out.push('-');
			out.push_str(&self.prerelease.join("."));
		}
		if let Some(build) = &self.build {
			out.push('+');
			out.push_str(build);
		}
		out
	}
}

impl Ord for GoVersion {
	fn cmp(&self, other: &Self) -> Ordering {
		self.major
			.cmp(&other.major)
			.then(self.minor.cmp(&other.minor))
			.then(self.patch.cmp(&other.patch))
			.then_with(|| prerelease_cmp(&self.prerelease, &other.prerelease))
	}
}

impl PartialOrd for GoVersion {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

/// Semver 2.0 §11 prerelease precedence: a stable release outranks any
/// prerelease; identifiers compare numerically when both numeric,
/// numerics rank below alphanumerics, and a shorter identifier list
/// ranks below a longer one when all shared identifiers are equal.
fn prerelease_cmp(a: &[String], b: &[String]) -> Ordering {
	match (a.is_empty(), b.is_empty()) {
		(true, true) => return Ordering::Equal,
		(true, false) => return Ordering::Greater,
		(false, true) => return Ordering::Less,
		(false, false) => {}
	}
	for (ai, bi) in a.iter().zip(b.iter()) {
		let ord = match (ai.parse::<u64>(), bi.parse::<u64>()) {
			(Ok(an), Ok(bn)) => an.cmp(&bn),
			(Ok(_), Err(_)) => Ordering::Less,
			(Err(_), Ok(_)) => Ordering::Greater,
			(Err(_), Err(_)) => ai.cmp(bi),
		};
		if ord != Ordering::Equal {
			return ord;
		}
	}
	a.len().cmp(&b.len())
}

/// Parse a strict-semver version. The leading `v` is required by
/// default (`require_v`), matching Go's tag convention; requested
/// versions from user input may omit it (see [`parse_requested`]).
pub fn parse_semver(text: &str, require_v: bool) -> Option<GoVersion> {
	let rest = match text.strip_prefix('v') {
		Some(rest) => rest,
		None if require_v => return None,
		None => text,
	};

	// Split off build metadata, then prerelease.
	let (rest, build) = match rest.split_once('+') {
		Some((head, build)) if !build.is_empty() => (head, Some(build.to_string())),
		Some(_) => return None,
		None => (rest, None),
	};
	let (core, prerelease) = match rest.split_once('-') {
		Some((head, pre)) if !pre.is_empty() => {
			let parts: Vec<String> = pre.split('.').map(str::to_string).collect();
			if parts.iter().any(|p| p.is_empty() || !p.chars().all(is_ident_char)) {
				return None;
			}
			(head, parts)
		}
		Some(_) => return None,
		None => (rest, Vec::new()),
	};

	let mut nums = core.split('.');
	let major = parse_num(nums.next()?)?;
	let minor = parse_num(nums.next()?)?;
	let patch = parse_num(nums.next()?)?;
	if nums.next().is_some() {
		return None;
	}

	Some(GoVersion { major, minor, patch, prerelease, build })
}

fn is_ident_char(c: char) -> bool {
	c.is_ascii_alphanumeric() || c == '-'
}

/// Numeric component: digits only, no leading zeros (semver strictness).
fn parse_num(text: &str) -> Option<u64> {
	if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
		return None;
	}
	if text.len() > 1 && text.starts_with('0') {
		return None;
	}
	text.parse().ok()
}

/// The major version encoded in a module path's `/vN` suffix
/// (`github.com/user/repo/v2` → 2). Paths without a suffix are major
/// 0/1 modules → returns 1.
pub fn module_path_major(module_path: &str) -> u64 {
	let Some((_, last)) = module_path.rsplit_once('/') else {
		return 1;
	};
	let Some(digits) = last.strip_prefix('v') else {
		return 1;
	};
	// `/v1` and `/v0` are not legal major suffixes; `/v2`+ are.
	match parse_num(digits) {
		Some(n) if n >= 2 => n,
		_ => 1,
	}
}

/// The tag prefix for a module living in a repository subdirectory:
/// `""` at the root, `"sub/dir/"` for a nested module. (`/vN` major
/// subdirectories are conventionally NOT part of the tag prefix.)
pub fn tag_prefix(module_rel_dir: &str) -> String {
	let trimmed = module_rel_dir.trim_matches('/');
	if trimmed.is_empty() || trimmed == "." {
		String::new()
	} else {
		format!("{trimmed}/")
	}
}

/// A version tag matched against a module's tag prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMatch<'a> {
	/// The parsed version.
	pub version: GoVersion,
	/// The original tag string, ready to be checked out.
	pub tag: &'a str,
}

/// Parse every tag that belongs to the module (correct subdirectory
/// prefix, strict semver, `v` required).
pub fn candidate_tags<'a, S: AsRef<str>>(tags: &'a [S], module_rel_dir: &str) -> Vec<TagMatch<'a>> {
	let prefix = tag_prefix(module_rel_dir);
	tags.iter()
		.filter_map(|tag| {
			let raw = tag.as_ref();
			let rest = raw.strip_prefix(prefix.as_str())?;
			// A root-module tag must not accidentally match a nested
			// module's tag (`sub/v1.0.0` is not a root tag).
			if prefix.is_empty() && rest.contains('/') {
				return None;
			}
			let version = parse_semver(rest, true)?;
			Some(TagMatch { version, tag: raw })
		})
		.collect()
}

/// Go's numeric-prefix constraint: a major or major.minor prefix (`v1`, `1.4`).
///
/// Carried as the `Constraint` case of the shared [`version::VersionRequest`].
/// The actual major-discipline filtering (which majors a `/vN` module admits,
/// `+incompatible` unlocking, etc.) lives in [`GoGrammar::matches_request`],
/// which reads this constraint directly — so `Constraint::matches` below is a
/// simple structural test used only when a grammar evaluates it generically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoPrefix {
	pub major: u64,
	pub minor: Option<u64>,
}

impl Constraint<GoVersion> for GoPrefix {
	fn matches(&self, v: &GoVersion) -> bool {
		v.major == self.major && self.minor.map_or(true, |m| v.minor == m)
	}
}

/// What the caller is asking for.
///
/// The shared [`version::VersionRequest`] specialised to [`GoVersion`] with
/// Go's [`GoPrefix`] as its constraint case (`v1`, `1.4`). Go's major-version
/// discipline is applied on top in [`GoGrammar::matches_request`].
pub type VersionRequest = version::VersionRequest<GoVersion, GoPrefix>;

/// Parse a requested version string: `latest`/empty → newest; full
/// semver → exact; `v1` / `1.4` → prefix query.
pub fn parse_requested(requested: &str) -> Result<VersionRequest> {
	let trimmed = requested.trim();
	if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("latest") {
		return Ok(VersionRequest::Latest);
	}
	if let Some(version) = parse_semver(trimmed, false) {
		return Ok(VersionRequest::Exact(version));
	}

	let rest = trimmed.strip_prefix('v').unwrap_or(trimmed);
	let mut parts = rest.split('.');
	let major = parts
		.next()
		.and_then(parse_num)
		.ok_or_else(|| GoError::UnparseableVersionRequest { requested: requested.to_string() })?;
	let minor = match parts.next() {
		Some(m) => Some(
			parse_num(m)
				.ok_or_else(|| GoError::UnparseableVersionRequest { requested: requested.to_string() })?,
		),
		None => None,
	};
	if parts.next().is_some() {
		return Err(GoError::UnparseableVersionRequest { requested: requested.to_string() });
	}
	Ok(VersionRequest::Constraint(GoPrefix { major, minor }))
}

// ---------------------------------------------------------------------------
// VersionGrammar implementation — wires GoVersion into the shared loop
// ---------------------------------------------------------------------------

/// Grammar adapter that makes [`resolve_from_tags`] work for Go modules.
///
/// * `parse_tag` strips the subdirectory prefix and strict-semver-parses the
///   remainder (via [`candidate_tags`]'s logic inlined here).
/// * `matches_request` encodes Go's major-version discipline on top of the
///   default `Latest`/`Exact`/`Prefix` semantics.
struct GoGrammar<'a> {
	/// Module path, used to extract the expected major via [`module_path_major`].
	module_path: &'a str,
	/// Repository-relative subdirectory prefix for this module's tags.
	module_rel_dir: &'a str,
}

impl VersionGrammar for GoGrammar<'_> {
	type V = GoVersion;
	type C = GoPrefix;

	fn parse_tag<'t>(&self, raw_tag: &'t str, _ctx: &TagContext<'_>) -> Option<(GoVersion, &'t str)> {
		let prefix = tag_prefix(self.module_rel_dir);
		let rest = raw_tag.strip_prefix(prefix.as_str())?;
		// Root-module tags must not accidentally include a nested-module slash.
		if prefix.is_empty() && rest.contains('/') {
			return None;
		}
		let version = parse_semver(rest, true)?;
		Some((version, raw_tag))
	}

	fn is_prerelease(&self, v: &GoVersion) -> bool {
		v.is_prerelease()
	}

	/// Extend the shared request-matching with Go's major-version discipline.
	///
	/// For a `/vN` (N≥2) module only major-N tags are accepted.
	/// For an unversioned module major-0/1 are preferred; major-≥2
	/// (`+incompatible`) are only admitted when the caller explicitly named
	/// that major (an `Exact`/`Constraint` request, never a bare `Latest`).
	fn matches_request(
		&self,
		v: &GoVersion,
		request: &VersionRequest,
		_ctx: &TagContext<'_>,
	) -> bool {
		let module_major = module_path_major(self.module_path);

		let major_allowed = |major: u64, explicitly: bool| -> bool {
			if module_major >= 2 {
				major == module_major
			} else {
				major <= 1 || explicitly
			}
		};

		match request {
			VersionRequest::Latest => major_allowed(v.major, false),
			VersionRequest::Exact(want) => {
				major_allowed(v.major, true)
					&& v.major == want.major
					&& v.minor == want.minor
					&& v.patch == want.patch
					&& v.prerelease == want.prerelease
			}
			VersionRequest::Constraint(GoPrefix { major, minor }) => {
				major_allowed(v.major, true)
					&& v.major == *major
					&& minor.map_or(true, |m| v.minor == m)
			}
		}
	}

	fn numeric_prefix(&self, v: &GoVersion) -> Vec<u64> {
		vec![v.major, v.minor, v.patch]
	}
}

/// Resolve a version request against a repository's tags for the module
/// identified by `module_path` (for `/vN` major matching) and
/// `module_rel_dir` (for the subdirectory tag prefix). Returns the
/// original tag string of the winning version so the caller can check
/// that ref out — mirroring `python::traversal::resolve_version_from_tags`.
///
/// Major discipline: a `/vN` module only accepts major-N tags. A
/// suffix-less module prefers majors 0/1; majors ≥ 2 (the
/// `+incompatible` regime) are considered only when the request names
/// them explicitly.
///
/// The stable-before-prerelease selection loop is provided by
/// [`version::resolve_from_tags`]; this function supplies the Go grammar.
pub fn resolve_version_from_tags<S: AsRef<str>>(
	tags: &[S],
	requested: &VersionRequest,
	module_path: &str,
	module_rel_dir: &str,
) -> Option<String> {
	let grammar = GoGrammar { module_path, module_rel_dir };
	let ctx = TagContext { identifier: module_path, subdir: module_rel_dir };
	resolve_from_tags(tags, requested, &ctx, &grammar)
}

// ---------------------------------------------------------------------------
// Tests (pure selection logic — no git required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn v(text: &str) -> GoVersion {
		parse_semver(text, true).expect("test version parses")
	}

	#[test]
	fn parses_strict_semver() {
		let ver = v("v1.2.3-rc.1+meta");
		assert_eq!((ver.major, ver.minor, ver.patch), (1, 2, 3));
		assert_eq!(ver.prerelease, vec!["rc".to_string(), "1".to_string()]);
		assert_eq!(ver.build.as_deref(), Some("meta"));
		assert_eq!(ver.to_tag(), "v1.2.3-rc.1+meta");

		// The `v` is mandatory for tags; leading zeros are rejected.
		assert!(parse_semver("1.2.3", true).is_none());
		assert!(parse_semver("v1.02.3", true).is_none());
		assert!(parse_semver("v1.2", true).is_none());
	}

	#[test]
	fn precedence_follows_semver() {
		assert!(v("v1.2.3") > v("v1.2.3-rc.1"));
		assert!(v("v1.2.3-rc.2") > v("v1.2.3-rc.1"));
		assert!(v("v1.2.3-rc.1") > v("v1.2.3-alpha"));
		assert!(v("v1.2.3-alpha.1") > v("v1.2.3-alpha"));
		assert!(v("v2.0.0") > v("v1.99.99"));
	}

	#[test]
	fn module_path_major_suffix() {
		assert_eq!(module_path_major("github.com/user/repo"), 1);
		assert_eq!(module_path_major("github.com/user/repo/v2"), 2);
		assert_eq!(module_path_major("github.com/user/repo/v10"), 10);
		// `/v1` is not a legal major suffix.
		assert_eq!(module_path_major("github.com/user/repo/v1"), 1);
	}

	#[test]
	fn latest_prefers_stable_and_module_major() {
		let tags = ["v1.0.0", "v1.1.0", "v1.2.0-rc.1", "v2.0.0", "sub/v9.9.9"];
		// A suffix-less module ignores the v2 (+incompatible) tag.
		assert_eq!(
			resolve_version_from_tags(&tags, &VersionRequest::Latest, "example.com/m", ""),
			Some("v1.1.0".to_string())
		);
		// A /v2 module only sees major-2 tags.
		assert_eq!(
			resolve_version_from_tags(&tags, &VersionRequest::Latest, "example.com/m/v2", ""),
			Some("v2.0.0".to_string())
		);
	}

	#[test]
	fn exact_and_prefix_requests() {
		let tags = ["v1.0.0", "v1.4.2", "v1.4.9", "v2.1.0"];
		let exact = parse_requested("1.4.2").unwrap();
		assert_eq!(
			resolve_version_from_tags(&tags, &exact, "example.com/m", ""),
			Some("v1.4.2".to_string())
		);
		let prefix = parse_requested("v1.4").unwrap();
		assert_eq!(
			resolve_version_from_tags(&tags, &prefix, "example.com/m", ""),
			Some("v1.4.9".to_string())
		);
		// Explicitly requesting the incompatible major finds it.
		let incompatible = parse_requested("v2").unwrap();
		assert_eq!(
			resolve_version_from_tags(&tags, &incompatible, "example.com/m", ""),
			Some("v2.1.0".to_string())
		);
	}

	#[test]
	fn subdirectory_modules_use_tag_prefixes() {
		let tags = ["v1.0.0", "tools/v0.3.0", "tools/v0.4.0"];
		assert_eq!(
			resolve_version_from_tags(&tags, &VersionRequest::Latest, "example.com/m/tools", "tools"),
			Some("tools/v0.4.0".to_string())
		);
		// Nested tags never leak into the root module.
		assert_eq!(
			resolve_version_from_tags(&tags, &VersionRequest::Latest, "example.com/m", ""),
			Some("v1.0.0".to_string())
		);
	}

	#[test]
	fn prerelease_only_fallback() {
		let tags = ["v0.1.0-alpha", "v0.1.0-beta"];
		assert_eq!(
			resolve_version_from_tags(&tags, &VersionRequest::Latest, "example.com/m", ""),
			Some("v0.1.0-beta".to_string())
		);
	}
}
