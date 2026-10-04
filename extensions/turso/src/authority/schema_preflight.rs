//! Read-only schema attestation before opening the authority for writes.
//!
//! The expected catalogue comes from executing the real authority DDL through
//! this build's Turso serializer. Existing files are inspected through a
//! read-only handle before the first read/write open.

use super::{AuthorityError, schema::AUTHORITY_SCHEMA, schema::AUTHORITY_SCHEMA_VERSION};
use std::sync::OnceLock;

const AUTHORITY_PREFIX: &str = "backend_index_authority_";
const META_TABLE: &str = "backend_index_authority_meta";
const MAX_SCHEMA_OBJECTS: usize = 64;
const MAX_IDENTIFIER_BYTES: i64 = 256;
const MAX_SQL_BYTES: i64 = 16 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024;

// This inventory intentionally duplicates only object identities, not DDL.
// It makes a missing declaration in AUTHORITY_SCHEMA fail closed while the
// canonical SQL itself continues to come from Turso's serializer.
const REQUIRED_TABLES: [&str; 9] = [
    "backend_index_authority_meta",
    "backend_index_authority_scopes",
    "backend_index_authority_observations",
    "backend_index_authority_attempts",
    "backend_index_authority_attempt_terminals",
    "backend_index_authority_no_result_barriers",
    "backend_index_authority_frontiers",
    "backend_index_authority_generation_history",
    "backend_index_authority_projection_watermarks",
];

const REQUIRED_TRIGGERS: [&str; 4] = [
    "backend_index_authority_attempt_terminal_immutable_update",
    "backend_index_authority_attempt_terminal_immutable_delete",
    "backend_index_authority_generation_history_immutable_update",
    "backend_index_authority_generation_history_immutable_delete",
];

static EXPECTED_SCHEMA: OnceLock<Box<[SchemaObject]>> = OnceLock::new();

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SchemaObject {
    pub(super) kind: Box<str>,
    pub(super) name: Box<str>,
    pub(super) table_name: Box<str>,
    pub(super) sql: Option<Box<str>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DatabaseState {
    Fresh,
    Current,
}

/// Attest a schema without creating or changing anything in the database.
pub(super) async fn classify(
    connection: &turso::Connection,
) -> Result<DatabaseState, AuthorityError> {
    let Some(version) = marker_version(connection).await? else {
        return if has_user_objects(connection).await? {
            Err(AuthorityError::SchemaIntegrity)
        } else {
            Ok(DatabaseState::Fresh)
        };
    };

    // Reject another supported version before generating or collecting the
    // full catalogue. In particular, a future file is never opened writable.
    if version != AUTHORITY_SCHEMA_VERSION {
        return Err(AuthorityError::Schema { found: version });
    }

    let expected = expected_schema().await?;
    let actual = inventory(connection).await?;
    if actual.as_slice() != expected {
        return Err(AuthorityError::SchemaIntegrity);
    }
    Ok(DatabaseState::Current)
}

/// Publish one immutable canonical schema witness without holding the
/// OnceLock while awaiting database work. Racing callers discard their own
/// bounded candidate after the first successful publication.
pub(super) async fn expected_schema() -> Result<&'static [SchemaObject], AuthorityError> {
    if let Some(expected) = EXPECTED_SCHEMA.get() {
        return Ok(expected);
    }

    let candidate = generate_expected_schema().await?;
    let _ = EXPECTED_SCHEMA.set(candidate);
    EXPECTED_SCHEMA
        .get()
        .map(Box::as_ref)
        .ok_or(AuthorityError::SchemaIntegrity)
}

async fn generate_expected_schema() -> Result<Box<[SchemaObject]>, AuthorityError> {
    // Multiprocess WAL is intentionally omitted for this volatile database;
    // Turso pre10 rejects that mode for :memory: databases.
    let database = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .build()
        .await?;
    let mut connection = database.connect()?;
    let transaction = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
        .await?;
    transaction.execute_batch(AUTHORITY_SCHEMA).await?;
    let objects = inventory(&transaction).await?;
    verify_required_identities(&objects)?;
    transaction.commit().await?;
    Ok(objects.into_boxed_slice())
}

