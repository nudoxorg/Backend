//! Lower the oracle's structural Go type tree into `nudox_ir::kinds::Type`.
//!
//! ## Design decisions
//!
//! * **Named / alias** → `Type::Nominal(RawRef)` obtained via
//!   `Lowering::refer`, or `Type::Apply { base, args }` for instantiated
//!   generic named types.  Requires `&mut Lowering<GoId>` in scope — every
//!   type-lowering helper that may encounter a named type therefore accepts a
//!   `low: &mut Lowering<GoId>` parameter.
//! * **Pointer** (`*T`) → `Type::Primitive(Primitive::MutPointer(elem))` — Go
//!   pointers are GC-managed, freely mutable, and carry no borrow discipline;
//!   `MutPointer` over-claims less than a reference type.
//! * **Slice** (`[]T`) → `Type::Slice(elem)`.
//! * **Array** (`[N]T`) → `Type::Array { ty: elem, length: N }`.
//! * **Map** (`map[K]V`) — no direct IR type.  Encoded as
//!   `Type::Apply { base: TypeVar("map"), args: [K, V] }`.  This preserves both
//!   type arguments structurally; `TypeVar("map")` is a sentinel that signals
//!   "Go built-in map" to renderers that special-case it.  The alternative
//!   (`Type::Any`) would silently drop K and V, which is worse.
//!   IR GAP: a dedicated `Type::Map` variant would be cleaner.
//! * **Chan** — encoded as `Type::Apply { base: TypeVar("chan" | "chan<-" |
//!   "<-chan"), args: [elem] }`.  Direction is preserved in the base sentinel.
//!   IR GAP: a dedicated `Type::Chan` variant would be cleaner.
//! * **Func type** → `Type::Apply { base: TypeVar("func"), args: [param...,
//!   result...] }` as a partial encoding.  Parameter/result arity is preserved;
//!   the boundary between inputs and outputs is not encoded at the type level
//!   (a `Type::FuncPointer` variant would be the right fix).
//!   IR GAP: no structural func-pointer type.
//! * **Struct literal** (anonymous) → `Type::Any`.
//!   IR GAP: no inline struct literal type.
//! * **Interface literal** → `Type::Any` for non-empty interfaces.
//!   IR GAP: no inline interface type.
//! * **Constraint union** → `Type::Union(parts)`, with `~T` approximation
//!   terms encoded as `Type::Apply { base: TypeVar("~"), args: [T] }`.
//! * **TypeParam** → `Type::TypeVar(name)` — a use of a generic parameter.
//! * **Basic** types → `Type::Primitive(...)` or `Type::Primitive(Str)` etc.

use nudox_ir::{
    build::*,
    kinds::{
        function::Receiver,
        generics::GenericParam,
        ty::{Primitive, Type, Width},
    },
};

use crate::{
    lower::GoId,
    oracle::{self, TypeKind},
};

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

// ---------------------------------------------------------------------------
// Primary entry point — requires Lowering access for nominal refs
// ---------------------------------------------------------------------------

/// Lower an oracle type node, emitting nominal refs via `low`.
///
/// Every named/alias type produces a `Type::Nominal(RawRef)` via
/// `low.refer(GoId::Item { ... })`, so the type graph is fully connected.
///
/// `low` and the oracle data are disjoint borrows; the borrow checker can
/// verify this at each call site (oracle `Type` lives in the oracle output,
/// `Lowering` owns only its internal index).
pub fn lower_type_with_lowering(t: &oracle::Type, low: &mut Lowering<GoId>) -> Type {
    lower_type_depth_low(t, low, 0)
}

