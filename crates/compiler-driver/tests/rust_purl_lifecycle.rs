#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use backend_semantic::ir::FragmentView;
use backend_frontend_rust::legacy::{RustPackageUrl, RustPurlError, RustToolchain};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
enum TestError {
    #[error("fixture I/O during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("no rustc was found")]
    MissingRustc,
    #[error("rust toolchain discovery failed: {0}")]
    Toolchain(#[from] backend_frontend_rust::legacy::LoadError),
    #[error("PURL failure: {0}")]
    Purl(String),
    #[error("compile failure: {0}")]
    Compile(String),
    #[error("publication failure: {0}")]
    Publication(String),
    #[error("reopen failure: {0}")]
    Reopen(String),
    #[error("index failure: {0}")]
    Index(String),
    #[error("lifecycle fact failed: {0}")]
    Fact(&'static str),
}

fn io(operation: &'static str, source: std::io::Error) -> TestError {
    TestError::Io { operation, source }
}

fn fresh(label: &str) -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TestError::Fact("clock preceded epoch"))?
        .as_nanos();
    let number = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-purl-{label}-{nonce}-{number}"));
    fs::create_dir_all(&root).map_err(|source| io("create temporary root", source))?;
    Ok(root)
}

fn rustc() -> Result<(PathBuf, RustToolchain, ResolvedToolchain<'static>), TestError> {
    let Some(path) = std::env::var_os("RUSTC").map(PathBuf::from).or_else(|| {
        std::env::var_os("PATH").and_then(|value| {
            std::env::split_paths(&value)
                .map(|directory| directory.join("rustc"))
                .find(|candidate| candidate.is_file())
        })
    }) else {
        return Err(TestError::MissingRustc);
    };
    let path = path
        .canonicalize()
        .map_err(|source| io("canonicalize rustc", source))?;
    let authority = RustToolchain::discover(path.clone())?;
    let leaked = Box::leak(path.clone().into_boxed_path());
    let resolved = ResolvedToolchain::from_version(NativeTool::Rustc, leaked, b"purl-fixture")
        .map_err(|source| TestError::Compile(source.to_string()))?;
    Ok((path, authority, resolved))
}

fn compile_one<'source>(
    source: &'source [u8],
    project: &'source backend_frontend_rust::legacy::RustProject,
    tool: &ResolvedToolchain<'_>,
    output: &'source mut [u8],
) -> Result<compiler_driver::CompiledFragment<'source>, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 16 * 1024];
    compile(
        CompileRequest {
            profile: LanguageProfile::Rust(project.edition),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(*tool),
            authority: SemanticAuthorityInput::Rust {
                project,
                maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit::from(65_536),
                features: backend_frontend_rust::legacy::RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: std::time::Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &project.root,
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .map_err(|failure| TestError::Compile(format!("{failure:?}")))
}

fn compile_ir_one(
    source: &[u8],
    project: &backend_frontend_rust::legacy::RustProject,
    tool: &ResolvedToolchain<'_>,
) -> Result<compiler_driver::CompiledIr, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 16 * 1024];
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::Rust(project.edition),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(*tool),
            authority: SemanticAuthorityInput::Rust {
                project,
                maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit::from(65_536),
                features: backend_frontend_rust::legacy::RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: std::time::Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &project.root,
        },
    )
    .map_err(|failure| TestError::Compile(format!("{failure:?}")))
}

fn limits() -> Result<PublicationLimits, TestError> {
    PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|error| TestError::Publication(error.to_string()))
}

fn publish<'a>(
    journal: &DurablePublisher,
    artifacts: &Path,
    fragment: &'a compiler_driver::CompiledFragment<'a>,
) -> Result<compiler_publication::PublishedCompilation, TestError> {
    let mut manifest = vec![0_u8; 1 << 20];
    let mut facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut locality = vec![0_u8; 1 << 16];
    let mut binding = vec![0_u8; 128];
    publish_compiled(
        journal,
        artifacts,
        std::slice::from_ref(fragment),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|error| TestError::Publication(error.to_string()))
}

fn index_current(
    journal: &DurablePublisher,
    artifacts: &Path,
    wanted: Option<&[u8]>,
) -> Result<
    (
        backend_store::journal::PublicationFacts,
        compiler_publication::manifest::StoredFragmentFacts,
    ),
    TestError,
