//! The offline/quota state machine: what is stored, what is derived, and every
//! posture named.
//!
//! # Stored state is small; posture is derived
//!
//! [`GateState`] has five variants. [`Posture`] — what the gate and the UI both
//! actually read — has nine. The extra four are *functions of the clock*
//! (`Active` vs `GraceOffline` vs `GraceExpired`) and of the last quota
//! observation (`OverLimit`), and they are computed from [`GateState`] on
//! demand rather than stored beside it.
//!
//! That is doctrine §8's counting rule applied to a state machine: a tally kept
//! in parallel with the thing it counts can drift from it, one computed from it
//! cannot. If `GateState` stored `GraceExpired` as its own variant, something
//! would have to notice the moment it became true — a timer, a tick, a
//! refresh — and every path that forgot to run that something would serve a
//! user whose grace ran out three days ago. Deriving it from `verified_at`
//! means the expiry happens whether or not anybody was watching.
//!
//! # The policy, in four numbers
//!
//! | Constant | Value | What it governs |
//! |---|---|---|
//! | [`FRESH_FOR`] | 12 h | how long a successful `authorize` is "current" before we are running on cached authority |
//! | [`GRACE_WINDOW`] | 7 days | how long past `verified_at` tool calls keep working with no reachable service |
//! | [`GRACE_WARNING_AT`] | 48 h remaining | when the UI escalates from a quiet badge to a warning the user cannot miss |
//! | [`NEAR_LIMIT_MARGIN`] | 25 calls | how close to the quota we get before usage is flushed one call at a time |
//!
//! ## Why seven days
//!
//! Commercial practice clusters into two bands, and this sits deliberately
//! between them.
//!
//! *The short band* is floating-licence keep-alive: JetBrains' on-prem License
//! Server gives clients a **48-hour** grace so a maintenance window does not
//! strand everyone, and its newer License Vault reclaims a seat within ~20–30
//! minutes of a client going quiet
//! (<https://www.jetbrains.com/help/ide-services/floating-licenses.html>).
//! Those numbers are about *reclaiming a shared seat*, which is not our
//! problem — an `ndx_` key is one user's.
//!
//! *The long band* is consumer subscription tolerance: Adobe's desktop apps
//! revalidate roughly monthly and allow 30 days offline for month-to-month
//! members, with annual members getting a further 99 days
//! (<https://helpx.adobe.com/creative-cloud/kb/internet-connection-creative-cloud-apps.html>),
//! and JetBrains gives a lapsed subscription a **one-week** grace with an
//! in-IDE notice at every launch. Those are about not punishing a billing
//! hiccup.
//!
//! Ours is neither: it is "this developer is on a plane / at a conference / on
//! a locked-down network". Seven days covers every real instance of that with
//! room to spare — the longest flight is under 20 hours — while keeping the
//! licence meaningful, which a 30-day window would not. It also matches
//! JetBrains' subscription grace, the closest comparable single-seat number.
//!
//! The anti-pattern we are avoiding is Tailscale's: node keys expire after 180
//! days with **no** grace and, by the reports of the people it strands, no
//! advance warning — unattended devices simply stop working
//! (<https://tailscale.com/kb/1028/key-expiry>,
//! <https://github.com/tailscale/tailscale/issues/19785>). A long window with
//! no warning is worse than a short window with one, which is why
//! [`GRACE_WARNING_AT`] exists and why [`Posture::GraceOffline`] carries the
//! remaining time rather than a bare flag.
//!
//! # Client state is advisory; the server is the authority
//!
//! Everything here is a cache of decisions `api.nudox.org` made. A user with a
//! text editor can set `verified_at` forward and extend their own grace; a user
//! with a debugger can do considerably better than that. This module does the
//! two cheap things that stop *accident* rather than *intent* — a cached
//! verdict is bound to the fingerprint of the key that earned it, so it cannot
//! be transplanted onto a different key, and `verified_at` is clamped to "not
//! in the future", so a clock skew does not silently mint grace — and does not
//! pretend to be tamper-proof. Metering is enforced server-side or it is not
//! enforced.

use std::time::{Duration, SystemTime};

use super::credential::KeyFingerprint;
use super::store::KeySource;

/// How long a successful `POST v1/authorize` stays "current".
///
/// Past this the client is running on cached authority and says so. Twelve
/// hours means a machine used every working day re-verifies about once per day
/// without the client ever becoming chatty: one request per half-day, against
/// an endpoint whose whole job is to answer that question.
pub const FRESH_FOR: Duration = Duration::from_hours(12);

/// How long past the last successful verification tool calls keep working with
/// no reachable service. See the module docs for why this number.
pub const GRACE_WINDOW: Duration = Duration::from_hours(7 * 24);

