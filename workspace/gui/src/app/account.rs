//! Hosting the account gate, and turning its nine postures into something a
//! view can paint.
//!
//! # Why a `Global` and not a store
//!
//! Exactly the reasoning in [`crate::app::mcp`]: an account is one-per-process
//! state that must outlive any window. `app::lifecycle` can dismiss and rebuild
//! the window at any time (cmd-W, then dock icon), and anything held in `Shell`
//! dies with it — so a signed-in session parked in a view field would silently
//! sign the user out when they closed the window.
//!
//! # Why there is a presentation type at all
//!
//! [`nudox_engine::mcp::Posture`] is nine variants carrying `SystemTime`s, `Duration`s
//! and a `QuotaSnapshot`. Rendering it directly would mean formatting in
//! `render` — which GUI-PLAN §1.1.4 rules out, because this is chrome that
//! repaints at 120 Hz and `format!` allocates every time — and would put the
//! wording of "5 days left" in a view file, where nothing tests it.
//!
//! [`AccountPresentation`] is derived once per transition, holds only
//! `SharedString`s, and is what both the status bar and the sign-in overlay
//! read. It carries **no credential**: `AccountSummary` gives us a user id and
//! `ndx_…4517`, and that is all the GUI ever holds. The most likely place for a
//! secret to end up permanently is a rendered string, and the cheapest defence
//! is that the renderer never has one.
//!
//! # Absent is a state, not a placeholder
//!
//! [`AccountStatus::Absent`] means *this process hosts no account gate* — every
//! `#[gpui::test]` app, and the frames rendered before `main` installs the
//! service. It is the same distinction [`crate::app::mcp::McpStatus::Absent`]
//! draws, for the same reason L35 records: a status that cannot tell "nothing
//! was asked for" from "it failed" makes the failure invisible.

use gpui::{App, Global, SharedString};
use nudox_engine::EngineHandle;
use nudox_engine::mcp::account::host::{AccountHost, UsageReport};
use nudox_engine::mcp::account::state::GRACE_WARNING_AT;
use nudox_engine::mcp::{AccountGate, ApiKey, Posture, SignInFailure};

/// What the process-wide account gate is doing, in the vocabulary views paint.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountStatus {
    /// This process hosts no account gate.
    Absent,
    /// A gate is installed; this is its current posture, pre-rendered.
    Live(AccountPresentation),
}

impl AccountStatus {
    /// The status of the process-wide gate, or [`Absent`](Self::Absent).
    ///
    /// Views read through this rather than being handed a value at
    /// construction, so a window opened before — or without — the service still
    /// renders something true.
    pub fn from_app(cx: &App) -> Self {
        cx.try_global::<AccountService>()
            .map_or(Self::Absent, |service| service.status().clone())
    }

    /// The presentation, when there is one.
    pub fn presentation(&self) -> Option<&AccountPresentation> {
        match self {
            Self::Live(p) => Some(p),
            Self::Absent => None,
        }
    }

    /// The status-bar segment text, or `None` to hide the segment.
    ///
    /// Hidden only when this process hosts no gate. Every real posture gets a
    /// segment — including `Active`, which is the one a naive design would hide
    /// as "nothing to report". A paid product whose signed-in state is
    /// invisible is one where a *signed-out* state is also invisible, and the
    /// user finds out at their first tool call.
    pub fn label(&self) -> Option<SharedString> {
        self.presentation().map(|p| p.segment.clone())
    }

    /// Whether the segment should be painted in the warning colour.
    pub fn is_urgent(&self) -> bool {
        self.presentation().is_some_and(|p| p.urgent)
    }

