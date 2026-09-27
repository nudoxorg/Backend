//! Projects source-preserving Python syntax facts directly from Ruff's AST.
//! Retains unresolved syntax explicitly instead of claiming type resolution.
//! Keeps Python syntax authority independent of compiler IR transport.
//!
//! The crate exposes two peer authorities: the Ruff syntax extractor in this
//! module, and the bounded pyrefly type-authority transaction in [`checker`].
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.

pub mod checker;

pub use self::checker::{
    CheckerError, CheckerReport, ImportResolution, Inference, InferenceSite, InferredType, Pyrefly,
    PyreflyExecutableError, SymbolOutcome, SymbolResolution,
};

use std::collections::HashSet;

use backend_semantic::vocabulary::PythonVersion;
use ruff_python_ast::{
    self as ast,
    visitor::{self, Visitor},
};
use ruff_python_parser::{Mode, ParseOptions, Parsed, parse_unchecked};
use ruff_text_size::Ranged;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeReason {
    /// The position carried no annotation at all.
    Unannotated { position: AnnotationPosition },
    /// Ruff proved the expression class but this extractor has no projection for it.
    UnsupportedSyntax {
        kind: AnnotationSyntaxKind,
        span: Span,
    },
    /// The quoted-annotation recursion guard fired before resolution finished.
    TruncatedAtDepthLimit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationPosition {
    Parameter,
    Return,
    Field,
    /// A function-body `AnnAssign` whose target is a simple name (`obj: Child =
    /// ...` and `obj: Child` with no value). Not a class-body field and not a
    /// module constant.
    Local,
    /// A function-body `AnnAssign` on `self.name` or `cls.name` inside a
    /// class-body method at `function_depth` 1. Not a class-body field.
    Instance,
    /// The value expression of a PEP 695 `type` alias statement.
    AliasValue,
}
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
    Generic {
        base: Box<Annotation>,
        args: Vec<Annotation>,
    },
    /// A bracketed list display inside an annotation, such as the parameter
    /// list of `Callable[[int], str]`.
    List(Vec<Annotation>),
    StringLiteral(String),
    Union(Vec<Annotation>),
    Literal(Vec<LiteralValue>),
    None,
    Unknown(TypeReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationSyntaxKind {
    Name,
    Attribute,
    Subscript,
    BooleanOperator,
    NamedExpression,
    BinaryOperator,
    UnaryOperator,
    Lambda,
    Conditional,
    Dictionary,
    Set,
    ListComprehension,
    SetComprehension,
    DictionaryComprehension,
    Generator,
    Await,
    Yield,
    YieldFrom,
    Comparison,
    Call,
    FormattedString,
    TemplateString,
    StringLiteral,
    Bytes,
    Number,
    Boolean,
    NoneLiteral,
    Ellipsis,
    Starred,
    List,
    Tuple,
    Slice,
    IpythonEscape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralValue {
    String(String),
    Integer(String),
    Float {
        ieee_bits: u64,
    },
    Complex {
        real_ieee_bits: u64,
        imaginary_ieee_bits: u64,
    },
    Boolean(bool),
    None,
    Ellipsis,
    Unsupported(AnnotationSyntaxKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    Module,
    Class,
    Function,
    Field,
    Constant,
    Alias,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassForm {
    Plain,
    Dataclass,
    Protocol,
    TypedDict,
    Enum,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    Plain,
    StaticMethod,
    ClassMethod,
    Property,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterKind {
    PositionalOnly,
    PositionalOrKeyword,
    VarArgs,
    KeywordOnly,
    KwArgs,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccurrenceKind {
    FunctionCall,
    MethodCall,
    AttributeRead,
}
/// How a call's receiver was written, which decides the target key the
/// occurrence resolves through. Bare-name and module-gated rows keep the
/// extractor's original keying; widened attribute rows carry the receiver
/// class proven by the declaration walk or an honestly foreign receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeChainRoot {
    /// `self` / `cls` inside a class-body method at function_depth 1.
    Enclosing { class: String },
    /// Any other plain name, including `obj` in a nested function.
    Name { name: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeStep {
    /// One field name, root to leaf, excluding the member being resolved.
    Field(String),
    /// A runtime index whose slice is not a plain name (`value[0]`, `value[-1]`).
    /// Not a slice (`value[0:1]`). A numeric subscript of a class name is not that class.
    Index,
    /// A subscript whose slice is a plain name (`items[index]`, `Child[int]`).
    /// An annotated value peels to the element type. A bare class name falls
    /// back to that class, which is how `Child[int].note` stays `Child.note`.
    NameIndex,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OccurrenceReceiver {
    /// A bare-name call (`fn()`): resolved through module names alone.
    None,
    /// A call recorded under the module-name gate (`mod.fn()` where the
    /// attribute spelling is a declared module name): resolved through
    /// module names alone, exactly as before the widening.
    Module,
    /// `self.method()` / `cls.method()`: keyed by the attribute name and
    /// the enclosing class, proven by the declaration walk.
    EnclosingClass { class: String },
    /// `super()` or `super(Start, self)` / `super(Start, cls)` written
    /// directly in a class-body function (`super().note`, `super(Child,
    /// self).note()`). The search starts after `class` when `after` is
    /// `None`, or after `Start` when `after` is `Some`. Nested functions and
    /// the bare name `super` are not this variant.
    Super {
        class: String,
        /// `None` for zero-argument `super()`: search starts after `class`.
        /// `Some` for `super(Start, self)` or `super(Start, cls)`: search
        /// starts after `Start`.
        after: Option<String>,
    },
    /// `Child().note` / `Child().note()`, `Child[int]().note`,
    /// `Child[int][str]().note()`, and `Child[int].note` when subscripts peel
    /// to one simple name. The lowerer binds that class's member when the name
    /// is one live class. Non-class names (`factory`, `list`) and attribute
    /// callees (`pkg.Child[int]().note()`) keep universe keys.
    Constructed { class: String },
    /// `self.child.note` / `self.child.note()` and the `cls` form, one attribute
    /// deep. `class` is the enclosing class. `attribute` is the field name
    /// (`child`). Two or more fields before the member are `ChainedAttribute`.
    /// Index subscripts (`self.child[0].note`) are `SubscriptedAttribute`.
    InstanceAttribute { class: String, attribute: String },
    /// `obj.child.note` / `obj.child.note()` when `obj` is a plain name other than
    /// the enclosing `self`/`cls` form. `name` is `obj`. `attribute` is `child`.
    /// Two or more fields before the member are `ChainedAttribute`. Index
    /// subscripts are `SubscriptedAttribute`.
    NamedAttribute { name: String, attribute: String },
    /// `self.child.other.note` and `obj.child.other.note`, two or more fields
    /// before the member. Deeper chains are included. Index subscripts are
    /// `SubscriptedAttribute`.
    ChainedAttribute {
        root: AttributeChainRoot,
        attributes: Vec<String>,
    },
    /// `self.child[0].note`, `items[0].note`, `self.child[0].other.note` when the
    /// receiver contains at least one index subscript on a name or `self`/`cls`
    /// base. `steps` are root-to-leaf and exclude the member. A slice, a subscript
    /// of a call, and a chain with no index are not this variant.
    SubscriptedAttribute {
        root: AttributeChainRoot,
        steps: Vec<AttributeStep>,
    },
    /// `self.note()[0].extra` when the call's return annotation is peeled by the
    /// following index steps. `call` is the `CallReturn` of `note()`. `steps` are
    /// root-to-leaf after that call and before the member, and contain at least
    /// one index. A slice is not this variant.
    SubscriptedCall {
        call: Box<OccurrenceReceiver>,
        steps: Vec<AttributeStep>,
    },
    /// `self.note().extra` / `obj.note().extra()` / `note().extra()` and one
    /// or more hops through an attribute or constructed receiver before the
    /// call (`self.child.note().extra()` / `Child().note().extra()` /
    /// `self.note().extra().more()`). `method` is the called name (`note`).
    /// `receiver` is `EnclosingClass` for `self`/`cls`,
    /// `Foreign { receiver: Some(name) }` for another plain name, or `None`
    /// for a bare call. The inner receiver may be `InstanceAttribute`,
    /// `NamedAttribute`, `ChainedAttribute`, `SubscriptedAttribute`,
    /// `SubscriptedCall`, `Constructed`, or a nested `CallReturn` for successive
    /// calls. Still not `Super`, not `Module`, and not a slice.
    CallReturn {
        method: String,
        receiver: Box<OccurrenceReceiver>,
    },
    /// Any other receiver (`obj.method()`, `factory().method()`): honestly
    /// foreign. The receiver's written spelling is carried when the receiver
    /// is a plain name, so an imported module receiver can still resolve
    /// through its own package key; it is never resolved to a local row.
    Foreign { receiver: Option<String> },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterFact {
    pub name: String,
    /// Exact source span of the declared identifier, so every consumer can
    /// borrow the name bytes without re-tokenizing the parameter range.
    pub name_span: Span,
    pub kind: ParameterKind,
    pub has_default: bool,
    pub default_source: Option<String>,
    pub annotation: Annotation,
    /// Exact source span of the written annotation expression, if any.
    pub annotation_span: Option<Span>,
    pub span: Span,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationFact {
    pub name: String,
    /// Exact source span of the declared identifier.
    pub name_span: Span,
    pub kind: DeclarationKind,
    pub span: Span,
    pub bases: Vec<Annotation>,
    pub class_form: Option<ClassForm>,
    pub decorators: Vec<String>,
    /// Source span of each decorator spelling, parallel to `decorators`;
    /// every span covers the leading `@` so the spelling is the tail.
    pub decorator_spans: Vec<Span>,
    pub is_async: bool,
    pub receiver: ReceiverKind,
    pub parameters: Vec<ParameterFact>,
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
    /// Byte offset just past a function header's final colon — the anchor
    /// for consumers that must find the body's first line. The declaration
    /// extent covers the complete definition, so the header boundary is
    /// carried explicitly; `None` for every non-function declaration.
    pub header_end: Option<u32>,
    pub docstring: Option<DocstringFact>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocstringFact {
    pub raw: String,
    pub span: Span,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceFact {
    pub owner: String,
    pub target: String,
    pub kind: OccurrenceKind,
    pub confidence: Confidence,
    pub span: Span,
    pub receiver: OccurrenceReceiver,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Index,
}
/// One `for` clause of a comprehension. Its iterable is evaluated before
/// `targets` exist. The leftmost iterable is also outside the comprehension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComprehensionClause {
    pub iterable: Span,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingScopeFact {
    /// Span of the function, lambda, or comprehension.
    pub span: Span,
    /// Empty for functions and lambdas. Comprehension clauses are in source order.
    pub clauses: Vec<ComprehensionClause>,
    pub locals: Vec<String>,
    /// Names assigned inside this scope, excluding parameters.
    pub assigned: Vec<String>,
    pub globals: Vec<String>,
    pub nonlocals: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFacts {
    pub identity: String,
    pub span: Span,
    pub declarations: Vec<DeclarationFact>,
    pub occurrences: Vec<OccurrenceFact>,
    pub docstring: Option<DocstringFact>,
    pub annotations: Vec<AnnotationFact>,
    pub binding_scopes: Vec<BindingScopeFact>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationFact {
    pub owner: String,
    pub position: AnnotationPosition,
    pub annotation: Annotation,
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

#[derive(Debug, Error)]
pub enum ExtractionError {
    #[error("source is not UTF-8 at {span:?}")]
    InvalidUtf8 { span: Span, bytes: Vec<u8> },
    #[error("Python parser rejected the source")]
    RejectedSyntax { rejection: RejectedSyntax },
    #[error("Python source has {actual} bytes, exceeding the 32-bit source-coordinate bound")]
    SourceLength {
        actual: usize,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("Ruff returned source range {start}..{end} outside {source_length} input bytes")]
    InvalidRange {
        start: usize,
        end: usize,
        source_length: usize,
    },
    #[error("Ruff returned an expression after a module-mode parse")]
    NonModuleParse { rejection: RejectedSyntax },
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
            header_end: None,
            docstring: module_doc.clone(),
        }],
        occurrences: Vec::new(),
        docstring: module_doc,
        annotations: Vec::new(),
        binding_scopes: Vec::new(),
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
            enclosing_class: None,
            decorator_ranges: Vec::new(),
            decorator_owner: None,
            error: None,
            module_declared: HashSet::new(),
            last_module_function: None,
            bodies: Vec::new(),
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

/// The byte offset just past one function header's final colon. The
/// declaration extent covers the complete definition, so the header
/// boundary derives from the header text before the first body statement;
/// `None` when the header carries no delimiter.
fn function_header_end(
    text: &str,
    range: ruff_text_size::TextRange,
    body: &[ast::Stmt],
) -> Option<u32> {
    let start = range.start().to_usize();
    let body_start = body
        .first()
        .map_or(range.end().to_usize(), |statement| {
            statement.range().start().to_usize()
        })
        .min(text.len());
    let header = text.get(start..body_start)?;
    let colon = header.rfind(':')?;
    u32::try_from(start + colon + 1).ok()
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

/// A module assignment is a type alias only when its value is a type expression.
///
/// Names, attributes, subscripts, `|` unions, quoted annotations, and `None`
/// match the shapes `annotation` already projects. Calls, numbers, lists, and
/// other runtime values stay ordinary constants and record no `AliasValue`.
fn type_shaped_alias_value(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Name(_)
        | ast::Expr::Attribute(_)
        | ast::Expr::Subscript(_)
        | ast::Expr::StringLiteral(_)
        | ast::Expr::NoneLiteral(_) => true,
        ast::Expr::BinOp(binary) if binary.op == ast::Operator::BitOr => {
            type_shaped_alias_value(&binary.left) && type_shaped_alias_value(&binary.right)
        }
        _ => false,
    }
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

/// Walks only `Subscript::value` until a `Name` is reached. Stops on anything
/// else, so `pkg.Child[int]` does not peel and `list[Child]` peels to `list`.
fn peel_subscript_name(expr: &ast::Expr) -> Option<&str> {
    let mut current = expr;
    loop {
        match current {
            ast::Expr::Subscript(subscript) => current = subscript.value.as_ref(),
            ast::Expr::Name(name) => return Some(name.id.as_str()),
            _ => return None,
        }
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
    /// The innermost class being walked, known from the declaration walk.
    /// A `self`/`cls` receiver resolves against exactly this class.
    enclosing_class: Option<String>,
    decorator_ranges: Vec<ruff_text_size::TextRange>,
    decorator_owner: Option<String>,
    error: Option<ExtractionError>,
    /// Module-level names already declared. The first declaration of a name
    /// wins; later rebindings are dropped. Consecutive overloads of one name
    /// are exempt via [`last_module_function`].
    module_declared: HashSet<String>,
    /// The most recent module-level function name, so overload runs stay
    /// distinct from later rebindings of the same spelling.
    last_module_function: Option<String>,
    /// Innermost class or function bodies, so a class nested in a function
    /// is not treated as that function's local scope.
    bodies: Vec<BodyFrame>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Function,
    Class,
    Comprehension,
    Lambda,
}

struct BodyFrame {
    kind: BodyKind,
    span: Span,
    clauses: Vec<ComprehensionClause>,
    locals: HashSet<String>,
    assigned: HashSet<String>,
    globals: HashSet<String>,
    nonlocals: HashSet<String>,
}
impl<'a> Projection<'a> {
    fn add_declaration(&mut self, declaration: DeclarationFact) {
        if self.function_depth == 0 && self.class_depth == 0 {
            let allow_overload = declaration.kind == DeclarationKind::Function
                && self.last_module_function.as_deref() == Some(declaration.name.as_str());
            if self.module_declared.contains(&declaration.name) && !allow_overload {
                return;
            }
            if !allow_overload {
                self.module_declared.insert(declaration.name.clone());
            }
            if declaration.kind == DeclarationKind::Function {
                self.last_module_function = Some(declaration.name.clone());
            } else {
                self.last_module_function = None;
            }
        }
        self.facts.declarations.push(declaration);
    }

    /// Stores the first projection fault; later traversal cannot replace its evidence.
    fn reject(&mut self, error: ExtractionError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// `super()` or `super(Start, self)` / `super(Start, cls)` inside a
    /// class-body method at `function_depth` 1. One-argument `super`, dotted
    /// start classes, other second arguments, keywords, nested functions, and
    /// bare `super` stay foreign.
    fn super_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        let ast::Expr::Call(call) = expr else {
            return None;
        };
        let ast::Expr::Name(name) = call.func.as_ref() else {
            return None;
        };
        if name.id.as_str() != "super" {
            return None;
        }
        if !call.arguments.keywords.is_empty() {
            return None;
        }
        if self.function_depth != 1 {
            return None;
        }
        let class = self.enclosing_class.clone()?;
        let after = match call.arguments.args.len() {
            0 => None,
            2 => {
                let ast::Expr::Name(start) = &call.arguments.args[0] else {
                    return None;
                };
                let ast::Expr::Name(second) = &call.arguments.args[1] else {
                    return None;
                };
                if !matches!(second.id.as_str(), "self" | "cls") {
                    return None;
                }
                Some(start.id.as_str().to_owned())
            }
            _ => return None,
        };
        Some(OccurrenceReceiver::Super { class, after })
    }

    /// `Child().note` / `Child().note()`, `Child[int]().note`, and
    /// `Child[int].note` when subscripts on the callee peel to one simple name.
    /// Attribute callees (`pkg.Child[int]().note()`) stay foreign.
    fn constructed_class_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        let class = match expr {
            ast::Expr::Call(call) => peel_subscript_name(call.func.as_ref()),
            ast::Expr::Subscript(_) => peel_subscript_name(expr),
            _ => None,
        }?;
        Some(OccurrenceReceiver::Constructed {
            class: class.to_owned(),
        })
    }

    /// `self.child.note` / `self.child.note()` and the `cls` form when `expr`
    /// is one `self`/`cls` attribute access (`self.child`).
    fn instance_attribute_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        if self.function_depth != 1 {
            return None;
        }
        let ast::Expr::Attribute(inner) = expr else {
            return None;
        };
        let ast::Expr::Name(name) = inner.value.as_ref() else {
            return None;
        };
        if !matches!(name.id.as_str(), "self" | "cls") {
            return None;
        }
        let class = self.enclosing_class.clone()?;
        Some(OccurrenceReceiver::InstanceAttribute {
            class,
            attribute: inner.attr.as_str().to_owned(),
        })
    }

    /// `obj.child.note` / `obj.child.note()` when `expr` is one plain-name
    /// attribute access (`obj.child`).
    fn named_attribute_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        let ast::Expr::Attribute(inner) = expr else {
            return None;
        };
        let ast::Expr::Name(name) = inner.value.as_ref() else {
            return None;
        };
        Some(OccurrenceReceiver::NamedAttribute {
            name: name.id.as_str().to_owned(),
            attribute: inner.attr.as_str().to_owned(),
        })
    }

    /// `self.child.other.note` / `obj.child.other.note` when `expr` is two or
    /// more attribute accesses before the member (`self.child.other`).
    fn chained_attribute_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        let mut attributes = Vec::new();
        let mut current = expr;
        while let ast::Expr::Attribute(inner) = current {
            attributes.push(inner.attr.as_str().to_owned());
            current = inner.value.as_ref();
        }
        if attributes.len() < 2 {
            return None;
        }
        attributes.reverse();
        let root = match current {
            ast::Expr::Name(name) => {
                let id = name.id.as_str();
                if matches!(id, "self" | "cls") && self.function_depth == 1 {
                    let class = self.enclosing_class.clone()?;
                    AttributeChainRoot::Enclosing { class }
                } else {
                    AttributeChainRoot::Name {
                        name: id.to_owned(),
                    }
                }
            }
            _ => return None,
        };
        Some(OccurrenceReceiver::ChainedAttribute { root, attributes })
    }

    /// True when a `CallReturn` nested inside `SubscriptedCall` must stay absent
    /// because `self`/`cls` would name the enclosing class at `function_depth != 1`.
    fn rejects_nested_self_call_receiver(&self, receiver: &OccurrenceReceiver) -> bool {
        let OccurrenceReceiver::CallReturn {
            receiver: inner, ..
        } = receiver
        else {
            return true;
        };
        match inner.as_ref() {
            OccurrenceReceiver::NamedAttribute { name, .. }
                if matches!(name.as_str(), "self" | "cls") && self.function_depth != 1 =>
            {
                true
            }
            OccurrenceReceiver::ChainedAttribute {
                root: AttributeChainRoot::Name { name },
                ..
            } if matches!(name.as_str(), "self" | "cls") && self.function_depth != 1 => true,
            OccurrenceReceiver::SubscriptedAttribute {
                root: AttributeChainRoot::Name { name },
                ..
            } if matches!(name.as_str(), "self" | "cls") && self.function_depth != 1 => true,
            OccurrenceReceiver::SubscriptedAttribute {
                root: AttributeChainRoot::Enclosing { .. },
                ..
            } if self.function_depth != 1 => true,
            _ => false,
        }
    }

    /// `self.child[0].note` / `items[0].note` / `self.note()[0].extra` when `expr`
    /// is an attribute or index subscript chain containing at least one index
    /// subscript before the member. Slices are not this receiver.
    fn subscripted_attribute_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        let mut steps = Vec::new();
        let mut current = expr;
        loop {
            match current {
                ast::Expr::Attribute(inner) => {
                    steps.push(AttributeStep::Field(inner.attr.as_str().to_owned()));
                    current = inner.value.as_ref();
                }
                ast::Expr::Subscript(subscript) => {
                    if matches!(subscript.slice.as_ref(), ast::Expr::Slice(_)) {
                        return None;
                    }
                    let step = if matches!(subscript.slice.as_ref(), ast::Expr::Name(_)) {
                        AttributeStep::NameIndex
                    } else {
                        AttributeStep::Index
                    };
                    steps.push(step);
                    current = subscript.value.as_ref();
                }
                ast::Expr::Call(_) => {
                    let call = self.call_return_receiver(current)?;
                    if self.rejects_nested_self_call_receiver(&call) {
                        return None;
                    }
                    steps.reverse();
                    if !steps.iter().any(|step| {
                        matches!(step, AttributeStep::Index | AttributeStep::NameIndex)
                    }) {
                        return None;
                    }
                    return Some(OccurrenceReceiver::SubscriptedCall {
                        call: Box::new(call),
                        steps,
                    });
                }
                ast::Expr::Name(name) => {
                    let id = name.id.as_str();
                    steps.reverse();
                    if !steps.iter().any(|step| {
                        matches!(step, AttributeStep::Index | AttributeStep::NameIndex)
                    }) {
                        return None;
                    }
                    if matches!(id, "self" | "cls") {
                        if self.function_depth != 1 {
                            return None;
                        }
                        let class = self.enclosing_class.clone()?;
                        if !steps
                            .iter()
                            .any(|step| matches!(step, AttributeStep::Field(_)))
                        {
                            return None;
                        }
                        return Some(OccurrenceReceiver::SubscriptedAttribute {
                            root: AttributeChainRoot::Enclosing { class },
                            steps,
                        });
                    }
                    return Some(OccurrenceReceiver::SubscriptedAttribute {
                        root: AttributeChainRoot::Name {
                            name: id.to_owned(),
                        },
                        steps,
                    });
                }
                _ => return None,
            }
        }
    }

    /// `self.note().extra` / `obj.note().extra()` / `note().extra()` when `expr`
    /// is the call before the member, including one or more hops through an
    /// attribute, constructed, or call-return receiver (`self.child.note().extra()`
    /// / `Child().note().extra()` / `self.note().extra().more()`).
    fn call_return_receiver(&self, expr: &ast::Expr) -> Option<OccurrenceReceiver> {
        let ast::Expr::Call(call) = expr else {
            return None;
        };
        match call.func.as_ref() {
            ast::Expr::Attribute(attribute) => {
                let method = attribute.attr.as_str().to_owned();
                match attribute.value.as_ref() {
                    ast::Expr::Name(name) => {
                        let id = name.id.as_str();
                        if matches!(id, "self" | "cls") && self.function_depth == 1 {
                            if let Some(class) = self.enclosing_class.clone() {
                                return Some(OccurrenceReceiver::CallReturn {
                                    method,
                                    receiver: Box::new(OccurrenceReceiver::EnclosingClass {
                                        class,
                                    }),
                                });
                            }
                        }
                        Some(OccurrenceReceiver::CallReturn {
                            method,
                            receiver: Box::new(OccurrenceReceiver::Foreign {
                                receiver: Some(id.to_owned()),
                            }),
                        })
                    }
                    value => {
                        let inner = self
                            .instance_attribute_receiver(value)
                            .or_else(|| self.named_attribute_receiver(value))
                            .or_else(|| self.chained_attribute_receiver(value))
                            .or_else(|| self.subscripted_attribute_receiver(value))
                            .or_else(|| self.call_return_receiver(value))
                            .or_else(|| self.constructed_class_receiver(value));
                        let inner = match inner {
                            Some(OccurrenceReceiver::NamedAttribute { ref name, .. })
                                if matches!(name.as_str(), "self" | "cls")
                                    && self.function_depth != 1 =>
                            {
                                None
                            }
                            Some(OccurrenceReceiver::ChainedAttribute {
                                root: AttributeChainRoot::Name { ref name },
                                ..
                            }) if matches!(name.as_str(), "self" | "cls")
                                && self.function_depth != 1 =>
                            {
                                None
                            }
                            Some(OccurrenceReceiver::SubscriptedAttribute {
                                root: AttributeChainRoot::Name { ref name },
                                ..
                            }) if matches!(name.as_str(), "self" | "cls")
                                && self.function_depth != 1 =>
                            {
                                None
                            }
                            Some(OccurrenceReceiver::SubscriptedAttribute {
                                root: AttributeChainRoot::Enclosing { .. },
                                ..
                            }) if self.function_depth != 1 =>
                            {
                                None
                            }
                            Some(OccurrenceReceiver::SubscriptedCall { ref call, .. })
                                if self.rejects_nested_self_call_receiver(call) =>
                            {
                                None
                            }
                            other => other,
                        };
                        inner.map(|receiver| OccurrenceReceiver::CallReturn {
                            method,
                            receiver: Box::new(receiver),
                        })
                    }
                }
            }
            ast::Expr::Name(name) => {
                let id = name.id.as_str();
                if self.module_level_function_name(id) {
                    Some(OccurrenceReceiver::CallReturn {
                        method: id.to_owned(),
                        receiver: Box::new(OccurrenceReceiver::None),
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// True when `name` is one live module-level function declaration.
    fn module_level_function_name(&self, name: &str) -> bool {
        self.facts.declarations.iter().any(|declaration| {
            declaration.kind == DeclarationKind::Function
                && declaration.name == name
                && !self
                    .facts
                    .declarations
                    .iter()
                    .any(|class| {
                        class.kind == DeclarationKind::Class
                            && class.span.start <= declaration.span.start
                            && declaration.span.end <= class.span.end
                    })
        })
    }

    /// Receiver classification shared by attribute reads and method calls.
    fn attribute_occurrence_receiver(&self, value: &ast::Expr) -> OccurrenceReceiver {
        if let Some(receiver) = self.super_receiver(value) {
            return receiver;
        }
        if let Some(receiver) = self.call_return_receiver(value) {
            return receiver;
        }
        if let Some(receiver) = self.subscripted_attribute_receiver(value) {
            return receiver;
        }
        if let Some(receiver) = self.constructed_class_receiver(value) {
            return receiver;
        }
        if let Some(receiver) = self.instance_attribute_receiver(value) {
            return receiver;
        }
        if let Some(receiver) = self.named_attribute_receiver(value) {
            return receiver;
        }
        if let Some(receiver) = self.chained_attribute_receiver(value) {
            return receiver;
        }
        match value {
            ast::Expr::Name(name) => {
                if matches!(name.id.as_str(), "self" | "cls") {
                    if let Some(class) = self.enclosing_class.clone() {
                        return OccurrenceReceiver::EnclosingClass { class };
                    }
                }
                OccurrenceReceiver::Foreign {
                    receiver: Some(name.id.as_str().to_owned()),
                }
            }
            _ => OccurrenceReceiver::Foreign { receiver: None },
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

    fn push_binding_frame(&mut self, kind: BodyKind, range: ruff_text_size::TextRange) {
        self.bodies.push(BodyFrame {
            kind,
            span: span(range),
            clauses: Vec::new(),
            locals: HashSet::new(),
            assigned: HashSet::new(),
            globals: HashSet::new(),
            nonlocals: HashSet::new(),
        });
    }

    fn pop_binding_frame(&mut self) {
        if let Some(frame) = self.bodies.pop() {
            match frame.kind {
                BodyKind::Function | BodyKind::Lambda | BodyKind::Comprehension => {
                    let mut locals: Vec<String> = frame.locals.into_iter().collect();
                    locals.sort();
                    let mut assigned: Vec<String> = frame.assigned.into_iter().collect();
                    assigned.sort();
                    let mut globals: Vec<String> = frame.globals.into_iter().collect();
                    globals.sort();
                    let mut nonlocals: Vec<String> = frame.nonlocals.into_iter().collect();
                    nonlocals.sort();
                    self.facts.binding_scopes.push(BindingScopeFact {
                        span: frame.span,
                        clauses: frame.clauses,
                        locals,
                        assigned,
                        globals,
                        nonlocals,
                    });
                }
                BodyKind::Class => {}
            }
        }
    }

    fn note_local(&mut self, name: &str) {
        if let Some(frame) = self.bodies.last_mut() {
            match frame.kind {
                BodyKind::Function | BodyKind::Lambda | BodyKind::Comprehension => {
                    frame.locals.insert(name.to_owned());
                }
                BodyKind::Class => {}
            }
        }
    }

    fn note_assigned(&mut self, name: &str) {
        if let Some(frame) = self.bodies.last_mut() {
            match frame.kind {
                BodyKind::Function | BodyKind::Lambda => {
                    frame.locals.insert(name.to_owned());
                    frame.assigned.insert(name.to_owned());
                }
                BodyKind::Comprehension => {
                    frame.locals.insert(name.to_owned());
                }
                BodyKind::Class => {}
            }
        }
    }

    fn note_walrus(&mut self, name: &str) {
        for frame in self.bodies.iter_mut().rev() {
            match frame.kind {
                BodyKind::Comprehension => continue,
                BodyKind::Function | BodyKind::Lambda => {
                    frame.locals.insert(name.to_owned());
                    frame.assigned.insert(name.to_owned());
                    return;
                }
                BodyKind::Class => return,
            }
        }
    }

    fn note_global(&mut self, name: &str) {
        if let Some(frame) = self.bodies.last_mut() {
            match frame.kind {
                BodyKind::Function | BodyKind::Lambda | BodyKind::Comprehension => {
                    frame.globals.insert(name.to_owned());
                }
                BodyKind::Class => {}
            }
        }
    }

    fn note_nonlocal(&mut self, name: &str) {
        if let Some(frame) = self.bodies.last_mut() {
            match frame.kind {
                BodyKind::Function | BodyKind::Lambda | BodyKind::Comprehension => {
                    frame.nonlocals.insert(name.to_owned());
                }
                BodyKind::Class => {}
            }
        }
    }

    fn note_nested_binding(&mut self, name: &str) {
        self.note_local(name);
    }

    fn note_import_alias(&mut self, alias: &ast::Alias) {
        if let Some(frame) = self.bodies.last() {
            if matches!(frame.kind, BodyKind::Function | BodyKind::Lambda) {
                self.note_local(&alias_binding(alias));
            }
        }
    }

    /// Default expressions are evaluated in the enclosing scope, so they are
    /// walked before the function or lambda frame exists.
    fn walk_parameter_defaults(&mut self, parameters: &'a ast::Parameters) {
        for item in &parameters.posonlyargs {
            if let Some(default) = item.default.as_deref() {
                self.visit_expr(default);
            }
        }
        for item in &parameters.args {
            if let Some(default) = item.default.as_deref() {
                self.visit_expr(default);
            }
        }
        for item in &parameters.kwonlyargs {
            if let Some(default) = item.default.as_deref() {
                self.visit_expr(default);
            }
        }
    }

    fn note_parameters(&mut self, parameters: &ast::Parameters) {
        for item in &parameters.posonlyargs {
            self.note_local(item.parameter.name.as_str());
        }
        for item in &parameters.args {
            self.note_local(item.parameter.name.as_str());
        }
        if let Some(item) = parameters.vararg.as_ref() {
            self.note_local(item.name.as_str());
        }
        for item in &parameters.kwonlyargs {
            self.note_local(item.parameter.name.as_str());
        }
        if let Some(item) = parameters.kwarg.as_ref() {
            self.note_local(item.name.as_str());
        }
    }

    fn comprehension_target_names(target: &ast::Expr, names: &mut Vec<String>) {
        match target {
            ast::Expr::Name(name) if name.ctx.is_store() => names.push(name.id.as_str().to_owned()),
            ast::Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    Self::comprehension_target_names(element, names);
                }
            }
            ast::Expr::List(list) => {
                for element in &list.elts {
                    Self::comprehension_target_names(element, names);
                }
            }
            ast::Expr::Starred(starred) => {
                Self::comprehension_target_names(starred.value.as_ref(), names);
            }
            _ => {}
        }
    }

    fn visit_comprehension_target(&mut self, target: &'a ast::Expr) {
        match target {
            ast::Expr::Name(name) if name.ctx.is_store() => self.note_local(name.id.as_str()),
            ast::Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.visit_comprehension_target(element);
                }
            }
            ast::Expr::List(list) => {
                for element in &list.elts {
                    self.visit_comprehension_target(element);
                }
            }
            ast::Expr::Starred(starred) => self.visit_comprehension_target(starred.value.as_ref()),
            _ => visitor::walk_expr(self, target),
        }
    }

    fn visit_comprehension_generators(
        &mut self,
        generators: &'a [ast::Comprehension],
        comp_range: ruff_text_size::TextRange,
        element: &'a ast::Expr,
        key: Option<&'a ast::Expr>,
    ) {
        let clauses = generators
            .iter()
            .map(|generator| {
                let mut targets = Vec::new();
                Self::comprehension_target_names(&generator.target, &mut targets);
                targets.sort();
                ComprehensionClause {
                    iterable: span(generator.iter.range()),
                    targets,
                }
            })
            .collect();
        if let Some(first) = generators.first() {
            self.visit_expr(&first.iter);
        }
        self.push_binding_frame(BodyKind::Comprehension, comp_range);
        if let Some(frame) = self.bodies.last_mut() {
            frame.clauses = clauses;
        }
        if let Some(first) = generators.first() {
            self.visit_comprehension_target(&first.target);
            for condition in &first.ifs {
                self.visit_expr(condition);
            }
        }
        for generator in generators.iter().skip(1) {
            self.visit_expr(&generator.iter);
            self.visit_comprehension_target(&generator.target);
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        if let Some(key) = key {
            self.visit_expr(key);
        }
        self.visit_expr(element);
        self.pop_binding_frame();
    }

    fn occurrence_owner(&self, expr: &ast::Expr) -> &str {
        if self.decorator_ranges.iter().any(|range| {
            range.start() <= expr.range().start() && range.end() >= expr.range().end()
        }) {
            match self.decorator_owner.as_deref() {
                Some(owner) => owner,
                None => &self.owner,
            }
        } else {
            &self.owner
        }
    }

    fn record_attribute_occurrence(&mut self, owner: &str, attribute: &ast::ExprAttribute) {
        let target = attribute.attr.as_str();
        let attr_span = span(attribute.attr.range());
        let already_recorded = self.facts.occurrences.iter().any(|occurrence| {
            occurrence.owner == owner
                && occurrence.target == target
                && occurrence.span.start <= attr_span.start
                && occurrence.span.end >= attr_span.end
        });
        if already_recorded {
            return;
        }
        let receiver = self.attribute_occurrence_receiver(attribute.value.as_ref());
        self.facts.occurrences.push(OccurrenceFact {
            owner: owner.to_owned(),
            target: target.to_owned(),
            kind: OccurrenceKind::AttributeRead,
            confidence: Confidence::Index,
            span: attr_span,
            receiver,
        });
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
                // The declaration extent covers the complete definition,
                // header and body alike, exactly like a class extent: body
                // occurrences and nested definitions must resolve inside
                // their owner's span rather than dangle past a header.
                let declaration_span = span(function.range);
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
                    header_end: function_header_end(self.text, function.range, &function.body),
                    docstring: doc,
                });
                self.decorator_ranges = function.decorator_list.iter().map(|d| d.range).collect();
                self.decorator_owner = Some(old.clone());
                self.note_nested_binding(&name);
                self.owner = name;
                self.function_depth += 1;
                self.push_binding_frame(BodyKind::Function, function.range);
                self.note_parameters(&function.parameters);
                visitor::walk_stmt(self, statement);
                self.pop_binding_frame();
                self.function_depth -= 1;
                self.owner = old;
                self.decorator_ranges = old_decorator_ranges;
                self.decorator_owner = old_decorator_owner;
                return;
            }
            ast::Stmt::ClassDef(class) => {
                let old = self.owner.clone();
                let old_enclosing_class = self.enclosing_class.take();
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
                    header_end: None,
                    docstring,
                });
                self.decorator_ranges = class.decorator_list.iter().map(|d| d.range).collect();
                self.decorator_owner = Some(old.clone());
                self.note_nested_binding(&name);
                self.owner = name;
                self.enclosing_class = Some(self.owner.clone());
                self.class_depth += 1;
                self.push_binding_frame(BodyKind::Class, class.range);
                visitor::walk_stmt(self, statement);
                self.pop_binding_frame();
                self.class_depth -= 1;
                self.owner = old;
                self.enclosing_class = old_enclosing_class;
                self.decorator_ranges = old_decorator_ranges;
                self.decorator_owner = old_decorator_owner;
                return;
            }
            ast::Stmt::Import(import) if self.function_depth == 0 && self.class_depth == 0 => {
                for alias in &import.names {
                    self.add_alias(alias, statement);
                }
            }
            ast::Stmt::Import(import) if self.function_depth > 0 => {
                for alias in &import.names {
                    self.note_import_alias(alias);
                }
            }
            ast::Stmt::ImportFrom(import) if self.function_depth == 0 && self.class_depth == 0 => {
                for alias in &import.names {
                    self.add_from_alias(import, alias, statement);
                }
            }
            ast::Stmt::ImportFrom(import) if self.function_depth > 0 => {
                for alias in &import.names {
                    self.note_import_alias(alias);
                }
            }
            ast::Stmt::Global(global) => {
                for name in &global.names {
                    self.note_global(name.as_str());
                }
            }
            ast::Stmt::Nonlocal(nonlocal) => {
                for name in &nonlocal.names {
                    self.note_nonlocal(name.as_str());
                }
            }
            ast::Stmt::TypeAlias(alias) if self.function_depth == 0 && self.class_depth == 0 => {
                self.add_type_alias(alias, statement);
            }
            ast::Stmt::TypeAlias(alias) if self.function_depth > 0 || self.class_depth > 0 => {
                if let ast::Expr::Name(name) = alias.name.as_ref() {
                    self.note_nested_binding(name.id.as_str());
                }
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
            ast::Stmt::AnnAssign(assign)
                if self.function_depth >= 1
                    && self.bodies.last().map(|frame| frame.kind) == Some(BodyKind::Function) =>
            {
                if let ast::Expr::Name(name) = assign.target.as_ref() {
                    self.facts.annotations.push(AnnotationFact {
                        owner: name.id.as_str().to_owned(),
                        position: AnnotationPosition::Local,
                        annotation: annotation(assign.annotation.as_ref()),
                        span: span(assign.range()),
                    });
                } else if self.function_depth == 1 && self.enclosing_class.is_some() {
                    if let ast::Expr::Attribute(attribute) = assign.target.as_ref() {
                        if let ast::Expr::Name(receiver) = attribute.value.as_ref() {
                            if matches!(receiver.id.as_str(), "self" | "cls") {
                                self.facts.annotations.push(AnnotationFact {
                                    owner: attribute.attr.as_str().to_owned(),
                                    position: AnnotationPosition::Instance,
                                    annotation: annotation(assign.annotation.as_ref()),
                                    span: span(assign.range()),
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, statement);
    }
    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(inner) = handler;
        if let Some(name) = &inner.name {
            self.note_assigned(name.as_str());
        }
        visitor::walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'a ast::Pattern) {
        match pattern {
            ast::Pattern::MatchAs(match_as) => {
                if let Some(name) = &match_as.name {
                    self.note_assigned(name.as_str());
                }
                if let Some(inner) = &match_as.pattern {
                    self.visit_pattern(inner);
                }
            }
            ast::Pattern::MatchStar(match_star) => {
                if let Some(name) = &match_star.name {
                    self.note_assigned(name.as_str());
                }
            }
            ast::Pattern::MatchMapping(mapping) => {
                if let Some(rest) = &mapping.rest {
                    self.note_assigned(rest.as_str());
                }
                for pattern in &mapping.patterns {
                    self.visit_pattern(pattern);
                }
            }
            _ => visitor::walk_pattern(self, pattern),
        }
    }

    fn visit_expr(&mut self, expr: &'a ast::Expr) {
        if let ast::Expr::Named(named) = expr {
            self.visit_expr(&named.value);
            if let ast::Expr::Name(name) = named.target.as_ref() {
                if name.ctx.is_store() {
                    self.note_walrus(name.id.as_str());
                }
            }
            self.visit_expr(&named.target);
            return;
        }
        if let ast::Expr::Lambda(lambda) = expr {
            if let Some(parameters) = lambda.parameters.as_ref() {
                self.walk_parameter_defaults(parameters);
            }
            // The binding span is the body. Defaults stay in the enclosing
            // scope, so `lambda obj=obj.note(): obj` still sees the module name.
            self.push_binding_frame(BodyKind::Lambda, lambda.body.range());
            if let Some(parameters) = lambda.parameters.as_ref() {
                self.note_parameters(parameters);
            }
            self.visit_expr(&lambda.body);
            self.pop_binding_frame();
            return;
        }
        if let ast::Expr::ListComp(list_comp) = expr {
            self.visit_comprehension_generators(
                &list_comp.generators,
                list_comp.range,
                &list_comp.elt,
                None,
            );
            return;
        }
        if let ast::Expr::SetComp(set_comp) = expr {
            self.visit_comprehension_generators(
                &set_comp.generators,
                set_comp.range,
                &set_comp.elt,
                None,
            );
            return;
        }
        if let ast::Expr::DictComp(dict_comp) = expr {
            self.visit_comprehension_generators(
                &dict_comp.generators,
                dict_comp.range,
                &dict_comp.value,
                dict_comp.key.as_deref(),
            );
            return;
        }
        if let ast::Expr::Generator(generator) = expr {
            self.visit_comprehension_generators(
                &generator.generators,
                generator.range,
                &generator.elt,
                None,
            );
            return;
        }
        if let ast::Expr::Name(name) = expr {
            if name.ctx.is_store() {
                self.note_assigned(name.id.as_str());
            }
            return;
        }
        if let ast::Expr::Call(call) = expr {
            // One call becomes at most one occurrence row. A callee whose
            // spelling is a declared module name keeps the original
            // module-keyed shape exactly (span included); any other
            // attribute call is widened honestly — any receiver is
            // recorded, the span covers the attribute token, and the
            // receiver decides the target key.
            let recorded = match call.func.as_ref() {
                ast::Expr::Name(name) => {
                    let target = name.id.as_str();
                    self.names
                        .iter()
                        .any(|declared| declared == target)
                        .then(|| {
                            (
                                target,
                                OccurrenceKind::FunctionCall,
                                OccurrenceReceiver::None,
                                call.func.range(),
                            )
                        })
                }
                ast::Expr::Attribute(attribute) => {
                    let target = attribute.attr.as_str();
                    let receiver =
                        self.attribute_occurrence_receiver(attribute.value.as_ref());
                    let receiver_is_module_name = matches!(
                        attribute.value.as_ref(),
                        ast::Expr::Name(name)
                            if self.names.iter().any(|declared| declared == name.id.as_str())
                    );
                    let gated = receiver_is_module_name
                        && self.names.iter().any(|declared| declared == target);
                    Some((
                        target,
                        OccurrenceKind::MethodCall,
                        if gated {
                            OccurrenceReceiver::Module
                        } else {
                            receiver
                        },
                        if gated {
                            call.func.range()
                        } else {
                            attribute.attr.range()
                        },
                    ))
                }
                _ => None,
            };
            if let Some((target, kind, receiver, callee_span)) = recorded {
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
                    span: span(callee_span),
                    receiver,
                });
            }
        }
        if let ast::Expr::Attribute(attribute) = expr {
            self.visit_expr(attribute.value.as_ref());
            let owner = self.occurrence_owner(expr).to_owned();
            self.record_attribute_occurrence(&owner, attribute);
            return;
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
            header_end: None,
            docstring: None,
        });
    }

    /// Records one `from … import …` binding. The alias's `value_source`
    /// carries the imported module spelling (`workout.service` in `from
    /// workout.service import service`); `value_span` keeps the binding
    /// identifier span so bare-name call keying stays unchanged.
    fn add_from_alias(
        &mut self,
        import: &ast::StmtImportFrom,
        alias: &ast::Alias,
        statement: &ast::Stmt,
    ) {
        let binding = alias_binding(alias);
        let Some(binding_span) = self.alias_binding_span(alias) else {
            return;
        };
        let value_source = import
            .module
            .as_ref()
            .map(|module| module.as_str().to_owned())
            .or_else(|| Some(alias.name.as_str().to_owned()));
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
            value_source,
            value_span: Some(span(alias.name.range())),
            type_parameters: Vec::new(),
            total: None,
            header_end: None,
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
            header_end: None,
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
                header_end: None,
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
                header_end: None,
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
            // Pushed after `add_declaration`, including when a later rebinding
            // of the same name was dropped, so two alias values stay ambiguous.
            if let Some(value) = value
                && type_shaped_alias_value(value)
            {
                self.facts.annotations.push(AnnotationFact {
                    owner: name.id.as_str().to_owned(),
                    position: AnnotationPosition::AliasValue,
                    annotation: annotation(value),
                    span: span(value.range()),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ruff_text_size::{TextRange, TextSize};

    use backend_semantic::vocabulary::PythonVersion;

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
            binding_scopes: Vec::new(),
        };
        let mut projection = Projection {
            text: "x",
            names: &[],
            facts: &mut facts,
            owner: String::new(),
            class_depth: 0,
            function_depth: 0,
            enclosing_class: None,
            decorator_ranges: Vec::new(),
            decorator_owner: None,
            error: None,
            module_declared: HashSet::new(),
            last_module_function: None,
            bodies: Vec::new(),
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