/// How much grace must remain before the UI escalates.
///
/// Two days is enough that a warning seen on a Friday is still actionable on
/// Monday. The point of the constant is that the user learns *before* being cut
/// off, not after — the failure mode this whole module is shaped around.
pub const GRACE_WARNING_AT: Duration = Duration::from_hours(48);

/// How close to the quota limit we get before usage stops being batched.
///
/// Batching is what keeps `POST v1/usage/record` off the request path, and its
/// cost is that the over-limit boundary is blurred by up to one batch. Below
/// this many remaining calls the batch size drops to one, so the user learns
/// they are over within a single tool call instead of up to a full batch later.
/// See [`super::ledger`].
pub const NEAR_LIMIT_MARGIN: u64 = 25;

// ---------------------------------------------------------------------------
// Small vocabulary types
// ---------------------------------------------------------------------------

/// The account id `POST v1/authorize` returns.
///
/// A newtype rather than a bare `u64` (doctrine §3) because it travels beside
/// quota counts, batch counts, and epoch seconds, all of which are also
/// integers and none of which it may be confused with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct UserId(pub u64);

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A `GET v1/usage` response, plus when we saw it.
///
/// `tier` and `period_start` are carried as the service spelled them. This
/// crate does not parse timestamps — it has no date library and needs none, and
/// re-rendering a server's own string is how a client ends up displaying a
/// different date than the dashboard.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuotaSnapshot {
    /// The plan name, e.g. `"free"`.
    pub tier: String,
    /// ISO-8601 start of the current billing period, verbatim from the service.
    ///
    /// There is deliberately no `period_end` here because the contract does not
    /// provide one — see `docs/auth.md` § "Gaps in the contract", gap 1. Its absence
    /// is why every over-limit message says "when the period rolls over" rather
    /// than naming a date.
    pub period_start: String,
    /// Billable API requests used this period.
    pub api_requests: u64,
    /// Billable tool calls used this period. The number this client moves.
    pub tool_calls: u64,
    /// Total units used, as the service computes it.
    pub used: u64,
    /// The plan's ceiling.
    pub limit: u64,
    /// `limit - used`, as the service computes it — not recomputed here, so a
    /// service that counts differently is visible rather than papered over.
    pub remaining: u64,
    /// The service's own verdict. Authoritative; never inferred from `used`
    /// and `limit`, which can disagree with it during a plan change.
    pub over_limit: bool,
    /// When this client received it.
    pub observed_at: SystemTime,
}

impl QuotaSnapshot {
    /// Whether the remaining allowance is small enough that batching should
    /// stop. See [`NEAR_LIMIT_MARGIN`].
    pub fn is_near_limit(&self) -> bool {
        self.over_limit || self.remaining <= NEAR_LIMIT_MARGIN
    }
}

/// What we know about quota for the signed-in account.
///
/// `Unknown` is a real state, not a missing value: `POST v1/authorize` does not
/// return usage, so between signing in and the first `GET v1/usage` we
/// genuinely do not know. Rendering that as `0 / 0` would be a fabrication.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QuotaKnowledge {
    /// No `GET v1/usage` has succeeded for this account yet.
    Unknown,
    /// The most recent snapshot.
    Known(QuotaSnapshot),
}

/// Why the service could not be reached, in the only two categories that
/// change what a caller may do.
///
/// This distinction is load-bearing for [`super::ledger`] and is the single
/// most important type in this module: it is what decides whether a usage batch
/// may be retried or must be dropped. See `docs/auth.md` § "At-most-once, and why".
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProbeFailure {
    /// The request provably never reached the service: DNS failure, connection
    /// refused, TLS handshake failure, no route. Retrying is safe because
    /// nothing happened.
    NotDelivered {
        /// The transport's own description, for diagnostics.
        detail: String,
    },
    /// The request may or may not have been processed: a timeout after the
    /// request was written, a connection reset mid-response, a 5xx. Retrying is
    /// **not** safe, because the service has no idempotency key to recognise
    /// the retry with.
    Indeterminate {
        /// The transport's own description, for diagnostics.
        detail: String,
    },
    /// The service answered, but not in a shape this client understands.
    ///
    /// Kept separate from the two above because it is *our* bug or a deployed
    /// contract change, not a network condition, and it must not silently look
    /// like weather.
    MalformedResponse {
        /// What could not be parsed. Never contains a credential.
        detail: String,
    },
}

impl ProbeFailure {
    /// Whether a request that failed this way may be sent again.
    ///
    /// Only [`ProbeFailure::NotDelivered`] is safely retryable. This is the
    /// whole of the retry policy, in one place, deliberately.
    pub fn is_safely_retryable(&self) -> bool {
        matches!(self, Self::NotDelivered { .. })
    }

