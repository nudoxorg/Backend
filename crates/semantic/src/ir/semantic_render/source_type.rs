//! Source-syntax rendering of semantic types.
//!
//! The canonical renderer in this module's sibling is an injective machine
//! encoding: it spells every name as hex and every type as a tagged tree, and it
//! must never reach a reader. This renderer is the human counterpart. It prints
//! one semantic type the way its source language writes it: Rust `&str` and
//! `Vec<u8>`, Go `*Config` and `map[string]int`, TypeScript
//! `(x: string) => void`, Python `list[int]`, Java `List<String>`, C `char *`.
//!
//! Rendering is **truthful or absent**. A type the image cannot name (an
//! external declaration known only by a stable fragment reference, an unknown
//! type with no source spelling, a construct the language has no syntax for)
//! makes the whole type unrenderable and the caller gets `None`: it shows no
//! type, never a placeholder and never the canonical form. A callable written
//! as a declaration header drops the parts it cannot name instead (a parameter
//! without a known type keeps its name), because a header with fewer facts is
//! still a header.

use core::num::NonZeroUsize;

use crate::ir::{
    AnnotationKind, ArrayShape, AtomId, BuiltinType, ChannelDirection, ComputedType, ConcreteType,
    CxxReferenceCategory, ExternalId, ExternalTarget, ItemName, LanguageProfile, LiteralType,
    Mutability, NativeCharacterRole, ObjectMember, PropertyKey, SemanticReader, TemplatePart,
    TupleElement, TupleElementKind, TupleElementListId, TypeExpr, TypeId, TypeQuery, UnknownReason,
    VariadicForm, WildcardBound,
};

/// Largest rendering, in bytes, this module will produce for one type.
const MAXIMUM_BYTES: usize = 2048;

/// The source language whose syntax a rendering follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Syntax {
    Rust,
    Go,
    Python,
    TypeScript,
    Java,
    CSharp,
    C,
    Cxx,
}

impl Syntax {
    const fn of(profile: LanguageProfile) -> Self {
        match profile {
            LanguageProfile::Rust(_) => Self::Rust,
            LanguageProfile::Go(_) => Self::Go,
            LanguageProfile::Python(_) => Self::Python,
            LanguageProfile::TypeScript(_) => Self::TypeScript,
            LanguageProfile::Java(_) => Self::Java,
            LanguageProfile::CSharp(_) => Self::CSharp,
            LanguageProfile::C(_) => Self::C,
            LanguageProfile::Cxx(_) => Self::Cxx,
        }
    }

    const fn is_c_family(self) -> bool {
        matches!(self, Self::C | Self::Cxx)
    }

    /// Whether the language writes generic arguments in square brackets.
    const fn brackets_arguments(self) -> bool {
        matches!(self, Self::Go | Self::Python)
    }
}

/// Renders one semantic type in the source syntax of `profile`.
///
/// Returns `None` when the type cannot be written truthfully: it names an
/// unresolvable declaration, an unknown type without a source spelling, a
/// construct the language has no spelling for, a dangling reference, or it is
/// deeper than `limits` or longer than the output bound.
#[must_use]
pub fn render_source_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    profile: LanguageProfile,
    root: TypeId,
    limits: super::CanonicalTypeRenderLimits,
) -> Option<String> {
    let mut printer = Printer::new(reader, profile, limits.maximum_depth);
    printer.ty(root, 1)?;
    Some(printer.out)
}

/// Renders a callable's type as the declaration header its language writes:
/// `fn name(a: u32) -> bool`, `func name(a int) bool`, `def name(a: int) ->
/// bool`, `function name(a: number): boolean`, `bool name(int a)`.
///
/// Returns `None` when `root` is not a callable type, or when the language
/// needs a result type the image cannot name.
#[must_use]
pub fn render_source_callable<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    profile: LanguageProfile,
    name: &str,
    root: TypeId,
    limits: super::CanonicalTypeRenderLimits,
) -> Option<String> {
    let mut printer = Printer::new(reader, profile, limits.maximum_depth);
    let Some(TypeExpr::Concrete(ConcreteType::Function {
        parameters,
        results,
        abi,
        variadic,
        unsafe_,
    })) = reader.ty(root)
    else {
        return None;
    };
    printer.header(
        name,
        Callable {
            parameters,
            results,
            abi,
            variadic,
            unsafe_,
        },
        1,
    )?;
    Some(printer.out)
}

/// The parts of one callable type.
#[derive(Clone, Copy)]
struct Callable {
    parameters: TupleElementListId,
    results: TupleElementListId,
    abi: Option<AtomId>,
    variadic: VariadicForm,
    unsafe_: bool,
}

type Written = Option<()>;

struct Printer<'reader, Reader: SemanticReader + ?Sized> {
    reader: &'reader Reader,
    syntax: Syntax,
    maximum_depth: usize,
    out: String,
}

impl<'reader, Reader: SemanticReader + ?Sized> Printer<'reader, Reader> {
    fn new(reader: &'reader Reader, profile: LanguageProfile, depth: NonZeroUsize) -> Self {
        Self {
            reader,
            syntax: Syntax::of(profile),
            maximum_depth: depth.get(),
            out: String::new(),
        }
    }

    fn put(&mut self, text: &str) -> Written {
        self.out.push_str(text);
        (self.out.len() <= MAXIMUM_BYTES).then_some(())
    }

    /// Runs `write`, and removes whatever it wrote when it fails, so a caller
    /// with a fallback never leaves half a type behind.
    fn attempt(&mut self, write: impl FnOnce(&mut Self) -> Written) -> Written {
        let start = self.out.len();
        let written = write(self);
        if written.is_none() {
            self.out.truncate(start);
        }
        written
    }

