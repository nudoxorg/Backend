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
//! `Primitive::ConstPointer` / `Primitive::MutPointer` / `Reference { mutable
//! }`. Wrapping them again in `Type::Annotated` would duplicate the information
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
//! `[[nodiscard]]` type annotations, a new `OracleType::Annotated { inner, attr
//! }` variant and a trivial match arm would be the right extension point.
//!
//! # `GenericParam::Type::variance`
//!
//! C++ template type parameters have no declaration-site variance keyword.
//! Variance is `None` for all `GenericParam::Type` entries from this producer.
//!
//! # `OracleType::Named` — local nominal, `std::` import, or a spelling
//!
//! An unresolved named type (`OracleType::Named`) previously lowered to
//! `Type::TypeVar(name)`, the same variant used for a genuine template
//! type-parameter use (`OracleType::TypeVar`). That conflated "this is a
//! generic parameter" with "this is an ordinary named type we have no
//! resolution path for" — the two are semantically opposite (a `TypeVar` is
//! alpha-equivalent and excluded from identity skeletons by name; a named
//! type's identity is exactly its name).
//!
//! `lower_type` takes the [`Lowering`] sink. A declaration this package
//! already extracted becomes [`Lowering::nominal`] / [`Lowering::apply`] on
//! that USR — a local nominal, not a stringly `Type::Nominal`. A name the
//! oracle spelled as `std::…` becomes [`Lowering::nominal_import`] /
//! [`Lowering::apply_import`]. `refer` on a `std::` USR would make `finish`
//! return `Undeclared`, because this producer never declares the standard
//! library. Anything else stays
//! `Type::Unknown(UnknownType::UnresolvedExternal { name })`, keeping the
//! spelling.
//!
//! # `OracleType::RValueRef` is not `T&`
//!
//! [`Primitive::Reference`](nudox_ir::kinds::ty::Primitive::Reference) is an
//! lvalue reference (`T&`, Rust `&T`). `T&&` used to lower to that same
//! primitive with `mutable: true`, so `f(T&)` and `f(T&&)` minted one
//! skeleton. There is still no rvalue-reference primitive. `T&&` lowers to
//! `Type::Apply` whose base is `Type::no_ir_representation("rvalue-reference")`
//! and whose argument is the pointee. That is not `Primitive::Reference`, and
//! it keeps the pointee so two rvalue overloads that differ in `T` stay
//! distinct.
//!
//! **Identity note:** this moves `IntroId`s for overload sets whose
//! signature mentions an unresolved named type (an entry's skeleton is
//! consulted only when it collides with another on `(kind, path, name)`, or
//! for `Kind::Impl`, which this producer never emits). It rode the single
//! `INTRO_DOMAIN` v4 → v5 bump shared with the rest of the type-lattice change
//! and with Python's `TypeData::Unsupported` change — one bump for all three,
//! since each alone would have invalidated the whole corpus.
//! `skeleton::tests::unknown_reason_opcodes_are_frozen` pins the `UnknownType`
//! variant→opcode mapping and needed no change for a new *caller* of an
//! existing variant.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

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

// ── Public entry point
// ────────────────────────────────────────────────────────

