//! Validated newtypes and result enumerations for the Dolt version-control
//! layer of the `rusqdoltlite` engine binding.
//!
//! This module intentionally contains **no** trait definitions or engine
//! implementations — those live in sibling modules. Everything here is pure
//! data: strongly typed wrappers that make illegal states unrepresentable at
//! the Rust type boundary.

use std::fmt;

use crate::error::EngineError;

// ──────────────────────────────────────────────────────────────────────────────
// CommitHash
// ──────────────────────────────────────────────────────────────────────────────

/// A validated Dolt commit hash.
///
/// DoltLite returns two kinds of hash-like strings:
///
/// * **Content-addressed commits** — 32 lowercase alphanumeric characters.
/// * **Fast-forward / merge commits** — 40 lowercase hex characters (matching
///   the underlying content-addressed storage format).
///
/// This newtype accepts any non-empty ASCII-alphanumeric string between 20 and
/// 64 characters long, covering both families without needing to hard-code a
/// single canonical length. The inner string is stored exactly as provided;
/// callers that need canonical casing must normalise before calling [`parse`].
///
/// [`parse`]: CommitHash::parse
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitHash(String);

impl CommitHash {
    /// Minimum accepted hash length (inclusive).
    const MINIMUM_LENGTH: usize = 20;

    /// Maximum accepted hash length (inclusive).
    const MAXIMUM_LENGTH: usize = 64;

    /// Validate `value` and wrap it in a [`CommitHash`].
    ///
    /// Accepts any non-empty string of length between
    /// [`MINIMUM_LENGTH`][Self::MINIMUM_LENGTH] and
    /// [`MAXIMUM_LENGTH`][Self::MAXIMUM_LENGTH] whose characters are all ASCII
    /// alphanumeric (`0-9`, `a-z`, `A-Z`).
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidCommitHash`] when any constraint is
    /// violated. The `reason` field of the error names the failing rule.
    pub fn parse(value: &str) -> Result<Self, EngineError> {
        if value.is_empty() {
            return Err(EngineError::InvalidCommitHash {
                value: value.to_string(),
                reason: "commit hash must not be empty".to_string(),
            });
        }

        if value.len() < Self::MINIMUM_LENGTH {
            return Err(EngineError::InvalidCommitHash {
                value: value.to_string(),
                reason: format!(
                    "commit hash is too short ({} chars); minimum is {}",
                    value.len(),
                    Self::MINIMUM_LENGTH,
                ),
            });
        }

        if value.len() > Self::MAXIMUM_LENGTH {
            return Err(EngineError::InvalidCommitHash {
                value: value.to_string(),
                reason: format!(
                    "commit hash is too long ({} chars); maximum is {}",
                    value.len(),
                    Self::MAXIMUM_LENGTH,
                ),
            });
        }

        if let Some(bad_char) = value
            .chars()
            .find(|character| !character.is_ascii_alphanumeric())
        {
            return Err(EngineError::InvalidCommitHash {
                value: value.to_string(),
                reason: format!(
                    "commit hash contains disallowed character {:?}; only ASCII alphanumeric characters are accepted",
                    bad_char,
                ),
            });
        }

        Ok(Self(value.to_string()))
    }

    /// Borrow the raw hash string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the newtype and return the inner [`String`].
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CommitHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// BranchName
// ──────────────────────────────────────────────────────────────────────────────

/// A validated Dolt branch name.
///
/// Branch names are passed verbatim to Dolt SQL functions (e.g.
/// `dolt_checkout`, `dolt_merge`). Accepting an arbitrary string would allow
/// SQL injection through specially crafted names, and would cause confusing
/// engine errors for names that are structurally impossible. This newtype
/// enforces the same rules as Git's `check-ref-format --branch`, plus a few
/// Dolt-specific additions.
///
/// **Accepted characters:** ASCII letters, digits, `/`, `_`, `-`, `.`
/// (dash is allowed in non-leading positions).
///
/// **Rejected patterns** (each produces a distinct `reason` string):
/// - Empty string.
/// - Length > 255.
/// - Contains whitespace or ASCII control characters.
/// - Contains NUL (`\0`).
/// - Contains `~ ^ : ? * [ \`.
/// - Contains the substring `..`.
/// - Starts or ends with `/`.
/// - Starts with `-` (would be interpreted as a flag by the engine).
/// - Starts or ends with `.`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BranchName(String);

impl BranchName {
    /// Maximum accepted branch-name byte length (inclusive).
    const MAXIMUM_LENGTH: usize = 255;

    /// Characters that are structurally disallowed in a branch name regardless
    /// of position.
    const DISALLOWED_CHARACTERS: &'static [char] = &['~', '^', ':', '?', '*', '[', '\\'];

    /// Validate `value` and wrap it in a [`BranchName`].
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidBranchName`] when any constraint is
    /// violated. The `reason` field names the specific failing rule.
    pub fn parse(value: &str) -> Result<Self, EngineError> {
        macro_rules! reject {
            ($reason:expr) => {
                return Err(EngineError::InvalidBranchName {
                    name: value.to_string(),
                    reason: $reason.to_string(),
                })
            };
        }

        if value.is_empty() {
            reject!("branch name must not be empty");
        }

        if value.len() > Self::MAXIMUM_LENGTH {
            reject!(format!(
                "branch name is too long ({} chars); maximum is {}",
                value.len(),
                Self::MAXIMUM_LENGTH,
            ));
        }

        // Check for whitespace and control characters (includes NUL).
        if let Some(bad_char) = value
            .chars()
            .find(|character| character.is_whitespace() || character.is_control())
        {
            reject!(format!(
                "branch name contains disallowed character {:?}; whitespace and control characters are not permitted",
                bad_char,
            ));
        }

        // Check for structurally disallowed characters.
        if let Some(bad_char) = value
            .chars()
            .find(|character| Self::DISALLOWED_CHARACTERS.contains(character))
        {
            reject!(format!(
                "branch name contains disallowed character {:?}",
                bad_char,
            ));
        }

        // Reject the ".." substring (Git's ambiguous range operator).
        if value.contains("..") {
            reject!("branch name must not contain the substring \"..\"");
        }

        // Slash boundary rules.
        if value.starts_with('/') {
            reject!("branch name must not start with '/'");
        }
        if value.ends_with('/') {
            reject!("branch name must not end with '/'");
        }

        // Leading dash would be interpreted as a command-line flag by Dolt.
        if value.starts_with('-') {
            reject!(
                "branch name must not start with '-'; it would be interpreted as a flag by the engine"
            );
        }

        // Dot boundary rules (Git convention).
        if value.starts_with('.') {
            reject!("branch name must not start with '.'");
        }
        if value.ends_with('.') {
            reject!("branch name must not end with '.'");
        }

        Ok(Self(value.to_string()))
    }

