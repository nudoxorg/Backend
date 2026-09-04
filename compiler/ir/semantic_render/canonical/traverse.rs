//! Typed pooled-list traversal and recursive edge emission.

use core::fmt;

use crate::{
    AtomListId, ObjectMember, ObjectMemberListId, PropertyKey, SemanticReader, TemplatePart,
    TemplatePartListId, TupleElement, TupleElementListId, TypeId, TypeListId,
};

use super::{
    CanonicalTypeRenderError, CanonicalTypeRenderLimits, CanonicalTypeRenderReference, emit_type,
    missing_type,
};
use super::names::tuple_kind_name;
use super::output::{emit_atom, emit_optional_atom, tagged_atom, write_bool, write_text};

pub(super) fn emit_tuple_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    list: TupleElementListId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let rows = reader.tuple_elements(list).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::TupleElements(list),
    })?;
    write_text(root, output, "[")?;
    for (index, row) in rows.enumerate() {
        if index != 0 { write_text(root, output, ",")?; }
        emit_tuple_element(reader, root, owner, row, depth, limits, output)?;
    }
    write_text(root, output, "]")
}

fn emit_tuple_element<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    row: TupleElement,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, "element(kind=")?;
    write_text(root, output, tuple_kind_name(row.kind))?;
    write_text(root, output, ",label=")?;
    emit_optional_atom(reader, root, owner, row.label, output)?;
    write_text(root, output, ",type=")?;
    child(reader, root, owner, row.ty, depth, limits, output)?;
    write_text(root, output, ")")
}

pub(super) fn emit_object_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    list: ObjectMemberListId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let rows = reader.object_members(list).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::ObjectMembers(list),
    })?;
    write_text(root, output, "[")?;
    for (index, row) in rows.enumerate() {
        if index != 0 { write_text(root, output, ",")?; }
        emit_object_member(reader, root, owner, row, depth, limits, output)?;
    }
    write_text(root, output, "]")
}

fn emit_object_member<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    row: ObjectMember,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match row {
        ObjectMember::Property { key, ty, optional, readonly } => {
            write_text(root, output, "property(key=")?;
            emit_property_key(reader, root, owner, key, depth, limits, output)?;
            write_text(root, output, ",type=")?;
            child(reader, root, owner, ty, depth, limits, output)?;
            write_text(root, output, ",optional=")?;
            write_bool(root, output, optional)?;
            write_text(root, output, ",readonly=")?;
            write_bool(root, output, readonly)?;
            write_text(root, output, ")")
        }
        ObjectMember::Method { key, signature, optional } => {
            write_text(root, output, "method(key=")?;
            emit_property_key(reader, root, owner, key, depth, limits, output)?;
            write_text(root, output, ",signature=")?;
            child(reader, root, owner, signature, depth, limits, output)?;
            write_text(root, output, ",optional=")?;
            write_bool(root, output, optional)?;
            write_text(root, output, ")")
        }
        ObjectMember::Index { parameter, key, value, readonly } => {
            write_text(root, output, "index(parameter=")?;
            emit_atom(reader, root, owner, parameter, output)?;
            write_text(root, output, ",key=")?;
            child(reader, root, owner, key, depth, limits, output)?;
            write_text(root, output, ",value=")?;
            child(reader, root, owner, value, depth, limits, output)?;
            write_text(root, output, ",readonly=")?;
            write_bool(root, output, readonly)?;
            write_text(root, output, ")")
        }
        ObjectMember::Call(signature) => unary(reader, root, owner, depth, limits, "call", signature, output),
        ObjectMember::Construct(signature) => unary(reader, root, owner, depth, limits, "construct", signature, output),
    }
}

fn emit_property_key<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    key: PropertyKey,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match key {
        PropertyKey::Named(atom) => tagged_atom(reader, root, owner, "named", atom, output),
        PropertyKey::Private(atom) => tagged_atom(reader, root, owner, "private", atom, output),
        PropertyKey::Numeric(atom) => tagged_atom(reader, root, owner, "numeric", atom, output),
        PropertyKey::Computed(ty) => unary(reader, root, owner, depth, limits, "computed", ty, output),
    }
}

pub(super) fn emit_template_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    list: TemplatePartListId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let rows = reader.template_parts(list).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::TemplateParts(list),
    })?;
    write_text(root, output, "[")?;
    for (index, row) in rows.enumerate() {
        if index != 0 { write_text(root, output, ",")?; }
        match row {
            TemplatePart::Bytes(atom) => tagged_atom(reader, root, owner, "bytes", atom, output)?,
            TemplatePart::Placeholder(ty) => unary(reader, root, owner, depth, limits, "placeholder", ty, output)?,
        }
    }
    write_text(root, output, "]")
}

pub(super) fn tagged_type_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    tag: &str,
    list: TypeListId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, tag)?;
    emit_type_list(reader, root, owner, list, depth, limits, output)
}

pub(super) fn emit_type_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    list: TypeListId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let rows = reader.types(list).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::TypeList(list),
    })?;
    write_text(root, output, "[")?;
    for (index, ty) in rows.enumerate() {
        if index != 0 { write_text(root, output, ",")?; }
        child(reader, root, owner, ty, depth, limits, output)?;
    }
    write_text(root, output, "]")
}

pub(super) fn emit_atom_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    list: AtomListId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let rows = reader.atom_list(list).ok_or(CanonicalTypeRenderError::MissingReference {
        owner,
        reference: CanonicalTypeRenderReference::AtomList(list),
    })?;
    write_text(root, output, "[")?;
    for (index, atom) in rows.enumerate() {
        if index != 0 { write_text(root, output, ",")?; }
        emit_atom(reader, root, owner, atom, output)?;
    }
    write_text(root, output, "]")
}

pub(super) fn unary<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    tag: &str,
    target: TypeId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, tag)?;
    write_text(root, output, "(")?;
    child(reader, root, owner, target, depth, limits, output)?;
    write_text(root, output, ")")
}

pub(super) fn child<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    target: TypeId,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    if reader.ty(target).is_none() {
        return Err(missing_type(owner, target));
    }
    emit_type(reader, root, target, depth.saturating_add(1), limits, output)
}

pub(super) fn emit_optional_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    target: Option<TypeId>,
    depth: usize,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match target {
        Some(target) => child(reader, root, owner, target, depth, limits, output),
        None => write_text(root, output, "none"),
    }
}
