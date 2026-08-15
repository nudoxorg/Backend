//! Lowering: wire-format types → nudox-ir semantic types.
//!
//! This module converts the local VCS format-layer types (`OwnedEntryPayload`,
//! `KindWire`, `TypeWire`, `SymbolWire`, etc.) into the nudox-ir semantic
//! model (`Entry`, `Kind`, `Type`, `Symbol`, `Function`, …).
//!
//! The conversion is used in two places:
//!
//! 1. **`session.rs`** — build `IrView` objects from staged `OwnedEntryPayload`
//!    entries so that `ir::continuity::resolve` can run Phase B matching.
//!
//! 2. **Nowhere else yet** — as the migration progresses, `repo.rs` and the
//!    archive/diff layers will eventually consume `Entry` natively. Until then,
//!    this module provides a clean conversion seam.
//!
//! # Fidelity
//!
//! The lowering is structurally faithful for all 12 kind variants. The one
//! deliberate approximation is `RecordWire.fields: Box<[IntroId]>` →
//! `Record.fields: Box<[Ref<Field>]>`: `IntroId`s are wrapped as
//! `Ref::<Field>::Intro(id)` post-seal references, which is correct for sealed
//! tables built from F1 files.  Similarly for `EnumWire.variants`,
//! `VariantWire.fields`, and `ReexportWire.target`.

use std::num::NonZeroU16;
use std::path::PathBuf;

use triomphe::Arc;

use ir::{
    apply::PristineIntroTable,
    body::BodyEmbed,
    change::{IntroId, PackageLineageId},
    entry::{AttrTok, CfgExpr, Deprecation, DocLink, Entry, Node, Symbol},
    foreign::ForeignKey,
    index::{RawRef, Ref},
    kind::Kind,
    kinds::{
        Module,
        alias::Alias,
        const_::Const,
        facts::{AutoFact, AutoState, AutoTrait, Sealed, TriState},
        function::{FnModifier, Function, Receiver},
        generics::{GenericParam, WherePred},
        impl_::{Impl, ImplFlags},
        param::Param,
        record::{Field, FieldKey, Record, RecordForm},
        reexport::Reexport,
        static_::Static,
        sum::{Enum, Variant, VariantForm},
        trait_::{Trait, TraitFlags},
        ty::{Primitive, TupleElement, Type, Width},
    },
    view::IrView,
};

use crate::wire::{
    AutoFact as WireAutoFact, AutoState as WireAutoState, AutoTrait as WireAutoTrait, FnSigFlags,
    GenericParamWire, KindWire, OwnedEntryPayload, ParamWire, PrimitiveWire,
    RecordForm as WireRecordForm, Sealed as WireSealed, SelfKind, SymbolWire,
    TraitFlags as WireTraitFlags, TriState as WireTriState, TypeAliasWire, TypeRefWire, TypeWire,
    VariantForm as WireVariantForm, WherePredWire,
};

// ---------------------------------------------------------------------------
// Public entry point: OwnedEntryPayload → Entry
// ---------------------------------------------------------------------------

/// Lower an [`OwnedEntryPayload`] to a nudox-ir [`Entry`].
///
/// The `Node` is always constructed as a root (`None` parent, empty children),
/// because the caller is responsible for recording parent edges separately via
/// `PristineIntroTable::insert_live`.
pub fn lower_payload(payload: &OwnedEntryPayload) -> Entry {
    let sym = lower_symbol(&payload.symbol);
    let node = Node::build(None::<RawRef>, []);
    let kind = lower_kind(&payload.kind);
    Entry::new(sym, node, kind)
}

/// Build an [`IrView`] from a snapshot of `(IntroId, OwnedEntryPayload, parent)`
/// triples.  The package lineage is bound to the view so that continuity
/// matching can emit correct `StableRef`s for same-package entries.
///
/// Bodies are attached from `bodies_bt` (keyed by wire id, which equals the
/// durable id in the tip table that this is called with).
pub fn build_ir_view(
    package: PackageLineageId,
    entries: impl IntoIterator<Item = (IntroId, OwnedEntryPayload, Option<IntroId>)>,
    bodies: &std::collections::BTreeMap<IntroId, BodyEmbed>,
) -> IrView {
    let mut table = PristineIntroTable::new();
    for (id, payload, parent) in entries {
        let entry = lower_payload(&payload);
        table.insert_live(id, entry, parent);
    }
    let mut view = IrView::with_package(package, table);
    for (id, body) in bodies {
        view.set_body(*id, body.clone());
    }
    view
}

