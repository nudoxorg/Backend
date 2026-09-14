//! Defines pack behavior for `backend-engine index_publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical physical packs and durable local content-addressed storage for index snapshots.

mod encode;
mod error;
mod grammar;
mod store;
mod view;

pub(crate) use self::encode::MAX_PACK_SEGMENTS;
pub use self::encode::{IndexPackPlan, IndexPackPlanFacts, encode_index_pack, plan_index_pack};
pub use self::error::{
    IndexPackCleanup, IndexPackConflict, IndexPackEncodeError, IndexPackLane, IndexPackOpenError,
    IndexPackPathRole, IndexPackRegion, IndexPackRowInvariant, IndexPackStoreError,
    IndexPackStorePhase, RejectedIndexPack, StoredIndexPack,
};
pub use self::store::IndexPackStore;
pub use self::view::{
    ExactPackRow, ExactPackSegment, ExactPackValue, IndexPack, IndexPackFacts, IndexPackView,
    LexicalPackRow, LexicalPackSegment, LexicalPackValue,
};
