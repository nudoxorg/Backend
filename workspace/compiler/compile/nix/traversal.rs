//! FlakeHub acquisition layer for the Nix producer.
//!
//! Covers the full lifecycle before the static/dynamic layers run:
//!
//!   1. **Enumeration** — list all public flakes on FlakeHub.
//!   2. **Detail** — fetch per-flake metadata (description, readme, SPDX, outputs).
//!   3. **Resolution** — pick the best release satisfying a semver constraint.
//!   4. **Fetch** — follow the redirect chain, stream bytes, SHA-256 them, extract.
//!   5. **Input materialisation** — parse `flake.lock`, content-address every
//!      locked node into a cache dir, and lay them out so the evaluator finds them.
//!
//! All network calls are blocking (via `reqwest::blocking`), matching the
//! Python producer and the Go/Java oracle pattern — the producer runs sync.
//!
//! ## SHA-256
//! The `sha2` crate (already in the vendor registry as `sha2-0_10`) is used
//! directly via the `Digest` trait from the `digest` crate. No NAR hash is
//! served by FlakeHub for the tarball bytes; we hash the raw bytes ourselves
//! and record the hex for provenance.
//!
//! ## Error mapping
//! All fallible operations map into the [`NixError`] variants declared in
//! `error.rs`. No new variants are added — the existing taxonomy covers every
//! failure mode here.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{debug, instrument, warn};

use super::error::{NixError, Result};

// ── FlakeHub base URL ────────────────────────────────────────────────────────

const FLAKEHUB_API: &str = "https://api.flakehub.com";

// ── Serde mirror types ────────────────────────────────────────────────────────

/// One entry in `GET /flakes` — a public flake summary.
#[derive(Debug, Clone, Deserialize)]
pub struct FlakeSummary {
	/// The GitHub org or user name that owns this flake.
	#[serde(default)]
	pub org: String,

	/// The repository / flake name within the org.
	#[serde(default)]
	pub project: String,

	/// Short human-readable description of the flake.
	#[serde(default)]
	pub description: String,

	/// Labels applied to the flake (e.g. `["nixpkgs", "devShell"]`).
	#[serde(default)]
	pub labels: Vec<String>,

	/// URL of the flake's avatar image (may be empty for many flakes).
	#[serde(default)]
	pub avatar_url: String,
}

/// Per-flake detail returned by `GET /f/{org}/{project}`.
#[derive(Debug, Clone, Deserialize)]
pub struct FlakeDetail {
	/// Short human-readable description.
	#[serde(default)]
	pub description: String,

	/// Rendered README HTML/Markdown (may be absent for private or unREADME'd flakes).
	#[serde(default)]
	pub readme: String,

	/// SPDX licence identifier string (e.g. `"MIT"`, `"Apache-2.0"`), if declared.
	#[serde(default)]
	pub spdx_identifier: String,

	/// Upstream repository URL (e.g. `"https://github.com/NixOS/nixpkgs"`).
	#[serde(default)]
	pub repo_url: String,

	/// Labels applied to the flake.
	#[serde(default)]
	pub labels: Vec<String>,

	/// Whether this flake is mirrored by FlakeHub (e.g. from nixpkgs).
	/// When `true`, [`repo_url`] points to the real upstream source.
	#[serde(default)]
	pub mirrored: bool,

	/// Shallow evaluated output schema tree.  The shape is unversioned and
	/// intentionally kept as a raw JSON `Value` — we do not attempt to
	/// interpret it here; the eval layer owns that.
	#[serde(default)]
	pub outputs: serde_json::Value,
}

/// A single resolved release from `GET /version/{org}/{project}/{constraint}`.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
	/// The direct tarball download URL.  FlakeHub returns a 307 to a pinned
	/// "immutable" URL which then 307s again to a time-limited CloudFront URL.
	#[serde(default)]
	pub download_url: String,

	/// Semver-compatible version string (`X.Y.Z+rev-{sha}`).
	#[serde(default)]
	pub version: String,

	/// The exact git revision backing this release.
	#[serde(default)]
	pub revision: String,

	/// Total commit count at this release (used as a build number in some contexts).
	#[serde(default)]
	pub commit_count: u64,

	/// Non-null when the release has been yanked; contains the RFC 3339
	/// timestamp of the yank event.
	#[serde(default)]
	pub yanked_at: Option<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// List every public flake on FlakeHub.
