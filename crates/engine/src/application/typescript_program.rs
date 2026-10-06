//! Compiler-API-resolved TypeScript program closure for native TSZ checking.
//!
//! The selected TypeScript package constructs the real program and resolves each
//! module request. The Rust side admits every source and filesystem observation
//! through the package's resolver capability before those facts can enter TSZ.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_frontend_typescript::{
    TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszLibraryInput,
    TszProjectModuleRequestKind, TszProjectModuleResolution, TszProjectModuleResolutionTarget,
    TszProjectOptions, TszProjectSemanticOptions, checker_options_from_compiler_api_json,
};
use backend_version::{ContentId, SourceFactDomain};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::typescript_host::{
    TypeScriptProjectHostError, TypeScriptProjectInputs, TypeScriptResolverCapability,
};
use super::{ToolchainProbeLimits, toolchain_probe::run_typescript_program_bridge};
use crate::application::compiler::PackageSource;

const MAX_BRIDGE_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_BRIDGE_FILES: usize = 16_384;
const MAX_BRIDGE_REQUESTS: usize = 131_072;
const MAX_PROJECT_WORK_UNITS: u64 = 16_000_000_000;
const LIB_VIRTUAL_PREFIX: &str = "@compiler/lib.";

/// Fully admitted source/lib/options set produced by the selected compiler API.
pub(crate) struct NativeTypeScriptInputs {
    pub(crate) sources: Vec<TszFileInput>,
    pub(crate) libraries: Vec<std::sync::Arc<backend_frontend_typescript::TszLibFile>>,
    pub(crate) options: TszProjectOptions,
    /// Package-relative source path to the exact workspace-root TSZ path.
    pub(crate) package_paths: BTreeMap<Box<str>, Box<str>>,
    pub(crate) work_units: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramReport {
    schema: u32,
    version: String,
    config_path: String,
    compiler_options: serde_json::Value,
    #[serde(default)]
    project_references: Vec<serde_json::Value>,
    files: Vec<ProgramFile>,
    resolutions: Vec<ProgramResolution>,
    #[serde(default)]
    accesses: Vec<ProgramAccess>,
    #[serde(default)]
    unsupported_options: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramFile {
    path: String,
    sha256: String,
    default_library: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramResolution {
    importer: String,
    specifier: String,
    kind: String,
    mode: Option<String>,
    target_path: Option<String>,
    package_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramAccess {
    kind: String,
    path: String,
    #[serde(default)]
    exists: Option<bool>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    realpath: Option<String>,
    #[serde(default)]
    extensions: Option<Vec<String>>,
    #[serde(default)]
    recursive: Option<bool>,
    #[serde(default)]
    depth: Option<usize>,
}

/// One script executed by the exact admitted TypeScript compiler API.
const COMPILER_API_PROGRAM_SCRIPT: &str = r#"
'use strict';
const path = require('path');
const crypto = require('crypto');
const compilerApiPath = process.argv[1];
const configPath = process.argv[2];
const workspaceRoot = process.argv[3];
const observations = new Map();
const key = (kind, p, extra) => JSON.stringify([kind, path.resolve(p), extra || null]);
const digest = value => crypto.createHash('sha256').update(Buffer.isBuffer(value) ? value : Buffer.from(value)).digest('hex');
const add = value => { const k = JSON.stringify(value); observations.set(k, value); };
const result = value => { process.stdout.write(JSON.stringify(value)); };
try {
  const ts = require(compilerApiPath);
  const wrapSystem = sys => {
    for (const name of ['readFile', 'fileExists', 'directoryExists', 'realpath', 'readDirectory', 'getDirectories']) {
      if (typeof sys[name] !== 'function') continue;
      const original = sys[name].bind(sys);
      sys[name] = (...args) => {
        const p = args[0];
        const value = original(...args);
        if (name === 'readFile') {
          add({kind:'file', path:p, exists:value !== undefined, sha256:value === undefined ? null : digest(value)});
        } else if (name === 'fileExists') {
          add({kind:'exists', path:p, exists:!!value});
        } else if (name === 'directoryExists') {
          add({kind:'directory', path:p, exists:!!value});
        } else if (name === 'realpath') {
          add({kind:'realpath', path:p, realpath:value === undefined ? null : path.resolve(value)});
        } else if (name === 'readDirectory') {
          add({kind:'readDirectory', path:p, extensions:args[1] || null, recursive:args[4] === undefined, depth:args[4] === undefined ? null : args[4]});
        } else if (name === 'getDirectories') {
          add({kind:'getDirectories', path:p});
        }
        return value;
      };
    }
    return sys;
  };
  wrapSystem(ts.sys);
  const configDiagnostics = [];
  const parseHost = Object.assign({}, ts.sys, {
    onUnRecoverableConfigFileDiagnostic: diagnostic => configDiagnostics.push(diagnostic)
  });
  const parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, parseHost);
  if (!parsed) throw new Error('getParsedCommandLineOfConfigFile returned no project');
  if (parsed.projectReferences && parsed.projectReferences.length) {
    result({schema:1, version:ts.version, config_path:path.resolve(configPath), compiler_options:{compilerOptions:{}},
      project_references:parsed.projectReferences.map(r => r.path), files:[], resolutions:[], accesses:Array.from(observations.values()),
      unsupported_options:[], error:'project references are not yet admitted as one TSZ program'});
  } else {
    const options = parsed.options;
    const host = ts.createCompilerHost(options, true);
    for (const name of ['readFile', 'fileExists', 'directoryExists', 'realpath', 'readDirectory', 'getDirectories']) {
      if (typeof host[name] === 'function' && typeof ts.sys[name] === 'function') host[name] = ts.sys[name].bind(ts.sys);
    }
    const program = ts.createProgram({rootNames:parsed.fileNames, options, host});
    const programFiles = program.getSourceFiles();
    const byPath = new Map(programFiles.map(file => [path.resolve(file.fileName), file]));
    const sourceFiles = programFiles.map(file => ({
      path:path.resolve(file.fileName),
      sha256:digest(file.text),
      default_library:program.isSourceFileDefaultLibrary(file)
    }));
    const resolutions = [];
    const resolutionKeys = new Map();
    const modeFor = (file, literal, kind) => {
      const attributes = literal.parent && literal.parent.attributes;
      if (attributes && Array.isArray(attributes.elements)) {
        for (const item of attributes.elements) {
          if (item.name && item.name.text === 'resolution-mode' && item.value && (item.value.text === 'import' || item.value.text === 'require')) return item.value.text;
        }
      }
      if (kind === 'DynamicImport') return 'import';
      if (kind === 'CjsRequire') return 'require';
      const mr = options.moduleResolution === undefined ? undefined : ts.ModuleResolutionKind[options.moduleResolution];
      if (mr !== 'Node16' && mr !== 'NodeNext') return null;
      const format = file.impliedNodeFormat;
      if (format === ts.ModuleKind.CommonJS) return 'require';
      if (format === ts.ModuleKind.ESNext) return 'import';
      const mod = options.module === undefined ? undefined : ts.ModuleKind[options.module];
      if (mod === 'CommonJS' || mod === 'Node16' && kind === 'CjsRequire') return 'require';
      if (mod === 'NodeNext' || mod === 'ESNext' || mod === 'ES2022' || mod === 'ES2020' || mod === 'ES2015') return 'import';
      return null;
    };
    const addRequest = (file, literal, kind) => {
      if (!literal || typeof literal.text !== 'string') return;
      const resolution = program.getResolvedModuleFromModuleSpecifier(literal, file);
      const resolved = resolution && resolution.resolvedModule;
      const mode = modeFor(file, literal, kind);
      const value = {importer:path.resolve(file.fileName), specifier:literal.text, kind, mode,
        target_path:resolved ? path.resolve(resolved.resolvedFileName) : null,
        package_id:resolved && resolved.packageId ? [resolved.packageId.name, resolved.packageId.version, resolved.packageId.subModuleName || ''].join('/') : null};
      const requestKey = JSON.stringify([value.importer, value.specifier, value.kind, value.mode]);
      const encoded = JSON.stringify(value);
      if (resolutionKeys.has(requestKey) && resolutionKeys.get(requestKey) !== encoded) throw new Error('conflicting compiler resolutions for one request key');
      if (!resolutionKeys.has(requestKey)) { resolutionKeys.set(requestKey, encoded); resolutions.push(value); }
    };
    const visit = (file, node) => {
      if (ts.isImportDeclaration(node) && ts.isStringLiteralLike(node.moduleSpecifier)) addRequest(file,node.moduleSpecifier,'EsmImport');
      else if (ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteralLike(node.moduleSpecifier)) addRequest(file,node.moduleSpecifier,'EsmReExport');
      else if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument) && ts.isStringLiteralLike(node.argument.literal)) addRequest(file,node.argument.literal,'EsmImport');
      else if (ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference) && node.moduleReference.expression && ts.isStringLiteralLike(node.moduleReference.expression)) addRequest(file,node.moduleReference.expression,'CjsRequire');
      else if (ts.isCallExpression(node) && node.arguments.length && ts.isStringLiteralLike(node.arguments[0])) {
        if (node.expression.kind === ts.SyntaxKind.ImportKeyword) addRequest(file,node.arguments[0],'DynamicImport');
        else if (ts.isIdentifier(node.expression) && node.expression.text === 'require') addRequest(file,node.arguments[0],'CjsRequire');
      }
      ts.forEachChild(node, child => visit(file, child));
    };
    for (const file of programFiles) visit(file,file);
    const reverseName = (table, value, fallback) => {
      if (value === undefined || value === null) return fallback;
      const name = table[value];
      if (typeof name !== 'string') throw new Error('compiler option enum has no canonical name');
      return name;
    };
    const scriptTarget = options.target === undefined ? ts.ScriptTarget.ES5 : options.target;
    const targetName = reverseName(ts.ScriptTarget, scriptTarget, 'ES5');
    const resolutionValue = options.moduleResolution === undefined
      ? ((options.module === ts.ModuleKind.Node16 || options.module === ts.ModuleKind.NodeNext) ? options.module : ts.ModuleResolutionKind.Node10)
      : options.moduleResolution;
    const moduleValue = options.module === undefined
      ? (resolutionValue === ts.ModuleResolutionKind.Node16 ? ts.ModuleKind.Node16 : resolutionValue === ts.ModuleResolutionKind.NodeNext ? ts.ModuleKind.NodeNext : (scriptTarget >= ts.ScriptTarget.ES2015 ? ts.ModuleKind.ES2015 : ts.ModuleKind.CommonJS))
      : options.module;
    const strictFlag = key => options[key] === undefined ? !!options.strict : !!options[key];
    const normalized = {
      target:targetName,
      module:reverseName(ts.ModuleKind,moduleValue,'CommonJS'),
      moduleResolution:reverseName(ts.ModuleResolutionKind,resolutionValue,'Node10'),
      moduleDetection:reverseName(ts.ModuleDetectionKind,options.moduleDetection, 'Auto'),
      strict:!!options.strict,
      noImplicitAny:strictFlag('noImplicitAny'), strictNullChecks:strictFlag('strictNullChecks'),
      strictFunctionTypes:strictFlag('strictFunctionTypes'), strictBindCallApply:strictFlag('strictBindCallApply'),
      strictPropertyInitialization:strictFlag('strictPropertyInitialization'), noImplicitThis:strictFlag('noImplicitThis'),
      useUnknownInCatchVariables:strictFlag('useUnknownInCatchVariables'), alwaysStrict:strictFlag('alwaysStrict'),
      exactOptionalPropertyTypes:!!options.exactOptionalPropertyTypes,
      noUncheckedIndexedAccess:!!options.noUncheckedIndexedAccess, noImplicitReturns:!!options.noImplicitReturns,
      noPropertyAccessFromIndexSignature:!!options.noPropertyAccessFromIndexSignature,
      noImplicitOverride:!!options.noImplicitOverride, noUnusedLocals:!!options.noUnusedLocals,
      noUnusedParameters:!!options.noUnusedParameters, noFallthroughCasesInSwitch:!!options.noFallthroughCasesInSwitch,
      allowUnreachableCode:options.allowUnreachableCode, allowUnusedLabels:options.allowUnusedLabels,
      experimentalDecorators:!!options.experimentalDecorators, esModuleInterop:!!options.esModuleInterop,
      allowSyntheticDefaultImports:options.allowSyntheticDefaultImports === undefined ? !!options.esModuleInterop : !!options.allowSyntheticDefaultImports,
      resolveJsonModule:!!options.resolveJsonModule, noResolve:!!options.noResolve, allowJs:!!options.allowJs,
      checkJs:!!options.checkJs, skipLibCheck:!!options.skipLibCheck,
      noUncheckedSideEffectImports:!!options.noUncheckedSideEffectImports,
      noTypesAndSymbols:!!options.noTypesAndSymbols,
      useDefineForClassFields:options.useDefineForClassFields,
      jsx:options.jsx === undefined ? undefined : reverseName(ts.JsxEmit,options.jsx,undefined),
      jsxFactory:options.jsxFactory, jsxFragmentFactory:options.jsxFragmentFactory,
      jsxImportSource:options.jsxImportSource, reactNamespace:options.reactNamespace,
      lib:options.lib, types:options.types, typeRoots:options.typeRoots,
      baseUrl:options.baseUrl, paths:options.paths
    };
    const checkerRelevant = new Set(['target','module','moduleResolution','moduleDetection','jsx','jsxFactory','jsxFragmentFactory','jsxImportSource','reactNamespace','lib','types','typeRoots','baseUrl','paths','strict','noImplicitAny','strictNullChecks','strictFunctionTypes','strictBindCallApply','strictPropertyInitialization','noImplicitThis','useUnknownInCatchVariables','alwaysStrict','exactOptionalPropertyTypes','noUncheckedIndexedAccess','noImplicitReturns','noPropertyAccessFromIndexSignature','noImplicitOverride','noUnusedLocals','noUnusedParameters','noFallthroughCasesInSwitch','allowUnreachableCode','allowUnusedLabels','experimentalDecorators','esModuleInterop','allowSyntheticDefaultImports','resolveJsonModule','noResolve','allowJs','checkJs','skipLibCheck','noUncheckedSideEffectImports','noTypesAndSymbols','useDefineForClassFields']);
    const unsupported = [];
    for (const declaration of (ts.optionDeclarations || [])) {
      const name = declaration.name;
      if ((declaration.affectsSemanticDiagnostics || declaration.affectsBindDiagnostics) && options[name] !== undefined && !checkerRelevant.has(name)) unsupported.push(name);
    }
    result({schema:1, version:ts.version, config_path:path.resolve(configPath), compiler_options:{compilerOptions:normalized},
      project_references:[], files:sourceFiles, resolutions, accesses:Array.from(observations.values()),
      unsupported_options:Array.from(new Set(unsupported)).sort(), error:null});
  }
} catch (error) {
  result({schema:1, version:'', config_path:path.resolve(configPath), compiler_options:{compilerOptions:{}},
    project_references:[], files:[], resolutions:[], accesses:Array.from(observations.values()),
    unsupported_options:[], error:String(error && error.message || error)});
}
"#;