> {
    let mut manifest = vec![0_u8; 1 << 20];
    let mut facts = [None; 1];
    let mut fragments = vec![0_u8; 1 << 20];
    let mut locality = vec![0_u8; 1 << 16];
    let opened = open_published(
        journal,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )
    .map_err(|error| TestError::Reopen(error.to_string()))?
    .ok_or(TestError::Fact("journal had no published compilation"))?;
    let publication = opened.publication;
    let fragment = opened
        .fragments()
        .next()
        .ok_or(TestError::Fact("publication had no fragment"))?
        .map_err(|error| TestError::Reopen(error.to_string()))?;
    let first_facts = fragment.facts;
    let mut projections = [MaybeUninit::uninit(); 64];
    let mut entities = [MaybeUninit::uninit(); 64];
    let mut exact = [MaybeUninit::uninit(); 256];
    let mut lexical = [MaybeUninit::uninit(); 256];
    let mut atoms = [MaybeUninit::uninit(); 256];
    let mut types = [MaybeUninit::uninit(); 256];
    let prepared = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    )
    .map_err(|error| TestError::Index(error.to_string()))?;
    if let Some(name) = wanted {
        let view = FragmentView::validate(fragment.view.as_ref())
            .map_err(|error| TestError::Reopen(error.to_string()))?;
        let present = view.entities().any(|entity| {
            view.atoms()
                .nth(entity.name.raw as usize)
                .is_some_and(|atom| atom.bytes == name)
        });
        if !present {
            return Err(TestError::Fact(
                "expected declaration was absent from fragment",
            ));
        }
    }
    let mut exact_ids = [prepared.exact.id];
    let mut lexical_ids = [prepared.lexical.id];
    let sealed = seal_compilation_index(
        opened,
        std::slice::from_ref(&prepared),
        CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|error| TestError::Index(error.error.to_string()))?;
    let plan = plan_index_pack(&sealed).map_err(|error| TestError::Index(error.to_string()))?;
    let mut encoded = vec![0_u8; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded).map_err(|error| TestError::Index(error.to_string()))?;
    Ok((publication, first_facts))
}

fn write_workspace(root: &Path, source: &str) -> Result<(), TestError> {
    fs::create_dir_all(root.join("member-a/src"))
        .map_err(|source| io("create member A", source))?;
    fs::create_dir_all(root.join("member-b/src"))
        .map_err(|source| io("create member B", source))?;
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=[\"member-a\",\"member-b\"]\nresolver=\"2\"\n",
    )
    .map_err(|source| io("write workspace manifest", source))?;
    for (name, body) in [("member-a", source), ("member-b", "pub fn other() {}\n")] {
        fs::write(
            root.join(name).join("Cargo.toml"),
            format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"),
        )
        .map_err(|source| io("write member manifest", source))?;
        fs::write(root.join(name).join("src/lib.rs"), body)
            .map_err(|source| io("write member source", source))?;
    }
    Ok(())
}

#[test]
fn rust_workspace_member_lifecycle_chains_two_generations() -> Result<(), TestError> {
    let root = fresh("workspace")?;
    let body = "pub fn first() {}\n";
    write_workspace(&root, body)?;
    let (_rustc, authority_tool, resolved) = rustc()?;
    let purl = RustPackageUrl::parse("cargo:member-a@0.1.0")
        .map_err(|e| TestError::Purl(e.to_string()))?;
    let cancelled = AtomicBool::new(false);
    let located = purl
        .locate(&root, &authority_tool, None, &cancelled)
        .map_err(|e| TestError::Purl(e.to_string()))?;
    if !located.from_workspace() || located.project().edition != RustEdition::Rust2021 {
        return Err(TestError::Fact(
            "workspace member or edition was not retained",
        ));
    }
    let source_path = located.project().source_path.clone();
    let first_source = fs::read(&source_path).map_err(|source| io("read first source", source))?;
    let first_ir = compile_ir_one(&first_source, located.project(), &resolved)?;
    if !first_ir
        .ir
        .entity_columns()
        .names
        .iter()
        .any(|id| first_ir.ir.atom(*id).is_some_and(|atom| atom == b"first"))
    {
        return Err(TestError::Fact("compile_ir omitted first"));
    }
    let mut first_output = vec![0_u8; 1 << 20];
    let first = compile_one(
        &first_source,
        located.project(),
        &resolved,
        &mut first_output,
    )?;
    let first_bytes = first.fragment.as_ref().to_vec();
    FragmentView::validate(&first_bytes).map_err(|error| TestError::Reopen(error.to_string()))?;
    let store = fresh("store")?;
    let artifacts = store.join("artifacts");
    let journal_path = store.join("journal");
    let journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits()?)
            .map_err(|e| TestError::Publication(e.to_string()))?;
    let published = publish(&journal, &artifacts, &first)?;
    let first_generation = published.publication.generation;
    let (_first_publication, first_facts) = index_current(&journal, &artifacts, Some(b"first"))?;
    let second_source = [
        first_source.as_slice(),
        b"\npub fn second_generation() {}\n",
    ]
    .concat();
    fs::write(&source_path, &second_source).map_err(|source| io("edit member source", source))?;
    let located_second = purl
        .locate(&root, &authority_tool, None, &cancelled)
        .map_err(|error| TestError::Purl(error.to_string()))?;
    let second_ir = compile_ir_one(&second_source, located_second.project(), &resolved)?;
    if !second_ir.ir.entity_columns().names.iter().any(|id| {
        second_ir
            .ir
            .atom(*id)
            .is_some_and(|atom| atom == b"second_generation")
    }) {
        return Err(TestError::Fact("second compile_ir omitted added function"));
    }
    let mut second_output = vec![0_u8; 1 << 20];
    let second = compile_one(
        &second_source,
        located_second.project(),
        &resolved,
        &mut second_output,
    )?;
    let second_published = publish(&journal, &artifacts, &second)?;
    let second_generation = second_published.publication.generation;
    if first_generation == second_generation {
        return Err(TestError::Fact("journal did not chain a new generation"));
    }
    if second_generation.pinned_root == first_generation.pinned_root {
        return Err(TestError::Fact("parent linkage did not advance the root"));
    }
    let (_second_publication, _second_facts) =
        index_current(&journal, &artifacts, Some(b"second_generation"))?;
    journal
        .shutdown()
        .map_err(|e| TestError::Publication(e.to_string()))?;
    let mut old_output = vec![0_u8; 1 << 20];
    let old = ImmutableArtifactStore::new(&artifacts)
        .map_err(|e| TestError::Reopen(e.to_string()))?
        .open(first_facts, &mut old_output)
        .map_err(|e| TestError::Reopen(e.to_string()))?;
    if Sha256::digest(old.as_ref()) != Sha256::digest(first_bytes.as_slice()) {
        return Err(TestError::Fact(
            "first fragment changed after second publication",
        ));
    }
    FragmentView::validate(old.as_ref()).map_err(|error| TestError::Reopen(error.to_string()))?;
    fs::remove_dir_all(root).map_err(|source| io("remove workspace", source))?;
    fs::remove_dir_all(store).map_err(|source| io("remove publication store", source))
}

