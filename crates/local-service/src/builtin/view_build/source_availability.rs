//! Available artifacts are not a certificate of a complete file or project.
//!
//! A Partial publication can supply positive compiler facts from a selected
//! image whose source is still current. Missing images and unavailable fact
//! planes remain unknown; in particular they never suppress structural rows.

use super::super::{ActivatedProductSemantics, BuiltinModelError, IndexedSources};
use super::image_rows::{self, CompiledImage, ImageRowResidence};
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticPublicationCoverage,
};
use backend_semantic::vocabulary::LanguageProfile;
use backend_version::{ContentId, SourceFactDomain};
use std::collections::{BTreeMap, BTreeSet};

type SourceKey = ([u8; 32], LanguageProfile, String);

/// Current persisted source identities. Absence is not an empty source or a
/// successful compilation, and a source without an identity cannot admit a
/// Partial publication's artifact.
pub(in crate::builtin) struct SourceAvailability {
    identities: BTreeMap<SourceKey, Option<ContentId<SourceFactDomain>>>,
    required_profiles: BTreeMap<[u8; 32], BTreeSet<LanguageProfile>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtifactSource {
    Current,
    Absent,
    IdentityUnavailable,
    Changed,
}

impl SourceAvailability {
    /// Profiles required by the selected package's current source frontier.
    pub(in crate::builtin) fn required_profiles(
        &self,
        package: backend_engine::PackageKey,
    ) -> impl Iterator<Item = LanguageProfile> + '_ {
        self.required_profiles
            .get(&package.to_bytes())
            .into_iter()
            .flatten()
            .copied()
    }

    pub(in crate::builtin) fn of(sources: &IndexedSources) -> Result<Self, BuiltinModelError> {
        Self::of_controlled(sources, None)
    }

