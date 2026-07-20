//! `compile_cache` — producer job-key → result cache (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::enums::CompileCacheKind;
use crate::ids::GenerationStamp;
use crate::ids::JobKeyHash;
use crate::ids::ObjectPackHash;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "compile_cache")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub job_key: JobKeyHash,
    pub kind: CompileCacheKind,
    pub gen_stamp: Option<GenerationStamp>,
    pub object_id: Option<ObjectPackHash>,
    pub image_digest: Option<String>,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type CompileCacheRow = Model;

pub const TABLE: &str = "compile_cache";

