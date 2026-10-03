//! One publication observer on the existing owner worker. The actor supplies
//! its first admitted root; subsequent reads share this latest worker root.
//! No socket, hydration, or retry sleep runs on GPUI. A short socket timeout
//! ticks the same in-flight frame; native dial/authentication and decoding
//! syscalls remain bounded by their platform behavior, not preempted here.

use crate::runtime::owner::{Epoch, ObservationFailure, OwnerGate, PublicationAdmission};
use backend_client::{
    ClientError, LocalSubscriptionTransport, ObservedPublicationControl, PublicationBudgetKind,
    PublicationExchangeBudget, PublicationExchangeError, PublicationLease,
    PublicationObservationDecision, PublicationObservationProgress, PublicationOperation,
};
use backend_replication::LocalControlExchangeFailure;
use std::cell::Cell;
use std::path::Path;
use std::time::{Duration, Instant};

/// Candidate policy pending blocked-owner latency measurements. The old three
/// one-second attempts plus pauses failed after about 3.75 s; neither that
/// failure point nor first-window startup measures a healthy subscription.
#[derive(Clone, Copy)]
struct ObservationTiming {
    poll: Duration,
    io_tick: Duration,
    dial: Duration,
    freshness: Duration,
    ordinary_recovery: Duration,
    first_root: Duration,
    renew: Duration,
    max_reconnects: usize,
}

impl ObservationTiming {
    const CANDIDATE: Self = Self {
        poll: Duration::from_millis(250),
        io_tick: Duration::from_secs(1),
        dial: Duration::from_secs(1),
        freshness: Duration::from_secs(3),
        ordinary_recovery: Duration::from_secs(30),
        first_root: Duration::from_secs(60),
        renew: Duration::from_secs(5),
        max_reconnects: 3,
    };
}

enum ObservedReply {
    Acquired(PublicationLease),
    Resumed,
    Renewed,
}

/// Keeps the host alive and observes its exact serving attachment until
/// closure, or until an explicit retry replaces a failed subscription.
pub(super) fn serve(gate: &OwnerGate, endpoint: &Path) -> bool {
    serve_with_timing(gate, endpoint, ObservationTiming::CANDIDATE, |_| {})
}

