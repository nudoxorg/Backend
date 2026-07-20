//! Schema-v4 DDL statements (INDEX-PLAN §8, REGISTRYLESS-PLAN §5).
//!
//! Every statement is built with sea-query 0.32 using [`Alias::new`] string
//! identifiers — no per-table `Iden` enums. All `CREATE TABLE` statements carry
//! `IF NOT EXISTS` and all `CREATE INDEX` statements carry `IF NOT EXISTS`, so
//! the sequence is safe to re-run even if some tables exist.
//!
//! Call [`schema_v4_statements`] to get the full ordered sequence. The caller
//! (usually [`super::runner::migrate_to_v4`]) feeds each statement to the engine.

use sea_query::{Alias, ColumnDef, Index, SqliteQueryBuilder, Table};

use crate::tables;

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Return all CREATE TABLE and CREATE INDEX statements for schema v4 in
/// dependency order (packages → versions → generations → … → registryless
/// tables → schema_meta).
///
/// Each string is a complete, semicolon-terminated SQLite DDL statement rendered
/// by sea-query. All tables use `IF NOT EXISTS`; all indexes use `IF NOT
/// EXISTS`. Running this sequence twice is a no-op (idempotence guarantee #2,
/// complementing the `user_version` early-exit in the runner).
///
/// # References
/// - INDEX-PLAN §8 — authoritative column list for the 21 core tables.
/// - REGISTRYLESS-PLAN §5 — `package_aliases`, `repo_lineage`, `feed_watermarks`.
/// - INDEX-PLAN §13 — migration protocol (pre-migrate branch + rollback).
pub fn schema_v4_statements() -> Vec<String> {
    vec![
        // Core identity tables (no FK dependencies within catalog)
        packages(),
        // Depends on packages(stem_id)
        versions(),
        repo_facts(),
        git_watermarks(),
        popularity(),
        // Depends on versions(id)
        generations(),
        facets(),
        listing_events(),
        advisories(),
        // Location / store tables
        stores(),
        generation_locations(),
        object_locations(),
        // Compile pipeline
        compile_cache(),
        edgepack_artifacts(),
        // Dependency graph
        edges(),
        // Projection + fan-out
        symbols_proj(),
        outbox(),
        sink_watermarks(),
        // Configuration / overlays
        overlays(),
        // REGISTRYLESS-PLAN §5 (package_aliases, repo_lineage, feed_watermarks)
        package_aliases(),
        idx_aliases_stem(),
        repo_lineage(),
        idx_lineage_target(),
        feed_watermarks(),
        // Migration metadata (always last)
        schema_meta(),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Core tables
// ─────────────────────────────────────────────────────────────────────────────

/// `packages` — stem-level package identity (INDEX-PLAN §8).
///
/// ```text
/// packages
///   stem_id        BLOB16 PK
///   ecosystem      TEXT NOT NULL
///   name_struct    TEXT NOT NULL
///   name_canonical TEXT NOT NULL
///   name_original  TEXT NOT NULL
///   repo_url       TEXT
///   created_at     INTEGER NOT NULL
///   UNIQUE(ecosystem, name_canonical)
/// ```
fn packages() -> String {
    Table::create()
        .table(Alias::new(tables::packages::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(tables::packages::columns::STEM_ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(tables::packages::columns::ECOSYSTEM)).text().not_null())
        .col(ColumnDef::new(Alias::new(tables::packages::columns::NAME_STRUCT)).text().not_null())
        .col(ColumnDef::new(Alias::new(tables::packages::columns::NAME_CANONICAL)).text().not_null())
        .col(ColumnDef::new(Alias::new(tables::packages::columns::NAME_ORIGINAL)).text().not_null())
        .col(ColumnDef::new(Alias::new(tables::packages::columns::REPO_URL)).text())
        .col(ColumnDef::new(Alias::new(tables::packages::columns::CREATED_AT)).big_integer().not_null())
        .index(
            Index::create()
                .name("uq_packages_eco_name")
                .col(Alias::new(tables::packages::columns::ECOSYSTEM))
                .col(Alias::new(tables::packages::columns::NAME_CANONICAL))
                .unique(),
        )
        .to_string(SqliteQueryBuilder)
}

/// `versions` — one row per (stem, version) (INDEX-PLAN §8).
///
/// ```text
/// versions
///   id                   BLOB16 PK
///   stem_id              BLOB16 NOT NULL
///   version_canonical    TEXT NOT NULL
///   version_original     TEXT NOT NULL
///   published_at         INTEGER
///   toolchain            TEXT
///   license_spdx         TEXT
///   yanked_upstream      INTEGER NOT NULL
///   parse_state          TEXT NOT NULL
///   parse_phase          TEXT
///   attempts             INTEGER NOT NULL
///   failure              TEXT
///   source_kind          TEXT NOT NULL
///   source_pack          BLOB32
///   source_rev           TEXT
///   registry_checksum    TEXT
///   registry_package_uri TEXT
///   UNIQUE(stem_id, version_canonical)
/// ```
fn versions() -> String {
    use tables::versions::columns as c;
    Table::create()
        .table(Alias::new(tables::versions::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::VERSION_CANONICAL)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::VERSION_ORIGINAL)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::PUBLISHED_AT)).big_integer())
        .col(ColumnDef::new(Alias::new(c::TOOLCHAIN)).text())
        .col(ColumnDef::new(Alias::new(c::LICENSE_SPDX)).text())
        .col(ColumnDef::new(Alias::new(c::YANKED_UPSTREAM)).integer().not_null().default(0))
        .col(ColumnDef::new(Alias::new(c::PARSE_STATE)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::PARSE_PHASE)).text())
        .col(ColumnDef::new(Alias::new(c::ATTEMPTS)).integer().not_null().default(0))
        .col(ColumnDef::new(Alias::new(c::FAILURE)).text())
        .col(ColumnDef::new(Alias::new(c::SOURCE_KIND)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::SOURCE_PACK)).blob())
        .col(ColumnDef::new(Alias::new(c::SOURCE_REV)).text())
        .col(ColumnDef::new(Alias::new(c::REGISTRY_CHECKSUM)).text())
        .col(ColumnDef::new(Alias::new(c::REGISTRY_PACKAGE_URI)).text())
        .index(
            Index::create()
                .name("uq_versions_stem_ver")
                .col(Alias::new(c::STEM_ID))
                .col(Alias::new(c::VERSION_CANONICAL))
                .unique(),
        )
        .to_string(SqliteQueryBuilder)
}