    /// The menu bar's account item, or `None` to omit it entirely.
    ///
    /// `None` only for [`Self::Absent`] — this process hosts no gate, so
    /// there is nothing for the item to open. Every real posture gets one,
    /// including `Active`: `cmd-shift-A`'s own overlay is how sign-out is
    /// reachable once the window is up, and with the window dismissed the
    /// menu bar is the *only* surface left (`app::menus`) — an app that can
    /// be signed in for good with no visible way to sign out is the same
    /// defect L35 records for a hidden MCP segment.
    ///
    /// Borrows the overlay's headline rather than the status bar's terser
    /// segment text: this reads as a clickable prompt ("open the account
    /// panel"), not an ambient status line.
    pub fn menu_label(&self) -> Option<SharedString> {
        self.presentation()
            .map(|p| SharedString::from(format!("{}…", p.headline)))
    }
}

/// Everything a view needs to paint the account, pre-formatted.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountPresentation {
    /// The posture's stable machine tag (`active`, `grace_offline`, …).
    ///
    /// Carried so a screenshot harness and a test can assert on the *state*
    /// rather than on prose that may be reworded. Never painted.
    pub tag: &'static str,
    /// The status-bar segment, e.g. `"account · offline 5d"`.
    pub segment: SharedString,
    /// The overlay's headline, e.g. `"Signed in"`.
    pub headline: SharedString,
    /// One sentence under the headline saying what is true and what to do.
    pub detail: SharedString,
    /// `ndx_…4517`, when a credential is loaded. Never the key.
    pub key_hint: Option<SharedString>,
    /// Where that credential came from — `Keychain` or `NUDOX_API_KEY`.
    pub source: Option<SharedString>,
    /// `"user 24"`, when verified.
    pub user: Option<SharedString>,
    /// `"700 of 1000 tool calls"`, when usage is known.
    pub usage: Option<SharedString>,
    /// `used / limit` in `0.0..=1.0`, for the meter bar. `None` when unknown —
    /// a meter drawn at zero would be a claim we cannot support.
    pub usage_fraction: Option<f32>,
    /// Locally counted calls not yet reported, e.g. `"12 pending"`.
    pub pending: Option<SharedString>,
    /// Calls this client failed to report at all. Non-empty means we
    /// under-billed, and it is shown rather than logged (doctrine §8: a repair
    /// must be *visible*).
    pub dropped: Option<SharedString>,
    /// Whether the user is signed in enough to work.
    pub can_work: bool,
    /// Whether a sign-out button should be offered.
    ///
    /// False for an environment-supplied key: the app cannot unset a variable
    /// it did not set, and a button that silently does nothing is worse than no
    /// button.
    pub can_sign_out: bool,
    /// Whether this state needs a colour the reader cannot ignore.
    pub urgent: bool,
}