// ---------------------------------------------------------------------------
// SymbolWire → Symbol
// ---------------------------------------------------------------------------

fn lower_symbol(sw: &SymbolWire) -> Symbol {
    Symbol {
        name: sw.name.clone(),
        visibility: sw.visibility,
        documentation: sw.documentation.clone().unwrap_or_default(),
        source: PathBuf::from(&sw.source_path),
        span: sw.span_start as usize..sw.span_end as usize,
        aliases: sw.aliases.iter().cloned().collect(),
        deprecation: sw.deprecation.as_ref().map(|d| Deprecation {
            note: d.note.clone(),
            since: d.since.clone(),
        }),
        doc_links: sw
            .doc_links
            .iter()
            .map(|dl| DocLink {
                target: dl.target.to_string(),
                label: dl.label.clone(),
            })
            .collect(),
        attrs: sw
            .attrs
            .iter()
            .map(|a| AttrTok {
                token: a.token.clone(),
                arg: a.arg.clone(),
            })
            .collect(),
        cfg: sw.cfg.as_ref().map(lower_cfg),
    }
}

fn lower_cfg(expr: &crate::wire::CfgExpr) -> CfgExpr {
    match expr {
        crate::wire::CfgExpr::All(inner) => CfgExpr::All(inner.iter().map(lower_cfg).collect()),
        crate::wire::CfgExpr::Any(inner) => CfgExpr::Any(inner.iter().map(lower_cfg).collect()),
        crate::wire::CfgExpr::Not(inner) => CfgExpr::Not(Box::new(lower_cfg(inner))),
        crate::wire::CfgExpr::Feature(s) => CfgExpr::Feature(s.clone()),
        crate::wire::CfgExpr::TargetOs(s) => CfgExpr::TargetOs(s.clone()),
        crate::wire::CfgExpr::TargetArch(s) => CfgExpr::TargetArch(s.clone()),
        crate::wire::CfgExpr::Other(s) => CfgExpr::Other(s.clone()),
    }
}

// ---------------------------------------------------------------------------
// KindWire → Kind
// ---------------------------------------------------------------------------

fn lower_kind(kw: &KindWire) -> Kind {
    match kw {
        KindWire::Module(_) => Kind::Module(Module),
        KindWire::Record(r) => Kind::Record(lower_record(r)),
        KindWire::Field(f) => Kind::Field(lower_field(f)),
        KindWire::Function(f) => Kind::Function(lower_function(f)),
        KindWire::Type(a) => Kind::Alias(lower_alias(a)),
        KindWire::Trait(t) => Kind::Trait(lower_trait(t)),
        KindWire::Impl(i) => Kind::Impl(lower_impl(i)),
        KindWire::Enum(e) => Kind::Enum(lower_enum(e)),
        KindWire::Variant(v) => Kind::Variant(lower_variant(v)),
        KindWire::Const(c) => Kind::Const(lower_const(c)),
        KindWire::Static(s) => Kind::Static(lower_static(s)),
        KindWire::Reexport(_) => Kind::Reexport(Reexport),
        KindWire::Param(p) => Kind::Param(lower_param(p)),
    }
}

// ---------------------------------------------------------------------------
// Kind-body lowerings
// ---------------------------------------------------------------------------

fn lower_record(r: &crate::wire::RecordWire) -> Record {
    let form = lower_record_form(r.form);
    let fields: Box<[Ref<Field>]> = r.fields.iter().map(|&id| Ref::<Field>::Intro(id)).collect();
    let generics = lower_generics(&r.generics);
    let wheres = lower_wheres(&r.wheres);
    let auto: Box<[AutoFact]> = r.auto.iter().map(lower_auto_fact).collect();
    Record {
        form,
        fields,
        super_types: Box::default(),
        generics,
        wheres,
        auto,
    }
}

