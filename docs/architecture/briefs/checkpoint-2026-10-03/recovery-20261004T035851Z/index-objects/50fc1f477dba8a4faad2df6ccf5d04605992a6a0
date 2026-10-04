//! Bounded Cargo facts and profile-selection request contracts shared by the
//! owner, compiler frontend, CLI, MCP, and desktop surfaces.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Maximum Cargo workspace member rows retained in one facts response.
pub const MAX_RUST_CARGO_WORKSPACE_PACKAGES: usize = 1_024;
/// Maximum declared feature edges retained in one facts response.
pub const MAX_RUST_CARGO_FEATURE_EDGES: usize = 65_536;
/// Maximum Cargo target rows retained in one facts response.
pub const MAX_RUST_CARGO_TARGETS: usize = 8_192;
/// Maximum resolved package rows retained in one facts response.
pub const MAX_RUST_CARGO_RESOLVED_PACKAGES: usize = 16_384;
/// Maximum resolved dependency edges retained in one facts response.
pub const MAX_RUST_CARGO_DEPENDENCY_EDGES: usize = 131_072;
/// Maximum caller-selected feature names in one profile request.
pub const MAX_RUST_CARGO_PROFILE_FEATURES: usize = 256;
/// Maximum individual Cargo metadata spelling admitted to a product DTO.
pub const MAX_RUST_CARGO_METADATA_TEXT_BYTES: usize = 4_096;

/// Exact declared Cargo feature edge from a full metadata response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoFeatureFactV1 {
    /// Feature spelling declared by this package.
    pub name: Box<str>,
    /// Feature/dependency spellings enabled by this feature.
    pub enables: Box<[Box<str>]>,
}

/// Manifest-admitted Cargo target metadata for one workspace member.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoTargetFactV1 {
    /// Cargo target name.
    pub name: Box<str>,
    /// Cargo target kinds, such as `lib`, `bin`, `example`, `test`, or `custom-build`.
    pub kinds: Box<[Box<str>]>,
    /// Rust crate types selected by Cargo for this target.
    pub crate_types: Box<[Box<str>]>,
    /// Exact Cargo metadata source path.
    pub source_path: PathBuf,
    /// Features required for Cargo to activate this target.
    pub required_features: Box<[Box<str>]>,
    /// Edition recorded by Cargo for this target.
    pub edition: Box<str>,
    /// Cargo target test flag.
    pub test: bool,
    /// Cargo target doctest flag.
    pub doctest: bool,
    /// Cargo target doc flag.
    pub doc: bool,
}

/// Manifest and resolved-graph facts for one Cargo workspace member.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoWorkspacePackageFactV1 {
    /// Cargo's exact package identifier, including source/version identity.
    pub package_id: Box<str>,
    /// Declared package name.
    pub name: Box<str>,
    /// Exact package version spelling from Cargo metadata.
    pub version: Box<str>,
    /// Absolute Cargo metadata manifest path.
    pub manifest_path: PathBuf,
    /// Declared feature graph.
    pub features: Box<[RustCargoFeatureFactV1]>,
    /// Manifest-declared target inventory, including inactive targets.
    pub targets: Box<[RustCargoTargetFactV1]>,
}

/// One resolved Cargo dependency-kind edge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoDependencyKindFactV1 {
    /// Cargo dependency kind (`normal`, `build`, `dev`), or `None` for normal.
    pub kind: Option<Box<str>>,
    /// Cargo target predicate, if dependency resolution is target-specific.
    pub target: Option<Box<str>>,
}

/// One edge in Cargo's exact resolved package graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoResolvedDependencyFactV1 {
    /// Name used by the dependent package.
    pub name: Box<str>,
    /// Exact Cargo package identifier of the resolved dependency.
    pub package_id: Box<str>,
    /// Resolved dependency kinds and target predicates.
    pub kinds: Box<[RustCargoDependencyKindFactV1]>,
}

/// Resolved feature/dependency facts for one node in Cargo's resolution graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoResolvedPackageFactV1 {
    /// Exact Cargo package identifier.
    pub package_id: Box<str>,
    /// Active features selected by Cargo's full resolver.
    pub active_features: Box<[Box<str>]>,
    /// Exact resolved dependency edges.
    pub dependencies: Box<[RustCargoResolvedDependencyFactV1]>,
}

