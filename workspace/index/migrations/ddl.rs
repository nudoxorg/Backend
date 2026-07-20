//! Schema-v4 DDL derived from SeaORM entities (INDEX-PLAN §8, REGISTRYLESS §5).
//!
//! Tables are created via [`sea_orm::Schema::create_table_from_entity`] so the
//! entity definitions are the single source of truth — no parallel hand-written
//! column lists. Extra unique indexes and secondary indexes that are not
//! expressible as entity primary keys are appended below.

use sea_orm::sea_query::{Index, SqliteQueryBuilder};
use sea_orm::{DbBackend, Schema};

use crate::entity;

/// Return all CREATE TABLE / CREATE INDEX statements for schema v4 in
/// dependency order. Every statement uses `IF NOT EXISTS`.
pub fn schema_v4_statements() -> Vec<String> {
    let schema = Schema::new(DbBackend::Sqlite);
    let mut out = Vec::new();

    // Core identity
    push_table(&mut out, &schema, entity::packages::Entity);
    push_table(&mut out, &schema, entity::versions::Entity);
    push_table(&mut out, &schema, entity::repo_facts::Entity);
    push_table(&mut out, &schema, entity::git_watermarks::Entity);
    push_table(&mut out, &schema, entity::popularity::Entity);
    push_table(&mut out, &schema, entity::generations::Entity);
    push_table(&mut out, &schema, entity::facets::Entity);
    push_table(&mut out, &schema, entity::listing_events::Entity);
    push_table(&mut out, &schema, entity::advisories::Entity);
    // Location / store
    push_table(&mut out, &schema, entity::stores::Entity);
    push_table(&mut out, &schema, entity::generation_locations::Entity);
    push_table(&mut out, &schema, entity::object_locations::Entity);
    // Compile pipeline
    push_table(&mut out, &schema, entity::compile_cache::Entity);
    push_table(&mut out, &schema, entity::edgepack_artifacts::Entity);
    // Dependency graph
    push_table(&mut out, &schema, entity::edges::Entity);
    // Projection + fan-out
    push_table(&mut out, &schema, entity::symbols_proj::Entity);
    push_table(&mut out, &schema, entity::outbox::Entity);
    push_table(&mut out, &schema, entity::sink_watermarks::Entity);
    // Overlays
    push_table(&mut out, &schema, entity::overlays::Entity);
    // Registryless
    push_table(&mut out, &schema, entity::package_aliases::Entity);
    push_table(&mut out, &schema, entity::repo_lineage::Entity);
    push_table(&mut out, &schema, entity::feed_watermarks::Entity);
    // Migration metadata last
    push_table(&mut out, &schema, entity::schema_meta::Entity);

    // Secondary indexes + unique constraints not encoded as PKs
    out.push(
        Index::create()
            .if_not_exists()
            .name("uq_packages_eco_name")
            .table(entity::packages::Entity)
            .col(entity::packages::Column::Ecosystem)
            .col(entity::packages::Column::NameCanonical)
            .unique()
            .to_string(SqliteQueryBuilder),
    );
    out.push(
        Index::create()
            .if_not_exists()
            .name("uq_versions_stem_ver")
            .table(entity::versions::Entity)
            .col(entity::versions::Column::StemId)
            .col(entity::versions::Column::VersionCanonical)
            .unique()
            .to_string(SqliteQueryBuilder),
    );
    out.push(
        Index::create()
            .if_not_exists()
            .name("idx_aliases_stem")
            .table(entity::package_aliases::Entity)
            .col(entity::package_aliases::Column::StemId)
            .to_string(SqliteQueryBuilder),
    );
    out.push(
        Index::create()
            .if_not_exists()
            .name("idx_lineage_target")
            .table(entity::repo_lineage::Entity)
            .col(entity::repo_lineage::Column::TargetStem)
            .to_string(SqliteQueryBuilder),
    );

    out
}

fn push_table<E: sea_orm::EntityTrait>(out: &mut Vec<String>, schema: &Schema, entity: E) {
    let mut stmt = schema.create_table_from_entity(entity);
    stmt.if_not_exists();
    out.push(stmt.to_string(SqliteQueryBuilder));
}