fn lower_type_depth_low(t: &oracle::Type, low: &mut Lowering<GoId>, depth: usize) -> Type {
    if depth >= MAX_DEPTH {
        return Type::Any;
    }
    match t.kind {
        TypeKind::Basic => lower_basic(&t.name),

        // Named and alias types → Nominal(RawRef).
        // `any` (universe alias for the empty interface) → top type.
        // Generic instantiations → Apply { base: Nominal, args }.
        TypeKind::Named | TypeKind::Alias => {
            if t.pkg.is_empty() && t.name == "any" {
                return Type::Any;
            }
            let go_id = GoId::Item {
                import_path: t.pkg.clone(),
                name: t.name.clone(),
            };
            let raw: RawRef = low.refer::<nudox_ir::kinds::Record>(go_id).into_raw();
            let base = Type::Nominal(raw);
            if t.type_args.is_empty() {
                base
            } else {
                let args: Box<[Type]> = t
                    .type_args
                    .iter()
                    .map(|a| lower_type_depth_low(a, low, depth + 1))
                    .collect();
                Type::Apply {
                    base: Box::new(base),
                    args,
                }
            }
        }

        // Type-parameter uses → TypeVar(name).
        // TypeVar is "a reference to a generic parameter by name" — exactly right.
        // SelfType means "the receiver type" — that is simply wrong for TypeParam.
        TypeKind::TypeParam => Type::TypeVar(t.name.clone()),

        TypeKind::Pointer => {
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth_low(e, low, depth + 1))
                .unwrap_or(Type::Any);
            Type::Primitive(Primitive::MutPointer(Box::new(elem)))
        }

        TypeKind::Slice => {
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth_low(e, low, depth + 1))
                .unwrap_or(Type::Any);
            Type::Slice(Box::new(elem))
        }

        TypeKind::Array => {
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth_low(e, low, depth + 1))
                .unwrap_or(Type::Any);
            Type::Array {
                ty: Box::new(elem),
                length: t.len.max(0) as usize,
            }
        }

        // IR GAP: no map type.  Encode as Apply { base: TypeVar("map"), args: [K, V] }.
        // This preserves both type arguments structurally; renderers that understand
        // the "map" sentinel can reconstruct Go map syntax.  Type::Any would lose K/V.
        TypeKind::Map => {
            let key = t
                .key
                .as_deref()
                .map(|k| lower_type_depth_low(k, low, depth + 1))
                .unwrap_or(Type::Any);
            let val = t
                .value
                .as_deref()
                .map(|v| lower_type_depth_low(v, low, depth + 1))
                .unwrap_or(Type::Any);
            Type::Apply {
                base: Box::new(Type::TypeVar("map".to_string())),
                args: Box::new([key, val]),
            }
        }

        // IR GAP: no channel type.  Encode as Apply { base: TypeVar(dir_op), args: [elem] }.
        // Direction ("chan", "chan<-", "<-chan") is preserved in the base sentinel.
        TypeKind::Chan => {
            let op = chan_op(t.dir).to_string();
            let elem = t
                .elem
                .as_deref()
                .map(|e| lower_type_depth_low(e, low, depth + 1))
                .unwrap_or(Type::Any);
            Type::Apply {
                base: Box::new(Type::TypeVar(op)),
                args: Box::new([elem]),
            }
        }

        // IR GAP: no structural func-pointer type.  Encode as
        // Apply { base: TypeVar("func"), args: [params..., results...] }.
        // Arity is preserved; input/output boundary is not encoded at the type level.
        // A dedicated Type::FuncPointer variant would be the correct fix in nudox-ir.
        TypeKind::Func => {
            let mut args: Vec<Type> = t
                .params
                .iter()
                .map(|p| {
                    p.r#type
                        .as_ref()
                        .map(|ty| lower_type_depth_low(ty, low, depth + 1))
                        .unwrap_or(Type::Any)
                })
                .collect();
            for r in t.results.iter() {
                args.push(
                    r.r#type
                        .as_ref()
                        .map(|ty| lower_type_depth_low(ty, low, depth + 1))
                        .unwrap_or(Type::Any),
                );
            }
            Type::Apply {
                base: Box::new(Type::TypeVar("func".to_string())),
                args: args.into_boxed_slice(),
            }
        }

        // IR GAP: no inline struct literal type.  Falls back to Any.
        TypeKind::Struct => Type::Any,

        TypeKind::Interface => {
            if t.is_empty_interface() {
                // empty interface = `any` = top type
                Type::Any
            } else {
                // IR GAP: no faithful inline interface literal type.
                // Anonymous non-empty interfaces cannot be faithfully represented
                // in the new IR's Type algebra.  Type::Any is the honest fallback.
                Type::Any
            }
        }

        // Constraint union → Type::Union.
        // Approximation terms (`~T`) are encoded as Apply { base: TypeVar("~"), args: [T] }.
        // Dropping the tilde would change the constraint's meaning: `~int` matches any
        // type whose underlying type is int (e.g. `type MyInt int`), while bare `int`
        // matches only `int` itself.
        TypeKind::Union => {
            let parts: Box<[Type]> = t
                .terms
                .iter()
                .map(|term| {
                    let inner = term
                        .r#type
                        .as_ref()
                        .map(|inner| lower_type_depth_low(inner, low, depth + 1))
                        .unwrap_or(Type::Any);
                    if term.tilde {
                        // Encode tilde-approximation: ~T ≠ T
                        Type::Apply {
                            base: Box::new(Type::TypeVar("~".to_string())),
                            args: Box::new([inner]),
                        }
                    } else {
                        inner
                    }
                })
                .collect();
            Type::Union(parts)
        }

        TypeKind::Tuple => {
            let parts: Box<[Type]> = t
                .types
                .iter()
                .map(|inner| lower_type_depth_low(inner, low, depth + 1))
                .collect();
            Type::Tuple(parts)
        }

        TypeKind::Invalid => Type::Any,
    }
}