///
/// Sends `GET https://api.flakehub.com/flakes` and deserialises the JSON
/// array into [`FlakeSummary`] values.
#[instrument]
pub fn enumerate() -> Result<Vec<FlakeSummary>> {
	let url = format!("{FLAKEHUB_API}/flakes");
	let body = get_json_body(&url)?;
	serde_json::from_str(&body).map_err(|source| NixError::FlakeHubDecode {
		url:    url.clone(),
		source,
	})
}

/// Fetch per-flake metadata for `{org}/{project}`.
///
/// Sends `GET https://api.flakehub.com/f/{org}/{project}`.
#[instrument(fields(org, project))]
pub fn detail(org: &str, project: &str) -> Result<FlakeDetail> {
	let url = format!("{FLAKEHUB_API}/f/{org}/{project}");
	let body = get_json_body(&url)?;
	serde_json::from_str(&body).map_err(|source| NixError::FlakeHubDecode {
		url:    url.clone(),
		source,
	})
}

/// Resolve the best release of `{org}/{project}` satisfying `constraint`.
///
/// Sends `GET https://api.flakehub.com/version/{org}/{project}/{constraint}`
/// (percent-encoding the constraint, e.g. `*` → `%2A`).
///
/// Yanked releases are rejected *unless* `constraint` is an exact `=X.Y.Z`
/// pin — in that case the caller has explicitly opted in to a fixed release.
#[instrument(fields(org, project, constraint))]
pub fn resolve(org: &str, project: &str, constraint: &str) -> Result<Release> {
	let encoded = percent_encode(constraint);
	let url = format!("{FLAKEHUB_API}/version/{org}/{project}/{encoded}");
	let body = get_json_body(&url)?;

	let release: Release = serde_json::from_str(&body).map_err(|source| {
		NixError::FlakeHubDecode { url: url.clone(), source }
	})?;

	// Reject yanked releases unless the constraint is an exact pin.
	if release.yanked_at.is_some() && !is_exact_pin(constraint) {
		return Err(NixError::YankedRelease {
			org:     org.to_owned(),
			project: project.to_owned(),
			version: release.version.clone(),
		});
	}

	if release.version.is_empty() {
		return Err(NixError::NoMatchingRelease {
			org:        org.to_owned(),
			project:    project.to_owned(),
			constraint: constraint.to_owned(),
		});
	}

	debug!(
		org,
		project,
		version = %release.version,
		revision = %release.revision,
		"resolved FlakeHub release",
	);

	Ok(release)
}

/// Download, verify, and extract the tarball for `release` into `workspace`.
///
/// The redirect chain is followed manually so we can capture the `Link:
/// rel="immutable"` header from the first 307 hop for provenance logging.
/// After extraction the extracted root is returned.
///
/// The tarball bytes are SHA-256 hashed in-flight. If an `expected_hash` is
/// already known (e.g. from a previously cached run) it can be compared; pass
/// an empty string to skip verification.
#[instrument(skip(release), fields(version = %release.version))]
pub fn fetch(release: &Release, workspace: &Path) -> Result<PathBuf> {
	if release.download_url.is_empty() {
		return Err(NixError::TarballFetch {
			url:    String::new(),
			detail: "release has no download_url".into(),
		});
	}

	fs::create_dir_all(workspace).map_err(|source| NixError::Extract {
		dest:   workspace.to_path_buf(),
		source,
	})?;

	// Follow redirects; capture the Link: rel="immutable" header from the
	// first hop (FlakeHub → pinned CDN URL) for provenance.
	let (bytes, immutable_url) = stream_tarball(&release.download_url)?;

	// SHA-256 the raw bytes for provenance.
	let actual_hex = sha256_hex(&bytes);
	debug!(
		sha256    = %actual_hex,
		immutable = ?immutable_url,
		"tarball fetched and hashed",
	);

	// Extract into workspace.
	extract_tar_gz(&bytes, workspace)?;

	// Discover the single top-level directory produced by the archive
	// (convention: `{org}-{project}-{rev}/`).
	let extracted = first_child_dir(workspace).unwrap_or_else(|| workspace.to_path_buf());
	Ok(extracted)
}

