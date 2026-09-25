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

use oxc_ast::{
    AstKind,
    ast::{
        AccessorPropertyType, Argument, AssignmentOperator, AssignmentTarget, BindingPattern,
        CallExpression, Class, ClassElement, Declaration, ExportDefaultDeclarationKind, Expression,
        Function, MethodDefinitionKind, MethodDefinitionType, PropertyDefinitionType, PropertyKey,
        Statement, TSAccessibility,         TSEnumDeclaration, TSEnumMemberName, TSGlobalDeclaration, TSInterfaceDeclaration,
        TSModuleDeclaration, TSModuleDeclarationBody, TSModuleDeclarationName, TSModuleReference,
        TSSignature,
        VariableDeclaration, VariableDeclarationKind,
    },
};
use oxc_ast_visit::Visit;
use oxc_semantic::{Reference, Semantic};
use oxc_span::{GetSpan, Span};
use oxc_syntax::module_record::{
    ExportExportName, ExportImportName, ExportLocalName, ImportImportName, ModuleRecord,
};

use super::{
    Accessibility, AttrTok, ClassBody, ClassFlags, ConstBody, DeclBody, DeclFact, EnumBody,
    ExportTable, FunctionBody, ImportFact, ImportName, IndexSignatureFact, IndirectExport,
    InterfaceBody, LocalExport, MemberFact, MemberKind, MemberModifiers, MethodFact, ModuleFacts,
    NamespaceBody, OccurrenceFact, OccurrenceKind, ParamFact, PropertyFact, ReceiverKind,
    SignatureKind, StarExport, StaticBody, TypeAliasBody, TypeOwned, VariantFact, jsdoc,
    types::{lower_ts_type, lower_ts_type_with_params, lower_type_params},
};

// ── Entry point
// ────────────────────────────────────────────────────────────────

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

    let default_local_name: Option<String> =
        module_record.local_export_entries.iter().find_map(|e| {
            if matches!(e.export_name, ExportExportName::Default(_)) {
                match &e.local_name {
                    ExportLocalName::Default(ns) | ExportLocalName::Name(ns) => {
                        Some(ns.name.to_string())
                    }
                    ExportLocalName::Null => None,
                }
            } else {
                None
            }
        });

    // ── CommonJS export recognition ───────────────────────────────────────────
    // `module_record`/`exported_names` above sees ES `export` syntax only.
    // Every CommonJS export (`module.exports = X`, `exports.X = Y`, …) is
    // syntactically an `ExpressionStatement`, invisible to that machinery.
    // Union the local identifiers these assignments name into the same
    // visibility set ESM declarations already consult below, so
    // `is_exported`/`is_public` is correct for both module systems through
    // one code path — the "honesty gate": without this, a CommonJS-exported
    // declaration is real (declared, counted) but `Visibility::Private`,
    // which is not actually honest about what the package's public API is.
    let runtime_exports = scan_runtime_exports(program);
    let mut cjs_exports = scan_commonjs_exports(&program.body);
    for runtime_export in &runtime_exports {
        if runtime_export.function.is_some() {
            cjs_exports.named.push((
                runtime_export.export_name.clone(),
                runtime_export.local_name.clone(),
            ));
        }
    }
    let mut visible_names = exported_names.clone();
    if let Some(name) = &cjs_exports.whole_module {
        visible_names.insert(name.clone());
    }
    for (_, local) in &cjs_exports.named {
        visible_names.insert(local.clone());
    }

    // ── Walk program body ─────────────────────────────────────────────────────
    for stmt in &program.body {
        let decls = extract_statement(
            stmt,
            source,
            semantic,
            path,
            &visible_names,
            &default_local_name,
            &mut name_counts,
        );
        declarations.extend(decls);
    }

    // CommonJS packages may return an object assembled inside a function:
    // `function setup() { createDebug.enable = enable; return createDebug; }`.
    // These assignments are part of the exported runtime surface even though
    // neither the property nor its function declaration is top-level syntax.
    // Recover only identifier-backed function members; expression-backed
    // values (notably `require('ms')`) are intentionally not fabricated as
    // constants.
    for runtime_export in runtime_exports {
        let Some(function) = runtime_export.function else {
            continue;
        };
        let name = runtime_export.local_name;
        let span = function.span();
        if declarations
            .iter()
            .any(|decl| decl.name == name && decl.span_start == span.start)
        {
            continue;
        }
        let decl_index = bump_count(&name, &mut name_counts);
        declarations.push(DeclFact {
            name,
            visibility: nudox_ir::entry::Visibility::Public,
            doc: jsdoc::jsdoc_for_span(semantic, span),
            body: DeclBody::Function(lower_function(function, source)),
            module: path.to_path_buf(),
            span_start: span.start,
            span_end: span.end,
            is_default: false,
            decl_index,
        });
    }

    // `exports.useState = function (initialState) { ... }` is an owned
    // function. `var Router = require("router"); exports.Router = Router`
    // is a reference to that package. Both are assignment expressions, so
    // the statement walk above never sees them.
    push_commonjs_value_decls(
        &program.body,
        source,
        semantic,
        path,
        &mut declarations,
        &mut name_counts,
        &mut cjs_exports,
    );

    // ── Reference occurrences ─────────────────────────────────────────────────
    // Must run after `declarations` is fully built: `record_occurrences` needs
    // the whole list to resolve both ends of every edge (see its doc comment).
    let occurrences = record_occurrences(semantic, &declarations);

    // ── Export table ──────────────────────────────────────────────────────────
    let exports = build_export_table(
        module_record,
        &exported_names,
        &default_local_name,
        &declarations,
        &cjs_exports,
    );

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
        occurrences,
        source_len: source.len(),
    }
}

// ── Reference occurrences
// ───────────────────────────────────────────────────────

/// Build this module's same-module occurrence graph from OXC's resolved
/// symbol/reference table. See [`OccurrenceFact`]'s doc comment for the
/// same-module and top-level-only restrictions this necessarily inherits.
///
/// OXC's `SemanticBuilder` (already run by `graph::build_and_extract` before
/// this is called) resolves every identifier to a declaring `SymbolId` and
/// keeps that symbol's full reference list — the same binder-level name
/// resolution a real compiler front end does, not a heuristic. Walking
/// `symbol_ids()` once and, for each, its `symbol_references()`, visits every
/// resolved binding/use pair in the file exactly once.
fn record_occurrences(semantic: &Semantic<'_>, declarations: &[DeclFact]) -> Vec<OccurrenceFact> {
    let mut out = Vec::new();
    for symbol_id in semantic.scoping().symbol_ids() {
        let decl_span = semantic.scoping().symbol_span(symbol_id);
        let Some(target) = innermost_declaration(declarations, decl_span.start, decl_span.end)
        else {
            // Not a top-level declaration of this module: a parameter, a
            // local variable/const, a destructured binding, an import
            // binding, a catch clause name, … `OccurrenceFact` only tracks
            // edges between this module's own top-level declarations.
            continue;
        };

        for reference in semantic.symbol_references(symbol_id) {
            let ref_span = semantic.nodes().get_node(reference.node_id()).kind().span();
            let Some(owner) = innermost_declaration(declarations, ref_span.start, ref_span.end)
            else {
                // A reference at module top level, outside every declaration
                // (e.g. a bare `console.log(someExport)` statement) has
                // nothing to attribute the edge to.
                continue;
            };

            // A reference is not a caller of its own declaration. The
            // clearest case is direct recursion (`function f() { f(); }`),
            // but the same equality also catches every reference that is
            // merely *nested inside* its own target at the top-level
            // granularity this graph tracks: a parameter or local variable
            // used inside the function that declares it (its declaring span
            // and every use of it both fall inside that one enclosing
            // `DeclFact`), or a call between two members of the same
            // namespace (see `OccurrenceFact`'s doc comment). None of those
            // are a useful "who calls this" row, and recording them would
            // make every recursive function — and every namespace with more
            // than one member — its own caller.
            if owner == target {
                continue;
            }

            let kind = classify_reference(semantic, reference);
            out.push(OccurrenceFact {
                owner,
                target,
                span_start: ref_span.start,
                span_end: ref_span.end,
                kind,
            });
        }
    }
    out
}

/// The top-level declaration whose span most tightly contains `[start, end)`,
/// or `None` when nothing does.
///
/// Top-level declarations never nest inside one another — a nested function
/// declaration inside another function's body is never walked into by
/// `extract_statement`, and a namespace's own children live in
/// `NamespaceBody::children`, not in this list — so in practice at most one
/// candidate ever contains a given span. `min_by_key` on span width exists
/// only to make the choice deterministic if that ever stops being true,
/// rather than depending on `declarations`' iteration order.
fn innermost_declaration(declarations: &[DeclFact], start: u32, end: u32) -> Option<usize> {
    declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.span_start <= start && end <= d.span_end)
        .min_by_key(|(_, d)| d.span_end - d.span_start)
        .map(|(idx, _)| idx)
}

/// Classify one resolved reference for [`OccurrenceFact::kind`].
///
/// `ReferenceFlags::Type` (checked via `Reference::is_type`) already tells
/// type-position references apart from value references — OXC's binder keeps
/// type and value name resolution in separate namespaces precisely so a type
/// and a value can share a name without colliding, and that flag is set
/// exactly when the reference was resolved through the type namespace. A
/// value reference is further split into `Call` — this identifier is exactly
/// a `CallExpression`'s callee, not one of its arguments or something else
/// entirely — versus every other value use, by checking the reference site's
/// immediate parent node.
fn classify_reference(semantic: &Semantic<'_>, reference: &Reference) -> OccurrenceKind {
    if reference.flags().is_type() {
        return OccurrenceKind::Type;
    }
    let ref_span = semantic.nodes().get_node(reference.node_id()).kind().span();
    if let AstKind::CallExpression(call) = semantic.nodes().parent_kind(reference.node_id())
        && call.callee.span() == ref_span
    {
        return OccurrenceKind::Call;
    }
    OccurrenceKind::ValueUse
}

// ── CommonJS export recognition
// ─────────────────────────────────────────────────

struct RuntimeExport<'a> {
    export_name: String,
    local_name: String,
    function: Option<&'a Function<'a>>,
}

