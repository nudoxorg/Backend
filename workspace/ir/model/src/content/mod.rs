//! Entry content-hash and entry storage-hash — the *third* and *fourth*
//! identity layers.
//!
//! nudox-ir carries two existing identity concepts:
//!
//! 1. **Nominal identity** ([`IntroId`]): answers "which declaration is this,
//!    across versions?" Assigned once at first insertion; survives renames.
//!
//! 2. **Structural identity** ([`Skeleton`][crate::skeleton]): a deliberate
//!    partial fingerprint of type structure used only for collision-scoped
//!    disambiguation when minting `IntroId`s.
//!
//! This module adds:
//!
//! 3. **Content identity** ([`entry_content_hash`]): answers "has *this
//!    declaration's payload* changed since the last snapshot?" It is a total,
//!    deterministic BLAKE3 hash of every field that describes the declaration
//!    itself — its symbol metadata and its kind body.
//!
//! 4. **Storage identity** ([`entry_storage_hash`]): answers a *different*
//!    question — "is this the same stored payload, independent of where it
//!    currently sits?" See "Storage identity" below for why this could not be
//!    answered by reusing (3).
//!
//! # Storage identity: why a fourth thing is needed (`docs/IR-STORAGE-PLAN.md` §3a.1)
//!
//! `entry_content_hash` folds `sym.source` and `sym.span.start`/`.end`
//! (see `encode_symbol`, below) into its preimage — the declaration's **byte
//! offsets in its file**. That is correct for the question it answers ("did
//! anything about this entry change, *including where it is*?" — the same
//! "the source changed" framing `encode_generic_param`'s comment applies to
//! renamed generic parameters applies here too, and is equally true of a
//! moved declaration). But it makes `entry_content_hash` unusable as a
//! **storage identity**: editing one line at the top of a file shifts every
//! later declaration's span, so every declaration below the edit gets a new
//! hash even though not one byte of *its own* text changed.
//!
//! This was measured, not assumed. Running the real producers over one real
//! adjacent-version pair (testify v1.9.0 → v1.11.1) and diffing entries by
//! `IntroId`:
//!
//! | hash | reported "modified" |
//! |---|---|
//! | `entry_content_hash` as-is | **75.6%** |
//! | same, with `source`/`span` excluded | **1.3%** |
//!
//! A 58× inflation of apparent churn, entirely from code *moving* rather than
//! *changing*. `manifest::generation_stamp` already folds `entry_content_hash`
//! over every entry to build a `(IntroId, ContentBlake3)` sequence — precisely
//! the shape a storage-dedup plane would want — so a naive reuse of that hash
//! for storage identity would make a real deployment's dedup ratio look
//! catastrophically worse than it is, for reasons unrelated to real edits.
//!
//! [`entry_storage_hash`] is the fix: the same total, deterministic encoding as
//! [`entry_content_hash`], minus `sym.source`/`sym.span`. Two hashes, two
//! questions — this mirrors an existing split in this codebase,
//! `index::blob::BlobManifest`'s Hash①/Hash②
//! (`workspace/index/blob/mod.rs:67-105`), whose own doc comment warns that
//! unifying a change-detection hash with a storage-address hash is a bug class,
//! not a simplification. Where does position go instead? It becomes
//! **generation-scoped, not content-scoped** — it belongs on the (as yet
//! unbuilt) `GenerationRoot`, which is rewritten every generation anyway,
//! rather than inside the content-addressed body, which is precisely what must
//! stay stable across a pure move. See `workspace/ir/model/tests/storage_hash.
//! rs` for the pinned spec.
//!
//! # Why the two layers are kept orthogonal
//!
//! The hash covers everything *except* the entry's own `IntroId` and its `Node`
//! tree edges (parent + children). This is intentional:
//!
//! - **`IntroId` exclusion** — nominal and content identity are complementary
//!   questions. The nominal id is the *key*; the content hash is the *value
//!   fingerprint*. Folding the key into its own value fingerprint would couple
//!   two concepts that must remain independent.
//!
//! - **`Node` (parent/children) exclusion** — children are *relational
//!   structure*, not content. If a parent entry's hash included its children,
//!   adding a leaf field declaration would cascade hash changes up through the
//!   module, the package root, and every ancestor — destroying the fast-path
//!   "this declaration is unchanged" equality check that this hash exists to
//!   provide. Each child is already identified by its own `IntroId`. The
//!   relational structure is captured separately by the edge table; it need not
//!   be duplicated in every ancestor's content hash.
//!
//! # Determinism contract (frozen — never change opcodes/layout)
//!
//! This module's encoding is **wire-stable**. Once a hash is committed to an
//! `ir-vcs` checkpoint, it must reproduce identically from the same `Entry` on
//! any platform and at any future date. The rules that enforce this:
//!
//! - Every sequence is `u32le(count)` then each element — no magic separators.
//!   Length prefixes make the encoding self-delimiting without them.
//! - Every enum variant has a **distinct opcode byte** listed in the opcode
//!   tables below. There are **no `_` catch-alls** anywhere in this module: a
//!   wildcard would silently stop distinguishing new variants and is a
//!   data-corruption bug waiting to happen. Every new variant added to any enum
//!   covered here must be assigned a new opcode here too.
//! - No `{:?}`, `Display`, `serde`, or platform-dependent formatting is used as
//!   hash input. All strings are `encode_str` (u32le length-prefix + UTF-8).
//! - Integers are `write_u16le` / `write_u32le` / `write_u64le`.
//! - Booleans are written as a single `0x00` or `0x01` byte.
//! - `usize` values are widened to `u64` before encoding so that 32-bit vs
//!   64-bit platforms produce the same bytes.
//!
//! # Opcode tables
//!
//! ## `Kind` discriminants (from `kind.rs` `register_kinds!` literals)
//!
//! | Variant     | Wire opcode (u16le) |
//! |-------------|---------------------|
//! | Module      | 1                   |
//! | Record      | 2                   |
//! | Field       | 3                   |
//! | Function    | 4                   |
//! | Alias       | 5                   |
//! | Trait       | 6                   |
//! | Impl        | 7                   |
//! | Enum        | 8                   |
//! | Variant     | 9                   |
//! | Const       | 10                  |
//! | Static      | 11                  |
//! | Reexport    | 12                  |
//! | Param       | 13                  |
//!
//! ## `Ref` opcodes (for `Type::Nominal` and kind-body refs)
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Local     | 0x00   |
//! | Intro     | 0x01   |
//! | Foreign   | 0x02   |
//!
//! `Ref::Local` MUST NOT appear in a post-seal entry (seal lowers all locals to
//! `Intro` or `Foreign`). It is given opcode `0x00` so the function is total
//! and never panics in release. A `debug_assert!` fires in debug builds to
//! catch seal bugs early.
//!
//! ## `Type` opcodes
//!
//! | Variant         | Opcode |
//! |-----------------|--------|
//! | SelfType        | 0x01   |
//! | Primitive       | 0x02   |
//! | Tuple           | 0x03   |
//! | Slice           | 0x04   |
//! | Array           | 0x05   |
//! | Union           | 0x06   |
//! | Intersection    | 0x07   |
//! | Never           | 0x08   |
//! | Any             | 0x09   |
//! | Nominal         | 0x0a   |
//! | Apply           | 0x0b   |
//! | TypeVar         | 0x0c   |
//! | Wildcard        | 0x0d   |
//! | FunctionPointer | 0x0e   |
//! | Annotated       | 0x0f   |
//! | Conditional     | 0x10   |
//! | Mapped          | 0x11   |
//! | TemplateLiteral | 0x12   |
//! | AnonymousRecord | 0x13   |
//! | ImplTrait       | 0x14   |
//! | DynTrait        | 0x15   |
//! | Inferred        | 0x16   |
//! | QualifiedPath   | 0x17   |
//!
//! ## `MappedModifier` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | Add     | 0x01   |
//! | Remove  | 0x02   |
//! | Absent  | 0x03   |
//!
//! ## `TemplatePart` opcodes
//!
//! | Variant      | Opcode |
//! |--------------|--------|
//! | Literal      | 0x01   |
//! | Interpolated | 0x02   |
//!
//! ## `AnonRecordForm` opcodes
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Struct    | 0x01   |
//! | Interface | 0x02   |
//!
//! ## `Primitive` opcodes
//!
//! | Variant       | Opcode |
//! |---------------|--------|
//! | Integer       | 0x01   |
//! | Float         | 0x02   |
//! | Bool          | 0x03   |
//! | Char          | 0x04   |
//! | Str           | 0x05   |
//! | MutPointer    | 0x06   |
//! | ConstPointer  | 0x07   |
//! | Reference     | 0x08   |
//! | Builtin       | 0x09   |
//!
//! ## `Width` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | Fixed   | 0x01   |
//! | Arch    | 0x02   |
//!
//! ## `GenericParam` opcodes
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Lifetime  | 0x01   |
//! | Type      | 0x02   |
//! | Const     | 0x03   |
//!
//! ## `Visibility` opcodes
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Public    | 0x01   |
//! | Private   | 0x02   |
//! | Protected | 0x03   |
//! | Internal  | 0x04   |
//! | Package   | 0x05   |
//! | Crate     | 0x06   |
//!
//! ## `CfgExpr` opcodes
//!
//! | Variant    | Opcode |
//! |------------|--------|
//! | All        | 0x01   |
//! | Any        | 0x02   |
//! | Not        | 0x03   |
//! | Feature    | 0x04   |
//! | TargetOs   | 0x05   |
//! | TargetArch | 0x06   |
//! | Other      | 0x07   |
//!
//! ## `RecordForm` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | Struct  | 0x01   |
//! | Tuple   | 0x02   |
//! | Unit    | 0x03   |
//!
//! ## `FieldKey` opcodes
//!
//! | Variant     | Opcode |
//! |-------------|--------|
//! | Named       | 0x01   |
//! | Positional  | 0x02   |
//!
//! ## `FieldAttribute` opcodes
//!
//! | Variant  | Opcode |
//! |----------|--------|
//! | Mutable  | 0x01   |
//! | Optional | 0x02   |
//! | Static   | 0x03   |
//!
//! ## `Receiver` opcodes
//!
//! | Variant    | Opcode |
//! |------------|--------|
//! | Owned      | 0x01   |
//! | SharedRef  | 0x02   |
//! | MutRef     | 0x03   |
//! | Arbitrary  | 0x04   |
//!
//! ## `FnModifier` opcodes
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Async     | 0x01   |
//! | Const     | 0x02   |
//! | Unsafe    | 0x03   |
//! | Pure      | 0x04   |
//! | Generator | 0x05   |
//!
//! ## `ParamAttribute` opcodes
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Inout     | 0x01   |
//! | Mutable   | 0x02   |
//! | Consuming | 0x03   |
//! | Borrowing | 0x04   |
//! | Isolated  | 0x05   |
//! | Variadic  | 0x06   |
//! | Optional  | 0x07   |
//! | KeywordOnly | 0x08 |
//! | Kwargs    | 0x09   |
//! | Out       | 0x0A   |
//!
//! ## `VariantForm` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | Unit    | 0x01   |
//! | Tuple   | 0x02   |
//! | Struct  | 0x03   |
//!
//! ## `TriState` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | Yes     | 0x01   |
//! | No      | 0x02   |
//! | Unknown | 0x03   |
//!
//! ## `Sealed` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | None    | 0x01   |
//! | PubApi  | 0x02   |
//! | Full    | 0x03   |
//!
//! ## `AutoTrait` opcodes
//!
//! | Variant        | Opcode |
//! |----------------|--------|
//! | Send           | 0x01   |
//! | Sync           | 0x02   |
//! | Unpin          | 0x03   |
//! | UnwindSafe     | 0x04   |
//! | RefUnwindSafe  | 0x05   |
//!
//! ## `AutoState` opcodes
//!
//! | Variant | Opcode |
//! |---------|--------|
//! | Yes     | 0x01   |
//! | No      | 0x02   |
//! | Cond    | 0x03   |
//!
//! ## `ImplFlags` — two boolean bytes (negative, blanket)
//!
//! `negative as u8` then `blanket as u8` — no opcode needed; the struct has a
//! fixed shape.
//!
//! ## `TraitFlags` — four bytes
//!
//! `is_unsafe as u8`, `is_auto as u8`, `dyn_compat opcode`, `sealed opcode`.
//!
//! ## `EntryInner` opcodes
//!
//! | Variant   | Opcode |
//! |-----------|--------|
//! | Owned     | 0x01   |
//! | Reference | 0x02   |

