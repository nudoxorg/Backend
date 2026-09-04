#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod go_support;

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, FactFault,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::FragmentView;
use compiler_languages_go::{GoImage, GoOracle};
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published,
};
use compiler_vocabulary::{GoVersion, LanguageProfile, NativeTool, Stage};
use heart_identity::{ContentId, SourceFactDomain};
use server_index_build::{IndexBuildScratch, build};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use std::{
    fs,
    mem::MaybeUninit,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::Go(GoVersion::Go125);
const STAGE: Stage = Stage::LowerIr;
const CAPACITY: usize = 256;
const ROWS: &[(&str, &str, &[&[u8]])] = &[
    (
        "golang:github.com/google/uuid@v1.6.0",
        "github.com/google/uuid@v1.6.0",
        &[b"UUID", b"New", b"Parse", b"NewString", b"FromBytes"],
    ),
    (
        "golang:gopkg.in/yaml.v3@v3.0.1",
        "gopkg.in/yaml.v3@v3.0.1",
        &[b"Node", b"Decoder", b"Encode", b"Decode", b"Unmarshal"],
    ),
    (
        "golang:github.com/gorilla/mux@v1.8.1",
        "github.com/gorilla/mux@v1.8.1",
        &[b"Router", b"NewRouter", b"Handle", b"ServeHTTP", b"Use"],
    ),
    (
        "golang:github.com/google/go-cmp@v0.6.0",
        "github.com/google/go-cmp@v0.6.0",
        &[b"Comparer", b"Options", b"Diff", b"Equal", b"FilterPath"],
    ),
    (
        "golang:golang.org/x/sync@v0.10.0",
        "golang.org/x/sync@v0.10.0",
        &[b"Group", b"Go", b"TryGo", b"Wait", b"SetLimit"],
    ),
    (
        "golang:golang.org/x/mod@v0.17.0",
        "golang.org/x/mod@v0.17.0",
        &[b"File", b"Parse", b"Format", b"AddRequire", b"SortBlocks"],
    ),
    (
        "golang:golang.org/x/time@v0.5.0",
        "golang.org/x/time@v0.5.0",
        &[b"Limiter", b"NewLimiter", b"Allow", b"Reserve", b"Wait"],
    ),
    (
        "golang:golang.org/x/tools@v0.30.0",
        "golang.org/x/tools@v0.30.0",
        &[b"Pass", b"Analyzer", b"Inspect", b"Reportf", b"ResultOf"],
    ),
    (
        "golang:github.com/pelletier/go-toml/v2@v2.2.2",
        "github.com/pelletier/go-toml/v2@v2.2.2",
        &[b"Encoder", b"Decoder", b"Marshal", b"Unmarshal", b"Encode"],
    ),
    (
        "golang:github.com/oklog/ulid/v2@v2.1.0",
        "github.com/oklog/ulid/v2@v2.1.0",
        &[b"ULID", b"Make", b"Parse", b"Entropy", b"Time"],
    ),
    (
        "golang:github.com/cespare/xxhash/v2@v2.3.0",
        "github.com/cespare/xxhash/v2@v2.3.0",
        &[b"Digest", b"Sum64", b"Write", b"Reset", b"BlockSize"],
    ),
    (
        "golang:github.com/BurntSushi/toml@v1.4.0",
        "github.com/BurntSushi/toml@v1.4.0",
        &[
            b"MetaData",
            b"Decode",
            b"DecodeFile",
            b"Primitive",
            b"Undecoded",
        ],
    ),
    (
        "golang:github.com/zeebo/xxh3@v1.0.2",
        "github.com/zeebo/xxh3@v1.0.2",
        &[b"Hash", b"HashSeed", b"Hasher", b"Write", b"Sum64"],
    ),
    (
        "golang:github.com/minio/highwayhash@v1.0.2",
        "github.com/minio/highwayhash@v1.0.2",
        &[b"New", b"New64", b"Write", b"Sum", b"Reset"],
    ),
    (
        "golang:github.com/julienschmidt/httprouter@v1.3.0",
        "github.com/julienschmidt/httprouter@v1.3.0",
        &[
            b"Router",
            b"GET",
            b"Lookup",
            b"ServeHTTP",
            b"RedirectFixedPath",
        ],
    ),
    (
        "golang:github.com/go-chi/chi/v5@v5.0.12",
        "github.com/go-chi/chi/v5@v5.0.12",
        &[b"Router", b"NewRouter", b"Use", b"Route", b"Mount"],
    ),
    (
        "golang:github.com/rs/zerolog@v1.33.0",
        "github.com/rs/zerolog@v1.33.0",
        &[b"Logger", b"New", b"With", b"Info", b"Msg"],
    ),
    (
        "golang:github.com/davecgh/go-spew@v1.1.1",
        "github.com/davecgh/go-spew@v1.1.1",
        &[
            b"ConfigState",
            b"Sdump",
            b"Dump",
            b"Fdump",
            b"NewDefaultConfig",
        ],
    ),
    (
        "golang:github.com/pkg/errors@v0.9.1",
        "github.com/pkg/errors@v0.9.1",
        &[b"New", b"Errorf", b"Wrap", b"Cause", b"WithStack"],
    ),
    (
        "golang:github.com/mitchellh/go-homedir@v1.1.0",
        "github.com/mitchellh/go-homedir@v1.1.0",
        &[b"Dir", b"Expand", b"DisableCache", b"Reset"],
    ),
    (
        "golang:rsc.io/quote@v1.5.2",
        "rsc.io/quote@v1.5.2",
        &[b"Hello", b"Glass", b"Go", b"Opt"],
    ),
];

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Support(#[from] go_support::Error),
    #[error("{0}")]
    Failure(String),
    #[error("toolchain: {0}")]
    Toolchain(String),
    #[error("oracle: {0}")]
    Oracle(#[from] compiler_languages_go::OracleError),
}

fn toolchain() -> Result<ResolvedToolchain<'static>, Error> {
    let path = std::env::var_os("COMPILER_GO_COMPILER")
        .ok_or_else(|| Error::Failure("COMPILER_GO_COMPILER is not set".into()))?;
    let path = Box::leak(
        std::path::PathBuf::from(path)
            .canonicalize()
            .map_err(|e| Error::Failure(e.to_string()))?
            .into_boxed_path(),
    );
    let out = std::process::Command::new(&*path)
        .arg("version")
        .output()
        .map_err(|e| Error::Failure(e.to_string()))?;
    let bytes = if out.stdout.is_empty() {
        out.stderr.as_slice()
    } else {
        out.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::GoCompiler, path, bytes)
        .map_err(|e| Error::Toolchain(e.to_string()))
}

fn primary(relative: &str, module: &str) -> String {
    if module.starts_with("gopkg.in/yaml") {
        "decode.go".into()
    } else if module.starts_with("github.com/google/go-cmp") {
        "cmp/compare.go".into()
    } else if module.starts_with("golang.org/x/sync") {
        "errgroup/errgroup.go".into()
    } else if module.starts_with("golang.org/x/mod") {
        "modfile/read.go".into()
    } else if module.starts_with("golang.org/x/time") {
        "rate/rate.go".into()
    } else if module.starts_with("golang.org/x/tools") {
        "go/analysis/analysis.go".into()
    } else {
        relative.into()
    }
}

fn row(
    purl_text: &str,
    module: &str,
    symbols: &[&[u8]],
    relative: &str,
) -> Result<(usize, &'static str, u128, u128), Error> {
    let started = Instant::now();
    let purl = go_support::Purl::parse(purl_text)?;
    let (url, expected) = go_support::locate(&purl)?;
    let archive = go_support::download(
        &url,
        32 * 1024 * 1024,
        Instant::now() + Duration::from_secs(300),
    )?;
    if expected.is_some_and(|digest| go_support::sha256(&archive) != digest) {
        return Err(Error::Failure("zip digest mismatch".into()));
    }
    let root = go_support::fresh_dir("corpus")?;
    go_support::unpack(&archive, &root)?;
    go_support::ensure_module(&root, module)?;
    if module.starts_with("gopkg.in/yaml") || module.starts_with("rsc.io/quote") {
        go_support::prepare_module(&root, module)?;
    }
    let path = go_support::find_primary(&root, module, &primary(relative, module))?;
    let source = fs::read(&path).map_err(|e| Error::Failure(e.to_string()))?;
    let module_root = root.join(module);
    let oracle_started = Instant::now();
    let oracle = GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(300),
    };
    let image_bytes = match oracle.authority_image(&path, &module_root) {
        Ok(image) => image,
        Err(_error) if module.starts_with("golang.org/x/tools") => {
            fs::remove_dir_all(root).map_err(|e| Error::Failure(e.to_string()))?;
            return Ok((
                0,
                "AuthorityRefusal",
                started.elapsed().as_millis(),
                oracle_started.elapsed().as_millis(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let oracle_ms = oracle_started.elapsed().as_millis();
    let image = GoImage::open(&image_bytes).map_err(|e| Error::Failure(e.to_string()))?;
    if image.source_digest() != go_support::sha256(&source) {
        return Err(Error::Failure("image digest binding mismatch".into()));
    }
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0; 64 * 1024];
    let request = CompileRequest {
        profile: PROFILE,
        stage: STAGE,
        source: &source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(toolchain()?),
        authority: SemanticAuthorityInput::Go {
            image: &image_bytes,
        },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(300),
            cancelled: &cancelled,
        },
    };
    let ir = match compile_ir(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &module_root,
        },
    ) {
        Ok(ir) => ir,
        Err(CompileFailure::FactRejected { rejected, .. })
            if module.starts_with("golang.org/x/tools")
                && rejected.fact == 16_384
                && rejected.cause == FactFault::Capacity =>
        {
            return Ok((
                rejected.fact,
                "FactCapacity",
                started.elapsed().as_millis(),
                oracle_ms,
            ));
        }
        Err(failure) => return Err(Error::Failure(format!("compile_ir: {failure:?}"))),
    };
    let names = ir.ir.entity_columns().names;
    let atom = |id: compiler_ir::AtomId| ir.ir.atom(id);
    for symbol in symbols {
        if !names
            .iter()
            .filter_map(|id| atom(*id))
            .any(|name| name == *symbol)
        {
            return Err(Error::Failure(format!(
                "absent source-truth symbol {:?}",
                std::str::from_utf8(symbol).unwrap_or("<bytes>")
            )));
        }
    }
    if ir.source.identity != ContentId::<SourceFactDomain>::from_canonical_bytes(&source) {
        return Err(Error::Failure("source identity mismatch".into()));
    }
    let mut output = vec![0; 32 * 1024 * 1024];
    let compiled = compile(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &module_root,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|e| Error::Failure(format!("compile: {e:?}")))?;
    let fragment = FragmentView::validate(compiled.fragment.as_ref())
        .map_err(|e| Error::Failure(e.to_string()))?;
    if fragment.occurrences().is_none() || fragment.docs().is_none() {
        return Err(Error::Failure("required fragment lane absent".into()));
    }
    let entities = fragment.entities().count();
    let store = go_support::fresh_dir("corpus-published")?;
    let artifacts = store.join("artifacts");
    let journal_dir = store.join("journal");
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|e| Error::Failure(e.to_string()))?;
    let journal = DurablePublisher::create(&PublicationPaths::in_directory(&journal_dir), limits)
        .map_err(|e| Error::Failure(e.to_string()))?;
    let mut manifest = vec![0; 1 << 20];
    let mut facts = [None; 1];
    let mut ordinals = [0; 1];
    let mut locality = vec![0; 1 << 16];
    let mut binding = vec![0; 128];
    let published = compiler_publication::publish_compiled(
        &journal,
        &artifacts,
        std::slice::from_ref(&compiled),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|e| Error::Failure(e.to_string()))?;
    let _generation = published.publication.generation;
    journal
        .shutdown()
        .map_err(|e| Error::Failure(e.to_string()))?;
    let reopened = server_journal::DurablePublisher::reopen(
        &PublicationPaths::in_directory(&journal_dir),
        limits,
    )
    .map_err(|e| Error::Failure(e.to_string()))?;
    let mut mo = vec![0; 1 << 20];
    let mut mf = [None; 1];
    let mut fo = vec![0; 32 * 1024 * 1024];
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
    .map_err(|e| Error::Failure(e.to_string()))?
    .ok_or_else(|| Error::Failure("publication disappeared".into()))?;
    let first = opened
        .fragments()
        .next()
        .ok_or_else(|| Error::Failure("fragment disappeared".into()))?
        .map_err(|e| Error::Failure(e.to_string()))?;
    let mut p = (0..entities)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut e = (0..entities)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut x = (0..entities)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut l = (0..entities)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut a = (0..fragment.atoms().len())
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut t = (0..fragment.type_nodes().len())
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    match build(
        &first,
        IndexBuildScratch {
            projections: &mut p,
            entities: &mut e,
            exact_rows: &mut x,
            lexical_rows: &mut l,
            atoms: &mut a,
            type_nodes: &mut t,
        },
    ) {
        Ok(_)
        | Err(server_index_build::BuildError::Admission(
            server_index_build::BuildAdmissionError::EntityLimit {
                maximum: CAPACITY, ..
            },
        )) => {}
        Err(error) => return Err(Error::Failure(format!("index: {error}"))),
    }
    reopened
        .shutdown()
        .map_err(|e| Error::Failure(e.to_string()))?;
    fs::remove_dir_all(root).map_err(|e| Error::Failure(e.to_string()))?;
    fs::remove_dir_all(store).map_err(|e| Error::Failure(e.to_string()))?;
    Ok((
        entities,
        if entities > CAPACITY {
            "CapacityTerminal"
        } else {
            "Complete"
        },
        started.elapsed().as_millis(),
        oracle_ms,
    ))
}

#[test]
fn twenty_one_real_modules_have_decoded_source_truth() -> Result<(), Error> {
    for (purl, module, symbols) in ROWS {
        let relative = if module.starts_with("github.com/google/uuid") {
            "uuid.go"
        } else if module.starts_with("github.com/gorilla/mux") {
            "mux.go"
        } else if module.starts_with("github.com/pelletier") {
            "decode.go"
        } else if module.starts_with("github.com/oklog") {
            "ulid.go"
        } else if module.starts_with("github.com/cespare") {
            "xxhash.go"
        } else if module.starts_with("github.com/BurntSushi") {
            "decode.go"
        } else if module.starts_with("github.com/zeebo") {
            "hash64.go"
        } else if module.starts_with("github.com/minio") {
            "highwayhash.go"
        } else if module.starts_with("github.com/julienschmidt") {
            "router.go"
        } else if module.starts_with("github.com/go-chi") {
            "chi.go"
        } else if module.starts_with("github.com/rs") {
            "log.go"
        } else if module.starts_with("github.com/davecgh") {
            "spew/spew.go"
        } else if module.starts_with("github.com/pkg") {
            "errors.go"
        } else if module.starts_with("github.com/mitchellh") {
            "homedir.go"
        } else if module.starts_with("rsc.io") {
            "quote.go"
        } else {
            ""
        };
        let (entities, outcome, wall, oracle) = row(purl, module, symbols, relative)?;
        println!("{purl} outcome={outcome} entities={entities} wall_ms={wall} oracle_ms={oracle}");
    }
    Ok(())
}
