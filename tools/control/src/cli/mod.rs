//! Bounded command dispatch for the typed control-plane adapter.

mod commands;
// Typed evidence adapters ported from the remote branch. The remote never
// assigned them command names, and `candidate`/`evaluation`/`decision` are
// already the ledger lifecycle commands, so they compile here unexposed until
// a dispatch grammar is chosen.
#[expect(
    dead_code,
    reason = "evidence adapters have no dispatch grammar yet; wiring them lifts this"
)]
mod evidence;
mod input;
mod output;

use std::env;

use backend_control::ControlError;

pub(crate) fn run() -> Result<(), ControlError> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let command = arguments
        .first()
        .map(String::as_str)
        .ok_or(ControlError::UnknownCommand)?;
    match command {
        "help" | "--help" | "-h" => {
            commands::help();
            Ok(())
        }
        "init" => commands::init(&arguments),
        "status" => commands::status(&arguments),
        "key" | "work-key" => commands::key(&arguments),
        "plan" | "apply" => commands::plan_or_apply(command, &arguments),
        "admit" => commands::admit(&arguments),
        "renew" => commands::renew(&arguments),
        "candidate" => commands::freeze(&arguments),
        "evaluation" => commands::evaluate(&arguments),
        "review" => commands::review(&arguments),
        "decision" => commands::decide(&arguments),
        "fail" | "cancel" => commands::fail_or_cancel(command, &arguments),
        "recover" => commands::recover(&arguments),
        "invalidate" => commands::invalidate(&arguments),
        _ => Err(ControlError::UnknownCommand),
    }
}
