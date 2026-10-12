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
fn calls(path: &std::path::Path, purpose: u64) -> usize {
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
    adapter.remote_semantic = crate::builtin::query::RemoteSemantic::select_owner(
        &producer.provision,
        &producer.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
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
