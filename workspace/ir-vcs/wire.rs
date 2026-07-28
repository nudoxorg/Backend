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
use ir::kind::KindDiscriminant;
use ir::entry::Visibility;

// ---------------------------------------------------------------------------
// TypeRefWire
// ---------------------------------------------------------------------------

/// Wire form of a type reference: same-package intro or cross-package stable ref.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum TypeRefWire {
    /// Same-package intro.
    Same(IntroId),
    /// Cross-package stable reference.
    Foreign(StableRef),
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
    Integer { signed: bool, width: WidthWire },
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
    Type { name: String, bounds: Box<[TypeRefWire]>, default: Option<TypeWire> },
    /// A const generic parameter, e.g. `const N: usize = 0`.
    Const { name: String, ty: TypeRefWire, default: Option<String> },
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
        Self { is_auto: false, is_unsafe: false, dyn_compat: TriState::Unknown, sealed: Sealed::None }
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

impl KindWire {
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
        Self { symbol, kind_disc, kind, flags, payload_hash }
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
    pub fn live_entries(
        &self,
    ) -> impl Iterator<Item = (ir::change::IntroId, &OwnedEntryPayload)> {
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
        self.entries.iter().map(|(id, (p, parent))| (*id, p, *parent))
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
    pub fn children_of(
        &self,
        intro: ir::change::IntroId,
    ) -> Vec<ir::change::IntroId> {
        self.entries
            .iter()
            .filter_map(|(id, (_, parent))| {
                if *parent == Some(intro) { Some(*id) } else { None }
            })
            .collect()
    }
}
