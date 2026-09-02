//! Defines the Python semantic frontend, whose purpose is to project typed facts from the caller's source bytes.
//! This module owns source-preserving declaration, annotation, and occurrence extraction over Ruff's typed AST.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Source-preserving Python facts projected from Ruff's typed AST.

use ruff_python_ast::{
    self as ast,
    visitor::{self, Visitor},
};
use ruff_text_size::Ranged;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeReason {
    Unannotated { position: AnnotationPosition },
    NoIrRepresentation { spelling: String },
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
    Literal(String),
    None,
    Unknown(TypeReason),
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

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExtractionError {
    #[error("source is not UTF-8 at {span:?}")]
    InvalidUtf8 {
        source_bytes: Vec<u8>,
        span: Span,
        bytes: Vec<u8>,
    },
    #[error("Python syntax error at {span:?}: {message}")]
    Syntax {
        source_bytes: Vec<u8>,
        span: Span,
        bytes: Vec<u8>,
        message: String,
    },
}

const MODULE_IDENTITY: &str = "__main__";

pub fn extract(source: &[u8]) -> Result<ModuleFacts, ExtractionError> {
    let text = std::str::from_utf8(source).map_err(|error| {
        let start = error.valid_up_to();
        let end = error
            .error_len()
            .map_or(source.len(), |len| start.saturating_add(len));
        ExtractionError::InvalidUtf8 {
            source_bytes: source.to_vec(),
            span: Span {
                start: checked_u32(start),
                end: checked_u32(end),
            },
            bytes: source[start..end].to_vec(),
        }
    })?;
    let parsed = ruff_python_parser::parse_module(text).map_err(|error| {
        let range = error.location;
        let start = range.start().to_usize();
        let end = range.end().to_usize().min(source.len());
        let operand_start = if start == source.len() {
            start.saturating_sub(1)
        } else {
            start
        };
        let operand_end = end.max(start.saturating_add(1)).min(source.len());
        ExtractionError::Syntax {
            source_bytes: source.to_vec(),
            span: Span {
                start: checked_u32(start),
                end: checked_u32(end),
            },
            bytes: source[operand_start..operand_end].to_vec(),
            message: error.to_string(),
        }
    })?;
    let syntax = parsed.syntax();
    let mut names = FunctionNames::default();
    for statement in &syntax.body {
        names.visit_stmt(statement);
    }
    let module_span = span(syntax.range);
    let module_doc = docstring(&syntax.body, text);
    let mut facts = ModuleFacts {
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
    };
    let mut projection = Projection {
        text,
        names: &names.0,
        facts: &mut facts,
        owner: MODULE_IDENTITY.to_owned(),
        class_depth: 0,
        function_depth: 0,
        decorator_ranges: Vec::new(),
        decorator_owner: None,
    };
    for statement in &syntax.body {
        projection.visit_stmt(statement);
    }
    Ok(facts)
}

fn checked_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
fn span(range: ruff_text_size::TextRange) -> Span {
    Span {
        start: range.start().to_u32(),
        end: range.end().to_u32(),
    }
}
fn source(text: &str, range: ruff_text_size::TextRange) -> &str {
    let slice = text.get(range.start().to_usize()..range.end().to_usize());
    debug_assert!(slice.is_some(), "Ruff returned a non-source range");
    slice.unwrap_or_else(|| {
        debug_assert!(false, "Ruff returned a non-source range");
        ""
    })
}

fn annotation(expr: &ast::Expr, text: &str) -> Annotation {
    parse_annotation(source(text, expr.range()))
}
fn parse_annotation(raw: &str) -> Annotation {
    let s = raw.trim();
    if s == "None" {
        return Annotation::None;
    }
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        return Annotation::StringLiteral(s.to_owned());
    }
    if let Some((a, b)) = s.split_once('|') {
        return Annotation::Union(vec![parse_annotation(a), parse_annotation(b)]);
    }
    if let Some(open) = s.find('[').filter(|_| s.ends_with(']')) {
        return Annotation::Generic {
            base: Box::new(parse_annotation(&s[..open])),
            args: s[open + 1..s.len() - 1]
                .split(',')
                .map(parse_annotation)
                .collect(),
        };
    }
    if s.chars()
        .all(|c| c == '_' || c.is_ascii_alphanumeric() || c == '.')
    {
        Annotation::Name(s.to_owned())
    } else {
        Annotation::Unknown(TypeReason::NoIrRepresentation {
            spelling: s.to_owned(),
        })
    }
}