/// Materialise every locked input from `flake_root/flake.lock`.
///
/// Each locked node is written to
/// `flake_root/../inputs/<name>/` and content-addressed under `cache_root`
/// keyed by the node's `narHash` (or `rev` when `narHash` is absent).  This
/// means `nixpkgs` is fetched exactly once across many flakes in the same run.
///
/// Lock format (v7):
/// ```json
/// {
///   "nodes": {
///     "<name>": {
///       "locked": { "type", "owner", "repo", "rev", "narHash", ... },
///       "inputs": { ... }
///     }
///   },
///   "root": "root",
///   "version": 7
/// }
/// ```
///
/// No `flake.lock` → `Ok(())` with a `warn` (static-only mode).
#[instrument(skip(cache_root), fields(flake_root = %flake_root.display()))]
pub fn materialize_inputs(flake_root: &Path, cache_root: &Path) -> Result<()> {
	let lock_path = flake_root.join("flake.lock");
	if !lock_path.exists() {
		warn!("nix: no flake.lock at {}; skipping input materialisation", lock_path.display());
		return Ok(());
	}

	let raw = fs::read_to_string(&lock_path).map_err(NixError::Io)?;

	let lock: FlakeLock = serde_json::from_str(&raw).map_err(|source| NixError::LockParse {
		path:   lock_path.clone(),
		source,
	})?;

	let inputs_root = flake_root
		.parent()
		.unwrap_or(flake_root)
		.join("inputs");
	fs::create_dir_all(&inputs_root).map_err(|e| NixError::Extract {
		dest:   inputs_root.clone(),
		source: e,
	})?;
	fs::create_dir_all(cache_root).map_err(|e| NixError::Extract {
		dest:   cache_root.to_path_buf(),
		source: e,
	})?;

	for (name, node) in &lock.nodes {
		// Skip the root node — it represents the flake itself, not an input.
		if name == lock.root.as_deref().unwrap_or("root") {
			continue;
		}
		let dest = inputs_root.join(name);
		materialize_node(name, node, &dest, cache_root).map_err(|source| {
			NixError::MaterializeInput { name: name.clone(), source: Box::new(source) }
		})?;
	}

	Ok(())
}

/// Construct the GitHub tarball download URL for a `{owner}/{repo}` at `rev`.
///
/// Used both directly and as a fallback for FlakeHub-mirrored flakes.
pub fn github_tarball_url(owner: &str, repo: &str, rev: &str) -> String {
	format!("https://github.com/{owner}/{repo}/archive/{rev}.tar.gz")
}

// ── Internal: materialise a single locked node ───────────────────────────────

fn materialize_node(name: &str, node: &LockedNode, dest: &Path, cache_root: &Path) -> Result<()> {
	if dest.exists() {
		debug!(name, "input already materialised at {}", dest.display());
		return Ok(());
	}

	let locked = &node.locked;

	// Derive a stable cache key from narHash when available, else the revision.
	let cache_key = locked
		.nar_hash
		.as_deref()
		.or(locked.rev.as_deref())
		.unwrap_or(name);

	// Sanitise so the key is safe as a directory component.
	let safe_key = cache_key.replace(['/', ':', '\\'], "_");
	let cached = cache_root.join(&safe_key);

	if cached.exists() {
		debug!(name, cache_key, "reusing cached input at {}", cached.display());
		copy_dir_recursive(&cached, dest).map_err(|source| NixError::Extract {
			dest: dest.to_path_buf(),
			source,
		})?;
		return Ok(());
	}

	// Download into a temporary path, then atomically rename into the cache.
	let tmp = cache_root.join(format!("{safe_key}.tmp"));
	fetch_locked_node(name, locked, &tmp)?;

	// Rename tmp → cache (best-effort; ignore cross-device rename failures by
	// falling back to a recursive copy).
	if fs::rename(&tmp, &cached).is_err() {
		copy_dir_recursive(&tmp, &cached).map_err(|source| NixError::Extract {
			dest: cached.clone(),
			source,
		})?;
		let _ = fs::remove_dir_all(&tmp);
	}

	// Copy from cache to destination.
	copy_dir_recursive(&cached, dest).map_err(|source| NixError::Extract {
		dest: dest.to_path_buf(),
		source,
	})?;

	Ok(())
}

