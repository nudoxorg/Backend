//! Direct TSZ configuration, root selection and module closure.
//!
//! All filesystem operations use the caller's captured capability. Standard library
//! declarations are immutable linked data; no JavaScript compiler or runtime is used.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use tsz::config::{ResolvedCompilerOptions, TsConfig};
use tsz::module_resolver::{ImportKind, ImportingModuleKind, ModuleResolver, ResolutionFailure};
use tsz::parser::node::NodeAccess;
use tsz::parser::{MeteredParserState, NodeIndex, syntax_kind_ext};
use tsz::scanner::SyntaxKind;
use tsz_common::ExecutionCheckpoint;

pub use tsz::{ResolutionDirEntry, ResolutionFileKind, ResolutionFileSystem};

use crate::{
    TszCheckerOptions, TszProjectFileFormat, TszProjectModuleRequestKind, TszResolutionModeOverride,
};

const MAX_FILES: usize = 4096;
const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 65_536;
const MAX_DIRECTORY_DEPTH: usize = 32;
const MAX_REQUESTS: usize = 131_072;
const MAX_PATTERN_BYTES: usize = 512 * 1024;

/// Source and compiler inputs resolved for one native TypeScript program.
pub struct NativeProgram {
    /// Canonical source paths and UTF-8 text included in the program.
    pub sources: BTreeMap<PathBuf, String>,
    /// Embedded standard-library declaration names and source text.
    pub libraries: Vec<(String, &'static str)>,
    /// Checker options resolved from the project's compiler configuration.
    pub checker: TszCheckerOptions,
    /// Source module requests with their resolver outcomes.
    pub resolutions: Vec<NativeModuleResolution>,
    /// Source paths paired with the resolver-selected ESM or CommonJS format.
    pub file_formats: Vec<(PathBuf, TszProjectFileFormat)>,
    /// Stable native options, including resolver policy, retained in the recipe.
    pub options_identity: String,
    /// Root source files selected by the project configuration or inferred inputs.
    pub root_names: Vec<PathBuf>,
    /// Diagnostics produced while loading and resolving project configuration.
    pub program_diagnostics: Vec<crate::TszDiagnostic>,
    /// Bounded filesystem transcript for configured root discovery. It is
    /// replayed before a retained native resolver proof may tolerate file-set
    /// changes in directories it enumerated.
    pub root_selection: Option<NativeRootSelectionTranscript>,
}

/// Exact configured-root query needed to replay TypeScript's root matcher
/// against a fresh admitted filesystem view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRootSelectionTranscript {
    pub config_path: PathBuf,
    pub root_names: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
}

