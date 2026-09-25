//! Flat, one-pass lowering of oracle output into `Lowering<GoId>`.
//!
//! # Module layout
//!
//! - [`mod`]   — `GoId`, shared helpers, entry-point dispatch
//! - [`types`] — lowering of struct / interface / newtype / iota-enum
//! - [`values`] — lowering of alias / func / const / var

mod types;
mod values;

use std::{collections::HashSet, ops::Range, path::PathBuf};

use nudox_ir::build::{
    Deprecation, DocLink, Function, IrPackage, Lowering, Module, PackageId, PackageLineageId,
    Param, ParamAttribute, Ref, Symbol, Visibility,
};
use nudox_ir::entry::{SourceLocation, Unlocated};
use nudox_ir::vocab::{Confidence, ReferenceKind, RelSpan};

use crate::go::{
    error::{self, Result},
    oracle::{self, DeclKind, TypeKind},
    types as go_types,
};

// ---------------------------------------------------------------------------
// GoId — the producer's natural symbol identifier
// ---------------------------------------------------------------------------

/// The Go oracle's natural symbol identifier.
///
/// Go uses the import path as the package namespace and the bare name as the
/// symbol name within that package.  Methods live under their receiver type, so
/// we encode that as a three-element form.
///
/// All variants are `Clone + Eq + Hash + Debug` (derived), as required by
/// `Lowering<Id>`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GoId {
    /// The synthetic root module that wraps the whole oracle output.
    Root,
    /// A package: the import path is the id.
    Package { import_path: String },
    /// A package-level declaration (type, alias, func, const, var).
    Item { import_path: String, name: String },
    /// A method or struct field, child of a named type.
    Member {
        import_path: String,
        type_name: String,
        member_name: String,
        /// `None` for a member this type declares itself (a field, a
        /// directly-declared method, or a synthetic member such as a
        /// function's params/results) — `import_path` above is already that
        /// member's home package. `Some(origin)` for a method *promoted*
        /// through an embedded field, carrying the oracle's fully qualified
        /// defining type (`oracle::Method::origin`, e.g.
        /// `"google.golang.org/grpc.EmptyServerOption"` — package path plus
        /// bare type name, joined by `.`; see `oracle/go/serialize.go`'s
        /// `originOf`).
        ///
        /// This is what makes `GoId::Member` able to express Go's
        /// "uniqueness of identifiers" rule (spec, "Declarations and
        /// scope"): two unexported names are the same identifier only when
        /// they belong to the same package. A struct can legally declare its
        /// own field `apply` *and* promote an embedded type's unexported
        /// method also named `apply` — real case:
        /// `google.golang.org/grpc/xds`'s `serverOption` embeds
        /// `grpc.EmptyServerOption` — because the two `apply`s are different
        /// identifiers per spec even though spelled the same.
        /// `go/types.NewMethodSet` (`oracle/go/serialize.go`'s
        /// `promotedMethods`) reports both. Tagging every promoted method
        /// with its origin, while every local member stays `None`, makes the
        /// two cases structurally distinct ids instead of colliding — no
        /// skip, no drop, and no risk of two *different* local members
        /// colliding with each other (they were never tagged before either).
        promoted_from: Option<String>,
    },
    /// A variant of a Go iota-enum convention.
    Variant {
        import_path: String,
        type_name: String,
        variant_name: String,
    },
}

// ---------------------------------------------------------------------------
// The iota enum detection heuristic (salvaged semantics from item.rs)
// ---------------------------------------------------------------------------

/// Which top-level const decls belong to the iota-enum pattern for each type.
struct IotaEnums<'a> {
    /// type-name → ordered list of const Decls that are variants of that type.
    variants_by_type: std::collections::HashMap<&'a str, Vec<&'a oracle::Decl>>,
}

