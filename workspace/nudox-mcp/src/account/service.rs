//! The seam between this client and `api.nudox.org`, and the only place that
//! knows the wire format of the three endpoints.
//!
//! # Why a trait
//!
//! Doctrine §4: a test that depends on a live paid service is not a test. So
//! [`AccountService`] is the seam, [`HttpAccountService`] is the one
//! implementation that speaks HTTP, and the suite points it at a **real local
//! HTTP server** it controls (`tests/support/fake_service.rs`) rather than
//! substituting a Rust-level stub.
//!
//! That choice matters. A hand-written `impl AccountService` that returns
//! `RecordOutcome::OverLimit` proves the state machine reacts to over-limit; it
//! proves nothing about whether we recognise a 429, because no 429 ever
//! existed. The local fake exercises status-code mapping, JSON decoding, header
//! construction and timeout classification — all of which have been wrong in
//! somebody's client at some point. The trait is still here, and unit tests of
//! the state machine still use it, because spinning a socket to assert that
//! `Denial::OverLimit` renders a particular `help` string would be ceremony.
//!
//! # The three endpoints, as verified against the deployed service
//!
//! Note that `authorize` takes the credential in its **body** while the other
//! two take it in the `Authorization` header. That asymmetry is not a choice;
//! it is what `api.nudox.org` accepts, confirmed by probing it directly. This
//! block previously described `authorize` as header-authenticated, the client
//! implemented that, and the fake service in
//! `tests/account_against_a_fake_service.rs` was written to match — so all
//! three agreed with each other and none of them agreed with the server. See
//! [`AuthorizeRequest`].
//!
//! ```text
//! POST {base}/v1/authorize      body {"token":"ndx_…"}   ← NOT the header
//!   200 {"allowed":true,"user_id":24,"scopes":[]}   → Authorized::Allowed
//!   200 {"allowed":false,"reason":"invalid token"}  → Authorized::Denied
//!   401/403                                         → Authorized::Denied
//!   400 "token is required"                         → the credential was not in the body
//!
//! POST {base}/v1/usage/record   Authorization: Bearer ndx_…
//!   body {"kind":"tool_call","count":N}
//!   204  → RecordOutcome::Accepted
//!   429  → RecordOutcome::OverLimit
//!
//! GET  {base}/v1/usage          Authorization: Bearer ndx_…
//!   200 {"tier":…,"period_start":…,"used":…,"limit":…,"over_limit":…}
//! ```
//!
//! `scopes` and any org fields are parsed and dropped: `docs/auth.md` says to ignore
//! them for now, and a field we neither store nor act on is better absent from
//! our types than present and unused.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use super::credential::ApiKey;
use super::state::{ProbeFailure, QuotaSnapshot, UserId};

/// The production base URL.
///
/// A constant rather than a default buried in a constructor, so
/// `tests/account_service_wire.rs` can assert that no test ever resolves to it.
pub const DEFAULT_BASE_URL: &str = "https://api.nudox.org";

/// Environment override for the base URL.
///
/// Exists for staging and for the fake in integration tests that go through
/// process boundaries. Nothing in the automated suite relies on it — the tests
/// construct [`HttpAccountService::with_base_url`] explicitly, because a test
/// whose target depends on ambient environment is a test that passes for the
/// wrong reason on somebody else's machine.
pub const BASE_URL_ENV: &str = "NUDOX_API_BASE";

/// How long any single request may take before it is abandoned.
///
/// Short, because none of these calls is on the request path: `authorize` and
/// `usage` run in the background, and `usage/record` flushes a batch. A long
/// timeout buys nothing here and lengthens the window in which a flush is
/// [`ProbeFailure::Indeterminate`] — which is the expensive category, because
/// it is the one that drops a batch.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The `kind` this client records. Always `tool_call`, per `docs/auth.md`.
///
/// Not an enum with an `api_request` variant: nothing in this process makes a
/// billable API request, and a variant nothing constructs is a variant that
/// invites someone to construct it.
pub const USAGE_KIND: &str = "tool_call";