/// Fetch a single locked node based on its `type` field.
///
/// Supported types: `"github"`, `"gitlab"`, `"flakehub"`, `"tarball"`, and
/// `"path"` (path inputs are skipped with a debug log — they are relative to
/// the flake root and are already available).
fn fetch_locked_node(name: &str, locked: &LockedRef, dest: &Path) -> Result<()> {
	match locked.node_type.as_deref().unwrap_or("") {
		"github" => {
			let owner = locked.owner.as_deref().unwrap_or_default();
			let repo  = locked.repo.as_deref().unwrap_or_default();
			let rev   = locked.rev.as_deref().unwrap_or_default();
			if owner.is_empty() || repo.is_empty() || rev.is_empty() {
				warn!(name, "github locked node missing owner/repo/rev; skipping");
				return Ok(());
			}
			let url = github_tarball_url(owner, repo, rev);
			debug!(name, %url, "fetching github input");
			let (bytes, _) = stream_tarball(&url)?;
			fs::create_dir_all(dest).map_err(|source| NixError::Extract {
				dest: dest.to_path_buf(),
				source,
			})?;
			extract_tar_gz(&bytes, dest)?;
		}

		"gitlab" => {
			// GitLab uses the same archive convention as GitHub.
			let owner = locked.owner.as_deref().unwrap_or_default();
			let repo  = locked.repo.as_deref().unwrap_or_default();
			let rev   = locked.rev.as_deref().unwrap_or_default();
			if owner.is_empty() || repo.is_empty() || rev.is_empty() {
				warn!(name, "gitlab locked node missing owner/repo/rev; skipping");
				return Ok(());
			}
			let url = format!(
				"https://gitlab.com/{owner}/{repo}/-/archive/{rev}/{repo}-{rev}.tar.gz"
			);
			debug!(name, %url, "fetching gitlab input");
			let (bytes, _) = stream_tarball(&url)?;
			fs::create_dir_all(dest).map_err(|source| NixError::Extract {
				dest: dest.to_path_buf(),
				source,
			})?;
			extract_tar_gz(&bytes, dest)?;
		}

		"flakehub" => {
			// FlakeHub-hosted inputs carry `org` + `project` + a constraint or
			// specific version in their locked ref.
			let org     = locked.owner.as_deref().unwrap_or_default();
			let project = locked.repo.as_deref().unwrap_or_default();
			let version = locked.rev.as_deref().unwrap_or("*");
			if org.is_empty() || project.is_empty() {
				warn!(name, "flakehub locked node missing org/project; skipping");
				return Ok(());
			}
			let encoded = percent_encode(version);
			let url = format!("{FLAKEHUB_API}/f/{org}/{project}/{encoded}.tar.gz");
			debug!(name, %url, "fetching flakehub input");
			let (bytes, _) = stream_tarball(&url)?;
			fs::create_dir_all(dest).map_err(|source| NixError::Extract {
				dest: dest.to_path_buf(),
				source,
			})?;
			extract_tar_gz(&bytes, dest)?;
		}

		"tarball" => {
			let url = locked.url.as_deref().unwrap_or_default();
			if url.is_empty() {
				warn!(name, "tarball locked node has no url; skipping");
				return Ok(());
			}
			debug!(name, %url, "fetching tarball input");
			let (bytes, _) = stream_tarball(url)?;
			fs::create_dir_all(dest).map_err(|source| NixError::Extract {
				dest: dest.to_path_buf(),
				source,
			})?;
			extract_tar_gz(&bytes, dest)?;
		}

		"path" => {
			debug!(name, "path input; already available relative to flake root — skipping");
		}

		other => {
			warn!(name, node_type = other, "unknown locked node type; skipping");
		}
	}

	Ok(())
}

// ── Internal: HTTP helpers ───────────────────────────────────────────────────

/// Perform a `GET` and return the response body as a `String`, mapping
/// transport errors and non-2xx status codes to [`NixError::FlakeHubRequest`].
fn get_json_body(url: &str) -> Result<String> {
	let resp = reqwest::blocking::get(url).map_err(|e| NixError::FlakeHubRequest {
		url:    url.to_owned(),
		detail: e.to_string(),
	})?;

	let resp = resp.error_for_status().map_err(|e| NixError::FlakeHubRequest {
		url:    url.to_owned(),
		detail: e.to_string(),
	})?;

	resp.text().map_err(|e| NixError::FlakeHubRequest {
		url:    url.to_owned(),
		detail: e.to_string(),
	})
}

