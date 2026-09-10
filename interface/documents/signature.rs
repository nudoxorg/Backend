//! Defines signature behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the signature invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Building a declaration signature that reads like its source language, token by hyperlinked token.
//!
//! The image retains no rendered signature: `prepare_profile` errors for every profile and the
//! canonical renderer emits a structural spelling with hex-encoded atoms. So the signature is built
//! here, structurally, by walking the semantic type and spelling it through a [`SignatureDialect`].
//! A node the walker cannot spell falls back to the canonical text as one untargeted token — never
//! to a guess, and never to a target the image did not prove.

use compiler_ir::{BuiltinType, ConcreteType, LiteralType, Mutability, TypeExpr, TypeId};
use compiler_ir_vocabulary::EntityKind;
use compiler_vocabulary::Language;

use crate::{Count, Target, Text, Token, TokenKind};

/// How one language spells a declaration.
///
/// Two languages that share a shape still spell it differently, and a reader copying a signature
/// out of a page expects their own language's punctuation. The dialect is the only place those
/// spellings live, so the CLI, the Markdown, and the GUI cannot disagree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureDialect(Language);

impl SignatureDialect {
    /// The dialect for one proven source language.
    #[must_use]
    pub const fn of(language: Language) -> Self {
        Self(language)
    }

    /// The language this dialect spells.
    #[must_use]
    pub const fn language(self) -> Language {
        self.0
    }

    /// Whether the return type leads the declaration, as in Java, C#, C, and C++.
    #[must_use]
    pub(crate) const fn result_leads(self) -> bool {
        matches!(self.0, Language::Java | Language::CSharp | Language::Clang)
    }

