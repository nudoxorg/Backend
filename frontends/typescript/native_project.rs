//! Direct TSZ configuration, root selection and module closure.
//!
//! All filesystem operations use the caller's captured capability. Standard library
//! declarations are immutable linked data; no JavaScript compiler or runtime is used.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use tsz::config::{ResolvedCompilerOptions, TsConfig};
use tsz::module_resolver::{ImportKind, ImportingModuleKind, ModuleResolver};
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

pub struct NativeProgram {
    pub sources: BTreeMap<PathBuf, String>,
    pub libraries: Vec<(String, &'static str)>,
    pub checker: TszCheckerOptions,
    pub resolutions: Vec<NativeModuleResolution>,
    pub file_formats: Vec<(PathBuf, TszProjectFileFormat)>,
    /// Stable native options, including resolver policy, retained in the recipe.
    pub options_identity: String,
    pub root_names: Vec<PathBuf>,
}

pub struct NativeModuleResolution {
    pub importer: PathBuf,
    pub specifier: String,
    pub kind: TszProjectModuleRequestKind,
    pub mode: Option<TszResolutionModeOverride>,
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
    poll(checkpoint, 0)?;
    let config = match config_path {
        Some(path) => tsz::config::load_tsconfig_with_file_system(path, fs)
            .map_err(|error| error.to_string())?,
        None => TsConfig::default(),
    };
    let mut options =
        tsz::config::resolve_compiler_options_embedded(config.compiler_options.as_ref())
            .map_err(|error| error.to_string())?;
    // Resolver options also determine checker diagnostics and must travel with it.
    options.checker.allow_importing_ts_extensions = options.allow_importing_ts_extensions;
    options.checker.rewrite_relative_import_extensions = options.rewrite_relative_import_extensions;
    let options_identity = format!("{options:?}");
    let roots = match config_path {
        Some(path) => select_roots(fs, path, &config, &options, checkpoint)?,
        None => inferred_roots.to_vec(),
    };
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
        checkpoint,
    )?;
    let mut sources = BTreeMap::new();
    let mut file_formats = Vec::new();
    let mut resolutions = Vec::new();
    let mut request_keys = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut source_lib_references = BTreeSet::new();
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
        let mut parser = MeteredParserState::new(path.to_string_lossy().into_owned(), text.clone());
        parser
            .parse_source_file_with_execution_checkpoint(checkpoint)
            .map_err(|error| format!("native TSZ source parse stopped: {error:?}"))?;
        let arena = &parser.arena;
        let format = match resolver.get_importing_module_kind(&path) {
            ImportingModuleKind::Esm => TszProjectFileFormat::EsModule,
            ImportingModuleKind::CommonJs => TszProjectFileFormat::CommonJs,
        };
        resolver
            .check_file_system_error()
            .map_err(|error| error.to_string())?;
        file_formats.push((path.clone(), format));
        {
            let mut type_imports = BTreeSet::new();
            for reference in &arena.type_refs {
                let mut name = reference.type_name;
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
            for reference in tsz::config::extract_lib_references(&text) {
                source_lib_references.insert(reference);
            }
            for reference in triple_slash_paths(&text)
                .into_iter()
                .filter(|_| !options.no_resolve)
            {
                pending.push_back(normalize(
                    &path.parent().unwrap_or(Path::new("")).join(reference),
                ));
            }
        }
        sources.insert(path, text);
    }
    // Linked declarations are data dependencies. Their bytes enter the program's
    // identity even when a project has no TypeScript package installed.
    let mut libs = options.lib_files.clone();
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
    })
}

