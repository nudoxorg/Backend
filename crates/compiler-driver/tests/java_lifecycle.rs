#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! T1 stages: PURL -> repository -> JAR -> safe extraction -> JDK harness image ->
//! driver compile -> fragment validation -> publication -> journal reopen -> index build ->
//! sealed index plan -> encoding; then Continue publication -> newest reopen -> old artifact reopen.
#[path = "java_lifecycle/mod.rs"]
mod support;
use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{EntityKind, ForeignOrigin, FragmentView, ItemKind, OccurrenceTarget};
use compiler_languages_java::{
    JavaRelease as HarnessRelease,
    central::{Central, FetchError},
    harness::{Harness, HarnessError, HarnessRequest, JavaSource, JdkToolchain},
    jar::Jar,
    purl::MavenCoordinates,
    repo::Repository,
};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::manifest::StoredFragmentFacts;
use compiler_publication::manifest_store::ImmutableManifestStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, NativeTool, Stage};
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use support::{CENTRAL_NAMES, Error as FixtureError, FIRST, TempDir, jar, write_file};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("filesystem operation failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("repository lookup failed: {0}")]
    Repo(String),
    #[error("archive parse failed: {0}")]
    Jar(String),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("fragment validation failed: {0}")]
    Fragment(String),
    #[error("publication failed: {0}")]
    Publish(String),
    #[error("index failed: {0}")]
    Index(String),
    #[error("assertion failed: {0}")]
    Fact(&'static str),
}
fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}
fn limits() -> Result<PublicationLimits, TestError> {
    PublicationLimits::new(
        std::num::NonZeroUsize::new(16).ok_or(TestError::Fact("invalid queue limit"))?,
        std::num::NonZeroUsize::new(16).ok_or(TestError::Fact("invalid group limit"))?,
    )
    .map_err(|e| TestError::Index(e.to_string()))
}

fn image(
    sources: &[JavaSource<'_>],
    classpath: &[&Path],
    output: &mut Vec<u8>,
) -> Result<(), TestError> {
    let tool = JdkToolchain::from_env()?;
    let mut harness = Harness::new()?;
    harness.prepare(&tool)?;
    harness.image(
        &tool,
        HarnessRequest {
            sources,
            classpath,
            release: HarnessRelease::Java21,
        },
        output,
    )?;
    if let Some(error) = harness.take_cleanup_error() {
        return Err(TestError::Io { source: error });
    }
    Ok(())
}
fn compile_fragment<'a>(
    source: &'a [u8],
    image: &'a [u8],
    output: &'a mut [u8],
    work: &Path,
) -> Result<compiler_driver::CompiledFragment<'a>, TestError> {
    let tool = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/usr/bin/true"),
        b"fixture",
    )
    .map_err(|e| TestError::Compile(e.to_string()))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 64 * 1024];
    compile(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::Java { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work,
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .map_err(|e| TestError::Compile(e.to_string()))
}

fn compile_many_into<'a>(
    names: &[&Path],
    sources: &'a [Vec<u8>],
    images: &'a [Vec<u8>],
    outputs: &'a mut [Vec<u8>],
    work: &Path,
    fragments: &mut Vec<compiler_driver::CompiledFragment<'a>>,
) -> Result<(), TestError> {
    if let Some((output, rest)) = outputs.split_first_mut() {
        let name = names
            .first()
            .ok_or(TestError::Fact("central name missing during compile"))?;
        let source = sources
            .first()
            .ok_or(TestError::Fact("central source missing during compile"))?;
        let image = images
            .first()
            .ok_or(TestError::Fact("central image missing during compile"))?;
        let fragment = compile_fragment(source, image, output.as_mut_slice(), work)
            .map_err(|error| TestError::Compile(format!("{}: {error}", name.display())))?;
        fragments.push(fragment);
        compile_many_into(
            &names[1..],
            &sources[1..],
            &images[1..],
            rest,
            work,
            fragments,
        )
    } else if names.is_empty() && sources.is_empty() && images.is_empty() {
        Ok(())
    } else {
        Err(TestError::Fact("central compile output count diverged"))
    }
}
fn publish<'a>(
    journal: &'a DurablePublisher,
    artifacts: &Path,
    fragment: &'a compiler_driver::CompiledFragment<'a>,
    control: PublishControl,
    buffers: &'a mut Buffers,
) -> Result<compiler_publication::PublishedCompilation, TestError> {
    publish_compiled(
        journal,
        artifacts,
        std::slice::from_ref(fragment),
        control,
        PublicationScratch {
            manifest_output: &mut buffers.manifest,
            manifest_facts: &mut buffers.facts,
            ordinals: &mut buffers.ordinals,
            locality_output: &mut buffers.locality,
            binding_output: &mut buffers.binding,
        },
    )
    .map_err(|e| TestError::Publish(format!("{e:?}")))
}

