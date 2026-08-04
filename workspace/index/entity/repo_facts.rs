//! `repo_facts` — GitHub/VCS metadata per stem (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::ids::PackageStemId;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "repo_facts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub stem_id: PackageStemId,
    pub stars: Option<i64>,
    pub last_activity_at: Option<i64>,
    pub archived: bool,
    pub default_branch: Option<String>,
    pub fetched_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type RepoFactsRow = Model;

pub const TABLE: &str = "repo_facts";

