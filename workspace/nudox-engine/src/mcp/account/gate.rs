//! [`AccountGate`] — the thing a tool call asks before it runs, and the two
//! background loops that keep its answer current.
//!
//! # The shape
//!
//! ```text
//!  tools/call ──► AccountGate::admit()  ──► ToolCallPermit   (no network, no await)
//!                        │
//!                        └─► UsageLedger::record_call()      (one small atomic write)
//!
//!  every 30 s  ──► flush_once()   seal → POST /v1/usage/record → settle
//!  every 15 m  ──► refresh_once() POST /v1/authorize (+ GET /v1/usage)
//! ```
//!
//! [`AccountGate::admit`] is synchronous and touches no socket. That is the
//! whole point: the alternative — a round trip to `api.nudox.org` before
//! answering a query against a *local* index — would add internet latency to
//! every interaction with a product whose pitch is that it is local.
//!
//! # Fail-closed, structurally
//!
//! [`crate::mcp::server::NudoxMcpServer::new`] takes a gate as a mandatory argument.
//! There is no `Default`, no `Option`, and no "unauthenticated mode" that
//! happens to serve. A build that wants an ungated server has to say so out
//! loud, with a reason, through [`AccountGate::unmetered`] — which exists
//! because tests and a future self-hosted build both legitimately need it, and
//! which is one grep away from anyone asking "where did metering go?".
//!
//! # Two secrets, kept apart
//!
//! This module holds an [`ApiKey`]. [`crate::mcp::endpoint`] holds a
//! [`crate::mcp::session::SessionToken`]. Neither type can be produced from the
//! other, neither appears in the other's module, and the gate is *below* the
//! session check in the request path: a caller that cannot prove it is local
//! never reaches a question about which account it is.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use super::cache::{AccountSummary, CachedAuthorization};
use super::credential::ApiKey;
use super::ledger::{Reconciliation, UsageLedger};
use super::service::{AccountService, AuthorizeOutcome, RecordOutcome};
use super::state::{
    Denial, GateState, Posture, ProbeFailure, ProbeStatus, QuotaKnowledge, QuotaSnapshot, Verdict,
};
use super::store::{
    CredentialLookup, CredentialStore, Error, KeySource, StoredCredential,
    resolve_credential,
};
use crate::mcp::error::McpError;

/// How often the gate re-asks `POST v1/authorize` while everything is working.
///
/// Fifteen minutes, not [`super::state::FRESH_FOR`]: the *wake* interval and
/// the *staleness threshold* are different numbers. Waking often and asking
/// rarely is what makes recovery from a lost network fast — the first tick
/// after the wifi comes back re-verifies — without turning a healthy client
/// into a poller.
pub const REFRESH_INTERVAL: Duration = Duration::from_mins(15);

/// How often the supervisor wakes to consider flushing usage.
///
/// Shorter than [`super::ledger::FLUSH_INTERVAL`] so the *decision* is
/// evaluated more often than the flush fires; the ledger's own trigger is what
/// actually gates the request.
const TICK: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// ToolCallPermit
// ---------------------------------------------------------------------------

/// Proof that a tool call was authorised and counted.
///
/// Carries no data: its existence is the payload, exactly like
/// [`crate::mcp::session::Session`]. The private field makes it unconstructible
/// outside this module, so every value of this type really did pass
/// [`AccountGate::admit`] and really did increment the ledger.
#[derive(Debug)]
pub struct ToolCallPermit {
    _private: (),
}

// ---------------------------------------------------------------------------
// AccountGate
// ---------------------------------------------------------------------------

/// Shared state behind the gate.
struct Inner {
    /// The current state. `RwLock` because `admit` reads it on every tool call
    /// and only the background loops write.
    state: RwLock<GateState>,
    /// The credential in use, if any.
    credential: RwLock<Option<StoredCredential>>,
    /// The local usage accumulator. `RwLock` because signing in swaps it for
    /// one bound to the new fingerprint.
    ledger: RwLock<Arc<UsageLedger>>,
    /// Where credentials are persisted.
    store: Box<dyn CredentialStore>,
    /// How we talk to the service. `None` for an unmetered gate.
    service: Option<Arc<dyn AccountService>>,
    /// Where the cache and ledger files live. `None` when no state directory
    /// could be resolved.
    state_dir: Option<std::path::PathBuf>,
    /// Set when metering is deliberately off, naming why.
    unmetered: Option<&'static str>,
    /// When usage was last flushed, for the ledger's time trigger.
    last_flush: Mutex<SystemTime>,
}

