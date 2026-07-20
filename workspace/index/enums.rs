//! The `TEXT`-enum columns of schema v4 (INDEX-PLAN §8) + the two registryless
//! additions (REGISTRYLESS-PLAN §5), as real Rust enums with **total** codecs.
//!
//! Each enum:
//! - renders to its stored token via [`TextEnum::as_token`],
//! - parses back via [`TextEnum::from_token`], returning
//!   [`TextEnumError::UnknownVariant`] on an unrecognized token — a typed error,
//!   never a panic (adversarial requirement),
//! - round-trips for every variant (proved in tests).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Why a stored token could not be decoded into a typed enum variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {enum_name} token: {token:?}")]
pub struct TextEnumError {
    /// The enum that rejected the token (for diagnostics).
    pub enum_name: &'static str,
    /// The offending token.
    pub token: String,
}

/// The contract every `TEXT`-enum column codec satisfies.
pub trait TextEnum: Sized + Copy + 'static {
    /// The stored, lowercase-snake token for this variant.
    fn as_token(&self) -> &'static str;
    /// Parse a stored token; unknown tokens are a typed error.
    fn from_token(token: &str) -> Result<Self, TextEnumError>;
    /// Every variant, in declaration order — the basis of the round-trip tests
    /// and the source a migration `CHECK` constraint could be derived from.
    fn all_variants() -> &'static [Self];
}

/// Declare a total `TEXT`-enum codec: variants ⇔ tokens, with round-trip tests.
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident => $token:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $( $(#[$variant_meta])* $variant ),+
        }

        impl TextEnum for $name {
            fn as_token(&self) -> &'static str {
                match self {
                    $( $name::$variant => $token ),+
                }
            }

            fn from_token(token: &str) -> Result<Self, TextEnumError> {
                match token {
                    $( $token => Ok($name::$variant), )+
                    other => Err(TextEnumError {
                        enum_name: stringify!($name),
                        token: other.to_owned(),
                    }),
                }
            }

            fn all_variants() -> &'static [Self] {
                &[ $( $name::$variant ),+ ]
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_token())
            }
        }
    };
}

text_enum! {
    /// `versions.parse_state` — pipeline lifecycle of a version (INDEX-PLAN §8).
    ParseState {
        /// Discovered, not yet attempted.
        Pending => "pending",
        /// Source acquisition / parse in flight.
        InProgress => "in_progress",
        /// Parsed and IR-eligible.
        Parsed => "parsed",
        /// Terminal failure (see `versions.failure`).
        Failed => "failed",
        /// Skipped by policy (e.g. yanked with no consumers).
        Skipped => "skipped",
    }
}

text_enum! {
    /// `generations.ir_status` — IR seal state, **always set** (INDEX-PLAN ID-15).
    IrStatus {
        /// No IR store linked / no work yet.
        None => "none",
        /// Gen registered; IR seal pending.
        Pending => "pending",
        /// IR sealed at `channel_tip`.
        Sealed => "sealed",
        /// IR production failed.
        Failed => "failed",
    }
}

text_enum! {
    /// `versions.source_kind` — where source bytes came from (INDEX-PLAN ID-13).
    SourceKind {
        /// A git checkout at `source_rev` (preferred).
        Git => "git",
        /// Reconstructed from a registry artifact, sealed as an ObjectPack.
        ReconstructedRegistryPackage => "reconstructed_registry_package",
        /// Provenance not yet determined.
        Unknown => "unknown",
    }
}

text_enum! {
    /// `listing_events.status` — bitemporal listing lifecycle (INDEX-PLAN §8).
    ListingStatus {
        /// Publicly listed / installable.
        Listed => "listed",
        /// Withdrawn (yanked, unpublished).
        Withdrawn => "withdrawn",
        /// A security advisory applies (REGISTRYLESS §12 / OSV).
        Advisory => "advisory",
        /// Marked deprecated upstream.
        Deprecated => "deprecated",
    }
}

