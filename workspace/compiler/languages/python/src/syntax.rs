//! Ruff-based, in-process, syntactic front end for [`PythonOracle`].
//!
//! # Why syntactic, and why ruff
//!
//! `lib.rs`'s module doc (see "pyrefly feature gate") records why the
//! semantic (type-checking) tier is unavailable today: `pyrefly` pins
//! `blake3 =1.8.2` against `workspace/index`'s `iroh` requirement of
//! `^1.8.3` — a hard dependency-resolution conflict — and its own gated
//! `context.rs` module was never checked in. Neither blocker is syntactic;
//! both are specific to running a *type checker* in-process.
//!
//! `ruff_python_parser` + `ruff_python_ast` have none of that baggage: they
//! are a parser and an AST, nothing more, exactly mirroring the shape of
//! `nudox-producer-typescript`'s OXC front end (`extract/decl.rs`,
//! `graph.rs`) — parse, walk the AST, extract into owned data, drop the
//! arena. This module is `PythonOracle`'s syntactic front end: everything it
//! reports is directly visible in the source text (a `def`, a `class`, a
//! written annotation, a base-class name, a decorator token). It never runs
//! Python, never resolves imports across files, and never infers a type that
//! was not spelled out in the source.
//!
//! # What this cannot do (see `lib.rs`'s "Constructs not yet representable"
//! and the CC-2 report filed against this task)
//!
//! - Cross-module name resolution. A `Foo` referenced in one file and
//!   defined in another lowers to `TypeData::Nominal("Foo")`, which
//!   `types::lower_nominal` can only resolve if `"Foo"` (after stripping any
//!   dotted prefix) is a *string match* against a fully-qualified id this
//!   same package declared. Import aliasing (`import numpy as np`) is not
//!   unwound, so `np.ndarray` never matches a same-package id and always
//!   degrades through the cross-package `Type::Any` fallback.
//! - Forward-reference strings (`x: "Foo"`) are recorded as
//!   `TypeData::Nominal("Foo")` verbatim, without re-parsing the string as a
//!   nested type expression. A forward reference to something more complex
//!   than a bare name (`x: "List[Foo]"`) is not unwound and lowers as a
//!   single opaque nominal.
//! - Statements inside control flow (`if TYPE_CHECKING:`, `try:`/`except
//!   ImportError:`, `if sys.version_info >= ...:`) are not walked. Only
//!   direct children of a module, class, or function body are declarations.
//!   This mirrors real Python scoping closely enough for top-level public
//!   API (which is essentially never conditionally defined) but will miss
//!   declarations some packages hide behind a version or typing guard.
//! - `TypeVar`/`ParamSpec`/`TypeVarTuple` bounds and constraints from the
//!   legacy call-based form (`T = TypeVar("T", bound=Foo)`) are recognized
//!   for name purposes (so `T` still lowers to `Type::TypeVar`, never
//!   `Type::Any`) but their `bound=`/constraint arguments are not parsed;
//!   only the PEP 695 `[T: Foo]` form carries a bound through.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use nudox_producer::{PackageSource, ProducerError};
use ruff_python_ast::{
    Decorator, Expr, ExprSubscript, ModModule, Operator, Parameter, ParameterWithDefault, Stmt,
    StmtClassDef, StmtFunctionDef, TypeParam, TypeParams,
    helpers::{body_without_leading_docstring, is_docstring_stmt, is_stub_body},
};
use ruff_text_size::Ranged;

use crate::docstring::{self, ParsedDocstring};
use crate::oracle::{
    AliasData, ClassData, ClassForm, ConstData, DeprecationData, FieldData, FunctionData,
    GenericParamData, ItemBody, ItemData, ModuleData, ParamData, ParamKind, PythonId, PythonOracle,
    ReceiverKind, TypeData,
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Walk every `.py` file reachable from `src.root()`, parse it with
/// `ruff_python_parser`, and extract a [`PythonOracle`].
///
/// A file that fails to read or fails to parse (genuine Python syntax
/// errors, or a grammar `ruff_python_parser` does not accept) is skipped
/// with a logged diagnostic rather than failing the whole package — one
/// unparseable vendored file must not blank out an otherwise-good package,
/// matching `nudox-producer-typescript`'s `graph.rs` precedent ("OXC parser
/// panicked; skipping module").
///
/// Only a failure to walk the package *root itself* (it does not exist, or
/// is not readable) is reported as a [`ProducerError`] — everything
/// downstream of that is a best-effort partial result, never a hard failure.
pub fn build_oracle(src: &PackageSource) -> Result<PythonOracle, ProducerError> {
    let files = discover_py_files(src.root()).map_err(|reason| ProducerError::OracleSpawn {
        command: "ruff-syntax-walk".to_owned(),
        reason,
    })?;

    let mut modules = Vec::with_capacity(files.len());
    let mut seen_names: HashSet<String> = HashSet::with_capacity(files.len());

    for file in files {
        let source = match std::fs::read_to_string(&file) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    path = %file.display(),
                    error = %err,
                    "failed to read Python source; skipping module"
                );
                continue;
            }
        };

        let parsed = match ruff_python_parser::parse_module(&source) {
            Ok(p) => p,
            Err(err) => {
                tracing::debug!(
                    path = %file.display(),
                    error = %err,
                    "ruff rejected this file as invalid Python syntax; skipping"
                );
                continue;
            }
        };

        let module_name = module_dotted_name(&file);
        if module_name.is_empty() {
            continue;
        }
        if !seen_names.insert(module_name.clone()) {
            // Two files under unrelated directory trees produced the same
            // dotted name (e.g. a stray same-named file the exclusion list
            // did not catch). Declaring both would collide on `PythonId` and
            // fail `Lowering::finish`; skip the second rather than corrupt
            // the first.
            tracing::warn!(
                path = %file.display(),
                module = %module_name,
                "duplicate module name; skipping to avoid a PythonId collision"
            );
            continue;
        }

        modules.push(extract_module(parsed.syntax(), &source, module_name));
    }

    Ok(PythonOracle { modules })
}

