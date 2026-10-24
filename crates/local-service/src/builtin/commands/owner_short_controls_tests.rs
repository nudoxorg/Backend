fn short_control_body(
    request_id: u64,
    command: backend_library::SurfaceCommand,
) -> Vec<u8> {
    serde_json::to_vec(&backend_engine::CommandDto::new(request_id, Command::Surface(command)))
        .expect("strict surface request")
}

fn short_control_reply(
    adapter: &mut CommandAdapter,
    daemon: &mut ProductDaemon,
    request_id: u64,
    command: backend_library::SurfaceCommand,
) -> CommandReply {
    let body = short_control_body(request_id, command);
    let Executed::Reply(bytes) = adapter.execute_or_defer(daemon, &body, request_id + 10_000)
        .expect("short owner control") else {
        panic!("short control must not wait for compiler completion");
    };
    let reply = backend_engine::decode_reply_dto(&bytes).expect("strict wire reply admission");
    assert_eq!(reply.request_id, request_id);
    reply.reply
}

#[test]
fn project_bound_subscription_preserves_prior_product_write_order() {
    use backend_library::{ProjectName, ProjectSelector, SurfaceCommand as S, SurfaceReply as R};
    for delete in [false, true] {
        let mut fixture = AdapterFixture::new();
        let (adapter, daemon) = fixture.parts();
        let name = ProjectName::new("Queued follow project").expect("project name");
        if delete {
            assert!(matches!(short_control_reply(adapter, daemon, 2200,
                S::ProjectCreate { name: name.clone(), lockfile: None }),
                CommandReply::Surface(R::ProjectCreated(_))));
        }
        install_transition_job(adapter);
        let project = ProjectSelector::Name(name.clone());
        let first = if delete {
            S::ProjectDelete { project: project.clone() }
        } else {
            S::ProjectCreate { name, lockfile: None }
        };
        let package = backend_library::PackageReference::parse("pkg:cargo/ordered@1.0.0")
            .expect("follow package");
        let queued = [
            first,
            S::Subscribe { package: package.clone(), project: Some(project) },
            S::Subscribe { package: package.clone(), project: None },
            S::Unsubscribe { package: package.clone() },
        ];
        for (ordinal, command) in queued.into_iter().enumerate() {
            let request_id = 2201 + ordinal as u64;
            assert!(matches!(adapter.execute_or_defer(daemon,
                &short_control_body(request_id, command), request_id + 10_000),
                Ok(Executed::Deferred)), "later follow writes must not overtake prior product writes");
        }
        assert_eq!(adapter.waiting.len(), 4);
        assert!(adapter.execute(daemon, &short_control_body(2210,
            S::Subscribe { package: package.clone(), project: None })).is_err(),
            "direct execution must also honor the existing product queue");
        drop(adapter.indexing.take()); // Release only the fixture gate; no compiler receipt is made.
        assert!(adapter.execute(daemon, &short_control_body(2210,
            S::Subscribe { package: package.clone(), project: None })).is_err(),
            "removing the index gate cannot bypass the prior product queue");
        let ready = adapter.poll_deferred(daemon);
        assert_eq!(ready.len(), 4);
        let replies = ready.into_iter().enumerate().map(|(ordinal, (ticket, reply))| {
            let request_id = 2201 + ordinal as u64;
            assert_eq!(ticket, request_id + 10_000);
            let reply = backend_engine::decode_reply_dto(&reply.expect("queued execution"))
                .expect("strict queued wire reply");
            assert_eq!(reply.request_id, request_id);
            reply.reply
        }).collect::<Vec<_>>();
        if delete {
            assert!(matches!(replies[0], CommandReply::Surface(R::ProjectDeleted(_))));
            assert!(matches!(&replies[1], CommandReply::Failed(
                backend_engine::CommandFailure::InvalidQuery(error)) if error == "unknown project"));
        } else {
            let CommandReply::Surface(R::ProjectCreated(project)) = &replies[0] else {
                panic!("queued project creation must precede subscription");
            };
            assert!(matches!(&replies[1], CommandReply::Surface(R::Subscribed(record))
                if record.project == Some(project.id)));
        }
        assert!(matches!(&replies[2], CommandReply::Surface(R::Subscribed(record))
            if record.project.is_none()));
        assert!(matches!(replies[3], CommandReply::Surface(R::Unsubscribed(true))));
        assert!(adapter.waiting.is_empty());
        assert!(matches!(short_control_reply(adapter, daemon, 2211, S::Subscriptions),
            CommandReply::Surface(R::Subscriptions(rows)) if rows.is_empty()));
    }
}

