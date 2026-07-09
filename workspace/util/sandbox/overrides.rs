//! Process-wide per-package / per-profile limit overlays (design §13 / P5).
//!
//! The server installs a map from config (`limits.sandbox_overrides`);
//! producers resolve via [`resolve`].

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::limits::{LimitOverride, Limits};
use crate::profiles::ProducerProfile;

static OVERRIDES: std::sync::OnceLock<RwLock<HashMap<String, LimitOverride>>> =
	std::sync::OnceLock::new();

fn map() -> &'static RwLock<HashMap<String, LimitOverride>> {
	OVERRIDES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Replace the process-wide override table (idempotent install from config).
pub fn install(overrides: HashMap<String, LimitOverride>) {
	if let Ok(mut guard) = map().write() {
		*guard = overrides;
	}
}

/// Merge `profile` base with optional keys:
/// 1. exact `package_key` (e.g. `crates.io/serde@1.0.0`)
/// 2. profile wire name (`rust`, `java`, `go`, `nix`, `static_parser`)
pub fn resolve(profile: ProducerProfile, package_key: Option<&str>) -> Limits {
	let base = profile.base_limits();
	let guard = map().read().unwrap_or_else(|e| e.into_inner());
	let mut overlay = LimitOverride::none();
	if let Some(key) = package_key {
		if let Some(o) = guard.get(key) {
			overlay = merge(overlay, *o);
		}
	}
	if let Some(o) = guard.get(profile.wire_name()) {
		overlay = merge(overlay, *o);
	}
	overlay.apply(base)
}

fn merge(mut a: LimitOverride, b: LimitOverride) -> LimitOverride {
	// Later arg wins on set fields.
	if b.mem_bytes.is_some() {
		a.mem_bytes = b.mem_bytes;
	}
	if b.cpu_secs.is_some() {
		a.cpu_secs = b.cpu_secs;
	}
	if b.wall.is_some() {
		a.wall = b.wall;
	}
	if b.pids.is_some() {
		a.pids = b.pids;
	}
	if b.max_stdout.is_some() {
		a.max_stdout = b.max_stdout;
	}
	if b.max_stderr.is_some() {
		a.max_stderr = b.max_stderr;
	}
	if b.fsize_bytes.is_some() {
		a.fsize_bytes = b.fsize_bytes;
	}
	if b.nofile.is_some() {
		a.nofile = b.nofile;
	}
	a
}

/// Shared reference type for observers that need the table.
pub type OverrideTable = Arc<HashMap<String, LimitOverride>>;