fn lower_field(f: &crate::wire::FieldWire) -> Field {
    let ty = f.ty.as_ref().map(lower_type_ref);
    Field {
        key: FieldKey::Named,
        ty,
        attributes: Box::default(),
    }
}

fn lower_function(fw: &crate::wire::FunctionWire) -> Function {
    let receiver = lower_receiver(&fw.sig.self_kind);
    let input_params: Box<[Ref<Param>]> = fw
        .input_params
        .iter()
        .map(|p| Ref::<Param>::Intro(lower_param_as_intro(p)))
        .collect();
    let output_params: Box<[Ref<Param>]> = fw
        .output_params
        .iter()
        .map(|p| Ref::<Param>::Intro(lower_param_as_intro(p)))
        .collect();
    let modifiers = lower_fn_modifiers(&fw.sig);
    let generics = lower_generics(&fw.generics);
    let wheres = lower_wheres(&fw.wheres);
    Function {
        receiver,
        input_params,
        output_params,
        modifiers,
        generics,
        wheres,
        abi: fw.sig.abi.clone(),
        is_defaulted: fw.sig.defaulted,
        throws: Box::default(),
    }
}

fn lower_alias(a: &TypeAliasWire) -> Alias {
    let target = Some(lower_type_wire(&a.ty));
    let generics = lower_generics(&a.generics);
    let wheres = lower_wheres(&a.wheres);
    let auto: Box<[AutoFact]> = a.auto.iter().map(lower_auto_fact).collect();
    Alias {
        target,
        generics,
        wheres,
        bounds: Box::default(),
        auto,
    }
}

fn lower_trait(t: &crate::wire::TraitWire) -> Trait {
    let flags = lower_trait_flags(&t.flags);
    let supers: Box<[Type]> = t.supers.iter().map(lower_type_ref).collect();
    let generics = lower_generics(&t.generics);
    let wheres = lower_wheres(&t.wheres);
    Trait {
        flags,
        supers,
        generics,
        wheres,
    }
}

fn lower_impl(i: &crate::wire::ImplWire) -> Impl {
    let flags = ImplFlags {
        negative: i.flags.negative,
        blanket: i.flags.blanket,
    };
    let of = i.of.as_ref().map(lower_type_ref);
    let self_ty = lower_type_wire(&i.self_ty);
    let generics = lower_generics(&i.generics);
    let wheres = lower_wheres(&i.wheres);
    Impl {
        flags,
        of,
        self_ty,
        generics,
        wheres,
    }
}

fn lower_enum(e: &crate::wire::EnumWire) -> Enum {
    let variants: Box<[Ref<Variant>]> = e
        .variants
        .iter()
        .map(|&id| Ref::<Variant>::Intro(id))
        .collect();
    let generics = lower_generics(&e.generics);
    let wheres = lower_wheres(&e.wheres);
    let auto: Box<[AutoFact]> = e.auto.iter().map(lower_auto_fact).collect();
    Enum {
        variants,
        generics,
        wheres,
        auto,
    }
}

fn lower_variant(v: &crate::wire::VariantWire) -> Variant {
    let form = lower_variant_form(v.form);
    let fields: Box<[Ref<Field>]> = v.fields.iter().map(|&id| Ref::<Field>::Intro(id)).collect();
    Variant {
        form,
        fields,
        discr: v.discr.clone(),
    }
}

fn lower_const(c: &crate::wire::ConstWire) -> Const {
    Const {
        ty: lower_type_ref(&c.ty),
        value: c.value.clone(),
    }
}

fn lower_static(s: &crate::wire::StaticWire) -> Static {
    Static {
        ty: lower_type_ref(&s.ty),
        mutable: s.mutable,
    }
}

fn lower_param(p: &ParamWire) -> ir::kinds::param::Param {
    ir::kinds::param::Param {
        ty: Some(lower_type_ref(&p.ty)),
        attributes: Box::default(),
    }
}

// ---------------------------------------------------------------------------
// Shared vocabulary lowerings
// ---------------------------------------------------------------------------