use crate::{
    change::{
        ContentBlake3,
        encode::{ByteSink, HasherSink, encode_str, write_u16le, write_u32le, write_u64le},
    },
    entry::{AttrTok, CfgExpr, Deprecation, DocLink, Entry, EntryInner, Symbol, Visibility},
    index::{RawRef, Ref},
    kind::Kind,
    kinds::{
        Alias, AutoFact, AutoState, AutoTrait, Const, ConstExpr, ConstToken, Enum, Field,
        FieldAttribute, FieldKey, FnModifier, Function, GenericParam, Impl, ImplFlags, Module,
        Param, ParamAttribute, Receiver, Record, RecordForm, Reexport, Sealed, Static, Trait,
        TraitFlags, TriState, Variant, VariantForm, WherePred,
        ty::{
            AnonRecordForm, MappedModifier, Primitive, TemplatePart, TupleElement, Type,
            UnknownType, Variance, Width,
        },
    },
};

/// Domain tag for entry content-hash preimages.
///
/// `v3` matches the `IntroId` format version so that the two hashing planes
/// are clearly versioned together. A format-breaking change to either plane
/// must bump both domains.
pub const ENTRY_CONTENT_DOMAIN: &str = "nudox.entry.v4";

/// Domain tag for entry storage-hash preimages.
///
/// A **sibling** of [`ENTRY_CONTENT_DOMAIN`], not a reuse of it. `blake3(domain
/// || preimage)` (see `change::hash::hash_domain`) is domain-separated by
/// construction whenever the two domain strings differ, so keeping this a
/// distinct constant — rather than branching on a flag inside a single
/// `from_domain` call — is what guarantees
/// [`entry_content_hash`] and [`entry_storage_hash`] can never coincide, even
/// on an entry whose position-bearing fields are empty and whose encoded
/// bytes would otherwise be identical (see
/// `the_two_hashes_are_domain_separated` in `tests/storage_hash.rs`).
///
/// Versioned independently of `ENTRY_CONTENT_DOMAIN`: this hash has its own
/// wire format (the same total encoding, minus position) and its own
/// evolution — a future field added only to one preimage must not force a
/// version bump on the other.
pub const ENTRY_STORAGE_DOMAIN: &str = "nudox.entry.storage.v1";

