//! Canonical, language-neutral type rendering over the complete semantic image.
//!
//! This is deliberately not source syntax. It is a compact structural form for
//! discovery, comparison, and diagnostics that gives every semantic type
//! constructor one unambiguous spelling. Language renderers may consume the
//! same reader later, but cannot redefine this representation's meaning.

use core::{fmt, num::NonZeroUsize, ops::Deref, str};

use thiserror::Error;

use crate::ir::{
    ArrayShape, AtomId, AtomListId, EntityId, ExternalId, ExternalTarget, ForeignTargetOrigin,
    LiteralType, ObjectMemberListId, QualifiedSegments, SemanticReader, TemplatePartListId,
    TupleElementListId, TypeId, TypeListId, TypeQuery,
};

#[path = "canonical/names.rs"]
mod names;
#[path = "canonical/output.rs"]
mod output;
#[path = "canonical/steps.rs"]
mod steps;
#[path = "canonical/traverse.rs"]
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
    /// The requested root coordinate is not present in the reader.
    #[error("canonical type root {root:?} is absent")]
    MissingRoot {
        /// Requested root coordinate.
        root: TypeId,
    },
    /// A type node points to a coordinate the reader cannot resolve.
    #[error("canonical type {owner:?} names absent semantic reference {reference:?}")]
    MissingReference {
        /// Type row that carried the rejected coordinate.
        owner: TypeId,
        /// Exact typed coordinate which could not be resolved.
        reference: CanonicalTypeRenderReference,
    },
    /// Traversal would visit a type node deeper than the selected limit.
    #[error("canonical type traversal from {root:?} reached {at:?} beyond depth limit {limit}")]
    TraversalLimit {
        /// Original requested root.
        root: TypeId,
        /// Type row that would exceed the traversal bound.
        at: TypeId,
        /// Caller-selected nonzero depth limit.
        limit: NonZeroUsize,
    },
    /// Computing the exact UTF-8 output length overflowed `usize`.
    #[error("canonical type rendering length overflowed for {root:?}")]
    OutputLengthOverflow {
        /// Requested root whose canonical length overflowed.
        root: TypeId,
    },
    /// The caller's output buffer is shorter than the prepared byte count.
    #[error("canonical type {root:?} needs {required} bytes but caller supplied {available}")]
    OutputTooSmall {
        /// Prepared root.
        root: TypeId,
        /// Exact prepared byte requirement.
        required: usize,
        /// Caller output capacity.
        available: usize,
    },
    /// Writing did not reproduce the exact length established by preparation.
    #[error("prepared canonical type {root:?} wrote {written} bytes rather than {promised}")]
    PreparedLengthMismatch {
        /// Prepared root.
        root: TypeId,
        /// Exact byte length from the validation pass.
        promised: usize,
        /// Bytes actually written before the invariant failed.
        written: usize,
    },
    /// The caller-provided formatter returned an error while writing.
    #[error("the caller formatter rejected canonical type output for {root:?}")]
    OutputWrite {
        /// Root whose output the formatter rejected.
        root: TypeId,
    },
    /// The byte writer produced a sequence that failed UTF-8 validation.
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
    /// Largest permitted traversal depth in semantic type nodes, including the root.
    pub maximum_depth: NonZeroUsize,
}

impl CanonicalTypeRenderLimits {
    /// Sets the maximum traversal depth; the root type counts as depth one.
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
        let written = {
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
            writer.written_len()
        };
        str::from_utf8(&output[..written]).map_err(|source| {
            CanonicalTypeRenderError::OutputEncoding {
                root: self.root,
                valid_up_to: source.valid_up_to(),
                source,
            }
        })
    }

    /// Streams the already-admitted structural form to a caller formatter.
    pub fn write_to(&self, output: &mut impl fmt::Write) -> Result<(), CanonicalTypeRenderError> {
        emit_type(self.reader, self.root, self.root, 1, self.limits, output)
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

fn emit_literal<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    literal: LiteralType,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match literal {
        LiteralType::String(atom) => {
            tagged_atom(reader, root, owner, "literal.string", atom, output)
        }
        LiteralType::Number(atom) => {
            tagged_atom(reader, root, owner, "literal.number", atom, output)
        }
        LiteralType::BigInt(atom) => {
            tagged_atom(reader, root, owner, "literal.bigint", atom, output)
        }
        LiteralType::Boolean(value) => tagged_text(
            root,
            output,
            "literal.boolean",
            if value { "true" } else { "false" },
        ),
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
    write_text(root, output, "nominal(")?;
    emit_entity_reference(reader, root, owner, entity, output)?;
    write_text(root, output, ")")
}

fn emit_entity_reference<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    entity: EntityId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let row = reader
        .entity(entity)
        .ok_or(CanonicalTypeRenderError::MissingReference {
            owner,
            reference: CanonicalTypeRenderReference::Entity(entity),
        })?;
    write_text(root, output, "entity(family=")?;
    emit_bytes(root, row.version.family.as_bytes(), output)?;
    write_text(root, output, ",variant=")?;
    emit_bytes(root, row.version.variant.as_bytes(), output)?;
    match row.name {
        crate::ir::ItemName::Named(name) => {
            write_text(root, output, ",name=")?;
            emit_atom(reader, root, owner, name, output)?;
        }
        crate::ir::ItemName::AnonymousCallable(anchor) => {
            write_text(root, output, ",anonymous-callable-anchor=")?;
            emit_atom(reader, root, owner, anchor, output)?;
        }
    }
    write_text(root, output, ")")
}

fn emit_external<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    external: ExternalId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let target = reader
        .external(external)
        .ok_or(CanonicalTypeRenderError::MissingReference {
            owner,
            reference: CanonicalTypeRenderReference::External(external),
        })?;
    write_text(root, output, "external(")?;
    match target {
        ExternalTarget::Stable { target } => {
            write_text(root, output, "stable.fragment=")?;
            emit_bytes(root, target.fragment.as_ref(), output)?;
            write_text(root, output, ",family=")?;
            emit_bytes(root, target.declaration.family.as_bytes(), output)?;
            write_text(root, output, ",variant=")?;
            emit_bytes(root, target.declaration.variant.as_bytes(), output)?;
        }
        ExternalTarget::Foreign(target) => {
            write_text(root, output, "foreign=")?;
            emit_bytes(root, target.identity.foreign.as_bytes(), output)?;
            write_text(root, output, ",variant=")?;
            match target.identity.variant {
                crate::ir::VariantAvailability::Known(variant) => {
                    emit_bytes(root, variant.as_bytes(), output)?
                }
                crate::ir::VariantAvailability::Unavailable => {
                    write_text(root, output, "unavailable")?
                }
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
            write_text(root, output, "fragment-entity.fragment=")?;
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
        ForeignTargetOrigin::Namespace {
            ecosystem,
            namespace,
        } => {
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
        ArrayShape::ConstExpression(atom) => {
            tagged_atom(reader, root, owner, "const", atom, output)
        }
        ArrayShape::Incomplete => write_text(root, output, "incomplete"),
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
            write_text(root, output, "entity=")?;
            emit_entity_reference(reader, root, owner, entity, output)?;
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
