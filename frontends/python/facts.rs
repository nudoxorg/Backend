//! Projects source-preserving Python syntax facts directly from Ruff's AST.
//! Retains unresolved syntax explicitly instead of claiming type resolution.
//! Keeps Python syntax authority independent of compiler IR transport.
//!
//! The crate exposes two peer authorities: the Ruff syntax extractor in this
//! module, and the bounded pyrefly type-authority transaction in [`checker`].

use backend_compile::PythonVersion;
use ruff_python_ast::{
    self as ast,
    visitor::{self, Visitor},
};
use ruff_python_parser::{Mode, ParseOptions, Parsed, parse_unchecked};
use ruff_text_size::Ranged;
use thiserror::Error;

/// A half-open UTF-8 byte span into the admitted source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// First byte offset, inclusive.
    pub start: u32,
    /// Second byte offset, exclusive.
    pub end: u32,
}
/// Why an annotation remains unresolved by the syntax authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeReason {
    /// The position carried no annotation at all.
    Unannotated {
        /// The declaration position that had no written annotation.
        position: AnnotationPosition,
    },
    /// Ruff proved the expression class but this extractor has no projection for it.
    UnsupportedSyntax {
        /// Ruff's expression category that was not projected.
        kind: AnnotationSyntaxKind,
        /// Exact source span of the unsupported expression.
        span: Span,
    },
    /// The quoted-annotation recursion guard fired before resolution finished.
    TruncatedAtDepthLimit,
}
/// The declaration position occupied by a written annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationPosition {
    /// A function parameter annotation.
    Parameter,
    /// A function return annotation.
    Return,
    /// A class field annotation.
    Field,
    /// The value expression of a PEP 695 `type` alias statement.
    AliasValue,
}
/// A source-preserving projection of one Python annotation expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Annotation {
    /// One name whose extractor-proved source span is retained independently
    /// of its containing annotation. Nested type-variable uses require this
    /// leaf span: their parent span names `list[T]`, not the written `T`.
    Name {
        /// Extracted identifier or qualified identifier spelling.
        name: String,
        /// Exact span when this expression came directly from the entered
        /// module. Quoted annotation recursion has no outer-source child
        /// coordinates, so it explicitly carries `None`.
        span: Option<Span>,
    },
    /// A generic annotation application.
    Generic {
        /// The generic base expression.
        base: Box<Annotation>,
        /// Generic arguments in source order.
        args: Vec<Annotation>,
    },
    /// A bracketed list display inside an annotation, such as the parameter
    /// list of `Callable[[int], str]`.
    List(Vec<Annotation>),
    /// The literal value of a quoted annotation that could not be parsed.
    StringLiteral(String),
    /// A PEP 604 union expression.
    Union(Vec<Annotation>),
    /// A `Literal[...]` expression with typed literal values.
    Literal(Vec<LiteralValue>),
    /// The explicit `None` annotation.
    None,
    /// An annotation expression the syntax authority cannot project.
    Unknown(TypeReason),
}

/// Ruff's closed expression categories retained for unsupported annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationSyntaxKind {
    /// A bare identifier.
    Name,
    /// An attribute expression.
    Attribute,
    /// A subscript expression.
    Subscript,
    /// A boolean operator expression.
    BooleanOperator,
    /// A named expression.
    NamedExpression,
    /// A binary operator expression.
    BinaryOperator,
    /// A unary operator expression.
    UnaryOperator,
    /// A lambda expression.
    Lambda,
    /// A conditional expression.
    Conditional,
    /// A dictionary expression.
    Dictionary,
    /// A set expression.
    Set,
    /// A list comprehension.
    ListComprehension,
    /// A set comprehension.
    SetComprehension,
    /// A dictionary comprehension.
    DictionaryComprehension,
    /// A generator expression.
    Generator,
    /// An await expression.
    Await,
    /// A yield expression.
    Yield,
    /// A yield-from expression.
    YieldFrom,
    /// A comparison expression.
    Comparison,
    /// A call expression.
    Call,
    /// An f-string expression.
    FormattedString,
    /// A template string expression.
    TemplateString,
    /// A string literal expression.
    StringLiteral,
    /// A bytes literal expression.
    Bytes,
    /// A numeric literal expression.
    Number,
    /// A boolean literal expression.
    Boolean,
    /// A `None` literal expression.
    NoneLiteral,
    /// An ellipsis literal expression.
    Ellipsis,
    /// A starred expression.
    Starred,
    /// A list expression.
    List,
    /// A tuple expression.
    Tuple,
    /// A slice expression.
    Slice,
    /// An IPython escape expression.
    IpythonEscape,
}

/// A typed literal retained from a `Literal[...]` annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralValue {
    /// A string literal.
    String(String),
    /// An integer literal's source spelling.
    Integer(String),
    /// A floating-point literal.
    Float {
        /// IEEE-754 representation of the value.
        ieee_bits: u64,
    },
    /// A complex-number literal.
    Complex {
        /// IEEE-754 representation of the real part.
        real_ieee_bits: u64,
        /// IEEE-754 representation of the imaginary part.
        imaginary_ieee_bits: u64,
    },
    /// A boolean literal.
    Boolean(bool),
    /// The `None` literal.
    None,
    /// The ellipsis literal.
    Ellipsis,
    /// An unsupported literal expression category.
    Unsupported(AnnotationSyntaxKind),
}

