//! Lower the oracle's structural Go type tree into `nudox_ir::kinds::Type`.
//!
//! Design decisions (mirroring the old `workspace/compiler/compile/go/types.rs`
//! but targeting the new IR):
//!
//! * **Named / alias** → `Type::Nominal(RawRef)` obtained via
//!   `Lowering::refer`, or `Type::Apply { base, args }` for instantiated
//!   generic named types.
//! * **Pointer** (`*T`) → `Type::Primitive(Primitive::MutPointer(elem))` —
//!   Go pointers are GC-managed, freely mutable, and carry no borrow
//!   discipline; `MutPointer` over-claims less than a reference type.
//! * **Slice** (`[]T`) → `Type::Slice(elem)`.
//! * **Array** (`[N]T`) → `Type::Array { ty: elem, length: N }`.
//! * **Map** (`map[K]V`) — no direct IR type.  Lowered as
//!   `Type::Nominal(refer("map"))` applied to `[K, V]` as a `Type::Apply`.
//!   However, since "map" is not a real declared type, we cannot refer to it.
//!   Instead we fall back to `Type::Any` with a logged note and return
//!   `ProducerError::Unsupported` at the call site when the caller wants
//!   precision.  In practice the IR has no map primitive — see the
//!   UNCERTAINTY note in the report.
//! * **Chan** — similarly no IR type; lowered as `Type::Any` (unsupported).
//! * **Func type** → `Type::Tuple` of param + result types as a fallback;
//!   structural function pointers cannot be faithfully round-tripped without
//!   a dedicated IR variant.  Returned as `Type::Any` (see uncertainty note).
//! * **Struct literal** (anonymous) → `Type::Any` (no inline struct literal
//!   in the new IR; the old IR had `RecordLiteral`).
//! * **Interface literal** → `Type::Any` (same gap; the old IR had
//!   `Intersection`/`RecordLiteral`).
//! * **Constraint union** → `Type::Union(parts)`.
//! * **TypeParam** → `Type::SelfType` placeholder (no GenericParam type-var
//!   in the new IR's `Type` enum — see uncertainty note).
//! * **Basic** types → `Type::Primitive(...)` or `Type::Primitive(Str)` etc.

use nudox_ir::kinds::{
    function::Receiver,
    generics::GenericParam,
    ty::{Primitive, Type, Width},
};

use crate::oracle::{self, TypeKind};

/// Defensive recursion bound.  Anonymous Go types cannot cycle, so this only
/// guards against malformed oracle output.
const MAX_DEPTH: usize = 64;

/// Qualify a Go identifier as `pkg.Name`.  Universe-scope names (`error`,
/// `comparable`, `any`) have no package and stay bare.
pub fn qualify(pkg: &str, name: &str) -> String {
    if pkg.is_empty() {
        name.to_string()
    } else {
        format!("{pkg}.{name}")
    }
}

/// Lower an oracle type node.  This version does NOT take a `Lowering` sink
/// because named-type references inside `Type` must be `RawRef` values, which
/// require calling `Lowering::refer` — but `Type` is a plain value, not a
/// builder step.  For named types we resolve them separately via
/// [`lower_named_ref`].
pub fn lower_type(t: &oracle::Type) -> Type {
    lower_type_depth(t, 0)
}

