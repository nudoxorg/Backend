//! The lexical scan of one TypeScript, TSX or JSX source: which stylesheets,
//! classes and custom properties it uses and which components it renders and
//! imports.
//!
//! This is the part of the code adapter whose output is published as index-time
//! facts (`code_links`), so it lives apart from the cross-file graph: changing
//! how the graph is built does not change what a scan says, and only this file
//! is part of the producer's recipe fingerprint.
use backend_semantic::ir::TypeScriptSource;
use backend_frontend_typescript::legacy::{
    ExportExportName, ExportImportName, ExportLocalName, Expression, ImportImportName,
    JSXAttributeName, JSXAttributeValue, JSXElementName, NodeId, ObjectPropertyKind, OxcModule,
};
use std::collections::{BTreeMap, BTreeSet};

/// Helper functions whose static string arguments are class names.
const CLASS_HELPERS: &[&str] = &["clsx", "classnames", "classNames", "cn", "cx", "twMerge", "cva"];
/// Attribute names which carry class lists.
const CLASS_ATTRIBUTES: &[&str] = &["className", "class"];

// ---------------------------------------------------------------------------
// Per-file facts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) enum Imported {
    Default,
    Named(String),
    Namespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct ImportBinding {
    pub local: String,
    pub spec: String,
    pub imported: Imported,
    pub line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct ClassUse {
    pub token: String,
    pub line: u32,
    pub owner: Option<String>,
    /// Appears in a branch, logical operand or helper object key.
    pub conditional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct ModuleClassUse {
    /// Index into `imports` of the stylesheet binding.
    pub binding: usize,
    pub class: String,
    pub line: u32,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct RenderUse {
    /// Local component name used as a JSX tag.
    pub local: String,
    pub line: u32,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct PropertyUse {
    pub name: String,
    pub line: u32,
    pub owner: Option<String>,
    pub writes: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::builtin) struct FileScan {
    pub path: String,
    pub imports: Vec<ImportBinding>,
    /// Stylesheet imports without bindings.
    pub side_effect_imports: Vec<(String, u32)>,
    pub class_uses: Vec<ClassUse>,
    pub module_class_uses: Vec<ModuleClassUse>,
    pub renders: Vec<RenderUse>,
    pub property_uses: Vec<PropertyUse>,
    /// Component-like declarations: name and line.
    pub components: BTreeMap<String, u32>,
    /// Exported name to local declaration name.
    pub exports: BTreeMap<String, String>,
    /// Re-exports: exported name to (specifier, imported name or "*").
    pub reexports: BTreeMap<String, (String, String)>,
    pub dynamic_class_expressions: u32,
}

fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(u32::try_from(index + 1).unwrap_or(u32::MAX));
        }
    }
    starts
}
fn line_at(starts: &[u32], offset: u32) -> u32 {
    u32::try_from(starts.partition_point(|start| *start <= offset)).unwrap_or(u32::MAX)
}

pub(super) fn is_component_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Whitespace-separated tokens of a static class string.
fn class_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace().filter(|token| !token.is_empty() && token.len() <= 256)
}

