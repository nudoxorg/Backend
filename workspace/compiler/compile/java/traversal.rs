//! Version resolution over git tags using Maven version conventions.
//!
//! Java projects version through Maven coordinates; releases land in git
//! under a handful of tag shapes. As in `go::traversal` / `python::
//! traversal`, everything here is *pure* selection logic — the git walking
//! itself lives in the compiler's shared VCS layer:
//!
//! * candidate tags are matched under the ecosystem's common prefixes, in
//!   order: `<artifactId>-1.2.3` (the maven-release-plugin default),
//!   `v1.2.3` / `V1.2.3`, `release-1.2.3`, and the bare `1.2.3`;
//! * versions order by a faithful subset of Maven's `ComparableVersion`:
//!   tokens split on `.`, `-`, and digit↔letter transitions; numeric
//!   tokens compare numerically; qualifiers rank
//!   `alpha < beta < milestone < rc < snapshot < ⟨release⟩ < sp`
//!   (with the `ga`/`final`/`release` ≡ ⟨release⟩ and `cr` ≡ `rc`
//!   aliases, and `a`/`b`/`m` short forms), unknown qualifiers ranking
//!   above `sp` lexically; missing trailing tokens compare as the release
//!   pad (`1.0` == `1.0.0`, `1.0-alpha` < `1.0` < `1.0-sp`);
//! * `-SNAPSHOT` and the alpha/beta/milestone/rc family count as
//!   *prereleases*: selection prefers the newest stable, falling back to
//!   the newest prerelease — the same policy as the Python and Go
//!   producers.
//!
//! Deliberately NOT implemented (documented subset): Maven *range* syntax
//! (`[1.0,2.0)`) and property-interpolated versions; requests are `latest`,
//! an exact version, or a numeric prefix (`1` / `1.4`).

use std::cmp::Ordering;

use super::error::MavenVersionError;

/// One token of a parsed Maven version.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
	Num(u64),
	/// Normalized (lowercased, alias-resolved) qualifier.
	Qual(String),
}

/// A parsed Maven version.
#[derive(Debug, Clone, Eq)]
pub struct MavenVersion {
	items: Vec<Item>,
	/// The original text, for display and exact-tag recovery.
	pub raw: String,
}

impl PartialEq for MavenVersion {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Ord for MavenVersion {
	fn cmp(&self, other: &Self) -> Ordering {
		let len = self.items.len().max(other.items.len());
		for i in 0..len {
			let ord = match (self.items.get(i), other.items.get(i)) {
				(Some(a), Some(b)) => item_cmp(a, b),
				(Some(a), None) => pad_cmp(a),
				(None, Some(b)) => pad_cmp(b).reverse(),
				(None, None) => Ordering::Equal,
			};
			if ord != Ordering::Equal {
				return ord;
			}
		}
		Ordering::Equal
	}
}

impl PartialOrd for MavenVersion {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl MavenVersion {
	/// Whether any token marks a prerelease
	/// (`alpha`/`beta`/`milestone`/`rc`/`snapshot` family).
	pub fn is_prerelease(&self) -> bool {
		self.items.iter().any(|item| match item {
			Item::Qual(q) => qualifier_rank(q).0 < RELEASE_RANK,
			Item::Num(_) => false,
		})
	}

