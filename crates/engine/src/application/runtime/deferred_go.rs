//! Owner-shared, request-controlled admission of the complete selected Go SDK.

use super::*;
use backend_frontend_go::legacy::{GoOracle, GoOracleChildEnvironment, GoOracleConfiguration};

/// Frozen Go paths, with no SDK contents read until a Go request needs authority.
#[derive(Debug)]
pub struct DeferredGoAuthority {
    configuration: GoOracleConfiguration,
    executable: PathBuf,
    goroot: PathBuf,
    module_cache: PathBuf,
    build_cache: PathBuf,
    ready: OnceLock<Result<ConfiguredGoOracle, LocalRuntimeGoAuthorityFailure>>,
    admission: Mutex<()>,
    observation_sent: AtomicBool,
    #[cfg(test)]
    admission_attempts: AtomicU64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_probe_wait_respects_sub_six_second_request_deadline_before_sdk_admission() {
        use backend_semantic::vocabulary::GoVersion;
        let executable = PathBuf::from("/not-read/deferred-go");
        let deferred = Arc::new(DeferredGoAuthority::new(
            GoOracleConfiguration::go_toolchain(executable.clone()).unwrap(),
            executable.clone(),
            PathBuf::from("/not-read/sdk"),
            PathBuf::from("/not-read/modules"),
            PathBuf::from("/not-read/build"),
        ));
        let timeout = Duration::from_millis(50);
        let configuration = LocalCompilerRuntimeConfiguration::new(
            LocalCompilerRuntimePaths::new(
                PathBuf::from("/not-read/artifacts"),
                PathBuf::from("/not-read/journal"),
                PathBuf::from("/not-read/native"),
            )
            .unwrap(),
            vec![LocalRuntimeToolchain::probing(
                NativeTool::GoCompiler,
                executable,
            )]
            .into_boxed_slice(),
            Box::new([]),
            LocalRuntimePackageAuthority {
                deferred_go: Some(Arc::clone(&deferred)),
                ..Default::default()
            },
            LocalCompilerTimeout::new(timeout).unwrap(),
            PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN).unwrap(),
            LocalCompilerScratch::default(),
        )
        .unwrap();
        // Use the existing channel/lease fixture without starting a worker or any probe child.
        // Its version observation remains genuinely pending for this entire bounded call.
        let mut shared = super::super::request_lease_tests::shared();
        shared.capabilities = Arc::new(RwLock::new(configuration.capabilities()));
        shared.deferred_go = Some(Arc::clone(&deferred));
        shared.native_python_timeout = configuration.timeout;
        let (sender, observations) = channel();
        shared.native_python_observations = sender;
        let client = LocalCompilerClient {
            shared: Arc::new(shared),
            client_id: 1,
            last_image: Mutex::new(None),
        };
        let profile = LanguageProfile::Go(GoVersion::Go125);
        assert_eq!(
            client.capabilities().for_profile(profile).state(),
            LocalCompilerCapabilityState::Probing
        );
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        let result = client.wait_for_toolchain(
            RequestFacts {
                profile,
                language: Language::Go,
                stage: Stage::LowerIr,
                target: None,
            },
            &cancelled,
        );
        assert!(matches!(
            result,
            Err(CompilerTerminal::Runtime {
                cause: CompilerRuntimeCause::ToolchainProbeTimeout,
                ..
            })
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a 50ms request must not inherit the independent six-second probe allowance"
        );
        assert!(deferred.is_pending());
        assert_eq!(deferred.admission_attempts.load(Ordering::Relaxed), 0);
        assert!(
            observations.try_recv().is_err(),
            "no authority observation on expiry"
        );
        assert!(
            client
                .capabilities()
                .for_profile(profile)
                .local_authority_fingerprint()
                .is_none()
        );
    }

