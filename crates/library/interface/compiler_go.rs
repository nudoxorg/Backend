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
        }
    }
}