/// Emit all declarations from `oracle` into `out`.
///
/// Called from [`ClangProducer::lower`].
pub fn lower_oracle(oracle: &ClangOracle, out: &mut Lowering<Usr>) {
    let locals = LocalNames::from_oracle(oracle);
    for ns in &oracle.namespaces {
        lower_namespace(ns, out);
    }
    for rec in &oracle.records {
        lower_record(rec, oracle, out, &locals);
    }
    for fun in &oracle.functions {
        lower_function(fun, out, &locals);
    }
    for e in &oracle.enums {
        lower_enum(e, oracle, out);
    }
    for alias in &oracle.aliases {
        lower_alias(alias, out, &locals);
    }
    for var in &oracle.vars {
        lower_var(var, out, &locals);
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

// ── Symbol construction
// ───────────────────────────────────────────────────────

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

// ── Namespace → Module
// ────────────────────────────────────────────────────────

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

fn lower_record(
    rec: &OracleRecord,
    oracle: &ClangOracle,
    out: &mut Lowering<Usr>,
    locals: &LocalNames,
) {
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
        .map(|ty| lower_type(ty, out, locals))
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
        lower_field(field, out, locals);
    }
}

// ── Field ─────────────────────────────────────────────────────────────────────

fn lower_field(f: &OracleField, out: &mut Lowering<Usr>, locals: &LocalNames) {
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
        .ty(lower_type(&f.ty, out, locals))
        .attributes(attrs)
        .build();

    out.declare(f.usr.clone(), f.parent_usr.clone(), sym, kind);
}

// ── Function ─────────────────────────────────────────────────────────────────

fn lower_function(fun: &OracleFunction, out: &mut Lowering<Usr>, locals: &LocalNames) {
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
                .ty(lower_type(&p.ty, out, locals))
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
        let kind = Param::builder()
            .ty(lower_type(&fun.ret, out, locals))
            .build();
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

fn lower_alias(a: &OracleAlias, out: &mut Lowering<Usr>, locals: &LocalNames) {
    let sym = make_sym(
        &a.name,
        a.source_file.clone(),
        a.byte_offset,
        &a.documentation,
        a.visibility,
    );
    let kind = Alias::builder()
        .target(lower_type(&a.target, out, locals))
        .build();
    out.declare(a.usr.clone(), parent_id(&a.parent_usr), sym, kind);
}

// ── Variable (const / static)
// ─────────────────────────────────────────────────

fn lower_var(v: &OracleVar, out: &mut Lowering<Usr>, locals: &LocalNames) {
    let sym = make_sym(
        &v.name,
        v.source_file.clone(),
        v.byte_offset,
        &v.documentation,
        v.visibility,
    );
    let ty = lower_type(&v.ty, out, locals);

    if v.is_const {
        let kind = Const::builder().ty(ty).build();
        out.declare(v.usr.clone(), parent_id(&v.parent_usr), sym, kind);
    } else {
        let kind = Static::builder().ty(ty).mutable(true).build();
        out.declare(v.usr.clone(), parent_id(&v.parent_usr), sym, kind);
    }
}

// ── Same-package nominals ────────────────────────────────────────────────────

/// USRs this oracle will declare, plus unambiguous spellings of those USRs.
///
/// `nominal` / `apply` call `refer`, and `finish` demands a declaration for
/// every referred id. Only USRs in `by_usr` are safe. A `std::` name is never
/// looked up here.
struct LocalNames {
    by_usr: HashSet<String>,
    by_name: HashMap<String, String>,
}

impl LocalNames {
    fn from_oracle(oracle: &ClangOracle) -> Self {
        let mut by_usr = HashSet::new();
        let mut provisional: HashMap<String, Option<String>> = HashMap::new();
        let mut parents: HashMap<&str, (&str, Option<&str>)> = HashMap::new();
        for ns in &oracle.namespaces {
            parents.insert(
                ns.usr.as_str(),
                (ns.name.as_str(), ns.parent_usr.as_deref()),
            );
        }
        for rec in &oracle.records {
            parents.insert(
                rec.usr.as_str(),
                (rec.name.as_str(), rec.parent_usr.as_deref()),
            );
        }
        for en in &oracle.enums {
            parents.insert(
                en.usr.as_str(),
                (en.name.as_str(), en.parent_usr.as_deref()),
            );
        }

        let mut note = |name: &str, usr: &str| {
            if name.is_empty() {
                return;
            }
            match provisional.entry(name.to_owned()) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Some(usr.to_owned()));
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    if slot.get().as_deref() != Some(usr) {
                        slot.insert(None);
                    }
                }
            }
        };

        for rec in &oracle.records {
            by_usr.insert(rec.usr.clone());
            note(&rec.name, &rec.usr);
            let qualified = qualify_name(&parents, rec.parent_usr.as_deref(), &rec.name);
            if qualified != rec.name {
                note(&qualified, &rec.usr);
            }
        }
        for en in &oracle.enums {
            by_usr.insert(en.usr.clone());
            note(&en.name, &en.usr);
            let qualified = qualify_name(&parents, en.parent_usr.as_deref(), &en.name);
            if qualified != en.name {
                note(&qualified, &en.usr);
            }
        }
        for alias in &oracle.aliases {
            by_usr.insert(alias.usr.clone());
            note(&alias.name, &alias.usr);
            let qualified = qualify_name(&parents, alias.parent_usr.as_deref(), &alias.name);
            if qualified != alias.name {
                note(&qualified, &alias.usr);
            }
        }

        let by_name = provisional
            .into_iter()
            .filter_map(|(name, usr)| usr.map(|usr| (name, usr)))
            .collect();
        Self { by_usr, by_name }
    }

    /// The USR to `nominal`, if this package declares the type.
    ///
    /// A resolved declaration USR wins. A spelling is used only when it names
    /// exactly one local declaration. A declaration USR that is *not* local
    /// (a system header, the standard library) does not fall through to a
    /// same-spelled local — that would point `std::string` at a local `string`.
    fn local_usr(&self, name: &str, decl_usr: Option<&str>) -> Option<String> {
        if let Some(usr) = decl_usr {
            if self.by_usr.contains(usr) {
                return Some(usr.to_owned());
            }
            return None;
        }
        self.by_name.get(name).cloned()
    }
}