#[derive(Default)]
struct RuntimeExportVisitor<'a> {
    functions: Vec<&'a Function<'a>>,
    returns: Vec<(Span, String)>,
    assignments: Vec<(Span, String, String, String)>,
}

impl<'a> Visit<'a> for RuntimeExportVisitor<'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        match kind {
            AstKind::Function(function) => {
                if function.id.is_some() {
                    self.functions.push(function);
                }
            }
            AstKind::ReturnStatement(return_statement) => {
                if let Some(Expression::Identifier(identifier)) = &return_statement.argument {
                    self.returns
                        .push((return_statement.span, identifier.name.to_string()));
                }
            }
            AstKind::AssignmentExpression(assignment)
                if assignment.operator == AssignmentOperator::Assign =>
            {
                let AssignmentTarget::StaticMemberExpression(member) = &assignment.left else {
                    return;
                };
                let Expression::Identifier(object) = &member.object else {
                    return;
                };
                let Some(local_name) = as_plain_identifier(&assignment.right) else {
                    return;
                };
                self.assignments.push((
                    assignment.span,
                    object.name.to_string(),
                    member.property.name.to_string(),
                    local_name,
                ));
            }
            _ => {}
        }
    }
}

/// Recover identifier-backed properties assigned to an object returned by a
/// function. This is deliberately structural rather than a package-specific
/// name rule: the innermost function containing `return value` owns assignments
/// to `value`, and only assignments whose RHS names a nested function become
/// declarations. Calls, literals, and object expressions remain unmodelled
/// values instead of becoming synthetic constants.
fn scan_runtime_exports<'a>(program: &'a oxc_ast::ast::Program<'a>) -> Vec<RuntimeExport<'a>> {
    let mut visitor = RuntimeExportVisitor::default();
    visitor.visit_program(program);

    let returned_objects: Vec<(Span, String)> = visitor
        .returns
        .iter()
        .filter_map(|(return_span, object_name)| {
            visitor
                .functions
                .iter()
                .filter(|function| {
                    let span = function.span();
                    span.start <= return_span.start && return_span.end <= span.end
                })
                .min_by_key(|function| {
                    let span = function.span();
                    span.end - span.start
                })
                .map(|function| (function.span(), object_name.clone()))
        })
        .collect();

    let mut out = Vec::new();
    for (assignment_span, object_name, export_name, local_name) in visitor.assignments {
        let Some((owner_span, _)) = returned_objects.iter().find(|(function_span, object)| {
            object == &object_name
                && function_span.start <= assignment_span.start
                && assignment_span.end <= function_span.end
        }) else {
            continue;
        };
        let function = visitor
            .functions
            .iter()
            .filter(|function| {
                let span = function.span();
                span.start >= owner_span.start
                    && span.end <= owner_span.end
                    && function
                        .id
                        .as_ref()
                        .is_some_and(|id| id.name.as_str() == local_name)
            })
            .min_by_key(|function| {
                let span = function.span();
                span.end - span.start
            })
            .copied();
        out.push(RuntimeExport {
            export_name,
            local_name,
            function,
        });
    }
    out
}

/// Local names this module exports via CommonJS `module.exports`/`exports`
/// assignment forms, discovered by a syntactic scan of the module's
/// top-level statement list.
///
/// Deliberately shallow: this crate never walks *inside* a function body
/// looking for further declarations (`lower_function` only reads params/
/// generics/return type), so a name assigned only from inside a closure —
/// `debug`'s `common.js` sets `createDebug.enable = enable` inside
/// `function setup(env) { ... }`, never at module top level in any file —
/// is a genuine, documented gap here, not a missed case. See
/// `tests/real_npm_packages.rs`'s `debug` fixture for the real-package
/// evidence this was checked against.
#[derive(Default)]
pub(crate) struct CommonJsExports {
    /// The identifier assigned via `module.exports = <ident>;` — this
    /// module's whole-module export target, the CommonJS analogue of
    /// `export default <ident>;`.
    pub(crate) whole_module: Option<String>,
    /// `(export-facing name, local identifier)` pairs from
    /// `exports.X = <ident>;`, `module.exports.X = <ident>;`, and
    /// `<export-binding>.X = <ident>;` (`<export-binding>` is
    /// `whole_module`'s identifier, once known — matched regardless of
    /// whether the `module.exports = <export-binding>;` statement that
    /// establishes it appears before or after this assignment in the file,
    /// which real packages do not keep consistent: `ws`'s `index.js` sets
    /// `WebSocket.createWebSocketStream = ...` several statements before
    /// its own `module.exports = WebSocket;`).
    pub(crate) named: Vec<(String, String)>,
}

/// True when `expr` is exactly `module.exports` (not `module.exports.X`,
/// which is one `StaticMemberExpression` deeper).
fn is_module_dot_exports(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::StaticMemberExpression(mem)
            if matches!(&mem.object, Expression::Identifier(id) if id.name == "module")
                && mem.property.name == "exports"
    )
}

/// True when `expr` is a CommonJS *import* expression — something that binds
/// a required module (or a name reached off one) to a local identifier, never
/// a real value the declaring package computed. Three shapes, all confirmed
/// live in real npm packages:
///
/// - `require("specifier")` — the bare call. Mirrors `graph.rs`'s
///   `as_require_call` (which additionally extracts the specifier, needed there
///   for graph edges and not here); kept as a separate, smaller check in this
///   module rather than sharing code across the `entry`/`extract`/`graph`
///   boundary. `ws`'s `const WebSocket = require('./lib/websocket');` is this
///   shape.
/// - `__importDefault(require("specifier"))` /
///   `__importStar(require("specifier"))` — `tsc`'s own
///   `esModuleInterop`-compiled output for `import x from "y"` / `import * as x
///   from "y"`. Recognizing only the bare call left every one of these
///   fabricated as a `Const` whose "value" was the interop-wrapped require call
///   rendered as if it were a string literal — `class-validator`'s bundled
///   `bundles/class-validator.umd.js` is wall-to-wall this shape (`const
///   isEmail_1 = __importDefault(require("validator/lib/isEmail"));`, ~70
///   occurrences in that one file).
/// - `require("specifier").propertyName` — a property read directly off the
///   required module, no intermediate binding. `commander`'s `const
///   EventEmitter = require('events').EventEmitter;` is this shape.
///
/// Recursion covers combinations of these that occur in practice
/// (`__importDefault(require("x")).default`, not observed in the npm corpus
/// swept so far but the same rule for free). Anything else — a computed
/// specifier, a renamed `require`, an arbitrary function call whose argument
/// is not itself one of these three shapes — is `false`; a specifier this
/// crate cannot read is not one it should guess at.
fn is_commonjs_import_expr(expr: &Expression) -> bool {
    match expr {
        Expression::CallExpression(call) => is_commonjs_import_call(call),
        Expression::StaticMemberExpression(mem) => is_commonjs_import_expr(&mem.object),
        _ => false,
    }
}

/// `is_commonjs_import_expr`'s counterpart over `Argument` — needed because
/// `__importDefault(...)`/`__importStar(...)`'s own argument is an `Argument`
/// node, a different (if variant-compatible) type from `Expression`.
fn is_commonjs_import_argument(arg: &Argument) -> bool {
    match arg {
        Argument::CallExpression(call) => is_commonjs_import_call(call),
        Argument::StaticMemberExpression(mem) => is_commonjs_import_expr(&mem.object),
        _ => false,
    }
}

fn is_commonjs_import_call(call: &CallExpression) -> bool {
    let Expression::Identifier(callee) = &call.callee else {
        return false;
    };
    match callee.name.as_str() {
        "require" => matches!(call.arguments.first(), Some(Argument::StringLiteral(_))),
        "__importDefault" | "__importStar" => call
            .arguments
            .first()
            .is_some_and(is_commonjs_import_argument),
        _ => false,
    }
}

/// `expr` reduced to a plain identifier name, or `None` for anything else
/// (a call expression, a literal, an object/array literal, …).
///
/// A function expression on the right of `exports.useState = function …`
/// and an identifier bound with `require("pkg")` are handled by
/// [`push_commonjs_value_decls`], not here.
fn as_plain_identifier(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        _ => None,
    }
}

/// A specifier that names another package (`"router"`, `"@scope/pkg"`),
/// not a relative file.
fn is_package_specifier(specifier: &str) -> bool {
    !specifier.is_empty() && !specifier.starts_with('.') && !specifier.starts_with('/')
}

/// `require("specifier")`, including a property read off that call
/// (`require("events").EventEmitter`).
fn require_specifier(expr: &Expression<'_>) -> Option<String> {
    match expr {
        Expression::CallExpression(call) => {
            let Expression::Identifier(callee) = &call.callee else {
                return None;
            };
            if callee.name != "require" {
                return None;
            }
            match call.arguments.first() {
                Some(Argument::StringLiteral(lit)) => Some(lit.value.to_string()),
                _ => None,
            }
        }
        Expression::StaticMemberExpression(mem) => require_specifier(&mem.object),
        _ => None,
    }
}

fn require_bindings<'a>(
    body: &'a [Statement<'a>],
    source: &'a str,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for stmt in body {
        let Statement::VariableDeclaration(var) = stmt else {
            continue;
        };
        for declarator in &var.declarations {
            let Some(name) = binding_pattern_name(&declarator.id, source) else {
                continue;
            };
            let Some(init) = declarator.init.as_ref() else {
                continue;
            };
            let Some(specifier) = require_specifier(init) else {
                continue;
            };
            out.insert(name, specifier);
        }
    }
    out
}

/// `Foo.prototype.bar = function (s) { ... }` is a method, not an export
/// assignment. The constructor stays the function it already is.
fn prototype_method<'a>(
    assign: &'a oxc_ast::ast::AssignmentExpression<'a>,
    source: &'a str,
) -> Option<(String, FunctionBody, Span)> {
    let AssignmentTarget::StaticMemberExpression(method) = &assign.left else {
        return None;
    };
    let Expression::StaticMemberExpression(proto) = &method.object else {
        return None;
    };
    if proto.property.name != "prototype" {
        return None;
    }
    let Expression::Identifier(_) = &proto.object else {
        return None;
    };
    let name = method.property.name.to_string();
    if name.is_empty() {
        return None;
    }
    match &assign.right {
        Expression::FunctionExpression(function) => {
            Some((name, lower_function(function, source), function.span()))
        }
        Expression::ArrowFunctionExpression(arrow) => {
            Some((name, lower_arrow(arrow, source), arrow.span()))
        }
        _ => None,
    }
}

