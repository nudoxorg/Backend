//! Exercises public index-operation lifecycle receipts across service restarts.

#![cfg(any(unix, windows))]
#![allow(clippy::expect_used, clippy::panic)]

// The public service/client lifecycle uses the repository's AF_UNIX transport.
// Unix and Windows implement it; other targets intentionally do not expose a
// local listener, so this integration test is not compiled there.

use backend_client::{ClientError, Session};
use backend_library::{
    CommandFailure, CommandReply, CompileExecutionIntent, DeclarationKind, IndexOperationKey,
    IndexOperationObservation, IndexOperationPublicationReceipt, IndexOperationState, PackageKey,
    PackageReference, RowId, SemanticCallableCarrierBindings, SemanticDeclarationShape,
    SemanticShapeBudget, SemanticTypeExpr, SemanticTypeFact,
};
use backend_local_service::{
    EmbeddedLocalService, FrameLimits, ListenerError, LocalHostVariable, ProcessConfig,
    ProcessError, RunReport,
};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const PUBLIC_MARKER: &str = "operation_lifecycle_public_marker";

#[test]
fn public_index_operation_replays_and_conflicts_across_restart() -> Result<(), Box<dyn Error>> {
    let mut fixture = FailureFixture::new(lifecycle_tempdir()?);
    let result = run_public_index_operation_lifecycle(&fixture, false);
    if result.is_err() {
        fixture.preserve_after_failure();
    }
    result
}

