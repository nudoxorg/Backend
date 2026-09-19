//! Shared typed identities, logical progress, weighted rows, and errors.
//!
//! The facade keeps the crate's public paths stable while each responsibility
//! lives in a focused module.

mod coverage;
mod errors;
mod row;
mod time;

pub(crate) type RawDigest = [u8; 32];

pub use backend_store::LayoutId;
pub use backend_version::DeltaId;
pub use backend_version::{Coverage, CoverageWitness, RelationState};

pub use coverage::{BoundFrontier, PreparedOutput};
pub use errors::FlowError;
pub use row::{
    ArrangementCanonicalRelation, ArrangementDeltaId, ArrangementKey, ArrangementRelation,
    ArrangementRoot, AuthorityIdentity, AuthoritySchema, CanonicalValue, Delta, DigestReference,
    EquivalenceIdentity, EquivalenceSchema, InputIdentity, InputSchema, ObjectIdentity,
    ObjectSchema, ReadIdentity, ReadSchema, RecipeIdentity, RecipeSchema, RelationIdentity,
    RelationSchema, RowKey, Weight,
};
pub use time::{Epoch, Frontier, Pin, Time, TraceSpine};

pub(crate) use coverage::arrangement_coverage;
