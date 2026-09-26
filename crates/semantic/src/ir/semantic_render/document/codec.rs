//! Shared semantic-document writers.
//!
//! Both the language-neutral frame and the extension planes encode lists,
//! type coordinates, atoms, and counts through this one grammar.

use core::{fmt, str};

use crate::ir::{
    AtomId, AtomListId, CanonicalTypeRenderError, CanonicalTypeRenderLimits,
    CanonicalTypeRenderReference, DeclarationIdentity, EntityId, EntityListId, SemanticReader,
    SourceSpan, TypeId, TypeListId, TypeParameter, TypeParameterBound, TypeParameterBoundListId,
    TypeParameterInference, TypeParameterListId, TypeParameterPrimaryRequirement,
    TypeParameterRequirements, prepare_canonical_type,
};

use super::{SemanticDocumentError, SemanticDocumentReference};

pub(super) fn emit_optional_source_span<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    source: Option<SourceSpan>,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match source {
        None => write_text(entity, output, "none"),
        Some(source) => {
            write_text(entity, output, "span(file=")?;
            emit_atom(reader, entity, source.file(), output)?;
            write_text(entity, output, ",start=")?;
            write_number(entity, output, u64::from(source.start()))?;
            write_text(entity, output, ",end=")?;
            write_number(entity, output, u64::from(source.end()))?;
            write_text(entity, output, ")")
        }
    }
}

pub(super) fn emit_optional_u32(
    entity: EntityId,
    value: Option<u32>,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match value {
        Some(value) => write_number(entity, output, u64::from(value)),
        None => write_text(entity, output, "none"),
    }
}

pub(super) fn emit_optional_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    value: Option<TypeId>,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match value {
        Some(value) => {
            write_text(entity, output, "some(")?;
            emit_type_coordinate(reader, entity, value, type_limits, output)?;
            write_text(entity, output, ")")
        }
        None => write_text(entity, output, "none"),
    }
}

pub(super) fn emit_type_coordinate<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    semantic_type: TypeId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "type=")?;
    emit_canonical_type(reader, entity, semantic_type, type_limits, output)
}

pub(super) fn emit_canonical_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    semantic_type: TypeId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let prepared = prepare_canonical_type(reader, semantic_type, type_limits)
        .map_err(|cause| map_canonical_type_error(entity, semantic_type, cause))?;
    prepared
        .write_to(output)
        .map_err(|cause| map_canonical_type_error(entity, semantic_type, cause))
}

pub(super) fn emit_type_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: TypeListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let types = reader
        .types(list)
        .ok_or(SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::TypeList(list),
        })?;
    write_text(entity, output, "type-list(values=[")?;
    for (index, semantic_type) in types.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_type_coordinate(reader, entity, semantic_type, type_limits, output)?;
    }
    write_text(entity, output, "])")
}

pub(super) fn emit_atom_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: AtomListId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let atoms = reader
        .atom_list(list)
        .ok_or(SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::AtomList(list),
        })?;
    write_text(entity, output, "atom-list(values=[")?;
    for (index, atom) in atoms.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        write_text(entity, output, "atom=")?;
        emit_atom(reader, entity, atom, output)?;
    }
    write_text(entity, output, "])")
}

pub(super) fn emit_entity_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: EntityListId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let entities = reader
        .entity_list(list)
        .ok_or(SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::EntityList(list),
        })?;
    write_text(entity, output, "entity-list(values=[")?;
    for (index, target) in entities.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_entity_coordinate(reader, entity, target, output)?;
    }
    write_text(entity, output, "])")
}

pub(super) fn emit_identity(
    owner: EntityId,
    output: &mut impl fmt::Write,
    identity: DeclarationIdentity,
) -> Result<(), SemanticDocumentError> {
    write_text(owner, output, "identity(family=")?;
    write_bytes(owner, output, identity.family.as_bytes())?;
    write_text(owner, output, ",variant=")?;
    write_bytes(owner, output, identity.variant.as_bytes())?;
    write_text(owner, output, ")")
}

pub(super) fn emit_entity_coordinate<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    target: EntityId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let row = reader
        .entity(target)
        .ok_or(SemanticDocumentError::MissingReference {
            entity: owner,
            reference: SemanticDocumentReference::Entity(target),
        })?;
    write_text(owner, output, "entity(identity=")?;
    emit_identity(owner, output, row.version.identity())?;
    write_text(owner, output, ",name=")?;
    emit_atom(reader, owner, row.name, output)?;
    write_text(owner, output, ")")
}

