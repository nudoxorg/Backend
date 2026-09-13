#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "python_support/journey.rs"]
mod journey_support;
mod python_support;

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{FragmentView, ImageProvenance};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, PythonVersion, Stage};
use backend_version::{ContentId, SourceFactDomain};
use server_index_build::{IndexBuildScratch, PreparedIndex, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::Python(PythonVersion::Python314);
const STAGE: Stage = Stage::LowerIr;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(#[from] python_support::Error),
    #[error(transparent)]
    Journey(#[from] journey_support::Error),
    #[error("filesystem operation failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("toolchain resolution failed: {source}")]
    Toolchain {
        #[source]
        source: compiler_driver::ToolchainResolutionError,
    },
    #[error("fragment validation failed: {source}")]
    Fragment {
        #[source]
        source: compiler_ir::FragmentError,
    },
    #[error("publication failed: {cause}")]
    Publish { cause: String },
    #[error("publication reopen failed: {cause}")]
    Reopen { cause: String },
    #[error("index build or seal failed: {cause}")]
    Index { cause: String },
    #[error("journey fact falsifier: {0}")]
    Fact(&'static str),
    #[error("journey compile failed: {cause}")]
    JourneyCompile { cause: String },
    #[error("digest lineage mismatch")]
    Digest,
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

/// One package-class journey: the pinned PURL, the primary module's exact
/// trailing path components (src layout, package dir, or bare top level),
/// the declarations the second generation must still carry, and the probe
/// function that distinguishes the second generation from the first.
struct Journey {
    label: &'static str,
    purl: &'static str,
    primary: &'static [&'static str],
    symbols: &'static [&'static [u8]],
    probe: &'static str,
    layout: journey_support::LayoutClass,
    /// The exact typed index-admission terminal this journey may hit: the
    /// shared exact/lexical segment bound is 256 entities per index segment.
    /// six 1.17.0's whole-module fragment carries 331 decoded entities, so
    /// its index build stops there; idna and PyYAML fragments fit and prove
    /// the full index path.
    index_entity_limit: Option<(usize, usize)>,
}

/// six 1.17.0 declares `PY2`/`PY3` at top level and `add_metaclass`/
/// `with_metaclass` as plain functions; `b`/`u` are declared inside BOTH
/// `if PY3:` and `else:` branches, so the journey also proves the emitted
/// set covers branch-carried declarations. `MovesMetaclass` is deliberately
/// absent: six 1.17.0 never declared it.
const SIX_JOURNEY: Journey = Journey {
    label: "six",
    purl: "pypi:six@1.17.0",
    primary: &["six.py"],
    symbols: &[
        b"PY2",
        b"PY3",
        b"add_metaclass",
        b"with_metaclass",
        b"b",
        b"u",
    ],
    probe: "six_lifecycle_probe",
    layout: journey_support::LayoutClass::FlatSingleModule,
    index_entity_limit: Some((256, 331)),
};

/// idna 3.10 ships its package directory at the archive root: the primary is
/// `idna/core.py` beside `setup.py`, with no `src/` prefix.
const IDNA_JOURNEY: Journey = Journey {
    label: "idna",
    purl: "pypi:idna@3.10",
    primary: &["idna", "core.py"],
    symbols: &[b"IDNAError", b"encode", b"decode", b"uts46_remap"],
    probe: "idna_lifecycle_probe",
    layout: journey_support::LayoutClass::FlatPackageDir,
    index_entity_limit: None,
};

/// PyYAML 6.0.2 ships a lib-rooted package-dir layout: the primary is `yaml/__init__.py`
/// under `lib/`, so the primary lives inside the runtime package directory.
const PYYAML_JOURNEY: Journey = Journey {
    label: "pyyaml",
    purl: "pypi:PyYAML@6.0.2",
    primary: &["yaml", "__init__.py"],
    symbols: &[b"load", b"dump", b"scan", b"safe_load"],
    probe: "pyyaml_lifecycle_probe",
    layout: journey_support::LayoutClass::LibPackageDir,
    index_entity_limit: None,
};

const WEBENCODINGS_JOURNEY: Journey = Journey {
    label: "webencodings",
    purl: "pypi:webencodings@0.5.1",
    primary: &["webencodings", "__init__.py"],
    symbols: &[b"Encoding", b"decode", b"lookup", b"ascii_lower"],
    probe: "webencodings_lifecycle_probe",
    layout: journey_support::LayoutClass::FlatPackageDir,
    index_entity_limit: None,
};

#[test]
fn malformed_purl_is_typed_rejection() -> Result<(), TestError> {
    match journey_support::Purl::parse("pypi:six") {
        Err(journey_support::Error::Purl { input }) if input == "pypi:six" => Ok(()),
        _ => Err(TestError::Fact(
            "malformed PURL was accepted or lost its input",
        )),
    }
}

#[test]
fn real_downloader_enforces_cap_timeout_and_archive_corruption() -> Result<(), TestError> {
    let purl = journey_support::Purl::parse("pypi:six@1.17.0")?;
    let (url, declared_digest, wheel) = journey_support::locate(&purl)?;
    if !wheel.ends_with(".whl") {
        return Err(TestError::Fact("wheel filename was not recorded"));
    }
    match python_support::download(&url, 1024, Instant::now() + Duration::from_secs(60)) {
        Err(python_support::Error::Cap {
            cap: 1024,
            observed,
        }) if observed > 1024 => {}
        _ => {
            return Err(TestError::Fact(
                "real downloader admitted the 1 KiB cap breach",
            ));
        }
    }
    match python_support::download(
        &url,
        8 * 1024 * 1024,
        Instant::now() + Duration::from_nanos(1),
    ) {
        Err(python_support::Error::Deadline { observed }) => {
            let _ = observed;
        }
        _ => {
            return Err(TestError::Fact(
                "1ns deadline did not produce typed timeout",
            ));
        }
    }
    let archive = python_support::download(
        &url,
        8 * 1024 * 1024,
        Instant::now() + Duration::from_secs(60),
    )?;
    if python_support::sha256(&archive) != declared_digest {
        return Err(TestError::Digest);
    }
    let mut corrupt = archive.clone();
    let at = corrupt.len() / 2;
    if let Some(byte) = corrupt.get_mut(at) {
        *byte ^= 1;
    } else {
        return Err(TestError::Fact("empty downloaded archive"));
    }
    let root = python_support::fresh_dir("corrupt")?;
    let result = python_support::unpack(&corrupt, &root);
    fs::remove_dir_all(root).map_err(io)?;
    match result {
        Err(python_support::Error::Archive { .. }) => Ok(()),
        _ => Err(TestError::Fact(
            "mutated archive was accepted or lost gzip cause",
        )),
    }
}

fn python_toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let path = std::env::var_os("COMPILER_PYTHON_COMPILER")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("PATH")
                .map(|paths| {
                    std::env::split_paths(&paths)
                        .map(|dir| dir.join("python3"))
                        .find(|path| path.is_file())
                })
                .unwrap_or_default()
        })
        .ok_or(TestError::Fact("no Python executable"))?;
    let path = Box::leak(path.canonicalize().map_err(io)?.into_boxed_path());
    let output = std::process::Command::new(&*path)
        .arg("--version")
        .output()
        .map_err(io)?;
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::Python, path, version)
        .map_err(|source| TestError::Toolchain { source })
}

