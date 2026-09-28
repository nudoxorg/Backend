//! Bounded deterministic lane routing and staged-output credits shared by compiler hosts.

use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        mpsc::{SyncSender, TrySendError},
    },
};

/// Exact stable owner key for one package/target compilation lineage.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LaneIdentity {
    pub(crate) capability_lineage: [u8; 32],
    pub(crate) profile: [u8; 2],
    pub(crate) stage: u8,
    pub(crate) target: Option<[u8; 32]>,
    pub(crate) source: Option<[u8; 32]>,
}

impl LaneIdentity {
    /// Selects one lane deterministically while retaining the complete equality key on the job.
    pub(crate) fn lane(self, lane_count: usize) -> usize {
        let Some(lane_count) = NonZeroUsize::new(lane_count) else {
            return 0;
        };
        let mut hash = blake3::Hasher::new();
        hash.update(b"compiler-application.lane-owner.v1\0");
        match self.target {
            Some(target) => {
                hash.update(b"package-target\0");
                hash.update(&target);
            }
            None => {
                hash.update(b"source-input\0");
                hash.update(&self.capability_lineage);
                hash.update(&self.profile);
                hash.update(&[self.stage]);
                hash.update(&self.source.unwrap_or([0; 32]));
            }
        }
        let bytes = hash.finalize();
        let route = u64::from_be_bytes(bytes.as_bytes()[..8].try_into().unwrap_or([0; 8]));
        let lane_count = u64::try_from(lane_count.get()).unwrap_or(u64::MAX);
        usize::try_from(route % lane_count).unwrap_or_default()
    }
}

/// Bounded FIFO lane senders. Equal identities always route to the same owner lane.
pub(crate) struct BoundedLaneQueue<Job> {
    senders: Box<[SyncSender<Job>]>,
}

impl<Job> BoundedLaneQueue<Job> {
    pub(crate) fn new(senders: Vec<SyncSender<Job>>) -> Option<Self> {
        (!senders.is_empty()).then(|| Self {
            senders: senders.into_boxed_slice(),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.senders.len()
    }

    pub(crate) fn try_send(
        &self,
        identity: LaneIdentity,
        job: Job,
    ) -> Result<usize, LaneSendError<Job>> {
        let lane = identity.lane(self.senders.len());
        match self.senders[lane].try_send(job) {
            Ok(()) => Ok(lane),
            Err(TrySendError::Full(job)) => Err(LaneSendError::Full(job)),
            Err(TrySendError::Disconnected(job)) => Err(LaneSendError::Closed(job)),
        }
    }
}

pub(crate) enum LaneSendError<Job> {
    Full(Job),
    Closed(Job),
}

/// Global byte credits held from lane admission until the publisher consumes or drops output.
pub(crate) struct StagedOutputBudget {
    capacity: usize,
    used: Mutex<usize>,
}

impl StagedOutputBudget {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            used: Mutex::new(0),
        })
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<StagedOutputLease, StagedOutputFull> {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(next) = used.checked_add(bytes) else {
            return Err(StagedOutputFull {
                requested: bytes,
                available: self.capacity.saturating_sub(*used),
            });
        };
        if next > self.capacity {
            return Err(StagedOutputFull {
                requested: bytes,
                available: self.capacity.saturating_sub(*used),
            });
        }
        *used = next;
        Ok(StagedOutputLease {
            budget: Arc::clone(self),
            bytes,
        })
    }

