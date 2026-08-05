//! Parser for `meson.build` manifests (REGISTRYLESS §8).
//!
//! Performs a pure static text scan — no execution, no eval. Recognises three
//! call forms:
//!
//! - `project('<name>', ...)` — establishes the project name (unused for facets
//!   since `meson.build` carries no description field).
//! - `dependency('<name>', ...)` → [`DependencyMechanism::PkgConfig`]
//! - `subproject('<name>', ...)` → [`DependencyMechanism::Wrap`]
//!
//! Meson uses single-quoted string literals and `#` line comments. The scanner
//! is character-level: no regex, no heap-heavy tokenizer. Calls may span
//! multiple lines; nested parentheses are tracked so the scanner stays in
//! sync even when arguments contain grouped expressions.

use super::{CppManifest, DependencyMechanism, DependencyRecord};

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse a `meson.build` file and return the extracted manifest.
///
/// Scans the full text for `dependency(...)` and `subproject(...)` calls,
/// extracting the first single-quoted string argument of each as the
/// dependency token. `project(...)` is recognised but contributes no facets
/// (meson has no description field). Never panics.
pub fn parse(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();
    let bytes = text.as_bytes();
    let length = bytes.len();
    let mut position = 0usize;

    while position < length {
        // Skip comments to end of line.
        if bytes[position] == b'#' {
            position = advance_past_line_end(bytes, position);
            continue;
        }

        // Skip single-quoted strings (so we don't false-match inside them).
        if bytes[position] == b'\'' {
            position = advance_past_single_quoted_string(bytes, position);
            continue;
        }

        // Try to match a keyword at the current position.
        if let Some((keyword, rest_position)) = try_match_call_keyword(bytes, position) {
            match keyword {
                CallKeyword::Dependency => {
                    if let Some((token, after_call)) =
                        extract_first_string_arg(bytes, rest_position)
                    {
                        manifest.push_dependency(DependencyRecord::new(
                            token,
                            DependencyMechanism::PkgConfig,
                        ));
                        position = after_call;
                        continue;
                    }
                    position = rest_position;
                    continue;
                }
                CallKeyword::Subproject => {
                    if let Some((token, after_call)) =
                        extract_first_string_arg(bytes, rest_position)
                    {
                        manifest.push_dependency(DependencyRecord::new(
                            token,
                            DependencyMechanism::Wrap,
                        ));
                        position = after_call;
                        continue;
                    }
                    position = rest_position;
                    continue;
                }
                CallKeyword::Project => {
                    // project() has no description field — consume call silently.
                    position = rest_position;
                    continue;
                }
            }
        }

        position += 1;
    }

    manifest
}

// ── Internal types ────────────────────────────────────────────────────────────

/// The call keywords the scanner recognises in `meson.build`.
#[derive(Debug, Clone, Copy)]
enum CallKeyword {
    Dependency,
    Subproject,
    Project,
}

// ── Scanner helpers ───────────────────────────────────────────────────────────