/// Compute the content hash of an [`Entry`].
///
/// The hash is a domain-separated BLAKE3 digest over the entry's [`Symbol`]
/// and [`EntryInner`] in full. The entry's own `IntroId` and its `Node` tree
/// edges (parent + children) are **excluded**; see the module documentation
/// for the rationale.
///
/// This hash is **position-sensitive**: it includes `sym.source` and
/// `sym.span`, so a declaration that only moved (same file, different offset,
/// or a different file entirely) gets a different hash. That is intentional —
/// this function answers "did anything about this entry change, including
/// where it is?" — but it means this hash must never be used as a *storage*
/// identity; use [`entry_storage_hash`] for that. See the module
/// documentation's "Storage identity" section for the measured cost of getting
/// this mixed up (a 58× churn inflation on a real package pair).
///
/// # Panics (debug)
///
/// A `debug_assert!` fires if any `Ref::Local` is encountered in the entry.
/// `Ref::Local` must have been lowered to `Ref::Intro` or `Ref::Foreign` by
/// the seal pass before content hashing. A `Local` here indicates a bug in the
/// seal pass.
pub fn entry_content_hash(entry: &Entry) -> ContentBlake3 {
    let mut buf = Vec::new();
    encode_symbol(&mut buf, entry.sym(), SymbolPosition::Included);
    encode_entry_inner(&mut buf, entry.kind());
    ContentBlake3::from_domain(ENTRY_CONTENT_DOMAIN, &buf)
}

/// Compute the storage hash of an [`Entry`] — a **position-independent**
/// content hash fit to be a storage identity.
///
/// This is [`entry_content_hash`]'s preimage with exactly one thing removed:
/// `sym.source` and `sym.span` (the declaration's file and byte offsets). Every
/// other field — name, visibility, documentation, aliases, deprecation,
/// doc-links, attrs, cfg, and the full kind body — is encoded identically, via
/// the same shared encoders, so the two hashes cannot silently drift apart as
/// fields are added to `Symbol` or any `Kind` variant in the future.
///
/// # Why this exists (`docs/IR-STORAGE-PLAN.md` §3a.1)
///
/// A declaration's byte span shifts whenever code earlier in the same file is
/// edited, even though the declaration's own text is untouched. Measured on a
/// real adjacent-version pair (testify v1.9.0 → v1.11.1): `entry_content_hash`
/// reports 75.6% of entries "modified"; the same hash with `source`/`span`
/// excluded reports 1.3%. A storage layer that deduplicates on the
/// position-sensitive hash would re-store 58× more than it needs to, entirely
/// from code moving rather than changing.
///
/// # Where position goes instead
///
/// Excluding position here does not throw the information away — a consumer
/// still needs to know where a symbol currently lives. That data is
/// **generation-scoped**, not content-scoped: it belongs on the (planned,
/// not-yet-built) `GenerationRoot`, which is rewritten every generation
/// regardless, rather than inside this content-addressed payload, which is
/// exactly what must stay stable when a declaration merely moves.
///
/// # Domain separation
///
/// This hash uses [`ENTRY_STORAGE_DOMAIN`], a sibling of
/// [`ENTRY_CONTENT_DOMAIN`], never the same constant.
/// `ContentBlake3::from_domain` folds the domain string into the BLAKE3
/// preimage ahead of the encoded bytes, so distinct domain strings guarantee
/// the two hashes never coincide — including on an entry whose position fields
/// are already empty, where the two encoders would otherwise diverge only in
/// the domain tag.
///
/// # Panics (debug)
///
/// Same as [`entry_content_hash`]: a `debug_assert!` fires on an unlowered
/// `Ref::Local`, since both hashes share the same `Kind`/`Ref` encoders.
pub fn entry_storage_hash(entry: &Entry) -> ContentBlake3 {
    let mut hasher = blake3::Hasher::new();
    {
        let mut out = HasherSink::new(&mut hasher);
        out.extend_from_slice(ENTRY_STORAGE_DOMAIN.as_bytes());
        encode_symbol(&mut out, entry.sym(), SymbolPosition::Excluded);
        encode_entry_inner(&mut out, entry.kind());
    }
    ContentBlake3::from_raw(*hasher.finalize().as_bytes())
}