    /// The keyword that introduces a declaration of one kind, when the language has one.
    #[must_use]
    pub(crate) const fn keyword(self, kind: EntityKind) -> Option<&'static str> {
        match kind {
            EntityKind::Function => match self.0 {
                Language::Rust => Some("fn"),
                Language::Python => Some("def"),
                Language::TypeScript => Some("function"),
                Language::Go => Some("func"),
                Language::Java | Language::CSharp | Language::Clang => None,
            },
            EntityKind::Record => match self.0 {
                Language::Rust | Language::Clang => Some("struct"),
                Language::Python | Language::TypeScript | Language::Java | Language::CSharp => {
                    Some("class")
                }
                Language::Go => Some("type"),
            },
            EntityKind::Enum => match self.0 {
                Language::Python => Some("class"),
                Language::Go => Some("type"),
                Language::Rust
                | Language::TypeScript
                | Language::Java
                | Language::CSharp
                | Language::Clang => Some("enum"),
            },
            EntityKind::Trait => match self.0 {
                Language::Rust => Some("trait"),
                Language::TypeScript | Language::Java | Language::CSharp => Some("interface"),
                Language::Go => Some("type"),
                Language::Python | Language::Clang => None,
            },
            EntityKind::Alias => match self.0 {
                Language::Rust | Language::TypeScript | Language::Go => Some("type"),
                Language::Clang => Some("typedef"),
                Language::Python | Language::Java | Language::CSharp => None,
            },
            EntityKind::Module | EntityKind::Namespace => match self.0 {
                Language::Rust => Some("mod"),
                Language::TypeScript | Language::CSharp | Language::Clang => Some("namespace"),
                Language::Go | Language::Java => Some("package"),
                Language::Python => Some("module"),
            },
            EntityKind::Constant => match self.0 {
                Language::Rust | Language::TypeScript | Language::Go => Some("const"),
                Language::Python | Language::Java | Language::CSharp | Language::Clang => None,
            },
            EntityKind::Static => match self.0 {
                Language::Rust => Some("static"),
                Language::Go => Some("var"),
                _ => None,
            },
            EntityKind::Implementation => match self.0 {
                Language::Rust => Some("impl"),
                _ => None,
            },
            EntityKind::Reexport => match self.0 {
                Language::Rust => Some("use"),
                Language::TypeScript => Some("export"),
                Language::Python | Language::Go => Some("import"),
                _ => None,
            },
            EntityKind::Macro => match self.0 {
                Language::Rust => Some("macro_rules!"),
                Language::Clang => Some("#define"),
                _ => None,
            },
            EntityKind::Field | EntityKind::Variant | EntityKind::Parameter => None,
        }
    }

    /// The public-visibility prefix a reader expects in front of a declaration.
    #[must_use]
    pub(crate) const fn public_prefix(self) -> Option<&'static str> {
        match self.0 {
            Language::Rust => Some("pub"),
            Language::TypeScript => Some("export"),
            Language::Java | Language::CSharp => Some("public"),
            Language::Python | Language::Go | Language::Clang => None,
        }
    }

    /// The bracket pair the language uses around type arguments.
    #[must_use]
    pub(crate) const fn type_argument_brackets(self) -> (&'static str, &'static str) {
        match self.0 {
            Language::Go | Language::Python => ("[", "]"),
            Language::Rust | Language::TypeScript | Language::Java | Language::CSharp
            | Language::Clang => ("<", ">"),
        }
    }

    /// Whether a parameter is spelled `name: Type` rather than `Type name` or `name Type`.
    #[must_use]
    pub(crate) const fn parameter_form(self) -> ParameterForm {
        match self.0 {
            Language::Rust | Language::TypeScript | Language::Python => ParameterForm::LabelColon,
            Language::Go => ParameterForm::LabelSpace,
            Language::Java | Language::CSharp | Language::Clang => ParameterForm::TypeFirst,
        }
    }

    /// The arrow that introduces a result, when the language writes one.
    #[must_use]
    pub(crate) const fn result_arrow(self) -> Option<&'static str> {
        match self.0 {
            Language::Rust | Language::Python => Some(" -> "),
            Language::TypeScript => Some(": "),
            Language::Go => Some(" "),
            Language::Java | Language::CSharp | Language::Clang => None,
        }
    }

    /// The language's own spelling for "this declaration returns nothing".
    #[must_use]
    pub(crate) const fn empty_result(self) -> Option<&'static str> {
        match self.0 {
            Language::Java | Language::CSharp | Language::Clang | Language::TypeScript => {
                Some("void")
            }
            Language::Python => Some("None"),
            Language::Rust | Language::Go => None,
        }
    }

    /// The language's spelling of one builtin scalar.
    ///
    /// Grouped by builtin rather than by language, so a scalar the reader knows under one name in
    /// their own language never arrives spelled in another's.
    #[must_use]
    pub(crate) const fn builtin(self, builtin: BuiltinType) -> &'static str {
        match builtin {
            BuiltinType::Unit => match self.0 {
                Language::Rust => "()",
                _ => "void",
            },
            BuiltinType::Void => "void",
            BuiltinType::Never => match self.0 {
                Language::Rust => "!",
                Language::Python => "NoReturn",
                _ => "never",
            },
            BuiltinType::Bool => match self.0 {
                Language::TypeScript => "boolean",
                _ => "bool",
            },
            BuiltinType::LegacyChar => "char",
            BuiltinType::String => match self.0 {
                Language::Python => "str",
                Language::Java | Language::CSharp => "String",
                _ => "string",
            },
            BuiltinType::Bytes => match self.0 {
                Language::Rust => "[u8]",
                _ => "bytes",
            },
            BuiltinType::I8 => match self.0 {
                Language::Go => "int8",
                Language::Java | Language::CSharp | Language::Clang => "sbyte",
                Language::Python => "int",
                _ => "i8",
            },
            BuiltinType::I16 => match self.0 {
                Language::Go => "int16",
                Language::Java | Language::CSharp | Language::Clang => "short",
                Language::Python => "int",
                _ => "i16",
            },
            BuiltinType::I32 => match self.0 {
                Language::Go => "int32",
                Language::Java | Language::CSharp | Language::Clang | Language::Python => "int",
                _ => "i32",
            },
            BuiltinType::I64 => match self.0 {
                Language::Go => "int64",
                Language::Java | Language::CSharp | Language::Clang => "long",
                Language::Python => "int",
                _ => "i64",
            },
            BuiltinType::I128 => "i128",
            BuiltinType::U8 => match self.0 {
                Language::Go => "uint8",
                Language::Java | Language::CSharp | Language::Clang => "byte",
                Language::Python => "int",
                _ => "u8",
            },
            BuiltinType::U16 => match self.0 {
                Language::Go => "uint16",
                Language::Java | Language::CSharp | Language::Clang => "ushort",
                Language::Python => "int",
                _ => "u16",
            },
            BuiltinType::U32 => match self.0 {
                Language::Go => "uint32",
                Language::Java | Language::CSharp | Language::Clang => "uint",
                Language::Python => "int",
                _ => "u32",
            },
            BuiltinType::U64 => match self.0 {
                Language::Go => "uint64",
                Language::Java | Language::CSharp | Language::Clang => "ulong",
                Language::Python => "int",
                _ => "u64",
            },
            BuiltinType::U128 => "u128",
            BuiltinType::F16 => "f16",
            BuiltinType::F32 => match self.0 {
                Language::Go => "float32",
                Language::Java | Language::CSharp | Language::Clang | Language::Python => "float",
                _ => "f32",
            },
            BuiltinType::F64 => match self.0 {
                Language::Go => "float64",
                Language::Java | Language::CSharp | Language::Clang => "double",
                Language::Python => "float",
                _ => "f64",
            },
            BuiltinType::NativeSignedInteger => match self.0 {
                Language::Go => "int",
                Language::CSharp => "nint",
                _ => "isize",
            },
            BuiltinType::NativeUnsignedInteger => match self.0 {
                Language::Go => "uint",
                Language::CSharp => "nuint",
                _ => "usize",
            },
            BuiltinType::PointerAddressInteger => "uintptr",
            BuiltinType::Object => "object",
            BuiltinType::Any => "any",
            BuiltinType::Unknown => "unknown",
            BuiltinType::Number => "number",
            BuiltinType::BigInt => "bigint",
            BuiltinType::Symbol => "symbol",
            BuiltinType::UniqueSymbol => "unique symbol",
            BuiltinType::Null => "null",
            BuiltinType::Undefined => "undefined",
            BuiltinType::None_ => "None",
            BuiltinType::List => "list",
            BuiltinType::Dict => "dict",
            BuiltinType::Set => "set",
            BuiltinType::FrozenSet => "frozenset",
            BuiltinType::Complex => "complex",
            BuiltinType::Decimal => "decimal",
            BuiltinType::ArbitraryInteger => match self.0 {
                Language::Python => "int",
                _ => "integer",
            },
        }
    }
}

