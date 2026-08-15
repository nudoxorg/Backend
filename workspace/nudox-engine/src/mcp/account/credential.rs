//! The account credential — an `ndx_` API key — and the rules for accepting one
//! *before* any network call happens.
//!
//! # This is not [`crate::mcp::session::SessionToken`], and the difference is the point
//!
//! Two secrets exist in this process and they are deliberately different types
//! in different modules with no conversion between them:
//!
//! | | [`crate::mcp::session::SessionToken`] | [`ApiKey`] |
//! |---|---|---|
//! | What it authenticates | a *transport*: one local process talking to this loopback socket | an *account*: this installation, against `api.nudox.org` |
//! | Lifetime | one launch — regenerated every start | until the user revokes it in the dashboard |
//! | Where it lives | RAM only, never written anywhere | the macOS Keychain (see [`super::store`]) |
//! | Who may see it | anyone who can already read this user's screen; it is *printed* in Settings → Connection | nobody, ever, including the user's own log files |
//! | Blast radius if leaked | another local process reads this machine's corpus | someone else spends this user's quota |
//!
//! The last row is why `SessionToken` may appear verbatim in
//! [`crate::mcp::endpoint::McpEndpoint::client_config_snippet`] and `ApiKey` must
//! never appear anywhere. `tests/account_secret_hygiene.rs` asserts that the
//! snippet does not contain the account key, so the two cannot be merged by a
//! later refactor without a test going red.
//!
//! # Why validate at all before calling `POST v1/authorize`
//!
//! Because the overwhelmingly common failure is a *paste* failure, not a
//! revocation: a truncated copy, a stray quote from a JSON config, a trailing
//! newline, the word `Bearer` copied along with the value. Every one of those
//! is detectable with zero network and zero latency, and the resulting message
//! can say what is wrong with the *input* rather than reporting the service's
//! generic `"invalid token"` — which is what the user would otherwise see for
//! all of them (GitHub's stated rationale for token prefixes and checksums:
//! <https://github.blog/engineering/platform-security/behind-githubs-new-authentication-token-formats/>).
//!
//! We can do the prefix and shape half of that. We cannot do the checksum half:
//! GitHub's `ghp_` tokens carry a CRC32 in their last six base62 characters
//! precisely so a client can reject a mistyped token offline, and `ndx_` keys
//! carry no such thing. See `docs/auth.md` § "Gaps in the contract" — adopting one is
//! a server-side change, not something this module can decide alone.

use std::fmt;

/// The prefix every nudox account key carries.
///
/// Its job is discrimination, not secrecy: a value without it is *certainly*
/// not an account key, so the user can be told they pasted the wrong thing —
/// most often the loopback session token, which is bare hex.
pub const API_KEY_PREFIX: &str = "ndx_";

/// Shortest value we will send to the service.
///
/// `ndx_` plus a body short enough to be a truncated paste is not worth a round
/// trip. Chosen to be comfortably below any plausible real key while still
/// rejecting the observed truncation failures (a half-selected double-click).
const MIN_LEN: usize = 16;

/// Longest value we will accept.
///
/// A bound exists so a runaway paste — a whole config file, a log line — is
/// rejected as a shape error instead of being posted to the service and echoed
/// back into an error message.
const MAX_LEN: usize = 256;

/// How many trailing characters of a key are safe to show a human.
///
/// Four is the industry-standard "last four" used for cards and for every
/// dashboard that lists API keys. It is enough for a user to tell two keys
/// apart and far too little to reconstruct one.
const TAIL_LEN: usize = 4;

// ---------------------------------------------------------------------------
// ApiKey
// ---------------------------------------------------------------------------