    pub(in crate::builtin) fn of_controlled(
        sources: &IndexedSources,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Self, BuiltinModelError> {
        super::super::check_search_preparation(cancelled, "source-availability")?;
        let mut identities = BTreeMap::new();
        let mut required_profiles = BTreeMap::<_, BTreeSet<_>>::new();
        for (file_key, record) in &sources.files {
            super::super::check_search_preparation(cancelled, "source-availability-file")?;
            let file = record.file_fields().ok_or_else(|| {
                BuiltinModelError("artifact source frontier contains a non-file row".to_owned())
            })?;
            // An unavailable source cannot simultaneously attest content or
            // identity. Validate this before a profile can use its identity as
            // proof that a compiler image is current. A valid unavailable row
            // has no identity and remains an unknown source in the frontier.
            file.admitted_unavailable_reason().map_err(|error| {
                BuiltinModelError(format!("admit artifact source availability: {error}"))
            })?;
            let project = sources.projects.get(&file.project).ok_or_else(|| {
                BuiltinModelError("artifact source refers to a missing project".to_owned())
            })?;
            if backend_engine::product_source_file_key(file.project, file.path) != *file_key
                || project.files.binary_search(file_key).is_err()
            {
                return Err(BuiltinModelError(
                    "artifact source is outside its canonical project frontier".to_owned(),
                ));
            }
            let Some(profile) =
                super::super::ingest::source_record_profile(file).map_err(BuiltinModelError)?
            else {
                continue;
            };
            let key = (project.package.to_bytes(), profile, file.path.to_owned());
            if identities.insert(key, file.source_identity).is_some() {
                return Err(BuiltinModelError(
                    "artifact source frontier contains a duplicate path".to_owned(),
                ));
            }
            required_profiles
                .entry(project.package.to_bytes())
                .or_default()
                .insert(profile);
        }
        Ok(Self {
            identities,
            required_profiles,
        })
    }

    /// Records the whole selected profile verdict, including an unavailable
    /// profile which supplied no activation. One incomplete Rust edition must
    /// also restrict the shared Rust source lane.
    pub(in crate::builtin) fn record_publication_scope(
        profiles: &mut BTreeMap<LanguageProfile, super::RetargetingScope>,
        key: &ProductSemanticPublicationKey,
        record: &ProductSemanticPublicationRecord,
    ) {
        let scope = profiles
            .entry(super::super::ingest::lane_profile(key.profile()))
            .or_insert(super::RetargetingScope::CompletePublication);
        match record {
            ProductSemanticPublicationRecord::Published {
                coverage: SemanticPublicationCoverage::Complete,
                ..
            } => {}
            ProductSemanticPublicationRecord::Published {
                coverage: SemanticPublicationCoverage::Partial(_),
                ..
            }
            | ProductSemanticPublicationRecord::Unavailable(_) => {
                *scope = super::RetargetingScope::SelectedArtifacts;
            }
        }
    }

    /// Name-based joins need every current profile in that language family.
    /// A missing selected TSX record cannot turn the surviving Complete TS
    /// images into a complete TypeScript name inventory.
    pub(in crate::builtin) fn retargeting_scopes(
        &self,
        package: backend_engine::PackageKey,
        profiles: &BTreeMap<LanguageProfile, super::RetargetingScope>,
    ) -> BTreeMap<backend_semantic::vocabulary::Language, super::RetargetingScope> {
        let mut scopes = BTreeMap::new();
        for (profile, verdict) in profiles {
            let scope = scopes
                .entry(profile.language())
                .or_insert(super::RetargetingScope::CompletePublication);
            if *verdict == super::RetargetingScope::SelectedArtifacts {
                *scope = super::RetargetingScope::SelectedArtifacts;
            }
        }
        for profile in self
            .required_profiles
            .get(&package.to_bytes())
            .into_iter()
            .flatten()
        {
            let scope = scopes
                .entry(profile.language())
                .or_insert(super::RetargetingScope::CompletePublication);
            if profiles.get(profile) != Some(&super::RetargetingScope::CompletePublication) {
                *scope = super::RetargetingScope::SelectedArtifacts;
            }
        }
        scopes
    }

    fn classify(
        &self,
        key: &ProductSemanticPublicationKey,
        path: &str,
        identity: ContentId<SourceFactDomain>,
    ) -> ArtifactSource {
        match self.identities.get(&(
            key.package_key().to_bytes(),
            super::super::ingest::lane_profile(key.profile()),
            path.to_owned(),
        )) {
            None => ArtifactSource::Absent,
            Some(None) => ArtifactSource::IdentityUnavailable,
            Some(Some(current)) if *current == identity => ArtifactSource::Current,
            Some(Some(_)) => ArtifactSource::Changed,
        }
    }

    /// Checks a Program path under its own admitted TS/TSX profile.
    pub(super) fn typescript_source_current(
        &self,
        package: backend_engine::PackageKey,
        path: &str,
        identity: ContentId<SourceFactDomain>,
    ) -> bool {
        let Some(profile) = super::super::ingest::admitted_file_profile(
            backend_engine::SourceLanguage::TypeScript, path,
        ) else { return false };
        self.identities.get(&(package.to_bytes(), super::super::ingest::lane_profile(profile), path.to_owned()))
            == Some(&Some(identity))
    }

    fn current_program(
        &self,
        key: &ProductSemanticPublicationKey,
        sources: &std::sync::Arc<backend_semantic::ir::NativeProgramSources>,
    ) -> Option<()> {
        for row in sources.sources() {
            let Some(path) = row.package_path else {
                continue;
            };
            let profile = super::super::ingest::admitted_file_profile(
                backend_engine::SourceLanguage::TypeScript,
                path,
            )?;
            if profile.language() != backend_semantic::vocabulary::Language::TypeScript
                || self.identities.get(&(
                    key.package_key().to_bytes(),
                    super::super::ingest::lane_profile(profile),
                    path.to_owned(),
                )) != Some(&Some(row.source.identity))
            {
                return None;
            }
        }
        Some(())
    }

    /// Keeps exact selected-image ownership while choosing the artifacts a
    /// Partial publication can answer from. The activation has already proven
    /// manifest, binding and immutable object membership; image admission binds
    /// the retained recipe/package/profile before its source is compared.
    /// Complete images remain available for the view's stale-row projection.
    pub(in crate::builtin) fn select(
        &self,
        key: &ProductSemanticPublicationKey,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
    ) -> Result<SelectedSourceArtifacts, BuiltinModelError> {
        self.select_with_policy(key, coverage, activated, residence, false, None)
    }

    /// References require current source even for a complete publication.
    pub(in crate::builtin) fn select_current(
        &self,
        key: &ProductSemanticPublicationKey,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
    ) -> Result<SelectedSourceArtifacts, BuiltinModelError> {
        self.select_with_policy(key, coverage, activated, residence, true, None)
    }

    /// Selects exact NSE-positive images and Program evidence for this same
    /// snapshot/activation, without asserting inventory completeness.
    pub(super) fn select_with_nse(
        &self,
        snapshot: &super::super::WorkspaceSnapshot,
        key: &ProductSemanticPublicationKey,
        claim: backend_engine::builtin::SemanticPublicationClaim,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
        proof: &super::semantic::NseProgramProof,
    ) -> Result<SelectedSourceArtifacts, BuiltinModelError> {
        if !proof.admits(snapshot, key, claim, coverage, &activated) {
            return Err(BuiltinModelError(
                "native source proof belongs to a different selected activation".into(),
            ));
        }
        let _ = residence;
        refuse_unqualified_nse_basis(proof.source_basis())
    }

    /// Current-only selection from the same admitted NSE Program proof.
    pub(in crate::builtin) fn select_current_with_nse(
        &self,
        snapshot: &super::super::WorkspaceSnapshot,
        key: &ProductSemanticPublicationKey,
        claim: backend_engine::builtin::SemanticPublicationClaim,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
        proof: &super::semantic::NseProgramProof,
    ) -> Result<SelectedSourceArtifacts, BuiltinModelError> {
        if !proof.admits(snapshot, key, claim, coverage, &activated) {
            return Err(BuiltinModelError(
                "native source proof belongs to a different selected activation".into(),
            ));
        }
        let _ = residence;
        refuse_unqualified_nse_basis(proof.source_basis())
    }

    /// Selects only source-proven artifacts into an explicitly observed,
    /// policy-qualified wrapper. It cannot yield an unqualified current program.
    pub(in crate::builtin) fn select_observed_with_nse(
        &self,
        snapshot: &super::super::WorkspaceSnapshot,
        key: &ProductSemanticPublicationKey,
        claim: backend_engine::builtin::SemanticPublicationClaim,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
        proof: &super::semantic::NseProgramProof,
    ) -> Result<ObservedNativeArtifacts, BuiltinModelError> {
        if !proof.admits(snapshot, key, claim, coverage, &activated) {
            return Err(BuiltinModelError(
                "native source proof belongs to a different selected activation".into(),
            ));
        }
        let basis = match proof.source_basis() {
            basis @ super::super::native_ts_admission::NativeTsSourceBasis::CapturedObserved {
                policy_stability: super::super::native_ts_admission::NativePolicyStability::Unknown,
            } => basis,
        };
        let artifacts = self.select_with_policy(key, coverage, activated, residence, true, Some(proof))?;
        if artifacts.program.is_none() {
            return Err(BuiltinModelError("native observed program is unavailable".into()));
        }
        Ok(ObservedNativeArtifacts { basis, artifacts })
    }

    fn select_with_policy(
        &self,
        key: &ProductSemanticPublicationKey,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
        current_only: bool,
        nse: Option<&super::semantic::NseProgramProof>,
    ) -> Result<SelectedSourceArtifacts, BuiltinModelError> {
        if !key.is_selected() {
            return Err(BuiltinModelError(
                "artifact availability requires the selected publication".to_owned(),
            ));
        }
        let lineage = key
            .lineage()
            .map_err(|error| BuiltinModelError(format!("semantic publication lineage: {error}")))?;
        let admission = image_rows::publication_admission(
            key.profile(),
            lineage.ecosystem,
            lineage.name,
            key.coordinate().as_str(),
        );
        let mut indices = Vec::new();
        for (index, image) in activated.images().iter().enumerate() {
            let opened = image_rows::open_compiled_snapshot(image, admission, residence)?;
            let (path, identity) = match &opened {
                CompiledImage::Opened {
                    path,
                    identity,
                    view,
                } => {
                    image_rows::bind_opened_image(image.as_ref(), admission, view, key, residence)?;
                    (path, *identity)
                }
                CompiledImage::Resident { path, identity, .. } => (path, *identity),
            };
            if (!current_only && matches!(coverage, SemanticPublicationCoverage::Complete))
                || self.classify(key, path, identity) == ArtifactSource::Current
                || nse.is_some_and(|proof| proof.contains(path, identity))
            {
                indices.push(index);
            }
        }
        let program = activated
            .native_program_sources
            .as_ref()
            .and_then(|sources| {
                if self.current_program(key, sources).is_none() && nse.is_none() {
                    return None;
                }
                let mappings = sources
                    .sources()
                    .filter_map(|row| row.package_path.map(|path| (path, row.source)))
                    .collect::<BTreeMap<_, _>>();
                for image in activated.images() {
                    use backend_semantic::ir::{ImageProvenance, SemanticCoreReader};
                    let view = image.reopen().ok()?;
                    let ImageProvenance::Captured {
                        source,
                        recipe,
                        scope,
                        ..
                    } = view.image_facts().provenance
                    else {
                        return None;
                    };
                    let path = std::str::from_utf8(view.atom(scope.path)?).ok()?;
                    if recipe.toolchain != sources.toolchain()
                        || mappings.get(path) != Some(&source)
                        || !sources.image_matches(
                            path,
                            backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(
                                image.as_ref(),
                            )
                            .as_ref(),
                        )
                    {
                        return None;
                    }
                }
                Some(CurrentNativeProgram {
                    sources: std::sync::Arc::clone(sources),
                    images: std::sync::Arc::clone(&activated.images),
                    indices: indices.clone(),
                })
            });
        Ok(SelectedSourceArtifacts {
            activated,
            indices,
            program,
        })
    }
}

/// Until consumers can carry the policy qualification, do not expose NSE
/// observations through an unqualified artifact selection.
fn refuse_unqualified_nse_basis<T>(
    basis: super::super::native_ts_admission::NativeTsSourceBasis,
) -> Result<T, BuiltinModelError> {
    match basis {
        super::super::native_ts_admission::NativeTsSourceBasis::CapturedObserved {
            policy_stability: super::super::native_ts_admission::NativePolicyStability::Unknown,
        } => Err(BuiltinModelError("native source policy stability is unknown; qualified projection is unavailable".into())),
    }
}

/// Source-proven artifacts whose discovery policy stability remains qualified.
/// This wrapper deliberately offers no conversion to unqualified artifacts.
pub(in crate::builtin) struct ObservedNativeArtifacts {
    basis: super::super::native_ts_admission::NativeTsSourceBasis,
    artifacts: SelectedSourceArtifacts,
}

impl ObservedNativeArtifacts {
    pub(in crate::builtin) fn source_basis(&self) -> super::super::native_ts_admission::NativeTsSourceBasis {
        self.basis
    }
    pub(in crate::builtin) fn has_excluded_images(&self) -> bool {
        self.artifacts.has_excluded_images()
    }
    pub(in crate::builtin) fn images(&self) -> impl ExactSizeIterator<Item = &backend_library::interface::SemanticImageSnapshot> {
        self.artifacts.images()
    }
    pub(in crate::builtin) fn program(&self) -> Option<ObservedNativeProgram<'_>> {
        self.artifacts.program.as_ref().map(|program| ObservedNativeProgram { program, basis: self.basis })
    }
}

