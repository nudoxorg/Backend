//! One accepted owner refresh worker. Captures, native joins and activation never
//! borrow CommandAdapter or run under its mutex. Transport observer loss is not
//! an operation cancellation. The adapter owns the explicit cancellation flag.

use super::super::query::{
    AdmittedLocalQueryRecipe, LocalSemanticError, PreparedLocalQueryRecipe, RemoteSemantic,
};
use backend_compile::{EmbeddingActivationControl, EmbeddingExecutableError};
use backend_engine::application::{
    EmbeddingRuntimeActivationFailure, EmbeddingRuntimeConfigIdentity, EmbeddingRuntimeError,
    LocalCompilerClient, LocalCompilerEmbeddingPhase, LocalCompilerEmbeddingReceipt,
    LocalCompilerEmbeddingTransition, LocalCompilerEmbeddingTransitionError,
    PreparedEmbeddingRuntime,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) struct Input {
    pub(super) compiler: LocalCompilerClient,
    pub(super) admission: backend_engine::application::LocalCompilerEmbeddingAdmission,
    pub(super) owner: backend_engine::workspace::owner::OwnerLeaseIdentity,
    pub(super) configuration: PathBuf,
    pub(super) descriptor: PathBuf,
    pub(super) expected: [u8; 32],
    pub(super) cache_directory: Option<backend_platform::DirectoryCapability>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    Capturing,
    Prepared,
    RetiringQuery,
    RetiringCompiler,
    Activating,
    Committing,
}

pub(super) enum Event {
    Prepared(EmbeddingRuntimeConfigIdentity),
    Phase(Phase),
    Retired(LocalCompilerEmbeddingReceipt),
    CompilerCommitted(LocalCompilerEmbeddingReceipt),
    Terminal(Result<Committed, Failure>),
}

/// A compiler Serving receipt is never erased by late cancellation of an observer.
/// Query installation can still be refused under the original descriptor/control.
pub(super) struct Committed {
    pub(super) compiler: LocalCompilerEmbeddingReceipt,
    pub(super) query: Result<Arc<AdmittedLocalQueryRecipe>, Failure>,
}

#[derive(Debug)]
pub(super) enum Failure {
    NotConfigured,
    ConfigurationChanged {
        expected: [u8; 32],
        observed: EmbeddingRuntimeConfigIdentity,
    },
    Owner(backend_engine::WorkspaceError),
    Capture(EmbeddingRuntimeError),
    Query(LocalSemanticError),
    Activation(EmbeddingRuntimeActivationFailure),
    Compiler(LocalCompilerEmbeddingTransitionError),
    Control(EmbeddingExecutableError),
    Stopped,
    Panicked,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("no installed embedding configuration"),
            Self::ConfigurationChanged { expected, observed } => write!(
                f,
                "installed embedding configuration differs: expected {}, observed {}",
                backend_engine::encode_id(expected),
                backend_engine::encode_id(&observed.as_bytes())
            ),
            Self::Owner(error) => error.fmt(f),
            Self::Capture(error) => error.fmt(f),
            Self::Query(error) => error.fmt(f),
            Self::Activation(error) => error.fmt(f),
            Self::Compiler(error) => error.fmt(f),
            Self::Control(error) => error.fmt(f),
            Self::Stopped => {
                f.write_str("embedding refresh worker stopped before producing its owner outcome")
            }
            Self::Panicked => {
                f.write_str("embedding refresh worker panicked before producing its owner outcome")
            }
        }
    }
}
impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Owner(error) => Some(error),
            Self::Capture(error) => Some(error),
            Self::Query(error) => Some(error),
            Self::Activation(error) => Some(error),
            Self::Compiler(error) => Some(error),
            Self::Control(error) => Some(error),
            Self::NotConfigured
            | Self::ConfigurationChanged { .. }
            | Self::Stopped
            | Self::Panicked => None,
        }
    }
}

pub(super) struct Worker {
    pub(super) events: mpsc::Receiver<Event>,
    handoff: mpsc::SyncSender<RemoteSemantic>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    /// Spawn BEFORE the adapter moves any old provider or renews its selection.
    pub(super) fn start(input: Input) -> std::io::Result<Self> {
        // At most eight phase/receipt/terminal messages per operation; every event fits even
        // if the observer disconnects. No channel send waits on the service loop.
        let (send, events) = mpsc::sync_channel(8);
        let (handoff, original) = mpsc::sync_channel(1);
        let cancelled = Arc::clone(&input.cancelled);
        let deadline = input.deadline;
        let thread = thread::Builder::new()
            .name("locald-embedding-refresh".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(input, &send, original)
                }))
                .unwrap_or(Err(Failure::Panicked));
                let _ = send.send(Event::Terminal(result));
            })?;
        Ok(Self {
            events,
            handoff,
            cancelled,
            deadline,
            thread: Some(thread),
        })
    }
    pub(super) fn handoff(
        &self,
        original: RemoteSemantic,
    ) -> Result<(), mpsc::TrySendError<RemoteSemantic>> {
        self.handoff.try_send(original)
    }
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    /// The service repeats the original operation control immediately before moving
    /// the old owner. A queued Prepared event never authorizes handoff after cancellation.
    pub(super) fn check_handoff(&self) -> Result<(), EmbeddingExecutableError> {
        EmbeddingActivationControl::new(&self.cancelled, self.deadline).checkpoint()
    }
    /// Joining is permitted only after the actual kernel reports thread completion.
    pub(super) fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub(super) fn join_finished(&mut self) -> Option<Result<(), Failure>> {
        if !self.finished() {
            return None;
        }
        Some(match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| Failure::Panicked),
            None => Ok(()),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Owner shutdown may join; ordinary polling drops only a finished worker.
    }
}

