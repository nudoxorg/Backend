//! Bounded, sanitized host-side cause text for TypeScript project failures.
//!
//! The closed [`TypeScriptAuthorityFailureKind`] says which stage refused; this
//! text says *why* in words a project owner can act on (for example which
//! `tsconfig*.json` was ambiguous or malformed). It is produced only from
//! host-controlled messages after project paths were made project-relative and
//! every other absolute path was masked; native process output never reaches it.
use super::TypeScriptAuthorityFailureKind;
use serde::{Deserialize, Deserializer, Serialize};

/// Validated one-line cause text for a TypeScript project host failure.
///
/// The text is non-empty, at most [`Self::MAX_BYTES`] bytes, free of control
/// characters, quotes and backslashes, and trimmed. Admission through
/// [`Self::new`] is the only constructor, including during deserialization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TypeScriptFailureCause(Box<str>);

impl TypeScriptFailureCause {
    /// Maximum UTF-8 bytes retained, so the fixed kind sentence plus this
    /// cause always fits `MAX_COMPILER_FRAGMENT_DETAIL_BYTES`.
    pub const MAX_BYTES: usize = 240;

    /// Admits a candidate exactly; returns `None` for empty, oversized,
    /// untrimmed, control-bearing, quoted or backslash-bearing text.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        let admitted = !text.is_empty()
            && text.len() <= Self::MAX_BYTES
            && text.trim() == text
            && !text
                .chars()
                .any(|character| character.is_control() || matches!(character, '"' | '\\'));
        admitted.then(|| Self(text.into()))
    }

    /// Normalizes arbitrary host text into an admitted cause: control characters,
    /// quotes and backslashes become spaces, runs of whitespace collapse, and the
    /// result is truncated on a character boundary with a trailing `...`.
    /// Returns `None` when nothing printable remains.
    #[must_use]
    pub fn sanitize(text: &str) -> Option<Self> {
        let mut retained = String::with_capacity(Self::MAX_BYTES.min(text.len()));
        let mut pending_space = false;
        let mut truncated = false;
        for character in text.chars() {
            let character = if character.is_control()
                || character.is_whitespace()
                || matches!(character, '"' | '\\')
            {
                ' '
            } else {
                character
            };
            if character == ' ' {
                pending_space = !retained.is_empty();
                continue;
            }
            let needed = character.len_utf8() + usize::from(pending_space);
            if retained.len() + needed > Self::MAX_BYTES - 3 {
                truncated = true;
                break;
            }
            if pending_space {
                retained.push(' ');
                pending_space = false;
            }
            retained.push(character);
        }
        if retained.is_empty() {
            return None;
        }
        if truncated {
            retained.push_str("...");
        }
        Self::new(&retained)
    }

    /// Admitted cause text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Public detail for `kind`: its fixed sentence followed by this cause.
    #[must_use]
    pub fn detail_for(&self, kind: TypeScriptAuthorityFailureKind) -> String {
        format!("{}: {}", kind.detail(), self.0)
    }
}

impl<'de> Deserialize<'de> for TypeScriptFailureCause {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::new(&text)
            .ok_or_else(|| serde::de::Error::custom("invalid bounded TypeScript failure cause"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_bounds_collapses_and_rejects_empty() {
        let cause = TypeScriptFailureCause::sanitize("  two\t\nconfigs \"a\"\\b  ").unwrap();
        assert_eq!(cause.as_str(), "two configs a b");
        assert!(TypeScriptFailureCause::sanitize(" \n\t ").is_none());
        let long = "x".repeat(1000);
        let cause = TypeScriptFailureCause::sanitize(&long).unwrap();
        assert_eq!(cause.as_str().len(), TypeScriptFailureCause::MAX_BYTES);
        assert!(cause.as_str().ends_with("..."));
        let wide = "é".repeat(1000);
        let cause = TypeScriptFailureCause::sanitize(&wide).unwrap();
        assert!(cause.as_str().len() <= TypeScriptFailureCause::MAX_BYTES);
        assert!(cause.as_str().is_char_boundary(cause.as_str().len()));
    }

    #[test]
    fn deserialization_revalidates() {
        assert!(serde_json::from_str::<TypeScriptFailureCause>("\"ok text\"").is_ok());
        assert!(serde_json::from_str::<TypeScriptFailureCause>("\"\"").is_err());
        assert!(serde_json::from_str::<TypeScriptFailureCause>("\" lead\"").is_err());
        assert!(serde_json::from_str::<TypeScriptFailureCause>("\"a\\nb\"").is_err());
        let oversized = format!("\"{}\"", "y".repeat(TypeScriptFailureCause::MAX_BYTES + 1));
        assert!(serde_json::from_str::<TypeScriptFailureCause>(&oversized).is_err());
    }

    #[test]
    fn longest_known_fixed_sentences_plus_cause_fit_public_detail_bound() {
        let cause =
            TypeScriptFailureCause::new(&"c".repeat(TypeScriptFailureCause::MAX_BYTES)).unwrap();
        for kind in [
            TypeScriptAuthorityFailureKind::HostNodeUnavailable,
            TypeScriptAuthorityFailureKind::HostCompilerApiBridge,
            TypeScriptAuthorityFailureKind::HostCompilerLinkMismatch,
            TypeScriptAuthorityFailureKind::HostConfigInvalid,
        ] {
            assert!(
                cause.detail_for(kind).len()
                    <= crate::interface::MAX_COMPILER_FRAGMENT_DETAIL_BYTES,
                "{kind:?}"
            );
        }
    }
}
