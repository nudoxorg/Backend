//! `outbox` — projection fan-out (INDEX-PLAN §8, ID-3).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::OutboxOperation;
use crate::enums::SinkKind;
use crate::ids::GenerationStamp;
use sea_orm::entity::prelude::*;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "outbox")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    pub version_id: Option<Uuid>,
    pub gen_stamp: Option<GenerationStamp>,
    pub sink_kind: SinkKind,
    pub op: OutboxOperation,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type OutboxRow = Model;

pub const TABLE: &str = "outbox";
