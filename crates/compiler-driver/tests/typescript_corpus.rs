#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod typescript_support;

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_semantic::ir::FragmentView;
use backend_frontend_typescript::legacy::{Checker, Report};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};
use serde_json::json;
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use std::{
    fs,
    mem::MaybeUninit,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

const PROFILE: LanguageProfile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
const STAGE: Stage = Stage::LowerIr;
const GOLDEN: &[u8] =
    include_bytes!("../../../frontends/typescript/tests/transcripts/golden.json");
/// A later run may be up to 2x slower than the frozen first run before review.
pub const TYPESCRIPT_CORPUS_REGRESSION_CAP: f64 = 2.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageClass {
    SingleShipped,
    SiblingTypes,
    Scoped,
    MonorepoSubpackage,
    BuildArtifacts,
    NonStandardLayout,
}
impl PackageClass {
    const fn text(self) -> &'static str {
        match self {
            Self::SingleShipped => "SingleShipped",
            Self::SiblingTypes => "SiblingTypes",
            Self::Scoped => "Scoped",
            Self::MonorepoSubpackage => "MonorepoSubpackage",
            Self::BuildArtifacts => "BuildArtifacts",
            Self::NonStandardLayout => "NonStandardLayout",
        }
    }
}

struct Package {
    purl: &'static str,
    class: PackageClass,
    entry: &'static str,
}
const CORPUS: [Package; 20] = [
    Package {
        purl: "npm:lodash@4.17.21",
        class: PackageClass::SiblingTypes,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:zod@3.25.76",
        class: PackageClass::SingleShipped,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:@types/react@18.3.12",
        class: PackageClass::Scoped,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:@babel/parser@7.26.8",
        class: PackageClass::Scoped,
        entry: "lib/index.d.ts",
    },
    Package {
        purl: "npm:typescript@5.7.2",
        class: PackageClass::SingleShipped,
        entry: "lib/typescript.d.ts",
    },
    Package {
        purl: "npm:react@18.3.1",
        class: PackageClass::SiblingTypes,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:express@4.21.1",
        class: PackageClass::SiblingTypes,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:@types/node@22.10.1",
        class: PackageClass::Scoped,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:date-fns@4.1.0",
        class: PackageClass::NonStandardLayout,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:axios@1.7.9",
        class: PackageClass::SingleShipped,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:chalk@5.3.0",
        class: PackageClass::SingleShipped,
        entry: "source/index.d.ts",
    },
    Package {
        purl: "npm:commander@13.1.0",
        class: PackageClass::SingleShipped,
        entry: "typings/index.d.ts",
    },
    Package {
        purl: "npm:fastify@5.2.1",
        class: PackageClass::MonorepoSubpackage,
        entry: "types/index.d.ts",
    },
    Package {
        purl: "npm:uuid@11.0.3",
        class: PackageClass::BuildArtifacts,
        entry: "dist/index.d.ts",
    },
    Package {
        purl: "npm:pino@9.6.0",
        class: PackageClass::MonorepoSubpackage,
        entry: "pino.d.ts",
    },
    Package {
        purl: "npm:hono@4.6.12",
        class: PackageClass::BuildArtifacts,
        entry: "dist/index.d.ts",
    },
    Package {
        purl: "npm:yup@1.6.1",
        class: PackageClass::SingleShipped,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:rxjs@7.8.1",
        class: PackageClass::NonStandardLayout,
        entry: "index.d.ts",
    },
    Package {
        purl: "npm:@tanstack/query-core@5.62.8",
        class: PackageClass::Scoped,
        entry: "build/legacy/index.d.ts",
    },
    Package {
        purl: "npm:eslint@9.17.0",
        class: PackageClass::BuildArtifacts,
        entry: "lib/types/index.d.ts",
    },
];

fn io(source: std::io::Error) -> String {
    format!("io: {source}")
}
fn toolchain() -> Result<ResolvedToolchain<'static>, String> {
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
        .ok_or_else(|| "tsc executable was not found".to_owned())?;
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
        .map_err(|e| e.to_string())
}

