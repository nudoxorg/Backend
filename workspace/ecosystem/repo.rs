//! Repository URL normalization for cross-ecosystem entity resolution.
//!
//! A single canonical form — [`RepoSlug`] — lets the registry cluster packages
//! across ecosystems even when they declare the same repository with different
//! schemes, casing, `.git` suffixes, or npm shorthands.

use smol_str::SmolStr;

// ── Public types ─────────────────────────────────────────────────────────────

/// A normalized repository identity: lowercase `host/path` with scheme, `www.`,
/// credentials, `.git`, trailing slashes, query, and fragment stripped.
///
/// Two packages sharing the same [`RepoSlug`] are presumed to live in the same
/// source repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoSlug(SmolStr);

impl RepoSlug {
	/// The normalized `host/path` string.
	pub fn as_str(&self) -> &str { self.0.as_str() }
}

impl core::fmt::Display for RepoSlug {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.write_str(self.as_str())
	}
}

// ── Normalization ─────────────────────────────────────────────────────────────

/// Normalize a raw manifest repository URL into a [`RepoSlug`].
///
/// Handles these input shapes:
/// - Standard `https://`, `http://`, `ssh://`, `git://` URLs.
/// - `git+https://` / `git+ssh://` scheme prefixes.
/// - SCP-style SSH syntax: `git@github.com:user/repo.git`.
/// - npm shorthands: `github:user/repo`, `gitlab:user/repo`, `bitbucket:user/repo`.
/// - Bare `user/repo` two-segment paths (treated as `github.com/user/repo`).
///
/// Returns `None` for empty, unparseable, or host-less input.
pub fn normalize_repo_url(raw: &str) -> Option<RepoSlug> {
	let s = raw.trim();
	if s.is_empty() { return None; }

	// ── 1. Strip git+ scheme prefix ──────────────────────────────────────────
	let s = s.strip_prefix("git+").unwrap_or(s);

	// ── 2. npm ecosystem shorthands ──────────────────────────────────────────
	// `github:user/repo`, `gitlab:user/repo`, `bitbucket:user/repo`
	for (prefix, host) in &[
		("github:", "github.com"),
		("gitlab:", "gitlab.com"),
		("bitbucket:", "bitbucket.org"),
	] {
		if let Some(rest) = s.strip_prefix(prefix) {
			// `rest` is `user/repo[#ref]` — strip fragment, then normalize.
			let rest = strip_fragment(rest);
			let path = strip_git_suffix(rest).trim_matches('/');
			if path.is_empty() { return None; }
			return slug_from_host_path(host, path);
		}
	}

	// ── 3. SCP-like SSH: `git@github.com:user/repo.git` ─────────────────────
	if !s.contains("://") && s.contains('@') && s.contains(':') {
		return parse_scp(s);
	}

	// ── 4. Standard URL with scheme ──────────────────────────────────────────
	if let Some(rest) = s.strip_prefix("ssh://") {
		return parse_url_after_scheme(rest);
	}
	if let Some(rest) = s.strip_prefix("git://") {
		return parse_url_after_scheme(rest);
	}
	if let Some(rest) = s.strip_prefix("https://") {
		return parse_url_after_scheme(rest);
	}
	if let Some(rest) = s.strip_prefix("http://") {
		return parse_url_after_scheme(rest);
	}

	// ── 5. Bare `user/repo` npm shorthand (no scheme, no host dot) ───────────
	// Exactly two non-empty segments; first segment must contain no dot
	// (to avoid treating `example.com/foo` as a shorthand).
	parse_bare_shorthand(s)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Parse `user:pass@host/path?query#fragment` (after the scheme `://` has been
/// stripped). Strips credentials and `www.` prefix from the host.
fn parse_url_after_scheme(after_scheme: &str) -> Option<RepoSlug> {
	// Strip userinfo (`user:pass@` or `user@`).
	let after_auth = if let Some(at) = after_scheme.find('@') {
		&after_scheme[at + 1..]
	} else {
		after_scheme
	};

	// Split host from path at the first `/`.
	let (raw_host, path_and_rest) = match after_auth.split_once('/') {
		Some((h, p)) => (h, p),
		None => (after_auth, ""),
	};

	// Strip port from host; lowercase before the `www.` strip so any casing of
	// the prefix is caught.
	let host = raw_host.split(':').next().unwrap_or(raw_host).to_ascii_lowercase();
	let host = host.strip_prefix("www.").unwrap_or(&host);

	if host.is_empty() { return None; }

	// Strip query and fragment from path.
	let path = strip_query(strip_fragment(path_and_rest));
	// Strip `.git` suffix and trailing slashes.
	let path = strip_git_suffix(path).trim_matches('/');

	slug_from_host_path(host, path)
}

/// Parse SCP-style `git@host:user/repo.git`.
fn parse_scp(s: &str) -> Option<RepoSlug> {
	// Find `@` then `:`.
	let after_at = s.split_once('@').map(|(_, rest)| rest)?;
	let (host, path_raw) = after_at.split_once(':')?;
	let host = host.trim();
	if host.is_empty() { return None; }
	let path = strip_fragment(path_raw);
	let path = strip_query(path);
	let path = strip_git_suffix(path).trim_matches('/');
	slug_from_host_path(host, path)
}

/// Treat `user/repo` (no scheme, no dot in first segment) as a GitHub shorthand.
fn parse_bare_shorthand(s: &str) -> Option<RepoSlug> {
	let s = strip_fragment(s);
	let s = strip_query(s);
	let s = s.trim_matches('/');
	let parts: Vec<&str> = s.split('/').collect();
	if parts.len() != 2 { return None; }
	let (user, repo) = (parts[0], parts[1]);
	if user.is_empty() || repo.is_empty() { return None; }
	// Reject if first segment contains a dot — likely a hostname.
	if user.contains('.') { return None; }
	// Each segment must be valid slug chars.
	if !is_slug_chars(user) || !is_slug_chars(repo) { return None; }
	slug_from_host_path("github.com", &format!("{user}/{repo}"))
}

/// Validate that a string uses only alphanumeric / `-` / `_` / `.` characters
/// (the common set for user/repo slugs across all major forges).
fn is_slug_chars(s: &str) -> bool {
	!s.is_empty()
		&& s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Build a [`RepoSlug`] from a host and a path, both lowercased.
///
/// The path must carry at least two segments (`owner/repo`): a host-only or
/// owner-only URL (`https://github.com`, `https://github.com/user`) is not a
/// repository identity, and admitting it would cluster unrelated packages
/// under one slug.
fn slug_from_host_path(host: &str, path: &str) -> Option<RepoSlug> {
	if host.is_empty() || !path.contains('/') {
		return None;
	}
	let combined = format!("{}/{}", host.to_ascii_lowercase(), path.to_ascii_lowercase());
	Some(RepoSlug(SmolStr::from(combined)))
}

/// Strip a `#fragment` suffix.
fn strip_fragment(s: &str) -> &str {
	s.split('#').next().unwrap_or(s)
}

/// Strip a `?query` suffix.
fn strip_query(s: &str) -> &str {
	s.split('?').next().unwrap_or(s)
}

/// Strip a trailing `.git` suffix (case-sensitive; lowercase convention).
fn strip_git_suffix(s: &str) -> &str {
	s.strip_suffix(".git").unwrap_or(s)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	fn slug(raw: &str) -> String {
		normalize_repo_url(raw).expect(raw).0.to_string()
	}

	fn none(raw: &str) {
		assert!(normalize_repo_url(raw).is_none(), "expected None for {raw:?}");
	}

	// ── Standard HTTPS URLs ───────────────────────────────────────────────────

	#[test]
	fn https_plain() {
		assert_eq!(slug("https://github.com/User/Repo"), "github.com/user/repo");
	}

	#[test]
	fn https_www_stripped() {
		assert_eq!(slug("http://www.github.com/user/repo/"), "github.com/user/repo");
	}

	#[test]
	fn https_git_suffix_stripped() {
		assert_eq!(slug("git+https://github.com/user/repo.git"), "github.com/user/repo");
	}

	#[test]
	fn git_scheme_with_fragment() {
		assert_eq!(slug("git://github.com/user/repo.git#main"), "github.com/user/repo");
	}

	// ── SSH forms ─────────────────────────────────────────────────────────────

	#[test]
	fn scp_style_ssh() {
		assert_eq!(slug("git@github.com:User/Repo.git"), "github.com/user/repo");
	}

	#[test]
	fn ssh_scheme() {
		assert_eq!(slug("ssh://git@github.com/user/repo"), "github.com/user/repo");
	}

	// ── GitLab subgroup (all segments preserved) ──────────────────────────────

	#[test]
	fn gitlab_subgroup_path_preserved() {
		assert_eq!(
			slug("https://gitlab.com/group/subgroup/project"),
			"gitlab.com/group/subgroup/project"
		);
	}

	// ── npm shorthands ────────────────────────────────────────────────────────

	#[test]
	fn npm_github_shorthand() {
		assert_eq!(slug("github:user/repo"), "github.com/user/repo");
	}

	#[test]
	fn npm_gitlab_shorthand() {
		assert_eq!(slug("gitlab:user/repo"), "gitlab.com/user/repo");
	}

	#[test]
	fn npm_bitbucket_shorthand() {
		assert_eq!(slug("bitbucket:user/repo"), "bitbucket.org/user/repo");
	}

	// ── Bare `user/repo` treated as github shorthand ──────────────────────────

	#[test]
	fn bare_user_repo() {
		assert_eq!(slug("user/repo"), "github.com/user/repo");
	}

	#[test]
	fn bare_user_repo_with_dots_in_name() {
		// dot in repo name is fine; dot in user segment is the gating condition.
		assert_eq!(slug("my-user/my.repo"), "github.com/my-user/my.repo");
	}

	// ── Things that must return None ──────────────────────────────────────────

	#[test]
	fn empty_returns_none() {
		none("");
		none("   ");
	}

	#[test]
	fn domain_slash_path_not_github_shorthand() {
		// First segment contains a dot → NOT treated as bare shorthand.
		// It has a real host, but no scheme → None (host-only without scheme
		// is ambiguous; we don't guess).
		none("example.com/x");
	}

	#[test]
	fn bare_single_segment_returns_none() {
		// A single segment with no `/` can't be resolved.
		none("justarepo");
	}

	#[test]
	fn host_only_url_returns_none() {
		// A repository identity needs owner/repo; a bare host (or owner-only
		// path) would cluster unrelated packages under one slug.
		none("https://github.com");
		none("https://github.com/");
		none("https://github.com/just-a-user");
	}

	#[test]
	fn bare_three_segments_returns_none() {
		// Three segments without a scheme are rejected (host? org? repo? — ambiguous).
		none("a/b/c");
	}

	// ── Misc. edge cases ──────────────────────────────────────────────────────

	#[test]
	fn query_string_stripped() {
		assert_eq!(
			slug("https://github.com/user/repo?ref=main"),
			"github.com/user/repo"
		);
	}

	#[test]
	fn credentials_stripped() {
		assert_eq!(
			slug("https://user:pass@github.com/user/repo"),
			"github.com/user/repo"
		);
	}

	#[test]
	fn uppercase_host_and_path_lowercased() {
		assert_eq!(
			slug("https://GitHub.COM/MyOrg/MyRepo.git"),
			"github.com/myorg/myrepo"
		);
	}

	#[test]
	fn trailing_slash_stripped() {
		assert_eq!(slug("https://github.com/user/repo/"), "github.com/user/repo");
	}

	#[test]
	fn npm_shorthand_with_fragment() {
		assert_eq!(slug("github:user/repo#semver:^1.0.0"), "github.com/user/repo");
	}

	#[test]
	fn bitbucket_org_https() {
		assert_eq!(
			slug("https://bitbucket.org/atlassian/python-bitbucket"),
			"bitbucket.org/atlassian/python-bitbucket"
		);
	}

	#[test]
	fn codeberg_https() {
		assert_eq!(
			slug("https://codeberg.org/forgejo/forgejo"),
			"codeberg.org/forgejo/forgejo"
		);
	}

	#[test]
	fn repo_slug_as_str_roundtrip() {
		let slug = normalize_repo_url("https://github.com/rust-lang/rust").unwrap();
		assert_eq!(slug.as_str(), "github.com/rust-lang/rust");
	}

	#[test]
	fn repo_slug_display() {
		let slug = normalize_repo_url("https://github.com/rust-lang/rust").unwrap();
		assert_eq!(format!("{slug}"), "github.com/rust-lang/rust");
	}

	#[test]
	fn repo_slug_equality() {
		let a = normalize_repo_url("https://github.com/User/Repo.git").unwrap();
		let b = normalize_repo_url("git@github.com:user/repo.git").unwrap();
		assert_eq!(a, b, "same repo via different URL forms must be equal");
	}

	#[test]
	fn npm_shorthand_github_casing() {
		// npm shorthand: casing in user/repo should be lowercased.
		let a = normalize_repo_url("github:User/Repo").unwrap();
		let b = normalize_repo_url("https://github.com/user/repo").unwrap();
		assert_eq!(a, b);
	}
}
