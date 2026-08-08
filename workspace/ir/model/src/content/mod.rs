//! Entry content-hash — the *third* identity layer.
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
        encode::{encode_str, write_u16le, write_u32le, write_u64le},
    },
    entry::{AttrTok, CfgExpr, Deprecation, DocLink, Entry, EntryInner, Symbol, Visibility},
    index::{RawRef, Ref},
    kind::Kind,
    kinds::{
        Alias, AutoFact, AutoState, AutoTrait, Const, Enum, Field, FieldAttribute, FieldKey,
        FnModifier, Function, GenericParam, Impl, ImplFlags, Module, Param, ParamAttribute,
        Receiver, Record, RecordForm, Reexport, Sealed, Static, Trait, TraitFlags, TriState,
        Variant, VariantForm, WherePred,
        ty::{
            AnonRecordForm, MappedModifier, Primitive, TemplatePart, TupleElement, Type, Variance,
            Width,
        },
    },
};

/// Domain tag for entry content-hash preimages.
///
/// `v3` matches the `IntroId` format version so that the two hashing planes
/// are clearly versioned together. A format-breaking change to either plane
/// must bump both domains.
pub const ENTRY_CONTENT_DOMAIN: &str = "nudox.entry.v3";

/// Compute the content hash of an [`Entry`].
///
/// The hash is a domain-separated BLAKE3 digest over the entry's [`Symbol`]
/// and [`EntryInner`] in full. The entry's own `IntroId` and its `Node` tree
/// edges (parent + children) are **excluded**; see the module documentation
/// for the rationale.
///
/// # Panics (debug)
///
/// A `debug_assert!` fires if any `Ref::Local` is encountered in the entry.
/// `Ref::Local` must have been lowered to `Ref::Intro` or `Ref::Foreign` by
/// the seal pass before content hashing. A `Local` here indicates a bug in the
/// seal pass.
pub fn entry_content_hash(entry: &Entry) -> ContentBlake3 {
    let mut buf = Vec::new();
    encode_symbol(&mut buf, entry.sym());
    encode_entry_inner(&mut buf, entry.kind());
    ContentBlake3::from_domain(ENTRY_CONTENT_DOMAIN, &buf)
}

// ---------------------------------------------------------------------------
// Symbol encoding
// ---------------------------------------------------------------------------

fn encode_symbol(out: &mut Vec<u8>, sym: &Symbol) {
    encode_str(out, &sym.name);
    encode_visibility(out, &sym.visibility);
    encode_str(out, &sym.documentation);
    // source path — encode as a string via to_string_lossy so it is
    // platform-independent in UTF-8; lossiness is acceptable because the path
    // is producer-reported metadata, not a cryptographic identifier.
    encode_str(out, &sym.source.to_string_lossy());
    // span: start and end as u64le (widened from usize for cross-platform
    // determinism).
    write_u64le(out, sym.span.start as u64);
    write_u64le(out, sym.span.end as u64);
    // aliases — deterministic order (declaration order from producer)
    encode_str_seq(out, &sym.aliases);
    // deprecation
    encode_opt_deprecation(out, sym.deprecation.as_ref());
    // doc_links
    write_u32le(out, sym.doc_links.len() as u32);
    for dl in sym.doc_links.iter() {
        encode_doc_link(out, dl);
    }
    // attrs
    write_u32le(out, sym.attrs.len() as u32);
    for attr in sym.attrs.iter() {
        encode_attr_tok(out, attr);
    }
    // cfg
    encode_opt_cfg(out, sym.cfg.as_ref());
}

