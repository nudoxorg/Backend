//! Canonical, language-neutral type rendering over the complete semantic image.
//!
//! This is deliberately not source syntax. It is a compact structural form for
//! discovery, comparison, and diagnostics that gives every semantic type
//! constructor one unambiguous spelling. Language renderers may consume the
//! same reader later, but cannot redefine this representation's meaning.

use core::{fmt, num::NonZeroUsize, ops::Deref, str};

use thiserror::Error;

use crate::{
    ArrayShape, AtomId, AtomListId, ComputedType, ConcreteType, EntityId, ExternalId,
    ExternalTarget, ForeignTargetOrigin, LiteralType, ObjectMemberListId, QualifiedSegments,
    SemanticReader, TemplatePartListId, TupleElementListId, TypeExpr, TypeId, TypeListId,
    TypeQuery, WildcardBound,
};

mod names;
mod output;
mod traverse;

use names::*;
use output::*;
use traverse::*;

/// Closed typed reference spaces traversed by canonical type rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalTypeRenderReference {
    /// Another semantic type row.
    Type(TypeId),
    /// An interned byte atom.
    Atom(AtomId),
    /// An ordered semantic type list.
    TypeList(TypeListId),
    /// An ordered atom list.
    AtomList(AtomListId),
    /// A role-bearing tuple or callable list.
    TupleElements(TupleElementListId),
    /// A structural object-member list.
    ObjectMembers(ObjectMemberListId),
    /// A template-literal part list.
    TemplateParts(TemplatePartListId),
    /// A local declaration row.
    Entity(EntityId),
    /// A cross-fragment or unresolved foreign target.
    External(ExternalId),
}

/// Exact structural, resource, or caller-output failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CanonicalTypeRenderError {
    #[error("canonical type root {root:?} is absent")]
    MissingRoot {
        /// Requested root coordinate.
        root: TypeId,
    },
    #[error("canonical type {owner:?} names absent semantic reference {reference:?}")]
    MissingReference {
        /// Type row that carried the rejected coordinate.
        owner: TypeId,
        /// Exact typed coordinate which could not be resolved.
        reference: CanonicalTypeRenderReference,
    },
    #[error("canonical type traversal from {root:?} reached {at:?} beyond depth limit {limit}")]
    TraversalLimit {
        /// Original requested root.
        root: TypeId,
        /// Type row that would exceed the traversal bound.
        at: TypeId,
        /// Caller-selected nonzero depth limit.
        limit: NonZeroUsize,
    },
    #[error("canonical type rendering length overflowed for {root:?}")]
    OutputLengthOverflow {
        /// Requested root whose canonical length overflowed.
        root: TypeId,
    },
    #[error("canonical type {root:?} needs {required} bytes but caller supplied {available}")]
    OutputTooSmall {
        /// Prepared root.
        root: TypeId,
        /// Exact prepared byte requirement.
        required: usize,
        /// Caller output capacity.
        available: usize,
    },
    #[error("prepared canonical type {root:?} wrote {written} bytes rather than {promised}")]
    PreparedLengthMismatch {
        /// Prepared root.
        root: TypeId,
        /// Exact byte length from the validation pass.
        promised: usize,
        /// Bytes actually written before the invariant failed.
        written: usize,
    },
    #[error("the caller formatter rejected canonical type output for {root:?}")]
    OutputWrite {
        /// Root whose output the formatter rejected.
        root: TypeId,
    },
    #[error("canonical type output for {root:?} was not UTF-8 after byte {valid_up_to}")]
    OutputEncoding {
        /// Prepared root.
        root: TypeId,
        /// First invalid UTF-8 byte offset.
        valid_up_to: usize,
        /// Original UTF-8 validation source.
        #[source]
        source: str::Utf8Error,
    },
}

/// Bounded traversal law for a canonical type rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalTypeRenderLimits {
    /// Largest permitted number of nested semantic type nodes.
    pub maximum_depth: NonZeroUsize,
}

