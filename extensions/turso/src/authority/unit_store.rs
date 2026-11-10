//! Untrusted storage for a physically separate C# unit domain.
//!
//! This layer never grants an owner lease, a unit permit, or selected compiler
//! authority. The engine must independently bind the file and every claim to
//! its live workspace owner before using these records.
use super::{AuthorityError, UnitFenceClaim, schema_preflight};
use crate::{connection::BUSY_TIMEOUT, sharing::SharedWalBackend};
use std::path::Path;

/// Raw unit storage. It cannot be converted to the legacy authority handle.
/// Opening it or recording a claim is not evidence that either is trusted.
pub struct UntrustedCSharpUnitStore {
    _database: turso::Database,
    connection: turso::Connection,
    workspace: [u8; 32],
    domain: [u8; 32],
}

impl UntrustedCSharpUnitStore {
    /// Opens an attested isolated unit database, or initializes a fresh file.
    /// The caller must hold an external owner/namespace interlock across the
    /// preflight and all subsequent operations. This API grants no authority.
    ///
    /// # Errors
    /// Refuses existing legacy, foreign, malformed, or mismatched databases.
    pub async fn open_untrusted(
        path: impl AsRef<Path>,
        workspace: [u8; 32],
        domain: [u8; 32],
    ) -> Result<Self, AuthorityError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| AuthorityError::NonUtf8Path(path.to_path_buf()))?;
        if workspace == [0; 32] || domain == [0; 32] {
            return Err(AuthorityError::SchemaIntegrity);
        }
        let backend = SharedWalBackend::detect()?;
        if path.try_exists().map_err(AuthorityError::PathIo)? {
            let database = backend
                .open_database(
                    backend
                        .builder(text)
                        .read_only(true)
                        .experimental_index_method(true),
                )
                .await?;
            let connection = database.connect()?;
            connection.busy_timeout(BUSY_TIMEOUT)?;
            schema_preflight::attest_v7_unit(&connection, &workspace, &domain).await?;
        } else {
            let _ = schema_preflight::expected_unit_schema_v7().await?;
        }
        let database = backend
            .open_database(backend.builder(text).experimental_index_method(true))
            .await?;
        let mut connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        schema_preflight::initialize_v7_unit(&mut connection, &workspace, &domain).await?;
        Ok(Self {
            _database: database,
            connection,
            workspace,
            domain,
        })
    }

    /// Records an untrusted immutable mapping. A successful write authenticates
    /// neither the supplied unit nor the caller's workspace or owner lease.
    ///
    /// # Errors
    /// Refuses malformed, conflicting, or foreign-domain mappings.
    pub async fn record_untrusted_fence(
        &mut self,
        claim: &UnitFenceClaim<'_>,
    ) -> Result<(), AuthorityError> {
        schema_preflight::record_unit_fence(
            &mut self.connection,
            &self.workspace,
            &self.domain,
            claim,
        )
        .await
    }
}
