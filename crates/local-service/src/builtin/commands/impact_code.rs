//! Code adapter for the shared impact framework: how TypeScript, TSX and JSX
//! use stylesheets, and how components use each other.
//!
//! Sources come from the selected index (native TypeScript evidence, or the
//! structural file records), are re-read through the confined project reader,
//! and are checked against the recorded identity before they are parsed. The
//! parse is the lexical authority already used by the TypeScript lane (OXC);
//! no CSS or SQL grammar is involved. Every link states its proof:
//!
//! * `styles.card` where `styles` is an import binding of a `*.module.css`
//!   file is an **exact** source-level link to that module's class;
//! * a literal class token in `className`/`class`, including the static
//!   arguments of `clsx`-style helpers, is a **candidate**: nothing here
//!   proves the element renders or which build transform applies;
//! * a JSX tag bound by import to a component in an indexed file is an
//!   **exact** render link; aliases, packages and unresolved barrels are gaps.
use super::impact_css::{global_class_key, module_class_key, stylesheet_key, custom_property_key};
use super::impact_framework::{
    DomainLoad, Edge, FileWitness, ImpactAdapter, ImpactContext, Key, Node, node_key,
};
use backend_semantic::ir::TypeScriptSource;
use backend_frontend_typescript::legacy::{
    ExportExportName, ExportImportName, ExportLocalName, Expression, ImportImportName,
    JSXAttributeName, JSXAttributeValue, JSXElementName, NodeId, ObjectPropertyKind, OxcModule,
    with_analysis,
};
use backend_library::impact::{
    ImpactDomain, ImpactDomainBasis, ImpactDomainState, ImpactGap, ImpactSeedSpec, ImpactStrength,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct CodeAdapter;

const MAX_FILES: usize = 4_000;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 48 * 1024 * 1024;
/// Helper functions whose static string arguments are class names.
const CLASS_HELPERS: &[&str] = &["clsx", "classnames", "classNames", "cn", "cx", "twMerge", "cva"];
/// Attribute names which carry class lists.
const CLASS_ATTRIBUTES: &[&str] = &["className", "class"];

// ---------------------------------------------------------------------------
// Per-file facts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Imported {
    Default,
    Named(String),
    Namespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ImportBinding {
    pub local: String,
    pub spec: String,
    pub imported: Imported,
    pub line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ClassUse {
    pub token: String,
    pub line: u32,
    pub owner: Option<String>,
    /// Appears in a branch, logical operand or helper object key.
    pub conditional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ModuleClassUse {
    /// Index into `imports` of the stylesheet binding.
    pub binding: usize,
    pub class: String,
    pub line: u32,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RenderUse {
    /// Local component name used as a JSX tag.
    pub local: String,
    pub line: u32,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PropertyUse {
    pub name: String,
    pub line: u32,
    pub owner: Option<String>,
    pub writes: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FileScan {
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

fn is_component_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Whitespace-separated tokens of a static class string.
fn class_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace().filter(|token| !token.is_empty() && token.len() <= 256)
}

/// Scan one parsed module.
fn scan_module(path: &str, source: &str, module: &OxcModule<'_>) -> FileScan {
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

// ---------------------------------------------------------------------------
// Cross-file graph
// ---------------------------------------------------------------------------

/// Join `spec` to the directory of `file`, collapsing `.` and `..`.
/// Returns `None` for non-relative specifiers or escapes above the root.
pub(super) fn resolve_relative(file: &str, spec: &str) -> Option<String> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    for part in spec.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

const CODE_EXTENSIONS: &[&str] = &["tsx", "ts", "jsx", "js", "mjs", "cjs", "mts", "cts"];

/// The indexed file a relative module specifier names, probing extensions and
/// index files the way bundlers do.
fn resolve_module(files: &BTreeSet<String>, from: &str, spec: &str) -> Option<String> {
    let base = resolve_relative(from, spec)?;
    if files.contains(&base) {
        return Some(base);
    }
    for extension in CODE_EXTENSIONS {
        let candidate = format!("{base}.{extension}");
        if files.contains(&candidate) {
            return Some(candidate);
        }
    }
    for extension in CODE_EXTENSIONS {
        let candidate = format!("{base}/index.{extension}");
        if files.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn component_key(file: &str, name: &str) -> Key {
    node_key(ImpactDomain::Code, "component", file, name)
}
fn module_key(file: &str) -> Key {
    node_key(ImpactDomain::Code, "module", file, "")
}

struct Resolver<'a> {
    scans: &'a BTreeMap<String, FileScan>,
    files: BTreeSet<String>,
}
impl Resolver<'_> {
    /// Component key behind `exported` of `file`, following barrels a few hops.
    fn export_target(&self, file: &str, exported: &str, hops: u8) -> Option<Key> {
        let scan = self.scans.get(file)?;
        if let Some(local) = scan.exports.get(exported) {
            if scan.components.contains_key(local) {
                return Some(component_key(file, local));
            }
            // `export default Foo` where Foo is imported or wrapped: treat the
            // local name as the component identity in this file.
            return is_component_name(local).then(|| component_key(file, local));
        }
        if hops == 0 {
            return None;
        }
        let (spec, imported) = scan.reexports.get(exported)?;
        let target = resolve_module(&self.files, file, spec)?;
        self.export_target(&target, if imported == "*" { exported } else { imported }, hops - 1)
    }
    fn import_target(&self, file: &str, binding: &ImportBinding) -> Option<Key> {
        let target = resolve_module(&self.files, file, &binding.spec)?;
        let exported = match &binding.imported {
            Imported::Default => "default".to_owned(),
            Imported::Named(name) => name.clone(),
            Imported::Namespace => return None,
        };
        self.export_target(&target, &exported, 4)
    }
}

/// Build the code graph from scans. Pure over `scans`, unit-testable.
pub(super) fn build_graph(scans: &BTreeMap<String, FileScan>) -> (BTreeMap<Key, Node>, Vec<Edge>, Vec<ImpactGap>) {
    let files: BTreeSet<String> = scans.keys().cloned().collect();
    let resolver = Resolver { scans, files };
    let mut nodes: BTreeMap<Key, Node> = BTreeMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut alias_imports = 0u32;
    let mut dynamic = 0u32;
    let mut unresolved_renders = 0u32;
    let mut owner_node = |nodes: &mut BTreeMap<Key, Node>, file: &str, owner: &Option<String>, line: u32| -> Key {
        match owner {
            Some(name) => {
                let key = component_key(file, name);
                nodes.entry(key).or_insert_with(|| {
                    let mut node = Node::new(ImpactDomain::Code, "component", file, name);
                    node.path = Some(file.to_owned());
                    node.line = scans.get(file).and_then(|scan| scan.components.get(name).copied()).or(Some(line));
                    node
                });
                key
            }
            None => {
                let key = module_key(file);
                nodes.entry(key).or_insert_with(|| {
                    let mut node = Node::new(ImpactDomain::Code, "module", file, file);
                    node.path = Some(file.to_owned());
                    node.line = Some(1);
                    node
                });
                key
            }
        }
    };
    for (file, scan) in scans {
        dynamic += scan.dynamic_class_expressions;
        // Declared components exist even when nothing links to them yet.
        for (name, line) in &scan.components {
            nodes.entry(component_key(file, name)).or_insert_with(|| {
                let mut node = Node::new(ImpactDomain::Code, "component", file, name);
                node.path = Some(file.clone());
                node.line = Some(*line);
                node
            });
        }
        for binding in &scan.imports {
            if (binding.spec.starts_with("@/") || binding.spec.starts_with("~/") || binding.spec.starts_with('#'))
                && resolve_relative(file, &binding.spec).is_none()
            {
                alias_imports += 1;
            }
        }
        // Class tokens.
        for usage in &scan.class_uses {
            let to = owner_node(&mut nodes, file, &usage.owner, usage.line);
            edges.push(Edge {
                from: global_class_key(&usage.token),
                to,
                strength: ImpactStrength::Candidate,
                relation: "code.uses-class",
                why: format!(
                    "{}class \"{}\" in className at line {}",
                    if usage.conditional { "conditional " } else { "" },
                    usage.token,
                    usage.line
                ),
            });
        }
        // CSS-module members.
        for usage in &scan.module_class_uses {
            let binding = &scan.imports[usage.binding];
            let Some(target) = resolve_relative(file, &binding.spec) else {
                alias_imports += 1;
                continue;
            };
            let to = owner_node(&mut nodes, file, &usage.owner, usage.line);
            edges.push(Edge {
                from: module_class_key(&target, &usage.class),
                to,
                strength: ImpactStrength::Exact,
                relation: "code.uses-module-class",
                why: format!("{}.{} from {} at line {}", binding.local, usage.class, binding.spec, usage.line),
            });
        }
        // Stylesheet imports reach the importing module and its components.
        let sheet_imports = scan
            .imports
            .iter()
            .map(|binding| (binding.spec.as_str(), binding.line))
            .chain(scan.side_effect_imports.iter().map(|(spec, line)| (spec.as_str(), *line)))
            .filter(|(spec, _)| spec.ends_with(".css"));
        let mut importer = None;
        for (spec, line) in sheet_imports {
            let Some(target) = resolve_relative(file, spec) else {
                alias_imports += 1;
                continue;
            };
            let module = *importer.get_or_insert_with(|| owner_node(&mut nodes, file, &None, line));
            edges.push(Edge {
                from: stylesheet_key(&target),
                to: module,
                strength: ImpactStrength::Exact,
                relation: "code.imports-stylesheet",
                why: format!("imported by {file} at line {line}"),
            });
            for name in scan.components.keys() {
                let ckey = owner_node(&mut nodes, file, &Some(name.clone()), line);
                if scan.exports.values().any(|local| local == name) {
                    edges.push(Edge {
                        from: module,
                        to: ckey,
                        strength: ImpactStrength::Candidate,
                        relation: "code.module-component",
                        why: format!("{name} lives in a module which loads {spec}"),
                    });
                }
            }
        }
        // Component renders.
        for render in &scan.renders {
            let Some(parent) = Some(owner_node(&mut nodes, file, &render.owner, render.line)) else { continue };
            let child = if let Some(binding) = scan.imports.iter().find(|binding| binding.local == render.local) {
                resolver.import_target(file, binding)
            } else if scan.components.contains_key(&render.local) {
                Some(component_key(file, &render.local))
            } else {
                None
            };
            match child {
                Some(child) if nodes.contains_key(&child) || child != parent => {
                    // The child node may live in a file not yet visited.
                    edges.push(Edge {
                        from: child,
                        to: parent,
                        strength: ImpactStrength::Exact,
                        relation: "code.renders",
                        why: format!("<{}> rendered at line {}", render.local, render.line),
                    });
                }
                _ => unresolved_renders += 1,
            }
        }
        // Custom properties used from code.
        for usage in &scan.property_uses {
            let node = owner_node(&mut nodes, file, &usage.owner, usage.line);
            if usage.writes {
                edges.push(Edge {
                    from: node,
                    to: custom_property_key(&usage.name),
                    strength: ImpactStrength::Candidate,
                    relation: "code.sets-property",
                    why: format!("sets {} at line {}", usage.name, usage.line),
                });
            } else {
                edges.push(Edge {
                    from: custom_property_key(&usage.name),
                    to: node,
                    strength: ImpactStrength::Candidate,
                    relation: "code.reads-property",
                    why: format!("reads {} at line {}", usage.name, usage.line),
                });
            }
        }
    }
    // Make sure every component endpoint has a node (children in unvisited files).
    for (file, scan) in scans {
        for (name, line) in &scan.components {
            nodes.entry(component_key(file, name)).or_insert_with(|| {
                let mut node = Node::new(ImpactDomain::Code, "component", file, name);
                node.path = Some(file.clone());
                node.line = Some(*line);
                node
            });
        }
    }
    let mut gaps = vec![
        ImpactGap {
            facet: "code.render-model".to_owned(),
            note: "class and render links are lexical: conditional rendering, portals, slots, runtime-built class names and spread props are not followed".to_owned(),
            count: None,
        },
        ImpactGap {
            facet: "code.build-transform".to_owned(),
            note: "bundler class-name transforms (CSS-module hashing, utility generators, CSS-in-JS) are not read; a literal class link is a candidate".to_owned(),
            count: None,
        },
    ];
    if dynamic > 0 {
        gaps.push(ImpactGap {
            facet: "code.dynamic-class".to_owned(),
            note: "className expressions with runtime parts; only their static tokens are linked".to_owned(),
            count: Some(dynamic),
        });
    }
    if alias_imports > 0 {
        gaps.push(ImpactGap {
            facet: "code.alias-imports".to_owned(),
            note: "imports through path aliases or packages are not resolved, so links through them are missing".to_owned(),
            count: Some(alias_imports),
        });
    }
    if unresolved_renders > 0 {
        gaps.push(ImpactGap {
            facet: "code.unresolved-renders".to_owned(),
            note: "JSX tags whose component could not be bound to an indexed file".to_owned(),
            count: Some(unresolved_renders),
        });
    }
    gaps.push(ImpactGap {
        facet: "code.vue".to_owned(),
        note: "Vue single-file components are not indexed, so their <style> and class usage are not seen".to_owned(),
        count: None,
    });
    (nodes, edges, gaps)
}

// ---------------------------------------------------------------------------
// Reading the selected index
// ---------------------------------------------------------------------------

struct IndexedFile {
    path: String,
    length: Option<u64>,
    source: Key,
}

fn profile_for(path: &str) -> Option<TypeScriptSource> {
    match path.rsplit('.').next()? {
        "tsx" | "jsx" | "js" | "mjs" | "cjs" => Some(TypeScriptSource::Tsx),
        "ts" | "mts" | "cts" => Some(TypeScriptSource::TypeScript),
        _ => None,
    }
}

fn indexed_files(cx: &ImpactContext<'_>) -> Result<Vec<IndexedFile>, String> {
    use backend_engine::builtin::{native_source_evidence_relation_budgeted, native_source_evidence_page_budgeted};
    let mut budget = backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES;
    let mut out: Vec<IndexedFile> = Vec::new();
    // Native TypeScript evidence.
    if let Some(relation) = native_source_evidence_relation_budgeted(cx.snapshot, &mut budget).map_err(|e| format!("native evidence: {e}"))? {
        let mut after = None;
        loop {
            let page = native_source_evidence_page_budgeted(&relation, after.as_ref(), &mut budget)
                .map_err(|e| format!("native evidence page: {e}"))?;
            let entries = page.entries();
            if entries.is_empty() {
                break;
            }
            for (key, row) in entries {
                after = Some(*key);
                if row.project == cx.project && row.admits_image() && profile_for(&row.path).is_some() {
                    out.push(IndexedFile { path: row.path.clone(), length: Some(u64::from(row.byte_length)), source: row.source_digest });
                }
            }
            if out.len() > MAX_FILES * 4 {
                break;
            }
        }
    }
    // Structural lane records.
    let package = backend_engine::PackageKey::from_value(cx.package.as_str());
    if let Ok(sources) = super::super::read_package_sources(cx.snapshot, package) {
        for (_, record) in &sources.files {
            let Some(fields) = record.file_fields() else { continue };
            if fields.project != cx.project || profile_for(fields.path).is_none() {
                continue;
            }
            if let Some(identity) = fields.source_identity {
                out.push(IndexedFile { path: fields.path.to_owned(), length: None, source: *identity.as_ref() });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    Ok(out)
}

impl ImpactAdapter for CodeAdapter {
    fn domain(&self) -> ImpactDomain {
        ImpactDomain::Code
    }

    fn load(&self, cx: &ImpactContext<'_>) -> DomainLoad {
        use backend_version::{ContentId, SourceFactDomain};
        let empty = |state: ImpactDomainState| DomainLoad::empty(ImpactDomain::Code, state);
        let Some(root) = cx.project_root.clone() else {
            return empty(ImpactDomainState::Unavailable { reason: "the project directory is not known to the owner".to_owned() });
        };
        let listed = match indexed_files(cx) {
            Ok(files) => files,
            Err(error) => return empty(ImpactDomainState::Unavailable { reason: format!("the indexed sources could not be listed: {error}") }),
        };
        let mut scans: BTreeMap<String, FileScan> = BTreeMap::new();
        let mut witnesses = Vec::new();
        let mut gaps = Vec::new();
        let mut reasons = Vec::new();
        let (mut stale, mut unparsed, mut total) = (Vec::new(), Vec::new(), 0usize);
        for (index, file) in listed.iter().enumerate() {
            if (cx.cancelled)() {
                return empty(ImpactDomainState::Unavailable { reason: "cancelled".to_owned() });
            }
            if index >= MAX_FILES || total >= MAX_TOTAL_BYTES {
                reasons.push(format!("only the first {index} of {} sources were scanned", listed.len()));
                gaps.push(ImpactGap {
                    facet: "code.scan-limit".to_owned(),
                    note: "the source scan budget was reached; later files were not read".to_owned(),
                    count: Some((listed.len() - index) as u32),
                });
                break;
            }
            let bytes = match super::super::ingest::read_stylo_selected_source(&root, &file.path, MAX_FILE_BYTES) {
                Ok(bytes) => bytes,
                Err(_) => {
                    stale.push(file.path.clone());
                    continue;
                }
            };
            total += bytes.len();
            let current = *ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes).as_ref();
            witnesses.push(FileWitness {
                root: root.clone(),
                path: file.path.clone(),
                length: file.length.unwrap_or(bytes.len() as u64),
                source: file.source,
            });
            if current != file.source || file.length.is_some_and(|length| length != bytes.len() as u64) {
                stale.push(file.path.clone());
                continue;
            }
            let (Ok(text), Some(profile)) = (std::str::from_utf8(&bytes), profile_for(&file.path)) else {
                unparsed.push(file.path.clone());
                continue;
            };
            match with_analysis(profile, text, |module| scan_module(&file.path, text, &module)) {
                Ok(scan) => {
                    scans.insert(file.path.clone(), scan);
                }
                Err(_) => unparsed.push(file.path.clone()),
            }
        }
        if !stale.is_empty() {
            reasons.push(format!("{} source files changed since indexing and were not scanned", stale.len()));
            gaps.push(ImpactGap {
                facet: "code.stale-files".to_owned(),
                note: format!("changed since indexing, not scanned: {}", stale.iter().take(4).cloned().collect::<Vec<_>>().join(", ")),
                count: Some(stale.len() as u32),
            });
        }
        if !unparsed.is_empty() {
            reasons.push(format!("{} source files do not parse", unparsed.len()));
            gaps.push(ImpactGap {
                facet: "code.unparsed-files".to_owned(),
                note: format!("syntax errors, not scanned: {}", unparsed.iter().take(4).cloned().collect::<Vec<_>>().join(", ")),
                count: Some(unparsed.len() as u32),
            });
        }
        if listed.is_empty() {
            gaps.push(ImpactGap {
                facet: "code.no-sources".to_owned(),
                note: "no TypeScript or JavaScript sources are indexed for this project, so code that uses these styles was not searched".to_owned(),
                count: None,
            });
        }
        let (nodes, edges, graph_gaps) = build_graph(&scans);
        gaps.extend(graph_gaps);
        let state = if reasons.is_empty() { ImpactDomainState::Analyzed } else { ImpactDomainState::Partial { reasons } };
        DomainLoad {
            nodes,
            edges,
            gaps,
            basis: ImpactDomainBasis { domain: ImpactDomain::Code, state, files: scans.len() as u32 },
            files: witnesses,
            known_paths: listed.iter().map(|file| file.path.clone()).collect(),
        }
    }

    fn seed_matches(&self, spec: &ImpactSeedSpec, node: &Node) -> bool {
        match spec.kind.as_str() {
            "component" => {
                node.kind == "component"
                    && node.name == spec.name
                    && spec.path.as_ref().is_none_or(|path| node.path.as_ref() == Some(path))
            }
            "file" => node.kind == "module" && node.path.as_deref() == Some(spec.name.trim_start_matches("./")),
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn scan(path: &str, source: &str) -> FileScan {
        let profile = profile_for(path).expect("profile");
        with_analysis(profile, source, |module| scan_module(path, source, &module)).expect("parse")
    }

    #[test]
    fn relative_resolution_collapses_and_refuses_escapes() {
        assert_eq!(resolve_relative("src/ui/Card.tsx", "./Card.module.css").as_deref(), Some("src/ui/Card.module.css"));
        assert_eq!(resolve_relative("src/ui/Card.tsx", "../theme.css").as_deref(), Some("src/theme.css"));
        assert_eq!(resolve_relative("a.tsx", "../x.css"), None);
        assert_eq!(resolve_relative("src/a.tsx", "react"), None);
        assert_eq!(resolve_relative("src/a.tsx", "@/x.css"), None);
    }

    #[test]
    fn class_names_are_read_from_literals_templates_and_helpers() {
        let source = r#"
import clsx from "clsx";
export function Button({ big, kind }) {
  return <button className={clsx("btn", big && "btn-big", { active: big, "is-on": kind }, `x ${kind} y`)}>go</button>;
}
export const Plain = () => <div className="a b" class="c" />;
"#;
        let scan = scan("src/Button.tsx", source);
        let tokens: BTreeSet<&str> = scan.class_uses.iter().map(|usage| usage.token.as_str()).collect();
        for expected in ["btn", "btn-big", "active", "is-on", "x", "y", "a", "b", "c"] {
            assert!(tokens.contains(expected), "{expected} missing from {tokens:?}");
        }
        assert!(scan.class_uses.iter().any(|usage| usage.token == "btn-big" && usage.conditional));
        assert!(!scan.class_uses.iter().find(|usage| usage.token == "btn").expect("btn").conditional);
        assert_eq!(scan.dynamic_class_expressions, 1);
        assert_eq!(scan.class_uses.iter().find(|usage| usage.token == "a").expect("a").owner.as_deref(), Some("Plain"));
        assert_eq!(scan.class_uses.iter().find(|usage| usage.token == "btn").expect("btn").owner.as_deref(), Some("Button"));
    }

    #[test]
    fn partial_words_at_a_template_boundary_are_not_class_names() {
        let scan = scan("a.tsx", "export const A = ({s}) => <i className={`btn-${s} x`} />;");
        let tokens: BTreeSet<&str> = scan.class_uses.iter().map(|usage| usage.token.as_str()).collect();
        assert!(tokens.contains("x") && !tokens.contains("btn-"), "{tokens:?}");
    }

    #[test]
    fn module_members_need_a_stylesheet_import_binding() {
        let source = r#"
import styles from "./Card.module.css";
import other from "./other";
export function Card() { return <div className={styles.card + " " + styles["title"]}>{other.card}</div>; }
"#;
        let scan = scan("src/Card.tsx", source);
        let classes: BTreeSet<&str> = scan.module_class_uses.iter().map(|usage| usage.class.as_str()).collect();
        assert_eq!(classes, BTreeSet::from(["card", "title"]));
    }

    #[test]
    fn custom_properties_used_from_code_are_found() {
        let source = r#"
export function Theme({ el }) {
  el.style.setProperty("--brand", "red");
  const v = getComputedStyle(el).getPropertyValue("--accent");
  return <div style={{ "--gap": 4 }} />;
}
"#;
        let scan = scan("src/Theme.tsx", source);
        let uses: BTreeSet<(&str, bool)> = scan.property_uses.iter().map(|usage| (usage.name.as_str(), usage.writes)).collect();
        assert!(uses.contains(&("--brand", true)));
        assert!(uses.contains(&("--accent", false)));
        assert!(uses.contains(&("--gap", true)));
    }

    #[test]
    fn graph_links_css_to_components_and_components_to_their_renderers() {
        let mut scans = BTreeMap::new();
        scans.insert(
            "src/Button.tsx".to_owned(),
            scan(
                "src/Button.tsx",
                r#"import styles from "./Button.module.css";
import "./theme.css";
export function Button() { return <button className={styles.btn + " shared"} />; }"#,
            ),
        );
        scans.insert(
            "src/Page.tsx".to_owned(),
            scan(
                "src/Page.tsx",
                r#"import { Button } from "./Button";
export default function Page() { return <main><Button /></main>; }"#,
            ),
        );
        let (nodes, edges, _) = build_graph(&scans);
        let exact_module = edges
            .iter()
            .find(|edge| edge.relation == "code.uses-module-class")
            .expect("module class edge");
        assert_eq!(exact_module.strength, ImpactStrength::Exact);
        assert_eq!(exact_module.from, module_class_key("src/Button.module.css", "btn"));
        let shared = edges.iter().find(|edge| edge.relation == "code.uses-class").expect("class edge");
        assert_eq!(shared.strength, ImpactStrength::Candidate);
        assert_eq!(shared.from, global_class_key("shared"));
        let render = edges.iter().find(|edge| edge.relation == "code.renders").expect("render edge");
        assert_eq!(render.strength, ImpactStrength::Exact);
        assert_eq!(nodes[&render.from].name, "Button");
        assert_eq!(nodes[&render.to].name, "Page");
        assert!(edges.iter().any(|edge| edge.relation == "code.imports-stylesheet" && edge.from == stylesheet_key("src/theme.css")));
    }

    #[test]
    fn css_changes_reach_components_and_their_renderers_across_both_adapters() {
        use super::super::impact_css::{CssAdapter, CssModel, HostModel};
        use backend_library::impact::{
            ImpactRequest, ImpactSeed, ImpactSeedSpec, ImpactSeedState, ImpactStrength,
        };
        use backend_semantic::stylo_analysis::{
            StyloDependencyCoverage, StyloLocation, StyloRule, StyloRuleKind, StyloSelector,
        };
        let style_rule = |path: &[u32], line: u32| StyloRule {
            path: path.to_vec(),
            kind: StyloRuleKind::Style,
            native_kind: "Style".into(),
            condition: None,
            location: Some(StyloLocation { line, column: 1, approximate: false }),
        };
        let selector = |path: &[u32], canonical: &str| StyloSelector {
            rule_path: path.to_vec(),
            ordinal: 0,
            canonical: canonical.into(),
            specificity: 0,
            components: Vec::new(),
            component_coverage: StyloDependencyCoverage::Unknown,
        };
        let module = HostModel {
            key: [1; 32],
            path: "src/Button.module.css".into(),
            parsed: true,
            rules: vec![style_rule(&[0], 0)],
            selectors: vec![selector(&[0], ".btn")],
            ..HostModel::default()
        };
        let global = HostModel {
            key: [2; 32],
            path: "src/theme.css".into(),
            parsed: true,
            rules: vec![style_rule(&[0], 0)],
            selectors: vec![selector(&[0], ".shared")],
            ..HostModel::default()
        };
        let css = CssModel {
            state: ImpactDomainState::Analyzed,
            source_root: None,
            hosts: vec![module, global],
            inventory_incomplete: false,
        };
        let (css_load, _) = super::super::impact_css::build_graph(&css, &|_, _| None);
        let mut scans = BTreeMap::new();
        scans.insert(
            "src/Button.tsx".to_owned(),
            scan(
                "src/Button.tsx",
                r#"import styles from "./Button.module.css";
export function Button() { return <button className={styles.btn + " shared"} />; }"#,
            ),
        );
        scans.insert(
            "src/Page.tsx".to_owned(),
            scan(
                "src/Page.tsx",
                r#"import { Button } from "./Button";
export default function Page() { return <Button />; }"#,
            ),
        );
        let (nodes, edges, gaps) = build_graph(&scans);
        let code_load = DomainLoad {
            nodes,
            edges,
            gaps,
            basis: ImpactDomainBasis { domain: ImpactDomain::Code, state: ImpactDomainState::Analyzed, files: 2 },
            files: Vec::new(),
            known_paths: BTreeSet::new(),
        };
        let seed = |kind: &str, name: &str| {
            ImpactRequest::new(
                backend_library::PackageReference::parse("app".to_owned()).expect("package"),
                ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Css, kind: kind.into(), name: name.into(), path: None }),
            )
        };
        let run = |request: &ImpactRequest| {
            let (css_load, _) = super::super::impact_css::build_graph(&css, &|_, _| None);
            let _ = &css_load;
            super::super::impact_framework::answer(
                [9; 32],
                [2; 32],
                &[7; 32],
                vec![
                    (ImpactDomain::Css, super::super::impact_css::build_graph(&css, &|_, _| None).0),
                    (ImpactDomain::Code, DomainLoad {
                        nodes: code_load.nodes.clone(),
                        edges: code_load.edges.clone(),
                        gaps: code_load.gaps.clone(),
                        basis: ImpactDomainBasis { domain: ImpactDomain::Code, state: ImpactDomainState::Analyzed, files: 2 },
                        files: Vec::new(),
                        known_paths: BTreeSet::new(),
                    }),
                ],
                &|spec, node| match node.domain {
                    ImpactDomain::Css => CssAdapter.seed_matches(spec, node),
                    _ => CodeAdapter.seed_matches(spec, node),
                },
                request,
                &|| false,
            )
            .expect("answer")
        };
        let _ = css_load;
        // The module class is an exact link to Button; Page renders Button.
        let report = run(&seed("selector", ".btn"));
        assert!(matches!(report.seed_state, ImpactSeedState::Resolved));
        let button = report.rows.iter().find(|row| row.node.name == "Button").expect("Button affected");
        assert_eq!(button.strength, ImpactStrength::Exact);
        assert_eq!(button.relation, "code.uses-module-class");
        let page = report.rows.iter().find(|row| row.node.name == "Page").expect("Page affected");
        assert_eq!(page.strength, ImpactStrength::Exact);
        assert_eq!(page.depth, 3);
        assert_eq!(page.via, report.rows.iter().find(|r| r.node.name == "Button").map(|r| r.n).expect("via"));
        // A global class used by literal is only a candidate, and so is everything after it.
        let shared = run(&seed("selector", ".shared"));
        let button = shared.rows.iter().find(|row| row.node.name == "Button").expect("Button affected");
        assert_eq!(button.strength, ImpactStrength::Candidate);
        assert_eq!(shared.rows.iter().find(|row| row.node.name == "Page").expect("Page").strength, ImpactStrength::Candidate);
        // Dependencies walk the same graph backwards: Page relies on Button which relies on the module rule.
        let mut back = seed("component", "Page");
        back.seed = ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Code, kind: "component".into(), name: "Page".into(), path: None });
        back.direction = backend_library::impact::ImpactDirection::Dependencies;
        back.depth = 4;
        let deps = run(&back);
        assert!(deps.rows.iter().any(|row| row.node.name == "Button"), "{deps:?}");
        assert!(deps.rows.iter().any(|row| row.node.kind == "rule" && row.node.name == ".btn"), "{deps:?}");
    }
}
