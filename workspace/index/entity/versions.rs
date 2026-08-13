//! `versions` — one row per (stem, version) (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::ids::ObjectPackHash;
use crate::ids::PackageStemId;
use crate::enums::ParseState;
use crate::enums::SourceKind;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "versions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub stem_id: PackageStemId,
    pub version_canonical: String,
    pub version_original: String,
    pub published_at: Option<i64>,
    pub toolchain: Option<String>,
    pub license_spdx: Option<String>,
    pub yanked_upstream: bool,
    pub parse_state: ParseState,
    pub parse_phase: Option<String>,
    pub attempts: i64,
    pub failure: Option<String>,
    pub source_kind: SourceKind,
    pub source_pack: Option<ObjectPackHash>,
    pub source_rev: Option<String>,
    pub registry_checksum: Option<String>,
    pub registry_package_uri: Option<String>,
    /// `ETag` observed on the most recent successful (non-304) fetch of this
    /// version's upstream source archive. `NULL` until the first fetch, or
    /// if the origin never sent one. Conditional-GET politeness (W4b): sent
    /// back as `If-None-Match` on the next fetch so an unchanged archive
    /// costs the origin a 304 instead of a full re-download. See
    /// `store::lifecycle::{get_archive_cache_meta, set_archive_cache_meta}`.
    pub archive_etag: Option<String>,
    /// `Last-Modified` observed on the most recent successful fetch of this
    /// version's upstream source archive, verbatim (RFC 7231 HTTP-date) so it
    /// can be replayed unchanged as `If-Modified-Since`. `NULL` until the
    /// first fetch, or if the origin never sent one.
    pub archive_last_modified: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::packages::Entity",
        from = "Column::StemId",
        to = "super::packages::Column::StemId"
    )]
    Package,
    #[sea_orm(has_many = "super::generations::Entity")]
    Generations,
    #[sea_orm(has_many = "super::edges::Entity")]
    Edges,
    #[sea_orm(has_one = "super::facets::Entity")]
    Facets,
}

impl Related<super::packages::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Package.def()
    }
}

impl Related<super::generations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Generations.def()
    }
}

impl Related<super::edges::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Edges.def()
    }
}

impl Related<super::facets::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Facets.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type VersionRow = Model;

pub const TABLE: &str = "versions";