/// The kind of source declaration projected by Ruff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    /// The synthetic module declaration.
    Module,
    /// A class declaration.
    Class,
    /// A function declaration.
    Function,
    /// A class field declaration.
    Field,
    /// A module or class assignment declaration.
    Constant,
    /// An import binding declaration.
    Alias,
}
/// The recognized semantic form of a class declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassForm {
    /// A class with no recognized special form.
    Plain,
    /// A dataclass declaration.
    Dataclass,
    /// A protocol declaration.
    Protocol,
    /// A TypedDict declaration.
    TypedDict,
    /// An enum declaration.
    Enum,
}
/// The receiver behavior of a method declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    /// A normal instance method.
    Plain,
    /// A static method.
    StaticMethod,
    /// A class method.
    ClassMethod,
    /// A property accessor.
    Property,
}
/// The calling convention of a function parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterKind {
    /// A positional-only parameter.
    PositionalOnly,
    /// A positional-or-keyword parameter.
    PositionalOrKeyword,
    /// A variadic positional parameter.
    VarArgs,
    /// A keyword-only parameter.
    KeywordOnly,
    /// A variadic keyword parameter.
    KwArgs,
}
/// A statically recognized call occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccurrenceKind {
    /// A direct function call.
    FunctionCall,
    /// A method call.
    MethodCall,
}
/// Facts for one function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterFact {
    /// Declared parameter name.
    pub name: String,
    /// Exact source span of the declared identifier, so every consumer can
    /// borrow the name bytes without re-tokenizing the parameter range.
    pub name_span: Span,
    /// Calling convention selected by the syntax authority.
    pub kind: ParameterKind,
    /// Whether the parameter has a default expression.
    pub has_default: bool,
    /// Exact source of the default expression.
    pub default_source: Option<String>,
    /// Projected written annotation or explicit unresolved reason.
    pub annotation: Annotation,
    /// Exact source span of the written annotation expression, if any.
    pub annotation_span: Option<Span>,
    /// Full parameter span including annotation/default syntax.
    pub span: Span,
}
/// Facts for one source declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationFact {
    /// Declared name.
    pub name: String,
    /// Exact source span of the declared identifier.
    pub name_span: Span,
    /// Declaration kind proven by Ruff's AST.
    pub kind: DeclarationKind,
    /// Full declaration span.
    pub span: Span,
    /// Base annotations for a class declaration.
    pub bases: Vec<Annotation>,
    /// Recognized class form, when applicable.
    pub class_form: Option<ClassForm>,
    /// Decorator spellings in source order.
    pub decorators: Vec<String>,
    /// Source span of each decorator spelling, parallel to `decorators`;
    /// every span covers the leading `@` so the spelling is the tail.
    pub decorator_spans: Vec<Span>,
    /// Whether the declaration is asynchronous.
    pub is_async: bool,
    /// Method receiver form.
    pub receiver: ReceiverKind,
    /// Parameters in source order.
    pub parameters: Vec<ParameterFact>,
    /// Exact value expression source, when one exists.
    pub value_source: Option<String>,
    /// For alias declarations, the source span of the imported module
    /// spelling (`json` in `from json import loads`, `os.path` in
    /// `import os.path`), so cross-package keys can borrow source bytes.
    pub value_span: Option<Span>,
    /// The PEP 695 type parameters declared by this function, class, or
    /// `type` alias (`T` in `class Box[T]:`), each as the exact source span
    /// of its written identifier, in declaration order. Their names scope
    /// the annotations of exactly this declaration, and every consumer can
    /// borrow the name bytes without a fresh buffer.
    pub type_parameters: Vec<Span>,
    /// For a `TypedDict` class, the written `total=` keyword value; `None`
    /// when the keyword is absent, which PEP 589 defines as total.
    pub total: Option<bool>,
    /// Attached docstring, when present.
    pub docstring: Option<DocstringFact>,
}
/// A source-backed docstring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocstringFact {
    /// Exact docstring source text.
    pub raw: String,
    /// Span of the docstring literal.
    pub span: Span,
}
/// A source-backed statically recognized call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceFact {
    /// Declaration containing the occurrence.
    pub owner: String,
    /// Called target name.
    pub target: String,
    /// Closed occurrence kind.
    pub kind: OccurrenceKind,
    /// Authority confidence level.
    pub confidence: Confidence,
    /// Exact call expression span.
    pub span: Span,
}
/// Confidence attached to a syntax-derived occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// The occurrence came directly from Ruff's typed AST index.
    Index,
}
/// Complete source-preserving syntax facts for one Python module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFacts {
    /// Stable module identity used by local references.
    pub identity: String,
    /// Full source span.
    pub span: Span,
    /// Declarations in source order.
    pub declarations: Vec<DeclarationFact>,
    /// Static call occurrences in source order.
    pub occurrences: Vec<OccurrenceFact>,
    /// Module docstring, when present.
    pub docstring: Option<DocstringFact>,
    /// Written annotations in source order.
    pub annotations: Vec<AnnotationFact>,
}
/// One written annotation attached to a declaration position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationFact {
    /// Owning declaration name.
    pub owner: String,
    /// Position occupied by the annotation.
    pub position: AnnotationPosition,
    /// Projected annotation tree.
    pub annotation: Annotation,
    /// Exact annotation expression span.
    pub span: Span,
}

/// Owns one rejected Ruff parse so every diagnostic remains borrowable without a second copy.
#[derive(Debug)]
pub struct RejectedSyntax {
    /// Canonical grammar used for this parse attempt.
    pub profile: PythonVersion,
    /// Ruff's original parse transaction, including every syntax and version diagnostic.
    pub parsed: Parsed<ast::Mod>,
}

/// A typed rejection from the Ruff syntax authority.
#[derive(Debug, Error)]
pub enum ExtractionError {
    /// Source bytes were not valid UTF-8.
    #[error("source is not UTF-8 at {span:?}")]
    InvalidUtf8 {
        /// Exact invalid-byte span.
        span: Span,
        /// Invalid bytes copied from the source.
        bytes: Vec<u8>,
    },
    /// Ruff rejected the source under the selected Python profile.
    #[error("Python parser rejected the source")]
    RejectedSyntax {
        /// Complete Ruff parse rejection and diagnostics.
        rejection: RejectedSyntax,
    },
    /// The source exceeded the representable 32-bit coordinate range.
    #[error("Python source has {actual} bytes, exceeding the 32-bit source-coordinate bound")]
    SourceLength {
        /// Observed source length.
        actual: usize,
        #[source]
        /// Integer conversion failure for the source length.
        source: core::num::TryFromIntError,
    },
    /// Ruff returned a range outside the admitted source bytes.
    #[error("Ruff returned source range {start}..{end} outside {source_length} input bytes")]
    InvalidRange {
        /// Observed range start.
        start: usize,
        /// Observed range end.
        end: usize,
        /// Input source length.
        source_length: usize,
    },
    /// A function header could not be mapped to a body delimiter.
    #[error("Ruff function header {start}..{end} has no Python delimiter")]
    MissingFunctionDelimiter {
        /// Function header start.
        start: usize,
        /// Function header end.
        end: usize,
    },
    /// Ruff returned an expression for a module-mode parse.
    #[error("Ruff returned an expression after a module-mode parse")]
    NonModuleParse {
        /// Complete parse transaction containing the unexpected expression.
        rejection: RejectedSyntax,
    },
}

const MODULE_IDENTITY: &str = "__main__";

/// Extracts syntax-derived facts under the caller-selected Python grammar.
///
/// The profile is required: Ruff must never select a grammar from host defaults.
pub fn extract(source: &[u8], profile: PythonVersion) -> Result<ModuleFacts, ExtractionError> {
    let (text, module_span) = source_text(source)?;
    let parsed = parse_module(text, profile)?;
    let syntax = match parsed.syntax() {
        ast::Mod::Module(module) => module,
        ast::Mod::Expression(_) => {
            return Err(ExtractionError::NonModuleParse {
                rejection: RejectedSyntax { profile, parsed },
            });
        }
    };
    let mut facts = module_facts(syntax, text, module_span)?;
    project(syntax, text, &mut facts)?;
    Ok(facts)
}

/// A declaration class proven directly by Ruff's typed module AST.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuffDeclarationKind {
    /// A Python class declaration.
    Class,
    /// A Python function or async-function declaration.
    Function,
    /// A top-level assignment binding whose finality Ruff does not prove.
    Static,
}