// ---------------------------------------------------------------------------
// File discovery
// ---------------------------------------------------------------------------

/// Directory basenames that never contain the public API surface of a
/// package: test suites, documentation, build tooling, packaging metadata.
/// Matched case-insensitively against a single path component, never a
/// substring, so a real subpackage that happens to contain one of these
/// words (`pydantic/plugin`) is never excluded.
const EXCLUDED_DIR_NAMES: &[&str] = &[
    "test",
    "tests",
    "testing",
    "docs",
    "doc",
    "documentation",
    "examples",
    "example",
    "benchmark",
    "benchmarks",
    "gallery",
    "scripts",
    "tools",
    "build",
    "dist",
    "requirements",
    "action",
    "autoload",
    "ci_tools",
    "changelog.d",
    "node_modules",
    "venv",
];

fn is_excluded_dir(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    if name == "__pycache__" {
        return true;
    }
    if name.ends_with(".egg-info") || name.ends_with(".dist-info") {
        return true;
    }
    EXCLUDED_DIR_NAMES.iter().any(|d| name.eq_ignore_ascii_case(d))
}

fn is_excluded_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "setup.py"
        || lower == "conftest.py"
        || lower == "noxfile.py"
        || lower.starts_with("test_")
        || lower.ends_with("_test.py")
}

/// Depth-first walk of `root`, collecting every non-excluded `.py` file.
///
/// Returns `Err` only when `root` itself cannot be listed; a subdirectory
/// that becomes unreadable mid-walk (permissions, a symlink cycle resolved
/// away, …) is logged and skipped rather than aborting the whole walk.
fn discover_py_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(err) if dir == root => return Err(err),
            Err(err) => {
                tracing::debug!(path = %dir.display(), error = %err, "cannot read directory; skipping");
                continue;
            }
        };

        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if path.is_dir() {
                if is_excluded_dir(&name) {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
                if is_excluded_file(&name) {
                    continue;
                }
                out.push(path);
            }
        }
    }

    // Deterministic order: downstream ids are derived from path, not walk
    // order, but a stable order makes diagnostics and tests reproducible.
    out.sort();
    Ok(out)
}

/// Derive a file's dotted module name from its position in the package tree.
///
/// A directory contributes a dotted segment **iff it itself contains an
/// `__init__.py`** — i.e. Python's own definition of "this directory is a
/// package". Climbing stops at the first ancestor without one, which is
/// exactly the sdist's source root, whatever that happens to be named
/// (`src/`, `lib/`, the sdist root itself for a single-file module like
/// `six.py`). This needs no prior knowledge of the PyPI project name or of
/// sdist layout conventions (`src/`-layout vs. flat vs. `lib/`-layout all
/// fall out of the same rule).
fn module_dotted_name(file: &Path) -> String {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut segments: Vec<String> = Vec::new();
    if stem != "__init__" && !stem.is_empty() {
        segments.push(stem.to_owned());
    }

    let mut cur = file.parent().map(Path::to_path_buf);
    while let Some(dir) = cur {
        if !dir.join("__init__.py").is_file() {
            break;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            break;
        };
        segments.push(name.to_owned());
        cur = dir.parent().map(Path::to_path_buf);
    }

    segments.reverse();
    segments.join(".")
}

// ---------------------------------------------------------------------------
// Module extraction
// ---------------------------------------------------------------------------

fn extract_module(module: &ModModule, source: &str, module_name: String) -> ModuleData {
    let parsed_doc = leading_docstring(&module.body).map(docstring::parse);
    let documentation = parsed_doc.as_ref().and_then(ParsedDocstring::documentation);

    let typevars = collect_typevar_names(&module.body, source);
    let items = walk_scope(&module.body, &module_name, source, &typevars, false, true);

    ModuleData {
        name: module_name,
        documentation,
        deprecation: None,
        items,
    }
}

/// Extract the module/class/function's own leading docstring, if its first
/// statement is a bare string-literal expression statement.
fn leading_docstring(body: &[Stmt]) -> Option<&str> {
    let first = body.first()?;
    if !is_docstring_stmt(first) {
        return None;
    }
    let Stmt::Expr(expr_stmt) = first else {
        return None;
    };
    let Expr::StringLiteral(lit) = expr_stmt.value.as_ref() else {
        return None;
    };
    Some(lit.value.to_str())
}

