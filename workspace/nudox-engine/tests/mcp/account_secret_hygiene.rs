//! One rule, enforced instead of documented: **the account key never leaves the
//! two places that need it.**
//!
//! # Why this file exists
//!
//! `nudox-mcp` handles two secrets and they have opposite disclosure rules.
//! [`nudox_engine::mcp::SessionToken`] is *printed on purpose* — it is in the status
//! bar, in Settings → Connection, and in the client-config snippet the user
//! pastes into their agent harness. [`nudox_engine::mcp::ApiKey`] must appear nowhere:
//! not in a log line, not in a `Debug`, not in an error body, not in a config
//! snippet, not on disk outside the Keychain.
//!
//! Those two rules live three modules apart, which is exactly the distance at
//! which a refactor merges them. The concrete failure this guards against has a
//! CVE number: Docker Desktop's client serialised an authentication error that
//! had retained the Hub token which produced it, wrote it to the app log, and
//! then offered that log to the user as an exportable diagnostics bundle
//! (CVE-2025-13743, CWE-532 "Insertion of Sensitive Information into Log
//! File"). Nothing about that bug required anyone to be careless — it required
//! one error type to hold one field.
//!
//! # What "enforced" means here
//!
//! Every assertion below renders a real value through a real code path and
//! greps the output for a fragment of the key. That is a weaker check than a
//! type-level guarantee and a much stronger one than a comment, and it is the
//! strongest thing available for a property whose violation is "a string ends
//! up somewhere".

use std::time::SystemTime;

use nudox_engine::mcp::account::cache::{AUTHORIZATION_FILE, CachedAuthorization};
use nudox_engine::mcp::account::credential::{ApiKey, ApiKeyError};
use nudox_engine::mcp::account::gate::AccountGate;
use nudox_engine::mcp::account::state::{GateState, QuotaKnowledge, UserId};
use nudox_engine::mcp::account::store::{CredentialStore, KeySource, MemoryStore};
use nudox_engine::mcp::{McpEndpoint, NudoxMcpServer, SessionToken};
use nudox_engine::store::source::fixtures::FixtureSource;
use nudox_engine::{Engine, EngineConfig, EngineHandle};

/// The distinctive middle of the test key. Every assertion looks for *this*
/// rather than for the whole value, so a truncated leak is caught too.
const SECRET_BODY: &str = "41a9b60d47e3a5710c9fbe2d836a";

fn key() -> ApiKey {
    ApiKey::parse(&format!("ndx_2f8c{SECRET_BODY}4517")).expect("the test key is well formed")
}

fn engine() -> EngineHandle {
    Engine::start(EngineConfig::default(), FixtureSource::rich())
}

/// Fail with the offending text if `haystack` contains any part of the key.
fn assert_clean(what: &str, haystack: &str) {
    assert!(
        !haystack.contains(SECRET_BODY),
        "the account key leaked into {what}:\n{haystack}"
    );
    assert!(
        !haystack.contains(key().expose()),
        "the account key leaked into {what}:\n{haystack}"
    );
}

#[test]
fn the_client_config_snippet_carries_the_session_token_and_never_the_account_key() {
    // The load-bearing test for the "do not conflate two secrets" rule. The
    // snippet is *designed* to contain a secret — the per-launch loopback
    // token — which is precisely why it is the most likely place for the wrong
    // secret to end up.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let engine = engine();

    let token = SessionToken::from_secret("session-token-not-an-account-key");
    let endpoint = rt
        .block_on(McpEndpoint::start_with_token(
            NudoxMcpServer::new(
                engine.clone(),
                AccountGate::in_state_for_tests(GateState::verified(
                    UserId(24),
                    key().fingerprint(),
                    KeySource::Keychain,
                    SystemTime::now(),
                    SystemTime::now(),
                    QuotaKnowledge::Unknown,
                )),
            ),
            token,
        ))
        .expect("the endpoint binds");

    let snippet = endpoint.client_config_snippet();

    assert!(
        snippet.contains("session-token-not-an-account-key"),
        "the snippet must carry the session token, or the config it produces does \
         not work:\n{snippet}"
    );
    assert_clean("the client config snippet", &snippet);

    rt.block_on(endpoint.stop());
}