/// `generations` — one row per (version, compiler-invocation) IR generation
/// (INDEX-PLAN §8, ID-15).
///
/// ```text
/// generations
///   gen_stamp          BLOB32 PK
///   version_id         BLOB16 NOT NULL
///   channel_tip        BLOB32
///   job_key            BLOB32
///   producer_toolchain TEXT
///   sealed_at          INTEGER
///   ir_status          TEXT NOT NULL
///   resolution_stats   TEXT
/// ```
fn generations() -> String {
    use tables::generations::columns as c;
    Table::create()
        .table(Alias::new(tables::generations::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::GEN_STAMP)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::VERSION_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::CHANNEL_TIP)).blob())
        .col(ColumnDef::new(Alias::new(c::JOB_KEY)).blob())
        .col(ColumnDef::new(Alias::new(c::PRODUCER_TOOLCHAIN)).text())
        .col(ColumnDef::new(Alias::new(c::SEALED_AT)).big_integer())
        .col(ColumnDef::new(Alias::new(c::IR_STATUS)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::RESOLUTION_STATS)).text())
        .to_string(SqliteQueryBuilder)
}

/// `stores` — IR-VCS and ObjectPack store registrations (INDEX-PLAN §8).
///
/// ```text
/// stores
///   store_id  BLOB16 PK
///   kind      TEXT NOT NULL
///   endpoint  TEXT NOT NULL
///   healthy   INTEGER NOT NULL
///   added_at  INTEGER NOT NULL
/// ```
fn stores() -> String {
    use tables::stores::columns as c;
    Table::create()
        .table(Alias::new(tables::stores::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::STORE_ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::KIND)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::ENDPOINT)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::HEALTHY)).integer().not_null())
        .col(ColumnDef::new(Alias::new(c::ADDED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `generation_locations` — store-presence tracking for IR generations
/// (INDEX-PLAN §8).
///
/// ```text
/// generation_locations
///   gen_stamp BLOB32 NOT NULL
///   store_id  BLOB16 NOT NULL
///   status    TEXT NOT NULL
///   PRIMARY KEY (gen_stamp, store_id)
/// ```
fn generation_locations() -> String {
    use tables::locations::{generation_columns as c, GENERATION_LOCATIONS_TABLE};
    Table::create()
        .table(Alias::new(GENERATION_LOCATIONS_TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::GEN_STAMP)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::STORE_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::STATUS)).text().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::GEN_STAMP))
                .col(Alias::new(c::STORE_ID)),
        )
        .to_string(SqliteQueryBuilder)
}