#[test]
fn first_real_compiler_refusal_has_no_selected_authority_after_cold_reopen()
-> Result<(), Box<dyn Error>> {
    let mut fixture = FailureFixture::new(lifecycle_tempdir()?);
    let result = run_public_index_operation_lifecycle(&fixture, true);
    if result.is_err() {
        fixture.preserve_after_failure();
    }
    result
}
fn run_public_index_operation_lifecycle(
    fixture: &FailureFixture,
    first_refusal: bool,
) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::symlink_metadata(fixture.path())?.permissions().mode() & 0o777,
            0o700,
            "the temporary project root must be owner-only before workspace initialization"
        );
    }
    let service_workspace = fixture.path().join("service-state");
    let endpoint = fixture.path().join("owner.sock");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().to_path_buf()),
        Some(service_workspace),
        Some(endpoint),
    )?;
    let authority_secret = paths.authority_secret().to_path_buf();
    assert_eq!(
        authority_secret.parent(),
        Some(paths.data()),
        "the fixture authority credential must stay inside its selected workspace"
    );
    paths.initialize()?;
    let authority_credential = fs::read(&authority_secret)?;
    assert_eq!(authority_credential.len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(paths.data())?.permissions().mode() & 0o777,
            0o700,
            "the initialized workspace directory must be owner-only"
        );
        assert_eq!(
            fs::metadata(&authority_secret)?.permissions().mode() & 0o777,
            0o600,
            "the initialized authority credential must be owner-only"
        );
    }
    assert!(
        backend_engine::UnixEndpointRef::new(paths.endpoint()).is_ok(),
        "the platform temp root leaves no room for the portable AF_UNIX endpoint"
    );

    let package_root = fixture.path().join("real-cargo-package");
    fs::create_dir_all(package_root.join("src"))?;
    fs::write(
        package_root.join("Cargo.toml"),
        if first_refusal {
            "[package]\nname = \"public_operation_lifecycle_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nmissing_dependency = { path = \"missing_dependency\" }\n"
        } else {
            "[package]\nname = \"public_operation_lifecycle_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
        },
    )?;
    fs::write(
        package_root.join("src/lib.rs"),
        format!(
            "pub mod child;\npub struct MorningSignal {{ pub pulse: u32 }}\npub fn cadence8(take: MorningSignal) -> MorningSignal {{ take }}\npub fn {PUBLIC_MARKER}() -> u32 {{ child::answer() }}\n"
        ),
    )?;
    fs::write(
        package_root.join("src/child.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )?;
    let package_lock = package_root.join("Cargo.lock");
    assert!(!package_lock.exists(), "the Cargo input starts lockless");

    let package =
        PackageReference::parse(package_root.canonicalize()?.to_string_lossy().into_owned())
            .map_err(|error| {
                io::Error::other(format!("invalid real fixture package reference: {error}"))
            })?;
    let key_file = fixture.path().join("caller-operation-key.txt");
    let operation_key = IndexOperationKey::from_bytes([0x6d; 32])
        .map_err(|error| io::Error::other(format!("invalid fixture operation key: {error}")))?;
    {
        let mut key_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&key_file)?;
        key_file.write_all(operation_key.to_hex().as_bytes())?;
        key_file.sync_all()?;
    }

    let mut config = ProcessConfig::parse([
        "--endpoint".to_owned(),
        paths.endpoint().to_string_lossy().into_owned(),
        "--workspace".to_owned(),
        paths.data().to_string_lossy().into_owned(),
        "--registry-offline".to_owned(),
        "--registry-discovery-offline".to_owned(),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ])?;
    config.profile = "builtin".to_owned();
    config.worker_endpoint = None;
    config.authority_secret = Some(authority_secret.clone());
    assert_eq!(
        config.authority_secret.as_deref(),
        Some(authority_secret.as_path())
    );
    let mut compiler_environment = vec![
        (LocalHostVariable::NudoxRustc, executable_in_path("rustc")?),
        (LocalHostVariable::NudoxCargo, executable_in_path("cargo")?),
        (LocalHostVariable::NudoxCargoHome, cargo_home()?),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if home.is_absolute() {
            compiler_environment.push((LocalHostVariable::Home, home));
        }
    }
    config.compiler_environment = Some(
        backend_local_service::ClosedLocalHostEnvironmentSnapshot::from_paths(
            compiler_environment,
        )?,
    );

    let owner = with_phase_context(
        "start initial embedded owner",
        EmbeddedLocalService::start(config.clone()),
    )?;
    let mut session = with_phase_context(
        "connect initial authenticated public Session",
        Session::connect(owner.endpoint()),
    )?;
    let started = with_phase_context(
        "submit caller-keyed interactive index operation",
        session.start_index_operation(
            operation_key,
            package.clone(),
            CompileExecutionIntent::Interactive,
        ),
    )?;
    if first_refusal {
        let failed = with_phase_context(
            "await actual first Cargo dependency refusal",
            wait_for_failed(&mut session, operation_key, started),
        )?;
        let capture = failed
            .source_capture
            .as_ref()
            .ok_or_else(|| io::Error::other("first refusal omitted its captured input"))?;
        assert!(!capture.profiles().is_empty());
        assert!(
            capture.profiles().iter().all(|profile| matches!(
                profile.state,
                backend_library::IndexOperationSemanticProfileState::Unavailable { .. }
            )),
            "a failed first compile cannot invent a prior or a published generation"
        );
        assert_first_refusal_has_no_authority(&mut session, &package)?;
        let expected = IndexOperationObservation::Known(failed);
        drop(session);
        let report = with_phase_context("close first refused owner", owner.close())?;
        assert_eq!(
            report.failures, 0,
            "typed unavailable queries are domain refusals"
        );
        let cold_owner = with_phase_context(
            "cold reopen first compiler refusal",
            EmbeddedLocalService::start(config),
        )?;
        let mut cold_session = with_phase_context(
            "connect first-refusal cold owner",
            Session::connect(cold_owner.endpoint()),
        )?;
        assert_eq!(
            cold_session.index_operation_status(operation_key)?,
            expected
        );
        assert_first_refusal_has_no_authority(&mut cold_session, &package)?;
        let before_replay = cold_session.revision()?.root;
        assert_eq!(
            cold_session.start_index_operation(
                operation_key,
                package,
                CompileExecutionIntent::Interactive
            )?,
            expected
        );
        assert_eq!(cold_session.revision()?.root, before_replay);
        drop(cold_session);
        let report = with_phase_context("close first-refusal cold owner", cold_owner.close())?;
        assert_eq!(report.failures, 0);
        return Ok(());
    }

    let published = match with_phase_context(
        "poll initial operation until publication",
        wait_for_published(&mut session, operation_key, started),
    ) {
        Ok(published) => published,
        Err(phase) => {
            let owner_running_before_cleanup = owner.is_running();
            drop(session);
            let second_session_health = probe_second_session_health(owner.endpoint());
            let owner_finish = OwnerFinishDiagnostic::from_result(owner.close());
            return Err(Box::new(LifecycleFailureDiagnostic {
                phase,
                owner_running_before_cleanup,
                second_session_health,
                owner_finish,
            }));
        }
    };
    assert_eq!(published.package, package);
    assert_eq!(
        published.execution_intent,
        CompileExecutionIntent::Interactive
    );
    let IndexOperationState::Published(receipt) = &published.state else {
        panic!("the first real Cargo index operation must publish");
    };
    assert!(
        receipt.request_identity().is_some(),
        "the source package must produce a durable workspace commit"
    );
    assert!(receipt.workspace_sequence() > 0);
    assert_ne!(receipt.commit_identity(), &[0; 32]);
    let first_capture = published.source_capture.as_ref().ok_or_else(|| {
        io::Error::other("initial publication omitted its durable source capture")
    })?;
    assert!(first_capture.profiles().iter().all(|profile| matches!(
        profile.state,
        backend_library::IndexOperationSemanticProfileState::Published { .. }
    )));
    assert!(
        !package_lock.exists(),
        "Cargo metadata must keep its generated lock out of the source package"
    );

    let names = with_phase_context(
        "query the published marker through public Session",
        session.names(PUBLIC_MARKER, 16),
    )?;
    let CommandReply::Names(names) = names.reply else {
        panic!("authenticated name query must return the name view");
    };
    let marker_suffix = format!("::{PUBLIC_MARKER}");
    let returned_rows = names
        .root
        .rows()
        .iter()
        .map(|row| (row.label.as_str(), row.kind.as_ref()))
        .collect::<Vec<_>>();
    assert!(
        returned_rows
            .iter()
            .any(|(label, kind)| label.ends_with(&marker_suffix) && kind.is_some()),
        "the public Session did not return the real declaration at its qualified terminal coordinate; returned rows: {returned_rows:?}"
    );

    let cadence = selected_symbol_by_name(&mut session, "cadence8")?;
    let signal = selected_symbol_by_name(&mut session, "MorningSignal")?;
    let cadence_package = selected_package_by_symbol(&mut session, "cadence8", cadence)?;
    // Resolve the parameter carrier through public name search. The result
    // carrier is a synthetic IR entity and is not itself a searchable symbol.
    let parameter_carrier = selected_symbol_by_name_and_kind(
        &mut session,
        "take",
        DeclarationKind::Variable,
        cadence_package,
    )?;
    let selected_source = session
        .semantic_versions(package.clone())?
        .into_vec()
        .into_iter()
        .find(|record| record.selected && record.complete && record.profile.name() == Some("rust"))
        .ok_or_else(|| io::Error::other("fixture has no selected complete Rust semantic image"))?;
    assert!(
        selected_source.selected_source_frontier.is_some(),
        "selected Rust semantics must expose their exact source frontier"
    );
    assert!(
        matches!(
            &selected_source.freshness,
            backend_library::SemanticVersionFreshness::Current { .. }
        ),
        "the just-published Rust image must be current before public shape reads: {:?}",
        selected_source.freshness
    );
    let shape_budget = SemanticShapeBudget::new(4096, 256 * 1024)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let changed_freshness = match selected_source.freshness {
        backend_library::SemanticVersionFreshness::Current { input_digest } => {
            backend_library::SemanticVersionFreshness::Historical {
                selected_input: input_digest,
                latest_input: if input_digest == [0; 32] {
                    [1; 32]
                } else {
                    [0; 32]
                },
            }
        }
        backend_library::SemanticVersionFreshness::Historical { .. } => {
            backend_library::SemanticVersionFreshness::Unverified
        }
        backend_library::SemanticVersionFreshness::Unverified => {
            backend_library::SemanticVersionFreshness::Current {
                input_digest: [0; 32],
            }
        }
    };
    let mut stale_source = selected_source.clone();
    stale_source.freshness = changed_freshness;
    if session
        .semantic_shapes(stale_source, &[cadence, signal], shape_budget)
        .is_ok()
    {
        return Err(io::Error::other(
            "semantic shapes accepted a source record with stale freshness",
        )
        .into());
    }
    let mut replaced_source = selected_source.clone();
    let mut replaced_generation = replaced_source.generation.to_bytes();
    replaced_generation[0] ^= 0xff;
    replaced_source.generation = backend_library::SemanticGenerationId::new(replaced_generation);
    if session
        .semantic_shapes(replaced_source, &[cadence, signal], shape_budget)
        .is_ok()
    {
        return Err(io::Error::other(
            "semantic shapes accepted a generation no longer selected by the owner",
        )
        .into());
    }
    let carrier_shapes = with_phase_context(
        "query the published parameter carrier shape",
        session.semantic_shapes(selected_source.clone(), &[parameter_carrier], shape_budget),
    )?;
    assert_eq!(carrier_shapes.entries.len(), 1);
    let parameter_identity = carrier_shapes.entries[0]
        .identity
        .ok_or_else(|| io::Error::other("take carrier has no stable image identity"))?;

    let shapes = with_phase_context(
        "query the published callable and aggregate shapes",
        session.semantic_shapes(selected_source.clone(), &[cadence, signal], shape_budget),
    )?;
    assert_eq!(shapes.entries.len(), 2);
    let cadence_shape = &shapes.entries[0];
    let signal_shape = &shapes.entries[1];
    assert!(cadence_shape.identity.is_some());
    assert!(signal_shape.identity.is_some());
    let backend_library::SemanticShapeFact::Available { shape, .. } = &cadence_shape.fact else {
        return Err(io::Error::other("cadence8 did not return an available compiler shape").into());
    };
    let SemanticDeclarationShape::Callable(callable) = shape else {
        return Err(io::Error::other("cadence8 did not return a callable shape").into());
    };
    assert_eq!(callable.parameters.len(), 1);
    assert_eq!(callable.results.len(), 1);
    assert_eq!(
        callable.parameters[0]
            .label
            .as_ref()
            .map(|label| label.as_str()),
        Some("take")
    );
    let SemanticTypeFact::Known(SemanticTypeExpr::Nominal {
        declaration: parameter_type,
        symbol: Some(_),
    }) = &callable.parameters[0].ty
    else {
        return Err(
            io::Error::other("cadence8 parameter lost its nominal compiler identity").into(),
        );
    };
    let SemanticTypeFact::Known(SemanticTypeExpr::Nominal {
        declaration: result_type,
        symbol: Some(_),
    }) = &callable.results[0].ty
    else {
        return Err(io::Error::other("cadence8 result lost its nominal compiler identity").into());
    };
    assert_eq!(parameter_type, result_type);
    let SemanticCallableCarrierBindings::Captured {
        parameters: parameter_bindings,
        results: result_bindings,
    } = &callable.carrier_bindings
    else {
        return Err(io::Error::other("cadence8 carrier bindings are not captured").into());
    };
    assert_eq!(parameter_bindings.as_ref(), &[parameter_identity]);
    assert_eq!(result_bindings.len(), 1);
    assert_ne!(result_bindings[0], parameter_identity);
    let backend_library::SemanticShapeFact::Available { shape, .. } = &signal_shape.fact else {
        return Err(
            io::Error::other("MorningSignal did not return an available compiler shape").into(),
        );
    };
    let SemanticDeclarationShape::Aggregate(members) = shape else {
        return Err(
            io::Error::other("MorningSignal did not return an aggregate member shape").into(),
        );
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].name.as_str(), "pulse");
    assert!(matches!(
        &members[0].ty,
        SemanticTypeFact::Known(SemanticTypeExpr::Builtin(_))
    ));
    assert!(matches!(
        &members[0].language,
        backend_library::SemanticShapeLanguageFacts::Partial {
            profile: backend_semantic::vocabulary::LanguageProfile::Rust(_),
            facts: backend_library::SemanticShapeLanguageFact::RustOwnership(_),
        } | backend_library::SemanticShapeLanguageFacts::Unavailable {
            profile: backend_semantic::vocabulary::LanguageProfile::Rust(_),
        } | backend_library::SemanticShapeLanguageFacts::CommonOnly {
            profile: backend_semantic::vocabulary::LanguageProfile::Rust(_),
        }
    ));
    let published_root = with_phase_context(
        "read initial published workspace revision",
        session.revision(),
    )?
    .root;
    assert_eq!(published_root.as_bytes(), receipt.view_root());

    let first_receipt = receipt.clone();
    let first_observation = IndexOperationObservation::Known(published);
    drop(session);
    let initial_owner_report = with_phase_context("close initial embedded owner", owner.close())?;
    assert!(
        initial_owner_report.connections >= 2,
        "status polling must reconnect after the listener retires its bounded frame stream"
    );
    assert!(
        initial_owner_report.frames > FrameLimits::default().max_frames_per_connection,
        "the public lifecycle must cross the real per-connection frame boundary"
    );
    assert_eq!(
        initial_owner_report.failures, 0,
        "the exact keyed status retry must not create listener or owner failures"
    );
    assert!(
        fs::read(&authority_secret)? == authority_credential,
        "closing the owner must preserve the initialized workspace credential"
    );

    let persisted = fs::read_to_string(&key_file)?;
    let restored_key = IndexOperationKey::parse_hex(persisted.trim()).map_err(|error| {
        io::Error::other(format!(
            "persisted caller operation key is invalid: {error}"
        ))
    })?;
    assert_eq!(restored_key, operation_key);
    let restarted_owner = with_phase_context(
        "restart embedded owner from the same durable workspace paths",
        EmbeddedLocalService::start(config.clone()),
    )?;
    assert!(
        fs::read(&authority_secret)? == authority_credential,
        "restarting the owner must reuse the same durable workspace credential"
    );
    let mut restarted_session = with_phase_context(
        "connect authenticated public Session after restart",
        Session::connect(restarted_owner.endpoint()),
    )?;

    let after_restart = with_phase_context(
        "read caller-keyed operation status after restart",
        restarted_session.index_operation_status(restored_key),
    )?;
    assert_eq!(after_restart, first_observation);
    assert_same_publication_receipt(&after_restart, &first_receipt);
    let cold_source = selected_rust_source(&mut restarted_session, &package)?;
    assert_eq!(cold_source.generation, selected_source.generation);
    assert!(matches!(
        cold_source.freshness,
        backend_library::SemanticVersionFreshness::Current { .. }
    ));
    assert_shape_facts_preserved(
        &mut restarted_session,
        cold_source,
        &[cadence, signal],
        shape_budget,
        &shapes,
    )?;
    let replay = with_phase_context(
        "replay the exact caller-keyed request after restart",
        restarted_session.start_index_operation(
            restored_key,
            package.clone(),
            CompileExecutionIntent::Interactive,
        ),
    )?;
    assert_eq!(replay, first_observation);
    assert_same_publication_receipt(&replay, &first_receipt);
    let status_after_replay = with_phase_context(
        "read operation status after exact replay",
        restarted_session.index_operation_status(restored_key),
    )?;
    assert_eq!(
        status_after_replay, first_observation,
        "exact replay must retain the original publication receipt"
    );
    assert_same_publication_receipt(&status_after_replay, &first_receipt);
    let after_replay_root = with_phase_context(
        "read workspace revision after exact replay",
        restarted_session.revision(),
    )?
    .root;
    assert_eq!(
        after_replay_root.as_bytes(),
        first_receipt.view_root(),
        "exact replay must not publish a different view"
    );

    let conflict = restarted_session.start_index_operation(
        restored_key,
        package.clone(),
        CompileExecutionIntent::Background,
    );
    match conflict {
        Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(detail)))
            if detail == "index-operation key was reused with another request" => {}
        Err(error) => {
            return Err(Box::new(PhaseError::from_source(
                "submit conflicting request under the persisted key",
                error,
            )));
        }
        Ok(observation) => {
            return Err(Box::new(PhaseError::message(
                "submit conflicting request under the persisted key",
                format!("expected the typed key-conflict rejection, got {observation:?}"),
            )));
        }
    }
    let status_after_conflict = with_phase_context(
        "read operation status after key-conflict rejection",
        restarted_session.index_operation_status(restored_key),
    )?;
    assert_eq!(
        status_after_conflict, first_observation,
        "a conflicting replay must leave the published operation untouched"
    );
    assert_same_publication_receipt(&status_after_conflict, &first_receipt);
    let root_after_conflict = with_phase_context(
        "read workspace revision after key-conflict rejection",
        restarted_session.revision(),
    )?
    .root;
    assert_eq!(
        root_after_conflict.as_bytes(),
        first_receipt.view_root(),
        "a conflicting replay must not change the published view root"
    );

    // A new caller key is a new operation even when the package inputs are
    // unchanged. It may legitimately reuse a proved generation or perform a
    // fresh capture; either path must leave terminal receipts and usable shapes.
    let same_add_key = IndexOperationKey::from_bytes([0x6e; 32])
        .map_err(|error| io::Error::other(error.to_string()))?;
    let same_add_started = with_phase_context(
        "submit same package under a distinct caller key",
        restarted_session.start_index_operation(
            same_add_key,
            package.clone(),
            CompileExecutionIntent::Interactive,
        ),
    )?;
    let same_add_status = with_phase_context(
        "publish same package under a distinct caller key",
        wait_for_published(&mut restarted_session, same_add_key, same_add_started),
    )?;
    if let Some(capture) = &same_add_status.source_capture {
        assert!(capture.profiles().iter().all(|profile| matches!(
            profile.state,
            backend_library::IndexOperationSemanticProfileState::Published { .. }
        )));
    }
    let same_add_source = selected_rust_source(&mut restarted_session, &package)?;
    assert!(matches!(
        same_add_source.freshness,
        backend_library::SemanticVersionFreshness::Current { .. }
    ));
    assert_shape_facts_preserved(
        &mut restarted_session,
        same_add_source.clone(),
        &[cadence, signal],
        shape_budget,
        &shapes,
    )?;
    let same_add_observation = IndexOperationObservation::Known(same_add_status);

    // This is a real Cargo project-authority refusal, after a successful
    // publication from the same configured compiler. The Rust source stays
    // valid while Cargo must reject the nonexistent path dependency.
    fs::write(
        package_root.join("Cargo.toml"),
        "[package]\nname = \"public_operation_lifecycle_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nmissing_dependency = { path = \"missing_dependency\" }\n",
    )?;
    let failed_key = IndexOperationKey::from_bytes([0x6f; 32])
        .map_err(|error| io::Error::other(error.to_string()))?;
    let failed_started = with_phase_context(
        "submit actual failed Cargo dependency refresh",
        restarted_session.start_index_operation(
            failed_key,
            package.clone(),
            CompileExecutionIntent::Interactive,
        ),
    )?;
    let failed_status = with_phase_context(
        "await actual failed Cargo dependency refresh",
        wait_for_failed(&mut restarted_session, failed_key, failed_started),
    )?;
    let failed_capture = failed_status
        .source_capture
        .as_ref()
        .ok_or_else(|| io::Error::other("failed compiler refresh omitted its source capture"))?;
    assert!(failed_capture.profiles().iter().all(|profile| matches!(
        profile.state,
        backend_library::IndexOperationSemanticProfileState::Failed { prior, .. }
            if prior.generation == same_add_source.generation.to_bytes()
    )));
    let failed_source = selected_rust_source(&mut restarted_session, &package)?;
    assert_eq!(failed_source.generation, same_add_source.generation);
    assert!(matches!(
        failed_source.freshness,
        backend_library::SemanticVersionFreshness::Historical {
            selected_input,
            latest_input,
        } if selected_input != latest_input
    ));
    assert_shape_facts_preserved(
        &mut restarted_session,
        failed_source.clone(),
        &[cadence, signal],
        shape_budget,
        &shapes,
    )?;
    let failed_observation = IndexOperationObservation::Known(failed_status);
    drop(restarted_session);
    with_phase_context("close restarted embedded owner", restarted_owner.close())?;

    let cold_owner = with_phase_context(
        "cold reopen after same-add and actual compiler refusal",
        EmbeddedLocalService::start(config),
    )?;
    let mut cold_session = with_phase_context(
        "connect after failed compiler cold reopen",
        Session::connect(cold_owner.endpoint()),
    )?;
    assert_eq!(
        cold_session.index_operation_status(same_add_key)?,
        same_add_observation
    );
    assert_eq!(
        cold_session.index_operation_status(failed_key)?,
        failed_observation
    );
    let failed_cold_source = selected_rust_source(&mut cold_session, &package)?;
    assert_eq!(failed_cold_source.generation, failed_source.generation);
    assert_eq!(failed_cold_source.freshness, failed_source.freshness);
    assert_shape_facts_preserved(
        &mut cold_session,
        failed_cold_source,
        &[cadence, signal],
        shape_budget,
        &shapes,
    )?;
    let before_failed_replay = cold_session.revision()?.root;
    assert_eq!(
        cold_session.start_index_operation(
            failed_key,
            package,
            CompileExecutionIntent::Interactive,
        )?,
        failed_observation,
        "exact-key replay must preserve the failed operation and its source receipt"
    );
    assert_eq!(cold_session.revision()?.root, before_failed_replay);
    drop(cold_session);
    with_phase_context("close failed compiler cold owner", cold_owner.close())?;
    Ok(())
}

