//! `generations` — one IR generation per compiler invocation (INDEX-PLAN §8, ID-15).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use crate::enums::IrStatus;
use crate::ids::ChannelTip;
use crate::ids::GenerationStamp;
use crate::ids::JobKeyHash;
use sea_orm::entity::prelude::*;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "generations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub gen_stamp: GenerationStamp,
    pub version_id: Uuid,
    pub channel_tip: Option<ChannelTip>,
    pub job_key: Option<JobKeyHash>,
    pub producer_toolchain: Option<String>,
    pub sealed_at: Option<i64>,
    pub ir_status: IrStatus,
    pub resolution_stats: Option<String>,
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
pub type GenerationRow = Model;

pub const TABLE: &str = "generations";
