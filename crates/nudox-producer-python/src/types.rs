//! Lower [`oracle::TypeData`] into [`nudox_ir::kinds::Type`].
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
//! | `Foo[T, U]`                | `Type::Apply { base, args }`                 |
//! | `int`, `float`, `bool`…    | `Type::Primitive(…)`                         |
//! | `Foo` (nominal, external)  | `Type::Any` (no local ref; registry resolves)|
//!
//! # Note on `Optional[T]` / `None`
//!
//! `Optional[T]` is stored in `TypeData::Union([T, NoneType])`. At the IR
//! level there is no distinct `NoneType` entry in scope, so
//! `TypeData::NoneType` lowers to `Type::Primitive(Primitive::Builtin("None"))`
//! — a sentinel that downstream consumers can recognize.
//!
//! # Note on nominal external references
//!
//! `Type::Nominal(RawRef)` requires either a local `Ref<T>` (which requires
//! the target to be declared in the current `Lowering`) or a `Foreign(StableRef)`
//! (which requires a sealed `IntroId`). Neither is available for external names
//! at lowering time. We therefore lower unresolved external nominals to
//! `Type::Any` and rely on the registry to substitute the correct ref when
//! packages are linked. This is the same approach used by Go, C#, and Java
//! producers for cross-package refs.

use nudox_ir::kinds::ty::{Primitive, Type, Width};
use nudox_ir::kinds::GenericParam;
use std::num::NonZeroU16;

use crate::oracle::{GenericParamData, TypeData};

/// Lower a [`TypeData`] into a [`nudox_ir::kinds::ty::Type`].
pub fn lower_type(ty: &TypeData) -> Type {
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

        TypeData::Nominal(name) => lower_nominal(name),

        TypeData::Apply { base, args } => {
            let base_ty = lower_type(base);
            let lowered_args: Box<[Type]> = args.iter().map(lower_type).collect();
            Type::Apply {
                base: Box::new(base_ty),
                args: lowered_args,
            }
        }

        TypeData::Union(members) => {
            if members.is_empty() {
                return Type::Any;
            }
            let lowered: Box<[Type]> = members.iter().map(lower_type).collect();
            Type::Union(lowered)
        }

        TypeData::Intersection(members) => {
            if members.is_empty() {
                return Type::Any;
            }
            let lowered: Box<[Type]> = members.iter().map(lower_type).collect();
            Type::Intersection(lowered)
        }

        TypeData::Tuple(elements) => {
            let lowered: Box<[Type]> = elements.iter().map(lower_type).collect();
            Type::Tuple(lowered)
        }

        TypeData::Slice(inner) => Type::Slice(Box::new(lower_type(inner))),
    }
}

/// Lower a nominal Python type name into the appropriate IR type.
///
/// Well-known builtins map to `Type::Primitive`; everything else maps to
/// `Type::Any` (registry resolves cross-package nominals later).
fn lower_nominal(name: &str) -> Type {
    // Strip location suffix that pyrefly appends (`builtins.int@418:7-10`).
    let clean = strip_loc(name);
    match clean {
        "builtins.int" | "int" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::Fixed(NonZeroU16::new(64).unwrap()),
        }),
        "builtins.float" | "float" => {
            Type::Primitive(Primitive::Float(Width::Fixed(NonZeroU16::new(64).unwrap())))
        }
        "builtins.bool" | "bool" => Type::Primitive(Primitive::Bool),
        "builtins.str" | "str" => Type::Primitive(Primitive::Str),
        "builtins.bytes" | "bytes" => Type::Primitive(Primitive::Builtin("bytes".to_owned())),
        "builtins.NoneType" | "None" | "NoneType" => {
            Type::Primitive(Primitive::Builtin("None".to_owned()))
        }
        _ => {
            // External nominal reference — cannot be represented as a local
            // `Ref` at lowering time. The registry resolves these at link time.
            // Lowering to `Any` is the documented policy for this phase.
            Type::Any
        }
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
pub fn lower_generics(params: &[GenericParamData]) -> Box<[GenericParam]> {
    params
        .iter()
        .map(|p| {
            let bounds: Box<[Type]> = p
                .bound
                .iter()
                .chain(p.constraints.iter())
                .map(lower_type)
                .collect();
            let default = p.default.as_ref().map(lower_type);
            GenericParam::Type {
                name: p.name.clone(),
                bounds,
                default,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typevar_maps_to_typevar_not_any() {
        let ty = TypeData::TypeVar("T".to_string());
        let lowered = lower_type(&ty);
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
        let lowered = lower_type(&ty);
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
        assert_eq!(lower_type(&TypeData::Never), Type::Never);
    }

    #[test]
    fn self_type_lowers_to_self_type() {
        assert_eq!(lower_type(&TypeData::SelfType), Type::SelfType);
    }

    #[test]
    fn apply_preserves_structure() {
        let ty = TypeData::Apply {
            base: Box::new(TypeData::Nominal("builtins.list".to_string())),
            args: vec![TypeData::TypeVar("T".to_string())],
        };
        let lowered = lower_type(&ty);
        assert!(
            matches!(lowered, Type::Apply { .. }),
            "Apply must lower to Apply, got {lowered:?}"
        );
    }

    #[test]
    fn str_nominal_maps_to_primitive() {
        assert!(
            matches!(
                lower_type(&TypeData::Nominal("str".to_string())),
                Type::Primitive(Primitive::Str)
            ),
            "str must map to Primitive::Str"
        );
        assert!(
            matches!(
                lower_type(&TypeData::Nominal("builtins.str".to_string())),
                Type::Primitive(Primitive::Str)
            ),
            "builtins.str must also map to Primitive::Str"
        );
    }

    #[test]
    fn external_nominal_maps_to_any() {
        // Unresolved external nominals degrade to Any at lowering time.
        let lowered = lower_type(&TypeData::Nominal("some_lib.SomeClass".to_string()));
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
        let lowered = lower_generics(&params);
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
        let lowered = lower_type(&ty);
        assert!(matches!(lowered, Type::Union(_)));
    }
}