fn assert_first_refusal_has_no_authority(
    session: &mut Session,
    package: &PackageReference,
) -> Result<(), Box<dyn Error>> {
    assert!(
        session.semantic_versions(package.clone())?.is_empty(),
        "captured structural input cannot manufacture a compiler generation"
    );
    // Deliberately present a caller-invented selected source. This is an
    // adversarial negative query, never evidence of compiler publication.
    let source = backend_library::SemanticVersionRecord {
        package: package.clone(),
        coordinate: backend_library::PackageCoordinate::parse(
            "pkg:cargo/public_operation_lifecycle_fixture@0.1.0".to_owned(),
        )?,
        profile: backend_library::SemanticLanguageProfile::new(
            backend_semantic::vocabulary::LanguageProfile::Rust(
                backend_semantic::vocabulary::RustEdition::Rust2021,
            ),
        ),
        generation: backend_library::SemanticGenerationId::new([0x61; 32]),
        generation_root: [0x62; 32],
        dependency_set: [0x63; 32],
        manifest: [0x64; 32],
        artifacts: 1,
        semantic_bytes: 1,
        complete: true,
        selected: true,
        freshness: backend_library::SemanticVersionFreshness::Unverified,
        history_status: backend_library::SemanticHistoryPublicationStatus::NotSelected,
        selected_source_frontier: None,
    };
    let budget = SemanticShapeBudget::new(4096, 256 * 1024)?;
    let symbol = backend_library::SymbolKey::from_value("unselected-first-refusal-probe");
    assert!(
        matches!(
            session.semantic_shapes(source, &[symbol], budget),
            Err(ClientError::CommandFailed(CommandFailure::NotFound))
        ),
        "an invented compiler source must get the typed absent-selection refusal"
    );
    Ok(())
}
fn selected_rust_source(
    session: &mut Session,
    package: &PackageReference,
) -> Result<backend_library::SemanticVersionRecord, Box<dyn Error>> {
    session
        .semantic_versions(package.clone())?
        .into_vec()
        .into_iter()
        .find(|record| record.selected && record.complete && record.profile.name() == Some("rust"))
        .ok_or_else(|| {
            io::Error::other("fixture lost its selected complete Rust generation").into()
        })
}