/// Canonical feature mode requested from Cargo for one metadata resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoFeatureSelectionV1 {
    /// Resolve every feature declared by the selected manifest/workspace.
    pub all_features: bool,
    /// Omit the package's default feature set.
    pub no_default_features: bool,
    /// Exact named features passed to Cargo, in canonical order.
    pub features: Box<[Box<str>]>,
}

/// Canonical Cargo resolution facts read by the owner.
///
/// These facts bind the manifest target inventory to one exact full Cargo
/// metadata response, lockfile, toolchain, and feature request. They do not
/// claim that build scripts ran or authorize compiler reuse.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoWorkspaceFactsV1 {
    /// Exact absolute manifest passed to Cargo metadata.
    pub manifest_path: PathBuf,
    /// Cargo-reported workspace root.
    pub workspace_root: PathBuf,
    /// Workspace package whose manifest exactly matched the selected root, if any.
    pub selected_package_id: Option<Box<str>>,
    /// Exact requested Cargo feature mode.
    pub requested_features: RustCargoFeatureSelectionV1,
    /// Workspace-member manifests, feature declarations, and targets.
    pub workspace_packages: Box<[RustCargoWorkspacePackageFactV1]>,
    /// Entire resolved graph, including registry/path dependencies.
    pub resolved_packages: Box<[RustCargoResolvedPackageFactV1]>,
    /// Digest of Cargo's exact full metadata response bytes for this read.
    pub metadata_response_digest: [u8; 32],
    /// Digest of the lockfile bytes used by this resolution (including an isolated lockfile).
    pub resolved_lockfile_digest: [u8; 32],
    /// Digest of the exact admitted compiler/Cargo executable and sysroot paths.
    pub toolchain_binding_digest: [u8; 32],
}