    #[cfg(test)]
    fn used(&self) -> usize {
        *self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One request's staged-output reservation; drop releases its exact byte credit.
pub(crate) struct StagedOutputLease {
    budget: Arc<StagedOutputBudget>,
    bytes: usize,
}

impl Drop for StagedOutputLease {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *used = used.saturating_sub(self.bytes);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StagedOutputFull {
    pub(crate) requested: usize,
    pub(crate) available: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        num::NonZeroUsize,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc::{Receiver, sync_channel},
        },
        thread,
        time::Duration,
    };

    type Job = Box<dyn FnOnce() + Send + 'static>;

    fn identity(seed: u8) -> LaneIdentity {
        LaneIdentity {
            capability_lineage: [9; 32],
            profile: [1, 2],
            stage: 1,
            target: Some([seed; 32]),
            source: None,
        }
    }

    #[test]
    fn package_target_stays_on_one_lane_across_profile_and_lineage_changes() {
        let first = identity(29);
        let second = LaneIdentity {
            capability_lineage: [17; 32],
            profile: [8, 9],
            stage: 3,
            target: first.target,
            source: None,
        };
        assert_ne!(first, second);
        for lane_count in [1, 2, 3, 8] {
            assert_eq!(first.lane(lane_count), second.lane(lane_count));
        }
    }

    fn queues(lane_count: usize, capacity: usize) -> (BoundedLaneQueue<Job>, Vec<Receiver<Job>>) {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..lane_count {
            let (sender, receiver) = sync_channel(NonZeroUsize::new(capacity).unwrap().get());
            senders.push(sender);
            receivers.push(receiver);
        }
        (BoundedLaneQueue::new(senders).unwrap(), receivers)
    }

    fn workers(receivers: Vec<Receiver<Job>>) -> Vec<thread::JoinHandle<()>> {
        receivers
            .into_iter()
            .map(|receiver| {
                thread::spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        job();
                    }
                })
            })
            .collect()
    }

    fn on_lane(queue: &BoundedLaneQueue<Job>, lane: usize, seed: u8) -> LaneIdentity {
        (0..=u8::MAX)
            .map(identity)
            .find(|candidate| candidate.lane(queue.len()) == lane)
            .unwrap_or_else(|| identity(seed))
    }

    #[test]
    fn a_blocked_lane_does_not_hold_an_independent_lineage() {
        let (queue, receivers) = queues(2, 1);
        let workers = workers(receivers);
        let first_identity = on_lane(&queue, 0, 10);
        let second_identity = on_lane(&queue, 1, 20);
        let (started_tx, started_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        assert!(
            queue
                .try_send(
                    first_identity,
                    Box::new(move || {
                        let _ = started_tx.send(());
                        let _ = release_rx.recv();
                    }),
                )
                .is_ok()
        );
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (complete_tx, complete_rx) = sync_channel(1);
        assert!(
            queue
                .try_send(
                    second_identity,
                    Box::new(move || {
                        let _ = complete_tx.send(());
                    }),
                )
                .is_ok()
        );
        complete_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
        drop(queue);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn one_lineage_has_stable_fifo_ownership_and_serial_execution() {
        let (queue, receivers) = queues(2, 2);
        let workers = workers(receivers);
        let lane_key = identity(44);
        let expected_lane = lane_key.lane(queue.len());
        assert_eq!(lane_key.lane(queue.len()), expected_lane);
        assert_eq!(identity(44), lane_key);
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let first = Arc::clone(&active);
        let first_maximum = Arc::clone(&maximum);
        let (first_started_tx, first_started_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        assert!(
            queue
                .try_send(
                    lane_key,
                    Box::new(move || {
                        let current = first.fetch_add(1, Ordering::AcqRel) + 1;
                        first_maximum.fetch_max(current, Ordering::AcqRel);
                        let _ = first_started_tx.send(());
                        let _ = release_rx.recv();
                        first.fetch_sub(1, Ordering::AcqRel);
                    }),
                )
                .is_ok()
        );
        first_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let second = Arc::clone(&active);
        let second_maximum = Arc::clone(&maximum);
        let (second_started_tx, second_started_rx) = sync_channel(1);
        assert!(
            queue
                .try_send(
                    lane_key,
                    Box::new(move || {
                        let current = second.fetch_add(1, Ordering::AcqRel) + 1;
                        second_maximum.fetch_max(current, Ordering::AcqRel);
                        let _ = second_started_tx.send(());
                        second.fetch_sub(1, Ordering::AcqRel);
                    }),
                )
                .is_ok()
        );
        assert!(
            second_started_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        release_tx.send(()).unwrap();
        second_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(maximum.load(Ordering::Acquire), 1);
        drop(queue);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn a_full_lane_queue_rejects_without_waiting_and_budget_releases_after_publish() {
        let (queue, receivers) = queues(1, 1);
        let workers = workers(receivers);
        let lane_key = identity(5);
        let (started_tx, started_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        assert!(
            queue
                .try_send(
                    lane_key,
                    Box::new(move || {
                        let _ = started_tx.send(());
                        let _ = release_rx.recv();
                    }),
                )
                .is_ok()
        );
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(queue.try_send(lane_key, Box::new(|| {})).is_ok());
        assert!(matches!(
            queue.try_send(lane_key, Box::new(|| {})),
            Err(LaneSendError::Full(_))
        ));

        let budget = StagedOutputBudget::new(10);
        let first = budget.reserve(6).unwrap();
        let second = budget.reserve(4).unwrap();
        assert!(budget.reserve(1).is_err());
        assert_eq!(budget.used(), 10);
        release_tx.send(()).unwrap();
        drop(first);
        assert_eq!(budget.used(), 4);
        drop(second);
        assert_eq!(budget.used(), 0);
        drop(queue);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn output_reservations_reject_one_oversized_package_and_release_on_drop() {
        let budget = StagedOutputBudget::new(32);
        let oversized = budget.reserve(33);
        assert!(oversized.is_err());
        let reservation = budget.reserve(32).unwrap();
        assert_eq!(budget.used(), 32);
        drop(reservation);
        assert_eq!(budget.used(), 0);
    }
}
