//! `feed_watermarks` — per-feed crawl cursor (REGISTRYLESS-PLAN §5).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "feed_watermarks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub feed: String,
    pub last_ref: Option<String>,
    pub last_checked_at: i64,
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type FeedWatermarkRow = Model;

pub const TABLE: &str = "feed_watermarks";

