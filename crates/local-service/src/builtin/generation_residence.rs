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

    fn stage_selected_generation(
        &self,
        replacement: bool,
    ) -> Result<backend_engine::application::StagedSemanticPackage, String> {
        use backend_engine::application::{OwnedPackageSource, OwnedPackageSourceSet};
        use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
        use backend_semantic::vocabulary::{CStandard, LanguageProfile, Stage};

        const ALPHA: &str = "int alpha(void) { return 1; }\n";
        const BETA: &str = "int beta(void) { return 2; }\n";
        const GAMMA: &str = "int gamma(void) { return 3; }\n";
        let package_url = self.key.coordinate().clone();
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(if replacement { 45 } else { 44 }),
                profile: LanguageProfile::C(CStandard::C23),
                stage: Stage::LowerIr,
            },
            package_url,
        )
        .map_err(|error| format!("build selected-history compiler request: {error:?}"))?;
        let (package_root, sources) = if replacement {
            (
                self.root.join("replacement"),
                vec![
                    OwnedPackageSource::new("src/gamma.c", GAMMA)
                        .map_err(|error| error.to_string())?,
                ],
            )
        } else {
            (
                self.root.join("package"),
                vec![
                    OwnedPackageSource::new("src/alpha.c", ALPHA)
                        .map_err(|error| error.to_string())?,
                    OwnedPackageSource::new("src/beta.c", BETA)
                        .map_err(|error| error.to_string())?,
                ],
            )
        };
        let sources = OwnedPackageSourceSet::new(request, package_root, sources.into_boxed_slice())
            .map_err(|error| format!("admit selected-history package sources: {error}"))?;
        self.client()?
            .compile_package_sources_staged(sources)
            .map_err(|error| format!("stage selected-history compiler output: {error}"))
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
        LocalRuntimeToolchain, OwnedPackageSource, OwnedPackageSourceSet, ToolchainProbeLimits,
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
    use std::time::Duration;

    let selected_clang = std::env::var_os("NUDOX_CLANG")
        .map(std::path::PathBuf::from)
        .ok_or("explicit NUDOX_CLANG is required for semantic generation residence")?;
    let libclang = std::env::var_os("LIBCLANG_PATH")
        .map(std::path::PathBuf::from)
        .ok_or("explicit LIBCLANG_PATH is required for semantic generation residence")?;
    let root = unique_directory().map_err(|error| error.to_string())?;
    let (clang, clang_authority, clang_toolchain) = match (|| -> Result<_, String> {
        let clang_authority =
            backend_frontend_clang::ClangAuthorityEnvironment::probe(&selected_clang, &libclang)
                .map_err(|error| format!("admit generation fixture Clang authority: {error}"))?;
        let clang = clang_authority.driver().to_path_buf();
        let probe_limits = ToolchainProbeLimits::new(
            Duration::from_secs(10),
            NonZeroUsize::new(16 * 1024).ok_or("toolchain probe output limit is zero")?,
        )
        .map_err(|error| error.to_string())?;
        let clang_toolchain =
            LocalRuntimeToolchain::probe(NativeTool::Clang, clang.clone(), probe_limits)
                .map_err(|error| format!("admit generation fixture Clang toolchain: {error}"))?;
        let clang_path = clang
            .canonicalize()
            .map_err(|error| format!("canonicalize generation fixture Clang driver: {error}"))?;
        if clang_authority.driver() != clang_path {
            return Err("generation fixture Clang authority and toolchain differ".to_owned());
        }
        Ok((clang, clang_authority, clang_toolchain))
    })() {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
    };
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
            vec![clang_toolchain].into_boxed_slice(),
            Box::new([]),
            LocalRuntimePackageAuthority {
                clang: Some(clang_authority),
                ..LocalRuntimePackageAuthority::default()
            },
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
    use backend_engine::builtin::{
        ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticPublicationClaim,
        SemanticPublicationCoverage,
    };
    use backend_library::interface::{SemanticImageAuthority, SemanticImageSnapshot};
    use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};
    use std::sync::{Arc, Mutex};

    const CLANG_FIXTURE_CHILD: &str = "BACKEND_GENERATION_RESIDENCE_CLANG_FIXTURE";

    /// Runs compiler-backed residence tests in a fresh process with an
    /// explicit Clang pair. The libclang selector is process-wide, so setting
    /// it in the parallel test harness would race unrelated tests.
    fn run_with_explicit_clang_fixture() -> bool {
        if std::env::var_os(CLANG_FIXTURE_CHILD).is_some() {
            return true;
        }

        let driver = std::env::var_os("NUDOX_CLANG").map(std::path::PathBuf::from);
        let library = std::env::var_os("LIBCLANG_PATH").map(std::path::PathBuf::from);
        let candidates = match (driver, library) {
            (Some(driver), Some(library)) => vec![(driver, library)],
            (None, None) => system_clang_fixture().unwrap_or_else(|error| {
                panic!("find a real Clang fixture for semantic residence tests: {error}")
            }),
            _ => panic!("set both NUDOX_CLANG and LIBCLANG_PATH for the Clang fixture"),
        };

        let thread = std::thread::current();
        let test_name = thread
            .name()
            .expect("named compiler-backed residence test")
            .to_owned();
        let test_executable = std::env::current_exe().expect("test executable");
        let mut failures = Vec::new();
        for (driver, library) in candidates {
            assert!(
                driver.is_absolute(),
                "fixture Clang driver must be absolute"
            );
            assert!(
                library.is_absolute(),
                "fixture libclang selector must be absolute"
            );
            let output = std::process::Command::new(&test_executable)
                .args(["--exact", &test_name, "--nocapture"])
                .env(CLANG_FIXTURE_CHILD, "1")
                .env("NUDOX_CLANG", &driver)
                .env("LIBCLANG_PATH", &library)
                .output()
                .expect("spawn isolated real-Clang fixture test");
            if output.status.success() {
                return false;
            }
            failures.push(format!(
                "{} + {}:\n{}{}",
                driver.display(),
                library.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        panic!(
            "all isolated compiler-backed fixture pairs failed:\n{}",
            failures.join("\n")
        )
    }

    fn system_clang_fixture() -> Result<Vec<(std::path::PathBuf, std::path::PathBuf)>, String> {
        #[cfg(target_os = "macos")]
        const CANDIDATES: &[(&str, &str)] = &[
            ("/usr/bin/clang", "/usr/lib/libclang.dylib"),
            (
                "/Library/Developer/CommandLineTools/usr/bin/clang",
                "/Library/Developer/CommandLineTools/usr/lib/libclang.dylib",
            ),
            (
                "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang",
                "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/libclang.dylib",
            ),
        ];
        #[cfg(target_os = "linux")]
        const CANDIDATES: &[(&str, &str)] = &[
            ("/usr/bin/clang-22", "/usr/lib/llvm-22/lib/libclang.so"),
            ("/usr/bin/clang-21", "/usr/lib/llvm-21/lib/libclang.so"),
            ("/usr/bin/clang-20", "/usr/lib/llvm-20/lib/libclang.so"),
            ("/usr/bin/clang-19", "/usr/lib/llvm-19/lib/libclang.so"),
            ("/usr/bin/clang-18", "/usr/lib/llvm-18/lib/libclang.so"),
            ("/usr/bin/clang-17", "/usr/lib/llvm-17/lib/libclang.so"),
            ("/usr/bin/clang-16", "/usr/lib/llvm-16/lib/libclang.so"),
            ("/usr/bin/clang-15", "/usr/lib/llvm-15/lib/libclang.so"),
            ("/usr/bin/clang-14", "/usr/lib/llvm-14/lib/libclang.so"),
            ("/usr/bin/clang", "/usr/lib/libclang.so"),
            ("/usr/local/bin/clang", "/usr/local/lib/libclang.so"),
        ];
        #[cfg(target_os = "windows")]
        const CANDIDATES: &[(&str, &str)] = &[(
            r"C:\Program Files\LLVM\bin\clang.exe",
            r"C:\Program Files\LLVM\bin",
        )];
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        const CANDIDATES: &[(&str, &str)] = &[];

        let mut candidates = nix_clang_fixture_candidates();
        candidates.extend(CANDIDATES.iter().map(|(driver, library)| {
            (
                std::path::PathBuf::from(driver),
                std::path::PathBuf::from(library),
            )
        }));

        let mut available = Vec::new();
        for (driver, library) in candidates {
            if !driver.is_file() || !library.exists() {
                continue;
            }
            let (Ok(driver), Ok(library)) = (driver.canonicalize(), library.canonicalize()) else {
                continue;
            };
            let pair = (driver, library);
            if !available.contains(&pair) {
                available.push(pair);
            }
        }
        if available.is_empty() {
            Err("no real compiler pair exists at the Nix store or fixed platform paths".to_owned())
        } else {
            Ok(available)
        }
    }

    /// Finds the active Nix Clang first, then its referenced libclang output.
    /// The child applies the narrow sysroot-query adapter and runs the real
    /// compiler before accepting a pair.
    #[cfg(unix)]
    fn nix_clang_fixture_candidates() -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
        use std::collections::BTreeSet;
        use std::path::Path;

        let store = Path::new("/nix/store");
        let Ok(entries) = std::fs::read_dir(store) else {
            return Vec::new();
        };
        let mut packages = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let version = nix_clang_package_version(&name)?.to_owned();
                Some((entry.path(), name, version))
            })
            .collect::<Vec<_>>();
        packages.sort_by(|left, right| left.1.cmp(&right.1));

        let path_drivers = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|directory| directory.join("clang"))
            .filter(|driver| driver.is_file())
            .filter_map(|driver| driver.canonicalize().ok())
            .filter(|driver| driver.starts_with(store))
            .collect::<BTreeSet<_>>();

        let mut drivers = Vec::new();
        let mut libraries = Vec::new();
        for (package, name, version) in packages {
            let lib_directory = package.join("lib");
            if lib_directory.is_dir()
                && let Ok(entries) = std::fs::read_dir(&lib_directory)
            {
                libraries.extend(entries.filter_map(Result::ok).filter_map(|entry| {
                    let path = entry.path();
                    (is_nix_libclang(&path) && path.is_file()).then_some((
                        version.clone(),
                        package.clone(),
                        path,
                    ))
                }));
            }

            let driver = package.join("bin/clang");
            if driver.is_file()
                && let Ok(driver) = driver.canonicalize()
            {
                drivers.push((
                    version,
                    driver.clone(),
                    path_drivers.contains(&driver),
                    name.contains("-clang-wrapper-"),
                    nix_store_references(&package),
                ));
            }
        }
        drivers.sort_by(|left, right| {
            right
                .2
                .cmp(&left.2)
                .then_with(|| right.3.cmp(&left.3))
                .then_with(|| left.1.cmp(&right.1))
        });
        libraries.sort_by(|left, right| left.2.cmp(&right.2));

        let mut referenced_candidates = Vec::new();
        let mut same_version_candidates = Vec::new();
        for (version, driver, _, _, references) in drivers {
            for (library_version, library_package, library) in &libraries {
                if version == *library_version {
                    let pair = (driver.clone(), library.clone());
                    if references.contains(library_package) {
                        referenced_candidates.push(pair);
                    } else {
                        same_version_candidates.push(pair);
                    }
                }
            }
        }
        let preferred_candidates = if referenced_candidates.is_empty() {
            same_version_candidates
        } else {
            referenced_candidates
        };
        let mut candidates = Vec::new();
        for pair in preferred_candidates {
            if !candidates.contains(&pair) {
                candidates.push(pair);
            }
        }
        candidates
    }

    #[cfg(not(unix))]
    fn nix_clang_fixture_candidates() -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
        Vec::new()
    }

    #[cfg(unix)]
    fn nix_clang_package_version(name: &str) -> Option<&str> {
        let suffix = name
            .rsplit_once("-clang-wrapper-")
            .map(|(_, suffix)| suffix)
            .or_else(|| name.rsplit_once("-clang-").map(|(_, suffix)| suffix))?;
        let version = suffix.split('-').next()?;
        (version
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
            && version.contains('.'))
        .then_some(version)
    }

    #[cfg(unix)]
    fn nix_store_references(
        path: &std::path::Path,
    ) -> std::collections::BTreeSet<std::path::PathBuf> {
        let Ok(output) = std::process::Command::new("nix-store")
            .args(["--query", "--references"])
            .arg(path)
            .output()
        else {
            return std::collections::BTreeSet::new();
        };
        if !output.status.success() {
            return std::collections::BTreeSet::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(std::path::PathBuf::from)
            .collect()
    }

    #[cfg(unix)]
    fn is_nix_libclang(path: &std::path::Path) -> bool {
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        #[cfg(target_os = "macos")]
        {
            filename == "libclang.dylib"
        }
        #[cfg(target_os = "linux")]
        {
            filename == "libclang.so"
                || filename.starts_with("libclang.so.")
                || (filename.starts_with("libclang-")
                    && filename.contains(".so")
                    && !filename.contains("-cpp."))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            filename == "libclang.dll" || filename == "clang.dll"
        }
    }

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
        if !run_with_explicit_clang_fixture() {
            return;
        }
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
        if !run_with_explicit_clang_fixture() {
            return;
        }
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
        if !run_with_explicit_clang_fixture() {
            return;
        }
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
        if !run_with_explicit_clang_fixture() {
            return;
        }
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

    fn admit_staged_candidate(
        authority: &mut super::super::semantic_authority::SemanticAuthority,
        key: &ProductSemanticPublicationKey,
        staged: &backend_engine::application::StagedSemanticPackage,
        source_count: u64,
    ) -> SemanticPublicationClaim {
        let observation = authority
            .observe(key, *staged.input_witness().input_root(), source_count)
            .expect("persist exact staged source observation");
        let attempt = authority
            .begin_candidate_attempt(key, &observation)
            .expect("begin exact staged candidate attempt");
        authority
            .publish_staged(key, attempt, staged, |_| Ok(()))
            .expect("admit immutable staged semantic candidate")
            .0
    }

    fn marker_intent(
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
        sources: &[(&str, &str)],
        removed_paths: &[&str],
    ) -> super::super::BuiltinIntent {
        use super::super::{BuiltinSemanticChange, BuiltinSourceChange};
        use backend_compile::SourceLanguage;

        let package = key.package_key();
        let label = key.package().as_str();
        let project = package.to_bytes();
        let mut files = sources
            .iter()
            .map(|(path, contents)| {
                let identity = *blake3::hash(contents.as_bytes()).as_bytes();
                let key = backend_engine::product_source_file_key(project, path);
                let row = backend_engine::ProductSourceRecord::file_within_row_capacity(
                    project,
                    *path,
                    SourceLanguage::Clang,
                    identity,
                    [0x62; 32],
                    Vec::new(),
                )
                .expect("admit source file row");
                (key, path, identity, row)
            })
            .collect::<Vec<_>>();
        files.sort_by_key(|(file_key, _, _, _)| *file_key);
        let file_keys = files
            .iter()
            .map(|(file_key, _, _, _)| *file_key)
            .collect::<Vec<_>>();
        let mut source_hasher = blake3::Hasher::new();
        source_hasher.update(b"native-history-marker-source-frontier.v1\0");
        for (_, path, identity, _) in &files {
            source_hasher.update(path.as_bytes());
            source_hasher.update(&[0]);
            source_hasher.update(identity);
        }
        let project_row = backend_engine::ProductSourceRecord::project(
            label,
            *source_hasher.finalize().as_bytes(),
            file_keys,
        )
        .expect("admit source project row");
        let mut source_changes = vec![BuiltinSourceChange {
            key: project,
            after: Some(project_row),
        }];
        source_changes.extend(
            files
                .into_iter()
                .map(|(file_key, _, _, row)| BuiltinSourceChange {
                    key: file_key,
                    after: Some(row),
                }),
        );
        source_changes.extend(removed_paths.iter().map(|path| BuiltinSourceChange {
            key: backend_engine::product_source_file_key(project, path),
            after: None,
        }));

        let generation = key
            .for_generation_bytes(*claim.binding().identity.as_ref())
            .expect("derive immutable generation relation key");
        let record = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim,
        };
        super::super::BuiltinIntent::index_with_semantics(
            package,
            label,
            source_changes,
            vec![
                BuiltinSemanticChange {
                    key: generation,
                    after: Some(record.clone()),
                },
                BuiltinSemanticChange {
                    key: key.clone(),
                    after: Some(record),
                },
            ],
        )
        .expect("construct source-plus-semantic workspace marker intent")
    }

    #[test]
    fn native_history_cas_holds_the_committed_marker_lease_across_two_generations() {
        if !run_with_explicit_clang_fixture() {
            return;
        }

        use backend_engine::application::StagedSemanticPackage;
        use backend_engine::{DaemonConfig, RelationAdmissionRegistry};
        use backend_replication::{
            FileSemanticRangeStore, HistoryRefKind, HistoryRefName, SemanticTargetKey,
            TransportLimits, TypedV3HistoryInputReplayStatus,
        };
        use backend_semantic::ir::{JumboRopeLimits, SemanticTypedPlaneVerificationTierV2};
        use std::time::{Duration, Instant};

        const SOURCE_A: [(&str, &str); 2] = [
            ("src/alpha.c", "int alpha(void) { return 1; }\n"),
            ("src/beta.c", "int beta(void) { return 2; }\n"),
        ];
        const SOURCE_B: [(&str, &str); 1] = [("src/gamma.c", "int gamma(void) { return 3; }\n")];

        let fixture = super::open_generation_fixture().expect("real Clang compiler fixture");
        let key = fixture.key().clone();
        let staged_a: StagedSemanticPackage = fixture
            .stage_selected_generation(false)
            .expect("stage generation A with the real compiler");
        let staged_b: StagedSemanticPackage = fixture
            .stage_selected_generation(true)
            .expect("stage generation B with the real compiler");
        let marker_workspace = fixture.root.join("selected-product-workspace");
        std::fs::create_dir_all(&marker_workspace).expect("create selected product workspace");

        let profile = super::super::profile_descriptor(super::super::BuiltinProfile::Product)
            .expect("admit product profile");
        let dispatcher = super::super::builtin_dispatcher(Some([0x79; 32]), profile, 1)
            .expect("configure product owner");
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<super::super::BuiltinWorkspaceRelation>()
            .expect("register source relation")
            .with_relation::<super::super::BuiltinSemanticRelation>()
            .expect("register semantic relation");
        let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
            &marker_workspace,
            super::super::BuiltinModel,
            super::super::genesis().expect("checked genesis"),
            dispatcher,
            DaemonConfig::default(),
            registry,
        )
        .expect("open product workspace owner");
        let mut authority =
            super::super::semantic_authority::SemanticAuthority::open(&marker_workspace)
                .expect("open product semantic authority");

        // Retain B before selecting A. Its immutable admitted closure is
        // available later, but neither Turso's candidate nor this row can
        // serve B until the matching workspace intent commits.
        let claim_b = admit_staged_candidate(
            &mut authority,
            &key,
            &staged_b,
            u64::try_from(SOURCE_B.len()).expect("source B count"),
        );
        let claim_a = admit_staged_candidate(
            &mut authority,
            &key,
            &staged_a,
            u64::try_from(SOURCE_A.len()).expect("source A count"),
        );
        assert_ne!(claim_a, claim_b, "compiler generations must differ");
        let intent_a = marker_intent(&key, claim_a, &SOURCE_A, &[]);
        let intent_b = marker_intent(&key, claim_b, &SOURCE_B, &["src/alpha.c", "src/beta.c"]);

        let (fence_reached, fence_reached_result) = std::sync::mpsc::sync_channel(1);
        let (fence_release, fence_release_result) = std::sync::mpsc::sync_channel(1);
        authority.install_native_history_fence_gate(fence_reached, fence_release_result);
        authority
            .commit_product_selection_changes(vec![(key.clone(), claim_a)], Vec::new(), || {
                super::super::commands::commit_builtin_intent(&mut daemon, 1, &intent_a)
            })
            .expect("commit source A and selected semantic claim A together");
        let stamp_a = authority
            .resolve_current_selected(&key)
            .expect("resolve committed generation A")
            .stamp();

        // The test-only gate is reached from the real V3 publication source
        // after it has acquired the same by-product read guard used by the
        // serving selector. It pauses the actual async publisher before the
        // replication API can begin the commit/ref CAS sequence.
        let fence_deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match fence_reached_result.try_recv() {
                Ok(()) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("real V3 publication dropped its marker-fence notification")
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }

            // This fixture uses the same owner-side history completion pump
            // that the product adapter runs while serving requests. Inspect
            // the exact status after every drain so a pre-fence refusal is
            // reported as its cause rather than disguised as a gate timeout.
            authority
                .drain_native_history_completions()
                .expect("pump native-history completions while awaiting the fence");
            let status = authority
                .native_history_status(&key, claim_a)
                .expect("read exact generation A native-history status");
            match &status {
                backend_engine::SemanticHistoryPublicationStatus::Refused { reason, .. } => {
                    panic!("real V3 publication refused before its marker fence: {reason}")
                }
                backend_engine::SemanticHistoryPublicationStatus::NotSelected => {
                    panic!("generation A stopped being selected before the marker fence")
                }
                backend_engine::SemanticHistoryPublicationStatus::Published { .. } => {
                    panic!("V3 publication completed without reaching its marker-fence gate")
                }
                _ => {}
            }
            assert!(
                Instant::now() < fence_deadline,
                "real V3 publication did not acquire the marker fence within 60 seconds; latest owner status: {status:?}; selected stamp: {stamp_a:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let loader = authority.native_history_loader_for_test();
        assert!(
            authority.native_history_reader_holds_selector(),
            "the production V3 fence must retain the product selector read lease"
        );

        let (writer_probe, writer_probe_result) = std::sync::mpsc::sync_channel(1);
        loader.install_native_history_writer_probe(writer_probe);
        let (writer_started, writer_started_result) = std::sync::mpsc::sync_channel(1);
        let store = authority.store();
        let target = SemanticTargetKey::new(
            key.package().as_str(),
            key.coordinate().as_str(),
            key.profile(),
        )
        .expect("derive selected history target");
        let branch = HistoryRefName::new("selected-native-v3").expect("valid history branch");
        let writer_target = target.clone();
        let writer_branch = branch.clone();
        let writer_key = key.clone();
        let writer = std::thread::spawn(move || {
            writer_started
                .send(())
                .expect("notify that generation B writer is starting");
            let update = authority.commit_product_selection_changes(
                vec![(writer_key, claim_b)],
                Vec::new(),
                || {
                    let history = FileSemanticRangeStore::open(
                        store.clone(),
                        TransportLimits::default(),
                    )
                    .map_err(|error| {
                        super::super::BuiltinModelError(format!(
                            "open history after generation A CAS: {error}"
                        ))
                    })?;
                    let selected = history
                        .history_ref(
                            &writer_target,
                            HistoryRefKind::Branch,
                            &writer_branch,
                        )
                        .map_err(|error| {
                            super::super::BuiltinModelError(format!(
                                "read V3 branch after generation A CAS: {error}"
                            ))
                        })?
                        .ok_or_else(|| {
                            super::super::BuiltinModelError(
                                "workspace marker writer passed the V3 fence before its branch CAS"
                                    .to_owned(),
                            )
                        })?;
                    let commit_a = selected.commit();
                    let ancestry = history
                        .history_ref_ancestry_proof(
                            &writer_target,
                            HistoryRefKind::Branch,
                            &writer_branch,
                            commit_a,
                        )
                        .map_err(|error| {
                            super::super::BuiltinModelError(format!(
                                "prove generation A on the V3 branch: {error}"
                            ))
                        })?;
                    let replay = history
                        .replay_typed_v3_history(
                            &writer_target,
                            HistoryRefKind::Branch,
                            &writer_branch,
                            commit_a,
                            &ancestry,
                            SemanticTypedPlaneVerificationTierV2::Standard,
                            JumboRopeLimits::default(),
                        )
                        .map_err(|error| {
                            super::super::BuiltinModelError(format!(
                                "cold-replay generation A before marker B: {error}"
                            ))
                        })?;
                    if replay.commit().selected_stamp() != stamp_a
                        || !replay.commit().parents().is_empty()
                        || replay.input_replay_status()
                            != TypedV3HistoryInputReplayStatus::Unproven
                    {
                        return Err(super::super::BuiltinModelError(
                            "generation A branch commit has the wrong stamp, ancestry, or input status"
                                .to_owned(),
                        ));
                    }
                    super::super::commands::commit_builtin_intent(&mut daemon, 2, &intent_b)?;
                    Ok(commit_a)
                },
            );
            (authority, daemon, update)
        });

        writer_started_result
            .recv_timeout(Duration::from_secs(2))
            .expect("generation B writer starts while generation A fence is held");
        assert_eq!(
            writer_probe_result
                .recv_timeout(Duration::from_secs(2))
                .expect("writer reaches the production selection write path"),
            true,
            "the real marker writer must observe the native-history read lease blocking its write"
        );
        assert!(
            loader.native_history_reader_holds_selector(),
            "the marker writer must remain excluded until V3 branch CAS completes"
        );
        // Let the admitted V3 publisher continue. The writer callback itself
        // independently reads and cold-replays A before it can durably install
        // B, so moving the product marker ahead of branch CAS fails this test.
        fence_release
            .send(())
            .expect("release V3 publisher after writer has blocked on the marker lease");

        let (mut authority, daemon, commit_a) = writer.join().expect("join product marker writer");
        let commit_a = commit_a.expect("generation B marker commits after generation A CAS");
        drop(daemon);

        let deadline = Instant::now() + Duration::from_secs(45);
        let (commit_b, proof_b) = loop {
            match authority
                .native_history_status(&key, claim_b)
                .expect("read exact selected generation B history status")
            {
                backend_engine::SemanticHistoryPublicationStatus::Published {
                    commit,
                    proof,
                    ..
                } => break (commit, proof),
                backend_engine::SemanticHistoryPublicationStatus::Pending { .. }
                | backend_engine::SemanticHistoryPublicationStatus::Deferred { .. }
                | backend_engine::SemanticHistoryPublicationStatus::NotRequested { .. } => {
                    assert!(
                        Instant::now() < deadline,
                        "generation B V3 publication timed out"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                status => panic!("generation B V3 publication did not succeed: {status:?}"),
            }
        };
        assert_eq!(proof_b.parent_commits.as_ref(), [*commit_a.as_bytes()]);
        assert_eq!(proof_b.reachable_commit, commit_b);

        let history = FileSemanticRangeStore::open(authority.store(), TransportLimits::default())
            .expect("reopen branch history store");
        let branch_tip = history
            .history_ref(&target, HistoryRefKind::Branch, &branch)
            .expect("read generation B branch")
            .expect("generation B branch exists");
        assert_eq!(branch_tip.commit().as_bytes(), &commit_b);
        let ancestry_b = history
            .history_ref_ancestry_proof(
                &target,
                HistoryRefKind::Branch,
                &branch,
                branch_tip.commit(),
            )
            .expect("prove generation B branch commit");
        let stamp_b = authority
            .resolve_current_selected(&key)
            .expect("resolve committed generation B")
            .stamp();
        assert_ne!(stamp_a, stamp_b);
        let replay_b = history
            .replay_typed_v3_history(
                &target,
                HistoryRefKind::Branch,
                &branch,
                branch_tip.commit(),
                &ancestry_b,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .expect("cold-replay generation B's exact typed closure");
        assert_eq!(replay_b.commit().selected_stamp(), stamp_b);
        assert_eq!(replay_b.commit().parents(), &[commit_a]);
        assert_eq!(
            replay_b.input_replay_status(),
            TypedV3HistoryInputReplayStatus::Unproven
        );
        drop(replay_b);
        drop(history);
        drop(authority);
        drop(fixture);
    }
}
