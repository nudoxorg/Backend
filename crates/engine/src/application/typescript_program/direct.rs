//! Captured filesystem adapter for the linked TSZ program builder.

use super::*;
use backend_frontend_typescript::native_project::{
    ResolutionDirEntry, ResolutionFileKind, ResolutionFileSystem,
};
use backend_frontend_typescript::{TszExecutionCheckpoint, TszProjectExecutionStop};
use std::cell::RefCell;
use std::io;

struct CapturedNativeFileSystem<'resolver, 'witness> {
    resolver: RefCell<&'resolver mut TypeScriptResolverCapability<'witness>>,
    error: RefCell<Option<TypeScriptProjectHostError>>,
    boundary: &'resolver Path,
    deadline: Instant,
    cancelled: &'resolver AtomicBool,
}

impl CapturedNativeFileSystem<'_, '_> {
    fn observe<T>(
        &self,
        operation: impl FnOnce(
            &mut TypeScriptResolverCapability<'_>,
        ) -> Result<T, TypeScriptProjectHostError>,
    ) -> io::Result<T> {
        if self.error.borrow().is_some() {
            return Err(io::Error::other(
                "native TSZ captured filesystem already refused",
            ));
        }
        let result = check_admission_control(self.deadline, self.cancelled)
            .and_then(|()| operation(&mut self.resolver.borrow_mut()));
        result.map_err(|error| {
            let message = error.to_string();
            *self.error.borrow_mut() = Some(error);
            io::Error::other(message)
        })
    }
}

impl ResolutionFileSystem for CapturedNativeFileSystem<'_, '_> {
    fn resolution_boundary(&self) -> Option<&Path> {
        Some(self.boundary)
    }
    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        self.observe(|resolver| {
            resolver
                .try_load_source(path)
                .map(|source| source.map(|source| source.bytes.to_vec()))
        })
    }
    fn is_file(&self, path: &Path) -> io::Result<bool> {
        self.observe(|resolver| {
            resolver
                .native_path_kind(path, false)
                .map(|kind| kind == Some(TypeScriptDirectoryEntryKind::RegularFile))
        })
    }
    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        self.observe(|resolver| resolver.directory_exists(path))
    }
    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        self.observe(|resolver| {
            resolver
                .realpath(path)
                .map(|path| path.map(|path| path.into_path_buf()))
        })
    }
    fn symlink_metadata(&self, path: &Path) -> io::Result<Option<ResolutionFileKind>> {
        self.observe(|resolver| {
            resolver
                .native_path_kind(path, true)
                .map(|kind| kind.map(native_kind))
        })
    }
    fn read_dir(&self, path: &Path) -> io::Result<Option<Vec<ResolutionDirEntry>>> {
        self.observe(|resolver| {
            let Some(canonical) = resolver.observe_program_directory(path)? else {
                return Ok(None);
            };
            let entries = resolver
                .observed_program_directory_entries(&canonical)
                .expect("captured native directory was just admitted");
            Ok(Some(
                entries
                    .iter()
                    .map(|entry| ResolutionDirEntry {
                        name: entry
                            .path
                            .file_name()
                            .expect("captured entry has a name")
                            .to_owned(),
                        kind: native_kind(entry.kind),
                    })
                    .collect(),
            ))
        })
    }
}

fn native_kind(kind: TypeScriptDirectoryEntryKind) -> ResolutionFileKind {
    match kind {
        TypeScriptDirectoryEntryKind::RegularFile => ResolutionFileKind::File,
        TypeScriptDirectoryEntryKind::Directory => ResolutionFileKind::Directory,
        TypeScriptDirectoryEntryKind::Symlink => ResolutionFileKind::Symlink,
        TypeScriptDirectoryEntryKind::Other => ResolutionFileKind::Other,
    }
}

struct NativeAdmissionCheckpoint<'a> {
    deadline: Instant,
    cancelled: &'a AtomicBool,
    work: std::sync::atomic::AtomicU64,
}
impl TszExecutionCheckpoint for NativeAdmissionCheckpoint<'_> {
    fn checkpoint(&self, work: u64) -> Result<(), TszProjectExecutionStop> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TszProjectExecutionStop::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(TszProjectExecutionStop::Deadline);
        }
        let previous = self
            .work
            .fetch_add(work, std::sync::atomic::Ordering::Relaxed);
        if previous.saturating_add(work) > MAX_CLOSURE_REPLAY_WORK_UNITS {
            return Err(TszProjectExecutionStop::WorkBudgetExhausted);
        }
        Ok(())
    }
}