impl RustCargoWorkspaceFactsV1 {
    /// Checks bounds and canonical ordering before facts cross a product surface.
    pub fn admit(&self) -> Result<(), RustCargoFactsAdmissionError> {
        if !self.manifest_path.is_absolute()
            || !self.workspace_root.is_absolute()
            || self.workspace_packages.is_empty()
            || self.workspace_packages.len() > MAX_RUST_CARGO_WORKSPACE_PACKAGES
            || self.resolved_packages.is_empty()
            || self.resolved_packages.len() > MAX_RUST_CARGO_RESOLVED_PACKAGES
            || self.requested_features.features.len() > MAX_RUST_CARGO_PROFILE_FEATURES
        {
            return Err(RustCargoFactsAdmissionError::Shape);
        }
        check_sorted_text(self.requested_features.features.iter().map(AsRef::as_ref))?;

        let mut targets_seen = 0_usize;
        let mut features_seen = 0_usize;
        let mut package_ids = std::collections::BTreeSet::new();
        for package in &self.workspace_packages {
            check_text(&package.package_id)?;
            check_text(&package.name)?;
            check_text(&package.version)?;
            if !package.manifest_path.is_absolute()
                || package.features.is_empty() && package.targets.is_empty()
            {
                return Err(RustCargoFactsAdmissionError::Shape);
            }
            if !package_ids.insert(package.package_id.as_ref()) {
                return Err(RustCargoFactsAdmissionError::Ordering);
            }
            features_seen = features_seen.saturating_add(package.features.len());
            targets_seen = targets_seen.saturating_add(package.targets.len());
            if features_seen > MAX_RUST_CARGO_FEATURE_EDGES || targets_seen > MAX_RUST_CARGO_TARGETS
            {
                return Err(RustCargoFactsAdmissionError::Bound);
            }
            check_sorted_text(package.features.iter().map(|feature| feature.name.as_ref()))?;
            for feature in &package.features {
                check_text(&feature.name)?;
                check_sorted_text(feature.enables.iter().map(AsRef::as_ref))?;
                features_seen = features_seen.saturating_add(feature.enables.len());
                if features_seen > MAX_RUST_CARGO_FEATURE_EDGES {
                    return Err(RustCargoFactsAdmissionError::Bound);
                }
            }
            if !package.targets.windows(2).all(|pair| {
                (&pair[0].name, &pair[0].source_path, &pair[0].kinds)
                    < (&pair[1].name, &pair[1].source_path, &pair[1].kinds)
            }) {
                return Err(RustCargoFactsAdmissionError::Ordering);
            }
            for target in &package.targets {
                check_text(&target.name)?;
                check_text(&target.edition)?;
                if !target.source_path.is_absolute()
                    || target.kinds.is_empty()
                    || target.crate_types.is_empty()
                {
                    return Err(RustCargoFactsAdmissionError::Shape);
                }
                check_sorted_text(target.kinds.iter().map(AsRef::as_ref))?;
                check_sorted_text(target.crate_types.iter().map(AsRef::as_ref))?;
                check_sorted_text(target.required_features.iter().map(AsRef::as_ref))?;
                features_seen = features_seen.saturating_add(target.required_features.len());
                if features_seen > MAX_RUST_CARGO_FEATURE_EDGES {
                    return Err(RustCargoFactsAdmissionError::Bound);
                }
            }
        }
        if !self
            .workspace_packages
            .windows(2)
            .all(|pair| pair[0].package_id < pair[1].package_id)
        {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }
        if let Some(selected) = &self.selected_package_id
            && !package_ids.contains(selected.as_ref())
        {
            return Err(RustCargoFactsAdmissionError::Shape);
        }

        let resolved_ids = self
            .resolved_packages
            .iter()
            .map(|package| package.package_id.as_ref())
            .collect::<std::collections::BTreeSet<_>>();
        if resolved_ids.len() != self.resolved_packages.len()
            || !self
                .resolved_packages
                .windows(2)
                .all(|pair| pair[0].package_id < pair[1].package_id)
        {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }
        if self
            .workspace_packages
            .iter()
            .any(|package| !resolved_ids.contains(package.package_id.as_ref()))
        {
            return Err(RustCargoFactsAdmissionError::WorkspaceMemberUnresolved);
        }
        let mut dependency_edges = 0_usize;
        for package in &self.resolved_packages {
            check_text(&package.package_id)?;
            check_sorted_text(package.active_features.iter().map(AsRef::as_ref))?;
            check_sorted_pairs(
                package
                    .dependencies
                    .iter()
                    .map(|edge| (edge.name.as_ref(), edge.package_id.as_ref())),
            )?;
            dependency_edges = dependency_edges.saturating_add(package.dependencies.len());
            if dependency_edges > MAX_RUST_CARGO_DEPENDENCY_EDGES {
                return Err(RustCargoFactsAdmissionError::Bound);
            }
            for dependency in &package.dependencies {
                check_text(&dependency.name)?;
                check_text(&dependency.package_id)?;
                if !resolved_ids.contains(dependency.package_id.as_ref()) {
                    return Err(RustCargoFactsAdmissionError::Shape);
                }
                if !dependency
                    .kinds
                    .windows(2)
                    .all(|pair| (&pair[0].kind, &pair[0].target) < (&pair[1].kind, &pair[1].target))
                {
                    return Err(RustCargoFactsAdmissionError::Ordering);
                }
                for kind in &dependency.kinds {
                    if let Some(value) = &kind.kind {
                        check_text(value)?;
                    }
                    if let Some(value) = &kind.target {
                        check_text(value)?;
                    }
                }
            }
        }
        if self
            .selected_package_id
            .as_deref()
            .is_some_and(|selected| !resolved_ids.contains(selected))
        {
            return Err(RustCargoFactsAdmissionError::Shape);
        }
        Ok(())
    }