/// A validated `ndx_` account key.
///
/// Constructible only through [`ApiKey::parse`], so a value of this type has
/// already passed every offline check. That is a typestate, not a convenience:
/// the service client accepts an `ApiKey` and nothing else, so "we sent an
/// unvalidated string to `api.nudox.org`" is unrepresentable.
///
/// # What protects it
///
/// * [`fmt::Debug`] is hand-written and redacted — the same discipline as
///   [`crate::mcp::session::SessionToken`], for the same reason (a `{:?}` in a
///   `tracing` call, a panic payload, a crash report).
/// * There is **no** [`fmt::Display`], so `{key}` does not compile.
/// * There is **no** `Serialize`/`Deserialize`. The key cannot be written into
///   a config file, a JSON error body, or a telemetry payload by accident,
///   because there is no code path that would do it.
/// * [`ApiKey::expose`] is the single accessor, named to make its call sites
///   greppable. Today it has exactly two: building the `Authorization` header,
///   and writing to the Keychain.
///
/// # What deliberately does *not* protect it
///
/// The secret is a plain `String` and is **not** zeroized on drop, and this
/// module does not claim otherwise. `String` reallocates and moves; a
/// `Drop` impl that overwrites the final buffer would leave every earlier
/// buffer intact and would buy a *claim* of scrubbing rather than scrubbing.
/// The credential's real defence is that it is at rest in the Keychain and in
/// process memory only while the process runs — see `docs/auth.md` § "Key hygiene".
#[derive(Clone)]
pub struct ApiKey {
    secret: String,
}

impl ApiKey {
    /// Validate a user-supplied string and wrap it.
    ///
    /// # Normalisation
    ///
    /// Leading and trailing ASCII whitespace is stripped, because a key pasted
    /// out of a dashboard or a shell almost always arrives with a newline and
    /// rejecting that would be user-hostile theatre. *Interior* whitespace is
    /// rejected rather than stripped: a space in the middle of a key means the
    /// selection was wrong, and silently deleting it would turn a diagnosable
    /// paste error into a mysterious `invalid token` from the server.
    /// # Check order
    ///
    /// The order below is chosen so that each real-world paste failure lands on
    /// the check whose message names it, rather than on whichever check happens
    /// to fire first:
    ///
    /// 1. `Empty` — nothing, or only whitespace.
    /// 2. `TooLong` — a whole config file was pasted; reporting "unexpected `{`
    ///    at position 0" for that would be technically true and useless.
    /// 3. `MissingPrefix` — catches `"ndx_…"` (JSON string literal),
    ///    `Bearer ndx_…`, and the loopback session token pasted into the wrong
    ///    field, all with one message that names all three.
    /// 4. `TooShort` — a truncated selection.
    /// 5. `IllegalCharacter` — what is left is a smart dash or an embedded
    ///    newline, i.e. a defect *inside* an otherwise well-shaped key.
    pub fn parse(raw: &str) -> Result<Self, ApiKeyError> {
        let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(ApiKeyError::Empty);
        }
        if trimmed.len() > MAX_LEN {
            return Err(ApiKeyError::TooLong {
                len: trimmed.len(),
                max: MAX_LEN,
            });
        }
        if !trimmed.starts_with(API_KEY_PREFIX) {
            return Err(ApiKeyError::MissingPrefix {
                expected: API_KEY_PREFIX,
            });
        }
        if trimmed.len() < MIN_LEN {
            return Err(ApiKeyError::TooShort {
                len: trimmed.len(),
                min: MIN_LEN,
            });
        }
        if let Some((position, class)) = first_illegal(trimmed) {
            return Err(ApiKeyError::IllegalCharacter { position, class });
        }

        Ok(Self {
            secret: trimmed.to_owned(),
        })
    }

    /// The secret, for the two callers that genuinely need it.
    ///
    /// Named `expose` rather than `as_str` so that every place the raw value
    /// escapes this type is one grep away. Adding a third call site is a review
    /// decision, not an accident.
    pub fn expose(&self) -> &str {
        &self.secret
    }

    /// The `Authorization` header value for a request to `api.nudox.org`.
    pub fn bearer_header_value(&self) -> String {
        format!("Bearer {}", self.secret)
    }

    /// A stable, non-reversible identifier for this key.
    ///
    /// Used for two things, both of which need to name a key without holding
    /// one: binding cached authorisation state to the credential that earned it
    /// (see [`super::state`]), and giving support a value the user can quote.
    pub fn fingerprint(&self) -> KeyFingerprint {
        KeyFingerprint::of(&self.secret)
    }

    /// How this key should be spelled in UI: `ndx_…` plus the last four
    /// characters.
    ///
    /// This is what a signed-in state shows. It is *not* a fingerprint — two
    /// keys can share a tail — so it identifies a key to the human who created
    /// it, and [`KeyFingerprint`] identifies it to us.
    pub fn display_hint(&self) -> String {
        let tail: String = self
            .secret
            .chars()
            .rev()
            .take(TAIL_LEN)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{API_KEY_PREFIX}…{tail}")
    }
}

