use std::sync::{
    Arc,
    atomic::Ordering,
    mpsc::{Receiver, SyncSender, TryRecvError},
};
use std::{collections::TryReserveError, num::NonZeroUsize};

use nudox_workflow::{EventKind, StageKey, WorkflowEvent, WorkflowVersion};

use super::{
    credit::CreditLease,
    errors::{
        PublicationFailure, PublicationIoStep, PublicationLimitError, PublicationOpenError,
        SharedCommitError, SharedPublicationFailure,
    },
    facts::{PublicationFacts, PublicationPaths},
    format::{
        FACT_BYTES, HEAD_BYTES, PublicationInput, path_exists, persist_fact, persist_head,
        read_fact, read_head,
    },
    service::{PublisherState, conflict, journal_failure},
};
use crate::{
    CommitError, FrameSequence, ReceiptFacts,
    journal::{FileJournal, GroupCommitError},
};

pub(super) struct StoredPublication {
    pub(super) input: PublicationInput,
    receipt: ReceiptFacts,
    facts: PublicationFacts,
}

struct PendingJournal {
    key: StageKey,
    receipt: ReceiptFacts,
    input: Option<PublicationInput>,
}

pub(super) struct InitialState {
    pub(super) published: Option<PublicationFacts>,
}

pub(super) enum OpenMode {
    Create,
    Open,
}

pub(super) struct OwnerStorage {
    frames: FrameBuffer,
    group: Vec<Command>,
    fact_bytes: [u8; FACT_BYTES],
    head_bytes: [u8; HEAD_BYTES],
}

impl OwnerStorage {
    pub(super) fn new(group_capacity: NonZeroUsize) -> Result<Self, PublicationLimitError> {
        let frame_bytes = group_capacity
            .get()
            .checked_mul(crate::JOURNAL_FRAME_BYTES)
            .ok_or(PublicationLimitError::FrameBytesOverflow {
                queue_capacity: group_capacity,
            })?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(frame_bytes)
            .map_err(
                |source: TryReserveError| PublicationLimitError::Allocation {
                    resource: "group frame",
                    capacity: frame_bytes,
                    source,
                },
            )?;
        frames.resize(frame_bytes, 0);
        let mut group = Vec::new();
        group
            .try_reserve_exact(group_capacity.get())
            .map_err(
                |source: TryReserveError| PublicationLimitError::Allocation {
                    resource: "group command",
                    capacity: group_capacity.get(),
                    source,
                },
            )?;
        Ok(Self {
            frames: FrameBuffer {
                bytes: frames,
                group_capacity: group_capacity.get(),
            },
            group,
            fact_bytes: [0; FACT_BYTES],
            head_bytes: [0; HEAD_BYTES],
        })
    }
}

struct FrameBuffer {
    bytes: Vec<u8>,
    group_capacity: usize,
}

impl FrameBuffer {
    fn as_mut_slice(&mut self) -> &mut [u8] {
        debug_assert!(
            self.group_capacity
                .checked_mul(crate::JOURNAL_FRAME_BYTES)
                .is_some_and(|expected| expected == self.bytes.len())
        );
        &mut self.bytes
    }
}

pub(super) struct Command {
    pub(super) input: PublicationInput,
    pub(super) lease: Arc<CreditLease>,
    pub(super) response: SyncSender<OwnerOutcome>,
}

/// One immutable physical source shared by every terminal in a failed group.
///
/// The source is reference-counted only at this failure fan-out boundary. No error is rebuilt from
/// its display text, so every terminal still exposes the original causal source and attempted
/// record.
pub(super) struct FailureOwner(Arc<PublicationFailure>);

impl FailureOwner {
    fn new(source: PublicationFailure) -> Self {
        Self(Arc::new(source))
    }

    fn terminal(&self) -> PublicationFailure {
        PublicationFailure::Shared(SharedPublicationFailure(Arc::clone(&self.0)))
    }
}

pub(super) enum OwnerOutcome {
    Published(PublicationFacts),
    Failed(PublicationFailure),
    Cancelled,
}

