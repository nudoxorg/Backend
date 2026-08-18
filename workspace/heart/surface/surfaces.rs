//! The concrete search surfaces (S2): `Symbols`, `Packages`, `Usages`.
//!
//! # The hole this closes
//!
//! `docs/LOCAL-REMOTE-CONTRACT.md` §0.6: `heart::query::Query` carries a
//! `target: Target` enum, but `QueryEngine` — and, transitively, every route
//! handler — has a single associated `Hit` type per implementation, so the
//! pairing of "what was asked" to "what comes back" was checked at *runtime*
//! (`index/server/http/handlers/search.rs:104`, `query.target != Target::Symbols`
//! → `400`). Each of the three [`Surface`] impls below ties that pairing to a
//! *type*: a caller building a `Serve<Symbols>` request cannot hand it a
//! `Packages` response type, so the runtime guard has nothing left to reject.
//!
//! It is also what makes `RemoteClient`'s blanket `impl<S: Surface> Serve<S>`
//! (`heart::client::remote`) cover these three routes for free — before this
//! module existed, `NudoxClient` needed one hand-written method per route and
//! covered 4 of the server's 17.
//!
//! # Why `Request = heart::query::Query` for all three
//!
//! Keeping the *request* type identical across all three surfaces (open
//! question 1 in the contract doc, resolved in the "share it" direction) is
//! what makes this a wire-compatible addition rather than a breaking one: the
//! JSON body a caller already sends to `/search`, `/packages/search`, or
//! `/usages` is byte-identical before and after this module exists. Only the
//! *response* envelope moves from `heart::stream::StreamFrame<T>` /
//! a bare `heart::Page<T>` to `Frame<S>`.

use serde::{Deserialize, Serialize};

use smol_str::SmolStr;

use crate::identity::PackageId;
use crate::package::PackageHit;
use crate::query::{Query, StableReference};
use crate::score::Scored;

use super::{Surface, SymbolHit};

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// The symbol-search surface: `POST /search`.
///
/// Dedup identity is `(PackageId, path)` — **not** the score and **not** a
/// durable symbol id (see [`Symbols::key`]'s own doc comment for why the
/// latter is disqualified) — because the federating merge
/// (`heart::surface::merge`) suppresses a duplicate row across sources on
/// this key. Keying on anything score-dependent would make the same symbol
/// at two different scores look like two different rows, which is exactly
/// backwards: it is the *lower*-precedence source's copy of a key an overlay
/// already claims that must be suppressed, not two legitimately distinct
/// symbols that happen to collide.
pub struct Symbols;

impl Surface for Symbols {
    const NAME: &'static str = "symbols";
    const PATH: &'static str = "/search";

    type Request = Query;
    type Item = Scored<SymbolHit>;
    type Note = SearchNote;

    /// **Not `SymbolId`.** See [`Symbols::key`] — this is load-bearing.
    type Key = (PackageId, SmolStr);

    /// The dedup identity: the package coordinate plus the fully-qualified path.
    ///
    /// # Why not a durable symbol id
    ///
    /// `heart::Symbol::id` (`SymbolId`) is minted by
    /// `EntryUri::symbol_id(instance_token)` as `UUIDv5(SYMBOL_ns,
    /// instance_token ‖ 0 ‖ package_id/path…)`, where `instance_token` is
    /// `"{org}/{db}"` read from *server configuration*
    /// (`index/server/config.rs:527`). Its own doc comment says the salt is
    /// deliberate — "so two instances of the same corpus don't share ids."
    ///
    /// That property is correct for its purpose and fatal for this one. The
    /// federating merge suppresses a duplicate across sources by comparing
    /// `S::key`; if the local engine and a remote server mint different ids for
    /// the same symbol, **dedup never fires** and every remote hit renders as a
    /// second row beside its local twin. Nothing errors — the answer is just
    /// quietly wrong, which is the failure mode most likely to survive review.
    /// [`SymbolHit`] does not even carry such an id (see its own doc comment on
    /// why) — there is nothing here for a future reader to reach for by
    /// mistake.
    ///
    /// # Why this pair is safe
    ///
    /// Both components are computable independently by either plane and neither
    /// is instance-salted: `PackageId` is a deterministic UUIDv5 over package
    /// coordinates (`identity/derive.rs`'s byte-framing law), and the
    /// fully-qualified path is a property of the source, not of whoever indexed
    /// it. Two instances of the same corpus therefore agree, while distinct
    /// symbols — in the same package or different ones — stay distinct.
    ///
    /// The key also does not depend on **fidelity**: a bare hit and a signed
    /// hit for the same symbol share a `(package, path)` pair, so they are one
    /// identity and [`Symbols::fuse`] gets a chance to run on them at all
    /// (`hit_fusion.rs`'s `fidelity_does_not_change_identity` pins this).
    fn key(item: &Self::Item) -> Self::Key {
        (item.value.package, item.value.path.clone())
    }