/// Replays only configured source-root selection. Module parsing and checking
/// are deliberately not performed; the resolver ledger separately replays all
/// positive and negative module candidates.
pub fn replay_root_selection(
    fs: &dyn ResolutionFileSystem,
    config_path: &Path,
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<NativeRootSelectionTranscript, String> {
    replay_root_selection_controlled(fs, config_path, &|work| poll(checkpoint, work))
}

/// Replays roots with a local caller control, including polls within glob matching.
/// This callback stays on the replay thread; compiler execution retains its
/// separate Send + Sync checkpoint contract.
pub fn replay_root_selection_controlled(
    fs: &dyn ResolutionFileSystem,
    config_path: &Path,
    control: &dyn Fn(u64) -> Result<(), String>,
) -> Result<NativeRootSelectionTranscript, String> {
    let checkpoint = &RootReplayCheckpoint(control);
    poll(checkpoint, 0)?;
    let parsed = tsz::config::load_tsconfig_with_diagnostics_and_file_system(config_path, fs)
        .map_err(|error| error.to_string())?;
    let mut options =
        tsz::config::resolve_compiler_options_embedded(parsed.config.compiler_options.as_ref())
            .map_err(|error| error.to_string())?;
    if options.base_url.is_none()
        && options
            .paths
            .as_ref()
            .is_some_and(|paths| !paths.is_empty())
        && let Some(paths_base_path) = parsed.paths_base_path
    {
        options.paths_base_path = Some(paths_base_path);
    }
    let (root_names, directories) =
        select_roots(fs, config_path, &parsed.config, &options, checkpoint)?;
    Ok(NativeRootSelectionTranscript {
        config_path: config_path.to_path_buf(),
        root_names,
        directories,
    })
}

/// Resolver metadata for one module request in a program source file.
pub struct NativeModuleResolution {
    /// Canonical path of the source file containing the request.
    pub importer: PathBuf,
    /// Exact module specifier written in the source file.
    pub specifier: String,
    /// Syntax form of the module request.
    pub kind: TszProjectModuleRequestKind,
    /// Optional import or require mode selected for resolution.
    pub mode: Option<TszResolutionModeOverride>,
    /// Resolved target path, or `None` when no target was resolved.
    pub target: Option<PathBuf>,
}

/// Builds one actual program using the linked compiler's config and resolver.
/// The caller validates the returned exact closure before semantic admission.
pub fn build(
    fs: &dyn ResolutionFileSystem,
    config_path: Option<&Path>,
    inferred_roots: &[PathBuf],
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<NativeProgram, String> {
    build_with_root_selection_observer(fs, config_path, inferred_roots, checkpoint, &|_| Ok(()))
}

/// Reports completed root enumeration before module resolution can enumerate any
/// of the same directories. Consumers retain these as separate observations.
pub fn build_with_root_selection_observer(
    fs: &dyn ResolutionFileSystem,
    config_path: Option<&Path>,
    inferred_roots: &[PathBuf],
    checkpoint: &dyn ExecutionCheckpoint,
    root_selection_complete: &dyn Fn(&NativeRootSelectionTranscript) -> Result<(), String>,
) -> Result<NativeProgram, String> {
    poll(checkpoint, 0)?;
    let (config, mut program_diagnostics, paths_base_path) = match config_path {
        Some(path) => {
            let parsed = tsz::config::load_tsconfig_with_diagnostics_and_file_system(path, fs)
                .map_err(|error| error.to_string())?;
            (parsed.config, parsed.diagnostics, parsed.paths_base_path)
        }
        None => (TsConfig::default(), Vec::new(), None),
    };
    let mut options =
        tsz::config::resolve_compiler_options_embedded(config.compiler_options.as_ref())
            .map_err(|error| error.to_string())?;
    // TypeScript 4.1 permits `paths` without `baseUrl`. In that form, each
    // substitution is relative to the tsconfig directory. TSZ's resolver has
    // an explicit `paths_base_path` for this case. The config loader retains
    // which config declared the effective mapping across `extends` merging.
    // Preserve the resolver's separate `base_url` fallback semantics by
    // setting only the paths anchor.
    if options.base_url.is_none()
        && options
            .paths
            .as_ref()
            .is_some_and(|paths| !paths.is_empty())
        && let Some(paths_base_path) = paths_base_path
    {
        options.paths_base_path = Some(paths_base_path);
    }
    // Resolver options also determine checker diagnostics and must travel with it.
    options.checker.allow_importing_ts_extensions = options.allow_importing_ts_extensions;
    options.checker.rewrite_relative_import_extensions = options.rewrite_relative_import_extensions;
    let options_identity = format!("{options:?}");
    let (roots, root_selection) = match config_path {
        Some(path) => {
            let (roots, directories) = select_roots(fs, path, &config, &options, checkpoint)?;
            let transcript = NativeRootSelectionTranscript {
                config_path: path.to_path_buf(),
                root_names: roots.clone(),
                directories,
            };
            (roots, Some(transcript))
        }
        None => (inferred_roots.to_vec(), None),
    };
    if let Some(query) = &root_selection {
        root_selection_complete(query)?;
    }
    let mut resolver = ModuleResolver::with_file_system(&options, fs);
    let solution = roots.is_empty()
        && config
            .references
            .as_ref()
            .is_some_and(|references| !references.is_empty());
    if solution {
        return Ok(NativeProgram {
            sources: BTreeMap::new(),
            libraries: Vec::new(),
            checker: options.checker,
            resolutions: Vec::new(),
            file_formats: Vec::new(),
            options_identity,
            root_names: roots,
            program_diagnostics,
            root_selection,
        });
    }
    let mut pending = roots.iter().cloned().collect::<VecDeque<_>>();
    // Explicit configured types are roots; absent `types` admits discovered @types
    // packages from captured ancestor node_modules, as TypeScript programs do.
    add_type_roots(
        fs,
        config_path
            .and_then(Path::parent)
            .or_else(|| inferred_roots.first().and_then(|p| p.parent())),
        &options,
        &mut resolver,
        &mut pending,
        &mut program_diagnostics,
        checkpoint,
    )?;
    let mut sources = BTreeMap::new();
    let mut file_formats = Vec::new();
    let mut resolutions = Vec::new();
    let mut request_keys = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut source_lib_references = BTreeSet::new();
    let mut suppress_configured_libraries = false;
    while let Some(path) = pending.pop_front() {
        poll(checkpoint, 1)?;
        let path = fs
            .canonicalize(&path)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("native TSZ source disappeared: {}", path.display()))?;
        if sources.contains_key(&path) {
            continue;
        }
        if sources.len() >= MAX_FILES {
            return Err("native TSZ source count exceeds captured bound".into());
        }
        let bytes = fs
            .read_file(&path)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("native TSZ source disappeared: {}", path.display()))?;
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > MAX_SOURCE_BYTES {
            return Err("native TSZ source bytes exceed captured bound".into());
        }
        let text =
            String::from_utf8(bytes).map_err(|_| "native TSZ source is not UTF-8".to_owned())?;
        let format = match resolver.get_importing_module_kind(&path) {
            ImportingModuleKind::Esm => TszProjectFileFormat::EsModule,
            ImportingModuleKind::CommonJs => TszProjectFileFormat::CommonJs,
        };
        resolver
            .check_file_system_error()
            .map_err(|error| error.to_string())?;
        file_formats.push((path.clone(), format));
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            // A resolved JSON module is a real program member, not a TS
            // import-scanning input. The authority's existing native JSON
            // parse/bind path validates its syntax and the checker derives
            // its value type from the retained text. Keep the exact bytes.
            sources.insert(path, text);
            continue;
        }
        let mut parser = MeteredParserState::new(path.to_string_lossy().into_owned(), text.clone());
        parser
            .parse_source_file_with_execution_checkpoint(checkpoint)
            .map_err(|error| format!("native TSZ source parse stopped: {error:?}"))?;
        let arena = &parser.arena;
        {
            let mut type_imports = BTreeSet::new();
            for mut name in arena
                .type_refs
                .iter()
                .map(|reference| reference.type_name)
                .chain(arena.type_queries.iter().map(|query| query.expr_name))
            {
                while let Some(node) = arena.get(name) {
                    if let Some(qualified) = arena.get_qualified_name(node) {
                        name = qualified.left;
                    } else {
                        if arena.get_call_expr(node).is_some() {
                            type_imports.insert(name.0);
                        }
                        break;
                    }
                }
            }
            for (raw_index, node) in arena.nodes.iter().enumerate() {
                poll(checkpoint, 1)?;
                let (specifier_index, kind, attributes) =
                    if let Some(import) = arena.get_import_decl(node) {
                        (
                            import.module_specifier,
                            if node.kind == syntax_kind_ext::IMPORT_EQUALS_DECLARATION {
                                ImportKind::CjsRequire
                            } else {
                                ImportKind::EsmImport
                            },
                            import.attributes,
                        )
                    } else if let Some(export) = arena.get_export_decl(node) {
                        (
                            export.module_specifier,
                            ImportKind::EsmReExport,
                            export.attributes,
                        )
                    } else if let Some(call) = arena.get_call_expr(node) {
                        let Some(expression) = arena.get(call.expression) else {
                            continue;
                        };
                        let kind = if expression.kind == SyntaxKind::ImportKeyword as u16 {
                            if type_imports.contains(&(raw_index as u32)) {
                                ImportKind::EsmImport
                            } else {
                                ImportKind::DynamicImport
                            }
                        } else if arena.get_identifier_text(call.expression) == Some("require") {
                            ImportKind::CjsRequire
                        } else {
                            continue;
                        };
                        let Some(argument) =
                            call.arguments.as_ref().and_then(|args| args.nodes.first())
                        else {
                            continue;
                        };
                        (*argument, kind, NodeIndex::NONE)
                    } else {
                        continue;
                    };
                let Some(specifier) = arena.get_literal_text(specifier_index) else {
                    continue;
                };
                let mode_override = explicit_resolution_mode(arena, attributes).or_else(|| {
                    arena
                        .get_call_expr(node)
                        .and_then(|call| call.arguments.as_ref())
                        .and_then(|arguments| arguments.nodes.get(1))
                        .copied()
                        .and_then(|options| object_property(arena, options, "with"))
                        .and_then(|with| object_property(arena, with, "resolution-mode"))
                        .and_then(|value| arena.get_literal_text(value))
                        .and_then(|value| match value {
                            "import" => Some(ImportingModuleKind::Esm),
                            "require" => Some(ImportingModuleKind::CommonJs),
                            _ => None,
                        })
                });
                let mode = resolver.importing_module_kind_for_import(&path, kind, mode_override);
                let target = resolver
                    .resolve_with_kind_and_module_kind_checked(
                        specifier,
                        &path,
                        tsz::span::Span::new(node.pos, node.end),
                        kind,
                        mode_override,
                    )
                    .map_err(|error| error.to_string())?
                    .ok()
                    .map(|resolved| resolved.resolved_path);
                let key = (
                    path.clone(),
                    specifier.to_owned(),
                    format!("{kind:?}"),
                    format!("{mode:?}"),
                );
                if request_keys.insert(key) {
                    if resolutions.len() >= MAX_REQUESTS {
                        return Err("native TSZ module request count exceeds bound".into());
                    }
                    if !options.no_resolve
                        && let Some(target) = &target
                    {
                        pending.push_back(target.clone());
                    }
                    resolutions.push(NativeModuleResolution {
                        importer: path.clone(),
                        specifier: specifier.to_owned(),
                        kind: match kind {
                            ImportKind::EsmImport => TszProjectModuleRequestKind::EsmImport,
                            ImportKind::EsmReExport => TszProjectModuleRequestKind::EsmReExport,
                            ImportKind::DynamicImport => TszProjectModuleRequestKind::DynamicImport,
                            ImportKind::CjsRequire => TszProjectModuleRequestKind::CjsRequire,
                        },
                        mode: Some(match mode {
                            ImportingModuleKind::Esm => TszResolutionModeOverride::Import,
                            ImportingModuleKind::CommonJs => TszResolutionModeOverride::Require,
                        }),
                        target,
                    });
                }
            }
            if arena.nodes.iter().any(|node| {
                matches!(
                    node.kind,
                    syntax_kind_ext::JSX_ELEMENT
                        | syntax_kind_ext::JSX_SELF_CLOSING_ELEMENT
                        | syntax_kind_ext::JSX_FRAGMENT
                )
            }) && let Some(specifier) = jsx_runtime_specifier(&options.checker, &text)
            {
                poll(checkpoint, 1)?;
                let kind = ImportKind::EsmImport;
                let mode = resolver.importing_module_kind_for_import(&path, kind, None);
                let target = resolver
                    .resolve_with_kind_checked(&specifier, &path, tsz::span::Span::new(0, 0), kind)
                    .map_err(|error| error.to_string())?
                    .ok()
                    .map(|resolved| resolved.resolved_path);
                let key = (
                    path.clone(),
                    specifier.clone(),
                    format!("{kind:?}"),
                    format!("{mode:?}"),
                );
                if request_keys.insert(key) {
                    if resolutions.len() >= MAX_REQUESTS {
                        return Err("native TSZ module request count exceeds bound".into());
                    }
                    if !options.no_resolve
                        && let Some(target) = &target
                    {
                        pending.push_back(target.clone());
                    }
                    resolutions.push(NativeModuleResolution {
                        importer: path.clone(),
                        specifier,
                        kind: TszProjectModuleRequestKind::EsmImport,
                        mode: Some(match mode {
                            ImportingModuleKind::Esm => TszResolutionModeOverride::Import,
                            ImportingModuleKind::CommonJs => TszResolutionModeOverride::Require,
                        }),
                        target,
                    });
                }
            }
            for directive in
                tsz::checker::triple_slash_validator::reference_directives(&text, checkpoint)
            {
                let directive = directive
                    .map_err(|stop| format!("native TSZ directive scan stopped: {stop:?}"))?;
                poll(checkpoint, 1)?;
                suppress_configured_libraries |= directive
                    .no_default_lib
                    .is_some_and(|value| value.text == "true");
                if let Some(lib) = directive.lib {
                    source_lib_references.insert(lib.text.to_owned());
                }
                if options.no_resolve {
                    continue;
                }
                if let Some(reference) = directive.path {
                    let target = tsz::checker::triple_slash_validator::resolve_reference_path_with(
                        &path,
                        &reference.text.replace('\\', "/"),
                        options.allow_js
                            || matches!(
                                path.extension().and_then(|extension| extension.to_str()),
                                Some("js" | "jsx" | "mjs" | "cjs")
                            ),
                        |candidate| fs.is_file(candidate),
                    )
                    .map_err(|error| error.to_string())?
                    .map(|target| normalize(&target));
                    if let Some(target) = &target {
                        pending.push_back(target.clone());
                    }
                    if resolutions.len() >= MAX_REQUESTS {
                        return Err("native TSZ module request count exceeds bound".into());
                    }
                    resolutions.push(NativeModuleResolution {
                        importer: path.clone(),
                        specifier: reference.text.to_owned(),
                        kind: TszProjectModuleRequestKind::ReferencePath {
                            offset: reference.offset as u32,
                        },
                        mode: None,
                        target,
                    });
                }
                if let Some(reference) = directive.types {
                    let offset = reference.offset as u32;
                    let reference = reference.text;
                    let mode = match directive.resolution_mode.map(|value| value.text) {
                        None => None,
                        Some("import") => Some(ImportingModuleKind::Esm),
                        Some("require") => Some(ImportingModuleKind::CommonJs),
                        Some(_) => {
                            return Err(
                                "native TSZ type reference has invalid resolution-mode".into()
                            );
                        }
                    };
                    let target = match resolver
                        .resolve_type_reference_directive_checked(
                            reference,
                            &path,
                            options.type_roots.as_deref(),
                            mode,
                        )
                        .map_err(|error| error.to_string())?
                    {
                        Ok(target) => Some(target.resolved_path),
                        Err(ResolutionFailure::NotFound { .. }) => None,
                        Err(error) => {
                            return Err(format!(
                                "native TSZ type reference {reference:?} refused: {error:?}"
                            ));
                        }
                    };
                    if let Some(target) = &target {
                        pending.push_back(target.clone());
                    }
                    if resolutions.len() >= MAX_REQUESTS {
                        return Err("native TSZ module request count exceeds bound".into());
                    }
                    resolutions.push(NativeModuleResolution {
                        importer: path.clone(),
                        specifier: reference.to_owned(),
                        kind: TszProjectModuleRequestKind::ReferenceTypes { offset },
                        mode: mode.map(|mode| match mode {
                            ImportingModuleKind::Esm => TszResolutionModeOverride::Import,
                            ImportingModuleKind::CommonJs => TszResolutionModeOverride::Require,
                        }),
                        target,
                    });
                }
            }
        }
        sources.insert(path, text);
    }
    // Linked declarations are data dependencies. Their bytes enter the program's
    // identity even when a project has no TypeScript package installed.
    // A source no-default-lib suppresses default/configured libraries, while
    // explicit source lib references still load unless noLib itself is set.
    let mut libs = if suppress_configured_libraries {
        Vec::new()
    } else {
        options.lib_files.clone()
    };
    if !options.checker.no_lib && !source_lib_references.is_empty() {
        libs.extend(
            tsz::config::resolve_lib_files_from_embedded(
                &source_lib_references.into_iter().collect::<Vec<_>>(),
                true,
            )
            .map_err(|error| error.to_string())?,
        );
    }
    let mut libraries = BTreeMap::new();
    for path in libs {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("embedded lib name is not UTF-8")?;
        let bytes = tsz::embedded_libs::get_lib_content(name)
            .ok_or_else(|| format!("linked TSZ declaration {name} is missing"))?;
        libraries.insert(name.to_owned(), bytes);
    }
    poll(checkpoint, 0)?;
    Ok(NativeProgram {
        sources,
        libraries: libraries.into_iter().collect(),
        checker: options.checker,
        resolutions,
        file_formats,
        options_identity,
        root_names: roots,
        program_diagnostics,
        root_selection,
    })
}

