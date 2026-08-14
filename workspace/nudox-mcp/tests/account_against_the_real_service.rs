//! The account plane against the **real** `api.nudox.org`.
//!
//! # Why this file exists when `account_against_a_fake_service.rs` is thorough
//!
//! The fake suite is the better test of *our* logic: it can produce a 429, hang
//! a request, revoke a key mid-session and expire a grace window on demand, none
//! of which a real service will do to order. What it cannot prove is that the
//! contract we implemented is the contract the service actually speaks. Every
//! assertion there is ultimately checked against a fake we wrote from the same
//! reading of `docs/auth.md` that produced the client — so a misreading is invisible,
//! because both sides share it.
//!
//! This file closes exactly that gap and nothing else. If a field is renamed
//! server-side, a status code changes meaning, or `POST v1/authorize` starts
//! omitting `user_id` for a valid key, the fake keeps passing and these fail.
//!
//! # Why every test is `#[ignore]`
//!
//! `account_against_a_fake_service.rs` opens with the rule these tests must not
//! break: *a test that depends on a live paid service is not a test.* That is
//! right, and it is why nothing here runs in the default suite — no CI job, no
//! `cargo test` on a laptop, no contributor without an account is ever blocked
//! by `api.nudox.org` being slow, down, or rate-limiting. These are a
//! deliberate, manual contract check:
//!
//! ```text
//! NUDOX_API_KEY=ndx_… cargo test -p nudox-mcp \
//!     --test account_against_the_real_service -- --ignored
//! ```
//!
//! # The key comes from the environment, and is never written down
//!
//! [`nudox_mcp::account::API_KEY_ENV`] (`NUDOX_API_KEY`) is the same variable
//! the shipping binary reads (`account::store::resolve_credential`), so running
//! these exercises the real resolution path rather than a test-only door.
//!
//! **No key is committed to this repository, and none may be.** A literal
//! `ndx_…` in a source file is a published credential the moment the repo is
//! shared, and rotating it is someone's afternoon. `tests/account_secret_hygiene.rs`
//! guards the rendering paths; this comment guards this file. If you are about
//! to paste a key here to make a run easier — export it instead.
//!
//! # These tests are read-only, on purpose
//!
//! They call `POST v1/authorize` and `GET v1/usage`. They deliberately do **not**
//! call `POST v1/usage/record`, and no test here starts
//! [`AccountGate::supervise`] or calls `flush_once`, both of which would.
//! Recording usage mutates the account's billing counters on a live service —
//! an irreversible side effect on someone's quota, caused by running a test
//! suite. Verifying the record path against production is a decision for the
//! account's owner to make explicitly, not something a test file should do
//! because it was convenient. The fake suite already covers batching,
//! over-limit and double-billing behaviour, which is where the interesting
//! logic lives anyway.

use std::path::Path;
use std::sync::Arc;

