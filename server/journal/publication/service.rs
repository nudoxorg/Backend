//! Defines publication service behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    fs,
    ops::Deref,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvError, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
};

use heart_hydration::{VerifiedGeneration, VerifiedGenerationFacts};
use heart_identity::Domain;

use super::{
    credit::{CreditPool, PendingLease},
    errors::{
        CancelError, PublicationConflict, PublicationError, PublicationFailure, PublicationIoStep,
        PublicationOpenError, PublicationStateConflict, SharedCommitError,
        SharedPublicationFailure, ShutdownError, SubmitError,
    },
    facts::{PublicationFacts, PublicationLimits, PublicationPaths},
    format::PublicationInput,
    latest::{LatestPublication, LatestPublicationReadError},
    owner::{Command, OpenMode, OwnerExit, OwnerOutcome, OwnerStorage, owner_thread},
};
use crate::CommitError;

/// The single owner service for one local journal and its publication artifacts.
pub struct DurablePublisher {
    sender: Option<SyncSender<Command>>,
    owner: Option<JoinHandle<OwnerExit>>,
    state: Arc<PublisherState>,
}

impl std::fmt::Debug for DurablePublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurablePublisher")
            .field("open", &self.sender.is_some())
            .finish_non_exhaustive()
    }
}

impl DurablePublisher {
    /// Creates a new local journal and starts its exclusive owner thread.
    pub fn create(
        paths: &PublicationPaths,
        limits: PublicationLimits,
    ) -> Result<Self, PublicationOpenError> {
        fs::create_dir_all(&paths.directory).map_err(|source| PublicationOpenError::Io {
            step: PublicationIoStep::CreateFact,
            source,
        })?;
        Self::start(paths, limits, OpenMode::Create)
    }

    /// Reopens and independently validates the journal, immutable fact, and visible head.
    pub fn reopen(
        paths: &PublicationPaths,
        limits: PublicationLimits,
    ) -> Result<Self, PublicationOpenError> {
        Self::start(paths, limits, OpenMode::Open)
    }

    /// Submits one real verified generation without blocking for owner or filesystem work.
    pub fn try_publish<'store, DomainTag, PayloadOwner>(
        &self,
        verified: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    ) -> Result<
        PendingPublication<'store, DomainTag, PayloadOwner>,
        SubmitError<'store, DomainTag, PayloadOwner>,
    >
    where
        DomainTag: Domain,
        PayloadOwner: AsRef<[u8]>,
    {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(SubmitError::Closed {
                generation: verified,
            });
        }
        let Some(reservation) = CreditPool::reserve(&self.state.credits) else {
            return Err(SubmitError::Full {
                generation: verified,
            });
        };
        let facts = *verified;
        let input = PublicationInput::from_facts(facts);
        let (response_sender, response_receiver) = sync_channel(1);
        let command = Command {
            input,
            lease: Arc::clone(&reservation),
            response: response_sender,
        };
        let Some(sender) = self.sender.as_ref() else {
            drop(command);
            drop(reservation);
            return Err(SubmitError::Closed {
                generation: verified,
            });
        };
        match sender.try_send(command) {
            Ok(()) => Ok(PendingPublication {
                verified,
                input,
                response: response_receiver,
                lease: PendingLease(reservation),
            }),
            Err(TrySendError::Full(command)) => {
                drop(command);
                drop(reservation);
                Err(SubmitError::Full {
                    generation: verified,
                })
            }
            Err(TrySendError::Disconnected(command)) => {
                drop(command);
                drop(reservation);
                Err(SubmitError::Closed {
                    generation: verified,
                })
            }
        }
    }

    /// Returns independently validated current publication facts, if a head is visible.
    pub fn published(&self) -> Result<Option<PublicationFacts>, PublicationOpenError> {
        self.state.latest.read().map_err(|source| match source {
            LatestPublicationReadError::Snapshot(source) => {
                PublicationOpenError::Snapshot(source)
            }
            LatestPublicationReadError::Generation(source) => {
                PublicationOpenError::Generation(source)
            }
        })
    }

    /// Closes admission, drains accepted commands, and joins the owner exactly once.
    pub fn shutdown(mut self) -> Result<(), ShutdownError> {
        self.state.closed.store(true, Ordering::Release);
        drop(self.sender.take());
        let Some(owner) = self.owner.take() else {
            return Ok(());
        };
        match owner.join() {
            Ok(OwnerExit::Clean) => Ok(()),
            Ok(OwnerExit::Failed(source)) => Err(ShutdownError::Owner(source)),
            Err(payload) => Err(ShutdownError::Join(
                super::errors::PublicationOwnerPanic::capture(payload.as_ref()),
            )),
        }
    }

    fn start(
        paths: &PublicationPaths,
        limits: PublicationLimits,
        mode: OpenMode,
    ) -> Result<Self, PublicationOpenError> {
        let queue_capacity = limits.queue_capacity.get();
        let group_capacity = limits.group_capacity.get();
        let credits = CreditPool::new(queue_capacity);
        let storage =
            OwnerStorage::new(limits.group_capacity).map_err(PublicationOpenError::Capacity)?;
        let state = Arc::new(PublisherState {
            closed: AtomicBool::new(false),
            credits,
            published: OnceLock::new(),
            latest: LatestPublication::empty(),
        });
        let (sender, receiver) = sync_channel(queue_capacity);
        let (startup_sender, startup_receiver) = sync_channel(1);
        let owner_paths = paths.clone();
        let owner_state = Arc::clone(&state);
        let owner = thread::Builder::new()
            .name("server-publication-owner".to_owned())
            .spawn(move || {
                owner_thread(
                    owner_paths,
                    group_capacity,
                    mode,
                    receiver,
                    startup_sender,
                    owner_state,
                    storage,
                )
            })
            .map_err(|source| PublicationOpenError::Io {
                step: PublicationIoStep::CreateFact,
                source,
            })?;
        let startup = match startup_receiver.recv() {
            Ok(startup) => startup,
            Err(source) => {
                drop(sender);
                return match owner.join() {
                    Ok(_) => Err(PublicationOpenError::OwnerStartupLost { source }),
                    Err(payload) => Err(PublicationOpenError::OwnerStartupPanic(
                        super::errors::PublicationOwnerPanic::capture(payload.as_ref()),
                    )),
                };
            }
        };
        let initial = match startup {
            Ok(initial) => initial,
            Err(error) => {
                drop(sender);
                drop(owner.join());
                return Err(error);
            }
        };
        if let Some(published) = initial.published {
            state.published.set(published).map_err(|attempted| {
                PublicationOpenError::PublishedStateConflict(Arc::new(PublicationStateConflict {
                    retained: state.published.get().copied(),
                    attempted,
                }))
            })?;
            state
                .latest
                .replace(published)
                .map_err(PublicationOpenError::Snapshot)?;
        }
        Ok(Self {
            sender: Some(sender),
            owner: Some(owner),
            state,
        })
    }
}