fn lower_record_form(f: WireRecordForm) -> RecordForm {
    match f {
        WireRecordForm::Struct => RecordForm::Struct,
        WireRecordForm::Tuple => RecordForm::Tuple,
        WireRecordForm::Unit => RecordForm::Unit,
        WireRecordForm::Union => RecordForm::Union,
    }
}

fn lower_variant_form(f: WireVariantForm) -> VariantForm {
    match f {
        WireVariantForm::Unit => VariantForm::Unit,
        WireVariantForm::Tuple => VariantForm::Tuple,
        WireVariantForm::Struct => VariantForm::Struct,
    }
}

fn lower_receiver(sk: &SelfKind) -> Option<Receiver> {
    match sk {
        SelfKind::None => None,
        SelfKind::Value => Some(Receiver::Owned),
        SelfKind::Ref => Some(Receiver::SharedRef),
        SelfKind::RefMut => Some(Receiver::MutRef),
        SelfKind::Arbitrary(_) => Some(Receiver::Arbitrary),
    }
}

fn lower_fn_modifiers(sig: &FnSigFlags) -> Box<[FnModifier]> {
    let mut mods = Vec::new();
    if sig.is_async {
        mods.push(FnModifier::Async);
    }
    if sig.is_const {
        mods.push(FnModifier::Const);
    }
    if sig.is_unsafe {
        mods.push(FnModifier::Unsafe);
    }
    mods.into_boxed_slice()
}

/// Compute a deterministic, content-addressed [`IntroId`] for a parameter wire
/// so that `Ref::<Param>::Intro(id)` is stable across lowering calls with the
/// same `ParamWire`.
///
/// The domain is `"nudox.param-wire.v1"` to distinguish it from real
/// declaration IntroIds.
fn lower_param_as_intro(p: &ParamWire) -> IntroId {
    use ir::change::ContentBlake3;
    let bytes = postcard::to_allocvec(p).expect("ParamWire postcard must not fail");
    let hash = ContentBlake3::from_domain("nudox.param-wire.v1", &bytes);
    IntroId::from_raw(*hash.as_bytes())
}

fn lower_trait_flags(tf: &WireTraitFlags) -> TraitFlags {
    TraitFlags {
        is_unsafe: tf.is_unsafe,
        is_auto: tf.is_auto,
        dyn_compat: lower_tristate(tf.dyn_compat.clone()),
        sealed: lower_sealed(tf.sealed.clone()),
    }
}

fn lower_tristate(t: WireTriState) -> TriState {
    match t {
        WireTriState::Yes => TriState::Yes,
        WireTriState::No => TriState::No,
        WireTriState::Unknown => TriState::Unknown,
    }
}

fn lower_sealed(s: WireSealed) -> Sealed {
    match s {
        WireSealed::None => Sealed::None,
        WireSealed::PubApi => Sealed::PubApi,
        WireSealed::Full => Sealed::Full,
    }
}

fn lower_auto_fact(af: &WireAutoFact) -> AutoFact {
    let trait_ = match af.trait_ {
        WireAutoTrait::Send => AutoTrait::Send,
        WireAutoTrait::Sync => AutoTrait::Sync,
        WireAutoTrait::Unpin => AutoTrait::Unpin,
        WireAutoTrait::UnwindSafe => AutoTrait::UnwindSafe,
        WireAutoTrait::RefUnwindSafe => AutoTrait::RefUnwindSafe,
    };
    let state = match af.state {
        WireAutoState::Yes => AutoState::Yes,
        WireAutoState::No => AutoState::No,
        WireAutoState::Cond => AutoState::Cond,
    };
    AutoFact { trait_, state }
}

fn lower_generics(generics: &[GenericParamWire]) -> Box<[GenericParam]> {
    generics.iter().map(lower_generic_param).collect()
}

