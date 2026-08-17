//! `McpError` — the crate's single error enum (§L7.4).
//!
//! `anyhow` appears nowhere in this crate outside tests. Every fallible
//! operation returns `Result<_, McpError>`, and the MCP tool layer converts
//! that into an `rmcp::ErrorData` with a JSON-RPC error code chosen from the
//! variant — so an agent sees `invalid_params` for a malformed `SymbolKey` and
//! `internal_error` for an engine failure, rather than one opaque code for
//! everything.

use std::net::SocketAddr;

use crate::wire::EngineError;

/// Errors raised by the hosted MCP server.
///
/// `#[non_exhaustive]` per §L7.5: adding a variant when a new tool or transport
/// concern appears must not be a breaking change for `lindsey`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The request carried no session token, or the wrong one.
    ///
    /// Deliberately carries no detail: a caller that guessed wrong learns only
    /// that it guessed wrong (§L6 local-only-by-construction).
    #[error("missing or invalid session token")]
    Unauthenticated,

    /// A `SymbolKey` argument was not in `ecosystem:name#introhex` form.
    #[error("malformed symbol key {key:?}: {reason}")]
    MalformedKey {
        /// The rejected input, echoed back so an agent can self-correct.
        key: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A package lineage argument was not in `ecosystem:name` form.
    ///
    /// Distinct from [`Self::MalformedKey`]: a package lineage carries no
    /// `#introhex` half, so the two have different valid shapes and an agent
    /// correcting one must not be told the rules of the other.
    #[error("malformed package lineage {package:?}: {reason}")]
    MalformedPackage {
        /// The rejected input, echoed back so an agent can self-correct.
        package: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A tool argument was structurally valid but semantically out of range.
    #[error("invalid argument {argument}: {reason}")]
    InvalidArgument {
        /// The offending argument's name.
        argument: &'static str,
        /// Why it was rejected.
        reason: String,
    },

    /// The engine stream failed or was cancelled mid-flight.
    #[error("engine error: {0}")]
    Engine(#[from] EngineError),

    /// The engine dropped the stream before emitting a terminal event.
    ///
    /// Distinct from [`McpError::Engine`]: this is a broken invariant on our
    /// side of the seam, not a failure the engine reported.
    #[error("engine stream ended without a terminal event")]
    TruncatedStream,

    /// The requested resource URI is not one this server serves.
    #[error("unknown resource uri: {0}")]
    UnknownResource(String),

    // -- Account authentication (see `crate::mcp::account` and `docs/auth.md`) ---------
    //
    // Seven variants rather than one `Account { message }`, because an agent
    // and a human do genuinely different things about each: sign in, unlock a
    // keychain, wait a second, get online, get online *within the week*,
    // replace a revoked key, or wait for the billing period to roll over.
    // Collapsing them into one stringly-typed variant beside the typed ones
    // above would be the exact regression `into_error_data`'s docs describe —
    // structured information thrown away at a boundary and recoverable only by
    // re-parsing prose.
    //
    // Note what none of them carries: a credential, or any fragment of one.
    // Docker Desktop shipped CVE-2025-13743 by serialising an error object that
    // had retained the Hub token that produced it, writing it to the app log,
    // and then bundling that log into a user-exportable diagnostics archive
    // (CWE-532). These variants are `error.data` on a JSON-RPC response — the
    // most quotable surface in the process — so the rule is absolute.
    /// No account credential is configured on this machine.
    #[error("not signed in to nudox")]
    NotSignedIn,

    /// The credential store could not be consulted, so whether a key exists is
    /// unknown.
    ///
    /// Deliberately **not** merged with [`Self::NotSignedIn`]: telling a user
    /// whose keychain is locked to "sign in" sends them to re-enter a key they
    /// already have. The GitHub CLI shipped exactly that conflation and it
    /// presented as unauthenticated requests (cli/cli#13317).
    #[error("the account credential store could not be read: {message}")]
    CredentialStoreUnavailable {
        /// The store's own rendering of the failure.
        message: String,
        /// What the user should do about it.
        help: &'static str,
    },

    /// A credential exists and its first verification has not answered yet.
    ///
    /// The only account denial where retrying the identical call is the right
    /// advice — it lives for about a second after launch on a machine with no
    /// cached verdict.
    #[error("nudox is still verifying this account")]
    AuthorizationPending,

    /// A credential exists and has never once been accepted by the service.
    #[error("this account key has never been verified: {cause}")]
    NeverVerified {
        /// Why the attempt failed.
        cause: String,
    },

    /// Offline past the grace window.
    ///
    /// Recoverable the instant the service is reachable. Worded, and `kind`ed,
    /// so it can never be mistaken for a revocation.
    #[error(
        "nudox has been unable to verify this account for {} days; \
         the {} day offline grace period has ended",
        offline_for.as_secs() / 86_400,
        grace_window.as_secs() / 86_400
    )]
    OfflineGraceExpired {
        /// How long since the last successful verification.
        offline_for: std::time::Duration,
        /// The configured grace window.
        grace_window: std::time::Duration,
        /// When the service last said yes.
        last_verified_at: std::time::SystemTime,
        /// Why re-verification is failing, when we know.
        cause: Option<String>,
    },

    /// The service answered `allowed: false`.
    #[error("nudox rejected this account key: {reason}")]
    KeyRevoked {
        /// The service's own reason, verbatim.
        reason: String,
    },

    /// The account is over its plan limit for the current period.
    ///
    /// A **product state**, not a fault. Everything about how this renders —
    /// the `kind`, the `help`, the numbers in `data` — exists so that the model
    /// reading it stops rather than retries, and the human reading it goes to
    /// billing rather than to a bug tracker.
    #[error("nudox usage limit reached: {used} of {limit} on the {tier} plan")]
    QuotaExceeded {
        /// The plan name.
        tier: String,
        /// Units used this period.
        used: u64,
        /// The plan's ceiling.
        limit: u64,
        /// How many of those units were tool calls.
        tool_calls: u64,
        /// ISO-8601 start of the current period, verbatim from the service.
        period_start: String,
    },

    /// The listener could not be bound.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// The address we tried to bind.
        addr: SocketAddr,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The HTTP server terminated abnormally.
    #[error("mcp http server failed: {0}")]
    Serve(#[source] std::io::Error),

    /// `index_package` could not fetch, verify or produce the named package.
    ///
    /// # Why one variant and not eleven
    ///
    /// [`crate::packages::acquire::Error`] already *is* the taxonomy — one variant per
    /// distinct recoverable situation, each with its own `kind()` and its own
    /// `help()`, defined next to the code that can fail that way. Mirroring
    /// those eleven variants here would give the vocabulary two homes, and the
    /// copy would be the one that rots: a new acquisition failure would reach
    /// an agent as whichever bucket this file happened to have.
    ///
    /// So this variant *delegates*: `kind`, `help` and the structured fields
    /// are all read off the wrapped error, and adding a failure mode in the
    /// engine reaches MCP clients with no edit here — the same argument
    /// [`Self::Engine`] makes for reusing `EngineError`'s own `Serialize`.
    ///
    /// `Box` because `Error` carries a version list and several owned
    /// strings, and clippy's `result_large_err` is *allowed* workspace-wide
    /// rather than fixed — an allowance worth not leaning on further.
    #[error("{error}")]
    Index {
        /// The typed acquisition failure.
        error: Box<crate::packages::acquire::Error>,
    },
}

impl McpError {
    /// Convert to the JSON-RPC error an MCP client sees.
    ///
    /// The `code`/`message` mapping is deliberate rather than uniform: an
    /// agent that gets `invalid_params` knows to fix its arguments and retry,
    /// whereas `internal_error` means retrying the same call is pointless.
    ///
    /// # The `data` field (§L7.4 extension, design doctrine's ariadne study)
    ///
    /// JSON-RPC's error object has a `data` member "defined by the sender"
    /// for exactly this — and until now every call here passed `None`,
    /// discarding every structured field a variant carried the moment it
    /// crossed the wire. An agent parsing `message` prose to recover a
    /// `reason` string it had a moment ago as a real field is the same class
    /// of loss doctrine §8 calls out for `map_err(|_|)`: real information,
    /// thrown away at a boundary, recoverable only by re-parsing what used to
    /// be structured.
    ///
    /// `data` is always `{"kind": ..., "help": ...?, ...variant fields}`.
    /// `kind` is a stable machine tag — distinct from the JSON-RPC `code`,
    /// which only encodes the *category* (`invalid_params` vs
    /// `internal_error`), and distinct from `message`'s prose, which can be
    /// reworded without notice. `help` is ariadne's label-vs-help split
    /// translated to a protocol with no terminal to underline a span in: it
    /// answers "what do I do next", never repeating what `message` already
    /// said happened. Everything else is the variant's own fields, verbatim
    /// — an `EngineError` embeds its own `#[serde(tag = "kind")]` payload
    /// under `engine` unchanged, so a new field added there (e.g.
    /// `SymbolNotFound::possibly_stale`) reaches an agent with no change
    /// needed here.
    pub fn into_error_data(self) -> rmcp::ErrorData {
        let message = self.to_string();
        let data = Some(self.error_data_json());
        match self {
            Self::Unauthenticated => rmcp::ErrorData::invalid_request(message, data),
            Self::MalformedKey { .. }
            | Self::MalformedPackage { .. }
            | Self::InvalidArgument { .. } => rmcp::ErrorData::invalid_params(message, data),
            Self::UnknownResource(_) => rmcp::ErrorData::resource_not_found(message, data),
            // The code encodes the *category*: an index failure the caller can
            // fix by changing its arguments (a misspelled name, a version that
            // does not exist) is `invalid_params`, and everything else — a
            // network failure, a producer that could not lower the package —
            // is `internal_error`. Deriving that from `is_transient` would be
            // the wrong axis: a permanent failure is not necessarily the
            // caller's fault, and `ProducerFailed` is both permanent and
            // nothing the arguments can fix.
            Self::Index { error } => {
                if matches!(
                    *error,
                    crate::packages::acquire::Error::MalformedPurl { .. }
                        | crate::packages::acquire::Error::UnknownPackage { .. }
                        | crate::packages::acquire::Error::VersionNotFound { .. }
                        | crate::packages::acquire::Error::VersionMissing { .. }
                ) {
                    rmcp::ErrorData::invalid_params(message, data)
                } else {
                    rmcp::ErrorData::internal_error(message, data)
                }
            }
            // Every account denial is `invalid_request`, the same code
            // `Unauthenticated` already uses, and for the same reason: the
            // request is well-formed and cannot be served *as this caller*.
            // No new JSON-RPC code is invented — `data.kind` is where the
            // seven-way distinction lives, which is precisely what a stable
            // machine tag is for. An agent that only understands codes gets
            // "do not retry with different arguments"; one that reads `data`
            // gets "you are over quota until the period rolls over".
            Self::NotSignedIn
            | Self::CredentialStoreUnavailable { .. }
            | Self::AuthorizationPending
            | Self::NeverVerified { .. }
            | Self::OfflineGraceExpired { .. }
            | Self::KeyRevoked { .. }
            | Self::QuotaExceeded { .. } => rmcp::ErrorData::invalid_request(message, data),
            Self::Engine(_) | Self::TruncatedStream | Self::Bind { .. } | Self::Serve(_) => {
                rmcp::ErrorData::internal_error(message, data)
            }
        }
    }

    /// The stable machine-readable tag for this variant, independent of
    /// `message`'s wording and coarser-grained `code`.
    fn kind(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::MalformedKey { .. } => "malformed_key",
            Self::MalformedPackage { .. } => "malformed_package",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::Engine(_) => "engine_error",
            Self::TruncatedStream => "truncated_stream",
            Self::UnknownResource(_) => "unknown_resource",
            Self::Bind { .. } => "bind_failed",
            Self::Serve(_) => "serve_failed",
            // Delegated, not mapped: the acquisition taxonomy has one home.
            Self::Index { error } => error.kind(),
            // These seven are the wire spelling of `account::Denial::kind`.
            // `crates/nudox-mcp/tests/account_error_vocabulary.rs` asserts the
            // two agree for every denial, so the mapping cannot drift into two
            // vocabularies for one concept.
            Self::NotSignedIn => "not_signed_in",
            Self::CredentialStoreUnavailable { .. } => "credential_store_unavailable",
            Self::AuthorizationPending => "authorization_pending",
            Self::NeverVerified { .. } => "never_verified",
            Self::OfflineGraceExpired { .. } => "offline_grace_expired",
            Self::KeyRevoked { .. } => "key_revoked",
            Self::QuotaExceeded { .. } => "quota_exceeded",
        }
    }

    /// "What to do next", distinct from `message`'s "what happened" — see
    /// `into_error_data`'s docs. `None` when there genuinely is no next step
    /// beyond what `message` already says (e.g. a truncated stream: retrying
    /// the identical call is reasonable, but there is no input to change).
    fn help(&self) -> Option<&'static str> {
        match self {
            Self::Unauthenticated => None,
            Self::MalformedKey { .. } => Some(
                "Copy this key verbatim from a search_symbols, get_symbol, find_usages, or \
                 graph_query result rather than constructing one by hand.",
            ),
            Self::MalformedPackage { .. } => Some(
                "Call list_packages to see the exact 'ecosystem:name' spelling for every loaded \
                 package.",
            ),
            Self::InvalidArgument { argument, .. } if *argument == "kinds" => {
                Some("Pick from data.validKinds, case-insensitive.")
            }
            Self::InvalidArgument { argument, .. } if *argument == "packages" => Some(
                "Each entry must be an 'ecosystem:name' lineage from list_packages, e.g. \
                 \"cargo:serde\".",
            ),
            Self::InvalidArgument { argument, .. } if *argument == "cursor" => Some(
                "Only pass back a cursor string this same tool returned in next_cursor; do not \
                 construct or edit one.",
            ),
            Self::InvalidArgument { argument, .. } if *argument == "query" => {
                Some("Call graph_schema for the queryable types and edges, then retry.")
            }
            Self::InvalidArgument { .. } => None,
            Self::Engine(EngineError::PackageNotLoaded { attempted, .. }) => Some(if attempted.is_some() {
                "The load for this package was attempted and failed — see data.engine.attempted \
                 for why. Retrying this call will not help until the underlying problem (a bad \
                 manifest, a missing toolchain) is fixed and the package is reloaded."
            } else {
                "This lineage was never loaded, or never existed under this exact \
                 'ecosystem:name' spelling. Call list_packages to see what is actually loaded."
            }),
            Self::Engine(EngineError::SymbolNotFound { possibly_stale }) => Some(if possibly_stale.is_some() {
                "This key resolves under a different loaded generation of the same package — see \
                 data.engine.possibly_stale. It may not have survived a select_version switch \
                 rather than having been deleted; re-search by name with search_symbols, or call \
                 diff_versions to see what it became."
            } else {
                "Re-search by name with search_symbols rather than assuming this key still \
                 identifies a declaration — it may have been renamed, removed, or never existed."
            }),
            Self::Engine(EngineError::GraphQueryFailed { position, .. }) => Some(if position.is_some() {
                "Fix the query at data.engine.position (1-based line:column) and retry. Call \
                 graph_schema first if the problem is an unfamiliar type or edge name."
            } else {
                "Call graph_schema for the exact type, edge, and property names this query must \
                 be written against, then retry."
            }),
            Self::Engine(EngineError::Chunk { .. }) => None,
            Self::Engine(EngineError::Cancelled) => None,
            Self::Engine(_) => None,
            Self::TruncatedStream => None,
            Self::UnknownResource(_) => None,
            Self::Bind { .. } => None,
            Self::Serve(_) => None,
            Self::Index { error } => error.help(),

            // -- Account -----------------------------------------------------
            //
            // Every one of these is `Some`. That is not a coincidence: an
            // account denial the caller can do nothing about would be a design
            // failure, not a message-writing failure. Each sentence names the
            // one action that changes the outcome and nothing else.
            Self::NotSignedIn => Some(
                "Open nudox and sign in with the key from https://nudox.org/dashboard/keys, or \
                 set NUDOX_API_KEY in the environment that launches this MCP server. Retrying \
                 this call will not help until one of those is done.",
            ),
            // Carried on the variant rather than chosen here: the store knows
            // whether it was locked, denied, or absent, and only it can say
            // which of those the user should act on.
            Self::CredentialStoreUnavailable { help, .. } => Some(help),
            Self::AuthorizationPending => Some(
                "This is the only account error worth retrying: verification is in flight and \
                 usually completes within a second. Retry once.",
            ),
            Self::NeverVerified { .. } => Some(
                "This key has never been accepted by nudox on this machine, so there is no \
                 cached authorisation to work from. Connect to the network once, then retry.",
            ),
            Self::OfflineGraceExpired { .. } => Some(
                "This is a connectivity problem, not a revoked key — nothing is wrong with the \
                 account. Connect to the network once and every tool call resumes immediately. \
                 See data.lastVerifiedAt for when the last successful check was.",
            ),
            Self::KeyRevoked { .. } => Some(
                "This key is no longer valid. Create a new one at \
                 https://nudox.org/dashboard/keys and sign in again; retrying with this key \
                 will fail identically.",
            ),
            // The sentence this whole module exists to get right. It says three
            // things in order: this is a plan limit, retrying is pointless, and
            // here are the two things that change it.
            Self::QuotaExceeded { .. } => Some(
                "This is a plan limit, not a bug — the account has used its full allowance for \
                 the current billing period. Every tool call will fail identically until the \
                 period rolls over or the plan is upgraded at \
                 https://nudox.org/dashboard/billing. Do not retry; tell the user.",
            ),
        }
    }

    /// The `data` object's variant-specific fields, before `kind`/`help` are
    /// merged in. `serde_json::Map` (not `Value`) so the merge in
    /// `error_data_json` is a plain key insertion, never a type mismatch.
    fn extra_fields(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::json;

        let obj = match self {
            Self::Unauthenticated => json!({}),
            Self::MalformedKey { key, reason } => json!({
                "input": key,
                "reason": reason,
                "expectedShape": "ecosystem:name#introhex",
                "example": "cargo:serde#3f1a…(64 lowercase hex characters)",
            }),
            Self::MalformedPackage { package, reason } => json!({
                "input": package,
                "reason": reason,
                "expectedShape": "ecosystem:name",
                "example": "cargo:serde",
            }),
            Self::InvalidArgument { argument, reason } => {
                let mut obj = json!({ "argument": argument, "reason": reason });
                if *argument == "kinds" {
                    obj["validKinds"] = json!(crate::mcp::tools::known_kind_names());
                }
                obj
            }
            Self::Engine(engine_err) => json!({
                // `EngineError` derives `Serialize` with `#[serde(tag =
                // "kind", rename_all = "snake_case")]` already — reusing it
                // rather than re-deriving the same fields by hand is what
                // keeps a future `EngineError` field (like
                // `SymbolNotFound::possibly_stale` above) visible here with
                // no matching edit required in this crate.
                "engine": serde_json::to_value(engine_err)
                    .unwrap_or(serde_json::Value::Null),
            }),
            Self::TruncatedStream => json!({}),
            Self::UnknownResource(uri) => json!({ "uri": uri }),
            Self::Bind { addr, source } => json!({
                "addr": addr.to_string(),
                "ioError": source.to_string(),
            }),
            Self::Serve(source) => json!({ "ioError": source.to_string() }),
            // `Error` derives `Serialize` with `#[serde(tag = "kind")]`,
            // so its own fields — the version list on `VersionNotFound`, the
            // producer's error chain on `ProducerFailed` — reach the agent
            // verbatim and a field added there needs no edit here. Flattened
            // into `data` rather than nested under a key, because unlike
            // `Engine` this variant *is* the whole error rather than a wrapper
            // around a lower layer's.
            Self::Index { error } => serde_json::to_value(error.as_ref())
                .unwrap_or_else(|_| json!({})),

            // -- Account -----------------------------------------------------
            //
            // Nothing here is a credential or a fragment of one. `NotSignedIn`
            // and `AuthorizationPending` carry no fields at all, on the same
            // reasoning as `Unauthenticated`: a caller that is not signed in
            // learns only that, and there is nothing else it could act on.
            Self::NotSignedIn | Self::AuthorizationPending => json!({}),
            Self::CredentialStoreUnavailable { message, .. } => json!({ "detail": message }),
            Self::NeverVerified { cause } => json!({ "cause": cause }),
            Self::OfflineGraceExpired {
                offline_for,
                grace_window,
                last_verified_at,
                cause,
            } => json!({
                "offlineForDays": offline_for.as_secs() / 86_400,
                "graceWindowDays": grace_window.as_secs() / 86_400,
                // Epoch seconds rather than a rendered date: this crate has no
                // date library, and formatting one by hand is how a client ends
                // up showing a different instant than the dashboard.
                "lastVerifiedAt": last_verified_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                "cause": cause,
                // Stated in-band so an agent does not have to infer it from
                // prose. The difference between this and `key_revoked` is the
                // difference between "wait for wifi" and "the account is gone",
                // and an agent that guesses wrong tells the user the wrong one.
                "recoverable": true,
            }),
            Self::KeyRevoked { reason } => json!({
                "reason": reason,
                "recoverable": false,
            }),
            Self::QuotaExceeded {
                tier,
                used,
                limit,
                tool_calls,
                period_start,
            } => json!({
                "tier": tier,
                "used": used,
                "limit": limit,
                "remaining": 0,
                "toolCalls": tool_calls,
                "periodStart": period_start,
                // There is deliberately no `periodEnd`: `GET v1/usage` does not
                // return one (`docs/auth.md` § "Gaps in the contract", gap 1), and
                // computing a plausible date from `periodStart` would be a
                // fabrication an agent would repeat to a user as fact.
                "isQuotaNotFault": true,
                "upgradeUrl": "https://nudox.org/dashboard/billing",
            }),
        };
        match obj {
            serde_json::Value::Object(map) => map,
            // Every arm above constructs an object literal; this is
            // unreachable, not a case to design for.
            _ => serde_json::Map::new(),
        }
    }

    /// Assemble the full `error.data` object: `kind`, optional `help`, then
    /// this variant's own fields.
    fn error_data_json(&self) -> serde_json::Value {
        self.error_data_object()
    }

    /// The `error.data` object, reachable from other modules' tests.
    ///
    /// `pub(crate)` rather than `pub`: the wire shape is an output of
    /// [`Self::into_error_data`] and asserting on it is a test's business, not
    /// a caller's. `crate::mcp::account::gate`'s tests use it to prove that every
    /// posture that denies reaches the wire with its own `kind` and a `help`.
    #[cfg(test)]
    pub(crate) fn error_data_for_tests(&self) -> serde_json::Value {
        self.error_data_object()
    }

    fn error_data_object(&self) -> serde_json::Value {
        let mut fields = self.extra_fields();
        fields.insert("kind".to_owned(), serde_json::Value::from(self.kind()));
        if let Some(help) = self.help() {
            fields.insert("help".to_owned(), serde_json::Value::from(help));
        }
        serde_json::Value::Object(fields)
    }
}

