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
//! * **Func type** → `Type::FunctionPointer { params, ret, abi: None }`.
//!   Single return → `ret = Some(T)`.  Multi-return → `ret = Some(Tuple(...))`.
//!   No return → `ret = None`.  Previously used `Apply { TypeVar("func") }`
//!   which abused `TypeVar` and lost the input/output boundary.
//! * **Struct literal** (anonymous) → `Type::AnonymousRecord { form: Struct, members }`.
//!   Previously degraded to `Type::Any`; now uses the new IR variant.
//!   KNOWN LIMITATION: FunctionPointer in field types loses param names — IR gap.
//! * **Interface literal** → `Type::AnonymousRecord { form: Interface, members }` for
//!   non-empty interfaces; each method becomes an `AnonField` with a `FunctionPointer` ty.
//!   Previously degraded to `Type::Any`; now uses the new IR variant.
//!   KNOWN LIMITATION: FunctionPointer loses param names — IR gap flagged for later pass.
//! * **Constraint union** → `Type::Union(parts)`, with `~T` approximation
//!   terms encoded as `Type::Apply { base: TypeVar("~"), args: [T] }`.
//! * **TypeParam** → `Type::TypeVar(name)` — a use of a generic parameter.
//! * **Basic** types → `Type::Primitive(...)` or `Type::Primitive(Str)` etc.

