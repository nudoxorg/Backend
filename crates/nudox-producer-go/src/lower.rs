//! Flat, one-pass lowering of oracle output into `Lowering<GoId>`.
//!
//! Each oracle [`Package`] becomes a flat sequence of `Lowering::declare` and
//! `Lowering::refer` calls.  Because `Lowering::refer` works before the target
//! is declared, there is **no intermediate tree and no path map** — the single
//! biggest simplification over the old code.
//!
//! ## Layout inside the IR
//!
//! ```text
//! root (Module)
//! └── pkg "example.com/m/foo" (Module)
//!     ├── Server (Record)
//!     │   ├── Name (Field)
//!     │   └── Handle (Function)   ← method, parent = Server
//!     ├── Handler (Alias)
//!     └── MaxConns (Const)
//! ```
//!
//! Methods are declared as children of their receiver type.  The `Lowering`
//! sink wires the parent pointer; no second pass is needed.

use std::path::PathBuf;

// `build::*` re-exports everything needed from nudox_ir: Symbol, Visibility,
// all kinds (Alias/Const/Enum/Field/Function/…), Type, Lowering, IrPackage,
// PackageId, PackageLineageId, and more.
use nudox_ir::build::*;

use crate::{
    error::{GoError, Result},
    oracle::{self, DeclKind, TypeKind},
    types,
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
    /// Every const name consumed as a variant (so we skip them in const loop).
    variant_names: std::collections::HashSet<&'a str>,
}

fn detect_iota_enums<'a>(pkg: &'a oracle::Package) -> IotaEnums<'a> {
    // Which defined types exist?
    let defined_types: std::collections::HashMap<&str, &oracle::Decl> = pkg
        .decls
        .iter()
        .filter(|d| d.kind == DeclKind::Type)
        .map(|d| (d.name.as_str(), d))
        .collect();

    let mut variants_by_type: std::collections::HashMap<&str, Vec<&oracle::Decl>> =
        std::collections::HashMap::new();

    for decl in pkg.decls.iter() {
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
            .map(|u| !matches!(u.kind, TypeKind::Struct | TypeKind::Interface))
            .unwrap_or(false);
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
                .map(|p| (p.file.as_str(), p.line))
                .unwrap_or(("", 0))
        });
    }

    let variant_names: std::collections::HashSet<&str> = variants_by_type
        .values()
        .flatten()
        .map(|d| d.name.as_str())
        .collect();

    IotaEnums {
        variants_by_type,
        variant_names,
    }
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
        if prev_blank {
            if let Some(rest) = trimmed.strip_prefix("Deprecated:") {
                in_deprecated_para = true;
                let note = rest.trim();
                if !note.is_empty() {
                    deprecated_lines.push(note);
                }
                prev_blank = false;
                continue;
            }
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
    })
}

// ---------------------------------------------------------------------------
// Symbol helpers
// ---------------------------------------------------------------------------

/// Build a [`Symbol`] for a declaration, parsing the doc string for
/// deprecation notices and link definitions.
fn sym_for(name: &str, doc: &str, exported: bool, pos: Option<&oracle::Pos>) -> Symbol {
    let vis = if exported {
        Visibility::Public
    } else {
        Visibility::Package
    };
    let source = pos.map(|p| PathBuf::from(&p.file)).unwrap_or_default();
    // Span: we only have line/col from the oracle, not byte offsets.
    // Store 0..0 — downstream consumers that need byte spans will re-parse.
    let (deprecation, doc_links) = parse_doc(doc);
    Symbol {
        name: name.to_owned(),
        visibility: vis,
        documentation: doc.to_owned(),
        source,
        span: 0..0,
        aliases: Box::new([]),
        deprecation,
        doc_links,
        attrs: Box::new([]),
        cfg: None,
    }
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
    let root_sym = sym_for("(root)", "", true, None);
    let mut low: Lowering<GoId> = Lowering::new(pkg_id, root_sym);

    for pkg in output.packages.iter() {
        lower_package(pkg, &mut low)?;
    }

    low.finish().map_err(|e| match e {
        LoweringError::Undeclared(ids) => GoError::Lowering {
            detail: format!("referred but never declared: {ids:?}"),
        },
        LoweringError::Duplicate(ids) => GoError::Lowering {
            detail: format!("declared more than once: {ids:?}"),
        },
        LoweringError::Cycle(ids) => GoError::Lowering {
            detail: format!("parent-pointer cycle: {ids:?}"),
        },
    })
}

