//! `listing_events` — bitemporal listing lifecycle (INDEX-PLAN §8).
//!
//! SeaORM entity (`DeriveEntityModel`): [`Model`], [`ActiveModel`], [`Column`], [`Entity`].

use sea_orm::entity::prelude::*;
use crate::enums::ListingStatus;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "listing_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    pub version_id: Uuid,
    pub status: ListingStatus,
    pub reason: Option<String>,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub recorded_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Typed row alias.
pub type ListingEventRow = Model;

pub const TABLE: &str = "listing_events";

