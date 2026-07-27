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

use std::path::PathBuf;

use nudox_ir::{
    entry::{Symbol, Visibility},
    kinds::{
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, GenericParam,
        Module, Param, ParamAttribute, Receiver, Record, RecordForm, Static, Type, Variant,
        VariantForm,
    },
    lower::Lowering,
};

use crate::oracle::{
    ClangOracle, OracleAlias, OracleEnum, OracleField, OracleFnMod, OracleFunction,
    OracleGenericParam, OracleNamespace, OracleReceiver, OracleRecord, OracleType,
    OracleVar, OracleVariant, OracleVisibility, Usr,
};

// ── Public entry point ────────────────────────────────────────────────────────

/// Emit all declarations from `oracle` into `out`.
///
/// Called from [`ClangProducer::lower`].
pub fn lower_oracle(oracle: &ClangOracle, out: &mut Lowering<Usr>) {
    for ns in &oracle.namespaces {
        lower_namespace(ns, out);
    }
    for rec in &oracle.records {
        lower_record(rec, oracle, out);
    }
    for fun in &oracle.functions {
        lower_function(fun, oracle, out);
    }
    for e in &oracle.enums {
        lower_enum(e, oracle, out);
    }
    for alias in &oracle.aliases {
        lower_alias(alias, out);
    }
    for var in &oracle.vars {
        lower_var(var, out);
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
        span: offset..offset,
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

fn lower_record(rec: &OracleRecord, oracle: &ClangOracle, out: &mut Lowering<Usr>) {
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
    let super_types: Vec<Type> = rec.super_types.iter().map(lower_type).collect();

    // RecordForm — IR has no Union variant; map union → Struct (gap noted in module doc).
    let form = match (rec.is_union, rec.is_class) {
        _ if rec.is_union => RecordForm::Union,
        _ => RecordForm::Struct,
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
    for field in oracle.fields.iter().filter(|f| f.parent_usr.as_deref() == Some(&rec.usr)) {
        lower_field(field, out);
    }
}

// ── Field ─────────────────────────────────────────────────────────────────────

fn lower_field(f: &OracleField, out: &mut Lowering<Usr>) {
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
        .ty(lower_type(&f.ty))
        .attributes(attrs)
        .build();

    out.declare(f.usr.clone(), f.parent_usr.clone(), sym, kind);
}

// ── Function ─────────────────────────────────────────────────────────────────

fn lower_function(fun: &OracleFunction, _oracle: &ClangOracle, out: &mut Lowering<Usr>) {
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
                source: fun.source_file.clone(),
                span: 0..0,
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
                .ty(lower_type(&p.ty))
                .attributes(param_attrs)
                .build();

            out.declare(param_usr, Some(fun.usr.clone()), sym, kind);
            pref
        })
        .collect();

    // Variadic trailing param.
    let mut variadic_ref = None;
    if fun.variadic {
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
        out.declare(vusr, Some(fun.usr.clone()), vsym, kind);
        variadic_ref = Some(vref);
    }

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
        let kind = Param::builder().ty(lower_type(&fun.ret)).build();
        out.declare(ret_usr, Some(fun.usr.clone()), rsym, kind);
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
    for v in oracle.variants.iter().filter(|v| v.parent_usr.as_deref() == Some(&e.usr)) {
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

fn lower_alias(a: &OracleAlias, out: &mut Lowering<Usr>) {
    let sym = make_sym(
        &a.name,
        a.source_file.clone(),
        a.byte_offset,
        &a.documentation,
        a.visibility,
    );
    let kind = Alias::builder().target(lower_type(&a.target)).build();
    out.declare(a.usr.clone(), parent_id(&a.parent_usr), sym, kind);
}

// ── Variable (const / static) ─────────────────────────────────────────────────

fn lower_var(v: &OracleVar, out: &mut Lowering<Usr>) {
    let sym = make_sym(
        &v.name,
        v.source_file.clone(),
        v.byte_offset,
        &v.documentation,
        v.visibility,
    );
    let ty = lower_type(&v.ty);

    if v.is_const {
        let kind = Const::builder().ty(ty).build();
        out.declare(v.usr.clone(), parent_id(&v.parent_usr), sym, kind);
    } else {
        let kind = Static::builder().ty(ty).mutable(true).build();
        out.declare(v.usr.clone(), parent_id(&v.parent_usr), sym, kind);
    }
}

// ── Type lowering ─────────────────────────────────────────────────────────────

fn lower_type(ty: &OracleType) -> Type {
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

        OracleType::ConstPointer(inner) => {
            Type::Primitive(Primitive::ConstPointer(Box::new(lower_type(inner))))
        }
        OracleType::MutPointer(inner) => {
            Type::Primitive(Primitive::MutPointer(Box::new(lower_type(inner))))
        }
        OracleType::LValueRef { mutable, ty } => {
            Type::Primitive(Primitive::Reference {
                lifetime: None,
                mutable: *mutable,
                ty: Box::new(lower_type(ty)),
            })
        }
        OracleType::RValueRef(inner) => {
            // No dedicated RValueRef in the IR; model as mutable reference.
            Type::Primitive(Primitive::Reference {
                lifetime: None,
                mutable: true,
                ty: Box::new(lower_type(inner)),
            })
        }
        OracleType::Array { ty, len } => Type::Array {
            ty: Box::new(lower_type(ty)),
            length: *len,
        },
        OracleType::Slice(inner) => Type::Slice(Box::new(lower_type(inner))),
        OracleType::FnPtr { ret, params } => {
            // C function pointers lower to Type::FunctionPointer.
            // `params` are the positional parameter types in order.
            // `ret` is the return type; if void, lower to None (no return).
            // ABI is None (C default / language default — no __attribute__ ABI
            // annotation is surfaced at this level by libclang).
            let lowered_params: Vec<Type> = params.iter().map(lower_type).collect();
            let lowered_ret = if matches!(ret.as_ref(), OracleType::Void) {
                None
            } else {
                Some(Box::new(lower_type(ret)))
            };
            Type::FunctionPointer {
                params: lowered_params.into_boxed_slice(),
                ret: lowered_ret,
                abi: None,
            }
        }
        OracleType::Named { name, args } if args.is_empty() => {
            // Pure nominal reference; no apply args.
            // We cannot resolve USRs at lower time, so use TypeVar as a string ref
            // to avoid the overhead of a full Nominal RawRef resolution.
            // The caller can post-process to resolve these via the registry.
            Type::TypeVar(name.clone())
        }
        OracleType::Named { name, args } => {
            let base = Box::new(Type::TypeVar(name.clone()));
            let lowered_args: Vec<Type> = args.iter().map(lower_type).collect();
            Type::Apply {
                base,
                args: lowered_args.into(),
            }
        }
        OracleType::TypeVar(name) => Type::TypeVar(name.clone()),
        OracleType::Inferred => Type::Any,
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
