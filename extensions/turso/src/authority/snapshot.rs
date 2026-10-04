//! Same-engine, single-file snapshots of the live index authority.
//!
//! `VACUUM INTO` runs through the authority's existing Turso connection. The
//! returned snapshot covers only `index-authority.turso`; it does not bind the
//! disposable projection, Registry Discovery journals, or a shared catalog
//! frontier. Failed attempts retain their private staging directory and exact
//! [`CreatedDirectory`] receipt so callers can inspect the incomplete artifact
//! without the snapshot code deleting a name that may have been substituted.

use super::{
    AuthorityError, TursoAuthority,
    schema_preflight::{self, DatabaseState},
};
use crate::{connection::BUSY_TIMEOUT, sharing::SharedWalBackend};
use backend_platform::{
    CreatedDirectory, DirectoryCapability, DirectoryCreateFailure, FileIdentity,
};
use std::{
    fmt,
    future::{Future, poll_fn},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
    time::{SystemTime, UNIX_EPOCH},
};

const SNAPSHOT_FILE_NAME: &str = "authority.turso";
const MAX_DESTINATION_SQL_BYTES: usize = 32 * 1024;
const SNAPSHOT_SQL_FIXED_BYTES: usize = "VACUUM INTO ''".len();
const SNAPSHOT_HASH_BUFFER_BYTES: usize = 64 * 1024;
const MAX_STAGE_NAME_ATTEMPTS: usize = 8;
static NEXT_STAGE_NAME: AtomicU64 = AtomicU64::new(0);

/// Admission limit for one compacted authority database snapshot.
///
/// The source's current page-count estimate must fit this limit before an
/// output is created. While Turso executes `VACUUM INTO`, the output is
/// checked whenever its async operation yields and again before admission.
/// The caller must still provision free space: the engine writes directly to
/// the filesystem and this API has no portable filesystem quota capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthoritySnapshotBudget {
    max_output_bytes: u64,
}

impl AuthoritySnapshotBudget {
    /// Creates a positive output-size admission limit.
    pub fn new(max_output_bytes: u64) -> Result<Self, AuthoritySnapshotError> {
        if max_output_bytes == 0 {
            return Err(AuthoritySnapshotError::InvalidBudget);
        }
        Ok(Self { max_output_bytes })
    }

    /// Maximum accepted bytes in the completed main database file.
    #[must_use]
    pub const fn max_output_bytes(self) -> u64 {
        self.max_output_bytes
    }
}

/// Exact file facts admitted after a same-engine read-only reopen.
///
/// This receipt names the snapshot file only. It carries no Registry Discovery
/// root, journal offset, source progress, or cross-database consistency claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthoritySnapshotReceipt {
    file_identity: FileIdentity,
    bytes: u64,
    digest: [u8; 32],
    schema_version: i64,
}

impl AuthoritySnapshotReceipt {
    /// Stable identity of the admitted output file.
    #[must_use]
    pub const fn file_identity(self) -> FileIdentity {
        self.file_identity
    }

    /// Exact output size in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    /// BLAKE3 digest of the admitted main database file.
    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }

    /// Authority schema version verified through the read-only connection.
    #[must_use]
    pub const fn schema_version(self) -> i64 {
        self.schema_version
    }
}

/// A complete, privately staged, same-engine snapshot of the authority file.
///
/// The object retains its exact staging-directory receipt. Dropping it does
/// not delete the snapshot. Callers may reopen [`Self::path`] while retaining
/// this value and can check that the recorded names still resolve through
/// [`Self::verify_named`].
#[derive(Debug)]
pub struct TursoAuthoritySnapshot {
    staging: CreatedDirectory,
    staging_path: PathBuf,
    path: PathBuf,
    receipt: AuthoritySnapshotReceipt,
}

impl TursoAuthoritySnapshot {
    /// Path to the self-contained main database snapshot.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Admitted file size, identity, digest, and schema version.
    #[must_use]
    pub const fn receipt(&self) -> AuthoritySnapshotReceipt {
        self.receipt
    }

