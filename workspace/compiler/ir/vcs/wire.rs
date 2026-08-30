//! Local format-level wire types for the NdIrF1 text format and archive sealing.
//!
//! These types represent the serialization-stable encoding of IR entries used by
//! the F1 text format (`{intro_hex}.nir`) and the binary archive. They are
//! explicitly NOT shared with `nudox-ir` — they are the *format* layer that
//! lives above the IR data model.
//!
//! # Why not nudox-ir types?
//!
//! The nudox-ir data model uses rich, high-fidelity types (`Type`, `GenericParam`,
//! etc.) that carry more information than the F1 format encodes. The wire types
//! here are the frozen, backwards-compatible encoding that libpijul's patching
//! operates on. Converting between them is done in `f1.rs`.

use serde::{Deserialize, Serialize};

use ir::change::{ContentBlake3, IntroId, StableRef};
use ir::entry::Visibility;
use ir::foreign::ForeignKey;
use ir::kind::KindDiscriminant;

// ---------------------------------------------------------------------------
// TypeRefWire
// ---------------------------------------------------------------------------

/// Wire form of a type reference: same-package intro or cross-package stable ref.
///
/// Appended variants (`ForeignUnlinked`, `UnresolvedExternal`) carry the two
/// cases a producer can name a cross-package target without the seal-time
/// resolution `Foreign(StableRef)` requires. They are new tail variants, not a
/// reorder — `Same`/`Foreign`'s existing postcard discriminants are unchanged.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum TypeRefWire {
    /// Same-package intro.
    Same(IntroId),
    /// Cross-package stable reference, resolved at seal time.
    Foreign(StableRef),
    /// A cross-package reference the producer *named* but `seal` could not
    /// resolve to a `StableRef` — the wire twin of
    /// [`ir::index::Ref::Foreign`]`{ key, target: None }`. `ForeignKey` is
    /// already the self-contained, durable wire-safe encoding of "everything a
    /// producer knows about a cross-package target" (see its own module
    /// docs), so it is carried here unprojected: a downstream reader can
    /// render and re-link it exactly as the in-process/table path
    /// (`build_reference_set_from_table`) already does from the live `Entry`.
    ForeignUnlinked(ForeignKey),
    /// A cross-package type mention the producer could not even build a
    /// `ForeignKey` for — the wire twin of
    /// [`ir::kinds::ty::UnknownType::UnresolvedExternal`]`{ name }`. Only the
    /// producer's spelling is carried; there is no package to name (that is
    /// exactly why the producer reached for this instead of
    /// `ForeignUnlinked`).
    UnresolvedExternal(String),
}

// ---------------------------------------------------------------------------
// WidthWire
// ---------------------------------------------------------------------------

/// Wire form of a numeric width specifier.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum WidthWire {
    /// Pointer-sized (platform-dependent).
    Arch,
    /// Fixed bit-width.
    Fixed(u32),
}

// ---------------------------------------------------------------------------
// PrimitiveWire
// ---------------------------------------------------------------------------

/// Wire form of a primitive type.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum PrimitiveWire {
    Integer {
        signed: bool,
        width: WidthWire,
    },
    Float(WidthWire),
    Bool,
    Char,
    Str,
    MutPointer(Box<TypeRefWire>),
    ConstPointer(Box<TypeRefWire>),
    Reference {
        lifetime: Option<String>,
        mutable: bool,
        ty: Box<TypeRefWire>,
    },
    Builtin(String),
}

// ---------------------------------------------------------------------------
// TypeWire
// ---------------------------------------------------------------------------

