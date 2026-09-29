//! The owner's own thread (W-Open I1): the index owner starts — or this
//! window attaches to a live one — beside the window, never before it.
//!
//! What used to run on the main thread before `gpui::Application` existed
//! (the lease, composing an embedded owner, twelve retries over twenty
//! seconds, and a full hydration of the view only to learn its root) runs
//! here. It ends in one [`OwnerState`] on the gate: `Ready` with the owner's
//! root from one `revision()` round trip, or `Failed` in the host's own words.
//! A failed owner waits for the window's "Try again" and starts again; a
//! serving one keeps its host alive until the app quits.

use super::lease::{DesktopHost, HostError, HostMode};
use crate::core::VersionedRoot;
use crate::model::ServiceMode;
use crate::runtime::owner::{OwnerGate, OwnerState};
use backend_client::Session;
use backend_runtime::WorkspacePaths;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Attempts before an owner that never answers is reported.
const ATTEMPTS: usize = 12;
/// Pause between attempts.
const RETRY: Duration = Duration::from_millis(120);
/// Wall-clock bound over all attempts.
const DEADLINE: Duration = Duration::from_secs(20);

/// The running owner thread; dropping it lets the host go and joins.
pub(crate) struct OwnerThread {
    gate: OwnerGate,
    join: Option<JoinHandle<()>>,
}

impl Drop for OwnerThread {
    fn drop(&mut self) {
        self.gate.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Starts the owner for `paths` on its own thread. Returns at once.
pub(crate) fn spawn(paths: WorkspacePaths, gate: OwnerGate) -> Option<OwnerThread> {
    let owner = gate.clone();
    match std::thread::Builder::new()
        .name("nudox-owner".to_owned())
        .spawn(move || run(&paths, &owner))
    {
        Ok(join) => Some(OwnerThread {
            gate,
            join: Some(join),
        }),
        Err(error) => {
            gate.publish(OwnerState::Failed(Arc::from(format!(
                "the owner's thread could not start: {error}"
            ))));
            None
        }
    }
}

fn run(paths: &WorkspacePaths, gate: &OwnerGate) {
    loop {
        match start(paths, gate) {
            Ok((host, key)) => {
                let mode = match host.mode() {
                    HostMode::Embedded => ServiceMode::Embedded,
                    HostMode::Attached => ServiceMode::Attached,
                };
                gate.publish(OwnerState::Ready { key, mode });
                gate.await_close();
                drop(host);
                return;
            }
            Err(message) => {
                gate.publish(OwnerState::Failed(Arc::from(message)));
                // `restart` publishes `Starting` itself.
                if !gate.await_restart() {
                    return;
                }
            }
        }
    }
}

/// Tries until the owner answers or the deadline passes. The first failure
/// is published at once, so the window says what is wrong within one
/// attempt, not after twenty seconds; the later attempts continue behind
/// it, and one that succeeds replaces the failure with the owner's root.
fn start(paths: &WorkspacePaths, gate: &OwnerGate) -> Result<(DesktopHost, VersionedRoot), String> {
    let started = Instant::now();
    let mut last = "the local service did not become ready".to_owned();
    for attempt in 0..ATTEMPTS {
        match attempt_once(paths) {
            Ok(opened) => return Ok(opened),
            Err(message) => {
                if attempt == 0 {
                    gate.publish(OwnerState::Failed(Arc::from(message.as_str())));
                }
                last = message;
            }
        }
        if started.elapsed() >= DEADLINE {
            return Err(format!("{last} (gave up after {}s)", started.elapsed().as_secs()));
        }
        if attempt.saturating_add(1) < ATTEMPTS {
            std::thread::sleep(RETRY);
        }
    }
    Err(last)
}

fn attempt_once(paths: &WorkspacePaths) -> Result<(DesktopHost, VersionedRoot), String> {
    let starting = Instant::now();
    let host = DesktopHost::start_with_paths(paths.clone()).map_err(|error| describe(&error))?;
    crate::runtime::trace::span("boot.owner_start", starting, format_args!("{:?}", host.mode()));
    // One round trip for the root, where a full hydration of the view used
    // to be (406-581 ms on the fixture index, LEDGER §2.1-2).
    let asking = Instant::now();
    let revision = Session::connect(host.endpoint())
        .and_then(|mut session| session.revision())
        .map_err(|error| format!("read the owner's revision: {error}"))?;
    crate::runtime::trace::span("boot.revision", asking, "Session::revision");
    Ok((host, VersionedRoot::from_revision(1, revision.cursor(), 0)))
}

fn describe(error: &HostError) -> String {
    format!("reach the local service at {}: {error}", error.operand())
}
