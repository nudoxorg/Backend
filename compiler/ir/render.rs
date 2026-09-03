//! Allocation-free docs.rs-style rendering over the condensed semantic IR.
//!
//! The `Display` wrappers write directly into any formatter. Callers that want
//! an owned string may use the standard `ToString`; hot paths can stream into
//! an existing `fmt::Write` buffer without an intermediate allocation.

use core::{fmt, str};

use crate::{
    BuiltinType, ComputedType, ConcreteType, DocFragment, EntityId, Ir, ItemKind, ItemView,
    LinkTarget, LiteralType, MappedModifier, Mutability, ObjectMember, PropertyKey, TemplatePart,
    TupleElementKind, TypeExpr, TypeId, TypeQuery, UnknownType, Visibility,
};

const MAX_TYPE_DEPTH: u8 = 96;

/// A declaration signature rendered in familiar docs.rs/Rustdoc syntax.
pub struct SignatureDisplay<'ir> {
    ir: &'ir Ir,
    item: ItemView<'ir>,
}
/// A semantic type rendered in familiar source syntax.
pub struct TypeDisplay<'ir> {
    ir: &'ir Ir,
    ty: TypeId,
}
/// Interned documentation rendered as Markdown suitable for rustdoc/docs.rs.
pub struct DocsDisplay<'ir> {
    ir: &'ir Ir,
    item: ItemView<'ir>,
}
/// Selects semantic lanes streamed into an embedding tokenizer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingProfile(u8);

impl EmbeddingProfile {
    /// Signature only, useful for symbol-name embeddings.
    pub const SYMBOL: Self = Self(0);
    /// Signature plus documentation.
    pub const DOCUMENTED: Self = Self(1);
    /// Signature, documentation, and typed outgoing graph context.
    pub const CONTEXTUAL: Self = Self(3);

    const fn docs(self) -> bool {
        self.0 & 1 != 0
    }

    const fn graph(self) -> bool {
        self.0 & 2 != 0
    }
}

/// Allocation-free semantic text stream for vector embedding.
///
/// This composes existing typed lanes at formatting time and never creates an
/// intermediate `String`; tokenizers can implement `fmt::Write` and consume it
/// directly.
pub struct EmbeddingDisplay<'ir> {
    ir: &'ir Ir,
    item: ItemView<'ir>,
    profile: EmbeddingProfile,
}

impl Ir {
    /// Returns a zero-allocation signature renderer.
    #[must_use]
    pub fn signature(&self, item: EntityId) -> Option<SignatureDisplay<'_>> {
        self.item(item)
            .map(|item| SignatureDisplay { ir: self, item })
    }

    /// Returns a zero-allocation semantic-type renderer.
    #[must_use]
    pub fn display_type(&self, ty: TypeId) -> Option<TypeDisplay<'_>> {
        self.ty(ty).map(|_| TypeDisplay { ir: self, ty })
    }

    /// Returns a zero-allocation Markdown documentation renderer.
    #[must_use]
    pub fn display_docs(&self, item: EntityId) -> Option<DocsDisplay<'_>> {
        self.item(item).map(|item| DocsDisplay { ir: self, item })
    }

    /// Returns a direct semantic stream suitable for vector tokenization.
    #[must_use]
    pub fn embedding_text(
        &self,
        item: EntityId,
        profile: EmbeddingProfile,
    ) -> Option<EmbeddingDisplay<'_>> {
        self.item(item).map(|item| EmbeddingDisplay {
            ir: self,
            item,
            profile,
        })
    }
}