    /// The transport's own description.
    pub fn detail(&self) -> &str {
        match self {
            Self::NotDelivered { detail }
            | Self::Indeterminate { detail }
            | Self::MalformedResponse { detail } => detail,
        }
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotDelivered { detail } => write!(f, "could not reach the service: {detail}"),
            Self::Indeterminate { detail } => {
                write!(f, "the service did not answer in time: {detail}")
            }
            Self::MalformedResponse { detail } => {
                write!(f, "the service answered unexpectedly: {detail}")
            }
        }
    }
}

/// Whether a verification attempt is outstanding or has failed.
///
/// The difference matters at exactly one moment — the first seconds after
/// launch, before the background `authorize` has answered. "Wait a moment" and
/// "you are offline" are different things to tell an agent, and collapsing them
/// makes the first launch after a sign-in look broken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    /// A verification is in flight, or is about to be.
    Pending,
    /// The last attempt failed this way.
    Failed(ProbeFailure),
}

/// A verified account: who, which key, and when the service last said yes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    /// The id `POST v1/authorize` returned.
    pub user: UserId,
    /// Fingerprint of the key that earned this verdict.
    ///
    /// A cached verdict is only honoured for the key it names, so swapping the
    /// key in the keychain cannot inherit the previous key's grace.
    pub fingerprint: KeyFingerprint,
    /// When the service last answered `allowed: true`.
    ///
    /// Every freshness and grace computation is a function of this. Clamped to
    /// "not in the future" on ingest — see [`GateState::verified`].
    pub verified_at: SystemTime,
}

// ---------------------------------------------------------------------------
// GateState — what is stored
// ---------------------------------------------------------------------------

/// The persisted half of the account gate.
///
/// Five variants, exhaustive on purpose (doctrine §3's in-workspace rule): a
/// sixth must break [`GateState::posture`] and every other match, so the person
/// adding it has to say what the new state means to a tool call, to the UI, and
/// to the cache.
///
/// [`KeySource`] rides along on the two states that have a credential because
/// "sign out" is available for one source and not the other, and a UI that
/// cannot tell them apart offers a button that does nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateState {
    /// No credential in the environment and none in the store, and the store
    /// answered successfully. This machine has never been signed in.
    SignedOut,

    /// The credential store could not be consulted, so we do not know whether a
    /// key exists.
    ///
    /// Distinct from [`GateState::SignedOut`] because the advice is different
    /// and because collapsing them is the documented `gh` defect
    /// (<https://github.com/cli/cli/issues/13317>) — see [`super::store`].
    StoreUnavailable {
        /// The store's rendering of the failure.
        message: String,
        /// What to do about it.
        help: &'static str,
    },

    /// A credential is present and the service has never confirmed it — either
    /// because we have not asked yet, or because we could not.
    Unverified {
        /// Which key.
        fingerprint: KeyFingerprint,
        /// Where it came from.
        source: KeySource,
        /// Whether a verification is outstanding or has failed.
        probe: ProbeStatus,
    },

    /// The service has confirmed this credential at least once.
    ///
    /// Whether that confirmation is still *current* is not stored — it is
    /// derived from `account.verified_at` and the clock by
    /// [`GateState::posture`]. That is what makes grace expire on its own.
    Authorized {
        /// Who, which key, when.
        account: Account,
        /// Where the key came from.
        source: KeySource,
        /// What we last knew about usage.
        quota: QuotaKnowledge,
        /// The most recent verification attempt, if it did not succeed. `None`
        /// while everything is working.
        last_failure: Option<ProbeFailure>,
    },

    /// The service answered `allowed: false`.
    ///
    /// **Not subject to grace.** A revocation is an *answer*, not a failure to
    /// get one, and extending offline tolerance to it would mean a key revoked
    /// because it leaked keeps working for a week. Sticky until the credential
    /// changes.
    Revoked {
        /// Which key was rejected.
        fingerprint: KeyFingerprint,
        /// The service's own `reason` string, shown verbatim.
        ///
        /// Verbatim because the contract gives no machine-readable code to
        /// branch on — see `docs/auth.md` § "Gaps in the contract", gap 4.
        reason: String,
        /// When it said so.
        at: SystemTime,
    },
}

impl GateState {
    /// The state a process starts in before anything has been read.
    pub fn initial() -> Self {
        Self::SignedOut
    }

    /// Build the `Authorized` state from a fresh `allowed: true`.
    ///
    /// `verified_at` is clamped to `now`: a service or a host clock that
    /// reports the future would otherwise mint grace out of nothing, and a
    /// clamp is cheaper than trusting either.
    pub fn verified(
        user: UserId,
        fingerprint: KeyFingerprint,
        source: KeySource,
        verified_at: SystemTime,
        now: SystemTime,
        quota: QuotaKnowledge,
    ) -> Self {
        let clamped = if verified_at > now { now } else { verified_at };
        Self::Authorized {
            account: Account {
                user,
                fingerprint,
                verified_at: clamped,
            },
            source,
            quota,
            last_failure: None,
        }
    }

