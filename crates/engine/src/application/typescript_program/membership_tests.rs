//! Real Compiler API and package-compiler oracles. These tests require explicit pinned tools;
//! an absent receipt remains ignored rather than substituting a fake checker.
use super::{NativeTypeScriptInputs, TypeScriptPackageSourceMembership, build_native_inputs};
use crate::application::{PackageSource, ToolchainProbeLimits, TypeScriptProjectHost};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAIN: &str = "import { answer } from './lib';\nexport function run(): number { return answer({value: 42}); }\n";
const LIB: &str = "import type { Token } from '../types/ambient';\nexport function answer(token: Token): number { return token.value; }\n";
const AMBIENT: &str = "export interface Token { value: number; }\n";
const ESLINT: &str = "export const eslintOnly = { rules: {} };\n";
const POSTCSS: &str = "\u{feff}module.exports = { plugins: {} };\n";
const NEXT: &str = "export const nextOnly = { reactStrictMode: true };\n";

struct Fixture {
    root: PathBuf,
    node: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new() -> Self {
        let node = PathBuf::from(std::env::var_os("TSZ_PROGRAM_TEST_NODE").expect("pinned Node"));
        let api = PathBuf::from(
            std::env::var_os("TSZ_PROGRAM_TEST_TYPESCRIPT_API").expect("pinned Compiler API"),
        );
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "nudox-ts-membership-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        copy_directory(
            api.parent().unwrap().parent().unwrap(),
            &root.join("node_modules/typescript"),
        );
        std::os::unix::fs::symlink("../typescript/bin/tsc", root.join("node_modules/.bin/tsc"))
            .unwrap();
        let root = root.canonicalize().unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("types")).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"program-membership","version":"1.0.0"}"#,
        )
        .unwrap();
        for (path, text) in Self::all_sources()
            .into_iter()
            .chain([("types/ambient.d.ts", AMBIENT)])
        {
            fs::write(root.join(path), text).unwrap();
        }
        let fixture = Self { root, node };
        fixture.config(false, false);
        fixture
    }
    fn all_sources() -> [(&'static str, &'static str); 5] {
        [
            ("eslint.config.ts", ESLINT),
            ("next.config.ts", NEXT),
            ("postcss.config.js", POSTCSS),
            ("src/lib.ts", LIB),
            ("src/main.ts", MAIN),
        ]
    }
    fn sources() -> Vec<PackageSource<'static>> {
        Self::all_sources()
            .into_iter()
            .map(|(path, text)| PackageSource::new(path, text).unwrap())
            .collect()
    }
    fn config(&self, allow_js: bool, include_configs: bool) {
        let includes = if include_configs {
            vec!["src/**/*", "*.ts", "*.js"]
        } else {
            vec!["src/**/*"]
        };
        fs::write(self.root.join("tsconfig.json"), serde_json::to_vec(&serde_json::json!({
            "compilerOptions": {"strict":true,"target":"ES2020","module":"CommonJS","allowJs":allow_js,"checkJs":false},
            "include": includes,
        })).unwrap()).unwrap();
    }
    fn host(&self) -> TypeScriptProjectHost {
        TypeScriptProjectHost::new(None, Some(self.node.clone()), None, None, limits())
    }
    fn native(&self, sources: &[PackageSource<'_>]) -> NativeTypeScriptInputs {
        let admitted = self.host().admit(&self.root).unwrap().unwrap();
        let inputs = admitted.inputs();
        let mut resolver = inputs.resolver();
        let native = build_native_inputs(
            &inputs,
            &mut resolver,
            sources,
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(false),
        )
        .unwrap();
        native
            .closure_witness
            .validate_current(admitted.witness.as_ref())
            .unwrap();
        native
    }
    /// This oracle constructs an ordinary TypeScript program without the production bridge.
    fn oracle(&self) -> serde_json::Value {
        let script = r#"
const ts = require(process.argv[1]);
const config = ts.getParsedCommandLineOfConfigFile(process.argv[2], {}, {...ts.sys, onUnRecoverableConfigFileDiagnostic:d=>{throw Error(String(d.code))}});
const p = ts.createProgram({rootNames:config.fileNames, options:config.options});
const c = p.getTypeChecker();
const calls=[];
for(const f of p.getSourceFiles()) {
  function visit(n) {
    if(ts.isCallExpression(n) && n.expression.getText(f)==='answer') {
      const sig=c.getResolvedSignature(n);
      calls.push({file:f.fileName, result:c.typeToString(c.getTypeAtLocation(n)), declaration:sig.declaration.getSourceFile().fileName});
    }
    ts.forEachChild(n,visit);
  }
  visit(f);
}
process.stdout.write(JSON.stringify({files:p.getSourceFiles().map(f=>f.fileName), formats:Object.fromEntries(p.getSourceFiles().map(f=>[f.fileName, f.impliedNodeFormat === undefined ? 'unknown' : f.impliedNodeFormat === ts.ModuleKind.CommonJS ? 'common_js' : f.impliedNodeFormat === ts.ModuleKind.ESNext ? 'es_module' : 'unsupported'])), calls, errors:ts.getPreEmitDiagnostics(p).filter(d=>d.category===ts.DiagnosticCategory.Error).map(d=>d.code)}));
"#;
        let args = [
            OsString::from(self.root.join("node_modules/typescript/lib/typescript.js")),
            OsString::from(self.root.join("tsconfig.json")),
        ];
        let bytes = crate::application::toolchain_probe::run_typescript_program_bridge(
            &self.node,
            script,
            &args,
            &self.root,
            limits(),
            &AtomicBool::new(false),
        )
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}
fn limits() -> ToolchainProbeLimits {
    ToolchainProbeLimits::new(
        Duration::from_secs(90),
        NonZeroUsize::new(8 * 1024 * 1024).unwrap(),
    )
    .unwrap()
}
fn copy_directory(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(
            !kind.is_symlink(),
            "the pinned compiler package must have regular immutable files"
        );
        if kind.is_dir() {
            copy_directory(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}
fn selected(native: &NativeTypeScriptInputs) -> BTreeSet<&str> {
    native
        .package_membership
        .iter()
        .filter_map(|(path, membership)| {
            matches!(
                membership,
                TypeScriptPackageSourceMembership::Selected { .. }
            )
            .then_some(path.as_ref())
        })
        .collect()
}

#[test]
#[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn configured_membership_matches_independent_compiler_api_without_adding_configs() {
    let fixture = Fixture::new();
    let sources = Fixture::sources();
    let native = fixture.native(&sources);
    assert_eq!(
        selected(&native),
        BTreeSet::from(["src/lib.ts", "src/main.ts"])
    );
    for config in ["eslint.config.ts", "next.config.ts", "postcss.config.js"] {
        assert_eq!(
            native.package_membership[config],
            TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
        );
        assert!(!native.sources.iter().any(|source| source.path == config));
    }
    let oracle = fixture.oracle();
    assert_eq!(oracle["errors"], serde_json::json!([]));
    assert_eq!(
        oracle["calls"],
        serde_json::json!([{ "file":fixture.root.join("src/main.ts"), "result":"number", "declaration":fixture.root.join("src/lib.ts") }])
    );
    // An imported declaration outside include is a real program member. Its absence from
    // the scanner frontier does not remove it or invent a captured-package mapping.
    assert!(
        native
            .sources
            .iter()
            .any(|source| source.path == "types/ambient.d.ts")
    );
    assert!(!native.package_membership.contains_key("types/ambient.d.ts"));
    let actual = native
        .sources
        .iter()
        .map(|source| fixture.root.join(&source.path))
        .chain(native.libraries.iter().map(|library| {
            fixture
                .root
                .join("node_modules/typescript/lib")
                .join(library.file_name.strip_prefix("@compiler/lib.").unwrap())
        }))
        .collect::<BTreeSet<_>>();
    let expected = oracle["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| PathBuf::from(file.as_str().unwrap()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "TSZ gets exactly the independent compiler program"
    );

    fixture.config(false, true);
    let without_js = fixture.native(&sources);
    assert_eq!(
        selected(&without_js),
        BTreeSet::from([
            "eslint.config.ts",
            "next.config.ts",
            "src/lib.ts",
            "src/main.ts"
        ])
    );
    assert_eq!(
        without_js.package_membership["postcss.config.js"],
        TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
    );
    fixture.config(true, true);
    let with_js = fixture.native(&sources);
    assert_eq!(selected(&with_js).len(), sources.len());
    assert_eq!(
        with_js
            .sources
            .iter()
            .find(|source| source.path == "postcss.config.js")
            .unwrap()
            .source,
        POSTCSS,
        "retain exact BOM-bearing source text"
    );
}

#[test]
#[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn configured_empty_and_all_excluded_frontiers_do_not_gain_program_members() {
    let fixture = Fixture::new();
    let sources = Fixture::sources();
    let configs = &sources[..3];
    let native = fixture.native(configs);
    assert!(selected(&native).is_empty());
    assert_eq!(native.package_membership.len(), configs.len());
    assert!(
        native
            .sources
            .iter()
            .any(|source| source.path == "src/main.ts"),
        "the configured program remains independent of the requested profile frontier"
    );
    fs::write(
        fixture.root.join("tsconfig.json"),
        r#"{"compilerOptions":{"allowJs":false},"files":[]}"#,
    )
    .unwrap();
    let empty = fixture.native(&sources);
    assert!(empty.sources.is_empty());
    assert!(empty.libraries.is_empty());
    assert!(selected(&empty).is_empty());
    assert_eq!(empty.package_membership.len(), sources.len());
    assert!(
        empty
            .package_membership
            .values()
            .all(|member| *member
                == TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram)
    );
}

#[test]
#[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn excluded_sources_still_refuse_missing_changed_outside_and_cancelled_inputs() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let admitted = host.admit(&fixture.root).unwrap().unwrap();
    let inputs = admitted.inputs();
    let sources = Fixture::sources();
    let native = build_native_inputs(
        &inputs,
        &mut inputs.resolver(),
        &sources,
        Instant::now() + Duration::from_secs(90),
        &AtomicBool::new(false),
    )
    .unwrap();
    fs::write(
        fixture.root.join("postcss.config.js"),
        "module.exports = {};\n",
    )
    .unwrap();
    assert!(
        native
            .closure_witness
            .validate_current(admitted.witness.as_ref())
            .is_err(),
        "an excluded source remains in the retained resolver closure"
    );

    let fresh = host.admit(&fixture.root).unwrap().unwrap();
    assert!(
        build_native_inputs(
            &fresh.inputs(),
            &mut fresh.inputs().resolver(),
            &sources,
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(false)
        )
        .is_err(),
        "excluded captured bytes cannot mismatch"
    );
    fs::remove_file(fixture.root.join("postcss.config.js")).unwrap();
    let fresh = host.admit(&fixture.root).unwrap().unwrap();
    assert!(
        build_native_inputs(
            &fresh.inputs(),
            &mut fresh.inputs().resolver(),
            &sources,
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(false)
        )
        .is_err(),
        "excluded source cannot disappear"
    );
    assert!(matches!(
        build_native_inputs(
            &inputs,
            &mut inputs.resolver(),
            &sources,
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(true)
        ),
        Err(super::TypeScriptProjectHostError::CompilerProgramCancelled)
    ));
    assert!(matches!(
        build_native_inputs(
            &inputs,
            &mut inputs.resolver(),
            &sources,
            Instant::now(),
            &AtomicBool::new(false)
        ),
        Err(super::TypeScriptProjectHostError::CompilerProgramDeadline)
    ));
    // PackageSource rejects traversal; a symlink to a real source outside admission also refuses.
    assert!(PackageSource::new("../outside.ts", "export {};").is_err());
    let outside = fixture.root.with_extension("outside.ts");
    fs::write(&outside, ESLINT).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("outside.ts")).unwrap();
    let fresh = host.admit(&fixture.root).unwrap().unwrap();
    let outside_source = [PackageSource::new("outside.ts", ESLINT).unwrap()];
    assert!(
        build_native_inputs(
            &fresh.inputs(),
            &mut fresh.inputs().resolver(),
            &outside_source,
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    fs::remove_file(outside).unwrap();
}

#[test]
#[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn configured_package_keeps_selected_semantics_excluded_gaps_and_exact_source_frontier() {
    use crate::application::{
        LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch,
        LocalCompilerTimeout, LocalPackageRootSet, LocalToolchainSet,
        PackageAuthorityConfiguration, PackageSourceCoverageGapCause, PackageSourceSet,
    };
    use backend_library::interface::{
        CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
    };
    use backend_semantic::{
        ir::{
            Confidence, ExternalTarget, LinkKind, LinkTarget, SemanticCoreReader, SemanticReader,
        },
        vocabulary::{LanguageProfile, Stage, TypeScriptSource},
    };
    use backend_store::journal::PublicationLimits;
    use backend_version::{ContentId, Coverage, SourceFactDomain};

    let fixture = Fixture::new();
    let host = fixture.host();
    let tools = [];
    let toolchains = LocalToolchainSet::validate(&tools).unwrap();
    let cancelled = AtomicBool::new(false);
    let artifacts = fixture.root.join("artifacts");
    let journal = fixture.root.join("journal");
    let native_work = fixture.root.join("native-work");
    fs::create_dir(&native_work).unwrap();
    let mut scratch =
        LocalCompilerScratch::with_fragment_capacity(NonZeroUsize::new(16 * 1024 * 1024).unwrap())
            .unwrap();
    let mut compiler = LocalCompiler::create_with_package_authority(
        LocalCompilerConfig {
            toolchains,
            artifact_directory: &artifacts,
            journal_directory: &journal,
            native_work_directory: &native_work,
            control: LocalCompilerControl {
                timeout: LocalCompilerTimeout::new(Duration::from_secs(120)).unwrap(),
                cancelled: &cancelled,
            },
        },
        LocalPackageRootSet::EMPTY,
        PackageAuthorityConfiguration {
            typescript_project_host: Some(&host),
            ..PackageAuthorityConfiguration::UNAVAILABLE
        },
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN).unwrap(),
        &mut scratch,
    )
    .unwrap();
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(137),
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
        },
        PackageUrl::try_from("pkg:npm/program-membership@1.0.0".to_owned()).unwrap(),
    )
    .unwrap();
    let sources = Fixture::sources();
    let package = PackageSourceSet::new(&request, &fixture.root, &sources).unwrap();
    let staged = compiler
        .compile_package_sources_staged(package.clone(), &mut |_| {})
        .unwrap();
    assert_eq!(staged.artifacts().len(), 2);
    assert_eq!(
        staged.artifacts().len() + staged.coverage_gaps().len(),
        sources.len(),
        "every captured source is accounted for exactly once"
    );
    assert_eq!(staged.input_witness().coverage().state(), Coverage::Partial);
    for ((path, text), gap) in Fixture::all_sources()[..3]
        .iter()
        .zip(staged.coverage_gaps())
    {
        assert_eq!(gap.relative_path(), *path);
        assert_eq!(
            gap.source().identity,
            ContentId::<SourceFactDomain>::from_canonical_bytes(text.as_bytes())
        );
        assert_eq!(gap.source().byte_len as usize, text.len());
        assert_eq!(
            gap.cause(),
            PackageSourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram
        );
    }
    let program = staged
        .native_program_sources()
        .expect("exact bound program membership");
    let mapped = program
        .sources()
        .filter_map(|row| row.package_path.map(str::to_owned))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        mapped,
        BTreeSet::from(["src/lib.ts".to_owned(), "src/main.ts".to_owned()])
    );
    assert!(
        program
            .sources()
            .any(|row| row.program_path == "types/ambient.d.ts" && row.package_path.is_none())
    );
    assert!(
        !program
            .sources()
            .any(|row| row.program_path.ends_with("config.ts")
                || row.program_path.ends_with("config.js"))
    );
    let mut names = BTreeSet::new();
    let mut oracle_calls = 0;
    for ordinal in 0..staged.artifacts().len() {
        staged
            .with_semantic_reader(ordinal, |reader, _| -> Result<(), std::io::Error> {
                for entity in reader.canonical_entities() {
                    if let Some(atom) = entity.name.named_atom() {
                        names.insert(reader.atom(atom).unwrap().to_vec());
                    }
                }
                for (_, link) in reader.canonical_links() {
                    if link.kind == LinkKind::Calls && link.confidence == Confidence::Compiler {
                        let target = match link.target {
                            LinkTarget::External(id) => reader.external(id),
                            LinkTarget::Local(_) => None,
                        };
                        if let Some(ExternalTarget::Foreign(target)) = target {
                            assert_eq!(reader.atom(target.display), Some(b"answer".as_slice()));
                        }
                        assert!(
                            target.is_some(),
                            "the cross-file answer call retains an external authoritative target"
                        );
                        oracle_calls += 1;
                    }
                }
                Ok(())
            })
            .unwrap();
    }
    assert!(names.contains(b"answer".as_slice()) && names.contains(b"run".as_slice()));
    assert!(!names.contains(b"eslintOnly".as_slice()) && !names.contains(b"nextOnly".as_slice()));
    assert_eq!(
        oracle_calls, 1,
        "selected source retains the Compiler API-proved cross-file call"
    );
    let published = compiler
        .compile_package_sources(package, &mut |_| {})
        .unwrap();
    assert_eq!(published.images.len(), 2);
    assert_eq!(published.coverage_gaps(), staged.coverage_gaps());
    assert_eq!(published.publication.manifest, staged.manifest_facts());
    // Changing only the captured excluded source changes the input witness, even while
    // the checked program and selected semantic output bytes are identical.
    let changed_text = "export const eslintOnly = { rules: { strict: true } };\n";
    fs::write(fixture.root.join("eslint.config.ts"), changed_text).unwrap();
    let changed_sources = [
        PackageSource::new("eslint.config.ts", changed_text).unwrap(),
        sources[1],
        sources[2],
        sources[3],
        sources[4],
    ];
    let changed = compiler
        .compile_package_sources_staged(
            PackageSourceSet::new(&request, &fixture.root, &changed_sources).unwrap(),
            &mut |_| {},
        )
        .unwrap();
    assert_ne!(
        changed.input_witness().input_root(),
        staged.input_witness().input_root()
    );
    assert_eq!(
        changed.native_program_sources().unwrap().program(),
        program.program()
    );
    assert_eq!(
        changed.coverage_gaps()[0].source().identity,
        ContentId::<SourceFactDomain>::from_canonical_bytes(changed_text.as_bytes())
    );
    // TS and TSX package profiles share the exact configured program but map only
    // their own captured frontier; imports and other-profile files are not dropped.
    let tsx_text = "export function view(): number { return 1; }\n";
    fs::write(fixture.root.join("src/view.tsx"), tsx_text).unwrap();
    let tsx_request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(138),
            profile: LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            stage: Stage::LowerIr,
        },
        PackageUrl::try_from("pkg:npm/program-membership@1.0.0".to_owned()).unwrap(),
    )
    .unwrap();
    let tsx_sources = [PackageSource::new("src/view.tsx", tsx_text).unwrap()];
    let tsx = compiler
        .compile_package_sources_staged(
            PackageSourceSet::new(&tsx_request, &fixture.root, &tsx_sources).unwrap(),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(tsx.artifacts().len(), 1);
    assert!(tsx.coverage_gaps().is_empty());
    let tsx_program = tsx.native_program_sources().unwrap();
    assert_eq!(
        tsx_program
            .sources()
            .filter_map(|row| row.package_path)
            .collect::<Vec<_>>(),
        vec!["src/view.tsx"]
    );
    assert!(
        tsx_program
            .sources()
            .any(|row| row.program_path == "src/main.ts" && row.package_path.is_none())
    );
    // All excluded sources still cannot produce a manufactured semantic generation.
    assert!(
        compiler
            .compile_package_sources_staged(
                PackageSourceSet::new(&request, &fixture.root, &changed_sources[..3]).unwrap(),
                &mut |_| {}
            )
            .is_err()
    );
    compiler.shutdown().unwrap();
}