use nudox_ir::{
    build::*,
    kinds::{
        function::Receiver,
        generics::GenericParam,
        ty::{AnonField, AnonRecordForm, Primitive, TupleElement, Type, Width},
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

        // Go `func(P...) R` anonymous function type.
        //
        // Previously encoded as `Apply { base: TypeVar("func"), args: [P..., R...] }`,
        // which abused `TypeVar` (a "reference to a generic parameter by name") as a
        // sentinel and lost the boundary between inputs and outputs at the type level.
        //
        // Now lowered to `Type::FunctionPointer { params, ret, abi }`:
        //   - `params`: all parameter types in order.
        //   - `ret`:    single return → `Some(Box<Type>)`.
        //               multi-return → `Some(Box<Type::Tuple([Positional(R1), ...])))`.
        //               no return    → `None`.
        //   - `abi`:    None — Go has no calling-convention annotation.
        //
        // Map/chan sentinels stay as Apply { base: TypeVar("map"|"chan"|...) } because:
        //   1. They are built-in collection types, not callable types — `FunctionPointer`
        //      would be semantically wrong.
        //   2. They do preserve their type arguments (K, V / elem + dir) structurally.
        //   3. The IR has no dedicated `Type::Map` or `Type::Chan` variant; until one
        //      is added the sentinel encoding is the least-lossy available option.
        TypeKind::Func => {
            let param_types: Box<[Type]> = t
                .params
                .iter()
                .map(|p| {
                    p.r#type
                        .as_ref()
                        .map(|ty| lower_type_depth_low(ty, low, depth + 1))
                        .unwrap_or(Type::Any)
                })
                .collect();

            let ret: Option<Box<Type>> = match t.results.len() {
                0 => None,
                1 => {
                    let r = &t.results[0];
                    let ty = r
                        .r#type
                        .as_ref()
                        .map(|ty| lower_type_depth_low(ty, low, depth + 1))
                        .unwrap_or(Type::Any);
                    Some(Box::new(ty))
                }
                _ => {
                    // Multi-value return: collect results into a positional Tuple.
                    let elems: Box<[TupleElement]> = t
                        .results
                        .iter()
                        .map(|r| {
                            let ty = r
                                .r#type
                                .as_ref()
                                .map(|ty| lower_type_depth_low(ty, low, depth + 1))
                                .unwrap_or(Type::Any);
                            TupleElement::Positional(ty)
                        })
                        .collect();
                    Some(Box::new(Type::Tuple(elems)))
                }
            };

            Type::FunctionPointer {
                params: param_types,
                ret,
                abi: None, // Go has no calling-convention annotation.
            }
        }

        TypeKind::Struct => {
            // Anonymous struct literal: `struct { X int; Y string }`.
            // Now representable as Type::AnonymousRecord { form: Struct, members }.
            //
            // KNOWN LIMITATION: FunctionPointer carries param types but NOT param names.
            // Go struct fields that are function types lose their parameter names here.
            // This is an IR gap in FunctionPointer, flagged for a later pass.
            let members: Box<[AnonField]> = t
                .fields
                .iter()
                .map(|f| AnonField {
                    name: f.name.clone(),
                    ty: f.r#type
                        .as_ref()
                        .map(|ft| lower_type_depth_low(ft, low, depth + 1))
                        .unwrap_or(Type::Any),
                    optional: false,
                    readonly: false,
                })
                .collect();
            Type::AnonymousRecord {
                form: AnonRecordForm::Struct,
                members,
            }
        }

        TypeKind::Interface => {
            if t.is_empty_interface() {
                // empty interface = `any` = top type
                Type::Any
            } else {
                // Anonymous interface literal: `interface { Foo() bool }`.
                // Now representable as Type::AnonymousRecord { form: Interface, members }.
                // Each method's `signature` is a TypeKind::Func oracle Type; recursing
                // into it via lower_type_depth_low produces a Type::FunctionPointer.
                //
                // KNOWN LIMITATION: FunctionPointer carries param types but NOT param names.
                // `func(ctx context.Context) error` loses `ctx`. IR gap flagged for later pass.
                let members: Box<[AnonField]> = t
                    .explicit_methods
                    .iter()
                    .map(|m| {
                        let ty = m.signature
                            .as_ref()
                            .map(|sig| lower_type_depth_low(sig, low, depth + 1))
                            .unwrap_or(Type::Any);
                        AnonField {
                            name: m.name.clone(),
                            ty,
                            optional: false,
                            readonly: false,
                        }
                    })
                    .collect();
                Type::AnonymousRecord {
                    form: AnonRecordForm::Interface,
                    members,
                }
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
            let parts: Box<[TupleElement]> = t
                .types
                .iter()
                .map(|inner| TupleElement::Positional(lower_type_depth_low(inner, low, depth + 1)))
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
        // Go has no declaration-site variance (`in`/`out`): the language only
        // supports use-site variance via type parameter constraints and
        // interface embedding. `None` is the correct value — it means
        // "unspecified / invariant by default", which is what every Go type
        // parameter is.
        variance: None,
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

    // ── Func type → Type::FunctionPointer (not TypeVar sentinel) ─────────────

    /// `func(string) int` must lower to `Type::FunctionPointer { params: [Str],
    /// ret: Some(I32), abi: None }` — not the old `Apply { TypeVar("func") }`.
    #[test]
    fn func_type_lowered_to_function_pointer() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Func,
            params: Box::new([oracle::Param {
                name: "s".to_string(),
                r#type: Some(oracle::Type {
                    kind: TypeKind::Basic,
                    name: "string".to_string(),
                    ..Default::default()
                }),
            }]),
            results: Box::new([oracle::Param {
                name: "".to_string(),
                r#type: Some(oracle::Type {
                    kind: TypeKind::Basic,
                    name: "int32".to_string(),
                    ..Default::default()
                }),
            }]),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::FunctionPointer { params, ret, abi } => {
                assert_eq!(params.len(), 1, "func(string) must have 1 param");
                assert!(
                    matches!(params[0], Type::Primitive(Primitive::Str)),
                    "param must be Str, got {:?}",
                    params[0]
                );
                let ret_ty = ret.expect("func(string) int must have a return type");
                assert!(
                    matches!(*ret_ty, Type::I32),
                    "return type must be I32, got {ret_ty:?}"
                );
                assert!(abi.is_none(), "Go func types have no ABI annotation");
            }
            // Must NOT fall through to Apply or any other variant.
            other => panic!(
                "func(string) int must lower to Type::FunctionPointer, got {other:?}"
            ),
        }
    }

    /// `func(int, bool) (string, error)` — multi-return must be wrapped in a
    /// positional Tuple as the `ret` field.
    #[test]
    fn func_type_multi_return_wrapped_in_tuple() {
        use nudox_ir::kinds::ty::TupleElement;
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Func,
            params: Box::new([
                oracle::Param {
                    name: "_0".to_string(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "int32".to_string(),
                        ..Default::default()
                    }),
                },
                oracle::Param {
                    name: "_1".to_string(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "bool".to_string(),
                        ..Default::default()
                    }),
                },
            ]),
            results: Box::new([
                oracle::Param {
                    name: "".to_string(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "string".to_string(),
                        ..Default::default()
                    }),
                },
                oracle::Param {
                    name: "".to_string(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "error".to_string(), // lowered to Any (universe type)
                        ..Default::default()
                    }),
                },
            ]),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::FunctionPointer { params, ret, .. } => {
                assert_eq!(params.len(), 2, "2 input params");
                let ret_ty = ret.expect("multi-return func must have ret");
                match *ret_ty {
                    Type::Tuple(ref elems) => {
                        assert_eq!(elems.len(), 2, "2 return values wrapped in Tuple");
                        // All multi-return elements are positional (Go has no named returns
                        // at the type level — names are local to the function body).
                        for e in elems.iter() {
                            assert!(
                                matches!(e, TupleElement::Positional(_)),
                                "multi-return elements must be Positional, got {e:?}"
                            );
                        }
                    }
                    other => panic!("multi-return must wrap in Tuple, got {other:?}"),
                }
            }
            other => panic!("func type must lower to FunctionPointer, got {other:?}"),
        }
    }

    /// `func()` with no params and no return must lower to
    /// `FunctionPointer { params: [], ret: None, abi: None }`.
    #[test]
    fn func_type_no_params_no_return() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Func,
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::FunctionPointer { params, ret, abi } => {
                assert!(params.is_empty(), "func() must have 0 params");
                assert!(ret.is_none(), "func() must have no return type");
                assert!(abi.is_none(), "func() must have no ABI");
            }
            other => panic!("empty func type must lower to FunctionPointer, got {other:?}"),
        }
    }

    // ── AnonymousRecord for struct and interface ──────────────────────────────

    /// Anonymous `struct { X int }` must lower to `AnonymousRecord { form: Struct }`.
    #[test]
    fn anonymous_struct_lowers_to_anonymous_record_struct() {
        use nudox_ir::kinds::ty::AnonRecordForm;
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Struct,
            fields: Box::new([oracle::StructField {
                name: "X".to_string(),
                r#type: Some(oracle::Type {
                    kind: TypeKind::Basic,
                    name: "int32".to_string(),
                    ..Default::default()
                }),
                tag: String::new(),
                embedded: false,
                exported: true,
            }]),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low) {
            Type::AnonymousRecord { form, members } => {
                assert_eq!(form, AnonRecordForm::Struct, "struct kind must produce Struct form");
                assert_eq!(members.len(), 1, "one field");
                assert_eq!(members[0].name, "X");
                assert!(!members[0].optional);
                assert!(!members[0].readonly);
            }
            other => panic!("anonymous struct must lower to AnonymousRecord, got {other:?}"),
        }
    }

    /// Empty anonymous `interface{}` must remain `Type::Any` (it means `any`).
    /// Non-empty anonymous `interface { Foo() bool }` must lower to
    /// `AnonymousRecord { form: Interface }`.
    #[test]
    fn empty_interface_stays_any_non_empty_becomes_anonymous_record() {
        use nudox_ir::kinds::ty::AnonRecordForm;
        let mut low = make_low();

        // Empty interface = Type::Any.
        let empty = oracle::Type {
            kind: TypeKind::Interface,
            ..Default::default()
        };
        assert!(
            matches!(lower_type_with_lowering(&empty, &mut low), Type::Any),
            "empty interface must remain Type::Any"
        );

        // Non-empty interface: one explicit method `Foo() bool`.
        let sig = oracle::Type {
            kind: TypeKind::Func,
            results: Box::new([oracle::Param {
                name: String::new(),
                r#type: Some(oracle::Type {
                    kind: TypeKind::Basic,
                    name: "bool".to_string(),
                    ..Default::default()
                }),
            }]),
            ..Default::default()
        };
        let non_empty = oracle::Type {
            kind: TypeKind::Interface,
            explicit_methods: Box::new([oracle::MethodSig {
                name: "Foo".to_string(),
                exported: true,
                signature: Some(sig),
                pos: None,
                pkg: String::new(),
            }]),
            all_methods: Box::new([]),
            ..Default::default()
        };
        match lower_type_with_lowering(&non_empty, &mut low) {
            Type::AnonymousRecord { form, members } => {
                assert_eq!(
                    form,
                    AnonRecordForm::Interface,
                    "non-empty interface must produce Interface form"
                );
                assert_eq!(members.len(), 1, "one method");
                assert_eq!(members[0].name, "Foo");
                assert!(
                    matches!(members[0].ty, Type::FunctionPointer { .. }),
                    "method ty must be FunctionPointer, got {:?}",
                    members[0].ty
                );
            }
            other => panic!(
                "non-empty interface must lower to AnonymousRecord, got {other:?}"
            ),
        }
    }
}