fn assert_shape_facts_preserved(
    session: &mut Session,
    source: backend_library::SemanticVersionRecord,
    symbols: &[backend_library::SymbolKey],
    budget: SemanticShapeBudget,
    expected: &backend_library::SemanticShapeBatch,
) -> Result<(), Box<dyn Error>> {
    let actual = session.semantic_shapes(source, symbols, budget)?;
    assert_eq!(actual.entries.len(), expected.entries.len());
    for (actual, expected) in actual.entries.iter().zip(expected.entries.iter()) {
        assert_eq!(actual.identity, expected.identity);
        assert_eq!(actual.fact, expected.fact);
    }
    Ok(())
}

fn selected_symbol_by_name(
    session: &mut Session,
    name: &str,
) -> Result<backend_library::SymbolKey, Box<dyn Error>> {
    let reply = session.names(name, 16)?;
    let CommandReply::Names(snapshot) = reply.reply else {
        return Err(io::Error::other("name lookup returned another reply shape").into());
    };
    let returned_labels = snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.label.as_str())
        .collect::<Vec<_>>();
    let semantic_suffix = format!("::{name}");
    snapshot
        .root
        .rows()
        .iter()
        .find(|row| row.label == name || row.label.ends_with(&semantic_suffix))
        .and_then(|row| match row.id {
            RowId::Symbol(symbol) => Some(symbol),
            RowId::Package(_) | RowId::Object(_) => None,
        })
        .ok_or_else(|| {
            io::Error::other(format!(
                "name lookup did not return {name}; returned labels: {returned_labels:?}"
            ))
            .into()
        })
}