async fn marker_version(connection: &turso::Connection) -> Result<Option<i64>, AuthorityError> {
    let mut objects = connection
        .query(
            "SELECT type, name FROM sqlite_schema \
             WHERE name=?1 COLLATE NOCASE LIMIT 2",
            [META_TABLE],
        )
        .await?;
    let Some(object) = objects.next().await? else {
        return Ok(None);
    };
    let kind: String = object.get(0)?;
    let name: String = object.get(1)?;
    let duplicate = objects.next().await?.is_some();
    drop(objects);
    if duplicate || kind != "table" || !name.eq_ignore_ascii_case(META_TABLE) {
        return Err(AuthorityError::SchemaIntegrity);
    }

    // Only two rows are ever copied. Values are returned only after a type
    // check in SQL, so malformed long text/blob values are not cloned here.
    let mut rows = connection
        .query(
            "SELECT typeof(singleton), \
                    CASE WHEN typeof(singleton)='integer' THEN singleton ELSE NULL END, \
                    typeof(schema_version), \
                    CASE WHEN typeof(schema_version)='integer' THEN schema_version ELSE NULL END \
             FROM backend_index_authority_meta LIMIT 2",
            (),
        )
        .await
        .map_err(marker_data_error)?;
    let Some(row) = rows.next().await.map_err(marker_data_error)? else {
        return Err(AuthorityError::SchemaIntegrity);
    };
    let singleton_type: String = row.get(0).map_err(marker_data_error)?;
    let singleton: Option<i64> = row.get(1).map_err(marker_data_error)?;
    let version_type: String = row.get(2).map_err(marker_data_error)?;
    let version: Option<i64> = row.get(3).map_err(marker_data_error)?;
    let duplicate = rows.next().await.map_err(marker_data_error)?.is_some();
    drop(rows);

    if duplicate || singleton_type != "integer" || singleton != Some(1) || version_type != "integer"
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    version.ok_or(AuthorityError::SchemaIntegrity).map(Some)
}

fn marker_data_error(error: turso::Error) -> AuthorityError {
    match error {
        error @ (turso::Error::Busy(_)
        | turso::Error::BusySnapshot(_)
        | turso::Error::Interrupt(_)
        | turso::Error::IoError(_, _)
        | turso::Error::DatabaseFull(_)
        | turso::Error::Readonly(_)
        | turso::Error::NotAdb(_)
        | turso::Error::Corrupt(_)) => AuthorityError::Database(error),
        _ => AuthorityError::SchemaIntegrity,
    }
}

async fn has_user_objects(connection: &turso::Connection) -> Result<bool, AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM sqlite_schema \
             WHERE substr(name, 1, 7) <> 'sqlite_' COLLATE NOCASE LIMIT 1",
            (),
        )
        .await?;
    let has_user_objects = rows.next().await?.is_some();
    drop(rows);
    Ok(has_user_objects)
}

