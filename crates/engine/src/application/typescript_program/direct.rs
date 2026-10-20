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
    if matches!(
        checkpoint.checkpoint(0),
        Err(TszProjectExecutionStop::WorkBudgetExhausted)
    ) {
        return Err(TypeScriptProjectHostError::CompilerIoClosureLimit {
            phase: "native TSZ program admission".into(),
            detail: format!("native parse and root/module closure work exceeded {MAX_CLOSURE_REPLAY_WORK_UNITS} units").into_boxed_str(),
        });
    }
    let built = built.map_err(
        |message| TypeScriptProjectHostError::CompilerIoClosureMismatch {
            detail: message.into_boxed_str(),
        },
    )?;
    let solution = forest
        && built.root_names.is_empty()
        && config.is_some_and(|config| !config.references.is_empty());
    if forest && built.root_names.is_empty() && !solution && built.program_diagnostics.is_empty() {
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
    let mut program_diagnostics = built.program_diagnostics;
    for diagnostic in &mut program_diagnostics {
        check_admission_control(deadline, cancelled)?;
        if diagnostic.file.is_empty() {
            continue;
        }
        let source = resolver
            .try_load_source(Path::new(&diagnostic.file))?
            .ok_or_else(|| closure_mismatch("native config diagnostic has no captured source"))?;
        let text = std::str::from_utf8(&source.bytes)
            .map_err(|_| closure_mismatch("native config diagnostic source is not UTF-8"))?;
        let start = diagnostic.start as usize;
        let end = start
            .checked_add(diagnostic.length as usize)
            .ok_or_else(|| closure_mismatch("native config diagnostic span overflows"))?;
        if text.get(start..end).is_none() {
            return Err(closure_mismatch(
                "native config diagnostic span is outside captured source",
            ));
        }
        diagnostic.file = workspace_virtual_path(inputs, &source.path)?;
    }
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
    for diagnostic in &program_diagnostics {
        digest.update(format!("{diagnostic:?}").as_bytes());
    }
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
            program_diagnostics,
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
    mod mixed_sources;
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
    fn native_admission_keeps_conditional_dependencies_and_automatic_types_among_unrelated_packages()
     {
        let fixture = Fixture::new();
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"module":"ESNext","moduleResolution":"Bundler"},"include":["src/**/*"]}"#);
        const SOURCE: &str =
            "import { value } from 'selected/feature'; export const answer: Environment = value;";
        fixture.write("src/main.ts", SOURCE);
        fixture.write("node_modules/selected/package.json", r#"{"name":"selected","exports":{"./feature":{"types":"./types/feature.d.ts","default":"./wrong.js"}}}"#);
        fixture.write(
            "node_modules/selected/types/feature.d.ts",
            "export declare const value: number;",
        );
        fixture.write(
            "node_modules/selected/wrong.js",
            "export const value = 'wrong';",
        );
        fixture.write(
            "node_modules/@types/environment/package.json",
            r#"{"name":"@types/environment","types":"index.d.ts"}"#,
        );
        fixture.write(
            "node_modules/@types/environment/index.d.ts",
            "declare type Environment = number;",
        );
        for index in 0..32 {
            fixture.write(
                &format!("node_modules/unrelated-{index}/package.json"),
                r#"{"name":"unused"}"#,
            );
            fixture.write(
                &format!("node_modules/unrelated-{index}/index.d.ts"),
                "export type Unused = string;",
            );
        }
        let admitted = fixture.admit();
        let input = admitted.inputs();
        assert!(matches!(
            input.authority,
            TypeScriptProjectInputAuthority::Native
        ));
        let mut resolver = input.resolver();
        let native = build_native_inputs(
            &input,
            &mut resolver,
            &[PackageSource::new("src/main.ts", SOURCE).unwrap()],
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == "node_modules/selected/types/feature.d.ts")
        );
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == "node_modules/@types/environment/index.d.ts"),
            "implicit global types remain compiler-selected roots"
        );
        assert!(
            !native
                .sources
                .iter()
                .any(|source| source.path.contains("unrelated-")
                    || source.path.ends_with("wrong.js")),
            "unrelated siblings and the non-selected export arm are not source members"
        );
        assert!(native.options.module_resolutions.iter().any(|request|
                request.specifier == "selected/feature" && matches!(&request.target,
                    TszProjectModuleResolutionTarget::File { path } if path == "node_modules/selected/types/feature.d.ts")));
        native
            .closure_witness
            .validate_current(admitted.witness())
            .unwrap();
        fixture.write(
            "node_modules/selected/types/feature.d.ts",
            "export declare const value: string;",
        );
        assert!(
            native
                .closure_witness
                .validate_current(admitted.witness())
                .is_err(),
            "changed selected dependency bytes cannot reuse a successful directory proof"
        );
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

    mod dotted_modules;
    mod pocketbase_captured45_diagnostic;
}