/// The exact byte string [`entry_storage_hash`] digests — and therefore the
/// bytes that should actually be **stored** for this entry.
///
/// # Why this is public, and why it is the payload
///
/// `entry_storage_hash` names a content-addressed object; that object's bytes
/// have to be *something*, and the only choice that keeps a content-addressed
/// store honest is the hash's own preimage. Then
/// `blake3(entry_storage_payload(e)) == entry_storage_hash(e)` by construction,
/// so an ordinary CAS integrity check — "do these bytes hash to the key they
/// were filed under?" — just works, with no per-payload special case and no
/// storage-layer knowledge of how IR is encoded.
///
/// The domain tag is folded into the returned bytes rather than prepended by
/// [`ContentBlake3::from_domain`], which is exactly equivalent
/// (`hash_domain(domain, preimage) = blake3(domain ‖ preimage)`,
/// `change/hash.rs:14-19`) while leaving the digest a plain, unqualified
/// `blake3` of the stored bytes.
///
/// # The error class this closes
///
/// P3's first implementation stored a *different* encoding (a full serde
/// `Entry`, position included) under this hash. Because the hash deliberately
/// excludes `sym.source`/`sym.span` (task #17 — including them inflated
/// apparent churn 58× on a real package pair), a declaration that merely
/// **moved** produced the same key with different bytes. The CAS integrity
/// check correctly refused it, and the first response was to route those
/// sections through an unverified namespace — trading a real content-addressing
/// violation for a silent one, and blinding `verify_blobs`' corruption audit
/// for the newest section class in the system.
///
/// Storing the preimage removes the contradiction at its source: position is
/// simply not in these bytes. Location is generation-scoped and lives in
/// `GenerationRoot`'s `RootEntry` (`source`/`span`), which is rewritten every
/// generation anyway — the payload dedups, the root carries where each entry
/// was that time. See `ir/model/tests/storage_hash.rs`.
pub fn entry_storage_payload(entry: &Entry) -> Vec<u8> {
    let mut buf = Vec::from(ENTRY_STORAGE_DOMAIN.as_bytes());
    encode_symbol(&mut buf, entry.sym(), SymbolPosition::Excluded);
    encode_entry_inner(&mut buf, entry.kind());
    buf
}

// ---------------------------------------------------------------------------
// Symbol encoding
// ---------------------------------------------------------------------------

/// Whether [`encode_symbol`] should fold `sym.source`/`sym.span` into the
/// preimage.
///
/// A `bool` would work but reads as noise at the call site (`encode_symbol(out,
/// sym, true)` — included what?). This exists so [`entry_content_hash`] and
/// [`entry_storage_hash`] can share one encoder — see the module
/// documentation's "Storage identity" section for why a second, drifting copy
/// of this function is the outcome to avoid: the next field added to `Symbol`
/// should only need to be decided about once, not once per hash.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SymbolPosition {
    /// Fold `sym.source` and `sym.span` into the preimage. Used by
    /// [`entry_content_hash`].
    Included,
    /// Omit `sym.source` and `sym.span` entirely — not zeroed, not
    /// length-prefixed-empty, simply absent from the preimage. Used by
    /// [`entry_storage_hash`].
    Excluded,
}

fn encode_symbol(out: &mut impl ByteSink, sym: &Symbol, position: SymbolPosition) {
    encode_str(out, &sym.name);
    encode_visibility(out, &sym.visibility);
    encode_str(out, &sym.documentation);
    if position == SymbolPosition::Included {
        // source path — encode as a string via to_string_lossy so it is
        // platform-independent in UTF-8; lossiness is acceptable because the
        // path is producer-reported metadata, not a cryptographic identifier.
        encode_str(out, &sym.source.to_string_lossy());
        // span: start and end as u64le (widened from usize for cross-platform
        // determinism).
        write_u64le(out, sym.span.start as u64);
        write_u64le(out, sym.span.end as u64);
    }
    // aliases — deterministic order (declaration order from producer)
    encode_str_seq(out, &sym.aliases);
    // deprecation
    encode_opt_deprecation(out, sym.deprecation.as_ref());
    // doc_links
    write_u32le(out, sym.doc_links.len() as u32);
    for dl in &sym.doc_links {
        encode_doc_link(out, dl);
    }
    // attrs
    write_u32le(out, sym.attrs.len() as u32);
    for attr in &sym.attrs {
        encode_attr_tok(out, attr);
    }
    // cfg
    encode_opt_cfg(out, sym.cfg.as_ref());
}

fn encode_visibility(out: &mut impl ByteSink, vis: &Visibility) {
    // No _ wildcard — every new Visibility variant must get an opcode here.
    match vis {
        Visibility::Public => out.push(0x01),
        Visibility::Private => out.push(0x02),
        Visibility::Protected => out.push(0x03),
        Visibility::Internal => out.push(0x04),
        Visibility::Package => out.push(0x05),
        Visibility::Crate => out.push(0x06),
    }
}

fn encode_opt_deprecation(out: &mut impl ByteSink, dep: Option<&Deprecation>) {
    match dep {
        None => out.push(0x00),
        Some(d) => {
            out.push(0x01);
            encode_opt_str(out, d.note.as_deref());
            encode_opt_str(out, d.since.as_deref());
        }
    }
}

fn encode_doc_link(out: &mut impl ByteSink, dl: &DocLink) {
    encode_str(out, &dl.target);
    encode_opt_str(out, dl.label.as_deref());
    match &dl.source_span {
        None => out.push(0x00),
        Some(span) => {
            out.push(0x01);
            write_u64le(out, span.start as u64);
            write_u64le(out, span.end as u64);
        }
    }
}

fn encode_attr_tok(out: &mut impl ByteSink, attr: &AttrTok) {
    encode_str(out, &attr.token);
    encode_opt_str(out, attr.arg.as_deref());
}

fn encode_opt_cfg(out: &mut impl ByteSink, cfg: Option<&CfgExpr>) {
    match cfg {
        None => out.push(0x00),
        Some(c) => {
            out.push(0x01);
            encode_cfg_expr(out, c);
        }
    }
}

fn encode_cfg_expr(out: &mut impl ByteSink, cfg: &CfgExpr) {
    // No _ wildcard — every new CfgExpr variant must get an opcode here.
    match cfg {
        CfgExpr::All(inner) => {
            out.push(0x01);
            write_u32le(out, inner.len() as u32);
            for c in inner {
                encode_cfg_expr(out, c);
            }
        }
        CfgExpr::Any(inner) => {
            out.push(0x02);
            write_u32le(out, inner.len() as u32);
            for c in inner {
                encode_cfg_expr(out, c);
            }
        }
        CfgExpr::Not(inner) => {
            out.push(0x03);
            encode_cfg_expr(out, inner);
        }
        CfgExpr::Feature(s) => {
            out.push(0x04);
            encode_str(out, s);
        }
        CfgExpr::TargetOs(s) => {
            out.push(0x05);
            encode_str(out, s);
        }
        CfgExpr::TargetArch(s) => {
            out.push(0x06);
            encode_str(out, s);
        }
        CfgExpr::Other(s) => {
            out.push(0x07);
            encode_str(out, s);
        }
    }
}

