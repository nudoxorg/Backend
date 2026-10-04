//! Bounded, source-observation-bound aliases derived from one exact Cargo
//! workspace metadata response.

use crate::{
    MAX_RUST_CARGO_METADATA_TEXT_BYTES, RustCargoWorkspaceFactsV1, RustCargoWorkspacePackageFactV1,
};
use backend_semantic::vocabulary::LanguageProfile;
use std::path::Path;

/// Maximum distinct Cargo aliases retained for one product package.
pub const MAX_CARGO_PACKAGE_ALIASES: usize = 64;
/// Maximum UTF-8 bytes retained across all Cargo aliases for one product package.
pub const MAX_CARGO_PACKAGE_ALIAS_BYTES: usize = 64 * 1024;
/// Maximum exact per-profile Cargo alias observations retained for one package.
pub const MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS: usize = 4;

/// Exact package-name spelling reported by Cargo.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CargoPackageNameV1(Box<str>);

impl CargoPackageNameV1 {
    /// Admits a bounded Cargo package name.
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, CargoPackageAliasErrorV1> {
        let value = value.into();
        check_alias_text(&value)?;
        Ok(Self(value))
    }

    /// Returns the exact Cargo spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact target-name spelling reported by Cargo metadata.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CargoTargetNameV1(Box<str>);

impl CargoTargetNameV1 {
    /// Admits a bounded Cargo target name.
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, CargoPackageAliasErrorV1> {
        let value = value.into();
        check_alias_text(&value)?;
        Ok(Self(value))
    }

    /// Returns the exact Cargo spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact Rust crate identifier when a producer reports that distinct fact.
///
/// Cargo target names are not rewritten into this type: the producer must
/// supply the exact identifier it observed.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RustCrateIdentifierV1(Box<str>);

impl RustCrateIdentifierV1 {
    /// Admits a bounded, exact Rust crate identifier spelling.
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, CargoPackageAliasErrorV1> {
        let value = value.into();
        check_alias_text(&value)?;
        Ok(Self(value))
    }

    /// Returns the exact producer spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One alias with its producer-reported identity domain intact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CargoPackageAliasV1 {
    /// Cargo `[package].name` from the exact selected workspace member.
    CargoPackageName(CargoPackageNameV1),
    /// One exact `target.name` from that same member.
    CargoTargetName(CargoTargetNameV1),
    /// A Rust crate identifier explicitly supplied by an authority producer.
    RustCrateIdentifier(RustCrateIdentifierV1),
}

impl CargoPackageAliasV1 {
    /// Returns the closed wire discriminant for this alias domain.
    #[must_use]
    pub const fn kind_tag(&self) -> u8 {
        match self {
            Self::CargoPackageName(_) => 0,
            Self::CargoTargetName(_) => 1,
            Self::RustCrateIdentifier(_) => 2,
        }
    }

    /// Returns the exact producer spelling without normalizing it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::CargoPackageName(value) => value.as_str(),
            Self::CargoTargetName(value) => value.as_str(),
            Self::RustCrateIdentifier(value) => value.as_str(),
        }
    }

    /// Admits one alias from a durable representation.
    pub fn from_wire_parts(
        kind: u8,
        value: impl Into<Box<str>>,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        let value = value.into();
        match kind {
            0 => CargoPackageNameV1::new(value).map(Self::CargoPackageName),
            1 => CargoTargetNameV1::new(value).map(Self::CargoTargetName),
            2 => RustCrateIdentifierV1::new(value).map(Self::RustCrateIdentifier),
            _ => Err(CargoPackageAliasErrorV1::Shape),
        }
    }
}

/// Why one exact Cargo profile could not provide a package alias.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CargoPackageAliasUnavailableV1 {
    /// The staged compiler result had no retained full metadata facts.
    MetadataFactsUnavailable,
    /// The admitted metadata did not select a package for the requested root.
    NoExactWorkspaceMember,
    /// The selected package did not bind to the exact requested manifest.
    SelectedManifestMismatch,
    /// Full Cargo facts failed their bounded/canonical admission.
    InvalidWorkspaceFacts,
    /// The product source row could not retain the bounded alias payload.
    ProjectRowCapacity,
}