/// If the bytes starting at `position` begin one of the call keywords
/// immediately followed by `(`, return the keyword and the index of the `(`
/// character. Returns `None` if no keyword matches.
fn try_match_call_keyword(bytes: &[u8], position: usize) -> Option<(CallKeyword, usize)> {
    const KEYWORDS: &[(&[u8], CallKeyword)] = &[
        (b"dependency", CallKeyword::Dependency),
        (b"subproject", CallKeyword::Subproject),
        (b"project", CallKeyword::Project),
    ];
    for (keyword_bytes, keyword) in KEYWORDS {
        let end = position + keyword_bytes.len();
        if bytes.get(position..end) == Some(keyword_bytes) {
            // Must be followed by `(` (whitespace not permitted in Meson between
            // a function name and its argument list — but we allow it defensively).
            let mut scan = end;
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
/// the `#` character. Returns the index of the first byte on the next line (or
/// the end of the input).
fn advance_past_line_end(bytes: &[u8], position: usize) -> usize {
    let mut index = position;
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    // Consume the `\n` itself.
    if index < bytes.len() {
        index += 1;
    }
    index
}

/// Advance past a single-quoted Meson string. `position` points at the opening
/// `'`. Handles triple-quoted `'''...'''` strings. Returns the index
/// immediately after the closing quote sequence. Never panics on unterminated
/// input.
fn advance_past_single_quoted_string(bytes: &[u8], position: usize) -> usize {
    debug_assert!(bytes.get(position) == Some(&b'\''));
    // Detect triple-quote.
    if bytes.get(position..position + 3) == Some(b"'''") {
        let mut index = position + 3;
        while index + 2 < bytes.len() {
            if bytes[index] == b'\'' && bytes[index + 1] == b'\'' && bytes[index + 2] == b'\'' {
                return index + 3;
            }
            index += 1;
        }
        // Unterminated triple-quoted string — consume to end.
        return bytes.len();
    }
    // Single-quoted string.
    let mut index = position + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                // Skip escape sequence.
                index += 2;
            }
            b'\'' => {
                return index + 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    // Unterminated string — consume to end.
    bytes.len()
}

/// Starting from the `(` at `paren_position`, extract the first single-quoted
/// string argument. Returns `(token_string, index_after_call_close_paren)` or
/// `None` if no quoted string is found before the matching `)`.
///
/// Tracks nested parentheses so triple/single-quoted strings that contain `)`
/// do not cause early termination.
fn extract_first_string_arg(bytes: &[u8], paren_position: usize) -> Option<(String, usize)> {
    debug_assert!(bytes.get(paren_position) == Some(&b'('));
    let mut depth = 0i32;
    let mut index = paren_position;

    while index < bytes.len() {
        match bytes[index] {
            b'#' => {
                index = advance_past_line_end(bytes, index);
            }
            b'\'' => {
                // Try to read the quoted string value.
                let string_start = index;
                let after_string = advance_past_single_quoted_string(bytes, string_start);
                // Extract string content when inside the call's argument list
                // (depth > 0 after we've seen the opening `(`).
                if depth > 0 {
                    // Detect triple-quote.
                    let is_triple = bytes.get(string_start..string_start + 3) == Some(b"'''");
                    let (content_start, content_end) = if is_triple {
                        // Content is between the triple-quote delimiters.
                        let close = after_string.saturating_sub(3);
                        (string_start + 3, close)
                    } else {
                        let close = after_string.saturating_sub(1);
                        (string_start + 1, close)
                    };
                    let content_end = content_end.min(bytes.len());
                    let content_start = content_start.min(content_end);
                    if let Ok(token) = std::str::from_utf8(&bytes[content_start..content_end]) {
                        if !token.is_empty() {
                            return Some((token.to_owned(), after_string));
                        }
                    }
                }
                index = after_string;
            }
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                if depth <= 0 {
                    // Closed the call's argument list without finding a string.
                    return None;
                }
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency_tokens(manifest: &CppManifest) -> Vec<(&str, DependencyMechanism)> {
        manifest
            .dependencies
            .iter()
            .map(|record| (record.token.as_str(), record.mechanism))
            .collect()
    }

    #[test]
    fn parse_empty_input_does_not_panic() {
        let manifest = parse("");
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn parse_basic_dependency_and_subproject() {
        let text = r#"
project('myproject', 'cpp', version: '1.0')
dep_zlib = dependency('zlib')
subproject('googletest')
"#;
        let manifest = parse(text);
        let pairs = dependency_tokens(&manifest);
        assert!(pairs.contains(&("zlib", DependencyMechanism::PkgConfig)));
        assert!(pairs.contains(&("googletest", DependencyMechanism::Wrap)));
    }

    #[test]
    fn parse_comment_mid_line_does_not_extract_from_comment() {
        let text = "dependency('libpng') # also need dependency('libjpeg')\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "libpng");
    }

    #[test]
    fn parse_call_spanning_multiple_lines() {
        let text = "dependency(\n  'openssl',\n  version: '>= 1.1'\n)\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "openssl");
    }

    #[test]
    fn parse_nested_parens_inside_call() {
        let text = "dependency('glib-2.0', version: '>= 2.56', required: get_option('glib'))\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "glib-2.0");
    }

    #[test]
    fn parse_unterminated_string_does_not_panic() {
        let text = "dependency('unterminated";
        let manifest = parse(text);
        // May or may not extract token, but must not panic.
        let _ = manifest;
    }

    #[test]
    fn parse_crlf_line_endings() {
        let text = "dependency('sdl2')\r\nsubproject('catch2')\r\n";
        let manifest = parse(text);
        let pairs = dependency_tokens(&manifest);
        assert!(pairs.contains(&("sdl2", DependencyMechanism::PkgConfig)));
        assert!(pairs.contains(&("catch2", DependencyMechanism::Wrap)));
    }

    #[test]
    fn parse_multiple_calls_per_line() {
        let text = "a = dependency('libfoo'); b = dependency('libbar')\n";
        let manifest = parse(text);
        let tokens: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|r| r.token.as_str())
            .collect();
        assert!(tokens.contains(&"libfoo"));
        assert!(tokens.contains(&"libbar"));
    }

    #[test]
    fn parse_triple_quoted_string_arg() {
        let text = "dependency('''my-lib''')\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "my-lib");
    }

    #[test]
    fn parse_facts_dependencies_mirror_synced() {
        let text = "dependency('openssl')\nsubproject('catch2')\n";
        let manifest = parse(text);
        assert!(manifest.facts.dependencies.contains(&"openssl".to_owned()));
        assert!(manifest.facts.dependencies.contains(&"catch2".to_owned()));
    }
}