/// Borrowed compiler membership for observed artifacts, including admitted
/// libraries. It cannot be converted into a CurrentNativeProgram and does not
/// claim discovery completeness or current effective-policy eligibility.
pub(in crate::builtin) struct ObservedNativeProgram<'a> {
    program: &'a CurrentNativeProgram,
    basis: super::super::native_ts_admission::NativeTsSourceBasis,
}

impl ObservedNativeProgram<'_> {
    pub(in crate::builtin) fn source_basis(&self) -> super::super::native_ts_admission::NativeTsSourceBasis {
        self.basis
    }
    pub(in crate::builtin) fn sources(&self) -> &backend_semantic::ir::NativeProgramSources {
        self.program.sources()
    }
    pub(in crate::builtin) fn images(&self) -> impl ExactSizeIterator<Item = &backend_library::interface::SemanticImageSnapshot> {
        self.program.images()
    }
}

/// Only artifacts admitted above can reach positive-fact consumers. This
/// owner retains the original selected activation; indices never address a
/// different image allocation. It makes no file or project completeness claim.
pub(in crate::builtin) struct SelectedSourceArtifacts {
    activated: ActivatedProductSemantics,
    indices: Vec<usize>,
    program: Option<CurrentNativeProgram>,
}

impl SelectedSourceArtifacts {
    /// Whether selected artifacts were excluded by the current source frontier.
    pub(in crate::builtin) fn has_excluded_images(&self) -> bool {
        self.indices.len() != self.activated.images().len()
    }

