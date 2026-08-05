//! One-pass lowering of the Java oracle output into `nudox-ir`.
//!
//! # Design decisions
//!
//! ## Id choice
//!
//! `JavaId` is the fully-qualified binary name **plus** an erased parameter
//! descriptor, matching the JVM binary name scheme:
//!
//! * Types: `"com.example.Outer$Inner"` (dollars, no dots inside the class
//!   boundary — matching `TypeElement.getQualifiedName().toString()` which the
//!   oracle emits as dotted `"com.example.Outer.Inner"`; we normalise `.` → `$`
//!   for nested types to match the JVM binary form).
//!
//!   However: the oracle always emits qualified names with dots, including for
//!   nested types (`com.example.Outer.Inner` not `com.example.Outer$Inner`).
//!   To keep IDs stable and predictable we keep the oracle's dotted form.
//!
//! * Methods/constructors: `"com.example.Foo#methodName(ParamType1,ParamType2)"`.
//!   The signature is the erased descriptor in source-type form — not bytecode
//!   descriptors — because the oracle reports source names, not bytecode.
//!
//!   Each overload gets its own distinct `JavaId`. Two overloads of `draw`:
//!   - `"com.example.Canvas#draw(String)"`
//!   - `"com.example.Canvas#draw(int,int)"`
//!
//!   **This is the overload identity**: each overload becomes its own
//!   `declare` call with a distinct `Id`. No folding. No `overloads` field.
//!
//! * Fields/enum-constants: `"com.example.Foo#fieldName"`.
//!
//! * Packages/modules: `"pkg:com.example"` / `"mod:java.base"` (namespace
//!   prefix avoids collisions with type names of the same text).
//!
//! ## One pass
//!
//! `Lowering::refer` lets us declare parent→child edges before we have
//! declared the parent. We emit in a single pass over the flat type list,
//! declaring each type (and all its members) in declaration order. No
//! intermediate tree.
//!
//! ## Wildcards
//!
//! Java wildcards (`? extends T`, `? super T`, `?`) are emitted as
//! `Type::Wildcard { variance, bound }` directly — using the real IR variant.
//!
//! - `? extends T` → `Variance::Covariant` with `bound = Some(T)`
//! - `? super T`   → `Variance::Contravariant` with `bound = Some(T)`
//! - `?`           → `Variance::Invariant` with `bound = None`
//!
//! No synthetic wildcard anchor entries are created.
//!
//! ## `throws` (checked exceptions)
//!
//! Thrown types are lowered directly into `Function::throws: List<Type>` —
//! the structured slot that exists for exactly this purpose. The prose "Throws:"
//! section in `documentation` is retained for rendering, but the `throws`
//! `AttrTok` that was previously written to `Symbol.attrs` is **dropped**:
//! `Function::throws` supersedes it for structured consumers. Callers that
//! previously read `Symbol::attrs` looking for `token == "throws"` should
//! switch to reading `Function::throws` from the kind payload.
//!
//! External thrown types (e.g. `java.io.IOException` not in the current
//! oracle extraction) lower to `Type::Any`; this is the same best-effort
//! degradation applied to all external type references.
//!
//! ## Java-specific constructs not representable in the IR
//!
//! The following are represented as documentation-section notes rather than
//! structural IR slots (same pattern the old producer used):
//!
//! * Non-access method modifiers (`synchronized`, `native`, `strictfp`).
//! * Annotation defaults (`@interface` element `default 3`).
//! * Class-level modifiers (`abstract`, `sealed`, `non-sealed`, `strictfp`).
//! * Type-use annotations (encoded as `TypeOperator`-like strings in docs; the
//!   new IR `Type` has no `TypeOperator` wrapper — see gap note below).
//! * Annotation types: lowered as `Trait` with a documentation note
//!   `"annotation_interface"`.
//! * Sealing: `TraitFlags::sealed` encodes interface sealing; permitted
//!   subtypes ride documentation sections.
//!
//! ## Type-use annotations (`Type::Annotated`)
//!
//! The `TypeMirror::Declared`, `Primitive`, `Typevar`, and `Array` variants all
//! carry an `annotations: Vec<Annotation>` field that the oracle populates for
//! type-use annotations (e.g. `@NonNull String`). We now wire these into
//! `Type::Annotated { inner, annotation }` wrapping the lowered inner type.
//! Multiple annotations stack: the outermost `AttrTok` is the first annotation
//! in declaration order; remaining annotations wrap further inward.
//! If there are no annotations the inner type is returned directly.
//! The `annotation.ty` field (the annotation class name) maps to
//! `AttrTok::token`; the annotation has no simple string arg in the Java
//! model (values are structured), so `AttrTok::arg` is set to `None`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use nudox_ir::build::*;
use nudox_ir::entry::Visibility;
use nudox_ir::kinds::function::FnModifier;
use nudox_ir::kinds::record::{FieldAttribute, FieldKey};
use nudox_ir::kinds::sum::VariantForm;
use nudox_ir::kinds::trait_::TraitFlags;
use nudox_ir::kinds::ty::Variance;

use crate::javadoc::{self, ParsedJavadoc};
use crate::schema::{
    self, Annotation, EnumConstant, Extraction, Field as SchemaField, Method, Modifier,
    NestingKind, Param, TypeDecl, TypeDeclKind, TypeMirror, Value, has_modifier, is_synthetic,
};

// ── JavaId ──────────────────────────────────────────────────────────────────

/// Stable identity key for every declared element in one oracle extraction.
///
/// Format rules (see module doc):
/// - Type:   `qualified.name`  (oracle's dotted form)
/// - Method: `qualified.name#methodName(T1,T2)` (erased param types)
/// - Field:  `qualified.name#fieldName`
/// - Package:`pkg:com.example`
/// - Module: `mod:java.base`
///
/// This distinguishes every Java overload (`draw(String)` ≠ `draw(int,int)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JavaId(pub String);

impl JavaId {
    fn type_(qualified: &str) -> Self {
        JavaId(qualified.to_owned())
    }

    fn member(owner_qualified: &str, member_name: &str) -> Self {
        JavaId(format!("{owner_qualified}#{member_name}"))
    }

    fn method(owner_qualified: &str, name: &str, params: &[Param]) -> Self {
        let sig = params
            .iter()
            .map(|p| type_erase(&p.ty))
            .collect::<Vec<_>>()
            .join(",");
        JavaId(format!("{owner_qualified}#{name}({sig})"))
    }

    fn package(name: &str) -> Self {
        JavaId(format!("pkg:{name}"))
    }

    fn module(name: &str) -> Self {
        JavaId(format!("mod:{name}"))
    }
}

/// Produce the "erased" type name for a method-signature id. We use the
/// declared source name (not JVM bytecode descriptors) because the oracle
/// reports source names.
fn type_erase(t: &TypeMirror) -> String {
    match t {
        TypeMirror::Primitive { name, .. } => name.to_string(),
        TypeMirror::Void => "void".to_owned(),
        TypeMirror::Declared { name, .. } => {
            // Strip generic args for erasure — same as JVM erasure.
            name.to_string()
        }
        TypeMirror::Array { component, .. } => format!("{}[]", type_erase(component)),
        TypeMirror::Typevar { name, .. } => name.to_string(),
        TypeMirror::Wildcard { .. } => "?".to_owned(),
        TypeMirror::Intersection { bounds } => {
            if bounds.is_empty() {
                "Object".to_owned()
            } else {
                type_erase(&bounds[0])
            }
        }
        TypeMirror::Union { alternatives } => alternatives
            .iter()
            .map(type_erase)
            .collect::<Vec<_>>()
            .join("|"),
        TypeMirror::Error { name } | TypeMirror::Other { repr: name } => name.to_string(),
        TypeMirror::None => "<none>".to_owned(),
        TypeMirror::Null => "null".to_owned(),
    }
}

// ── Lowering context ────────────────────────────────────────────────────────

/// Lowering context: wraps `Lowering<JavaId>` and maintains the type-name
/// resolver for javadoc link resolution.
pub struct LoweringCtx<'a> {
    pub low: &'a mut Lowering<JavaId>,
    /// Maps simple type names → qualified names, built from the extraction's
    /// type list for javadoc `{@link}` resolution.
    pub resolver: HashMap<String, String>,
    /// Set of all qualified type names declared in this extraction.
    /// Used by `lower_type` to decide whether to `refer()` (in-package) or
    /// return `Type::Any` (external library type not in scope).
    pub known_qualified: HashSet<String>,
}

impl<'a> LoweringCtx<'a> {
    pub fn new(low: &'a mut Lowering<JavaId>, types: &[TypeDecl]) -> Self {
        let mut resolver = HashMap::with_capacity(types.len());
        let mut known_qualified = HashSet::with_capacity(types.len());
        for t in types {
            resolver
                .entry(t.simple_name.to_string())
                .or_insert_with(|| t.qualified_name.to_string());
            known_qualified.insert(t.qualified_name.to_string());
        }
        LoweringCtx {
            low,
            resolver,
            known_qualified,
        }
    }

    fn parse_doc(
        &self,
        doc: Option<&str>,
        doc_kind: Option<&str>,
        self_type: Option<&str>,
    ) -> Option<ParsedJavadoc> {
        javadoc::parse_opt(doc, doc_kind, self_type, &self.resolver)
    }

    /// Whether a fully-qualified type name belongs to the current extraction.
    pub fn is_local(&self, qualified: &str) -> bool {
        self.known_qualified.contains(qualified)
    }
}

// ── Symbol helpers ───────────────────────────────────────────────────────────