#[test]
fn short_controls_while_index_pending_do_not_consume_publication_writers() {
    use backend_library::{SurfaceCommand as S, SurfaceReply as R};
    let mut fixture = AdapterFixture::new();
    let project = fixture.label.clone();
    let source_package = fixture.package;
    fs::write(Path::new(&project).join("Cargo.toml"), "[package\n").expect("unrelated manifest refusal");
    let (adapter, daemon) = fixture.parts();
    let cancelled = install_transition_job(adapter);
    // Exercise the actual Compiling gate with an owned, bounded worker held
    // on a protocol channel. This does not fabricate a semantic completion.
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || {
        blocked.recv_timeout(Duration::from_secs(30)).expect("release owned worker");
    });
    let (_completed, completion) = std::sync::mpsc::sync_channel(1);
    let (retire, _retired) = std::sync::mpsc::sync_channel(1);
    adapter.indexing.as_mut().expect("active index").work = IndexJobWork::Compiling {
        worker: ProfileWorker::Owned { handle, completion, retire },
    };
    let before = owner_cursor(daemon);
    let workspace = daemon.engine().daemon().owner().snapshot().root();
    let read_head = daemon.engine_mut().daemon_mut().reserve_read_head_writer()
        .expect("compiler owns original read-head writer");
    let writer = adapter.sql_projection.reserve_writer().expect("publication owns original SQL writer");
    assert!(matches!(adapter.execute_or_defer(daemon, &remove_body(2001, source_package, &project), 12001), Ok(Executed::Deferred)));
    let package = backend_library::PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("follow package");
    let reply = short_control_reply(adapter, daemon, 2002, S::Advisory { package: package.clone(), override_evidence: None });
    assert!(matches!(reply, CommandReply::Surface(R::Advisory(value)) if value == backend_library::AdvisoryPackageDto::unknown()));
    assert!(matches!(short_control_reply(adapter, daemon, 2003, S::Subscribe { package: package.clone(), project: None }), CommandReply::Surface(R::Subscribed(record)) if record.package == package));
    assert!(matches!(short_control_reply(adapter, daemon, 2004, S::Subscriptions), CommandReply::Surface(R::Subscriptions(rows)) if rows.len() == 1 && rows[0].package == package));
    assert_eq!(owner_cursor(daemon), before, "short controls cannot select a publication");
    assert_eq!(daemon.engine().daemon().owner().snapshot().root(), workspace);
    assert_eq!(adapter.waiting.len(), 1, "only source mutation remains queued");
    assert!(adapter.dependencies.is_none(), "short controls cannot build a package graph");
    assert!(adapter.indexing.is_some());
    assert!(!cancelled.load(Ordering::Acquire));
    assert!(matches!(&adapter.indexing.as_ref().expect("active compile").work,
        IndexJobWork::Compiling { worker: ProfileWorker::Owned { handle, .. } } if !handle.is_finished()),
        "controls answer before the held compiler worker completes");
    drop(daemon.engine_mut().daemon_mut().return_unselected_read_head_writer(read_head)
        .unwrap_or_else(|_| panic!("return original compiler writer")));
    let returned = adapter.sql_projection.prepare_return(writer).unwrap_or_else(|_| panic!("return original SQL writer"));
    drop(returned.install());
    fs::remove_file(Path::new(&project).join("Cargo.toml")).expect("remove unrelated refusal fixture");
    fs::create_dir(Path::new(&project).join("new-source")).expect("next source directory");
    let next_label = Path::new(&project).join("new-source").to_string_lossy().into_owned();
    let intent = BuiltinIntent::add(backend_engine::package_key(&next_label), next_label).expect("next publication");
    super::commit_builtin_intent(daemon, 2005, &intent).expect("serial source publication");
    adapter.publish_view(daemon, Some(&intent)).expect("install next admitted view");
    assert_ne!(owner_cursor(daemon), before);
    assert!(matches!(short_control_reply(adapter, daemon, 2006, S::Subscriptions), CommandReply::Surface(R::Subscriptions(rows)) if rows.len() == 1 && rows[0].package == package));
    assert!(matches!(short_control_reply(adapter, daemon, 2007, S::Unsubscribe { package }), CommandReply::Surface(R::Unsubscribed(true))));
    assert_eq!(adapter.waiting.len(), 1);
    adapter.waiting.clear();
    let IndexJobWork::Compiling { worker: ProfileWorker::Owned { handle, .. } } =
        adapter.indexing.take().expect("active compile").work else {
        panic!("retain the same owned worker");
    };
    release.send(()).expect("release owned worker after all controls");
    handle.join().expect("join owned worker");
}

