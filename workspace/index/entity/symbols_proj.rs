//! `symbols_proj` — IR symbol search projection (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::ids::GenerationStamp;
use crate::ids::IntroIdHash;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "symbols_proj")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub gen_stamp: GenerationStamp,
    #[sea_orm(primary_key, auto_increment = false)]
    pub intro_id: IntroIdHash,
    pub version_id: Uuid,
    pub moniker: String,
    pub kind: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type SymbolProjectionRow = Model;

pub const TABLE: &str = "symbols_proj";

