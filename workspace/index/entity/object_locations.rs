//! `object_locations` — store-presence for ObjectPacks (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::enums::LocationStatus;
use crate::ids::ObjectPackHash;
use crate::ids::StoreId;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "object_locations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub object_id: ObjectPackHash,
    #[sea_orm(primary_key, auto_increment = false)]
    pub store_id: StoreId,
    pub status: LocationStatus,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type ObjectLocationRow = Model;

pub const TABLE: &str = "object_locations";

