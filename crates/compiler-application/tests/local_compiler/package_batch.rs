//! Proves that a package source frontier becomes one reopened semantic generation.

use std::{
    fs, num::NonZeroUsize, path::PathBuf, process::Command, sync::atomic::AtomicBool,
    time::Duration,
};

use compiler_application::{
    DocumentationSession, LocalCompiler, LocalCompilerClient, LocalCompilerConfig,
    LocalCompilerControl, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
    LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
    LocalRuntimeToolchain, LocalToolchainSet, OwnedPackageSource, OwnedPackageSourceSet,
    PackageSource, PackageSourceSet,
};
use compiler_driver::{ResolvedToolchain, ToolchainSelection};
use backend_semantic::ir::SemanticImageView;
use backend_semantic::vocabulary::{CStandard, LanguageProfile, NativeTool, Stage};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompilePhase, PackageCompileRequest, PackageUrl,
};
use backend_store::journal::PublicationLimits;

#[test]
fn two_sources_publish_as_one_reopened_package_generation() -> Result<(), Box<dyn std::error::Error>>
{
    let clang = find_clang().ok_or("clang is required for the package semantic journey")?;
    let version = Command::new(&clang).arg("--version").output()?;
    if !version.status.success() {
        return Err("clang version probe failed".into());
    }
    let root = unique_directory()?;
    let package_root = root.join("package");
    let artifacts = root.join("artifacts");
    let journal = root.join("journal");
    let native_work = root.join("native-work");
    fs::create_dir_all(package_root.join("src"))?;
    fs::create_dir(&native_work)?;
    let first = "int alpha(void) { return 1; }\n";
    let second = "int beta(void) { return 2; }\n";
    fs::write(package_root.join("src/alpha.c"), first)?;
    fs::write(package_root.join("src/beta.c"), second)?;

    let toolchains = [ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_version(NativeTool::Clang, &clang, &version.stdout)?,
    )];
    let selected = LocalToolchainSet::validate(&toolchains)?;
    let cancelled = AtomicBool::new(false);
    let mut scratch = LocalCompilerScratch::with_fragment_capacity(
        NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
    )?;
    let mut compiler = LocalCompiler::create(
        LocalCompilerConfig {
            toolchains: selected,
            artifact_directory: &artifacts,
            journal_directory: &journal,
            native_work_directory: &native_work,
            control: LocalCompilerControl {
                timeout: LocalCompilerTimeout::new(Duration::from_secs(30))?,
                cancelled: &cancelled,
            },
        },
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        &mut scratch,
    )?;
    let package_url = PackageUrl::try_from("pkg:generic/sample@1.0.0".to_owned())
        .map_err(|error| fixture_error(format!("package URL rejected: {error:?}")))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(31),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package_url,
    )
    .map_err(|error| fixture_error(format!("package profile rejected: {error:?}")))?;
    let sources = [
        PackageSource::new("src/alpha.c", first)?,
        PackageSource::new("src/beta.c", second)?,
    ];
    let package = PackageSourceSet::new(&request, &package_root, &sources)?;
    let mut phases = Vec::new();
    let published = compiler.compile_package_sources(package, &mut |phase| phases.push(phase))?;

    assert_eq!(published.publication.manifest.fragment_count, 2);
    assert_eq!(published.images.len(), 2);
    assert_eq!(
        phases,
        [
            PackageCompilePhase::Authority,
            PackageCompilePhase::Lower,
            PackageCompilePhase::Authority,
            PackageCompilePhase::Lower,
            PackageCompilePhase::Publish,
            PackageCompilePhase::Reopen,
        ]
    );
    let mut names = Vec::new();
    for image in &published.images {
        let view = SemanticImageView::reopen(image.as_ref())?;
        let session = DocumentationSession::new(&view);
        for entity in session.canonical_entities() {
            names.push(entity?.name.to_vec());
        }
    }
    assert!(names.iter().any(|name| name.as_slice() == b"alpha"));
    assert!(names.iter().any(|name| name.as_slice() == b"beta"));
    assert!(
        published
            .publication
            .publication
            .immutable
            .checksum
            .iter()
            .any(|byte| *byte != 0)
    );

    compiler.shutdown()?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn owned_runtime_frontier_reaches_the_package_publication_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let clang = find_clang().ok_or("clang is required for the package runtime journey")?;
    let version = Command::new(&clang).arg("--version").output()?;
    if !version.status.success() {
        return Err("clang version probe failed".into());
    }
    let root = unique_directory()?;
    let package_root = root.join("package");
    let native_work = root.join("native-work");
    fs::create_dir_all(package_root.join("src"))?;
    fs::create_dir(&native_work)?;
    let package_url = PackageUrl::try_from("pkg:generic/sample@1.0.0".to_owned())
        .map_err(|error| fixture_error(format!("package URL rejected: {error:?}")))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(32),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package_url,
    )
    .map_err(|error| fixture_error(format!("package profile rejected: {error:?}")))?;
    let configuration = LocalCompilerRuntimeConfiguration::new(
        LocalCompilerRuntimePaths::new(root.join("artifacts"), root.join("journal"), native_work)?,
        vec![LocalRuntimeToolchain::resolved(
            NativeTool::Clang,
            clang,
            &version.stdout,
        )?]
        .into_boxed_slice(),
        Box::new([]),
        LocalRuntimePackageAuthority::default(),
        LocalCompilerTimeout::new(Duration::from_secs(30))?,
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
        )?,
    )?;
    let client = LocalCompilerClient::start(configuration)?;
    let sources = vec![
        OwnedPackageSource::new("src/alpha.c", "int alpha(void) { return 1; }\n")?,
        OwnedPackageSource::new("src/beta.c", "int beta(void) { return 2; }\n")?,
    ]
    .into_boxed_slice();
    let published = client.compile_package_sources(OwnedPackageSourceSet::new(
        request.clone(),
        package_root,
        sources,
    )?)?;

    assert_eq!(published.publication.manifest.fragment_count, 2);
    assert_eq!(published.images.len(), 2);
    let replacement = client.compile_package_sources(OwnedPackageSourceSet::new(
        request,
        root.join("replacement"),
        vec![OwnedPackageSource::new(
            "src/gamma.c",
            "int gamma(void) { return 3; }\n",
        )?]
        .into_boxed_slice(),
    )?)?;
    assert_ne!(
        replacement.publication.binding.generation,
        published.publication.binding.generation
    );
    let activated = client.activate_semantic_generation(
        LanguageProfile::C(CStandard::C23),
        published.publication.manifest,
        published.publication.binding,
    )?;
    assert_eq!(activated.manifest, published.publication.manifest);
    assert_eq!(activated.binding, published.publication.binding);
    assert_eq!(activated.images.len(), 2);
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

fn fixture_error(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn find_clang() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join("clang"))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

fn unique_directory() -> Result<PathBuf, std::io::Error> {
    for ordinal in 0_u16..64 {
        let path = std::env::temp_dir().join(format!(
            "compiler-package-batch-{}-{ordinal}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "package batch fixture capacity exhausted",
    ))
}