// ---------------------------------------------------------------------------
// EntryInner encoding
// ---------------------------------------------------------------------------

fn encode_entry_inner(out: &mut impl ByteSink, inner: &EntryInner) {
    // No _ wildcard — every new EntryInner variant must get an opcode here.
    match inner {
        EntryInner::Owned(kind) => {
            out.push(0x01);
            encode_kind(out, kind);
        }
        EntryInner::Reference(r) => {
            out.push(0x02);
            encode_ref(out, r);
        }
    }
}

fn encode_kind(out: &mut impl ByteSink, kind: &Kind) {
    // The opcode is the wire discriminant from register_kinds! (u16le).
    // No _ wildcard — every new Kind variant must get a discriminant and an
    // encoder here.
    match kind {
        Kind::Module(m) => {
            write_u16le(out, 1);
            encode_module(out, m);
        }
        Kind::Record(r) => {
            write_u16le(out, 2);
            encode_record(out, r);
        }
        Kind::Field(f) => {
            write_u16le(out, 3);
            encode_field(out, f);
        }
        Kind::Function(f) => {
            write_u16le(out, 4);
            encode_function(out, f);
        }
        Kind::Alias(a) => {
            write_u16le(out, 5);
            encode_alias(out, a);
        }
        Kind::Trait(t) => {
            write_u16le(out, 6);
            encode_trait(out, t);
        }
        Kind::Impl(i) => {
            write_u16le(out, 7);
            encode_impl(out, i);
        }
        Kind::Enum(e) => {
            write_u16le(out, 8);
            encode_enum(out, e);
        }
        Kind::Variant(v) => {
            write_u16le(out, 9);
            encode_variant(out, v);
        }
        Kind::Const(c) => {
            write_u16le(out, 10);
            encode_const(out, c);
        }
        Kind::Static(s) => {
            write_u16le(out, 11);
            encode_static(out, s);
        }
        Kind::Reexport(rx) => {
            write_u16le(out, 12);
            encode_reexport(out, rx);
        }
        Kind::Param(p) => {
            write_u16le(out, 13);
            encode_param(out, p);
        }
    }
}

// ---------------------------------------------------------------------------
// Per-kind encoders
// ---------------------------------------------------------------------------

fn encode_module(_out: &mut impl ByteSink, _m: &Module) {
    // Module is a unit struct — no fields to encode. The opcode above
    // (u16le = 1) already distinguishes it.
}

fn encode_record(out: &mut impl ByteSink, r: &Record) {
    encode_record_form(out, &r.form);
    // fields: refs to child Field entries — encode by ref so the hash
    // captures the identity of the referenced field declarations.
    write_u32le(out, r.fields.len() as u32);
    for rf in &r.fields {
        encode_ref(out, &rf.clone().into_raw());
    }
    // super_types
    encode_type_seq(out, &r.super_types);
    encode_generic_params(out, &r.generics);
    encode_where_preds(out, &r.wheres);
    encode_auto_facts(out, &r.auto);
}

fn encode_record_form(out: &mut impl ByteSink, form: &RecordForm) {
    // No _ wildcard.
    match form {
        RecordForm::Struct => out.push(0x01),
        RecordForm::Tuple => out.push(0x02),
        RecordForm::Unit => out.push(0x03),
        RecordForm::Union => out.push(0x04),
    }
}

fn encode_field(out: &mut impl ByteSink, f: &Field) {
    encode_field_key(out, &f.key);
    encode_opt_type(out, f.ty.as_ref());
    // attributes
    write_u32le(out, f.attributes.len() as u32);
    for attr in &f.attributes {
        encode_field_attribute(out, attr);
    }
}

fn encode_field_key(out: &mut impl ByteSink, key: &FieldKey) {
    // No _ wildcard.
    match key {
        FieldKey::Named => out.push(0x01),
        FieldKey::Positional(n) => {
            out.push(0x02);
            write_u64le(out, *n as u64);
        }
    }
}

fn encode_field_attribute(out: &mut impl ByteSink, attr: &FieldAttribute) {
    // No _ wildcard.
    match attr {
        FieldAttribute::Mutable => out.push(0x01),
        FieldAttribute::Optional => out.push(0x02),
        FieldAttribute::Static => out.push(0x03),
    }
}

fn encode_function(out: &mut impl ByteSink, f: &Function) {
    // receiver
    match &f.receiver {
        None => out.push(0x00),
        Some(r) => {
            out.push(0x01);
            encode_receiver(out, r);
        }
    }
    // input_params: typed refs to Param entries
    write_u32le(out, f.input_params.len() as u32);
    for rf in &f.input_params {
        encode_ref(out, &rf.clone().into_raw());
    }
    // output_params
    write_u32le(out, f.output_params.len() as u32);
    for rf in &f.output_params {
        encode_ref(out, &rf.clone().into_raw());
    }
    // modifiers
    write_u32le(out, f.modifiers.len() as u32);
    for m in &f.modifiers {
        encode_fn_modifier(out, m);
    }
    encode_generic_params(out, &f.generics);
    encode_where_preds(out, &f.wheres);
    // abi
    encode_opt_str(out, f.abi.as_deref());
    // is_defaulted
    out.push(u8::from(f.is_defaulted));
    // throws: checked exception types (Java / C#)
    encode_type_seq(out, &f.throws);
}

fn encode_receiver(out: &mut impl ByteSink, r: &Receiver) {
    // No _ wildcard.
    match r {
        Receiver::Owned => out.push(0x01),
        Receiver::SharedRef => out.push(0x02),
        Receiver::MutRef => out.push(0x03),
        Receiver::Arbitrary => out.push(0x04),
    }
}

fn encode_fn_modifier(out: &mut impl ByteSink, m: &FnModifier) {
    // No _ wildcard.
    match m {
        FnModifier::Async => out.push(0x01),
        FnModifier::Const => out.push(0x02),
        FnModifier::Unsafe => out.push(0x03),
        FnModifier::Pure => out.push(0x04),
        FnModifier::Generator => out.push(0x05),
    }
}