    /// Builds an owner-bound target selection from this exact metadata response.
    pub fn select_target(
        &self,
        package_id: &str,
        target_name: &str,
        profile: RustCargoBuildProfileV1,
        default_features: bool,
        features: &[Box<str>],
    ) -> Result<RustCargoProfileSelectionV1, RustCargoProfileRequestError> {
        self.admit()
            .map_err(|_| RustCargoProfileRequestError::InvalidFacts)?;
        if self.requested_features.all_features
            || self.requested_features.no_default_features == default_features
            || self.requested_features.features.as_ref() != features
        {
            return Err(RustCargoProfileRequestError::FeatureResolutionMismatch);
        }
        let package = self
            .workspace_packages
            .iter()
            .find(|package| package.package_id.as_ref() == package_id)
            .ok_or(RustCargoProfileRequestError::UnknownPackage)?;
        let mut targets = package
            .targets
            .iter()
            .filter(|target| target.name.as_ref() == target_name);
        let target = targets
            .next()
            .ok_or(RustCargoProfileRequestError::UnknownTarget)?;
        if targets.next().is_some() {
            return Err(RustCargoProfileRequestError::AmbiguousTarget);
        }
        if target
            .kinds
            .iter()
            .any(|kind| matches!(kind.as_ref(), "test" | "bench"))
        {
            return Err(RustCargoProfileRequestError::TargetCfgUnsupported);
        }
        let declared = package
            .features
            .iter()
            .map(|feature| feature.name.as_ref())
            .collect::<std::collections::BTreeSet<_>>();
        let mut selected = features.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        if selected.len() > MAX_RUST_CARGO_PROFILE_FEATURES {
            return Err(RustCargoProfileRequestError::FeatureBound);
        }
        selected.sort_unstable();
        if selected.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RustCargoProfileRequestError::DuplicateFeature);
        }
        if selected.iter().any(|feature| !declared.contains(feature)) {
            return Err(RustCargoProfileRequestError::UnknownFeature);
        }
        let resolved = self
            .resolved_packages
            .iter()
            .find(|resolved| resolved.package_id.as_ref() == package_id)
            .ok_or(RustCargoProfileRequestError::UnresolvedPackage)?;
        if target.required_features.iter().any(|required| {
            !resolved
                .active_features
                .iter()
                .any(|active| active == required)
        }) {
            return Err(RustCargoProfileRequestError::TargetRequiredFeaturesUnavailable);
        }
        let target_digest = rust_cargo_target_digest(package, target);
        Ok(RustCargoProfileSelectionV1 {
            package_id: package.package_id.clone(),
            package_name: package.name.clone(),
            target_name: target.name.clone(),
            target_digest,
            metadata_response_digest: self.metadata_response_digest,
            resolved_lockfile_digest: self.resolved_lockfile_digest,
            toolchain_binding_digest: self.toolchain_binding_digest,
            profile,
            default_features,
            features: selected
                .into_iter()
                .map(str::to_owned)
                .map(String::into_boxed_str)
                .collect(),
        })
    }
}

/// Built-in Cargo profile supported by the first owner apply path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RustCargoBuildProfileV1 {
    /// Cargo's `dev` profile.
    Dev,
    /// Cargo's `release` profile.
    Release,
}

/// Exact target/profile/features selection made against one admitted metadata response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCargoProfileSelectionV1 {
    /// Cargo package id returned by the exact metadata response.
    pub package_id: Box<str>,
    /// Cargo package name returned by that metadata response.
    pub package_name: Box<str>,
    /// Exact target name returned by that metadata response.
    pub target_name: Box<str>,
    /// Digest of all target identity fields, including source path and required features.
    pub target_digest: [u8; 32],
    /// Cargo metadata bytes against which the target was selected.
    pub metadata_response_digest: [u8; 32],
    /// Resolved lockfile bytes against which dependencies were selected.
    pub resolved_lockfile_digest: [u8; 32],
    /// Exact admitted Rust/Cargo/sysroot path binding.
    pub toolchain_binding_digest: [u8; 32],
    /// Requested built-in Cargo profile.
    pub profile: RustCargoBuildProfileV1,
    /// Whether Cargo should include the package's default feature set.
    pub default_features: bool,
    /// Exact enabled package features, sorted and unique.
    pub features: Box<[Box<str>]>,
}

impl RustCargoProfileSelectionV1 {
    /// Revalidates this selection against newly read full Cargo metadata.
    pub fn validate_against(
        &self,
        facts: &RustCargoWorkspaceFactsV1,
    ) -> Result<(), RustCargoProfileRequestError> {
        let current = facts.select_target(
            &self.package_id,
            &self.target_name,
            self.profile,
            self.default_features,
            &self.features,
        )?;
        if current == *self {
            Ok(())
        } else {
            Err(RustCargoProfileRequestError::StaleFacts)
        }
    }

    /// Returns a canonical digest suitable for compiler recipe and session identity binding.
    #[must_use]
    pub fn identity_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key("backend.rust-cargo-profile-selection.v1");
        update_text(&mut hasher, &self.package_id);
        update_text(&mut hasher, &self.package_name);
        update_text(&mut hasher, &self.target_name);
        hasher.update(&self.target_digest);
        hasher.update(&self.metadata_response_digest);
        hasher.update(&self.resolved_lockfile_digest);
        hasher.update(&self.toolchain_binding_digest);
        hasher.update(&[match self.profile {
            RustCargoBuildProfileV1::Dev => 0,
            RustCargoBuildProfileV1::Release => 1,
        }]);
        hasher.update(&[u8::from(self.default_features)]);
        hasher.update(&(self.features.len() as u64).to_be_bytes());
        for feature in &self.features {
            update_text(&mut hasher, feature);
        }
        *hasher.finalize().as_bytes()
    }
}

