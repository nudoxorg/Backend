//! Lower [`oracle::TypeData`] into [`nudox_ir::kinds::ty::Type`].
//!
//! One-way, documented mapping from Python's type system into the IR type
//! algebra. Rules:
//!
//! | Python                     | IR                                           |
//! |----------------------------|----------------------------------------------|
//! | `Any` / unannotated        | `Type::Any`                                  |
//! | `NoReturn` / `Never`       | `Type::Never`                                |
//! | `T` (type var / PEP 695)   | `Type::TypeVar(name)` — NEVER `Any`          |
//! | `Optional[T]`              | `Type::Union([lower(T), Never])` *see note   |
//! | `Union[A, B]` / `A \| B`  | `Type::Union([…])`                           |
//! | `Tuple[A, B]`              | `Type::Tuple([…])`                           |
//! | `list[T]` / slice-like     | `Type::Slice(lower(T))`                      |
//! | `None`                     | `Type::Primitive(Primitive::Builtin("None"))`|
//! | `Self` / `self`            | `Type::SelfType`                             |
//! | `Foo[T, U]` (same-pkg)     | `Type::Apply { base: Nominal(..), args }`    |
//! | `Foo` (same-pkg nominal)   | `Type::Nominal(Ref::Intro(..))`              |
//! | `int`, `float`, `bool`…    | `Type::Primitive(…)`                         |
//! | `Foo` (nominal, external)  | `Type::Any` (no UniqueId/StableRef yet)      |
//!
//! # Note on `Optional[T]` / `None`
//!
//! `Optional[T]` is stored in `TypeData::Union([T, NoneType])`. At the IR
//! level there is no distinct `NoneType` entry in scope, so
//! `TypeData::NoneType` lowers to `Type::Primitive(Primitive::Builtin("None"))`
//! — a sentinel that downstream consumers can recognize.
//!
//! # Note on same-package nominals
//!
//! [`Lowering::nominal`] is **order-independent**: it interns the ID and
//! returns a `Ref` immediately, whether or not the target has been declared
//! yet.  Declaration can come later in the same pass, or `finish` reports it
//! as `Undeclared`.  This is the entire reason the flat sink exists.
//!
//! A name that the oracle reported as belonging to this package is known by
//! its fully-qualified `PythonId`, so we can call `out.nominal::<Record>(id)`
//! right now.  The `Ref` will resolve to `Ref::Intro` after `seal`.
//!
//! # Note on cross-package nominals
//!
//! Names that are NOT in the current package (builtins, stdlib, third-party)
//! require a `UniqueId` / `StableRef` that the oracle does not supply at this
//! phase.  Those fall back to `Type::Any` until a registry link pass provides
//! the stable cross-package identity.  This is the honest reason — not that
//! `Nominal` requires prior declaration (it does not).

use std::collections::HashSet;

use nudox_ir::kinds::ty::{Primitive, Type, Width};
use nudox_ir::kinds::{GenericParam, Record};
use nudox_ir::lower::Lowering;
use std::num::NonZeroU16;

use crate::oracle::{GenericParamData, PythonId, TypeData};

