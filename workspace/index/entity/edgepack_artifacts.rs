//! `edgepack_artifacts` — built edgepack bundles (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::ids::EdgepackKeyDigest;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "edgepack_artifacts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub edgepack_key_digest: EdgepackKeyDigest,
    pub version_id: Uuid,
    pub recipe_fingerprint: String,
    pub artifact_id: Option<Vec<u8>>,
    pub ram_estimate: Option<i64>,
    pub published_at: Option<i64>,
    pub status: String,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type EdgepackArtifactRow = Model;

pub const TABLE: &str = "edgepack_artifacts";

