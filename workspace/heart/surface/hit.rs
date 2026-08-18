//! `SymbolHit`, `Signature`, `SigToken` — the one item type `Symbols` answers
//! with, whether the answer came from the local engine or a remote server.
//!
//! # The error class this closes
//!
//! `docs/LOCAL-REMOTE-CONTRACT.md`'s whole premise is that a caller holding
//! `Arc<dyn Serve<Symbols>>` cannot tell whether it is local, remote, or a
//! federating merge of both. Before this module existed that was false in
//! practice: the GUI rendered `nudox_engine::wire::HitRow` locally (carrying a
//! rendered `sig_preview: Vec<SigToken>`, which needs IR) while the remote
//! produced bare `heart::Symbol` (name/kind/package only, no signature at
//! all). Two item shapes for one surface is exactly the thing `Surface`
//! exists to make impossible.
//!
//! The tempting fix — downgrade local to `Symbol` so both sides match — is
//! backwards: it deletes signature previews from the UI to make remote rows
//! *not look* poorer, i.e. it makes every row worse to hide that some rows are
//! worse. [`SymbolHit`] instead makes the signature **optional**, so a source
//! that cannot supply one (today, every remote source; see
//! [`SymbolHit::from`]'s own doc comment) is honestly represented without
//! forcing every other source down to its level.
//!
//! # `None` is a claim about the SOURCE, not the symbol
//!
//! This is the whole design, so it is worth saying three times in three
//! places (here, [`SymbolHit::signature`]'s field doc, and
//! `hit_fusion.rs`'s module doc): [`SymbolHit::signature`] being `None` means
//! *"whatever produced this row could not supply a signature"* — never *"this
//! symbol has no signature."* The distinction matters operationally: `None` is a
//! prompt to go fetch a richer copy (from a source that *can* answer, or from
//! a future IR-serving upgrade — `docs/IR-STORAGE-PLAN.md` §3); "this symbol
//! truly has no signature" is not a real state a reader would ever act on.
//! [`Residence`](super::Residence) already exists to say *where* a datum came
//! from and how much to trust it; an absent signature is exactly the kind of
//! fact `Residence` was built to contextualise, so `None` here composes with
//! it rather than duplicating it.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::ecosystem::Language;
use crate::identity::PackageId;
use crate::query::StableReference;
use crate::symbol::{Symbol, SymbolKind};

// ---------------------------------------------------------------------------
// SigToken
// ---------------------------------------------------------------------------

