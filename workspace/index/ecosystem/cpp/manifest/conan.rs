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
//! - `homepage = "..."` → `facts.repository`, and sets `facts.documentation`
//!   (homepage-present is used as the documentation-URL proxy, matching the
//!   `ts`/`pkgconfig` ecosystem convention — conanfile.py has no dedicated
//!   documentation key of its own).
//! - `topics = ("a", "b")` or `["a", "b"]` → `facts.keywords`
//! - `exports`/`exports_sources` naming a `LICENSE`/`LICENCE`/`COPYING` file,
//!   or a `self.copy("LICENSE", ...)` / `copy(self, "LICENSE*", ...)` call in
//!   `package()` → `facts.has_license_file`.
//! - `requires = "dep/ver"` (single)
//! - `requires = ("dep1/ver", "dep2/ver")` (tuple/list)
//! - `self.requires("dep/ver")` call form
//!
//! All mechanisms produce [`DependencyMechanism::Recipe`] edges.
//!
//! **Deliberately NOT extracted**: the `url` attribute. In the Conan Center
//! Index convention (the vast majority of real-world conanfile.py recipes),
//! `url` points at the *recipe* repository (typically
//! `github.com/conan-io/conan-center-index`), not the packaged library's own
//! source — using it for `facts.repository` would systematically misattribute
//! the repo for nearly every centrally-indexed recipe. `homepage` is the
//! correct, conventional source and is what's used here.

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
        if trimmed.starts_with("class ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("import ")
        {
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
            in_requires_section = section.eq_ignore_ascii_case("requires")
                || section.eq_ignore_ascii_case("tool_requires");
            continue;
        }

        if in_requires_section && let Some(token) = conan_dep_token(trimmed) {
            manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
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
        if manifest.facts.description.is_none()
            && let Some(value) = extract_string_assignment(effective, "description")
        {
            manifest.facts.description = Some(value);
        }
        if manifest.facts.license.is_none()
            && let Some(value) = extract_string_assignment(effective, "license")
        {
            manifest.facts.license = Some(value);
        }
        if manifest.facts.repository.is_none()
            && let Some(value) = extract_string_assignment(effective, "homepage")
        {
            // NOTE: deliberately not also reading the sibling `url` attribute
            // here — see the module-level "not extracted" note below.
            manifest.facts.repository = Some(value);
            manifest.facts.documentation = true;
        }
        if manifest.facts.keywords.is_empty()
            && let Some(keywords) = extract_topics(effective)
        {
            manifest.facts.keywords = keywords;
        }

        // ── `has_license_file` signal ───────────────────────────────────────
        // `exports` / `exports_sources = "LICENSE"` (single string or
        // tuple/list) packages a license file alongside the recipe — a
        // structural signal, same idea as Cargo's `license-file` key.
        if !manifest.facts.has_license_file
            && (line_declares_license_export(effective, "exports")
                || line_declares_license_export(effective, "exports_sources"))
        {
            manifest.facts.has_license_file = true;
        }
        // `self.copy("LICENSE", ...)` (Conan 1.x) / `copy(self, "LICENSE*", ...)`
        // (Conan 2.x, `conan.tools.files.copy`) in `package()` — the standard
        // idiom for bundling the license text into the built package.
        if !manifest.facts.has_license_file && line_copies_license_file(trimmed) {
            manifest.facts.has_license_file = true;
        }

        // ── Requirement declarations ──────────────────────────────────────

        // `self.requires("dep/ver")` or `self.tool_requires("dep/ver")`
        if trimmed.contains("self.requires(") || trimmed.contains("self.tool_requires(") {
            if let Some(token) = extract_single_call_dep(trimmed) {
                manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
            }
            index += 1;
            continue;
        }

        // `requires = "dep/ver"` — single string assignment.
        if let Some(value) = extract_string_assignment(effective, "requires") {
            if let Some(token) = conan_dep_token(&value) {
                manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
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
                manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
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
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// Extract a single-line `key = "value"` or `key = 'value'` assignment.
/// Returns the unquoted value, or `None` when the line does not match the
/// pattern or the value is not a simple string literal.
fn extract_string_assignment(line: &str, key: &str) -> Option<String> {
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
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
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
            ('\\', Some(_)) => {
                escape_next = true;
            }
            (q, None) if q == '"' || q == '\'' => {
                in_string = Some(q);
            }
            (q, Some(open)) if q == open => {
                in_string = None;
            }
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
    let rest = rest.strip_prefix('(').or_else(|| rest.strip_prefix('['))?;

    let keywords: Vec<String> = extract_all_quoted_strings(rest);
    if keywords.is_empty() {
        None
    } else {
        Some(keywords)
    }
}

/// Extract all quoted string literals from a snippet, returning their unquoted
/// contents.
fn extract_all_quoted_strings(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut s = String::new();
            let mut done = false;
            while let Some(inner) = chars.next() {
                if inner == '\\' {
                    // consume next char as escaped
                    if let Some(next) = chars.next() {
                        if next == quote {
                            s.push(quote);
                        } else {
                            s.push(next);
                        }
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
    let rest = rest.strip_prefix('=')?.trim_start();
    // Must start with `(` or `[`, not a quote (single string handled separately).
    rest.strip_prefix('(').or_else(|| rest.strip_prefix('['))
}

/// Whether `s` (a filename or glob, path components already allowed) names a
/// conventional license file: `LICENSE`, `LICENCE`, `COPYING`, optionally
/// with an extension or a trailing `*` glob, case-insensitive, ignoring any
/// leading path components (`licenses/LICENSE.md`, `LICENSE*`).
fn looks_like_license_filename(s: &str) -> bool {
    let leaf = s.rsplit(['/', '\\']).next().unwrap_or(s).trim();
    let stem = leaf.trim_end_matches('*').split('.').next().unwrap_or(leaf);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "LICENSE" | "LICENCE" | "COPYING"
    )
}

/// Whether `line` is an `exports = "..."` / `exports_sources = (...)`
/// assignment (single string, or tuple/list of strings on one line) naming a
/// license file among its entries. Multi-line collections are not followed —
/// consistent with this parser's line-oriented, best-effort scanning
/// elsewhere; a license file named on a continuation line of a multi-line
/// `exports_sources = (...)` is simply not detected (no fabrication, no
/// false positive).
fn line_declares_license_export(line: &str, key: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix(key) else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix('=') else {
        return false;
    };
    // Scan every quoted string on the line rather than just the first: the
    // value may be a single string, a bare comma tuple (`"a", "b"`), or a
    // parenthesized/bracketed tuple — and the license entry need not be
    // first in any of those forms.
    extract_all_quoted_strings(rest)
        .iter()
        .any(|s| looks_like_license_filename(s))
}

/// Whether `line` contains a Conan `self.copy("LICENSE", ...)` (1.x) or
/// `copy(self, "LICENSE*", ...)` (2.x, `conan.tools.files.copy`) call whose
/// first quoted argument names a license file/glob.
fn line_copies_license_file(line: &str) -> bool {
    let after = if let Some(pos) = line.find("self.copy(") {
        &line[pos + "self.copy(".len()..]
    } else if let Some(pos) = line.find("copy(self,") {
        &line[pos + "copy(self,".len()..]
    } else {
        return false;
    };
    extract_all_quoted_strings(after)
        .first()
        .is_some_and(|first| looks_like_license_filename(first))
}

/// Extract the dep token from a `self.requires("dep/ver")` call.
fn extract_single_call_dep(line: &str) -> Option<String> {
    // Find the opening paren of the `requires(` call.
    let after_paren = line
        .find("self.requires(")
        .map(|pos| &line[pos + "self.requires(".len()..])
        .or_else(|| {
            line.find("self.tool_requires(")
                .map(|pos| &line[pos + "self.tool_requires(".len()..])
        })?;

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
        assert!(
            manifest
                .dependencies
                .iter()
                .all(|d| d.mechanism == DependencyMechanism::Recipe)
        );
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
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("A great library")
        );
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://github.com/example/mylib")
        );
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

    // ── `documentation` (P6 gap fill) ────────────────────────────────────────

    #[test]
    fn parse_conanfile_py_homepage_sets_documentation() {
        let text = r#"homepage = "https://mylib.dev""#;
        let manifest = parse(text);
        assert!(manifest.facts.documentation);
    }

    #[test]
    fn parse_conanfile_py_no_homepage_documentation_false() {
        let manifest = parse(r#"license = "MIT""#);
        assert!(!manifest.facts.documentation);
    }

    // ── `has_license_file` (P6 gap fill) ─────────────────────────────────────

    #[test]
    fn parse_conanfile_py_exports_single_license_string() {
        let manifest = parse(r#"exports = "LICENSE""#);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_exports_sources_tuple_with_license_not_first() {
        let text = r#"exports_sources = "CMakeLists.txt", "LICENSE.md", "src/*""#;
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_exports_sources_parenthesized_tuple() {
        let text = r#"exports_sources = ("CMakeLists.txt", "COPYING", "src/*")"#;
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_exports_without_license_leaves_flag_false() {
        let text = r#"exports_sources = ("CMakeLists.txt", "src/*")"#;
        let manifest = parse(text);
        assert!(!manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_self_copy_license_call() {
        let text = r#"
    def package(self):
        self.copy("LICENSE", dst="licenses", src=self._source_subfolder)
"#;
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_conan2_copy_call() {
        let text = r#"
    def package(self):
        copy(self, "LICENSE*", self.source_folder, os.path.join(self.package_folder, "licenses"))
"#;
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_self_copy_non_license_does_not_set_flag() {
        let text = r#"
    def package(self):
        self.copy("*.h", dst="include", src="include")
"#;
        let manifest = parse(text);
        assert!(!manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_license_assignment_alone_does_not_set_has_license_file() {
        // A `license = "MIT"` SPDX expression is a distinct signal from a
        // license *file* reference; must not cross-set the other flag.
        let manifest = parse(r#"license = "MIT""#);
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
        assert!(!manifest.facts.has_license_file);
    }

    #[test]
    fn parse_conanfile_py_real_world_recipe_shape() {
        // Shaped after a real conan-center-index recipe (zlib-style).
        let text = r#"
import os
from conan import ConanFile
from conan.tools.files import copy, get

class ZlibConan(ConanFile):
    name = "zlib"
    description = "A Massively Spiffy Yet Delicately Unobtrusive Compression Library"
    topics = ("zlib", "compression")
    url = "https://github.com/conan-io/conan-center-index"
    homepage = "https://zlib.net"
    license = "Zlib"

    def requirements(self):
        self.requires("zlib-ng/2.2.1", override=True)

    def package(self):
        copy(self, "LICENSE", self.source_folder, os.path.join(self.package_folder, "licenses"))
"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("A Massively Spiffy Yet Delicately Unobtrusive Compression Library")
        );
        assert_eq!(manifest.facts.license.as_deref(), Some("Zlib"));
        // `homepage`, not the recipe-pointing `url`, is the repository source.
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://zlib.net")
        );
        assert!(manifest.facts.documentation);
        assert!(manifest.facts.has_license_file);
        assert_eq!(manifest.facts.keywords, vec!["zlib", "compression"]);
    }

    // ── Hostile-input hardening ──────────────────────────────────────────────

    #[test]
    fn parse_conanfile_py_unterminated_string_no_panic() {
        let manifest = parse(r#"description = "unterminated"#);
        // Best-effort: may or may not extract a value, must not panic.
        let _ = manifest;
    }

    #[test]
    fn parse_conanfile_py_unterminated_tuple_no_panic() {
        let text = "requires = (\n    \"zlib/1.2.11\",\n    \"openssl/3.0.0\",\n";
        let manifest = parse(text);
        // Never closes — depth never returns to 0, so the continuation loop
        // must terminate on running out of lines, not hang.
        let _ = manifest;
    }

    #[test]
    fn parse_conanfile_py_crlf_line_endings() {
        let text = "description = \"CRLF lib\"\r\nlicense = \"MIT\"\r\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.description.as_deref(), Some("CRLF lib"));
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn parse_conanfile_py_unicode_topics_and_description() {
        let text = "description = \"日本語のライブラリ 😀\"\ntopics = (\"网络\", \"http\")\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("日本語のライブラリ 😀")
        );
        assert_eq!(manifest.facts.keywords, vec!["网络", "http"]);
    }

    #[test]
    fn parse_conanfile_py_deeply_nested_requires_collection_no_hang() {
        // A pathological run of opening parens in a `requires = (` collection
        // must not hang the continuation-line accumulator.
        use std::fmt::Write as _;
        let mut text = String::from("requires = (\n");
        for i in 0..5000 {
            let _ = writeln!(text, "    \"dep{i}/1.0\",");
        }
        text.push(')');
        let manifest = parse(&text);
        assert!(manifest.dependencies.len() >= 5000);
    }

    #[test]
    fn parse_conanfile_py_enormous_single_line_no_panic() {
        let huge = "x".repeat(2 * 1024 * 1024);
        let text = format!(r#"description = "{huge}""#);
        let manifest = parse(&text);
        assert_eq!(
            manifest.facts.description.as_deref().map(str::len),
            Some(huge.len())
        );
    }

    #[test]
    fn conanfile_txt_is_not_misdetected_as_py_and_vice_versa() {
        // A conanfile.txt whose first non-blank line is a section header must
        // route to the INI parser even if later lines look Python-ish.
        let text = "[requires]\nzlib/1.2.11\n# a comment mentioning class Foo:\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "zlib");
    }

    #[test]
    fn looks_like_license_filename_matches_common_conventions() {
        assert!(looks_like_license_filename("LICENSE"));
        assert!(looks_like_license_filename("License.txt"));
        assert!(looks_like_license_filename("LICENCE.md"));
        assert!(looks_like_license_filename("COPYING"));
        assert!(looks_like_license_filename("licenses/LICENSE"));
        assert!(looks_like_license_filename("LICENSE*"));
        assert!(!looks_like_license_filename("README.md"));
        assert!(!looks_like_license_filename("license_check.py"));
    }
}