/// Wire form of a type expression (used in kind bodies and parameter types).
///
/// `UnresolvedExternal` is an appended tail variant (new postcard
/// discriminant; existing variants keep theirs) — the *value*-position twin
/// of [`TypeRefWire::UnresolvedExternal`]: an alias target, an impl's
/// `self_ty`, a where-predicate's target, or a generic default can each be an
/// unresolved cross-package mention just as easily as a field or parameter
/// type can. Before this variant existed, `raise_type_wire` had no choice but
/// to collapse it into `TypeWire::Any`, indistinguishable from a real
/// top-typed value — see that function's doc comment.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum TypeWire {
    SelfType,
    Primitive(PrimitiveWire),
    Tuple(Box<[TypeRefWire]>),
    Slice(Box<TypeRefWire>),
    Array { ty: Box<TypeRefWire>, length: u64 },
    Union(Box<[TypeRefWire]>),
    Intersection(Box<[TypeRefWire]>),
    Never,
    Any,
    /// A cross-package type mention (value position) the producer could not
    /// build a `ForeignKey` for — the wire twin of
    /// [`ir::kinds::ty::UnknownType::UnresolvedExternal`]`{ name }`.
    UnresolvedExternal(String),
}

// ---------------------------------------------------------------------------
// ParamWire
// ---------------------------------------------------------------------------

/// Wire form of a function parameter (input or output).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ParamWire {
    /// Parameter name, if named (optional in some languages).
    pub name: Option<String>,
    /// Parameter type.
    pub ty: TypeRefWire,
}

// ---------------------------------------------------------------------------
// Shared vocab: SelfKind, FnSigFlags
// ---------------------------------------------------------------------------

/// The receiver kind for the first parameter of a method.
// frozen — never renumber/reorder
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum SelfKind {
    /// No self parameter (free function or static method).
    None,
    /// `self` — consumed by value.
    Value,
    /// `&self` — shared reference.
    Ref,
    /// `&mut self` — mutable reference.
    RefMut,
    /// Any other receiver type (e.g. `Arc<Self>`, `Pin<&mut Self>`).
    Arbitrary(TypeRefWire),
}

/// Canonical flags on a function / method signature.
// frozen — never renumber/reorder field encoding
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FnSigFlags {
    /// Receiver / self-parameter kind.
    pub self_kind: SelfKind,
    /// `async fn`.
    pub is_async: bool,
    /// `const fn`.
    pub is_const: bool,
    /// `unsafe fn`.
    pub is_unsafe: bool,
    /// Explicit ABI string (e.g. `"C"`, `"Rust"`). `None` = default ABI.
    pub abi: Option<String>,
    /// Variadic (`...`) parameter.
    pub variadic: bool,
    /// Has a default body (trait method with default).
    pub defaulted: bool,
}

impl Default for FnSigFlags {
    fn default() -> Self {
        Self {
            self_kind: SelfKind::None,
            is_async: false,
            is_const: false,
            is_unsafe: false,
            abi: None,
            variadic: false,
            defaulted: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared vocab: GenericParamWire, WherePredWire
// ---------------------------------------------------------------------------

/// One generic parameter (lifetime, type, or const).
// frozen — never renumber/reorder
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum GenericParamWire {
    /// A lifetime parameter, e.g. `'a`.
    Lifetime { name: String },
    /// A type parameter, e.g. `T: Trait = Default`.
    Type {
        name: String,
        bounds: Box<[TypeRefWire]>,
        default: Option<TypeWire>,
    },
    /// A const generic parameter, e.g. `const N: usize = 0`.
    Const {
        name: String,
        ty: TypeRefWire,
        default: Option<String>,
    },
}

/// A single `where` predicate: `target: bounds`.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct WherePredWire {
    /// The type being constrained.
    pub target: TypeWire,
    /// The bounds it must satisfy.
    pub bounds: Box<[TypeRefWire]>,
}

// ---------------------------------------------------------------------------
// Shared vocab: TriState, Sealed, TraitFlags, ImplFlags
// ---------------------------------------------------------------------------

/// A three-valued boolean for facts that may be unknown at record time.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum TriState {
    Yes,
    No,
    Unknown,
}

/// How thoroughly a trait is sealed against external implementation.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum Sealed {
    None,
    PubApi,
    Full,
}

/// Flags specific to a trait definition.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TraitFlags {
    pub is_auto: bool,
    pub is_unsafe: bool,
    pub dyn_compat: TriState,
    pub sealed: Sealed,
}

