//! Parser for `MODULE.bazel` manifests (REGISTRYLESS §8).
//!
//! Performs a pure static text scan of Starlark source — no execution, no
//! eval. Recognises two call forms:
//!
//! - `module(name = "<n>", version = "<v>", ...)` — the module's own identity
//!   (no description or license field in `MODULE.bazel`; facets remain
//!   default).
//! - `bazel_dep(name = "<n>", version = "<v>", ...)` →
//!   [`DependencyMechanism::BazelDep`]
//!
//! Starlark uses double-quoted string literals and `#` line comments. The
//! scanner is character-level: no regex, no heap-heavy tokenizer. Calls may
//! span multiple lines; `name = "..."` keyword arguments may appear in any
//! position within the argument list.

use super::{CppManifest, DependencyMechanism, DependencyRecord};

// ── Public entry point
// ────────────────────────────────────────────────────────

/// Parse a `MODULE.bazel` file and return the extracted manifest.
///
/// Scans for `bazel_dep(...)` calls and extracts the `name = "..."` keyword
/// argument value from each as the dependency token. `module(...)` is
/// recognised but contributes no facets. Never panics.
pub fn parse(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();
    let bytes = text.as_bytes();
    let length = bytes.len();
    let mut position = 0usize;

    while position < length {
        // Skip comments.
        if bytes[position] == b'#' {
            position = advance_past_line_end(bytes, position);
            continue;
        }

        // Skip double-quoted strings to prevent matching inside string values.
        if bytes[position] == b'"' {
            position = advance_past_double_quoted_string(bytes, position);
            continue;
        }

        // Try to match a recognised keyword at this position.
        if let Some((keyword, paren_position)) = try_match_call_keyword(bytes, position) {
            match keyword {
                CallKeyword::BazelDep => {
                    if let Some((token, requirement, after_call)) =
                        extract_bazel_dep(bytes, paren_position)
                    {
                        let mut record =
                            DependencyRecord::new(token, DependencyMechanism::BazelDep);
                        record.requirement = requirement;
                        manifest.push_dependency(record);
                        position = after_call;
                    } else {
                        position = paren_position + 1;
                    }
                    continue;
                }
                CallKeyword::Module => {
                    // module() carries no facets we can extract.
                    position = paren_position + 1;
                    continue;
                }
            }
        }

        position += 1;
    }

    manifest
}

// ── Internal types
// ────────────────────────────────────────────────────────────

/// The call keywords the scanner recognises in `MODULE.bazel`.
#[derive(Debug, Clone, Copy)]
enum CallKeyword {
    BazelDep,
    Module,
}

// ── Scanner helpers
// ───────────────────────────────────────────────────────────

/// If the bytes starting at `position` begin one of the recognised call
/// keywords immediately followed by `(`, return the keyword and the index of
/// the `(` character. Returns `None` if no keyword matches.
fn try_match_call_keyword(bytes: &[u8], position: usize) -> Option<(CallKeyword, usize)> {
    const KEYWORDS: &[(&[u8], CallKeyword)] = &[
        (b"bazel_dep", CallKeyword::BazelDep),
        (b"module", CallKeyword::Module),
    ];
    for (keyword_bytes, keyword) in KEYWORDS {
        let end = position + keyword_bytes.len();
        if bytes.get(position..end) == Some(keyword_bytes) {
            let mut scan = end;
            // Allow optional whitespace between the function name and `(`.
            while scan < bytes.len() && bytes[scan] == b' ' {
                scan += 1;
            }
            if bytes.get(scan) == Some(&b'(') {
                return Some((*keyword, scan));
            }
        }
    }
    None
}

/// Advance past a `#` comment to the end of the line. `position` points at
/// the `#` character. Returns the index of the first byte on the next line
/// (or the end of input).
fn advance_past_line_end(bytes: &[u8], position: usize) -> usize {
    let mut index = position;
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    if index < bytes.len() {
        index += 1;
    }
    index
}

/// Advance past a double-quoted Starlark string. `position` points at the
/// opening `"`. Returns the index immediately after the closing `"`. Handles
/// `\"` escape sequences. Never panics on unterminated input.
fn advance_past_double_quoted_string(bytes: &[u8], position: usize) -> usize {
    debug_assert_eq!(bytes.get(position), Some(&b'"'));
    let mut index = position + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 2;
            }
            b'"' => {
                return index + 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    bytes.len()
}

