//! Raising: nudox-ir semantic types → wire-format types.
//!
//! This is [`crate::lower`] run backwards: it converts the semantic IR model
//! (`Entry`, `Kind`, `Type`, `Symbol`, …) into the local VCS format-layer
//! types (`OwnedEntryPayload`, `KindWire`, `TypeWire`, `SymbolWire`, …) that
//! [`crate::repo::IrRepository::record_generation`] and
//! [`crate::repo::IrRepository::materialize`] actually store and read.
//!
//! # The error class this closes
//!
//! Before this module existed there was **no function anywhere in the tree**
//! that built an `OwnedEntryPayload` from a real `Entry` — every existing
//! caller of `OwnedEntryPayload`/`PayloadTable` was a hand-built test fixture
//! (`f1/tests.rs`, `size_tests.rs`, `checkout_tests.rs`, `subst.rs`,
//! `diff/tests.rs`). `IrRepository::record_generation` therefore had a
//! producer with nothing to feed it: a package could be *lowered* out of the
//! local store, but nothing could put one *in*. That is IR-STORAGE-PLAN §1a/P5
//! ("wire `IrRepository` as the local store") stalled on a missing encoder,
//! exactly mirroring §5's `compile_inprocess.rs` gap on the remote-object
//! side. This module is the local-store half of that fix; §5's own advice
//! ("do not write that encoder") is scoped to the *remote* `GenerationRoot`
//! plane, which hashes semantic `Entry` payloads directly and has no wire
//! format to raise into — it does not apply here, because
//! `IrRepository::record_generation` is hard-typed to `&PayloadTable` and has
//! no other entry point.
//!
//! # Fidelity — read this before trusting a round-trip
//!
//! `crate::wire`'s own module doc says it plainly: these types "carry more
//! information than the F1 format encodes." That gap predates this module and
//! is **not** closed by it — closing it would mean extending the wire format,
//! which is out of scope for wiring up local persistence and is a decision
//! for whoever next revisits `docs/IR-STORAGE-PLAN.md` §4-§5. Two concrete
//! restrictions matter most:
//!
//! 1. **`TypeRefWire` names only nominal references** (`Same(IntroId)` /
//!    `Foreign(StableRef)`) — there is no variant for "here is a primitive
//!    type inline". Every non-nominal `Type` that needs a `TypeRefWire` slot
//!    (a function parameter's `u32`, a field's `bool`, …) is therefore given a
//!    **synthetic, content-addressed `IntroId`** that does not name any live
//!    table entry — see [`synthetic_type_intro`]. This keeps
//!    `record_generation`'s content-diffing correct (the same `Type` always
//!    raises to the same bytes, so an unchanged generation is still detected
//!    as unchanged) at the cost of the *read* side seeing a dangling nominal
//!    reference instead of the original primitive. Nothing in this crate
//!    currently resolves that reference, so nothing currently observes the
//!    loss — but a future reader must not assume every `TypeRefWire::Same`
//!    names a live entry.
//! 2. **`TypeWire` predates most of `Type`'s variants.** It can express
//!    `SelfType`, `Primitive`, `Tuple` (unlabelled), `Slice`, `Array`,
//!    `Union`, `Intersection`, `Never`, and `Any` — and nothing else. A
//!    `Type::Nominal` (e.g. `impl Display for Point`'s `self_ty`), `Apply`,
//!    `TypeVar`, `Wildcard`, `FunctionPointer`, `Annotated`, `Conditional`,
//!    `Mapped`, `TemplateLiteral`, `AnonymousRecord`, `ImplTrait`, `DynTrait`,
//!    `Inferred`, `QualifiedPath`, or `Unknown` has **no** `TypeWire`
//!    representation at all and raises to [`TypeWire::Any`] — see
//!    [`raise_type_wire`]. This can make two structurally different
//!    declarations (e.g. two `impl`s for different nominal self-types) raise
//!    to identical wire bytes; the persisted local copy is a lossy compression
//!    of the produced IR, not a lossless mirror of it.
//!
//! Both restrictions are pre-existing properties of `crate::wire`, not
//! defects introduced here — see that module's own doc comment. They matter
//! for what a *reader* of the local store can trust, not for whether
//! persistence itself works: `record_generation`'s change detection operates
//! on the raised bytes, and raising is deterministic (a pure function of the
//! `Entry`/`Type` values), so identical input always produces identical
//! output regardless of how much of the original richness survived the trip.
//!
//! # What is dropped outright (no wire slot, not even lossy)
//!
//! A few semantic fields have no corresponding wire field at all (as opposed
//! to a wire field that only *partially* represents them). Each is called out
//! at its raise site rather than listed here in the abstract, but the
//! headline ones: `Record.super_types`, `Field.key`/`attributes`,
//! `Function.throws`, `Alias.bounds`, `Symbol.doc_links` (the wire form needs
//! a resolved `StableRef` and the semantic form only ever carries free text —
//! recovering one from the other is a link-resolution pass this module does
//! not have access to), and generic-parameter `variance`.