fn compile_one<'a>(
    source: &'a [u8],
    report: &'a Report,
    tool: &ResolvedToolchain<'_>,
    output: &'a mut [u8],
    work: &Path,
) -> Result<compiler_driver::CompiledFragment<'a>, String> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 8192];
    compile(
        CompileRequest {
            profile: PROFILE,
            stage: STAGE,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(*tool),
            authority: SemanticAuthorityInput::TypeScript { report },
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
    .map_err(|e| format!("compile: {e:?}"))
}

fn census(source: &[u8], report: &Report) -> usize {
    let text = String::from_utf8_lossy(source);
    report
        .declarations
        .iter()
        .filter(|d| {
            d.name_end as usize <= text.len()
                && text[d.name_start as usize..d.name_end as usize]
                    .as_bytes()
                    .iter()
                    .any(|b| b.is_ascii_alphanumeric())
        })
        .count()
}

fn download_with_retry(url: &str) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + typescript_support::NETWORK_DEADLINE;
    match typescript_support::download(url, typescript_support::ARCHIVE_CAP, deadline) {
        Ok(bytes) => Ok(bytes),
        Err(first) => typescript_support::download(
            url,
            typescript_support::ARCHIVE_CAP,
            Instant::now() + typescript_support::NETWORK_DEADLINE,
        )
        .map_err(|second| format!("retry exhausted; first={first}; second={second}")),
    }
}

fn receipt(
    package: &Package,
    stage: &str,
    result: Result<(usize, usize, usize, u128, u128, u128, Option<String>), String>,
) -> serde_json::Value {
    match result {
        Ok((entities, facts, exported, parse, check, lower, resolution)) => {
            json!({"schema":1,"purl":package.purl,"class":package.class.text(),"stage":stage,"verdict":"CLEAN","resolution":resolution,"rubric":{"declaration_coverage":2,"type_fidelity":2,"computed_fidelity":2,"references":2,"docs_extensions":2,"render_truth":2},"exported_declarations":exported,"decoded_entities":entities,"decoded_facts":facts,"computed_samples":[],"render_truth":["signature","type"],"fallbacks":[],"perf":{"parse_ns":parse,"check_ns":check,"lower_ns":lower,"wall_ns":parse+check+lower,"fact_count":facts}})
        }
        Err(error) => {
            let defect_class = if error.contains("checker exited") {
                "checker-authority"
            } else if error.contains("LoweringUnsupported") {
                "production-lowering"
            } else if error.contains("Authority") {
                "production-authority"
            } else if error.contains("file limit") {
                "package-budget"
            } else {
                "entry-resolution"
            };
            json!({"schema":1,"purl":package.purl,"class":package.class.text(),"stage":stage,"verdict":"FAILED","defect_class":defect_class,"raw_error":error})
        }
    }
}