/// A TSZ-parsed function declaration and the authored header before its body.
/// Offsets address the exact UTF-8 source supplied to the parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeFunctionHeader {
    /// Complete declaration extent used to join an emitted compiler entity.
    pub declaration: (u32, u32),
    /// Exact TSZ-owned name extent in these source bytes.
    pub name: (u32, u32),
    /// Start of a singular parser-owned export wrapper, when present.
    pub export_start: Option<u32>,
    /// Start of the TSZ-owned function body.
    pub body_start: u32,
    /// Whether TSZ observed an async function.
    pub is_async: bool,
}

/// Extracts bounded function-header spans from TSZ syntax without inferring
/// declarations from their names. A stopped parse contributes no partial spans.
pub fn function_headers(
    path: &Path,
    text: &str,
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<Vec<NativeFunctionHeader>, String> {
    const MAX_SOURCE: usize = 256 * 1024;
    poll(checkpoint, 0)?;
    if text.len() > MAX_SOURCE { return Ok(Vec::new()); }
    let mut parser = MeteredParserState::new(path.to_string_lossy().into_owned(), text.to_owned());
    parser.parse_source_file_with_execution_checkpoint(checkpoint)
        .map_err(|error| format!("native TSZ header parse stopped: {error:?}"))?;
    if !parser.get_diagnostics().is_empty() { return Ok(Vec::new()); }
    function_headers_from_arena(&parser.arena, text, checkpoint)
}

/// Extracts compact header claims from the native compiler's already-admitted
/// TSZ arena. The caller owns and budgets the arena and exact source bytes.
pub fn function_headers_from_arena(
    arena: &crate::TszNodeArena,
    text: &str,
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<Vec<NativeFunctionHeader>, String> {
    const MAX_SOURCE: usize = 1024 * 1024;
    const MAX_HEADERS: usize = 16384;
    poll(checkpoint, 0)?;
    if text.len() > MAX_SOURCE { return Err("native TSZ header source limit exceeded".into()); }
    let mut export_starts = BTreeMap::<u32, Option<u32>>::new();
    for (index, node) in arena.nodes.iter().enumerate() {
        poll(checkpoint, 1)?;
        let Some(export) = arena.get_export_decl(node) else { continue };
        let Some(index) = u32::try_from(index).ok().map(NodeIndex) else { return Err("native TSZ node index overflow".into()) };
        let Some((start, _)) = arena.pos_end_at(index) else { continue };
        // Default exports require their own declaration/identity proof.
        let value = (!export.is_default_export).then_some(start);
        if !export_starts.contains_key(&export.export_clause.0) && export_starts.len() == MAX_HEADERS {
            poll(checkpoint, 0)?;
            return Err("native TSZ export limit exceeded".into());
        }
        export_starts.entry(export.export_clause.0)
            .and_modify(|value| *value = None).or_insert(value);
    }
    let mut headers = Vec::new();
    for (index, node) in arena.nodes.iter().enumerate() {
        poll(checkpoint, 1)?;
        if node.kind != syntax_kind_ext::FUNCTION_DECLARATION { continue; }
        let Some(function) = arena.get_function(node) else { continue };
        let Some(index) = u32::try_from(index).ok().map(NodeIndex) else { return Err("native TSZ node index overflow".into()) };
        let Some((start, end)) = arena.pos_end_at(index) else { continue };
        let Some((body_start, _)) = arena.pos_end_at(function.body) else { continue };
        let Some((name_start, name_end)) = arena.pos_end_at(function.name) else { continue };
        if !(start <= name_start && name_start < name_end && name_end <= body_start && body_start < end) { continue; }
        let export_start = match export_starts.get(&index.0) {
            Some(Some(start)) => Some(*start),
            Some(None) => continue,
            None => None,
        };
        if export_start.is_some_and(|offset| offset > start)
            || text.get(start as usize..end as usize).is_none()
            || text.get(name_start as usize..name_end as usize).is_none() { continue; }
        if headers.len() == MAX_HEADERS {
            poll(checkpoint, 0)?;
            return Err("native TSZ header limit exceeded".into());
        }
        headers.push(NativeFunctionHeader {
            declaration: (start, end), name: (name_start, name_end), export_start,
            body_start, is_async: function.is_async,
        });
    }
    headers.sort_unstable_by_key(|header| header.declaration);
    if headers.windows(2).any(|pair| pair[0].declaration == pair[1].declaration) {
        poll(checkpoint, 0)?;
        return Ok(Vec::new());
    }
    poll(checkpoint, 0)?;
    Ok(headers)
}

/// Index admitted captures in place by end offset. The caller budgets the
/// existing rows; this index has no second row allocation.
pub fn index_function_headers_by_end(rows: &mut [NativeFunctionHeader], checkpoint: &dyn ExecutionCheckpoint) -> Result<(), String> {
    let work = u64::try_from(rows.len()).ok().and_then(|n| n.checked_mul(64)).ok_or("native header index work overflow")?;
    poll(checkpoint, work)?;
    rows.sort_unstable_by_key(|row| (row.declaration.1, row.declaration.0));
    poll(checkpoint, 0)
}

/// Return every same-end candidate, retaining ambiguity for the exact join.
pub fn function_headers_with_end<'a>(rows: &'a [NativeFunctionHeader], end: u32, checkpoint: &dyn ExecutionCheckpoint) -> Result<&'a [NativeFunctionHeader], String> {
    let steps = u64::from(usize::BITS - rows.len().leading_zeros()).saturating_mul(2).saturating_add(2);
    poll(checkpoint, steps)?;
    let first = rows.partition_point(|row| row.declaration.1 < end);
    let last = rows.partition_point(|row| row.declaration.1 <= end);
    poll(checkpoint, 0)?;
    Ok(&rows[first..last])
}