/// Resolves one checked TypeScript program using only the exact admitted compiler installation.
pub(crate) fn build_native_inputs(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    package_sources: &[PackageSource<'_>],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTypeScriptInputs, TypeScriptProjectHostError> {
    let config = select_config(inputs)?;
    // The admitted `compiler_path` is the executable CLI shim (`bin/tsc`). It
    // is probed separately to establish the selected tool identity, but it is
    // not the Compiler API module. Resolve that module only from the exact
    // captured TypeScript package closure so `require` cannot silently select
    // a different install from Node's module search path.
    let compiler_api_path = inputs
        .typescript_module_root
        .join("typescript/lib/typescript.js");
    let compiler_api = inputs
        .typescript_files
        .iter()
        .find(|file| normalize_path(&file.path) == normalize_path(&compiler_api_path))
        .ok_or_else(|| {
            bridge_error("the admitted TypeScript package has no witnessed Compiler API module")
        })?;
    let report = run_program_bridge(
        inputs,
        config.path.as_ref(),
        &compiler_api_path,
        deadline,
        cancelled,
    )?;
    if report.schema != 1 {
        return Err(bridge_error("unsupported compiler API report schema"));
    }
    if let Some(message) = report.error.as_deref() {
        return Err(bridge_error(message));
    }
    let expected_version = std::str::from_utf8(inputs.compiler_version)
        .ok()
        .map(str::trim)
        .and_then(|version| version.strip_prefix("Version ").or(Some(version)));
    if expected_version != Some(report.version.as_str()) {
        return Err(bridge_error(
            "the compiler API version differs from the admitted TypeScript identity",
        ));
    }
    let observed_config = Path::new(&report.config_path);
    if normalize_path(observed_config) != normalize_path(config.path.as_ref()) {
        return Err(bridge_error(
            "the compiler API parsed a different tsconfig than the selected project config",
        ));
    }
    if !report.project_references.is_empty() {
        return Err(bridge_error(
            "project references are not admitted as one TSZ project",
        ));
    }
    if let Some(option) = report.unsupported_options.first() {
        return Err(bridge_error(&format!(
            "compiler option {option:?} affects semantic analysis but is outside the TSZ option model"
        )));
    }
    if report.files.is_empty() || report.files.len() > MAX_BRIDGE_FILES {
        return Err(bridge_error(
            "compiler API program source count is empty or exceeds the admitted bound",
        ));
    }
    if report.resolutions.len() > MAX_BRIDGE_REQUESTS {
        return Err(bridge_error(
            "compiler API module-request count exceeds the admitted bound",
        ));
    }

    replay_observations(resolver, &report.accesses)?;

    let checker = compiler_api_checker_options(&report.compiler_options)?;
    let mut virtual_by_canonical = BTreeMap::<PathBuf, String>::new();
    let mut source_by_virtual = BTreeMap::<String, String>::new();
    let mut libraries = Vec::new();
    let mut content_ids = BTreeMap::<String, ContentId<SourceFactDomain>>::new();
    let mut source_paths = BTreeSet::new();

    let mut files = report.files.iter().collect::<Vec<_>>();
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    for file in files {
        let path = Path::new(&file.path);
        let Some(admitted) = resolver.try_load_source(path)? else {
            return Err(bridge_error(&format!(
                "the compiler API included a missing source file {:?}",
                file.path
            )));
        };
        let text = std::str::from_utf8(&admitted.bytes)
            .map_err(|_| bridge_error("a compiler program source is not UTF-8"))?;
        let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
        if sha256_hex(without_bom.as_bytes()) != file.sha256 {
            return Err(bridge_error(&format!(
                "the compiler API source bytes changed at {:?}",
                file.path
            )));
        }
        let (virtual_path, is_library) = if file.default_library {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| bridge_error("a compiler library path is not portable UTF-8"))?;
            (format!("{LIB_VIRTUAL_PREFIX}{name}"), true)
        } else {
            (workspace_virtual_path(inputs, &admitted.path)?, false)
        };
        if !source_paths.insert(virtual_path.clone()) {
            return Err(bridge_error(&format!(
                "two compiler program files collapse to TSZ path {virtual_path:?}"
            )));
        }
        virtual_by_canonical.insert(normalize_path(&admitted.path), virtual_path.clone());
        content_ids.insert(virtual_path.clone(), admitted.content_id);
        if is_library {
            let input = TszLibraryInput::from_utf8(virtual_path, admitted.bytes.to_vec()).map_err(
                |error| bridge_error(&format!("compiler library input was rejected: {error}")),
            )?;
            libraries.push(input.into_lib_file());
        } else {
            source_by_virtual.insert(virtual_path, text.to_owned());
        }
    }

    let mut package_paths = BTreeMap::new();
    for source in package_sources {
        let package_path = source.relative_path();
        let absolute = inputs.package_root.join(package_path);
        let Some(admitted) = resolver.try_load_source(&absolute)? else {
            return Err(bridge_error(&format!(
                "package source {package_path:?} is absent from the admitted compiler program"
            )));
        };
        let canonical = normalize_path(&admitted.path);
        let Some(virtual_path) = virtual_by_canonical.get(&canonical) else {
            return Err(bridge_error(&format!(
                "package source {package_path:?} is absent from the selected tsconfig program"
            )));
        };
        let expected = source.source().as_bytes();
        if admitted.bytes.as_ref() != expected {
            return Err(bridge_error(&format!(
                "package source bytes differ from the compiler program at {package_path:?}"
            )));
        }
        package_paths.insert(package_path.into(), virtual_path.clone().into_boxed_str());
    }

    let mut resolutions = Vec::with_capacity(report.resolutions.len());
    let mut resolution_keys = BTreeSet::new();
    for request in report.resolutions {
        let importer =
            resolve_virtual_path(inputs, resolver, &virtual_by_canonical, &request.importer)?;
        let request_kind = match request.kind.as_str() {
            "EsmImport" => TszProjectModuleRequestKind::EsmImport,
            "DynamicImport" => TszProjectModuleRequestKind::DynamicImport,
            "CjsRequire" => TszProjectModuleRequestKind::CjsRequire,
            "EsmReExport" => TszProjectModuleRequestKind::EsmReExport,
            other => TszProjectModuleRequestKind::Unsupported {
                syntax_kind: other.to_owned(),
            },
        };
        let resolution_mode = match request.mode.as_deref() {
            None => None,
            Some("import") => Some(backend_frontend_typescript::TszResolutionModeOverride::Import),
            Some("require") => {
                Some(backend_frontend_typescript::TszResolutionModeOverride::Require)
            }
            Some(other) => {
                return Err(bridge_error(&format!(
                    "unsupported TypeScript resolution mode {other:?}"
                )));
            }
        };
        let target = if let Some(target_path) = request.target_path.as_deref() {
            let target_path = Path::new(target_path);
            let Some(target_input) = resolver.try_load_source(target_path)? else {
                return Err(bridge_error(&format!(
                    "compiler resolution target {:?} was not admitted",
                    target_path
                )));
            };
            let canonical_target = normalize_path(&target_input.path);
            if let Some(path) = virtual_by_canonical.get(&canonical_target) {
                TszProjectModuleResolutionTarget::File { path: path.clone() }
            } else {
                let mut identity = Sha256::new();
                identity.update(b"typescript.external-module-target.v1\0");
                identity.update(target_input.content_id.as_ref());
                identity.update(target_input.path.as_os_str().as_encoded_bytes());
                if let Some(package_id) = request.package_id.as_deref() {
                    identity.update(package_id.as_bytes());
                }
                TszProjectModuleResolutionTarget::External {
                    identity: format!("ts-module:{}", hex_digest(&identity.finalize())),
                }
            }
        } else {
            TszProjectModuleResolutionTarget::Unresolved
        };
        let resolution = TszProjectModuleResolution {
            importer_path: importer,
            specifier: request.specifier,
            request_kind,
            resolution_mode,
            target,
        };
        if !resolution_keys.insert(format!("{:?}", resolution)) {
            return Err(bridge_error(
                "compiler API returned a duplicate module-resolution record",
            ));
        }
        resolutions.push(resolution);
    }
    resolutions.sort_by(|left, right| {
        (
            &left.importer_path,
            &left.specifier,
            format!("{:?}", left.request_kind),
            format!("{:?}", left.resolution_mode),
        )
            .cmp(&(
                &right.importer_path,
                &right.specifier,
                format!("{:?}", right.request_kind),
                format!("{:?}", right.resolution_mode),
            ))
    });

    let mut sources = Vec::with_capacity(source_by_virtual.len());
    for (path, source) in source_by_virtual {
        sources.push(TszFileInput { path, source });
    }
    let resolver_digest = resolver.resolver_witness()?;
    resolver.validate_current()?;
    let environment = program_environment_fingerprint(
        inputs,
        config,
        &report.version,
        &report.compiler_options,
        &content_ids,
        &compiler_api.content_id,
        &resolutions,
        &resolver_digest,
    )?;
    let work_units = calculate_work_units(inputs, &sources, &libraries);
    let options = TszProjectOptions {
        checker,
        semantic_options: TszProjectSemanticOptions::declaration_scoped(),
        module_resolutions: resolutions,
        environment,
    };
    Ok(NativeTypeScriptInputs {
        sources,
        libraries,
        options,
        package_paths,
        work_units,
    })
}

