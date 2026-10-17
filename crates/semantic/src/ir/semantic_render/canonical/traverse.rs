//! Stack-independent type traversal. Each pooled iterator retains only its next
//! row; pending work is bounded by node depth, never the width of a type list.

use core::fmt;

use crate::ir::{AtomListId, SemanticReader, TypeId};

use super::output::{emit_atom, emit_optional_atom, write_bool, write_number, write_text};
use super::steps::{Step, object_steps, template_steps, tuple_steps, type_steps};
use super::{
    CanonicalTypeRenderError, CanonicalTypeRenderLimits, CanonicalTypeRenderReference,
    emit_array_shape, emit_external, emit_literal, emit_nominal, emit_qualified_segments,
    emit_type_query, missing_type,
};

enum Work<'image, Reader: SemanticReader + ?Sized + 'image> {
    Step(Step),
    Types(Reader::Types<'image>, bool),
    Tuples(Reader::TupleElements<'image>, bool),
    Objects(Reader::ObjectMembers<'image>, bool),
    Template(Reader::TemplateParts<'image>, bool),
}

struct Frame<'image, Reader: SemanticReader + ?Sized + 'image> {
    owner: TypeId,
    depth: usize,
    work: Work<'image, Reader>,
}

pub(super) fn emit_type<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    root: TypeId,
    current: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let mut pending: Vec<Frame<'image, Reader>> = vec![Frame {
        owner: current,
        depth,
        work: Work::Step(Step::Type),
    }];
    // Every expansion has a fixed number of steps. Reuse this scratch capacity
    // rather than allocating strings or materializing a whole pooled list.
    let mut steps = Vec::new();
    while let Some(Frame { owner, depth, work }) = pending.pop() {
        match work {
            Work::Step(step) => match step {
                Step::Type => {
                    if depth > limits.maximum_depth.get() {
                        return Err(CanonicalTypeRenderError::TraversalLimit {
                            root,
                            at: owner,
                            limit: limits.maximum_depth,
                        });
                    }
                    let ty = reader.ty(owner).ok_or(missing_type(owner, owner))?;
                    type_steps(ty, &mut steps);
                }
                Step::Child(target) => {
                    // Preserve the reference's actual owner and the existing
                    // missing-reference precedence over a depth refusal.
                    if reader.ty(target).is_none() {
                        return Err(missing_type(owner, target));
                    }
                    let depth =
                        depth
                            .checked_add(1)
                            .ok_or(CanonicalTypeRenderError::TraversalLimit {
                                root,
                                at: target,
                                limit: limits.maximum_depth,
                            })?;
                    pending.push(Frame {
                        owner: target,
                        depth,
                        work: Work::Step(Step::Type),
                    });
                }
                Step::Text(value) => write_text(root, output, value)?,
                Step::Bool(value) => write_bool(root, output, value)?,
                Step::Number(value) => write_number(root, output, value)?,
                Step::Atom(atom) => emit_atom(reader, root, owner, atom, output)?,
                Step::OptionalAtom(atom) => emit_optional_atom(reader, root, owner, atom, output)?,
                Step::Atoms(list) => emit_atom_list(reader, root, owner, list, output)?,
                Step::Literal(value) => emit_literal(reader, root, owner, value, output)?,
                Step::Nominal(entity) => emit_nominal(reader, root, owner, entity, output)?,
                Step::External(external) => emit_external(reader, root, owner, external, output)?,
                Step::ArrayShape(shape) => emit_array_shape(reader, root, owner, shape, output)?,
                Step::QualifiedSegments(segments) => {
                    emit_qualified_segments(reader, root, owner, segments, output)?
                }
                Step::TypeQuery(query) => emit_type_query(reader, root, owner, query, output)?,
                Step::Types(list) => {
                    let rows =
                        reader
                            .types(list)
                            .ok_or(CanonicalTypeRenderError::MissingReference {
                                owner,
                                reference: CanonicalTypeRenderReference::TypeList(list),
                            })?;
                    write_text(root, output, "[")?;
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Types(rows, true),
                    });
                }
                Step::Tuples(list) => {
                    let rows = reader.tuple_elements(list).ok_or(
                        CanonicalTypeRenderError::MissingReference {
                            owner,
                            reference: CanonicalTypeRenderReference::TupleElements(list),
                        },
                    )?;
                    write_text(root, output, "[")?;
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Tuples(rows, true),
                    });
                }
                Step::Objects(list) => {
                    let rows = reader.object_members(list).ok_or(
                        CanonicalTypeRenderError::MissingReference {
                            owner,
                            reference: CanonicalTypeRenderReference::ObjectMembers(list),
                        },
                    )?;
                    write_text(root, output, "[")?;
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Objects(rows, true),
                    });
                }
                Step::Template(list) => {
                    let rows = reader.template_parts(list).ok_or(
                        CanonicalTypeRenderError::MissingReference {
                            owner,
                            reference: CanonicalTypeRenderReference::TemplateParts(list),
                        },
                    )?;
                    write_text(root, output, "[")?;
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Template(rows, true),
                    });
                }
            },
            Work::Types(mut rows, first) => {
                if let Some(target) = rows.next() {
                    if !first {
                        write_text(root, output, ",")?;
                    }
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Types(rows, false),
                    });
                    steps.push(Step::Child(target));
                } else {
                    write_text(root, output, "]")?;
                }
            }
            Work::Tuples(mut rows, first) => {
                if let Some(row) = rows.next() {
                    if !first {
                        write_text(root, output, ",")?;
                    }
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Tuples(rows, false),
                    });
                    tuple_steps(row, &mut steps);
                } else {
                    write_text(root, output, "]")?;
                }
            }
            Work::Objects(mut rows, first) => {
                if let Some(row) = rows.next() {
                    if !first {
                        write_text(root, output, ",")?;
                    }
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Objects(rows, false),
                    });
                    object_steps(row, &mut steps);
                } else {
                    write_text(root, output, "]")?;
                }
            }
            Work::Template(mut rows, first) => {
                if let Some(row) = rows.next() {
                    if !first {
                        write_text(root, output, ",")?;
                    }
                    pending.push(Frame {
                        owner,
                        depth,
                        work: Work::Template(rows, false),
                    });
                    template_steps(row, &mut steps);
                } else {
                    write_text(root, output, "]")?;
                }
            }
        }
        // LIFO storage, canonical left-to-right execution. Only this one node
        // (or one pooled row) is scheduled before its first child is visited.
        pending.extend(steps.drain(..).rev().map(|step| Frame {
            owner,
            depth,
            work: Work::Step(step),
        }));
    }
    Ok(())
}

pub(super) fn emit_atom_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    list: AtomListId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let rows = reader
        .atom_list(list)
        .ok_or(CanonicalTypeRenderError::MissingReference {
            owner,
            reference: CanonicalTypeRenderReference::AtomList(list),
        })?;
    write_text(root, output, "[")?;
    for (index, atom) in rows.enumerate() {
        if index != 0 {
            write_text(root, output, ",")?;
        }
        emit_atom(reader, root, owner, atom, output)?;
    }
    write_text(root, output, "]")
}