#[cfg(test)]
mod native_web_closure_tests {
    use super::*;
    use crate::application::typescript_host::TypeScriptProjectHost;
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
                .join(format!("nudox-native-web-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fn native(&self, sources: &[PackageSource<'_>]) -> NativeTypeScriptInputs {
            let host = TypeScriptProjectHost::direct(
                None,
                ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                    .unwrap(),
            );
            let admitted = host.admit(&self.0).unwrap().unwrap();
            let input = admitted.inputs();
            let mut resolver = input.resolver();
            let native = build_native_inputs(
                &input,
                &mut resolver,
                sources,
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
        fn forest(&self, sources: &[PackageSource<'_>]) -> NativeTypeScriptForestInputs {
            let host = TypeScriptProjectHost::direct(
                None,
                ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                    .unwrap(),
            );
            let admitted = host.admit(&self.0).unwrap().unwrap();
            build_native_forest_inputs(
                &admitted.inputs(),
                sources,
                Instant::now() + Duration::from_secs(30),
                &AtomicBool::new(false),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_implicit_jsx_runtime_closure_follows_native_conditional_exports() {
        let fixture = Fixture::new();
        const SOURCE: &str = "export const element = <div />;";
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"module":"ESNext","moduleResolution":"Bundler","jsx":"react-jsx","types":[]},"include":["src/**/*"]}"#);
        fixture.write("src/main.tsx", SOURCE);
        fixture.write("node_modules/react/package.json", r#"{"name":"react","version":"1.0.0","exports":{"./jsx-runtime":{"types":"./types/runtime.d.ts","default":"./wrong.js"}}}"#);
        fixture.write("node_modules/react/types/runtime.d.ts", "export namespace JSX { export interface Element {} export interface IntrinsicElements { div: {}; } } export declare function jsx(): JSX.Element;");
        fixture.write(
            "node_modules/react/wrong.js",
            "export const runtime = 'wrong';",
        );
        let sources = [PackageSource::new("src/main.tsx", SOURCE).unwrap()];
        let native = fixture.native(&sources);
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == "node_modules/react/types/runtime.d.ts")
        );
        assert!(
            !native
                .sources
                .iter()
                .any(|source| source.path.ends_with("wrong.js"))
        );
        assert!(native.options.module_resolutions.iter().any(|request| request.specifier == "react/jsx-runtime" && matches!(&request.target, TszProjectModuleResolutionTarget::File {path} if path == "node_modules/react/types/runtime.d.ts")));
    }

    #[test]
    fn native_triple_slash_types_prefers_type_authority_over_same_named_runtime_package() {
        let fixture = Fixture::new();
        const SOURCE: &str = "\u{feff}/* leading comment */\n/// <reference types = 'environment' resolution-mode = 'require' />\nexport const value: Environment = { ready: true };";
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"module":"NodeNext","moduleResolution":"NodeNext","types":[]},"include":["src/**/*"]}"#);
        fixture.write("src/main.ts", SOURCE);
        fixture.write(
            "node_modules/@types/environment/package.json",
            r#"{"name":"@types/environment","version":"1.0.0","types":"./environment.d.ts"}"#,
        );
        fixture.write(
            "node_modules/@types/environment/environment.d.ts",
            "interface Environment { ready: boolean; }",
        );
        fixture.write(
            "node_modules/environment/package.json",
            r#"{"name":"environment","version":"1.0.0","types":"./wrong.d.ts"}"#,
        );
        fixture.write(
            "node_modules/environment/wrong.d.ts",
            "interface Environment { wrong: string; }",
        );
        let native = fixture.native(&[PackageSource::new("src/main.ts", SOURCE).unwrap()]);
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == "node_modules/@types/environment/environment.d.ts")
        );
        assert!(
            !native
                .sources
                .iter()
                .any(|source| source.path.ends_with("wrong.d.ts"))
        );
    }

    #[test]
    fn native_configured_types_uses_custom_type_roots_with_native_package_selection() {
        let fixture = Fixture::new();
        const SOURCE: &str = "export const value: Environment = { ready: true };";
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"module":"CommonJS","types":["environment"],"typeRoots":["./custom-types"]},"include":["src/**/*"]}"#);
        fixture.write("src/main.ts", SOURCE);
        fixture.write(
            "custom-types/environment/package.json",
            r#"{"name":"environment","version":"1.0.0","types":"./environment.d.ts"}"#,
        );
        fixture.write(
            "custom-types/environment/environment.d.ts",
            "interface Environment { ready: boolean; }",
        );
        let native = fixture.native(&[PackageSource::new("src/main.ts", SOURCE).unwrap()]);
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == "custom-types/environment/environment.d.ts")
        );
    }

    #[test]
    fn native_root_program_respects_dotted_directory_and_wildcard_ancestor_include_contract() {
        let fixture = Fixture::new();
        const SOURCE: &str = "export const value = 1;";
        fixture.write("src.v1/main.ts", SOURCE);
        fixture.write("packages/app/src/main.ts", SOURCE);
        fixture.write(".hidden/stray.ts", SOURCE);
        let sources = [
            PackageSource::new("src.v1/main.ts", SOURCE).unwrap(),
            PackageSource::new("packages/app/src/main.ts", SOURCE).unwrap(),
            PackageSource::new(".hidden/stray.ts", SOURCE).unwrap(),
        ];
        fixture.write(
            "tsconfig.json",
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src.v1"]}"#,
        );
        assert!(fixture.native(&sources).sources.is_empty());
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src.v1/**/*","packages/*/src"]}"#);
        let native = fixture.native(&sources);
        assert_eq!(native.sources.len(), 2);
        assert_eq!(
            native.package_membership[".hidden/stray.ts"],
            TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
        );
    }

    #[test]
    fn native_terminal_recursive_include_keeps_explicit_and_valid_config_roots() {
        let fixture = Fixture::new();
        const SOURCE: &str = "export const value = 1;";
        fixture.write("src/explicit.ts", SOURCE);
        fixture.write("valid/selected.ts", SOURCE);
        fixture.write("src/stray.ts", SOURCE);
        const CONFIG: &str = r#"{/* π: preserved diagnostic offsets */ "compilerOptions":{"noLib":true,"types":[]},"files":["src/explicit.ts"],"include":["src/**","src\\**","valid/**/*"]}"#;
        fixture.write("tsconfig.json", CONFIG);
        let native = fixture.native(&[
            PackageSource::new("src/explicit.ts", SOURCE).unwrap(),
            PackageSource::new("valid/selected.ts", SOURCE).unwrap(),
            PackageSource::new("src/stray.ts", SOURCE).unwrap(),
        ]);
        assert_eq!(native.sources.len(), 2);
        assert_eq!(
            native.package_membership["src/stray.ts"],
            TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
        );
        assert_eq!(native.options.program_diagnostics.len(), 2);
        for (diagnostic, token) in native
            .options
            .program_diagnostics
            .iter()
            .zip([r#""src/**""#, r#""src\\**""#])
        {
            assert_eq!(diagnostic.code, 5010);
            assert_eq!(diagnostic.file, "tsconfig.json");
            assert_eq!(diagnostic.start as usize, CONFIG.find(token).unwrap());
            assert_eq!(diagnostic.length as usize, token.len());
        }
        let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
        authority
            .update(native.sources, native.options, &native.libraries)
            .unwrap();
        let project = authority.project().unwrap();
        assert_eq!(project.program_diagnostics().len(), 2);
        assert!(!project.source_paths().any(|path| path == "tsconfig.json"));
        assert!(
            project
                .program_diagnostics()
                .iter()
                .all(|diagnostic| project.diagnostic_utf8_span(diagnostic).is_none())
        );
    }

    #[test]
    fn native_invalid_includes_retain_diagnostics_without_fabricating_source_roots() {
        let fixture = Fixture::new();
        const SOURCE: &str = "export const unselected = 1;";
        fixture.write("src/unselected.ts", SOURCE);
        fixture.write(
            "tsconfig.json",
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src/**"]}"#,
        );
        let native = fixture.native(&[PackageSource::new("src/unselected.ts", SOURCE).unwrap()]);
        assert!(native.sources.is_empty());
        assert!(native.libraries.is_empty());
        assert!(native.options.module_resolutions.is_empty());
        assert_eq!(native.options.program_diagnostics.len(), 1);
        assert_eq!(native.options.program_diagnostics[0].code, 5010);
        let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
        authority
            .update(native.sources, native.options, &native.libraries)
            .unwrap();
        let project = authority.project().unwrap();
        assert_eq!(project.source_paths().count(), 0);
        assert!(project.check_result().file_results.is_empty());
        assert_eq!(project.diagnostics().count(), 1);
    }

    #[test]
    fn native_solution_config_diagnostics_retain_an_image_and_evict_when_resolved() {
        use crate::application::typescript_program_lane::TypeScriptProgramAuthorityLane;

        for inherited in [false, true] {
            let fixture = Fixture::new();
            const SOURCE: &str = "export const value = 1;";
            fixture.write("child/src/main.ts", SOURCE);
            fixture.write("child/tsconfig.json", r#"{"compilerOptions":{"noLib":true,"types":[],"composite":true},"files":["src/main.ts"]}"#);
            let config = if inherited {
                fixture.write("base.json", r#"{/* π */ "include":["unused/**"]}"#);
                r#"{"extends":"./base.json","compilerOptions":{"noLib":true,"types":[]},"references":[{"path":"./child"}]}"#
            } else {
                r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["unused/**"],"references":[{"path":"./child"}]}"#
            };
            fixture.write("tsconfig.json", config);
            let selected = [PackageSource::new("child/src/main.ts", SOURCE).unwrap()];
            let forest = fixture.forest(&selected);
            let root = forest.nodes.into_iter().nth(forest.root as usize).unwrap();
            let key = root.config.map(|(path, _)| path);
            assert!(root.native.solution);
            assert!(root.native.sources.is_empty());
            assert!(root.native.libraries.is_empty());
            assert!(root.native.requires_retained_authority());
            let diagnostics = &root.native.options.program_diagnostics;
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, 5010);
            assert_eq!(
                diagnostics[0].file,
                if inherited {
                    "base.json"
                } else {
                    "tsconfig.json"
                }
            );

            let mut lane = TypeScriptProgramAuthorityLane::default();
            lane.prepare(&fixture.0, &[key.clone()]).unwrap();
            lane.authority(key.clone())
                .update(
                    root.native.sources,
                    root.native.options,
                    &root.native.libraries,
                )
                .unwrap();
            let image = lane.project(&key).unwrap();
            assert_eq!(image.program_diagnostics().len(), 1);
            assert_eq!(image.source_paths().count(), 0);
            assert!(image.check_result().file_results.is_empty());

            if inherited {
                fixture.write("base.json", r#"{"include":[]}"#);
            } else {
                fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"types":[]},"include":[],"references":[{"path":"./child"}]}"#);
            }
            let resolved = fixture.forest(&selected);
            let root = &resolved.nodes[resolved.root as usize];
            assert!(root.native.solution);
            assert!(!root.native.requires_retained_authority());
            assert!(root.native.options.program_diagnostics.is_empty());
            lane.prepare(&fixture.0, &[]).unwrap();
            assert!(lane.project(&key).is_none());
        }
    }

    #[test]
    fn native_missing_source_references_retain_diagnostics_and_negative_witnesses() {
        let fixture = Fixture::new();
        const SOURCE: &str = "#!/usr/bin/env node\n/// <reference path='./missing.d.ts' />\n/// <reference types='absent-types' />\nexport const value = 1;";
        fixture.write("src/main.ts", SOURCE);
        fixture.write(
            "tsconfig.json",
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src/**/*"]}"#,
        );
        let native = fixture.native(&[PackageSource::new("src/main.ts", SOURCE).unwrap()]);
        assert_eq!(native.sources.len(), 1);
        assert_eq!(native.options.module_resolutions.len(), 2);
        assert!(
            native
                .options
                .module_resolutions
                .iter()
                .all(|request| matches!(
                    request.target,
                    TszProjectModuleResolutionTarget::Unresolved
                ))
        );
        let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
        authority
            .update(native.sources, native.options, &native.libraries)
            .unwrap();
        let diagnostics = authority
            .project()
            .unwrap()
            .check_result()
            .file_results
            .iter()
            .flat_map(|file| &file.diagnostics)
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert!(diagnostics.contains(&6053));
        assert!(diagnostics.contains(&2688));

        fixture.write("src/missing.d.ts", "interface AppearedAfterCapture {}");
        let host = TypeScriptProjectHost::direct(
            None,
            ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                .unwrap(),
        );
        let changed = host.admit(&fixture.0).unwrap().unwrap();
        assert!(
            native
                .closure_witness
                .validate_current(changed.witness())
                .is_err()
        );
    }

    #[test]
    fn native_no_default_lib_suppresses_config_libraries_and_keeps_source_lib_requests() {
        let fixture = Fixture::new();
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"target":"ES2022","lib":["es5"],"types":[],"noResolve":true},"include":["src/**/*"]}"#);
        for (source, suppressed) in [
            (
                "#!/usr/bin/env node\n/// <reference no-default-lib='true' />\nexport {};",
                true,
            ),
            (
                "/// <reference no-default-lib='false' />\nexport {};",
                false,
            ),
            (
                "/* /// <reference no-default-lib='true' /> */\nexport {};",
                false,
            ),
            ("export {};\n/// <reference no-default-lib='true' />", false),
        ] {
            fixture.write("src/main.ts", source);
            let native = fixture.native(&[PackageSource::new("src/main.ts", source).unwrap()]);
            assert_eq!(native.libraries.is_empty(), suppressed);
            assert!(
                !native.options.checker.no_lib,
                "a source directive does not rewrite the explicit noLib option"
            );
        }
        const REFERENCED: &str = "/// <reference no-default-lib='true' />\n/// <reference lib='es2015.promise' />\nexport {};";
        fixture.write("src/main.ts", REFERENCED);
        let native = fixture.native(&[PackageSource::new("src/main.ts", REFERENCED).unwrap()]);
        assert!(
            native
                .libraries
                .iter()
                .any(|lib| lib.file_name.ends_with("lib.es2015.promise.d.ts"))
        );
        assert!(
            !native
                .libraries
                .iter()
                .any(|lib| lib.file_name.ends_with("lib.es5.d.ts"))
        );
        fixture.write(
            "tsconfig.json",
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src/**/*"]}"#,
        );
        assert!(
            fixture
                .native(&[PackageSource::new("src/main.ts", REFERENCED).unwrap()])
                .libraries
                .is_empty()
        );
    }

    #[test]
    fn native_dependency_no_default_lib_suppresses_program_libraries_before_embedded_insertion() {
        let fixture = Fixture::new();
        const SOURCE: &str = "/// <reference path='./dependency.d.ts' />\nexport {};";
        fixture.write(
            "tsconfig.json",
            r#"{"compilerOptions":{"lib":["es5"],"types":[]},"files":["src/main.ts"]}"#,
        );
        fixture.write("src/main.ts", SOURCE);
        fixture.write("src/dependency.d.ts", "interface Dependency {}");
        let selected = [PackageSource::new("src/main.ts", SOURCE).unwrap()];
        let baseline = fixture.native(&selected);
        assert_eq!(baseline.sources.len(), 2);
        assert!(
            baseline
                .libraries
                .iter()
                .any(|library| library.file_name.ends_with("lib.es5.d.ts")),
            "embedded libraries' own directives cannot suppress their insertion"
        );

        fixture.write(
            "src/dependency.d.ts",
            "/// <reference no-default-lib='true' />\ninterface Dependency {}",
        );
        let suppressed = fixture.native(&selected);
        assert_eq!(suppressed.sources.len(), 2);
        assert!(suppressed.libraries.is_empty());
        assert!(!suppressed.options.checker.no_lib);
    }

    #[test]
    fn native_missing_configured_types_retain_global_diagnostics_and_negative_witnesses() {
        let fixture = Fixture::new();
        const SOURCE: &str = "export const value = 1;";
        fixture.write("src/main.ts", SOURCE);
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"types":["environment","environment"],"typeRoots":["./custom-types"]},"include":["src/**/*"]}"#);
        let native = fixture.native(&[PackageSource::new("src/main.ts", SOURCE).unwrap()]);
        assert_eq!(native.sources.len(), 1);
        assert!(native.options.module_resolutions.is_empty());
        assert_eq!(native.options.program_diagnostics.len(), 1);
        let diagnostic = &native.options.program_diagnostics[0];
        assert_eq!(
            (
                diagnostic.code,
                diagnostic.file.as_str(),
                diagnostic.start,
                diagnostic.length
            ),
            (2688, "", 0, 0)
        );
        assert!(
            diagnostic
                .related_information
                .iter()
                .any(|reason| reason.code == 1417)
        );
        let environment = native.options.environment;
        let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
        authority
            .update(native.sources, native.options, &native.libraries)
            .unwrap();
        assert_eq!(authority.project().unwrap().program_diagnostics().len(), 1);
        fixture.write(
            "custom-types/environment/index.d.ts",
            "interface Environment {}",
        );
        let resolved = fixture.native(&[PackageSource::new("src/main.ts", SOURCE).unwrap()]);
        assert!(resolved.options.program_diagnostics.is_empty());
        assert_ne!(environment, resolved.options.environment);
        let host = TypeScriptProjectHost::direct(
            None,
            ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                .unwrap(),
        );
        let changed = host.admit(&fixture.0).unwrap().unwrap();
        assert!(
            native
                .closure_witness
                .validate_current(changed.witness())
                .is_err()
        );
    }
    #[test]
    fn native_vite_client_types_use_package_declarations_after_primary_type_lookup() {
        let fixture = Fixture::new();
        const CONFIGURED: &str = "export const mode: ImportMetaEnv = { MODE: 'test' };";
        const REFERENCED: &str = "/// <reference types = 'vite/client' />\nexport const mode: ImportMetaEnv = { MODE: 'test' };";
        fixture.write(
            "node_modules/vite/package.json",
            r#"{"name":"vite","version":"1.0.0","exports":{"./client":{"types":"./client.d.ts"}}}"#,
        );
        fixture.write(
            "node_modules/vite/client.d.ts",
            "interface ImportMetaEnv { MODE: string; }",
        );
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"module":"ESNext","moduleResolution":"Bundler","types":["vite/client"]},"include":["src/**/*"]}"#);
        fixture.write("src/main.ts", CONFIGURED);
        let configured = fixture.native(&[PackageSource::new("src/main.ts", CONFIGURED).unwrap()]);
        assert!(
            configured
                .sources
                .iter()
                .any(|source| source.path == "node_modules/vite/client.d.ts")
        );
        fixture.write("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"module":"ESNext","moduleResolution":"Bundler","types":[]},"include":["src/**/*"]}"#);
        fixture.write("src/main.ts", REFERENCED);
        let referenced = fixture.native(&[PackageSource::new("src/main.ts", REFERENCED).unwrap()]);
        assert!(
            referenced
                .sources
                .iter()
                .any(|source| source.path == "node_modules/vite/client.d.ts")
        );
    }
}