pub(super) enum OwnerExit {
    Clean,
    Failed(PublicationFailure),
}

pub(super) fn owner_thread(
    paths: PublicationPaths,
    group_capacity: usize,
    mode: OpenMode,
    receiver: Receiver<Command>,
    startup: SyncSender<Result<InitialState, PublicationOpenError>>,
    state: Arc<PublisherState>,
    storage: OwnerStorage,
) -> OwnerExit {
    let OwnerStorage {
        mut frames,
        mut group,
        mut fact_bytes,
        mut head_bytes,
    } = storage;
    let mut journal = match mode {
        OpenMode::Create => match FileJournal::create(paths.journal()) {
            Ok(journal) => journal,
            Err(error) => {
                send_startup_error(startup, PublicationOpenError::Journal(error));
                return OwnerExit::Clean;
            }
        },
        OpenMode::Open => match FileJournal::open(paths.journal()) {
            Ok(journal) => journal,
            Err(error) => {
                send_startup_error(startup, PublicationOpenError::Journal(error));
                return OwnerExit::Clean;
            }
        },
    };
    let existing = match load_existing(&paths, &journal) {
        Ok(existing) => existing,
        Err(error) => {
            send_startup_error(startup, error);
            return OwnerExit::Clean;
        }
    };
    let initial = InitialState {
        published: existing.as_ref().map(|stored| stored.facts),
    };
    if startup.send(Ok(initial)).is_err() {
        return OwnerExit::Clean;
    }
    let mut current = existing;
    let mut pending = journal.last_receipt().and_then(|receipt| {
        (current.is_none()).then_some(PendingJournal {
            key: journal.current_key()?,
            receipt: *receipt,
            input: None,
        })
    });
    let mut poison = None;
    loop {
        let first = match receiver.recv() {
            Ok(command) => command,
            Err(_) => break,
        };
        group.clear();
        group.push(first);
        while group.len() < group_capacity {
            match receiver.try_recv() {
                Ok(command) => group.push(command),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        process_group(
            &mut journal,
            &mut group,
            &state,
            &mut current,
            &mut pending,
            &mut poison,
            OwnerBuffers {
                paths: &paths,
                frames: frames.as_mut_slice(),
                fact_bytes: &mut fact_bytes,
                head_bytes: &mut head_bytes,
            },
        );
    }
    match poison {
        Some(source) => OwnerExit::Failed(source.terminal()),
        None => OwnerExit::Clean,
    }
}

fn send_startup_error(
    startup: SyncSender<Result<InitialState, PublicationOpenError>>,
    error: PublicationOpenError,
) {
    drop(startup.send(Err(error)));
}

struct OwnerBuffers<'paths> {
    paths: &'paths PublicationPaths,
    frames: &'paths mut [u8],
    fact_bytes: &'paths mut [u8; FACT_BYTES],
    head_bytes: &'paths mut [u8; HEAD_BYTES],
}

fn process_group(
    journal: &mut FileJournal,
    group: &mut Vec<Command>,
    state: &PublisherState,
    current: &mut Option<StoredPublication>,
    pending: &mut Option<PendingJournal>,
    poison: &mut Option<FailureOwner>,
    buffers: OwnerBuffers<'_>,
) {
    if let Some(source) = poison.as_ref() {
        finish_poisoned(group, source, state);
        return;
    }

    // A dropped/cancelled pending command remains in the queue until this owner observes it. This
    // is the only point where the queued lease can become a terminal cancellation.
    while let Some(index) = group
        .iter()
        .position(|command| command.lease.is_cancelled())
    {
        let command = group.remove(index);
        finish(command, OwnerOutcome::Cancelled, state);
    }
    let Some(candidate) = group.iter().position(|command| command.lease.claim()) else {
        for command in group.drain(..) {
            finish(command, OwnerOutcome::Cancelled, state);
        }
        return;
    };
    let candidate_input = group[candidate].input;
    let receipt = if let Some(stored) = current.as_ref() {
        if stored.input == candidate_input {
            Some(stored.receipt)
        } else {
            let command = group.remove(candidate);
            finish(
                command,
                OwnerOutcome::Failed(conflict(candidate_input, stored.input)),
                state,
            );
            finish_conflicts(group, stored.input, state);
            return;
        }
    } else if let Some(existing) = pending.as_ref() {
        if existing.key == candidate_input.key
            && existing.input.is_none_or(|input| input == candidate_input)
        {
            Some(existing.receipt)
        } else if let Some(observed) = existing.input {
            let command = group.remove(candidate);
            finish(
                command,
                OwnerOutcome::Failed(conflict(candidate_input, observed)),
                state,
            );
            finish_conflicts(group, observed, state);
            return;
        } else {
            let command = group.remove(candidate);
            let source = PublicationFailure::JournalKeyConflict {
                expected_key: candidate_input.key,
                observed_key: existing.key,
            };
            finish(command, OwnerOutcome::Failed(source), state);
            finish_pending_conflicts(group, existing.key, state);
            return;
        }
    } else {
        let event = WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key: candidate_input.key,
            kind: EventKind::Requested,
        };
        match journal.append_group(&[event], buffers.frames) {
            Ok(receipts) => match receipts.receipt_at(0) {
                Some(receipt) => {
                    let facts = *receipt;
                    *pending = Some(PendingJournal {
                        key: candidate_input.key,
                        receipt: facts,
                        input: Some(candidate_input),
                    });
                    Some(facts)
                }
                None => {
                    poison_group(
                        poison,
                        journal_failure(CommitError::ReceiptOverflow {
                            sequence: FrameSequence::FIRST,
                        }),
                        group,
                        state,
                    );
                    return;
                }
            },
            Err(error) => {
                poison_group(poison, map_group_error(error), group, state);
                return;
            }
        }
    };
    let Some(receipt) = receipt else {
        return;
    };
    if current.is_none() {
        let fact = match persist_fact(buffers.paths, candidate_input, receipt, buffers.fact_bytes) {
            Ok(fact) => fact,
            Err(source) => {
                poison_group(poison, source, group, state);
                return;
            }
        };
        let head = match persist_head(buffers.paths, fact.identity, receipt, buffers.head_bytes) {
            Ok(head) => head,
            Err(source) => {
                poison_group(poison, source, group, state);
                return;
            }
        };
        let facts = PublicationFacts {
            stable: receipt,
            immutable: fact.identity,
            head: head.identity,
        };
        let stored = StoredPublication {
            input: candidate_input,
            receipt,
            facts,
        };
        let mut published = match state.published.lock() {
            Ok(published) => published,
            Err(poisoned) => poisoned.into_inner(),
        };
        *published = Some(facts);
        *current = Some(stored);
        *pending = None;
    }
    let Some(observed) = current.as_ref().map(|stored| stored.input) else {
        poison_group(poison, PublicationFailure::Poisoned, group, state);
        return;
    };
    finish_group_success(group, observed, candidate, state, current);
}

