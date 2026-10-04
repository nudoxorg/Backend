//! Database opening and process-shared generation policy.

use crate::projection_namespace::{
    ProjectionGenerationId, ProjectionGraphSeed, ProjectionNamespace, ProjectionOperationGuard,
    ProjectionSeed, ProjectionSelector, PublishOutcome,
};
use crate::read::metadata_from;
use crate::schema;
use crate::sharing::SharedWalBackend;
use crate::{ProjectionError, TursoProjection};
use backend_platform::{CreatedDirectory, DirectoryCapability, FileIdentity};
use backend_library::CheckedPackageGraphFacts;
use std::path::Path;
use std::time::Duration;

/// Bounded wait for another process-owned writer lane.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SELECTION_RETRIES: usize = 8;

impl TursoProjection {
    /// Opens only an already selected projection generation.
    ///
    /// This path never creates a namespace, opens an unselected legacy path,
    /// executes DDL, or chooses a generation. A fresh database must use
    /// [`Self::open_or_rebuild`] with a complete admitted seed.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, ProjectionError> {
        let backend = SharedWalBackend::detect()?;
        let namespace = ProjectionNamespace::open_existing(path.as_ref())?;
        for _ in 0..MAX_SELECTION_RETRIES {
            let selector = namespace.selected()?.ok_or(ProjectionError::NeedsSeed)?;
            match open_selected(namespace.clone(), selector, &backend).await {
                Err(ProjectionError::SupersededGeneration) => continue,
                result => return result,
            }
        }
        Err(ProjectionError::NamespaceBusy)
    }

    /// Opens the selected current generation, or privately seeds and publishes
    /// a new generation when none exists or the selected schema is older.
    ///
    /// A current selected generation is returned without comparing its root
    /// with `seed`: the caller must reconcile it from the authoritative
    /// current `ViewRoot` using `synchronize` or checked deltas. The seed is
    /// consumed before publication only when this call creates a generation.
    /// This separation avoids treating a selector's immutable seed witness as
    /// the generation's mutable current root.
    pub async fn open_or_rebuild(
        path: impl AsRef<Path>,
        seed: ProjectionSeed<'_>,
    ) -> Result<Self, ProjectionError> {
        let backend = SharedWalBackend::detect()?;
        let namespace = ProjectionNamespace::open_for_seed(path.as_ref())?;
        for _ in 0..MAX_SELECTION_RETRIES {
            let expected = namespace.selected()?;
            if let Some(selector) = expected {
                match open_selected(namespace.clone(), selector, &backend).await {
                    Ok(projection) => return Ok(projection),
                    Err(ProjectionError::SupersededGeneration) => continue,
                    Err(ProjectionError::Schema { found })
                        if found < schema::SCHEMA_VERSION => {}
                    Err(error) => return Err(error),
                }
            }
            match publish_seed(&namespace, &backend, expected, seed).await? {
                SeedPublication::Published(selector) => {
                    match open_selected(namespace.clone(), selector, &backend).await {
                        Err(ProjectionError::SupersededGeneration) => continue,
                        result => return result,
                    }
                }
                SeedPublication::Conflict => continue,
            }
        }
        Err(ProjectionError::NamespaceBusy)
    }

    /// Acquires a process-shared selector fence for one bounded database
    /// operation. Private staging operations are not selected yet and need no
    /// namespace fence; they remain owned by their creation receipt.
    pub(crate) fn operation_guard(
        &self,
    ) -> Result<Option<ProjectionOperationGuard>, ProjectionError> {
        if self.staging {
            return Ok(None);
        }
        if self.generation != self.selector.generation {
            return Err(ProjectionError::NamespaceIdentity);
        }
        self.namespace
            .operation_guard(self.selector, self.marker_identity)
            .map(Some)
    }
}

enum SeedPublication {
    Published(ProjectionSelector),
    Conflict,
}