impl From<McpError> for rmcp::ErrorData {
    fn from(e: McpError) -> Self {
        e.into_error_data()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::wire::{
        EcosystemId, KeyStaleness, KeyTierName, PackageLineageId, PackageName, QueryErrorPosition,
    };

    use super::*;

    fn lineage(name: &str) -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name))
    }

    /// Every `into_error_data` call must carry a non-empty `data.kind` and a
    /// `data` that is a JSON object — the minimum bar for "structured, not
    /// prettier strings" (doctrine §3 applied to the JSON-RPC seam).
    fn assert_structured(err: McpError) -> serde_json::Value {
        let expected_kind = err.kind().to_owned();
        let rmcp_err = err.into_error_data();
        let data = rmcp_err
            .data
            .clone()
            .expect("every McpError must carry a structured error.data object");
        assert_eq!(
            data.get("kind").and_then(serde_json::Value::as_str),
            Some(expected_kind.as_str()),
            "data.kind must be the stable machine tag, got {data}"
        );
        data
    }

    #[test]
    fn malformed_key_error_data_carries_the_echoed_input_and_expected_shape() {
        let err = McpError::MalformedKey {
            key: "not-a-key".to_owned(),
            reason: "expected 'ecosystem:name#introhex' — no '#' found",
        };
        let data = assert_structured(err);
        assert_eq!(data["kind"], "malformed_key");
        assert_eq!(data["input"], "not-a-key");
        assert_eq!(data["reason"], "expected 'ecosystem:name#introhex' — no '#' found");
        assert_eq!(data["expectedShape"], "ecosystem:name#introhex");
        assert!(
            data.get("help").is_some(),
            "a malformed key must come with a help string distinct from message"
        );
    }

    #[test]
    fn invalid_kind_argument_error_data_lists_every_valid_kind() {
        let err = McpError::InvalidArgument {
            argument: "kinds",
            reason: "\"bogus\" is not a known kind; expected one of Module, ...".to_owned(),
        };
        let data = assert_structured(err);
        let valid_kinds = data["validKinds"]
            .as_array()
            .expect("kinds InvalidArgument must carry data.validKinds");
        let names: Vec<&str> = valid_kinds.iter().filter_map(|v| v.as_str()).collect();
        for expected in ["Function", "Trait", "Enum"] {
            assert!(
                names.contains(&expected),
                "validKinds must include {expected}, got {names:?}"
            );
        }
    }

    #[test]
    fn package_not_loaded_without_a_failed_load_gets_the_never_seen_help() {
        let err = McpError::Engine(crate::wire::EngineError::PackageNotLoaded {
            package: lineage("nonexistent"),
            attempted: None,
        });
        let data = assert_structured(err);
        assert_eq!(data["kind"], "engine_error");
        assert_eq!(data["engine"]["kind"], "package_not_loaded");
        assert!(data["engine"]["attempted"].is_null());
        let help = data["help"].as_str().expect("help must be present");
        assert!(
            help.contains("never loaded") || help.contains("never existed"),
            "help for an unattempted package must say so, got {help:?}"
        );
    }

    #[test]
    fn package_not_loaded_with_a_failed_load_surfaces_the_reason_and_different_help() {
        let err = McpError::Engine(crate::wire::EngineError::PackageNotLoaded {
            package: lineage("broken"),
            attempted: Some("oracle failed for cargo:broken: exit 1".into()),
        });
        let data = assert_structured(err);
        assert_eq!(
            data["engine"]["attempted"], "oracle failed for cargo:broken: exit 1",
            "the recorded failure reason must reach the structured data verbatim"
        );
        let help = data["help"].as_str().expect("help must be present");
        assert!(
            help.contains("attempted and failed") || help.contains("Retrying"),
            "help for a failed load must say retrying will not help, got {help:?}"
        );
    }

    #[test]
    fn symbol_not_found_with_a_stale_hint_surfaces_the_tier_and_version() {
        let err = McpError::Engine(crate::wire::EngineError::SymbolNotFound {
            possibly_stale: Some(KeyStaleness {
                seen_in_version: "2.7.6".into(),
                tier: Some(KeyTierName::Ordinal),
            }),
        });
        let data = assert_structured(err);
        assert_eq!(data["engine"]["possibly_stale"]["seen_in_version"], "2.7.6");
        assert_eq!(data["engine"]["possibly_stale"]["tier"], "Ordinal");
        let help = data["help"].as_str().expect("help must be present");
        assert!(
            help.contains("select_version") || help.contains("different loaded generation"),
            "help for a stale-key hit must point at the version-switch explanation, got {help:?}"
        );
    }

    #[test]
    fn symbol_not_found_without_a_stale_hint_gets_a_plain_research_help() {
        let err = McpError::Engine(crate::wire::EngineError::SymbolNotFound {
            possibly_stale: None,
        });
        let data = assert_structured(err);
        assert!(data["engine"]["possibly_stale"].is_null());
        let help = data["help"].as_str().expect("help must be present");
        assert!(help.contains("search_symbols"));
    }

    #[test]
    fn graph_query_failed_with_a_position_surfaces_line_and_column() {
        let err = McpError::Engine(crate::wire::EngineError::GraphQueryFailed {
            message: "Unrecognized directive foo".to_owned(),
            position: Some(QueryErrorPosition { line: 3, column: 12 }),
        });
        let data = assert_structured(err);
        assert_eq!(data["engine"]["position"]["line"], 3);
        assert_eq!(data["engine"]["position"]["column"], 12);
        let help = data["help"].as_str().expect("help must be present");
        assert!(
            help.contains("data.engine.position"),
            "help must point the agent at the position field, got {help:?}"
        );
    }

    #[test]
    fn every_current_engine_error_variant_gets_a_distinct_engine_kind_tag() {
        // Pins that the nested `data.engine.kind` tag actually varies per
        // variant — the whole point of reusing `EngineError`'s own
        // `#[serde(tag = "kind")]` rather than re-deriving these fields.
        let cases: Vec<(&str, crate::wire::EngineError)> = vec![
            (
                "package_not_loaded",
                crate::wire::EngineError::PackageNotLoaded {
                    package: lineage("x"),
                    attempted: None,
                },
            ),
            (
                "symbol_not_found",
                crate::wire::EngineError::SymbolNotFound {
                    possibly_stale: None,
                },
            ),
            (
                "graph_query_failed",
                crate::wire::EngineError::GraphQueryFailed {
                    message: "bad query".to_owned(),
                    position: None,
                },
            ),
            (
                "chunk",
                crate::wire::EngineError::Chunk {
                    message: "chunker returned None for a live entry".to_owned(),
                },
            ),
            ("cancelled", crate::wire::EngineError::Cancelled),
        ];
        for (expected_tag, engine_err) in cases {
            let data = assert_structured(McpError::Engine(engine_err));
            assert_eq!(
                data["engine"]["kind"], expected_tag,
                "data.engine.kind must be {expected_tag:?}, got {data}"
            );
        }
    }

    #[test]
    fn unauthenticated_carries_no_variant_fields_but_still_a_kind() {
        // §L6: a caller that guessed the token wrong must learn only that it
        // guessed wrong — this pins that the structured `data` upgrade did
        // not accidentally leak anything beyond the bare kind tag.
        let data = assert_structured(McpError::Unauthenticated);
        assert_eq!(data.as_object().map(|m| m.len()), Some(1));
    }
}
