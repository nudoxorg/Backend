//! Read-only schema attestation before opening the authority for writes.
//!
//! The expected catalogue comes from executing the real authority DDL through
//! this build's Turso serializer. Existing files are inspected through a
//! read-only handle before the first read/write open.

use super::{
    AuthorityError,
    schema::{
        AUTHORITY_DOMAIN_V7_SCHEMA, AUTHORITY_SCHEMA, AUTHORITY_SCHEMA_VERSION,
        AUTHORITY_SCHEMA_VERSION_V7, CSHARP_UNIT_FENCE_V7_SCHEMA,
    },
};
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
static EXPECTED_SCHEMA_V7: OnceLock<Box<[SchemaObject]>> = OnceLock::new();
static EXPECTED_UNIT_SCHEMA_V7: OnceLock<Box<[SchemaObject]>> = OnceLock::new();

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

/// Attest a v7 main-domain file against caller-supplied owner-bound identities.
/// This read-only check is not a workspace lease or migration capability.
pub(super) async fn attest_v7_main(
    connection: &turso::Connection,
    expected_workspace: &[u8; 32],
    expected_domain: &[u8; 32],
) -> Result<(), AuthorityError> {
    attest_v7_domain(
        connection,
        expected_workspace,
        expected_domain,
        0,
        expected_schema_v7().await?,
    )
    .await
}

/// Read-only attestation for the physically separate unit database. The caller
/// must still bind its file and expected IDs to an owner-held workspace lease.
pub(super) async fn attest_v7_unit(
    connection: &turso::Connection,
    expected_workspace: &[u8; 32],
    expected_domain: &[u8; 32],
) -> Result<(), AuthorityError> {
    attest_v7_domain(
        connection,
        expected_workspace,
        expected_domain,
        1,
        expected_unit_schema_v7().await?,
    )
    .await
}

async fn attest_v7_domain(
    connection: &turso::Connection,
    expected_workspace: &[u8; 32],
    expected_domain: &[u8; 32],
    expected_kind: i64,
    expected_schema: &[SchemaObject],
) -> Result<(), AuthorityError> {
    if *expected_workspace == [0; 32] || *expected_domain == [0; 32] {
        return Err(AuthorityError::SchemaIntegrity);
    }
    let version = marker_version(connection)
        .await?
        .ok_or(AuthorityError::SchemaIntegrity)?;
    if version != AUTHORITY_SCHEMA_VERSION_V7 {
        return Err(AuthorityError::Schema { found: version });
    }
    if inventory(connection).await?.as_slice() != expected_schema {
        return Err(AuthorityError::SchemaIntegrity);
    }
    let mut rows = connection.query(
        "SELECT singleton, domain_kind, \
                CASE WHEN typeof(workspace_id)='blob' AND length(workspace_id)=32 THEN workspace_id ELSE NULL END, \
                CASE WHEN typeof(domain_id)='blob' AND length(domain_id)=32 THEN domain_id ELSE NULL END \
         FROM backend_index_authority_domain_v7 LIMIT 2", ()
    ).await?;
    let row = rows.next().await?.ok_or(AuthorityError::SchemaIntegrity)?;
    let singleton: i64 = row.get(0)?;
    let kind: i64 = row.get(1)?;
    let workspace: Option<Vec<u8>> = row.get(2)?;
    let domain: Option<Vec<u8>> = row.get(3)?;
    if rows.next().await?.is_some()
        || singleton != 1
        || kind != expected_kind
        || workspace.as_deref() != Some(expected_workspace.as_slice())
        || domain.as_deref() != Some(expected_domain.as_slice())
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    Ok(())
}

/// Initializes a fresh physically separate C# unit authority in one immediate
/// transaction, or attests the same IDs on retry. This private primitive is not
/// a workspace lease, unit fence, or permission to publish.
pub(super) async fn initialize_v7_unit(
    connection: &mut turso::Connection,
    workspace: &[u8; 32],
    domain: &[u8; 32],
) -> Result<(), AuthorityError> {
    if *workspace == [0; 32] || *domain == [0; 32] {
        return Err(AuthorityError::SchemaIntegrity);
    }
    let tx = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
        .await?;
    let result = async {
        if marker_version(&tx).await?.is_some() { return attest_v7_unit(&tx, workspace, domain).await; }
        if has_user_objects(&tx).await? { return Err(AuthorityError::SchemaIntegrity); }
        tx.execute_batch(AUTHORITY_SCHEMA).await?;
        tx.execute_batch(AUTHORITY_DOMAIN_V7_SCHEMA).await?;
        tx.execute_batch(CSHARP_UNIT_FENCE_V7_SCHEMA).await?;
        tx.execute("INSERT INTO backend_index_authority_meta(singleton, schema_version) VALUES (1, ?1)", [AUTHORITY_SCHEMA_VERSION_V7]).await?;
        tx.execute("INSERT INTO backend_index_authority_domain_v7(singleton, domain_kind, workspace_id, domain_id) VALUES (1, 1, ?1, ?2)",
            turso::params![workspace.as_slice(), domain.as_slice()]).await?;
        attest_v7_unit(&tx, workspace, domain).await
    }.await;
    if let Err(error) = result {
        tx.rollback().await?;
        return Err(error);
    }
    tx.commit().await?;
    Ok(())
}

