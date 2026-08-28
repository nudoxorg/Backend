//! Lower a [`ClangOracle`] into the nudox-ir [`Lowering`] sink.
//!
//! One pass; no intermediate tree; order-independent.
//!
//! # `Type::FunctionPointer`
//!
//! C function pointers (`void (*)(int, bool)`) previously degraded to
//! `Type::Any`. They now lower to `Type::FunctionPointer { params, ret, abi }`.
//! The ABI field is `None` (C default); libclang does not surface
//! `__attribute__((stdcall))` etc. at this level.
//!
//! # `Type::Annotated` — not wired for `const`/`volatile`
//!
//! `const` and `volatile` qualifiers are structural parts of the C/C++ type
//! system, NOT source-level annotations. They are represented in the oracle
//! as distinct type wrappers (`OracleType::ConstPointer`, the `mutable` flag
//! on `LValueRef`/`Reference`, etc.) and are already lowered faithfully to
//! `Primitive::ConstPointer` / `Primitive::MutPointer` / `Reference { mutable }`.
//! Wrapping them again in `Type::Annotated` would duplicate the information
//! already in those structural variants and mislead consumers that treat
//! `Annotated` as a source-level attribute rather than a type qualifier.
//!
//! `__attribute__` spellings are similarly structural in practice (alignment,
//! calling convention, visibility) and are not surfaced as type-level data by
//! libclang at the oracle boundary; wiring them to `Type::Annotated` would
//! require a new oracle field that does not exist.
//!
//! Conclusion: `Type::Annotated` is intentionally NOT wired for C/C++ in this
//! producer. If a future oracle version exposes `__attribute__` or
//! `[[nodiscard]]` type annotations, a new `OracleType::Annotated { inner, attr }`
//! variant and a trivial match arm would be the right extension point.
//!
//! # `GenericParam::Type::variance`
//!
//! C++ template type parameters have no declaration-site variance keyword.
//! Variance is `None` for all `GenericParam::Type` entries from this producer.
//!
//! # `OracleType::Named` — not a `Type::TypeVar`
//!
//! An unresolved named type (`OracleType::Named`) previously lowered to
//! `Type::TypeVar(name)`, the same variant used for a genuine template
//! type-parameter use (`OracleType::TypeVar`). That conflated "this is a
//! generic parameter" with "this is an ordinary named type we have no
//! resolution path for" — the two are semantically opposite (a `TypeVar` is
//! alpha-equivalent and excluded from identity skeletons by name; a named
//! type's identity is exactly its name).
//!
//! **Identity note:** this moved `IntroId`s for overload sets whose
//! signature mentions an unresolved named type (an entry's skeleton is
//! consulted only when it collides with another on `(kind, path, name)`, or
//! for `Kind::Impl`, which this producer never emits). It rode the single
//! `INTRO_DOMAIN` v4 → v5 bump shared with the rest of the type-lattice change
//! and with Python's `TypeData::Unsupported` change — one bump for all three,
//! since each alone would have invalidated the whole corpus.
//! `skeleton::tests::unknown_reason_opcodes_are_frozen` pins the `UnknownType`
//! variant→opcode mapping and needed no change for a new *caller* of an
//! existing variant.
//!
//! # Reference resolution — same-package, standard-library, and unresolved
//!
//! `OracleType::Named` used to keep only the display name discarded of its
//! declaration's identity, and `lower_type` took just `&OracleType` with no
//! `Lowering` sink in scope — so it blanket-mapped *every* zero-arg `Named`
//! to `Type::unresolved_external`, even a same-package `struct` field or an
//! inheritance base one line above. `resolve_type` (`extract.rs`) now also
//! captures `entity_usr(ty.get_declaration())` on the `Named` it builds, and
//! `lower_type` takes `out: &mut Lowering<Usr>` plus `known: &HashSet<Usr>`
//! (the package's own declared record/enum/alias USRs, built once by
//! [`known_nominal_usrs`]) so [`lower_named`] can tell apart:
//!
//! - a same-package reference → `out.nominal::<Record>(usr)` → `Ref::Intro`
//!   after `seal` (mirrors `python::types::lower_nominal` case 2);
//! - a standard-library reference (`is_std_usr`) → a named
//!   `out.nominal_import(ForeignKey::in_namespace(cpp, "std", …))` →
//!   `Ref::Foreign`, never a bare unlinkable string (mirrors
//!   `python::types::lower_nominal` case 4's `ForeignKey` fix);
//! - a resolved-but-neither name → the honest `Type::unresolved_external`,
//!   unchanged from before;
//! - no declaration at all (`usr: None`) → `Type::unresolved_local`, a
//!   distinct gap from "genuinely external" because there was nothing to look
//!   up, local or foreign.
//!
//! See [`lower_named`]'s doc for the full four-way decision and
//! `tests/clang/refs_resolution.rs` for one fixture per case.