/// The account gate.
///
/// Cheap to clone — every clone is the same gate. `lindsey` holds one to render
/// the account view and to drive sign-in; [`crate::mcp::server::NudoxMcpServer`]
/// holds one to admit tool calls.
#[derive(Clone)]
pub struct AccountGate {
    inner: Arc<Inner>,
}

impl AccountGate {
    /// Build a gate over a credential store and a service, and read whatever
    /// this machine already knows.
    ///
    /// Does no network. The returned gate is immediately usable and immediately
    /// honest: with a cached verdict it starts in
    /// [`Posture::Active`]/[`Posture::GraceOffline`], and without one it starts
    /// in [`Posture::AwaitingFirstVerification`] and denies until the first
    /// refresh answers.
    pub fn new(
        store: Box<dyn CredentialStore>,
        service: Arc<dyn AccountService>,
        state_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let now = SystemTime::now();
        let (state, credential) = initial_state(store.as_ref(), state_dir.as_deref(), now);
        let ledger = open_ledger(state_dir.as_deref(), credential.as_ref());

        Self {
            inner: Arc::new(Inner {
                state: RwLock::new(state),
                credential: RwLock::new(credential),
                ledger: RwLock::new(Arc::new(ledger)),
                store,
                service: Some(service),
                state_dir,
                unmetered: None,
                last_flush: Mutex::new(now),
            }),
        }
    }

    /// A gate that admits everything and records nothing.
    ///
    /// `reason` is mandatory and `&'static str` so it appears in the binary and
    /// in review. The two legitimate uses today are the test suite and
    /// `lindsey`'s screenshot harness, both of which must not depend on a paid
    /// service being reachable.
    ///
    /// **This is a weakening by construction.** It exists so that the *default*
    /// does not have to be.
    pub fn unmetered(reason: &'static str) -> Self {
        let now = SystemTime::now();
        Self {
            inner: Arc::new(Inner {
                state: RwLock::new(GateState::SignedOut),
                credential: RwLock::new(None),
                ledger: RwLock::new(Arc::new(UsageLedger::in_memory(
                    &no_account_fingerprint(),
                ))),
                store: Box::new(super::store::MemoryStore::empty()),
                service: None,
                state_dir: None,
                unmetered: Some(reason),
                last_flush: Mutex::new(now),
            }),
        }
    }

    /// A metered gate pinned to a particular state, for tests that need to
    /// stand in a posture the clock would take a week to reach.
    ///
    /// Records into an in-memory ledger and has no service, so it never talks
    /// to anything.
    pub fn in_state_for_tests(state: GateState) -> Self {
        let now = SystemTime::now();
        Self {
            inner: Arc::new(Inner {
                state: RwLock::new(state),
                credential: RwLock::new(None),
                ledger: RwLock::new(Arc::new(UsageLedger::in_memory(
                    &no_account_fingerprint(),
                ))),
                store: Box::new(super::store::MemoryStore::empty()),
                service: None,
                state_dir: None,
                unmetered: None,
                last_flush: Mutex::new(now),
            }),
        }
    }

    /// Ask whether a billable tool call may run, and count it if so.
    ///
    /// Synchronous, no network, no `await`. The only I/O is the ledger's
    /// unsynced atomic write, which is page-cache-fast and is what makes a
    /// process crash cost nothing.
    pub fn admit(&self) -> Result<ToolCallPermit, McpError> {
        if self.inner.unmetered.is_some() {
            return Ok(ToolCallPermit { _private: () });
        }

        match self.posture().verdict() {
            Verdict::Allow => {
                self.ledger().record_call();
                Ok(ToolCallPermit { _private: () })
            }
            Verdict::Deny(denial) => Err(McpError::from_denial(denial)),
        }
    }

    /// The current posture, computed against the wall clock.
    pub fn posture(&self) -> Posture {
        self.posture_at(SystemTime::now())
    }