/// Scan a scope's *direct* statements for legacy `X = TypeVar("X", ...)` /
/// `ParamSpec(...)` / `TypeVarTuple(...)` bindings, returning the bound
/// names.
///
/// This is what lets `T` in `def f(x: T) -> T` lower to `Type::TypeVar`
/// rather than `Type::Any` for packages that predate PEP 695's `[T]`
/// syntax — i.e. nearly all of the pypi corpus. Bounds/constraints on the
/// call are not parsed (see this file's module doc); only the name is.
fn collect_typevar_names(body: &[Stmt], source: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in body {
        let Stmt::Assign(assign) = stmt else { continue };
        let [Expr::Name(target)] = assign.targets.as_slice() else {
            continue;
        };
        let Expr::Call(call) = assign.value.as_ref() else {
            continue;
        };
        let head = decorator_head_name(&call.func, source);
        let last = head.rsplit('.').next().unwrap_or(&head);
        if matches!(last, "TypeVar" | "TypeVarTuple" | "ParamSpec") {
            out.insert(target.id.as_str().to_owned());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Scope walking: functions, classes, and (at module scope) bindings
// ---------------------------------------------------------------------------

/// Walk one scope's direct statements into [`ItemData`]s.
///
/// `is_method_scope` controls whether a leading `self`/`@classmethod`
/// /`@staticmethod` parameter is read as a receiver (true for class bodies,
/// false for module bodies — a free function's first parameter is never a
/// receiver no matter what it is named).
///
/// `include_bindings` controls whether `Assign`/`AnnAssign`/`TypeAlias`
/// statements become `ItemBody::Const`/`ItemBody::Alias` entries. It is
/// `true` at module scope and `false` at class scope, because a class-body
/// binding is a *field* (`ClassData::fields`, extracted separately by
/// [`extract_fields`]), never a standalone declaration.
///
/// A single `declared` name set is shared across every statement kind this
/// walks (functions, classes, and — at module scope — bindings), and the
/// *first* declaration of a name wins. Real packages in the corpus rely on
/// this: `attrs` writes `class Attribute: ...` then later rebinds
/// `Attribute = _add_hash(_add_eq(...))(Attribute)` to attach synthesized
/// dunders to the same class; `pydantic` and `python-dateutil` both reuse a
/// bare module-level name as a two-phase sentinel
/// (`_is_base_model_class_defined = False` … `_is_base_model_class_defined =
/// True`). Every one of these is a rebinding of an already-declared name, not
/// a second public symbol, and without this dedup they collide on
/// `PythonId` and fail `Lowering::finish` outright — which is exactly what
/// happened here before this had a shared set (5 of the 22 corpus entries
/// failed this way on the first real sweep).
fn walk_scope(
    body: &[Stmt],
    parent: &str,
    source: &str,
    typevars: &HashSet<String>,
    is_method_scope: bool,
    include_bindings: bool,
) -> Vec<ItemData> {
    let mut items = Vec::new();
    let mut pending: Vec<&StmtFunctionDef> = Vec::new();
    let mut declared: HashSet<String> = HashSet::new();

    let flush = |pending: &mut Vec<&StmtFunctionDef>, items: &mut Vec<ItemData>, declared: &mut HashSet<String>| {
        if !pending.is_empty() {
            if declared.insert(pending[0].name.as_str().to_owned()) {
                items.push(build_function_group(pending, parent, source, typevars, is_method_scope));
            }
            pending.clear();
        }
    };

    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                if let Some(last) = pending.last()
                    && last.name.as_str() != f.name.as_str()
                {
                    flush(&mut pending, &mut items, &mut declared);
                }
                pending.push(f);
            }
            Stmt::ClassDef(c) => {
                flush(&mut pending, &mut items, &mut declared);
                if declared.insert(c.name.as_str().to_owned()) {
                    items.push(extract_class(c, parent, source, typevars));
                }
            }
            Stmt::Assign(_) | Stmt::AnnAssign(_) | Stmt::TypeAlias(_) if include_bindings => {
                flush(&mut pending, &mut items, &mut declared);
                if let Some(item) = binding_item(stmt, parent, source, typevars)
                    && declared.insert(item.name.clone())
                {
                    items.push(item);
                }
            }
            _ => {
                flush(&mut pending, &mut items, &mut declared);
            }
        }
    }
    flush(&mut pending, &mut items, &mut declared);

    items
}

