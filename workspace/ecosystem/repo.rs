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
///
/// For known forges the path is first passed through [`reduce_forge_path`], so
/// a release/archive/blob download URL collapses to its `owner/repo` root.
fn slug_from_host_path(host: &str, path: &str) -> Option<RepoSlug> {
	if host.is_empty() || !path.contains('/') {
		return None;
	}
	let host = host.to_ascii_lowercase();
	let path = reduce_forge_path(&host, path);
	let combined = format!("{}/{}", host, path.to_ascii_lowercase());
	Some(RepoSlug(SmolStr::from(combined)))
}

/// Forges whose `owner/repo` root can be recovered from a deeper download or
/// browse URL by truncating at a well-known sub-path marker.
const KNOWN_FORGES: &[&str] = &["github.com", "gitlab.com", "codeberg.org", "bitbucket.org"];

/// Sub-path markers that separate a forge's `owner/repo` root from a
/// release/archive/browse tail. When any of these appears *past* the
/// `owner/repo` prefix the tail is noise (a release tarball path, a blob view,
/// an archive download) and the slug reduces to the first two segments.
///
/// GitLab subgroups (`gitlab.com/a/b/c` **project** URLs) carry NO marker, so
/// they are never truncated — only a marker-bearing path reduces.
const FORGE_SUBPATH_MARKERS: &[&str] = &[
	"releases", "archive", "-", "downloads", "get", "raw", "blob", "tree",
];

/// Reduce a known-forge path to its `owner/repo` root when it extends past the
/// root through a recognized sub-path marker (`/releases/`, `/archive/`,
/// `/-/archive/`, `/downloads/`, `/get/`, `/raw/`, `/blob/`, `/tree/`).
///
/// * Unknown hosts are returned untouched — on an arbitrary host the identity
///   **is** the whole URL and guessing an `owner/repo` split would mis-cluster.
/// * A known-forge path with no marker (a plain `owner/repo`, or a GitLab
///   subgroup `owner/group/project`) is returned untouched.
///
/// Reduction keeps the first two segments (`owner/repo`); the marker must sit at
/// segment index ≥ 2, i.e. strictly past the `owner/repo` prefix, so a repo
/// literally named `archive` or `raw` at the root is not itself a marker.
fn reduce_forge_path<'a>(host: &str, path: &'a str) -> &'a str {
	if !KNOWN_FORGES.contains(&host) {
		return path;
	}
	let segments: Vec<&str> = path.split('/').collect();
	if segments.len() <= 2 {
		return path;
	}
	// Scan segments strictly past `owner/repo` for the first marker.
	let has_marker = segments[2..]
		.iter()
		.any(|segment| FORGE_SUBPATH_MARKERS.contains(&segment.to_ascii_lowercase().as_str()));
	if !has_marker {
		return path;
	}
	// Reduce to `owner/repo` — the prefix up to (but excluding) the third `/`.
	match path.match_indices('/').nth(1) {
		Some((index, _)) => &path[..index],
		None => path,
	}
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

	// ── Known-forge sub-path reduction ────────────────────────────────────────

	#[test]
	fn github_release_download_reduces_to_owner_repo() {
		assert_eq!(
			slug("https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz"),
			"github.com/madler/zlib"
		);
	}

	#[test]
	fn github_archive_reduces() {
		assert_eq!(
			slug("https://github.com/owner/repo/archive/refs/tags/v2.0.0.tar.gz"),
			"github.com/owner/repo"
		);
	}

	#[test]
	fn github_blob_and_tree_reduce() {
		assert_eq!(slug("https://github.com/owner/repo/blob/main/src/lib.rs"), "github.com/owner/repo");
		assert_eq!(slug("https://github.com/owner/repo/tree/main/src"), "github.com/owner/repo");
	}

	#[test]
	fn github_raw_reduces() {
		assert_eq!(slug("https://github.com/owner/repo/raw/main/f"), "github.com/owner/repo");
	}

	#[test]
	fn gitlab_dash_archive_reduces() {
		assert_eq!(
			slug("https://gitlab.com/owner/repo/-/archive/v1.0/repo-v1.0.tar.gz"),
			"gitlab.com/owner/repo"
		);
	}

	#[test]
	fn codeberg_releases_reduces() {
		assert_eq!(
			slug("https://codeberg.org/forgejo/forgejo/releases/download/v1.0/forgejo.tar.gz"),
			"codeberg.org/forgejo/forgejo"
		);
	}

	#[test]
	fn bitbucket_get_and_downloads_reduce() {
		assert_eq!(slug("https://bitbucket.org/owner/repo/get/v1.0.tar.gz"), "bitbucket.org/owner/repo");
		assert_eq!(
			slug("https://bitbucket.org/owner/repo/downloads/repo-1.0.tar.gz"),
			"bitbucket.org/owner/repo"
		);
	}

	#[test]
	fn gitlab_subgroup_without_marker_not_reduced() {
		// `gitlab.com/a/b/c` project URLs (no sub-path marker) MUST stay whole —
		// truncating a subgroup project to `a/b` would mis-cluster it.
		assert_eq!(slug("https://gitlab.com/a/b/c"), "gitlab.com/a/b/c");
		assert_eq!(
			slug("https://gitlab.com/group/subgroup/project"),
			"gitlab.com/group/subgroup/project"
		);
	}

	#[test]
	fn unknown_host_path_not_reduced() {
		// An arbitrary host: identity IS the whole URL path; a `/releases/` tail
		// on an unknown forge is NOT reduced (we do not guess owner/repo).
		assert_eq!(
			slug("https://sourceware.org/git/glibc/releases/download/v1/glibc.tar.gz"),
			"sourceware.org/git/glibc/releases/download/v1/glibc.tar.gz"
		);
	}

	#[test]
	fn known_forge_plain_owner_repo_untouched() {
		// No marker → unchanged even on a known forge.
		assert_eq!(slug("https://github.com/madler/zlib"), "github.com/madler/zlib");
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