pub(super) fn build_direct_inputs_for_config(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    package_sources: &[PackageSource<'_>],
    config: Option<&super::super::typescript_host::TypeScriptConfigInput>,
    forest: bool,
    aggregate: &mut ForestAdmissionBudget,
    validated_host: Option<&ValidatedTypeScriptHost<'_>>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTypeScriptInputs, TypeScriptProjectHostError> {
    check_admission_control(deadline, cancelled)?;
    let checkpoint = NativeAdmissionCheckpoint {
        deadline,
        cancelled,
        work: std::sync::atomic::AtomicU64::new(aggregate.replay_work),
    };
    let inferred_roots = package_sources
        .iter()
        .map(|source| inputs.package_root.join(source.relative_path()))
        .collect::<Vec<_>>();
    let fs = CapturedNativeFileSystem {
        resolver: RefCell::new(resolver),
        error: RefCell::new(None),
        boundary: inputs.workspace_root,
        deadline,
        cancelled,
    };
    let built = backend_frontend_typescript::native_project::build(
        &fs,
        config.map(|config| config.path.as_ref()),
        &inferred_roots,
        &checkpoint,
    );
    if let Some(error) = fs.error.into_inner() {
        return Err(error);
    }
    drop(fs.resolver);
    check_admission_control(deadline, cancelled)?;
    let built = built.map_err(
        |message| TypeScriptProjectHostError::CompilerIoClosureMismatch {
            detail: message.into_boxed_str(),
        },
    )?;
    let solution = forest
        && built.root_names.is_empty()
        && config.is_some_and(|config| !config.references.is_empty());
    if forest && built.root_names.is_empty() && !solution {
        return Err(closure_mismatch(
            "empty native TypeScript config has no solution edges",
        ));
    }
    // A solution container does not acquire implicit global types or libraries.
    if solution && !built.sources.is_empty() {
        return Err(closure_mismatch(
            "native TypeScript solution unexpectedly has semantic source members",
        ));
    }
    aggregate.files = aggregate.files.saturating_add(built.sources.len());
    aggregate.requests = aggregate.requests.saturating_add(built.resolutions.len());
    if aggregate.files > MAX_BRIDGE_FILES {
        return Err(closure_limit(
            "native forest sources",
            aggregate.files,
            MAX_BRIDGE_FILES,
        ));
    }
    if aggregate.requests > MAX_BRIDGE_REQUESTS {
        return Err(closure_limit(
            "native forest module requests",
            aggregate.requests,
            MAX_BRIDGE_REQUESTS,
        ));
    }
    let mut paths = BTreeMap::new();
    let mut content_ids = BTreeMap::new();
    let mut sources = Vec::new();
    for (path, text) in &built.sources {
        check_admission_control(deadline, cancelled)?;
        let admitted = resolver.try_load_source(path)?.ok_or_else(|| {
            closure_mismatch("native TypeScript source disappeared after resolution")
        })?;
        if admitted.bytes.as_ref() != text.as_bytes() {
            return Err(closure_mismatch(
                "native TypeScript source bytes changed after resolution",
            ));
        }
        aggregate.source_bytes = aggregate.source_bytes.saturating_add(text.len());
        if aggregate.source_bytes > (256 * 1024 * 1024) {
            return Err(closure_limit(
                "native forest source bytes",
                aggregate.source_bytes,
                (256 * 1024 * 1024),
            ));
        }
        let virtual_path = workspace_virtual_path(inputs, &admitted.path)?;
        paths.insert(normalize_path(&admitted.path), virtual_path.clone());
        content_ids.insert(virtual_path.clone(), admitted.content_id);
        sources.push(TszFileInput {
            path: virtual_path,
            source: text.clone(),
        });
    }
    let mut libraries = Vec::new();
    if !solution {
        for (name, text) in built.libraries {
            let path = format!("{LIB_VIRTUAL_PREFIX}{name}");
            let content_id = ContentId::<SourceFactDomain>::from_canonical_bytes(text.as_bytes());
            let key = (path.clone(), content_id);
            let library = match aggregate.libraries.get(&key) {
                Some(library) => library.clone(),
                None => {
                    let library =
                        TszLibraryInput::from_utf8(path.clone(), text.as_bytes().to_vec())
                            .map_err(|error| {
                                closure_mismatch(&format!(
                                    "linked TypeScript declaration rejected: {error}"
                                ))
                            })?
                            .into_lib_file();
                    aggregate.libraries.insert(key, library.clone());
                    library
                }
            };
            content_ids.insert(path, content_id);
            libraries.push(library);
        }
    }
    let mut membership = BTreeMap::new();
    for source in package_sources {
        check_admission_control(deadline, cancelled)?;
        let absolute = inputs.package_root.join(source.relative_path());
        let admitted = resolver.try_load_source(&absolute)?.ok_or_else(|| {
            TypeScriptProjectHostError::CompilerProgramSourceMissing {
                package_relative_path: source.relative_path().into(),
            }
        })?;
        if admitted.bytes.as_ref() != source.source().as_bytes() {
            return Err(closure_mismatch(
                "captured package source differs from native TypeScript source",
            ));
        }
        let member = match paths.get(&normalize_path(&admitted.path)) {
            Some(path) => TypeScriptPackageSourceMembership::Selected {
                program_path: path.clone().into(),
            },
            None if config.is_some() => {
                TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
            }
            None => {
                return Err(TypeScriptProjectHostError::CompilerProgramSourceMissing {
                    package_relative_path: source.relative_path().into(),
                });
            }
        };
        membership.insert(source.relative_path().into(), member);
    }
    let mut resolutions = Vec::new();
    for request in built.resolutions {
        let importer = paths
            .get(&normalize_path(&request.importer))
            .ok_or_else(|| closure_mismatch("native module importer has no admitted source"))?
            .clone();
        let target = match request.target {
            Some(target) => {
                let admitted = resolver
                    .try_load_source(&target)?
                    .ok_or_else(|| closure_mismatch("native module target disappeared"))?;
                match paths.get(&normalize_path(&admitted.path)) {
                    Some(path) => TszProjectModuleResolutionTarget::File { path: path.clone() },
                    None => {
                        let mut identity = blake3::Hasher::new();
                        identity.update(b"typescript.native-external-module.v1\0");
                        identity.update(admitted.content_id.as_ref());
                        identity.update(admitted.path.as_os_str().as_encoded_bytes());
                        TszProjectModuleResolutionTarget::External {
                            identity: format!("tsz-module:{}", identity.finalize().to_hex()),
                        }
                    }
                }
            }
            None => TszProjectModuleResolutionTarget::Unresolved,
        };
        resolutions.push(TszProjectModuleResolution {
            importer_path: importer,
            specifier: request.specifier,
            request_kind: request.kind,
            resolution_mode: request.mode,
            target,
        });
    }
    resolutions.sort_by_key(|resolution| format!("{resolution:?}"));
    let file_formats = built
        .file_formats
        .into_iter()
        .map(|(path, format)| {
            paths
                .get(&normalize_path(&path))
                .cloned()
                .map(|path| TszProjectFileFormatInput { path, format })
                .ok_or_else(|| closure_mismatch("native file format has no admitted source"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let previous_work = aggregate.replay_work;
    aggregate.replay_work = checkpoint.work.load(std::sync::atomic::Ordering::Relaxed);
    let closure_work = aggregate.replay_work.saturating_sub(previous_work);
    let sealed = match validated_host {
        Some(host) => resolver.seal_with_host(host)?,
        None => resolver.seal()?,
    };
    let mut digest = blake3::Hasher::new();
    digest.update(b"compiler.typescript.direct-tsz-program.v1\0");
    digest.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
    digest.update(&inputs.fingerprint);
    digest.update(sealed.digest());
    digest.update(built.options_identity.as_bytes());
    for (path, content) in &content_ids {
        digest.update(&(path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(content.as_ref());
    }
    for resolution in &resolutions {
        digest.update(format!("{resolution:?}").as_bytes());
    }
    for format in &file_formats {
        digest.update(format!("{format:?}").as_bytes());
    }
    let work = calculate_work_allowance(inputs, &sources, &libraries, closure_work);
    let metrics = ProgramClosureMetrics::default();
    let compiler_accesses: Box<[ProgramAccess]> = Box::new([]);
    let directory_views: Box<[ProgramDirectoryView]> = Box::new([]);
    let access_digest = compiler_access_digest(&compiler_accesses, &directory_views, metrics)?;
    Ok(NativeTypeScriptInputs {
        sources,
        libraries,
        options: TszProjectOptions {
            checker: built.checker,
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            module_resolutions: resolutions,
            file_formats,
            environment: TszEnvironmentFingerprint::from_sha256(*digest.finalize().as_bytes()),
        },
        closure_witness: TypeScriptProgramClosureWitness {
            resolver: sealed,
            compiler_accesses,
            directory_views,
            metrics,
            access_digest,
        },
        package_membership: membership,
        work_units: work.program,
        file_work_units: work.per_file,
        solution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::typescript_host::{
        AdmittedTypeScriptProject, TypeScriptProjectHost, TypeScriptProjectInputAuthority,
    };
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("nudox-direct-tsz-{}-{nonce}", std::process::id()));
            fs::create_dir_all(root.join("src")).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fn admit(&self) -> AdmittedTypeScriptProject {
            let limits =
                ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                    .unwrap();
            TypeScriptProjectHost::direct(None, limits)
                .admit(&self.0)
                .unwrap()
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const MAIN: &str = "import {value} from '@local/value'; export const answer: number = value;";
    const VALUE: &str = "export const value: number = 42;";
    const JS: &str = "module.exports = { editor: true };";

    fn inputs(admitted: &AdmittedTypeScriptProject) -> NativeTypeScriptInputs {
        let input = admitted.inputs();
        let mut resolver = input.resolver();
        let sources = [
            PackageSource::new("src/main.ts", MAIN).unwrap(),
            PackageSource::new("src/value.ts", VALUE).unwrap(),
            PackageSource::new("editor.config.js", JS).unwrap(),
        ];
        let native = build_native_inputs(
            &input,
            &mut resolver,
            &sources,
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
        native
            .closure_witness
            .validate_current(admitted.witness())
            .unwrap();
        native
    }
    fn configured() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("base.json", r#"{"compilerOptions":{"target":"ES2020","module":"CommonJS","strict":true,"baseUrl":".","paths":{"@local/*":["src/*"]},"experimentalDecorators":true,"jsx":"react"}}"#);
        fixture.write("tsconfig.json", r#"{ "extends":"./base.json", "compilerOptions":{"noLib":true}, "include":["src/**/*"] }"#);
        fixture.write("src/main.ts", MAIN);
        fixture.write("src/value.ts", VALUE);
        fixture.write("editor.config.js", JS);
        fixture
    }

    #[test]
    fn direct_tsz_has_no_host_compiler_and_keeps_config_module_and_mixed_source_authority() {
        let fixture = configured();
        let admitted = fixture.admit();
        assert!(matches!(
            admitted.inputs().authority,
            TypeScriptProjectInputAuthority::Native
        ));
        assert!(admitted.checker().is_none());
        assert!(admitted.resolved_toolchain().unwrap().external().is_err());
        assert!(admitted.inputs().compiler_api().is_err());
        let native = inputs(&admitted);
        assert_eq!(native.sources.len(), 2);
        assert!(native.options.checker.experimental_decorators);
        assert!(native.options.module_resolutions.iter().any(|request| request.specifier == "@local/value" && matches!(&request.target, TszProjectModuleResolutionTarget::File {path} if path == "src/value.ts")));
        assert_eq!(
            native.package_membership["editor.config.js"],
            TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
        );
    }

    #[test]
    fn native_options_and_source_closure_changes_have_distinct_semantic_identity() {
        let fixture = configured();
        let first = fixture.admit();
        let native = inputs(&first);
        let environment = *native.options.environment.as_bytes();
        fixture.write("base.json", r#"{"compilerOptions":{"target":"ES2022","module":"CommonJS","strict":true,"baseUrl":".","paths":{"@local/*":["src/*"]},"experimentalDecorators":true}}"#);
        assert!(
            native
                .closure_witness
                .validate_current(first.witness())
                .is_err()
        );
        let second = fixture.admit();
        let next = inputs(&second);
        assert_ne!(environment, *next.options.environment.as_bytes());
        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn direct_tsz_uses_linked_declaration_data_without_project_typescript_install() {
        let fixture = configured();
        fixture.write(
            "tsconfig.json",
            r#"{"extends":"./base.json","include":["src/**/*"]}"#,
        );
        let native = inputs(&fixture.admit());
        assert!(!native.libraries.is_empty());
        assert!(!fixture.0.join("node_modules/typescript").exists());
    }

    #[test]
    fn direct_tsz_observes_cancellation_before_program_admission() {
        let fixture = configured();
        let admitted = fixture.admit();
        let input = admitted.inputs();
        let mut resolver = input.resolver();
        assert!(matches!(
            build_native_inputs(
                &input,
                &mut resolver,
                &[],
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(true)
            ),
            Err(TypeScriptProjectHostError::CompilerProgramCancelled)
        ));
    }

    #[test]
    fn native_glob_closure_ignores_nested_non_directory_misses_and_rejects_changed_negative() {
        let fixture = configured();
        let admitted = fixture.admit();
        let input = admitted.inputs();
        let mut resolver = input.resolver();
        assert_eq!(
            resolver
                .native_path_kind(&fixture.0.join("src/value.ts/missing"), false)
                .unwrap(),
            None
        );
        let witness = resolver.seal().unwrap();
        fs::remove_file(fixture.0.join("src/value.ts")).unwrap();
        fs::create_dir(fixture.0.join("src/value.ts")).unwrap();
        fixture.write("src/value.ts/missing", "present");
        assert!(witness.validate_current(admitted.witness()).is_err());
    }
}
