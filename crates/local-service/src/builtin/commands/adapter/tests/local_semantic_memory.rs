//! Actual deferred owner admission under shared credit pressure; no model-quality claim.
use super::*;

fn owed_reply(
    adapter: &mut CommandAdapter,
    daemon: &mut ProductDaemon,
    ticket: u64,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    loop {
        let mut replies = adapter.poll_deferred(daemon);
        if let Some((actual, reply)) = replies.pop() {
            assert_eq!(actual, ticket);
            assert!(replies.is_empty());
            return serde_json::from_slice(&reply.expect("owed encoded reply")).expect("wire");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual owner owed reply deadline"
        );
        let health = serde_json::to_vec(&backend_engine::CommandDto::new(9900, Command::Health))
            .expect("health");
        assert!(
            matches!(
                adapter.execute_or_defer(daemon, &health, 9901),
                Ok(Executed::Reply(_))
            ),
            "required semantic work does not block owner health"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn calls(path: &Path, purpose: u64) -> usize {
    fs::read_to_string(path)
        .expect("actual protocol log")
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("raw log row")["purpose"]
                .as_u64()
                == Some(purpose)
        })
        .count()
}
#[test]
fn local_semantic_deferred_shared_pressure_yields_optional_cache_before_reply_and_worker() {
    let producer = crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "plain")
        .expect("actual protocol activation");
    let runtime = producer.provision.runtime().expect("activated runtime");
    let mut fixture = AdapterFixture::new();
    let (adapter, daemon) = fixture.parts();
    adapter.remote_semantic =
        crate::builtin::query::local_semantic::tests::fixture_owner(&producer);
    for at in 0..12 {
        let id = 4000 + at;
        let body = search_body(daemon, id, &format!("warm intent {at}"));
        assert!(matches!(
            adapter.execute_or_defer(daemon, &body, id + 1000),
            Ok(Executed::Deferred)
        ));
        let wire = owed_reply(adapter, daemon, id + 1000);
        assert_eq!(wire["request_id"], id);
        assert_eq!(wire["reply"]["kind"], "search");
        assert_eq!(wire["semantic_search"]["status"], "available");
    }
    let documents = calls(&producer.log, 2);
    let queries = calls(&producer.log, 1);
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())
        .expect("external mandatory work occupies every remaining credit");
    assert_eq!(
        runtime.memory_budget().used(),
        runtime.memory_budget().capacity()
    );
    let body = search_body(daemon, 4500, "warm intent 11");
    assert!(
        matches!(
            adapter.execute_or_defer(daemon, &body, 5500),
            Ok(Executed::Deferred)
        ),
        "actual reply admission reclaims optional storage rather than reject owed search"
    );
    let wire = owed_reply(adapter, daemon, 5500);
    assert_eq!(wire["request_id"], 4500);
    assert_eq!(wire["semantic_search"]["status"], "available");
    assert_eq!(
        calls(&producer.log, 1),
        queries,
        "warm exact answer avoids inference despite reply/output pressure"
    );
    let body = search_body(daemon, 4501, "new owed pressured intent");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, 5501),
        Ok(Executed::Deferred)
    ));
    let wire = owed_reply(adapter, daemon, 5501);
    assert_eq!(wire["request_id"], 4501);
    assert_eq!(wire["reply"]["kind"], "search");
    assert_eq!(wire["semantic_search"]["status"], "available");
    assert_eq!(
        calls(&producer.log, 2),
        documents,
        "selected complete projection survives later-stage pressure"
    );
    assert_eq!(
        calls(&producer.log, 1),
        queries + 1,
        "only new owed text starts actual inference"
    );
    assert!(runtime.memory_budget().used() <= runtime.memory_budget().capacity());
    drop(pressure);
    let body = search_body(daemon, 4502, "new owed pressured intent");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, 5502),
        Ok(Executed::Deferred)
    ));
    let wire = owed_reply(adapter, daemon, 5502);
    assert_eq!(wire["semantic_search"]["status"], "available");
    assert_eq!(
        calls(&producer.log, 1),
        queries + 1,
        "recovering credits reuses answer or exact runtime input cache"
    );
    assert_eq!(calls(&producer.log, 2), documents);
    assert!(adapter.semantic_search_replies.is_empty());
}