/// Coverage for the aliases derived from one exact profile observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CargoPackageAliasCoverageV1 {
    /// Every distinct admitted alias for this profile is retained.
    Complete,
    /// Some exact aliases were omitted at the fixed name or byte ceiling.
    Truncated {
        /// Number of this profile's aliases present in the shared alias table.
        retained: u8,
        /// Exact number of this profile's distinct Cargo aliases omitted.
        omitted: u32,
    },
    /// No exact selected Cargo member was available for this profile.
    Unavailable(CargoPackageAliasUnavailableV1),
}

/// Compact exact Cargo metadata provenance persisted with one profile alias set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CargoPackageAliasCargoFactsV1 {
    metadata_response_digest: [u8; 32],
    resolved_lockfile_digest: [u8; 32],
    toolchain_binding_digest: [u8; 32],
}

impl CargoPackageAliasCargoFactsV1 {
    /// Reconstructs opaque digest provenance from its durable bytes.
    #[must_use]
    pub const fn from_wire_parts(
        metadata_response_digest: [u8; 32],
        resolved_lockfile_digest: [u8; 32],
        toolchain_binding_digest: [u8; 32],
    ) -> Self {
        Self {
            metadata_response_digest,
            resolved_lockfile_digest,
            toolchain_binding_digest,
        }
    }

    /// Exact digest of Cargo's full metadata response bytes.
    #[must_use]
    pub const fn metadata_response_digest(self) -> [u8; 32] {
        self.metadata_response_digest
    }

    /// Exact digest of the lockfile used by the Cargo resolution.
    #[must_use]
    pub const fn resolved_lockfile_digest(self) -> [u8; 32] {
        self.resolved_lockfile_digest
    }

    /// Exact digest of the admitted Cargo invocation, config, and toolchain binding.
    #[must_use]
    pub const fn toolchain_binding_digest(self) -> [u8; 32] {
        self.toolchain_binding_digest
    }
}

/// One per-profile observation joining exact Cargo facts to one source revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoPackageAliasObservationV1 {
    profile: LanguageProfile,
    source_observation_revision: [u8; 32],
    cargo_facts: Option<CargoPackageAliasCargoFactsV1>,
    alias_indices: Box<[u8]>,
    coverage: CargoPackageAliasCoverageV1,
}

impl CargoPackageAliasObservationV1 {
    /// Reconstructs an exact per-profile observation from its durable representation.
    pub fn from_wire_parts(
        profile: LanguageProfile,
        source_observation_revision: [u8; 32],
        cargo_facts: Option<CargoPackageAliasCargoFactsV1>,
        alias_indices: Vec<u8>,
        coverage: CargoPackageAliasCoverageV1,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        if !matches!(profile, LanguageProfile::Rust(_))
            || alias_indices.len() > MAX_CARGO_PACKAGE_ALIASES
            || alias_indices.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(CargoPackageAliasErrorV1::Shape);
        }
        let observation = Self {
            profile,
            source_observation_revision,
            cargo_facts,
            alias_indices: alias_indices.into_boxed_slice(),
            coverage,
        };
        match observation.coverage {
            CargoPackageAliasCoverageV1::Complete
                if observation.cargo_facts.is_some() && !observation.alias_indices.is_empty() =>
            {
                Ok(observation)
            }
            CargoPackageAliasCoverageV1::Truncated { retained, omitted }
                if observation.cargo_facts.is_some()
                    && usize::from(retained) == observation.alias_indices.len()
                    && omitted > 0 =>
            {
                Ok(observation)
            }
            CargoPackageAliasCoverageV1::Unavailable(_) if observation.alias_indices.is_empty() => {
                Ok(observation)
            }
            _ => Err(CargoPackageAliasErrorV1::Shape),
        }
    }

    /// Profile whose compiler preflight produced this observation.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Exact revision returned by the source observation joined to this profile.
    #[must_use]
    pub const fn source_observation_revision(&self) -> [u8; 32] {
        self.source_observation_revision
    }

    /// Exact Cargo provenance, when a full metadata response was admitted.
    #[must_use]
    pub const fn cargo_facts(&self) -> Option<CargoPackageAliasCargoFactsV1> {
        self.cargo_facts
    }

    /// Indexes into the evidence's bounded shared alias table.
    #[must_use]
    pub fn alias_indices(&self) -> &[u8] {
        &self.alias_indices
    }

    /// Exact completeness state for this profile.
    #[must_use]
    pub const fn coverage(&self) -> CargoPackageAliasCoverageV1 {
        self.coverage
    }
}

/// Package aliases and the exact source/Cargo observations that authorize them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoPackageAliasEvidenceV1 {
    aliases: Box<[CargoPackageAliasV1]>,
    observations: Box<[CargoPackageAliasObservationV1]>,
}

