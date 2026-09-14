//! Mechanical enforcement for the backend v2 package graph.
//!
//! Cargo's workspace globs describe where a package happens to live.  They do
//! not describe which package owns that location, which product packages must
//! be present, or which direct product dependencies are part of the intended
//! architecture.  This crate keeps those facts in one small, exact table and
//! checks Cargo metadata against it.

use serde::Deserialize;
use std::collections::BTreeMap;
use thiserror::Error;

mod dag;
mod graph_validation;
mod quality;
mod repository;

use dag::canonical_dag;
#[cfg(test)]
use dag::{PACKAGE_DAG_JSON, parse_dag};

pub use graph_validation::validate;
pub(crate) use graph_validation::{find_cycle, normalized_path, workspace_member_ids};
pub use repository::{validate_repository, validate_workspace_json};

pub use quality::{
    SourceFile, validate_architecture_guards, validate_contracts, validate_cutover_contract,
    validate_cutover_mutations, validate_markdown_links, validate_module_structure,
    validate_policy_control, validate_rust_sources, validate_scope_fixture,
    validate_workspace_lints,
};

const LEGACY_OWNER_DIRECTORIES: [&str; 4] = ["compiler", "heart", "interface", "server"];
const PRODUCT_OWNER_DIRECTORIES: [&str; 4] = ["crates", "frontends", "extensions", "apps"];
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
const MAX_METADATA_PACKAGES: usize = 4096;
const MAX_METADATA_MEMBERS: usize = 4096;

type NameIndex<'a> = BTreeMap<&'a str, Vec<&'a Package>>;
type ManifestIndex<'a> = BTreeMap<String, Vec<&'a Package>>;

/// Cargo metadata fields required for graph validation.
#[derive(Debug, Deserialize)]
pub struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    #[serde(default)]
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: String,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
    /// Cargo emits this for a renamed dependency.  Use the package name when
    /// present so an alias cannot bypass a package-boundary check.
    #[serde(default)]
    package: Option<String>,
    /// Cargo metadata v1 calls the package name behind a dependency alias
    /// `rename`.  Accept both spellings because fixtures and older Cargo
    /// versions have emitted each form.
    #[serde(default)]
    rename: Option<String>,
    /// Dependency kind emitted by cargo metadata: absent/null is a normal
    /// runtime dependency, while `dev` and `build` are not part of the
    /// runtime product graph.  The product DAG must ignore them so test-only
    /// edges cannot masquerade as architectural dependencies.
    #[serde(default)]
    kind: Option<String>,
}

