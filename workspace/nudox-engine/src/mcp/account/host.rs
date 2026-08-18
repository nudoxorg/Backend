//! [`AccountHost`] — driving the account gate from a *synchronous* caller.
//!
//! Exactly the problem [`crate::mcp::host::McpHost`] solves, for exactly the same
//! reason, so it is solved the same way: `lindsey` links no `tokio`, cannot
//! `await`, and must not block the GPUI foreground thread (LD-2, LR-9). The
//! host borrows the engine's runtime, keeps its own [`crate::EngineHandle`]
//! clone so the runtime provably outlives the tasks by *ownership* rather than
//! by documented ordering, and exposes plain synchronous methods.
//!
//! # What is synchronous and what is not
//!
//! * [`AccountHost::posture`], [`AccountHost::account_summary`],
//!   [`AccountHost::usage`] — pure reads of shared state. Safe on the render
//!   thread, called every frame if a view wants.
//! * [`AccountHost::sign_in`] — genuinely needs the network, so it is
//!   *spawned*, not blocked on, and the caller learns the outcome through a
//!   callback delivered on the runtime. A `block_on` here would freeze the
//!   window for the length of an HTTP round trip, which is the exact defect
//!   LD-2 exists to prevent.
//! * [`AccountHost::sign_out`] — local only (delete from the keychain, clear
//!   two files), so it is a direct call.
//!
//! # Shutdown
//!
//! [`AccountHost::stop`] cancels the supervisor and waits briefly for the final
//! usage flush, because that flush is the difference between "the user's last
//! twenty tool calls were billed" and "they were not". [`Drop`] cancels without
//! waiting, on the same reasoning as `McpHost`: a forgotten `stop` should cost
//! a drain, never a hang.

use std::sync::Arc;
use std::time::Duration;

use crate::EngineHandle;
use tokio_util::sync::CancellationToken;

use super::cache::AccountSummary;
use super::credential::ApiKey;
use super::gate::{AccountGate, SignInFailure};
use super::ledger::DroppedTally;
use super::service::{AccountService, HttpAccountService};
use super::state::{Posture, ProbeFailure};
use super::store::{Error, platform_store};

/// How long [`AccountHost::stop`] waits for the final usage flush.
///
/// Bounded for the same reason [`crate::mcp::host::DRAIN_TIMEOUT`] is: this runs on
/// the main thread during app quit, and an unreachable service must not be able
/// to stall the window's close. Three seconds is one request timeout's worth of
/// patience minus the time it takes a user to notice.
pub const FLUSH_ON_QUIT_TIMEOUT: Duration = Duration::from_secs(3);

/// What a view needs to render the usage meter.
///
/// A snapshot rather than live handles, so a render pass reads one consistent
/// set of numbers instead of three that can disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageReport {
    /// Calls counted locally and not yet acknowledged by the service.
    pub pending: u32,
    /// Calls this client knows it failed to report, ever.
    ///
    /// Non-zero means we under-billed. Surfaced rather than logged because a
    /// number only a log knows is a number nobody reconciles.
    pub dropped: DroppedTally,
    /// Whether the ledger survives a restart on this machine.
    pub persistent: bool,
}

