//! Drive libclang to extract fully-owned data into a [`ClangOracle`].
//!
//! This module contains everything that touches the `clang` crate.  By the
//! time [`extract_tu`] returns, all libclang objects have been dropped and
//! only owned Rust values survive.

use std::path::{Path, PathBuf};

use clang::diagnostic::Severity;
use clang::{
    Accessibility, Entity, EntityKind, Index, StorageClass, TranslationUnit, Type as CType,
    TypeKind, Unsaved,
};

use crate::oracle::{
    ClangOracle, OracleAlias, OracleDiagnostic, OracleEnum, OracleField, OracleFnMod,
    OracleFunction, OracleGenericParam, OracleNamespace, OracleParam, OracleReceiver, OracleRecord,
    OracleType, OracleVar, OracleVariant, OracleVisibility, Reference, Usr,
};

// ── Public entry points ───────────────────────────────────────────────────────

/// Parse a real file and preserve a libclang parse failure for callers that
/// require an explicit authority result.
///
/// # Errors
/// Returns the native parser error when libclang cannot build a translation unit.
pub(crate) fn extract_file_checked(
    index: &Index<'_>,
    path: &Path,
    args: &[&str],
) -> Result<ClangOracle, String> {
    let mut parser = index.parser(path);
    parser.arguments(args);
    parser.detailed_preprocessing_record(true);
    let tu = parser.parse().map_err(|error| error.to_string())?;
    Ok(extract_tu(&tu, path))
}

/// Parse an in-memory source string while preserving the native parser error.
///
/// # Errors
/// Returns the native parser error when libclang cannot build a translation unit.
pub(crate) fn extract_unsaved_checked(
    index: &Index<'_>,
    virtual_path: &Path,
    source: &str,
    args: &[&str],
) -> Result<ClangOracle, String> {
    let unsaved = [Unsaved::new(virtual_path, source)];
    let mut parser = index.parser(virtual_path);
    parser.arguments(args);
    parser.unsaved(&unsaved);
    parser.detailed_preprocessing_record(true);
    let tu = parser.parse().map_err(|error| error.to_string())?;
    Ok(extract_tu(&tu, virtual_path))
}

// ── TU extraction ─────────────────────────────────────────────────────────────

fn extract_tu(tu: &TranslationUnit<'_>, main_file: &Path) -> ClangOracle {
    let mut oracle = ClangOracle::default();
    oracle.diagnostics = tu
        .get_diagnostics()
        .into_iter()
        .filter_map(|diagnostic| {
            let location = diagnostic.get_location().get_file_location();
            let source_file = location.file.map(|file| file.get_path())?;
            let severity = match diagnostic.get_severity() {
                Severity::Ignored => "ignored",
                Severity::Note => "note",
                Severity::Warning => "warning",
                Severity::Error => "error",
                Severity::Fatal => "fatal",
            };
            Some(OracleDiagnostic {
                severity: severity.to_owned(),
                message: diagnostic.get_text(),
                source_file,
                byte_start: location.offset as usize,
                byte_end: location.offset as usize,
            })
        })
        .collect();
    let root = tu.get_entity();
    for child in root.get_children() {
        visit_entity(&mut oracle, child, None, main_file);
    }
    oracle
}