/// Lower a single oracle package into the `Lowering` sink.
fn lower_package(pkg: &oracle::Package, low: &mut Lowering<GoId>) -> Result<()> {
    let pkg_id = GoId::Package {
        import_path: pkg.import_path.clone(),
    };

    let pkg_sym = sym_for(&pkg.import_path, &pkg.doc, true, None);
    // Declare the package as a Module, child of root (None).
    low.declare(pkg_id.clone(), None, pkg_sym, Module);

    let enums = detect_iota_enums(pkg);

    for decl in pkg.decls.iter() {
        lower_decl(pkg, decl, &enums, low)?;
    }

    Ok(())
}

/// Lower one package-level declaration.
fn lower_decl(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    enums: &IotaEnums<'_>,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let parent_pkg = GoId::Package {
        import_path: pkg.import_path.clone(),
    };

    match decl.kind {
        DeclKind::Type => lower_type_decl(pkg, decl, enums, parent_pkg, low),
        DeclKind::Alias => lower_alias(pkg, decl, parent_pkg, low),
        DeclKind::Func => lower_func(pkg, decl, parent_pkg, low),
        DeclKind::Const => {
            // Skip constants that were consumed as enum variants.
            if !enums.variant_names.contains(decl.name.as_str()) {
                lower_const(pkg, decl, parent_pkg, low)?;
            }
            Ok(())
        }
        DeclKind::Var => lower_var(pkg, decl, parent_pkg, low),
    }
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
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };

    // ── iota enum ────────────────────────────────────────────────────────────
    if let Some(variants) = enums.variants_by_type.get(decl.name.as_str()) {
        return lower_iota_enum(pkg, decl, variants, item_id, parent, low);
    }

    // ── struct ───────────────────────────────────────────────────────────────
    match decl.underlying.as_ref().map(|u| u.kind) {
        Some(TypeKind::Struct) => lower_struct(pkg, decl, item_id, parent, low),
        Some(TypeKind::Interface) => lower_interface(pkg, decl, item_id, parent, low),
        _ => lower_newtype(pkg, decl, item_id, parent, low),
    }
}

// ---------------------------------------------------------------------------
// Struct
// ---------------------------------------------------------------------------

fn lower_struct(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    // Collect field Refs (forward-refer them — declare will happen below).
    let underlying = decl.underlying.as_ref();
    let fields_slice = underlying.map(|u| u.fields.as_ref()).unwrap_or(&[]);

    let mut field_refs: Vec<Ref<Field>> = Vec::with_capacity(fields_slice.len());
    for f in fields_slice {
        let fid = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            member_name: f.name.clone(),
        };
        field_refs.push(low.refer(fid));
    }

    // Generics.
    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low))
        .collect();

    // Item 3: Populate super_types from the oracle's `implements` list.
    // Each entry is a Named/Alias type ref — lower_type_with_lowering produces
    // Type::Nominal(RawRef) for it, giving us a live edge in the type graph.
    let super_types: Vec<Type> = decl
        .implements
        .iter()
        .map(|iface_ty| types::lower_type_with_lowering(iface_ty, low))
        .collect();

    let record = Record::builder()
        .form(RecordForm::Struct)
        .fields(field_refs)
        .generics(generics)
        .super_types(super_types)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, record);

    // Declare fields as children of the struct.
    for f in fields_slice {
        let fid = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            member_name: f.name.clone(),
        };
        let fdoc = decl
            .field_docs
            .get(&f.name)
            .map(|s| s.as_str())
            .unwrap_or("");
        let mut fsym = sym_for(&f.name, fdoc, f.exported, None);

        // Item 5: Struct field tags and embedded markers.
        // Tags (`json:"name,omitempty"`) are real API surface and go into attrs.
        // Embedded fields are marked with an "embedded" AttrTok.
        let mut attrs: Vec<AttrTok> = Vec::new();
        if f.embedded {
            attrs.push(AttrTok {
                token: "embedded".to_string(),
                arg: None,
            });
        }
        if !f.tag.is_empty() {
            attrs.push(AttrTok {
                token: "tag".to_string(),
                arg: Some(f.tag.clone()),
            });
        }
        fsym.attrs = attrs.into_boxed_slice();

        // Go struct fields are always assignable: mark Mutable.
        let mut field_attrs = vec![FieldAttribute::Mutable];
        // Note: FieldAttribute has no Embedded variant; we use AttrTok above.
        // If a dedicated FieldAttribute::Embedded is added to nudox-ir, the
        // AttrTok approach should be replaced.
        if f.embedded {
            // Keep the embedded marker only in attrs (above); FieldAttribute
            // has no Embedded variant in the current IR.
            let _ = &mut field_attrs; // suppress unused-mut warning
        }

        // Field type: use lower_type_with_lowering so named types produce
        // Nominal refs rather than Any.
        let fty = f
            .r#type
            .as_ref()
            .map(|t| types::lower_type_with_lowering(t, low));

        let field_kind = Field::builder()
            .key(FieldKey::Named)
            .maybe_ty(fty)
            .attributes(field_attrs)
            .build();

        low.declare(fid, Some(item_id.clone()), fsym, field_kind);
    }

    // Declare methods as children of the struct.
    lower_methods(pkg, decl, &item_id, low)
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