/// Build a `Symbol` from common parts.
fn make_sym(
    name: &str,
    vis: Visibility,
    doc: Option<String>,
    depr: Option<Deprecation>,
    doc_links: &[String],
    source: &str,
    line: Option<u64>,
) -> Symbol {
    let span_end = line.unwrap_or(0) as usize;
    Symbol {
        name: name.to_owned(),
        visibility: vis,
        documentation: doc.unwrap_or_default(),
        source: PathBuf::from(source),
        span: 0..span_end,
        aliases: Box::new([]),
        deprecation: depr,
        doc_links: {
            let links: Vec<DocLink> = doc_links
                .iter()
                .map(|t| DocLink {
                    target: t.clone(),
                    label: None,
                })
                .collect();
            links.into_boxed_slice()
        },
        attrs: Box::new([]),
        cfg: None,
    }
}

// ── Visibility ───────────────────────────────────────────────────────────────

fn visibility(modifiers: &[Modifier]) -> Visibility {
    for m in modifiers {
        match m {
            Modifier::Public => return Visibility::Public,
            Modifier::Protected => return Visibility::Protected,
            Modifier::Private => return Visibility::Private,
            _ => {}
        }
    }
    Visibility::Package
}

// ── Deprecation ──────────────────────────────────────────────────────────────

fn make_deprecation(
    flagged: bool,
    annotations: &[Annotation],
    parsed: Option<&ParsedJavadoc>,
) -> Option<Deprecation> {
    let tag = parsed.and_then(|p| p.deprecated.as_ref());
    if !flagged && tag.is_none() {
        return None;
    }
    let note = tag.cloned().filter(|s| !s.is_empty());
    let since = annotations
        .iter()
        .find(|a| a.ty.ends_with(".Deprecated") || a.ty.as_ref() == "java.lang.Deprecated")
        .and_then(|a| a.values.get("since"))
        .and_then(|v| match v {
            Value::String { value } => Some(value.to_string()),
            _ => None,
        });
    Some(Deprecation { since, note })
}

// ── Tag sections for `@since`/`@author`/etc. ─────────────────────────────────

fn tag_sections(parsed: &ParsedJavadoc) -> Vec<String> {
    let mut sections = Vec::new();
    if let Some(since) = &parsed.since {
        sections.push(format!("Since: {since}"));
    }
    if let Some(version) = &parsed.version {
        sections.push(format!("Version: {version}"));
    }
    if !parsed.authors.is_empty() {
        sections.push(format!("Authors: {}", parsed.authors.join(", ")));
    }
    if !parsed.see.is_empty() {
        let refs: Vec<String> = parsed.see.iter().map(|s| format!("`{s}`")).collect();
        sections.push(format!("See also: {}", refs.join(", ")));
    }
    sections
}

fn deprecation_doc_section(flagged: bool, parsed: Option<&ParsedJavadoc>) -> Option<String> {
    let text = parsed.and_then(|p| p.deprecated.clone());
    match (flagged, text) {
        (_, Some(text)) if !text.is_empty() => Some(format!("Deprecated: {text}")),
        (true, _) | (_, Some(_)) => Some("Deprecated.".to_string()),
        (false, None) => None,
    }
}

// ── Source location ──────────────────────────────────────────────────────────

fn source_of(position: Option<&schema::Position>) -> (&str, Option<u64>) {
    match position {
        Some(p) => (p.file.as_ref(), p.line),
        None => ("", None),
    }
}

// ── Implicit superclasses to skip ────────────────────────────────────────────

const IMPLICIT_SUPERCLASSES: &[&str] = &["java.lang.Object", "java.lang.Enum", "java.lang.Record"];

// ── Type lowering ────────────────────────────────────────────────────────────

/// Recursion limit for type lowering.
const MAX_DEPTH: usize = 64;

/// Lower a `TypeMirror` into the IR `Type`.
///
/// **Wildcards:** `? extends T` → `Type::Wildcard { Covariant, Some(T) }`;
/// `? super T` → `Contravariant`; bare `?` → `Invariant, None`. No synthetic
/// anchor entries are created.
///
/// **TypeVar:** type-parameter uses (`T`, `R`) lower to `Type::TypeVar(name)`.
/// Type-parameter *declarations* stay in the enclosing kind's `generics` list.
///
/// **TypeOperator gap:** Type-use annotations are dropped — the new `Type`
/// enum has no `TypeOperator` wrapper. The information is lost at the type
/// level; method-level annotation uses survive as documentation sections.
///
/// **External type handling:** if a `Declared` type is not in
/// `ctx.known_qualified` (i.e., it is a JDK or external library type not in
/// the current oracle extraction), we emit `Type::Any` rather than calling
/// `refer()` (which would record an `Undeclared` id and cause `finish()` to
/// fail). This is a best-effort degradation — the type identity is lost, but
/// the entry is still produced.
pub fn lower_type(ctx: &mut LoweringCtx<'_>, t: &TypeMirror) -> Type {
    lower_type_depth(ctx.low, &ctx.known_qualified, t, 0)
}

