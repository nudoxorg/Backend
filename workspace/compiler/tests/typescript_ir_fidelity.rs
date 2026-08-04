//! Max-fidelity assertions for the TypeScript OXC IR producer.
//!
//! Pins the critical gaps that historically golden-wronged as:
//! - Symbol / member documentation always `None`
//! - Constructor parameter properties dropped
//! - Type-alias generics discarded
//! - `Module.members` left `None` after link
//! - `#private` fields losing their private marker
//! - `null` / `void` / `any` / `unknown` collapsing into each other
//! - TypedBinding types missing on const/let

use std::fs;
use std::path::{Path, PathBuf};

use compiler::languages::typescript::generate_ir;
use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Visibility};
use ir::record::{Field, FieldKey};
use ir::ty::Type;
use tempfile::TempDir;

// ─── Fixture plumbing ────────────────────────────────────────────────────────

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typescript")
}

fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn lower(fixture: &str, package: &str) -> (Index, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    copy_tree(&fixture_root().join(fixture), dir.path()).expect("fixture copies");
    let index = generate_ir(dir.path(), package).expect("lowering succeeds");
    (index, dir)
}

fn dump(index: &Index) -> String {
    let mut v: Vec<String> = index
        .entries_by_path
        .iter()
        .map(|(p, e)| {
            let key = match p {
                NudoxPath::Local(pb) => pb.display().to_string(),
                NudoxPath::External { path, dependency } => {
                    format!("{}::{}", dependency, path.display())
                }
            };
            format!("{key}:{}:{}", e.kind_tag(), e.name())
        })
        .collect();
    v.sort();
    v.join(", ")
}

fn entry_by_name<'i>(index: &'i Index, name: &str) -> &'i Entry {
    index
        .entries_by_path
        .values()
        .find(|e| e.name() == name)
        .unwrap_or_else(|| panic!("expected entry named `{name}`; have: {}", dump(index)))
}