use std::collections::HashSet;
use std::path::PathBuf;

use nudox_ir::{
    change::EcosystemId,
    entry::{SourceLocation, Symbol, Unlocated, Visibility},
    foreign::ForeignKey,
    kinds::{
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, GenericParam,
        Module, Param, ParamAttribute, Receiver, Record, RecordForm, Static, Type, Variant,
        VariantForm,
    },
    lower::Lowering,
    vocab::{Confidence, ReferenceKind, RelSpan},
};

use crate::clang::oracle::{
    ClangOracle, OracleAlias, OracleEnum, OracleField, OracleFnMod, OracleFunction,
    OracleGenericParam, OracleNamespace, OracleReceiver, OracleRecord, OracleType, OracleVar,
    OracleVariant, OracleVisibility, Usr,
};

// ── Public entry point ────────────────────────────────────────────────────────

/// The USRs this package declares under a *nominal* kind (record, enum,
/// alias) — exactly the set a `resolve_type`-captured USR (`extract.rs`) can
/// equal for a same-package reference.
///
/// Built once per lowering pass and threaded read-only through every
/// `lower_type` call, mirroring `python::types::KnownIds` (same problem: tell
/// "this package declares it" apart from "it does not"), except here the
/// lookup is an exact USR match rather than a dotted-suffix index — libclang
/// already hands back a single canonical id per declaration, so there is no
/// short-name ambiguity to resolve.
fn known_nominal_usrs(oracle: &ClangOracle) -> HashSet<Usr> {
    oracle
        .records
        .iter()
        .map(|r| r.usr.clone())
        .chain(oracle.enums.iter().map(|e| e.usr.clone()))
        .chain(oracle.aliases.iter().map(|a| a.usr.clone()))
        .collect()
}

/// Emit all declarations from `oracle` into `out`.
///
/// Called from [`ClangProducer::lower`].
pub fn lower_oracle(oracle: &ClangOracle, out: &mut Lowering<Usr>) {
    let known = known_nominal_usrs(oracle);

    for ns in &oracle.namespaces {
        lower_namespace(ns, out);
    }
    for rec in &oracle.records {
        lower_record(rec, oracle, out, &known);
    }
    for fun in &oracle.functions {
        lower_function(fun, oracle, out, &known);
    }
    for e in &oracle.enums {
        lower_enum(e, oracle, out);
    }
    for alias in &oracle.aliases {
        lower_alias(alias, out, &known);
    }
    for var in &oracle.vars {
        lower_var(var, out, &known);
    }
    for reference in &oracle.references {
        let Some(owner) = oracle.functions.iter().find(|f| f.usr == reference.owner) else {
            continue;
        };
        let Some(target) = oracle.functions.iter().find(|f| f.usr == reference.target) else {
            continue;
        };
        let reference_start = reference.byte_start;
        let owner_start = owner.byte_offset;
        if owner.source_file != reference.source_file || reference_start < owner_start {
            continue;
        }
        let Ok(start) = u32::try_from(reference_start - owner_start) else {
            continue;
        };
        let Ok(end) = u32::try_from(reference.byte_end.saturating_sub(owner_start)) else {
            continue;
        };
        out.record_occurrence(
            owner.usr.clone(),
            target.usr.clone(),
            ReferenceKind::FunctionCall,
            Confidence::Oracle,
            RelSpan::new(start, end),
        );
    }
}

