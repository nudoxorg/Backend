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
use nudox_producer_clang::ClangProducer;
use nudox_producer_csharp::CSharpProducer;
use nudox_producer_go::GoProducer;
use nudox_producer_java::JavaProducer;
// Only referenced when the `pyrefly` feature is on — see `with_all_available`.
// Without it, `PythonProducer` exists but is deliberately never registered,
// so importing it unconditionally would be an unused-import warning.
#[cfg(feature = "pyrefly")]
use nudox_producer_python::PythonProducer;
use nudox_producer_rust::RustProducer;
use nudox_producer_typescript::TypescriptProducer;
use tokio::task;

use crate::{
    package::{PackageView, Provenance},
    source::{IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, SourceError},
};

// ---------------------------------------------------------------------------
// RunProducer — type-erased producer runner
// ---------------------------------------------------------------------------

/// Type-erased closure that runs a language producer over one `PackageSource`.
///
/// The closure is `Send + Sync + 'static` so it can be moved into a
/// `spawn_blocking` call. It returns a [`Produced`] on success or a
/// `ProducerError` on failure.
///
/// [`Produced`]: nudox_producer::Produced
trait RunProducer: Send + Sync + 'static {
    fn run(
        &self,
        src: &PackageSource,
        lineage: &PackageLineageId,
    ) -> Result<nudox_producer::Produced, ProducerError>;
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
    ) -> Result<nudox_producer::Produced, ProducerError> {
        // `Unlinked` is the honest resolver for the local-first load path as it
        // exists today: `IrSource` loads one package at a time and has no
        // handle to the sealed tables of its dependencies. Cross-package
        // references therefore arrive *named* (so signatures render) and
        // *unlinked* (so no hyperlink points at nothing), and
        // `SealReport::unlinked` says exactly how many and why.
        produce(&self.0, src, lineage, &nudox_ir::foreign::Unlinked)
    }
}

// ---------------------------------------------------------------------------
// ProducerEntry
// ---------------------------------------------------------------------------

/// One entry in the [`ProducerRegistry`].
struct ProducerEntry {
    /// `Arc`, not `Box`: [`ProducerRegistry::register_c_and_cpp`] shares one
    /// runner between two [`Language`] keys (`C` and `Cpp`) without cloning
    /// the underlying producer.
    runner: Arc<dyn RunProducer>,
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
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    /// Create a registry pre-loaded with `nudox-producer-rust` as the pilot.
    ///
    /// The Rust producer uses `ra_ap_*` in-process and does not require any
    /// external toolchain binary beyond the Cargo manifest. It is registered
    /// under `Language::Rust`.
    pub fn with_rust_pilot() -> Self {
        let mut reg = Self::new();
        // The producer carries no package identity: it reads the name from the
        // `PackageSource` it is invoked with. That is what makes one registered
        // producer per language correct — see `RustProducer`'s type docs for the
        // bug that shape used to hide.
        reg.register(Language::Rust, RustProducer { direct_repo: false });
        reg
    }

