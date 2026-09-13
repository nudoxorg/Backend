//! Exercises the `backend-flow` operation tests compiler-publication contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Chief-owned public compiler and publication falsifiers.

#[path = "support/native_tooling.rs"]
mod native_tooling;

use std::{
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledFragment, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use compiler_ir::{EntityKind, ImageProvenance, PrimitiveType, TypeNode};
use compiler_languages_rust::{
    LoadError, RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use compiler_publication::{
    OpenPublicationScratch, OpenPublishedError, PublicationScratch, PublishCompiledError,
    PublishControl, PublishedCompilation, binding::COMPILATION_BINDING_BYTES, open_published,
    publish_compiled,
};
use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};
use server_journal::{
    DurablePublisher, PublicationLimitError, PublicationLimits, PublicationOpenError,
    PublicationPaths, ShutdownError,
};
use thiserror::Error;

use native_tooling::{HostTool, NativeToolingError, NativeWork};

static JOURNEY_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
#[allow(
    clippy::large_enum_variant,
    reason = "this one-shot test terminal keeps exact production sources inline without heap indirection"
)]
enum TestFailure {
    #[error(transparent)]
    Tooling(#[from] NativeToolingError),
    #[error(transparent)]
    RustToolchain(#[from] LoadError),
    #[error(transparent)]
    RustProject(#[from] RustAuthorityError),
    #[error("could not create the Rust authority fixture at {path}")]
    CreateAuthority {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write the Rust authority fixture at {path}")]
    WriteAuthority {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not create the compiler publication journey directory")]
    CreateJourney(#[source] std::io::Error),
    #[error(transparent)]
    PublicationLimit(#[from] PublicationLimitError),
    #[error("could not create or reopen the durable compiler publisher")]
    Publisher(#[from] PublicationOpenError),
    #[error("compiler output could not reach durable publication")]
    Publish(#[from] PublishCompiledError),
    #[error("the durable compiler publication could not be independently reopened")]
    Open(#[from] OpenPublishedError),
    #[error("the durable compiler publisher could not shut down cleanly")]
    Shutdown(#[from] ShutdownError),
    #[error("the durable compiler publisher had no selected package after publication")]
    MissingOpenedPackage,
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "ordinary journey retains exact source-bearing compile, publication, and reopen terminals"
)]
fn distinct_declarations_compile_publish_and_reopen_from_a_stable_receipt()
-> Result<(), TestFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1    ;";
    assert_eq!(alpha_source.len(), bravo_source.len());

    let host = HostTool::resolve(NativeTool::Rustc)?;
    let toolchain = host.toolchain()?;
    let authority_toolchain = RustToolchain::discover(host.executable())?;
    let alpha_authority =
        RustAuthorityFixture::create("alpha", alpha_source, &authority_toolchain)?;
    let bravo_authority =
        RustAuthorityFixture::create("bravo", bravo_source, &authority_toolchain)?;
    let work = NativeWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut alpha_diagnostic = [0; 4_096];
    let mut bravo_diagnostic = [0; 4_096];
    let mut alpha_output = [0; 4_096];
    let mut bravo_output = [0; 4_096];
    let alpha = compile_semantic(
        request(
            alpha_source,
            &alpha_authority.project,
            toolchain,
            &cancelled,
        ),
        CompileScratch {
            diagnostic_output: &mut alpha_diagnostic,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut alpha_output,
        },
    )
    .unwrap_or_else(|source| panic!("alpha compile failed with exact terminal: {source:#?}"));
    work.assert_empty()?;
    let bravo = compile_semantic(
        request(
            bravo_source,
            &bravo_authority.project,
            toolchain,
            &cancelled,
        ),
        CompileScratch {
            diagnostic_output: &mut bravo_diagnostic,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut bravo_output,
        },
    )
    .unwrap_or_else(|source| panic!("bravo compile failed with exact terminal: {source:#?}"));
    work.assert_empty()?;

    assert!(matches!(
        alpha.ir.image_provenance(),
        ImageProvenance::Captured { source, .. } if source == alpha.artifact.source
    ));
    assert!(matches!(
        bravo.ir.image_provenance(),
        ImageProvenance::Captured { source, .. } if source == bravo.artifact.source
    ));
    let alpha = alpha.artifact;
    let bravo = bravo.artifact;
    assert_ne!(alpha.source.identity, bravo.source.identity);
    assert_ne!(alpha.fragment.as_ref(), bravo.fragment.as_ref());
    assert_fragment(&alpha, b"alpha", PrimitiveType::Bool);
    assert_fragment(&bravo, b"bravo", PrimitiveType::I32);
    let directory = JourneyDirectory::create()?;
    let paths = PublicationPaths::in_directory(&directory.path().join("durable"));
    let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?;
    let journal_owner = DurablePublisher::create(&paths, limits)?;
    let artifacts = directory.path().join("artifacts");
    let inputs = [alpha, bravo];
    let selected = publish(&journal_owner, &artifacts, &inputs)?;
    assert_eq!(selected.publication.generation, selected.binding.generation);
    assert_eq!(selected.manifest.fragment_count, 2);
    assert_opened(&journal_owner, &artifacts, &selected)?;
    journal_owner.shutdown()?;

    let reopened = DurablePublisher::reopen(&paths, limits)?;
    assert_opened(&reopened, &artifacts, &selected)?;
    reopened.shutdown()?;
    Ok(())
}

#[allow(
    clippy::result_large_err,
    reason = "fixture forwards the exact public publication terminal without allocation or erasure"
)]
fn publish(
    journal_owner: &DurablePublisher,
    artifacts: &Path,
    inputs: &[CompiledFragment<'_>],
) -> Result<PublishedCompilation, PublishCompiledError> {
    let mut manifest_output = [0; 1_024];
    let mut manifest_facts = [None; 2];
    let mut ordinals = [0; 2];
    let mut locality_output = [0; 256];
    let mut binding_output = [0; COMPILATION_BINDING_BYTES];
    publish_compiled(
        journal_owner,
        artifacts,
        inputs,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality_output,
            binding_output: &mut binding_output,
        },
    )
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact public reopen facts and source-bearing failures"
)]
fn assert_opened(
    journal_owner: &DurablePublisher,
    artifacts: &Path,
    selected: &PublishedCompilation,
) -> Result<(), TestFailure> {
    let mut manifest_output = [0; 1_024];
    let mut manifest_facts = [None; 2];
    let mut fragment_output = [0; 4_096];
    let mut locality_output = [0; 256];
    let opened = open_published(
        journal_owner,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut locality_output,
        },
    )?;
    let Some(opened) = opened else {
        return Err(TestFailure::MissingOpenedPackage);
    };
    assert_eq!(opened.publication, selected.publication);
    assert_eq!(opened.binding, selected.binding);
    assert_eq!(*opened.manifest, selected.manifest);
    assert_eq!(opened.manifest.fragments().count(), 2);
    Ok(())
}

fn assert_fragment(
    compiled: &CompiledFragment<'_>,
    expected_name: &[u8],
    expected_type: PrimitiveType,
) {
    assert!(
        compiled
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(EntityKind::Constant, 0)])
    );
    assert!(
        compiled
            .fragment
            .atoms()
            .map(|atom| atom.bytes)
            .eq([expected_name])
    );
    assert!(
        compiled
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(expected_type)])
    );
}

fn request<'source, 'toolchain, 'cancel>(
    source: &'source [u8],
    project: &'source RustProject,
    toolchain: ResolvedToolchain<'toolchain>,
    cancelled: &'cancel AtomicBool,
) -> CompileRequest<'source, 'toolchain, 'cancel> {
    CompileRequest {
        profile: LanguageProfile::Rust(RustEdition::Rust2024),
        stage: Stage::LowerIr,
        source,
        declaration_scope: compiler_driver::DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(toolchain),
        authority: SemanticAuthorityInput::Rust {
            project,
            maximum_source_bytes: SourceByteLimit::from(65_536),
            features: RustFeatureControl::default(),
        },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(120),
            cancelled,
        },
    }
}

