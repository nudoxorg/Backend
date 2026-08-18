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

use std::collections::HashSet;

use nudox_ir::{
    build::{EcosystemId, Lowering, RawRef},
    foreign::ForeignKey,
    kinds::{
        function::Receiver,
        generics::GenericParam,
        ty::{AnonField, AnonRecordForm, Primitive, TupleElement, Type, Width},
    },
};

use crate::go::{
    lower::GoId,
    oracle::{self, TypeKind},
};

/// Defensive recursion bound.  Anonymous Go types cannot cycle, so this only
/// guards against malformed oracle output.
const MAX_DEPTH: usize = 64;

/// The cross-package key for a Go type outside the loaded module.
///
/// # What Go can and cannot state
///
/// `oracle::Type::pkg` is the canonical **import path** and is globally stable —
/// `sync.Mutex` produces byte-identical bytes in every package that names it,
/// which is the best raw material any producer in this workspace has.
///
/// What it is *not* is the **module path**, which is what
/// `PackageDescriptor::go` uses as the lineage name. `go.uber.org/zap/zapcore`
/// cannot be split into module + subpath without a go.mod answer the oracle does
/// not currently produce. Emitting `ForeignOrigin::Namespace` states exactly
/// that, and upgrades to `Package` the day the oracle reports module paths —
/// whereas the code this replaces synthesised `PackageId::path(import_path)`,
/// which reads like a resolved package identity and is not one.
fn go_foreign_key(import_path: &str, name: &str) -> ForeignKey {
    let ecosystem = EcosystemId::new("go");
    if import_path.is_empty() {
        // Universe scope: `error`, `comparable`, `any`. These are declared by
        // the language, not by a package. The previous encoding was
        // `PackageId::path("")`, which collapsed every predeclared identifier
        // into one empty pseudo-package.
        ForeignKey::in_universe(ecosystem, name, name)
    } else {
        ForeignKey::in_namespace(
            ecosystem,
            import_path,
            format!("{import_path}.{name}"),
            name,
        )
    }
}

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
/// Every named/alias type produces a `Type::Nominal(RawRef)`, but the two
/// named/alias branches below emit that `RawRef` from two different arenas
/// depending on `local`:
///
/// * A package present in `local` (i.e. one this oracle invocation actually
///   loaded and will emit `decls` for) uses `low.refer(GoId::Item { ... })`
///   — a same-package forward reference that [`Lowering::finish`] requires
///   to be `declare`d before the pass ends.
/// * Everything else — the Go standard library (`sync`, `io`, `time`, …),
///   universe-scope predeclared identifiers (`error`, `comparable`; the
///   oracle marks these with an empty `pkg`), or a dependency from a
///   different Go module entirely — uses `low.refer_import` into a separate
///   *import* arena that `finish` does not validate.
///
/// Treating every named type as local (the previous behavior) crashes
/// [`Lowering::finish`] with `LoweringError::Undeclared` on essentially
/// every real-world Go package: the oracle only emits `decls` for packages
/// *within the loaded module* (see `oracle/main.go`'s `extract`), so a
/// same-package-style `refer()` on `sync.Mutex` or even the bare `error`
/// interface can never be satisfied. This is not a hypothetical — it is
/// exactly what running the real oracle over `go.uber.org/zap` produced
/// before this fix (see `tests/real_package.rs`).
///
/// `low` and the oracle data are disjoint borrows; the borrow checker can
/// verify this at each call site (oracle `Type` lives in the oracle output,
/// `Lowering` owns only its internal index).
pub fn lower_type_with_lowering(
    t: &oracle::Type,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Type {
    lower_type_depth_low(t, low, local, 0)
}

fn lower_type_depth_low(
    t: &oracle::Type,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
    depth: usize,
) -> Type {
    // Our own guard, not the language's and not the oracle's: a real type
    // exists below here and we chose not to walk it. Raising MAX_DEPTH closes
    // this with no new information from anywhere, which is exactly what
    // separates it from `OracleGap`.
    if depth >= MAX_DEPTH {
        return Type::TRUNCATED;
    }
    match t.kind {
        TypeKind::Basic => lower_basic(&t.name),

        // Named and alias types → Nominal(RawRef).
        // Generic instantiations → Apply { base: Nominal, args }.
        TypeKind::Named | TypeKind::Alias => {
            // `any` is Go's genuine top type — the universe alias for
            // `interface{}`, which every value implements. This is the one
            // place in this producer where `Type::Any` is the right answer,
            // and it stays.
            if t.pkg.is_empty() && t.name == "any" {
                return Type::Any;
            }
            let go_id = GoId::Item {
                import_path: t.pkg.clone(),
                name: t.name.clone(),
            };
            let raw: RawRef = if local.contains(&t.pkg) {
                // Same-package (or same-module, different-package) reference:
                // the oracle will emit a `decls` entry for it, so a
                // same-arena forward reference is correct and `finish` will
                // resolve it.
                low.refer::<nudox_ir::kinds::Record>(go_id).into_raw()
            } else {
                // Foreign: stdlib, a universe-scope predeclared identifier
                // (empty `pkg`), or a dependency the oracle never loaded.
                // See this function's doc comment for why `refer` would
                // crash `finish` here.
                low.refer_import::<nudox_ir::kinds::Record>(go_foreign_key(&t.pkg, &t.name))
                    .into_raw()
            };
            let base = Type::Nominal(raw);
            if t.type_args.is_empty() {
                base
            } else {
                let args: Box<[Type]> = t
                    .type_args
                    .iter()
                    .map(|a| lower_type_depth_low(a, low, local, depth + 1))
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
            let elem = t.elem.as_deref().map_or(Type::ORACLE_GAP, |e| {
                lower_type_depth_low(e, low, local, depth + 1)
            });
            Type::Primitive(Primitive::MutPointer(Box::new(elem)))
        }

        TypeKind::Slice => {
            let elem = t.elem.as_deref().map_or(Type::ORACLE_GAP, |e| {
                lower_type_depth_low(e, low, local, depth + 1)
            });
            Type::Slice(Box::new(elem))
        }

        TypeKind::Array => {
            let elem = t.elem.as_deref().map_or(Type::ORACLE_GAP, |e| {
                lower_type_depth_low(e, low, local, depth + 1)
            });
            Type::Array {
                ty: Box::new(elem),
                length: t.len.max(0) as usize,
            }
        }

        // IR GAP: no map type.  Encode as Apply { base: TypeVar("map"), args: [K, V] }.
        // This preserves both type arguments structurally; renderers that understand
        // the "map" sentinel can reconstruct Go map syntax.  Type::Any would lose K/V.
        TypeKind::Map => {
            let key = t.key.as_deref().map_or(Type::ORACLE_GAP, |k| {
                lower_type_depth_low(k, low, local, depth + 1)
            });
            let val = t.value.as_deref().map_or(Type::ORACLE_GAP, |v| {
                lower_type_depth_low(v, low, local, depth + 1)
            });
            Type::Apply {
                base: Box::new(Type::TypeVar("map".to_string())),
                args: Box::new([key, val]),
            }
        }

        // IR GAP: no channel type.  Encode as Apply { base: TypeVar(dir_op), args: [elem] }.
        // Direction ("chan", "chan<-", "<-chan") is preserved in the base sentinel.
        TypeKind::Chan => {
            let op = chan_op(t.dir).to_string();
            let elem = t.elem.as_deref().map_or(Type::ORACLE_GAP, |e| {
                lower_type_depth_low(e, low, local, depth + 1)
            });
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
                    p.r#type.as_ref().map_or(Type::ORACLE_GAP, |ty| {
                        lower_type_depth_low(ty, low, local, depth + 1)
                    })
                })
                .collect();

            let ret: Option<Box<Type>> = match t.results.len() {
                0 => None,
                1 => {
                    let r = &t.results[0];
                    let ty = r.r#type.as_ref().map_or(Type::ORACLE_GAP, |ty| {
                        lower_type_depth_low(ty, low, local, depth + 1)
                    });
                    Some(Box::new(ty))
                }
                _ => {
                    // Multi-value return: collect results into a positional Tuple.
                    let elems: Box<[TupleElement]> = t
                        .results
                        .iter()
                        .map(|r| {
                            let ty = r.r#type.as_ref().map_or(Type::ORACLE_GAP, |ty| {
                                lower_type_depth_low(ty, low, local, depth + 1)
                            });
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
                    ty: f.r#type.as_ref().map_or(Type::ORACLE_GAP, |ft| {
                        lower_type_depth_low(ft, low, local, depth + 1)
                    }),
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
                // `interface{}` — Go's genuine top type, satisfied by every
                // value. A real type, not an absence: `Type::Any` is correct
                // here in the post-CC-2 sense of that variant.
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
                        let ty = m.signature.as_ref().map_or(Type::ORACLE_GAP, |sig| {
                            lower_type_depth_low(sig, low, local, depth + 1)
                        });
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
                    let inner = term.r#type.as_ref().map_or(Type::ORACLE_GAP, |inner| {
                        lower_type_depth_low(inner, low, local, depth + 1)
                    });
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
                .map(|inner| {
                    TupleElement::Positional(lower_type_depth_low(inner, low, local, depth + 1))
                })
                .collect();
            Type::Tuple(parts)
        }

        // `go/types` already reported this as invalid and already logged. We
        // are propagating its failure, not adding a claim of our own.
        TypeKind::Invalid => Type::ORACLE_GAP,
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
/// * `byte` → `Type::U8`. The Go spec makes `byte` an **alias** for `uint8`,
///   not a distinct type: `[]byte` and `[]uint8` are the same type and are
///   mutually assignable without conversion. The previous `Type::Any` was not
///   a conservative degradation, it was wrong — it claimed a `byte` parameter
///   accepted anything. Only the alias *spelling* is lost, and no IR slot for
///   a spelling exists to lose it to.
/// * `rune` → `Primitive::Char`.
/// * `complex64`/`complex128`, `unsafe.Pointer`, and any other universe name →
///   `UnknownType::NoIrRepresentation { construct: name }`. The producer knows
///   exactly what it is looking at; this IR has no slot for it. Naming the
///   construct keeps `complex128` and `unsafe.Pointer` distinguishable, which
///   one shared `Type::Any` opcode did not.
///
/// The trailing `_` arm matches an **open string domain**, not an enum, so it
/// cannot be made exhaustive — but it must never yield a bare `Type::Any`,
/// which would re-collapse everything it catches into one opcode.
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
        // uintptr: pointer-sized unsigned int, not a pointer.
        "uint" | "uintptr" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::Arch,
        }),
        "uint16" => Type::U16,
        "uint32" => Type::U32,
        "uint64" => Type::U64,
        // `byte` is an *alias* for uint8 — the same type, by spec. See the doc
        // comment for why `Type::Any` here was wrong rather than cautious.
        "uint8" | "byte" => Type::U8,
        "rune" => Type::Primitive(Primitive::Char),
        "float32" => Type::Primitive(Primitive::Float(Width::W32)),
        "float64" | "float" => Type::Primitive(Primitive::Float(Width::W64)),
        // complex64/complex128, unsafe.Pointer, error, comparable — known
        // constructs with no IR slot. Keep the name so they stay distinct.
        other => Type::no_ir_representation(other),
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
    local: &HashSet<String>,
) -> GenericParam {
    let bounds: Box<[Type]> = tp.constraint.as_ref().map_or_else(
        || Vec::new().into_boxed_slice(),
        |c| {
            if c.is_empty_interface() {
                // `any` constraint = unconstrained
                Vec::<Type>::new().into_boxed_slice()
            } else {
                let t = lower_type_with_lowering(c, low, local);
                vec![t].into_boxed_slice()
            }
        },
    );

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
        use nudox_ir::{build::Symbol, entry::Visibility, package::PackageId};
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
        assert!(matches!(
            lower_type_with_lowering(&t, &mut low, &HashSet::new()),
            Type::Slice(_)
        ));
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
            lower_type_with_lowering(&t, &mut low, &HashSet::new()),
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
            Type::Nominal(_) => {}
            other => panic!("named type must lower to Nominal, got {other:?}"),
        }
    }

    #[test]
    fn named_type_any_stays_any() {
        let mut low = make_low();
        let t = oracle::Type {
            kind: TypeKind::Named,
            pkg: String::new(),
            name: "any".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            lower_type_with_lowering(&t, &mut low, &HashSet::new()),
            Type::Any
        ));
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
            Type::TypeVar(n) => assert_eq!(n, "T"),
            other => panic!("TypeParam must lower to TypeVar, got {other:?}"),
        }
    }

    // ── Item 6: Tilde constraint terms encoded ────────────────────────────────

    #[test]
    fn union_with_tilde_encodes_approximation() {
        use crate::go::oracle::{Term, Type as OType};
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
        use crate::go::oracle::{Term, Type as OType};
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
        use crate::go::oracle::ChanDir;
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
                pos: None,
            }]),
            results: Box::new([oracle::Param {
                name: String::new(),
                r#type: Some(oracle::Type {
                    kind: TypeKind::Basic,
                    name: "int32".to_string(),
                    ..Default::default()
                }),
                pos: None,
            }]),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
            other => panic!("func(string) int must lower to Type::FunctionPointer, got {other:?}"),
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
                    pos: None,
                },
                oracle::Param {
                    name: "_1".to_string(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "bool".to_string(),
                        ..Default::default()
                    }),
                    pos: None,
                },
            ]),
            results: Box::new([
                oracle::Param {
                    name: String::new(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "string".to_string(),
                        ..Default::default()
                    }),
                    pos: None,
                },
                oracle::Param {
                    name: String::new(),
                    r#type: Some(oracle::Type {
                        kind: TypeKind::Basic,
                        name: "error".to_string(), // lowered to Any (universe type)
                        ..Default::default()
                    }),
                    pos: None,
                },
            ]),
            ..Default::default()
        };
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
            Type::FunctionPointer { params, ret, .. } => {
                assert_eq!(params.len(), 2, "2 input params");
                let ret_ty = ret.expect("multi-return func must have ret");
                match *ret_ty {
                    Type::Tuple(ref elems) => {
                        assert_eq!(elems.len(), 2, "2 return values wrapped in Tuple");
                        // All multi-return elements are positional (Go has no named returns
                        // at the type level — names are local to the function body).
                        for e in elems {
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
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
        match lower_type_with_lowering(&t, &mut low, &HashSet::new()) {
            Type::AnonymousRecord { form, members } => {
                assert_eq!(
                    form,
                    AnonRecordForm::Struct,
                    "struct kind must produce Struct form"
                );
                assert_eq!(members.len(), 1, "one field");
                assert_eq!(members[0].name, "X");
                assert!(!members[0].optional);
                assert!(!members[0].readonly);
            }
            other => panic!("anonymous struct must lower to AnonymousRecord, got {other:?}"),
        }
    }

    // ── CC-2: `Type::Any` means *top type* and nothing else ──────────────────

    /// Go's two top-type spellings survive CC-2 as `Type::Any`; nothing else
    /// in this producer does.
    ///
    /// `any` and `interface{}` are real types — every value implements the
    /// empty method set. They are not gaps and must not be reported as such.
    #[test]
    fn only_gos_real_top_type_stays_any() {
        let mut low = make_low();
        let universe_any = oracle::Type {
            kind: TypeKind::Named,
            pkg: String::new(),
            name: "any".to_string(),
            ..Default::default()
        };
        assert_eq!(
            lower_type_with_lowering(&universe_any, &mut low, &HashSet::new()),
            Type::Any
        );
    }

    /// An oracle `Invalid` node is `OracleGap` — `go/types` already failed and
    /// already reported; the producer is propagating, not claiming a top type.
    #[test]
    fn invalid_oracle_node_is_an_oracle_gap() {
        use nudox_ir::kinds::UnknownType;
        let mut low = make_low();
        let invalid = oracle::Type {
            kind: TypeKind::Invalid,
            ..Default::default()
        };
        assert_eq!(
            lower_type_with_lowering(&invalid, &mut low, &HashSet::new()),
            Type::Unknown(UnknownType::OracleGap)
        );
    }

    /// Universe names with no IR slot keep their spelling and stay distinct.
    ///
    /// Under `Type::Any` a `complex128` parameter and an `unsafe.Pointer`
    /// parameter produced the same skeleton byte.
    #[test]
    fn unrepresentable_basics_name_themselves_and_stay_distinct() {
        use nudox_ir::kinds::UnknownType;
        assert_eq!(
            lower_basic("complex128"),
            Type::Unknown(UnknownType::NoIrRepresentation {
                construct: "complex128".to_owned()
            })
        );
        assert_ne!(lower_basic("complex128"), lower_basic("unsafe.Pointer"));
        // Neither may masquerade as the top type.
        assert_ne!(lower_basic("complex128"), Type::Any);
    }

    /// `byte` is an alias for `uint8` by spec — not a gap, and certainly not a
    /// top type. `Type::Any` here claimed a `[]byte` accepted anything.
    #[test]
    fn byte_is_uint8_not_a_gap() {
        assert_eq!(lower_basic("byte"), Type::U8);
        assert_eq!(lower_basic("byte"), lower_basic("uint8"));
    }

    /// The recursion guard reports itself as *our* limit, distinctly from an
    /// oracle failure — raising `MAX_DEPTH` closes one and not the other.
    #[test]
    fn depth_limit_is_reported_as_truncation_not_as_a_gap() {
        use nudox_ir::kinds::UnknownType;
        let mut low = make_low();
        // Build a pointer chain deeper than MAX_DEPTH.
        let mut t = oracle::Type {
            kind: TypeKind::Basic,
            name: "int".to_string(),
            ..Default::default()
        };
        for _ in 0..(MAX_DEPTH + 2) {
            t = oracle::Type {
                kind: TypeKind::Pointer,
                elem: Some(Box::new(t)),
                ..Default::default()
            };
        }
        let rendered = lower_type_with_lowering(&t, &mut low, &HashSet::new()).to_string();
        assert!(
            rendered.contains("?depth-limit"),
            "the truncation point must name itself; got {rendered}"
        );
        assert!(
            !rendered.contains("any"),
            "a truncated chain must not claim a top type; got {rendered}"
        );
        assert_ne!(Type::TRUNCATED, Type::Unknown(UnknownType::OracleGap));
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
            matches!(
                lower_type_with_lowering(&empty, &mut low, &HashSet::new()),
                Type::Any
            ),
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
                pos: None,
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
        match lower_type_with_lowering(&non_empty, &mut low, &HashSet::new()) {
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
            other => panic!("non-empty interface must lower to AnonymousRecord, got {other:?}"),
        }
    }
}