impl Drop for DurablePublisher {
    fn drop(&mut self) {
        self.state.closed.store(true, Ordering::Release);
        drop(self.sender.take());
        if let Some(owner) = self.owner.take() {
            drop(owner.join());
        }
    }
}

/// An admitted publication retaining its exact verified witness until terminal observation.
pub struct PendingPublication<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    verified: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    input: PublicationInput,
    response: Receiver<OwnerOutcome>,
    lease: PendingLease,
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for PendingPublication<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingPublication")
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl<'store, DomainTag, PayloadOwner> PendingPublication<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// Waits for one stable owner terminal and binds the exact witness on success.
    pub fn wait(
        self,
    ) -> Result<
        PublishedGeneration<'store, DomainTag, PayloadOwner>,
        PublicationError<'store, DomainTag, PayloadOwner>,
    > {
        let Self {
            verified,
            input,
            response,
            lease: _pending_lease,
        } = self;
        let outcome = response.recv();
        match outcome {
            Ok(OwnerOutcome::Published(publication)) => {
                if !input.matches_publication(publication) {
                    return Err(PublicationError::Failed {
                        generation: verified,
                        source: shared_failure(PublicationFailure::InputMismatch),
                    });
                }
                let facts = *verified;
                Ok(PublishedGeneration {
                    facts,
                    publication,
                    _seal: PublicationSeal {
                        _verified: verified,
                    },
                })
            }
            Ok(OwnerOutcome::Failed(source)) => Err(PublicationError::Failed {
                generation: verified,
                source: shared_failure(source),
            }),
            Ok(OwnerOutcome::Cancelled) => Err(PublicationError::Cancelled {
                generation: verified,
            }),
            Err(source) => Err(PublicationError::OwnerLost {
                generation: verified,
                source,
            }),
        }
    }

    /// Cancels this pending command and returns its exact witness when cancellation wins.
    pub fn cancel(
        self,
    ) -> Result<
        VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        CancelError<'store, DomainTag, PayloadOwner>,
    > {
        let Self {
            verified,
            response,
            lease: pending_lease,
            ..
        } = self;
        let lease = pending_lease.arc();
        let _cancel_won = lease.cancel();
        match response.recv() {
            Ok(OwnerOutcome::Published(publication)) => Err(CancelError::Completed {
                generation: verified,
                publication: Arc::new(publication),
            }),
            Ok(OwnerOutcome::Failed(source)) => Err(CancelError::Failed {
                generation: verified,
                source: shared_failure(source),
            }),
            Ok(OwnerOutcome::Cancelled) => Ok(verified),
            Err(_) => Err(CancelError::OwnerLost {
                generation: verified,
                source: RecvError,
            }),
        }
    }
}

/// A witness-bound durable publication authority.
#[non_exhaustive]
pub struct PublishedGeneration<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// Independently checked immutable publication and head facts.
    pub publication: PublicationFacts,
    facts: VerifiedGenerationFacts,
    _seal: PublicationSeal<'store, DomainTag, PayloadOwner>,
}

impl<DomainTag, PayloadOwner> Deref for PublishedGeneration<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    type Target = VerifiedGenerationFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for PublishedGeneration<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishedGeneration")
            .field("facts", &self.facts)
            .field("publication", &self.publication)
            .finish_non_exhaustive()
    }
}

struct PublicationSeal<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    _verified: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
}

#[derive(Debug)]
pub(super) struct PublisherState {
    pub(super) closed: AtomicBool,
    pub(super) credits: Arc<CreditPool>,
    pub(super) published: OnceLock<PublicationFacts>,
    pub(super) latest: LatestPublication,
}

pub(super) fn conflict(
    expected: PublicationInput,
    observed: PublicationInput,
) -> PublicationFailure {
    PublicationFailure::Conflict {
        facts: Arc::new(PublicationConflict {
            expected_root: expected.root,
            expected_dep_set: expected.dep_set,
            observed_root: observed.root,
            observed_dep_set: observed.dep_set,
        }),
    }
}

pub(super) fn journal_failure(error: CommitError) -> PublicationFailure {
    PublicationFailure::Journal(SharedCommitError::new(error))
}

pub(super) fn shared_failure(source: PublicationFailure) -> SharedPublicationFailure {
    SharedPublicationFailure(Arc::new(source))
}

#[cfg(test)]
mod tests;