fn encode_alias(out: &mut impl ByteSink, a: &Alias) {
    encode_opt_type(out, a.target.as_ref());
    encode_generic_params(out, &a.generics);
    encode_where_preds(out, &a.wheres);
    encode_type_seq(out, &a.bounds);
    encode_auto_facts(out, &a.auto);
}

fn encode_trait(out: &mut impl ByteSink, t: &Trait) {
    encode_trait_flags(out, &t.flags);
    encode_type_seq(out, &t.supers);
    encode_generic_params(out, &t.generics);
    encode_where_preds(out, &t.wheres);
}

fn encode_trait_flags(out: &mut impl ByteSink, f: &TraitFlags) {
    out.push(u8::from(f.is_unsafe));
    out.push(u8::from(f.is_auto));
    encode_tristate(out, &f.dyn_compat);
    encode_sealed(out, &f.sealed);
}

fn encode_tristate(out: &mut impl ByteSink, t: &TriState) {
    // No _ wildcard.
    match t {
        TriState::Yes => out.push(0x01),
        TriState::No => out.push(0x02),
        TriState::Unknown => out.push(0x03),
    }
}

fn encode_sealed(out: &mut impl ByteSink, s: &Sealed) {
    // No _ wildcard.
    match s {
        Sealed::None => out.push(0x01),
        Sealed::PubApi => out.push(0x02),
        Sealed::Full => out.push(0x03),
    }
}

fn encode_impl(out: &mut impl ByteSink, i: &Impl) {
    encode_impl_flags(out, &i.flags);
    encode_opt_type(out, i.of.as_ref());
    encode_type(out, &i.self_ty);
    encode_generic_params(out, &i.generics);
    encode_where_preds(out, &i.wheres);
}

fn encode_impl_flags(out: &mut impl ByteSink, f: &ImplFlags) {
    out.push(u8::from(f.negative));
    out.push(u8::from(f.blanket));
}

fn encode_enum(out: &mut impl ByteSink, e: &Enum) {
    // variants: typed refs to Variant entries
    write_u32le(out, e.variants.len() as u32);
    for rf in &e.variants {
        encode_ref(out, &rf.clone().into_raw());
    }
    encode_generic_params(out, &e.generics);
    encode_where_preds(out, &e.wheres);
    encode_auto_facts(out, &e.auto);
}

fn encode_variant(out: &mut impl ByteSink, v: &Variant) {
    encode_variant_form(out, &v.form);
    // fields: typed refs to Field entries
    write_u32le(out, v.fields.len() as u32);
    for rf in &v.fields {
        encode_ref(out, &rf.clone().into_raw());
    }
    encode_opt_str(out, v.discr.as_deref());
}

fn encode_variant_form(out: &mut impl ByteSink, form: &VariantForm) {
    // No _ wildcard.
    match form {
        VariantForm::Unit => out.push(0x01),
        VariantForm::Tuple => out.push(0x02),
        VariantForm::Struct => out.push(0x03),
    }
}

fn encode_const(out: &mut impl ByteSink, c: &Const) {
    encode_type(out, &c.ty);
    encode_opt_const_expr(out, c.value.as_ref());
}

fn encode_static(out: &mut impl ByteSink, s: &Static) {
    encode_type(out, &s.ty);
    out.push(u8::from(s.mutable));
}

fn encode_reexport(_out: &mut impl ByteSink, _rx: &Reexport) {
    // Reexport is a unit struct. The target lives in EntryInner::Reference
    // (already encoded by encode_entry_inner). No fields here.
}

fn encode_param(out: &mut impl ByteSink, p: &Param) {
    encode_opt_type(out, p.ty.as_ref());
    encode_opt_const_expr(out, p.default_value.as_ref());
    write_u32le(out, p.attributes.len() as u32);
    for attr in &p.attributes {
        encode_param_attribute(out, attr);
    }
}

fn encode_opt_const_expr(out: &mut impl ByteSink, expr: Option<&ConstExpr>) {
    match expr {
        None => out.push(0x00),
        Some(expr) => {
            out.push(0x01);
            encode_type(out, &expr.ty);
            write_u32le(out, expr.tokens.len() as u32);
            for token in &expr.tokens {
                match token {
                    ConstToken::Identifier(value) => {
                        out.push(0x01);
                        encode_str(out, value);
                    }
                    ConstToken::Number(value) => {
                        out.push(0x02);
                        encode_str(out, value);
                    }
                    ConstToken::String(value) => {
                        out.push(0x03);
                        encode_str(out, value);
                    }
                    ConstToken::Character(value) => {
                        out.push(0x04);
                        encode_str(out, value);
                    }
                    ConstToken::Punctuation(value) => {
                        out.push(0x05);
                        encode_str(out, value);
                    }
                }
            }
            encode_str(out, &expr.source);
        }
    }
}

fn encode_param_attribute(out: &mut impl ByteSink, attr: &ParamAttribute) {
    // No _ wildcard.
    match attr {
        ParamAttribute::Inout => out.push(0x01),
        ParamAttribute::Mutable => out.push(0x02),
        ParamAttribute::Consuming => out.push(0x03),
        ParamAttribute::Borrowing => out.push(0x04),
        ParamAttribute::Isolated => out.push(0x05),
        ParamAttribute::Variadic => out.push(0x06),
        ParamAttribute::Optional => out.push(0x07),
        ParamAttribute::KeywordOnly => out.push(0x08),
        ParamAttribute::Kwargs => out.push(0x09),
        ParamAttribute::Out => out.push(0x0a),
    }
}

// ---------------------------------------------------------------------------
// Generics / where-clauses
// ---------------------------------------------------------------------------

fn encode_generic_params(out: &mut impl ByteSink, params: &[GenericParam]) {
    write_u32le(out, params.len() as u32);
    for p in params {
        encode_generic_param(out, p);
    }
}