/// How the language orders a parameter's label and type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParameterForm {
    /// `name: Type`.
    LabelColon,
    /// `name Type`.
    LabelSpace,
    /// `Type name`.
    TypeFirst,
}

/// Ordered tokens under one budget, refusing further work once the budget is spent.
pub(crate) struct TokenSink {
    tokens: Vec<Token>,
    budget: usize,
    refused: u32,
}

impl TokenSink {
    pub(crate) fn new(budget: Count) -> Self {
        Self {
            tokens: Vec::new(),
            budget: usize::try_from(budget.0).unwrap_or(usize::MAX),
            refused: 0,
        }
    }

    pub(crate) fn push(&mut self, kind: TokenKind, text: &str) {
        self.push_targeted(kind, text, None);
    }

    pub(crate) fn push_targeted(&mut self, kind: TokenKind, text: &str, target: Option<Target>) {
        if self.tokens.len() >= self.budget {
            self.refused = self.refused.saturating_add(1);
            return;
        }
        self.tokens.push(Token {
            kind,
            text: Text::new(text),
            target,
        });
    }

    pub(crate) const fn is_full(&self) -> bool {
        self.tokens.len() >= self.budget
    }

    pub(crate) fn finish(self) -> (Vec<Token>, Option<Count>) {
        let refused = (self.refused != 0).then_some(Count(self.refused));
        (self.tokens, refused)
    }
}

/// The literal spelling of one literal type node, when the image retained one.
pub(crate) const fn literal_kind(literal: LiteralType) -> TokenKind {
    match literal {
        LiteralType::String(_) | LiteralType::Number(_) | LiteralType::BigInt(_) => {
            TokenKind::Literal
        }
        LiteralType::Boolean(_) | LiteralType::Null | LiteralType::Undefined => TokenKind::Keyword,
    }
}

/// Rust's spelling of a reference or raw-pointer mutability.
pub(crate) const fn rust_mutability(mutability: Mutability) -> &'static str {
    match mutability {
        Mutability::Immutable => "",
        Mutability::Mutable => "mut ",
    }
}

/// Whether one type node is the language's own "nothing" and should be omitted from a result.
pub(crate) const fn is_empty_result(expr: TypeExpr) -> bool {
    matches!(
        expr.concrete(),
        Some(ConcreteType::Builtin(BuiltinType::Unit | BuiltinType::Void))
    )
}

/// A type coordinate paired with the depth at which the walker met it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TypeCursor {
    /// The type to spell.
    pub(crate) id: TypeId,
    /// Remaining nesting the walker may descend.
    pub(crate) budget: u8,
}

impl TypeCursor {
    /// Deepest nesting the type walker descends before falling back to canonical text.
    pub(crate) const MAX_DEPTH: u8 = 8;

    pub(crate) const fn root(id: TypeId) -> Self {
        Self {
            id,
            budget: Self::MAX_DEPTH,
        }
    }

    pub(crate) const fn child(self, id: TypeId) -> Option<Self> {
        match self.budget.checked_sub(1) {
            Some(budget) if budget != 0 => Some(Self { id, budget }),
            _ => None,
        }
    }
}

use compiler_ir::{
    ComputedType, EntityId, ObjectMember, PropertyKey, SemanticReader, TupleElement, TypeQuery,
    Visibility, WildcardBound,
};

use crate::{Name, Projector};