#[test]
fn short_advisory_refuses_reserved_configured_authority_without_queueing() {
    use backend_library::{SurfaceCommand as S, SurfaceReply as R};
    let mut fixture = AdapterFixture::new();
    let (adapter, daemon) = fixture.parts();
    install_transition_job(adapter);
    adapter.registry_configured = true;
    let before = owner_cursor(daemon);
    let package = backend_library::PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("package");
    let reply = short_control_reply(adapter, daemon, 2101, S::Advisory { package: package.clone(), override_evidence: None });
    assert!(matches!(reply, CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(error)) if error.contains("configured registry authority") && error.contains("retry")));
    assert!(adapter.waiting.is_empty());
    assert_eq!(owner_cursor(daemon), before);
    assert!(matches!(short_control_reply(adapter, daemon, 2102, S::Subscribe { package, project: None }), CommandReply::Surface(R::Subscribed(_))), "product-only control does not depend on reserved registry authority");
    assert!(adapter.indexing.is_some());
    let gate = backend_engine::advisory::AcquisitionGate {
        offline: backend_engine::advisory::OfflinePolicy::Warn,
    };
    let config = crate::process::RegistryConfig {
        sources: backend_engine::registry::RegistrySourceSet::empty(),
        endpoint: None,
        authentication: None,
        policy: backend_engine::registry::AcquisitionPolicy::Offline,
        native: false,
        limits: backend_engine::registry::AcquisitionLimits::default(),
        advisory_gate: gate,
        cache_max_age_millis: None,
    };
    let advisory = crate::process::AdvisoryConfig {
        sources: Vec::new(),
        osv_scope: None,
        max_age_secs: 3600,
        offline: true,
        refresh_enabled: false,
        gate,
        max_feed_bytes: 1024 * 1024,
    };
    adapter.registry = crate::builtin::RegistryGateway::open(
        &config, adapter.product_state.workspace_path().expect("workspace"), &advisory,
    ).expect("restore actual registry owner");
    let package = backend_library::PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("package");
    assert!(matches!(short_control_reply(adapter, daemon, 2103, S::Advisory { package, override_evidence: None }), CommandReply::Surface(R::Advisory(value)) if value == backend_library::AdvisoryPackageDto::unknown()), "source-checked empty catalog can answer after authority returns");
    assert_eq!(owner_cursor(daemon), before);
    assert!(adapter.waiting.is_empty());
    drop(adapter.indexing.take());
}
