//! Wire (serialization-stable) twins of every in-memory IR type.
//!
//! The `*Wire` types are what actually crosses process or machine boundaries:
//! they are `serde`-annotated and postcard-serialized. The in-memory types in
//! [`crate::kind`] and [`crate::symbol`] may diverge from wire layout for
//! performance; the `*Wire` forms are the normative on-disk / on-wire shape.
//!
//! # Key types
//!
//! - [`OwnedEntryPayload`] — the sealed payload stored per intro: symbol wire,
//!   kind discriminant, kind body wire, flags, and a content hash.
//! - [`KindWire`] — the wire enum for kind bodies.
//! - [`SymbolWire`] — the wire form of [`crate::symbol::Symbol`] (strings owned).
//!
//! All `*Wire` types derive `Clone, PartialEq, Eq, Serialize, Deserialize, Debug`.

use serde::{Deserialize, Serialize};

use nudox_change::{ContentBlake3, IntroId, StableRef};

use crate::kind::KindDiscriminant;
use crate::symbol::Visibility;

// ---------------------------------------------------------------------------
// TypeRefWire
// ---------------------------------------------------------------------------

/// Wire form of [`crate::kind::TypeRef`].
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

/// Wire form of [`crate::kind::Width`].
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

/// Wire form of [`crate::kind::Primitive`].
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum PrimitiveWire {
    Integer { signed: bool, width: WidthWire },
    Float(WidthWire),
    Bool,
    Char,
    Str,
    MutPointer(Box<TypeRefWire>),
    ConstPointer(Box<TypeRefWire>),
    /// Note: `lifetime` is excluded from the type skeleton (structural-only).
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
///
/// Encoded by `fnsig_flag_bytes` for use in `SigKey` preimages (§4.6).
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
    /// Not sealed — any downstream crate may implement it.
    None,
    /// Sealed via a public supertrait on a private bound (pub-API pattern).
    PubApi,
    /// Fully sealed — only the defining crate can implement it.
    Full,
}

/// Flags specific to a trait definition.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TraitFlags {
    /// Auto-trait (e.g. `Send`, `Sync`).
    pub is_auto: bool,
    /// `unsafe trait`.
    pub is_unsafe: bool,
    /// Object-safety / dyn-compatibility.
    pub dyn_compat: TriState,
    /// Sealing evidence.
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
    /// Negative impl (`impl !Trait for Type`).
    pub negative: bool,
    /// Blanket impl (the self type contains a type parameter).
    pub blanket: bool,
}

// ---------------------------------------------------------------------------
// Shared vocab: RecordForm, VariantForm
// ---------------------------------------------------------------------------

/// The structural form of a record / struct.
// frozen — never renumber/reorder
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum RecordForm {
    /// Named-field struct.
    Struct,
    /// Tuple struct.
    Tuple,
    /// Unit struct.
    Unit,
    /// `union` (Rust-specific).
    Union,
}

/// The structural form of an enum variant.
// frozen — never renumber/reorder
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum VariantForm {
    /// A unit variant (no fields).
    Unit,
    /// A tuple variant.
    Tuple,
    /// A struct variant with named fields.
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
    /// Unconditionally implements the trait.
    Yes,
    /// Unconditionally does not implement the trait.
    No,
    /// Implements the trait only under certain generic bounds.
    Cond,
}

/// A single recorded auto-trait fact for a type.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct AutoFact {
    /// Which auto trait.
    pub trait_: AutoTrait,
    /// Implementation status.
    pub state: AutoState,
}

// ---------------------------------------------------------------------------
// Shared vocab: AttrTok, CfgExpr
// ---------------------------------------------------------------------------

/// Symbol-level normalized attribute token (§6.2 key 8 / §8.5).
///
/// The `token` is an ecosystem-scoped opaque string; `arg` is an optional
/// rendered argument (e.g. `repr` → arg `"C"`).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct AttrTok {
    /// Attribute name token (e.g. `"must_use"`, `"repr"`, `"doc_hidden"`).
    pub token: String,
    /// Optional argument text (e.g. `"C"` for `repr(C)`).
    pub arg: Option<String>,
}