/// `object_locations` — store-presence tracking for ObjectPacks (INDEX-PLAN §8).
///
/// ```text
/// object_locations
///   object_id BLOB32 NOT NULL
///   store_id  BLOB16 NOT NULL
///   status    TEXT NOT NULL
///   PRIMARY KEY (object_id, store_id)
/// ```
fn object_locations() -> String {
    use tables::locations::{object_columns as c, OBJECT_LOCATIONS_TABLE};
    Table::create()
        .table(Alias::new(OBJECT_LOCATIONS_TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::OBJECT_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::STORE_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::STATUS)).text().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::OBJECT_ID))
                .col(Alias::new(c::STORE_ID)),
        )
        .to_string(SqliteQueryBuilder)
}

/// `repo_facts` — GitHub/VCS metadata per stem (INDEX-PLAN §8).
///
/// ```text
/// repo_facts
///   stem_id          BLOB16 PK
///   stars            INTEGER
///   last_activity_at INTEGER
///   archived         INTEGER NOT NULL
///   default_branch   TEXT
///   fetched_at       INTEGER NOT NULL
/// ```
fn repo_facts() -> String {
    use tables::repo_facts::columns as c;
    Table::create()
        .table(Alias::new(tables::repo_facts::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::STARS)).big_integer())
        .col(ColumnDef::new(Alias::new(c::LAST_ACTIVITY_AT)).big_integer())
        .col(ColumnDef::new(Alias::new(c::ARCHIVED)).integer().not_null().default(0))
        .col(ColumnDef::new(Alias::new(c::DEFAULT_BRANCH)).text())
        .col(ColumnDef::new(Alias::new(c::FETCHED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `git_watermarks` — per-stem git polling watermarks (INDEX-PLAN §8).
///
/// ```text
/// git_watermarks
///   stem_id         BLOB16 PK
///   last_rev        TEXT
///   last_checked_at INTEGER NOT NULL
///   last_error      TEXT
/// ```
fn git_watermarks() -> String {
    use tables::git_watermarks::columns as c;
    Table::create()
        .table(Alias::new(tables::git_watermarks::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::LAST_REV)).text())
        .col(ColumnDef::new(Alias::new(c::LAST_CHECKED_AT)).big_integer().not_null())
        .col(ColumnDef::new(Alias::new(c::LAST_ERROR)).text())
        .to_string(SqliteQueryBuilder)
}

/// `popularity` — download and dependent-count percentiles per (ecosystem, stem)
/// (INDEX-PLAN §8).
///
/// ```text
/// popularity
///   ecosystem          TEXT NOT NULL
///   stem_id            BLOB16 NOT NULL
///   downloads          INTEGER
///   downloads_pct_ppm  INTEGER
///   dependents_pct_ppm INTEGER
///   computed_at        INTEGER NOT NULL
///   PRIMARY KEY (ecosystem, stem_id)
/// ```
fn popularity() -> String {
    use tables::popularity::columns as c;
    Table::create()
        .table(Alias::new(tables::popularity::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::ECOSYSTEM)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::DOWNLOADS)).big_integer())
        .col(ColumnDef::new(Alias::new(c::DOWNLOADS_PCT_PPM)).big_integer())
        .col(ColumnDef::new(Alias::new(c::DEPENDENTS_PCT_PPM)).big_integer())
        .col(ColumnDef::new(Alias::new(c::COMPUTED_AT)).big_integer().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::ECOSYSTEM))
                .col(Alias::new(c::STEM_ID)),
        )
        .to_string(SqliteQueryBuilder)
}