fn selected_package_by_symbol(
    session: &mut Session,
    name: &str,
    symbol: backend_library::SymbolKey,
) -> Result<PackageKey, Box<dyn Error>> {
    let reply = session.names(name, 200)?;
    let CommandReply::Names(snapshot) = reply.reply else {
        return Err(io::Error::other("name lookup returned another reply shape").into());
    };
    snapshot
        .root
        .rows()
        .iter()
        .find(|row| row.id == RowId::Symbol(symbol))
        .and_then(|row| row.package)
        .ok_or_else(|| io::Error::other(format!("symbol {name} has no package row")).into())
}

fn selected_symbol_by_name_and_kind(
    session: &mut Session,
    name: &str,
    kind: DeclarationKind,
    package: PackageKey,
) -> Result<backend_library::SymbolKey, Box<dyn Error>> {
    let reply = session.names(name, 200)?;
    let CommandReply::Names(snapshot) = reply.reply else {
        return Err(io::Error::other("name lookup returned another reply shape").into());
    };
    let semantic_name_suffix = format!("::{name}");
    snapshot
        .root
        .rows()
        .iter()
        .find(|row| {
            (row.label == name || row.label.ends_with(&semantic_name_suffix))
                && row.kind == Some(kind)
                && row.package == Some(package)
                && matches!(row.id, RowId::Symbol(_))
        })
        .and_then(|row| match row.id {
            RowId::Symbol(symbol) => Some(symbol),
            RowId::Package(_) | RowId::Object(_) => None,
        })
        // A function and its synthetic result carrier can share the same
        // spelling; name ranking may return only the function on this query.
        .map_or_else(
            || find_symbol_in_package_semantics(session, name, kind, package),
            Ok,
        )
}