fn finish_poisoned(group: &mut Vec<Command>, source: &FailureOwner, state: &PublisherState) {
    for command in group.drain(..) {
        if command.lease.is_committing() {
            finish(command, OwnerOutcome::Failed(source.terminal()), state);
        } else if command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if command.lease.claim() {
            finish(command, OwnerOutcome::Failed(source.terminal()), state);
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

pub(super) fn poison_group(
    poison: &mut Option<FailureOwner>,
    source: PublicationFailure,
    group: &mut Vec<Command>,
    state: &PublisherState,
) {
    // Poison is terminal for this publisher. Close admission before fanning out the first failure
    // so no later caller can reserve a credit while the owner is draining already accepted work.
    state.closed.store(true, Ordering::Release);
    *poison = Some(FailureOwner::new(source));
    if let Some(owner) = poison.as_ref() {
        finish_poisoned(group, owner, state);
    }
}

fn finish_conflicts(group: &mut Vec<Command>, observed: PublicationInput, state: &PublisherState) {
    for command in group.drain(..) {
        if command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if command.lease.claim() {
            let input = command.input;
            finish(
                command,
                OwnerOutcome::Failed(conflict(input, observed)),
                state,
            );
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn finish_pending_conflicts(
    group: &mut Vec<Command>,
    observed_key: StageKey,
    state: &PublisherState,
) {
    for command in group.drain(..) {
        if command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if command.lease.claim() {
            let expected_key = command.input.key;
            finish(
                command,
                OwnerOutcome::Failed(PublicationFailure::JournalKeyConflict {
                    expected_key,
                    observed_key,
                }),
                state,
            );
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn finish_group_success(
    group: &mut Vec<Command>,
    observed: PublicationInput,
    candidate: usize,
    state: &PublisherState,
    current: &Option<StoredPublication>,
) {
    for (index, command) in group.drain(..).enumerate() {
        let claimed = index == candidate;
        if !claimed && command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if claimed || command.lease.claim() {
            if command.input == observed {
                if let Some(stored) = current.as_ref() {
                    finish(command, OwnerOutcome::Published(stored.facts), state);
                }
            } else {
                let input = command.input;
                finish(
                    command,
                    OwnerOutcome::Failed(conflict(input, observed)),
                    state,
                );
            }
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn finish(command: Command, outcome: OwnerOutcome, _state: &PublisherState) {
    command.lease.complete();
    drop(command.response.send(outcome));
}

fn map_group_error(error: GroupCommitError) -> PublicationFailure {
    match error {
        GroupCommitError::Reduction { attempted, source } => {
            PublicationFailure::Journal(SharedCommitError::reduction(attempted, source))
        }
        GroupCommitError::Poisoned => journal_failure(CommitError::Poisoned),
        GroupCommitError::ReceiptOverflow { sequence } => {
            journal_failure(CommitError::ReceiptOverflow { sequence })
        }
        GroupCommitError::ReceiptConversion { sequence, source } => {
            PublicationFailure::Journal(SharedCommitError::receipt_conversion(sequence, source))
        }
        GroupCommitError::StorageTooSmall { .. } => PublicationFailure::InputMismatch,
        GroupCommitError::OutcomeUnknown {
            attempted,
            first_sequence,
            step,
            source,
            ..
        } => journal_failure(CommitError::OutcomeUnknown {
            attempted: *attempted,
            sequence: first_sequence,
            step,
            source,
        }),
    }
}

fn load_existing(
    paths: &PublicationPaths,
    journal: &FileJournal,
) -> Result<Option<StoredPublication>, PublicationOpenError> {
    if path_exists(&paths.head_temp(), PublicationIoStep::InspectHeadTemp)? {
        return Err(PublicationOpenError::HeadTempPresent);
    }
    let fact = read_fact(&paths.fact())?;
    let head = read_head(&paths.head())?;
    match (fact, head) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(PublicationOpenError::MissingHead),
        (None, Some(_)) => Err(PublicationOpenError::MissingFact),
        (Some(fact), Some(head)) => {
            if PublicationInput::from_parts(fact.input.root, fact.input.dep_set) != fact.input {
                return Err(PublicationOpenError::EncodingMismatch);
            }
            if !journal.receipt_is_current(fact.receipt) {
                return Err(PublicationOpenError::ReceiptMismatch);
            }
            if journal.current_key() != Some(fact.input.key) {
                return Err(PublicationOpenError::JournalKeyMismatch);
            }
            if head.fact_checksum != fact.identity.checksum || head.receipt != fact.receipt {
                return Err(PublicationOpenError::HeadLinkMismatch);
            }
            let facts = PublicationFacts {
                stable: fact.receipt,
                immutable: fact.identity,
                head: head.identity,
            };
            Ok(Some(StoredPublication {
                input: fact.input,
                receipt: fact.receipt,
                facts,
            }))
        }
    }
}

#[cfg(test)]
mod tests;