/// One rendered piece of a symbol's signature preview.
///
/// This is `nudox_engine::wire::SigToken`'s vocabulary — the same seven
/// variants, the same reasoning per variant — lifted into `heart` so a
/// signature can travel on the `Symbols` wire rather than living only inside
/// the local engine's IR-backed render path. Two deliberate departures from
/// the original, both forced by the fact that this type now has to cross a
/// wire and be handed to a caller (a remote server) the local engine's IR
/// graph is meaningless to:
///
/// * **`Kw`/`Punct` are [`SmolStr`], not `&'static str`.** The original
///   interns these from a small fixed keyword/punctuation table baked into
///   the local engine's binary, which is exactly why `&'static str` worked
///   there — every value handed to `Kw`/`Punct` really was a `'static`
///   string literal. That property cannot survive `serde_json::from_str`:
///   deserializing has to *produce* a string from bytes read off the wire,
///   and nothing a `Deserialize` impl allocates at runtime can be `'static`
///   without leaking it. `SmolStr` keeps the same "cheap to hold, cheap to
///   clone" property for the short strings every variant here actually holds
///   (`"fn"`, `"("`, `"T"`, `"'a"`, …) via its inline-small-string
///   optimization, without the `'static` requirement `Deserialize` cannot
///   satisfy. Every other field on every other variant already uses
///   `SmolStr`-family types elsewhere in `heart` (`Name`, `PackageName`,
///   `Symbols::Key`'s path component), so this also stops `SigToken` from
///   being the one place in this crate reaching for a different owned-string
///   type.
/// * **`Ty`'s link target is `Option<StableReference>`, not
///   `Option<SymbolKey>`.** `SymbolKey` is local-engine-internal and, per
///   `LOCAL-REMOTE-CONTRACT.md` §0.7, exactly the kind of id that must never
///   be used as cross-plane identity: it is derived from in-process graph
///   state a remote server does not have. `StableReference`'s frozen
///   `F:<eco>/<pkg>#<hex>` grammar is instance-independent by construction
///   (it is already the type this crate uses everywhere a symbol reference
///   has to survive a hop across the wire), so a signature a remote server
///   rendered stays navigable on a client that has never talked to that
///   server's local graph.
///
/// Serialized externally-tagged, snake_case, the same discipline
/// [`super::Frame`] uses: `{"kw":"fn"}`, `{"ident":"from_str"}`,
/// `{"ty":{"text":"&str"}}`, `{"punct":"("}`, `"ws"`, `{"generic":"T"}`,
/// `{"lifetime":"'a"}` — serde's default derived representation for a mix of
/// unit/newtype/struct variants, chosen over an explicit `tag`/`content`
/// pairing because `Ty` alone carries more than one field and an adjacently
/// tagged encoding would nest its two fields inside a `content` object for
/// no reason the other six variants share.
///
/// `#[non_exhaustive]`: a future token kind (an attribute, a doc-link marker)
/// must not break an older reader's `match` — the same reasoning
/// [`super::Frame`] and [`super::Residence`] already apply to themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SigToken {
    /// A language keyword (`fn`, `struct`, `impl`, …).
    Kw(SmolStr),
    /// An identifier (symbol name, parameter name, …).
    Ident(SmolStr),
    /// A type reference, optionally linked to another symbol.
    Ty {
        text: SmolStr,
        /// The linked symbol, when the renderer could resolve one. Absent
        /// for a type this crate could not, or chose not to, cross-reference
        /// (a primitive, an unresolved generic bound, …) — not itself a
        /// signal about whether *the signature* is complete.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<StableReference>,
    },
    /// Punctuation (`(`, `)`, `,`, `->`, `<`, `>`, …).
    Punct(SmolStr),
    /// A single whitespace separator — keeps rendering logic whitespace-aware
    /// rather than forcing every consumer to re-derive spacing from token
    /// adjacency.
    Ws,
    /// A generic parameter name (`T`, `K`, `V`, …).
    Generic(SmolStr),
    /// A lifetime name (`'a`, `'static`, …).
    Lifetime(SmolStr),
}

impl SigToken {
    /// A keyword token (`fn`, `struct`, `impl`, …).
    pub fn keyword(text: impl Into<SmolStr>) -> Self {
        SigToken::Kw(text.into())
    }

    /// An identifier token (a symbol or parameter name).
    pub fn ident(text: impl Into<SmolStr>) -> Self {
        SigToken::Ident(text.into())
    }

    /// A punctuation token.
    pub fn punct(text: impl Into<SmolStr>) -> Self {
        SigToken::Punct(text.into())
    }

    /// A single whitespace separator.
    pub const fn space() -> Self {
        SigToken::Ws
    }

    /// A type-reference token, optionally linked to another symbol via its
    /// instance-independent [`StableReference`].
    pub fn ty(text: impl Into<SmolStr>, target: Option<StableReference>) -> Self {
        SigToken::Ty {
            text: text.into(),
            target,
        }
    }

    /// A generic parameter token.
    pub fn generic(text: impl Into<SmolStr>) -> Self {
        SigToken::Generic(text.into())
    }

    /// A lifetime token.
    pub fn lifetime(text: impl Into<SmolStr>) -> Self {
        SigToken::Lifetime(text.into())
    }
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// A rendered signature preview: an ordered run of [`SigToken`]s.
///
/// A thin newtype rather than a bare `Vec<SigToken>` so [`SymbolHit`] has one
/// named place to hang [`Signature::token_count`] and any future
/// signature-level operation (truncation for a narrow column, say) without
/// reaching for free functions over a `Vec` a caller could otherwise mutate
/// in ways that break the renderer's assumptions (token 0 is never `Ws`,
/// etc.). Serializes transparently as a bare JSON array of tokens — serde's
/// default behaviour for a single-field tuple struct — so this newtype costs
/// nothing on the wire relative to `Vec<SigToken>` directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(Vec<SigToken>);

impl Signature {
    /// Wrap an already-rendered token run.
    pub const fn new(tokens: Vec<SigToken>) -> Self {
        Self(tokens)
    }

    /// The tokens, in render order.
    pub fn tokens(&self) -> &[SigToken] {
        &self.0
    }