pub(super) fn emit_type_parameter_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: TypeParameterListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let parameters =
        reader
            .type_parameters(list)
            .ok_or(SemanticDocumentError::MissingReference {
                entity,
                reference: SemanticDocumentReference::TypeParameterList(list),
            })?;
    write_text(entity, output, "type-parameter-list(values=[")?;
    for (index, parameter) in parameters.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_type_parameter(reader, entity, parameter, type_limits, output)?;
    }
    write_text(entity, output, "])")
}

pub(super) fn emit_type_parameter<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    parameter: TypeParameter,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "parameter(name=")?;
    emit_atom(reader, entity, parameter.name, output)?;
    write_text(entity, output, ",bounds=")?;
    emit_type_parameter_bound_list(reader, entity, parameter.bounds, type_limits, output)?;
    write_text(entity, output, ",default=")?;
    emit_optional_type(reader, entity, parameter.default, type_limits, output)?;
    write_text(entity, output, ",variance=")?;
    write_text(entity, output, variance_name(parameter.variance))?;
    write_text(entity, output, ",kind=")?;
    emit_type_parameter_kind(reader, entity, parameter.kind, type_limits, output)?;
    write_text(entity, output, ",requirements=")?;
    emit_type_parameter_requirements(entity, parameter.requirements, output)?;
    write_text(entity, output, ")")
}

pub(super) fn emit_type_parameter_bound_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: TypeParameterBoundListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let bounds =
        reader
            .type_parameter_bounds(list)
            .ok_or(SemanticDocumentError::MissingReference {
                entity,
                reference: SemanticDocumentReference::TypeParameterBoundList(list),
            })?;
    write_text(entity, output, "bound-list(values=[")?;
    for (index, bound) in bounds.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        match bound {
            TypeParameterBound::Type(semantic_type) => {
                emit_type_coordinate(reader, entity, semantic_type, type_limits, output)?;
            }
            TypeParameterBound::Lifetime(atom) => {
                write_text(entity, output, "lifetime(atom=")?;
                emit_atom(reader, entity, atom, output)?;
                write_text(entity, output, ")")?;
            }
        }
    }
    write_text(entity, output, "])")
}

pub(super) fn emit_type_parameter_kind<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    kind: crate::ir::TypeParameterKind,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match kind {
        crate::ir::TypeParameterKind::Type { inference } => {
            write_text(entity, output, "type(inference=")?;
            write_text(entity, output, type_parameter_inference_name(inference))?;
            write_text(entity, output, ")")
        }
        crate::ir::TypeParameterKind::ConstValue { value_type } => {
            write_text(entity, output, "const-value(type=")?;
            emit_type_coordinate(reader, entity, value_type, type_limits, output)?;
            write_text(entity, output, ")")
        }
        crate::ir::TypeParameterKind::Lifetime => write_text(entity, output, "lifetime"),
    }
}

pub(super) fn emit_type_parameter_requirements(
    entity: EntityId,
    requirements: TypeParameterRequirements,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "(primary=")?;
    match requirements.primary {
        TypeParameterPrimaryRequirement::None => write_text(entity, output, "none")?,
        TypeParameterPrimaryRequirement::Reference { nullable } => {
            write_text(entity, output, "reference(nullable=")?;
            write_bool(entity, output, nullable)?;
            write_text(entity, output, ")")?;
        }
        TypeParameterPrimaryRequirement::Value => write_text(entity, output, "value")?,
        TypeParameterPrimaryRequirement::Unmanaged => write_text(entity, output, "unmanaged")?,
        TypeParameterPrimaryRequirement::NotNull => write_text(entity, output, "not-null")?,
        TypeParameterPrimaryRequirement::Default => write_text(entity, output, "default")?,
    }
    write_text(entity, output, ",constructor=")?;
    write_bool(entity, output, requirements.constructor)?;
    write_text(entity, output, ",allows-ref-like=")?;
    write_bool(entity, output, requirements.allows_ref_like)?;
    write_text(entity, output, ")")
}

fn map_canonical_type_error(
    entity: EntityId,
    semantic_type: TypeId,
    cause: CanonicalTypeRenderError,
) -> SemanticDocumentError {
    match cause {
        CanonicalTypeRenderError::MissingRoot { root } => SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::Type(root),
        },
        CanonicalTypeRenderError::MissingReference { reference, .. } => {
            SemanticDocumentError::MissingReference {
                entity,
                reference: map_canonical_reference(reference),
            }
        }
        cause => SemanticDocumentError::CanonicalType {
            entity,
            semantic_type,
            cause,
        },
    }
}