    /// Borrow the raw branch name string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// MergeOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// The typed result of a Dolt merge operation.
///
/// Callers should pattern-match on this enum rather than inspecting raw engine
/// text so that new engine behaviours can be handled in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The target branch already contained every commit from the source; no
    /// work was performed. Corresponds to the engine message "Already up to
    /// date".
    AlreadyUpToDate,

    /// The branch pointer was advanced to the tip of the source branch without
    /// creating a merge commit. Only possible when the source is a direct
    /// ancestor of the target.
    FastForward {
        /// The commit hash that the branch head now points to.
        new_head: CommitHash,
    },

    /// A three-way merge succeeded and produced a new merge commit.
    MergeCommit {
        /// The hash of the newly created merge commit.
        new_head: CommitHash,
    },

    /// The merge was halted because one or more tables contain row-level
    /// conflicts that require manual resolution.
    Conflicts {
        /// Names of the tables that contain conflicts. May be empty when the
        /// engine could not retrieve the conflict table list (e.g. during
        /// error recovery), but the variant still signals that conflicts exist.
        conflicted_tables: Vec<String>,
    },
}

impl MergeOutcome {
    /// Returns `true` when the merge completed without row-level conflicts.
    ///
    /// Both [`AlreadyUpToDate`][Self::AlreadyUpToDate],
    /// [`FastForward`][Self::FastForward], and
    /// [`MergeCommit`][Self::MergeCommit] are considered clean outcomes.
    /// Only [`Conflicts`][Self::Conflicts] returns `false`.
    pub fn is_clean(&self) -> bool {
        !matches!(self, Self::Conflicts { .. })
    }

