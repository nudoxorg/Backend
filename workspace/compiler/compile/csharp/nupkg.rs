//! NuGet acquisition helpers (host-side, NOT in the cage — CSHARP-PLAN §3.3).
//!
//! Plain HTTP against the NuGet v3 API, no NuGet client libraries:
//!
//! * flat container (`PackageBaseAddress/3.0.0`) for the version list and the
//!   `.nupkg` (a zip) itself;
//! * registration (`RegistrationsBaseUrl/3.6.0`) for `listed` flags and
//!   dependency groups (mode-M reference resolution).
//!
//! IDs are lowercased and versions NuGet-normalized (leading zeros stripped, a
//! trailing `.0` fourth part dropped, `+build` removed). This module owns the
//! *pure* URL/TFM/normalization logic (fully tested); wiring the download +
//! zip extraction into the CAS/acquisition pipeline is deferred (the `.nupkg`
//! is a zip, which needs a zip dependency the third-party feed does not yet
//! carry — tracked as a follow-up; the flat-container `.nupkg` URL is already
//! produced by the server acquisition path).

/// The public NuGet flat-container base.
pub const FLAT_CONTAINER: &str = "https://api.nuget.org/v3-flatcontainer";

/// The NuGet service index.
pub const SERVICE_INDEX: &str = "https://api.nuget.org/v3/index.json";

/// Normalize a package id to its lowercase flat-container form.
pub fn normalize_id(id: &str) -> String {
	id.trim().to_ascii_lowercase()
}

/// Normalize a NuGet version string: strip leading zeros from each numeric
/// part, drop a trailing `.0` fourth part, drop `+build` metadata, lowercase
/// the prerelease. (The flat container keys on this normalized form.)
pub fn normalize_version(version: &str) -> String {
	let version = version.trim();
	// Drop build metadata.
	let core = version.split('+').next().unwrap_or(version);
	let (numeric, pre) = match core.split_once('-') {
		Some((n, p)) => (n, Some(p.to_ascii_lowercase())),
		None => (core, None),
	};
	let mut parts: Vec<u64> =
		numeric.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect();
	// Drop a redundant trailing `.0` fourth part.
	if parts.len() == 4 && parts[3] == 0 {
		parts.pop();
	}
	// Pad to at least three parts (NuGet normalizes `1.0` → `1.0.0`).
	while parts.len() < 3 {
		parts.push(0);
	}
	let numeric = parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
	match pre {
		Some(p) => format!("{numeric}-{p}"),
		None => numeric,
	}
}

/// The flat-container version-list URL for a package.
pub fn version_index_url(id: &str) -> String {
	format!("{FLAT_CONTAINER}/{}/index.json", normalize_id(id))
}

/// The flat-container `.nupkg` URL for a package version.
pub fn nupkg_url(id: &str, version: &str) -> String {
	let id = normalize_id(id);
	let ver = normalize_version(version);
	format!("{FLAT_CONTAINER}/{id}/{ver}/{id}.{ver}.nupkg")
}

/// The registration index URL (for `listed` flags + dependency groups).
pub fn registration_url(id: &str) -> String {
	format!("https://api.nuget.org/v3/registration5-gz-semver2/{}/index.json", normalize_id(id))
}

/// Select the best target framework moniker from those available inside a
/// nupkg, following NuGet's compatibility precedence. Platform-suffixed TFMs
/// (`net8.0-windows`) are ignored unless they are the sole option.
pub fn select_tfm<'a>(available: &[&'a str]) -> Option<&'a str> {
	if available.is_empty() {
		return None;
	}
	// Prefer platform-neutral TFMs; fall back to platform-suffixed.
	let neutral: Vec<&str> = available.iter().copied().filter(|t| !t.contains('-')).collect();
	let pool: &[&str] = if neutral.is_empty() { available } else { &neutral };
	pool.iter().copied().max_by_key(|tfm| tfm_rank(tfm))
}

/// A comparable rank for a TFM — higher is preferred.
fn tfm_rank(tfm: &str) -> i64 {
	let base = tfm.split('-').next().unwrap_or(tfm);
	// `netX.Y` (modern .NET) ranks highest, ordered by version.
	if let Some(rest) = base.strip_prefix("net") {
		// Distinguish `net5.0`+ (has a dot) from legacy `net48`/`net45`.
		if let Some((major, minor)) = rest.split_once('.') {
			if let (Ok(maj), Ok(min)) = (major.parse::<i64>(), minor.parse::<i64>()) {
				return 100_000 + maj * 1000 + min;
			}
		}
		// Legacy `net48`, `net472`, `net45`, …
		if let Ok(n) = rest.parse::<i64>() {
			return 1_000 + n;
		}
	}
	if let Some(rest) = base.strip_prefix("netcoreapp") {
		if let Some((major, minor)) = rest.split_once('.') {
			if let (Ok(maj), Ok(min)) = (major.parse::<i64>(), minor.parse::<i64>()) {
				return 50_000 + maj * 1000 + min;
			}
		}
	}
	if let Some(rest) = base.strip_prefix("netstandard") {
		if let Some((major, minor)) = rest.split_once('.') {
			if let (Ok(maj), Ok(min)) = (major.parse::<i64>(), minor.parse::<i64>()) {
				return 40_000 + maj * 1000 + min;
			}
		}
	}
	0
}

/// Within an extracted nupkg, the preferred asset directory for a chosen TFM:
/// `ref/{tfm}/` (reference assemblies) beats `lib/{tfm}/`.
pub fn asset_dirs(tfm: &str) -> [String; 2] {
	[format!("ref/{tfm}"), format!("lib/{tfm}")]
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn version_normalization() {
		assert_eq!(normalize_version("1.0"), "1.0.0");
		assert_eq!(normalize_version("1.2.3.0"), "1.2.3");
		assert_eq!(normalize_version("1.02.3"), "1.2.3");
		assert_eq!(normalize_version("1.0.0-RC.1+abc"), "1.0.0-rc.1");
		assert_eq!(normalize_version("1.2.3.4"), "1.2.3.4");
	}

	#[test]
	fn urls() {
		assert_eq!(
			version_index_url("Newtonsoft.Json"),
			"https://api.nuget.org/v3-flatcontainer/newtonsoft.json/index.json"
		);
		assert_eq!(
			nupkg_url("Newtonsoft.Json", "13.0.1"),
			"https://api.nuget.org/v3-flatcontainer/newtonsoft.json/13.0.1/newtonsoft.json.13.0.1.nupkg"
		);
	}

	#[test]
	fn tfm_precedence() {
		let avail = ["netstandard2.0", "net8.0", "net10.0", "net6.0", "netstandard2.1"];
		assert_eq!(select_tfm(&avail), Some("net10.0"));

		let legacy = ["net45", "net48", "netstandard2.0"];
		assert_eq!(select_tfm(&legacy), Some("netstandard2.0"));

		// Platform-suffixed ignored unless sole option.
		let mixed = ["net8.0-windows", "net8.0"];
		assert_eq!(select_tfm(&mixed), Some("net8.0"));
		let only_platform = ["net8.0-windows"];
		assert_eq!(select_tfm(&only_platform), Some("net8.0-windows"));
	}

	#[test]
	fn netcore_below_modern_net_above_netstandard() {
		let avail = ["netstandard2.1", "netcoreapp3.1", "net5.0"];
		assert_eq!(select_tfm(&avail), Some("net5.0"));
		let avail2 = ["netstandard2.1", "netcoreapp3.1"];
		assert_eq!(select_tfm(&avail2), Some("netcoreapp3.1"));
	}
}
