//! [`IrOp`] — the typed structural-delta operation over one [`IntroId`].
//!
//! Every operation represents a semantic change observed at diff time between
//! two [`nudox_ir::apply::PristineIntroTable`] generations. Operations are
//! **matcher-free**: continuity (rename/move/sigkey) is re-derived from the two
//! payloads at diff time; the IDs have already been stabilized by the recording
//! session.
//!
//! # Ordering discipline (§7.3)
//!
//! Within a single `IntroId`'s op list, ops are sorted:
//! `lifecycle < continuity < meta < kind-specific < links`
//!
//! This is the canonical render order used by both [`crate::diff::diff_tables`]
//! and [`crate::delta::PackageDelta::canonical_bytes`].

use nudox_change::{ContentBlake3, IntroId, StableRef};
use nudox_change::domain::LinkDomainKey;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use nudox_ir::wire::{
    AttrTok, AutoTrait, CfgExpr, FnSigFlags, TraitFlags, TriState, TypeRefWire, WherePredWire,
};
use nudox_ir::symbol::Visibility;

// ---------------------------------------------------------------------------
// SigKey
// ---------------------------------------------------------------------------

/// Content-addressed key for a function's full signature skeleton (§4.6).
///
/// `SigKey = blake3("nudox.sigkey.v1" || function_signature_skeleton(inputs, outputs) || 0xFE || fnsig_flag_bytes)`
///
/// Computed by [`nudox_ir::intro::sig_key`] and never stored in the VCS file
/// (it is always re-derived). Carried inside [`IrOp::SignatureEvolved`] so that
/// downstream consumers can check for overload-set churn without re-deriving.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
#[repr(transparent)]
pub struct SigKey(pub ContentBlake3);

impl SigKey {
    /// Wrap a raw [`ContentBlake3`] as a `SigKey`.
    #[inline]
    pub const fn from_content_blake3(inner: ContentBlake3) -> Self {
        Self(inner)
    }

    #[inline]
    pub const fn as_content_blake3(&self) -> ContentBlake3 {
        self.0
    }

    #[inline]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

// ---------------------------------------------------------------------------
// GenericsDelta
// ---------------------------------------------------------------------------

/// Fine-grained delta for `gparam` / `where` changes on any generic item.
///
/// Used inside [`IrOp::GenericsChanged`]; intentionally richer than a simple
/// before/after diff so downstream Pack B lints can classify without re-deriving.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GenericsDelta {
    /// Names of newly added generic parameters.
    pub added: Vec<SmolStr>,
    /// Names of removed generic parameters.
    pub removed: Vec<SmolStr>,
    /// Names of parameters whose bound set grew (tightened — potentially breaking).
    pub bounds_tightened: Vec<SmolStr>,
    /// Names of parameters whose bound set shrank (loosened — potentially minor).
    pub bounds_loosened: Vec<SmolStr>,
    /// Names of parameters that gained a default value.
    pub default_added: Vec<SmolStr>,
    /// Names of parameters that lost a default value.
    pub default_removed: Vec<SmolStr>,
}

// ---------------------------------------------------------------------------
// WherePred (local alias for the spec's `WherePred`)
// ---------------------------------------------------------------------------

/// A single where-predicate carried in an [`IrOp`].
///
/// This is a re-export of [`WherePredWire`] from `nudox-ir`; the spec names it
/// `WherePred` in the op signatures, so we alias it here for concision.
pub type WherePred = WherePredWire;

// ---------------------------------------------------------------------------
// IrOp
// ---------------------------------------------------------------------------

