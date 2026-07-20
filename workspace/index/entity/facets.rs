//! `facets` — per-version quality metadata (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "facets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub version_id: Uuid,
    pub keywords: Option<String>,
    pub quality_ppm: Option<i64>,
    pub extras: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::versions::Entity",
        from = "Column::VersionId",
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
pub type FacetRow = Model;

pub const TABLE: &str = "facets";