fn lower_generic_param(gp: &GenericParamWire) -> GenericParam {
    match gp {
        GenericParamWire::Lifetime { name } => GenericParam::Lifetime { name: name.clone() },
        GenericParamWire::Type {
            name,
            bounds,
            default,
        } => GenericParam::Type {
            name: name.clone(),
            bounds: bounds.iter().map(lower_type_ref).collect(),
            default: default.as_ref().map(lower_type_wire),
            variance: None,
        },
        GenericParamWire::Const {
            name,
            ty,
            default: _,
        } => GenericParam::Const {
            name: name.clone(),
            ty: lower_type_ref(ty),
        },
    }
}

fn lower_wheres(wheres: &[WherePredWire]) -> Box<[WherePred]> {
    wheres.iter().map(lower_where_pred).collect()
}

fn lower_where_pred(wp: &WherePredWire) -> WherePred {
    WherePred {
        target: lower_type_wire(&wp.target),
        bounds: wp.bounds.iter().map(lower_type_ref).collect(),
    }
}

// ---------------------------------------------------------------------------
// Type lowerings
// ---------------------------------------------------------------------------

/// Lower a `TypeRefWire` to a `Type`.
///
/// `TypeRefWire::Same(id)` becomes `Type::Nominal(RawRef::Intro(id))`.
/// `TypeRefWire::Foreign(sr)` becomes `Type::Nominal(RawRef::Foreign(sr))`.
pub fn lower_type_ref(tr: &TypeRefWire) -> Type {
    match tr {
        TypeRefWire::Same(id) => Type::Nominal(RawRef::Intro(*id)),
        // `target` is ALWAYS `Some` here, and that is what makes the `display`
        // value below acceptable.
        //
        // A `TypeRefWire::Foreign` carries a `StableRef` — package + `IntroId`
        // — and nothing else. There is no name anywhere in the wire form to
        // put in `ForeignKey::display`, whose contract is "what to render when
        // the reference is *not linked*" (`Clone`, `List`, `Mutex`). We pass
        // the intro hex because it is the only identifier that exists at this
        // point, and it is never rendered: this reference arrives already
        // resolved, so consumers take `target` and never fall back to
        // `display`.
        //
        // If a future change can produce `target: None` on this path, the hex
        // WILL reach a reader as a symbol name. Fix the wire format to carry a
        // display name before allowing that — do not leave this as-is.
        TypeRefWire::Foreign(sr) => Type::Nominal(RawRef::Foreign {
            key: Arc::new(ForeignKey::in_package(
                sr.package.clone(),
                sr.intro.to_hex(),
                sr.intro.to_hex(),
            )),
            target: Some(sr.clone()),
        }),
    }
}

/// Lower a `TypeWire` to a `Type`.
pub fn lower_type_wire(tw: &TypeWire) -> Type {
    match tw {
        TypeWire::SelfType => Type::SelfType,
        TypeWire::Primitive(p) => Type::Primitive(lower_primitive(p)),
        TypeWire::Tuple(refs) => Type::Tuple(
            refs.iter()
                .map(|r| TupleElement::Positional(lower_type_ref(r)))
                .collect(),
        ),
        TypeWire::Slice(r) => Type::Slice(Box::new(lower_type_ref(r))),
        TypeWire::Array { ty, length } => Type::Array {
            ty: Box::new(lower_type_ref(ty)),
            length: *length as usize,
        },
        TypeWire::Union(refs) => Type::Union(refs.iter().map(lower_type_ref).collect()),
        TypeWire::Intersection(refs) => {
            Type::Intersection(refs.iter().map(lower_type_ref).collect())
        }
        TypeWire::Never => Type::Never,
        TypeWire::Any => Type::Any,
    }
}

fn lower_primitive(pw: &PrimitiveWire) -> Primitive {
    match pw {
        PrimitiveWire::Integer { signed, width } => Primitive::Integer {
            signed: *signed,
            width: lower_width(width.clone()),
        },
        PrimitiveWire::Float(w) => Primitive::Float(lower_width(w.clone())),
        PrimitiveWire::Bool => Primitive::Bool,
        PrimitiveWire::Char => Primitive::Char,
        PrimitiveWire::Str => Primitive::Str,
        PrimitiveWire::MutPointer(r) => Primitive::MutPointer(Box::new(lower_type_ref(r))),
        PrimitiveWire::ConstPointer(r) => Primitive::ConstPointer(Box::new(lower_type_ref(r))),
        PrimitiveWire::Reference {
            lifetime,
            mutable,
            ty,
        } => Primitive::Reference {
            lifetime: lifetime.clone(),
            mutable: *mutable,
            ty: Box::new(lower_type_ref(ty)),
        },
        PrimitiveWire::Builtin(s) => Primitive::Builtin(s.clone()),
    }
}

