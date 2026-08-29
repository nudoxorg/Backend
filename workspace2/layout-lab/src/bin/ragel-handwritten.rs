//! Typed handwritten controls for the Ragel source-generation experiment.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CursorPhase {
    New,
    Requested,
    Admitted,
    Staged,
    Verified,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CursorError {
    Impossible { phase: CursorPhase, event: u8 },
}

fn cursor_step(phase: CursorPhase, event: u8) -> Result<CursorPhase, CursorError> {
    match (phase, event) {
        (CursorPhase::New, b'r') => Ok(CursorPhase::Requested),
        (CursorPhase::Requested, b'a') => Ok(CursorPhase::Admitted),
        (CursorPhase::Admitted, b's') => Ok(CursorPhase::Staged),
        (CursorPhase::Staged, b'v') => Ok(CursorPhase::Verified),
        (phase, event) => Err(CursorError::Impossible { phase, event }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkflowPhase {
    New,
    Requested,
    Admitted,
    Staged,
    Verified,
    Publishing,
    Published,
}

fn workflow_step(phase: WorkflowPhase, event: u8) -> Option<WorkflowPhase> {
    match (phase, event) {
        (WorkflowPhase::New, b'r') => Some(WorkflowPhase::Requested),
        (WorkflowPhase::Requested, b'a') => Some(WorkflowPhase::Admitted),
        (WorkflowPhase::Admitted, b's') => Some(WorkflowPhase::Staged),
        (WorkflowPhase::Staged, b'v') => Some(WorkflowPhase::Verified),
        (WorkflowPhase::Verified, b'p') => Some(WorkflowPhase::Publishing),
        (WorkflowPhase::Publishing, b'u') => Some(WorkflowPhase::Published),
        _ => None,
    }
}

fn run(input: &[u8]) -> bool {
    let cursor = input
        .iter()
        .copied()
        .take(4)
        .try_fold(CursorPhase::New, cursor_step);
    let workflow = input
        .iter()
        .copied()
        .try_fold(WorkflowPhase::New, |phase, event| {
            workflow_step(phase, event).ok_or(())
        });
    cursor == Ok(CursorPhase::Verified) && workflow == Ok(WorkflowPhase::Published)
}

fn main() {
    #[allow(
        clippy::manual_unwrap_or_default,
        reason = "the control keeps absent command input explicit"
    )]
    let input = match std::env::args().nth(1) {
        Some(value) => value,
        None => String::new(),
    };
    if !run(input.as_bytes()) {
        std::process::exit(1);
    }
}
