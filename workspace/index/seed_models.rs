//! System / toolchain model-package seeding (REGISTRYLESS-PLAN §3.5, P10, RL-11).
//!
//! Six toolchain libraries have no upstream registry row of their own but are
//! depended on constantly: `system/libc`, `system/posix`, `system/stdcpp`,
//! `system/pthread`, `system/openssl`, `system/zlib`. The frozen laws model them
//! as **packages** on the `overlay/models` branch (models-as-packages, §16),
//! with stems `system/<name>`. This module emits the [`CatalogOp`]s that create
//! those six stems plus the curated aliases that bind common tokens onto them —
//! most importantly `Threads → system/pthread` (and CMake's `Threads::Threads`).
//!
//! Here we only create the **packages + curated aliases** (stub facts); the
//! model *content* (the `sum.*` IR frames) is IR-plane work, out of scope.
//!
//! # `overlay/models` branch mechanics (honest note)
//! The catalog writer ([`crate::store::writer::CatalogWriter`]) exposes DoltLite
//! branch primitives via its [`VersioningEngine`](crate::engine::VersioningEngine)
//! — `dolt_branch_create` / `dolt_checkout` / `dolt_merge`. The intended
//! deployment mechanic is: create/checkout `overlay/models`, apply these ops,
//! commit, then merge into `main` under the overlay merge policy (ID-9). This
//! module deliberately returns the *ops only* (a pure function) and does **not**
//! drive branch checkout itself, because the single-writer branch position is
//! deployment-bootstrap policy, not seed-data policy. The caller that owns the
//! writer performs the `overlay/models` checkout around
//! [`crate::store::MetaStore::apply_ops`]; if that overlay is not yet wired, the
//! ops apply on `main` and the stems carry their `system/` name prefix as the
//! overlay marker (the `system/` authority is legible in `name_canonical`).

use heart::Language;
use heart::identity::derive;
use smol_str::SmolStr;

use crate::enums::AliasConfidence;
use crate::ids::PackageStemId;
use crate::protocol::{CatalogOp, PackageStemWire};

/// The overlay branch model packages live on (REGISTRYLESS §3.5, ID-9).
pub const MODELS_OVERLAY_BRANCH: &str = "overlay/models";

/// The `system/` stem authority prefix (REGISTRYLESS §3.5 point 2).
pub const SYSTEM_AUTHORITY_PREFIX: &str = "system/";

/// The six seed model-package names (without the `system/` prefix), in a stable
/// order (REGISTRYLESS §3.5).
pub const SYSTEM_MODEL_NAMES: &[&str] =
    &["libc", "posix", "stdcpp", "pthread", "openssl", "zlib"];

/// Derive the deterministic stem id for a `system/<name>` model package.
///
/// Routes through heart's frozen stem framing — the `(cpp_token,
/// system/<name>)` parts under [`heart::identity::derive`] — the SAME law the
/// direct-git enumerator uses, so a system stem and a cpp git stem for the same
/// canonical name derive an identical id (no orphaning across producers). Stable
/// across processes and reruns, so seeding is idempotent (a rerun upserts the
/// *same* stem id).
pub fn system_stem_id(name: &str) -> PackageStemId {
    let canonical = system_stem_name(name);
    let id = derive::package_id_from_parts([
        Language::Cpp.as_token().as_bytes(),
        canonical.as_bytes(),
    ]);
    PackageStemId::from_uuid(*id.as_uuid())
}

/// The canonical name of a system model stem: `system/<name>`.
pub fn system_stem_name(name: &str) -> String {
    format!("{SYSTEM_AUTHORITY_PREFIX}{name}")
}

/// One curated alias binding a manifest token onto a `system/*` stem
/// (REGISTRYLESS §3.5, RL-11). `alias_kind` matches the `package_aliases`
/// vocabulary (e.g. `find_package`, `pkg_config`).
struct SystemAlias {
    /// The `alias_kind` token (matches `EdgeKind → alias_kind` map).
    alias_kind: &'static str,
    /// The alias token as it appears in a manifest (already lowercased for the
    /// case-insensitive resolver join).
    alias: &'static str,
    /// The bare `system/<name>` the alias resolves to.
    system_name: &'static str,
}

