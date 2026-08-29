//! Durable append capability, canonical records, and allocation-free replay.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "closed wire/error variants and capability types are documented at their public boundaries"
)]

use core::{future::Future, mem::size_of};

use nudox_id::{ContentId, FixedCanonicalRecord, ObjectDomain};
use thiserror::Error;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16},
};

use crate::{
    EventKind, EventName, FailureCode, Recovery, Reduction, ReductionError, StageKey,
    WorkflowEvent, WorkflowState, WorkflowVersion, reduce,
};

/// Canonical width derived from the declared record rather than a parallel offset table.
pub const WORKFLOW_RECORD_BYTES: usize = size_of::<WorkflowRecord>();

/// Fixed canonical event record with explicit typed wire cells.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct WorkflowRecord {
    version: U16<LittleEndian>,
    key: [u8; 32],
    event: u8,
    failure: u8,
    output: [u8; 32],
}

impl From<WorkflowEvent> for WorkflowRecord {
    fn from(event: WorkflowEvent) -> Self {
        let (output, failure) = match event.kind {
            EventKind::Staged { output }
            | EventKind::Verified { output }
            | EventKind::PublicationStarted { output }
            | EventKind::Published { output } => (*output, 0),
            EventKind::Failed { code } => ([0; 32], u8::from(code)),
            EventKind::Requested | EventKind::Admitted | EventKind::Cancelled => ([0; 32], 0),
        };
        Self {
            version: U16::new(*event.version),
            key: *event.key,
            event: u8::from(event.kind.name()),
            failure,
            output,
        }
    }
}

impl FixedCanonicalRecord<WORKFLOW_RECORD_BYTES> for WorkflowRecord {
    fn canonical_bytes(&self) -> &[u8; WORKFLOW_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

/// Exact canonical decode rejection.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WorkflowRecordError {
    #[error("workflow record version {observed} is unknown")]
    UnknownVersion { observed: u16 },
    #[error("workflow record event tag {observed} is unknown")]
    UnknownEvent { observed: u8 },
    #[error("workflow record failure tag {observed} is unknown")]
    UnknownFailure { observed: u8 },
    #[error("event {event:?} carried unexpected output bytes")]
    UnexpectedOutput { event: EventName },
    #[error("event {event:?} carried unexpected failure tag {observed}")]
    UnexpectedFailure { event: EventName, observed: u8 },
}

impl TryFrom<&WorkflowRecord> for WorkflowEvent {
    type Error = WorkflowRecordError;

    fn try_from(record: &WorkflowRecord) -> Result<Self, Self::Error> {
        let version = u16::from(record.version);
        if version != *WorkflowVersion::WAVE1 {
            return Err(WorkflowRecordError::UnknownVersion { observed: version });
        }
        let name = EventName::from_repr(record.event).ok_or(WorkflowRecordError::UnknownEvent {
            observed: record.event,
        })?;
        let output = ContentId::<ObjectDomain>::from(record.output);
        let kind = match name {
            EventName::Requested => no_payload(record, EventKind::Requested, name)?,
            EventName::Admitted => no_payload(record, EventKind::Admitted, name)?,
            EventName::Cancelled => no_payload(record, EventKind::Cancelled, name)?,
            EventName::Staged => output_event(record, EventKind::Staged { output }, name)?,
            EventName::Verified => output_event(record, EventKind::Verified { output }, name)?,
            EventName::PublicationStarted => {
                output_event(record, EventKind::PublicationStarted { output }, name)?
            }
            EventName::Published => output_event(record, EventKind::Published { output }, name)?,
            EventName::Failed => {
                ensure_zero_output(record, name)?;
                let code = FailureCode::from_repr(record.failure).ok_or(
                    WorkflowRecordError::UnknownFailure {
                        observed: record.failure,
                    },
                )?;
                EventKind::Failed { code }
            }
        };
        Ok(Self {
            version: WorkflowVersion::WAVE1,
            key: StageKey::from(record.key),
            kind,
        })
    }
}

fn no_payload(
    record: &WorkflowRecord,
    kind: EventKind,
    name: EventName,
) -> Result<EventKind, WorkflowRecordError> {
    ensure_zero_output(record, name)?;
    ensure_zero_failure(record, name)?;
    Ok(kind)
}

fn output_event(
    record: &WorkflowRecord,
    kind: EventKind,
    name: EventName,
) -> Result<EventKind, WorkflowRecordError> {
    ensure_zero_failure(record, name)?;
    Ok(kind)
}

fn ensure_zero_output(
    record: &WorkflowRecord,
    event: EventName,
) -> Result<(), WorkflowRecordError> {
    if record.output == [0; 32] {
        Ok(())
    } else {
        Err(WorkflowRecordError::UnexpectedOutput { event })
    }
}

const fn ensure_zero_failure(
    record: &WorkflowRecord,
    event: EventName,
) -> Result<(), WorkflowRecordError> {
    if record.failure == 0 {
        Ok(())
    } else {
        Err(WorkflowRecordError::UnexpectedFailure {
            event,
            observed: record.failure,
        })
    }
}

/// Shared async sink whose concrete receipt follows stable append commitment.
///
/// The shared receiver permits multiple append futures to coexist. Implementations that support
/// cross-thread producers additionally expose `Sync`; local implementations need not.
pub trait DurableAppend {
    type Error;
    type Receipt;
    type Append<'append>: Future<Output = Result<Self::Receipt, Self::Error>>
    where
        Self: 'append;

