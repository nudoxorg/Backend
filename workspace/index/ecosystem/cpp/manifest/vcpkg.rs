//! Parser for `vcpkg.json` manifest files (REGISTRYLESS §8).
//!
//! Extracts description, SPDX license, homepage (→ repository), and
//! dependency records from the JSON package descriptor used by the vcpkg
//! C/C++ package manager.
//!
//! Uses `serde_json::Value` for parsing — never panics on malformed input;
//! returns [`super::CppManifest::default()`] on any parse failure.

use serde_json::Value;

use super::{CppManifest, DependencyMechanism, DependencyRecord};
use crate::ecosystem::manifest::ExtractedFacts;

/// Parse a `vcpkg.json` manifest.
///
/// Extracts:
/// - `description` — a string, or an array of strings joined with a single
///   space.
/// - `license` — SPDX expression string.
/// - `homepage` — stored in `facts.repository`.
/// - `dependencies` — each entry is either a bare name string or an object with
///   a `"name"` key; each becomes a [`DependencyRecord`] with mechanism
///   [`DependencyMechanism::Recipe`].
///
/// Any JSON parse error or missing field degrades gracefully: malformed input
/// returns [`CppManifest::default()`]; missing optional fields are `None` /
/// empty.
pub fn parse(text: &str) -> CppManifest {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return CppManifest::default(),
    };

    let description = extract_description(&value);
    let license = value["license"].as_str().map(str::to_owned);
    let repository = value["homepage"].as_str().map(str::to_owned);
    // vcpkg.json's own `documentation` key is a dedicated docs URL (distinct
    // from `homepage`, which usually IS the upstream repo) — a precise,
    // one-to-one signal, unlike ecosystems that fall back to "homepage is
    // present" as a documentation proxy.
    let documentation = value["documentation"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty());

    let mut manifest = CppManifest {
        facts: ExtractedFacts {
            description,
            repository,
            documentation,
            license,
            ..ExtractedFacts::default()
        },
        dependencies: Vec::new(),
    };

    if let Some(deps) = value["dependencies"].as_array() {
        for dep in deps {
            if let Some(record) = extract_dependency(dep) {
                manifest.push_dependency(record);
            }
        }
    }

    manifest
}