/// A precise package-graph violation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Violation {
    /// A required product package does not exist.
    #[error("missing v2 product package `{0}`")]
    MissingPackage(String),
    /// A product package exists more than once among workspace members.
    #[error("duplicate v2 product package `{0}`")]
    DuplicatePackage(String),
    /// A workspace member ID occurs more than once.
    #[error("workspace member `{0}` is listed more than once")]
    DuplicateWorkspaceMember(String),
    /// A workspace member ID has no corresponding metadata package.
    #[error("workspace member `{0}` is absent from cargo metadata")]
    UnknownWorkspaceMember(String),
    /// More than one workspace package claims one manifest path.
    #[error("manifest path `{path}` has multiple workspace owners: {packages:?}")]
    DuplicateManifestPath {
        /// Normalized manifest path.
        path: String,
        /// Package names claiming that path.
        packages: Vec<String>,
    },
    /// A package occupies a forbidden owner directory.
    #[error("package `{package}` has invalid v2 owner path `{path}`")]
    InvalidOwnerPath {
        /// Cargo package name.
        package: String,
        /// Observed manifest path.
        path: String,
    },
    /// A package with the wrong name occupies a reserved product path.
    #[error("manifest path `{path}` belongs to `{expected}` but is claimed by `{package}`")]
    OwnerPathCollision {
        /// Cargo package name that claimed the path.
        package: String,
        /// Package name reserved for the path.
        expected: String,
        /// Observed manifest path.
        path: String,
    },
    /// An unrecognized package occupies the v2 product source tree.
    #[error("unexpected product package `{package}` at `{path}`")]
    UnexpectedProductPackage {
        /// Cargo package name.
        package: String,
        /// Observed manifest path.
        path: String,
    },
    /// A lower core package imports an owner above it.
    #[error("forbidden core edge `{from}` -> `{to}`")]
    ForbiddenCoreEdge {
        /// Importing package.
        from: String,
        /// Imported package above it in the core DAG.
        to: String,
    },
    /// A core package omitted a boundary it is required to use directly.
    #[error("required core edge `{from}` -> `{to}` is missing")]
    MissingCoreEdge {
        /// Importing package.
        from: String,
        /// Required lower boundary.
        to: String,
    },
    /// A product package imports a backend package outside its exact list.
    #[error("forbidden backend edge `{from}` -> `{to}`")]
    ForbiddenBackendEdge {
        /// Importing package.
        from: String,
        /// Imported backend package.
        to: String,
    },
    /// A product package omitted one of its exact direct backend dependencies.
    #[error("required backend edge `{from}` -> `{to}` is missing")]
    MissingBackendEdge {
        /// Importing package.
        from: String,
        /// Required direct backend package.
        to: String,
    },
    /// The product dependency graph contains a directed cycle.
    #[error("cycle in v2 package graph: {cycle:?}")]
    DependencyCycle {
        /// One closed cycle, with its first package repeated at the end.
        cycle: Vec<String>,
    },
    /// Legacy code accidentally entered the workspace.
    #[error("legacy package `{package}` entered the v2 workspace from `{path}`")]
    LegacyWorkspaceMember {
        /// Legacy Cargo package name.
        package: String,
        /// Legacy manifest selected by workspace membership.
        path: String,
    },
    /// Cargo metadata was malformed.
    #[error("invalid cargo metadata: {0}")]
    InvalidMetadata(String),
    /// The compile-time product DAG is malformed or internally inconsistent.
    #[error("invalid package DAG: {0}")]
    InvalidDag(String),
    /// An actual product dependency violates the documented layer ordering.
    #[error("layer-violating product edge `{from}` -> `{to}`")]
    LayerViolation {
        /// Importing product package.
        from: String,
        /// Imported product package.
        to: String,
    },
    /// A product or workspace package does not inherit the root lint policy.
    #[error("package `{package}` does not inherit workspace lints")]
    MissingLintInheritance {
        /// Cargo package name.
        package: String,
    },
    /// One required root workspace lint is absent or has a different level.
    #[error("workspace lint `{table}.{lint}` must be `{expected}` (observed {observed:?})")]
    MissingWorkspaceLint {
        /// Manifest table containing the lint.
        table: String,
        /// Lint name.
        lint: String,
        /// Required level.
        expected: String,
        /// Observed level, if present.
        observed: Option<String>,
    },
    /// A generated policy file still names a legacy package or owner path.
    #[error("stale policy package reference at {path}:{line}: `{reference}`")]
    StalePolicyPackageReference {
        /// Policy source path.
        path: String,
        /// One-based policy source line.
        line: usize,
        /// Legacy package or owner reference.
        reference: String,
    },
    /// A generated policy file omitted one of the target workspace roots.
    #[error("policy `{path}` has no declared scope for `{scope}`")]
    MissingPolicyScope {
        /// Policy source path.
        path: String,
        /// Required repository root pattern.
        scope: String,
    },
    /// A production Rust file uses a panic-style escape hatch.
    #[error("forbidden production construct `{construct}` at {path}:{line}")]
    ForbiddenProductionConstruct {
        /// Repository-relative source path.
        path: String,
        /// One-based source line.
        line: usize,
        /// Construct spelling.
        construct: String,
    },
    /// A production Rust file suppresses a broad lint family.
    #[error("broad lint allow at {path}:{line}: {lints:?}")]
    BroadLintAllow {
        /// Repository-relative source path.
        path: String,
        /// One-based source line.
        line: usize,
        /// Broad lint names.
        lints: Vec<String>,
    },
    /// Production code references the removed forgeable identity constructor.
    #[error("removed identity constructor `{symbol}` at {path}:{line}")]
    RemovedIdentityConstructor {
        /// Repository-relative source path.
        path: String,
        /// One-based source line.
        line: usize,
        /// Removed constructor spelling.
        symbol: String,
    },
    /// Publication code attempts to select a store head without authority.
    #[error("authority-less store publication at {path}:{line}: `{call}`")]
    AuthoritylessStorePublication {
        /// Repository-relative source path.
        path: String,
        /// One-based source line.
        line: usize,
        /// Forbidden call spelling.
        call: String,
    },
    /// A canonical grammar literal or encoder was duplicated outside its owner.
    #[error(
        "canonical grammar `{domain}` marker `{marker}` is outside owner `{owner}` at {path}:{line}"
    )]
    CanonicalGrammarOutsideOwner {
        /// Registry domain name.
        domain: String,
        /// Repository-relative source path containing the duplicate.
        path: String,
        /// One-based source line.
        line: usize,
        /// Registry-declared owner file.
        owner: String,
        /// Literal or encoder marker.
        marker: String,
    },
    /// The canonical grammar ownership registry is malformed.
    #[error("invalid canonical grammar ownership policy at {path}: {detail}")]
    InvalidCanonicalOwnershipPolicy {
        /// Policy source path.
        path: String,
        /// Precise policy mismatch.
        detail: String,
    },
    /// The production module-size policy is malformed.
    #[error("invalid module structure policy at {path}: {detail}")]
    InvalidModuleStructurePolicy {
        /// Policy source path.
        path: String,
        /// Precise policy mismatch.
        detail: String,
    },
    /// A production Rust module exceeds the repository's structural size cap.
    #[error("production module exceeds {limit} lines at {path}: {lines} lines")]
    OversizedProductionModule {
        /// Repository-relative source path.
        path: String,
        /// Counted production source lines.
        lines: usize,
        /// Maximum allowed production source lines.
        limit: usize,
    },
    /// A second tree-shaped recursive node exists outside the shared version
    /// path-copy kernel.
    #[error("duplicate path-copy kernel `{name}` at {path}:{line}")]
    DuplicatePathCopyKernel {
        /// Repository-relative source path.
        path: String,
        /// Recursive node type.
        name: String,
        /// One-based source line.
        line: usize,
    },
    /// A local Markdown destination does not exist in the repository
    /// inventory.
    #[error("broken local Markdown link at {path}:{line}: `{target}`")]
    BrokenMarkdownLink {
        /// Markdown source path.
        path: String,
        /// One-based source line.
        line: usize,
        /// Original destination.
        target: String,
    },
    /// The canonical control-plane schema or one of its projections drifted.
    #[error("invalid control-plane contract schema at {path}: {detail}")]
    InvalidContractSchema {
        /// Contract file path.
        path: String,
        /// Precise schema mismatch.
        detail: String,
    },
    /// A canonical control-plane definition has no projection.
    #[error("missing control-plane contract projection `{0}`")]
    MissingContractProjection(String),
    /// A canonical control-plane definition has multiple projections.
    #[error("control-plane contract projection `{name}` appears {count} times")]
    DuplicateContractProjection {
        /// Definition name.
        name: String,
        /// Number of projections.
        count: usize,
    },
    /// A projection exists for no canonical definition.
    #[error("unexpected control-plane contract projection `{0}`")]
    UnexpectedContractProjection(String),
    /// A required quality input could not be read.
    #[error("quality gate could not read `{path}`: {detail}")]
    QualityInputError {
        /// Path that was requested.
        path: String,
        /// Filesystem or decoding detail.
        detail: String,
    },
    /// A documented cutover contract or mutation campaign drifted.
    #[error("invalid cutover contract at {path}: {detail}")]
    InvalidCutoverContract {
        /// Contract source path.
        path: String,
        /// Precise contract mismatch.
        detail: String,
    },
    /// The exactly-one ownership fixture is malformed or does not cover the
    /// target workspace roots.
    #[error("invalid scope fixture at {path}: {detail}")]
    InvalidScopeFixture {
        /// Fixture source path.
        path: String,
        /// Precise fixture mismatch.
        detail: String,
    },
}