#[cfg(test)]
mod native_import_kind_controls {
    use super::*;
    use crate::application::typescript_host::TypeScriptProjectHost;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn native_typeof_import_and_dynamic_import_keep_distinct_native_conditional_targets() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nudox-native-import-kinds-{}-{nonce}",
            std::process::id()
        ));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        std::fs::create_dir_all(directory.join("src")).unwrap();
        std::fs::create_dir_all(directory.join("node_modules/dual/types")).unwrap();
        let fixture = Cleanup(directory.canonicalize().unwrap());
        const SOURCE: &str =
            "export type Static = typeof import('dual'); export const dynamic = import('dual');";
        for (path, text) in [
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"noLib":true,"module":"NodeNext","moduleResolution":"NodeNext","types":[]},"include":["src/**/*"]}"#,
            ),
            ("src/main.ts", SOURCE),
            (
                "node_modules/dual/package.json",
                r#"{"name":"dual","version":"1.0.0","exports":{".":{"types":{"import":"./types/import.d.ts","require":"./types/require.d.ts"}}}}"#,
            ),
            (
                "node_modules/dual/types/import.d.ts",
                "export declare const selected: 'import';",
            ),
            (
                "node_modules/dual/types/require.d.ts",
                "export declare const selected: 'require';",
            ),
        ] {
            std::fs::write(fixture.0.join(path), text).unwrap();
        }
        let host = TypeScriptProjectHost::direct(
            None,
            ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                .unwrap(),
        );
        let admitted = host.admit(&fixture.0).unwrap().unwrap();
        let input = admitted.inputs();
        let mut resolver = input.resolver();
        let native = build_native_inputs(
            &input,
            &mut resolver,
            &[PackageSource::new("src/main.ts", SOURCE).unwrap()],
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
        let exact = |kind, expected: &str| {
            native.options.module_resolutions.iter().any(|request| request.specifier == "dual" && request.request_kind == kind && matches!(&request.target,TszProjectModuleResolutionTarget::File {path} if path == expected))
        };
        assert!(exact(
            TszProjectModuleRequestKind::EsmImport,
            "node_modules/dual/types/require.d.ts"
        ));
        assert!(exact(
            TszProjectModuleRequestKind::DynamicImport,
            "node_modules/dual/types/import.d.ts"
        ));
    }
}

#[cfg(test)]
#[path = "direct/tests/independent_projects.rs"]
mod independent_project_tests;