	/// The leading numeric segments (`1.4.2-rc.1` → `[1, 4, 2]`).
	fn numeric_prefix(&self) -> Vec<u64> {
		self.items
			.iter()
			.take_while(|item| matches!(item, Item::Num(_)))
			.map(|item| match item {
				Item::Num(n) => *n,
				Item::Qual(_) => unreachable!("take_while guards"),
			})
			.collect()
	}
}

/// The rank of the empty (release) qualifier in [`qualifier_rank`].
const RELEASE_RANK: u8 = 5;

/// Compare a leftover item against Maven's "null" pad: numbers pad against
/// `0` (`1.0.0` ≡ `1.0`), qualifiers against the release marker
/// (`1.0-snapshot` < `1.0` < `1.0-sp`).
fn pad_cmp(item: &Item) -> Ordering {
	match item {
		Item::Num(n) => n.cmp(&0),
		Item::Qual(q) => qualifier_rank(q).cmp(&qualifier_rank("")),
	}
}

fn item_cmp(a: &Item, b: &Item) -> Ordering {
	match (a, b) {
		(Item::Num(x), Item::Num(y)) => x.cmp(y),
		// Numbers always outrank qualifiers (Maven rule: `1.0.1 > 1.0-sp`).
		(Item::Num(_), Item::Qual(_)) => Ordering::Greater,
		(Item::Qual(_), Item::Num(_)) => Ordering::Less,
		(Item::Qual(x), Item::Qual(y)) => qualifier_rank(x).cmp(&qualifier_rank(y)),
	}
}

/// Well-known qualifier ordering; unknown qualifiers rank last, lexically.
fn qualifier_rank(q: &str) -> (u8, &str) {
	match q {
		"alpha" => (0, ""),
		"beta" => (1, ""),
		"milestone" => (2, ""),
		"rc" => (3, ""),
		"snapshot" => (4, ""),
		"" => (RELEASE_RANK, ""),
		"sp" => (6, ""),
		other => (7, other),
	}
}

/// Resolve Maven's qualifier aliases after tokenization.
fn normalize_qualifier(q: &str) -> String {
	match q {
		"ga" | "final" | "release" => String::new(),
		"cr" => "rc".to_string(),
		"a" => "alpha".to_string(),
		"b" => "beta".to_string(),
		"m" => "milestone".to_string(),
		other => other.to_string(),
	}
}

/// Parse a Maven version string. Returns `None` for text that doesn't
/// start with a digit (versions lead numerically in this ecosystem; that
/// requirement is also what keeps tag-prefix stripping unambiguous).
pub fn parse_version(text: &str) -> Option<MavenVersion> {
	let trimmed = text.trim();
	if !trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
		return None;
	}

	let lower = trimmed.to_ascii_lowercase();
	let mut items = Vec::new();
	let mut current = String::new();
	let mut current_is_digit = true;

	let flush = |token: &str, items: &mut Vec<Item>| {
		if token.is_empty() {
			return;
		}
		if token.bytes().all(|b| b.is_ascii_digit()) {
			// Oversized numerics degrade to qualifiers rather than panicking.
			match token.parse::<u64>() {
				Ok(n) => items.push(Item::Num(n)),
				Err(_) => items.push(Item::Qual(token.to_string())),
			}
		} else {
			items.push(Item::Qual(normalize_qualifier(token)));
		}
	};

	for c in lower.chars() {
		match c {
			'.' | '-' | '_' | '+' => {
				flush(&current, &mut items);
				current.clear();
			}
			_ => {
				let is_digit = c.is_ascii_digit();
				if !current.is_empty() && is_digit != current_is_digit {
					// digit↔letter transition splits tokens (`1a2` → 1, a, 2).
					flush(&current, &mut items);
					current.clear();
				}
				current_is_digit = is_digit;
				current.push(c);
			}
		}
	}
	flush(&current, &mut items);

	if items.is_empty() {
		return None;
	}
	Some(MavenVersion { items, raw: trimmed.to_string() })
}

// ---------------------------------------------------------------------------
// Tag matching
// ---------------------------------------------------------------------------

/// A version tag matched against the ecosystem's tag conventions.
#[derive(Debug, Clone)]
pub struct TagMatch<'a> {
	pub version: MavenVersion,
	/// The original tag string, ready to be checked out.
	pub tag: &'a str,
}

/// Parse every tag that looks like a release of this artifact.
/// `artifact_id` (from the Maven coordinates, when known) unlocks the
/// maven-release-plugin's `artifactId-version` tag shape.
pub fn candidate_tags<'a, S: AsRef<str>>(
	tags: &'a [S],
	artifact_id: Option<&str>,
) -> Vec<TagMatch<'a>> {
	let artifact_prefix = artifact_id.map(|a| format!("{a}-"));

	tags.iter()
		.filter_map(|tag| {
			let raw = tag.as_ref();
			let rest = strip_tag_prefix(raw, artifact_prefix.as_deref())?;
			let version = parse_version(rest)?;
			Some(TagMatch { version, tag: raw })
		})
		.collect()
}