fn run_program_bridge(
    inputs: &TypeScriptProjectInputs<'_>,
    config_path: &Path,
    compiler_api_path: &Path,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<ProgramReport, TypeScriptProjectHostError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(bridge_error(
            "deadline elapsed before compiler API program construction",
        ));
    }
    let limits = ToolchainProbeLimits::new(
        remaining,
        NonZeroUsize::new(MAX_BRIDGE_STDOUT_BYTES).expect("nonzero bridge stream cap"),
    )
    .map_err(|error| bridge_error(&error.to_string()))?;
    let arguments = [
        OsString::from(compiler_api_path.as_os_str()),
        OsString::from(config_path.as_os_str()),
        OsString::from(inputs.workspace_root.as_os_str()),
    ];
    let output = run_typescript_program_bridge(
        inputs.node_path,
        COMPILER_API_PROGRAM_SCRIPT,
        &arguments,
        inputs.package_root,
        limits,
        cancelled,
    )
    .map_err(|error| bridge_error(&error.to_string()))?;
    serde_json::from_slice(&output).map_err(|error| {
        bridge_error(&format!(
            "compiler API output was not bounded valid JSON: {error}"
        ))
    })
}

fn select_config<'a>(
    inputs: &'a TypeScriptProjectInputs<'_>,
) -> Result<&'a super::typescript_host::TypeScriptConfigInput, TypeScriptProjectHostError> {
    let selected = inputs
        .config_candidates
        .iter()
        .filter(|candidate| candidate.selected_build_config)
        .collect::<Vec<_>>();
    if selected.len() == 1 {
        return Ok(selected[0]);
    }
    if selected.len() > 1 {
        return Err(bridge_error(
            "the host admitted more than one selected TypeScript build config",
        ));
    }
    let mut candidates = inputs
        .config_candidates
        .iter()
        .filter(|candidate| candidate.project_candidate)
        .filter(|candidate| {
            inputs
                .package_root
                .starts_with(candidate.path.parent().unwrap_or(Path::new("/")))
        })
        .collect::<Vec<_>>();
    candidates
        .sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.path.components().count()));
    match candidates.as_slice() {
        [only] => Ok(*only),
        [first, second, ..]
            if first.path.components().count() == second.path.components().count() =>
        {
            Err(bridge_error(
                "the nearest TypeScript project config is ambiguous",
            ))
        }
        [first, ..] => Ok(*first),
        [] => Err(bridge_error(
            "no admitted TypeScript project config applies to this package",
        )),
    }
}