/// An untrusted proposed mapping. This value alone grants no unit authority;
/// callers must independently admit the unit and hold the owner-bound domain.
pub struct UnitFenceClaim<'a> {
    pub unit_key: &'a [u8; 32],
    pub package: &'a str,
    pub coordinate: &'a str,
    pub profile: &'a str,
    pub project_path: &'a str,
    pub target: &'a [u8; 32],
}

/// Records an immutable mapping in the isolated unit database. The caller
/// must bind the database to the current workspace owner before using this;
/// this low-level transaction does not authenticate the claim or start work.
pub(super) async fn record_unit_fence(
    connection: &mut turso::Connection,
    workspace: &[u8; 32],
    domain: &[u8; 32],
    claim: &UnitFenceClaim<'_>,
) -> Result<(), AuthorityError> {
    if *claim.unit_key == [0; 32]
        || *claim.target == [0; 32]
        || [
            (claim.package, 4096),
            (claim.coordinate, 4096),
            (claim.profile, 256),
            (claim.project_path, 4096),
        ]
        .iter()
        .any(|(s, max)| s.is_empty() || s.len() > *max || s.chars().any(char::is_control))
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    let tx = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
        .await?;
    let result = async {
        attest_v7_unit(&tx, workspace, domain).await?;
        tx.execute(
            "INSERT OR IGNORE INTO backend_index_authority_csharp_unit_fences_v7 (unit_key, package, coordinate, profile, project_path, target) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            turso::params![claim.unit_key.as_slice(), claim.package, claim.coordinate, claim.profile, claim.project_path, claim.target.as_slice()],
        ).await?;
        let mut rows = tx.query(
            "SELECT package, coordinate, profile, project_path, target FROM backend_index_authority_csharp_unit_fences_v7 WHERE unit_key=?1 LIMIT 2",
            turso::params![claim.unit_key.as_slice()],
        ).await?;
        let row = rows.next().await?.ok_or(AuthorityError::SchemaIntegrity)?;
        let package: String = row.get(0)?;
        let coordinate: String = row.get(1)?;
        let profile: String = row.get(2)?;
        let project_path: String = row.get(3)?;
        let target: Vec<u8> = row.get(4)?;
        if rows.next().await?.is_some() || package != claim.package || coordinate != claim.coordinate
            || profile != claim.profile || project_path != claim.project_path || target.as_slice() != claim.target.as_slice() {
            return Err(AuthorityError::SchemaIntegrity);
        }
        Ok(())
    }.await;
    if let Err(error) = result {
        tx.rollback().await?;
        return Err(error);
    }
    tx.commit().await?;
    Ok(())
}

