//! Account authentication against `api.nudox.org`: the `ndx_` key, where it
//! lives, what happens offline, and how billable tool calls are metered.
//!
//! Read `docs/auth.md` at the repository root for the design and the research behind
//! it. This is the code that implements it.
//!
//! # Why this lives in `nudox-mcp`
//!
//! It is not obviously MCP's business, and the first instinct is a `nudox-account`
//! crate. That instinct is wrong here for a checkable reason: `lindsey` needs
//! the sign-in surface, and AGENTS-DOCTRINE §1 — enforced by
//! `workspace/gui/tests/dependency_law.rs`, not by review — permits `lindsey` to
//! declare a direct dependency on `nudox-engine` and `nudox-mcp` and on nothing
//! else. A new backend crate would therefore cost a doctrine amendment and an
//! allow-list entry before it compiled once.
//!
//! It also earns its place. The *only* enforcement point in this system is a
//! `tools/call`, which is this crate's whole subject, and the account key sits
//! beside [`crate::session::SessionToken`] as the second of exactly two secrets
//! this process handles. Keeping them in one crate is what lets
//! [`credential`]'s module docs state the difference between them as a table
//! rather than as folklore, and what lets one test assert that the client-config
//! snippet contains one and not the other.
//!
//! # The map
//!
//! | Module | Answers |
//! |---|---|
//! | [`credential`] | what an `ndx_` key is, and how a paste error is caught before any network call |
//! | [`store`] | where the key lives at rest, and the seam that keeps tests off the login keychain |
//! | [`service`] | the three endpoints, and the delivery-certainty classification the ledger depends on |
//! | [`state`] | nine named postures, derived from five stored states and the clock |
//! | [`cache`] | the non-secret half of that state, on disk, bound to a key fingerprint |
//! | [`ledger`] | batching, crash safety, and the at-most-once reconciliation policy |
//! | [`gate`] | what a tool call asks, and the loops that keep the answer current |
//! | [`host`] | the synchronous surface `lindsey` drives, mirroring [`crate::host::McpHost`] |

pub mod cache;
pub mod credential;
pub mod gate;
pub mod host;
pub mod ledger;
pub mod service;
pub mod state;
pub mod store;

pub use cache::{AccountSummary, STATE_DIR_ENV, state_dir};
pub use credential::{API_KEY_PREFIX, ApiKey, ApiKeyError, KeyFingerprint};
pub use gate::{AccountGate, SignInFailure, ToolCallPermit};
pub use host::AccountHost;
pub use ledger::{DroppedTally, Reconciliation, UsageLedger};
pub use service::{
    AccountService, AuthorizeOutcome, DEFAULT_BASE_URL, HttpAccountService, RecordOutcome,
};
pub use state::{
    Denial, GRACE_WARNING_AT, GRACE_WINDOW, FRESH_FOR, GateState, Posture, ProbeFailure,
    QuotaKnowledge, QuotaSnapshot, UserId, Verdict,
};
pub use store::{
    API_KEY_ENV, CredentialStore, CredentialStoreError, KeySource, MemoryStore, platform_store,
};
