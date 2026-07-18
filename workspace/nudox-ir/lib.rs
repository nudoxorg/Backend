//! # nudox-ir — the IR data model
//!
//! The durable IR unit is a per-package set of [`entry::Entry`] values
//! (`Symbol` + `Node` + kind body), addressed *within a generation* by typed
//! [`index::EntryIdx`] handles and *across generations / on the wire* by
//! [`nudox_change::IntroId`] / [`nudox_change::StableRef`].
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

pub mod apply;
pub mod builder;
pub mod entry;
pub mod index;
pub mod intro;
pub mod kind;
pub mod registry;
pub mod skeleton;
pub mod symbol;
pub mod wire;

#[cfg(test)]
mod tests;

pub use apply::{LinkRecord, PristineIntroTable};
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