fn field_names(fields: &[Field]) -> Vec<String> {
    fields
        .iter()
        .filter_map(|f| match f {
            Field::Known(kf) => match &kf.key {
                FieldKey::Ident(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn known_field<'a>(fields: &'a [Field], name: &str) -> &'a ir::record::KnownField {
    fields
        .iter()
        .find_map(|f| match f {
            Field::Known(kf) => match &kf.key {
                FieldKey::Ident(s) if s == name => Some(kf),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("field `{name}` not found; have {:?}", field_names(fields))
        })
}

// ─── 1. JSDoc on Coordinate (and member docs) ────────────────────────────────

/// Interface + property JSDoc must flow into the IR (not stay `None`).
#[test]
fn jsdoc_on_coordinate_is_non_empty() {
    let (index, _dir) = lower("jsdoc", "jsdoc");

    let coord = match entry_by_name(&index, "Coordinate") {
        Entry::TraitDef(sym) => sym,
        other => panic!("Coordinate must be TraitDef, got {other:?}"),
    };

    let doc = coord
        .documentation
        .as_deref()
        .unwrap_or("")
        .trim();
    assert!(
        !doc.is_empty(),
        "Coordinate.documentation must be non-empty (JSDoc regression); got {:?}",
        coord.documentation
    );
    assert!(
        doc.contains("geographic coordinate") || doc.contains("Latitude"),
        "Coordinate doc should mention the coordinate semantics; got {doc:?}"
    );

    let props = coord
        .inner
        .properties
        .as_ref()
        .expect("Coordinate must have properties");
    let lat = known_field(props, "lat");
    assert!(
        lat.documentation
            .as_deref()
            .map(|d| !d.trim().is_empty())
            .unwrap_or(false),
        "Coordinate.lat member documentation must be non-empty; got {:?}",
        lat.documentation
    );
    let lng = known_field(props, "lng");
    assert!(
        lng.documentation
            .as_deref()
            .map(|d| !d.trim().is_empty())
            .unwrap_or(false),
        "Coordinate.lng member documentation must be non-empty; got {:?}",
        lng.documentation
    );
}

// ─── 2. Widget parameter property → fields contains id ───────────────────────

/// `constructor(public readonly id: string)` synthesises a class field.
#[test]
fn widget_parameter_property_becomes_field() {
    let (index, _dir) = lower("kinds", "kinds");

    let widget = match entry_by_name(&index, "Widget") {
        Entry::RecordType(sym) => sym,
        other => panic!("Widget must be RecordType, got {other:?}"),
    };

    let names = field_names(&widget.inner.fields);
    assert!(
        names.iter().any(|n| n == "id"),
        "Widget.fields must contain synthesised parameter property `id`; have {names:?}"
    );

    let id = known_field(&widget.inner.fields, "id");
    assert!(
        !id.attributes.is_mutable,
        "Widget.id is `readonly` → is_mutable=false; attrs={:?}",
        id.attributes
    );
    assert!(
        matches!(
            id.r#type.as_deref(),
            Some(Type::Primitive(ir::primitives::Primitive::String))
        ),
        "Widget.id type should be string; got {:?}",
        id.r#type
    );
    assert_eq!(
        id.visibility.as_ref(),
        Some(&Visibility::Public),
        "public parameter property → Visibility::Public"
    );
}

// ─── 3. IsString / generic alias has TypeAliasBody.generics Some ─────────────

/// `type IsString<T> = …` must retain declaration-site generics.
#[test]
fn is_string_type_alias_preserves_generics() {
    let (index, _dir) = lower("structural", "structural");

    let alias = match entry_by_name(&index, "IsString") {
        Entry::TypeAlias(sym) => sym,
        other => panic!("IsString must be TypeAlias, got {other:?}"),
    };

    let generics = alias
        .inner
        .generics
        .as_ref()
        .expect("IsString must have TypeAliasBody.generics = Some(...)");
    let param_names: Vec<Option<&str>> = generics
        .params
        .iter()
        .filter_map(|p| match p {
            ir::parameter::Parameter::Type(tp) => Some(tp.name.as_deref()),
            _ => None,
        })
        .collect();
    assert!(
        param_names.iter().any(|n| *n == Some("T")),
        "IsString generics must include type param T; have {param_names:?}"
    );
    assert!(
        matches!(alias.inner.target, Type::Conditional(_)),
        "IsString target should remain Conditional; got {:?}",
        alias.inner.target
    );
}

// ─── 4. Module.members non-empty after link ──────────────────────────────────

/// Synthetic file-module entry must list top-level symbols under `members`.
#[test]
fn module_members_populated_after_link() {
    let (index, _dir) = lower("kinds", "kinds");

    let module_entry = index
        .entries_by_path
        .values()
        .find(|e| matches!(e, Entry::Module(s) if s.name == "mod" || s.inner.members.is_some()))
        .or_else(|| {
            index
                .entries_by_path
                .values()
                .find(|e| matches!(e, Entry::Module(_)))
        })
        .unwrap_or_else(|| panic!("expected a Module entry; have: {}", dump(&index)));

    match module_entry {
        Entry::Module(sym) => {
            let members = sym
                .inner
                .members
                .as_ref()
                .expect("Module.members must be Some(...) after link (was historically None)");
            assert!(
                !members.is_empty(),
                "Module.members must be non-empty after collecting entries"
            );
            // Expect several top-level symbols from the kinds fixture.
            let member_strs: Vec<String> = members
                .iter()
                .map(|p| match p {
                    NudoxPath::Local(pb) => pb.display().to_string(),
                    NudoxPath::External { path, dependency } => {
                        format!("{}::{}", dependency, path.display())
                    }
                })
                .collect();
            assert!(
                member_strs.iter().any(|m| m.contains("Widget")),
                "members should include Widget; have {member_strs:?}"
            );
            assert!(
                member_strs.iter().any(|m| m.contains("MAX_SIZE")),
                "members should include MAX_SIZE; have {member_strs:?}"
            );
        }
        other => panic!("expected Module, got {other:?}"),
    }
}

// ─── 5. Const MAX_SIZE TypedBinding.ty Some ──────────────────────────────────

/// `const MAX_SIZE: number = 100` must carry the annotated type on TypedBinding.
#[test]
fn max_size_typed_binding_has_type() {
    let (index, _dir) = lower("kinds", "kinds");

    match entry_by_name(&index, "MAX_SIZE") {
        Entry::Constant(sym) => {
            assert!(
                sym.inner.ty.is_some(),
                "MAX_SIZE TypedBinding.ty must be Some; got {:?}",
                sym.inner
            );
            assert!(
                matches!(
                    sym.inner.ty.as_ref(),
                    Some(Type::Primitive(ir::primitives::Primitive::Float(_)))
                ),
                "MAX_SIZE should be number (Float); got {:?}",
                sym.inner.ty
            );
            assert_eq!(
                sym.inner.mutable,
                Some(false),
                "const → mutable = Some(false)"
            );
        }
        other => panic!("MAX_SIZE must be Constant, got {other:?}"),
    }
}

// ─── 6. #private field not Public bare segments without marker ───────────────

/// Private `#brand` must keep the `#` prefix and Private visibility.
#[test]
fn private_hash_field_keeps_marker_and_visibility() {
    let (index, _dir) = lower("kinds", "kinds");

    let widget = match entry_by_name(&index, "Widget") {
        Entry::RecordType(sym) => sym,
        other => panic!("Widget must be RecordType, got {other:?}"),
    };

    let names = field_names(&widget.inner.fields);
    assert!(
        names.iter().any(|n| n == "#brand" || n.starts_with('#')),
        "Widget must have a `#…` private field (not bare `brand`); have {names:?}"
    );

    let brand = widget
        .inner
        .fields
        .iter()
        .find_map(|f| match f {
            Field::Known(kf) => match &kf.key {
                FieldKey::Ident(s) if s.starts_with('#') => Some(kf),
                _ => None,
            },
            _ => None,
        })
        .expect("#private field KnownField");

    assert_eq!(
        brand.visibility.as_ref(),
        Some(&Visibility::Private),
        "#private field must be Visibility::Private, not Public; got {:?}",
        brand.visibility
    );
}

// ─── 7. string|null distinguishable from void ────────────────────────────────

/// `null` must not collapse to empty Tuple (void unit).
#[test]
fn null_is_distinguishable_from_void() {
    let (index, _dir) = lower("kinds", "kinds");

    let void_ty = match entry_by_name(&index, "VoidOnly") {
        Entry::TypeAlias(sym) => &sym.inner.target,
        other => panic!("VoidOnly must be TypeAlias, got {other:?}"),
    };
    let null_ty = match entry_by_name(&index, "NullOnly") {
        Entry::TypeAlias(sym) => &sym.inner.target,
        other => panic!("NullOnly must be TypeAlias, got {other:?}"),
    };

    assert!(
        matches!(void_ty, Type::Tuple(ts) if ts.is_empty()),
        "void must lower to empty Tuple (unit); got {void_ty:?}"
    );
    assert!(
        matches!(
            null_ty,
            Type::TypeReference(tr) if tr.identifier == "null"
        ),
        "null must lower to TypeReference(\"null\"), not empty Tuple; got {null_ty:?}"
    );
    assert_ne!(
        void_ty, null_ty,
        "void and null must remain distinguishable in IR"
    );

    // Bonus: string|null union keeps null as a named reference, not unit.
    let (unions, _d) = lower("unions", "unions");
    match entry_by_name(&unions, "MaybeString") {
        Entry::TypeAlias(sym) => match &sym.inner.target {
            Type::Union(parts) => {
                let has_null = parts.iter().any(|p| {
                    matches!(p, Type::TypeReference(tr) if tr.identifier == "null")
                });
                let has_void_unit = parts.iter().any(|p| matches!(p, Type::Tuple(t) if t.is_empty()));
                assert!(
                    has_null,
                    "MaybeString = string | null must contain TypeReference(null); got {parts:?}"
                );
                assert!(
                    !has_void_unit,
                    "MaybeString must not treat null as empty Tuple/void; got {parts:?}"
                );
            }
            other => panic!("MaybeString target should be Union, got {other:?}"),
        },
        other => panic!("MaybeString must be TypeAlias, got {other:?}"),
    }
}

// ─── Extra fidelity pins ─────────────────────────────────────────────────────

/// Function JSDoc on `haversine` must also survive export-wrapper attachment.
#[test]
fn function_jsdoc_survives_export_wrapper() {
    let (index, _dir) = lower("jsdoc", "jsdoc");
    match entry_by_name(&index, "haversine") {
        Entry::Function(sym) => {
            let doc = sym.documentation.as_deref().unwrap_or("").trim();
            assert!(
                !doc.is_empty(),
                "haversine.documentation must be non-empty; got {:?}",
                sym.documentation
            );
            assert!(
                doc.to_ascii_lowercase().contains("haversine")
                    || doc.to_ascii_lowercase().contains("distance"),
                "haversine doc should mention the function purpose; got {doc:?}"
            );
        }
        other => panic!("haversine must be Function, got {other:?}"),
    }
}

/// Enum member initializers land in `SumVariant.data`; const enums are flagged.
#[test]
fn enum_values_and_const_flag() {
    let (index, _dir) = lower("kinds", "kinds");

    match entry_by_name(&index, "Direction") {
        Entry::SumType(sym) => {
            let up = sym
                .inner
                .variants
                .iter()
                .find(|v| v.name == "Up")
                .expect("Direction.Up variant");
            assert!(
                up.data.is_some() || up.discriminant.is_some(),
                "Direction.Up = 0 must populate SumVariant data/discriminant; got {:?}",
                up
            );
        }
        other => panic!("Direction must be SumType, got {other:?}"),
    }

    match entry_by_name(&index, "HttpStatus") {
        Entry::SumType(sym) => {
            let ok = sym
                .inner
                .variants
                .iter()
                .find(|v| v.name == "Ok")
                .expect("HttpStatus.Ok");
            assert!(
                ok.documentation
                    .as_deref()
                    .map(|d| d.contains("[const]"))
                    .unwrap_or(false),
                "const enum variants must carry [const] documentation flag; got {:?}",
                ok.documentation
            );
            assert!(
                ok.data.is_some(),
                "HttpStatus.Ok = 200 must populate data; got {:?}",
                ok
            );
        }
        other => panic!("HttpStatus must be SumType, got {other:?}"),
    }
}

/// `any` and `unknown` stay distinct keyword lowerings.
#[test]
fn any_and_unknown_are_distinct() {
    // Inline via existing structural/kinds isn't enough — write a tiny package.
    let dir = TempDir::new().expect("tempdir");
    fs::write(
        dir.path().join("mod.ts"),
        "export type A = any;\nexport type U = unknown;\n",
    )
    .unwrap();
    // Minimal package surface so entry discovery finds mod.ts.
    fs::write(
        dir.path().join("package.json"),
        r#"{"name":"kw","types":"mod.ts"}"#,
    )
    .unwrap();
    let index = generate_ir(dir.path(), "kw").expect("lower");

    let any_ty = match entry_by_name(&index, "A") {
        Entry::TypeAlias(s) => &s.inner.target,
        other => panic!("A must be TypeAlias, got {other:?}"),
    };
    let unk_ty = match entry_by_name(&index, "U") {
        Entry::TypeAlias(s) => &s.inner.target,
        other => panic!("U must be TypeAlias, got {other:?}"),
    };

    assert!(
        matches!(any_ty, Type::Any),
        "any → Type::Any; got {any_ty:?}"
    );
    assert!(
        matches!(
            unk_ty,
            Type::TypeReference(tr) if tr.identifier == "unknown"
        ),
        "unknown → TypeReference(\"unknown\"), not Type::Any; got {unk_ty:?}"
    );
    assert_ne!(any_ty, unk_ty, "any and unknown must be distinguishable");
}
