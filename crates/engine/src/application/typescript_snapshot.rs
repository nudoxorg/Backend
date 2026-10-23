//! Same-owner reuse of an exact selected native TSZ output, with its original read fences.
//!
//! This retains a bounded proof, not checker state or a durable completeness claim.
//! A new owner, changed selection, or failed replay always requests compilation.

use super::{Arc, AtomicBool, LocalCompilerClient, Ordering, Path, PathBuf, StagedSemanticPackage};
use crate::application::typescript_host::TypeScriptProjectWitness;
use crate::application::typescript_program::TypeScriptForestClosureWitness;
use backend_semantic::vocabulary::LanguageProfile;
use std::collections::VecDeque;

const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;
const SNAPSHOT_CERTIFICATE_MAGIC: &[u8; 8] = b"TSZCOLD1";
const MAX_SNAPSHOT_CERTIFICATE_BYTES: usize =
    backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES;
const MAX_SNAPSHOT_PROGRAMS: usize = 256;

fn fits_retained_budget(host_bytes: usize, forest_bytes: usize) -> bool {
    host_bytes.saturating_add(forest_bytes) <= MAX_RETAINED_BYTES
}

struct SnapshotCertificate {
    root: PathBuf,
    profile: LanguageProfile,
    input: [u8; 32],
    binding: [u8; 32],
    toolchain: [u8; 32],
    // Runtime startup authority checked against the currently opened owner.
    local_authority: [u8; 32],
    // Project-specific authority sealed by the staged compiler and recomputed
    // from the host fingerprint plus ordered native forest environments.
    project_authority: [u8; 32],
    environment: [u8; 32],
    platform: [u8; 32],
    program_environments: Vec<[u8; 32]>,
    host: Vec<u8>,
    programs: Vec<Vec<u8>>,
}

pub(crate) fn encode_staged_snapshot_certificate(
    staged: &StagedSemanticPackage,
    root: &Path,
    input: [u8; 32],
) -> Option<Vec<u8>> {
    let (host, forest) = staged.typescript_snapshot_witness(root)?;
    let identity = staged.plane_execution_identity()?;
    if identity.profile() != staged.profile()
        || identity.input_witness().input_root() != &input
        || !matches!(staged.profile(), LanguageProfile::TypeScript(_))
    {
        return None;
    }
    let local_authority = staged.typescript_snapshot_runtime_local_authority()?;
    let environments = staged.typescript_snapshot_program_environments()?;
    if environments.is_empty() || environments.len() > MAX_SNAPSHOT_PROGRAMS {
        return None;
    }
    let program_environments = environments.to_vec();
    let project_authority = identity.local_authority_fingerprint();
    let root_len = root.to_str()?.len();
    let fixed_bytes = 8usize
        .checked_add(4)?
        .checked_add(root_len)?
        .checked_add(2 + 32 * 7)?
        .checked_add(4)?
        .checked_add(program_environments.len().checked_mul(32)?)?
        .checked_add(4 + 4)?;
    let minimum_program_framing = program_environments.len().checked_mul(4)?;
    let host_budget = MAX_SNAPSHOT_CERTIFICATE_BYTES
        .checked_sub(fixed_bytes)?
        .checked_sub(minimum_program_framing)?;
    let host_bytes = host.encode_native_snapshot_with_limit(host_budget)?;
    let program_budget = MAX_SNAPSHOT_CERTIFICATE_BYTES
        .checked_sub(fixed_bytes)?
        .checked_sub(host_bytes.len())?;
    let program_bytes = forest.encode_native_snapshot_with_limit(&host, program_budget)?;
    if program_bytes.len() != program_environments.len()
        || crate::application::typescript_program::native_program_execution_fingerprint(
            host.native_project_fingerprint()?,
            program_environments.iter().copied().map(|environment| {
                backend_frontend_typescript::TszEnvironmentFingerprint::from_sha256(environment)
            }),
        ) != project_authority
    {
        return None;
    }
    let certificate = SnapshotCertificate {
        root: root.to_path_buf(),
        profile: staged.profile(),
        input,
        binding: *staged.binding_facts().identity.as_ref(),
        toolchain: identity.toolchain_identity(),
        local_authority,
        project_authority,
        environment: identity.environment_identity(),
        platform: identity.target_platform_identity(),
        program_environments,
        host: host_bytes,
        programs: program_bytes,
    };
    encode_snapshot_certificate(&certificate)
}