fn detect_iota_enums(pkg: &oracle::Package) -> IotaEnums<'_> {
    // Which defined types exist?
    let defined_types: std::collections::HashMap<&str, &oracle::Decl> = pkg
        .decls
        .iter()
        .filter(|d| d.kind == DeclKind::Type)
        .map(|d| (d.name.as_str(), d))
        .collect();

    let mut variants_by_type: std::collections::HashMap<&str, Vec<&oracle::Decl>> =
        std::collections::HashMap::new();

    for decl in &pkg.decls {
        if decl.kind != DeclKind::Const || !decl.group_has_iota {
            continue;
        }
        let Some(const_type) = &decl.r#type else {
            continue;
        };
        // The constant's type must be a defined type of THIS package.
        if !matches!(const_type.kind, TypeKind::Named | TypeKind::Alias)
            || const_type.pkg != pkg.import_path
        {
            continue;
        }
        let Some(type_decl) = defined_types.get(const_type.name.as_str()) else {
            continue;
        };
        // Only non-struct, non-interface underlying types form the enum pattern.
        let is_sum_like = type_decl
            .underlying
            .as_ref()
            .is_none_or(|u| !matches!(u.kind, TypeKind::Struct | TypeKind::Interface));
        if is_sum_like {
            variants_by_type
                .entry(const_type.name.as_str())
                .or_default()
                .push(decl);
        }
    }

    // Sort variants by source position for deterministic order.
    for variants in variants_by_type.values_mut() {
        variants.sort_by_key(|d| {
            d.pos
                .as_ref()
                .map_or(("", 0), |p| (p.file.as_str(), p.line))
        });
    }

    IotaEnums { variants_by_type }
}

// ---------------------------------------------------------------------------
// Doc-string parsing
// ---------------------------------------------------------------------------

/// Parse a Go doc comment and populate `deprecation` and `doc_links` on a
/// [`Symbol`].
///
/// The Go doc convention for deprecation is a paragraph beginning with
/// `Deprecated: <reason>`.  We detect this in the raw doc text with a
/// simple prefix scan.  Full go/doc block parsing (headings, code blocks,
/// list items, link definitions) is available in the companion
/// `workspace/compiler/compile/go/docstring.rs` parser; here we implement
/// the minimum required by the mission brief.
///
/// Concretely:
/// * If any line or the start of any paragraph begins with `Deprecated: `
///   (case-sensitive, space-after-colon), we extract the rest of that line
///   and any following non-blank lines as the deprecation notice.
/// * `[Name]: URL` link-definition lines (go/doc format) are extracted as
///   `DocLink { target: URL, label: Some(Name) }` entries.
fn parse_doc(raw: &str) -> (Option<Deprecation>, Box<[DocLink]>) {
    let mut deprecation: Option<Deprecation> = None;
    let mut doc_links: Vec<DocLink> = Vec::new();

    // --- Deprecation detection ---
    // Walk paragraphs (separated by blank lines).  A paragraph that starts
    // with "Deprecated: " carries the notice.  Everything from that line
    // onward (within the paragraph) is the note.
    let mut in_deprecated_para = false;
    let mut deprecated_lines: Vec<&str> = Vec::new();
    let mut prev_blank = true; // treat the start as a paragraph boundary

    for line in raw.lines() {
        let trimmed = line.trim();
        let is_blank = trimmed.is_empty();

        if is_blank {
            in_deprecated_para = false;
            prev_blank = true;
            continue;
        }

        // Paragraph start: check for Deprecated: prefix.
        if prev_blank && let Some(rest) = trimmed.strip_prefix("Deprecated:") {
            in_deprecated_para = true;
            let note = rest.trim();
            if !note.is_empty() {
                deprecated_lines.push(note);
            }
            prev_blank = false;
            continue;
        }

        if in_deprecated_para {
            deprecated_lines.push(trimmed);
        }

        // Link definition: `[Name]: URL`
        if let Some(link) = parse_link_def(trimmed) {
            doc_links.push(link);
        }

        prev_blank = false;
    }

    if !deprecated_lines.is_empty() {
        let note = deprecated_lines.join(" ");
        deprecation = Some(Deprecation {
            note: Some(note),
            since: None,
        });
    }

    (deprecation, doc_links.into_boxed_slice())
}

