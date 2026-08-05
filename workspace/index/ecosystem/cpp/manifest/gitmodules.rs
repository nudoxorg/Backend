//! Parser for `.gitmodules` manifests (REGISTRYLESS §8).
//!
//! Parses the git-config INI format used by `.gitmodules`. Each
//! `[submodule "name"]` section may contain a `url = <url>` key and a
//! `path = <path>` key. For each submodule, the `url` value is normalized via
//! [`crate::ecosystem::repo::normalize_repo_url`]:
//!
//! - If normalization succeeds, the resulting [`RepoSlug`] string becomes the
//!   dependency token (highest-value edge — self-resolving).
//! - If normalization returns `None`, the raw URL string is stored as the
//!   token (EDB law RL-5: store what the extractor saw).
//!
//! Comment characters: `#` and `;` (both are valid in git-config).
//! Keys are indented with tabs or spaces (git-config convention).
//! CRLF line endings are handled transparently via [`str::lines`].

use super::{CppManifest, DependencyMechanism, DependencyRecord};

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse a `.gitmodules` file and return the extracted manifest.
///
/// Each submodule whose `url` key is present produces one
/// [`DependencyRecord`] with mechanism [`DependencyMechanism::Submodule`].
/// The token is the [`crate::ecosystem::repo::normalize_repo_url`] slug when
/// normalization succeeds, otherwise the raw URL string. Never panics.
pub fn parse(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();

    // Accumulated state for the section currently being parsed.
    let mut current_url: Option<String> = None;
    let mut in_submodule_section = false;

    for line in text.lines() {
        let line = strip_inline_comment(line).trim();

        if line.is_empty() {
            continue;
        }

        if let Some(section_kind) = try_parse_section_header(line) {
            // Flush the previous section before moving on.
            if in_submodule_section {
                flush_submodule(&mut manifest, current_url.take());
            }
            current_url = None;
            in_submodule_section = matches!(section_kind, SectionKind::Submodule);
            continue;
        }

        if !in_submodule_section {
            continue;
        }

        if let Some(key_value) = try_parse_key_value(line) {
            match key_value.key {
                "url" => {
                    current_url = Some(key_value.value.to_owned());
                }
                // `path` and other keys are silently ignored — only `url`
                // contributes a dependency edge.
                _ => {}
            }
        }
    }

    // Flush the last section.
    if in_submodule_section {
        flush_submodule(&mut manifest, current_url.take());
    }

    manifest
}

// ── Internal types ────────────────────────────────────────────────────────────

/// The kind of git-config section header encountered.
#[derive(Debug, Clone, Copy)]
enum SectionKind {
    /// `[submodule "name"]`
    Submodule,
    /// Any other section header.
    Other,
}

/// A parsed `key = value` pair from a git-config line.
struct KeyValue<'line> {
    key: &'line str,
    value: &'line str,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Emit a [`DependencyRecord`] for the completed submodule section, if a URL
/// was found.
fn flush_submodule(manifest: &mut CppManifest, url: Option<String>) {
    let Some(raw_url) = url else { return };
    let token = match crate::ecosystem::repo::normalize_repo_url(&raw_url) {
        Some(slug) => slug.as_str().to_owned(),
        None => raw_url,
    };
    manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Submodule));
}

