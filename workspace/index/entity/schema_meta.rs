//! `schema_meta` — migration version tracking (INDEX-PLAN ID-5).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "schema_meta")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type SchemaMetaRow = Model;

pub const TABLE: &str = "schema_meta";
