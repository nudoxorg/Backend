//! Resident images for one immutable semantic generation.
//!
//! `load_semantic_publication` used to ask the compiler owner to reopen the
//! manifest and copy every image on each publish and each query. The claim's
//! manifest identity and binding identity already name that immutable closure.
//! A hit shares those image bytes. A failed activation is not remembered, and
//! each package, coordinate, and language profile keeps its own resident scope.

use backend_engine::builtin::ProductSemanticPublicationKey;
use backend_engine::builtin::SemanticPublicationClaim;
use backend_library::interface::SemanticImageSnapshot;
use std::collections::HashMap;
use std::sync::Arc;

/// Generations retained at once. One workspace walks every selected publication;
/// the least recently used closure leaves first.
const MAX_RESIDENT_GENERATIONS: usize = 4096;
/// Aggregate encoded-image bytes owned by cache entries. Decoded views borrow
/// these bytes and retain only fixed-size structural proofs.
const MAX_RESIDENT_BYTES: usize = 512 * 1024 * 1024;
/// Largest generation the selected loader may materialize. It checks selected
/// metadata before reading payloads; larger selections fail before allocation.
const MAX_RESIDENT_GENERATION_BYTES: usize = 128 * 1024 * 1024;
/// Image snapshots are small owners in addition to their payload bytes. Bound
/// both the count per generation and across the residence as well.
const MAX_RESIDENT_GENERATION_IMAGES: usize = 4096;
const MAX_RESIDENT_IMAGES: usize = 16 * 1024;

/// Manifest and binding identities of one immutable semantic generation.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct GenerationKey {
    scope: [u8; 32],
    manifest: [u8; 32],
    binding: [u8; 32],
}

impl GenerationKey {
    fn from_claim(claim: SemanticPublicationClaim, scope: [u8; 32]) -> Self {
        Self {
            scope,
            manifest: *claim.manifest().identity.as_ref(),
            binding: *claim.binding().identity.as_ref(),
        }
    }

    #[cfg(test)]
    fn from_parts(manifest: [u8; 32], binding: [u8; 32]) -> Self {
        Self {
            scope: [0; 32],
            manifest,
            binding,
        }
    }
}

/// Exact selected-closure loader installed by the local semantic authority.
///
/// The loader owns a snapshot of typed Turso selections and a CAS reader. It
/// does not retain the mutable authority database handle, so concurrent
/// semantic cache misses do not serialize on one authority mutex.
pub(crate) trait SelectedSemanticImageLoader: Send + Sync {
    fn load(
        &self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
        max_bytes: usize,
        max_images: usize,
    ) -> Result<Box<[SemanticImageSnapshot]>, super::BuiltinModelError>;
}