/// `facets` — per-version quality metadata and keyword tags (INDEX-PLAN §8).
///
/// ```text
/// facets
///   version_id  BLOB16 PK
///   keywords    TEXT
///   quality_ppm INTEGER
///   extras      TEXT
/// ```
fn facets() -> String {
    use tables::facets::columns as c;
    Table::create()
        .table(Alias::new(tables::facets::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::VERSION_ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::KEYWORDS)).text())
        .col(ColumnDef::new(Alias::new(c::QUALITY_PPM)).big_integer())
        .col(ColumnDef::new(Alias::new(c::EXTRAS)).text())
        .to_string(SqliteQueryBuilder)
}

/// `listing_events` — bitemporal listing lifecycle for package versions
/// (INDEX-PLAN §8).
///
/// `seq` is `INTEGER PRIMARY KEY AUTOINCREMENT` (engine-assigned; omitted from
/// inserts by the row codec — see [`crate::tables::listing`]).
///
/// ```text
/// listing_events
///   seq        INTEGER PRIMARY KEY AUTOINCREMENT
///   version_id BLOB16 NOT NULL
///   status     TEXT NOT NULL
///   reason     TEXT
///   valid_from INTEGER NOT NULL
///   valid_to   INTEGER
///   recorded_at INTEGER NOT NULL
/// ```
fn listing_events() -> String {
    use tables::listing::columns as c;
    Table::create()
        .table(Alias::new(tables::listing::TABLE))
        .if_not_exists()
        .col(
            ColumnDef::new(Alias::new(c::SEQ))
                .integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(ColumnDef::new(Alias::new(c::VERSION_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::STATUS)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::REASON)).text())
        .col(ColumnDef::new(Alias::new(c::VALID_FROM)).big_integer().not_null())
        .col(ColumnDef::new(Alias::new(c::VALID_TO)).big_integer())
        .col(ColumnDef::new(Alias::new(c::RECORDED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `advisories` — security advisory records (INDEX-PLAN §8, REGISTRYLESS §12).
///
/// ```text
/// advisories
///   id           BLOB16 PK
///   stem_id      BLOB16
///   version_range TEXT
///   severity     TEXT
///   summary      TEXT
///   url          TEXT
///   valid_from   INTEGER NOT NULL
///   valid_to     INTEGER
///   recorded_at  INTEGER NOT NULL
/// ```
fn advisories() -> String {
    use tables::advisories::columns as c;
    Table::create()
        .table(Alias::new(tables::advisories::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::ID)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob())
        .col(ColumnDef::new(Alias::new(c::VERSION_RANGE)).text())
        .col(ColumnDef::new(Alias::new(c::SEVERITY)).text())
        .col(ColumnDef::new(Alias::new(c::SUMMARY)).text())
        .col(ColumnDef::new(Alias::new(c::URL)).text())
        .col(ColumnDef::new(Alias::new(c::VALID_FROM)).big_integer().not_null())
        .col(ColumnDef::new(Alias::new(c::VALID_TO)).big_integer())
        .col(ColumnDef::new(Alias::new(c::RECORDED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `symbols_proj` — IR symbol search projection (INDEX-PLAN §8).
///
/// ```text
/// symbols_proj
///   intro_id   BLOB32 NOT NULL
///   version_id BLOB16 NOT NULL
///   gen_stamp  BLOB32 NOT NULL
///   moniker    TEXT NOT NULL
///   kind       TEXT NOT NULL
///   PRIMARY KEY (gen_stamp, intro_id)
/// ```
fn symbols_proj() -> String {
    use tables::symbols_proj::columns as c;
    Table::create()
        .table(Alias::new(tables::symbols_proj::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::INTRO_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::VERSION_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::GEN_STAMP)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::MONIKER)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::KIND)).text().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::GEN_STAMP))
                .col(Alias::new(c::INTRO_ID)),
        )
        .to_string(SqliteQueryBuilder)
}

/// `outbox` — the projection fan-out table (INDEX-PLAN §8, ID-3).
///
/// `seq` is `INTEGER PRIMARY KEY AUTOINCREMENT` (engine-assigned; omitted from
/// inserts — see [`crate::tables::outbox`]).
///
/// ```text
/// outbox
///   seq        INTEGER PRIMARY KEY AUTOINCREMENT
///   version_id BLOB16
///   gen_stamp  BLOB32
///   sink_kind  TEXT NOT NULL
///   op         TEXT NOT NULL
///   created_at INTEGER NOT NULL
/// ```
fn outbox() -> String {
    use tables::outbox::columns as c;
    Table::create()
        .table(Alias::new(tables::outbox::TABLE))
        .if_not_exists()
        .col(
            ColumnDef::new(Alias::new(c::SEQ))
                .integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(ColumnDef::new(Alias::new(c::VERSION_ID)).blob())
        .col(ColumnDef::new(Alias::new(c::GEN_STAMP)).blob())
        .col(ColumnDef::new(Alias::new(c::SINK_KIND)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::OP)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::CREATED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `sink_watermarks` — per-sink outbox consume cursor (INDEX-PLAN ID-3).
///
/// ```text
/// sink_watermarks
///   sink_kind  TEXT PRIMARY KEY
///   last_seq   INTEGER NOT NULL
///   updated_at INTEGER NOT NULL
/// ```
fn sink_watermarks() -> String {
    use tables::sink_watermarks::columns as c;
    Table::create()
        .table(Alias::new(tables::sink_watermarks::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::SINK_KIND)).text().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::LAST_SEQ)).big_integer().not_null())
        .col(ColumnDef::new(Alias::new(c::UPDATED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `overlays` — DoltLite overlay branch registry (INDEX-PLAN ID-9).
///
/// ```text
/// overlays
///   name               TEXT PRIMARY KEY
///   remote_endpoint    TEXT
///   branch             TEXT NOT NULL
///   precedence         INTEGER NOT NULL
///   last_merged_commit TEXT
///   added_at           INTEGER NOT NULL
/// ```
fn overlays() -> String {
    use tables::overlays::columns as c;
    Table::create()
        .table(Alias::new(tables::overlays::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::NAME)).text().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::REMOTE_ENDPOINT)).text())
        .col(ColumnDef::new(Alias::new(c::BRANCH)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::PRECEDENCE)).integer().not_null())
        .col(ColumnDef::new(Alias::new(c::LAST_MERGED_COMMIT)).text())
        .col(ColumnDef::new(Alias::new(c::ADDED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `compile_cache` — producer job-key → result cache (INDEX-PLAN §8).
///
/// ```text
/// compile_cache
///   job_key      BLOB32 PRIMARY KEY
///   kind         TEXT NOT NULL
///   gen_stamp    BLOB32
///   object_id    BLOB32
///   image_digest TEXT
///   updated_at   INTEGER NOT NULL
/// ```
fn compile_cache() -> String {
    use tables::compile_cache::columns as c;
    Table::create()
        .table(Alias::new(tables::compile_cache::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::JOB_KEY)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::KIND)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::GEN_STAMP)).blob())
        .col(ColumnDef::new(Alias::new(c::OBJECT_ID)).blob())
        .col(ColumnDef::new(Alias::new(c::IMAGE_DIGEST)).text())
        .col(ColumnDef::new(Alias::new(c::UPDATED_AT)).big_integer().not_null())
        .to_string(SqliteQueryBuilder)
}

/// `edgepack_artifacts` — built edgepack bundles keyed by recipe digest
/// (INDEX-PLAN §8).
///
/// ```text
/// edgepack_artifacts
///   edgepack_key_digest BLOB PRIMARY KEY
///   version_id          BLOB16 NOT NULL
///   recipe_fingerprint  TEXT NOT NULL
///   artifact_id         BLOB
///   ram_estimate        INTEGER
///   published_at        INTEGER
///   status              TEXT NOT NULL       -- claimed | ready | failed
///   updated_at          INTEGER NOT NULL    -- unix milliseconds
/// ```
fn edgepack_artifacts() -> String {
    use tables::edgepack::columns as c;
    Table::create()
        .table(Alias::new(tables::edgepack::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::EDGEPACK_KEY_DIGEST)).blob().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::VERSION_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::RECIPE_FINGERPRINT)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::ARTIFACT_ID)).blob())
        .col(ColumnDef::new(Alias::new(c::RAM_ESTIMATE)).big_integer())
        .col(ColumnDef::new(Alias::new(c::PUBLISHED_AT)).big_integer())
        .col(ColumnDef::new(Alias::new(c::STATUS)).text().not_null().default("claimed"))
        .col(ColumnDef::new(Alias::new(c::UPDATED_AT)).big_integer().not_null().default(0))
        .to_string(SqliteQueryBuilder)
}