    /// The current posture as of `now`.
    pub fn posture_at(&self, now: SystemTime) -> Posture {
        self.read_state().posture(now)
    }

    /// Why metering is off, if it is.
    pub fn unmetered_reason(&self) -> Option<&'static str> {
        self.inner.unmetered
    }

    /// A credential-free description of the signed-in account, for UI.
    ///
    /// Returns `None` in every posture that has no verified account. `lindsey`
    /// renders from this and never holds an [`ApiKey`].
    pub fn account_summary(&self) -> Option<AccountSummary> {
        let credential = self.inner.credential.read().ok()?;
        let StoredCredential { key, source } = credential.as_ref()?;
        let state = self.read_state();
        match &state {
            GateState::Authorized { account, .. } => Some(AccountSummary::new(
                account,
                key.display_hint(),
                *source,
            )),
            _ => None,
        }
    }

    /// The key hint (`ndx_…4517`) and source of whatever credential is loaded.
    ///
    /// Available in postures with no verified account, so the sign-in surface
    /// can say "we have a key, it has not been accepted" rather than showing an
    /// empty form the user already filled in.
    pub fn credential_hint(&self) -> Option<(String, KeySource)> {
        let credential = self.inner.credential.read().ok()?;
        credential
            .as_ref()
            .map(|c| (c.key.display_hint(), c.source))
    }

    /// The local usage ledger, for the status bar and for the flusher.
    pub fn ledger(&self) -> Arc<UsageLedger> {
        self.inner
            .ledger
            .read()
            .map_or_else(|e| Arc::clone(&e.into_inner()), |g| Arc::clone(&g))
    }

    // -----------------------------------------------------------------------
    // Sign in / sign out
    // -----------------------------------------------------------------------

    /// Store `key` and verify it against the service.
    ///
    /// The key is written to the credential store **only after** the service
    /// accepts it. Storing first would leave a rejected key in the Keychain for
    /// the next launch to trip over, and the user would be signed in as nobody
    /// until they noticed.
    ///
    /// Returns the posture the gate settled into, so a caller can render the
    /// outcome without a second read and without a race.
    pub async fn sign_in(&self, key: ApiKey) -> Result<Posture, SignInFailure> {
        let Some(service) = self.inner.service.clone() else {
            return Err(SignInFailure::NoService);
        };

        let outcome = service
            .authorize(&key)
            .await
            .map_err(SignInFailure::Unreachable)?;

        let user = match outcome {
            AuthorizeOutcome::Allowed { user } => user,
            AuthorizeOutcome::Denied { reason } => {
                // Not stored: a key the service just refused has no business
                // persisting. The state still records the refusal so the UI can
                // show it, but it is not cached to disk either — a rejection of
                // a key we never adopted is about this attempt, not about this
                // machine.
                return Err(SignInFailure::Rejected { reason });
            }
        };

        self.inner
            .store
            .store(&key)
            .map_err(SignInFailure::CouldNotStore)?;

        let fingerprint = key.fingerprint();
        let now = SystemTime::now();

        // A new account gets a new ledger. `UsageLedger::open` is what notices
        // the fingerprint changed and counts the previous account's unflushed
        // calls as unreportable rather than billing them to this one.
        let ledger = open_ledger(
            self.inner.state_dir.as_deref(),
            Some(&StoredCredential {
                key: key.clone(),
                source: KeySource::Keychain,
            }),
        );
        if let Ok(mut slot) = self.inner.ledger.write() {
            *slot = Arc::new(ledger);
        }

        if let Ok(mut slot) = self.inner.credential.write() {
            *slot = Some(StoredCredential {
                key,
                source: KeySource::Keychain,
            });
        }

        let state = GateState::verified(
            user,
            fingerprint,
            KeySource::Keychain,
            now,
            now,
            QuotaKnowledge::Unknown,
        );
        self.commit(state.clone());

        // Best-effort: a fresh sign-in should show real numbers rather than
        // "usage unknown", but a `GET /v1/usage` that fails must not turn a
        // successful sign-in into a failure.
        if let Some(credential) = self.credential_clone()
            && let Ok(snapshot) = service.usage(&credential.key).await
        {
            self.ledger().adopt_anchor(&snapshot);
            self.observe_quota(snapshot);
        }

        Ok(self.posture_at(now))
    }

    /// Forget the credential and every cached verdict.
    ///
    /// Deletes from the credential store, clears the on-disk cache, and resets
    /// the ledger — which counts any unflushed calls as unreportable, because
    /// they are.
    pub fn sign_out(&self) -> Result<(), Error> {
        self.ledger().reset_for_sign_out();
        if let Some(dir) = self.inner.state_dir.as_deref() {
            let _ = CachedAuthorization::clear(dir);
        }
        let result = self.inner.store.delete();
        if let Ok(mut slot) = self.inner.credential.write() {
            *slot = None;
        }
        self.set_state(GateState::SignedOut);
        result
    }

    // -----------------------------------------------------------------------
    // The background work
    // -----------------------------------------------------------------------

    /// Re-verify the credential and refresh quota.
    ///
    /// Idempotent and safe to call at any cadence. Returns the posture
    /// afterwards so a caller can log or render one consistent value.
    pub async fn refresh_once(&self) -> Posture {
        let now = SystemTime::now();

        let (Some(service), Some(credential)) =
            (self.inner.service.clone(), self.credential_clone())
        else {
            // No credential loaded: re-read the store, since the user may have
            // signed in on another window, or the keychain may have unlocked.
            self.reload_credential(now);
            return self.posture_at(now);
        };

        match service.authorize(&credential.key).await {
            Ok(AuthorizeOutcome::Allowed { user }) => {
                let quota = match self.read_state() {
                    GateState::Authorized { quota, .. } => quota,
                    _ => QuotaKnowledge::Unknown,
                };
                self.commit(GateState::verified(
                    user,
                    credential.key.fingerprint(),
                    credential.source,
                    now,
                    now,
                    quota,
                ));

                if let Ok(snapshot) = service.usage(&credential.key).await {
                    self.ledger().adopt_anchor(&snapshot);
                    self.observe_quota(snapshot);
                }
            }

            Ok(AuthorizeOutcome::Denied { reason }) => {
                // A verdict, not weather. Sticky, cached, and never softened by
                // grace — see `state::GateState::Revoked`.
                self.commit(GateState::Revoked {
                    fingerprint: credential.key.fingerprint(),
                    reason,
                    at: now,
                });
            }

            Err(failure) => self.note_probe_failure(failure, credential, now),
        }

        self.posture_at(now)
    }

    /// Send one batch of usage, if one is due.
    ///
    /// Returns `true` when a request was actually made, so a supervisor can
    /// keep its own cadence honest.
    pub async fn flush_once(&self, force: bool) -> bool {
        let (Some(service), Some(credential)) =
            (self.inner.service.clone(), self.credential_clone())
        else {
            return false;
        };

        let ledger = self.ledger();

        // Resolve a batch left in flight by a crash before sealing a new one:
        // two unknowns at once make the reconciliation arithmetic unsound.
        if ledger.orphaned_batch().is_some() {
            match service.usage(&credential.key).await {
                Ok(snapshot) => {
                    let verdict = ledger.resolve_orphan(snapshot.tool_calls);
                    self.observe_quota(snapshot);
                    if verdict == Some(Reconciliation::Indeterminate) {
                        tracing::warn!(
                            "an interrupted usage batch could not be reconciled against \
                             GET /v1/usage and was dropped"
                        );
                    }
                }
                // Cannot reconcile without the authority. Leave the orphan in
                // place; the next flush tries again. It is already persisted,
                // so nothing is lost by waiting.
                Err(_) => return false,
            }
        }

        let near_limit = matches!(
            self.read_state(),
            GateState::Authorized {
                quota: QuotaKnowledge::Known(ref q),
                ..
            } if q.is_near_limit()
        );

        let since = self.since_last_flush();
        if !force && !ledger.flush_is_due(since, near_limit) {
            return false;
        }

        let Some(batch) = ledger.seal(SystemTime::now()) else {
            return false;
        };
        self.mark_flushed();

        match service.record_usage(&credential.key, batch.count()).await {
            Ok(RecordOutcome::Accepted) => {
                ledger.settle_accepted(batch);
                true
            }

            Ok(RecordOutcome::OverLimit) => {
                // The contract does not say whether a 429 recorded the batch
                // (`docs/auth.md` gap 3). Settling is the choice that cannot
                // double-bill, and the `GET /v1/usage` immediately below is
                // what establishes the truth either way. Sentry's transport
                // spec makes the same call for its own 429s — discard, respect
                // the limit, do not retry.
                ledger.settle_accepted(batch);
                match service.usage(&credential.key).await {
                    Ok(snapshot) => {
                        ledger.adopt_anchor(&snapshot);
                        self.observe_quota(snapshot);
                    }
                    // We know we are over even if we cannot say by how much.
                    // Recording that with fabricated numbers would be worse
                    // than recording it with none, so the state keeps whatever
                    // snapshot it had and the posture is driven by the 429.
                    Err(_) => self.mark_over_limit_without_numbers(),
                }
                true
            }

            Err(failure) => {
                if failure.is_safely_retryable() {
                    ledger.requeue(batch);
                } else {
                    ledger.drop_ambiguous(batch);
                }
                true
            }
        }
    }

    /// Run the refresh and flush loops until `shutdown` fires, then flush once
    /// more.
    ///
    /// The final flush is why `stop` exists at all: without it, quitting the
    /// app strands up to [`super::ledger::FLUSH_INTERVAL`] of usage in a file
    /// that the next launch would have to reconcile.
    pub async fn supervise(self, shutdown: tokio_util::sync::CancellationToken) {
        let mut last_refresh = SystemTime::UNIX_EPOCH;

        loop {
            let due_for_refresh = SystemTime::now()
                .duration_since(last_refresh)
                .map_or(true, |d| d >= REFRESH_INTERVAL);

            if due_for_refresh {
                self.refresh_once().await;
                last_refresh = SystemTime::now();
            }

            self.flush_once(false).await;

            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(TICK) => {}
            }
        }

        // Drain. `force` because the time trigger almost certainly has not
        // fired, and the calls are billable regardless of when the user quit.
        self.flush_once(true).await;
        self.ledger().checkpoint();
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn read_state(&self) -> GateState {
        self.inner
            .state
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    fn set_state(&self, state: GateState) {
        match self.inner.state.write() {
            Ok(mut slot) => *slot = state,
            Err(e) => *e.into_inner() = state,
        }
    }

    /// Set the state *and* persist whatever part of it belongs on disk.
    fn commit(&self, state: GateState) {
        if let Some(dir) = self.inner.state_dir.as_deref()
            && let Some(cached) = CachedAuthorization::from_state(&state)
            && let Err(e) = cached.save(dir)
        {
            tracing::warn!(error = %e, "could not cache the authorisation verdict");
        }
        self.set_state(state);
    }

    fn credential_clone(&self) -> Option<StoredCredential> {
        self.inner.credential.read().ok()?.clone()
    }

    /// Fold a fresh usage snapshot into the state.
    fn observe_quota(&self, snapshot: QuotaSnapshot) {
        let state = self.read_state();
        if let GateState::Authorized {
            account,
            source,
            last_failure,
            ..
        } = state
        {
            self.commit(GateState::Authorized {
                account,
                source,
                quota: QuotaKnowledge::Known(snapshot),
                last_failure,
            });
        }
    }

    /// Record that we are over limit when `GET /v1/usage` is unreachable.
    ///
    /// Marks the *existing* snapshot over-limit rather than inventing one: a
    /// fabricated `used`/`limit` pair on a billing screen is a lie the user
    /// would reasonably act on.
    fn mark_over_limit_without_numbers(&self) {
        let state = self.read_state();
        if let GateState::Authorized {
            account,
            source,
            quota: QuotaKnowledge::Known(mut snapshot),
            last_failure,
        } = state
        {
            snapshot.over_limit = true;
            snapshot.remaining = 0;
            self.commit(GateState::Authorized {
                account,
                source,
                quota: QuotaKnowledge::Known(snapshot),
                last_failure,
            });
        }
    }

    /// Record a failed verification without destroying what we already knew.
    ///
    /// An unreachable service must never *downgrade* an `Authorized` state —
    /// that is the whole of the grace design. It only annotates it, so the UI
    /// can say why re-verification is failing, while
    /// [`GateState::posture`] keeps deriving freshness from the clock.
    fn note_probe_failure(
        &self,
        failure: ProbeFailure,
        credential: StoredCredential,
        now: SystemTime,
    ) {
        let state = self.read_state();
        let next = match state {
            GateState::Authorized {
                account,
                source,
                quota,
                ..
            } => GateState::Authorized {
                account,
                source,
                quota,
                last_failure: Some(failure),
            },
            // Never verified and still cannot verify. Stays denied, but now
            // says why rather than "waiting".
            GateState::Unverified { fingerprint, source, .. } => GateState::Unverified {
                fingerprint,
                source,
                probe: ProbeStatus::Failed(failure),
            },
            // A revocation is sticky; a failed probe does not lift it.
            revoked @ GateState::Revoked { .. } => revoked,
            GateState::SignedOut | GateState::StoreUnavailable { .. } => GateState::Unverified {
                fingerprint: credential.key.fingerprint(),
                source: credential.source,
                probe: ProbeStatus::Failed(failure),
            },
        };
        let _ = now;
        self.set_state(next);
    }

    /// Re-read the credential store, e.g. after the keychain was unlocked.
    fn reload_credential(&self, now: SystemTime) {
        let (state, credential) =
            initial_state(self.inner.store.as_ref(), self.inner.state_dir.as_deref(), now);
        if let Ok(mut slot) = self.inner.credential.write() {
            *slot = credential;
        }
        self.set_state(state);
    }

    fn since_last_flush(&self) -> Duration {
        let last = self
            .inner
            .last_flush
            .lock()
            .map_or_else(|e| *e.into_inner(), |g| *g);
        SystemTime::now()
            .duration_since(last)
            .unwrap_or(Duration::ZERO)
    }

    fn mark_flushed(&self) {
        match self.inner.last_flush.lock() {
            Ok(mut slot) => *slot = SystemTime::now(),
            Err(e) => *e.into_inner() = SystemTime::now(),
        }
    }
}