/// One source-backed declaration borrowed from an in-process Ruff parse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuffDeclaration {
    /// Exact source span of the declared identifier.
    pub name: Span,
    /// Closed AST declaration kind.
    pub kind: RuffDeclarationKind,
}

/// A non-escaping, source-backed Ruff module authority.
pub struct RuffModule<'source> {
    module: &'source ast::ModModule,
}

impl RuffModule<'_> {
    /// Streams top-level declarations directly from Ruff AST nodes.
    pub fn declarations(&self) -> impl Iterator<Item = RuffDeclaration> + '_ {
        self.module.body.iter().filter_map(ruff_declaration)
    }
}

/// Runs one non-escaping Ruff parse under the exact selected Python grammar.
///
/// The closure receives only borrowed AST facts. It must lower them before
/// the parser owner is dropped, so no owned syntax DTO crosses this boundary.
///
/// # Errors
///
/// Returns Ruff's complete typed parse terminal without replacing it with a
/// scanner result.
pub fn with_module<Output>(
    source: &[u8],
    profile: PythonVersion,
    consume: impl for<'module> FnOnce(RuffModule<'module>) -> Output,
) -> Result<Output, ExtractionError> {
    let (text, _) = source_text(source)?;
    let parsed = parse_module(text, profile)?;
    match parsed.syntax() {
        ast::Mod::Module(module) => Ok(consume(RuffModule { module })),
        ast::Mod::Expression(_) => Err(ExtractionError::NonModuleParse {
            rejection: RejectedSyntax { profile, parsed },
        }),
    }
}

fn ruff_declaration(statement: &ast::Stmt) -> Option<RuffDeclaration> {
    match statement {
        ast::Stmt::FunctionDef(function) => Some(RuffDeclaration {
            name: span(function.name.range()),
            kind: RuffDeclarationKind::Function,
        }),
        ast::Stmt::ClassDef(class) => Some(RuffDeclaration {
            name: span(class.name.range()),
            kind: RuffDeclarationKind::Class,
        }),
        ast::Stmt::Assign(assign) => assignment_declaration(assign.targets.first()),
        ast::Stmt::AnnAssign(assign) => assignment_declaration(Some(assign.target.as_ref())),
        _ => None,
    }
}

fn assignment_declaration(target: Option<&ast::Expr>) -> Option<RuffDeclaration> {
    let ast::Expr::Name(name) = target? else {
        return None;
    };
    Some(RuffDeclaration {
        name: span(name.range()),
        kind: RuffDeclarationKind::Static,
    })
}

/// Validates source representation and derives its full byte span once.
fn source_text(source: &[u8]) -> Result<(&str, Span), ExtractionError> {
    let module_span = source_span(source, 0, source.len())?;
    std::str::from_utf8(source).map_or_else(
        |error| {
            let start = error.valid_up_to();
            let end = error
                .error_len()
                .map_or(source.len(), |length| start.saturating_add(length))
                .min(source.len());
            Err(ExtractionError::InvalidUtf8 {
                span: source_span(source, start, end)?,
                bytes: source[start..end].to_vec(),
            })
        },
        |text| Ok((text, module_span)),
    )
}

/// Converts byte offsets to canonical coordinates without truncating an input.
fn source_span(source: &[u8], start: usize, end: usize) -> Result<Span, ExtractionError> {
    let source_length = source.len();
    let start = u32::try_from(start).map_err(|source_error| ExtractionError::SourceLength {
        actual: source_length,
        source: source_error,
    })?;
    let end = u32::try_from(end).map_err(|source_error| ExtractionError::SourceLength {
        actual: source_length,
        source: source_error,
    })?;
    Ok(Span { start, end })
}

/// Seeds the module declaration before its nested syntax is projected.
fn module_facts(
    syntax: &ast::ModModule,
    text: &str,
    module_span: Span,
) -> Result<ModuleFacts, ExtractionError> {
    let module_doc = docstring(&syntax.body, text)?;
    Ok(ModuleFacts {
        identity: MODULE_IDENTITY.to_owned(),
        span: module_span,
        declarations: vec![DeclarationFact {
            name: MODULE_IDENTITY.to_owned(),
            name_span: module_span,
            kind: DeclarationKind::Module,
            span: module_span,
            bases: Vec::new(),
            class_form: None,
            decorators: Vec::new(),
            decorator_spans: Vec::new(),
            is_async: false,
            receiver: ReceiverKind::Plain,
            parameters: Vec::new(),
            value_source: None,
            value_span: None,
            type_parameters: Vec::new(),
            total: None,
            docstring: module_doc.clone(),
        }],
        occurrences: Vec::new(),
        docstring: module_doc,
        annotations: Vec::new(),
    })
}

/// Walks Ruff syntax once and appends all syntax-derived facts.
fn project(
    syntax: &ast::ModModule,
    text: &str,
    facts: &mut ModuleFacts,
) -> Result<(), ExtractionError> {
    let mut names = FunctionNames::default();
    for statement in &syntax.body {
        names.visit_stmt(statement);
    }
    let error = {
        let mut projection = Projection {
            text,
            names: &names.0,
            facts,
            owner: MODULE_IDENTITY.to_owned(),
            class_depth: 0,
            function_depth: 0,
            decorator_ranges: Vec::new(),
            decorator_owner: None,
            error: None,
        };
        for statement in &syntax.body {
            projection.visit_stmt(statement);
        }
        projection.error.take()
    };
    error.map_or(Ok(()), Err)
}

/// Maps the canonical profile to Ruff's exact grammar switch.
const fn ruff_version(profile: PythonVersion) -> ast::PythonVersion {
    match profile {
        PythonVersion::Python310 => ast::PythonVersion::PY310,
        PythonVersion::Python311 => ast::PythonVersion::PY311,
        PythonVersion::Python312 => ast::PythonVersion::PY312,
        PythonVersion::Python313 => ast::PythonVersion::PY313,
        PythonVersion::Python314 => ast::PythonVersion::PY314,
    }
}

/// Parses one complete module and moves Ruff's own failed transaction into the error terminal.
fn parse_module(text: &str, profile: PythonVersion) -> Result<Parsed<ast::Mod>, ExtractionError> {
    let parsed = parse_unchecked(
        text,
        ParseOptions::from(Mode::Module).with_target_version(ruff_version(profile)),
    );
    if parsed.has_syntax_errors() {
        return Err(ExtractionError::RejectedSyntax {
            rejection: RejectedSyntax { profile, parsed },
        });
    }
    Ok(parsed)
}

fn span(range: ruff_text_size::TextRange) -> Span {
    Span {
        start: range.start().to_u32(),
        end: range.end().to_u32(),
    }
}
/// Borrows an exact source range or returns a typed projection fault.
fn source_slice(text: &str, range: ruff_text_size::TextRange) -> Result<&str, ExtractionError> {
    let start = range.start().to_usize();
    let end = range.end().to_usize();
    text.get(start..end).ok_or(ExtractionError::InvalidRange {
        start,
        end,
        source_length: text.len(),
    })
}

