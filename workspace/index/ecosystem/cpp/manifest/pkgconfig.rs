//! Parser for `*.pc` and `*.pc.in` pkg-config descriptor files (REGISTRYLESS
//! §8).
//!
//! Extracts:
//! - `Description:` → `facts.description`.
//! - `URL:` → `facts.repository` (the package's homepage per the pkg-config
//!   spec), and sets `facts.documentation`.
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
            } else if key.eq_ignore_ascii_case("URL") {
                if manifest.facts.repository.is_none() && !value.is_empty() {
                    manifest.facts.repository = Some(value.to_owned());
                    manifest.facts.documentation = true;
                }
            } else if key.eq_ignore_ascii_case("Requires")
                || key.eq_ignore_ascii_case("Requires.private")
            {
                for (token, requirement) in parse_requires_list(value) {
                    let mut record = DependencyRecord::new(token, DependencyMechanism::PkgConfig);
                    record.requirement = requirement;
                    manifest.push_dependency(record);
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
/// a comparison operator. The operator and version are the requirement.
///
/// Empty specs and specs containing only operators are silently dropped.
fn parse_requires_list(value: &str) -> Vec<(String, Option<String>)> {
    let mut tokens = Vec::new();

    for comma_segment in value.split(',') {
        let words: Vec<&str> = comma_segment.split_ascii_whitespace().collect();
        let mut index = 0;
        while index < words.len() {
            let word = words[index];
            if is_version_operator(word) {
                index += 2;
                continue;
            }
            let (name, embedded) = split_constraint(word);
            if name.is_empty() {
                index += 1;
                continue;
            }
            let requirement = if let Some(embedded) = embedded {
                index += 1;
                Some(embedded.to_owned())
            } else if words
                .get(index + 1)
                .is_some_and(|next| is_version_operator(next))
            {
                let operator = words[index + 1];
                let version = words.get(index + 2).copied().unwrap_or("");
                if !version.is_empty() && !is_version_operator(version) {
                    index += 3;
                    Some(format!("{operator} {version}"))
                } else {
                    index += 2;
                    Some(operator.to_owned())
                }
            } else {
                index += 1;
                None
            };
            tokens.push((name.to_owned(), requirement.filter(|text| !text.is_empty())));
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
fn split_constraint(spec: &str) -> (&str, Option<&str>) {
    match spec.find(['>', '<', '=', '!']) {
        Some(position) if position > 0 => {
            (spec[..position].trim_end(), Some(spec[position..].trim()))
        }
        _ => (spec.trim(), None),
    }
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
        assert_eq!(
            manifest.dependencies[0].requirement.as_deref(),
            Some(">= 2.40")
        );
    }

    #[test]
    fn parse_pkgconfig_requires_private() {
        let text = "Description: Test\nRequires.private: zlib, libpng >= 1.6\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "zlib");
        assert_eq!(manifest.dependencies[1].token, "libpng");
        assert_eq!(
            manifest.dependencies[1].requirement.as_deref(),
            Some(">= 1.6")
        );
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

    // ── `URL:` (P6 gap fill) ──────────────────────────────────────────────────

    #[test]
    fn parse_pkgconfig_url_sets_repository_and_documentation() {
        let text = "Name: zlib\nURL: https://zlib.net/\nDescription: compression\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://zlib.net/")
        );
        assert!(manifest.facts.documentation);
    }

    #[test]
    fn parse_pkgconfig_no_url_leaves_repository_none() {
        let text = "Name: zlib\nDescription: compression\n";
        let manifest = parse(text);
        assert!(manifest.facts.repository.is_none());
        assert!(!manifest.facts.documentation);
    }

    #[test]
    fn parse_pkgconfig_real_world_shape() {
        // Shaped after a real zlib.pc.
        let text = "prefix=/usr\nexec_prefix=${prefix}\nlibdir=${exec_prefix}/lib\nincludedir=${prefix}/include\n\nName: zlib\nDescription: zlib compression library\nVersion: 1.3.1\nURL: https://zlib.net/\nRequires:\nLibs: -L${libdir} -lz\nCflags: -I${includedir}\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("zlib compression library")
        );
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://zlib.net/")
        );
        assert!(manifest.facts.documentation);
        assert!(
            manifest.dependencies.is_empty(),
            "empty Requires: is no deps"
        );
    }

    // ── Hostile-input hardening ──────────────────────────────────────────────

    #[test]
    fn parse_pkgconfig_url_key_is_case_insensitive() {
        let text = "url: https://example.com\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn parse_pkgconfig_unicode_description() {
        let text = "Description: 圧縮ライブラリ 😀\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("圧縮ライブラリ 😀")
        );
    }

    #[test]
    fn parse_pkgconfig_enormous_requires_line_no_panic() {
        let many_deps = (0..50_000)
            .map(|i| format!("dep{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!("Requires: {many_deps}\n");
        let manifest = parse(&text);
        assert_eq!(manifest.dependencies.len(), 50_000);
    }

    #[test]
    fn parse_pkgconfig_only_colons_no_panic() {
        let text = ":::::::::::::::::\n";
        let manifest = parse(text);
        assert_eq!(manifest, CppManifest::default());
    }

    #[test]
    fn parse_pkgconfig_key_with_no_value_no_panic() {
        let text = "Description:\nURL:\nRequires:\n";
        let manifest = parse(text);
        assert!(manifest.facts.description.is_none());
        assert!(manifest.facts.repository.is_none());
        assert!(manifest.dependencies.is_empty());
    }
}