fn enter_held_query(adapter: &mut CommandAdapter, daemon: &mut ProductDaemon, entered: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    while !entered.exists() {
        assert!(adapter.poll_deferred(daemon).is_empty());
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn drain_worker_without_reply_dispatch(adapter: &mut CommandAdapter, daemon: &ProductDaemon) {
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    while adapter.remote_semantic.local_preparation().is_some() {
        assert!(adapter.search_completions(daemon).is_empty());
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn local_semantic_sole_ticket_abandon_or_deadline_demotes_completion_without_cancelling_worker() {
    for expire in [false, true] {
        let producer =
            crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "gate-query")
                .expect("actual protocol activation");
        let runtime = producer.provision.runtime().expect("activated runtime");
        let mut fixture = AdapterFixture::new();
        let (adapter, daemon) = fixture.parts();
        adapter.remote_semantic =
            crate::builtin::query::local_semantic::tests::fixture_owner(&producer);
        let body = search_body(daemon, 4600, "sole held intent");
        assert!(matches!(
            adapter.execute_or_defer(daemon, &body, 5600),
            Ok(Executed::Deferred)
        ));
        enter_held_query(adapter, daemon, &producer.entered);
        let workspace = daemon.engine().daemon().owner().snapshot().root();
        let view = daemon.engine().daemon().library().view().root();
        let selected = adapter
            .search_lane
            .selected_cancellation(workspace, view)
            .expect("selected lifetime");
        if expire {
            adapter
                .semantic_search_replies
                .front_mut()
                .expect("sole live ticket")
                .deadline = std::time::Instant::now();
            let replies = adapter.poll_deferred(daemon);
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0].0, 5600);
            let wire: serde_json::Value =
                serde_json::from_slice(replies[0].1.as_ref().expect("bounded deadline reply"))
                    .expect("wire");
            assert_eq!(wire["reply"]["data"]["kind"], "invalid_query");
        } else {
            adapter.abandon_reply(5600);
        }
        assert!(adapter.semantic_search_replies.is_empty());
        assert!(
            !selected.load(Ordering::Acquire),
            "waiter loss never cancels shared work"
        );
        assert!(adapter.remote_semantic.local_preparation().is_some());
        fs::write(&producer.release, b"finish orphaned owned query").expect("release");
        drain_worker_without_reply_dispatch(adapter, daemon);
        let crate::builtin::query::RemoteSemantic::Local(owner) = &adapter.remote_semantic else {
            panic!("local owner")
        };
        assert!(owner.has_retained_projection());
        assert!(!owner.owes_completion(), "no orphan nonoptional completion");
        assert_eq!(
            owner.cached_completions(),
            1,
            "successful shared work becomes optional reuse"
        );
        let log = fs::read_to_string(&producer.log).expect("raw protocol log");
        let pressure = runtime
            .memory_budget()
            .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())
            .expect("all unoccupied credits held");
        let required = runtime
            .memory_budget()
            .reserve(1)
            .expect("mandatory admission reclaims the abandoned first completion");
        let crate::builtin::query::RemoteSemantic::Local(owner) = &adapter.remote_semantic else {
            panic!("local owner")
        };
        assert_eq!(owner.cached_completions(), 0);
        assert!(!owner.owes_completion());
        assert!(owner.has_retained_projection());
        drop(required);
        drop(pressure);
        let retry = search_body(daemon, 4601, "sole held intent");
        assert!(matches!(
            adapter.execute_or_defer(daemon, &retry, 5601),
            Ok(Executed::Deferred)
        ));
        let wire = owed_reply(adapter, daemon, 5601);
        assert_eq!(wire["semantic_search"]["status"], "available");
        assert_eq!(
            fs::read_to_string(&producer.log).expect("raw log"),
            log,
            "retry reuses admitted document/query inputs without another helper call"
        );
    }
}
#[test]
fn local_semantic_exact_live_duplicate_protects_first_completion_until_last_ticket_abandons() {
    let producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "gate-query")
            .expect("actual protocol activation");
    let runtime = producer.provision.runtime().expect("runtime");
    let mut fixture = AdapterFixture::new();
    let (adapter, daemon) = fixture.parts();
    adapter.remote_semantic =
        crate::builtin::query::local_semantic::tests::fixture_owner(&producer);
    for (id, ticket) in [(4700, 5700), (4701, 5701)] {
        let body = search_body(daemon, id, "same held intent");
        assert!(matches!(
            adapter.execute_or_defer(daemon, &body, ticket),
            Ok(Executed::Deferred)
        ));
    }
    enter_held_query(adapter, daemon, &producer.entered);
    adapter.abandon_reply(5700);
    fs::write(&producer.release, b"finish while duplicate remains").expect("release");
    drain_worker_without_reply_dispatch(adapter, daemon);
    let crate::builtin::query::RemoteSemantic::Local(owner) = &adapter.remote_semantic else {
        panic!("local owner")
    };
    assert!(
        owner.owes_completion(),
        "exact surviving duplicate protects delivery"
    );
    assert_eq!(owner.cached_completions(), 0);
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())
        .expect("all free credits held");
    assert!(
        runtime.memory_budget().reserve(1).is_err(),
        "live first answer is never optional cache"
    );
    // A different queued query cannot inherit the old query's delivery ownership.
    drop(pressure);
    let body = search_body(daemon, 4702, "different waiting intent");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, 5702),
        Ok(Executed::Deferred)
    ));
    adapter.abandon_reply(5701);
    let crate::builtin::query::RemoteSemantic::Local(owner) = &adapter.remote_semantic else {
        panic!("local owner")
    };
    assert!(!owner.owes_completion());
    assert_eq!(owner.cached_completions(), 1);
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())
        .expect("all free credits held");
    let required = runtime
        .memory_budget()
        .reserve(1)
        .expect("different query cannot pin abandoned completion");
    drop(required);
    drop(pressure);
    let wire = owed_reply(adapter, daemon, 5702);
    assert_eq!(wire["request_id"], 4702);
    assert_eq!(wire["semantic_search"]["status"], "available");
}