fn qualify_name(
    parents: &HashMap<&str, (&str, Option<&str>)>,
    parent: Option<&str>,
    name: &str,
) -> String {
    let mut parts = vec![name];
    let mut cur = parent;
    for _ in 0..32 {
        let Some(usr) = cur else { break };
        let Some((parent_name, next)) = parents.get(usr) else {
            break;
        };
        parts.push(*parent_name);
        cur = *next;
    }
    parts.reverse();
    parts.join("::")
}

fn is_std_spelling(name: &str) -> bool {
    let name = name.trim_start_matches("::");
    name == "std" || name.starts_with("std::")
}

fn std_foreign_key(name: &str) -> ForeignKey {
    let spelled = name.trim_start_matches("::");
    let leaf = spelled.rsplit("::").next().unwrap_or(spelled);
    let display = leaf.split('<').next().unwrap_or(leaf);
    ForeignKey::in_namespace(EcosystemId::new("cpp"), "std", spelled, display)
}

// ── Type lowering
// ─────────────────────────────────────────────────────────────

fn lower_type(ty: &OracleType, out: &mut Lowering<Usr>, locals: &LocalNames) -> Type {
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
            lower_type(inner, out, locals),
        ))),
        OracleType::MutPointer(inner) => Type::Primitive(Primitive::MutPointer(Box::new(
            lower_type(inner, out, locals),
        ))),
        OracleType::LValueRef { mutable, ty } => Type::Primitive(Primitive::Reference {
            lifetime: None,
            mutable: *mutable,
            ty: Box::new(lower_type(ty, out, locals)),
        }),
        OracleType::RValueRef(inner) => {
            // `Primitive::Reference` is `T&`. Wrapping the pointee in `Apply`
            // keeps `T&&` off that primitive and keeps the pointee in the
            // skeleton. `refer` is not involved.
            let pointee = lower_type(inner, out, locals);
            Type::Apply {
                base: Box::new(Type::no_ir_representation("rvalue-reference")),
                args: [pointee].into(),
            }
        }
        OracleType::Array { ty, len } => Type::Array {
            ty: Box::new(lower_type(ty, out, locals)),
            length: *len,
        },
        OracleType::Slice(inner) => Type::Slice(Box::new(lower_type(inner, out, locals))),
        OracleType::FnPtr { ret, params } => {
            // C function pointers lower to Type::FunctionPointer.
            // `params` are the positional parameter types in order.
            // `ret` is the return type; if void, lower to None (no return).
            // ABI is None (C default / language default — no __attribute__ ABI
            // annotation is surfaced at this level by libclang).
            let lowered_params: Vec<Type> = params
                .iter()
                .map(|ty| lower_type(ty, out, locals))
                .collect();
            let lowered_ret = if matches!(ret.as_ref(), OracleType::Void) {
                None
            } else {
                Some(Box::new(lower_type(ret, out, locals)))
            };
            Type::FunctionPointer {
                params: lowered_params.into_boxed_slice(),
                ret: lowered_ret,
                abi: None,
            }
        }
        OracleType::Named {
            name,
            args,
            decl_usr,
        } => lower_named(name, args, decl_usr.as_deref(), out, locals),
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

fn lower_named(
    name: &str,
    args: &[OracleType],
    decl_usr: Option<&str>,
    out: &mut Lowering<Usr>,
    locals: &LocalNames,
) -> Type {
    let lowered_args: Vec<Type> = args.iter().map(|ty| lower_type(ty, out, locals)).collect();
    if let Some(usr) = locals.local_usr(name, decl_usr) {
        // Same-package declaration. `apply` is `nominal` when `args` is empty.
        // The kind marker is erased by `into_raw`; the USR is the one
        // `lower_record` / `lower_enum` / `lower_alias` declares.
        return out.apply::<Record>(usr, lowered_args);
    }
    if is_std_spelling(name) {
        // Not `refer`: this package does not declare `std`.
        return out.apply_import(std_foreign_key(name), lowered_args);
    }
    if lowered_args.is_empty() {
        Type::unresolved_external(name.to_owned())
    } else {
        Type::Apply {
            base: Box::new(Type::unresolved_external(name.to_owned())),
            args: lowered_args.into(),
        }
    }
}

fn fixed_width(bits: u16) -> nudox_ir::kinds::ty::Width {
    use nudox_ir::kinds::ty::Width;
    use std::num::NonZero;
    Width::Fixed(NonZero::new(bits).unwrap_or(NonZero::new(8).unwrap()))
}