/// Validates package identity, ownership, exact direct dependencies, and the
/// selected product DAG.
///
/// When `require_complete` is false, missing product packages are tolerated so
/// the same checker can guard intermediate construction without weakening any
/// boundary, ownership, dependency, or cycle law for packages that are
/// present.
///
/// # Errors
///
/// Returns every package, ownership, dependency, cycle, and legacy-member
/// violation found in the decoded metadata. Malformed JSON is returned as an
/// [`Violation::InvalidMetadata`] entry.
pub fn validate_json(json: &str, require_complete: bool) -> Result<(), Vec<Violation>> {
    if json.len() > MAX_METADATA_BYTES {
        return Err(vec![Violation::InvalidMetadata(format!(
            "metadata exceeds {MAX_METADATA_BYTES} bytes"
        ))]);
    }
    // `serde_json` is the only parser boundary; all structural errors are
    // returned as one typed violation for the command-line caller.
    let metadata: Metadata = serde_json::from_str(json)
        .map_err(|error| vec![Violation::InvalidMetadata(error.to_string())])?;
    validate(&metadata, require_complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_string(value: &str) -> String {
        format!(
            "\"{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        )
    }

    fn package(id: &str, name: &str, path: &str, dependencies: &[&str]) -> String {
        let dependencies = dependencies
            .iter()
            .map(|dependency| format!(r#"{{"name":{}}}"#, json_string(dependency)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"id":{},"name":{},"manifest_path":{},"dependencies":[{}]}}"#,
            json_string(id),
            json_string(name),
            json_string(path),
            dependencies,
        )
    }

    /// Removes JSON structural whitespace while preserving string literals,
    /// so tests can mutate the pretty-printed canonical DAG with compact
    /// needles without depending on formatting.
    fn compact_json(input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut in_string = false;
        let mut escaped = false;
        for character in input.chars() {
            if in_string {
                output.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
                continue;
            }
            match character {
                '"' => {
                    in_string = true;
                    output.push(character);
                }
                character if character.is_whitespace() => {}
                character => output.push(character),
            }
        }
        output
    }

    fn dependency(name: &str, kind: Option<&str>) -> String {
        match kind {
            Some(kind) => format!(
                r#"{{"name":{},"kind":{}}}"#,
                json_string(name),
                json_string(kind)
            ),
            None => format!(r#"{{"name":{}}}"#, json_string(name)),
        }
    }

    fn package_with_dependencies(
        id: &str,
        name: &str,
        path: &str,
        dependencies: &[String],
    ) -> String {
        format!(
            r#"{{"id":{},"name":{},"manifest_path":{},"dependencies":[{}]}}"#,
            json_string(id),
            json_string(name),
            json_string(path),
            dependencies.join(","),
        )
    }

    fn metadata(packages: &[String], members: &[&str]) -> String {
        format!(
            r#"{{"packages":[{}],"workspace_members":[{}]}}"#,
            packages.join(","),
            members
                .iter()
                .map(|member| json_string(member))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    fn complete_metadata() -> String {
        let result = canonical_dag();
        assert!(result.is_ok());
        let Some(dag) = result.ok() else {
            return String::new();
        };
        let packages = dag
            .packages
            .iter()
            .map(|spec| {
                let dependencies = spec
                    .dependencies
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                package(&spec.name, &spec.name, &spec.manifest_path, &dependencies)
            })
            .collect::<Vec<_>>();
        let members = dag
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        format!(
            r#"{{"packages":[{}],"workspace_members":[{}]}}"#,
            packages.join(","),
            members
                .iter()
                .map(|member| json_string(member))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    #[test]
    fn canonical_spec_covers_all_product_packages_once() {
        let result = canonical_dag();
        assert!(result.is_ok());
        let Some(dag) = result.ok() else { return };
        assert_eq!(dag.packages.len(), 29);
        assert_eq!(dag.core_names.len(), 11);
        let names = dag
            .packages
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            dag.packages
                .windows(2)
                .all(|packages| packages[0].order < packages[1].order)
        );
        assert_eq!(names.len(), dag.packages.len());
    }

    #[test]
    fn accepts_complete_canonical_graph() {
        assert_eq!(validate_json(&complete_metadata(), true), Ok(()));
    }

    #[test]
    fn rejects_malformed_and_metadata_drifted_dag_documents() {
        assert!(parse_dag("{").is_err());
        let drifted = PACKAGE_DAG_JSON.replace("\"core\": 11", "\"core\": 10");
        let error = parse_dag(&drifted).err().unwrap_or_default();
        assert!(error.contains("layer_counts metadata drift"));
    }

    #[test]
    fn rejects_duplicate_unknown_self_and_cyclic_dag_entries() {
        let source = compact_json(PACKAGE_DAG_JSON);
        let duplicate = source.replace(
            "{\"name\":\"backend-store\",\"manifest_path\"",
            "{\"name\":\"backend-version\",\"manifest_path\"",
        );
        assert!(
            parse_dag(&duplicate)
                .err()
                .is_some_and(|error| error.contains("duplicate package"))
        );

        let unknown = source.replace(
            "\"dependencies\":[\"backend-version\"],\"layer\":\"core\",\"order\":2",
            "\"dependencies\":[\"backend-missing\"],\"layer\":\"core\",\"order\":2",
        );
        assert!(
            parse_dag(&unknown)
                .err()
                .is_some_and(|error| error.contains("unknown dependency"))
        );

        let self_edge = source.replace(
            "\"dependencies\":[\"backend-version\"],\"layer\":\"core\",\"order\":2",
            "\"dependencies\":[\"backend-store\"],\"layer\":\"core\",\"order\":2",
        );
        assert!(
            parse_dag(&self_edge)
                .err()
                .is_some_and(|error| error.contains("depends on itself"))
        );

        let cycle = source.replace(
            "\"dependencies\":[],\"layer\":\"core\",\"order\":0",
            "\"dependencies\":[\"backend-store\"],\"layer\":\"core\",\"order\":0",
        );
        assert!(
            parse_dag(&cycle)
                .err()
                .is_some_and(|error| error.contains("cycle in package DAG"))
        );
    }

    #[test]
    fn ignores_dev_and_build_dependency_edges() {
        // A test-only (`dev`/`build`) edge to another product package that is
        // not part of the declared runtime DAG must not be treated as an
        // architectural dependency, because that would inject a false cycle.
        let dev = metadata(
            &[package_with_dependencies(
                "backend-version",
                "backend-version",
                "/x/crates/version/Cargo.toml",
                &[dependency("backend-flow", Some("dev"))],
            )],
            &["backend-version"],
        );
        assert_eq!(validate_json(&dev, false), Ok(()));

        let build = metadata(
            &[package_with_dependencies(
                "backend-version",
                "backend-version",
                "/x/crates/version/Cargo.toml",
                &[dependency("backend-flow", Some("build"))],
            )],
            &["backend-version"],
        );
        assert_eq!(validate_json(&build, false), Ok(()));

        // The exact same edge declared as a normal dependency is a real
        // upward layer violation and must be rejected.
        let normal = metadata(
            &[package_with_dependencies(
                "backend-version",
                "backend-version",
                "/x/crates/version/Cargo.toml",
                &[dependency("backend-flow", None)],
            )],
            &["backend-version"],
        );
        assert!(validate_json(&normal, false).is_err());
    }

    #[test]
    fn rejects_upward_core_edge() {
        let json = metadata(
            &[package(
                "a",
                "backend-store",
                "/x/crates/store/Cargo.toml",
                &["backend-flow"],
            )],
            &["a"],
        );
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.contains(&Violation::ForbiddenCoreEdge {
                from: "backend-store".into(),
                to: "backend-flow".into(),
            }) && items.contains(&Violation::LayerViolation {
                from: "backend-store".into(),
                to: "backend-flow".into(),
            })
        ));
    }

    #[test]
    fn rejects_renamed_dependency_that_hides_an_inverse_edge() {
        let json = metadata(
            &[r#"{"id":"a","name":"backend-store","manifest_path":"/x/crates/store/Cargo.toml","dependencies":[{"name":"lower_alias","rename":"backend-flow"}]}"#.to_owned()],
            &["a"],
        );
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.contains(&Violation::ForbiddenCoreEdge {
                from: "backend-store".into(),
                to: "backend-flow".into(),
            })
        ));

        // Cargo metadata v1 uses `name` for the resolved package and `rename`
        // for its local alias.  The resolved package must remain authoritative
        // when an actual alias appears in a manifest.
        let cargo_shape = metadata(
            &[r#"{"id":"a","name":"backend-store","manifest_path":"/x/crates/store/Cargo.toml","dependencies":[{"name":"backend-flow","rename":"local_flow"}]}"#.to_owned()],
            &["a"],
        );
        assert!(matches!(
            validate_json(&cargo_shape, false),
            Err(items) if items.contains(&Violation::ForbiddenCoreEdge {
                from: "backend-store".into(),
                to: "backend-flow".into(),
            })
        ));
    }

    #[test]
    fn accepts_exact_core_edge() {
        let json = metadata(
            &[package(
                "a",
                "backend-store",
                "/x/crates/store/Cargo.toml",
                &["backend-version"],
            )],
            &["a"],
        );
        assert_eq!(validate_json(&json, false), Ok(()));
    }

    #[test]
    fn rejects_missing_exact_leaf_edges() {
        let json = metadata(
            &[package(
                "a",
                "backend-compile",
                "/x/crates/compile/Cargo.toml",
                &["backend-version"],
            )],
            &["a"],
        );
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.contains(&Violation::MissingCoreEdge {
                from: "backend-compile".into(),
                to: "backend-semantic".into(),
            }) && items.contains(&Violation::MissingCoreEdge {
                from: "backend-compile".into(),
                to: "backend-flow".into(),
            })
        ));
    }

    #[test]
    fn rejects_broad_optional_extension_edges() {
        let json = metadata(
            &[package(
                "a",
                "backend-extension-qdrant",
                "/x/extensions/qdrant/Cargo.toml",
                &[
                    "backend-version",
                    "backend-replication",
                    "backend-execution",
                    "backend-flow",
                ],
            )],
            &["a"],
        );
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.contains(&Violation::ForbiddenBackendEdge {
                from: "backend-extension-qdrant".into(),
                to: "backend-flow".into(),
            })
        ));
    }

    #[test]
    fn rejects_wrong_owner_path_and_reserved_path_collision() {
        let json = metadata(
            &[package(
                "a",
                "backend-store",
                "/x/crates/flow/Cargo.toml",
                &[],
            )],
            &["a"],
        );
        let result = validate_json(&json, false);
        assert!(result.is_err());
        let Err(items) = result else { return };
        assert!(items.iter().any(|item| matches!(
            item,
            Violation::InvalidOwnerPath { package, .. } if package == "backend-store"
        )));
        assert!(items.iter().any(|item| matches!(
            item,
            Violation::OwnerPathCollision { expected, .. } if expected == "backend-flow"
        )));
    }

    #[test]
    fn rejects_duplicate_name_path_and_member_ownership() {
        let first = package("a", "backend-version", "/x/crates/version/Cargo.toml", &[]);
        let second = package("b", "backend-version", "/x/crates/version/Cargo.toml", &[]);
        let json = metadata(&[first, second], &["a", "b", "b"]);
        let result = validate_json(&json, false);
        assert!(result.is_err());
        let Err(items) = result else { return };
        assert!(items.contains(&Violation::DuplicatePackage("backend-version".into())));
        assert!(items.contains(&Violation::DuplicateWorkspaceMember("b".into())));
        assert!(
            items
                .iter()
                .any(|item| matches!(item, Violation::DuplicateManifestPath { .. }))
        );
    }

    #[test]
    fn rejects_unrecognized_package_in_product_tree() {
        let json = metadata(
            &[package(
                "a",
                "backend-frontend-kotlin",
                "/x/frontends/kotlin/Cargo.toml",
                &[],
            )],
            &["a"],
        );
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.iter().any(|item| matches!(
                item,
                Violation::UnexpectedProductPackage { package, .. }
                    if package == "backend-frontend-kotlin"
            ))
        ));
    }

    #[test]
    fn rejects_legacy_workspace_member() {
        let json = metadata(
            &[package("a", "legacy", "/x/server/runtime/Cargo.toml", &[])],
            &["a"],
        );
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.iter().any(|item| matches!(
                item,
                Violation::LegacyWorkspaceMember { package, .. } if package == "legacy"
            ))
        ));
    }

    #[test]
    fn rejects_product_cycle_even_when_other_packages_are_incomplete() {
        let packages = [
            package(
                "backend-version",
                "backend-version",
                "/x/crates/version/Cargo.toml",
                &["backend-store"],
            ),
            package(
                "b",
                "backend-store",
                "/x/crates/store/Cargo.toml",
                &["backend-version"],
            ),
        ];
        let json = metadata(&packages, &["backend-version", "b"]);
        assert!(matches!(
            validate_json(&json, false),
            Err(items) if items.iter().any(|item| matches!(
                item,
                Violation::DependencyCycle { cycle }
                    if cycle
                        == &vec![
                            "backend-store".to_owned(),
                            "backend-version".to_owned(),
                            "backend-store".to_owned(),
                        ]
            ))
        ));
    }

    #[test]
    fn accepts_backslash_and_absolute_manifest_prefixes() {
        let json = metadata(
            &[package(
                "a",
                "backend-store",
                r"C:\workspace\crates\store\Cargo.toml",
                &["backend-version"],
            )],
            &["a"],
        );
        assert_eq!(validate_json(&json, false), Ok(()));
    }

    #[test]
    fn workspace_root_makes_owner_paths_exact_and_legacy_detection_relative() {
        let json = format!(
            r#"{{"packages":[{}],"workspace_members":["a"],"workspace_root":"/x/server"}}"#,
            package(
                "a",
                "backend-store",
                "/x/server/crates/store/Cargo.toml",
                &["backend-version"],
            )
        );
        assert_eq!(validate_json(&json, false), Ok(()));

        let foreign = format!(
            r#"{{"packages":[{}],"workspace_members":["a"],"workspace_root":"/x/server"}}"#,
            package(
                "a",
                "backend-store",
                "/x/server/other/crates/store/Cargo.toml",
                &["backend-version"],
            )
        );
        assert!(matches!(
            validate_json(&foreign, false),
            Err(items) if items.iter().any(|item| matches!(item, Violation::InvalidOwnerPath { package, .. } if package == "backend-store"))
        ));
    }
}
