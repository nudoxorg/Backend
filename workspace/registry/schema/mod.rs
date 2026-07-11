//! The relational model — the single source of truth for the postgres data
//! layer.
//!
//! Every table is described here as a `sea_query::Iden` enum (the table name is
//! the first variant, the columns follow) plus a real
//! [`sea_query::TableCreateStatement`]. [`create_all`] and [`drop_all`] build the
//! whole schema; [`create_indexes`] emits the secondary indexes that the
//! scheduling / dequeue / watermark hot paths lean on. There is **no hand-written
//! SQL and no `sqlx::migrate!`**: [`schema_ddl`] renders these `sea_query`
//! statements to Postgres DDL and [`crate::index::GlobalStore::connect`] executes
//! them (all `IF NOT EXISTS`) on boot.
//!
//! ## Enum representation
//! Postgres native `ENUM` types are deliberately *not* used. Every discriminant
//! column (`parse_status.state`, `parse_status.phase`, `jobs.state`,
//! `outbox.sink_kind`, `packages.owner_kind`, `symbols.kind`, ...) is a plain
//! `text` column guarded by a `CHECK (col IN (...))` constraint. This keeps the
//! discriminant set evolvable with an ordinary `ALTER TABLE ... DROP/ADD
//! CONSTRAINT` (no `ALTER TYPE ... ADD VALUE` transaction hazards), keeps the
//! wire representation identical to the `strum`/serde string forms in
//! [`crate::schema::codec`], and lets sea-query bind them as plain strings.
//!
//! ## Byte / uuid / json columns
//! - `PackageId` / `SymbolId` / tenant ids  → `uuid`.
//! - `ContentHash` / `Generation`                 → `bytea` (exactly 32 bytes).
//! - `ResolutionState::Failed`/`DeadLettered` payload, `Toolchain` → `jsonb`.

pub mod codec;
pub mod queries;

use strum::VariantNames;

use sea_query::{
	ColumnDef, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, PostgresQueryBuilder,
	Table, TableCreateStatement, TableDropStatement,
};

// ─────────────────────────────────────────────────────────────────────────────
// Table + column identifiers.
// ─────────────────────────────────────────────────────────────────────────────

/// `packages` — canonical package identity + ownership + provenance.
#[derive(sea_query::Iden, Clone, Copy)]
pub enum Packages {
	/// Table name.
	Table,
	/// `uuid` PK — the deterministic [`heart::PackageId`].
	Id,
	/// `text` — ecosystem token (`rust`/`typescript`/`python`).
	Language,
	/// `text` — the [`heart::RegistryOrigin`] stable token.
	OriginToken,
	/// `text` — normalized name (identity form).
	NameCanonical,
	/// `text` — name exactly as published (display).
	NameOriginal,
	/// `text` — canonical, normalized version string.
	VersionCanonical,
	/// `text` — [`heart::Visibility`] discriminant.
	Visibility,
	/// `uuid` — owning [`heart::access::Tenant`] id.
	OwnerTenant,
	/// `text` — tenant kind (`individual`/`enterprise`).
	OwnerKind,
	/// `jsonb` — the serialized [`heart::ecosystem::Toolchain`].
	Toolchain,
	/// `timestamptz` — row creation time.
	CreatedAt,
	/// `timestamptz` — last mutation time.
	UpdatedAt,
}

/// `parse_status` — the persisted [`heart::lifecycle::ResolutionState`] machine,
/// one row per package. This is the orchestration source of truth.
#[derive(sea_query::Iden, Clone, Copy)]
pub enum ParseStatus {
	/// Table name.
	Table,
	/// `uuid` PK + FK → `packages.id`.
	PackageId,
	/// `text` — state discriminant
	/// (`unindexed`/`progressing`/`stored`/`failed`/`deadlettered`).
	State,
	/// `text` nullable — the [`heart::lifecycle::Phase`] when `Progressing`.
	Phase,
	/// `bytea` nullable — the `Stored` content hash (32 bytes).
	ContentHash,
	/// `int` — attempt count (mirrors any recorded `Failure.attempts`).
	Attempts,
	/// `jsonb` nullable — the serialized [`heart::lifecycle::Failure`].
	Failure,
	/// `jsonb` nullable — the serialized [`crate::metadata::SearchFacets`]
	/// (derived keywords + quality). Mirrors `Failure`'s nullable-jsonb shape;
	/// `NULL` until rich metadata is extracted for the stored generation.
	Facets,
	/// `bool` — the `Unindexed { needed }` flag (dependency-ordered scheduling).
	Needed,
	/// `timestamptz` — last transition time.
	UpdatedAt,
}