/// Strip an inline comment from a line: everything from the first `#` or `;`
/// that is not inside a quoted value.
///
/// Git-config does not allow inline comments after a `value` that itself
/// contains `#`, but in practice tools emit them. We strip conservatively:
/// only the first unquoted `#` or `;` is treated as a comment start.
fn strip_inline_comment(line: &str) -> &str {
    // Comments are rare inside `url = ...` values; a simple scan suffices.
    let mut chars = line.char_indices();
    let mut in_double_quote = false;
    while let Some((index, character)) = chars.next() {
        match character {
            '"' => in_double_quote = !in_double_quote,
            '#' | ';' if !in_double_quote => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Parse a git-config section header line of the form `[section]` or
/// `[section "subsection"]`. Returns `Some(SectionKind)` if the line is a
/// section header, `None` otherwise.
fn try_parse_section_header(line: &str) -> Option<SectionKind> {
    let line = line.trim();
    if !line.starts_with('[') || !line.ends_with(']') {
        return None;
    }
    let interior = &line[1..line.len() - 1].trim_end();
    // The section name is the first whitespace-delimited token.
    let section_name = interior.split_ascii_whitespace().next().unwrap_or("");
    if section_name.eq_ignore_ascii_case("submodule") {
        Some(SectionKind::Submodule)
    } else {
        Some(SectionKind::Other)
    }
}

/// Parse a git-config key-value line of the form `key = value` (leading
/// whitespace already stripped by the caller). Returns `None` for lines that
/// do not contain `=`.
fn try_parse_key_value(line: &str) -> Option<KeyValue<'_>> {
    let equals_position = line.find('=')?;
    let key = line[..equals_position].trim();
    let value = line[equals_position + 1..].trim();
    if key.is_empty() {
        return None;
    }
    Some(KeyValue { key, value })
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
    fn parse_basic_submodule_with_https_url() {
        let text = "[submodule \"third_party/zlib\"]\n\tpath = third_party/zlib\n\turl = https://github.com/madler/zlib.git\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "github.com/madler/zlib");
        assert_eq!(
            manifest.dependencies[0].mechanism,
            DependencyMechanism::Submodule
        );
    }

    #[test]
    fn parse_scp_style_url_normalized() {
        let text =
            "[submodule \"ext/fmt\"]\n\tpath = ext/fmt\n\turl = git@github.com:fmtlib/fmt.git\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "github.com/fmtlib/fmt");
    }

    #[test]
    fn parse_url_that_cannot_be_normalized_uses_raw() {
        let text = "[submodule \"internal\"]\n\tpath = internal\n\turl = /local/path/to/repo\n";
        let manifest = parse(text);
        // normalize_repo_url returns None for non-URL paths.
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "/local/path/to/repo");
    }

    #[test]
    fn parse_section_without_url_skipped() {
        let text = "[submodule \"missing-url\"]\n\tpath = some/path\n";
        let manifest = parse(text);
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn parse_comment_lines_ignored() {
        let text = "# This is a comment\n[submodule \"lib\"]\n\t; another comment\n\turl = https://github.com/owner/repo.git\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "github.com/owner/repo");
    }

    #[test]
    fn parse_crlf_line_endings() {
        let text = "[submodule \"a\"]\r\n\turl = https://github.com/user/a.git\r\n[submodule \"b\"]\r\n\turl = https://github.com/user/b.git\r\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
    }

    #[test]
    fn parse_multiple_submodules() {
        let text = "[submodule \"deps/googletest\"]\n\tpath = deps/googletest\n\turl = https://github.com/google/googletest.git\n\n[submodule \"deps/abseil\"]\n\tpath = deps/abseil\n\turl = https://github.com/abseil/abseil-cpp.git\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        let tokens: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|r| r.token.as_str())
            .collect();
        assert!(tokens.contains(&"github.com/google/googletest"));
        assert!(tokens.contains(&"github.com/abseil/abseil-cpp"));
    }

    #[test]
    fn parse_facts_mirror_synced() {
        let text = "[submodule \"lib\"]\n\turl = https://github.com/owner/lib.git\n";
        let manifest = parse(text);
        assert!(
            manifest
                .facts
                .dependencies
                .contains(&"github.com/owner/lib".to_owned())
        );
    }

    #[test]
    fn parse_non_submodule_section_ignored() {
        let text = "[core]\n\trepositoryformatversion = 0\n[submodule \"real\"]\n\turl = https://github.com/user/real.git\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "github.com/user/real");
    }
}