/// Starting from the `(` at `paren_position`, scan the call's argument list
/// for a `name = "..."` keyword argument (in any position, with any spacing
/// around `=`). Returns `(name_value, index_after_closing_paren)` or `None`
/// if no `name` keyword argument is found.
///
/// Tracks nested parentheses and skips string contents so `name` that appears
/// inside a string value is not mistaken for a keyword argument.
fn extract_bazel_dep(
    bytes: &[u8],
    paren_position: usize,
) -> Option<(String, Option<String>, usize)> {
    debug_assert_eq!(bytes.get(paren_position), Some(&b'('));
    let mut depth = 0i32;
    let mut index = paren_position;
    let mut pending: Option<usize> = None;
    let mut name = None;
    let mut version = None;

    while index < bytes.len() {
        match bytes[index] {
            b'#' => {
                pending = None;
                index = advance_past_line_end(bytes, index);
            }
            b'"' => {
                let after = advance_past_double_quoted_string(bytes, index);
                if let Some(keyword) = pending.take() {
                    let keyword_len = if bytes.get(keyword..keyword + 4) == Some(b"name") {
                        4
                    } else {
                        7
                    };
                    if is_keyword_assignment(bytes, keyword + keyword_len, index)
                        && let Ok(value) =
                            std::str::from_utf8(&bytes[index + 1..after.saturating_sub(1)])
                    {
                        if bytes.get(keyword..keyword + 4) == Some(b"name") {
                            name = Some(value.to_owned());
                        } else {
                            version = Some(value.to_owned());
                        }
                    }
                }
                index = after;
            }
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                if depth <= 0 {
                    return name
                        .map(|name| (name, version.filter(|text| !text.is_empty()), index + 1));
                }
                index += 1;
            }
            _ => {
                if depth > 0
                    && let Some(keyword) = keyword_at(bytes, index)
                {
                    pending = Some(keyword);
                }
                index += 1;
            }
        }
    }
    None
}

fn keyword_at(bytes: &[u8], index: usize) -> Option<usize> {
    let word = if bytes.get(index..index + 7) == Some(b"version") {
        7
    } else if bytes.get(index..index + 4) == Some(b"name") {
        4
    } else {
        return None;
    };
    let preceding_ok =
        index == 0 || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_';
    let following_ok = bytes
        .get(index + word)
        .is_none_or(|&byte| !byte.is_ascii_alphanumeric() && byte != b'_');
    if preceding_ok && following_ok {
        Some(index)
    } else {
        None
    }
}