    /// Receipt for the exclusive private staging directory.
    #[must_use]
    pub const fn staging_receipt(&self) -> &CreatedDirectory {
        &self.staging
    }

    /// Checks that the retained staging directory and file still have their
    /// admitted identities at the recorded names.
    pub fn verify_named(&self) -> Result<(), AuthoritySnapshotError> {
        verify_snapshot_names(
            &self.staging,
            &self.staging_path,
            &self.path,
            Some(self.receipt.file_identity),
        )
    }
}

/// A private output directory retained after an unverified or failed attempt.
///
/// This is not a usable snapshot. Its optional file identity only records the
/// regular file found at the intended output name when the error was returned;
/// it does not prove that Turso created the file or that it is valid. The
/// receipt is retained so later inspection or cleanup can verify the exact
/// staging directory first. No cleanup runs implicitly.
#[derive(Debug)]
pub struct IncompleteAuthoritySnapshot {
    staging: CreatedDirectory,
    staging_path: PathBuf,
    path: PathBuf,
    output_file_identity: Option<FileIdentity>,
    output_file_bytes: Option<u64>,
}

impl IncompleteAuthoritySnapshot {
    /// Intended output path, which may be absent or incomplete.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Exact private staging-directory receipt retained by the failed attempt.
    #[must_use]
    pub const fn staging_receipt(&self) -> &CreatedDirectory {
        &self.staging
    }

    /// Identity observed through a no-follow file handle, if a regular file
    /// existed at the intended output name when the failure was returned.
    #[must_use]
    pub const fn output_file_identity(&self) -> Option<FileIdentity> {
        self.output_file_identity
    }

    /// Size observed through the same partial-file handle, if present.
    #[must_use]
    pub const fn output_file_bytes(&self) -> Option<u64> {
        self.output_file_bytes
    }

    /// Verifies the retained directory receipt and, when captured, the exact
    /// partial-file identity. This does not claim that the partial file is
    /// valid or safe to open as a database.
    pub fn verify_named(&self) -> Result<(), AuthoritySnapshotError> {
        verify_snapshot_names(
            &self.staging,
            &self.staging_path,
            &self.path,
            self.output_file_identity,
        )
    }
}

/// Why snapshot creation or admission failed.
#[derive(Debug)]
pub enum AuthoritySnapshotError {
    /// An owner database operation or read-only admission check failed.
    Owner(AuthorityError),
    /// A platform filesystem operation failed.
    Filesystem(io::Error),
    /// The supplied output budget is zero.
    InvalidBudget,
    /// The output root is not a private absolute directory matching its handle.
    InvalidDestinationRoot,
    /// The UTF-8 path cannot be safely represented in the bounded SQL statement.
    InvalidDestinationPath,
    /// The source database's current page estimate exceeds the output budget.
    SourceExceedsBudget { estimate: u64, limit: u64 },
    /// The source page-count estimate could not be represented.
    PageEstimateOverflow,
    /// The output exceeded the requested limit during execution or admission.
    OutputExceedsBudget { observed: u64, limit: u64 },
    /// A unique private staging directory could not be allocated after bounded retries.
    StageNameExhausted,
    /// The staging directory was created but could not be pinned by the platform.
    /// Its pathname is informational only; this error carries no cleanup receipt.
    StageUnpinned { path: PathBuf, source: io::Error },
    /// The Turso operation returned successfully but created no regular output file.
    OutputMissing,
    /// Output name or identity changed during verification.
    OutputIdentityChanged,
    /// The read-only same-engine integrity check did not return exactly `ok`.
    IntegrityCheckFailed,
}

