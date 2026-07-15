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
	/// Extra directories to prepend to the sealed `PATH`, from
	/// `NUDOX_TOOLCHAIN_PATH` (colon-separated). Empty by default, so the
	/// hermetic PATH is unchanged unless explicitly opted in. Intended for dev
	/// hosts where the toolchain binaries (`go`, `javadoc`) live outside the
	/// fixed hermetic PATH (e.g. a Nix devshell store path).
	pub path_dirs: Vec<PathBuf>,
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
	///
	/// Language oracles (Go/Java/C#) still shell out to host SDKs after the
	/// sealer installs a hermetic PATH. Those SDK dirs must be injected here
	/// via `NUDOX_TOOLCHAIN_PATH` (devshell) or an assembled [`ToolchainSet`]
	/// (server) — never ambient `which` discovery.
	pub fn from_env() -> Self {
		fn first(keys: &[&str]) -> Option<PathBuf> {
			keys.iter()
				.find_map(|k| std::env::var_os(k).map(PathBuf::from))
				.filter(|p| !p.as_os_str().is_empty())
		}
		let path_dirs = std::env::var_os("NUDOX_TOOLCHAIN_PATH")
			.map(|v| std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect())
			.unwrap_or_default();
		Self {
			rustup_home: first(&["NUDOX_TOOLCHAIN_RUSTUP_HOME", "RUSTUP_HOME"]),
			cargo_home: first(&["NUDOX_TOOLCHAIN_CARGO_HOME", "CARGO_HOME"]),
			java_home: first(&["NUDOX_TOOLCHAIN_JAVA_HOME", "JAVA_HOME"]),
			go_root: first(&["NUDOX_TOOLCHAIN_GOROOT", "GOROOT"]),
			go_path: first(&["NUDOX_TOOLCHAIN_GOPATH", "GOPATH"]),
			path_dirs,
		}
	}

	/// The extra PATH dirs joined for prepending, if any (`dir1:dir2`).
	pub fn path_prefix(&self) -> Option<String> {
		if self.path_dirs.is_empty() {
			return None;
		}
		std::env::join_paths(&self.path_dirs)
			.ok()
			.map(|s| s.to_string_lossy().into_owned())
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
		.chain(self.path_dirs.iter().cloned())
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
		h.update(b"path_dirs");
		h.update(&[0]);
		for p in &self.path_dirs {
			h.update(p.as_os_str().as_encoded_bytes());
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
