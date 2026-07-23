//! Parser for `conanfile.py` and `conanfile.txt` manifests (REGISTRYLESS §8).
//!
//! A single [`parse`] entry point handles both formats via content detection.
//! **Never executes Python** — conanfile.py is treated as plain text and
//! scanned for static string literals only. Dynamic or computed values are
//! silently skipped; partial extraction is correct behaviour.
//!
//! ## conanfile.txt
//! Section-based INI-like format. The `[requires]` and `[tool_requires]`
//! sections list one dependency per line as `name/version`. The token before
//! the first `/` is taken as the package name.
//!
//! ## conanfile.py
//! Python class scanned as text. Extracts:
//! - `description = "..."` (or `'...'`) → `facts.description`
//! - `license = "..."` → `facts.license`
//! - `homepage = "..."` → `facts.repository`
//! - `topics = ("a", "b")` or `["a", "b"]` → `facts.keywords`
//! - `requires = "dep/ver"` (single)
//! - `requires = ("dep1/ver", "dep2/ver")` (tuple/list)
//! - `self.requires("dep/ver")` call form
//!
//! All mechanisms produce [`DependencyMechanism::Recipe`] edges.

use super::{CppManifest, DependencyMechanism, DependencyRecord};

/// Parse a `conanfile.py` or `conanfile.txt` manifest.
///
/// Format is auto-detected by the presence of `[requires]` (txt) or the
/// Python `class` keyword / `def ` (py). On any parse error the affected
/// field is silently skipped; the parser never panics.
pub fn parse(text: &str) -> CppManifest {
    if is_conanfile_txt(text) {
        parse_conanfile_txt(text)
    } else {
        parse_conanfile_py(text)
    }
}

/// Returns `true` when `text` looks like a conanfile.txt (section-header
/// style) rather than a Python file.
fn is_conanfile_txt(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            return true;
        }
        // If we see Python indicators first, it's a .py
        if trimmed.starts_with("class ") || trimmed.starts_with("def ") || trimmed.starts_with("import ") {
            return false;
        }
    }
    // Default to py-style scanning when ambiguous.
    false
}

// ── conanfile.txt ────────────────────────────────────────────────────────────

/// Parse the INI-style `conanfile.txt` format.
fn parse_conanfile_txt(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();
    let mut in_requires_section = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Ignore blank lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with('[') {
            // Section header.
            let section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            in_requires_section =
                section.eq_ignore_ascii_case("requires")
                || section.eq_ignore_ascii_case("tool_requires");
            continue;
        }

        if in_requires_section {
            if let Some(token) = conan_dep_token(trimmed) {
                manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
            }
        }
    }

    manifest
}

// ── conanfile.py ─────────────────────────────────────────────────────────────