#[test]
fn local_semantic_reattached_ticket_reclaims_owed_interest_before_nontransport_worker_poll() {
    let producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "gate-query")
            .expect("actual protocol activation");
    let runtime = producer.provision.runtime().expect("runtime");
    let queries_before = calls(&producer.log, 1);
    let mut fixture = AdapterFixture::new();
    let (adapter, daemon) = fixture.parts();
    adapter.remote_semantic =
        crate::builtin::query::local_semantic::tests::fixture_owner(&producer);
    let body = search_body(daemon, 4800, "reattached held intent");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, 5800),
        Ok(Executed::Deferred)
    ));
    enter_held_query(adapter, daemon, &producer.entered);
    adapter.abandon_reply(5800);
    let body = search_body(daemon, 4801, "reattached held intent");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, 5801),
        Ok(Executed::Deferred)
    ));
    fs::write(&producer.release, b"finish after reattachment").expect("release");
    // Publication/preparation may observe finished work before the transport tail
    // next runs. Admission itself must already have restored exact live interest.
    drain_worker_without_reply_dispatch(adapter, daemon);
    let crate::builtin::query::RemoteSemantic::Local(owner) = &adapter.remote_semantic else {
        panic!("local owner")
    };
    assert!(owner.owes_completion());
    assert_eq!(owner.cached_completions(), 0);
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())
        .expect("every free credit held");
    assert!(
        runtime.memory_budget().reserve(1).is_err(),
        "reattached live reply remains protected before any transport poll"
    );
    drop(pressure);
    let wire = owed_reply(adapter, daemon, 5801);
    assert_eq!(wire["request_id"], 4801);
    assert_eq!(wire["semantic_search"]["status"], "available");
    assert_eq!(
        calls(&producer.log, 1),
        queries_before + 1,
        "only one shared query after the actual activation probes"
    );
}
