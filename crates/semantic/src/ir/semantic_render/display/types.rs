//! Source-syntax rendering of one semantic type.
//!
//! Concrete and computed types share one depth-bounded walk. A signature or
//! type display calls into this walk; documentation and embeddings do not.

use super::{
    MAX_TYPE_DEPTH, builtin_name, external_display, external_path, unknown_name, write_atom,
    write_c_qualifier_prefix, write_native_character,
};
use crate::ir::{
    AnnotationKind, ArrayShape, ChannelDirection, ComputedType, ConcreteType, Ir, LiteralType,
    MappedModifier, Mutability, ObjectMember, PropertyKey, TemplatePart, TupleElementKind,
    TypeExpr, TypeId, TypeQuery, VariadicForm, WildcardBound,
};
use core::fmt;

pub(super) fn write_type(
    output: &mut impl fmt::Write,
    ir: &Ir,
    id: TypeId,
    depth: u8,
) -> fmt::Result {
    if depth >= MAX_TYPE_DEPTH {
        return output.write_str("…");
    }
    let Some(ty) = ir.ty(id) else {
        return output.write_str("?dangling");
    };
    match ty {
        TypeExpr::Concrete(concrete) => write_concrete_type(output, ir, concrete, depth),
        TypeExpr::Computed(computed) => write_computed_type(output, ir, computed, depth),
        TypeExpr::Unknown(unknown) => {
            write!(output, "?{}", unknown_name(unknown.reason))?;
            if let Some(spelling) = unknown.spelling {
                write!(output, "(")?;
                write_atom(output, ir.atom(spelling).ok_or(fmt::Error)?)?;
                write!(output, ")")?;
            }
            Ok(())
        }
    }
}