/// A boxed future, because [`AccountService`] must be usable as `dyn`.
///
/// `async fn` in traits does not produce a `dyn`-safe trait, and the gate holds
/// its service behind a trait object so the fake and the real client are
/// interchangeable at runtime rather than through a generic parameter that
/// would infect [`super::gate::AccountGate`], [`crate::server::NudoxMcpServer`]
/// and everything that names them.
pub type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What `POST v1/authorize` said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizeOutcome {
    /// `{"allowed": true, …}`.
    Allowed {
        /// The account this key belongs to.
        user: UserId,
    },
    /// `{"allowed": false, "reason": …}`, or a 401/403.
    Denied {
        /// The service's own `reason`, verbatim.
        reason: String,
    },
}

/// What `POST v1/usage/record` said.
///
/// Two variants because the endpoint has two success-ish outcomes and no body.
/// Note what is *not* here: there is no `Rejected` variant, because a 429 does
/// not tell us whether the batch was counted — see [`RecordOutcome::OverLimit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    /// 204. The batch is recorded.
    Accepted,
    /// 429. The account is at or over its limit.
    ///
    /// **Whether the batch that triggered this was itself recorded is
    /// unspecified by the contract** (`docs/auth.md` § "Gaps in the contract", gap
    /// 3). The ledger treats the batch as settled either way and immediately
    /// reconciles against `GET v1/usage`, which is the only authority on the
    /// question.
    OverLimit,
}

/// The three calls this client makes.
///
/// Every method takes the key explicitly rather than the implementation holding
/// one. A service object that owned a credential would have to be rebuilt on
/// every sign-in, and would be one more place a key sits.
pub trait AccountService: Send + Sync + 'static {
    /// Validate the key and learn who it belongs to.
    fn authorize<'a>(
        &'a self,
        key: &'a ApiKey,
    ) -> ServiceFuture<'a, Result<AuthorizeOutcome, ProbeFailure>>;

    /// Record `count` tool calls.
    fn record_usage<'a>(
        &'a self,
        key: &'a ApiKey,
        count: u32,
    ) -> ServiceFuture<'a, Result<RecordOutcome, ProbeFailure>>;

    /// Read the authoritative usage for the current period.
    fn usage<'a>(&'a self, key: &'a ApiKey)
    -> ServiceFuture<'a, Result<QuotaSnapshot, ProbeFailure>>;

    /// The base URL this implementation talks to, for diagnostics and for the
    /// test that asserts the suite never points at production.
    fn base_url(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Wire DTOs
// ---------------------------------------------------------------------------

/// `POST v1/authorize` response body.
///
/// `#[serde(default)]` on `reason` because the allowed case omits it, and
/// `deny_unknown_fields` is deliberately *not* set: the contract already
/// promises `scopes` and org fields we are told to ignore, and a client that
/// hard-fails on a field the server adds is a client that breaks on the next
/// deploy.
#[derive(Debug, serde::Deserialize)]
struct AuthorizeBody {
    allowed: bool,
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST v1/authorize` **request** body.
///
/// # Why the key travels in the body here and in the header everywhere else
///
/// It is not a design choice; it is what the deployed service accepts, verified
/// against `api.nudox.org` rather than inferred from `docs/auth.md`:
///
/// ```text
/// POST /v1/authorize  Authorization: Bearer ndx_…            → 400 "token is required"
/// POST /v1/authorize  Authorization: Bearer ndx_…  body {}   → 400 "token is required"
/// POST /v1/authorize  body {"token":"ndx_…"}                 → 200 {"allowed":true,"user_id":…}
/// GET  /v1/usage      Authorization: Bearer ndx_…            → 200 {…}
/// ```
///
/// So `authorize` reads the credential from this field and the other endpoints
/// read it from the header. The `Authorization` header is still sent on
/// `authorize` as well — verified to be accepted alongside the body — so that if
/// the service later unifies on the header, this client already satisfies the
/// stricter of the two shapes.
///
/// This mismatch shipped undetected because `tests/account_against_a_fake_service.rs`
/// asserts against a fake built from the same reading of `docs/auth.md` that produced
/// the client: both sides shared the misreading, so both agreed. Sign-in could
/// never have succeeded for any real user. It was caught by
/// `tests/account_against_the_real_service.rs`, which exists for exactly this
/// class and no other.
#[derive(Debug, serde::Serialize)]
struct AuthorizeRequest<'a> {
    token: &'a str,
}

/// `POST v1/usage/record` request body.
#[derive(Debug, serde::Serialize)]
struct RecordBody<'a> {
    kind: &'a str,
    count: u32,
}