/// Verify that the bytes in `bytes[from..to]` consist only of optional
/// whitespace followed by exactly one `=` followed by optional whitespace —
/// confirming `name <here> "value"` is a keyword assignment.
fn is_keyword_assignment(bytes: &[u8], from: usize, to: usize) -> bool {
    if from > to || to > bytes.len() {
        return false;
    }
    let slice = &bytes[from..to];
    let trimmed = slice
        .iter()
        .position(|&b| b != b' ' && b != b'\t' && b != b'\n' && b != b'\r');
    let Some(eq_pos) = trimmed else { return false };
    if slice[eq_pos] != b'=' {
        return false;
    }
    // Everything after `=` must be whitespace.
    slice[eq_pos + 1..]
        .iter()
        .all(|&b| b == b' ' || b == b'\t' || b == b'\n' || b == b'\r')
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_input_does_not_panic() {
        let manifest = parse("");
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn parse_basic_bazel_dep() {
        let text = r#"
module(name = "my_module", version = "1.0.0")
bazel_dep(name = "rules_cc", version = "0.0.9")
bazel_dep(name = "abseil-cpp", version = "20230802.1")
"#;
        let manifest = parse(text);
        let tokens: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|r| r.token.as_str())
            .collect();
        assert!(tokens.contains(&"rules_cc"));
        assert!(tokens.contains(&"abseil-cpp"));
        assert_eq!(
            manifest.dependencies[0].mechanism,
            DependencyMechanism::BazelDep
        );
        assert_eq!(
            manifest.dependencies[0].requirement.as_deref(),
            Some("0.0.9")
        );
        assert_eq!(
            manifest.facts.dependencies[0].requirement.as_deref(),
            Some("0.0.9")
        );
    }

    #[test]
    fn parse_name_arg_in_any_position() {
        // version comes before name.
        let text = r#"bazel_dep(version = "1.2.3", name = "protobuf")"#;
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "protobuf");
        assert_eq!(
            manifest.dependencies[0].requirement.as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn parse_no_space_around_equals() {
        let text = r#"bazel_dep(name="grpc", version="1.0")"#;
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "grpc");
    }

    #[test]
    fn parse_comment_stripped_correctly() {
        let text = "# bazel_dep(name = \"should_not_appear\")\nbazel_dep(name = \"real_dep\", version = \"1.0\")\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "real_dep");
    }

    #[test]
    fn parse_call_spanning_multiple_lines() {
        let text =
            "bazel_dep(\n    name = \"boringssl\",\n    version = \"0.0.0-20230215-5c22014\",\n)\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "boringssl");
    }

    #[test]
    fn parse_malformed_no_name_arg() {
        let text = "bazel_dep(version = \"1.0\")\n";
        let manifest = parse(text);
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn parse_facts_mirror_synced() {
        let text = "bazel_dep(name = \"zlib\", version = \"1.3\")\n";
        let manifest = parse(text);
        assert!(
            manifest
                .facts
                .dependency_names()
                .contains(&"zlib".to_owned())
        );
    }

    #[test]
    fn parse_crlf_line_endings() {
        let text = "bazel_dep(name = \"sdl\", version = \"2.0\")\r\nbazel_dep(name = \"glfw\", version = \"3.3\")\r\n";
        let manifest = parse(text);
        let tokens: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|r| r.token.as_str())
            .collect();
        assert!(tokens.contains(&"sdl"));
        assert!(tokens.contains(&"glfw"));
    }

    #[test]
    fn parse_real_world_module_bazel_shape() {
        // Shaped after a real MODULE.bazel (protobuf-style).
        let text = r#"
module(
    name = "protobuf",
    version = "27.0",
    compatibility_level = 1,
)

bazel_dep(name = "abseil-cpp", version = "20230802.1")
bazel_dep(name = "rules_cc", version = "0.0.9")
bazel_dep(name = "zlib", version = "1.3.1", repo_name = "zlib_repo")

# A dev-only dependency.
bazel_dep(name = "googletest", version = "1.14.0", dev_dependency = True)
"#;
        let manifest = parse(text);
        let tokens: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|r| r.token.as_str())
            .collect();
        assert!(tokens.contains(&"abseil-cpp"));
        assert!(tokens.contains(&"rules_cc"));
        assert!(tokens.contains(&"zlib"));
        assert!(tokens.contains(&"googletest"));
        assert_eq!(manifest.dependencies.len(), 4);
        // `module()` carries no ExtractedFacts-mappable field (no name/version
        // slot in the shared facts model for cpp — identity/version come from
        // git, not the manifest).
        assert_eq!(manifest.facts.description, None);
    }

    // ── Hostile-input hardening ───────────────────────────────────────────────

    #[test]
    fn parse_unterminated_string_no_panic() {
        let text = r#"bazel_dep(name = "unterminated"#;
        let manifest = parse(text);
        let _ = manifest;
    }

    #[test]
    fn parse_unterminated_call_no_panic() {
        let text = r#"bazel_dep(name = "zlib""#;
        let manifest = parse(text);
        let _ = manifest;
    }

    #[test]
    fn parse_unicode_name_value() {
        let text = "bazel_dep(name = \"日本語モジュール\", version = \"1.0\")\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "日本語モジュール");
    }

    #[test]
    fn parse_deeply_nested_parens_no_panic() {
        let mut text = String::from("bazel_dep(name = \"x\", version = \"1.0\"");
        for _ in 0..5000 {
            text.push('(');
        }
        for _ in 0..5000 {
            text.push(')');
        }
        text.push(')');
        let manifest = parse(&text);
        let _ = manifest;
    }

    #[test]
    fn parse_enormous_number_of_bazel_deps_no_panic() {
        use std::fmt::Write as _;
        let mut text = String::new();
        for i in 0..20_000 {
            let _ = writeln!(text, "bazel_dep(name = \"dep{i}\", version = \"1.0\")");
        }
        let manifest = parse(&text);
        assert_eq!(manifest.dependencies.len(), 20_000);
    }

    #[test]
    fn parse_name_value_containing_escaped_quote_no_panic() {
        let text = r#"bazel_dep(name = "we\"ird", version = "1.0")"#;
        let manifest = parse(text);
        let _ = manifest;
    }
}