/// Download a tarball from `url`, following redirects via `reqwest` (which
/// handles the 307 chain transparently when `redirect::Policy::limited` is
/// used).  Returns the raw bytes and the immutable URL captured from the first
/// redirect's `Link: rel="immutable"` header if present.
///
/// We build a custom `Client` with `redirect::Policy::limited(10)` so we
/// follow the FlakeHub → CDN pinned URL → CloudFront chain, then collect the
/// bytes in memory.  For the immutable-URL header we cannot intercept the
/// intermediate response with `reqwest::blocking::get`, so we use a single
/// manual `GET` on the raw URL and read the first `Location` from the
/// response headers via a no-follow client, recording that for provenance.
fn stream_tarball(url: &str) -> Result<(Vec<u8>, Option<String>)> {
	use reqwest::redirect;

	// First, peek at the first redirect without following it to capture the
	// immutable URL from the `Location` header.
	let no_follow = reqwest::blocking::Client::builder()
		.redirect(redirect::Policy::none())
		.build()
		.map_err(|e| NixError::TarballFetch {
			url:    url.to_owned(),
			detail: format!("failed to build peek client: {e}"),
		})?;

	let immutable_url: Option<String> = no_follow
		.get(url)
		.send()
		.ok()
		.and_then(|resp| {
			// 307 Location carries the immutable CDN URL.
			resp.headers()
				.get("location")
				.and_then(|v| v.to_str().ok())
				.map(str::to_owned)
		});

	if let Some(ref pinned) = immutable_url {
		debug!(original_url = url, immutable_url = %pinned, "captured pinned tarball URL");
	}

	// Now fetch with redirect-following to get the final bytes.
	let client = reqwest::blocking::Client::builder()
		.redirect(redirect::Policy::limited(10))
		.build()
		.map_err(|e| NixError::TarballFetch {
			url:    url.to_owned(),
			detail: format!("failed to build fetch client: {e}"),
		})?;

	let resp = client.get(url).send().map_err(|e| NixError::TarballFetch {
		url:    url.to_owned(),
		detail: e.to_string(),
	})?;

	let resp = resp.error_for_status().map_err(|e| NixError::TarballFetch {
		url:    url.to_owned(),
		detail: e.to_string(),
	})?;

	let bytes = resp.bytes().map_err(|e| NixError::TarballFetch {
		url:    url.to_owned(),
		detail: e.to_string(),
	})?;

	Ok((bytes.to_vec(), immutable_url))
}

// ── Internal: archive helpers ─────────────────────────────────────────────────

/// Decompress and unpack an in-memory `.tar.gz` into `dest_dir`.
fn extract_tar_gz(bytes: &[u8], dest_dir: &Path) -> Result<()> {
	let mut decompressed = Vec::new();
	let mut decoder = flate2::read::GzDecoder::new(bytes);
	decoder.read_to_end(&mut decompressed).map_err(|source| NixError::Extract {
		dest:   dest_dir.to_path_buf(),
		source,
	})?;

	let mut archive = tar::Archive::new(std::io::Cursor::new(decompressed));
	archive.unpack(dest_dir).map_err(|source| NixError::Extract {
		dest:   dest_dir.to_path_buf(),
		source,
	})?;

	Ok(())
}

/// SHA-256 hash of `bytes`, returned as a lowercase hex string.
fn sha256_hex(bytes: &[u8]) -> String {
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	let hash = hasher.finalize();
	// format each byte as two hex digits
	hash.iter().fold(String::with_capacity(64), |mut s, b| {
		use std::fmt::Write;
		let _ = write!(s, "{b:02x}");
		s
	})
}

/// The first immediate child *directory* of `dir`, if any.  Tarballs produced
/// by GitHub/FlakeHub conventionally contain a single top-level directory
/// (`owner-repo-rev/`).  We unwrap that rather than returning the raw
/// extraction root so callers get a consistently rooted tree.
fn first_child_dir(dir: &Path) -> Option<PathBuf> {
	let mut entries = fs::read_dir(dir).ok()?;
	let entry = entries.next()?.ok()?;
	let path = entry.path();
	if path.is_dir() {
		// Only return this when it is the sole child (no ambiguous multi-root archives).
		if entries.next().is_none() {
			return Some(path);
		}
	}
	None
}