    pub(in crate::builtin) fn program(&self) -> Option<&CurrentNativeProgram> {
        self.program.as_ref()
    }
    pub(in crate::builtin) fn has_program_manifest(&self) -> bool {
        self.activated.native_program_sources.is_some()
    }

    pub(in crate::builtin) fn images(
        &self,
    ) -> impl ExactSizeIterator<Item = &backend_library::interface::SemanticImageSnapshot> {
        self.indices
            .iter()
            .map(|index| &self.activated.images()[*index])
    }
}

/// Immutable program membership with every mapped source admitted. This does
/// not claim complete discovery or structural declaration inventory. Only
/// SourceAvailability can construct it; an available image alone cannot.
pub(in crate::builtin) struct CurrentNativeProgram {
    sources: std::sync::Arc<backend_semantic::ir::NativeProgramSources>,
    // The exact immutable backing allocations must stay alive while their
    // process-local association keys are consumed. Source/path equality alone
    // cannot associate another activation's image with this program.
    images: std::sync::Arc<[backend_library::interface::SemanticImageSnapshot]>,
    indices: Vec<usize>,
}
impl CurrentNativeProgram {
    pub(in crate::builtin) fn sources(&self) -> &backend_semantic::ir::NativeProgramSources {
        &self.sources
    }

