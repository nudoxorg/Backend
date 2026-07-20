//! SeaORM entities for the versioned catalog (schema v4).
//!
//! Each submodule is one table via [`sea_orm::DeriveEntityModel`].
//! Migrations use [`sea_orm::Schema::create_table_from_entity`].
//!
//! Historical (as-of) reads use [`Historical`]: the entity type *is* the
//! table handle; `DOLT_AT` lives next to the entity, not in a side map.

use sea_orm::EntityTrait;

pub mod advisories;
pub mod compile_cache;
pub mod edgepack_artifacts;
pub mod edges;
pub mod facets;
pub mod feed_watermarks;
pub mod generation_locations;
pub mod generations;
pub mod git_watermarks;
pub mod listing_events;
pub mod object_locations;
pub mod outbox;
pub mod overlays;
pub mod package_aliases;
pub mod packages;
pub mod popularity;
pub mod repo_facts;
pub mod repo_lineage;
pub mod schema_meta;
pub mod sink_watermarks;
pub mod stores;
pub mod symbols_proj;
pub mod versions;

// ── Pre-SeaORM module path aliases (call sites keep working) ─────────────────

/// `package_aliases` (was `tables::aliases`).
pub mod aliases {
    pub use super::package_aliases::*;
}

/// `repo_lineage` (was `tables::lineage`).
pub mod lineage {
    pub use super::repo_lineage::*;
}

/// `listing_events` (was `tables::listing`).
pub mod listing {
    pub use super::listing_events::*;
}

/// `edgepack_artifacts` (was `tables::edgepack`).
pub mod edgepack {
    pub use super::edgepack_artifacts::*;
}

/// Location tables (was a single `tables::locations` module).
pub mod locations {
    pub use super::generation_locations::{
        ActiveModel as GenerationLocationActiveModel, Column as GenerationColumn,
        Entity as GenerationLocationEntity, Model as GenerationLocationRow,
    };
    pub use super::object_locations::{
        ActiveModel as ObjectLocationActiveModel, Column as ObjectColumn,
        Entity as ObjectLocationEntity, Model as ObjectLocationRow,
    };

    pub const GENERATION_LOCATIONS_TABLE: &str = super::generation_locations::TABLE;
    pub const OBJECT_LOCATIONS_TABLE: &str = super::object_locations::TABLE;
}

/// Every catalog table name schema v4 authors (excluding migration metadata).
pub const ALL_TABLE_NAMES: &[&str] = &[
    packages::TABLE,
    versions::TABLE,
    generations::TABLE,
    stores::TABLE,
    generation_locations::TABLE,
    object_locations::TABLE,
    repo_facts::TABLE,
    git_watermarks::TABLE,
    popularity::TABLE,
    facets::TABLE,
    listing_events::TABLE,
    advisories::TABLE,
    symbols_proj::TABLE,
    outbox::TABLE,
    sink_watermarks::TABLE,
    overlays::TABLE,
    compile_cache::TABLE,
    edgepack_artifacts::TABLE,
    edges::TABLE,
    package_aliases::TABLE,
    repo_lineage::TABLE,
    feed_watermarks::TABLE,
];

/// Whether `name` is a known schema-v4 catalog table.
pub fn is_known_table(name: &str) -> bool {
    ALL_TABLE_NAMES.contains(&name)
}

pub use advisories::AdvisoryRow;
pub use outbox::OutboxRow;
pub use packages::PackageRow;
pub use versions::VersionRow;

// ─────────────────────────────────────────────────────────────────────────────
// Historical reads (entity type = table; no parallel CatalogTable map)
// ─────────────────────────────────────────────────────────────────────────────

/// A catalog entity that participates in DoltLite point-in-time reads.
///
/// Implemented next to each entity's `TABLE` constant. Call sites name the
/// entity type (`packages::Entity`); engines use [`EntityTrait`] for the tip
/// table and [`Historical::DOLT_AT`] for the `dolt_at_*` TVF.
pub trait Historical: EntityTrait + Default {
    /// DoltLite TVF identifier for this table (`dolt_at_packages`, …).
    /// A string literal co-located with the entity — never assembled.
    const DOLT_AT: &'static str;
}

macro_rules! impl_historical {
    ($($module:ident => $dolt_at:literal),+ $(,)?) => {
        $(
            impl Historical for $module::Entity {
                const DOLT_AT: &'static str = $dolt_at;
            }
        )+
    };
}

impl_historical! {
    packages => "dolt_at_packages",
    versions => "dolt_at_versions",
    generations => "dolt_at_generations",
    stores => "dolt_at_stores",
    generation_locations => "dolt_at_generation_locations",
    object_locations => "dolt_at_object_locations",
    repo_facts => "dolt_at_repo_facts",
    git_watermarks => "dolt_at_git_watermarks",
    popularity => "dolt_at_popularity",
    facets => "dolt_at_facets",
    listing_events => "dolt_at_listing_events",
    advisories => "dolt_at_advisories",
    symbols_proj => "dolt_at_symbols_proj",
    outbox => "dolt_at_outbox",
    sink_watermarks => "dolt_at_sink_watermarks",
    overlays => "dolt_at_overlays",
    compile_cache => "dolt_at_compile_cache",
    edgepack_artifacts => "dolt_at_edgepack_artifacts",
    edges => "dolt_at_edges",
    package_aliases => "dolt_at_package_aliases",
    repo_lineage => "dolt_at_repo_lineage",
    feed_watermarks => "dolt_at_feed_watermarks",
}
