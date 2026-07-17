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

/// Wire form of the type expression attached to a [`KindWire::Type`] entry.
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
// Kind body wires
// ---------------------------------------------------------------------------

/// Wire body for a module entry (currently empty; reserved for future fields).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug, Default)]
pub struct ModuleWire {}

/// Wire body for a record entry: the ordered list of its field intros.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct RecordWire {
    /// Ordered list of field intro IDs (same-package).
    pub fields: Box<[IntroId]>,
}

/// Wire body for a field entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FieldWire {
    /// Field type, if known.
    pub ty: Option<TypeRefWire>,
}

/// Wire body for a function entry.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct FunctionWire {
    /// Input parameters (left-to-right).
    pub input_params: Box<[ParamWire]>,
    /// Output parameters / return types.
    pub output_params: Box<[ParamWire]>,
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
    Type(TypeWire),
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
    pub visibility: u8,
    pub documentation: Option<String>,
    pub source_path: String,
    pub span_start: u32,
    pub span_end: u32,
    pub aliases: Vec<String>,
    pub deprecation: Option<DeprecationWire>,
    pub doc_links: Vec<DocLinkWire>,
}

// ---------------------------------------------------------------------------
// EntryPayloadFlags
// ---------------------------------------------------------------------------

/// Bitmask flags on an [`OwnedEntryPayload`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug, Default)]
pub struct EntryPayloadFlags(pub u8);

impl EntryPayloadFlags {
    /// The entry is a *reference* to another entry (a forwarding alias / re-export).
    pub const IS_REFERENCE: u8 = 1 << 0;
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
/// `(symbol, kind_disc, kind, flags)`. It is used as the `before_hash` field in
/// [`crate::atom::IrAtom::Update`] and [`crate::atom::IrAtom::Delete`] to detect
/// concurrent write conflicts.
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
    /// Domain tag for the v1 payload hash.
    pub const HASH_DOMAIN: &'static str = "nudox.entry.v1";

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

    /// Create a minimal placeholder payload where `payload_hash` is forcibly set
    /// to `known_hash`.
    ///
    /// # Safety (semantic)
    ///
    /// This placeholder is **only valid** as the `entry` field of an inverse
    /// [`crate::atom::IrAtom::Insert`] atom returned by [`crate::atom::IrAtom::inverse`]
    /// on a `Delete`. Applying such a placeholder directly will fail or produce
    /// incorrect state — the actual before-entry must be recovered from the change
    /// `contents` snapshot first.
    pub fn placeholder_for_recovery(known_hash: ContentBlake3) -> Self {
        let symbol = SymbolWire {
            name: String::new(),
            visibility: 0,
            documentation: None,
            source_path: String::new(),
            span_start: 0,
            span_end: 0,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        };
        let kind = KindWire::Module(ModuleWire {});
        let kind_disc = KindDiscriminant::Module;
        let flags = EntryPayloadFlags::default();
        Self { symbol, kind_disc, kind, flags, payload_hash: known_hash }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_hash_is_deterministic() {
        let sym = SymbolWire {
            name: "foo".into(),
            visibility: 0,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 10,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        };
        let kind_disc = KindDiscriminant::Function;
        let kind = KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
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
            visibility: 0,
            documentation: None,
            source_path: "src/main.rs".into(),
            span_start: 5,
            span_end: 50,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        };
        let kind_disc = KindDiscriminant::Module;
        let kind = KindWire::Module(ModuleWire {});
        let flags = EntryPayloadFlags::default();
        let expected = OwnedEntryPayload::compute_payload_hash(&sym, &kind_disc, &kind, &flags);
        let payload = OwnedEntryPayload::sealed(sym, kind_disc, kind, flags);
        assert_eq!(payload.payload_hash, expected);
    }

    #[test]
    fn flags_bits() {
        let mut f = EntryPayloadFlags::default();
        assert!(!f.has(EntryPayloadFlags::IS_REFERENCE));
        f.set(EntryPayloadFlags::IS_REFERENCE);
        assert!(f.has(EntryPayloadFlags::IS_REFERENCE));
        assert!(!f.has(EntryPayloadFlags::HAS_DEPRECATION));
    }
}
