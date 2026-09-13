//! Exercises the `server-workflow` tests durable-shared contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Shared-receiver integration laws for durable append capabilities.

use core::{
    cell::Cell,
    convert::Infallible,
    future::{Future, Ready, ready},
    pin::pin,
    task::{Context, Poll, Waker},
};

use server_workflow::{
    DurableAppend, DurableCommitError, EventKind, StageKey, WORKFLOW_RECORD_BYTES, WorkflowEvent,
    WorkflowRecord, WorkflowState, WorkflowVersion, append_committed,
};
use thiserror::Error;

struct SharedJournal {
    calls: Cell<u8>,
}

impl DurableAppend for SharedJournal {
    type Error = Infallible;
    type Receipt = u8;
    type Append<'journal> = Ready<Result<Self::Receipt, Self::Error>>;

    fn append(&self, _record: WorkflowRecord) -> Self::Append<'_> {
        let receipt = self.calls.get() + 1;
        self.calls.set(receipt);
        ready(Ok(receipt))
    }
}

#[derive(Debug, Error)]
enum ConcurrentCommitTestError {
    #[error("a ready in-memory journal unexpectedly left a commit pending")]
    Pending,
    #[error("the in-memory journal rejected a valid commit")]
    Commit(#[source] DurableCommitError<Infallible>),
}

#[test]
fn shared_receiver_allows_two_commits_to_coexist() -> Result<(), ConcurrentCommitTestError> {
    let journal = SharedJournal {
        calls: Cell::new(0),
    };
    assert_eq!(WORKFLOW_RECORD_BYTES, 68);

    let event = |key| WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([key; 32]),
        kind: EventKind::Requested,
    };
    let mut first = pin!(append_committed(WorkflowState::empty(), event(1), &journal));
    let mut second = pin!(append_committed(WorkflowState::empty(), event(2), &journal));
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    let Poll::Ready(first) = first.as_mut().poll(&mut context) else {
        return Err(ConcurrentCommitTestError::Pending);
    };
    let Poll::Ready(second) = second.as_mut().poll(&mut context) else {
        return Err(ConcurrentCommitTestError::Pending);
    };
    assert_eq!(first.map_err(ConcurrentCommitTestError::Commit)?.receipt, 1);
    assert_eq!(
        second.map_err(ConcurrentCommitTestError::Commit)?.receipt,
        2
    );
    assert_eq!(journal.calls.get(), 2);
    Ok(())
}
