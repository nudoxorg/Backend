//! `package_aliases` — name-alias mappings (REGISTRYLESS-PLAN §5).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::AliasConfidence;
use crate::ids::PackageStemId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "package_aliases")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub ecosystem: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub alias_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub alias: String,
    pub stem_id: PackageStemId,
    pub confidence: AliasConfidence,
    pub recorded_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type PackageAliasRow = Model;

pub const TABLE: &str = "package_aliases";