fn compiler_api_checker_options(
    options: &serde_json::Value,
) -> Result<TszCheckerOptions, TypeScriptProjectHostError> {
    let json = serde_json::to_string(options).map_err(|error| {
        bridge_error(&format!(
            "normalized TypeScript options could not be encoded: {error}"
        ))
    })?;
    checker_options_from_compiler_api_json(&json).map_err(|error| {
        bridge_error(&format!(
            "normalized TypeScript checker options are unsupported: {error}"
        ))
    })
}

fn replay_observations(
    resolver: &mut TypeScriptResolverCapability<'_>,
    accesses: &[ProgramAccess],
) -> Result<(), TypeScriptProjectHostError> {
    let mut observed = BTreeSet::new();
    for access in accesses {
        let key = format!("{}:{}", access.kind, access.path);
        if !observed.insert(key) {
            continue;
        }
        let path = Path::new(&access.path);
        match access.kind.as_str() {
            "file" | "exists" => {
                let source = resolver.try_load_source(path)?;
                if source.is_some() != access.exists.unwrap_or(false) {
                    return Err(bridge_error(&format!(
                        "compiler filesystem result changed at {:?}",
                        access.path
                    )));
                }
                if let (Some(source), Some(expected)) = (source, access.sha256.as_deref())
                    && sha256_hex(&source.bytes) != expected
                {
                    return Err(bridge_error(&format!(
                        "compiler filesystem bytes changed at {:?}",
                        access.path
                    )));
                }
            }
            "directory" => {
                if resolver.directory_exists(path)? != access.exists.unwrap_or(false) {
                    return Err(bridge_error(&format!(
                        "compiler directory result changed at {:?}",
                        access.path
                    )));
                }
            }
            "realpath" => {
                let observed_path = resolver.realpath(path)?;
                let expected = access
                    .realpath
                    .as_deref()
                    .map(Path::new)
                    .map(normalize_path);
                if observed_path.as_deref().map(normalize_path) != expected {
                    return Err(bridge_error(&format!(
                        "compiler realpath result changed at {:?}",
                        access.path
                    )));
                }
            }
            "readDirectory" | "getDirectories" => {
                let extensions = access
                    .extensions
                    .as_deref()
                    .filter(|items| !items.is_empty())
                    .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>())
                    .unwrap_or_else(|| {
                        vec![
                            ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json",
                            ".d.ts",
                        ]
                    });
                let recursive = access.kind == "getDirectories" || access.recursive.unwrap_or(true);
                let depth = access.depth.unwrap_or(32).min(32);
                let _ = resolver.read_directory(path, &extensions, recursive, depth, 16_384)?;
            }
            other => {
                return Err(bridge_error(&format!(
                    "compiler reported unsupported filesystem operation {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn resolve_virtual_path(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    paths: &BTreeMap<PathBuf, String>,
    path: &str,
) -> Result<String, TypeScriptProjectHostError> {
    let Some(source) = resolver.try_load_source(Path::new(path))? else {
        return Err(bridge_error(&format!(
            "compiler module importer {path:?} is not admitted"
        )));
    };
    if let Some(virtual_path) = paths.get(&normalize_path(&source.path)) {
        return Ok(virtual_path.clone());
    }
    workspace_virtual_path(inputs, &source.path)
}

fn workspace_virtual_path(
    inputs: &TypeScriptProjectInputs<'_>,
    path: &Path,
) -> Result<String, TypeScriptProjectHostError> {
    let relative = path.strip_prefix(inputs.workspace_root).map_err(|_| {
        bridge_error(&format!(
            "TypeScript program source {:?} is outside its admitted workspace",
            path
        ))
    })?;
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(value) = component else {
            return Err(bridge_error(
                "TypeScript program path is not a normalized workspace-relative path",
            ));
        };
        let value = value
            .to_str()
            .ok_or_else(|| bridge_error("TypeScript source path is not portable UTF-8"))?;
        components.push(value);
    }
    if components.is_empty() {
        return Err(bridge_error(
            "TypeScript program path names the workspace root",
        ));
    }
    Ok(components.join("/"))
}

fn program_environment_fingerprint(
    inputs: &TypeScriptProjectInputs<'_>,
    config: &super::typescript_host::TypeScriptConfigInput,
    compiler_version: &str,
    options: &serde_json::Value,
    content_ids: &BTreeMap<String, ContentId<SourceFactDomain>>,
    compiler_api_content_id: &ContentId<SourceFactDomain>,
    resolutions: &[TszProjectModuleResolution],
    resolver_digest: &[u8; 32],
) -> Result<TszEnvironmentFingerprint, TypeScriptProjectHostError> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"compiler.typescript.tsz-program.v1\0");
    digest.update(&inputs.fingerprint);
    digest.update(compiler_version.as_bytes());
    digest.update(config.content_id.as_ref());
    digest.update(compiler_api_content_id.as_ref());
    digest.update(resolver_digest);
    let options = serde_json::to_vec(options).map_err(|error| {
        bridge_error(&format!(
            "normalized options could not be fingerprinted: {error}"
        ))
    })?;
    digest.update(&(options.len() as u64).to_le_bytes());
    digest.update(&options);
    for (path, content) in content_ids {
        digest.update(&(path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(content.as_ref());
    }
    for resolution in resolutions {
        let encoded = format!("{resolution:?}");
        digest.update(&(encoded.len() as u64).to_le_bytes());
        digest.update(encoded.as_bytes());
    }
    Ok(TszEnvironmentFingerprint::from_sha256(
        *digest.finalize().as_bytes(),
    ))
}

fn calculate_work_units(
    inputs: &TypeScriptProjectInputs<'_>,
    sources: &[TszFileInput],
    libraries: &[std::sync::Arc<backend_frontend_typescript::TszLibFile>],
) -> u64 {
    let source_bytes = sources
        .iter()
        .map(|source| source.source.len() as u64)
        .sum::<u64>();
    let library_bytes = libraries
        .iter()
        .map(|library| {
            library
                .arena
                .get_source_file_at(library.root_index)
                .map(|source| source.text.len() as u64)
                .unwrap_or(0)
        })
        .sum::<u64>();
    let config_bytes = inputs
        .config_candidates
        .iter()
        .map(|config| config.bytes.len() as u64)
        .sum::<u64>();
    let bytes = source_bytes
        .saturating_add(library_bytes)
        .saturating_add(config_bytes);
    let count = (sources.len() as u64)
        .saturating_add(libraries.len() as u64)
        .saturating_add(inputs.typescript_files.len() as u64);
    1_000_000_u64
        .saturating_add(bytes.saturating_mul(48))
        .saturating_add(count.saturating_mul(65_536))
        .min(MAX_PROJECT_WORK_UNITS)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn bridge_error(message: &str) -> TypeScriptProjectHostError {
    TypeScriptProjectHostError::CompilerApiBridge {
        message: message.into(),
    }
}