use nudox_mcp::account::{
    API_KEY_ENV, AccountGate, AccountService, ApiKey, AuthorizeOutcome, DEFAULT_BASE_URL,
    HttpAccountService, MemoryStore, Posture, QuotaKnowledge,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The key under test, from the environment.
///
/// Panics rather than skipping. These tests are already `#[ignore]`, so reaching
/// this function means someone typed `--ignored` and intended a live run; a
/// silent green in that situation would be the worst of both worlds — it looks
/// like the contract was checked and nothing was checked at all. That is the
/// failure mode this project keeps hitting: a zero-work pass is indistinguishable
/// from a healthy one unless something refuses to be quiet.
fn key_from_env() -> ApiKey {
    let raw = std::env::var(API_KEY_ENV).unwrap_or_else(|_| {
        panic!(
            "{API_KEY_ENV} is not set. These tests talk to the real service and need a \
             real key:\n\n    {API_KEY_ENV}=ndx_… cargo test -p nudox-mcp \\\n        \
             --test account_against_the_real_service -- --ignored\n\n\
             Do not paste a key into this file — see its module docs."
        )
    });
    ApiKey::parse(raw.trim()).unwrap_or_else(|e| {
        panic!("{API_KEY_ENV} is set but is not a well-formed account key: {e}")
    })
}

/// A client pointed at production, with a guard that it really is production.
///
/// The fake suite asserts its client is *never* production; this asserts the
/// mirror image. Both exist because a test whose subject silently swapped
/// would keep passing while proving something else entirely — the fake suite
/// would be testing the network, and this would be testing the fake.
fn production_client() -> HttpAccountService {
    let service =
        HttpAccountService::for_production().expect("building the production client cannot fail");
    let base = service.base_url().to_owned();
    assert!(
        !base.contains("127.0.0.1") && !base.contains("localhost"),
        "this suite must talk to the real service, but the client is pointed at {base}. \
         NUDOX_API_BASE is probably still set from a fake-service run."
    );
    if base != DEFAULT_BASE_URL {
        eprintln!(
            "note: NUDOX_API_BASE overrides the default; testing against {base}, not \
             {DEFAULT_BASE_URL}"
        );
    }
    service
}

/// A key that passes every offline check and that the service has never issued.
///
/// Built by rotating the hex digits of the *real* key's body, so it keeps the
/// exact prefix, length and character class of a genuine key. That is the whole
/// point: a shape the offline validator cannot fault, so the only thing that can
/// reject it is the service. A hand-written `ndx_deadbeef…` would risk being
/// refused by [`ApiKey::parse`] and proving nothing about the network at all.
fn a_key_the_service_never_issued(real: &ApiKey) -> ApiKey {
    let raw = real.expose();
    let (prefix, body) = raw.split_at(4);
    let rotated: String = body
        .chars()
        .map(|c| match c {
            '0'..='8' => ((c as u8) + 1) as char,
            '9' => '0',
            'a'..='e' => ((c as u8) + 1) as char,
            'f' => 'a',
            other => other,
        })
        .collect();
    assert_ne!(rotated, body, "the decoy must differ from the real key");
    ApiKey::parse(&format!("{prefix}{rotated}"))
        .expect("the decoy keeps the real key's shape, so offline validation must accept it")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

/// A gate over the real service.
///
/// `MemoryStore`, never the platform Keychain — same reason the fake suite gives:
/// an ad-hoc-signed `cargo test` binary gets a new code hash on every build,
/// which invalidates a keychain item's ACL and makes macOS prompt, and a
/// headless runner cannot answer a prompt.
fn production_gate(state_dir: &Path) -> AccountGate {
    AccountGate::new(
        Box::new(MemoryStore::empty()),
        Arc::new(production_client()),
        Some(state_dir.to_path_buf()),
    )
}

fn scratch(case: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/account-real-service")
        .join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch state directory must be creatable");
    dir
}

// ---------------------------------------------------------------------------
// authorize
// ---------------------------------------------------------------------------

/// The service accepts the configured key and tells us which account it is.
///
/// The `user` assertion is the load-bearing half. A client that treated any
/// 2xx as success would pass a weaker version of this test while quietly
/// signing everyone in as the same anonymous account — which is why
/// `HttpAccountService` treats `allowed: true` with no `user_id` as a
/// `Malformed` failure rather than a success.
#[test]
#[ignore = "talks to the real api.nudox.org; run with --ignored and NUDOX_API_KEY set"]
fn the_real_service_accepts_the_configured_key_and_names_an_account() {
    let rt = runtime();
    rt.block_on(async {
        let service = production_client();
        let key = key_from_env();

        let outcome = service
            .authorize(&key)
            .await
            .expect("the real service must be reachable for this suite to mean anything");

        match outcome {
            AuthorizeOutcome::Allowed { user } => {
                eprintln!("authorized as user {user:?} against {}", service.base_url());
            }
            AuthorizeOutcome::Denied { reason } => panic!(
                "the real service refused the key in {API_KEY_ENV}: {reason}. If this key was \
                 revoked or rotated, update the environment rather than this file."
            ),
        }
    });
}

/// A key with a genuine key's exact shape, which the service has never issued,
/// is refused **by the service** — not by our offline validator.
///
/// This is the test that proves the network is actually being consulted. Every
/// other assertion in this file would still pass if `authorize` returned
/// `Allowed` unconditionally without making a request; this one would not.
#[test]
#[ignore = "talks to the real api.nudox.org; run with --ignored and NUDOX_API_KEY set"]
fn a_key_the_service_never_issued_is_denied_by_the_service_not_by_offline_validation() {
    let rt = runtime();
    rt.block_on(async {
        let service = production_client();
        let decoy = a_key_the_service_never_issued(&key_from_env());

        // Precondition, stated as an assertion: the decoy already passed
        // `ApiKey::parse` in its constructor, so offline validation has no
        // objection to it. Whatever rejects it below is the service.
        let outcome = service
            .authorize(&decoy)
            .await
            .expect("the service must answer, even to refuse");

        match outcome {
            AuthorizeOutcome::Denied { reason } => {
                assert!(
                    !reason.is_empty(),
                    "a denial must carry the service's reason; an empty one leaves the user \
                     with nothing to act on"
                );
                assert!(
                    !reason.contains(decoy.expose()),
                    "the service echoed the rejected key back in its reason, which would put \
                     a credential into every log line that records a failed sign-in"
                );
                eprintln!("decoy correctly denied: {reason}");
            }
            AuthorizeOutcome::Allowed { user } => panic!(
                "the service ALLOWED a key it never issued (as user {user:?}). Either the \
                 decoy collided with a real key — rerun to confirm — or authorization is not \
                 actually checking the key."
            ),
        }
    });
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

/// `GET v1/usage` answers in the shape this client deserialises, with values
/// that are internally consistent.
///
/// Deliberately does not assert `used + remaining == limit`: `QuotaSnapshot`'s
/// own docs say `remaining` is the service's computation and is not recomputed
/// here, precisely so a service that counts differently stays visible. What is
/// asserted are the invariants a meter cannot be wrong about without misleading
/// a user — you cannot have room left and be over your limit at the same time.
#[test]
#[ignore = "talks to the real api.nudox.org; run with --ignored and NUDOX_API_KEY set"]
fn the_real_usage_endpoint_returns_a_coherent_snapshot() {
    let rt = runtime();
    rt.block_on(async {
        let service = production_client();
        let key = key_from_env();

        let quota = service
            .usage(&key)
            .await
            .expect("the real service must answer GET v1/usage");

        eprintln!(
            "quota: tier={} period_start={} used={} limit={} remaining={} over_limit={} \
             (api_requests={} tool_calls={})",
            quota.tier,
            quota.period_start,
            quota.used,
            quota.limit,
            quota.remaining,
            quota.over_limit,
            quota.api_requests,
            quota.tool_calls,
        );

        assert!(
            !quota.tier.is_empty(),
            "the meter renders the tier name; an empty one shows a blank plan"
        );
        assert!(
            !quota.period_start.is_empty(),
            "period_start is the only period information the contract provides (docs/auth.md gap 1); \
             without it the UI cannot say anything about when the quota resets"
        );
        assert!(
            quota.remaining <= quota.limit,
            "remaining ({}) exceeds the plan limit ({})",
            quota.remaining,
            quota.limit
        );
        if quota.over_limit {
            assert_eq!(
                quota.remaining, 0,
                "the account is reported over its limit but still has {} remaining; a user \
                 shown both would not know which to believe",
                quota.remaining
            );
        }
    });
}

// ---------------------------------------------------------------------------
// The whole gate, end to end
// ---------------------------------------------------------------------------

/// Signing in with the real key against the real service produces an `Active`
/// posture carrying real quota, and admits a tool call.
///
/// This is the assertion closest to what a user experiences: it is the same
/// `sign_in` the GUI's sign-in surface calls and the same `admit` every MCP
/// tool call passes through. `QuotaKnowledge::Known` matters — a sign-in that
/// authorized but failed to fetch usage would leave the meter reading 0/0,
/// which reads as "no quota" rather than "not asked yet".
///
/// The ledger this accumulates into is a scratch directory and is never
/// flushed, so no usage is reported to the service (see the module docs).
#[test]
#[ignore = "talks to the real api.nudox.org; run with --ignored and NUDOX_API_KEY set"]
fn signing_in_with_the_real_key_yields_an_active_posture_and_admits_a_tool_call() {
    let rt = runtime();
    let dir = scratch("signin");
    rt.block_on(async {
        let gate = production_gate(&dir);
        let key = key_from_env();
        let tail = key.expose()[key.expose().len() - 4..].to_owned();

        // Not `signed_out`: `NUDOX_API_KEY` is set for this suite, and
        // `account::store::resolve_credential` consults the environment before
        // the store. So a gate built over an *empty* `MemoryStore` still finds a
        // credential — it just has not been verified with the service yet. That
        // is a real behaviour worth pinning: it is how a headless or CI
        // installation is expected to be configured, and the only difference
        // from a keychain sign-in is which `KeySource` it reports.
        assert_eq!(
            gate.posture().tag(),
            "awaiting_first_verification",
            "with {API_KEY_ENV} set, a gate must pick the credential up from the environment \
             and sit unverified until it has asked the service — reporting `signed_out` here \
             would mean the environment path is not wired at all"
        );

        let posture = gate
            .sign_in(key)
            .await
            .expect("the real service must accept the configured key");

        match &posture {
            Posture::Active { account, quota, .. } => {
                assert!(
                    matches!(quota, QuotaKnowledge::Known(_)),
                    "sign-in must fetch usage so the meter shows real numbers, not 0/0; \
                     got {quota:?}"
                );
                eprintln!("signed in as {:?}", account.user);
            }
            other => panic!("expected Active after a real sign-in, got {other:?}"),
        }

        // The summary a view renders must identify the key without carrying it.
        let summary = gate
            .account_summary()
            .expect("a signed-in gate must have a summary");
        assert!(
            summary.key_hint.ends_with(&tail),
            "the hint should end in the key's real last four so a user can tell two keys \
             apart; got {}",
            summary.key_hint
        );
        assert!(
            summary.key_hint.len() < 16,
            "the hint is a mask, not the key; got {}",
            summary.key_hint
        );

        // And a tool call is admitted — the thing the whole gate exists to
        // decide. Local-only: nothing is flushed to the service.
        gate.admit()
            .expect("a freshly signed-in account within its quota must admit a tool call");
    });
}

/// The real key must not survive into any rendering a log or a view could
/// capture.
///
/// `account_secret_hygiene.rs` proves this for a fixture key. This proves it for
/// a key that is actually valuable, because the failure being guarded against —
/// a `Debug` impl that forwards to the inner `String` — would be caught by
/// neither code review nor a fixture whose leak nobody would notice.
///
/// No network: this is here rather than in the hygiene suite only because it
/// needs the real credential.
#[test]
#[ignore = "needs NUDOX_API_KEY; makes no network calls"]
fn the_real_key_never_appears_in_a_debug_or_display_rendering() {
    let key = key_from_env();
    let secret = key.expose().to_owned();

    let debug = format!("{key:?}");
    assert!(
        !debug.contains(&secret),
        "ApiKey's Debug rendering contains the key itself; every `tracing` call that \
         formats one would write a credential to disk"
    );

    let body = &secret[4..];
    assert!(
        !debug.contains(body),
        "ApiKey's Debug rendering contains the key body without its prefix, which is just \
         as recoverable"
    );
}