fn encode_visibility(out: &mut Vec<u8>, vis: &Visibility) {
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

fn encode_opt_deprecation(out: &mut Vec<u8>, dep: Option<&Deprecation>) {
    match dep {
        None => out.push(0x00),
        Some(d) => {
            out.push(0x01);
            encode_opt_str(out, d.note.as_deref());
            encode_opt_str(out, d.since.as_deref());
        }
    }
}

fn encode_doc_link(out: &mut Vec<u8>, dl: &DocLink) {
    encode_str(out, &dl.target);
    encode_opt_str(out, dl.label.as_deref());
}

fn encode_attr_tok(out: &mut Vec<u8>, attr: &AttrTok) {
    encode_str(out, &attr.token);
    encode_opt_str(out, attr.arg.as_deref());
}

fn encode_opt_cfg(out: &mut Vec<u8>, cfg: Option<&CfgExpr>) {
    match cfg {
        None => out.push(0x00),
        Some(c) => {
            out.push(0x01);
            encode_cfg_expr(out, c);
        }
    }
}

fn encode_cfg_expr(out: &mut Vec<u8>, cfg: &CfgExpr) {
    // No _ wildcard — every new CfgExpr variant must get an opcode here.
    match cfg {
        CfgExpr::All(inner) => {
            out.push(0x01);
            write_u32le(out, inner.len() as u32);
            for c in inner.iter() {
                encode_cfg_expr(out, c);
            }
        }
        CfgExpr::Any(inner) => {
            out.push(0x02);
            write_u32le(out, inner.len() as u32);
            for c in inner.iter() {
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

fn encode_entry_inner(out: &mut Vec<u8>, inner: &EntryInner) {
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

fn encode_kind(out: &mut Vec<u8>, kind: &Kind) {
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

fn encode_module(_out: &mut Vec<u8>, _m: &Module) {
    // Module is a unit struct — no fields to encode. The opcode above
    // (u16le = 1) already distinguishes it.
}

fn encode_record(out: &mut Vec<u8>, r: &Record) {
    encode_record_form(out, &r.form);
    // fields: refs to child Field entries — encode by ref so the hash
    // captures the identity of the referenced field declarations.
    write_u32le(out, r.fields.len() as u32);
    for rf in r.fields.iter() {
        encode_ref(out, &rf.clone().into_raw());
    }
    // super_types
    encode_type_seq(out, &r.super_types);
    encode_generic_params(out, &r.generics);
    encode_where_preds(out, &r.wheres);
    encode_auto_facts(out, &r.auto);
}

fn encode_record_form(out: &mut Vec<u8>, form: &RecordForm) {
    // No _ wildcard.
    match form {
        RecordForm::Struct => out.push(0x01),
        RecordForm::Tuple => out.push(0x02),
        RecordForm::Unit => out.push(0x03),
        RecordForm::Union => out.push(0x04),
    }
}

fn encode_field(out: &mut Vec<u8>, f: &Field) {
    encode_field_key(out, &f.key);
    encode_opt_type(out, f.ty.as_ref());
    // attributes
    write_u32le(out, f.attributes.len() as u32);
    for attr in f.attributes.iter() {
        encode_field_attribute(out, attr);
    }
}

fn encode_field_key(out: &mut Vec<u8>, key: &FieldKey) {
    // No _ wildcard.
    match key {
        FieldKey::Named => out.push(0x01),
        FieldKey::Positional(n) => {
            out.push(0x02);
            write_u64le(out, *n as u64);
        }
    }
}

fn encode_field_attribute(out: &mut Vec<u8>, attr: &FieldAttribute) {
    // No _ wildcard.
    match attr {
        FieldAttribute::Mutable => out.push(0x01),
        FieldAttribute::Optional => out.push(0x02),
        FieldAttribute::Static => out.push(0x03),
    }
}

fn encode_function(out: &mut Vec<u8>, f: &Function) {
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
    for rf in f.input_params.iter() {
        encode_ref(out, &rf.clone().into_raw());
    }
    // output_params
    write_u32le(out, f.output_params.len() as u32);
    for rf in f.output_params.iter() {
        encode_ref(out, &rf.clone().into_raw());
    }
    // modifiers
    write_u32le(out, f.modifiers.len() as u32);
    for m in f.modifiers.iter() {
        encode_fn_modifier(out, m);
    }
    encode_generic_params(out, &f.generics);
    encode_where_preds(out, &f.wheres);
    // abi
    encode_opt_str(out, f.abi.as_deref());
    // is_defaulted
    out.push(f.is_defaulted as u8);
    // throws: checked exception types (Java / C#)
    encode_type_seq(out, &f.throws);
}

fn encode_receiver(out: &mut Vec<u8>, r: &Receiver) {
    // No _ wildcard.
    match r {
        Receiver::Owned => out.push(0x01),
        Receiver::SharedRef => out.push(0x02),
        Receiver::MutRef => out.push(0x03),
        Receiver::Arbitrary => out.push(0x04),
    }
}

fn encode_fn_modifier(out: &mut Vec<u8>, m: &FnModifier) {
    // No _ wildcard.
    match m {
        FnModifier::Async => out.push(0x01),
        FnModifier::Const => out.push(0x02),
        FnModifier::Unsafe => out.push(0x03),
        FnModifier::Pure => out.push(0x04),
        FnModifier::Generator => out.push(0x05),
    }
}

fn encode_alias(out: &mut Vec<u8>, a: &Alias) {
    encode_opt_type(out, a.target.as_ref());
    encode_generic_params(out, &a.generics);
    encode_where_preds(out, &a.wheres);
    encode_type_seq(out, &a.bounds);
    encode_auto_facts(out, &a.auto);
}

fn encode_trait(out: &mut Vec<u8>, t: &Trait) {
    encode_trait_flags(out, &t.flags);
    encode_type_seq(out, &t.supers);
    encode_generic_params(out, &t.generics);
    encode_where_preds(out, &t.wheres);
}

fn encode_trait_flags(out: &mut Vec<u8>, f: &TraitFlags) {
    out.push(f.is_unsafe as u8);
    out.push(f.is_auto as u8);
    encode_tristate(out, &f.dyn_compat);
    encode_sealed(out, &f.sealed);
}

fn encode_tristate(out: &mut Vec<u8>, t: &TriState) {
    // No _ wildcard.
    match t {
        TriState::Yes => out.push(0x01),
        TriState::No => out.push(0x02),
        TriState::Unknown => out.push(0x03),
    }
}

fn encode_sealed(out: &mut Vec<u8>, s: &Sealed) {
    // No _ wildcard.
    match s {
        Sealed::None => out.push(0x01),
        Sealed::PubApi => out.push(0x02),
        Sealed::Full => out.push(0x03),
    }
}

fn encode_impl(out: &mut Vec<u8>, i: &Impl) {
    encode_impl_flags(out, &i.flags);
    encode_opt_type(out, i.of.as_ref());
    encode_type(out, &i.self_ty);
    encode_generic_params(out, &i.generics);
    encode_where_preds(out, &i.wheres);
}

fn encode_impl_flags(out: &mut Vec<u8>, f: &ImplFlags) {
    out.push(f.negative as u8);
    out.push(f.blanket as u8);
}

fn encode_enum(out: &mut Vec<u8>, e: &Enum) {
    // variants: typed refs to Variant entries
    write_u32le(out, e.variants.len() as u32);
    for rf in e.variants.iter() {
        encode_ref(out, &rf.clone().into_raw());
    }
    encode_generic_params(out, &e.generics);
    encode_where_preds(out, &e.wheres);
    encode_auto_facts(out, &e.auto);
}

fn encode_variant(out: &mut Vec<u8>, v: &Variant) {
    encode_variant_form(out, &v.form);
    // fields: typed refs to Field entries
    write_u32le(out, v.fields.len() as u32);
    for rf in v.fields.iter() {
        encode_ref(out, &rf.clone().into_raw());
    }
    encode_opt_str(out, v.discr.as_deref());
}

fn encode_variant_form(out: &mut Vec<u8>, form: &VariantForm) {
    // No _ wildcard.
    match form {
        VariantForm::Unit => out.push(0x01),
        VariantForm::Tuple => out.push(0x02),
        VariantForm::Struct => out.push(0x03),
    }
}

fn encode_const(out: &mut Vec<u8>, c: &Const) {
    encode_type(out, &c.ty);
    encode_opt_str(out, c.value.as_deref());
}

fn encode_static(out: &mut Vec<u8>, s: &Static) {
    encode_type(out, &s.ty);
    out.push(s.mutable as u8);
}

fn encode_reexport(_out: &mut Vec<u8>, _rx: &Reexport) {
    // Reexport is a unit struct. The target lives in EntryInner::Reference
    // (already encoded by encode_entry_inner). No fields here.
}

fn encode_param(out: &mut Vec<u8>, p: &Param) {
    encode_opt_type(out, p.ty.as_ref());
    write_u32le(out, p.attributes.len() as u32);
    for attr in p.attributes.iter() {
        encode_param_attribute(out, attr);
    }
}

fn encode_param_attribute(out: &mut Vec<u8>, attr: &ParamAttribute) {
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
    }
}

// ---------------------------------------------------------------------------
// Generics / where-clauses
// ---------------------------------------------------------------------------

fn encode_generic_params(out: &mut Vec<u8>, params: &[GenericParam]) {
    write_u32le(out, params.len() as u32);
    for p in params {
        encode_generic_param(out, p);
    }
}

fn encode_generic_param(out: &mut Vec<u8>, p: &GenericParam) {
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

fn encode_where_preds(out: &mut Vec<u8>, preds: &[WherePred]) {
    write_u32le(out, preds.len() as u32);
    for pred in preds {
        encode_type(out, &pred.target);
        encode_type_seq(out, &pred.bounds);
    }
}

// ---------------------------------------------------------------------------
// Type encoding
// ---------------------------------------------------------------------------

fn encode_type_seq(out: &mut Vec<u8>, types: &[Type]) {
    write_u32le(out, types.len() as u32);
    for t in types {
        encode_type(out, t);
    }
}

fn encode_opt_type(out: &mut Vec<u8>, ty: Option<&Type>) {
    match ty {
        None => out.push(0x00),
        Some(t) => {
            out.push(0x01);
            encode_type(out, t);
        }
    }
}

fn encode_type(out: &mut Vec<u8>, ty: &Type) {
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
            for part in parts.iter() {
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
            for m in members.iter() {
                encode_str(out, &m.name);
                encode_type(out, &m.ty);
                out.push(m.optional as u8);
                out.push(m.readonly as u8);
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

fn encode_tuple_elements(out: &mut Vec<u8>, elems: &[TupleElement]) {
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

fn encode_variance(out: &mut Vec<u8>, v: &Variance) {
    // No _ wildcard.
    out.push(match v {
        Variance::Invariant => 0x01,
        Variance::Covariant => 0x02,
        Variance::Contravariant => 0x03,
    });
}

fn encode_mapped_modifier(out: &mut Vec<u8>, m: &MappedModifier) {
    // No _ wildcard.
    out.push(match m {
        MappedModifier::Add => 0x01,
        MappedModifier::Remove => 0x02,
        MappedModifier::Absent => 0x03,
    });
}

fn encode_anon_record_form(out: &mut Vec<u8>, f: &AnonRecordForm) {
    // No _ wildcard.
    out.push(match f {
        AnonRecordForm::Struct => 0x01,
        AnonRecordForm::Interface => 0x02,
    });
}

fn encode_primitive(out: &mut Vec<u8>, p: &Primitive) {
    // No _ wildcard.
    match p {
        Primitive::Integer { signed, width } => {
            out.push(0x01);
            out.push(*signed as u8);
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
            out.push(*mutable as u8);
            encode_type(out, ty);
        }
        Primitive::Builtin(s) => {
            out.push(0x09);
            encode_str(out, s);
        }
    }
}

fn encode_width(out: &mut Vec<u8>, w: &Width) {
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
fn encode_ref(out: &mut Vec<u8>, r: &RawRef) {
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

fn encode_auto_facts(out: &mut Vec<u8>, facts: &[AutoFact]) {
    write_u32le(out, facts.len() as u32);
    for f in facts {
        encode_auto_trait(out, &f.trait_);
        encode_auto_state(out, &f.state);
    }
}

fn encode_auto_trait(out: &mut Vec<u8>, t: &AutoTrait) {
    // No _ wildcard.
    match t {
        AutoTrait::Send => out.push(0x01),
        AutoTrait::Sync => out.push(0x02),
        AutoTrait::Unpin => out.push(0x03),
        AutoTrait::UnwindSafe => out.push(0x04),
        AutoTrait::RefUnwindSafe => out.push(0x05),
    }
}

fn encode_auto_state(out: &mut Vec<u8>, s: &AutoState) {
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

fn encode_opt_str(out: &mut Vec<u8>, s: Option<&str>) {
    match s {
        None => out.push(0x00),
        Some(v) => {
            out.push(0x01);
            encode_str(out, v);
        }
    }
}

fn encode_str_seq(out: &mut Vec<u8>, ss: &[String]) {
    write_u32le(out, ss.len() as u32);
    for s in ss {
        encode_str(out, s);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