fn checkpoint(input: &Input, control: &EmbeddingActivationControl<'_>) -> Result<(), Failure> {
    control.checkpoint().map_err(Failure::Control)?;
    input.owner.assert_current().map_err(Failure::Owner)?;
    control.checkpoint().map_err(Failure::Control)
}
fn phase(send: &mpsc::SyncSender<Event>, phase: Phase) -> Result<(), Failure> {
    send.send(Event::Phase(phase)).map_err(|_| Failure::Stopped)
}
fn transition(
    input: &Input,
    update: LocalCompilerEmbeddingTransition,
) -> Result<LocalCompilerEmbeddingReceipt, Failure> {
    input
        .admission
        .begin_transition(update, Arc::clone(&input.cancelled), input.deadline)
        .map_err(Failure::Compiler)?
        .observe_owner_outcome()
        .map_err(Failure::Compiler)
}
fn execute(
    input: Input,
    send: &mpsc::SyncSender<Event>,
    original: mpsc::Receiver<RemoteSemantic>,
) -> Result<Committed, Failure> {
    let control = EmbeddingActivationControl::new(&input.cancelled, input.deadline);
    checkpoint(&input, &control)?;
    let prepared = PreparedEmbeddingRuntime::capture_until(&input.configuration, &control)
        .map_err(Failure::Capture)?
        .ok_or(Failure::NotConfigured)?;
    if prepared.identity().as_bytes() != input.expected {
        return Err(Failure::ConfigurationChanged {
            expected: input.expected,
            observed: prepared.identity(),
        });
    }
    let query =
        PreparedLocalQueryRecipe::capture_until(&input.descriptor, prepared.spec(), &control)
            .map_err(Failure::Query)?;
    checkpoint(&input, &control)?;
    send.send(Event::Prepared(prepared.identity()))
        .map_err(|_| Failure::Stopped)?;
    let original = loop {
        checkpoint(&input, &control)?;
        match original.recv_timeout(Duration::from_millis(5)) {
            Ok(original) => break original,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Failure::Stopped),
        }
    };
    phase(send, Phase::RetiringQuery)?;
    original.retire_local().map_err(Failure::Query)?;
    checkpoint(&input, &control)?;
    prepared
        .verify_current_until(&control)
        .map_err(Failure::Capture)?;
    query
        .verify_current_until(&control)
        .map_err(Failure::Query)?;
    phase(send, Phase::RetiringCompiler)?;
    let retired = match input.compiler.retired_embedding_configuration() {
        Some(previous) if previous != prepared.identity() => transition(
            &input,
            LocalCompilerEmbeddingTransition::RetargetRetired {
                previous,
                configuration: prepared.configuration_witness(),
            },
        )?,
        _ => transition(
            &input,
            LocalCompilerEmbeddingTransition::Retire {
                desired: prepared.identity(),
            },
        )?,
    };
    send.send(Event::Retired(retired.clone()))
        .map_err(|_| Failure::Stopped)?;
    let LocalCompilerEmbeddingPhase::Retired { account } = retired.phase else {
        return Err(Failure::Compiler(
            LocalCompilerEmbeddingTransitionError::ConfigurationMismatch,
        ));
    };
    if let Some(account) = account {
        let used = account.used();
        if used != 0 {
            return Err(Failure::Compiler(
                LocalCompilerEmbeddingTransitionError::RetainedMemory { used },
            ));
        }
    }
    checkpoint(&input, &control)?;
    phase(send, Phase::Activating)?;
    let provision = prepared
        .activate_until(&control)
        .map_err(Failure::Activation)?;
    let runtime = provision.runtime().ok_or(Failure::NotConfigured)?;
    let admitted = query
        .admit_until(Arc::clone(&runtime), &control)
        .map_err(Failure::Query)?;
    let cache = input
        .cache_directory
        .clone()
        .and_then(|directory| runtime.open_durable_cache_session(directory));
    checkpoint(&input, &control)?;
    query
        .verify_current_until(&control)
        .map_err(Failure::Query)?;
    phase(send, Phase::Committing)?;
    let compiler = transition(
        &input,
        LocalCompilerEmbeddingTransition::Install { provision, cache },
    )?;
    send.send(Event::CompilerCommitted(compiler.clone()))
        .map_err(|_| Failure::Stopped)?;
    // Compiler is already committed. Keep its exact receipt even if the original
    // operation control or query pointer now refuses; never rewrite it as uncommitted.
    let query = checkpoint(&input, &control)
        .and_then(|()| query.verify_current_until(&control).map_err(Failure::Query))
        .map(|()| admitted);
    Ok(Committed { compiler, query })
}