impl AccountPresentation {
    /// Derive the presentation from a posture and a usage report.
    ///
    /// A pure function of its inputs, in one place, so the status bar and the
    /// overlay cannot disagree about what state the account is in — and so the
    /// wording is testable without a window.
    pub fn derive(
        posture: &Posture,
        summary: Option<nudox_engine::mcp::AccountSummary>,
        hint: Option<(String, nudox_engine::mcp::KeySource)>,
        usage: &UsageReport,
    ) -> Self {
        use nudox_engine::mcp::account::state::QuotaKnowledge;

        let (key_hint, source) = match (&summary, &hint) {
            (Some(s), _) => (Some(s.key_hint.clone()), Some(s.source)),
            (None, Some((h, src))) => (Some(h.clone()), Some(*src)),
            (None, None) => (None, None),
        };

        let quota = match posture {
            Posture::Active { quota, .. } | Posture::GraceOffline { quota, .. } => match quota {
                QuotaKnowledge::Known(q) => Some(q.clone()),
                QuotaKnowledge::Unknown => None,
            },
            Posture::OverLimit { quota, .. } => Some(quota.clone()),
            _ => None,
        };

        let (segment, headline, detail, urgent, can_work) = match posture {
            Posture::SignedOut => (
                "account · signed out".to_owned(),
                "Sign in to nudox".to_owned(),
                "Paste the API key from your dashboard. Agent tool calls are refused until \
                 you do."
                    .to_owned(),
                false,
                false,
            ),
            Posture::StoreUnavailable { message, help } => (
                "account · locked".to_owned(),
                "Keychain unavailable".to_owned(),
                format!("{message} {help}"),
                true,
                false,
            ),
            Posture::AwaitingFirstVerification { .. } => (
                "account · checking".to_owned(),
                "Checking your account…".to_owned(),
                "Verifying this key with nudox. This usually takes a moment.".to_owned(),
                false,
                false,
            ),
            Posture::NeverVerified { cause, .. } => (
                "account · unverified".to_owned(),
                "This key has never been verified".to_owned(),
                format!(
                    "{cause} An account has to be confirmed online once before it can work \
                     offline."
                ),
                true,
                false,
            ),
            Posture::Active { account, .. } => (
                "account · signed in".to_owned(),
                "Signed in".to_owned(),
                format!("Verified as user {}. Everything is working.", account.user),
                false,
                true,
            ),
            Posture::GraceOffline { remaining, .. } => {
                let days = remaining.as_secs() / 86_400;
                let hours = (remaining.as_secs() % 86_400) / 3_600;
                let left = if days > 0 {
                    format!("{days}d {hours}h")
                } else {
                    format!("{hours}h")
                };
                (
                    format!("account · offline {left}"),
                    "Working offline".to_owned(),
                    format!(
                        "nudox cannot reach the licence service. Tool calls keep working for \
                         another {left}; connect once before then and the timer resets."
                    ),
                    *remaining <= GRACE_WARNING_AT,
                    true,
                )
            }
            Posture::GraceExpired { .. } => (
                "account · offline too long".to_owned(),
                "Offline too long".to_owned(),
                "The offline grace period has ended, so tool calls are paused. This is a \
                 connection problem, not a problem with your account — everything resumes the \
                 moment nudox can reach the licence service."
                    .to_owned(),
                true,
                false,
            ),
            Posture::Revoked { reason, .. } => (
                "account · key rejected".to_owned(),
                "Key rejected".to_owned(),
                format!("nudox says: {reason}. Create a new key in the dashboard and sign in again."),
                true,
                false,
            ),
            Posture::OverLimit { quota, .. } => (
                "account · limit reached".to_owned(),
                "Plan limit reached".to_owned(),
                format!(
                    "This account has used all {} calls on the {} plan for the current period. \
                     Nothing is broken — tool calls resume when the period rolls over, or \
                     immediately on a larger plan.",
                    quota.limit, quota.tier
                ),
                true,
                false,
            ),
        };

        Self {
            tag: posture.tag(),
            segment: SharedString::from(segment),
            headline: SharedString::from(headline),
            detail: SharedString::from(detail),
            key_hint: key_hint.map(SharedString::from),
            source: source.map(|s| SharedString::from(s.label())),
            user: summary.map(|s| SharedString::from(format!("user {}", s.user))),
            usage: quota.as_ref().map(|q| {
                SharedString::from(format!("{} of {} tool calls", q.tool_calls, q.limit))
            }),
            usage_fraction: quota.as_ref().and_then(|q| {
                (q.limit > 0).then(|| (q.used as f32 / q.limit as f32).clamp(0.0, 1.0))
            }),
            pending: (usage.pending > 0)
                .then(|| SharedString::from(format!("{} pending", usage.pending))),
            dropped: (usage.dropped.calls > 0).then(|| {
                SharedString::from(format!("{} unreported", usage.dropped.calls))
            }),
            can_work,
            can_sign_out: matches!(source, Some(nudox_engine::mcp::KeySource::Keychain))
                && !matches!(posture, Posture::SignedOut),
            urgent,
        }
    }
}

/// The process-wide account gate and its rendered status.
///
/// Installed by `main` with `cx.set_global`, beside [`crate::app::mcp::McpService`]
/// — and **before** it, because `McpService::start` needs the gate this owns.
pub struct AccountService {
    host: Option<AccountHost>,
    /// Derived from `host` at every refresh, never accumulated beside it
    /// (doctrine §8).
    status: AccountStatus,
}

impl Global for AccountService {}