use ir::{
    change::{IntroId, PackageLineageId, StableRef},
    entry::{CfgExpr, Entry, EntryInner, Symbol},
    index::{Indexable, RawRef, Ref},
    kind::{Kind, KindDiscriminant},
    kinds::{
        alias::Alias,
        const_::Const,
        facts::{AutoFact, AutoState, AutoTrait, Sealed, TriState},
        function::{FnModifier, Function, Receiver},
        generics::{GenericParam, WherePred},
        impl_::Impl,
        param::Param,
        record::{Field, Record, RecordForm},
        static_::Static,
        sum::{Enum, Variant, VariantForm},
        // `ir::kinds::trait_::TraitFlags` and `crate::wire::TraitFlags` share
        // a name; the wire side is aliased below (`TraitFlags as
        // WireTraitFlags`), so the semantic side is aliased here to keep
        // every call site unqualified and unambiguous.
        trait_::{Trait, TraitFlags as TraitFlagsSemantic},
        ty::{Primitive, TupleElement, Type, Width},
    },
    view::IrView,
};

use crate::wire::{
    AttrTok as WireAttrTok, AutoFact as WireAutoFact, AutoState as WireAutoState,
    AutoTrait as WireAutoTrait, CfgExpr as WireCfgExpr, ConstWire, DeprecationWire, EntryPayloadFlags,
    EnumWire, FieldWire, FnSigFlags, FunctionWire, GenericParamWire, ImplFlags as WireImplFlags,
    ImplWire, KindWire, ModuleWire, OwnedEntryPayload, ParamWire, PayloadTable, PrimitiveWire,
    RecordForm as WireRecordForm, RecordWire, ReexportWire, Sealed as WireSealed, SelfKind,
    StaticWire, SymbolWire, TraitFlags as WireTraitFlags, TraitWire, TriState as WireTriState,
    TypeAliasWire, TypeRefWire, TypeWire, VariantForm as WireVariantForm, VariantWire, WherePredWire,
    WidthWire,
};

// ---------------------------------------------------------------------------
// Public entry point: IrView → PayloadTable
// ---------------------------------------------------------------------------

/// Raise every live entry of `view` into a [`PayloadTable`], suitable for
/// [`crate::repo::IrRepository::record_generation`].
///
/// Walks `entries_sorted` (deterministic `IntroId` order) rather than
/// `entries` so two calls over the same view always raise to byte-identical
/// output — load-bearing for `record_generation`'s content diffing, which
/// would otherwise see a spurious change every time the walk order happened
/// to differ (`IrView::entries`'s own docs: unordered iteration is a
/// per-process `HashMap` seed).
///
/// Occurrence-derived cross-references (`PackageIndexes::usages`/`type_refs`)
/// are not projected into [`PayloadTable`]'s link set — that is a distinct
/// enhancement (wiring the reverse-position index into `PayloadTable::insert_link`)
/// left for whoever picks up P6 (fault-in) or a richer local `symbol_history`,
/// not required for a package to survive a restart.
pub fn raise_view(view: &IrView) -> PayloadTable {
    let mut table = PayloadTable::new();
    for (intro, entry) in view.entries_sorted() {
        let parent = view.parent_of(intro);
        let payload = raise_entry(view, entry);
        table.insert_live(intro, payload, parent);
    }
    table
}

// ---------------------------------------------------------------------------
// Entry → OwnedEntryPayload
// ---------------------------------------------------------------------------