impl fmt::Display for SignatureDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_visibility(formatter, self.item.visibility())?;
        match self.item.kind() {
            ItemKind::Module => formatter.write_str("mod ")?,
            ItemKind::Record => formatter.write_str("struct ")?,
            ItemKind::Field | ItemKind::Parameter | ItemKind::Variant => {}
            ItemKind::Function => formatter.write_str("fn ")?,
            ItemKind::TypeAlias => formatter.write_str("type ")?,
            ItemKind::Trait => formatter.write_str("trait ")?,
            ItemKind::Implementation => formatter.write_str("impl ")?,
            ItemKind::Enum => formatter.write_str("enum ")?,
            ItemKind::Constant => formatter.write_str("const ")?,
            ItemKind::Static => formatter.write_str("static ")?,
            ItemKind::Reexport => formatter.write_str("use ")?,
            ItemKind::Macro => formatter.write_str("macro ")?,
            ItemKind::Namespace => formatter.write_str("namespace ")?,
        }
        write_atom(formatter, self.item.name())?;
        if matches!(self.item.kind(), ItemKind::Record | ItemKind::Enum)
            && !self.item.members().is_empty()
        {
            write_c_body(formatter, self.ir, self.item)?;
            return Ok(());
        }
        if let Some(ty) = self.item.semantic_type() {
            match (self.item.kind(), self.ir.ty(ty)) {
                (
                    ItemKind::Function,
                    Some(TypeExpr::Concrete(ConcreteType::Function {
                        parameters,
                        result,
                        variadic,
                        unsafe_,
                        abi,
                    })),
                ) => {
                    write_function_tail(
                        formatter, self.ir, parameters, result, variadic, unsafe_, abi, 0,
                    )?;
                }
                (ItemKind::TypeAlias | ItemKind::Implementation, _) => {
                    formatter.write_str(" = ")?;
                    write_type(formatter, self.ir, ty, 0)?;
                }
                (
                    ItemKind::Record
                    | ItemKind::Enum
                    | ItemKind::Trait
                    | ItemKind::Module
                    | ItemKind::Namespace,
                    _,
                ) => {}
                _ => {
                    formatter.write_str(": ")?;
                    write_type(formatter, self.ir, ty, 0)?;
                }
            }
        }
        Ok(())
    }
}

fn write_c_body(output: &mut impl fmt::Write, ir: &Ir, item: ItemView<'_>) -> fmt::Result {
    output.write_str(" {")?;
    for member_id in item.members() {
        let Some(member) = ir.item(*member_id) else {
            continue;
        };
        output.write_str("\n    ")?;
        if item.kind() == ItemKind::Enum {
            write_atom(output, member.name())?;
            output.write_str(",")?;
        } else if let Some(ty) = member.semantic_type() {
            write_c_declaration(output, ir, ty, member.name(), 0)?;
            output.write_str(";")?;
        }
    }
    output.write_str("\n};")
}

fn write_c_declaration(
    output: &mut impl fmt::Write,
    ir: &Ir,
    id: TypeId,
    name: &[u8],
    depth: u8,
) -> fmt::Result {
    if depth >= MAX_TYPE_DEPTH {
        return output.write_str("… ");
    }
    let Some(TypeExpr::Concrete(ty)) = ir.ty(id) else {
        write_c_type(output, ir, id, depth)?;
        output.write_str(" ")?;
        return write_atom(output, name);
    };
    match ty {
        ConcreteType::Pointer { target, mutability } => {
            if mutability == Mutability::Immutable {
                output.write_str("const ")?;
            }
            write_c_type(output, ir, target, depth + 1)?;
            output.write_str(" *")?;
            write_atom(output, name)
        }
        ConcreteType::Function {
            parameters,
            result,
            variadic,
            ..
        } => {
            if let Some(result) = result {
                write_c_type(output, ir, result, depth + 1)?;
            } else {
                output.write_str("void")?;
            }
            output.write_str(" (*")?;
            write_atom(output, name)?;
            output.write_str(")(")?;
            write_c_parameters(output, ir, parameters, variadic, depth + 1)?;
            output.write_str(")")
        }
        ConcreteType::Array { element, length } => {
            write_c_type(output, ir, element, depth + 1)?;
            output.write_str(" ")?;
            write_atom(output, name)?;
            output.write_str("[")?;
            if let Some(length) = length {
                write_atom(output, ir.atom(length).unwrap_or(b"?"))?;
            }
            output.write_str("]")
        }
        _ => {
            write_c_type(output, ir, id, depth + 1)?;
            output.write_str(" ")?;
            write_atom(output, name)
        }
    }
}

