//! Bounded Cargo facts and profile-selection request contracts shared by the
//! owner, compiler frontend, CLI, MCP, and desktop surfaces.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Maximum Cargo workspace member rows retained in one facts response.
pub const MAX_RUST_CARGO_WORKSPACE_PACKAGES: usize = 1_024;
/// Maximum declared feature names, enabling edges, and required-feature entries.
pub const MAX_RUST_CARGO_FEATURE_EDGES: usize = 65_536;
/// Maximum active Cargo feature names retained across the resolved graph.
pub const MAX_RUST_CARGO_ACTIVE_FEATURES: usize = 65_536;
/// Maximum Cargo target rows retained in one facts response.
pub const MAX_RUST_CARGO_TARGETS: usize = 8_192;
/// Maximum resolved package rows retained in one facts response.
pub const MAX_RUST_CARGO_RESOLVED_PACKAGES: usize = 16_384;
/// Maximum resolved dependency edges retained in one facts response.
pub const MAX_RUST_CARGO_DEPENDENCY_EDGES: usize = 131_072;
/// Maximum dependency-kind rows retained across the resolved graph.
pub const MAX_RUST_CARGO_DEPENDENCY_KINDS: usize = 131_072;
/// Maximum text bytes retained across one Cargo facts response.
pub const MAX_RUST_CARGO_TOTAL_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum collection entries visited while constructing or admitting one response.
pub const MAX_RUST_CARGO_FACT_ENTRIES: usize = 524_288;
/// Maximum individual text values retained across one Cargo facts response.
pub const MAX_RUST_CARGO_FACT_TEXT_ENTRIES: usize = 262_144;
/// Maximum caller-selected feature names in one profile request.
pub const MAX_RUST_CARGO_PROFILE_FEATURES: usize = 256;
/// Maximum individual Cargo metadata spelling admitted to a product DTO.
pub const MAX_RUST_CARGO_METADATA_TEXT_BYTES: usize = 4_096;

/// Tracks the aggregate size of DTOs derived from Cargo's already-bounded
/// metadata stream. Every derived collection and string is admitted before a
/// corresponding allocation or clone is made.
#[derive(Default)]
struct CargoFactsBudget {
    entries: usize,
    text_entries: usize,
    text_bytes: usize,
    feature_edges: usize,
    active_features: usize,
    targets: usize,
    dependency_edges: usize,
    dependency_kinds: usize,
}

impl CargoFactsBudget {
    fn reserve_entries(&mut self, count: usize) -> Result<(), RustCargoFactsAdmissionError> {
        let next = self
            .entries
            .checked_add(count)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next > MAX_RUST_CARGO_FACT_ENTRIES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.entries = next;
        Ok(())
    }

    fn reserve_features(&mut self, count: usize) -> Result<(), RustCargoFactsAdmissionError> {
        let next = self
            .feature_edges
            .checked_add(count)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next > MAX_RUST_CARGO_FEATURE_EDGES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.feature_edges = next;
        Ok(())
    }

    fn reserve_active_features(
        &mut self,
        count: usize,
    ) -> Result<(), RustCargoFactsAdmissionError> {
        let next = self
            .active_features
            .checked_add(count)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next > MAX_RUST_CARGO_ACTIVE_FEATURES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.active_features = next;
        Ok(())
    }

