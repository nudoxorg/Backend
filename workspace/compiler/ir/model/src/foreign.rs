//! Cross-package references — what a producer knows about a symbol in another
//! package at the only moment anyone knows it.
//!
//! # Why this module exists
//!
//! Before this module, [`Lowering::refer_import`](crate::lower::Lowering::refer_import)
//! returned `Ref::Local(import_index)` — an index into a per-package import
//! arena that [`seal`](crate::package) **drops** on the floor. Nothing ever
//! resolved it. `Ref::Local` therefore named two categorically different
//! things ("an export index, which seal rewrites" and "an import index, which
//! seal cannot"), distinguished only by a reserved bit inside `EntryIndex` that
//! `Ref` never inspects. That is the illegal-state-is-representable shape
//! §3 of the doctrine forbids, and it shipped a bare `?` to the GUI for every
//! foreign type in every package, in every language.
//!
//! The fix is to make a cross-package reference **self-contained**: it carries
//! everything needed to render itself and to be linked later, rather than
//! pointing into an arena that will not outlive the build.
//!
//! # The two halves of a cross-package reference
//!
//! A [`StableRef`] has two halves with categorically different availability:
//!
//! * `package: PackageLineageId` — a **producer** fact. Available at the
//!   reference site (Rust holds a live `ra_ap_hir::Crate`; Go holds the import
//!   path).
//! * `intro: IntroId` — a **corpus** fact. `bootstrap_intro_id`'s disambiguator
//!   is selected from collision counts *inside the target's own package*
//!   (`seal`'s `count >= 2` test), so whether `core::option::Option::map`
//!   collides inside `core` is not computable from `memchr`. It is only
//!   obtainable by looking the foreign package up.
//!
//! [`ForeignKey`] is the first half plus everything else a producer can state.
//! [`ForeignResolver`] is how the second half arrives — a *parameter* to
//! `seal`, never something `seal` computes, because the answer is not local
//! information.
//!
//! # Rendering is a pure function of one package
//!
//! [`ForeignKey::display`] exists because `chunk::signature::resolve_nominal`
//! holds one `PackageView` and no corpus. A name the key does not carry is a
//! name nothing downstream can ever print. Threading a corpus into the renderer
//! instead would make the *same* signature render `Clone` or
//! `cargo:core#8f3a…` depending on whether an unrelated package happened to be
//! loaded — a determinism hazard, and exactly the "degraded case mistakable for
//! the good one" shape §8 warns about.

use triomphe::Arc;

use crate::{
    change::{
        EcosystemId, PackageLineageId, StableRef,
        encode::{encode_str, write_u16le},
    },
    kind::KindDiscriminant,
};

/// How precisely the producer could place a cross-package target's owner.
///
/// Exhaustive on purpose (the `ProducerError` precedent in §3): a seventh
/// producer whose situation is none of these must break this match and decide,
/// rather than fall into a `_` bucket. The bucket is how the original defect
/// stayed invisible.
#[derive(Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum ForeignOrigin {
    /// The producer resolved the owning package exactly, in the same spelling
    /// its own lineage uses. Only a `Package` origin can ever link.
    ///
    /// Rust can do this today from the `ra_ap_hir::Crate` handle it already
    /// holds at every reference site.
    Package(PackageLineageId),

    /// A language-level namespace whose *publishing package* the producer
    /// cannot establish: Java's `java.util` (a classpath fact the doclet never
    /// emits), C#'s assembly-qualified name, a Go import path before go.mod
    /// resolution.
    ///
    /// A resolver may still match it by convention; nothing may *guess* a
    /// lineage from it. A wrong link renders as a working hyperlink to the
    /// wrong symbol, which is worse than no link.
    Namespace {
        ecosystem: EcosystemId,
        namespace: Box<str>,
    },

    /// The language itself declares the name; no package owns it. Go's
    /// universe scope (`error`, `comparable`), C builtins.
    ///
    /// This replaces Go's `PackageId::path("")`, which collapsed every
    /// predeclared identifier into one empty pseudo-package.
    Universe { ecosystem: EcosystemId },
}