fn compile_fragment<'a>(
    source: &'a [u8],
    tool: &ResolvedToolchain<'_>,
    output: &'a mut [u8],
) -> Result<compiler_driver::CompiledFragment<'a>, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let work = python_support::fresh_dir("fragment")?;
    let result = compile(
        CompileRequest {
            profile: PROFILE,
            stage: STAGE,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(*tool),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: output,
        },
    );
    let cleanup = fs::remove_dir_all(&work).map_err(io);
    let result = result.map_err(|failure| TestError::JourneyCompile {
        cause: format!("{failure:?}"),
    });
    match (result, cleanup) {
        (Ok(compiled), Ok(())) => Ok(compiled),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), _) => Err(error),
    }
}

/// The shared fetch→compile→publish→reopen→index→second-generation→
/// old-fragment-revalidate skeleton behind the three package-class journeys.
fn package_class_lifecycle(journey: &Journey) -> Result<(), TestError> {
    let purl = journey_support::Purl::parse(journey.purl)?;
    let (url, declared_digest, _wheel) = journey_support::locate(&purl)?;
    let archive = python_support::download(
        &url,
        8 * 1024 * 1024,
        Instant::now() + Duration::from_secs(60),
    )?;
    let download_digest = python_support::sha256(&archive);
    if download_digest != declared_digest {
        return Err(TestError::Digest);
    }
    let root = python_support::fresh_dir("journey")?;
    python_support::unpack(&archive, &root)?;
    let source_path = journey_support::find_primary(&root, journey.layout, journey.primary)?;
    eprintln!(
        "python journey: {} class={:?} primary={}",
        journey.label,
        journey.layout,
        source_path.display()
    );
    let source = fs::read(&source_path).map_err(io)?;
    let source_digest = python_support::sha256(&source);
    let tool = python_toolchain()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let ir = compile_ir(
        CompileRequest {
            profile: PROFILE,
            stage: STAGE,
            source: &source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &root,
        },
    )
    .map_err(|failure| TestError::JourneyCompile {
        cause: format!("{failure:?}"),
    })?;
    let compiled_source_digest: [u8; 32] = Sha256::digest(source.as_slice()).into();
    let ImageProvenance::Captured {
        source: image_source,
        ..
    } = ir.ir.image_provenance()
    else {
        return Err(TestError::Digest);
    };
    if image_source.identity != ContentId::<SourceFactDomain>::from_canonical_bytes(&source)
        || compiled_source_digest != source_digest
    {
        return Err(TestError::Digest);
    }
    let columns = ir.ir.entity_columns();
    let names = columns.names;
    let atom = |id: compiler_ir::AtomId| ir.ir.atom(id);
    let mut facts = Vec::new();
    for (id, name) in names.iter().enumerate() {
        let bytes = atom(*name).ok_or(TestError::Fact("entity atom missing"))?;
        facts.push((id, bytes, columns.kinds[id]));
    }
    for wanted in journey.symbols {
        if !facts.iter().any(|(_, name, _)| *name == *wanted) {
            return Err(TestError::Fact("required package declaration absent"));
        }
    }
    let storage = ir.ir.storage_columns();
    if storage.docs.elements.is_empty() {
        return Err(TestError::Fact("docstring facts absent"));
    }
    let mut fragment_output = vec![0_u8; 8 * 1024 * 1024];
    let fragment = compile_fragment(&source, &tool, &mut fragment_output)?;
    let decoded = FragmentView::validate(fragment.fragment.as_ref())
        .map_err(|source| TestError::Fragment { source })?;
    if decoded.occurrences().is_none() {
        return Err(TestError::Fact("import occurrence lane absent"));
    }
    let fragment_bytes = fragment.fragment.as_ref().to_vec();
    let root_store = python_support::fresh_dir("published")?;
    let journal_dir = root_store.join("journal");
    let artifacts = root_store.join("artifacts");
    let journal = DurablePublisher::create(
        &PublicationPaths::in_directory(&journal_dir),
        PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN).map_err(
            |cause| TestError::Index {
                cause: cause.to_string(),
            },
        )?,
    )
    .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let mut manifest = vec![0_u8; 1 << 20];
    let mut mf = [None; 1];
    let mut ord = [0_usize; 1];
    let mut locality = vec![0_u8; 1 << 16];
    let mut binding = vec![0_u8; 128];
    let published = publish_compiled(
        &journal,
        &artifacts,
        std::slice::from_ref(&fragment),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut mf,
            ordinals: &mut ord,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let first_generation = published.publication.generation;
    journal.shutdown().map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let reopened_journal = DurablePublisher::reopen(
        &PublicationPaths::in_directory(&journal_dir),
        PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN).map_err(
            |cause| TestError::Index {
                cause: cause.to_string(),
            },
        )?,
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?;
    let mut mo = vec![0_u8; 1 << 20];
    let mut mff = [None; 1];
    let mut fo = vec![0_u8; 8 * 1024 * 1024];
    let mut lo = vec![0_u8; 1 << 16];
    let opened = open_published(
        &reopened_journal,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut mo,
            manifest_facts: &mut mff,
            fragment_output: &mut fo,
            locality_output: &mut lo,
        },
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?
    .ok_or_else(|| TestError::Reopen {
        cause: "no published compilation".to_owned(),
    })?;
    let fragment_ref = opened
        .fragments()
        .next()
        .ok_or_else(|| TestError::Reopen {
            cause: "published compilation has no fragment".to_owned(),
        })?
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    let first_fragment_facts = fragment_ref.facts;
    // One scratch entry per reopened lane row: projections, entity facts,
    // and both index rows are one per fragment entity; the atom and
    // type-node lookups are one per respective lane entry.
    let reopened_view = &fragment_ref.view;
    let entity_count = reopened_view.entities().len();
    let atom_count = reopened_view.atoms().len();
    let type_node_count = reopened_view.type_nodes().len();
    let mut projections = (0..entity_count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut entities = (0..entity_count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut exact = (0..entity_count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut lexical = (0..entity_count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut atoms = (0..atom_count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut types = (0..type_node_count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    // The index build either prepares one segment or stops at the journey's
    // exact typed admission terminal: the shared exact/lexical segment bound
    // (256 entities per segment) against the whole-module entity count.
    let built = build(
        &fragment_ref,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    );
    enum IndexOutcome<'scratch> {
        Prepared(PreparedIndex<'scratch>),
        Bounded,
    }
    let prepared = match built {
        Ok(prepared) => IndexOutcome::Prepared(prepared),
        Err(server_index_build::BuildError::Admission(admission))
            if journey
                .index_entity_limit
                .is_some_and(|(maximum, observed)| {
                    admission
                        == server_index_build::BuildAdmissionError::EntityLimit {
                            maximum,
                            observed,
                        }
                }) =>
        {
            eprintln!(
                "python journey typed terminal: {} index segment bound {admission:?}",
                journey.label
            );
            IndexOutcome::Bounded
        }
        Err(cause) => {
            return Err(TestError::Index {
                cause: cause.to_string(),
            });
        }
    };
    if let IndexOutcome::Prepared(prepared) = prepared {
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
        .map_err(|cause| TestError::Index {
            cause: cause.error.to_string(),
        })?;
        let plan = plan_index_pack(&sealed).map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
        let mut encoded = vec![0_u8; plan.encoded_bytes];
        encode_index_pack(&plan, &mut encoded).map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
    }
    let modified = [
        source.as_slice(),
        format!("\n\ndef {}():\n    return True\n", journey.probe).as_bytes(),
    ]
    .concat();
    let mut second_output = vec![0_u8; 8 * 1024 * 1024];
    let second = compile_fragment(&modified, &tool, &mut second_output)?;
    // One durable journal commits exactly one workflow stage key, so the
    // second generation publishes through its own journal over the same
    // immutable artifact store, mirroring the production two-stage flow.
    let second_journal_dir = root_store.join("journal-second");
    let second_journal = DurablePublisher::create(
        &PublicationPaths::in_directory(&second_journal_dir),
        PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN).map_err(
            |cause| TestError::Index {
                cause: cause.to_string(),
            },
        )?,
    )
    .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let mut second_manifest = vec![0_u8; 1 << 20];
    let mut second_mf = [None; 1];
    let mut second_ord = [0_usize; 1];
    let mut second_locality = vec![0_u8; 1 << 16];
    let mut second_binding = vec![0_u8; 128];
    let second_published = publish_compiled(
        &second_journal,
        &artifacts,
        std::slice::from_ref(&second),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut second_manifest,
            manifest_facts: &mut second_mf,
            ordinals: &mut second_ord,
            locality_output: &mut second_locality,
            binding_output: &mut second_binding,
        },
    )
    .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    second_journal
        .shutdown()
        .map_err(|cause| TestError::Publish {
            cause: cause.to_string(),
        })?;
    let newest_publisher = DurablePublisher::reopen(
        &PublicationPaths::in_directory(&second_journal_dir),
        PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN).map_err(
            |cause| TestError::Index {
                cause: cause.to_string(),
            },
        )?,
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?;
    let mut newest_manifest = vec![0_u8; 1 << 20];
    let mut newest_facts = [None; 1];
    let mut newest_fragments = vec![0_u8; 8 * 1024 * 1024];
    let mut newest_locality = vec![0_u8; 1 << 16];
    let newest = open_published(
        &newest_publisher,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut newest_manifest,
            manifest_facts: &mut newest_facts,
            fragment_output: &mut newest_fragments,
            locality_output: &mut newest_locality,
        },
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?
    .ok_or_else(|| TestError::Reopen {
        cause: "no newest published compilation".to_owned(),
    })?;
    let newest_fragment = newest
        .fragments()
        .next()
        .ok_or_else(|| TestError::Reopen {
            cause: "newest compilation has no fragment".to_owned(),
        })?
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    let newest_view = FragmentView::validate(newest_fragment.view.as_ref())
        .map_err(|source| TestError::Fragment { source })?;
    let has_symbol = |wanted: &[u8]| {
        newest_view.entities().any(|entity| {
            newest_view
                .atoms()
                .nth(entity.name.raw as usize)
                .is_some_and(|atom| atom.bytes == wanted)
        })
    };
    for wanted in journey.symbols {
        if !has_symbol(wanted) {
            return Err(TestError::Fact(
                "new generation lost a pre-existing declaration",
            ));
        }
    }
    if !has_symbol(journey.probe.as_bytes()) {
        return Err(TestError::Fact("new generation lost the probe declaration"));
    }
    let second_generation = second_published.publication.generation;
    if first_generation == second_generation {
        return Err(TestError::Fact(
            "second publication reused first generation",
        ));
    }
    let mut old_fragment_output = vec![0_u8; 8 * 1024 * 1024];
    let old_fragment = ImmutableArtifactStore::new(&artifacts)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?
        .open(first_fragment_facts, &mut old_fragment_output)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    let reread_digest: [u8; 32] = Sha256::digest(old_fragment.as_ref()).into();
    let original_digest: [u8; 32] = Sha256::digest(fragment_bytes.as_slice()).into();
    if reread_digest != original_digest {
        return Err(TestError::Digest);
    }
    FragmentView::validate(old_fragment.as_ref())
        .map_err(|source| TestError::Fragment { source })?;
    // Close each journal through its own live owner: the second journal's
    // reopened owner shuts down here, and the first journal's reopened
    // owner shuts down too, so both directories are closed before removal.
    drop(newest);
    newest_publisher
        .shutdown()
        .map_err(|cause| TestError::Publish {
            cause: cause.to_string(),
        })?;
    reopened_journal
        .shutdown()
        .map_err(|cause| TestError::Publish {
            cause: cause.to_string(),
        })?;
    fs::remove_dir_all(root).map_err(io)?;
    fs::remove_dir_all(root_store).map_err(io)?;
    Ok(())
}

#[test]
fn purl_six_download_unpack_compile_publish_reopen_index_and_old_generation()
-> Result<(), TestError> {
    package_class_lifecycle(&SIX_JOURNEY)
}

#[test]
fn purl_idna_src_layout_full_lifecycle() -> Result<(), TestError> {
    package_class_lifecycle(&IDNA_JOURNEY)
}

#[test]
fn purl_pyyaml_package_dir_full_lifecycle() -> Result<(), TestError> {
    package_class_lifecycle(&PYYAML_JOURNEY)
}

#[test]
fn purl_webencodings_single_module_full_lifecycle() -> Result<(), TestError> {
    package_class_lifecycle(&WEBENCODINGS_JOURNEY)
}
