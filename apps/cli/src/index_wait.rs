//! CLI caller lifetime for one durable Add; owner work remains asynchronous.
use backend_client::{
    IndexOperationWaitResult, IndexOperationWaitStop, MAX_INDEX_OPERATION_WAIT, RequestContext,
    Session,
};
use backend_library::{IndexOperationObservation, IndexOperationState, SurfaceReply};
use backend_present::{Answer, Fault, Invocation, Operand};
use std::io::Write as _;
use std::time::Duration;

pub(crate) const EXIT_WAIT_INCOMPLETE: u8 = 75;

#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexWaitPolicy {
    timeout: Duration,
}

impl IndexWaitPolicy {
    pub(crate) fn from_invocation(invocation: &Invocation) -> Result<Option<Self>, Fault> {
        if !invocation.flag("wait") {
            if invocation.option("wait-timeout").is_some() {
                return Err(Fault::usage(
                    "--wait-timeout",
                    "--wait-timeout requires --wait",
                ));
            }
            return Ok(None);
        }
        let timeout =
            invocation
                .option("wait-timeout")
                .map_or(Ok(MAX_INDEX_OPERATION_WAIT), |value| {
                    value
                        .parse::<u64>()
                        .ok()
                        .filter(|seconds| *seconds <= MAX_INDEX_OPERATION_WAIT.as_secs())
                        .map(Duration::from_secs)
                        .ok_or_else(|| {
                            Fault::usage(
                                "--wait-timeout",
                                "--wait-timeout must be an integer from 0 to 900 seconds",
                            )
                        })
                })?;
        Ok(Some(Self { timeout }))
    }
}

pub(crate) fn finish(
    session: &mut Session,
    paths: &backend_runtime::WorkspacePaths,
    answer: Answer,
    policy: IndexWaitPolicy,
) -> Result<(Answer, Option<u8>), Fault> {
    finish_with_recovery(session, paths, answer, policy, None)
}

pub(crate) fn finish_with_recovery(
    session: &mut Session,
    paths: &backend_runtime::WorkspacePaths,
    answer: Answer,
    policy: IndexWaitPolicy,
    mut recovery: Option<&mut crate::owner_recovery::OwnerRecovery>,
) -> Result<(Answer, Option<u8>), Fault> {
    let Answer::Product(product) = &answer else {
        return Err(Fault::usage(
            "--wait",
            "--wait requires a durable Add operation",
        ));
    };
    let initial = product.index_operation().cloned().ok_or_else(||
        Fault::usage("--wait", "the Add reply has no durable operation identity; waiting cannot infer one from a ticket"))?;
    let context = RequestContext::new();
    // Zero is an explicit immediate observation. Keep its accepted identity
    // even if the caller's deadline has already expired before socket binding.
    let deadline = context.read_deadline(policy.timeout);
    let binding = if deadline.is_ok() {
        session.set_request_context(Some(context.clone()))
    } else {
        Ok(())
    };
    let progress = progress_sink();
    let result = match binding {
        Ok(()) => backend_client::wait_index_operation(
            initial,
            &context,
            |command| {
                backend_client::surface_with_index_journal(paths, command, |command| match recovery
                    .as_deref_mut()
                {
                    Some(recovery) => recovery.surface(session, command, Some(&context)),
                    None => session.surface(command),
                })
            },
            |observation| {
                if let Some(sender) = &progress {
                    let _ = sender.try_send(progress_line(observation));
                }
            },
        ),
        Err(error) => Ok(IndexOperationWaitResult::Stopped {
            observation: initial,
            reason: IndexOperationWaitStop::Client(error),
        }),
    };
    // Detach the exact socket before the context is completed or reused.
    let detached = session.set_request_context(None);
    context.finish();
    detached.map_err(|error| {
        Fault::from_client_error(&error, Operand::Text("index wait".to_owned()))
    })?;
    Ok(project_result(result.map_err(|error| {
        Fault::from_client_error(&error, Operand::Text("index wait".to_owned()))
    })?))
}

fn project_result(result: IndexOperationWaitResult) -> (Answer, Option<u8>) {
    let (observation, stop) = match result {
        IndexOperationWaitResult::Terminal(observation) => (observation, None),
        IndexOperationWaitResult::Stopped {
            observation,
            reason,
        } => (observation, Some(reason)),
    };
    let product = backend_present::product_view(&SurfaceReply::IndexOperationStatus(observation));
    match stop {
        None => (Answer::Product(Box::new(product)), None),
        Some(reason) => (
            Answer::Product(Box::new(product.with_index_wait_stop(&reason))),
            Some(EXIT_WAIT_INCOMPLETE),
        ),
    }
}

fn progress_line(observation: &IndexOperationObservation) -> String {
    let (key, state) = match observation {
        IndexOperationObservation::Known(status) => (
            status.operation_key,
            match &status.state {
                IndexOperationState::Accepted => "accepted".to_owned(),
                IndexOperationState::Active { stage, .. } => {
                    format!("active ({})", backend_present::index_stage_name(*stage))
                }
                IndexOperationState::Published(_) => "published".to_owned(),
                IndexOperationState::PartiallyPublished { .. } => "partially published".to_owned(),
                IndexOperationState::Failed { .. } => "failed".to_owned(),
                IndexOperationState::Unresolved { .. } => "unresolved".to_owned(),
            },
        ),
        IndexOperationObservation::Unknown { operation_key } => {
            (*operation_key, "unknown".to_owned())
        }
        IndexOperationObservation::OutsideReceiptWindow { operation_key, .. } => {
            (*operation_key, "outside receipt window".to_owned())
        }
    };
    format!("index operation {key}: {state}; resume: nudox index_progress --operation-key {key}")
}

pub(crate) fn progress_sink() -> Option<std::sync::mpsc::SyncSender<String>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<String>(4);
    // One bounded best-effort reporter cannot block the operation's deadline
    // on redirected stderr. It never writes protocol/machine stdout.
    std::thread::Builder::new()
        .name("index-wait-progress".to_owned())
        .spawn(move || {
            for line in receiver {
                let _ = writeln!(std::io::stderr().lock(), "{line}");
            }
        })
        .ok()?;
    Some(sender)
}