/// Redacted: an account key must never reach a log line, a panic message, or a
/// crash report. Mirrors [`crate::mcp::session::SessionToken`]'s impl exactly.
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Compares in constant time, so an `ApiKey` can be checked against another
/// without leaking where they diverge.
///
/// This exists because `PartialEq` deriving would give a short-circuiting
/// compare, and the type is a secret. The only current caller is
/// "did the credential change?" in [`super::gate`], where timing is irrelevant
/// — but a derived impl would be the thing a future caller reached for.
impl PartialEq for ApiKey {
    fn eq(&self, other: &Self) -> bool {
        crate::mcp::session::constant_time_eq(self.secret.as_bytes(), other.secret.as_bytes())
    }
}

impl Eq for ApiKey {}

// ---------------------------------------------------------------------------
// KeyFingerprint
// ---------------------------------------------------------------------------

/// The first 64 bits of `SHA-256(key)`, hex-encoded.
///
/// Sixteen hex characters is short enough to show in a UI and long enough that
/// two keys colliding is not a thing that happens. It is a one-way function of
/// the key, so persisting one is not persisting a credential — which is what
/// lets the cached-authorisation file live in plain JSON next to the app's
/// other state.
#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct KeyFingerprint(String);

impl KeyFingerprint {
    /// Hash a secret into its fingerprint.
    fn of(secret: &str) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(secret.as_bytes());
        let mut hex = String::with_capacity(16);
        for byte in &digest[..8] {
            use fmt::Write as _;
            // Infallible: writing to a `String` cannot fail, and §L7.6 bans
            // `unwrap` outside tests, so the impossible branch is a no-op.
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    /// The hex form, for display and for the cache file.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Safe to print: a fingerprint is not a credential.
impl fmt::Debug for KeyFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyFingerprint({})", self.0)
    }
}

impl fmt::Display for KeyFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// ApiKeyError
// ---------------------------------------------------------------------------

/// Why an offered string is not an account key.
///
/// Exhaustive on purpose (doctrine §3's `ProducerError` exception): this enum
/// is internal to the workspace, and a new shape check must break every match
/// so each reader decides what the new rejection means to it — the sign-in
/// form's inline hint and the MCP error's `help` are not the same sentence.
///
/// **No variant carries the rejected value.** That is the whole reason this is
/// not `MalformedKey { key, reason }` like [`crate::mcp::error::McpError`]'s symbol
/// key error: echoing the input back is exactly right for a symbol key an agent
/// mangled, and exactly wrong for a credential. What a user needs is *what is
/// wrong with it*, and the position is enough for that.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyError {
    /// Nothing was supplied, or only whitespace was.
    #[error("no API key was supplied")]
    Empty,

    /// The value does not begin with `ndx_`.
    #[error("an API key must begin with `{expected}`")]
    MissingPrefix {
        /// The prefix that was expected — quoted back so the message is
        /// self-contained if [`API_KEY_PREFIX`] ever changes.
        expected: &'static str,
    },

    /// Too short to be a real key — almost always a truncated selection.
    #[error("this key is {len} characters; a nudox key is at least {min}")]
    TooShort {
        /// The length of what was supplied.
        len: usize,
        /// The minimum this build accepts.
        min: usize,
    },

    /// Too long to be a key — almost always a whole config file was pasted.
    #[error("this key is {len} characters; a nudox key is at most {max}")]
    TooLong {
        /// The length of what was supplied.
        len: usize,
        /// The maximum this build accepts.
        max: usize,
    },

    /// A character appeared that no key contains.
    #[error("unexpected {class} at position {position}")]
    IllegalCharacter {
        /// 0-based index of the offending character, so a UI can point at it.
        position: usize,
        /// What *kind* of character it was. Deliberately not the character
        /// itself: an arbitrary byte out of a secret is part of the secret,
        /// and the class is what makes the failure diagnosable.
        class: CharClass,
    },
}