// ── Symbol construction ───────────────────────────────────────────────────────

fn make_sym(
    name: &str,
    source: PathBuf,
    offset: usize,
    doc: &str,
    vis: OracleVisibility,
) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: oracle_vis(vis),
        documentation: doc.to_owned(),
        source,
        // libclang gives this frontend the declaration's start offset but not
        // an extent. The identifier itself is still a genuine, non-empty
        // source range and is preferable to the historical 0..0 sentinel.
        span: offset..offset.saturating_add(name.len()),
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn oracle_vis(v: OracleVisibility) -> Visibility {
    match v {
        OracleVisibility::Public => Visibility::Public,
        OracleVisibility::Protected => Visibility::Protected,
        OracleVisibility::Private => Visibility::Private,
        OracleVisibility::Internal => Visibility::Internal,
    }
}

fn parent_id(parent_usr: &Option<Usr>) -> Option<Usr> {
    parent_usr.clone()
}

// ── Namespace → Module ────────────────────────────────────────────────────────

fn lower_namespace(ns: &OracleNamespace, out: &mut Lowering<Usr>) {
    let sym = make_sym(
        &ns.name,
        ns.source_file.clone(),
        ns.byte_offset,
        &ns.documentation,
        ns.visibility,
    );
    out.declare(ns.usr.clone(), parent_id(&ns.parent_usr), sym, Module);
}

// ── Record (struct / class / union) ──────────────────────────────────────────

fn lower_record(rec: &OracleRecord, oracle: &ClangOracle, out: &mut Lowering<Usr>, known: &HashSet<Usr>) {
    // Collect field Refs first.  Fields are in `oracle.fields` keyed by parent USR.
    let fields: Vec<_> = oracle
        .fields
        .iter()
        .filter(|f| f.parent_usr.as_deref() == Some(&rec.usr))
        .map(|f| out.refer::<Field>(f.usr.clone()))
        .collect();

    // Generics.
    let generics: Vec<GenericParam> = rec.generics.iter().map(lower_generic_param).collect();

    // Super-types.
    let super_types: Vec<Type> = rec
        .super_types
        .iter()
        .map(|ty| lower_type(ty, out, known))
        .collect();

    let form = if rec.is_union {
        RecordForm::Union
    } else {
        RecordForm::Struct
    };

    let sym = make_sym(
        &rec.name,
        rec.source_file.clone(),
        rec.byte_offset,
        &rec.documentation,
        rec.visibility,
    );
    let kind = Record::builder()
        .form(form)
        .fields(fields)
        .super_types(super_types)
        .generics(generics)
        .build();

    out.declare(rec.usr.clone(), parent_id(&rec.parent_usr), sym, kind);

    // Now lower fields with their correct parent.
    for field in oracle
        .fields
        .iter()
        .filter(|f| f.parent_usr.as_deref() == Some(&rec.usr))
    {
        lower_field(field, out, known);
    }
}

// ── Field ─────────────────────────────────────────────────────────────────────

fn lower_field(f: &OracleField, out: &mut Lowering<Usr>, known: &HashSet<Usr>) {
    let sym = make_sym(
        &f.name,
        f.source_file.clone(),
        f.byte_offset,
        &f.documentation,
        f.visibility,
    );

    let mut attrs = Vec::new();
    if f.is_mutable {
        attrs.push(FieldAttribute::Mutable);
    }
    if f.is_static {
        attrs.push(FieldAttribute::Static);
    }

    let kind = Field::builder()
        .key(FieldKey::Named)
        .ty(lower_type(&f.ty, out, known))
        .attributes(attrs)
        .build();

    out.declare(f.usr.clone(), f.parent_usr.clone(), sym, kind);
}

// ── Function ─────────────────────────────────────────────────────────────────