/// `Foo.prototype = { bar: function (s) {}, baz: (n) => n }` declares each
/// function property. The constructor stays the function it already is.
fn prototype_object<'a>(
    assign: &'a oxc_ast::ast::AssignmentExpression<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    declarations: &mut Vec<DeclFact>,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> bool {
    let AssignmentTarget::StaticMemberExpression(proto) = &assign.left else {
        return false;
    };
    if proto.property.name != "prototype" {
        return false;
    }
    let Expression::Identifier(_) = &proto.object else {
        return false;
    };
    let Expression::ObjectExpression(object) = &assign.right else {
        return false;
    };
    for prop in &object.properties {
        let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(prop) = prop else {
            continue;
        };
        if prop.computed {
            continue;
        }
        let name = property_key_name(&prop.key, source);
        if name.is_empty() {
            continue;
        }
        let (body, span) = match &prop.value {
            Expression::FunctionExpression(function) => {
                (lower_function(function, source), function.span())
            }
            Expression::ArrowFunctionExpression(arrow) => (lower_arrow(arrow, source), arrow.span()),
            _ => continue,
        };
        push_commonjs_function(
            &name,
            body,
            span,
            semantic,
            path,
            declarations,
            name_counts,
        );
    }
    true
}

fn is_whole_module_exports(mem: &oxc_ast::ast::StaticMemberExpression<'_>) -> bool {
    matches!(&mem.object, Expression::Identifier(id) if id.name == "module")
        && mem.property.name == "exports"
}

/// `module.exports = { left: function () {}, right: 1 }` declares `left` and
/// `right`. A spread or a computed key has no stable name and is skipped.
/// `require("pkg")` stays a package re-export. An identifier is not turned
/// into a new const: the local declaration, when there is one, is the symbol.
fn push_commonjs_object_properties<'a>(
    object: &'a oxc_ast::ast::ObjectExpression<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    declarations: &mut Vec<DeclFact>,
    name_counts: &mut std::collections::HashMap<String, u32>,
    cjs_exports: &mut CommonJsExports,
    bindings: &std::collections::HashMap<String, String>,
) {
    use nudox_ir::entry::Visibility;
    use oxc_ast::ast::ObjectPropertyKind;

    for prop in &object.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = prop else {
            continue;
        };
        if prop.computed {
            continue;
        }
        let export_name = property_key_name(&prop.key, source);
        if export_name.is_empty() {
            continue;
        }
        if let Expression::FunctionExpression(function) = &prop.value {
            let span = function.span();
            if declarations
                .iter()
                .any(|decl| decl.name == export_name && decl.span_start == span.start)
            {
                continue;
            }
            let decl_index = bump_count(&export_name, name_counts);
            declarations.push(DeclFact {
                name: export_name.clone(),
                visibility: Visibility::Public,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                body: DeclBody::Function(lower_function(function, source)),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: false,
                decl_index,
            });
            cjs_exports
                .named
                .push((export_name.clone(), export_name));
            continue;
        }
        if let Expression::ArrowFunctionExpression(arrow) = &prop.value {
            push_commonjs_function(
                &export_name,
                lower_arrow(arrow, source),
                arrow.span(),
                semantic,
                path,
                declarations,
                name_counts,
            );
            cjs_exports
                .named
                .push((export_name.clone(), export_name));
            continue;
        }
        if let Expression::ClassExpression(class) = &prop.value {
            push_commonjs_class(
                class,
                &export_name,
                source,
                semantic,
                path,
                declarations,
                name_counts,
            );
            cjs_exports
                .named
                .push((export_name.clone(), export_name));
            continue;
        }
        if let Some(specifier) = require_specifier(&prop.value) {
            if !is_package_specifier(&specifier) {
                continue;
            }
            let span = prop.span();
            let decl_index = bump_count(&export_name, name_counts);
            declarations.push(DeclFact {
                name: export_name.clone(),
                visibility: Visibility::Public,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                body: DeclBody::Reexport {
                    module_request: specifier,
                    import_name: "*".to_string(),
                },
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: false,
                decl_index,
            });
            cjs_exports
                .named
                .push((export_name.clone(), export_name));
            continue;
        }
        if let Some(local) = as_plain_identifier(&prop.value) {
            if let Some(specifier) = bindings.get(&local).filter(|s| is_package_specifier(s)) {
                let span = prop.span();
                let decl_index = bump_count(&export_name, name_counts);
                declarations.push(DeclFact {
                    name: export_name.clone(),
                    visibility: Visibility::Public,
                    doc: jsdoc::jsdoc_for_span(semantic, span),
                    body: DeclBody::Reexport {
                        module_request: specifier.clone(),
                        import_name: "*".to_string(),
                    },
                    module: path.to_path_buf(),
                    span_start: span.start,
                    span_end: span.end,
                    is_default: false,
                    decl_index,
                });
                cjs_exports.named.push((export_name, local));
            } else if declarations.iter().any(|decl| decl.name == local) {
                cjs_exports.named.push((export_name, local));
            }
            continue;
        }
        let span = prop.span();
        let decl_index = bump_count(&export_name, name_counts);
        let value = prop.value.span().source_text(source).to_string();
        declarations.push(DeclFact {
            name: export_name.clone(),
            visibility: Visibility::Public,
            doc: jsdoc::jsdoc_for_span(semantic, span),
            body: DeclBody::Const(ConstBody {
                ty: None,
                value: Some(value),
                satisfies: None,
            }),
            module: path.to_path_buf(),
            span_start: span.start,
            span_end: span.end,
            is_default: false,
            decl_index,
        });
        cjs_exports
            .named
            .push((export_name.clone(), export_name));
    }
}

fn push_commonjs_function<'a>(
    name: &str,
    body: FunctionBody,
    span: Span,
    semantic: &'a Semantic<'a>,
    path: &Path,
    declarations: &mut Vec<DeclFact>,
    name_counts: &mut std::collections::HashMap<String, u32>,
) {
    use nudox_ir::entry::Visibility;

    if declarations
        .iter()
        .any(|decl| decl.name == name && decl.span_start == span.start)
    {
        return;
    }
    let decl_index = bump_count(name, name_counts);
    declarations.push(DeclFact {
        name: name.to_string(),
        visibility: Visibility::Public,
        doc: jsdoc::jsdoc_for_span(semantic, span),
        body: DeclBody::Function(body),
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default: false,
        decl_index,
    });
}

fn push_commonjs_class<'a>(
    class: &'a Class<'a>,
    name: &str,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    declarations: &mut Vec<DeclFact>,
    name_counts: &mut std::collections::HashMap<String, u32>,
) {
    use nudox_ir::entry::Visibility;

    let span = class.span();
    if declarations
        .iter()
        .any(|decl| decl.name == name && decl.span_start == span.start)
    {
        return;
    }
    let decl_index = bump_count(name, name_counts);
    declarations.push(DeclFact {
        name: name.to_string(),
        visibility: Visibility::Public,
        doc: jsdoc::jsdoc_for_span(semantic, span),
        body: DeclBody::Class(lower_class(class, source, semantic)),
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default: false,
        decl_index,
    });
}

fn export_property_name(
    mem: &oxc_ast::ast::StaticMemberExpression<'_>,
    cjs: &CommonJsExports,
) -> Option<String> {
    let object_is_export_binding = match &mem.object {
        Expression::Identifier(id) if id.name == "exports" => true,
        Expression::Identifier(id) => cjs.whole_module.as_deref() == Some(id.name.as_str()),
        other => is_module_dot_exports(other),
    };
    if object_is_export_binding {
        Some(mem.property.name.to_string())
    } else {
        None
    }
}