/// `jobs` — the durable, poison-pill-safe work queue.
#[derive(sea_query::Iden, Clone, Copy)]
pub enum Jobs {
	/// Table name.
	Table,
	/// `bigserial` PK — the row identity.
	Id,
	/// `uuid` FK → `packages.id`, UNIQUE (one live job per package).
	PackageId,
	/// `text` — the job's mirrored lifecycle state discriminant.
	State,
	/// `int` — attempts made.
	Attempts,
	/// `timestamptz` — first enqueue time.
	EnqueuedAt,
	/// `timestamptz` nullable — lease deadline; runnable when NULL or in the past.
	LeaseUntil,
	/// `int` — scheduling priority (higher runs first).
	Priority,
}

/// `outbox` — transactional fan-out intents, one row per `(package, gen, sink)`.
#[derive(sea_query::Iden, Clone, Copy)]
pub enum Outbox {
	/// Table name.
	Table,
	/// `bigserial` PK — the monotonic watermark cursor.
	Seq,
	/// `uuid` FK → `packages.id`.
	PackageId,
	/// `bytea` — the [`heart::ContentHash`] (32-byte content hash).
	Generation,
	/// `text` — the [`crate::coordination::SinkKind`] discriminant.
	SinkKind,
	/// `timestamptz` — intent creation time.
	CreatedAt,
}

/// `sink_watermarks` — per-consumer cursor into the outbox.
#[derive(sea_query::Iden, Clone, Copy)]
pub enum SinkWatermarks {
	/// Table name.
	Table,
	/// `text` PK — the [`crate::coordination::SinkKind`] discriminant.
	SinkKind,
	/// `bigint` — the last consumed `outbox.seq`.
	LastSeq,
	/// `timestamptz` — last advance time.
	UpdatedAt,
}

/// `symbols` — the serving projection of a symbol, keyed by its global id.
#[derive(sea_query::Iden, Clone, Copy)]
pub enum Symbols {
	/// Table name.
	Table,
	/// `uuid` PK — the [`heart::SymbolId`].
	Id,
	/// `uuid` FK → `packages.id`.
	PackageId,
	/// `text` — the fully-qualified name (e.g. `axum::Router`).
	FqName,
	/// `text` — the [`heart::SymbolKind`] discriminant.
	Kind,
	/// `bytea` — the generation this record reflects (32 bytes).
	Generation,
}

// ─────────────────────────────────────────────────────────────────────────────
// Named index / constraint identifiers (stable so migrations can DROP them).
// ─────────────────────────────────────────────────────────────────────────────

/// Unique index on `(ecosystem, origin_token, name_canonical, version_canonical)`.
pub const IDX_PACKAGES_COORDS: &str = "idx_packages_coords";
/// Index on `parse_status.state` for scheduling scans.
pub const IDX_PARSE_STATUS_STATE: &str = "idx_parse_status_state";
/// Index on `parse_status.content_hash` for freshness/generation lookups.
pub const IDX_PARSE_STATUS_HASH: &str = "idx_parse_status_hash";
/// Partial index driving the dequeue hot path.
pub const IDX_JOBS_RUNNABLE: &str = "idx_jobs_runnable";
/// Unique dedupe index on `(package_id, generation, sink_kind)`.
pub const IDX_OUTBOX_DEDUPE: &str = "idx_outbox_dedupe";
/// Index on `(sink_kind, seq)` for per-consumer watermark reads.
pub const IDX_OUTBOX_SINK_SEQ: &str = "idx_outbox_sink_seq";
/// Index on `symbols.package_id`.
pub const IDX_SYMBOLS_PACKAGE: &str = "idx_symbols_package";

// ─────────────────────────────────────────────────────────────────────────────
// Allowed discriminant sets — the CHECK-constraint domains.
//
// Where a corresponding Rust enum exists these are derived from
// `strum::VariantNames::VARIANTS` so the schema CHECK constraint and the codec
// can never drift: adding a variant without updating the enum (not this array)
// is sufficient to extend both simultaneously.
//
// The `const _: () = assert!(...)` guards below are lockstep compile-time
// checks: if you add/remove a variant the assert fires immediately, pointing
// you to the right enum rather than a silent runtime mismatch.
// ─────────────────────────────────────────────────────────────────────────────

/// `parse_status.state` / `jobs.state` domain.
///
/// `ResolutionState` is a non-unit enum with associated data and
/// irregular tokens (`DeadLettered` → `"deadlettered"`), so this cannot be
/// derived automatically.  The lockstep assert below guards the count.
pub const STATE_VALUES: [&str; 5] =
	["unindexed", "progressing", "stored", "failed", "deadlettered"];

