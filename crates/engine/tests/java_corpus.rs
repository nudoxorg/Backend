#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Zero-fold corpus stage law (P17): every frozen row passes every stage through
//! public APIs, in frozen table order — `MavenCoordinates::parse` -> canonical
//! Central fetch of the sources jar, binary jar, and every classpath dep jar
//! (typed `FetchError`; rows are never skipped) -> `Jar::parse` -> safe
//! extraction of the frozen entries (`is_safe_relative_path` guard; typed
//! failure naming any missing path) -> per-file JDK harness image ->
//! per-file `Stage::LowerIr` compile at profile Java21 -> `FragmentView::validate`
//! on every fragment -> declared-entity, occurrence, extension-payload, and
//! type-fact laws -> one CORPUS profile line per row. Any typed lowering
//! failure on any frozen file fails the journey naming purl + path + exact
//! failure; no stage may fold, degrade, or be bypassed. Then, once, after all
//! rows, the frozen commons-csv row publishes every fragment (P18):
//! `DurablePublisher::create` -> `publish_compiled` -> shutdown -> `reopen` ->
//! `open_published` with CSV qualified names -> index build/seal/plan/encode
//! with `CSVFormat` in the built rows -> CSVParser bound-source generation-2
//! leg with `PublishControl::Continue` -> newest generation carries the added
//! entity -> first-generation SHA-256 equality through the immutable stores.
#[path = "java_corpus/mod.rs"]
mod support;

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledFragment,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use backend_engine::index_build::{IndexBuildScratch, build};
use backend_engine::index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use backend_engine::publication::immutable::ImmutableArtifactStore;
use backend_engine::publication::manifest::StoredFragmentFacts;
use backend_engine::publication::manifest_store::ImmutableManifestStore;
use backend_engine::publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_frontend_java::legacy::{DeclarationKind, JavaAuthorityImage};
use backend_frontend_java::legacy::{
    JavaRelease as HarnessRelease,
    central::{Central, FetchError},
    harness::{Harness, HarnessError, HarnessRequest, JavaSource, JdkToolchain},
    jar::Jar,
    purl::MavenCoordinates,
};
use backend_semantic::ir::{EntityKind, FragmentView, ItemKind};
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, NativeTool, Stage};
use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use support::{
    ANNOTATIONS_ROW, CENTRAL_HOST, CORPUS, Error as FixtureError, TempDir, qualified_name,
    write_file,
};
use thiserror::Error;

const MAX_IMAGE_BYTES: usize = 16 << 20;
const MAX_FRAGMENT_BYTES: usize = 4 << 20;
/// The frozen commons-csv row freezes exactly two entries, and the publication
/// leg publishes both fragments of that exact compilation.
const COMPILE_DEADLINE: Duration = Duration::from_secs(120);

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
    #[error("authority image failed: {0}")]
    Image(String),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("fragment validation failed: {0}")]
    Fragment(String),
    #[error("publication failed: {0}")]
    Publish(String),
    #[error("index failed: {0}")]
    Index(String),
    #[error("frozen entry {path} missing from the sources jar of {purl}")]
    MissingEntry { purl: &'static str, path: String },
    #[error("corpus law failed for {purl} {path}: {cause}")]
    Law {
        purl: &'static str,
        path: String,
        cause: String,
    },
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

/// One prepared JDK harness reused by every row so the embedded doclet is
/// compiled once for the whole corpus run.
struct Bench {
    tool: JdkToolchain<'static>,
    harness: Harness,
}

impl Bench {
    fn new() -> Result<Self, TestError> {
        let tool = JdkToolchain::from_env()?;
        let mut harness = Harness::new()?;
        harness.prepare(&tool)?;
        Ok(Self { tool, harness })
    }

    fn image(
        &mut self,
        sources: &[JavaSource<'_>],
        classpath: &[&Path],
        output: &mut Vec<u8>,
    ) -> Result<(), TestError> {
        self.harness.image(
            &self.tool,
            HarnessRequest {
                sources,
                classpath,
                release: HarnessRelease::Java21,
            },
            output,
        )?;
        if let Some(error) = self.harness.take_cleanup_error() {
            return Err(TestError::Io { source: error });
        }
        Ok(())
    }
}

/// Strips `//` and `/* */` comments so a keyword search over the remaining
/// bytes never misreads prose (e.g. a javadoc line mentioning "interface")
/// as a declaration. Not a full lexer: string/char literals are not
/// tracked, so a comment-marker byte sequence quoted inside one would still
/// be treated as a comment start. The frozen corpus files this drives never
/// do that inside the file's leading declaration region, so the shortcut is
/// safe for this test-only classification.
fn strip_java_comments(source: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        if source[index] == b'/' && source.get(index + 1) == Some(&b'/') {
            while index < source.len() && source[index] != b'\n' {
                index += 1;
            }
        } else if source[index] == b'/' && source.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < source.len() && !(source[index] == b'*' && source[index + 1] == b'/')
            {
                index += 1;
            }
            index = (index + 2).min(source.len());
        } else {
            out.push(source[index]);
            index += 1;
        }
    }
    out
}