#[test]
fn rust_registry_cache_locate_compiles_end_to_end() -> Result<(), TestError> {
    let workspace = fresh("registry-workspace")?;
    write_workspace(&workspace, "pub fn workspace() {}\n")?;
    let cache = fresh("registry-cache")?;
    let package = cache.join("registry/src/hash-1/cache-fixture-0.2.0");
    fs::create_dir_all(package.join("src"))
        .map_err(|source| io("create registry package", source))?;
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname=\"cache-fixture\"\nversion=\"0.2.0\"\n",
    )
    .map_err(|source| io("write registry manifest", source))?;
    let source = b"pub fn cached_declaration() {}\n";
    fs::write(package.join("src/lib.rs"), source)
        .map_err(|source| io("write registry source", source))?;
    let (_rustc, authority_tool, resolved) = rustc()?;
    let purl = RustPackageUrl::parse("cargo:cache-fixture@0.2.0")
        .map_err(|e| TestError::Purl(e.to_string()))?;
    let located = purl
        .locate(
            &workspace,
            &authority_tool,
            Some(&cache.join("registry/src")),
            &AtomicBool::new(false),
        )
        .map_err(|e| TestError::Purl(e.to_string()))?;
    if located.from_workspace() {
        return Err(TestError::Fact(
            "registry package was marked workspace-local",
        ));
    }
    let ir = compile_ir_one(source, located.project(), &resolved)?;
    if !ir.ir.entity_columns().names.iter().any(|id| {
        ir.ir
            .atom(*id)
            .is_some_and(|atom| atom == b"cached_declaration")
    }) {
        return Err(TestError::Fact(
            "registry declaration absent from compile_ir",
        ));
    }
    let mut output = vec![0_u8; 1 << 20];
    let fragment = compile_one(source, located.project(), &resolved, &mut output)?;
    let view = FragmentView::validate(fragment.fragment.as_ref())
        .map_err(|error| TestError::Compile(error.to_string()))?;
    if !view.entities().any(|entity| {
        view.atoms()
            .nth(entity.name.raw as usize)
            .is_some_and(|atom| atom.bytes == b"cached_declaration")
    }) {
        return Err(TestError::Fact("registry declaration absent from fragment"));
    }
    fs::remove_dir_all(workspace).map_err(|source| io("remove registry workspace", source))?;
    fs::remove_dir_all(cache).map_err(|source| io("remove registry cache", source))
}

#[test]
fn rust_purl_version_mismatch_is_typed() -> Result<(), TestError> {
    let root = fresh("mismatch")?;
    write_workspace(&root, "pub fn stable() {}\n")?;
    let (_rustc, authority_tool, _) = rustc()?;
    let input = "cargo:member-a@9.9.9";
    let purl = RustPackageUrl::parse(input).map_err(|e| TestError::Purl(e.to_string()))?;
    match purl.locate(&root, &authority_tool, None, &AtomicBool::new(false)) {
        Err(RustPurlError::VersionMismatch {
            requested_name,
            requested_version,
            observed_versions,
        }) => {
            assert_eq!(requested_name, "member-a");
            assert_eq!(requested_version, "9.9.9");
            assert_eq!(observed_versions, vec!["0.1.0".to_owned()]);
        }
        Err(other) => return Err(TestError::Purl(other.to_string())),
        Ok(_) => return Err(TestError::Fact("wrong package version was accepted")),
    }
    fs::remove_dir_all(root).map_err(|source| io("remove mismatch workspace", source))
}