fn raise_entry(view: &IrView, entry: &Entry) -> OwnedEntryPayload {
    let symbol = raise_symbol(entry.sym());
    let (kind_disc, kind) = match entry.kind() {
        // `Kind::Param` needs the owning entry's own `Symbol.name` (a
        // standalone parameter entry's name lives on itself, not inside the
        // `Param` body — mirrors `push_param` in `nudox-engine`'s
        // `store::package` reading it the same way). Special-cased here,
        // rather than inside `raise_kind`, because `raise_kind` has no
        // `Entry` in scope to read a name from.
        EntryInner::Owned(Kind::Param(p)) => {
            let name = entry.sym().name.clone();
            let name = if name.is_empty() { None } else { Some(name) };
            (KindDiscriminant::Param, KindWire::Param(raise_param(name, p)))
        }
        EntryInner::Owned(kind) => raise_kind(view, kind),
        // A re-export / alias entry (`Entry::reference`) has no owned `Kind`
        // at all — its payload IS the reference. `KindWire::Reexport` is the
        // only wire body shaped to carry a target, so that is what a
        // reference entry raises to, regardless of the (unrelated)
        // `KindWire::Reexport` produced by `Kind::Reexport(Reexport)` below.
        EntryInner::Reference(raw) => {
            let target = raw_ref_to_stable(raw, view.package());
            (
                KindDiscriminant::Reexport,
                KindWire::Reexport(ReexportWire { target }),
            )
        }
    };
    let flags = raise_flags(entry.sym());
    OwnedEntryPayload::sealed(symbol, kind_disc, kind, flags)
}

fn raise_flags(sym: &Symbol) -> EntryPayloadFlags {
    let mut flags = EntryPayloadFlags::default();
    if sym.deprecation.is_some() {
        flags.set(EntryPayloadFlags::HAS_DEPRECATION);
    }
    flags
}

// ---------------------------------------------------------------------------
// Symbol → SymbolWire
// ---------------------------------------------------------------------------

fn raise_symbol(sym: &Symbol) -> SymbolWire {
    SymbolWire {
        name: sym.name.clone(),
        visibility: sym.visibility,
        documentation: if sym.documentation.is_empty() {
            None
        } else {
            Some(sym.documentation.clone())
        },
        source_path: sym.source.to_string_lossy().into_owned(),
        span_start: sym.span.start as u32,
        span_end: sym.span.end as u32,
        aliases: sym.aliases.to_vec(),
        deprecation: sym.deprecation.as_ref().map(|d| DeprecationWire {
            note: d.note.clone(),
            since: d.since.clone(),
        }),
        // `DocLinkWire::target` is a resolved `StableRef`; `DocLink::target`
        // on the semantic side is free-form text (`lower_symbol` produces it
        // via `dl.target.to_string()` — a one-way translation this function
        // cannot reverse without a link-resolution pass it does not have
        // access to here). Doc-link cross-references are dropped on the way
        // into the local store; the documentation text itself (which still
        // contains the raw link) is unaffected.
        doc_links: Vec::new(),
        attrs: sym
            .attrs
            .iter()
            .map(|a| WireAttrTok {
                token: a.token.clone(),
                arg: a.arg.clone(),
            })
            .collect(),
        cfg: sym.cfg.as_ref().map(raise_cfg),
    }
}

fn raise_cfg(expr: &CfgExpr) -> WireCfgExpr {
    match expr {
        CfgExpr::All(inner) => WireCfgExpr::All(inner.iter().map(raise_cfg).collect()),
        CfgExpr::Any(inner) => WireCfgExpr::Any(inner.iter().map(raise_cfg).collect()),
        CfgExpr::Not(inner) => WireCfgExpr::Not(Box::new(raise_cfg(inner))),
        CfgExpr::Feature(s) => WireCfgExpr::Feature(s.clone()),
        CfgExpr::TargetOs(s) => WireCfgExpr::TargetOs(s.clone()),
        CfgExpr::TargetArch(s) => WireCfgExpr::TargetArch(s.clone()),
        CfgExpr::Other(s) => WireCfgExpr::Other(s.clone()),
    }
}

// ---------------------------------------------------------------------------
// Kind → (KindDiscriminant, KindWire)
// ---------------------------------------------------------------------------

