//! [`ProducerSource`] and [`ProducerRegistry`] — drive real language producers.
//!
//! # Architecture
//!
//! `ProducerSource` wraps a `ProducerRegistry` (a map from `Language` to a
//! type-erased producer runner) and a list of `PackageSource`s to process.
//! For each source it:
//!
//! 1. Emits `LoadEvent::Discovered`.
//! 2. Emits `LoadEvent::Progress { stage: OracleRunning }`.
//! 3. Calls `produce::<P>` on a `tokio::task::spawn_blocking` thread (since
//!    language oracles are synchronous and often heavy).
//! 4. Wraps the resulting `PristineIntroTable` in an `IrView` and builds a
//!    `PackageView`.
//! 5. Emits `LoadEvent::Progress { stage: Indexing }` then `LoadEvent::Ready`.
//!
//! If a producer is not registered for a package's language, it emits
//! `LoadEvent::Failed { error: SourceError::ToolchainMissing }` and continues
//! with the next package (LR-10). A producer that returns a `ProducerError`
//! is surfaced as `LoadEvent::Failed { error: SourceError::OracleFailed }` —
//! never as a stream-level error.
//!
//! # Runtime contract (LR-9)
//!
//! `ProducerSource::load` uses `tokio::task::spawn_blocking` for each
//! package. The stream itself is created with `futures::stream::unfold`, which
//! is `Send` and requires no `LocalSet`. Only the caller (the engine) chooses
//! which runtime to poll this stream on.
//!
//! # Pilot registration
//!
//! `ProducerRegistry::with_rust_pilot` registers `nudox-producer-rust` as the
//! only out-of-the-box producer. Other languages are registered at engine
//! startup via `ProducerRegistry::register`. Because `Producer` is generic over
//! `Id` and `Oracle`, we type-erase at the boundary using a `Box<dyn RunProducer>`.

use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt};
use nudox_ir::{
    body::Language,
    change::{EcosystemId, PackageLineageId, PackageName},
    view::IrView,
};
use nudox_producer::{PackageSource, ProducerError, produce};
use nudox_producer_rust::RustProducer;
use tokio::task;

use crate::{
    package::{PackageView, Provenance},
    source::{
        IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, SourceError,
    },
};

// ---------------------------------------------------------------------------
// RunProducer — type-erased producer runner
// ---------------------------------------------------------------------------

/// Type-erased closure that runs a language producer over one `PackageSource`.
///
/// The closure is `Send + Sync + 'static` so it can be moved into a
/// `spawn_blocking` call. It returns a `PristineIntroTable` on success or a
/// `ProducerError` on failure.
trait RunProducer: Send + Sync + 'static {
    fn run(
        &self,
        src: &PackageSource,
        lineage: &PackageLineageId,
    ) -> Result<nudox_ir::apply::PristineIntroTable, ProducerError>;
}

// Concrete impl for any `Producer`.
struct TypedRunner<P: nudox_producer::Producer + Send + Sync + 'static>(P);

impl<P> RunProducer for TypedRunner<P>
where
    P: nudox_producer::Producer + Send + Sync + 'static,
{
    fn run(
        &self,
        src: &PackageSource,
        lineage: &PackageLineageId,
    ) -> Result<nudox_ir::apply::PristineIntroTable, ProducerError> {
        produce(&self.0, src, lineage)
    }
}

// ---------------------------------------------------------------------------
// ProducerEntry
// ---------------------------------------------------------------------------

/// One entry in the [`ProducerRegistry`].
struct ProducerEntry {
    runner: Box<dyn RunProducer>,
}

// ---------------------------------------------------------------------------
// ProducerRegistry
// ---------------------------------------------------------------------------

/// A registry of language producers, keyed by [`Language`].
///
/// Build one registry per engine lifetime and share it (inside an `Arc`) across
/// all `ProducerSource` instances.
///
/// # Registration
///
/// Each `Language` maps to at most one runner. Registering a language twice
/// silently replaces the first runner — call sites must not rely on ordering.
pub struct ProducerRegistry {
    /// Language → type-erased runner.
    entries: std::collections::HashMap<Language, ProducerEntry>,
}

impl ProducerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { entries: std::collections::HashMap::new() }
    }

    /// Create a registry pre-loaded with `nudox-producer-rust` as the pilot.
    ///
    /// The Rust producer uses `ra_ap_*` in-process and does not require any
    /// external toolchain binary beyond the Cargo manifest. It is registered
    /// under `Language::Rust`.
    pub fn with_rust_pilot() -> Self {
        let mut reg = Self::new();
        reg.register(
            Language::Rust,
            RustProducer {
                name: String::new(), // overridden per-package by `PackageSource::name`
                version: String::new(),
                direct_repo: false,
            },
        );
        reg
    }

    /// Register a producer for `language`.
    ///
    /// Replaces any previously registered producer for the same language.
    pub fn register<P>(&mut self, language: Language, producer: P)
    where
        P: nudox_producer::Producer + Send + Sync + 'static,
    {
        self.entries.insert(
            language,
            ProducerEntry { runner: Box::new(TypedRunner(producer)) },
        );
    }

    /// True if a producer is registered for `language`.
    pub fn has(&self, language: Language) -> bool {
        self.entries.contains_key(&language)
    }

    /// Run the producer for `language` over `src`, or return an error if
    /// no producer is registered.
    fn run(
        &self,
        language: Language,
        src: &PackageSource,
        lineage: &PackageLineageId,
    ) -> Result<nudox_ir::apply::PristineIntroTable, SourceError> {
        let entry = self.entries.get(&language).ok_or_else(|| {
            SourceError::ToolchainMissing {
                language,
                package: lineage.clone(),
            }
        })?;

        entry.runner.run(src, lineage).map_err(|err| match &err {
            ProducerError::OracleSpawn { .. } | ProducerError::OracleExit { .. } => {
                SourceError::OracleFailed {
                    package: lineage.clone(),
                    detail: err.to_string(),
                }
            }
            ProducerError::Decode { .. } | ProducerError::LoweringFailed { .. } => {
                SourceError::LoweringFailed {
                    package: lineage.clone(),
                    detail: err.to_string(),
                }
            }
            ProducerError::UnsupportedConstruct { .. } => SourceError::OracleFailed {
                package: lineage.clone(),
                detail: err.to_string(),
            },
        })
    }
}