fn lower_type_depth(t: &oracle::Type, depth: usize) -> Type {
    if depth >= MAX_DEPTH {
        return Type::Any;
    }
    match t.kind {
        TypeKind::Basic => lower_basic(&t.name),

        // Named and alias types — in the new IR these are `Type::Nominal(RawRef)`.
        // We CANNOT lower them here because we do not have access to the
        // `Lowering` sink to call `refer`.  Callers that need a nominal ref must
        // call `lower_named_as_type(t, lowering)` from the lowering module.
        // This fallback handles types that appear only in annotation position
        // (field types, param types etc.) where the lowering function is threaded
        // through.
        //
        // UNCERTAINTY: This path returns `Type::Any` for named/alias types when
        // called from contexts that do not pass the Lowering sink (e.g. inside
        // anonymous type recursion).  The caller `types_lower_with_lowering`
        // in `lower.rs` handles the top-level case correctly.
        TypeKind::Named | TypeKind::Alias => {
            if t.pkg.is_empty() && t.name == "any" {
                return Type::Any;
            }
            // Cannot produce a RawRef here — see module doc.
            // Fallback: opaque Any.  Callers that need precision must use
            // lower_named_as_type instead.
            Type::Any
        }

        // Type parameters — the new IR's `Type` enum has no TypeParam/TypeVar
        // variant.  `Type::SelfType` is a placeholder; its semantics differ.
        // UNCERTAINTY: This is a gap in the IR.
        TypeKind::TypeParam => Type::SelfType,

        TypeKind::Pointer => {
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth(e, depth + 1))
                .unwrap_or(Type::Any);
            Type::Primitive(Primitive::MutPointer(Box::new(elem)))
        }

        TypeKind::Slice => {
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth(e, depth + 1))
                .unwrap_or(Type::Any);
            Type::Slice(Box::new(elem))
        }

        TypeKind::Array => {
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth(e, depth + 1))
                .unwrap_or(Type::Any);
            Type::Array {
                ty: Box::new(elem),
                length: t.len.max(0) as usize,
            }
        }

        // No map/chan/func/struct-literal/interface-literal in the new IR type
        // algebra.  These fall back to Any.  The lowering module notes these
        // with ProducerError::Unsupported when they appear at declaration level.
        TypeKind::Map | TypeKind::Chan | TypeKind::Func | TypeKind::Struct => Type::Any,

        TypeKind::Interface => {
            if t.is_empty_interface() {
                Type::Any
            } else {
                // Anonymous non-empty interface: no faithful representation.
                Type::Any
            }
        }

        TypeKind::Union => {
            let parts: Box<[Type]> = t
                .terms
                .iter()
                .map(|term| {
                    term.r#type
                        .as_ref()
                        .map(|inner| lower_type_depth(inner, depth + 1))
                        .unwrap_or(Type::Any)
                })
                .collect();
            Type::Union(parts)
        }

        TypeKind::Tuple => {
            let parts: Box<[Type]> = t
                .types
                .iter()
                .map(|inner| lower_type_depth(inner, depth + 1))
                .collect();
            Type::Tuple(parts)
        }

        TypeKind::Invalid => Type::Any,
    }
}

/// Map a Go basic-type name onto the IR primitive algebra.
///
/// Notable decisions:
/// * `byte` → `Type::Any` (distinguished from `uint8` which → `U8`).
///   UNCERTAINTY: The new IR has no `Nominal` alias for `byte`.  A forward-ref
///   approach would require Lowering access.
/// * `rune` → `Primitive::Char`.
/// * `complex64`/`complex128` → `Type::Any` (no complex number primitive).
/// * `unsafe.Pointer` → `Type::Any` (no unsafe-pointer primitive).
pub fn lower_basic(name: &str) -> Type {
    let name = name.strip_prefix("untyped ").unwrap_or(name);
    match name {
        "bool" => Type::Primitive(Primitive::Bool),
        "string" | "untyped string" => Type::Primitive(Primitive::Str),
        "int" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::Arch,
        }),
        "int8" => Type::I8,
        "int16" => Type::I16,
        "int32" => Type::I32,
        "int64" => Type::I64,
        "uint" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::Arch,
        }),
        "uint8" => Type::U8,
        "uint16" => Type::U16,
        "uint32" => Type::U32,
        "uint64" => Type::U64,
        // uintptr: pointer-sized unsigned int, not a pointer.
        "uintptr" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::Arch,
        }),
        // `byte` is a distinct universe alias for uint8, but we cannot produce
        // a named-type RawRef here without Lowering access.
        // UNCERTAINTY: falls to Any; see module doc.
        "byte" => Type::Any,
        "rune" => Type::Primitive(Primitive::Char),
        "float32" => Type::Primitive(Primitive::Float(Width::W32)),
        "float64" | "float" => Type::Primitive(Primitive::Float(Width::W64)),
        // complex64/complex128, unsafe.Pointer, error, comparable — no IR match.
        _ => Type::Any,
    }
}