/// Shared semantic images for claims this process has already activated.
///
/// The byte charge covers each encoded image allocation held by this cache
/// once. Reopened semantic views borrow those bytes and cache only fixed-size
/// structural proofs. `Arc` clones returned to callers can keep bytes alive
/// after eviction and are outside the cache's eviction control.
pub(crate) struct SemanticGenerationResidence {
    budget: ResidenceBudget,
    images: HashMap<GenerationKey, ResidentGeneration>,
    access_epoch: u64,
    resident_bytes: usize,
    resident_images: usize,
    high_water_bytes: usize,
    uncached_oversized_generations: u64,
    owner_calls: u64,
    hits: u64,
    selected_loader: Option<Arc<dyn SelectedSemanticImageLoader>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResidenceBudget {
    max_generations: usize,
    max_bytes: usize,
    max_generation_bytes: usize,
    max_images: usize,
    max_generation_images: usize,
}

impl Default for ResidenceBudget {
    fn default() -> Self {
        Self {
            max_generations: MAX_RESIDENT_GENERATIONS,
            max_bytes: MAX_RESIDENT_BYTES,
            max_generation_bytes: MAX_RESIDENT_GENERATION_BYTES,
            max_images: MAX_RESIDENT_IMAGES,
            max_generation_images: MAX_RESIDENT_GENERATION_IMAGES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResidentImageWeight {
    bytes: usize,
    images: usize,
}

struct ResidentGeneration {
    images: Arc<[SemanticImageSnapshot]>,
    weight: ResidentImageWeight,
    last_access: u64,
}

/// Result of deciding whether one selected image set can be retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResidentAdmission {
    /// The exact immutable image set is held by the bounded residence.
    Retained(ResidentImageWeight),
    /// The generation remains usable for this call but exceeds its own byte cap.
    GenerationBytesExceeded { bytes: usize, maximum: usize },
    /// The generation remains usable for this call but has too many image owners.
    GenerationImagesExceeded { images: usize, maximum: usize },
    /// The metadata count overflowed while calculating the byte charge.
    WeightOverflow,
    /// Another activation for this exact key already installed a resident entry.
    AlreadyResident,
}

impl Default for SemanticGenerationResidence {
    fn default() -> Self {
        Self::with_limit(MAX_RESIDENT_GENERATIONS)
    }
}

impl SemanticGenerationResidence {
    fn with_limit(limit: usize) -> Self {
        let mut budget = ResidenceBudget::default();
        budget.max_generations = limit.max(1);
        Self::with_budget(budget)
    }

    fn with_budget(mut budget: ResidenceBudget) -> Self {
        budget.max_generations = budget.max_generations.clamp(1, MAX_RESIDENT_GENERATIONS);
        budget.max_bytes = budget.max_bytes.min(MAX_RESIDENT_BYTES);
        budget.max_images = budget.max_images.min(MAX_RESIDENT_IMAGES);
        budget.max_generation_bytes = budget
            .max_generation_bytes
            .min(budget.max_bytes)
            .min(MAX_RESIDENT_GENERATION_BYTES);
        budget.max_generation_images = budget
            .max_generation_images
            .min(budget.max_images)
            .min(MAX_RESIDENT_GENERATION_IMAGES);
        Self {
            budget,
            images: HashMap::new(),
            access_epoch: 0,
            resident_bytes: 0,
            resident_images: 0,
            high_water_bytes: 0,
            uncached_oversized_generations: 0,
            owner_calls: 0,
            hits: 0,
            selected_loader: None,
        }
    }

    pub(crate) fn install_selected_loader(&mut self, loader: Arc<dyn SelectedSemanticImageLoader>) {
        self.selected_loader = Some(loader);
    }

    pub(crate) fn has_selected_loader(&self) -> bool {
        self.selected_loader.is_some()
    }

    /// Owner activations performed, including failures.
    #[must_use]
    pub(super) fn owner_calls(&self) -> u64 {
        self.owner_calls
    }

    /// Cache hits. A hit does not call the owner.
    #[must_use]
    pub(super) fn hits(&self) -> u64 {
        self.hits
    }

    /// Returns the resident images for `claim`, activating through `activate` on a miss.
    ///
    /// # Errors
    ///
    /// Returns the activation error unchanged and does not remember it.
    pub(super) fn load<E>(
        &mut self,
        claim: SemanticPublicationClaim,
        activate: impl FnOnce() -> Result<Box<[SemanticImageSnapshot]>, E>,
    ) -> Result<Arc<[SemanticImageSnapshot]>, E> {
        self.recall(GenerationKey::from_claim(claim, [0; 32]), activate)
    }

    /// Returns selected images from the authority identified closure on a
    /// cache miss. A configured authority loader is mandatory for runtime
    /// claims, including misses after LRU eviction.
    pub(crate) fn load_selected(
        &mut self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
    ) -> Result<Arc<[SemanticImageSnapshot]>, super::BuiltinModelError> {
        let scope = publication_scope(key);
        let cache_key = GenerationKey::from_claim(claim, scope);
        if let Some(entry) = self.images.get(&cache_key) {
            let hit = Arc::clone(&entry.images);
            self.touch(cache_key);
            self.hits += 1;
            return Ok(hit);
        }
        self.owner_calls += 1;
        let loader = self.selected_loader.as_ref().ok_or_else(|| {
            super::BuiltinModelError("semantic authority image loader is unavailable".to_owned())
        })?;
        let images = Arc::from(loader.load(
            key,
            claim,
            self.budget.max_generation_bytes,
            self.budget.max_generation_images,
        )?);
        let _admission = self.remember(cache_key, Arc::clone(&images));
        Ok(images)
    }

    fn recall<E>(
        &mut self,
        key: GenerationKey,
        activate: impl FnOnce() -> Result<Box<[SemanticImageSnapshot]>, E>,
    ) -> Result<Arc<[SemanticImageSnapshot]>, E> {
        if let Some(entry) = self.images.get(&key) {
            let hit = Arc::clone(&entry.images);
            self.touch(key);
            self.hits += 1;
            return Ok(hit);
        }
        self.owner_calls += 1;
        let images = Arc::from(activate()?);
        let _admission = self.remember(key, Arc::clone(&images));
        Ok(images)
    }

    fn touch(&mut self, key: GenerationKey) {
        let epoch = self.next_access_epoch();
        if let Some(entry) = self.images.get_mut(&key) {
            entry.last_access = epoch;
        }
    }

    fn next_access_epoch(&mut self) -> u64 {
        if self.access_epoch == u64::MAX {
            // This can happen only after an impractical number of cache hits.
            // Rebase existing ranks once to keep the hot path O(1).
            let mut by_age = self
                .images
                .iter()
                .map(|(key, entry)| (entry.last_access, *key))
                .collect::<Vec<_>>();
            by_age.sort_unstable();
            for (rank, (_, key)) in by_age.into_iter().enumerate() {
                if let Some(entry) = self.images.get_mut(&key) {
                    entry.last_access = u64::try_from(rank + 1).unwrap_or(u64::MAX);
                }
            }
            self.access_epoch = u64::try_from(self.images.len()).unwrap_or(u64::MAX - 1);
        }
        self.access_epoch = self.access_epoch.saturating_add(1);
        self.access_epoch
    }

    fn remember(
        &mut self,
        key: GenerationKey,
        images: Arc<[SemanticImageSnapshot]>,
    ) -> ResidentAdmission {
        if self.images.contains_key(&key) {
            self.touch(key);
            return ResidentAdmission::AlreadyResident;
        }
        let Some(weight) = resident_weight(&images) else {
            self.uncached_oversized_generations =
                self.uncached_oversized_generations.saturating_add(1);
            return ResidentAdmission::WeightOverflow;
        };
        if weight.bytes > self.budget.max_generation_bytes {
            self.uncached_oversized_generations =
                self.uncached_oversized_generations.saturating_add(1);
            return ResidentAdmission::GenerationBytesExceeded {
                bytes: weight.bytes,
                maximum: self.budget.max_generation_bytes,
            };
        }
        if weight.images > self.budget.max_generation_images {
            self.uncached_oversized_generations =
                self.uncached_oversized_generations.saturating_add(1);
            return ResidentAdmission::GenerationImagesExceeded {
                images: weight.images,
                maximum: self.budget.max_generation_images,
            };
        }
        while self.images.len() >= self.budget.max_generations
            || self.resident_bytes.saturating_add(weight.bytes) > self.budget.max_bytes
            || self.resident_images.saturating_add(weight.images) > self.budget.max_images
        {
            let oldest = self
                .images
                .iter()
                .min_by_key(|(key, entry)| (entry.last_access, **key))
                .map(|(key, _)| *key);
            let Some(oldest) = oldest else {
                return ResidentAdmission::WeightOverflow;
            };
            if let Some(evicted) = self.images.remove(&oldest) {
                self.resident_bytes -= evicted.weight.bytes;
                self.resident_images -= evicted.weight.images;
            }
        }
        self.resident_bytes += weight.bytes;
        self.resident_images += weight.images;
        self.high_water_bytes = self.high_water_bytes.max(self.resident_bytes);
        let last_access = self.next_access_epoch();
        self.images.insert(
            key,
            ResidentGeneration {
                images,
                weight,
                last_access,
            },
        );
        ResidentAdmission::Retained(weight)
    }
}

fn resident_weight(images: &[SemanticImageSnapshot]) -> Option<ResidentImageWeight> {
    let bytes = images.iter().try_fold(0_usize, |total, image| {
        total.checked_add(image.as_ref().len())
    })?;
    Some(ResidentImageWeight {
        bytes,
        images: images.len(),
    })
}

fn publication_scope(key: &ProductSemanticPublicationKey) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.local-service.semantic-residence.v1\0");
    for part in [
        key.package().as_str().as_bytes(),
        key.coordinate().as_str().as_bytes(),
    ] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.update(&<[u8; 2]>::from(key.profile()));
    *hasher.finalize().as_bytes()
}

/// Times a real compiler reopen against a resident generation.
///
/// The package is compiled before either timer. `cold` reopens the generation
/// from the artifact directory. `warm` returns the same image allocation and
/// does not call the owner.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_generation() {
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let fixture = open_generation_fixture().expect("semantic generation fixture");
    let primed = {
        let mut residence = SemanticGenerationResidence::default();
        let primed = load_fixture(&fixture, &fixture.claim, &mut residence).expect("prime");
        (primed.images().len() == 2)
            .then_some(())
            .expect("generation fixture dropped an image");
        primed
    };
    let bytes: usize = primed
        .images()
        .iter()
        .map(|image| image.as_ref().len())
        .sum();
    let mut cold_owner_calls = 0_u64;
    for _ in 0..WARMUPS {
        let mut residence = SemanticGenerationResidence::default();
        let _ = load_fixture(&fixture, &fixture.claim, &mut residence).expect("cold warmup");
    }
    let mut cold = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let mut residence = SemanticGenerationResidence::default();
        let started = std::time::Instant::now();
        let images = load_fixture(&fixture, &fixture.claim, &mut residence).expect("cold");
        cold.push(started.elapsed().as_nanos());
        cold_owner_calls += residence.owner_calls();
        std::hint::black_box(images.images().len());
    }
    let mut warm_residence = SemanticGenerationResidence::default();
    let primed = load_fixture(&fixture, &fixture.claim, &mut warm_residence).expect("warm prime");
    for _ in 0..WARMUPS {
        let hit = load_fixture(&fixture, &fixture.claim, &mut warm_residence).expect("warm warmup");
        (Arc::ptr_eq(primed.image_set(), hit.image_set()))
            .then_some(())
            .expect("warm generation replaced the image allocation");
    }
    let owners_before = warm_residence.owner_calls();
    let hits_before = warm_residence.hits();
    let mut warm = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = std::time::Instant::now();
        let hit = load_fixture(&fixture, &fixture.claim, &mut warm_residence).expect("warm");
        warm.push(started.elapsed().as_nanos());
        std::hint::black_box(hit.image_set().as_ptr());
    }
    let warm_owner_calls = warm_residence.owner_calls() - owners_before;
    let warm_hits = warm_residence.hits() - hits_before;
    (cold_owner_calls == u64::try_from(SAMPLES).expect("sample count") && warm_owner_calls == 0)
        .then_some(())
        .expect("generation cache called the owner on a hit");
    let (cold_median, cold_p95) = percentiles(&cold);
    let (warm_median, warm_p95) = percentiles(&warm);
    println!(
        "semantic_generation images=2 bytes={bytes} cold_median_ns={cold_median} cold_p95_ns={cold_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} cold_owner_calls={cold_owner_calls} warm_owner_calls={warm_owner_calls} warm_hits={warm_hits}"
    );
}

