//! `git_watermarks` — per-stem git polling watermarks (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::ids::PackageStemId;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "git_watermarks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub stem_id: PackageStemId,
    pub last_rev: Option<String>,
    pub last_checked_at: i64,
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type GitWatermarkRow = Model;

pub const TABLE: &str = "git_watermarks";

