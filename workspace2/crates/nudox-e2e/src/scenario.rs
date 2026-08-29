#![allow(
    missing_docs,
    clippy::result_large_err,
    reason = "the scenario retains descriptor-rich typed sources directly without heap allocation"
)]

use core::convert::Infallible;

use nudox_frame::{SectionInput, encode_into, encoded_len};
use nudox_hydration::{
    AbsentCount, HydrationProbeEvent, Need, PlanScratch, Projection, ReadyGeneration,
    VerificationError, plan_with_probe,
};
use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef, ProviderId, ProviderSet};
use nudox_observe::Probe;
use nudox_operation::{
    BatchSource, LocalObjectProvider, PinnedObjectRequest, Provider, SourcePoll, TerminalSummary,
};
use nudox_root::{
    ClosureScratch, EntryKey, GenerationRoot, GenerationView, LocalityException, NonResident,
    PreparedLocality, RootEntry, RootProbeEvent,
};
use nudox_runtime::{
    ByteBudget, ByteQuantum, RemoteRuntime, RetainedBytes, RuntimeMetrics, RuntimeProbeEvent,
    TerminalClass, TerminalOutcome,
};
use nudox_schema::{OperationId, RowCount, SchemaId, SectionKind};
use nudox_store_memory::{
    ByteCapacity, InsertOutcome, MemoryStore, SlotCapacity, StoreCapacity, StoreProbeEvent,
};
use nudox_view::ValidatedFrame;
use nudox_workflow::{
    CapabilityDomain, CommitError, ConfigurationDomain, Effect, EventKind, LogConfigError,
    MemoryWorkflowLog, Phase, PhaseName, ReductionError, StageId, StageInput, StageKey,
    WorkflowEvent, WorkflowProbeEvent, WorkflowState, WorkflowVersion,
};
use thiserror::Error;

/// Observer accepted by the one shared scenario driver.
pub trait ScenarioProbe:
    Probe<RootProbeEvent>
    + Probe<HydrationProbeEvent>
    + Probe<StoreProbeEvent>
    + Probe<RuntimeProbeEvent>
    + Probe<WorkflowProbeEvent>
{
}

impl<Observation> ScenarioProbe for Observation where
    Observation: Probe<RootProbeEvent>
        + Probe<HydrationProbeEvent>
        + Probe<StoreProbeEvent>
        + Probe<RuntimeProbeEvent>
        + Probe<WorkflowProbeEvent>
{
}

