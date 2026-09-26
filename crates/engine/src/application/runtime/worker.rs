//! Compiler-owner thread: toolchain probes and one generation at a time.
//!
//! The client transfers owned requests through a one-slot channel. This module
//! owns the thread that admits configuration, applies probe results, and runs
//! each command against the borrowed compiler.

use std::{
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender},
    },
    thread,
    time::Duration,
};

use arrayvec::ArrayVec;
use backend_frontend_csharp::legacy::CSharpAuthorityConfiguration;
use backend_frontend_rust::legacy::RustFeatureControl;
use backend_library::interface::{
    CompilerCapability, CompilerRequest, CompilerRuntimeCause, SemanticImageAccessError,
};
use backend_semantic::vocabulary::{NativeTool, Stage};

use crate::application::toolchain_probe::{ToolchainProbeError, ToolchainProbeLimits};
use crate::application::{
    CSharpPackageAuthorityConfiguration, JavaPackageAuthorityConfiguration, LocalCompiler,
    LocalCompilerConfig, LocalCompilerControl, LocalPackageRoot, LocalPackageRootSet,
    LocalToolchainSet, MAX_LOCAL_PACKAGE_ROOTS, MAX_LOCAL_TOOLCHAINS,
    PackageAuthorityConfiguration, PackageSourceSet, RustPackageAuthorityConfiguration,
};
use crate::driver::ToolchainSelection;

use super::{
    CapabilitySignal, LocalCompilerCapabilities, LocalCompilerRuntimeConfiguration,
    LocalCompilerRuntimeOpenError, LocalRuntimeToolchain, OwnedCompilerRequest,
    PackageSemanticRuntimeError, RequestFacts, RuntimeCommand, RuntimeEvent,
    borrow_package_sources,
};

pub(super) struct ToolchainProbeObservation {
    tool: NativeTool,
    result: Result<LocalRuntimeToolchain, ToolchainProbeError>,
}

pub(super) fn start_toolchain_probes(
    pending: Vec<(NativeTool, PathBuf)>,
    observations: Sender<ToolchainProbeObservation>,
) {
    let limits = ToolchainProbeLimits::new(
        Duration::from_secs(5),
        NonZeroUsize::new(16 * 1024).expect("fixed probe stream bound is nonzero"),
    )
    .expect("fixed probe deadline is nonzero");
    for (tool, executable) in pending {
        let probe_observations = observations.clone();
        let name = format!("nudox-toolchain-{}-probe", u8::from(tool));
        let spawn = thread::Builder::new().name(name).spawn(move || {
            let result = LocalRuntimeToolchain::probe(tool, executable, limits);
            let _ = probe_observations.send(ToolchainProbeObservation { tool, result });
        });
        if spawn.is_err() {
            let _ = observations.send(ToolchainProbeObservation {
                tool,
                result: Ok(LocalRuntimeToolchain::probe_failed(tool)),
            });
        }
    }
}

pub(super) fn run_worker(
    mut configuration: LocalCompilerRuntimeConfiguration,
    commands: Receiver<RuntimeCommand>,
    probes: Receiver<ToolchainProbeObservation>,
    startup: SyncSender<Result<(), LocalCompilerRuntimeOpenError>>,
    cancelled: &AtomicBool,
    alive: &AtomicBool,
    capabilities: &RwLock<LocalCompilerCapabilities>,
    capability_signal: &CapabilitySignal,
) {
    let mut startup = Some(startup);
    loop {
        match run_worker_generation(
            &mut configuration,
            &commands,
            &probes,
            &mut startup,
            cancelled,
            alive,
        ) {
            WorkerDisposition::Stop => break,
            WorkerDisposition::Reconfigure(observation) => {
                let replacement = observation
                    .result
                    .unwrap_or_else(|_| LocalRuntimeToolchain::probe_failed(observation.tool));
                let Some(slot) = configuration
                    .toolchains
                    .iter_mut()
                    .find(|candidate| candidate.tool == observation.tool)
                else {
                    alive.store(false, Ordering::Release);
                    break;
                };
                *slot = replacement;
                *capabilities
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    LocalCompilerCapabilities::from_configuration(&configuration);
                capability_signal.changed.notify_all();
            }
        }
    }
    alive.store(false, Ordering::Release);
}

