//! Per-module declaration extraction: OXC AST → `DeclFact` / `ModuleFacts`.
//!
//! Rewritten against the new IR shapes. All OXC AST pattern-matching logic is
//! preserved from the old `oxc/extract/decl.rs`; only the emission target
//! changed from old `ir::*` to the new `DeclFact` / `TypeOwned` intermediates.
//!
//! # One-pass guarantee
//!
//! This module produces `Vec<DeclFact>`. The caller (`graph.rs`) stores them in
//! `ModuleFacts`. The emitter (`emit.rs`) drives `Lowering` in one pass. No
//! intermediate path→members map is built anywhere.
//!
//! # Declaration merging
//!
//! Same-named declarations (interface + namespace, or function overloads) are
//! assigned distinct `discriminant` values within the group. Each becomes its
//! own IR entry.

use std::path::Path;

use oxc_ast::ast::{
    AccessorPropertyType, AssignmentOperator, AssignmentTarget, BindingPattern, Class,
    ClassElement, Declaration, Expression, ExportDefaultDeclarationKind, Function,
    MethodDefinitionKind, MethodDefinitionType, PropertyDefinitionType, PropertyKey, Statement,
    TSAccessibility, TSEnumDeclaration, TSEnumMemberName, TSInterfaceDeclaration,
    TSModuleDeclaration, TSModuleDeclarationBody, TSModuleDeclarationName,
    TSSignature, VariableDeclaration, VariableDeclarationKind,
};
use oxc_semantic::Semantic;
use oxc_span::GetSpan;
use oxc_syntax::module_record::{
    ExportExportName, ExportImportName, ExportLocalName, ImportImportName, ModuleRecord,
};

use super::{
    jsdoc,
    types::{lower_ts_type, lower_type_params},
    Accessibility, AttrTok, ClassBody, ConstBody, DeclBody, DeclFact, DocFacts, EnumBody,
    ExportTable, FunctionBody, ImportFact, ImportName, IndirectExport, IndexSignatureFact,
    InterfaceBody, MemberFact, MemberKind, MemberModifiers, MethodFact, ModuleFacts, NamespaceBody,
    ParamFact, PropertyFact, ReceiverKind, StarExport, StaticBody, TypeAliasBody, VariantFact,
};

// ── Entry point ────────────────────────────────────────────────────────────────

/// Extract one module's declarations into owned `ModuleFacts`.
pub fn extract_module<'a>(
    source: &'a str,
    semantic: &'a Semantic<'a>,
    program: &'a oxc_ast::ast::Program<'a>,
    path: &Path,
    module_record: &ModuleRecord<'a>,
    module_name: String,
) -> ModuleFacts {
    let mut declarations: Vec<DeclFact> = Vec::new();
    let mut name_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    // ── Build exported-name set ───────────────────────────────────────────────
    let exported_names: std::collections::HashSet<String> = module_record
        .exported_bindings
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();

    let default_local_name: Option<String> = module_record
        .local_export_entries
        .iter()
        .find_map(|e| {
            if matches!(e.export_name, ExportExportName::Default(_)) {
                match &e.local_name {
                    ExportLocalName::Default(ns) => Some(ns.name.to_string()),
                    ExportLocalName::Name(ns) => Some(ns.name.to_string()),
                    ExportLocalName::Null => None,
                }
            } else {
                None
            }
        });

    // ── Walk program body ─────────────────────────────────────────────────────
    for stmt in program.body.iter() {
        let decls = extract_statement(
            stmt,
            source,
            semantic,
            path,
            &exported_names,
            &default_local_name,
            &mut name_counts,
        );
        declarations.extend(decls);
    }

    // ── Export table ──────────────────────────────────────────────────────────
    let exports = build_export_table(module_record, &exported_names, &default_local_name);

    // ── Import table ──────────────────────────────────────────────────────────
    let imports = build_import_table(module_record);

    // ── Module doc ────────────────────────────────────────────────────────────
    let module_doc = jsdoc::module_doc(semantic, program);

    ModuleFacts {
        path: path.to_path_buf(),
        module_name,
        module_doc,
        declarations,
        exports,
        imports,
    }
}

// ── Statement dispatch ─────────────────────────────────────────────────────────