fn poll(checkpoint: &dyn ExecutionCheckpoint, work: u64) -> Result<(), String> {
    checkpoint
        .checkpoint(work)
        .map_err(|stop| format!("native TSZ admission stopped: {stop:?}"))
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
    checkpoint: &dyn ExecutionCheckpoint,
) -> Result<Vec<PathBuf>, String> {
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
        .map(|pattern| selector_pattern(directory, pattern))
        .collect::<Vec<_>>();
    let excludes = excludes
        .iter()
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
    let mut entries_seen = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        poll(checkpoint, 1)?;
        let Some(canonical) = fs
            .canonicalize(&directory)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if !visited.insert(canonical) {
            continue;
        }
        if depth > MAX_DIRECTORY_DEPTH {
            return Err("native TSZ root directory depth exceeds bound".into());
        }
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
            if excludes
                .iter()
                .any(|pattern| selector_matches(pattern, &logical))
            {
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
                pending.push((path, depth + 1));
                continue;
            }
            if source_extension(&path, options.allow_js)
                && includes
                    .iter()
                    .any(|pattern| selector_matches(pattern, &logical))
            {
                roots.insert(path);
                if roots.len() > MAX_FILES {
                    return Err("native TSZ roots exceed bound".into());
                }
            }
        }
    }
    Ok(roots.into_iter().collect())
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
    if !selector.contains(['*', '?']) && Path::new(selector).extension().is_none() {
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

// TypeScript root selectors support *, ? and **. This iterative DP has bounded
// work and does not interpret shell character classes or follow ambient globs.
fn selector_matches(pattern: &str, path: &str) -> bool {
    glob_matches(pattern, path)
        || pattern
            .strip_suffix("/**/*")
            .is_some_and(|directory| glob_matches(directory, path))
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.as_bytes();
    let path = path.as_bytes();
    let mut current = vec![false; path.len() + 1];
    current[0] = true;
    let mut i = 0;
    while i < pattern.len() {
        let mut next = vec![false; path.len() + 1];
        if pattern[i] == b'*' {
            let recursive = pattern.get(i + 1) == Some(&b'*');
            let slash = recursive && pattern.get(i + 2) == Some(&b'/');
            if slash {
                let mut active = false;
                for j in 0..=path.len() {
                    active |= current[j];
                    next[j] = current[j] || j > 0 && path[j - 1] == b'/' && active;
                }
            } else {
                for j in 0..=path.len() {
                    if current[j] {
                        next[j] = true;
                    }
                    if j > 0 && next[j - 1] && (recursive || path[j - 1] != b'/') {
                        next[j] = true;
                    }
                }
            }
            if slash {
                i += 3;
            } else {
                i += if recursive { 2 } else { 1 };
            }
        } else {
            for j in 0..path.len() {
                if current[j] && (pattern[i] == path[j] || pattern[i] == b'?' && path[j] != b'/') {
                    next[j + 1] = true;
                }
            }
            i += 1;
        }
        current = next;
    }
    current[path.len()]
}

fn triple_slash_paths(source: &str) -> Vec<String> {
    source
        .lines()
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with("//"))
        .filter_map(|line| {
            let line = line
                .trim_start()
                .strip_prefix("///")?
                .trim_start()
                .strip_prefix("<reference")?;
            let field = line.find("path=")?;
            let value = line[field + 5..].trim_start();
            let quote = value.chars().next()?;
            if quote != '\'' && quote != '"' {
                return None;
            }
            Some(value[1..].split(quote).next()?.to_owned())
        })
        .collect()
}

fn add_type_roots(
    fs: &dyn ResolutionFileSystem,
    directory: Option<&Path>,
    options: &ResolvedCompilerOptions,
    resolver: &mut ModuleResolver<'_>,
    pending: &mut VecDeque<PathBuf>,
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
            if fs
                .resolution_boundary()
                .is_some_and(|boundary| ancestor == boundary)
            {
                break;
            }
        }
        roots
    };
    let requested = options
        .types
        .as_ref()
        .map(|types| types.iter().cloned().collect::<BTreeSet<_>>());
    for root in roots {
        poll(checkpoint, 1)?;
        let Some(entries) = fs.read_dir(&root).map_err(|error| error.to_string())? else {
            continue;
        };
        for entry in entries {
            let Some(name) = entry.name.to_str() else {
                return Err("native TSZ @types name is not UTF-8".into());
            };
            if name.starts_with('.')
                || requested
                    .as_ref()
                    .is_some_and(|requested| !requested.contains(name))
            {
                continue;
            }
            let path = root.join(&entry.name);
            let manifest = path.join("package.json");
            let target =
                if let Some(bytes) = fs.read_file(&manifest).map_err(|error| error.to_string())? {
                    let value: serde_json::Value =
                        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                    value
                        .get("types")
                        .or_else(|| value.get("typings"))
                        .and_then(serde_json::Value::as_str)
                        .map_or_else(|| path.join("index.d.ts"), |entry| path.join(entry))
                } else {
                    path.join("index.d.ts")
                };
            if fs.is_file(&target).map_err(|error| error.to_string())? {
                pending.push_back(target);
            }
        }
    }
    if let Some(types) = &options.types {
        for name in types {
            poll(checkpoint, 1)?;
            let importer = directory.join("__native_tsz_type_reference__.ts");
            if let Ok(resolved) = resolver
                .resolve_with_kind_checked(
                    name,
                    &importer,
                    tsz::span::Span::new(0, 0),
                    ImportKind::EsmImport,
                )
                .map_err(|error| error.to_string())?
            {
                pending.push_back(resolved.resolved_path);
            }
        }
    }
    Ok(())
}