fn lower_interface(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let mut sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low))
        .collect();

    // Item 7a: surface IsComparable as an AttrTok.
    // The underlying interface type carries `is_comparable`.
    let is_comparable = decl
        .underlying
        .as_ref()
        .map(|u| u.is_comparable)
        .unwrap_or(false);
    if is_comparable {
        // AttrTok does not derive Clone; build the new list from scratch.
        // (sym.attrs is empty for freshly-constructed symbols, so we know the
        // previous list had no prior attrs — if it ever gains entries, extend here.)
        sym.attrs = Box::new([AttrTok {
            token: "comparable".to_string(),
            arg: None,
        }]);
    }

    let trait_kind = Trait::builder()
        .flags(TraitFlags::default())
        .generics(generics)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, trait_kind);

    // Interface methods become Function children of the Trait entry.
    let underlying = decl.underlying.as_ref();
    if let Some(iface) = underlying {
        // Use the full expanded method set (all_methods) so inherited methods
        // are also declared.
        for sig in iface.all_methods.iter() {
            let mid = GoId::Member {
                import_path: pkg.import_path.clone(),
                type_name: decl.name.clone(),
                member_name: sig.name.clone(),
            };
            let mdoc = decl
                .method_docs
                .get(&sig.name)
                .map(|s| s.as_str())
                .unwrap_or(&sig.pkg);
            // Explicit methods: use method_docs; inherited: note the source pkg.
            let actual_doc = if !sig.pkg.is_empty() && sig.pkg != pkg.import_path {
                format!(
                    "{mdoc}\n\nInherited via embedded interface (declared in `{}`).",
                    sig.pkg
                )
            } else {
                mdoc.to_owned()
            };

            let msym = sym_for(&sig.name, &actual_doc, sig.exported, sig.pos.as_ref());

            let (input_refs, output_refs) = lower_sig_params_into_lowering(
                pkg,
                &decl.name,
                &sig.name,
                sig.signature.as_ref(),
                low,
            );

            let fn_kind = Function::builder()
                // Interface methods have no receiver in the IR sense (they are
                // requirements, not implementations).
                .maybe_receiver(None)
                .input_params(input_refs)
                .output_params(output_refs)
                .is_defaulted(false)
                .build();

            low.declare(mid, Some(item_id.clone()), msym, fn_kind);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Newtype (defined type over non-struct, non-interface underlying)
// ---------------------------------------------------------------------------

fn lower_newtype(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    // Forward-ref the single "inner" field.
    let inner_id = GoId::Member {
        import_path: pkg.import_path.clone(),
        type_name: decl.name.clone(),
        member_name: "(inner)".to_string(),
    };
    let inner_ref: Ref<Field> = low.refer(inner_id.clone());

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low))
        .collect();

    let record = Record::builder()
        .form(RecordForm::Tuple) // single-field tuple-struct analogue
        .fields([inner_ref])
        .generics(generics)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, record);

    // Declare the single inner field using lower_type_with_lowering so
    // named underlying types produce Nominal refs.
    let underlying_ty = decl
        .underlying
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low));
    let inner_sym = sym_for("(inner)", "(underlying type field)", decl.exported, None);
    let inner_field = Field::builder()
        .key(FieldKey::Positional(0))
        .maybe_ty(underlying_ty)
        .build();
    low.declare(inner_id, Some(item_id.clone()), inner_sym, inner_field);

    // Methods.
    lower_methods(pkg, decl, &item_id, low)
}

