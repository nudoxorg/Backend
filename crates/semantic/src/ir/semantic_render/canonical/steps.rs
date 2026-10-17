//! A bounded node expansion in canonical output order. Children are work items,
//! never Rust calls; pooled rows are expanded separately, one at a time.

use crate::ir::{
    ArrayShape, AtomId, AtomListId, ComputedType, ConcreteType, EntityId, ExternalId, LiteralType,
    ObjectMember, ObjectMemberListId, PropertyKey, QualifiedSegments, TemplatePartListId,
    TupleElement, TupleElementListId, TypeExpr, TypeId, TypeListId, TypeQuery, WildcardBound,
};

use super::names::*;

pub(super) enum Step {
    Type,
    Child(TypeId),
    Text(&'static str),
    Bool(bool),
    Number(u64),
    Atom(AtomId),
    OptionalAtom(Option<AtomId>),
    Atoms(AtomListId),
    Literal(LiteralType),
    Nominal(EntityId),
    External(ExternalId),
    ArrayShape(ArrayShape),
    QualifiedSegments(QualifiedSegments),
    TypeQuery(TypeQuery),
    Types(TypeListId),
    Tuples(TupleElementListId),
    Objects(ObjectMemberListId),
    Template(TemplatePartListId),
}

fn unary(out: &mut Vec<Step>, tag: &'static str, target: TypeId) {
    out.extend([
        Step::Text(tag),
        Step::Text("("),
        Step::Child(target),
        Step::Text(")"),
    ]);
}

fn tagged_atom(out: &mut Vec<Step>, tag: &'static str, atom: AtomId) {
    out.extend([
        Step::Text(tag),
        Step::Text("("),
        Step::Atom(atom),
        Step::Text(")"),
    ]);
}

fn optional_type(out: &mut Vec<Step>, target: Option<TypeId>) {
    out.push(match target {
        Some(target) => Step::Child(target),
        None => Step::Text("none"),
    });
}

pub(super) fn type_steps(ty: TypeExpr, out: &mut Vec<Step>) {
    match ty {
        TypeExpr::Concrete(ty) => concrete_steps(ty, out),
        TypeExpr::Computed(ty) => computed_steps(ty, out),
        TypeExpr::Unknown(ty) => {
            out.extend([Step::Text("unknown("), Step::Text(unknown_name(ty.reason))]);
            if let Some(spelling) = ty.spelling {
                out.extend([Step::Text(","), Step::Atom(spelling)]);
            }
            out.push(Step::Text(")"));
        }
    }
}

fn concrete_steps(ty: ConcreteType, out: &mut Vec<Step>) {
    use Step::*;
    match ty {
        ConcreteType::Builtin(value) => {
            out.extend([Text("builtin("), Text(builtin_name(value)), Text(")")]);
        }
        ConcreteType::Literal(value) => out.push(Literal(value)),
        ConcreteType::Nominal(entity) => out.push(Nominal(entity)),
        ConcreteType::External(external) => out.push(External(external)),
        ConcreteType::Parameter(atom) => tagged_atom(out, "parameter", atom),
        ConcreteType::Applied {
            constructor,
            arguments,
        } => {
            out.extend([
                Text("applied("),
                Child(constructor),
                Text(","),
                Types(arguments),
                Text(")"),
            ]);
        }
        ConcreteType::Tuple(elements) => out.extend([Text("tuple"), Tuples(elements)]),
        ConcreteType::Object(members) => out.extend([Text("object"), Objects(members)]),
        ConcreteType::Function {
            parameters,
            results,
            abi,
            variadic,
            unsafe_,
        } => {
            out.extend([
                Text("function(parameters="),
                Tuples(parameters),
                Text(",results="),
                Tuples(results),
                Text(",abi="),
                OptionalAtom(abi),
                Text(",variadic="),
                Text(variadic_name(variadic)),
                Text(",unsafe="),
                Bool(unsafe_),
                Text(")"),
            ]);
        }
        ConcreteType::Reference {
            target,
            mutability,
            lifetime,
        } => {
            out.extend([
                Text("reference(mutability="),
                Text(mutability_name(mutability)),
                Text(",lifetime="),
                OptionalAtom(lifetime),
                Text(",target="),
                Child(target),
                Text(")"),
            ]);
        }
        ConcreteType::CxxReference { target, category } => {
            out.extend([
                Text("cxx-reference(category="),
                Text(cxx_reference_name(category)),
                Text(",target="),
                Child(target),
                Text(")"),
            ]);
        }
        ConcreteType::CPointer { target } => unary(out, "c-pointer", target),
        ConcreteType::CxxMemberPointer { owner, member } => {
            out.extend([
                Text("cxx-member-pointer(owner="),
                Child(owner),
                Text(",member="),
                Child(member),
                Text(")"),
            ]);
        }
        ConcreteType::CQualified { target, qualifiers } => {
            out.extend([
                Text("c-qualified(const="),
                Bool(qualifiers.const_),
                Text(",volatile="),
                Bool(qualifiers.volatile),
                Text(",restrict="),
                Bool(qualifiers.restrict),
                Text(",target="),
                Child(target),
                Text(")"),
            ]);
        }
        ConcreteType::CBlockPointer { target } => unary(out, "c-block-pointer", target),
        ConcreteType::NativeCharacter { role, width } => {
            out.extend([
                Text("native-character(role="),
                Text(character_role_name(role)),
                Text(",width="),
                Number(u64::from(width.get())),
                Text(")"),
            ]);
        }
        ConcreteType::Pointer { target, mutability } => {
            out.extend([
                Text("pointer(mutability="),
                Text(mutability_name(mutability)),
                Text(",target="),
                Child(target),
                Text(")"),
            ]);
        }
        ConcreteType::Slice(target) => unary(out, "slice", target),
        ConcreteType::Array { element, shape } => {
            out.extend([
                Text("array(shape="),
                ArrayShape(shape),
                Text(",element="),
                Child(element),
                Text(")"),
            ]);
        }
        ConcreteType::Optional(target) => unary(out, "optional", target),
        ConcreteType::Union(types) => out.extend([Text("union"), Types(types)]),
        ConcreteType::Intersection(types) => out.extend([Text("intersection"), Types(types)]),
        ConcreteType::ImplTrait(types) => out.extend([Text("impl-trait"), Types(types)]),
        ConcreteType::DynTrait(types) => out.extend([Text("dyn-trait"), Types(types)]),
        ConcreteType::Wildcard(bound) => match bound {
            WildcardBound::Unbounded => out.push(Text("wildcard")),
            WildcardBound::Extends(target) => unary(out, "wildcard.extends", target),
            WildcardBound::Super(target) => unary(out, "wildcard.super", target),
        },
        ConcreteType::Annotated { kind, target } => {
            out.extend([
                Text("annotated(kind="),
                Text(annotation_name(kind)),
                Text(",target="),
                Child(target),
                Text(")"),
            ]);
        }
        ConcreteType::Inferred(spelling) => {
            out.extend([Text("inferred("), OptionalAtom(spelling), Text(")")])
        }
        ConcreteType::QualifiedPath {
            self_type,
            trait_type,
            segments,
            spelling,
        } => {
            out.extend([Text("qualified(self="), Child(self_type), Text(",trait=")]);
            optional_type(out, trait_type);
            out.extend([
                Text(",segments="),
                QualifiedSegments(segments),
                Text(",spelling="),
                Atom(spelling),
                Text(")"),
            ]);
        }
        ConcreteType::Map { key, value } => {
            out.extend([
                Text("map(key="),
                Child(key),
                Text(",value="),
                Child(value),
                Text(")"),
            ]);
        }
        ConcreteType::Channel { direction, element } => {
            out.extend([
                Text("channel(direction="),
                Text(channel_name(direction)),
                Text(",element="),
                Child(element),
                Text(")"),
            ]);
        }
    }
}

fn computed_steps(ty: ComputedType, out: &mut Vec<Step>) {
    use Step::*;
    match ty {
        ComputedType::KeyOf(target) => unary(out, "keyof", target),
        ComputedType::TypeOf(query) => out.push(TypeQuery(query)),
        ComputedType::IndexedAccess { object, index } => {
            out.extend([
                Text("indexed(object="),
                Child(object),
                Text(",index="),
                Child(index),
                Text(")"),
            ]);
        }
        ComputedType::Conditional {
            check,
            extends,
            then_type,
            else_type,
            distributive,
        } => {
            out.extend([
                Text("conditional(check="),
                Child(check),
                Text(",extends="),
                Child(extends),
                Text(",then="),
                Child(then_type),
                Text(",else="),
                Child(else_type),
                Text(",distributive="),
                Bool(distributive),
                Text(")"),
            ]);
        }
        ComputedType::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            readonly,
            optional,
        } => {
            out.extend([
                Text("mapped(parameter="),
                Atom(parameter),
                Text(",constraint="),
                Child(constraint),
                Text(",as="),
            ]);
            optional_type(out, name_as);
            out.extend([
                Text(",value="),
                Child(value),
                Text(",readonly="),
                Text(modifier_name(readonly)),
                Text(",optional="),
                Text(modifier_name(optional)),
                Text(")"),
            ]);
        }
        ComputedType::Infer {
            parameter,
            constraint,
        } => {
            out.extend([
                Text("infer(parameter="),
                Atom(parameter),
                Text(",constraint="),
            ]);
            optional_type(out, constraint);
            out.push(Text(")"));
        }
        ComputedType::TemplateLiteral(parts) => out.extend([Text("template"), Template(parts)]),
        ComputedType::Import {
            specifier,
            qualifier,
            arguments,
        } => {
            out.extend([
                Text("import(specifier="),
                Atom(specifier),
                Text(",qualifier="),
                Atoms(qualifier),
                Text(",arguments="),
                Types(arguments),
                Text(")"),
            ]);
        }
        ComputedType::Awaited(target) => unary(out, "awaited", target),
        ComputedType::This => out.push(Text("this")),
    }
}

