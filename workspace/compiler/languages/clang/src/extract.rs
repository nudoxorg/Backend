//! Drive libclang to extract fully-owned data into a [`ClangOracle`].
//!
//! This module contains everything that touches the `clang` crate.  By the
//! time [`extract_tu`] returns, all libclang objects have been dropped and
//! only owned Rust values survive.

use std::path::{Path, PathBuf};

use clang::{
    Accessibility, Entity, EntityKind, Index, StorageClass, TranslationUnit, Type as CType,
    TypeKind,
};

#[cfg(test)]
use clang::Unsaved;

use crate::oracle::{
    ClangOracle, OracleAlias, OracleEnum, OracleField, OracleFnMod, OracleFunction,
    OracleGenericParam, OracleNamespace, OracleParam, OracleReceiver, OracleRecord, OracleType,
    OracleVar, OracleVariant, OracleVisibility, Usr,
};

// ── Public entry points ───────────────────────────────────────────────────────

/// Parse a real file on disk and return fully-owned oracle data.
pub fn extract_file(index: &Index<'_>, path: &Path, args: &[&str]) -> ClangOracle {
    let mut parser = index.parser(path);
    parser.arguments(args);
    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => {
            // Malformed or missing file: return empty oracle.
            let _ = e;
            return ClangOracle::default();
        }
    };
    extract_tu(&tu)
}

/// Parse an **in-memory** source string under the given virtual filename.
///
/// Used in tests to avoid touching the filesystem.
#[cfg(test)]
pub fn extract_unsaved(
    index: &Index<'_>,
    virtual_path: &Path,
    source: &str,
    args: &[&str],
) -> ClangOracle {
    let unsaved = [Unsaved::new(virtual_path, source)];
    let mut parser = index.parser(virtual_path);
    parser.arguments(args);
    parser.unsaved(&unsaved);
    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => {
            let _ = e;
            return ClangOracle::default();
        }
    };
    extract_tu(&tu)
}

// ── TU extraction ─────────────────────────────────────────────────────────────

fn extract_tu(tu: &TranslationUnit<'_>) -> ClangOracle {
    let mut oracle = ClangOracle::default();
    let root = tu.get_entity();
    for child in root.get_children() {
        visit_entity(&mut oracle, child, None);
    }
    oracle
}

// ── Recursive entity visitor ──────────────────────────────────────────────────

fn visit_entity(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
    // Filter to entities lexically defined in the main translation unit file.
    if !entity.is_in_main_file() {
        return;
    }
    if entity.is_invalid_declaration() {
        return;
    }

    match entity.get_kind() {
        EntityKind::Namespace => visit_namespace(oracle, entity, parent_usr),
        EntityKind::StructDecl => visit_record(oracle, entity, parent_usr, false),
        EntityKind::ClassDecl | EntityKind::ClassTemplate => {
            visit_record(oracle, entity, parent_usr, false)
        }
        EntityKind::UnionDecl => visit_record(oracle, entity, parent_usr, true),
        EntityKind::FunctionDecl | EntityKind::FunctionTemplate => {
            visit_function(oracle, entity, parent_usr, None)
        }
        EntityKind::Method
        | EntityKind::Constructor
        | EntityKind::Destructor
        | EntityKind::ConversionFunction => {
            visit_function(oracle, entity, parent_usr, Some(receiver_kind(entity)))
        }
        EntityKind::EnumDecl => visit_enum(oracle, entity, parent_usr),
        EntityKind::TypedefDecl | EntityKind::TypeAliasDecl => {
            visit_alias(oracle, entity, parent_usr)
        }
        EntityKind::VarDecl => visit_var(oracle, entity, parent_usr),
        EntityKind::FieldDecl => visit_field(oracle, entity, parent_usr),
        // Linkage-spec blocks (`extern "C" { … }`) — recurse transparently.
        EntityKind::LinkageSpec => {
            for child in entity.get_children() {
                visit_entity(oracle, child, parent_usr);
            }
        }
        // Everything else we skip without error.
        _ => {}
    }
}

// ── Namespace ─────────────────────────────────────────────────────────────────

fn visit_namespace(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
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
        usr: usr.clone(),
        name,
        source_file: entity_file(entity),
        byte_offset: entity_offset(entity),
        documentation: documentation(entity),
        visibility: visibility(entity),
        parent_usr: parent_usr.map(str::to_owned),
    });

    for child in entity.get_children() {
        visit_entity(oracle, child, Some(&this_usr));
    }
}

// ── Record (struct / class / union) ──────────────────────────────────────────

