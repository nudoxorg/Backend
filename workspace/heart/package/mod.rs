//! Package coordinates, search hits, and name validation.

pub mod coordinates;

pub use coordinates::{CoordinateError, Coordinates};

use crate::{NameError, PackageId, ResolutionState, ecosystem::Language, score::RankKey};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// The typed hit shape `POST /packages/search` serialises — the package
/// analogue of `Scored<Symbol>` for the symbol surface.
///
/// # Why this type exists
///
/// The client's `search_packages` used to return `Vec<serde_json::Value>`: the
/// server's rich `GlobalPackage` shape was flattened to untyped JSON the caller
/// re-indexed by string key at runtime, so a schema change on the server became
/// a runtime `Value` index-panic in the client rather than a compile error. And
/// because it decoded the response as NDJSON while the server actually answers
/// with a single JSON `Page` object, the two never even agreed on the framing.
///
/// `PackageHit` is the one shared shape both sides name. Its fields are all
/// already-`heart` vocabulary ([`Coordinates`], [`PackageId`],
/// [`ResolutionState`]) plus a lean projection of the server's search facets —
/// so the server *projects into it* (the single conversion point) and the client
/// *decodes it*, and any drift between the two is a compile error at the
/// projection, never a silent runtime mismatch.
///
/// It deliberately does **not** mirror `GlobalPackage`'s full facet record: the
/// heavy metadata lives server-side, and a client rendering a package row needs
/// its coordinates, its pipeline state, and a couple of ranking-visible signals,
/// not the reverse-dependency sweep. Adding a field here is a deliberate wire
/// change, reviewed on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageHit {
    /// The canonical, deterministic global identity of the package. Doubles as
    /// the keyset tiebreak (see the [`RankKey`] impl).
    pub id: PackageId,
    /// The full addressing tuple (origin × name × version). Carries the
    /// ecosystem, so the client filters/labels without a second lookup.
    pub coordinates: Coordinates,
    /// Where the package currently sits in the indexing pipeline.
    pub state: ResolutionState,
    /// Quality in parts-per-million (0..=1_000_000), when facets are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_ppm: Option<u32>,
    /// The manifest description, when facets are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<SmolStr>,
    /// Monthly downloads, when the ecosystem reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<u64>,
}

impl PackageHit {
    /// The package's ecosystem (delegates to the coordinate name).
    pub const fn ecosystem(&self) -> Language {
        self.coordinates.ecosystem()
    }
}

/// A package hit's stable tiebreak key is its durable id — the same
/// determinism guarantee symbols get, so `Scored<PackageHit>` pages keyset-cleanly.
impl RankKey for PackageHit {
    type Key = PackageId;
    fn rank_key(&self) -> PackageId {
        self.id
    }
}

/// A validated, ecosystem-normalized package name.
///
/// The name *grammar* (parse/canonicalize) lives in the `ecosystem` spec crate,
/// which sits *above* `heart`. To keep `heart` the light shared vocabulary (and
/// off the `ecosystem`→`ir`→`registry` link path), this type owns only the
/// already-canonicalized data; construction that needs the per-ecosystem
/// grammar goes through `ecosystem::PackageNameExt::new` (which calls
/// [`PackageName::from_canonical`] after running the spec).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageName {
    /// The ecosystem this name belongs to (part of its identity).
    pub ecosystem: Language,
    /// Our sanitized name.
    canonical: SmolStr,
    /// Their display name.
    original: SmolStr,
}

impl PackageName {
    /// Assemble a `PackageName` from an already-computed canonical form.
    ///
    /// The `ecosystem`-crate constructor (`PackageNameExt::new`) runs the
    /// per-ecosystem grammar (`parse_name` / `render_canonical`) and then calls
    /// this. `heart` itself performs only the grammar-free length check so it
    /// need not depend on the spec crate.
    pub fn from_canonical(
        ecosystem: Language,
        original: impl Into<String>,
        canonical: impl Into<String>,
    ) -> Self {
        Self {
            ecosystem,
            canonical: canonical.into().into(),
            original: original.into().into(),
        }
    }

    /// The grammar-free length check shared by every ecosystem's constructor.
    /// Returns the raw `original` on success so the caller can proceed to the
    /// (spec-driven) grammar validation.
    pub fn check_length(ecosystem: Language, original: &str) -> Result<(), NameError> {
        if original.is_empty() {
            return Err(NameError::NameEmpty);
        }
        let max = length_limit(ecosystem);
        if original.len() > max {
            return Err(NameError::NameTooLong {
                ecosystem,
                len: original.len(),
                max,
            });
        }
        Ok(())
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }
    pub fn original(&self) -> &str {
        &self.original
    }
    pub const fn ecosystem(&self) -> Language {
        self.ecosystem
    }
}

/// The per-ecosystem raw-name length ceiling (crates.io caps at 64; npm and
/// PyPI both live comfortably under npm's documented 214).
pub const fn length_limit(ecosystem: Language) -> usize {
    match ecosystem {
        Language::Rust => 64,
        Language::Typescript | Language::Python => 214,
        // `cpp` names are repository slugs (`host/org/repo`) or scoped forms —
        // roomier than a bare package name; match Go/Java's 256 ceiling.
        Language::Go | Language::Java | Language::CSharp | Language::Cpp => 256,
    }
}

/// Returns whether the char is in the base allowed set for the ecosystem.
/// Grammar rules (start/end/leading-dot etc.) are enforced by `parse_name`.
pub fn is_valid_char_for(ecosystem: Language, c: char) -> bool {
    match ecosystem {
        Language::Rust => c.is_ascii_alphanumeric() || matches!(c, '-' | '_'),
        Language::Typescript | Language::Python => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
        }
        Language::Go | Language::Java => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':')
        }
        Language::CSharp => c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'),
        // `cpp` names may be full repository URLs / SCP forms, so the base set
        // admits URL punctuation (`:` `/` `@` `+` `~`); `parse_name` enforces
        // the real slug grammar via `normalize_repo_url`.
        Language::Cpp => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '+' | '~')
        }
    }
}