/// Projects only syntax proven by Ruff's typed expression tree.
fn annotation(expr: &ast::Expr) -> Annotation {
    annotation_at_depth(expr, 0)
}

/// Quoted annotations may quote further annotations; the guard keeps the
/// projection total without trusting unbounded recursion.
const MAX_STRING_ANNOTATION_DEPTH: usize = 8;

/// Projects an annotation expression, resolving quoted annotations through
/// Ruff's own expression authority up to the depth guard.
fn annotation_at_depth(expr: &ast::Expr, depth: usize) -> Annotation {
    match expr {
        ast::Expr::Name(name) => Annotation::Name {
            name: name.id.as_str().to_owned(),
            span: Some(span(expr.range())),
        },
        ast::Expr::Attribute(_) => qualified_name(expr).map_or_else(
            || unsupported_annotation(expr),
            |name| Annotation::Name {
                name,
                span: Some(span(expr.range())),
            },
        ),
        ast::Expr::StringLiteral(literal) => string_annotation(literal, expr, depth),
        ast::Expr::NoneLiteral(_) => Annotation::None,
        ast::Expr::BinOp(binary) if binary.op == ast::Operator::BitOr => Annotation::Union(vec![
            annotation_at_depth(&binary.left, depth),
            annotation_at_depth(&binary.right, depth),
        ]),
        ast::Expr::List(list) => Annotation::List(
            list.elts
                .iter()
                .map(|element| annotation_at_depth(element, depth))
                .collect(),
        ),
        ast::Expr::Subscript(subscript) => application(subscript, depth),
        _ => unsupported_annotation(expr),
    }
}

/// Resolves one quoted annotation by parsing its exact value with Ruff.
///
/// A value Ruff can parse as an expression is projected like any written
/// annotation; a value Ruff rejects stays an explicit unsupported-syntax
/// fact carrying the literal's exact span, so every consumer can borrow the
/// unresolved spelling from the source; a value beyond the depth guard
/// reports the truncation instead of guessing.
fn string_annotation(
    literal: &ast::ExprStringLiteral,
    full: &ast::Expr,
    depth: usize,
) -> Annotation {
    let value = literal.value.to_str();
    if depth >= MAX_STRING_ANNOTATION_DEPTH {
        return Annotation::Unknown(TypeReason::TruncatedAtDepthLimit);
    }
    let parsed = parse_unchecked(value, ParseOptions::from(Mode::Expression));
    if parsed.has_syntax_errors() {
        return unsupported_annotation(full);
    }
    match parsed.syntax() {
        ast::Mod::Expression(expression) => {
            let mut annotation = annotation_at_depth(&expression.body, depth + 1);
            annotation.clear_source_spans();
            annotation
        }
        ast::Mod::Module(_) => unsupported_annotation(full),
    }
}

impl Annotation {
    /// Prevents coordinates parsed from a quoted string value from being
    /// mistaken for offsets into the enclosing source module.
    fn clear_source_spans(&mut self) {
        match self {
            Self::Name { span, .. } => *span = None,
            Self::Generic { base, args } => {
                base.clear_source_spans();
                for argument in args {
                    argument.clear_source_spans();
                }
            }
            Self::List(elements) | Self::Union(elements) => {
                for element in elements {
                    element.clear_source_spans();
                }
            }
            Self::StringLiteral(_) | Self::Literal(_) | Self::None | Self::Unknown(_) => {}
        }
    }
}

/// Projects a typed subscription as either `Literal[...]` or a generic application.
fn application(subscript: &ast::ExprSubscript, depth: usize) -> Annotation {
    if terminal_name(&subscript.value) == Some("Literal") {
        Annotation::Literal(literal_arguments(&subscript.slice))
    } else {
        Annotation::Generic {
            base: Box::new(annotation_at_depth(&subscript.value, depth)),
            args: annotation_arguments(&subscript.slice, depth),
        }
    }
}

/// Keeps tuple subscription arguments distinct without re-tokenizing source text.
fn annotation_arguments(slice: &ast::Expr, depth: usize) -> Vec<Annotation> {
    match slice {
        ast::Expr::Tuple(tuple) => tuple
            .elts
            .iter()
            .map(|expression| annotation_at_depth(expression, depth))
            .collect(),
        expression => vec![annotation_at_depth(expression, depth)],
    }
}

/// Converts literal AST expressions to typed literal facts without string parsing.
fn literal_arguments(slice: &ast::Expr) -> Vec<LiteralValue> {
    match slice {
        ast::Expr::Tuple(tuple) => tuple.elts.iter().map(literal_value).collect(),
        expression => vec![literal_value(expression)],
    }
}

/// Converts only Ruff literal nodes to literal facts; every other AST class stays explicit.
fn literal_value(expr: &ast::Expr) -> LiteralValue {
    match expr {
        ast::Expr::StringLiteral(literal) => {
            LiteralValue::String(literal.value.to_str().to_owned())
        }
        ast::Expr::NumberLiteral(number) => match &number.value {
            ast::Number::Int(value) => LiteralValue::Integer(value.to_string()),
            ast::Number::Float(value) => LiteralValue::Float {
                ieee_bits: value.to_bits(),
            },
            ast::Number::Complex { real, imag } => LiteralValue::Complex {
                real_ieee_bits: real.to_bits(),
                imaginary_ieee_bits: imag.to_bits(),
            },
        },
        ast::Expr::BooleanLiteral(boolean) => LiteralValue::Boolean(boolean.value),
        ast::Expr::NoneLiteral(_) => LiteralValue::None,
        ast::Expr::EllipsisLiteral(_) => LiteralValue::Ellipsis,
        _ => LiteralValue::Unsupported(annotation_syntax_kind(expr)),
    }
}

/// Retains an unsupported expression category and its exact AST byte range.
fn unsupported_annotation(expr: &ast::Expr) -> Annotation {
    Annotation::Unknown(TypeReason::UnsupportedSyntax {
        kind: annotation_syntax_kind(expr),
        span: span(expr.range()),
    })
}