impl ApiKeyError {
    /// One sentence telling the user what to do, distinct from what went wrong.
    ///
    /// Same split as [`crate::mcp::error::McpError`]'s `help` vs `message`: this
    /// answers "what next", and never repeats the `Display` text.
    pub fn help(&self) -> &'static str {
        match self {
            Self::Empty => "Paste the key from https://nudox.org/dashboard/keys.",
            Self::MissingPrefix { .. } => {
                "Every nudox key starts with `ndx_`. Paste the value only — not the surrounding \
                 quotes from a JSON config, not the word `Bearer`, and not the bare hex token \
                 from Settings → Connection, which is the local MCP session token and a \
                 different secret entirely."
            }
            Self::TooShort { .. } => {
                "The paste looks truncated — select the whole value in the dashboard, or use its \
                 copy button."
            }
            Self::TooLong { .. } => {
                "Paste only the key itself, not the config file it lives in."
            }
            Self::IllegalCharacter { class, .. } => match class {
                CharClass::Whitespace => {
                    "There is a space or newline inside the key — the selection picked up more \
                     than one line."
                }
                CharClass::Quote => {
                    "Drop the quotes; paste the value, not the JSON string literal."
                }
                CharClass::NonAscii => {
                    "A smart quote or dash got in — copy from the dashboard rather than from a \
                     document or a chat message."
                }
                CharClass::Other => {
                    "Copy the key again from the dashboard; something other than the key came \
                     with it."
                }
            },
        }
    }
}

/// The category of an unexpected character, reported instead of the character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharClass {
    /// A space, tab, or newline inside the value.
    Whitespace,
    /// A `"` or `'` — the value was copied as a string literal.
    Quote,
    /// A non-ASCII character — typically a smart quote or an en dash from a
    /// word processor or a chat client.
    NonAscii,
    /// Anything else outside `[A-Za-z0-9_-]`.
    Other,
}

impl fmt::Display for CharClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Whitespace => "whitespace",
            Self::Quote => "quote character",
            Self::NonAscii => "non-ASCII character",
            Self::Other => "character",
        })
    }
}

