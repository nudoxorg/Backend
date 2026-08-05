//! The curated `cpp` alias seed (REGISTRYLESS §3.2, §8, RL-6/RL-7).
//!
//! A bare `find_package(ZLIB)` / `pkg_check_modules(openssl)` token is not a
//! `cpp` name — it is an *alias* that must resolve to a repository-slug stem
//! (`github.com/madler/zlib`) or a `system/<name>` toolchain stem. This module
//! carries the hand-curated seed, deserialized from `assets/cpp_alias_seed.ron`
//! (Repology rules used only as a reference while curating, never bulk-imported
//! — RL-7). The startup reconcile (registry crate) loads these into the
//! `package_aliases` table at confidence `curated`.

use serde::Deserialize;

/// The kind of name being aliased (REGISTRYLESS §3.2). Mirrors the
/// `package_aliases.alias_kind` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasKind {
    /// A vcpkg port name.
    VcpkgPort,
    /// A Conan recipe name.
    ConanRecipe,
    /// A Homebrew formula name.
    BrewFormula,
    /// A CMake `find_package(<Name>)` token.
    FindPackage,
    /// A pkg-config module name.
    PkgConfig,
    /// A meson wrap name.
    MesonWrap,
    /// A Bazel module name.
    BazelModule,
    /// A Debian distro package name.
    DistroDebian,
}

/// Alias confidence tier (REGISTRYLESS §3.2). The seed file is entirely
/// `curated`; feeds contribute `authoritative`, homepage sniffs `heuristic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasConfidence {
    /// The feed itself declares the upstream repo (portfile `REPO`).
    Authoritative,
    /// Our seed file / a human mapping.
    Curated,
    /// Derived (homepage URL sniff); display-gated.
    Heuristic,
}

/// One curated alias entry: a `(kind, alias) → stem` mapping with a confidence
/// tier (REGISTRYLESS §3.2 grammar).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AliasSeedEntry {
    /// The kind of name being aliased.
    pub alias_kind: AliasKind,
    /// The alias token as it appears in manifests (`ZLIB`, `openssl`, …).
    pub alias: String,
    /// The canonical stem it resolves to (`github.com/madler/zlib`,
    /// `system/pthread`).
    pub stem: String,
    /// The confidence tier (`curated` for every seed entry).
    pub confidence: AliasConfidence,
}

/// The seed file, embedded at compile time.
const SEED_RON: &str = include_str!("../assets/cpp_alias_seed.ron");

/// Deserialize the curated alias seed (REGISTRYLESS §8).
///
/// Returns the parse error rather than panicking so a malformed edit surfaces
/// in the startup reconcile's logs, not as a crash.
pub fn load_seed() -> Result<Vec<AliasSeedEntry>, ron::error::SpannedError> {
    ron::from_str(SEED_RON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_parses_and_is_nonempty() {
        let seed = load_seed().expect("cpp_alias_seed.ron must deserialize");
        assert!(
            seed.len() >= 60,
            "expected >= 60 curated aliases, got {}",
            seed.len()
        );
    }

    #[test]
    fn seed_entries_are_all_curated() {
        let seed = load_seed().expect("parses");
        assert!(
            seed.iter()
                .all(|e| e.confidence == AliasConfidence::Curated),
            "every seed entry must be curated"
        );
    }

    #[test]
    fn seed_stems_are_slug_or_system() {
        let seed = load_seed().expect("parses");
        for entry in &seed {
            let is_system = entry.stem.starts_with("system/");
            let is_slug = entry.stem.contains('.') && entry.stem.contains('/');
            assert!(
                is_system || is_slug,
                "stem {:?} for alias {:?} is neither a slug nor a system stem",
                entry.stem,
                entry.alias
            );
        }
    }

    #[test]
    fn known_anchors_present() {
        let seed = load_seed().expect("parses");
        let has = |alias: &str, stem: &str| seed.iter().any(|e| e.alias == alias && e.stem == stem);
        assert!(has("ZLIB", "github.com/madler/zlib"), "ZLIB anchor missing");
        assert!(
            has("Threads", "system/pthread"),
            "Threads→system/pthread missing"
        );
    }
}