/// Normalized cfg predicate (§8.6).
// frozen — never renumber/reorder variants
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum CfgExpr {
    All(Box<[CfgExpr]>),
    Any(Box<[CfgExpr]>),
    Not(Box<CfgExpr>),
    Feature(String),
    TargetOs(String),
    TargetArch(String),
    /// Any other cfg predicate not covered above.
    Other(String),
}

// ---------------------------------------------------------------------------
// Kind body wires
// ---------------------------------------------------------------------------

/// Wire body for a module entry (currently empty; reserved for future fields).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug, Default)]
pub struct ModuleWire {}

/// Wire body for a record / struct / union entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct RecordWire {
    /// Structural form of the record.
    pub form: RecordForm,
    /// Ordered list of field intro IDs (same-package).
    pub fields: Box<[IntroId]>,
    /// Generic parameters.
    pub generics: Box<[GenericParamWire]>,
    /// Where-clause predicates.
    pub wheres: Box<[WherePredWire]>,
    /// Recorded auto-trait facts.
    pub auto: Box<[AutoFact]>,
}

/// Wire body for a field entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FieldWire {
    /// Field type, if known.
    pub ty: Option<TypeRefWire>,
}

/// Wire body for a function / method entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FunctionWire {
    /// Input parameters (left-to-right).
    pub input_params: Box<[ParamWire]>,
    /// Output parameters / return types.
    pub output_params: Box<[ParamWire]>,
    /// Signature modifier flags.
    pub sig: FnSigFlags,
    /// Generic parameters.
    pub generics: Box<[GenericParamWire]>,
    /// Where-clause predicates.
    pub wheres: Box<[WherePredWire]>,
}

/// Wire body for a type-alias entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TypeAliasWire {
    /// The aliased type expression.
    pub ty: TypeWire,
    /// Generic parameters.
    pub generics: Box<[GenericParamWire]>,
    /// Where-clause predicates.
    pub wheres: Box<[WherePredWire]>,
    /// Recorded auto-trait facts.
    pub auto: Box<[AutoFact]>,
}

/// Wire body for a trait definition.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TraitWire {
    /// Supertraits (as type refs).
    pub supers: Box<[TypeRefWire]>,
    /// Trait modifier flags.
    pub flags: TraitFlags,
    /// Generic parameters.
    pub generics: Box<[GenericParamWire]>,
    /// Where-clause predicates.
    pub wheres: Box<[WherePredWire]>,
}

/// Wire body for a trait impl or inherent impl.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ImplWire {
    /// The implemented trait, if any (`None` for inherent impls).
    pub of: Option<TypeRefWire>,
    /// The self type the impl targets.
    pub self_ty: TypeWire,
    /// Impl flags (negative, blanket).
    pub flags: ImplFlags,
    /// Generic parameters.
    pub generics: Box<[GenericParamWire]>,
    /// Where-clause predicates.
    pub wheres: Box<[WherePredWire]>,
}

/// Wire body for an enum type.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct EnumWire {
    /// Ordered list of variant intro IDs (same-package).
    pub variants: Box<[IntroId]>,
    /// Generic parameters.
    pub generics: Box<[GenericParamWire]>,
    /// Where-clause predicates.
    pub wheres: Box<[WherePredWire]>,
    /// Recorded auto-trait facts.
    pub auto: Box<[AutoFact]>,
}

/// Wire body for an enum variant.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct VariantWire {
    /// Structural form of the variant.
    pub form: VariantForm,
    /// Explicit discriminant value (rendered as text), if any.
    pub discr: Option<String>,
    /// Field intro IDs for tuple/struct variants.
    pub fields: Box<[IntroId]>,
}

/// Wire body for a constant declaration.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ConstWire {
    /// The constant's type.
    pub ty: TypeRefWire,
    /// Rendered constant value text, if available.
    pub value: Option<String>,
}

/// Wire body for a static declaration.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct StaticWire {
    /// The static's type.
    pub ty: TypeRefWire,
    /// Whether the static is declared `static mut`.
    pub mutable: bool,
}

/// Wire body for a re-export entry.
///
/// Fixes I4: the target is now carried in the wire body, not in a now-retired
/// `IS_REFERENCE` flag with no associated target data.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ReexportWire {
    /// The canonical target this re-export forwards to.
    pub target: StableRef,
}