    /// Create a registry pre-loaded with every producer that both
    /// implements `nudox_producer::Producer` *and* can actually produce
    /// something in this build — "available" means available, not merely
    /// "compiled in."
    ///
    /// This is the engine's production registry — the multi-language
    /// successor to [`Self::with_rust_pilot`], which this program's history
    /// shows is easy to mistake for "the" registry rather than "the pilot
    /// one" (see LIMITATIONS.md L2). It stays available for tests and callers
    /// that deliberately want a single-language corpus.
    ///
    /// # Why this is six producers, not seven
    ///
    /// Seven `nudox-producer-*` crates exist in the workspace, and all seven
    /// compile standalone and implement `nudox_producer::Producer`. Six are
    /// registered unconditionally here: Rust, Go, Java, C#, TypeScript (OXC),
    /// and C/C++ (clang, registered under both `Language::C` and
    /// `Language::Cpp` via [`Self::register_c_and_cpp`] — see that method's
    /// doc comment for why plain [`Self::register`] would strand `Cpp`).
    ///
    /// Python is registered here **only** behind this crate's own `pyrefly`
    /// Cargo feature (forwarded to `nudox-producer-python/pyrefly` — see
    /// this crate's `Cargo.toml`). Without it, `PythonProducer::invoke`
    /// unconditionally returns `Ok(PythonOracle::default())` and never reads
    /// its `PackageSource`.
    ///
    /// That used to make "we never really tried" and "we tried and there was
    /// nothing" the same observable outcome, and not registering the producer
    /// was the only defence available. It is no longer the only one, and the
    /// claim that it is has been removed from this comment: `PythonProducer`
    /// now declares `YieldContract::RootOnly` under
    /// `cfg(not(feature = "pyrefly"))`, so `nudox_producer::produce` fails such
    /// a run as `ProducerError::NoDeclarationsContributed` even if something
    /// does register it. The two defences are independent and answer different
    /// questions, which is why both are kept: the yield contract makes the
    /// silent success *unrepresentable* for every producer, and withholding the
    /// registration additionally spares the caller a pointless load — it gets a
    /// `SourceError::ToolchainMissing` naming the language up front, instead of
    /// an oracle failure after the work (LIMITATIONS.md L2).
    ///
    /// A package described for a language with no registered producer is not
    /// a build failure: `ProducerRegistry::run` returns
    /// `SourceError::ToolchainMissing` for it, naming the language, and the
    /// rest of the corpus is unaffected — that behaviour predates this
    /// constructor and is unchanged. See LIMITATIONS.md L2.
    pub fn with_all_available() -> Self {
        let mut reg = Self::new();
        reg.register(Language::Rust, RustProducer { direct_repo: false });
        #[cfg(feature = "pyrefly")]
        reg.register(Language::Python, PythonProducer);
        reg.register(Language::TypeScript, TypescriptProducer::new());
        reg.register(Language::Go, GoProducer);
        reg.register(Language::Java, JavaProducer::new());
        reg.register(Language::CSharp, CSharpProducer::from_env());
        reg.register_c_and_cpp(ClangProducer::new());
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
            ProducerEntry {
                runner: Arc::new(TypedRunner(producer)),
            },
        );
    }

    /// Register a C/C++ producer under **both** [`Language::C`] and
    /// [`Language::Cpp`], so a package tagged either variant reaches the
    /// same runner.
    ///
    /// `nudox_ir::body::Language` carries `C` and `Cpp` as two distinct
    /// variants, but `Producer::LANGUAGE` is a single associated constant —
    /// a libclang-based producer that parses both dialects with one
    /// toolchain has no way to declare that at the trait level, so it
    /// declares only `Language::C` (see `ClangProducer::LANGUAGE`, which
    /// carries the comment "also covers C++ files"). Registering such a
    /// producer with the plain [`Self::register`] under `Language::C` alone
    /// leaves every package tagged `Language::Cpp` falling through to
    /// `SourceError::ToolchainMissing`, even though the exact same runner
    /// would have served it — not a deliberate limitation, a gap nobody
    /// closed. This method closes it structurally: call it once and both
    /// languages resolve to the same runner, so wiring in a C/C++ producer
    /// cannot recreate the trap by forgetting the second registration.
    ///
    /// `ProducerRegistry::with_all_available` calls this once, with
    /// `ClangProducer::new()`, rather than a `Language::C`-only `register`
    /// call that would silently strand `Language::Cpp`.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if `P::LANGUAGE != Language::C` — this
    /// method's whole reason to exist is the C/C++ convention above; a
    /// producer for any other language should use [`Self::register`].
    pub fn register_c_and_cpp<P>(&mut self, producer: P)
    where
        P: nudox_producer::Producer + Send + Sync + 'static,
    {
        debug_assert_eq!(
            P::LANGUAGE,
            Language::C,
            "register_c_and_cpp is for producers declaring Language::C under \
             the C/C++-family convention (see ClangProducer::LANGUAGE); got {:?} — \
             use register() for a single-language producer",
            P::LANGUAGE,
        );
        let runner: Arc<dyn RunProducer> = Arc::new(TypedRunner(producer));
        self.entries.insert(
            Language::C,
            ProducerEntry {
                runner: Arc::clone(&runner),
            },
        );
        self.entries.insert(Language::Cpp, ProducerEntry { runner });
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
    ) -> Result<nudox_producer::Produced, SourceError> {
        let entry = self
            .entries
            .get(&language)
            .ok_or_else(|| SourceError::ToolchainMissing {
                language,
                package: lineage.clone(),
            })?;

        let produced = entry.runner.run(src, lineage).map_err(|err| match &err {
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
            // The environment could not hand the producer a complete dependency
            // graph, so any table it produced would be silently partial. That is
            // an oracle-side failure, not a lowering bug, so it maps to
            // `OracleFailed` rather than `LoweringFailed`.
            //
            // `detail` goes through `chain` instead of calling `to_string()`
            // like its neighbours: this variant's own `Display` is deliberately
            // terse, and everything a reader needs — *which* dependency failed
            // and why — lives in the cause.
            ProducerError::DependenciesUnresolved { .. } => SourceError::OracleFailed {
                package: lineage.clone(),
                detail: chain(&err),
            },
            // The producer said it would contribute declarations and did not:
            // the lowering that came back holds nothing but the root module
            // `produce` synthesized. That is an *oracle-side* outcome — the
            // producer's lowering code is not implicated, it simply had nothing
            // handed to it — so it maps to `OracleFailed` alongside
            // `DependenciesUnresolved`, not to `LoweringFailed`.
            //
            // This is the variant that closes the hole `with_all_available`'s
            // doc comment describes but could only address by *not registering*
            // Python: "'we never really tried' and 'we tried and there was
            // nothing' [being] the same observable outcome". Declining to
            // register a producer only helps for producers nobody registers;
            // this arm makes the distinction hold for every registered one,
            // including a future Python registration.
            ProducerError::NoDeclarationsContributed { .. } => SourceError::OracleFailed {
                package: lineage.clone(),
                detail: chain(&err),
            },
            // The producer declared itself inert and then contributed real
            // declarations. Nothing is wrong with the package or the toolchain
            // — the producer's own `yield_contract` is stale — so this is
            // `LoweringFailed`, whose contract in this crate is "the IR side is
            // at fault", and never `ToolchainMissing`, which would send the
            // reader to look for a toolchain that is evidently present and
            // working.
            ProducerError::YieldContractOutgrown { .. } => SourceError::LoweringFailed {
                package: lineage.clone(),
                detail: chain(&err),
            },
        })?;

        // A producer that declared `YieldContract::RootOnly` and contributed
        // nothing is *consistent*, so `nudox_producer::produce` returns `Ok` for
        // it — see `Produced::contract`. That `Ok` must not become a
        // `LoadEvent::Ready` carrying a one-entry package: the GUI would render
        // it, the corpus would count it, and nothing downstream could tell it
        // from a package that genuinely has one public symbol. This is the only
        // place between `produce` and `PackageView` that still knows, so it is
        // where the degraded case is turned back into a typed failure.
        //
        // `OracleFailed` rather than `ToolchainMissing`, even though "the
        // toolchain is absent" is often *why*: `ToolchainMissing` carries no
        // detail field, and `DegradedYield::blocker` is the only text that says
        // which blocker was declared. Discarding it to reuse a better-fitting
        // variant name would be the `map_err(|_| …)` mistake with extra steps.
        if let Some(degraded) = produced.contract.degraded() {
            return Err(SourceError::OracleFailed {
                package: lineage.clone(),
                detail: degraded.to_string(),
            });
        }

        Ok(produced)
    }
}