    pub(super) fn images(
        &self,
    ) -> impl ExactSizeIterator<Item = &backend_library::interface::SemanticImageSnapshot> {
        self.indices.iter().map(|index| &self.images[*index])
    }
}

/// Process-local identity of a whole borrowed image allocation. This is never
/// serialized and grants no authority without its retained program owner.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct NativeProgramImageBacking {
    address: usize,
    length: usize,
}

impl NativeProgramImageBacking {
    pub(super) fn of(bytes: &[u8]) -> Self {
        Self {
            address: bytes.as_ptr() as usize,
            length: bytes.len(),
        }
    }
}

#[cfg(test)]
mod nse_tests {
    use super::*;
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    #[test]
    fn unknown_policy_never_admits_unqualified_nse_artifacts() {
        let basis = super::super::super::native_ts_admission::NativeTsSourceBasis::CapturedObserved {
            policy_stability: super::super::super::native_ts_admission::NativePolicyStability::Unknown,
        };
        assert!(refuse_unqualified_nse_basis::<()>(basis).is_err());
    }

    #[test]
    fn observed_program_retains_unknown_policy_basis_when_borrowed() {
        // Synthetic wrapper fixture: this checks qualification propagation,
        // not compiler, source, or selected-workspace admission.
        use backend_semantic::ir::{NativeProgramSource, NativeProgramSourceManifest, SourceIdentity};
        use backend_version::ToolchainDomain;
        let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(b"observed fixture toolchain");
        let source = SourceIdentity::from_bytes(b"export const value = 1;\n").expect("bounded source");
        let manifest = NativeProgramSourceManifest::from_sources(toolchain, vec![NativeProgramSource {
            program_path: "workspace/value.ts".into(),
            package_path: Some("value.ts".into()),
            source,
        }]).expect("program fixture");
        let program: std::sync::Arc<backend_semantic::ir::NativeProgramSources> = std::sync::Arc::new(manifest.into());
        let images: std::sync::Arc<[backend_library::interface::SemanticImageSnapshot]> = std::sync::Arc::from([]);
        let basis = super::super::super::native_ts_admission::NativeTsSourceBasis::CapturedObserved {
            policy_stability: super::super::super::native_ts_admission::NativePolicyStability::Unknown,
        };
        let wrapper = ObservedNativeArtifacts {
            basis,
            artifacts: SelectedSourceArtifacts {
                activated: ActivatedProductSemantics {
                    images: std::sync::Arc::clone(&images),
                    native_program_sources: Some(std::sync::Arc::clone(&program)),
                },
                indices: Vec::new(),
                program: Some(CurrentNativeProgram { sources: program, images, indices: Vec::new() }),
            },
        };
        assert_eq!(wrapper.source_basis(), basis);
        let borrowed = wrapper.program().expect("observed program");
        assert_eq!(borrowed.source_basis(), basis);
        assert_eq!(borrowed.sources().sources().count(), 1);
    }

    #[test]
    fn typescript_program_sources_use_each_files_own_profile_and_identity() {
        let package = backend_engine::PackageKey::from_value("/fixture");
        let ts = ContentId::<SourceFactDomain>::from_canonical_bytes(b"typescript");
        let tsx = ContentId::<SourceFactDomain>::from_canonical_bytes(b"tsx");
        let mut identities = BTreeMap::new();
        identities.insert(
            (
                package.to_bytes(),
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                "dep.ts".to_owned(),
            ),
            Some(ts),
        );
        identities.insert(
            (
                package.to_bytes(),
                LanguageProfile::TypeScript(TypeScriptSource::Tsx),
                "view.tsx".to_owned(),
            ),
            Some(tsx),
        );
        let available = SourceAvailability {
            identities,
            required_profiles: BTreeMap::new(),
        };
        assert!(available.typescript_source_current(package, "dep.ts", ts));
        assert!(available.typescript_source_current(package, "view.tsx", tsx));
        assert!(!available.typescript_source_current(package, "dep.ts", tsx));
        assert!(!available.typescript_source_current(package, "missing.ts", ts));
        assert!(!available.typescript_source_current(package, "dep.css", ts));
    }
}