impl fmt::Display for AuthoritySnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner(error) => error.fmt(formatter),
            Self::Filesystem(error) => write!(formatter, "snapshot filesystem operation failed: {error}"),
            Self::InvalidBudget => formatter.write_str("snapshot output budget must be positive"),
            Self::InvalidDestinationRoot => formatter.write_str(
                "snapshot destination root must be an absolute, private directory matching its capability",
            ),
            Self::InvalidDestinationPath => {
                formatter.write_str("snapshot destination path is not a bounded UTF-8 path")
            }
            Self::SourceExceedsBudget { estimate, limit } => write!(
                formatter,
                "authority source estimate {estimate} bytes exceeds snapshot limit {limit} bytes"
            ),
            Self::PageEstimateOverflow => {
                formatter.write_str("authority source page estimate overflowed")
            }
            Self::OutputExceedsBudget { observed, limit } => write!(
                formatter,
                "authority snapshot output reached {observed} bytes above limit {limit} bytes"
            ),
            Self::StageNameExhausted => {
                formatter.write_str("could not allocate a unique private snapshot staging directory")
            }
            Self::StageUnpinned { path, source } => write!(
                formatter,
                "snapshot staging directory could not be pinned at {}: {source}",
                path.display()
            ),
            Self::OutputMissing => formatter.write_str("VACUUM INTO produced no regular output file"),
            Self::OutputIdentityChanged => {
                formatter.write_str("snapshot output identity changed during verification")
            }
            Self::IntegrityCheckFailed => {
                formatter.write_str("snapshot failed read-only schema or integrity admission")
            }
        }
    }
}

