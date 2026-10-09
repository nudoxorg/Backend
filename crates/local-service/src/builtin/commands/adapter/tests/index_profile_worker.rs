//! Actual product/Turso/compiler-owner controls for the existing worker route.
use super::*;
use crate::builtin::BuiltinModelError;
use crate::builtin::commands::adapter::index_profile_worker::{
    ProfileCompletion, ProfileOutcome, ProfileWork, ProfileWorker,
};
use crate::builtin::commands::index;

fn actual_work(fixture: &mut AdapterFixture) -> ProfileWork {
    actual_work_with_queue(fixture, false)
}

fn actual_work_with_queue(fixture: &mut AdapterFixture, queued: bool) -> ProfileWork {
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("pyproject.toml"),
        "[project]\nname=\"worker_handoff\"\nversion=\"1.0.0\"\n",
    )
    .expect("real manifest");
    fs::write(
        Path::new(&label).join("source.py"),
        "def worker_name(value: int) -> int:\n    return value + 1\n",
    )
    .expect("real source");
    if queued {
        fs::write(
            Path::new(&label).join("queued.go"),
            "package worker\nfunc queuedWorker(value int) int { return value + 1 }\n",
        )
        .expect("real queued Go source");
    }
    let (adapter, daemon) = fixture.parts();
    let cancel = Arc::new(AtomicBool::new(false));
    let scan = index::capture_index_scan(
        daemon,
        package,
        &label,
        Path::new(&label),
        None,
        0x991,
        CompileExecutionIntent::Interactive,
        false,
        Arc::clone(&cancel),
    )
    .expect("actual scan capture");
    let scan = index::run_index_scan(scan).unwrap_or_else(|_| panic!("actual source scan"));
    let mut captures = BTreeMap::new();
    let prepared = index::finish_index_scan(
        daemon,
        scan,
        &adapter.compiler,
        &mut adapter.semantic_authority,
        None,
        None,
        true,
        None,
        &mut captures,
        None,
    )
    .expect("actual source-capture publication");
    let index::PreparedIndex::Compile(mut job) = prepared else {
        panic!("real Python source must reach deferred compile")
    };
    adapter
        .publish_view(daemon, None)
        .expect("coherent admitted prior view");
    let (profile, sources) = job.take_next_work().expect("actual compiler work");
    if queued {
        assert!(matches!(
            profile.profile(),
            backend_semantic::vocabulary::LanguageProfile::Python(_)
        ));
        assert_eq!(
            job.pending_attempts().len(),
            1,
            "actual distinct queued profile"
        );
    }
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let read_head = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("unique workspace writer and durable sink");
    let semantic = adapter
        .semantic_authority
        .detach_index_writer(snapshot.root())
        .expect("unique actual Turso writer");
    adapter
        .semantic_authority
        .install_image_loader(&mut adapter.generations);
    ProfileWork::new(
        Some(semantic),
        read_head,
        job,
        snapshot,
        adapter.compiler.clone(),
        profile,
        sources,
        cancel,
    )
}

fn attempt_dispositions(
    fixture: &AdapterFixture,
    attempts: &[backend_extension_turso::CandidateAttempt],
) -> Vec<backend_extension_turso::AttemptDisposition> {
    futures_executor::block_on(async {
        let authority = backend_extension_turso::TursoAuthority::open(
            fixture
                .root
                .0
                .join("workspace")
                .join(backend_extension_turso::AUTHORITY_FILE_NAME),
        )
        .await
        .expect("reopen actual attempt authority");
        let mut dispositions = Vec::with_capacity(attempts.len());
        for attempt in attempts {
            dispositions.push(
                authority
                    .attempt_disposition(&attempt.recovery_claim())
                    .await
                    .expect("read exact actual attempt disposition"),
            );
        }
        dispositions
    })
}