/// `edges` — dependency graph edges (INDEX-PLAN §8, REGISTRYLESS RL-5).
///
/// ```text
/// edges
///   dependent_version  BLOB16 NOT NULL
///   dep_ecosystem      TEXT NOT NULL
///   dep_name_canonical TEXT NOT NULL
///   requirement        TEXT NOT NULL
///   resolved_stem      BLOB16
///   kind               TEXT NOT NULL
///   source             TEXT NOT NULL
///   PRIMARY KEY (dependent_version, dep_ecosystem, dep_name_canonical, kind)
/// ```
fn edges() -> String {
    use tables::edges::columns as c;
    Table::create()
        .table(Alias::new(tables::edges::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::DEPENDENT_VERSION)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::DEP_ECOSYSTEM)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::DEP_NAME_CANONICAL)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::REQUIREMENT)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::RESOLVED_STEM)).blob())
        .col(ColumnDef::new(Alias::new(c::KIND)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::SOURCE)).text().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::DEPENDENT_VERSION))
                .col(Alias::new(c::DEP_ECOSYSTEM))
                .col(Alias::new(c::DEP_NAME_CANONICAL))
                .col(Alias::new(c::KIND)),
        )
        .to_string(SqliteQueryBuilder)
}

// ─────────────────────────────────────────────────────────────────────────────
// REGISTRYLESS-PLAN §5 tables and their indexes
// ─────────────────────────────────────────────────────────────────────────────