fn serve_with_timing(
    gate: &OwnerGate,
    endpoint: &Path,
    timing: ObservationTiming,
    mut on_tick: impl FnMut(PublicationObservationProgress),
) -> bool {
    let Some(mut attachment) = gate.ready_epoch() else {
        return gate.await_close_or_restart();
    };
    let Some(mut cancel) = gate.observation_scope(attachment) else {
        return gate.await_close_or_restart();
    };
    let first_root_at = Instant::now();
    let mut confirmed_at: Option<Instant> = None;
    let mut lease: Option<PublicationLease> = None;
    let mut last_renew = Instant::now();
    let mut connection: Option<LocalSubscriptionTransport> = None;
    let mut reconnects = 0usize;
    let mut suspended = false;
    let mut reacquiring = false;
    // Once an authenticated descriptor extends the budget, a reconnect may
    // not restart that aggregate clock. Cell is worker-local, shared only by
    // the two short callbacks borrowed by one observed operation.
    let reset_ceiling: Cell<Option<PublicationExchangeBudget>> = Cell::new(None);

    while !cancel.is_cancelled() {
        let Some((root, cursor)) = gate.publication(attachment) else {
            if first_root_at.elapsed() >= timing.first_root {
                gate.observation_failed(
                    attachment,
                    ObservationFailure::InitialRoot(timing.first_root),
                );
                break;
            }
            if !gate.observation_pause(attachment, timing.poll) {
                break;
            }
            continue;
        };
        let anchor = *confirmed_at.get_or_insert_with(Instant::now);
        if !fence_if_stale(
            gate,
            &mut attachment,
            &mut suspended,
            anchor,
            timing.freshness,
        ) {
            break;
        }
        let deadline = reset_ceiling
            .get()
            .map_or(anchor + timing.ordinary_recovery, |budget| {
                budget.deadline()
            });
        if Instant::now() >= deadline {
            let failure = reset_ceiling.get().map_or(
                ObservationFailure::RecoveryExpired(timing.ordinary_recovery),
                |budget| budget_expired(budget),
            );
            gate.observation_failed(attachment, failure);
            break;
        }

        if connection.is_none() {
            match LocalSubscriptionTransport::connect_with_timeouts(
                endpoint,
                timing.dial,
                timing.io_tick,
            ) {
                Ok(transport) => connection = Some(transport),
                Err(error) => {
                    let cause = ObservationFailure::Setup(error);
                    if reacquiring {
                        gate.observation_failed(
                            attachment,
                            ObservationFailure::ReacquisitionFailed(Box::new(cause)),
                        );
                        break;
                    }
                    reconnects += 1;
                    if reconnects >= timing.max_reconnects {
                        gate.observation_failed(
                            attachment,
                            ObservationFailure::ReconnectsExhausted(Box::new(cause)),
                        );
                        break;
                    }
                    if !gate.observation_pause(attachment, timing.poll * reconnects as u32) {
                        break;
                    }
                    continue;
                }
            }
        }
        let Some(interrupt) = connection
            .as_ref()
            .and_then(LocalSubscriptionTransport::interrupt_handle)
        else {
            gate.observation_failed(attachment, ObservationFailure::MissingInterrupt);
            break;
        };
        let result = {
            // Select the initial operation before the progress callback may
            // suspend this attachment. A later suspension still cancels the
            // in-flight operation through `tick`.
            let may_renew = !suspended
                && reset_ceiling.get().is_none()
                && last_renew.elapsed() >= timing.renew;
            let _wake = cancel.on_cancel(move || interrupt.interrupt());
            let cancelled = || {
                cancel.is_cancelled()
                    || reset_ceiling
                        .get()
                        .is_some_and(|budget| Instant::now() >= budget.deadline())
            };
            let mut tick = |progress: PublicationObservationProgress| {
                on_tick(progress);
                if matches!(
                    progress.budget.kind(),
                    PublicationBudgetKind::AuthenticatedReset { .. }
                ) {
                    let earlier = reset_ceiling
                        .get()
                        .is_none_or(|budget| progress.budget.deadline() < budget.deadline());
                    if earlier {
                        reset_ceiling.set(Some(progress.budget));
                    }
                }
                if !fence_if_stale(
                    gate,
                    &mut attachment,
                    &mut suspended,
                    anchor,
                    timing.freshness,
                ) || reset_ceiling
                    .get()
                    .is_some_and(|budget| Instant::now() >= budget.deadline())
                {
                    PublicationObservationDecision::Cancel
                } else {
                    PublicationObservationDecision::Continue
                }
            };
            let mut control = ObservedPublicationControl::new(deadline, &cancelled, &mut tick);
            let Some(transport) = connection.as_mut() else {
                break;
            };
            let result = match lease.as_mut() {
                Some(state) if may_renew =>
                {
                    transport
                        .renew_publications_observed(state, &mut control)
                        .map(|()| ObservedReply::Renewed)
                }
                Some(state) => transport
                    .resume_publications_observed(state, &mut control)
                    .map(|()| ObservedReply::Resumed),
                None => transport
                    .acquire_publications_observed(root, cursor, &mut control)
                    .map(ObservedReply::Acquired),
            };
            if matches!(
                control.budget().kind(),
                PublicationBudgetKind::AuthenticatedReset { .. }
            ) {
                let budget = control.budget();
                if reset_ceiling
                    .get()
                    .is_none_or(|earlier| budget.deadline() < earlier.deadline())
                {
                    reset_ceiling.set(Some(budget));
                }
            }
            result
        };
        if cancel.is_cancelled() || gate.observation_scope(attachment).is_none() {
            break;
        }
        match result {
            Ok(ObservedReply::Renewed) => {
                last_renew = Instant::now();
                reconnects = 0;
            }
            Ok(reply) => {
                if let ObservedReply::Acquired(state) = reply {
                    lease = Some(state);
                }
                // The client returns only after complete proof and final Ack.
                // A reset budget belongs to that finished operation, not the
                // next ordinary quiet Resume.
                reset_ceiling.set(None);
                let Some(state) = lease.as_ref() else {
                    gate.observation_failed(
                        attachment,
                        invalid_authority("completed publication omitted its lease"),
                    );
                    break;
                };
                match gate.publish_view(attachment, state.root(), state.cursor()) {
                    PublicationAdmission::Admitted => {
                        if suspended && !gate.complete_observation(attachment) {
                            gate.observation_failed(
                                attachment,
                                invalid_authority("certified publication did not reopen its exact attachment"),
                            );
                            break;
                        }
                        suspended = false;
                        reacquiring = false;
                        confirmed_at = Some(Instant::now());
                        reconnects = 0;
                    }
                    PublicationAdmission::Obsolete if !reacquiring && !suspended => {
                        reconnects = 0;
                    }
                    PublicationAdmission::Obsolete => {
                        gate.observation_failed(
                            attachment,
                            invalid_authority("fresh publication regressed behind retained certified cursor"),
                        );
                        break;
                    }
                    PublicationAdmission::Withdrawn => break,
                    PublicationAdmission::Invalid => {
                        gate.observation_failed(
                            attachment,
                            invalid_authority("publication changed producer stream identity"),
                        );
                        break;
                    }
                }
            }
            Err(error) => {
                if reset_ceiling
                    .get()
                    .is_some_and(|budget| Instant::now() >= budget.deadline())
                {
                    if let Some(budget) = reset_ceiling.get() {
                        gate.observation_failed(attachment, budget_expired(budget));
                    }
                    break;
                }
                if matches!(
                    &error,
                    PublicationExchangeError::ProducerRejected {
                        operation: PublicationOperation::Resume,
                        ..
                    }
                ) && lease.is_some()
                    && !reacquiring
                {
                    lease = None;
                    connection = None;
                    let Some((next_attachment, next_cancel)) = gate.replace_observation(attachment)
                    else {
                        break;
                    };
                    attachment = next_attachment;
                    cancel = next_cancel;
                    suspended = true;
                    reacquiring = true;
                    reconnects = 0;
                    continue;
                }
                let retry = recoverable(&error);
                let cause = classify(error);
                if reacquiring {
                    gate.observation_failed(
                        attachment,
                        ObservationFailure::ReacquisitionFailed(Box::new(cause)),
                    );
                    break;
                }
                if retry {
                    connection = None;
                    // A lost Renew reply is resolved by exact-cursor Resume
                    // on the replacement socket, not by repeating Renew.
                    last_renew = Instant::now();
                    reconnects += 1;
                    if reconnects < timing.max_reconnects {
                        if !gate.observation_pause(attachment, timing.poll * reconnects as u32) {
                            break;
                        }
                        continue;
                    }
                    gate.observation_failed(
                        attachment,
                        ObservationFailure::ReconnectsExhausted(Box::new(cause)),
                    );
                } else {
                    gate.observation_failed(attachment, cause);
                }
                break;
            }
        }
        if !gate.observation_pause(attachment, timing.poll) {
            break;
        }
    }
    // Best-effort exact-socket Cancel is bounded to 50 ms. A retired socket
    // can reject it; producer lease expiry independently reclaims that case.
    if let (Some(transport), Some(state)) = (&mut connection, &lease) {
        let _ = transport.cancel_publications_current(state);
    }
    drop(connection);
    gate.await_close_or_restart()
}