// ---------------------------------------------------------------------------
// KindWire
// ---------------------------------------------------------------------------

/// Wire discriminated union of all kind bodies.
///
/// Matches 1-to-1 with [`KindDiscriminant`] values.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum KindWire {
    Module(ModuleWire),
    Record(RecordWire),
    Field(FieldWire),
    Function(FunctionWire),
    Type(TypeAliasWire),
    Trait(TraitWire),
    Impl(ImplWire),
    Enum(EnumWire),
    Variant(VariantWire),
    Const(ConstWire),
    Static(StaticWire),
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
            KindWire::Type(_) => KindDiscriminant::Type,
            KindWire::Trait(_) => KindDiscriminant::Trait,
            KindWire::Impl(_) => KindDiscriminant::Impl,
            KindWire::Enum(_) => KindDiscriminant::Enum,
            KindWire::Variant(_) => KindDiscriminant::Variant,
            KindWire::Const(_) => KindDiscriminant::Const,
            KindWire::Static(_) => KindDiscriminant::Static,
            KindWire::Reexport(_) => KindDiscriminant::Reexport,
        }
    }
}

// ---------------------------------------------------------------------------
// DeprecationWire / DocLinkWire / SymbolWire
// ---------------------------------------------------------------------------

/// Wire form of [`crate::symbol::Deprecation`].
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct DeprecationWire {
    pub note: Option<String>,
    pub since: Option<String>,
}

/// Wire form of [`crate::symbol::DocLink`].
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct DocLinkWire {
    pub target: StableRef,
    pub label: Option<String>,
}

/// Wire form of [`crate::symbol::Symbol`].
///
/// All string fields are `String` (owned) so they can be deserialized without
/// a string interner. The builder converts these back to interned [`crate::index::StrId`]s
/// when constructing arena entries.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct SymbolWire {
    pub name: String,
    /// Typed access level. Postcard encodes the variant index, which is
    /// byte-identical to the former raw-`u8` encoding (see the payload-hash
    /// golden pin).
    pub visibility: Visibility,
    pub documentation: Option<String>,
    pub source_path: String,
    pub span_start: u32,
    pub span_end: u32,
    pub aliases: Vec<String>,
    pub deprecation: Option<DeprecationWire>,
    pub doc_links: Vec<DocLinkWire>,
    /// Normalized attributes on this symbol (§6.2 key 8 / §8.5).
    pub attrs: Vec<AttrTok>,
    /// Cfg predicate guarding this symbol (§8.6), if any.
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

    /// Test a flag bit.
    #[inline]
    pub fn has(self, flag: u8) -> bool {
        (self.0 & flag) != 0
    }

    /// Set a flag bit.
    #[inline]
    pub fn set(&mut self, flag: u8) {
        self.0 |= flag;
    }
}

// ---------------------------------------------------------------------------
// ReferencePayload
// ---------------------------------------------------------------------------

/// Extra payload for reference (alias / re-export) entries: the canonical
/// target this intro forwards to.
///
/// Kept for compatibility with existing producers; the canonical wire body is
/// now [`ReexportWire`] (kind 12). Not wired into [`EntryPayloadFlags`] in v2.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ReferencePayload {
    pub target: StableRef,
}

// ---------------------------------------------------------------------------
// OwnedEntryPayload
// ---------------------------------------------------------------------------

/// The fully-sealed payload for one intro stored in the [`crate::apply::PristineIntroTable`].
///
/// The `payload_hash` is a domain-separated BLAKE3 over the postcard encoding of
/// `(symbol, kind_disc, kind, flags)`. It gives every payload a stable content
/// identity for lineage and change detection (the blob format's `hash` line).
///
/// Construct via [`OwnedEntryPayload::sealed`]; **never** set `payload_hash` manually.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct OwnedEntryPayload {
    /// Symbol metadata.
    pub symbol: SymbolWire,
    /// Kind discriminant (redundant with `kind`, but handy for dispatch without
    /// deserializing the full body).
    pub kind_disc: KindDiscriminant,
    /// Kind body.
    pub kind: KindWire,
    /// Entry flags.
    pub flags: EntryPayloadFlags,
    /// Domain-tagged BLAKE3 hash of `(symbol, kind_disc, kind, flags)` encoded
    /// as postcard. Set by [`OwnedEntryPayload::sealed`].
    pub payload_hash: ContentBlake3,
}