    /// The fingerprint of the credential this state is about, if any.
    ///
    /// Used to notice that the credential changed underneath us, which
    /// invalidates every cached verdict.
    pub fn fingerprint(&self) -> Option<&KeyFingerprint> {
        match self {
            Self::SignedOut | Self::StoreUnavailable { .. } => None,
            Self::Unverified { fingerprint, .. } | Self::Revoked { fingerprint, .. } => {
                Some(fingerprint)
            }
            Self::Authorized { account, .. } => Some(&account.fingerprint),
        }
    }

    /// The nine-way posture this state has *at this instant*.
    ///
    /// Total, exhaustive, and the only thing callers should branch on. See the
    /// module docs for why the clock-dependent postures are computed here
    /// rather than stored.
    ///
    /// # Precedence inside `Authorized`
    ///
    /// `GraceExpired` → `OverLimit` → `GraceOffline` → `Active`.
    ///
    /// Expiry outranks over-limit because once grace has lapsed our quota
    /// knowledge is at least a week stale, and reporting a stale
    /// `over_limit: true` as the reason would send the user to the billing page
    /// to fix a network problem. Over-limit outranks `GraceOffline` because it
    /// is the more specific and more actionable of the two.
    pub fn posture(&self, now: SystemTime) -> Posture {
        match self {
            Self::SignedOut => Posture::SignedOut,

            Self::StoreUnavailable { message, help } => Posture::StoreUnavailable {
                message: message.clone(),
                help,
            },

            Self::Unverified {
                fingerprint,
                source,
                probe: ProbeStatus::Pending,
            } => Posture::AwaitingFirstVerification {
                fingerprint: fingerprint.clone(),
                source: *source,
            },

            Self::Unverified {
                fingerprint,
                source,
                probe: ProbeStatus::Failed(cause),
            } => Posture::NeverVerified {
                fingerprint: fingerprint.clone(),
                source: *source,
                cause: cause.clone(),
            },

            Self::Revoked {
                fingerprint,
                reason,
                at,
            } => Posture::Revoked {
                fingerprint: fingerprint.clone(),
                reason: reason.clone(),
                at: *at,
            },

            Self::Authorized {
                account,
                source,
                quota,
                last_failure,
            } => {
                let age = now
                    .duration_since(account.verified_at)
                    // A host clock that moved *backwards* makes `verified_at`
                    // look like the future. Treating that as "zero age" is the
                    // conservative reading: it keeps the user working and lets
                    // the next successful probe re-anchor the timestamp. It
                    // cannot be used to extend grace, because grace is measured
                    // forward from `verified_at`, which the clamp in
                    // `verified` already bounds.
                    .unwrap_or(Duration::ZERO);

                if age >= GRACE_WINDOW {
                    return Posture::GraceExpired {
                        account: account.clone(),
                        source: *source,
                        expired_for: age.checked_sub(GRACE_WINDOW).unwrap(),
                        cause: last_failure.clone(),
                    };
                }

                if let QuotaKnowledge::Known(snapshot) = quota
                    && snapshot.over_limit
                {
                    return Posture::OverLimit {
                        account: account.clone(),
                        source: *source,
                        quota: snapshot.clone(),
                    };
                }

                if age >= FRESH_FOR {
                    return Posture::GraceOffline {
                        account: account.clone(),
                        source: *source,
                        quota: quota.clone(),
                        remaining: GRACE_WINDOW.checked_sub(age).unwrap(),
                        cause: last_failure.clone(),
                    };
                }

                Posture::Active {
                    account: account.clone(),
                    source: *source,
                    quota: quota.clone(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Posture — every state, named
// ---------------------------------------------------------------------------

/// Every state the account gate can be in, as the gate and the UI see it.
///
/// Nine variants, exhaustive. This is the vocabulary the whole feature is
/// written in: [`Posture::verdict`] turns it into an allow/deny for a tool
/// call, and `lindsey`'s account view renders one row per variant. A tenth
/// variant breaks both, which is the point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Posture {
    /// No credential on this machine. The user has never signed in here.
    SignedOut,

    /// The credential store could not be read, so whether a key exists is
    /// unknown. Never rendered as "signed out".
    StoreUnavailable {
        /// The store's rendering of the failure.
        message: String,
        /// What to do about it.
        help: &'static str,
    },

    /// A key is present and its first verification is in flight.
    ///
    /// Lives for the second or so between launch and the first `authorize`
    /// response on a machine with no cached verdict. Denies tool calls, but
    /// says "retry in a moment" rather than "you are offline".
    AwaitingFirstVerification {
        /// Which key.
        fingerprint: KeyFingerprint,
        /// Where it came from.
        source: KeySource,
    },

    /// A key is present, and the service has never once confirmed it.
    ///
    /// The honest hard stop: an account relationship cannot be established
    /// offline, because nothing on this machine knows whether the key is real.
    NeverVerified {
        /// Which key.
        fingerprint: KeyFingerprint,
        /// Where it came from.
        source: KeySource,
        /// Why the attempt failed.
        cause: ProbeFailure,
    },

    /// Verified within [`FRESH_FOR`] and under quota. The normal state.
    Active {
        /// Who, which key, when.
        account: Account,
        /// Where the key came from.
        source: KeySource,
        /// What we last knew about usage.
        quota: QuotaKnowledge,
    },

    /// Verified, but not recently, and re-verification is not succeeding.
    ///
    /// Tool calls proceed. The UI shows the remaining window, and escalates
    /// once it drops below [`GRACE_WARNING_AT`].
    GraceOffline {
        /// Who, which key, when.
        account: Account,
        /// Where the key came from.
        source: KeySource,
        /// What we last knew about usage. Stale by construction.
        quota: QuotaKnowledge,
        /// How much of [`GRACE_WINDOW`] is left.
        remaining: Duration,
        /// Why re-verification is failing, when we have tried and know.
        cause: Option<ProbeFailure>,
    },

    /// [`GRACE_WINDOW`] elapsed without a successful re-verification.
    ///
    /// Tool calls stop. Recoverable the instant the service is reachable again
    /// — this is not a revocation and must never be worded like one.
    GraceExpired {
        /// Who, which key, when it was last verified.
        account: Account,
        /// Where the key came from.
        source: KeySource,
        /// How long ago the window closed.
        expired_for: Duration,
        /// Why re-verification is failing, when we have tried and know.
        cause: Option<ProbeFailure>,
    },

    /// The service answered `allowed: false`. Sticky until the key changes.
    Revoked {
        /// Which key was rejected.
        fingerprint: KeyFingerprint,
        /// The service's own reason, verbatim.
        reason: String,
        /// When it said so.
        at: SystemTime,
    },

    /// The account is over its plan limit for the current period.
    ///
    /// A product state, not an error: nothing is broken, the plan ran out.
    OverLimit {
        /// Who, which key, when.
        account: Account,
        /// Where the key came from.
        source: KeySource,
        /// The snapshot that says so.
        quota: QuotaSnapshot,
    },
}

impl Posture {
    /// Whether a billable tool call may proceed, and if not, why.
    ///
    /// Total over [`Posture`] with no wildcard arm, so a new posture cannot
    /// default into "allow" — which is the direction a paid product must never
    /// fail.
    pub fn verdict(&self) -> Verdict {
        match self {
            Self::Active { .. } | Self::GraceOffline { .. } => Verdict::Allow,
            Self::SignedOut => Verdict::Deny(Denial::NotSignedIn),
            Self::StoreUnavailable { message, help } => Verdict::Deny(Denial::StoreUnavailable {
                message: message.clone(),
                help,
            }),
            Self::AwaitingFirstVerification { .. } => Verdict::Deny(Denial::VerificationPending),
            Self::NeverVerified { cause, .. } => Verdict::Deny(Denial::NeverVerified {
                cause: cause.clone(),
            }),
            Self::GraceExpired {
                account,
                expired_for,
                cause,
                ..
            } => Verdict::Deny(Denial::GraceExpired {
                last_verified_at: account.verified_at,
                expired_for: *expired_for,
                cause: cause.clone(),
            }),
            Self::Revoked { reason, .. } => Verdict::Deny(Denial::Revoked {
                reason: reason.clone(),
            }),
            Self::OverLimit { quota, .. } => Verdict::Deny(Denial::OverLimit {
                quota: quota.clone(),
            }),
        }
    }

    /// Whether the user should be shown a warning they cannot dismiss casually.
    ///
    /// True while grace is running low, so the escalation happens *before* the
    /// cut-off rather than at it. This is the whole difference between this
    /// design and the Tailscale expiry the module docs cite.
    pub fn needs_urgent_attention(&self) -> bool {
        match self {
            Self::GraceOffline { remaining, .. } => *remaining <= GRACE_WARNING_AT,
            Self::GraceExpired { .. }
            | Self::Revoked { .. }
            | Self::OverLimit { .. }
            | Self::NeverVerified { .. }
            | Self::StoreUnavailable { .. } => true,
            Self::Active { .. } | Self::SignedOut | Self::AwaitingFirstVerification { .. } => false,
        }
    }

    /// A stable machine tag, for tests, logs, and the status line.
    ///
    /// Distinct from the `Display` prose, which can be reworded, and from
    /// [`Denial::kind`], which only exists for the postures that deny.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::SignedOut => "signed_out",
            Self::StoreUnavailable { .. } => "store_unavailable",
            Self::AwaitingFirstVerification { .. } => "awaiting_first_verification",
            Self::NeverVerified { .. } => "never_verified",
            Self::Active { .. } => "active",
            Self::GraceOffline { .. } => "grace_offline",
            Self::GraceExpired { .. } => "grace_expired",
            Self::Revoked { .. } => "revoked",
            Self::OverLimit { .. } => "over_limit",
        }
    }
}

/// Whether a tool call proceeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The call is billable and may run.
    Allow,
    /// The call must not run, for this reason.
    Deny(Denial),
}

/// Why a tool call was refused.
///
/// Each variant carries what an agent and a human need to act, and each maps to
/// exactly one [`crate::mcp::error::McpError`] variant with its own `kind` and
/// `help`. Exhaustive so that mapping cannot acquire a `_` arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Denial {
    /// No credential is configured.
    NotSignedIn,
    /// The credential store could not be read.
    StoreUnavailable {
        /// The store's rendering of the failure.
        message: String,
        /// What to do about it.
        help: &'static str,
    },
    /// A first verification is in flight; the caller may retry shortly.
    VerificationPending,
    /// A key exists but has never been confirmed, and cannot be right now.
    NeverVerified {
        /// Why the attempt failed.
        cause: ProbeFailure,
    },
    /// Offline for longer than [`GRACE_WINDOW`].
    GraceExpired {
        /// When the service last said yes.
        last_verified_at: SystemTime,
        /// How long ago the window closed.
        expired_for: Duration,
        /// Why re-verification is failing, when we know.
        cause: Option<ProbeFailure>,
    },
    /// The service rejected the key.
    Revoked {
        /// The service's own reason, verbatim.
        reason: String,
    },
    /// The account is over its plan limit.
    OverLimit {
        /// The snapshot that says so.
        quota: QuotaSnapshot,
    },
}

impl Denial {
    /// The stable machine tag an agent branches on.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotSignedIn => "not_signed_in",
            Self::StoreUnavailable { .. } => "credential_store_unavailable",
            Self::VerificationPending => "authorization_pending",
            Self::NeverVerified { .. } => "never_verified",
            Self::GraceExpired { .. } => "offline_grace_expired",
            Self::Revoked { .. } => "key_revoked",
            Self::OverLimit { .. } => "quota_exceeded",
        }
    }

    /// Whether retrying the identical call could succeed without anything
    /// changing.
    ///
    /// Only [`Denial::VerificationPending`] qualifies. Everything else needs a
    /// human, a network, or a calendar — and telling an agent to retry into a
    /// quota wall is how a rate limit becomes a denial-of-service against our
    /// own API.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::VerificationPending)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fp() -> KeyFingerprint {
        super::super::credential::ApiKey::parse("ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517")
            .expect("sample parses")
            .fingerprint()
    }

    fn epoch(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// A fixed instant well clear of the epoch, so subtracting a week from it
    /// is still a valid `SystemTime` on every platform.
    fn t0() -> SystemTime {
        epoch(1_800_000_000)
    }

    fn authorized_at(verified_at: SystemTime, quota: QuotaKnowledge) -> GateState {
        GateState::Authorized {
            account: Account {
                user: UserId(24),
                fingerprint: fp(),
                verified_at,
            },
            source: KeySource::Keychain,
            quota,
            last_failure: None,
        }
    }

    fn snapshot(used: u64, limit: u64, over: bool) -> QuotaSnapshot {
        QuotaSnapshot {
            tier: "free".to_owned(),
            period_start: "2026-08-01T00:00:00Z".to_owned(),
            api_requests: 300,
            tool_calls: used.saturating_sub(300),
            used,
            limit,
            remaining: limit.saturating_sub(used),
            over_limit: over,
            observed_at: t0(),
        }
    }

    #[test]
    fn a_fresh_verification_is_active_and_allows_tool_calls() {
        let state = authorized_at(t0(), QuotaKnowledge::Unknown);
        let posture = state.posture(t0() + Duration::from_secs(60));
        assert_eq!(posture.tag(), "active");
        assert_eq!(posture.verdict(), Verdict::Allow);
        assert!(!posture.needs_urgent_attention());
    }

    #[test]
    fn grace_begins_at_twelve_hours_and_ends_at_seven_days() {
        // The three boundaries in one place, asserted on both sides. A test
        // that only checked the middle of each band would pass against a
        // constant that was off by an hour.
        let state = authorized_at(t0(), QuotaKnowledge::Unknown);

        let just_fresh = state.posture(t0() + FRESH_FOR - Duration::from_secs(1));
        assert_eq!(just_fresh.tag(), "active");

        let just_stale = state.posture(t0() + FRESH_FOR);
        assert_eq!(just_stale.tag(), "grace_offline");
        assert_eq!(
            just_stale.verdict(),
            Verdict::Allow,
            "grace must keep working — a developer on a plane is the normal case"
        );

        let last_moment = state.posture(t0() + GRACE_WINDOW - Duration::from_secs(1));
        assert_eq!(last_moment.tag(), "grace_offline");
        assert_eq!(last_moment.verdict(), Verdict::Allow);

        let expired = state.posture(t0() + GRACE_WINDOW);
        assert_eq!(expired.tag(), "grace_expired");
        assert!(matches!(
            expired.verdict(),
            Verdict::Deny(Denial::GraceExpired { .. })
        ));
    }

    #[test]
    fn grace_reports_the_time_remaining_and_escalates_before_the_cutoff() {
        // The Tailscale failure this design is shaped against: a user must be
        // warned *before* the cut-off, not discover it at the cut-off.
        let state = authorized_at(t0(), QuotaKnowledge::Unknown);

        let early = state.posture(t0() + FRESH_FOR);
        match &early {
            Posture::GraceOffline { remaining, .. } => {
                assert_eq!(*remaining, GRACE_WINDOW.checked_sub(FRESH_FOR).unwrap());
            }
            other => panic!("expected grace, got {other:?}"),
        }
        assert!(
            !early.needs_urgent_attention(),
            "an hour into a week-long window is not an emergency"
        );

        let late = state.posture(t0() + GRACE_WINDOW - GRACE_WARNING_AT);
        assert!(
            late.needs_urgent_attention(),
            "with {GRACE_WARNING_AT:?} left the user must be told while they can still act"
        );
        assert_eq!(
            late.verdict(),
            Verdict::Allow,
            "the warning must precede the cut-off, not coincide with it"
        );
    }

    #[test]
    fn expiry_outranks_a_stale_over_limit_reading() {
        // Once grace has lapsed our quota knowledge is at least a week old.
        // Reporting `quota_exceeded` then would send the user to the billing
        // page to fix a network outage.
        let state = authorized_at(t0(), QuotaKnowledge::Known(snapshot(1000, 1000, true)));
        let posture = state.posture(t0() + GRACE_WINDOW);
        assert_eq!(posture.tag(), "grace_expired");
        match posture.verdict() {
            Verdict::Deny(d) => assert_eq!(d.kind(), "offline_grace_expired"),
            other @ Verdict::Allow => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn over_limit_outranks_grace_while_the_window_is_open() {
        let state = authorized_at(t0(), QuotaKnowledge::Known(snapshot(1000, 1000, true)));
        let posture = state.posture(t0() + FRESH_FOR + Duration::from_secs(1));
        assert_eq!(posture.tag(), "over_limit");
        match posture.verdict() {
            Verdict::Deny(Denial::OverLimit { quota }) => {
                assert_eq!(quota.used, 1000);
                assert_eq!(quota.limit, 1000);
                assert_eq!(quota.remaining, 0);
            }
            other => panic!("expected an over-limit denial carrying the numbers, got {other:?}"),
        }
    }

    #[test]
    fn over_limit_is_the_services_verdict_not_a_local_comparison() {
        // `used >= limit` and `over_limit` can disagree during a plan change.
        // The service's own flag is what counts, in both directions.
        let under_but_flagged =
            authorized_at(t0(), QuotaKnowledge::Known(snapshot(10, 1000, true)));
        assert_eq!(under_but_flagged.posture(t0()).tag(), "over_limit");

        let at_limit_but_not_flagged =
            authorized_at(t0(), QuotaKnowledge::Known(snapshot(1000, 1000, false)));
        assert_eq!(at_limit_but_not_flagged.posture(t0()).tag(), "active");
    }

    #[test]
    fn a_revocation_is_never_softened_by_grace() {
        // The security property: a key revoked because it leaked must stop
        // working now, not in seven days.
        let state = GateState::Revoked {
            fingerprint: fp(),
            reason: "invalid token".to_owned(),
            at: t0(),
        };
        for offset in [Duration::ZERO, FRESH_FOR, GRACE_WINDOW * 2] {
            let posture = state.posture(t0() + offset);
            assert_eq!(posture.tag(), "revoked");
            match posture.verdict() {
                Verdict::Deny(Denial::Revoked { reason }) => {
                    assert_eq!(reason, "invalid token", "the service's own words, verbatim");
                }
                other => panic!("a revoked key must always deny, got {other:?}"),
            }
        }
    }

    #[test]
    fn pending_and_failed_first_verification_are_different_answers() {
        let pending = GateState::Unverified {
            fingerprint: fp(),
            source: KeySource::Keychain,
            probe: ProbeStatus::Pending,
        };
        let failed = GateState::Unverified {
            fingerprint: fp(),
            source: KeySource::Environment,
            probe: ProbeStatus::Failed(ProbeFailure::NotDelivered {
                detail: "dns error".to_owned(),
            }),
        };

        let pending_denial = match pending.posture(t0()).verdict() {
            Verdict::Deny(d) => d,
            other @ Verdict::Allow => panic!("expected denial, got {other:?}"),
        };
        let failed_denial = match failed.posture(t0()).verdict() {
            Verdict::Deny(d) => d,
            other @ Verdict::Allow => panic!("expected denial, got {other:?}"),
        };

        assert_eq!(pending_denial.kind(), "authorization_pending");
        assert!(
            pending_denial.is_retryable(),
            "the first second after launch must tell an agent to try again, not that it is offline"
        );
        assert_eq!(failed_denial.kind(), "never_verified");
        assert!(!failed_denial.is_retryable());
    }

    #[test]
    fn signed_out_and_store_unavailable_never_collapse() {
        let out = GateState::SignedOut.posture(t0());
        let broken = GateState::StoreUnavailable {
            message: "the macOS Keychain is locked".to_owned(),
            help: "Unlock your login keychain and try again.",
        }
        .posture(t0());

        assert_eq!(out.tag(), "signed_out");
        assert_eq!(broken.tag(), "store_unavailable");
        match (out.verdict(), broken.verdict()) {
            (Verdict::Deny(a), Verdict::Deny(b)) => {
                assert_ne!(a.kind(), b.kind(), "cli/cli#13317 in one assertion");
            }
            other => panic!("both must deny, got {other:?}"),
        }
    }

    #[test]
    fn a_backwards_clock_cannot_be_used_to_extend_grace() {
        // `verified_at` in the future is clamped on ingest, so the only way to
        // reach a future timestamp is a host clock that moved backwards after
        // the fact. That reads as zero age — the user keeps working — and the
        // clamp is what stops it becoming a way to mint grace.
        let future = GateState::verified(
            UserId(24),
            fp(),
            KeySource::Keychain,
            t0() + Duration::from_hours(24),
            t0(),
            QuotaKnowledge::Unknown,
        );
        match &future {
            GateState::Authorized { account, .. } => assert_eq!(
                account.verified_at,
                t0(),
                "a verification timestamp in the future must be clamped to now"
            ),
            other => panic!("expected Authorized, got {other:?}"),
        }
        assert_eq!(future.posture(t0()).tag(), "active");
        assert_eq!(
            future.posture(t0() + GRACE_WINDOW).tag(),
            "grace_expired",
            "the clamp must not have moved the expiry out by a day"
        );
    }

    #[test]
    fn every_posture_has_a_distinct_tag_and_denials_have_distinct_kinds() {
        // Doctrine §3: the machine tag is the contract. Two postures sharing
        // one tag would make the UI and an agent unable to tell them apart,
        // which is the defect L35 records for `Option<SharedString>`.
        let postures = [
            GateState::SignedOut.posture(t0()),
            GateState::StoreUnavailable {
                message: String::new(),
                help: "",
            }
            .posture(t0()),
            GateState::Unverified {
                fingerprint: fp(),
                source: KeySource::Keychain,
                probe: ProbeStatus::Pending,
            }
            .posture(t0()),
            GateState::Unverified {
                fingerprint: fp(),
                source: KeySource::Keychain,
                probe: ProbeStatus::Failed(ProbeFailure::NotDelivered {
                    detail: String::new(),
                }),
            }
            .posture(t0()),
            authorized_at(t0(), QuotaKnowledge::Unknown).posture(t0()),
            authorized_at(t0(), QuotaKnowledge::Unknown).posture(t0() + FRESH_FOR),
            authorized_at(t0(), QuotaKnowledge::Unknown).posture(t0() + GRACE_WINDOW),
            GateState::Revoked {
                fingerprint: fp(),
                reason: String::new(),
                at: t0(),
            }
            .posture(t0()),
            authorized_at(t0(), QuotaKnowledge::Known(snapshot(1, 1, true))).posture(t0()),
        ];

        let mut tags: Vec<&str> = postures.iter().map(Posture::tag).collect();
        assert_eq!(tags.len(), 9, "every posture must be represented here");
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), 9, "two postures share a tag: {tags:?}");

        let mut kinds: Vec<&str> = postures
            .iter()
            .filter_map(|p| match p.verdict() {
                Verdict::Deny(d) => Some(d.kind()),
                Verdict::Allow => None,
            })
            .collect();
        assert_eq!(kinds.len(), 7, "seven of the nine postures deny");
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), 7, "two denials share a kind: {kinds:?}");
    }

    #[test]
    fn only_a_not_delivered_failure_may_be_retried() {
        // The single decision the usage ledger's correctness rests on.
        assert!(
            ProbeFailure::NotDelivered {
                detail: "connection refused".to_owned()
            }
            .is_safely_retryable()
        );
        assert!(
            !ProbeFailure::Indeterminate {
                detail: "timed out reading response".to_owned()
            }
            .is_safely_retryable(),
            "a timeout after the request was written may have been processed; \
             retrying it double-bills the user"
        );
        assert!(
            !ProbeFailure::MalformedResponse {
                detail: "expected 204, got 200".to_owned()
            }
            .is_safely_retryable()
        );
    }
}