impl Default for TraitFlags {
    fn default() -> Self {
        Self {
            is_auto: false,
            is_unsafe: false,
            dyn_compat: TriState::Unknown,
            sealed: Sealed::None,
        }
    }
}

/// Flags specific to an `impl` block.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug, Default)]
pub struct ImplFlags {
    pub negative: bool,
    pub blanket: bool,
}

// ---------------------------------------------------------------------------
// Shared vocab: RecordForm, VariantForm
// ---------------------------------------------------------------------------

/// The structural form of a record / struct.
// frozen — never renumber/reorder
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum RecordForm {
    Struct,
    Tuple,
    Unit,
    Union,
}

/// The structural form of an enum variant.
// frozen — never renumber/reorder
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum VariantForm {
    Unit,
    Tuple,
    Struct,
}

// ---------------------------------------------------------------------------
// Shared vocab: AutoTrait, AutoState, AutoFact
// ---------------------------------------------------------------------------

/// Well-known auto traits whose implementation status is recorded.
// frozen — never renumber/reorder
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum AutoTrait {
    Send,
    Sync,
    Unpin,
    UnwindSafe,
    RefUnwindSafe,
}

/// Whether a type implements an auto trait.
// frozen — never renumber/reorder
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum AutoState {
    Yes,
    No,
    Cond,
}

/// A single recorded auto-trait fact for a type.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct AutoFact {
    pub trait_: AutoTrait,
    pub state: AutoState,
}

// ---------------------------------------------------------------------------
// Shared vocab: AttrTok, CfgExpr
// ---------------------------------------------------------------------------

/// Symbol-level normalized attribute token.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct AttrTok {
    pub token: String,
    pub arg: Option<String>,
}

/// Normalized cfg predicate.
// frozen — never renumber/reorder variants
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum CfgExpr {
    All(Box<[CfgExpr]>),
    Any(Box<[CfgExpr]>),
    Not(Box<CfgExpr>),
    Feature(String),
    TargetOs(String),
    TargetArch(String),
    Other(String),
}

// ---------------------------------------------------------------------------
// Kind body wires
// ---------------------------------------------------------------------------

/// Wire body for a module entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug, Default)]
pub struct ModuleWire {}

/// Wire body for a record / struct / union entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct RecordWire {
    pub form: RecordForm,
    pub fields: Box<[IntroId]>,
    pub generics: Box<[GenericParamWire]>,
    pub wheres: Box<[WherePredWire]>,
    pub auto: Box<[AutoFact]>,
}

/// Wire body for a field entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FieldWire {
    pub ty: Option<TypeRefWire>,
}

/// Wire body for a function / method entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FunctionWire {
    pub input_params: Box<[ParamWire]>,
    pub output_params: Box<[ParamWire]>,
    pub sig: FnSigFlags,
    pub generics: Box<[GenericParamWire]>,
    pub wheres: Box<[WherePredWire]>,
}

/// Wire body for a type-alias entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TypeAliasWire {
    pub ty: TypeWire,
    pub generics: Box<[GenericParamWire]>,
    pub wheres: Box<[WherePredWire]>,
    pub auto: Box<[AutoFact]>,
}

/// Wire body for a trait definition.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TraitWire {
    pub supers: Box<[TypeRefWire]>,
    pub flags: TraitFlags,
    pub generics: Box<[GenericParamWire]>,
    pub wheres: Box<[WherePredWire]>,
}

/// Wire body for a trait impl or inherent impl.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ImplWire {
    pub of: Option<TypeRefWire>,
    pub self_ty: TypeWire,
    pub flags: ImplFlags,
    pub generics: Box<[GenericParamWire]>,
    pub wheres: Box<[WherePredWire]>,
}

/// Wire body for an enum type.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct EnumWire {
    pub variants: Box<[IntroId]>,
    pub generics: Box<[GenericParamWire]>,
    pub wheres: Box<[WherePredWire]>,
    pub auto: Box<[AutoFact]>,
}