#[test]
#[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn configured_extends_inherits_membership_while_project_references_still_refuse() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tsconfig.base.json"), r#"{"compilerOptions":{"strict":true,"target":"ES2020","module":"CommonJS","allowJs":false},"include":["src/**/*"]}"#).unwrap();
    fs::write(
        fixture.root.join("tsconfig.json"),
        r#"{"extends":"./tsconfig.base.json"}"#,
    )
    .unwrap();
    let sources = Fixture::sources();
    let inherited = fixture.native(&sources);
    assert_eq!(
        selected(&inherited),
        BTreeSet::from(["src/lib.ts", "src/main.ts"])
    );
    assert_eq!(
        inherited.package_membership["eslint.config.ts"],
        TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
    );
    fs::write(
        fixture.root.join("tsconfig.json"),
        r#"{"extends":"./tsconfig.base.json","references":[{"path":"./referenced"}]}"#,
    )
    .unwrap();
    fs::create_dir(fixture.root.join("referenced")).unwrap();
    fs::write(
        fixture.root.join("referenced/tsconfig.json"),
        r#"{"compilerOptions":{"composite":true},"files":[]}"#,
    )
    .unwrap();
    let admitted = fixture.host().admit(&fixture.root).unwrap().unwrap();
    let inputs = admitted.inputs();
    assert!(
        build_native_inputs(
            &inputs,
            &mut inputs.resolver(),
            &sources,
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(false)
        )
        .is_err(),
        "membership exclusion does not admit project references as one TSZ program"
    );
}

