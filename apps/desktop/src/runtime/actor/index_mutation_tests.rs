//! First sends without an owner capability never enter an adapter.
use super::*;
use std::sync::atomic::AtomicUsize;

#[test]
fn an_unbound_first_send_is_refused_before_client_execution()
-> Result<(), Box<dyn std::error::Error>> {
    struct CountCalls(Arc<AtomicUsize>);
    impl EngineClient for CountCalls {
        fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(EngineFault::Cancelled)
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let actor = EngineActor::start(CountCalls(calls.clone()), 2)?;
    let project = LocalProjectId::new("/fixture/unbound-first-send")?;
    let request = RequestId::new(0x91);
    assert!(matches!(
        actor.try_submit(EngineRequest::IndexProject {
            owner: None,
            operation: crate::model::index_operation::tests::claim(&project, 0x91),
            project: project.clone(),
            request,
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new(),
        }),
        PushResult::Enqueued
    ));
    let event = crate::runtime::wait::until_some("unbound first-send refusal", || {
        actor
            .drain_events()
            .into_iter()
            .find(|event| event.request == request)
    });
    assert!(
        matches!(event.result, Err(EngineFault::IndexNotSent { project: found, error })
        if found == project && error.code() == crate::core::FaultCode::Protocol)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    actor.shutdown();
    Ok(())
}