pub(crate) fn restore_snapshot_certificate(
    client: &LocalCompilerClient,
    root: &Path,
    profile: LanguageProfile,
    input: [u8; 32],
    binding: [u8; 32],
    bytes: &[u8],
    cancelled: &AtomicBool,
) -> Option<TypeScriptSnapshotReuse> {
    if !client.is_ready() || cancelled.load(Ordering::Acquire) {
        return None;
    }
    let capability = client.capabilities().for_profile(profile);
    let current = (
        capability.toolchain_identity()?,
        capability.local_authority_fingerprint()?,
        capability.environment_identity()?,
        capability.target_platform_identity()?,
    );
    if capability.state() != crate::application::LocalCompilerCapabilityState::Ready
        || capability.manifest().is_none()
    {
        return None;
    }
    let certificate = decode_snapshot_certificate(bytes)?;
    if certificate.root != root
        || certificate.profile != profile
        || certificate.input != input
        || certificate.binding != binding
        || (
            certificate.toolchain,
            certificate.local_authority,
            certificate.environment,
            certificate.platform,
        ) != current
    {
        return None;
    }
    let host = Arc::new(TypeScriptProjectWitness::decode_native_snapshot(
        &certificate.host,
    )?);
    if !host.is_native_for_root(root)
        || certificate.program_environments.len() != certificate.programs.len()
        || crate::application::typescript_program::native_program_execution_fingerprint(
            host.native_project_fingerprint()?,
            certificate
                .program_environments
                .iter()
                .copied()
                .map(|environment| {
                    backend_frontend_typescript::TszEnvironmentFingerprint::from_sha256(environment)
                }),
        ) != certificate.project_authority
    {
        return None;
    }
    let mut active = || {
        if !cancelled.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerProgramCancelled)
        }
    };
    let forest = Arc::new(TypeScriptForestClosureWitness::decode_native_snapshot(
        certificate.programs,
        &host,
        &mut active,
    )?);
    if cancelled.load(Ordering::Acquire) || !client.is_ready() {
        return None;
    }
    let retained_bytes = host
        .retained_native_bytes()?
        .saturating_add(forest.retained_bytes());
    if retained_bytes > MAX_RETAINED_BYTES {
        return None;
    }
    let entry = Arc::new(Entry {
        root: root.to_path_buf(),
        profile,
        input,
        binding,
        alive: Arc::clone(&client.shared.alive),
        host,
        forest,
        retained_bytes,
    });
    Some(TypeScriptSnapshotReuse { entry })
}

fn encode_snapshot_certificate(certificate: &SnapshotCertificate) -> Option<Vec<u8>> {
    let root = certificate.root.to_str()?.as_bytes();
    if root.len() > u32::MAX as usize
        || certificate.host.len() > u32::MAX as usize
        || certificate.programs.is_empty()
        || certificate.programs.len() > MAX_SNAPSHOT_PROGRAMS
        || certificate.program_environments.len() > MAX_SNAPSHOT_PROGRAMS
        || certificate.programs.len() != certificate.program_environments.len()
    {
        return None;
    }
    let total = 8usize
        .checked_add(4)?
        .checked_add(root.len())?
        .checked_add(2 + 32 * 7 + 4 + certificate.program_environments.len().checked_mul(32)?)?
        .checked_add(4)?
        .checked_add(certificate.host.len())?
        .checked_add(4)?
        .checked_add(certificate.programs.iter().try_fold(0usize, |n, program| {
            n.checked_add(4)?.checked_add(program.len())
        })?)?;
    if total > MAX_SNAPSHOT_CERTIFICATE_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(SNAPSHOT_CERTIFICATE_MAGIC);
    out.extend_from_slice(&(root.len() as u32).to_le_bytes());
    out.extend_from_slice(root);
    out.extend_from_slice(&<[u8; 2]>::from(certificate.profile));
    for digest in [
        certificate.input,
        certificate.binding,
        certificate.toolchain,
        certificate.local_authority,
        certificate.project_authority,
        certificate.environment,
        certificate.platform,
    ] {
        out.extend_from_slice(&digest);
    }
    out.extend_from_slice(&(certificate.program_environments.len() as u32).to_le_bytes());
    for environment in &certificate.program_environments {
        out.extend_from_slice(environment);
    }
    out.extend_from_slice(&(certificate.host.len() as u32).to_le_bytes());
    out.extend_from_slice(&certificate.host);
    out.extend_from_slice(&(certificate.programs.len() as u32).to_le_bytes());
    for program in &certificate.programs {
        out.extend_from_slice(&u32::try_from(program.len()).ok()?.to_le_bytes());
        out.extend_from_slice(program);
    }
    Some(out)
}

fn decode_snapshot_certificate(bytes: &[u8]) -> Option<SnapshotCertificate> {
    if bytes.len() > MAX_SNAPSHOT_CERTIFICATE_BYTES
        || bytes.len() < 8 + 4 + 2 + 32 * 7 + 4 + 4 + 4
        || bytes.get(..8)? != SNAPSHOT_CERTIFICATE_MAGIC
    {
        return None;
    }
    let mut cursor = 8usize;
    let mut take = |len: usize| -> Option<&[u8]> {
        let end = cursor.checked_add(len)?;
        let chunk = bytes.get(cursor..end)?;
        cursor = end;
        Some(chunk)
    };
    let root_len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    let root = PathBuf::from(std::str::from_utf8(take(root_len)?).ok()?);
    if !root.is_absolute() || root.to_str().is_none() {
        return None;
    }
    let profile_bytes: [u8; 2] = take(2)?.try_into().ok()?;
    let profile = LanguageProfile::try_from(profile_bytes).ok()?;
    if !matches!(profile, LanguageProfile::TypeScript(_)) {
        return None;
    }
    let mut digest = || -> Option<[u8; 32]> { take(32)?.try_into().ok() };
    let input = digest()?;
    let binding = digest()?;
    let toolchain = digest()?;
    let local_authority = digest()?;
    let project_authority = digest()?;
    let environment = digest()?;
    let platform = digest()?;
    let environment_count = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    if environment_count == 0 || environment_count > MAX_SNAPSHOT_PROGRAMS {
        return None;
    }
    let mut program_environments = Vec::with_capacity(environment_count);
    for _ in 0..environment_count {
        program_environments.push(take(32)?.try_into().ok()?);
    }
    let host_len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    let host = take(host_len)?.to_vec();
    let program_count = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    if program_count == 0
        || program_count > MAX_SNAPSHOT_PROGRAMS
        || program_count != environment_count
    {
        return None;
    }
    let mut programs = Vec::with_capacity(program_count);
    for _ in 0..program_count {
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        programs.push(take(len)?.to_vec());
    }
    if cursor != bytes.len() {
        return None;
    }
    Some(SnapshotCertificate {
        root,
        profile,
        input,
        binding,
        toolchain,
        local_authority,
        project_authority,
        environment,
        platform,
        program_environments,
        host,
        programs,
    })
}

