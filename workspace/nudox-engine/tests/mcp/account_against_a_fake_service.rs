//! The account gate, driven against a **real HTTP server** that this test
//! starts, controls, and can break on demand.
//!
//! # Why a real server and not an `impl AccountService`
//!
//! Doctrine §4: a test that would pass against a stub is not a test, and a test
//! that depends on a live paid service is not a test either. The gap between
//! those is exactly where this file sits.
//!
//! A hand-written `impl AccountService` returning `RecordOutcome::OverLimit`
//! would prove the state machine reacts to over-limit and nothing else — no 429
//! would ever have existed, so nothing would prove we *recognise* one. Every
//! failure this file reproduces is a failure of the wire: a 204 that must not
//! be read as a 200, a 429 that must not be read as an error, a JSON body with
//! `allowed: false`, a connection refused, a socket that accepts the request
//! and then dies without answering.
//!
//! [`FakeService`] is `api.nudox.org` reduced to the three endpoints `docs/auth.md`
//! specifies, on loopback, with a script the test sets. Nothing here resolves
//! `api.nudox.org`, and `the_suite_never_points_at_production` asserts it.
//!
//! # What each test is really about
//!
//! | Test | The defect it would catch |
//! |---|---|
//! | `a_valid_key_is_accepted_and_signs_the_user_in` | the happy path silently not persisting |
//! | `a_revoked_key_is_refused_and_the_refusal_is_sticky` | grace papering over a revocation |
//! | `a_malformed_key_never_reaches_the_network` | a paste error costing a round trip and a useless message |
//! | `tool_calls_accumulate_locally_and_flush_in_one_batch` | a request per tool call — the latency this whole design exists to avoid |
//! | `a_429_puts_the_account_over_limit_and_stops_tool_calls` | 429 read as a transport error, or as success |
//! | `an_unreachable_service_keeps_a_verified_account_working` | offline being treated as unauthorised |
//! | `an_unreachable_service_never_loses_pending_usage` | a connection refused eating the batch |
//! | `a_hung_request_drops_the_batch_rather_than_double_billing` | at-least-once retry on an ambiguous timeout |
//! | `expired_grace_denies_with_a_recoverable_error` | "you are offline" rendered as "your key is revoked" |

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use nudox_engine::mcp::account::cache::AUTHORIZATION_FILE;
use nudox_engine::mcp::account::credential::ApiKey;
use nudox_engine::mcp::account::gate::{AccountGate, SignInFailure};
use nudox_engine::mcp::account::ledger::DroppedTally;
use nudox_engine::mcp::account::service::HttpAccountService;
use nudox_engine::mcp::account::state::{
    Denial, GRACE_WINDOW, GateState, Posture, QuotaKnowledge, UserId, Verdict,
};
use nudox_engine::mcp::account::store::{KeySource, MemoryStore};

// ---------------------------------------------------------------------------
// The fake service
// ---------------------------------------------------------------------------

/// How the fake should behave for the next request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Answer normally.
    Healthy,
    /// Answer `allowed: false` from `/v1/authorize`.
    Revoked,
    /// Answer 429 from `/v1/usage/record`.
    OverLimit,
    /// Accept the connection, read the request, then close without answering.
    ///
    /// This is the *ambiguous* failure the whole at-most-once policy exists
    /// for: the request was written, so the client cannot know whether it was
    /// processed. Reproducing it with a real socket is the only way to be sure
    /// `reqwest`'s error classification puts it in the right bucket.
    HangUp,
}

/// A loopback stand-in for `api.nudox.org`, speaking the three endpoints in
/// `docs/auth.md` and nothing else.
struct FakeService {
    addr: SocketAddr,
    mode: Arc<std::sync::Mutex<Mode>>,
    /// `tool_calls` the fake has recorded — the number `GET /v1/usage` reports
    /// and the number reconciliation is measured against.
    recorded: Arc<AtomicU64>,
    /// How many `POST /v1/usage/record` requests arrived, as distinct from how
    /// many calls they carried. The difference *is* the batching.
    record_requests: Arc<AtomicU64>,
    /// How many `POST /v1/authorize` requests arrived.
    authorize_requests: Arc<AtomicU64>,
    /// Set once any request arrives carrying the wrong bearer.
    saw_bad_credential: Arc<AtomicBool>,
    shutdown: tokio_util::sync::CancellationToken,
}