impl<Reader: SemanticReader + ?Sized> Projector<'_, Reader> {
    /// Builds one declaration's signature and reports the tokens the budget refused.
    ///
    /// An entity the image does not hold yields an empty signature rather than a fabricated one.
    #[must_use]
    pub fn signature(&self, entity: EntityId, budget: Count) -> (crate::Signature, Option<Count>) {
        let mut sink = TokenSink::new(budget);
        self.write_declaration(entity, &mut sink);
        let (tokens, refused) = sink.finish();
        (crate::Signature::new(tokens), refused)
    }

    fn write_declaration(&self, entity: EntityId, sink: &mut TokenSink) {
        let Some(row) = self.reader.entity(entity) else {
            return;
        };
        let name = self.name(entity).unwrap_or_else(|| Name::displayable(b""));
        let function = row
            .semantic_type
            .and_then(|id| self.reader.ty(id))
            .and_then(TypeExpr::concrete)
            .and_then(|concrete| match concrete {
                ConcreteType::Function {
                    parameters,
                    results,
                    ..
                } => Some((parameters, results)),
                _ => None,
            });
        if row.visibility == Visibility::Public
            && let Some(prefix) = self.dialect.public_prefix()
        {
            sink.push(TokenKind::Keyword, prefix);
            sink.push(TokenKind::Text, " ");
        }
        match function {
            Some((parameters, results)) => self.write_function(entity, &name, parameters, results, sink),
            None => self.write_value(entity, &name, row.kind, row.semantic_type, sink),
        }
    }

    fn write_value(
        &self,
        entity: EntityId,
        name: &Name,
        kind: EntityKind,
        semantic_type: Option<TypeId>,
        sink: &mut TokenSink,
    ) {
        if let Some(keyword) = self.dialect.keyword(kind) {
            sink.push(TokenKind::Keyword, keyword);
            sink.push(TokenKind::Text, " ");
        }
        sink.push(TokenKind::Name, name.as_str());
        self.write_generics(entity, sink);
        let Some(id) = semantic_type else {
            return;
        };
        match kind {
            EntityKind::Alias => sink.push(TokenKind::Punctuation, " = "),
            _ => sink.push(TokenKind::Punctuation, ": "),
        }
        self.write_type(TypeCursor::root(id), sink);
    }

    fn write_function(
        &self,
        entity: EntityId,
        name: &Name,
        parameters: compiler_ir::TupleElementListId,
        results: compiler_ir::TupleElementListId,
        sink: &mut TokenSink,
    ) {
        let leading = self.dialect.result_leads();
        if leading {
            self.write_results(results, sink);
            sink.push(TokenKind::Text, " ");
        } else if let Some(keyword) = self.dialect.keyword(EntityKind::Function) {
            sink.push(TokenKind::Keyword, keyword);
            sink.push(TokenKind::Text, " ");
        }
        sink.push(TokenKind::Name, name.as_str());
        self.write_generics(entity, sink);
        sink.push(TokenKind::Punctuation, "(");
        self.write_parameters(parameters, sink);
        sink.push(TokenKind::Punctuation, ")");
        if !leading {
            self.write_trailing_results(results, sink);
        }
    }

    fn write_parameters(&self, parameters: compiler_ir::TupleElementListId, sink: &mut TokenSink) {
        let Some(elements) = self.reader.tuple_elements(parameters) else {
            return;
        };
        for (index, element) in elements.enumerate() {
            if index != 0 {
                sink.push(TokenKind::Punctuation, ", ");
            }
            if sink.is_full() {
                return;
            }
            self.write_parameter(element, sink);
        }
    }

    fn write_parameter(&self, element: TupleElement, sink: &mut TokenSink) {
        let label = element.label.map(|atom| self.atom_text(atom));
        match (self.dialect.parameter_form(), label) {
            (ParameterForm::TypeFirst, label) => {
                self.write_type(TypeCursor::root(element.ty), sink);
                if let Some(label) = label {
                    sink.push(TokenKind::Text, " ");
                    sink.push(TokenKind::Binding, label.as_str());
                }
            }
            (ParameterForm::LabelColon, Some(label)) => {
                sink.push(TokenKind::Binding, label.as_str());
                sink.push(TokenKind::Punctuation, ": ");
                self.write_type(TypeCursor::root(element.ty), sink);
            }
            (ParameterForm::LabelSpace, Some(label)) => {
                sink.push(TokenKind::Binding, label.as_str());
                sink.push(TokenKind::Text, " ");
                self.write_type(TypeCursor::root(element.ty), sink);
            }
            (ParameterForm::LabelColon | ParameterForm::LabelSpace, None) => {
                self.write_type(TypeCursor::root(element.ty), sink);
            }
        }
        if matches!(element.kind, compiler_ir::TupleElementKind::Optional) {
            sink.push(TokenKind::Punctuation, "?");
        }
    }

    fn write_trailing_results(&self, results: compiler_ir::TupleElementListId, sink: &mut TokenSink) {
        let Some(elements) = self.reader.tuple_elements(results) else {
            return;
        };
        let rows: Vec<TupleElement> = elements.collect();
        let meaningful = rows.iter().any(|row| {
            self.reader
                .ty(row.ty)
                .is_none_or(|expr| !is_empty_result(expr))
        });
        if rows.is_empty() || !meaningful {
            if let Some(arrow) = self.dialect.result_arrow()
                && let Some(empty) = self.dialect.empty_result()
            {
                sink.push(TokenKind::Punctuation, arrow);
                sink.push(TokenKind::Keyword, empty);
            }
            return;
        }
        let Some(arrow) = self.dialect.result_arrow() else {
            return;
        };
        sink.push(TokenKind::Punctuation, arrow);
        self.write_result_rows(&rows, sink);
    }

    fn write_results(&self, results: compiler_ir::TupleElementListId, sink: &mut TokenSink) {
        let rows: Vec<TupleElement> = self
            .reader
            .tuple_elements(results)
            .map(Iterator::collect)
            .unwrap_or_default();
        let meaningful = rows.iter().any(|row| {
            self.reader
                .ty(row.ty)
                .is_none_or(|expr| !is_empty_result(expr))
        });
        if rows.is_empty() || !meaningful {
            if let Some(empty) = self.dialect.empty_result() {
                sink.push(TokenKind::Keyword, empty);
            }
            return;
        }
        self.write_result_rows(&rows, sink);
    }

    fn write_result_rows(&self, rows: &[TupleElement], sink: &mut TokenSink) {
        if rows.len() > 1 {
            sink.push(TokenKind::Punctuation, "(");
        }
        for (index, row) in rows.iter().enumerate() {
            if index != 0 {
                sink.push(TokenKind::Punctuation, ", ");
            }
            self.write_type(TypeCursor::root(row.ty), sink);
        }
        if rows.len() > 1 {
            sink.push(TokenKind::Punctuation, ")");
        }
    }

    /// Writes the declaration's own generic parameters and regions, when the image captured them.
    fn write_generics(&self, entity: EntityId, sink: &mut TokenSink) {
        let mut names: Vec<(TokenKind, String)> = Vec::new();
        if let Some(facts) = self.reader.rust_extension(entity)
            && let Some(lifetimes) = self.reader.atom_list(facts.lifetimes)
        {
            for atom in lifetimes {
                names.push((TokenKind::Lifetime, self.atom_text(atom).as_str().to_owned()));
            }
        }
        for parameters in [
            self.reader
                .typescript_extension(entity)
                .map(|facts| facts.type_parameters),
            self.reader
                .go_extension(entity)
                .map(|facts| facts.type_parameters),
            self.reader
                .csharp_extension(entity)
                .map(|facts| facts.constraints),
            self.reader
                .clang_extension(entity)
                .map(|facts| facts.templates),
        ]
        .into_iter()
        .flatten()
        {
            let Some(rows) = self.reader.type_parameters(parameters) else {
                continue;
            };
            for parameter in rows {
                names.push((
                    TokenKind::Type,
                    self.atom_text(parameter.name).as_str().to_owned(),
                ));
            }
        }
        if names.is_empty() {
            return;
        }
        let (open, close) = self.dialect.type_argument_brackets();
        sink.push(TokenKind::Punctuation, open);
        for (index, (kind, name)) in names.iter().enumerate() {
            if index != 0 {
                sink.push(TokenKind::Punctuation, ", ");
            }
            sink.push(*kind, name);
        }
        sink.push(TokenKind::Punctuation, close);
    }

    /// Writes one type node, falling back to the canonical spelling when the node is unwalkable.
    pub(crate) fn write_type(&self, cursor: TypeCursor, sink: &mut TokenSink) {
        if sink.is_full() {
            return;
        }
        let Some(expr) = self.reader.ty(cursor.id) else {
            sink.push(TokenKind::Type, "?");
            return;
        };
        match expr {
            TypeExpr::Concrete(concrete) => self.write_concrete(concrete, cursor, sink),
            TypeExpr::Computed(computed) => self.write_computed(computed, cursor, sink),
            TypeExpr::Unknown(unknown) => {
                let text = unknown
                    .spelling
                    .map_or_else(|| unknown_reason(unknown.reason).to_owned(), |atom| {
                        self.atom_text(atom).as_str().to_owned()
                    });
                sink.push(TokenKind::Type, &text);
            }
        }
    }

    fn write_concrete(&self, concrete: ConcreteType, cursor: TypeCursor, sink: &mut TokenSink) {
        match concrete {
            ConcreteType::Builtin(builtin) => {
                sink.push(TokenKind::Type, self.dialect.builtin(builtin));
            }
            ConcreteType::Literal(literal) => self.write_literal(literal, sink),
            ConcreteType::Nominal(entity) => {
                let symbol = self.symbol(entity).ok();
                let text = symbol.as_ref().map_or_else(
                    || String::from("?"),
                    |symbol| symbol.name.as_str().to_owned(),
                );
                sink.push_targeted(TokenKind::Type, &text, symbol.map(Target::Local));
            }
            ConcreteType::External(external) => {
                let target = self.external(external);
                let text = match &target {
                    Target::External(reference) => reference.display.as_str().to_owned(),
                    Target::Unresolved(text) => text.as_str().to_owned(),
                    Target::Local(symbol) => symbol.name.as_str().to_owned(),
                };
                sink.push_targeted(TokenKind::Type, &text, Some(target));
            }
            ConcreteType::Parameter(atom) => {
                sink.push(TokenKind::Type, self.atom_text(atom).as_str());
            }
            ConcreteType::Applied {
                constructor,
                arguments,
            } => self.write_applied(constructor, arguments, cursor, sink),
            ConcreteType::Tuple(elements) => self.write_tuple(elements, cursor, sink),
            ConcreteType::Object(members) => self.write_object(members, cursor, sink),
            ConcreteType::Function {
                parameters, results, ..
            } => self.write_function_type(parameters, results, cursor, sink),
            ConcreteType::Reference {
                target,
                mutability,
                lifetime,
            } => self.write_reference(target, mutability, lifetime, cursor, sink),
            ConcreteType::Pointer { target, mutability } => {
                self.write_pointer(target, mutability, cursor, sink);
            }
            ConcreteType::CxxReference { target, category } => {
                self.write_child(target, cursor, sink);
                sink.push(
                    TokenKind::Punctuation,
                    match category {
                        compiler_ir::CxxReferenceCategory::Lvalue => "&",
                        compiler_ir::CxxReferenceCategory::Rvalue => "&&",
                    },
                );
            }
            ConcreteType::CPointer { target } => {
                self.write_child(target, cursor, sink);
                sink.push(TokenKind::Punctuation, "*");
            }
            ConcreteType::Slice(element) | ConcreteType::Array { element, .. } => {
                self.write_slice(element, cursor, sink);
            }
            ConcreteType::Optional(inner) => self.write_optional(inner, cursor, sink),
            ConcreteType::Union(list) => self.write_joined(list, " | ", cursor, sink),
            ConcreteType::Intersection(list) => self.write_joined(list, " & ", cursor, sink),
            ConcreteType::ImplTrait(list) => {
                sink.push(TokenKind::Keyword, "impl ");
                self.write_joined(list, " + ", cursor, sink);
            }
            ConcreteType::DynTrait(list) => {
                sink.push(TokenKind::Keyword, "dyn ");
                self.write_joined(list, " + ", cursor, sink);
            }
            ConcreteType::Wildcard(bound) => self.write_wildcard(bound, cursor, sink),
            ConcreteType::Annotated { target, .. } => self.write_child(target, cursor, sink),
            ConcreteType::Inferred(atom) => match atom {
                Some(atom) => sink.push(TokenKind::Type, self.atom_text(atom).as_str()),
                None => sink.push(TokenKind::Type, "_"),
            },
            ConcreteType::QualifiedPath { spelling, .. } => {
                sink.push(TokenKind::Type, self.atom_text(spelling).as_str());
            }
            ConcreteType::Map { key, value } => self.write_map(key, value, cursor, sink),
            ConcreteType::Channel { direction, element } => {
                sink.push(
                    TokenKind::Keyword,
                    match direction {
                        compiler_ir::ChannelDirection::Both => "chan ",
                        compiler_ir::ChannelDirection::Send => "chan<- ",
                        compiler_ir::ChannelDirection::Receive => "<-chan ",
                    },
                );
                self.write_child(element, cursor, sink);
            }
            ConcreteType::CQualified { .. }
            | ConcreteType::CxxMemberPointer { .. }
            | ConcreteType::CBlockPointer { .. }
            | ConcreteType::NativeCharacter { .. } => self.write_canonical(cursor, sink),
        }
    }

    fn write_computed(&self, computed: ComputedType, cursor: TypeCursor, sink: &mut TokenSink) {
        match computed {
            ComputedType::TypeOf(TypeQuery::Entity(entity)) => {
                let symbol = self.symbol(entity).ok();
                let text = symbol
                    .as_ref()
                    .map_or_else(|| String::from("?"), |symbol| symbol.name.as_str().to_owned());
                sink.push(TokenKind::Keyword, "typeof ");
                sink.push_targeted(TokenKind::Type, &text, symbol.map(Target::Local));
            }
            ComputedType::TypeOf(TypeQuery::External(external)) => {
                let target = self.external(external);
                let text = match &target {
                    Target::External(reference) => reference.display.as_str().to_owned(),
                    Target::Unresolved(text) => text.as_str().to_owned(),
                    Target::Local(symbol) => symbol.name.as_str().to_owned(),
                };
                sink.push(TokenKind::Keyword, "typeof ");
                sink.push_targeted(TokenKind::Type, &text, Some(target));
            }
            ComputedType::Awaited(inner) | ComputedType::KeyOf(inner) => {
                sink.push(
                    TokenKind::Keyword,
                    if matches!(computed, ComputedType::Awaited(_)) {
                        "await "
                    } else {
                        "keyof "
                    },
                );
                self.write_child(inner, cursor, sink);
            }
            ComputedType::This => sink.push(TokenKind::Keyword, "this"),
            _ => self.write_canonical(cursor, sink),
        }
    }

    fn write_literal(&self, literal: LiteralType, sink: &mut TokenSink) {
        let kind = literal_kind(literal);
        match literal {
            LiteralType::String(atom)
            | LiteralType::Number(atom)
            | LiteralType::BigInt(atom) => {
                sink.push(kind, self.atom_text(atom).as_str());
            }
            LiteralType::Boolean(value) => {
                sink.push(kind, if value { "true" } else { "false" });
            }
            LiteralType::Null => sink.push(kind, "null"),
            LiteralType::Undefined => sink.push(kind, "undefined"),
        }
    }

    fn write_child(&self, id: TypeId, cursor: TypeCursor, sink: &mut TokenSink) {
        match cursor.child(id) {
            Some(next) => self.write_type(next, sink),
            None => self.write_canonical(TypeCursor::root(id), sink),
        }
    }

    fn write_applied(
        &self,
        constructor: TypeId,
        arguments: compiler_ir::TypeListId,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        self.write_child(constructor, cursor, sink);
        let Some(rows) = self.reader.types(arguments) else {
            return;
        };
        let (open, close) = self.dialect.type_argument_brackets();
        sink.push(TokenKind::Punctuation, open);
        for (index, argument) in rows.enumerate() {
            if index != 0 {
                sink.push(TokenKind::Punctuation, ", ");
            }
            self.write_child(argument, cursor, sink);
        }
        sink.push(TokenKind::Punctuation, close);
    }

    fn write_tuple(
        &self,
        elements: compiler_ir::TupleElementListId,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        sink.push(TokenKind::Punctuation, "(");
        if let Some(rows) = self.reader.tuple_elements(elements) {
            for (index, element) in rows.enumerate() {
                if index != 0 {
                    sink.push(TokenKind::Punctuation, ", ");
                }
                self.write_child(element.ty, cursor, sink);
            }
        }
        sink.push(TokenKind::Punctuation, ")");
    }

    fn write_object(
        &self,
        members: compiler_ir::ObjectMemberListId,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        sink.push(TokenKind::Punctuation, "{ ");
        if let Some(rows) = self.reader.object_members(members) {
            for (index, member) in rows.enumerate() {
                if index != 0 {
                    sink.push(TokenKind::Punctuation, ", ");
                }
                self.write_object_member(member, cursor, sink);
            }
        }
        sink.push(TokenKind::Punctuation, " }");
    }

    fn write_object_member(
        &self,
        member: ObjectMember,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        match member {
            ObjectMember::Property { key, ty, .. } => {
                self.write_property_key(key, cursor, sink);
                sink.push(TokenKind::Punctuation, ": ");
                self.write_child(ty, cursor, sink);
            }
            ObjectMember::Method { key, signature, .. } => {
                self.write_property_key(key, cursor, sink);
                sink.push(TokenKind::Punctuation, ": ");
                self.write_child(signature, cursor, sink);
            }
            ObjectMember::Index {
                parameter,
                key,
                value,
                ..
            } => {
                sink.push(TokenKind::Punctuation, "[");
                sink.push(TokenKind::Binding, self.atom_text(parameter).as_str());
                sink.push(TokenKind::Punctuation, ": ");
                self.write_child(key, cursor, sink);
                sink.push(TokenKind::Punctuation, "]: ");
                self.write_child(value, cursor, sink);
            }
            ObjectMember::Call(signature) | ObjectMember::Construct(signature) => {
                self.write_child(signature, cursor, sink);
            }
        }
    }

    fn write_property_key(&self, key: PropertyKey, cursor: TypeCursor, sink: &mut TokenSink) {
        match key {
            PropertyKey::Named(atom) | PropertyKey::Private(atom) | PropertyKey::Numeric(atom) => {
                sink.push(TokenKind::Binding, self.atom_text(atom).as_str());
            }
            PropertyKey::Computed(id) => self.write_child(id, cursor, sink),
        }
    }

    fn write_function_type(
        &self,
        parameters: compiler_ir::TupleElementListId,
        results: compiler_ir::TupleElementListId,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        if let Some(keyword) = self.dialect.keyword(EntityKind::Function) {
            sink.push(TokenKind::Keyword, keyword);
        }
        sink.push(TokenKind::Punctuation, "(");
        if let Some(rows) = self.reader.tuple_elements(parameters) {
            for (index, element) in rows.enumerate() {
                if index != 0 {
                    sink.push(TokenKind::Punctuation, ", ");
                }
                self.write_child(element.ty, cursor, sink);
            }
        }
        sink.push(TokenKind::Punctuation, ")");
        let rows: Vec<TupleElement> = self
            .reader
            .tuple_elements(results)
            .map(Iterator::collect)
            .unwrap_or_default();
        if rows.is_empty() {
            return;
        }
        if let Some(arrow) = self.dialect.result_arrow() {
            sink.push(TokenKind::Punctuation, arrow);
        }
        for (index, row) in rows.iter().enumerate() {
            if index != 0 {
                sink.push(TokenKind::Punctuation, ", ");
            }
            self.write_child(row.ty, cursor, sink);
        }
    }

    fn write_reference(
        &self,
        target: TypeId,
        mutability: Mutability,
        lifetime: Option<compiler_ir::AtomId>,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        match self.dialect.language() {
            Language::Rust => {
                sink.push(TokenKind::Punctuation, "&");
                if let Some(atom) = lifetime {
                    sink.push(TokenKind::Lifetime, self.atom_text(atom).as_str());
                    sink.push(TokenKind::Text, " ");
                }
                let mutable = rust_mutability(mutability);
                if !mutable.is_empty() {
                    sink.push(TokenKind::Keyword, mutable);
                }
                self.write_child(target, cursor, sink);
            }
            Language::Clang => {
                self.write_child(target, cursor, sink);
                sink.push(TokenKind::Punctuation, "&");
            }
            _ => self.write_child(target, cursor, sink),
        }
    }

    fn write_pointer(
        &self,
        target: TypeId,
        mutability: Mutability,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        match self.dialect.language() {
            Language::Rust => {
                sink.push(
                    TokenKind::Punctuation,
                    match mutability {
                        Mutability::Immutable => "*const ",
                        Mutability::Mutable => "*mut ",
                    },
                );
                self.write_child(target, cursor, sink);
            }
            Language::Clang => {
                self.write_child(target, cursor, sink);
                sink.push(TokenKind::Punctuation, "*");
            }
            _ => {
                sink.push(TokenKind::Punctuation, "*");
                self.write_child(target, cursor, sink);
            }
        }
    }

    fn write_slice(&self, element: TypeId, cursor: TypeCursor, sink: &mut TokenSink) {
        match self.dialect.language() {
            Language::Rust => {
                sink.push(TokenKind::Punctuation, "[");
                self.write_child(element, cursor, sink);
                sink.push(TokenKind::Punctuation, "]");
            }
            Language::Go => {
                sink.push(TokenKind::Punctuation, "[]");
                self.write_child(element, cursor, sink);
            }
            Language::Python => {
                sink.push(TokenKind::Type, "list");
                sink.push(TokenKind::Punctuation, "[");
                self.write_child(element, cursor, sink);
                sink.push(TokenKind::Punctuation, "]");
            }
            _ => {
                self.write_child(element, cursor, sink);
                sink.push(TokenKind::Punctuation, "[]");
            }
        }
    }

    fn write_optional(&self, inner: TypeId, cursor: TypeCursor, sink: &mut TokenSink) {
        match self.dialect.language() {
            Language::Rust => {
                sink.push(TokenKind::Type, "Option");
                sink.push(TokenKind::Punctuation, "<");
                self.write_child(inner, cursor, sink);
                sink.push(TokenKind::Punctuation, ">");
            }
            Language::Python => {
                self.write_child(inner, cursor, sink);
                sink.push(TokenKind::Punctuation, " | ");
                sink.push(TokenKind::Keyword, "None");
            }
            Language::Go => {
                sink.push(TokenKind::Punctuation, "*");
                self.write_child(inner, cursor, sink);
            }
            _ => {
                self.write_child(inner, cursor, sink);
                sink.push(TokenKind::Punctuation, "?");
            }
        }
    }

    fn write_joined(
        &self,
        list: compiler_ir::TypeListId,
        separator: &str,
        cursor: TypeCursor,
        sink: &mut TokenSink,
    ) {
        let Some(rows) = self.reader.types(list) else {
            return;
        };
        for (index, id) in rows.enumerate() {
            if index != 0 {
                sink.push(TokenKind::Punctuation, separator);
            }
            self.write_child(id, cursor, sink);
        }
    }

    fn write_wildcard(&self, bound: WildcardBound, cursor: TypeCursor, sink: &mut TokenSink) {
        sink.push(TokenKind::Punctuation, "?");
        match bound {
            WildcardBound::Unbounded => {}
            WildcardBound::Extends(id) => {
                sink.push(TokenKind::Keyword, " extends ");
                self.write_child(id, cursor, sink);
            }
            WildcardBound::Super(id) => {
                sink.push(TokenKind::Keyword, " super ");
                self.write_child(id, cursor, sink);
            }
        }
    }

    fn write_map(&self, key: TypeId, value: TypeId, cursor: TypeCursor, sink: &mut TokenSink) {
        match self.dialect.language() {
            Language::Go => {
                sink.push(TokenKind::Keyword, "map");
                sink.push(TokenKind::Punctuation, "[");
                self.write_child(key, cursor, sink);
                sink.push(TokenKind::Punctuation, "]");
                self.write_child(value, cursor, sink);
            }
            language => {
                sink.push(
                    TokenKind::Type,
                    match language {
                        Language::Python => "dict",
                        Language::TypeScript => "Record",
                        _ => "Map",
                    },
                );
                let (open, close) = self.dialect.type_argument_brackets();
                sink.push(TokenKind::Punctuation, open);
                self.write_child(key, cursor, sink);
                sink.push(TokenKind::Punctuation, ", ");
                self.write_child(value, cursor, sink);
                sink.push(TokenKind::Punctuation, close);
            }
        }
    }

    /// Emits the canonical structural spelling of one node as a single untargeted token.
    ///
    /// Reached only for nodes this dialect has no source syntax for. The canonical renderer is
    /// exact, so the reader sees what the image actually holds rather than a guess, and no target
    /// is attached because the fallback did not resolve one.
    fn write_canonical(&self, cursor: TypeCursor, sink: &mut TokenSink) {
        let Some(depth) = core::num::NonZeroUsize::new(usize::from(cursor.budget).max(1)) else {
            return;
        };
        let limits = compiler_ir::CanonicalTypeRenderLimits::new(depth);
        let Ok(prepared) = compiler_ir::prepare_canonical_type(self.reader, cursor.id, limits)
        else {
            sink.push(TokenKind::Type, "?");
            return;
        };
        let mut text = String::new();
        if prepared.write_to(&mut text).is_ok() {
            sink.push(TokenKind::Type, &text);
        } else {
            sink.push(TokenKind::Type, "?");
        }
    }
}

/// The word one unknown-type reason reads as, when the image kept no spelling.
const fn unknown_reason(reason: compiler_ir::UnknownReason) -> &'static str {
    match reason {
        compiler_ir::UnknownReason::Unannotated => "unannotated",
        compiler_ir::UnknownReason::DynamicallyTyped => "dynamic",
        compiler_ir::UnknownReason::UnresolvedLocalName => "unresolved",
        compiler_ir::UnknownReason::UnresolvedExternal => "unresolved-external",
        compiler_ir::UnknownReason::TruncatedAtDepthLimit => "truncated",
        compiler_ir::UnknownReason::OracleGap => "oracle-gap",
        compiler_ir::UnknownReason::NoIrRepresentation => "unrepresented",
        compiler_ir::UnknownReason::Error => "error",
    }
}

