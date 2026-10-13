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
    fn normalized_options(&self, sources: &[PackageSource<'_>]) -> serde_json::Value {
        let admitted = self.host().admit(&self.root).unwrap().unwrap();
        let inputs = admitted.inputs();
        let report = super::run_program_bridge(
            &inputs,
            Some(&self.root.join("tsconfig.json")),
            sources,
            &self.root.join("node_modules/typescript/lib/typescript.js"),
            Instant::now() + Duration::from_secs(90),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            report.error.is_none(),
            "compiler report failed: {:?}",
            report.error
        );
        report.compiler_options["compilerOptions"].clone()
    }
    fn assert_normalized_options(&self, sources: &[PackageSource<'_>], oracle: &serde_json::Value) {
        let normalized = self.normalized_options(sources);
        let checker = backend_frontend_typescript::checker_options_from_compiler_api_json(
            &serde_json::to_string(&serde_json::json!({"compilerOptions": normalized})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            checker.target as u64,
            oracle["target_value"].as_u64().unwrap(),
            "selected SDK target semantic identity"
        );
        if oracle["target_is_esnext"].as_bool().unwrap() {
            assert_eq!(normalized["target"], "ESNext");
        }
        for name in [
            "module",
            "moduleResolution",
            "moduleDetection",
            "esModuleInterop",
            "allowSyntheticDefaultImports",
        ] {
            assert_eq!(
                normalized[name], oracle["normalized"][name],
                "selected SDK computed option {name}"
            );
        }
    }
    /// This oracle constructs an ordinary TypeScript program without the production bridge.
    fn oracle(&self) -> serde_json::Value {
        let script = r#"
const ts = require(process.argv[1]);
const config = ts.getParsedCommandLineOfConfigFile(process.argv[2], {}, {...ts.sys, onUnRecoverableConfigFileDiagnostic:d=>{throw Error(String(d.code))}});
const p = ts.createProgram({rootNames:config.fileNames, options:config.options});
const c = p.getTypeChecker();
const ownedOptions = p.getCompilerOptions();
const computed = name => ts.computedOptions[name].computeValue(ownedOptions);
const normalized = {
  target:ts.ScriptTarget[computed('target')], module:ts.ModuleKind[computed('module')],
  moduleResolution:ts.ModuleResolutionKind[computed('moduleResolution')],
  moduleDetection:ts.ModuleDetectionKind[computed('moduleDetection')],
  esModuleInterop:computed('esModuleInterop'), allowSyntheticDefaultImports:computed('allowSyntheticDefaultImports')
};
const calls=[];
const requests = new Map();
const request = (f, literal, kind) => {
  const mode = p.getModeForUsageLocation(f, literal);
  if (mode !== undefined && mode !== ts.ModuleKind.CommonJS && mode !== ts.ModuleKind.ESNext) throw Error('unsupported oracle resolution mode');
  const row = {importer:f.fileName, specifier:literal.text, kind, mode:mode === undefined ? null : mode === ts.ModuleKind.CommonJS ? 'require' : 'import'};
  requests.set(JSON.stringify(row), row);
};
for(const f of p.getSourceFiles()) {
  function visit(n) {
    if(ts.isCallExpression(n) && n.expression.getText(f)==='answer') {
      const sig=c.getResolvedSignature(n);
      calls.push({file:f.fileName, result:c.typeToString(c.getTypeAtLocation(n)), declaration:sig.declaration.getSourceFile().fileName});
    }
    if(ts.isImportDeclaration(n) && n.moduleSpecifier && ts.isStringLiteralLike(n.moduleSpecifier)) request(f,n.moduleSpecifier,'EsmImport');
    else if(ts.isExportDeclaration(n) && n.moduleSpecifier && ts.isStringLiteralLike(n.moduleSpecifier)) request(f,n.moduleSpecifier,'EsmReExport');
    else if(ts.isImportTypeNode(n) && ts.isLiteralTypeNode(n.argument) && ts.isStringLiteralLike(n.argument.literal)) request(f,n.argument.literal,'EsmImport');
    else if(ts.isImportEqualsDeclaration(n) && ts.isExternalModuleReference(n.moduleReference) && n.moduleReference.expression && ts.isStringLiteralLike(n.moduleReference.expression)) request(f,n.moduleReference.expression,'CjsRequire');
    else if(ts.isCallExpression(n) && n.arguments.length && ts.isStringLiteralLike(n.arguments[0])) {
      if(n.expression.kind === ts.SyntaxKind.ImportKeyword) request(f,n.arguments[0],'DynamicImport');
      else if(ts.isIdentifier(n.expression) && n.expression.text === 'require') request(f,n.arguments[0],'CjsRequire');
    }
    ts.forEachChild(n,visit);
  }
  visit(f);
}
const diagnostics = ts.getPreEmitDiagnostics(p).filter(d=>d.category===ts.DiagnosticCategory.Error);
process.stdout.write(JSON.stringify({normalized, target_value:computed('target'), target_is_esnext:computed('target') === ts.ScriptTarget.ESNext, files:p.getSourceFiles().map(f=>f.fileName), requests:Array.from(requests.values()), formats:Object.fromEntries(p.getSourceFiles().map(f=>[f.fileName, f.impliedNodeFormat === undefined ? 'unknown' : f.impliedNodeFormat === ts.ModuleKind.CommonJS ? 'common_js' : f.impliedNodeFormat === ts.ModuleKind.ESNext ? 'es_module' : 'unsupported'])), calls, diagnostics:diagnostics.map(d=>({file:d.file ? d.file.fileName : null, code:d.code})), declarations:Object.fromEntries(p.getSourceFiles().map(f=>[f.fileName, f.isDeclarationFile])), errors:diagnostics.map(d=>d.code)}));
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
fn configured_paths_and_bom_dependency_reads_keep_exact_raw_source_authority() {
    const MAIN_ALIAS: &str = "import { answer } from '@/lib';\nexport function run(): number { return answer({value: 42}); }\n";
    const LIB_PACKAGE: &str = "import type { Token } from 'dependency-token';\nexport function answer(token: Token): number { return token.value; }\n";
    const DECLARATION: &str = "\u{feff}export interface Token { value: number; }\ndeclare const config: readonly unknown[];\nexport default config;\n";
    const ESLINT_PACKAGE: &str =
        "import dependencyConfig from 'dependency-token';\nexport default dependencyConfig;\n";
    let fixture = Fixture::new();
    let dependency = fixture.root.join("node_modules/dependency-token");
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        dependency.join("package.json"),
        r#"{"name":"dependency-token","version":"1.0.0","type":"module","types":"index.d.ts","main":"index.js"}"#,
    )
    .unwrap();
    fs::write(dependency.join("index.d.ts"), DECLARATION).unwrap();
    fs::write(dependency.join("index.js"), "export default [];\n").unwrap();
    for (path, text) in [
        ("src/main.ts", MAIN_ALIAS),
        ("src/lib.ts", LIB_PACKAGE),
        ("eslint.config.mjs", ESLINT_PACKAGE),
    ] {
        fs::write(fixture.root.join(path), text).unwrap();
    }
    fs::write(
        fixture.root.join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true,"target":"ES2020","module":"ESNext","moduleResolution":"Bundler","allowJs":true,"baseUrl":".","paths":{"@/*":["src/*"]}},"include":["src/**/*.ts"]}"#,
    )
    .unwrap();
    let sources = [
        PackageSource::new("src/main.ts", MAIN_ALIAS).unwrap(),
        PackageSource::new("src/lib.ts", LIB_PACKAGE).unwrap(),
        PackageSource::new("eslint.config.mjs", ESLINT_PACKAGE).unwrap(),
    ];
    let admitted = fixture.host().admit(&fixture.root).unwrap().unwrap();
    let inputs = admitted.inputs();
    let native = build_native_inputs(
        &inputs,
        &mut inputs.resolver(),
        &sources,
        Instant::now() + Duration::from_secs(90),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        selected(&native),
        BTreeSet::from(["src/lib.ts", "src/main.ts"])
    );
    assert_eq!(
        native.package_membership["eslint.config.mjs"],
        TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
    );
    let declaration = native
        .sources
        .iter()
        .find(|source| source.path == "node_modules/dependency-token/index.d.ts")
        .unwrap();
    assert_eq!(
        declaration.source, DECLARATION,
        "TSZ retains exact raw source text"
    );
    let oracle = fixture.oracle();
    assert_eq!(oracle["errors"], serde_json::json!([]));
    assert_eq!(oracle["calls"][0]["result"], "number");
    assert_eq!(
        oracle["calls"][0]["declaration"],
        serde_json::json!(fixture.root.join("src/lib.ts"))
    );
    let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
    authority
        .update(
            native.sources.clone(),
            native.options.clone(),
            &native.libraries,
        )
        .unwrap();
    let project = authority.project().unwrap();
    for (importer, specifier, expected) in [
        ("src/main.ts", "@/lib", "src/lib.ts"),
        (
            "src/lib.ts",
            "dependency-token",
            "node_modules/dependency-token/index.d.ts",
        ),
    ] {
        let index = project
            .program()
            .files
            .iter()
            .position(|file| file.file_name == importer)
            .unwrap();
        project
            .with_file_checker_and_types(index, |checker, _, _, _| {
                let target = checker
                    .ctx
                    .resolve_import_target_from_file_with_mode(index, specifier, None)
                    .unwrap();
                assert_eq!(project.program().files[target].file_name, expected);
            })
            .unwrap();
    }
    let dependency_path = dependency.join("index.d.ts");
    let mut forged = native.closure_witness.compiler_accesses.to_vec();
    let read = forged
        .iter_mut()
        .find(|access| {
            access.kind == super::ProgramAccessKind::File
                && Path::new(&access.path) == dependency_path
        })
        .unwrap();
    assert_eq!(
        read.sha256.as_deref(),
        Some(super::sha256_hex(DECLARATION.trim_start_matches('\u{feff}').as_bytes()).as_str())
    );
    read.sha256 = Some(super::sha256_hex(DECLARATION.as_bytes()));
    assert!(matches!(
        super::replay_observations(&mut inputs.resolver(), &forged, &native.closure_witness.directory_views, native.closure_witness.metrics),
        Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerIoClosureMismatch { .. })
    ), "a raw-byte hash cannot impersonate the compiler's decoded text receipt");
    native
        .closure_witness
        .validate_current(admitted.witness.as_ref())
        .unwrap();
    fs::write(
        &dependency_path,
        DECLARATION.strip_prefix('\u{feff}').unwrap(),
    )
    .unwrap();
    assert!(
        native
            .closure_witness
            .validate_current(admitted.witness.as_ref())
            .is_err(),
        "removing only the BOM still changes the raw source proof"
    );
    let refreshed = fixture.native(&sources);
    assert_ne!(native.options.environment, refreshed.options.environment);
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
fn angular_solution_selects_app_program_without_inheriting_root_references() {
    let fixture = Fixture::new();
    let root_config = r#"{"compilerOptions":{"strict":true,"target":"ES2022","module":"ES2022","moduleResolution":"Bundler"},"files":[],"references":[{"path":"./tsconfig.app.json"},{"path":"./tsconfig.spec.json"}]}"#;
    let app_config =
        r#"{"extends":"./tsconfig.json","files":["src/main.ts"],"include":["types/**/*.d.ts"]}"#;
    let spec_config = r#"{"extends":"./tsconfig.json","files":["src/main.spec.ts"]}"#;
    for (path, text) in [
        ("tsconfig.json", root_config),
        ("tsconfig.app.json", app_config),
        ("tsconfig.spec.json", spec_config),
        (
            "angular.json",
            r#"{"projects":{"app":{"architect":{"build":{"options":{"tsConfig":"tsconfig.app.json"}},"test":{"options":{"tsConfig":"tsconfig.spec.json"}}}}}}"#,
        ),
        ("src/main.spec.ts", "export const testOnly = 1;\n"),
    ] {
        fs::write(fixture.root.join(path), text).unwrap();
    }
    let mut sources = Fixture::sources();
    sources.push(PackageSource::new("src/main.spec.ts", "export const testOnly = 1;\n").unwrap());
    let admitted = fixture.host().admit(&fixture.root).unwrap().unwrap();
    let inputs = admitted.inputs();
    let app_path = fixture.root.join("tsconfig.app.json");
    assert_eq!(
        super::select_config(&inputs)
            .unwrap()
            .unwrap()
            .path
            .as_ref(),
        app_path
    );
    for (path, text) in [
        ("tsconfig.json", root_config),
        ("tsconfig.app.json", app_config),
        ("tsconfig.spec.json", spec_config),
    ] {
        let captured = inputs
            .config_candidates
            .iter()
            .find(|config| config.path.as_ref() == fixture.root.join(path))
            .unwrap();
        assert_eq!(captured.bytes.as_ref(), text.as_bytes());
    }
    let report = super::run_program_bridge(
        &inputs,
        Some(&app_path),
        &sources,
        &fixture
            .root
            .join("node_modules/typescript/lib/typescript.js"),
        Instant::now() + Duration::from_secs(90),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(Path::new(&report.config_path), app_path);
    assert!(
        report.project_references.is_empty(),
        "the selected app must not inherit solution references"
    );
    let options = &report.compiler_options["compilerOptions"];
    assert_eq!(options["strict"], true);
    assert_eq!(options["target"], "ES2022");
    assert_eq!(options["module"], "ES2022");
    assert_eq!(options["moduleResolution"], "Bundler");
    let native = build_native_inputs(
        &inputs,
        &mut inputs.resolver(),
        &sources,
        Instant::now() + Duration::from_secs(90),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        selected(&native),
        BTreeSet::from(["src/lib.ts", "src/main.ts"])
    );
    assert_eq!(
        native.package_membership["src/main.spec.ts"],
        TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
    );
    native
        .closure_witness
        .validate_current(admitted.witness.as_ref())
        .unwrap();
    fs::write(
        fixture.root.join("tsconfig.spec.json"),
        format!("{spec_config}\n"),
    )
    .unwrap();
    assert!(
        native
            .closure_witness
            .validate_current(admitted.witness.as_ref())
            .is_err(),
        "the excluded spec config remains an exact captured witness"
    );
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
    let main = "import { value } from './dep.js';\nimport type { Token as I } from './dep.js' with { 'resolution-mode': 'import' };\nimport type { Token as R } from './dep.js' with { 'resolution-mode': 'require' };\nexport const result = value; export type Pair = [I, R];\nexport { value as forwarded } from './dep.js';\nexport type InlineImport = import('./dep.js', { with: { 'resolution-mode': 'import' } }).Token;\nexport type InlineRequire = import('./dep.js', { with: { 'resolution-mode': 'require' } }).Token;\nimport legacy = require('./fixed.cjs'); export const legacyValue = legacy.cjs;\nexport const dynamic = import('./dep.js');\ndeclare function require(name: string): { cjs: number }; export const rawRequire = require('./fixed.cjs');\n";
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
        for explicit_resolution in [true, false] {
            let mut compiler_options =
                serde_json::json!({"strict":true, "target":"ES2022", "module":module});
            if explicit_resolution {
                compiler_options["moduleResolution"] = serde_json::json!(module);
            }
            fs::write(
                fixture.root.join("tsconfig.json"),
                serde_json::to_vec(&serde_json::json!({
                    "compilerOptions": compiler_options,
                    "files": ["src/main.ts", "src/dep.ts", "src/fixed.mts", "src/fixed.cts"],
                }))
                .unwrap(),
            )
            .unwrap();
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
                fixture.assert_normalized_options(&sources, &oracle);
                assert_eq!(
                    format!("{:?}", native.options.checker.module),
                    oracle["normalized"]["module"].as_str().unwrap()
                );
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
                let expected_requests: BTreeSet<_> = oracle["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|request| {
                        let importer = Path::new(request["importer"].as_str().unwrap())
                            .strip_prefix(&fixture.root)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned();
                        (
                            importer,
                            request["specifier"].as_str().unwrap().to_owned(),
                            request["kind"].as_str().unwrap().to_owned(),
                            request["mode"].as_str().map(str::to_owned),
                        )
                    })
                    .collect();
                let actual_requests: BTreeSet<_> = native
                    .options
                    .module_resolutions
                    .iter()
                    .map(|request| {
                        let mode = request.resolution_mode.map(|mode| match mode {
                            backend_frontend_typescript::TszResolutionModeOverride::Import => {
                                "import".to_owned()
                            }
                            backend_frontend_typescript::TszResolutionModeOverride::Require => {
                                "require".to_owned()
                            }
                        });
                        (
                            request.importer_path.clone(),
                            request.specifier.clone(),
                            format!("{:?}", request.request_kind),
                            mode,
                        )
                    })
                    .collect();
                assert_eq!(
                    actual_requests, expected_requests,
                    "exact SDK modes with module={module}, explicit moduleResolution={explicit_resolution}, package={package_type}"
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

#[test]
#[ignore = "requires pinned TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn actual_compiler_normalized_defaults_match_owned_sdk_computed_options() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("src/main.ts"),
        "export const value = 42;\n",
    )
    .unwrap();
    let sources = vec![PackageSource::new("src/main.ts", "export const value = 42;\n").unwrap()];
    for options in [
        serde_json::json!({}),
        serde_json::json!({"target":"ES2022"}),
        serde_json::json!({"target":"ESNext"}),
        serde_json::json!({"module":"CommonJS"}),
        serde_json::json!({"module":"ESNext"}),
        serde_json::json!({"module":"Preserve", "moduleResolution":"Bundler"}),
        serde_json::json!({"module":"ESNext", "moduleResolution":"Bundler"}),
        serde_json::json!({"module":"Node16"}),
        serde_json::json!({"module":"NodeNext"}),
    ] {
        fs::write(
            fixture.root.join("tsconfig.json"),
            serde_json::to_vec(&serde_json::json!({
                "compilerOptions":options, "files":["src/main.ts"],
            }))
            .unwrap(),
        )
        .unwrap();
        let oracle = fixture.oracle();
        assert_eq!(oracle["errors"], serde_json::json!([]));
        fixture.assert_normalized_options(&sources, &oracle);
        let native = fixture.native(&sources);
        assert_eq!(
            format!("{:?}", native.options.checker.module),
            oracle["normalized"]["module"].as_str().unwrap()
        );
        assert_eq!(
            native.options.checker.es_module_interop,
            oracle["normalized"]["esModuleInterop"].as_bool().unwrap()
        );
        assert_eq!(
            native.options.checker.allow_synthetic_default_imports,
            oracle["normalized"]["allowSyntheticDefaultImports"]
                .as_bool()
                .unwrap()
        );
    }
}

#[test]
#[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
fn configured_skip_lib_check_matches_sdk_diagnostics_without_losing_declaration_types() {
    use backend_frontend_typescript::TszDeclarationDiagnosticPolicy;
    const DECLARATION: &str = "export interface Broken { missing: DoesNotExist; } export declare const value: { value: number };\n";
    let fixture = Fixture::new();
    fs::write(fixture.root.join("types/diagnostic.d.ts"), DECLARATION).unwrap();
    let mut previous = None;
    for wrong_application in [false, true] {
        let app = format!(
            "import {{ value, Broken }} from '../types/diagnostic'; export type UsedBroken = Broken; export const actual: {} = value.value;\n",
            if wrong_application {
                "string"
            } else {
                "number"
            }
        );
        fs::write(fixture.root.join("src/main.ts"), &app).unwrap();
        let sources = [PackageSource::new("src/main.ts", &app).unwrap()];
        for skip in [false, true] {
            fs::write(fixture.root.join("tsconfig.json"), serde_json::to_vec(&serde_json::json!({
                "compilerOptions":{"strict":true,"target":"ES2020","module":"CommonJS","skipLibCheck":skip},
                "files":["src/main.ts"]
            })).unwrap()).unwrap();
            let oracle = fixture.oracle();
            let expected = if skip { vec![] } else { vec![2304] };
            let sdk_declaration = oracle["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| {
                    d["file"].as_str() == fixture.root.join("types/diagnostic.d.ts").to_str()
                })
                .map(|d| d["code"].as_u64().unwrap() as u32)
                .collect::<Vec<_>>();
            assert_eq!(
                sdk_declaration, expected,
                "genuine SDK declaration oracle, skip={skip}"
            );
            let sdk_application = oracle["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["file"].as_str() == fixture.root.join("src/main.ts").to_str())
                .map(|d| d["code"].as_u64().unwrap() as u32)
                .collect::<Vec<_>>();
            assert_eq!(
                sdk_application,
                if wrong_application {
                    vec![2322]
                } else {
                    vec![]
                }
            );
            let native = fixture.native(&sources);
            assert_eq!(
                native.options.checker.declaration_diagnostics,
                if skip {
                    TszDeclarationDiagnosticPolicy::SkipSemantic
                } else {
                    TszDeclarationDiagnosticPolicy::Check
                }
            );
            let source_rows = native
                .sources
                .iter()
                .map(|row| (row.path.clone(), row.source.clone()))
                .collect::<Vec<_>>();
            let library_rows = native
                .libraries
                .iter()
                .map(|lib| {
                    (
                        lib.file_name.clone(),
                        lib.arena
                            .get_source_file_at(lib.root_index)
                            .unwrap()
                            .text
                            .to_string(),
                    )
                })
                .collect::<Vec<_>>();
            if let Some((prior_environment, prior_sources, prior_libraries)) = previous.take() {
                assert_ne!(
                    native.options.environment, prior_environment,
                    "resolved policy changes program environment"
                );
                if skip {
                    assert_eq!(
                        source_rows, prior_sources,
                        "skipLibCheck does not drop declarations"
                    );
                    assert_eq!(
                        library_rows, prior_libraries,
                        "skipLibCheck does not drop SDK libraries"
                    );
                }
            }
            previous = Some((native.options.environment, source_rows, library_rows));
            let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
            authority
                .update(native.sources, native.options, &native.libraries)
                .unwrap();
            let project = authority.project().unwrap();
            for (path, sdk) in [
                ("src/main.ts", sdk_application),
                ("types/diagnostic.d.ts", sdk_declaration),
            ] {
                let actual = project
                    .check_result()
                    .file_results
                    .iter()
                    .find(|row| row.file_name == path)
                    .unwrap();
                assert_eq!(
                    actual
                        .diagnostics
                        .iter()
                        .map(|d| d.code)
                        .collect::<Vec<_>>(),
                    sdk,
                    "native vs genuine SDK {path}, skip={skip}, wrong_application={wrong_application}: {:?}",
                    actual.diagnostics
                );
            }
            assert_eq!(
                project.source_text("types/diagnostic.d.ts"),
                Some(DECLARATION)
            );
            assert!(project.bind_result("types/diagnostic.d.ts").is_some());
            assert!(
                oracle["declarations"]
                    [fixture.root.join("types/diagnostic.d.ts").to_str().unwrap()]
                .as_bool()
                .unwrap()
            );
        }
    }
}