/// Stable semantics compared across disabled, flight-recorder, and tracing modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScenarioEvidence {
    pub promised_objects: AbsentCount,
    pub emitted_objects: RowCount,
    pub workflow_phase: PhaseName,
    pub runtime_terminal: TerminalClass,
    pub runtime: RuntimeMetrics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScratchStep {
    RootSelection,
    PromisedPlanning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertStep {
    FirstWrite,
    DuplicateWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStep {
    ResidentBatch,
    ResidentTerminal,
    ResidentFinished,
    PromisedTerminal,
    PromisedFinished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollClass {
    Batch,
    Complete,
    Partial,
    Cancelled,
    Finished,
}

/// Exact scenario failure preserving every cross-crate source family.
#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("integer conversion failed")]
    Integer(#[from] core::num::TryFromIntError),
    #[error("schema limit construction failed")]
    Limit(#[from] nudox_schema::LimitError),
    #[error("root construction failed")]
    Root(#[from] nudox_root::RootBuildError),
    #[error("locality validation failed")]
    Locality(#[from] nudox_root::LocalityError),
    #[error("locality write failed")]
    LocalityWrite(#[from] nudox_root::LocalityWriteError),
    #[error("locality read failed")]
    LocalityRead(#[from] nudox_root::LocalityReadError),
    #[error("closure selection failed")]
    Closure(#[from] nudox_root::ClosureError),
    #[error("provider construction failed")]
    Provider(#[from] nudox_object::ProviderIdError),
    #[error("planning demand failed")]
    Demand(#[from] nudox_hydration::DemandBindError),
    #[error("planning failed")]
    Plan(#[from] nudox_hydration::PlanError),
    #[error("verification failed")]
    Verification(#[from] VerificationError<ObjectDomain>),
    #[error("{step:?} scratch reservation failed")]
    Scratch {
        step: ScratchStep,
        #[source]
        source: std::collections::TryReserveError,
    },
    #[error("frame encoding failed")]
    Encode(#[from] nudox_frame::EncodeError),
    #[error("frame validation failed")]
    Validate(#[from] nudox_view::ValidateError),
    #[error("store initialization failed")]
    StoreInit(#[from] nudox_store_memory::StoreInitError),
    #[error("{step:?} store insert rejected {reference:?}")]
    StoreRejected {
        step: InsertStep,
        reference: ObjectRef<ObjectDomain>,
        #[source]
        source: nudox_store_memory::StoreError<ObjectDomain>,
        bytes: Box<[u8]>,
    },
    #[error("object operation failed")]
    Operation(#[from] nudox_operation::LocalObjectError<ObjectDomain>),
    #[error("runtime configuration failed")]
    RuntimeConfig(#[from] nudox_runtime::RuntimeConfigError),
    #[error("runtime byte budget failed")]
    ByteBudget(#[from] nudox_runtime::ByteBudgetError),
    #[error("runtime admission failed")]
    Admission(#[from] nudox_runtime::AdmissionError<RetainedBytes>),
    #[error("runtime owner failed")]
    Owner(#[from] nudox_runtime::OwnerFault),
    #[error("workflow log configuration failed")]
    LogConfig(#[from] LogConfigError),
    #[error("workflow commit failed")]
    Commit(#[from] CommitError),
    #[error("workflow replay failed")]
    Replay(#[from] ReductionError),
    #[error("root row {key:?} was absent")]
    MissingRootRow { key: EntryKey },
    #[error("root selection expected {expected:?}, observed {observed:?}")]
    SelectionCount {
        expected: nudox_root::SelectedCount,
        observed: nudox_root::SelectedCount,
    },
    #[error("{step:?} expected {expected:?}, observed {observed:?}")]
    InsertOutcome {
        step: InsertStep,
        expected: InsertOutcome,
        observed: InsertOutcome,
    },
    #[error("promised verification unexpectedly succeeded for root {root:?} and object {object:?}")]
    ExpectedMissingVerification {
        root: GenerationId,
        object: ObjectRef<ObjectDomain>,
    },
    #[error("frame expected {expected} section, observed {observed}")]
    SectionCount { expected: usize, observed: usize },
    #[error("{step:?} could not compose local provider for {key:?}")]
    MissingOperationProvider { step: OperationStep, key: EntryKey },
    #[error("{step:?} expected batch object {expected:?}, observed {observed:?}")]
    BatchObject {
        step: OperationStep,
        expected: ObjectRef<ObjectDomain>,
        observed: ObjectRef<ObjectDomain>,
    },
    #[error("{step:?} expected {expected} batch item, observed {observed}")]
    BatchCount {
        step: OperationStep,
        expected: usize,
        observed: usize,
    },
    #[error("{step:?} expected {expected:?}, observed {observed:?}")]
    PollClass {
        step: OperationStep,
        expected: PollClass,
        observed: PollClass,
    },
    #[error("{step:?} expected terminal {expected:?}, observed {observed:?}")]
    TerminalSummary {
        step: OperationStep,
        expected: TerminalSummary<nudox_operation::MissingObject<ObjectDomain>>,
        observed: TerminalSummary<nudox_operation::MissingObject<ObjectDomain>>,
    },
    #[error("runtime expected work-slot rejection, observed {observed:?}")]
    RuntimeRejection {
        observed: nudox_runtime::RejectionReason,
    },
    #[error("runtime second admission unexpectedly returned {handle:?}")]
    RuntimeUnexpectedAdmission { handle: nudox_runtime::WorkHandle },
    #[error("runtime expected terminalization, observed {observed:?}")]
    OwnerProgress {
        observed: nudox_runtime::OwnerProgress,
    },
    #[error("runtime terminal lane was empty after terminalization")]
    MissingRuntimeTerminal,
    #[error("runtime expected completed terminal, observed {observed:?}")]
    RuntimeTerminal { observed: TerminalClass },
    #[error("workflow replay state/effect differed from the exact published recovery")]
    Recovery {
        expected_state: WorkflowState,
        observed_state: WorkflowState,
        expected_effect: Option<Effect>,
        observed_effect: Option<Effect>,
    },
}

impl From<Infallible> for ScenarioError {
    fn from(error: Infallible) -> Self {
        match error {}
    }
}

/// Runs the exact same public scenario under any portable or server-side observer.
///
/// # Errors
///
/// Returns the exact typed source from the first cross-crate boundary that rejects the scenario.
pub fn run_wave1_scenario(
    probe: &mut impl ScenarioProbe,
) -> Result<ScenarioEvidence, ScenarioError> {
    let bytes = b"wave";
    let object = descriptor(bytes)?;
    let root = generation_root(object)?;
    let providers = ProviderSet::only(ProviderId::try_from(1)?);
    let row = root
        .locality_row(EntryKey::from(1))
        .ok_or(ScenarioError::MissingRootRow {
            key: EntryKey::from(1),
        })?;
    let promised_fact = [LocalityException::new(
        row,
        NonResident::Promised(providers),
    )];
    let mut promised_bytes = locality_bytes(&root, &promised_fact)?;
    let promised = PreparedLocality::prepare(&root, &promised_fact)?.write(&mut promised_bytes)?;
    let promised_view = GenerationView::new(&root, &promised)?;

    observe_root_selection(&promised_view, probe)?;
    let promised_objects = verify_promised_plan(&promised_view, object, probe)?;
    validate_frame(bytes)?;

    let mut store = MemoryStore::new(StoreCapacity {
        bytes: ByteCapacity::from(4),
        slots: SlotCapacity::from(1),
    })?;
    assert_insert(
        &mut store,
        object,
        bytes,
        probe,
        InsertStep::FirstWrite,
        InsertOutcome::Inserted,
    )?;
    assert_insert(
        &mut store,
        object,
        bytes,
        probe,
        InsertStep::DuplicateWrite,
        InsertOutcome::AlreadyPresent,
    )?;

    let mut resident_bytes = locality_bytes(&root, &[])?;
    let resident = PreparedLocality::prepare(&root, &[])?.write(&mut resident_bytes)?;
    let resident_view = GenerationView::new(&root, &resident)?;
    let ready = verify_ready_plan(&resident_view, &mut store, probe)?;
    let emitted_objects = run_resident_operation(&resident_view, &ready, object)?;
    let (runtime_terminal, runtime) = run_runtime(ready.pinned_root, probe)?;
    let workflow_phase = run_workflow(ready.pinned_root, object, probe)?;
    run_promised_operation(&promised_view, object, providers)?;

    Ok(ScenarioEvidence {
        promised_objects,
        emitted_objects,
        workflow_phase,
        runtime_terminal,
        runtime,
    })
}

fn assert_insert(
    store: &mut MemoryStore<ObjectDomain>,
    object: ObjectRef<ObjectDomain>,
    bytes: &[u8],
    probe: &mut impl ScenarioProbe,
    step: InsertStep,
    expected: InsertOutcome,
) -> Result<(), ScenarioError> {
    let observed = store
        .insert_owned_with_probe(object, bytes.to_vec().into_boxed_slice(), probe)
        .map_err(|rejected| ScenarioError::StoreRejected {
            step,
            reference: rejected.reference,
            source: rejected.error,
            bytes: rejected.bytes,
        })?;
    if observed != expected {
        return Err(ScenarioError::InsertOutcome {
            step,
            expected,
            observed,
        });
    }
    Ok(())
}

fn descriptor(bytes: &[u8]) -> Result<ObjectRef<ObjectDomain>, ScenarioError> {
    Ok(ObjectRef {
        content: ContentId::from_canonical_bytes(bytes),
        schema: SchemaId::Object,
        length: ObjectLength::from(u64::try_from(bytes.len())?),
        kind: ObjectKind::from(1),
    })
}

fn generation_root(
    object: ObjectRef<ObjectDomain>,
) -> Result<GenerationRoot<ObjectDomain>, ScenarioError> {
    Ok(GenerationRoot::new(vec![RootEntry {
        key: EntryKey::from(1),
        parent: None,
        object,
    }])?)
}

fn locality_bytes(
    root: &GenerationRoot<ObjectDomain>,
    facts: &[LocalityException<ObjectDomain>],
) -> Result<Vec<u8>, ScenarioError> {
    Ok(vec![
        0;
        usize::from(
            PreparedLocality::prepare(root, facts)?.required_bytes
        )
    ])
}

fn observe_root_selection(
    view: &GenerationView<'_, '_, ObjectDomain>,
    probe: &mut impl ScenarioProbe,
) -> Result<(), ScenarioError> {
    let mut scratch = ClosureScratch::new(1).map_err(|source| ScenarioError::Scratch {
        step: ScratchStep::RootSelection,
        source,
    })?;
    let selected = view.select_closure_with_probe(None, &mut scratch, probe)?;
    let expected = nudox_root::SelectedCount::from(1);
    if selected.count() != expected {
        return Err(ScenarioError::SelectionCount {
            expected,
            observed: selected.count(),
        });
    }
    Ok(())
}

fn plan_scratch() -> Result<(ClosureScratch, PlanScratch), ScenarioError> {
    let closure = ClosureScratch::new(1).map_err(|source| ScenarioError::Scratch {
        step: ScratchStep::PromisedPlanning,
        source,
    })?;
    let planning = PlanScratch::new(nudox_root::SelectedCount::from(1_u32)).map_err(|source| {
        ScenarioError::Scratch {
            step: ScratchStep::PromisedPlanning,
            source,
        }
    })?;
    Ok((closure, planning))
}

fn verify_promised_plan(
    view: &GenerationView<'_, '_, ObjectDomain>,
    object: ObjectRef<ObjectDomain>,
    probe: &mut impl ScenarioProbe,
) -> Result<AbsentCount, ScenarioError> {
    let (mut closure, mut planning) = plan_scratch()?;
    let need = Need::new(view.id, Projection::CompleteGeneration).bind(view)?;
    let planned = plan_with_probe(need, &mut closure, &mut planning, |_| false, probe)?;
    let promised = planned.promised().try_fold(0_u32, |count, object| {
        let _object = object?;
        Ok::<u32, nudox_root::LocalityReadError>(count + 1)
    })?;
    match planned.stage().verify(|_| false) {
        Err(VerificationError::MissingObject {
            pinned_root,
            object: missing,
        }) if pinned_root == view.id && missing == object => Ok(AbsentCount::from(promised)),
        Err(error) => Err(ScenarioError::Verification(error)),
        Ok(_) => Err(ScenarioError::ExpectedMissingVerification {
            root: view.id,
            object,
        }),
    }
}

fn verify_ready_plan(
    view: &GenerationView<'_, '_, ObjectDomain>,
    store: &mut MemoryStore<ObjectDomain>,
    probe: &mut impl ScenarioProbe,
) -> Result<ReadyGeneration, ScenarioError> {
    let (mut closure, mut planning) = plan_scratch()?;
    let need = Need::new(view.id, Projection::CompleteGeneration).bind(view)?;
    let planned = plan_with_probe(
        need,
        &mut closure,
        &mut planning,
        |candidate| store.get(candidate.content).is_some(),
        probe,
    )?;
    let verified = planned
        .stage()
        .verify(|candidate| store.get(candidate.content).is_some())?;
    Ok(verified.publish())
}

fn validate_frame(bytes: &[u8]) -> Result<(), ScenarioError> {
    let sections = [SectionInput {
        kind: SectionKind::Data,
        rows: RowCount::try_from(1)?,
        bytes,
    }];
    let mut frame = vec![0; usize::from(encoded_len(&sections)?)];
    encode_into(&sections, &mut frame)?;
    let observed = ValidatedFrame::validate(&frame)?.sections().count();
    if observed != 1 {
        return Err(ScenarioError::SectionCount {
            expected: 1,
            observed,
        });
    }
    Ok(())
}

fn run_resident_operation(
    view: &GenerationView<'_, '_, ObjectDomain>,
    ready: &ReadyGeneration,
    object: ObjectRef<ObjectDomain>,
) -> Result<RowCount, ScenarioError> {
    let provider = LocalObjectProvider::from_view(view, EntryKey::from(1))?.ok_or(
        ScenarioError::MissingOperationProvider {
            step: OperationStep::ResidentBatch,
            key: EntryKey::from(1),
        },
    )?;
    let mut run = provider.start(PinnedObjectRequest {
        generation: ready.pinned_root,
        required: object,
    })?;
    let emitted = match run.next_batch()? {
        SourcePoll::Batch(batch) => match batch.as_ref() {
            [observed] if *observed == object => batch.len(),
            [observed] => {
                return Err(ScenarioError::BatchObject {
                    step: OperationStep::ResidentBatch,
                    expected: object,
                    observed: *observed,
                });
            }
            observed => {
                return Err(ScenarioError::BatchCount {
                    step: OperationStep::ResidentBatch,
                    expected: 1,
                    observed: observed.len(),
                });
            }
        },
        SourcePoll::Terminal(observed) => {
            return Err(ScenarioError::PollClass {
                step: OperationStep::ResidentBatch,
                expected: PollClass::Batch,
                observed: summary_class(&observed),
            });
        }
        SourcePoll::Finished => {
            return Err(ScenarioError::PollClass {
                step: OperationStep::ResidentBatch,
                expected: PollClass::Batch,
                observed: PollClass::Finished,
            });
        }
    };
    expect_terminal(
        run.next_batch()?,
        OperationStep::ResidentTerminal,
        TerminalSummary::Complete { emitted: 1 },
    )?;
    expect_finished(run.next_batch()?, OperationStep::ResidentFinished)?;
    Ok(RowCount::try_from(u32::try_from(emitted)?)?)
}

const fn summary_class<Missing>(summary: &TerminalSummary<Missing>) -> PollClass {
    match summary {
        TerminalSummary::Complete { .. } => PollClass::Complete,
        TerminalSummary::Partial { .. } => PollClass::Partial,
        TerminalSummary::Cancelled { .. } => PollClass::Cancelled,
    }
}

fn expect_terminal<Batch>(
    poll: SourcePoll<Batch, nudox_operation::MissingObject<ObjectDomain>>,
    step: OperationStep,
    expected: TerminalSummary<nudox_operation::MissingObject<ObjectDomain>>,
) -> Result<(), ScenarioError> {
    match poll {
        SourcePoll::Terminal(observed) if observed == expected => Ok(()),
        SourcePoll::Terminal(observed) => Err(ScenarioError::TerminalSummary {
            step,
            expected,
            observed,
        }),
        SourcePoll::Batch(_) => Err(ScenarioError::PollClass {
            step,
            expected: summary_class(&expected),
            observed: PollClass::Batch,
        }),
        SourcePoll::Finished => Err(ScenarioError::PollClass {
            step,
            expected: summary_class(&expected),
            observed: PollClass::Finished,
        }),
    }
}

fn expect_finished<Batch>(
    poll: SourcePoll<Batch, nudox_operation::MissingObject<ObjectDomain>>,
    step: OperationStep,
) -> Result<(), ScenarioError> {
    match poll {
        SourcePoll::Finished => Ok(()),
        SourcePoll::Batch(_) => Err(ScenarioError::PollClass {
            step,
            expected: PollClass::Finished,
            observed: PollClass::Batch,
        }),
        SourcePoll::Terminal(observed) => Err(ScenarioError::PollClass {
            step,
            expected: PollClass::Finished,
            observed: summary_class(&observed),
        }),
    }
}

fn run_runtime(
    generation: GenerationId,
    probe: &mut impl ScenarioProbe,
) -> Result<(TerminalClass, RuntimeMetrics), ScenarioError> {
    let mut runtime: RemoteRuntime<GenerationId, RetainedBytes> = RemoteRuntime::new(
        1,
        ByteBudget::new(RetainedBytes::from(4), ByteQuantum::try_from(4)?)?,
    )?;
    let (admission, mut owner) = runtime.split();
    let _handle = admission.admit_with_probe(generation, RetainedBytes::from(4), probe)?;
    match admission.admit_with_probe(generation, RetainedBytes::from(4), probe) {
        Err(nudox_runtime::AdmissionError::Rejected { rejected })
            if rejected.reason == nudox_runtime::RejectionReason::WorkSlots
                && rejected.work == RetainedBytes::from(4) => {}
        Err(nudox_runtime::AdmissionError::Rejected { rejected }) => {
            return Err(ScenarioError::RuntimeRejection {
                observed: rejected.reason,
            });
        }
        Err(error) => return Err(ScenarioError::Admission(error)),
        Ok(handle) => return Err(ScenarioError::RuntimeUnexpectedAdmission { handle }),
    }
    let progress = owner.execute_next_with_probe(&generation, |_| Ok(()), probe)?;
    if progress != nudox_runtime::OwnerProgress::Terminalized {
        return Err(ScenarioError::OwnerProgress { observed: progress });
    }
    let terminal = owner
        .poll_terminal_with_probe(probe)?
        .ok_or(ScenarioError::MissingRuntimeTerminal)?;
    let terminal_class = match terminal.outcome {
        TerminalOutcome::Completed => TerminalClass::Completed,
        TerminalOutcome::Failed { failure } => match failure {},
        TerminalOutcome::Cancelled => TerminalClass::Cancelled,
        TerminalOutcome::StaleGeneration => TerminalClass::StaleGeneration,
        TerminalOutcome::ExecutorUnwound => TerminalClass::ExecutorUnwound,
    };
    if terminal_class != TerminalClass::Completed {
        return Err(ScenarioError::RuntimeTerminal {
            observed: terminal_class,
        });
    }
    Ok((terminal_class, runtime.metrics()))
}

fn run_workflow(
    generation: GenerationId,
    object: ObjectRef<ObjectDomain>,
    probe: &mut impl ScenarioProbe,
) -> Result<PhaseName, ScenarioError> {
    let key = StageKey::derive(
        OperationId::PinnedObject,
        StageId::HydrateObject,
        &StageInput {
            generation,
            content: object.content,
        },
        ContentId::<CapabilityDomain>::from([1; 32]),
        ContentId::<ConfigurationDomain>::from([2; 32]),
    );
    let events = [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged {
            output: object.content,
        },
        EventKind::Verified {
            output: object.content,
        },
        EventKind::PublicationStarted {
            output: object.content,
        },
        EventKind::Published {
            output: object.content,
        },
    ];
    let mut log = MemoryWorkflowLog::new(events.len())?;
    for kind in events {
        let _reduction = log.append_then_reduce_with_probe(
            WorkflowEvent {
                version: WorkflowVersion::WAVE1,
                key,
                kind,
            },
            probe,
        )?;
    }
    let replay = log.replay()?;
    let expected_state = WorkflowState::Keyed {
        key,
        phase: Phase::Published(object.content),
    };
    if replay.state != expected_state || replay.pending_effect.is_some() {
        return Err(ScenarioError::Recovery {
            expected_state,
            observed_state: replay.state,
            expected_effect: None,
            observed_effect: replay.pending_effect,
        });
    }
    Ok(replay.state.phase())
}

fn run_promised_operation(
    view: &GenerationView<'_, '_, ObjectDomain>,
    object: ObjectRef<ObjectDomain>,
    providers: ProviderSet,
) -> Result<(), ScenarioError> {
    let provider = LocalObjectProvider::from_view(view, EntryKey::from(1))?.ok_or(
        ScenarioError::MissingOperationProvider {
            step: OperationStep::PromisedTerminal,
            key: EntryKey::from(1),
        },
    )?;
    let mut run = provider.start(PinnedObjectRequest {
        generation: view.id,
        required: object,
    })?;
    expect_terminal(
        run.next_batch()?,
        OperationStep::PromisedTerminal,
        TerminalSummary::Partial {
            emitted: 0,
            missing: nudox_operation::MissingObject {
                required: object,
                providers,
            },
        },
    )?;
    expect_finished(run.next_batch()?, OperationStep::PromisedFinished)?;
    Ok(())
}
