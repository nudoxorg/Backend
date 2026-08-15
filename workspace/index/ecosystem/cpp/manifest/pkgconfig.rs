//! Parser for `*.pc` and `*.pc.in` pkg-config descriptor files (REGISTRYLESS §8).
//!
//! Extracts:
//! - `Description:` → `facts.description`.
//! - `Requires:` and `Requires.private:` → dependency records with mechanism
//!   [`DependencyMechanism::PkgConfig`].
//!
//! ## Format notes
//!
//! pkg-config files use a `Key: value` line format. Dependency lists are
//! comma- or whitespace-separated sequences of package specs, each of the
//! form `<name> [op version]` where `op` is one of `>=`, `<=`, `=`, `>`, `<`,
//! `!=`. Only the package name (text before the first operator or whitespace
//! followed by an operator) is extracted.
//!
//! `.pc.in` template files contain `@VARIABLE@` and `${var}` placeholders.
//! These are left in place — placeholder tokens in the package name position
//! are extracted as-is and not expanded.
//!
//! Variable definition lines (`Key=value`, no space before `=`) and `#`-line
//! comments are skipped. The parser never panics.

use super::{CppManifest, DependencyMechanism, DependencyRecord};

/// Parse a `*.pc` or `*.pc.in` pkg-config file.
///
/// Returns a [`CppManifest`] populated with:
/// - `facts.description` from the `Description:` field.
/// - Dependency records from `Requires:` and `Requires.private:`.
///
/// Any unrecognised lines are silently skipped. The parser never panics.
pub fn parse(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();

    for line in text.lines() {
        let line = line.trim_end_matches('\r'); // Handle CRLF.
        let trimmed = line.trim();

        // Skip blank lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // pkg-config metadata fields use `Key: value` (colon + space).
        // Variable assignments use `Key=value` (no colon) — skip them.
        if let Some(colon_position) = trimmed.find(':') {
            // Make sure this is not a variable assignment (no `=` before `:`).
            let before_colon = &trimmed[..colon_position];
            if before_colon.contains('=') {
                // This looks like a URL or assignment — not a metadata field.
                continue;
            }

            let key = before_colon.trim();
            let value = trimmed[colon_position + 1..].trim();

            if key.eq_ignore_ascii_case("Description") {
                if manifest.facts.description.is_none() && !value.is_empty() {
                    manifest.facts.description = Some(value.to_owned());
                }
            } else if key.eq_ignore_ascii_case("Requires")
                || key.eq_ignore_ascii_case("Requires.private")
            {
                for token in parse_requires_list(value) {
                    manifest.push_dependency(DependencyRecord::new(
                        token,
                        DependencyMechanism::PkgConfig,
                    ));
                }
            }
        }
    }

    manifest
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a `Requires:` or `Requires.private:` value into individual package
/// name tokens.
///
/// The value is a comma- or whitespace-separated list of package specs.
/// Each spec has the form `<name>` or `<name> <op> <version>` where `op` is
/// a comparison operator. Only `<name>` is returned per spec.
///
/// Empty specs and specs containing only operators are silently dropped.
fn parse_requires_list(value: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();

    // Split on commas first, then handle each comma-segment as a
    // whitespace-separated sequence of (name [op version]) specs.
    for comma_segment in value.split(',') {
        let segment = comma_segment.trim();
        if segment.is_empty() {
            continue;
        }

        // Within each comma segment, consume tokens; when we see a version
        // operator token, skip it and the following version string.
        let mut word_iterator = segment.split_ascii_whitespace();
        while let Some(word) = word_iterator.next() {
            if is_version_operator(word) {
                // Skip the version string that follows the operator.
                let _ = word_iterator.next();
                continue;
            }
            // Strip any embedded version operator suffix from the word itself
            // (e.g. `glib-2.0>=2.40` written without spaces).
            let name = strip_embedded_version_constraint(word);
            if !name.is_empty() {
                tokens.push(name.to_owned());
            }
        }
    }

    tokens
}

/// Returns `true` when `word` is a standalone version comparison operator.
fn is_version_operator(word: &str) -> bool {
    matches!(word, ">=" | "<=" | "!=" | "=" | ">" | "<")
}

/// Strip an embedded version constraint from a package spec token.
///
/// Examples: `glib-2.0>=2.40` → `glib-2.0`, `foo` → `foo`.
fn strip_embedded_version_constraint(spec: &str) -> &str {
    spec.find(['>', '<', '=', '!'])
        .map_or_else(|| spec.trim(), |position| spec[..position].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pkgconfig_basic() {
        let text = "Name: libfoo\nDescription: A useful library\nVersion: 1.0\nRequires: glib-2.0 >= 2.40\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("A useful library")
        );
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "glib-2.0");
        assert_eq!(
            manifest.dependencies[0].mechanism,
            DependencyMechanism::PkgConfig
        );
    }

    #[test]
    fn parse_pkgconfig_requires_private() {
        let text = "Description: Test\nRequires.private: zlib, libpng >= 1.6\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "zlib");
        assert_eq!(manifest.dependencies[1].token, "libpng");
    }

    #[test]
    fn parse_pkgconfig_comma_and_space_separated() {
        let text = "Requires: foo, bar >= 1.0, baz\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 3);
        let tokens: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|d| d.token.as_str())
            .collect();
        assert!(tokens.contains(&"foo"));
        assert!(tokens.contains(&"bar"));
        assert!(tokens.contains(&"baz"));
    }

    #[test]
    fn parse_pkgconfig_embedded_version_constraint() {
        // Some .pc files write `glib-2.0>=2.40` without spaces.
        let text = "Requires: glib-2.0>=2.40 gio-2.0\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "glib-2.0");
        assert_eq!(manifest.dependencies[1].token, "gio-2.0");
    }

    #[test]
    fn parse_pkgconfig_template_placeholders_passed_through() {
        // `.pc.in` files contain `@VAR@` — tokens should be kept as-is.
        let text = "Description: @DESCRIPTION@\nRequires: @DEP_NAME@ >= @DEP_VERSION@\n";
        let manifest = parse(text);
        // The placeholder name should be extracted as a token.
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "@DEP_NAME@");
    }

    #[test]
    fn parse_pkgconfig_crlf_line_endings() {
        let text = "Description: CRLF lib\r\nRequires: openssl\r\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.description.as_deref(), Some("CRLF lib"));
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "openssl");
    }

    #[test]
    fn parse_pkgconfig_comments_and_blank_lines_skipped() {
        let text = "# comment\n\nDescription: A lib\n# another comment\nRequires: zlib\n";
        let manifest = parse(text);
        assert_eq!(manifest.facts.description.as_deref(), Some("A lib"));
        assert_eq!(manifest.dependencies.len(), 1);
    }

    #[test]
    fn parse_pkgconfig_empty_input_does_not_panic() {
        let manifest = parse("");
        assert_eq!(manifest, CppManifest::default());
    }

    #[test]
    fn parse_pkgconfig_variable_assignments_skipped() {
        // Variable assignment lines (`prefix=/usr`) must not be treated as metadata.
        let text = "prefix=/usr\nlibdir=${prefix}/lib\nDescription: Real description\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("Real description")
        );
    }
}