enum WorkerDisposition {
    Stop,
    Reconfigure(ToolchainProbeObservation),
}

fn run_worker_generation(
    configuration: &mut LocalCompilerRuntimeConfiguration,
    commands: &Receiver<RuntimeCommand>,
    probes: &Receiver<ToolchainProbeObservation>,
    startup: &mut Option<SyncSender<Result<(), LocalCompilerRuntimeOpenError>>>,
    cancelled: &AtomicBool,
    alive: &AtomicBool,
) -> WorkerDisposition {
    let mut toolchains = ArrayVec::<ToolchainSelection<'_>, MAX_LOCAL_TOOLCHAINS>::new();
    for (ordinal, owned) in configuration.toolchains.iter().enumerate() {
        match owned.selection() {
            Ok(selection) => toolchains.push(selection),
            Err(source) => {
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(LocalCompilerRuntimeOpenError::Toolchain {
                        ordinal,
                        source,
                    }));
                }
                alive.store(false, Ordering::Release);
                return WorkerDisposition::Stop;
            }
        }
    }
    let toolchains = match LocalToolchainSet::validate(&toolchains) {
        Ok(toolchains) => toolchains,
        Err(error) => {
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(error.into()));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        }
    };

    let mut package_roots = ArrayVec::<LocalPackageRoot<'_>, MAX_LOCAL_PACKAGE_ROOTS>::new();
    for (ordinal, owned) in configuration.package_roots.iter().enumerate() {
        match LocalPackageRoot::new(owned.ecosystem, &owned.path) {
            Ok(root) => package_roots.push(root),
            Err(source) => {
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(LocalCompilerRuntimeOpenError::PackageRoot {
                        ordinal,
                        source,
                    }));
                }
                alive.store(false, Ordering::Release);
                return WorkerDisposition::Stop;
            }
        }
    }
    let package_roots = match LocalPackageRootSet::validate(&package_roots) {
        Ok(package_roots) => package_roots,
        Err(error) => {
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(error.into()));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        }
    };

    let rust_feature_storage = configuration.package_authority.rust.as_ref().map(|rust| {
        rust.features
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>()
    });
    let java_classpath_storage = configuration.package_authority.java.as_ref().map(|java| {
        java.classpath
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&Path>>()
    });
    let csharp_define_storage = configuration
        .package_authority
        .csharp
        .as_ref()
        .map(|csharp| {
            csharp
                .define_symbols
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
        });
    let csharp_using_storage = configuration
        .package_authority
        .csharp
        .as_ref()
        .map(|csharp| {
            csharp
                .extra_usings
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
        });
    let rust = configuration.package_authority.rust.as_ref().map(|rust| {
        RustPackageAuthorityConfiguration {
            toolchain: &rust.toolchain,
            maximum_source_bytes: rust.maximum_source_bytes,
            features: RustFeatureControl {
                all_features: rust.all_features,
                no_default_features: rust.no_default_features,
                features: rust_feature_storage.as_deref().unwrap_or_default(),
            },
        }
    });
    let java = configuration.package_authority.java.as_ref().map(|java| {
        JavaPackageAuthorityConfiguration {
            toolchain: &java.toolchain,
            classpath: java_classpath_storage.as_deref().unwrap_or_default(),
        }
    });
    let csharp = configuration
        .package_authority
        .csharp
        .as_ref()
        .map(|csharp| CSharpPackageAuthorityConfiguration {
            producer: &csharp.producer,
            configuration: CSharpAuthorityConfiguration {
                assembly_name: csharp.assembly_name.as_deref(),
                reference_directory: csharp.reference_directory.as_deref(),
                define_symbols: csharp_define_storage.as_deref().unwrap_or_default(),
                extra_usings: csharp_using_storage.as_deref().unwrap_or_default(),
                implicit_usings: csharp.implicit_usings,
                include_non_public: csharp.include_non_public,
                maximum_source_bytes: csharp.maximum_source_bytes,
            },
        });
    let authority = PackageAuthorityConfiguration {
        typescript: configuration.package_authority.typescript.as_ref(),
        python: configuration.package_authority.python.as_ref(),
        rust,
        go: configuration.package_authority.go.as_ref(),
        csharp,
        java,
        maximum_image_bytes: configuration
            .package_authority
            .maximum_image_bytes
            .map_or(0, NonZeroUsize::get),
    };
    let config = LocalCompilerConfig {
        toolchains,
        artifact_directory: &configuration.paths.artifact_directory,
        journal_directory: &configuration.paths.journal_directory,
        native_work_directory: &configuration.paths.native_work_directory,
        control: LocalCompilerControl {
            timeout: configuration.timeout,
            cancelled,
        },
    };
    let mut compiler = match LocalCompiler::create_with_package_authority(
        config,
        package_roots,
        authority,
        configuration.publication_limits,
        &mut configuration.scratch,
    ) {
        Ok(compiler) => compiler,
        Err(error) => {
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(error.into()));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        }
    };
    if let Some(startup) = startup.take()
        && startup.send(Ok(())).is_err()
    {
        alive.store(false, Ordering::Release);
        let _ = compiler.shutdown();
        return WorkerDisposition::Stop;
    }

    loop {
        if let Ok(observation) = probes.try_recv() {
            let _ = compiler.shutdown();
            return WorkerDisposition::Reconfigure(observation);
        }
        let command = match commands.recv_timeout(Duration::from_millis(5)) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        cancelled.store(false, Ordering::Release);
        match command {
            RuntimeCommand::Compile { request, response } => {
                let facts = request.facts();
                let result = catch_unwind(AssertUnwindSafe(|| match &request {
                    OwnedCompilerRequest::Generate {
                        profile,
                        stage,
                        source,
                    } => compiler.generate(CompilerRequest {
                        profile: *profile,
                        stage: *stage,
                        source,
                    }),
                    OwnedCompilerRequest::Package(request) => {
                        compiler.compile_package(request, &mut |phase| {
                            if response.send(RuntimeEvent::Phase(phase)).is_err() {
                                cancelled.store(true, Ordering::Release);
                            }
                        })
                    }
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(RuntimeEvent::Complete(result));
                    }
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(
                            payload.as_ref(),
                        );
                        let _ = response.send(RuntimeEvent::Complete(Err(
                            facts.terminal(CompilerRuntimeCause::WorkerPanic(cause))
                        )));
                        break;
                    }
                }
            }
            RuntimeCommand::SemanticImage {
                requested,
                response,
            } => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    compiler.semantic_image_snapshot(requested)
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(result);
                    }
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(
                            payload.as_ref(),
                        );
                        let _ = response.send(Err(SemanticImageAccessError::WorkerPanic {
                            requested,
                            cause,
                        }));
                        break;
                    }
                }
            }
            RuntimeCommand::CompilePackageSources { request, response } => {
                let facts = request.facts();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let sources = borrow_package_sources(&request.sources)?;
                    let borrowed =
                        PackageSourceSet::new(&request.request, &request.package_root, &sources)?;
                    compiler
                        .compile_package_sources(borrowed, &mut |_| {})
                        .map_err(PackageSemanticRuntimeError::from)
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(result);
                    }
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(
                            payload.as_ref(),
                        );
                        let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(
                            facts.terminal(CompilerRuntimeCause::WorkerPanic(cause)),
                        )));
                        break;
                    }
                }
            }
            RuntimeCommand::ActivateSemanticGeneration {
                profile,
                manifest,
                binding,
                response,
            } => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    compiler
                        .activate_semantic_generation(manifest, binding)
                        .map_err(PackageSemanticRuntimeError::from)
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(result);
                    }
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(
                            payload.as_ref(),
                        );
                        let terminal = RequestFacts {
                            profile,
                            language: profile.language(),
                            stage: Stage::LowerIr,
                            target: None,
                        }
                        .terminal(CompilerRuntimeCause::WorkerPanic(cause));
                        let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(terminal)));
                        break;
                    }
                }
            }
        }
    }
    let _ = compiler.shutdown();
    WorkerDisposition::Stop
}
