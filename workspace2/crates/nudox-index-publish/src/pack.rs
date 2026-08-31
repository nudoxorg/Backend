//! Canonical physical packs and durable local content-addressed storage for index snapshots.

mod encode;
mod error;
mod grammar;
mod store;
mod view;

pub(crate) use encode::MAX_PACK_SEGMENTS;
pub use encode::{IndexPackPlan, IndexPackPlanFacts, encode_index_pack, plan_index_pack};
pub use error::{
    IndexPackCleanup, IndexPackConflict, IndexPackEncodeError, IndexPackLane, IndexPackOpenError,
    IndexPackPathRole, IndexPackRegion, IndexPackRowInvariant, IndexPackStoreError,
    IndexPackStorePhase, RejectedIndexPack, StoredIndexPack,
};
pub use store::IndexPackStore;
pub use view::{
    ExactPackRow, ExactPackSegment, ExactPackValue, IndexPack, IndexPackFacts, IndexPackView,
    LexicalPackRow, LexicalPackSegment, LexicalPackValue,
};