impl CanonicalTypeRenderLimits {
    #[must_use]
    pub const fn new(maximum_depth: NonZeroUsize) -> Self {
        Self { maximum_depth }
    }
}

/// Immutable public facts for one admitted canonical type rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedCanonicalTypeView {
    /// Root type admitted by the preparation pass.
    pub root: TypeId,
    /// Exact canonical byte count produced by a successful write.
    pub encoded_len: usize,
    /// Traversal bound used by both preparation and writing.
    pub limits: CanonicalTypeRenderLimits,
}

/// A complete semantic type admitted and measured without touching output.
pub struct PreparedCanonicalType<'image, Reader: SemanticReader + ?Sized> {
    reader: &'image Reader,
    view: PreparedCanonicalTypeView,
}

impl<Reader: SemanticReader + ?Sized> Deref for PreparedCanonicalType<'_, Reader> {
    type Target = PreparedCanonicalTypeView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<Reader: SemanticReader + ?Sized> PreparedCanonicalType<'_, Reader> {
    /// Writes atomically into caller-owned bytes after the preparation pass
    /// proved the complete graph and exact byte requirement.
    pub fn write_into<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output str, CanonicalTypeRenderError> {
        if output.len() < self.encoded_len {
            return Err(CanonicalTypeRenderError::OutputTooSmall {
                root: self.root,
                required: self.encoded_len,
                available: output.len(),
            });
        }
        let mut writer = ByteWriter::new(output);
        emit_type(
            self.reader,
            self.root,
            self.root,
            1,
            self.limits,
            &mut writer,
        )?;
        if writer.written_len() != self.encoded_len {
            return Err(CanonicalTypeRenderError::PreparedLengthMismatch {
                root: self.root,
                promised: self.encoded_len,
                written: writer.written_len(),
            });
        }
        str::from_utf8(writer.written()).map_err(|source| {
            CanonicalTypeRenderError::OutputEncoding {
                root: self.root,
                valid_up_to: source.valid_up_to(),
                source,
            }
        })
    }

    /// Streams the already-admitted structural form to a caller formatter.
    pub fn write_to(&self, output: &mut impl fmt::Write) -> Result<(), CanonicalTypeRenderError> {
        emit_type(
            self.reader,
            self.root,
            self.root,
            1,
            self.limits,
            output,
        )
    }
}

/// Validates and exactly measures a complete canonical type before output.
pub fn prepare_canonical_type<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    root: TypeId,
    limits: CanonicalTypeRenderLimits,
) -> Result<PreparedCanonicalType<'image, Reader>, CanonicalTypeRenderError> {
    if reader.ty(root).is_none() {
        return Err(CanonicalTypeRenderError::MissingRoot { root });
    }
    let mut writer = CountWriter::new();
    emit_type(reader, root, root, 1, limits, &mut writer)?;
    let encoded_len = writer
        .encoded_len()
        .ok_or(CanonicalTypeRenderError::OutputLengthOverflow { root })?;
    Ok(PreparedCanonicalType {
        reader,
        view: PreparedCanonicalTypeView {
            root,
            encoded_len,
            limits,
        },
    })
}

fn emit_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    current: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    if depth > limits.maximum_depth.get() {
        return Err(CanonicalTypeRenderError::TraversalLimit {
            root,
            at: current,
            limit: limits.maximum_depth,
        });
    }
    let ty = reader.ty(current).ok_or(missing_type(current, current))?;
    match ty {
        TypeExpr::Concrete(ty) => emit_concrete(reader, root, current, depth, limits, ty, output),
        TypeExpr::Computed(ty) => emit_computed(reader, root, current, depth, limits, ty, output),
        TypeExpr::Unknown(ty) => {
            write_text(root, output, "unknown(")?;
            write_text(root, output, unknown_name(ty.reason))?;
            if let Some(spelling) = ty.spelling {
                write_text(root, output, ",")?;
                emit_atom(reader, root, current, spelling, output)?;
            }
            write_text(root, output, ")")
        }
    }
}

