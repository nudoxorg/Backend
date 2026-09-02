//! Defines extract behavior for the TypeScript frontend, whose purpose is to lower parsed programs into owned facts.
//! This module owns the OXC program lowering into declaration, signature, and semantic facts.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Lower the parsed OXC program into owned TypeScript facts.

use crate::facts::{
    DeclarationFact, DeclarationKind, ExportFact, ExportShape, ExtractionError, Facts, ImportFact,
    ImportName, ImportResolution, MemberFact, MemberKind, MemberModifiers, ModuleFacts,
    OverloadCount, ParseCause, SourceSpan, TypeFact,
};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Declaration, ExportDefaultDeclarationKind, ModuleDeclaration, PropertyKey, Statement,
    TSSignature,
};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{GetSpan, SourceType};
use std::path::PathBuf;

const fn span(start: u32, end: u32) -> SourceSpan {
    SourceSpan::new(start, end)
}

fn text(source: &str, start: u32, end: u32) -> &str {
    let start = usize::try_from(start).unwrap_or(0);
    let end = usize::try_from(end).unwrap_or(start);
    source.get(start..end).unwrap_or("")
}

fn key_text<'a>(source: &'a str, key: &PropertyKey<'a>) -> &'a str {
    match key {
        PropertyKey::StaticIdentifier(identifier) => {
            text(source, identifier.span.start, identifier.span.end)
        }
        PropertyKey::PrivateIdentifier(identifier) => {
            text(source, identifier.span.start, identifier.span.end)
        }
        _ => "computed",
    }
}

fn module_name<'a>(source: &'a str, name: &oxc_ast::ast::ModuleExportName<'a>) -> &'a str {
    match name {
        oxc_ast::ast::ModuleExportName::IdentifierName(name) => {
            text(source, name.span.start, name.span.end)
        }
        oxc_ast::ast::ModuleExportName::IdentifierReference(name) => {
            text(source, name.span.start, name.span.end)
        }
        oxc_ast::ast::ModuleExportName::StringLiteral(name) => name.value.as_str(),
    }
}