/// Execute the exact v6-to-v7 main-domain schema transition in one immediate
/// transaction. This lower-level primitive is intentionally private: a future
/// engine owner wrapper must retain/recheck its actual workspace lease and
/// bound directory before calling it. It does not initialize a unit database.
pub(super) async fn migrate_v6_main_to_v7(
    connection: &mut turso::Connection,
    workspace: &[u8; 32],
    domain: &[u8; 32],
) -> Result<(), AuthorityError> {
    if *workspace == [0; 32] || *domain == [0; 32] {
        return Err(AuthorityError::SchemaIntegrity);
    }
    let tx = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
        .await?;
    let result = async {
        match marker_version(&tx).await? {
            Some(AUTHORITY_SCHEMA_VERSION_V7) => return attest_v7_main(&tx, workspace, domain).await,
            Some(AUTHORITY_SCHEMA_VERSION) => {
                if classify(&tx).await? != DatabaseState::Current { return Err(AuthorityError::SchemaIntegrity); }
            }
            Some(found) => return Err(AuthorityError::Schema { found }),
            None => return Err(AuthorityError::SchemaIntegrity),
        }
        tx.execute_batch(AUTHORITY_DOMAIN_V7_SCHEMA).await?;
        tx.execute(
            "INSERT INTO backend_index_authority_domain_v7(singleton, domain_kind, workspace_id, domain_id) VALUES (1, 0, ?1, ?2)",
            turso::params![workspace.as_slice(), domain.as_slice()],
        ).await?;
        tx.execute("UPDATE backend_index_authority_meta SET schema_version=?1 WHERE singleton=1 AND schema_version=?2",
            turso::params![AUTHORITY_SCHEMA_VERSION_V7, AUTHORITY_SCHEMA_VERSION]).await?;
        attest_v7_main(&tx, workspace, domain).await
    }.await;
    if let Err(error) = result {
        tx.rollback().await?;
        return Err(error);
    }
    tx.commit().await?;
    Ok(())
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

pub(super) async fn expected_schema_v7() -> Result<&'static [SchemaObject], AuthorityError> {
    if let Some(expected) = EXPECTED_SCHEMA_V7.get() {
        return Ok(expected);
    }
    let database = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .build()
        .await?;
    let mut connection = database.connect()?;
    let tx = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(AUTHORITY_SCHEMA).await?;
    tx.execute_batch(AUTHORITY_DOMAIN_V7_SCHEMA).await?;
    let objects = inventory(&tx).await?;
    let base = expected_schema().await?;
    if !base.iter().all(|entry| objects.contains(entry))
        || !objects.iter().any(|entry| {
            entry.kind.as_ref() == "table"
                && entry.name.as_ref() == "backend_index_authority_domain_v7"
        })
        || !objects.iter().any(|entry| {
            entry.kind.as_ref() == "trigger"
                && entry.name.as_ref() == "backend_index_authority_domain_v7_immutable_update"
        })
        || !objects.iter().any(|entry| {
            entry.kind.as_ref() == "trigger"
                && entry.name.as_ref() == "backend_index_authority_domain_v7_immutable_delete"
        })
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    tx.commit().await?;
    let _ = EXPECTED_SCHEMA_V7.set(objects.into_boxed_slice());
    EXPECTED_SCHEMA_V7
        .get()
        .map(Box::as_ref)
        .ok_or(AuthorityError::SchemaIntegrity)
}

/// Canonical catalogue for the physically separate C# unit database.
pub(super) async fn expected_unit_schema_v7() -> Result<&'static [SchemaObject], AuthorityError> {
    if let Some(expected) = EXPECTED_UNIT_SCHEMA_V7.get() {
        return Ok(expected);
    }
    let database = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .build()
        .await?;
    let mut connection = database.connect()?;
    let tx = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(AUTHORITY_SCHEMA).await?;
    tx.execute_batch(AUTHORITY_DOMAIN_V7_SCHEMA).await?;
    tx.execute_batch(CSHARP_UNIT_FENCE_V7_SCHEMA).await?;
    let objects = inventory(&tx).await?;
    let base = expected_schema_v7().await?;
    if !base.iter().all(|entry| objects.contains(entry))
        || !objects.iter().any(|entry| {
            entry.kind.as_ref() == "table"
                && entry.name.as_ref() == "backend_index_authority_csharp_unit_fences_v7"
        })
        || !objects.iter().any(|entry| {
            entry.kind.as_ref() == "trigger"
                && entry.name.as_ref()
                    == "backend_index_authority_csharp_unit_fences_v7_immutable_update"
        })
        || !objects.iter().any(|entry| {
            entry.kind.as_ref() == "trigger"
                && entry.name.as_ref()
                    == "backend_index_authority_csharp_unit_fences_v7_immutable_delete"
        })
    {
        return Err(AuthorityError::SchemaIntegrity);
    }
    tx.commit().await?;
    let _ = EXPECTED_UNIT_SCHEMA_V7.set(objects.into_boxed_slice());
    EXPECTED_UNIT_SCHEMA_V7
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

#[cfg(test)]
mod v7_tests {
    use super::*;

