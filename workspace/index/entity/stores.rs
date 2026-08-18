//! `stores` — IR-VCS and ObjectPack store registrations (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::StoreKind;
use crate::ids::StoreId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "stores")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub store_id: StoreId,
    pub kind: StoreKind,
    pub endpoint: String,
    pub healthy: bool,
    pub added_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type StoreRow = Model;

pub const TABLE: &str = "stores";
