//! The Nudox account: the `ndx_` API key a person signs in with, where it is
//! kept, and the three `api.nudox.org` calls the products make with it.
//!
//! One key serves every surface. The desktop app asks for it on first launch,
//! `nudox auth login` takes it on the command line, and the MCP server reads
//! whatever either of them saved. Only MCP tool calls count as usage; the app
//! and the CLI never record any.
//!
//! | Module | Answers |
//! |---|---|
//! | [`key`] | what a key looks like, and how a paste error is caught offline |
//! | [`store`] | where the key lives (an owner-only file, or `NUDOX_API_KEY`) |
//! | [`service`] | `GET /v1/me`, `GET /v1/usage`, `POST /v1/usage/record` |
//! | [`meter`] | what an MCP tool call asks before it runs |
//!
//! # Why a file and not the Keychain
//!
//! The desktop app, the CLI and the MCP server are three executables, and the
//! MCP server is launched by an agent client with no window of its own. A
//! Keychain item belongs to the executable that created it: the other two get
//! an "allow access" prompt, and a headless MCP call would wait on a dialog
//! nobody sees. The key is therefore written the way the MCP HTTP token
//! already is, as an owner-only file in the per-user application directory,
//! like `gh`, `cargo` and `npm` keep theirs.

pub mod key;
pub mod meter;
pub mod service;
pub mod store;

pub use key::{API_KEY_PREFIX, ApiKey, KeyError};
pub use meter::{Admission, ToolCallMeter};
pub use service::{Account, DEFAULT_BASE_URL, RecordError, Service, ServiceError, Usage};
pub use store::{API_KEY_ENV, KeySource, KeyStore, StoreError};

/// Environment variable that turns the account requirement on in a debug build.
pub const GATE_ENV: &str = "NUDOX_ACCOUNT_GATE";

/// Where people create and copy their keys.
pub const KEYS_URL: &str = "https://nudox.org/dashboard/keys";

/// Whether this build requires a signed-in account.
///
/// Release builds always do. Debug builds, which every test lane and local
/// `cargo run` uses, only do when [`GATE_ENV`] is `on`, so the product's own
/// tests never need a real key.
#[must_use]
pub fn required() -> bool {
    !cfg!(debug_assertions) || std::env::var(GATE_ENV).is_ok_and(|value| value == "on")
}
