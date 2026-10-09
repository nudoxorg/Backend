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
    pub(in crate::builtin) fn of(sources: &IndexedSources) -> Result<Self, BuiltinModelError> {
        let mut identities = BTreeMap::new();
        let mut required_profiles = BTreeMap::<_, BTreeSet<_>>::new();
        for (file_key, record) in &sources.files {
            let file = record.file_fields().ok_or_else(|| {
                BuiltinModelError("artifact source frontier contains a non-file row".to_owned())
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
                super::super::ingest::source_profile(std::path::Path::new(file.path))
                    .map_err(BuiltinModelError)?
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

    fn current_program(
        &self,
        key: &ProductSemanticPublicationKey,
        sources: &std::sync::Arc<backend_semantic::ir::NativeProgramSourceManifest>,
    ) -> Option<()> {
        for row in sources.sources() {
            let Some(path) = row.package_path else {
                continue;
            };
            let profile =
                super::super::ingest::source_profile(std::path::Path::new(path)).ok()??;
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
    /// Existing Complete-publication freshness policy is unchanged here.
    pub(in crate::builtin) fn select(
        &self,
        key: &ProductSemanticPublicationKey,
        coverage: SemanticPublicationCoverage,
        activated: ActivatedProductSemantics,
        residence: &mut ImageRowResidence,
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
            if matches!(coverage, SemanticPublicationCoverage::Complete)
                || self.classify(key, path, identity) == ArtifactSource::Current
            {
                indices.push(index);
            }
        }
        let program = activated
            .native_program_sources
            .as_ref()
            .and_then(|sources| {
                self.current_program(key, sources)?;
                let mappings = sources
                    .sources()
                    .filter_map(|row| row.package_path.map(|path| (path, row.source)))
                    .collect::<BTreeMap<_, _>>();
                for image in activated.images() {
                    use backend_semantic::ir::{
                        ImageProvenance, SemanticCoreReader, SemanticReader,
                    };
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

/// Only artifacts admitted above can reach positive-fact consumers. This
/// owner retains the original selected activation; indices never address a
/// different image allocation. It makes no file or project completeness claim.
pub(in crate::builtin) struct SelectedSourceArtifacts {
    activated: ActivatedProductSemantics,
    indices: Vec<usize>,
    program: Option<CurrentNativeProgram>,
}

impl SelectedSourceArtifacts {
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

/// Full immutable program membership with every mapped current source admitted.
/// Only SourceAvailability can construct this value; an available image alone cannot.
pub(in crate::builtin) struct CurrentNativeProgram {
    sources: std::sync::Arc<backend_semantic::ir::NativeProgramSourceManifest>,
    // The exact immutable backing allocations must stay alive while their
    // process-local association keys are consumed. Source/path equality alone
    // cannot associate another activation's image with this program.
    images: std::sync::Arc<[backend_library::interface::SemanticImageSnapshot]>,
    indices: Vec<usize>,
}
impl CurrentNativeProgram {
    pub(super) fn sources(&self) -> &backend_semantic::ir::NativeProgramSourceManifest {
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