fn raise_kind(view: &IrView, kind: &Kind) -> (KindDiscriminant, KindWire) {
    match kind {
        Kind::Module(_) => (KindDiscriminant::Module, KindWire::Module(ModuleWire::default())),
        Kind::Record(r) => (KindDiscriminant::Record, KindWire::Record(raise_record(r))),
        Kind::Field(f) => (KindDiscriminant::Field, KindWire::Field(raise_field(f))),
        Kind::Function(f) => (
            KindDiscriminant::Function,
            KindWire::Function(raise_function(view, f)),
        ),
        Kind::Alias(a) => (KindDiscriminant::Alias, KindWire::Type(raise_alias(a))),
        Kind::Trait(t) => (KindDiscriminant::Trait, KindWire::Trait(raise_trait(t))),
        Kind::Impl(i) => (KindDiscriminant::Impl, KindWire::Impl(raise_impl(i))),
        Kind::Enum(e) => (KindDiscriminant::Enum, KindWire::Enum(raise_enum(e))),
        Kind::Variant(v) => (KindDiscriminant::Variant, KindWire::Variant(raise_variant(v))),
        Kind::Const(c) => (KindDiscriminant::Const, KindWire::Const(raise_const(c))),
        Kind::Static(s) => (KindDiscriminant::Static, KindWire::Static(raise_static(s))),
        // `Kind::Reexport(Reexport)` is the OWNED marker kind, which per its
        // own doc comment carries no target — the real target lives on
        // `EntryInner::Reference`, handled in `raise_entry`. Reaching this
        // arm means a producer emitted an owned `Reexport` kind directly
        // (not through `Entry::reference`), which the type system allows but
        // which no in-tree producer does. There is nothing to raise it to
        // except a self-reference, which is honestly meaningless but keeps
        // the wire payload well-formed rather than panicking on input this
        // crate did not itself construct.
        Kind::Reexport(_) => (
            KindDiscriminant::Reexport,
            KindWire::Reexport(ReexportWire {
                target: StableRef::new(
                    view.package().clone(),
                    IntroId::from_domain("nudox.wire.unowned-reexport.v1", &[]),
                ),
            }),
        ),
        // Reachable only when a standalone `Param` entry is visited through
        // some path other than `raise_entry`'s special case above (there is
        // none today, but `Kind` is not `#[non_exhaustive]`, so this arm must
        // exist). No entry/symbol is in scope here to supply a name.
        Kind::Param(p) => (KindDiscriminant::Param, KindWire::Param(raise_param(None, p))),
    }
}

// ---------------------------------------------------------------------------
// Kind-body raisings
// ---------------------------------------------------------------------------

fn raise_record(r: &Record) -> RecordWire {
    RecordWire {
        form: raise_record_form(r.form),
        fields: r.fields.iter().filter_map(ref_intro).collect(),
        // `super_types` has no `RecordWire` field — mirrors `lower_record`,
        // which sets it to `Box::default()` on the way back in. Pre-existing
        // wire-format gap (see module docs).
        generics: raise_generics(&r.generics),
        wheres: raise_wheres(&r.wheres),
        auto: r.auto.iter().map(raise_auto_fact).collect(),
    }
}

fn raise_record_form(f: RecordForm) -> WireRecordForm {
    match f {
        RecordForm::Struct => WireRecordForm::Struct,
        RecordForm::Tuple => WireRecordForm::Tuple,
        RecordForm::Unit => WireRecordForm::Unit,
        RecordForm::Union => WireRecordForm::Union,
    }
}

fn raise_field(f: &Field) -> FieldWire {
    // `key` (named vs. positional) and `attributes` have no `FieldWire`
    // field — pre-existing wire-format gap (see module docs).
    FieldWire {
        ty: f.ty.as_ref().map(raise_type_ref),
    }
}