    fn reserve_targets(&mut self, count: usize) -> Result<(), RustCargoFactsAdmissionError> {
        let next = self
            .targets
            .checked_add(count)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next > MAX_RUST_CARGO_TARGETS {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.targets = next;
        Ok(())
    }

    fn reserve_dependencies(&mut self, count: usize) -> Result<(), RustCargoFactsAdmissionError> {
        let next = self
            .dependency_edges
            .checked_add(count)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next > MAX_RUST_CARGO_DEPENDENCY_EDGES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.dependency_edges = next;
        Ok(())
    }

    fn reserve_dependency_kinds(
        &mut self,
        count: usize,
    ) -> Result<(), RustCargoFactsAdmissionError> {
        let next = self
            .dependency_kinds
            .checked_add(count)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next > MAX_RUST_CARGO_DEPENDENCY_KINDS {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.dependency_kinds = next;
        Ok(())
    }

    fn retain_text(&mut self, value: &str) -> Result<(), RustCargoFactsAdmissionError> {
        check_text(value)?;
        let next_entries = self
            .text_entries
            .checked_add(1)
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        let next_bytes = self
            .text_bytes
            .checked_add(value.len())
            .ok_or(RustCargoFactsAdmissionError::Bound)?;
        if next_entries > MAX_RUST_CARGO_FACT_TEXT_ENTRIES
            || next_bytes > MAX_RUST_CARGO_TOTAL_TEXT_BYTES
        {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        self.text_entries = next_entries;
        self.text_bytes = next_bytes;
        Ok(())
    }

    fn copy_text(&mut self, value: &str) -> Result<Box<str>, RustCargoFactsAdmissionError> {
        self.retain_text(value)?;
        Ok(value.into())
    }

    fn copy_path(&mut self, value: &str) -> Result<PathBuf, RustCargoFactsAdmissionError> {
        if !std::path::Path::new(value).is_absolute() {
            return Err(RustCargoFactsAdmissionError::Shape);
        }
        self.retain_text(value)?;
        Ok(PathBuf::from(value))
    }

    fn retain_path(&mut self, value: &PathBuf) -> Result<(), RustCargoFactsAdmissionError> {
        if !value.is_absolute() {
            return Err(RustCargoFactsAdmissionError::Shape);
        }
        if value.as_os_str().len() > MAX_RUST_CARGO_METADATA_TEXT_BYTES {
            return Err(RustCargoFactsAdmissionError::Text);
        }
        let lossy = value.to_string_lossy();
        self.retain_text(&lossy)
    }
}

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
/// metadata response, lockfile, configuration snapshot, toolchain invocation,
/// and feature request. They do not
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
    /// Digest of the exact Cargo invocation, configuration snapshot, compiler/Cargo
    /// executable paths, sysroot, selected feature request, and wrapper environment.
    pub toolchain_binding_digest: [u8; 32],
}

impl RustCargoWorkspaceFactsV1 {
    /// Builds canonical bounded facts from the exact full metadata value that
    /// the owner already admitted. This does not run Cargo or reread a
    /// manifest; callers supply the exact request and byte digests from that
    /// same preflight transaction.
    pub fn from_metadata_value(
        metadata: &serde_json::Value,
        manifest_path: PathBuf,
        mut requested_features: RustCargoFeatureSelectionV1,
        metadata_response_digest: [u8; 32],
        resolved_lockfile_digest: [u8; 32],
        toolchain_binding_digest: [u8; 32],
    ) -> Result<Self, RustCargoFactsAdmissionError> {
        let workspace_root = required_text_field(metadata, "workspace_root")?;
        let mut budget = CargoFactsBudget::default();
        budget.retain_path(&manifest_path)?;
        let workspace_root = budget.copy_path(workspace_root)?;
        if requested_features.features.len() > MAX_RUST_CARGO_PROFILE_FEATURES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        budget.reserve_entries(requested_features.features.len())?;
        for feature in &requested_features.features {
            budget.retain_text(feature)?;
        }
        requested_features.features.sort_unstable();
        if requested_features
            .features
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }

        let packages_value = required_array_field(metadata, "packages")?;
        if packages_value.len() > MAX_RUST_CARGO_RESOLVED_PACKAGES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        budget.reserve_entries(packages_value.len())?;
        let mut packages_by_id = BTreeMap::new();
        for package in packages_value {
            let id = required_text_field(package, "id")?;
            check_text(id)?;
            if packages_by_id.insert(id, package).is_some() {
                return Err(RustCargoFactsAdmissionError::Ordering);
            }
        }

        let workspace_members = required_array_field(metadata, "workspace_members")?;
        if workspace_members.is_empty()
            || workspace_members.len() > MAX_RUST_CARGO_WORKSPACE_PACKAGES
        {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        budget.reserve_entries(workspace_members.len())?;
        let mut member_ids = Vec::with_capacity(workspace_members.len());
        for member in workspace_members {
            let member_id = required_text_ref(member)?;
            check_text(member_id)?;
            member_ids.push(member_id);
        }
        member_ids.sort_unstable();
        if member_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }

        budget.reserve_entries(member_ids.len())?;
        let mut workspace_packages = Vec::with_capacity(member_ids.len());
        let mut selected_candidate = None;
        let mut selected_is_ambiguous = false;
        for package_id in &member_ids {
            let package = packages_by_id
                .get(package_id)
                .copied()
                .ok_or(RustCargoFactsAdmissionError::WorkspaceMemberUnresolved)?;
            let package_manifest = absolute_path_field(package, "manifest_path", &mut budget)?;
            if package_manifest == manifest_path {
                if selected_candidate.replace(*package_id).is_some() {
                    selected_is_ambiguous = true;
                }
            }
            let package_id = budget.copy_text(package_id)?;
            let name = budget.copy_text(required_text_field(package, "name")?)?;
            let version = budget.copy_text(required_text_field(package, "version")?)?;
            let features = parse_declared_features(package, &mut budget)?;
            let targets = parse_targets(package, &mut budget)?;
            workspace_packages.push(RustCargoWorkspacePackageFactV1 {
                package_id,
                name,
                version,
                manifest_path: package_manifest,
                features,
                targets,
            });
        }
        let selected_package_id = match (selected_candidate, selected_is_ambiguous) {
            (Some(selected), false) => Some(budget.copy_text(selected)?),
            _ => None,
        };

        let nodes = required_array_field(
            metadata
                .get("resolve")
                .filter(|resolve| !resolve.is_null())
                .ok_or(RustCargoFactsAdmissionError::Shape)?,
            "nodes",
        )?;
        if nodes.is_empty() || nodes.len() > MAX_RUST_CARGO_RESOLVED_PACKAGES {
            return Err(RustCargoFactsAdmissionError::Bound);
        }
        budget.reserve_entries(nodes.len())?;
        let mut resolved_packages = Vec::with_capacity(nodes.len());
        for node in nodes {
            let package_id = budget.copy_text(required_text_field(node, "id")?)?;
            let active_features_value = required_array_field(node, "features")?;
            budget.reserve_active_features(active_features_value.len())?;
            let active_features = canonical_text_array(
                active_features_value,
                MAX_RUST_CARGO_FEATURE_EDGES,
                &mut budget,
            )?;
            let dependencies_value = required_array_field(node, "deps")?;
            budget.reserve_dependencies(dependencies_value.len())?;
            budget.reserve_entries(dependencies_value.len())?;
            let mut dependencies = Vec::with_capacity(dependencies_value.len());
            for dependency in dependencies_value {
                let name = budget.copy_text(required_text_field(dependency, "name")?)?;
                let package_id = budget.copy_text(required_text_field(dependency, "pkg")?)?;
                let kinds_value = required_array_field(dependency, "dep_kinds")?;
                budget.reserve_dependency_kinds(kinds_value.len())?;
                budget.reserve_entries(kinds_value.len())?;
                let mut kinds = Vec::with_capacity(kinds_value.len());
                for kind in kinds_value {
                    kinds.push(RustCargoDependencyKindFactV1 {
                        kind: optional_bounded_text_field(kind, "kind", &mut budget)?,
                        target: optional_bounded_text_field(kind, "target", &mut budget)?,
                    });
                }
                kinds.sort_unstable_by(|left, right| {
                    (&left.kind, &left.target).cmp(&(&right.kind, &right.target))
                });
                dependencies.push(RustCargoResolvedDependencyFactV1 {
                    name,
                    package_id,
                    kinds: kinds.into_boxed_slice(),
                });
            }
            dependencies.sort_unstable_by(|left, right| {
                (&left.name, &left.package_id).cmp(&(&right.name, &right.package_id))
            });
            resolved_packages.push(RustCargoResolvedPackageFactV1 {
                package_id,
                active_features,
                dependencies: dependencies.into_boxed_slice(),
            });
        }
        resolved_packages.sort_unstable_by(|left, right| left.package_id.cmp(&right.package_id));

        let facts = Self {
            manifest_path,
            workspace_root,
            selected_package_id,
            requested_features,
            workspace_packages: workspace_packages.into_boxed_slice(),
            resolved_packages: resolved_packages.into_boxed_slice(),
            metadata_response_digest,
            resolved_lockfile_digest,
            toolchain_binding_digest,
        };
        facts.admit()?;
        Ok(facts)
    }

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
        let mut budget = CargoFactsBudget::default();
        budget.retain_path(&self.manifest_path)?;
        budget.retain_path(&self.workspace_root)?;
        budget.reserve_entries(self.requested_features.features.len())?;
        for feature in &self.requested_features.features {
            budget.retain_text(feature)?;
        }
        check_sorted_text(self.requested_features.features.iter().map(AsRef::as_ref))?;

        budget.reserve_entries(self.workspace_packages.len())?;
        let mut package_ids = std::collections::BTreeSet::new();
        for package in &self.workspace_packages {
            budget.retain_text(&package.package_id)?;
            budget.retain_text(&package.name)?;
            budget.retain_text(&package.version)?;
            budget.retain_path(&package.manifest_path)?;
            if !package.manifest_path.is_absolute()
                || package.features.is_empty() && package.targets.is_empty()
            {
                return Err(RustCargoFactsAdmissionError::Shape);
            }
            if !package_ids.insert(package.package_id.as_ref()) {
                return Err(RustCargoFactsAdmissionError::Ordering);
            }
            budget.reserve_features(package.features.len())?;
            budget.reserve_entries(package.features.len())?;
            for feature in &package.features {
                budget.retain_text(&feature.name)?;
                budget.reserve_features(feature.enables.len())?;
                budget.reserve_entries(feature.enables.len())?;
                for value in &feature.enables {
                    budget.retain_text(value)?;
                }
                check_sorted_text(feature.enables.iter().map(AsRef::as_ref))?;
            }
            check_sorted_text(package.features.iter().map(|feature| feature.name.as_ref()))?;
            budget.reserve_targets(package.targets.len())?;
            budget.reserve_entries(package.targets.len())?;
            if !package.targets.windows(2).all(|pair| {
                (&pair[0].name, &pair[0].source_path, &pair[0].kinds)
                    < (&pair[1].name, &pair[1].source_path, &pair[1].kinds)
            }) {
                return Err(RustCargoFactsAdmissionError::Ordering);
            }
            for target in &package.targets {
                budget.retain_text(&target.name)?;
                budget.retain_text(&target.edition)?;
                budget.retain_path(&target.source_path)?;
                if !target.source_path.is_absolute()
                    || target.kinds.is_empty()
                    || target.crate_types.is_empty()
                {
                    return Err(RustCargoFactsAdmissionError::Shape);
                }
                budget.reserve_entries(target.kinds.len())?;
                budget.reserve_entries(target.crate_types.len())?;
                budget.reserve_entries(target.required_features.len())?;
                for value in target.kinds.iter().chain(target.crate_types.iter()) {
                    budget.retain_text(value)?;
                }
                budget.reserve_features(target.required_features.len())?;
                for value in &target.required_features {
                    budget.retain_text(value)?;
                }
                check_sorted_text(target.kinds.iter().map(AsRef::as_ref))?;
                check_sorted_text(target.crate_types.iter().map(AsRef::as_ref))?;
                check_sorted_text(target.required_features.iter().map(AsRef::as_ref))?;
            }
        }
        if !self
            .workspace_packages
            .windows(2)
            .all(|pair| pair[0].package_id < pair[1].package_id)
        {
            return Err(RustCargoFactsAdmissionError::Ordering);
        }
        if let Some(selected) = &self.selected_package_id {
            budget.retain_text(selected)?;
            if !package_ids.contains(selected.as_ref()) {
                return Err(RustCargoFactsAdmissionError::Shape);
            }
        }

        budget.reserve_entries(self.resolved_packages.len())?;
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
        for package in &self.resolved_packages {
            budget.retain_text(&package.package_id)?;
            budget.reserve_active_features(package.active_features.len())?;
            budget.reserve_entries(package.active_features.len())?;
            for feature in &package.active_features {
                budget.retain_text(feature)?;
            }
            check_sorted_text(package.active_features.iter().map(AsRef::as_ref))?;
            budget.reserve_dependencies(package.dependencies.len())?;
            budget.reserve_entries(package.dependencies.len())?;
            check_sorted_pairs(
                package
                    .dependencies
                    .iter()
                    .map(|edge| (edge.name.as_ref(), edge.package_id.as_ref())),
            )?;
            for dependency in &package.dependencies {
                budget.retain_text(&dependency.name)?;
                budget.retain_text(&dependency.package_id)?;
                if !resolved_ids.contains(dependency.package_id.as_ref()) {
                    return Err(RustCargoFactsAdmissionError::Shape);
                }
                budget.reserve_dependency_kinds(dependency.kinds.len())?;
                budget.reserve_entries(dependency.kinds.len())?;
                if !dependency
                    .kinds
                    .windows(2)
                    .all(|pair| (&pair[0].kind, &pair[0].target) < (&pair[1].kind, &pair[1].target))
                {
                    return Err(RustCargoFactsAdmissionError::Ordering);
                }
                for kind in &dependency.kinds {
                    if let Some(value) = &kind.kind {
                        budget.retain_text(value)?;
                    }
                    if let Some(value) = &kind.target {
                        budget.retain_text(value)?;
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

fn required_array_field<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a [serde_json::Value], RustCargoFactsAdmissionError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or(RustCargoFactsAdmissionError::Shape)
}

fn required_text_field<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str, RustCargoFactsAdmissionError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(RustCargoFactsAdmissionError::Shape)
}

fn required_text_ref(value: &serde_json::Value) -> Result<&str, RustCargoFactsAdmissionError> {
    value.as_str().ok_or(RustCargoFactsAdmissionError::Shape)
}

fn absolute_path_field(
    value: &serde_json::Value,
    field: &str,
    budget: &mut CargoFactsBudget,
) -> Result<PathBuf, RustCargoFactsAdmissionError> {
    budget.copy_path(required_text_field(value, field)?)
}

fn canonical_text_array(
    values: &[serde_json::Value],
    maximum: usize,
    budget: &mut CargoFactsBudget,
) -> Result<Box<[Box<str>]>, RustCargoFactsAdmissionError> {
    if values.len() > maximum {
        return Err(RustCargoFactsAdmissionError::Bound);
    }
    budget.reserve_entries(values.len())?;
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        result.push(budget.copy_text(required_text_ref(value)?)?);
    }
    result.sort_unstable();
    Ok(result.into_boxed_slice())
}

fn parse_declared_features(
    package: &serde_json::Value,
    budget: &mut CargoFactsBudget,
) -> Result<Box<[RustCargoFeatureFactV1]>, RustCargoFactsAdmissionError> {
    let features = package
        .get("features")
        .and_then(serde_json::Value::as_object)
        .ok_or(RustCargoFactsAdmissionError::Shape)?;
    if features.len() > MAX_RUST_CARGO_FEATURE_EDGES {
        return Err(RustCargoFactsAdmissionError::Bound);
    }
    budget.reserve_features(features.len())?;
    budget.reserve_entries(features.len())?;
    let mut result = Vec::with_capacity(features.len());
    for (name, enables) in features {
        let name = budget.copy_text(name)?;
        let enables = enables
            .as_array()
            .ok_or(RustCargoFactsAdmissionError::Shape)?;
        budget.reserve_features(enables.len())?;
        let enables = canonical_text_array(enables, MAX_RUST_CARGO_FEATURE_EDGES, budget)?;
        result.push(RustCargoFeatureFactV1 { name, enables });
    }
    result.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(result.into_boxed_slice())
}

fn parse_targets(
    package: &serde_json::Value,
    budget: &mut CargoFactsBudget,
) -> Result<Box<[RustCargoTargetFactV1]>, RustCargoFactsAdmissionError> {
    let targets = required_array_field(package, "targets")?;
    if targets.len() > MAX_RUST_CARGO_TARGETS {
        return Err(RustCargoFactsAdmissionError::Bound);
    }
    budget.reserve_targets(targets.len())?;
    budget.reserve_entries(targets.len())?;
    let mut result = Vec::with_capacity(targets.len());
    for target in targets {
        let kinds = canonical_text_array(
            required_array_field(target, "kind")?,
            MAX_RUST_CARGO_TARGETS,
            budget,
        )?;
        let crate_types = canonical_text_array(
            required_array_field(target, "crate_types")?,
            MAX_RUST_CARGO_TARGETS,
            budget,
        )?;
        let required_features = match target.get("required-features") {
            None => Box::new([]),
            Some(value) => {
                let values = value
                    .as_array()
                    .ok_or(RustCargoFactsAdmissionError::Shape)?;
                budget.reserve_features(values.len())?;
                canonical_text_array(values, MAX_RUST_CARGO_FEATURE_EDGES, budget)?
            }
        };
        result.push(RustCargoTargetFactV1 {
            name: budget.copy_text(required_text_field(target, "name")?)?,
            kinds,
            crate_types,
            source_path: absolute_path_field(target, "src_path", budget)?,
            required_features,
            edition: budget.copy_text(required_text_field(target, "edition")?)?,
            test: required_bool_field(target, "test")?,
            doctest: required_bool_field(target, "doctest")?,
            doc: required_bool_field(target, "doc")?,
        });
    }
    result.sort_unstable_by(|left, right| {
        (&left.name, &left.source_path, &left.kinds).cmp(&(
            &right.name,
            &right.source_path,
            &right.kinds,
        ))
    });
    Ok(result.into_boxed_slice())
}

fn required_bool_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<bool, RustCargoFactsAdmissionError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .ok_or(RustCargoFactsAdmissionError::Shape)
}

fn optional_bounded_text_field(
    value: &serde_json::Value,
    field: &str,
    budget: &mut CargoFactsBudget,
) -> Result<Option<Box<str>>, RustCargoFactsAdmissionError> {
    match value.get(field) {
        Some(value) if value.is_null() => Ok(None),
        Some(value) => value
            .as_str()
            .ok_or(RustCargoFactsAdmissionError::Shape)
            .and_then(|text| budget.copy_text(text).map(Some)),
        None => Ok(None),
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
    /// Exact admitted Cargo invocation, configuration, and toolchain binding.
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

    fn metadata_fixture() -> serde_json::Value {
        serde_json::json!({
            "workspace_root": "/workspace",
            "workspace_members": [
                "path+file:///workspace/crates/a#a@1.0.0",
                "path+file:///workspace/crates/b#b@1.0.0"
            ],
            "packages": [
                {
                    "id": "path+file:///workspace/crates/a#a@1.0.0",
                    "name": "a",
                    "version": "1.0.0",
                    "manifest_path": "/workspace/crates/a/Cargo.toml",
                    "features": {"default": []},
                    "targets": [{
                        "name": "a_lib",
                        "kind": ["lib"],
                        "crate_types": ["lib"],
                        "src_path": "/workspace/crates/a/src/lib.rs",
                        "edition": "2024",
                        "test": true,
                        "doctest": true,
                        "doc": true
                    }]
                },
                {
                    "id": "path+file:///workspace/crates/b#b@1.0.0",
                    "name": "b",
                    "version": "1.0.0",
                    "manifest_path": "/workspace/crates/b/Cargo.toml",
                    "features": {"default": []},
                    "targets": [{
                        "name": "b_bin",
                        "kind": ["bin"],
                        "crate_types": ["bin"],
                        "src_path": "/workspace/crates/b/src/main.rs",
                        "edition": "2021",
                        "test": true,
                        "doctest": false,
                        "doc": true
                    }]
                }
            ],
            "resolve": {"nodes": [
                {
                    "id": "path+file:///workspace/crates/a#a@1.0.0",
                    "features": ["default"],
                    "deps": []
                },
                {
                    "id": "path+file:///workspace/crates/b#b@1.0.0",
                    "features": ["default"],
                    "deps": []
                }
            ]}
        })
    }

    #[test]
    fn metadata_parser_keeps_virtual_root_unselected_and_selects_only_exact_member() {
        let metadata = metadata_fixture();
        let make_facts = |manifest_path| {
            RustCargoWorkspaceFactsV1::from_metadata_value(
                &metadata,
                PathBuf::from(manifest_path),
                RustCargoFeatureSelectionV1 {
                    all_features: false,
                    no_default_features: false,
                    features: Box::new([]),
                },
                [1; 32],
                [2; 32],
                [3; 32],
            )
            .expect("bounded Cargo metadata fixture")
        };

        let virtual_root = make_facts("/workspace/Cargo.toml");
        assert_eq!(virtual_root.selected_package_id, None);
        assert_eq!(virtual_root.workspace_packages.len(), 2);

        let member = make_facts("/workspace/crates/a/Cargo.toml");
        assert_eq!(
            member.selected_package_id.as_deref(),
            Some("path+file:///workspace/crates/a#a@1.0.0")
        );
        assert_eq!(
            member.workspace_packages[0].targets[0].name.as_ref(),
            "a_lib"
        );
    }

    #[test]
    fn metadata_parser_rejects_aggregate_feature_and_dependency_kind_overflow_before_copying() {
        let make_facts = |metadata: &serde_json::Value| {
            RustCargoWorkspaceFactsV1::from_metadata_value(
                metadata,
                PathBuf::from("/workspace/Cargo.toml"),
                RustCargoFeatureSelectionV1 {
                    all_features: false,
                    no_default_features: false,
                    features: Box::new([]),
                },
                [1; 32],
                [2; 32],
                [3; 32],
            )
        };

        let mut too_many_features = metadata_fixture();
        too_many_features["resolve"]["nodes"][0]["features"] = serde_json::Value::Array(
            std::iter::repeat_n(serde_json::Value::Null, MAX_RUST_CARGO_ACTIVE_FEATURES + 1)
                .collect(),
        );
        assert_eq!(
            make_facts(&too_many_features),
            Err(RustCargoFactsAdmissionError::Bound),
            "the aggregate active-feature budget is checked before text copies"
        );

        let mut too_many_dependency_kinds = metadata_fixture();
        too_many_dependency_kinds["resolve"]["nodes"][0]["deps"] = serde_json::json!([{
            "name": "b",
            "pkg": "path+file:///workspace/crates/b#b@1.0.0",
            "dep_kinds": std::iter::repeat_n(
                serde_json::Value::Null,
                MAX_RUST_CARGO_DEPENDENCY_KINDS + 1,
            ).collect::<Vec<_>>(),
        }]);
        assert_eq!(
            make_facts(&too_many_dependency_kinds),
            Err(RustCargoFactsAdmissionError::Bound),
            "dependency-kind rows have an aggregate bound before a DTO vector is reserved"
        );

        let mut decoded_facts = fixture_facts();
        decoded_facts.resolved_packages[0].active_features =
            vec![Box::<str>::from("active"); MAX_RUST_CARGO_ACTIVE_FEATURES + 1].into_boxed_slice();
        assert_eq!(
            decoded_facts.admit(),
            Err(RustCargoFactsAdmissionError::Bound),
            "already-decoded DTOs are rechecked against the aggregate active-feature limit"
        );
    }

    #[test]
    fn aggregate_budget_uses_checked_text_bytes_and_collection_counts() {
        let text = "x".repeat(MAX_RUST_CARGO_METADATA_TEXT_BYTES);
        let mut budget = CargoFactsBudget::default();
        for _ in 0..MAX_RUST_CARGO_TOTAL_TEXT_BYTES / text.len() {
            budget
                .retain_text(&text)
                .expect("text remains within aggregate limit");
        }
        assert_eq!(
            budget.copy_text(&text),
            Err(RustCargoFactsAdmissionError::Bound),
            "over-budget strings are refused before cloning into a facts DTO"
        );

        let mut entries = CargoFactsBudget::default();
        entries
            .reserve_entries(MAX_RUST_CARGO_FACT_ENTRIES)
            .expect("exact entry capacity is accepted");
        assert_eq!(
            entries.reserve_entries(1),
            Err(RustCargoFactsAdmissionError::Bound),
            "collection lengths use checked aggregate accounting"
        );
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