impl CargoPackageAliasEvidenceV1 {
    /// Creates one profile observation from the already admitted Cargo facts.
    ///
    /// The selected package is used only when Cargo's selected ID names a
    /// workspace member whose manifest is the exact requested manifest. A
    /// virtual workspace root stays an explicit unavailable observation.
    #[must_use]
    pub fn from_workspace_facts(
        profile: LanguageProfile,
        source_observation_revision: [u8; 32],
        requested_manifest: &Path,
        facts: Option<&RustCargoWorkspaceFactsV1>,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        let pending = match facts {
            Some(facts) => pending_observation(
                profile,
                source_observation_revision,
                requested_manifest,
                facts,
            ),
            None => PendingObservation::unavailable(
                profile,
                source_observation_revision,
                CargoPackageAliasUnavailableV1::MetadataFactsUnavailable,
            ),
        };
        Self::merge_profiles([pending])
    }

    /// Creates a checked unavailable observation when a compiler producer
    /// did not retain its Cargo preflight result.
    pub fn unavailable(
        profile: LanguageProfile,
        source_observation_revision: [u8; 32],
        reason: CargoPackageAliasUnavailableV1,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        Self::merge_profiles([PendingObservation::unavailable(
            profile,
            source_observation_revision,
            reason,
        )])
    }

    /// Joins the exact per-profile producer observations for one product package.
    fn merge_profiles(
        observations: impl IntoIterator<Item = PendingObservation>,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        let mut pending = Vec::with_capacity(MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS);
        for observation in observations {
            if pending.len() == MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS {
                return Err(CargoPackageAliasErrorV1::Bound);
            }
            pending.push(observation);
        }
        if pending.is_empty() {
            return Err(CargoPackageAliasErrorV1::Bound);
        }
        pending.sort_by_key(|observation| observation.profile);
        if pending
            .windows(2)
            .any(|pair| pair[0].profile >= pair[1].profile)
            || pending
                .iter()
                .any(|observation| !matches!(observation.profile, LanguageProfile::Rust(_)))
        {
            return Err(CargoPackageAliasErrorV1::Ordering);
        }

        let mut candidates = pending
            .iter()
            .flat_map(|observation| observation.aliases.iter())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        let mut aliases = Vec::new();
        let mut alias_bytes = 0_usize;
        for alias in candidates {
            if aliases.len() >= MAX_CARGO_PACKAGE_ALIASES {
                break;
            }
            let Some(next_bytes) = alias_bytes.checked_add(alias.as_str().len()) else {
                break;
            };
            if next_bytes > MAX_CARGO_PACKAGE_ALIAS_BYTES {
                continue;
            }
            alias_bytes = next_bytes;
            aliases.push(alias.clone());
        }

        let aliases = aliases.into_boxed_slice();
        let mut admitted_observations = Vec::with_capacity(pending.len());
        for observation in pending {
            let mut indices = Vec::new();
            let mut globally_omitted = 0_u32;
            for alias in &observation.aliases {
                match aliases.binary_search(alias) {
                    Ok(index) => indices
                        .push(u8::try_from(index).map_err(|_| CargoPackageAliasErrorV1::Bound)?),
                    Err(_) => {
                        globally_omitted = globally_omitted.saturating_add(1);
                    }
                }
            }
            indices.sort_unstable();
            indices.dedup();
            let omitted = observation.omitted.saturating_add(globally_omitted);
            let coverage = match observation.unavailable {
                Some(reason) => CargoPackageAliasCoverageV1::Unavailable(reason),
                None if omitted > 0 => CargoPackageAliasCoverageV1::Truncated {
                    retained: u8::try_from(indices.len())
                        .map_err(|_| CargoPackageAliasErrorV1::Bound)?,
                    omitted,
                },
                None => CargoPackageAliasCoverageV1::Complete,
            };
            admitted_observations.push(CargoPackageAliasObservationV1 {
                profile: observation.profile,
                source_observation_revision: observation.source_observation_revision,
                cargo_facts: observation.cargo_facts,
                alias_indices: indices.into_boxed_slice(),
                coverage,
            });
        }
        let evidence = Self {
            aliases,
            observations: admitted_observations.into_boxed_slice(),
        };
        evidence.admit()?;
        Ok(evidence)
    }