impl AccountService {
    /// Start the account gate on the engine's runtime.
    ///
    /// Never returns an error. A lindsey that cannot reach `api.nudox.org` is
    /// still a working documentation browser for whatever its grace window
    /// allows, and refusing to open the window would turn a network blip into a
    /// product that will not launch. The posture says what is true, and the
    /// status bar shows it — which is what makes this defensible rather than a
    /// swallowed failure.
    pub fn start(engine: &EngineHandle) -> Self {
        match AccountHost::start(engine) {
            Ok(host) => {
                let status = Self::status_of(&host);
                Self {
                    host: Some(host),
                    status,
                }
            }
            Err(failure) => {
                tracing::error!(%failure, "account service could not be built");
                Self {
                    host: None,
                    status: AccountStatus::Absent,
                }
            }
        }
    }

    /// Start against a caller-supplied gate.
    ///
    /// This is what `tests/screenshots.rs` uses: it stands up a loopback fake
    /// of `api.nudox.org` and points a real gate at it, so the frames it
    /// records are of a real sign-in rather than of a hand-set flag. Doctrine
    /// §6 — never fabricate UI.
    pub fn start_with_gate(engine: &EngineHandle, gate: AccountGate) -> Self {
        let host = AccountHost::start_with_gate(engine, gate);
        let status = Self::status_of(&host);
        Self {
            host: Some(host),
            status,
        }
    }

    /// The current status. Cheap; clone it into a view.
    pub fn status(&self) -> &AccountStatus {
        &self.status
    }

    /// The gate, to hand to `McpService`.
    ///
    /// One gate, two readers: the MCP server admits tool calls through it and
    /// this service renders it. A second gate would be a second answer to
    /// "is this user signed in?".
    pub fn gate(&self) -> AccountGate {
        self.host
            .as_ref()
            .map(AccountHost::gate)
            .unwrap_or_else(|| {
                AccountGate::unmetered(
                    "no account host could be built in this process; the gate would deny \
                     every call with a message about a service that was never constructed",
                )
            })
    }

    /// Re-derive the status from the host.
    ///
    /// Called after every transition — sign-in, sign-out, refresh — and by the
    /// window's own poll. Not called from `render`: the whole point of
    /// [`AccountPresentation`] is that formatting happens on transitions.
    pub fn refresh(&mut self) -> AccountStatus {
        if let Some(host) = self.host.as_ref() {
            self.status = Self::status_of(host);
        }
        self.status.clone()
    }

    /// Verify and store `key`, then hand the outcome to `on_done`.
    ///
    /// `on_done` runs on the engine's runtime, not on the GPUI thread — the
    /// caller must hop back, which is what every other async result in this app
    /// already does through `crate::bridge`.
    pub fn sign_in<F>(&self, key: ApiKey, on_done: F)
    where
        F: FnOnce(Result<Posture, SignInFailure>) + Send + 'static,
    {
        match self.host.as_ref() {
            Some(host) => host.sign_in(key, on_done),
            None => on_done(Err(SignInFailure::NoService)),
        }
    }

    /// Forget the credential and every cached verdict.
    pub fn sign_out(&mut self) -> AccountStatus {
        if let Some(host) = self.host.as_ref()
            && let Err(e) = host.sign_out()
        {
            tracing::warn!(error = %e, "sign-out could not clear the credential store");
        }
        self.refresh()
    }

    /// Stop the background loops and flush any unreported usage.
    ///
    /// Called from `on_app_quit`. Without it, the user's last few tool calls
    /// sit in a file that the *next* launch has to reconcile — which works, but
    /// bills late and for no reason.
    pub fn stop(&mut self) {
        if let Some(host) = self.host.as_mut() {
            host.stop();
        }
    }

    fn status_of(host: &AccountHost) -> AccountStatus {
        AccountStatus::Live(AccountPresentation::derive(
            &host.posture(),
            host.account_summary(),
            host.credential_hint(),
            &host.usage(),
        ))
    }
}