    /// Backfill a missing signature and a missing reference; never let a bare
    /// copy erase either one the winner already carries.
    ///
    /// This is the concrete instance of the general rule
    /// [`Surface::fuse`]'s doc comment states: fusion is precedence-wins
    /// *plus backfill*, not a symmetric field-by-field merge. Every field
    /// other than `signature` and `reference` comes from `winner`
    /// unconditionally — a lower-precedence source does not get to overwrite
    /// `winner`'s package, display name, kind, or ecosystem just because it
    /// happened to answer with one. `signature` and `reference` are the sole
    /// exceptions, and only in the direction that adds information: each is
    /// adopted from `loser` **only** when `winner` has none at all; a
    /// `winner` that already carries one keeps its own, even if `loser`'s
    /// differs, because nothing about lower precedence makes `loser`'s copy
    /// more trustworthy — only "having one at all vs. having none" is a
    /// strict improvement here, "one value vs. a different value" is not.
    /// For `reference` in particular, adopting a `loser`'s copy over an
    /// existing `winner` reference would silently retarget a navigation
    /// link, which is never correct regardless of score.
    ///
    /// Idempotent by construction: once `winner.signature` and
    /// `winner.reference` are both `Some(_)` (as they are immediately after
    /// any backfill), a further call with the same or any other `loser`
    /// takes the untouched-`winner` branch for each field and returns
    /// `winner` unchanged.
    fn fuse(winner: Self::Item, loser: Self::Item) -> Self::Item {
        let mut fused = winner;
        if fused.value.signature.is_none() {
            fused.value.signature.clone_from(&loser.value.signature);
        }
        if fused.value.reference.is_none() {
            fused.value.reference.clone_from(&loser.value.reference);
        }
        fused
    }
}

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

/// The package-search surface: `POST /packages/search`.
///
/// `Item = Scored<PackageHit>` reuses the wire type `PackageHit` already
/// established for exactly this reason (see that type's own doc comment): the
/// server projects its rich internal `GlobalPackage` into it at one point, and
/// any drift between what the server sends and what a `Serve<Packages>`
/// consumer expects is a compile error at that projection, never a runtime
/// `Value` index-panic.
pub struct Packages;

impl Surface for Packages {
    const NAME: &'static str = "packages";
    const PATH: &'static str = "/packages/search";

    type Request = Query;
    type Item = Scored<PackageHit>;
    type Note = SearchNote;
    type Key = PackageId;

    fn key(item: &Self::Item) -> Self::Key {
        item.value.id
    }
}

// ---------------------------------------------------------------------------
// Usages
// ---------------------------------------------------------------------------

/// The wire shape of one recorded use of a symbol — the `Usages` surface's
/// item.
///
/// Mirrors `index::server::registry::search::usages::Usage` field-for-field
/// (`within`/`kind`/`relative_span`), the same relationship [`PackageHit`] has
/// to the server's internal `GlobalPackage`: the server-side type is free to
/// carry engine-internal detail (today it does not, but the split keeps that
/// door open) while this is the one thin shape both a `Serve<Usages>` and a
/// client of it name. It lives in `heart` rather than `index` because
/// [`Surface`] impls must be nameable from a client that does not depend on
/// `index` at all (`heart::client::remote::RemoteClient`'s blanket impl is the
/// whole reason this crate can offer `Usages` for free) — and its fields are
/// already exclusively `heart` vocabulary ([`StableReference`], `String`, a
/// `(u32, u32)` span), so this is a projection, not new data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UsageHit {
    /// The stable reference of the symbol that *contains* this use (the
    /// enclosing entry the occurrence hangs off).
    pub within: StableReference,
    /// The kind of reference (`call`, `mcall`, `typeref`, …).
    pub kind: String,
    /// Byte span of the use, relative to the enclosing entry's span start.
    pub relative_span: (u32, u32),
}

/// The usage-lookup surface: `POST /usages`.
///
/// # Why the dedup key is a tuple, not a durable id
///
/// Unlike a `Symbol`/[`SymbolHit`] or a [`PackageHit`], a recorded use has no id of its
/// own — it is a position, not an entity. `(within, kind, relative_span)` is
/// nonetheless a genuine identity: two occurrences with the same enclosing
/// symbol, the same reference kind, and the same relative span *are* the same
/// occurrence (the reverse-`occ` index that produces these is deterministic
/// over a fixed IR snapshot), so the tuple satisfies [`Surface::key`]'s "equal
/// keys imply equal values" contract exactly the way a synthesized id would,
/// without inventing one that would need to be minted and kept stable across
/// re-derivation.
pub struct Usages;

impl Surface for Usages {
    const NAME: &'static str = "usages";
    const PATH: &'static str = "/usages";

    type Request = Query;
    type Item = Scored<UsageHit>;
    type Note = SearchNote;
    type Key = (StableReference, String, (u32, u32));

    fn key(item: &Self::Item) -> Self::Key {
        let UsageHit {
            within,
            kind,
            relative_span,
        } = &item.value;
        (within.clone(), kind.clone(), *relative_span)
    }
}

// ---------------------------------------------------------------------------
// SearchNote
// ---------------------------------------------------------------------------

/// Out-of-band metadata a search surface emits alongside its items.
///
/// Generalises what `SearchEvent::{Latency, SectionState}` carried on the
/// local-only path (contract §1.1) so the remote can say the same things the
/// local engine already says. Without this, a remote answer has no way to
/// report "the semantic index is still building" and a client is left
/// inferring coverage from an empty result set — precisely the false "we
/// searched and found nothing" the local protocol's `SectionState` exists to
/// prevent.
///
/// `#[non_exhaustive]`: a future note kind (a column header, a facet count)
/// must not break an older reader's `match` — see [`Frame`](super::Frame)'s
/// own doc comment for the same reasoning applied to the frame kinds
/// themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SearchNote {
    /// How long this source took to answer, in milliseconds — the remote
    /// analogue of `SearchEvent::Latency`.
    Latency { millis: u64 },
    /// How much of the corpus this answer covers. `total` is `None` when the
    /// corpus size itself is not yet known (the same shape as
    /// [`Completeness::Partial`](super::Completeness::Partial), reused here
    /// as an in-flight progress note rather than only a terminal summary).
    Coverage {
        covered: u64,
        total: Option<u64>,
    },
}