text_enum! {
    /// `stores.kind` — the four store families (INDEX-PLAN §8).
    StoreKind {
        /// Local libpijul IR repository.
        IrVcsLocal => "ir_vcs_local",
        /// Remote IR repository over iroh.
        IrVcsIroh => "ir_vcs_iroh",
        /// Local ObjectPack store.
        ObjectPackLocal => "object_pack_local",
        /// Remote ObjectPack provider over iroh.
        ObjectPackIroh => "object_pack_iroh",
    }
}

text_enum! {
    /// `generation_locations.status` / `object_locations.status` (INDEX-PLAN §8).
    LocationStatus {
        /// Present at this store.
        Present => "present",
        /// Known to belong here but not yet fetchable.
        Pending => "pending",
        /// Evicted from this store.
        Evicted => "evicted",
    }
}

text_enum! {
    /// `outbox.sink_kind` / `sink_watermarks.sink_kind` — projection fan-out
    /// targets (INDEX-PLAN ID-3). Mirrors the sinks the followers drain.
    SinkKind {
        /// The tantivy text/package index.
        Text => "text",
        /// The vector plane.
        Vector => "vector",
        /// The reverse-position usage index.
        UsageIndex => "usage_index",
    }
}

text_enum! {
    /// `outbox.op` — the mutation a projection follower must apply (INDEX-PLAN §8).
    OutboxOperation {
        /// Insert or replace the projected row(s).
        Upsert => "upsert",
        /// Delete the projected row(s).
        Delete => "delete",
    }
}

text_enum! {
    /// `edges.kind` — the dependency mechanism (INDEX-PLAN §8 + REGISTRYLESS RL-5).
    /// The registryless C/C++ vocabulary is included so the column is total.
    EdgeKind {
        /// A normal manifest/lockfile runtime dependency.
        Runtime => "runtime",
        /// A build-time / dev dependency.
        Build => "build",
        /// CMake `find_package(<N>)`.
        FindPackage => "find_package",
        /// pkg-config `Requires:` / `pkg_check_modules`.
        PkgConfig => "pkg_config",
        /// A git submodule.
        Submodule => "submodule",
        /// CMake `FetchContent_Declare`.
        FetchContent => "fetchcontent",
        /// A meson wrap subproject.
        Wrap => "wrap",
        /// A vcpkg/conan/brew recipe dependency.
        Recipe => "recipe",
        /// A Bazel `bazel_dep`.
        BazelDep => "bazel_dep",
        /// A detected vendored copy (REGISTRYLESS RL-16, Phase R).
        Vendored => "vendored",
    }
}

text_enum! {
    /// `edges.source` — provenance of an edge fact (INDEX-PLAN §8, REGISTRYLESS RL-5).
    EdgeSource {
        /// A registry/feed listing.
        Feed => "feed",
        /// Extracted from a package manifest.
        Manifest => "manifest",
        /// Read from the git repo directly.
        Git => "git",
        /// Content-detected (Phase R vendored-copy detection).
        Detected => "detected",
    }
}

text_enum! {
    /// `package_aliases.confidence` / `repo_lineage.confidence`
    /// (REGISTRYLESS-PLAN §3.2, §5).
    AliasConfidence {
        /// The feed itself declares the upstream repo.
        Authoritative => "authoritative",
        /// Our seed file / human mapping.
        Curated => "curated",
        /// Derived heuristically; display-gated.
        Heuristic => "heuristic",
    }
}

text_enum! {
    /// `repo_lineage.relation` (REGISTRYLESS-PLAN §5, RL-16).
    LineageRelation {
        /// A real fork with its own diverged commits.
        ForkOf => "fork_of",
        /// A mirror with no independent commits.
        MirrorOf => "mirror_of",
    }
}

text_enum! {
    /// `repo_lineage.evidence` (REGISTRYLESS-PLAN §5, §3.6).
    LineageEvidence {
        /// Shared git ancestry / merge-base (primary).
        GitHistory => "git_history",
        /// Distinctive-file overlap (content fallback).
        ContentOverlap => "content_overlap",
    }
}

text_enum! {
    /// `compile_cache.kind` (INDEX-PLAN §8).
    CompileCacheKind {
        /// An L0 IR tip reference.
        L0Tip => "l0_tip",
        /// An L1 compile-stage ObjectPack member.
        L1Stage => "l1_stage",
        /// A warm-boot golden.
        Golden => "golden",
    }
}