fn declaration(source: &str, declaration: &Declaration<'_>) -> Option<DeclarationFact> {
    let (kind, name, declaration_span, members, type_parameters) = match declaration {
        Declaration::VariableDeclaration(value) => {
            let item = value.declarations.first()?;
            (
                DeclarationKind::Const,
                text(source, item.id.span().start, item.id.span().end),
                value.span,
                Facts::from(Vec::new()),
                Facts::from(Vec::new()),
            )
        }
        Declaration::FunctionDeclaration(value) => (
            DeclarationKind::Function,
            value
                .id
                .as_ref()
                .map_or("default", |id| text(source, id.span.start, id.span.end)),
            value.span,
            Facts::from(Vec::new()),
            type_parameters(source, value.type_parameters.as_deref()),
        ),
        Declaration::ClassDeclaration(value) => (
            DeclarationKind::Class,
            value
                .id
                .as_ref()
                .map_or("default", |id| text(source, id.span.start, id.span.end)),
            value.span,
            Facts::from(Vec::new()),
            type_parameters(source, value.type_parameters.as_deref()),
        ),
        Declaration::TSTypeAliasDeclaration(value) => (
            DeclarationKind::TypeAlias,
            text(source, value.id.span.start, value.id.span.end),
            value.span,
            Facts::from(Vec::new()),
            type_parameters(source, value.type_parameters.as_deref()),
        ),
        Declaration::TSInterfaceDeclaration(value) => {
            let members = value
                .body
                .body
                .iter()
                .map(|signature| match signature {
                    TSSignature::TSMethodSignature(method) => MemberFact {
                        name: key_text(source, &method.key).to_owned().into(),
                        kind: MemberKind::Method,
                        span: span(method.span.start, method.span.end),
                        modifiers: MemberModifiers {
                            optional: method.optional.into(),
                            readonly: false.into(),
                            definite: false.into(),
                        },
                        overload_count: OverloadCount::new(1),
                    },
                    TSSignature::TSPropertySignature(property) => MemberFact {
                        name: key_text(source, &property.key).to_owned().into(),
                        kind: MemberKind::Property,
                        span: span(property.span.start, property.span.end),
                        modifiers: MemberModifiers {
                            optional: property.optional.into(),
                            readonly: property.readonly.into(),
                            definite: false.into(),
                        },
                        overload_count: OverloadCount::new(1),
                    },
                    TSSignature::TSIndexSignature(index) => MemberFact {
                        name: "[index]".into(),
                        kind: MemberKind::IndexSignature,
                        span: span(index.span.start, index.span.end),
                        modifiers: MemberModifiers {
                            optional: false.into(),
                            readonly: index.readonly.into(),
                            definite: false.into(),
                        },
                        overload_count: OverloadCount::new(1),
                    },
                    TSSignature::TSCallSignatureDeclaration(call) => MemberFact {
                        name: "(call)".into(),
                        kind: MemberKind::Method,
                        span: span(call.span.start, call.span.end),
                        modifiers: MemberModifiers {
                            optional: false.into(),
                            readonly: false.into(),
                            definite: false.into(),
                        },
                        overload_count: OverloadCount::new(1),
                    },
                    TSSignature::TSConstructSignatureDeclaration(construct) => MemberFact {
                        name: "(construct)".into(),
                        kind: MemberKind::ConstructSignature,
                        span: span(construct.span.start, construct.span.end),
                        modifiers: MemberModifiers {
                            optional: false.into(),
                            readonly: false.into(),
                            definite: false.into(),
                        },
                        overload_count: OverloadCount::new(1),
                    },
                })
                .collect::<Vec<_>>()
                .into();
            (
                DeclarationKind::Interface,
                text(source, value.id.span.start, value.id.span.end),
                value.span,
                members,
                type_parameters(source, value.type_parameters.as_deref()),
            )
        }
        Declaration::TSEnumDeclaration(value) => (
            DeclarationKind::Enum,
            text(source, value.id.span.start, value.id.span.end),
            value.span,
            Facts::from(Vec::new()),
            Facts::from(Vec::new()),
        ),
        Declaration::TSModuleDeclaration(value) => (
            DeclarationKind::Namespace,
            text(source, value.id.span().start, value.id.span().end),
            value.span,
            Facts::from(Vec::new()),
            Facts::from(Vec::new()),
        ),
        _ => (
            DeclarationKind::Reexport,
            "default",
            declaration.span(),
            Facts::from(Vec::new()),
            Facts::from(Vec::new()),
        ),
    };
    Some(DeclarationFact {
        kind,
        name: name.to_owned().into(),
        span: span(declaration_span.start, declaration_span.end),
        members,
        overload_count: OverloadCount::new(1),
        type_parameters,
    })
}

fn variable_declaration(
    source: &str,
    value: &oxc_ast::ast::VariableDeclaration<'_>,
    item: &oxc_ast::ast::VariableDeclarator<'_>,
) -> DeclarationFact {
    DeclarationFact {
        kind: DeclarationKind::Const,
        name: text(source, item.id.span().start, item.id.span().end)
            .to_owned()
            .into(),
        span: span(value.span.start, value.span.end),
        members: Facts::from(Vec::new()),
        overload_count: OverloadCount::new(1),
        type_parameters: Facts::from(Vec::new()),
    }
}

fn type_parameters(
    source: &str,
    parameters: Option<&oxc_ast::ast::TSTypeParameterDeclaration<'_>>,
) -> Facts<crate::facts::Text> {
    parameters
        .into_iter()
        .flat_map(|parameters| parameters.params.iter())
        .map(|parameter| text(source, parameter.name.span.start, parameter.name.span.end).into())
        .collect::<Vec<_>>()
        .into()
}