#[derive(Default)]
pub(super) struct TypeScriptSnapshotCache {
    // One entry per root/profile, bounded across all retained producer ledgers.
    // Eviction can only lose the opportunity to reuse.
    entries: VecDeque<TypeScriptSnapshotReuse>,
}

/// Opaque compiler-produced fence for one exact selected native TypeScript output.
/// Clones retain the same proof through source preparation and final product admission.
#[derive(Clone)]
pub struct TypeScriptSnapshotReuse {
    entry: Arc<Entry>,
}

struct Entry {
    root: PathBuf,
    profile: LanguageProfile,
    input: [u8; 32],
    binding: [u8; 32],
    alive: Arc<AtomicBool>,
    host: Arc<TypeScriptProjectWitness>,
    forest: Arc<TypeScriptForestClosureWitness>,
    retained_bytes: usize,
}

impl TypeScriptSnapshotReuse {
    /// Replays the compiler's original complete native filesystem observation ledger.
    /// No stale or partial ledger is promoted to current authority by a digest match.
    #[must_use]
    pub fn is_current(&self, cancelled: Option<&AtomicBool>) -> bool {
        let active = || {
            self.entry.alive.load(Ordering::Acquire)
                && !cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
        };
        let mut control = || {
            if active() {
                Ok(())
            } else {
                Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerProgramCancelled)
            }
        };
        active()
            && self
                .entry
                .forest
                .validate_current_controlled(&self.entry.host, &mut control)
                .is_ok()
            && active()
    }
}

impl LocalCompilerClient {
    /// Returns a replayed fence only for the exact output currently selected by the caller.
    /// The result must also be checked at final product selection.
    pub fn typescript_snapshot_reuse(
        &self,
        root: &Path,
        profile: LanguageProfile,
        input: [u8; 32],
        binding: [u8; 32],
        cancelled: &AtomicBool,
    ) -> Option<TypeScriptSnapshotReuse> {
        let proof = self
            .shared
            .typescript_snapshot
            .lock()
            .ok()?
            .entries
            .iter()
            .find(|proof| proof.entry.root == root && proof.entry.profile == profile)
            .cloned()?;
        let entry = &proof.entry;
        if entry.root != root
            || entry.profile != profile
            || entry.input != input
            || entry.binding != binding
            || !Arc::ptr_eq(&entry.alive, &self.shared.alive)
        {
            return None;
        }
        proof.is_current(Some(cancelled)).then_some(proof)
    }

    /// Reopens a certificate supplied only by the authenticated selected-generation
    /// metadata path. Replayed proof is retained under the same bounded owner cache as
    /// a freshly compiled witness.
    pub fn restore_typescript_snapshot_certificate(
        &self,
        root: &Path,
        profile: LanguageProfile,
        input: [u8; 32],
        binding: [u8; 32],
        certificate: &backend_extension_turso::VerifiedTypeScriptSnapshotWitness,
        cancelled: &AtomicBool,
    ) -> Option<TypeScriptSnapshotReuse> {
        let proof = restore_snapshot_certificate(
            self,
            root,
            profile,
            input,
            binding,
            certificate.as_bytes(),
            cancelled,
        )?;
        let mut cache = self.shared.typescript_snapshot.lock().ok()?;
        cache
            .entries
            .retain(|existing| existing.entry.root != root || existing.entry.profile != profile);
        let mut total = cache.entries.iter().fold(0usize, |sum, existing| {
            sum.saturating_add(existing.entry.retained_bytes)
        });
        while !fits_retained_budget(total, proof.entry.retained_bytes) {
            let evicted = cache.entries.pop_front()?;
            total = total.saturating_sub(evicted.entry.retained_bytes);
        }
        cache.entries.push_back(proof.clone());
        Some(proof)
    }

