use super::{WireCertificate, WireSchema};
use crate::canonical::{SymbolSchema, decode_id, encode_id};
use crate::semantic_shape::semantic_shape_source_key;
use crate::{
    SemanticArrayShape, SemanticCallableCarrierBindings, SemanticCallableShape,
    SemanticDeclarationIdentity, SemanticDeclarationShape, SemanticLiteral, SemanticObjectMember,
    SemanticPropertyKey, SemanticShapeBatch, SemanticShapeEntry, SemanticShapeFact,
    SemanticShapeImageOrigin, SemanticShapeLanguageFact, SemanticShapeLanguageFacts,
    SemanticShapeMember, SemanticShapeSourceOrigin, SemanticShapeUnavailable, SemanticTypeElement,
    SemanticTypeExpr, SemanticTypeFact, SemanticTypeUnavailable, SymbolAddress,
};
use backend_semantic::ir::{
    BuiltinType, CSharpNullability, CSharpReferenceKind, ChannelDirection, Confidence,
    FunctionVariadicForm, ItemKind, PythonParameterKind, RustOwnership, TupleElementKind, TypeTag,
    UnknownReason,
};
use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding, ObjectKey};
use serde::{Deserialize, Serialize};

macro_rules! closed_wire_discriminant {
    ($wire:ident, $semantic:ty, u8, { $($code:literal => $variant:path),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Serialize, Deserialize)]
        #[serde(try_from = "u8", into = "u8")]
        struct $wire($semantic);

        impl From<$semantic> for $wire {
            fn from(value: $semantic) -> Self { Self(value) }
        }
        impl From<$wire> for u8 {
            fn from(value: $wire) -> Self {
                match value.0 { $($variant => $code,)+ }
            }
        }
        impl TryFrom<u8> for $wire {
            type Error = String;
            fn try_from(value: u8) -> Result<Self, Self::Error> {
                match value {
                    $($code => Ok(Self($variant)),)+
                    _ => Err(format!("invalid {} discriminant {value}", stringify!($wire))),
                }
            }
        }
    };
    ($wire:ident, $semantic:ty, u16, { $($code:literal => $variant:path),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Serialize, Deserialize)]
        #[serde(try_from = "u16", into = "u16")]
        struct $wire($semantic);

        impl From<$semantic> for $wire {
            fn from(value: $semantic) -> Self { Self(value) }
        }
        impl From<$wire> for u16 {
            fn from(value: $wire) -> Self {
                match value.0 { $($variant => $code,)+ }
            }
        }
        impl TryFrom<u16> for $wire {
            type Error = String;
            fn try_from(value: u16) -> Result<Self, Self::Error> {
                match value {
                    $($code => Ok(Self($variant)),)+
                    _ => Err(format!("invalid {} discriminant {value}", stringify!($wire))),
                }
            }
        }
    };
}