/// Render a `ProducerError` and every link of its `#[source]` chain.
///
/// Three `ProducerError` variants — `DependenciesUnresolved`,
/// `NoDeclarationsContributed`, `YieldContractOutgrown` — carry the thing a
/// reader actually needs (which dependency failed; which blocker was declared)
/// in a `#[source]` slot rather than in their own `Display`. `SourceError`'s
/// `detail` is a `String` and is the last point at which that chain still
/// exists, so it is flattened here rather than lost here. AGENTS-DOCTRINE.md §8:
/// "Print the whole `#[source]` chain when a producer fails … Reading only it is
/// how a five-second diagnosis becomes an hour."
fn chain(err: &ProducerError) -> String {
    std::iter::successors(Some(err as &dyn std::error::Error), |e| {
        std::error::Error::source(*e)
    })
    .map(|e| e.to_string())
    .collect::<Vec<_>>()
    .join(": ")
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
        Self::new(root, name, version, EcosystemId::new("cargo"), Language::Rust)
    }

    /// Construct a descriptor for a Go module (manifest: `go.mod`),
    /// produced by the `nudox-producer-go` subprocess oracle.
    pub fn go(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::new(root, name, version, EcosystemId::new("go"), Language::Go)
    }

    /// Construct a descriptor for an npm package (manifest: `package.json`),
    /// produced by the OXC-based TypeScript producer.
    pub fn npm(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::new(
            root,
            name,
            version,
            EcosystemId::new("npm"),
            Language::TypeScript,
        )
    }

    /// Construct a descriptor for a Maven package (manifest: `pom.xml` or
    /// `build.gradle`), produced by the javadoc-doclet-based Java producer.
    pub fn maven(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::new(root, name, version, EcosystemId::new("maven"), Language::Java)
    }

    /// Construct a descriptor for a NuGet package (manifest: `*.csproj`),
    /// produced by the Roslyn-based C# producer.
    pub fn nuget(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::new(
            root,
            name,
            version,
            EcosystemId::new("nuget"),
            Language::CSharp,
        )
    }

    /// Construct a descriptor for a PyPI package (manifest: `pyproject.toml`),
    /// produced by the pyrefly-based Python producer.
    pub fn pypi(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::new(
            root,
            name,
            version,
            EcosystemId::new("pypi"),
            Language::Python,
        )
    }

    /// Construct a descriptor for a C/C++ package (manifest: `CMakeLists.txt`
    /// or `compile_commands.json`), produced by the libclang-based producer.
    ///
    /// # Ecosystem id
    ///
    /// C/C++ has no single canonical package registry the way Cargo/npm/PyPI
    /// do — CMake, Conan, and vcpkg all coexist. `"cpp"` is used as the
    /// ecosystem tag because it is what identifies the lineage across
    /// generations here, not a claim about any particular registry.
    ///
    /// # Language
    ///
    /// Set to `Language::C`, matching `ClangProducer::LANGUAGE` (the const
    /// can only name one variant, and the clang producer's own doc comment
    /// says its single `Language::C` registration "also covers C++ files" —
    /// `workspace/compiler/languages/clang/src/producer.rs`). Building this
    /// descriptor with `Language::Cpp` instead would risk silently missing
    /// the registered runner if it were looked up with plain `register`;
    /// that trap is closed structurally —
    /// [`ProducerRegistry::register_c_and_cpp`] registers the C/C++ producer
    /// under both `Language::C` and `Language::Cpp` at once, so either tag
    /// resolves to the same runner. `Language::C` is kept here as the
    /// established convention, not because `Language::Cpp` would behave any
    /// differently.
    pub fn cpp(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::new(root, name, version, EcosystemId::new("cpp"), Language::C)
    }

    /// Shared constructor behind every ecosystem-specific constructor above.
    ///
    /// Kept private: the public surface is one named constructor per
    /// ecosystem (`cargo`, `go`, `npm`, `maven`, `nuget`, `pypi`, `cpp`) so a
    /// call site cannot pair an ecosystem id with the wrong `Language` by
    /// mistake — a mismatch here silently makes a package unreachable by any
    /// registered producer, which surfaces at runtime as `ToolchainMissing`
    /// rather than as a compile error.
    fn new(
        root: impl Into<std::path::PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
        ecosystem: EcosystemId,
        language: Language,
    ) -> Self {
        let name_str: String = name.into();
        let lineage = PackageLineageId::new(ecosystem, PackageName::new(name_str.clone()));
        Self {
            source: PackageSource::new(root, name_str, version),
            lineage,
            language,
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
                    Ok(produced) => {
                        // Sealing observations that need a human. Unlinked
                        // cross-package references are deliberately NOT logged:
                        // for a local-first load of a single package they are
                        // the normal case, and a line per unloaded dependency
                        // would bury the ones that matter.
                        if !produced.report.is_clean() {
                            tracing::warn!(
                                package = %lineage,
                                forced = produced.report.forced.len(),
                                unmapped_local = produced.report.unmapped_local.len(),
                                collisions = produced.report.collisions.len(),
                                "seal observed identity defects while lowering"
                            );
                        }
                        let view = IrView::with_package(lineage.clone(), produced.table);
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
        event_stream.flat_map(|events| stream::iter(events)).boxed()
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
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("test"));
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

    // -----------------------------------------------------------------
    // with_all_available — one test per language it actually registers
    // (LIMITATIONS.md L2).
    // -----------------------------------------------------------------

    #[test]
    fn with_all_available_resolves_a_runner_for_rust() {
        assert!(ProducerRegistry::with_all_available().has(Language::Rust));
    }

    #[test]
    #[cfg(feature = "pyrefly")]
    fn with_all_available_resolves_a_runner_for_python_with_pyrefly_feature() {
        assert!(ProducerRegistry::with_all_available().has(Language::Python));
    }

    #[test]
    #[cfg(not(feature = "pyrefly"))]
    fn with_all_available_has_no_runner_for_python_without_pyrefly_feature() {
        // Without the `pyrefly` feature, `PythonProducer::invoke`
        // unconditionally returns `Ok(PythonOracle::default())` — a
        // *successful* empty result, not an error. Not registering it means
        // the caller is told up front, as a typed
        // `SourceError::ToolchainMissing`, rather than after a wasted load:
        // this is the "registered-but-inert producer cannot be obtained from
        // the available constructor" property.
        //
        // It is no longer the *only* thing standing between a stub and a
        // counted success — `PythonProducer` now declares
        // `YieldContract::RootOnly`, so `produce` would reject it anyway — but
        // that is a different property, proven in `nudox-producer-python`'s own
        // suite. This test still owns the registration half.
        assert!(!ProducerRegistry::with_all_available().has(Language::Python));
    }

    #[test]
    fn with_all_available_resolves_a_runner_for_typescript() {
        assert!(ProducerRegistry::with_all_available().has(Language::TypeScript));
    }

    #[test]
    fn with_all_available_resolves_a_runner_for_go() {
        assert!(ProducerRegistry::with_all_available().has(Language::Go));
    }

    #[test]
    fn with_all_available_resolves_a_runner_for_java() {
        assert!(ProducerRegistry::with_all_available().has(Language::Java));
    }

    #[test]
    fn with_all_available_resolves_a_runner_for_csharp() {
        // `CSharpProducer::from_env()` is infallible by construction (see its
        // doc comment): a missing published oracle does not stop the
        // registry from being built, so this proves only that an entry
        // exists — not that `dotnet publish` has run. See
        // `csharp_runner_missing_oracle_surfaces_at_run_not_at_registration`
        // for the corresponding runtime behaviour.
        assert!(ProducerRegistry::with_all_available().has(Language::CSharp));
    }

    #[test]
    fn with_all_available_resolves_a_runner_for_cpp() {
        // Registered via `register_c_and_cpp`, so both keys must resolve
        // together — this is the positive twin of the C/Cpp-split trap
        // `register_c_and_cpp_resolves_a_runner_for_both_c_and_cpp` guards
        // at the unit level below; here it is proven through the real
        // production constructor and the real `ClangProducer`.
        let reg = ProducerRegistry::with_all_available();
        assert!(reg.has(Language::C));
        assert!(reg.has(Language::Cpp));
    }

    /// `CSharpProducer::from_env()` resolves its oracle path eagerly but does
    /// not require the oracle to exist: `CSharpProducer::new` (which
    /// `from_env` bottoms out in) never touches the filesystem, so
    /// constructing one — and therefore building the whole registry with
    /// `with_all_available` — cannot fail because `dotnet publish` has never
    /// run. A missing `oracle.dll` is discovered only when `invoke` actually
    /// tries to use it, where it surfaces as a typed
    /// `ProducerError::OracleSpawn` naming the oracle path and the `dotnet
    /// publish` command that would produce it. This test calls `invoke`
    /// directly (via `produce`, already imported above) rather than through
    /// `ProducerRegistry::run`, because `SourceError::OracleFailed::detail`
    /// is built from `ProducerError`'s deliberately terse top-level
    /// `Display` (`"oracle spawn failed"` — AGENTS-DOCTRINE.md §8, "Print
    /// the whole `#[source]` chain") and would not itself contain the path.
    #[test]
    fn csharp_runner_missing_oracle_surfaces_at_invoke_not_at_construction() {
        // Point at an oracle path that provably does not exist, bypassing
        // `NUDOX_CSHARP_ORACLE`/the in-tree publish directory entirely, so
        // this test is independent of whether this host has ever published
        // the real oracle.
        let producer = CSharpProducer::new("/does/not/exist/oracle.dll");
        let src = PackageSource::new("/tmp", "test", "0.1.0");
        let lineage = PackageLineageId::new(EcosystemId::new("nuget"), PackageName::new("test"));
        match produce(&producer, &src, &lineage, &nudox_ir::foreign::Unlinked) {
            Err(ProducerError::OracleSpawn { command, reason }) => {
                assert!(
                    command.contains("does/not/exist"),
                    "OracleSpawn command should name the missing oracle path; got {command:?}"
                );
                assert!(
                    reason.to_string().contains("dotnet publish"),
                    "OracleSpawn reason should name the fix; got {reason}"
                );
            }
            other => panic!("expected OracleSpawn naming the missing oracle, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "pyrefly")]
    fn python_runner_is_reachable_when_pyrefly_feature_is_enabled() {
        // `has()` only proves an entry exists in the map; this proves `run()`
        // reaches the real `PythonProducer` through the type-erased
        // `TypedRunner` rather than short-circuiting to `ToolchainMissing`.
        // Only meaningful with the `pyrefly` feature on — without it there is
        // no runner registered at all (see
        // `with_all_available_has_no_runner_for_python_without_pyrefly_feature`),
        // so "reachable" would be vacuous.
        let reg = ProducerRegistry::with_all_available();
        let src = PackageSource::new("/does/not/exist", "test-pkg", "0.1.0");
        let lineage = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new("test-pkg"));
        let result = reg.run(Language::Python, &src, &lineage);
        if let Err(err) = &result {
            assert!(
                !matches!(err, SourceError::ToolchainMissing { .. }),
                "a runner is registered for Python; got {err:?}",
            );
        }
    }

    // -----------------------------------------------------------------
    // Unregistered / unavailable languages must name themselves in a typed
    // error, never fall through to an empty success (this is the root
    // property the whole audit is about). Go, Java, CSharp, and C/Cpp are
    // all registered by `with_all_available` now (see the positive tests
    // above and `register_c_and_cpp`'s own tests below) — Python without the
    // `pyrefly` feature is the one case this constructor still leaves
    // unregistered, so it is the only remaining language that can exercise
    // this invariant here.
    // -----------------------------------------------------------------

    #[test]
    #[cfg(not(feature = "pyrefly"))]
    fn run_names_the_unsupported_language_in_its_error() {
        let reg = ProducerRegistry::with_all_available();
        let src = PackageSource::new("/tmp", "test", "0.1.0");
        for language in [Language::Python] {
            let lineage =
                PackageLineageId::new(EcosystemId::new("test-ecosystem"), PackageName::new("test"));
            match reg.run(language, &src, &lineage) {
                Err(SourceError::ToolchainMissing { language: named, .. }) => {
                    assert_eq!(
                        named, language,
                        "error must name the language that was actually requested"
                    );
                }
                other => panic!("expected ToolchainMissing naming {language:?}, got {other:?}"),
            }
        }
    }

    #[test]
    #[cfg(not(feature = "pyrefly"))]
    fn python_lookup_yields_the_same_typed_error_shape_as_any_other_unsupported_language() {
        // Pins the exact `SourceError` variant (not just "is an Err") for the
        // one language `with_all_available` still leaves unregistered — the
        // regression this guards is a future change accidentally routing an
        // unregistered language through a different error path (or, worse,
        // to `Ok`) instead of the typed `ToolchainMissing` every unsupported
        // language must produce.
        let reg = ProducerRegistry::with_all_available();
        let src = PackageSource::new("/tmp", "test", "0.1.0");
        let lineage = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new("test"));
        assert!(matches!(
            reg.run(Language::Python, &src, &lineage),
            Err(SourceError::ToolchainMissing {
                language: Language::Python,
                ..
            })
        ));
    }

    // -----------------------------------------------------------------
    // register_c_and_cpp — the typed C/C++ unification.
    // -----------------------------------------------------------------

    /// A minimal stand-in for a libclang-based producer, used only to prove
    /// `register_c_and_cpp` wires one runner to both `Language::C` and
    /// `Language::Cpp`, independent of the real `ClangProducer` (registered
    /// separately in `with_all_available_resolves_a_runner_for_cpp` above).
    ///
    /// Its `lower` declares one real entry. That is not decoration: it used to
    /// declare nothing, and `nudox_producer::produce` now — correctly — refuses
    /// such a run as `ProducerError::NoDeclarationsContributed`. Of the two ways
    /// to keep the routing tests green, declaring `YieldContract::RootOnly` and
    /// declaring an entry, only the second leaves the tests *stronger*: an
    /// `is_ok()` from a producer that contributes something proves the runner
    /// was reached and driven end to end, where an `is_ok()` from a declared
    /// no-op would prove only that the degraded path is reachable. Doctrine §4:
    /// "a test that would pass against a stub is not a test".
    #[derive(Debug, Default, Clone, Copy)]
    struct StubCFamilyProducer;

    impl nudox_producer::Producer for StubCFamilyProducer {
        type Id = u32;
        type Oracle = ();

        const ID: nudox_producer::ProducerId = nudox_producer::ProducerId("stub-c-family/1");
        const LANGUAGE: Language = Language::C;

        fn invoke(&self, _src: &PackageSource) -> Result<Self::Oracle, ProducerError> {
            Ok(())
        }

        fn lower(
            &self,
            _oracle: &Self::Oracle,
            out: &mut nudox_ir::lower::Lowering<Self::Id>,
        ) -> Result<(), ProducerError> {
            out.declare(
                1,
                None,
                nudox_ir::entry::Symbol {
                    name: "stub_header".to_owned(),
                    visibility: nudox_ir::entry::Visibility::Public,
                    documentation: String::new(),
                    source: std::path::PathBuf::new(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                },
                nudox_ir::kinds::Module,
            );
            Ok(())
        }
    }

    #[test]
    fn register_c_and_cpp_resolves_a_runner_for_both_c_and_cpp() {
        let mut reg = ProducerRegistry::new();
        reg.register_c_and_cpp(StubCFamilyProducer);
        assert!(reg.has(Language::C));
        assert!(reg.has(Language::Cpp));
    }

    #[test]
    fn register_c_and_cpp_runner_is_reachable_via_cpp_not_just_via_c() {
        // `has()` only proves a map entry exists; this proves `run()` on
        // `Language::Cpp` reaches the same runner `Language::C` does, rather
        // than one of the two keys silently pointing at nothing.
        let mut reg = ProducerRegistry::new();
        reg.register_c_and_cpp(StubCFamilyProducer);
        let src = PackageSource::new("/tmp", "test", "0.1.0");

        let c_lineage = PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new("test"));
        let cpp_lineage = PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new("test"));
        assert!(
            reg.run(Language::C, &src, &c_lineage).is_ok(),
            "Language::C must reach the registered runner"
        );
        assert!(
            reg.run(Language::Cpp, &src, &cpp_lineage).is_ok(),
            "Language::Cpp must reach the same runner as Language::C, not ToolchainMissing"
        );
    }

    // -----------------------------------------------------------------
    // PackageDescriptor ecosystem constructors
    // -----------------------------------------------------------------

    #[test]
    fn each_ecosystem_constructor_pairs_the_matching_ecosystem_id_and_language() {
        let cases: [(PackageDescriptor, &str, Language); 7] = [
            (PackageDescriptor::cargo("/tmp", "a", "0.1"), "cargo", Language::Rust),
            (PackageDescriptor::go("/tmp", "a", "0.1"), "go", Language::Go),
            (
                PackageDescriptor::npm("/tmp", "a", "0.1"),
                "npm",
                Language::TypeScript,
            ),
            (
                PackageDescriptor::maven("/tmp", "a", "0.1"),
                "maven",
                Language::Java,
            ),
            (
                PackageDescriptor::nuget("/tmp", "a", "0.1"),
                "nuget",
                Language::CSharp,
            ),
            (
                PackageDescriptor::pypi("/tmp", "a", "0.1"),
                "pypi",
                Language::Python,
            ),
            (PackageDescriptor::cpp("/tmp", "a", "0.1"), "cpp", Language::C),
        ];
        for (desc, ecosystem, language) in cases {
            assert_eq!(desc.lineage.ecosystem.as_str(), ecosystem);
            assert_eq!(desc.language, language);
        }
    }
}