impl ForeignOrigin {
    /// The ecosystem this target lives in, whichever precision was available.
    pub fn ecosystem(&self) -> &EcosystemId {
        match self {
            ForeignOrigin::Package(l) => &l.ecosystem,
            ForeignOrigin::Namespace { ecosystem, .. } | ForeignOrigin::Universe { ecosystem } => {
                ecosystem
            }
        }
    }

    /// The lineage, when the producer could name one. `None` for `Namespace`
    /// and `Universe` — the states that say "I cannot place this".
    pub fn lineage(&self) -> Option<&PackageLineageId> {
        match self {
            ForeignOrigin::Package(l) => Some(l),
            ForeignOrigin::Namespace { .. } | ForeignOrigin::Universe { .. } => None,
        }
    }

    /// Canonical, endian-stable bytes. A leading tag keeps the three variants
    /// from ever sharing a preimage.
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ForeignOrigin::Package(l) => {
                out.push(0x00);
                l.encode(out);
            }
            ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                out.push(0x01);
                encode_str(out, ecosystem.as_str());
                encode_str(out, namespace);
            }
            ForeignOrigin::Universe { ecosystem } => {
                out.push(0x02);
                encode_str(out, ecosystem.as_str());
            }
        }
    }
}

/// Everything a producer knows about a cross-package target at the moment it
/// makes the reference.
///
/// This is what makes a `Ref::Foreign` *renderable* and *re-linkable* without a
/// corpus. It is also what identity and content hashing encode — never the
/// resolved target — so linking a reference changes no hash (see
/// [`ForeignKey::encode`]).
#[derive(Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct ForeignKey {
    /// How precisely the owning package could be named.
    pub origin: ForeignOrigin,

    /// The target's canonical path in its own language's spelling —
    /// `core::clone::Clone`, `java.util.List`, `sync.Mutex`, a clang USR.
    ///
    /// This is the join key a corpus-side resolver matches on. It is a plain
    /// string because nudox-ir must not know seven path grammars; the cost is
    /// that a producer can silently emit keys that will never join, which is
    /// why [`Resolution`] distinguishes `PathNotFound` from `PackageNotLoaded`.
    pub path: Box<str>,

    /// What to render when the reference is not linked: `Clone`, `List`,
    /// `Mutex`.
    ///
    /// Deliberately **not** derived from `path`: the separator is
    /// language-specific and a clang USR has no leaf at all. The producer knows
    /// this string and nothing downstream ever will.
    pub display: Box<str>,

    /// The target's kind, when the producer knows it.
    ///
    /// `None` is the honest answer for Rust's `make_ref_for!`, which sees only
    /// a path string. Never a placeholder: the code this replaces passed
    /// `IrModule` for every Rust target and `Record` for every Go target, and
    /// the wrongness was invisible because the marker was discarded on the next
    /// line.
    pub kind: Option<KindDiscriminant>,
}

impl ForeignKey {
    /// A key for a target whose owning package is known exactly.
    pub fn in_package(
        package: PackageLineageId,
        path: impl Into<Box<str>>,
        display: impl Into<Box<str>>,
    ) -> Self {
        ForeignKey {
            origin: ForeignOrigin::Package(package),
            path: path.into(),
            display: display.into(),
            kind: None,
        }
    }

    /// A key for a target the producer can name but whose publishing package it
    /// cannot establish.
    pub fn in_namespace(
        ecosystem: EcosystemId,
        namespace: impl Into<Box<str>>,
        path: impl Into<Box<str>>,
        display: impl Into<Box<str>>,
    ) -> Self {
        ForeignKey {
            origin: ForeignOrigin::Namespace {
                ecosystem,
                namespace: namespace.into(),
            },
            path: path.into(),
            display: display.into(),
            kind: None,
        }
    }

    /// A key for a name the language itself declares (Go's `error`).
    pub fn in_universe(
        ecosystem: EcosystemId,
        path: impl Into<Box<str>>,
        display: impl Into<Box<str>>,
    ) -> Self {
        ForeignKey {
            origin: ForeignOrigin::Universe { ecosystem },
            path: path.into(),
            display: display.into(),
            kind: None,
        }
    }

