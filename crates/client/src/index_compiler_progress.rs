//! Caller-side composition of existing durable status and exact-ticket progress.
//! These values are transient; saved operation and publication authority do not change.
use super::*;
use backend_library::IndexProgressPage;

// One probe reads at most the current owner's 256-event history in 16-event
// pages. This bounds caller work; it does not cap owner/compiler progress.
const MAX_PROFILE_PROGRESS_PAGES: usize = 16;

/// An admitted durable observation with optional, separately read compiler milestones.
#[derive(Clone, Debug)]
pub struct IndexOperationProgress {
    /// Publication and cancellation authority remains this durable-key observation.
    pub observation: IndexOperationObservation,
    /// A bounded page for the exact active ticket; never a publication receipt.
    pub profile_progress: Option<IndexProgressPage>,
}

/// Changed composed facts or the same bounded conditional continuation.
#[derive(Debug)]
pub enum IndexCompilerProgressUpdate {
    /// A complete admitted observation and its optional exact-ticket page.
    Changed(IndexOperationProgress),
    /// No durable or compiler milestone advancement during the requested wait.
    Unchanged(IndexProgressReference),
}

impl IndexObservationFingerprint {
    /// Commits the existing status and a separately admitted exact-ticket page.
    /// This cursor is compared by the caller-side composed reader, not the owner.
    /// # Errors
    /// Refuses foreign tickets, inconsistent stages, or malformed progress pages.
    pub fn active_with_profile_progress(
        status: &IndexOperationStatus,
        page: Option<&IndexProgressPage>,
    ) -> Result<Option<Self>, ClientError> {
        let Some(base) = Self::active(status)? else {
            if page.is_some() {
                return Err(ClientError::Protocol(
                    "inactive operation has compiler progress".to_owned(),
                ));
            }
            return Ok(None);
        };
        let Some(page) = page else {
            return Ok(Some(base));
        };
        SurfaceReply::IndexProgress(IndexJobObservation::Pending(page.clone()))
            .admit(backend_library::CommandId::IndexProgress)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        if !matches!(&status.state, IndexOperationState::Active { ticket, stage }
            if ticket == &page.ticket && stage == &page.stage)
        {
            return Err(ClientError::Protocol(
                "compiler progress changed active ticket or stage".to_owned(),
            ));
        }
        if let Some(capture) = &status.source_capture {
            if page.events.iter().any(|event| match &event.kind {
                backend_library::IndexJobProgressKind::ProfileStarted { profile, .. }
                | backend_library::IndexJobProgressKind::ProfileAdmitted { profile, .. }
                | backend_library::IndexJobProgressKind::ProfileRefused { profile, .. } => !capture
                    .profiles()
                    .iter()
                    .any(|source| source.profile == *profile),
                backend_library::IndexJobProgressKind::StageChanged { .. }
                | backend_library::IndexJobProgressKind::PublicationPending { .. }
                | backend_library::IndexJobProgressKind::PublicationRetryAdmitted => false,
            }) {
                return Err(ClientError::Protocol(
                    "compiler progress profile is outside this operation source capture".to_owned(),
                ));
            }
        }
        // Page boundaries/retention are transport observations, not new owner work.
        // The latest sequence+typed event is stable when an incremental page and
        // a fresh history page end at the same milestone.
        if page.next_sequence == 0 && page.events.is_empty() {
            return Ok(Some(base));
        }
        let bytes = serde_json::to_vec(&(page.next_sequence, page.events.last()))
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"nudox/index-active-profile-observation/v1\0");
        digest.update(&base.0);
        digest.update(&bytes);
        Ok(Some(Self(*digest.finalize().as_bytes())))
    }
}

/// Reads existing progress for an already admitted durable observation.
/// A ticket terminal/unknown or stage race causes a durable-key re-read; it
/// never supplies a publication or cancellation outcome by itself.
/// # Errors
/// Propagates original-scope cancellation, transport and exact identity failures.
#[cfg(any(unix, windows))]
pub fn observe_index_operation_progress(
    observation: IndexOperationObservation,
    context: &RequestContext,
    mut observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
) -> Result<IndexOperationProgress, ClientError> {
    observe_profile_progress_from(observation, context, &mut observe, None)
}

