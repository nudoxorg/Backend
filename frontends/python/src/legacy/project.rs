//! Walks one Ruff module into syntax-derived declaration and occurrence facts.

use super::*;

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

    /// Copies an AST-selected source range only after a checked bounds proof.
    pub(super) fn source_owned(&mut self, range: ruff_text_size::TextRange) -> Option<String> {
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
                self.owner = name;
                self.enclosing_class = Some(self.owner.clone());
                self.class_depth += 1;
                visitor::walk_stmt(self, statement);
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
                    let receiver = match attribute.value.as_ref() {
                        ast::Expr::Name(name)
                            if matches!(name.id.as_str(), "self" | "cls")
                                && self.enclosing_class.is_some() =>
                        {
                            OccurrenceReceiver::EnclosingClass {
                                class: self
                                    .enclosing_class
                                    .clone()
                                    .expect("enclosing class proven above"),
                            }
                        }
                        ast::Expr::Name(name) => OccurrenceReceiver::Foreign {
                            receiver: Some(name.id.as_str().to_owned()),
                        },
                        _ => OccurrenceReceiver::Foreign { receiver: None },
                    };
                    let gated = self.names.iter().any(|declared| declared == target);
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
        }
    }
}
