//! Typed per-profile / per-package limit overlays (DAEMON-PLAN §2.6).
//!
//! Replaces the process-global `OnceLock<RwLock<HashMap<String, _>>>` with an
//! owned [`OverrideTable`] keyed by a typed [`SandboxKey`]. The table is built
//! once from config at `ForgeRuntime::assemble` and passed down — no install,
//! no global.

use std::collections::HashMap;

use crate::limits::{LimitOverride, Limits};
use crate::profiles::ProducerProfile;

/// A typed key into the override table.
///
/// The package arm carries an origin/name/version triple (no registry dep at
/// this layer — see DAEMON-PLAN §8) so per-package threat-tier overrides can be
/// wired end to end without a `PackageId` type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SandboxKey {
    /// A producer profile (`rust`, `java`, `go`, `nix`, `static_parser`).
    Profile(ProducerProfile),
    /// A specific package by `origin/name@version`.
    Package {
        /// Registry origin label (`crates.io`, `npm`, …).
        origin: String,
        /// Package name.
        name: String,
        /// Package version.
        version: String,
    },
}

impl SandboxKey {
    /// Wire key for a package (`crates.io/serde@1.0.0`).
    pub fn package(
        origin: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::Package {
            origin: origin.into(),
            name: name.into(),
            version: version.into(),
        }
    }

    /// The stringly wire form used as the config-map key.
    fn wire(&self) -> String {
        match self {
            Self::Profile(p) => p.wire_name().to_string(),
            Self::Package {
                origin,
                name,
                version,
            } => format!("{origin}/{name}@{version}"),
        }
    }
}

/// Owned per-key limit overlays, resolved once from config.
#[derive(Debug, Clone, Default)]
pub struct OverrideTable {
    map: HashMap<String, LimitOverride>,
}

impl OverrideTable {
    /// An empty table (tests / no config).
    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Build from a config map (profile wire-names or `origin/name@version` keys).
    pub fn from_config(map: HashMap<String, LimitOverride>) -> Self {
        Self { map }
    }

    /// Resolve limits for a profile, applying (package overlay then) profile
    /// overlay on top of the profile base.
    ///
    /// `package` names a specific package whose per-package overlay takes
    /// precedence over the profile-wide one.
    pub fn resolve(&self, profile: ProducerProfile, package: Option<&SandboxKey>) -> Limits {
        let base = profile.base_limits();
        let mut overlay = LimitOverride::none();
        if let Some(key) = package
            && let Some(o) = self.map.get(&key.wire())
        {
            overlay = merge(overlay, *o);
        }
        if let Some(o) = self.map.get(profile.wire_name()) {
            overlay = merge(overlay, *o);
        }
        overlay.apply(base)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_table_returns_profile_base() {
        let t = OverrideTable::empty();
        assert_eq!(
            t.resolve(ProducerProfile::Rust, None),
            ProducerProfile::Rust.base_limits()
        );
    }

    #[test]
    fn package_overlay_beats_profile() {
        let mut map = HashMap::new();
        map.insert(
            "crates.io/serde@1.0.0".to_string(),
            LimitOverride {
                pids: std::num::NonZeroU32::new(7),
                ..LimitOverride::none()
            },
        );
        let t = OverrideTable::from_config(map);
        let key = SandboxKey::package("crates.io", "serde", "1.0.0");
        let limits = t.resolve(ProducerProfile::Rust, Some(&key));
        assert_eq!(limits.pids.get(), 7);
    }
}