#[cfg(any(unix, windows))]
fn observe_profile_progress_from(
    observation: IndexOperationObservation,
    context: &RequestContext,
    mut observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    previous: Option<&IndexProgressPage>,
) -> Result<IndexOperationProgress, ClientError> {
    SurfaceReply::IndexOperationStatus(observation.clone())
        .admit(backend_library::CommandId::IndexProgress)
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let IndexOperationObservation::Known(status) = &observation else {
        return Ok(IndexOperationProgress {
            observation,
            profile_progress: None,
        });
    };
    let IndexOperationState::Active { ticket, stage } = &status.state else {
        return Ok(IndexOperationProgress {
            observation,
            profile_progress: None,
        });
    };
    context.read_deadline(crate::CLIENT_REQUEST_TIMEOUT)?;
    let previous = previous.filter(|page| page.ticket == *ticket && page.stage == *stage);
    let mut after_sequence = previous.map_or(0, |page| page.next_sequence);
    let mut pages_read = 0;
    loop {
        pages_read += 1;
        context.check()?;
        let reply = observe(SurfaceCommand::IndexProgress {
            ticket: ticket.clone(),
            after_sequence,
        });
        context.check()?;
        let reply = reply?;
        // Existing admission plus the requested cursor/ticket join must both hold.
        let query = IndexProgressRead::new(
            SurfaceCommand::IndexProgress {
                ticket: ticket.clone(),
                after_sequence,
            },
            None,
            0,
        )?;
        let _ = query.unchanged(&reply)?;
        match reply {
            SurfaceReply::IndexProgress(IndexJobObservation::Pending(page))
                if page.stage == *stage =>
            {
                if (page.events.is_empty() && page.next_sequence != after_sequence)
                    || page.next_sequence < after_sequence
                    || page
                        .events
                        .first()
                        .is_some_and(|event| event.sequence <= after_sequence)
                    || (page.has_more && page.next_sequence <= after_sequence)
                {
                    return Err(ClientError::Protocol(
                        "compiler progress cursor did not advance".to_owned(),
                    ));
                }
                IndexObservationFingerprint::active_with_profile_progress(status, Some(&page))?;
                if page.events.is_empty() && !page.truncated && !page.has_more {
                    if let Some(previous) = previous {
                        IndexObservationFingerprint::active_with_profile_progress(
                            status,
                            Some(previous),
                        )?;
                        return Ok(IndexOperationProgress {
                            observation,
                            profile_progress: Some(previous.clone()),
                        });
                    }
                }
                // A refusal is useful public data, not a disposable catch-up
                // milestone. Return it with its exact continuation instead of
                // draining later pages and silently replacing the cause.
                let has_refusal = page.events.iter().any(|event| {
                    matches!(
                        &event.kind,
                        backend_library::IndexJobProgressKind::ProfileRefused { .. }
                    )
                });
                if page.has_more && !has_refusal && pages_read < MAX_PROFILE_PROGRESS_PAGES {
                    after_sequence = page.next_sequence;
                    continue;
                }
                return Ok(IndexOperationProgress {
                    observation,
                    profile_progress: Some(page),
                });
            }
            SurfaceReply::IndexProgress(_) => {
                // A separate ticket read can race owner retirement or stage advance.
                // Reconcile only the original key, under the same cancellation scope.
                context.check()?;
                let reply = observe(SurfaceCommand::IndexOperationStatus {
                    operation_key: status.operation_key,
                });
                context.check()?;
                let reply = reply?;
                IndexProgressRead::new(
                    SurfaceCommand::IndexOperationStatus {
                        operation_key: status.operation_key,
                    },
                    None,
                    0,
                )?
                .unchanged(&reply)?;
                let SurfaceReply::IndexOperationStatus(incoming) = reply else {
                    return Err(ClientError::Protocol(
                        "operation progress changed reply shape".to_owned(),
                    ));
                };
                if let IndexOperationObservation::Known(next) = &incoming {
                    if next.package != status.package
                        || next.execution_intent != status.execution_intent
                        || next.request_digest != status.request_digest
                    {
                        return Err(ClientError::Protocol(
                            "operation progress changed request identity".to_owned(),
                        ));
                    }
                }
                if let IndexOperationObservation::OutsideReceiptWindow { request_digest, .. } =
                    &incoming
                {
                    if request_digest != &status.request_digest {
                        return Err(ClientError::Protocol(
                            "operation progress changed request digest".to_owned(),
                        ));
                    }
                }
                return Ok(IndexOperationProgress {
                    observation: incoming,
                    profile_progress: None,
                });
            }
            _ => {
                return Err(ClientError::Protocol(
                    "compiler progress changed reply shape".to_owned(),
                ));
            }
        }
    }
}

/// Conditional operation reads including existing exact-ticket compiler milestones.
/// Uses the original request context and the same 500ms probe cadence as the coarse
/// reader. The durable key is retained in every continuation and terminal result.
/// # Errors
/// Refuses non-operation commands and propagates cancellation/admission failures.
#[cfg(any(unix, windows))]
pub fn read_index_operation_progress(
    query: &IndexProgressRead,
    context: &RequestContext,
    observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
) -> Result<IndexCompilerProgressUpdate, ClientError> {
    context.read_deadline(crate::CLIENT_REQUEST_TIMEOUT)?;
    read_operation_with_clock(query, context, observe, &mut WallClock(Instant::now()))
}