    fn atom(&self, atom: AtomId) -> Option<&'reader str> {
        let reader = self.reader;
        core::str::from_utf8(reader.atom(atom)?)
            .ok()
            .filter(|text| !text.is_empty())
    }

    fn entity_name(&self, entity: crate::ir::EntityId) -> Option<&'reader str> {
        match self.reader.entity(entity)?.name {
            ItemName::Named(atom) => self.atom(atom),
            ItemName::AnonymousCallable(_) => None,
        }
    }

    fn external_name(&self, external: ExternalId) -> Option<&'reader str> {
        match self.reader.external(external)? {
            ExternalTarget::Stable { .. } => None,
            ExternalTarget::Foreign(target) => {
                self.atom(target.display).or_else(|| self.atom(target.path))
            }
            ExternalTarget::FragmentEntity { display, .. } => self.atom(display),
        }
    }

    fn ty(&mut self, id: TypeId, depth: usize) -> Written {
        if depth > self.maximum_depth {
            return None;
        }
        match self.reader.ty(id)? {
            TypeExpr::Concrete(ty) => self.concrete(ty, depth),
            TypeExpr::Computed(ty) => self.computed(ty, depth),
            TypeExpr::Unknown(unknown) => {
                // A type the model has no representation for keeps the
                // author's own spelling (`error`, `Dict[str, int]`). A
                // reference that did not resolve keeps only the name the
                // producer saw, which may be missing its qualifier
                // (`T` for `testing.T`), so it is not written.
                if matches!(
                    unknown.reason,
                    UnknownReason::UnresolvedExternal | UnknownReason::UnresolvedLocalName
                ) {
                    return None;
                }
                let spelling = self.atom(unknown.spelling?)?;
                // Go spells a type from another package by its bare name
                // (`T` for `testing.T`), even where the model has no
                // representation for it. Only the universe types, which are
                // never qualified, are written.
                if self.syntax == Syntax::Go && !is_go_universe_type(spelling) {
                    return None;
                }
                self.put(spelling)
            }
        }
    }

    fn below(&mut self, id: TypeId, depth: usize) -> Written {
        self.ty(id, depth.saturating_add(1))
    }

    /// Writes a type that is the operand of a postfix or infix constructor,
    /// parenthesizing the shapes that would otherwise bind the wrong way.
    fn operand(&mut self, id: TypeId, depth: usize) -> Written {
        let compound = matches!(
            self.reader.ty(id),
            Some(
                TypeExpr::Concrete(
                    ConcreteType::Function { .. }
                        | ConcreteType::Union(_)
                        | ConcreteType::Intersection(_)
                        | ConcreteType::Optional(_)
                ) | TypeExpr::Computed(ComputedType::Conditional { .. })
            )
        ) && matches!(
            self.syntax,
            Syntax::TypeScript | Syntax::Python | Syntax::CSharp
        );
        if compound {
            self.put("(")?;
        }
        self.below(id, depth)?;
        if compound {
            self.put(")")?;
        }
        Some(())
    }

    fn list(
        &mut self,
        types: crate::ir::TypeListId,
        separator: &str,
        depth: usize,
        operand: bool,
    ) -> Written {
        let reader = self.reader;
        for (index, member) in reader.types(types)?.enumerate() {
            if index != 0 {
                self.put(separator)?;
            }
            if operand {
                self.operand(member, depth)?;
            } else {
                self.below(member, depth)?;
            }
        }
        Some(())
    }

    fn arguments(&mut self, types: crate::ir::TypeListId, depth: usize) -> Written {
        let reader = self.reader;
        if reader.types(types)?.len() == 0 {
            return Some(());
        }
        let (open, close) = if self.syntax.brackets_arguments() {
            ("[", "]")
        } else {
            ("<", ">")
        };
        self.put(open)?;
        self.list(types, ", ", depth, false)?;
        self.put(close)
    }

    fn concrete(&mut self, ty: ConcreteType, depth: usize) -> Written {
        match ty {
            ConcreteType::Builtin(builtin) => {
                let text = builtin_text(self.syntax, builtin)?;
                self.put(text)
            }
            ConcreteType::Literal(literal) => self.literal(literal),
            ConcreteType::Nominal(entity) => {
                let name = self.entity_name(entity)?;
                self.put(name)
            }
            ConcreteType::External(external) => {
                let name = self.external_name(external)?;
                self.put(name)
            }
            ConcreteType::Parameter(atom) => {
                let name = self.atom(atom)?;
                self.put(name)
            }
            ConcreteType::Applied {
                constructor,
                arguments,
            } => {
                self.below(constructor, depth)?;
                self.arguments(arguments, depth)
            }
            ConcreteType::Tuple(elements) => self.tuple(elements, depth),
            ConcreteType::Object(members) => self.object(members, depth),
            ConcreteType::Function {
                parameters,
                results,
                abi,
                variadic,
                unsafe_,
            } => self.function_type(
                Callable {
                    parameters,
                    results,
                    abi,
                    variadic,
                    unsafe_,
                },
                depth,
            ),
            ConcreteType::Reference {
                target,
                mutability,
                lifetime,
            } => {
                if self.syntax == Syntax::Rust {
                    self.put("&")?;
                    if let Some(lifetime) = lifetime.and_then(|atom| self.atom(atom)) {
                        self.put(lifetime)?;
                        self.put(" ")?;
                    }
                    if mutability == Mutability::Mutable {
                        self.put("mut ")?;
                    }
                }
                self.below(target, depth)
            }
            ConcreteType::CxxReference { target, category } => {
                self.below(target, depth)?;
                self.put(match category {
                    CxxReferenceCategory::Lvalue => "&",
                    CxxReferenceCategory::Rvalue => "&&",
                })
            }
            ConcreteType::CPointer { target } => self.c_pointer(target, depth),
            ConcreteType::CxxMemberPointer { owner, member } => {
                self.below(member, depth)?;
                self.put(" ")?;
                self.below(owner, depth)?;
                self.put("::*")
            }
            ConcreteType::CQualified { target, qualifiers } => {
                let postfix = matches!(
                    self.reader.ty(target),
                    Some(TypeExpr::Concrete(ConcreteType::CPointer { .. }))
                );
                let mut words = Vec::with_capacity(3);
                if qualifiers.const_ {
                    words.push("const");
                }
                if qualifiers.volatile {
                    words.push("volatile");
                }
                if qualifiers.restrict {
                    words.push("restrict");
                }
                if postfix {
                    self.below(target, depth)?;
                    self.put(" ")?;
                    self.put(&words.join(" "))
                } else {
                    self.put(&words.join(" "))?;
                    self.put(" ")?;
                    self.below(target, depth)
                }
            }
            ConcreteType::CBlockPointer { target } => self.block_pointer(target, depth),
            ConcreteType::NativeCharacter { role, .. } => {
                self.put(character_text(self.syntax, role))
            }
            ConcreteType::Pointer { target, mutability } => match self.syntax {
                Syntax::Rust => {
                    self.put(if mutability == Mutability::Mutable {
                        "*mut "
                    } else {
                        "*const "
                    })?;
                    self.below(target, depth)
                }
                Syntax::Go => {
                    self.put("*")?;
                    self.below(target, depth)
                }
                _ => {
                    self.below(target, depth)?;
                    self.put("*")
                }
            },
            ConcreteType::Slice(element) => match self.syntax {
                Syntax::Rust => {
                    self.put("[")?;
                    self.below(element, depth)?;
                    self.put("]")
                }
                Syntax::Go => {
                    self.put("[]")?;
                    self.below(element, depth)
                }
                Syntax::Python => {
                    self.put("Sequence[")?;
                    self.below(element, depth)?;
                    self.put("]")
                }
                _ => {
                    self.operand(element, depth)?;
                    self.put("[]")
                }
            },
            ConcreteType::Array { element, shape } => self.array(element, shape, depth),
            ConcreteType::Optional(target) => match self.syntax {
                Syntax::Rust => {
                    self.put("Option<")?;
                    self.below(target, depth)?;
                    self.put(">")
                }
                Syntax::TypeScript => {
                    self.operand(target, depth)?;
                    self.put(" | undefined")
                }
                Syntax::Python => {
                    self.operand(target, depth)?;
                    self.put(" | None")
                }
                Syntax::CSharp => {
                    self.operand(target, depth)?;
                    self.put("?")
                }
                _ => self.below(target, depth),
            },
            ConcreteType::Union(types) => {
                if self.syntax == Syntax::Python
                    || self.syntax == Syntax::TypeScript
                    || self.syntax == Syntax::Java
                {
                    self.list(types, " | ", depth, true)
                } else {
                    None
                }
            }
            ConcreteType::Intersection(types) => {
                if matches!(self.syntax, Syntax::TypeScript | Syntax::Java) {
                    self.list(types, " & ", depth, true)
                } else {
                    None
                }
            }
            ConcreteType::ImplTrait(types) => {
                if self.syntax != Syntax::Rust {
                    return None;
                }
                self.put("impl ")?;
                self.list(types, " + ", depth, false)
            }
            ConcreteType::DynTrait(types) => {
                if self.syntax != Syntax::Rust {
                    return None;
                }
                self.put("dyn ")?;
                self.list(types, " + ", depth, false)
            }
            ConcreteType::Wildcard(bound) => match bound {
                WildcardBound::Unbounded => self.put("?"),
                WildcardBound::Extends(target) => {
                    self.put("? extends ")?;
                    self.below(target, depth)
                }
                WildcardBound::Super(target) => {
                    self.put("? super ")?;
                    self.below(target, depth)
                }
            },
            ConcreteType::Annotated { kind, target } => match kind {
                AnnotationKind::Readonly => {
                    self.put("readonly ")?;
                    self.operand(target, depth)
                }
                AnnotationKind::NullableValue | AnnotationKind::NullableReference => {
                    self.operand(target, depth)?;
                    self.put("?")
                }
                AnnotationKind::NonNullableReference => self.below(target, depth),
            },
            ConcreteType::Inferred(spelling) => {
                if let Some(text) = spelling.and_then(|atom| self.atom(atom)) {
                    return self.put(text);
                }
                match self.syntax {
                    Syntax::Rust => self.put("_"),
                    Syntax::Cxx => self.put("auto"),
                    Syntax::Java | Syntax::CSharp => self.put("var"),
                    _ => None,
                }
            }
            ConcreteType::QualifiedPath { spelling, .. } => {
                let text = self.atom(spelling)?;
                self.put(text)
            }
            ConcreteType::Map { key, value } => {
                if self.syntax == Syntax::Go {
                    self.put("map[")?;
                    self.below(key, depth)?;
                    self.put("]")?;
                } else {
                    self.put("Map<")?;
                    self.below(key, depth)?;
                    self.put(", ")?;
                }
                self.below(value, depth)?;
                if self.syntax == Syntax::Go {
                    Some(())
                } else {
                    self.put(">")
                }
            }
            ConcreteType::Channel { direction, element } => {
                self.put(match direction {
                    ChannelDirection::Both => "chan ",
                    ChannelDirection::Send => "chan<- ",
                    ChannelDirection::Receive => "<-chan ",
                })?;
                self.below(element, depth)
            }
        }
    }

    fn literal(&mut self, literal: LiteralType) -> Written {
        match literal {
            LiteralType::String(atom) => {
                let text = self.atom(atom).unwrap_or("");
                self.put("\"")?;
                for character in text.chars() {
                    match character {
                        '"' => self.put("\\\"")?,
                        '\\' => self.put("\\\\")?,
                        '\n' => self.put("\\n")?,
                        other => self.put(other.encode_utf8(&mut [0; 4]))?,
                    }
                }
                self.put("\"")
            }
            LiteralType::Number(atom) => {
                let text = self.atom(atom)?;
                self.put(text)
            }
            LiteralType::BigInt(atom) => {
                let text = self.atom(atom)?;
                self.put(text)?;
                if self.syntax == Syntax::TypeScript {
                    self.put("n")?;
                }
                Some(())
            }
            LiteralType::Boolean(value) => {
                let text = match (self.syntax, value) {
                    (Syntax::Python, true) => "True",
                    (Syntax::Python, false) => "False",
                    (_, true) => "true",
                    (_, false) => "false",
                };
                self.put(text)
            }
            LiteralType::Null => self.put(if self.syntax == Syntax::Python {
                "None"
            } else {
                "null"
            }),
            LiteralType::Undefined => self.put("undefined"),
        }
    }

    fn tuple(&mut self, list: TupleElementListId, depth: usize) -> Written {
        let reader = self.reader;
        let elements = reader.tuple_elements(list)?;
        let ts = self.syntax == Syntax::TypeScript;
        match self.syntax {
            Syntax::Python => self.put("tuple[")?,
            _ if ts => self.put("[")?,
            _ => self.put("(")?,
        }
        let count = elements.len();
        for (index, element) in elements.enumerate() {
            if index != 0 {
                self.put(", ")?;
            }
            if ts {
                if element.kind == TupleElementKind::Rest {
                    self.put("...")?;
                }
                if let Some(label) = element.label.and_then(|atom| self.atom(atom)) {
                    self.put(label)?;
                    self.put(if element.kind == TupleElementKind::Optional {
                        "?: "
                    } else {
                        ": "
                    })?;
                }
            }
            self.operand(element.ty, depth)?;
            if ts && element.label.is_none() && element.kind == TupleElementKind::Optional {
                self.put("?")?;
            }
        }
        match self.syntax {
            Syntax::Python => self.put("]"),
            _ if ts => self.put("]"),
            Syntax::Rust if count == 1 => self.put(",)"),
            _ => self.put(")"),
        }
    }

    fn object(&mut self, list: crate::ir::ObjectMemberListId, depth: usize) -> Written {
        if self.syntax != Syntax::TypeScript {
            return None;
        }
        let reader = self.reader;
        let members = reader.object_members(list)?;
        if members.len() == 0 {
            return self.put("{}");
        }
        self.put("{ ")?;
        for (index, member) in members.enumerate() {
            if index != 0 {
                self.put("; ")?;
            }
            self.object_member(member, depth)?;
        }
        self.put(" }")
    }

    fn property_key(&mut self, key: PropertyKey, depth: usize) -> Written {
        match key {
            PropertyKey::Named(atom) | PropertyKey::Numeric(atom) => {
                let text = self.atom(atom)?;
                self.put(text)
            }
            PropertyKey::Private(atom) => {
                let text = self.atom(atom)?;
                self.put("#")?;
                self.put(text)
            }
            PropertyKey::Computed(ty) => {
                self.put("[")?;
                self.below(ty, depth)?;
                self.put("]")
            }
        }
    }

    fn object_member(&mut self, member: ObjectMember, depth: usize) -> Written {
        match member {
            ObjectMember::Property {
                key,
                ty,
                optional,
                readonly,
            } => {
                if readonly {
                    self.put("readonly ")?;
                }
                self.property_key(key, depth)?;
                self.put(if optional { "?: " } else { ": " })?;
                self.below(ty, depth)
            }
            ObjectMember::Method {
                key,
                signature,
                optional,
            } => {
                self.property_key(key, depth)?;
                if optional {
                    self.put("?")?;
                }
                self.method_signature(signature, depth)
            }
            ObjectMember::Index {
                parameter,
                key,
                value,
                readonly,
            } => {
                if readonly {
                    self.put("readonly ")?;
                }
                let name = self.atom(parameter)?;
                self.put("[")?;
                self.put(name)?;
                self.put(": ")?;
                self.below(key, depth)?;
                self.put("]: ")?;
                self.below(value, depth)
            }
            ObjectMember::Call(signature) => self.method_signature(signature, depth),
            ObjectMember::Construct(signature) => {
                self.put("new ")?;
                self.method_signature(signature, depth)
            }
        }
    }

    /// `(a: A): R` — a TypeScript signature as a member writes it.
    fn method_signature(&mut self, signature: TypeId, depth: usize) -> Written {
        let Some(TypeExpr::Concrete(ConcreteType::Function {
            parameters,
            results,
            variadic,
            ..
        })) = self.reader.ty(signature)
        else {
            return None;
        };
        self.parameters(parameters, variadic, depth)?;
        self.put(": ")?;
        self.result_or(results, "void", depth)
    }

    fn array(&mut self, element: TypeId, shape: ArrayShape, depth: usize) -> Written {
        match (self.syntax, shape) {
            (Syntax::Rust, ArrayShape::FixedValue { length }) => {
                self.put("[")?;
                self.below(element, depth)?;
                self.put(&format!("; {length}]"))
            }
            (Syntax::Rust, ArrayShape::ConstExpression(atom)) => {
                let expression = self.atom(atom)?;
                self.put("[")?;
                self.below(element, depth)?;
                self.put(&format!("; {expression}]"))
            }
            (Syntax::Rust, _) => {
                self.put("[")?;
                self.below(element, depth)?;
                self.put("]")
            }
            (Syntax::Go, ArrayShape::FixedValue { length }) => {
                self.put(&format!("[{length}]"))?;
                self.below(element, depth)
            }
            (Syntax::Go, ArrayShape::ConstExpression(atom)) => {
                let expression = self.atom(atom)?;
                self.put(&format!("[{expression}]"))?;
                self.below(element, depth)
            }
            (Syntax::Go, _) => {
                self.put("[]")?;
                self.below(element, depth)
            }
            (Syntax::CSharp, ArrayShape::Rectangular { rank }) => {
                self.operand(element, depth)?;
                let commas = ",".repeat(usize::from(rank.get()).saturating_sub(1));
                self.put(&format!("[{commas}]"))
            }
            (Syntax::Java, ArrayShape::Rectangular { rank }) => {
                self.operand(element, depth)?;
                self.put(&"[]".repeat(usize::from(rank.get())))
            }
            (syntax, ArrayShape::FixedValue { length }) if syntax.is_c_family() => {
                self.below(element, depth)?;
                self.put(&format!("[{length}]"))
            }
            (syntax, ArrayShape::ConstExpression(atom)) if syntax.is_c_family() => {
                let expression = self.atom(atom)?;
                self.below(element, depth)?;
                self.put(&format!("[{expression}]"))
            }
            (Syntax::Python, _) => {
                self.put("list[")?;
                self.below(element, depth)?;
                self.put("]")
            }
            _ => {
                self.operand(element, depth)?;
                self.put("[]")
            }
        }
    }

    fn c_pointer(&mut self, target: TypeId, depth: usize) -> Written {
        if let Some(TypeExpr::Concrete(ConcreteType::Function {
            parameters,
            results,
            variadic,
            ..
        })) = self.reader.ty(target)
        {
            return self.c_function_pointer("*", parameters, results, variadic, depth);
        }
        self.below(target, depth)?;
        self.put(" *")
    }

    fn block_pointer(&mut self, target: TypeId, depth: usize) -> Written {
        let Some(TypeExpr::Concrete(ConcreteType::Function {
            parameters,
            results,
            variadic,
            ..
        })) = self.reader.ty(target)
        else {
            return None;
        };
        self.c_function_pointer("^", parameters, results, variadic, depth)
    }

    /// `R (*)(A, B)` — a C function pointer, or a block pointer with `^`.
    fn c_function_pointer(
        &mut self,
        sigil: &str,
        parameters: TupleElementListId,
        results: TupleElementListId,
        variadic: VariadicForm,
        depth: usize,
    ) -> Written {
        self.result_or(results, "void", depth)?;
        self.put(" (")?;
        self.put(sigil)?;
        self.put(")")?;
        self.parameters(parameters, variadic, depth)
    }

    fn computed(&mut self, ty: ComputedType, depth: usize) -> Written {
        if self.syntax != Syntax::TypeScript {
            return None;
        }
        match ty {
            ComputedType::KeyOf(target) => {
                self.put("keyof ")?;
                self.operand(target, depth)
            }
            ComputedType::TypeOf(query) => {
                self.put("typeof ")?;
                match query {
                    TypeQuery::Entity(entity) => {
                        let name = self.entity_name(entity)?;
                        self.put(name)
                    }
                    TypeQuery::Path(path) => {
                        let reader = self.reader;
                        for (index, atom) in reader.atom_list(path)?.enumerate() {
                            if index != 0 {
                                self.put(".")?;
                            }
                            let segment = self.atom(atom)?;
                            self.put(segment)?;
                        }
                        Some(())
                    }
                    TypeQuery::External(external) => {
                        let name = self.external_name(external)?;
                        self.put(name)
                    }
                }
            }
            ComputedType::IndexedAccess { object, index } => {
                self.operand(object, depth)?;
                self.put("[")?;
                self.below(index, depth)?;
                self.put("]")
            }
            ComputedType::Conditional {
                check,
                extends,
                then_type,
                else_type,
                ..
            } => {
                self.operand(check, depth)?;
                self.put(" extends ")?;
                self.operand(extends, depth)?;
                self.put(" ? ")?;
                self.below(then_type, depth)?;
                self.put(" : ")?;
                self.below(else_type, depth)
            }
            ComputedType::Mapped {
                parameter,
                constraint,
                name_as,
                value,
                ..
            } => {
                let name = self.atom(parameter)?;
                self.put("{ [")?;
                self.put(name)?;
                self.put(" in ")?;
                self.below(constraint, depth)?;
                if let Some(name_as) = name_as {
                    self.put(" as ")?;
                    self.below(name_as, depth)?;
                }
                self.put("]: ")?;
                self.below(value, depth)?;
                self.put(" }")
            }
            ComputedType::Infer { parameter, .. } => {
                let name = self.atom(parameter)?;
                self.put("infer ")?;
                self.put(name)
            }
            ComputedType::TemplateLiteral(parts) => {
                let reader = self.reader;
                self.put("`")?;
                for part in reader.template_parts(parts)? {
                    match part {
                        TemplatePart::Bytes(atom) => {
                            let text = self.atom(atom).unwrap_or("");
                            self.put(text)?;
                        }
                        TemplatePart::Placeholder(ty) => {
                            self.put("${")?;
                            self.below(ty, depth)?;
                            self.put("}")?;
                        }
                    }
                }
                self.put("`")
            }
            ComputedType::Import {
                specifier,
                qualifier,
                arguments,
            } => {
                let reader = self.reader;
                let specifier = self.atom(specifier)?;
                self.put("import(\"")?;
                self.put(specifier)?;
                self.put("\")")?;
                for atom in reader.atom_list(qualifier)? {
                    let segment = self.atom(atom)?;
                    self.put(".")?;
                    self.put(segment)?;
                }
                self.arguments(arguments, depth)
            }
            ComputedType::Awaited(target) => {
                self.put("Awaited<")?;
                self.below(target, depth)?;
                self.put(">")
            }
            ComputedType::This => self.put("this"),
        }
    }

    // ---- callables --------------------------------------------------------

    /// One parameter list, `(a: A, b?: B, ...rest: C[])` in the current
    /// language's spelling. A parameter whose type cannot be written keeps its
    /// name; an unnamed one keeps its type.
    fn parameters(
        &mut self,
        list: TupleElementListId,
        variadic: VariadicForm,
        depth: usize,
    ) -> Written {
        let reader = self.reader;
        let elements = reader.tuple_elements(list)?.collect::<Vec<_>>();
        let named = elements.iter().any(|element| element.label.is_some());
        self.put("(")?;
        let last = elements.len().saturating_sub(1);
        for (index, element) in elements.iter().enumerate() {
            if index != 0 {
                self.put(", ")?;
            }
            let rest = element.kind == TupleElementKind::Rest
                || (variadic == VariadicForm::TypedLast && index == last);
            self.parameter(*element, rest, named, depth)?;
        }
        if variadic == VariadicForm::CUnbounded {
            if !elements.is_empty() {
                self.put(", ")?;
            }
            self.put("...")?;
        }
        self.put(")")
    }

    fn parameter(
        &mut self,
        element: TupleElement,
        rest: bool,
        named: bool,
        depth: usize,
    ) -> Written {
        let label = element.label.and_then(|atom| self.atom(atom));
        let optional = element.kind == TupleElementKind::Optional;
        match self.syntax {
            Syntax::Rust => {
                if label == Some("self") {
                    return self.rust_receiver(element.ty);
                }
                match label {
                    Some(label) => {
                        self.put(label)?;
                        self.typed_suffix(": ", element.ty, depth)
                    }
                    None => self.below(element.ty, depth),
                }
            }
            Syntax::Go => {
                let label = label.or(named.then_some("_"));
                if let Some(label) = label {
                    self.put(label)?;
                    self.put(" ")?;
                }
                if rest {
                    self.put("...")?;
                    if let Some(TypeExpr::Concrete(ConcreteType::Slice(inner))) =
                        self.reader.ty(element.ty)
                    {
                        return self.below(inner, depth);
                    }
                }
                self.below(element.ty, depth)
            }
            Syntax::TypeScript => {
                if rest {
                    self.put("...")?;
                }
                self.put(label.unwrap_or("_"))?;
                if optional {
                    self.put("?")?;
                }
                self.typed_suffix(": ", element.ty, depth)
            }
            Syntax::Python => {
                if rest {
                    self.put("*")?;
                }
                self.put(label.unwrap_or("_"))?;
                self.typed_suffix(": ", element.ty, depth)?;
                if optional {
                    self.put(" = ...")?;
                }
                Some(())
            }
            Syntax::Java | Syntax::CSharp | Syntax::C | Syntax::Cxx => {
                if rest && matches!(self.syntax, Syntax::Java) {
                    if let Some(TypeExpr::Concrete(ConcreteType::Array {
                        element: inner, ..
                    })) = self.reader.ty(element.ty)
                    {
                        self.below(inner, depth)?;
                        self.put("...")?;
                        return label.map_or(Some(()), |label| {
                            self.put(" ")?;
                            self.put(label)
                        });
                    }
                }
                self.below(element.ty, depth)?;
                if let Some(label) = label {
                    self.put(" ")?;
                    self.put(label)?;
                }
                Some(())
            }
        }
    }

    /// Writes `separator` and the type after a parameter name when the type can
    /// be written, and nothing otherwise: a parameter whose type is unknown
    /// keeps its name.
    fn typed_suffix(&mut self, separator: &str, ty: TypeId, depth: usize) -> Written {
        let _ = self.attempt(|printer| {
            printer.put(separator)?;
            printer.below(ty, depth)
        });
        Some(())
    }

    /// Rust's receiver, written the way a method writes it: `&self`,
    /// `&mut self`, `self`; absent when its type is unknown.
    fn rust_receiver(&mut self, ty: TypeId) -> Written {
        match self.reader.ty(ty) {
            Some(TypeExpr::Concrete(ConcreteType::Reference {
                mutability,
                lifetime,
                ..
            })) => {
                self.put("&")?;
                if let Some(lifetime) = lifetime.and_then(|atom| self.atom(atom)) {
                    self.put(lifetime)?;
                    self.put(" ")?;
                }
                if mutability == Mutability::Mutable {
                    self.put("mut ")?;
                }
                self.put("self")
            }
            // A receiver taken by value. An unknown type could be either, so
            // the header is not written rather than guessed.
            Some(TypeExpr::Concrete(_)) => self.put("self"),
            _ => None,
        }
    }

    /// The result lane written as a type: one result is that type, several are
    /// a tuple in the language's spelling, none is `empty`.
    fn result_or(&mut self, results: TupleElementListId, empty: &str, depth: usize) -> Written {
        let reader = self.reader;
        let elements = reader.tuple_elements(results)?.collect::<Vec<_>>();
        match elements.as_slice() {
            [] => self.put(empty),
            [only] => self.below(only.ty, depth),
            many => {
                self.put(if self.syntax == Syntax::Python {
                    "tuple["
                } else {
                    "("
                })?;
                for (index, element) in many.iter().enumerate() {
                    if index != 0 {
                        self.put(", ")?;
                    }
                    self.below(element.ty, depth)?;
                }
                self.put(if self.syntax == Syntax::Python {
                    "]"
                } else {
                    ")"
                })
            }
        }
    }

    /// Go's result lane: `T`, or `(a T, b error)`.
    fn go_results(&mut self, results: TupleElementListId, depth: usize) -> Written {
        let reader = self.reader;
        let elements = reader.tuple_elements(results)?.collect::<Vec<_>>();
        match elements.as_slice() {
            [] => Some(()),
            [only] if only.label.is_none() => {
                self.put(" ")?;
                self.below(only.ty, depth)
            }
            many => {
                self.put(" (")?;
                for (index, element) in many.iter().enumerate() {
                    if index != 0 {
                        self.put(", ")?;
                    }
                    if let Some(label) = element.label.and_then(|atom| self.atom(atom)) {
                        self.put(label)?;
                        self.put(" ")?;
                    }
                    self.below(element.ty, depth)?;
                }
                self.put(")")
            }
        }
    }

    /// A callable as a type: `fn(u8) -> bool`, `func(int) bool`,
    /// `(x: number) => void`, `Callable[[int], str]`.
    fn function_type(&mut self, callable: Callable, depth: usize) -> Written {
        let Callable {
            parameters,
            results,
            abi,
            variadic,
            unsafe_,
        } = callable;
        match self.syntax {
            Syntax::Rust => {
                if unsafe_ {
                    self.put("unsafe ")?;
                }
                if let Some(abi) = abi.and_then(|atom| self.atom(atom)) {
                    self.put(&format!("extern \"{abi}\" "))?;
                }
                self.put("fn")?;
                self.parameters(parameters, variadic, depth)?;
                self.rust_result(results, depth)
            }
            Syntax::Go => {
                self.put("func")?;
                self.parameters(parameters, variadic, depth)?;
                self.go_results(results, depth)
            }
            Syntax::TypeScript => {
                self.parameters(parameters, variadic, depth)?;
                self.put(" => ")?;
                self.result_or(results, "void", depth)
            }
            Syntax::Python => {
                let reader = self.reader;
                self.put("Callable[")?;
                let listed = self.attempt(|printer| {
                    printer.put("[")?;
                    for (index, element) in reader.tuple_elements(parameters)?.enumerate() {
                        if index != 0 {
                            printer.put(", ")?;
                        }
                        printer.below(element.ty, depth)?;
                    }
                    printer.put("]")
                });
                if listed.is_none() {
                    self.put("...")?;
                }
                self.put(", ")?;
                let written = self.attempt(|printer| printer.result_or(results, "None", depth));
                if written.is_none() {
                    self.put("Any")?;
                }
                self.put("]")
            }
            Syntax::Java | Syntax::CSharp => {
                self.parameters(parameters, variadic, depth)?;
                self.put(" -> ")?;
                self.result_or(results, "void", depth)
            }
            Syntax::C | Syntax::Cxx => {
                self.result_or(results, "void", depth)?;
                self.put(" ")?;
                self.parameters(parameters, variadic, depth)
            }
        }
    }

    /// Rust's ` -> R`, absent for a callable that returns nothing or unit.
    fn rust_result(&mut self, results: TupleElementListId, depth: usize) -> Written {
        let reader = self.reader;
        let elements = reader.tuple_elements(results)?.collect::<Vec<_>>();
        let unit = |element: &TupleElement| {
            matches!(
                reader.ty(element.ty),
                Some(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::Unit)))
            )
        };
        match elements.as_slice() {
            [] => Some(()),
            [only] if unit(only) => Some(()),
            _ => {
                self.put(" -> ")?;
                self.result_or(results, "()", depth)
            }
        }
    }

    /// A callable as the declaration header its language writes, `name`
    /// included.
    fn header(&mut self, name: &str, callable: Callable, depth: usize) -> Written {
        let Callable {
            parameters,
            results,
            abi,
            variadic,
            unsafe_,
        } = callable;
        match self.syntax {
            Syntax::Rust => {
                if unsafe_ {
                    self.put("unsafe ")?;
                }
                if let Some(abi) = abi.and_then(|atom| self.atom(atom)) {
                    self.put(&format!("extern \"{abi}\" "))?;
                }
                self.put("fn ")?;
                self.put(name)?;
                self.parameters(parameters, variadic, depth)?;
                self.rust_result(results, depth)
            }
            Syntax::Go => {
                self.put("func ")?;
                self.put(name)?;
                self.parameters(parameters, variadic, depth)?;
                self.go_results(results, depth)
            }
            Syntax::TypeScript => {
                self.put("function ")?;
                self.put(name)?;
                self.parameters(parameters, variadic, depth)?;
                let reader = self.reader;
                if reader.tuple_elements(results)?.len() != 0 {
                    let _ = self.attempt(|printer| {
                        printer.put(": ")?;
                        printer.result_or(results, "void", depth)
                    });
                }
                Some(())
            }
            Syntax::Python => {
                self.put("def ")?;
                self.put(name)?;
                self.parameters(parameters, variadic, depth)?;
                let _ = self.attempt(|printer| {
                    printer.put(" -> ")?;
                    printer.result_or(results, "None", depth)
                });
                Some(())
            }
            Syntax::Java | Syntax::CSharp | Syntax::C | Syntax::Cxx => {
                self.result_or(results, "void", depth)?;
                self.put(" ")?;
                self.put(name)?;
                self.parameters(parameters, variadic, depth)
            }
        }
    }
}