fn fence_if_stale(
    gate: &OwnerGate,
    attachment: &mut Epoch,
    suspended: &mut bool,
    certified: Instant,
    threshold: Duration,
) -> bool {
    if *suspended || certified.elapsed() < threshold {
        return true;
    }
    let Some(next) = gate.suspend_observation(*attachment) else {
        return false;
    };
    *attachment = next;
    *suspended = true;
    true
}

fn budget_expired(budget: PublicationExchangeBudget) -> ObservationFailure {
    ObservationFailure::BudgetExpired {
        kind: budget.kind(),
        allowance: budget.allowance(),
    }
}

fn invalid_authority(message: &str) -> ObservationFailure {
    ObservationFailure::InvalidAuthority(ClientError::Protocol(message.to_owned()))
}

fn recoverable(error: &PublicationExchangeError) -> bool {
    match error {
        PublicationExchangeError::Setup(ClientError::Io(_) | ClientError::Disconnected(_)) => true,
        PublicationExchangeError::Exchange(exchange) => matches!(
            exchange.failure,
            LocalControlExchangeFailure::Closed | LocalControlExchangeFailure::Io(_)
        ),
        _ => false,
    }
}

fn classify(error: PublicationExchangeError) -> ObservationFailure {
    match error {
        PublicationExchangeError::Setup(error) => ObservationFailure::Setup(error),
        PublicationExchangeError::Exchange(exchange) => match exchange.failure {
            LocalControlExchangeFailure::Stalled => ObservationFailure::ResponseStalled {
                phase: exchange.progress.phase,
                elapsed: exchange.progress.elapsed,
            },
            LocalControlExchangeFailure::Closed => ObservationFailure::PeerClosed {
                phase: exchange.progress.phase,
            },
            LocalControlExchangeFailure::Io(kind) => ObservationFailure::TransportIo {
                phase: exchange.progress.phase,
                kind,
            },
            LocalControlExchangeFailure::Protocol(error) => ObservationFailure::InvalidFrame {
                phase: exchange.progress.phase,
                error,
            },
            LocalControlExchangeFailure::Cancelled => ObservationFailure::UnexpectedCancellation,
        },
        PublicationExchangeError::ProducerRejected {
            operation, message, ..
        } => ObservationFailure::ProducerRejected {
            operation,
            detail: message.into(),
        },
        PublicationExchangeError::Invalid(error) => ObservationFailure::InvalidAuthority(error),
        PublicationExchangeError::Cancelled => ObservationFailure::UnexpectedCancellation,
        PublicationExchangeError::BudgetExpired(budget) => budget_expired(budget),
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::panic)]
#[path = "observation_tests.rs"]
mod tests;