/// The account gate, driven from a synchronous host.
pub struct AccountHost {
    /// Held, not borrowed: the supervisor runs on this engine's runtime, so the
    /// engine must not be droppable before the host is.
    engine: EngineHandle,
    gate: AccountGate,
    shutdown: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl AccountHost {
    /// Start the account gate against the production service.
    ///
    /// Reads the keychain and the on-disk cache synchronously — both are local
    /// and fast — then spawns the refresh and flush loops. Returns immediately;
    /// the posture is already meaningful and will sharpen within a tick.
    pub fn start(engine: &EngineHandle) -> Result<Self, ProbeFailure> {
        let service = HttpAccountService::for_production()?;
        Ok(Self::start_with_service(engine, Arc::new(service)))
    }

    /// Start against a caller-supplied service.
    ///
    /// This is the seam the suite uses: `tests/support/fake_service.rs` runs a
    /// real HTTP server on loopback and this points at it. Doctrine §4 — a test
    /// that depends on a live paid service is not a test, and one that
    /// substitutes a Rust-level stub for the transport is not testing the
    /// transport.
    pub fn start_with_service(engine: &EngineHandle, service: Arc<dyn AccountService>) -> Self {
        let gate = AccountGate::new(platform_store(), service, super::cache::state_dir());
        Self::start_with_gate(engine, gate)
    }

    /// Start with a fully-constructed gate.
    ///
    /// Used by tests that supply their own credential store, and by the GUI's
    /// screenshot harness, which runs on [`AccountGate::unmetered`].
    pub fn start_with_gate(engine: &EngineHandle, gate: AccountGate) -> Self {
        let shutdown = CancellationToken::new();
        let task = {
            let gate = gate.clone();
            let shutdown = shutdown.clone();
            // `spawn` on the engine's runtime rather than a runtime of our own:
            // a second runtime would put these loops on a different reactor
            // from every other socket this process owns, for no benefit (LR-9).
            engine
                .runtime_handle()
                .spawn(async move { gate.supervise(shutdown).await })
        };

        Self {
            engine: engine.clone(),
            gate,
            shutdown,
            task: Some(task),
        }
    }

    /// The gate itself, to hand to [`crate::mcp::server::NudoxMcpServer::new`].
    pub fn gate(&self) -> AccountGate {
        self.gate.clone()
    }

    /// The current posture. Cheap; safe to call every frame.
    pub fn posture(&self) -> Posture {
        self.gate.posture()
    }

    /// The signed-in account, with no credential in it.
    pub fn account_summary(&self) -> Option<AccountSummary> {
        self.gate.account_summary()
    }

    /// The key hint and source of whatever credential is loaded, verified or
    /// not.
    pub fn credential_hint(&self) -> Option<(String, super::store::KeySource)> {
        self.gate.credential_hint()
    }

    /// A consistent snapshot of local usage state.
    pub fn usage(&self) -> UsageReport {
        let ledger = self.gate.ledger();
        UsageReport {
            pending: ledger.pending(),
            dropped: ledger.dropped(),
            persistent: ledger.is_persistent(),
        }
    }

    /// Verify and store `key`, delivering the outcome to `on_done`.
    ///
    /// Spawned rather than blocked on. `on_done` runs on the engine's runtime,
    /// so a GUI caller must hop back to its own thread inside it — which is
    /// what `lindsey`'s bridge already does for every engine result, so the
    /// pattern is not a new one for this codebase.
    pub fn sign_in<F>(&self, key: ApiKey, on_done: F)
    where
        F: FnOnce(Result<Posture, SignInFailure>) + Send + 'static,
    {
        let gate = self.gate.clone();
        self.engine.runtime_handle().spawn(async move {
            let outcome = gate.sign_in(key).await;
            on_done(outcome);
        });
    }

    /// Forget the credential and every cached verdict. Local only, immediate.
    pub fn sign_out(&self) -> Result<(), Error> {
        self.gate.sign_out()
    }

    /// Re-verify now rather than waiting for the next tick.
    ///
    /// What the "Retry" button in a `GraceOffline` or `NeverVerified` state
    /// calls. Spawned, for the same reason [`Self::sign_in`] is.
    pub fn refresh<F>(&self, on_done: F)
    where
        F: FnOnce(Posture) + Send + 'static,
    {
        let gate = self.gate.clone();
        self.engine.runtime_handle().spawn(async move {
            let posture = gate.refresh_once().await;
            on_done(posture);
        });
    }

    /// Stop the loops and wait up to [`FLUSH_ON_QUIT_TIMEOUT`] for the final
    /// usage flush.
    ///
    /// Idempotent.
    pub fn stop(&mut self) {
        self.shutdown.cancel();
        let Some(task) = self.task.take() else {
            return;
        };

        if tokio::runtime::Handle::try_current().is_ok() {
            // Blocking a runtime worker on a task scheduled to the same runtime
            // is a deadlock, not a slow path. `lindsey` never reaches here; a
            // test calling `stop` from inside `block_on` would.
            task.abort();
            return;
        }

        let _ = self
            .engine
            .runtime_handle()
            .block_on(async move { tokio::time::timeout(FLUSH_ON_QUIT_TIMEOUT, task).await });
    }
}

impl Drop for AccountHost {
    /// Stop the loops even when nobody called [`AccountHost::stop`].
    ///
    /// Cancel-only, never blocking: `Drop` can run during process teardown on
    /// any thread, and a `block_on` there would turn a missed `stop` into a
    /// hang. The final flush is what `stop` buys you; stopping is not optional.
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl std::fmt::Debug for AccountHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountHost")
            .field("posture", &self.gate.posture().tag())
            .finish_non_exhaustive()
    }
}