/// Wire body for an enum variant.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct VariantWire {
    pub form: VariantForm,
    pub discr: Option<String>,
    pub fields: Box<[IntroId]>,
}

/// Wire body for a constant declaration.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ConstWire {
    pub ty: TypeRefWire,
    pub value: Option<String>,
}

/// Wire body for a static declaration.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct StaticWire {
    pub ty: TypeRefWire,
    pub mutable: bool,
}

/// Wire body for a re-export entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ReexportWire {
    pub target: StableRef,
}

// ---------------------------------------------------------------------------
// KindWire
// ---------------------------------------------------------------------------

/// Wire discriminated union of all kind bodies.
///
/// `Type` is the wire variant for type aliases (nudox-ir's `KindDiscriminant::Alias`).
/// The name `Type` is kept here for backward compatibility with the NdIrF1 format
/// token "type" and all existing ir-vcs pattern matches.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum KindWire {
    Module(ModuleWire),
    Record(RecordWire),
    Field(FieldWire),
    Function(FunctionWire),
    /// Type alias. Wire discriminant 5. Token "type" in NdIrF1.
    /// Maps to `KindDiscriminant::Alias` in nudox-ir (renamed to avoid collision).
    Type(TypeAliasWire),
    Trait(TraitWire),
    Impl(ImplWire),
    Enum(EnumWire),
    Variant(VariantWire),
    Const(ConstWire),
    Static(StaticWire),
    /// A function parameter. Params became first-class entries with their own
    /// `IntroId` in nudox-ir, so they need a wire form: a rename of one param
    /// now touches only that param's own `.nir` file and cannot conflict with a
    /// sibling's type change.
    Param(ParamWire),
    Reexport(ReexportWire),
}