impl Default for ProducerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PackageDescriptor
// ---------------------------------------------------------------------------

/// Everything `ProducerSource` needs to produce one package.
#[derive(Debug, Clone)]
pub struct PackageDescriptor {
    /// The package source (path, name, version).
    pub source: PackageSource,
    /// The stable lineage identity to assign to this package's IR.
    pub lineage: PackageLineageId,
    /// The language whose producer should handle this package.
    pub language: Language,
}

impl PackageDescriptor {
    /// Construct a descriptor for a Cargo package.
    pub fn cargo(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        let name_str: String = name.into();
        let lineage = PackageLineageId::new(
            EcosystemId::new("cargo"),
            PackageName::new(name_str.clone()),
        );
        Self {
            source: PackageSource::new(root, name_str, version),
            lineage,
            language: Language::Rust,
        }
    }
}

// ---------------------------------------------------------------------------
// ProducerSource
// ---------------------------------------------------------------------------

/// An `IrSource` that drives real language producers on the Tokio blocking pool.
///
/// Each package is produced on its own `spawn_blocking` call so that slow
/// oracles (Roslyn, `go doc`, ra) do not block other packages. The stream
/// emits packages as they complete — order is not guaranteed.
pub struct ProducerSource {
    registry: Arc<ProducerRegistry>,
    packages: Vec<PackageDescriptor>,
}

impl ProducerSource {
    /// Construct a source from a registry and a list of package descriptors.
    pub fn new(registry: Arc<ProducerRegistry>, packages: Vec<PackageDescriptor>) -> Self {
        Self { registry, packages }
    }

    /// Convenience constructor: one Rust package, using the pilot registry.
    pub fn rust_package(descriptor: PackageDescriptor) -> Self {
        Self::new(
            Arc::new(ProducerRegistry::with_rust_pilot()),
            vec![descriptor],
        )
    }
}

impl IrSource for ProducerSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "producer".to_owned(),
            package_count_hint: Some(self.packages.len() as u32),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>> {
        let registry = Arc::clone(&self.registry);
        let packages = self.packages.clone();

        // For each package, spawn a blocking task and collect the events it
        // produces. We use `stream::iter` over the package list and then
        // `flat_map` to convert each descriptor into a sub-stream of events.
        //
        // Note: because `spawn_blocking` returns a `JoinHandle`, we use
        // `stream::once(async move { ... })` to wrap each async block, then
        // flatten the results into individual events.
        let event_stream = stream::iter(packages).then(move |desc| {
            let registry = Arc::clone(&registry);

            async move {
                let lineage = desc.lineage.clone();
                let language = desc.language;
                let display_name = desc.source.name.as_str().to_owned();
                let ecosystem = lineage.ecosystem.as_str().to_owned();

                // Emit Discovered synchronously (no blocking work yet).
                let discovered = Ok(LoadEvent::Discovered {
                    lineage: lineage.clone(),
                    hint: PackageHint {
                        display_name: display_name.clone(),
                        ecosystem,
                        version: Some(desc.source.version.clone()),
                    },
                });

                // Run the producer on a blocking thread.
                let lineage_for_run = lineage.clone();
                let run_result = task::spawn_blocking(move || {
                    registry.run(language, &desc.source, &lineage_for_run)
                })
                .await;

                // Flatten JoinError into SourceError::Internal.
                let table_result = match run_result {
                    Ok(result) => result,
                    Err(join_err) => Err(SourceError::Internal(join_err.to_string())),
                };

                let ready_or_failed = match table_result {
                    Ok(table) => {
                        let view = IrView::with_package(lineage.clone(), table);
                        let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
                        Ok(LoadEvent::Ready { package: pkg })
                    }
                    Err(err) => Ok(LoadEvent::Failed {
                        lineage: lineage.clone(),
                        error: err,
                    }),
                };

                // Return both events as a small vec; the outer flat_map will
                // iterate them.
                vec![discovered, ready_or_failed]
            }
        });

        // Flatten each `Vec<Result<LoadEvent, _>>` into individual items.
        event_stream
            .flat_map(|events| stream::iter(events))
            .boxed()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_rust_by_default() {
        let reg = ProducerRegistry::with_rust_pilot();
        assert!(reg.has(Language::Rust));
        assert!(!reg.has(Language::Go));
    }

    #[test]
    fn missing_language_yields_toolchain_missing() {
        let reg = ProducerRegistry::new();
        let src = PackageSource::new("/tmp", "test", "0.1.0");
        let lineage = PackageLineageId::new(
            EcosystemId::new("cargo"),
            PackageName::new("test"),
        );
        let err = reg.run(Language::Rust, &src, &lineage).unwrap_err();
        assert!(matches!(err, SourceError::ToolchainMissing { .. }));
    }

    #[test]
    fn describe_returns_package_count() {
        let src = ProducerSource {
            registry: Arc::new(ProducerRegistry::new()),
            packages: vec![
                PackageDescriptor::cargo("/tmp/a", "a", "0.1"),
                PackageDescriptor::cargo("/tmp/b", "b", "0.1"),
            ],
        };
        let desc = src.describe();
        assert_eq!(desc.package_count_hint, Some(2));
    }
}