/// One typed structural-delta operation on a single [`IntroId`] (§7.2).
///
/// Variants are grouped into six categories that define the canonical sort order
/// within an id's op list (§7.3):
///
/// 1. **Lifecycle** — introduced, deleted, resurrected
/// 2. **Continuity** — rename, move, signature evolution
/// 3. **Meta** — visibility, docs, deprecation, aliases, span, cfg, attrs
/// 4. **Kind-specific** — functions, records/enums, traits/impls, consts/aliases, reexports
/// 5. **Links** — graph-tier edge changes
/// 6. **Probes** — auto-trait facts
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum IrOp {
    // -----------------------------------------------------------------------
    // 1. Lifecycle
    // -----------------------------------------------------------------------

    /// The entry is new in T1 (no corresponding entry in T0).
    Introduced,

    /// The entry was in T0 and is absent in T1.
    Deleted,

    /// The entry was deleted in some earlier generation and has re-appeared with
    /// the same `IntroId` (bootstrap determinism). Semantically an addition for
    /// semver; historically a resurrect for lineage queries.
    ///
    /// Tagging requires symbol_history access (available in the live recording
    /// path). Historical replays may omit this and emit [`IrOp::Introduced`]
    /// instead.
    Resurrected,

    // -----------------------------------------------------------------------
    // 2. Continuity (re-derived from payload comparison at diff time)
    // -----------------------------------------------------------------------

    /// The symbol's canonical name changed between T0 and T1.
    Renamed {
        old: SmolStr,
        new: SmolStr,
    },

    /// The symbol's parent entry changed between T0 and T1 (module move).
    Moved {
        old_parent: Option<IntroId>,
        new_parent: Option<IntroId>,
    },

    /// The function's [`SigKey`] changed (param types / flags).
    ///
    /// Emitted only for `Function` entries when the sig key differs.
    SignatureEvolved {
        old: SigKey,
        new: SigKey,
    },

    // -----------------------------------------------------------------------
    // 3. Meta
    // -----------------------------------------------------------------------

    /// The symbol's visibility level changed.
    VisChanged {
        old: Visibility,
        new: Visibility,
    },

    /// The documentation text changed. Signals re-embedding; does not imply an
    /// API surface change.
    DocChanged,

    /// The deprecation notice was added (`added = true`) or removed (`added = false`).
    DeprecationChanged {
        added: bool,
    },

    /// Aliases were added or removed.
    AliasesChanged {
        added: Vec<SmolStr>,
        removed: Vec<SmolStr>,
    },

    /// The source span (`span_start` / `span_end`) changed.
    SpanChanged,

    /// The `source_path` string changed.
    SourcePathChanged,

    /// The cfg predicate changed (including being added or removed).
    CfgChanged {
        old: Option<CfgExpr>,
        new: Option<CfgExpr>,
    },

    /// Normalized attributes were added or removed.
    AttrsChanged {
        added: Vec<AttrTok>,
        removed: Vec<AttrTok>,
    },

    // -----------------------------------------------------------------------
    // 4a. Kind-specific: functions
    // -----------------------------------------------------------------------

    /// An input parameter was added at position `index` (0-based).
    ParamAdded {
        index: u16,
    },

    /// An input parameter was removed from position `index` (0-based).
    ParamRemoved {
        index: u16,
    },

    /// An input parameter at `index` was renamed (same position, different name).
    ParamRenamed {
        index: u16,
    },

    /// An input parameter at `index` changed type.
    ParamTypeChanged {
        index: u16,
        old: TypeRefWire,
        new: TypeRefWire,
    },

    /// The input parameter sequence has the same multiset of (name, type)
    /// elements but in a different order.
    ParamsReordered,

    /// The output parameter list changed.
    ReturnChanged {
        old: Vec<TypeRefWire>,
        new: Vec<TypeRefWire>,
    },

    /// The function's [`FnSigFlags`] changed (async/const/unsafe/abi/variadic/self-kind).
    FnSigFlagsChanged {
        old: FnSigFlags,
        new: FnSigFlags,
    },

    /// The generic parameter list changed (detailed breakdown).
    GenericsChanged {
        detail: GenericsDelta,
    },

    /// The where-clause set changed.
    WhereChanged {
        added: Vec<WherePred>,
        removed: Vec<WherePred>,
    },

    // -----------------------------------------------------------------------
    // 4b. Kind-specific: records / enums
    // -----------------------------------------------------------------------

    /// A field's type changed (applies to `Field` entries).
    FieldTypeChanged {
        old: Option<TypeRefWire>,
        new: Option<TypeRefWire>,
    },

    /// The record's form changed (e.g. `Struct` → `Tuple`).
    RecFormChanged,

    /// The `recfield` list has the same multiset but a different order.
    FieldsReordered,

    /// A child entry (field or variant) was added to the parent record/enum.
    ChildAdded {
        child: IntroId,
    },

    /// A child entry (field or variant) was removed from the parent record/enum.
    ChildRemoved {
        child: IntroId,
    },

    /// A variant's form changed (e.g. `Unit` → `Struct`).
    VariantFormChanged,

    /// A variant's explicit discriminant value changed.
    VariantDiscrChanged,

    // -----------------------------------------------------------------------
    // 4c. Kind-specific: traits / impls
    // -----------------------------------------------------------------------

    /// The trait's supertraits changed.
    SupertraitsChanged {
        added: Vec<TypeRefWire>,
        removed: Vec<TypeRefWire>,
    },

    /// The trait's flags changed (auto / unsafe / dyn_compat / sealed).
    TraitFlagsChanged {
        old: TraitFlags,
        new: TraitFlags,
    },

    /// An impl's header changed (`of` trait or `self_ty` or flags). The full
    /// old/new is recoverable from the table; this op signals re-classification.
    ImplHeaderChanged,

    // -----------------------------------------------------------------------
    // 4d. Kind-specific: consts / statics / type aliases
    // -----------------------------------------------------------------------

    /// A const or static's type changed.
    ConstTypeChanged,

    /// A const's rendered value changed.
    ConstValueChanged,

    /// A type alias's type expression changed.
    TypeExprChanged,

    // -----------------------------------------------------------------------
    // 4e. Kind-specific: reexports
    // -----------------------------------------------------------------------

    /// A re-export's target was changed to a different stable reference.
    ReexportRetargeted {
        old: StableRef,
        new: StableRef,
    },

    // -----------------------------------------------------------------------
    // 5. Links (graph tier)
    // -----------------------------------------------------------------------

    /// A graph-tier link was added.
    LinkAdded {
        key: LinkDomainKey,
    },

    /// A graph-tier link was removed.
    LinkRemoved {
        key: LinkDomainKey,
    },

    // -----------------------------------------------------------------------
    // 6. Probes
    // -----------------------------------------------------------------------

    /// Auto-trait fact(s) changed.
    ///
    /// Each tuple is `(trait, old_state, new_state)`. Uses [`TriState`]
    /// (`Yes`/`No`/`Unknown`) rather than `AutoState` (`Yes`/`No`/`Cond`) to
    /// be forward-compatible with v2 phased facts (§8.7).
    AutoTraitsChanged {
        changed: Vec<(AutoTrait, TriState, TriState)>,
    },
}