/// Strip the first matching tag prefix; bare numeric tags pass through.
fn strip_tag_prefix<'a>(tag: &'a str, artifact_prefix: Option<&str>) -> Option<&'a str> {
	if let Some(prefix) = artifact_prefix {
		if let Some(rest) = tag.strip_prefix(prefix) {
			return Some(rest);
		}
	}
	for prefix in ["v", "V", "release-", "releases/"] {
		if let Some(rest) = tag.strip_prefix(prefix) {
			if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
				return Some(rest);
			}
		}
	}
	if tag.chars().next().is_some_and(|c| c.is_ascii_digit()) {
		return Some(tag);
	}
	None
}

// ---------------------------------------------------------------------------
// Requests and selection
// ---------------------------------------------------------------------------

/// What the caller is asking for.
#[derive(Debug, Clone, PartialEq)]
pub enum VersionRequest {
	/// The newest release (stable preferred).
	Latest,
	/// An exact version (`1.4.2`, `2.0.0-rc.1`).
	Exact(MavenVersion),
	/// A numeric prefix (`1`, `1.4`): the newest match wins.
	Prefix(Vec<u64>),
}

/// Parse a requested version: empty / `latest` → newest; one or two bare
/// numeric segments (`1`, `1.4`) → prefix query; anything else that parses
/// as a Maven version → exact.
pub fn parse_requested(requested: &str) -> Result<VersionRequest, MavenVersionError> {
	let trimmed = requested.trim();
	if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("latest") {
		return Ok(VersionRequest::Latest);
	}

	let segments: Vec<&str> = trimmed.split('.').collect();
	let all_numeric = segments.iter().all(|s| {
		!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
	});
	if all_numeric && segments.len() <= 2 {
		let nums = segments
			.iter()
			.map(|s| {
				s.parse::<u64>().map_err(|source| {
					MavenVersionError::NumericSegmentOverflow {
						requested: requested.to_owned(),
						source,
					}
				})
			})
			.collect::<Result<Vec<u64>>>()?;
		return Ok(VersionRequest::Prefix(nums));
	}

	match parse_version(trimmed) {
		Some(version) => Ok(VersionRequest::Exact(version)),
		None => Err(MavenVersionError::UnparseableVersion {
			requested: requested.to_owned(),
		}),
	}
}

/// Resolve a version request against a repository's tags. Returns the
/// original tag string of the winning version so the caller can check that
/// ref out — mirroring the Go/Python producers.
pub fn resolve_version_from_tags<S: AsRef<str>>(
	tags: &[S],
	requested: &VersionRequest,
	artifact_id: Option<&str>,
) -> Option<String> {
	let candidates = candidate_tags(tags, artifact_id);

	let matching: Vec<&TagMatch> = candidates
		.iter()
		.filter(|c| match requested {
			VersionRequest::Latest => true,
			VersionRequest::Exact(want) => c.version == *want,
			VersionRequest::Prefix(prefix) => {
				// Zero-padded so `1.0` matches a bare `1` tag.
				let nums = c.version.numeric_prefix();
				prefix
					.iter()
					.enumerate()
					.all(|(i, p)| nums.get(i).copied().unwrap_or(0) == *p)
			}
		})
		.collect();

	// Stable-before-prerelease preference, then newest.
	let pick = matching
		.iter()
		.filter(|c| !c.version.is_prerelease())
		.max_by(|a, b| a.version.cmp(&b.version))
		.or_else(|| matching.iter().max_by(|a, b| a.version.cmp(&b.version)));

	pick.map(|c| c.tag.to_string())
}