fn raise_function(view: &IrView, f: &Function) -> FunctionWire {
    FunctionWire {
        input_params: f.input_params.iter().map(|r| raise_param_ref(view, r)).collect(),
        output_params: f.output_params.iter().map(|r| raise_param_ref(view, r)).collect(),
        sig: FnSigFlags {
            self_kind: raise_receiver(f.receiver),
            is_async: f.modifiers.iter().any(|m| matches!(m, FnModifier::Async)),
            is_const: f.modifiers.iter().any(|m| matches!(m, FnModifier::Const)),
            is_unsafe: f.modifiers.iter().any(|m| matches!(m, FnModifier::Unsafe)),
            abi: f.abi.clone(),
            // `Function` has no signature-level variadic flag in the current
            // semantic model (variadic-ness moved to `ParamAttribute::Variadic`
            // on individual parameters, which `ParamWire` has no slot for
            // either) — pre-existing wire-format gap.
            variadic: false,
            defaulted: f.is_defaulted,
        },
        generics: raise_generics(&f.generics),
        wheres: raise_wheres(&f.wheres),
        // `throws` has no `FunctionWire` field — pre-existing wire-format gap.
    }
}

fn raise_receiver(receiver: Option<Receiver>) -> SelfKind {
    match receiver {
        None => SelfKind::None,
        Some(Receiver::Owned) => SelfKind::Value,
        Some(Receiver::SharedRef) => SelfKind::Ref,
        Some(Receiver::MutRef) => SelfKind::RefMut,
        // `Receiver::Arbitrary` carries no concrete type (see its doc
        // comment: adding one would break `Copy`), but `SelfKind::Arbitrary`
        // requires a `TypeRefWire` payload. There is nothing real to put
        // there; a fixed, domain-separated placeholder keeps the payload
        // well-formed and — because it is a *constant* — never spuriously
        // marks two otherwise-identical methods as different.
        Some(Receiver::Arbitrary) => SelfKind::Arbitrary(TypeRefWire::Same(
            IntroId::from_domain("nudox.wire.arbitrary-receiver.v1", &[]),
        )),
    }
}

/// Resolve one `Ref<Param>` (a function's input/output parameter list entry)
/// against `view` and raise the target `Param` entry inline.
///
/// `FunctionWire.input_params`/`output_params` store `ParamWire` **inline**
/// rather than by reference — the wire format predates params becoming
/// first-class entries with their own `IntroId` (see `KindWire::Param`'s doc
/// comment). Resolving the reference here, rather than leaving a dangling
/// pointer, is what lets `lower_function`'s reverse direction reconstruct a
/// (synthetic but deterministic) `Ref::Intro` for each parameter.
fn raise_param_ref(view: &IrView, r: &Ref<Param>) -> ParamWire {
    match r {
        Ref::Intro(id) => {
            if let Some(entry) = view.entry(*id)
                && let Some(Kind::Param(p)) = entry.kind().as_owned_kind()
            {
                let name = entry.sym().name.clone();
                let name = if name.is_empty() { None } else { Some(name) };
                return raise_param(name, p);
            }
            // The intro is not present in this view, or is present but is not
            // a `Param` entry (a malformed table). Preserve the parameter
            // *slot* — dropping it would silently change the function's
            // arity — with an unknown type rather than guessing one.
            ParamWire {
                name: None,
                ty: TypeRefWire::Same(synthetic_type_intro(&Type::UNANNOTATED)),
            }
        }
        Ref::Foreign { target: Some(sr), .. } => ParamWire {
            name: None,
            ty: TypeRefWire::Foreign(sr.clone()),
        },
        Ref::Foreign { target: None, .. } | Ref::Local(_) => ParamWire {
            name: None,
            ty: TypeRefWire::Same(synthetic_type_intro(&Type::UNANNOTATED)),
        },
    }
}

fn raise_param(name: Option<String>, p: &Param) -> ParamWire {
    ParamWire {
        name,
        ty: p
            .ty
            .as_ref()
            .map_or_else(|| synthetic_type_ref(&Type::UNANNOTATED), raise_type_ref),
        // `default_value` / `attributes` have no `ParamWire` field —
        // pre-existing wire-format gap.
    }
}

fn raise_alias(a: &Alias) -> TypeAliasWire {
    TypeAliasWire {
        // `target: None` (an abstract associated type, `type Item;`) has no
        // `TypeWire` equivalent either — `ty` is not optional on
        // `TypeAliasWire`. `Any` is the least-wrong placeholder available.
        ty: a.target.as_ref().map_or(TypeWire::Any, raise_type_wire),
        generics: raise_generics(&a.generics),
        wheres: raise_wheres(&a.wheres),
        auto: a.auto.iter().map(raise_auto_fact).collect(),
        // `bounds` has no `TypeAliasWire` field — pre-existing wire-format gap.
    }
}