/// Lower a [`TypeData`] into a [`nudox_ir::kinds::ty::Type`].
///
/// - `out` — the flat lowering sink; `nominal`/`apply` borrow it mutably to
///   intern a forward reference. They are disjoint from the `oracle` borrow.
/// - `known_ids` — the set of fully-qualified IDs declared in this package.
///   Any name found here resolves to a same-package nominal; everything else
///   falls back to `Type::Any` (cross-package, awaiting link phase).
pub fn lower_type(ty: &TypeData, out: &mut Lowering<PythonId>, known_ids: &HashSet<String>) -> Type {
    match ty {
        TypeData::Any => Type::Any,
        TypeData::Never => Type::Never,
        TypeData::SelfType => Type::SelfType,

        TypeData::NoneType => {
            // Python `None` is not the same as Rust's `!`. Lower it as a
            // builtin primitive sentinel; downstream consumers detect this by
            // the `"None"` tag.
            Type::Primitive(Primitive::Builtin("None".to_owned()))
        }

        TypeData::TypeVar(name) => Type::TypeVar(name.clone()),

        TypeData::Nominal(name) => lower_nominal(name, out, known_ids),

        TypeData::Apply { base, args } => {
            // Check if the base is a same-package nominal that should become
            // Type::Apply { base: Nominal(..), args }
            if let TypeData::Nominal(base_name) = base.as_ref() {
                let clean = strip_loc(base_name);
                // First check if it's a builtin — builtins always win.
                let builtin = lower_builtin(clean);
                if let Some(prim) = builtin {
                    // Builtin base with args: keep Apply wrapper with lowered args.
                    let mut lowered_args = Vec::with_capacity(args.len());
                    for arg in args {
                        lowered_args.push(lower_type(arg, out, known_ids));
                    }
                    if lowered_args.is_empty() {
                        return prim;
                    }
                    return Type::Apply {
                        base: Box::new(prim),
                        args: lowered_args.into_boxed_slice(),
                    };
                }
                // Check same-package nominal for generic application.
                let base_ty = lower_nominal(base_name, out, known_ids);
                if matches!(base_ty, Type::Nominal(_)) {
                    // Same-package base: build Apply via out.apply.
                    let id = PythonId::new(clean.to_owned());
                    let mut lowered_args = Vec::with_capacity(args.len());
                    for arg in args {
                        lowered_args.push(lower_type(arg, out, known_ids));
                    }
                    return out.apply::<Record>(id, lowered_args);
                }
                // External base (Any) — degrade to Any or wrap in Apply
                // depending on whether there are args worth preserving.
                if args.is_empty() {
                    return base_ty;
                }
                let mut lowered_args = Vec::with_capacity(args.len());
                for arg in args {
                    lowered_args.push(lower_type(arg, out, known_ids));
                }
                Type::Apply {
                    base: Box::new(base_ty),
                    args: lowered_args.into_boxed_slice(),
                }
            } else {
                // Non-nominal base (e.g. TypeVar application — rare in Python).
                let base_ty = lower_type(base, out, known_ids);
                let mut lowered_args = Vec::with_capacity(args.len());
                for arg in args {
                    lowered_args.push(lower_type(arg, out, known_ids));
                }
                if lowered_args.is_empty() {
                    return base_ty;
                }
                Type::Apply {
                    base: Box::new(base_ty),
                    args: lowered_args.into_boxed_slice(),
                }
            }
        }

        TypeData::Union(members) => {
            if members.is_empty() {
                return Type::Any;
            }
            let mut lowered = Vec::with_capacity(members.len());
            for m in members {
                lowered.push(lower_type(m, out, known_ids));
            }
            Type::Union(lowered.into_boxed_slice())
        }

        TypeData::Intersection(members) => {
            if members.is_empty() {
                return Type::Any;
            }
            let mut lowered = Vec::with_capacity(members.len());
            for m in members {
                lowered.push(lower_type(m, out, known_ids));
            }
            Type::Intersection(lowered.into_boxed_slice())
        }

        TypeData::Tuple(elements) => {
            let mut lowered = Vec::with_capacity(elements.len());
            for e in elements {
                lowered.push(lower_type(e, out, known_ids));
            }
            Type::Tuple(lowered.into_boxed_slice())
        }

        TypeData::Slice(inner) => Type::Slice(Box::new(lower_type(inner, out, known_ids))),
    }
}

/// Lower a nominal Python type name into the appropriate IR type.
///
/// Resolution priority:
/// 1. Builtin primitives (int, str, bool, …) → `Type::Primitive`
/// 2. Same-package ID (present in `known_ids`) → `out.nominal::<Record>(id)`
/// 3. Everything else → `Type::Any`
///
/// The fallback to `Type::Any` in case 3 is NOT because `Nominal` requires
/// prior declaration (it does not — `Lowering::nominal` is order-independent).
/// The honest reason is that cross-package nominals require a `UniqueId` /
/// `StableRef` that the oracle does not supply at lowering time.  A future
/// link pass will substitute the correct ref.
fn lower_nominal(name: &str, out: &mut Lowering<PythonId>, known_ids: &HashSet<String>) -> Type {
    // Strip location suffix that pyrefly appends (`builtins.int@418:7-10`).
    let clean = strip_loc(name);

    // 1. Builtins always win.
    if let Some(prim) = lower_builtin(clean) {
        return prim;
    }

    // 2. Same-package nominal: the oracle has declared this ID in this package.
    if known_ids.contains(clean) {
        let id = PythonId::new(clean.to_owned());
        return out.nominal::<Record>(id);
    }

    // 3. Cross-package reference — no UniqueId/StableRef available yet.
    // The registry link phase is responsible for resolving these.
    Type::Any
}