fn load_fixture(
    fixture: &GenerationFixture,
    claim: &SemanticPublicationClaim,
    generations: &mut SemanticGenerationResidence,
) -> Result<super::ActivatedProductSemantics, super::BuiltinModelError> {
    let client = fixture.client().map_err(super::BuiltinModelError)?;
    super::load_semantic_publication(client, fixture.key(), *claim, generations)
}

struct GenerationFixture {
    root: std::path::PathBuf,
    /// Deleted by the adversarial test after the first successful activation.
    #[allow(dead_code)]
    artifacts: std::path::PathBuf,
    client: Option<backend_engine::application::LocalCompilerClient>,
    key: backend_engine::builtin::ProductSemanticPublicationKey,
    claim: SemanticPublicationClaim,
    /// A second compiled generation. The adversarial test reopens it after the
    /// artifact directory is gone, so the miss must reach the owner and fail.
    #[allow(dead_code)]
    replacement: SemanticPublicationClaim,
    /// Same claim, different language profile. Admission checks the profile;
    /// the cache key does not.
    #[allow(dead_code)]
    cxx_key: backend_engine::builtin::ProductSemanticPublicationKey,
}

impl GenerationFixture {
    fn client(&self) -> Result<&backend_engine::application::LocalCompilerClient, String> {
        self.client
            .as_ref()
            .ok_or_else(|| "compiler client is gone".to_owned())
    }