fn raise_trait(t: &Trait) -> TraitWire {
    TraitWire {
        supers: t.supers.iter().map(raise_type_ref).collect(),
        flags: raise_trait_flags(t.flags),
        generics: raise_generics(&t.generics),
        wheres: raise_wheres(&t.wheres),
    }
}

fn raise_trait_flags(f: TraitFlagsSemantic) -> WireTraitFlags {
    WireTraitFlags {
        is_unsafe: f.is_unsafe,
        is_auto: f.is_auto,
        dyn_compat: raise_tristate(f.dyn_compat),
        sealed: raise_sealed(f.sealed),
    }
}

fn raise_tristate(t: TriState) -> WireTriState {
    match t {
        TriState::Yes => WireTriState::Yes,
        TriState::No => WireTriState::No,
        TriState::Unknown => WireTriState::Unknown,
    }
}

fn raise_sealed(s: Sealed) -> WireSealed {
    match s {
        Sealed::None => WireSealed::None,
        Sealed::PubApi => WireSealed::PubApi,
        Sealed::Full => WireSealed::Full,
    }
}

fn raise_impl(i: &Impl) -> ImplWire {
    ImplWire {
        of: i.of.as_ref().map(raise_type_ref),
        self_ty: raise_type_wire(&i.self_ty),
        flags: WireImplFlags {
            negative: i.flags.negative,
            blanket: i.flags.blanket,
        },
        generics: raise_generics(&i.generics),
        wheres: raise_wheres(&i.wheres),
    }
}

fn raise_enum(e: &Enum) -> EnumWire {
    EnumWire {
        variants: e.variants.iter().filter_map(ref_intro).collect(),
        generics: raise_generics(&e.generics),
        wheres: raise_wheres(&e.wheres),
        auto: e.auto.iter().map(raise_auto_fact).collect(),
    }
}

fn raise_variant(v: &Variant) -> VariantWire {
    VariantWire {
        form: raise_variant_form(v.form),
        fields: v.fields.iter().filter_map(ref_intro).collect(),
        discr: v.discr.clone(),
    }
}

fn raise_variant_form(f: VariantForm) -> WireVariantForm {
    match f {
        VariantForm::Unit => WireVariantForm::Unit,
        VariantForm::Tuple => WireVariantForm::Tuple,
        VariantForm::Struct => WireVariantForm::Struct,
    }
}

fn raise_const(c: &Const) -> ConstWire {
    ConstWire {
        ty: raise_type_ref(&c.ty),
        value: c.value.as_ref().map(|v| v.source.clone()),
    }
}

fn raise_static(s: &Static) -> StaticWire {
    StaticWire {
        ty: raise_type_ref(&s.ty),
        mutable: s.mutable,
    }
}

// ---------------------------------------------------------------------------
// Shared vocabulary raisings
// ---------------------------------------------------------------------------

fn raise_generics(generics: &[GenericParam]) -> Box<[GenericParamWire]> {
    generics.iter().map(raise_generic_param).collect()
}

fn raise_generic_param(gp: &GenericParam) -> GenericParamWire {
    match gp {
        GenericParam::Lifetime { name } => GenericParamWire::Lifetime { name: name.clone() },
        GenericParam::Type {
            name,
            bounds,
            default,
            // Declaration-site variance (C# `in T`/`out T`) has no
            // `GenericParamWire::Type` field — pre-existing wire-format gap.
            variance: _,
        } => GenericParamWire::Type {
            name: name.clone(),
            bounds: bounds.iter().map(raise_type_ref).collect(),
            default: default.as_ref().map(raise_type_wire),
        },
        GenericParam::Const { name, ty } => GenericParamWire::Const {
            name: name.clone(),
            ty: raise_type_ref(ty),
            // The const generic's default value has no semantic-side
            // counterpart to raise (`GenericParam::Const` carries none).
            default: None,
        },
    }
}