impl std::error::Error for AuthoritySnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Owner(error) => Some(error),
            Self::Filesystem(error) => Some(error),
            Self::StageUnpinned { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Failure with an optional retained incomplete artifact receipt.
#[derive(Debug)]
pub struct AuthoritySnapshotFailure {
    error: AuthoritySnapshotError,
    incomplete: Option<IncompleteAuthoritySnapshot>,
}

impl AuthoritySnapshotFailure {
    /// Cause of the failed attempt or admission.
    #[must_use]
    pub const fn error(&self) -> &AuthoritySnapshotError {
        &self.error
    }

    /// Incomplete artifact and exact staging receipt, when staging had been
    /// pinned before the failure. It must not be treated as a valid database.
    #[must_use]
    pub const fn incomplete(&self) -> Option<&IncompleteAuthoritySnapshot> {
        self.incomplete.as_ref()
    }

    /// Transfers the incomplete artifact receipt to the caller for later
    /// inspection or verified cleanup.
    #[must_use]
    pub fn into_incomplete(self) -> Option<IncompleteAuthoritySnapshot> {
        self.incomplete
    }
}

impl fmt::Display for AuthoritySnapshotFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for AuthoritySnapshotFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl TursoAuthority {
    /// Creates a same-engine, single-file snapshot through this process-owned
    /// authority connection.
    ///
    /// The output is built with Turso `VACUUM INTO` while the owner and any
    /// other multiprocess-WAL clients remain open. The destination root must
    /// already be an owner-private directory, supplied both as a pinned
    /// capability and its absolute path. This method creates a unique private
    /// child staging directory and never overwrites or deletes an output name.
    /// On error after staging, [`AuthoritySnapshotFailure::incomplete`] retains
    /// the exact [`CreatedDirectory`] receipt. A complete return requires a
    /// bounded output, successful schema preflight and `PRAGMA integrity_check`
    /// on a new read-only connection from this same Turso build.
    ///
    /// The returned object covers only the authority database. It does not
    /// capture projection files or Registry Discovery journals and makes no
    /// claim about a shared catalog frontier.
    pub async fn snapshot(
        &mut self,
        destination_root: &DirectoryCapability,
        destination_root_path: &Path,
        budget: AuthoritySnapshotBudget,
    ) -> Result<TursoAuthoritySnapshot, AuthoritySnapshotFailure> {
        create_snapshot(self, destination_root, destination_root_path, budget).await
    }
}

async fn create_snapshot(
    authority: &mut TursoAuthority,
    root: &DirectoryCapability,
    root_path: &Path,
    budget: AuthoritySnapshotBudget,
) -> Result<TursoAuthoritySnapshot, AuthoritySnapshotFailure> {
    if !root_path.is_absolute() {
        return Err(failure(
            AuthoritySnapshotError::InvalidDestinationRoot,
            None,
        ));
    }
    if let Err(error) = root.validate_private() {
        return Err(failure(AuthoritySnapshotError::Filesystem(error), None));
    }
    if let Err(error) = root.verify_path(root_path) {
        return Err(failure(AuthoritySnapshotError::Filesystem(error), None));
    }

    let source_estimate = match database_page_estimate(&authority.connection).await {
        Ok(estimate) => estimate,
        Err(error) => return Err(failure(error, None)),
    };
    if source_estimate > budget.max_output_bytes {
        return Err(failure(
            AuthoritySnapshotError::SourceExceedsBudget {
                estimate: source_estimate,
                limit: budget.max_output_bytes,
            },
            None,
        ));
    }

    let (staging, staging_path) = match create_staging(root, root_path) {
        Ok(staging) => staging,
        Err(error) => return Err(error),
    };
    let path = staging_path.join(SNAPSHOT_FILE_NAME);
    let path_text = match snapshot_sql_path(&path) {
        Ok(path) => path,
        Err(error) => {
            return Err(failure(
                error,
                Some(capture_incomplete(staging, staging_path, path)),
            ));
        }
    };

    let attempt = async {
        staging
            .verify_named()
            .map_err(AuthoritySnapshotError::Filesystem)?;
        staging
            .verify_path(&staging_path)
            .map_err(AuthoritySnapshotError::Filesystem)?;
        staging
            .capability()
            .validate_private()
            .map_err(AuthoritySnapshotError::Filesystem)?;
        let entries = staging
            .capability()
            .entries(1)
            .map_err(AuthoritySnapshotError::Filesystem)?;
        if !entries.is_empty() {
            return Err(AuthoritySnapshotError::OutputIdentityChanged);
        }

        let sql = format!("VACUUM INTO '{path_text}'");
        let execution = execute_bounded(
            &authority.connection,
            &sql,
            staging.capability(),
            budget.max_output_bytes,
        )
        .await?;
        let receipt = verify_output(&staging, &staging_path, &path, budget, execution).await?;
        Ok::<AuthoritySnapshotReceipt, AuthoritySnapshotError>(receipt)
    }
    .await;

    match attempt {
        Ok(receipt) => Ok(TursoAuthoritySnapshot {
            staging,
            staging_path,
            path,
            receipt,
        }),
        Err(error) => Err(failure(
            error,
            Some(capture_incomplete(staging, staging_path, path)),
        )),
    }
}

fn create_staging(
    root: &DirectoryCapability,
    root_path: &Path,
) -> Result<(CreatedDirectory, PathBuf), AuthoritySnapshotFailure> {
    for _ in 0..MAX_STAGE_NAME_ATTEMPTS {
        let name = staging_name();
        let path = root_path.join(&name);
        match root.create_private_dir_tracked(&name) {
            Ok(staging) => return Ok((staging, path)),
            Err(DirectoryCreateFailure::NotCreated(error))
                if error.kind() == io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(DirectoryCreateFailure::NotCreated(error)) => {
                return Err(failure(AuthoritySnapshotError::Filesystem(error), None));
            }
            Err(DirectoryCreateFailure::CreatedButUnready { directory, source }) => {
                return Err(failure(
                    AuthoritySnapshotError::Filesystem(source),
                    Some(capture_incomplete(
                        directory,
                        path.clone(),
                        path.join(SNAPSHOT_FILE_NAME),
                    )),
                ));
            }
            Err(DirectoryCreateFailure::CreatedButUnpinned(source)) => {
                return Err(failure(
                    AuthoritySnapshotError::StageUnpinned { path, source },
                    None,
                ));
            }
        }
    }
    Err(failure(AuthoritySnapshotError::StageNameExhausted, None))
}

fn staging_name() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    let sequence = NEXT_STAGE_NAME.fetch_add(1, Ordering::Relaxed);
    format!(
        ".authority-snapshot-{}-{nonce:032x}-{sequence:016x}",
        std::process::id()
    )
}

