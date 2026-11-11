//! Bounded byte-range requests and replies for one indexed declaration.
use serde::{Deserialize, Serialize};

/// A position relative to the declaration owner extent at the requested view.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRangeRequest {
    /// Inclusive byte offset within the owner extent.
    pub start: u32,
    /// Maximum output bytes, at least four and at most 4096.
    pub limit: u16,
    /// Exact source content identity required for a continuation.
    pub source_identity: Option<[u8; 32]>,
}
impl SourceRangeRequest {
    /// Whether the range is bounded and a continuation binds source identity.
    #[must_use]
    pub const fn valid(self) -> bool {
        self.limit >= 4 && self.limit <= 4096 && (self.start == 0 || self.source_identity.is_some())
    }
}

/// Scope proven by the owner for a source byte range.
///
/// End-of-range only completes the indicated extent. It does not establish
/// that a declaration, its merged parts, or the containing file is complete.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRangeScope {
    /// The owner did not provide a scope proof, including on legacy replies.
    #[default]
    Unknown,
    /// The native producer's captured primary declaration span only.
    NativePrimaryDeclaration,
    /// A unique exact structural declaration capture, checked against current
    /// indexed host bytes; this grants no compiler semantic authority.
    StructuralDeclaration,
}

impl SourceRangeScope {
    /// Plain-language description for human-readable source page output.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Unknown => {
                "Scope unknown; page completion does not establish a complete declaration or file."
            }
            Self::NativePrimaryDeclaration => {
                "Scope: native primary declaration span; modifiers or merged parts may be outside this span."
            }
            Self::StructuralDeclaration => {
                "Scope: structural declaration source; compiler semantic authority is not established."
            }
        }
    }
}

/// Exact UTF-8 bytes for one bounded range; line endings are preserved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRangePage {
    /// Captured-policy claim, omitted for historical or unqualified results.
    #[serde(
        default,
        skip_serializing_if = "crate::SourcePolicyQualification::is_unknown"
    )]
    pub source_policy: crate::SourcePolicyQualification,
    /// Exact page text, including original line endings.
    pub text: String,
    /// Inclusive region-relative byte offset.
    pub start: u32,
    /// Exclusive region-relative byte offset.
    pub end: u32,
    /// Total bytes in the scoped extent, not necessarily the whole declaration.
    pub total: u32,
    /// Owner-proven extent scope; legacy replies without this field are unknown.
    #[serde(default)]
    pub scope: SourceRangeScope,
    /// Exact indexed host source identity.
    pub source_identity: [u8; 32],
    /// Inclusive host byte offset of the owner extent.
    pub owner_start: u32,
    /// Exclusive host byte offset of the owner extent.
    pub owner_end: u32,
}
impl SourceRangePage {
    /// Verifies bounds, progress and request correlation at the wire boundary.
    #[must_use]
    pub fn admits(&self, request: SourceRangeRequest) -> bool {
        self.source_policy.is_unknown() && self.matches_request_shape(request)
    }

    /// Checks only byte-range geometry and request correlation. This does not
    /// authenticate the page, its source identity, or its policy claim; callers
    /// must first admit the reply through its authenticated owner channel.
    #[must_use]
    pub fn matches_request_shape(&self, request: SourceRangeRequest) -> bool {
        request.valid()
            && self.start == request.start
            && self.start <= self.end
            && self.end <= self.total
            && self.owner_end.checked_sub(self.owner_start) == Some(self.total)
            && (self.scope == SourceRangeScope::Unknown || self.total != 0)
            && usize::try_from(self.end - self.start).ok() == Some(self.text.len())
            && self.text.len() <= usize::from(request.limit)
            && (self.end == self.total || self.end > self.start)
            && request
                .source_identity
                .is_none_or(|identity| identity == self.source_identity)
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceRangePage, SourceRangeRequest, SourceRangeScope};

    #[test]
    fn legacy_range_scope_is_unknown_and_new_scope_is_explicit() {
        let page = SourceRangePage {
            source_policy: crate::SourcePolicyQualification::Unknown,
            text: "part".to_owned(),
            start: 0,
            end: 4,
            total: 4,
            scope: SourceRangeScope::NativePrimaryDeclaration,
            source_identity: [7; 32],
            owner_start: 9,
            owner_end: 13,
        };
        let mut value = serde_json::to_value(&page).expect("encode native scope");
        assert_eq!(value["scope"], "native_primary_declaration");
        value.as_object_mut().expect("object").remove("scope");
        let legacy: SourceRangePage = serde_json::from_value(value).expect("decode legacy page");
        assert_eq!(legacy.scope, SourceRangeScope::Unknown);
        assert_eq!(
            serde_json::to_value(&legacy).expect("encode unknown")["scope"],
            "unknown"
        );
        let mut invalid = serde_json::to_value(&legacy).expect("object");
        invalid["scope"] = serde_json::json!("complete_declaration");
        assert!(serde_json::from_value::<SourceRangePage>(invalid).is_err());
    }

    #[test]
    fn reply_admission_rejects_wrong_offset_identity_and_nonprogress() {
        let request = SourceRangeRequest {
            start: 4,
            limit: 8,
            source_identity: Some([9; 32]),
        };
        let page = SourceRangePage {
            source_policy: crate::SourcePolicyQualification::Unknown,
            text: "é漢".to_owned(),
            start: 4,
            end: 9,
            total: 20,
            scope: SourceRangeScope::Unknown,
            source_identity: [9; 32],
            owner_start: 11,
            owner_end: 31,
        };
        assert!(page.admits(request));
        let mut wrong = page.clone();
        wrong.start = 5;
        assert!(!wrong.admits(request));
        let mut wrong = page.clone();
        wrong.source_identity = [8; 32];
        assert!(!wrong.admits(request));
        let mut wrong = page.clone();
        wrong.text.clear();
        wrong.end = wrong.start;
        assert!(!wrong.admits(request));
        assert!(
            !SourceRangeRequest {
                source_identity: None,
                ..request
            }
            .valid()
        );
    }
}