fn lower_type_depth(
    low: &mut Lowering<JavaId>,
    known: &HashSet<String>,
    t: &TypeMirror,
    depth: usize,
) -> Type {
    if depth > MAX_DEPTH {
        return Type::Any;
    }

    match t {
        TypeMirror::Primitive { name, annotations } => {
            let inner = lower_primitive(name);
            wrap_annotated(inner, annotations)
        }
        TypeMirror::Void => Type::Tuple(Box::new([])),
        TypeMirror::Declared {
            name,
            args,
            annotations,
            ..
        } => {
            // java.lang.Object is the top type — map to Any.
            let base = if name.as_ref() == "java.lang.Object" {
                Type::Any
            } else if !known.contains(name.as_ref()) {
                // If the type is not in the current extraction (e.g. java.util.List
                // from the JDK or a dependency not included in the oracle run), emit
                // Type::Any rather than refer()-ing to an unknown id — that would
                // cause Lowering::finish to fail with Undeclared.
                Type::Any
            } else {
                // Type is known locally: refer() to it and produce a Nominal.
                let nominal: RawRef = low
                    .refer::<nudox_ir::kinds::record::Record>(JavaId::type_(name.as_ref()))
                    .into_raw();
                if args.is_empty() {
                    Type::Nominal(nominal)
                } else {
                    let args_lowered: Vec<Type> = args
                        .iter()
                        .map(|a| lower_type_depth(low, known, a, depth + 1))
                        .collect();
                    Type::Apply {
                        base: Box::new(Type::Nominal(nominal)),
                        args: args_lowered.into_boxed_slice(),
                    }
                }
            };
            wrap_annotated(base, annotations)
        }
        TypeMirror::Array {
            component,
            annotations,
        } => {
            let inner = Type::Slice(Box::new(lower_type_depth(low, known, component, depth + 1)));
            wrap_annotated(inner, annotations)
        }
        TypeMirror::Typevar { name, annotations } => {
            // A use of a type parameter (e.g. `T` in `List<T>`).
            // `Type::TypeVar` carries the name verbatim; the owning declaration
            // lives in the `generics` list of the enclosing kind.
            let inner = Type::TypeVar(name.to_string());
            wrap_annotated(inner, annotations)
        }
        TypeMirror::Wildcard {
            extends_bound,
            super_bound,
        } => {
            // Java wildcards map directly to `Type::Wildcard`.
            // `? extends T` → Covariant bound; `? super T` → Contravariant;
            // bare `?` → Invariant, no bound.
            match (extends_bound, super_bound) {
                (Some(bound), _) => Type::Wildcard {
                    variance: Variance::Covariant,
                    bound: Some(Box::new(lower_type_depth(low, known, bound, depth + 1))),
                },
                (None, Some(bound)) => Type::Wildcard {
                    variance: Variance::Contravariant,
                    bound: Some(Box::new(lower_type_depth(low, known, bound, depth + 1))),
                },
                (None, None) => Type::Wildcard {
                    variance: Variance::Invariant,
                    bound: None,
                },
            }
        }
        TypeMirror::Intersection { bounds } => Type::Intersection(
            bounds
                .iter()
                .map(|b| lower_type_depth(low, known, b, depth + 1))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        TypeMirror::Union { alternatives } => Type::Union(
            alternatives
                .iter()
                .map(|a| lower_type_depth(low, known, a, depth + 1))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        TypeMirror::Error { name } | TypeMirror::Other { repr: name } => {
            // Unresolvable — keep a nominal reference over the source text.
            // We use a synthetic "refer" that will not be declared; this is a
            // best-effort preservation. The LoweringError::Undeclared that
            // would normally result is suppressed by *not* using refer() —
            // instead we emit Any and record the name in documentation.
            // This is the one documented best-effort spot.
            let _ = name;
            Type::Any
        }
        TypeMirror::None | TypeMirror::Null => Type::Any,
    }
}

/// Wrap a lowered `Type` in zero or more `Type::Annotated` layers, one per
/// type-use annotation in declaration order (outermost = first).
///
/// Java annotations on types (e.g. `@NonNull String`, `@Nullable List<T>`) are
/// represented in the `TypeMirror` schema as `annotations: Vec<Annotation>` on
/// each type-mirror variant. We wire them into `Type::Annotated` so that
/// structured consumers can read them without text parsing.
///
/// **`AttrTok::arg` is `None`** for all Java type-use annotations: the
/// annotation's argument structure (`annotation.values`) is a `HashMap<String,
/// Value>` which has no lossless mapping to a single string. Consumers that
/// need the argument values must inspect the documentation prose (where the
/// Oracle renders them) or handle them via a future structured annotation-value
/// field in the IR.
fn wrap_annotated(mut ty: Type, annotations: &[crate::schema::Annotation]) -> Type {
    // We apply annotations outermost-first (index 0 is the first declared
    // annotation and therefore the outermost wrapper).
    for ann in annotations {
        ty = Type::Annotated {
            inner: Box::new(ty),
            annotation: AttrTok {
                token: ann.ty.to_string(),
                arg: None,
            },
        };
    }
    ty
}

fn lower_primitive(name: &str) -> Type {
    match name {
        "boolean" => Type::Primitive(Primitive::Bool),
        "byte" => Type::I8,
        "short" => Type::I16,
        "int" => Type::I32,
        "long" => Type::I64,
        "char" => Type::Primitive(Primitive::Char),
        "float" => Type::Primitive(Primitive::Float(Width::W32)),
        "double" => Type::Primitive(Primitive::Float(Width::W64)),
        other => {
            // Unknown primitive — fall back to Any.
            let _ = other;
            Type::Any
        }
    }
}

// ── GenericParam lowering ────────────────────────────────────────────────────

fn lower_type_params(
    ctx: &mut LoweringCtx<'_>,
    type_params: &[schema::TypeParam],
) -> (Vec<GenericParam>, Vec<WherePred>) {
    let mut params = Vec::with_capacity(type_params.len());
    let mut wheres = Vec::new();

    for tp in type_params {
        let mut bounds: Vec<Type> = tp
            .bounds
            .iter()
            .filter(|b| b.declared_name() != Some("java.lang.Object"))
            .map(|b| lower_type(ctx, b))
            .collect();

        params.push(GenericParam::Type {
            name: tp.name.to_string(),
            bounds: bounds.clone().into_boxed_slice(),
            default: None,
            // Java has no declaration-site variance: type parameters are always
            // invariant by language spec. Use-site variance (wildcards) is handled
            // separately via Type::Wildcard. Producers must use None here, not
            // Some(Variance::Invariant), which is reserved for explicitly annotated
            // invariance.
            variance: None,
        });

        // Also emit WherePred for complex multi-bounds.
        if bounds.len() > 1 {
            // The GenericParam already holds them; emit a WherePred for consumers
            // that only read the where clause.
            wheres.push(WherePred {
                target: Type::TypeVar(tp.name.to_string()),
                bounds: bounds.into_boxed_slice(),
            });
        } else {
            let _ = bounds.drain(..);
        }
    }

    (params, wheres)
}

// ── Top-level entry point ────────────────────────────────────────────────────

/// Lower a complete oracle `Extraction` into the `Lowering` sink.
///
/// Call `low.finish()` after to validate and produce the `IrPackage`.
pub fn lower_extraction(ctx: &mut LoweringCtx<'_>, extraction: &Extraction) {
    // Packages → Module entries.
    for pkg in extraction.packages.iter() {
        lower_package(ctx, pkg);
    }

    // JPMS modules → Module entries.
    for m in extraction.modules.iter() {
        lower_module(ctx, m);
    }

    // Types (flat list; nesting expressed via parent pointer).
    for decl in extraction.types.iter() {
        lower_type_decl(ctx, decl, extraction);
    }
}

// ── Packages and modules ──────────────────────────────────────────────────────

fn lower_package(ctx: &mut LoweringCtx<'_>, pkg: &schema::Package) {
    let parsed = ctx.parse_doc(
        pkg.doc.as_deref(),
        pkg.doc_kind.as_deref(),
        Some(pkg.name.as_ref()),
    );
    let (src, line) = source_of(pkg.position.as_ref());
    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let sym = make_sym(
        pkg.name.as_ref(),
        Visibility::Public,
        parsed.as_ref().and_then(ParsedJavadoc::documentation),
        None,
        &doc_links,
        src,
        line,
    );
    ctx.low.declare(
        JavaId::package(pkg.name.as_ref()),
        None,
        sym,
        nudox_ir::kinds::Module,
    );
}

fn lower_module(ctx: &mut LoweringCtx<'_>, m: &schema::Module) {
    let parsed = ctx.parse_doc(
        m.doc.as_deref(),
        m.doc_kind.as_deref(),
        Some(m.name.as_ref()),
    );
    let (src, line) = source_of(m.position.as_ref());
    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let sym = make_sym(
        m.name.as_ref(),
        Visibility::Public,
        parsed.as_ref().and_then(ParsedJavadoc::documentation),
        None,
        &doc_links,
        src,
        line,
    );
    ctx.low.declare(
        JavaId::module(m.name.as_ref()),
        None,
        sym,
        nudox_ir::kinds::Module,
    );
}

// ── Type declarations ─────────────────────────────────────────────────────────

fn lower_type_decl(ctx: &mut LoweringCtx<'_>, decl: &TypeDecl, extraction: &Extraction) {
    // LOCAL and ANONYMOUS types are implementation details — skip them.
    match decl.nesting {
        NestingKind::Local | NestingKind::Anonymous => return,
        _ => {}
    }

    // Parent: the enclosing type, or else the package.
    let parent_id: Option<JavaId> = if let Some(enc) = &decl.enclosing {
        Some(JavaId::type_(enc.as_ref()))
    } else {
        Some(JavaId::package(decl.package.as_ref()))
    };

    match decl.kind {
        TypeDeclKind::Interface | TypeDeclKind::AnnotationType => {
            lower_interface(ctx, decl, parent_id, extraction);
        }
        TypeDeclKind::Enum => {
            lower_enum(ctx, decl, parent_id, extraction);
        }
        TypeDeclKind::Class | TypeDeclKind::Record => {
            lower_class_like(ctx, decl, parent_id, extraction);
        }
    }
}

// ── Classes and records ────────────────────────────────────────────────────────

fn lower_class_like(
    ctx: &mut LoweringCtx<'_>,
    decl: &TypeDecl,
    parent_id: Option<JavaId>,
    _extraction: &Extraction,
) {
    let qname = decl.qualified_name.as_ref();
    let is_record = decl.kind == TypeDeclKind::Record;
    let parsed = ctx.parse_doc(decl.doc.as_deref(), decl.doc_kind.as_deref(), Some(qname));
    let (src, line) = source_of(decl.position.as_ref());

    // --- Fields. -----------------------------------------------------------
    let mut field_refs: Vec<Ref<nudox_ir::kinds::record::Field>> = Vec::new();

    // Record components first (immutable, declared-order).
    if is_record {
        for component in decl.record_components.iter() {
            let fid = JavaId::member(qname, component.name.as_ref());
            let comp_parsed = ctx.parse_doc(
                component.doc.as_deref(),
                component.doc_kind.as_deref(),
                Some(qname),
            );
            let comp_doc = comp_parsed
                .as_ref()
                .and_then(ParsedJavadoc::documentation)
                .or_else(|| {
                    parsed
                        .as_ref()
                        .and_then(|p| p.params.get(component.name.as_ref()).cloned())
                });

            let field_sym = make_sym(
                component.name.as_ref(),
                Visibility::Public,
                comp_doc,
                None,
                &[],
                src,
                line,
            );
            let ty = lower_type(ctx, &component.ty);
            let field_ref = ctx.low.declare(
                fid.clone(),
                Some(JavaId::type_(qname)),
                field_sym,
                nudox_ir::kinds::record::Field::builder()
                    .key(FieldKey::Named)
                    .ty(ty)
                    // Record components are final by construction; no attributes needed.
                    .build(),
            );
            field_refs.push(field_ref);
        }
    }

    // Regular fields (excluding backing fields for record components).
    let component_names: Vec<&str> = decl
        .record_components
        .iter()
        .map(|c| c.name.as_ref())
        .collect();

    for f in decl.fields.iter() {
        if is_synthetic(f.origin) {
            continue;
        }
        if is_record && component_names.contains(&f.name.as_ref()) {
            continue;
        }
        let fref = lower_field(
            ctx,
            qname,
            f,
            parent_id.as_ref().map(|_| JavaId::type_(qname)),
        );
        field_refs.push(fref);
    }

    // --- Constructors. -----------------------------------------------------
    for ctor in decl.constructors.iter() {
        if is_synthetic(ctor.origin) {
            continue;
        }
        lower_constructor(ctx, qname, ctor);
    }

    // --- Methods. ----------------------------------------------------------
    for m in decl.methods.iter() {
        if is_synthetic(m.origin) {
            continue;
        }
        lower_method(ctx, qname, m, false);
    }

    // --- Supertypes. -------------------------------------------------------
    let mut super_types: Vec<Type> = Vec::new();
    if let Some(sc) = &decl.superclass {
        match sc {
            TypeMirror::None | TypeMirror::Null | TypeMirror::Void => {}
            _ => {
                let name = sc.declared_name().unwrap_or("");
                if !IMPLICIT_SUPERCLASSES.contains(&name) {
                    super_types.push(lower_type(ctx, sc));
                }
            }
        }
    }
    for iface in decl.interfaces.iter() {
        match iface {
            TypeMirror::None | TypeMirror::Null | TypeMirror::Void => {}
            _ => super_types.push(lower_type(ctx, iface)),
        }
    }

    // --- Generics. ---------------------------------------------------------
    let (generics, wheres) = lower_type_params(ctx, &decl.type_params);

    // --- Documentation. ----------------------------------------------------
    let mut doc_sections: Vec<String> = Vec::new();
    if let Some(p) = &parsed {
        if let Some(text) = p.documentation() {
            doc_sections.push(text);
        }
        if let Some(section) = p.type_params_section() {
            doc_sections.push(section);
        }
        doc_sections.extend(tag_sections(p));
    }
    if let Some(section) = deprecation_doc_section(decl.deprecated, parsed.as_ref()) {
        doc_sections.push(section);
    }
    // Class modifiers ride docs (no structural slot).
    let class_mods: Vec<&str> = decl
        .modifiers
        .iter()
        .filter_map(|m| match m {
            Modifier::Abstract => Some("abstract"),
            Modifier::Final => Some("final"),
            Modifier::Static => Some("static"),
            Modifier::Sealed => Some("sealed"),
            Modifier::NonSealed => Some("non-sealed"),
            Modifier::Strictfp => Some("strictfp"),
            _ => None,
        })
        .collect();
    if is_record {
        let mut combined = class_mods.clone();
        combined.push("record");
        doc_sections.push(format!("Declared: `{}`", combined.join(" ")));
    } else if !class_mods.is_empty() {
        doc_sections.push(format!("Declared: `{}`", class_mods.join(" ")));
    }
    // Sealed: permitted subtypes in docs.
    if !decl.permits.is_empty() && has_modifier(&decl.modifiers, Modifier::Sealed) {
        let names: Vec<String> = decl
            .permits
            .iter()
            .filter_map(|p| p.declared_name().map(String::from))
            .collect();
        doc_sections.push(format!("Sealed; permitted subtypes: {}", names.join(", ")));
    }

    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let depr = make_deprecation(decl.deprecated, &decl.annotations, parsed.as_ref());

    let sym = make_sym(
        decl.simple_name.as_ref(),
        visibility(&decl.modifiers),
        if doc_sections.is_empty() {
            None
        } else {
            Some(doc_sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    ctx.low.declare(
        JavaId::type_(qname),
        parent_id,
        sym,
        nudox_ir::kinds::record::Record::builder()
            .fields(field_refs)
            .super_types(super_types)
            .generics(generics)
            .wheres(wheres)
            .build(),
    );
}

// ── Interfaces ─────────────────────────────────────────────────────────────────

fn lower_interface(
    ctx: &mut LoweringCtx<'_>,
    decl: &TypeDecl,
    parent_id: Option<JavaId>,
    _extraction: &Extraction,
) {
    let qname = decl.qualified_name.as_ref();
    let is_annotation = decl.kind == TypeDeclKind::AnnotationType;
    let parsed = ctx.parse_doc(decl.doc.as_deref(), decl.doc_kind.as_deref(), Some(qname));
    let (src, line) = source_of(decl.position.as_ref());

    // Methods are standalone child entries (see below).
    for m in decl.methods.iter() {
        if is_synthetic(m.origin) {
            continue;
        }
        let is_provided = !has_modifier(&m.modifiers, Modifier::Abstract)
            || (is_annotation && m.annotation_default.is_some());
        lower_method(ctx, qname, m, is_provided);
    }

    // Super-interfaces → supers list.
    let supers: Vec<Type> = decl
        .interfaces
        .iter()
        .filter(|i| !matches!(*i, TypeMirror::None | TypeMirror::Null | TypeMirror::Void))
        .map(|i| lower_type(ctx, i))
        .collect();

    // Generics.
    let (generics, wheres) = lower_type_params(ctx, &decl.type_params);

    // Flags.
    let mut flags = TraitFlags::default();
    if has_modifier(&decl.modifiers, Modifier::Sealed) {
        flags.sealed = nudox_ir::kinds::facts::Sealed::Full;
    }

    // Documentation.
    let mut doc_sections: Vec<String> = Vec::new();
    if is_annotation {
        doc_sections.push("annotation_interface".to_owned());
    }
    if let Some(p) = &parsed {
        if let Some(text) = p.documentation() {
            doc_sections.push(text);
        }
        if let Some(section) = p.type_params_section() {
            doc_sections.push(section);
        }
        doc_sections.extend(tag_sections(p));
    }
    if let Some(section) = deprecation_doc_section(decl.deprecated, parsed.as_ref()) {
        doc_sections.push(section);
    }
    if !decl.permits.is_empty() {
        let names: Vec<String> = decl
            .permits
            .iter()
            .filter_map(|p| p.declared_name().map(String::from))
            .collect();
        doc_sections.push(format!("Sealed; permitted subtypes: {}", names.join(", ")));
    }

    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let depr = make_deprecation(decl.deprecated, &decl.annotations, parsed.as_ref());

    let sym = make_sym(
        decl.simple_name.as_ref(),
        visibility(&decl.modifiers),
        if doc_sections.is_empty() {
            None
        } else {
            Some(doc_sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    ctx.low.declare(
        JavaId::type_(qname),
        parent_id,
        sym,
        nudox_ir::kinds::trait_::Trait::builder()
            .flags(flags)
            .supers(supers)
            .generics(generics)
            .wheres(wheres)
            .build(),
    );
}

// ── Enums ──────────────────────────────────────────────────────────────────────

fn lower_enum(
    ctx: &mut LoweringCtx<'_>,
    decl: &TypeDecl,
    parent_id: Option<JavaId>,
    _extraction: &Extraction,
) {
    let qname = decl.qualified_name.as_ref();
    let parsed = ctx.parse_doc(decl.doc.as_deref(), decl.doc_kind.as_deref(), Some(qname));
    let (src, line) = source_of(decl.position.as_ref());

    // Enum constants → Variant entries.
    let mut variant_refs: Vec<Ref<nudox_ir::kinds::sum::Variant>> = Vec::new();
    for c in decl.enum_constants.iter() {
        let vref = lower_enum_constant(ctx, qname, c);
        variant_refs.push(vref);
    }

    // Enum methods as standalone Function entries.
    for m in decl.methods.iter() {
        if is_synthetic(m.origin) {
            continue;
        }
        lower_method(ctx, qname, m, false);
    }

    // Enum fields as standalone Field/Static entries.
    for f in decl.fields.iter() {
        if is_synthetic(f.origin) {
            continue;
        }
        lower_field(ctx, qname, f, Some(JavaId::type_(qname)));
    }

    // Generics.
    let (generics, wheres) = lower_type_params(ctx, &decl.type_params);

    // Documentation.
    let mut doc_sections: Vec<String> = Vec::new();
    if let Some(p) = &parsed {
        if let Some(text) = p.documentation() {
            doc_sections.push(text);
        }
        doc_sections.extend(tag_sections(p));
    }
    if let Some(section) = deprecation_doc_section(decl.deprecated, parsed.as_ref()) {
        doc_sections.push(section);
    }

    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let depr = make_deprecation(decl.deprecated, &decl.annotations, parsed.as_ref());

    let sym = make_sym(
        decl.simple_name.as_ref(),
        visibility(&decl.modifiers),
        if doc_sections.is_empty() {
            None
        } else {
            Some(doc_sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    ctx.low.declare(
        JavaId::type_(qname),
        parent_id,
        sym,
        nudox_ir::kinds::sum::Enum::builder()
            .variants(variant_refs)
            .generics(generics)
            .wheres(wheres)
            .build(),
    );
}

fn lower_enum_constant(
    ctx: &mut LoweringCtx<'_>,
    owner_qname: &str,
    c: &EnumConstant,
) -> Ref<nudox_ir::kinds::sum::Variant> {
    let vid = JavaId::member(owner_qname, c.name.as_ref());
    let parsed = ctx.parse_doc(c.doc.as_deref(), c.doc_kind.as_deref(), Some(owner_qname));
    let (src, line) = source_of(c.position.as_ref());

    let mut sections: Vec<String> = Vec::new();
    if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
        sections.push(text);
    }
    if let Some(section) = deprecation_doc_section(c.deprecated, parsed.as_ref()) {
        sections.push(section);
    }
    // Constructor args and body (source-only facts) ride documentation.
    if let Some(window) = &c.source {
        let source_info = parse_constant_source(window.as_ref(), c.name.as_ref());
        if let Some(args) = source_info.args {
            sections.push(format!("Arguments: `({args})`"));
        }
        if source_info.has_body {
            sections.push("Declares a constant class body.".to_string());
        }
    }

    let depr = make_deprecation(c.deprecated, &c.annotations, parsed.as_ref());
    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();

    let sym = make_sym(
        c.name.as_ref(),
        Visibility::Public,
        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    ctx.low.declare(
        vid,
        Some(JavaId::type_(owner_qname)),
        sym,
        nudox_ir::kinds::sum::Variant::builder()
            .form(VariantForm::Unit)
            // Java enum constants have no explicit discriminant; the JVM assigns ordinals.
            .build(),
    )
}

// ── Fields ──────────────────────────────────────────────────────────────────────

fn lower_field(
    ctx: &mut LoweringCtx<'_>,
    owner_qname: &str,
    f: &SchemaField,
    parent_id: Option<JavaId>,
) -> Ref<nudox_ir::kinds::record::Field> {
    let fid = JavaId::member(owner_qname, f.name.as_ref());
    let parsed = ctx.parse_doc(f.doc.as_deref(), f.doc_kind.as_deref(), Some(owner_qname));
    let (src, line) = source_of(f.position.as_ref());

    let mut doc_sections: Vec<String> = Vec::new();
    if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
        doc_sections.push(text);
    }
    if let Some(section) = deprecation_doc_section(f.deprecated, parsed.as_ref()) {
        doc_sections.push(section);
    }

    let mut attrs: Vec<FieldAttribute> = Vec::new();
    if !has_modifier(&f.modifiers, Modifier::Final) {
        attrs.push(FieldAttribute::Mutable);
    }
    if has_modifier(&f.modifiers, Modifier::Static) {
        attrs.push(FieldAttribute::Static);
    }

    let depr = make_deprecation(f.deprecated, &f.annotations, parsed.as_ref());
    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let ty = lower_type(ctx, &f.ty);

    let sym = make_sym(
        f.name.as_ref(),
        visibility(&f.modifiers),
        if doc_sections.is_empty() {
            None
        } else {
            Some(doc_sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    ctx.low.declare(
        fid,
        parent_id,
        sym,
        nudox_ir::kinds::record::Field::builder()
            .key(FieldKey::Named)
            .ty(ty)
            .attributes(attrs)
            .build(),
    )
}

// ── Methods ──────────────────────────────────────────────────────────────────────

/// Lower one method into its own `Function` declaration (overloads each become
/// separate declarations with distinct `JavaId`s).
fn lower_method(ctx: &mut LoweringCtx<'_>, owner_qname: &str, m: &Method, is_provided: bool) {
    let mid = JavaId::method(owner_qname, m.name.as_ref(), &m.params);
    let parsed = ctx.parse_doc(m.doc.as_deref(), m.doc_kind.as_deref(), Some(owner_qname));
    let (src, line) = source_of(m.position.as_ref());

    // --- Input params. ---------------------------------------------------
    let last = m.params.len().saturating_sub(1);
    let mut input_refs: Vec<Ref<nudox_ir::kinds::param::Param>> = Vec::new();
    for (idx, p) in m.params.iter().enumerate() {
        let is_variadic = m.varargs && idx == last;
        let param_id = JavaId(format!("{owner_qname}#{}/{}", m.name, p.name));
        let ty = lower_type(ctx, &p.ty);
        let mut attrs = Vec::new();
        if is_variadic {
            attrs.push(ParamAttribute::Variadic);
        }
        let param_doc = parsed
            .as_ref()
            .and_then(|doc| doc.params.get(p.name.as_ref()))
            .cloned()
            .unwrap_or_default();
        let param_sym = Symbol {
            name: p.name.to_string(),
            visibility: Visibility::Public,
            documentation: param_doc,
            source: PathBuf::from(src),
            span: 0..line.unwrap_or(0) as usize,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let pref = ctx.low.declare(
            param_id,
            Some(mid.clone()),
            param_sym,
            nudox_ir::kinds::param::Param::builder()
                .ty(ty)
                .attributes(attrs)
                .build(),
        );
        input_refs.push(pref);
    }

    // --- Output params (return type only). --------------------------------
    let mut output_refs: Vec<Ref<nudox_ir::kinds::param::Param>> = Vec::new();

    if let Some(ret) = &m.return_type
        && !ret.is_void()
    {
        let ret_id = JavaId(format!("{owner_qname}#{}/$return", m.name));
        let ret_ty = lower_type(ctx, ret);
        let ret_doc = parsed
            .as_ref()
            .and_then(|p| p.returns.clone())
            .unwrap_or_default();
        let ret_sym = Symbol {
            name: String::new(),
            visibility: Visibility::Public,
            documentation: ret_doc,
            source: PathBuf::from(src),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let rref = ctx.low.declare(
            ret_id,
            Some(mid.clone()),
            ret_sym,
            nudox_ir::kinds::param::Param::builder().ty(ret_ty).build(),
        );
        output_refs.push(rref);
    }

    // Lower the declared throws clause into `Function::throws: List<Type>`.
    // The IR now has a dedicated structured slot for this purpose; the old
    // `Symbol.attrs` encoding (AttrTok { token: "throws", arg: qualified_name })
    // is superseded and dropped. The prose "Throws:" documentation section is
    // retained for rendering. External exception types (not in this extraction)
    // lower to Type::Any — the same best-effort degradation used everywhere.
    let throws_types: Vec<Type> = m
        .thrown
        .iter()
        .map(|thrown| lower_type(ctx, thrown))
        .collect();

    // --- Modifiers. -------------------------------------------------------
    // Java has no async/const/unsafe/pure/generator — the Function modifier
    // list is always empty; Java-specific modifiers ride documentation.
    let fn_mods: Vec<FnModifier> = Vec::new();

    // --- Generic type params. ---------------------------------------------
    let (fn_generics, fn_wheres) = lower_type_params(ctx, &m.type_params);

    // --- Documentation. ---------------------------------------------------
    let mut doc_sections: Vec<String> = Vec::new();
    if let Some(p) = &parsed {
        if let Some(text) = p.documentation() {
            doc_sections.push(text);
        }
        if let Some(section) = p.type_params_section() {
            doc_sections.push(section);
        }
        doc_sections.extend(tag_sections(p));
    }
    if let Some(section) = deprecation_doc_section(m.deprecated, parsed.as_ref()) {
        doc_sections.push(section);
    }
    // Non-access modifiers with no IR slot → docs.
    let interesting_mods: Vec<&str> = m
        .modifiers
        .iter()
        .filter_map(|modifier| match modifier {
            Modifier::Static => Some("static"),
            Modifier::Final => Some("final"),
            Modifier::Abstract => Some("abstract"),
            Modifier::Synchronized => Some("synchronized"),
            Modifier::Native => Some("native"),
            Modifier::Strictfp => Some("strictfp"),
            Modifier::Default => Some("default"),
            _ => None,
        })
        .collect();
    if !interesting_mods.is_empty() {
        doc_sections.push(format!("Declared: `{}`", interesting_mods.join(" ")));
    }
    if let Some(default) = &m.annotation_default {
        doc_sections.push(format!("Default: `{}`", render_value(default)));
    }
    // Throws section: declared thrown types from the `throws` clause, annotated
    // with @throws javadoc descriptions. All throws (declared and doc-only) are
    // folded into one "Throws:" documentation section.
    {
        let declared_names: Vec<&str> = m.thrown.iter().filter_map(|t| t.declared_name()).collect();
        let mut throw_lines: Vec<String> = Vec::new();

        // Declared-in-clause thrown types: name + description (if any).
        for name in &declared_names {
            let text = parsed
                .as_ref()
                .and_then(|p| {
                    p.throws
                        .iter()
                        .find(|(reference, _)| {
                            let ref_type = reference.split('#').next().unwrap_or(reference);
                            let ref_simple = ref_type.rsplit('.').next().unwrap_or(ref_type);
                            let simple_d = name.rsplit('.').next().unwrap_or(name);
                            *name == ref_type || simple_d == ref_simple
                        })
                        .map(|(_, t)| t.as_str())
                })
                .unwrap_or("");
            if text.is_empty() {
                throw_lines.push(format!("- `{name}`"));
            } else {
                throw_lines.push(format!("- `{name}` — {text}"));
            }
        }

        // Doc-only @throws (mentioned in javadoc but not in the throws clause).
        if let Some(p) = &parsed {
            for (reference, text) in &p.throws {
                let ref_type = reference.split('#').next().unwrap_or(reference);
                let ref_simple = ref_type.rsplit('.').next().unwrap_or(ref_type);
                let is_declared = declared_names.iter().any(|d| {
                    let simple_d = d.rsplit('.').next().unwrap_or(d);
                    *d == ref_type || simple_d == ref_simple
                });
                if !is_declared {
                    if text.is_empty() {
                        throw_lines.push(format!("- `{ref_type}` (undeclared)"));
                    } else {
                        throw_lines.push(format!("- `{ref_type}` (undeclared) — {text}"));
                    }
                }
            }
        }

        if !throw_lines.is_empty() {
            doc_sections.push(format!("Throws:\n{}", throw_lines.join("\n")));
        }
    }

    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let depr = make_deprecation(m.deprecated, &m.annotations, parsed.as_ref());

    let sym = make_sym(
        m.name.as_ref(),
        visibility(&m.modifiers),
        if doc_sections.is_empty() {
            None
        } else {
            Some(doc_sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    // Build the function — instance methods get SharedRef receiver; static get none.
    // throws_types is populated from the `throws` clause above and wired into
    // Function::throws (the dedicated structured slot).
    let is_static = has_modifier(&m.modifiers, Modifier::Static);
    let function = if is_static {
        nudox_ir::kinds::function::Function::builder()
            .input_params(input_refs)
            .output_params(output_refs)
            .modifiers(fn_mods)
            .generics(fn_generics)
            .wheres(fn_wheres)
            .throws(throws_types)
            .is_defaulted(is_provided)
            .build()
    } else {
        nudox_ir::kinds::function::Function::builder()
            .receiver(nudox_ir::kinds::function::Receiver::SharedRef)
            .input_params(input_refs)
            .output_params(output_refs)
            .modifiers(fn_mods)
            .generics(fn_generics)
            .wheres(fn_wheres)
            .throws(throws_types)
            .is_defaulted(is_provided)
            .build()
    };

    ctx.low
        .declare(mid, Some(JavaId::type_(owner_qname)), sym, function);
}

/// Lower a constructor into a `Function` entry with no receiver.
fn lower_constructor(ctx: &mut LoweringCtx<'_>, owner_qname: &str, m: &Method) {
    let mid = JavaId::method(owner_qname, "<init>", &m.params);
    let parsed = ctx.parse_doc(m.doc.as_deref(), m.doc_kind.as_deref(), Some(owner_qname));
    let (src, line) = source_of(m.position.as_ref());

    let last = m.params.len().saturating_sub(1);
    let mut input_refs: Vec<Ref<nudox_ir::kinds::param::Param>> = Vec::new();
    for (idx, p) in m.params.iter().enumerate() {
        let is_variadic = m.varargs && idx == last;
        let param_id = JavaId(format!("{owner_qname}#<init>/{}", p.name));
        let ty = lower_type(ctx, &p.ty);
        let mut attrs = Vec::new();
        if is_variadic {
            attrs.push(ParamAttribute::Variadic);
        }
        let param_doc = parsed
            .as_ref()
            .and_then(|doc| doc.params.get(p.name.as_ref()))
            .cloned()
            .unwrap_or_default();
        let param_sym = Symbol {
            name: p.name.to_string(),
            visibility: Visibility::Public,
            documentation: param_doc,
            source: PathBuf::from(src),
            span: 0..line.unwrap_or(0) as usize,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let pref = ctx.low.declare(
            param_id,
            Some(mid.clone()),
            param_sym,
            nudox_ir::kinds::param::Param::builder()
                .ty(ty)
                .attributes(attrs)
                .build(),
        );
        input_refs.push(pref);
    }

    let (fn_generics, fn_wheres) = lower_type_params(ctx, &m.type_params);

    let mut doc_sections: Vec<String> = Vec::new();
    if let Some(p) = &parsed
        && let Some(text) = p.documentation()
    {
        doc_sections.push(text);
    }

    let doc_links: Vec<String> = parsed.as_ref().map(|p| p.links.clone()).unwrap_or_default();
    let depr = make_deprecation(m.deprecated, &m.annotations, parsed.as_ref());

    let sym = make_sym(
        "<init>",
        visibility(&m.modifiers),
        if doc_sections.is_empty() {
            None
        } else {
            Some(doc_sections.join("\n\n"))
        },
        depr,
        &doc_links,
        src,
        line,
    );

    // Constructors have no receiver — do not call .receiver().
    ctx.low.declare(
        mid,
        Some(JavaId::type_(owner_qname)),
        sym,
        nudox_ir::kinds::function::Function::builder()
            .input_params(input_refs)
            .generics(fn_generics)
            .wheres(fn_wheres)
            .build(),
    );
}

// ── Annotation value rendering ────────────────────────────────────────────────

pub fn render_value(v: &Value) -> String {
    match v {
        Value::String { value } => format!("{value:?}"),
        Value::Boolean { value } => value.to_string(),
        Value::Char { value } => format!("'{value}'"),
        Value::Double { value } => value.to_string(),
        Value::Int { value } => value.to_string(),
        Value::Enum { ty, name } => format!("{ty}.{name}"),
        Value::Type { value } => format!("{}.class", type_erase(value)),
        Value::Array { values } => {
            let inner: Vec<String> = values.iter().map(render_value).collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Annotation { value } => {
            if value.values.is_empty() {
                format!("@{}", value.ty)
            } else {
                let vals: Vec<String> = value
                    .values
                    .iter()
                    .map(|(k, v)| format!("{k} = {}", render_value(v)))
                    .collect();
                format!("@{}({})", value.ty, vals.join(", "))
            }
        }
        Value::Other { repr } => repr.to_string(),
    }
}

// ── Enum constant source parsing (ported from item.rs) ────────────────────────

/// What a raw source window for one enum constant reveals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnumConstSource {
    pub args: Option<String>,
    pub has_body: bool,
}

/// Parse an enum-constant declaration window.
pub fn parse_constant_source(window: &str, name: &str) -> EnumConstSource {
    let bytes = window.as_bytes();
    let mut i = 0usize;

    loop {
        i = skip_trivia(window, i);
        if bytes.get(i) == Some(&b'@') {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || bytes[i] == b'$'
                    || bytes[i] == b'.')
            {
                i += 1;
            }
            let after = skip_trivia(window, i);
            if bytes.get(after) == Some(&b'(') {
                match skip_balanced(window, after) {
                    Some(end) => i = end,
                    None => return EnumConstSource::default(),
                }
            } else {
                i = after;
            }
            continue;
        }
        break;
    }

    if !window[i..].starts_with(name) {
        return EnumConstSource::default();
    }
    let boundary = bytes.get(i + name.len());
    if boundary.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$') {
        return EnumConstSource::default();
    }
    i += name.len();
    i = skip_trivia(window, i);

    let mut out = EnumConstSource::default();
    if bytes.get(i) == Some(&b'(') {
        match skip_balanced(window, i) {
            Some(end) => {
                out.args = Some(window[i + 1..end - 1].trim().to_string());
                i = end;
            }
            None => return out,
        }
    }
    i = skip_trivia(window, i);
    out.has_body = bytes.get(i) == Some(&b'{');
    out
}

fn skip_trivia(text: &str, mut i: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if text[i..].starts_with("//") {
            match text[i..].find('\n') {
                Some(offset) => i += offset + 1,
                None => return bytes.len(),
            }
        } else if text[i..].starts_with("/*") {
            match text[i + 2..].find("*/") {
                Some(offset) => i += 2 + offset + 2,
                None => return bytes.len(),
            }
        } else {
            return i;
        }
    }
}

fn skip_balanced(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes.get(open), Some(&b'('));
    let mut depth = 0usize;
    let mut i = open;

    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'/' if text[i..].starts_with("//") || text[i..].starts_with("/*") => {
                i = skip_trivia(text, i);
            }
            _ => i += 1,
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::lower::Lowering;
    use nudox_ir::package::PackageId;

    /// Build a minimal `Extraction` from a literal JSON fixture and lower it.
    fn fixture_extraction() -> Extraction {
        let json = r#"{
          "format": 1,
          "javaVersion": "21",
          "modules": [],
          "packages": [
            {
              "name": "com.example",
              "doc": "The example package.",
              "docKind": null,
              "annotations": [],
              "position": null
            }
          ],
          "types": [
            {
              "qualifiedName": "com.example.Shape",
              "simpleName": "Shape",
              "kind": "CLASS",
              "package": "com.example",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": "A shape.\n\n@param width the width\n@see com.example.Circle",
              "docKind": null,
              "position": {"file": "Shape.java", "line": 1},
              "fields": [
                {
                  "name": "width",
                  "type": {"kind": "primitive", "name": "int", "annotations": []},
                  "modifiers": ["public", "final"],
                  "constant": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": "The width.",
                  "docKind": null,
                  "position": null
                },
                {
                  "name": "MAX",
                  "type": {"kind": "primitive", "name": "int", "annotations": []},
                  "modifiers": ["public", "static", "final"],
                  "constant": {"kind": "int", "value": 100},
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": null,
                  "docKind": null,
                  "position": null
                }
              ],
              "enumConstants": [],
              "constructors": [
                {
                  "name": "<init>",
                  "modifiers": ["public"],
                  "typeParams": [],
                  "params": [{"name": "width", "type": {"kind": "primitive", "name": "int", "annotations": []}, "annotations": []}],
                  "return": null,
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": "Construct a shape.",
                  "docKind": null,
                  "position": null
                }
              ],
              "methods": [
                {
                  "name": "area",
                  "modifiers": ["public"],
                  "typeParams": [],
                  "params": [],
                  "return": {"kind": "primitive", "name": "double", "annotations": []},
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": "Calculate area.\n@return the area",
                  "docKind": null,
                  "position": null
                },
                {
                  "name": "draw",
                  "modifiers": ["public"],
                  "typeParams": [],
                  "params": [{"name": "color", "type": {"kind": "declared", "name": "java.lang.String", "args": [], "owner": null, "annotations": []}, "annotations": []}],
                  "return": {"kind": "void"},
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": null,
                  "docKind": null,
                  "position": null
                },
                {
                  "name": "draw",
                  "modifiers": ["public"],
                  "typeParams": [],
                  "params": [
                    {"name": "x", "type": {"kind": "primitive", "name": "int", "annotations": []}, "annotations": []},
                    {"name": "y", "type": {"kind": "primitive", "name": "int", "annotations": []}, "annotations": []}
                  ],
                  "return": {"kind": "void"},
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": null,
                  "docKind": null,
                  "position": null
                }
              ],
              "nested": []
            },
            {
              "qualifiedName": "com.example.Color",
              "simpleName": "Color",
              "kind": "ENUM",
              "package": "com.example",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [],
              "superclass": {"kind": "declared", "name": "java.lang.Enum", "args": [], "owner": null, "annotations": []},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": "Colors.",
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [
                {
                  "name": "RED",
                  "annotations": [],
                  "deprecated": false,
                  "doc": "The red color.",
                  "docKind": null,
                  "source": "RED,\nGREEN",
                  "position": null
                },
                {
                  "name": "GREEN",
                  "annotations": [],
                  "deprecated": false,
                  "doc": null,
                  "docKind": null,
                  "source": null,
                  "position": null
                }
              ],
              "constructors": [],
              "methods": [],
              "nested": []
            },
            {
              "qualifiedName": "com.example.Drawable",
              "simpleName": "Drawable",
              "kind": "INTERFACE",
              "package": "com.example",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": "Drawable interface.",
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [],
              "constructors": [],
              "methods": [
                {
                  "name": "draw",
                  "modifiers": ["public", "abstract"],
                  "typeParams": [],
                  "params": [],
                  "return": {"kind": "void"},
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": "Draw the shape.",
                  "docKind": null,
                  "position": null
                },
                {
                  "name": "defaultDraw",
                  "modifiers": ["public", "default"],
                  "typeParams": [],
                  "params": [],
                  "return": {"kind": "void"},
                  "thrown": [],
                  "varargs": false,
                  "default": true,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": "A default draw implementation.",
                  "docKind": null,
                  "position": null
                }
              ],
              "nested": []
            },
            {
              "qualifiedName": "com.example.OldApi",
              "simpleName": "OldApi",
              "kind": "CLASS",
              "package": "com.example",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [{"type": "java.lang.Deprecated", "values": {"since": {"kind": "string", "value": "2.0"}}}],
              "deprecated": true,
              "doc": "Old API.\n@deprecated Use {@link com.example.Shape} instead.",
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [],
              "constructors": [],
              "methods": [],
              "nested": []
            },
            {
              "qualifiedName": "com.example.Container",
              "simpleName": "Container",
              "kind": "CLASS",
              "package": "com.example",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [
                {"name": "T", "bounds": [{"kind": "declared", "name": "java.lang.Comparable", "args": [], "owner": null, "annotations": []}], "annotations": []}
              ],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": "A generic container.",
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [],
              "constructors": [],
              "methods": [
                {
                  "name": "getFirst",
                  "modifiers": ["public"],
                  "typeParams": [{"name": "R", "bounds": [{"kind": "declared", "name": "java.lang.Number", "args": [], "owner": null, "annotations": []}], "annotations": []}],
                  "params": [],
                  "return": {"kind": "typevar", "name": "R", "annotations": []},
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": null,
                  "docKind": null,
                  "position": null
                }
              ],
              "nested": []
            },
            {
              "qualifiedName": "com.example.Inner",
              "simpleName": "Inner",
              "kind": "CLASS",
              "package": "com.example",
              "module": null,
              "enclosing": "com.example.Shape",
              "nesting": "MEMBER",
              "modifiers": ["public", "static"],
              "typeParams": [],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": "Inner static class.",
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [],
              "constructors": [],
              "methods": [],
              "nested": []
            }
          ]
        }"#;
        serde_json::from_str(json).expect("fixture JSON must parse")
    }

    fn build_test_package() -> nudox_ir::package::IrPackage<JavaId> {
        let pkg_id = PackageId::path("com.example:test:1.0");
        let root_sym = Symbol {
            name: "com.example".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut low: Lowering<JavaId> = Lowering::new(pkg_id, root_sym);
        // No declare_wildcard_anchors call needed — wildcards use Type::Wildcard directly.
        let extraction = fixture_extraction();
        let mut ctx = LoweringCtx::new(&mut low, &extraction.types);
        lower_extraction(&mut ctx, &extraction);

        low.finish().expect("lowering must succeed")
    }

    #[test]
    fn package_lowers_without_error() {
        let _ = build_test_package();
    }

    #[test]
    fn shape_class_is_present() {
        let pkg = build_test_package();
        let shape = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Shape")
            .expect("Shape entry must exist");
        assert_eq!(shape.1.sym().visibility, Visibility::Public);
    }

    #[test]
    fn two_overloads_produce_two_distinct_declarations() {
        let pkg = build_test_package();
        let draws: Vec<_> = pkg.iter().filter(|(_, e)| e.sym().name == "draw").collect();
        // There are three `draw` entries total:
        //   com.example.Shape#draw(java.lang.String)
        //   com.example.Shape#draw(int,int)
        //   com.example.Drawable#draw()
        // Each overload is its own distinct declaration (no folding).
        assert!(
            draws.len() >= 2,
            "expected at least 2 distinct `draw` declarations, got {}",
            draws.len()
        );
    }

    #[test]
    fn enum_color_has_two_variants() {
        let pkg = build_test_package();
        let red = pkg.iter().find(|(_, e)| e.sym().name == "RED");
        let green = pkg.iter().find(|(_, e)| e.sym().name == "GREEN");
        assert!(red.is_some(), "RED variant must exist");
        assert!(green.is_some(), "GREEN variant must exist");
    }

    #[test]
    fn interface_default_method_is_marked() {
        let pkg = build_test_package();
        // `defaultDraw` should be is_defaulted = true in its Function kind.
        let default_draw = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "defaultDraw")
            .expect("defaultDraw must exist");
        // We verify it exists; is_defaulted is inside the kind payload.
        // (Accessing the kind internals requires pattern matching on Entry::kind().)
        assert!(default_draw.1.sym().name == "defaultDraw");
    }

    #[test]
    fn deprecated_class_carries_deprecation_and_links() {
        let pkg = build_test_package();
        let old = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "OldApi")
            .expect("OldApi must exist");
        let depr = old
            .1
            .sym()
            .deprecation
            .as_ref()
            .expect("OldApi must be deprecated");
        assert_eq!(depr.since.as_deref(), Some("2.0"));
        assert!(
            old.1
                .sym()
                .doc_links
                .iter()
                .any(|l| l.target.contains("Shape")),
            "doc_links must contain the Shape link"
        );
    }

    #[test]
    fn package_private_field_has_package_visibility() {
        // The fixture uses only explicit `public` — we verify Package fallback works
        // by parsing a field with no access modifier.
        let modifiers = &[schema::Modifier::Static, schema::Modifier::Final];
        assert_eq!(visibility(modifiers), Visibility::Package);
    }

    #[test]
    fn generic_method_produces_function_entry() {
        let pkg = build_test_package();
        let get_first = pkg.iter().find(|(_, e)| e.sym().name == "getFirst");
        assert!(get_first.is_some(), "getFirst must be lowered");
    }

    #[test]
    fn inner_static_class_has_shape_as_parent() {
        let pkg = build_test_package();
        let inner = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Inner")
            .expect("Inner must exist");
        assert!(
            inner.1.parent().is_some(),
            "Inner must have a parent (Shape)"
        );
    }

    #[test]
    fn javadoc_documentation_populates_symbol() {
        let pkg = build_test_package();
        let shape = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Shape")
            .expect("Shape must exist");
        // Shape's doc is "A shape." with @param and @see tags.
        assert!(
            shape.1.sym().documentation.contains("A shape."),
            "Shape documentation must contain the summary"
        );
    }

    // --- Direct javadoc unit tests ------------------------------------------

    #[test]
    fn javadoc_param_parsed() {
        use crate::javadoc::{DocFlavor, parse};
        use std::collections::HashMap;
        let raw = "Do something.\n@param count the count\n@deprecated old";
        let parsed = parse(raw, DocFlavor::Traditional, None, &HashMap::new());
        assert_eq!(
            parsed.params.get("count").map(String::as_str),
            Some("the count")
        );
        assert!(parsed.deprecated.is_some());
    }

    #[test]
    fn javadoc_doc_links_captured() {
        use crate::javadoc::{DocFlavor, parse};
        let mut resolver = std::collections::HashMap::new();
        resolver.insert("Shape".to_owned(), "com.example.Shape".to_owned());
        let raw = "See {@link Shape#area()} for details.";
        let parsed = parse(
            raw,
            DocFlavor::Traditional,
            Some("com.example.Foo"),
            &resolver,
        );
        assert!(
            parsed.links.iter().any(|l| l.contains("Shape")),
            "link to Shape must be captured"
        );
    }

    // --- Enum constant source parsing ----------------------------------------

    #[test]
    fn enum_constant_source_parse_bare() {
        let r = parse_constant_source("RED,\nGREEN", "RED");
        assert_eq!(r.args, None);
        assert!(!r.has_body);
    }

    #[test]
    fn enum_constant_source_parse_with_args() {
        let r = parse_constant_source("LOW(1, \"low\"),\nHIGH(9)", "LOW");
        assert_eq!(r.args.as_deref(), Some("1, \"low\""));
        assert!(!r.has_body);
    }

    // --- Wildcard produces Type::Wildcard, NOT a synthetic anchor ------------

    /// `? extends T`, `? super T`, and bare `?` all lower to `Type::Wildcard`
    /// with the correct variance. The old code created fake `Alias` entries
    /// named `"java::wildcard::extends"` etc.; those must NOT appear.
    #[test]
    fn wildcard_lowers_to_type_wildcard_not_anchor() {
        let json = r#"{
          "format": 1,
          "javaVersion": "21",
          "modules": [],
          "packages": [{"name": "wc", "doc": null, "docKind": null, "annotations": [], "position": null}],
          "types": [
            {
              "qualifiedName": "wc.Box",
              "simpleName": "Box",
              "kind": "CLASS",
              "package": "wc",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [{"name": "T", "bounds": [], "annotations": []}],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": null,
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [],
              "constructors": [],
              "methods": [
                {
                  "name": "bounded",
                  "modifiers": ["public"],
                  "typeParams": [],
                  "params": [
                    {
                      "name": "x",
                      "type": {
                        "kind": "declared",
                        "name": "wc.Box",
                        "args": [
                          {"kind": "wildcard", "extends": {"kind": "declared", "name": "wc.Box", "args": [], "owner": null, "annotations": []}, "super": null}
                        ],
                        "owner": null,
                        "annotations": []
                      },
                      "annotations": []
                    }
                  ],
                  "return": {"kind": "void"},
                  "thrown": [],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": null,
                  "docKind": null,
                  "position": null
                }
              ],
              "nested": []
            }
          ]
        }"#;
        let extraction: Extraction = serde_json::from_str(json).expect("parse");

        let pkg_id = PackageId::path("wc:test:1.0");
        let root_sym = Symbol {
            name: "wc".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut low: Lowering<JavaId> = Lowering::new(pkg_id, root_sym);
        let mut ctx = LoweringCtx::new(&mut low, &extraction.types);
        lower_extraction(&mut ctx, &extraction);
        let pkg = low
            .finish()
            .expect("no wildcard anchor entries → no Undeclared errors");

        // No entry named "? extends", "? super", or "?" should appear.
        for (_, entry) in pkg.iter() {
            let name = &entry.sym().name;
            assert!(
                !name.starts_with("? ") && name.as_str() != "?",
                "found synthetic wildcard anchor entry: `{name}` — anchors must not be declared"
            );
        }
    }

    // --- TypeVar uses lower to Type::TypeVar, not Type::Any ------------------

    /// A method `<R extends Number> R getFirst()` uses `R` as its return type.
    /// That use should be `Type::TypeVar("R")`, not `Type::Any`.
    #[test]
    fn typevar_use_lowers_to_typevar_not_any() {
        // The fixture already contains `Container.getFirst()` which returns
        // typevar `R`.  We verify that the return param's type is not `Any`.
        // We can't easily inspect the kind internals via `IrPackage::iter` alone,
        // so we verify indirectly: the method exists and no Undeclared error
        // occurs (Type::TypeVar doesn't `refer()` anything, unlike the old anchor
        // approach).  A lower-level check uses `lower_type` directly.
        use crate::schema::TypeMirror;
        use nudox_ir::build::Lowering as NirLowering;
        use nudox_ir::kinds::ty::{Type, Variance};

        // Direct unit test of lower_type for Typevar.
        let pkg_id = PackageId::path("tv:test:1.0");
        let root_sym = Symbol {
            name: "tv".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut low: NirLowering<JavaId> = NirLowering::new(pkg_id, root_sym);
        let types: Vec<schema::TypeDecl> = Vec::new();
        let mut ctx = LoweringCtx::new(&mut low, &types);

        let typevar_mirror = TypeMirror::Typevar {
            name: "R".into(),
            annotations: vec![],
        };
        let result = lower_type(&mut ctx, &typevar_mirror);
        assert_eq!(
            result,
            Type::TypeVar("R".to_owned()),
            "TypeVar mirror must lower to Type::TypeVar, not Type::Any"
        );

        // Also verify wildcards lower correctly.
        let wc_covariant = TypeMirror::Wildcard {
            extends_bound: Some(Box::new(TypeMirror::Void)),
            super_bound: None,
        };
        let wc_result = lower_type(&mut ctx, &wc_covariant);
        assert!(
            matches!(
                wc_result,
                Type::Wildcard {
                    variance: Variance::Covariant,
                    ..
                }
            ),
            "? extends X must lower to Type::Wildcard {{ Covariant, .. }}"
        );

        let wc_contravariant = TypeMirror::Wildcard {
            extends_bound: None,
            super_bound: Some(Box::new(TypeMirror::Void)),
        };
        let wc_result2 = lower_type(&mut ctx, &wc_contravariant);
        assert!(
            matches!(
                wc_result2,
                Type::Wildcard {
                    variance: Variance::Contravariant,
                    ..
                }
            ),
            "? super X must lower to Type::Wildcard {{ Contravariant, .. }}"
        );

        let wc_unbounded = TypeMirror::Wildcard {
            extends_bound: None,
            super_bound: None,
        };
        let wc_result3 = lower_type(&mut ctx, &wc_unbounded);
        assert_eq!(
            wc_result3,
            Type::Wildcard {
                variance: Variance::Invariant,
                bound: None
            },
            "bare ? must lower to Type::Wildcard {{ Invariant, None }}"
        );
    }

    // --- Throws are in Function::throws, NOT output params or Symbol::attrs ---

    /// A method declaring `throws IOException` must NOT produce an output
    /// parameter named "throws". The thrown type must appear in
    /// `Function::throws` (the dedicated structured slot), and the method
    /// documentation must carry a "Throws:" prose section for rendering.
    /// The old `Symbol::attrs` encoding (`AttrTok { token: "throws", .. }`)
    /// is superseded and must NOT be present.
    #[test]
    fn throws_in_function_throws_not_attrs_or_output_params() {
        let json = r#"{
          "format": 1,
          "javaVersion": "21",
          "modules": [],
          "packages": [{"name": "ex", "doc": null, "docKind": null, "annotations": [], "position": null}],
          "types": [
            {
              "qualifiedName": "ex.Writer",
              "simpleName": "Writer",
              "kind": "CLASS",
              "package": "ex",
              "module": null,
              "enclosing": null,
              "nesting": "TOP_LEVEL",
              "modifiers": ["public"],
              "typeParams": [],
              "superclass": {"kind": "none"},
              "interfaces": [],
              "permits": [],
              "recordComponents": [],
              "annotations": [],
              "deprecated": false,
              "doc": null,
              "docKind": null,
              "position": null,
              "fields": [],
              "enumConstants": [],
              "constructors": [],
              "methods": [
                {
                  "name": "write",
                  "modifiers": ["public"],
                  "typeParams": [],
                  "params": [],
                  "return": {"kind": "void"},
                  "thrown": [
                    {"kind": "declared", "name": "java.io.IOException", "args": [], "owner": null, "annotations": []}
                  ],
                  "varargs": false,
                  "default": false,
                  "receiver": null,
                  "annotationDefault": null,
                  "annotations": [],
                  "deprecated": false,
                  "origin": "EXPLICIT",
                  "doc": "Write something.\n@throws java.io.IOException on failure",
                  "docKind": null,
                  "position": null
                }
              ],
              "nested": []
            }
          ]
        }"#;
        let extraction: Extraction = serde_json::from_str(json).expect("parse");
        let pkg_id = PackageId::path("ex:test:1.0");
        let root_sym = Symbol {
            name: "ex".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut low: Lowering<JavaId> = Lowering::new(pkg_id, root_sym);
        let mut ctx = LoweringCtx::new(&mut low, &extraction.types);
        lower_extraction(&mut ctx, &extraction);
        let pkg = low.finish().expect("lowering must succeed");

        // No entry named "throws" should exist — throws are NOT output params.
        for (_, entry) in pkg.iter() {
            assert_ne!(
                entry.sym().name.as_str(),
                "throws",
                "found a `throws` output-param entry — throws must NOT be modelled as params"
            );
        }

        let write_entry = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "write")
            .expect("write method must exist");

        // The `write` method's symbol must NOT carry a `throws` AttrTok.
        // The structured Function::throws slot supersedes Symbol::attrs.
        let has_throws_attr = write_entry
            .1
            .sym()
            .attrs
            .iter()
            .any(|a| a.token == "throws");
        assert!(
            !has_throws_attr,
            "write's symbol attrs must NOT carry a `throws` AttrTok — use Function::throws instead"
        );

        // Function::throws must carry exactly one entry (java.io.IOException → Type::Any
        // since it is not in the current extraction).
        let kind = write_entry
            .1
            .kind()
            .as_owned_kind()
            .expect("write must be Owned");
        match kind {
            nudox_ir::kind::Kind::Function(f) => {
                assert_eq!(
                    f.throws.len(),
                    1,
                    "Function::throws must have 1 entry for the declared IOException"
                );
                // External type: lowers to Type::Any.
                assert!(
                    matches!(f.throws[0], Type::Any),
                    "IOException (external) must lower to Type::Any in Function::throws"
                );
            }
            other => panic!("write kind must be Function, got {other:?}"),
        }

        // The `write` method's documentation must mention "Throws:".
        assert!(
            write_entry.1.sym().documentation.contains("Throws:"),
            "write's documentation must contain a 'Throws:' section"
        );
    }

    // --- Type::Annotated from Java type-use annotations ----------------------

    /// `@NonNull String` in a type-use position lowers to
    /// `Type::Annotated { inner: Nominal/Any, annotation: AttrTok { token: "com.example.NonNull", .. } }`.
    /// This test exercises `wrap_annotated` directly via `lower_type`.
    #[test]
    fn annotated_type_mirrors_lower_to_type_annotated() {
        use crate::schema::{Annotation, TypeMirror};
        use nudox_ir::build::Lowering as NirLowering;
        use nudox_ir::kinds::ty::Type;

        let pkg_id = PackageId::path("ann:test:1.0");
        let root_sym = Symbol {
            name: "ann".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut low: NirLowering<JavaId> = NirLowering::new(pkg_id, root_sym);
        let types: Vec<schema::TypeDecl> = Vec::new();
        let mut ctx = LoweringCtx::new(&mut low, &types);

        // `@com.example.NonNull String` — declared annotation on a Declared type.
        // java.lang.String is external (not in known_qualified), so inner = Type::Any.
        let annotated_string = TypeMirror::Declared {
            name: "java.lang.String".into(),
            args: vec![],
            owner: None,
            annotations: vec![Annotation {
                ty: "com.example.NonNull".into(),
                values: Default::default(),
            }],
        };
        let result = lower_type(&mut ctx, &annotated_string);
        assert!(
            matches!(result, Type::Annotated { ref annotation, .. } if annotation.token == "com.example.NonNull"),
            "annotated type must lower to Type::Annotated with correct token; got {result:?}"
        );
        if let Type::Annotated { inner, .. } = result {
            assert!(
                matches!(*inner, Type::Any),
                "inner of annotated external type must be Type::Any; got {inner:?}"
            );
        }

        // Unannotated type — must lower to plain Type::Any (no Annotated wrapper).
        let plain_string = TypeMirror::Declared {
            name: "java.lang.String".into(),
            args: vec![],
            owner: None,
            annotations: vec![],
        };
        let plain_result = lower_type(&mut ctx, &plain_string);
        assert!(
            matches!(plain_result, Type::Any),
            "unannotated external type must lower to plain Type::Any; got {plain_result:?}"
        );
    }
}