fn encode_generic_param(out: &mut impl ByteSink, p: &GenericParam) {
    // Unlike skeleton.rs, we DO encode generic-parameter names — this is a
    // content hash, not a skeleton. `fn<T>(…)` and `fn<U>(…)` are the same
    // declaration (alpha-equivalent), but a content hash records what the
    // producer emitted verbatim. If the producer renames `T` to `U`, the
    // hash changes, which is correct: the source changed.
    //
    // No _ wildcard.
    match p {
        GenericParam::Lifetime { name } => {
            out.push(0x01);
            encode_str(out, name);
        }
        GenericParam::Type {
            name,
            bounds,
            default,
            variance,
        } => {
            out.push(0x02);
            encode_str(out, name);
            encode_type_seq(out, bounds);
            encode_opt_type(out, default.as_ref());
            // Variance: 0x00 = None; otherwise 0x01 + variance opcode.
            match variance {
                None => out.push(0x00),
                Some(v) => {
                    out.push(0x01);
                    encode_variance(out, v);
                }
            }
        }
        GenericParam::Const { name, ty } => {
            out.push(0x03);
            encode_str(out, name);
            encode_type(out, ty);
        }
    }
}

fn encode_where_preds(out: &mut impl ByteSink, preds: &[WherePred]) {
    write_u32le(out, preds.len() as u32);
    for pred in preds {
        encode_type(out, &pred.target);
        encode_type_seq(out, &pred.bounds);
    }
}

// ---------------------------------------------------------------------------
// Type encoding
// ---------------------------------------------------------------------------

fn encode_type_seq(out: &mut impl ByteSink, types: &[Type]) {
    write_u32le(out, types.len() as u32);
    for t in types {
        encode_type(out, t);
    }
}

fn encode_opt_type(out: &mut impl ByteSink, ty: Option<&Type>) {
    match ty {
        None => out.push(0x00),
        Some(t) => {
            out.push(0x01);
            encode_type(out, t);
        }
    }
}

fn encode_type(out: &mut impl ByteSink, ty: &Type) {
    // No _ wildcard — every new Type variant must get an opcode here.
    match ty {
        Type::SelfType => out.push(0x01),
        Type::Primitive(p) => {
            out.push(0x02);
            encode_primitive(out, p);
        }
        Type::Tuple(elems) => {
            out.push(0x03);
            encode_tuple_elements(out, elems);
        }
        Type::Slice(t) => {
            out.push(0x04);
            encode_type(out, t);
        }
        Type::Array { ty, length } => {
            out.push(0x05);
            encode_type(out, ty);
            write_u64le(out, *length as u64);
        }
        Type::Union(ts) => {
            out.push(0x06);
            encode_type_seq(out, ts);
        }
        Type::Intersection(ts) => {
            out.push(0x07);
            encode_type_seq(out, ts);
        }
        Type::Never => out.push(0x08),
        Type::Any => out.push(0x09),
        // The content hash is *total*: both the reason and any name it carries
        // are part of what the producer emitted, so a producer that goes from
        // "unresolved external `Foo`" to "unresolved external `Bar`" — or from
        // unannotated to explicitly dynamic — is a real content change.
        Type::Unknown(reason) => {
            out.push(0x18);
            encode_unknown(out, reason);
        }
        Type::Nominal(r) => {
            out.push(0x0a);
            encode_ref(out, r);
        }
        Type::Apply { base, args } => {
            out.push(0x0b);
            encode_type(out, base);
            encode_type_seq(out, args);
        }
        // Unlike the identity skeleton, the content hash is *total*: the type
        // variable's name is part of what the producer emitted, so renaming it
        // is a real content change even though it is identity-preserving.
        Type::TypeVar(name) => {
            out.push(0x0c);
            encode_str(out, name);
        }
        Type::Wildcard { variance, bound } => {
            out.push(0x0d);
            encode_variance(out, variance);
            match bound {
                Some(t) => {
                    out.push(0x01);
                    encode_type(out, t);
                }
                None => out.push(0x00),
            }
        }
        Type::FunctionPointer { params, ret, abi } => {
            out.push(0x0e);
            encode_type_seq(out, params);
            encode_opt_type(out, ret.as_deref());
            encode_opt_str(out, abi.as_deref());
        }
        Type::Annotated { inner, annotation } => {
            out.push(0x0f);
            encode_type(out, inner);
            encode_attr_tok(out, annotation);
        }
        Type::Conditional {
            check,
            extends_ty,
            then_ty,
            else_ty,
        } => {
            out.push(0x10);
            encode_type(out, check);
            encode_type(out, extends_ty);
            encode_type(out, then_ty);
            encode_type(out, else_ty);
        }
        // Content hash includes key_var verbatim (unlike the skeleton, which
        // excludes it as alpha-equivalent).
        Type::Mapped {
            key_var,
            source,
            value,
            readonly,
            optional,
        } => {
            out.push(0x11);
            encode_str(out, key_var);
            encode_type(out, source);
            encode_type(out, value);
            encode_mapped_modifier(out, readonly);
            encode_mapped_modifier(out, optional);
        }
        Type::TemplateLiteral(parts) => {
            out.push(0x12);
            write_u32le(out, parts.len() as u32);
            for part in parts {
                match part {
                    TemplatePart::Literal(s) => {
                        out.push(0x01);
                        encode_str(out, s);
                    }
                    TemplatePart::Interpolated(t) => {
                        out.push(0x02);
                        encode_type(out, t);
                    }
                }
            }
        }
        Type::AnonymousRecord { form, members } => {
            out.push(0x13);
            encode_anon_record_form(out, form);
            write_u32le(out, members.len() as u32);
            for m in members {
                encode_str(out, &m.name);
                encode_type(out, &m.ty);
                out.push(u8::from(m.optional));
                out.push(u8::from(m.readonly));
            }
        }
        Type::ImplTrait(bounds) => {
            out.push(0x14);
            encode_type_seq(out, bounds);
        }
        Type::DynTrait(bounds) => {
            out.push(0x15);
            encode_type_seq(out, bounds);
        }
        Type::Inferred => out.push(0x16),
        Type::QualifiedPath {
            self_ty,
            trait_ref,
            assoc,
        } => {
            out.push(0x17);
            encode_type(out, self_ty);
            match trait_ref {
                Some(t) => {
                    out.push(0x01);
                    encode_type(out, t);
                }
                None => out.push(0x00),
            }
            encode_str(out, assoc);
        }
    }
}

/// Encode an [`UnknownType`] reason. No `_` wildcard — every new reason must
/// get an opcode here, or two different gaps hash to the same content.
fn encode_unknown(out: &mut impl ByteSink, r: &UnknownType) {
    match r {
        UnknownType::Unannotated => out.push(0x01),
        UnknownType::DynamicallyTyped => out.push(0x02),
        UnknownType::UnresolvedLocalName { name } => {
            out.push(0x03);
            encode_str(out, name);
        }
        UnknownType::UnresolvedExternal { name } => {
            out.push(0x04);
            encode_str(out, name);
        }
        UnknownType::TruncatedAtDepthLimit => out.push(0x05),
        UnknownType::OracleGap => out.push(0x06),
        UnknownType::NoIrRepresentation { construct } => {
            out.push(0x07);
            encode_str(out, construct);
        }
    }
}