fn extract_statement<'a>(
    stmt: &'a Statement<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    exported_names: &std::collections::HashSet<String>,
    default_local_name: &Option<String>,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    match stmt {
        Statement::ExportNamedDeclaration(exp) => {
            if let Some(decl) = &exp.declaration {
                return extract_declaration(
                    decl,
                    source,
                    semantic,
                    path,
                    true,
                    false,
                    name_counts,
                );
            }
            vec![]
        }

        Statement::ExportDefaultDeclaration(exp) => {
            extract_default_export(&exp.declaration, source, semantic, path, name_counts)
        }

        Statement::FunctionDeclaration(f) => {
            let decl = stmt.as_declaration().expect("FunctionDeclaration is a Declaration");
            let name = f.id.as_ref().map(|id| id.name.to_string());
            if let Some(name) = name {
                let is_exported = exported_names.contains(&name)
                    || default_local_name.as_deref() == Some(&name);
                extract_declaration(
                    decl,
                    source,
                    semantic,
                    path,
                    is_exported,
                    false,
                    name_counts,
                )
            } else {
                vec![]
            }
        }

        Statement::ClassDeclaration(c) => {
            let decl = stmt.as_declaration().expect("ClassDeclaration is a Declaration");
            let name = c.id.as_ref().map(|id| id.name.to_string());
            if let Some(name) = name {
                let is_exported = exported_names.contains(&name);
                extract_declaration(decl, source, semantic, path, is_exported, false, name_counts)
            } else {
                vec![]
            }
        }

        Statement::VariableDeclaration(_v) => {
            let decl = stmt.as_declaration().expect("VariableDeclaration is a Declaration");
            extract_declaration(decl, source, semantic, path, false, false, name_counts)
        }

        Statement::TSTypeAliasDeclaration(a) => {
            let decl = stmt.as_declaration().expect("TSTypeAlias is a Declaration");
            let is_exported = exported_names.contains(a.id.name.as_str());
            extract_declaration(decl, source, semantic, path, is_exported, false, name_counts)
        }

        Statement::TSInterfaceDeclaration(i) => {
            let decl = stmt.as_declaration().expect("TSInterface is a Declaration");
            let is_exported = exported_names.contains(i.id.name.as_str());
            extract_declaration(decl, source, semantic, path, is_exported, false, name_counts)
        }

        Statement::TSEnumDeclaration(e) => {
            let decl = stmt.as_declaration().expect("TSEnum is a Declaration");
            let is_exported = exported_names.contains(e.id.name.as_str());
            extract_declaration(decl, source, semantic, path, is_exported, false, name_counts)
        }

        Statement::TSModuleDeclaration(m) => {
            let decl = stmt.as_declaration().expect("TSModule is a Declaration");
            let sym_name = match &m.id {
                TSModuleDeclarationName::Identifier(id) => id.name.to_string(),
                TSModuleDeclarationName::StringLiteral(s) => s.value.to_string(),
            };
            let is_string_literal = matches!(&m.id, TSModuleDeclarationName::StringLiteral(_));
            let is_exported =
                exported_names.contains(&sym_name) || m.declare || is_string_literal;
            extract_declaration(decl, source, semantic, path, is_exported, false, name_counts)
        }

        _ => vec![],
    }
}

// ── Declaration dispatch ───────────────────────────────────────────────────────

