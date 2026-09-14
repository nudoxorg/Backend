#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod typescript_support;

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_semantic::ir::FragmentView;
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use std::{
    fs,
    mem::MaybeUninit,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
const STAGE: Stage = Stage::LowerIr;
const SOURCE: &[u8] = include_bytes!("../../../frontends/typescript/tests/fixtures/source.ts");
const GOLDEN: &[u8] =
    include_bytes!("../../../frontends/typescript/tests/transcripts/golden.json");

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(#[from] typescript_support::Error),
    #[error("filesystem failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("toolchain failed: {source}")]
    Toolchain {
        #[source]
        source: compiler_driver::ToolchainResolutionError,
    },
    #[error("compile failed: {cause}")]
    Compile { cause: String },
    #[error("fragment failed: {source}")]
    Fragment {
        #[source]
        source: backend_semantic::ir::FragmentError,
    },
    #[error("publication failed: {cause}")]
    Publication { cause: String },
    #[error("index failed: {cause}")]
    Index { cause: String },
    #[error("law failed: {0}")]
    Law(&'static str),
}
fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

fn toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let path = std::env::var_os("COMPILER_TYPESCRIPT_COMPILER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|p| p.join("tsc"))
                .find(|p| p.is_file())
        })
        .or_else(|| {
            Some(PathBuf::from(
                "/Users/mileswirht/Downloads/backend/node_modules/.bin/tsc",
            ))
            .filter(|p| p.is_file())
        })
        .ok_or(TestError::Law("tsc executable was not found"))?;
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
    ResolvedToolchain::from_version(NativeTool::TypeScriptCompiler, path, version)
        .map_err(|source| TestError::Toolchain { source })
}

fn compile_source<'a>(
    source: &'a [u8],
    tool: &ResolvedToolchain<'_>,
    output: &'a mut [u8],
    work: &Path,
) -> Result<compiler_driver::CompiledFragment<'a>, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 8192];
    compile(
        CompileRequest {
            profile: PROFILE,
            stage: STAGE,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(*tool),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + typescript_support::NETWORK_DEADLINE,
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
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    })
}

fn publish(
    publisher: &DurablePublisher,
    artifacts: &Path,
    fragment: &compiler_driver::CompiledFragment<'_>,
    scratch: &mut [u8],
) -> Result<compiler_publication::PublishedCompilation, TestError> {
    let mut facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut locality = vec![0_u8; 1 << 16];
    let mut binding = [0_u8; compiler_publication::binding::COMPILATION_BINDING_BYTES];
    publish_compiled(
        publisher,
        artifacts,
        std::slice::from_ref(fragment),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: scratch,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|cause| TestError::Publication {
        cause: cause.to_string(),
    })
}