/// Top-level CommonJS assignments the statement walker does not turn into
/// declarations.
///
/// - `exports.useState = function (initialState) { ... }` becomes an owned
///   function named `useState`.
/// - `var Router = require("router"); exports.Router = Router` becomes a
///   re-export of package `router`, not a const and not a function.
fn push_commonjs_value_decls<'a>(
    body: &'a [Statement<'a>],
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    declarations: &mut Vec<DeclFact>,
    name_counts: &mut std::collections::HashMap<String, u32>,
    cjs_exports: &mut CommonJsExports,
) {
    use nudox_ir::entry::Visibility;

    let bindings = require_bindings(body, source);
    for stmt in body {
        let Statement::ExpressionStatement(expr_stmt) = stmt else {
            continue;
        };
        let Expression::AssignmentExpression(assign) = &expr_stmt.expression else {
            continue;
        };
        if assign.operator != AssignmentOperator::Assign {
            continue;
        }
        if let Some((name, body, span)) = prototype_method(assign, source) {
            push_commonjs_function(
                &name,
                body,
                span,
                semantic,
                path,
                declarations,
                name_counts,
            );
            continue;
        }
        if prototype_object(
            assign,
            source,
            semantic,
            path,
            declarations,
            name_counts,
        ) {
            continue;
        }
        let AssignmentTarget::StaticMemberExpression(mem) = &assign.left else {
            continue;
        };
        if is_whole_module_exports(mem) {
            if let Expression::ObjectExpression(object) = &assign.right {
                push_commonjs_object_properties(
                    object,
                    source,
                    semantic,
                    path,
                    declarations,
                    name_counts,
                    cjs_exports,
                    &bindings,
                );
            } else if let Expression::ClassExpression(class) = &assign.right {
                let name = class
                    .id
                    .as_ref()
                    .map(|id| id.name.to_string())
                    .unwrap_or_else(|| "default".to_string());
                push_commonjs_class(
                    class,
                    &name,
                    source,
                    semantic,
                    path,
                    declarations,
                    name_counts,
                );
                cjs_exports.whole_module.get_or_insert(name);
            } else if let Expression::FunctionExpression(function) = &assign.right {
                let name = function
                    .id
                    .as_ref()
                    .map(|id| id.name.to_string())
                    .unwrap_or_else(|| "default".to_string());
                push_commonjs_function(
                    &name,
                    lower_function(function, source),
                    function.span(),
                    semantic,
                    path,
                    declarations,
                    name_counts,
                );
                cjs_exports.whole_module.get_or_insert(name);
            } else if let Expression::ArrowFunctionExpression(arrow) = &assign.right {
                push_commonjs_function(
                    "default",
                    lower_arrow(arrow, source),
                    arrow.span(),
                    semantic,
                    path,
                    declarations,
                    name_counts,
                );
                cjs_exports
                    .whole_module
                    .get_or_insert_with(|| "default".to_string());
            }
            continue;
        }
        let Some(export_name) = export_property_name(mem, cjs_exports) else {
            continue;
        };

        if let Expression::ArrowFunctionExpression(arrow) = &assign.right {
            push_commonjs_function(
                &export_name,
                lower_arrow(arrow, source),
                arrow.span(),
                semantic,
                path,
                declarations,
                name_counts,
            );
            cjs_exports.named.push((export_name.clone(), export_name));
            continue;
        }

        if let Expression::ClassExpression(class) = &assign.right {
            push_commonjs_class(
                class,
                &export_name,
                source,
                semantic,
                path,
                declarations,
                name_counts,
            );
            cjs_exports.named.push((export_name.clone(), export_name));
            continue;
        }

        if let Expression::FunctionExpression(function) = &assign.right {
            let span = function.span();
            if declarations
                .iter()
                .any(|decl| decl.name == export_name && decl.span_start == span.start)
            {
                continue;
            }
            let decl_index = bump_count(&export_name, name_counts);
            declarations.push(DeclFact {
                name: export_name.clone(),
                visibility: Visibility::Public,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                body: DeclBody::Function(lower_function(function, source)),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: false,
                decl_index,
            });
            cjs_exports.named.push((export_name.clone(), export_name));
            continue;
        }

        let Some(local) = as_plain_identifier(&assign.right) else {
            continue;
        };
        let Some(specifier) = bindings.get(&local) else {
            continue;
        };
        if !is_package_specifier(specifier) {
            continue;
        }
        // The same package assigned twice is one export. A different
        // package under the same name is a second declaration.
        let same_package = declarations.iter().any(|decl| {
            decl.name == export_name
                && matches!(
                    &decl.body,
                    DeclBody::Reexport {
                        module_request,
                        import_name,
                    } if module_request == specifier && import_name == "*"
                )
        });
        if same_package {
            continue;
        }
        let span = assign.span();
        let decl_index = bump_count(&export_name, name_counts);
        declarations.push(DeclFact {
            name: export_name,
            visibility: Visibility::Public,
            doc: jsdoc::jsdoc_for_span(semantic, span),
            body: DeclBody::Reexport {
                module_request: specifier.clone(),
                import_name: "*".to_string(),
            },
            module: path.to_path_buf(),
            span_start: span.start,
            span_end: span.end,
            is_default: false,
            decl_index,
        });
    }
}

/// Scan `body` for the CommonJS export assignment forms `CommonJsExports`
/// documents.
fn scan_commonjs_exports(body: &[Statement<'_>]) -> CommonJsExports {
    let mut out = CommonJsExports::default();

    // Pass 1: find `module.exports = <ident>;` first, so pass 2 can
    // recognise `<ident>.X = …` regardless of where in the file that
    // assignment appears relative to the ones establishing `<ident>` as the
    // export binding.
    for stmt in body {
        let Statement::ExpressionStatement(expr_stmt) = stmt else {
            continue;
        };
        let Expression::AssignmentExpression(assign) = &expr_stmt.expression else {
            continue;
        };
        if assign.operator != AssignmentOperator::Assign {
            continue;
        }
        let AssignmentTarget::StaticMemberExpression(mem) = &assign.left else {
            continue;
        };
        let is_whole_module_target = matches!(&mem.object, Expression::Identifier(id) if id.name == "module")
            && mem.property.name == "exports";
        if is_whole_module_target {
            if let Some(name) = as_plain_identifier(&assign.right) {
                out.whole_module = Some(name);
            }
            break; // a module has at most one `module.exports = ident` target
        }
    }

    // Pass 2: `exports.X = ident;`, `module.exports.X = ident;`, and
    // `<export-binding>.X = ident;`.
    for stmt in body {
        let Statement::ExpressionStatement(expr_stmt) = stmt else {
            continue;
        };
        let Expression::AssignmentExpression(assign) = &expr_stmt.expression else {
            continue;
        };
        if assign.operator != AssignmentOperator::Assign {
            continue;
        }
        let AssignmentTarget::StaticMemberExpression(mem) = &assign.left else {
            continue;
        };
        let Some(rhs_name) = as_plain_identifier(&assign.right) else {
            continue;
        };

        let object_is_export_binding = match &mem.object {
            Expression::Identifier(id) if id.name == "exports" => true,
            Expression::Identifier(id) => out.whole_module.as_deref() == Some(id.name.as_str()),
            other => is_module_dot_exports(other),
        };

        if object_is_export_binding {
            out.named.push((mem.property.name.to_string(), rhs_name));
        }
    }

    out
}

// ── Statement dispatch
// ─────────────────────────────────────────────────────────

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
                    exported_names,
                    name_counts,
                );
            }
            vec![]
        }

        Statement::ExportDefaultDeclaration(exp) => {
            extract_default_export(&exp.declaration, source, semantic, path, name_counts)
        }

        Statement::FunctionDeclaration(f) => {
            let decl = stmt
                .as_declaration()
                .expect("FunctionDeclaration is a Declaration");
            f.id.as_ref().map_or_else(Vec::new, |id| {
                let name = id.name.to_string();
                let is_exported =
                    exported_names.contains(&name) || default_local_name.as_deref() == Some(&name);
                extract_declaration(
                    decl,
                    source,
                    semantic,
                    path,
                    is_exported,
                    false,
                    exported_names,
                    name_counts,
                )
            })
        }

        Statement::ClassDeclaration(c) => {
            let decl = stmt
                .as_declaration()
                .expect("ClassDeclaration is a Declaration");
            c.id.as_ref().map_or_else(Vec::new, |id| {
                let name = id.name.to_string();
                let is_exported = exported_names.contains(&name);
                extract_declaration(
                    decl,
                    source,
                    semantic,
                    path,
                    is_exported,
                    false,
                    exported_names,
                    name_counts,
                )
            })
        }

        Statement::VariableDeclaration(_v) => {
            let decl = stmt
                .as_declaration()
                .expect("VariableDeclaration is a Declaration");
            // `is_exported` here is deliberately `false`, not a guess: a
            // bare `const merge = ...;` (no `export` keyword) is only public
            // when some other statement makes it so — an ESM `export {
            // merge };` elsewhere in the file (already in `exported_names`),
            // or a CommonJS `module.exports = merge;` /
            // `exports.merge = merge;` (folded into `exported_names` by
            // `extract_module`'s CommonJS scan before this is called). Both
            // are per-*name* facts, and `lower_variable` — not this
            // statement-level dispatch — is what checks `exported_names` per
            // declarator, because one `var`/`let`/`const` statement can
            // declare several names with different export status
            // (`export const a = 1, b = 2;` cannot, but a bare
            // `const a = 1, b = 2;` where only `b` is separately
            // `export`-ed can). `lodash`'s `merge.js` is exactly this
            // shape: `var merge = createAssigner(...)` is a
            // `VariableDeclaration`, never `export`-prefixed, made public
            // only by the later `module.exports = merge;`.
            extract_declaration(
                decl,
                source,
                semantic,
                path,
                false,
                false,
                exported_names,
                name_counts,
            )
        }

        Statement::TSTypeAliasDeclaration(a) => {
            let decl = stmt.as_declaration().expect("TSTypeAlias is a Declaration");
            let is_exported = exported_names.contains(a.id.name.as_str());
            extract_declaration(
                decl,
                source,
                semantic,
                path,
                is_exported,
                false,
                exported_names,
                name_counts,
            )
        }

        Statement::TSInterfaceDeclaration(i) => {
            let decl = stmt.as_declaration().expect("TSInterface is a Declaration");
            let is_exported = exported_names.contains(i.id.name.as_str());
            extract_declaration(
                decl,
                source,
                semantic,
                path,
                is_exported,
                false,
                exported_names,
                name_counts,
            )
        }

        Statement::TSEnumDeclaration(e) => {
            let decl = stmt.as_declaration().expect("TSEnum is a Declaration");
            let is_exported = exported_names.contains(e.id.name.as_str());
            extract_declaration(
                decl,
                source,
                semantic,
                path,
                is_exported,
                false,
                exported_names,
                name_counts,
            )
        }

        Statement::TSImportEqualsDeclaration(import) => {
            let name = import.id.name.to_string();
            let is_exported = exported_names.contains(&name);
            let visibility = if is_exported {
                nudox_ir::entry::Visibility::Public
            } else {
                nudox_ir::entry::Visibility::Private
            };
            import_equals_fact(import, semantic, path, visibility, name_counts)
        }

        Statement::TSModuleDeclaration(m) => {
            let decl = stmt.as_declaration().expect("TSModule is a Declaration");
            let sym_name = match &m.id {
                TSModuleDeclarationName::Identifier(id) => id.name.to_string(),
                TSModuleDeclarationName::StringLiteral(s) => s.value.to_string(),
            };
            let is_string_literal = matches!(&m.id, TSModuleDeclarationName::StringLiteral(_));
            let is_exported = exported_names.contains(&sym_name) || m.declare || is_string_literal;
            extract_declaration(
                decl,
                source,
                semantic,
                path,
                is_exported,
                false,
                exported_names,
                name_counts,
            )
        }

        Statement::TSExportAssignment(assign) => export_assignment(assign, source, semantic, path, name_counts),

        Statement::TSGlobalDeclaration(global) => {
            lower_global(global, source, semantic, path, name_counts)
        }

        Statement::TSNamespaceExportDeclaration(ns) => {
            let name = ns.id.name.to_string();
            let span = ns.span();
            let decl_index = bump_count(&name, name_counts);
            vec![DeclFact {
                name,
                visibility: nudox_ir::entry::Visibility::Public,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                body: DeclBody::Namespace(NamespaceBody {
                    is_ambient: true,
                    children: Vec::new(),
                }),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: false,
                decl_index,
            }]
        }

        _ => vec![],
    }
}