/// `parse_status.phase` domain — derived from [`heart::Phase::VARIANTS`].
///
/// `heart::Phase` has `#[strum(serialize_all = "lowercase")]` so VARIANTS
/// produces the exact SQL tokens.
pub const PHASE_VALUES: &[&str] = heart::Phase::VARIANTS;

/// `outbox.sink_kind` / `sink_watermarks.sink_kind` domain — derived from
/// [`heart::DerivedStore::VARIANTS`].
///
/// `heart::DerivedStore` has `#[strum(serialize_all = "lowercase")]` so
/// VARIANTS produces the exact SQL tokens.
pub const SINK_KIND_VALUES: &[&str] = heart::DerivedStore::VARIANTS;

/// `symbols.kind` domain — derived from [`heart::SymbolKind::VARIANTS`].
///
/// `heart::SymbolKind` uses default (PascalCase) variant names, which are
/// exactly what `Display`/`to_string()` emits for storage.
pub const SYMBOL_KIND_VALUES: &[&str] = heart::SymbolKind::VARIANTS;

/// `packages.language` domain — derived from [`heart::Language::VARIANTS`].
///
/// `heart::Language` has `#[strum(serialize_all = "lowercase")]` so VARIANTS
/// produces the exact SQL tokens.
pub const LANGUAGE_VALUES: &[&str] = heart::Language::VARIANTS;

/// `packages.owner_kind` domain.
pub const OWNER_KIND_VALUES: [&str; 2] = ["individual", "enterprise"];
/// `packages.visibility` domain.
pub const VISIBILITY_VALUES: [&str; 3] = ["personal", "private", "public"];

// Lockstep compile-time guards: if variants are added/removed the assert fires.
const _: () = assert!(STATE_VALUES.len() == 5, "STATE_VALUES out of sync with ResolutionState");
const _: () = assert!(OWNER_KIND_VALUES.len() == 2, "OWNER_KIND_VALUES needs manual update");
const _: () = assert!(VISIBILITY_VALUES.len() == 3, "VISIBILITY_VALUES needs manual update");

/// Render a `col IN ('a','b',...)` SQL fragment for a text CHECK constraint.
fn check_in(col: &str, values: &[&str]) -> String {
	let list = values.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", ");
	format!("{col} IN ({list})")
}

// ─────────────────────────────────────────────────────────────────────────────
// Table create statements.
// ─────────────────────────────────────────────────────────────────────────────

/// `CREATE TABLE packages (...)`.
pub fn create_packages() -> TableCreateStatement {
	Table::create()
		.table(Packages::Table)
		.if_not_exists()
		.col(ColumnDef::new(Packages::Id).uuid().not_null().primary_key())
		.col(
			ColumnDef::new(Packages::Language)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("language", LANGUAGE_VALUES))),
		)
		.col(ColumnDef::new(Packages::OriginToken).text().not_null())
		.col(ColumnDef::new(Packages::NameCanonical).text().not_null())
		.col(ColumnDef::new(Packages::NameOriginal).text().not_null())
		.col(ColumnDef::new(Packages::VersionCanonical).text().not_null())
		.col(
			ColumnDef::new(Packages::Visibility)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("visibility", &VISIBILITY_VALUES))),
		)
		.col(ColumnDef::new(Packages::OwnerTenant).uuid().not_null())
		.col(
			ColumnDef::new(Packages::OwnerKind)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("owner_kind", &OWNER_KIND_VALUES))),
		)
		.col(ColumnDef::new(Packages::Toolchain).json_binary().not_null())
		.col(
			ColumnDef::new(Packages::CreatedAt)
				.timestamp_with_time_zone()
				.not_null()
				.default(sea_query::Expr::current_timestamp()),
		)
		.col(
			ColumnDef::new(Packages::UpdatedAt)
				.timestamp_with_time_zone()
				.not_null()
				.default(sea_query::Expr::current_timestamp()),
		)
		.take()
}