fn write_c_parameters(
    output: &mut impl fmt::Write,
    ir: &Ir,
    parameters: crate::TupleElementListId,
    variadic: bool,
    depth: u8,
) -> fmt::Result {
    for (index, parameter) in ir
        .tuple_elements(parameters)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        if index != 0 {
            output.write_str(", ")?;
        }
        write_c_type(output, ir, parameter.ty, depth)?;
    }
    if variadic {
        if !ir.tuple_elements(parameters).unwrap_or(&[]).is_empty() {
            output.write_str(", ")?;
        }
        output.write_str("...")?;
    }
    Ok(())
}

fn write_c_type(output: &mut impl fmt::Write, ir: &Ir, id: TypeId, depth: u8) -> fmt::Result {
    if depth >= MAX_TYPE_DEPTH {
        return output.write_str("…");
    }
    let Some(ty) = ir.ty(id) else {
        return output.write_str("?");
    };
    match ty {
        TypeExpr::Concrete(ConcreteType::Builtin(builtin)) => {
            output.write_str(c_builtin_name(builtin))
        }
        TypeExpr::Concrete(ConcreteType::Nominal(entity)) => match ir.item(entity) {
            Some(item) => {
                output.write_str(match item.kind() {
                    ItemKind::Record => "struct ",
                    ItemKind::Enum => "enum ",
                    _ => "",
                })?;
                write_atom(output, item.name())
            }
            None => output.write_str("?"),
        },
        TypeExpr::Concrete(ConcreteType::Pointer { target, mutability }) => {
            if mutability == Mutability::Immutable {
                output.write_str("const ")?;
            }
            write_c_type(output, ir, target, depth + 1)?;
            output.write_str(" *")
        }
        TypeExpr::Concrete(ConcreteType::Function { result, .. }) => {
            if let Some(result) = result {
                write_c_type(output, ir, result, depth + 1)
            } else {
                output.write_str("void")
            }
        }
        TypeExpr::Concrete(ConcreteType::Array { element, .. }) => {
            write_c_type(output, ir, element, depth + 1)
        }
        _ => write_type(output, ir, id, depth),
    }
}

const fn c_builtin_name(builtin: BuiltinType) -> &'static str {
    match builtin {
        BuiltinType::Void | BuiltinType::Unit => "void",
        BuiltinType::Bool => "bool",
        BuiltinType::Char => "char",
        BuiltinType::I8 => "signed char",
        BuiltinType::I16 => "short",
        BuiltinType::I32 => "int",
        BuiltinType::I64 => "long long",
        BuiltinType::I128 => "__int128",
        BuiltinType::U8 => "unsigned char",
        BuiltinType::U16 => "unsigned short",
        BuiltinType::U32 => "unsigned int",
        BuiltinType::U64 => "unsigned long long",
        BuiltinType::U128 => "unsigned __int128",
        BuiltinType::F32 => "float",
        BuiltinType::F64 => "double",
        _ => "unknown",
    }
}

impl fmt::Display for EmbeddingDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        SignatureDisplay {
            ir: self.ir,
            item: self.item,
        }
        .fmt(formatter)?;
        if self.profile.docs() && !self.item.docs().is_empty() {
            formatter.write_str("\n\n")?;
            DocsDisplay {
                ir: self.ir,
                item: self.item,
            }
            .fmt(formatter)?;
        }
        if self.profile.graph() {
            for (_, link) in self.item.links_from() {
                formatter.write_str("\n")?;
                formatter.write_str(link_kind_name(link.kind))?;
                formatter.write_str(": ")?;
                write_embedding_target(formatter, self.ir, link.target)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_type(formatter, self.ir, self.ty, 0)
    }
}