fn emit_concrete<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    ty: ConcreteType,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match ty {
        ConcreteType::Builtin(value) => tagged_text(root, output, "builtin", builtin_name(value)),
        ConcreteType::Literal(value) => emit_literal(reader, root, owner, value, output),
        ConcreteType::Nominal(entity) => emit_nominal(reader, root, owner, entity, output),
        ConcreteType::External(external) => emit_external(reader, root, owner, external, output),
        ConcreteType::Parameter(atom) => tagged_atom(reader, root, owner, "parameter", atom, output),
        ConcreteType::Applied { constructor, arguments } => {
            write_text(root, output, "applied(")?;
            child(reader, root, owner, constructor, depth, limits, output)?;
            write_text(root, output, ",")?;
            emit_type_list(reader, root, owner, arguments, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::Tuple(elements) => {
            write_text(root, output, "tuple")?;
            emit_tuple_list(reader, root, owner, elements, depth, limits, output)
        }
        ConcreteType::Object(members) => {
            write_text(root, output, "object")?;
            emit_object_list(reader, root, owner, members, depth, limits, output)
        }
        ConcreteType::Function {
            parameters,
            results,
            abi,
            variadic,
            unsafe_,
        } => {
            write_text(root, output, "function(parameters=")?;
            emit_tuple_list(reader, root, owner, parameters, depth, limits, output)?;
            write_text(root, output, ",results=")?;
            emit_tuple_list(reader, root, owner, results, depth, limits, output)?;
            write_text(root, output, ",abi=")?;
            emit_optional_atom(reader, root, owner, abi, output)?;
            write_text(root, output, ",variadic=")?;
            write_text(root, output, variadic_name(variadic))?;
            write_text(root, output, ",unsafe=")?;
            write_bool(root, output, unsafe_)?;
            write_text(root, output, ")")
        }
        ConcreteType::Reference {
            target,
            mutability,
            lifetime,
        } => {
            write_text(root, output, "reference(mutability=")?;
            write_text(root, output, mutability_name(mutability))?;
            write_text(root, output, ",lifetime=")?;
            emit_optional_atom(reader, root, owner, lifetime, output)?;
            write_text(root, output, ",target=")?;
            child(reader, root, owner, target, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::CxxReference { target, category } => {
            write_text(root, output, "cxx-reference(category=")?;
            write_text(root, output, cxx_reference_name(category))?;
            write_text(root, output, ",target=")?;
            child(reader, root, owner, target, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::CPointer { target } => unary(reader, root, owner, depth, limits, "c-pointer", target, output),
        ConcreteType::CxxMemberPointer { owner: class, member } => {
            write_text(root, output, "cxx-member-pointer(owner=")?;
            child(reader, root, owner, class, depth, limits, output)?;
            write_text(root, output, ",member=")?;
            child(reader, root, owner, member, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::CQualified { target, qualifiers } => {
            write_text(root, output, "c-qualified(const=")?;
            write_bool(root, output, qualifiers.const_)?;
            write_text(root, output, ",volatile=")?;
            write_bool(root, output, qualifiers.volatile)?;
            write_text(root, output, ",restrict=")?;
            write_bool(root, output, qualifiers.restrict)?;
            write_text(root, output, ",target=")?;
            child(reader, root, owner, target, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::CBlockPointer { target } => unary(reader, root, owner, depth, limits, "c-block-pointer", target, output),
        ConcreteType::NativeCharacter { role, width } => {
            write_text(root, output, "native-character(role=")?;
            write_text(root, output, character_role_name(role))?;
            write_text(root, output, ",width=")?;
            write_number(root, output, u64::from(width.get()))?;
            write_text(root, output, ")")
        }
        ConcreteType::Pointer { target, mutability } => {
            write_text(root, output, "pointer(mutability=")?;
            write_text(root, output, mutability_name(mutability))?;
            write_text(root, output, ",target=")?;
            child(reader, root, owner, target, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::Slice(target) => unary(reader, root, owner, depth, limits, "slice", target, output),
        ConcreteType::Array { element, shape } => {
            write_text(root, output, "array(shape=")?;
            emit_array_shape(reader, root, owner, shape, output)?;
            write_text(root, output, ",element=")?;
            child(reader, root, owner, element, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::Optional(target) => unary(reader, root, owner, depth, limits, "optional", target, output),
        ConcreteType::Union(types) => tagged_type_list(reader, root, owner, depth, limits, "union", types, output),
        ConcreteType::Intersection(types) => tagged_type_list(reader, root, owner, depth, limits, "intersection", types, output),
        ConcreteType::ImplTrait(types) => tagged_type_list(reader, root, owner, depth, limits, "impl-trait", types, output),
        ConcreteType::DynTrait(types) => tagged_type_list(reader, root, owner, depth, limits, "dyn-trait", types, output),
        ConcreteType::Wildcard(bound) => emit_wildcard(reader, root, owner, depth, limits, bound, output),
        ConcreteType::Annotated { kind, target } => {
            write_text(root, output, "annotated(kind=")?;
            write_text(root, output, annotation_name(kind))?;
            write_text(root, output, ",target=")?;
            child(reader, root, owner, target, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::Inferred(spelling) => {
            write_text(root, output, "inferred(")?;
            emit_optional_atom(reader, root, owner, spelling, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::QualifiedPath { self_type, trait_type, segments, spelling } => {
            write_text(root, output, "qualified(self=")?;
            child(reader, root, owner, self_type, depth, limits, output)?;
            write_text(root, output, ",trait=")?;
            emit_optional_type(reader, root, owner, trait_type, depth, limits, output)?;
            write_text(root, output, ",segments=")?;
            emit_qualified_segments(reader, root, owner, segments, output)?;
            write_text(root, output, ",spelling=")?;
            emit_atom(reader, root, owner, spelling, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::Map { key, value } => {
            write_text(root, output, "map(key=")?;
            child(reader, root, owner, key, depth, limits, output)?;
            write_text(root, output, ",value=")?;
            child(reader, root, owner, value, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ConcreteType::Channel { direction, element } => {
            write_text(root, output, "channel(direction=")?;
            write_text(root, output, channel_name(direction))?;
            write_text(root, output, ",element=")?;
            child(reader, root, owner, element, depth, limits, output)?;
            write_text(root, output, ")")
        }
    }
}

fn emit_computed<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    ty: ComputedType,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match ty {
        ComputedType::KeyOf(target) => unary(reader, root, owner, depth, limits, "keyof", target, output),
        ComputedType::TypeOf(query) => emit_type_query(reader, root, owner, query, output),
        ComputedType::IndexedAccess { object, index } => {
            write_text(root, output, "indexed(object=")?;
            child(reader, root, owner, object, depth, limits, output)?;
            write_text(root, output, ",index=")?;
            child(reader, root, owner, index, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ComputedType::Conditional { check, extends, then_type, else_type, distributive } => {
            write_text(root, output, "conditional(check=")?;
            child(reader, root, owner, check, depth, limits, output)?;
            write_text(root, output, ",extends=")?;
            child(reader, root, owner, extends, depth, limits, output)?;
            write_text(root, output, ",then=")?;
            child(reader, root, owner, then_type, depth, limits, output)?;
            write_text(root, output, ",else=")?;
            child(reader, root, owner, else_type, depth, limits, output)?;
            write_text(root, output, ",distributive=")?;
            write_bool(root, output, distributive)?;
            write_text(root, output, ")")
        }
        ComputedType::Mapped { parameter, constraint, name_as, value, readonly, optional } => {
            write_text(root, output, "mapped(parameter=")?;
            emit_atom(reader, root, owner, parameter, output)?;
            write_text(root, output, ",constraint=")?;
            child(reader, root, owner, constraint, depth, limits, output)?;
            write_text(root, output, ",as=")?;
            emit_optional_type(reader, root, owner, name_as, depth, limits, output)?;
            write_text(root, output, ",value=")?;
            child(reader, root, owner, value, depth, limits, output)?;
            write_text(root, output, ",readonly=")?;
            write_text(root, output, modifier_name(readonly))?;
            write_text(root, output, ",optional=")?;
            write_text(root, output, modifier_name(optional))?;
            write_text(root, output, ")")
        }
        ComputedType::Infer { parameter, constraint } => {
            write_text(root, output, "infer(parameter=")?;
            emit_atom(reader, root, owner, parameter, output)?;
            write_text(root, output, ",constraint=")?;
            emit_optional_type(reader, root, owner, constraint, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ComputedType::TemplateLiteral(parts) => {
            write_text(root, output, "template")?;
            emit_template_list(reader, root, owner, parts, depth, limits, output)
        }
        ComputedType::Import { specifier, qualifier, arguments } => {
            write_text(root, output, "import(specifier=")?;
            emit_atom(reader, root, owner, specifier, output)?;
            write_text(root, output, ",qualifier=")?;
            emit_atom_list(reader, root, owner, qualifier, output)?;
            write_text(root, output, ",arguments=")?;
            emit_type_list(reader, root, owner, arguments, depth, limits, output)?;
            write_text(root, output, ")")
        }
        ComputedType::Awaited(target) => unary(reader, root, owner, depth, limits, "awaited", target, output),
        ComputedType::This => write_text(root, output, "this"),
    }
}

fn emit_literal<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    literal: LiteralType,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match literal {
        LiteralType::String(atom) => tagged_atom(reader, root, owner, "literal.string", atom, output),
        LiteralType::Number(atom) => tagged_atom(reader, root, owner, "literal.number", atom, output),
        LiteralType::BigInt(atom) => tagged_atom(reader, root, owner, "literal.bigint", atom, output),
        LiteralType::Boolean(value) => tagged_text(root, output, "literal.boolean", if value { "true" } else { "false" }),
        LiteralType::Null => write_text(root, output, "literal.null"),
        LiteralType::Undefined => write_text(root, output, "literal.undefined"),
    }
}

fn emit_nominal<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    entity: EntityId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let row = reader.entity(entity).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::Entity(entity),
    })?;
    write_text(root, output, "nominal(entity=")?;
    write_number(root, output, u64::from(entity.raw))?;
    write_text(root, output, ",name=")?;
    emit_atom(reader, root, owner, row.name, output)?;
    write_text(root, output, ")")
}

fn emit_external<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    external: ExternalId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let target = reader.external(external).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::External(external),
    })?;
    write_text(root, output, "external(id=")?;
    write_number(root, output, u64::from(external.raw))?;
    match target {
        ExternalTarget::Stable { target } => {
            write_text(root, output, ",stable.fragment=")?;
            emit_bytes(root, target.fragment.as_ref(), output)?;
            write_text(root, output, ",family=")?;
            emit_bytes(root, target.declaration.family.as_bytes(), output)?;
            write_text(root, output, ",variant=")?;
            emit_bytes(root, target.declaration.variant.as_bytes(), output)?;
        }
        ExternalTarget::Foreign(target) => {
            write_text(root, output, ",foreign=")?;
            emit_bytes(root, target.identity.foreign.as_bytes(), output)?;
            write_text(root, output, ",variant=")?;
            match target.identity.variant {
                crate::VariantAvailability::Known(variant) => emit_bytes(root, variant.as_bytes(), output)?,
                crate::VariantAvailability::Unavailable => write_text(root, output, "unavailable")?,
            }
            write_text(root, output, ",origin=")?;
            emit_foreign_origin(reader, root, owner, target.origin, output)?;
            write_text(root, output, ",path=")?;
            emit_atom(reader, root, owner, target.path, output)?;
            write_text(root, output, ",display=")?;
            emit_atom(reader, root, owner, target.display, output)?;
            write_text(root, output, ",kind=")?;
            match target.kind {
                Some(kind) => write_text(root, output, item_kind_name(kind))?,
                None => write_text(root, output, "none")?,
            }
        }
        ExternalTarget::FragmentEntity { target, display } => {
            write_text(root, output, ",fragment-entity.fragment=")?;
            emit_bytes(root, target.fragment.as_ref(), output)?;
            write_text(root, output, ",ordinal=")?;
            write_number(root, output, u64::from(target.ordinal))?;
            write_text(root, output, ",display=")?;
            emit_atom(reader, root, owner, display, output)?;
        }
    }
    write_text(root, output, ")")
}

fn emit_foreign_origin<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    origin: ForeignTargetOrigin,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match origin {
        ForeignTargetOrigin::Package { ecosystem, package } => {
            write_text(root, output, "package(")?;
            emit_atom(reader, root, owner, ecosystem, output)?;
            write_text(root, output, ",")?;
            emit_atom(reader, root, owner, package, output)?;
            write_text(root, output, ")")
        }
        ForeignTargetOrigin::Namespace { ecosystem, namespace } => {
            write_text(root, output, "namespace(")?;
            emit_atom(reader, root, owner, ecosystem, output)?;
            write_text(root, output, ",")?;
            emit_atom(reader, root, owner, namespace, output)?;
            write_text(root, output, ")")
        }
        ForeignTargetOrigin::Universe { ecosystem } => {
            tagged_atom(reader, root, owner, "universe", ecosystem, output)
        }
        ForeignTargetOrigin::Unspecified { ecosystem } => {
            tagged_atom(reader, root, owner, "unspecified", ecosystem, output)
        }
    }
}

fn emit_array_shape<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    shape: ArrayShape,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match shape {
        ArrayShape::Sequence => write_text(root, output, "sequence"),
        ArrayShape::Rectangular { rank } => {
            write_text(root, output, "rectangular(")?;
            write_number(root, output, u64::from(rank.get()))?;
            write_text(root, output, ")")
        }
        ArrayShape::FixedValue { length } => {
            write_text(root, output, "fixed(")?;
            write_number(root, output, length)?;
            write_text(root, output, ")")
        }
        ArrayShape::ConstExpression(atom) => tagged_atom(reader, root, owner, "const", atom, output),
        ArrayShape::Incomplete => write_text(root, output, "incomplete"),
    }
}

fn emit_wildcard<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    bound: WildcardBound,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match bound {
        WildcardBound::Unbounded => write_text(root, output, "wildcard"),
        WildcardBound::Extends(target) => unary(reader, root, owner, depth, limits, "wildcard.extends", target, output),
        WildcardBound::Super(target) => unary(reader, root, owner, depth, limits, "wildcard.super", target, output),
    }
}

fn emit_qualified_segments<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    segments: QualifiedSegments,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match segments {
        QualifiedSegments::Captured(list) => emit_atom_list(reader, root, owner, list, output),
        QualifiedSegments::Unavailable => write_text(root, output, "unavailable"),
    }
}

fn emit_type_query<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    query: TypeQuery,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, "typeof(")?;
    match query {
        TypeQuery::Entity(entity) => {
            if reader.entity(entity).is_none() {
                return Err(CanonicalTypeRenderError::MissingReference {
                    owner,
                    reference: CanonicalTypeRenderReference::Entity(entity),
                });
            }
            write_text(root, output, "entity=")?;
            write_number(root, output, u64::from(entity.raw))?;
        }
        TypeQuery::Path(path) => {
            write_text(root, output, "path=")?;
            emit_atom_list(reader, root, owner, path, output)?;
        }
        TypeQuery::External(external) => {
            emit_external(reader, root, owner, external, output)?;
        }
    }
    write_text(root, output, ")")
}

const fn missing_type(owner: TypeId, target: TypeId) -> CanonicalTypeRenderError {
    CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::Type(target),
    }
}