fn snapshot_sql_path(path: &Path) -> Result<String, AuthoritySnapshotError> {
    let text = path.to_str().ok_or_else(|| {
        AuthoritySnapshotError::Owner(AuthorityError::NonUtf8Path(path.to_owned()))
    })?;
    if text.contains('\0') {
        return Err(AuthoritySnapshotError::InvalidDestinationPath);
    }
    let escaped_bytes = text.bytes().try_fold(0_usize, |length, byte| {
        length.checked_add(if byte == b'\'' { 2 } else { 1 })
    });
    let Some(escaped_bytes) = escaped_bytes else {
        return Err(AuthoritySnapshotError::InvalidDestinationPath);
    };
    if escaped_bytes > MAX_DESTINATION_SQL_BYTES - SNAPSHOT_SQL_FIXED_BYTES {
        return Err(AuthoritySnapshotError::InvalidDestinationPath);
    }
    let mut escaped = String::with_capacity(escaped_bytes);
    for character in text.chars() {
        escaped.push(character);
        if character == '\'' {
            escaped.push('\'');
        }
    }
    Ok(escaped)
}

async fn database_page_estimate(
    connection: &turso::Connection,
) -> Result<u64, AuthoritySnapshotError> {
    let page_count = pragma_i64(connection, "PRAGMA page_count").await?;
    let page_size = pragma_i64(connection, "PRAGMA page_size").await?;
    page_count
        .checked_mul(page_size)
        .ok_or(AuthoritySnapshotError::PageEstimateOverflow)
}