fn find_symbol_in_package_semantics(
    session: &mut Session,
    name: &str,
    kind: DeclarationKind,
    package: PackageKey,
) -> Result<backend_library::SymbolKey, Box<dyn Error>> {
    let semantic_name_suffix = format!("::{name}");
    let mut continuation = None;
    loop {
        let reply = session.names_page("::semantic::", 200, continuation)?;
        let CommandReply::Names(snapshot) = reply.reply else {
            return Err(io::Error::other("all-name page returned another reply shape").into());
        };
        if let Some(symbol) = snapshot
            .root
            .rows()
            .iter()
            .find(|row| {
                (row.label == name || row.label.ends_with(&semantic_name_suffix))
                    && row.kind == Some(kind)
                    && row.package == Some(package)
                    && matches!(row.id, RowId::Symbol(_))
            })
            .and_then(|row| match row.id {
                RowId::Symbol(symbol) => Some(symbol),
                RowId::Package(_) | RowId::Object(_) => None,
            })
        {
            return Ok(symbol);
        }
        continuation = snapshot
            .next
            .map(backend_library::PageContinuation::from_cursor);
        if continuation.is_none() {
            break;
        }
    }
    Err(io::Error::other(format!(
        "name lookup did not return {name} with kind {kind:?} in package {package:?}"
    ))
    .into())
}

#[derive(Debug)]
struct FailureFixture {
    tempdir: tempfile::TempDir,
    retained: bool,
}

impl FailureFixture {
    fn new(tempdir: tempfile::TempDir) -> Self {
        Self {
            tempdir,
            retained: false,
        }
    }

    fn path(&self) -> &Path {
        self.tempdir.path()
    }

    fn preserve_after_failure(&mut self) {
        if !self.retained {
            self.tempdir.disable_cleanup(true);
            self.retained = true;
            let _ = writeln!(
                io::stderr(),
                "public index-operation failure fixture retained at {}",
                self.tempdir.path().display()
            );
        }
    }
}

impl Drop for FailureFixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.preserve_after_failure();
        }
    }
}

#[derive(Debug)]
struct PhaseError {
    phase: &'static str,
    message: String,
    source: Option<Box<dyn Error>>,
}

impl PhaseError {
    fn from_source<E: Error + 'static>(phase: &'static str, source: E) -> Self {
        let message = source.to_string();
        Self {
            phase,
            message,
            source: Some(Box::new(source)),
        }
    }

    fn message(phase: &'static str, message: String) -> Self {
        Self {
            phase,
            message,
            source: None,
        }
    }
}

impl std::fmt::Display for PhaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.phase, self.message)
    }
}

impl Error for PhaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref()
    }
}

#[derive(Debug)]
struct LifecycleFailureDiagnostic {
    phase: PhaseError,
    owner_running_before_cleanup: bool,
    second_session_health: SecondSessionHealth,
    owner_finish: OwnerFinishDiagnostic,
}