fn write_concrete_type(
    output: &mut impl fmt::Write,
    ir: &Ir,
    ty: ConcreteType,
    depth: u8,
) -> fmt::Result {
    let next = depth + 1;
    match ty {
        ConcreteType::Builtin(builtin) => output.write_str(builtin_name(builtin)),
        ConcreteType::Literal(literal) => write_literal(output, ir, literal),
        ConcreteType::Nominal(entity) => match ir.item(entity) {
            Some(item) => write_atom(output, item.name()),
            None => output.write_str("?entity"),
        },
        ConcreteType::External(external) => {
            match ir.external(external).and_then(external_display) {
                Some(display) => write_atom(output, ir.atom(display).unwrap_or(b"?external")),
                None => output.write_str("?external"),
            }
        }
        ConcreteType::Parameter(name) => write_atom(output, ir.atom(name).unwrap_or(b"?generic")),
        ConcreteType::Applied {
            constructor,
            arguments,
        } => {
            write_type(output, ir, constructor, next)?;
            output.write_str("<")?;
            write_type_list(output, ir, arguments, next, ", ")?;
            output.write_str(">")
        }
        ConcreteType::Tuple(elements) => {
            output.write_str("(")?;
            let elements = ir.tuple_elements(elements).unwrap_or(&[]);
            for (index, element) in elements.iter().enumerate() {
                if index != 0 {
                    output.write_str(", ")?;
                }
                if element.kind == TupleElementKind::Rest {
                    output.write_str("...")?;
                }
                if let Some(label) = element.label {
                    write_atom(output, ir.atom(label).unwrap_or(b"?"))?;
                    if element.kind == TupleElementKind::Optional {
                        output.write_str("?")?;
                    }
                    output.write_str(": ")?;
                }
                write_type(output, ir, element.ty, next)?;
            }
            if elements.len() == 1 {
                output.write_str(",")?;
            }
            output.write_str(")")
        }
        ConcreteType::Object(members) => write_object(output, ir, members, next),
        ConcreteType::Function {
            parameters,
            results,
            variadic,
            unsafe_,
            abi,
        } => {
            output.write_str("fn")?;
            write_function_tail(
                output, ir, parameters, results, variadic, unsafe_, abi, next,
            )
        }
        ConcreteType::Reference {
            target,
            mutability,
            lifetime,
        } => {
            output.write_str("&")?;
            if let Some(lifetime) = lifetime {
                output.write_str("'")?;
                write_atom(output, ir.atom(lifetime).unwrap_or(b"_"))?;
                output.write_str(" ")?;
            }
            if mutability == Mutability::Mutable {
                output.write_str("mut ")?;
            }
            write_type(output, ir, target, next)
        }
        ConcreteType::CxxReference { target, category } => {
            write_type(output, ir, target, next)?;
            output.write_str(match category {
                crate::ir::CxxReferenceCategory::Lvalue => "&",
                crate::ir::CxxReferenceCategory::Rvalue => "&&",
            })
        }
        ConcreteType::CPointer { target } => {
            write_type(output, ir, target, next)?;
            output.write_str("*")
        }
        ConcreteType::CxxMemberPointer { owner, member } => {
            write_type(output, ir, member, next)?;
            output.write_str(" ")?;
            write_type(output, ir, owner, next)?;
            output.write_str("::*")
        }
        ConcreteType::CQualified { target, qualifiers } => {
            write_c_qualifier_prefix(output, qualifiers)?;
            output.write_str(" ")?;
            write_type(output, ir, target, next)
        }
        ConcreteType::CBlockPointer { target } => {
            // Neutral traversal makes the distinct block-pointer semantic
            // form visible without pretending this is a C declarator. A
            // future C-family declarator dialect owns exact `^` placement.
            output.write_str("blockptr<")?;
            write_type(output, ir, target, next)?;
            output.write_str(">")
        }
        ConcreteType::NativeCharacter { role, width } => {
            write_native_character(output, role, width)
        }
        ConcreteType::Pointer { target, mutability } => {
            output.write_str(if mutability == Mutability::Mutable {
                "*mut "
            } else {
                "*const "
            })?;
            write_type(output, ir, target, next)
        }
        ConcreteType::Slice(element) => {
            output.write_str("[")?;
            write_type(output, ir, element, next)?;
            output.write_str("]")
        }
        ConcreteType::Array { element, shape } => match shape {
            ArrayShape::Sequence => {
                output.write_str("[]")?;
                write_type(output, ir, element, next)
            }
            ArrayShape::Rectangular { rank } => {
                write_type(output, ir, element, next)?;
                output.write_str("[")?;
                for _ in 1..rank.get() {
                    output.write_str(",")?;
                }
                output.write_str("]")
            }
            ArrayShape::FixedValue { length } => {
                output.write_str("[")?;
                write_type(output, ir, element, next)?;
                write!(output, "; {length}]")
            }
            ArrayShape::ConstExpression(expression) => {
                output.write_str("[")?;
                write_type(output, ir, element, next)?;
                output.write_str("; ")?;
                write_atom(output, ir.atom(expression).unwrap_or(b"?"))?;
                output.write_str("]")
            }
            ArrayShape::Incomplete => {
                write_type(output, ir, element, next)?;
                output.write_str("[]")
            }
        },
        ConcreteType::Optional(inner) => {
            output.write_str("Option<")?;
            write_type(output, ir, inner, next)?;
            output.write_str(">")
        }
        ConcreteType::Union(types) => write_type_list(output, ir, types, next, " | "),
        ConcreteType::Intersection(types) => write_type_list(output, ir, types, next, " & "),
        ConcreteType::ImplTrait(types) => {
            output.write_str("impl ")?;
            write_type_list(output, ir, types, next, " + ")
        }
        ConcreteType::DynTrait(types) => {
            output.write_str("dyn ")?;
            write_type_list(output, ir, types, next, " + ")
        }
        ConcreteType::Wildcard(WildcardBound::Unbounded) => output.write_str("?"),
        ConcreteType::Wildcard(WildcardBound::Extends(bound)) => {
            output.write_str("? extends ")?;
            write_type(output, ir, bound, next)
        }
        ConcreteType::Wildcard(WildcardBound::Super(bound)) => {
            output.write_str("? super ")?;
            write_type(output, ir, bound, next)
        }
        ConcreteType::Annotated { kind, target } => match kind {
            AnnotationKind::Readonly => {
                output.write_str("readonly ")?;
                write_type(output, ir, target, next)
            }
            AnnotationKind::NullableValue => {
                write_type(output, ir, target, next)?;
                output.write_str("?")
            }
            AnnotationKind::NullableReference => {
                write_type(output, ir, target, next)?;
                output.write_str("?")
            }
            AnnotationKind::NonNullableReference => {
                // C# has no non-null-reference type suffix: `!` is an
                // expression-level null-forgiving operator.  Preserve the
                // semantic annotation in IR while neutral rendering leaves
                // token placement to the C# dialect.
                write_type(output, ir, target, next)
            }
        },
        ConcreteType::Inferred(spelling) => write_atom(
            output,
            spelling.and_then(|atom| ir.atom(atom)).unwrap_or(b"_"),
        ),
        ConcreteType::QualifiedPath { spelling, .. } => {
            write_atom(output, ir.atom(spelling).unwrap_or(b"?qualified"))
        }
        ConcreteType::Map { key, value } => {
            output.write_str("map[")?;
            write_type(output, ir, key, next)?;
            output.write_str("]")?;
            write_type(output, ir, value, next)
        }
        ConcreteType::Channel { direction, element } => {
            output.write_str(match direction {
                ChannelDirection::Both => "chan ",
                ChannelDirection::Send => "chan<- ",
                ChannelDirection::Receive => "<-chan ",
            })?;
            write_type(output, ir, element, next)
        }
    }
}