/// One leaf [`KindWire::for_each_type_mention`] visits: either a full
/// [`TypeRefWire`] occupying a reference-position slot, or an
/// unresolved-external name found directly in a *value*-position
/// [`TypeWire::UnresolvedExternal`] — which is not itself a `TypeRefWire` (a
/// value-position slot is typed `TypeWire`, not `TypeRefWire`; see that
/// variant's doc comment on why it had to be added to both enums). A caller
/// that only cares "is this an unresolved cross-package mention, and what
/// does it name" — as `index`'s payload-harvest does — matches both arms of
/// this type uniformly instead of needing two separate visitor methods.
#[derive(Clone, Copy, Debug)]
pub enum TypeMention<'a> {
    Ref(&'a TypeRefWire),
    UnresolvedExternalValue(&'a str),
}

impl KindWire {
    /// Visit every cross-package/unresolved [`TypeMention`] this kind body's
    /// declaration surface names — field/param/return types, generic bounds
    /// and defaults, where-clause targets and bounds, trait supertraits, and
    /// an impl's `of`/`self_ty` — however deeply nested inside a
    /// tuple/slice/array/union/intersection/pointer/reference wrapper.
    ///
    /// The wire-format analogue of [`ir::entry::Entry::for_each_ref`] /
    /// [`ir::entry::Entry::for_each_unknown`], scoped to what a `KindWire`
    /// can hold: unlike the semantic walk, no `Node` parent/child structural
    /// edges are mixed in here, because those live on `WireEntry::parent` /
    /// `WireEntry::links` (see `ir_vcs::protocol::frame::WireEntry`), never
    /// inside a `KindWire` body — so every mention this reaches is a genuine
    /// type-nominal usage, not containment. `RecordWire::fields`,
    /// `EnumWire::variants` and `VariantWire::fields` are `Box<[IntroId]>`
    /// (child entry ids, the wire equivalent of those same structural edges)
    /// and are correctly never visited here for the same reason.
    ///
    /// Traversal mirrors [`crate::subst::substitute_and_reseal`]'s
    /// `map_kind`/`map_type_wire`/`map_primitive` walk — the two must agree
    /// on which slots hold a `TypeRefWire`/`TypeWire`, so a future
    /// wire-format addition should update both together.
    pub fn for_each_type_mention<'a>(&'a self, mut f: impl FnMut(TypeMention<'a>)) {
        fn visit_generics<'a>(
            generics: &'a [GenericParamWire],
            f: &mut impl FnMut(TypeMention<'a>),
        ) {
            for g in generics {
                match g {
                    GenericParamWire::Lifetime { .. } => {}
                    GenericParamWire::Type { bounds, default, .. } => {
                        for b in bounds.iter() {
                            f(TypeMention::Ref(b));
                        }
                        if let Some(ty) = default {
                            visit_type_wire(ty, f);
                        }
                    }
                    GenericParamWire::Const { ty, .. } => f(TypeMention::Ref(ty)),
                }
            }
        }
        fn visit_wheres<'a>(wheres: &'a [WherePredWire], f: &mut impl FnMut(TypeMention<'a>)) {
            for w in wheres {
                visit_type_wire(&w.target, f);
                for b in w.bounds.iter() {
                    f(TypeMention::Ref(b));
                }
            }
        }
        fn visit_primitive<'a>(p: &'a PrimitiveWire, f: &mut impl FnMut(TypeMention<'a>)) {
            match p {
                PrimitiveWire::MutPointer(t) | PrimitiveWire::ConstPointer(t) => {
                    f(TypeMention::Ref(t));
                }
                PrimitiveWire::Reference { ty, .. } => f(TypeMention::Ref(ty)),
                PrimitiveWire::Integer { .. }
                | PrimitiveWire::Float(_)
                | PrimitiveWire::Bool
                | PrimitiveWire::Char
                | PrimitiveWire::Str
                | PrimitiveWire::Builtin(_) => {}
            }
        }
        fn visit_type_wire<'a>(t: &'a TypeWire, f: &mut impl FnMut(TypeMention<'a>)) {
            match t {
                TypeWire::Primitive(p) => visit_primitive(p, f),
                TypeWire::Tuple(ts) | TypeWire::Union(ts) | TypeWire::Intersection(ts) => {
                    for t in ts.iter() {
                        f(TypeMention::Ref(t));
                    }
                }
                TypeWire::Slice(t) => f(TypeMention::Ref(t)),
                TypeWire::Array { ty, .. } => f(TypeMention::Ref(ty)),
                TypeWire::UnresolvedExternal(name) => {
                    f(TypeMention::UnresolvedExternalValue(name));
                }
                TypeWire::SelfType | TypeWire::Never | TypeWire::Any => {}
            }
        }

        match self {
            KindWire::Module(_) | KindWire::Variant(_) | KindWire::Reexport(_) => {}
            KindWire::Record(r) => {
                visit_generics(&r.generics, &mut f);
                visit_wheres(&r.wheres, &mut f);
            }
            KindWire::Field(field) => {
                if let Some(ty) = &field.ty {
                    f(TypeMention::Ref(ty));
                }
            }
            KindWire::Function(func) => {
                for p in func.input_params.iter().chain(func.output_params.iter()) {
                    f(TypeMention::Ref(&p.ty));
                }
                if let SelfKind::Arbitrary(ty) = &func.sig.self_kind {
                    f(TypeMention::Ref(ty));
                }
                visit_generics(&func.generics, &mut f);
                visit_wheres(&func.wheres, &mut f);
            }
            KindWire::Type(alias) => {
                visit_type_wire(&alias.ty, &mut f);
                visit_generics(&alias.generics, &mut f);
                visit_wheres(&alias.wheres, &mut f);
            }
            KindWire::Trait(t) => {
                for s in t.supers.iter() {
                    f(TypeMention::Ref(s));
                }
                visit_generics(&t.generics, &mut f);
                visit_wheres(&t.wheres, &mut f);
            }
            KindWire::Impl(imp) => {
                if let Some(of) = &imp.of {
                    f(TypeMention::Ref(of));
                }
                visit_type_wire(&imp.self_ty, &mut f);
                visit_generics(&imp.generics, &mut f);
                visit_wheres(&imp.wheres, &mut f);
            }
            KindWire::Enum(e) => {
                visit_generics(&e.generics, &mut f);
                visit_wheres(&e.wheres, &mut f);
            }
            KindWire::Const(c) => f(TypeMention::Ref(&c.ty)),
            KindWire::Static(s) => f(TypeMention::Ref(&s.ty)),
            KindWire::Param(p) => f(TypeMention::Ref(&p.ty)),
        }
    }

    /// The frozen discriminant for this wire variant.
    #[inline]
    pub fn discriminant(&self) -> KindDiscriminant {
        match self {
            KindWire::Module(_) => KindDiscriminant::Module,
            KindWire::Record(_) => KindDiscriminant::Record,
            KindWire::Field(_) => KindDiscriminant::Field,
            KindWire::Function(_) => KindDiscriminant::Function,
            KindWire::Type(_) => KindDiscriminant::Alias,
            KindWire::Trait(_) => KindDiscriminant::Trait,
            KindWire::Impl(_) => KindDiscriminant::Impl,
            KindWire::Enum(_) => KindDiscriminant::Enum,
            KindWire::Variant(_) => KindDiscriminant::Variant,
            KindWire::Const(_) => KindDiscriminant::Const,
            KindWire::Static(_) => KindDiscriminant::Static,
            KindWire::Reexport(_) => KindDiscriminant::Reexport,
            KindWire::Param(_) => KindDiscriminant::Param,
        }
    }
}

// ---------------------------------------------------------------------------
// DeprecationWire / DocLinkWire / SymbolWire
// ---------------------------------------------------------------------------

/// Wire form of symbol deprecation metadata.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct DeprecationWire {
    pub note: Option<String>,
    pub since: Option<String>,
}