fn compile_fragment<'a>(
    source: &'a [u8],
    image: &'a [u8],
    output: &'a mut [u8],
    work: &Path,
) -> Result<CompiledFragment<'a>, TestError> {
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
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::Java { image },
            control: CompileControl {
                deadline: Instant::now() + COMPILE_DEADLINE,
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

fn entity_present(view: &FragmentView<'_>, wanted: &[u8]) -> bool {
    view.entities().any(|entity| {
        view.atoms()
            .nth(entity.name.raw as usize)
            .is_some_and(|atom| atom.bytes == wanted)
    })
}

fn function_named(view: &FragmentView<'_>, wanted: &[u8]) -> bool {
    view.entities().any(|entity| {
        entity.kind == EntityKind::Function
            && view
                .atoms()
                .nth(entity.name.raw as usize)
                .is_some_and(|atom| atom.bytes == wanted)
    })
}

fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Module => EntityKind::Module,
        DeclarationKind::Package => EntityKind::Namespace,
        DeclarationKind::Class => EntityKind::Record,
        DeclarationKind::Interface | DeclarationKind::Annotation => EntityKind::Trait,
        DeclarationKind::Record => EntityKind::Record,
        DeclarationKind::Enum => EntityKind::Enum,
        DeclarationKind::Field => EntityKind::Field,
        DeclarationKind::EnumConstant => EntityKind::Variant,
        DeclarationKind::Constructor | DeclarationKind::Method => EntityKind::Function,
    }
}

fn deep_review(
    purl: &'static str,
    path: &str,
    source: &[u8],
    image: &[u8],
    view: &FragmentView<'_>,
) -> Result<(usize, usize), TestError> {
    let authority = JavaAuthorityImage::open(image).map_err(|error| TestError::Law {
        purl,
        path: path.to_owned(),
        cause: format!("image decode: {error}"),
    })?;
    let declarations: Vec<_> = authority
        .image
        .declarations()
        .collect::<Result<_, _>>()
        .map_err(|error| TestError::Law {
            purl,
            path: path.to_owned(),
            cause: format!("declaration decode: {error}"),
        })?;
    for (ordinal, declaration) in declarations.iter().enumerate() {
        let expected = declarations
            .iter()
            .filter(|candidate| {
                candidate.name.bytes == declaration.name.bytes
                    && entity_kind(candidate.kind) == entity_kind(declaration.kind)
            })
            .count();
        let found = view
            .entities()
            .filter(|entity| {
                view.atoms()
                    .nth(entity.name.raw as usize)
                    .is_some_and(|atom| atom.bytes == declaration.name.bytes)
                    && entity.kind == entity_kind(declaration.kind)
            })
            .count();
        if found != expected {
            return Err(TestError::Law {
                purl,
                path: path.to_owned(),
                cause: format!(
                    "image declaration {:?} {:?} expected {expected}, admitted {found} entities",
                    declaration.name.bytes, declaration.kind
                ),
            });
        }
        let mut extensions = authority
            .image
            .declaration_extensions(ordinal)
            .map_err(|error| TestError::Law {
                purl,
                path: path.to_owned(),
                cause: format!("extension decode: {error}"),
            })?;
        let mut extension_count = 0;
        while let Some(extension) = extensions.next() {
            extension.map_err(|error| TestError::Law {
                purl,
                path: path.to_owned(),
                cause: format!("extension entry: {error}"),
            })?;
            extension_count += 1;
        }
        // The Java lowerer's `EmissionExtension::Java` plane is fed only by
        // executables (`push_executable`'s `java_facts` call in
        // `driver/lower/java.rs`) — throws, parameter conventions, and
        // record components ride on a method or constructor's own fact. A
        // meta-annotation the image records directly on a type or field
        // declaration (e.g. `@Retention`/`@Target` on a bodyless
        // `@interface` with no members at all, such as
        // `commons-lang3`'s `DiffExclude`) has no executable in the file to
        // carry it and is not lowered into this plane; that is the
        // documented shape, not a dropped fact.
        let is_executable = matches!(
            declaration.kind,
            DeclarationKind::Constructor | DeclarationKind::Method
        );
        if is_executable && extension_count > 0 && view.language_extension_payload().is_none() {
            return Err(TestError::Law {
                purl,
                path: path.to_owned(),
                cause: "image extensions absent from fragment".into(),
            });
        }
    }
    let references = authority.image.references().count() + authority.image.uses().count();
    let mut occurrences = 0;
    if let Some(rows) = view.occurrences() {
        for row in rows {
            let row = row.map_err(|error| TestError::Law {
                purl,
                path: path.to_owned(),
                cause: format!("occurrence decode: {error}"),
            })?;
            if usize::try_from(row.occurrence.span.end)
                .ok()
                .is_none_or(|end| end > source.len())
            {
                return Err(TestError::Law {
                    purl,
                    path: path.to_owned(),
                    cause: format!(
                        "occurrence span ends outside source: {:?}",
                        row.occurrence.span
                    ),
                });
            }
            occurrences += 1;
        }
    }
    if occurrences != references {
        return Err(TestError::Law {
            purl,
            path: path.to_owned(),
            cause: format!("image references {references}, fragment occurrences {occurrences}"),
        });
    }
    Ok((declarations.len(), occurrences))
}