async fn pragma_i64(
    connection: &turso::Connection,
    pragma: &str,
) -> Result<u64, AuthoritySnapshotError> {
    let mut rows = connection
        .query(pragma, ())
        .await
        .map_err(|error| owner_error(error.into()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| owner_error(error.into()))?
        .ok_or(AuthoritySnapshotError::IntegrityCheckFailed)?;
    let value: i64 = row.get(0).map_err(|error| owner_error(error.into()))?;
    let extra = rows
        .next()
        .await
        .map_err(|error| owner_error(error.into()))?
        .is_some();
    drop(rows);
    if value <= 0 || extra {
        return Err(AuthoritySnapshotError::IntegrityCheckFailed);
    }
    u64::try_from(value).map_err(|_| AuthoritySnapshotError::IntegrityCheckFailed)
}

enum BoundedExecution {
    Complete,
    Exceeded(u64),
}

enum ExecutionPoll {
    Finished(Result<u64, turso::Error>),
    Exceeded(u64),
    Filesystem(io::Error),
}

async fn execute_bounded(
    connection: &turso::Connection,
    sql: &str,
    destination: &DirectoryCapability,
    max_output_bytes: u64,
) -> Result<BoundedExecution, AuthoritySnapshotError> {
    let mut execution = Box::pin(connection.execute(sql, ()));
    let result = poll_fn(|context| match execution.as_mut().poll(context) {
        Poll::Ready(result) => Poll::Ready(ExecutionPoll::Finished(result)),
        Poll::Pending => match optional_file_bytes(destination, SNAPSHOT_FILE_NAME) {
            Ok(Some(bytes)) if bytes > max_output_bytes => {
                Poll::Ready(ExecutionPoll::Exceeded(bytes))
            }
            Ok(_) => Poll::Pending,
            Err(error) => Poll::Ready(ExecutionPoll::Filesystem(error)),
        },
    })
    .await;
    // Dropping a pending Turso statement invokes the fork's statement-abandon
    // cleanup path, which rolls back VACUUM INTO's source transaction and
    // closes its target handle. The incomplete file remains receipt-backed.
    drop(execution);

    match result {
        ExecutionPoll::Finished(Ok(_)) => {
            match optional_file_bytes(destination, SNAPSHOT_FILE_NAME)
                .map_err(AuthoritySnapshotError::Filesystem)?
            {
                Some(bytes) if bytes <= max_output_bytes => Ok(BoundedExecution::Complete),
                Some(bytes) => Ok(BoundedExecution::Exceeded(bytes)),
                None => Err(AuthoritySnapshotError::OutputMissing),
            }
        }
        ExecutionPoll::Finished(Err(error)) => Err(owner_error(error.into())),
        ExecutionPoll::Exceeded(bytes) => Ok(BoundedExecution::Exceeded(bytes)),
        ExecutionPoll::Filesystem(error) => Err(AuthoritySnapshotError::Filesystem(error)),
    }
}

fn optional_file_bytes(directory: &DirectoryCapability, name: &str) -> io::Result<Option<u64>> {
    match directory.open_file_read(name) {
        Ok(file) => Ok(Some(file.metadata()?.len())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn verify_output(
    staging: &CreatedDirectory,
    staging_path: &Path,
    path: &Path,
    budget: AuthoritySnapshotBudget,
    execution: BoundedExecution,
) -> Result<AuthoritySnapshotReceipt, AuthoritySnapshotError> {
    if let BoundedExecution::Exceeded(observed) = execution {
        return Err(AuthoritySnapshotError::OutputExceedsBudget {
            observed,
            limit: budget.max_output_bytes,
        });
    }
    verify_snapshot_names(staging, staging_path, path, None)?;

    let directory = staging.capability();
    let file = directory
        .open_file_read_write(SNAPSHOT_FILE_NAME, false)
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => AuthoritySnapshotError::OutputMissing,
            _ => AuthoritySnapshotError::Filesystem(error),
        })?;
    let file_identity = FileIdentity::of_file(&file).map_err(AuthoritySnapshotError::Filesystem)?;

    // The database is already in an owner-private directory. Narrow the file's
    // mode as well before admitting it. On Windows the private open below
    // validates the inherited protected ACL through the platform capability.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(AuthoritySnapshotError::Filesystem)?;
    }
    let byte_count = file
        .metadata()
        .map_err(AuthoritySnapshotError::Filesystem)?
        .len();
    if byte_count > budget.max_output_bytes {
        return Err(AuthoritySnapshotError::OutputExceedsBudget {
            observed: byte_count,
            limit: budget.max_output_bytes,
        });
    }
    file.sync_all()
        .map_err(AuthoritySnapshotError::Filesystem)?;
    directory
        .sync_all()
        .map_err(AuthoritySnapshotError::Filesystem)?;
    drop(file);

    let private_file = directory
        .open_private_file(SNAPSHOT_FILE_NAME)
        .map_err(AuthoritySnapshotError::Filesystem)?;
    let private_identity =
        FileIdentity::of_file(&private_file).map_err(AuthoritySnapshotError::Filesystem)?;
    if private_identity != file_identity {
        return Err(AuthoritySnapshotError::OutputIdentityChanged);
    }
    drop(private_file);

    verify_read_only(path).await?;

    verify_snapshot_names(staging, staging_path, path, Some(file_identity))?;
    let (digest, bytes) = digest_private_file(directory, file_identity, budget.max_output_bytes)?;
    if bytes != byte_count {
        return Err(AuthoritySnapshotError::OutputIdentityChanged);
    }
    verify_snapshot_names(staging, staging_path, path, Some(file_identity))?;

    Ok(AuthoritySnapshotReceipt {
        file_identity,
        bytes,
        digest,
        schema_version: super::schema::AUTHORITY_SCHEMA_VERSION,
    })
}

fn digest_private_file(
    directory: &DirectoryCapability,
    expected_identity: FileIdentity,
    maximum: u64,
) -> Result<([u8; 32], u64), AuthoritySnapshotError> {
    let mut file = directory
        .open_private_file(SNAPSHOT_FILE_NAME)
        .map_err(AuthoritySnapshotError::Filesystem)?;
    if FileIdentity::of_file(&file).map_err(AuthoritySnapshotError::Filesystem)?
        != expected_identity
    {
        return Err(AuthoritySnapshotError::OutputIdentityChanged);
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; SNAPSHOT_HASH_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(AuthoritySnapshotError::Filesystem)?;
        if read == 0 {
            break;
        }
        let read =
            u64::try_from(read).map_err(|_| AuthoritySnapshotError::OutputExceedsBudget {
                observed: u64::MAX,
                limit: maximum,
            })?;
        total = total
            .checked_add(read)
            .filter(|total| *total <= maximum)
            .ok_or(AuthoritySnapshotError::OutputExceedsBudget {
                observed: u64::MAX,
                limit: maximum,
            })?;
        hasher.update(&buffer[..read]);
    }
    Ok((*hasher.finalize().as_bytes(), total))
}

async fn verify_read_only(path: &Path) -> Result<(), AuthoritySnapshotError> {
    let text = path.to_str().ok_or_else(|| {
        AuthoritySnapshotError::Owner(AuthorityError::NonUtf8Path(path.to_owned()))
    })?;
    let backend =
        SharedWalBackend::detect().map_err(|error| owner_error(AuthorityError::Sharing(error)))?;
    let database = backend
        .open_database(
            backend
                .builder(text)
                .read_only(true)
                .experimental_index_method(true),
        )
        .await
        .map_err(|error| owner_error(error.into()))?;
    let connection = database
        .connect()
        .map_err(|error| owner_error(error.into()))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| owner_error(error.into()))?;

    if schema_preflight::classify(&connection)
        .await
        .map_err(owner_error)?
        != DatabaseState::Current
    {
        return Err(AuthoritySnapshotError::IntegrityCheckFailed);
    }
    let mut rows = connection
        .query("PRAGMA integrity_check(1)", ())
        .await
        .map_err(|error| owner_error(error.into()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| owner_error(error.into()))?
        .ok_or(AuthoritySnapshotError::IntegrityCheckFailed)?;
    let verdict: String = row.get(0).map_err(|error| owner_error(error.into()))?;
    let extra = rows
        .next()
        .await
        .map_err(|error| owner_error(error.into()))?
        .is_some();
    drop(rows);
    drop(connection);
    drop(database);
    if verdict != "ok" || extra {
        return Err(AuthoritySnapshotError::IntegrityCheckFailed);
    }
    Ok(())
}

fn verify_snapshot_names(
    staging: &CreatedDirectory,
    staging_path: &Path,
    file_path: &Path,
    expected_file: Option<FileIdentity>,
) -> Result<(), AuthoritySnapshotError> {
    staging
        .verify_named()
        .map_err(AuthoritySnapshotError::Filesystem)?;
    staging
        .verify_path(staging_path)
        .map_err(AuthoritySnapshotError::Filesystem)?;
    if let Some(expected) = expected_file {
        let directory = staging.capability();
        let file = directory
            .open_file_read(SNAPSHOT_FILE_NAME)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => AuthoritySnapshotError::OutputMissing,
                _ => AuthoritySnapshotError::Filesystem(error),
            })?;
        if FileIdentity::of_file(&file).map_err(AuthoritySnapshotError::Filesystem)? != expected
            || FileIdentity::of_path_nofollow(file_path)
                .map_err(AuthoritySnapshotError::Filesystem)?
                != expected
        {
            return Err(AuthoritySnapshotError::OutputIdentityChanged);
        }
    }
    Ok(())
}