    fn key(&self) -> &backend_engine::builtin::ProductSemanticPublicationKey {
        &self.key
    }
}

impl Drop for GenerationFixture {
    fn drop(&mut self) {
        drop(self.client.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn open_generation_fixture() -> Result<GenerationFixture, String> {
    use backend_engine::application::{
        LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
        LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
        LocalRuntimeToolchain, OwnedPackageSource, OwnedPackageSourceSet,
    };
    use backend_engine::builtin::{ProductSemanticPublicationKey, SemanticPublicationClaim};
    use backend_library::interface::{
        CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
    };
    use backend_semantic::vocabulary::{
        CStandard, CxxStandard, LanguageProfile, NativeTool, Stage,
    };
    use backend_store::journal::PublicationLimits;
    use std::num::NonZeroUsize;
    use std::process::Command;
    use std::time::Duration;

    let clang = find_clang().ok_or("clang is required for semantic generation residence")?;
    let version = Command::new(&clang)
        .arg("--version")
        .output()
        .map_err(|error| error.to_string())?;
    if !version.status.success() {
        return Err("clang version probe failed".to_owned());
    }
    let root = unique_directory().map_err(|error| error.to_string())?;
    let package_root = root.join("package");
    let artifacts = root.join("artifacts");
    let native_work = root.join("native-work");
    if let Err(error) = std::fs::create_dir_all(package_root.join("src"))
        .and_then(|()| std::fs::create_dir(&native_work))
    {
        let _ = std::fs::remove_dir_all(&root);
        return Err(error.to_string());
    }
    let alpha = "int alpha(void) { return 1; }\n";
    let beta = "int beta(void) { return 2; }\n";
    let gamma = "int gamma(void) { return 3; }\n";
    let configuration = match (|| -> Result<_, String> {
        std::fs::write(package_root.join("src/alpha.c"), alpha)
            .map_err(|error| error.to_string())?;
        std::fs::write(package_root.join("src/beta.c"), beta).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(root.join("replacement/src")).map_err(|error| error.to_string())?;
        std::fs::write(root.join("replacement/src/gamma.c"), gamma)
            .map_err(|error| error.to_string())?;
        LocalCompilerRuntimeConfiguration::new(
            LocalCompilerRuntimePaths::new(
                artifacts.clone(),
                root.join("journal"),
                native_work.clone(),
            )
            .map_err(|error| error.to_string())?,
            vec![
                LocalRuntimeToolchain::resolved(NativeTool::Clang, clang, &version.stdout)
                    .map_err(|error| error.to_string())?,
            ]
            .into_boxed_slice(),
            Box::new([]),
            LocalRuntimePackageAuthority::default(),
            LocalCompilerTimeout::new(Duration::from_secs(30))
                .map_err(|error| error.to_string())?,
            PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
                .map_err(|error| error.to_string())?,
            LocalCompilerScratch::with_fragment_capacity(
                NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    })() {
        Ok(configuration) => configuration,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
    };
    let client = match LocalCompilerClient::start(configuration) {
        Ok(client) => client,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_string());
        }
    };
    let package_url = match PackageUrl::try_from("pkg:generic/sample@1.0.0".to_owned()) {
        Ok(url) => url,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(format!("package URL rejected: {error:?}"));
        }
    };
    let request = match PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(41),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package_url.clone(),
    ) {
        Ok(request) => request,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(format!("package profile rejected: {error:?}"));
        }
    };
    let published = match client.compile_package_sources(
        match OwnedPackageSourceSet::new(
            request.clone(),
            package_root,
            vec![
                match OwnedPackageSource::new("src/alpha.c", alpha) {
                    Ok(source) => source,
                    Err(error) => {
                        drop(client);
                        let _ = std::fs::remove_dir_all(&root);
                        return Err(error.to_string());
                    }
                },
                match OwnedPackageSource::new("src/beta.c", beta) {
                    Ok(source) => source,
                    Err(error) => {
                        drop(client);
                        let _ = std::fs::remove_dir_all(&root);
                        return Err(error.to_string());
                    }
                },
            ]
            .into_boxed_slice(),
        ) {
            Ok(sources) => sources,
            Err(error) => {
                drop(client);
                let _ = std::fs::remove_dir_all(&root);
                return Err(error.to_string());
            }
        },
    ) {
        Ok(published) => published,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_string());
        }
    };
    let replacement = match client.compile_package_sources(
        match OwnedPackageSourceSet::new(
            request,
            root.join("replacement"),
            vec![match OwnedPackageSource::new("src/gamma.c", gamma) {
                Ok(source) => source,
                Err(error) => {
                    drop(client);
                    let _ = std::fs::remove_dir_all(&root);
                    return Err(error.to_string());
                }
            }]
            .into_boxed_slice(),
        ) {
            Ok(sources) => sources,
            Err(error) => {
                drop(client);
                let _ = std::fs::remove_dir_all(&root);
                return Err(error.to_string());
            }
        },
    ) {
        Ok(published) => published,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_string());
        }
    };
    let claim = match SemanticPublicationClaim::admit(
        published.publication.manifest,
        published.publication.binding,
    ) {
        Ok(claim) => claim,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_owned());
        }
    };
    let replacement_claim = match SemanticPublicationClaim::admit(
        replacement.publication.manifest,
        replacement.publication.binding,
    ) {
        Ok(claim) => claim,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_owned());
        }
    };
    let package =
        match backend_engine::PackageReference::parse("pkg:generic/sample@1.0.0".to_owned()) {
            Ok(package) => package,
            Err(error) => {
                drop(client);
                let _ = std::fs::remove_dir_all(&root);
                return Err(error.to_string());
            }
        };
    let key = match ProductSemanticPublicationKey::new(
        package.clone(),
        package_url.clone(),
        LanguageProfile::C(CStandard::C23),
    ) {
        Ok(key) => key,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_owned());
        }
    };
    let cxx_key = match ProductSemanticPublicationKey::new(
        package,
        package_url,
        LanguageProfile::Cxx(CxxStandard::Cxx23),
    ) {
        Ok(key) => key,
        Err(error) => {
            drop(client);
            let _ = std::fs::remove_dir_all(&root);
            return Err(error.to_owned());
        }
    };
    Ok(GenerationFixture {
        root,
        artifacts,
        client: Some(client),
        key,
        claim,
        replacement: replacement_claim,
        cxx_key,
    })
}