impl fmt::Display for DocsDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for fragment in self.item.docs() {
            match *fragment {
                DocFragment::Text(text) => {
                    formatter.write_str(self.ir.text(text).unwrap_or("�"))?
                }
                DocFragment::Code(text) => {
                    formatter.write_str("`")?;
                    formatter.write_str(self.ir.text(text).unwrap_or("�"))?;
                    formatter.write_str("`")?;
                }
                DocFragment::Link { label, target } => {
                    formatter.write_str("[")?;
                    formatter.write_str(self.ir.text(label).unwrap_or("�"))?;
                    formatter.write_str("](")?;
                    write_link_target(formatter, self.ir, target)?;
                    formatter.write_str(")")?;
                }
                DocFragment::SoftBreak => formatter.write_str(" ")?,
                DocFragment::HardBreak => formatter.write_str("\n\n")?,
            }
        }
        Ok(())
    }
}

fn write_visibility(output: &mut impl fmt::Write, visibility: Visibility) -> fmt::Result {
    match visibility {
        Visibility::Unknown => output.write_str("/* visibility unknown */ "),
        Visibility::Private => Ok(()),
        Visibility::Restricted => output.write_str("pub(restricted) "),
        Visibility::Package => output.write_str("pub(crate) "),
        Visibility::Public => output.write_str("pub "),
    }
}