fn write_computed_type(
    output: &mut impl fmt::Write,
    ir: &Ir,
    ty: ComputedType,
    depth: u8,
) -> fmt::Result {
    let next = depth + 1;
    match ty {
        ComputedType::KeyOf(target) => {
            output.write_str("keyof ")?;
            write_type(output, ir, target, next)
        }
        ComputedType::TypeOf(query) => {
            output.write_str("typeof ")?;
            write_type_query(output, ir, query)
        }
        ComputedType::IndexedAccess { object, index } => {
            write_type(output, ir, object, next)?;
            output.write_str("[")?;
            write_type(output, ir, index, next)?;
            output.write_str("]")
        }
        ComputedType::Conditional {
            check,
            extends,
            then_type,
            else_type,
            distributive,
        } => {
            if !distributive {
                output.write_str("[")?;
            }
            write_type(output, ir, check, next)?;
            if !distributive {
                output.write_str("]")?;
            }
            output.write_str(" extends ")?;
            write_type(output, ir, extends, next)?;
            output.write_str(" ? ")?;
            write_type(output, ir, then_type, next)?;
            output.write_str(" : ")?;
            write_type(output, ir, else_type, next)
        }
        ComputedType::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            readonly,
            optional,
        } => {
            output.write_str("{ ")?;
            write_modifier(output, readonly, "readonly ")?;
            output.write_str("[")?;
            write_atom(output, ir.atom(parameter).unwrap_or(b"?"))?;
            output.write_str(" in ")?;
            write_type(output, ir, constraint, next)?;
            if let Some(name_as) = name_as {
                output.write_str(" as ")?;
                write_type(output, ir, name_as, next)?;
            }
            output.write_str("]")?;
            write_modifier(output, optional, "?")?;
            output.write_str(": ")?;
            write_type(output, ir, value, next)?;
            output.write_str(" }")
        }
        ComputedType::Infer {
            parameter,
            constraint,
        } => {
            output.write_str("infer ")?;
            write_atom(output, ir.atom(parameter).unwrap_or(b"?"))?;
            if let Some(constraint) = constraint {
                output.write_str(" extends ")?;
                write_type(output, ir, constraint, next)?;
            }
            Ok(())
        }
        ComputedType::TemplateLiteral(parts) => {
            output.write_str("`")?;
            for part in ir.template_parts(parts).unwrap_or(&[]) {
                match *part {
                    TemplatePart::Bytes(bytes) => {
                        write_atom(output, ir.atom(bytes).unwrap_or(b""))?
                    }
                    TemplatePart::Placeholder(ty) => {
                        output.write_str("${")?;
                        write_type(output, ir, ty, next)?;
                        output.write_str("}")?;
                    }
                }
            }
            output.write_str("`")
        }
        ComputedType::Import {
            specifier,
            qualifier,
            arguments,
        } => {
            output.write_str("import(\"")?;
            write_atom(output, ir.atom(specifier).unwrap_or(b"?"))?;
            output.write_str("\")")?;
            for component in ir.atom_list(qualifier).unwrap_or(&[]) {
                output.write_str(".")?;
                write_atom(output, ir.atom(*component).unwrap_or(b"?"))?;
            }
            if ir.types(arguments).is_some_and(|types| !types.is_empty()) {
                output.write_str("<")?;
                write_type_list(output, ir, arguments, next, ", ")?;
                output.write_str(">")?;
            }
            Ok(())
        }
        ComputedType::Awaited(target) => {
            output.write_str("Awaited<")?;
            write_type(output, ir, target, next)?;
            output.write_str(">")
        }
        ComputedType::This => output.write_str("this"),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the renderer receives one already-flattened function node and avoids constructing a temporary aggregate"
)]
pub(super) fn write_function_tail(
    output: &mut impl fmt::Write,
    ir: &Ir,
    parameters: crate::ir::TupleElementListId,
    results: crate::ir::TupleElementListId,
    variadic: VariadicForm,
    unsafe_: bool,
    abi: Option<crate::ir::AtomId>,
    depth: u8,
) -> fmt::Result {
    if unsafe_ {
        output.write_str(" unsafe")?;
    }
    if let Some(abi) = abi {
        output.write_str(" extern \"")?;
        write_atom(output, ir.atom(abi).unwrap_or(b"?"))?;
        output.write_str("\"")?;
    }
    output.write_str("(")?;
    let parameters = ir.tuple_elements(parameters).unwrap_or(&[]);
    for (index, parameter) in parameters.iter().enumerate() {
        if index != 0 {
            output.write_str(", ")?;
        }
        if parameter.kind == TupleElementKind::Rest {
            output.write_str("...")?;
        }
        if let Some(label) = parameter.label {
            write_atom(output, ir.atom(label).unwrap_or(b"?"))?;
            if parameter.kind == TupleElementKind::Optional {
                output.write_str("?")?;
            }
            output.write_str(": ")?;
        }
        write_type(output, ir, parameter.ty, depth + 1)?;
    }
    if variadic == VariadicForm::CUnbounded {
        if !parameters.is_empty() {
            output.write_str(", ")?;
        }
        output.write_str("...")?;
    }
    output.write_str(")")?;
    let results = ir.tuple_elements(results).unwrap_or(&[]);
    if results.len() == 1 {
        output.write_str(" -> ")?;
        if let Some(label) = results[0].label {
            // A labelled singleton result is still a role-bearing callable
            // element. Use a neutral one-element result tuple until the
            // selected dialect owns its exact declaration syntax; dropping
            // the label here would make rendering semantically lossy.
            output.write_str("(")?;
            write_atom(output, ir.atom(label).unwrap_or(b"?"))?;
            output.write_str(": ")?;
            write_type(output, ir, results[0].ty, depth + 1)?;
            output.write_str(")")?;
        } else {
            write_type(output, ir, results[0].ty, depth + 1)?;
        }
    } else if results.len() > 1 {
        output.write_str(" -> (")?;
        for (index, result) in results.iter().enumerate() {
            if index != 0 {
                output.write_str(", ")?;
            }
            if let Some(label) = result.label {
                write_atom(output, ir.atom(label).unwrap_or(b"?"))?;
                output.write_str(": ")?;
            }
            write_type(output, ir, result.ty, depth + 1)?;
        }
        output.write_str(")")?;
    }
    Ok(())
}