fn extract_declaration<'a>(
    decl: &'a Declaration<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    is_exported: bool,
    is_default: bool,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;

    let visibility = if is_exported { Visibility::Public } else { Visibility::Private };

    match decl {
        Declaration::FunctionDeclaration(f) => {
            let Some(name) = f.id.as_ref().map(|id| id.name.to_string()) else {
                return vec![];
            };
            let span = f.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            if doc.ignore {
                return vec![];
            }
            let discriminant = bump_count(&name, name_counts);
            let body = lower_function(f, source);
            vec![DeclFact {
                name,
                visibility,
                doc,
                body: DeclBody::Function(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default,
                decl_index: discriminant,
            }]
        }

        Declaration::ClassDeclaration(c) => {
            let Some(name) = c.id.as_ref().map(|id| id.name.to_string()) else {
                return vec![];
            };
            let span = c.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            if doc.ignore {
                return vec![];
            }
            let discriminant = bump_count(&name, name_counts);
            let body = lower_class(c, source);
            vec![DeclFact {
                name,
                visibility,
                doc,
                body: DeclBody::Class(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default,
                decl_index: discriminant,
            }]
        }

        Declaration::VariableDeclaration(v) => {
            lower_variable(v, source, semantic, path, is_exported, name_counts)
        }

        Declaration::TSTypeAliasDeclaration(a) => {
            let name = a.id.name.to_string();
            let span = a.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            if doc.ignore {
                return vec![];
            }
            let discriminant = bump_count(&name, name_counts);
            let generics = a
                .type_parameters
                .as_ref()
                .map(|tp| lower_type_params(tp, source))
                .unwrap_or_default();
            let target = lower_ts_type(&a.type_annotation, source);
            vec![DeclFact {
                name,
                visibility,
                doc,
                body: DeclBody::TypeAlias(TypeAliasBody { generics, target }),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default,
                decl_index: discriminant,
            }]
        }

        Declaration::TSInterfaceDeclaration(i) => {
            let name = i.id.name.to_string();
            let span = i.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            if doc.ignore {
                return vec![];
            }
            let discriminant = bump_count(&name, name_counts);
            let body = lower_interface(i, source, semantic);
            vec![DeclFact {
                name,
                visibility,
                doc,
                body: DeclBody::Interface(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default,
                decl_index: discriminant,
            }]
        }

        Declaration::TSEnumDeclaration(e) => {
            let name = e.id.name.to_string();
            let span = e.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            if doc.ignore {
                return vec![];
            }
            let discriminant = bump_count(&name, name_counts);
            let body = lower_enum(e);
            vec![DeclFact {
                name,
                visibility,
                doc,
                body: DeclBody::Enum(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default,
                decl_index: discriminant,
            }]
        }

        Declaration::TSModuleDeclaration(m) => {
            lower_namespace(m, source, semantic, path, is_exported, is_default, name_counts)
        }

        _ => vec![],
    }
}

// ── Default export ─────────────────────────────────────────────────────────────

fn extract_default_export<'a>(
    kind: &'a ExportDefaultDeclarationKind<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;
    match kind {
        ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
            let name = f.id.as_ref().map(|id| id.name.to_string()).unwrap_or_else(|| "default".to_string());
            let span = f.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            let discriminant = bump_count(&name, name_counts);
            let body = lower_function(f, source);
            vec![DeclFact {
                name,
                visibility: Visibility::Public,
                doc,
                body: DeclBody::Function(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: true,
                decl_index: discriminant,
            }]
        }
        ExportDefaultDeclarationKind::ClassDeclaration(c) => {
            let name = c.id.as_ref().map(|id| id.name.to_string()).unwrap_or_else(|| "default".to_string());
            let span = c.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            let discriminant = bump_count(&name, name_counts);
            let body = lower_class(c, source);
            vec![DeclFact {
                name,
                visibility: Visibility::Public,
                doc,
                body: DeclBody::Class(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: true,
                decl_index: discriminant,
            }]
        }
        ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
            let name = i.id.name.to_string();
            let span = i.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            let discriminant = bump_count(&name, name_counts);
            let body = lower_interface(i, source, semantic);
            vec![DeclFact {
                name,
                visibility: Visibility::Public,
                doc,
                body: DeclBody::Interface(body),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: true,
                decl_index: discriminant,
            }]
        }
        _ => vec![],
    }
}

// ── Kind-specific lowering ─────────────────────────────────────────────────────

fn lower_function<'a>(f: &Function<'a>, source: &'a str) -> FunctionBody {
    let has_body = f.body.is_some();

    // Detect `this` pseudo-parameter → shared receiver.
    let first_param_is_this = f
        .params
        .items
        .first()
        .and_then(|p| match &p.pattern {
            BindingPattern::BindingIdentifier(id) => Some(id.name.as_str() == "this"),
            _ => None,
        })
        .unwrap_or(false);

    let receiver = if first_param_is_this {
        ReceiverKind::SharedRef
    } else {
        ReceiverKind::None
    };

    let params = lower_formal_parameters(&f.params, source, first_param_is_this);

    let return_type = f
        .return_type
        .as_ref()
        .map(|ann| lower_ts_type(&ann.type_annotation, source));

    let generics = f
        .type_parameters
        .as_ref()
        .map(|tp| lower_type_params(tp, source))
        .unwrap_or_default();

    let is_async = f.r#async;
    let is_generator = f.generator;

    FunctionBody { generics, params, return_type, is_async, is_generator, has_body, receiver }
}

fn lower_formal_parameters<'a>(
    params: &oxc_ast::ast::FormalParameters<'a>,
    source: &'a str,
    skip_first: bool,
) -> Vec<ParamFact> {
    let items = if skip_first && !params.items.is_empty() {
        &params.items[1..]
    } else {
        &params.items
    };

    let mut out: Vec<ParamFact> = Vec::with_capacity(items.len() + 1);
    for param in items {
        let name = binding_pattern_name(&param.pattern)
            .unwrap_or_else(|| "_".to_string());
        let ty = param
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type(&ann.type_annotation, source));
        let is_readonly = param.readonly;
        out.push(ParamFact {
            name,
            ty,
            is_optional: param.optional,
            is_rest: false,
            is_readonly,
        });
    }
    if let Some(rest) = &params.rest {
        let ty = rest
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type(&ann.type_annotation, source));
        let name = binding_pattern_name(&rest.rest.argument)
            .unwrap_or_else(|| "...rest".to_string());
        out.push(ParamFact { name, ty, is_optional: false, is_rest: true, is_readonly: false });
    }
    out
}