    fn append(&self, record: WorkflowRecord) -> Self::Append<'_>;
}

/// Reduction and the durable adapter's concrete receipt, exposed as honest data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedReduction<Receipt> {
    pub reduction: Reduction,
    pub receipt: Receipt,
}

/// Failure before any external effect is eligible for execution.
#[derive(Debug, Error)]
pub enum DurableCommitError<AppendError> {
    #[error("workflow reduction rejected the event")]
    Reduction(#[source] ReductionError),
    #[error("durable workflow append failed")]
    Append(#[source] AppendError),
}

/// Reduces, canonically encodes, and awaits durable commit before releasing the effect.
pub async fn append_committed<Journal>(
    state: WorkflowState,
    event: WorkflowEvent,
    journal: &Journal,
) -> Result<CommittedReduction<Journal::Receipt>, DurableCommitError<Journal::Error>>
where
    Journal: DurableAppend,
{
    let reduction = reduce(state, event).map_err(DurableCommitError::Reduction)?;
    let receipt = journal
        .append(WorkflowRecord::from(event))
        .await
        .map_err(DurableCommitError::Append)?;
    Ok(CommittedReduction { reduction, receipt })
}

/// Exact fallible canonical-record replay error without source erasure.
#[derive(Debug, Error)]
pub enum ReplayError<SourceError> {
    #[error("committed record stream failed")]
    Source(#[source] SourceError),
    #[error("committed record was not canonical")]
    Decode(#[source] WorkflowRecordError),
    #[error("committed event violated workflow reduction")]
    Reduction(#[source] ReductionError),
}

/// Decodes and replays a fallible committed-record stream without retaining its records.
pub fn replay_stream<Records, SourceError>(
    records: Records,
) -> Result<Recovery, ReplayError<SourceError>>
where
    Records: IntoIterator<Item = Result<WorkflowRecord, SourceError>>,
{
    let mut state = WorkflowState::empty();
    for record in records {
        let event = WorkflowEvent::try_from(&record.map_err(ReplayError::Source)?)
            .map_err(ReplayError::Decode)?;
        state = reduce(state, event).map_err(ReplayError::Reduction)?.state;
    }
    Ok(Recovery::from_state(state))
}

#[cfg(test)]
mod tests {
    use crate::{EventKind, WorkflowEvent, WorkflowVersion, tests::key};
    use nudox_id::{ContentId, FixedCanonicalRecord, ObjectDomain};

    use super::{WORKFLOW_RECORD_BYTES, WorkflowRecord, WorkflowRecordError};

    #[test]
    fn canonical_record_round_trips_and_rejects_mutated_tags() {
        let event = WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key: key(),
            kind: EventKind::Published {
                output: ContentId::<ObjectDomain>::from([7; 32]),
            },
        };
        let record = WorkflowRecord::from(event);
        assert_eq!(record.canonical_bytes().len(), WORKFLOW_RECORD_BYTES);
        assert_eq!(WorkflowEvent::try_from(&record), Ok(event));

        let mut mutated = record;
        mutated.event = u8::MAX;
        assert_eq!(
            WorkflowEvent::try_from(&mutated),
            Err(WorkflowRecordError::UnknownEvent { observed: u8::MAX })
        );
    }
}