/// Wire form of a documentation cross-reference link.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct DocLinkWire {
    pub target: StableRef,
    pub label: Option<String>,
    #[serde(default)]
    pub source_span: Option<(u32, u32)>,
}

/// Wire form of symbol metadata.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct SymbolWire {
    pub name: String,
    pub visibility: Visibility,
    pub documentation: Option<String>,
    pub source_path: String,
    pub span_start: u32,
    pub span_end: u32,
    pub aliases: Vec<String>,
    pub deprecation: Option<DeprecationWire>,
    pub doc_links: Vec<DocLinkWire>,
    pub attrs: Vec<AttrTok>,
    pub cfg: Option<CfgExpr>,
}

// ---------------------------------------------------------------------------
// EntryPayloadFlags
// ---------------------------------------------------------------------------

/// Bitmask flags on an [`OwnedEntryPayload`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug, Default)]
pub struct EntryPayloadFlags(pub u8);

impl EntryPayloadFlags {
    // bit 0 reserved (was IS_REFERENCE, retired v2 — never reuse)
    /// The symbol has a deprecation notice.
    pub const HAS_DEPRECATION: u8 = 1 << 1;

    #[inline]
    pub fn has(self, flag: u8) -> bool {
        (self.0 & flag) != 0
    }

    #[inline]
    pub fn set(&mut self, flag: u8) {
        self.0 |= flag;
    }
}

// ---------------------------------------------------------------------------
// OwnedEntryPayload
// ---------------------------------------------------------------------------

/// The fully-sealed payload for one intro stored in the `PristineIntroTable`.
///
/// Kept in ir-vcs as the format-level type for the archive sealing path.
/// The data model uses `nudox_ir::entry::Entry` instead.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct OwnedEntryPayload {
    pub symbol: SymbolWire,
    pub kind_disc: KindDiscriminant,
    pub kind: KindWire,
    pub flags: EntryPayloadFlags,
    pub payload_hash: ContentBlake3,
}

impl OwnedEntryPayload {
    pub const HASH_DOMAIN: &'static str = "nudox.entry.v2";

    pub fn compute_payload_hash(
        symbol: &SymbolWire,
        kind_disc: &KindDiscriminant,
        kind: &KindWire,
        flags: &EntryPayloadFlags,
    ) -> ContentBlake3 {
        let bytes = postcard::to_allocvec(&(symbol, kind_disc, kind, flags))
            .expect("OwnedEntryPayload postcard serialization is infallible");
        ContentBlake3::from_domain(Self::HASH_DOMAIN, &bytes)
    }