// ---------------------------------------------------------------------------
// Iota enum
// ---------------------------------------------------------------------------

fn lower_iota_enum(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    variants: &[&oracle::Decl],
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    // Forward-refer variants.
    let mut variant_refs: Vec<Ref<Variant>> = Vec::with_capacity(variants.len());
    for v in variants.iter() {
        let vid = GoId::Variant {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            variant_name: v.name.clone(),
        };
        variant_refs.push(low.refer(vid));
    }

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low))
        .collect();

    let enum_kind = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, enum_kind);

    // Declare each variant.
    // Item 7b: discriminant values are stored as raw oracle strings.
    // The oracle emits `constant.Value.ExactString()` which for integer iota
    // constants is the decimal string ("0", "1", …).  A structured ConstExpr
    // would require a `ConstExpr` enum in nudox-ir (int / float / bool / str /
    // rational / var) — the old workspace/compiler/compile/go/types.rs had
    // `parse_const_value` that produced such a struct.  Until nudox-ir adds
    // `ConstExpr`, we store the best-available rendering: the raw string.
    for v in variants.iter() {
        let vid = GoId::Variant {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            variant_name: v.name.clone(),
        };
        let vsym = sym_for(&v.name, &v.doc, v.exported, v.pos.as_ref());
        // The value, if present, as a string discriminant.
        let discr = if v.value.is_empty() {
            None
        } else {
            Some(v.value.clone())
        };
        let variant_kind = Variant::builder()
            .form(VariantForm::Unit)
            .maybe_discr(discr)
            .build();
        low.declare(vid, Some(item_id.clone()), vsym, variant_kind);
    }

    // Methods.
    lower_methods(pkg, decl, &item_id, low)
}

// ---------------------------------------------------------------------------
// Alias (`type A = B`)
// ---------------------------------------------------------------------------

fn lower_alias(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    // Use lower_type_with_lowering so named alias targets produce Nominal refs.
    let target = decl
        .target
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low));
    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low))
        .collect();

    let alias_kind = Alias::builder()
        .maybe_target(target)
        .generics(generics)
        .build();

    low.declare(item_id, Some(parent), sym, alias_kind);
    Ok(())
}

// ---------------------------------------------------------------------------
// Package-level function
// ---------------------------------------------------------------------------

fn lower_func(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    let (input_refs, output_refs) =
        lower_sig_params_into_lowering(pkg, "", &decl.name, decl.signature.as_ref(), low);

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low))
        .collect();

    // Variadic flag is captured on the last Param via ParamAttribute::Variadic
    // (handled in lower_sig_params_into_lowering).  No dedicated FnModifier
    // exists for variadic in the new IR.
    let fn_kind = Function::builder()
        .maybe_receiver(None)
        .input_params(input_refs)
        .output_params(output_refs)
        .generics(generics)
        .build();

    low.declare(item_id, Some(parent), sym, fn_kind);
    Ok(())
}

// ---------------------------------------------------------------------------
// Constant
// ---------------------------------------------------------------------------

fn lower_const(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    // Use lower_type_with_lowering so named const types produce Nominal refs.
    let ty = decl
        .r#type
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low))
        .unwrap_or(Type::Any);

    // Item 7b: const values stored as raw oracle strings (ExactString from
    // go/constant).  A structured ConstExpr would need nudox-ir support.
    let value = if decl.value.is_empty() {
        None
    } else {
        Some(decl.value.clone())
    };

    let const_kind = Const::builder().ty(ty).maybe_value(value).build();
    low.declare(item_id, Some(parent), sym, const_kind);
    Ok(())
}

// ---------------------------------------------------------------------------
// Variable
// ---------------------------------------------------------------------------

fn lower_var(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref());

    // Use lower_type_with_lowering so named var types produce Nominal refs.
    let ty = decl
        .r#type
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low))
        .unwrap_or(Type::Any);

    let static_kind = Static::builder().ty(ty).mutable(true).build();
    low.declare(item_id, Some(parent), sym, static_kind);
    Ok(())
}