fn raise_wheres(wheres: &[WherePred]) -> Box<[WherePredWire]> {
    wheres
        .iter()
        .map(|wp| WherePredWire {
            target: raise_type_wire(&wp.target),
            bounds: wp.bounds.iter().map(raise_type_ref).collect(),
        })
        .collect()
}

fn raise_auto_fact(af: &AutoFact) -> WireAutoFact {
    let trait_ = match af.trait_ {
        AutoTrait::Send => WireAutoTrait::Send,
        AutoTrait::Sync => WireAutoTrait::Sync,
        AutoTrait::Unpin => WireAutoTrait::Unpin,
        AutoTrait::UnwindSafe => WireAutoTrait::UnwindSafe,
        AutoTrait::RefUnwindSafe => WireAutoTrait::RefUnwindSafe,
    };
    let state = match af.state {
        AutoState::Yes => WireAutoState::Yes,
        AutoState::No => WireAutoState::No,
        AutoState::Cond => WireAutoState::Cond,
    };
    WireAutoFact { trait_, state }
}

// ---------------------------------------------------------------------------
// Type raisings
// ---------------------------------------------------------------------------

/// Raise a `Type` into a `TypeRefWire` (a *reference*-position slot: field
/// types, parameter types, const/static types, trait supertypes, an impl's
/// `of`, generic bounds).
///
/// A genuinely nominal type raises to the reference it already is. Every
/// other `Type` is given a [`synthetic_type_intro`] — see the module docs for
/// why, and for what a reader loses as a result.
fn raise_type_ref(ty: &Type) -> TypeRefWire {
    // `Type::Nominal` wraps a `RawRef` (`= Ref<UntypedMarker>`), the same
    // `Ref` used for `Ref<Param>`/`Ref<Field>`/`Ref<Variant>` elsewhere in
    // this file.
    if let Type::Nominal(raw) = ty {
        match raw {
            Ref::Intro(id) => return TypeRefWire::Same(*id),
            Ref::Foreign { target: Some(sr), .. } => {
                return TypeRefWire::Foreign(sr.clone());
            }
            Ref::Foreign { target: None, .. } | Ref::Local(_) => {}
        }
    }
    synthetic_type_ref(ty)
}

fn synthetic_type_ref(ty: &Type) -> TypeRefWire {
    TypeRefWire::Same(synthetic_type_intro(ty))
}

/// Mint a deterministic, content-addressed [`IntroId`] standing in for a
/// [`Type`] that `TypeRefWire` has no direct way to name — see the module
/// docs' fidelity section for why this is necessary and what it costs.
///
/// Same trick [`lower::lower_param_as_intro`](crate::lower) already uses for
/// the *inverse* direction: the id is a pure function of the type's
/// structure, so two raisings of the same `Type` value always agree, which is
/// exactly what [`crate::repo::IrRepository::record_generation`]'s
/// content-diffing needs to recognise an unchanged generation. The id does
/// not name a live table entry.
fn synthetic_type_intro(ty: &Type) -> IntroId {
    let bytes = postcard::to_allocvec(ty).expect("Type postcard serialization is infallible");
    IntroId::from_domain("nudox.type-wire-ref.v1", &bytes)
}

/// Raise a `Type` into a `TypeWire` (a *value*-position slot: an impl's
/// `self_ty`, an alias's `target`, a where-predicate's `target`, a generic
/// parameter's `default`).
///
/// See the module docs' fidelity section: only the nine variants `TypeWire`
/// actually has map across; everything else — including `Type::Nominal`,
/// which has no `TypeWire` variant at all — falls back to [`TypeWire::Any`].
fn raise_type_wire(ty: &Type) -> TypeWire {
    match ty {
        Type::SelfType => TypeWire::SelfType,
        Type::Primitive(p) => TypeWire::Primitive(raise_primitive(p)),
        Type::Tuple(elements) => TypeWire::Tuple(
            elements
                .iter()
                .map(|e| match e {
                    // `TypeWire::Tuple` carries no per-element label slot — a
                    // labelled tuple element (`(int start, int end)`) loses
                    // its label on the way into the local store; pre-existing
                    // wire-format gap.
                    TupleElement::Positional(t) | TupleElement::Named { ty: t, .. } => {
                        raise_type_ref(t)
                    }
                })
                .collect(),
        ),
        Type::Slice(inner) => TypeWire::Slice(Box::new(raise_type_ref(inner))),
        Type::Array { ty, length } => TypeWire::Array {
            ty: Box::new(raise_type_ref(ty)),
            length: *length as u64,
        },
        Type::Union(list) => TypeWire::Union(list.iter().map(raise_type_ref).collect()),
        Type::Intersection(list) => TypeWire::Intersection(list.iter().map(raise_type_ref).collect()),
        Type::Never => TypeWire::Never,
        // `Type::Any` falls through to the wildcard below — it happens to
        // want the same `TypeWire::Any` value the fallback already produces
        // for everything `TypeWire` cannot represent at all (`Nominal`,
        // `Apply`, `TypeVar`, `Wildcard`, `FunctionPointer`, `Annotated`,
        // `Conditional`, `Mapped`, `TemplateLiteral`, `AnonymousRecord`,
        // `ImplTrait`, `DynTrait`, `Inferred`, `QualifiedPath`, `Unknown`) —
        // see the module docs' fidelity section, point 2.
        _ => TypeWire::Any,
    }
}