/// `package_aliases` — name-alias mappings for the registryless resolver
/// (REGISTRYLESS-PLAN §5, RL-6).
///
/// ```text
/// package_aliases
///   ecosystem   TEXT NOT NULL
///   alias_kind  TEXT NOT NULL
///   alias       TEXT NOT NULL
///   stem_id     BLOB16 NOT NULL
///   confidence  TEXT NOT NULL
///   recorded_at INTEGER NOT NULL
///   PRIMARY KEY (ecosystem, alias_kind, alias)
/// ```
fn package_aliases() -> String {
    use tables::aliases::columns as c;
    Table::create()
        .table(Alias::new(tables::aliases::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::ECOSYSTEM)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::ALIAS_KIND)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::ALIAS)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::CONFIDENCE)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::RECORDED_AT)).big_integer().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::ECOSYSTEM))
                .col(Alias::new(c::ALIAS_KIND))
                .col(Alias::new(c::ALIAS)),
        )
        .to_string(SqliteQueryBuilder)
}

/// `CREATE INDEX IF NOT EXISTS idx_aliases_stem ON package_aliases(stem_id)`
/// (REGISTRYLESS-PLAN §5).
fn idx_aliases_stem() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_aliases_stem")
        .table(Alias::new(tables::aliases::TABLE))
        .col(Alias::new(tables::aliases::columns::STEM_ID))
        .to_string(SqliteQueryBuilder)
}