/// Turn a run of consecutive same-named `def`s into one [`ItemData`].
///
/// Two shapes reach here with more than one branch:
///
/// - A genuine `@overload` chain (plus, conventionally, an undecorated
///   implementation last) — each branch becomes its own declaration per
///   `oracle.rs`'s `PythonId` scheme (`base#0`, `base#1`, …), matching the
///   mission brief: "each overload is its own declaration, never folded".
/// - A `@property` getter followed by its `@x.setter`/`@x.deleter` — these
///   share a name but are not overloads of one callable; folding them into
///   `Overloaded` would misrepresent the property as a multi-signature
///   function. Detected by decorator suffix and collapsed to the first
///   (getter) definition only.
///
/// Any other repeated same-name run (a conditional redefinition our
/// non-descent into control flow never actually sees twice, or a genuine
/// authoring mistake) safely falls into the `Overloaded` branch: every
/// branch still gets a distinct `#N` id, so nothing collides at `declare()`.
fn build_function_group(
    group: &[&StmtFunctionDef],
    parent: &str,
    source: &str,
    typevars: &HashSet<String>,
    is_method_scope: bool,
) -> ItemData {
    let first = group[0];
    let name = first.name.as_str().to_owned();
    let item_id = format!("{parent}.{name}");

    let is_property_pair = group.iter().skip(1).any(|f| {
        f.decorator_list.iter().any(|d| {
            let tok = decorator_head_name(&d.expression, source);
            tok.ends_with(".setter") || tok.ends_with(".deleter") || tok.ends_with(".getter")
        })
    });

    let effective: Vec<&StmtFunctionDef> = if is_property_pair { vec![first] } else { group.to_vec() };

    let decorators = decorator_tokens(&first.decorator_list, source);
    let parsed_doc = leading_docstring(&first.body).map(docstring::parse);
    let documentation = parsed_doc.as_ref().and_then(ParsedDocstring::documentation);
    let deprecation = deprecation_from_decorators(&first.decorator_list, source);

    let body = if effective.len() == 1 {
        let doc = leading_docstring(&effective[0].body).map(docstring::parse);
        ItemBody::Function(function_data(effective[0], source, typevars, is_method_scope, doc.as_ref()))
    } else {
        let branches = effective
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let doc = leading_docstring(&f.body).map(docstring::parse);
                let mut fd = function_data(f, source, typevars, is_method_scope, doc.as_ref());
                fd.overload_index = i;
                fd
            })
            .collect();
        ItemBody::Overloaded(branches)
    };

    ItemData {
        id: PythonId::new(item_id),
        parent: Some(PythonId::new(parent.to_owned())),
        is_private: name.starts_with('_'),
        name,
        documentation,
        deprecation,
        decorators,
        body,
    }
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

fn function_data(
    f: &StmtFunctionDef,
    source: &str,
    typevars: &HashSet<String>,
    is_method_scope: bool,
    doc: Option<&ParsedDocstring>,
) -> FunctionData {
    let decorators = decorator_tokens(&f.decorator_list, source);

    let mut local_typevars = typevars.clone();
    for n in pep695_names(f.type_params.as_deref()) {
        local_typevars.insert(n);
    }

    let mut params = Vec::new();
    for p in f.parameters.posonlyargs.iter() {
        params.push(param_data(p, ParamKind::PositionalOnly, source, &local_typevars, doc));
    }
    for p in f.parameters.args.iter() {
        params.push(param_data(p, ParamKind::Normal, source, &local_typevars, doc));
    }
    if let Some(va) = f.parameters.vararg.as_deref() {
        params.push(bare_param(va, ParamKind::Varargs, source, &local_typevars, doc));
    }
    for p in f.parameters.kwonlyargs.iter() {
        params.push(param_data(p, ParamKind::KeywordOnly, source, &local_typevars, doc));
    }
    if let Some(kw) = f.parameters.kwarg.as_deref() {
        params.push(bare_param(kw, ParamKind::Kwargs, source, &local_typevars, doc));
    }

    let is_static = decorators.iter().any(|d| d == "staticmethod");
    let is_classmethod = decorators.iter().any(|d| d == "classmethod");
    let first_name = params.first().map(|p| p.name.as_str());
    let receiver = if !is_method_scope {
        ReceiverKind::None
    } else if is_static {
        ReceiverKind::Static
    } else if is_classmethod {
        ReceiverKind::ClassMethod
    } else if first_name == Some("self") {
        ReceiverKind::SharedRef
    } else {
        ReceiverKind::None
    };

    let return_ty = f.returns.as_deref().map(|e| expr_to_type(e, source, &local_typevars));
    let generics = pep695_generics(f.type_params.as_deref(), source, &local_typevars);
    let is_abstract = decorators.iter().any(|d| {
        let last = d.rsplit('.').next().unwrap_or(d);
        last == "abstractmethod" || last == "abstractproperty"
    });
    let is_stub = is_stub_body(body_without_leading_docstring(&f.body));

    FunctionData {
        overload_index: 0,
        receiver,
        params,
        return_ty,
        generics,
        is_async: f.is_async,
        is_abstract,
        is_stub,
    }
}

fn bare_param(
    p: &Parameter,
    kind: ParamKind,
    source: &str,
    typevars: &HashSet<String>,
    doc: Option<&ParsedDocstring>,
) -> ParamData {
    let name = p.name.as_str().to_owned();
    let ty = p.annotation.as_deref().map(|e| expr_to_type(e, source, typevars));
    let doc_description = doc.and_then(|d| d.params.get(&name).cloned());
    ParamData { name, ty, kind, has_default: false, doc_description }
}