/// Whether `entity` is lexically written in `main_file`, resolved the way a
/// caller actually means it: "physically part of the file I asked libclang
/// to parse", not "reachable via `clang_Location_isFromMainFile`".
///
/// # Why not `Entity::is_in_main_file`
///
/// `clang::Entity::is_in_main_file` (this crate's wrapper around
/// `clang_Location_isFromMainFile`) is evaluated on the cursor's raw extent
/// start, which for any declaration whose *opening token* comes from a macro
/// expansion is still a macro `SourceLocation`, not a resolved file location
/// — and libclang's `isFromMainFile` answers `false` for those without
/// resolving through the expansion first. This is not a hypothetical: real
/// C++ libraries routinely open their public namespace through a macro for
/// ABI-tagging (`NLOHMANN_JSON_NAMESPACE_BEGIN` in nlohmann/json,
/// `FMT_BEGIN_NAMESPACE` in fmt, `ABSL_NAMESPACE_BEGIN` in abseil-cpp, …).
/// Verified empirically: `#define NS_BEGIN namespace foo {` then `NS_BEGIN`
/// at top level yields a `Namespace` cursor with `is_in_main_file() ==
/// false`, even though every byte of it is in the file being parsed — while
/// a `struct` written two lines later with no macro involved correctly comes
/// back `true`. Because [`visit_entity`] returns early on a `false` result
/// *before* recursing, one macro-wrapped namespace silently discards its
/// entire subtree — for `nlohmann::json` specifically, this reduced a
/// single-header library with hundreds of public declarations down to the
/// nine names that happened to sit in a macro-free `namespace std { … }`
/// block.
///
/// `entity_file` (used elsewhere in this module for the `source_file` we
/// actually persist) goes through `clang_getFileLocation`, which *does*
/// resolve through macro expansion to a concrete file — confirmed against
/// the same repro to correctly report the main `.cpp` path for the
/// macro-opened namespace. Comparing that resolved file to the file we asked
/// the parser to open is what "in the main file" should have meant all
/// along.
fn is_in_main_file(entity: Entity<'_>, main_file: &Path) -> bool {
    entity_file(entity) == main_file
}

// ── Recursive entity visitor ──────────────────────────────────────────────────

fn visit_entity(
    oracle: &mut ClangOracle,
    entity: Entity<'_>,
    parent_usr: Option<&str>,
    main_file: &Path,
) {
    // Filter to entities lexically defined in the main translation unit file.
    if !is_in_main_file(entity, main_file) {
        return;
    }
    // Macros are preprocessing entities, not always "valid declarations" in
    // libclang's sense — skip `is_invalid_declaration` so they are not
    // dropped. Keep the main-file filter so system/builtin macros stay out.
    // Skip `MacroExpansion` (instantiations) via the catch-all.
    if entity.get_kind() == EntityKind::MacroDefinition {
        visit_macro(oracle, entity, parent_usr);
        return;
    }
    if entity.is_invalid_declaration() {
        return;
    }

    match entity.get_kind() {
        EntityKind::Namespace => visit_namespace(oracle, entity, parent_usr, main_file),
        EntityKind::StructDecl => visit_record(oracle, entity, parent_usr, false, main_file),
        EntityKind::ClassDecl
        | EntityKind::ClassTemplate
        | EntityKind::ClassTemplatePartialSpecialization => {
            visit_record(oracle, entity, parent_usr, false, main_file);
        }
        EntityKind::UnionDecl => visit_record(oracle, entity, parent_usr, true, main_file),
        EntityKind::FunctionDecl | EntityKind::FunctionTemplate => {
            visit_function(oracle, entity, parent_usr, None);
        }
        EntityKind::Method
        | EntityKind::Constructor
        | EntityKind::Destructor
        | EntityKind::ConversionFunction => {
            visit_function(oracle, entity, parent_usr, Some(receiver_kind(entity)));
        }
        EntityKind::EnumDecl => visit_enum(oracle, entity, parent_usr, main_file),
        EntityKind::TypedefDecl | EntityKind::TypeAliasDecl => {
            visit_alias(oracle, entity, parent_usr);
        }
        EntityKind::VarDecl => visit_var(oracle, entity, parent_usr),
        EntityKind::FieldDecl => visit_field(oracle, entity, parent_usr),
        // Linkage-spec blocks (`extern "C" { … }`) — recurse transparently.
        EntityKind::LinkageSpec => {
            for child in entity.get_children() {
                visit_entity(oracle, child, parent_usr, main_file);
            }
        }
        // Everything else we skip without error.
        _ => {}
    }
}

// ── Namespace ─────────────────────────────────────────────────────────────────

fn visit_namespace(
    oracle: &mut ClangOracle,
    entity: Entity<'_>,
    parent_usr: Option<&str>,
    main_file: &Path,
) {
    let name = match entity.get_name() {
        Some(n) if !n.is_empty() => n,
        _ => return, // anonymous namespace
    };
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    let this_usr = usr.clone();
    oracle.namespaces.push(OracleNamespace {
        usr,
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });

    for child in entity.get_children() {
        visit_entity(oracle, child, Some(&this_usr), main_file);
    }
}