/// Scan one parsed module.
pub(in crate::builtin) fn scan_module(path: &str, source: &str, module: &OxcModule<'_>) -> FileScan {
    let semantic = &module.semantic;
    let nodes = semantic.nodes();
    let starts = line_starts(source);
    let mut scan = FileScan { path: path.to_owned(), ..FileScan::default() };

    // Imports and exports come from the module record.
    let mut bound_specs: BTreeSet<String> = BTreeSet::new();
    for entry in module.module_record.import_entries.iter() {
        if entry.is_type {
            continue;
        }
        let spec = entry.module_request.name.as_str().to_owned();
        bound_specs.insert(spec.clone());
        let imported = match &entry.import_name {
            ImportImportName::Name(name) => Imported::Named(name.name.as_str().to_owned()),
            ImportImportName::NamespaceObject => Imported::Namespace,
            ImportImportName::Default(_) => Imported::Default,
        };
        scan.imports.push(ImportBinding {
            local: entry.local_name.name.as_str().to_owned(),
            spec,
            imported,
            line: line_at(&starts, entry.statement_span.start),
        });
    }
    for (spec, requests) in module.module_record.requested_modules.iter() {
        if bound_specs.contains(spec.as_str()) {
            continue;
        }
        for request in requests.iter() {
            if request.is_import && !request.is_type {
                scan.side_effect_imports.push((spec.as_str().to_owned(), line_at(&starts, request.statement_span.start)));
            }
        }
    }
    for entry in module.module_record.local_export_entries.iter() {
        if entry.is_type {
            continue;
        }
        let exported = match &entry.export_name {
            ExportExportName::Name(name) => name.name.as_str().to_owned(),
            ExportExportName::Default(_) => "default".to_owned(),
            _ => continue,
        };
        let local = match &entry.local_name {
            ExportLocalName::Name(name) => name.name.as_str().to_owned(),
            ExportLocalName::Default(name) => name.name.as_str().to_owned(),
            _ => continue,
        };
        scan.exports.insert(exported, local);
    }
    for entry in module.module_record.indirect_export_entries.iter() {
        if entry.is_type {
            continue;
        }
        let (Some(request), ExportExportName::Name(exported)) = (&entry.module_request, &entry.export_name) else {
            if let (Some(request), ExportExportName::Default(_)) = (&entry.module_request, &entry.export_name) {
                scan.reexports.insert("default".to_owned(), (request.name.as_str().to_owned(), "default".to_owned()));
            }
            continue;
        };
        let imported = match &entry.import_name {
            ExportImportName::Name(name) => name.name.as_str().to_owned(),
            _ => "*".to_owned(),
        };
        scan.reexports.insert(exported.name.as_str().to_owned(), (request.name.as_str().to_owned(), imported));
    }

    // Owner of a node: the nearest component-like function, class or variable.
    let owner_of = |id: NodeId| -> Option<String> {
        let mut current = id;
        loop {
            let parent = nodes.parent_id(current);
            if parent == current {
                return None;
            }
            current = parent;
            let kind = nodes.get_node(current).kind();
            let name: Option<String> = if let Some(function) = kind.as_function() {
                function.id.as_ref().map(|id| id.name.as_str().to_owned())
            } else if let Some(class) = kind.as_class() {
                class.id.as_ref().map(|id| id.name.as_str().to_owned())
            } else if kind.as_arrow_function_expression().is_some() {
                // `const Name = () => ...` or `const Name = memo(() => ...)`.
                let mut up = nodes.parent_id(current);
                let mut found = None;
                for _ in 0..3 {
                    let k = nodes.get_node(up).kind();
                    if let Some(declarator) = k.as_variable_declarator() {
                        found = declarator.id.get_identifier_name().map(|n| n.as_str().to_owned());
                        break;
                    }
                    if k.as_call_expression().is_none() && k.as_parenthesized_expression().is_none() {
                        break;
                    }
                    up = nodes.parent_id(up);
                }
                found
            } else {
                None
            };
            if let Some(name) = name {
                if is_component_name(&name) {
                    return Some(name);
                }
            }
        }
    };

    let module_locals: BTreeSet<String> = scan
        .imports
        .iter()
        .filter(|binding| binding.spec.ends_with(".module.css") && !matches!(binding.imported, Imported::Named(_)))
        .map(|binding| binding.local.clone())
        .collect();
    for node in nodes.iter() {
        let kind = node.kind();
        let id = node.id();
        // Component-like declarations at any depth of the module.
        if let Some(function) = kind.as_function() {
            if let Some(name) = function.id.as_ref().map(|id| id.name.as_str()) {
                if is_component_name(name) {
                    scan.components.entry(name.to_owned()).or_insert(line_at(&starts, function.span.start));
                }
            }
        } else if let Some(class) = kind.as_class() {
            if let Some(name) = class.id.as_ref().map(|id| id.name.as_str()) {
                if is_component_name(name) {
                    scan.components.entry(name.to_owned()).or_insert(line_at(&starts, class.span.start));
                }
            }
        } else if let Some(declarator) = kind.as_variable_declarator() {
            if let (Some(name), Some(init)) = (declarator.id.get_identifier_name(), declarator.init.as_ref()) {
                let looks_callable = matches!(
                    init,
                    Expression::ArrowFunctionExpression(_)
                        | Expression::FunctionExpression(_)
                        | Expression::CallExpression(_)
                );
                if looks_callable && is_component_name(name.as_str()) {
                    scan.components.entry(name.as_str().to_owned()).or_insert(line_at(&starts, declarator.span.start));
                }
            }
        }
        // JSX attributes with class lists.
        if let Some(attribute) = kind.as_jsx_attribute() {
            let name = match &attribute.name {
                JSXAttributeName::Identifier(identifier) => identifier.name.as_str(),
                _ => continue,
            };
            if !CLASS_ATTRIBUTES.contains(&name) {
                continue;
            }
            let owner = owner_of(id);
            let line = line_at(&starts, attribute.span.start);
            match &attribute.value {
                Some(JSXAttributeValue::StringLiteral(text)) => {
                    for token in class_tokens(text.value.as_str()) {
                        scan.class_uses.push(ClassUse { token: token.to_owned(), line, owner: owner.clone(), conditional: false });
                    }
                }
                Some(JSXAttributeValue::ExpressionContainer(container)) => {
                    if let Some(expression) = container.expression.as_expression() {
                        let mut dynamic = false;
                        collect_expression(expression, false, &module_locals, &mut |token, conditional| {
                            scan.class_uses.push(ClassUse { token: token.to_owned(), line, owner: owner.clone(), conditional });
                        }, &mut dynamic);
                        if dynamic {
                            scan.dynamic_class_expressions += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        // JSX tags: component renders.
        if let Some(opening) = kind.as_jsx_opening_element() {
            if let JSXElementName::IdentifierReference(reference) = &opening.name {
                let name = reference.name.as_str();
                if is_component_name(name) {
                    scan.renders.push(RenderUse {
                        local: name.to_owned(),
                        line: line_at(&starts, opening.span.start),
                        owner: owner_of(id),
                    });
                }
            }
        }
        // `styles.card` and `styles["card"]`.
        if let Some(member) = kind.as_static_member_expression() {
            if let Expression::Identifier(object) = &member.object {
                scan.record_module_use(object.name.as_str(), member.property.name.as_str(), line_at(&starts, member.span.start), owner_of(id));
            }
        }
        if let Some(member) = kind.as_computed_member_expression() {
            if let (
                Expression::Identifier(object),
                Expression::StringLiteral(key),
            ) = (&member.object, &member.expression)
            {
                scan.record_module_use(object.name.as_str(), key.value.as_str(), line_at(&starts, member.span.start), owner_of(id));
            }
        }
        // Custom properties set or read from code.
        if let Some(call) = kind.as_call_expression() {
            if let Expression::StaticMemberExpression(callee) = &call.callee {
                let method = callee.property.name.as_str();
                let writes = match method {
                    "setProperty" => Some(true),
                    "getPropertyValue" | "removeProperty" => Some(method == "removeProperty"),
                    _ => None,
                };
                if let (Some(writes), Some(first)) = (writes, call.arguments.first()) {
                    if let Some(Expression::StringLiteral(name)) = first.as_expression() {
                        if name.value.as_str().starts_with("--") {
                            scan.property_uses.push(PropertyUse {
                                name: name.value.as_str().to_owned(),
                                line: line_at(&starts, call.span.start),
                                owner: owner_of(id),
                                writes,
                            });
                        }
                    }
                }
            }
        }
        if let Some(property) = kind.as_object_property() {
            if let Some(name) = property.key.static_name() {
                if name.starts_with("--") && name.len() > 2 {
                    scan.property_uses.push(PropertyUse {
                        name: name.to_string(),
                        line: line_at(&starts, property.span.start),
                        owner: owner_of(id),
                        writes: true,
                    });
                }
            }
        }
    }
    scan
}

impl FileScan {
    fn record_module_use(&mut self, local: &str, class: &str, line: u32, owner: Option<String>) {
        if let Some(index) = self.imports.iter().position(|binding| {
            binding.local == local
                && binding.spec.ends_with(".module.css")
                && matches!(binding.imported, Imported::Default | Imported::Namespace)
        }) {
            self.module_class_uses.push(ModuleClassUse { binding: index, class: class.to_owned(), line, owner });
        }
    }
}

/// Collect static class tokens from a class-list expression. `dynamic` becomes
/// true when any part cannot be read statically.
fn collect_expression(
    expression: &Expression<'_>,
    conditional: bool,
    module_locals: &BTreeSet<String>,
    emit: &mut dyn FnMut(&str, bool),
    dynamic: &mut bool,
) {
    use Expression as E;
    match expression {
        E::StringLiteral(text) => {
            for token in class_tokens(text.value.as_str()) {
                emit(token, conditional);
            }
        }
        E::TemplateLiteral(template) => {
            // Tokens touching a `${}` boundary are partial words, so only whole
            // whitespace-delimited tokens inside a quasi are static.
            let count = template.quasis.len();
            for (index, quasi) in template.quasis.iter().enumerate() {
                let text = quasi.value.raw.as_str();
                let starts_open = index > 0 && !text.starts_with(char::is_whitespace);
                let ends_open = index + 1 < count && !text.ends_with(char::is_whitespace);
                let mut tokens: Vec<&str> = class_tokens(text).collect();
                if ends_open && !tokens.is_empty() {
                    tokens.pop();
                    *dynamic = true;
                }
                if starts_open && !tokens.is_empty() {
                    tokens.remove(0);
                    *dynamic = true;
                }
                for token in tokens {
                    emit(token, conditional);
                }
            }
            if !template.expressions.is_empty() {
                *dynamic = true;
            }
        }
        E::ConditionalExpression(branch) => {
            collect_expression(&branch.consequent, true, module_locals, emit, dynamic);
            collect_expression(&branch.alternate, true, module_locals, emit, dynamic);
        }
        E::LogicalExpression(logical) => collect_expression(&logical.right, true, module_locals, emit, dynamic),
        E::ParenthesizedExpression(inner) => collect_expression(&inner.expression, conditional, module_locals, emit, dynamic),
        E::TSAsExpression(inner) => collect_expression(&inner.expression, conditional, module_locals, emit, dynamic),
        E::ArrayExpression(array) => {
            for element in &array.elements {
                match element.as_expression() {
                    Some(inner) => collect_expression(inner, conditional, module_locals, emit, dynamic),
                    None => *dynamic = true,
                }
            }
        }
        E::CallExpression(call) => {
            let helper = matches!(&call.callee, E::Identifier(name) if CLASS_HELPERS.contains(&name.name.as_str()));
            if helper {
                for argument in &call.arguments {
                    match argument.as_expression() {
                        Some(inner) => collect_expression(inner, conditional, module_locals, emit, dynamic),
                        None => *dynamic = true,
                    }
                }
            } else {
                *dynamic = true;
            }
        }
        E::ObjectExpression(object) => {
            for property in &object.properties {
                match property {
                    ObjectPropertyKind::ObjectProperty(entry) => {
                        match entry.key.static_name() {
                            Some(name) => {
                                for token in class_tokens(&name) {
                                    emit(token, true);
                                }
                            }
                            None => *dynamic = true,
                        }
                    }
                    _ => *dynamic = true,
                }
            }
        }
        // `a + " b"`: both operands are class text.
        E::BinaryExpression(sum) if sum.operator.as_str() == "+" => {
            collect_expression(&sum.left, conditional, module_locals, emit, dynamic);
            collect_expression(&sum.right, conditional, module_locals, emit, dynamic);
        }
        // `styles.card` is a resolved CSS-module member, not a runtime class.
        E::StaticMemberExpression(member)
            if matches!(&member.object, E::Identifier(object) if module_locals.contains(object.name.as_str())) => {}
        E::ComputedMemberExpression(member)
            if matches!(&member.object, E::Identifier(object) if module_locals.contains(object.name.as_str()))
                && matches!(&member.expression, E::StringLiteral(_)) => {}
        _ => *dynamic = true,
    }
}


pub(in crate::builtin) fn profile_for(path: &str) -> Option<TypeScriptSource> {
    match path.rsplit('.').next()? {
        "tsx" | "jsx" | "js" | "mjs" | "cjs" => Some(TypeScriptSource::Tsx),
        "ts" | "mts" | "cts" => Some(TypeScriptSource::TypeScript),
        _ => None,
    }
}