/// Find the first character outside the key alphabet, if any.
///
/// The alphabet is `[A-Za-z0-9_-]`: base62 plus the two separators every
/// prefixed-token scheme in the wild uses. Position is a `char` index rather
/// than a byte index so a UI can place a caret without re-deriving it.
fn first_illegal(s: &str) -> Option<(usize, CharClass)> {
    s.char_indices().enumerate().find_map(|(position, (_, c))| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            None
        } else if c.is_whitespace() {
            Some((position, CharClass::Whitespace))
        } else if c == '"' || c == '\'' {
            Some((position, CharClass::Quote))
        } else if !c.is_ascii() {
            Some((position, CharClass::NonAscii))
        } else {
            Some((position, CharClass::Other))
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A syntactically plausible key. Not a real credential — it is 40
    /// characters of fixed text, and nothing in this crate ever sends it
    /// anywhere but a loopback fake.
    fn sample() -> &'static str {
        "ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517"
    }

    #[test]
    fn a_well_formed_key_parses_and_keeps_its_value() {
        let key = ApiKey::parse(sample()).expect("sample key is well formed");
        assert_eq!(key.expose(), sample());
        assert_eq!(key.bearer_header_value(), format!("Bearer {}", sample()));
    }

    #[test]
    fn surrounding_whitespace_is_stripped_but_interior_whitespace_is_rejected() {
        let padded = format!("  {}\n", sample());
        assert_eq!(
            ApiKey::parse(&padded).expect("a trailing newline is a paste, not an error").expose(),
            sample(),
            "a key pasted with a trailing newline must be accepted verbatim once trimmed"
        );

        let split = sample().replacen("41a9", "41 a9", 1);
        assert_eq!(&split[8..13], "41 a9", "the fixture must place the space at index 10");
        match ApiKey::parse(&split) {
            Err(ApiKeyError::IllegalCharacter { position, class }) => {
                assert_eq!(class, CharClass::Whitespace);
                assert_eq!(
                    position, 10,
                    "the reported position must be where the space actually is, so a caret can \
                     be placed on it"
                );
            }
            other => panic!("interior whitespace must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn every_rejection_names_the_specific_defect_not_a_generic_failure() {
        // Doctrine §4: assert on the typed variant and its content, never on
        // `is_err()`. Each case is a real paste failure observed in the wild.
        let long = format!("ndx_{}", "a".repeat(MAX_LEN));
        let cases: Vec<(String, ApiKeyError)> = vec![
            (String::new(), ApiKeyError::Empty),
            ("   \n ".to_owned(), ApiKeyError::Empty),
            (
                // The loopback session token, pasted into the wrong field.
                "0123456789abcdef0123456789abcdef".to_owned(),
                ApiKeyError::MissingPrefix {
                    expected: API_KEY_PREFIX,
                },
            ),
            (
                format!("Bearer {}", sample()),
                ApiKeyError::MissingPrefix {
                    expected: API_KEY_PREFIX,
                },
            ),
            (
                "ndx_short".to_owned(),
                ApiKeyError::TooShort {
                    len: 9,
                    min: MIN_LEN,
                },
            ),
            (
                long.clone(),
                ApiKeyError::TooLong {
                    len: long.len(),
                    max: MAX_LEN,
                },
            ),
            (
                // A key copied out of a JSON config, quotes and all. This is
                // `MissingPrefix` rather than `IllegalCharacter` *by design* —
                // see `ApiKey::parse`'s check-order note. One message names
                // quotes, `Bearer`, and the session token together, because a
                // user who made one of those mistakes cannot be told which.
                format!("\"{}\"", sample()),
                ApiKeyError::MissingPrefix {
                    expected: API_KEY_PREFIX,
                },
            ),
            (
                // A quote *inside* an otherwise well-shaped key does reach the
                // charset check.
                format!("ndx_2f8c\"41a9{}", "b60d47e3a5710c9fbe2d836a4517"),
                ApiKeyError::IllegalCharacter {
                    position: 8,
                    class: CharClass::Quote,
                },
            ),
            (
                sample().replacen("2f8c", "2f\u{2013}c", 1),
                ApiKeyError::IllegalCharacter {
                    position: 6,
                    class: CharClass::NonAscii,
                },
            ),
        ];

        for (input, expected) in cases {
            let err = ApiKey::parse(&input)
                .map(|_| ())
                .expect_err("must be rejected");
            assert_eq!(
                err, expected,
                "input of {} characters was rejected with the wrong reason",
                input.len()
            );
            assert!(
                !err.help().is_empty(),
                "every rejection must tell the user what to do next"
            );
        }
    }

    #[test]
    fn no_rejection_ever_echoes_the_rejected_value() {
        // The one hard rule this type has that `MalformedKey` deliberately does
        // not: the input is a credential, so it must not appear in the message,
        // the help, or the Debug output of the error.
        let secretish = "ndx_THISISTHESECRETVALUE\u{2013}NEVERPRINTME";
        let err = ApiKey::parse(secretish).expect_err("smart dash is rejected");
        for rendering in [format!("{err}"), format!("{err:?}"), err.help().to_owned()] {
            assert!(
                !rendering.contains("THISISTHESECRETVALUE"),
                "a credential leaked into an error rendering: {rendering}"
            );
        }
    }

    #[test]
    fn debug_is_redacted_and_there_is_no_display() {
        let key = ApiKey::parse(sample()).expect("sample parses");
        let debug = format!("{key:?}");
        assert_eq!(debug, "ApiKey(<redacted>)");
        assert!(!debug.contains("2f8c"), "no fragment of the key may appear");
        // There is deliberately no `impl Display for ApiKey`; if one is ever
        // added, `format!("{key}")` starts compiling and this comment is the
        // only thing that would have objected. The `tests/account_secret_
        // hygiene.rs` suite is the enforceable half.
    }

    #[test]
    fn the_fingerprint_is_stable_one_way_and_key_specific() {
        let key = ApiKey::parse(sample()).expect("sample parses");
        let other = ApiKey::parse("ndx_0000000000000000000000000000000000000000")
            .expect("second key parses");

        assert_eq!(
            key.fingerprint().as_str(),
            key.fingerprint().as_str(),
            "the same key must fingerprint identically across calls, or cached \
             authorisation would never match"
        );
        assert_ne!(key.fingerprint(), other.fingerprint());
        assert_eq!(key.fingerprint().as_str().len(), 16);
        assert!(key.fingerprint().as_str().chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            !key.fingerprint().as_str().contains("2f8c"),
            "a fingerprint that contained a prefix of the key would not be one-way"
        );
    }

    #[test]
    fn the_display_hint_shows_four_characters_and_no_more() {
        let key = ApiKey::parse(sample()).expect("sample parses");
        assert_eq!(key.display_hint(), "ndx_…4517");
        assert!(
            !key.display_hint().contains("2f8c41a9"),
            "the hint must not reveal the body of the key"
        );
    }

    #[test]
    fn keys_compare_by_value_without_a_derived_short_circuit() {
        let a = ApiKey::parse(sample()).expect("parses");
        let b = ApiKey::parse(sample()).expect("parses");
        let c = ApiKey::parse("ndx_0000000000000000000000000000000000000000").expect("parses");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
