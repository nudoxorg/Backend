//! Readers that turn Cargo's own answers into a [`TreeInput`].
//!
//! `cargo metadata --filter-platform <host>` is the authority for the current
//! target and selected feature resolution, and carries each package's license,
//! description, categories and keywords. `Cargo.lock` supplies rows not in
//! that resolution; it cannot say whether each row is inactive for this target,
//! disabled by feature selection, or both. It also stands in, reduced, when
//! Cargo cannot answer.

use super::tree::{
    LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership, PackageOrigin,
    TreeEdge, TreeInput, TreeInputPackage, TreeSource,
};
use crate::{
    CargoPackageSourceAuthorityFailureV1 as SourceAuthorityFailure,
    CargoPackageSourceAuthorityStateV1 as SourceAuthorityState,
    CargoPackageSourceAuthorityV1 as SourceAuthority,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

/// Largest Cargo metadata response accepted by the pure parser.
pub const MAX_CARGO_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Largest Cargo.lock document admitted by the lockfile-only parser.
pub const MAX_CARGO_LOCKFILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CARGO_METADATA_STRING_BYTES: usize = 4 * 1024;
const MAX_CARGO_METADATA_STRING_ARRAY: usize = 8 * 1024;
const MAX_CARGO_TARGETS_PER_PACKAGE: usize = 256;
const MAX_CARGO_RESOLVE_EDGES: usize = 1_000_000;
const MAX_CARGO_RESOLVED_FEATURES: usize = 262_144;
const MAX_CARGO_RESOLVED_FEATURE_BYTES: usize = 16 * 1024 * 1024;
const MAX_LOCKED_PACKAGES: usize = 20_000;
const MAX_LOCKED_DEPENDENCIES_PER_PACKAGE: usize = 16_384;

/// Why a reader could not produce a tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoTreeError {
    /// The metadata was not the JSON Cargo writes.
    Metadata(String),
    /// The lockfile was not TOML Cargo writes.
    Lockfile(String),
}

impl std::fmt::Display for CargoTreeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Metadata(detail) => write!(formatter, "cargo metadata is unreadable: {detail}"),
            Self::Lockfile(detail) => write!(formatter, "Cargo.lock is unreadable: {detail}"),
        }
    }
}

impl std::error::Error for CargoTreeError {}

fn required_string(value: &Value, key: &str, context: &str) -> Result<String, CargoTreeError> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| text.len() <= MAX_CARGO_METADATA_STRING_BYTES)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CargoTreeError::Metadata(format!("{context} has no string {key}")))?;
    if text.is_empty() {
        return Err(CargoTreeError::Metadata(format!(
            "{context} has an empty {key}"
        )));
    }
    Ok(text)
}

fn optional_string(
    value: &Value,
    key: &str,
    context: &str,
) -> Result<Option<String>, CargoTreeError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= MAX_CARGO_METADATA_STRING_BYTES => {
            Ok(Some(text.clone()))
        }
        Some(Value::String(_)) => Err(CargoTreeError::Metadata(format!(
            "{context} has an overlong {key}"
        ))),
        Some(_) => Err(CargoTreeError::Metadata(format!(
            "{context} has a non-string {key}"
        ))),
    }
}

fn required_array<'a>(
    value: &'a Value,
    key: &str,
    context: &str,
) -> Result<&'a [Value], CargoTreeError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| CargoTreeError::Metadata(format!("{context} has no array {key}")))
}

fn required_string_array(
    value: &Value,
    key: &str,
    context: &str,
) -> Result<Vec<String>, CargoTreeError> {
    let values = required_array(value, key, context)?;
    if values.len() > MAX_CARGO_METADATA_STRING_ARRAY {
        return Err(CargoTreeError::Metadata(format!(
            "{context} has too many {key} entries"
        )));
    }
    values
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|text| text.len() <= MAX_CARGO_METADATA_STRING_BYTES)
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    CargoTreeError::Metadata(format!(
                        "{context} has a non-string or overlong {key} entry"
                    ))
                })
        })
        .collect()
}

fn optional_string_array(
    value: &Value,
    key: &str,
    context: &str,
) -> Result<Vec<String>, CargoTreeError> {
    match value.get(key) {
        None => Ok(Vec::new()),
        Some(_) => required_string_array(value, key, context),
    }
}

/// Retain Cargo's source spelling so a later reader never guesses a registry
/// from a display name. An unfamiliar source stays explicitly unresolved.
fn source_origin(source: &str) -> PackageOrigin {
    if source.starts_with("git+") {
        PackageOrigin::Git {
            source: source.to_owned(),
        }
    } else if source.starts_with("registry+") || source.starts_with("sparse+") {
        PackageOrigin::Registry {
            source: source.to_owned(),
        }
    } else {
        PackageOrigin::Unresolved {
            source: Some(source.to_owned()),
        }
    }
}

fn package_source(package: &TreeInputPackage) -> Option<&str> {
    match package.origin.as_ref()? {
        PackageOrigin::Registry { source } | PackageOrigin::Git { source } => Some(source),
        PackageOrigin::Unresolved { source } => source.as_deref(),
        PackageOrigin::Vendored { .. } => None,
    }
}

// Cargo.lock omits paths. A source-less name/version tuple is paired only
// one-to-one; duplicate or mismatched groups remain explicitly Partial.
fn package_identity(package: &TreeInputPackage) -> (String, String, Option<String>) {
    (
        package.name.clone(),
        package.version.clone(),
        package_source(package).map(ToOwned::to_owned),
    )
}