impl std::fmt::Display for LifecycleFailureDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} [owner-running-before-cleanup={}, second-session-health={:?}, owner-finish={:?}]",
            self.phase,
            self.owner_running_before_cleanup,
            self.second_session_health,
            self.owner_finish,
        )
    }
}

impl Error for LifecycleFailureDiagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.phase)
    }
}

#[derive(Debug)]
enum SecondSessionHealth {
    Healthy,
    ConnectDisconnected(io::ErrorKind),
    ConnectOther,
    ReadDisconnected(io::ErrorKind),
    ReadOther,
}

fn probe_second_session_health(endpoint: &Path) -> SecondSessionHealth {
    let mut session = match Session::connect_with_timeouts(
        endpoint,
        Duration::from_secs(1),
        Duration::from_secs(2),
    ) {
        Ok(session) => session,
        Err(ClientError::Disconnected(kind)) => {
            return SecondSessionHealth::ConnectDisconnected(kind);
        }
        Err(_) => return SecondSessionHealth::ConnectOther,
    };
    match session.health() {
        Ok(_) => SecondSessionHealth::Healthy,
        Err(ClientError::Disconnected(kind)) => SecondSessionHealth::ReadDisconnected(kind),
        Err(_) => SecondSessionHealth::ReadOther,
    }
}

#[derive(Debug)]
enum OwnerFinishDiagnostic {
    Report {
        connections: usize,
        frames: usize,
        failures: usize,
    },
    Listener(ListenerFinishDiagnostic),
    OwnerThreadFailed,
    OtherProcessError,
}

#[derive(Debug)]
enum ListenerFinishDiagnostic {
    InvalidConfig,
    FrameLimitsMismatch,
    EndpointOccupied,
    AlreadyRunning,
    Io(io::ErrorKind),
    Protocol,
    ServiceStopped,
}

impl OwnerFinishDiagnostic {
    fn from_result(result: Result<RunReport, ProcessError>) -> Self {
        match result {
            Ok(RunReport {
                connections,
                frames,
                failures,
            }) => Self::Report {
                connections,
                frames,
                failures,
            },
            Err(ProcessError::Listener(error)) => Self::Listener(match error {
                ListenerError::InvalidConfig => ListenerFinishDiagnostic::InvalidConfig,
                ListenerError::FrameLimitsMismatch => ListenerFinishDiagnostic::FrameLimitsMismatch,
                ListenerError::EndpointOccupied => ListenerFinishDiagnostic::EndpointOccupied,
                ListenerError::AlreadyRunning => ListenerFinishDiagnostic::AlreadyRunning,
                ListenerError::Io(kind) => ListenerFinishDiagnostic::Io(kind),
                ListenerError::Protocol(_) => ListenerFinishDiagnostic::Protocol,
                ListenerError::ServiceStopped => ListenerFinishDiagnostic::ServiceStopped,
            }),
            Err(ProcessError::Profile(_)) => Self::OwnerThreadFailed,
            Err(_) => Self::OtherProcessError,
        }
    }
}

fn with_phase_context<T, E: Error + 'static>(
    phase: &'static str,
    result: Result<T, E>,
) -> Result<T, PhaseError> {
    result.map_err(|source| PhaseError::from_source(phase, source))
}

fn assert_same_publication_receipt(
    observation: &IndexOperationObservation,
    expected: &IndexOperationPublicationReceipt,
) {
    let IndexOperationObservation::Known(status) = observation else {
        panic!("a persisted operation must remain known: {observation:?}");
    };
    let IndexOperationState::Published(receipt) = &status.state else {
        panic!("the persisted operation must remain published: {status:?}");
    };
    assert_eq!(receipt.request_identity(), expected.request_identity());
    assert_eq!(receipt.commit_identity(), expected.commit_identity());
    assert_eq!(receipt.workspace_root(), expected.workspace_root());
    assert_eq!(receipt.workspace_sequence(), expected.workspace_sequence());
    assert_eq!(receipt.view_root(), expected.view_root());
    assert_eq!(receipt.view_version(), expected.view_version());
    assert_eq!(receipt.view_recipe(), expected.view_recipe());
}

fn wait_for_published(
    session: &mut Session,
    operation_key: IndexOperationKey,
    mut observation: IndexOperationObservation,
) -> Result<backend_library::IndexOperationStatus, PhaseError> {
    let deadline = Instant::now() + Duration::from_secs(660);
    let initial_status = known_status_for_key(observation.clone(), operation_key)?;
    let diagnostic_ticket = match &initial_status.state {
        IndexOperationState::Active { ticket, .. } => Some(ticket.clone()),
        _ => None,
    };
    reject_failed_status(session, &initial_status, diagnostic_ticket.as_ref())?;

    // Exercise the listener's real per-connection frame cap through the public
    // Session. Start consumes one frame, so these exact keyed reads guarantee
    // that the first connection is retired and the next status query must
    // reconnect this same Session. No Start or other mutation is repeated.
    let boundary_reads = FrameLimits::default()
        .max_frames_per_connection
        .saturating_add(4);
    for _ in 0..boundary_reads {
        observation = read_status_with_keyed_reconnect(session, operation_key, deadline)?;
        let status = status_for_exact_start(observation, &initial_status)?;
        reject_failed_status(session, &status, diagnostic_ticket.as_ref())?;
        observation = IndexOperationObservation::Known(status);
    }

    loop {
        let status = status_for_exact_start(observation, &initial_status)?;
        match &status.state {
            IndexOperationState::Published(_) => {
                // Status polling deliberately retires bounded frame streams.
                // Ordinary Session reads are one-shot, so start their phase
                // on a freshly authenticated stream without replaying Start.
                with_phase_context(
                    "reconnect after exact keyed publication polling before ordinary reads",
                    session.reconnect(),
                )?;
                return Ok(status);
            }
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {
                check_poll_deadline(deadline)?;
                thread::sleep(Duration::from_millis(50));
                observation = read_status_with_keyed_reconnect(session, operation_key, deadline)?;
            }
            IndexOperationState::Failed { .. } | IndexOperationState::Unresolved { .. } => {
                return Err(PhaseError::message(
                    "poll durable operation status while awaiting publication",
                    format!(
                        "the real Cargo operation reached a non-published terminal state: {:?}",
                        status.state
                    ),
                ));
            }
        }
    }
}

