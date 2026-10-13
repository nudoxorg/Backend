use super::*;
use crate::application::embedding_provision::tests::{
    install_protocol_fixture, remove_protocol_fixture_model,
};
use crate::application::{EmbeddingRuntimeProvision, PreparedEmbeddingRuntime};
use std::fs;

fn fixture_reply(
    sender: SyncSender<
        Result<LocalCompilerEmbeddingReceipt, LocalCompilerEmbeddingTransitionError>,
    >,
) -> EmbeddingTransitionReply {
    EmbeddingTransitionReply {
        sender,
        reservation: Arc::new(EmbeddingRefreshReservation {
            requests: Arc::new(Mutex::new(RequestAdmissions::Refreshing(1))),
            sequence: 1,
        }),
    }
}

fn root(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-embedding-generation-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(root)
}

#[test]
fn actual_compiler_generation_retires_credit_owners_and_installs_on_the_same_actor()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("same-actor")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let old = EmbeddingRuntimeProvision::open(&path)?;
    let runtime = old.runtime().ok_or("fixture activation unavailable")?;
    let account = runtime.memory_budget().clone();
    let deferred = Arc::new(DeferredNativePython::new()?);
    let configuration = deferred_native_python_tests::configuration(&root, Arc::clone(&deferred))
        .with_embedding_runtime(Arc::clone(&runtime), EmbeddingRequirement::Required);
    let mut client = LocalCompilerClient::start(configuration)?;
    let actor = client
        .shared
        .worker
        .as_ref()
        .ok_or("missing owner thread")?
        .thread()
        .id();
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("missing config")?;
    let desired = prepared.identity();
    let witness = prepared.configuration_witness();
    let retired = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            Arc::clone(&cancelled),
            deadline,
        )?
        .wait()?;
    assert_eq!(retired.configuration, desired);
    let LocalCompilerEmbeddingPhase::Retired {
        account: Some(retired_account),
    } = retired.phase
    else {
        return Err("retirement did not retain the actual account".into());
    };
    assert_eq!(retired_account.used(), account.used());
    assert!(
        retired_account.used() > 0,
        "external actual runtime still owns residency"
    );
    assert_eq!(client.retired_embedding_configuration(), Some(desired));
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Unavailable
    );
    let refusal = CompilerCapability::generate(
        &mut client,
        CompilerRequest {
            profile: LanguageProfile::Python(
                backend_semantic::vocabulary::PythonVersion::Python314,
            ),
            stage: Stage::LowerIr,
            source: "value = 1\n",
        },
    );
    assert!(matches!(
        refusal,
        Err(CompilerTerminal::Runtime {
            cause: CompilerRuntimeCause::RequestInFlight,
            ..
        })
    ));
    assert_eq!(
        deferred.admission_attempts.load(Ordering::Acquire),
        0,
        "retired compiler must not admit a deferred host"
    );
    let refused = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::ResumeUnavailable {
                configuration: witness.clone(),
                requirement: EmbeddingRequirement::Required,
                cause: EmbeddingProvisioningFailure::ActivationRejected,
            },
            Arc::clone(&cancelled),
            deadline,
        )?
        .wait();
    assert!(
        matches!(refused, Err(LocalCompilerEmbeddingTransitionError::RetainedMemory { used }) if used == account.used())
    );
    drop(old);
    drop(runtime);
    assert_eq!(account.used(), 0);
    let next = prepared.activate_until(&control)?;
    let next_runtime = next.runtime().ok_or("next runtime unavailable")?;
    let execution = next_runtime.execution_identity();
    let next_account = next_runtime.memory_budget().clone();
    drop(next_runtime);
    let pending_install = client.begin_embedding_transition(
        LocalCompilerEmbeddingTransition::Install {
            provision: next,
            cache: None,
        },
        Arc::clone(&cancelled),
        deadline,
    )?;
    let installed = loop {
        if let Some(receipt) = client.embedding_generation_receipt() {
            break receipt;
        }
        if Instant::now() >= deadline {
            return Err("actual actor replacement never committed".into());
        }
        thread::yield_now();
    };
    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        pending_install.wait(),
        Err(LocalCompilerEmbeddingTransitionError::Activation(
            backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Cancelled
            )
        ))
    ));
    assert_eq!(installed.configuration, desired);
    assert!(
        matches!(installed.phase, LocalCompilerEmbeddingPhase::Activated { execution: observed } if observed == execution)
    );
    assert_eq!(
        client
            .shared
            .worker
            .as_ref()
            .ok_or("missing owner")?
            .thread()
            .id(),
        actor
    );
    assert_eq!(client.retired_embedding_configuration(), None);
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Ready
    );
    cancelled.store(true, Ordering::Release);
    let observed = client
        .embedding_generation_receipt()
        .ok_or("committed receipt lost after late cancellation")?;
    assert_eq!(observed.configuration, desired);
    assert!(
        matches!(observed.phase, LocalCompilerEmbeddingPhase::Activated { execution: actual } if actual == execution)
    );
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Ready
    );
    drop(client);
    assert_eq!(account.used(), 0);
    assert_eq!(
        next_account.used(),
        0,
        "replacement actor releases its own runtime on final owner drop"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn actual_retirement_survives_waiter_loss_replays_exact_reference_and_rejects_rebinding()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("waiter-loss")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let deferred = Arc::new(DeferredNativePython::new()?);
    let client =
        LocalCompilerClient::start(deferred_native_python_tests::configuration(&root, deferred))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("missing config")?;
    let desired = prepared.identity();
    let witness = prepared.configuration_witness();
    drop(client.begin_embedding_transition(
        LocalCompilerEmbeddingTransition::Retire { desired },
        Arc::clone(&cancelled),
        deadline,
    )?);
    while client.retired_embedding_configuration().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(client.retired_embedding_configuration(), Some(desired));
    let repeated = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            Arc::clone(&cancelled),
            deadline,
        )?
        .wait()?;
    assert!(matches!(
        repeated.phase,
        LocalCompilerEmbeddingPhase::Retired { account: None }
    ));
    let other = root.join("other");
    backend_platform::durable::ensure_private_directory(&other)?;
    let (other_path, _) = install_protocol_fixture(&other, EmbeddingRequirement::Optional)?;
    let foreign = PreparedEmbeddingRuntime::capture_until(&other_path, &control)?
        .ok_or("missing other config")?
        .identity();
    assert_ne!(foreign, desired);
    let rebound = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired: foreign },
            Arc::clone(&cancelled),
            deadline,
        )?
        .wait();
    assert!(matches!(
        rebound,
        Err(LocalCompilerEmbeddingTransitionError::ConfigurationMismatch)
    ));
    assert_eq!(client.retired_embedding_configuration(), Some(desired));
    let response = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::ResumeUnavailable {
                configuration: witness.clone(),
                requirement: EmbeddingRequirement::Required,
                cause: EmbeddingProvisioningFailure::ModelUnavailable,
            },
            cancelled,
            deadline,
        )?
        .wait()?;
    assert!(matches!(
        response.phase,
        LocalCompilerEmbeddingPhase::Unavailable {
            cause: EmbeddingProvisioningFailure::ModelUnavailable
        }
    ));
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Ready,
        "native compiler serving does not imply embedding available"
    );
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn registered_work_and_original_cancellation_refuse_before_generation_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("refusal")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let deferred = Arc::new(DeferredNativePython::new()?);
    let client =
        LocalCompilerClient::start(deferred_native_python_tests::configuration(&root, deferred))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("missing config")?;
    let desired = prepared.identity();
    let facts = RequestFacts {
        profile: LanguageProfile::C(backend_semantic::vocabulary::CStandard::C23),
        language: Language::C,
        stage: Stage::LowerIr,
        target: None,
    };
    let lease = RequestLease::acquire(&client.shared, client.client_id, facts)
        .map_err(|cause| format!("fixture request admission failed: {cause:?}"))?;
    assert!(matches!(
        client.begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            Arc::clone(&cancelled),
            deadline
        ),
        Err(LocalCompilerEmbeddingTransitionError::Busy)
    ));
    assert_eq!(client.retired_embedding_configuration(), None);
    drop(lease);
    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        client.begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            Arc::clone(&cancelled),
            Instant::now()
        ),
        Err(LocalCompilerEmbeddingTransitionError::Activation(
            backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Cancelled
            )
        ))
    ));
    cancelled.store(false, Ordering::Release);
    assert!(matches!(
        client.begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            cancelled,
            Instant::now()
        ),
        Err(LocalCompilerEmbeddingTransitionError::Activation(
            backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Deadline
            )
        ))
    ));
    assert_eq!(client.retired_embedding_configuration(), None);
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Ready
    );
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn compiler_replacement_commit_checks_before_serving_and_preserves_late_observation()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("commit-cancel-boundary")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let flag = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(5);
    let control = backend_compile::EmbeddingActivationControl::new(&flag, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("prepared fixture")?;
    let desired = prepared.identity();
    let witness = prepared.configuration_witness();
    let signal = CapabilitySignal::new();
    *signal
        .lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        LocalCompilerGenerationState::Retired(desired);
    let receipt = LocalCompilerEmbeddingReceipt {
        configuration: desired,
        phase: LocalCompilerEmbeddingPhase::Unavailable {
            cause: EmbeddingProvisioningFailure::ActivationRejected,
        },
    };
    let (response, receiver) = sync_channel(1);
    flag.store(true, Ordering::Release);
    assert!(
        RuntimeGenerationAcknowledgement::Embedding {
            authority: EmbeddingTransitionAuthority::CompilerActor,
            response: fixture_reply(response),
            receipt: receipt.clone(),
            configuration: witness.clone(),
            cancelled: Arc::clone(&flag),
            deadline
        }
        .commit(&signal)
        .is_err()
    );
    assert!(matches!(
        receiver.recv()?,
        Err(LocalCompilerEmbeddingTransitionError::Activation(
            backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Cancelled
            )
        ))
    ));
    assert!(
        matches!(&*signal.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner), LocalCompilerGenerationState::Retired(actual) if *actual == desired)
    );
    flag.store(false, Ordering::Release);
    let (response, receiver) = sync_channel(1);
    RuntimeGenerationAcknowledgement::Embedding {
        authority: EmbeddingTransitionAuthority::CompilerActor,
        response: fixture_reply(response),
        receipt,
        configuration: witness.clone(),
        cancelled: Arc::clone(&flag),
        deadline,
    }
    .commit(&signal)
    .map_err(|()| "valid commit refused")?;
    flag.store(true, Ordering::Release);
    let committed = receiver.recv()??;
    assert_eq!(committed.configuration, desired);
    assert!(
        matches!(&*signal.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner), LocalCompilerGenerationState::Serving(Some(receipt)) if receipt.configuration == desired)
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn compiler_replacement_commit_revalidates_config_after_actual_activation()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("commit-config-drift")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let flag = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(5);
    let control = backend_compile::EmbeddingActivationControl::new(&flag, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("prepared config")?;
    let desired = prepared.identity();
    let provision = prepared.activate_until(&control)?;
    let runtime = provision
        .runtime()
        .ok_or("actual fixture activation missing")?;
    let account = runtime.memory_budget().clone();
    let execution = runtime.execution_identity();
    let original = fs::read(&path)?;
    // Reinstall a valid full configuration differing only by Required/Optional.
    // The existing fixture installer rewrites the same trusted artifacts/program.
    install_protocol_fixture(&root, EmbeddingRequirement::Optional)?;
    assert_ne!(fs::read(&path)?, original);
    let signal = CapabilitySignal::new();
    *signal
        .lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        LocalCompilerGenerationState::Retired(desired);
    let (response, receiver) = sync_channel(1);
    assert!(
        RuntimeGenerationAcknowledgement::Embedding {
            authority: EmbeddingTransitionAuthority::CompilerActor,
            response: fixture_reply(response),
            receipt: LocalCompilerEmbeddingReceipt {
                configuration: desired,
                phase: LocalCompilerEmbeddingPhase::Activated { execution }
            },
            configuration: provision
                .configuration_witness()
                .ok_or("activation missing config proof")?,
            cancelled: flag,
            deadline,
        }
        .commit(&signal)
        .is_err()
    );
    assert!(matches!(
        receiver.recv()?,
        Err(LocalCompilerEmbeddingTransitionError::Configuration(
            crate::application::EmbeddingRuntimeError::InvalidConfig
        ))
    ));
    assert!(
        matches!(&*signal.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner), LocalCompilerGenerationState::Retired(actual) if *actual == desired)
    );
    drop(provision);
    drop(runtime);
    assert_eq!(account.used(), 0);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn failed_activation_cannot_resume_a_drifted_configuration_on_the_actual_actor()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("failed-activation-config-drift")?;
    let (path, spec) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let flag = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&flag, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("prepared config")?;
    let desired = prepared.identity();
    // Remove the actual content-addressed model after capture, not a constructed failure.
    remove_protocol_fixture_model(&path, spec)?;
    let failure = prepared
        .activate_until(&control)
        .err()
        .ok_or("missing model activated")?;
    assert_eq!(
        failure.reason(),
        crate::application::EmbeddingUnavailableReason::ModelArtifactUnavailable
    );
    assert!(
        matches!(failure.cause(), crate::application::EmbeddingRuntimeError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
    );
    let configuration = failure.configuration_witness();
    assert_eq!(configuration.identity(), desired);
    let deferred = Arc::new(DeferredNativePython::new()?);
    let client =
        LocalCompilerClient::start(deferred_native_python_tests::configuration(&root, deferred))?;
    client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            Arc::clone(&flag),
            deadline,
        )?
        .wait()?;
    // A new valid config B now points to the same fixture artifacts with different policy.
    install_protocol_fixture(&root, EmbeddingRequirement::Optional)?;
    let next =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("replacement config")?;
    assert_ne!(next.identity(), desired);
    let refused = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::ResumeUnavailable {
                configuration,
                requirement: EmbeddingRequirement::Required,
                cause: EmbeddingProvisioningFailure::ModelUnavailable,
            },
            Arc::clone(&flag),
            deadline,
        )?
        .wait();
    assert!(matches!(
        refused,
        Err(LocalCompilerEmbeddingTransitionError::Configuration(
            crate::application::EmbeddingRuntimeError::InvalidConfig
        ))
    ));
    assert_eq!(client.retired_embedding_configuration(), Some(desired));
    assert!(
        client.embedding_generation_receipt().is_none(),
        "no stale unavailable A receipt is committed"
    );
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Unavailable
    );
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn actual_retired_actor_requires_exact_previous_and_current_proof_for_deliberate_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("retarget-retired")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let flag = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&flag, deadline);
    let first = PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("first config")?;
    let previous = first.identity();
    let old = EmbeddingRuntimeProvision::open(&path)?;
    let runtime = old.runtime().ok_or("old fixture runtime")?;
    let account = runtime.memory_budget().clone();
    let deferred = Arc::new(DeferredNativePython::new()?);
    let configuration = deferred_native_python_tests::configuration(&root, deferred)
        .with_embedding_runtime(Arc::clone(&runtime), EmbeddingRequirement::Required);
    let client = LocalCompilerClient::start(configuration)?;
    client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired: previous },
            Arc::clone(&flag),
            deadline,
        )?
        .wait()?;
    install_protocol_fixture(&root, EmbeddingRequirement::Optional)?;
    let corrected =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("corrected config")?;
    let next = corrected.identity();
    assert_ne!(next, previous);
    let wrong = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::RetargetRetired {
                previous: next,
                configuration: corrected.configuration_witness(),
            },
            Arc::clone(&flag),
            deadline,
        )?
        .wait();
    assert!(matches!(
        wrong,
        Err(LocalCompilerEmbeddingTransitionError::ConfigurationMismatch)
    ));
    assert_eq!(client.retired_embedding_configuration(), Some(previous));
    flag.store(true, Ordering::Release);
    assert!(
        client
            .begin_embedding_transition(
                LocalCompilerEmbeddingTransition::RetargetRetired {
                    previous,
                    configuration: corrected.configuration_witness()
                },
                Arc::clone(&flag),
                deadline
            )
            .is_err()
    );
    flag.store(false, Ordering::Release);
    install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let drift = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::RetargetRetired {
                previous,
                configuration: corrected.configuration_witness(),
            },
            Arc::clone(&flag),
            deadline,
        )?
        .wait();
    assert!(matches!(
        drift,
        Err(LocalCompilerEmbeddingTransitionError::Configuration(
            crate::application::EmbeddingRuntimeError::InvalidConfig
        ))
    ));
    assert_eq!(client.retired_embedding_configuration(), Some(previous));
    install_protocol_fixture(&root, EmbeddingRequirement::Optional)?;
    let receipt = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::RetargetRetired {
                previous,
                configuration: corrected.configuration_witness(),
            },
            Arc::clone(&flag),
            deadline,
        )?
        .wait()?;
    assert_eq!(receipt.configuration, next);
    let LocalCompilerEmbeddingPhase::Retired {
        account: Some(retained),
    } = receipt.phase
    else {
        return Err("retarget lost old shared account".into());
    };
    assert_eq!(retained.used(), account.used());
    assert!(
        retained.used() > 0,
        "retarget never detaches outstanding actual residency"
    );
    assert!(client.embedding_generation_receipt().is_none());
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Unavailable
    );
    drop(old);
    drop(runtime);
    assert_eq!(account.used(), 0);
    assert_eq!(retained.used(), 0);
    let provision = corrected.activate_until(&control)?;
    let installed = client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Install {
                provision,
                cache: None,
            },
            Arc::clone(&flag),
            deadline,
        )?
        .wait()?;
    assert_eq!(installed.configuration, next);
    assert!(matches!(
        installed.phase,
        LocalCompilerEmbeddingPhase::Activated { .. }
    ));
    assert_eq!(
        CompilerCapability::readiness(&client),
        CompilerReadiness::Ready
    );
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn actual_owner_outcome_retains_committed_receipt_after_original_observer_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("owned-outcome")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let flag = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&flag, deadline);
    let prepared = PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("config")?;
    let desired = prepared.identity();
    let deferred = Arc::new(DeferredNativePython::new()?);
    let client =
        LocalCompilerClient::start(deferred_native_python_tests::configuration(&root, deferred))?;
    client
        .begin_embedding_transition(
            LocalCompilerEmbeddingTransition::Retire { desired },
            Arc::clone(&flag),
            deadline,
        )?
        .observe_owner_outcome()?;
    let provision = prepared.activate_until(&control)?;
    let pending = client.begin_embedding_transition(
        LocalCompilerEmbeddingTransition::Install {
            provision,
            cache: None,
        },
        Arc::clone(&flag),
        deadline,
    )?;
    while client.embedding_generation_receipt().is_none() {
        if Instant::now() >= deadline {
            return Err("actor did not commit".into());
        }
        thread::yield_now();
    }
    flag.store(true, Ordering::Release);
    let receipt = pending.observe_owner_outcome()?;
    assert_eq!(receipt.configuration, desired);
    assert!(matches!(
        receipt.phase,
        LocalCompilerEmbeddingPhase::Activated { .. }
    ));
    assert_eq!(
        client
            .embedding_generation_receipt()
            .ok_or("lost serving receipt")?
            .configuration,
        desired
    );
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn compiler_replacement_commit_rechecks_original_workspace_fence_after_owner_handoff()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root("commit-owner-fence")?;
    let (path, _) = install_protocol_fixture(&root, EmbeddingRequirement::Required)?;
    let flag = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(5);
    let control = backend_compile::EmbeddingActivationControl::new(&flag, deadline);
    let prepared =
        PreparedEmbeddingRuntime::capture_until(&path, &control)?.ok_or("prepared config")?;
    let configuration = prepared.configuration_witness();
    let desired = prepared.identity();
    let owner_path = root.join("workspace");
    let owner = crate::workspace::owner::OwnerLease::acquire(&owner_path)?;
    let original = owner.test_identity();
    original.assert_current()?;
    let signal = CapabilitySignal::new();
    *signal
        .lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        LocalCompilerGenerationState::Retired(desired);
    let receipt = LocalCompilerEmbeddingReceipt {
        configuration: desired,
        phase: LocalCompilerEmbeddingPhase::Unavailable {
            cause: EmbeddingProvisioningFailure::ActivationRejected,
        },
    };
    // A genuine kernel-lease handoff changes the durable epoch/fence at the SAME path.
    // The configuration and original cancellation/deadline remain valid.
    drop(owner);
    let replacement = crate::workspace::owner::OwnerLease::acquire(&owner_path)?;
    assert!(replacement.epoch() > original.epoch());
    assert_ne!(replacement.fence(), original.fence());
    configuration.verify_current_until(&control)?;
    control.checkpoint()?;
    let (response, receiver) = sync_channel(1);
    assert!(
        RuntimeGenerationAcknowledgement::Embedding {
            authority: EmbeddingTransitionAuthority::Workspace(original),
            response: fixture_reply(response),
            receipt: receipt.clone(),
            configuration: configuration.clone(),
            cancelled: Arc::clone(&flag),
            deadline,
        }
        .commit(&signal)
        .is_err()
    );
    assert!(matches!(
        receiver.recv()?,
        Err(LocalCompilerEmbeddingTransitionError::WorkspaceOwner(
            crate::workspace::owner::WorkspaceError::Fenced
        ))
    ));
    assert!(
        matches!(&*signal.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        LocalCompilerGenerationState::Retired(actual) if *actual == desired)
    );
    // A deliberate operation admitted by the actual replacement may commit. Its
    // result remains observable after a late waiter cancellation, as before.
    let (response, receiver) = sync_channel(1);
    RuntimeGenerationAcknowledgement::Embedding {
        authority: EmbeddingTransitionAuthority::Workspace(replacement.test_identity()),
        response: fixture_reply(response),
        receipt,
        configuration,
        cancelled: Arc::clone(&flag),
        deadline,
    }
    .commit(&signal)
    .map_err(|()| "current owner commit refused")?;
    flag.store(true, Ordering::Release);
    assert_eq!(receiver.recv()??.configuration, desired);
    assert!(
        matches!(&*signal.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        LocalCompilerGenerationState::Serving(Some(receipt)) if receipt.configuration == desired)
    );
    drop(replacement);
    fs::remove_dir_all(root)?;
    Ok(())
}