/// Maps every unprojected Ruff expression variant to a closed unsupported fact.
fn annotation_syntax_kind(expr: &ast::Expr) -> AnnotationSyntaxKind {
    match expr {
        ast::Expr::BoolOp(_) => AnnotationSyntaxKind::BooleanOperator,
        ast::Expr::Named(_) => AnnotationSyntaxKind::NamedExpression,
        ast::Expr::BinOp(_) => AnnotationSyntaxKind::BinaryOperator,
        ast::Expr::UnaryOp(_) => AnnotationSyntaxKind::UnaryOperator,
        ast::Expr::Lambda(_) => AnnotationSyntaxKind::Lambda,
        ast::Expr::If(_) => AnnotationSyntaxKind::Conditional,
        ast::Expr::Dict(_) => AnnotationSyntaxKind::Dictionary,
        ast::Expr::Set(_) => AnnotationSyntaxKind::Set,
        ast::Expr::ListComp(_) => AnnotationSyntaxKind::ListComprehension,
        ast::Expr::SetComp(_) => AnnotationSyntaxKind::SetComprehension,
        ast::Expr::DictComp(_) => AnnotationSyntaxKind::DictionaryComprehension,
        ast::Expr::Generator(_) => AnnotationSyntaxKind::Generator,
        ast::Expr::Await(_) => AnnotationSyntaxKind::Await,
        ast::Expr::Yield(_) => AnnotationSyntaxKind::Yield,
        ast::Expr::YieldFrom(_) => AnnotationSyntaxKind::YieldFrom,
        ast::Expr::Compare(_) => AnnotationSyntaxKind::Comparison,
        ast::Expr::Call(_) => AnnotationSyntaxKind::Call,
        ast::Expr::FString(_) => AnnotationSyntaxKind::FormattedString,
        ast::Expr::TString(_) => AnnotationSyntaxKind::TemplateString,
        ast::Expr::StringLiteral(_) => AnnotationSyntaxKind::StringLiteral,
        ast::Expr::BytesLiteral(_) => AnnotationSyntaxKind::Bytes,
        ast::Expr::NumberLiteral(_) => AnnotationSyntaxKind::Number,
        ast::Expr::BooleanLiteral(_) => AnnotationSyntaxKind::Boolean,
        ast::Expr::NoneLiteral(_) => AnnotationSyntaxKind::NoneLiteral,
        ast::Expr::EllipsisLiteral(_) => AnnotationSyntaxKind::Ellipsis,
        ast::Expr::Attribute(_) => AnnotationSyntaxKind::Attribute,
        ast::Expr::Subscript(_) => AnnotationSyntaxKind::Subscript,
        ast::Expr::Starred(_) => AnnotationSyntaxKind::Starred,
        ast::Expr::Name(_) => AnnotationSyntaxKind::Name,
        ast::Expr::List(_) => AnnotationSyntaxKind::List,
        ast::Expr::Tuple(_) => AnnotationSyntaxKind::Tuple,
        ast::Expr::Slice(_) => AnnotationSyntaxKind::Slice,
        ast::Expr::IpyEscapeCommand(_) => AnnotationSyntaxKind::IpythonEscape,
    }
}

/// Builds a qualified name only from Name and Attribute AST nodes.
fn qualified_name(expr: &ast::Expr) -> Option<String> {
    match expr {
        ast::Expr::Name(name) => Some(name.id.as_str().to_owned()),
        ast::Expr::Attribute(attribute) => {
            let mut prefix = qualified_name(&attribute.value)?;
            prefix.push('.');
            prefix.push_str(attribute.attr.as_str());
            Some(prefix)
        }
        _ => None,
    }
}

/// Returns the terminal identifier of a decorator or qualified type expression.
fn terminal_name(expr: &ast::Expr) -> Option<&str> {
    match expr {
        ast::Expr::Name(name) => Some(name.id.as_str()),
        ast::Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
        ast::Expr::Call(call) => terminal_name(&call.func),
        _ => None,
    }
}

fn docstring(body: &[ast::Stmt], text: &str) -> Result<Option<DocstringFact>, ExtractionError> {
    let Some(first) = body.first() else {
        return Ok(None);
    };
    match first {
        ast::Stmt::Expr(statement) => match statement.value.as_ref() {
            ast::Expr::StringLiteral(literal) => Ok(Some(DocstringFact {
                raw: source_slice(text, literal.range)?.to_owned(),
                span: span(literal.range),
            })),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

#[derive(Default)]
struct FunctionNames(Vec<String>);
impl<'a> Visitor<'a> for FunctionNames {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::FunctionDef(function) => self.0.push(function.name.as_str().to_owned()),
            ast::Stmt::ClassDef(class) => self.0.push(class.name.as_str().to_owned()),
            ast::Stmt::Import(import) => {
                for alias in &import.names {
                    self.0.push(alias_binding(alias));
                }
            }
            ast::Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    self.0.push(alias_binding(alias));
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, statement);
    }
}

fn alias_binding(alias: &ast::Alias) -> String {
    alias.asname.as_ref().map_or_else(
        || last_segment(alias.name.as_str()).to_owned(),
        |name| name.as_str().to_owned(),
    )
}

fn last_segment(spelling: &str) -> &str {
    match spelling.rsplit_once('.') {
        Some((_, segment)) => segment,
        None => spelling,
    }
}

/// The proven decorator spellings of one declaration, each paired with
/// its exact source span (including the leading `@`).
type Decorated = (Vec<String>, Vec<Span>);

/// The exact source spans of one PEP 695 type-parameter list's written
/// identifiers, in declaration order (`[T, U]` for `[T, U]`).
fn type_parameter_spans(type_params: Option<&ast::TypeParams>) -> Vec<Span> {
    type_params
        .map(|parameters| {
            parameters
                .type_params
                .iter()
                .map(|parameter| span(parameter.name().range()))
                .collect()
        })
        .unwrap_or_default()
}

struct Projection<'a> {
    text: &'a str,
    names: &'a [String],
    facts: &'a mut ModuleFacts,
    owner: String,
    class_depth: usize,
    function_depth: usize,
    decorator_ranges: Vec<ruff_text_size::TextRange>,
    decorator_owner: Option<String>,
    error: Option<ExtractionError>,
}
impl<'a> Projection<'a> {
    fn add_declaration(&mut self, declaration: DeclarationFact) {
        self.facts.declarations.push(declaration);
    }