    #[test]
    fn deferred_go_configuration_rejects_conflicting_or_unmatched_authority() {
        let executable = PathBuf::from("/not-read/deferred-go");
        let deferred = Arc::new(DeferredGoAuthority::new(
            GoOracleConfiguration::go_toolchain(executable.clone()).unwrap(),
            executable.clone(),
            PathBuf::from("/not-read/sdk"),
            PathBuf::from("/not-read/modules"),
            PathBuf::from("/not-read/build"),
        ));
        let configuration = |authority, rows| {
            LocalCompilerRuntimeConfiguration::new(
                LocalCompilerRuntimePaths::new(
                    PathBuf::from("/not-read/artifacts"),
                    PathBuf::from("/not-read/journal"),
                    PathBuf::from("/not-read/native"),
                )
                .unwrap(),
                rows,
                Box::new([]),
                authority,
                LocalCompilerTimeout::new(Duration::from_secs(1)).unwrap(),
                PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN).unwrap(),
                LocalCompilerScratch::default(),
            )
        };
        for conflict in 0..4 {
            let mut authority = LocalRuntimePackageAuthority {
                deferred_go: Some(Arc::clone(&deferred)),
                ..Default::default()
            };
            if conflict == 0 {
                authority.go = Some(GoOracle::default().with_configuration(
                    GoOracleConfiguration::go_toolchain(executable.clone()).unwrap(),
                ));
            }
            if conflict == 1 {
                authority.go_unavailable = Some(LocalRuntimeGoAuthorityFailure::OracleUnavailable);
            }
            let rows: Box<[LocalRuntimeToolchain]> = if conflict == 2 {
                Box::new([])
            } else {
                vec![
                    LocalRuntimeToolchain::resolved(
                        NativeTool::GoCompiler,
                        if conflict == 3 {
                            PathBuf::from("/different/go")
                        } else {
                            executable.clone()
                        },
                        b"go version go1.25.0 linux/amd64",
                    )
                    .unwrap(),
                ]
                .into_boxed_slice()
            };
            assert!(matches!(
                configuration(authority, rows),
                Err(LocalCompilerRuntimeConfigurationError::GoAdmissionPair)
            ));
        }
        assert!(
            configuration(
                LocalRuntimePackageAuthority {
                    deferred_go: Some(Arc::clone(&deferred)),
                    ..Default::default()
                },
                vec![
                    LocalRuntimeToolchain::resolved(
                        NativeTool::GoCompiler,
                        executable,
                        b"go version go1.25.0 linux/amd64"
                    )
                    .unwrap()
                ]
                .into_boxed_slice()
            )
            .is_ok()
        );
        assert_eq!(
            deferred.admission_attempts.load(Ordering::Relaxed),
            0,
            "consistent configuration still performs no SDK admission"
        );
    }

    #[test]
    fn cancelled_and_expired_go_admission_never_walk_or_publish_and_remain_retryable() {
        // No paths exist: cancellation/deadline must win before even one SDK admission attempt.
        let executable = PathBuf::from("/not-read/deferred-go");
        let deferred = DeferredGoAuthority::new(
            GoOracleConfiguration::go_toolchain(executable.clone()).unwrap(),
            executable,
            PathBuf::from("/not-read/sdk"),
            PathBuf::from("/not-read/modules"),
            PathBuf::from("/not-read/build"),
        );
        let (sender, observations) = channel();
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            deferred.resolve(
                GoOracleControl::new(Some(&cancelled), Instant::now() + Duration::from_secs(1)),
                &sender
            ),
            Err(OracleError::Cancelled)
        ));
        cancelled.store(false, Ordering::Release);
        let expired = deferred
            .resolve(
                GoOracleControl::new(Some(&cancelled), Instant::now() - Duration::from_millis(1)),
                &sender,
            )
            .unwrap_err();
        assert!(GoOracleControl::deadline_phase(&expired).is_some());
        assert!(deferred.is_pending());
        assert_eq!(deferred.admission_attempts.load(Ordering::Relaxed), 0);
        assert!(observations.try_recv().is_err());
        // A later live request actually attempts admission and retains the existing provider
        // failure rather than a cancellation/expiry result from the earlier requests.
        deferred
            .resolve(
                GoOracleControl::new(Some(&cancelled), Instant::now() + Duration::from_secs(1)),
                &sender,
            )
            .unwrap();
        assert_eq!(deferred.admission_attempts.load(Ordering::Relaxed), 1);
        assert_eq!(
            deferred.failure(),
            Some(LocalRuntimeGoAuthorityFailure::ExecutableUnavailable)
        );
        assert!(observations.try_recv().unwrap().result.is_none());
    }
}

impl DeferredGoAuthority {
    pub(crate) fn new(
        configuration: GoOracleConfiguration,
        executable: PathBuf,
        goroot: PathBuf,
        module_cache: PathBuf,
        build_cache: PathBuf,
    ) -> Self {
        Self {
            configuration,
            executable,
            goroot,
            module_cache,
            build_cache,
            ready: OnceLock::new(),
            admission: Mutex::new(()),
            observation_sent: AtomicBool::new(false),
            #[cfg(test)]
            admission_attempts: AtomicU64::new(0),
        }
    }

    pub(super) fn selected_executable(&self) -> &Path {
        &self.executable
    }

    pub(super) fn admitted(&self) -> Option<&ConfiguredGoOracle> {
        self.ready.get().and_then(|result| result.as_ref().ok())
    }

    pub(super) fn failure(&self) -> Option<LocalRuntimeGoAuthorityFailure> {
        self.ready
            .get()
            .and_then(|result| result.as_ref().err())
            .copied()
    }

    pub(super) fn is_pending(&self) -> bool {
        self.ready.get().is_none()
    }

    pub(super) fn resolve(
        &self,
        control: GoOracleControl<'_>,
        observations: &Sender<ToolchainProbeObservation>,
    ) -> Result<(), OracleError> {
        let _admission = loop {
            control.check("authority_admission")?;
            match self.admission.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
            }
        };
        control.check("authority_admission")?;
        if self.ready.get().is_none() {
            #[cfg(test)]
            self.admission_attempts.fetch_add(1, Ordering::Relaxed);
            let admitted = GoOracleChildEnvironment::new_controlled(
                self.executable.clone(),
                self.goroot.clone(),
                self.module_cache.clone(),
                self.build_cache.clone(),
                control,
            )
            .and_then(|environment| {
                GoOracle::default()
                    .with_configuration(self.configuration.clone())
                    .with_child_environment(environment)
            });
            // A cancelled/expired partial walk is retryable and cannot publish a provider failure.
            control.check("authority_admission")?;
            let admitted = admitted.map_err(|error| {
                super::super::host::go_authority_failure(
                    &super::super::host::LocalCompilerHostError::GoAuthority(error),
                )
            });
            let _ = self.ready.set(admitted);
        }
        if !self.observation_sent.load(Ordering::Acquire) {
            // Rebuild the compiler generation with the completed authority (or original typed
            // provider refusal). Keep the independently observed version row unchanged.
            observations
                .send(ToolchainProbeObservation {
                    tool: NativeTool::GoCompiler,
                    result: None,
                })
                .map_err(|_| OracleError::ToolingUnavailable {
                    tool: "compiler owner",
                    source: std::io::Error::other("compiler owner stopped"),
                })?;
            self.observation_sent.store(true, Ordering::Release);
        }
        Ok(())
    }
}