    /// Reconstructs one bounded evidence value from its versioned durable representation.
    pub fn from_wire_parts(
        aliases: Vec<CargoPackageAliasV1>,
        observations: Vec<CargoPackageAliasObservationV1>,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        let evidence = Self {
            aliases: aliases.into_boxed_slice(),
            observations: observations.into_boxed_slice(),
        };
        evidence.admit()?;
        Ok(evidence)
    }

    /// Combines already admitted profile evidence without discarding each
    /// profile's independent source revision or Cargo provenance.
    pub fn merge(
        evidence: impl IntoIterator<Item = CargoPackageAliasEvidenceV1>,
    ) -> Result<Self, CargoPackageAliasErrorV1> {
        let mut pending = Vec::with_capacity(MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS);
        for value in evidence {
            value.admit()?;
            for observation in value.observations.iter() {
                if pending.len() == MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS {
                    return Err(CargoPackageAliasErrorV1::Bound);
                }
                let aliases = observation
                    .alias_indices
                    .iter()
                    .map(|index| {
                        value
                            .aliases
                            .get(usize::from(*index))
                            .cloned()
                            .ok_or(CargoPackageAliasErrorV1::Shape)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let (omitted, unavailable) = match observation.coverage {
                    CargoPackageAliasCoverageV1::Complete => (0, None),
                    CargoPackageAliasCoverageV1::Truncated { omitted, .. } => (omitted, None),
                    CargoPackageAliasCoverageV1::Unavailable(reason) => (0, Some(reason)),
                };
                pending.push(PendingObservation {
                    profile: observation.profile,
                    source_observation_revision: observation.source_observation_revision,
                    cargo_facts: observation.cargo_facts,
                    aliases,
                    omitted,
                    unavailable,
                });
            }
        }
        Self::merge_profiles(pending)
    }

    /// Preserves each exact observation while stating that this relation row
    /// could not fit the aliases within its own encoded-value ceiling.
    #[must_use]
    pub fn unavailable_for_project_row_capacity(&self) -> Self {
        Self {
            aliases: Box::new([]),
            observations: self
                .observations
                .iter()
                .map(|observation| CargoPackageAliasObservationV1 {
                    profile: observation.profile,
                    source_observation_revision: observation.source_observation_revision,
                    cargo_facts: observation.cargo_facts,
                    alias_indices: Box::new([]),
                    coverage: CargoPackageAliasCoverageV1::Unavailable(
                        CargoPackageAliasUnavailableV1::ProjectRowCapacity,
                    ),
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    /// Checks counts, canonical ordering, alias indexes, and coverage consistency.
    pub fn admit(&self) -> Result<(), CargoPackageAliasErrorV1> {
        if self.aliases.len() > MAX_CARGO_PACKAGE_ALIASES
            || self.observations.is_empty()
            || self.observations.len() > MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS
            || self
                .aliases
                .iter()
                .map(|alias| alias.as_str().len())
                .sum::<usize>()
                > MAX_CARGO_PACKAGE_ALIAS_BYTES
            || self.aliases.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .observations
                .windows(2)
                .any(|pair| pair[0].profile >= pair[1].profile)
        {
            return Err(CargoPackageAliasErrorV1::Ordering);
        }
        let mut referenced_aliases = [false; MAX_CARGO_PACKAGE_ALIASES];
        for observation in &self.observations {
            if !matches!(observation.profile, LanguageProfile::Rust(_))
                || observation.alias_indices.len() > MAX_CARGO_PACKAGE_ALIASES
                || observation
                    .alias_indices
                    .windows(2)
                    .any(|pair| pair[0] >= pair[1])
                || observation
                    .alias_indices
                    .iter()
                    .any(|index| usize::from(*index) >= self.aliases.len())
            {
                return Err(CargoPackageAliasErrorV1::Shape);
            }
            for index in observation.alias_indices.iter() {
                referenced_aliases[usize::from(*index)] = true;
            }
            match observation.coverage {
                CargoPackageAliasCoverageV1::Complete => {
                    if observation.cargo_facts.is_none() || observation.alias_indices.is_empty() {
                        return Err(CargoPackageAliasErrorV1::Shape);
                    }
                }
                CargoPackageAliasCoverageV1::Truncated { retained, omitted } => {
                    if observation.cargo_facts.is_none()
                        || usize::from(retained) != observation.alias_indices.len()
                        || omitted == 0
                    {
                        return Err(CargoPackageAliasErrorV1::Shape);
                    }
                }
                CargoPackageAliasCoverageV1::Unavailable(_) => {
                    if !observation.alias_indices.is_empty() {
                        return Err(CargoPackageAliasErrorV1::Shape);
                    }
                }
            }
        }
        if referenced_aliases[..self.aliases.len()].contains(&false) {
            return Err(CargoPackageAliasErrorV1::Shape);
        }
        Ok(())
    }

    /// Exact shared alias table, already sorted and bounded.
    #[must_use]
    pub fn aliases(&self) -> &[CargoPackageAliasV1] {
        &self.aliases
    }

    /// Exact per-profile source/Cargo observations.
    #[must_use]
    pub fn observations(&self) -> &[CargoPackageAliasObservationV1] {
        &self.observations
    }

    /// Returns the observation for one exact Rust language profile.
    #[must_use]
    pub fn observation(&self, profile: LanguageProfile) -> Option<&CargoPackageAliasObservationV1> {
        self.observations
            .binary_search_by_key(&profile, |observation| observation.profile)
            .ok()
            .and_then(|index| self.observations.get(index))
    }

    /// Returns whether one exact alias spelling is admitted for a profile.
    #[must_use]
    pub fn admits_for_profile(&self, profile: LanguageProfile, value: &str) -> bool {
        let Some(observation) = self.observation(profile) else {
            return false;
        };
        observation.alias_indices.iter().any(|index| {
            self.aliases
                .get(usize::from(*index))
                .is_some_and(|alias| alias.as_str() == value)
        })
    }

    /// Hashes every alias and each exact source/Cargo provenance binding.
    pub fn update_digest(&self, hasher: &mut blake3::Hasher) {
        hasher.update(&[0xa5]);
        hash_count(hasher, self.aliases.len());
        for alias in &self.aliases {
            hasher.update(&[alias.kind_tag()]);
            hash_text(hasher, alias.as_str());
        }
        hash_count(hasher, self.observations.len());
        for observation in &self.observations {
            hasher.update(&<[u8; 2]>::from(observation.profile));
            hasher.update(&observation.source_observation_revision);
            match observation.cargo_facts {
                Some(facts) => {
                    hasher.update(&[1]);
                    hasher.update(&facts.metadata_response_digest);
                    hasher.update(&facts.resolved_lockfile_digest);
                    hasher.update(&facts.toolchain_binding_digest);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            match observation.coverage {
                CargoPackageAliasCoverageV1::Complete => {
                    hasher.update(&[0]);
                }
                CargoPackageAliasCoverageV1::Truncated { retained, omitted } => {
                    hasher.update(&[1, retained]);
                    hasher.update(&omitted.to_be_bytes());
                }
                CargoPackageAliasCoverageV1::Unavailable(reason) => {
                    hasher.update(&[2, unavailable_tag(reason)]);
                }
            }
            hash_count(hasher, observation.alias_indices.len());
            hasher.update(&observation.alias_indices);
        }
    }
}

/// Intermediate exact facts before profile observations are compacted into one shared table.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingObservation {
    profile: LanguageProfile,
    source_observation_revision: [u8; 32],
    cargo_facts: Option<CargoPackageAliasCargoFactsV1>,
    aliases: Vec<CargoPackageAliasV1>,
    omitted: u32,
    unavailable: Option<CargoPackageAliasUnavailableV1>,
}

impl PendingObservation {
    /// Creates an unavailable exact-profile observation.
    fn unavailable(
        profile: LanguageProfile,
        source_observation_revision: [u8; 32],
        reason: CargoPackageAliasUnavailableV1,
    ) -> Self {
        Self {
            profile,
            source_observation_revision,
            cargo_facts: None,
            aliases: Vec::new(),
            omitted: 0,
            unavailable: Some(reason),
        }
    }
}

/// Bounded package-alias evidence construction failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CargoPackageAliasErrorV1 {
    /// A field or coverage combination is malformed.
    Shape,
    /// An alias, observation, or text field exceeds its fixed protocol limit.
    Bound,
    /// Aliases or profile observations are not in canonical order.
    Ordering,
}

fn pending_observation(
    profile: LanguageProfile,
    source_observation_revision: [u8; 32],
    requested_manifest: &Path,
    facts: &RustCargoWorkspaceFactsV1,
) -> PendingObservation {
    let cargo_facts = Some(CargoPackageAliasCargoFactsV1 {
        metadata_response_digest: facts.metadata_response_digest,
        resolved_lockfile_digest: facts.resolved_lockfile_digest,
        toolchain_binding_digest: facts.toolchain_binding_digest,
    });
    let unavailable = if !matches!(profile, LanguageProfile::Rust(_)) {
        Some(CargoPackageAliasUnavailableV1::InvalidWorkspaceFacts)
    } else if facts.admit().is_err() {
        Some(CargoPackageAliasUnavailableV1::InvalidWorkspaceFacts)
    } else if facts.manifest_path != requested_manifest {
        Some(CargoPackageAliasUnavailableV1::SelectedManifestMismatch)
    } else {
        match facts.selected_package_id.as_deref() {
            Some(selected_id) => {
                let mut selected = facts
                    .workspace_packages
                    .iter()
                    .filter(|package| package.package_id.as_ref() == selected_id);
                match selected.next() {
                    Some(package)
                        if package.manifest_path == requested_manifest
                            && selected.next().is_none() =>
                    {
                        return selected_package_observation(
                            profile,
                            source_observation_revision,
                            cargo_facts,
                            package,
                        );
                    }
                    _ => Some(CargoPackageAliasUnavailableV1::SelectedManifestMismatch),
                }
            }
            None => Some(CargoPackageAliasUnavailableV1::NoExactWorkspaceMember),
        }
    };
    PendingObservation {
        profile,
        source_observation_revision,
        cargo_facts,
        aliases: Vec::new(),
        omitted: 0,
        unavailable,
    }
}

fn selected_package_observation(
    profile: LanguageProfile,
    source_observation_revision: [u8; 32],
    cargo_facts: Option<CargoPackageAliasCargoFactsV1>,
    package: &RustCargoWorkspacePackageFactV1,
) -> PendingObservation {
    let mut aliases = Vec::new();
    let package_alias = CargoPackageAliasV1::CargoPackageName(
        CargoPackageNameV1::new(package.name.clone()).expect("admitted Cargo package name"),
    );
    aliases.push(package_alias);
    let mut previous_target: Option<&str> = None;
    let mut alias_bytes = aliases[0].as_str().len();
    let mut omitted = 0_u32;
    for target in package.targets.iter() {
        if previous_target == Some(target.name.as_ref()) {
            continue;
        }
        previous_target = Some(target.name.as_ref());
        let candidate = CargoPackageAliasV1::CargoTargetName(
            CargoTargetNameV1::new(target.name.clone()).expect("admitted Cargo target name"),
        );
        let candidate_bytes = candidate.as_str().len();
        let next_bytes = alias_bytes.checked_add(candidate_bytes);
        if aliases.len() >= MAX_CARGO_PACKAGE_ALIASES
            || next_bytes.is_none_or(|bytes| bytes > MAX_CARGO_PACKAGE_ALIAS_BYTES)
        {
            omitted = omitted.saturating_add(1);
            continue;
        }
        alias_bytes = next_bytes.unwrap_or(alias_bytes);
        aliases.push(candidate);
    }
    aliases.sort();
    PendingObservation {
        profile,
        source_observation_revision,
        cargo_facts,
        aliases,
        omitted,
        unavailable: None,
    }
}

fn check_alias_text(value: &str) -> Result<(), CargoPackageAliasErrorV1> {
    if value.is_empty() || value.len() > MAX_RUST_CARGO_METADATA_TEXT_BYTES {
        Err(CargoPackageAliasErrorV1::Bound)
    } else {
        Ok(())
    }
}

fn hash_count(hasher: &mut blake3::Hasher, count: usize) {
    hasher.update(&(count as u64).to_be_bytes());
}

fn hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hash_count(hasher, value.len());
    hasher.update(value.as_bytes());
}

const fn unavailable_tag(reason: CargoPackageAliasUnavailableV1) -> u8 {
    match reason {
        CargoPackageAliasUnavailableV1::MetadataFactsUnavailable => 0,
        CargoPackageAliasUnavailableV1::NoExactWorkspaceMember => 1,
        CargoPackageAliasUnavailableV1::SelectedManifestMismatch => 2,
        CargoPackageAliasUnavailableV1::InvalidWorkspaceFacts => 3,
        CargoPackageAliasUnavailableV1::ProjectRowCapacity => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RustCargoFeatureFactV1, RustCargoFeatureSelectionV1, RustCargoResolvedPackageFactV1,
        RustCargoTargetFactV1, RustCargoWorkspacePackageFactV1,
    };
    use backend_semantic::vocabulary::RustEdition;
    use std::path::PathBuf;

    fn observation(
        evidence: &CargoPackageAliasEvidenceV1,
        profile: LanguageProfile,
    ) -> Result<&CargoPackageAliasObservationV1, String> {
        evidence
            .observation(profile)
            .ok_or_else(|| format!("missing observation for {profile:?}"))
    }

    fn facts(
        target_names: &[String],
        selected_package_id: Option<&str>,
    ) -> RustCargoWorkspaceFactsV1 {
        let package_id = "path+file:///workspace#fixture@1.0.0";
        let mut targets = target_names
            .iter()
            .enumerate()
            .map(|(index, name)| RustCargoTargetFactV1 {
                name: name.clone().into_boxed_str(),
                kinds: vec!["lib".into()].into_boxed_slice(),
                crate_types: vec!["lib".into()].into_boxed_slice(),
                source_path: PathBuf::from(format!("/workspace/src/{index:04}.rs")),
                required_features: Box::new([]),
                edition: "2024".into(),
                test: true,
                doctest: true,
                doc: true,
            })
            .collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            (&left.name, &left.source_path, &left.kinds).cmp(&(
                &right.name,
                &right.source_path,
                &right.kinds,
            ))
        });
        RustCargoWorkspaceFactsV1 {
            manifest_path: PathBuf::from("/workspace/Cargo.toml"),
            workspace_root: PathBuf::from("/workspace"),
            selected_package_id: selected_package_id.map(Into::into),
            requested_features: RustCargoFeatureSelectionV1 {
                all_features: false,
                no_default_features: false,
                features: Box::new([]),
            },
            workspace_packages: vec![RustCargoWorkspacePackageFactV1 {
                package_id: package_id.into(),
                name: "fixture".into(),
                version: "1.0.0".into(),
                manifest_path: PathBuf::from("/workspace/Cargo.toml"),
                features: vec![RustCargoFeatureFactV1 {
                    name: "default".into(),
                    enables: Box::new([]),
                }]
                .into_boxed_slice(),
                targets: targets.into_boxed_slice(),
            }]
            .into_boxed_slice(),
            resolved_packages: vec![RustCargoResolvedPackageFactV1 {
                package_id: package_id.into(),
                active_features: vec!["default".into()].into_boxed_slice(),
                dependencies: Box::new([]),
            }]
            .into_boxed_slice(),
            metadata_response_digest: [10; 32],
            resolved_lockfile_digest: [11; 32],
            toolchain_binding_digest: [12; 32],
        }
    }

    #[test]
    fn aliases_are_bound_to_each_exact_profile_observation() -> Result<(), String> {
        let profile_2021 = LanguageProfile::Rust(RustEdition::Rust2021);
        let profile_2024 = LanguageProfile::Rust(RustEdition::Rust2024);
        let admitted = CargoPackageAliasEvidenceV1::from_workspace_facts(
            profile_2021,
            [21; 32],
            Path::new("/workspace/Cargo.toml"),
            Some(&facts(
                &["fixture_lib".to_owned()],
                Some("path+file:///workspace#fixture@1.0.0"),
            )),
        )
        .expect("exact selected member facts");
        let unavailable = CargoPackageAliasEvidenceV1::unavailable(
            profile_2024,
            [22; 32],
            CargoPackageAliasUnavailableV1::MetadataFactsUnavailable,
        )
        .expect("bounded unavailable profile observation");
        let merged = CargoPackageAliasEvidenceV1::merge([admitted, unavailable])
            .expect("sorted independent profile observations");

        assert_eq!(
            observation(&merged, profile_2021)?.source_observation_revision(),
            [21; 32]
        );
        assert_eq!(
            observation(&merged, profile_2024)?.source_observation_revision(),
            [22; 32]
        );
        assert!(merged.admits_for_profile(profile_2021, "fixture"));
        assert!(merged.admits_for_profile(profile_2021, "fixture_lib"));
        assert!(!merged.admits_for_profile(profile_2024, "fixture"));
        Ok(())
    }

    #[test]
    fn virtual_workspace_root_does_not_guess_a_member_alias() -> Result<(), String> {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let root_facts = facts(&["fixture_lib".to_owned()], None);
        let evidence = CargoPackageAliasEvidenceV1::from_workspace_facts(
            profile,
            [31; 32],
            Path::new("/workspace/Cargo.toml"),
            Some(&root_facts),
        )
        .expect("unselected virtual root is an explicit unavailable observation");

        assert!(evidence.aliases().is_empty());
        assert_eq!(
            observation(&evidence, profile)?.coverage(),
            CargoPackageAliasCoverageV1::Unavailable(
                CargoPackageAliasUnavailableV1::NoExactWorkspaceMember
            )
        );
        Ok(())
    }

    #[test]
    fn selected_member_is_not_reused_for_a_different_requested_manifest() -> Result<(), String> {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let facts = facts(
            &["fixture_lib".to_owned()],
            Some("path+file:///workspace#fixture@1.0.0"),
        );
        let evidence = CargoPackageAliasEvidenceV1::from_workspace_facts(
            profile,
            [35; 32],
            Path::new("/workspace/member/Cargo.toml"),
            Some(&facts),
        )
        .expect("manifest mismatch is an explicit unavailable observation");

        assert!(evidence.aliases().is_empty());
        assert_eq!(
            observation(&evidence, profile)?.coverage(),
            CargoPackageAliasCoverageV1::Unavailable(
                CargoPackageAliasUnavailableV1::SelectedManifestMismatch
            )
        );
        Ok(())
    }

    #[test]
    fn alias_bounds_report_truncation_and_reject_orphaned_wire_names() -> Result<(), String> {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let names = (0..70)
            .map(|index| format!("target_{index:02}"))
            .collect::<Vec<_>>();
        let facts = facts(&names, Some("path+file:///workspace#fixture@1.0.0"));
        let evidence = CargoPackageAliasEvidenceV1::from_workspace_facts(
            profile,
            [41; 32],
            Path::new("/workspace/Cargo.toml"),
            Some(&facts),
        )
        .expect("alias set is truncated at its fixed bound");
        assert_eq!(evidence.aliases().len(), MAX_CARGO_PACKAGE_ALIASES);
        assert_eq!(
            observation(&evidence, profile)?.coverage(),
            CargoPackageAliasCoverageV1::Truncated {
                retained: MAX_CARGO_PACKAGE_ALIASES as u8,
                omitted: 7,
            }
        );

        let orphan = CargoPackageAliasEvidenceV1::from_wire_parts(
            vec![
                CargoPackageAliasV1::CargoPackageName(
                    CargoPackageNameV1::new("orphan")
                        .map_err(|error| format!("invalid fixture package alias: {error:?}"))?,
                ),
                CargoPackageAliasV1::CargoTargetName(
                    CargoTargetNameV1::new("used")
                        .map_err(|error| format!("invalid fixture target alias: {error:?}"))?,
                ),
            ],
            vec![
                CargoPackageAliasObservationV1::from_wire_parts(
                    profile,
                    [42; 32],
                    Some(CargoPackageAliasCargoFactsV1 {
                        metadata_response_digest: [1; 32],
                        resolved_lockfile_digest: [2; 32],
                        toolchain_binding_digest: [3; 32],
                    }),
                    vec![1],
                    CargoPackageAliasCoverageV1::Complete,
                )
                .expect("shape is locally consistent"),
            ],
        );
        assert_eq!(orphan.err(), Some(CargoPackageAliasErrorV1::Shape));
        Ok(())
    }

    #[test]
    fn profile_merge_stops_at_the_fixed_observation_bound() {
        use std::cell::Cell;

        let yielded = Cell::new(0);
        let observations = std::iter::from_fn(|| {
            yielded.set(yielded.get() + 1);
            Some(PendingObservation::unavailable(
                LanguageProfile::Rust(RustEdition::Rust2024),
                [yielded.get() as u8; 32],
                CargoPackageAliasUnavailableV1::MetadataFactsUnavailable,
            ))
        })
        .take(MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS + 8);

        assert_eq!(
            CargoPackageAliasEvidenceV1::merge_profiles(observations),
            Err(CargoPackageAliasErrorV1::Bound)
        );
        assert_eq!(
            yielded.get(),
            MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS + 1,
            "oversized iterators are refused before collecting their full contents"
        );
    }

    #[test]
    fn evidence_merge_stops_before_expanding_more_than_four_profiles() {
        let evidence = CargoPackageAliasEvidenceV1::unavailable(
            LanguageProfile::Rust(RustEdition::Rust2024),
            [51; 32],
            CargoPackageAliasUnavailableV1::MetadataFactsUnavailable,
        )
        .expect("bounded profile");

        assert_eq!(
            CargoPackageAliasEvidenceV1::merge(std::iter::repeat_n(
                evidence,
                MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS + 1
            )),
            Err(CargoPackageAliasErrorV1::Bound)
        );
    }
}
