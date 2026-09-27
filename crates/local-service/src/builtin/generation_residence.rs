//! Resident images for one immutable semantic generation.
//!
//! `load_semantic_publication` used to ask the compiler owner to reopen the
//! manifest and copy every image on each publish and each query. The claim's
//! manifest identity and binding identity already name that immutable closure.
//! A hit shares those image bytes. A failed activation is not remembered, and
//! the language profile stays out of the key because admission checks it later.

use backend_engine::builtin::SemanticPublicationClaim;
use backend_library::interface::SemanticImageSnapshot;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Generations retained at once. One workspace walks every selected publication;
/// the least recently used closure leaves first.
const MAX_RESIDENT_GENERATIONS: usize = 4096;

/// Manifest and binding identities of one immutable semantic generation.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct GenerationKey {
    manifest: [u8; 32],
    binding: [u8; 32],
}

impl GenerationKey {
    fn from_claim(claim: SemanticPublicationClaim) -> Self {
        Self {
            manifest: *claim.manifest().identity.as_ref(),
            binding: *claim.binding().identity.as_ref(),
        }
    }

    #[cfg(test)]
    fn from_parts(manifest: [u8; 32], binding: [u8; 32]) -> Self {
        Self { manifest, binding }
    }
}

/// Shared semantic images for claims this process has already activated.
pub(crate) struct SemanticGenerationResidence {
    limit: usize,
    images: BTreeMap<GenerationKey, Arc<[SemanticImageSnapshot]>>,
    order: VecDeque<GenerationKey>,
    owner_calls: u64,
    hits: u64,
}

impl Default for SemanticGenerationResidence {
    fn default() -> Self {
        Self::with_limit(MAX_RESIDENT_GENERATIONS)
    }
}

impl SemanticGenerationResidence {
    fn with_limit(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            images: BTreeMap::new(),
            order: VecDeque::new(),
            owner_calls: 0,
            hits: 0,
        }
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
        self.recall(GenerationKey::from_claim(claim), activate)
    }

    fn recall<E>(
        &mut self,
        key: GenerationKey,
        activate: impl FnOnce() -> Result<Box<[SemanticImageSnapshot]>, E>,
    ) -> Result<Arc<[SemanticImageSnapshot]>, E> {
        if let Some(hit) = self.images.get(&key) {
            let hit = Arc::clone(hit);
            self.touch(key);
            self.hits += 1;
            return Ok(hit);
        }
        self.owner_calls += 1;
        let images = Arc::from(activate()?);
        self.remember(key, Arc::clone(&images));
        Ok(images)
    }

    fn touch(&mut self, key: GenerationKey) {
        if let Some(position) = self.order.iter().position(|item| *item == key) {
            self.order.remove(position);
            self.order.push_back(key);
        }
    }

    fn remember(&mut self, key: GenerationKey, images: Arc<[SemanticImageSnapshot]>) {
        if self.images.contains_key(&key) {
            self.touch(key);
            return;
        }
        self.images.insert(key, images);
        self.order.push_back(key);
        while self.images.len() > self.limit {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.images.remove(&oldest);
        }
    }
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

    use super::{GenerationKey, SemanticGenerationResidence};
    use backend_library::interface::{SemanticImageAuthority, SemanticImageSnapshot};
    use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};
    use std::sync::Arc;

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
}