const fn map_canonical_reference(
    reference: CanonicalTypeRenderReference,
) -> SemanticDocumentReference {
    match reference {
        CanonicalTypeRenderReference::Type(value) => SemanticDocumentReference::Type(value),
        CanonicalTypeRenderReference::Atom(value) => SemanticDocumentReference::Atom(value),
        CanonicalTypeRenderReference::TypeList(value) => SemanticDocumentReference::TypeList(value),
        CanonicalTypeRenderReference::AtomList(value) => SemanticDocumentReference::AtomList(value),
        CanonicalTypeRenderReference::TupleElements(value) => {
            SemanticDocumentReference::TupleElements(value)
        }
        CanonicalTypeRenderReference::ObjectMembers(value) => {
            SemanticDocumentReference::ObjectMembers(value)
        }
        CanonicalTypeRenderReference::TemplateParts(value) => {
            SemanticDocumentReference::TemplateParts(value)
        }
        CanonicalTypeRenderReference::Entity(value) => SemanticDocumentReference::Entity(value),
        CanonicalTypeRenderReference::External(value) => SemanticDocumentReference::External(value),
    }
}

const fn variance_name(value: crate::ir::Variance) -> &'static str {
    match value {
        crate::ir::Variance::Invariant => "invariant",
        crate::ir::Variance::Covariant => "covariant",
        crate::ir::Variance::Contravariant => "contravariant",
        crate::ir::Variance::Bivariant => "bivariant",
    }
}

const fn type_parameter_inference_name(value: TypeParameterInference) -> &'static str {
    match value {
        TypeParameterInference::Ordinary => "ordinary",
        TypeParameterInference::Const => "const",
    }
}

pub(super) fn emit_atom<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    atom: AtomId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let bytes = reader
        .atom(atom)
        .ok_or(SemanticDocumentError::MissingReference {
            entity: owner,
            reference: SemanticDocumentReference::Atom(atom),
        })?;
    write_bytes(owner, output, bytes)
}

pub(super) fn emit_text<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    text: crate::ir::TextId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let text = reader
        .text(text)
        .ok_or(SemanticDocumentError::MissingReference {
            entity: owner,
            reference: SemanticDocumentReference::Text(text),
        })?;
    write_bytes(owner, output, text.as_bytes())
}

pub(super) fn write_bytes(
    entity: EntityId,
    output: &mut impl fmt::Write,
    bytes: &[u8],
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "x\"")?;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in bytes {
        let high = char::from(HEX[usize::from(*byte >> 4)]);
        let low = char::from(HEX[usize::from(*byte & 0x0F)]);
        output
            .write_char(high)
            .and_then(|()| output.write_char(low))
            .map_err(|_| SemanticDocumentError::OutputWrite { entity })?;
    }
    write_text(entity, output, "\"")
}

pub(super) fn write_number(
    entity: EntityId,
    output: &mut impl fmt::Write,
    value: u64,
) -> Result<(), SemanticDocumentError> {
    fmt::write(output, format_args!("{value}"))
        .map_err(|_| SemanticDocumentError::OutputWrite { entity })
}

pub(super) fn write_signed(
    entity: EntityId,
    output: &mut impl fmt::Write,
    value: i64,
) -> Result<(), SemanticDocumentError> {
    fmt::write(output, format_args!("{value}"))
        .map_err(|_| SemanticDocumentError::OutputWrite { entity })
}

pub(super) fn write_bool(
    entity: EntityId,
    output: &mut impl fmt::Write,
    value: bool,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, if value { "true" } else { "false" })
}

pub(super) fn write_text(
    entity: EntityId,
    output: &mut impl fmt::Write,
    text: &str,
) -> Result<(), SemanticDocumentError> {
    output
        .write_str(text)
        .map_err(|_| SemanticDocumentError::OutputWrite { entity })
}

#[derive(Default)]
pub(super) struct CountWriter {
    pub(super) length: Option<usize>,
}

impl CountWriter {
    pub(super) const fn new() -> Self {
        Self { length: Some(0) }
    }
}

impl fmt::Write for CountWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.length = self
            .length
            .and_then(|length| length.checked_add(value.len()));
        Ok(())
    }
}

pub(super) struct ByteWriter<'output> {
    output: &'output mut [u8],
    pub(super) written: usize,
}

impl<'output> ByteWriter<'output> {
    pub(super) const fn new(output: &'output mut [u8]) -> Self {
        Self { output, written: 0 }
    }
}

impl fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.output.get_mut(self.written..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}