fn publish_many<'a>(
    journal: &'a DurablePublisher,
    artifacts: &Path,
    fragments: &'a [compiler_driver::CompiledFragment<'a>],
    buffers: &'a mut ManyBuffers,
) -> Result<compiler_publication::PublishedCompilation, TestError> {
    publish_compiled(
        journal,
        artifacts,
        fragments,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut buffers.manifest,
            manifest_facts: &mut buffers.facts,
            ordinals: &mut buffers.ordinals,
            locality_output: &mut buffers.locality,
            binding_output: &mut buffers.binding,
        },
    )
    .map_err(|e| TestError::Publish(format!("{e:?}")))
}
struct Buffers {
    manifest: Vec<u8>,
    facts: [Option<StoredFragmentFacts>; 1],
    ordinals: [usize; 1],
    locality: Vec<u8>,
    binding: Vec<u8>,
}

struct ManyBuffers {
    manifest: Vec<u8>,
    facts: [Option<StoredFragmentFacts>; 5],
    ordinals: [usize; 5],
    locality: Vec<u8>,
    binding: Vec<u8>,
}
impl ManyBuffers {
    fn new() -> Self {
        Self {
            manifest: vec![0; 1 << 20],
            facts: [None; 5],
            ordinals: [0; 5],
            locality: vec![0; 1 << 16],
            binding: vec![0; 7 * 128],
        }
    }
}
impl Buffers {
    fn new() -> Self {
        Self {
            manifest: vec![0; 1 << 20],
            facts: [None],
            ordinals: [0],
            locality: vec![0; 1 << 16],
            binding: vec![0; 128],
        }
    }
}

fn open_one<'a>(
    journal: &'a DurablePublisher,
    artifacts: &'a Path,
    buffers: &'a mut OpenBuffers,
) -> Result<compiler_publication::OpenedCompilation<'a, 'a>, TestError> {
    open_published(
        journal,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut buffers.manifest,
            manifest_facts: &mut buffers.facts,
            fragment_output: &mut buffers.fragment,
            locality_output: &mut buffers.locality,
        },
    )
    .map_err(|e| TestError::Publish(e.to_string()))?
    .ok_or(TestError::Fact("publication had no compilation"))
}
struct OpenBuffers {
    manifest: Vec<u8>,
    facts: [Option<StoredFragmentFacts>; 5],
    fragment: Vec<u8>,
    locality: Vec<u8>,
}
impl OpenBuffers {
    fn new() -> Self {
        Self {
            manifest: vec![0; 1 << 20],
            facts: [None; 5],
            fragment: vec![0; 16 << 20],
            locality: vec![0; 1 << 16],
        }
    }
}

fn index(
    compilation: compiler_publication::OpenedCompilation<'_, '_>,
    wanted: &[u8],
) -> Result<(), TestError> {
    const MAX_INDEX_FRAGMENTS: usize = 5;
    let mut scratch: Vec<_> = (0..MAX_INDEX_FRAGMENTS)
        .map(|_| IndexScratch::new())
        .collect();
    let mut prepared = Vec::with_capacity(MAX_INDEX_FRAGMENTS);
    for (slots, fragment) in scratch.iter_mut().zip(compilation.fragments()) {
        let fragment = fragment.map_err(|e| TestError::Index(e.to_string()))?;
        let built = build(
            &fragment,
            IndexBuildScratch {
                projections: &mut slots.projections,
                entities: &mut slots.entities,
                exact_rows: &mut slots.exact,
                lexical_rows: &mut slots.lexical,
                atoms: &mut slots.atoms,
                type_nodes: &mut slots.types,
            },
        )
        .map_err(|e| TestError::Index(e.to_string()))?;
        prepared.push(built);
    }
    if prepared.is_empty() {
        return Err(TestError::Fact("published fragment missing"));
    }
    if !prepared
        .iter()
        .flat_map(|fragment| fragment.entities.iter())
        .any(|entity| entity.name == wanted)
    {
        return Err(TestError::Fact(
            "required name absent from built index rows",
        ));
    }
    let mut exact_ids = [prepared[0].exact.id; MAX_INDEX_FRAGMENTS];
    let mut lexical_ids = [prepared[0].lexical.id; MAX_INDEX_FRAGMENTS];
    let sealed = seal_compilation_index(
        compilation,
        &prepared,
        CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|e| TestError::Index(e.error.to_string()))?;
    let plan = plan_index_pack(&sealed).map_err(|e| TestError::Index(e.to_string()))?;
    let mut encoded = vec![0; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded).map_err(|e| TestError::Index(e.to_string()))?;
    Ok(())
}