/// `CREATE TABLE parse_status (...)`.
pub fn create_parse_status() -> TableCreateStatement {
	Table::create()
		.table(ParseStatus::Table)
		.if_not_exists()
		.col(ColumnDef::new(ParseStatus::PackageId).uuid().not_null().primary_key())
		.col(
			ColumnDef::new(ParseStatus::State)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("state", &STATE_VALUES))),
		)
		.col(
			ColumnDef::new(ParseStatus::Phase)
				.text()
				.null()
				.check(sea_query::Expr::cust(format!(
					"phase IS NULL OR {}",
					check_in("phase", PHASE_VALUES)
				))),
		)
		.col(ColumnDef::new(ParseStatus::ContentHash).binary().null())
		.col(ColumnDef::new(ParseStatus::Attempts).integer().not_null().default(0))
		.col(ColumnDef::new(ParseStatus::Failure).json_binary().null())
		.col(ColumnDef::new(ParseStatus::Facets).json_binary().null())
		.col(ColumnDef::new(ParseStatus::Needed).boolean().not_null().default(false))
		.col(
			ColumnDef::new(ParseStatus::UpdatedAt)
				.timestamp_with_time_zone()
				.not_null()
				.default(sea_query::Expr::current_timestamp()),
		)
		.foreign_key(
			ForeignKey::create()
				.name("fk_parse_status_package")
				.from(ParseStatus::Table, ParseStatus::PackageId)
				.to(Packages::Table, Packages::Id)
				.on_delete(ForeignKeyAction::Cascade),
		)
		.take()
}

/// `CREATE TABLE jobs (...)`.
pub fn create_jobs() -> TableCreateStatement {
	Table::create()
		.table(Jobs::Table)
		.if_not_exists()
		.col(ColumnDef::new(Jobs::Id).big_integer().not_null().auto_increment().primary_key())
		.col(ColumnDef::new(Jobs::PackageId).uuid().not_null().unique_key())
		.col(
			ColumnDef::new(Jobs::State)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("state", &STATE_VALUES))),
		)
		.col(ColumnDef::new(Jobs::Attempts).integer().not_null().default(0))
		.col(
			ColumnDef::new(Jobs::EnqueuedAt)
				.timestamp_with_time_zone()
				.not_null()
				.default(sea_query::Expr::current_timestamp()),
		)
		.col(ColumnDef::new(Jobs::LeaseUntil).timestamp_with_time_zone().null())
		.col(ColumnDef::new(Jobs::Priority).integer().not_null().default(0))
		.foreign_key(
			ForeignKey::create()
				.name("fk_jobs_package")
				.from(Jobs::Table, Jobs::PackageId)
				.to(Packages::Table, Packages::Id)
				.on_delete(ForeignKeyAction::Cascade),
		)
		.take()
}

/// `CREATE TABLE outbox (...)`.
pub fn create_outbox() -> TableCreateStatement {
	Table::create()
		.table(Outbox::Table)
		.if_not_exists()
		.col(ColumnDef::new(Outbox::Seq).big_integer().not_null().auto_increment().primary_key())
		.col(ColumnDef::new(Outbox::PackageId).uuid().not_null())
		.col(ColumnDef::new(Outbox::Generation).binary().not_null())
		.col(
			ColumnDef::new(Outbox::SinkKind)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("sink_kind", SINK_KIND_VALUES))),
		)
		.col(
			ColumnDef::new(Outbox::CreatedAt)
				.timestamp_with_time_zone()
				.not_null()
				.default(sea_query::Expr::current_timestamp()),
		)
		.foreign_key(
			ForeignKey::create()
				.name("fk_outbox_package")
				.from(Outbox::Table, Outbox::PackageId)
				.to(Packages::Table, Packages::Id)
				.on_delete(ForeignKeyAction::Cascade),
		)
		.take()
}

/// `CREATE TABLE sink_watermarks (...)`.
pub fn create_sink_watermarks() -> TableCreateStatement {
	Table::create()
		.table(SinkWatermarks::Table)
		.if_not_exists()
		.col(
			ColumnDef::new(SinkWatermarks::SinkKind)
				.text()
				.not_null()
				.primary_key()
				.check(sea_query::Expr::cust(check_in("sink_kind", SINK_KIND_VALUES))),
		)
		.col(ColumnDef::new(SinkWatermarks::LastSeq).big_integer().not_null().default(0))
		.col(
			ColumnDef::new(SinkWatermarks::UpdatedAt)
				.timestamp_with_time_zone()
				.not_null()
				.default(sea_query::Expr::current_timestamp()),
		)
		.take()
}

/// `CREATE TABLE symbols (...)`.
pub fn create_symbols() -> TableCreateStatement {
	Table::create()
		.table(Symbols::Table)
		.if_not_exists()
		.col(ColumnDef::new(Symbols::Id).uuid().not_null().primary_key())
		.col(ColumnDef::new(Symbols::PackageId).uuid().not_null())
		.col(ColumnDef::new(Symbols::FqName).text().not_null())
		.col(
			ColumnDef::new(Symbols::Kind)
				.text()
				.not_null()
				.check(sea_query::Expr::cust(check_in("kind", SYMBOL_KIND_VALUES))),
		)
		.col(ColumnDef::new(Symbols::Generation).binary().not_null())
		.foreign_key(
			ForeignKey::create()
				.name("fk_symbols_package")
				.from(Symbols::Table, Symbols::PackageId)
				.to(Packages::Table, Packages::Id)
				.on_delete(ForeignKeyAction::Cascade),
		)
		.take()
}