fn raise_primitive(p: &Primitive) -> PrimitiveWire {
    match p {
        Primitive::Integer { signed, width } => PrimitiveWire::Integer {
            signed: *signed,
            width: raise_width(*width),
        },
        Primitive::Float(w) => PrimitiveWire::Float(raise_width(*w)),
        Primitive::Bool => PrimitiveWire::Bool,
        Primitive::Char => PrimitiveWire::Char,
        Primitive::Str => PrimitiveWire::Str,
        Primitive::MutPointer(inner) => PrimitiveWire::MutPointer(Box::new(raise_type_ref(inner))),
        Primitive::ConstPointer(inner) => {
            PrimitiveWire::ConstPointer(Box::new(raise_type_ref(inner)))
        }
        Primitive::Reference { lifetime, mutable, ty } => PrimitiveWire::Reference {
            lifetime: lifetime.clone(),
            mutable: *mutable,
            ty: Box::new(raise_type_ref(ty)),
        },
        Primitive::Builtin(s) => PrimitiveWire::Builtin(s.clone()),
    }
}

fn raise_width(w: Width) -> WidthWire {
    match w {
        Width::Arch => WidthWire::Arch,
        Width::Fixed(n) => WidthWire::Fixed(u32::from(n.get())),
    }
}

// ---------------------------------------------------------------------------
// Reference helpers
// ---------------------------------------------------------------------------

/// The `IntroId` a same-package `Ref<T>` names, if it names one directly.
///
/// `None` for `Foreign`/`Local` — used at call sites (`Record.fields`,
/// `Enum.variants`, `Variant.fields`) whose wire counterpart
/// (`Box<[IntroId]>`) has no slot for a cross-package or build-time
/// reference; those members are dropped rather than guessed at. A `Local`
/// surviving into a sealed table is already a seal bug per `Ref::Local`'s own
/// doc comment, so silently omitting it here does not hide a case any
/// well-formed input would produce.
fn ref_intro<T: Indexable>(r: &Ref<T>) -> Option<IntroId> {
    match r {
        Ref::Intro(id) => Some(*id),
        Ref::Foreign { .. } | Ref::Local(_) => None,
    }
}

/// Resolve a `RawRef` (an `Entry::reference`'s target) to a [`StableRef`].
///
/// `package` is the raising package's own lineage, used to qualify a
/// same-package `Ref::Intro`. `Ref::Foreign { target: None, .. }` (named but
/// not yet linked) and `Ref::Local` (should not survive `seal`) have no
/// `StableRef` this function can honestly produce; both fall back to a fixed,
/// domain-separated placeholder rather than panicking on input this crate did
/// not itself construct.
fn raw_ref_to_stable(raw: &RawRef, package: &PackageLineageId) -> StableRef {
    match raw {
        Ref::Intro(id) => StableRef::new(package.clone(), *id),
        Ref::Foreign { target: Some(sr), .. } => sr.clone(),
        Ref::Foreign { target: None, .. } | Ref::Local(_) => StableRef::new(
            package.clone(),
            IntroId::from_domain("nudox.wire.unresolved-ref.v1", &[]),
        ),
    }
}