fn encode_tuple_elements(out: &mut impl ByteSink, elems: &[TupleElement]) {
    write_u32le(out, elems.len() as u32);
    for elem in elems {
        // No _ wildcard.
        match elem {
            TupleElement::Positional(t) => {
                out.push(0x01);
                encode_type(out, t);
            }
            TupleElement::Named { label, ty } => {
                out.push(0x02);
                encode_str(out, label);
                encode_type(out, ty);
            }
        }
    }
}

fn encode_variance(out: &mut impl ByteSink, v: &Variance) {
    // No _ wildcard.
    out.push(match v {
        Variance::Invariant => 0x01,
        Variance::Covariant => 0x02,
        Variance::Contravariant => 0x03,
    });
}

fn encode_mapped_modifier(out: &mut impl ByteSink, m: &MappedModifier) {
    // No _ wildcard.
    out.push(match m {
        MappedModifier::Add => 0x01,
        MappedModifier::Remove => 0x02,
        MappedModifier::Absent => 0x03,
    });
}

fn encode_anon_record_form(out: &mut impl ByteSink, f: &AnonRecordForm) {
    // No _ wildcard.
    out.push(match f {
        AnonRecordForm::Struct => 0x01,
        AnonRecordForm::Interface => 0x02,
    });
}

fn encode_primitive(out: &mut impl ByteSink, p: &Primitive) {
    // No _ wildcard.
    match p {
        Primitive::Integer { signed, width } => {
            out.push(0x01);
            out.push(u8::from(*signed));
            encode_width(out, width);
        }
        Primitive::Float(w) => {
            out.push(0x02);
            encode_width(out, w);
        }
        Primitive::Bool => out.push(0x03),
        Primitive::Char => out.push(0x04),
        Primitive::Str => out.push(0x05),
        Primitive::MutPointer(t) => {
            out.push(0x06);
            encode_type(out, t);
        }
        Primitive::ConstPointer(t) => {
            out.push(0x07);
            encode_type(out, t);
        }
        Primitive::Reference {
            lifetime,
            mutable,
            ty,
        } => {
            out.push(0x08);
            // Include the lifetime name — unlike skeleton.rs this IS a content
            // hash, not a structural fingerprint. `&'a T` and `&'b T` differ
            // in source even if they are semantically alpha-equivalent.
            encode_opt_str(out, lifetime.as_deref());
            out.push(u8::from(*mutable));
            encode_type(out, ty);
        }
        Primitive::Builtin(s) => {
            out.push(0x09);
            encode_str(out, s);
        }
    }
}

fn encode_width(out: &mut impl ByteSink, w: &Width) {
    // No _ wildcard.
    match w {
        Width::Fixed(n) => {
            out.push(0x01);
            write_u16le(out, n.get());
        }
        Width::Arch => out.push(0x02),
    }
}

// ---------------------------------------------------------------------------
// Ref encoding
// ---------------------------------------------------------------------------

/// Encode a [`RawRef`] into the preimage buffer.
///
/// Opcodes:
/// - `0x00` — `Ref::Local` (MUST NOT appear post-seal; debug_assert fires).
/// - `0x01` + 32 bytes — `Ref::Intro`.
/// - `0x02` + canonical bytes — `Ref::Foreign`.
///
/// A `Ref::Local` here means the seal pass has a bug: it failed to lower this
/// local reference before content hashing was called. The function is written
/// to be total (never panics in release) so that content hashing can still
/// proceed on partially-sealed arenas during development/testing, but the
/// debug_assert ensures the bug is caught in tests.
fn encode_ref(out: &mut impl ByteSink, r: &RawRef) {
    // No _ wildcard.
    match r {
        Ref::Local(_) => {
            // A Ref::Local must not survive to content-hash time.
            // This fires in debug/test builds to surface seal bugs early.
            debug_assert!(
                false,
                "Ref::Local encountered in entry_content_hash — \
                 the seal pass has a bug: it failed to lower this local \
                 reference before content hashing was invoked"
            );
            out.push(0x00);
        }
        Ref::Intro(id) => {
            out.push(0x01);
            out.extend_from_slice(id.as_bytes());
        }
        // The cross-package KEY, never the resolved target.
        //
        // `entry_content_hash` is folded over every entry by
        // `manifest::generation_stamp`. Hashing the target instead would make a
        // package's generation stamp a function of which *other* packages
        // happened to be loaded — so the change plane would see a phantom
        // generation every time the corpus warmed up, in a direction that
        // depends on load order.
        Ref::Foreign { key, .. } => {
            out.push(0x02);
            out.extend_from_slice(&key.canonical_bytes());
        }
    }
}

// ---------------------------------------------------------------------------
// Auto-fact encoding
// ---------------------------------------------------------------------------

fn encode_auto_facts(out: &mut impl ByteSink, facts: &[AutoFact]) {
    write_u32le(out, facts.len() as u32);
    for f in facts {
        encode_auto_trait(out, &f.trait_);
        encode_auto_state(out, &f.state);
    }
}

fn encode_auto_trait(out: &mut impl ByteSink, t: &AutoTrait) {
    // No _ wildcard.
    match t {
        AutoTrait::Send => out.push(0x01),
        AutoTrait::Sync => out.push(0x02),
        AutoTrait::Unpin => out.push(0x03),
        AutoTrait::UnwindSafe => out.push(0x04),
        AutoTrait::RefUnwindSafe => out.push(0x05),
    }
}

fn encode_auto_state(out: &mut impl ByteSink, s: &AutoState) {
    // No _ wildcard.
    match s {
        AutoState::Yes => out.push(0x01),
        AutoState::No => out.push(0x02),
        AutoState::Cond => out.push(0x03),
    }
}

// ---------------------------------------------------------------------------
// Primitive helpers
// ---------------------------------------------------------------------------

fn encode_opt_str(out: &mut impl ByteSink, s: Option<&str>) {
    match s {
        None => out.push(0x00),
        Some(v) => {
            out.push(0x01);
            encode_str(out, v);
        }
    }
}

fn encode_str_seq(out: &mut impl ByteSink, ss: &[String]) {
    write_u32le(out, ss.len() as u32);
    for s in ss {
        encode_str(out, s);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