/// Why a profile request could not be admitted against the current Cargo facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustCargoProfileRequestError {
    /// The facts did not pass their own bounded admission.
    InvalidFacts,
    /// The selected package id is not in the workspace member inventory.
    UnknownPackage,
    /// The selected target name is not declared by the package.
    UnknownTarget,
    /// More than one Cargo target has the requested name.
    AmbiguousTarget,
    /// Test/bench target cfg is not currently target-scoped in the RA adapter.
    TargetCfgUnsupported,
    /// The requested feature list exceeds the fixed bound.
    FeatureBound,
    /// The requested feature list repeats a name.
    DuplicateFeature,
    /// A requested feature is not declared by the selected package.
    UnknownFeature,
    /// Full Cargo resolution did not produce the selected workspace package node.
    UnresolvedPackage,
    /// The target's `required-features` are not active in the admitted resolution.
    TargetRequiredFeaturesUnavailable,
    /// The full metadata resolution was produced for another feature request.
    FeatureResolutionMismatch,
    /// The metadata, lockfile, toolchain, or target facts have moved since selection.
    StaleFacts,
}

/// Why Cargo facts could not cross the product surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustCargoFactsAdmissionError {
    /// A field, required root, or resolved edge is malformed.
    Shape,
    /// A collection exceeds its fixed protocol bound.
    Bound,
    /// A collection is not in canonical sorted unique order.
    Ordering,
    /// Full resolution omitted a declared workspace member.
    WorkspaceMemberUnresolved,
    /// A text field exceeds its protocol bound or is empty.
    Text,
}

fn rust_cargo_target_digest(
    package: &RustCargoWorkspacePackageFactV1,
    target: &RustCargoTargetFactV1,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("backend.rust-cargo-target-fact.v1");
    update_text(&mut hasher, &package.package_id);
    update_text(&mut hasher, &package.name);
    update_text(&mut hasher, &package.version);
    update_text(&mut hasher, &package.manifest_path.to_string_lossy());
    update_text(&mut hasher, &target.name);
    update_text(&mut hasher, &target.source_path.to_string_lossy());
    update_text(&mut hasher, &target.edition);
    for values in [
        &target.kinds,
        &target.crate_types,
        &target.required_features,
    ] {
        hasher.update(&(values.len() as u64).to_be_bytes());
        for value in values.iter() {
            update_text(&mut hasher, value);
        }
    }
    hasher.update(&[
        u8::from(target.test),
        u8::from(target.doctest),
        u8::from(target.doc),
    ]);
    *hasher.finalize().as_bytes()
}

fn check_text(value: &str) -> Result<(), RustCargoFactsAdmissionError> {
    if value.is_empty() || value.len() > MAX_RUST_CARGO_METADATA_TEXT_BYTES {
        Err(RustCargoFactsAdmissionError::Text)
    } else {
        Ok(())
    }
}

fn check_sorted_text<'a>(
    values: impl Iterator<Item = &'a str>,
) -> Result<(), RustCargoFactsAdmissionError> {
    let mut previous = None;
    for value in values {
        check_text(value)?;
        if previous.is_some_and(|prior| prior >= value) {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }
        previous = Some(value);
    }
    Ok(())
}

fn check_sorted_pairs<'a>(
    values: impl Iterator<Item = (&'a str, &'a str)>,
) -> Result<(), RustCargoFactsAdmissionError> {
    let mut previous = None;
    for value in values {
        check_text(value.0)?;
        check_text(value.1)?;
        if previous.is_some_and(|prior| prior >= value) {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }
        previous = Some(value);
    }
    Ok(())
}