fn find_clang() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join("clang"))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

fn unique_directory() -> Result<std::path::PathBuf, std::io::Error> {
    for ordinal in 0_u16..64 {
        let path = std::env::temp_dir().join(format!(
            "semantic-generation-{}-{ordinal}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "semantic generation fixture capacity exhausted",
    ))
}

#[allow(clippy::indexing_slicing)]
fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        GenerationKey, SelectedSemanticImageLoader, SemanticGenerationResidence, resident_weight,
    };
    use backend_engine::builtin::{ProductSemanticPublicationKey, SemanticPublicationClaim};
    use backend_library::interface::{SemanticImageAuthority, SemanticImageSnapshot};
    use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};
    use std::sync::{Arc, Mutex};

    struct SnapshotLoader {
        // Backing data stands in for already durable selected CAS members;
        // these copies are not produced by a cache-miss load.
        images: Vec<SemanticImageSnapshot>,
        checks: Arc<Mutex<usize>>,
        materializations: Arc<Mutex<usize>>,
    }

    impl SelectedSemanticImageLoader for SnapshotLoader {
        fn load(
            &self,
            _key: &ProductSemanticPublicationKey,
            _claim: SemanticPublicationClaim,
            max_bytes: usize,
            max_images: usize,
        ) -> Result<Box<[SemanticImageSnapshot]>, super::super::BuiltinModelError> {
            *self.checks.lock().expect("checks") += 1;
            let weight = resident_weight(&self.images).expect("fixture weight");
            if weight.bytes > max_bytes || weight.images > max_images {
                return Err(super::super::BuiltinModelError(
                    "fixture exceeds residence limit".to_owned(),
                ));
            }
            *self.materializations.lock().expect("materializations") += 1;
            self.images
                .iter()
                .map(|image| {
                    image.try_clone().map_err(|error| {
                        super::super::BuiltinModelError(format!("clone fixture: {error:?}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Vec::into_boxed_slice)
        }
    }

    fn copies(images: &[SemanticImageSnapshot]) -> Vec<SemanticImageSnapshot> {
        images
            .iter()
            .map(|image| image.try_clone().expect("image clone"))
            .collect()
    }

    fn loader(
        images: &[SemanticImageSnapshot],
    ) -> (Arc<SnapshotLoader>, Arc<Mutex<usize>>, Arc<Mutex<usize>>) {
        let checks = Arc::new(Mutex::new(0));
        let materializations = Arc::new(Mutex::new(0));
        (
            Arc::new(SnapshotLoader {
                images: copies(images),
                checks: Arc::clone(&checks),
                materializations: Arc::clone(&materializations),
            }),
            checks,
            materializations,
        )
    }

    fn snapshot(bytes: &[u8]) -> SemanticImageSnapshot {
        let authority = SemanticImageAuthority {
            identity:
                ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_encoded_bytes(
                    bytes,
                ),
            byte_len: u32::try_from(bytes.len()).expect("snapshot length"),
        };
        SemanticImageSnapshot::try_from_reopened(authority, bytes).expect("snapshot")
    }

    fn images(bytes: &[u8]) -> Box<[SemanticImageSnapshot]> {
        vec![snapshot(bytes)].into_boxed_slice()
    }

    #[test]
    fn a_second_load_shares_the_image_bytes_and_skips_the_owner() {
        let mut residence = SemanticGenerationResidence::default();
        let key = GenerationKey::from_parts([1; 32], [2; 32]);
        let mut calls = 0_u32;
        let first = residence
            .recall(key, || {
                calls += 1;
                Ok::<_, &str>(images(b"alpha"))
            })
            .expect("cold");
        let second = residence
            .recall(key, || {
                calls += 1;
                Ok::<_, &str>(images(b"other"))
            })
            .expect("warm");
        assert_eq!(calls, 1);
        assert_eq!(residence.owner_calls(), 1);
        assert_eq!(residence.hits(), 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second[0].as_ref(), b"alpha");
    }

    #[test]
    fn a_different_binding_misses() {
        let mut residence = SemanticGenerationResidence::default();
        let first_key = GenerationKey::from_parts([1; 32], [2; 32]);
        let second_key = GenerationKey::from_parts([1; 32], [3; 32]);
        let mut calls = 0_u32;
        residence
            .recall(first_key, || {
                calls += 1;
                Ok::<_, &str>(images(b"alpha"))
            })
            .expect("first");
        let second = residence
            .recall(second_key, || {
                calls += 1;
                Ok::<_, &str>(images(b"beta"))
            })
            .expect("second");
        assert_eq!(calls, 2);
        assert_eq!(second[0].as_ref(), b"beta");
    }

    #[test]
    fn a_different_manifest_misses() {
        let mut residence = SemanticGenerationResidence::default();
        let first_key = GenerationKey::from_parts([1; 32], [2; 32]);
        let second_key = GenerationKey::from_parts([9; 32], [2; 32]);
        let mut calls = 0_u32;
        residence
            .recall(first_key, || {
                calls += 1;
                Ok::<_, &str>(images(b"alpha"))
            })
            .expect("first");
        let second = residence
            .recall(second_key, || {
                calls += 1;
                Ok::<_, &str>(images(b"other-manifest"))
            })
            .expect("second");
        assert_eq!(calls, 2);
        assert_eq!(second[0].as_ref(), b"other-manifest");
    }

    #[test]
    fn a_failed_activation_is_not_remembered() {
        let mut residence = SemanticGenerationResidence::default();
        let key = GenerationKey::from_parts([4; 32], [5; 32]);
        let mut calls = 0_u32;
        let failed = residence.recall(key, || {
            calls += 1;
            Err::<Box<[SemanticImageSnapshot]>, &str>("disk")
        });
        assert_eq!(failed.expect_err("failure"), "disk");
        let failed_again = residence.recall(key, || {
            calls += 1;
            Err::<Box<[SemanticImageSnapshot]>, &str>("disk")
        });
        assert_eq!(failed_again.expect_err("second failure"), "disk");
        assert_eq!(calls, 2);
        assert_eq!(residence.hits(), 0);
        let stored = residence
            .recall(key, || {
                calls += 1;
                Ok::<_, &str>(images(b"kept"))
            })
            .expect("stored");
        let hit = residence
            .recall(key, || {
                calls += 1;
                Ok::<_, &str>(images(b"ignored"))
            })
            .expect("hit");
        assert_eq!(calls, 3);
        assert!(Arc::ptr_eq(&stored, &hit));
        assert_eq!(hit[0].as_ref(), b"kept");
    }

    #[test]
    fn evicting_one_generation_reloads_it_and_keeps_the_neighbor() {
        let mut residence = SemanticGenerationResidence::with_limit(2);
        let alpha = GenerationKey::from_parts([1; 32], [1; 32]);
        let beta = GenerationKey::from_parts([2; 32], [2; 32]);
        let gamma = GenerationKey::from_parts([3; 32], [3; 32]);
        residence
            .recall(alpha, || Ok::<_, &str>(images(b"alpha")))
            .expect("alpha");
        residence
            .recall(beta, || Ok::<_, &str>(images(b"beta")))
            .expect("beta");
        residence
            .recall(alpha, || Ok::<_, &str>(images(b"alpha-again")))
            .expect("touch alpha");
        residence
            .recall(gamma, || Ok::<_, &str>(images(b"gamma")))
            .expect("gamma");
        let mut calls = 0_u32;
        let alpha_hit = residence
            .recall(alpha, || {
                calls += 1;
                Ok::<_, &str>(images(b"alpha-reloaded"))
            })
            .expect("alpha stayed");
        assert_eq!(calls, 0);
        assert_eq!(alpha_hit[0].as_ref(), b"alpha");
        let beta_again = residence
            .recall(beta, || {
                calls += 1;
                Ok::<_, &str>(images(b"beta-reloaded"))
            })
            .expect("beta reloaded");
        assert_eq!(calls, 1);
        assert_eq!(beta_again[0].as_ref(), b"beta-reloaded");
    }

    #[test]
    fn a_deleted_artifact_directory_still_serves_the_admitted_generation() {
        let fixture = super::open_generation_fixture().expect("fixture");
        let mut generations = SemanticGenerationResidence::default();
        let cold = super::load_fixture(&fixture, &fixture.claim, &mut generations).expect("cold");
        let cold_bytes = cold
            .images()
            .iter()
            .map(|image| image.as_ref().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(generations.owner_calls(), 1);
        let shared = super::super::load_semantic_publication(
            fixture.client().expect("client"),
            &fixture.cxx_key,
            fixture.claim,
            &mut generations,
        )
        .expect("cxx profile shares the claim");
        assert_eq!(generations.owner_calls(), 1);
        assert!(Arc::ptr_eq(cold.image_set(), shared.image_set()));
        let rejected = super::super::activate_semantic_publication(
            fixture.client().expect("client"),
            &fixture.cxx_key,
            fixture.claim,
            &mut generations,
            &mut super::super::view_build::ImageRowResidence::default(),
        );
        let message = rejected.err().map(|error| error.to_string());
        assert!(
            message
                .as_deref()
                .is_some_and(|text| { text.contains("bind semantic publication to product key") }),
            "a cxx key must reject a cached c image: {message:?}"
        );
        assert_eq!(generations.owner_calls(), 1);
        std::fs::remove_dir_all(&fixture.artifacts).expect("delete artifacts");
        let warm = super::load_fixture(&fixture, &fixture.claim, &mut generations).expect("warm");
        assert_eq!(generations.owner_calls(), 1);
        assert!(Arc::ptr_eq(cold.image_set(), warm.image_set()));
        for (left, right) in cold_bytes.iter().zip(warm.images()) {
            assert_eq!(left.as_slice(), right.as_ref());
        }
        let missing = super::load_fixture(&fixture, &fixture.replacement, &mut generations);
        assert!(
            missing.is_err(),
            "a different binding must reopen from disk"
        );
        assert_eq!(generations.owner_calls(), 2);
        let missing_again = super::load_fixture(&fixture, &fixture.replacement, &mut generations);
        assert!(
            missing_again.is_err(),
            "a failed activation must not be remembered"
        );
        assert_eq!(generations.owner_calls(), 3);
        let still = super::load_fixture(&fixture, &fixture.claim, &mut generations).expect("still");
        assert_eq!(generations.owner_calls(), 3);
        assert!(Arc::ptr_eq(cold.image_set(), still.image_set()));
    }

    #[test]
    fn byte_budget_evicts_asymmetric_valid_generations_and_reopens_them() {
        let fixture = super::open_generation_fixture().expect("fixture");
        let mut expected_residence = SemanticGenerationResidence::default();
        let initial = super::load_fixture(&fixture, &fixture.claim, &mut expected_residence)
            .expect("open two-image publication");
        let replacement =
            super::load_fixture(&fixture, &fixture.replacement, &mut expected_residence)
                .expect("open one-image publication");
        let initial_bytes = initial
            .images()
            .iter()
            .map(|image| image.as_ref().to_vec())
            .collect::<Vec<_>>();
        let replacement_bytes = replacement
            .images()
            .iter()
            .map(|image| image.as_ref().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(initial_bytes.len(), 2);
        assert_eq!(replacement_bytes.len(), 1);
        let initial_weight = super::resident_weight(initial.images()).expect("initial weight");
        let replacement_weight =
            super::resident_weight(replacement.images()).expect("replacement weight");
        assert!(initial_weight.bytes > replacement_weight.bytes);
        assert!(initial_bytes.iter().all(|bytes| !bytes.is_empty()));
        assert!(!replacement_bytes[0].is_empty());
        assert_ne!(initial_bytes[0], replacement_bytes[0]);

        let max_bytes = initial_weight
            .bytes
            .checked_add(replacement_weight.bytes)
            .expect("combined fixture size")
            - 1;
        let mut budget = super::ResidenceBudget::default();
        budget.max_bytes = max_bytes;
        budget.max_generation_bytes = initial_weight.bytes.max(replacement_weight.bytes);
        budget.max_images = 8;
        budget.max_generation_images = 4;
        let mut bounded = SemanticGenerationResidence::with_budget(budget);

        let resident_initial =
            super::load_fixture(&fixture, &fixture.claim, &mut bounded).expect("initial");
        assert_eq!(
            bounded.resident_bytes, initial_weight.bytes,
            "the first generation is charged by exact image payload bytes"
        );
        let resident_replacement =
            super::load_fixture(&fixture, &fixture.replacement, &mut bounded).expect("replacement");
        assert_eq!(resident_replacement.images().len(), 1);
        assert_eq!(
            bounded.images.len(),
            1,
            "the byte limit evicts the older set"
        );
        assert_eq!(bounded.resident_bytes, replacement_weight.bytes);
        assert!(bounded.high_water_bytes <= max_bytes);
        assert!(bounded.resident_bytes <= max_bytes);
        drop(resident_initial);
        drop(resident_replacement);

        let reopened_initial =
            super::load_fixture(&fixture, &fixture.claim, &mut bounded).expect("reload initial");
        assert_eq!(
            bounded.owner_calls(),
            3,
            "evicted data reopens from its exact claim"
        );
        assert_eq!(bounded.resident_bytes, initial_weight.bytes);
        assert!(bounded.high_water_bytes <= max_bytes);
        assert_eq!(reopened_initial.images().len(), initial_bytes.len());
        for (expected, observed) in initial_bytes.iter().zip(reopened_initial.images()) {
            assert_eq!(expected.as_slice(), observed.as_ref());
        }

        // A fresh residence models process restart: no in-memory entry is
        // trusted, so the replacement is reopened from its durable claim.
        let mut cold = SemanticGenerationResidence::with_budget(budget);
        let cold_replacement =
            super::load_fixture(&fixture, &fixture.replacement, &mut cold).expect("cold reopen");
        assert_eq!(cold.owner_calls(), 1);
        assert_eq!(cold.resident_bytes, replacement_weight.bytes);
        assert!(cold.high_water_bytes <= max_bytes);
        assert_eq!(cold_replacement.images().len(), replacement_bytes.len());
        assert_eq!(
            cold_replacement.images()[0].as_ref(),
            replacement_bytes[0].as_slice()
        );
    }

    #[test]
    fn over_limit_generation_is_not_retained() {
        let fixture = super::open_generation_fixture().expect("fixture");
        let mut expected_residence = SemanticGenerationResidence::default();
        let expected = super::load_fixture(&fixture, &fixture.claim, &mut expected_residence)
            .expect("open valid generation");
        let expected_bytes = expected
            .images()
            .iter()
            .map(|image| image.as_ref().to_vec())
            .collect::<Vec<_>>();
        let total_bytes = super::resident_weight(expected.images())
            .expect("generation weight")
            .bytes;
        assert!(total_bytes > 1);

        let mut budget = super::ResidenceBudget::default();
        budget.max_bytes = total_bytes;
        budget.max_generation_bytes = total_bytes - 1;
        let mut residence = SemanticGenerationResidence::with_budget(budget);
        let first = super::load_fixture(&fixture, &fixture.claim, &mut residence)
            .expect("valid over-limit generation remains usable");
        assert_eq!(first.images().len(), expected_bytes.len());
        for (expected, observed) in expected_bytes.iter().zip(first.images()) {
            assert_eq!(expected.as_slice(), observed.as_ref());
        }
        assert!(residence.images.is_empty());
        assert_eq!(residence.resident_bytes, 0);
        assert_eq!(residence.uncached_oversized_generations, 1);

        // This caller still holds the first Arc while the next activation is
        // performed. Such outstanding clones are outside cache eviction
        // control; the residence never adds either allocation to its budget.
        let second = super::load_fixture(&fixture, &fixture.claim, &mut residence)
            .expect("uncached generation reopens on the next call");
        assert_eq!(residence.owner_calls(), 2);
        assert!(!Arc::ptr_eq(first.image_set(), second.image_set()));
        assert!(residence.images.is_empty());
        assert_eq!(residence.resident_bytes, 0);
    }

    #[test]
    fn selected_loader_checks_entry_cap_before_copying_payloads() {
        let fixture = super::open_generation_fixture().expect("fixture");
        let mut source_residence = SemanticGenerationResidence::default();
        let source =
            super::load_fixture(&fixture, &fixture.claim, &mut source_residence).expect("source");
        let weight = resident_weight(source.images()).expect("source weight");
        assert!(weight.bytes > 1);
        assert_eq!(weight.images, 2);

        let mut too_small = super::ResidenceBudget::default();
        too_small.max_bytes = weight.bytes;
        too_small.max_generation_bytes = weight.bytes - 1;
        too_small.max_generation_images = weight.images;
        let (bounded_loader, checks, materializations) = loader(source.images());
        let mut residence = SemanticGenerationResidence::with_budget(too_small);
        residence.install_selected_loader(bounded_loader);
        assert!(
            residence
                .load_selected(fixture.key(), fixture.claim)
                .is_err()
        );
        assert_eq!(*checks.lock().expect("checks"), 1);
        assert_eq!(
            *materializations.lock().expect("materializations"),
            0,
            "the loader rejects from the selected metadata inventory before materializing the result"
        );
        assert!(residence.images.is_empty());
        assert_eq!(residence.resident_bytes, 0);

        let mut exact = super::ResidenceBudget::default();
        exact.max_bytes = weight.bytes;
        exact.max_generation_bytes = weight.bytes;
        exact.max_images = weight.images;
        exact.max_generation_images = weight.images;
        let (admitted_loader, admitted_checks, admitted_materializations) = loader(source.images());
        let mut admitted = SemanticGenerationResidence::with_budget(exact);
        admitted.install_selected_loader(admitted_loader);
        let reopened = admitted
            .load_selected(fixture.key(), fixture.claim)
            .expect("entry at exact byte cap");
        assert_eq!(*admitted_checks.lock().expect("checks"), 1);
        assert_eq!(
            *admitted_materializations.lock().expect("materializations"),
            1
        );
        assert_eq!(admitted.resident_bytes, weight.bytes);
        assert!(admitted.high_water_bytes <= weight.bytes);
        for (expected, observed) in source.images().iter().zip(reopened.iter()) {
            assert_eq!(expected.as_ref(), observed.as_ref());
        }
    }
}
