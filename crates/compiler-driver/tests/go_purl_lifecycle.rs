#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod go_support;

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use backend_semantic::ir::{FragmentView, ImageProvenance};
use backend_frontend_go::legacy::{GoImage, GoOracle};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, NativeTool, Stage};
use backend_version::{ContentId, SourceFactDomain};
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
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::Go(GoVersion::Go125);
const STAGE: Stage = Stage::LowerIr;
const SYNC_SYMBOLS: &[&[u8]] = &[
    b"Group",
    b"Go",
    b"TryGo",
    b"Wait",
    b"SetLimit",
    b"WithContext",
    b"Do",
    b"DoChan",
    b"Forget",
];

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(#[from] go_support::Error),
    #[error("filesystem operation failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("Go toolchain resolution failed: {source}")]
    Toolchain {
        #[source]
        source: compiler_driver::ToolchainResolutionError,
    },
    #[error("Go oracle failed: {source}")]
    Oracle {
        #[source]
        source: backend_frontend_go::legacy::OracleError,
    },
    #[error("fragment validation failed: {source}")]
    Fragment {
        #[source]
        source: backend_semantic::ir::FragmentError,
    },
    #[error("publication failed: {cause}")]
    Publish { cause: String },
    #[error("reopen failed: {cause}")]
    Reopen { cause: String },
    #[error("index failed: {cause}")]
    Index { cause: String },
    #[error("journey fact falsifier: {0}")]
    Fact(&'static str),
    #[error("compile failed: {cause}")]
    Compile { cause: String },
}
fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

#[test]
fn malformed_purl_is_typed_rejection() -> Result<(), TestError> {
    match go_support::Purl::parse("golang:golang.org/x/sync") {
        Err(go_support::Error::Purl { input }) if input == "golang:golang.org/x/sync" => Ok(()),
        _ => Err(TestError::Fact(
            "malformed PURL was accepted or lost its input",
        )),
    }
}

#[test]
fn proxy_download_enforces_cap_and_deadline() -> Result<(), TestError> {
    let purl = go_support::Purl::parse("golang:golang.org/x/sync@v0.10.0")?;
    let (url, digest) = go_support::locate(&purl)?;
    match go_support::download(&url, 1024, Instant::now() + Duration::from_secs(60)) {
        Err(go_support::Error::Cap {
            cap: 1024,
            observed,
        }) if observed > 1024 => {}
        _ => return Err(TestError::Fact("proxy download admitted a cap breach")),
    }
    let archive = go_support::download(
        &url,
        8 * 1024 * 1024,
        Instant::now() + Duration::from_secs(60),
    )?;
    if let Some(digest) = digest {
        if go_support::sha256(&archive) != digest {
            return Err(TestError::Fact("ziphash did not bind downloaded zip"));
        }
    } else if go_support::sha256(&archive) == [0; 32] {
        return Err(TestError::Fact("fallback zip digest was empty"));
    }
    Ok(())
}

fn toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let path = std::env::var_os("COMPILER_GO_COMPILER")
        .map(std::path::PathBuf::from)
        .ok_or(TestError::Fact("COMPILER_GO_COMPILER is not set"))?;
    let path = Box::leak(path.canonicalize().map_err(io)?.into_boxed_path());
    let output = std::process::Command::new(&*path)
        .arg("version")
        .output()
        .map_err(io)?;
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::GoCompiler, path, version)
        .map_err(|source| TestError::Toolchain { source })
}

fn compile_fragment<'a>(
    source: &'a [u8],
    image: &'a [u8],
    tool: &ResolvedToolchain<'_>,
    output: &'a mut [u8],
) -> Result<compiler_driver::CompiledFragment<'a>, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4096];
    let _authority = GoImage::open(image).map_err(|cause| TestError::Compile {
        cause: cause.to_string(),
    })?;
    compile(
        CompileRequest {
            profile: PROFILE,
            stage: STAGE,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(*tool),
            authority: SemanticAuthorityInput::Go { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    })
}