    /// How many tokens this signature carries.
    pub fn token_count(&self) -> usize {
        self.0.len()
    }
}

// ---------------------------------------------------------------------------
// SymbolHit
// ---------------------------------------------------------------------------

/// The one item type `Symbols` answers with — produced identically by the
/// local engine and a remote server, per `LOCAL-REMOTE-CONTRACT.md`'s
/// transparency requirement.
///
/// # Why there is no `id` field
///
/// The type this replaces as `Symbols::Item`'s payload, `heart::Symbol`,
/// carries a `SymbolId` — and `LOCAL-REMOTE-CONTRACT.md` §0.7 is explicit
/// that `SymbolId` must never be treated as cross-plane identity: it is
/// salted with the indexing instance's own `{org}/{db}` token specifically
/// so that two instances of the same corpus do *not* share ids
/// (`EntryUri::symbol_id`'s own doc comment). `Symbols::key` already had to
/// stop using it for dedup for exactly this reason (`(PackageId, path)`
/// instead). Carrying `SymbolId` on the wire item anyway — even unused by
/// `key` — would leave a trap for the next reader who reaches for "the
/// obvious identity field" without re-deriving why it cannot be one. Omitting
/// it outright means there is nothing on this type a caller could
/// mistakenly compare across instances.
///
/// # `signature`
///
/// `None` means the source that produced this row could not supply a
/// rendered signature — **not** that the symbol has no signature. See this
/// module's top-level doc comment for the full reasoning; see
/// [`SymbolHit::has_signature`] for the one true way to ask "do I have one
/// right now."
///
/// `#[serde(skip_serializing_if = "Option::is_none")]` is load-bearing, not
/// cosmetic: most rows come from a source without IR (today, every remote
/// row; see [`SymbolHit::from`]), so most `signature` fields on the wire are
/// `None`. `/search` is the hottest path in this system — a `"signature":
/// null` on every line would be pure overhead paid on every hit, forever,
/// for a value that is absent far more often than it is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolHit {
    /// The package this symbol belongs to.
    pub package: PackageId,
    /// The fully-qualified path — the other half of [`super::Symbols`]'s
    /// dedup key (`(package, path)`), and a property of the source, not of
    /// whoever indexed it.
    pub path: SmolStr,
    /// The bare display name (the last path component, typically).
    pub display_name: SmolStr,
    /// The ecosystem, carried so a language-erased read plane can still
    /// filter without a second lookup.
    pub ecosystem: Language,
    /// What kind of code entity this symbol is.
    pub kind: SymbolKind,
    /// A rendered signature preview, when the source that produced this row
    /// could supply one. See this type's own doc comment — `None` is a
    /// statement about the *source*, never about the symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

impl SymbolHit {
    /// Whether this row currently carries a rendered signature. The one true
    /// way to ask — never match `self.signature` directly and call the
    /// `None` case "no signature" in a comment or a log line, because that is
    /// exactly the conflation this type exists to prevent.
    pub const fn has_signature(&self) -> bool {
        self.signature.is_some()
    }
}

impl From<Symbol> for SymbolHit {
    /// Project the engine's rich [`Symbol`] down to the wire hit shape, with
    /// **no** signature.
    ///
    /// This is a *current source limitation*, not a permanent property of
    /// the conversion: every caller of this `From` impl today
    /// (`index::server::coordination::search`'s `precise_source_answer` /
    /// `semantic_source_answer`) answers from catalog/text-index/qdrant
    /// metadata alone — none of them have IR loaded, so none of them could
    /// render a signature even if this conversion tried to synthesize one.
    /// `docs/IR-STORAGE-PLAN.md` §3 is explicit that the IR section of the
    /// catalog is currently *write-only*: bytes are stored but nothing reads
    /// them back yet. Once that lands, the server gets a real code path that
    /// *can* produce `Some(Signature)` for a hit it already has IR for, and
    /// this conversion (or a sibling one, at the call site that has IR in
    /// hand) is the strict upgrade — no schema change, because
    /// `SymbolHit::signature` was already `Option`.
    fn from(symbol: Symbol) -> Self {
        SymbolHit {
            package: symbol.package,
            path: symbol.name.fully_qualified,
            display_name: symbol.name.plain,
            ecosystem: symbol.ecosystem,
            kind: symbol.kind,
            signature: None,
        }
    }
}