impl std::fmt::Debug for AccountGate {
    /// Renders the posture tag and nothing that could be a credential.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountGate")
            .field("posture", &self.posture().tag())
            .field("unmetered", &self.inner.unmetered)
            .finish_non_exhaustive()
    }
}

/// Why [`AccountGate::sign_in`] did not sign anyone in.
///
/// Four distinct situations, because the sign-in form says something different
/// for each: retry, fix the key, unlock the keychain, or file a bug.
#[derive(Debug, thiserror::Error)]
pub enum SignInFailure {
    /// The service could not be reached.
    #[error("{0}")]
    Unreachable(ProbeFailure),
    /// The service rejected the key.
    #[error("the service rejected this key: {reason}")]
    Rejected {
        /// The service's own reason, verbatim.
        reason: String,
    },
    /// The key was accepted but could not be saved.
    ///
    /// Deliberately a failure and not a warning: signing in without persisting
    /// means the next launch is signed out, and a user who was told "you're in"
    /// will read that as the app losing their account.
    #[error("{0}")]
    CouldNotStore(Error),
    /// This gate has no service. Only reachable on an unmetered gate.
    #[error("this build has no account service configured")]
    NoService,
}

impl SignInFailure {
    /// What the user should do next.
    pub fn help(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => {
                "Check your connection and try again. Nothing has been saved, so you can retry \
                 with the same key."
            }
            Self::Rejected { .. } => {
                "Check the key at https://nudox.org/dashboard/keys — it may have been revoked, or \
                 belong to a different environment."
            }
            Self::CouldNotStore(e) => e.help(),
            Self::NoService => "This build was compiled without an account service.",
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// The fingerprint used by a ledger with no account behind it.
///
/// A real fingerprint of a value that is not a key, so a ledger created before
/// sign-in can never be mistaken for one belonging to an account.
fn no_account_fingerprint() -> super::credential::KeyFingerprint {
    // `ApiKey::parse` is the only constructor, so this goes through the same
    // validation as a real key and produces a fingerprint in the same space —
    // one that no dashboard will ever issue.
    ApiKey::parse("ndx_0000000000000000000000000000000000000000")
        .expect("the placeholder is well formed by construction")
        .fingerprint()
}

/// Read the credential store and the cache, and decide where we start.
fn initial_state(
    store: &dyn CredentialStore,
    state_dir: Option<&std::path::Path>,
    now: SystemTime,
) -> (GateState, Option<StoredCredential>) {
    match resolve_credential(store) {
        CredentialLookup::Absent => (GateState::SignedOut, None),

        CredentialLookup::Unavailable(e) => (
            GateState::StoreUnavailable {
                message: e.to_string(),
                help: e.help(),
            },
            None,
        ),

        CredentialLookup::Found(credential) => {
            let fingerprint = credential.key.fingerprint();
            let cached = state_dir
                .and_then(CachedAuthorization::load)
                .and_then(|c| c.into_state(&fingerprint, credential.source, now));

            let state = cached.unwrap_or(GateState::Unverified {
                fingerprint,
                source: credential.source,
                probe: ProbeStatus::Pending,
            });
            (state, Some(credential))
        }
    }
}

/// Open the ledger for whichever account we hold.
fn open_ledger(
    state_dir: Option<&std::path::Path>,
    credential: Option<&StoredCredential>,
) -> UsageLedger {
    let fingerprint = credential.map_or_else(no_account_fingerprint, |c| c.key.fingerprint());
    UsageLedger::open(state_dir, &fingerprint)
}

/// Convert a [`Denial`] into the wire error an agent sees.
///
/// Lives here rather than in `error.rs` so `error.rs` keeps its single job —
/// rendering `McpError` — and the account vocabulary has exactly one place that
/// knows how it maps.
impl McpError {
    /// Build the `McpError` for a refused tool call.
    pub fn from_denial(denial: Denial) -> Self {
        match denial {
            Denial::NotSignedIn => Self::NotSignedIn,
            Denial::StoreUnavailable { message, help } => {
                Self::CredentialStoreUnavailable { message, help }
            }
            Denial::VerificationPending => Self::AuthorizationPending,
            Denial::NeverVerified { cause } => Self::NeverVerified {
                cause: cause.to_string(),
            },
            Denial::GraceExpired {
                last_verified_at,
                expired_for,
                cause,
            } => Self::OfflineGraceExpired {
                offline_for: super::state::GRACE_WINDOW + expired_for,
                grace_window: super::state::GRACE_WINDOW,
                last_verified_at,
                cause: cause.map(|c| c.to_string()),
            },
            Denial::Revoked { reason } => Self::KeyRevoked { reason },
            Denial::OverLimit { quota } => Self::QuotaExceeded {
                tier: quota.tier,
                used: quota.used,
                limit: quota.limit,
                tool_calls: quota.tool_calls,
                period_start: quota.period_start,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::account::state::{Account, UserId};

    fn key() -> ApiKey {
        ApiKey::parse("ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517").expect("parses")
    }

    fn t_ago(d: Duration) -> SystemTime {
        SystemTime::now() - d
    }

    fn authorized(verified_at: SystemTime, quota: QuotaKnowledge) -> GateState {
        GateState::Authorized {
            account: Account {
                user: UserId(24),
                fingerprint: key().fingerprint(),
                verified_at,
            },
            source: KeySource::Keychain,
            quota,
            last_failure: None,
        }
    }

    #[test]
    fn an_unmetered_gate_admits_and_records_nothing() {
        let gate = AccountGate::unmetered("unit test");
        assert_eq!(gate.unmetered_reason(), Some("unit test"));
        for _ in 0..5 {
            gate.admit().expect("an unmetered gate admits");
        }
        assert_eq!(
            gate.ledger().unreported_calls(),
            0,
            "an unmetered gate must not accrue usage it will never send"
        );
    }

    #[test]
    fn a_metered_gate_counts_every_admitted_call() {
        let gate = AccountGate::in_state_for_tests(authorized(
            SystemTime::now(),
            QuotaKnowledge::Unknown,
        ));
        for _ in 0..3 {
            gate.admit().expect("an active gate admits");
        }
        assert_eq!(gate.ledger().pending(), 3);
    }

    #[test]
    fn a_denied_call_is_never_counted() {
        // Billing a user for a call we refused to make would be the worst
        // possible defect in this module.
        let gate = AccountGate::in_state_for_tests(GateState::SignedOut);
        assert!(gate.admit().is_err());
        assert_eq!(gate.ledger().pending(), 0);
        assert_eq!(gate.ledger().unreported_calls(), 0);
    }

    #[test]
    fn every_denial_reaches_the_wire_as_its_own_error_kind() {
        use crate::mcp::account::state::{ProbeFailure, QuotaSnapshot};

        let quota = QuotaSnapshot {
            tier: "free".to_owned(),
            period_start: "2026-08-01T00:00:00Z".to_owned(),
            api_requests: 300,
            tool_calls: 700,
            used: 1000,
            limit: 1000,
            remaining: 0,
            over_limit: true,
            observed_at: SystemTime::now(),
        };

        let cases: Vec<(GateState, &str)> = vec![
            (GateState::SignedOut, "not_signed_in"),
            (
                GateState::StoreUnavailable {
                    message: "locked".to_owned(),
                    help: "unlock it",
                },
                "credential_store_unavailable",
            ),
            (
                GateState::Unverified {
                    fingerprint: key().fingerprint(),
                    source: KeySource::Keychain,
                    probe: ProbeStatus::Pending,
                },
                "authorization_pending",
            ),
            (
                GateState::Unverified {
                    fingerprint: key().fingerprint(),
                    source: KeySource::Keychain,
                    probe: ProbeStatus::Failed(ProbeFailure::NotDelivered {
                        detail: "dns".to_owned(),
                    }),
                },
                "never_verified",
            ),
            (
                authorized(
                    t_ago(crate::mcp::account::state::GRACE_WINDOW + Duration::from_secs(60)),
                    QuotaKnowledge::Unknown,
                ),
                "offline_grace_expired",
            ),
            (
                GateState::Revoked {
                    fingerprint: key().fingerprint(),
                    reason: "invalid token".to_owned(),
                    at: SystemTime::now(),
                },
                "key_revoked",
            ),
            (
                authorized(SystemTime::now(), QuotaKnowledge::Known(quota)),
                "quota_exceeded",
            ),
        ];

        for (state, expected_kind) in cases {
            let gate = AccountGate::in_state_for_tests(state);
            let err = gate.admit().expect_err("this posture must deny");
            let data = err.error_data_for_tests();
            assert_eq!(
                data.get("kind").and_then(|k| k.as_str()),
                Some(expected_kind),
                "wrong kind for {data}"
            );
            assert!(
                data.get("help").and_then(|h| h.as_str()).is_some(),
                "every account denial must tell the caller what to do: {data}"
            );
        }
    }

    #[test]
    fn a_quota_denial_carries_the_numbers_an_agent_would_otherwise_have_to_guess() {
        use crate::mcp::account::state::QuotaSnapshot;
        let gate = AccountGate::in_state_for_tests(authorized(
            SystemTime::now(),
            QuotaKnowledge::Known(QuotaSnapshot {
                tier: "free".to_owned(),
                period_start: "2026-08-01T00:00:00Z".to_owned(),
                api_requests: 300,
                tool_calls: 700,
                used: 1000,
                limit: 1000,
                remaining: 0,
                over_limit: true,
                observed_at: SystemTime::now(),
            }),
        ));
        let data = gate
            .admit()
            .expect_err("over limit must deny")
            .error_data_for_tests();

        assert_eq!(data["kind"], "quota_exceeded");
        assert_eq!(data["used"], 1000);
        assert_eq!(data["limit"], 1000);
        assert_eq!(data["tier"], "free");
        assert_eq!(data["toolCalls"], 700);
        let help = data["help"].as_str().expect("help is a string");
        assert!(
            help.contains("plan") || help.contains("limit"),
            "the help must say this is a quota, not a bug: {help}"
        );
        assert!(
            help.contains("Do not retry"),
            "the help must tell an agent to stop — retrying into a quota wall turns a \
             plan limit into a denial-of-service against our own API: {help}"
        );
        assert!(
            data["isQuotaNotFault"] == true,
            "an agent must be able to tell 'the plan ran out' from 'something broke' \
             without reading prose: {data}"
        );
    }

    #[test]
    fn the_gate_debug_carries_no_credential() {
        let gate = AccountGate::in_state_for_tests(authorized(
            SystemTime::now(),
            QuotaKnowledge::Unknown,
        ));
        let rendered = format!("{gate:?}");
        assert!(rendered.contains("active"));
        assert!(!rendered.contains("ndx_"));
    }
}