fn lower_class<'a>(cls: &Class<'a>, source: &'a str) -> ClassBody {
    let generics = cls
        .type_parameters
        .as_ref()
        .map(|tp| lower_type_params(tp, source))
        .unwrap_or_default();

    // `super_class` is an Expression (runtime value); extract its identifier name.
    // `super_type_parameters` carries the type arguments in 0.139.0.
    // UNCERTAINTY: In OXC 0.138.x the field was `super_type_arguments`;
    // in 0.139.0 it may be `super_type_parameters`. We use whichever compiles.
    let extends: Vec<super::TypeOwned> = cls
        .super_class
        .as_ref()
        .map(|expr| {
            let name = match expr {
                Expression::Identifier(id) => id.name.to_string(),
                other => other.span().source_text(source).to_string(),
            };
            let args: Vec<super::TypeOwned> = cls
                .super_type_arguments
                .as_ref()
                .map(|tp| tp.params.iter().map(|t| lower_ts_type(t, source)).collect())
                .unwrap_or_default();
            if args.is_empty() {
                vec![super::TypeOwned::Nominal(name)]
            } else {
                vec![super::TypeOwned::Apply {
                    base: Box::new(super::TypeOwned::Nominal(name)),
                    args,
                }]
            }
        })
        .unwrap_or_default();

    // TSClassImplements has `expression: Expression` (the type being implemented)
    // and `type_arguments: Option<TSTypeParameterInstantiation>`.
    // We extract the name from the expression and type-args safely.
    let implements: Vec<super::TypeOwned> = cls
        .implements
        .iter()
        .map(|i| {
            use oxc_ast::ast::TSTypeName;
            let name = match &i.expression {
                TSTypeName::IdentifierReference(id) => id.name.to_string(),
                other => other.span().source_text(source).to_string(),
            };
            let args: Vec<super::TypeOwned> = i
                .type_arguments
                .as_ref()
                .map(|tp| tp.params.iter().map(|t| lower_ts_type(t, source)).collect())
                .unwrap_or_default();
            if args.is_empty() {
                super::TypeOwned::Nominal(name)
            } else {
                super::TypeOwned::Apply {
                    base: Box::new(super::TypeOwned::Nominal(name)),
                    args,
                }
            }
        })
        .collect();

    let is_abstract = cls.r#abstract;

    // Class-level decorators (item 7).
    let decorators: Vec<AttrTok> = cls
        .decorators
        .iter()
        .map(|d| AttrTok { token: d.span.source_text(source).to_string() })
        .collect();

    // First pass: collect non-constructor members from the AST.
    let mut members: Vec<MemberFact> = cls
        .body
        .body
        .iter()
        .filter_map(|elem| lower_class_element(elem, source))
        .collect();

    // Build a set of already-declared field names (from PropertyDefinition).
    let mut declared_field_names: std::collections::HashSet<String> = members
        .iter()
        .filter_map(|m| match &m.kind {
            MemberKind::Property { .. } | MemberKind::Accessor { .. } => Some(m.name.clone()),
            _ => None,
        })
        .collect();

    // ── Constructor parameter properties (item 1) ────────────────────────
    // `constructor(public x: T, private readonly y: U)` synthesises fields.
    let ctor_elem = cls.body.body.iter().find(|elem| {
        matches!(
            elem,
            ClassElement::MethodDefinition(m) if m.kind == MethodDefinitionKind::Constructor
        )
    });
    if let Some(ClassElement::MethodDefinition(ctor)) = ctor_elem {
        for param in ctor.value.params.items.iter() {
            // Only parameter properties: must have accessibility OR readonly.
            if param.accessibility.is_none() && !param.readonly {
                continue;
            }
            let name = binding_pattern_name(&param.pattern)
                .unwrap_or_else(|| "_".to_string());
            if !declared_field_names.insert(name.clone()) {
                continue; // already declared as a PropertyDefinition
            }
            let accessibility = ts_accessibility(&param.accessibility);
            let is_readonly = param.readonly;
            let modifiers = MemberModifiers {
                accessibility,
                is_static: false,
                is_readonly,
                is_optional: param.optional,
                is_abstract: false,
            };
            let ty = param
                .type_annotation
                .as_ref()
                .map(|ann| lower_ts_type(&ann.type_annotation, source));
            members.push(MemberFact {
                name,
                kind: MemberKind::Property { ty },
                modifiers,
                doc: DocFacts::default(),
                decorators: Vec::new(),
            });
        }

        // ── this.x = … field synthesis (item 2) ─────────────────────────
        // Walk constructor body for `this.<name> = …` assignments.
        if let Some(ref ctor_body) = ctor.value.body {
            let mut synth_seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for stmt in ctor_body.statements.iter() {
                if let Statement::ExpressionStatement(expr_stmt) = stmt {
                    if let Expression::AssignmentExpression(assign) = &expr_stmt.expression {
                        if assign.operator == AssignmentOperator::Assign {
                            if let AssignmentTarget::StaticMemberExpression(mem) = &assign.left {
                                if matches!(&mem.object, Expression::ThisExpression(_)) {
                                    let prop_name = mem.property.name.to_string();
                                    if !declared_field_names.contains(&prop_name)
                                        && synth_seen.insert(prop_name.clone())
                                    {
                                        declared_field_names.insert(prop_name.clone());
                                        members.push(MemberFact {
                                            name: prop_name,
                                            kind: MemberKind::Property { ty: None },
                                            modifiers: MemberModifiers::default(),
                                            doc: DocFacts::default(),
                                            decorators: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Rename static block placeholders with sequential names ───────────
    // StaticBlock members are initially emitted with the placeholder name.
    // We rename them here to `__static`, `__static_1`, `__static_2`, … in
    // declaration order.
    let mut static_count: usize = 0;
    for m in members.iter_mut() {
        if let MemberKind::StaticBlock { ref mut name } = m.kind {
            let real_name = if static_count == 0 {
                "__static".to_string()
            } else {
                format!("__static_{}", static_count)
            };
            static_count += 1;
            *name = real_name.clone();
            m.name = real_name;
        }
    }

    ClassBody { generics, extends, implements, members, is_abstract, decorators }
}

fn lower_class_element<'a>(elem: &ClassElement<'a>, source: &'a str) -> Option<MemberFact> {
    match elem {
        ClassElement::MethodDefinition(m) => {
            let name = property_key_name(&m.key, source);
            let accessibility = ts_accessibility(&m.accessibility);
            let is_private_field = matches!(m.key, PropertyKey::PrivateIdentifier(_));
            let is_static = m.r#static;
            let is_abstract = m.r#type == MethodDefinitionType::TSAbstractMethodDefinition;
            let modifiers = MemberModifiers {
                accessibility: if is_private_field {
                    Accessibility::PrivateField
                } else {
                    accessibility
                },
                is_static,
                is_readonly: false,
                is_optional: m.optional,
                is_abstract,
            };
            // Decorators on the method (item 7).
            let decorators: Vec<AttrTok> = m
                .decorators
                .iter()
                .map(|d| AttrTok { token: d.span.source_text(source).to_string() })
                .collect();
            let sig = lower_function(&m.value, source);
            Some(MemberFact {
                name,
                kind: match m.kind {
                    MethodDefinitionKind::Constructor => MemberKind::Constructor(sig),
                    _ => MemberKind::Method(vec![sig]),
                },
                modifiers,
                doc: DocFacts::default(),
                decorators,
            })
        }

        ClassElement::PropertyDefinition(p) => {
            let name = property_key_name(&p.key, source);
            let accessibility = ts_accessibility(&p.accessibility);
            let is_private_field = matches!(p.key, PropertyKey::PrivateIdentifier(_));
            let is_static = p.r#static;
            let is_readonly = p.readonly;
            let is_optional = p.optional;
            let is_abstract = p.r#type == PropertyDefinitionType::TSAbstractPropertyDefinition;
            let modifiers = MemberModifiers {
                accessibility: if is_private_field {
                    Accessibility::PrivateField
                } else {
                    accessibility
                },
                is_static,
                is_readonly,
                is_optional,
                is_abstract,
            };
            let ty = p
                .type_annotation
                .as_ref()
                .map(|ann| lower_ts_type(&ann.type_annotation, source));
            // Decorators on the property (item 7).
            let decorators: Vec<AttrTok> = p
                .decorators
                .iter()
                .map(|d| AttrTok { token: d.span.source_text(source).to_string() })
                .collect();
            Some(MemberFact {
                name,
                kind: MemberKind::Property { ty },
                modifiers,
                doc: DocFacts::default(),
                decorators,
            })
        }

        // ── Item 3: Accessor property (`accessor x: T`) ───────────────────
        // The TC39 `accessor` keyword auto-creates a getter/setter pair.
        // We surface it as a distinct `MemberKind::Accessor` member so emit
        // can record it as a Field with an `accessor` decorator.
        ClassElement::AccessorProperty(ap) => {
            let name = property_key_name(&ap.key, source);
            let accessibility = ts_accessibility(&ap.accessibility);
            let is_private_field = matches!(ap.key, PropertyKey::PrivateIdentifier(_));
            let is_static = ap.r#static;
            let is_abstract = ap.r#type == AccessorPropertyType::TSAbstractAccessorProperty;
            let modifiers = MemberModifiers {
                accessibility: if is_private_field {
                    Accessibility::PrivateField
                } else {
                    accessibility
                },
                is_static,
                is_readonly: false, // accessor is read+write
                is_optional: false,
                is_abstract,
            };
            let ty = ap
                .type_annotation
                .as_ref()
                .map(|ann| lower_ts_type(&ann.type_annotation, source));
            // Decorators on the accessor (item 7).
            let mut decorators: Vec<AttrTok> = ap
                .decorators
                .iter()
                .map(|d| AttrTok { token: d.span.source_text(source).to_string() })
                .collect();
            // Synthetic marker so downstream consumers can tell accessor from plain property.
            decorators.push(AttrTok { token: "accessor".to_string() });
            Some(MemberFact {
                name,
                kind: MemberKind::Accessor { ty },
                modifiers,
                doc: DocFacts::default(),
                decorators,
            })
        }

        // ── Item 4: Static initializer block (`static { … }`) ────────────
        // No type-level surface, but must not be silently dropped.
        // Emit as `MemberKind::StaticBlock` with a synthetic name; emit.rs
        // will translate this to a synthetic Function child.
        ClassElement::StaticBlock(_sb) => {
            // The caller (`lower_class`) assigns the unique __static[_N] name.
            // We emit a placeholder here; lower_class re-names them in order.
            Some(MemberFact {
                name: "__static_placeholder".to_string(),
                kind: MemberKind::StaticBlock { name: "__static".to_string() },
                modifiers: MemberModifiers {
                    accessibility: Accessibility::Private,
                    is_static: true,
                    is_readonly: false,
                    is_optional: false,
                    is_abstract: false,
                },
                doc: DocFacts::default(),
                decorators: Vec::new(),
            })
        }

        // TSIndexSignature does not produce a MemberFact — it is handled
        // separately in the interface path and is not a class member kind.
        ClassElement::TSIndexSignature(_) => None,
    }
}

fn lower_interface<'a>(
    iface: &TSInterfaceDeclaration<'a>,
    source: &'a str,
    _semantic: &Semantic<'a>,
) -> InterfaceBody {
    let generics = iface
        .type_parameters
        .as_ref()
        .map(|tp| lower_type_params(tp, source))
        .unwrap_or_default();

    let extends: Vec<_> = iface
        .extends
        .iter()
        .map(|h| {
            // TSInterfaceHeritage has an expression; we use the source span text.
            let name = match &h.expression {
                Expression::Identifier(id) => id.name.to_string(),
                other => format!("{:?}", other.span().source_text(source)),
            };
            if let Some(tp) = &h.type_arguments {
                let args: Vec<_> = tp.params.iter().map(|p| lower_ts_type(p, source)).collect();
                super::TypeOwned::Apply {
                    base: Box::new(super::TypeOwned::Nominal(name)),
                    args,
                }
            } else {
                super::TypeOwned::Nominal(name)
            }
        })
        .collect();

    let mut methods: Vec<MethodFact> = Vec::new();
    let mut properties: Vec<PropertyFact> = Vec::new();
    let mut call_signatures: Vec<FunctionBody> = Vec::new();
    let mut index_signatures: Vec<IndexSignatureFact> = Vec::new();
    let mut construct_signatures: Vec<FunctionBody> = Vec::new();

    for sig in iface.body.body.iter() {
        match sig {
            TSSignature::TSMethodSignature(m) => {
                let name = property_key_name(&m.key, source);
                let modifiers = MemberModifiers {
                    accessibility: Accessibility::Public,
                    is_static: false,
                    is_readonly: false,
                    is_optional: m.optional,
                    is_abstract: false,
                };
                let params = lower_formal_parameters(&m.params, source, false);
                let return_type = m
                    .return_type
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let generics = m
                    .type_parameters
                    .as_ref()
                    .map(|tp| lower_type_params(tp, source))
                    .unwrap_or_default();
                let sig = FunctionBody {
                    generics,
                    params,
                    return_type,
                    is_async: false,
                    is_generator: false,
                    has_body: false,
                    receiver: ReceiverKind::SharedRef,
                };
                methods.push(MethodFact {
                    name,
                    sig,
                    modifiers,
                    doc: DocFacts::default(),
                    is_overload: false,
                });
            }
            TSSignature::TSPropertySignature(p) => {
                let name = property_key_name(&p.key, source);
                let ty = p
                    .type_annotation
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let modifiers = MemberModifiers {
                    accessibility: Accessibility::Public,
                    is_static: false,
                    is_readonly: p.readonly,
                    is_optional: p.optional,
                    is_abstract: false,
                };
                properties.push(PropertyFact {
                    name,
                    ty,
                    modifiers,
                    doc: DocFacts::default(),
                });
            }
            TSSignature::TSCallSignatureDeclaration(c) => {
                let params = lower_formal_parameters(&c.params, source, false);
                let return_type = c
                    .return_type
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let generics = c
                    .type_parameters
                    .as_ref()
                    .map(|tp| lower_type_params(tp, source))
                    .unwrap_or_default();
                call_signatures.push(FunctionBody {
                    generics,
                    params,
                    return_type,
                    is_async: false,
                    is_generator: false,
                    has_body: false,
                    receiver: ReceiverKind::None,
                });
            }
            // ── Item 5: Index signatures (`[k: string]: T`) ───────────────
            TSSignature::TSIndexSignature(idx) => {
                // TSIndexSignature has `parameters: Vec<TSIndexSignatureNameBinding>`
                // and `type_annotation: TSTypeAnnotation`.
                // Each parameter has a `name` (BindingIdentifier) and `type_annotation`.
                if let Some(param) = idx.parameters.first() {
                    let key_name = param.name.as_str().to_string();
                    let key_ty = lower_ts_type(&param.type_annotation.type_annotation, source);
                    let value_ty = lower_ts_type(&idx.type_annotation.type_annotation, source);
                    index_signatures.push(IndexSignatureFact { key_name, key_ty, value_ty });
                }
            }
            // ── Item 6: Construct signatures (`new (…): T`) ───────────────
            TSSignature::TSConstructSignatureDeclaration(cs) => {
                let params = lower_formal_parameters(&cs.params, source, false);
                let return_type = cs
                    .return_type
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let generics = cs
                    .type_parameters
                    .as_ref()
                    .map(|tp| lower_type_params(tp, source))
                    .unwrap_or_default();
                construct_signatures.push(FunctionBody {
                    generics,
                    params,
                    return_type,
                    is_async: false,
                    is_generator: false,
                    has_body: false,
                    receiver: ReceiverKind::None,
                });
            }
        }
    }

    InterfaceBody {
        generics,
        extends,
        methods,
        properties,
        call_signatures,
        index_signatures,
        construct_signatures,
    }
}

fn lower_enum<'a>(e: &TSEnumDeclaration<'a>) -> EnumBody {
    let is_const = e.r#const;
    let variants: Vec<VariantFact> = e
        .body
        .members
        .iter()
        .map(|m| {
            let name = match &m.id {
                TSEnumMemberName::Identifier(id) => id.name.to_string(),
                TSEnumMemberName::String(s) => s.value.to_string(),
                other => format!("{:?}", other),
            };
            let discriminant = m.initializer.as_ref().map(|init| match init {
                Expression::StringLiteral(s) => format!("\"{}\"", s.value),
                Expression::NumericLiteral(n) => n.value.to_string(),
                Expression::UnaryExpression(u) => {
                    // Handle `-1` style numeric enum values.
                    format!("{:?}", u.operator)
                }
                other => format!("{:?}", other),
            });
            VariantFact { name, discriminant }
        })
        .collect();
    EnumBody { is_const, variants }
}

fn lower_variable<'a>(
    v: &VariableDeclaration<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    is_exported: bool,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;

    let is_const = matches!(v.kind, VariableDeclarationKind::Const);
    let mut out = Vec::new();
    for d in v.declarations.iter() {
        let Some(name) = binding_pattern_name(&d.id) else {
            continue;
        };
        let span = d.span();
        let doc = jsdoc::jsdoc_for_span(semantic, span);
        if doc.ignore {
            continue;
        }
        let discriminant = bump_count(&name, name_counts);
        let ty = d.type_annotation
            .as_ref()
            .map(|ann| lower_ts_type(&ann.type_annotation, source));
        let value = d.init.as_ref().map(|e| format!("{:?}", e.span().source_text(source)));

        let body = if is_const {
            DeclBody::Const(ConstBody { ty, value })
        } else {
            DeclBody::Static(StaticBody {
                ty,
                value,
                is_mutable: matches!(v.kind, VariableDeclarationKind::Let),
            })
        };

        let visibility = if is_exported { Visibility::Public } else { Visibility::Private };
        out.push(DeclFact {
            name,
            visibility,
            doc,
            body,
            module: path.to_path_buf(),
            span_start: span.start,
            span_end: span.end,
            is_default: false,
            decl_index: discriminant,
        });
    }
    out
}

fn lower_namespace<'a>(
    m: &'a TSModuleDeclaration<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    is_exported: bool,
    is_default: bool,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;

    let name = match &m.id {
        TSModuleDeclarationName::Identifier(id) => id.name.to_string(),
        TSModuleDeclarationName::StringLiteral(s) => s.value.to_string(),
    };
    let is_ambient = m.declare;
    let span = m.span();
    let doc = jsdoc::jsdoc_for_span(semantic, span);
    if doc.ignore {
        return vec![];
    }
    let discriminant = bump_count(&name, name_counts);

    let children = match &m.body {
        Some(TSModuleDeclarationBody::TSModuleBlock(block)) => {
            let mut child_counts: std::collections::HashMap<String, u32> = Default::default();
            let mut children = Vec::new();
            for stmt in block.body.iter() {
                let decls = extract_statement(
                    stmt,
                    source,
                    semantic,
                    path,
                    &Default::default(),
                    &None,
                    &mut child_counts,
                );
                children.extend(decls);
            }
            children
        }
        Some(TSModuleDeclarationBody::TSModuleDeclaration(inner)) => {
            lower_namespace(inner, source, semantic, path, is_exported, false, name_counts)
        }
        None => vec![],
    };

    let visibility = if is_exported { Visibility::Public } else { Visibility::Private };

    vec![DeclFact {
        name,
        visibility,
        doc,
        body: DeclBody::Namespace(NamespaceBody { is_ambient, children }),
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default,
        decl_index: discriminant,
    }]
}

// ── Export / import table builders ─────────────────────────────────────────────

fn build_export_table<'a>(
    module_record: &ModuleRecord<'a>,
    exported_names: &std::collections::HashSet<String>,
    default_local_name: &Option<String>,
) -> ExportTable {
    let exported_names_list: Vec<String> = exported_names.iter().cloned().collect();

    let mut indirect: Vec<IndirectExport> = Vec::new();
    let mut star: Vec<StarExport> = Vec::new();

    for e in module_record.indirect_export_entries.iter() {
        let Some(module_request) = e.module_request.as_ref().map(|n| n.name.to_string()) else {
            continue;
        };
        let import_name = match &e.import_name {
            ExportImportName::Name(ns) => ns.name.to_string(),
            ExportImportName::All | ExportImportName::AllButDefault => "*".to_string(),
            ExportImportName::Null => continue,
        };
        let export_name = match &e.export_name {
            ExportExportName::Name(ns) => ns.name.to_string(),
            ExportExportName::Default(_) => "default".to_string(),
            ExportExportName::Null => continue,
        };
        indirect.push(IndirectExport { module_request, import_name, export_name });
    }

    for e in module_record.star_export_entries.iter() {
        let Some(module_request) = e.module_request.as_ref().map(|n| n.name.to_string()) else {
            continue;
        };
        star.push(StarExport { module_request });
    }

    ExportTable {
        exported_names: exported_names_list,
        indirect,
        star,
        default_local_name: default_local_name.clone(),
    }
}

fn build_import_table<'a>(module_record: &ModuleRecord<'a>) -> Vec<ImportFact> {
    module_record
        .import_entries
        .iter()
        .map(|e| {
            let import_name = match &e.import_name {
                ImportImportName::Name(ns) => ImportName::Named(ns.name.to_string()),
                ImportImportName::Default(_) => ImportName::Default,
                ImportImportName::NamespaceObject => ImportName::Namespace,
            };
            ImportFact {
                module_request: e.module_request.name.to_string(),
                import_name,
                local_name: e.local_name.name.to_string(),
                is_type: e.is_type,
            }
        })
        .collect()
}

// ── Shared helpers ─────────────────────────────────────────────────────────────

fn property_key_name<'a>(key: &PropertyKey<'a>, source: &'a str) -> String {
    match key {
        PropertyKey::StaticIdentifier(id) => id.name.to_string(),
        PropertyKey::PrivateIdentifier(id) => format!("#{}", id.name),
        other => other.span().source_text(source).to_string(),
    }
}

fn ts_accessibility(acc: &Option<TSAccessibility>) -> Accessibility {
    match acc {
        Some(TSAccessibility::Private) => Accessibility::Private,
        Some(TSAccessibility::Protected) => Accessibility::Protected,
        _ => Accessibility::Public,
    }
}

fn binding_pattern_name<'a>(pat: &BindingPattern<'a>) -> Option<String> {
    match pat {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        BindingPattern::AssignmentPattern(ap) => binding_pattern_name(&ap.left),
        BindingPattern::ObjectPattern(_) => Some("{...}".to_string()),
        BindingPattern::ArrayPattern(_) => Some("[...]".to_string()),
    }
}

fn bump_count(name: &str, counts: &mut std::collections::HashMap<String, u32>) -> u32 {
    let entry = counts.entry(name.to_string()).or_insert(0);
    let n = *entry;
    *entry += 1;
    n
}