    pub fn sealed(
        symbol: SymbolWire,
        kind_disc: KindDiscriminant,
        kind: KindWire,
        flags: EntryPayloadFlags,
    ) -> Self {
        let payload_hash = Self::compute_payload_hash(&symbol, &kind_disc, &kind, &flags);
        Self {
            symbol,
            kind_disc,
            kind,
            flags,
            payload_hash,
        }
    }
}

// ---------------------------------------------------------------------------
// PayloadTable — format-layer container for OwnedEntryPayload
// ---------------------------------------------------------------------------

/// The VCS format-layer materialized table: maps `IntroId` to its
/// [`OwnedEntryPayload`] + parent edge + link set.
///
/// This mirrors the old `workspace/ir` `PristineIntroTable` API but stores
/// wire-format payloads rather than semantic [`nudox_ir::entry::Entry`]s.
///
/// The nudox-ir [`ir::apply::PristineIntroTable`] now stores rich `Entry`
/// objects and is used by the continuity layer (via [`crate::lower`]).
/// `PayloadTable` is used by the diff, archive, semver, checkout, and test
/// layers that still operate on wire-format data.
#[derive(Debug, Default, Clone)]
pub struct PayloadTable {
    entries: std::collections::BTreeMap<
        ir::change::IntroId,
        (OwnedEntryPayload, Option<ir::change::IntroId>),
    >,
    links: Vec<crate::vcs_types::LinkRecord>,
}

impl PayloadTable {
    /// Construct an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a live entry.
    pub fn insert_live(
        &mut self,
        intro: ir::change::IntroId,
        payload: OwnedEntryPayload,
        parent: Option<ir::change::IntroId>,
    ) {
        self.entries.insert(intro, (payload, parent));
    }

    /// Insert a link record.
    pub fn insert_link(&mut self, link: crate::vcs_types::LinkRecord) {
        self.links.push(link);
    }

    /// Look up a live entry by `IntroId`.
    pub fn get(&self, intro: ir::change::IntroId) -> Option<&OwnedEntryPayload> {
        self.entries.get(&intro).map(|(p, _)| p)
    }

    /// Return the parent `IntroId` of this intro, if any.
    pub fn parent_of(&self, intro: ir::change::IntroId) -> Option<ir::change::IntroId> {
        self.entries.get(&intro).and_then(|(_, p)| *p)
    }

    /// Iterate all live entries in key order.
    pub fn live_entries(&self) -> impl Iterator<Item = (ir::change::IntroId, &OwnedEntryPayload)> {
        self.entries.iter().map(|(id, (p, _))| (*id, p))
    }

    /// Iterate all live entries with their parent edges.
    pub fn live_entries_with_parent(
        &self,
    ) -> impl Iterator<
        Item = (
            ir::change::IntroId,
            &OwnedEntryPayload,
            Option<ir::change::IntroId>,
        ),
    > {
        self.entries
            .iter()
            .map(|(id, (p, parent))| (*id, p, *parent))
    }

    /// Iterate all link records.
    pub fn links(&self) -> impl Iterator<Item = &crate::vcs_types::LinkRecord> {
        self.links.iter()
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries are present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True if this intro is present.
    pub fn contains(&self, intro: ir::change::IntroId) -> bool {
        self.entries.contains_key(&intro)
    }

    /// True if this intro is present (alias for [`contains`](Self::contains)).
    pub fn is_live(&self, intro: ir::change::IntroId) -> bool {
        self.contains(intro)
    }

    /// Return the children of `intro` (all entries whose parent is `intro`).
    ///
    /// O(n) — scans all entries.  For performance-sensitive callers, build a
    /// children index separately.
    pub fn children_of(&self, intro: ir::change::IntroId) -> Vec<ir::change::IntroId> {
        self.entries
            .iter()
            .filter_map(|(id, (_, parent))| {
                if *parent == Some(intro) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    }
}
