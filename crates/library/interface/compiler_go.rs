//! Closed Go dependency-admission failures, without native diagnostic bytes.

use serde::{Deserialize, Serialize};

/// Exact offline dependency-selection failure retained by Go package authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoAuthorityFailureKind {
    /// The selected Go executable could not complete its dependency listing.
    DependencyInvocation,
    /// Go's listing did not satisfy its expected package protocol.
    DependencyProtocol,
    /// Go rejected one package or its transitive dependency.
    DependencyPackageLoad,
    /// Go selected no packages for the admitted root.
    DependencyEmptyGraph,
    /// A selected dependency file changed or could not be read safely.
    DependencyIncompleteFiles,
    /// The loader selected a path outside the admitted filesystem roots.
    DependencyUnsafePath,
    /// Selected dependency files exceeded a bounded witness budget.
    DependencyLimit,
}

#[cfg(test)]
mod tests {
    use super::GoAuthorityFailureKind as G;

    #[test]
    fn public_go_failure_contract_has_exact_closed_names_and_guidance() {
        for (kind, wire, tag, detail) in [
            (
                G::DependencyInvocation,
                "dependency_invocation",
                "go_dependency_invocation_failed",
                "Go could not inspect the dependency graph with the selected offline toolchain; run `go mod download` in the project and retry",
            ),
            (
                G::DependencyProtocol,
                "dependency_protocol",
                "go_dependency_protocol_invalid",
                "Go returned an invalid dependency listing; check the installed Go toolchain and retry",
            ),
            (
                G::DependencyPackageLoad,
                "dependency_package_load",
                "go_dependency_package_load_rejected",
                "Go rejected the offline package dependency graph; run `go mod download` in the project and retry; use `go list -deps ./...` to inspect any remaining Go errors",
            ),
            (
                G::DependencyEmptyGraph,
                "dependency_empty_graph",
                "go_dependency_graph_empty",
                "Go selected no packages for semantic compilation; check the project module and source layout",
            ),
            (
                G::DependencyIncompleteFiles,
                "dependency_incomplete_files",
                "go_dependency_files_incomplete",
                "Go dependency files could not be captured stably; finish dependency setup or edits and retry",
            ),
            (
                G::DependencyUnsafePath,
                "dependency_unsafe_path",
                "go_dependency_path_rejected",
                "Go selected a dependency path outside the admitted module, cache, or local replacement roots; check module replacements and source symlinks",
            ),
            (
                G::DependencyLimit,
                "dependency_limit",
                "go_dependency_witness_limit",
                "Go selected dependency files beyond the bounded semantic witness budget; the dependency closure was not admitted",
            ),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(wire));
            assert_eq!(
                serde_json::from_value::<G>(serde_json::json!(wire)).unwrap(),
                kind
            );
            assert_eq!(kind.kind_tag(), tag);
            assert_eq!(kind.detail(), detail);
        }
        assert!(serde_json::from_value::<G>(serde_json::json!("unrecognized_go_failure")).is_err());
    }
}

impl GoAuthorityFailureKind {
    /// Stable machine-readable refusal tag.
    #[must_use]
    pub const fn kind_tag(self) -> &'static str {
        match self {
            Self::DependencyInvocation => "go_dependency_invocation_failed",
            Self::DependencyProtocol => "go_dependency_protocol_invalid",
            Self::DependencyPackageLoad => "go_dependency_package_load_rejected",
            Self::DependencyEmptyGraph => "go_dependency_graph_empty",
            Self::DependencyIncompleteFiles => "go_dependency_files_incomplete",
            Self::DependencyUnsafePath => "go_dependency_path_rejected",
            Self::DependencyLimit => "go_dependency_witness_limit",
        }
    }

    /// Path-free explanation and ordinary setup command for an offline retry.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::DependencyInvocation => {
                "Go could not inspect the dependency graph with the selected offline toolchain; run `go mod download` in the project and retry"
            }
            Self::DependencyProtocol => {
                "Go returned an invalid dependency listing; check the installed Go toolchain and retry"
            }
            Self::DependencyPackageLoad => {
                "Go rejected the offline package dependency graph; run `go mod download` in the project and retry; use `go list -deps ./...` to inspect any remaining Go errors"
            }
            Self::DependencyEmptyGraph => {
                "Go selected no packages for semantic compilation; check the project module and source layout"
            }
            Self::DependencyIncompleteFiles => {
                "Go dependency files could not be captured stably; finish dependency setup or edits and retry"
            }
            Self::DependencyUnsafePath => {
                "Go selected a dependency path outside the admitted module, cache, or local replacement roots; check module replacements and source symlinks"
            }
            Self::DependencyLimit => {
                "Go selected dependency files beyond the bounded semantic witness budget; the dependency closure was not admitted"
            }
        }
    }
}
