//! Production lint-suppression checks.

use super::{RustToken, attribute_is_test_only, bracket_attribute, in_ranges};
use crate::{SourceFile, Violation};

pub(super) fn broad_allows(
    file: &SourceFile,
    tokens: &[RustToken],
    test_ranges: &[(usize, usize)],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.text != "#" || in_ranges(token.start, test_ranges) {
            continue;
        }
        let Some((attribute_end, attribute_tokens)) = bracket_attribute(tokens, index) else {
            continue;
        };
        let Some(allow_tokens) = allow_attribute_tokens(&attribute_tokens) else {
            continue;
        };
        if attribute_is_test_only(tokens, index) || cfg_attr_is_test_only(&attribute_tokens) {
            continue;
        }
        let lints = allow_lint_names(&allow_tokens);
        let broad = lints
            .iter()
            .filter(|lint| is_broad_allow(lint))
            .cloned()
            .collect::<Vec<_>>();
        if !broad.is_empty() {
            violations.push(Violation::BroadLintAllow {
                path: file.path.clone(),
                line: token.line,
                lints: broad,
            });
        }
        // Keep the parser's end visible to the optimizer/debugger and make it
        // impossible to accidentally treat a nested `#` as a new attribute.
        let _ = attribute_end;
    }
    violations
}

fn is_broad_allow(lint: &str) -> bool {
    matches!(
        lint,
        "unused"
            | "warnings"
            | "dead_code"
            | "unused_imports"
            | "unused_variables"
            | "clippy::all"
            | "clippy::pedantic"
            | "clippy::restriction"
            | "clippy::nursery"
            | "clippy::cargo"
            | "clippy::expect_used"
            | "clippy::unwrap_used"
            | "clippy::panic"
            | "clippy::todo"
            | "clippy::unimplemented"
    )
}

fn allow_lint_names(tokens: &[RustToken]) -> Vec<String> {
    let Some(open) = tokens.iter().position(|token| token.text == "(") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0usize;
    for token in tokens.iter().skip(open + 1) {
        match token.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" if depth == 0 => {
                push_lint_name(&mut names, &current);
                break;
            }
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                push_lint_name(&mut names, &current);
                current.clear();
            }
            _ => current.push(token.text.clone()),
        }
    }
    names
}

fn allow_attribute_tokens(attribute: &[RustToken]) -> Option<Vec<RustToken>> {
    if attribute.first().map(RustToken::as_str) == Some("allow") {
        return Some(attribute.to_vec());
    }
    if attribute.first().map(RustToken::as_str) != Some("cfg_attr") {
        return None;
    }
    let allow = attribute.iter().position(|token| token.text == "allow")?;
    (attribute.get(allow + 1).map(RustToken::as_str) == Some("("))
        .then(|| attribute[allow..].to_vec())
}

fn cfg_attr_is_test_only(attribute: &[RustToken]) -> bool {
    if attribute.first().map(RustToken::as_str) != Some("cfg_attr") {
        return false;
    }
    let Some(open) = attribute.iter().position(|token| token.text == "(") else {
        return false;
    };
    let mut depth = 0usize;
    let mut condition = Vec::new();
    for token in attribute.iter().skip(open + 1) {
        match token.text.as_str() {
            "(" => {
                depth += 1;
                condition.push(token.text.clone());
            }
            ")" if depth > 0 => {
                depth = depth.saturating_sub(1);
                condition.push(token.text.clone());
            }
            ")" | "," if depth == 0 => break,
            _ => condition.push(token.text.clone()),
        }
    }
    condition == ["test"] || condition == ["all", "(", "test", ")"]
}

fn push_lint_name(names: &mut Vec<String>, segment: &[String]) {
    let Some(first) = segment.first() else { return };
    if first == "reason" {
        return;
    }
    let mut name = String::new();
    for part in segment {
        if part == "::" {
            name.push_str("::");
        } else if part
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            name.push_str(part);
        } else {
            return;
        }
    }
    if !name.is_empty() {
        names.push(name);
    }
}