struct IndexScratch<'bytes> {
    projections: [MaybeUninit<server_index_build::EntityProjection<'bytes>>; 512],
    entities: [MaybeUninit<server_index_build::EntityFact<'bytes>>; 512],
    exact: [MaybeUninit<server_index_core::ExactRow<'bytes>>; 512],
    lexical: [MaybeUninit<server_index_core::LexicalRow<'bytes>>; 512],
    atoms: [MaybeUninit<compiler_ir::Atom<'bytes>>; 512],
    types: [MaybeUninit<compiler_ir::TypeNode>; 512],
}

impl<'bytes> IndexScratch<'bytes> {
    fn new() -> Self {
        Self {
            projections: [MaybeUninit::uninit(); 512],
            entities: [MaybeUninit::uninit(); 512],
            exact: [MaybeUninit::uninit(); 512],
            lexical: [MaybeUninit::uninit(); 512],
            atoms: [MaybeUninit::uninit(); 512],
            types: [MaybeUninit::uninit(); 512],
        }
    }
}
fn entities(view: &FragmentView<'_>, wanted: &[&[u8]]) -> Result<(), TestError> {
    for name in wanted {
        let found = view.entities().any(|e| {
            view.atoms()
                .nth(e.name.raw as usize)
                .is_some_and(|a| a.bytes == *name)
        });
        if !found {
            return Err(TestError::Fact("required Java entity missing"));
        }
    }
    Ok(())
}

#[test]
fn offline_local_repo_purl_to_published_index() -> Result<(), TestError> {
    let temp = TempDir::new("local")?;
    let coords = MavenCoordinates::parse("pkg:maven/demo/lifecycle@1.0.0")
        .map_err(|e| TestError::Repo(e.to_string()))?;
    let dir = temp.path.join("demo/lifecycle/1.0.0");
    fs::create_dir_all(&dir).map_err(io)?;
    let entries: Vec<_> = FIRST
        .iter()
        .enumerate()
        .map(|(i, (n, b))| (*n, *b, i == 1))
        .collect();
    let sources = jar(&entries)?;
    let source_path = dir.join("lifecycle-1.0.0-sources.jar");
    write_file(&source_path, &sources)?;
    let binary_path = dir.join("lifecycle-1.0.0.jar");
    write_file(&binary_path, b"binary")?;
    let repo = Repository::new(&temp.path);
    if !repo
        .jar(&coords)
        .map_err(|e| TestError::Repo(e.to_string()))?
        .is_file()
        || !repo
            .sources_jar(&coords)
            .map_err(|e| TestError::Repo(e.to_string()))?
            .is_file()
    {
        return Err(TestError::Fact("repository paths did not resolve"));
    }
    let bytes = fs::read(&source_path).map_err(io)?;
    let parsed = Jar::parse(&bytes).map_err(|e| TestError::Jar(e.to_string()))?;
    let mut extracted = Vec::new();
    let mut scratch = Vec::new();
    for entry in parsed.entries() {
        let entry = entry.map_err(|e| TestError::Jar(e.to_string()))?;
        if entry.name().ends_with(b".java") {
            if !entry.is_safe_relative_path() {
                return Err(TestError::Fact("unsafe Java path admitted"));
            }
            let data = entry
                .data(&mut scratch)
                .map_err(|e| TestError::Jar(e.to_string()))?;
            extracted.push((
                String::from_utf8_lossy(entry.name()).into_owned(),
                data.to_vec(),
            ));
        }
    }
    if extracted.len() != 2 {
        return Err(TestError::Fact("fixture did not extract two Java sources"));
    }
    let owned: Vec<_> = extracted
        .iter()
        .map(|(name, bytes)| JavaSource {
            name: Path::new(name),
            bytes,
        })
        .collect();
    let mut image_bytes = Vec::new();
    image(&owned, &[], &mut image_bytes)?;
    if image_bytes.len() > 16 << 20 {
        return Err(TestError::Fact("image exceeded 16 MiB"));
    }
    let mut fragment_bytes = vec![0; 16 << 20];
    let fragment = compile_fragment(
        &owned[0].bytes,
        &image_bytes,
        &mut fragment_bytes,
        &temp.path,
    )?;
    let view = FragmentView::validate(fragment.fragment.as_ref())
        .map_err(|e| TestError::Fragment(e.to_string()))?;
    entities(&view, &[b"Annotated", b"Pair"])?;
    if view.language_extension_payload().is_none() || view.type_facts().is_none() {
        return Err(TestError::Fact("Java extension or type facts absent"));
    }
    let first_bytes = fragment.fragment.as_ref().to_vec();
    let store = temp.path.join("published");
    let journal_path = store.join("journal");
    let artifacts = store.join("artifacts");
    fs::create_dir_all(&store).map_err(io)?;
    let journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits()?)
            .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut buffers = Buffers::new();
    let published = publish(
        &journal,
        &artifacts,
        &fragment,
        PublishControl::Continue,
        &mut buffers,
    )?;
    let first_generation = published.publication.generation;
    journal
        .shutdown()
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let reopened =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits()?)
            .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut open_buffers = OpenBuffers::new();
    let opened = open_one(&reopened, &artifacts, &mut open_buffers)?;
    let reopened_fragment = opened
        .fragments()
        .next()
        .ok_or(TestError::Fact("reopened fragment missing"))?
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let reopened_view = FragmentView::validate(reopened_fragment.view.as_ref())
        .map_err(|e| TestError::Fragment(e.to_string()))?;
    entities(&reopened_view, &[b"Annotated", b"Pair"])?;
    index(opened, b"Annotated")?;
    let first_source = owned
        .first()
        .ok_or(TestError::Fact("first source missing"))?;
    let closing = first_source
        .bytes
        .iter()
        .rposition(|byte| *byte == b'}')
        .ok_or(TestError::Fact("bound source has no class terminator"))?;
    let mut modified = first_source.bytes[..closing].to_vec();
    modified.extend_from_slice(b"  public void added() {}\n}\n");
    let mut combined = vec![JavaSource {
        name: first_source.name,
        bytes: &modified,
    }];
    combined.extend(owned.iter().skip(1).copied());
    let mut image2 = Vec::new();
    image(&combined, &[], &mut image2)?;
    let mut second_bytes = vec![0; 16 << 20];
    let second = compile_fragment(&modified, &image2, &mut second_bytes, &temp.path)?;
    let mut second_buffers = Buffers::new();
    let second_published = publish(
        &reopened,
        &artifacts,
        &second,
        PublishControl::Continue,
        &mut second_buffers,
    )?;
    if second_published.publication.generation == first_generation {
        return Err(TestError::Fact("generation did not advance"));
    }
    reopened
        .shutdown()
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut old_manifest = vec![0; 1 << 20];
    let mut old_facts = [None];
    let old = ImmutableManifestStore::new(&artifacts)
        .map_err(|e| TestError::Publish(e.to_string()))?
        .open(
            published.manifest.identity,
            &mut old_manifest,
            &mut old_facts,
        )
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let fact = old
        .fragments()
        .next()
        .ok_or(TestError::Fact("old manifest fragment missing"))?;
    let mut old_bytes = vec![0; 16 << 20];
    let old_bytes = ImmutableArtifactStore::new(&artifacts)
        .map_err(|e| TestError::Publish(e.to_string()))?
        .open(fact, &mut old_bytes)
        .map_err(|e| TestError::Publish(e.to_string()))?;
    if Sha256::digest(old_bytes.as_ref()) != Sha256::digest(first_bytes.as_slice()) {
        return Err(TestError::Fact("old generation digest changed"));
    }
    FragmentView::validate(old_bytes.as_ref()).map_err(|e| TestError::Fragment(e.to_string()))?;
    temp.remove()?;
    Ok(())
}

#[test]
fn central_commons_lang3_slice_journey() -> Result<(), TestError> {
    let coords = MavenCoordinates::parse("maven:org.apache.commons:commons-lang3@3.14.0")
        .map_err(|e| TestError::Repo(e.to_string()))?;
    let central = Central::default();
    let mut source_bytes = Vec::new();
    central.fetch(&central.sources_jar_url(&coords), &mut source_bytes)?;
    let mut binary = Vec::new();
    central.fetch(&central.jar_url(&coords), &mut binary)?;
    let temp = TempDir::new("central")?;
    let binary_path = temp.path.join("commons-lang3.jar");
    write_file(&binary_path, &binary)?;
    let parsed = Jar::parse(&source_bytes).map_err(|e| TestError::Jar(e.to_string()))?;
    let mut found = Vec::new();
    let mut scratch = Vec::new();
    for entry in parsed.entries() {
        let entry = entry.map_err(|e| TestError::Jar(e.to_string()))?;
        let name = std::str::from_utf8(entry.name())
            .map_err(|_| TestError::Jar("non-UTF8 frozen entry name".to_owned()))?;
        if CENTRAL_NAMES.contains(&name) {
            let data = entry
                .data(&mut scratch)
                .map_err(|e| TestError::Jar(e.to_string()))?;
            found.push((name.to_owned(), data.to_vec()));
        }
    }
    if found.len() != CENTRAL_NAMES.len() {
        return Err(TestError::Fact("frozen commons slice is missing"));
    }
    found.sort_by_key(|(name, _)| CENTRAL_NAMES.iter().position(|wanted| wanted == name));
    let source_bytes: Vec<Vec<u8>> = found.into_iter().map(|(_, bytes)| bytes).collect();
    let names: Vec<&Path> = CENTRAL_NAMES.iter().map(Path::new).collect();
    let mut images = Vec::with_capacity(CENTRAL_NAMES.len());
    for (name, bytes) in names.iter().zip(source_bytes.iter()) {
        let mut image_bytes = Vec::new();
        let one = [JavaSource { name, bytes }];
        image(&one, &[&binary_path], &mut image_bytes)?;
        if image_bytes.len() > 16 << 20 {
            return Err(TestError::Fact("image exceeded 16 MiB"));
        }
        images.push(image_bytes);
    }
    let mut outputs: Vec<Vec<u8>> = (0..CENTRAL_NAMES.len()).map(|_| vec![0; 4 << 20]).collect();
    let mut fragments = Vec::with_capacity(source_bytes.len());
    compile_many_into(
        &names,
        &source_bytes,
        &images,
        &mut outputs,
        &temp.path,
        &mut fragments,
    )?;
    if fragments
        .iter()
        .any(|fragment| fragment.fragment.as_ref().len() > 4 << 20)
    {
        return Err(TestError::Fact("fragment exceeded 4 MiB"));
    }
    for fragment in &fragments {
        FragmentView::validate(fragment.fragment.as_ref())
            .map_err(|e| TestError::Fragment(e.to_string()))?;
    }
    // Overload law through the live Ir terminal: `add(int)` and
    // `add(Number)` must render as two distinct exact signatures.
    {
        let mut ir_diagnostic = [0; 64 * 1024];
        let cancelled = AtomicBool::new(false);
        let built = compile_ir(
            CompileRequest {
                profile: LanguageProfile::Java(JavaRelease::Java21),
                stage: Stage::LowerIr,
                source: &source_bytes[1],
                declaration_scope: compiler_driver::DeclarationScope::fixture(),
                toolchain: ToolchainSelection::ResolvedNative(
                    ResolvedToolchain::from_version(
                        NativeTool::JavaCompiler,
                        Path::new("/usr/bin/true"),
                        b"fixture",
                    )
                    .map_err(|e| TestError::Compile(e.to_string()))?,
                ),
                authority: SemanticAuthorityInput::Java { image: &images[1] },
                control: CompileControl {
                    deadline: Instant::now() + Duration::from_secs(60),
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut ir_diagnostic,
                native_work: &temp.path,
            },
        )
        .map_err(|e| TestError::Compile(e.to_string()))?;
        let mut add_signatures: Vec<String> = built
            .ir
            .items()
            .filter(|item| item.kind() == ItemKind::Function && item.name() == b"add")
            .map(|item| {
                built
                    .ir
                    .signature(item.id())
                    .map(|rendered| rendered.to_string())
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(TestError::Fact("add signature unavailable"))?;
        add_signatures.sort();
        add_signatures.dedup();
        if add_signatures.len() < 2 {
            return Err(TestError::Fact("add overloads rendered identically"));
        }
    }
    let store = temp.path.join("published");
    let journal = DurablePublisher::create(
        &PublicationPaths::in_directory(&store.join("journal")),
        limits()?,
    )
    .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut buffers = ManyBuffers::new();
    let published = publish_many(&journal, &store.join("artifacts"), &fragments, &mut buffers)?;
    journal
        .shutdown()
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let reopened = DurablePublisher::reopen(
        &PublicationPaths::in_directory(&store.join("journal")),
        limits()?,
    )
    .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut open_buffers = OpenBuffers::new();
    let central_artifacts = store.join("artifacts");
    let opened = open_one(&reopened, &central_artifacts, &mut open_buffers)?;
    let mut pair = false;
    let mut mutable_int = false;
    let mut add_shapes: Vec<Vec<(u32, u8)>> = Vec::new();
    let mut maven = false;
    let mut occurrences = false;
    let mut compound = false;
    for fragment in opened.fragments() {
        let fragment = fragment.map_err(|e| TestError::Publish(e.to_string()))?;
        let view = FragmentView::validate(fragment.view.as_ref())
            .map_err(|e| TestError::Fragment(e.to_string()))?;
        occurrences |= view
            .occurrences()
            .is_some_and(|mut rows| rows.next().is_some());
        compound |= view
            .type_facts()
            .is_some_and(|mut rows| rows.next().is_some());
        if let Some(mut rows) = view.occurrences() {
            while let Some(row) = rows.next() {
                let row = row.map_err(|e| TestError::Fragment(e.to_string()))?;
                if let OccurrenceTarget::Foreign(key) = row.occurrence.target
                    && let ForeignOrigin::Namespace { ecosystem, .. } = key.origin
                {
                    maven |= ecosystem == "maven";
                }
            }
        }
        for entity in view.entities() {
            let name = view
                .atoms()
                .nth(entity.name.raw as usize)
                .map(|atom| atom.bytes);
            // Packaged declarations carry their qualified name; the slice's
            // binary jar is on the classpath so foreign nominals stay spelled.
            pair |= name == Some(b"org.apache.commons.lang3.tuple.Pair");
            mutable_int |= name == Some(b"org.apache.commons.lang3.mutable.MutableInt");
        }
    }
    if !pair || !mutable_int {
        return Err(TestError::Fact("commons entities absent"));
    }
    if !maven || !occurrences || !compound {
        eprintln!("DIAG maven={maven} occurrences={occurrences} compound={compound}");
        return Err(TestError::Fact(
            "commons atom, occurrence, or compound row absent",
        ));
    }
    index(opened, b"org.apache.commons.lang3.tuple.Pair")?;
    reopened
        .shutdown()
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let _ = published;
    temp.remove()?;
    Ok(())
}

#[test]
fn central_missing_artifact_is_typed() -> Result<(), TestError> {
    let central = Central::new("https://repo1.maven.org/maven2");
    let coords = MavenCoordinates::parse("maven:demo:missing@9.9.9")
        .map_err(|e| TestError::Repo(e.to_string()))?;
    let url = central.jar_url(&coords);
    let mut output = Vec::new();
    match central.fetch(&url, &mut output) {
        Err(FetchError::NotFound { url: found }) if found == url => Ok(()),
        Err(error) => Err(TestError::Fact(
            if matches!(error, FetchError::Network { .. }) {
                "network failure prevented typed 404"
            } else {
                "missing artifact returned wrong typed error"
            },
        )),
        Ok(()) => Err(TestError::Fact("missing artifact returned Ok")),
    }
}