struct RustAuthorityFixture {
    root: PathBuf,
    project: RustProject,
}

impl RustAuthorityFixture {
    fn create(name: &str, source: &[u8], toolchain: &RustToolchain) -> Result<Self, TestFailure> {
        let sequence = JOURNEY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-flow-operation-rust-authority-{}-{sequence}",
            std::process::id()
        ));
        let source_directory = root.join("src");
        fs::create_dir(&root).map_err(|source| TestFailure::CreateAuthority {
            path: root.clone(),
            source,
        })?;
        fs::create_dir(&source_directory).map_err(|source| TestFailure::CreateAuthority {
            path: source_directory.clone(),
            source,
        })?;
        let manifest_path = root.join("Cargo.toml");
        let manifest = format!(
            "[package]\nname = \"publication-{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"
        );
        fs::write(&manifest_path, manifest).map_err(|source| TestFailure::WriteAuthority {
            path: manifest_path,
            source,
        })?;
        let source_path = source_directory.join("lib.rs");
        fs::write(&source_path, source).map_err(|source| TestFailure::WriteAuthority {
            path: source_path.clone(),
            source,
        })?;
        let project =
            RustProject::open_with_source(&root, &source_path, toolchain, RustEdition::Rust2024)?;
        Ok(Self { root, project })
    }
}

impl Drop for RustAuthorityFixture {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.root);
    }
}

struct JourneyDirectory {
    path: PathBuf,
}

impl JourneyDirectory {
    #[allow(
        clippy::result_large_err,
        reason = "fixture construction participates in the exact typed end-to-end test terminal"
    )]
    fn create() -> Result<Self, TestFailure> {
        let sequence = JOURNEY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-flow-operation-publication-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(TestFailure::CreateJourney)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for JourneyDirectory {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.path);
    }
}