fn render_review(
    purl: &'static str,
    path: &str,
    source: &[u8],
    image: &[u8],
    work: &Path,
) -> Result<(), TestError> {
    let tool = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/usr/bin/true"),
        b"fixture",
    )
    .map_err(|error| TestError::Compile(error.to_string()))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 64 * 1024];
    let built = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::Java { image },
            control: CompileControl {
                deadline: Instant::now() + COMPILE_DEADLINE,
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work,
        },
    )
    .map_err(|error| TestError::Compile(format!("{purl} {path}: {error}")))?;
    // Java lane entities are interned under their qualified names
    // (`org.apache.commons.lang3.AnnotationUtils`), matching the declared
    // qualified-name entity law, so the primary item is looked up by the
    // path-derived qualified name while the rendered check keeps the simple
    // name. A package-info entry's retainable entity is its PACKAGE row (see
    // the identical special case below in the main per-file law): the
    // qualified name is the package name, not a `...package-info` type
    // spelling that no declaration in the image ever carries.
    let qualified = if path.rsplit('/').next() == Some("package-info.java") {
        path.strip_suffix("package-info.java")
            .map(|directory| qualified_name(directory.trim_end_matches('/')))
            .unwrap_or_else(|| qualified_name(path))
    } else {
        qualified_name(path)
    };
    let simple = qualified
        .rsplit('.')
        .next()
        .ok_or_else(|| TestError::Law {
            purl,
            path: path.to_owned(),
            cause: "primary declaration has no simple name".into(),
        })?
        .to_owned();
    let rendered = built
        .ir
        .items()
        .find(|item| item.kind() != ItemKind::Function && item.name() == qualified.as_bytes())
        .and_then(|item| built.ir.signature(item.id()))
        .map(|signature| signature.to_string())
        .ok_or_else(|| TestError::Law {
            purl,
            path: path.to_owned(),
            cause: "primary entity signature was not renderable".into(),
        })?;
    if !rendered.contains(simple.as_str()) {
        return Err(TestError::Law {
            purl,
            path: path.to_owned(),
            cause: format!("rendered signature omitted simple name {simple}"),
        });
    }
    Ok(())
}

struct OpenBuffers {
    manifest: Vec<u8>,
    facts: Vec<Option<StoredFragmentFacts>>,
    fragment: Vec<u8>,
    locality: Vec<u8>,
}
impl OpenBuffers {
    fn new(entry_count: usize) -> Self {
        Self {
            manifest: vec![0; 1 << 20],
            facts: vec![None; entry_count],
            fragment: vec![0; 16 << 20],
            locality: vec![0; 1 << 16],
        }
    }
}

struct PublishBuffers {
    manifest: Vec<u8>,
    facts: Vec<Option<StoredFragmentFacts>>,
    ordinals: Vec<usize>,
    locality: Vec<u8>,
    binding: Vec<u8>,
}
impl PublishBuffers {
    fn new(entry_count: usize) -> Self {
        Self {
            manifest: vec![0; 1 << 20],
            facts: vec![None; entry_count],
            ordinals: vec![0; entry_count],
            locality: vec![0; 1 << 16],
            binding: vec![0; 128 * entry_count],
        }
    }
}