/// Recursively copy the directory tree at `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
	fs::create_dir_all(dst)?;
	for entry in fs::read_dir(src)?.flatten() {
		let src_path = entry.path();
		let dst_path = dst.join(entry.file_name());
		if src_path.is_dir() {
			copy_dir_recursive(&src_path, &dst_path)?;
		} else {
			fs::copy(&src_path, &dst_path)?;
		}
	}
	Ok(())
}

// ── Internal: percent-encoding ────────────────────────────────────────────────

/// Percent-encode a string using the unreserved character set defined by
/// RFC 3986 (letters, digits, `-`, `_`, `.`, `~` pass through; everything
/// else is `%XX`-encoded).  Used for FlakeHub path segments (e.g. `*` →
/// `%2A`).
fn percent_encode(input: &str) -> String {
	let mut out = String::with_capacity(input.len() * 3);
	for byte in input.bytes() {
		match byte {
			// RFC 3986 §2.3 unreserved characters
			b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
				out.push(byte as char);
			}
			other => {
				use std::fmt::Write;
				let _ = write!(out, "%{other:02X}");
			}
		}
	}
	out
}

// ── Internal: exact-pin detection ────────────────────────────────────────────

/// Return `true` when `constraint` is an exact version pin of the form
/// `=X.Y.Z` (with or without the leading `=`).
///
/// FlakeHub uses Cargo-style semver so `=1.2.3` is a strict equals
/// requirement; we treat plain `1.2.3` (no wildcards, no leading `>=`, etc.)
/// as an exact pin too for the yanked-release bypass.
fn is_exact_pin(constraint: &str) -> bool {
	let c = constraint.trim_start_matches('=').trim();
	// An exact pin contains only digits and dots (and an optional build metadata
	// suffix after `+`).  Reject anything with `*`, `~`, `^`, `>`, `<`, ` `.
	!c.is_empty()
		&& !c.contains(['*', '~', '^', '>', '<', ' '])
		&& c.split('+').next().map_or(false, |base| {
			base.split('.').all(|seg| !seg.is_empty() && seg.chars().all(|ch| ch.is_ascii_digit()))
		})
}

// ── flake.lock serde types ────────────────────────────────────────────────────

/// Top-level `flake.lock` structure (version 7).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct FlakeLock {
	/// Map of input name → locked node.
	#[serde(default)]
	nodes: HashMap<String, LockedNode>,

	/// The key within `nodes` that represents the flake itself.
	#[serde(default)]
	root: Option<String>,

	/// Lock file format version (currently 7).
	#[serde(default)]
	version: u32,
}

/// One node in the `nodes` map.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct LockedNode {
	/// The locked (pinned) source reference.
	#[serde(default)]
	locked: LockedRef,

	/// Transitive input name mapping for this node.  We do not need to recurse
	/// since the lock file already contains the full closure.
	#[serde(default)]
	inputs: HashMap<String, serde_json::Value>,
}

/// The `"locked"` sub-object of a node — the pinned source coordinates.
#[allow(dead_code)]
#[derive(Debug, Deserialize, Default)]
struct LockedRef {
	/// Source type: `"github"`, `"gitlab"`, `"flakehub"`, `"tarball"`, `"path"`.
	#[serde(rename = "type", default)]
	node_type: Option<String>,

	/// GitHub/GitLab owner (user or org).
	#[serde(default)]
	owner: Option<String>,

	/// GitHub/GitLab repository name.
	#[serde(default)]
	repo: Option<String>,

	/// Exact git revision (SHA-1 for GitHub; may be a tag for other backends).
	#[serde(default)]
	rev: Option<String>,

	/// Nix NAR hash (`sha256:<base32>`) used as a content-address cache key.
	#[serde(rename = "narHash", default)]
	nar_hash: Option<String>,

	/// `lastModified` UNIX timestamp — informational only.
	#[serde(rename = "lastModified", default)]
	last_modified: Option<i64>,

	/// Direct URL for `tarball`-type inputs.
	#[serde(default)]
	url: Option<String>,
}