async fn publish_seed(
    namespace: &ProjectionNamespace,
    backend: &SharedWalBackend,
    expected: Option<ProjectionSelector>,
    seed: ProjectionSeed<'_>,
) -> Result<SeedPublication, ProjectionError> {
    let generation = match namespace.allocate_generation(expected) {
        Ok(generation) => generation,
        Err(ProjectionError::StaleTransition) => return Ok(SeedPublication::Conflict),
        Err(error) => return Err(error),
    };
    let selector = ProjectionSelector::for_seed(generation, seed);
    let (receipt, staged) = prepare_generation(namespace, backend, selector, seed).await?;
    #[cfg(test)]
    hold_after_stage_for_process_test();
    close_projection(staged);
    if let Err(error) = receipt.capability().sync_all() {
        let error = ProjectionError::Filesystem(error);
        cleanup_stage(receipt, &error.to_string())?;
        return Err(error);
    }
    match namespace.publish(expected, selector) {
        Ok(PublishOutcome::Published) => Ok(SeedPublication::Published(selector)),
        Ok(PublishOutcome::Conflict(_)) => {
            cleanup_stage(receipt, "selector compare-and-swap lost")?;
            Ok(SeedPublication::Conflict)
        }
        Err(error @ ProjectionError::PublicationIndeterminate) => {
            // The selector rename may already be visible. Preserve the stage
            // so a cold reopen can decide whether it became authoritative.
            drop(receipt);
            Err(error)
        }
        Err(error) => {
            cleanup_stage(receipt, &error.to_string())?;
            Err(error)
        }
    }
}

#[cfg(test)]
fn hold_after_stage_for_process_test() {
    if std::env::var_os("BACKEND_TURSO_TEST_HOLD_AFTER_STAGE").is_some() {
        crate::process_harness::report("STAGED");
        if crate::process_harness::commands().next().as_deref() != Some("GO") {
            loop {
                std::thread::park();
            }
        }
    }
}

async fn prepare_generation(
    namespace: &ProjectionNamespace,
    backend: &SharedWalBackend,
    selector: ProjectionSelector,
    seed: ProjectionSeed<'_>,
) -> Result<(CreatedDirectory, TursoProjection), ProjectionError> {
    let receipt = namespace.create_generation(selector.generation)?;
    let setup = async {
        let generation_directory = receipt.capability().clone();
        let marker_identity = namespace.write_generation_identity(&generation_directory, selector)?;
        let generation_pin = namespace.generation_pin(&generation_directory, true)?;
        let database_path = namespace.database_path(selector.generation)?;
        let path_text = database_path
            .to_str()
            .ok_or_else(|| ProjectionError::NonUtf8Path(database_path.clone()))?;
        let database = backend
            .open_database(backend.builder(path_text))
            .await
            .map_err(ProjectionError::from)?;
        let connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.execute_batch(schema::SCHEMA).await?;
        let mut projection = TursoProjection {
            _database: database,
            connection,
            namespace: namespace.clone(),
            generation: selector.generation,
            selector,
            marker_identity,
            generation_directory,
            generation_pin,
            staging: true,
        };
        projection.synchronize(seed.view).await?;
        if let ProjectionGraphSeed::Checked(facts) = seed.graph {
            projection
                .synchronize_checked_package_graph(seed.view.root(), facts)
                .await?;
        }
        validate_seed_projection(&projection, seed).await?;
        Ok::<_, ProjectionError>(projection)
    }
    .await;
    match setup {
        Ok(projection) => Ok((receipt, projection)),
        Err(error) => {
            cleanup_stage(receipt, &error.to_string())?;
            Err(error)
        }
    }
}