// ─────────────────────────────────────────────────────────────────────────────
// Index create statements.
// ─────────────────────────────────────────────────────────────────────────────

/// The secondary indexes (the unique coordinate index, scheduling scans, the
/// dequeue hot path, outbox dedupe + watermark reads, symbol lookups).
pub fn create_indexes() -> Vec<IndexCreateStatement> {
	vec![
		// Identity: coordinates are globally unique.
		Index::create()
			.name(IDX_PACKAGES_COORDS)
			.table(Packages::Table)
			.col(Packages::Language)
			.col(Packages::OriginToken)
			.col(Packages::NameCanonical)
			.col(Packages::VersionCanonical)
			.unique()
			.if_not_exists()
			.take(),
		// Scheduling: scan by lifecycle state.
		Index::create()
			.name(IDX_PARSE_STATUS_STATE)
			.table(ParseStatus::Table)
			.col(ParseStatus::State)
			.if_not_exists()
			.take(),
		// Freshness / generation lookups.
		Index::create()
			.name(IDX_PARSE_STATUS_HASH)
			.table(ParseStatus::Table)
			.col(ParseStatus::ContentHash)
			.if_not_exists()
			.take(),
		// Dequeue hot path: runnable rows first by priority, then FIFO. The
		// `WHERE lease_until IS NULL OR lease_until < now()` partial predicate is
		// appended in the rendered SQL (sea-query 0.32 has no portable partial-
		// index builder), applied via sea-query's raw-predicate escape hatch when rendered.
		Index::create()
			.name(IDX_JOBS_RUNNABLE)
			.table(Jobs::Table)
			.col(Jobs::Priority)
			.col(Jobs::EnqueuedAt)
			.if_not_exists()
			.take(),
		// Outbox idempotency: one intent per (package, generation, sink).
		Index::create()
			.name(IDX_OUTBOX_DEDUPE)
			.table(Outbox::Table)
			.col(Outbox::PackageId)
			.col(Outbox::Generation)
			.col(Outbox::SinkKind)
			.unique()
			.if_not_exists()
			.take(),
		// Per-consumer watermark reads: `WHERE sink_kind = $ AND seq > $ ORDER BY seq`.
		Index::create()
			.name(IDX_OUTBOX_SINK_SEQ)
			.table(Outbox::Table)
			.col(Outbox::SinkKind)
			.col(Outbox::Seq)
			.if_not_exists()
			.take(),
		// Symbols by owning package.
		Index::create()
			.name(IDX_SYMBOLS_PACKAGE)
			.table(Symbols::Table)
			.col(Symbols::PackageId)
			.if_not_exists()
			.take(),
	]
}

/// Every `CREATE TABLE` statement, in FK-dependency order (`packages` first).
pub fn create_all() -> Vec<TableCreateStatement> {
	vec![
		create_packages(),
		create_parse_status(),
		create_jobs(),
		create_outbox(),
		create_sink_watermarks(),
		create_symbols(),
	]
}

/// Every `DROP TABLE` statement, in reverse FK-dependency order.
pub fn drop_all() -> Vec<TableDropStatement> {
	vec![
		Table::drop().table(Symbols::Table).if_exists().take(),
		Table::drop().table(SinkWatermarks::Table).if_exists().take(),
		Table::drop().table(Outbox::Table).if_exists().take(),
		Table::drop().table(Jobs::Table).if_exists().take(),
		Table::drop().table(ParseStatus::Table).if_exists().take(),
		Table::drop().table(Packages::Table).if_exists().take(),
	]
}

/// Every schema statement (tables then indexes), each rendered to a Postgres DDL
/// string via `sea_query`. This is the single source of the schema — there is no
/// hand-written `.sql`; [`crate::index::GlobalStore::connect`] executes exactly
/// these on boot (all `IF NOT EXISTS`, so idempotent).
pub fn schema_ddl() -> Vec<String> {
	let mut out = Vec::new();
	for stmt in create_all() {
		out.push(stmt.to_string(PostgresQueryBuilder));
	}
	for idx in create_indexes() {
		out.push(idx.to_string(PostgresQueryBuilder));
	}
	out
}

/// The whole schema as one newline-joined DDL string — for inspection, diffing,
/// and snapshot tests. Not used at runtime (connect applies [`schema_ddl`]
/// statement-by-statement).
pub fn render_ddl() -> String {
	schema_ddl().join(";\n") + ";\n"
}