/// Joins parser captures to an independently selected compiler-owned function.
/// A same-name collision or an ambiguous extent yields no header. The caller
/// must first authenticate the source bytes, image, entity and source policy.
pub fn selected_function_header<'a>(
    rows: &[NativeFunctionHeader], text: &'a str, compiler_extent: (u32, u32), name: &str,
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<Option<&'a str>, String> {
    poll(checkpoint, 0)?;
    let mut selected = None;
    for row in rows {
        poll(checkpoint, 1)?;
        let (start, end) = row.declaration;
        let (compiler_start, compiler_end) = compiler_extent;
        let header_start = row.export_start.unwrap_or(compiler_start);
        if compiler_end == end && header_start <= compiler_start && compiler_start <= start
            && (if row.is_async { compiler_start < start } else { compiler_start == start })
            && row.name.0 >= start && row.name.1 <= row.body_start
            && text.get(row.name.0 as usize..row.name.1 as usize) == Some(name)
        {
            if selected.replace(row).is_some() { return Ok(None); }
        }
    }
    let Some(row) = selected else { return Ok(None) };
    let start = row.export_start.unwrap_or(compiler_extent.0);
    let Some(header) = text.get(start as usize..row.body_start as usize) else { return Ok(None) };
    let header = header.trim();
    poll(checkpoint, 0)?;
    Ok((!header.is_empty()).then_some(header))
}

/// Returns the byte range of a selected authored header in the same verified
/// source buffer. The returned range is still a claim until image admission.
pub fn selected_function_header_range(
    rows: &[NativeFunctionHeader], text: &str, compiler_extent: (u32, u32), name: &str,
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<Option<(u32, u32)>, String> {
    let Some(header) = selected_function_header(rows, text, compiler_extent, name, checkpoint)? else {
        return Ok(None);
    };
    if header.len() > backend_compile::SourceDeclaration::MAX_TEXT_BYTES { return Ok(None); }
    let start = (header.as_ptr() as usize).checked_sub(text.as_ptr() as usize)
        .and_then(|offset| u32::try_from(offset).ok())
        .ok_or("native header source offset overflow")?;
    let end = start.checked_add(u32::try_from(header.len()).map_err(|_| "native header length overflow")?)
        .ok_or("native header source extent overflow")?;
    if text.get(start as usize..end as usize) != Some(header) { return Err("native header source range differs".into()); }
    poll(checkpoint, 0)?;
    Ok(Some((start, end)))
}

trait NativeCheckpoint {
    fn poll(&self, work: u64) -> Result<(), String>;
}

impl<T: ExecutionCheckpoint + ?Sized> NativeCheckpoint for T {
    fn poll(&self, work: u64) -> Result<(), String> {
        self.checkpoint(work)
            .map_err(|stop| format!("native TSZ admission stopped: {stop:?}"))
    }
}

struct RootReplayCheckpoint<'a>(&'a dyn Fn(u64) -> Result<(), String>);

impl NativeCheckpoint for RootReplayCheckpoint<'_> {
    fn poll(&self, work: u64) -> Result<(), String> {
        (self.0)(work)
    }
}

fn poll(checkpoint: &(impl NativeCheckpoint + ?Sized), work: u64) -> Result<(), String> {
    checkpoint.poll(work)
}

fn explicit_resolution_mode(
    arena: &tsz::parser::node::NodeArena,
    attributes: NodeIndex,
) -> Option<ImportingModuleKind> {
    let attributes = arena
        .get(attributes)
        .and_then(|node| arena.get_import_attributes_data(node))?;
    attributes.elements.nodes.iter().find_map(|&index| {
        let entry = arena
            .get(index)
            .and_then(|node| arena.get_import_attribute_data(node))?;
        let name = arena
            .get_literal_text(entry.name)
            .or_else(|| arena.get_identifier_text(entry.name))?;
        if name != "resolution-mode" {
            return None;
        }
        match arena.get_literal_text(entry.value)? {
            "import" => Some(ImportingModuleKind::Esm),
            "require" => Some(ImportingModuleKind::CommonJs),
            _ => None,
        }
    })
}

fn object_property(
    arena: &tsz::parser::node::NodeArena,
    object: NodeIndex,
    name: &str,
) -> Option<NodeIndex> {
    let object = arena
        .get(object)
        .and_then(|node| arena.get_literal_expr(node))?;
    object.elements.nodes.iter().find_map(|&index| {
        let property = arena
            .get(index)
            .and_then(|node| arena.get_property_assignment(node))?;
        (arena
            .get_literal_text(property.name)
            .or_else(|| arena.get_identifier_text(property.name))
            == Some(name))
        .then_some(property.initializer)
    })
}

fn select_roots(
    fs: &dyn ResolutionFileSystem,
    config_path: &Path,
    config: &TsConfig,
    options: &ResolvedCompilerOptions,
    checkpoint: &(impl NativeCheckpoint + ?Sized),
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let directory = config_path
        .parent()
        .ok_or("native TSZ config has no directory")?;
    let mut roots = BTreeSet::new();
    for file in config.files.as_deref().unwrap_or_default() {
        poll(checkpoint, 1)?;
        let path = absolute_selector(directory, file);
        if !fs.is_file(&path).map_err(|error| error.to_string())? {
            return Err(format!(
                "configured TypeScript root is missing: {}",
                path.display()
            ));
        }
        roots.insert(path);
    }
    let includes = config.include.clone().unwrap_or_else(|| {
        if config.files.is_some() {
            Vec::new()
        } else {
            vec!["**/*".into()]
        }
    });
    let mut excludes = config.exclude.clone().unwrap_or_else(|| {
        vec![
            "**/node_modules".into(),
            "**/bower_components".into(),
            "**/jspm_packages".into(),
        ]
    });
    if config.exclude.is_none()
        && let Some(out_dir) = &options.out_dir
    {
        excludes.push(out_dir.to_string_lossy().into_owned());
    }
    let includes = includes
        .iter()
        .filter(|pattern| !pattern.is_empty())
        // TSZ's config contract discards invalid recursive terminal selectors;
        // other selectors and explicit files still determine the program.
        .filter(|pattern| !tsz::config::include_specification_has_trailing_recursion(pattern))
        .map(|pattern| selector_pattern(directory, pattern))
        .collect::<Vec<_>>();
    let excludes = excludes
        .iter()
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| selector_pattern(directory, pattern))
        .collect::<Vec<_>>();
    if includes
        .iter()
        .chain(&excludes)
        .map(String::len)
        .sum::<usize>()
        > MAX_PATTERN_BYTES
    {
        return Err("native TSZ root patterns exceed bound".into());
    }
    let mut scan_roots = BTreeSet::new();
    for pattern in &includes {
        let literal = pattern
            .split('/')
            .take_while(|segment| !segment.contains(['*', '?']))
            .collect::<Vec<_>>()
            .join("/");
        let mut path = PathBuf::from(literal);
        if !fs.is_dir(&path).map_err(|error| error.to_string())? {
            path.pop();
        }
        scan_roots.insert(path);
    }
    let mut pending = scan_roots
        .into_iter()
        .map(|path| (path, 0usize))
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    let mut queried_directories = BTreeSet::new();
    let mut entries_seen = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        poll(checkpoint, 1)?;
        let Some(canonical) = fs
            .canonicalize(&directory)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if !visited.insert(canonical.clone()) {
            continue;
        }
        if depth > MAX_DIRECTORY_DEPTH {
            return Err("native TSZ root directory depth exceeds bound".into());
        }
        queried_directories.insert(canonical);
        let Some(entries) = fs.read_dir(&directory).map_err(|error| error.to_string())? else {
            continue;
        };
        entries_seen = entries_seen.saturating_add(entries.len());
        if entries_seen > MAX_DIRECTORY_ENTRIES {
            return Err("native TSZ root directory entries exceed bound".into());
        }
        for entry in entries {
            poll(checkpoint, 1)?;
            let path = directory.join(entry.name);
            let logical = path.to_string_lossy().replace('\\', "/");
            if any_selector_matches(&excludes, &logical, false, false, checkpoint)? {
                continue;
            }
            let is_directory = match entry.kind {
                ResolutionFileKind::Directory => true,
                ResolutionFileKind::Symlink => {
                    fs.is_dir(&path).map_err(|error| error.to_string())?
                }
                _ => false,
            };
            if is_directory {
                if !any_selector_matches(&includes, &logical, true, true, checkpoint)? {
                    continue;
                }
                pending.push((path, depth + 1));
                continue;
            }
            if source_extension(&path, options.allow_js)
                && any_selector_matches(&includes, &logical, true, false, checkpoint)?
            {
                roots.insert(path);
                if roots.len() > MAX_FILES {
                    return Err("native TSZ roots exceed bound".into());
                }
            }
        }
    }
    Ok((
        roots.into_iter().collect(),
        queried_directories.into_iter().collect(),
    ))
}