/// `GET v1/usage` response body.
#[derive(Debug, serde::Deserialize)]
struct UsageBody {
    tier: String,
    period_start: String,
    api_requests: u64,
    tool_calls: u64,
    used: u64,
    limit: u64,
    remaining: u64,
    over_limit: bool,
}

// ---------------------------------------------------------------------------
// HttpAccountService
// ---------------------------------------------------------------------------

/// The real client.
///
/// Holds one `reqwest::Client` so connections are pooled across the refresh
/// loop and the usage flusher — three requests every few minutes down one TLS
/// session rather than three handshakes.
pub struct HttpAccountService {
    client: reqwest::Client,
    base: String,
}

impl HttpAccountService {
    /// A client pointed at `base`, which must not end in a slash.
    ///
    /// There is deliberately no `Default` and no zero-argument constructor:
    /// every caller states its target, so "which service did that test hit?"
    /// is answerable by reading the call site.
    pub fn with_base_url(base: impl Into<String>) -> Result<Self, ProbeFailure> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // No redirect following. Every one of these requests carries a
            // bearer credential in a header, and a redirect to another origin
            // would forward it there. `reqwest` strips sensitive headers on
            // cross-origin redirects, but the endpoint has no legitimate reason
            // to redirect at all, so the safe thing and the correct thing agree.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("nudox-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProbeFailure::NotDelivered {
                detail: format!("could not build an HTTP client: {e}"),
            })?;
        Ok(Self {
            client,
            base: base.into().trim_end_matches('/').to_owned(),
        })
    }

    /// A client pointed at [`DEFAULT_BASE_URL`], or at [`BASE_URL_ENV`] when
    /// that is set.
    pub fn for_production() -> Result<Self, ProbeFailure> {
        let base = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        Self::with_base_url(base)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// Turn a `reqwest` error into the delivery-certainty category the ledger needs.
///
/// This is the most consequential five lines in the module: it is what decides
/// whether a usage batch may be retried. The rule is deliberately conservative
/// — only a failure that provably happened *before the request was written* is
/// [`ProbeFailure::NotDelivered`].
///
/// `is_connect()` covers DNS resolution, connection refused, and TLS handshake
/// failure: in all three the server never saw a byte of the request.
/// `is_timeout()` is **not** included, even though many timeouts are connect
/// timeouts, because `reqwest` reports a read timeout the same way and a read
/// timeout means the request was written and may have been processed.
fn classify(e: &reqwest::Error) -> ProbeFailure {
    if e.is_connect() {
        ProbeFailure::NotDelivered {
            detail: e.to_string(),
        }
    } else if e.is_decode() || e.is_body() {
        ProbeFailure::MalformedResponse {
            detail: e.to_string(),
        }
    } else {
        ProbeFailure::Indeterminate {
            detail: e.to_string(),
        }
    }
}

/// How much of a service error body is worth carrying into a `ProbeFailure`.
///
/// Bounded for the reason `nudox_store::source::producer`'s `MAX_DETAIL_BYTES`
/// is bounded: an unbounded error detail is one bad response away from a log
/// line the size of an HTML error page. Long enough for any real API message,
/// short enough that a proxy's stack trace is truncated rather than stored.
const MAX_DETAIL_CHARS: usize = 200;

/// Turn a non-2xx response into the right kind of failure, carrying the
/// service's own explanation of it.
///
/// # Both halves of this were learned from a bug that shipped
///
/// **The body is included** because it is usually the only thing that says what
/// is actually wrong. `POST /v1/authorize` answered `400 "token is required"`
/// for every real user, and the client reported only `returned 400 Bad Request`
/// — discarding the four words that named the defect. The mismatch was
/// invisible for as long as that string was thrown away.
///
/// **A 4xx is not `Indeterminate`.** `Indeterminate` means *the request may or
/// may not have been processed, and retrying is unsafe* — a timeout, a reset, a
/// 5xx. A 4xx is the opposite: the service received the request, understood it,
/// and refused it, and it will refuse the identical request forever. Reporting
/// one as `Indeterminate` made a permanent client bug look like a transient
/// network problem, so [`AccountGate`](super::gate::AccountGate) held the user
/// in grace — papering over a sign-in that could never succeed instead of
/// surfacing it. `MalformedResponse` is the honest bucket: its own docs say it
/// means *our bug or a deployed mismatch*, which is exactly what a 4xx from our
/// own client is.
///
/// 401/403 never reach here — callers translate those to a `Denied` verdict
/// first, because they are the service saying no rather than the request being
/// wrong.
async fn non_success(
    endpoint: &str,
    status: reqwest::StatusCode,
    response: reqwest::Response,
) -> ProbeFailure {
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();

    let detail = if body.is_empty() {
        format!("{endpoint} returned {status}")
    } else {
        let mut shown: String = body.chars().take(MAX_DETAIL_CHARS).collect();
        if body.chars().count() > MAX_DETAIL_CHARS {
            shown.push('…');
        }
        format!("{endpoint} returned {status}: {shown}")
    };

    if status.is_client_error() {
        ProbeFailure::MalformedResponse { detail }
    } else {
        ProbeFailure::Indeterminate { detail }
    }
}

impl AccountService for HttpAccountService {
    fn authorize<'a>(
        &'a self,
        key: &'a ApiKey,
    ) -> ServiceFuture<'a, Result<AuthorizeOutcome, ProbeFailure>> {
        Box::pin(async move {
            let response = self
                .client
                .post(self.url("/v1/authorize"))
                .header(reqwest::header::AUTHORIZATION, key.bearer_header_value())
                .header(reqwest::header::ACCEPT, "application/json")
                // The credential must appear in the body for this endpoint; the
                // header alone is a 400. See [`AuthorizeRequest`].
                .json(&AuthorizeRequest {
                    token: key.expose(),
                })
                .send()
                .await
                .map_err(|e| classify(&e))?;

            let status = response.status();

            // 401/403 is the service saying no in HTTP rather than in JSON. It
            // is a *verdict*, not a transport failure, so it must reach the
            // gate as `Denied` and become a sticky revocation — not as an
            // `Indeterminate` that grace would paper over for a week.
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Ok(AuthorizeOutcome::Denied {
                    reason: format!("the service rejected this key ({status})"),
                });
            }

            if !status.is_success() {
                return Err(non_success("POST /v1/authorize", status, response).await);
            }

            let body: AuthorizeBody = response.json().await.map_err(|e| classify(&e))?;

            if !body.allowed {
                return Ok(AuthorizeOutcome::Denied {
                    reason: body
                        .reason
                        .unwrap_or_else(|| "the service did not say why".to_owned()),
                });
            }

            // `allowed: true` with no `user_id` is a broken contract, not an
            // authorised user. Defaulting the id to 0 here is exactly the
            // silent repair doctrine §8 forbids: it would produce a working
            // session attributed to nobody.
            let Some(user_id) = body.user_id else {
                return Err(ProbeFailure::MalformedResponse {
                    detail: "POST /v1/authorize allowed the key but returned no user_id"
                        .to_owned(),
                });
            };

            Ok(AuthorizeOutcome::Allowed {
                user: UserId(user_id),
            })
        })
    }

    fn record_usage<'a>(
        &'a self,
        key: &'a ApiKey,
        count: u32,
    ) -> ServiceFuture<'a, Result<RecordOutcome, ProbeFailure>> {
        Box::pin(async move {
            let response = self
                .client
                .post(self.url("/v1/usage/record"))
                .header(reqwest::header::AUTHORIZATION, key.bearer_header_value())
                .json(&RecordBody {
                    kind: USAGE_KIND,
                    count,
                })
                .send()
                .await
                .map_err(|e| classify(&e))?;

            match response.status() {
                reqwest::StatusCode::NO_CONTENT => Ok(RecordOutcome::Accepted),
                reqwest::StatusCode::TOO_MANY_REQUESTS => Ok(RecordOutcome::OverLimit),
                // A 401 here means the key was revoked between the last
                // `authorize` and this flush. It is a verdict, but the ledger's
                // job is delivery, not authorisation — reporting it as
                // indeterminate lets the batch drop and lets the next
                // `authorize` (which runs on its own cadence) produce the
                // revocation. Encoding a second authorisation path through the
                // usage endpoint would give us two places that can revoke.
                status => Err(ProbeFailure::Indeterminate {
                    detail: format!("POST /v1/usage/record returned {status}, expected 204 or 429"),
                }),
            }
        })
    }

    fn usage<'a>(
        &'a self,
        key: &'a ApiKey,
    ) -> ServiceFuture<'a, Result<QuotaSnapshot, ProbeFailure>> {
        Box::pin(async move {
            let response = self
                .client
                .get(self.url("/v1/usage"))
                .header(reqwest::header::AUTHORIZATION, key.bearer_header_value())
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| classify(&e))?;

            let status = response.status();
            if !status.is_success() {
                return Err(non_success("GET /v1/usage", status, response).await);
            }

            let body: UsageBody = response.json().await.map_err(|e| classify(&e))?;
            Ok(QuotaSnapshot {
                tier: body.tier,
                period_start: body.period_start,
                api_requests: body.api_requests,
                tool_calls: body.tool_calls,
                used: body.used,
                limit: body.limit,
                remaining: body.remaining,
                over_limit: body.over_limit,
                observed_at: SystemTime::now(),
            })
        })
    }

    fn base_url(&self) -> &str {
        &self.base
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_never_silently_points_at_production() {
        let staging = HttpAccountService::with_base_url("http://127.0.0.1:1/")
            .expect("building a client cannot fail for a well-formed base");
        assert_eq!(
            staging.base_url(),
            "http://127.0.0.1:1",
            "a trailing slash must be normalised away, or every URL gains a double slash"
        );
        assert_ne!(staging.base_url(), DEFAULT_BASE_URL);
    }

    #[test]
    fn the_usage_body_is_the_shape_the_contract_specifies() {
        // `docs/auth.md` gives the body literally as {"kind":"tool_call","count":1}.
        // Serialising it through the real struct is what keeps a field rename
        // from becoming a silent billing outage.
        let json = serde_json::to_value(RecordBody {
            kind: USAGE_KIND,
            count: 7,
        })
        .expect("a two-field struct serialises");
        assert_eq!(json, serde_json::json!({ "kind": "tool_call", "count": 7 }));
    }

    #[test]
    fn an_authorize_body_missing_user_id_is_not_an_authorised_user() {
        // Parsing it must succeed (the field is optional in the DTO) so the
        // *caller* can reject it with a message naming the contract violation,
        // rather than serde producing "missing field user_id" for both this and
        // a legitimate `allowed: false`.
        let denied: AuthorizeBody = serde_json::from_str(r#"{"allowed":false,"reason":"revoked"}"#)
            .expect("the denied shape carries no user_id and must still parse");
        assert!(!denied.allowed);
        assert_eq!(denied.reason.as_deref(), Some("revoked"));
        assert!(denied.user_id.is_none());
    }

    #[test]
    fn unknown_response_fields_do_not_break_the_client() {
        // `docs/auth.md` promises `scopes` and org fields we are told to ignore, and
        // the service will grow more. A client that rejects them breaks on a
        // deploy it was not part of.
        let body: AuthorizeBody = serde_json::from_str(
            r#"{"allowed":true,"user_id":24,"scopes":[],"org_id":9,"plan":{"x":1}}"#,
        )
        .expect("extra fields must be ignored, not rejected");
        assert!(body.allowed);
        assert_eq!(body.user_id, Some(24));
    }

    #[test]
    fn the_usage_body_parses_the_exact_example_from_the_contract() {
        let body: UsageBody = serde_json::from_str(
            r#"{"tier":"free","period_start":"2026-08-01T00:00:00Z","api_requests":300,
                "tool_calls":700,"used":1000,"limit":1000,"remaining":0,"over_limit":true}"#,
        )
        .expect("the documented example must parse");
        assert_eq!(body.tier, "free");
        assert_eq!(body.tool_calls, 700);
        assert_eq!(body.used, 1000);
        assert_eq!(body.remaining, 0);
        assert!(body.over_limit);
    }
}