async fn inventory(connection: &turso::Connection) -> Result<Vec<SchemaObject>, AuthorityError> {
    // The CASE expressions prevent Rust-side copies of oversized fields. The
    // separate byte lengths distinguish a genuine SQL NULL autoindex from a
    // value masked because it exceeded its application bound. Turso has
    // already parsed the file's schema while opening it, before these bounds
    // can apply; this is an application copy bound, not a parser resource cap.
    let mut rows = connection
        .query(
            "SELECT \
                CASE WHEN length(CAST(type AS BLOB)) <= 256 THEN type ELSE NULL END, \
                CASE WHEN length(CAST(name AS BLOB)) <= 256 THEN name ELSE NULL END, \
                CASE WHEN length(CAST(tbl_name AS BLOB)) <= 256 THEN tbl_name ELSE NULL END, \
                CASE WHEN sql IS NULL OR length(CAST(sql AS BLOB)) <= 16384 THEN sql ELSE NULL END, \
                length(CAST(type AS BLOB)), \
                length(CAST(name AS BLOB)), \
                length(CAST(tbl_name AS BLOB)), \
                CASE WHEN sql IS NULL THEN -1 ELSE length(CAST(sql AS BLOB)) END \
             FROM sqlite_schema \
             WHERE substr(name, 1, length(?1)) = ?1 COLLATE NOCASE \
                OR substr(tbl_name, 1, length(?1)) = ?1 COLLATE NOCASE \
             LIMIT ?2",
            turso::params![AUTHORITY_PREFIX, (MAX_SCHEMA_OBJECTS + 1) as i64],
        )
        .await?;

    let mut objects = Vec::with_capacity(MAX_SCHEMA_OBJECTS);
    let mut total_bytes = 0usize;
    while let Some(row) = rows.next().await? {
        if objects.len() == MAX_SCHEMA_OBJECTS {
            return Err(AuthorityError::SchemaIntegrity);
        }

        let kind_bytes: i64 = row.get(4)?;
        let name_bytes: i64 = row.get(5)?;
        let table_bytes: i64 = row.get(6)?;
        let sql_bytes: i64 = row.get(7)?;
        if !(0..=MAX_IDENTIFIER_BYTES).contains(&kind_bytes)
            || !(0..=MAX_IDENTIFIER_BYTES).contains(&name_bytes)
            || !(0..=MAX_IDENTIFIER_BYTES).contains(&table_bytes)
            || sql_bytes < -1
            || sql_bytes > MAX_SQL_BYTES
        {
            return Err(AuthorityError::SchemaIntegrity);
        }
        let sql_size =
            usize::try_from(sql_bytes.max(0)).map_err(|_| AuthorityError::SchemaIntegrity)?;
        let object_bytes = usize::try_from(kind_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(usize::try_from(name_bytes).ok()?))
            .and_then(|bytes| bytes.checked_add(usize::try_from(table_bytes).ok()?))
            .and_then(|bytes| bytes.checked_add(sql_size))
            .ok_or(AuthorityError::SchemaIntegrity)?;
        total_bytes = total_bytes
            .checked_add(object_bytes)
            .filter(|bytes| *bytes <= MAX_TOTAL_BYTES)
            .ok_or(AuthorityError::SchemaIntegrity)?;

        let kind = bounded_text(row.get::<Option<String>>(0)?, kind_bytes)?;
        let name = bounded_text(row.get::<Option<String>>(1)?, name_bytes)?;
        let table_name = bounded_text(row.get::<Option<String>>(2)?, table_bytes)?;
        let sql = match (sql_bytes, row.get::<Option<String>>(3)?) {
            (-1, None) => None,
            (length, Some(value)) if length >= 0 && value.len() == sql_size => {
                Some(value.into_boxed_str())
            }
            _ => return Err(AuthorityError::SchemaIntegrity),
        };

        objects.push(SchemaObject {
            kind,
            name,
            table_name,
            sql,
        });
    }
    drop(rows);
    objects.sort_unstable();
    Ok(objects)
}

fn bounded_text(value: Option<String>, expected_bytes: i64) -> Result<Box<str>, AuthorityError> {
    let value = value.ok_or(AuthorityError::SchemaIntegrity)?;
    if value.len()
        != usize::try_from(expected_bytes).map_err(|_| AuthorityError::SchemaIntegrity)?
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    Ok(value.into_boxed_str())
}

fn verify_required_identities(objects: &[SchemaObject]) -> Result<(), AuthorityError> {
    let tables: Vec<&str> = objects
        .iter()
        .filter(|object| object.kind.as_ref() == "table")
        .map(|object| object.name.as_ref())
        .collect();
    let triggers: Vec<&str> = objects
        .iter()
        .filter(|object| object.kind.as_ref() == "trigger")
        .map(|object| object.name.as_ref())
        .collect();

    if tables.len() != REQUIRED_TABLES.len()
        || triggers.len() != REQUIRED_TRIGGERS.len()
        || REQUIRED_TABLES
            .iter()
            .any(|expected| tables.iter().filter(|actual| **actual == *expected).count() != 1)
        || REQUIRED_TRIGGERS.iter().any(|expected| {
            triggers
                .iter()
                .filter(|actual| **actual == *expected)
                .count()
                != 1
        })
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    Ok(())
}