// ---------------------------------------------------------------------------
// Method lowering (shared by struct / newtype / iota-enum)
// ---------------------------------------------------------------------------

fn lower_methods(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: &GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    for method in decl.methods.iter() {
        lower_one_method(pkg, decl, method, false, parent, low)?;
    }
    for method in decl.promoted_methods.iter() {
        lower_one_method(pkg, decl, method, true, parent, low)?;
    }
    Ok(())
}

fn lower_one_method(
    pkg: &oracle::Package,
    type_decl: &oracle::Decl,
    method: &oracle::Method,
    is_promoted: bool,
    parent: &GoId,
    low: &mut Lowering<GoId>,
) -> Result<()> {
    let mid = GoId::Member {
        import_path: pkg.import_path.clone(),
        type_name: type_decl.name.clone(),
        member_name: method.name.clone(),
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

    let mut msym = sym_for(&method.name, &doc, method.exported, method.pos.as_ref());
    // Populate the four search-entry-point aliases so the method can be
    // found by short name, dotted form, and fully-qualified variants.
    msym.aliases = method_aliases(&pkg.import_path, &type_decl.name, &method.name);
    let receiver = types::lower_receiver(method.pointer_recv);

    let (input_refs, output_refs) = lower_sig_params_into_lowering(
        pkg,
        &type_decl.name,
        &method.name,
        method.signature.as_ref(),
        low,
    );

    let fn_kind = Function::builder()
        .receiver(receiver)
        .input_params(input_refs)
        .output_params(output_refs)
        .build();

    low.declare(mid, Some(parent.clone()), msym, fn_kind);
    Ok(())
}

// ---------------------------------------------------------------------------
// Param lowering
// ---------------------------------------------------------------------------

/// Lower a function signature's params and results, declaring each `Param`
/// as a child of the function and returning the Refs for the Function builder.
///
/// Returns `(input_refs, output_refs)`.
fn lower_sig_params_into_lowering(
    pkg: &oracle::Package,
    type_name: &str,
    fn_name: &str,
    sig: Option<&oracle::Type>,
    low: &mut Lowering<GoId>,
) -> (Vec<Ref<Param>>, Vec<Ref<Param>>) {
    let (params, results, variadic) = sig
        .map(|s| (s.params.as_ref(), s.results.as_ref(), s.variadic))
        .unwrap_or((&[], &[], false));

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
        }
    };

    let last_param = params.len().saturating_sub(1);
    let mut input_refs: Vec<Ref<Param>> = Vec::with_capacity(params.len());
    for (i, p) in params.iter().enumerate() {
        let param_id = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: format!("{type_name}::{fn_name}"),
            member_name: format!(
                "param:{}",
                if p.name.is_empty() {
                    i.to_string()
                } else {
                    p.name.clone()
                }
            ),
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
            .map(|t| types::lower_type_with_lowering(t, low));
        let pname = if p.name.is_empty() {
            format!("_{i}")
        } else {
            p.name.clone()
        };
        let psym = sym_for(&pname, "", true, None);
        let param_kind = Param::builder().maybe_ty(ty).attributes(attrs).build();
        input_refs.push(low.refer(param_id.clone()));
        low.declare(param_id, Some(fn_id.clone()), psym, param_kind);
    }

    let mut output_refs: Vec<Ref<Param>> = Vec::with_capacity(results.len());
    for (i, r) in results.iter().enumerate() {
        let result_id = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: format!("{type_name}::{fn_name}"),
            member_name: format!(
                "result:{}",
                if r.name.is_empty() {
                    i.to_string()
                } else {
                    r.name.clone()
                }
            ),
        };
        // Use lower_type_with_lowering so named result types produce Nominal refs.
        let ty = r
            .r#type
            .as_ref()
            .map(|t| types::lower_type_with_lowering(t, low));
        let rname = if r.name.is_empty() {
            format!("_{i}")
        } else {
            r.name.clone()
        };
        let rsym = sym_for(&rname, "", true, None);
        let result_kind = Param::builder().maybe_ty(ty).build();
        output_refs.push(low.refer(result_id.clone()));
        low.declare(result_id, Some(fn_id.clone()), rsym, result_kind);
    }

    (input_refs, output_refs)
}