/// The one key the fake accepts.
const GOOD_KEY: &str = "ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517";

/// The account id the fake reports for [`GOOD_KEY`].
const GOOD_USER: u64 = 24;

impl FakeService {
    async fn start() -> Self {
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;

        /// A JSON response without `axum::Json`.
        ///
        /// `axum` is a *production* dependency of this crate, declared
        /// `default-features = false` with only `http1` and `tokio`. Turning
        /// its `json` feature on so a test fake can be three lines shorter
        /// would change what the shipped server links, because Cargo unifies
        /// features across normal and dev dependencies.
        fn json(value: serde_json::Value) -> axum::response::Response {
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                value.to_string(),
            )
                .into_response()
        }
        use axum::routing::{get, post};

        #[derive(Clone)]
        struct Shared {
            mode: Arc<std::sync::Mutex<Mode>>,
            recorded: Arc<AtomicU64>,
            record_requests: Arc<AtomicU64>,
            authorize_requests: Arc<AtomicU64>,
            saw_bad_credential: Arc<AtomicBool>,
        }

        impl Shared {
            fn mode(&self) -> Mode {
                *self.mode.lock().expect("mode lock")
            }

            /// Every endpoint checks the bearer, because a client that stops
            /// sending one must fail here rather than keep working.
            fn check(&self, headers: &HeaderMap) -> bool {
                let ok = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    == Some(&format!("Bearer {GOOD_KEY}"));
                if !ok {
                    self.saw_bad_credential.store(true, Ordering::SeqCst);
                }
                ok
            }
        }

        let shared = Shared {
            mode: Arc::new(std::sync::Mutex::new(Mode::Healthy)),
            recorded: Arc::new(AtomicU64::new(0)),
            record_requests: Arc::new(AtomicU64::new(0)),
            authorize_requests: Arc::new(AtomicU64::new(0)),
            saw_bad_credential: Arc::new(AtomicBool::new(false)),
        };

        /// `POST /v1/authorize`.
        ///
        /// # The credential is read from the BODY here, and that is not a
        /// stylistic choice
        ///
        /// The deployed `api.nudox.org` answers `400 "token is required"` to a
        /// header-only `authorize`, and `200 {"allowed":true,…}` when the key
        /// arrives as `{"token":"ndx_…"}` in the body — the opposite of
        /// `/v1/usage`, which is header-authenticated. That asymmetry was
        /// verified by probing the real service.
        ///
        /// This fake previously accepted the header alone, because it was
        /// written from the same reading of `docs/auth.md` that produced the client.
        /// Both agreed; neither matched the server; **sign-in could not succeed
        /// for any real user** and the whole suite stayed green. Enforcing the
        /// real shape here is what stops that recurring: a client that reverts
        /// to header-only now fails every sign-in test in this file, not just
        /// the live-service suite that nobody runs by default.
        ///
        /// `body: String` rather than a typed extractor because `axum`'s `json`
        /// feature is deliberately off (see [`json`] above).
        async fn authorize(
            State(s): State<Shared>,
            headers: HeaderMap,
            body: String,
        ) -> axum::response::Response {
            s.authorize_requests.fetch_add(1, Ordering::SeqCst);

            // Checked before the header, matching production: a request with a
            // valid bearer and no body is a 400 there, not a 401.
            let token = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("token")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            let Some(token) = token else {
                return (StatusCode::BAD_REQUEST, "token is required").into_response();
            };
            if token != GOOD_KEY {
                s.saw_bad_credential.store(true, Ordering::SeqCst);
                return json(serde_json::json!({
                    "allowed": false,
                    "reason": "invalid token",
                }));
            }

            if !s.check(&headers) {
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }
            match s.mode() {
                Mode::Revoked => json(serde_json::json!({
                    "allowed": false,
                    "reason": "invalid token",
                })),
                // A hung `authorize` is modelled as a plain 502 rather than a
                // dropped socket: the connection-level hang-up is exercised on
                // `/v1/usage/record`, which is where its classification
                // actually changes behaviour.
                Mode::HangUp => (StatusCode::BAD_GATEWAY, "").into_response(),
                Mode::Healthy | Mode::OverLimit => json(serde_json::json!({
                    "allowed": true,
                    "user_id": GOOD_USER,
                    "scopes": [],
                    // A field this client does not know about. It must be
                    // ignored, not rejected — the service will grow, and a
                    // client that 500s on a deploy it was not part of is worse
                    // than one that ignores a field.
                    "org_id": 9,
                })),
            }
        }