fn visit_record(
    oracle: &mut ClangOracle,
    entity: Entity<'_>,
    parent_usr: Option<&str>,
    is_union: bool,
) {
    // Forward declarations (no definition) — skip to avoid duplicates.
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

    let is_class = matches!(
        entity.get_kind(),
        EntityKind::ClassDecl | EntityKind::ClassTemplate
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

    let this_usr = usr.clone();
    for child in children {
        visit_entity(oracle, child, Some(&this_usr));
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

    let ty = entity
        .get_type()
        .map(|t| resolve_type(t))
        .unwrap_or(OracleType::Inferred);

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
    let (params, variadic) = function_params(entity);
    let ret = entity
        .get_result_type()
        .map(resolve_type)
        .unwrap_or(OracleType::Void);

    let modifiers = fn_modifiers(entity, &children);
    let abi = fn_abi(entity);

    oracle.functions.push(OracleFunction {
        usr,
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
}

fn function_params(entity: Entity<'_>) -> (Vec<OracleParam>, bool) {
    let mut params = Vec::new();

    if let Some(args) = entity.get_arguments() {
        for arg in args {
            let ty = arg
                .get_type()
                .map(resolve_type)
                .unwrap_or(OracleType::Inferred);
            params.push(OracleParam {
                name: arg.get_name().unwrap_or_default(),
                ty,
                is_variadic: false,
            });
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

fn visit_enum(oracle: &mut ClangOracle, entity: Entity<'_>, parent_usr: Option<&str>) {
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
        usr: usr.clone(),
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
        if child.get_kind() == EntityKind::EnumConstantDecl && child.is_in_main_file() {
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
        .map(resolve_type)
        .unwrap_or(OracleType::Inferred);

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

    let ty = entity
        .get_type()
        .map(resolve_type)
        .unwrap_or(OracleType::Inferred);
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
                    .map(|t| t.get_display_name())
                    .unwrap_or_else(|| "auto".to_owned());
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

        TypeKind::CharS | TypeKind::SChar => width_int(ty, true),
        TypeKind::CharU | TypeKind::UChar => width_int(ty, false),
        TypeKind::WChar => width_int(ty, false),
        TypeKind::Char16 => OracleType::Integer {
            signed: false,
            bits: 16,
        },
        TypeKind::Char32 => OracleType::Integer {
            signed: false,
            bits: 32,
        },

        TypeKind::Short => width_int(ty, true),
        TypeKind::Int => width_int(ty, true),
        TypeKind::Long => width_int(ty, true),
        TypeKind::LongLong => width_int(ty, true),
        TypeKind::Int128 => OracleType::Integer {
            signed: true,
            bits: 128,
        },

        TypeKind::UShort => width_int(ty, false),
        TypeKind::UInt => width_int(ty, false),
        TypeKind::ULong => width_int(ty, false),
        TypeKind::ULongLong => width_int(ty, false),
        TypeKind::UInt128 => OracleType::Integer {
            signed: false,
            bits: 128,
        },

        TypeKind::Half | TypeKind::Float16 => OracleType::Float { bits: 16 },
        TypeKind::Float => OracleType::Float { bits: 32 },
        TypeKind::Double => OracleType::Float { bits: 64 },
        TypeKind::LongDouble | TypeKind::Float128 => width_float(ty),

        TypeKind::Pointer => {
            if let Some(pointee) = ty.get_pointee_type() {
                if pointee.is_const_qualified() {
                    OracleType::ConstPointer(Box::new(resolve_type(pointee)))
                } else {
                    OracleType::MutPointer(Box::new(resolve_type(pointee)))
                }
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::LValueReference => {
            if let Some(referent) = ty.get_pointee_type().or_else(|| ty.get_element_type()) {
                let mutable = !referent.is_const_qualified();
                OracleType::LValueRef {
                    mutable,
                    ty: Box::new(resolve_type(referent)),
                }
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::RValueReference => {
            if let Some(referent) = ty.get_pointee_type().or_else(|| ty.get_element_type()) {
                OracleType::RValueRef(Box::new(resolve_type(referent)))
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::ConstantArray | TypeKind::VariableArray | TypeKind::DependentSizedArray => {
            if let Some(elem) = ty.get_element_type() {
                let len = ty.get_size().unwrap_or(0);
                OracleType::Array {
                    ty: Box::new(resolve_type(elem)),
                    len,
                }
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::IncompleteArray => {
            if let Some(elem) = ty.get_element_type() {
                OracleType::Slice(Box::new(resolve_type(elem)))
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::FunctionNoPrototype | TypeKind::FunctionPrototype => {
            let params = ty
                .get_argument_types()
                .unwrap_or_default()
                .into_iter()
                .map(resolve_type)
                .collect();
            let ret = ty
                .get_result_type()
                .map(resolve_type)
                .unwrap_or(OracleType::Void);
            OracleType::FnPtr {
                ret: Box::new(ret),
                params,
            }
        }
        TypeKind::Elaborated => ty
            .get_elaborated_type()
            .map(resolve_type)
            .unwrap_or(OracleType::Inferred),
        TypeKind::Typedef => {
            let name = ty
                .get_typedef_name()
                .unwrap_or_else(|| ty.get_display_name());
            OracleType::Named {
                name,
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
            OracleType::Named { name, args }
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
                    args: Vec::new(),
                }
            } else {
                OracleType::Inferred
            }
        }
        TypeKind::Auto | TypeKind::Dependent => OracleType::Inferred,
        _ => {
            let name = ty.get_display_name();
            if !name.is_empty() {
                OracleType::Named {
                    name,
                    args: Vec::new(),
                }
            } else {
                OracleType::Inferred
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
        .map(|loc| loc.get_file_location().offset as usize)
        .unwrap_or(0)
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
                .unwrap_or_else(|| line.trim().strip_prefix('*').unwrap_or(line.trim()))
                .trim()
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