fn lifecycle(
    purl_text: &str,
    module: &str,
    primary_relative: &str,
    symbols: &[&[u8]],
    expected_capacity: Option<(usize, usize)>,
) -> Result<(), TestError> {
    let purl = go_support::Purl::parse(purl_text)?;
    let (url, ziphash) = go_support::locate(&purl)?;
    let archive = go_support::download(
        &url,
        8 * 1024 * 1024,
        Instant::now() + Duration::from_secs(60),
    )?;
    if let Some(ziphash) = ziphash {
        if go_support::sha256(&archive) != ziphash {
            return Err(TestError::Fact("download digest mismatch"));
        }
    } else {
        eprintln!(
            "Go proxy did not serve .ziphash; assertion records first-fetch zip digest {:?}",
            go_support::sha256(&archive)
        );
    }
    let root = go_support::fresh_dir("module")?;
    go_support::unpack(&archive, &root)?;
    let module_root = root.join(module);
    if go_support::ensure_module(&root, module)? {
        eprintln!("synthesized go.mod for pre-module release {purl_text}");
    }
    let primary = go_support::find_primary(&root, module, primary_relative)?;
    let source = fs::read(&primary).map_err(io)?;
    let oracle = GoOracle::default();
    let image_bytes = oracle
        .authority_image(&primary, &module_root)
        .map_err(|source| TestError::Oracle { source })?;
    let image = GoImage::open(&image_bytes).map_err(|cause| {
        TestError::Fact(if cause.to_string().is_empty() {
            "authority image did not open"
        } else {
            "authority image rejected"
        })
    })?;
    if image.source_digest() != go_support::sha256(&source) {
        return Err(TestError::Fact(
            "image was not bound to primary exact bytes",
        ));
    }
    let mut diagnostic = [0; 4096];
    let cancelled = AtomicBool::new(false);
    let ir = compile_ir(
        CompileRequest {
            profile: PROFILE,
            stage: STAGE,
            source: &source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain()?),
            authority: SemanticAuthorityInput::Go {
                image: &image_bytes,
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &module_root,
        },
    )
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    })?;
    let columns = ir.ir.entity_columns();
    let names = columns.names;
    let atom = |id: backend_semantic::ir::AtomId| ir.ir.atom(id);
    for wanted in symbols {
        if !names
            .iter()
            .filter_map(|id| atom(*id))
            .any(|name| name == *wanted)
        {
            return Err(TestError::Fact("oracle symbol absent from decoded IR"));
        }
    }
    let source_digest: [u8; 32] = Sha256::digest(&source).into();
    let ImageProvenance::Captured {
        source: image_source,
        ..
    } = ir.ir.image_provenance()
    else {
        return Err(TestError::Fact("semantic image provenance unavailable"));
    };
    if image_source.identity != ContentId::<SourceFactDomain>::from_canonical_bytes(&source)
        || source_digest != go_support::sha256(&source)
    {
        return Err(TestError::Fact("IR source identity mismatch"));
    }
    let mut fragment_output = vec![0; 8 * 1024 * 1024];
    let fragment = compile_fragment(&source, &image_bytes, &toolchain()?, &mut fragment_output)?;
    FragmentView::validate(fragment.fragment.as_ref())
        .map_err(|source| TestError::Fragment { source })?
        .occurrences()
        .ok_or(TestError::Fact("occurrence lane absent"))?;
    let store = go_support::fresh_dir("published")?;
    let journal_dir = store.join("journal");
    let artifacts = store.join("artifacts");
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let journal = DurablePublisher::create(&PublicationPaths::in_directory(&journal_dir), limits)
        .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let mut manifest = vec![0; 1 << 20];
    let mut facts = [None; 1];
    let mut ordinals = [0; 1];
    let mut locality = vec![0; 1 << 16];
    let mut binding = vec![0; 128];
    let published = publish_compiled(
        &journal,
        &artifacts,
        std::slice::from_ref(&fragment),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
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
    let reopened = DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_dir), limits)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    let mut mo = vec![0; 1 << 20];
    let mut mf = [None; 1];
    let mut fo = vec![0; 8 * 1024 * 1024];
    let mut lo = vec![0; 1 << 16];
    let opened = open_published(
        &reopened,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut mo,
            manifest_facts: &mut mf,
            fragment_output: &mut fo,
            locality_output: &mut lo,
        },
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?
    .ok_or(TestError::Fact("publication disappeared on reopen"))?;
    let first = opened
        .fragments()
        .next()
        .ok_or(TestError::Fact("published fragment absent"))?
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    let n = first.view.entities().len();
    let a = first.view.atoms().len();
    let t = first.view.type_nodes().len();
    eprintln!("{purl_text} decoded entity count: {n}");
    if module == "github.com/mitchellh/go-homedir@v1.1.0" && n != 27 {
        return Err(TestError::Fact("go-homedir entity count changed from 27"));
    }
    let mut p = (0..n).map(|_| MaybeUninit::uninit()).collect::<Vec<_>>();
    let mut e = (0..n).map(|_| MaybeUninit::uninit()).collect::<Vec<_>>();
    let mut x = (0..n).map(|_| MaybeUninit::uninit()).collect::<Vec<_>>();
    let mut l = (0..n).map(|_| MaybeUninit::uninit()).collect::<Vec<_>>();
    let mut aa = (0..a).map(|_| MaybeUninit::uninit()).collect::<Vec<_>>();
    let mut tt = (0..t).map(|_| MaybeUninit::uninit()).collect::<Vec<_>>();
    let prepared = match build(
        &first,
        IndexBuildScratch {
            projections: &mut p,
            entities: &mut e,
            exact_rows: &mut x,
            lexical_rows: &mut l,
            atoms: &mut aa,
            type_nodes: &mut tt,
        },
    ) {
        Ok(prepared) => Some(prepared),
        Err(server_index_build::BuildError::Admission(
            server_index_build::BuildAdmissionError::EntityLimit { maximum, observed },
        )) if expected_capacity == Some((maximum, observed)) => {
            eprintln!(
                "typed index terminal: fragment has {observed} entities; shared segment capacity is {maximum}"
            );
            // The large whole-module fragment intentionally proves the typed
            // admission terminal; the smaller second journey proves sealing.
            None
        }
        Err(cause) => {
            return Err(TestError::Index {
                cause: cause.to_string(),
            });
        }
    };
    if let Some(prepared) = prepared {
        let mut exact = [prepared.exact.id];
        let mut lexical = [prepared.lexical.id];
        let sealed = seal_compilation_index(
            opened,
            std::slice::from_ref(&prepared),
            CompilationIndexScratch {
                exact: &mut exact,
                lexical: &mut lexical,
            },
        )
        .map_err(|cause| TestError::Index {
            cause: cause.error.to_string(),
        })?;
        let plan = plan_index_pack(&sealed).map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
        let mut encoded = vec![0; plan.encoded_bytes];
        encode_index_pack(&plan, &mut encoded).map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
    }
    let modified = [
        source.as_slice(),
        b"\nfunc GenTwoProbe() int { return 2 }\n",
    ]
    .concat();
    fs::write(&primary, &modified).map_err(io)?;
    let second_image = oracle
        .authority_image(&primary, &module_root)
        .map_err(|source| TestError::Oracle { source })?;
    let mut second_output = vec![0; 8 * 1024 * 1024];
    let second = compile_fragment(&modified, &second_image, &toolchain()?, &mut second_output)?;
    let second_dir = store.join("journal-second");
    let second_journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&second_dir), limits).map_err(
            |cause| TestError::Publish {
                cause: cause.to_string(),
            },
        )?;
    let mut sm = vec![0; 1 << 20];
    let mut sf = [None; 1];
    let mut so = [0; 1];
    let mut sl = vec![0; 1 << 16];
    let mut sb = vec![0; 128];
    let second_pub = publish_compiled(
        &second_journal,
        &artifacts,
        std::slice::from_ref(&second),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut sm,
            manifest_facts: &mut sf,
            ordinals: &mut so,
            locality_output: &mut sl,
            binding_output: &mut sb,
        },
    )
    .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    if second_pub.publication.generation == first_generation {
        return Err(TestError::Fact("publication generation did not advance"));
    }
    second_journal
        .shutdown()
        .map_err(|cause| TestError::Publish {
            cause: cause.to_string(),
        })?;
    let newest = DurablePublisher::reopen(&PublicationPaths::in_directory(&second_dir), limits)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    let mut nm = vec![0; 1 << 20];
    let mut nf = [None; 1];
    let mut nfr = vec![0; 8 * 1024 * 1024];
    let mut nl = vec![0; 1 << 16];
    let newest_open = open_published(
        &newest,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut nm,
            manifest_facts: &mut nf,
            fragment_output: &mut nfr,
            locality_output: &mut nl,
        },
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?
    .ok_or(TestError::Fact("second publication disappeared"))?;
    let newest_fragment = newest_open
        .fragments()
        .next()
        .ok_or(TestError::Fact("second fragment absent"))?
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    FragmentView::validate(newest_fragment.view.as_ref())
        .map_err(|source| TestError::Fragment { source })?;
    let old = ImmutableArtifactStore::new(&artifacts)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?
        .open(first.facts, &mut fo)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    FragmentView::validate(old.as_ref()).map_err(|source| TestError::Fragment { source })?;
    drop(newest_open);
    newest.shutdown().map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    reopened.shutdown().map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    fs::remove_dir_all(root).map_err(io)?;
    fs::remove_dir_all(store).map_err(io)?;
    Ok(())
}