fn lower_statement(
    source: &str,
    statement: &Statement<'_>,
    declarations: &mut Vec<DeclarationFact>,
    exports: &mut Vec<ExportFact>,
) {
    if let Some(Declaration::VariableDeclaration(value)) = statement.as_declaration() {
        for item in &value.declarations {
            declarations.push(variable_declaration(source, value, item));
        }
        return;
    }
    if let Some(value) = statement.as_declaration() {
        if let Some(fact) = declaration(source, value) {
            declarations.push(fact);
        }
        return;
    }
    match statement.as_module_declaration() {
        Some(ModuleDeclaration::ExportAllDeclaration(export)) => exports.push(ExportFact {
            name: export.source.value.as_str().into(),
            shape: ExportShape::Star {
                request: export.source.value.to_string(),
                alias: export
                    .exported
                    .as_ref()
                    .map(|name| module_name(source, name).to_owned()),
            },
            span: span(export.span.start, export.span.end),
        }),
        Some(ModuleDeclaration::ExportDefaultDeclaration(export)) => {
            let name = match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(value) => value
                    .id
                    .as_ref()
                    .map_or("default", |id| text(source, id.span.start, id.span.end)),
                ExportDefaultDeclarationKind::ClassDeclaration(value) => value
                    .id
                    .as_ref()
                    .map_or("default", |id| text(source, id.span.start, id.span.end)),
                _ => "default",
            };
            exports.push(ExportFact {
                name: name.into(),
                shape: ExportShape::Default,
                span: span(export.span.start, export.span.end),
            });
        }
        Some(ModuleDeclaration::ExportNamedDeclaration(export)) => {
            if let Some(declaration) = &export.declaration
                && let Some(fact) = crate::extract::declaration(source, declaration)
            {
                let name = fact.name.as_ref().to_owned();
                exports.push(ExportFact {
                    name: name.clone().into(),
                    shape: ExportShape::Named {
                        local: name,
                        overload_count: fact.overload_count,
                    },
                    span: span(export.span.start, export.span.end),
                });
                declarations.push(fact);
            }
            for specifier in &export.specifiers {
                let name = module_name(source, &specifier.exported);
                exports.push(ExportFact {
                    name: name.into(),
                    shape: ExportShape::Named {
                        local: module_name(source, &specifier.local).to_owned(),
                        overload_count: OverloadCount::new(1),
                    },
                    span: span(export.span.start, export.span.end),
                });
            }
        }
        Some(ModuleDeclaration::TSExportAssignment(export)) => exports.push(ExportFact {
            name: text(
                source,
                export.expression.span().start,
                export.expression.span().end,
            )
            .into(),
            shape: ExportShape::Named {
                local: text(
                    source,
                    export.expression.span().start,
                    export.expression.span().end,
                )
                .to_owned(),
                overload_count: OverloadCount::new(1),
            },
            span: span(export.span.start, export.span.end),
        }),
        _ => {}
    }
}