fn export_assignment<'a>(
    assign: &'a oxc_ast::ast::TSExportAssignment<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    let span = assign.span();
    let (name, body) = match &assign.expression {
        Expression::FunctionExpression(f) => {
            let name = f
                .id
                .as_ref()
                .map(|id| id.name.to_string())
                .unwrap_or_else(|| "default".to_string());
            (name, DeclBody::Function(lower_function(f, source)))
        }
        Expression::Identifier(_) => return vec![],
        other => (
            "default".to_string(),
            DeclBody::Const(ConstBody {
                ty: None,
                value: Some(other.span().source_text(source).to_string()),
                satisfies: None,
            }),
        ),
    };
    let decl_index = bump_count(&name, name_counts);
    vec![DeclFact {
        name,
        visibility: nudox_ir::entry::Visibility::Public,
        doc: jsdoc::jsdoc_for_span(semantic, span),
        body,
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default: false,
        decl_index,
    }]
}

// ── Declaration dispatch
// ───────────────────────────────────────────────────────

fn extract_declaration<'a>(
    decl: &'a Declaration<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    is_exported: bool,
    is_default: bool,
    exported_names: &std::collections::HashSet<String>,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;

    let visibility = if is_exported {
        Visibility::Public
    } else {
        Visibility::Private
    };

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
            let body = lower_class(c, source, semantic);
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

        Declaration::VariableDeclaration(v) => lower_variable(
            v,
            source,
            semantic,
            path,
            is_exported,
            exported_names,
            name_counts,
        ),

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
            let body = lower_enum(e, source);
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

        Declaration::TSModuleDeclaration(m) => lower_namespace(
            m,
            source,
            semantic,
            path,
            is_exported,
            is_default,
            name_counts,
        ),

        Declaration::TSImportEqualsDeclaration(import) => {
            import_equals_fact(import, semantic, path, visibility, name_counts)
        }

        Declaration::TSGlobalDeclaration(global) => {
            lower_global(global, source, semantic, path, name_counts)
        }
    }
}

/// `import Name = require("pkg")` is a package reference.
/// `import Name = Bar` and `import Name = NS.Bar` are aliases of that spelling.
///
/// The alias target is a nominal name. Lowering resolves it when this package
/// declares it, and otherwise records an unresolved external. It does not
/// `refer` a name that will never be declared.
fn import_equals_fact<'a>(
    import: &'a oxc_ast::ast::TSImportEqualsDeclaration<'a>,
    semantic: &'a Semantic<'a>,
    path: &Path,
    visibility: nudox_ir::entry::Visibility,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    let name = import.id.name.to_string();
    let span = import.span();
    let decl_index = bump_count(&name, name_counts);
    let body = match &import.module_reference {
        TSModuleReference::ExternalModuleReference(external) => DeclBody::Reexport {
            module_request: external.expression.value.to_string(),
            import_name: "*".to_string(),
        },
        TSModuleReference::IdentifierReference(id) => DeclBody::TypeAlias(TypeAliasBody {
            generics: Vec::new(),
            target: super::TypeOwned::Nominal(id.name.to_string()),
        }),
        TSModuleReference::QualifiedName(qualified) => DeclBody::TypeAlias(TypeAliasBody {
            generics: Vec::new(),
            target: super::TypeOwned::Nominal(qualified_module_name(qualified)),
        }),
    };
    vec![DeclFact {
        name,
        visibility,
        doc: jsdoc::jsdoc_for_span(semantic, span),
        body,
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default: false,
        decl_index,
    }]
}

fn qualified_module_name(name: &oxc_ast::ast::TSQualifiedName<'_>) -> String {
    let left = match &name.left {
        oxc_ast::ast::TSTypeName::IdentifierReference(id) => id.name.to_string(),
        oxc_ast::ast::TSTypeName::QualifiedName(inner) => qualified_module_name(inner),
        oxc_ast::ast::TSTypeName::ThisExpression(_) => "this".to_string(),
    };
    format!("{left}.{}", name.right.name)
}

// ── Default export
// ─────────────────────────────────────────────────────────────

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
            let name =
                f.id.as_ref()
                    .map_or_else(|| "default".to_string(), |id| id.name.to_string());
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
            let name =
                c.id.as_ref()
                    .map_or_else(|| "default".to_string(), |id| id.name.to_string());
            let span = c.span();
            let doc = jsdoc::jsdoc_for_span(semantic, span);
            let discriminant = bump_count(&name, name_counts);
            let body = lower_class(c, source, semantic);
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
        // `export default foo` already aliases the local declaration.
        // A literal or other expression has no name, so it is a const
        // called `default` rather than a missing export.
        other => {
            if matches!(other, ExportDefaultDeclarationKind::Identifier(_)) {
                return vec![];
            }
            let span = other.span();
            let discriminant = bump_count("default", name_counts);
            vec![DeclFact {
                name: "default".to_string(),
                visibility: Visibility::Public,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                body: DeclBody::Const(ConstBody {
                    ty: None,
                    value: Some(span.source_text(source).to_string()),
                    satisfies: None,
                }),
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: true,
                decl_index: discriminant,
            }]
        }
    }
}

// ── Kind-specific lowering
// ─────────────────────────────────────────────────────

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

    let type_params = f.type_parameters.as_ref().map(|tp| {
        tp.params
            .iter()
            .map(|p| p.name.to_string())
            .collect::<std::collections::HashSet<_>>()
    });
    let this_ty = f.this_param.as_ref().and_then(|param| {
        param.type_annotation.as_ref().map(|ann| match &type_params {
            Some(set) => lower_ts_type_with_params(&ann.type_annotation, source, set),
            None => lower_ts_type(&ann.type_annotation, source),
        })
    });
    let receiver = if first_param_is_this || this_ty.is_some() {
        ReceiverKind::SharedRef
    } else {
        ReceiverKind::None
    };
    let params =
        lower_formal_parameters(&f.params, source, first_param_is_this, type_params.as_ref());

    let return_type = f.return_type.as_ref().map(|ann| match &type_params {
        Some(set) => lower_ts_type_with_params(&ann.type_annotation, source, set),
        None => lower_ts_type(&ann.type_annotation, source),
    });

    let generics = f
        .type_parameters
        .as_ref()
        .map(|tp| lower_type_params(tp, source))
        .unwrap_or_default();

    let is_async = f.r#async;
    let is_generator = f.generator;
    let span = f.span();

    FunctionBody {
        generics,
        params,
        return_type,
        is_async,
        is_generator,
        has_body,
        receiver,
        this_ty,
        abstract_construct: false,
        body_text: f
            .body
            .as_ref()
            .map(|body| body.span().source_text(source).to_string()),
        leading_doc: None,
        span_start: span.start,
        span_end: span.end,
    }
}

fn lower_arrow<'a>(
    arrow: &oxc_ast::ast::ArrowFunctionExpression<'a>,
    source: &'a str,
) -> FunctionBody {
    let span = arrow.span();
    let type_params = arrow.type_parameters.as_ref().map(|tp| {
        tp.params
            .iter()
            .map(|p| p.name.to_string())
            .collect::<std::collections::HashSet<_>>()
    });
    FunctionBody {
        generics: arrow
            .type_parameters
            .as_ref()
            .map(|tp| lower_type_params(tp, source))
            .unwrap_or_default(),
        params: lower_formal_parameters(&arrow.params, source, false, type_params.as_ref()),
        return_type: arrow.return_type.as_ref().map(|ann| match &type_params {
            Some(set) => lower_ts_type_with_params(&ann.type_annotation, source, set),
            None => lower_ts_type(&ann.type_annotation, source),
        }),
        is_async: arrow.r#async,
        is_generator: false,
        has_body: true,
        receiver: ReceiverKind::None,
        this_ty: None,
        abstract_construct: false,
        body_text: Some(arrow.body.span().source_text(source).to_string()),
        leading_doc: None,
        span_start: span.start,
        span_end: span.end,
    }
}

fn lower_formal_parameters<'a>(
    params: &oxc_ast::ast::FormalParameters<'a>,
    source: &'a str,
    skip_first: bool,
    type_params: Option<&std::collections::HashSet<String>>,
) -> Vec<ParamFact> {
    let items = if skip_first && !params.items.is_empty() {
        &params.items[1..]
    } else {
        &params.items
    };

    let mut out: Vec<ParamFact> = Vec::with_capacity(items.len() + 1);
    for param in items {
        let mut names = binding_names(&param.pattern);
        if names.is_empty() {
            names.push("_".to_string());
        }
        let ty = param.type_annotation.as_ref().map(|ann| match type_params {
            Some(set) => lower_ts_type_with_params(&ann.type_annotation, source, set),
            None => lower_ts_type(&ann.type_annotation, source),
        });
        let is_readonly = param.readonly;
        let span = param.span();
        let initializer = param
            .initializer
            .as_ref()
            .map(|init| init.span().source_text(source).to_string());
        let decorators: Vec<AttrTok> = param
            .decorators
            .iter()
            .map(|decorator| AttrTok {
                token: decorator.span.source_text(source).to_string(),
            })
            .collect();
        for name in names {
            out.push(ParamFact {
                name,
                ty: ty.clone(),
                is_optional: param.optional,
                is_rest: false,
                is_readonly,
                initializer: initializer.clone(),
                decorators: decorators.clone(),
                span_start: span.start,
                span_end: span.end,
            });
        }
    }
    if let Some(rest) = &params.rest {
        let ty = rest
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type(&ann.type_annotation, source));
        let mut names = binding_names(&rest.rest.argument);
        if names.is_empty() {
            names.push("...rest".to_string());
        }
        let span = rest.span();
        let decorators: Vec<AttrTok> = rest
            .decorators
            .iter()
            .map(|decorator| AttrTok {
                token: decorator.span.source_text(source).to_string(),
            })
            .collect();
        for name in names {
            out.push(ParamFact {
                name,
                ty: ty.clone(),
                is_optional: false,
                is_rest: true,
                is_readonly: false,
                initializer: None,
                decorators: decorators.clone(),
                span_start: span.start,
                span_end: span.end,
            });
        }
    }
    out
}