/// `repo_lineage` — git-based fork/mirror relationships (REGISTRYLESS-PLAN §5,
/// RL-16).
///
/// ```text
/// repo_lineage
///   stem_id        BLOB16 NOT NULL
///   relation       TEXT NOT NULL
///   target_stem    BLOB16 NOT NULL
///   evidence       TEXT NOT NULL
///   fork_point_rev TEXT
///   overlap_ratio  REAL
///   confidence     TEXT NOT NULL
///   recorded_at    INTEGER NOT NULL
///   PRIMARY KEY (stem_id, relation, target_stem)
/// ```
fn repo_lineage() -> String {
    use tables::lineage::columns as c;
    Table::create()
        .table(Alias::new(tables::lineage::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::STEM_ID)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::RELATION)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::TARGET_STEM)).blob().not_null())
        .col(ColumnDef::new(Alias::new(c::EVIDENCE)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::FORK_POINT_REV)).text())
        .col(ColumnDef::new(Alias::new(c::OVERLAP_RATIO)).double())
        .col(ColumnDef::new(Alias::new(c::CONFIDENCE)).text().not_null())
        .col(ColumnDef::new(Alias::new(c::RECORDED_AT)).big_integer().not_null())
        .primary_key(
            Index::create()
                .col(Alias::new(c::STEM_ID))
                .col(Alias::new(c::RELATION))
                .col(Alias::new(c::TARGET_STEM)),
        )
        .to_string(SqliteQueryBuilder)
}

/// `CREATE INDEX IF NOT EXISTS idx_lineage_target ON repo_lineage(target_stem)`
/// (REGISTRYLESS-PLAN §5).
fn idx_lineage_target() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_lineage_target")
        .table(Alias::new(tables::lineage::TABLE))
        .col(Alias::new(tables::lineage::columns::TARGET_STEM))
        .to_string(SqliteQueryBuilder)
}

/// `feed_watermarks` — per-feed crawl cursor for the registryless discovery
/// pipeline (REGISTRYLESS-PLAN §5).
///
/// ```text
/// feed_watermarks
///   feed            TEXT PRIMARY KEY
///   last_ref        TEXT
///   last_checked_at INTEGER NOT NULL
///   last_error      TEXT
/// ```
fn feed_watermarks() -> String {
    use tables::feed_watermarks::columns as c;
    Table::create()
        .table(Alias::new(tables::feed_watermarks::TABLE))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new(c::FEED)).text().not_null().primary_key())
        .col(ColumnDef::new(Alias::new(c::LAST_REF)).text())
        .col(ColumnDef::new(Alias::new(c::LAST_CHECKED_AT)).big_integer().not_null())
        .col(ColumnDef::new(Alias::new(c::LAST_ERROR)).text())
        .to_string(SqliteQueryBuilder)
}

// ─────────────────────────────────────────────────────────────────────────────
// Migration metadata
// ─────────────────────────────────────────────────────────────────────────────

/// `schema_meta` — single-row migration version tracking (INDEX-PLAN ID-5).
///
/// The runner maintains exactly one row: `user_version` is the schema version
/// that was successfully applied. `0` (no row) means the schema has never been
/// migrated. The runner inserts/replaces this row at the end of each migration.
///
/// ```text
/// schema_meta
///   user_version INTEGER NOT NULL
/// ```
fn schema_meta() -> String {
    Table::create()
        .table(Alias::new("schema_meta"))
        .if_not_exists()
        .col(ColumnDef::new(Alias::new("user_version")).integer().not_null())
        .to_string(SqliteQueryBuilder)
}