    /// Remembers only an actual native compiler read closure for its exact selected output.
    /// The binding comparison prevents associating a producer fence with another output.
    pub fn remember_typescript_snapshot(
        &self,
        root: &Path,
        input: [u8; 32],
        selected_binding: [u8; 32],
        staged: &StagedSemanticPackage,
    ) {
        if !self.is_ready() || selected_binding != *staged.binding_facts().identity.as_ref() {
            return;
        }
        // This exact output has superseded the previous selection. Retire a
        // predecessor even when the new result is ineligible for reuse, so a
        // transient outcome cannot recover an older eligibility decision.
        let Ok(mut cache) = self.shared.typescript_snapshot.lock() else {
            return;
        };
        cache
            .entries
            .retain(|proof| proof.entry.root != root || proof.entry.profile != staged.profile());
        let Some((host, forest)) = staged.typescript_snapshot_witness(root) else {
            return;
        };
        // Ledger accounting includes captured source bytes and retained path/directory
        // metadata. The native host adds its separately retained raw config candidates.
        let Some(host_bytes) = host.retained_native_bytes() else {
            return;
        };
        // Include the per-entry root and conservative cache/Arc allocation
        // overhead in addition to both compiler-owned ledgers.
        let host_bytes = host_bytes
            .saturating_add(root.as_os_str().as_encoded_bytes().len())
            .saturating_add(1024);
        if !fits_retained_budget(host_bytes, forest.retained_bytes()) {
            return;
        }
        {
            let retained_bytes = host_bytes + forest.retained_bytes();
            let mut total = cache.entries.iter().fold(0usize, |total, proof| {
                total.saturating_add(proof.entry.retained_bytes)
            });
            while !fits_retained_budget(total, retained_bytes) {
                let Some(evicted) = cache.entries.pop_front() else {
                    return;
                };
                total = total.saturating_sub(evicted.entry.retained_bytes);
            }
            cache.entries.push_back(TypeScriptSnapshotReuse {
                entry: Arc::new(Entry {
                    root: root.to_path_buf(),
                    profile: staged.profile(),
                    input,
                    binding: selected_binding,
                    alive: Arc::clone(&self.shared.alive),
                    host,
                    forest,
                    retained_bytes,
                }),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ToolchainProbeLimits;
    use crate::application::typescript_host::TypeScriptProjectHost;
    use crate::application::{
        LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch,
        LocalCompilerTimeout, LocalPackageRootSet, LocalToolchainSet,
        PackageAuthorityConfiguration, PackageSource, PackageSourceSet,
    };
    use backend_library::interface::{
        CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
    };
    use backend_semantic::vocabulary::{Stage, TypeScriptSource};
    use backend_store::journal::PublicationLimits;
    use std::{
        fs,
        num::NonZeroUsize,
        sync::Mutex,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    const MAIN: &str = "import { value } from 'dep'; import type { Missing } from './missing'; import type { Leaf } from './not-yet/leaf'; export const result: number = value;";

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("tsz-snapshot-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            let fixture = Self(root.canonicalize().unwrap());
            fixture.write("tsconfig.json", r#"{"extends":"./base.json","compilerOptions":{"noLib":true,"types":[],"moduleResolution":"Bundler","module":"ESNext"},"include":["src/**/*"]}"#);
            fixture.write("base.json", r#"{"compilerOptions":{"strict":true}}"#);
            fixture.write("src/main.ts", MAIN);
            fixture.write(
                "node_modules/dep/package.json",
                r#"{"name":"dep","exports":{".":{"types":"./index.d.ts"}}}"#,
            );
            fixture.write("node_modules/dep/index.d.ts", "export const value: number;");
            fixture
        }
        fn write(&self, path: &str, contents: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fn compile(&self) -> StagedSemanticPackage {
            self.compile_sources(false)
        }
        fn compile_sources(&self, excluded_source: bool) -> StagedSemanticPackage {
            self.compile_profile(profile(), excluded_source)
        }
        fn compile_profile(
            &self,
            selected_profile: LanguageProfile,
            excluded_source: bool,
        ) -> StagedSemanticPackage {
            let host = TypeScriptProjectHost::direct(
                None,
                ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                    .unwrap(),
            );
            let cancelled = AtomicBool::new(false);
            let artifacts = self.0.join("artifacts");
            let journal = self.0.join("journal");
            let work = self.0.join("native-work");
            fs::create_dir_all(&work).unwrap();
            let mut scratch = LocalCompilerScratch::with_fragment_capacity(
                NonZeroUsize::new(16 * 1024 * 1024).unwrap(),
            )
            .unwrap();
            let mut compiler = LocalCompiler::create_with_package_authority(
                LocalCompilerConfig {
                    toolchains: LocalToolchainSet::validate(&[]).unwrap(),
                    artifact_directory: &artifacts,
                    journal_directory: &journal,
                    native_work_directory: &work,
                    control: LocalCompilerControl {
                        timeout: LocalCompilerTimeout::new(Duration::from_secs(30)).unwrap(),
                        cancelled: &cancelled,
                    },
                },
                LocalPackageRootSet::EMPTY,
                PackageAuthorityConfiguration {
                    typescript_project_host: Some(&host),
                    ..PackageAuthorityConfiguration::UNAVAILABLE
                },
                PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN).unwrap(),
                &mut scratch,
            )
            .unwrap();
            let request = PackageCompileRequest::new(
                GenerateTarget {
                    correlation: CorrelationId(1),
                    profile: selected_profile,
                    stage: Stage::LowerIr,
                },
                PackageUrl::try_from("pkg:npm/snapshot-fixture@1.0.0".to_owned()).unwrap(),
            )
            .unwrap();
            let mut sources = if selected_profile == tsx_profile() {
                vec![PackageSource::new("src/view.tsx", "export const view = 1;").unwrap()]
            } else {
                vec![PackageSource::new("src/main.ts", MAIN).unwrap()]
            };
            if excluded_source {
                const EXCLUDED: &str = "export const excluded = 1;";
                self.write("excluded.ts", EXCLUDED);
                sources.push(PackageSource::new("excluded.ts", EXCLUDED).unwrap());
            }
            sources.sort_unstable_by(|left, right| left.relative_path().cmp(right.relative_path()));
            compiler
                .compile_package_sources_staged(
                    PackageSourceSet::new(&request, &self.0, &sources).unwrap(),
                    &mut |_| {},
                )
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn profile() -> LanguageProfile {
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript)
    }
    fn tsx_profile() -> LanguageProfile {
        LanguageProfile::TypeScript(TypeScriptSource::Tsx)
    }
    fn client() -> LocalCompilerClient {
        LocalCompilerClient {
            shared: Arc::new(super::super::request_lease_tests::shared()),
            client_id: 1,
            last_image: Mutex::new(None),
        }
    }

    #[test]
    fn retained_budget_rejects_the_full_charge_and_saturates_on_overflow() {
        assert!(fits_retained_budget(MAX_RETAINED_BYTES - 1, 1));
        assert!(!fits_retained_budget(MAX_RETAINED_BYTES - 1, 2));
        assert!(!fits_retained_budget(usize::MAX, 1));
    }

    #[test]
    fn native_ts_and_tsx_retain_independent_selected_producer_fences() {
        let fixture = Fixture::new();
        fixture.write("src/view.tsx", "export const view = 1;");
        let typescript = fixture.compile();
        let tsx = fixture.compile_profile(tsx_profile(), false);
        let ts_binding = *typescript.binding_facts().identity.as_ref();
        let tsx_binding = *tsx.binding_facts().identity.as_ref();
        let owner = client();
        let cancelled = AtomicBool::new(false);
        owner.remember_typescript_snapshot(&fixture.0, [1; 32], ts_binding, &typescript);
        // A present TS generation cannot supply a missing TSX producer fence,
        // even if a caller supplies the TS input and binding for that profile.
        assert!(
            owner
                .typescript_snapshot_reuse(
                    &fixture.0,
                    tsx_profile(),
                    [1; 32],
                    ts_binding,
                    &cancelled,
                )
                .is_none()
        );
        owner.remember_typescript_snapshot(&fixture.0, [2; 32], tsx_binding, &tsx);
        let ts_proof = owner
            .typescript_snapshot_reuse(&fixture.0, profile(), [1; 32], ts_binding, &cancelled)
            .expect("TS profile must survive a TSX publication");
        let tsx_proof = owner
            .typescript_snapshot_reuse(&fixture.0, tsx_profile(), [2; 32], tsx_binding, &cancelled)
            .expect("TSX has its own producer proof");
        assert!(
            owner
                .typescript_snapshot_reuse(&fixture.0, profile(), [3; 32], ts_binding, &cancelled,)
                .is_none(),
            "changed TS input requests compilation"
        );
        assert!(
            owner
                .typescript_snapshot_reuse(
                    &fixture.0,
                    tsx_profile(),
                    [1; 32],
                    ts_binding,
                    &cancelled,
                )
                .is_none(),
            "TSX must never borrow the TS binding"
        );
        assert!(
            tsx_proof.is_current(None),
            "unchanged TSX remains independently reusable"
        );
        fixture.write("src/missing.ts", "export interface Missing {}");
        assert!(
            !ts_proof.is_current(None),
            "a negative lookup mutation rejects the retained final fence"
        );
        assert!(
            owner
                .typescript_snapshot_reuse(&fixture.0, profile(), [1; 32], ts_binding, &cancelled,)
                .is_none(),
            "matching scan identity cannot override the changed read closure"
        );
        fixture.write("src/view.tsx", "export const view = 'changed';");
        assert!(
            !tsx_proof.is_current(None),
            "the TSX final fence also rejects its own source mutation"
        );
    }

    #[test]
    fn native_profile_cache_budget_is_aggregate_and_replacement_does_not_evict_siblings() {
        let fixture = Fixture::new();
        fixture.write("src/view.tsx", "export const view = 1;");
        let typescript = fixture.compile();
        let tsx = fixture.compile_profile(tsx_profile(), false);
        let ts_binding = *typescript.binding_facts().identity.as_ref();
        let tsx_binding = *tsx.binding_facts().identity.as_ref();
        let owner = client();
        owner.remember_typescript_snapshot(&fixture.0, [1; 32], ts_binding, &typescript);
        owner.remember_typescript_snapshot(&fixture.0, [2; 32], tsx_binding, &tsx);
        owner.remember_typescript_snapshot(&fixture.0, [3; 32], ts_binding, &typescript);
        {
            let mut cache = owner.shared.typescript_snapshot.lock().unwrap();
            assert_eq!(
                cache.entries.len(),
                2,
                "replacement retains only one proof per profile"
            );
            assert!(
                cache
                    .entries
                    .iter()
                    .map(|proof| proof.entry.retained_bytes)
                    .sum::<usize>()
                    <= MAX_RETAINED_BYTES
            );
            // Charge the oldest entry the full budget; a new entry must evict
            // that ledger rather than grant every profile a separate 64MiB.
            Arc::get_mut(&mut cache.entries.front_mut().unwrap().entry)
                .unwrap()
                .retained_bytes = MAX_RETAINED_BYTES;
        }
        owner.remember_typescript_snapshot(&fixture.0, [4; 32], ts_binding, &typescript);
        let cache = owner.shared.typescript_snapshot.lock().unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].entry.profile, profile());
        assert!(cache.entries[0].entry.retained_bytes <= MAX_RETAINED_BYTES);
    }

    #[test]
    fn native_snapshot_preserves_configured_source_exclusions() {
        let fixture = Fixture::new();
        let staged = fixture.compile_sources(true);
        assert_eq!(staged.coverage_gaps().len(), 1);
        assert_eq!(staged.coverage_gaps()[0].cause(),
            crate::application::PackageSourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram);
        let owner = client();
        let binding = *staged.binding_facts().identity.as_ref();
        owner.remember_typescript_snapshot(&fixture.0, [1; 32], binding, &staged);
        assert!(
            owner
                .typescript_snapshot_reuse(
                    &fixture.0,
                    profile(),
                    [1; 32],
                    binding,
                    &AtomicBool::new(false),
                )
                .is_some()
        );
    }

    #[test]
    fn native_snapshot_requires_same_owner_input_and_exact_selected_output() {
        let fixture = Fixture::new();
        let staged = fixture.compile();
        let binding = *staged.binding_facts().identity.as_ref();
        let owner = client();
        let cancelled = AtomicBool::new(false);
        let input = [0x61; 32];
        owner.remember_typescript_snapshot(&fixture.0, input, [0x62; 32], &staged);
        assert!(
            owner
                .typescript_snapshot_reuse(&fixture.0, profile(), input, binding, &cancelled)
                .is_none()
        );
        owner.remember_typescript_snapshot(&fixture.0, input, binding, &staged);
        let proof = owner
            .typescript_snapshot_reuse(&fixture.0, profile(), input, binding, &cancelled)
            .unwrap();
        assert!(proof.is_current(None));
        assert!(
            owner
                .typescript_snapshot_reuse(&fixture.0, profile(), [0x63; 32], binding, &cancelled)
                .is_none()
        );
        assert!(
            owner
                .typescript_snapshot_reuse(&fixture.0, profile(), input, [0x64; 32], &cancelled)
                .is_none()
        );
        let another_owner = client();
        another_owner
            .shared
            .typescript_snapshot
            .lock()
            .unwrap()
            .entries
            .push_back(proof.clone());
        assert!(
            another_owner
                .typescript_snapshot_reuse(&fixture.0, profile(), input, binding, &cancelled)
                .is_none(),
            "even a copied ledger cannot transfer producer authority to another toolchain session"
        );
        cancelled.store(true, Ordering::Release);
        assert!(!proof.is_current(Some(&cancelled)));
        owner.shared.alive.store(false, Ordering::Release);
        assert!(!proof.is_current(None));
    }

    #[test]
    fn native_snapshot_certificate_cold_decode_replays_complete_ledger_and_rejects_probes() {
        let fixture = Fixture::new();
        let staged = fixture.compile();
        let input = *staged.input_witness().input_root();
        let binding = *staged.binding_facts().identity.as_ref();
        let (host_witness, forest_witness) = staged
            .typescript_snapshot_witness(&fixture.0)
            .expect("native fixture retains complete witnesses");
        let program_environments = staged
            .typescript_snapshot_program_environments()
            .expect("staged forest retains ordered environments")
            .to_vec();
        assert!(host_witness.encode_native_snapshot_with_limit(1).is_none());
        assert!(
            forest_witness
                .encode_native_snapshot_with_limit(&host_witness, 1)
                .is_none()
        );
        let project_authority =
            crate::application::typescript_program::native_program_execution_fingerprint(
                host_witness
                    .native_project_fingerprint()
                    .expect("native host identity matches admitted project inputs"),
                program_environments.iter().copied().map(|environment| {
                    backend_frontend_typescript::TszEnvironmentFingerprint::from_sha256(environment)
                }),
            );
        let bytes = encode_snapshot_certificate(&SnapshotCertificate {
            root: fixture.0.clone(),
            profile: profile(),
            input,
            binding,
            toolchain: [1; 32],
            local_authority: [2; 32],
            project_authority,
            environment: [3; 32],
            platform: [4; 32],
            program_environments,
            host: host_witness
                .encode_native_snapshot()
                .expect("complete native host encodes"),
            programs: forest_witness
                .encode_native_snapshot(&host_witness)
                .expect("complete native program forest encodes"),
        })
        .expect("complete bounded certificate encodes");
        fixture.write("src/unrelated.txt", "");
        let parsed = decode_snapshot_certificate(&bytes).expect("strict certificate decode");
        assert_eq!(parsed.root, fixture.0);
        assert_eq!(parsed.profile, profile());
        assert_eq!(parsed.input, input);
        assert_eq!(parsed.binding, binding);
        let host = Arc::new(
            TypeScriptProjectWitness::decode_native_snapshot(&parsed.host)
                .expect("complete native host rehydrates"),
        );
        let mut active = || Ok(());
        let forest = TypeScriptForestClosureWitness::decode_native_snapshot(
            parsed.programs,
            &host,
            &mut active,
        )
        .expect("all resolver observations replay before witness admission");
        assert!(forest.validate_current(&host).is_ok());

        fixture.write("src/new.ts", "export const added = 1;");
        let parsed = decode_snapshot_certificate(&bytes).expect("certificate stays well formed");
        let host = Arc::new(
            TypeScriptProjectWitness::decode_native_snapshot(&parsed.host)
                .expect("host structure remains decodable"),
        );
        let mut active = || Ok(());
        assert!(
            TypeScriptForestClosureWitness::decode_native_snapshot(
                parsed.programs,
                &host,
                &mut active,
            )
            .is_none(),
            "cold replay rejects a new source selected by the configured glob"
        );
        fs::remove_file(fixture.0.join("src/new.ts")).unwrap();

        fixture.write("base.json", r#"{"compilerOptions":{"strict":false}}"#);
        let parsed = decode_snapshot_certificate(&bytes).expect("certificate stays well formed");
        let host = Arc::new(
            TypeScriptProjectWitness::decode_native_snapshot(&parsed.host)
                .expect("host structure remains decodable"),
        );
        let mut active = || Ok(());
        assert!(
            TypeScriptForestClosureWitness::decode_native_snapshot(
                parsed.programs,
                &host,
                &mut active,
            )
            .is_none(),
            "cold replay rejects a changed config observation"
        );

        for corrupt in [&bytes[..8], &bytes[..bytes.len() - 1]] {
            assert!(decode_snapshot_certificate(corrupt).is_none());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode_snapshot_certificate(&trailing).is_none());
    }

    #[test]
    fn native_runtime_staged_exporter_emits_replayable_certificate_and_misses_changed_inputs() {
        use crate::application::{
            LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths, LocalCompilerScratch,
            LocalCompilerTimeout, LocalRuntimePackageAuthority, LocalRuntimeToolchain,
            OwnedPackageSource, OwnedPackageSourceSet,
        };
        use backend_library::interface::{
            CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
        };
        use backend_semantic::vocabulary::Stage;
        use backend_store::journal::PublicationLimits;
        use std::num::NonZeroUsize;

        fn owner(runtime_root: &Path) -> LocalCompilerClient {
            let host = TypeScriptProjectHost::direct(
                None,
                ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                    .unwrap(),
            );
            let configuration = LocalCompilerRuntimeConfiguration::new(
                LocalCompilerRuntimePaths::new(
                    runtime_root.join("artifacts"),
                    runtime_root.join("journal"),
                    runtime_root.join("native-work"),
                )
                .unwrap(),
                vec![LocalRuntimeToolchain::compiled_native_typescript()].into_boxed_slice(),
                Box::new([]),
                LocalRuntimePackageAuthority::default(),
                LocalCompilerTimeout::new(Duration::from_secs(60)).unwrap(),
                PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN).unwrap(),
                LocalCompilerScratch::with_fragment_capacity(
                    NonZeroUsize::new(16 * 1024 * 1024).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
            .with_typescript_project_host(host);
            LocalCompilerClient::start(configuration).expect("Ready linked TSZ owner")
        }

        fn package(root: &Path) -> OwnedPackageSourceSet {
            let profile = profile();
            let request = PackageCompileRequest::new(
                GenerateTarget {
                    correlation: CorrelationId(0x71),
                    profile,
                    stage: Stage::LowerIr,
                },
                PackageUrl::try_from("pkg:npm/staged-snapshot-fixture@1.0.0".to_owned()).unwrap(),
            )
            .unwrap();
            OwnedPackageSourceSet::new(
                request,
                root.to_path_buf(),
                vec![OwnedPackageSource::new("src/main.ts", MAIN).unwrap()].into_boxed_slice(),
            )
            .unwrap()
        }

        let fixture = Fixture::new();
        const CONFIG: &str = r#"{"extends":"./base.json","compilerOptions":{"noLib":true,"types":[],"moduleResolution":"Bundler","module":"ESNext"},"files":["src/main.ts"]}"#;
        fixture.write("tsconfig.json", CONFIG);
        let runtime_root = fixture.0.join(".runtime-data");
        fs::create_dir(&runtime_root).expect("create isolated owner storage");
        let producer = owner(&runtime_root);
        assert_eq!(
            producer.capabilities().for_profile(profile()).state(),
            crate::application::LocalCompilerCapabilityState::Ready,
            "the exporter is exercised only under an actually admitted linked-native capability"
        );
        let staged = producer
            .compile_package_sources_staged(package(&fixture.0))
            .expect("native TSZ staged compilation");
        let input = *staged.input_witness().input_root();
        let binding = *staged.binding_facts().identity.as_ref();
        let program = staged
            .native_program_sources()
            .expect("compiler-issued program source forest");
        let (_, nodes) = program.nodes().expect("configured program forest");
        let project_authority = staged
            .plane_execution_identity()
            .expect("native staged execution identity")
            .local_authority_fingerprint();
        let certificate_bytes = staged
            .typescript_snapshot_certificate(input)
            .expect("production staged exporter accepts exact native identity");
        let mut changed_input = input;
        changed_input[0] ^= 1;
        assert!(
            staged
                .typescript_snapshot_certificate(changed_input)
                .is_none(),
            "the production exporter refuses a substituted input root"
        );
        let certificate = decode_snapshot_certificate(&certificate_bytes)
            .expect("strictly decode exported certificate");
        let capability = producer.capabilities().for_profile(profile());
        assert_eq!(certificate.root, fixture.0);
        assert_eq!(certificate.profile, profile());
        assert_eq!(certificate.input, input);
        assert_eq!(certificate.binding, binding);
        assert_eq!(certificate.project_authority, project_authority);
        assert_eq!(
            certificate.toolchain,
            capability.toolchain_identity().unwrap()
        );
        assert_eq!(
            certificate.local_authority,
            capability.local_authority_fingerprint().unwrap()
        );
        assert_eq!(
            certificate.environment,
            capability.environment_identity().unwrap()
        );
        assert_eq!(
            certificate.platform,
            capability.target_platform_identity().unwrap()
        );
        assert_eq!(
            certificate.program_environments,
            nodes
                .iter()
                .map(|node| node.environment)
                .collect::<Vec<_>>(),
            "exporter preserves the exact configured forest environment order"
        );
        assert!(!certificate.programs.is_empty());
        drop(staged);
        drop(producer);

        let fresh = owner(&runtime_root);
        let cancelled = AtomicBool::new(false);
        let restored = restore_snapshot_certificate(
            &fresh,
            &fixture.0,
            profile(),
            input,
            binding,
            &certificate_bytes,
            &cancelled,
        )
        .expect("fresh Ready owner replays the production export");
        assert!(restored.is_current(None));

        fixture.write(
            "tsconfig.json",
            r#"{"extends":"./base.json","compilerOptions":{"noLib":true,"types":[],"moduleResolution":"Bundler","module":"ESNext","strict":true},"files":["src/main.ts"]}"#,
        );
        assert!(
            restore_snapshot_certificate(
                &fresh,
                &fixture.0,
                profile(),
                input,
                binding,
                &certificate_bytes,
                &cancelled,
            )
            .is_none(),
            "changed project configuration rejects the exported ledger"
        );
        fixture.write("tsconfig.json", CONFIG);
        fixture.write("node_modules/dep/index.d.ts", "export const value: string;");
        assert!(
            restore_snapshot_certificate(
                &fresh,
                &fixture.0,
                profile(),
                input,
                binding,
                &certificate_bytes,
                &cancelled,
            )
            .is_none(),
            "changed dependency bytes reject the exported ledger"
        );
        fixture.write("node_modules/dep/index.d.ts", "export const value: number;");
        fixture.write("src/missing.ts", "export interface Missing {}");
        assert!(
            restore_snapshot_certificate(
                &fresh,
                &fixture.0,
                profile(),
                input,
                binding,
                &certificate_bytes,
                &cancelled,
            )
            .is_none(),
            "a formerly negative resolver lookup becoming present rejects reuse"
        );
        drop(fresh);
    }

    #[test]
    fn native_snapshot_replays_dependencies_config_globs_and_negative_reads_at_final_admission() {
        for mutation in [
            "source",
            "removed-source",
            "created-source",
            "negative",
            "negative-parent",
            "type-root",
            "config",
            "dependency",
            "exports",
        ] {
            let fixture = Fixture::new();
            if mutation == "type-root" {
                fixture.write(
                    "tsconfig.json",
                    r#"{"extends":"./base.json","compilerOptions":{"noLib":true,"moduleResolution":"Bundler","module":"ESNext","typeRoots":["./type-roots"]},"include":["src/**/*"]}"#,
                );
                fixture.write(
                    "type-roots/baseline/index.d.ts",
                    "interface BaselineAmbientType {}",
                );
            }
            let staged = fixture.compile();
            let binding = *staged.binding_facts().identity.as_ref();
            let owner = client();
            owner.remember_typescript_snapshot(&fixture.0, [1; 32], binding, &staged);
            let proof = owner
                .typescript_snapshot_reuse(
                    &fixture.0,
                    profile(),
                    [1; 32],
                    binding,
                    &AtomicBool::new(false),
                )
                .expect("actual producer fence is current before mutation");
            match mutation {
                "source" => fixture.write("src/main.ts", "export const changed = 2;"),
                "removed-source" => fs::remove_file(fixture.0.join("src/main.ts")).unwrap(),
                "created-source" => fixture.write("src/new.ts", "export const newFile = 1;"),
                "negative" => fixture.write("src/missing.ts", "export interface Missing {}"),
                "negative-parent" => {
                    fixture.write("src/not-yet/leaf.ts", "export interface Leaf {}")
                }
                "type-root" => fixture.write(
                    "type-roots/added/index.d.ts",
                    "interface AddedAmbientType {}",
                ),
                "config" => fixture.write("base.json", r#"{"compilerOptions":{"strict":false}}"#),
                "dependency" => {
                    fixture.write("node_modules/dep/index.d.ts", "export const value: string;")
                }
                "exports" => fixture.write(
                    "node_modules/dep/package.json",
                    r#"{"name":"dep","exports":{".":{"types":"./other.d.ts"}}}"#,
                ),
                _ => unreachable!(),
            }
            assert!(
                !proof.is_current(None),
                "final admission must reject {mutation}"
            );
        }
    }

    #[test]
    fn native_snapshot_replay_cancellation_interrupts_observation_steps_without_admitting_partial_proof()
     {
        let fixture = Fixture::new();
        let staged = fixture.compile();
        let (host, forest) = staged.typescript_snapshot_witness(&fixture.0).unwrap();
        let owner = client();
        let binding = *staged.binding_facts().identity.as_ref();
        owner.remember_typescript_snapshot(&fixture.0, [1; 32], binding, &staged);
        let proof = owner
            .typescript_snapshot_reuse(
                &fixture.0,
                profile(),
                [1; 32],
                binding,
                &AtomicBool::new(false),
            )
            .expect("the real native read closure enters the owning cache");
        let mut complete_steps = 0usize;
        forest
            .validate_current_controlled(&host, &mut || {
                complete_steps += 1;
                Ok(())
            })
            .expect("actual retained native read ledger is initially current");
        assert!(
            complete_steps > 4,
            "fixture exercises multiple actual filesystem replay steps"
        );
        for stop_at in [2, complete_steps / 2, complete_steps - 1] {
            let cancelled = AtomicBool::new(false);
            let mut steps = 0usize;
            let result = forest.validate_current_controlled(&host, &mut || {
                steps += 1;
                if steps == stop_at { cancelled.store(true, Ordering::Release); }
                if cancelled.load(Ordering::Acquire) {
                    Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerProgramCancelled)
                } else { Ok(()) }
            });
            assert!(matches!(result, Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerProgramCancelled)));
            assert_eq!(
                steps, stop_at,
                "no later observation runs after cancellation"
            );
            assert!(
                !proof.is_current(Some(&cancelled)),
                "public replay observes the same actual cancellation flag"
            );
            assert!(
                proof.is_current(None),
                "cancellation does not mutate the owning cache's retained witness"
            );
            forest
                .validate_current(&host)
                .expect("cancelled replay does not mutate retained proof");
        }
        owner.shared.alive.store(false, Ordering::Release);
        assert!(
            !proof.is_current(None),
            "a retired actual owner cannot reuse its cached producer fence"
        );
        fixture.write("node_modules/dep/index.d.ts", "export const value: string;");
        assert!(
            forest
                .validate_current_controlled(&host, &mut || Ok(()))
                .is_err(),
            "healthy controlled replay still refuses a changed actual dependency"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_snapshot_rejects_retargeted_dependency_realpath() {
        let fixture = Fixture::new();
        fixture.write("deps/one.d.ts", "export const value: number;");
        fixture.write("deps/two.d.ts", "export const value: number;");
        let link = fixture.0.join("node_modules/dep/index.d.ts");
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("deps/one.d.ts"), &link).unwrap();
        let staged = fixture.compile();
        let owner = client();
        let binding = *staged.binding_facts().identity.as_ref();
        owner.remember_typescript_snapshot(&fixture.0, [1; 32], binding, &staged);
        let proof = owner
            .typescript_snapshot_reuse(
                &fixture.0,
                profile(),
                [1; 32],
                binding,
                &AtomicBool::new(false),
            )
            .unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("deps/two.d.ts"), &link).unwrap();
        assert!(!proof.is_current(None));
    }
}