/// Hashes Cargo's exact metadata and lock response together with target and
/// workspace identity. File bytes below package roots remain a separate read
/// observation.
fn metadata_observation_revision(
    metadata: &[u8],
    lockfile: Option<&str>,
    target: &str,
    workspace_root: &str,
    stable_input_witness: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-metadata-observation.v2\0");
    for value in [workspace_root.as_bytes(), target.as_bytes()] {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    hasher.update(&(metadata.len() as u64).to_le_bytes());
    hasher.update(metadata);
    if let Some(lockfile) = lockfile {
        hasher.update(&[1]);
        hasher.update(&(lockfile.len() as u64).to_le_bytes());
        hasher.update(lockfile.as_bytes());
    } else {
        hasher.update(&[0]);
    }
    match stable_input_witness {
        Some(witness) if witness != [0; 32] => {
            hasher.update(&[1]);
            hasher.update(&witness);
        }
        _ => hasher.update(&[0]),
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn no_advisories(_: &str, _: &str) -> backend_advisory::AdvisoryObservation {
        let authority = backend_advisory::AdvisoryAuthority::new(0);
        let package =
            backend_advisory::normalize_package("cargo", "none").expect("test advisory identity");
        authority.observe(&package, "0.0.0", false, false, 0, false)
    }

    fn minimal_metadata() -> Value {
        serde_json::json!({
            "workspace_root": "/workspace",
            "workspace_members": ["path+file:///workspace/app#app@0.1.0"],
            "packages": [{
                "id": "path+file:///workspace/app#app@0.1.0",
                "name": "app",
                "version": "0.1.0",
                "source": null,
                "manifest_path": "/workspace/Cargo.toml",
                "targets": []
            }],
            "resolve": { "nodes": [{
                "id": "path+file:///workspace/app#app@0.1.0",
                "deps": []
            }] }
        })
    }

    #[test]
    fn alternative_and_unrecognized_sources_keep_their_observed_authority() {
        let first = source_origin("registry+https://one.example.test/index");
        let second = source_origin("registry+https://two.example.test/index");
        assert_ne!(first, second);
        assert!(!first.is_crates_io_registry());
        assert_eq!(
            first,
            PackageOrigin::Registry {
                source: "registry+https://one.example.test/index".to_owned()
            }
        );
        assert_eq!(
            source_origin("other+opaque"),
            PackageOrigin::Unresolved {
                source: Some("other+opaque".to_owned())
            }
        );
    }

    #[test]
    fn git_source_identity_keeps_reference_and_resolved_commit() {
        let first_source = "git+https://git.example.test/team/widget?branch=stable#1111111111111111111111111111111111111111";
        let second_source = "git+https://git.example.test/team/widget?branch=stable#2222222222222222222222222222222222222222";
        let first = source_origin(first_source);
        let second = source_origin(second_source);
        assert_ne!(
            first, second,
            "one repository URL can resolve to distinct package commits"
        );
        assert_eq!(
            first,
            PackageOrigin::Git {
                source: first_source.to_owned()
            }
        );
        assert_eq!(
            second,
            PackageOrigin::Git {
                source: second_source.to_owned()
            }
        );
    }

    #[test]
    fn metadata_rejects_dangling_or_malformed_resolve_edges() {
        let mut malformed = minimal_metadata();
        malformed["resolve"]["nodes"][0]["deps"] = serde_json::json!([{
            "dep_kinds": [{ "kind": null }]
        }]);
        assert!(matches!(
            metadata_input(
                &serde_json::to_vec(&malformed).expect("metadata JSON"),
                "host",
                None,
            ),
            Err(CargoTreeError::Metadata(message)) if message.contains("no string pkg")
        ));

        let mut dangling = minimal_metadata();
        dangling["resolve"]["nodes"][0]["deps"] = serde_json::json!([{
            "pkg": "registry+https://example.test/index#missing@1.0.0",
            "dep_kinds": [{ "kind": null }]
        }]);
        assert!(matches!(
            metadata_input(
                &serde_json::to_vec(&dangling).expect("metadata JSON"),
                "host",
                None,
            ),
            Err(CargoTreeError::Metadata(message)) if message.contains("no matching package")
        ));

        let mut unknown_kind = minimal_metadata();
        unknown_kind["packages"]
            .as_array_mut()
            .expect("packages array")
            .push(serde_json::json!({
                "id": "registry+https://example.test/index#dep@1.0.0",
                "name": "dep",
                "version": "1.0.0",
                "source": "registry+https://example.test/index",
                "manifest_path": "/cargo/dep/Cargo.toml",
                "targets": []
            }));
        unknown_kind["resolve"]["nodes"]
            .as_array_mut()
            .expect("resolve nodes array")
            .push(serde_json::json!({
                "id": "registry+https://example.test/index#dep@1.0.0",
                "deps": []
            }));
        unknown_kind["resolve"]["nodes"][0]["deps"] = serde_json::json!([{
            "pkg": "registry+https://example.test/index#dep@1.0.0",
            "dep_kinds": [{ "kind": "runtime" }]
        }]);
        assert!(matches!(
            metadata_input(
                &serde_json::to_vec(&unknown_kind).expect("metadata JSON"),
                "host",
                None,
            ),
            Err(CargoTreeError::Metadata(message)) if message.contains("unknown kind")
        ));
    }

    #[test]
    fn metadata_rejects_duplicate_package_and_resolve_identities() {
        let mut duplicate_package = minimal_metadata();
        let package = duplicate_package["packages"][0].clone();
        duplicate_package["packages"]
            .as_array_mut()
            .expect("packages array")
            .push(package);
        assert!(matches!(
            metadata_input(
                &serde_json::to_vec(&duplicate_package).expect("metadata JSON"),
                "host",
                None,
            ),
            Err(CargoTreeError::Metadata(message)) if message.contains("duplicate package id")
        ));

        let mut duplicate_node = minimal_metadata();
        let node = duplicate_node["resolve"]["nodes"][0].clone();
        duplicate_node["resolve"]["nodes"]
            .as_array_mut()
            .expect("resolve nodes array")
            .push(node);
        assert!(matches!(
            metadata_input(
                &serde_json::to_vec(&duplicate_node).expect("metadata JSON"),
                "host",
                None,
            ),
            Err(CargoTreeError::Metadata(message)) if message.contains("duplicate resolve node")
        ));
    }

    #[test]
    fn lockfile_graph_keeps_same_git_url_releases_separate_by_commit() {
        let first_source = "git+https://git.example.test/team/widget?branch=stable#1111111111111111111111111111111111111111";
        let second_source = "git+https://git.example.test/team/widget?branch=stable#2222222222222222222222222222222222222222";
        let lockfile = format!(
            r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = [
  "widget 1.0.0 ({first_source})",
  "widget 1.0.0 ({second_source})",
]
[[package]]
name = "widget"
version = "1.0.0"
source = "{first_source}"
[[package]]
name = "widget"
version = "1.0.0"
source = "{second_source}"
"#
        );
        let input = lockfile_input(
            &lockfile,
            "/workspace",
            &BTreeSet::new(),
            "metadata unavailable",
        )
        .expect("Cargo lockfile");
        let widget_origins = input
            .packages
            .iter()
            .filter(|package| package.name == "widget")
            .map(|package| package.origin.clone().expect("Git source"))
            .collect::<Vec<_>>();
        assert_eq!(widget_origins.len(), 2);
        assert!(widget_origins.contains(&PackageOrigin::Git {
            source: first_source.to_owned(),
        }));
        assert!(widget_origins.contains(&PackageOrigin::Git {
            source: second_source.to_owned(),
        }));
        assert_eq!(input.edges.len(), 2);
        assert_ne!(input.edges[0].to, input.edges[1].to);
        assert!(matches!(
            input.source,
            TreeSource::Lockfile {
                coverage: LockfileGraphCoverage::Complete,
                ..
            }
        ));
    }

    #[test]
    fn cargo_generated_lockfile_unqualified_edge_uses_unique_registry_authority() {
        // Cargo writes the ordinary crates.io edge as just `"serde"` here,
        // while the unique package row records the source authority. Resolving
        // the edge must carry that exact row rather than assume a registry.
        let lockfile = include_str!("fixtures/tree-2026-09-27/Cargo.lock");
        let input = lockfile_input(
            lockfile,
            "/workspace",
            &BTreeSet::new(),
            "metadata unavailable",
        )
        .expect("Cargo-generated lockfile");
        assert!(input.packages.iter().all(|package| !package.member));
        let local_patch = input
            .packages
            .iter()
            .find(|package| package.name == "gpui-ce")
            .expect("source-less patched path row from real Cargo.lock");
        assert_eq!(
            local_patch.origin,
            Some(PackageOrigin::Unresolved { source: None }),
            "the real lockfile row carries neither workspace membership nor its patch path"
        );
        let bincode = input
            .packages
            .iter()
            .find(|package| package.name == "bincode" && package.version == "1.3.3")
            .expect("bincode package");
        let serde = input
            .packages
            .iter()
            .find(|package| package.name == "serde" && package.version == "1.0.229")
            .expect("serde package");
        assert!(matches!(
            serde.origin.as_ref(),
            Some(PackageOrigin::Registry { source })
                if source == "registry+https://github.com/rust-lang/crates.io-index"
        ));
        assert!(
            input
                .edges
                .iter()
                .any(|edge| edge.from == bincode.id && edge.to == serde.id)
        );
        assert!(matches!(
            input.source,
            TreeSource::Lockfile {
                coverage: LockfileGraphCoverage::Complete,
                ..
            }
        ));
    }

    #[test]
    fn lockfile_does_not_claim_source_less_local_paths_are_workspace_members() {
        let lockfile = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = ["local-helper 0.1.0"]
[[package]]
name = "local-helper"
version = "0.1.0"
"#;
        let input = lockfile_input(
            lockfile,
            "/workspace",
            &BTreeSet::from(["local-helper".to_owned()]),
            "metadata unavailable",
        )
        .expect("lockfile-only graph");
        assert!(input.packages.iter().all(|package| !package.member));
        let local = input
            .packages
            .iter()
            .find(|package| package.name == "local-helper")
            .expect("source-less local path row");
        assert_eq!(
            local.origin,
            Some(PackageOrigin::Unresolved { source: None }),
            "a source-less lock row proves neither member status nor local path"
        );
        assert!(matches!(
            input.source,
            TreeSource::Lockfile {
                workspace_membership: LockfileWorkspaceMembership::Unknown,
                ..
            }
        ));
        let tree = super::super::build_tree(&input, &no_advisories);
        assert!(tree.members.is_empty());
        assert_eq!(tree.packages.len(), 2);
        assert!(tree.packages.iter().all(|package| {
            package.why.is_empty() && package.role == super::super::PackageRole::Unknown
        }));
    }

    #[test]
    fn malformed_lockfile_source_is_not_treated_as_a_path() {
        let malformed_source = r#"
version = 4
[[package]]
name = "local-helper"
version = "0.1.0"
source = 7
"#;
        assert!(matches!(
            lockfile_input(malformed_source, "/workspace", &BTreeSet::new(), "unavailable"),
            Err(CargoTreeError::Lockfile(message)) if message.contains("source is not a string")
        ));
    }

    #[test]
    fn indistinguishable_source_less_rows_keep_package_identity_partial() {
        let duplicate_path_identity = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = ["local-helper 0.1.0"]
[[package]]
name = "local-helper"
version = "0.1.0"
[[package]]
name = "local-helper"
version = "0.1.0"
"#;
        let input = lockfile_input(
            duplicate_path_identity,
            "/workspace",
            &BTreeSet::new(),
            "unavailable",
        )
        .expect("ambiguous but readable lockfile");
        assert_eq!(input.packages.len(), 3);
        assert_ne!(input.packages[1].id, input.packages[2].id);
        assert!(input.edges.is_empty(), "the edge cannot choose a path row");
        assert_eq!(
            input.source,
            TreeSource::Lockfile {
                reason: "unavailable".to_owned(),
                coverage: LockfileGraphCoverage::Partial {
                    ambiguous_edges: 1,
                    ambiguous_package_rows: 2,
                },
                workspace_membership: LockfileWorkspaceMembership::Unknown,
            }
        );
    }

    #[test]
    fn lockfile_unqualified_or_bad_source_edges_remain_partial_without_unique_matches() {
        let lockfile = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = [
  "widget 1.0.0 (registry+https://one.example.test/index)",
  "widget 1.0.0 (registry+https://two.example.test/index)",
  "missing 1.0.0",
  "widget 1.0.0 registry+https://one.example.test/index",
]
[[package]]
name = "widget"
version = "1.0.0"
source = "registry+https://one.example.test/index"
"#;
        let input = lockfile_input(
            lockfile,
            "/workspace",
            &BTreeSet::new(),
            "metadata unavailable",
        )
        .expect("synthetic lockfile");
        let widget = input
            .packages
            .iter()
            .find(|package| package.name == "widget")
            .expect("widget row");
        assert_eq!(input.edges.len(), 1);
        assert_eq!(input.edges[0].to, widget.id);
        assert_eq!(
            input.source,
            TreeSource::Lockfile {
                reason: "metadata unavailable".to_owned(),
                coverage: LockfileGraphCoverage::Partial {
                    ambiguous_edges: 3,
                    ambiguous_package_rows: 0,
                },
                workspace_membership: LockfileWorkspaceMembership::Unknown,
            }
        );
    }

    #[test]
    fn metadata_and_tree_keep_git_commit_identity_for_the_same_url_release() {
        let first_source = "git+https://git.example.test/team/widget?branch=stable#1111111111111111111111111111111111111111";
        let second_source = "git+https://git.example.test/team/widget?branch=stable#2222222222222222222222222222222222222222";
        let root_id = "path+file:///workspace/app#app@0.1.0";
        let first_id = format!("{first_source}#widget@1.0.0");
        let second_id = format!("{second_source}#widget@1.0.0");
        let metadata = serde_json::json!({
            "workspace_root": "/workspace",
            "workspace_members": [root_id],
            "packages": [
                {
                    "id": root_id,
                    "name": "app",
                    "version": "0.1.0",
                    "source": null,
                    "manifest_path": "/workspace/Cargo.toml",
                    "targets": []
                },
                {
                    "id": first_id,
                    "name": "widget",
                    "version": "1.0.0",
                    "source": first_source,
                    "manifest_path": "/cargo/git/first/Cargo.toml",
                    "targets": []
                },
                {
                    "id": second_id,
                    "name": "widget",
                    "version": "1.0.0",
                    "source": second_source,
                    "manifest_path": "/cargo/git/second/Cargo.toml",
                    "targets": []
                }
            ],
            "resolve": {
                "nodes": [
                    { "id": root_id, "deps": [
                        { "pkg": first_id, "dep_kinds": [{ "kind": null }] },
                        { "pkg": second_id, "dep_kinds": [{ "kind": null }] }
                    ] },
                    { "id": first_id, "deps": [] },
                    { "id": second_id, "deps": [] }
                ]
            }
        });
        let input = metadata_input(
            &serde_json::to_vec(&metadata).expect("metadata"),
            "host",
            None,
        )
        .expect("metadata");
        let widget_origins = input
            .packages
            .iter()
            .filter(|package| package.name == "widget")
            .map(|package| package.origin.clone().expect("git source"))
            .collect::<Vec<_>>();
        assert_eq!(widget_origins.len(), 2);
        assert!(widget_origins.contains(&PackageOrigin::Git {
            source: first_source.to_owned(),
        }));
        assert!(widget_origins.contains(&PackageOrigin::Git {
            source: second_source.to_owned(),
        }));

        let no_advisories = |_: &str, _: &str| {
            let authority = backend_advisory::AdvisoryAuthority::new(0);
            let package = backend_advisory::normalize_package("cargo", "none")
                .expect("test advisory identity");
            authority.observe(&package, "0.0.0", false, false, 0, false)
        };
        let tree = super::super::build_tree(&input, &no_advisories);
        assert_eq!(
            tree.packages
                .iter()
                .filter(|package| package.name == "widget")
                .count(),
            2
        );
        assert!(tree.package("widget", "1.0.0").is_none());
    }

    #[test]
    fn locked_inactive_count_keeps_registry_packages_distinct_from_members_and_paths() {
        let metadata = serde_json::json!({
            "workspace_root": "/workspace",
            "workspace_members": [
                "path+file:///workspace/app#app@0.1.0",
                "path+file:///workspace/member#shared-member@1.0.0"
            ],
            "packages": [
                {
                    "id": "path+file:///workspace/app#app@0.1.0",
                    "name": "app",
                    "version": "0.1.0",
                    "source": null,
                    "manifest_path": "/workspace/Cargo.toml",
                    "targets": []
                },
                {
                    "id": "path+file:///workspace/member#shared-member@1.0.0",
                    "name": "shared-member",
                    "version": "1.0.0",
                    "source": null,
                    "manifest_path": "/workspace/member/Cargo.toml",
                    "targets": []
                },
                {
                    "id": "path+file:///vendor/shared-path#shared-path@1.0.0",
                    "name": "shared-path",
                    "version": "1.0.0",
                    "source": null,
                    "manifest_path": "/vendor/shared-path/Cargo.toml",
                    "targets": []
                }
            ],
            "resolve": {
                "nodes": [
                    {
                        "id": "path+file:///workspace/app#app@0.1.0",
                        "deps": [{
                            "pkg": "path+file:///vendor/shared-path#shared-path@1.0.0",
                            "dep_kinds": [{ "kind": null }]
                        }]
                    },
                    { "id": "path+file:///workspace/member#shared-member@1.0.0", "deps": [] },
                    { "id": "path+file:///vendor/shared-path#shared-path@1.0.0", "deps": [] }
                ]
            }
        });
        let lockfile = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
[[package]]
name = "shared-member"
version = "1.0.0"
[[package]]
name = "shared-member"
version = "1.0.0"
source = "registry+https://registry.example.test/index"
[[package]]
name = "shared-path"
version = "1.0.0"
source = "registry+https://registry.example.test/index"
"#;

        let input = metadata_input(
            &serde_json::to_vec(&metadata).expect("metadata"),
            "host",
            Some(lockfile),
        )
        .expect("metadata and lockfile");
        assert_eq!(
            input.locked_inactive, 2,
            "a workspace member or path dependency with the same display key must not hide a different registry package"
        );
        assert_eq!(
            input.locked_inactive_coverage,
            LockedInactiveCoverage::Partial {
                unmatched_packages: 1,
            },
            "only the active path row absent from Cargo.lock remains unpaired"
        );
    }

    #[test]
    fn locked_inactive_distinguish_registry_authority_at_the_same_name_and_version() {
        let metadata = serde_json::json!({
            "workspace_root": "/workspace",
            "workspace_members": ["path+file:///workspace/app#app@0.1.0"],
            "packages": [
                {
                    "id": "path+file:///workspace/app#app@0.1.0",
                    "name": "app",
                    "version": "0.1.0",
                    "source": null,
                    "manifest_path": "/workspace/Cargo.toml",
                    "targets": []
                },
                {
                    "id": "registry+https://one.example.test/index#widget@1.0.0",
                    "name": "widget",
                    "version": "1.0.0",
                    "source": "registry+https://one.example.test/index",
                    "manifest_path": "/registry/widget/Cargo.toml",
                    "targets": []
                }
            ],
            "resolve": {
                "nodes": [
                    {
                        "id": "path+file:///workspace/app#app@0.1.0",
                        "deps": [{
                            "pkg": "registry+https://one.example.test/index#widget@1.0.0",
                            "dep_kinds": [{ "kind": null }]
                        }]
                    },
                    { "id": "registry+https://one.example.test/index#widget@1.0.0", "deps": [] }
                ]
            }
        });
        let lockfile = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
[[package]]
name = "widget"
version = "1.0.0"
source = "registry+https://two.example.test/index"
"#;

        let input = metadata_input(
            &serde_json::to_vec(&metadata).expect("metadata"),
            "host",
            Some(lockfile),
        )
        .expect("metadata and lockfile");
        assert_eq!(input.locked_inactive, 1);
        assert_eq!(
            input.locked_inactive_coverage,
            LockedInactiveCoverage::Partial {
                unmatched_packages: 1,
            }
        );
    }

    #[test]
    fn filtered_resolve_graph_excludes_unreachable_packages_from_inactive_count() {
        let mut darwin: Value =
            serde_json::from_str(include_str!("fixtures/filter-platform-targets/darwin.json"))
                .expect("captured Darwin metadata");
        let windows: Value = serde_json::from_str(include_str!(
            "fixtures/filter-platform-targets/windows.json"
        ))
        .expect("captured Windows metadata");
        let windows_only = windows["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .find(|package| package["name"] == "windows-only-proof")
            .expect("Windows-only path dependency")
            .clone();

        // The captured Cargo 1.98 response omits the Windows-only package row
        // on Darwin. Graft its real Windows row into the Darwin package list
        // to cover Cargo versions that retain manifest rows even though
        // --filter-platform only promises to filter `resolve`.
        darwin["packages"]
            .as_array_mut()
            .expect("packages")
            .push(windows_only);
        let input = metadata_input(
            &serde_json::to_vec(&darwin).expect("metadata JSON"),
            "x86_64-apple-darwin",
            Some(include_str!("fixtures/filter-platform-targets/Cargo.lock")),
        )
        .expect("filtered metadata and lockfile");

        assert!(
            input
                .packages
                .iter()
                .any(|package| package.name == "common-proof")
        );
        assert!(
            !input
                .packages
                .iter()
                .any(|package| package.name == "windows-only-proof")
        );
        assert!(
            !input
                .edges
                .iter()
                .any(|edge| edge.to.contains("windows-only-proof"))
        );
        assert_eq!(input.locked_inactive, 1);
        assert_eq!(
            input.locked_inactive_coverage,
            LockedInactiveCoverage::Complete,
            "unique source-less identities pair one-to-one with Cargo.lock; this row is locked but inactive under the current target/features resolution"
        );

        let actual_darwin: Value =
            serde_json::from_str(include_str!("fixtures/filter-platform-targets/darwin.json"))
                .expect("captured Darwin metadata");
        let actual_windows: Value = serde_json::from_str(include_str!(
            "fixtures/filter-platform-targets/windows.json"
        ))
        .expect("captured Windows metadata");
        assert!(
            !actual_darwin["packages"]
                .as_array()
                .expect("Darwin packages")
                .iter()
                .any(|package| package["name"] == "windows-only-proof")
        );
        assert!(
            actual_darwin["packages"]
                .as_array()
                .expect("Darwin packages")
                .iter()
                .find(|package| package["name"] == "filter-platform-proof-app")
                .expect("app")["dependencies"]
                .as_array()
                .expect("manifest dependencies")
                .iter()
                .any(|dependency| dependency["target"] == "cfg(windows)")
        );
        assert!(
            actual_windows["resolve"]["nodes"]
                .as_array()
                .expect("Windows resolve nodes")
                .iter()
                .flat_map(|node| node["deps"].as_array().into_iter().flatten())
                .any(|dependency| dependency["pkg"]
                    .as_str()
                    .is_some_and(|id| id.contains("windows-only-proof")))
        );
    }

    #[test]
    fn pinned_cargo_197_metadata_uses_resolve_reachability_for_target_inventory() {
        // These real Cargo 1.97.1 captures come from the adjacent offline
        // fixture. The lockfile contains the target-specific and disabled
        // optional path rows, while the filtered resolve graph only includes
        // dependencies active for each request.
        let lockfile = include_str!("fixtures/filter-platform-cargo-1.97/project/Cargo.lock");
        let linux = metadata_input(
            include_bytes!("fixtures/filter-platform-cargo-1.97/linux.json"),
            "x86_64-unknown-linux-gnu",
            Some(lockfile),
        )
        .expect("Cargo 1.97.1 Linux metadata");
        let linux_names = linux
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            linux_names,
            BTreeSet::from(["common-proof", "filter-platform-proof-app"])
        );
        assert_eq!(linux.locked_inactive, 2);
        assert_eq!(
            linux.locked_inactive_coverage,
            LockedInactiveCoverage::Complete
        );

        let windows = metadata_input(
            include_bytes!("fixtures/filter-platform-cargo-1.97/windows.json"),
            "x86_64-pc-windows-msvc",
            Some(lockfile),
        )
        .expect("Cargo 1.97.1 Windows metadata");
        let windows_names = windows
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            windows_names,
            BTreeSet::from([
                "common-proof",
                "filter-platform-proof-app",
                "windows-only-proof"
            ])
        );
        assert_eq!(windows.locked_inactive, 1);
        assert_eq!(
            windows.locked_inactive_coverage,
            LockedInactiveCoverage::Complete
        );
    }

    #[test]
    fn lockfile_edges_keep_source_ids_and_refuse_ambiguous_name_versions() {
        let lock = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = [
  "widget 1.0.0 (registry+https://one.example.test/index)",
  "widget 1.0.0 (registry+https://two.example.test/index)",
]
[[package]]
name = "widget"
version = "1.0.0"
source = "registry+https://one.example.test/index"
[[package]]
name = "widget"
version = "1.0.0"
source = "registry+https://two.example.test/index"
"#;
        let input = lockfile_input(
            lock,
            "/workspace/app",
            &BTreeSet::new(),
            "Cargo unavailable",
        )
        .expect("lockfile");
        assert_ne!(input.packages[1].id, input.packages[2].id);
        assert_eq!(input.edges.len(), 2);
        assert_eq!(
            input.source,
            TreeSource::Lockfile {
                reason: "Cargo unavailable".to_owned(),
                coverage: LockfileGraphCoverage::Complete,
                workspace_membership: LockfileWorkspaceMembership::Unknown,
            }
        );
        assert_ne!(input.edges[0].to, input.edges[1].to);

        let ambiguous = lock.replace(
            "  \"widget 1.0.0 (registry+https://one.example.test/index)\",\n  \"widget 1.0.0 (registry+https://two.example.test/index)\",",
            "  \"widget 1.0.0\",",
        );
        let input = lockfile_input(
            &ambiguous,
            "/workspace/app",
            &BTreeSet::new(),
            "Cargo unavailable",
        )
        .expect("ambiguous lockfile");
        assert!(
            input.edges.is_empty(),
            "a name/version-only edge cannot choose a registry"
        );
        assert_eq!(
            input.source,
            TreeSource::Lockfile {
                reason: "Cargo unavailable".to_owned(),
                coverage: LockfileGraphCoverage::Partial {
                    ambiguous_edges: 1,
                    ambiguous_package_rows: 0,
                },
                workspace_membership: LockfileWorkspaceMembership::Unknown,
            },
            "an omitted ambiguous edge remains explicitly partial"
        );
    }
}

/// Reads `cargo metadata --format-version 1 --filter-platform <host>`.
///
/// The lockfile supplies package rows outside the current target/features
/// resolve graph. The difference does not identify why those rows are
/// inactive. Rows that cannot be paired one-to-one by exact Cargo source
/// identity remain an explicit partial lower bound.
///
/// # Errors
///
/// Returns [`CargoTreeError`] when either document is not in Cargo's format.
pub fn metadata_input(
    metadata: &[u8],
    host: &str,
    lockfile: Option<&str>,
) -> Result<TreeInput, CargoTreeError> {
    metadata_input_observed(metadata, host, lockfile, None)
}

/// Parses one Cargo metadata result whose local manifest/configuration input
/// set was observed unchanged immediately before and after the Cargo run.
/// Only this owner-side entry point emits actionable source authority; the
/// ordinary byte parser intentionally leaves it unavailable.
pub fn metadata_input_with_stable_source_witness(
    metadata: &[u8],
    host: &str,
    lockfile: Option<&str>,
    stable_input_witness: [u8; 32],
) -> Result<TreeInput, CargoTreeError> {
    if stable_input_witness == [0; 32] {
        return Err(CargoTreeError::Metadata(
            "Cargo source input witness is empty".to_owned(),
        ));
    }
    metadata_input_observed(metadata, host, lockfile, Some(stable_input_witness))
}

fn metadata_input_observed(
    metadata: &[u8],
    host: &str,
    lockfile: Option<&str>,
    stable_input_witness: Option<[u8; 32]>,
) -> Result<TreeInput, CargoTreeError> {
    if metadata.len() > MAX_CARGO_METADATA_BYTES {
        return Err(CargoTreeError::Metadata(
            "metadata exceeds the bounded input size".to_owned(),
        ));
    }
    let root: Value = serde_json::from_slice(metadata)
        .map_err(|error| CargoTreeError::Metadata(error.to_string()))?;
    if host.is_empty() || host.len() > MAX_CARGO_METADATA_STRING_BYTES {
        return Err(CargoTreeError::Metadata(
            "Cargo effective target is empty or exceeds its limit".to_owned(),
        ));
    }
    let workspace_root = required_string(&root, "workspace_root", "metadata")?;
    if !admitted_metadata_path(&workspace_root) {
        return Err(CargoTreeError::Metadata(
            "workspace_root is not a canonical absolute path".to_owned(),
        ));
    }
    let source_revision = stable_input_witness.map(|witness| {
        metadata_observation_revision(metadata, lockfile, host, &workspace_root, Some(witness))
    });
    let raw_members = required_array(&root, "workspace_members", "metadata")?;
    if raw_members.len() > super::tree::MAX_TREE_PACKAGES {
        return Err(CargoTreeError::Metadata(
            "workspace_members exceeds the package limit".to_owned(),
        ));
    }
    let members = required_string_array(&root, "workspace_members", "metadata")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if members.len() != raw_members.len() {
        return Err(CargoTreeError::Metadata(
            "workspace_members contains duplicate package ids".to_owned(),
        ));
    }
    let resolved_nodes = required_array(
        root.get("resolve")
            .ok_or_else(|| CargoTreeError::Metadata("no resolve graph".to_owned()))?,
        "nodes",
        "resolve graph",
    )?;
    if resolved_nodes.len() > super::tree::MAX_TREE_PACKAGES {
        return Err(CargoTreeError::Metadata(
            "resolve.nodes exceeds the package limit".to_owned(),
        ));
    }
    let mut resolved_features = BTreeMap::<String, Vec<String>>::new();
    let mut total_features = 0_usize;
    let mut total_feature_bytes = 0_usize;
    for node in resolved_nodes {
        let id = required_string(node, "id", "a resolve node")?;
        if let Some(features) = node.get("features") {
            let feature_values = features.as_array().ok_or_else(|| {
                CargoTreeError::Metadata(format!("resolve node {id} has no feature array"))
            })?;
            if feature_values.len() > MAX_CARGO_METADATA_STRING_ARRAY {
                return Err(CargoTreeError::Metadata(format!(
                    "resolve node {id} has too many features"
                )));
            }
            total_features = total_features.saturating_add(feature_values.len());
            if total_features > MAX_CARGO_RESOLVED_FEATURES {
                return Err(CargoTreeError::Metadata(
                    "resolved feature inventory exceeds its limit".to_owned(),
                ));
            }
            let features = feature_values
                .iter()
                .map(|feature| {
                    let feature = feature
                        .as_str()
                        .filter(|feature| {
                            !feature.is_empty() && feature.len() <= MAX_CARGO_METADATA_STRING_BYTES
                        })
                        .ok_or_else(|| {
                            CargoTreeError::Metadata(format!(
                                "resolve node {id} has a malformed feature"
                            ))
                        })?;
                    total_feature_bytes = total_feature_bytes.saturating_add(feature.len());
                    if total_feature_bytes > MAX_CARGO_RESOLVED_FEATURE_BYTES {
                        return Err(CargoTreeError::Metadata(
                            "resolved feature bytes exceed their limit".to_owned(),
                        ));
                    }
                    Ok(feature.to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            if resolved_features.insert(id.clone(), features).is_some() {
                return Err(CargoTreeError::Metadata(format!(
                    "duplicate resolve node {id}"
                )));
            }
        }
    }

    let listed = required_array(&root, "packages", "metadata")?;
    if listed.len() > super::tree::MAX_TREE_PACKAGES {
        return Err(CargoTreeError::Metadata(
            "packages exceeds the package limit".to_owned(),
        ));
    }
    let mut packages = Vec::with_capacity(listed.len());
    let mut package_ids = BTreeSet::new();
    for package in listed {
        let id = required_string(package, "id", "a package")?;
        if !package_ids.insert(id.clone()) {
            return Err(CargoTreeError::Metadata(format!(
                "duplicate package id {id}"
            )));
        }
        let member = members.contains(&id);
        let source = match package.get("source") {
            Some(Value::Null) => None,
            Some(Value::String(source))
                if !source.is_empty() && source.len() <= MAX_CARGO_METADATA_STRING_BYTES =>
            {
                Some(source.as_str())
            }
            _ => {
                return Err(CargoTreeError::Metadata(format!(
                    "{id} has no valid source field"
                )));
            }
        };
        let manifest = required_string(package, "manifest_path", &id)?;
        if !admitted_metadata_path(&manifest) {
            return Err(CargoTreeError::Metadata(format!(
                "{id} has a noncanonical manifest_path"
            )));
        }
        let targets = required_array(package, "targets", &id)?;
        if targets.len() > MAX_CARGO_TARGETS_PER_PACKAGE {
            return Err(CargoTreeError::Metadata(format!(
                "{id} exceeds the target limit"
            )));
        }
        let mut has_bin = false;
        for target in targets {
            let kinds = required_string_array(target, "kind", "a target")?;
            if kinds.is_empty() {
                return Err(CargoTreeError::Metadata(format!(
                    "{id} has a target with no kinds"
                )));
            }
            has_bin |= kinds.iter().any(|kind| kind == "bin");
        }
        let origin = Some(match source {
            Some(source) => source_origin(source),
            None => PackageOrigin::Vendored {
                path: Path::new(&manifest)
                    .parent()
                    .and_then(|package_root| {
                        SourceAuthority::vendored_display_path(
                            package_root,
                            Path::new(&workspace_root),
                        )
                    })
                    .unwrap_or_default(),
            },
        });
        let name = required_string(package, "name", &id)?;
        let version = required_string(package, "version", &id)?;
        let license = optional_string(package, "license", &id)?;
        let description = optional_string(package, "description", &id)?;
        let categories = optional_string_array(package, "categories", &id)?;
        let keywords = optional_string_array(package, "keywords", &id)?;
        let source_root = Path::new(&manifest).parent().map(Path::to_path_buf);
        let source_authority = match (source_revision, resolved_features.get(&id)) {
            (Some(source_revision), Some(features)) => SourceAuthority::from_metadata_observation(
                &name,
                &version,
                &id,
                source,
                &manifest,
                &workspace_root,
                source_revision,
                host,
                features,
            )
            .map(SourceAuthorityState::Admitted)
            .unwrap_or_else(SourceAuthorityState::Unavailable),
            (None, _) => {
                SourceAuthorityState::Unavailable(SourceAuthorityFailure::MissingSourceRevision)
            }
            (_, None) => {
                SourceAuthorityState::Unavailable(SourceAuthorityFailure::MissingResolvedFeatures)
            }
        };
        packages.push(TreeInputPackage {
            name,
            version,
            id,
            member,
            has_bin,
            origin,
            source_root,
            source_authority,
            license,
            description,
            categories,
            keywords,
        });
    }
    if let Some(missing) = members.iter().find(|member| !package_ids.contains(*member)) {
        return Err(CargoTreeError::Metadata(format!(
            "workspace member {missing} is missing from packages"
        )));
    }

    let nodes = required_array(
        root.get("resolve")
            .ok_or_else(|| CargoTreeError::Metadata("no resolve graph".to_owned()))?,
        "nodes",
        "resolve graph",
    )?;
    if nodes.len() > super::tree::MAX_TREE_PACKAGES {
        return Err(CargoTreeError::Metadata(
            "resolve.nodes exceeds the package limit".to_owned(),
        ));
    }
    let mut node_ids = BTreeSet::new();
    for node in nodes {
        let id = required_string(node, "id", "a resolve node")?;
        if !package_ids.contains(&id) {
            return Err(CargoTreeError::Metadata(format!(
                "resolve node {id} has no matching package"
            )));
        }
        if !node_ids.insert(id.clone()) {
            return Err(CargoTreeError::Metadata(format!(
                "duplicate resolve node {id}"
            )));
        }
        let _ = required_array(node, "deps", &format!("resolve node {id}"))?;
    }
    if let Some(missing) = members.iter().find(|member| !node_ids.contains(*member)) {
        return Err(CargoTreeError::Metadata(format!(
            "workspace member {missing} is missing from the resolve graph"
        )));
    }

    let mut edges = Vec::new();
    let mut edge_count = 0_usize;
    for node in nodes {
        let from = required_string(node, "id", "a resolve node")?;
        let dependencies = required_array(node, "deps", &format!("resolve node {from}"))?;
        edge_count = edge_count.saturating_add(dependencies.len());
        if edge_count > MAX_CARGO_RESOLVE_EDGES {
            return Err(CargoTreeError::Metadata(
                "resolved dependency edges exceed their limit".to_owned(),
            ));
        }
        for dependency in dependencies {
            let to = required_string(dependency, "pkg", "a resolved dependency")?;
            if !package_ids.contains(&to) {
                return Err(CargoTreeError::Metadata(format!(
                    "resolved dependency {from} -> {to} has no matching package"
                )));
            }
            if !node_ids.contains(&to) {
                return Err(CargoTreeError::Metadata(format!(
                    "resolved dependency {from} -> {to} has no resolve node"
                )));
            }
            let mut edge = TreeEdge {
                from: from.clone(),
                to,
                normal: false,
                dev: false,
                build: false,
            };
            let kinds = required_array(dependency, "dep_kinds", "a resolved dependency")?;
            if kinds.is_empty() {
                return Err(CargoTreeError::Metadata(format!(
                    "resolved dependency {from} -> {} has no dependency kinds",
                    edge.to
                )));
            }
            for kind in kinds {
                match kind.get("kind") {
                    Some(Value::Null) => edge.normal = true,
                    Some(Value::String(kind)) if kind == "normal" => edge.normal = true,
                    Some(Value::String(kind)) if kind == "dev" => edge.dev = true,
                    Some(Value::String(kind)) if kind == "build" => edge.build = true,
                    _ => {
                        return Err(CargoTreeError::Metadata(format!(
                            "resolved dependency {from} -> {} has an unknown kind",
                            edge.to
                        )));
                    }
                }
            }
            edges.push(edge);
        }
    }
    // Cargo's package array describes package manifests, while the filtered
    // `resolve` graph identifies packages selected for this target. Start at
    // every workspace member and retain only packages reachable through the
    // exact resolved edges; an unreferenced package row is not evidence that
    // it builds here.
    let mut reachable = members.clone();
    let mut pending = members.iter().cloned().collect::<VecDeque<_>>();
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &edges {
        outgoing
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    while let Some(from) = pending.pop_front() {
        if let Some(targets) = outgoing.get(from.as_str()) {
            for &to in targets {
                if reachable.insert(to.to_owned()) {
                    pending.push_back(to.to_owned());
                }
            }
        }
    }
    packages.retain(|package| reachable.contains(&package.id));
    edges.retain(|edge| reachable.contains(&edge.from) && reachable.contains(&edge.to));
    let (locked_inactive, locked_inactive_coverage) = match lockfile {
        Some(lockfile) => {
            let mut here = BTreeMap::new();
            for package in &packages {
                *here.entry(package_identity(package)).or_insert(0_usize) += 1;
            }
            let locked = locked_packages(lockfile)?;
            let mut locked_counts = BTreeMap::new();
            for package in &locked {
                *locked_counts
                    .entry((
                        package.name.clone(),
                        package.version.clone(),
                        package.source.clone(),
                    ))
                    .or_insert(0_usize) += 1;
            }
            let mut count = 0_usize;
            let mut unmatched_packages = 0_usize;
            let identities = locked_counts
                .keys()
                .chain(here.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for identity in identities {
                let locked_count = locked_counts.get(&identity).copied().unwrap_or_default();
                let current_count = here.get(&identity).copied().unwrap_or_default();
                match (locked_count, current_count) {
                    (0, 0) | (1, 1) => {}
                    (1, 0) => count = count.saturating_add(1),
                    (locked_count, current_count) => {
                        // A source-less lock row has no path field. A
                        // one-to-one name/version match is unambiguous under
                        // Cargo's lockfile package-collision rule; repeated
                        // rows or mismatched source identities are not. Do not
                        // count uncertain rows as proven packages.
                        unmatched_packages =
                            unmatched_packages.saturating_add(locked_count.max(current_count));
                    }
                }
            }
            let coverage = if unmatched_packages == 0 {
                LockedInactiveCoverage::Complete
            } else {
                LockedInactiveCoverage::Partial {
                    unmatched_packages: u32::try_from(unmatched_packages).unwrap_or(u32::MAX),
                }
            };
            (u32::try_from(count).unwrap_or(u32::MAX), coverage)
        }
        None => (0, LockedInactiveCoverage::Unavailable),
    };
    Ok(TreeInput {
        source: TreeSource::Cargo {
            host: host.to_owned(),
        },
        root: workspace_root,
        packages,
        edges,
        locked_inactive,
        locked_inactive_coverage,
    })
}

fn admitted_metadata_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        && !value.contains("//")
        && !value.chars().any(char::is_control)
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
}

struct Locked {
    row_index: usize,
    duplicate_identity: bool,
    name: String,
    version: String,
    source: Option<String>,
    dependencies: Vec<String>,
}

fn locked_packages(lockfile: &str) -> Result<Vec<Locked>, CargoTreeError> {
    if lockfile.len() > MAX_CARGO_LOCKFILE_BYTES {
        return Err(CargoTreeError::Lockfile(
            "Cargo.lock exceeds the bounded input size".to_owned(),
        ));
    }
    let document: toml::Value = lockfile
        .parse()
        .map_err(|error: toml::de::Error| CargoTreeError::Lockfile(error.to_string()))?;
    let listed = document
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| CargoTreeError::Lockfile("no [[package]] entries".to_owned()))?;
    if listed.len() > MAX_LOCKED_PACKAGES {
        return Err(CargoTreeError::Lockfile(
            "Cargo.lock package inventory exceeds its limit".to_owned(),
        ));
    }
    let mut total_dependencies = 0_usize;
    let packages = listed
        .iter()
        .enumerate()
        .map(|(row_index, package)| {
            let field = |key: &str| {
                package
                    .get(key)
                    .and_then(toml::Value::as_str)
                    .filter(|value| {
                        !value.is_empty() && value.len() <= MAX_CARGO_METADATA_STRING_BYTES
                    })
                    .map(ToOwned::to_owned)
            };
            let source = match package.get("source") {
                None => None,
                Some(toml::Value::String(source))
                    if !source.is_empty() && source.len() <= MAX_CARGO_METADATA_STRING_BYTES =>
                {
                    Some(source.clone())
                }
                Some(_) => {
                    return Err(CargoTreeError::Lockfile(
                        "a package source is not a string".to_owned(),
                    ));
                }
            };
            let dependencies = match package.get("dependencies") {
                None => Vec::new(),
                Some(dependencies) => {
                    let dependencies = dependencies.as_array().ok_or_else(|| {
                        CargoTreeError::Lockfile(
                            "a package's dependencies are not an array".to_owned(),
                        )
                    })?;
                    if dependencies.len() > MAX_LOCKED_DEPENDENCIES_PER_PACKAGE {
                        return Err(CargoTreeError::Lockfile(
                            "a package's dependency inventory exceeds its limit".to_owned(),
                        ));
                    }
                    total_dependencies = total_dependencies
                        .checked_add(dependencies.len())
                        .filter(|count| *count <= MAX_CARGO_RESOLVE_EDGES)
                        .ok_or_else(|| {
                            CargoTreeError::Lockfile(
                                "Cargo.lock dependency inventory exceeds its limit".to_owned(),
                            )
                        })?;
                    dependencies
                        .iter()
                        .map(|dependency| {
                            dependency
                                .as_str()
                                .filter(|text| text.len() <= MAX_CARGO_METADATA_STRING_BYTES)
                                .map(ToOwned::to_owned)
                                .ok_or_else(|| {
                                    CargoTreeError::Lockfile(
                                        "a dependency entry is not a bounded string".to_owned(),
                                    )
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                }
            };
            Ok(Locked {
                row_index,
                duplicate_identity: false,
                name: field("name")
                    .ok_or_else(|| CargoTreeError::Lockfile("a package has no name".to_owned()))?,
                version: field("version").ok_or_else(|| {
                    CargoTreeError::Lockfile("a package has no version".to_owned())
                })?,
                source,
                dependencies,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut identity_counts = BTreeMap::new();
    for package in &packages {
        *identity_counts
            .entry((
                package.name.clone(),
                package.version.clone(),
                package.source.clone(),
            ))
            .or_insert(0_usize) += 1;
    }
    let mut packages = packages;
    for package in &mut packages {
        package.duplicate_identity = identity_counts
            .get(&(
                package.name.clone(),
                package.version.clone(),
                package.source.clone(),
            ))
            .is_some_and(|count| *count > 1);
    }
    Ok(packages)
}

/// Reads `Cargo.lock` alone, for when Cargo cannot answer.
///
/// All package rows and lockfile edges are retained, but workspace membership
/// is unknown. Without roots or package metadata, the graph cannot claim which
/// rows are direct dependencies or provide paths from the workspace.
/// The lockfile does not distinguish workspace members from local path
/// dependencies, nor does it reveal the path for either. `patched` is retained
/// for source compatibility but is not treated as proof of membership or a
/// package's exact path.
///
/// # Errors
///
/// Returns [`CargoTreeError::Lockfile`] when `lockfile` is not Cargo's format.
pub fn lockfile_input(
    lockfile: &str,
    root: &str,
    _patched: &BTreeSet<String>,
    reason: &str,
) -> Result<TreeInput, CargoTreeError> {
    if lockfile.len() > MAX_CARGO_LOCKFILE_BYTES {
        return Err(CargoTreeError::Lockfile(
            "Cargo.lock exceeds the bounded input size".to_owned(),
        ));
    }
    if root.is_empty()
        || root.len() > MAX_CARGO_METADATA_STRING_BYTES
        || reason.len() > MAX_CARGO_METADATA_STRING_BYTES
    {
        return Err(CargoTreeError::Lockfile(
            "Cargo lockfile root or reason exceeds its limit".to_owned(),
        ));
    }
    let locked = locked_packages(lockfile)?;
    let id = |package: &Locked| {
        let base = format!("{} {} {:?}", package.name, package.version, package.source);
        if package.duplicate_identity {
            format!("{base} row {}", package.row_index)
        } else {
            base
        }
    };
    let mut by_name: BTreeMap<&str, Vec<&Locked>> = BTreeMap::new();
    let mut by_name_version: BTreeMap<(&str, &str), Vec<&Locked>> = BTreeMap::new();
    let mut by_name_source: BTreeMap<(&str, Option<&str>), Vec<&Locked>> = BTreeMap::new();
    let mut by_name_version_source: BTreeMap<(&str, &str, &str), Vec<&Locked>> = BTreeMap::new();
    for package in &locked {
        by_name
            .entry(package.name.as_str())
            .or_default()
            .push(package);
        by_name_version
            .entry((package.name.as_str(), package.version.as_str()))
            .or_default()
            .push(package);
        by_name_source
            .entry((package.name.as_str(), package.source.as_deref()))
            .or_default()
            .push(package);
        if let Some(source) = package.source.as_deref() {
            by_name_version_source
                .entry((package.name.as_str(), package.version.as_str(), source))
                .or_default()
                .push(package);
        }
    }
    let packages = locked
        .iter()
        .map(|package| TreeInputPackage {
            id: id(package),
            name: package.name.clone(),
            version: package.version.clone(),
            member: false,
            has_bin: false,
            origin: Some(match package.source.as_deref() {
                Some(source) => source_origin(source),
                None => PackageOrigin::Unresolved { source: None },
            }),
            ..TreeInputPackage::default()
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut unattributed_edges = 0_usize;
    let ambiguous_package_rows = locked
        .iter()
        .filter(|package| package.duplicate_identity)
        .count();
    for package in &locked {
        for dependency in &package.dependencies {
            let mut parts = dependency.split_whitespace();
            let Some(name) = parts.next() else {
                unattributed_edges = unattributed_edges.saturating_add(1);
                continue;
            };
            let second = parts.next();
            let (version, source_selector) = match second {
                Some(source) if source.starts_with('(') => (None, Some(source)),
                version => (version, parts.next()),
            };
            let source_selector = match source_selector {
                Some(source) => match source
                    .strip_prefix('(')
                    .and_then(|source| source.strip_suffix(')'))
                {
                    Some(source) => Some(source),
                    None => {
                        unattributed_edges = unattributed_edges.saturating_add(1);
                        continue;
                    }
                },
                None => None,
            };
            if parts.next().is_some() {
                unattributed_edges = unattributed_edges.saturating_add(1);
                continue;
            }
            let candidates = match (version, source_selector) {
                (Some(version), Some(source)) => {
                    by_name_version_source.get(&(name, version, source))
                }
                (Some(version), None) => by_name_version.get(&(name, version)),
                (None, Some(source)) => by_name_source.get(&(name, Some(source))),
                (None, None) => by_name.get(name),
            };
            let Some(candidates) = candidates else {
                unattributed_edges = unattributed_edges.saturating_add(1);
                continue;
            };
            if let [target] = candidates.as_slice() {
                edges.push(TreeEdge {
                    from: id(package),
                    to: id(target),
                    normal: true,
                    dev: false,
                    build: false,
                });
            } else {
                unattributed_edges = unattributed_edges.saturating_add(1);
            }
        }
    }
    Ok(TreeInput {
        source: TreeSource::Lockfile {
            reason: reason.to_owned(),
            coverage: if unattributed_edges == 0 && ambiguous_package_rows == 0 {
                LockfileGraphCoverage::Complete
            } else {
                LockfileGraphCoverage::Partial {
                    ambiguous_edges: u32::try_from(unattributed_edges).unwrap_or(u32::MAX),
                    ambiguous_package_rows: u32::try_from(ambiguous_package_rows)
                        .unwrap_or(u32::MAX),
                }
            },
            workspace_membership: LockfileWorkspaceMembership::Unknown,
        },
        root: root.to_owned(),
        packages,
        edges,
        locked_inactive: 0,
        locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
    })
}