/// Whether a Go type spelling names a universe type, which no package
/// qualifies.
fn is_go_universe_type(spelling: &str) -> bool {
    matches!(spelling, "error" | "comparable" | "any" | "unsafe.Pointer")
        || spelling.starts_with("untyped ")
}

const fn character_text(syntax: Syntax, role: NativeCharacterRole) -> &'static str {
    match role {
        NativeCharacterRole::UnicodeScalar => "char",
        NativeCharacterRole::Utf16CodeUnit => match syntax {
            Syntax::Cxx | Syntax::C => "char16_t",
            _ => "char",
        },
        NativeCharacterRole::Utf32CodeUnit => "char32_t",
        NativeCharacterRole::CPlainSigned | NativeCharacterRole::CPlainUnsigned => "char",
        NativeCharacterRole::CSigned => "signed char",
        NativeCharacterRole::CUnsigned => "unsigned char",
        NativeCharacterRole::CWideSigned
        | NativeCharacterRole::CWideUnsigned
        | NativeCharacterRole::CWideSignednessUnavailable => "wchar_t",
    }
}

/// The spelling of one builtin in `syntax`, or `None` when the language has no
/// such type.
const fn builtin_text(syntax: Syntax, builtin: BuiltinType) -> Option<&'static str> {
    use BuiltinType as B;
    Some(match syntax {
        Syntax::Rust => match builtin {
            B::Unit | B::Void => "()",
            B::Never => "!",
            B::Bool => "bool",
            B::LegacyChar => "char",
            B::I8 => "i8",
            B::I16 => "i16",
            B::I32 => "i32",
            B::I64 => "i64",
            B::I128 => "i128",
            B::U8 => "u8",
            B::U16 => "u16",
            B::U32 => "u32",
            B::U64 => "u64",
            B::U128 => "u128",
            B::F16 => "f16",
            B::F32 => "f32",
            B::F64 => "f64",
            B::String => "str",
            B::Bytes => "[u8]",
            B::NativeSignedInteger => "isize",
            B::NativeUnsignedInteger | B::PointerAddressInteger => "usize",
            _ => return None,
        },
        Syntax::Go => match builtin {
            B::Unit => "struct{}",
            B::Bool => "bool",
            B::LegacyChar => "rune",
            B::I8 => "int8",
            B::I16 => "int16",
            B::I32 => "int32",
            B::I64 => "int64",
            B::U8 => "uint8",
            B::U16 => "uint16",
            B::U32 => "uint32",
            B::U64 => "uint64",
            B::F32 => "float32",
            B::F64 => "float64",
            B::Complex => "complex128",
            B::String => "string",
            B::Bytes => "[]byte",
            B::Any | B::Object | B::Unknown => "any",
            B::NativeSignedInteger | B::ArbitraryInteger => "int",
            B::NativeUnsignedInteger => "uint",
            B::PointerAddressInteger => "uintptr",
            _ => return None,
        },
        Syntax::Python => match builtin {
            B::Unit | B::Void | B::None_ | B::Null => "None",
            B::Never => "NoReturn",
            B::Bool => "bool",
            B::I8
            | B::I16
            | B::I32
            | B::I64
            | B::I128
            | B::U8
            | B::U16
            | B::U32
            | B::U64
            | B::U128
            | B::ArbitraryInteger
            | B::NativeSignedInteger
            | B::NativeUnsignedInteger
            | B::PointerAddressInteger => "int",
            B::F16 | B::F32 | B::F64 => "float",
            B::Complex => "complex",
            B::String => "str",
            B::Bytes => "bytes",
            B::Object | B::Unknown => "object",
            B::Any => "Any",
            B::List => "list",
            B::Dict => "dict",
            B::Set => "set",
            B::FrozenSet => "frozenset",
            B::Decimal => "Decimal",
            _ => return None,
        },
        Syntax::TypeScript => match builtin {
            B::Unit | B::Void => "void",
            B::Never => "never",
            B::Bool => "boolean",
            B::I8
            | B::I16
            | B::I32
            | B::I64
            | B::U8
            | B::U16
            | B::U32
            | B::U64
            | B::F16
            | B::F32
            | B::F64
            | B::Number => "number",
            B::BigInt | B::I128 | B::U128 | B::ArbitraryInteger => "bigint",
            B::String => "string",
            B::Symbol => "symbol",
            B::UniqueSymbol => "unique symbol",
            B::Null => "null",
            B::Undefined => "undefined",
            B::Object => "object",
            B::Any => "any",
            B::Unknown => "unknown",
            _ => return None,
        },
        Syntax::Java => match builtin {
            B::Unit | B::Void => "void",
            B::Bool => "boolean",
            B::LegacyChar | B::U16 => "char",
            B::I8 => "byte",
            B::I16 => "short",
            B::I32 => "int",
            B::I64 => "long",
            B::F32 => "float",
            B::F64 => "double",
            B::String => "String",
            B::Object => "Object",
            B::Null => "null",
            _ => return None,
        },
        Syntax::CSharp => match builtin {
            B::Unit | B::Void => "void",
            B::Bool => "bool",
            B::LegacyChar => "char",
            B::I8 => "sbyte",
            B::U8 => "byte",
            B::I16 => "short",
            B::U16 => "ushort",
            B::I32 => "int",
            B::U32 => "uint",
            B::I64 => "long",
            B::U64 => "ulong",
            B::F32 => "float",
            B::F64 => "double",
            B::Decimal => "decimal",
            B::String => "string",
            B::Object => "object",
            B::NativeSignedInteger => "nint",
            B::NativeUnsignedInteger | B::PointerAddressInteger => "nuint",
            B::Null => "null",
            _ => return None,
        },
        Syntax::C | Syntax::Cxx => match builtin {
            B::Unit | B::Void => "void",
            B::Bool => "bool",
            B::I8 => "int8_t",
            B::U8 => "uint8_t",
            B::I16 => "short",
            B::U16 => "unsigned short",
            B::I32 => "int",
            B::U32 => "unsigned int",
            B::I64 => "long",
            B::U64 => "unsigned long",
            B::I128 => "__int128",
            B::U128 => "unsigned __int128",
            B::F32 => "float",
            B::F64 => "double",
            B::NativeSignedInteger => "intptr_t",
            B::NativeUnsignedInteger => "size_t",
            B::PointerAddressInteger => "uintptr_t",
            _ => return None,
        },
    })
}
