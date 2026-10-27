//! What an `ndx_` key is.
//!
//! The auth service mints `ndx_` followed by 64 lowercase hex digits and shows
//! it exactly once. Checking that shape before any network call turns the
//! common paste mistakes (a stray quote, half the key, the key's label) into a
//! sentence that says which one happened.

use std::fmt;

/// Every key starts with this.
pub const API_KEY_PREFIX: &str = "ndx_";
const HEX_DIGITS: usize = 64;

/// A syntactically valid API key. Its `Debug` and `Display` never print it.
#[derive(Clone, Eq, PartialEq)]
pub struct ApiKey(String);

/// Why pasted text is not a key.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KeyError {
    /// Nothing was entered.
    #[error("enter your API key")]
    Empty,
    /// The text does not start with `ndx_`.
    #[error("Nudox API keys start with ndx_")]
    MissingPrefix,
    /// The text after `ndx_` is the wrong length, usually a partial copy.
    #[error("that key is {found} characters after ndx_; a full key has 64 — copy it again")]
    WrongLength {
        /// Characters found after the prefix.
        found: usize,
    },
    /// The text after `ndx_` contains something other than hex digits.
    #[error("that key contains characters a Nudox key never has — copy it again")]
    NotHex,
}

impl ApiKey {
    /// Admits pasted text as a key, ignoring surrounding whitespace.
    ///
    /// # Errors
    /// Returns which part of the key's shape is wrong.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(KeyError::Empty);
        }
        let digits = text
            .strip_prefix(API_KEY_PREFIX)
            .ok_or(KeyError::MissingPrefix)?;
        if digits.chars().count() != HEX_DIGITS {
            return Err(KeyError::WrongLength {
                found: digits.chars().count(),
            });
        }
        if !digits
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(KeyError::NotHex);
        }
        Ok(Self(text.to_owned()))
    }

    /// The full key, for the `Authorization` header and the key file only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The prefix and last four digits, safe to show on screen.
    #[must_use]
    pub fn redacted(&self) -> String {
        format!("{API_KEY_PREFIX}…{}", &self.0[self.0.len() - 4..])
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.redacted())
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.redacted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "ndx_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn accepts_a_full_key_and_trims_the_paste() {
        let key = ApiKey::parse(&format!("  {VALID}\n")).expect("valid key");
        assert_eq!(key.expose(), VALID);
    }

    #[test]
    fn names_each_paste_mistake() {
        assert_eq!(ApiKey::parse(" "), Err(KeyError::Empty));
        assert_eq!(ApiKey::parse("sk_abc"), Err(KeyError::MissingPrefix));
        assert_eq!(
            ApiKey::parse(&VALID[..40]),
            Err(KeyError::WrongLength { found: 36 })
        );
        assert_eq!(
            ApiKey::parse(&VALID.replace('a', "A")),
            Err(KeyError::NotHex)
        );
        assert_eq!(
            ApiKey::parse(&format!("\"{}", &VALID[..VALID.len() - 1])),
            Err(KeyError::MissingPrefix)
        );
    }

    #[test]
    fn never_prints_the_secret() {
        let key = ApiKey::parse(VALID).expect("valid key");
        for shown in [format!("{key}"), format!("{key:?}"), key.redacted()] {
            assert_eq!(shown, "ndx_…cdef");
        }
    }
}