fn capture_incomplete(
    staging: CreatedDirectory,
    staging_path: PathBuf,
    path: PathBuf,
) -> IncompleteAuthoritySnapshot {
    let observed = staging
        .capability()
        .open_file_read(SNAPSHOT_FILE_NAME)
        .ok()
        .and_then(|file| {
            let identity = FileIdentity::of_file(&file).ok()?;
            let bytes = file.metadata().ok()?.len();
            Some((identity, bytes))
        });
    IncompleteAuthoritySnapshot {
        staging,
        staging_path,
        path,
        output_file_identity: observed.map(|(identity, _)| identity),
        output_file_bytes: observed.map(|(_, bytes)| bytes),
    }
}

fn failure(
    error: AuthoritySnapshotError,
    incomplete: Option<IncompleteAuthoritySnapshot>,
) -> AuthoritySnapshotFailure {
    AuthoritySnapshotFailure { error, incomplete }
}

fn owner_error(error: AuthorityError) -> AuthoritySnapshotError {
    AuthoritySnapshotError::Owner(error)
}

impl From<AuthorityError> for AuthoritySnapshotError {
    fn from(error: AuthorityError) -> Self {
        owner_error(error)
    }
}

impl From<turso::Error> for AuthoritySnapshotError {
    fn from(error: turso::Error) -> Self {
        owner_error(AuthorityError::Database(error))
    }
}

impl From<io::Error> for AuthoritySnapshotError {
    fn from(error: io::Error) -> Self {
        Self::Filesystem(error)
    }
}