    /// Stores the first projection fault; later traversal cannot replace its evidence.
    fn reject(&mut self, error: ExtractionError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// Copies an AST-selected source range only after a checked bounds proof.
    fn source_owned(&mut self, range: ruff_text_size::TextRange) -> Option<String> {
        if self.error.is_some() {
            return None;
        }
        match source_slice(self.text, range) {
            Ok(value) => Some(value.to_owned()),
            Err(error) => {
                self.reject(error);
                None
            }
        }
    }

    /// Preserves each decorator spelling and its exact source span after
    /// validating the parser-owned range.
    fn decorators(&mut self, decorators: &[ast::Decorator]) -> Option<Decorated> {
        let mut values = Vec::with_capacity(decorators.len());
        let mut spans = Vec::with_capacity(decorators.len());
        for decorator in decorators {
            let source = self.source_owned(decorator.range)?;
            values.push(source.trim_start_matches('@').to_owned());
            spans.push(span(decorator.range));
        }
        Some((values, spans))
    }

    /// Preserves an optional expression spelling without conflating absence and projection failure.
    fn optional_source(&mut self, expression: Option<&ast::Expr>) -> Option<Option<String>> {
        match expression {
            Some(expression) => self.source_owned(expression.range()).map(Some),
            None => Some(None),
        }
    }

    /// Preserves a docstring's complete source spelling or retains the bounds fault.
    fn docstring(&mut self, body: &[ast::Stmt]) -> Option<Option<DocstringFact>> {
        match docstring(body, self.text) {
            Ok(fact) => Some(fact),
            Err(error) => {
                self.reject(error);
                None
            }
        }
    }

    fn function_header_span(&mut self, function: &ast::StmtFunctionDef) -> Option<Span> {
        let start = function.range.start().to_usize();
        let body_start = function
            .body
            .first()
            .map_or(function.range.end().to_usize(), |statement| {
                statement.range().start().to_usize()
            });
        let header_end = body_start.min(self.text.len());
        let header = match self.text.get(start..header_end) {
            Some(header) => header,
            None => {
                self.reject(ExtractionError::InvalidRange {
                    start,
                    end: header_end,
                    source_length: self.text.len(),
                });
                return None;
            }
        };
        let end = match header.rfind(':') {
            Some(offset) => start + offset + 1,
            None => {
                self.reject(ExtractionError::MissingFunctionDelimiter {
                    start,
                    end: header_end,
                });
                return None;
            }
        };
        let start = start.min(self.text.len());
        let end = end.min(self.text.len()).max(start);
        let start_coordinate = match u32::try_from(start) {
            Ok(coordinate) => coordinate,
            Err(_) => {
                self.reject(ExtractionError::InvalidRange {
                    start,
                    end,
                    source_length: self.text.len(),
                });
                return None;
            }
        };
        let end_coordinate = match u32::try_from(end) {
            Ok(coordinate) => coordinate,
            Err(_) => {
                self.reject(ExtractionError::InvalidRange {
                    start,
                    end,
                    source_length: self.text.len(),
                });
                return None;
            }
        };
        Some(Span {
            start: start_coordinate,
            end: end_coordinate,
        })
    }
    fn parameter(
        &mut self,
        owner: &str,
        item: &ast::Parameter,
        default: Option<&ast::Expr>,
        kind: ParameterKind,
    ) -> Option<ParameterFact> {
        let annotation_span = item
            .annotation
            .as_deref()
            .map(|expression| span(expression.range()));
        let annotation_value = item.annotation.as_deref().map_or(
            Annotation::Unknown(TypeReason::Unannotated {
                position: AnnotationPosition::Parameter,
            }),
            annotation,
        );
        let fact = ParameterFact {
            name: item.name.as_str().to_owned(),
            name_span: span(item.name.range()),
            kind,
            has_default: default.is_some(),
            default_source: self.optional_source(default)?,
            annotation: annotation_value.clone(),
            annotation_span,
            span: span(item.range),
        };
        self.facts.annotations.push(AnnotationFact {
            owner: owner.to_owned(),
            position: AnnotationPosition::Parameter,
            annotation: annotation_value,
            span: item
                .annotation
                .as_deref()
                .map_or(span(item.range), |x| span(x.range())),
        });
        Some(fact)
    }
    fn params(&mut self, function: &ast::StmtFunctionDef) -> Option<Vec<ParameterFact>> {
        let mut result = Vec::new();
        for item in &function.parameters.posonlyargs {
            result.push(self.parameter(
                function.name.as_str(),
                &item.parameter,
                item.default.as_deref(),
                ParameterKind::PositionalOnly,
            )?);
        }
        for item in &function.parameters.args {
            result.push(self.parameter(
                function.name.as_str(),
                &item.parameter,
                item.default.as_deref(),
                ParameterKind::PositionalOrKeyword,
            )?);
        }
        if let Some(item) = function.parameters.vararg.as_deref() {
            result.push(self.parameter(
                function.name.as_str(),
                item,
                None,
                ParameterKind::VarArgs,
            )?);
        }
        for item in &function.parameters.kwonlyargs {
            result.push(self.parameter(
                function.name.as_str(),
                &item.parameter,
                item.default.as_deref(),
                ParameterKind::KeywordOnly,
            )?);
        }
        if let Some(item) = function.parameters.kwarg.as_deref() {
            result.push(self.parameter(
                function.name.as_str(),
                item,
                None,
                ParameterKind::KwArgs,
            )?);
        }
        if let Some(returns) = function.returns.as_deref() {
            self.facts.annotations.push(AnnotationFact {
                owner: function.name.as_str().to_owned(),
                position: AnnotationPosition::Return,
                annotation: annotation(returns),
                span: span(returns.range()),
            });
        }
        Some(result)
    }
    fn class_form(&self, class: &ast::StmtClassDef) -> (Vec<Annotation>, ClassForm, Option<bool>) {
        let (mut bases, mut form, mut total) = (Vec::new(), ClassForm::Plain, None);
        if let Some(arguments) = class.arguments.as_deref() {
            for base in &arguments.args {
                if terminal_name(base) == Some("Protocol") {
                    form = ClassForm::Protocol;
                } else if terminal_name(base) == Some("TypedDict") {
                    form = ClassForm::TypedDict;
                } else if terminal_name(base) == Some("Enum") {
                    form = ClassForm::Enum;
                }
                bases.push(annotation(base));
            }
            for keyword in &arguments.keywords {
                if keyword.arg.as_ref().map(|name| name.as_str()) == Some("total")
                    && let ast::Expr::BooleanLiteral(boolean) = &keyword.value
                {
                    total = Some(boolean.value);
                }
            }
        }
        if class
            .decorator_list
            .iter()
            .any(|decorator| terminal_name(&decorator.expression) == Some("dataclass"))
        {
            form = ClassForm::Dataclass;
        }
        (bases, form, total)
    }
}
impl<'a> Visitor<'a> for Projection<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::FunctionDef(function) => {
                let old = self.owner.clone();
                let old_decorator_ranges = std::mem::take(&mut self.decorator_ranges);
                let old_decorator_owner = self.decorator_owner.take();
                let name = function.name.as_str().to_owned();
                let Some(parameters) = self.params(function) else {
                    return;
                };
                let Some((decorators, decorator_spans)) = self.decorators(&function.decorator_list)
                else {
                    return;
                };
                let receiver =
                    if function.decorator_list.iter().any(|decorator| {
                        terminal_name(&decorator.expression) == Some("staticmethod")
                    }) {
                        ReceiverKind::StaticMethod
                    } else if function.decorator_list.iter().any(|decorator| {
                        terminal_name(&decorator.expression) == Some("classmethod")
                    }) {
                        ReceiverKind::ClassMethod
                    } else if function
                        .decorator_list
                        .iter()
                        .any(|decorator| terminal_name(&decorator.expression) == Some("property"))
                    {
                        ReceiverKind::Property
                    } else {
                        ReceiverKind::Plain
                    };
                let Some(doc) = self.docstring(&function.body) else {
                    return;
                };
                let Some(declaration_span) = self.function_header_span(function) else {
                    return;
                };
                self.add_declaration(DeclarationFact {
                    name: name.clone(),
                    name_span: span(function.name.range()),
                    kind: DeclarationKind::Function,
                    span: declaration_span,
                    bases: Vec::new(),
                    class_form: None,
                    decorators,
                    decorator_spans,
                    is_async: function.is_async,
                    receiver,
                    parameters,
                    value_source: None,
                    value_span: None,
                    type_parameters: type_parameter_spans(function.type_params.as_deref()),
                    total: None,
                    docstring: doc,
                });
                self.decorator_ranges = function.decorator_list.iter().map(|d| d.range).collect();
                self.decorator_owner = Some(old.clone());
                self.owner = name;
                self.function_depth += 1;
                visitor::walk_stmt(self, statement);
                self.function_depth -= 1;
                self.owner = old;
                self.decorator_ranges = old_decorator_ranges;
                self.decorator_owner = old_decorator_owner;
                return;
            }
            ast::Stmt::ClassDef(class) => {
                let old = self.owner.clone();
                let old_decorator_ranges = std::mem::take(&mut self.decorator_ranges);
                let old_decorator_owner = self.decorator_owner.take();
                let name = class.name.as_str().to_owned();
                let (bases, form, total) = self.class_form(class);
                let Some((decorators, decorator_spans)) = self.decorators(&class.decorator_list)
                else {
                    return;
                };
                let Some(docstring) = self.docstring(&class.body) else {
                    return;
                };
                self.add_declaration(DeclarationFact {
                    name: name.clone(),
                    name_span: span(class.name.range()),
                    kind: DeclarationKind::Class,
                    span: span(class.range),
                    bases,
                    class_form: Some(form),
                    decorators,
                    decorator_spans,
                    is_async: false,
                    receiver: ReceiverKind::Plain,
                    parameters: Vec::new(),
                    value_source: None,
                    value_span: None,
                    type_parameters: type_parameter_spans(class.type_params.as_deref()),
                    total,
                    docstring,
                });
                self.decorator_ranges = class.decorator_list.iter().map(|d| d.range).collect();
                self.decorator_owner = Some(old.clone());
                self.owner = name;
                self.class_depth += 1;
                visitor::walk_stmt(self, statement);
                self.class_depth -= 1;
                self.owner = old;
                self.decorator_ranges = old_decorator_ranges;
                self.decorator_owner = old_decorator_owner;
                return;
            }
            ast::Stmt::Import(import) if self.function_depth == 0 && self.class_depth == 0 => {
                for alias in &import.names {
                    self.add_alias(alias, statement);
                }
            }
            ast::Stmt::ImportFrom(import) if self.function_depth == 0 && self.class_depth == 0 => {
                for alias in &import.names {
                    self.add_alias(alias, statement);
                }
            }
            ast::Stmt::TypeAlias(alias) if self.function_depth == 0 && self.class_depth == 0 => {
                self.add_type_alias(alias, statement);
            }
            ast::Stmt::Assign(assign) if self.class_depth > 0 && self.function_depth == 0 => {
                if let Some(target) = assign.targets.first() {
                    self.field_from_target(target, Some(assign.value.as_ref()), None, statement);
                }
            }
            ast::Stmt::Assign(assign) if self.class_depth == 0 && self.function_depth == 0 => {
                if let Some(target) = assign.targets.first() {
                    self.constant_from_target(target, Some(assign.value.as_ref()), None, statement);
                }
            }
            ast::Stmt::AnnAssign(assign) if self.class_depth > 0 && self.function_depth == 0 => {
                self.field_from_target(
                    &assign.target,
                    assign.value.as_deref(),
                    Some(assign.annotation.as_ref()),
                    statement,
                )
            }
            ast::Stmt::AnnAssign(assign) if self.class_depth == 0 && self.function_depth == 0 => {
                self.constant_from_target(
                    &assign.target,
                    assign.value.as_deref(),
                    Some(assign.annotation.as_ref()),
                    statement,
                )
            }
            _ => {}
        }
        visitor::walk_stmt(self, statement);
    }
    fn visit_expr(&mut self, expr: &'a ast::Expr) {
        if let ast::Expr::Call(call) = expr {
            let (target, kind) = match call.func.as_ref() {
                ast::Expr::Name(name) => (name.id.as_str(), OccurrenceKind::FunctionCall),
                ast::Expr::Attribute(attribute) => {
                    (attribute.attr.as_str(), OccurrenceKind::MethodCall)
                }
                _ => ("", OccurrenceKind::FunctionCall),
            };
            if !target.is_empty() && self.names.iter().any(|name| name == target) {
                let owner = if self.decorator_ranges.iter().any(|range| {
                    range.start() <= expr.range().start() && range.end() >= expr.range().end()
                }) {
                    match self.decorator_owner.as_deref() {
                        Some(owner) => owner,
                        None => &self.owner,
                    }
                } else {
                    &self.owner
                };
                self.facts.occurrences.push(OccurrenceFact {
                    owner: owner.to_owned(),
                    target: target.to_owned(),
                    kind,
                    confidence: Confidence::Index,
                    span: span(call.func.range()),
                });
            }
        }
        visitor::walk_expr(self, expr);
    }
}