/// Parse the Python-source `conanfile.py` format via static text scanning.
///
/// Never executes Python. Scans for:
/// - Single-line `key = "value"` or `key = 'value'` assignments for
///   `description`, `license`, `homepage`, and `topics`.
/// - Requirement declarations in several common forms.
fn parse_conanfile_py(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();

    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();

        // Skip blank lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            index += 1;
            continue;
        }

        // Strip inline comment (outside strings — best-effort; handles the
        // common case of `key = "val"  # comment`).
        let effective = strip_inline_comment(trimmed);

        // ── Metadata assignments ──────────────────────────────────────────
        if manifest.facts.description.is_none() {
            if let Some(value) = extract_string_assignment(effective, "description") {
                manifest.facts.description = Some(value);
            }
        }
        if manifest.facts.license.is_none() {
            if let Some(value) = extract_string_assignment(effective, "license") {
                manifest.facts.license = Some(value);
            }
        }
        if manifest.facts.repository.is_none() {
            if let Some(value) = extract_string_assignment(effective, "homepage") {
                manifest.facts.repository = Some(value);
            }
        }
        if manifest.facts.keywords.is_empty() {
            if let Some(keywords) = extract_topics(effective) {
                manifest.facts.keywords = keywords;
            }
        }

        // ── Requirement declarations ──────────────────────────────────────

        // `self.requires("dep/ver")` or `self.tool_requires("dep/ver")`
        if trimmed.contains("self.requires(") || trimmed.contains("self.tool_requires(") {
            if let Some(token) = extract_single_call_dep(trimmed) {
                manifest
                    .push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
            }
            index += 1;
            continue;
        }

        // `requires = "dep/ver"` — single string assignment.
        if let Some(value) = extract_string_assignment(effective, "requires") {
            if let Some(token) = conan_dep_token(&value) {
                manifest
                    .push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
            }
            index += 1;
            continue;
        }

        // `requires = ("dep1/ver", "dep2/ver", ...)` or list form.
        // Accumulate lines until balanced parens/brackets.
        if let Some(stripped) = try_strip_requires_collection_start(effective) {
            let mut collected = String::from(stripped);
            // Collect continuation lines if the collection spans multiple lines.
            let open_char = if effective.contains('(') { '(' } else { '[' };
            let close_char = if open_char == '(' { ')' } else { ']' };
            let mut depth: i32 = effective.chars().filter(|&c| c == open_char).count() as i32
                - effective.chars().filter(|&c| c == close_char).count() as i32;

            index += 1;
            while depth > 0 && index < lines.len() {
                let continuation = lines[index].trim();
                depth += continuation.chars().filter(|&c| c == open_char).count() as i32;
                depth -= continuation.chars().filter(|&c| c == close_char).count() as i32;
                collected.push(' ');
                collected.push_str(continuation);
                index += 1;
            }
            // Extract all quoted strings from `collected`.
            for token in extract_all_quoted_dep_tokens(&collected) {
                manifest
                    .push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
            }
            continue;
        }

        index += 1;
    }

    manifest
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract the package name token from a Conan requirement spec like
/// `"zlib/1.2.11"` or `"zlib"`. Returns the text before the first `/`, or
/// the whole string when no `/` is present. Returns `None` for empty or
/// comment-only lines.
fn conan_dep_token(spec: &str) -> Option<String> {
    let spec = spec.trim().trim_matches(|c| c == '"' || c == '\'');
    let spec = spec.trim();
    if spec.is_empty() || spec.starts_with('#') {
        return None;
    }
    let token = spec.split('/').next().unwrap_or(spec).trim();
    if token.is_empty() { None } else { Some(token.to_owned()) }
}

/// Extract a single-line `key = "value"` or `key = 'value'` assignment.
/// Returns the unquoted value, or `None` when the line does not match the
/// pattern or the value is not a simple string literal.
fn extract_string_assignment<'a>(line: &'a str, key: &str) -> Option<String> {
    // Trim leading/trailing whitespace and look for `key` at word boundary.
    let rest = line.trim().strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    // Must start with a quote character.
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rest[quote.len_utf8()..];
    Some(extract_quoted_string(inner, quote))
}

/// Collect characters from `text` up to the matching unescaped closing `quote`
/// character. Handles `\\` and `\"` / `\'` escapes.
fn extract_quoted_string(text: &str, quote: char) -> String {
    let mut result = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) if next == quote => result.push(quote),
                Some('\\') => result.push('\\'),
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some(other) => { result.push('\\'); result.push(other); }
                None => {}
            }
        } else if c == quote {
            break;
        } else {
            result.push(c);
        }
    }
    result
}

/// Strip a best-effort `#`-comment from outside a quoted string.
/// This is a simplification: it stops at the first `#` that is NOT inside a
/// `"..."` or `'...'` section. Good enough for the common single-line case.
fn strip_inline_comment(line: &str) -> &str {
    let mut in_string: Option<char> = None;
    let mut escape_next = false;
    for (i, c) in line.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match (c, in_string) {
            ('\\', Some(_)) => { escape_next = true; }
            (q, None) if q == '"' || q == '\'' => { in_string = Some(q); }
            (q, Some(open)) if q == open => { in_string = None; }
            ('#', None) => return line[..i].trim_end(),
            _ => {}
        }
    }
    line
}

/// Extract the topics/keywords from a `topics = ("a", "b")` or `["a", "b"]`
/// assignment on a single line.
fn extract_topics(line: &str) -> Option<Vec<String>> {
    let rest = line.strip_prefix("topics")?.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    // Accept both `(` and `[`.
    let rest = if let Some(r) = rest.strip_prefix('(') {
        r
    } else if let Some(r) = rest.strip_prefix('[') {
        r
    } else {
        return None;
    };

    let keywords: Vec<String> = extract_all_quoted_strings(rest);
    if keywords.is_empty() { None } else { Some(keywords) }
}

