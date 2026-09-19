//! Production panic and placeholder construct checks.

use super::{RustToken, in_ranges};
use crate::{SourceFile, Violation};

pub(super) fn forbidden_constructs(
    file: &SourceFile,
    tokens: &[RustToken],
    test_ranges: &[(usize, usize)],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if in_ranges(token.start, test_ranges) {
            continue;
        }
        let is_macro = |name: &str| {
            token.text == name && tokens.get(index + 1).is_some_and(|next| next.text == "!")
        };
        let is_method = |name: &str| {
            token.text == name
                && method_call_has_open_paren(tokens, index)
                && index
                    .checked_sub(1)
                    .and_then(|previous| tokens.get(previous))
                    .is_some_and(|previous| matches!(previous.text.as_str(), "." | "::"))
        };
        let construct = if is_macro("todo") {
            Some("todo!")
        } else if is_macro("unimplemented") {
            Some("unimplemented!")
        } else if is_macro("panic") {
            Some("panic!")
        } else if is_macro("unreachable") {
            Some("unreachable!")
        } else if ["unwrap", "unwrap_err", "unwrap_unchecked"]
            .iter()
            .any(|name| is_method(name))
        {
            Some("unwrap()")
        } else if ["expect", "expect_err"].iter().any(|name| is_method(name)) {
            Some("expect()")
        } else {
            None
        };
        if let Some(construct) = construct {
            violations.push(Violation::ForbiddenProductionConstruct {
                path: file.path.clone(),
                line: token.line,
                construct: construct.to_owned(),
            });
        }
    }
    violations
}

fn method_call_has_open_paren(tokens: &[RustToken], index: usize) -> bool {
    let mut cursor = index + 1;
    // Rust's turbofish spelling (`method::<T>()`) is tokenized as a separate
    // `::` followed by the generic argument list.
    if tokens.get(cursor).is_some_and(|token| token.text == "::") {
        cursor += 1;
    }
    match tokens.get(cursor).map(RustToken::as_str) {
        Some("(") => true,
        Some("<") => {
            let mut depth = 0usize;
            for (position, token) in tokens.iter().enumerate().skip(cursor) {
                match token.text.as_str() {
                    "<" => depth += 1,
                    ">" => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return tokens
                                .get(position + 1)
                                .is_some_and(|open| open.text == "(");
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        _ => false,
    }
}