closed_wire_discriminant!(BuiltinTypeWire, BuiltinType, u8, {
    0 => BuiltinType::Unit, 1 => BuiltinType::Never, 2 => BuiltinType::Bool,
    3 => BuiltinType::LegacyChar, 4 => BuiltinType::I8, 5 => BuiltinType::I16,
    6 => BuiltinType::I32, 7 => BuiltinType::I64, 8 => BuiltinType::I128,
    9 => BuiltinType::U8, 10 => BuiltinType::U16, 11 => BuiltinType::U32,
    12 => BuiltinType::U64, 13 => BuiltinType::U128, 14 => BuiltinType::F16,
    15 => BuiltinType::F32, 16 => BuiltinType::F64, 17 => BuiltinType::String,
    18 => BuiltinType::Bytes, 19 => BuiltinType::Object, 20 => BuiltinType::Any,
    21 => BuiltinType::Unknown, 22 => BuiltinType::Void, 23 => BuiltinType::Number,
    24 => BuiltinType::BigInt, 25 => BuiltinType::Symbol, 26 => BuiltinType::UniqueSymbol,
    27 => BuiltinType::Null, 28 => BuiltinType::Undefined, 30 => BuiltinType::None_,
    31 => BuiltinType::List, 32 => BuiltinType::Dict, 33 => BuiltinType::Set,
    34 => BuiltinType::FrozenSet, 36 => BuiltinType::Complex, 37 => BuiltinType::Decimal,
    38 => BuiltinType::ArbitraryInteger, 39 => BuiltinType::NativeSignedInteger,
    40 => BuiltinType::NativeUnsignedInteger, 41 => BuiltinType::PointerAddressInteger,
});
closed_wire_discriminant!(TypeTagWire, TypeTag, u8, {
    0 => TypeTag::Builtin, 1 => TypeTag::Literal, 2 => TypeTag::Nominal,
    3 => TypeTag::External, 4 => TypeTag::Parameter, 5 => TypeTag::Applied,
    6 => TypeTag::Tuple, 7 => TypeTag::Object, 8 => TypeTag::Function,
    9 => TypeTag::Reference, 10 => TypeTag::Pointer, 11 => TypeTag::Slice,
    12 => TypeTag::Array, 13 => TypeTag::Optional, 14 => TypeTag::Union,
    15 => TypeTag::Intersection, 16 => TypeTag::KeyOf, 17 => TypeTag::TypeOf,
    18 => TypeTag::IndexedAccess, 19 => TypeTag::Conditional, 20 => TypeTag::Mapped,
    21 => TypeTag::Infer, 22 => TypeTag::TemplateLiteral, 23 => TypeTag::Import,
    24 => TypeTag::Awaited, 25 => TypeTag::This, 26 => TypeTag::Unknown,
    27 => TypeTag::ImplTrait, 28 => TypeTag::DynTrait, 29 => TypeTag::Wildcard,
    30 => TypeTag::Annotated, 31 => TypeTag::Inferred, 32 => TypeTag::QualifiedPath,
    33 => TypeTag::Map, 34 => TypeTag::Channel, 35 => TypeTag::CxxReference,
    36 => TypeTag::CPointer, 37 => TypeTag::CxxMemberPointer, 38 => TypeTag::CQualified,
    39 => TypeTag::CBlockPointer, 40 => TypeTag::NativeCharacter,
});
closed_wire_discriminant!(UnknownReasonWire, UnknownReason, u8, {
    0 => UnknownReason::Unannotated, 1 => UnknownReason::DynamicallyTyped,
    2 => UnknownReason::UnresolvedLocalName, 3 => UnknownReason::UnresolvedExternal,
    4 => UnknownReason::TruncatedAtDepthLimit, 5 => UnknownReason::OracleGap,
    6 => UnknownReason::NoIrRepresentation, 7 => UnknownReason::Error,
});
closed_wire_discriminant!(VariadicFormWire, FunctionVariadicForm, u8, {
    0 => FunctionVariadicForm::None, 1 => FunctionVariadicForm::TypedLast,
    2 => FunctionVariadicForm::CUnbounded,
});
closed_wire_discriminant!(TupleElementKindWire, TupleElementKind, u8, {
    0 => TupleElementKind::Required, 1 => TupleElementKind::Optional,
    2 => TupleElementKind::Rest,
});
closed_wire_discriminant!(ChannelDirectionWire, ChannelDirection, u8, {
    0 => ChannelDirection::Both, 1 => ChannelDirection::Send,
    2 => ChannelDirection::Receive,
});
closed_wire_discriminant!(RustOwnershipWire, RustOwnership, u8, {
    0 => RustOwnership::Value, 1 => RustOwnership::SharedBorrow,
    2 => RustOwnership::MutableBorrow, 3 => RustOwnership::Moved,
});
closed_wire_discriminant!(PythonParameterKindWire, PythonParameterKind, u8, {
    0 => PythonParameterKind::PositionalOnly, 1 => PythonParameterKind::PositionalOrKeyword,
    2 => PythonParameterKind::VariadicPositional, 3 => PythonParameterKind::KeywordOnly,
    4 => PythonParameterKind::VariadicKeyword,
});
closed_wire_discriminant!(ConfidenceWire, Confidence, u8, {
    0 => Confidence::Syntactic, 1 => Confidence::Heuristic, 2 => Confidence::Indexed,
    3 => Confidence::Imported, 4 => Confidence::Compiler,
});
closed_wire_discriminant!(CSharpNullabilityWire, CSharpNullability, u8, {
    0 => CSharpNullability::Oblivious, 1 => CSharpNullability::NonNullable,
    2 => CSharpNullability::Nullable,
});
closed_wire_discriminant!(CSharpReferenceKindWire, CSharpReferenceKind, u8, {
    0 => CSharpReferenceKind::Value, 1 => CSharpReferenceKind::In,
    2 => CSharpReferenceKind::Ref, 3 => CSharpReferenceKind::Out,
});
closed_wire_discriminant!(ItemKindWire, ItemKind, u16, {
    0 => ItemKind::Function, 1 => ItemKind::Constant, 2 => ItemKind::Record,
    3 => ItemKind::Module, 4 => ItemKind::Field, 5 => ItemKind::Alias,
    6 => ItemKind::Trait, 7 => ItemKind::Implementation, 8 => ItemKind::Enum,
    9 => ItemKind::Variant, 10 => ItemKind::Static, 11 => ItemKind::Reexport,
    12 => ItemKind::Parameter, 13 => ItemKind::Macro, 14 => ItemKind::Namespace,
});

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticShapeBatchWire {
    basis: String,
    entries: Vec<SemanticShapeEntryWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticShapeEntryWire {
    symbol: SymbolAddressWire,
    identity: Option<SemanticDeclarationIdentity>,
    origin: Option<SemanticShapeSourceOriginWire>,
    fact: SemanticShapeFactWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticShapeSourceOriginWire {
    source: crate::SemanticShapeSelection,
    selection_root: [u8; 32],
    image: Option<SemanticShapeImageOriginWire>,
    source_commitment: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticShapeImageOriginWire {
    image_identity: String,
    byte_len: u32,
    profile: crate::SemanticLanguageProfile,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SymbolAddressKindWire {
    Canonical,
    Selected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolAddressWire {
    kind: SymbolAddressKindWire,
    id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticShapeFactWire {
    Available {
        shape: SemanticDeclarationShapeWire,
        language: SemanticShapeLanguageFactsWire,
    },
    Unknown {
        reason: UnknownReasonWire,
        spelling: Option<crate::SourceAtomText>,
    },
    Unsupported {
        tag: TypeTagWire,
    },
    Unavailable(SemanticShapeUnavailable),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticDeclarationShapeWire {
    Callable(SemanticCallableShapeWire),
    Aggregate(Vec<SemanticShapeMemberWire>),
    Typed(SemanticTypeFactWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticCallableShapeWire {
    parameters: Vec<SemanticTypeElementWire>,
    results: Vec<SemanticTypeElementWire>,
    carrier_bindings: SemanticCallableCarrierBindingsWire,
    abi: Option<crate::SourceAtomText>,
    variadic: VariadicFormWire,
    unsafe_: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "capture",
    content = "bindings",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticCallableCarrierBindingsWire {
    Unavailable,
    Captured {
        parameters: Vec<SemanticDeclarationIdentity>,
        results: Vec<SemanticDeclarationIdentity>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticTypeElementWire {
    label: Option<crate::SourceAtomText>,
    kind: TupleElementKindWire,
    ty: SemanticTypeFactWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticShapeMemberWire {
    identity: SemanticDeclarationIdentity,
    name: crate::SourceAtomText,
    kind: ItemKindWire,
    ty: SemanticTypeFactWire,
    language: SemanticShapeLanguageFactsWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "coverage",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticShapeLanguageFactsWire {
    Unavailable {
        profile: crate::SemanticLanguageProfile,
    },
    CommonOnly {
        profile: crate::SemanticLanguageProfile,
    },
    Partial {
        profile: crate::SemanticLanguageProfile,
        facts: SemanticShapeLanguageFactWire,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticShapeLanguageFactWire {
    RustOwnership(RustOwnershipWire),
    GoVariadic(bool),
    PythonParameter {
        kind: PythonParameterKindWire,
        confidence: ConfidenceWire,
    },
    CSharp {
        nullability: CSharpNullabilityWire,
        reference_kind: CSharpReferenceKindWire,
        is_async: bool,
        is_iterator: bool,
        is_extension: bool,
    },
    TypeScript {
        declared: Option<Box<SemanticTypeFactWire>>,
        observed: Option<Box<SemanticTypeFactWire>>,
    },
    Java {
        throws: Vec<SemanticTypeFactWire>,
        annotations: Vec<crate::SourceAtomText>,
    },
    Clang {
        is_const: bool,
        is_volatile: bool,
        is_restrict: bool,
        size_bits: Option<u32>,
        align_bits: Option<u32>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticTypeFactWire {
    Known(SemanticTypeExprWire),
    Unknown {
        reason: UnknownReasonWire,
        spelling: Option<crate::SourceAtomText>,
    },
    Unsupported {
        tag: TypeTagWire,
    },
    Unavailable(SemanticTypeUnavailable),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticTypeExprWire {
    Builtin(BuiltinTypeWire),
    Literal(SemanticLiteralWire),
    Nominal {
        declaration: SemanticDeclarationIdentity,
        symbol: Option<SymbolAddressWire>,
    },
    External {
        identity: [u8; 32],
        display: Option<crate::SourceAtomText>,
    },
    Parameter(crate::SourceAtomText),
    Applied {
        constructor: Box<SemanticTypeFactWire>,
        arguments: Vec<SemanticTypeFactWire>,
    },
    Tuple(Vec<SemanticTypeElementWire>),
    Object(Vec<SemanticObjectMemberWire>),
    Function(Box<SemanticCallableShapeWire>),
    Reference {
        target: Box<SemanticTypeFactWire>,
        mutable: bool,
        lifetime: Option<crate::SourceAtomText>,
    },
    Pointer {
        target: Box<SemanticTypeFactWire>,
        mutable: bool,
    },
    Slice(Box<SemanticTypeFactWire>),
    Array {
        element: Box<SemanticTypeFactWire>,
        shape: SemanticArrayShapeWire,
    },
    Optional(Box<SemanticTypeFactWire>),
    Union(Vec<SemanticTypeFactWire>),
    Intersection(Vec<SemanticTypeFactWire>),
    Map {
        key: Box<SemanticTypeFactWire>,
        value: Box<SemanticTypeFactWire>,
    },
    Channel {
        direction: ChannelDirectionWire,
        element: Box<SemanticTypeFactWire>,
    },
    Unsupported {
        tag: TypeTagWire,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticLiteralWire {
    String(crate::SourceAtomText),
    Number(crate::SourceAtomText),
    BigInt(crate::SourceAtomText),
    Boolean(bool),
    Null,
    Undefined,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticArrayShapeWire {
    Sequence,
    Rectangular { rank: u16 },
    FixedValue { length: u64 },
    ConstExpression(crate::SourceAtomText),
    Incomplete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticPropertyKeyWire {
    Named(crate::SourceAtomText),
    Private(crate::SourceAtomText),
    Numeric(crate::SourceAtomText),
    Computed(Box<SemanticTypeFactWire>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SemanticObjectMemberWire {
    Property {
        key: SemanticPropertyKeyWire,
        ty: SemanticTypeFactWire,
        optional: bool,
        readonly: bool,
    },
    Method {
        key: SemanticPropertyKeyWire,
        signature: SemanticTypeFactWire,
        optional: bool,
    },
    Index {
        parameter: crate::SourceAtomText,
        key: SemanticTypeFactWire,
        value: SemanticTypeFactWire,
        readonly: bool,
    },
    Call(SemanticTypeFactWire),
    Construct(SemanticTypeFactWire),
}

pub(crate) fn semantic_shape_batch_to_wire(
    batch: &SemanticShapeBatch,
) -> Result<SemanticShapeBatchWire, String> {
    // Traverse the owned product before cloning it into the transport DTO.
    // The wire preflight below then independently checks the serialized tree's
    // own node/depth/byte bounds.
    batch
        .admission_summary()
        .map_err(|error| error.to_string())?;
    Ok(SemanticShapeBatchWire {
        basis: encode_id(batch.basis.as_bytes()),
        entries: batch.entries.iter().map(entry_to_wire).collect(),
    })
}

/// Computes the owner-attested commitment to the complete projected batch.
/// The receiver repeats this exact conversion and digest before accepting the
/// reply certificate; it is a payload-integrity commitment, not an image-byte
/// derivation proof.
pub fn semantic_shape_batch_key(
    batch: &SemanticShapeBatch,
) -> Result<ObjectKey<crate::SemanticShapeBatchSchema>, String> {
    let wire = semantic_shape_batch_to_wire(batch)?;
    let preimage = admit_shape_wire_tree(&wire)?;
    Ok(ObjectKey::from_value(&preimage))
}

pub(crate) fn semantic_shape_batch_from_wire(
    value: SemanticShapeBatchWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeBatch, String> {
    let preimage = admit_shape_wire_tree(&value)?;
    let batch_key = ObjectKey::<crate::SemanticShapeBatchSchema>::from_value(&preimage);
    certificate.key_commitment(
        crate::WireSchema::SemanticShapeBatch,
        &encode_id(batch_key.as_bytes()),
    )?;
    let basis = certificate
        .root_commitment_bytes::<crate::canonical::ViewRelation>(
            WireSchema::ViewRelation,
            &value.basis,
        )
        .map(crate::ViewRevision::from_bytes)?;
    let entries = value
        .entries
        .iter()
        .map(|entry| entry_from_wire(entry, certificate))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    Ok(SemanticShapeBatch { basis, entries })
}

pub(super) fn admit_shape_wire_tree(batch: &SemanticShapeBatchWire) -> Result<Vec<u8>, String> {
    if batch.entries.is_empty() || batch.entries.len() > crate::MAX_SEMANTIC_SHAPE_BATCH {
        return Err("semantic-shape wire batch exceeds its fixed declaration bound".to_owned());
    }
    let mut nodes = 0usize;
    for entry in &batch.entries {
        shape_wire_node(&mut nodes)?;
        shape_wire_fact(&entry.fact, 0, &mut nodes)?;
    }
    bounded_shape_json(batch)
}

fn bounded_shape_json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    struct BoundedWriter(Vec<u8>);

    impl std::io::Write for BoundedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let total = self
                .0
                .len()
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("semantic-shape wire size overflowed"))?;
            if total > crate::MAX_SEMANTIC_SHAPE_BYTES {
                return Err(std::io::Error::other(
                    "semantic-shape wire payload exceeds its fixed byte bound",
                ));
            }
            self.0
                .try_reserve(bytes.len())
                .map_err(std::io::Error::other)?;
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut output = BoundedWriter(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|error| error.to_string())?;
    Ok(output.0)
}

fn shape_wire_node(nodes: &mut usize) -> Result<(), String> {
    *nodes = nodes
        .checked_add(1)
        .ok_or_else(|| "semantic-shape wire node count overflowed".to_owned())?;
    if *nodes > crate::MAX_SEMANTIC_SHAPE_NODES {
        return Err("semantic-shape wire graph exceeds its fixed node bound".to_owned());
    }
    Ok(())
}

fn shape_wire_fact(
    fact: &SemanticShapeFactWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > crate::MAX_SEMANTIC_SHAPE_DEPTH {
        return Err("semantic-shape wire graph exceeds its fixed depth bound".to_owned());
    }
    shape_wire_node(nodes)?;
    match fact {
        SemanticShapeFactWire::Available { shape, language } => {
            shape_wire_language(language, 0, nodes)?;
            match shape {
                SemanticDeclarationShapeWire::Callable(callable) => {
                    shape_wire_callable(callable, 0, nodes)?;
                }
                SemanticDeclarationShapeWire::Aggregate(members) => {
                    for member in members {
                        shape_wire_node(nodes)?;
                        shape_wire_language(&member.language, 0, nodes)?;
                        shape_wire_type_fact(&member.ty, 0, nodes)?;
                    }
                }
                SemanticDeclarationShapeWire::Typed(ty) => {
                    shape_wire_type_fact(ty, 0, nodes)?;
                }
            }
        }
        SemanticShapeFactWire::Unknown { .. }
        | SemanticShapeFactWire::Unsupported { .. }
        | SemanticShapeFactWire::Unavailable(_) => {}
    }
    Ok(())
}

fn shape_wire_language(
    language: &SemanticShapeLanguageFactsWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    shape_wire_node(nodes)?;
    match language {
        SemanticShapeLanguageFactsWire::Partial { facts, .. } => match facts {
            SemanticShapeLanguageFactWire::TypeScript { declared, observed } => {
                for fact in declared.iter().chain(observed.iter()) {
                    shape_wire_type_fact(fact, depth, nodes)?;
                }
            }
            SemanticShapeLanguageFactWire::Java {
                throws,
                annotations,
            } => {
                for fact in throws {
                    shape_wire_type_fact(fact, depth, nodes)?;
                }
                for _ in annotations {
                    shape_wire_node(nodes)?;
                }
            }
            SemanticShapeLanguageFactWire::RustOwnership(_)
            | SemanticShapeLanguageFactWire::GoVariadic(_)
            | SemanticShapeLanguageFactWire::PythonParameter { .. }
            | SemanticShapeLanguageFactWire::CSharp { .. }
            | SemanticShapeLanguageFactWire::Clang { .. } => {}
        },
        SemanticShapeLanguageFactsWire::Unavailable { .. }
        | SemanticShapeLanguageFactsWire::CommonOnly { .. } => {}
    }
    Ok(())
}

fn shape_wire_callable(
    callable: &SemanticCallableShapeWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    match &callable.carrier_bindings {
        SemanticCallableCarrierBindingsWire::Unavailable => {}
        SemanticCallableCarrierBindingsWire::Captured {
            parameters,
            results,
        } => {
            if parameters.len() != callable.parameters.len()
                || results.len() != callable.results.len()
            {
                return Err(
                    "semantic-shape callable carrier identities do not align with IR tuple cells"
                        .to_owned(),
                );
            }
            let bindings = parameters
                .len()
                .checked_add(results.len())
                .ok_or_else(|| "semantic-shape callable carrier count overflowed".to_owned())?;
            let total = nodes
                .checked_add(bindings)
                .ok_or_else(|| "semantic-shape wire node count overflowed".to_owned())?;
            if total > crate::MAX_SEMANTIC_SHAPE_NODES {
                return Err("semantic-shape wire graph exceeds its fixed node bound".to_owned());
            }
            *nodes = total;
        }
    }
    for element in callable.parameters.iter().chain(callable.results.iter()) {
        shape_wire_element(element, depth, nodes)?;
    }
    Ok(())
}

fn shape_wire_element(
    element: &SemanticTypeElementWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    shape_wire_node(nodes)?;
    shape_wire_type_fact(&element.ty, depth, nodes)
}

fn shape_wire_type_fact(
    fact: &SemanticTypeFactWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > crate::MAX_SEMANTIC_SHAPE_DEPTH {
        return Err("semantic-shape wire graph exceeds its fixed depth bound".to_owned());
    }
    shape_wire_node(nodes)?;
    if let SemanticTypeFactWire::Known(expression) = fact {
        shape_wire_expr(expression, depth, nodes)?;
    }
    Ok(())
}

fn shape_wire_expr(
    expression: &SemanticTypeExprWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > crate::MAX_SEMANTIC_SHAPE_DEPTH {
        return Err("semantic-shape wire graph exceeds its fixed depth bound".to_owned());
    }
    shape_wire_node(nodes)?;
    match expression {
        SemanticTypeExprWire::Applied {
            constructor,
            arguments,
        } => {
            shape_wire_type_fact(constructor, depth + 1, nodes)?;
            for argument in arguments {
                shape_wire_type_fact(argument, depth + 1, nodes)?;
            }
        }
        SemanticTypeExprWire::Tuple(elements) => {
            for element in elements {
                shape_wire_element(element, depth + 1, nodes)?;
            }
        }
        SemanticTypeExprWire::Object(members) => {
            for member in members {
                shape_wire_node(nodes)?;
                match member {
                    SemanticObjectMemberWire::Property { key, ty, .. } => {
                        shape_wire_property_key(key, depth + 1, nodes)?;
                        shape_wire_type_fact(ty, depth + 1, nodes)?;
                    }
                    SemanticObjectMemberWire::Method { key, signature, .. } => {
                        shape_wire_property_key(key, depth + 1, nodes)?;
                        shape_wire_type_fact(signature, depth + 1, nodes)?;
                    }
                    SemanticObjectMemberWire::Index { key, value, .. } => {
                        shape_wire_type_fact(key, depth + 1, nodes)?;
                        shape_wire_type_fact(value, depth + 1, nodes)?;
                    }
                    SemanticObjectMemberWire::Call(signature)
                    | SemanticObjectMemberWire::Construct(signature) => {
                        shape_wire_type_fact(signature, depth + 1, nodes)?;
                    }
                }
            }
        }
        SemanticTypeExprWire::Function(callable) => {
            shape_wire_callable(callable, depth + 1, nodes)?;
        }
        SemanticTypeExprWire::Reference { target, .. }
        | SemanticTypeExprWire::Pointer { target, .. }
        | SemanticTypeExprWire::Slice(target)
        | SemanticTypeExprWire::Optional(target) => {
            shape_wire_type_fact(target, depth + 1, nodes)?;
        }
        SemanticTypeExprWire::Array { element, shape } => {
            if matches!(shape, SemanticArrayShapeWire::Rectangular { rank: 0 }) {
                return Err("rectangular array rank must be nonzero".to_owned());
            }
            shape_wire_type_fact(element, depth + 1, nodes)?;
        }
        SemanticTypeExprWire::Union(items) | SemanticTypeExprWire::Intersection(items) => {
            for item in items {
                shape_wire_type_fact(item, depth + 1, nodes)?;
            }
        }
        SemanticTypeExprWire::Map { key, value } => {
            shape_wire_type_fact(key, depth + 1, nodes)?;
            shape_wire_type_fact(value, depth + 1, nodes)?;
        }
        SemanticTypeExprWire::Channel { element, .. } => {
            shape_wire_type_fact(element, depth + 1, nodes)?;
        }
        SemanticTypeExprWire::Builtin(_)
        | SemanticTypeExprWire::Literal(_)
        | SemanticTypeExprWire::Nominal { .. }
        | SemanticTypeExprWire::External { .. }
        | SemanticTypeExprWire::Parameter(_)
        | SemanticTypeExprWire::Unsupported { .. } => {}
    }
    Ok(())
}

fn shape_wire_property_key(
    key: &SemanticPropertyKeyWire,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if let SemanticPropertyKeyWire::Computed(fact) = key {
        shape_wire_type_fact(fact, depth, nodes)?;
    }
    Ok(())
}

fn entry_to_wire(entry: &SemanticShapeEntry) -> SemanticShapeEntryWire {
    SemanticShapeEntryWire {
        symbol: symbol_address_to_wire(entry.symbol),
        identity: entry.identity,
        origin: entry.origin.as_ref().map(origin_to_wire),
        fact: fact_to_wire(&entry.fact),
    }
}

fn entry_from_wire(
    value: &SemanticShapeEntryWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeEntry, String> {
    Ok(SemanticShapeEntry {
        symbol: symbol_address_from_wire(&value.symbol, certificate)?,
        identity: value.identity,
        origin: value
            .origin
            .as_ref()
            .map(|origin| origin_from_wire(origin, certificate))
            .transpose()?,
        fact: fact_from_wire(&value.fact, certificate)?,
    })
}

fn origin_to_wire(origin: &SemanticShapeSourceOrigin) -> SemanticShapeSourceOriginWire {
    let source_key = semantic_shape_source_key(origin);
    SemanticShapeSourceOriginWire {
        source: origin.source.clone(),
        selection_root: origin.selection_root,
        image: origin.image.map(|image| SemanticShapeImageOriginWire {
            image_identity: encode_id(image.image.identity.as_ref()),
            byte_len: image.image.byte_len,
            profile: crate::SemanticLanguageProfile::new(image.profile),
        }),
        source_commitment: encode_id(source_key.as_bytes()),
    }
}

fn origin_from_wire(
    origin: &SemanticShapeSourceOriginWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeSourceOrigin, String> {
    let admitted = SemanticShapeSourceOrigin {
        source: origin.source.clone(),
        selection_root: origin.selection_root,
        image: origin
            .image
            .as_ref()
            .map(|image| {
                let bytes = decode_id(&image.image_identity).map_err(|error| error.to_string())?;
                let profile = image.profile.profile().map_err(|error| error.to_string())?;
                let identity =
                    ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::try_from(
                        bytes.as_slice(),
                    )
                    .map_err(|error| error.to_string())?;
                Ok(SemanticShapeImageOrigin {
                    image: crate::interface::SemanticImageAuthority {
                        identity,
                        byte_len: image.byte_len,
                    },
                    profile,
                })
            })
            .transpose()?,
    };
    let source_key = semantic_shape_source_key(&admitted);
    if encode_id(source_key.as_bytes()) != origin.source_commitment {
        return Err("semantic-shape source witness commitment does not match".to_owned());
    }
    certificate.key_bytes::<crate::SemanticShapeSourceSchema>(
        crate::WireSchema::SemanticShapeSource,
        &origin.source_commitment,
    )?;
    Ok(admitted)
}

fn fact_to_wire(fact: &SemanticShapeFact) -> SemanticShapeFactWire {
    match fact {
        SemanticShapeFact::Available { shape, language } => SemanticShapeFactWire::Available {
            shape: shape_to_wire(shape),
            language: language_to_wire(language),
        },
        SemanticShapeFact::Unknown { reason, spelling } => SemanticShapeFactWire::Unknown {
            reason: (*reason).into(),
            spelling: spelling.clone(),
        },
        SemanticShapeFact::Unsupported { tag } => {
            SemanticShapeFactWire::Unsupported { tag: (*tag).into() }
        }
        SemanticShapeFact::Unavailable(reason) => SemanticShapeFactWire::Unavailable(*reason),
    }
}

fn fact_from_wire(
    fact: &SemanticShapeFactWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeFact, String> {
    Ok(match fact {
        SemanticShapeFactWire::Available { shape, language } => SemanticShapeFact::Available {
            shape: shape_from_wire(shape, certificate)?,
            language: language_from_wire(language, certificate)?,
        },
        SemanticShapeFactWire::Unknown { reason, spelling } => SemanticShapeFact::Unknown {
            reason: reason.0,
            spelling: spelling.clone(),
        },
        SemanticShapeFactWire::Unsupported { tag } => SemanticShapeFact::Unsupported { tag: tag.0 },
        SemanticShapeFactWire::Unavailable(reason) => SemanticShapeFact::Unavailable(*reason),
    })
}

fn shape_to_wire(shape: &SemanticDeclarationShape) -> SemanticDeclarationShapeWire {
    match shape {
        SemanticDeclarationShape::Callable(callable) => {
            SemanticDeclarationShapeWire::Callable(callable_to_wire(callable))
        }
        SemanticDeclarationShape::Aggregate(members) => SemanticDeclarationShapeWire::Aggregate(
            members
                .iter()
                .map(|member| SemanticShapeMemberWire {
                    identity: member.identity,
                    name: member.name.clone(),
                    kind: member.kind.into(),
                    ty: type_fact_to_wire(&member.ty),
                    language: language_to_wire(&member.language),
                })
                .collect(),
        ),
        SemanticDeclarationShape::Typed(ty) => {
            SemanticDeclarationShapeWire::Typed(type_fact_to_wire(ty))
        }
    }
}

fn shape_from_wire(
    shape: &SemanticDeclarationShapeWire,
    certificate: &WireCertificate,
) -> Result<SemanticDeclarationShape, String> {
    Ok(match shape {
        SemanticDeclarationShapeWire::Callable(callable) => {
            SemanticDeclarationShape::Callable(callable_from_wire(callable, certificate)?)
        }
        SemanticDeclarationShapeWire::Aggregate(members) => SemanticDeclarationShape::Aggregate(
            members
                .iter()
                .map(|member| {
                    Ok(SemanticShapeMember {
                        identity: member.identity,
                        name: member.name.clone(),
                        kind: member.kind.0,
                        ty: type_fact_from_wire(&member.ty, certificate)?,
                        language: language_from_wire(&member.language, certificate)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
                .into_boxed_slice(),
        ),
        SemanticDeclarationShapeWire::Typed(ty) => {
            SemanticDeclarationShape::Typed(type_fact_from_wire(ty, certificate)?)
        }
    })
}

fn callable_to_wire(callable: &SemanticCallableShape) -> SemanticCallableShapeWire {
    SemanticCallableShapeWire {
        parameters: callable.parameters.iter().map(element_to_wire).collect(),
        results: callable.results.iter().map(element_to_wire).collect(),
        carrier_bindings: carrier_bindings_to_wire(&callable.carrier_bindings),
        abi: callable.abi.clone(),
        variadic: callable.variadic.into(),
        unsafe_: callable.unsafe_,
    }
}

fn callable_from_wire(
    callable: &SemanticCallableShapeWire,
    certificate: &WireCertificate,
) -> Result<SemanticCallableShape, String> {
    let carrier_bindings = carrier_bindings_from_wire(&callable.carrier_bindings);
    if let SemanticCallableCarrierBindings::Captured {
        parameters,
        results,
    } = &carrier_bindings
    {
        if parameters.len() != callable.parameters.len() || results.len() != callable.results.len()
        {
            return Err(
                "semantic-shape callable carrier identities do not align with IR tuple cells"
                    .to_owned(),
            );
        }
    }
    Ok(SemanticCallableShape {
        parameters: callable
            .parameters
            .iter()
            .map(|element| element_from_wire(element, certificate))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice(),
        results: callable
            .results
            .iter()
            .map(|element| element_from_wire(element, certificate))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice(),
        carrier_bindings,
        abi: callable.abi.clone(),
        variadic: callable.variadic.0,
        unsafe_: callable.unsafe_,
    })
}

fn carrier_bindings_to_wire(
    bindings: &SemanticCallableCarrierBindings,
) -> SemanticCallableCarrierBindingsWire {
    match bindings {
        SemanticCallableCarrierBindings::Unavailable => {
            SemanticCallableCarrierBindingsWire::Unavailable
        }
        SemanticCallableCarrierBindings::Captured {
            parameters,
            results,
        } => SemanticCallableCarrierBindingsWire::Captured {
            parameters: parameters.to_vec(),
            results: results.to_vec(),
        },
    }
}

fn carrier_bindings_from_wire(
    bindings: &SemanticCallableCarrierBindingsWire,
) -> SemanticCallableCarrierBindings {
    match bindings {
        SemanticCallableCarrierBindingsWire::Unavailable => {
            SemanticCallableCarrierBindings::Unavailable
        }
        SemanticCallableCarrierBindingsWire::Captured {
            parameters,
            results,
        } => SemanticCallableCarrierBindings::Captured {
            parameters: parameters.clone().into_boxed_slice(),
            results: results.clone().into_boxed_slice(),
        },
    }
}

fn element_to_wire(element: &SemanticTypeElement) -> SemanticTypeElementWire {
    SemanticTypeElementWire {
        label: element.label.clone(),
        kind: element.kind.into(),
        ty: type_fact_to_wire(&element.ty),
    }
}

fn element_from_wire(
    element: &SemanticTypeElementWire,
    certificate: &WireCertificate,
) -> Result<SemanticTypeElement, String> {
    Ok(SemanticTypeElement {
        label: element.label.clone(),
        kind: element.kind.0,
        ty: type_fact_from_wire(&element.ty, certificate)?,
    })
}

fn language_to_wire(language: &SemanticShapeLanguageFacts) -> SemanticShapeLanguageFactsWire {
    match language {
        SemanticShapeLanguageFacts::Unavailable { profile } => {
            SemanticShapeLanguageFactsWire::Unavailable {
                profile: crate::SemanticLanguageProfile::new(*profile),
            }
        }
        SemanticShapeLanguageFacts::CommonOnly { profile } => {
            SemanticShapeLanguageFactsWire::CommonOnly {
                profile: crate::SemanticLanguageProfile::new(*profile),
            }
        }
        SemanticShapeLanguageFacts::Partial { profile, facts } => {
            SemanticShapeLanguageFactsWire::Partial {
                profile: crate::SemanticLanguageProfile::new(*profile),
                facts: language_fact_to_wire(facts),
            }
        }
    }
}

fn language_from_wire(
    language: &SemanticShapeLanguageFactsWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeLanguageFacts, String> {
    Ok(match language {
        SemanticShapeLanguageFactsWire::Unavailable { profile } => {
            SemanticShapeLanguageFacts::Unavailable {
                profile: profile.profile().map_err(|error| error.to_string())?,
            }
        }
        SemanticShapeLanguageFactsWire::CommonOnly { profile } => {
            SemanticShapeLanguageFacts::CommonOnly {
                profile: profile.profile().map_err(|error| error.to_string())?,
            }
        }
        SemanticShapeLanguageFactsWire::Partial { profile, facts } => {
            SemanticShapeLanguageFacts::Partial {
                profile: profile.profile().map_err(|error| error.to_string())?,
                facts: language_fact_from_wire(facts, certificate)?,
            }
        }
    })
}

fn language_fact_to_wire(facts: &SemanticShapeLanguageFact) -> SemanticShapeLanguageFactWire {
    match facts {
        SemanticShapeLanguageFact::RustOwnership(value) => {
            SemanticShapeLanguageFactWire::RustOwnership((*value).into())
        }
        SemanticShapeLanguageFact::GoVariadic(value) => {
            SemanticShapeLanguageFactWire::GoVariadic(*value)
        }
        SemanticShapeLanguageFact::PythonParameter { kind, confidence } => {
            SemanticShapeLanguageFactWire::PythonParameter {
                kind: (*kind).into(),
                confidence: (*confidence).into(),
            }
        }
        SemanticShapeLanguageFact::CSharp {
            nullability,
            reference_kind,
            is_async,
            is_iterator,
            is_extension,
        } => SemanticShapeLanguageFactWire::CSharp {
            nullability: (*nullability).into(),
            reference_kind: (*reference_kind).into(),
            is_async: *is_async,
            is_iterator: *is_iterator,
            is_extension: *is_extension,
        },
        SemanticShapeLanguageFact::TypeScript { declared, observed } => {
            SemanticShapeLanguageFactWire::TypeScript {
                declared: declared
                    .as_ref()
                    .map(|fact| Box::new(type_fact_to_wire(fact))),
                observed: observed
                    .as_ref()
                    .map(|fact| Box::new(type_fact_to_wire(fact))),
            }
        }
        SemanticShapeLanguageFact::Java {
            throws,
            annotations,
        } => SemanticShapeLanguageFactWire::Java {
            throws: throws.iter().map(type_fact_to_wire).collect(),
            annotations: annotations.to_vec(),
        },
        SemanticShapeLanguageFact::Clang {
            is_const,
            is_volatile,
            is_restrict,
            size_bits,
            align_bits,
        } => SemanticShapeLanguageFactWire::Clang {
            is_const: *is_const,
            is_volatile: *is_volatile,
            is_restrict: *is_restrict,
            size_bits: *size_bits,
            align_bits: *align_bits,
        },
    }
}

fn language_fact_from_wire(
    facts: &SemanticShapeLanguageFactWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeLanguageFact, String> {
    Ok(match facts {
        SemanticShapeLanguageFactWire::RustOwnership(value) => {
            SemanticShapeLanguageFact::RustOwnership(value.0)
        }
        SemanticShapeLanguageFactWire::GoVariadic(value) => {
            SemanticShapeLanguageFact::GoVariadic(*value)
        }
        SemanticShapeLanguageFactWire::PythonParameter { kind, confidence } => {
            SemanticShapeLanguageFact::PythonParameter {
                kind: kind.0,
                confidence: confidence.0,
            }
        }
        SemanticShapeLanguageFactWire::CSharp {
            nullability,
            reference_kind,
            is_async,
            is_iterator,
            is_extension,
        } => SemanticShapeLanguageFact::CSharp {
            nullability: nullability.0,
            reference_kind: reference_kind.0,
            is_async: *is_async,
            is_iterator: *is_iterator,
            is_extension: *is_extension,
        },
        SemanticShapeLanguageFactWire::TypeScript { declared, observed } => {
            SemanticShapeLanguageFact::TypeScript {
                declared: declared
                    .as_ref()
                    .map(|fact| type_fact_from_wire(fact, certificate).map(Box::new))
                    .transpose()?,
                observed: observed
                    .as_ref()
                    .map(|fact| type_fact_from_wire(fact, certificate).map(Box::new))
                    .transpose()?,
            }
        }
        SemanticShapeLanguageFactWire::Java {
            throws,
            annotations,
        } => SemanticShapeLanguageFact::Java {
            throws: throws
                .iter()
                .map(|fact| type_fact_from_wire(fact, certificate))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            annotations: annotations.clone().into_boxed_slice(),
        },
        SemanticShapeLanguageFactWire::Clang {
            is_const,
            is_volatile,
            is_restrict,
            size_bits,
            align_bits,
        } => SemanticShapeLanguageFact::Clang {
            is_const: *is_const,
            is_volatile: *is_volatile,
            is_restrict: *is_restrict,
            size_bits: *size_bits,
            align_bits: *align_bits,
        },
    })
}

fn type_fact_to_wire(fact: &SemanticTypeFact) -> SemanticTypeFactWire {
    match fact {
        SemanticTypeFact::Known(expression) => {
            SemanticTypeFactWire::Known(expr_to_wire(expression))
        }
        SemanticTypeFact::Unknown { reason, spelling } => SemanticTypeFactWire::Unknown {
            reason: (*reason).into(),
            spelling: spelling.clone(),
        },
        SemanticTypeFact::Unsupported { tag } => {
            SemanticTypeFactWire::Unsupported { tag: (*tag).into() }
        }
        SemanticTypeFact::Unavailable(reason) => SemanticTypeFactWire::Unavailable(*reason),
    }
}

fn type_fact_from_wire(
    fact: &SemanticTypeFactWire,
    certificate: &WireCertificate,
) -> Result<SemanticTypeFact, String> {
    Ok(match fact {
        SemanticTypeFactWire::Known(expression) => {
            SemanticTypeFact::Known(expr_from_wire(expression, certificate)?)
        }
        SemanticTypeFactWire::Unknown { reason, spelling } => SemanticTypeFact::Unknown {
            reason: reason.0,
            spelling: spelling.clone(),
        },
        SemanticTypeFactWire::Unsupported { tag } => SemanticTypeFact::Unsupported { tag: tag.0 },
        SemanticTypeFactWire::Unavailable(reason) => SemanticTypeFact::Unavailable(*reason),
    })
}

fn expr_to_wire(expression: &SemanticTypeExpr) -> SemanticTypeExprWire {
    match expression {
        SemanticTypeExpr::Builtin(value) => SemanticTypeExprWire::Builtin((*value).into()),
        SemanticTypeExpr::Literal(value) => SemanticTypeExprWire::Literal(match value {
            SemanticLiteral::String(value) => SemanticLiteralWire::String(value.clone()),
            SemanticLiteral::Number(value) => SemanticLiteralWire::Number(value.clone()),
            SemanticLiteral::BigInt(value) => SemanticLiteralWire::BigInt(value.clone()),
            SemanticLiteral::Boolean(value) => SemanticLiteralWire::Boolean(*value),
            SemanticLiteral::Null => SemanticLiteralWire::Null,
            SemanticLiteral::Undefined => SemanticLiteralWire::Undefined,
        }),
        SemanticTypeExpr::Nominal {
            declaration,
            symbol,
        } => SemanticTypeExprWire::Nominal {
            declaration: *declaration,
            symbol: symbol.map(symbol_address_to_wire),
        },
        SemanticTypeExpr::External { identity, display } => SemanticTypeExprWire::External {
            identity: *identity,
            display: display.clone(),
        },
        SemanticTypeExpr::Parameter(value) => SemanticTypeExprWire::Parameter(value.clone()),
        SemanticTypeExpr::Applied {
            constructor,
            arguments,
        } => SemanticTypeExprWire::Applied {
            constructor: Box::new(type_fact_to_wire(constructor)),
            arguments: arguments.iter().map(type_fact_to_wire).collect(),
        },
        SemanticTypeExpr::Tuple(elements) => {
            SemanticTypeExprWire::Tuple(elements.iter().map(element_to_wire).collect())
        }
        SemanticTypeExpr::Object(members) => {
            SemanticTypeExprWire::Object(members.iter().map(object_member_to_wire).collect())
        }
        SemanticTypeExpr::Function(callable) => {
            SemanticTypeExprWire::Function(Box::new(callable_to_wire(callable)))
        }
        SemanticTypeExpr::Reference {
            target,
            mutable,
            lifetime,
        } => SemanticTypeExprWire::Reference {
            target: Box::new(type_fact_to_wire(target)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        SemanticTypeExpr::Pointer { target, mutable } => SemanticTypeExprWire::Pointer {
            target: Box::new(type_fact_to_wire(target)),
            mutable: *mutable,
        },
        SemanticTypeExpr::Slice(target) => {
            SemanticTypeExprWire::Slice(Box::new(type_fact_to_wire(target)))
        }
        SemanticTypeExpr::Array { element, shape } => SemanticTypeExprWire::Array {
            element: Box::new(type_fact_to_wire(element)),
            shape: array_shape_to_wire(shape),
        },
        SemanticTypeExpr::Optional(target) => {
            SemanticTypeExprWire::Optional(Box::new(type_fact_to_wire(target)))
        }
        SemanticTypeExpr::Union(items) => {
            SemanticTypeExprWire::Union(items.iter().map(type_fact_to_wire).collect())
        }
        SemanticTypeExpr::Intersection(items) => {
            SemanticTypeExprWire::Intersection(items.iter().map(type_fact_to_wire).collect())
        }
        SemanticTypeExpr::Map { key, value } => SemanticTypeExprWire::Map {
            key: Box::new(type_fact_to_wire(key)),
            value: Box::new(type_fact_to_wire(value)),
        },
        SemanticTypeExpr::Channel { direction, element } => SemanticTypeExprWire::Channel {
            direction: (*direction).into(),
            element: Box::new(type_fact_to_wire(element)),
        },
        SemanticTypeExpr::Unsupported { tag } => {
            SemanticTypeExprWire::Unsupported { tag: (*tag).into() }
        }
    }
}

fn object_member_to_wire(member: &SemanticObjectMember) -> SemanticObjectMemberWire {
    match member {
        SemanticObjectMember::Property {
            key,
            ty,
            optional,
            readonly,
        } => SemanticObjectMemberWire::Property {
            key: property_key_to_wire(key),
            ty: type_fact_to_wire(ty),
            optional: *optional,
            readonly: *readonly,
        },
        SemanticObjectMember::Method {
            key,
            signature,
            optional,
        } => SemanticObjectMemberWire::Method {
            key: property_key_to_wire(key),
            signature: type_fact_to_wire(signature),
            optional: *optional,
        },
        SemanticObjectMember::Index {
            parameter,
            key,
            value,
            readonly,
        } => SemanticObjectMemberWire::Index {
            parameter: parameter.clone(),
            key: type_fact_to_wire(key),
            value: type_fact_to_wire(value),
            readonly: *readonly,
        },
        SemanticObjectMember::Call(signature) => {
            SemanticObjectMemberWire::Call(type_fact_to_wire(signature))
        }
        SemanticObjectMember::Construct(signature) => {
            SemanticObjectMemberWire::Construct(type_fact_to_wire(signature))
        }
    }
}

fn property_key_to_wire(key: &SemanticPropertyKey) -> SemanticPropertyKeyWire {
    match key {
        SemanticPropertyKey::Named(value) => SemanticPropertyKeyWire::Named(value.clone()),
        SemanticPropertyKey::Private(value) => SemanticPropertyKeyWire::Private(value.clone()),
        SemanticPropertyKey::Numeric(value) => SemanticPropertyKeyWire::Numeric(value.clone()),
        SemanticPropertyKey::Computed(fact) => {
            SemanticPropertyKeyWire::Computed(Box::new(type_fact_to_wire(fact)))
        }
    }
}

fn array_shape_to_wire(shape: &SemanticArrayShape) -> SemanticArrayShapeWire {
    match shape {
        SemanticArrayShape::Sequence => SemanticArrayShapeWire::Sequence,
        SemanticArrayShape::Rectangular { rank } => {
            SemanticArrayShapeWire::Rectangular { rank: rank.get() }
        }
        SemanticArrayShape::FixedValue { length } => {
            SemanticArrayShapeWire::FixedValue { length: *length }
        }
        SemanticArrayShape::ConstExpression(value) => {
            SemanticArrayShapeWire::ConstExpression(value.clone())
        }
        SemanticArrayShape::Incomplete => SemanticArrayShapeWire::Incomplete,
    }
}

fn array_shape_from_wire(shape: &SemanticArrayShapeWire) -> Result<SemanticArrayShape, String> {
    Ok(match shape {
        SemanticArrayShapeWire::Sequence => SemanticArrayShape::Sequence,
        SemanticArrayShapeWire::Rectangular { rank } => SemanticArrayShape::Rectangular {
            rank: std::num::NonZeroU16::new(*rank)
                .ok_or_else(|| "rectangular array rank must be nonzero".to_owned())?,
        },
        SemanticArrayShapeWire::FixedValue { length } => {
            SemanticArrayShape::FixedValue { length: *length }
        }
        SemanticArrayShapeWire::ConstExpression(value) => {
            SemanticArrayShape::ConstExpression(value.clone())
        }
        SemanticArrayShapeWire::Incomplete => SemanticArrayShape::Incomplete,
    })
}

fn expr_from_wire(
    expression: &SemanticTypeExprWire,
    certificate: &WireCertificate,
) -> Result<SemanticTypeExpr, String> {
    Ok(match expression {
        SemanticTypeExprWire::Builtin(value) => SemanticTypeExpr::Builtin(value.0),
        SemanticTypeExprWire::Literal(value) => SemanticTypeExpr::Literal(match value {
            SemanticLiteralWire::String(value) => SemanticLiteral::String(value.clone()),
            SemanticLiteralWire::Number(value) => SemanticLiteral::Number(value.clone()),
            SemanticLiteralWire::BigInt(value) => SemanticLiteral::BigInt(value.clone()),
            SemanticLiteralWire::Boolean(value) => SemanticLiteral::Boolean(*value),
            SemanticLiteralWire::Null => SemanticLiteral::Null,
            SemanticLiteralWire::Undefined => SemanticLiteral::Undefined,
        }),
        SemanticTypeExprWire::Nominal {
            declaration,
            symbol,
        } => SemanticTypeExpr::Nominal {
            declaration: *declaration,
            symbol: symbol
                .as_ref()
                .map(|symbol| symbol_address_from_wire(symbol, certificate))
                .transpose()?,
        },
        SemanticTypeExprWire::External { identity, display } => SemanticTypeExpr::External {
            identity: *identity,
            display: display.clone(),
        },
        SemanticTypeExprWire::Parameter(value) => SemanticTypeExpr::Parameter(value.clone()),
        SemanticTypeExprWire::Applied {
            constructor,
            arguments,
        } => SemanticTypeExpr::Applied {
            constructor: Box::new(type_fact_from_wire(constructor, certificate)?),
            arguments: arguments
                .iter()
                .map(|fact| type_fact_from_wire(fact, certificate))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        },
        SemanticTypeExprWire::Tuple(elements) => SemanticTypeExpr::Tuple(
            elements
                .iter()
                .map(|element| element_from_wire(element, certificate))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        SemanticTypeExprWire::Object(members) => SemanticTypeExpr::Object(
            members
                .iter()
                .map(|member| object_member_from_wire(member, certificate))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        SemanticTypeExprWire::Function(callable) => {
            SemanticTypeExpr::Function(Box::new(callable_from_wire(callable, certificate)?))
        }
        SemanticTypeExprWire::Reference {
            target,
            mutable,
            lifetime,
        } => SemanticTypeExpr::Reference {
            target: Box::new(type_fact_from_wire(target, certificate)?),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        SemanticTypeExprWire::Pointer { target, mutable } => SemanticTypeExpr::Pointer {
            target: Box::new(type_fact_from_wire(target, certificate)?),
            mutable: *mutable,
        },
        SemanticTypeExprWire::Slice(target) => {
            SemanticTypeExpr::Slice(Box::new(type_fact_from_wire(target, certificate)?))
        }
        SemanticTypeExprWire::Array { element, shape } => SemanticTypeExpr::Array {
            element: Box::new(type_fact_from_wire(element, certificate)?),
            shape: array_shape_from_wire(shape)?,
        },
        SemanticTypeExprWire::Optional(target) => {
            SemanticTypeExpr::Optional(Box::new(type_fact_from_wire(target, certificate)?))
        }
        SemanticTypeExprWire::Union(items) => SemanticTypeExpr::Union(
            items
                .iter()
                .map(|fact| type_fact_from_wire(fact, certificate))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        SemanticTypeExprWire::Intersection(items) => SemanticTypeExpr::Intersection(
            items
                .iter()
                .map(|fact| type_fact_from_wire(fact, certificate))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        SemanticTypeExprWire::Map { key, value } => SemanticTypeExpr::Map {
            key: Box::new(type_fact_from_wire(key, certificate)?),
            value: Box::new(type_fact_from_wire(value, certificate)?),
        },
        SemanticTypeExprWire::Channel { direction, element } => SemanticTypeExpr::Channel {
            direction: direction.0,
            element: Box::new(type_fact_from_wire(element, certificate)?),
        },
        SemanticTypeExprWire::Unsupported { tag } => SemanticTypeExpr::Unsupported { tag: tag.0 },
    })
}

fn object_member_from_wire(
    member: &SemanticObjectMemberWire,
    certificate: &WireCertificate,
) -> Result<SemanticObjectMember, String> {
    Ok(match member {
        SemanticObjectMemberWire::Property {
            key,
            ty,
            optional,
            readonly,
        } => SemanticObjectMember::Property {
            key: property_key_from_wire(key, certificate)?,
            ty: type_fact_from_wire(ty, certificate)?,
            optional: *optional,
            readonly: *readonly,
        },
        SemanticObjectMemberWire::Method {
            key,
            signature,
            optional,
        } => SemanticObjectMember::Method {
            key: property_key_from_wire(key, certificate)?,
            signature: type_fact_from_wire(signature, certificate)?,
            optional: *optional,
        },
        SemanticObjectMemberWire::Index {
            parameter,
            key,
            value,
            readonly,
        } => SemanticObjectMember::Index {
            parameter: parameter.clone(),
            key: type_fact_from_wire(key, certificate)?,
            value: type_fact_from_wire(value, certificate)?,
            readonly: *readonly,
        },
        SemanticObjectMemberWire::Call(signature) => {
            SemanticObjectMember::Call(type_fact_from_wire(signature, certificate)?)
        }
        SemanticObjectMemberWire::Construct(signature) => {
            SemanticObjectMember::Construct(type_fact_from_wire(signature, certificate)?)
        }
    })
}

fn property_key_from_wire(
    key: &SemanticPropertyKeyWire,
    certificate: &WireCertificate,
) -> Result<SemanticPropertyKey, String> {
    Ok(match key {
        SemanticPropertyKeyWire::Named(value) => SemanticPropertyKey::Named(value.clone()),
        SemanticPropertyKeyWire::Private(value) => SemanticPropertyKey::Private(value.clone()),
        SemanticPropertyKeyWire::Numeric(value) => SemanticPropertyKey::Numeric(value.clone()),
        SemanticPropertyKeyWire::Computed(fact) => {
            SemanticPropertyKey::Computed(type_fact_from_wire(fact, certificate)?)
        }
    })
}

fn symbol_address_to_wire(address: SymbolAddress) -> SymbolAddressWire {
    SymbolAddressWire {
        kind: if address.is_selected() {
            SymbolAddressKindWire::Selected
        } else {
            SymbolAddressKindWire::Canonical
        },
        id: encode_id(&address.claimed_bytes()),
    }
}

fn symbol_address_from_wire(
    value: &SymbolAddressWire,
    certificate: &WireCertificate,
) -> Result<SymbolAddress, String> {
    match value.kind {
        SymbolAddressKindWire::Canonical => certificate
            .key_value::<SymbolSchema>(WireSchema::Symbol, &value.id)
            .map(SymbolAddress::canonical),
        SymbolAddressKindWire::Selected => {
            certificate.key_commitment(WireSchema::Symbol, &value.id)?;
            decode_id(&value.id)
                .map(SymbolAddress::from_selected_bytes)
                .map_err(|error| error.to_string())
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::{MAX_SEMANTIC_SHAPE_BYTES, SemanticShapeUnavailable};
    use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, RustEdition};

    fn batch(fact: SemanticShapeFactWire) -> SemanticShapeBatchWire {
        SemanticShapeBatchWire {
            basis: String::new(),
            entries: vec![SemanticShapeEntryWire {
                symbol: SymbolAddressWire {
                    kind: SymbolAddressKindWire::Selected,
                    id: String::new(),
                },
                identity: None,
                origin: None,
                fact,
            }],
        }
    }

    fn callable_wire_batch(callable: SemanticCallableShapeWire) -> SemanticShapeBatchWire {
        batch(SemanticShapeFactWire::Available {
            shape: SemanticDeclarationShapeWire::Callable(callable),
            language: SemanticShapeLanguageFactsWire::CommonOnly {
                profile: crate::SemanticLanguageProfile::new(LanguageProfile::Rust(
                    RustEdition::Rust2021,
                )),
            },
        })
    }

    fn unavailable_wire_element() -> SemanticTypeElementWire {
        SemanticTypeElementWire {
            label: None,
            kind: TupleElementKind::Required.into(),
            ty: SemanticTypeFactWire::Unavailable(SemanticTypeUnavailable::MissingImageFact),
        }
    }

    #[test]
    fn callable_carrier_bindings_round_trip_all_capture_states() {
        let identity = SemanticDeclarationIdentity {
            family: [41; 16],
            variant: [42; 16],
        };
        let captured = SemanticCallableShape {
            parameters: vec![SemanticTypeElement {
                label: None,
                kind: TupleElementKind::Required,
                ty: SemanticTypeFact::Unavailable(SemanticTypeUnavailable::MissingImageFact),
            }]
            .into_boxed_slice(),
            results: Box::new([]),
            carrier_bindings: SemanticCallableCarrierBindings::Captured {
                parameters: vec![identity].into_boxed_slice(),
                results: Box::new([]),
            },
            abi: None,
            variadic: FunctionVariadicForm::None,
            unsafe_: false,
        };
        let captured_wire = callable_to_wire(&captured);
        let encoded = serde_json::to_value(&captured_wire).expect("serialize carrier bindings");
        assert_eq!(
            encoded["carrier_bindings"]["capture"], "captured",
            "wire names the completeness state explicitly"
        );
        let decoded: SemanticCallableShapeWire =
            serde_json::from_value(encoded.clone()).expect("decode captured identities");
        assert_eq!(
            callable_from_wire(&decoded, &WireCertificate::new())
                .expect("admit captured identities"),
            captured
        );

        let captured_empty = SemanticCallableShape {
            parameters: Box::new([]),
            results: Box::new([]),
            carrier_bindings: SemanticCallableCarrierBindings::Captured {
                parameters: Box::new([]),
                results: Box::new([]),
            },
            abi: None,
            variadic: FunctionVariadicForm::None,
            unsafe_: false,
        };
        let empty_wire = callable_to_wire(&captured_empty);
        assert_eq!(
            serde_json::to_value(&empty_wire).expect("serialize captured empty relation")["carrier_bindings"]
                ["capture"],
            "captured"
        );
        assert_eq!(
            callable_from_wire(&empty_wire, &WireCertificate::new())
                .expect("admit captured empty relation"),
            captured_empty
        );

        let unavailable = SemanticCallableShape {
            carrier_bindings: SemanticCallableCarrierBindings::Unavailable,
            ..captured_empty
        };
        let unavailable_wire = callable_to_wire(&unavailable);
        assert_eq!(
            serde_json::to_value(&unavailable_wire).expect("serialize unavailable relation")["carrier_bindings"]
                ["capture"],
            "unavailable"
        );
        assert_eq!(
            callable_from_wire(&unavailable_wire, &WireCertificate::new())
                .expect("admit unavailable relation"),
            unavailable
        );

        let malformed = serde_json::from_value::<SemanticCallableShapeWire>(serde_json::json!({
            "parameters": [],
            "results": [],
            "carrier_bindings": {"capture": "future_state", "bindings": {}},
            "abi": null,
            "variadic": 0,
            "unsafe_": false
        }));
        assert!(malformed.is_err(), "capture states are a closed wire enum");
    }

    #[test]
    fn callable_carrier_wire_preflight_checks_alignment_and_shared_node_budget() {
        let identity = SemanticDeclarationIdentity {
            family: [51; 16],
            variant: [52; 16],
        };
        let misaligned = SemanticCallableShapeWire {
            parameters: vec![unavailable_wire_element()],
            results: Vec::new(),
            carrier_bindings: SemanticCallableCarrierBindingsWire::Captured {
                parameters: Vec::new(),
                results: Vec::new(),
            },
            abi: None,
            variadic: FunctionVariadicForm::None.into(),
            unsafe_: false,
        };
        let error = admit_shape_wire_tree(&callable_wire_batch(misaligned.clone()))
            .expect_err("captured identities must align with tuple cells");
        assert!(error.contains("align"));
        assert!(callable_from_wire(&misaligned, &WireCertificate::new()).is_err());

        let mut at_node_limit = SemanticCallableShapeWire {
            parameters: vec![unavailable_wire_element()],
            results: Vec::new(),
            carrier_bindings: SemanticCallableCarrierBindingsWire::Captured {
                parameters: vec![identity],
                results: Vec::new(),
            },
            abi: None,
            variadic: FunctionVariadicForm::None.into(),
            unsafe_: false,
        };
        let mut nodes = crate::MAX_SEMANTIC_SHAPE_NODES - 3;
        assert!(shape_wire_callable(&at_node_limit, 0, &mut nodes).is_ok());
        at_node_limit.carrier_bindings = SemanticCallableCarrierBindingsWire::Captured {
            parameters: vec![identity, identity],
            results: Vec::new(),
        };
        at_node_limit.parameters = vec![unavailable_wire_element(); 2];
        let mut nodes = crate::MAX_SEMANTIC_SHAPE_NODES - 5;
        assert!(
            shape_wire_callable(&at_node_limit, 0, &mut nodes)
                .expect_err("binding identities consume the shared hard node bound")
                .contains("node")
        );
    }

    #[test]
    fn wire_preflight_rejects_deep_type_graphs_before_typed_expansion() {
        let mut ty =
            SemanticTypeFactWire::Known(SemanticTypeExprWire::Builtin(BuiltinType::Never.into()));
        for _ in 0..=crate::MAX_SEMANTIC_SHAPE_DEPTH {
            ty = SemanticTypeFactWire::Known(SemanticTypeExprWire::Optional(Box::new(ty)));
        }
        let wire = batch(SemanticShapeFactWire::Available {
            shape: SemanticDeclarationShapeWire::Typed(ty),
            language: SemanticShapeLanguageFactsWire::CommonOnly {
                profile: crate::SemanticLanguageProfile::new(LanguageProfile::Rust(
                    RustEdition::Rust2021,
                )),
            },
        });
        let error = admit_shape_wire_tree(&wire).expect_err("depth limit must reject wire tree");
        assert!(error.contains("depth"));
    }

    #[test]
    fn wire_preflight_counts_structural_member_edges_in_depth() {
        let mut ty = SemanticTypeFactWire::Unavailable(SemanticTypeUnavailable::DepthBudget);
        for _ in 0..=crate::MAX_SEMANTIC_SHAPE_DEPTH {
            ty = SemanticTypeFactWire::Known(SemanticTypeExprWire::Object(vec![
                SemanticObjectMemberWire::Property {
                    key: SemanticPropertyKeyWire::Named(
                        crate::SourceAtomText::new("next").expect("exact key spelling"),
                    ),
                    ty,
                    optional: false,
                    readonly: false,
                },
            ]));
        }
        let wire = batch(SemanticShapeFactWire::Available {
            shape: SemanticDeclarationShapeWire::Typed(ty),
            language: SemanticShapeLanguageFactsWire::CommonOnly {
                profile: crate::SemanticLanguageProfile::new(LanguageProfile::Rust(
                    RustEdition::Rust2021,
                )),
            },
        });
        let error = admit_shape_wire_tree(&wire)
            .expect_err("object member edges must consume the shared depth bound");
        assert!(error.contains("depth"));
    }

    #[test]
    fn wire_preflight_rejects_node_and_byte_overruns() {
        let elements = (0..crate::MAX_SEMANTIC_SHAPE_NODES)
            .map(|_| SemanticTypeElementWire {
                label: None,
                kind: TupleElementKind::Required.into(),
                ty: SemanticTypeFactWire::Unavailable(SemanticTypeUnavailable::MissingImageFact),
            })
            .collect();
        let many_nodes = batch(SemanticShapeFactWire::Available {
            shape: SemanticDeclarationShapeWire::Typed(SemanticTypeFactWire::Known(
                SemanticTypeExprWire::Tuple(elements),
            )),
            language: SemanticShapeLanguageFactsWire::CommonOnly {
                profile: crate::SemanticLanguageProfile::new(LanguageProfile::Rust(
                    RustEdition::Rust2021,
                )),
            },
        });
        let error =
            admit_shape_wire_tree(&many_nodes).expect_err("node limit must reject wire tree");
        assert!(error.contains("node"));

        let many_annotations = batch(SemanticShapeFactWire::Available {
            shape: SemanticDeclarationShapeWire::Typed(SemanticTypeFactWire::Unavailable(
                SemanticTypeUnavailable::MissingImageFact,
            )),
            language: SemanticShapeLanguageFactsWire::Partial {
                profile: crate::SemanticLanguageProfile::new(LanguageProfile::Java(
                    JavaRelease::Java17,
                )),
                facts: SemanticShapeLanguageFactWire::Java {
                    throws: Vec::new(),
                    annotations: vec![
                        crate::SourceAtomText::new("A")
                            .expect("exact annotation text");
                        crate::MAX_SEMANTIC_SHAPE_NODES
                    ],
                },
            },
        });
        let error = admit_shape_wire_tree(&many_annotations)
            .expect_err("language annotation nodes must share the hard node limit");
        assert!(error.contains("node"));

        let mut oversized = batch(SemanticShapeFactWire::Unavailable(
            SemanticShapeUnavailable::NotInView,
        ));
        oversized.entries[0].symbol.id = "x".repeat(MAX_SEMANTIC_SHAPE_BYTES + 1);
        let error =
            admit_shape_wire_tree(&oversized).expect_err("byte limit must reject wire body");
        assert!(error.contains("byte"));
    }

    #[test]
    fn wire_enum_decoder_rejects_unknown_builtin_codes() {
        let malformed = serde_json::json!({"kind":"builtin", "data": 255});
        assert!(serde_json::from_value::<SemanticTypeExprWire>(malformed).is_err());
    }

    #[test]
    fn closed_nested_wire_states_reject_invalid_codes_and_impossible_payloads() {
        let malformed =
            serde_json::json!({"state":"unknown", "data":{"reason":255, "spelling":null}});
        assert!(serde_json::from_value::<SemanticTypeFactWire>(malformed).is_err());

        let rank_zero = SemanticArrayShapeWire::Rectangular { rank: 0 };
        assert!(array_shape_from_wire(&rank_zero).is_err());
        let malformed_array = batch(SemanticShapeFactWire::Available {
            shape: SemanticDeclarationShapeWire::Typed(SemanticTypeFactWire::Known(
                SemanticTypeExprWire::Array {
                    element: Box::new(SemanticTypeFactWire::Unavailable(
                        SemanticTypeUnavailable::MissingImageFact,
                    )),
                    shape: rank_zero,
                },
            )),
            language: SemanticShapeLanguageFactsWire::CommonOnly {
                profile: crate::SemanticLanguageProfile::new(LanguageProfile::Rust(
                    RustEdition::Rust2021,
                )),
            },
        });
        assert!(
            admit_shape_wire_tree(&malformed_array)
                .expect_err("wire preflight rejects rank zero before conversion")
                .contains("nonzero")
        );

        let malformed_profile: SemanticShapeLanguageFactsWire = serde_json::from_value(
            serde_json::json!({"coverage":"common_only", "data":{"profile":[255,255]}}),
        )
        .expect("profile bytes remain an opaque closed-scalar wire value until admission");
        assert!(language_from_wire(&malformed_profile, &WireCertificate::new()).is_err());
    }
}