fn lifecycle_body(purl_text: &str) -> Result<(usize, usize), TestError> {
    let purl = typescript_support::Purl::parse(purl_text)?;
    let url = typescript_support::locate(&purl)?;
    let archive = typescript_support::download(
        &url,
        typescript_support::ARCHIVE_CAP,
        Instant::now() + typescript_support::NETWORK_DEADLINE,
    )?;
    eprintln!(
        "purl={} url={} sha256={}",
        purl_text,
        url,
        typescript_support::hex(&typescript_support::sha256(&archive))
    );
    let root = typescript_support::fresh_dir("lifecycle")?;
    typescript_support::unpack(&archive, &root)?;
    let package = typescript_support::package_root(&root, &purl.name)?;
    let entry = typescript_support::entry(&package)?;
    eprintln!(
        "entry={} lockfile={}",
        entry.display(),
        package.join("package-lock.json").is_file()
    );
    let source = fs::read(&entry).map_err(io)?;
    let tool = toolchain()?;
    let work = typescript_support::fresh_dir("work")?;
    let mut first_bytes = vec![0_u8; 16 * 1024 * 1024];
    let first = compile_source(&source, &tool, &mut first_bytes, &work)?;
    let first_generation;
    let store = typescript_support::fresh_dir("publication")?;
    let artifacts = store.join("artifacts");
    let journal_dir = store.join("journal");
    fs::create_dir_all(&artifacts).map_err(io)?;
    fs::create_dir_all(&journal_dir).map_err(io)?;
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|cause| TestError::Publication {
        cause: cause.to_string(),
    })?;
    let publisher = DurablePublisher::create(&PublicationPaths::in_directory(&journal_dir), limits)
        .map_err(|cause| TestError::Publication {
            cause: cause.to_string(),
        })?;
    let mut manifest = vec![0_u8; 1 << 20];
    let published = publish(&publisher, &artifacts, &first, &mut manifest)?;
    first_generation = published.publication.generation;
    let mut modified = source.clone();
    modified.extend_from_slice(b"\nexport const nudoxLifecycleGeneration = true;\n");
    let mut second_bytes = vec![0_u8; 16 * 1024 * 1024];
    let second = compile_source(&modified, &tool, &mut second_bytes, &work)?;
    let second_published = publish(&publisher, &artifacts, &second, &mut manifest)?;
    if second_published.publication.generation == first_generation {
        return Err(TestError::Law(
            "generation did not advance in the same journal",
        ));
    }
    let mut m = vec![0_u8; 1 << 20];
    let mut mf = [None; 1];
    let mut fragments = vec![0_u8; 16 * 1024 * 1024];
    let mut locality = vec![0_u8; 1 << 16];
    let reopened = open_published(
        &publisher,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut m,
            manifest_facts: &mut mf,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )
    .map_err(|cause| TestError::Publication {
        cause: cause.to_string(),
    })?
    .ok_or(TestError::Law("journal reopen returned no publication"))?;
    let fragment = reopened
        .fragments()
        .next()
        .ok_or(TestError::Law("publication contains no fragment"))?
        .map_err(|cause| TestError::Publication {
            cause: cause.to_string(),
        })?;
    let view = &fragment.view;
    let count = view.entities().len();
    let atoms = view.atoms().len();
    let types = view.type_nodes().len();
    let mut projections = (0..count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut entities = (0..count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut exact = (0..count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut lexical = (0..count)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut atom_slots = (0..atoms)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut type_slots = (0..types)
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let prepared = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atom_slots,
            type_nodes: &mut type_slots,
        },
    )
    .map_err(|cause| TestError::Index {
        cause: cause.to_string(),
    })?;
    let mut exact_ids = [prepared.exact.id];
    let mut lexical_ids = [prepared.lexical.id];
    let sealed = seal_compilation_index(
        reopened,
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
    let mut old_bytes = vec![0_u8; 16 * 1024 * 1024];
    let old = ImmutableArtifactStore::new(&artifacts)
        .map_err(|cause| TestError::Publication {
            cause: cause.to_string(),
        })?
        .open(fragment.facts, &mut old_bytes)
        .map_err(|cause| TestError::Publication {
            cause: cause.to_string(),
        })?;
    FragmentView::validate(old.as_ref()).map_err(|source| TestError::Fragment { source })?;
    backend_frontend_typescript::legacy::Checker::default()
        .decode(GOLDEN)
        .map_err(|cause| {
            TestError::Law(Box::leak(
                format!("golden decode: {cause}").into_boxed_str(),
            ))
        })?;
    publisher
        .shutdown()
        .map_err(|cause| TestError::Publication {
            cause: cause.to_string(),
        })?;
    fs::remove_dir_all(root).map_err(io)?;
    fs::remove_dir_all(work).map_err(io)?;
    fs::remove_dir_all(store).map_err(io)?;
    eprintln!(
        "generations first={:?} second={:?}",
        first_generation, second_published.publication.generation
    );
    eprintln!(
        "index_rows exact={} lexical={}",
        prepared.exact.rows.len(),
        prepared.lexical.rows.len()
    );
    Ok((prepared.exact.rows.len(), prepared.lexical.rows.len()))
}

fn lifecycle(purl_text: &str) -> Result<(usize, usize), TestError> {
    let purl = purl_text.to_owned();
    let result = std::thread::Builder::new()
        .name("typescript-lifecycle".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || lifecycle_body(&purl))
        .map_err(io)?
        .join()
        .map_err(|_| TestError::Law("lifecycle exceeded the 2 MiB thread stack"))?;
    eprintln!("stack_size=2097152 falsifier=ok");
    result
}

#[test]
fn npm_purls_reject_malformed_and_build_exact_urls() -> Result<(), TestError> {
    let lodash = typescript_support::Purl::parse("npm:lodash@4.17.21")?;
    let babel = typescript_support::Purl::parse("npm:@babel/parser@7.26.8")?;
    if typescript_support::locate(&lodash)?
        != "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz"
        || typescript_support::locate(&babel)?
            != "https://registry.npmjs.org/@babel/parser/-/parser-7.26.8.tgz"
    {
        return Err(TestError::Law("npm tarball URL convention changed"));
    }
    match typescript_support::Purl::parse("npm:lodash") {
        Err(typescript_support::Error::Purl { input }) if input == "npm:lodash" => Ok(()),
        _ => Err(TestError::Law("missing npm version was not retained")),
    }
}

#[test]
fn scoped_typescript_package_survives_two_generations_index_and_golden_decode()
-> Result<(), TestError> {
    let (exact, lexical) = lifecycle("npm:@babel/parser@7.26.8")?;
    eprintln!("babel index_rows exact={} lexical={}", exact, lexical);
    Ok(())
}

#[test]
fn single_package_typescript_purl_survives_two_generations_index_and_golden_decode()
-> Result<(), TestError> {
    let (exact, lexical) = lifecycle("npm:zod@3.25.76")?;
    eprintln!("zod index_rows exact={} lexical={}", exact, lexical);
    Ok(())
}

#[test]
fn local_build_artifact_leg_requires_the_build_step() -> Result<(), TestError> {
    let root = typescript_support::fresh_dir("build-leg")?;
    fs::write(
        root.join("package.json"),
        br#"{"name":"nudox-build-fixture","version":"1.0.0","scripts":{"build":"node build.js"}}"#,
    )
    .map_err(io)?;
    fs::write(
        root.join("build.js"),
        b"require('fs').writeFileSync('index.d.ts', 'export declare const built: true;\\n')",
    )
    .map_err(io)?;
    match typescript_support::entry(&root) {
        Err(typescript_support::Error::MissingTypes { .. }) => {}
        _ => {
            return Err(TestError::Law(
                "build fixture unexpectedly had declarations before build",
            ));
        }
    }
    let status = std::process::Command::new("node")
        .arg("build.js")
        .current_dir(&root)
        .status()
        .map_err(io)?;
    if !status.success() {
        return Err(TestError::Law("local build fixture failed"));
    }
    let entry = typescript_support::entry(&root)?;
    if entry != root.join("index.d.ts") {
        return Err(TestError::Law(
            "build fixture resolved the wrong declaration entry",
        ));
    }
    eprintln!(
        "build_leg package_sha256={} entry={} build_status={}",
        typescript_support::hex(&typescript_support::sha256(
            &fs::read(root.join("package.json")).map_err(io)?
        )),
        entry.display(),
        status
    );
    fs::remove_dir_all(root).map_err(io)
}