        async fn record(
            State(s): State<Shared>,
            headers: HeaderMap,
            body: String,
        ) -> axum::response::Response {
            s.record_requests.fetch_add(1, Ordering::SeqCst);
            if !s.check(&headers) {
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }

            let parsed: serde_json::Value =
                serde_json::from_str(&body).expect("the client must send valid JSON");
            assert_eq!(
                parsed["kind"], "tool_call",
                "docs/auth.md: for MCP it is always tool_call"
            );
            let count = parsed["count"].as_u64().expect("count must be a number");

            match s.mode() {
                Mode::HangUp => {
                    // Record it, then never answer. This is the ambiguity: the
                    // service processed the batch and the client will never
                    // learn that.
                    s.recorded.fetch_add(count, Ordering::SeqCst);
                    // Sleeping past the client's 10 s request timeout is what
                    // makes `reqwest` produce a read timeout rather than a
                    // connect error.
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    (StatusCode::NO_CONTENT, "").into_response()
                }
                Mode::OverLimit => (StatusCode::TOO_MANY_REQUESTS, "").into_response(),
                Mode::Healthy | Mode::Revoked => {
                    s.recorded.fetch_add(count, Ordering::SeqCst);
                    (StatusCode::NO_CONTENT, "").into_response()
                }
            }
        }

        async fn usage(State(s): State<Shared>, headers: HeaderMap) -> axum::response::Response {
            if !s.check(&headers) {
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }
            let tool_calls = s.recorded.load(Ordering::SeqCst);
            let limit = 1000_u64;
            let over = s.mode() == Mode::OverLimit;
            json(serde_json::json!({
                "tier": "free",
                "period_start": "2026-08-01T00:00:00Z",
                "api_requests": 0,
                "tool_calls": tool_calls,
                "used": tool_calls,
                "limit": limit,
                "remaining": if over { 0 } else { limit.saturating_sub(tool_calls) },
                "over_limit": over,
            }))
            .into_response()
        }

