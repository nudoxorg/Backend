//! `edges` — dependency graph edges (INDEX-PLAN §8, REGISTRYLESS RL-5).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::enums::EdgeKind;
use crate::enums::EdgeSource;
use crate::ids::PackageStemId;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "edges")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub dependent_version: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub dep_ecosystem: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub dep_name_canonical: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: EdgeKind,
    pub requirement: String,
    pub resolved_stem: Option<PackageStemId>,
    pub source: EdgeSource,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::versions::Entity",
        from = "Column::DependentVersion",
        to = "super::versions::Column::Id"
    )]
    Version,
}

impl Related<super::versions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Version.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type EdgeRow = Model;

pub const TABLE: &str = "edges";