// ── Generic param lowering
// ────────────────────────────────────────────────────

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
    use nudox_ir::index::Ref;

    fn lower_alone(ty: &OracleType) -> (Type, Lowering<Usr>) {
        let pkg_id = nudox_ir::package::PackageId::path("/tmp");
        let root_sym = nudox_ir::entry::Symbol {
            name: "test".to_owned(),
            visibility: nudox_ir::entry::Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from("/tmp"),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut out = Lowering::new(pkg_id, root_sym);
        let lowered = lower_type(
            ty,
            &mut out,
            &LocalNames::from_oracle(&ClangOracle::default()),
        );
        (lowered, out)
    }

    /// `OracleType::Inferred` must lower to `Type::Inferred`, not `Type::Any`.
    ///
    /// Clang's `auto`, `__auto_type`, and `decltype(auto)` all produce
    /// `OracleType::Inferred` in the oracle. These are producer resolution
    /// gaps, not genuine top types — `Type::Any` would be semantically
    /// wrong.
    #[test]
    fn inferred_oracle_type_lowers_to_inferred_not_any() {
        let (ty, _) = lower_alone(&OracleType::Inferred);
        assert!(
            matches!(ty, Type::Inferred),
            "OracleType::Inferred must lower to Type::Inferred; got {ty:?}"
        );
        assert!(
            !matches!(ty, Type::Any),
            "OracleType::Inferred must NOT lower to Type::Any"
        );
    }

    /// An unresolved named type must lower to `Unknown(UnresolvedExternal)`,
    /// carrying its spelling — never to `TypeVar`, which is reserved for a
    /// genuine template type-parameter use (`OracleType::TypeVar`).
    #[test]
    fn unresolved_named_type_lowers_to_unresolved_external_not_typevar() {
        use nudox_ir::kinds::UnknownType;

        let (ty, out) = lower_alone(&OracleType::Named {
            name: "SomeStruct".to_owned(),
            args: Vec::new(),
            decl_usr: None,
        });
        out.finish()
            .expect("an unresolved name must not refer() an undeclared id");
        assert_eq!(
            ty,
            Type::Unknown(UnknownType::UnresolvedExternal {
                name: "SomeStruct".to_owned()
            }),
            "an unresolved named type must be a named gap, not a type variable"
        );
        assert!(
            !matches!(ty, Type::TypeVar(_)),
            "a named type must never be reported as a generic parameter"
        );
    }

    /// A genuine template type-parameter use is unaffected: it still lowers
    /// to `Type::TypeVar`, distinct from an unresolved named type above.
    #[test]
    fn template_type_parameter_use_still_lowers_to_typevar() {
        let (ty, _) = lower_alone(&OracleType::TypeVar("T".to_owned()));
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

        let (ty, _) = lower_alone(&OracleType::Named {
            name: "Foo".to_owned(),
            args: vec![OracleType::Integer {
                signed: true,
                bits: 32,
            }],
            decl_usr: None,
        });
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

    /// A USR this package declares lowers through the sink to a local nominal.
    /// `finish` succeeds only because that USR is declared in the same pass.
    #[test]
    fn named_type_declared_in_this_package_is_a_local_nominal() {
        let mut oracle = ClangOracle::default();
        oracle.records.push(OracleRecord {
            usr: "USR-Session".to_owned(),
            name: "Session".to_owned(),
            source_file: std::path::PathBuf::from("session.hpp"),
            byte_offset: 0,
            is_class: false,
            is_union: false,
            generics: Vec::new(),
            super_types: Vec::new(),
            fields: Vec::new(),
            documentation: String::new(),
            visibility: OracleVisibility::Public,
            parent_usr: None,
        });
        let locals = LocalNames::from_oracle(&oracle);
        let pkg_id = nudox_ir::package::PackageId::path("/tmp");
        let root_sym = nudox_ir::entry::Symbol {
            name: "test".to_owned(),
            visibility: nudox_ir::entry::Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from("/tmp"),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut out = Lowering::new(pkg_id, root_sym);
        let ty = lower_type(
            &OracleType::Named {
                name: "Session".to_owned(),
                args: Vec::new(),
                decl_usr: Some("USR-Session".to_owned()),
            },
            &mut out,
            &locals,
        );
        assert!(
            matches!(ty, Type::Nominal(Ref::Local(_))),
            "a type this package declares must be a local nominal, got {ty:?}"
        );
        lower_oracle(&oracle, &mut out);
        out.finish()
            .expect("the local nominal's USR is declared by lower_oracle");
    }

    /// `std::` is a foreign nominal. `refer()` would make `finish` return
    /// `Undeclared` because this package never declares the standard library.
    #[test]
    fn std_spelled_name_is_nominal_import_and_finish_succeeds() {
        let (ty, out) = lower_alone(&OracleType::Named {
            name: "std::string".to_owned(),
            args: Vec::new(),
            decl_usr: Some("c:@N@std@T@string".to_owned()),
        });
        assert!(
            matches!(ty, Type::Nominal(Ref::Foreign { .. })),
            "std::string must be nominal_import, not refer(); got {ty:?}"
        );
        out.finish()
            .expect("nominal_import must not leave an undeclared local id");
    }
}