fn lower_class<'a>(cls: &Class<'a>, source: &'a str, semantic: &'a Semantic<'a>) -> ClassBody {
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
        .map(|d| AttrTok {
            token: d.span.source_text(source).to_string(),
        })
        .collect();

    // First pass: collect non-constructor members from the AST.
    let mut members: Vec<MemberFact> = cls
        .body
        .body
        .iter()
        .filter_map(|elem| lower_class_element(elem, source, semantic))
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
        for param in &ctor.value.params.items {
            // Only parameter properties: must have accessibility OR readonly.
            if param.accessibility.is_none() && !param.readonly {
                continue;
            }
            let name = binding_pattern_name(&param.pattern, source).unwrap_or_else(|| "_".to_string());
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
            let span = param.span();
            members.push(MemberFact {
                name,
                kind: MemberKind::Property { ty },
                modifiers,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                decorators: Vec::new(),
                class_flags: ClassFlags {
                    declare: false,
                    override_: param.r#override,
                    definite: false,
                },
                initializer: None,
                signature_kind: SignatureKind::Method,
                span_start: span.start,
                span_end: span.end,
            });
        }

        // ── this.x = … field synthesis (item 2) ─────────────────────────
        // Walk constructor body for `this.<name> = …` assignments.
        if let Some(ref ctor_body) = ctor.value.body {
            let mut synth_seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for stmt in &ctor_body.statements {
                if let Statement::ExpressionStatement(expr_stmt) = stmt
                    && let Expression::AssignmentExpression(assign) = &expr_stmt.expression
                    && assign.operator == AssignmentOperator::Assign
                    && let AssignmentTarget::StaticMemberExpression(mem) = &assign.left
                    && matches!(&mem.object, Expression::ThisExpression(_))
                {
                    let prop_name = mem.property.name.to_string();
                    if !declared_field_names.contains(&prop_name)
                        && synth_seen.insert(prop_name.clone())
                    {
                        declared_field_names.insert(prop_name.clone());
                        let span = assign.span();
                        members.push(MemberFact {
                            name: prop_name,
                            kind: MemberKind::Property { ty: None },
                            modifiers: MemberModifiers::default(),
                            doc: jsdoc::jsdoc_for_span(semantic, span),
                            decorators: Vec::new(),
                            class_flags: ClassFlags::default(),
                            initializer: None,
                            signature_kind: SignatureKind::Method,
                            span_start: span.start,
                            span_end: span.end,
                        });
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
    for m in &mut members {
        if let MemberKind::StaticBlock { ref mut name } = m.kind {
            let real_name = if static_count == 0 {
                "__static".to_string()
            } else {
                format!("__static_{static_count}")
            };
            static_count += 1;
            name.clone_from(&real_name);
            m.name = real_name;
        }
    }

    let index_signatures = cls
        .body
        .body
        .iter()
        .filter_map(|elem| {
            let ClassElement::TSIndexSignature(idx) = elem else {
                return None;
            };
            let param = idx.parameters.first()?;
            let idx_span = idx.span();
            Some(IndexSignatureFact {
                key_name: param.name.as_str().to_string(),
                key_ty: lower_ts_type(&param.type_annotation.type_annotation, source),
                value_ty: lower_ts_type(&idx.type_annotation.type_annotation, source),
                readonly: idx.readonly,
                is_static: idx.r#static,
                doc: signature_doc(&jsdoc::jsdoc_for_span(semantic, idx_span)),
                span_start: idx_span.start,
                span_end: idx_span.end,
            })
        })
        .collect();

    ClassBody {
        generics,
        extends,
        implements,
        members,
        index_signatures,
        is_abstract,
        decorators,
    }
}

fn lower_class_element<'a>(
    elem: &ClassElement<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
) -> Option<MemberFact> {
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
                .map(|d| AttrTok {
                    token: d.span.source_text(source).to_string(),
                })
                .collect();
            let sig = lower_function(&m.value, source);
            let span = m.span();
            Some(MemberFact {
                name,
                kind: match m.kind {
                    MethodDefinitionKind::Constructor => MemberKind::Constructor(sig),
                    _ => MemberKind::Method(vec![sig]),
                },
                modifiers,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                decorators,
                class_flags: ClassFlags {
                    declare: false,
                    override_: m.r#override,
                    definite: false,
                },
                initializer: None,
                signature_kind: match m.kind {
                    MethodDefinitionKind::Get => SignatureKind::Get,
                    MethodDefinitionKind::Set => SignatureKind::Set,
                    MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => {
                        SignatureKind::Method
                    }
                },
                span_start: span.start,
                span_end: span.end,
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
                .map(|d| AttrTok {
                    token: d.span.source_text(source).to_string(),
                })
                .collect();
            let span = p.span();
            Some(MemberFact {
                name,
                kind: MemberKind::Property { ty },
                modifiers,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                decorators,
                class_flags: ClassFlags {
                    declare: p.declare,
                    override_: p.r#override,
                    definite: p.definite,
                },
                initializer: p
                    .value
                    .as_ref()
                    .map(|value| value.span().source_text(source).to_string()),
                signature_kind: SignatureKind::Method,
                span_start: span.start,
                span_end: span.end,
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
                .map(|d| AttrTok {
                    token: d.span.source_text(source).to_string(),
                })
                .collect();
            // Synthetic marker so downstream consumers can tell accessor from plain
            // property.
            decorators.push(AttrTok {
                token: "accessor".to_string(),
            });
            let span = ap.span();
            Some(MemberFact {
                name,
                kind: MemberKind::Accessor { ty },
                modifiers,
                doc: jsdoc::jsdoc_for_span(semantic, span),
                decorators,
                class_flags: ClassFlags {
                    declare: false,
                    override_: ap.r#override,
                    definite: ap.definite,
                },
                initializer: ap
                    .value
                    .as_ref()
                    .map(|value| value.span().source_text(source).to_string()),
                signature_kind: SignatureKind::Method,
                span_start: span.start,
                span_end: span.end,
            })
        }

        // ── Item 4: Static initializer block (`static { … }`) ────────────
        // No type-level surface, but must not be silently dropped.
        // Emit as `MemberKind::StaticBlock` with a synthetic name; emit.rs
        // will translate this to a synthetic Function child.
        ClassElement::StaticBlock(sb) => {
            // The caller (`lower_class`) assigns the unique __static[_N] name.
            // We emit a placeholder here; lower_class re-names them in order.
            let span = sb.span();
            Some(MemberFact {
                name: "__static_placeholder".to_string(),
                kind: MemberKind::StaticBlock {
                    name: "__static".to_string(),
                },
                modifiers: MemberModifiers {
                    accessibility: Accessibility::Private,
                    is_static: true,
                    is_readonly: false,
                    is_optional: false,
                    is_abstract: false,
                },
                doc: jsdoc::jsdoc_for_span(semantic, span),
                decorators: Vec::new(),
                class_flags: ClassFlags::default(),
                initializer: Some(span.source_text(source).to_string()),
                signature_kind: SignatureKind::Method,
                span_start: span.start,
                span_end: span.end,
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
    semantic: &'a Semantic<'a>,
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

    for sig in &iface.body.body {
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
                let params = lower_formal_parameters(&m.params, source, false, None);
                let return_type = m
                    .return_type
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let generics = m
                    .type_parameters
                    .as_ref()
                    .map(|tp| lower_type_params(tp, source))
                    .unwrap_or_default();
                let this_ty = m.this_param.as_ref().and_then(|param| {
                    param
                        .type_annotation
                        .as_ref()
                        .map(|ann| lower_ts_type(&ann.type_annotation, source))
                });
                let m_span = m.span();
                let sig = FunctionBody {
                    generics,
                    params,
                    return_type,
                    is_async: false,
                    is_generator: false,
                    has_body: false,
                    receiver: if this_ty.is_some() {
                        ReceiverKind::SharedRef
                    } else {
                        ReceiverKind::None
                    },
                    this_ty,
                    abstract_construct: false,
                    body_text: None,
                    leading_doc: None,
                    span_start: m_span.start,
                    span_end: m_span.end,
                };
                methods.push(MethodFact {
                    name,
                    sig,
                    modifiers,
                    doc: jsdoc::jsdoc_for_span(semantic, m_span),
                    is_overload: false,
                    signature_kind: match m.kind {
                        oxc_ast::ast::TSMethodSignatureKind::Get => {
                            crate::typescript::extract::SignatureKind::Get
                        }
                        oxc_ast::ast::TSMethodSignatureKind::Set => {
                            crate::typescript::extract::SignatureKind::Set
                        }
                        oxc_ast::ast::TSMethodSignatureKind::Method => {
                            crate::typescript::extract::SignatureKind::Method
                        }
                    },
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
                let p_span = p.span();
                properties.push(PropertyFact {
                    name,
                    ty,
                    modifiers,
                    doc: jsdoc::jsdoc_for_span(semantic, p_span),
                    span_start: p_span.start,
                    span_end: p_span.end,
                });
            }
            TSSignature::TSCallSignatureDeclaration(c) => {
                let params = lower_formal_parameters(&c.params, source, false, None);
                let return_type = c
                    .return_type
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let generics = c
                    .type_parameters
                    .as_ref()
                    .map(|tp| lower_type_params(tp, source))
                    .unwrap_or_default();
                let this_ty = c.this_param.as_ref().and_then(|param| {
                    param
                        .type_annotation
                        .as_ref()
                        .map(|ann| lower_ts_type(&ann.type_annotation, source))
                });
                let c_span = c.span();
                call_signatures.push(FunctionBody {
                    generics,
                    params,
                    return_type,
                    is_async: false,
                    is_generator: false,
                    has_body: false,
                    receiver: if this_ty.is_some() {
                        ReceiverKind::SharedRef
                    } else {
                        ReceiverKind::None
                    },
                    this_ty,
                    abstract_construct: false,
                    body_text: None,
                    leading_doc: signature_doc(&jsdoc::jsdoc_for_span(semantic, c_span)),
                    span_start: c_span.start,
                    span_end: c_span.end,
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
                    let idx_span = idx.span();
                    index_signatures.push(IndexSignatureFact {
                        key_name,
                        key_ty,
                        value_ty,
                        readonly: idx.readonly,
                        is_static: idx.r#static,
                        doc: signature_doc(&jsdoc::jsdoc_for_span(semantic, idx_span)),
                        span_start: idx_span.start,
                        span_end: idx_span.end,
                    });
                }
            }
            // ── Item 6: Construct signatures (`new (…): T`) ───────────────
            TSSignature::TSConstructSignatureDeclaration(cs) => {
                let params = lower_formal_parameters(&cs.params, source, false, None);
                let return_type = cs
                    .return_type
                    .as_ref()
                    .map(|ann| lower_ts_type(&ann.type_annotation, source));
                let generics = cs
                    .type_parameters
                    .as_ref()
                    .map(|tp| lower_type_params(tp, source))
                    .unwrap_or_default();
                let cs_span = cs.span();
                construct_signatures.push(FunctionBody {
                    generics,
                    params,
                    return_type,
                    is_async: false,
                    is_generator: false,
                    has_body: false,
                    receiver: ReceiverKind::None,
                    this_ty: None,
                    abstract_construct: false,
                    body_text: None,
                    leading_doc: signature_doc(&jsdoc::jsdoc_for_span(semantic, cs_span)),
                    span_start: cs_span.start,
                    span_end: cs_span.end,
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

fn lower_enum<'a>(e: &TSEnumDeclaration<'a>, source: &'a str) -> EnumBody {
    let is_const = e.r#const;
    let variants: Vec<VariantFact> = e
        .body
        .members
        .iter()
        .map(|m| {
            let name = match &m.id {
                TSEnumMemberName::Identifier(id) => id.name.to_string(),
                TSEnumMemberName::String(s) => s.value.to_string(),
                other => format!("{other:?}"),
            };
            let discriminant = m.initializer.as_ref().map(|init| match init {
                Expression::StringLiteral(s) => format!("\"{}\"", s.value),
                Expression::NumericLiteral(n) => n.value.to_string(),
                Expression::UnaryExpression(u) => {
                    format!("{:?}{}", u.operator, u.argument.span().source_text(source))
                }
                other => format!("{other:?}"),
            });
            let span = m.span();
            VariantFact {
                name,
                discriminant,
                span_start: span.start,
                span_end: span.end,
            }
        })
        .collect();
    EnumBody { is_const, variants }
}

/// The type in `expr satisfies T`, through parentheses. Not an `as` cast:
/// a cast replaces the expression's type, and that type is not stored here.
fn satisfies_type<'a>(expr: &Expression<'a>, source: &'a str) -> Option<TypeOwned> {
    let mut current = expr;
    loop {
        match current {
            Expression::ParenthesizedExpression(inner) => current = &inner.expression,
            Expression::TSSatisfiesExpression(satisfied) => {
                return Some(lower_ts_type(&satisfied.type_annotation, source));
            }
            _ => return None,
        }
    }
}

fn lower_variable<'a>(
    v: &VariableDeclaration<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    force_exported: bool,
    exported_names: &std::collections::HashSet<String>,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;

    let is_const = matches!(v.kind, VariableDeclarationKind::Const);
    let mut out = Vec::new();
    for d in &v.declarations {
        let names = binding_names(&d.id);
        if names.is_empty() {
            continue;
        }

        // `const X = require('./y');` — and the two wider CommonJS-import
        // shapes `is_commonjs_import_expr` also recognizes — are CommonJS's
        // import syntax, not a real constant, the same relationship
        // `import X from './y';` has to a `Const`. Before the first check,
        // the initializer's raw source text was captured as the declared
        // "value" regardless of what it was, so `ws`'s `index.js` (`const
        // WebSocket = require('./lib/websocket');`) produced a `Const { ty:
        // Any, value: Some("\"require('./lib/websocket')\"") }` — a require
        // call rendered as if it were a string literal constant, sitting
        // alongside the real `WebSocket` class `graph.rs`'s `require()`
        // edges (and `wrapper.mjs`'s real ESM chain) already resolve to.
        // Widened after the bare-call check alone left two further real
        // shapes fabricating the same way: `class-validator`'s bundled
        // `.umd.js` (`const isEmail_1 =
        // __importDefault(require("validator/lib/isEmail"));` — `tsc`'s own
        // `esModuleInterop` output, ~70 occurrences in that one file) and
        // `commander`'s `const EventEmitter = require('events').EventEmitter;`
        // (a property read off a require call). Skipped exactly like an ESM
        // import in all three shapes: no `DeclFact` at all, not a
        // `Const`/`Static` with a fabricated value. `ExpressionStatement`
        // forms (`module.exports = require('./y');`) never had this bug —
        // `extract_statement` has no dispatch arm for a bare
        // `ExpressionStatement`, so they already produce no `DeclFact`.
        if d.init.as_ref().is_some_and(is_commonjs_import_expr) {
            continue;
        }

        let span = d.span();
        let doc = jsdoc::jsdoc_for_span(semantic, span);
        if doc.ignore {
            continue;
        }
        let ty = d
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type(&ann.type_annotation, source));
        let satisfied = d.init.as_ref().and_then(|e| satisfies_type(e, source));
        let value = d
            .init
            .as_ref()
            .map(|e| format!("{:?}", e.span().source_text(source)));

        // `force_exported` carries the statement-level `export` keyword
        // (`export const x = 1;` — every declarator in the statement is
        // exported, no per-name check needed or possible). Absent that,
        // each binding's own name is checked against `exported_names`
        // independently, because one `var`/`let`/`const` statement can bind
        // several names with different export status — see the call site
        // in `extract_statement` for why this cannot be decided once per
        // statement the way `FunctionDeclaration`/`ClassDeclaration` can.
        // A pattern (`{ left, right: renamed }`, `[first, second]`) declares
        // each binding, not one symbol whose name is the pattern text.
        for name in names {
            let discriminant = bump_count(&name, name_counts);
            let body = if is_const {
                DeclBody::Const(ConstBody {
                    ty: ty.clone(),
                    value: value.clone(),
                    satisfies: satisfied.clone(),
                })
            } else {
                DeclBody::Static(StaticBody {
                    ty: ty.clone(),
                    value: value.clone(),
                    is_mutable: matches!(v.kind, VariableDeclarationKind::Let),
                })
            };
            let is_exported = force_exported || exported_names.contains(&name);
            let visibility = if is_exported {
                Visibility::Public
            } else {
                Visibility::Private
            };
            out.push(DeclFact {
                name,
                visibility,
                doc: doc.clone(),
                body,
                module: path.to_path_buf(),
                span_start: span.start,
                span_end: span.end,
                is_default: false,
                decl_index: discriminant,
            });
        }
    }
    out
}

/// `declare global { ... }` augments the global scope. Every declaration in
/// the block is public there, including ones written without `export`.
fn lower_global<'a>(
    global: &'a TSGlobalDeclaration<'a>,
    source: &'a str,
    semantic: &'a Semantic<'a>,
    path: &Path,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> Vec<DeclFact> {
    use nudox_ir::entry::Visibility;

    let span = global.span();
    let doc = jsdoc::jsdoc_for_span(semantic, span);
    if doc.ignore {
        return vec![];
    }
    let name = "global".to_string();
    let discriminant = bump_count(&name, name_counts);
    let mut child_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::default();
    let mut children = Vec::new();
    for stmt in &global.body.body {
        children.extend(extract_statement(
            stmt,
            source,
            semantic,
            path,
            &std::collections::HashSet::default(),
            &None,
            &mut child_counts,
        ));
    }
    for child in &mut children {
        child.visibility = Visibility::Public;
    }
    vec![DeclFact {
        name,
        visibility: Visibility::Public,
        doc,
        body: DeclBody::Namespace(NamespaceBody {
            is_ambient: true,
            children,
        }),
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default: false,
        decl_index: discriminant,
    }]
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
            let mut child_counts: std::collections::HashMap<String, u32> =
                std::collections::HashMap::default();
            let mut children = Vec::new();
            for stmt in &block.body {
                let decls = extract_statement(
                    stmt,
                    source,
                    semantic,
                    path,
                    &std::collections::HashSet::default(),
                    &None,
                    &mut child_counts,
                );
                children.extend(decls);
            }
            children
        }
        Some(TSModuleDeclarationBody::TSModuleDeclaration(inner)) => lower_namespace(
            inner,
            source,
            semantic,
            path,
            is_exported,
            false,
            name_counts,
        ),
        None => vec![],
    };

    let visibility = if is_exported {
        Visibility::Public
    } else {
        Visibility::Private
    };

    vec![DeclFact {
        name,
        visibility,
        doc,
        body: DeclBody::Namespace(NamespaceBody {
            is_ambient,
            children,
        }),
        module: path.to_path_buf(),
        span_start: span.start,
        span_end: span.end,
        is_default,
        decl_index: discriminant,
    }]
}

// ── Export / import table builders
// ─────────────────────────────────────────────

fn build_export_table(
    module_record: &ModuleRecord<'_>,
    exported_names: &std::collections::HashSet<String>,
    default_local_name: &Option<String>,
    declarations: &[DeclFact],
    cjs_exports: &CommonJsExports,
) -> ExportTable {
    let exported_names_list: Vec<String> = exported_names.iter().cloned().collect();

    let mut indirect: Vec<IndirectExport> = Vec::new();
    let mut star: Vec<StarExport> = Vec::new();

    // Statement spans of default-import declarations (`import D from "m"`).
    // oxc_parser's `resolve_export_entries` (module_record.rs step
    // 10.a.ii.3) synthesizes an indirect export entry for `export { D };`
    // re-exporting one of these, and — per its own comment,
    // "`import d from "mod"` / `export { d }` / ^ this is local_name of ie"
    // — sets that entry's `import_name` to `D`, the LOCAL alias *this* file
    // chose, not the string `"default"` that actually names the binding
    // inside `m`. `zod`'s `errors.d.ts` hits this directly: `import
    // defaultErrorMap from "./locales/en"; export { defaultErrorMap };`
    // produces an indirect entry naming `en.d.ts`'s target "defaultErrorMap"
    // — a name `en.d.ts` never declares — instead of "default", which it
    // does. The synthesized entry's `statement_span` is copied from the
    // *import* statement (`ie.statement_span`), unchanged from the original
    // import — matching on it (not on the local name, which could
    // coincidentally collide with an unrelated real named export) is exact.
    let default_import_spans: std::collections::HashSet<oxc_span::Span> = module_record
        .import_entries
        .iter()
        .filter(|e| e.import_name.is_default())
        .map(|e| e.statement_span)
        .collect();

    for e in &module_record.indirect_export_entries {
        let Some(module_request) = e.module_request.as_ref().map(|n| n.name.to_string()) else {
            continue;
        };
        let import_name = match &e.import_name {
            ExportImportName::Name(_) if default_import_spans.contains(&e.statement_span) => {
                "default".to_string()
            }
            ExportImportName::Name(ns) => ns.name.to_string(),
            ExportImportName::All | ExportImportName::AllButDefault => "*".to_string(),
            ExportImportName::Null => continue,
        };
        let export_name = match &e.export_name {
            ExportExportName::Name(ns) => ns.name.to_string(),
            ExportExportName::Default(_) => "default".to_string(),
            ExportExportName::Null => continue,
        };
        indirect.push(IndirectExport {
            module_request,
            import_name,
            export_name,
            span_start: e.statement_span.start,
            span_end: e.statement_span.end,
        });
    }

    for e in &module_record.star_export_entries {
        let Some(module_request) = e.module_request.as_ref().map(|n| n.name.to_string()) else {
            continue;
        };
        star.push(StarExport {
            module_request,
            span_start: e.statement_span.start,
            span_end: e.statement_span.end,
        });
    }

    let mut locals = build_local_export_map(module_record, declarations);

    // CommonJS interop: `module.exports = <ident>;` makes this module
    // resolvable exactly the way a real ESM `export default <ident>;` would
    // be. `resolve_export_target` (emit.rs) looks up `locals.get("default")`
    // whenever another module does `import X from "this-module"` — real ESM
    // importing a CommonJS dependency, which is exactly what `ws`'s
    // `wrapper.mjs` does to every file under `lib/`. Without this, that
    // lookup misses, falls through to `resolve_export_target`'s "genuine
    // indirect re-export" branch, and builds a `TsId` naming "default" that
    // nothing here ever `declare()`s — `Lowering::finish` then rejects the
    // whole package as "referred but never declared" the moment a real ESM
    // entry point re-exports a CommonJS sibling (reproduced against `ws`
    // 8.16.0 before this fix). `.entry(...).or_insert(...)` never overwrites
    // a real ESM local export — there should never be one under "default" in
    // a file that also has `module.exports = …`, but this keeps ESM
    // authoritative if some future syntax mix produced one anyway.
    // CommonJS values have no TS-overload syntax, but the *name* a
    // `module.exports`/`exports.X` assignment names can still legitimately
    // collect more than one declaration the same way any other top-level
    // name can (declaration merging, or simply two unrelated declarations
    // that happen to share a name) — so this counts `declarations` exactly
    // like `build_local_export_map`'s own `overload_count_of` above, rather
    // than assuming 1.
    let cjs_overload_count = |name: &str| -> u32 {
        declarations
            .iter()
            .filter(|d| d.name == name)
            .count()
            .max(1) as u32
    };
    if let Some(local_name) = &cjs_exports.whole_module {
        locals
            .entry("default".to_string())
            .or_insert_with(|| LocalExport::Named {
                overload_count: cjs_overload_count(local_name),
                local_name: local_name.clone(),
            });
    }
    for (export_name, local_name) in &cjs_exports.named {
        locals
            .entry(export_name.clone())
            .or_insert_with(|| LocalExport::Named {
                overload_count: cjs_overload_count(local_name),
                local_name: local_name.clone(),
            });
    }

    ExportTable {
        exported_names: exported_names_list,
        indirect,
        star,
        default_local_name: default_local_name.clone(),
        locals,
    }
}

/// Build the `export name -> LocalExport` resolution map for every export
/// this module produces *without* a `from` clause. See [`LocalExport`]'s doc
/// comment for why an export's externally-visible name and the `TsId` this
/// module's own emitter declares for it can differ.
fn build_local_export_map(
    module_record: &ModuleRecord<'_>,
    declarations: &[DeclFact],
) -> std::collections::HashMap<String, LocalExport> {
    // Namespace-object imports, keyed by their local binding name
    // (`import * as z from "m"` -> "z" -> "m"). `local_export_entries`
    // conflates a plain local declaration with a re-exported namespace
    // import -- both land there per oxc_parser's `resolve_export_entries`
    // (module_record.rs, step 10.a.ii.2: "this is a re-export of an
    // imported module namespace object" appends to `localExportEntries`,
    // same as an ordinary declaration) -- so this is how we tell them apart.
    let namespace_imports: std::collections::HashMap<&str, &str> = module_record
        .import_entries
        .iter()
        .filter(|e| e.import_name.is_namespace_object())
        .map(|e| (e.local_name.name.as_str(), e.module_request.name.as_str()))
        .collect();

    // Top-level names this module's own emitter actually declares a `TsId`
    // for (see `decl_ts_id` in emit.rs: unqualified for a module-root
    // parent, which is exactly what every entry in `declarations` has here).
    // Used below to tell an anonymous default export backed by a real
    // declaration (`export default function() {}` / `export default class
    // {}` -- `extract_default_export` itself falls back to naming these
    // "default") apart from one backed by nothing nameable at all
    // (`export default "a literal";` / `export default 42;`), which
    // `ExportLocalName::Null` alone cannot distinguish.
    let declared_names: std::collections::HashSet<&str> =
        declarations.iter().map(|d| d.name.as_str()).collect();

    // How many declarations this module's own emitter will give each name —
    // `LocalExport::Named`'s `overload_count`. `bump_count` (this module's
    // discriminant assignment) hands out `0..count` for a same-named group,
    // so counting occurrences in `declarations` here reconstructs exactly
    // that count without re-deriving it a second, possibly-inconsistent way.
    let mut overload_counts: std::collections::HashMap<&str, u32> =
        std::collections::HashMap::new();
    for d in declarations {
        *overload_counts.entry(d.name.as_str()).or_insert(0) += 1;
    }
    // A name with no declaration at all (only possible for the synthetic
    // "default" key inserted below, when it names something CommonJS-only)
    // still needs a valid, non-zero count — 1, matching the pre-existing
    // single-target behaviour, is the honest fallback since nothing here
    // observed more than one.
    let overload_count_of = |name: &str| overload_counts.get(name).copied().unwrap_or(1).max(1);

    let mut locals = std::collections::HashMap::new();

    for e in &module_record.local_export_entries {
        let export_name = match &e.export_name {
            ExportExportName::Name(ns) => ns.name.to_string(),
            ExportExportName::Default(_) => "default".to_string(),
            ExportExportName::Null => continue,
        };

        let local_name: Option<&str> = match &e.local_name {
            ExportLocalName::Name(ns) | ExportLocalName::Default(ns) => Some(ns.name.as_str()),
            ExportLocalName::Null => None,
        };

        let resolution = match local_name {
            Some(name) => match namespace_imports.get(name) {
                Some(&module_request) => LocalExport::NamespaceOf(module_request.to_string()),
                None => LocalExport::Named {
                    local_name: name.to_string(),
                    overload_count: overload_count_of(name),
                },
            },
            // Identifier and named-declaration defaults are covered by the
            // `Some` arm above (their `local_name` names the declaration);
            // this is `export default <expression>;` with no attached
            // identifier at all.
            None if declared_names.contains("default") => LocalExport::Named {
                local_name: "default".to_string(),
                overload_count: overload_count_of("default"),
            },
            None => LocalExport::Unresolvable,
        };

        locals.insert(export_name, resolution);
    }

    locals
}

fn build_import_table(module_record: &ModuleRecord<'_>) -> Vec<ImportFact> {
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

// ── Shared helpers
// ─────────────────────────────────────────────────────────────

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

fn signature_doc(doc: &super::DocFacts) -> Option<String> {
    let mut text = String::new();
    if let Some(body) = &doc.doc {
        text.push_str(body);
    }
    if let Some(deprecation) = &doc.deprecation {
        text.push_str("~dep:");
        if let Some(note) = &deprecation.note {
            text.push_str(note);
        }
        if let Some(since) = &deprecation.since {
            text.push('@');
            text.push_str(since);
        }
    }
    if doc.ignore {
        text.push_str("~ignore");
    }
    if text.is_empty() { None } else { Some(text) }
}

/// Every identifier a pattern binds. `{ left, right: renamed, ...rest }`
/// yields `left`, `renamed`, and `rest`. A hole (`[, second]`) contributes
/// nothing.
fn binding_names(pat: &BindingPattern<'_>) -> Vec<String> {
    let mut names = Vec::new();
    collect_binding_names(pat, &mut names);
    names
}

fn collect_binding_names(pat: &BindingPattern<'_>, names: &mut Vec<String>) {
    match pat {
        BindingPattern::BindingIdentifier(id) => names.push(id.name.to_string()),
        BindingPattern::AssignmentPattern(ap) => collect_binding_names(&ap.left, names),
        BindingPattern::ObjectPattern(pat) => {
            for prop in &pat.properties {
                collect_binding_names(&prop.value, names);
            }
            if let Some(rest) = &pat.rest {
                collect_binding_names(&rest.argument, names);
            }
        }
        BindingPattern::ArrayPattern(pat) => {
            for elem in pat.elements.iter().flatten() {
                collect_binding_names(elem, names);
            }
            if let Some(rest) = &pat.rest {
                collect_binding_names(&rest.argument, names);
            }
        }
    }
}

fn binding_pattern_name(pat: &BindingPattern<'_>, source: &str) -> Option<String> {
    match pat {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        BindingPattern::AssignmentPattern(ap) => binding_pattern_name(&ap.left, source),
        BindingPattern::ObjectPattern(pat) => Some(pat.span().source_text(source).to_string()),
        BindingPattern::ArrayPattern(pat) => Some(pat.span().source_text(source).to_string()),
    }
}

fn bump_count(name: &str, counts: &mut std::collections::HashMap<String, u32>) -> u32 {
    let entry = counts.entry(name.to_string()).or_insert(0);
    let n = *entry;
    *entry += 1;
    n
}