fn open_one<'a>(
    journal: &'a DurablePublisher,
    artifacts: &'a Path,
    buffers: &'a mut OpenBuffers,
) -> Result<backend_engine::publication::OpenedCompilation<'a, 'a>, TestError> {
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

// `whole_artifact` (row 0, commons-lang3) publishes every fragment in the
// package, including files like `StringUtils.java` whose entity/type-child
// count comfortably exceeds the 512-slot capacity this scratch buffer was
// originally sized for when every row selected only a handful of files.
// 4096 is a generous ceiling above the largest observed need (568) rather
// than a tight fit, so a slightly bigger file in a future dependency bump
// does not reopen this same capacity error. The six scratch planes are
// heap-allocated (`Vec`-backed boxed slices), not stack arrays: one
// `IndexScratch` at this capacity is several megabytes, and this test
// constructs one per fragment (up to 246 for the whole-artifact row), which
// overflows the default thread stack if held as `[MaybeUninit<T>; N]`.
const INDEX_SCRATCH_CAPACITY: usize = 4096;

struct IndexScratch<'bytes> {
    projections: Box<[MaybeUninit<backend_engine::index_build::EntityProjection<'bytes>>]>,
    entities: Box<[MaybeUninit<backend_engine::index_build::EntityFact<'bytes>>]>,
    exact: Box<[MaybeUninit<backend_semantic::index_core::ExactRow<'bytes>>]>,
    lexical: Box<[MaybeUninit<backend_semantic::index_core::LexicalRow<'bytes>>]>,
    atoms: Box<[MaybeUninit<backend_semantic::ir::Atom<'bytes>>]>,
    types: Box<[MaybeUninit<backend_semantic::ir::TypeNode>]>,
}

impl IndexScratch<'_> {
    fn new() -> Self {
        fn uninit_slice<T>() -> Box<[MaybeUninit<T>]> {
            (0..INDEX_SCRATCH_CAPACITY)
                .map(|_| MaybeUninit::uninit())
                .collect()
        }
        Self {
            projections: uninit_slice(),
            entities: uninit_slice(),
            exact: uninit_slice(),
            lexical: uninit_slice(),
            atoms: uninit_slice(),
            types: uninit_slice(),
        }
    }
}

fn index(
    compilation: backend_engine::publication::OpenedCompilation<'_, '_>,
    wanted: &[u8],
) -> Result<(), TestError> {
    let fragment_count = compilation.fragments().count();
    let mut scratch: Vec<_> = (0..fragment_count).map(|_| IndexScratch::new()).collect();
    let mut prepared = Vec::with_capacity(fragment_count);
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
    let mut exact_ids = vec![prepared[0].exact.id; prepared.len()];
    let mut lexical_ids = vec![prepared[0].lexical.id; prepared.len()];
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

/// One frozen file lowered through every stage: image -> LowerIr compile ->
/// validate -> declared-entity/occurrence/extension/type-fact laws.
struct FileLaws {
    image_bytes: usize,
    fragment_bytes: usize,
    occurrence_present: bool,
    occurrence_absent: bool,
    fragment: Vec<u8>,
    declarations: usize,
    references: usize,
}

fn lower_frozen_file(
    purl: &'static str,
    path: &str,
    source: &[u8],
    classpath: &[&Path],
    work: &Path,
    bench: &mut Bench,
) -> Result<FileLaws, TestError> {
    let sources = [JavaSource {
        name: Path::new(path),
        bytes: source,
    }];
    let mut image_bytes = Vec::new();
    bench
        .image(&sources, classpath, &mut image_bytes)
        .map_err(|error| TestError::Image(format!("{purl} {path}: {error}")))?;
    if image_bytes.len() > MAX_IMAGE_BYTES {
        return Err(TestError::Law {
            purl,
            path: path.to_owned(),
            cause: "image exceeded 16 MiB".into(),
        });
    }
    let mut fragment_output = vec![0; MAX_FRAGMENT_BYTES];
    let fragment = compile_fragment(source, &image_bytes, &mut fragment_output, work)
        .map_err(|error| TestError::Compile(format!("{purl} {path}: {error}")))?;
    if fragment.fragment.as_ref().len() > MAX_FRAGMENT_BYTES {
        return Err(TestError::Law {
            purl,
            path: path.to_owned(),
            cause: "fragment exceeded 4 MiB".into(),
        });
    }
    let view = FragmentView::validate(fragment.fragment.as_ref())
        .map_err(|error| TestError::Fragment(format!("{purl} {path}: {error}")))?;
    let (declarations, references) = deep_review(purl, path, source, &image_bytes, &view)?;
    render_review(purl, path, source, &image_bytes, work)?;
    // A package-info entry's retainable entity is its PACKAGE row: the
    // entity name is the package name, not the path-derived type spelling.
    let qualified = if path.rsplit('/').next() == Some("package-info.java") {
        path.strip_suffix("package-info.java")
            .map(|directory| qualified_name(directory.trim_end_matches('/')))
            .unwrap_or_else(|| qualified_name(path))
    } else {
        qualified_name(path)
    };
    if !entity_present(&view, qualified.as_bytes()) {
        return Err(TestError::Law {
            purl,
            path: path.to_owned(),
            cause: "declared entity missing by qualified name".into(),
        });
    }
    // An unannotated `package-info.java` declares no type and carries no
    // annotation, so its fragment has no type facts and no extension
    // payload to demand; that is not a lowering gap, it is the file.
    let is_package_info = path.rsplit('/').next() == Some("package-info.java");
    // `EmissionExtension::Java` is fed only by executables (see the matching
    // note in `deep_review`); a file whose only declaration is a bodyless
    // `@interface` (e.g. commons-lang3's marker annotation `DiffExclude`,
    // which carries meta-annotations but declares no member) has no
    // executable to carry one and legitimately has no extension payload.
    let has_executable = view
        .entities()
        .any(|entity| entity.kind == EntityKind::Function);
    if !is_package_info
        && (view.type_facts().is_none()
            || (has_executable && view.language_extension_payload().is_none()))
    {
        return Err(TestError::Law {
            purl,
            path: path.to_owned(),
            cause: "Java extension payload or type facts absent".into(),
        });
    }
    let occurrence_present = view
        .occurrences()
        .is_some_and(|mut rows| rows.next().is_some());
    let occurrence_absent = view.occurrences().is_none();
    Ok(FileLaws {
        image_bytes: image_bytes.len(),
        fragment_bytes: fragment.fragment.as_ref().len(),
        occurrence_present,
        occurrence_absent,
        fragment: fragment.fragment.as_ref().to_vec(),
        declarations,
        references,
    })
}

fn run_row(row_index: usize, central: &Central, bench: &mut Bench) -> Result<(), TestError> {
    let temp = TempDir::new(&format!("row-{}", row_index + 1))?;
    let outcome = journey_row(row_index, central, bench, &temp);
    let cleanup = temp.remove();
    outcome?;
    cleanup?;
    Ok(())
}

fn journey_row(
    row_index: usize,
    central: &Central,
    bench: &mut Bench,
    temp: &TempDir,
) -> Result<(), TestError> {
    let row = &CORPUS[row_index];
    let started = Instant::now();
    let coords = MavenCoordinates::parse(row.purl)
        .map_err(|error| TestError::Repo(format!("{}: {error}", row.purl)))?;
    let mut fetched = Vec::new();
    central.fetch(&central.sources_jar_url(&coords), &mut fetched)?;
    let source_bytes = std::mem::take(&mut fetched);
    central.fetch(&central.jar_url(&coords), &mut fetched)?;
    let binary_path = temp
        .path
        .join(format!("{}-{}.jar", coords.artifact, coords.version));
    write_file(&binary_path, &fetched)?;
    let mut dep_paths = Vec::with_capacity(row.deps.len());
    for dep in row.deps {
        let dep_coords = MavenCoordinates::parse(dep)
            .map_err(|error| TestError::Repo(format!("{dep}: {error}")))?;
        central.fetch(&central.jar_url(&dep_coords), &mut fetched)?;
        let dep_path = temp.path.join(format!(
            "{}-{}.jar",
            dep_coords.artifact, dep_coords.version
        ));
        write_file(&dep_path, &fetched)?;
        dep_paths.push(dep_path);
    }
    let mut classpath: Vec<&Path> = vec![binary_path.as_path()];
    classpath.extend(dep_paths.iter().map(Path::new));

    let parsed = Jar::parse(&source_bytes)
        .map_err(|error| TestError::Jar(format!("{}: {error}", row.purl)))?;
    let mut found: Vec<(String, Vec<u8>)> = Vec::new();
    let whole_artifact = row_index == 0;
    let mut scratch = Vec::new();
    for entry in parsed.entries() {
        let entry = entry.map_err(|error| TestError::Jar(format!("{}: {error}", row.purl)))?;
        let name = std::str::from_utf8(entry.name())
            .map_err(|_| TestError::Jar(format!("{}: non-UTF8 entry name", row.purl)))?;
        if (whole_artifact && name.ends_with(".java")) || row.entries.contains(&name) {
            if !entry.is_safe_relative_path() {
                return Err(TestError::Jar(format!(
                    "{}: unsafe frozen entry path {name}",
                    row.purl
                )));
            }
            let data = entry
                .data(&mut scratch)
                .map_err(|error| TestError::Jar(format!("{name}: {error}")))?;
            found.push((name.to_owned(), data.to_vec()));
        }
    }
    let mut extracted = if whole_artifact {
        found
    } else {
        let mut selected = Vec::with_capacity(row.entries.len());
        for wanted in row.entries {
            let at = found.iter().position(|(name, _)| name == wanted).ok_or(
                TestError::MissingEntry {
                    purl: row.purl,
                    path: (*wanted).to_owned(),
                },
            )?;
            selected.push(found.remove(at));
        }
        selected
    };
    if extracted.is_empty() {
        return Err(TestError::Law {
            purl: row.purl,
            path: "<sources.jar>".into(),
            cause: "artifact contained no Java entries".into(),
        });
    }
    if whole_artifact {
        extracted.sort_by(|left, right| left.0.cmp(&right.0));
    }

    let mut total_image_bytes = 0;
    let mut total_fragment_bytes = 0;
    let mut occurrence_present = false;
    let mut fragments = Vec::with_capacity(extracted.len());
    for (name, bytes) in &extracted {
        let laws = lower_frozen_file(row.purl, name, bytes, &classpath, &temp.path, bench)?;
        total_image_bytes += laws.image_bytes;
        total_fragment_bytes += laws.fragment_bytes;
        occurrence_present |= laws.occurrence_present;
        println!(
            "CORPUS|{}|{}|{}|{}|{}|{}|{}",
            row.purl,
            name,
            laws.image_bytes,
            laws.fragment_bytes,
            laws.declarations,
            laws.references,
            started.elapsed().as_millis()
        );
        fragments.push(laws.fragment);
    }
    if !occurrence_present {
        return Err(TestError::Law {
            purl: row.purl,
            path: extracted[0].0.clone(),
            cause: "row carried no occurrence row".into(),
        });
    }
    if row_index == ANNOTATIONS_ROW {
        // NotNull and its neighboring annotations use @Target, @Retention,
        // and other meta-annotations: those are real typed references. Keep
        // the negative-plane proof, but ground it in a genuinely reference-
        // free package declaration instead of pretending the corpus row is
        // empty. This fixture runs through the same javac and lowerer path.
        let empty_package = lower_frozen_file(
            "fixture:empty-package",
            "fixture/package-info.java",
            b"package fixture;\n",
            &[],
            &temp.path,
            bench,
        )?;
        if !empty_package.occurrence_absent || empty_package.occurrence_present {
            return Err(TestError::Law {
                purl: row.purl,
                path: "fixture/package-info.java".into(),
                cause: "reference-free package did not assert typed occurrence absence".into(),
            });
        }
    }
    println!(
        "CORPUS|{}|{}|{}|{}|{}|{}|{}",
        row.purl,
        extracted.len(),
        total_image_bytes,
        total_fragment_bytes,
        started.elapsed().as_millis(),
        fragments
            .iter()
            .map(|bytes| FragmentView::validate(bytes)
                .map(|view| view.entities().count())
                .unwrap_or(0))
            .sum::<usize>(),
        fragments
            .iter()
            .map(|bytes| FragmentView::validate(bytes)
                .ok()
                .and_then(|view| view.occurrences().map(|rows| rows.count()))
                .unwrap_or(0))
            .sum::<usize>()
    );
    publication_leg(row, temp, &extracted, &classpath, bench)?;
    Ok(())
}

/// Publish every fragment, reopen, index, then append a generation-2 method to
/// the row's final source with first-generation digest equality.
fn publication_leg(
    row: &support::CorpusRow,
    temp: &TempDir,
    extracted: &[(String, Vec<u8>)],
    classpath: &[&Path],
    bench: &mut Bench,
) -> Result<(), TestError> {
    let mut images: Vec<Vec<u8>> = Vec::with_capacity(extracted.len());
    for (position, (_, bytes)) in extracted.iter().enumerate() {
        let sources = [JavaSource {
            name: Path::new(extracted[position].0.as_str()),
            bytes,
        }];
        let mut image_bytes = Vec::new();
        bench
            .image(&sources, classpath, &mut image_bytes)
            .map_err(|error| {
                TestError::Image(format!("{} {}: {error}", row.purl, extracted[position].0))
            })?;
        if image_bytes.len() > MAX_IMAGE_BYTES {
            return Err(TestError::Fact("image exceeded 16 MiB"));
        }
        images.push(image_bytes);
    }
    let mut outputs: Vec<Vec<u8>> = (0..extracted.len())
        .map(|_| vec![0; MAX_FRAGMENT_BYTES])
        .collect();
    let mut compiled = Vec::with_capacity(extracted.len());
    for ((name, source), (image, output)) in
        extracted.iter().zip(images.iter().zip(outputs.iter_mut()))
    {
        compiled.push(
            compile_fragment(source, image, output, &temp.path)
                .map_err(|error| TestError::Compile(format!("{} {name}: {error}", row.purl)))?,
        );
    }
    let first_bytes: Vec<Vec<u8>> = compiled
        .iter()
        .map(|fragment| fragment.fragment.as_ref().to_vec())
        .collect();

    let store = temp.path.join("published");
    fs::create_dir_all(&store).map_err(io)?;
    let journal_path = store.join("journal");
    let artifacts = store.join("artifacts");
    let journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits()?)
            .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut publish_buffers = PublishBuffers::new(compiled.len());
    let published = publish_compiled(
        &journal,
        &artifacts,
        &compiled,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut publish_buffers.manifest,
            manifest_facts: &mut publish_buffers.facts,
            ordinals: &mut publish_buffers.ordinals,
            locality_output: &mut publish_buffers.locality,
            binding_output: &mut publish_buffers.binding,
        },
    )
    .map_err(|e| TestError::Publish(format!("{e:?}")))?;
    let first_generation = published.publication.generation;
    let first_identity = published.manifest.identity;
    journal
        .shutdown()
        .map_err(|e| TestError::Publish(e.to_string()))?;
    let reopened =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits()?)
            .map_err(|e| TestError::Publish(e.to_string()))?;
    let mut open_buffers = OpenBuffers::new(compiled.len());
    let opened = open_one(&reopened, &artifacts, &mut open_buffers)?;
    let mut format_present = false;
    let mut parser_present = false;
    for fragment in opened.fragments() {
        let fragment = fragment.map_err(|e| TestError::Publish(e.to_string()))?;
        let view = FragmentView::validate(fragment.view.as_ref())
            .map_err(|e| TestError::Fragment(e.to_string()))?;
        format_present |= entity_present(&view, qualified_name(&extracted[0].0).as_bytes());
        parser_present |= entity_present(
            &view,
            qualified_name(&extracted[1.min(extracted.len() - 1)].0).as_bytes(),
        );
    }
    if !format_present || !parser_present {
        return Err(TestError::Fact(
            "reopened fragments missing the CSV qualified names",
        ));
    }
    index(opened, qualified_name(&extracted[0].0).as_bytes())?;

    // `package-info.java` declares no type and has no closing brace to
    // append a generation-2 member into (the plain last-file choice worked
    // for the frozen small rows, but the whole-artifact row 0 sorts every
    // real `.java` file alphabetically, and `package-info.java` can and
    // does land last within a subpackage, e.g. commons-lang3's `util/`).
    let last = extracted
        .iter()
        .rposition(|(name, _)| !name.ends_with("package-info.java"))
        .ok_or(TestError::Fact("row has no generation-2 eligible file"))?;
    let parser_source = &extracted[last].1;
    let closing = parser_source
        .iter()
        .rposition(|byte| *byte == b'}')
        .ok_or(TestError::Fact("generation-2 target has no closing brace"))?;
    let mut modified = parser_source[..closing].to_vec();
    // The declared top-level kind decides the only legal shape for the
    // added member. Comments are stripped first so a stray "interface" in a
    // javadoc line can't misclassify a class or enum, and the search is
    // further limited to the header text before the outermost declaration
    // body's `{`: a top-level class can and does declare its own nested
    // `interface` (or `@interface`) in its body, which is irrelevant to the
    // top-level kind that governs a member appended just inside the file's
    // last `}`. The outermost `{` is the first one at paren-depth zero, not
    // simply the first `{` in the file: a leading annotation's array
    // argument (e.g. `@Target({ ElementType.METHOD })`) opens a `{` of its
    // own, nested inside that annotation's `(...)`, before the real
    // declaration is reached.
    let uncommented = strip_java_comments(parser_source);
    let mut header_end = uncommented.len();
    let mut paren_depth = 0_i32;
    for (offset, byte) in uncommented.iter().enumerate() {
        match byte {
            b'(' => paren_depth += 1,
            b')' => paren_depth -= 1,
            b'{' if paren_depth == 0 => {
                header_end = offset;
                break;
            }
            _ => {}
        }
    }
    let header = &uncommented[..header_end];
    let is_annotation = header.windows(10).any(|window| window == b"@interface");
    let is_interface = !is_annotation && header.windows(10).any(|window| window == b" interface");
    if is_annotation {
        // Annotation-type members are abstract (a body would be a javac
        // error) and must return one of the closed annotation-element
        // types — `void` is not among them ("invalid type for annotation
        // interface element"), so a defaulted `int` return keeps the added
        // element legal even when the target `@interface` (e.g. JUnit's
        // empty `BeforeEach`) declares no other elements to borrow a valid
        // type from.
        modified.extend_from_slice(b"  int added() default 0;\n}\n");
    } else if is_interface {
        // A plain interface method with a body is a javac error ("interface
        // abstract methods cannot have body"), and a real abstract member
        // would in turn break every class in the same file that already
        // implements the interface ("is not abstract and does not override
        // abstract method"), e.g. vavr's `Either`/`Left`/`Right`. `default`
        // supplies a body without minting a new abstract obligation.
        modified.extend_from_slice(b"  default void added() {}\n}\n");
    } else {
        modified.extend_from_slice(b"  public void added() {}\n}\n");
    }
    let gen2_sources = [JavaSource {
        name: Path::new(extracted[last].0.as_str()),
        bytes: &modified,
    }];
    let mut gen2_image = Vec::new();
    bench
        .image(&gen2_sources, classpath, &mut gen2_image)
        .map_err(|error| {
            TestError::Image(format!("{} {} gen-2: {error}", row.purl, extracted[last].0))
        })?;
    if gen2_image.len() > MAX_IMAGE_BYTES {
        return Err(TestError::Fact("image exceeded 16 MiB"));
    }
    let mut gen2_output = vec![0; MAX_FRAGMENT_BYTES];
    let second = compile_fragment(&modified, &gen2_image, &mut gen2_output, &temp.path).map_err(
        |error| TestError::Compile(format!("{} {} gen-2: {error}", row.purl, extracted[last].0)),
    )?;
    let mut gen2_buffers = PublishBuffers::new(1);
    let second_published = publish_compiled(
        &reopened,
        &artifacts,
        std::slice::from_ref(&second),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut gen2_buffers.manifest,
            manifest_facts: &mut gen2_buffers.facts,
            ordinals: &mut gen2_buffers.ordinals,
            locality_output: &mut gen2_buffers.locality,
            binding_output: &mut gen2_buffers.binding,
        },
    )
    .map_err(|e| TestError::Publish(format!("{e:?}")))?;
    let second_generation = second_published.publication.generation;
    if second_generation == first_generation {
        return Err(TestError::Fact("generation did not advance"));
    }
    let newest = open_one(&reopened, &artifacts, &mut open_buffers)?;
    if newest.publication.generation != second_generation {
        return Err(TestError::Fact("reopened newest generation mismatch"));
    }
    let mut added = false;
    for fragment in newest.fragments() {
        let fragment = fragment.map_err(|e| TestError::Publish(e.to_string()))?;
        let view = FragmentView::validate(fragment.view.as_ref())
            .map_err(|e| TestError::Fragment(e.to_string()))?;
        added |= function_named(&view, b"added");
    }
    if !added {
        return Err(TestError::Fact(
            "newest generation missing the added entity",
        ));
    }
    reopened
        .shutdown()
        .map_err(|e| TestError::Publish(e.to_string()))?;

    let mut old_manifest = vec![0; 1 << 20];
    let mut old_facts = vec![None; first_bytes.len()];
    let old = ImmutableManifestStore::new(&artifacts)
        .map_err(|e| TestError::Publish(e.to_string()))?
        .open(first_identity, &mut old_manifest, &mut old_facts)
        .map_err(|e| TestError::Publish(e.to_string()))?;
    // `old.fragments()` walks the manifest in canonical package order, which
    // `CompilationPrepare::prepare` sorts by content-addressed fragment
    // identity (see `crates/engine/src/publication/manifest/build.rs`), not
    // by the compile-time extraction order captured in `first_bytes`. Match
    // each stored fragment back to its original by that identity rather than
    // position, with no weakening: every original fragment must still be
    // found, byte for byte.
    let mut original_by_identity: std::collections::HashMap<
        ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
        &Vec<u8>,
    > = std::collections::HashMap::with_capacity(first_bytes.len());
    for original in &first_bytes {
        let identity =
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(original);
        original_by_identity.insert(identity, original);
    }
    let mut matched = 0usize;
    for fact in old.fragments() {
        let original = original_by_identity
            .get(&fact.fragment)
            .ok_or(TestError::Fact("first-generation fragment facts diverged"))?;
        let mut old_bytes = vec![0; 16 << 20];
        let old_bytes = ImmutableArtifactStore::new(&artifacts)
            .map_err(|e| TestError::Publish(e.to_string()))?
            .open(fact, &mut old_bytes)
            .map_err(|e| TestError::Publish(e.to_string()))?;
        if Sha256::digest(old_bytes.as_ref()) != Sha256::digest(original.as_slice()) {
            return Err(TestError::Fact("old generation digest changed"));
        }
        FragmentView::validate(old_bytes.as_ref())
            .map_err(|e| TestError::Fragment(e.to_string()))?;
        matched += 1;
    }
    if matched != first_bytes.len() {
        return Err(TestError::Fact("first-generation fragment count diverged"));
    }
    Ok(())
}

#[test]
fn real_maven_corpus_lowers_and_publishes() -> Result<(), TestError> {
    let central = Central::new(CENTRAL_HOST);
    let started = Instant::now();
    let mut bench = Bench::new()?;
    for row_index in 0..CORPUS.len() {
        run_row(row_index, &central, &mut bench)?;
    }
    if let Some(error) = bench.harness.take_cleanup_error() {
        return Err(TestError::Io { source: error });
    }
    println!(
        "CORPUS|TOTAL|{}|{}",
        started.elapsed().as_millis(),
        CORPUS.len()
    );
    Ok(())
}

#[test]
fn corpus_missing_artifact_is_typed() -> Result<(), TestError> {
    let central = Central::new(CENTRAL_HOST);
    let coords = MavenCoordinates::parse("maven:demo:corpus-missing@9.9.9")
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
