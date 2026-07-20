//! `packages` — stem-level package identity (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::ids::PackageStemId;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "packages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub stem_id: PackageStemId,
    pub ecosystem: String,
    pub name_struct: String,
    pub name_canonical: String,
    pub name_original: String,
    pub repo_url: Option<String>,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::versions::Entity")]
    Versions,
}

impl Related<super::versions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Versions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type PackageRow = Model;

pub const TABLE: &str = "packages";