fn lower_width(w: crate::wire::WidthWire) -> Width {
    match w {
        crate::wire::WidthWire::Arch => Width::Arch,
        crate::wire::WidthWire::Fixed(bits) => {
            // `bits` is a u32, but `Width::Fixed` takes `NonZeroU16`.
            // In practice all bit widths in use (8, 16, 32, 64, 80, 128) fit in u16.
            // A width of 0 would be malformed; map to Width::Arch as a safe fallback.
            NonZeroU16::new(bits as u16)
                .map_or(Width::Arch, Width::Fixed)
        }
    }
}

/// Build an [`IrView`] for a generation whose package lineage is not needed.
///
/// The continuity matcher compares two generations of the *same* package, so
/// the lineage is constant across both sides and never participates in a
/// score. [`build_ir_view`] is the right entry point when a real lineage is at
/// hand (archive open, checkout); this one exists for the recording session,
/// which matches staged-against-tip and has no lineage in scope.
pub fn build_ir_view_unnamed(
    entries: impl IntoIterator<Item = (IntroId, OwnedEntryPayload, Option<IntroId>)>,
    bodies: &std::collections::BTreeMap<IntroId, BodyEmbed>,
) -> IrView {
    let mut table = PristineIntroTable::new();
    for (id, payload, parent) in entries {
        table.insert_live(id, lower_payload(&payload), parent);
    }
    let mut view = IrView::new(table);
    for (id, body) in bodies {
        view.set_body(*id, body.clone());
    }
    view
}

/// Project a [`Substitution`] into the VCS-layer [`ContinuitySummary`].
///
/// The matcher answers "which declaration is which"; the change log needs
/// "what visibly happened". This derives the second from the first by
/// comparing the two generations' symbols, so a `Continuation` becomes a
/// `Renamed`/`Moved` op only when the name or parent actually differs — a
/// stable entry produces no op at all.
pub fn summarize_continuity(
    subst: &ir::continuity::Substitution,
    prev: &IrView,
    next: &IrView,
) -> crate::f1::ContinuitySummary {
    use crate::f1::{ContinuityOp, ContinuitySummary, RenameEdge};
    use ir::continuity::Assignment;

    let mut ops = Vec::new();

    for a in &subst.assignments {
        match a {
            Assignment::Continuation {
                next_id, prior_id, ..
            } => {
                let (Some(pe), Some(ne)) = (prev.entry(*prior_id), next.entry(*next_id)) else {
                    continue;
                };
                if pe.sym().name != ne.sym().name {
                    ops.push(ContinuityOp::Renamed {
                        id: *prior_id,
                        old_name: pe.sym().name.clone(),
                        new_name: ne.sym().name.clone(),
                    });
                }
                let (op, np) = (prev.parent_of(*prior_id), next.parent_of(*next_id));
                // Compare parents through σ: the next parent is a wire id until
                // the cascade rewrites it, so a stable child under a renamed
                // parent must not read as a move.
                let np_durable = np.map(|p| subst.sigma().get(&p).copied().unwrap_or(p));
                if op != np_durable {
                    ops.push(ContinuityOp::Moved {
                        id: *prior_id,
                        old_parent: op,
                        new_parent: np_durable,
                    });
                }
            }
            Assignment::New { next_id, .. } => {
                ops.push(ContinuityOp::Introduced { id: *next_id });
            }
        }
    }

    for d in &subst.deletions {
        ops.push(ContinuityOp::Deleted { id: d.prior_id });
    }

    let rename_edges = subst
        .rename_edges
        .iter()
        .map(|e| RenameEdge {
            deleted_id: e.prior_id,
            added_id: e.next_id,
            score: e.score,
        })
        .collect();

    ContinuitySummary { ops, rename_edges }
}
