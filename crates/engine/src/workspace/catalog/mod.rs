//! Store-backed derived-output catalog bound to the workspace head.
//!
//! The catalog is three version-layer `PersistentTree` relations. The
//! primary relation is keyed by the complete semantic/authority identity; the
//! freshness relation is keyed by generation for deterministic bounded
//! eviction; the payload relation reference-counts immutable output objects.
//! Relation nodes are persisted through the store's shared node CAS and only
//! a fixed descriptor plus live payloads are added to the workspace closure.
//! There is no second catalog CAS, log, or head.

mod proof;
mod publication;
mod query;
pub(crate) mod relation;
mod state;
mod storage;
mod work;

use backend_version::{Schema, SchemaIdentity};

pub use proof::{DerivedOutputEntry, DerivedOutputPublication};
pub(crate) use proof::{DerivedOutputProof, StagedDerivedOutput};
pub(crate) use publication::append_to_closure;
pub(crate) use query::{LatestQuery, find_latest_in_state, find_proof_in_state};
pub(crate) use relation::CatalogState;
pub(crate) use relation::{
    FreshnessRelation as CatalogFreshnessRelation, PayloadRefRelation as CatalogPayloadRefRelation,
    PrimaryRelation as CatalogPrimaryRelation,
};
pub(crate) use state::{catalog_gc_roots, state_from_durable_manifest};
pub(super) use work::CatalogWork;

pub(super) const CATALOG_DOMAIN: u8 = 0x86;
// Version two changes the primary-key order so the newest generation for an
// exact semantic identity is the first authenticated row.  A new schema
// identity deliberately prevents a v1 flat index from being interpreted as
// the descending-generation format after restart.
pub(super) const CATALOG_VERSION: u8 = 3;
pub(super) const BYTES_SCHEMA_TYPE: u16 = 7;
pub(super) const MANIFEST_SCHEMA_TYPE: u16 = 8;
pub(super) const INDEX_SCHEMA_TYPE: u16 = 9;
pub(super) const PRIMARY_KEY_BYTES: usize = 192;
pub(super) const FRESHNESS_KEY_BYTES: usize = 200;
/// Bytes before the inverted generation suffix in a primary key.
pub(super) const SEMANTIC_KEY_PREFIX_BYTES: usize = PRIMARY_KEY_BYTES - 8;
pub(super) const MAX_CATALOG_ENTRIES: usize = 1_024;

/// Typed schema for one canonical output byte object.
#[derive(Debug)]
pub(crate) struct DerivedOutputBytesSchema;

impl Schema for DerivedOutputBytesSchema {
    const DOMAIN: u8 = CATALOG_DOMAIN;
    const TYPE: u16 = BYTES_SCHEMA_TYPE;
    const VERSION: u8 = CATALOG_VERSION;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Typed schema for one canonical dependency-manifest object.
#[derive(Debug)]
pub(crate) struct DerivedOutputManifestSchema;

impl Schema for DerivedOutputManifestSchema {
    const DOMAIN: u8 = CATALOG_DOMAIN;
    const TYPE: u16 = MANIFEST_SCHEMA_TYPE;
    const VERSION: u8 = CATALOG_VERSION;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Typed schema for the fixed catalog descriptor object.
#[derive(Debug)]
pub(crate) struct DerivedOutputIndexSchema;

impl Schema for DerivedOutputIndexSchema {
    const DOMAIN: u8 = CATALOG_DOMAIN;
    const TYPE: u16 = INDEX_SCHEMA_TYPE;
    const VERSION: u8 = CATALOG_VERSION;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

pub(super) fn catalog_schema(ty: u16) -> SchemaIdentity {
    SchemaIdentity::new(CATALOG_DOMAIN, ty, CATALOG_VERSION)
}

impl From<backend_store::StoreError> for super::owner::WorkspaceError {
    fn from(error: backend_store::StoreError) -> Self {
        super::owner::WorkspaceError::store(error)
    }
}