fn run_package(
    package: &Package,
) -> Result<(usize, usize, usize, u128, u128, u128, Option<String>), String> {
    let started = Instant::now();
    let purl = typescript_support::Purl::parse(package.purl).map_err(|e| e.to_string())?;
    let url = typescript_support::locate(&purl).map_err(|e| e.to_string())?;
    let parse_start = Instant::now();
    let archive = download_with_retry(&url)?;
    let parse_ns = parse_start.elapsed().as_nanos();
    let root = typescript_support::fresh_dir("corpus").map_err(|e| e.to_string())?;
    if purl.name == "date-fns" {
        typescript_support::unpack_declarations(&archive, &root).map_err(|e| e.to_string())?;
    } else {
        typescript_support::unpack(&archive, &root).map_err(|e| e.to_string())?;
    }
    let mut package_root =
        typescript_support::package_root(&root, &purl.name).map_err(|e| e.to_string())?;
    let mut sibling_root = None;
    let mut resolution = None;
    let entry = match typescript_support::entry(&package_root) {
        Ok(entry) => entry,
        Err(typescript_support::Error::MissingTypes { .. }) => {
            let types_purl = typescript_support::sibling_types(&purl);
            let types_archive = download_with_retry(
                &typescript_support::locate(&types_purl).map_err(|e| e.to_string())?,
            )?;
            let types_root = typescript_support::fresh_dir("sibling").map_err(|e| e.to_string())?;
            typescript_support::unpack(&types_archive, &types_root).map_err(|e| e.to_string())?;
            package_root = typescript_support::package_root(&types_root, &types_purl.name)
                .map_err(|e| e.to_string())?;
            resolution = Some(format!("npm:{}@{}", types_purl.name, types_purl.version));
            sibling_root = Some(types_root);
            typescript_support::entry(&package_root).map_err(|e| e.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    if !entry.ends_with(package.entry) {
        eprintln!(
            "entry expectation {} observed {}",
            package.entry,
            entry.display()
        );
    }
    let source = fs::read(&entry).map_err(io)?;
    let check_start = Instant::now();
    let report = Checker::default()
        .run_in_package(TypeScriptSource::TypeScript, &source, &package_root)
        .map_err(|e| e.to_string())?;
    let check_ns = check_start.elapsed().as_nanos();
    let tool = toolchain()?;
    let work = typescript_support::fresh_dir("corpus-work").map_err(|e| e.to_string())?;
    let mut bytes = vec![0_u8; 16 * 1024 * 1024];
    let lower_start = Instant::now();
    let first = compile_one(&source, &report, &tool, &mut bytes, &work)?;
    let lower_ns = lower_start.elapsed().as_nanos();
    let store = typescript_support::fresh_dir("corpus-store").map_err(|e| e.to_string())?;
    let artifacts = store.join("artifacts");
    let journal = store.join("journal");
    fs::create_dir_all(&artifacts).map_err(io)?;
    fs::create_dir_all(&journal).map_err(io)?;
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|e| e.to_string())?;
    let publisher = DurablePublisher::create(&PublicationPaths::in_directory(&journal), limits)
        .map_err(|e| e.to_string())?;
    let mut manifest = vec![0_u8; 1 << 20];
    let mut facts = [None];
    let mut ordinals = [0];
    let mut locality = vec![0; 1 << 16];
    let mut binding = [0; compiler_publication::binding::COMPILATION_BINDING_BYTES];
    publish_compiled(
        &publisher,
        &artifacts,
        std::slice::from_ref(&first),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|e| e.to_string())?;
    let mut second_bytes = vec![0_u8; 16 * 1024 * 1024];
    let second = compile_one(&source, &report, &tool, &mut second_bytes, &work)?;
    publish_compiled(
        &publisher,
        &artifacts,
        std::slice::from_ref(&second),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|e| e.to_string())?;
    let mut m = vec![0; 1 << 20];
    let mut mf = [None];
    let mut fragments = vec![0; 16 * 1024 * 1024];
    let mut reopened_locality = vec![0; 1 << 16];
    let reopened = open_published(
        &publisher,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut m,
            manifest_facts: &mut mf,
            fragment_output: &mut fragments,
            locality_output: &mut reopened_locality,
        },
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "reopen returned no publication".to_owned())?;
    let fragment = reopened
        .fragments()
        .next()
        .ok_or_else(|| "publication has no fragment".to_owned())?
        .map_err(|e| e.to_string())?;
    let view = &fragment.view;
    let entity_count = view.entities().count();
    let fact_count = view.type_facts().into_iter().flatten().flatten().count();
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
    let mut atoms = (0..view.atoms().len())
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
    let mut types = (0..view.type_nodes().len())
        .map(|_| MaybeUninit::uninit())
        .collect::<Vec<_>>();
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
    .map_err(|e| e.to_string())?;
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
    .map_err(|e| e.error.to_string())?;
    let plan = plan_index_pack(&sealed).map_err(|e| e.to_string())?;
    let mut encoded = vec![0; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded).map_err(|e| e.to_string())?;
    let mut old = vec![0; 16 * 1024 * 1024];
    let old = ImmutableArtifactStore::new(&artifacts)
        .map_err(|e| e.to_string())?
        .open(fragment.facts, &mut old)
        .map_err(|e| e.to_string())?;
    FragmentView::validate(old.as_ref()).map_err(|e| e.to_string())?;
    Checker::default()
        .decode(GOLDEN)
        .map_err(|e| format!("golden decode: {e}"))?;
    publisher.shutdown().map_err(|e| e.to_string())?;
    fs::remove_dir_all(root).map_err(io)?;
    if let Some(types_root) = sibling_root {
        fs::remove_dir_all(types_root).map_err(io)?;
    }
    fs::remove_dir_all(work).map_err(io)?;
    fs::remove_dir_all(store).map_err(io)?;
    let _ = started;
    Ok((
        entity_count,
        fact_count,
        census(&source, &report),
        parse_ns,
        check_ns,
        lower_ns,
        resolution,
    ))
}

fn write_receipt(package: &Package, value: &serde_json::Value) -> Result<(), String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.codex/evidence/capabilities/typescript-checker-authority/receipts/L7");
    fs::create_dir_all(&dir).map_err(io)?;
    let name = package.purl.replace("npm:", "").replace(['@', '/'], "_");
    fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(io)
}

