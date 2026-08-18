//! Parser for `meson.build` manifests (REGISTRYLESS §8).
//!
//! Performs a pure static text scan — no execution, no eval. Recognises three
//! call forms:
//!
//! - `project('<name>', ..., license: ..., license_files: ...)` — the project
//!   name itself is unused (meson has no description field), but the
//!   `license:` keyword argument (a single string or array of strings,
//!   joined with `" AND "`) populates `facts.license`, and a non-empty
//!   `license_files:` keyword argument (Meson 1.1.0+) sets
//!   `facts.has_license_file`.
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
                    // project() has no description field, but does carry
                    // `license:`/`license_files:` keyword arguments.
                    let (span, after_call) = extract_call_raw_span(bytes, rest_position);
                    if manifest.facts.license.is_none()
                        && let Some(values) = extract_meson_kwarg_strings(&span, "license")
                    {
                        let joined = values
                            .iter()
                            .map(String::as_str)
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" AND ");
                        if !joined.is_empty() {
                            manifest.facts.license = Some(joined);
                        }
                    }
                    if !manifest.facts.has_license_file
                        && let Some(values) = extract_meson_kwarg_strings(&span, "license_files")
                        && values.iter().any(|s| !s.trim().is_empty())
                    {
                        manifest.facts.has_license_file = true;
                    }
                    position = after_call;
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
    debug_assert_eq!(bytes.get(position), Some(&b'\''));
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
    debug_assert_eq!(bytes.get(paren_position), Some(&b'('));
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
                if depth > 0
                    && let Some(token) = quoted_string_content(bytes, string_start, after_string)
                    && !token.is_empty()
                {
                    return Some((token.to_owned(), after_string));
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

/// Given a (possibly triple-quoted) single-quoted string's span
/// `[string_start, after_string)` (as returned by
/// [`advance_past_single_quoted_string`]), return its unquoted content, or
/// `None` if the content bytes are not valid UTF-8 (never panics — this is a
/// byte-slice decode, not a `text[a..b]` string-index that could split a
/// multi-byte character and panic).
fn quoted_string_content(bytes: &[u8], string_start: usize, after_string: usize) -> Option<&str> {
    let is_triple = bytes.get(string_start..string_start + 3) == Some(b"'''");
    let (content_start, content_end) = if is_triple {
        let close = after_string.saturating_sub(3);
        (string_start + 3, close)
    } else {
        let close = after_string.saturating_sub(1);
        (string_start + 1, close)
    };
    let content_end = content_end.min(bytes.len());
    let content_start = content_start.min(content_end);
    std::str::from_utf8(&bytes[content_start..content_end]).ok()
}

/// Extract the argument-list text of a call from the opening `(` at
/// `paren_position`, tracking nested `(...)`/`[...]` depth (sharing one
/// counter — adequate for finding where the *outer* call closes even on
/// mismatched/adversarial bracket nesting, since it can only under- or
/// over-count, never index out of bounds or loop forever) and skipping over
/// quoted string contents so quote/paren/bracket characters inside a string
/// literal cannot desynchronize the scan. Returns `(argument_text,
/// index_after_closing_paren)`. On unterminated input, returns everything
/// scanned and the end of the buffer — never panics, never hangs (each
/// branch advances `index`).
fn extract_call_raw_span(bytes: &[u8], paren_position: usize) -> (String, usize) {
    debug_assert_eq!(bytes.get(paren_position), Some(&b'('));
    let mut depth: i32 = 1;
    let mut index = paren_position + 1;
    let start = index;
    while index < bytes.len() {
        match bytes[index] {
            b'#' => index = advance_past_line_end(bytes, index),
            b'\'' => index = advance_past_single_quoted_string(bytes, index),
            b'(' | b'[' => {
                depth += 1;
                index += 1;
            }
            b')' | b']' => {
                depth -= 1;
                if depth <= 0 {
                    let text = String::from_utf8_lossy(&bytes[start..index]).into_owned();
                    return (text, index + 1);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    (
        String::from_utf8_lossy(&bytes[start..index]).into_owned(),
        index,
    )
}

/// Extract the value(s) of a `key: <value>` keyword argument from a call's
/// already-isolated argument-list text (as produced by
/// [`extract_call_raw_span`]): either a single single-quoted string (returns
/// a one-element `Vec`), or a bracketed `[...]` list of single-quoted
/// strings. `key` is matched at a word boundary (so `"license"` does not
/// match inside `"license_files"`), and only the FIRST occurrence is used.
/// Returns `None` when `key:` is not present or its value is neither a
/// string nor a bracketed list of strings.
fn extract_meson_kwarg_strings(text: &str, key: &str) -> Option<Vec<String>> {
    let bytes = text.as_bytes();
    let key_bytes = key.as_bytes();
    let mut index = 0usize;
    while index + key_bytes.len() <= bytes.len() {
        if &bytes[index..index + key_bytes.len()] != key_bytes {
            index += 1;
            continue;
        }
        let before_ok =
            index == 0 || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
        let after = index + key_bytes.len();
        let after_is_ident = bytes
            .get(after)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        if !before_ok || after_is_ident {
            index += 1;
            continue;
        }
        let mut cursor = after;
        while bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b':') {
            index += 1;
            continue;
        }
        cursor += 1;
        while matches!(bytes.get(cursor), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            cursor += 1;
        }
        match bytes.get(cursor) {
            Some(b'\'') => {
                let string_start = cursor;
                let after_string = advance_past_single_quoted_string(bytes, string_start);
                return quoted_string_content(bytes, string_start, after_string)
                    .map(|s| vec![s.to_owned()]);
            }
            Some(b'[') => {
                let mut values = Vec::new();
                let mut i = cursor + 1;
                let mut bracket_depth = 1i32;
                while i < bytes.len() && bracket_depth > 0 {
                    match bytes[i] {
                        b'\'' => {
                            let string_start = i;
                            let after_string = advance_past_single_quoted_string(bytes, i);
                            if let Some(s) =
                                quoted_string_content(bytes, string_start, after_string)
                            {
                                values.push(s.to_owned());
                            }
                            i = after_string;
                        }
                        b'[' => {
                            bracket_depth += 1;
                            i += 1;
                        }
                        b']' => {
                            bracket_depth -= 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                return Some(values);
            }
            _ => return None,
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

    // ── `license:` / `license_files:` (P6 gap fill) ──────────────────────────

    #[test]
    fn parse_project_license_single_string() {
        let text = "project('myproject', 'cpp', license: 'MIT')\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn parse_project_license_array_joined_with_and() {
        let text = "project('myproject', license: ['MIT', 'BSD-3-Clause'])\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.license.as_deref(),
            Some("MIT AND BSD-3-Clause")
        );
    }

    #[test]
    fn parse_project_license_files_sets_has_license_file() {
        let text = "project('myproject', license: 'MIT', license_files: ['COPYING'])\n";
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_project_license_files_array_multiple() {
        let text = "project('myproject', license_files: ['LICENSE', 'LICENSE-THIRD-PARTY'])\n";
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_project_without_license_kwargs_leaves_fields_unset() {
        let text = "project('myproject', 'cpp', version: '1.0')\n";
        let manifest = parse(text);
        assert!(manifest.facts.license.is_none());
        assert!(!manifest.facts.has_license_file);
    }

    #[test]
    fn parse_project_license_keyword_not_confused_with_license_files() {
        // `license:` must not accidentally match inside `license_files:`.
        let text = "project('myproject', license_files: ['COPYING'])\n";
        let manifest = parse(text);
        assert!(manifest.facts.license.is_none());
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_project_license_multiline_call() {
        let text = "project(\n  'myproject',\n  license: 'Apache-2.0',\n  version: '2.0',\n)\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.license.as_deref(), Some("Apache-2.0"));
    }

    #[test]
    fn parse_real_world_meson_build_shape() {
        // Shaped after a real meson.build (spdlog-style).
        let text = r#"
project('spdlog',
  'cpp',
  version : '1.14.1',
  license : 'MIT',
  license_files : ['LICENSE'],
  default_options : ['cpp_std=c++11'],
)

spdlog_dep = dependency('fmt', version: '>=9.0.0')
catch2_dep = subproject('catch2')
"#;
        let manifest = parse(text);
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
        assert!(manifest.facts.has_license_file);
        let pairs = dependency_tokens(&manifest);
        assert!(pairs.contains(&("fmt", DependencyMechanism::PkgConfig)));
        assert!(pairs.contains(&("catch2", DependencyMechanism::Wrap)));
    }

    #[test]
    fn parse_duplicate_project_calls_first_wins() {
        let text = "project('a', license: 'MIT')\nproject('b', license: 'GPL-3.0')\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
    }

    // ── Hostile-input hardening ──────────────────────────────────────────────

    #[test]
    fn parse_project_unterminated_call_no_panic() {
        let text = "project('myproject', license: 'MIT'";
        let manifest = parse(text);
        let _ = manifest;
    }

    #[test]
    fn parse_project_unterminated_license_files_array_no_panic() {
        let text = "project('myproject', license_files: ['LICENSE', 'COPYING'\n";
        let manifest = parse(text);
        let _ = manifest;
    }

    #[test]
    fn parse_project_deeply_nested_brackets_in_license_files_no_panic() {
        let mut text = String::from("project('p', license_files: [");
        for _ in 0..5000 {
            text.push('[');
        }
        for _ in 0..5000 {
            text.push(']');
        }
        text.push_str("])\n");
        let manifest = parse(&text);
        let _ = manifest;
    }

    #[test]
    fn parse_unicode_license_string() {
        let text = "project('p', license: '日本語ライセンス 😀')\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.license.as_deref(),
            Some("日本語ライセンス 😀")
        );
    }

    #[test]
    fn parse_enormous_license_string_no_panic() {
        let huge = "x".repeat(2 * 1024 * 1024);
        let text = format!("project('p', license: '{huge}')\n");
        let manifest = parse(&text);
        assert_eq!(
            manifest.facts.license.as_deref().map(str::len),
            Some(huge.len())
        );
    }

    #[test]
    fn parse_crlf_project_call() {
        let text = "project('p',\r\n  license: 'MIT',\r\n)\r\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
    }
}
