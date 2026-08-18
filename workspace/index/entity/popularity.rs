//! `popularity` — download/dependent percentiles (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::ids::PackageStemId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "popularity")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub ecosystem: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub stem_id: PackageStemId,
    pub downloads: Option<i64>,
    pub downloads_pct_ppm: Option<i64>,
    pub dependents_pct_ppm: Option<i64>,
    pub computed_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type PopularityRow = Model;

pub const TABLE: &str = "popularity";