// ---------------------------------------------------------------------------
// chan direction sentinel
// ---------------------------------------------------------------------------

fn chan_op(dir: oracle::ChanDir) -> &'static str {
    match dir {
        oracle::ChanDir::Send => "chan<-",
        oracle::ChanDir::Recv => "<-chan",
        oracle::ChanDir::Both => "chan",
    }
}

// ---------------------------------------------------------------------------
// Basic type lowering (no Lowering needed)
// ---------------------------------------------------------------------------

/// Map a Go basic-type name onto the IR primitive algebra.
///
/// Notable decisions:
/// * `byte` → `Type::Any`.
///   UNCERTAINTY: `byte` is a universe alias for `uint8`, but we cannot
///   produce a nominal `RawRef` without Lowering access from this path.
///   Callers that need a precise `byte` type should use
///   `lower_type_with_lowering` on the containing oracle Type node instead.
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
        // `byte` is a distinct universe alias for uint8.
        // We cannot produce a nominal RawRef in this pure-function context.
        // Callers that need precision must use lower_type_with_lowering.
        "byte" => Type::Any,
        "rune" => Type::Primitive(Primitive::Char),
        "float32" => Type::Primitive(Primitive::Float(Width::W32)),
        "float64" | "float" => Type::Primitive(Primitive::Float(Width::W64)),
        // complex64/complex128, unsafe.Pointer, error, comparable — no IR match.
        _ => Type::Any,
    }
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// Lower a receiver kind from oracle method metadata into
/// [`nudox_ir::kinds::function::Receiver`].
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

// ---------------------------------------------------------------------------
// Generic parameter declarations
// ---------------------------------------------------------------------------