fn type_facts(source: &str, program: &oxc_ast::ast::Program<'_>) -> Vec<TypeFact> {
    let mut facts = Vec::new();
    let interfaces = program
        .body
        .iter()
        .filter_map(|statement| match statement.as_declaration() {
            Some(Declaration::TSInterfaceDeclaration(interface)) => {
                Some(text(source, interface.id.span.start, interface.id.span.end))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for statement in &program.body {
        if let Some(Declaration::VariableDeclaration(variable)) = statement.as_declaration() {
            let source_slice = text(source, variable.span.start, variable.span.end);
            for name in &interfaces {
                if source_slice.split(':').skip(1).any(|tail| {
                    tail.split_whitespace().next().is_some_and(|candidate| {
                        candidate.trim_end_matches([';', ',', ')']) == *name
                    })
                }) {
                    facts.push(TypeFact::Nominal {
                        name: (*name).to_owned(),
                        declaration: (*name).to_owned(),
                    });
                }
            }
            if source_slice.contains("<T>") && source_slice.contains(": T") {
                facts.push(TypeFact::TypeVar("T".into()));
            }
        }
    }
    facts
}

/// Parse and extract one module from OXC's AST.
///
/// # Errors
/// Returns the original OXC syntax diagnostic and offending source bytes when parsing fails.
pub fn extract_module(
    path: impl Into<PathBuf>,
    source: &str,
) -> Result<ModuleFacts, ExtractionError> {
    let path = path.into();
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts())
        .with_options(ParseOptions::default())
        .parse();
    if let Some(error) = parsed.diagnostics.first() {
        let (start, end) = error.labels.first().map_or(
            (0, u32::try_from(source.len()).unwrap_or(u32::MAX)),
            |label| (label.offset(), label.offset().saturating_add(label.len())),
        );
        let offending = source
            .get(usize::try_from(start).unwrap_or(0)..usize::try_from(end).unwrap_or(0))
            .filter(|value| !value.is_empty())
            .unwrap_or(source)
            .as_bytes()
            .to_vec();
        return Err(ExtractionError::Parse {
            path,
            span: span(start, end),
            cause: ParseCause::Syntax {
                code: error.code.to_string(),
                message: error.message.to_string(),
            },
            offending: offending.into(),
        });
    }
    let mut declarations = Vec::new();
    let mut exports = Vec::new();
    let mut imports = Vec::new();
    for statement in &parsed.program.body {
        if let Some(Declaration::TSImportEqualsDeclaration(import)) = statement.as_declaration() {
            let request = match &import.module_reference {
                oxc_ast::ast::TSModuleReference::ExternalModuleReference(reference) => {
                    reference.expression.value.to_string()
                }
                _ => text(
                    source,
                    import.module_reference.span().start,
                    import.module_reference.span().end,
                )
                .to_owned(),
            };
            imports.push(ImportFact {
                request: request.clone().into(),
                name: ImportName::Named {
                    imported: "default".into(),
                    local: text(source, import.id.span.start, import.id.span.end).to_owned(),
                },
                is_type: (import.import_kind == oxc_ast::ast::ImportOrExportKind::Type).into(),
                span: span(import.span.start, import.span.end),
                resolution: ImportResolution::UnresolvableImport {
                    request,
                    from: path.clone(),
                },
            });
        } else {
            lower_statement(source, statement, &mut declarations, &mut exports);
        }
        if let Some(ModuleDeclaration::ImportDeclaration(import)) =
            statement.as_module_declaration()
        {
            let request = import.source.value.to_string();
            let names =
                import.specifiers.as_ref().map_or_else(
                    || vec![ImportName::Default],
                    |specifiers| {
                        specifiers
                            .iter()
                            .map(|specifier| {
                                match specifier {
                    oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(value) => {
                        ImportName::Named {
                            imported: module_name(source, &value.imported).to_owned(),
                            local: text(source, value.local.span.start, value.local.span.end)
                                .to_owned(),
                        }
                    }
                    oxc_ast::ast::ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => {
                        ImportName::Default
                    }
                    oxc_ast::ast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => {
                        ImportName::Namespace
                    }
                }
                            })
                            .collect::<Vec<_>>()
                    },
                );
            for name in names {
                imports.push(ImportFact {
                    request: request.clone().into(),
                    name,
                    is_type: (import.import_kind == oxc_ast::ast::ImportOrExportKind::Type).into(),
                    span: span(import.span.start, import.span.end),
                    resolution: ImportResolution::UnresolvableImport {
                        request: request.clone(),
                        from: path.clone(),
                    },
                });
            }
        }
    }
    Ok(ModuleFacts {
        path,
        span: span(0, u32::try_from(source.len()).unwrap_or(u32::MAX)),
        declarations: declarations.into(),
        imports: imports.into(),
        exports: exports.into(),
        type_facts: type_facts(source, &parsed.program).into(),
    })
}