fn wait_for_failed(
    session: &mut Session,
    operation_key: IndexOperationKey,
    mut observation: IndexOperationObservation,
) -> Result<backend_library::IndexOperationStatus, PhaseError> {
    let deadline = Instant::now() + Duration::from_secs(660);
    let initial_status = known_status_for_key(observation.clone(), operation_key)?;
    let mut diagnostic_ticket = None;
    loop {
        let status = status_for_exact_start(observation, &initial_status)?;
        match &status.state {
            IndexOperationState::Failed { .. } => {
                let terminal = diagnostic_ticket.map(|ticket| session.await_index_job(ticket));
                eprintln!(
                    "intentional Cargo refusal: {:?}; retained job terminal: {terminal:?}",
                    status.state
                );
                return Ok(status);
            }
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {
                if let IndexOperationState::Active { ticket, .. } = &status.state {
                    diagnostic_ticket = Some(ticket.clone());
                }
                check_poll_deadline(deadline)?;
                thread::sleep(Duration::from_millis(50));
                observation = read_status_with_keyed_reconnect(session, operation_key, deadline)?;
            }
            IndexOperationState::Published(_) | IndexOperationState::Unresolved { .. } => {
                return Err(PhaseError::message(
                    "await actual failed Cargo dependency refresh",
                    format!(
                        "expected durable Failed after the invalid dependency, got {:?}",
                        status.state
                    ),
                ));
            }
        }
    }
}

fn read_status_with_keyed_reconnect(
    session: &mut Session,
    operation_key: IndexOperationKey,
    deadline: Instant,
) -> Result<IndexOperationObservation, PhaseError> {
    check_poll_deadline(deadline)?;
    match session.index_operation_status(operation_key) {
        Ok(observation) => Ok(observation),
        Err(ClientError::Disconnected(_)) => {
            check_poll_deadline(deadline)?;
            with_phase_context(
                "reconnect the same public Session for keyed status polling",
                session.reconnect(),
            )?;
            check_poll_deadline(deadline)?;
            with_phase_context(
                "retry the exact keyed status read once after reconnect",
                session.index_operation_status(operation_key),
            )
        }
        Err(error) => Err(PhaseError::from_source(
            "poll durable operation status while awaiting publication",
            error,
        )),
    }
}

fn known_status_for_key(
    observation: IndexOperationObservation,
    operation_key: IndexOperationKey,
) -> Result<backend_library::IndexOperationStatus, PhaseError> {
    match observation {
        IndexOperationObservation::Known(status) if status.operation_key == operation_key => {
            Ok(status)
        }
        _ => Err(PhaseError::message(
            "validate exact durable operation status",
            "status reply was unknown, outside the receipt window, or for another key".to_owned(),
        )),
    }
}

fn status_for_exact_start(
    observation: IndexOperationObservation,
    expected: &backend_library::IndexOperationStatus,
) -> Result<backend_library::IndexOperationStatus, PhaseError> {
    match observation {
        IndexOperationObservation::Known(status)
            if status.operation_key == expected.operation_key
                && status.request_digest == expected.request_digest
                && status.package == expected.package
                && status.execution_intent == expected.execution_intent =>
        {
            Ok(status)
        }
        _ => Err(PhaseError::message(
            "validate exact durable operation status",
            "status reply did not match the original key and request binding".to_owned(),
        )),
    }
}

fn reject_failed_status(
    session: &mut Session,
    status: &backend_library::IndexOperationStatus,
    diagnostic_ticket: Option<&backend_library::IndexJobTicket>,
) -> Result<(), PhaseError> {
    if matches!(
        &status.state,
        IndexOperationState::Failed { .. } | IndexOperationState::Unresolved { .. }
    ) {
        return Err(PhaseError::message(
            "poll durable operation status while awaiting publication",
            format!(
                "the real Cargo operation reached a non-published terminal state: {:?}; retained job terminal: {:?}",
                status.state,
                diagnostic_ticket.and_then(|ticket| session.await_index_job(ticket.clone()).ok())
            ),
        ));
    }
    Ok(())
}

fn check_poll_deadline(deadline: Instant) -> Result<(), PhaseError> {
    if Instant::now() >= deadline {
        return Err(PhaseError::message(
            "poll durable operation status while awaiting publication",
            "the 660-second publication deadline elapsed".to_owned(),
        ));
    }
    Ok(())
}

fn executable_in_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::var_os("PATH").ok_or("the test process has no PATH")?;
    for directory in std::env::split_paths(&path) {
        #[cfg(windows)]
        let candidate = directory.join(format!("{name}.exe"));
        #[cfg(not(windows))]
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("could not find {name} in the test process PATH").into())
}

fn cargo_home() -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("the test process has no Cargo home")?;
    if !path.is_dir() {
        return Err(format!("Cargo home is not a directory: {}", path.display()).into());
    }
    Ok(path)
}

fn lifecycle_tempdir() -> Result<tempfile::TempDir, Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut builder = tempfile::Builder::new();
        builder
            .prefix("b-")
            .permissions(fs::Permissions::from_mode(0o700));
        Ok(builder.tempdir_in("/tmp")?)
    }
    #[cfg(windows)]
    {
        Ok(tempfile::Builder::new().prefix("b-").tempdir()?)
    }
}