/// Parse a go/doc link definition line `[Name]: URL`.
///
/// Returns `None` if the line does not match the expected pattern.
fn parse_link_def(line: &str) -> Option<DocLink> {
    let rest = line.strip_prefix('[')?;
    let (name, tail) = rest.split_once("]:")?;
    let url = tail.trim();
    if name.is_empty() || url.is_empty() || url.contains(char::is_whitespace) {
        return None;
    }
    Some(DocLink {
        target: url.to_string(),
        label: Some(name.to_string()),
        source_span: None,
    })
}

// ---------------------------------------------------------------------------
// Symbol helpers
// ---------------------------------------------------------------------------

/// Build a [`Symbol`] for a declaration, parsing the doc string for
/// deprecation notices and link definitions.
///
/// `span` is the oracle's full-declaration byte range when it has one (every
/// package-level `Decl` and every method declared in the package being
/// lowered — see `oracle/docs.go::declSpan`/`methodSpan`). `pos` is always
/// the declaring identifier's position; `resolve_span` falls back to it
/// (narrowed to just the identifier's own bytes) when `span` is `None` —
/// interface method signatures and promoted methods whose origin lives
/// outside this package, per the same doc comment.
fn sym_for(
    name: &str,
    doc: &str,
    exported: bool,
    pos: Option<&oracle::Pos>,
    span: Option<&oracle::Span>,
) -> Symbol {
    let vis = if exported {
        Visibility::Public
    } else {
        Visibility::Package
    };
    let source = pos.map(|p| PathBuf::from(&p.file)).unwrap_or_default();
    let span = resolve_span(name, pos, span);
    let (deprecation, doc_links) = parse_doc(doc);
    Symbol {
        name: name.to_owned(),
        visibility: vis,
        documentation: doc.to_owned(),
        source,
        span,
        aliases: Box::new([]),
        deprecation,
        doc_links,
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Resolve a declaration's byte span from what the oracle recorded.
///
/// Prefers `span` — the full declaration's AST range (`fset.Position(...).
/// Offset` on both ends; see `oracle/serialize.go`). When the oracle has no
/// such range for this symbol, falls back to `pos`'s identifier offset,
/// narrowed to just the identifier's own byte length: still a genuine,
/// non-empty span (real file, real bytes, a real substring of the
/// declaration), which is what distinguishes it from the historical `0..0`
/// sentinel this replaces. Only degrades to `0..0` when the oracle recorded
/// no position at all — the synthesized root module, and struct fields
/// (`oracle::StructField` carries no position; see `types::lower_struct`).
fn resolve_span(
    name: &str,
    pos: Option<&oracle::Pos>,
    span: Option<&oracle::Span>,
) -> Range<usize> {
    if let Some(span) = span
        && let (Ok(start), Ok(end)) = (usize::try_from(span.start), usize::try_from(span.end))
        && end > start
    {
        return start..end;
    }
    if let Some(pos) = pos
        && let Ok(start) = usize::try_from(pos.offset)
    {
        return start..start + name.len();
    }
    0..0
}

/// Build the four search-entry-point aliases for a method.
///
/// Go methods are identified by receiver type and method name.  A search
/// index must be able to find `Server.Close` by several query forms:
///
/// 1. `"TypeName.MethodName"` — short dotted form (the canonical Go notation).
/// 2. `"TypeName MethodName"` — space-separated, so a tokenizing full-text
///    index can match documents that mention the type and method separately.
/// 3. `"import_path.TypeName.MethodName"` — fully qualified dotted form, for
///    users who qualify their queries with the package path.
/// 4. `"import_path TypeName.MethodName"` — package path plus the dotted
///    method form, for package-scoped search (a common IDE query pattern).
///
/// Interface methods (no receiver) do NOT get aliases because they are
/// requirements, not implementations; their identity is the interface itself.
fn method_aliases(import_path: &str, type_name: &str, method_name: &str) -> Box<[String]> {
    let dotted = format!("{type_name}.{method_name}");
    let spaced = format!("{type_name} {method_name}");
    let full_dotted = format!("{import_path}.{type_name}.{method_name}");
    let pkg_dotted = format!("{import_path} {type_name}.{method_name}");
    Box::new([dotted, spaced, full_dotted, pkg_dotted])
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Lower every package in an oracle `Output` into an already-constructed
/// [`Lowering`] sink.
///
/// This is the shape [`crate::Producer::lower`] needs: the caller
/// (`crate::produce`, via `GoProducer`'s `Producer` impl) builds the
/// root-wrapped `Lowering` itself and hands it in, so this function neither
/// constructs a root symbol nor calls `finish`. [`lower_output`] is the
/// standalone counterpart that does both, for callers (tests, the inherent
/// `GoProducer::lower_bytes`/`produce` methods) that want a self-contained
/// entry point instead.
///
/// # Local vs. foreign named types
///
/// Before dispatching to each package, this computes `local`: the set of
/// import paths this oracle invocation actually loaded (and will therefore
/// emit `decls` for). Every named/alias type reference is checked against
/// it (see `crate::go::types::lower_type_with_lowering`) so that a reference to
/// a package outside this set — the Go standard library, or a dependency —
/// routes through `Lowering::refer_import` instead of `Lowering::refer`.
/// Skipping this check made `Lowering::finish` fail with
/// `LoweringError::Undeclared` on any real Go package that imports so much
/// as `sync` or `time`, i.e. nearly all of them; see `tests/real_package.rs`.
pub fn lower_into(output: &oracle::Output, low: &mut Lowering<GoId>) -> Result<()> {
    let local: HashSet<String> = output
        .packages
        .iter()
        .map(|pkg| pkg.import_path.clone())
        .collect();
    for pkg in &output.packages {
        report_build_constraints(pkg);
        lower_package(pkg, low, &local);
    }
    Ok(())
}

/// Surface declarations that the active Go build intentionally excluded.
///
/// They cannot be lowered as ordinary IR symbols without misrepresenting
/// availability, but keeping this typed diagnostic on the oracle package and
/// reporting it here prevents the source scan from becoming another silent
/// omission at the lowering boundary.
fn report_build_constraints(pkg: &oracle::Package) {
    for file in &pkg.build_constraints {
        for decl in &file.exported_decls {
            eprintln!(
                "[go-oracle] unavailable build-constrained declaration {} in {} ({})",
                decl.name,
                file.file,
                file.constraints.join(" && ")
            );
        }
    }
}

pub(super) fn unbound_name(name: &str, index: usize) -> String {
    // `_` does not bind. A package may declare it any number of times.
    if name.is_empty() || name == "_" {
        format!("_:{index}")
    } else {
        name.to_string()
    }
}

/// Lower all packages in an oracle `Output` into one [`IrPackage`].
///
/// The returned package's root module wraps every Go package as a sub-module.
pub fn lower_output(
    output: &oracle::Output,
    pkg_id: PackageId,
    // `lineage` is reserved for `IrPackage::seal()` — the caller seals when
    // ready; we do not seal here.
    _lineage: &PackageLineageId,
) -> Result<IrPackage<GoId>> {
    let root_sym = sym_for("(root)", "", true, None, None);
    let mut low: Lowering<GoId> = Lowering::new(pkg_id, root_sym);

    lower_into(output, &mut low)?;

    low.finish()
        .map_err(|e| error::Error::Lowering(Box::new(e)))
}

/// Lower a single oracle package into the `Lowering` sink.
fn lower_package(pkg: &oracle::Package, low: &mut Lowering<GoId>, local: &HashSet<String>) {
    let pkg_id = GoId::Package {
        import_path: pkg.import_path.clone(),
    };

    let pkg_sym = sym_for(&pkg.import_path, &pkg.doc, true, None, None);
    // Declare the package as a Module, child of root (None).
    low.declare(pkg_id.clone(), None, pkg_sym, Module);

    let enums = detect_iota_enums(pkg);

    for (index, decl) in pkg.decls.iter().enumerate() {
        lower_decl(pkg, decl, index, &enums, low, local);
    }
    types::lower_unresolved_cgo(pkg, pkg_id, low);
    record_references(pkg, low);
}

fn record_references(pkg: &oracle::Package, low: &mut Lowering<GoId>) {
    for reference in &pkg.references {
        let Some(owner_decl) = pkg
            .decls
            .iter()
            .find(|decl| decl.kind == DeclKind::Func && decl.name == reference.owner)
        else {
            continue;
        };
        let Some(owner_span) = owner_decl.span.as_ref() else {
            continue;
        };
        if owner_decl
            .pos
            .as_ref()
            .is_none_or(|pos| pos.file != reference.file)
            || reference.start < owner_span.start as usize
            || reference.end > owner_span.end as usize
        {
            continue;
        }
        low.record_occurrence(
            GoId::Item {
                import_path: pkg.import_path.clone(),
                name: reference.owner.clone(),
            },
            GoId::Item {
                import_path: pkg.import_path.clone(),
                name: reference.target.clone(),
            },
            ReferenceKind::FunctionCall,
            Confidence::Oracle,
            RelSpan::new(
                u32::try_from(reference.start.saturating_sub(owner_span.start as usize))
                    .unwrap_or(u32::MAX),
                u32::try_from(reference.end.saturating_sub(owner_span.start as usize))
                    .unwrap_or(u32::MAX),
            ),
        );
    }
}

/// Lower one package-level declaration.
fn lower_decl(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    index: usize,
    enums: &IotaEnums<'_>,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let parent_pkg = GoId::Package {
        import_path: pkg.import_path.clone(),
    };

    match decl.kind {
        DeclKind::Type => lower_type_decl(pkg, decl, enums, parent_pkg, low, local),
        DeclKind::Alias => values::lower_alias(pkg, decl, parent_pkg, low, local),
        DeclKind::Func => values::lower_func(pkg, decl, parent_pkg, low, local),
        DeclKind::Const => {
            // Skip this const only when it is one of the iota variants.
            // Matching on the name dropped every other `_` in the package.
            if !is_enum_variant(enums, decl) {
                values::lower_const(pkg, decl, index, parent_pkg, low, local);
            }
        }
        DeclKind::Var => values::lower_var(pkg, decl, index, parent_pkg, low, local),
    }
}

fn is_enum_variant(enums: &IotaEnums<'_>, decl: &oracle::Decl) -> bool {
    enums
        .variants_by_type
        .values()
        .flatten()
        .any(|variant| std::ptr::eq(*variant, decl))
}

// ---------------------------------------------------------------------------
// Type declarations
// ---------------------------------------------------------------------------

fn lower_type_decl(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    enums: &IotaEnums<'_>,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };

    // ── iota enum ────────────────────────────────────────────────────────────
    if let Some(variants) = enums.variants_by_type.get(decl.name.as_str()) {
        types::lower_iota_enum(pkg, decl, variants, item_id, parent, low, local);
        return;
    }

    // ── struct ───────────────────────────────────────────────────────────────
    match decl.underlying.as_ref().map(|u| u.kind) {
        Some(TypeKind::Struct) => types::lower_struct(pkg, decl, item_id, parent, low, local),
        Some(TypeKind::Interface) => types::lower_interface(pkg, decl, item_id, parent, low, local),
        _ => types::lower_newtype(pkg, decl, item_id, parent, low, local),
    }
}

// ---------------------------------------------------------------------------
// Method lowering (shared by struct / newtype / iota-enum)
// ---------------------------------------------------------------------------

fn lower_methods(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: &GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    // A promoted method's `GoId::Member` carries its defining type
    // (`promoted_from`), so it can never collide with a local field or
    // directly-declared method of the same spelling — those always keep
    // `promoted_from: None`. See `GoId::Member`'s doc for the Go-spec
    // background (the real case that motivated this:
    // `google.golang.org/grpc/xds`'s `serverOption`, which declares its own
    // field `apply` and also promotes an embedded `apply` method).
    //
    // Two *different* promoted methods can only collide under the same id if
    // they share both `member_name` and `promoted_from` — i.e. the oracle
    // reported the same (name, origin) pair twice. `go/types.NewMethodSet`
    // (`oracle/go/serialize.go`'s `promotedMethods`) already returns at most
    // one entry per name — same-depth ties are ambiguous selectors and
    // excluded, and a deeper shadowed candidate is never reached — so
    // `decl.promoted_methods` cannot itself contain such a duplicate. No
    // dedup pass is needed here; `Lowering::finish` still catches it as
    // `Error::Duplicate` if that invariant is ever violated.
    for method in &decl.methods {
        lower_one_method(pkg, decl, method, false, parent, low, local);
    }
    for method in &decl.promoted_methods {
        lower_one_method(pkg, decl, method, true, parent, low, local);
    }
}

fn lower_one_method(
    pkg: &oracle::Package,
    type_decl: &oracle::Decl,
    method: &oracle::Method,
    is_promoted: bool,
    parent: &GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let mid = GoId::Member {
        import_path: pkg.import_path.clone(),
        type_name: type_decl.name.clone(),
        member_name: method.name.clone(),
        promoted_from: is_promoted.then(|| method.origin.clone()),
    };

    let doc = if is_promoted {
        let provenance = if method.origin.is_empty() {
            "Promoted from an embedded field.".to_string()
        } else {
            format!("Promoted from embedded `{}`.", method.origin)
        };
        if method.doc.is_empty() {
            provenance
        } else {
            format!("{}\n\n{provenance}", method.doc)
        }
    } else {
        method.doc.clone()
    };

    let mut msym = sym_for(
        &method.name,
        &doc,
        method.exported,
        method.pos.as_ref(),
        method.span.as_ref(),
    );
    // Populate the four search-entry-point aliases so the method can be
    // found by short name, dotted form, and fully-qualified variants.
    msym.aliases = method_aliases(&pkg.import_path, &type_decl.name, &method.name);
    let receiver = go_types::lower_receiver(method.pointer_recv);

    let (input_refs, output_refs) = lower_sig_params_into_lowering(
        pkg,
        &type_decl.name,
        &method.name,
        is_promoted.then_some(method.origin.as_str()),
        method.signature.as_ref(),
        low,
        local,
    );

    let fn_kind = Function::builder()
        .receiver(receiver)
        .input_params(input_refs)
        .output_params(output_refs)
        .build();

    low.declare(mid, Some(parent.clone()), msym, fn_kind);
}

// ---------------------------------------------------------------------------
// Param lowering
// ---------------------------------------------------------------------------

/// Lower a function signature's params and results, declaring each `Param`
/// as a child of the function and returning the Refs for the Function builder.
///
/// Returns `(input_refs, output_refs)`.
///
/// `promoted_from` must be exactly what the caller used to build the
/// function/method's own `GoId::Member` (`None` for a package-level func or
/// a directly-declared/interface method, `Some(origin)` for a promoted
/// method) — `fn_id` below is only ever *referred* to as the params'/
/// results' parent, never itself declared here, so if it disagreed with the
/// id the method was actually `declare`d under, `Lowering::finish` would
/// report every one of its params and results as "referred but never
/// declared" instead of nesting them under the method.
fn lower_sig_params_into_lowering(
    pkg: &oracle::Package,
    type_name: &str,
    fn_name: &str,
    promoted_from: Option<&str>,
    sig: Option<&oracle::Type>,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> (Vec<Ref<Param>>, Vec<Ref<Param>>) {
    let (params, results, variadic) = sig.map_or((&[][..], &[][..], false), |s| {
        (s.params.as_ref(), s.results.as_ref(), s.variadic)
    });

    let fn_id = if type_name.is_empty() {
        GoId::Item {
            import_path: pkg.import_path.clone(),
            name: fn_name.to_string(),
        }
    } else {
        GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: type_name.to_string(),
            member_name: fn_name.to_string(),
            promoted_from: promoted_from.map(str::to_string),
        }
    };

    let last_param = params.len().saturating_sub(1);
    let mut input_refs: Vec<Ref<Param>> = Vec::with_capacity(params.len());
    for (i, p) in params.iter().enumerate() {
        // Go's blank identifier `_` is a legal, non-unique parameter name —
        // `func (h) Handle(_ Context, _ Record) error` is ordinary Go, and a
        // signature may repeat `_` any number of times because none of them
        // bind. Treating it as distinct from "no name at all" (the
        // `is_empty()` branch below) is what let two blank params collide
        // under the identical id `Member{.., member_name: "param:_"}` and
        // fail `Lowering::finish` as a duplicate declare on real signatures
        // (github.com/spf13/viper's `discardHandler.Handle`,
        // github.com/redis/go-redis/v9's `cscEvictOnRemoveHook.OnGet`,
        // github.com/google/go-cmp's `filter` methods all declare `_` two or
        // more times). Both "" and "_" mean "unnamed" in Go, so both must
        // fall back to the positional index for id uniqueness.
        let is_blank = p.name.is_empty() || p.name == "_";
        let param_id = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: format!("{type_name}::{fn_name}"),
            member_name: format!(
                "param:{}",
                if is_blank {
                    i.to_string()
                } else {
                    p.name.clone()
                }
            ),
            promoted_from: None,
        };
        let is_last_variadic = variadic && i == last_param;
        let attrs: Vec<ParamAttribute> = if is_last_variadic {
            vec![ParamAttribute::Variadic]
        } else {
            vec![]
        };
        // Use lower_type_with_lowering so named param types produce Nominal refs.
        let ty = p
            .r#type
            .as_ref()
            .map(|t| go_types::lower_type_with_lowering(t, low, local));
        let pname = if is_blank {
            format!("_{i}")
        } else {
            p.name.clone()
        };
        let ppos = (!is_blank).then_some(p.pos.as_ref()).flatten();
        let psym = sym_for(
            &pname,
            "",
            true,
            ppos,
            ppos.map(|pos| oracle::Span {
                start: pos.offset,
                end: pos.offset + p.name.len().max(1) as i64,
            })
            .as_ref(),
        );
        let param_kind = Param::builder().maybe_ty(ty).attributes(attrs).build();
        input_refs.push(low.refer(param_id.clone()));
        let location = if is_blank {
            SourceLocation::Unlocated(Unlocated::Synthesized)
        } else {
            SourceLocation::from_legacy(&psym.source, &psym.span)
        };
        low.declare_at(param_id, Some(fn_id.clone()), psym, param_kind, location);
    }

    let mut output_refs: Vec<Ref<Param>> = Vec::with_capacity(results.len());
    for (i, r) in results.iter().enumerate() {
        // Same "_ is not a name" reasoning as the param loop above — a named
        // return list can repeat `_` too (`func f() (_ int, _ error)`).
        let is_blank = r.name.is_empty() || r.name == "_";
        let result_id = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: format!("{type_name}::{fn_name}"),
            member_name: format!(
                "result:{}",
                if is_blank {
                    i.to_string()
                } else {
                    r.name.clone()
                }
            ),
            promoted_from: None,
        };
        // Use lower_type_with_lowering so named result types produce Nominal refs.
        let ty = r
            .r#type
            .as_ref()
            .map(|t| go_types::lower_type_with_lowering(t, low, local));
        let rname = if is_blank {
            format!("_{i}")
        } else {
            r.name.clone()
        };
        let rpos = (!is_blank).then_some(r.pos.as_ref()).flatten();
        let rsym = sym_for(
            &rname,
            "",
            true,
            rpos,
            rpos.map(|pos| oracle::Span {
                start: pos.offset,
                end: pos.offset + r.name.len().max(1) as i64,
            })
            .as_ref(),
        );
        let result_kind = Param::builder().maybe_ty(ty).build();
        output_refs.push(low.refer(result_id.clone()));
        let location = if is_blank {
            SourceLocation::Unlocated(Unlocated::Synthesized)
        } else {
            SourceLocation::from_legacy(&rsym.source, &rsym.span)
        };
        low.declare_at(result_id, Some(fn_id.clone()), rsym, result_kind, location);
    }

    (input_refs, output_refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_parameter_position_becomes_a_real_span() {
        let pos = oracle::Pos {
            file: "fixture.go".to_owned(),
            line: 1,
            col: 10,
            offset: 9,
        };
        let span = oracle::Span {
            start: pos.offset,
            end: pos.offset + 3,
        };
        let sym = sym_for("arg", "", true, Some(&pos), Some(&span));
        assert_eq!(sym.source, PathBuf::from("fixture.go"));
        assert_eq!(sym.span, 9..12);
    }
}