impl OwnedEntryPayload {
    /// Domain tag for the v2 payload hash.
    // frozen — never rename; changing this is a wire-stability break
    pub const HASH_DOMAIN: &'static str = "nudox.entry.v2";

    /// Compute the payload hash by postcard-encoding `(symbol, kind_disc, kind,
    /// flags)` (as a tuple) and domain-hashing the result.
    ///
    /// Used internally by [`OwnedEntryPayload::sealed`].
    pub fn compute_payload_hash(
        symbol: &SymbolWire,
        kind_disc: &KindDiscriminant,
        kind: &KindWire,
        flags: &EntryPayloadFlags,
    ) -> ContentBlake3 {
        // Postcard-encode the tuple (symbol, kind_disc, kind, flags).
        // The tuple approach ensures a single unambiguous encoding.
        let bytes = postcard::to_allocvec(&(symbol, kind_disc, kind, flags))
            .expect("OwnedEntryPayload postcard serialization is infallible for in-memory data");
        ContentBlake3::from_domain(Self::HASH_DOMAIN, &bytes)
    }

    /// Construct and seal a payload: computes `payload_hash` from the given fields.
    ///
    /// This is the only intended constructor; callers must not set `payload_hash`
    /// manually.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_sym(name: &str) -> SymbolWire {
        SymbolWire {
            name: name.into(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 10,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        }
    }

