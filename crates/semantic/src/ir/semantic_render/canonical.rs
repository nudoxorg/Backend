//! Canonical, language-neutral type rendering over the complete semantic image.
//!
//! This is deliberately not source syntax. It is a compact structural form for
//! discovery, comparison, and diagnostics that gives every semantic type
//! constructor one unambiguous spelling. Language renderers may consume the
//! same reader later, but cannot redefine this representation's meaning.

use core::{fmt, num::NonZeroUsize, ops::Deref, str};

use thiserror::Error;

use crate::ir::{
    ArrayShape, AtomId, AtomListId, ComputedType, ConcreteType, EntityId, ExternalId,
    ExternalTarget, ForeignTargetOrigin, LiteralType, ObjectMemberListId, QualifiedSegments,
    SemanticReader, TemplatePartListId, TupleElementListId, TypeExpr, TypeId, TypeListId,
    TypeQuery, WildcardBound,
};

#[path = "canonical/names.rs"]
mod names;
#[path = "canonical/output.rs"]
mod output;
#[path = "canonical/traverse.rs"]
mod traverse;
#[path = "canonical/emit.rs"]
mod emit;

use names::*;
use output::*;
use traverse::*;
use emit::{emit_type, missing_type};

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