/// Extract the `description` field: either a string or an array of strings
/// joined with a single space. Returns `None` when the field is absent, null,
/// or an empty string / empty array.
fn extract_description(value: &Value) -> Option<String> {
    match &value["description"] {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Value::Array(parts) => {
            let joined: String = parts
                .iter()
                .filter_map(|p| p.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

/// A vcpkg dependency is a bare name or an object. `version>=` is the
/// constraint the object declared.
fn extract_dependency(entry: &Value) -> Option<DependencyRecord> {
    let (token, requirement) = match entry {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            (trimmed.to_owned(), None)
        }
        Value::Object(_) => {
            let name = entry["name"].as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let requirement = entry["version>="]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| format!(">={text}"));
            (name.to_owned(), requirement)
        }
        _ => return None,
    };
    let mut record = DependencyRecord::new(token, DependencyMechanism::Recipe);
    record.requirement = requirement;
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vcpkg_full_manifest() {
        let text = r#"{
            "name": "mylib",
            "version": "1.0.0",
            "description": "A useful C++ library",
            "license": "MIT",
            "homepage": "https://github.com/example/mylib",
            "dependencies": ["zlib", "openssl", {"name": "boost-filesystem", "version>=": "1.70.0"}]
        }"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("A useful C++ library")
        );
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://github.com/example/mylib")
        );
        assert_eq!(manifest.dependencies.len(), 3);
        assert_eq!(manifest.dependencies[0].token, "zlib");
        assert_eq!(manifest.dependencies[1].token, "openssl");
        assert_eq!(manifest.dependencies[2].token, "boost-filesystem");
        assert_eq!(
            manifest.dependencies[2].requirement.as_deref(),
            Some(">=1.70.0")
        );
        assert!(manifest.dependencies[0].requirement.is_none());
        assert!(
            manifest
                .dependencies
                .iter()
                .all(|d| d.mechanism == DependencyMechanism::Recipe)
        );
    }

    #[test]
    fn parse_vcpkg_description_as_array() {
        let text = r#"{"description": ["First sentence.", "Second sentence."]}"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("First sentence. Second sentence.")
        );
    }

    #[test]
    fn parse_vcpkg_empty_input() {
        let manifest = parse("");
        assert_eq!(manifest, CppManifest::default());
    }

    #[test]
    fn parse_vcpkg_malformed_json() {
        let manifest = parse("{not valid json");
        assert_eq!(manifest, CppManifest::default());
    }

    #[test]
    fn parse_vcpkg_null_optional_fields() {
        let text = r#"{"name": "mylib", "description": null, "license": null, "dependencies": []}"#;
        let manifest = parse(text);
        assert!(manifest.facts.description.is_none());
        assert!(manifest.facts.license.is_none());
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn parse_vcpkg_object_dependency_without_name_key_skipped() {
        // Object dep entries lacking a "name" key should be silently skipped.
        let text = r#"{"dependencies": [{"features": ["networking"]}]}"#;
        let manifest = parse(text);
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn parse_vcpkg_empty_string_description_yields_none() {
        let text = r#"{"description": "   "}"#;
        let manifest = parse(text);
        assert!(manifest.facts.description.is_none());
    }

    #[test]
    fn parse_vcpkg_dependencies_token_mirror_in_sync() {
        let text = r#"{"dependencies": ["zlib", {"name": "libpng"}]}"#;
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(
            crate::ecosystem::manifest::ManifestFacts::into_facts(manifest).dependency_names(),
            vec!["zlib".to_owned(), "libpng".to_owned()]
        );
    }

    // ── `documentation` field (P6 gap fill) ─────────────────────────────────

    #[test]
    fn parse_vcpkg_documentation_url_present() {
        let text = r#"{"name": "mylib", "documentation": "https://mylib.dev/docs"}"#;
        let manifest = parse(text);
        assert!(manifest.facts.documentation);
    }

    #[test]
    fn parse_vcpkg_documentation_absent_is_false() {
        let text = r#"{"name": "mylib", "homepage": "https://mylib.dev"}"#;
        let manifest = parse(text);
        // `homepage` alone is not treated as a documentation signal for
        // vcpkg — the manifest has its own dedicated `documentation` key.
        assert!(!manifest.facts.documentation);
    }

    #[test]
    fn parse_vcpkg_documentation_blank_string_is_false() {
        let text = r#"{"documentation": "   "}"#;
        let manifest = parse(text);
        assert!(!manifest.facts.documentation);
    }

    #[test]
    fn parse_vcpkg_full_manifest_real_world_shape() {
        // Shaped after a real vcpkg.json (fmt-style port).
        let text = r#"{
            "name": "fmt",
            "version": "10.2.1",
            "description": "A modern formatting library",
            "homepage": "https://github.com/fmtlib/fmt",
            "documentation": "https://fmt.dev",
            "license": "MIT",
            "dependencies": [
                {
                    "name": "vcpkg-cmake",
                    "host": true
                },
                {
                    "name": "vcpkg-cmake-config",
                    "host": true
                }
            ]
        }"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("A modern formatting library")
        );
        assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://github.com/fmtlib/fmt")
        );
        assert!(manifest.facts.documentation);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "vcpkg-cmake");
    }

    // ── Hostile-input hardening ──────────────────────────────────────────────

    #[test]
    fn parse_vcpkg_duplicate_keys_last_wins_no_panic() {
        // JSON with a duplicate top-level key: serde_json's Value map keeps
        // the last occurrence. Must not panic either way.
        let text = r#"{"description": "first", "description": "second"}"#;
        let manifest = parse(text);
        assert_eq!(manifest.facts.description.as_deref(), Some("second"));
    }

    #[test]
    fn parse_vcpkg_deeply_nested_description_value_no_panic() {
        // `description` is normally a string or array of strings; feeding it
        // a pathologically deep array nest must not panic or hang — it's
        // simply not a string/array-of-strings shape `extract_description`
        // recognises, so it degrades to `None`.
        let mut text = String::from(r#"{"description":"#);
        for _ in 0..20_000 {
            text.push('[');
        }
        for _ in 0..20_000 {
            text.push(']');
        }
        text.push('}');
        let manifest = parse(&text);
        assert!(manifest.facts.description.is_none());
    }

    #[test]
    fn parse_vcpkg_unicode_and_control_chars_in_description() {
        let text = r#"{"description": "日本語 emoji 😀 tab\tnewline\n"}"#;
        let manifest = parse(text);
        assert!(manifest.facts.description.is_some());
    }

    #[test]
    fn parse_vcpkg_enormous_description_string_no_panic() {
        let huge = "x".repeat(2 * 1024 * 1024);
        let text = format!(r#"{{"description": "{huge}"}}"#);
        let manifest = parse(&text);
        assert_eq!(
            manifest.facts.description.as_deref().map(str::len),
            Some(huge.len())
        );
    }

    #[test]
    fn parse_vcpkg_non_string_documentation_field_ignored() {
        let text = r#"{"documentation": 12345}"#;
        let manifest = parse(text);
        assert!(!manifest.facts.documentation);
    }
}