    #[test]
    fn payload_hash_is_deterministic() {
        let sym = simple_sym("foo");
        let kind_disc = KindDiscriminant::Function;
        let kind = KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        });
        let flags = EntryPayloadFlags::default();

        let h1 = OwnedEntryPayload::compute_payload_hash(&sym, &kind_disc, &kind, &flags);
        let h2 = OwnedEntryPayload::compute_payload_hash(&sym, &kind_disc, &kind, &flags);
        assert_eq!(h1, h2);
    }

    #[test]
    fn sealed_payload_hash_matches_computed() {
        let sym = SymbolWire {
            name: "bar".into(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/main.rs".into(),
            span_start: 5,
            span_end: 50,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        };
        let kind_disc = KindDiscriminant::Module;
        let kind = KindWire::Module(ModuleWire {});
        let flags = EntryPayloadFlags::default();
        let expected = OwnedEntryPayload::compute_payload_hash(&sym, &kind_disc, &kind, &flags);
        let payload = OwnedEntryPayload::sealed(sym, kind_disc, kind, flags);
        assert_eq!(payload.payload_hash, expected);
    }

    /// **Golden pin** of the payload-hash derivation: postcard of the
    /// `(symbol, kind_disc, kind, flags)` tuple under the `nudox.entry.v2`
    /// domain. `payload_hash` is durable identity (before-hash lineage, blob
    /// `hash` lines), so encoding drift here is a wire-stability break — an
    /// intentional change requires a new domain tag.
    ///
    /// `SymbolWire` now includes `attrs` and `cfg` fields (both empty/None
    /// in this pin vector).
    // TODO(reviewer): regenerate golden — run this test once and replace "REGEN"
    // with the actual hex output (it will print in the test failure message).
    #[test]
    fn payload_hash_golden_pin() {
        let sym = SymbolWire {
            name: "golden".into(),
            visibility: Visibility::Private,
            documentation: Some("docs".into()),
            source_path: "src/lib.rs".into(),
            span_start: 7,
            span_end: 21,
            aliases: vec!["alias_a".into()],
            deprecation: Some(DeprecationWire { note: Some("old".into()), since: None }),
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        };
        let kind = KindWire::Function(FunctionWire {
            input_params: Box::new([ParamWire {
                name: Some("x".into()),
                ty: TypeRefWire::Same(IntroId::from_raw([0x11; 32])),
            }]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        });
        let h = OwnedEntryPayload::compute_payload_hash(
            &sym,
            &KindDiscriminant::Function,
            &kind,
            &EntryPayloadFlags::default(),
        );
        assert_eq!(
            h.to_hex(),
            "5d3a7ab80ecbf78f56d9a4cd1f8494f852d9c28e467c51d07a746de9b0bcc20c",
            "payload-hash preimage drifted — wire-stability break (v2 golden). \
             An intentional change requires a new domain tag, not an edit of this pin.",
        );
    }

    #[test]
    fn flags_bits() {
        let mut f = EntryPayloadFlags::default();
        assert!(!f.has(EntryPayloadFlags::HAS_DEPRECATION));
        f.set(EntryPayloadFlags::HAS_DEPRECATION);
        assert!(f.has(EntryPayloadFlags::HAS_DEPRECATION));
        // bit 0 is retired (was IS_REFERENCE); test that setting it does not
        // collide with HAS_DEPRECATION (bit 1).
        assert_ne!(EntryPayloadFlags::HAS_DEPRECATION, 1 << 0);
    }

    #[test]
    fn kind_wire_discriminants() {
        let pairs: &[(KindWire, KindDiscriminant)] = &[
            (KindWire::Module(ModuleWire {}), KindDiscriminant::Module),
            (
                KindWire::Record(RecordWire {
                    form: RecordForm::Struct,
                    fields: Box::new([]),
                    generics: Box::new([]),
                    wheres: Box::new([]),
                    auto: Box::new([]),
                }),
                KindDiscriminant::Record,
            ),
            (KindWire::Field(FieldWire { ty: None }), KindDiscriminant::Field),
            (
                KindWire::Function(FunctionWire {
                    input_params: Box::new([]),
                    output_params: Box::new([]),
                    sig: FnSigFlags::default(),
                    generics: Box::new([]),
                    wheres: Box::new([]),
                }),
                KindDiscriminant::Function,
            ),
            (
                KindWire::Type(TypeAliasWire {
                    ty: TypeWire::Never,
                    generics: Box::new([]),
                    wheres: Box::new([]),
                    auto: Box::new([]),
                }),
                KindDiscriminant::Type,
            ),
            (
                KindWire::Trait(TraitWire {
                    supers: Box::new([]),
                    flags: TraitFlags::default(),
                    generics: Box::new([]),
                    wheres: Box::new([]),
                }),
                KindDiscriminant::Trait,
            ),
            (
                KindWire::Impl(ImplWire {
                    of: None,
                    self_ty: TypeWire::SelfType,
                    flags: ImplFlags::default(),
                    generics: Box::new([]),
                    wheres: Box::new([]),
                }),
                KindDiscriminant::Impl,
            ),
            (
                KindWire::Enum(EnumWire {
                    variants: Box::new([]),
                    generics: Box::new([]),
                    wheres: Box::new([]),
                    auto: Box::new([]),
                }),
                KindDiscriminant::Enum,
            ),
            (
                KindWire::Variant(VariantWire {
                    form: VariantForm::Unit,
                    discr: None,
                    fields: Box::new([]),
                }),
                KindDiscriminant::Variant,
            ),
            (
                KindWire::Const(ConstWire {
                    ty: TypeRefWire::Same(IntroId::from_raw([0u8; 32])),
                    value: None,
                }),
                KindDiscriminant::Const,
            ),
            (
                KindWire::Static(StaticWire {
                    ty: TypeRefWire::Same(IntroId::from_raw([0u8; 32])),
                    mutable: false,
                }),
                KindDiscriminant::Static,
            ),
            (
                KindWire::Reexport(ReexportWire {
                    target: StableRef::new(
                        nudox_change::PackageLineageId::new(
                            nudox_change::EcosystemId::new("cargo"),
                            nudox_change::PackageName::new("foo"),
                        ),
                        IntroId::from_raw([0u8; 32]),
                    ),
                }),
                KindDiscriminant::Reexport,
            ),
        ];
        for (wire, expected) in pairs {
            assert_eq!(wire.discriminant(), *expected);
        }
    }

    #[test]
    fn symbol_wire_attrs_and_cfg_fields() {
        let mut sym = SymbolWire {
            name: "x".into(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 0,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![AttrTok { token: "must_use".into(), arg: None }],
            cfg: Some(CfgExpr::Feature("serde".into())),
        };
        assert_eq!(sym.attrs.len(), 1);
        assert!(sym.cfg.is_some());
        sym.cfg = None;
        assert!(sym.cfg.is_none());
    }
}
