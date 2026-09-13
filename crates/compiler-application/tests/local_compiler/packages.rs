//! Proves that package compilation enters only exact caller-selected source paths.

use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use compiler_application::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch,
    LocalCompilerTimeout, LocalPackageRoot, LocalPackageRootSet, LocalToolchainSet,
};
use backend_semantic::vocabulary::{CStandard, Language, LanguageProfile, NativeTool, Stage};
use interface_core::{
    CompilerCapability, CompilerTerminal, CorrelationId, GenerateTarget, PackageCompilePhase,
    PackageCompileRequest, PackageEcosystem, PackageSourceCause, PackageUrl, PackageUrlError,
};
use server_journal::{PublicationLimits, ShutdownError};

static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct PackageFixture {
    root: PathBuf,
    artifacts: PathBuf,
    journal: PathBuf,
    native_work: PathBuf,
}

impl PackageFixture {
    fn create() -> Result<Self, io::Error> {
        for _ in 0..64 {
            let serial = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "compiler-application-package-{}-{serial}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let artifacts = root.join("artifacts");
                    let journal = root.join("journal");
                    let native_work = root.join("native-work");
                    fs::create_dir(&native_work)?;
                    return Ok(Self {
                        root,
                        artifacts,
                        journal,
                        native_work,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "package test fixture capacity exhausted",
        ))
    }

    fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    fn remove(self) -> Result<(), io::Error> {
        fs::remove_dir_all(self.root)
    }
}

fn request(spelling: &str) -> PackageCompileRequest {
    let package = PackageUrl::try_from(spelling.to_owned()).expect("canonical package URL");
    PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(91),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package,
    )
    .expect("generic package belongs to the C family")
}

fn limits() -> PublicationLimits {
    let two = NonZeroUsize::new(2).expect("two is nonzero");
    PublicationLimits::new(two, two).expect("two publication slots are representable")
}

fn compiler<'path, 'scratch, 'cancel>(
    fixture: &'path PackageFixture,
    roots: LocalPackageRootSet<'path>,
    cancelled: &'cancel AtomicBool,
    scratch: &'scratch mut LocalCompilerScratch,
) -> LocalCompiler<'path, 'scratch, 'cancel> {
    let toolchains = LocalToolchainSet::validate(&[]).expect("empty toolchain table is canonical");
    let config = LocalCompilerConfig {
        toolchains,
        artifact_directory: &fixture.artifacts,
        journal_directory: &fixture.journal,
        native_work_directory: &fixture.native_work,
        control: LocalCompilerControl {
            timeout: LocalCompilerTimeout::new(Duration::from_secs(10))
                .expect("ten seconds is a legal local timeout"),
            cancelled,
        },
    };
    LocalCompiler::create_with_package_roots(config, roots, limits(), scratch)
        .expect("package compiler fixture opens")
}

#[test]
fn exact_subpath_enters_source_before_toolchain_selection() -> Result<(), io::Error> {
    let fixture = PackageFixture::create()?;
    let store = fixture.store();
    let source_path = store.join("sqlite/3.50.4/src/main.c");
    fs::create_dir_all(source_path.parent().expect("source has parent"))?;
    let source = b"int answer(void) { return 42; }\n";
    fs::write(&source_path, source)?;
    let root = LocalPackageRoot::new(PackageEcosystem::Generic, &store)
        .expect("fixture store is absolute");
    let roots = LocalPackageRootSet::validate(core::slice::from_ref(&root))
        .expect("single root is canonical");
    let cancelled = AtomicBool::new(false);
    let mut scratch = LocalCompilerScratch::default();
    let mut compiler = compiler(&fixture, roots, &cancelled, &mut scratch);
    let mut phases = Vec::new();
    let error = compiler
        .compile_package(
            &request("pkg:generic/sqlite@3.50.4#src/main.c"),
            &mut |phase| phases.push(phase),
        )
        .expect_err("the fixture intentionally has no Clang toolchain");
    assert_eq!(
        phases,
        [
            PackageCompilePhase::Locate,
            PackageCompilePhase::EnterSource,
            PackageCompilePhase::Authority,
        ]
    );
    assert!(matches!(
        error,
        CompilerTerminal::Toolchain {
            language: Language::Clang,
            stage: Stage::LowerIr,
            selected: NativeTool::Clang,
            configured: None,
            ..
        }
    ));
    compiler.shutdown().map_err(shutdown_io)?;
    fixture.remove()
}