#[test]
fn existing_index_worker_shutdown_retires_current_and_queued_attempts() {
    use backend_extension_turso::{AttemptDisposition, CandidateAttemptRetirementReason};
    use std::time::{Duration, Instant};
    // Cover the thread-owned connection, a returned startup result, and the
    // already-restored connection while displaced reads finish retiring.
    for mode in [0, 1, 2] {
        let mut fixture = AdapterFixture::new();
        let work = actual_work_with_queue(&mut fixture, true);
        let mut attempts = vec![work.identity.attempt.clone()];
        attempts.extend(work.job.pending_attempts());
        assert_eq!(
            attempt_dispositions(&fixture, &attempts),
            vec![AttemptDisposition::Pending; 2]
        );
        let cancelled = work.cancellation_for_test();
        cancelled.store(true, Ordering::Release);
        let worker = if mode == 1 {
            ProfileWorker::Returned(ProfileCompletion::returned(work, ProfileOutcome::Cancelled))
        } else {
            let mut worker = ProfileWorker::spawn(work)
                .unwrap_or_else(|(_, error)| panic!("spawn actual cancellation worker: {error}"));
            if mode == 2 {
                let deadline = Instant::now() + Duration::from_secs(30);
                let completion = loop {
                    match worker.poll() {
                        Ok(completion) => break completion,
                        Err(returned) => worker = returned,
                    }
                    assert!(
                        Instant::now() < deadline,
                        "actual canceled worker completion"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                };
                let (adapter, daemon) = fixture.parts();
                let completion = adapter
                    .settle_index_profile(daemon, completion)
                    .unwrap_or_else(|_| {
                        panic!("return original writers before payload retirement")
                    });
                assert!(completion.work.semantic.is_none());
                match completion.retire() {
                    Err(worker) => worker,
                    Ok(_) => panic!("production worker retains its read-payload retirement"),
                }
            } else {
                worker
            }
        };
        let adapter = fixture.adapter.as_mut().expect("actual adapter");
        install_transition_job(adapter);
        let job = adapter.indexing.as_mut().expect("actual resident job");
        job.cancelled = cancelled;
        job.work = IndexJobWork::Compiling { worker };
        adapter.close();
        adapter.close(); // Cleanup is terminal and idempotent.
        assert!(matches!(
            adapter.indexing.as_ref().expect("job").work,
            IndexJobWork::Transition
        ));
        drop(fixture.adapter.take());
        assert_eq!(
            attempt_dispositions(&fixture, &attempts),
            vec![AttemptDisposition::Retired(CandidateAttemptRetirementReason::Cancelled); 2],
            "cold authority must retain both exact terminal dispositions"
        );
    }
}

#[test]
fn existing_index_worker_shutdown_preserves_selected_current_and_retires_queue() {
    use backend_extension_turso::{AttemptDisposition, CandidateAttemptRetirementReason};
    let mut fixture = AdapterFixture::new();
    install_mixed_native_compiler(&mut fixture);
    let work = actual_work_with_queue(&mut fixture, true);
    let mut attempts = vec![work.identity.attempt.clone()];
    attempts.extend(work.job.pending_attempts());
    let cancelled = work.cancellation_for_test();
    let completion = ProfileWorker::spawn(work)
        .unwrap_or_else(|(_, error)| panic!("spawn actual native compiler: {error}"))
        .join();
    assert!(
        matches!(&completion.outcome, ProfileOutcome::Admitted(Ok(()))),
        "actual compiled Python profile must be admitted"
    );
    let before = attempt_dispositions(&fixture, &attempts);
    assert!(matches!(&before[0], AttemptDisposition::Published(_)));
    assert_eq!(before[1], AttemptDisposition::Pending);
    let adapter = fixture.adapter.as_mut().expect("actual adapter");
    install_transition_job(adapter);
    let job = adapter.indexing.as_mut().expect("actual resident job");
    job.cancelled = cancelled;
    job.work = IndexJobWork::Compiling {
        worker: ProfileWorker::Returned(completion),
    };
    adapter.close();
    drop(fixture.adapter.take());
    let after = attempt_dispositions(&fixture, &attempts);
    assert_eq!(
        after[0], before[0],
        "cleanup must preserve the exact selected generation"
    );
    assert_eq!(
        after[1],
        AttemptDisposition::Retired(CandidateAttemptRetirementReason::Cancelled)
    );
}

fn settle_then_write(fixture: &mut AdapterFixture, completion: ProfileCompletion) {
    let next_label = format!("{}/next", fixture.label);
    let (adapter, daemon) = fixture.parts();
    let completion = adapter
        .settle_index_profile(daemon, completion)
        .unwrap_or_else(|_| panic!("same-owner completion must return both unique writers"));
    assert!(completion.work.semantic.is_none());
    assert!(completion.work.read_head.is_none());
    let next = backend_engine::package_key(&next_label);
    let intent = BuiltinIntent::add(next, next_label).expect("next actual product intent");
    crate::builtin::commands::adapter::commit_builtin_intent(daemon, 0x992, &intent)
        .expect("next real commit proves workspace lease returned");
    let coordinate = backend_library::interface::PackageUrl::parse(
        "pkg:pypi/next-worker-write@1.0.0".to_owned(),
    )
    .expect("admitted namespace");
    let key = backend_engine::builtin::ProductSemanticPublicationKey::new(
        backend_library::PackageReference::Purl(coordinate.clone()),
        coordinate,
        backend_semantic::vocabulary::LanguageProfile::Python(
            backend_semantic::vocabulary::PythonVersion::Python312,
        ),
    )
    .expect("real semantic key");
    adapter
        .semantic_authority
        .observe(&key, [0x99; 32], 1)
        .expect("next real observation proves original Turso writer returned");
}

#[test]
fn existing_index_worker_caught_admission_panic_returns_writers_for_next_real_commit() {
    let mut fixture = AdapterFixture::new();
    let work = actual_work(&mut fixture);
    let joined = std::thread::spawn(move || {
        work.run_with(|_| panic!("admission callback unwinds while both writers remain borrowed"))
    })
    .join()
    .expect("actual worker retired");
    assert!(matches!(&joined.outcome, ProfileOutcome::Admitted(Err(_))));
    settle_then_write(&mut fixture, joined);
}

#[test]
fn existing_index_worker_spawn_rejection_retains_writers_for_next_real_commit() {
    let mut fixture = AdapterFixture::new();
    let work = actual_work(&mut fixture);
    let work = match ProfileWorker::spawn_with(work, |_| {
        Err(std::io::Error::other(
            "actual startup factory rejected before transfer",
        ))
    }) {
        Err((work, _)) => work,
        Ok(_) => panic!("injected spawn rejection"),
    };
    settle_then_write(
        &mut fixture,
        ProfileCompletion::returned(
            work,
            ProfileOutcome::Admitted(Err(BuiltinModelError("spawn refused".to_owned()).into())),
        ),
    );
}

#[test]
fn existing_index_worker_actual_compiler_refusal_returns_writers_before_terminal() {
    let mut fixture = AdapterFixture::new();
    let work = actual_work(&mut fixture);
    // AdapterFixture uses the actual LocalCompiler owner with an explicitly
    // unavailable Python toolchain. This is typed refusal, not native success.
    let completion = ProfileWorker::spawn(work)
        .unwrap_or_else(|(_, e)| panic!("actual worker spawn: {e}"))
        .join();
    assert!(matches!(&completion.outcome,
        ProfileOutcome::Admitted(Err(failure)) if failure.compiler_failure.is_some()));
    settle_then_write(&mut fixture, completion);
}

#[test]
fn existing_index_worker_retirement_failure_does_not_skip_queued_attempts() {
    let mut fixture = AdapterFixture::new();
    let work = actual_work(&mut fixture);
    let attempt = work.identity.attempt.clone();
    let mut visited = 0usize;
    let error = crate::builtin::commands::adapter::index_profile_worker::retire_failed_attempts(
        [attempt.clone(), attempt.clone(), attempt],
        |_| {
            visited += 1;
            match visited {
                1 => Err(BuiltinModelError(
                    "first durable retirement refused".to_owned(),
                )),
                2 => Err(BuiltinModelError(
                    "second durable retirement refused".to_owned(),
                )),
                _ => Ok(()),
            }
        },
    )
    .expect_err("cleanup failure remains visible");
    assert_eq!(visited, 3, "the later attempt must still receive cleanup");
    assert!(error.to_string().contains("2 attempts"));
    assert!(
        error
            .to_string()
            .contains("first durable retirement refused")
    );
    settle_then_write(
        &mut fixture,
        ProfileCompletion::returned(work, ProfileOutcome::Cancelled),
    );
}

#[cfg(any(unix, windows))]
#[test]
fn existing_index_worker_listener_error_and_drop_cancel_and_join_before_retirement() {
    use crate::listener::{ListenerConfig, ListenerError, UnixListenerService};
    use crate::service::{LocaldOwner, LocaldService};
    use std::sync::Mutex;
    use std::time::Duration;

    for mode in [0, 1, 2] {
        let mut fixture = AdapterFixture::new();
        let work = actual_work(&mut fixture);
        let cancelled = work.cancellation_for_test();
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::clone(&exited);
        let worker_cancel = Arc::clone(&cancelled);
        let (entered, admitted) = std::sync::mpsc::sync_channel(1);
        let worker = ProfileWorker::spawn_with(work, move |receive| {
            Ok(std::thread::spawn(move || {
                receive.recv().expect("actual owned work").run_with(|_| {
                    entered.send(()).expect("test observes active admission");
                    while !worker_cancel.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    worker_exited.store(true, Ordering::Release);
                    ProfileOutcome::Cancelled
                })
            }))
        })
        .unwrap_or_else(|(_, error)| panic!("spawn actual writer-owning worker: {error}"));
        admitted
            .recv_timeout(Duration::from_secs(2))
            .expect("worker owns original Turso connection and read head");
        let adapter = fixture.adapter.as_mut().expect("actual adapter");
        install_transition_job(adapter);
        let job = adapter.indexing.as_mut().expect("resident owner job");
        job.cancelled = Arc::clone(&cancelled);
        job.work = IndexJobWork::Compiling { worker };
        let adapter = Arc::new(Mutex::new(fixture.adapter.take().expect("actual adapter")));
        let daemon = fixture.daemon.take().expect("actual daemon");
        let owner = LocaldOwner::new(
            daemon,
            |_: &mut ProductDaemon, _: &[u8]| -> Result<Vec<u8>, String> {
                Err("commands are not needed for terminal accept failure".to_owned())
            },
        )
        .with_deferred_commands(Box::new(crate::builtin::DeferredBuiltinCommands(
            Arc::clone(&adapter),
        )));
        let path = crate::test_support::socket_path(&format!("active-worker-accept-error-{mode}"));
        let config = ListenerConfig::new(path);
        let service = LocaldService::new(owner, config.limits).expect("actual service");
        let mut listener = UnixListenerService::bind(service, config).expect("actual listener");
        listener.fail_next_accept_for_test(std::io::ErrorKind::Other);
        match mode {
            0 => assert_eq!(
                listener.run(),
                Err(ListenerError::Io(std::io::ErrorKind::Other))
            ),
            1 => assert_eq!(
                listener.run_once(),
                Err(ListenerError::Io(std::io::ErrorKind::Other))
            ),
            _ => drop(listener),
        }
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            exited.load(Ordering::Acquire),
            "terminal listener path must join its actual admission worker"
        );
        assert!(matches!(
            adapter
                .lock()
                .expect("adapter")
                .indexing
                .as_ref()
                .expect("job")
                .work,
            IndexJobWork::Transition
        ));
    }
}