fn param_data(
    p: &ParameterWithDefault,
    kind: ParamKind,
    source: &str,
    typevars: &HashSet<String>,
    doc: Option<&ParsedDocstring>,
) -> ParamData {
    let name = p.parameter.name.as_str().to_owned();
    let ty = p.parameter.annotation.as_deref().map(|e| expr_to_type(e, source, typevars));
    let has_default = p.default.is_some();
    let doc_description = doc.and_then(|d| d.params.get(&name).cloned());
    ParamData { name, ty, kind, has_default, doc_description }
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

fn extract_class(c: &StmtClassDef, parent: &str, source: &str, module_typevars: &HashSet<String>) -> ItemData {
    let name = c.name.as_str().to_owned();
    let class_id = format!("{parent}.{name}");
    let decorators = decorator_tokens(&c.decorator_list, source);

    let base_exprs: &[Expr] = c.arguments.as_deref().map(|a| a.args.as_ref()).unwrap_or(&[]);

    let mut typevars = module_typevars.clone();
    for n in pep695_names(c.type_params.as_deref()) {
        typevars.insert(n);
    }

    let super_types: Vec<TypeData> = base_exprs.iter().map(|e| expr_to_type(e, source, &typevars)).collect();
    let base_names: Vec<String> = base_exprs.iter().map(|e| dotted_name(e, source)).collect();
    let form = classify_form(&base_names, &decorators);
    let generics = pep695_generics(c.type_params.as_deref(), source, &typevars);

    let parsed_doc = leading_docstring(&c.body).map(docstring::parse);
    let documentation = parsed_doc.as_ref().and_then(ParsedDocstring::documentation);

    // Methods/nested classes are extracted first so their names can be
    // excluded from field extraction below — see `extract_fields`'s doc
    // comment for why (a `def foo(...): ...` followed later by a rebinding
    // `foo = some_decorator(foo)` is a real pattern in the corpus, e.g.
    // `sqlalchemy`'s `OrderingList._raw_append = collection.adds(1)(_raw_append)`,
    // and both a method and a field declaring the same `PythonId` is a
    // `Lowering::finish` collision, not two distinct members).
    let scope_items = walk_scope(&c.body, &class_id, source, &typevars, true, false);
    let method_and_nested_names: HashSet<String> = scope_items.iter().map(|i| i.name.clone()).collect();

    let mut methods = Vec::with_capacity(scope_items.len());
    let mut nested = Vec::new();
    for item in scope_items {
        match item.body {
            ItemBody::Function(_) | ItemBody::Overloaded(_) => methods.push(item),
            ItemBody::Class(_) => nested.push(item),
            ItemBody::Module | ItemBody::Const(_) | ItemBody::Alias(_) => {}
        }
    }

    let fields = extract_fields(&c.body, source, &typevars, parsed_doc.as_ref(), &method_and_nested_names);

    ItemData {
        id: PythonId::new(class_id),
        parent: Some(PythonId::new(parent.to_owned())),
        is_private: name.starts_with('_'),
        name,
        documentation,
        deprecation: deprecation_from_decorators(&c.decorator_list, source),
        decorators,
        body: ItemBody::Class(ClassData { super_types, generics, form, fields, methods, nested }),
    }
}

/// Classify a class's form from **syntax alone**: base-class names and
/// decorator names, exactly as the mission brief specifies ("syntactic via
/// decorator and base-class names"). Checked in an order that prefers the
/// more specific stdlib marker over a same-named user base class collision
/// where plausible (e.g. `Enum` before `Protocol` before a generic
/// `@dataclass` decorator, since a class can be decorated `@dataclass` while
/// also subclassing `Enum`/`Protocol`/`TypedDict`, and the base-class shape
/// is the stronger signal in that case).
fn classify_form(base_names: &[String], decorators: &[String]) -> ClassForm {
    let base_last: Vec<&str> = base_names.iter().map(|b| last_segment(b)).collect();

    if base_last
        .iter()
        .any(|b| matches!(*b, "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag" | "ReprEnum"))
    {
        return ClassForm::Enum;
    }
    if base_last.iter().any(|b| *b == "Protocol") {
        return ClassForm::Protocol;
    }
    if base_last.iter().any(|b| *b == "TypedDict") {
        return ClassForm::TypedDict;
    }
    if base_last.iter().any(|b| *b == "NamedTuple") {
        return ClassForm::NamedTuple;
    }
    if decorators.iter().any(|d| last_segment(d) == "dataclass") {
        return ClassForm::Dataclass;
    }
    ClassForm::Plain
}

fn extract_fields(
    body: &[Stmt],
    source: &str,
    typevars: &HashSet<String>,
    doc: Option<&ParsedDocstring>,
    // Names already claimed by a method or nested class in this same class
    // body. A field whose name collides with one of these is a rebinding of
    // that method/class (a decorator-application idiom — see the call site's
    // doc comment), not a second, competing declaration; skip it rather than
    // colliding on `PythonId`.
    taken: &HashSet<String>,
) -> Vec<FieldData> {
    let mut fields = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for stmt in body {
        match stmt {
            Stmt::AnnAssign(a) => {
                let Expr::Name(target) = a.target.as_ref() else { continue };
                let name = target.id.as_str().to_owned();
                if taken.contains(&name) || !seen.insert(name.clone()) {
                    continue;
                }
                let (ty, is_class_var, is_final) = annotation_to_field_type(&a.annotation, source, typevars);
                fields.push(FieldData {
                    documentation: doc.and_then(|d| d.params.get(&name).cloned()),
                    name,
                    ty,
                    is_class_var,
                    is_final,
                    is_property: false,
                    has_default: a.value.is_some(),
                });
            }
            Stmt::Assign(asg) => {
                let [Expr::Name(target)] = asg.targets.as_slice() else { continue };
                let name = target.id.as_str().to_owned();
                if name.starts_with("__") && name.ends_with("__") {
                    // Dunder class attributes (`__slots__`, `__all__`, …) are
                    // interpreter/tooling metadata, not public field surface.
                    continue;
                }
                if taken.contains(&name) || !seen.insert(name.clone()) {
                    continue;
                }
                fields.push(FieldData {
                    documentation: doc.and_then(|d| d.params.get(&name).cloned()),
                    name,
                    ty: None,
                    is_class_var: false,
                    is_final: false,
                    is_property: false,
                    has_default: true,
                });
            }
            _ => {}
        }
    }

    fields
}

/// Unwrap `ClassVar[T]` / `Final[T]` / bare `Final` from a class-body
/// annotation, since neither is a real member of Python's type algebra —
/// both are qualifiers `oracle::FieldData` models as flags rather than
/// nested `TypeData`.
fn annotation_to_field_type(ann: &Expr, source: &str, typevars: &HashSet<String>) -> (Option<TypeData>, bool, bool) {
    if let Expr::Subscript(sub) = ann {
        let base_full = dotted_name(&sub.value, source);
        match last_segment(&base_full) {
            "ClassVar" => return (Some(expr_to_type(&sub.slice, source, typevars)), true, false),
            "Final" => return (Some(expr_to_type(&sub.slice, source, typevars)), false, true),
            _ => {}
        }
    }
    if last_segment(&dotted_name(ann, source)) == "Final" && matches!(ann, Expr::Name(_) | Expr::Attribute(_)) {
        return (None, false, true);
    }
    (Some(expr_to_type(ann, source, typevars)), false, false)
}

// ---------------------------------------------------------------------------
// Module-level bindings: Const / Alias
// ---------------------------------------------------------------------------

fn binding_item(stmt: &Stmt, parent: &str, source: &str, typevars: &HashSet<String>) -> Option<ItemData> {
    match stmt {
        // PEP 695 `type X = ...`.
        Stmt::TypeAlias(ta) => {
            let Expr::Name(name_expr) = ta.name.as_ref() else { return None };
            let name = name_expr.id.as_str().to_owned();
            let generics = pep695_generics(ta.type_params.as_deref(), source, typevars);
            let target = Some(expr_to_type(&ta.value, source, typevars));
            Some(plain_item(
                parent,
                name,
                ItemBody::Alias(AliasData { target, generics }),
            ))
        }
        Stmt::AnnAssign(a) => {
            let Expr::Name(target) = a.target.as_ref() else { return None };
            let name = target.id.as_str().to_owned();
            if name.starts_with("__") && name.ends_with("__") {
                return None;
            }
            // Legacy `X: TypeAlias = <expr>`.
            if last_segment(&dotted_name(&a.annotation, source)) == "TypeAlias" {
                let target = a.value.as_deref().map(|v| expr_to_type(v, source, typevars));
                return Some(plain_item(
                    parent,
                    name,
                    ItemBody::Alias(AliasData { target, generics: Vec::new() }),
                ));
            }
            let ty = Some(expr_to_type(&a.annotation, source, typevars));
            let value = a.value.as_deref().map(|v| source[v.range()].to_owned());
            Some(plain_item(parent, name, ItemBody::Const(ConstData { ty, value })))
        }
        Stmt::Assign(asg) => {
            let [Expr::Name(target)] = asg.targets.as_slice() else { return None };
            let name = target.id.as_str().to_owned();
            if name.starts_with("__") && name.ends_with("__") {
                return None;
            }
            let value = Some(source[asg.value.range()].to_owned());
            Some(plain_item(parent, name, ItemBody::Const(ConstData { ty: None, value })))
        }
        _ => None,
    }
}

fn plain_item(parent: &str, name: String, body: ItemBody) -> ItemData {
    ItemData {
        id: PythonId::new(format!("{parent}.{name}")),
        parent: Some(PythonId::new(parent.to_owned())),
        is_private: name.starts_with('_'),
        name,
        documentation: None,
        deprecation: None,
        decorators: Vec::new(),
        body,
    }
}

// ---------------------------------------------------------------------------
// Decorators
// ---------------------------------------------------------------------------

fn decorator_tokens(list: &[Decorator], source: &str) -> Vec<String> {
    list.iter().map(|d| decorator_head_name(&d.expression, source)).collect()
}

/// The decorator's callee name, ignoring any call arguments: `@dataclass`
/// and `@dataclass(frozen=True)` both yield `"dataclass"`; `@app.route(...)`
/// yields `"app.route"`.
fn decorator_head_name(e: &Expr, source: &str) -> String {
    match e {
        Expr::Call(c) => decorator_head_name(&c.func, source),
        _ => dotted_name(e, source),
    }
}

/// PEP 702 `@deprecated("message")` / `@warnings.deprecated(...)` is
/// syntactically self-describing — decorator name plus an optional leading
/// string argument — so it is recognized without any semantic analysis.
fn deprecation_from_decorators(list: &[Decorator], source: &str) -> Option<DeprecationData> {
    list.iter().find_map(|d| {
        let head = decorator_head_name(&d.expression, source);
        if last_segment(&head) != "deprecated" {
            return None;
        }
        let note = if let Expr::Call(c) = &d.expression {
            c.arguments.args.first().and_then(|a| match a {
                Expr::StringLiteral(s) => Some(s.value.to_str().to_owned()),
                _ => None,
            })
        } else {
            None
        };
        Some(DeprecationData { since: None, note })
    })
}

// ---------------------------------------------------------------------------
// Generics
// ---------------------------------------------------------------------------

fn pep695_names(tp: Option<&TypeParams>) -> Vec<String> {
    let Some(tp) = tp else { return Vec::new() };
    tp.iter()
        .map(|t| match t {
            TypeParam::TypeVar(v) => v.name.as_str().to_owned(),
            TypeParam::TypeVarTuple(v) => v.name.as_str().to_owned(),
            TypeParam::ParamSpec(v) => v.name.as_str().to_owned(),
        })
        .collect()
}

fn pep695_generics(tp: Option<&TypeParams>, source: &str, typevars: &HashSet<String>) -> Vec<GenericParamData> {
    let Some(tp) = tp else { return Vec::new() };
    tp.iter()
        .map(|t| match t {
            TypeParam::TypeVar(v) => GenericParamData {
                name: v.name.as_str().to_owned(),
                bound: v.bound.as_deref().map(|b| expr_to_type(b, source, typevars)),
                constraints: Vec::new(),
                default: v.default.as_deref().map(|d| expr_to_type(d, source, typevars)),
            },
            TypeParam::TypeVarTuple(v) => GenericParamData {
                name: v.name.as_str().to_owned(),
                bound: None,
                constraints: Vec::new(),
                default: v.default.as_deref().map(|d| expr_to_type(d, source, typevars)),
            },
            TypeParam::ParamSpec(v) => GenericParamData {
                name: v.name.as_str().to_owned(),
                bound: None,
                constraints: Vec::new(),
                default: v.default.as_deref().map(|d| expr_to_type(d, source, typevars)),
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Type expressions
// ---------------------------------------------------------------------------

fn last_segment(dotted: &str) -> &str {
    dotted.rsplit('.').next().unwrap_or(dotted)
}

/// Render a `Name`/`Attribute` chain as a dotted string (`"typing.Optional"`,
/// `"Foo"`). For any other expression kind (used rarely in type position —
/// e.g. a subscripted decorator base) falls back to the raw source text.
fn dotted_name(e: &Expr, source: &str) -> String {
    match e {
        Expr::Name(n) => n.id.as_str().to_owned(),
        Expr::Attribute(a) => format!("{}.{}", dotted_name(&a.value, source), a.attr.as_str()),
        other => source[other.range()].to_owned(),
    }
}

fn tuple_or_single(e: &Expr, source: &str, typevars: &HashSet<String>) -> Vec<TypeData> {
    match e {
        Expr::Tuple(t) => t.elts.iter().map(|el| expr_to_type(el, source, typevars)).collect(),
        other => vec![expr_to_type(other, source, typevars)],
    }
}

/// Lower one written type expression into [`TypeData`]. See this file's
/// module doc for what is deliberately not attempted (cross-file
/// resolution, deep forward-reference re-parsing, control-flow-gated
/// declarations).
fn expr_to_type(e: &Expr, source: &str, typevars: &HashSet<String>) -> TypeData {
    match e {
        Expr::NoneLiteral(_) => TypeData::NoneType,
        Expr::EllipsisLiteral(_) => TypeData::Any,

        Expr::Name(n) => {
            let id = n.id.as_str();
            match id {
                "Self" => TypeData::SelfType,
                "Any" => TypeData::Any,
                "None" => TypeData::NoneType,
                "NoReturn" | "Never" => TypeData::Never,
                _ if typevars.contains(id) => TypeData::TypeVar(id.to_owned()),
                _ => TypeData::Nominal(id.to_owned()),
            }
        }

        Expr::Attribute(_) => {
            let full = dotted_name(e, source);
            match last_segment(&full) {
                "Any" => TypeData::Any,
                "NoReturn" | "Never" => TypeData::Never,
                "Self" => TypeData::SelfType,
                _ => TypeData::Nominal(full),
            }
        }

        // A forward reference: `x: "Foo"`. Recorded as a nominal by its raw
        // text — see the module doc's "What this cannot do".
        Expr::StringLiteral(s) => TypeData::Nominal(s.value.to_str().to_owned()),

        Expr::Subscript(sub) => subscript_to_type(sub, source, typevars),

        // PEP 604 `A | B`.
        Expr::BinOp(b) if b.op == Operator::BitOr => {
            let mut members = Vec::new();
            flatten_union(e, source, typevars, &mut members);
            TypeData::Union(members)
        }

        Expr::Tuple(t) => TypeData::Tuple(t.elts.iter().map(|el| expr_to_type(el, source, typevars)).collect()),
        // A bracketed list only appears in type position as `Callable`'s
        // parameter list (`Callable[[int, str], R]`); there is no dedicated
        // "list of types" `TypeData` variant, so it is approximated as a
        // `Tuple` — order and arity survive, the "this is a param list, not
        // a value tuple" distinction does not.
        Expr::List(l) => TypeData::Tuple(l.elts.iter().map(|el| expr_to_type(el, source, typevars)).collect()),

        // Every `Expr` shape this front end has no case for: a lambda, a
        // call, a comparison, a bool-op, an f-string, … This is the
        // extractor's own gap, not a source-written `Any` — see
        // `TypeData::Unsupported` and `types::lower_type`'s handling of it.
        // The raw source text is what lets a later reader (or a census) name
        // exactly which construct is still missing.
        _ => TypeData::Unsupported(source[e.range()].to_owned()),
    }
}

fn flatten_union(e: &Expr, source: &str, typevars: &HashSet<String>, out: &mut Vec<TypeData>) {
    if let Expr::BinOp(b) = e
        && b.op == Operator::BitOr
    {
        flatten_union(&b.left, source, typevars, out);
        flatten_union(&b.right, source, typevars, out);
        return;
    }
    out.push(expr_to_type(e, source, typevars));
}

fn subscript_to_type(sub: &ExprSubscript, source: &str, typevars: &HashSet<String>) -> TypeData {
    let base_full = dotted_name(&sub.value, source);
    match last_segment(&base_full) {
        "Optional" => {
            let inner = expr_to_type(&sub.slice, source, typevars);
            TypeData::Union(vec![inner, TypeData::NoneType])
        }
        "Union" => TypeData::Union(tuple_or_single(&sub.slice, source, typevars)),
        "Annotated" => annotated_type(&sub.slice, source, typevars),
        // Qualifiers, not real type constructors: unwrap to the inner type.
        // `ClassVar`/`Final` on a *field* annotation are additionally caught
        // by `annotation_to_field_type` so the flag survives; this arm is
        // what a `ClassVar[T]`/`Final[T]` reached through a non-field
        // position (a nested generic argument, say) degrades to.
        "ClassVar" | "Final" => expr_to_type(&sub.slice, source, typevars),
        // A `Literal[...]` value set has no type-algebra representation
        // here (its members are values, not types). This is a real, common
        // PEP 586 construct the author deliberately wrote — the opposite of
        // asking for dynamic typing — so it is `Unsupported`/
        // `NoIrRepresentation`, not `Any`/`DynamicallyTyped`. The full
        // subscript text (`Literal[GET, POST]`) is kept rather than a bare
        // "Literal" tag, so two distinct literal sets stay distinguishable
        // in a census or a skeleton.
        "Literal" => TypeData::Unsupported(source[sub.range()].to_owned()),
        "Type" | "type" => TypeData::Apply {
            base: Box::new(TypeData::Nominal("type".to_owned())),
            args: vec![expr_to_type(&sub.slice, source, typevars)],
        },
        // `list[T]` / `typing.List[T]` map to the dedicated slice-like
        // variant per `types.rs`'s mapping table, not a generic `Apply`.
        //
        // `args` is empty only for a degenerate subscript this extractor
        // should never see written (`list[]` is not valid Python). That is
        // the extractor's own gap, not the source asking for `Any` — see
        // `TypeData::Unsupported`.
        "List" | "list" => {
            let mut args = tuple_or_single(&sub.slice, source, typevars);
            let elem = args
                .pop()
                .unwrap_or_else(|| TypeData::Unsupported(source[sub.range()].to_owned()));
            TypeData::Slice(Box::new(elem))
        }
        "Tuple" | "tuple" => TypeData::Tuple(tuple_or_single(&sub.slice, source, typevars)),
        "Callable" => TypeData::Apply {
            base: Box::new(TypeData::Nominal("Callable".to_owned())),
            args: tuple_or_single(&sub.slice, source, typevars),
        },
        _ => TypeData::Apply {
            base: Box::new(TypeData::Nominal(base_full)),
            args: tuple_or_single(&sub.slice, source, typevars),
        },
    }
}

fn annotated_type(slice: &Expr, source: &str, typevars: &HashSet<String>) -> TypeData {
    if let Expr::Tuple(t) = slice
        && let Some((first, rest)) = t.elts.split_first()
    {
        let inner = expr_to_type(first, source, typevars);
        let metadata = rest.iter().map(|m| source[m.range()].to_owned()).collect();
        return TypeData::Annotated { inner: Box::new(inner), metadata };
    }
    // `Annotated[T]` with no metadata is not valid Python, but degrade to
    // the inner type rather than panicking on malformed input.
    expr_to_type(slice, source, typevars)
}

#[cfg(test)]
mod tests;
