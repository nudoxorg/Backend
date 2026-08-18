//! `advisories` — security advisory records (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::ids::AdvisoryId;
use crate::ids::PackageStemId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "advisories")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: AdvisoryId,
    pub stem_id: Option<PackageStemId>,
    pub version_range: Option<String>,
    pub severity: Option<String>,
    pub summary: Option<String>,
    pub url: Option<String>,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub recorded_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type AdvisoryRow = Model;

pub const TABLE: &str = "advisories";