    #[test]
    fn v7_main_migration_is_atomic_idempotent_and_domain_bound() {
        futures_executor::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_index_method(true)
                .build()
                .await
                .expect("memory db");
            let mut connection = database.connect().expect("connection");
            connection
                .execute_batch(AUTHORITY_SCHEMA)
                .await
                .expect("v6 schema");
            connection.execute(
                "INSERT INTO backend_index_authority_meta(singleton, schema_version) VALUES (1, ?1)",
                [AUTHORITY_SCHEMA_VERSION],
            ).await.expect("v6 marker");
            assert_eq!(
                classify(&connection).await.expect("v6 preflight"),
                DatabaseState::Current
            );
            let workspace = [1; 32];
            let domain = [2; 32];
            migrate_v6_main_to_v7(&mut connection, &workspace, &domain)
                .await
                .expect("v7 migration");
            attest_v7_main(&connection, &workspace, &domain)
                .await
                .expect("v7 domain");
            migrate_v6_main_to_v7(&mut connection, &workspace, &domain)
                .await
                .expect("idempotent migration");
            assert!(
                attest_v7_main(&connection, &[3; 32], &domain)
                    .await
                    .is_err()
            );
            assert!(
                attest_v7_main(&connection, &workspace, &[3; 32])
                    .await
                    .is_err()
            );
            assert!(matches!(
                classify(&connection).await,
                Err(AuthorityError::Schema { found: 7 })
            ));
            assert!(
                connection
                    .execute(
                        "UPDATE backend_index_authority_domain_v7 SET domain_kind=1",
                        ()
                    )
                    .await
                    .is_err()
            );
            assert!(
                connection
                    .execute("DELETE FROM backend_index_authority_domain_v7", ())
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn unit_domain_is_physically_distinct_and_rejects_foreign_ids() {
        futures_executor::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_index_method(true)
                .build()
                .await
                .expect("memory db");
            let mut connection = database.connect().expect("connection");
            let workspace = [11; 32];
            let domain = [12; 32];
            initialize_v7_unit(&mut connection, &workspace, &domain)
                .await
                .expect("unit init");
            attest_v7_unit(&connection, &workspace, &domain)
                .await
                .expect("unit attestation");
            initialize_v7_unit(&mut connection, &workspace, &domain)
                .await
                .expect("same unit retry");
            assert!(
                attest_v7_unit(&connection, &[13; 32], &domain)
                    .await
                    .is_err()
            );
            assert!(
                attest_v7_unit(&connection, &workspace, &[13; 32])
                    .await
                    .is_err()
            );
            assert!(
                attest_v7_main(&connection, &workspace, &domain)
                    .await
                    .is_err()
            );
            assert!(matches!(
                classify(&connection).await,
                Err(AuthorityError::Schema { found: 7 })
            ));
            let table = "backend_index_authority_csharp_unit_fences_v7";
            for values in [
                "('12345678901234567890123456789012','p','c','CSharp','x',zeroblob(32))",
                "(zeroblob(32),x'70','c','CSharp','x',zeroblob(32))",
                "(zeroblob(32),'p',x'63','CSharp','x',zeroblob(32))",
                "(zeroblob(32),'p','c',x'43','x',zeroblob(32))",
                "(zeroblob(32),'p','c','CSharp',x'78',zeroblob(32))",
                "(zeroblob(32),'p','c','CSharp','x','12345678901234567890123456789012')",
            ] {
                assert!(
                    connection
                        .execute(&format!("INSERT INTO {table} VALUES {values}"), ())
                        .await
                        .is_err()
                );
            }
        });
    }