#[test]
fn a_debug_rendering_of_anything_that_holds_a_key_is_redacted() {
    let gate = AccountGate::in_state_for_tests(GateState::verified(
        UserId(24),
        key().fingerprint(),
        KeySource::Keychain,
        SystemTime::now(),
        SystemTime::now(),
        QuotaKnowledge::Unknown,
    ));

    // The types a `tracing::debug!` or a panic payload would most plausibly
    // reach for.
    assert_eq!(format!("{:?}", key()), "ApiKey(<redacted>)");
    assert_clean("ApiKey's Debug", &format!("{:?}", key()));
    assert_clean("AccountGate's Debug", &format!("{gate:?}"));
    assert_clean("UsageLedger's Debug", &format!("{:?}", gate.ledger()));
    assert_clean(
        "KeyFingerprint's Debug",
        &format!("{:?}", key().fingerprint()),
    );
    assert_clean("the display hint", &key().display_hint());
}

#[test]
fn no_rejection_of_a_key_ever_quotes_the_key() {
    // The Docker CVE shape: an error object that retains the credential which
    // produced it. `ApiKeyError` carries a *position* and a *class*, never a
    // value, and this asserts that across `Display`, `Debug` and `help`.
    let cases = [
        format!("ndx_2f8c{SECRET_BODY}4517\u{2013}trailing"),
        format!("ndx_2f8c {SECRET_BODY}4517"),
        format!("\"ndx_2f8c{SECRET_BODY}4517\""),
        format!("Bearer ndx_2f8c{SECRET_BODY}4517"),
    ];
    for input in cases {
        let err: ApiKeyError = ApiKey::parse(&input).expect_err("each case is malformed");
        assert_clean("ApiKeyError's Display", &err.to_string());
        assert_clean("ApiKeyError's Debug", &format!("{err:?}"));
        assert_clean("ApiKeyError's help", err.help());
    }
}

#[test]
fn nothing_written_to_disk_contains_the_key() {
    // The cache and the ledger both *name* the credential — they have to, or a
    // verdict earned by one key would authorise another — and both name it by
    // a truncated SHA-256. This asserts the fingerprint really is one-way in
    // practice, on the actual bytes that hit the filesystem.
    let dir = std::env::temp_dir().join(format!(
        "nudox-hygiene-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let state = GateState::verified(
        UserId(24),
        key().fingerprint(),
        KeySource::Keychain,
        SystemTime::now(),
        SystemTime::now(),
        QuotaKnowledge::Unknown,
    );
    CachedAuthorization::from_state(&state)
        .expect("an authorised state is cacheable")
        .save(&dir)
        .expect("save succeeds");

    let ledger =
        nudox_engine::mcp::account::ledger::UsageLedger::open(Some(&dir), &key().fingerprint());
    ledger.record_call();
    ledger.checkpoint();

    let mut files = 0;
    for entry in std::fs::read_dir(&dir).expect("readable") {
        let path = entry.expect("entry").path();
        let contents = std::fs::read_to_string(&path).expect("state files are text");
        assert_clean(&format!("{}", path.display()), &contents);
        assert!(
            !contents.contains("ndx_"),
            "no state file may contain even the key prefix: {}",
            path.display()
        );
        files += 1;
    }
    assert_eq!(
        files,
        2,
        "both state files must exist and both must have been checked: {}",
        dir.display()
    );
    assert!(dir.join(AUTHORIZATION_FILE).exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stored_key_comes_back_byte_identical() {
    // The counterweight to every assertion above: redaction that also mangled
    // the stored value would pass all of them and sign nobody in.
    let store = MemoryStore::empty();
    store.store(&key()).expect("store");
    let loaded = store.load().expect("load").expect("a key is there");
    assert_eq!(loaded.expose(), key().expose());
    assert_eq!(
        loaded.bearer_header_value(),
        format!("Bearer {}", key().expose()),
        "the header we send must be the key we were given"
    );
}

#[test]
fn the_two_secrets_are_not_interchangeable_types() {
    // Structural, not textual. There is no `From<SessionToken> for ApiKey`, no
    // `From<ApiKey> for SessionToken`, and a session token does not even parse
    // as an account key — so the conflation cannot be written by accident, and
    // the *first* thing someone would try in order to write it on purpose
    // fails here with a message naming the mistake.
    let session = SessionToken::generate();
    match ApiKey::parse(session.expose()) {
        Err(ApiKeyError::MissingPrefix { expected }) => {
            assert_eq!(expected, nudox_engine::mcp::account::API_KEY_PREFIX);
        }
        other => panic!(
            "a loopback session token must never be accepted as an account key, got {other:?}"
        ),
    }
}