/// Extract all quoted string literals from a snippet, returning their unquoted
/// contents.
fn extract_all_quoted_strings(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut s = String::new();
            let mut done = false;
            while let Some(inner) = chars.next() {
                if inner == '\\' {
                    // consume next char as escaped
                    if let Some(next) = chars.next() {
                        if next == quote { s.push(quote); } else { s.push(next); }
                    }
                } else if inner == quote {
                    done = true;
                    break;
                } else {
                    s.push(inner);
                }
            }
            if done && !s.is_empty() {
                result.push(s);
            }
        }
    }
    result
}

/// Extract all dep tokens from a string containing quoted dep specs.
fn extract_all_quoted_dep_tokens(text: &str) -> Vec<String> {
    extract_all_quoted_strings(text)
        .into_iter()
        .filter_map(|s| conan_dep_token(&s))
        .collect()
}

/// If the line starts a `requires = (` or `requires = [` collection, return
/// the text after the opening bracket/paren. Returns `None` otherwise.
fn try_strip_requires_collection_start(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("requires")?.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    // Must start with `(` or `[`, not a quote (single string handled separately).
    if rest.starts_with('(') {
        Some(&rest[1..])
    } else if rest.starts_with('[') {
        Some(&rest[1..])
    } else {
        None
    }
}

/// Extract the dep token from a `self.requires("dep/ver")` call.
fn extract_single_call_dep(line: &str) -> Option<String> {
    // Find the opening paren of the `requires(` call.
    let after_paren = if let Some(pos) = line.find("self.requires(") {
        &line[pos + "self.requires(".len()..]
    } else if let Some(pos) = line.find("self.tool_requires(") {
        &line[pos + "self.tool_requires(".len()..]
    } else {
        return None;
    };

    let after_paren = after_paren.trim_start();
    let quote = after_paren.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value = extract_quoted_string(&after_paren[quote.len_utf8()..], quote);
    conan_dep_token(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conanfile_txt_basic_requires() {
        let text = "[requires]\nzlib/1.2.11\nopenssl/3.0.0\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "zlib");
        assert_eq!(manifest.dependencies[1].token, "openssl");
        assert!(manifest.dependencies.iter().all(|d| d.mechanism == DependencyMechanism::Recipe));
    }

    #[test]
    fn parse_conanfile_txt_tool_requires() {
        let text = "[tool_requires]\ncmake/3.26.0\nninja/1.11.1\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "cmake");
    }

    #[test]
    fn parse_conanfile_txt_comments_and_blank_lines() {
        let text = "[requires]\n# this is a comment\n\nzlib/1.2.11\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "zlib");
    }

    #[test]
    fn parse_conanfile_py_metadata() {
        let text = r#"
class MyConan(ConanFile):
    description = "A great library"
    license = "MIT"
    homepage = "https://github.com/example/mylib"
    topics = ("networking", "http")
    requires = "zlib/1.2.11"
"#;
        let manifest = parse(text);
        assert_eq!(manifest.facts.description.as_deref(), Some("A great library"));
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.facts.repository.as_deref(), Some("https://github.com/example/mylib"));
        assert_eq!(manifest.facts.keywords, vec!["networking", "http"]);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "zlib");
    }

    #[test]
    fn parse_conanfile_py_self_requires_call() {
        let text = r#"
    def requirements(self):
        self.requires("openssl/3.0.0")
        self.requires("zlib/1.2.11")
"#;
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "openssl");
        assert_eq!(manifest.dependencies[1].token, "zlib");
    }

    #[test]
    fn parse_conanfile_py_multiline_requires_tuple() {
        let text = r#"requires = (
    "zlib/1.2.11",
    "openssl/3.0.0",
)
"#;
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "zlib");
        assert_eq!(manifest.dependencies[1].token, "openssl");
    }

    #[test]
    fn parse_empty_input_does_not_panic() {
        let manifest = parse("");
        assert_eq!(manifest, CppManifest::default());
    }

    #[test]
    fn parse_conanfile_py_inline_comment_does_not_corrupt_value() {
        // The `#` in the comment must not truncate the description value.
        let text = r#"description = "A library"  # important"#;
        let manifest = parse(text);
        assert_eq!(manifest.facts.description.as_deref(), Some("A library"));
    }
}