    #[test]
    fn unit_fence_is_idempotent_and_conflicts_refuse() {
        futures_executor::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_index_method(true)
                .build()
                .await
                .expect("memory db");
            let mut connection = database.connect().expect("connection");
            let workspace = [11; 32];
            let domain = [12; 32];
            initialize_v7_unit(&mut connection, &workspace, &domain)
                .await
                .expect("unit init");
            let key = [13; 32];
            let target = [14; 32];
            let claim = UnitFenceClaim {
                unit_key: &key,
                package: "pkg",
                coordinate: "pkg:nuget/demo@1",
                profile: "CSharp",
                project_path: "src/App.csproj",
                target: &target,
            };
            record_unit_fence(&mut connection, &workspace, &domain, &claim)
                .await
                .expect("first fence");
            record_unit_fence(&mut connection, &workspace, &domain, &claim)
                .await
                .expect("idempotent fence");
            let wrong_target = [15; 32];
            let conflicting = UnitFenceClaim {
                target: &wrong_target,
                ..claim
            };
            assert!(
                record_unit_fence(&mut connection, &workspace, &domain, &conflicting)
                    .await
                    .is_err()
            );
            let other_key = [16; 32];
            let duplicate_scope = UnitFenceClaim {
                unit_key: &other_key,
                target: &target,
                ..conflicting
            };
            assert!(
                record_unit_fence(&mut connection, &workspace, &domain, &duplicate_scope)
                    .await
                    .is_err()
            );
            assert!(
                record_unit_fence(&mut connection, &[99; 32], &domain, &duplicate_scope)
                    .await
                    .is_err()
            );
            let mut rows = connection
                .query(
                    "SELECT count(*) FROM backend_index_authority_csharp_unit_fences_v7",
                    (),
                )
                .await
                .expect("count");
            let row = rows.next().await.expect("query").expect("count row");
            assert_eq!(row.get::<i64>(0).expect("count value"), 1);
        });
    }

    #[test]
    fn unit_partial_ddl_transaction_rolls_back_to_empty() {
        futures_executor::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_index_method(true)
                .build()
                .await
                .expect("memory db");
            let mut connection = database.connect().expect("connection");
            let tx = connection
                .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
                .await
                .expect("unit transaction");
            tx.execute_batch(AUTHORITY_SCHEMA)
                .await
                .expect("base schema");
            tx.execute_batch(AUTHORITY_DOMAIN_V7_SCHEMA)
                .await
                .expect("domain schema");
            tx.execute_batch(CSHARP_UNIT_FENCE_V7_SCHEMA)
                .await
                .expect("unit schema");
            tx.rollback()
                .await
                .expect("simulated interruption rollback");
            assert!(
                !has_user_objects(&connection)
                    .await
                    .expect("empty inventory")
            );
            initialize_v7_unit(&mut connection, &[11; 32], &[12; 32])
                .await
                .expect("retry after rollback");
        });
    }

    #[test]
    fn v7_partial_ddl_transaction_rolls_back_to_attested_v6() {
        futures_executor::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_index_method(true)
                .build()
                .await
                .expect("memory db");
            let mut connection = database.connect().expect("connection");
            connection
                .execute_batch(AUTHORITY_SCHEMA)
                .await
                .expect("v6 schema");
            connection.execute(
                "INSERT INTO backend_index_authority_meta(singleton, schema_version) VALUES (1, ?1)",
                [AUTHORITY_SCHEMA_VERSION],
            ).await.expect("v6 marker");
            let tx = connection
                .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
                .await
                .expect("migration transaction");
            tx.execute_batch(AUTHORITY_DOMAIN_V7_SCHEMA)
                .await
                .expect("new marker schema");
            tx.execute(
                "UPDATE backend_index_authority_meta SET schema_version=7 WHERE singleton=1",
                (),
            )
            .await
            .expect("staged version");
            tx.rollback()
                .await
                .expect("simulated interruption rollback");
            assert_eq!(
                classify(&connection).await.expect("v6 preflight"),
                DatabaseState::Current
            );
            assert_eq!(
                marker_version(&connection).await.expect("version"),
                Some(AUTHORITY_SCHEMA_VERSION)
            );
            let mut rows = connection
                .query(
                    "SELECT name FROM sqlite_schema WHERE name='backend_index_authority_domain_v7'",
                    (),
                )
                .await
                .expect("lookup marker");
            assert!(rows.next().await.expect("row").is_none());
        });
    }

    #[test]
    fn v7_migration_refuses_malformed_v6_without_mutation() {
        futures_executor::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_index_method(true)
                .build()
                .await
                .expect("memory db");
            let mut connection = database.connect().expect("connection");
            connection
                .execute_batch(AUTHORITY_SCHEMA)
                .await
                .expect("v6 schema");
            connection.execute(
                "INSERT INTO backend_index_authority_meta(singleton, schema_version) VALUES (1, ?1)",
                [AUTHORITY_SCHEMA_VERSION],
            ).await.expect("v6 marker");
            connection
                .execute_batch("CREATE TABLE backend_index_authority_unexpected(x INTEGER);")
                .await
                .expect("corrupt catalogue");
            assert!(
                migrate_v6_main_to_v7(&mut connection, &[1; 32], &[2; 32])
                    .await
                    .is_err()
            );
            assert_eq!(
                marker_version(&connection).await.expect("marker"),
                Some(AUTHORITY_SCHEMA_VERSION)
            );
            let mut rows = connection
                .query(
                    "SELECT name FROM sqlite_schema WHERE name='backend_index_authority_domain_v7'",
                    (),
                )
                .await
                .expect("lookup marker");
            assert!(rows.next().await.expect("row").is_none());
        });
    }
}