async fn validate_seed_projection(
    projection: &TursoProjection,
    seed: ProjectionSeed<'_>,
) -> Result<(), ProjectionError> {
    let metadata = projection
        .metadata()
        .await?
        .ok_or(ProjectionError::StaleTransition)?;
    if metadata.root.as_slice() != seed.view.root().as_bytes()
        || metadata.view_version.as_slice() != seed.view.version().as_bytes()
    {
        return Err(ProjectionError::StaleTransition);
    }
    let graph = crate::graph::package_graph_metadata_from(&projection.connection).await?;
    match (seed.graph, graph) {
        (ProjectionGraphSeed::Unavailable, None) => Ok(()),
        (ProjectionGraphSeed::Checked(facts), Some(metadata))
            if metadata.root.as_slice() == seed.view.root().as_bytes()
                && metadata.facts_witness == facts.witness() =>
        {
            Ok(())
        }
        _ => Err(ProjectionError::StaleTransition),
    }
}

fn cleanup_stage(receipt: CreatedDirectory, cause: &str) -> Result<(), ProjectionError> {
    receipt
        .remove_all(256)
        .map_err(|cleanup| ProjectionError::StageCleanup {
            cause: cause.to_owned(),
            cleanup,
        })
}

fn close_projection(projection: TursoProjection) {
    let TursoProjection {
        connection,
        _database,
        generation_pin,
        generation_directory,
        namespace,
        ..
    } = projection;
    drop(connection);
    drop(_database);
    drop(generation_pin);
    drop(generation_directory);
    drop(namespace);
}

async fn open_selected(
    namespace: ProjectionNamespace,
    selector: ProjectionSelector,
    backend: &SharedWalBackend,
) -> Result<TursoProjection, ProjectionError> {
    let generation_directory = namespace.generation_directory(selector.generation)?;
    let marker_identity = namespace.verify_generation_identity(&generation_directory, selector, None)?;
    let generation_pin = namespace.generation_pin(&generation_directory, false)?;
    let guard = namespace.operation_guard(selector, marker_identity)?;
    let database_path = namespace.database_path(selector.generation)?;
    let path_text = database_path
        .to_str()
        .ok_or_else(|| ProjectionError::NonUtf8Path(database_path.clone()))?;
    let database = backend
        .open_database(backend.builder(path_text))
        .await
        .map_err(ProjectionError::from)?;
    let connection = database.connect()?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    let found = projection_schema_version(&connection).await?;
    if found != selector.schema_version {
        return Err(ProjectionError::CorruptNamespace {
            field: "selector_schema_parity",
        });
    }
    if found != schema::SCHEMA_VERSION {
        return Err(ProjectionError::Schema { found });
    }
    let metadata = metadata_from(&connection).await?;
    if metadata.is_none() {
        return Err(ProjectionError::StaleTransition);
    }
    namespace.verify_generation_identity(&generation_directory, selector, Some(marker_identity))?;
    drop(guard);
    Ok(TursoProjection {
        _database: database,
        connection,
        namespace,
        generation: selector.generation,
        selector,
        marker_identity,
        generation_directory,
        generation_pin,
        staging: false,
    })
}

async fn projection_schema_version(
    connection: &turso::Connection,
) -> Result<i64, ProjectionError> {
    let has_row_digest = meta_has_row_digest(connection).await?;
    let found = legacy_schema_version(connection).await?;
    if !has_row_digest && found >= schema::SCHEMA_VERSION {
        return Err(ProjectionError::CorruptMetadata {
            field: "missing_row_digest_column",
        });
    }
    Ok(found)
}

async fn meta_has_row_digest(connection: &turso::Connection) -> Result<bool, ProjectionError> {
    let mut rows = connection
        .query("PRAGMA table_info(backend_projection_meta)", ())
        .await?;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if name == "row_digest" {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn legacy_schema_version(connection: &turso::Connection) -> Result<i64, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT schema_version FROM backend_projection_meta WHERE singleton=1 LIMIT 2",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(ProjectionError::StaleTransition);
    };
    let version = row.get(0)?;
    if rows.next().await?.is_some() {
        return Err(ProjectionError::CorruptMetadata {
            field: "duplicate_metadata_singleton",
        });
    }
    Ok(version)
}
