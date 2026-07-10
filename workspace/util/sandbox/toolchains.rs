//! Resolved toolchain store paths (DAEMON-PLAN §2.5).
//!
//! Absorbs the compiler's former `ToolchainPaths::from_env`. This is the one
//! sanctioned place that may read `NUDOX_TOOLCHAIN_*` / `RUSTUP_HOME` etc. — the
//! seal boundary. The result is hashed into every `JobKey` via [`Self::digest`]
//! so a toolchain change invalidates the content-addressed cache.
//!
//! Owned by the `ForgeRuntime` and injected downstream; the `compiler` crate
//! never reads toolchain env itself.

use std::path::PathBuf;

use heart::ContentHash;

use crate::spec::Env;

/// Pinned toolchain paths for hermetic producer runs.
///
/// Loaded once at assemble from env; ambient `HOME` is never a writable mount.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainSet {
	/// fenix/rustup root (RO).
	pub rustup_home: Option<PathBuf>,
	/// cargo home (RO preferred; RW only for explicit registries).
	pub cargo_home: Option<PathBuf>,
	/// JAVA_HOME.
	pub java_home: Option<PathBuf>,
	/// GOROOT.
	pub go_root: Option<PathBuf>,
	/// GOPATH (scratch-like; optional).
	pub go_path: Option<PathBuf>,
}

impl ToolchainSet {
	/// Empty set (no toolchains resolved).
	pub fn empty() -> Self {
		Self::default()
	}

	/// From `NUDOX_TOOLCHAIN_*` then standard env vars.
	///
	/// The single sanctioned toolchain-env read (the sealer boundary). Called
	/// once at `ForgeRuntime::assemble`, never from the compiler.
	pub fn from_env() -> Self {
		fn first(keys: &[&str]) -> Option<PathBuf> {
			keys.iter()
				.find_map(|k| std::env::var_os(k).map(PathBuf::from))
				.filter(|p| !p.as_os_str().is_empty())
		}
		Self {
			rustup_home: first(&["NUDOX_TOOLCHAIN_RUSTUP_HOME", "RUSTUP_HOME"]),
			cargo_home: first(&["NUDOX_TOOLCHAIN_CARGO_HOME", "CARGO_HOME"]),
			java_home: first(&["NUDOX_TOOLCHAIN_JAVA_HOME", "JAVA_HOME"]),
			go_root: first(&["NUDOX_TOOLCHAIN_GOROOT", "GOROOT"]),
			go_path: first(&["NUDOX_TOOLCHAIN_GOPATH", "GOPATH"]),
		}
	}

	/// Overlay the toolchain env bindings onto an allowlist.
	pub fn apply_env(&self, mut env: Env) -> Env {
		if let Some(p) = &self.rustup_home {
			env = env.set("RUSTUP_HOME", p);
		}
		if let Some(p) = &self.cargo_home {
			env = env.set("CARGO_HOME", p);
		}
		if let Some(p) = &self.java_home {
			env = env.set("JAVA_HOME", p);
		}
		if let Some(p) = &self.go_root {
			env = env.set("GOROOT", p);
		}
		if let Some(p) = &self.go_path {
			env = env.set("GOPATH", p);
		}
		env
	}

	/// Read-only binds for the toolchain store paths that exist on disk.
	pub fn ro_binds(&self) -> Vec<PathBuf> {
		[
			&self.rustup_home,
			&self.cargo_home,
			&self.java_home,
			&self.go_root,
		]
		.into_iter()
		.filter_map(|p| p.clone())
		.filter(|p| p.exists())
		.collect()
	}

	/// Content hash of the resolved toolchain — part of every `JobKey`.
	///
	/// Path *strings* are hashed (order-stable, labeled) so a toolchain change
	/// invalidates the CAS. Missing entries hash as empty.
	pub fn digest(&self) -> ContentHash {
		let mut h = ContentHash::builder();
		for (label, path) in [
			("rustup_home", &self.rustup_home),
			("cargo_home", &self.cargo_home),
			("java_home", &self.java_home),
			("go_root", &self.go_root),
			("go_path", &self.go_path),
		] {
			h.update(label.as_bytes());
			h.update(&[0]);
			if let Some(p) = path {
				h.update(p.as_os_str().as_encoded_bytes());
			}
			h.update(&[0]);
		}
		h.finalize()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn digest_is_stable_and_sensitive() {
		let a = ToolchainSet {
			rustup_home: Some(PathBuf::from("/nix/store/aaa")),
			..Default::default()
		};
		let b = ToolchainSet {
			rustup_home: Some(PathBuf::from("/nix/store/aaa")),
			..Default::default()
		};
		let c = ToolchainSet {
			rustup_home: Some(PathBuf::from("/nix/store/bbb")),
			..Default::default()
		};
		assert_eq!(a.digest(), b.digest());
		assert_ne!(a.digest(), c.digest());
		assert_ne!(ToolchainSet::empty().digest(), a.digest());
	}
}