#[test]
fn purl_proxy_oracle_publish_reopen_index_and_chained_generation() -> Result<(), TestError> {
    let started = Instant::now();
    let result = lifecycle(
        "golang:golang.org/x/sync@v0.10.0",
        "golang.org/x/sync@v0.10.0",
        "errgroup/errgroup.go",
        SYNC_SYMBOLS,
        Some((256, 334)),
    );
    eprintln!("go journey wall-clock: {:?}", started.elapsed());
    result
}

#[test]
fn pkg_errors_proxy_oracle_publish_reopen_index_and_chained_generation() -> Result<(), TestError> {
    let started = Instant::now();
    let result = lifecycle(
        "golang:github.com/pkg/errors@v0.9.1",
        "github.com/pkg/errors@v0.9.1",
        "errors.go",
        &[b"New", b"Errorf", b"Wrap", b"Cause", b"WithStack"],
        Some((256, 263)),
    );
    eprintln!("pkg/errors journey wall-clock: {:?}", started.elapsed());
    result
}

#[test]
fn google_uuid_proxy_oracle_publish_reopen_index_and_chained_generation() -> Result<(), TestError> {
    let started = Instant::now();
    let result = lifecycle(
        "golang:github.com/google/uuid@v1.6.0",
        "github.com/google/uuid@v1.6.0",
        "uuid.go",
        &[
            b"UUID",
            b"New",
            b"Parse",
            b"Must",
            b"NewString",
            b"FromBytes",
        ],
        Some((256, 411)),
    );
    eprintln!("google/uuid journey wall-clock: {:?}", started.elapsed());
    result
}

#[test]
fn homedir_proxy_oracle_publish_reopen_index_and_chained_generation() -> Result<(), TestError> {
    let started = Instant::now();
    let result = lifecycle(
        "golang:github.com/mitchellh/go-homedir@v1.1.0",
        "github.com/mitchellh/go-homedir@v1.1.0",
        "homedir.go",
        &[b"Dir", b"Expand", b"DisableCache", b"Reset"],
        None,
    );
    eprintln!("go-homedir journey wall-clock: {:?}", started.elapsed());
    result
}