fn source_extension(path: &Path, allow_js: bool) -> bool {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ts" | "tsx" | "mts" | "cts") => true,
        Some("js" | "jsx" | "mjs" | "cjs") => allow_js,
        _ => false,
    }
}

fn absolute_selector(directory: &Path, selector: &str) -> PathBuf {
    normalize(&directory.join(selector))
}
fn selector_pattern(directory: &Path, selector: &str) -> String {
    let mut path = absolute_selector(directory, selector)
        .to_string_lossy()
        .replace('\\', "/");
    // TypeScript isImplicitGlob tests only the last normalized component:
    // a dotted directory needs an explicit wildcard, while packages/*/src is implicit.
    if path
        .rsplit('/')
        .next()
        .is_some_and(|last| !last.contains(['.', '*', '?']))
    {
        path.push_str("/**/*");
    }
    path
}
fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            value => output.push(value.as_os_str()),
        }
    }
    output
}

// TypeScript's include matcher excludes hidden/common-package components when
// a wildcard consumes them. Excludes allow them. Both matching levels use bounded
// iterative DP; question marks consume one UTF-16 code unit, as the native contract does.
fn any_selector_matches(
    patterns: &[String],
    path: &str,
    include: bool,
    directory_prefix: bool,
    checkpoint: &(impl NativeCheckpoint + ?Sized),
) -> Result<bool, String> {
    for pattern in patterns {
        if glob_path_matches(pattern, path, include, directory_prefix, checkpoint)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn glob_path_matches(
    pattern: &str,
    path: &str,
    include: bool,
    directory_prefix: bool,
    checkpoint: &(impl NativeCheckpoint + ?Sized),
) -> Result<bool, String> {
    let pattern = pattern.split('/').collect::<Vec<_>>();
    let path = path.split('/').collect::<Vec<_>>();
    let mut current = vec![false; path.len() + 1];
    current[0] = true;
    for component in pattern {
        poll(checkpoint, path.len() as u64)?;
        if directory_prefix && current[path.len()] {
            return Ok(true);
        }
        let mut next = vec![false; path.len() + 1];
        if component == "**" {
            for j in 0..=path.len() {
                if current[j] {
                    next[j] = true;
                }
                if j > 0 && next[j - 1] && (!include || wildcard_directory(path[j - 1])) {
                    next[j] = true;
                }
            }
        } else {
            for j in 0..path.len() {
                if current[j]
                    && segment_matches(
                        component,
                        path[j],
                        include,
                        !directory_prefix && j + 1 == path.len(),
                        checkpoint,
                    )?
                {
                    next[j + 1] = true;
                }
            }
        }
        current = next;
    }
    Ok(if include {
        current[path.len()]
    } else {
        current.into_iter().any(|matched| matched)
    })
}

fn wildcard_directory(component: &str) -> bool {
    !component.starts_with('.')
        && !matches!(
            component,
            "node_modules" | "bower_components" | "jspm_packages"
        )
}

fn segment_matches(
    pattern: &str,
    path: &str,
    include: bool,
    file_name: bool,
    checkpoint: &(impl NativeCheckpoint + ?Sized),
) -> Result<bool, String> {
    let wildcard = pattern.contains(['*', '?']);
    if include && wildcard && matches!(path, "node_modules" | "bower_components" | "jspm_packages")
    {
        return Ok(false);
    }
    if include && pattern.starts_with(['*', '?']) && path.starts_with('.') {
        return Ok(false);
    }
    if !wildcard {
        // Literal absolute-path components recur for every directory entry.
        // Compare them directly instead of building a quadratic wildcard table;
        // still meter both byte strings and propagate the checkpoint stop.
        poll(checkpoint, pattern.len().saturating_add(path.len()) as u64)?;
        return Ok(pattern == path);
    }
    let pattern = pattern.encode_utf16().collect::<Vec<_>>();
    let path = path.encode_utf16().collect::<Vec<_>>();
    let minified = ".min.js".encode_utf16().collect::<Vec<_>>();
    let mut current = vec![false; path.len() + 1];
    current[0] = true;
    for token in pattern {
        poll(checkpoint, path.len() as u64)?;
        let mut next = vec![false; path.len() + 1];
        if token == u16::from(b'*') {
            for j in 0..=path.len() {
                if current[j] {
                    next[j] = true;
                }
                if j > 0 && next[j - 1] && !(include && file_name && path[j - 1..] == minified) {
                    next[j] = true;
                }
            }
        } else {
            for j in 0..path.len() {
                if current[j] && (token == u16::from(b'?') || token == path[j]) {
                    next[j + 1] = true;
                }
            }
        }
        current = next;
    }
    Ok(current[path.len()])
}

fn jsx_runtime_specifier(options: &TszCheckerOptions, source: &str) -> Option<String> {
    use tsz_common::checker_options::JsxMode;
    let pragma_source = tsz::checker::jsx::program_import_source_pragma(source);
    let mode = match tsz::checker::jsx::program_runtime_pragma(source) {
        Some("classic") => JsxMode::React,
        Some("automatic") if options.jsx_mode == JsxMode::ReactJsxDev => JsxMode::ReactJsxDev,
        Some("automatic") => JsxMode::ReactJsx,
        _ => options.jsx_mode,
    };
    let suffix = match mode {
        JsxMode::ReactJsxDev => "jsx-dev-runtime",
        JsxMode::ReactJsx => "jsx-runtime",
        _ if pragma_source.is_some() || !options.jsx_import_source.is_empty() => "jsx-runtime",
        _ => return None,
    };
    let source = pragma_source.unwrap_or_else(|| {
        if options.jsx_import_source.is_empty() {
            "react".into()
        } else {
            options.jsx_import_source.clone()
        }
    });
    Some(format!("{source}/{suffix}"))
}

fn add_type_roots(
    fs: &dyn ResolutionFileSystem,
    directory: Option<&Path>,
    options: &ResolvedCompilerOptions,
    resolver: &mut ModuleResolver<'_>,
    pending: &mut VecDeque<PathBuf>,
    program_diagnostics: &mut Vec<crate::TszDiagnostic>,
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<(), String> {
    let Some(directory) = directory else {
        return Ok(());
    };
    let roots = if let Some(roots) = &options.type_roots {
        roots.clone()
    } else {
        let mut roots = Vec::new();
        for ancestor in directory.ancestors().take(32) {
            roots.push(ancestor.join("node_modules/@types"));
            if !fs.may_walk_parent(ancestor) {
                break;
            }
        }
        roots
    };
    let requested = match &options.types {
        Some(types) => types.clone(),
        None => {
            poll(checkpoint, 1)?;
            resolver
                .automatic_type_directive_names_checked(&roots)
                .map_err(|error| error.to_string())?
        }
    };
    let importer = directory.join("__native_tsz_type_reference__.ts");
    if requested.len() > MAX_REQUESTS {
        return Err("native TSZ type directive count exceeds bound".into());
    }
    let mut seen = BTreeSet::new();
    for name in requested {
        poll(checkpoint, 1)?;
        if !seen.insert(name.clone()) {
            continue;
        }
        match resolver
            .resolve_program_type_reference_directive_checked(
                &name,
                &importer,
                options.type_roots.as_deref(),
                None,
            )
            .map_err(|error| error.to_string())?
        {
            Ok(resolved) => pending.push_back(resolved.resolved_path),
            Err(ResolutionFailure::NotFound { .. }) => {
                use tsz::checker::diagnostics::{
                    Diagnostic, diagnostic_codes, diagnostic_messages, format_message,
                };
                let mut diagnostic = Diagnostic::from_code(
                    diagnostic_codes::CANNOT_FIND_TYPE_DEFINITION_FILE_FOR,
                    "",
                    0,
                    0,
                    &[&name],
                );
                let (code, message) = if options.types.is_some() {
                    (diagnostic_codes::ENTRY_POINT_OF_TYPE_LIBRARY_SPECIFIED_IN_COMPILEROPTIONS, diagnostic_messages::ENTRY_POINT_OF_TYPE_LIBRARY_SPECIFIED_IN_COMPILEROPTIONS)
                } else {
                    (
                        diagnostic_codes::ENTRY_POINT_FOR_IMPLICIT_TYPE_LIBRARY,
                        diagnostic_messages::ENTRY_POINT_FOR_IMPLICIT_TYPE_LIBRARY,
                    )
                };
                diagnostic
                    .related_information
                    .push(Diagnostic::related_message(
                        code,
                        "",
                        0,
                        0,
                        format_message(message, &[&name]),
                    ));
                program_diagnostics.push(diagnostic);
            }
            Err(error) => {
                return Err(format!(
                    "native TSZ configured type {name:?} refused: {error:?}"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{error::Error, io};

    fn glob_match(
        pattern: &str,
        path: &str,
        is_directory: bool,
        allow_directory_match: bool,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<bool, io::Error> {
        glob_path_matches(
            pattern,
            path,
            is_directory,
            allow_directory_match,
            checkpoint,
        )
        .map_err(|stop| io::Error::other(format!("glob matching stopped: {stop:?}")))
    }

    struct Unlimited;
    impl ExecutionCheckpoint for Unlimited {
        fn checkpoint(&self, _: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
            Ok(())
        }
    }
    #[test]
    fn five_thousand_arena_headers_join_with_bounded_index_and_ambiguity() -> Result<(), Box<dyn Error>> {
        use std::sync::atomic::{AtomicU64, Ordering};
        struct Budget(AtomicU64);
        impl ExecutionCheckpoint for Budget {
            fn checkpoint(&self, work: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                let used = self.0.fetch_add(work, Ordering::Relaxed).saturating_add(work);
                if used > 2_000_000 { Err(tsz_common::ProjectExecutionStop::WorkBudgetExhausted) } else { Ok(()) }
            }
        }
        struct Stopped;
        impl ExecutionCheckpoint for Stopped {
            fn checkpoint(&self, _: u64) -> Result<(), tsz_common::ProjectExecutionStop> { Err(tsz_common::ProjectExecutionStop::WorkBudgetExhausted) }
        }
        let source = (0..5000).map(|i| format!("function f{i}() {{ return {i}; }}\n")).collect::<String>();
        let mut parser = MeteredParserState::new("src/many.ts".to_owned(), source.clone());
        parser.parse_source_file_with_execution_checkpoint(&Unlimited).map_err(|e| format!("parse: {e:?}"))?;
        assert!(parser.get_diagnostics().is_empty());
        let budget = Budget(AtomicU64::new(0));
        let mut rows = function_headers_from_arena(&parser.arena, &source, &budget)?;
        assert_eq!(rows.len(), 5000);
        index_function_headers_by_end(&mut rows, &budget)?;
        for row in &rows {
            let name = &source[row.name.0 as usize..row.name.1 as usize];
            let candidates = function_headers_with_end(&rows, row.declaration.1, &budget)?;
            assert!(selected_function_header_range(candidates, &source, row.declaration, name, &budget)?.is_some());
        }
        let last = rows.last().ok_or("last function")?;
        let name = &source[last.name.0 as usize..last.name.1 as usize];
        assert_eq!(name, "f4999");
        let duplicate = [*last, *last];
        assert_eq!(selected_function_header_range(&duplicate, &source, last.declaration, name, &budget)?, None);
        assert!(function_headers_with_end(&rows, last.declaration.1, &Stopped).is_err());
        assert!(index_function_headers_by_end(&mut rows, &Stopped).is_err());
        assert!(budget.0.load(Ordering::Relaxed) < 2_000_000);
        Ok(())
    }

    #[test]
    fn header_capture_count_boundary_is_explicit() -> Result<(), Box<dyn Error>> {
        fn capture(count: usize) -> Result<Vec<NativeFunctionHeader>, String> {
            let text = (0..count).map(|i| format!("function f{i}() {{}}\n")).collect::<String>();
            let mut parser = MeteredParserState::new("src/boundary.ts".to_owned(), text.clone());
            parser.parse_source_file_with_execution_checkpoint(&Unlimited).map_err(|e| format!("parse: {e:?}"))?;
            assert!(parser.get_diagnostics().is_empty());
            function_headers_from_arena(&parser.arena, &text, &Unlimited)
        }
        assert_eq!(capture(16_384)?.len(), 16_384);
        assert_eq!(capture(16_385).unwrap_err(), "native TSZ header limit exceeded");
        Ok(())
    }

    #[test]
    fn native_function_keeps_parser_owned_async_start() -> Result<(), Box<dyn Error>> {
        for (source, expected_async) in [
            ("export async function run() {}", true),
            ("export async /* comment */ function run() {}", true),
            ("export default async function run() {}", true),
            ("async\nfunction run() {}", false),
            ("function run() {}", false),
        ] {
            let mut parser = MeteredParserState::new("src/modifiers.ts".to_owned(), source.to_owned());
            parser.parse_source_file_with_execution_checkpoint(&Unlimited).map_err(|e| format!("parse: {e:?}"))?;
            assert!(parser.get_diagnostics().is_empty(), "{source}");
            let functions = parser.arena.nodes.iter().filter_map(|node| {
                let function = parser.arena.get_function(node)?;
                let name = parser.arena.get_identifier_at(function.name)?;
                (name.escaped_text == "run").then_some(function)
            }).collect::<Vec<_>>();
            let [function] = functions.as_slice() else { panic!("one declaration for {source}") };
            assert_eq!(function.is_async, expected_async, "{source}");
            assert_eq!(function.async_start, expected_async.then(|| source.find("async").unwrap() as u32), "{source}");
        }
        Ok(())
    }

    #[test]
    fn function_headers_use_native_body_and_modifier_spans() -> Result<(), Box<dyn Error>> {
        let source = "// λ\nexport async function run(x: { a: number }): Promise<string> { return ''; }\nfunction other() {}\n";
        let headers = function_headers(Path::new("src/main.ts"), source, &Unlimited)?;
        assert_eq!(headers.len(), 2);
        let compiler_extent = crate::legacy::with_analysis(
            backend_semantic::vocabulary::TypeScriptSource::TypeScript, source, |module| {
                module.semantic.nodes().iter_enumerated().find_map(|(_, node)| {
                    let function = node.kind().as_function()?;
                    (function.id.as_ref()?.name.as_str() == "run").then_some((function.span.start, function.span.end))
                })
            },
        )?.ok_or("compiler function missing")?;
        assert_eq!(selected_function_header(&headers, source, compiler_extent, "run", &Unlimited)?,
            Some("export async function run(x: { a: number }): Promise<string>"));
        assert_eq!(selected_function_header(&headers, source, (compiler_extent.0, compiler_extent.1 - 1), "run", &Unlimited)?, None);
        assert_eq!(selected_function_header(&headers, source, compiler_extent, "other", &Unlimited)?, None);
        let duplicate = [headers[0], headers[0]];
        assert_eq!(selected_function_header(&duplicate, source, compiler_extent, "run", &Unlimited)?, None);
        struct Stopped;
        impl ExecutionCheckpoint for Stopped {
            fn checkpoint(&self, _: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                Err(tsz_common::ProjectExecutionStop::WorkBudgetExhausted)
            }
        }
        assert!(selected_function_header(&headers, source, compiler_extent, "run", &Stopped).is_err());
        struct StopAfterOne(std::sync::atomic::AtomicUsize);
        impl ExecutionCheckpoint for StopAfterOne {
            fn checkpoint(&self, work: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                if work != 0 && self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1 {
                    Err(tsz_common::ProjectExecutionStop::WorkBudgetExhausted)
                } else { Ok(()) }
            }
        }
        assert!(selected_function_header(&duplicate, source, compiler_extent, "run", &StopAfterOne(std::sync::atomic::AtomicUsize::new(0))).is_err());
        assert!(selected_function_header(&[], source, compiler_extent, "run", &Stopped).is_err());
        for (path, source, expected) in [
            ("src/main.ts", "const async = 1; async\nfunction plain() {}", None),
            ("src/main.ts", "const async = 1; async; function plain() {}", Some("function plain()")),
            ("src/main.ts", "export default async function* plain(): Promise<number> { yield 1; }", None),
            ("src/main.ts", "export async /* comment */ function plain(): Promise<number> { return 1; }", Some("export async /* comment */ function plain(): Promise<number>")),
            ("src/main.ts", "const pattern = /`/; export async function plain() {}", Some("export async function plain()")),
            ("src/main.ts", "const text = `before ${1}`; export async function plain() {}", Some("export async function plain()")),
            ("src/main.tsx", "const view = <div>{1}</div>; export async function plain() {}", Some("export async function plain()")),
        ] {
            let rows = function_headers(Path::new(path), source, &Unlimited)?;
            let compiler_extent = crate::legacy::with_analysis(
                if path.ends_with(".tsx") { backend_semantic::vocabulary::TypeScriptSource::Tsx } else { backend_semantic::vocabulary::TypeScriptSource::TypeScript }, source, |module| {
                    module.semantic.nodes().iter_enumerated().find_map(|(_, node)| {
                        let function = node.kind().as_function()?;
                        (function.id.as_ref()?.name.as_str() == "plain").then_some((function.span.start, function.span.end))
                    })
                },
            )?.ok_or("compiler function missing")?;
            assert_eq!(selected_function_header(&rows, source, compiler_extent, "plain", &Unlimited)?, expected, "{source}");
        }
        Ok(())
    }

    #[test]
    fn include_selectors_follow_last_component_implicit_glob_contract() -> Result<(), Box<dyn Error>>
    {
        let root = Path::new("/repo");
        assert_eq!(selector_pattern(root, "src.v1"), "/repo/src.v1");
        assert_eq!(selector_pattern(root, ".src"), "/repo/.src");
        assert_eq!(
            selector_pattern(root, "packages/*/src"),
            "/repo/packages/*/src/**/*"
        );
        assert!(glob_match(
            "/repo/src.v1/**/*",
            "/repo/src.v1/main.ts",
            true,
            false,
            &Unlimited
        )?);
        assert!(!glob_match(
            "/repo/src.v1",
            "/repo/src.v1/main.ts",
            true,
            false,
            &Unlimited
        )?);
        Ok(())
    }

    #[test]
    fn configured_root_transcript_replays_unrelated_files_but_rejects_new_roots()
    -> Result<(), Box<dyn Error>> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("tsz-root-query-replay-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("src"))?;
        let config = root.join("tsconfig.json");
        std::fs::write(
            &config,
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src/**/*"]}"#,
        )?;
        std::fs::write(root.join("src/main.ts"), "export const main = 1;")?;
        let before = replay_root_selection(&tsz::StdResolutionFileSystem, &config, &Unlimited)?;
        std::fs::write(root.join("src/unrelated.txt"), "")?;
        let after_text = replay_root_selection(&tsz::StdResolutionFileSystem, &config, &Unlimited)?;
        assert_eq!(before, after_text, "the .txt is not a TSZ root input");

        std::fs::write(root.join("src/new.ts"), "export const added = 1;")?;
        let after_source =
            replay_root_selection(&tsz::StdResolutionFileSystem, &config, &Unlimited)?;
        assert_ne!(before.root_names, after_source.root_names);

        std::fs::write(
            &config,
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["src/main.ts"]}"#,
        )?;
        let after_options =
            replay_root_selection(&tsz::StdResolutionFileSystem, &config, &Unlimited)?;
        assert_ne!(after_source, after_options);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn native_include_globs_keep_hidden_dependency_and_minified_file_boundaries()
    -> Result<(), Box<dyn Error>> {
        let matches = |pattern, path| glob_match(pattern, path, true, false, &Unlimited);
        assert!(!matches("/repo/**/*", "/repo/.hidden/main.ts")?);
        assert!(!matches("/repo/**/*", "/repo/node_modules/pkg/index.d.ts")?);
        assert!(matches("/repo/.hidden/**/*", "/repo/.hidden/main.ts")?);
        assert!(matches(
            "/repo/node_modules/pkg/**/*",
            "/repo/node_modules/pkg/index.d.ts"
        )?);
        assert!(!matches("/repo/**/*", "/repo/src/vendor.min.js")?);
        assert!(matches("/repo/**/*.min.js", "/repo/src/vendor.min.js")?);
        assert!(matches(
            "/repo/*/src/**/*",
            "/repo/vendor.min.js/src/main.ts"
        )?);
        assert!(matches("/repo/src/?.ts", "/repo/src/é.ts")?);
        assert!(!matches("/repo/src/?.ts", "/repo/src/😀.ts")?);
        assert!(matches("/repo/src/??.ts", "/repo/src/😀.ts")?);
        assert!(!glob_match(
            "/repo/src/**/*.ts",
            "/repo/vendor",
            true,
            true,
            &Unlimited
        )?);
        Ok(())
    }
    #[test]
    fn local_root_replay_control_stops_inside_a_single_pattern() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let control = |_: u64| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err("caller cancelled replay".to_string())
            } else {
                Ok(())
            }
        };
        let result = glob_path_matches(
            "/repo/**/*",
            "/repo/src/main.ts",
            true,
            false,
            &RootReplayCheckpoint(&control),
        );
        assert_eq!(result, Err("caller cancelled replay".to_string()));
        assert_eq!(calls.get(), 2, "stop propagates from inside the matcher");
    }

    #[test]
    fn native_root_matcher_propagates_work_stops_inside_a_single_pattern() {
        struct Stopped;
        impl ExecutionCheckpoint for Stopped {
            fn checkpoint(&self, _: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                Err(tsz_common::ProjectExecutionStop::WorkBudgetExhausted)
            }
        }
        assert!(
            glob_path_matches("/repo/**/*", "/repo/src/main.ts", true, false, &Stopped).is_err()
        );
    }
    #[test]
    fn native_literal_root_components_remain_linear_and_metered() -> Result<(), Box<dyn Error>> {
        struct Budget(std::sync::atomic::AtomicU64);
        impl ExecutionCheckpoint for Budget {
            fn checkpoint(&self, work: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                let prior = self.0.fetch_add(work, std::sync::atomic::Ordering::Relaxed);
                if prior.saturating_add(work) > 2_000_000 {
                    return Err(tsz_common::ProjectExecutionStop::WorkBudgetExhausted);
                }
                Ok(())
            }
        }
        let budget = Budget(std::sync::atomic::AtomicU64::new(0));
        let component = format!("captured-{}-😀", "r".repeat(220));
        let pattern = format!("/root/{component}/src/**/*.ts");
        let path = format!("/root/{component}/src/app/main.ts");
        for _ in 0..100 {
            assert!(glob_match(&pattern, &path, true, false, &budget)?);
        }
        assert!(!glob_match(
            &pattern,
            &path.replace("captured-", "changed-"),
            true,
            false,
            &budget
        )?);
        assert!(segment_matches(
            &component, &component, false, false, &budget
        )?);
        assert!(!segment_matches(
            &component,
            "different",
            false,
            false,
            &budget
        )?);
        let stopped = Budget(std::sync::atomic::AtomicU64::new(2_000_000));
        assert!(segment_matches(&component, &component, false, false, &stopped).is_err());
        Ok(())
    }
    #[test]
    fn reference_directives_are_leading_trivia_with_exact_attribute_names_and_modes()
    -> Result<(), Box<dyn Error>> {
        let references = tsz::checker::triple_slash_validator::reference_directives(
            "\u{feff}/* /// <reference types=\"wrong\" /> */\n/// <reference types = 'node' resolution-mode = 'require' />\n/// <reference path = './ambient.d.ts' />\nexport const text = '/// <reference types=\"wrong\" />';\n/// <reference types=\"late\" />",
            &Unlimited,
        )
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io::Error::other(format!("reference directive parsing failed: {error:?}")))?;
        assert_eq!(references.len(), 2);
        assert_eq!(
            references[0]
                .types
                .ok_or_else(|| io::Error::other("first reference is missing its types attribute"))?
                .text,
            "node"
        );
        assert_eq!(
            references[0]
                .resolution_mode
                .ok_or_else(|| io::Error::other("first reference is missing its resolution mode"))?
                .text,
            "require"
        );
        assert_eq!(
            references[1]
                .path
                .ok_or_else(|| io::Error::other("second reference is missing its path attribute"))?
                .text,
            "./ambient.d.ts"
        );
        Ok(())
    }

    #[test]
    fn configured_paths_alias_resolves_without_base_url() -> Result<(), Box<dyn Error>> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("tsz-path-alias-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("src/app"))?;
        std::fs::create_dir_all(root.join("src/components"))?;
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{"files":["src/app/page.ts"],"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
        )?;
        std::fs::write(
            root.join("src/app/page.ts"),
            "export { NavBar } from '@/components/NavBar';\n",
        )?;
        std::fs::write(
            root.join("src/components/NavBar.ts"),
            "export const NavBar = 1;\n",
        )?;

        let config = root.join("tsconfig.json");
        let program = build(
            &tsz::StdResolutionFileSystem,
            Some(&config),
            &[],
            &Unlimited,
        )
        .map_err(io::Error::other)?;
        let target = root.join("src/components/NavBar.ts").canonicalize()?;
        assert!(
            program.resolutions.iter().any(|resolution| {
                resolution.importer.ends_with("src/app/page.ts")
                    && resolution.specifier == "@/components/NavBar"
                    && resolution.target.as_ref() == Some(&target)
            }),
            "tsconfig paths without baseUrl must resolve through the configured project root: {:?}",
            program
                .resolutions
                .iter()
                .map(|resolution| (
                    &resolution.importer,
                    &resolution.specifier,
                    &resolution.target
                ))
                .collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn missing_selected_config_does_not_invent_a_paths_anchor() -> Result<(), Box<dyn Error>> {
        let program = build(&tsz::StdResolutionFileSystem, None, &[], &Unlimited)
            .map_err(io::Error::other)?;
        assert!(
            program.options_identity.contains("paths_base_path: None"),
            "an absent config must leave the resolver's paths anchor unset: {}",
            program.options_identity
        );
        Ok(())
    }

    #[test]
    fn inherited_paths_keep_declaring_config_anchor_and_local_override()
    -> Result<(), Box<dyn Error>> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "tsz-inherited-path-alias-{}-{id}",
            std::process::id()
        ));
        let base = root.join("base");
        let inherited_leaf = root.join("inherited");
        let overridden_leaf = root.join("overridden");
        for dir in [
            base.join("src/components"),
            inherited_leaf.join("src/app"),
            inherited_leaf.join("src/components"),
            overridden_leaf.join("src/app"),
            overridden_leaf.join("override-src/components"),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            base.join("tsconfig.json"),
            r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
        )?;
        std::fs::write(
            inherited_leaf.join("tsconfig.json"),
            r#"{"extends":"../base/tsconfig.json","files":["src/app/page.ts"]}"#,
        )?;
        std::fs::write(
            inherited_leaf.join("src/app/page.ts"),
            "import { Inherited } from '@/components/Inherited';\nexport { Inherited };\n",
        )?;
        std::fs::write(
            base.join("src/components/Inherited.ts"),
            "export const Inherited = 1;\n",
        )?;

        // The inherited mapping resolves beneath `base`, even though the
        // selected config and importing source live under `inherited`.
        let inherited_config = inherited_leaf.join("tsconfig.json");
        let inherited = build(
            &tsz::StdResolutionFileSystem,
            Some(&inherited_config),
            &[],
            &Unlimited,
        )
        .map_err(io::Error::other)?;
        let inherited_target = base.join("src/components/Inherited.ts").canonicalize()?;
        assert!(
            inherited.resolutions.iter().any(|resolution| {
                resolution.specifier == "@/components/Inherited"
                    && resolution.target.as_ref() == Some(&inherited_target)
            }),
            "inherited paths must retain their declaring config directory: {:?}",
            inherited
                .resolutions
                .iter()
                .map(|resolution| (&resolution.specifier, &resolution.target))
                .collect::<Vec<_>>()
        );

        // A later extends entry without `paths` leaves the earlier mapping
        // and its origin in force.
        let empty_base = root.join("empty-base");
        let array_leaf = root.join("array-inherited");
        std::fs::create_dir_all(empty_base.as_path())?;
        std::fs::create_dir_all(array_leaf.join("src"))?;
        std::fs::write(empty_base.join("tsconfig.json"), "{}")?;
        std::fs::write(
            array_leaf.join("tsconfig.json"),
            r#"{"extends":["../base/tsconfig.json","../empty-base/tsconfig.json"],"files":["src/page.ts"]}"#,
        )?;
        std::fs::write(
            array_leaf.join("src/page.ts"),
            "import { Inherited } from '@/components/Inherited';\nexport { Inherited };\n",
        )?;
        let array_config = array_leaf.join("tsconfig.json");
        let array_program = build(
            &tsz::StdResolutionFileSystem,
            Some(&array_config),
            &[],
            &Unlimited,
        )
        .map_err(io::Error::other)?;
        assert!(
            array_program.resolutions.iter().any(|resolution| {
                resolution.specifier == "@/components/Inherited"
                    && resolution.target.as_ref() == Some(&inherited_target)
            }),
            "an extends entry without paths must retain the earlier mapping origin"
        );

        // A later extends entry that inherits `paths` from its own base wins
        // as a whole option and carries that ancestor's declaring directory.
        let alternate_mapping = root.join("alternate-mapping");
        let alternate = root.join("alternate");
        let array_override_leaf = root.join("array-override");
        std::fs::create_dir_all(alternate_mapping.join("alt-src/components"))?;
        std::fs::create_dir_all(alternate.as_path())?;
        std::fs::create_dir_all(array_override_leaf.join("src"))?;
        std::fs::write(
            alternate_mapping.join("tsconfig.json"),
            r#"{"compilerOptions":{"paths":{"@/*":["./alt-src/*"]}}}"#,
        )?;
        std::fs::write(
            alternate.join("tsconfig.json"),
            r#"{"extends":"../alternate-mapping/tsconfig.json"}"#,
        )?;
        std::fs::write(
            array_override_leaf.join("tsconfig.json"),
            r#"{"extends":["../base/tsconfig.json","../alternate/tsconfig.json"],"files":["src/page.ts"]}"#,
        )?;
        std::fs::write(
            array_override_leaf.join("src/page.ts"),
            "import { Alternate } from '@/components/Alternate';\nexport { Alternate };\n",
        )?;
        std::fs::write(
            alternate_mapping.join("alt-src/components/Alternate.ts"),
            "export const Alternate = 1;\n",
        )?;
        let array_override_config = array_override_leaf.join("tsconfig.json");
        let array_override_program = build(
            &tsz::StdResolutionFileSystem,
            Some(&array_override_config),
            &[],
            &Unlimited,
        )
        .map_err(io::Error::other)?;
        let alternate_target = alternate_mapping
            .join("alt-src/components/Alternate.ts")
            .canonicalize()?;
        assert!(
            array_override_program.resolutions.iter().any(|resolution| {
                resolution.specifier == "@/components/Alternate"
                    && resolution.target.as_ref() == Some(&alternate_target)
            }),
            "the later extends entry must carry its inherited paths origin"
        );

        std::fs::write(
            overridden_leaf.join("tsconfig.json"),
            r#"{"extends":"../base/tsconfig.json","files":["src/app/page.ts"],"compilerOptions":{"paths":{"@/*":["./override-src/*"]}}}"#,
        )?;
        std::fs::write(
            overridden_leaf.join("src/app/page.ts"),
            "import { Overridden } from '@/components/Overridden';\nexport { Overridden };\n",
        )?;
        std::fs::write(
            overridden_leaf.join("override-src/components/Overridden.ts"),
            "export const Overridden = 1;\n",
        )?;
        let overridden_config = overridden_leaf.join("tsconfig.json");
        let overridden = build(
            &tsz::StdResolutionFileSystem,
            Some(&overridden_config),
            &[],
            &Unlimited,
        )
        .map_err(io::Error::other)?;
        let overridden_target = overridden_leaf
            .join("override-src/components/Overridden.ts")
            .canonicalize()?;
        assert!(
            overridden.resolutions.iter().any(|resolution| {
                resolution.specifier == "@/components/Overridden"
                    && resolution.target.as_ref() == Some(&overridden_target)
            }),
            "an entry-config paths override must anchor at that config: {:?}",
            overridden
                .resolutions
                .iter()
                .map(|resolution| (&resolution.specifier, &resolution.target))
                .collect::<Vec<_>>()
        );

        // With baseUrl, TSZ's existing baseUrl anchor remains authoritative.
        let base_url_config = root.join("base-url");
        let base_url_leaf = root.join("base-url-leaf");
        std::fs::create_dir_all(base_url_config.join("lib/components"))?;
        std::fs::create_dir_all(base_url_leaf.join("src"))?;
        std::fs::write(
            base_url_config.join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":"./lib","paths":{"@/*":["./components/*"]}}}"#,
        )?;
        std::fs::write(
            base_url_leaf.join("tsconfig.json"),
            r#"{"extends":"../base-url/tsconfig.json","files":["src/page.ts"]}"#,
        )?;
        std::fs::write(
            base_url_leaf.join("src/page.ts"),
            "import { BaseUrl } from '@/BaseUrl';\nexport { BaseUrl };\n",
        )?;
        std::fs::write(
            base_url_config.join("lib/components/BaseUrl.ts"),
            "export const BaseUrl = 1;\n",
        )?;
        let base_url_leaf_config = base_url_leaf.join("tsconfig.json");
        let base_url_program = build(
            &tsz::StdResolutionFileSystem,
            Some(&base_url_leaf_config),
            &[],
            &Unlimited,
        )
        .map_err(io::Error::other)?;
        let base_url_target = base_url_config
            .join("lib/components/BaseUrl.ts")
            .canonicalize()?;
        assert!(
            base_url_program.resolutions.iter().any(|resolution| {
                resolution.specifier == "@/BaseUrl"
                    && resolution.target.as_ref() == Some(&base_url_target)
            }),
            "an inherited baseUrl must continue to anchor paths substitutions"
        );

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn automatic_jsx_runtime_honors_source_pragma_and_classic_override() {
        let mut options = TszCheckerOptions::default();
        options.jsx_mode = tsz_common::checker_options::JsxMode::ReactJsx;
        assert_eq!(
            jsx_runtime_specifier(&options, "export const element = <div />;"),
            Some("react/jsx-runtime".into())
        );
        assert_eq!(
            jsx_runtime_specifier(
                &options,
                "/** @jsxImportSourcex wrong */\n/** @jsxImportSource preact */\nexport const element = <div />;"
            ),
            Some("preact/jsx-runtime".into())
        );
        assert_eq!(
            jsx_runtime_specifier(
                &options,
                "/** @jsxRuntime classic */\nexport const element = <div />;"
            ),
            None
        );
    }
}