fn lower_function(
    fun: &OracleFunction,
    _oracle: &ClangOracle,
    out: &mut Lowering<Usr>,
    known: &HashSet<Usr>,
) {
    // Build Param entries inline using a per-param synthetic USR.
    let input_refs: Vec<_> = fun
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let param_usr = format!("{}::param::{i}", fun.usr);
            let pref = out.refer::<Param>(param_usr.clone());

            let sym = Symbol {
                name: if p.name.is_empty() {
                    format!("arg{i}")
                } else {
                    p.name.clone()
                },
                visibility: Visibility::Public,
                documentation: String::new(),
                source: p.source_file.clone(),
                span: p.byte_offset..p.byte_offset.saturating_add(p.name.len()),
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            };

            let mut param_attrs: Vec<ParamAttribute> = Vec::new();
            if p.is_variadic {
                param_attrs.push(ParamAttribute::Variadic);
            }
            // C++ reference params → Inout / Borrowing.
            match &p.ty {
                OracleType::LValueRef { mutable: true, .. } => {
                    param_attrs.push(ParamAttribute::Inout);
                }
                OracleType::LValueRef { mutable: false, .. } => {
                    param_attrs.push(ParamAttribute::Borrowing);
                }
                _ => {}
            }

            let kind = Param::builder()
                .ty(lower_type(&p.ty, out, known))
                .attributes(param_attrs)
                .build();

            out.declare(param_usr, Some(fun.usr.clone()), sym, kind);
            pref
        })
        .collect();

    // Variadic trailing param.
    let variadic_ref = if fun.variadic {
        let vusr = format!("{}::param::variadic", fun.usr);
        let vref = out.refer::<Param>(vusr.clone());
        let vsym = Symbol {
            name: "...".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: fun.source_file.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let kind = Param::builder()
            .attributes([ParamAttribute::Variadic])
            .build();
        out.declare_at(
            vusr,
            Some(fun.usr.clone()),
            vsym,
            kind,
            SourceLocation::Unlocated(Unlocated::Synthesized),
        );
        Some(vref)
    } else {
        None
    };

    // Output param (return type) unless void.
    let mut output_refs: Vec<_> = Vec::new();
    if !matches!(fun.ret, OracleType::Void) {
        let ret_usr = format!("{}::ret", fun.usr);
        let rref = out.refer::<Param>(ret_usr.clone());
        let rsym = Symbol {
            name: String::new(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: fun.source_file.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let kind = Param::builder().ty(lower_type(&fun.ret, out, known)).build();
        out.declare_at(
            ret_usr,
            Some(fun.usr.clone()),
            rsym,
            kind,
            SourceLocation::Unlocated(Unlocated::Synthesized),
        );
        output_refs.push(rref);
    }

    let receiver = fun.receiver.map(lower_receiver);
    let generics: Vec<GenericParam> = fun.generics.iter().map(lower_generic_param).collect();

    let mut modifiers: Vec<FnModifier> = Vec::new();
    for m in &fun.modifiers {
        match m {
            OracleFnMod::Const => modifiers.push(FnModifier::Const),
            OracleFnMod::Pure => modifiers.push(FnModifier::Pure),
            OracleFnMod::Virtual | OracleFnMod::NoReturn => {}
        }
    }

    let mut all_input = input_refs;
    if let Some(v) = variadic_ref {
        all_input.push(v);
    }

    let kind = Function::builder()
        .maybe_receiver(receiver)
        .input_params(all_input)
        .output_params(output_refs)
        .modifiers(modifiers)
        .generics(generics)
        .maybe_abi(fun.abi.clone())
        .build();

    let sym = make_sym(
        &fun.name,
        fun.source_file.clone(),
        fun.byte_offset,
        &fun.documentation,
        fun.visibility,
    );

    out.declare(fun.usr.clone(), parent_id(&fun.parent_usr), sym, kind);
}

fn lower_receiver(r: OracleReceiver) -> Receiver {
    match r {
        OracleReceiver::Owned => Receiver::Owned,
        OracleReceiver::SharedRef => Receiver::SharedRef,
        OracleReceiver::MutRef => Receiver::MutRef,
        OracleReceiver::Static => {
            // Static methods have no receiver; handled by leaving `receiver: None`.
            // This branch is unreachable in practice because we pass
            // `maybe_receiver(Some(Static))` only for constructors.
            // Map to Owned as a safe fallback.
            Receiver::Owned
        }
    }
}

// ── Enum ──────────────────────────────────────────────────────────────────────

fn lower_enum(e: &OracleEnum, oracle: &ClangOracle, out: &mut Lowering<Usr>) {
    let variant_refs: Vec<_> = oracle
        .variants
        .iter()
        .filter(|v| v.parent_usr.as_deref() == Some(&e.usr))
        .map(|v| out.refer::<Variant>(v.usr.clone()))
        .collect();

    let sym = make_sym(
        &e.name,
        e.source_file.clone(),
        e.byte_offset,
        &e.documentation,
        e.visibility,
    );

    let kind = Enum::builder().variants(variant_refs).build();
    out.declare(e.usr.clone(), parent_id(&e.parent_usr), sym, kind);

    // Lower variants with correct parent.
    for v in oracle
        .variants
        .iter()
        .filter(|v| v.parent_usr.as_deref() == Some(&e.usr))
    {
        lower_variant(v, out);
    }
}

fn lower_variant(v: &OracleVariant, out: &mut Lowering<Usr>) {
    let sym = make_sym(
        &v.name,
        v.source_file.clone(),
        v.byte_offset,
        &v.documentation,
        OracleVisibility::Public,
    );
    let discr = v.discr.map(|d| d.to_string());
    let kind = Variant::builder()
        .form(VariantForm::Unit)
        .maybe_discr(discr)
        .build();
    out.declare(v.usr.clone(), v.parent_usr.clone(), sym, kind);
}

// ── Alias ─────────────────────────────────────────────────────────────────────

fn lower_alias(a: &OracleAlias, out: &mut Lowering<Usr>, known: &HashSet<Usr>) {
    let sym = make_sym(
        &a.name,
        a.source_file.clone(),
        a.byte_offset,
        &a.documentation,
        a.visibility,
    );
    let kind = Alias::builder()
        .target(lower_type(&a.target, out, known))
        .build();
    out.declare(a.usr.clone(), parent_id(&a.parent_usr), sym, kind);
}

// ── Variable (const / static) ─────────────────────────────────────────────────

fn lower_var(v: &OracleVar, out: &mut Lowering<Usr>, known: &HashSet<Usr>) {
    let sym = make_sym(
        &v.name,
        v.source_file.clone(),
        v.byte_offset,
        &v.documentation,
        v.visibility,
    );
    let ty = lower_type(&v.ty, out, known);

    if v.is_const {
        let kind = Const::builder().ty(ty).build();
        out.declare(v.usr.clone(), parent_id(&v.parent_usr), sym, kind);
    } else {
        let kind = Static::builder().ty(ty).mutable(true).build();
        out.declare(v.usr.clone(), parent_id(&v.parent_usr), sym, kind);
    }
}

// ── Type lowering ─────────────────────────────────────────────────────────────

/// Resolve a nominal type reference captured by `resolve_type`
/// (`extract.rs`'s `OracleType::Named`) into the right IR `Type`.
///
/// Priority, mirroring `python::types::lower_nominal`'s same-package-vs-
/// foreign split (see that function's doc for the shape of the problem this
/// answers):
///
/// 1. **`usr` is `Some` and this package declares it** (`known` — built once
///    per pass by [`known_nominal_usrs`]) → same-package `Nominal(Ref::Intro)`
///    via `out.nominal`. Order-independent: the declaration need not have
///    been lowered yet (`Outer` naming a `struct Inner` declared later in the
///    same TU works exactly like every other producer's forward reference).
/// 2. **`usr` is `Some` and it names a standard-library declaration**
///    (`is_std_usr`) → a *named* `Nominal(Ref::Foreign)` carrying a `cpp`
///    `ForeignKey` in the `"std"` namespace. This used to collapse into the
///    same `UnresolvedExternal` bucket as every other external name, with no
///    `ForeignKey` behind it anywhere — exactly Python's case-4 defect
///    (`python::types`'s module doc), applied to C++. `target` starts `None`;
///    a resolver fills it in.
/// 3. **`usr` is `Some` but neither of the above** → genuinely outside this
///    package and not a standard-library name this producer recognizes:
///    the honest `Type::unresolved_external`, keeping the spelling.
/// 4. **`usr` is `None`** — libclang could not resolve *any* declaration for
///    this name at all (a dependent type, some exotic type-kind sugar).
///    There is nothing to look up, local or foreign, so this is reported as a
///    local resolution gap (`Type::unresolved_local`) rather than silently
///    reusing the "genuinely external" bucket: a later within-TU pass is
///    what closes it, not a registry link.
fn lower_named(name: &str, usr: Option<&Usr>, out: &mut Lowering<Usr>, known: &HashSet<Usr>) -> Type {
    let Some(usr) = usr else {
        return Type::unresolved_local(name.to_owned());
    };
    if known.contains(usr) {
        return out.nominal::<Record>(usr.clone());
    }
    if is_std_usr(usr) {
        let key = ForeignKey::in_namespace(EcosystemId::new("cpp"), "std", usr.clone(), name.to_owned());
        return out.nominal_import(key);
    }
    Type::unresolved_external(name.to_owned())
}

/// Whether `usr` names a declaration inside namespace `std` (or an inline
/// namespace nested directly under it, e.g. libc++'s `std::__1`).
///
/// libclang's USR grammar encodes each namespace segment as `@N@<name>`, so a
/// declaration written `std::string` carries `@N@std@` as a literal substring
/// of its USR regardless of which inline-namespace ABI-tagging scheme the
/// standard library implementation uses underneath — verified empirically
/// against libc++: `std::string`'s USR is `c:@N@std@N@__1@string`.
///
/// This is a heuristic, not a lookup against a real system-header allowlist:
/// a user namespace that happens to be (re-)opened under the name `std` would
/// also match. That is an acceptable false positive for a producer with no
/// header-provenance information at this layer — the cost is a `Ref::Foreign`
/// mislabeled `"std"` rather than a wrong `Ref::Intro`, the same bounded-cost
/// shape `python::types::KnownIds`'s doc argues for its own suffix-index false
/// positive.
fn is_std_usr(usr: &str) -> bool {
    usr.contains("@N@std@")
}

fn lower_type(ty: &OracleType, out: &mut Lowering<Usr>, known: &HashSet<Usr>) -> Type {
    use nudox_ir::kinds::ty::{Primitive, Width};

    match ty {
        OracleType::Void => Type::Tuple(Box::new([])),
        OracleType::Bool => Type::Primitive(Primitive::Bool),
        OracleType::Integer { signed, bits } => Type::Primitive(Primitive::Integer {
            signed: *signed,
            width: fixed_width(*bits),
        }),
        OracleType::IntegerArch { signed } => Type::Primitive(Primitive::Integer {
            signed: *signed,
            width: Width::Arch,
        }),
        OracleType::Float { bits } => Type::Primitive(Primitive::Float(fixed_width(*bits))),
        OracleType::FloatArch => Type::Primitive(Primitive::Float(Width::Arch)),

        OracleType::ConstPointer(inner) => Type::Primitive(Primitive::ConstPointer(Box::new(
            lower_type(inner, out, known),
        ))),
        OracleType::MutPointer(inner) => Type::Primitive(Primitive::MutPointer(Box::new(
            lower_type(inner, out, known),
        ))),
        OracleType::LValueRef { mutable, ty } => Type::Primitive(Primitive::Reference {
            lifetime: None,
            mutable: *mutable,
            ty: Box::new(lower_type(ty, out, known)),
        }),
        OracleType::RValueRef(inner) => {
            // No dedicated RValueRef in the IR; model as mutable reference.
            Type::Primitive(Primitive::Reference {
                lifetime: None,
                mutable: true,
                ty: Box::new(lower_type(inner, out, known)),
            })
        }
        OracleType::Array { ty, len } => Type::Array {
            ty: Box::new(lower_type(ty, out, known)),
            length: *len,
        },
        OracleType::Slice(inner) => Type::Slice(Box::new(lower_type(inner, out, known))),
        OracleType::FnPtr { ret, params } => {
            // C function pointers lower to Type::FunctionPointer.
            // `params` are the positional parameter types in order.
            // `ret` is the return type; if void, lower to None (no return).
            // ABI is None (C default / language default — no __attribute__ ABI
            // annotation is surfaced at this level by libclang).
            let lowered_params: Vec<Type> =
                params.iter().map(|p| lower_type(p, out, known)).collect();
            let lowered_ret = if matches!(ret.as_ref(), OracleType::Void) {
                None
            } else {
                Some(Box::new(lower_type(ret, out, known)))
            };
            Type::FunctionPointer {
                params: lowered_params.into_boxed_slice(),
                ret: lowered_ret,
                abi: None,
            }
        }
        OracleType::Named { name, usr, args } if args.is_empty() => {
            // A nominal type reference (a `struct`/`class`/`enum`/alias name),
            // NOT a template type-parameter use — those are the distinct
            // `OracleType::TypeVar` case below, which the oracle already tells
            // apart from this one. `Type::TypeVar(name)` here was therefore a
            // lie: it told every downstream consumer "this is a generic
            // parameter" about ordinary named types, most of which are not.
            //
            // `usr` (captured by `resolve_type` from `ty.get_declaration()`)
            // is what lets `lower_named` tell same-package, standard-library,
            // and genuinely-unresolved names apart — see its doc for the
            // four-way split.
            lower_named(name, usr.as_ref(), out, known)
        }
        OracleType::Named { name, usr, args } => {
            // `Foo<Arg…>` — resolve the base exactly as the zero-arg case
            // above, then wrap in `Apply` with the lowered args. This mirrors
            // `python::types::lower_type`'s `TypeData::Apply` handling:
            // `lower_named` already returns a complete, correctly-formed
            // `Type` (Intro, Foreign, or an honest Unknown), so there is no
            // second same-package check to perform here — just wrap it.
            let base = Box::new(lower_named(name, usr.as_ref(), out, known));
            let lowered_args: Vec<Type> = args.iter().map(|a| lower_type(a, out, known)).collect();
            Type::Apply {
                base,
                args: lowered_args.into(),
            }
        }
        OracleType::TypeVar(name) => Type::TypeVar(name.clone()),
        // `auto` / `__auto_type` / `decltype(auto)` — the source *asked* for
        // inference and libclang did not resolve it at oracle time.
        // `Type::Inferred` is right precisely because a written inference
        // request is neither a top type (`Type::Any` — C has none) nor an
        // unrequested gap (`Type::Unknown`); see `Type::Inferred`'s doc for
        // the three-way boundary.
        OracleType::Inferred => Type::Inferred,
    }
}

fn fixed_width(bits: u16) -> nudox_ir::kinds::ty::Width {
    use nudox_ir::kinds::ty::Width;
    use std::num::NonZero;
    Width::Fixed(NonZero::new(bits).unwrap_or(NonZero::new(8).unwrap()))
}

// ── Generic param lowering ────────────────────────────────────────────────────

fn lower_generic_param(p: &OracleGenericParam) -> GenericParam {
    match p {
        OracleGenericParam::Type { name } => GenericParam::Type {
            name: name.clone(),
            bounds: Box::new([]),
            default: None,
            // C++ template type parameters have no declaration-site variance
            // annotation in the language — there is no `in`/`out` keyword at
            // the template parameter list level (unlike C# generic interfaces).
            // libclang does not report covariance/contravariance. Use None.
            variance: None,
        },
        OracleGenericParam::Const { name, ty } => GenericParam::Const {
            name: name.clone(),
            ty: Type::TypeVar(ty.clone()),
        },
        OracleGenericParam::Template { name } => {
            // Template-template param — best approximation in the IR is a type param.
            GenericParam::Type {
                name: name.clone(),
                bounds: Box::new([]),
                default: None,
                // Same variance note as OracleGenericParam::Type above.
                variance: None,
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty sink + empty known-USR set for tests that only care
    /// about `lower_type`'s pure branches (no same-package/std resolution).
    fn test_sink() -> Lowering<Usr> {
        let pkg_id = nudox_ir::package::PackageId::path("/tmp/test");
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

    /// `OracleType::Inferred` must lower to `Type::Inferred`, not `Type::Any`.
    ///
    /// Clang's `auto`, `__auto_type`, and `decltype(auto)` all produce
    /// `OracleType::Inferred` in the oracle. These are producer resolution gaps,
    /// not genuine top types — `Type::Any` would be semantically wrong.
    #[test]
    fn inferred_oracle_type_lowers_to_inferred_not_any() {
        let mut sink = test_sink();
        let ty = lower_type(&OracleType::Inferred, &mut sink, &HashSet::new());
        assert!(
            matches!(ty, Type::Inferred),
            "OracleType::Inferred must lower to Type::Inferred; got {ty:?}"
        );
        assert!(
            !matches!(ty, Type::Any),
            "OracleType::Inferred must NOT lower to Type::Any"
        );
    }

    /// A named type with a USR that resolves to neither this package nor
    /// `std` must lower to `Unknown(UnresolvedExternal)`, carrying its
    /// spelling — never to `TypeVar`, which is reserved for a genuine
    /// template type-parameter use (`OracleType::TypeVar`).
    #[test]
    fn unresolved_named_type_lowers_to_unresolved_external_not_typevar() {
        use nudox_ir::kinds::UnknownType;

        let mut sink = test_sink();
        let ty = lower_type(
            &OracleType::Named {
                name: "SomeStruct".to_owned(),
                usr: Some("c:@N@some_other_lib@S@SomeStruct".to_owned()),
                args: Vec::new(),
            },
            &mut sink,
            &HashSet::new(),
        );
        assert_eq!(
            ty,
            Type::Unknown(UnknownType::UnresolvedExternal {
                name: "SomeStruct".to_owned()
            }),
            "a resolved-but-foreign-and-non-std named type must be a named gap, not a type variable"
        );
        assert!(
            !matches!(ty, Type::TypeVar(_)),
            "a named type must never be reported as a generic parameter"
        );
    }

    /// A named type with NO USR at all (libclang could not resolve any
    /// declaration) is a different gap from the case above: there is nothing
    /// to look up, local or foreign, so it lowers to `UnresolvedLocalName`
    /// rather than reusing the "genuinely external" bucket.
    #[test]
    fn named_type_with_no_usr_lowers_to_unresolved_local_not_external() {
        use nudox_ir::kinds::UnknownType;

        let mut sink = test_sink();
        let ty = lower_type(
            &OracleType::Named {
                name: "Dependent".to_owned(),
                usr: None,
                args: Vec::new(),
            },
            &mut sink,
            &HashSet::new(),
        );
        assert_eq!(
            ty,
            Type::Unknown(UnknownType::UnresolvedLocalName {
                name: "Dependent".to_owned()
            }),
            "a named type with no declaration at all must be UnresolvedLocalName, got {ty:?}"
        );
    }

    /// A genuine template type-parameter use is unaffected: it still lowers
    /// to `Type::TypeVar`, distinct from an unresolved named type above.
    #[test]
    fn template_type_parameter_use_still_lowers_to_typevar() {
        let mut sink = test_sink();
        let ty = lower_type(&OracleType::TypeVar("T".to_owned()), &mut sink, &HashSet::new());
        assert!(
            matches!(ty, Type::TypeVar(ref n) if n == "T"),
            "a real template type-parameter use must stay Type::TypeVar; got {ty:?}"
        );
    }

    /// A named type with template arguments (`Foo<int>`) keeps the `Apply`
    /// wrapper, with the unresolved base carrying the name.
    #[test]
    fn unresolved_named_type_with_args_keeps_apply_wrapper() {
        use nudox_ir::kinds::UnknownType;

        let mut sink = test_sink();
        let ty = lower_type(
            &OracleType::Named {
                name: "Foo".to_owned(),
                usr: Some("c:@N@some_other_lib@S@Foo".to_owned()),
                args: vec![OracleType::Integer {
                    signed: true,
                    bits: 32,
                }],
            },
            &mut sink,
            &HashSet::new(),
        );
        match ty {
            Type::Apply { base, args } => {
                assert_eq!(
                    *base,
                    Type::Unknown(UnknownType::UnresolvedExternal {
                        name: "Foo".to_owned()
                    })
                );
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected Type::Apply, got {other:?}"),
        }
    }
}