    /// State the target's kind. Chainable; omit it rather than guess.
    #[must_use]
    pub fn with_kind(mut self, kind: KindDiscriminant) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Canonical, endian-stable bytes for identity and content hashing.
    ///
    /// # Why the key and never the target
    ///
    /// `skeleton::ref_` and `content::encode_ref` both hash *this*, not the
    /// `Option<StableRef>` beside it. That is what makes a package sealed with
    /// its dependencies loaded byte-identical to the same package sealed
    /// without them. Hashing the target instead would make a package's content
    /// hash — and therefore `manifest::generation_stamp` — a function of which
    /// *other* packages happened to be in the corpus, so the change plane would
    /// see a phantom generation every time the corpus warmed up.
    pub fn encode(&self, out: &mut Vec<u8>) {
        self.origin.encode(out);
        encode_str(out, &self.path);
        // `display` is deliberately excluded: it is a rendering affordance, and
        // two producers spelling the same target's label differently must not
        // mint different identities for it.
        match self.kind {
            Some(k) => {
                out.push(0x01);
                write_u16le(out, k.as_u16());
            }
            None => out.push(0x00),
        }
    }

    /// Canonical bytes as an owned buffer.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

/// The outcome of asking a [`ForeignResolver`] to place one [`ForeignKey`].
///
/// A flat `Option<StableRef>` cannot distinguish "the dependency is not loaded"
/// (normal, expected, boring) from "this key will never join because the
/// referring producer's path grammar disagrees with the target's" (a producer
/// bug, and the only interesting case). Counting them separately is what turns
/// an unbounded calibration risk into a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The target was found; this is the only variant that produces a link.
    Resolved(StableRef),
    /// The origin names a package the resolver does not have.
    PackageNotLoaded,
    /// The package is present and the path names nothing in it.
    PathNotFound { package: PackageLineageId },
    /// The path names several entries and the key states no kind to pick one.
    /// Refusing is deliberate: a wrong link is worse than no link.
    Ambiguous { candidates: usize },
    /// The origin is a `Namespace` or `Universe` — the producer could not name
    /// a package, so there is nothing to look up. Not a failure; a stated
    /// limitation.
    Unplaceable,
}

/// Supplies the sealed identity of a cross-package target so `seal` can fill in
/// `Ref::Foreign`'s `target`.
///
/// This is a **parameter** to `seal` rather than something `seal` computes,
/// because a foreign `IntroId` is not local information: it depends on the
/// target package's own collision counts, which only the target's own `seal`
/// can know.
pub trait ForeignResolver {
    /// Place `key`, or state precisely why it could not be placed.
    fn resolve(&self, key: &ForeignKey) -> Resolution;
}

/// The resolver for "nothing has been sealed alongside this package".
///
/// Explicit so that *not* linking is a stated decision at each call site. The
/// unstated omission is exactly what the original defect was: `seal` silently
/// left every import ref dangling and had no channel to say so.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unlinked;

impl ForeignResolver for Unlinked {
    fn resolve(&self, key: &ForeignKey) -> Resolution {
        match &key.origin {
            ForeignOrigin::Package(_) => Resolution::PackageNotLoaded,
            ForeignOrigin::Namespace { .. } | ForeignOrigin::Universe { .. } => {
                Resolution::Unplaceable
            }
        }
    }
}

/// A resolver backed by an explicit table, for tests and for callers that have
/// already computed the mapping.
///
/// Exists so the hash-invariance property (`seal` with `&Unlinked` and `seal`
/// with everything linked must produce byte-identical content hashes) is
/// *testable* rather than asserted in a comment.
#[derive(Debug, Default)]
pub struct TableResolver {
    entries: Vec<(Arc<ForeignKey>, StableRef)>,
}

impl TableResolver {
    /// An empty table; every lookup reports `PackageNotLoaded`/`Unplaceable`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `key` resolves to `target`.
    pub fn link(&mut self, key: ForeignKey, target: StableRef) -> &mut Self {
        self.entries.push((Arc::new(key), target));
        self
    }
}

impl ForeignResolver for TableResolver {
    fn resolve(&self, key: &ForeignKey) -> Resolution {
        for (k, target) in &self.entries {
            if k.origin == key.origin && k.path == key.path {
                return Resolution::Resolved(target.clone());
            }
        }
        Unlinked.resolve(key)
    }
}