#[test]
#[ignore = "requires pinned TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn actual_compiler_node_formats_follow_package_type_with_identical_source_membership() {
    use backend_frontend_typescript::{TszProjectFileFormat, TszProjectModuleRequestKind};
    use std::collections::BTreeMap;
    let fixture = Fixture::new();
    let main = "import { value } from './dep.js';\nimport type { Token as I } from './dep.js' with { 'resolution-mode': 'import' };\nimport type { Token as R } from './dep.js' with { 'resolution-mode': 'require' };\nexport const result = value; export type Pair = [I, R];\n";
    let dep = "export interface Token { value: number; } export const value = 42;\n";
    let source_pairs = [
        ("src/main.ts", main),
        ("src/dep.ts", dep),
        ("src/fixed.mts", "export const esm = 1;\n"),
        ("src/fixed.cts", "export const cjs = 1;\n"),
        ("eslint.config.ts", ESLINT),
    ];
    for (path, text) in source_pairs {
        fs::write(fixture.root.join(path), text).unwrap();
    }
    let sources: Vec<_> = source_pairs
        .into_iter()
        .map(|(path, text)| PackageSource::new(path, text).unwrap())
        .collect();
    for module in ["Node16", "NodeNext"] {
        fs::write(fixture.root.join("tsconfig.json"), serde_json::to_vec(&serde_json::json!({
            "compilerOptions": {"strict": true, "target":"ES2022", "module": module, "moduleResolution": module},
            "files": ["src/main.ts", "src/dep.ts", "src/fixed.mts", "src/fixed.cts"],
        })).unwrap()).unwrap();
        let mut prior = None;
        for (package_type, format) in [
            ("commonjs", TszProjectFileFormat::CommonJs),
            ("module", TszProjectFileFormat::EsModule),
        ] {
            fs::write(
                fixture.root.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name":"program-membership", "version":"1.0.0", "type":package_type,
                }))
                .unwrap(),
            )
            .unwrap();
            let native = fixture.native(&sources);
            let oracle = fixture.oracle();
            assert_eq!(oracle["errors"], serde_json::json!([]));
            let formats: BTreeMap<_, _> = native
                .options
                .file_formats
                .iter()
                .map(|input| (input.path.as_str(), input.format))
                .collect();
            assert_eq!(formats.len(), 4);
            assert_eq!(formats["src/main.ts"], format);
            assert_eq!(formats["src/dep.ts"], format);
            assert_eq!(formats["src/fixed.mts"], TszProjectFileFormat::EsModule);
            assert_eq!(formats["src/fixed.cts"], TszProjectFileFormat::CommonJs);
            assert!(!formats.contains_key("eslint.config.ts"));
            for input in &native.options.file_formats {
                let expected = match input.format {
                    TszProjectFileFormat::Unknown => "unknown",
                    TszProjectFileFormat::CommonJs => "common_js",
                    TszProjectFileFormat::EsModule => "es_module",
                };
                assert_eq!(
                    oracle["formats"][fixture.root.join(&input.path).to_str().unwrap()],
                    expected
                );
            }
            let requests: Vec<_> = native
                .options
                .module_resolutions
                .iter()
                .filter(|request| {
                    request.importer_path == "src/main.ts"
                        && request.specifier == "./dep.js"
                        && request.request_kind == TszProjectModuleRequestKind::EsmImport
                })
                .collect();
            assert_eq!(
                requests.len(),
                2,
                "implicit and explicit same-mode observations deduplicate only their exact tuple"
            );
            assert!(
                requests
                    .iter()
                    .any(|request| format!("{:?}", request.resolution_mode) == "Some(Import)")
            );
            assert!(
                requests
                    .iter()
                    .any(|request| format!("{:?}", request.resolution_mode) == "Some(Require)")
            );
            // Exercise the same admitted real API recipe through TSZ rather
            // than treating the compiler report as the only positive control.
            let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
            authority
                .update(
                    native.sources.clone(),
                    native.options.clone(),
                    &native.libraries,
                )
                .unwrap();
            let project = authority.project().unwrap();
            let main_index = project
                .program()
                .files
                .iter()
                .position(|file| file.file_name == "src/main.ts")
                .unwrap();
            project
                .with_file_checker_and_types(main_index, |checker, _, _, _| {
                    assert_eq!(
                        checker.ctx.file_is_esm,
                        Some(format == TszProjectFileFormat::EsModule)
                    );
                    assert!(std::sync::Arc::ptr_eq(
                        checker.ctx.file_is_esm_map.as_ref().unwrap(),
                        &project.program().file_is_esm_map
                    ));
                    let target = checker
                        .ctx
                        .resolve_import_target_from_file_with_mode(main_index, "./dep.js", None)
                        .unwrap();
                    assert_eq!(project.program().files[target].file_name, "src/dep.ts");
                })
                .unwrap();
            let snapshots: Vec<_> = native
                .sources
                .iter()
                .map(|source| (source.path.clone(), source.source.clone()))
                .collect();
            if let Some((prior_sources, prior_environment)) = prior {
                assert_eq!(
                    snapshots, prior_sources,
                    "package format edit keeps exact source bytes and membership"
                );
                assert_ne!(
                    native.options.environment, prior_environment,
                    "format-only package change invalidates durable semantics"
                );
            }
            prior = Some((snapshots, native.options.environment));
        }
    }
    // A package manifest alone cannot fabricate format evidence outside the
    // compiler's Node16/NodeNext format classification.
    fs::write(fixture.root.join("tsconfig.json"), serde_json::to_vec(&serde_json::json!({
        "compilerOptions": {"target":"ES2022", "module":"CommonJS", "moduleResolution":"Node10"},
        "files":["src/main.ts", "src/dep.ts"],
    })).unwrap()).unwrap();
    let native = fixture.native(&sources);
    let oracle = fixture.oracle();
    let main = native
        .options
        .file_formats
        .iter()
        .find(|input| input.path == "src/main.ts")
        .unwrap();
    assert_eq!(main.format, TszProjectFileFormat::Unknown);
    assert_eq!(
        oracle["formats"][fixture.root.join("src/main.ts").to_str().unwrap()],
        "unknown"
    );
}

#[test]
fn compiler_file_format_report_requires_closed_typed_evidence() {
    use backend_frontend_typescript::TszProjectFileFormat;
    let report = |format: serde_json::Value| {
        serde_json::json!({
            "path":"/project/src/main.ts", "sha256":"00", "default_library":false,
            "implied_node_format": format,
        })
    };
    for (value, expected) in [
        ("unknown", TszProjectFileFormat::Unknown),
        ("common_js", TszProjectFileFormat::CommonJs),
        ("es_module", TszProjectFileFormat::EsModule),
    ] {
        let file: super::ProgramFile =
            serde_json::from_value(report(serde_json::json!(value))).unwrap();
        assert_eq!(file.implied_node_format, expected);
    }
    for unsupported in [
        serde_json::json!(null),
        serde_json::json!(1),
        serde_json::json!("ES2015"),
        serde_json::json!(true),
    ] {
        assert!(serde_json::from_value::<super::ProgramFile>(report(unsupported)).is_err());
    }
    let mut missing = report(serde_json::json!("unknown"));
    missing
        .as_object_mut()
        .unwrap()
        .remove("implied_node_format");
    assert!(serde_json::from_value::<super::ProgramFile>(missing).is_err());
}
