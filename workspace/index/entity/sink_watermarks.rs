//! `sink_watermarks` — per-sink outbox consume cursor (INDEX-PLAN ID-3).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::SinkKind;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sink_watermarks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub sink_kind: SinkKind,
    pub last_seq: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type SinkWatermarkRow = Model;

pub const TABLE: &str = "sink_watermarks";