/// Lower a receiver kind from oracle method metadata into [`nudox_ir::kinds::function::Receiver`].
///
/// Go receiver semantics:
/// * `pointer_recv = true` → `&mut self` style → `MutRef`.
/// * `pointer_recv = false` → value receiver → `Owned`.
pub fn lower_receiver(pointer_recv: bool) -> Receiver {
    if pointer_recv {
        Receiver::MutRef
    } else {
        Receiver::Owned
    }
}

/// Lower a single `oracle::Type` that is known to be `Named` or `Alias` into
/// the IR's generic-parameter list entry (constraint bound expressed as
/// `GenericParam::Type { bounds }` using `Type::Any` for now, since we cannot
/// produce a `Nominal` ref without lowering access).
///
/// UNCERTAINTY: Constraint bounds cannot be faithfully represented as
/// `Type::Nominal(...)` from this context.  They use `Type::Any` as a
/// placeholder until the lowering layer threads the `Lowering` sink here.
pub fn lower_type_param_decl(tp: &oracle::TypeParamDecl) -> GenericParam {
    let bounds: Box<[Type]> = tp
        .constraint
        .as_ref()
        .map(|c| {
            // A named constraint interface → single bound (Any placeholder).
            // A union constraint → Union of terms.
            // An empty interface (any) → no bound.
            if c.is_empty_interface() {
                Vec::<Type>::new().into_boxed_slice()
            } else {
                match c.kind {
                    TypeKind::Union => {
                        let u = lower_type_depth(c, 0);
                        vec![u].into_boxed_slice()
                    }
                    _ => vec![Type::Any].into_boxed_slice(),
                }
            }
        })
        .unwrap_or_else(|| Vec::new().into_boxed_slice());

    nudox_ir::kinds::GenericParam::Type {
        name: tp.name.clone(),
        bounds,
        default: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::kinds::ty::{Primitive, Width};

    #[test]
    fn basic_primitives() {
        assert!(matches!(lower_basic("bool"), Type::Primitive(Primitive::Bool)));
        assert!(matches!(lower_basic("string"), Type::Primitive(Primitive::Str)));
        assert!(matches!(lower_basic("int32"), Type::I32));
        assert!(matches!(lower_basic("float64"), Type::Primitive(Primitive::Float(Width::W64))));
        assert!(matches!(lower_basic("rune"), Type::Primitive(Primitive::Char)));
    }

    #[test]
    fn untyped_prefix_stripped() {
        assert!(matches!(lower_basic("untyped int"), Type::Primitive(Primitive::Integer { signed: true, width: Width::Arch })));
    }

    #[test]
    fn slice_lowering() {
        let t = oracle::Type {
            kind: TypeKind::Slice,
            elem: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "int32".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(matches!(lower_type(&t), Type::Slice(_)));
    }

    #[test]
    fn array_lowering() {
        let t = oracle::Type {
            kind: TypeKind::Array,
            len: 8,
            elem: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "bool".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        match lower_type(&t) {
            Type::Array { length, .. } => assert_eq!(length, 8),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn pointer_lowering() {
        let t = oracle::Type {
            kind: TypeKind::Pointer,
            elem: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "int64".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(matches!(
            lower_type(&t),
            Type::Primitive(Primitive::MutPointer(_))
        ));
    }

    #[test]
    fn union_lowering() {
        use crate::oracle::{Term, Type as OType};
        let t = OType {
            kind: TypeKind::Union,
            terms: Box::new([
                Term {
                    tilde: false,
                    r#type: Some(OType {
                        kind: TypeKind::Basic,
                        name: "int".to_string(),
                        ..Default::default()
                    }),
                },
                Term {
                    tilde: false,
                    r#type: Some(OType {
                        kind: TypeKind::Basic,
                        name: "string".to_string(),
                        ..Default::default()
                    }),
                },
            ]),
            ..Default::default()
        };
        match lower_type(&t) {
            Type::Union(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected Union, got {other:?}"),
        }
    }
}