/// Canonical sort order among [`IrOp`] variants within one `IntroId`'s list.
///
/// Lower = emitted first. Used by [`crate::diff`] when building the op list
/// and by [`crate::delta::PackageDelta::canonical_bytes`].
pub fn op_sort_key(op: &IrOp) -> u8 {
    match op {
        // lifecycle
        IrOp::Introduced | IrOp::Deleted | IrOp::Resurrected => 0,
        // continuity
        IrOp::Renamed { .. } | IrOp::Moved { .. } | IrOp::SignatureEvolved { .. } => 1,
        // meta
        IrOp::VisChanged { .. }
        | IrOp::DocChanged
        | IrOp::DeprecationChanged { .. }
        | IrOp::AliasesChanged { .. }
        | IrOp::SpanChanged
        | IrOp::SourcePathChanged
        | IrOp::CfgChanged { .. }
        | IrOp::AttrsChanged { .. } => 2,
        // kind-specific
        IrOp::ParamAdded { .. }
        | IrOp::ParamRemoved { .. }
        | IrOp::ParamRenamed { .. }
        | IrOp::ParamTypeChanged { .. }
        | IrOp::ParamsReordered
        | IrOp::ReturnChanged { .. }
        | IrOp::FnSigFlagsChanged { .. }
        | IrOp::GenericsChanged { .. }
        | IrOp::WhereChanged { .. }
        | IrOp::FieldTypeChanged { .. }
        | IrOp::RecFormChanged
        | IrOp::FieldsReordered
        | IrOp::ChildAdded { .. }
        | IrOp::ChildRemoved { .. }
        | IrOp::VariantFormChanged
        | IrOp::VariantDiscrChanged
        | IrOp::SupertraitsChanged { .. }
        | IrOp::TraitFlagsChanged { .. }
        | IrOp::ImplHeaderChanged
        | IrOp::ConstTypeChanged
        | IrOp::ConstValueChanged
        | IrOp::TypeExprChanged
        | IrOp::ReexportRetargeted { .. } => 3,
        // links
        IrOp::LinkAdded { .. } | IrOp::LinkRemoved { .. } => 4,
        // probes
        IrOp::AutoTraitsChanged { .. } => 5,
    }
}
