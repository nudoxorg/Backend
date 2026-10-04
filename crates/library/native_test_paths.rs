//! Host-native spellings of absolute paths for test fixtures.
//!
//! Every owner-side admission in this crate requires an absolute path on the
//! machine that produced it: a recorded `cargo metadata` document is read on
//! the host that ran Cargo, and a workspace root is a real directory there.
//! A POSIX fixture root such as `/workspace` is not absolute on Windows (it
//! names a root on the current drive), so the production check correctly
//! refuses it. Fixtures therefore spell their roots through this module,
//! which keeps the production predicate untouched and the fixtures
//! meaningful on both platforms.

use std::path::PathBuf;

use serde_json::Value;

/// Keys of a Cargo metadata document whose string values are file-system
/// paths. Package identifiers, URLs and free text are never rewritten.
const CARGO_METADATA_PATH_KEYS: [&str; 6] = [
    "workspace_root",
    "manifest_path",
    "src_path",
    "target_directory",
    "build_directory",
    "path",
];

/// Spells a POSIX absolute path in the host's native absolute form.
///
/// `/workspace/app` stays as written on POSIX hosts and becomes
/// `C:/workspace/app` on Windows, which is the spelling `cargo metadata`
/// itself emits there.
pub fn absolute(posix: &str) -> String {
    debug_assert!(
        posix.starts_with('/') && !posix.starts_with("//"),
        "fixture roots are written as POSIX absolute paths"
    );
    if cfg!(windows) {
        format!("C:{posix}")
    } else {
        posix.to_owned()
    }
}

/// [`absolute`] as an owned path.
pub fn absolute_path(posix: &str) -> PathBuf {
    PathBuf::from(absolute(posix))
}

/// Rewrites the path-valued fields of a recorded Cargo metadata document into
/// the host's native absolute form. The document is returned byte for byte on
/// POSIX hosts.
pub fn localize_cargo_metadata(metadata: &[u8]) -> Vec<u8> {
    if !cfg!(windows) {
        return metadata.to_vec();
    }
    let mut document: Value = serde_json::from_slice(metadata).expect("recorded Cargo metadata");
    rewrite_paths(&mut document);
    serde_json::to_vec(&document).expect("re-encoded Cargo metadata")
}

fn rewrite_paths(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                match field {
                    Value::String(text)
                        if CARGO_METADATA_PATH_KEYS.contains(&key.as_str())
                            && text.starts_with('/')
                            && !text.starts_with("//") =>
                    {
                        *text = absolute(text);
                    }
                    other => rewrite_paths(other),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_paths),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_path_fields_are_rewritten() {
        let localized = localize_cargo_metadata(
            br#"{"workspace_root":"/ws","packages":[{"id":"path+file:///ws/app#app@1.0.0","description":"/ws is a word","manifest_path":"/ws/Cargo.toml","targets":[{"src_path":"/ws/src/lib.rs"}]}]}"#,
        );
        let document: Value = serde_json::from_slice(&localized).expect("localized document");
        assert_eq!(document["workspace_root"], absolute("/ws"));
        let package = &document["packages"][0];
        assert_eq!(package["manifest_path"], absolute("/ws/Cargo.toml"));
        assert_eq!(
            package["targets"][0]["src_path"],
            absolute("/ws/src/lib.rs")
        );
        assert_eq!(package["id"], "path+file:///ws/app#app@1.0.0");
        assert_eq!(package["description"], "/ws is a word");
    }

    #[test]
    fn the_native_spelling_is_absolute_on_this_host() {
        assert!(absolute_path("/workspace/app").is_absolute());
    }
}
