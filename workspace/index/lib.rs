//! `index` — the versioned catalog crate (INDEX-PLAN phase IP-1).
//!
//! Two stores back the catalog:
//! - **Sovereign** `catalog.dolt` via the rusqdoltlite engine — packages,
//!   versions, generations, locations, edges, repo facts, advisories,
//!   git-monitor watermarks, outbox, overlays (schema v4, INDEX-PLAN §8, plus
//!   the three registryless tables of REGISTRYLESS-PLAN §5). All engine access
//!   funnels through the [`engine`] facade (ID-20).
//! - **Ephemeral** `scratch.sqlite` via rusqlite — jobs, wanted, sessions
//!   (writer-sticky), claims. Delete-anytime semantics; never on remotes
//!   ([`scratch`], ID-2).
//!
//! Writes flow through a single [`store::writer::CatalogWriter`] on `main`
//! (ID-1); the outbox is the only projection fan-out and is written in the same
//! transaction as the business row (ID-3); batch heartbeat commits (ID-4) are
//! explicit [`store::writer::CatalogWriter::commit_batch`] calls. Reads go
//! through the [`store::Catalog`] trait, including bitemporal
//! [`store::Catalog::at`] views resolved over the commit graph.
//!
//! Migrations are sea-query-built DDL run behind a `pre-migrate-vN` snapshot
//! branch, with rollback = checkout ([`migrations`], §13).

pub mod codec;
pub mod engine;
pub mod enums;
pub mod ids;
pub mod migrations;
pub mod overlays;
pub mod protocol;
pub mod resolution;
pub mod scratch;
pub mod seed_models;
pub mod store;
pub mod tables;

/// Schema version this crate authors and reads (INDEX-PLAN ID-5).
pub const SCHEMA_VERSION: u32 = 4;

pub use codec::CodecError;
pub use enums::{
    AliasConfidence, CompileCacheKind, EdgeKind, EdgeSource, IrStatus, LineageEvidence,
    LineageRelation, ListingStatus, LocationStatus, OutboxOperation, ParseState, SinkKind,
    SourceKind, StoreKind, TextEnum, TextEnumError,
};
pub use ids::{
    AdvisoryId, ChannelTip, EdgepackKeyDigest, GenerationStamp, IdDecodeError, IntroIdHash,
    JobKeyHash, ObjectPackHash, PackageId, PackageStemId, StoreId,
};
pub use protocol::CatalogOp;
pub use store::{Catalog, MetaStore};
