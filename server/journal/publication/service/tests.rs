//! Defines publication service tests behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication service tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use super::super::credit::CreditLease;
use super::super::owner::poison_group;
use super::*;
use std::{
    error::Error,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
};

fn lease(pool: &Arc<CreditPool>) -> io::Result<Arc<CreditLease>> {
    CreditPool::reserve(pool).ok_or_else(|| io::Error::other("bounded lease unavailable"))
}

#[test]
fn response_channel_allocation_and_send_rejection_release_credit() -> io::Result<()> {
    let pool = CreditPool::new(1);
    let lease = lease(&pool)?;
    let input = PublicationInput::from_parts([1; 32], [2; 32]);
    let (response, receiver) = sync_channel(1);
    let command = Command {
        input,
        lease: Arc::clone(&lease),
        response,
    };
    let (queue, queue_receiver) = sync_channel(1);
    drop(queue_receiver);
    let command = match queue.try_send(command) {
        Err(TrySendError::Disconnected(command)) => command,
        Err(TrySendError::Full(_)) => {
            return Err(io::Error::other("queue unexpectedly reported full"));
        }
        Ok(()) => return Err(io::Error::other("queue unexpectedly accepted command")),
    };
    drop(command);
    drop(receiver);
    drop(lease);
    assert_eq!(pool.active_count(), 0);
    Ok(())
}

#[test]
fn response_channel_unwind_after_reserve_does_not_strand_credit() -> Result<(), Box<dyn Error>> {
    let pool = CreditPool::new(1);
    let pool_for_panic = Arc::clone(&pool);
    let result = catch_unwind(AssertUnwindSafe(move || -> io::Result<()> {
        let lease = lease(&pool_for_panic)?;
        let (_sender, _receiver) = sync_channel::<OwnerOutcome>(1);
        drop(lease);
        panic!("injected channel-admission unwind");
    }));
    match result {
        Err(_) => {}
        Ok(Err(error)) => return Err(error.into()),
        Ok(Ok(())) => return Err(io::Error::other("injected unwind did not occur").into()),
    }
    assert_eq!(pool.active_count(), 0);
    Ok(())
}

#[test]
fn poison_closes_admission_before_fanning_out_accepted_commands() -> io::Result<()> {
    let pool = CreditPool::new(1);
    let lease = lease(&pool)?;
    let (response, receiver) = sync_channel(1);
    let mut group = vec![Command {
        input: PublicationInput::from_parts([3; 32], [4; 32]),
        lease,
        response,
    }];
    let state = PublisherState {
        closed: AtomicBool::new(false),
        credits: Arc::clone(&pool),
        published: OnceLock::new(),
        latest: std::sync::Mutex::new(None),
    };
    let mut poison = None;
    poison_group(
        &mut poison,
        PublicationFailure::Poisoned,
        &mut group,
        &state,
    );
    assert!(state.closed.load(Ordering::Acquire));
    assert!(group.is_empty());
    match receiver
        .recv()
        .map_err(|source| io::Error::other(format!("poison terminal receive failed: {source}")))?
    {
        OwnerOutcome::Failed(PublicationFailure::Shared(_)) => {}
        OwnerOutcome::Failed(source) => {
            return Err(io::Error::other(format!("wrong poison source: {source:?}")));
        }
        OwnerOutcome::Published(_) => {
            return Err(io::Error::other("poison unexpectedly published"));
        }
        OwnerOutcome::Cancelled => {
            return Err(io::Error::other("poison unexpectedly cancelled"));
        }
    }
    assert_eq!(pool.active_count(), 0);
    Ok(())
}

#[test]
fn drop_joins_a_panicked_owner_as_last_resort_cleanup() -> io::Result<()> {
    let completed = Arc::new(AtomicBool::new(false));
    let owner_completed = Arc::clone(&completed);
    let owner = thread::spawn(move || -> OwnerExit {
        owner_completed.store(true, Ordering::Release);
        panic!("injected owner panic");
    });
    let state = Arc::new(PublisherState {
        closed: AtomicBool::new(false),
        credits: CreditPool::new(1),
        published: OnceLock::new(),
        latest: std::sync::Mutex::new(None),
    });
    let publisher = DurablePublisher {
        sender: None,
        owner: Some(owner),
        state,
    };
    drop(publisher);
    assert!(completed.load(Ordering::Acquire));
    Ok(())
}
