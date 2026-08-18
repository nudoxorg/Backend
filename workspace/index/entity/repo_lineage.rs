//! `repo_lineage` — git fork/mirror relationships (REGISTRYLESS-PLAN §5).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::AliasConfidence;
use crate::enums::LineageEvidence;
use crate::enums::LineageRelation;
use crate::ids::PackageStemId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "repo_lineage")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub stem_id: PackageStemId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub relation: LineageRelation,
    #[sea_orm(primary_key, auto_increment = false)]
    pub target_stem: PackageStemId,
    pub evidence: LineageEvidence,
    pub fork_point_rev: Option<String>,
    pub overlap_ratio: Option<f64>,
    pub confidence: AliasConfidence,
    pub recorded_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type RepoLineageRow = Model;

pub const TABLE: &str = "repo_lineage";