/// Lower a single type-parameter declaration into a [`GenericParam`].
///
/// Constraint bounds call back into `lower_type_with_lowering` so that named
/// constraint interfaces produce `Type::Nominal(RawRef)` rather than `Any`.
pub fn lower_type_param_decl(
    tp: &oracle::TypeParamDecl,
    low: &mut Lowering<GoId>,
) -> GenericParam {
    let bounds: Box<[Type]> = tp
        .constraint
        .as_ref()
        .map(|c| {
            if c.is_empty_interface() {
                // `any` constraint = unconstrained
                Vec::<Type>::new().into_boxed_slice()
            } else {
                let t = lower_type_with_lowering(c, low);
                vec![t].into_boxed_slice()
            }
        })
        .unwrap_or_else(|| Vec::new().into_boxed_slice());

    GenericParam::Type {
        name: tp.name.clone(),
        bounds,
        default: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::kinds::ty::{Primitive, Width};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_low() -> Lowering<GoId> {
        use nudox_ir::{
            build::Symbol,
            entry::{Visibility},
            package::PackageId,
        };
        let sym = Symbol {
            name: "(root)".into(),
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
        Lowering::new(PackageId::path("test"), sym)
    }

    // ── Primitive lowering (stateless) ────────────────────────────────────────

    #[test]
    fn basic_primitives() {
        assert!(matches!(
            lower_basic("bool"),
            Type::Primitive(Primitive::Bool)
        ));
        assert!(matches!(
            lower_basic("string"),
            Type::Primitive(Primitive::Str)
        ));
        assert!(matches!(lower_basic("int32"), Type::I32));
        assert!(matches!(
            lower_basic("float64"),
            Type::Primitive(Primitive::Float(Width::W64))
        ));
        assert!(matches!(
            lower_basic("rune"),
            Type::Primitive(Primitive::Char)
        ));
    }

    #[test]
    fn untyped_prefix_stripped() {
        assert!(matches!(
            lower_basic("untyped int"),
            Type::Primitive(Primitive::Integer {
                signed: true,
                width: Width::Arch
            })
        ));
    }

    // ── Structural type lowering (with lowering) ──────────────────────────────

    #[test]
    fn slice_lowering() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Slice,
            elem: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "int32".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(matches!(lower_type_with_lowering(&t, &mut low), Type::Slice(_)));
    }

    #[test]
    fn array_lowering() {
        let mut low = make_low();
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
        match lower_type_with_lowering(&t, &mut low) {
            Type::Array { length, .. } => assert_eq!(length, 8),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn pointer_lowering() {
        let mut low = make_low();
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
            lower_type_with_lowering(&t, &mut low),
            Type::Primitive(Primitive::MutPointer(_))
        ));
    }

    // ── Item 1: Named type → Type::Nominal, not Type::Any ───────────────────

    #[test]
    fn named_type_lowered_to_nominal() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Named,
            pkg: "example.com/m/shapes".to_string(),
            name: "Rect".to_string(),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::Nominal(_) => {}
            other => panic!("named type must lower to Nominal, got {other:?}"),
        }
    }

    #[test]
    fn named_type_any_stays_any() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Named,
            pkg: "".to_string(),
            name: "any".to_string(),
            ..Default::default()
        };
        assert!(matches!(lower_type_with_lowering(&t, &mut low), Type::Any));
    }

    #[test]
    fn generic_named_type_lowered_to_apply() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Named,
            pkg: "example.com/m".to_string(),
            name: "List".to_string(),
            type_args: Box::new([oracle::Type {
                kind: TypeKind::Basic,
                name: "int32".to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::Apply { base, args } => {
                assert!(matches!(*base, Type::Nominal(_)), "base must be Nominal");
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0], Type::I32));
            }
            other => panic!("generic named type must lower to Apply, got {other:?}"),
        }
    }

    // ── Item 2: TypeParam → TypeVar, not SelfType ────────────────────────────

    #[test]
    fn type_param_lowered_to_typevar() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::TypeParam,
            name: "T".to_string(),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::TypeVar(n) => assert_eq!(n, "T"),
            other => panic!("TypeParam must lower to TypeVar, got {other:?}"),
        }
    }

    // ── Item 6: Tilde constraint terms encoded ────────────────────────────────

    #[test]
    fn union_with_tilde_encodes_approximation() {
        use crate::oracle::{Term, Type as OType};
        let mut low = make_low();
        let t = OType {
            kind: TypeKind::Union,
            terms: Box::new([
                Term {
                    tilde: true,
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
        match lower_type_with_lowering(&t, &mut low) {
            Type::Union(parts) => {
                assert_eq!(parts.len(), 2);
                // First term is tilde: must be Apply { base: TypeVar("~"), args: [int] }
                match &parts[0] {
                    Type::Apply { base, args } => {
                        assert!(
                            matches!(**base, Type::TypeVar(ref n) if n == "~"),
                            "tilde term base must be TypeVar(\"~\")"
                        );
                        assert_eq!(args.len(), 1);
                    }
                    other => panic!("tilde term must be Apply{{TypeVar(\"~\")}}, got {other:?}"),
                }
                // Second term is bare int — must not be Apply.
                assert!(
                    !matches!(&parts[1], Type::Apply { .. }),
                    "bare term must not be Apply"
                );
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn union_bare_terms_not_wrapped() {
        use crate::oracle::{Term, Type as OType};
        let mut low = make_low();
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
        match lower_type_with_lowering(&t, &mut low) {
            Type::Union(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected Union, got {other:?}"),
        }
    }

    // ── IR GAP: Map encodes K, V via Apply ───────────────────────────────────

    #[test]
    fn map_encoded_as_apply_with_kv() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Map,
            key: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "string".to_string(),
                ..Default::default()
            })),
            value: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "int32".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::Apply { base, args } => {
                assert!(
                    matches!(*base, Type::TypeVar(ref n) if n == "map"),
                    "map base must be TypeVar(\"map\")"
                );
                assert_eq!(args.len(), 2, "map must have K and V args");
                assert!(matches!(args[0], Type::Primitive(Primitive::Str)));
                assert!(matches!(args[1], Type::I32));
            }
            other => panic!("map must lower to Apply, got {other:?}"),
        }
    }

    // ── IR GAP: Chan encodes direction and elem ───────────────────────────────

    #[test]
    fn chan_encoded_as_apply_with_dir() {
        use crate::oracle::ChanDir;
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Chan,
            dir: ChanDir::Send,
            elem: Some(Box::new(oracle::Type {
                kind: TypeKind::Basic,
                name: "int32".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::Apply { base, args } => {
                assert!(
                    matches!(*base, Type::TypeVar(ref n) if n == "chan<-"),
                    "send chan must use sentinel chan<-"
                );
                assert_eq!(args.len(), 1);
            }
            other => panic!("chan must lower to Apply, got {other:?}"),
        }
    }
}