        let app = axum::Router::new()
            .route("/v1/authorize", post(authorize))
            .route("/v1/usage/record", post(record))
            .route("/v1/usage", get(usage))
            .with_state(shared.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake must bind loopback");
        let addr = listener.local_addr().expect("bound address");
        let shutdown = tokio_util::sync::CancellationToken::new();

        {
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move { shutdown.cancelled().await })
                    .await;
            });
        }

        Self {
            addr,
            mode: shared.mode,
            recorded: shared.recorded,
            record_requests: shared.record_requests,
            authorize_requests: shared.authorize_requests,
            saw_bad_credential: shared.saw_bad_credential,
            shutdown,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn set_mode(&self, mode: Mode) {
        *self.mode.lock().expect("mode lock") = mode;
    }

    fn recorded(&self) -> u64 {
        self.recorded.load(Ordering::SeqCst)
    }

    fn record_requests(&self) -> u64 {
        self.record_requests.load(Ordering::SeqCst)
    }

    fn authorize_requests(&self) -> u64 {
        self.authorize_requests.load(Ordering::SeqCst)
    }

    fn saw_bad_credential(&self) -> bool {
        self.saw_bad_credential.load(Ordering::SeqCst)
    }

    /// Stop serving, so a client pointed here gets `connection refused`.
    ///
    /// This is how "offline" is reproduced: a real socket that is really gone,
    /// rather than an error value handed to the client by a stub.
    fn go_offline(&self) {
        self.shutdown.cancel();
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "nudox-account-it-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn case_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn good_key() -> ApiKey {
    ApiKey::parse(GOOD_KEY).expect("the fixture key is well formed")
}

/// A gate wired to `fake`, storing credentials in memory and state in `dir`.
///
/// `MemoryStore`, never the Keychain: an ad-hoc-signed `cargo test` binary gets
/// a new code hash on every build, which invalidates a keychain item's ACL and
/// makes macOS prompt — and a headless runner cannot answer a prompt at all.
/// See `account::store`'s module docs.
/// Refuse to run if the ambient environment can reach the code under test.
///
/// [`AccountGate`] resolves credentials through
/// `account::store::resolve_credential`, which consults `NUDOX_API_KEY`
/// **before** the store it was handed. So a developer who exports a real key —
/// which `tests/account_against_the_real_service.rs` requires — silently
/// changes what this suite is testing: a gate built over `MemoryStore::empty()`
/// then reports `awaiting_first_verification` rather than `signed_out`, and
/// `KeySource::Environment` rather than `Keychain`.
///
/// That is not hypothetical. Two tests here failed exactly this way, and both
/// failures read as product bugs — an assertion about a *fresh machine* was
/// quietly being made about a machine with a credential. The premise of this
/// whole file is a gate with no credential; if the environment supplies one,
/// there is nothing left to test and saying so is better than proceeding.
///
/// Panics rather than skipping, for the same reason the live suite panics on a
/// missing key: a suite that quietly tests nothing is worse than one that stops.
fn require_a_clean_environment() {
    assert!(
        std::env::var_os(nudox_engine::mcp::account::API_KEY_ENV).is_none(),
        "{} is set in this environment. This suite's subject is a gate with NO \
         credential, and the gate reads that variable before the store it is given, so \
         every 'fresh machine' assertion below would silently be testing something \
         else.\n\nRun it without the variable:\n\n    env -u {} cargo test -p nudox-mcp \
         --test account_against_a_fake_service\n",
        nudox_engine::mcp::account::API_KEY_ENV,
        nudox_engine::mcp::account::API_KEY_ENV,
    );
}

fn gate_for(fake: &FakeService, dir: &Path) -> AccountGate {
    require_a_clean_environment();
    let service = HttpAccountService::with_base_url(fake.base_url())
        .expect("building a client against a loopback base cannot fail");
    AccountGate::new(
        Box::new(MemoryStore::empty()),
        Arc::new(service),
        Some(dir.to_path_buf()),
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn the_suite_never_points_at_production() {
    // Doctrine: a test that depends on a live paid service is not a test. This
    // is the assertion that keeps it true as the file grows — the fake always
    // binds loopback, so any client built here must name 127.0.0.1.
    let rt = runtime();
    rt.block_on(async {
        let fake = FakeService::start().await;
        assert!(
            fake.base_url().starts_with("http://127.0.0.1:"),
            "the fake must be loopback, got {}",
            fake.base_url()
        );
        assert_ne!(
            fake.base_url(),
            nudox_engine::mcp::account::DEFAULT_BASE_URL
        );
    });
}

#[test]
fn a_valid_key_is_accepted_and_signs_the_user_in() {
    let rt = runtime();
    let scratch = Scratch::new("signin");
    let ((), _cost) = heart::cost::measured("account_sign_in", case_dir(), || {
        rt.block_on(async {
            let fake = FakeService::start().await;
            let gate = gate_for(&fake, scratch.path());

            assert_eq!(
                gate.posture().tag(),
                "signed_out",
                "a fresh machine must start signed out, not 'unknown'"
            );

            let posture = gate.sign_in(good_key()).await.expect("the fake accepts it");

            match &posture {
                Posture::Active {
                    account,
                    quota,
                    source,
                } => {
                    assert_eq!(account.user, UserId(GOOD_USER));
                    assert_eq!(*source, KeySource::Keychain);
                    assert!(
                        matches!(quota, QuotaKnowledge::Known(_)),
                        "sign-in must fetch usage so the meter shows real numbers rather \
                         than 0/0"
                    );
                }
                other => panic!("expected Active after a successful sign-in, got {other:?}"),
            }

            let summary = gate
                .account_summary()
                .expect("a signed-in gate has a summary");
            assert_eq!(summary.user, UserId(GOOD_USER));
            assert_eq!(summary.key_hint, "ndx_…4517");
            assert!(
                !summary.key_hint.contains("2f8c41a9"),
                "the summary a view renders must not carry the key"
            );

            // The verdict is cached, so the next launch does not have to ask.
            assert!(
                scratch.path().join(AUTHORIZATION_FILE).exists(),
                "a verified account must survive a restart without a round trip"
            );
            let cached = std::fs::read_to_string(scratch.path().join(AUTHORIZATION_FILE))
                .expect("the cache is readable");
            assert!(
                !cached.contains("ndx_"),
                "the on-disk cache must name the key by fingerprint, never hold it: {cached}"
            );

            assert!(!fake.saw_bad_credential());
        });
    });
}

#[test]
fn a_revoked_key_is_refused_and_the_refusal_is_sticky() {
    let rt = runtime();
    let scratch = Scratch::new("revoked");
    let ((), _cost) = heart::cost::measured("account_revoked_key", case_dir(), || {
        rt.block_on(async {
            let fake = FakeService::start().await;
            let gate = gate_for(&fake, scratch.path());

            // Sign in while healthy, so there is a real verified state and a
            // real grace window for a revocation to have to override.
            gate.sign_in(good_key())
                .await
                .expect("accepted while healthy");
            assert_eq!(gate.posture().tag(), "active");

            fake.set_mode(Mode::Revoked);
            let posture = gate.refresh_once().await;

            assert_eq!(
                posture.tag(),
                "revoked",
                "an `allowed: false` is an answer, not a network failure, and must not \
                 fall into the grace window"
            );
            match posture.verdict() {
                Verdict::Deny(Denial::Revoked { reason }) => {
                    assert_eq!(reason, "invalid token", "the service's own words, verbatim");
                }
                other => panic!("expected a revocation denial, got {other:?}"),
            }

            let err = gate.admit().expect_err("a revoked key admits nothing");
            assert_eq!(
                err.to_string(),
                "nudox rejected this account key: invalid token"
            );

            // Going offline must not un-revoke it: without this, "pull the
            // ethernet cable" would be a workaround for revocation.
            fake.go_offline();
            let after_offline = gate.refresh_once().await;
            assert_eq!(after_offline.tag(), "revoked");
        });
    });
}

#[test]
fn a_malformed_key_never_reaches_the_network() {
    let rt = runtime();
    let scratch = Scratch::new("malformed");
    rt.block_on(async {
        let fake = FakeService::start().await;
        let gate = gate_for(&fake, scratch.path());
        let before = fake.authorize_requests();

        // The four real paste failures. Each is caught by `ApiKey::parse`
        // before a gate ever sees it, which is the point: the user gets a
        // message about *their input* instead of the service's generic
        // "invalid token", and it arrives instantly.
        for bad in [
            "",
            "0123456789abcdef0123456789abcdef",
            "\"ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517\"",
            "ndx_short",
        ] {
            let err = ApiKey::parse(bad).expect_err("must be rejected offline");
            assert!(!err.help().is_empty(), "{bad:?} must come with advice");
            assert!(
                !format!("{err}").contains(bad) || bad.is_empty(),
                "an error about a credential must not echo it: {err}"
            );
        }

        assert_eq!(
            fake.authorize_requests(),
            before,
            "not one malformed key may cost a round trip"
        );

        // And a well-formed key the service does not know still fails cleanly.
        let wrong = ApiKey::parse("ndx_ffffffffffffffffffffffffffffffffffff")
            .expect("well formed but wrong");
        // The service answers an unknown token with `200 {"allowed":false,
        // "reason":"invalid token"}` — NOT a 401. That was verified against the
        // real `api.nudox.org`, and this assertion used to require "401" or
        // "rejected" in the text because the fake authenticated from the header
        // and 401'd instead. It was pinning the fake's behaviour, not the
        // service's.
        //
        // What actually matters is asserted instead: the refusal is a *verdict*
        // (`Rejected`, which is sticky) rather than an `Unreachable` that grace
        // would paper over, it carries the service's own words so the user knows
        // the key is wrong rather than the network, and it does not echo the
        // credential back into a log line.
        match gate.sign_in(wrong).await {
            Err(SignInFailure::Rejected { reason }) => {
                assert!(
                    !reason.trim().is_empty(),
                    "a rejection must say something the user can act on"
                );
                assert!(
                    !reason.contains("ffffffffffffffffffffffffffffffffffff"),
                    "the rejection echoed the key: {reason}"
                );
            }
            other => panic!("an unknown key must be rejected, got {other:?}"),
        }
        assert_eq!(
            gate.posture().tag(),
            "signed_out",
            "a rejected key must not be stored — the next launch would trip over it"
        );
    });
}

#[test]
fn tool_calls_accumulate_locally_and_flush_in_one_batch() {
    let rt = runtime();
    let scratch = Scratch::new("batching");
    let ((), _cost) = heart::cost::measured("account_usage_batching", case_dir(), || {
        rt.block_on(async {
            let fake = FakeService::start().await;
            let gate = gate_for(&fake, scratch.path());
            gate.sign_in(good_key()).await.expect("signed in");

            let requests_after_signin = fake.record_requests();

            for _ in 0..20 {
                gate.admit().expect("an active account admits");
            }

            assert_eq!(
                fake.record_requests(),
                requests_after_signin,
                "THE property this whole design exists for: twenty tool calls must cost \
                 zero requests to api.nudox.org on the request path"
            );
            assert_eq!(gate.ledger().pending(), 20);

            gate.flush_once(true).await;

            assert_eq!(
                fake.record_requests(),
                requests_after_signin + 1,
                "and then exactly one request, carrying all twenty"
            );
            assert_eq!(
                fake.recorded(),
                20,
                "every admitted call must be billed exactly once"
            );
            assert_eq!(gate.ledger().pending(), 0);
            assert_eq!(gate.ledger().unreported_calls(), 0);
            assert_eq!(gate.ledger().dropped(), DroppedTally::default());
        });
    });
}

#[test]
fn a_429_puts_the_account_over_limit_and_stops_tool_calls() {
    let rt = runtime();
    let scratch = Scratch::new("overlimit");
    let ((), _cost) = heart::cost::measured("account_over_limit", case_dir(), || {
        rt.block_on(async {
            let fake = FakeService::start().await;
            let gate = gate_for(&fake, scratch.path());
            gate.sign_in(good_key()).await.expect("signed in");

            for _ in 0..3 {
                gate.admit().expect("still under limit");
            }

            fake.set_mode(Mode::OverLimit);
            gate.flush_once(true).await;

            let posture = gate.posture();
            assert_eq!(
                posture.tag(),
                "over_limit",
                "a 429 is a product state, not a transport error"
            );

            let err = gate
                .admit()
                .expect_err("an over-limit account must stop admitting");
            let rendered = format!("{err}");
            assert!(
                rendered.contains("usage limit"),
                "the message must say quota, not 'error': {rendered}"
            );

            // The batch that triggered the 429 is settled, not re-queued: the
            // contract does not say whether it was counted, and re-sending it
            // is the one outcome that can bill twice.
            assert_eq!(gate.ledger().pending(), 0);
            assert_eq!(
                gate.ledger().dropped(),
                DroppedTally::default(),
                "a 429 is an answer, so it is not a dropped batch"
            );
        });
    });
}

#[test]
fn an_unreachable_service_keeps_a_verified_account_working() {
    let rt = runtime();
    let scratch = Scratch::new("offline");
    let ((), _cost) = heart::cost::measured("account_offline_grace", case_dir(), || {
        rt.block_on(async {
            let fake = FakeService::start().await;
            let gate = gate_for(&fake, scratch.path());
            gate.sign_in(good_key()).await.expect("signed in");

            // A real socket, really gone.
            fake.go_offline();
            // Give the listener a moment to actually stop accepting.
            tokio::time::sleep(Duration::from_millis(100)).await;

            let posture = gate.refresh_once().await;
            assert!(
                matches!(posture.verdict(), Verdict::Allow),
                "a developer on a plane must keep working; got {posture:?}"
            );
            assert!(
                matches!(
                    posture,
                    Posture::Active { .. } | Posture::GraceOffline { .. }
                ),
                "an unreachable service must not downgrade a verified account to \
                 unauthorised; got {}",
                posture.tag()
            );

            for _ in 0..5 {
                gate.admit().expect("tool calls continue offline");
            }
            assert_eq!(
                gate.ledger().pending(),
                5,
                "and the calls are counted for when the network comes back"
            );
        });
    });
}

#[test]
fn an_unreachable_service_never_loses_pending_usage() {
    let rt = runtime();
    let scratch = Scratch::new("requeue");
    rt.block_on(async {
        let fake = FakeService::start().await;
        let gate = gate_for(&fake, scratch.path());
        gate.sign_in(good_key()).await.expect("signed in");

        for _ in 0..4 {
            gate.admit().expect("admitted");
        }

        fake.go_offline();
        tokio::time::sleep(Duration::from_millis(100)).await;

        gate.flush_once(true).await;

        assert_eq!(
            gate.ledger().pending(),
            4,
            "connection refused proves the request never left, so re-sending it is \
             safe and dropping it would be revenue thrown away for nothing"
        );
        assert_eq!(gate.ledger().dropped(), DroppedTally::default());
    });
}

#[test]
fn a_hung_request_drops_the_batch_rather_than_double_billing() {
    let rt = runtime();
    let scratch = Scratch::new("ambiguous");
    let ((), _cost) = heart::cost::measured("account_ambiguous_flush", case_dir(), || {
        rt.block_on(async {
            let fake = FakeService::start().await;
            let gate = gate_for(&fake, scratch.path());
            gate.sign_in(good_key()).await.expect("signed in");

            for _ in 0..6 {
                gate.admit().expect("admitted");
            }

            // The service will read the request, record it, and never answer.
            // The client cannot distinguish this from "never processed".
            fake.set_mode(Mode::HangUp);
            gate.flush_once(true).await;

            assert_eq!(
                fake.recorded(),
                6,
                "the fake really did process the batch — that is what makes this ambiguous"
            );
            assert_eq!(
                gate.ledger().pending(),
                0,
                "an ambiguous batch must NOT be re-queued: re-sending it would bill the \
                 user twice for work they did once"
            );
            assert_eq!(
                gate.ledger().dropped(),
                DroppedTally {
                    batches: 1,
                    calls: 6
                },
                "and the under-count must be counted, not silently absorbed (doctrine §8: \
                 a repair is permitted only when it is typed, counted, bounded and visible)"
            );
            assert_eq!(
                gate.ledger().unreported_calls(),
                6,
                "so a status bar can show it"
            );
        });
    });
}

#[test]
fn expired_grace_denies_with_an_error_that_cannot_be_read_as_a_revocation() {
    // The clock cannot be moved forward a week in a test, so the state is
    // constructed directly at the far side of the window. `state.rs`'s unit
    // tests pin the boundary arithmetic; this pins what the *wire* says when
    // it is crossed, which is the half a user actually meets.
    let gate = AccountGate::in_state_for_tests(GateState::verified(
        UserId(GOOD_USER),
        good_key().fingerprint(),
        KeySource::Keychain,
        SystemTime::now() - (GRACE_WINDOW + Duration::from_secs(3600)),
        SystemTime::now(),
        QuotaKnowledge::Unknown,
    ));

    assert_eq!(gate.posture().tag(), "grace_expired");

    let err = gate.admit().expect_err("expired grace must deny");
    let message = err.to_string();
    let data = err
        .into_error_data()
        .data
        .expect("every account error carries structured data");

    assert!(
        message.contains("unable to verify") && message.contains("offline grace"),
        "the message must name connectivity, not the key: {message}"
    );
    assert!(
        !message.to_lowercase().contains("reject")
            && !message.to_lowercase().contains("revok")
            && !message.to_lowercase().contains("invalid"),
        "a user who is merely offline must never be told their key is bad: {message}"
    );
    assert_eq!(data["kind"], "offline_grace_expired");
    assert_eq!(
        data["recoverable"], true,
        "an agent must be able to tell 'wait for wifi' from 'the account is gone' \
         without parsing prose"
    );
    assert_eq!(data["graceWindowDays"], 7);
    assert!(
        data["help"]
            .as_str()
            .expect("help is present")
            .contains("connectivity"),
        "help must name the actual problem: {data}"
    );
}

#[test]
fn signing_out_forgets_the_credential_the_cache_and_the_ledger() {
    let rt = runtime();
    let scratch = Scratch::new("signout");
    rt.block_on(async {
        let fake = FakeService::start().await;
        let gate = gate_for(&fake, scratch.path());
        gate.sign_in(good_key()).await.expect("signed in");
        gate.admit().expect("admitted");

        assert!(scratch.path().join(AUTHORIZATION_FILE).exists());

        gate.sign_out().expect("sign out succeeds");

        assert_eq!(gate.posture().tag(), "signed_out");
        assert!(gate.account_summary().is_none());
        assert!(
            !scratch.path().join(AUTHORIZATION_FILE).exists(),
            "a cached verdict must not survive a sign-out — it would re-authorise the \
             next launch for an account the user left"
        );
        assert!(gate.admit().is_err());
        assert_eq!(
            gate.ledger().dropped(),
            DroppedTally {
                batches: 1,
                calls: 1
            },
            "the one unflushed call belonged to the account being left, so it is \
             unreportable rather than forgotten"
        );
    });
}