#[test]
fn existing_index_worker_displaced_read_maps_retire_on_its_original_thread() {
    use std::time::{Duration, Instant};
    let mut fixture = AdapterFixture::new();
    let work = actual_work(&mut fixture);
    let mut worker =
        ProfileWorker::spawn(work).unwrap_or_else(|(_, error)| panic!("actual worker: {error}"));
    let deadline = Instant::now() + Duration::from_secs(30);
    let completion = loop {
        match worker.poll() {
            Ok(completion) => break completion,
            Err(returned) => worker = returned,
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(
        completion.outcome,
        ProfileOutcome::Admitted(Err(_))
    ));
    let (adapter, daemon) = fixture.parts();
    let mut completion = adapter
        .settle_index_profile(daemon, completion)
        .unwrap_or_else(|_| panic!("return both original writers"));
    assert!(completion.retired.head.is_some() && completion.retired.semantic.is_some());
    let actor_thread = std::thread::current().id();
    let (entered, observed) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    completion.retired.test_retirement = Some(Box::new(move || {
        assert_ne!(std::thread::current().id(), actor_thread);
        entered
            .send(())
            .expect("actual original compiler thread starts retirement");
        released
            .recv_timeout(Duration::from_secs(30))
            .expect("bounded retirement release");
    }));
    let worker = match completion.retire() {
        Err(worker) => worker,
        Ok(_) => panic!("actual production worker remains owned through retirement"),
    };
    observed
        .recv_timeout(Duration::from_secs(2))
        .expect("worker owns displaced map destruction");
    let health = serde_json::to_vec(&backend_engine::CommandDto::new(0x995, Command::Health))
        .expect("health");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &health, 0x995),
        Ok(Executed::Reply(_))
    ));
    let worker = match worker.poll() {
        Err(worker) => worker,
        Ok(_) => panic!("owner must not report worker retired while destructor is active"),
    };
    release
        .send(())
        .expect("finish old read payload retirement");
    let completion = worker.join();
    settle_then_write(&mut fixture, completion);
}