fn write_type(output: &mut impl fmt::Write, ir: &Ir, id: TypeId, depth: u8) -> fmt::Result {
    if depth >= MAX_TYPE_DEPTH {
        return output.write_str("…");
    }
    let Some(ty) = ir.ty(id) else {
        return output.write_str("?dangling");
    };
    match ty {
        TypeExpr::Concrete(concrete) => write_concrete_type(output, ir, concrete, depth),
        TypeExpr::Computed(computed) => write_computed_type(output, ir, computed, depth),
        TypeExpr::Unknown(reason) => write!(output, "?{}", unknown_name(reason)),
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
        ConcreteType::External(external) => match ir.external(external) {
            Some(target) => write_atom(output, ir.atom(target.display).unwrap_or(b"?external")),
            None => output.write_str("?external"),
        },
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
            result,
            variadic,
            unsafe_,
            abi,
        } => {
            output.write_str("fn")?;
            write_function_tail(output, ir, parameters, result, variadic, unsafe_, abi, next)
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
        ConcreteType::Array { element, length } => {
            output.write_str("[")?;
            write_type(output, ir, element, next)?;
            if let Some(length) = length {
                output.write_str("; ")?;
                write_atom(output, ir.atom(length).unwrap_or(b"?"))?;
            }
            output.write_str("]")
        }
        ConcreteType::Optional(inner) => {
            output.write_str("Option<")?;
            write_type(output, ir, inner, next)?;
            output.write_str(">")
        }
        ConcreteType::Union(types) => write_type_list(output, ir, types, next, " | "),
        ConcreteType::Intersection(types) => write_type_list(output, ir, types, next, " & "),
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
fn write_function_tail(
    output: &mut impl fmt::Write,
    ir: &Ir,
    parameters: crate::TupleElementListId,
    result: Option<TypeId>,
    variadic: bool,
    unsafe_: bool,
    abi: Option<crate::AtomId>,
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
    if variadic {
        if !parameters.is_empty() {
            output.write_str(", ")?;
        }
        output.write_str("...")?;
    }
    output.write_str(")")?;
    if let Some(result) = result {
        output.write_str(" -> ")?;
        write_type(output, ir, result, depth + 1)?;
    }
    Ok(())
}

fn write_type_list(
    output: &mut impl fmt::Write,
    ir: &Ir,
    list: crate::TypeListId,
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
        TypeQuery::External(external) => match ir.external(external) {
            Some(target) => write_atom(output, ir.atom(target.path).unwrap_or(b"?external")),
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
    members: crate::ObjectMemberListId,
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
                    result,
                    variadic,
                    unsafe_,
                    abi,
                })) = ir.ty(signature)
                {
                    write_function_tail(
                        output, ir, parameters, result, variadic, unsafe_, abi, depth,
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

fn write_link_target(output: &mut impl fmt::Write, ir: &Ir, target: LinkTarget) -> fmt::Result {
    match target {
        LinkTarget::Local(entity) => match ir.item(entity) {
            Some(item) => {
                output.write_str(docs_prefix(item.kind()))?;
                write_atom(output, item.name())?;
                output.write_str(".html")
            }
            None => output.write_str("#dangling"),
        },
        LinkTarget::External(external) => match ir.external(external) {
            Some(target) => write_atom(output, ir.atom(target.path).unwrap_or(b"#unresolved")),
            None => output.write_str("#unresolved"),
        },
    }
}

fn write_embedding_target(
    output: &mut impl fmt::Write,
    ir: &Ir,
    target: LinkTarget,
) -> fmt::Result {
    match target {
        LinkTarget::Local(entity) => match ir.item(entity) {
            Some(item) => write_atom(output, item.name()),
            None => output.write_str("?dangling"),
        },
        LinkTarget::External(external) => match ir.external(external) {
            Some(target) => write_atom(output, ir.atom(target.display).unwrap_or(b"?unresolved")),
            None => output.write_str("?unresolved"),
        },
    }
}

const fn link_kind_name(kind: crate::LinkKind) -> &'static str {
    match kind {
        crate::LinkKind::Calls => "calls",
        crate::LinkKind::MethodCall => "method-call",
        crate::LinkKind::TypeReference => "type-reference",
        crate::LinkKind::Reads => "reads",
        crate::LinkKind::Writes => "writes",
        crate::LinkKind::Imports => "imports",
        crate::LinkKind::Implements => "implements",
        crate::LinkKind::Overrides => "overrides",
        crate::LinkKind::Reexports => "reexports",
        crate::LinkKind::Inherits => "inherits",
        crate::LinkKind::Documents => "documents",
    }
}

fn write_atom(output: &mut impl fmt::Write, bytes: &[u8]) -> fmt::Result {
    if let Ok(text) = str::from_utf8(bytes) {
        return output.write_str(text);
    }
    for byte in bytes {
        if byte.is_ascii_graphic() && *byte != b'%' {
            output.write_char(char::from(*byte))?;
        } else {
            write!(output, "%{byte:02X}")?;
        }
    }
    Ok(())
}

const fn docs_prefix(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Record => "struct.",
        ItemKind::Enum => "enum.",
        ItemKind::Trait => "trait.",
        ItemKind::Function => "fn.",
        ItemKind::TypeAlias => "type.",
        ItemKind::Constant => "constant.",
        ItemKind::Static => "static.",
        ItemKind::Macro => "macro.",
        _ => "",
    }
}

const fn builtin_name(builtin: BuiltinType) -> &'static str {
    match builtin {
        BuiltinType::Unit => "()",
        BuiltinType::Never => "!",
        BuiltinType::Bool => "bool",
        BuiltinType::Char => "char",
        BuiltinType::I8 => "i8",
        BuiltinType::I16 => "i16",
        BuiltinType::I32 => "i32",
        BuiltinType::I64 => "i64",
        BuiltinType::I128 => "i128",
        BuiltinType::U8 => "u8",
        BuiltinType::U16 => "u16",
        BuiltinType::U32 => "u32",
        BuiltinType::U64 => "u64",
        BuiltinType::U128 => "u128",
        BuiltinType::F16 => "f16",
        BuiltinType::F32 => "f32",
        BuiltinType::F64 => "f64",
        BuiltinType::String => "str",
        BuiltinType::Bytes => "[u8]",
        BuiltinType::Object => "object",
        BuiltinType::Any => "any",
        BuiltinType::Unknown => "unknown",
        BuiltinType::Void => "void",
        BuiltinType::Number => "number",
        BuiltinType::BigInt => "bigint",
        BuiltinType::Symbol => "symbol",
        BuiltinType::UniqueSymbol => "unique symbol",
        BuiltinType::Null => "null",
        BuiltinType::Undefined => "undefined",
    }
}

const fn unknown_name(reason: UnknownType) -> &'static str {
    match reason {
        UnknownType::Unannotated => "unannotated",
        UnknownType::Unresolved => "unresolved",
        UnknownType::Unsupported => "unsupported",
        UnknownType::Inferred => "inferred",
        UnknownType::Error => "error",
    }
}