#[cfg(any(unix, windows))]
pub(super) fn read_operation_with_clock(
    query: &IndexProgressRead,
    context: &RequestContext,
    mut observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    clock: &mut impl ProgressClock,
) -> Result<IndexCompilerProgressUpdate, ClientError> {
    if !matches!(query.command, SurfaceCommand::IndexOperationStatus { .. }) {
        return Err(ClientError::Protocol(
            "composed progress requires a durable operation key".to_owned(),
        ));
    }
    let started = clock.elapsed();
    let mut previous = None;
    loop {
        context.check()?;
        let reply = observe(query.command.clone());
        context.check()?;
        let reply = reply?;
        let _ = query.unchanged(&reply)?; // existing shape, key and complete receipt admission
        let SurfaceReply::IndexOperationStatus(observation) = reply else {
            return Err(ClientError::Protocol(
                "operation progress changed reply shape".to_owned(),
            ));
        };
        let composed =
            observe_profile_progress_from(observation, context, &mut observe, previous.as_ref())?;
        previous = composed.profile_progress.clone();
        let reference = match &composed.observation {
            IndexOperationObservation::Known(status) => {
                match (
                    &status.state,
                    query.after_observation,
                    IndexObservationFingerprint::active_with_profile_progress(
                        status,
                        composed.profile_progress.as_ref(),
                    )?,
                ) {
                    (IndexOperationState::Active { ticket, .. }, Some(after), Some(current))
                        if current == after =>
                    {
                        Some(IndexProgressReference::Operation {
                            operation_key: status.operation_key,
                            ticket: ticket.clone(),
                            after_observation: after,
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(reference) = reference else {
            return Ok(IndexCompilerProgressUpdate::Changed(composed));
        };
        let remaining = query
            .wait
            .saturating_sub(clock.elapsed().saturating_sub(started));
        if remaining.is_zero() {
            return Ok(IndexCompilerProgressUpdate::Unchanged(reference));
        }
        clock.wait(remaining.min(PROBE_INTERVAL), context)?;
    }
}

#[cfg(all(test, any(unix, windows)))]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "admitted owner progress fixtures"
)]
mod tests {
    use super::*;
    use backend_library::{
        IndexJobProgressEvent, IndexJobProgressKind, IndexJobStage, SemanticLanguageProfile,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    struct Clock(Rc<Cell<Duration>>);
    impl ProgressClock for Clock {
        fn elapsed(&self) -> Duration {
            self.0.get()
        }
        fn wait(&mut self, gap: Duration, context: &RequestContext) -> Result<(), ClientError> {
            context.check()?;
            self.0.set(self.0.get() + gap);
            Ok(())
        }
    }
    fn status() -> IndexOperationStatus {
        let capture: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/go05-progress-waveform.json"))
                .expect("retained waveform");
        let reply: SurfaceReply = serde_json::from_value(capture["runs"][1]["reply"].clone())
            .expect("admitted active receipt");
        let SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(mut status)) =
            reply
        else {
            panic!("active receipt")
        };
        let IndexOperationState::Active { stage, .. } = &mut status.state else {
            panic!("active ticket")
        };
        *stage = IndexJobStage::Compiling;
        status
    }
    fn page(status: &IndexOperationStatus, sequence: u64, admitted: bool) -> IndexProgressPage {
        let IndexOperationState::Active { ticket, stage } = &status.state else {
            panic!("active")
        };
        let profile = SemanticLanguageProfile::from_name("typescript").expect("native profile");
        let kind = if admitted {
            IndexJobProgressKind::ProfileAdmitted {
                profile,
                ordinal: 1,
                total: 3,
            }
        } else {
            IndexJobProgressKind::ProfileStarted {
                profile,
                ordinal: 1,
                total: 3,
            }
        };
        IndexProgressPage {
            ticket: ticket.clone(),
            stage: *stage,
            events: vec![IndexJobProgressEvent {
                ticket: ticket.clone(),
                sequence,
                kind,
            }]
            .into_boxed_slice(),
            next_sequence: sequence,
            truncated: false,
            has_more: false,
        }
    }
    fn keyed(status: &IndexOperationStatus) -> SurfaceReply {
        SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status.clone()))
    }
    fn query(
        status: &IndexOperationStatus,
        page: &IndexProgressPage,
        wait: u64,
    ) -> IndexProgressRead {
        IndexProgressRead::new(
            SurfaceCommand::IndexOperationStatus {
                operation_key: status.operation_key,
            },
            IndexObservationFingerprint::active_with_profile_progress(status, Some(page))
                .expect("joined fingerprint"),
            wait,
        )
        .expect("bounded operation read")
    }
    fn empty_after(mut page: IndexProgressPage, after: u64) -> SurfaceReply {
        page.events = Box::new([]);
        page.next_sequence = after;
        page.has_more = false;
        SurfaceReply::IndexProgress(IndexJobObservation::Pending(page))
    }

    #[test]
    fn unchanged_compiler_milestone_uses_the_full_wait_and_incremental_cursor() {
        let status = status();
        let page = page(&status, 3, false);
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut operation_reads = 0;
        let mut ticket_reads = 0;
        let result = read_operation_with_clock(
            &query(&status, &page, 10000),
            &RequestContext::new(),
            |command| match command {
                SurfaceCommand::IndexOperationStatus { operation_key } => {
                    assert_eq!(operation_key, status.operation_key);
                    operation_reads += 1;
                    Ok(keyed(&status))
                }
                SurfaceCommand::IndexProgress {
                    ticket,
                    after_sequence,
                } => {
                    assert_eq!(ticket, page.ticket);
                    ticket_reads += 1;
                    if ticket_reads == 1 {
                        assert_eq!(after_sequence, 0);
                        Ok(SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                            page.clone(),
                        )))
                    } else {
                        assert_eq!(after_sequence, 3);
                        Ok(empty_after(page.clone(), after_sequence))
                    }
                }
                _ => panic!("no mutations, retries or ticket cancellation"),
            },
            &mut Clock(now.clone()),
        )
        .expect("bounded wait");
        assert!(
            matches!(result, IndexCompilerProgressUpdate::Unchanged(IndexProgressReference::Operation { operation_key, .. }) if operation_key == status.operation_key)
        );
        assert_eq!(now.get(), Duration::from_secs(10));
        assert_eq!(operation_reads, 21);
        assert_eq!(ticket_reads, 21);
    }

    #[test]
    fn profile_only_advancement_changes_the_fingerprint_before_stage_or_publication() {
        let status = status();
        let before = page(&status, 3, false);
        let after = page(&status, 4, true);
        let now = Rc::new(Cell::new(Duration::ZERO));
        let result = read_operation_with_clock(
            &query(&status, &before, 10000),
            &RequestContext::new(),
            |command| match command {
                SurfaceCommand::IndexOperationStatus { .. } => Ok(keyed(&status)),
                SurfaceCommand::IndexProgress { .. } => Ok(SurfaceReply::IndexProgress(
                    IndexJobObservation::Pending(after.clone()),
                )),
                _ => panic!("read-only"),
            },
            &mut Clock(now.clone()),
        )
        .expect("profile advancement");
        let IndexCompilerProgressUpdate::Changed(observed) = result else {
            panic!("new milestone")
        };
        assert_eq!(
            observed.observation,
            IndexOperationObservation::Known(status)
        );
        assert_eq!(observed.profile_progress, Some(after));
        assert_eq!(now.get(), Duration::ZERO);
    }

    #[test]
    fn publication_pending_wakes_conditional_progress_without_a_terminal() {
        let mut status = status();
        let IndexOperationState::Active { stage, .. } = &mut status.state else {
            panic!("actual active fixture")
        };
        *stage = IndexJobStage::Publishing;
        let mut before = page(&status, 3, false);
        before.events[0].kind = IndexJobProgressKind::StageChanged {
            stage: IndexJobStage::Publishing,
        };
        let mut after = page(&status, 4, false);
        after.events[0].kind = IndexJobProgressKind::PublicationPending {
            detail: backend_library::ProductText::from_static(
                "view journal snapshot exceeds record bound",
            ),
            detail_truncated: false,
        };
        let now = Rc::new(Cell::new(Duration::ZERO));
        let result = read_operation_with_clock(
            &query(&status, &before, 10000),
            &RequestContext::new(),
            |command| match command {
                SurfaceCommand::IndexOperationStatus { .. } => Ok(keyed(&status)),
                SurfaceCommand::IndexProgress { .. } => Ok(SurfaceReply::IndexProgress(
                    IndexJobObservation::Pending(after.clone()),
                )),
                _ => panic!("read-only pending observation"),
            },
            &mut Clock(now.clone()),
        )
        .expect("publication observation advances conditional progress");
        let IndexCompilerProgressUpdate::Changed(observed) = result else {
            panic!("actual publication error must change the observation")
        };
        assert_eq!(
            observed.observation,
            IndexOperationObservation::Known(status.clone())
        );
        assert_eq!(observed.profile_progress, Some(after.clone()));
        assert_eq!(
            now.get(),
            Duration::ZERO,
            "do not wait out the 10-second poll"
        );
        assert!(matches!(status.state, IndexOperationState::Active { .. }));
        let mut reads = 0;
        let result = super::super::wait_operation_with_profiles(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |command| {
                reads += 1;
                match command {
                    SurfaceCommand::IndexOperationStatus { .. } => Ok(keyed(&status)),
                    SurfaceCommand::IndexProgress { .. } => Ok(SurfaceReply::IndexProgress(
                        IndexJobObservation::Pending(after.clone()),
                    )),
                    _ => panic!("no owner mutation or resubmission"),
                }
            },
            |_, _| {},
            &mut Clock(now.clone()),
            true,
        )
        .expect("caller wait stops on the original retained failure");
        assert!(
            matches!(result, super::super::IndexOperationWaitResult::Stopped {
            observation: IndexOperationObservation::Known(ref actual),
            reason: super::super::IndexOperationWaitStop::PublicationPending { ref detail, detail_truncated: false },
        } if actual == &status && detail.as_str() == "view journal snapshot exceeds record bound")
        );
        assert_eq!(
            reads, 2,
            "one keyed read and one ticket read; no 900-second retry loop"
        );
        assert_eq!(now.get(), Duration::ZERO);
        // A later admitted retry supersedes the retained pending error. The
        // caller waits on this same operation instead of stopping on history.
        let mut retrying = after.clone();
        retrying.events = vec![
            after.events[0].clone(),
            IndexJobProgressEvent {
                ticket: retrying.ticket.clone(),
                sequence: 5,
                kind: IndexJobProgressKind::PublicationRetryAdmitted,
            },
        ]
        .into_boxed_slice();
        retrying.next_sequence = 5;
        let mut retry_reads = 0;
        let retry_clock = Rc::new(Cell::new(Duration::ZERO));
        let result = super::super::wait_operation_with_profiles(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |command| {
                retry_reads += 1;
                match command {
                    SurfaceCommand::IndexOperationStatus { .. } if retry_reads == 1 => {
                        Ok(keyed(&status))
                    }
                    SurfaceCommand::IndexProgress { .. } => Ok(SurfaceReply::IndexProgress(
                        IndexJobObservation::Pending(retrying.clone()),
                    )),
                    SurfaceCommand::IndexOperationStatus { .. } => {
                        Err(ClientError::RemoteDeadlineExceeded)
                    }
                    _ => panic!("no owner mutation or resubmission"),
                }
            },
            |_, _| {},
            &mut Clock(retry_clock.clone()),
            true,
        )
        .expect("wait on accepted original retry");
        assert!(
            matches!(result, super::super::IndexOperationWaitResult::Stopped {
            observation: IndexOperationObservation::Known(ref actual),
            reason: super::super::IndexOperationWaitStop::Client(ClientError::RemoteDeadlineExceeded),
        } if actual == &status)
        );
        assert_eq!(
            retry_reads, 3,
            "retry milestone permits the next ordinary keyed observation"
        );
        assert!(
            retry_clock.get() > Duration::ZERO,
            "the active retry was actually awaited"
        );
        let mut foreign = after.clone();
        foreign.ticket = backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::MIN,
            [99; 16],
            status.package.clone(),
        );
        for event in &mut foreign.events {
            event.ticket = foreign.ticket.clone();
        }
        assert!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&foreign))
                .is_err()
        );
        let mut wrong_stage = after.clone();
        wrong_stage.stage = IndexJobStage::Compiling;
        assert!(
            SurfaceReply::IndexProgress(IndexJobObservation::Pending(wrong_stage))
                .admit(backend_library::CommandId::IndexProgress)
                .is_err()
        );
        after.events[0].kind = IndexJobProgressKind::PublicationPending {
            detail: backend_library::ProductText::new("x".repeat(1025))
                .expect("product text bound"),
            detail_truncated: false,
        };
        assert!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&after))
                .is_err()
        );
    }


    #[test]
    fn fingerprint_is_stable_across_history_and_incremental_page_boundaries() {
        let status = status();
        let mut history = page(&status, 4, true);
        let incremental = history.clone();
        history.events = vec![
            page(&status, 3, false).events[0].clone(),
            history.events[0].clone(),
        ]
        .into_boxed_slice();
        history.truncated = true;
        assert_eq!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&history))
                .expect("history"),
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&incremental))
                .expect("incremental")
        );
        let mut foreign = incremental.clone();
        foreign.ticket = backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::MIN,
            [9; 16],
            status.package.clone(),
        );
        for event in &mut foreign.events {
            event.ticket = foreign.ticket.clone();
        }
        assert!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&foreign))
                .is_err()
        );
    }

    #[test]
    fn ticket_terminal_race_reconciles_durable_cancellation_without_publication_inference() {
        let status = status();
        let page = page(&status, 3, false);
        let mut terminal = status.clone();
        terminal.state = IndexOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Cancelled,
            detail: backend_library::ProductText::from_static("cancelled"),
            compiler_failure: None,
        };
        // A terminal durable receipt cannot retain Pending source outcomes.
        terminal.source_capture = None;
        let mut keys = Vec::new();
        let observed = observe_index_operation_progress(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |command| match command {
                SurfaceCommand::IndexProgress { ticket, .. } => Ok(SurfaceReply::IndexProgress(
                    IndexJobObservation::Terminal(backend_library::IndexJobTerminal {
                        ticket,
                        outcome: backend_library::IndexJobOutcome::Cancelled,
                    }),
                )),
                SurfaceCommand::IndexOperationStatus { operation_key } => {
                    keys.push(operation_key);
                    Ok(keyed(&terminal))
                }
                _ => panic!("no replacement index or cancel command"),
            },
        )
        .expect("durable reconciliation");
        assert_eq!(keys, vec![status.operation_key]);
        assert!(observed.profile_progress.is_none());
        assert_eq!(
            observed.observation,
            IndexOperationObservation::Known(terminal)
        );
        assert_eq!(page.next_sequence, 3);
    }

    #[test]
    fn paginated_reads_are_bounded_and_preserve_the_last_actual_page() {
        let status = status();
        let template = page(&status, 1, false);
        let mut reads = 0;
        let observed = observe_index_operation_progress(
            IndexOperationObservation::Known(status),
            &RequestContext::new(),
            |command| {
                let SurfaceCommand::IndexProgress { after_sequence, .. } = command else {
                    panic!("ticket page only")
                };
                reads += 1;
                let mut page = template.clone();
                page.events = (1..=backend_library::MAX_INDEX_PROGRESS_EVENTS)
                    .map(|offset| {
                        let mut event = template.events[0].clone();
                        event.sequence = after_sequence + offset as u64;
                        event
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                page.next_sequence = page.events.last().expect("full page").sequence;
                page.has_more = true;
                Ok(SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                    page,
                )))
            },
        )
        .expect("bounded partial history");
        let page = observed.profile_progress.expect("last real page");
        assert_eq!(reads, MAX_PROFILE_PROGRESS_PAGES);
        assert_eq!(
            page.events.len(),
            backend_library::MAX_INDEX_PROGRESS_EVENTS
        );
        assert_eq!(page.next_sequence, 256);
        assert!(page.has_more);
        assert!(!page.truncated);
    }

    #[test]
    fn pending_at_bounded_history_page_does_not_hide_a_later_retry() {
        let mut status = status();
        let IndexOperationState::Active { stage, .. } = &mut status.state else {
            panic!("actual active fixture")
        };
        *stage = IndexJobStage::Publishing;
        let template = page(&status, 1, false);
        let mut status_reads = 0;
        let mut ticket_reads = 0;
        let mut retry_seen = false;
        let clock = Rc::new(Cell::new(Duration::ZERO));
        let context = RequestContext::new();
        let original_deadline = context
            .read_deadline(Duration::from_secs(2))
            .expect("install one original wait deadline");
        let result = super::super::wait_operation_with_profiles(
            IndexOperationObservation::Known(status.clone()),
            &context,
            |command| match command {
                SurfaceCommand::IndexOperationStatus { .. } => {
                    status_reads += 1;
                    if status_reads >= 3 {
                        return Err(ClientError::RemoteDeadlineExceeded);
                    }
                    Ok(keyed(&status))
                }
                SurfaceCommand::IndexProgress { after_sequence, .. } => {
                    ticket_reads += 1;
                    let mut page = template.clone();
                    if after_sequence == (MAX_PROFILE_PROGRESS_PAGES
                        * backend_library::MAX_INDEX_PROGRESS_EVENTS) as u64
                    {
                        retry_seen = true;
                        let mut event = template.events[0].clone();
                        event.sequence = after_sequence + 1;
                        event.kind = IndexJobProgressKind::PublicationRetryAdmitted;
                        page.events = vec![event].into_boxed_slice();
                        page.next_sequence = after_sequence + 1;
                        page.has_more = false;
                    } else {
                        page.events = (1..=backend_library::MAX_INDEX_PROGRESS_EVENTS)
                            .map(|offset| {
                                let mut event = template.events[0].clone();
                                event.sequence = after_sequence + offset as u64;
                                event.kind = IndexJobProgressKind::StageChanged {
                                    stage: IndexJobStage::Publishing,
                                };
                                event
                            })
                            .collect::<Vec<_>>()
                            .into_boxed_slice();
                        if ticket_reads == MAX_PROFILE_PROGRESS_PAGES {
                            let event = page.events.last_mut().expect("bounded page has events");
                            event.kind = IndexJobProgressKind::PublicationPending {
                                detail: backend_library::ProductText::from_static(
                                    "view journal snapshot exceeds record bound",
                                ),
                                detail_truncated: false,
                            };
                        }
                        page.next_sequence = page.events.last().expect("full page").sequence;
                        page.has_more = true;
                    }
                    Ok(SurfaceReply::IndexProgress(IndexJobObservation::Pending(page)))
                }
                _ => panic!("read-only exact-operation wait"),
            },
            |_, _| {},
            &mut Clock(clock.clone()),
            true,
        )
        .expect("same operation remains bounded by its original request context");
        assert!(matches!(
            result,
            super::super::IndexOperationWaitResult::Stopped {
                observation: IndexOperationObservation::Known(ref actual),
                reason: super::super::IndexOperationWaitStop::Client(
                    ClientError::RemoteDeadlineExceeded
                ),
            } if actual == &status
        ));
        assert_eq!(ticket_reads, MAX_PROFILE_PROGRESS_PAGES + 1);
        assert!(retry_seen, "the later same-ticket retry page was consumed");
        assert_eq!(status_reads, 3);
        assert_eq!(
            context
                .read_deadline(Duration::from_secs(30))
                .expect("reuse the caller's original wait deadline"),
            original_deadline
        );
    }

    #[test]
    fn nonadvancing_and_empty_advancing_cursors_are_refused() {
        for empty in [false, true] {
            let status = status();
            let template = page(&status, 1, false);
            let mut reads = 0;
            let result = observe_index_operation_progress(
                IndexOperationObservation::Known(status),
                &RequestContext::new(),
                |command| {
                    let SurfaceCommand::IndexProgress { after_sequence, .. } = command else {
                        panic!("ticket only")
                    };
                    reads += 1;
                    if empty {
                        let mut page = template.clone();
                        page.events = Box::new([]);
                        page.next_sequence = 1;
                        return Ok(SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                            page,
                        )));
                    }
                    let mut page = template.clone();
                    page.events = (1..=backend_library::MAX_INDEX_PROGRESS_EVENTS)
                        .map(|offset| {
                            let mut event = template.events[0].clone();
                            event.sequence = offset as u64;
                            event
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice();
                    page.next_sequence = backend_library::MAX_INDEX_PROGRESS_EVENTS as u64;
                    page.has_more = true;
                    assert!(after_sequence == 0 || after_sequence == page.next_sequence);
                    Ok(SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                        page,
                    )))
                },
            );
            assert!(matches!(result, Err(ClientError::Protocol(_))));
            assert!(reads <= 2);
        }
    }

    #[test]
    fn cancellation_and_original_expiration_prevent_further_ticket_or_owner_reads() {
        let status = status();
        let page = page(&status, 3, false);
        for reason in [
            crate::RequestCancellationReason::Explicit,
            crate::RequestCancellationReason::ClientDisconnected,
            crate::RequestCancellationReason::Shutdown,
        ] {
            let context = RequestContext::new();
            let cancel = context.clone();
            let mut reads = 0;
            let result =
                read_index_operation_progress(&query(&status, &page, 10000), &context, |command| {
                    reads += 1;
                    assert!(matches!(
                        command,
                        SurfaceCommand::IndexOperationStatus { .. }
                    ));
                    cancel.cancel(reason);
                    Ok(keyed(&status))
                });
            assert!(
                matches!(result, Err(ClientError::RequestCancelled(actual)) if actual == reason)
            );
            assert_eq!(reads, 1);
        }
        let context = RequestContext::new();
        assert!(context.read_deadline(Duration::ZERO).is_err());
        let result = read_index_operation_progress(&query(&status, &page, 10000), &context, |_| {
            panic!("expired original deadline")
        });
        assert!(matches!(result, Err(ClientError::RemoteDeadlineExceeded)));
    }
    #[test]
    fn wait_reports_profile_advancement_and_retains_the_exact_durable_terminal() {
        let status = status();
        let started = page(&status, 3, false);
        let admitted = page(&status, 4, true);
        let mut terminal = status.clone();
        terminal.source_capture = None;
        terminal.state = IndexOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Cancelled,
            detail: backend_library::ProductText::from_static("cancelled"),
            compiler_failure: None,
        };
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut milestones = Vec::new();
        let mut operation_read_times = Vec::new();
        let mut ticket_read_times = Vec::new();
        let result = super::super::wait_operation_with_profiles(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |command| match command {
                SurfaceCommand::IndexOperationStatus { operation_key } => {
                    assert_eq!(operation_key, status.operation_key);
                    operation_read_times.push(now.get());
                    Ok(keyed(if now.get() >= Duration::from_secs(2) {
                        &terminal
                    } else {
                        &status
                    }))
                }
                SurfaceCommand::IndexProgress {
                    ticket,
                    after_sequence,
                } => {
                    assert_eq!(ticket, started.ticket);
                    ticket_read_times.push(now.get());
                    let current = if now.get() >= Duration::from_secs(1) {
                        &admitted
                    } else {
                        &started
                    };
                    Ok(if after_sequence == current.next_sequence {
                        empty_after(current.clone(), after_sequence)
                    } else {
                        SurfaceReply::IndexProgress(IndexJobObservation::Pending(current.clone()))
                    })
                }
                _ => panic!("no reindex or owner cancellation"),
            },
            |_, page| {
                if let Some(page) = page {
                    milestones.push(page.next_sequence);
                }
            },
            &mut Clock(now.clone()),
            true,
        )
        .expect("same durable wait");
        assert!(milestones.contains(&3));
        assert!(milestones.contains(&4));
        assert!(
            matches!(result, IndexOperationWaitResult::Terminal(actual) if actual == IndexOperationObservation::Known(terminal))
        );
        // The first changed milestone starts a new caller slice after the
        // required minimum read spacing. Its fixed half-second probes first
        // observe the two-second terminal on that shifted grid, at 2.1s.
        let first = super::super::MIN_READ_SPACING;
        let probe = super::super::PROBE_INTERVAL;
        let expected_reads = [
            Duration::ZERO,
            first,
            first + probe,
            first + probe * 2,
            first + probe * 2,
            first + probe * 3,
            first + probe * 4,
        ];
        assert_eq!(operation_read_times, expected_reads);
        assert_eq!(
            ticket_read_times.as_slice(),
            &expected_reads[..expected_reads.len() - 1]
        );
        assert_eq!(
            now.get(),
            *expected_reads.last().expect("terminal read"),
            "an admitted terminal returns without another cadence delay"
        );
        assert!(
            now.get() >= Duration::from_secs(2) && now.get() < Duration::from_secs(2) + probe
        );
    }

    #[test]
    fn cancellation_during_ticket_drain_wins_before_any_reconciliation_read() {
        let status = status();
        let page = page(&status, 3, false);
        let context = RequestContext::new();
        let cancel = context.clone();
        let mut reads = 0;
        let result =
            read_index_operation_progress(&query(&status, &page, 10000), &context, |command| {
                reads += 1;
                match command {
                    SurfaceCommand::IndexOperationStatus { .. } => Ok(keyed(&status)),
                    SurfaceCommand::IndexProgress { .. } => {
                        cancel.cancel(crate::RequestCancellationReason::Explicit);
                        Ok(SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
                            ticket: page.ticket.clone(),
                            current_owner_epoch: [9; 16],
                        }))
                    }
                    _ => panic!("no mutation"),
                }
            });
        assert!(matches!(
            result,
            Err(ClientError::RequestCancelled(
                crate::RequestCancellationReason::Explicit
            ))
        ));
        assert_eq!(reads, 2);
    }

    #[test]
    fn owner_restart_and_stage_races_reconcile_only_the_original_durable_key() {
        for restarted in [false, true] {
            let status = status();
            let mut page = page(&status, 3, false);
            page.stage = IndexJobStage::Publishing;
            let mut next = status.clone();
            if let IndexOperationState::Active { stage, .. } = &mut next.state {
                *stage = IndexJobStage::Publishing;
            }
            let mut reads = 0;
            let observed = observe_index_operation_progress(
                IndexOperationObservation::Known(status.clone()),
                &RequestContext::new(),
                |command| {
                    reads += 1;
                    match command {
                        SurfaceCommand::IndexProgress { ticket, .. } if restarted => {
                            Ok(SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
                                ticket,
                                current_owner_epoch: [9; 16],
                            }))
                        }
                        SurfaceCommand::IndexProgress { .. } => Ok(SurfaceReply::IndexProgress(
                            IndexJobObservation::Pending(page.clone()),
                        )),
                        SurfaceCommand::IndexOperationStatus { operation_key } => {
                            assert_eq!(operation_key, status.operation_key);
                            Ok(if restarted {
                                SurfaceReply::IndexOperationStatus(
                                    IndexOperationObservation::Unknown { operation_key },
                                )
                            } else {
                                keyed(&next)
                            })
                        }
                        _ => panic!("never restart or replace owner work"),
                    }
                },
            )
            .expect("durable reconciliation");
            assert!(observed.profile_progress.is_none());
            assert_eq!(reads, 2);
            assert_eq!(
                observed.observation,
                if restarted {
                    IndexOperationObservation::Unknown {
                        operation_key: status.operation_key,
                    }
                } else {
                    IndexOperationObservation::Known(next)
                }
            );
        }
    }
    #[test]
    fn source_capture_identity_changes_the_commitment_and_rejects_foreign_profiles() {
        use backend_library::{
            IndexOperationSemanticProfileState, IndexOperationSourceCaptureReceipt,
            IndexOperationSourceProfile,
        };
        let mut status = status();
        let page = page(&status, 3, false);
        let make_capture = |profile, byte| {
            IndexOperationSourceCaptureReceipt::from_checked_parts(
                status.operation_key,
                [2; 32],
                [3; 32],
                1,
                vec![IndexOperationSourceProfile {
                    profile,
                    source_version: [byte; 32],
                    input_digest: [4; 32],
                    observation_sequence: 1,
                    source_count: 1,
                    state: IndexOperationSemanticProfileState::Pending { prior: None },
                }]
                .into_boxed_slice(),
            )
            .expect("checked pending source capture")
        };
        let typescript = SemanticLanguageProfile::from_name("typescript").expect("native profile");
        let original = make_capture(typescript, 5);
        let changed = make_capture(typescript, 6);
        let foreign = make_capture(
            SemanticLanguageProfile::from_name("python").expect("Python profile"),
            5,
        );
        status.source_capture = Some(original);
        let before =
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&page))
                .expect("exact source/profile");
        status.source_capture = Some(changed);
        assert_ne!(
            before,
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&page))
                .expect("changed source receipt")
        );
        status.source_capture = Some(foreign);
        assert!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&page))
                .is_err()
        );
    }
    #[test]
    fn already_admitted_durable_terminal_needs_no_new_read_or_deadline() {
        let mut terminal = status();
        terminal.source_capture = None;
        terminal.state = IndexOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Cancelled,
            detail: backend_library::ProductText::from_static("cancelled"),
            compiler_failure: None,
        };
        let context = RequestContext::new();
        assert!(context.read_deadline(Duration::ZERO).is_err());
        let observation = IndexOperationObservation::Known(terminal);
        let result = observe_index_operation_progress(observation.clone(), &context, |_| {
            panic!("terminal receipt requires no ticket read")
        })
        .expect("retain admitted terminal");
        assert_eq!(result.observation, observation);
        assert!(result.profile_progress.is_none());
    }
    fn refused_profile_fixture() -> backend_library::PackageCompilerFailure {
        use backend_library::interface::{
            CompilerAttempt, CompilerFragmentFailure, SourceAuthority,
        };
        use backend_semantic::ir::{BuildError, EntityId};
        backend_library::PackageCompilerFailure::from_fragment_failure(
            "ui/main.ts",
            CompilerAttempt {
                source: SourceAuthority {
                    identity: backend_version::ContentId::<
                        backend_version::SourceFactDomain,
                    >::from_canonical_bytes(b"source bytes"),
                    byte_len: 12,
                },
                recipe: backend_version::ContentId::<
                    backend_version::CompileRecipeDomain,
                >::from_canonical_bytes(b"recipe bytes"),
            },
            &CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
                owner: EntityId::new(7),
                start: 18,
                end: 24,
            }),
        )
        .expect("exact bounded typed refusal fixture")
    }

    #[test]
    fn refused_profile_changes_cursor_and_preserves_exact_failure_without_publication() {
        let status = status();
        let original = page(&status, 4, false);
        let mut refused = original.clone();
        refused.has_more = true;
        let failure = refused_profile_fixture();
        let profile = SemanticLanguageProfile::from_name("typescript").expect("captured profile");
        refused.events[0].kind = IndexJobProgressKind::ProfileRefused {
            profile,
            ordinal: 1,
            total: 3,
            failure: Box::new(failure),
        };
        assert_ne!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&original))
                .expect("started"),
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&refused))
                .expect("refused"),
            "typed refusal is a new observable fact even while the operation remains compiling",
        );
        let mut foreign = refused.clone();
        foreign.ticket = backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::MIN,
            [9; 16],
            status.package.clone(),
        );
        for event in &mut foreign.events {
            event.ticket = foreign.ticket.clone();
        }
        assert!(
            IndexObservationFingerprint::active_with_profile_progress(&status, Some(&foreign))
                .is_err()
        );
        let observed = observe_index_operation_progress(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |command| match command {
                SurfaceCommand::IndexProgress { after_sequence, .. } => {
                    assert_eq!(
                        after_sequence, 0,
                        "do not drain past an undelivered refusal"
                    );
                    Ok(SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                        refused.clone(),
                    )))
                }
                _ => panic!("refusal progress cannot dispatch, cancel or replace the operation"),
            },
        )
        .expect("same admitted compiler progress");
        assert_eq!(
            observed.observation,
            IndexOperationObservation::Known(status)
        );
        assert_eq!(observed.profile_progress, Some(refused));
    }
}