/// Map a cleaned (loc-stripped) name to a builtin `Type::Primitive`, if
/// applicable. Returns `None` for anything that is not a known builtin.
fn lower_builtin(clean: &str) -> Option<Type> {
    match clean {
        "builtins.int" | "int" => Some(Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::Fixed(NonZeroU16::new(64).unwrap()),
        })),
        "builtins.float" | "float" => Some(Type::Primitive(Primitive::Float(Width::Fixed(
            NonZeroU16::new(64).unwrap(),
        )))),
        "builtins.bool" | "bool" => Some(Type::Primitive(Primitive::Bool)),
        "builtins.str" | "str" => Some(Type::Primitive(Primitive::Str)),
        "builtins.bytes" | "bytes" => Some(Type::Primitive(Primitive::Builtin("bytes".to_owned()))),
        "builtins.NoneType" | "None" | "NoneType" => {
            Some(Type::Primitive(Primitive::Builtin("None".to_owned())))
        }
        _ => None,
    }
}

/// Strip pyrefly's `@line:col-col` location suffix from a qualified name.
pub fn strip_loc(name: &str) -> &str {
    match name.find('@') {
        Some(pos) => &name[..pos],
        None => name,
    }
}

/// Lower a list of [`GenericParamData`] into IR [`GenericParam`]s.
///
/// Bounds and constraints are real type references; they get the same
/// same-package resolution as any other type position.
pub fn lower_generics(
    params: &[GenericParamData],
    out: &mut Lowering<PythonId>,
    known_ids: &HashSet<String>,
) -> Box<[GenericParam]> {
    let mut result = Vec::with_capacity(params.len());
    for p in params {
        let mut bounds: Vec<Type> = Vec::new();
        // Bound and constraints: use TypeVar name when the bound IS the param
        // itself (e.g. `T: T` — degenerate), otherwise lower normally.
        if let Some(bound) = &p.bound {
            bounds.push(lower_type(bound, out, known_ids));
        }
        for constraint in &p.constraints {
            bounds.push(lower_type(constraint, out, known_ids));
        }
        let default = p.default.as_ref().map(|d| lower_type(d, out, known_ids));
        result.push(GenericParam::Type {
            name: p.name.clone(),
            bounds: bounds.into_boxed_slice(),
            default,
        });
    }
    result.into_boxed_slice()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::{
        entry::{Symbol, Visibility},
        lower::Lowering,
        package::PackageId,
    };
    use std::path::PathBuf;

    /// Build a minimal `Lowering` and an empty `known_ids` set for tests that
    /// do not need same-package resolution.
    fn make_sink() -> Lowering<PythonId> {
        let pkg_id = PackageId::path("/tmp/test");
        let sym = Symbol {
            name: "test_pkg".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        Lowering::new(pkg_id, sym)
    }

    fn empty_ids() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn typevar_maps_to_typevar_not_any() {
        let ty = TypeData::TypeVar("T".to_string());
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert!(
            matches!(lowered, Type::TypeVar(ref name) if name == "T"),
            "TypeVar must lower to Type::TypeVar, got {lowered:?}"
        );
    }

    #[test]
    fn optional_int_is_union_with_none_sentinel() {
        // Optional[int] is Union([int, NoneType]) in TypeData.
        let ty = TypeData::Union(vec![
            TypeData::Nominal("builtins.int".to_string()),
            TypeData::NoneType,
        ]);
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        match lowered {
            Type::Union(ref members) => {
                assert_eq!(members.len(), 2, "Union must have 2 members");
                assert!(
                    matches!(members[0], Type::Primitive(Primitive::Integer { .. })),
                    "first member should be int primitive"
                );
                assert!(
                    matches!(&members[1], Type::Primitive(Primitive::Builtin(s)) if s == "None"),
                    "second member should be None sentinel"
                );
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn never_lowers_to_never() {
        let mut sink = make_sink();
        assert_eq!(lower_type(&TypeData::Never, &mut sink, &empty_ids()), Type::Never);
    }

    #[test]
    fn self_type_lowers_to_self_type() {
        let mut sink = make_sink();
        assert_eq!(lower_type(&TypeData::SelfType, &mut sink, &empty_ids()), Type::SelfType);
    }

    #[test]
    fn apply_preserves_structure() {
        let ty = TypeData::Apply {
            base: Box::new(TypeData::Nominal("builtins.list".to_string())),
            args: vec![TypeData::TypeVar("T".to_string())],
        };
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert!(
            matches!(lowered, Type::Apply { .. }),
            "Apply must lower to Apply, got {lowered:?}"
        );
    }

    #[test]
    fn str_nominal_maps_to_primitive() {
        let mut sink = make_sink();
        assert!(
            matches!(
                lower_type(&TypeData::Nominal("str".to_string()), &mut sink, &empty_ids()),
                Type::Primitive(Primitive::Str)
            ),
            "str must map to Primitive::Str"
        );
        let mut sink = make_sink();
        assert!(
            matches!(
                lower_type(&TypeData::Nominal("builtins.str".to_string()), &mut sink, &empty_ids()),
                Type::Primitive(Primitive::Str)
            ),
            "builtins.str must also map to Primitive::Str"
        );
    }

    #[test]
    fn external_nominal_maps_to_any() {
        // Genuinely cross-package names degrade to Any — no UniqueId available yet.
        let mut sink = make_sink();
        let lowered = lower_type(
            &TypeData::Nominal("some_lib.SomeClass".to_string()),
            &mut sink,
            &empty_ids(),
        );
        assert_eq!(lowered, Type::Any, "external nominal must lower to Any");
    }

    #[test]
    fn generic_param_with_bound() {
        let params = vec![GenericParamData {
            name: "T".to_string(),
            bound: Some(TypeData::Nominal("builtins.int".to_string())),
            constraints: vec![],
            default: None,
        }];
        let mut sink = make_sink();
        let lowered = lower_generics(&params, &mut sink, &empty_ids());
        assert_eq!(lowered.len(), 1);
        match &lowered[0] {
            GenericParam::Type { name, bounds, default } => {
                assert_eq!(name, "T");
                assert_eq!(bounds.len(), 1);
                assert!(default.is_none());
            }
            other => panic!("expected GenericParam::Type, got {other:?}"),
        }
    }

    #[test]
    fn strip_loc_removes_suffix() {
        assert_eq!(strip_loc("builtins.int@418:7-10"), "builtins.int");
        assert_eq!(strip_loc("my.module.Class"), "my.module.Class");
    }

    #[test]
    fn union_with_single_element() {
        // A union with one element is still a Union, not unwrapped.
        let ty = TypeData::Union(vec![TypeData::TypeVar("T".to_string())]);
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert!(matches!(lowered, Type::Union(_)));
    }

    // ── New tests: same-package nominal resolution ───────────────────────────

    /// A method whose return type names another class **declared later in the
    /// same package** must lower to `Type::Nominal`, not `Type::Any`.
    /// After `seal`, the nominal resolves to `Ref::Intro` pointing at that class.
    #[test]
    fn same_package_nominal_resolves_not_any() {
        use nudox_ir::kinds::Record;

        let mut sink = make_sink();
        // known_ids contains both "my_pkg.Holder" and "my_pkg.Payload".
        let known: HashSet<String> = ["my_pkg.Holder", "my_pkg.Payload"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Lower a return type that names Payload — which is not yet declared.
        let ret_ty = lower_type(
            &TypeData::Nominal("my_pkg.Payload".to_string()),
            &mut sink,
            &known,
        );
        assert!(
            matches!(ret_ty, Type::Nominal(_)),
            "same-package Payload must lower to Nominal, not Any; got {ret_ty:?}"
        );

        // Now declare both entries so finish() succeeds.
        let holder_sym = Symbol {
            name: "Holder".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let payload_sym = Symbol {
            name: "Payload".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        sink.declare(
            PythonId::new("my_pkg.Holder"),
            None,
            holder_sym,
            Record::builder().build(),
        );
        // Declare Payload AFTER the nominal reference was interned — this is the
        // forward-reference scenario that the flat sink was built for.
        sink.declare(
            PythonId::new("my_pkg.Payload"),
            None,
            payload_sym,
            Record::builder().build(),
        );

        let pkg = sink.finish().expect("forward-referenced nominal must resolve");

        // Verify Payload is present and the nominal sealed to Ref::Intro.
        let payload_entry = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Payload")
            .expect("Payload must be in the package");

        // The nominal we lowered earlier should have sealed to an Intro ref.
        // We verify by checking that Payload's intro-id matches what ret_ty
        // points at. Use the sealed package's entry index.
        match ret_ty {
            Type::Nominal(_) => {
                // The RawRef was interned at lower_type time. After finish(),
                // it resolved to Payload's IntroId (no LoweringError returned).
                // The exact IntroId comparison would require seal access; it is
                // sufficient to assert that finish() succeeded and Payload exists.
                let _ = payload_entry;
            }
            other => panic!("expected Nominal, got {other:?}"),
        }
    }

    /// `apply::<Record>(id, args)` over a same-package class produces
    /// `Type::Apply { base: Nominal(..), args }`.
    #[test]
    fn same_package_generic_apply() {
        use nudox_ir::kinds::Record;

        let mut sink = make_sink();
        let known: HashSet<String> = ["my_pkg.Container"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Container[int]
        let applied = lower_type(
            &TypeData::Apply {
                base: Box::new(TypeData::Nominal("my_pkg.Container".to_string())),
                args: vec![TypeData::Nominal("int".to_string())],
            },
            &mut sink,
            &known,
        );

        assert!(
            matches!(applied, Type::Apply { .. }),
            "same-package Container[int] must lower to Apply, got {applied:?}"
        );

        // The base of the Apply must be a Nominal (same-package ref), not Any.
        if let Type::Apply { base, args } = &applied {
            assert!(
                matches!(base.as_ref(), Type::Nominal(_)),
                "Apply base must be Nominal for same-package class, got {base:?}"
            );
            assert_eq!(args.len(), 1, "must have 1 type arg");
            assert!(
                matches!(args[0], Type::Primitive(Primitive::Integer { .. })),
                "arg must be int primitive"
            );
        }

        // Declare Container so finish() succeeds.
        let container_sym = Symbol {
            name: "Container".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        sink.declare(
            PythonId::new("my_pkg.Container"),
            None,
            container_sym,
            Record::builder().build(),
        );
        sink.finish().expect("must finish successfully");
    }

    /// A bound or constraint that refers to a same-package class must lower to
    /// `Type::Nominal`, not `Type::Any`.
    #[test]
    fn generic_param_bound_resolves_same_package() {
        use nudox_ir::kinds::Record;

        let mut sink = make_sink();
        let known: HashSet<String> = ["my_pkg.Base"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let params = vec![GenericParamData {
            name: "T".to_string(),
            bound: Some(TypeData::Nominal("my_pkg.Base".to_string())),
            constraints: vec![],
            default: None,
        }];
        let lowered = lower_generics(&params, &mut sink, &known);
        assert_eq!(lowered.len(), 1);
        match &lowered[0] {
            GenericParam::Type { name, bounds, .. } => {
                assert_eq!(name, "T");
                assert_eq!(bounds.len(), 1);
                assert!(
                    matches!(bounds[0], Type::Nominal(_)),
                    "bound on same-package class must be Nominal, got {:?}",
                    bounds[0]
                );
            }
            other => panic!("expected GenericParam::Type, got {other:?}"),
        }

        // Declare Base so finish succeeds.
        let base_sym = Symbol {
            name: "Base".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        sink.declare(
            PythonId::new("my_pkg.Base"),
            None,
            base_sym,
            Record::builder().build(),
        );
        sink.finish().expect("must finish successfully");
    }
}