pub(super) fn tuple_steps(row: TupleElement, out: &mut Vec<Step>) {
    use Step::*;
    out.extend([
        Text("element(kind="),
        Text(tuple_kind_name(row.kind)),
        Text(",label="),
        OptionalAtom(row.label),
        Text(",type="),
        Child(row.ty),
        Text(")"),
    ]);
}

fn property_steps(key: PropertyKey, out: &mut Vec<Step>) {
    match key {
        PropertyKey::Named(atom) => tagged_atom(out, "named", atom),
        PropertyKey::Private(atom) => tagged_atom(out, "private", atom),
        PropertyKey::Numeric(atom) => tagged_atom(out, "numeric", atom),
        PropertyKey::Computed(ty) => unary(out, "computed", ty),
    }
}

pub(super) fn object_steps(row: ObjectMember, out: &mut Vec<Step>) {
    use Step::*;
    match row {
        ObjectMember::Property {
            key,
            ty,
            optional,
            readonly,
        } => {
            out.push(Text("property(key="));
            property_steps(key, out);
            out.extend([
                Text(",type="),
                Child(ty),
                Text(",optional="),
                Bool(optional),
                Text(",readonly="),
                Bool(readonly),
                Text(")"),
            ]);
        }
        ObjectMember::Method {
            key,
            signature,
            optional,
        } => {
            out.push(Text("method(key="));
            property_steps(key, out);
            out.extend([
                Text(",signature="),
                Child(signature),
                Text(",optional="),
                Bool(optional),
                Text(")"),
            ]);
        }
        ObjectMember::Index {
            parameter,
            key,
            value,
            readonly,
        } => {
            out.extend([
                Text("index(parameter="),
                Atom(parameter),
                Text(",key="),
                Child(key),
                Text(",value="),
                Child(value),
                Text(",readonly="),
                Bool(readonly),
                Text(")"),
            ]);
        }
        ObjectMember::Call(signature) => unary(out, "call", signature),
        ObjectMember::Construct(signature) => unary(out, "construct", signature),
    }
}

pub(super) fn template_steps(row: crate::ir::TemplatePart, out: &mut Vec<Step>) {
    match row {
        crate::ir::TemplatePart::Bytes(atom) => tagged_atom(out, "bytes", atom),
        crate::ir::TemplatePart::Placeholder(ty) => unary(out, "placeholder", ty),
    }
}