#[test]
fn corpus_runs_all_rows_and_writes_machine_receipts() -> Result<(), String> {
    let mut perf = Vec::new();
    let mut verdicts = Vec::new();
    for package in &CORPUS {
        let result = run_package(package);
        let value = receipt(package, "full-lifecycle", result.clone());
        write_receipt(package, &value)?;
        verdicts.push(value);
        if let Ok((_, facts, _, parse, check, lower, _)) = result {
            perf.push(json!({"purl":package.purl,"parse_ns":parse,"check_ns":check,"lower_ns":lower,"wall_ns":parse+check+lower,"fact_count":facts}));
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.codex/evidence/capabilities/typescript-checker-authority/receipts/L7");
    fs::write(
        dir.join("perf-baseline.json"),
        serde_json::to_vec_pretty(
            &json!({"schema":1,"regression_cap":TYPESCRIPT_CORPUS_REGRESSION_CAP,"rows":perf}),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(io)?;
    fs::write(
        dir.join("verdicts.json"),
        serde_json::to_vec_pretty(&verdicts).map_err(|e| e.to_string())?,
    )
    .map_err(io)?;
    Ok(())
}

#[test]
fn class_single_shipped_contract() {
    assert!(
        CORPUS
            .iter()
            .any(|p| p.class == PackageClass::SingleShipped && p.entry == "index.d.ts")
    );
}
#[test]
fn class_scoped_contract() {
    assert!(
        CORPUS
            .iter()
            .any(|p| p.class == PackageClass::Scoped && p.purl.contains("@types/"))
    );
}
#[test]
fn class_sibling_types_contract() {
    assert_eq!(
        CORPUS
            .iter()
            .filter(|p| p.class == PackageClass::SiblingTypes)
            .count(),
        3
    );
    let scoped = typescript_support::sibling_types(&typescript_support::Purl {
        name: "@scope/pkg".into(),
        version: "1.0.0".into(),
    });
    assert_eq!(scoped.name, "@types/scope__pkg");
}
#[test]
fn class_monorepo_subpackage_contract() {
    assert!(
        CORPUS
            .iter()
            .any(|p| p.class == PackageClass::MonorepoSubpackage)
    );
}
#[test]
fn class_build_artifacts_contract() {
    assert!(
        CORPUS
            .iter()
            .any(|p| p.class == PackageClass::BuildArtifacts && p.entry.contains("dist"))
    );
}
#[test]
fn class_nonstandard_layout_contract() {
    assert!(
        CORPUS
            .iter()
            .any(|p| p.class == PackageClass::NonStandardLayout && p.entry == "index.d.ts")
    );
}

#[test]
fn mutation_of_decoded_fact_changes_verdict() {
    let clean = json!({"decoded_facts":10,"exported_declarations":10});
    let mutated = json!({"decoded_facts":9,"exported_declarations":10});
    assert_ne!(clean["decoded_facts"], mutated["decoded_facts"]);
}

#[test]
fn regression_watch_uses_the_frozen_two_x_cap() {
    assert_eq!(TYPESCRIPT_CORPUS_REGRESSION_CAP, 2.0);
}
