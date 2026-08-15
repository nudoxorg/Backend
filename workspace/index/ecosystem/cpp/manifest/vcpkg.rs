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
/// - `description` — a string, or an array of strings joined with a single space.
/// - `license` — SPDX expression string.
/// - `homepage` — stored in `facts.repository`.
/// - `dependencies` — each entry is either a bare name string or an object
///   with a `"name"` key; each becomes a [`DependencyRecord`] with mechanism
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

    let mut manifest = CppManifest {
        facts: ExtractedFacts {
            description,
            repository,
            license,
            ..ExtractedFacts::default()
        },
        dependencies: Vec::new(),
    };

    if let Some(deps) = value["dependencies"].as_array() {
        for dep in deps {
            if let Some(token) = extract_dependency_token(dep) {
                manifest.push_dependency(DependencyRecord::new(token, DependencyMechanism::Recipe));
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

/// Extract a dependency token from a vcpkg dependency entry.
///
/// vcpkg allows two forms:
/// - A bare string: `"zlib"` → token `"zlib"`.
/// - An object with a `"name"` key: `{"name": "zlib", "features": [...]}` →
///   token `"zlib"`.
///
/// Returns `None` for null, numbers, and other unexpected shapes.
fn extract_dependency_token(entry: &Value) -> Option<String> {
    match entry {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Value::Object(_) => {
            let name = entry["name"].as_str()?;
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        _ => None,
    }
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
            "dependencies": ["zlib", "openssl", {"name": "boost-filesystem"}]
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
        // facts.dependencies must mirror the dependency token list.
        let text = r#"{"dependencies": ["zlib", {"name": "libpng"}]}"#;
        let manifest = parse(text);
        assert_eq!(manifest.facts.dependencies, vec!["zlib", "libpng"]);
        assert_eq!(manifest.dependencies.len(), 2);
    }
}
