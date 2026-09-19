//! Canonical ownership and authority boundary checks.

use super::{RustToken, has_extension, in_ranges, is_test_path, test_only_ranges, tokenize_rust};
use crate::{SourceFile, Violation};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

/// Applies the narrow architecture guards that cannot be expressed by Cargo
/// or rustc alone.  The ownership table is deliberately data-driven: adding
/// a new canonical grammar requires naming its owner and its exact marker in
/// the checked-in JSON policy.
#[must_use]
pub fn validate_architecture_guards(
    files: &[SourceFile],
    policy_source: &SourceFile,
) -> Vec<Violation> {
    let policy: CanonicalOwnershipPolicy = match serde_json::from_str(&policy_source.contents) {
        Ok(policy) => policy,
        Err(error) => {
            return vec![Violation::InvalidCanonicalOwnershipPolicy {
                path: policy_source.path.clone(),
                detail: error.to_string(),
            }];
        }
    };
    if let Err(detail) = policy.validate() {
        return vec![Violation::InvalidCanonicalOwnershipPolicy {
            path: policy_source.path.clone(),
            detail,
        }];
    }

    let known_paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(rule) = policy
        .rules
        .iter()
        .find(|rule| !known_paths.contains(rule.owner.as_str()))
    {
        return vec![Violation::InvalidCanonicalOwnershipPolicy {
            path: policy_source.path.clone(),
            detail: format!(
                "owner `{}` is not an inventoried Rust source file",
                rule.owner
            ),
        }];
    }

    let mut violations = Vec::new();
    for file in files {
        if is_test_path(&file.path) || !has_extension(&file.path, "rs") {
            continue;
        }
        let tokens = tokenize_rust(&file.contents);
        let test_ranges = test_only_ranges(&tokens);
        for (index, token) in tokens.iter().enumerate() {
            if in_ranges(token.start, &test_ranges) {
                continue;
            }
            if token.text == "from_authenticated_digest" {
                violations.push(Violation::RemovedIdentityConstructor {
                    path: file.path.clone(),
                    line: token.line,
                    symbol: token.text.clone(),
                });
            }
            if token.text == "publish_inner" && call_starts_with_none(&tokens, index) {
                violations.push(Violation::AuthoritylessStorePublication {
                    path: file.path.clone(),
                    line: token.line,
                    call: "publish_inner(None)".to_owned(),
                });
            }
        }
        for rule in &policy.rules {
            if let Some(literal) = rule.literal.as_deref() {
                for (index, token) in tokens.iter().enumerate() {
                    if in_ranges(token.start, &test_ranges)
                        || !byte_literal_matches(&tokens, index, literal)
                        || file.path == rule.owner
                    {
                        continue;
                    }
                    violations.push(canonical_violation(file, token, rule, literal));
                }
            }
            if let Some(encoder) = rule.encoder.as_deref() {
                for (index, token) in tokens.iter().enumerate() {
                    if in_ranges(token.start, &test_ranges)
                        || !definition_matches(&tokens, index, encoder)
                        || file.path == rule.owner
                    {
                        continue;
                    }
                    violations.push(canonical_violation(file, token, rule, encoder));
                }
            }
        }
    }
    violations
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalOwnershipPolicy {
    schema_version: u8,
    rules: Vec<CanonicalOwnershipRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalOwnershipRule {
    domain: String,
    owner: String,
    #[serde(default)]
    literal: Option<String>,
    #[serde(default)]
    encoder: Option<String>,
}

impl CanonicalOwnershipPolicy {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("schema_version must be 1".to_owned());
        }
        let required = [
            "version-node-object-identity",
            "product-closure-manifest-row-object",
            "local-ldc2",
            "replication-frame",
            "control-work-spec-wire",
            "control-record-wire",
            "dispatch-authority-statement",
            "store-manifest-descriptor-node-envelope",
            "flow-checkpoint-manifest",
        ];
        let domains = self
            .rules
            .iter()
            .map(|rule| rule.domain.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(domain) = required.iter().find(|domain| !domains.contains(**domain)) {
            return Err(format!("missing required domain `{domain}`"));
        }
        if self.rules.is_empty() {
            return Err("rules must not be empty".to_owned());
        }
        let mut markers = BTreeSet::new();
        for rule in &self.rules {
            if rule.domain.trim().is_empty() {
                return Err("rule domain must not be empty".to_owned());
            }
            if rule.owner.trim().is_empty() || !has_extension(&rule.owner, "rs") {
                return Err(format!("rule owner must be a Rust file: `{}`", rule.owner));
            }
            if rule.literal.is_some() == rule.encoder.is_some() {
                return Err(format!(
                    "rule `{}` must contain exactly one literal or encoder",
                    rule.domain
                ));
            }
            if rule
                .literal
                .as_deref()
                .or(rule.encoder.as_deref())
                .is_none_or(str::is_empty)
            {
                return Err(format!("rule `{}` marker must not be empty", rule.domain));
            }
            let Some(marker) = rule.literal.as_deref().or(rule.encoder.as_deref()) else {
                return Err(format!("rule `{}` marker must not be empty", rule.domain));
            };
            if !markers.insert((rule.domain.as_str(), marker)) {
                return Err(format!(
                    "canonical marker `{marker}` appears more than once in domain `{}`",
                    rule.domain
                ));
            }
            let normalized_owner = rule.owner.replace('\\', "/");
            let owner = Path::new(&normalized_owner);
            if owner.is_absolute()
                || owner
                    .components()
                    .any(|component| component.as_os_str() == "..")
            {
                return Err(format!(
                    "rule owner must be repository-relative: `{}`",
                    rule.owner
                ));
            }
        }
        Ok(())
    }
}

fn canonical_violation(
    file: &SourceFile,
    token: &RustToken,
    rule: &CanonicalOwnershipRule,
    marker: &str,
) -> Violation {
    Violation::CanonicalGrammarOutsideOwner {
        domain: rule.domain.clone(),
        path: file.path.clone(),
        line: token.line,
        owner: rule.owner.clone(),
        marker: marker.to_owned(),
    }
}

fn byte_literal_matches(tokens: &[RustToken], index: usize, literal: &str) -> bool {
    tokens.get(index).is_some_and(|token| token.text == "b")
        && tokens
            .get(index + 1)
            .is_some_and(|token| token.literal && token.text == format!("\"{literal}\""))
}

fn definition_matches(tokens: &[RustToken], index: usize, name: &str) -> bool {
    tokens.get(index).is_some_and(|token| {
        matches!(
            token.text.as_str(),
            "fn" | "struct" | "enum" | "trait" | "type" | "const" | "static"
        )
    }) && tokens
        .get(index + 1)
        .is_some_and(|token| token.text == name)
}

fn call_starts_with_none(tokens: &[RustToken], index: usize) -> bool {
    let mut cursor = index + 1;
    if tokens.get(cursor).is_some_and(|token| token.text == "::") {
        let mut depth = 0usize;
        while let Some(token) = tokens.get(cursor) {
            match token.text.as_str() {
                "<" => depth += 1,
                ">" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        cursor += 1;
                        break;
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
    }
    tokens.get(cursor).is_some_and(|token| token.text == "(")
        && tokens
            .get(cursor + 1)
            .is_some_and(|token| token.text == "None")
}