impl Projection<'_> {
    /// The exact source span of an alias's binding identifier: the `as`
    /// name when written, otherwise the final dotted segment of the
    /// imported path.
    fn alias_binding_span(&mut self, alias: &ast::Alias) -> Option<Span> {
        if let Some(asname) = alias.asname.as_ref() {
            return Some(span(asname.range()));
        }
        let range = alias.name.range();
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        let bytes = self.text.as_bytes();
        let Some(spelling) = bytes.get(start..end) else {
            self.reject(ExtractionError::InvalidRange {
                start,
                end,
                source_length: bytes.len(),
            });
            return None;
        };
        let tail = spelling
            .iter()
            .rposition(|byte| *byte == b'.')
            .map_or(start, |dot| start + dot + 1);
        match source_span(bytes, tail, end) {
            Ok(binding) => Some(binding),
            Err(error) => {
                self.reject(error);
                None
            }
        }
    }

    fn add_alias(&mut self, alias: &ast::Alias, statement: &ast::Stmt) {
        let binding = alias_binding(alias);
        let Some(binding_span) = self.alias_binding_span(alias) else {
            return;
        };
        self.add_declaration(DeclarationFact {
            name: binding,
            name_span: binding_span,
            kind: DeclarationKind::Alias,
            span: span(statement.range()),
            bases: Vec::new(),
            class_form: None,
            decorators: Vec::new(),
            decorator_spans: Vec::new(),
            is_async: false,
            receiver: ReceiverKind::Plain,
            parameters: Vec::new(),
            value_source: Some(alias.name.as_str().to_owned()),
            value_span: Some(span(alias.name.range())),
            type_parameters: Vec::new(),
            total: None,
            docstring: None,
        });
    }

    /// Records one PEP 695 `type` alias statement (`type Vector[T] =
    /// list[T]`): an alias declaration whose written value is projected
    /// exactly like any annotation, keyed by the new `AliasValue` position.
    fn add_type_alias(&mut self, alias: &ast::StmtTypeAlias, statement: &ast::Stmt) {
        let ast::Expr::Name(name) = alias.name.as_ref() else {
            return;
        };
        let value_annotation = annotation(&alias.value);
        let value_span = span(alias.value.range());
        let Some(value_source) = self.source_owned(alias.value.range()) else {
            return;
        };
        self.add_declaration(DeclarationFact {
            name: name.id.as_str().to_owned(),
            name_span: span(name.range()),
            kind: DeclarationKind::Alias,
            span: span(statement.range()),
            bases: Vec::new(),
            class_form: None,
            decorators: Vec::new(),
            decorator_spans: Vec::new(),
            is_async: false,
            receiver: ReceiverKind::Plain,
            parameters: Vec::new(),
            value_source: Some(value_source),
            value_span: None,
            type_parameters: type_parameter_spans(alias.type_params.as_deref()),
            total: None,
            docstring: None,
        });
        self.facts.annotations.push(AnnotationFact {
            owner: name.id.as_str().to_owned(),
            position: AnnotationPosition::AliasValue,
            annotation: value_annotation,
            span: value_span,
        });
    }
    fn field_from_target(
        &mut self,
        target: &ast::Expr,
        value: Option<&ast::Expr>,
        annotation_expr: Option<&ast::Expr>,
        statement: &ast::Stmt,
    ) {
        if let ast::Expr::Name(name) = target {
            let Some(value_source) = self.optional_source(value) else {
                return;
            };
            self.add_declaration(DeclarationFact {
                name: name.id.as_str().to_owned(),
                name_span: span(name.range()),
                kind: DeclarationKind::Field,
                span: span(statement.range()),
                bases: Vec::new(),
                class_form: None,
                decorators: Vec::new(),
                decorator_spans: Vec::new(),
                is_async: false,
                receiver: ReceiverKind::Plain,
                parameters: Vec::new(),
                value_source,
                value_span: None,
                type_parameters: Vec::new(),
                total: None,
                docstring: None,
            });
            if let Some(annotation_expr) = annotation_expr {
                self.facts.annotations.push(AnnotationFact {
                    owner: name.id.as_str().to_owned(),
                    position: AnnotationPosition::Field,
                    annotation: annotation(annotation_expr),
                    span: span(annotation_expr.range()),
                });
            }
        }
    }

    fn constant_from_target(
        &mut self,
        target: &ast::Expr,
        value: Option<&ast::Expr>,
        annotation_expr: Option<&ast::Expr>,
        statement: &ast::Stmt,
    ) {
        if let ast::Expr::Name(name) = target {
            let Some(value_source) = self.optional_source(value) else {
                return;
            };
            self.add_declaration(DeclarationFact {
                name: name.id.as_str().to_owned(),
                name_span: span(name.range()),
                kind: DeclarationKind::Constant,
                span: span(statement.range()),
                bases: Vec::new(),
                class_form: None,
                decorators: Vec::new(),
                decorator_spans: Vec::new(),
                is_async: false,
                receiver: ReceiverKind::Plain,
                parameters: Vec::new(),
                value_source,
                value_span: None,
                type_parameters: Vec::new(),
                total: None,
                docstring: None,
            });
            if let Some(annotation_expr) = annotation_expr {
                self.facts.annotations.push(AnnotationFact {
                    owner: name.id.as_str().to_owned(),
                    position: AnnotationPosition::Field,
                    annotation: annotation(annotation_expr),
                    span: span(annotation_expr.range()),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ruff_text_size::{TextRange, TextSize};

    use backend_compile::PythonVersion;

    use super::{ExtractionError, ModuleFacts, Projection, RuffDeclarationKind, Span, with_module};

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("expected an invalid AST source range")]
        ExpectedInvalidRange,
        #[error("Ruff authority rejected the valid direct-declaration fixture: {0:?}")]
        Authority(ExtractionError),
        #[error("Ruff direct declaration count differed: expected {expected}, observed {observed}")]
        DeclarationCount { expected: usize, observed: usize },
        #[error("Ruff direct declaration at index {index} had unexpected kind")]
        DeclarationKind { index: usize },
        #[error("Ruff direct declaration at index {index} had unexpected name span")]
        DeclarationSpan { index: usize },
    }

    #[test]
    fn source_range_fault_is_typed_without_constructing_a_parser_ast() -> Result<(), TestError> {
        let range = TextRange::new(TextSize::new(0), TextSize::new(2));
        let mut facts = ModuleFacts {
            identity: String::new(),
            span: Span { start: 0, end: 1 },
            declarations: Vec::new(),
            occurrences: Vec::new(),
            docstring: None,
            annotations: Vec::new(),
        };
        let mut projection = Projection {
            text: "x",
            names: &[],
            facts: &mut facts,
            owner: String::new(),
            class_depth: 0,
            function_depth: 0,
            decorator_ranges: Vec::new(),
            decorator_owner: None,
            error: None,
        };
        assert!(projection.source_owned(range).is_none());
        match projection.error.take() {
            Some(ExtractionError::InvalidRange {
                start: 0,
                end: 2,
                source_length: 1,
            }) => Ok(()),
            _ => Err(TestError::ExpectedInvalidRange),
        }
    }

    #[test]
    fn direct_declarations_exclude_docs_and_nested_bindings_with_exact_spans()
    -> Result<(), TestError> {
        let source = b"\"module docs\"\nvalue = 1\ntyped: int = 2\nasync def fetch() -> int:\n    return typed\nclass Shell:\n    def nested(self) -> None:\n        pass\n";
        let declarations = with_module(source, PythonVersion::Python313, |module| {
            module.declarations().collect::<Vec<_>>()
        })
        .map_err(TestError::Authority)?;
        let expected = [
            (RuffDeclarationKind::Static, b"value".as_slice()),
            (RuffDeclarationKind::Static, b"typed".as_slice()),
            (RuffDeclarationKind::Function, b"fetch".as_slice()),
            (RuffDeclarationKind::Class, b"Shell".as_slice()),
        ];
        if declarations.len() != expected.len() {
            return Err(TestError::DeclarationCount {
                expected: expected.len(),
                observed: declarations.len(),
            });
        }
        for (index, (declaration, (kind, name))) in declarations.iter().zip(expected).enumerate() {
            if declaration.kind != kind {
                return Err(TestError::DeclarationKind { index });
            }
            let start = match usize::try_from(declaration.name.start) {
                Ok(start) => start,
                Err(_) => return Err(TestError::DeclarationSpan { index }),
            };
            let end = match usize::try_from(declaration.name.end) {
                Ok(end) => end,
                Err(_) => return Err(TestError::DeclarationSpan { index }),
            };
            if source.get(start..end) != Some(name) {
                return Err(TestError::DeclarationSpan { index });
            }
        }
        Ok(())
    }
}