    /// Returns a reference to the new branch head commit hash, if one was
    /// produced by the merge.
    ///
    /// Returns `Some` for [`FastForward`][Self::FastForward] and
    /// [`MergeCommit`][Self::MergeCommit], and `None` for
    /// [`AlreadyUpToDate`][Self::AlreadyUpToDate] and
    /// [`Conflicts`][Self::Conflicts].
    pub fn new_head(&self) -> Option<&CommitHash> {
        match self {
            Self::FastForward { new_head } | Self::MergeCommit { new_head } => Some(new_head),
            Self::AlreadyUpToDate | Self::Conflicts { .. } => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CommitHash ────────────────────────────────────────────────────────────

    #[test]
    fn commit_hash_accepts_32_char_lowercase_hex() {
        let hash_string = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";
        let hash = CommitHash::parse(hash_string).expect("valid 32-char hash");
        assert_eq!(hash.as_str(), hash_string);
    }

    #[test]
    fn commit_hash_accepts_40_char_lowercase_hex() {
        let hash_string = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let hash = CommitHash::parse(hash_string).expect("valid 40-char hash");
        assert_eq!(hash.as_str(), hash_string);
    }

    #[test]
    fn commit_hash_accepts_mixed_case_alphanumeric() {
        // permissive: uppercase is fine
        let hash_string = "ABCDEF1234567890ABCDEF1234567890";
        CommitHash::parse(hash_string).expect("mixed-case alphanumeric hash");
    }

    #[test]
    fn commit_hash_rejects_empty_string() {
        let result = CommitHash::parse("");
        assert!(result.is_err(), "empty string must be rejected");
    }

    #[test]
    fn commit_hash_rejects_too_short() {
        // 10 characters — below the 20-char minimum
        let result = CommitHash::parse("a1b2c3d4e5");
        assert!(
            result.is_err(),
            "10-char hash must be rejected as too short"
        );
    }

    #[test]
    fn commit_hash_rejects_uppercase_with_symbol() {
        // Contains '!' which is not ASCII alphanumeric
        let result = CommitHash::parse("ABC!DEF1234567890ABCDEF1234567890");
        assert!(result.is_err(), "hash containing '!' must be rejected");
    }

    #[test]
    fn commit_hash_rejects_too_long() {
        // 65 characters — one beyond the 64-char maximum
        let long_string = "a".repeat(65);
        let result = CommitHash::parse(&long_string);
        assert!(result.is_err(), "65-char hash must be rejected as too long");
    }

    #[test]
    fn commit_hash_display_round_trips() {
        let hash_string = "1234567890abcdef1234567890abcdef";
        let hash = CommitHash::parse(hash_string).unwrap();
        assert_eq!(hash.to_string(), hash_string);
    }

    #[test]
    fn commit_hash_into_string_consumes_value() {
        let hash_string = "1234567890abcdef1234567890abcdef";
        let hash = CommitHash::parse(hash_string).unwrap();
        assert_eq!(hash.into_string(), hash_string);
    }

    // ── BranchName ────────────────────────────────────────────────────────────

    #[test]
    fn branch_name_accepts_main() {
        BranchName::parse("main").expect("\"main\" is a valid branch name");
    }

    #[test]
    fn branch_name_accepts_feature_slash_x() {
        BranchName::parse("feature/x").expect("\"feature/x\" is a valid branch name");
    }

    #[test]
    fn branch_name_accepts_local_slash_dev_dash_1() {
        BranchName::parse("local/dev-1").expect("\"local/dev-1\" is a valid branch name");
    }

    #[test]
    fn branch_name_rejects_leading_dash() {
        let result = BranchName::parse("-x");
        assert!(result.is_err(), "leading dash must be rejected");
    }

    #[test]
    fn branch_name_rejects_double_dot() {
        let result = BranchName::parse("a..b");
        assert!(result.is_err(), "double-dot substring must be rejected");
    }

    #[test]
    fn branch_name_rejects_leading_slash() {
        let result = BranchName::parse("/x");
        assert!(result.is_err(), "leading slash must be rejected");
    }

    #[test]
    fn branch_name_rejects_trailing_slash() {
        let result = BranchName::parse("x/");
        assert!(result.is_err(), "trailing slash must be rejected");
    }

    #[test]
    fn branch_name_rejects_embedded_space() {
        let result = BranchName::parse("has space");
        assert!(result.is_err(), "space must be rejected");
    }

    #[test]
    fn branch_name_rejects_caret() {
        let result = BranchName::parse("ha^t");
        assert!(result.is_err(), "caret must be rejected");
    }

    #[test]
    fn branch_name_rejects_empty_string() {
        let result = BranchName::parse("");
        assert!(result.is_err(), "empty string must be rejected");
    }

    // ── MergeOutcome ─────────────────────────────────────────────────────────

    #[test]
    fn merge_outcome_already_up_to_date_is_clean() {
        assert!(MergeOutcome::AlreadyUpToDate.is_clean());
        assert!(MergeOutcome::AlreadyUpToDate.new_head().is_none());
    }

    #[test]
    fn merge_outcome_fast_forward_is_clean_and_has_head() {
        let head = CommitHash::parse("cafebabe00000000cafebabe00000000").unwrap();
        let outcome = MergeOutcome::FastForward {
            new_head: head.clone(),
        };
        assert!(outcome.is_clean());
        assert_eq!(outcome.new_head(), Some(&head));
    }

    #[test]
    fn merge_outcome_merge_commit_is_clean_and_has_head() {
        let head = CommitHash::parse("deadbeefdeadbeefdeadbeefdeadbeef").unwrap();
        let outcome = MergeOutcome::MergeCommit {
            new_head: head.clone(),
        };
        assert!(outcome.is_clean());
        assert_eq!(outcome.new_head(), Some(&head));
    }

    #[test]
    fn merge_outcome_conflicts_is_not_clean() {
        let outcome = MergeOutcome::Conflicts {
            conflicted_tables: vec!["packages".to_string()],
        };
        assert!(!outcome.is_clean());
        assert!(outcome.new_head().is_none());
    }

    #[test]
    fn merge_outcome_conflicts_may_have_empty_table_list() {
        let outcome = MergeOutcome::Conflicts {
            conflicted_tables: vec![],
        };
        assert!(!outcome.is_clean());
    }
}