fn write_type_list(
    output: &mut impl fmt::Write,
    ir: &Ir,
    list: crate::ir::TypeListId,
    depth: u8,
    separator: &str,
) -> fmt::Result {
    for (index, ty) in ir.types(list).unwrap_or(&[]).iter().enumerate() {
        if index != 0 {
            output.write_str(separator)?;
        }
        write_type(output, ir, *ty, depth)?;
    }
    Ok(())
}

fn write_literal(output: &mut impl fmt::Write, ir: &Ir, literal: LiteralType) -> fmt::Result {
    match literal {
        LiteralType::String(value) => {
            output.write_str("\"")?;
            write_atom(output, ir.atom(value).unwrap_or(b""))?;
            output.write_str("\"")
        }
        LiteralType::Number(value) | LiteralType::BigInt(value) => {
            write_atom(output, ir.atom(value).unwrap_or(b"?"))
        }
        LiteralType::Boolean(value) => output.write_str(if value { "true" } else { "false" }),
        LiteralType::Null => output.write_str("null"),
        LiteralType::Undefined => output.write_str("undefined"),
    }
}

fn write_type_query(output: &mut impl fmt::Write, ir: &Ir, query: TypeQuery) -> fmt::Result {
    match query {
        TypeQuery::Entity(entity) => match ir.item(entity) {
            Some(item) => write_atom(output, item.name()),
            None => output.write_str("?entity"),
        },
        TypeQuery::Path(path) => {
            for (index, component) in ir.atom_list(path).unwrap_or(&[]).iter().enumerate() {
                if index != 0 {
                    output.write_str(".")?;
                }
                write_atom(output, ir.atom(*component).unwrap_or(b"?"))?;
            }
            Ok(())
        }
        TypeQuery::External(external) => match ir.external(external).and_then(external_path) {
            Some(path) => write_atom(output, ir.atom(path).unwrap_or(b"?external")),
            None => output.write_str("?external"),
        },
    }
}

