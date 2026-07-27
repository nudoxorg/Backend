//! LR-11 typestates: `Unauthenticated` and [`Session`] are different types.
//!
//! # Why a typestate and not a `bool`
//!
//! §L6 requires a per-launch session token on every request. The obvious
//! implementation — an `authenticated: bool` on a request context — is exactly
//! the shape LR-11 forbids, because nothing stops a later refactor from
//! reading a tool's arguments before the flag is checked.
//!
//! Here the two states are distinct types with different legal operations:
//!
//! * [`Unauthenticated`] is what arrives off the wire. It can be *inspected*
//!   (which credential was presented) and *authenticated*. It cannot be used
//!   to serve a request, because nothing in this crate accepts one.
//! * [`Session`] is proof that the presented credential matched the launch
//!   token. Its constructor is private to this module, so the only way to
//!   obtain one is [`Unauthenticated::authenticate`]. It cannot be forged,
//!   defaulted, or deserialized.
//!
//! The HTTP layer (`crate::endpoint`) therefore *cannot* forward a request to
//! the tool router without first producing a `Session`, and that fact is
//! checked by the compiler rather than by review.
//!
//! # Threat model
//!
//! The listener is loopback-only and non-configurable (§L6), so the token is
//! not defending against a network attacker — it defends against *other local
//! processes* on a shared machine, which can otherwise reach any 127.0.0.1
//! port. That makes a 128-bit per-launch secret and a constant-time compare
//! the right weight: enough that another user's process cannot guess or time
//! its way in, without dragging TLS into a GUI's startup path.

use std::fmt;

use axum::http::HeaderMap;

use crate::error::McpError;

/// The name of the non-standard header accepted in addition to
/// `Authorization: Bearer …`.
///
/// Some MCP clients cannot set `Authorization` on a streamable-HTTP transport
/// but can set arbitrary headers; accepting both keeps the server usable
/// without weakening anything (the same secret is required either way).
pub const TOKEN_HEADER: &str = "x-nudox-session";

/// The per-launch shared secret.
///
/// Generated once when `lindsey` starts the server and shown in
/// Settings → Connection (GUI-PLAN §21) so the user can paste a working client
/// config. It is *not* persisted: a new launch means a new token, so a stale
/// config fails closed rather than silently attaching to a different corpus.
#[derive(Clone)]
pub struct SessionToken {
    secret: String,
}

impl SessionToken {
    /// Generate a fresh 128-bit token, hex-encoded.
    ///
    /// 128 bits is the standard bearer-secret width; at loopback speeds an
    /// online guessing attack is not meaningfully bounded by anything else.
    pub fn generate() -> Self {
        let hi: u64 = rand::random();
        let lo: u64 = rand::random();
        Self { secret: format!("{hi:016x}{lo:016x}") }
    }

    /// Wrap an externally supplied secret.
    ///
    /// Exists for tests and for a future "reuse the previous token" setting;
    /// production callers should use [`SessionToken::generate`].
    pub fn from_secret(secret: impl Into<String>) -> Self {
        Self { secret: secret.into() }
    }

    /// The secret in the form a client must present.
    ///
    /// Only Settings → Connection and the client-config snippet call this.
    pub fn expose(&self) -> &str {
        &self.secret
    }

    /// The `Authorization` header value a client must send.
    pub fn bearer_header_value(&self) -> String {
        format!("Bearer {}", self.secret)
    }
}

/// Redacted: a token must never reach a log line or a panic message.
impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// A credential as it arrived off the wire — unverified.
///
/// Deliberately borrows the header map rather than owning a `String`, so the
/// secret is never copied into a longer-lived allocation on the failure path.
#[derive(Debug, Clone, Copy)]
pub struct Unauthenticated<'h> {
    presented: Option<&'h str>,
}