/// The curated aliases onto system stems. `Threads` (CMake `find_package`) and
/// its `Threads::Threads` imported-target spelling bind to `system/pthread`
/// (REGISTRYLESS §3.5, §8). Tokens are lowercased to match the resolver, which
/// lowercases `dep_name_canonical` before the lookup.
const SYSTEM_ALIASES: &[SystemAlias] = &[
    SystemAlias { alias_kind: "find_package", alias: "threads", system_name: "pthread" },
    SystemAlias { alias_kind: "find_package", alias: "threads::threads", system_name: "pthread" },
];

/// Build the full set of seed ops: the six `system/*` stems, then the curated
/// aliases onto them (P10). Pure and deterministic — the same ops every call, so
/// re-applying is an idempotent upsert (stems key on their deterministic
/// `stem_id`; aliases key on `(ecosystem, alias_kind, alias)`).
///
/// The caller applies these through [`crate::store::MetaStore::apply_ops`]
/// (ideally on the `overlay/models` branch — see the module note) and commits.
pub fn system_model_seed_ops() -> Vec<CatalogOp> {
    let mut ops = Vec::with_capacity(SYSTEM_MODEL_NAMES.len() + SYSTEM_ALIASES.len());

    for name in SYSTEM_MODEL_NAMES {
        let canonical = system_stem_name(name);
        ops.push(CatalogOp::UpsertPackage {
            stem: PackageStemWire {
                stem_id: system_stem_id(name),
                ecosystem: Language::Cpp,
                // The structured name carries the `system/` authority (there is no
                // separate authority column in schema v4; the prefix IS the marker).
                name_struct: format!("pkg:cpp/{canonical}"),
                name_canonical: canonical.clone(),
                name_original: canonical,
            },
            // System stems have no upstream repository URL — they are models.
            repo_url: None,
        });
    }

    for alias in SYSTEM_ALIASES {
        ops.push(CatalogOp::UpsertAlias {
            ecosystem: Language::Cpp,
            kind: SmolStr::new(alias.alias_kind),
            alias: SmolStr::new(alias.alias),
            stem: system_stem_id(alias.system_name),
            // Curated per RL-11 (a human mapping, not a feed declaration).
            confidence: AliasConfidence::Curated,
        });
    }

    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_ids_are_deterministic() {
        assert_eq!(system_stem_id("pthread"), system_stem_id("pthread"));
        assert_ne!(system_stem_id("pthread"), system_stem_id("zlib"));
    }

    #[test]
    fn seed_emits_six_stems_and_the_thread_aliases() {
        let ops = system_model_seed_ops();
        let stems = ops.iter().filter(|op| matches!(op, CatalogOp::UpsertPackage { .. })).count();
        assert_eq!(stems, 6, "six system model stems");

        let threads_to_pthread = ops.iter().any(|op| {
            matches!(
                op,
                CatalogOp::UpsertAlias { alias, stem, .. }
                    if alias.as_str() == "threads" && *stem == system_stem_id("pthread")
            )
        });
        assert!(threads_to_pthread, "Threads must alias to system/pthread");
    }

    /// Cross-crate consistency: the index seeder's stem derivation MUST agree,
    /// byte-for-byte-of-framing, with the shared heart law that every other
    /// producer (the ingestor's `cpp_stem_id`) routes through — otherwise the
    /// add-by-URL / P6 sealing route would orphan seeded rows.
    #[test]
    fn stem_derivation_agrees_with_shared_heart_law() {
        // `system/pthread` derived here == the same slug through heart directly.
        let via_seeder = system_stem_id("pthread");
        let via_heart = {
            let id = derive::package_id_from_parts([
                Language::Cpp.as_token().as_bytes(),
                b"system/pthread".as_slice(),
            ]);
            PackageStemId::from_uuid(*id.as_uuid())
        };
        assert_eq!(via_seeder, via_heart, "system/pthread must derive identically");

        // A cpp git slug through the shared law is deterministic and distinct.
        let zlib = {
            let id = derive::package_id_from_parts([
                Language::Cpp.as_token().as_bytes(),
                b"github.com/madler/zlib".as_slice(),
            ]);
            PackageStemId::from_uuid(*id.as_uuid())
        };
        assert_ne!(zlib, via_seeder, "distinct slugs derive distinct ids");
    }

    #[test]
    fn seed_ops_are_stable_across_calls() {
        // Idempotence at the op level: identical ops each call ⇒ re-apply is a
        // pure upsert.
        assert_eq!(system_model_seed_ops(), system_model_seed_ops());
    }
}
