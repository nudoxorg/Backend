//! Actual asynchronous runtime/credit handoff fixtures, not pretrained model proof.
use super::*;
use crate::builtin::commands::embedding_refresh::{Event, Failure, Input, Worker};
use backend_engine::application::{
    EmbeddingRuntimeProvision, LocalCompilerEmbeddingPhase, LocalCompilerRuntimeConfiguration,
    LocalCompilerRuntimePaths, LocalCompilerScratch, LocalCompilerTimeout,
    LocalRuntimePackageAuthority, PreparedEmbeddingRuntime,
};
use std::{num::NonZeroUsize, time::Instant};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn compiler(
    root: &Path,
    runtime: Arc<backend_compile::EmbeddingExecutable>,
) -> Result<LocalCompilerClient, Box<dyn std::error::Error>> {
    let configuration = LocalCompilerRuntimeConfiguration::new(
        LocalCompilerRuntimePaths::new(
            root.join("artifacts"),
            root.join("journal"),
            root.join("native"),
        )?,
        Box::new([]),
        Box::new([]),
        LocalRuntimePackageAuthority::default(),
        LocalCompilerTimeout::new(Duration::from_secs(10))?,
        backend_store::journal::PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        LocalCompilerScratch::with_fragment_capacity(NonZeroUsize::new(1024).ok_or("scratch")?)?,
    )?
    .with_embedding_runtime(
        runtime,
        backend_engine::application::EmbeddingRequirement::Required,
    );
    Ok(LocalCompilerClient::start(configuration)?)
}
fn next(worker: &Worker, deadline: Instant) -> Result<Event, Box<dyn std::error::Error>> {
    loop {
        match worker.events.try_recv() {
            Ok(event) => return Ok(event),
            Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(format!("refresh fixture event refused: {error}").into()),
        }
    }
}
fn join(worker: &mut Worker, deadline: Instant) -> TestResult {
    while !worker.finished() {
        if Instant::now() >= deadline {
            return Err("refresh fixture worker did not finish".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.join_finished().ok_or("worker is still retiring")??;
    Ok(())
}

#[test]
fn owner_refresh_worker_keeps_original_control_before_handoff_and_preserves_old_provider()
-> TestResult {
    let producer = crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let old = producer.provision.runtime().ok_or("old real runtime")?;
    let actor_root = crate::test_support::private_temp_dir("embedding-refresh-actor")?;
    let compiler = compiler(actor_root.path(), Arc::clone(&old))?;
    let account = old.memory_budget().clone();
    let credits = account.used();
    let provider = crate::builtin::query::RemoteSemantic::select_owner(
        &producer.provision,
        &producer.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let mut owner = AdapterFixture::new();
    let (_, daemon) = owner.parts();
    let lease = daemon.engine().daemon().owner().lease().clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let path = producer.descriptor.with_file_name("embedding.config");
    let desired = PreparedEmbeddingRuntime::capture_until(&path, &control)?
        .ok_or("config")?
        .identity();
    let log = fs::read_to_string(&producer.log)?;
    let mut worker = Worker::start(Input {
        compiler: compiler.clone(),
        admission: compiler.admit_embedding_refresh(lease.clone())?,
        owner: lease,
        configuration: path,
        descriptor: producer.descriptor.clone(),
        expected: desired.as_bytes(),
        cache_directory: None,
        cancelled,
        deadline,
    })?;
    assert!(matches!(next(&worker, deadline)?, Event::Prepared(identity) if identity == desired));
    worker.cancel();
    assert!(
        matches!(
            worker.check_handoff(),
            Err(backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Cancelled
            ))
        ),
        "an actual queued Prepared event cannot authorize handoff after cancellation"
    );
    let result = match next(&worker, deadline)? {
        Event::Terminal(result) => result,
        _ => return Err("unhanded work mutated actor".into()),
    };
    assert!(matches!(
        result,
        Err(Failure::Control(
            backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Cancelled
            )
        ))
    ));
    join(&mut worker, deadline)?;
    assert!(matches!(
        provider,
        crate::builtin::query::RemoteSemantic::Local(_)
    ));
    assert!(compiler.retired_embedding_configuration().is_none());
    assert!(compiler.embedding_generation_receipt().is_none());
    assert_eq!(account.used(), credits);
    assert_eq!(
        fs::read_to_string(&producer.log)?,
        log,
        "capture never infer/activate"
    );
    let (adapter, daemon) = owner.parts();
    let health = serde_json::to_vec(&backend_engine::CommandDto::new(11, Command::Health))?;
    assert!(matches!(
        adapter.execute_or_defer(daemon, &health, 12)?,
        Executed::Reply(_)
    ));
    Ok(())
}

#[test]
fn owner_refresh_worker_joins_old_account_and_commits_one_shared_actual_runtime() -> TestResult {
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let old = producer.provision.runtime().ok_or("old real runtime")?;
    let old_account = old.memory_budget().clone();
    let actor_root = crate::test_support::private_temp_dir("embedding-refresh-actor")?;
    let compiler = compiler(actor_root.path(), Arc::clone(&old))?;
    let provider = crate::builtin::query::RemoteSemantic::select_owner(
        &producer.provision,
        &producer.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    // The production startup provision is consumed after its exact Arc reaches both owners.
    let absent =
        EmbeddingRuntimeProvision::open(&producer.descriptor.with_file_name("absent.config"))?;
    drop(std::mem::replace(&mut producer.provision, absent));
    drop(old);
    let mut owner = AdapterFixture::new();
    let (_, daemon) = owner.parts();
    let lease = daemon.engine().daemon().owner().lease().clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let path = producer.descriptor.with_file_name("embedding.config");
    let desired = PreparedEmbeddingRuntime::capture_until(&path, &control)?
        .ok_or("config")?
        .identity();
    let mut worker = Worker::start(Input {
        compiler: compiler.clone(),
        admission: compiler.admit_embedding_refresh(lease.clone())?,
        owner: lease,
        configuration: path,
        descriptor: producer.descriptor.clone(),
        expected: desired.as_bytes(),
        cache_directory: None,
        cancelled: Arc::clone(&cancelled),
        deadline,
    })?;
    assert!(matches!(next(&worker, deadline)?, Event::Prepared(identity) if identity == desired));
    worker.handoff(provider).map_err(|_| "handoff refused")?;
    let mut retired = false;
    let mut committed = None;
    let terminal = loop {
        match next(&worker, deadline)? {
            Event::Retired(receipt) => {
                assert_eq!(receipt.configuration, desired);
                let LocalCompilerEmbeddingPhase::Retired {
                    account: Some(account),
                } = receipt.phase
                else {
                    return Err("old actual account not retained".into());
                };
                assert_eq!(account.used(), 0);
                assert_eq!(old_account.used(), 0);
                retired = true;
            }
            Event::CompilerCommitted(receipt) => {
                assert!(retired);
                committed = Some(receipt);
            }
            Event::Terminal(result) => break result?,
            Event::Phase(_) => {}
            Event::Prepared(_) => return Err("duplicate capture".into()),
        }
        let (adapter, daemon) = owner.parts();
        let health = serde_json::to_vec(&backend_engine::CommandDto::new(21, Command::Health))?;
        assert!(
            matches!(
                adapter.execute_or_defer(daemon, &health, 22)?,
                Executed::Reply(_)
            ),
            "owner control stays responsive during actual refresh"
        );
    };
    join(&mut worker, deadline)?;
    let receipt = committed.ok_or("lost actual committed receipt")?;
    let admitted = terminal.query?;
    let LocalCompilerEmbeddingPhase::Activated { execution } = receipt.phase else {
        return Err("not activated".into());
    };
    assert_eq!(receipt.configuration, desired);
    assert_eq!(terminal.compiler.configuration, desired);
    assert_eq!(admitted.runtime().execution_identity(), execution);
    assert_eq!(admitted.execution_identity(), execution);
    let account = admitted.runtime().memory_budget().clone();
    assert!(account.used() > 0);
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        compiler
            .embedding_generation_receipt()
            .ok_or("receipt after observer stop")?
            .configuration,
        desired
    );
    drop(admitted);
    drop(compiler);
    drop(worker);
    assert_eq!(
        account.used(),
        0,
        "new account released after both actual owners drop"
    );
    Ok(())
}

fn configure_serving_owner(
    owner: &mut AdapterFixture,
    producer: &mut crate::builtin::query::local_semantic::tests::ProtocolFixture,
) -> Result<(backend_compile::EmbeddingMemoryBudget, [u8; 32]), Box<dyn std::error::Error>> {
    let old = producer.provision.runtime().ok_or("old runtime")?;
    let account = old.memory_budget().clone();
    let (adapter, _) = owner.parts();
    let root = adapter.product_state.workspace_path()?.join("compiler");
    fs::create_dir_all(&root)?;
    crate::test_support::make_private(&root)?;
    backend_platform::durable::write_private_atomic(
        &root.join("embedding.config"),
        &fs::read(producer.descriptor.with_file_name("embedding.config"))?,
    )?;
    backend_platform::durable::write_private_atomic(
        &root.join("embedding-query.config"),
        &fs::read(&producer.descriptor)?,
    )?;
    let flag = AtomicBool::new(false);
    let control = backend_compile::EmbeddingActivationControl::new(
        &flag,
        Instant::now() + Duration::from_secs(10),
    );
    let expected =
        PreparedEmbeddingRuntime::capture_until(&root.join("embedding.config"), &control)?
            .ok_or("copied config")?
            .identity()
            .as_bytes();
    adapter.compiler = compiler(&root.join("actor"), Arc::clone(&old))?;
    adapter.remote_semantic = crate::builtin::query::RemoteSemantic::select_owner(
        &producer.provision,
        &producer.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    drop(std::mem::replace(
        &mut producer.provision,
        EmbeddingRuntimeProvision::open(&producer.descriptor.with_file_name("absent.config"))?,
    ));
    drop(old);
    Ok((account, expected))
}

fn refresh_call(
    owner: &mut AdapterFixture,
    id: u64,
    request: backend_library::EmbeddingRefreshAction,
) -> Result<backend_library::EmbeddingRefreshObservation, Box<dyn std::error::Error>> {
    let body = serde_json::to_vec(&backend_engine::CommandDto::new(
        id,
        Command::Surface(backend_library::SurfaceCommand::EmbeddingRefresh { request }),
    ))?;
    let (adapter, daemon) = owner.parts();
    let Executed::Reply(bytes) = adapter.execute_or_defer(daemon, &body, id + 1000)? else {
        return Err("refresh controls must answer immediately".into());
    };
    let reply = backend_engine::decode_reply_dto(&bytes)?;
    assert_eq!(reply.request_id, id);
    let CommandReply::Surface(backend_library::SurfaceReply::EmbeddingRefresh(observation)) =
        reply.reply
    else {
        return Err("typed refresh observation missing".into());
    };
    Ok(observation)
}

fn wait_refresh(
    owner: &mut AdapterFixture,
    ticket: &backend_library::EmbeddingRefreshTicket,
) -> Result<backend_library::EmbeddingRefreshObservation, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (adapter, daemon) = owner.parts();
        let ready = adapter.poll_deferred(daemon);
        assert!(
            ready.is_empty(),
            "refresh never registers a lost transport waiter"
        );
        let health = serde_json::to_vec(&backend_engine::CommandDto::new(700, Command::Health))?;
        assert!(
            matches!(
                adapter.execute_or_defer(daemon, &health, 701)?,
                Executed::Reply(_)
            ),
            "normal reads stay responsive while the actual refresh worker runs"
        );
        let observation = refresh_call(
            owner,
            702,
            backend_library::EmbeddingRefreshAction::Observe {
                ticket: ticket.clone(),
            },
        )?;
        if !matches!(
            observation,
            backend_library::EmbeddingRefreshObservation::Pending { .. }
        ) {
            return Ok(observation);
        }
        if Instant::now() >= deadline {
            return Err("actual refresh did not reach an observed owner outcome".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn owner_refresh_public_route_keeps_copied_ticket_after_caller_loss_and_replays_real_commit()
-> TestResult {
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshObservation as O,
        EmbeddingRefreshQueryStatus as Q,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let (old_account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    let (adapter, daemon) = owner.parts();
    adapter.warm_queries(daemon);
    finish_search_lane(adapter, daemon);
    let workspace = daemon.engine().daemon().owner().snapshot().root();
    let basis = daemon.engine().daemon().library().view().root();
    let old_lifetime = adapter
        .search_lane
        .selected_cancellation(workspace, basis)
        .ok_or("actual selected lifetime")?;
    let counts = (
        adapter.search_snapshots.projection_builds(),
        adapter.search_snapshots.projection_opens(),
        adapter.search_snapshots.corpus_builds(),
    );
    let lexical = adapter
        .search_snapshots
        .selected_for(workspace, daemon.engine().daemon().library().view())
        .ok_or("actual selected projection")?
        .search_local(crate::builtin::query::LocalQuery::prefix("project", 32)?)?
        .finish();
    assert!(
        lexical.total_matches > 0,
        "fixture must exercise retained lexical rows"
    );
    let accepted = refresh_call(
        &mut owner,
        100,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?;
    let O::Pending { ticket, .. } = accepted else {
        return Err("refresh was not accepted".into());
    };
    let (adapter, daemon) = owner.parts();
    assert_eq!(
        ticket.owner_epoch,
        daemon.engine().daemon().owner().lease().epoch()
    );
    assert_eq!(
        ticket.owner_fence,
        daemon.engine().daemon().owner().lease().fence()
    );
    adapter.abandon_reply(1100);
    assert!(
        matches!(refresh_call(&mut owner, 101, A::Start { configuration: expected, timeout_seconds: 1 })?, O::Pending { ticket: same, .. } if same == ticket),
        "repeat observation does not renew the original deadline or issue another job"
    );
    let terminal = wait_refresh(&mut owner, &ticket)?;
    let O::Committed {
        execution,
        query: Q::Admitted,
        ..
    } = &terminal
    else {
        return Err(format!("actual refresh refused: {terminal:?}").into());
    };
    assert_eq!(
        old_account.used(),
        0,
        "old query/compiler/cache account was released before replacement"
    );
    let (adapter, daemon) = owner.parts();
    assert!(old_lifetime.load(Ordering::Acquire));
    let new_lifetime = adapter
        .search_lane
        .selected_cancellation(workspace, basis)
        .ok_or("renewed exact lexical lifetime")?;
    assert!(!Arc::ptr_eq(&old_lifetime, &new_lifetime));
    assert!(!new_lifetime.load(Ordering::Acquire));
    assert_eq!(
        (
            adapter.search_snapshots.projection_builds(),
            adapter.search_snapshots.projection_opens(),
            adapter.search_snapshots.corpus_builds()
        ),
        counts,
        "refresh cannot rebuild or rehydrate the lexical selection"
    );
    let after = adapter
        .search_snapshots
        .selected_for(workspace, daemon.engine().daemon().library().view())
        .ok_or("same selected projection")?
        .search_local(crate::builtin::query::LocalQuery::prefix("project", 32)?)?
        .finish();
    assert_eq!(after.total_matches, lexical.total_matches);
    assert_eq!(
        after.rows, lexical.rows,
        "complete lexical scores and order survive the semantic lifetime change"
    );
    let current = adapter
        .remote_semantic
        .local_refresh_runtime()
        .ok_or("installed actual query owner")?;
    assert_eq!(
        backend_library::EmbeddingRefreshExecution::from(current.execution_identity()),
        *execution
    );
    let new_account = current.memory_budget().clone();
    let compiler = adapter
        .compiler
        .embedding_generation_receipt()
        .ok_or("actual compiler commit")?;
    assert_eq!(compiler.configuration.as_bytes(), expected);
    assert!(
        matches!(compiler.phase, LocalCompilerEmbeddingPhase::Activated { execution: identity } if backend_library::EmbeddingRefreshExecution::from(identity) == *execution)
    );
    assert!(new_account.used() > 0);
    let log = fs::read_to_string(&producer.log)?;
    assert_eq!(
        refresh_call(
            &mut owner,
            102,
            A::Start {
                configuration: expected,
                timeout_seconds: 10
            }
        )?,
        terminal,
        "successful unchanged configuration replays the exact committed receipt"
    );
    assert_eq!(
        refresh_call(
            &mut owner,
            103,
            A::Cancel {
                ticket: ticket.clone()
            }
        )?,
        terminal,
        "late cancellation cannot rewrite a committed outcome"
    );
    assert_eq!(
        fs::read_to_string(&producer.log)?,
        log,
        "receipt replay never activates a second helper"
    );
    let mut foreign = ticket.clone();
    foreign.owner_fence[31] ^= 1;
    assert!(
        matches!(refresh_call(&mut owner, 104, A::Cancel { ticket: foreign.clone() })?, O::Unknown { ticket: same } if same == foreign)
    );
    drop(owner);
    assert_eq!(
        new_account.used(),
        0,
        "actual service and compiler owners release the charged replacement"
    );
    Ok(())
}

#[test]
fn owner_refresh_public_cancel_before_handoff_preserves_actual_old_owner_and_credits() -> TestResult
{
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshFailureKind as K,
        EmbeddingRefreshObservation as O,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let (account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    let credits = account.used();
    let log = fs::read_to_string(&producer.log)?;
    let O::Pending { ticket, .. } = refresh_call(
        &mut owner,
        200,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("not accepted".into());
    };
    let (adapter, daemon) = owner.parts();
    let view = daemon.engine().daemon().library().view().root();
    assert!(
        matches!(
            adapter.prepare_query(daemon, view.into())?,
            Some(backend_library::CommandFailure::QueryPreparation {
                state: backend_library::QueryPreparationState::Retiring,
                ..
            })
        ),
        "a missing lexical corpus cannot start work against the retiring compiler"
    );
    assert!(!adapter.search_lane.active());
    let mut foreign = ticket.clone();
    foreign.configuration[31] ^= 1;
    assert!(matches!(
        refresh_call(&mut owner, 201, A::Cancel { ticket: foreign })?,
        O::Unknown { .. }
    ));
    assert!(matches!(
        refresh_call(
            &mut owner,
            202,
            A::Cancel {
                ticket: ticket.clone()
            }
        )?,
        O::Pending { .. }
    ));
    assert!(
        matches!(wait_refresh(&mut owner, &ticket)?, O::Failed { cause, compiler: None, .. } if cause.kind == K::Cancelled)
    );
    let (adapter, _) = owner.parts();
    assert!(adapter.remote_semantic.local_refresh_runtime().is_some());
    assert!(adapter.compiler.embedding_generation_receipt().is_none());
    assert!(adapter.compiler.retired_embedding_configuration().is_none());
    assert_eq!(account.used(), credits);
    assert_eq!(fs::read_to_string(&producer.log)?, log);
    Ok(())
}

#[test]
fn owner_refresh_public_busy_and_changed_configuration_refuse_before_retirement() -> TestResult {
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshFailureKind as K,
        EmbeddingRefreshObservation as O,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let (account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    let credits = account.used();
    let log = fs::read_to_string(&producer.log)?;
    owner.parts().0.waiting.push_back((301, Vec::new()));
    assert!(
        matches!(refresh_call(&mut owner, 302, A::Start { configuration: expected, timeout_seconds: 10 })?, O::Refused { cause } if cause.kind == K::Busy)
    );
    owner.parts().0.waiting.clear();
    let mut foreign = expected;
    foreign[31] ^= 1;
    let O::Pending { ticket, .. } = refresh_call(
        &mut owner,
        303,
        A::Start {
            configuration: foreign,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("not accepted for captured comparison".into());
    };
    let O::Failed {
        cause,
        compiler: None,
        ..
    } = wait_refresh(&mut owner, &ticket)?
    else {
        return Err("foreign exact configuration not refused".into());
    };
    assert_eq!(cause.kind, K::Configuration);
    assert!(
        cause
            .detail
            .as_str()
            .contains(&backend_engine::encode_id(&expected))
    );
    assert!(
        cause
            .detail
            .as_str()
            .contains(&backend_engine::encode_id(&foreign))
    );
    assert!(
        owner
            .parts()
            .0
            .remote_semantic
            .local_refresh_runtime()
            .is_some()
    );
    assert!(
        owner
            .parts()
            .0
            .compiler
            .retired_embedding_configuration()
            .is_none()
    );
    assert_eq!(account.used(), credits);
    assert_eq!(fs::read_to_string(&producer.log)?, log);
    Ok(())
}

#[test]
fn owner_refresh_actual_spawn_refusal_boundary_preserves_provider_and_acceptance_sequence()
-> TestResult {
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshFailureKind as K,
        EmbeddingRefreshObservation as O,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let (account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    let credits = account.used();
    let log = fs::read_to_string(&producer.log)?;
    let (adapter, daemon) = owner.parts();
    let refusal = adapter.start_embedding_refresh(daemon, expected, 10, |_input| {
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "fixture thread creation refusal",
        ))
    });
    assert!(
        matches!(refusal, O::Refused { cause } if cause.kind == K::Worker && cause.detail.as_str().contains("fixture thread creation refusal"))
    );
    assert!(adapter.remote_semantic.local_refresh_runtime().is_some());
    assert!(adapter.compiler.retired_embedding_configuration().is_none());
    assert_eq!(account.used(), credits);
    assert_eq!(fs::read_to_string(&producer.log)?, log);
    let O::Pending { ticket, .. } = refresh_call(
        &mut owner,
        400,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("actual factory retry not accepted".into());
    };
    assert_eq!(
        ticket.sequence.get(),
        1,
        "failed spawn is not an accepted operation"
    );
    refresh_call(
        &mut owner,
        401,
        A::Cancel {
            ticket: ticket.clone(),
        },
    )?;
    assert!(
        matches!(wait_refresh(&mut owner, &ticket)?, O::Failed { cause, .. } if cause.kind == K::Cancelled)
    );
    Ok(())
}

#[test]
fn owner_refresh_real_dependency_drift_retains_retired_receipt_and_deliberate_retry_commits()
-> TestResult {
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshCompilerReceipt as C,
        EmbeddingRefreshFailureKind as K, EmbeddingRefreshObservation as O,
        EmbeddingRefreshQueryStatus as Q,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let (account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    // This is the actual declared dependency written by fixture_with_mode, not a model claim.
    let dependency = producer.log.with_file_name("protocol-fixture.py");
    let original = fs::read(&dependency)?;
    fs::write(&dependency, b"changed actual declared dependency")?;
    let O::Pending { ticket, .. } = refresh_call(
        &mut owner,
        500,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("first accepted refresh missing".into());
    };
    let terminal = wait_refresh(&mut owner, &ticket)?;
    assert!(
        matches!(terminal, O::Failed { ref cause, compiler: Some(C::Retired { configuration }), .. } if cause.kind == K::Activation && configuration == expected),
        "{terminal:?}"
    );
    assert_eq!(account.used(), 0);
    assert_eq!(
        owner
            .parts()
            .0
            .compiler
            .retired_embedding_configuration()
            .map(|identity| identity.as_bytes()),
        Some(expected)
    );
    assert!(
        owner
            .parts()
            .0
            .remote_semantic
            .local_refresh_runtime()
            .is_none()
    );
    fs::write(&dependency, original)?;
    let O::Pending { ticket: retry, .. } = refresh_call(
        &mut owner,
        501,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("deliberate failed-configuration retry missing".into());
    };
    assert_ne!(retry, ticket);
    assert_eq!(retry.sequence.get(), ticket.sequence.get() + 1);
    assert!(matches!(
        wait_refresh(&mut owner, &retry)?,
        O::Committed {
            query: Q::Admitted,
            ..
        }
    ));
    assert_eq!(
        refresh_call(&mut owner, 502, A::Observe { ticket })?,
        terminal,
        "old failed receipt remains observable beside the replacement"
    );
    Ok(())
}

#[test]
fn owner_refresh_refuses_real_prepared_publication_without_handoff_or_hidden_cancellation()
-> TestResult {
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshFailureKind as K,
        EmbeddingRefreshObservation as O,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let label = owner.label.clone();
    let (account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    let before = account.used();
    let (adapter, _) = owner.parts();
    let operation =
        backend_library::IndexOperationKey::from_bytes([0x19; 32]).ok_or("operation")?;
    adapter.index_operations.accept(
        operation,
        backend_library::PackageReference::parse(label)?,
        CompileExecutionIntent::Interactive,
    )?;
    adapter
        .index_operations
        .prepare(operation, Some([0x21; 32]), [0x39; 32], 1)?;
    assert!(adapter.index_operations.has_prepared()?);
    let log = fs::read_to_string(&producer.log)?;
    let O::Refused { cause } = refresh_call(
        &mut owner,
        610,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("prepared publication was not refused".into());
    };
    assert_eq!(cause.kind, K::Busy);
    assert!(cause.detail.as_str().contains("prepared publication"));
    let (adapter, daemon) = owner.parts();
    assert!(
        adapter.index_operations.has_prepared()?,
        "refresh must not erase the prepared operation"
    );
    assert!(adapter.remote_semantic.local_refresh_runtime().is_some());
    assert!(adapter.compiler.retired_embedding_configuration().is_none());
    assert!(
        adapter
            .compiler
            .admit_embedding_refresh(daemon.engine().daemon().owner().lease().clone())
            .is_ok(),
        "no refresh reservation survives pre-admission refusal"
    );
    assert_eq!(account.used(), before);
    assert_eq!(fs::read_to_string(&producer.log)?, log);
    Ok(())
}

#[test]
fn owner_refresh_admits_first_query_owner_from_actual_unconfigured_provider() -> TestResult {
    use backend_library::{
        EmbeddingRefreshAction as A, EmbeddingRefreshObservation as O,
        EmbeddingRefreshQueryStatus as Q,
    };
    let mut producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let mut owner = AdapterFixture::new();
    let (account, expected) = configure_serving_owner(&mut owner, &mut producer)?;
    let (adapter, _) = owner.parts();
    adapter.remote_semantic = crate::builtin::query::RemoteSemantic::Unconfigured;
    assert!(
        account.used() > 0,
        "actual old compiler still holds model credits"
    );
    let O::Pending { ticket, .. } = refresh_call(
        &mut owner,
        620,
        A::Start {
            configuration: expected,
            timeout_seconds: 10,
        },
    )?
    else {
        return Err("first query owner refresh was not accepted".into());
    };
    assert!(matches!(
        wait_refresh(&mut owner, &ticket)?,
        O::Committed {
            query: Q::Admitted,
            ..
        }
    ));
    assert_eq!(account.used(), 0);
    let (adapter, _) = owner.parts();
    let current = adapter
        .remote_semantic
        .local_refresh_runtime()
        .ok_or("actual first admitted query owner")?;
    let receipt = adapter
        .compiler
        .embedding_generation_receipt()
        .ok_or("real compiler commit")?;
    assert!(
        matches!(receipt.phase, LocalCompilerEmbeddingPhase::Activated { execution } if execution == current.execution_identity())
    );
    Ok(())
}

#[test]
fn owner_refresh_receiver_stop_after_check_retains_actual_semantic_projection_and_lifetime()
-> TestResult {
    use crate::builtin::query::{RemoteSemantic, search_page_with_cancellation};
    use backend_library::SemanticSearchStatus;
    // This is a real protocol/runtime + full kernel projection fixture, not model or
    // installed preparation proof. Both production handoff and renewal implementations run.
    let producer = crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")?;
    let runtime = producer.provision.runtime().ok_or("actual old runtime")?;
    let account = runtime.memory_budget().clone();
    let actor_root = crate::test_support::private_temp_dir("refresh-late-handoff")?;
    let compiler = compiler(actor_root.path(), Arc::clone(&runtime))?;
    let mut provider = RemoteSemantic::select_owner(
        &producer.provision,
        &producer.descriptor,
        RemoteSemantic::Unconfigured,
    );
    let (projection, library, view) =
        crate::builtin::query::local_semantic::tests::selected_owner_many(30)?;
    let workspace = crate::builtin::genesis()?.root();
    let coordinator = projection
        .selected_for(workspace, &view)
        .ok_or("full selected corpus")?;
    let original = Arc::new(AtomicBool::new(false));
    let mut lane = super::super::super::search_lane::SearchLane::fixture_for_admitted_selection(
        &projection,
        workspace,
        &view,
        Arc::clone(&original),
    )?;
    let query = backend_engine::Query::new(
        "first intent",
        view.root(),
        backend_engine::QueryLimit::new(3)?,
    );
    let warm_deadline = Instant::now() + Duration::from_secs(8);
    let expected = loop {
        let (page, status) = search_page_with_cancellation(
            coordinator,
            &library,
            &mut provider,
            crate::builtin::admitted_coverage()?,
            &query,
            Arc::clone(&original),
        )
        .map_err(|error| error.to_string())?;
        if status == SemanticSearchStatus::Available {
            break page;
        }
        if Instant::now() >= warm_deadline {
            return Err(format!("original semantic fixture did not contribute: {status:?}").into());
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(!expected.root.rows().is_empty());
    let credits = account.used();
    let counts = (
        projection.projection_builds(),
        projection.projection_opens(),
        projection.corpus_builds(),
    );
    let log = fs::read_to_string(&producer.log)?;
    let mut owner = AdapterFixture::new();
    let (_, daemon) = owner.parts();
    let lease = daemon.engine().daemon().owner().lease().clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let path = producer.descriptor.with_file_name("embedding.config");
    let expected_config = PreparedEmbeddingRuntime::capture_until(&path, &control)?
        .ok_or("actual config")?
        .identity();
    fn stop_real_receiver_after_check(worker: &Worker) {
        worker.cancel();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !worker.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            worker.finished(),
            "actual receiver must exit before the real try_send"
        );
    }
    let mut worker = Worker::start(Input {
        compiler: compiler.clone(),
        admission: compiler.admit_embedding_refresh(lease.clone())?,
        owner: lease,
        configuration: path,
        descriptor: producer.descriptor.clone(),
        expected: expected_config.as_bytes(),
        cache_directory: None,
        cancelled,
        deadline,
    })?
    .with_handoff_boundary(stop_real_receiver_after_check);
    assert!(
        matches!(next(&worker, deadline)?, Event::Prepared(identity) if identity == expected_config)
    );
    worker.check_handoff()?;
    let renewal = lane.prepare_selected_lifetime_renewal(&projection, workspace, &view)?;
    assert!(
        !original.load(Ordering::Acquire),
        "preparation has no irreversible effect"
    );
    let refusal = super::super::embedding_refresh_owner::handoff_original_owner(
        &mut worker,
        &mut provider,
        Some(renewal),
    )
    .err()
    .ok_or("stopped receiver accepted original owner")?;
    assert_eq!(
        refusal.kind,
        backend_library::EmbeddingRefreshFailureKind::Worker
    );
    assert!(matches!(
        next(&worker, deadline)?,
        Event::Terminal(Err(Failure::Control(
            backend_compile::EmbeddingExecutableError::Process(
                backend_compile::ProcessError::Cancelled
            )
        )))
    ));
    join(&mut worker, deadline)?;
    assert!(!original.load(Ordering::Acquire));
    assert!(Arc::ptr_eq(
        &original,
        &lane
            .selected_cancellation(workspace, view.root())
            .ok_or("original selection was lost")?
    ));
    assert!(Arc::ptr_eq(
        &runtime,
        provider
            .local_refresh_runtime()
            .ok_or("original provider lost")?
    ));
    // Poll is the exact boundary that retired the old projection in the former race.
    provider.poll_local(workspace, view.root());
    let (after, status) = search_page_with_cancellation(
        coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &query,
        Arc::clone(&original),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(status, SemanticSearchStatus::Available);
    assert_eq!(
        after, expected,
        "same real retained semantic answer, scores and order"
    );
    assert_eq!(
        account.used(),
        credits,
        "refusal preserves retained projection/query credits"
    );
    assert_eq!(
        (
            projection.projection_builds(),
            projection.projection_opens(),
            projection.corpus_builds()
        ),
        counts
    );
    assert_eq!(
        fs::read_to_string(&producer.log)?,
        log,
        "retained answer needs no reinference"
    );
    assert!(compiler.retired_embedding_configuration().is_none());
    assert!(compiler.embedding_generation_receipt().is_none());
    Ok(())
}