fn update_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_facts() -> RustCargoWorkspaceFactsV1 {
        RustCargoWorkspaceFactsV1 {
            manifest_path: PathBuf::from("/workspace/Cargo.toml"),
            workspace_root: PathBuf::from("/workspace"),
            selected_package_id: Some("path+file:///workspace#fixture@1.0.0".into()),
            requested_features: RustCargoFeatureSelectionV1 {
                all_features: false,
                no_default_features: false,
                features: Box::new([]),
            },
            workspace_packages: vec![RustCargoWorkspacePackageFactV1 {
                package_id: "path+file:///workspace#fixture@1.0.0".into(),
                name: "fixture".into(),
                version: "1.0.0".into(),
                manifest_path: PathBuf::from("/workspace/Cargo.toml"),
                features: vec![RustCargoFeatureFactV1 {
                    name: "default".into(),
                    enables: Box::new([]),
                }]
                .into_boxed_slice(),
                targets: vec![RustCargoTargetFactV1 {
                    name: "fixture".into(),
                    kinds: vec!["lib".into()].into_boxed_slice(),
                    crate_types: vec!["lib".into()].into_boxed_slice(),
                    source_path: PathBuf::from("/workspace/src/lib.rs"),
                    required_features: Box::new([]),
                    edition: "2024".into(),
                    test: true,
                    doctest: true,
                    doc: true,
                }]
                .into_boxed_slice(),
            }]
            .into_boxed_slice(),
            resolved_packages: vec![RustCargoResolvedPackageFactV1 {
                package_id: "path+file:///workspace#fixture@1.0.0".into(),
                active_features: vec!["default".into()].into_boxed_slice(),
                dependencies: Box::new([]),
            }]
            .into_boxed_slice(),
            metadata_response_digest: [1; 32],
            resolved_lockfile_digest: [2; 32],
            toolchain_binding_digest: [3; 32],
        }
    }

    #[test]
    fn target_selection_binds_all_exact_facts_and_profile_inputs() {
        let facts = fixture_facts();
        let first = facts
            .select_target(
                "path+file:///workspace#fixture@1.0.0",
                "fixture",
                RustCargoBuildProfileV1::Dev,
                true,
                &[],
            )
            .expect("valid exact target request");
        first
            .validate_against(&facts)
            .expect("selection remains current");

        let release = RustCargoProfileSelectionV1 {
            profile: RustCargoBuildProfileV1::Release,
            ..first.clone()
        };
        assert_ne!(first.identity_digest(), release.identity_digest());
        let changed_lock = RustCargoProfileSelectionV1 {
            resolved_lockfile_digest: [9; 32],
            ..first.clone()
        };
        assert_eq!(
            changed_lock.validate_against(&facts),
            Err(RustCargoProfileRequestError::StaleFacts)
        );
    }

    #[test]
    fn full_facts_refuse_workspace_member_missing_from_resolution_nodes() {
        let mut facts = fixture_facts();
        facts.resolved_packages[0].package_id =
            "registry+https://example.invalid#other@1.0.0".into();

        assert_eq!(
            facts.admit(),
            Err(RustCargoFactsAdmissionError::WorkspaceMemberUnresolved)
        );
    }

    #[test]
    fn profile_selection_refuses_unresolved_target_requirements_and_test_cfg() {
        let mut facts = fixture_facts();
        facts.workspace_packages[0].targets[0].required_features =
            vec!["extra".into()].into_boxed_slice();
        assert_eq!(
            facts.select_target(
                "path+file:///workspace#fixture@1.0.0",
                "fixture",
                RustCargoBuildProfileV1::Dev,
                true,
                &[],
            ),
            Err(RustCargoProfileRequestError::TargetRequiredFeaturesUnavailable)
        );

        let mut facts = fixture_facts();
        facts.workspace_packages[0].targets[0].kinds = vec!["test".into()].into_boxed_slice();
        assert_eq!(
            facts.select_target(
                "path+file:///workspace#fixture@1.0.0",
                "fixture",
                RustCargoBuildProfileV1::Dev,
                true,
                &[],
            ),
            Err(RustCargoProfileRequestError::TargetCfgUnsupported)
        );
    }
    #[test]
    fn empty_resolution_is_malformed_before_member_checks() {
        let mut facts = fixture_facts();
        facts.resolved_packages = Box::new([]);
        assert_eq!(facts.admit(), Err(RustCargoFactsAdmissionError::Shape));
    }

    #[test]
    fn ordinary_library_test_flag_does_not_select_test_cfg() {
        for test_enabled in [false, true] {
            let mut facts = fixture_facts();
            facts.workspace_packages[0].targets[0].test = test_enabled;
            facts
                .select_target(
                    "path+file:///workspace#fixture@1.0.0",
                    "fixture",
                    RustCargoBuildProfileV1::Dev,
                    true,
                    &[],
                )
                .expect("Cargo test availability is independent of this library target's cfg");
        }
    }

    #[test]
    fn benchmark_target_requires_target_specific_cfg() {
        let mut facts = fixture_facts();
        facts.workspace_packages[0].targets[0].kinds = vec!["bench".into()].into_boxed_slice();
        assert_eq!(
            facts.select_target(
                "path+file:///workspace#fixture@1.0.0",
                "fixture",
                RustCargoBuildProfileV1::Dev,
                true,
                &[],
            ),
            Err(RustCargoProfileRequestError::TargetCfgUnsupported),
        );
    }
}
