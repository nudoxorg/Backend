//! `overlays` — DoltLite overlay branch registry (INDEX-PLAN ID-9).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "overlays")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    pub remote_endpoint: Option<String>,
    pub branch: String,
    pub precedence: i64,
    pub last_merged_commit: Option<String>,
    pub added_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type OverlayRow = Model;

pub const TABLE: &str = "overlays";
