//! `generation_locations` — store-presence for IR generations (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::LocationStatus;
use crate::ids::GenerationStamp;
use crate::ids::StoreId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "generation_locations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub gen_stamp: GenerationStamp,
    #[sea_orm(primary_key, auto_increment = false)]
    pub store_id: StoreId,
    pub status: LocationStatus,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type GenerationLocationRow = Model;

pub const TABLE: &str = "generation_locations";