fn docstring(body: &[ast::Stmt], text: &str) -> Option<DocstringFact> {
    match body.first()? {
        ast::Stmt::Expr(statement) => match statement.value.as_ref() {
            ast::Expr::StringLiteral(literal) => Some(DocstringFact {
                raw: source(text, literal.range).to_owned(),
                span: span(literal.range),
            }),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Default)]
struct FunctionNames(Vec<String>);
impl<'a> Visitor<'a> for FunctionNames {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement {
            self.0.push(function.name.as_str().to_owned());
        }
        visitor::walk_stmt(self, statement);
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
}
impl<'a> Projection<'a> {
    fn add_declaration(&mut self, declaration: DeclarationFact) {
        self.facts.declarations.push(declaration);
    }
    fn parameter(
        &mut self,
        owner: &str,
        item: &ast::Parameter,
        default: Option<&ast::Expr>,
        kind: ParameterKind,
    ) -> ParameterFact {
        let annotation_value = item.annotation.as_deref().map_or(
            Annotation::Unknown(TypeReason::Unannotated {
                position: AnnotationPosition::Parameter,
            }),
            |x| annotation(x, self.text),
        );
        let fact = ParameterFact {
            name: item.name.as_str().to_owned(),
            kind,
            has_default: default.is_some(),
            default_source: default.map(|x| source(self.text, x.range()).to_owned()),
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
        fact
    }
    fn params(&mut self, function: &ast::StmtFunctionDef) -> Vec<ParameterFact> {
        let mut result = Vec::new();
        for item in &function.parameters.posonlyargs {
            result.push(self.parameter(
                function.name.as_str(),
                &item.parameter,
                item.default.as_deref(),
                ParameterKind::PositionalOnly,
            ));
        }
        for item in &function.parameters.args {
            result.push(self.parameter(
                function.name.as_str(),
                &item.parameter,
                item.default.as_deref(),
                ParameterKind::PositionalOrKeyword,
            ));
        }
        if let Some(item) = function.parameters.vararg.as_deref() {
            result.push(self.parameter(function.name.as_str(), item, None, ParameterKind::VarArgs));
        }
        for item in &function.parameters.kwonlyargs {
            result.push(self.parameter(
                function.name.as_str(),
                &item.parameter,
                item.default.as_deref(),
                ParameterKind::KeywordOnly,
            ));
        }
        if let Some(item) = function.parameters.kwarg.as_deref() {
            result.push(self.parameter(function.name.as_str(), item, None, ParameterKind::KwArgs));
        }
        if let Some(returns) = function.returns.as_deref() {
            self.facts.annotations.push(AnnotationFact {
                owner: function.name.as_str().to_owned(),
                position: AnnotationPosition::Return,
                annotation: annotation(returns, self.text),
                span: span(returns.range()),
            });
        }
        result
    }
    fn class_form(&self, class: &ast::StmtClassDef) -> (Vec<Annotation>, ClassForm) {
        let (mut bases, mut form) = (Vec::new(), ClassForm::Plain);
        if let Some(arguments) = class.arguments.as_deref() {
            for base in &arguments.args {
                let spelling = source(self.text, base.range());
                let final_name = spelling.rsplit('.').next().unwrap_or(spelling);
                if final_name == "Protocol" {
                    form = ClassForm::Protocol;
                } else if final_name == "TypedDict" {
                    form = ClassForm::TypedDict;
                } else if final_name == "Enum" {
                    form = ClassForm::Enum;
                }
                bases.push(annotation(base, self.text));
            }
        }
        if class.decorator_list.iter().any(|d| {
            let spelling = source(self.text, d.range)
                .trim()
                .trim_start_matches('@')
                .trim_end_matches("()");
            spelling.rsplit('.').next() == Some("dataclass")
        }) {
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
                let parameters = self.params(function);
                let decorators: Vec<String> = function
                    .decorator_list
                    .iter()
                    .map(|d| {
                        source(self.text, d.range)
                            .trim_start_matches('@')
                            .to_owned()
                    })
                    .collect();
                let receiver = if decorators.iter().any(|d| d == "staticmethod") {
                    ReceiverKind::StaticMethod
                } else if decorators.iter().any(|d| d == "classmethod") {
                    ReceiverKind::ClassMethod
                } else if decorators.iter().any(|d| d == "property") {
                    ReceiverKind::Property
                } else {
                    ReceiverKind::Plain
                };
                let doc = docstring(&function.body, self.text);
                let header_start = function.range.start().to_usize();
                let search_start = function
                    .returns
                    .as_deref()
                    .map_or(function.parameters.range.end().to_usize(), |returns| {
                        returns.range().end().to_usize()
                    });
                let header_end = self
                    .text
                    .get(search_start..function.range.end().to_usize())
                    .and_then(|tail| tail.find(':'))
                    .map_or(search_start, |offset| search_start + offset + 1);
                let declaration_span = Span {
                    start: checked_u32(header_start),
                    end: checked_u32(header_end),
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
                let decorators = class
                    .decorator_list
                    .iter()
                    .map(|d| {
                        source(self.text, d.range)
                            .trim_start_matches('@')
                            .to_owned()
                    })
                    .collect();
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
                    docstring: docstring(&class.body, self.text),
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
                    self.field_from_target(target, Some(assign.value.as_ref()), statement);
                }
            }
            ast::Stmt::Assign(assign) if self.class_depth == 0 && self.function_depth == 0 => {
                if let Some(target) = assign.targets.first() {
                    self.constant_from_target(target, Some(assign.value.as_ref()), None, statement);
                }
            }
            ast::Stmt::AnnAssign(assign) if self.class_depth > 0 && self.function_depth == 0 => {
                self.field_from_target(&assign.target, assign.value.as_deref(), statement)
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
                    self.decorator_owner.as_deref().unwrap_or(&self.owner)
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
        let binding = alias.asname.as_ref().map_or_else(
            || {
                alias
                    .name
                    .as_str()
                    .rsplit('.')
                    .next()
                    .unwrap_or(alias.name.as_str())
                    .to_owned()
            },
            |x| x.as_str().to_owned(),
        );
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
        statement: &ast::Stmt,
    ) {
        if let ast::Expr::Name(name) = target {
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
                value_source: value.map(|x| source(self.text, x.range()).to_owned()),
                docstring: None,
            });
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
                value_source: value.map(|x| source(self.text, x.range()).to_owned()),
                docstring: None,
            });
            if let Some(annotation_expr) = annotation_expr {
                self.facts.annotations.push(AnnotationFact {
                    owner: name.id.as_str().to_owned(),
                    position: AnnotationPosition::Field,
                    annotation: annotation(annotation_expr, self.text),
                    span: span(annotation_expr.range()),
                });
            }
        }
    }
}