impl<'h> Unauthenticated<'h> {
    /// Extract whatever credential the request offered, if any.
    ///
    /// Accepts `Authorization: Bearer <token>` or [`TOKEN_HEADER`]. A request
    /// with neither yields `presented: None`, which always fails
    /// authentication — there is no anonymous path.
    pub fn from_headers(headers: &'h HeaderMap) -> Self {
        let bearer = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);

        let custom = headers
            .get(TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim);

        Self { presented: bearer.or(custom) }
    }

    /// Construct directly from a presented credential (tests, and any future
    /// non-HTTP transport).
    pub fn from_presented(presented: Option<&'h str>) -> Self {
        Self { presented }
    }

    /// `true` when the request offered no credential at all.
    ///
    /// Distinguishes "client is not configured" from "client is wrong" for
    /// diagnostics only — both outcomes are rejected identically.
    pub fn is_absent(&self) -> bool {
        self.presented.is_none()
    }

    /// The only way to obtain a [`Session`].
    ///
    /// Compares in constant time with respect to the *contents* of the
    /// credential, so a caller cannot binary-search the secret by measuring
    /// response latency.
    pub fn authenticate(self, expected: &SessionToken) -> Result<Session, McpError> {
        let presented = self.presented.ok_or(McpError::Unauthenticated)?;
        if constant_time_eq(presented.as_bytes(), expected.secret.as_bytes()) {
            Ok(Session { _private: () })
        } else {
            Err(McpError::Unauthenticated)
        }
    }
}

/// Proof that the caller presented the launch token.
///
/// Carries no data: its *existence* is the whole payload. The private field
/// makes it unconstructible outside this module, so every value of this type
/// is genuinely the result of a successful [`Unauthenticated::authenticate`].
///
/// `Clone` and `'static` so it can be parked in an `http::Extensions`, which is
/// how the transport layer hands the proof down to whatever runs next.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    _private: (),
}

/// Compare two byte strings without an early exit.
///
/// Lengths are compared up front — that leaks the *length* of the presented
/// credential, which is harmless (the token's length is a public constant) and
/// is what every constant-time string compare does.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_128_bit_hex_and_distinct() {
        let a = SessionToken::generate();
        let b = SessionToken::generate();
        assert_eq!(a.expose().len(), 32, "128 bits = 32 hex chars");
        assert!(a.expose().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.expose(), b.expose(), "each launch gets a fresh token");
    }

    #[test]
    fn absent_credential_is_rejected() {
        let token = SessionToken::generate();
        let cred = Unauthenticated::from_presented(None);
        assert!(cred.is_absent());
        assert!(cred.authenticate(&token).is_err());
    }

    #[test]
    fn wrong_credential_is_rejected() {
        let token = SessionToken::from_secret("aaaa");
        let cred = Unauthenticated::from_presented(Some("bbbb"));
        assert!(cred.authenticate(&token).is_err());
    }

    #[test]
    fn matching_credential_yields_a_session() {
        let token = SessionToken::generate();
        let secret = token.expose().to_owned();
        let cred = Unauthenticated::from_presented(Some(&secret));
        assert!(cred.authenticate(&token).is_ok());
    }

    #[test]
    fn bearer_and_custom_headers_are_both_accepted() {
        let token = SessionToken::from_secret("s3cret");
        let mut bearer = HeaderMap::new();
        bearer.insert(
            axum::http::header::AUTHORIZATION,
            token.bearer_header_value().parse().expect("valid header value"),
        );
        assert!(Unauthenticated::from_headers(&bearer).authenticate(&token).is_ok());

        let mut custom = HeaderMap::new();
        custom.insert(TOKEN_HEADER, "s3cret".parse().expect("valid header value"));
        assert!(Unauthenticated::from_headers(&custom).authenticate(&token).is_ok());

        let empty = HeaderMap::new();
        assert!(Unauthenticated::from_headers(&empty).authenticate(&token).is_err());
    }

    #[test]
    fn token_debug_is_redacted() {
        let token = SessionToken::from_secret("super-secret");
        assert!(!format!("{token:?}").contains("super-secret"));
    }
}