// ── Record (struct / class / union) ──────────────────────────────────────────

fn visit_record(
    oracle: &mut ClangOracle,
    entity: Entity<'_>,
    parent_usr: Option<&str>,
    is_union: bool,
    main_file: &Path,
) {
    // Forward declarations (no definition) — skip to avoid duplicates.
    // Partial specializations *are* definitions of a specialized template;
    // extract them even if libclang's definition bit is unset.
    if !entity.is_definition()
        && entity.get_kind() != EntityKind::ClassTemplatePartialSpecialization
    {
        return;
    }

    let name = match entity.get_name() {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    let is_class = matches!(
        entity.get_kind(),
        EntityKind::ClassDecl
            | EntityKind::ClassTemplate
            | EntityKind::ClassTemplatePartialSpecialization
    );

    let children = entity.get_children();
    let generics = generic_params(&children);
    let super_types = base_specifiers(entity, &children);

    oracle.records.push(OracleRecord {
        usr: usr.clone(),
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        is_class,
        is_union,
        generics,
        super_types,
        fields: Vec::new(), // populated below via FieldDecl children
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });

    let this_usr = usr;
    for child in children {
        visit_entity(oracle, child, Some(&this_usr), main_file);
    }
}

// ── Fields ────────────────────────────────────────────────────────────────────

fn visit_field(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
    let name = entity.get_name().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    let ty = entity.get_type().map_or(OracleType::Inferred, resolve_type);

    let is_static = matches!(entity.get_kind(), EntityKind::VarDecl);
    let is_mutable =
        entity.is_mutable() || entity.get_type().is_some_and(|t| !t.is_const_qualified());

    oracle.fields.push(OracleField {
        usr,
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        ty,
        visibility: visibility(entity),
        is_static,
        is_mutable,
        documentation: documentation(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });
}

// ── Functions ─────────────────────────────────────────────────────────────────

fn visit_function(
    oracle: &mut ClangOracle,
    entity: Entity<'_>,
    parent_usr: Option<&str>,
    receiver: Option<OracleReceiver>,
) {
    // Only declarations (not forward-decl-only) unless there is no definition.
    // We emit every overload separately — they each have a distinct USR.
    let name = match entity.get_name().or_else(|| entity.get_display_name()) {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    let children = entity.get_children();
    let generics = generic_params(&children);
    let (params, variadic) = function_params(entity, &children);
    let ret = entity
        .get_result_type()
        .map_or(OracleType::Void, resolve_type);

    let modifiers = fn_modifiers(entity, &children);
    let abi = fn_abi(entity);

    oracle.functions.push(OracleFunction {
        usr: usr.clone(),
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        receiver,
        generics,
        params,
        variadic,
        ret,
        modifiers,
        abi,
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });
    collect_references(oracle, entity, &usr);
}

fn collect_references(oracle: &mut ClangOracle, entity: Entity<'_>, owner: &str) {
    for child in entity.get_children() {
        if matches!(
            child.get_kind(),
            EntityKind::DeclRefExpr | EntityKind::MemberRefExpr | EntityKind::CallExpr
        ) && let Some(target) = child.get_reference()
        {
            let target_kind = target.get_kind();
            if matches!(
                target_kind,
                EntityKind::FunctionDecl
                    | EntityKind::FunctionTemplate
                    | EntityKind::Method
                    | EntityKind::Constructor
                    | EntityKind::Destructor
            ) {
                let target_usr = entity_usr(target);
                if let Some(range) = child.get_range() {
                    let start = range.get_start().get_file_location();
                    let end = range.get_end().get_file_location();
                    if !target_usr.is_empty() {
                        oracle.references.push(Reference {
                            owner: owner.to_owned(),
                            target: target_usr,
                            source_file: entity_file(child),
                            byte_start: start.offset as usize,
                            byte_end: end.offset as usize,
                        });
                    }
                }
            }
        }
        collect_references(oracle, child, owner);
    }
}

fn function_params(entity: Entity<'_>, children: &[Entity<'_>]) -> (Vec<OracleParam>, bool) {
    let mut params = Vec::new();

    if let Some(args) = entity.get_arguments() {
        // The common path: `clang_Cursor_getNumArguments` reports a real
        // count for concrete function/method declarations.
        for arg in args {
            let ty = arg.get_type().map_or(OracleType::Inferred, resolve_type);
            params.push(OracleParam {
                name: arg.get_name().unwrap_or_default(),
                ty,
                is_variadic: false,
                source_file: entity_file(arg),
                byte_offset: entity_offset(arg),
            });
        }
    } else {
        // `entity.get_arguments()` returns `None` for an *uninstantiated*
        // `FunctionTemplate` cursor — libclang's `clang_Cursor_getNumArguments`
        // only answers for concrete declarations, not templates (verified
        // against `template <typename T> T identity(T x)`: the cursor kind
        // is `FunctionTemplate` and the call reports no arguments at all,
        // even though the template plainly has one). The parameters are
        // still real `ParmDecl` children of the cursor, exactly as for a
        // non-template function, so filter for those directly instead.
        for child in children {
            if child.get_kind() == EntityKind::ParmDecl {
                let ty = child.get_type().map_or(OracleType::Inferred, resolve_type);
                params.push(OracleParam {
                    name: child.get_name().unwrap_or_default(),
                    ty,
                    is_variadic: false,
                    source_file: entity_file(*child),
                    byte_offset: entity_offset(*child),
                });
            }
        }
    }

    let variadic = entity.is_variadic();
    (params, variadic)
}

fn receiver_kind(entity: Entity<'_>) -> OracleReceiver {
    match entity.get_kind() {
        EntityKind::Constructor => OracleReceiver::Static,
        EntityKind::Destructor => OracleReceiver::Owned,
        _ if entity.is_static_method() => OracleReceiver::Static,
        _ if entity.is_const_method() => OracleReceiver::SharedRef,
        _ => OracleReceiver::MutRef,
    }
}

fn fn_modifiers(_entity: Entity<'_>, children: &[Entity<'_>]) -> Vec<OracleFnMod> {
    let mut mods = Vec::new();
    for child in children {
        match child.get_kind() {
            EntityKind::PureAttr => mods.push(OracleFnMod::Pure),
            EntityKind::ConstAttr => mods.push(OracleFnMod::Const),
            _ => {}
        }
    }
    mods
}

fn fn_abi(entity: Entity<'_>) -> Option<String> {
    // libclang's Rust bindings don't expose the ABI string directly; infer
    // from the kind if needed.
    let _ = entity;
    None
}

// ── Enums ─────────────────────────────────────────────────────────────────────

fn visit_enum(
    oracle: &mut ClangOracle,
    entity: Entity<'_>,
    parent_usr: Option<&str>,
    main_file: &Path,
) {
    if !entity.is_definition() {
        return;
    }
    let name = match entity.get_name() {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    // C++11 `enum class` or `enum struct` → scoped.
    let is_scoped = entity
        .get_type()
        .is_some_and(|t| t.get_kind() == TypeKind::Enum);

    let this_usr = usr.clone();
    oracle.enums.push(OracleEnum {
        usr,
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        is_scoped,
        variants: Vec::new(), // populated below
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });

    for child in entity.get_children() {
        if child.get_kind() == EntityKind::EnumConstantDecl && is_in_main_file(child, main_file) {
            let vname = child.get_name().unwrap_or_default();
            if vname.is_empty() {
                continue;
            }
            let vusr = entity_usr(child);
            if vusr.is_empty() {
                continue;
            }
            let discr = child.get_enum_constant_value().map(|(signed, _)| signed);
            oracle.variants.push(OracleVariant {
                usr: vusr,
                name: vname,
                source_file: entity_file(child),
                byte_offset: entity_offset(child),
                discr,
                documentation: documentation(child),
                parent_usr: Some(this_usr.clone()),
            });
        }
    }
}

// ── Aliases ───────────────────────────────────────────────────────────────────

fn visit_alias(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
    let name = match entity.get_name() {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    let target = entity
        .get_typedef_underlying_type()
        .or_else(|| entity.get_type())
        .map_or(OracleType::Inferred, resolve_type);

    oracle.aliases.push(OracleAlias {
        usr,
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        target,
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });
}

// ── Variables ─────────────────────────────────────────────────────────────────

fn visit_var(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
    let name = match entity.get_name() {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };
    let usr = entity_usr(entity);
    if usr.is_empty() {
        return;
    }

    let ty = entity.get_type().map_or(OracleType::Inferred, resolve_type);
    let is_const = entity.get_type().is_some_and(|t| t.is_const_qualified());

    oracle.vars.push(OracleVar {
        usr,
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        ty,
        is_const,
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });
}

// ── Macros ────────────────────────────────────────────────────────────────────

fn visit_macro(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
    let name = match entity.get_name() {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };
    // libclang often has no USR for macros; synthesize one so an empty-USR
    // early-return cannot drop them.
    let usr = match entity_usr(entity) {
        usr if !usr.is_empty() => usr,
        _ => format!("c:@macro@{name}"),
    };

    oracle.vars.push(OracleVar {
        usr,
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        ty: OracleType::Inferred,
        is_const: !entity.is_function_like_macro(),
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });
}

// ── Generics / templates ──────────────────────────────────────────────────────

fn generic_params(children: &[Entity<'_>]) -> Vec<OracleGenericParam> {
    let mut params = Vec::new();
    for child in children {
        match child.get_kind() {
            EntityKind::TemplateTypeParameter => {
                let name = child.get_name().unwrap_or_default();
                params.push(OracleGenericParam::Type { name });
            }
            EntityKind::NonTypeTemplateParameter => {
                let name = child.get_name().unwrap_or_default();
                let ty = child
                    .get_type()
                    .map_or_else(|| "auto".to_owned(), |t| t.get_display_name());
                params.push(OracleGenericParam::Const { name, ty });
            }
            EntityKind::TemplateTemplateParameter => {
                let name = child.get_name().unwrap_or_default();
                params.push(OracleGenericParam::Template { name });
            }
            _ => {}
        }
    }
    params
}

fn base_specifiers(entity: Entity<'_>, children: &[Entity<'_>]) -> Vec<OracleType> {
    let _ = entity;
    children
        .iter()
        .filter(|c| c.get_kind() == EntityKind::BaseSpecifier)
        .filter_map(|c| c.get_type().map(resolve_type))
        .collect()
}

// ── Type resolution ───────────────────────────────────────────────────────────

pub(crate) fn resolve_type(ty: CType<'_>) -> OracleType {
    match ty.get_kind() {
        TypeKind::Void => OracleType::Void,
        TypeKind::Bool => OracleType::Bool,

        TypeKind::CharS
        | TypeKind::SChar
        | TypeKind::Short
        | TypeKind::Int
        | TypeKind::Long
        | TypeKind::LongLong => width_int(ty, true),
        TypeKind::CharU
        | TypeKind::UChar
        | TypeKind::WChar
        | TypeKind::UShort
        | TypeKind::UInt
        | TypeKind::ULong
        | TypeKind::ULongLong => width_int(ty, false),
        TypeKind::Char16 => OracleType::Integer {
            signed: false,
            bits: 16,
        },
        TypeKind::Char32 => OracleType::Integer {
            signed: false,
            bits: 32,
        },

        TypeKind::Int128 => OracleType::Integer {
            signed: true,
            bits: 128,
        },

        TypeKind::UInt128 => OracleType::Integer {
            signed: false,
            bits: 128,
        },

        TypeKind::Half | TypeKind::Float16 => OracleType::Float { bits: 16 },
        TypeKind::Float => OracleType::Float { bits: 32 },
        TypeKind::Double => OracleType::Float { bits: 64 },
        TypeKind::LongDouble | TypeKind::Float128 => width_float(ty),

        TypeKind::Pointer => ty
            .get_pointee_type()
            .map_or(OracleType::Inferred, |pointee| {
                if pointee.is_const_qualified() {
                    OracleType::ConstPointer(Box::new(resolve_type(pointee)))
                } else {
                    OracleType::MutPointer(Box::new(resolve_type(pointee)))
                }
            }),
        TypeKind::LValueReference => ty
            .get_pointee_type()
            .or_else(|| ty.get_element_type())
            .map_or(OracleType::Inferred, |referent| {
                let mutable = !referent.is_const_qualified();
                OracleType::LValueRef {
                    mutable,
                    ty: Box::new(resolve_type(referent)),
                }
            }),
        TypeKind::RValueReference => ty
            .get_pointee_type()
            .or_else(|| ty.get_element_type())
            .map_or(OracleType::Inferred, |referent| {
                OracleType::RValueRef(Box::new(resolve_type(referent)))
            }),
        TypeKind::ConstantArray | TypeKind::VariableArray | TypeKind::DependentSizedArray => {
            ty.get_element_type().map_or(OracleType::Inferred, |elem| {
                let len = ty.get_size().unwrap_or(0);
                OracleType::Array {
                    ty: Box::new(resolve_type(elem)),
                    len,
                }
            })
        }
        TypeKind::IncompleteArray => ty.get_element_type().map_or(OracleType::Inferred, |elem| {
            OracleType::Slice(Box::new(resolve_type(elem)))
        }),
        TypeKind::FunctionNoPrototype | TypeKind::FunctionPrototype => {
            let params = ty
                .get_argument_types()
                .unwrap_or_default()
                .into_iter()
                .map(resolve_type)
                .collect();
            let ret = ty.get_result_type().map_or(OracleType::Void, resolve_type);
            OracleType::FnPtr {
                ret: Box::new(ret),
                params,
            }
        }
        TypeKind::Elaborated => ty
            .get_elaborated_type()
            .map_or(OracleType::Inferred, resolve_type),
        TypeKind::Typedef => {
            let name = ty
                .get_typedef_name()
                .unwrap_or_else(|| ty.get_display_name());
            OracleType::Named {
                name,
                usr: named_decl_usr(ty),
                args: Vec::new(),
            }
        }
        TypeKind::Record | TypeKind::Enum => {
            let name = ty
                .get_declaration()
                .and_then(|e| e.get_display_name())
                .unwrap_or_else(|| ty.get_display_name());
            let args = ty
                .get_template_argument_types()
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .map(resolve_type)
                .collect();
            OracleType::Named {
                name,
                usr: named_decl_usr(ty),
                args,
            }
        }
        TypeKind::Unexposed => {
            // Try canonical form first.
            let canonical = ty.get_canonical_type();
            if canonical != ty {
                return resolve_type(canonical);
            }
            // Dependent / template-parameter-use.
            let name = ty.get_display_name();
            if name.starts_with("type-parameter") || name.starts_with("template-parameter") {
                // `type-parameter-0-0` is a template type param use.
                OracleType::TypeVar(name)
            } else if !name.is_empty() {
                OracleType::Named {
                    name,
                    usr: named_decl_usr(ty),
                    args: Vec::new(),
                }
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::Auto | TypeKind::Dependent => OracleType::Inferred,
        _ => {
            let name = ty.get_display_name();
            if name.is_empty() {
                OracleType::Inferred
            } else {
                OracleType::Named {
                    name,
                    usr: named_decl_usr(ty),
                    args: Vec::new(),
                }
            }
        }
    }
}

// ── Width helpers ─────────────────────────────────────────────────────────────

fn width_int(ty: CType<'_>, signed: bool) -> OracleType {
    match ty.get_sizeof().ok().map(|b| (b * 8) as u16) {
        Some(bits @ (8 | 16 | 32 | 64 | 128)) => OracleType::Integer { signed, bits },
        _ => OracleType::IntegerArch { signed },
    }
}

fn width_float(ty: CType<'_>) -> OracleType {
    match ty.get_sizeof().ok().map(|b| (b * 8) as u16) {
        Some(bits @ (16 | 32 | 64 | 80 | 128)) => OracleType::Float { bits },
        _ => OracleType::FloatArch,
    }
}

// ── Metadata helpers ──────────────────────────────────────────────────────────

fn entity_usr(entity: Entity<'_>) -> Usr {
    entity.get_usr().map(|u| u.0).unwrap_or_default()
}

/// The USR of `ty`'s declaration, when libclang can resolve one.
///
/// This is the id `lower_type` (`lower.rs`) matches against the package's own
/// declared USRs to decide same-package (`Ref::Intro`) vs standard-library
/// (`Ref::Foreign`) vs genuinely unresolved. `get_declaration()` returns
/// `None` for type kinds with no backing declaration at all (a dependent
/// type, some exotic sugar) — that absence is itself meaningful downstream
/// (see `lower::lower_named`'s "no USR" branch), so it is preserved as `None`
/// rather than papered over with an empty string.
///
/// # Class-template applications: normalize specialization USR → primary
///
/// For a class-TEMPLATE APPLICATION (`Box<Widget>` where `template<class T>
/// struct Box{};` is declared somewhere), `ty.get_declaration()` returns the
/// SPECIALIZATION's declaration cursor, not the primary template's — and that
/// specialization carries its own distinct USR that encodes the argument
/// (empirically, for `template<class T> struct Box{}; Box<Widget> b;`: the
/// field's `Record`-kind type's declaration is a `StructDecl` cursor named
/// "Box" whose USR is `c:@S@Box>#$@S@Widget`), while `known_nominal_usrs`
/// (`lower.rs`) indexes the PRIMARY template declaration under its own USR
/// (`c:@ST>1#T@Box`). Those two USRs never match, so a same-package
/// template's application fell back to `Type::unresolved_external` even
/// though the template is declared in the same file.
///
/// `Entity::get_template` (libclang's `clang_getSpecializedCursorTemplate`)
/// recovers the primary template's cursor from a specialization's
/// declaration cursor — verified empirically against the fixture above,
/// where it returns the `ClassTemplate` cursor named "Box" with USR
/// `c:@ST>1#T@Box`, exactly the id `known_nominal_usrs` indexes. It returns
/// `None` for an ordinary (non-template) declaration (verified against a
/// plain `struct Widget`), so `.unwrap_or(decl)` is a no-op for every
/// non-template case and changes nothing about `cc1`-`cc9`'s existing
/// behavior.
fn named_decl_usr(ty: CType<'_>) -> Option<Usr> {
    ty.get_declaration()
        .map(|decl| decl.get_template().unwrap_or(decl))
        .map(entity_usr)
        .filter(|u| !u.is_empty())
}

fn entity_file(entity: Entity<'_>) -> PathBuf {
    entity
        .get_location()
        .and_then(|loc| loc.get_file_location().file)
        .map(|f| f.get_path())
        .unwrap_or_default()
}

fn entity_offset(entity: Entity<'_>) -> usize {
    entity
        .get_location()
        .map_or(0, |loc| loc.get_file_location().offset as usize)
}

fn documentation(entity: Entity<'_>) -> String {
    entity.get_comment().map(clean_comment).unwrap_or_default()
}

fn visibility(entity: Entity<'_>) -> OracleVisibility {
    match entity.get_accessibility() {
        Some(Accessibility::Private) => OracleVisibility::Private,
        Some(Accessibility::Protected) => OracleVisibility::Protected,
        Some(Accessibility::Public) => OracleVisibility::Public,
        None => match entity.get_storage_class() {
            Some(StorageClass::Static) => OracleVisibility::Internal,
            _ => OracleVisibility::Public,
        },
    }
}

fn clean_comment(raw: String) -> String {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("/**")
        .and_then(|t| t.strip_suffix("*/"))
        .or_else(|| {
            trimmed
                .strip_prefix("/*")
                .and_then(|t| t.strip_suffix("*/"))
        })
        .unwrap_or(trimmed);
    inner
        .lines()
        .map(|line| {
            line.trim()
                .strip_prefix("///")
                .or_else(|| line.trim().strip_prefix("//"))
                .unwrap_or_else(|| line.trim().strip_prefix('*').unwrap_or_else(|| line.trim()))
                .trim()
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