#[test]
fn omitted_subpath_reaches_resolution_but_noncanonical_escape_stops_at_admission()
-> Result<(), io::Error> {
    let fixture = PackageFixture::create()?;
    let store = fixture.store();
    fs::create_dir_all(store.join("sqlite/3.50.4/src"))?;
    fs::write(store.join("sqlite/3.50.4/main.c"), b"int main(void) {}\n")?;
    let root = LocalPackageRoot::new(PackageEcosystem::Generic, &store)
        .expect("fixture store is absolute");
    let roots = LocalPackageRootSet::validate(core::slice::from_ref(&root))
        .expect("single root is canonical");
    let cancelled = AtomicBool::new(false);
    let mut scratch = LocalCompilerScratch::default();
    let mut compiler = compiler(&fixture, roots, &cancelled, &mut scratch);

    let mut missing_phases = Vec::new();
    let missing = compiler
        .compile_package(&request("pkg:generic/sqlite@3.50.4"), &mut |phase| {
            missing_phases.push(phase)
        })
        .expect_err("source selection is mandatory");
    assert_eq!(missing_phases, [PackageCompilePhase::Locate]);
    assert!(matches!(
        missing,
        CompilerTerminal::PackageSource {
            phase: PackageCompilePhase::Locate,
            cause: PackageSourceCause::SubpathRequired {
                ecosystem: PackageEcosystem::Generic,
            },
            ..
        }
    ));

    let traversal = PackageUrl::try_from("pkg:generic/sqlite@3.50.4#src/%2E%2E/main.c".to_owned())
        .expect_err("an escape for an unreserved dot is not canonical");
    assert_eq!(traversal.error, PackageUrlError::Escape { offset: 30 });

    compiler.shutdown().map_err(shutdown_io)?;
    fixture.remove()
}

#[cfg(unix)]
#[test]
fn package_symlink_cannot_escape_the_configured_store() -> Result<(), io::Error> {
    use std::os::unix::fs::symlink;

    let fixture = PackageFixture::create()?;
    let store = fixture.store();
    let outside = fixture.root.join("outside");
    fs::create_dir_all(store.join("sqlite"))?;
    fs::create_dir_all(&outside)?;
    fs::write(outside.join("main.c"), b"int escaped(void) {}\n")?;
    symlink(&outside, store.join("sqlite/3.50.4"))?;
    let root = LocalPackageRoot::new(PackageEcosystem::Generic, &store)
        .expect("fixture store is absolute");
    let roots = LocalPackageRootSet::validate(core::slice::from_ref(&root))
        .expect("single root is canonical");
    let cancelled = AtomicBool::new(false);
    let mut scratch = LocalCompilerScratch::default();
    let mut compiler = compiler(&fixture, roots, &cancelled, &mut scratch);
    let error = compiler
        .compile_package(&request("pkg:generic/sqlite@3.50.4#main.c"), &mut |_| {})
        .expect_err("package symlink escaped the configured store");
    match error {
        CompilerTerminal::PackageSource {
            phase: PackageCompilePhase::Locate,
            cause: PackageSourceCause::PackageEscapesStore { store, package },
            ..
        } => {
            assert_eq!(store.as_ref(), fs::canonicalize(&fixture.store())?);
            assert_eq!(package.as_ref(), fs::canonicalize(&outside)?);
        }
        observed => panic!("unexpected package escape terminal: {observed:?}"),
    }
    compiler.shutdown().map_err(shutdown_io)?;
    fixture.remove()
}

#[test]
fn package_root_table_rejects_relative_duplicate_and_unordered_rows() {
    assert!(matches!(
        LocalPackageRoot::new(PackageEcosystem::Cargo, Path::new("relative")),
        Err(compiler_application::LocalPackageRootError::Relative {
            ecosystem: PackageEcosystem::Cargo,
        })
    ));
    let cargo = LocalPackageRoot::new(PackageEcosystem::Cargo, Path::new("/stores/cargo"))
        .expect("absolute root");
    let cargo_again =
        LocalPackageRoot::new(PackageEcosystem::Cargo, Path::new("/stores/cargo-again"))
            .expect("absolute root");
    assert!(matches!(
        LocalPackageRootSet::validate(&[cargo, cargo_again]),
        Err(compiler_application::LocalPackageRootSetError::Duplicate {
            ecosystem: PackageEcosystem::Cargo,
        })
    ));
    let generic = LocalPackageRoot::new(PackageEcosystem::Generic, Path::new("/stores/generic"))
        .expect("absolute root");
    assert!(matches!(
        LocalPackageRootSet::validate(&[generic, cargo]),
        Err(compiler_application::LocalPackageRootSetError::OutOfOrder {
            preceding: PackageEcosystem::Generic,
            observed: PackageEcosystem::Cargo,
        })
    ));
}

fn shutdown_io(error: ShutdownError) -> io::Error {
    io::Error::other(error)
}