fn write_modifier(
    output: &mut impl fmt::Write,
    modifier: MappedModifier,
    spelling: &str,
) -> fmt::Result {
    match modifier {
        MappedModifier::Preserve => Ok(()),
        MappedModifier::Add => {
            output.write_str("+")?;
            output.write_str(spelling)
        }
        MappedModifier::Remove => {
            output.write_str("-")?;
            output.write_str(spelling)
        }
    }
}

fn write_object(
    output: &mut impl fmt::Write,
    ir: &Ir,
    members: crate::ir::ObjectMemberListId,
    depth: u8,
) -> fmt::Result {
    output.write_str("{ ")?;
    for (index, member) in ir.object_members(members).unwrap_or(&[]).iter().enumerate() {
        if index != 0 {
            output.write_str("; ")?;
        }
        match *member {
            ObjectMember::Property {
                key,
                ty,
                optional,
                readonly,
            } => {
                if readonly {
                    output.write_str("readonly ")?;
                }
                write_property_key(output, ir, key, depth)?;
                if optional {
                    output.write_str("?")?;
                }
                output.write_str(": ")?;
                write_type(output, ir, ty, depth)?;
            }
            ObjectMember::Method {
                key,
                signature,
                optional,
            } => {
                write_property_key(output, ir, key, depth)?;
                if optional {
                    output.write_str("?")?;
                }
                if let Some(TypeExpr::Concrete(ConcreteType::Function {
                    parameters,
                    results,
                    variadic,
                    unsafe_,
                    abi,
                })) = ir.ty(signature)
                {
                    write_function_tail(
                        output, ir, parameters, results, variadic, unsafe_, abi, depth,
                    )?;
                } else {
                    output.write_str(": ")?;
                    write_type(output, ir, signature, depth)?;
                }
            }
            ObjectMember::Index {
                parameter,
                key,
                value,
                readonly,
            } => {
                if readonly {
                    output.write_str("readonly ")?;
                }
                output.write_str("[")?;
                write_atom(output, ir.atom(parameter).unwrap_or(b"?"))?;
                output.write_str(": ")?;
                write_type(output, ir, key, depth)?;
                output.write_str("]: ")?;
                write_type(output, ir, value, depth)?;
            }
            ObjectMember::Call(signature) => write_type(output, ir, signature, depth)?,
            ObjectMember::Construct(signature) => {
                output.write_str("new ")?;
                write_type(output, ir, signature, depth)?;
            }
        }
    }
    output.write_str(" }")
}

fn write_property_key(
    output: &mut impl fmt::Write,
    ir: &Ir,
    key: PropertyKey,
    depth: u8,
) -> fmt::Result {
    match key {
        PropertyKey::Named(name) | PropertyKey::Numeric(name) => {
            write_atom(output, ir.atom(name).unwrap_or(b"?"))
        }
        PropertyKey::Private(name) => {
            output.write_str("#")?;
            write_atom(output, ir.atom(name).unwrap_or(b"?"))
        }
        PropertyKey::Computed(ty) => {
            output.write_str("[")?;
            write_type(output, ir, ty, depth + 1)?;
            output.write_str("]")
        }
    }
}
