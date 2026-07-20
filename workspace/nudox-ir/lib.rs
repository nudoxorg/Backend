//! # nudox-ir — the IR data model
//!
//! The durable IR unit is a per-package set of [`entry::Entry`] values
//! (`Symbol` + `Node` + kind body), addressed *within a generation* by typed
//! [`index::EntryIdx`] handles and *across generations / on the wire* by
//! [`crate::change::IntroId`] / [`crate::change::StableRef`].
//!
//! This crate owns the **data model** only — the entry/symbol/kind types, the
//! wire twins, the deterministic [`intro`] bootstrap, the type-[`skeleton`]
//! encoder, the [`builder::EntryBuilder`] producer API, and the
//! [`apply::PristineIntroTable`] *container* that a `materialize` produces and
//! that `nudox-ir-archive` seals. It does **not** own change/patch semantics:
//! libpijul (via `nudox-ir-vcs`) is the change engine — changes, dependencies,
//! apply, and unrecord are its job, not ours.
//!
//! Spec context: `.research/ir-vcs/design/SEMANTIC-IR-VCS-PLAN.md` and brief
//! `06-new-ir-rewrite.md` (data model); the change-algebra sections are
//! superseded by the libpijul-backed VCS.

// ── Folded-in leaf planes (formerly sibling crates) ─────────────────────────
/// Content-addressed identity primitives (formerly the `nudox-change` crate):
/// domain-separated BLAKE3 hashes, `IntroId`/`ChangeId`/`StableRef`, encode.
pub mod change;
/// Generation manifest + `GenerationStamp` derivation (formerly the
/// `nudox-ir-manifest` crate): `BlobManifestV3`, outbox staging.
pub mod manifest;

pub mod apply;
pub mod body;
pub mod body_wire;
pub mod builder;
pub mod entry;
pub mod index;
pub mod intro;
pub mod kind;
pub mod reflect;
pub mod registry;
pub mod skeleton;
pub mod symbol;
pub mod view;
pub mod vocab;
pub mod wire;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "tests_body_merge.rs"]
mod tests_body_merge;

pub use apply::{LinkRecord, PristineIntroTable};
pub use body::{
    merge_body, overlapping_call, AccessMode, BodyCall, BodyEmbed, BodyFacts, BodyImport,
    BodyMergeNote, ConflictPolicy, ControlSketch, LocalBind, LocalKind, OracleAccess, OracleBody,
    OracleCall, OracleTypeMention, TreesitterBody,
};
pub use body_wire::{
    body_path, deserialize_body, serialize_body, BodyWireError, BODY_DOMAIN_V1,
    BODY_FILE_EXTENSION,
};
pub use reflect::{
    boundary, exported, monikers, CfgAssignment, DepIrProvider, DepMissing, ExportPolicy,
    MonikerPath,
};
pub use view::IrView;
pub use vocab::{Confidence, ReferenceKind, RelSpan};
pub use builder::{EntryBuilder, SymbolBuf};
pub use entry::{Entry, EntryInner, Node};
pub use index::{ArenaIdx, EntryIdx, EntryKind, LinkId, PackageIdx, RawEntryIdx, StrId, TypeFingerprintId};
pub use intro::{
    bootstrap_intro_id, bootstrap_intro_id_v2, sig_key, Disambiguator, DisambiguatorV2,
};
pub use kind::{Kind, KindDiscriminant};
pub use registry::{
    AsyncRegistryResolver, ProductionEntryId, Registry, RegistryResolver, RegistryState,
    ResolveError,
};
pub use skeleton::{fnsig_flag_bytes, trait_impl_skeleton};
pub use symbol::{ByteSpan, Deprecation, DocLink, Symbol, Visibility};
pub use wire::{
    AttrTok, AutoFact, AutoState, AutoTrait, CfgExpr, ConstWire, EnumWire, EntryPayloadFlags,
    FnSigFlags, GenericParamWire, ImplFlags, ImplWire, KindWire, OwnedEntryPayload, RecordForm,
    ReexportWire, Sealed, SelfKind, StaticWire, SymbolWire, TraitFlags, TraitWire, TriState,
    TypeAliasWire, VariantForm, VariantWire, WherePredWire,
};