// ---------------------------------------------------------------------------
// Tests (pure selection logic — no git required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn v(text: &str) -> MavenVersion {
		parse_version(text).expect("test version parses")
	}

	#[test]
	fn ordering_follows_maven_rules() {
		assert!(v("1.0.0") == v("1.0"));
		assert!(v("1.0-alpha") < v("1.0-beta"));
		assert!(v("1.0-beta") < v("1.0-rc1"));
		assert!(v("1.0-rc1") < v("1.0-SNAPSHOT"));
		assert!(v("1.0-SNAPSHOT") < v("1.0"));
		assert!(v("1.0") < v("1.0-sp1"));
		assert!(v("1.0-sp1") < v("1.0.1"));
		assert!(v("2.0.0") > v("1.99.99"));
		// Aliases.
		assert!(v("1.0-ga") == v("1.0"));
		assert!(v("1.0-cr2") == v("1.0-rc2"));
		assert!(v("1.0a1") == v("1.0-alpha-1"));
		// Unknown qualifiers rank above sp, lexically.
		assert!(v("1.0-zeta") > v("1.0-sp"));
	}

	#[test]
	fn prerelease_detection() {
		assert!(v("1.0-SNAPSHOT").is_prerelease());
		assert!(v("2.0.0-rc.1").is_prerelease());
		assert!(v("1.0-alpha-2").is_prerelease());
		assert!(!v("1.0").is_prerelease());
		assert!(!v("1.0-sp1").is_prerelease());
		assert!(!v("1.0-custom").is_prerelease());
	}

	#[test]
	fn tag_prefixes() {
		let tags = ["widget-1.2.0", "v1.3.0", "release-1.1.0", "1.0.0", "unrelated"];
		let matched = candidate_tags(&tags, Some("widget"));
		let raws: Vec<&str> = matched.iter().map(|m| m.tag).collect();
		assert_eq!(raws, vec!["widget-1.2.0", "v1.3.0", "release-1.1.0", "1.0.0"]);
		// Without the artifact id, the plugin-shaped tag is skipped.
		assert_eq!(candidate_tags(&tags, None).len(), 3);
	}

	#[test]
	fn latest_prefers_stable() {
		let tags = ["v1.0.0", "v1.1.0", "v1.2.0-rc.1", "v1.2.0-SNAPSHOT"];
		assert_eq!(
			resolve_version_from_tags(&tags, &VersionRequest::Latest, None),
			Some("v1.1.0".to_string())
		);
		// Prerelease-only repos fall back to the newest prerelease.
		let pre = ["v0.1.0-alpha", "v0.1.0-beta"];
		assert_eq!(
			resolve_version_from_tags(&pre, &VersionRequest::Latest, None),
			Some("v0.1.0-beta".to_string())
		);
	}

	#[test]
	fn exact_and_prefix_requests() {
		let tags = ["widget-1.4.2", "widget-1.4.9", "widget-2.1.0", "widget-1.5.0-rc1"];
		let exact = parse_requested("1.4.2").unwrap();
		assert_eq!(
			resolve_version_from_tags(&tags, &exact, Some("widget")),
			Some("widget-1.4.2".to_string())
		);
		let prefix = parse_requested("1.4").unwrap();
		assert_eq!(
			resolve_version_from_tags(&tags, &prefix, Some("widget")),
			Some("widget-1.4.9".to_string())
		);
		let major = parse_requested("2").unwrap();
		assert_eq!(
			resolve_version_from_tags(&tags, &major, Some("widget")),
			Some("widget-2.1.0".to_string())
		);
	}

	#[test]
	fn requested_forms() {
		assert_eq!(parse_requested("latest").unwrap(), VersionRequest::Latest);
		assert_eq!(parse_requested("").unwrap(), VersionRequest::Latest);
		assert_eq!(parse_requested("1.4").unwrap(), VersionRequest::Prefix(vec![1, 4]));
		assert!(matches!(parse_requested("1.4.2").unwrap(), VersionRequest::Exact(_)));
		assert!(matches!(
			parse_requested("2.0.0-SNAPSHOT").unwrap(),
			VersionRequest::Exact(_)
		));
		assert!(parse_requested("not-a-version").is_err());
	}
}
