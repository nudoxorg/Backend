//! Projects source-preserving Python syntax facts directly from Ruff's AST.
//! Retains unresolved syntax explicitly instead of claiming type resolution.
//! Keeps Python syntax authority independent of compiler IR transport.

use compiler_vocabulary::PythonVersion;
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
    Unannotated {
        position: AnnotationPosition,
    },
    UnsupportedSyntax {
        kind: AnnotationSyntaxKind,
        span: Span,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationPosition {
    Parameter,
    Return,
    Field,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Annotation {
    Name(String),
    Generic {
        base: Box<Annotation>,
        args: Vec<Annotation>,
    },
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
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterFact {
    pub name: String,
    pub kind: ParameterKind,
    pub has_default: bool,
    pub default_source: Option<String>,
    pub annotation: Annotation,
    pub span: Span,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationFact {
    pub name: String,
    pub kind: DeclarationKind,
    pub span: Span,
    pub bases: Vec<Annotation>,
    pub class_form: Option<ClassForm>,
    pub decorators: Vec<String>,
    pub is_async: bool,
    pub receiver: ReceiverKind,
    pub parameters: Vec<ParameterFact>,
    pub value_source: Option<String>,
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
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Index,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFacts {
    pub identity: String,
    pub span: Span,
    pub declarations: Vec<DeclarationFact>,
    pub occurrences: Vec<OccurrenceFact>,
    pub docstring: Option<DocstringFact>,
    pub annotations: Vec<AnnotationFact>,
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
    #[error("Ruff function header {start}..{end} has no Python delimiter")]
    MissingFunctionDelimiter { start: usize, end: usize },
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
            kind: DeclarationKind::Module,
            span: module_span,
            bases: Vec::new(),
            class_form: None,
            decorators: Vec::new(),
            is_async: false,
            receiver: ReceiverKind::Plain,
            parameters: Vec::new(),
            value_source: None,
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
    match expr {
        ast::Expr::Name(name) => Annotation::Name(name.id.as_str().to_owned()),
        ast::Expr::Attribute(_) => {
            qualified_name(expr).map_or_else(|| unsupported_annotation(expr), Annotation::Name)
        }
        ast::Expr::StringLiteral(literal) => {
            Annotation::StringLiteral(literal.value.to_str().to_owned())
        }
        ast::Expr::NoneLiteral(_) => Annotation::None,
        ast::Expr::BinOp(binary) if binary.op == ast::Operator::BitOr => {
            Annotation::Union(vec![annotation(&binary.left), annotation(&binary.right)])
        }
        ast::Expr::Subscript(subscript) => application(subscript),
        _ => unsupported_annotation(expr),
    }
}

/// Projects a typed subscription as either `Literal[...]` or a generic application.
fn application(subscript: &ast::ExprSubscript) -> Annotation {
    if terminal_name(&subscript.value) == Some("Literal") {
        Annotation::Literal(literal_arguments(&subscript.slice))
    } else {
        Annotation::Generic {
            base: Box::new(annotation(&subscript.value)),
            args: annotation_arguments(&subscript.slice),
        }
    }
}

/// Keeps tuple subscription arguments distinct without re-tokenizing source text.
fn annotation_arguments(slice: &ast::Expr) -> Vec<Annotation> {
    match slice {
        ast::Expr::Tuple(tuple) => tuple.elts.iter().map(annotation).collect(),
        expression => vec![annotation(expression)],
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

    /// Preserves each decorator spelling after validating its parser-owned range.
    fn decorators(&mut self, decorators: &[ast::Decorator]) -> Option<Vec<String>> {
        let mut values = Vec::with_capacity(decorators.len());
        for decorator in decorators {
            let source = self.source_owned(decorator.range)?;
            values.push(source.trim_start_matches('@').to_owned());
        }
        Some(values)
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
        let annotation_value = item.annotation.as_deref().map_or(
            Annotation::Unknown(TypeReason::Unannotated {
                position: AnnotationPosition::Parameter,
            }),
            annotation,
        );
        let fact = ParameterFact {
            name: item.name.as_str().to_owned(),
            kind,
            has_default: default.is_some(),
            default_source: self.optional_source(default)?,
            annotation: annotation_value.clone(),
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
    fn class_form(&self, class: &ast::StmtClassDef) -> (Vec<Annotation>, ClassForm) {
        let (mut bases, mut form) = (Vec::new(), ClassForm::Plain);
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
        }
        if class
            .decorator_list
            .iter()
            .any(|decorator| terminal_name(&decorator.expression) == Some("dataclass"))
        {
            form = ClassForm::Dataclass;
        }
        (bases, form)
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
                let Some(decorators) = self.decorators(&function.decorator_list) else {
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
                    kind: DeclarationKind::Function,
                    span: declaration_span,
                    bases: Vec::new(),
                    class_form: None,
                    decorators,
                    is_async: function.is_async,
                    receiver,
                    parameters,
                    value_source: None,
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
                let (bases, form) = self.class_form(class);
                let Some(decorators) = self.decorators(&class.decorator_list) else {
                    return;
                };
                let Some(docstring) = self.docstring(&class.body) else {
                    return;
                };
                self.add_declaration(DeclarationFact {
                    name: name.clone(),
                    kind: DeclarationKind::Class,
                    span: span(class.range),
                    bases,
                    class_form: Some(form),
                    decorators,
                    is_async: false,
                    receiver: ReceiverKind::Plain,
                    parameters: Vec::new(),
                    value_source: None,
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
    fn add_alias(&mut self, alias: &ast::Alias, statement: &ast::Stmt) {
        let binding = alias_binding(alias);
        self.add_declaration(DeclarationFact {
            name: binding,
            kind: DeclarationKind::Alias,
            span: span(statement.range()),
            bases: Vec::new(),
            class_form: None,
            decorators: Vec::new(),
            is_async: false,
            receiver: ReceiverKind::Plain,
            parameters: Vec::new(),
            value_source: Some(alias.name.as_str().to_owned()),
            docstring: None,
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
                kind: DeclarationKind::Field,
                span: span(statement.range()),
                bases: Vec::new(),
                class_form: None,
                decorators: Vec::new(),
                is_async: false,
                receiver: ReceiverKind::Plain,
                parameters: Vec::new(),
                value_source,
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
                kind: DeclarationKind::Constant,
                span: span(statement.range()),
                bases: Vec::new(),
                class_form: None,
                decorators: Vec::new(),
                is_async: false,
                receiver: ReceiverKind::Plain,
                parameters: Vec::new(),
                value_source,
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

    use super::{ExtractionError, ModuleFacts, Projection, Span};

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("expected an invalid AST source range")]
        ExpectedInvalidRange,
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
}
