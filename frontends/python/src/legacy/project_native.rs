//! Exact typed answers and definitions from one committed Pyrefly 1.2 State.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pyrefly::state::require::Require;
use pyrefly::state::state::State;
use pyrefly_config::base::{InferReturnTypes, Preset};
use pyrefly_config::config::{ConfigFile, ConfigSource, FallbackSearchPath, ProjectLayout};
use pyrefly_config::finder::{ConfigError, ConfigFinder};
use pyrefly_python::ast::Ast;
use pyrefly_python::module::TextRangeWithModule;
use pyrefly_python::module_name::{ModuleName, ModuleNameWithKind};
use pyrefly_python::module_path::ModulePath;
use pyrefly_python::sys_info::PythonVersion as NativeVersion;
use pyrefly_types::callable::{Callable, Param, Params};
use pyrefly_types::literal::Lit;
use pyrefly_types::tuple::Tuple;
use pyrefly_types::types::{BoundMethodType, Forallable, Type};
use pyrefly_util::arc_id::ArcId;
use pyrefly_util::thread_pool::ThreadCount;
use ruff_native_text_size::{Ranged, TextSize};
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_text_size::Ranged as SyntaxRanged;

use super::project::{
    CandidateWitness, CapturedBaselines, CapturedProjectLayout, DefinitionTarget, DirectoryWitness,
    PythonProjectBytesSource, PythonProjectControl, PythonProjectCoverageGap,
    PythonProjectCoverageGapKind, PythonProjectDiagnostic, PythonProjectSource,
    PythonTypeProjectionFault, SourceDirectoryWitness, checkpoint, project_error,
};
use super::{
    CheckerError, CheckerReport, ImportResolution, Inference, InferenceSite, InferredType,
    NativePythonTypeConstructor, SymbolOutcome, SymbolResolution,
};
use crate::legacy::{
    AnnotationPosition, DeclarationFact, DeclarationKind, ModuleFacts, OccurrenceKind, Span,
};

pub(super) struct NativeProjectResult {
    pub(super) modules: BTreeMap<Box<str>, CheckerReport>,
    pub(super) unavailable_dependencies: BTreeMap<Box<str>, Box<[Box<str>]>>,
    pub(super) configuration_fingerprint: [u8; 32],
    pub(super) candidates: Vec<CandidateWitness>,
    pub(super) diagnostics: Vec<PythonProjectDiagnostic>,
    pub(super) coverage_gaps: Vec<PythonProjectCoverageGap>,
    pub(super) frontier: Vec<SourceDirectoryWitness>,
    pub(super) mirror_tree: Vec<DirectoryWitness>,
}

pub(super) fn analyze(
    layout: &CapturedProjectLayout,
    original_root: &Path,
    package: &str,
    sources: &[PythonProjectSource<'_>],
    raw_sources: &[PythonProjectBytesSource<'_>],
    syntax: &BTreeMap<&str, ModuleFacts>,
    rejected: &BTreeMap<&str, crate::legacy::RejectedSyntax>,
    baselines: &CapturedBaselines,
    source_context: &backend_discovery::PythonSourceContext,
    profile: backend_semantic::vocabulary::PythonVersion,
    control: PythonProjectControl<'_>,
) -> Result<NativeProjectResult, CheckerError> {
    checkpoint(control)?;
    let mirror = layout.source_root();
    let source_manifest = raw_sources
        .iter()
        .map(|source| {
            let identity = backend_semantic::ir::SourceIdentity::from_bytes(source.source)
                .ok_or_else(|| {
                    project_error(
                        source.relative_path,
                        "selected source extent exceeds IR bounds",
                    )
                })?;
            Ok((source.relative_path.to_owned(), *identity.identity))
        })
        .collect::<Result<Vec<_>, CheckerError>>()?;
    let program_identity = backend_semantic::ir::python_program_identity(&source_manifest)
        .ok_or_else(|| project_error("", "invalid native selected source manifest"))?;
    let source_identities = source_manifest
        .iter()
        .map(|(path, identity)| (path.as_str(), *identity))
        .collect::<BTreeMap<_, _>>();
    let minor = super::profile_tag(profile)
        .split('.')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| project_error("", "invalid selected native Python version"))?;
    let (finder, configuration_fingerprint) = captured_finder(
        layout,
        original_root,
        raw_sources,
        baselines,
        NativeVersion::new(3, minor, 0),
        control,
    )?;
    let raw_handles = raw_sources
        .iter()
        .map(|source| {
            let path = ModulePath::filesystem(mirror.join(source.relative_path));
            let config =
                finder.python_file(ModuleNameWithKind::guaranteed(ModuleName::unknown()), &path);
            config.handle_from_module_path(path)
        })
        .collect::<Vec<_>>();
    let handles_by_path = raw_sources
        .iter()
        .zip(&raw_handles)
        .map(|(source, handle)| (source.relative_path, handle))
        .collect::<BTreeMap<_, _>>();
    let handles = sources
        .iter()
        .map(|source| (*handles_by_path[source.relative_path]).clone())
        .collect::<Vec<_>>();
    let mut imports = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    let mut compiled_imports = BTreeSet::new();
    let mut roots = BTreeSet::new();
    for handle in &raw_handles {
        let config = finder.python_file(handle.module_kind(), handle.path());
        for root in config.search_path().chain(config.site_package_path()) {
            let original = layout
                .original_search_root(root, original_root)
                .ok_or_else(|| CheckerError::UncapturedDependency { path: root.clone() })?;
            roots.insert(original);
        }
    }
    let frontier = SourceDirectoryWitness::capture_frontier(
        roots,
        original_root,
        mirror,
        raw_sources[0].relative_path,
        source_context,
        control,
    )?;
    let mut coverage_gaps = Vec::new();
    for (source, handle) in sources.iter().zip(&handles) {
        checkpoint(control)?;
        let parsed;
        let module_syntax = if let Some(rejection) = rejected.get(source.relative_path) {
            // Recovery syntax is used only to witness possible import reads, never
            // to certify declarations, definitions or type facts for this file.
            rejection.parsed.syntax()
        } else {
            parsed =
                crate::legacy::parse_module(source.source, profile).map_err(|source_error| {
                    CheckerError::ProjectSyntax {
                        path: source.relative_path.into(),
                        source: Box::new(source_error),
                    }
                })?;
            parsed.syntax()
        };
        let mut collector = ImportCollector {
            module: handle.module(),
            is_init: source.relative_path.ends_with("/__init__.py")
                || source.relative_path.ends_with("/__init__.pyi")
                || source.relative_path == "__init__.py"
                || source.relative_path == "__init__.pyi",
            imports: Vec::new(),
            gaps: Vec::new(),
            load_aliases: BTreeMap::new(),
            collect_imports: true,
        };
        if let ruff_python_ast::Mod::Module(module) = module_syntax {
            collector.visit_body(&module.body);
            collector.collect_imports = false;
            collector.visit_body(&module.body);
        }
        coverage_gaps.extend(
            collector
                .gaps
                .iter()
                .map(|(span, kind)| PythonProjectCoverageGap {
                    relative_path: source.relative_path.into(),
                    span: *span,
                    kind: *kind,
                }),
        );
        let config = finder.python_file(handle.module_kind(), handle.path());
        for import in &collector.imports {
            for root in config.search_path().chain(config.site_package_path()) {
                layout
                    .original_search_root(root, original_root)
                    .ok_or_else(|| CheckerError::UncapturedDependency { path: root.clone() })?;
                for module in &import.candidates {
                    let mut prefix = root.clone();
                    for component in module.as_str().split('.').filter(|part| !part.is_empty()) {
                        prefix.push(component);
                        let mut paths = vec![
                            (prefix.clone(), true),
                            (prefix.join("__init__.pyi"), true),
                            (prefix.join("__init__.py"), true),
                            (prefix.with_extension("pyi"), true),
                            (prefix.with_extension("py"), true),
                        ];
                        paths.extend(
                            pyrefly_python::COMPILED_FILE_SUFFIXES
                                .iter()
                                .map(|suffix| (prefix.with_extension(suffix), false)),
                        );
                        for (captured_path, requires_source_capture) in paths {
                            checkpoint(control)?;
                            let Some(path) = layout.original_source(&captured_path, original_root)
                            else {
                                // Only the genuinely named package exists at an owned
                                // synthetic import root. Never probe its original siblings.
                                if !captured_path.starts_with(layout.capture_root())
                                    || captured_path.exists()
                                {
                                    return Err(CheckerError::UncapturedDependency {
                                        path: captured_path,
                                    });
                                }
                                continue;
                            };
                            let probe = match candidates.entry(path.clone()) {
                                std::collections::btree_map::Entry::Occupied(entry) => {
                                    entry.into_mut()
                                }
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    entry.insert(CandidateWitness::capture(path.clone())?)
                                }
                            };
                            let relative = path
                                .strip_prefix(original_root)
                                .expect("configured package-local root");
                            if probe.is_present() && !requires_source_capture {
                                compiled_imports.insert((
                                    source.relative_path,
                                    import.span.start,
                                    import.span.end,
                                ));
                            }
                            if requires_source_capture
                                && probe.is_present()
                                && !mirror.join(relative).exists()
                            {
                                return Err(CheckerError::IncompleteSourceFrontier {
                                    source_path: source.relative_path.into(),
                                    module: module.as_str().into(),
                                    candidate: path,
                                });
                            }
                        }
                    }
                }
            }
        }
        imports.insert(source.relative_path, collector.imports);
    }
    for directory in &frontier {
        directory.validate_current(control)?;
    }
    let mirror_tree = DirectoryWitness::capture_tree(layout.capture_root(), control)?;
    let state = State::new(finder, ThreadCount::NumThreads(std::num::NonZeroUsize::MIN));
    let mut transaction = state.new_committable_transaction(Require::Everything, None);
    let cancellation = transaction.as_mut().get_cancellation_handle();
    let done = AtomicBool::new(false);
    let completed = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if checkpoint(control).is_err() {
                    cancellation.cancel();
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transaction
                .as_mut()
                .run(&handles, Require::Everything, None)
        }));
        done.store(true, Ordering::Release);
        completed
    });
    if completed.is_err() {
        return Err(CheckerError::ProjectPanic);
    }
    checkpoint(control)?;
    state.commit_transaction(transaction, None);
    let read = state.transaction();
    let errors = read.get_config_errors();
    if !errors.is_empty() {
        return Err(project_error(
            "",
            &format!(
                "native configuration errors: {}",
                errors
                    .iter()
                    .map(ConfigError::get_message)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    }
    // Recovery answers from an unavailable resource must not leak precise
    // authority through otherwise valid importing files. Use this committed
    // State's exact Handle dependency graph, preserving configured path/style
    // selection; no textual module-name matching supplies the reverse closure.
    let unavailable_paths = raw_sources
        .iter()
        .filter(|source| !syntax.contains_key(source.relative_path))
        .map(|source| source.relative_path)
        .collect::<BTreeSet<_>>();
    let selected_native_paths = raw_sources
        .iter()
        .zip(&raw_handles)
        .map(|(source, handle)| (handle.path().as_path(), source.relative_path))
        .collect::<BTreeMap<_, _>>();
    let mut unavailable_dependencies = BTreeMap::<Box<str>, BTreeSet<Box<str>>>::new();
    for loaded in read.handles() {
        checkpoint(control)?;
        let Some(path) = selected_native_paths
            .get(loaded.path().as_path())
            .filter(|path| unavailable_paths.contains(**path))
        else {
            continue;
        };
        let unavailable_path: Box<str> = (*path).into();
        for dependent in read.get_transitive_rdeps(loaded) {
            checkpoint(control)?;
            let Some(dependent_path) = selected_native_paths
                .get(dependent.path().as_path())
                .filter(|path| syntax.contains_key(**path))
            else {
                continue;
            };
            unavailable_dependencies
                .entry((*dependent_path).into())
                .or_default()
                .insert(unavailable_path.clone());
        }
    }
    let selected = sources
        .iter()
        .map(|source| (source.relative_path, source.source))
        .collect::<BTreeMap<_, _>>();
    let mut modules = BTreeMap::new();
    let mut diagnostics = Vec::new();
    let mut projection = TypeProjection::new(control);
    // The committed State and captured syntax are immutable for this projection.
    // Index exact identifier sites once, retaining ambiguous sites as unresolved.
    let mut declaration_sites = BTreeMap::<(&str, u32, u32), Option<&DeclarationFact>>::new();
    for (path, facts) in syntax {
        for declaration in &facts.declarations {
            checkpoint(control)?;
            if declaration.kind == DeclarationKind::Module {
                continue;
            }
            let site = (
                *path,
                declaration.name_span.start,
                declaration.name_span.end,
            );
            match declaration_sites.entry(site) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Some(declaration));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
    }
    let mut definition_templates = BTreeMap::<(&str, u32, u32), DefinitionTemplate>::new();
    let mut import_binding_sites = BTreeSet::new();
    for (path, probes) in &imports {
        for import in probes {
            checkpoint(control)?;
            import_binding_sites.insert((
                *path,
                import.binding_span.start,
                import.binding_span.end,
            ));
        }
    }
    for (source, handle) in sources.iter().zip(&handles) {
        checkpoint(control)?;
        let native_module = read.get_module_info(handle).ok_or_else(|| {
            project_error(source.relative_path, "native selected module unavailable")
        })?;
        if native_module.contents().as_bytes() != source.source.as_bytes() {
            return Err(project_error(
                source.relative_path,
                "native selected module differs from captured bytes",
            ));
        }
        if unavailable_dependencies.contains_key(source.relative_path) {
            modules.insert(source.relative_path.into(), CheckerReport::default());
            coverage_gaps.push(PythonProjectCoverageGap {
                relative_path: source.relative_path.into(),
                span: Span {
                    start: 0,
                    end: source.source.len() as u32,
                },
                kind: PythonProjectCoverageGapKind::UnavailableDependency,
            });
            continue;
        }
        let Some(facts) = syntax.get(source.relative_path) else {
            // This exact decoded source still ran through the native parser, so
            // its native diagnostics below remain available. Recovered syntax
            // must never become precise semantic authority for a rejected file.
            if !rejected.contains_key(source.relative_path) {
                return Err(project_error(
                    source.relative_path,
                    "source admission status is missing",
                ));
            }
            modules.insert(source.relative_path.into(), CheckerReport::default());
            coverage_gaps.push(PythonProjectCoverageGap {
                relative_path: source.relative_path.into(),
                span: Span {
                    start: 0,
                    end: source.source.len() as u32,
                },
                kind: PythonProjectCoverageGapKind::UnavailableSyntax,
            });
            continue;
        };
        let annotated_returns = annotated_function_returns(facts, control)?;
        let native_ast = read.get_ast(handle);
        let mut inferences = Vec::new();
        for declaration in &facts.declarations {
            checkpoint(control)?;
            let Some(ty) = read.get_type_at_preserving_declaration(
                handle,
                TextSize::new(declaration.name_span.start),
            ) else {
                continue;
            };
            if declaration.kind == DeclarationKind::Function {
                let Some(callable) = callable(&ty) else {
                    continue;
                };
                if !annotated_returns
                    .contains(&(declaration.name_span.start, declaration.name_span.end))
                {
                    inferences.push(Inference {
                        site: declaration.name_span,
                        kind: InferenceSite::Return,
                        observed: projection.convert(
                            &callable.ret,
                            source.relative_path,
                            declaration.name_span,
                        )?,
                    });
                }
                if let Params::List(parameters) = &callable.params {
                    for parameter in parameters.items() {
                        let (name, ty) = match parameter {
                            Param::Pos(name, ty, _) | Param::KwOnly(name, ty, _) => {
                                (Some(name.as_str()), ty)
                            }
                            Param::PosOnly(name, ty, _)
                            | Param::Varargs(name, ty)
                            | Param::Kwargs(name, ty) => {
                                (name.as_ref().map(|name| name.as_str()), ty)
                            }
                        };
                        if let Some(written) = declaration.parameters.iter().find(|written| {
                            Some(written.name.as_str()) == name && written.annotation_span.is_none()
                        }) {
                            inferences.push(Inference {
                                site: written.name_span,
                                kind: InferenceSite::Parameter,
                                observed: projection.convert(
                                    ty,
                                    source.relative_path,
                                    written.name_span,
                                )?,
                            });
                        }
                    }
                }
            } else if matches!(
                declaration.kind,
                DeclarationKind::Field | DeclarationKind::Constant
            ) {
                if facts.annotations.iter().any(|annotation| {
                    annotation.position == AnnotationPosition::Field
                        && annotation.owner == declaration.name
                        && declaration.span.start <= annotation.span.start
                        && annotation.span.end <= declaration.span.end
                }) {
                    continue;
                }
                inferences.push(Inference {
                    site: declaration.name_span,
                    kind: if declaration.kind == DeclarationKind::Field {
                        InferenceSite::ClassField
                    } else {
                        InferenceSite::ModuleBinding
                    },
                    observed: projection.convert(
                        &ty,
                        source.relative_path,
                        declaration.name_span,
                    )?,
                });
            }
        }
        for inference in &inferences {
            let mut pending = vec![&inference.observed];
            while let Some(ty) = pending.pop() {
                checkpoint(control)?;
                match ty {
                    InferredType::Unavailable(constructor) => {
                        diagnostics.push(PythonProjectDiagnostic {
                            relative_path: source.relative_path.into(),
                            span: inference.site,
                            kind: "unavailable-type-projection".into(),
                            severity: "info".into(),
                            message: format!(
                                "native {constructor:?} type has no admitted structural projection"
                            )
                            .into_boxed_str(),
                        })
                    }
                    InferredType::Tuple(children) | InferredType::Union(children) => {
                        pending.extend(children.iter())
                    }
                    InferredType::List(Some(child)) | InferredType::Set(Some(child)) => {
                        pending.push(child)
                    }
                    InferredType::Dict(Some((key, value))) => {
                        pending.push(key);
                        pending.push(value);
                    }
                    _ => {}
                }
            }
        }
        let mut symbols = Vec::new();
        for occurrence in &facts.occurrences {
            checkpoint(control)?;
            let position = occurrence
                .span
                .end
                .checked_sub(occurrence.target.len() as u32)
                .filter(|start| *start >= occurrence.span.start)
                .unwrap_or(occurrence.span.start);
            // A native ClassDef binds the class declaration. The native LSP
            // definition query can independently select its constructor callee.
            // Keep those answers distinct; never infer a class from a parent name.
            // In this pinned release, the identifier API still substitutes a
            // chosen overload for attribute callees. Query the exact native AST
            // expression range to retain its raw ClassDef declaration link.
            let expression_range = native_ast.as_ref().and_then(|ast| {
                Ast::locate_node(ast, TextSize::new(position))
                    .into_iter()
                    .find_map(|node| {
                        node.as_expr_ref()
                            .map(|expression| expression.range())
                            .filter(|range| {
                                range.start().to_u32() <= position
                                    && range.end().to_u32() == occurrence.span.end
                            })
                    })
            });
            let bound_type = expression_range
                .and_then(|range| read.get_computed_type_at_range(handle, range))
                .or_else(|| {
                    read.get_type_at_preserving_declaration(handle, TextSize::new(position))
                });
            let class_binding = match bound_type {
                Some(Type::ClassDef(class)) => {
                    let qname = class.qname();
                    Some((
                        TextRangeWithModule::new(qname.module().clone(), qname.range()),
                        qname.module_qualified_name().into_boxed_str(),
                    ))
                }
                _ => None,
            };
            let is_class = class_binding.is_some();
            let class_name = class_binding.as_ref().map(|(_, name)| name.clone());
            let definitions = class_binding
                .into_iter()
                .map(|(definition, _)| (true, definition))
                .chain(
                    read.goto_definition(handle, TextSize::new(position))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|definition| (false, definition)),
                );
            let mut bindings = Vec::new();
            let mut callees = Vec::new();
            for (is_binding, definition) in definitions {
                let Ok(path) = definition.module.path().as_path().strip_prefix(mirror) else {
                    continue;
                };
                let Some(path) = path.to_str() else {
                    continue;
                };
                let Some((path, target_source)) = selected.get_key_value(path) else {
                    continue;
                };
                let path = *path;
                let target_source = *target_source;
                if definition.module.contents().as_bytes() != target_source.as_bytes() {
                    return Err(project_error(
                        path,
                        "native definition module differs from captured bytes",
                    ));
                }
                let span = Span {
                    start: definition.range.start().to_u32(),
                    end: definition.range.end().to_u32(),
                };
                if target_source
                    .get(span.start as usize..span.end as usize)
                    .is_none()
                {
                    return Err(project_error(
                        path,
                        "native definition span is outside captured UTF-8 bytes",
                    ));
                }
                let site = (path, span.start, span.end);
                let Some(target) = declaration_sites.get(&site).copied().flatten() else {
                    continue;
                };
                // Native fallback to an import binding still does not prove a
                // callable target. Keep this refusal before coordinate encoding.
                if matches!(
                    occurrence.kind,
                    OccurrenceKind::FunctionCall | OccurrenceKind::MethodCall
                ) && target.kind == DeclarationKind::Alias
                    && import_binding_sites.contains(&site)
                {
                    continue;
                }
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    definition_templates.entry(site)
                {
                    let mut enclosing = syntax[path]
                        .declarations
                        .iter()
                        .filter(|candidate| {
                            matches!(
                                candidate.kind,
                                DeclarationKind::Class | DeclarationKind::Function
                            ) && candidate.name_span != target.name_span
                                && candidate.binding_span.start <= target.binding_span.start
                                && target.binding_span.end <= candidate.binding_span.end
                        })
                        .collect::<Vec<_>>();
                    enclosing.sort_by_key(|candidate| {
                        (
                            candidate.binding_span.start,
                            std::cmp::Reverse(candidate.binding_span.end),
                        )
                    });
                    let mut scopes = enclosing
                        .iter()
                        .map(|candidate| candidate.name.as_str())
                        .collect::<Vec<_>>();
                    scopes.push(target.name.as_str());
                    let source_identity = source_identities.get(path).ok_or_else(|| {
                        project_error(path, "native target is outside selected source identities")
                    })?;
                    let source_coordinate = backend_semantic::ir::PythonSourceCoordinate(
                        backend_semantic::ir::SourceDeclarationCoordinate {
                            program: program_identity,
                            source: *source_identity,
                            path,
                            declaration_start: target.binding_span.start,
                            declaration_end: target.binding_span.end,
                            name_start: target.name_span.start,
                        },
                    )
                    .encode()
                    .ok_or_else(|| {
                        project_error(path, "native target source coordinate is inadmissible")
                    })?;
                    entry.insert(DefinitionTemplate {
                        source_coordinate: source_coordinate.into_boxed_str(),
                        qualified_suffix: scopes.join(".").into_boxed_str(),
                    });
                }
                let template = &definition_templates[&site];
                let target = DefinitionTarget {
                    relative_path: path.into(),
                    source_coordinate: template.source_coordinate.clone(),
                    package: package.into(),
                    name_span: span,
                    qualified_name: if is_binding {
                        class_name.clone().expect("native ClassDef name")
                    } else {
                        format!("{}.{}", definition.module.name(), template.qualified_suffix)
                            .into_boxed_str()
                    },
                    name: target.name.clone().into_boxed_str(),
                    kind: target.kind,
                    same_module: path == source.relative_path,
                };
                let targets = if is_binding {
                    &mut bindings
                } else {
                    &mut callees
                };
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
            let primary = if is_class {
                bindings.as_slice()
            } else {
                callees.as_slice()
            };
            let outcome = match primary {
                [target] => SymbolOutcome::Definition {
                    target: target.clone(),
                    callee: match callees.as_slice() {
                        [callee] if is_class && callee != target => Some(callee.clone()),
                        _ => None,
                    },
                },
                _ => SymbolOutcome::Unresolved,
            };
            symbols.push(SymbolResolution {
                target: occurrence.target.clone(),
                span: occurrence.span,
                outcome,
            });
        }
        let import_resolutions = imports[source.relative_path]
            .iter()
            .map(|import| {
                let resolved = read
                    .import_handle(handle, import.module, None)
                    .finding()
                    .is_some()
                    && read
                        .get_type_at_preserving_declaration(
                            handle,
                            TextSize::new(import.binding_span.start),
                        )
                        .is_some_and(|ty| match ty {
                            Type::Any(_) => false,
                            Type::Module(module) => read
                                .import_handle(
                                    handle,
                                    ModuleName::from_parts(
                                        module.parts().iter().map(|part| part.as_str()),
                                    ),
                                    None,
                                )
                                .finding()
                                .is_some(),
                            _ => true,
                        });
                if !resolved {
                    coverage_gaps.push(PythonProjectCoverageGap {
                        relative_path: source.relative_path.into(),
                        span: import.span,
                        kind: if compiled_imports.contains(&(
                            source.relative_path,
                            import.span.start,
                            import.span.end,
                        )) {
                            PythonProjectCoverageGapKind::UnavailableCompiledImport
                        } else {
                            PythonProjectCoverageGapKind::UnavailableImport
                        },
                    });
                }
                ImportResolution {
                    binding: import.binding.clone(),
                    module: import.module.as_str().to_owned(),
                    module_span: import.span,
                    resolved,
                }
            })
            .collect();
        modules.insert(
            source.relative_path.into(),
            CheckerReport {
                inferences: inferences.into_boxed_slice(),
                imports: import_resolutions,
                symbols: symbols.into_boxed_slice(),
            },
        );
    }
    checkpoint(control)?;
    for error in read.get_errors(&handles).collect_display_errors() {
        checkpoint(control)?;
        let Some(path) = selected_native_paths.get(error.path().as_path()) else {
            continue;
        };
        diagnostics.push(PythonProjectDiagnostic {
            relative_path: (*path).into(),
            span: Span {
                start: error.range().start().to_u32(),
                end: error.range().end().to_u32(),
            },
            kind: error.error_kind().to_name().into(),
            severity: error.severity().label().trim().into(),
            message: error.msg().into_boxed_str(),
        });
    }
    Ok(NativeProjectResult {
        modules,
        unavailable_dependencies: unavailable_dependencies
            .into_iter()
            .map(|(path, dependencies)| (path, dependencies.into_iter().collect()))
            .collect(),
        configuration_fingerprint,
        candidates: candidates.into_values().collect(),
        diagnostics,
        coverage_gaps,
        frontier,
        mirror_tree,
    })
}

/// Syntax-derived parts shared by every native answer at one exact declaration
/// site. Native module names and ClassDef qualified names remain per-answer.
struct DefinitionTemplate {
    source_coordinate: Box<str>,
    qualified_suffix: Box<str>,
}

fn annotated_function_returns(
    facts: &ModuleFacts,
    control: PythonProjectControl<'_>,
) -> Result<BTreeSet<(u32, u32)>, CheckerError> {
    let mut sites = BTreeSet::new();
    for annotation in &facts.annotations {
        checkpoint(control)?;
        if annotation.position != AnnotationPosition::Return {
            continue;
        }
        if let Some(owner) = facts
            .declarations
            .iter()
            .filter(|candidate| {
                candidate.kind == DeclarationKind::Function
                    && candidate.span.start <= annotation.span.start
                    && annotation.span.end <= candidate.span.end
            })
            .min_by_key(|candidate| candidate.span.end - candidate.span.start)
        {
            sites.insert((owner.name_span.start, owner.name_span.end));
        }
    }
    Ok(sites)
}

struct ImportProbe {
    binding: String,
    module: ModuleName,
    span: Span,
    binding_span: Span,
    candidates: Vec<ModuleName>,
}

struct ImportCollector {
    module: ModuleName,
    is_init: bool,
    imports: Vec<ImportProbe>,
    gaps: Vec<(Span, PythonProjectCoverageGapKind)>,
    load_aliases: BTreeMap<String, PythonProjectCoverageGapKind>,
    collect_imports: bool,
}

impl<'syntax> Visitor<'syntax> for ImportCollector {
    fn visit_expr(&mut self, expression: &'syntax ruff_python_ast::Expr) {
        if !self.collect_imports
            && let ruff_python_ast::Expr::Call(call) = expression
        {
            let name = match call.func.as_ref() {
                ruff_python_ast::Expr::Name(name) => Some(name.id.as_str()),
                ruff_python_ast::Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
                _ => None,
            };
            let kind = match name {
                Some("__import__" | "import_module") => {
                    Some(PythonProjectCoverageGapKind::DynamicImport)
                }
                Some("iter_modules" | "walk_packages") => {
                    Some(PythonProjectCoverageGapKind::ModuleEnumeration)
                }
                name => name.and_then(|name| self.load_aliases.get(name).copied()),
            };
            if let Some(kind) = kind {
                self.gaps.push((
                    Span {
                        start: expression.range().start().to_u32(),
                        end: expression.range().end().to_u32(),
                    },
                    kind,
                ));
            }
        }
        walk_expr(self, expression);
    }
    fn visit_stmt(&mut self, statement: &'syntax ruff_python_ast::Stmt) {
        if !self.collect_imports {
            walk_stmt(self, statement);
            return;
        }
        match statement {
            ruff_python_ast::Stmt::Import(import) => {
                for alias in &import.names {
                    let module = ModuleName::from_parts(alias.name.as_str().split('.'));
                    self.imports.push(ImportProbe {
                        binding: alias.asname.as_ref().map_or_else(
                            || {
                                alias
                                    .name
                                    .as_str()
                                    .split('.')
                                    .next()
                                    .unwrap_or_default()
                                    .to_owned()
                            },
                            |name| name.as_str().to_owned(),
                        ),
                        module,
                        span: Span {
                            start: alias.name.range().start().to_u32(),
                            end: alias.name.range().end().to_u32(),
                        },
                        binding_span: Span {
                            start: alias
                                .asname
                                .as_ref()
                                .unwrap_or(&alias.name)
                                .range()
                                .start()
                                .to_u32(),
                            end: alias
                                .asname
                                .as_ref()
                                .unwrap_or(&alias.name)
                                .range()
                                .end()
                                .to_u32(),
                        },
                        candidates: vec![module],
                    });
                }
            }
            ruff_python_ast::Stmt::ImportFrom(import) => {
                let mut base = if import.level == 0 {
                    Vec::new()
                } else {
                    let mut parts = self.module.as_str().split('.').collect::<Vec<_>>();
                    let remove = import.level.saturating_sub(u32::from(self.is_init)) as usize;
                    if remove > parts.len() {
                        return;
                    }
                    parts.truncate(parts.len() - remove);
                    parts
                };
                if let Some(suffix) = &import.module {
                    base.extend(suffix.as_str().split('.'));
                }
                let module = ModuleName::from_parts(&base);
                for alias in &import.names {
                    let primitive = match (module.as_str(), alias.name.as_str()) {
                        ("importlib", "import_module") | ("builtins", "__import__") => {
                            Some(PythonProjectCoverageGapKind::DynamicImport)
                        }
                        ("pkgutil", "iter_modules" | "walk_packages") => {
                            Some(PythonProjectCoverageGapKind::ModuleEnumeration)
                        }
                        _ => None,
                    };
                    if let Some(kind) = primitive {
                        self.load_aliases.insert(
                            alias
                                .asname
                                .as_ref()
                                .unwrap_or(&alias.name)
                                .as_str()
                                .to_owned(),
                            kind,
                        );
                    }
                    if alias.name.as_str() == "*" {
                        self.gaps.push((
                            Span {
                                start: alias.name.range().start().to_u32(),
                                end: alias.name.range().end().to_u32(),
                            },
                            PythonProjectCoverageGapKind::WildcardImport,
                        ));
                    }
                    let mut candidates = vec![module];
                    if alias.name.as_str() != "*" {
                        let mut child = base.clone();
                        child.push(alias.name.as_str());
                        candidates.push(ModuleName::from_parts(child));
                    }
                    let module_range = import
                        .module
                        .as_ref()
                        .map_or(alias.name.range(), |name| name.range());
                    self.imports.push(ImportProbe {
                        binding: alias
                            .asname
                            .as_ref()
                            .unwrap_or(&alias.name)
                            .as_str()
                            .to_owned(),
                        module,
                        span: Span {
                            start: module_range.start().to_u32(),
                            end: module_range.end().to_u32(),
                        },
                        binding_span: Span {
                            start: alias
                                .asname
                                .as_ref()
                                .unwrap_or(&alias.name)
                                .range()
                                .start()
                                .to_u32(),
                            end: alias
                                .asname
                                .as_ref()
                                .unwrap_or(&alias.name)
                                .range()
                                .end()
                                .to_u32(),
                        },
                        candidates,
                    });
                }
            }
            _ => walk_stmt(self, statement),
        }
    }
}

/// No ancestor discovery, interpreter/site-package probing, or external loader
/// roots are admitted. All filesystem module searches remain in the finite
/// private tree; standard/third-party bundled stubs are immutable producer data.
fn captured_finder(
    layout: &CapturedProjectLayout,
    original_root: &Path,
    sources: &[PythonProjectBytesSource<'_>],
    baselines: &CapturedBaselines,
    version: NativeVersion,
    control: PythonProjectControl<'_>,
) -> Result<(ConfigFinder, [u8; 32]), CheckerError> {
    fn configure(
        mut config: ConfigFile,
        layout: &CapturedProjectLayout,
        original: &Path,
        baselines: &CapturedBaselines,
        version: NativeVersion,
    ) -> Result<ArcId<ConfigFile>, CheckerError> {
        if let Some(path) = &config.typeshed_path {
            return Err(CheckerError::UncapturedDependency { path: path.clone() });
        }
        if let Some(path) = &mut config.baseline {
            baselines.admit(path, layout, original)?;
        }
        if config.build_system.is_some() || config.source_db.is_some() {
            return Err(project_error(
                "",
                "build-system dependency sources are outside the captured native contract",
            ));
        }
        for path in config
            .search_path_from_args
            .iter_mut()
            .chain(&mut config.search_path_from_file)
        {
            layout.rebase(path, original)?;
        }
        if let Some(path) = &mut config.import_root {
            layout.rebase_import_root(path, original)?;
        }
        // Explicit site paths may only refer to captured mirror members. An
        // empty explicit list prevents native typings/ interpreter discovery.
        if let Some(paths) = &mut config.python_environment.site_package_path {
            for path in paths {
                layout.rebase(path, original)?;
            }
        } else {
            config.python_environment.site_package_path = Some(Vec::new());
        }
        if !config
            .python_environment
            .interpreter_site_package_path
            .is_empty()
            || !config.python_environment.interpreter_stdlib_path.is_empty()
        {
            return Err(project_error(
                "",
                "interpreter dependency sources are outside the captured native contract",
            ));
        }
        // Preserve explicit captured diagnostic policy. The native authority
        // must not silence missing dependencies by imposing an IDE-only preset.
        config.preset.get_or_insert(Preset::Default);
        config.root.check_unannotated_defs = Some(true);
        config.root.infer_return_types = Some(InferReturnTypes::Checked);
        config.interpreters.skip_interpreter_query = true;
        config.python_environment.python_version = Some(version);
        config.fallback_search_path = FallbackSearchPath::Empty;
        config.enable_fallback_search_path = false;
        // Ignore-file and project-index discovery is not needed for these exact
        // handles. Never let it open uncaptured .gitignore or index paths.
        config.use_ignore_files = false;
        config.skip_lsp_config_indexing = true;
        let errors = config.configure();
        if !errors.is_empty() {
            return Err(project_error(
                "",
                &format!(
                    "native captured configuration errors: {}",
                    errors
                        .iter()
                        .map(ConfigError::get_message)
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ));
        }
        Ok(ArcId::new(config))
    }
    let mirror = layout.source_root();
    let mut directories = BTreeSet::from([mirror.to_path_buf()]);
    for source in sources {
        let path = mirror.join(source.relative_path);
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|directory| directory.starts_with(mirror)) {
            directories.insert(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    let fallback = configure(
        ConfigFile::init_at_root(mirror, &ProjectLayout::new(mirror), false),
        layout,
        original_root,
        baselines,
        version,
    )?;
    let mut loaded = BTreeMap::new();
    for directory in &directories {
        for name in ConfigFile::CONFIG_FILE_NAMES
            .iter()
            .chain(ConfigFile::ADDITIONAL_ROOT_FILE_NAMES)
        {
            checkpoint(control)?;
            let path = directory.join(name);
            if !path.is_file() {
                continue;
            }
            let (config, errors) = ConfigFile::from_file(&path);
            if !errors.is_empty() {
                let relative = path.strip_prefix(mirror).map_err(|_| {
                    project_error("", "native config is outside the captured mirror")
                })?;
                return Err(super::project_configuration::configuration_error(
                    &path,
                    &original_root.join(relative),
                    &errors,
                    control,
                )?);
            }
            let priority = match config.source {
                ConfigSource::File(_) | ConfigSource::FailedParse(_) => 0,
                ConfigSource::PythonToolMarker(_) => 1,
                _ => 2,
            };
            loaded.insert(
                path,
                (
                    priority,
                    configure(config, layout, original_root, baselines, version)?,
                ),
            );
        }
    }
    let mut effective = BTreeMap::new();
    let mut scope_identity = blake3::Hasher::new();
    scope_identity.update(b"compiler.python.effective-config-scope.v1\0");
    scope_identity.update(b"root-isolated;named-package-import-root.v1;captured-diagnostic-baseline.v1;native-priority;candidate-presence+absence;checked-unannotated;checked-returns;no-interpreter;no-fallback;no-ignore;no-index;classdef+ctor;depth=64;work=262144\0");
    for directory in directories {
        let mut candidates = Vec::new();
        for (depth, ancestor) in directory
            .ancestors()
            .take_while(|ancestor| ancestor.starts_with(mirror))
            .enumerate()
        {
            for (ordinal, name) in ConfigFile::CONFIG_FILE_NAMES
                .iter()
                .chain(ConfigFile::ADDITIONAL_ROOT_FILE_NAMES)
                .enumerate()
            {
                if let Some((priority, config)) = loaded.get(&ancestor.join(name)) {
                    candidates.push(((*priority, depth, ordinal), ancestor.join(name), config));
                }
            }
        }
        candidates.sort_by_key(|(priority, _, _)| *priority);
        let config = candidates
            .first()
            .map_or_else(|| fallback.clone(), |(_, _, config)| (*config).clone());
        for path in [
            Some(directory.as_path()),
            candidates.first().map(|(_, path, _)| path.as_path()),
        ] {
            if let Some(path) = path {
                let relative = path
                    .strip_prefix(mirror)
                    .expect("captured configuration scope");
                let bytes = relative.as_os_str().as_encoded_bytes();
                scope_identity.update(&[1]);
                scope_identity.update(&(bytes.len() as u64).to_be_bytes());
                scope_identity.update(bytes);
            } else {
                scope_identity.update(&[0]);
            }
        }
        effective.insert(directory, config);
    }
    let before_root = layout.capture_root().to_path_buf();
    let before_fallback = fallback.clone();
    let load_fallback = fallback.clone();
    Ok((
        ConfigFinder::new_custom(
            Box::new(move |_, path| {
                let path = path.as_path();
                if path.is_absolute() {
                    // This cannot be reached by the validated resolver roots. A
                    // producer violation terminates before opening ambient bytes.
                    assert!(
                        path.starts_with(&before_root),
                        "native resolver escaped captured mirror"
                    );
                    let config = path
                        .ancestors()
                        .find_map(|ancestor| effective.get(ancestor))
                        .unwrap_or(&before_fallback);
                    Ok(Some(config.clone()))
                } else {
                    // Bundled module paths have no filesystem source path.
                    Ok(Some(before_fallback.clone()))
                }
            }),
            Box::new(move |_| {
                // python_file always returns above. No directory/config discovery
                // can influence this producer through the fallback load callback.
                (load_fallback.clone(), Vec::new())
            }),
            Box::new(move |_, _| fallback.clone()),
            Box::new(|| {}),
        ),
        *scope_identity.finalize().as_bytes(),
    ))
}

fn callable(ty: &Type) -> Option<&Callable> {
    match ty {
        Type::Function(function) => Some(&function.signature),
        Type::Callable(callable) => Some(callable),
        Type::BoundMethod(method) => match &method.func {
            BoundMethodType::Function(function) => Some(&function.signature),
            BoundMethodType::Forall(forall) => Some(&forall.body.signature),
            BoundMethodType::Overload(_) => None,
        },
        Type::Forall(forall) => match &forall.body {
            Forallable::Function(function) => Some(&function.signature),
            Forallable::Callable(callable) => Some(callable),
            _ => None,
        },
        // A source branch cannot be identified by taking an arbitrary overload.
        _ => None,
    }
}

const TYPE_DEPTH: usize = 64;
const TYPE_WORK: usize = 262_144;

/// One allowance shared by every inference in the solved transaction. The
/// traversal borrows native nodes and keeps recursion off the thread stack.
struct TypeProjection<'control> {
    control: PythonProjectControl<'control>,
    remaining: usize,
}

#[derive(Clone, Copy)]
enum TypeContainer {
    Union,
    Tuple,
    List,
    Set,
    Dict,
}

enum TypeStep<'type_> {
    Visit(&'type_ Type, usize),
    Build(TypeContainer, usize),
    Leave(usize),
}

fn queue_children<'type_>(
    pending: &mut Vec<TypeStep<'type_>>,
    container: TypeContainer,
    children: &'type_ [Type],
    depth: usize,
    remaining: &mut usize,
) -> Result<(), PythonTypeProjectionFault> {
    if children.len() > *remaining {
        return Err(PythonTypeProjectionFault::Work { limit: TYPE_WORK });
    }
    *remaining -= children.len();
    pending.push(TypeStep::Build(container, children.len()));
    pending.extend(
        children
            .iter()
            .rev()
            .map(|child| TypeStep::Visit(child, depth + 1)),
    );
    Ok(())
}

impl<'control> TypeProjection<'control> {
    fn new(control: PythonProjectControl<'control>) -> Self {
        Self {
            control,
            remaining: TYPE_WORK,
        }
    }

    fn convert(&mut self, ty: &Type, path: &str, site: Span) -> Result<InferredType, CheckerError> {
        let refusal = |cause| CheckerError::NativeTypeProjection {
            path: path.into(),
            site,
            cause,
        };
        checkpoint(self.control)?;
        if self.remaining == 0 {
            return Err(refusal(PythonTypeProjectionFault::Work {
                limit: TYPE_WORK,
            }));
        }
        self.remaining -= 1;
        let mut pending = vec![TypeStep::Visit(ty, 0)];
        let mut active = BTreeSet::new();
        let mut values = Vec::new();
        while let Some(step) = pending.pop() {
            checkpoint(self.control)?;
            match step {
                TypeStep::Leave(identity) => {
                    active.remove(&identity);
                }
                TypeStep::Build(container, count) => {
                    let children = values.split_off(values.len() - count);
                    let value = match container {
                        TypeContainer::Union => InferredType::Union(children.into_boxed_slice()),
                        TypeContainer::Tuple => InferredType::Tuple(children.into_boxed_slice()),
                        TypeContainer::List => {
                            InferredType::List(children.into_iter().next().map(Box::new))
                        }
                        TypeContainer::Set => {
                            InferredType::Set(children.into_iter().next().map(Box::new))
                        }
                        TypeContainer::Dict => {
                            let mut children = children.into_iter();
                            InferredType::Dict(Some((
                                Box::new(children.next().expect("dict key")),
                                Box::new(children.next().expect("dict value")),
                            )))
                        }
                    };
                    values.push(value);
                }
                TypeStep::Visit(ty, depth) => {
                    if depth > TYPE_DEPTH {
                        return Err(refusal(PythonTypeProjectionFault::Depth {
                            observed: depth,
                            limit: TYPE_DEPTH,
                        }));
                    }
                    let identity = std::ptr::from_ref(ty) as usize;
                    if !active.insert(identity) {
                        return Err(refusal(PythonTypeProjectionFault::Cycle));
                    }
                    pending.push(TypeStep::Leave(identity));
                    let leaf = match ty {
                        Type::None => Some(InferredType::NoneType),
                        Type::Literal(literal) => Some(match &literal.value {
                            Lit::Str(_) => InferredType::Str,
                            Lit::Int(_) => InferredType::Integer,
                            Lit::Bool(_) => InferredType::Boolean,
                            Lit::Bytes(_) => InferredType::Bytes,
                            Lit::Enum(_) => {
                                InferredType::Unavailable(NativePythonTypeConstructor::EnumLiteral)
                            }
                        }),
                        Type::LiteralString(_) => Some(InferredType::Str),
                        Type::Union(union) => {
                            queue_children(
                                &mut pending,
                                TypeContainer::Union,
                                &union.members,
                                depth,
                                &mut self.remaining,
                            )
                            .map_err(refusal)?;
                            None
                        }
                        Type::Tuple(Tuple::Concrete(elements)) => {
                            queue_children(
                                &mut pending,
                                TypeContainer::Tuple,
                                elements,
                                depth,
                                &mut self.remaining,
                            )
                            .map_err(refusal)?;
                            None
                        }
                        Type::Annotated(inner, _) | Type::Unpack(inner) => {
                            if self.remaining == 0 {
                                return Err(refusal(PythonTypeProjectionFault::Work {
                                    limit: TYPE_WORK,
                                }));
                            }
                            self.remaining -= 1;
                            pending.push(TypeStep::Visit(inner, depth + 1));
                            None
                        }
                        Type::ClassType(class) => {
                            let builtin = [
                                ("int", InferredType::Integer),
                                ("float", InferredType::Float),
                                ("bool", InferredType::Boolean),
                                ("str", InferredType::Str),
                                ("bytes", InferredType::Bytes),
                                ("complex", InferredType::Complex),
                            ]
                            .into_iter()
                            .find(|(name, _)| class.is_builtin(name));
                            if let Some((_, value)) = builtin {
                                Some(value)
                            } else if class.is_builtin("list")
                                || class.is_builtin("set")
                                || class.is_builtin("frozenset")
                            {
                                let container = if class.is_builtin("list") {
                                    TypeContainer::List
                                } else {
                                    TypeContainer::Set
                                };
                                queue_children(
                                    &mut pending,
                                    container,
                                    &class.targs().as_slice()[..class.targs().len().min(1)],
                                    depth,
                                    &mut self.remaining,
                                )
                                .map_err(refusal)?;
                                None
                            } else if class.is_builtin("dict") && class.targs().len() == 2 {
                                queue_children(
                                    &mut pending,
                                    TypeContainer::Dict,
                                    class.targs().as_slice(),
                                    depth,
                                    &mut self.remaining,
                                )
                                .map_err(refusal)?;
                                None
                            } else if class.is_builtin("dict") {
                                Some(InferredType::Dict(None))
                            } else {
                                Some(if class.targs().is_empty() {
                                    InferredType::Named(
                                        class.qname().module_qualified_name().into_boxed_str(),
                                    )
                                } else {
                                    InferredType::Unavailable(
                                        NativePythonTypeConstructor::ClassType,
                                    )
                                })
                            }
                        }
                        Type::Any(_) => Some(InferredType::Any),
                        _ => Some(InferredType::Unavailable(native_constructor(ty))),
                    };
                    if let Some(leaf) = leaf {
                        values.push(leaf);
                    }
                }
            }
        }
        Ok(values.pop().expect("one native type result"))
    }
}

fn native_constructor(ty: &Type) -> NativePythonTypeConstructor {
    match ty {
        Type::Literal(_) => NativePythonTypeConstructor::Literal,
        Type::LiteralString(_) => NativePythonTypeConstructor::LiteralString,
        Type::Callable(_) => NativePythonTypeConstructor::Callable,
        Type::CallableResidual(_) => NativePythonTypeConstructor::CallableResidual,
        Type::Function(_) => NativePythonTypeConstructor::Function,
        Type::BoundMethod(_) => NativePythonTypeConstructor::BoundMethod,
        Type::Overload(_) => NativePythonTypeConstructor::Overload,
        Type::Union(_) => NativePythonTypeConstructor::Union,
        Type::Intersect(_) => NativePythonTypeConstructor::Intersect,
        Type::ClassDef(_) => NativePythonTypeConstructor::ClassDef,
        Type::ClassType(_) => NativePythonTypeConstructor::ClassType,
        Type::TypedDict(_) => NativePythonTypeConstructor::TypedDict,
        Type::PartialTypedDict(_) => NativePythonTypeConstructor::PartialTypedDict,
        Type::ShapedArray(_) => NativePythonTypeConstructor::ShapedArray,
        Type::NNModule(_) => NativePythonTypeConstructor::NNModule,
        Type::Size(_) => NativePythonTypeConstructor::Size,
        Type::Dim(_) => NativePythonTypeConstructor::Dim,
        Type::Tuple(_) => NativePythonTypeConstructor::Tuple,
        Type::Module(_) => NativePythonTypeConstructor::Module,
        Type::Forall(_) => NativePythonTypeConstructor::Forall,
        Type::Var(_) => NativePythonTypeConstructor::Var,
        Type::Quantified(_) => NativePythonTypeConstructor::Quantified,
        Type::QuantifiedValue(_) => NativePythonTypeConstructor::QuantifiedValue,
        Type::ElementOfTypeVarTuple(_) => NativePythonTypeConstructor::ElementOfTypeVarTuple,
        Type::TypeGuard(_) => NativePythonTypeConstructor::TypeGuard,
        Type::TypeIs(_) => NativePythonTypeConstructor::TypeIs,
        Type::Annotated(_, _) => NativePythonTypeConstructor::Annotated,
        Type::Unpack(_) => NativePythonTypeConstructor::Unpack,
        Type::TypeVar(_) => NativePythonTypeConstructor::TypeVar,
        Type::ParamSpec(_) => NativePythonTypeConstructor::ParamSpec,
        Type::TypeVarTuple(_) => NativePythonTypeConstructor::TypeVarTuple,
        Type::SpecialForm(_) => NativePythonTypeConstructor::SpecialForm,
        Type::Concatenate(_, _) => NativePythonTypeConstructor::Concatenate,
        Type::ParamSpecValue(_) => NativePythonTypeConstructor::ParamSpecValue,
        Type::Args(_) => NativePythonTypeConstructor::Args,
        Type::Kwargs(_) => NativePythonTypeConstructor::Kwargs,
        Type::ArgsValue(_) => NativePythonTypeConstructor::ArgsValue,
        Type::KwargsValue(_) => NativePythonTypeConstructor::KwargsValue,
        Type::Type(_) => NativePythonTypeConstructor::Type,
        Type::TypeForm(_) => NativePythonTypeConstructor::TypeForm,
        Type::Ellipsis => NativePythonTypeConstructor::Ellipsis,
        Type::Any(_) => NativePythonTypeConstructor::Any,
        Type::Never(_) => NativePythonTypeConstructor::Never,
        Type::TypeAlias(_) => NativePythonTypeConstructor::TypeAlias,
        Type::UntypedAlias(_) => NativePythonTypeConstructor::UntypedAlias,
        Type::Sentinel(_) => NativePythonTypeConstructor::Sentinel,
        Type::SuperInstance(_) => NativePythonTypeConstructor::SuperInstance,
        Type::SelfType(_) => NativePythonTypeConstructor::SelfType,
        Type::KwCall(_) => NativePythonTypeConstructor::KwCall,
        Type::Materialization => NativePythonTypeConstructor::Materialization,
        Type::None => NativePythonTypeConstructor::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn written_return_owner_is_the_innermost_function_site() {
        let source =
            "def outer():\n    def inner() -> int:\n        return 1\n    return inner()\n";
        let facts = crate::legacy::extract(
            source.as_bytes(),
            backend_semantic::vocabulary::PythonVersion::Python314,
        )
        .expect("valid nested functions");
        let cancelled = AtomicBool::new(false);
        let sites = annotated_function_returns(
            &facts,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(5),
            },
        )
        .expect("annotation ownership");
        let inner = facts
            .declarations
            .iter()
            .find(|declaration| declaration.name == "inner")
            .expect("inner declaration");
        let outer = facts
            .declarations
            .iter()
            .find(|declaration| declaration.name == "outer")
            .expect("outer declaration");
        assert_eq!(sites.len(), 1);
        assert!(sites.contains(&(inner.name_span.start, inner.name_span.end)));
        assert!(!sites.contains(&(outer.name_span.start, outer.name_span.end)));
    }

    #[test]
    fn scheduled_type_nodes_reserve_work_before_growing_pending_queue() {
        let children = vec![Type::None, Type::None];
        let mut pending = vec![TypeStep::Visit(&children[0], 0)];
        let mut remaining = 2;
        queue_children(
            &mut pending,
            TypeContainer::Tuple,
            &children,
            0,
            &mut remaining,
        )
        .expect("reserve two nodes");
        assert_eq!(remaining, 0);
        let queued = pending.len();
        assert_eq!(
            queue_children(
                &mut pending,
                TypeContainer::Tuple,
                &children,
                1,
                &mut remaining
            ),
            Err(PythonTypeProjectionFault::Work { limit: TYPE_WORK })
        );
        assert_eq!(pending.len(), queued, "rejection precedes queue allocation");
    }

    #[test]
    fn unsupported_constructor_is_distinct_from_native_any() {
        let cancelled = AtomicBool::new(false);
        let control = PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let site = Span { start: 0, end: 1 };
        let mut projection = TypeProjection::new(control);
        assert_eq!(
            projection
                .convert(&Type::Ellipsis, "type.py", site)
                .expect("typed unavailable"),
            InferredType::Unavailable(NativePythonTypeConstructor::Ellipsis)
        );
        assert_eq!(
            projection
                .convert(&Type::any_implicit(), "type.py", site)
                .expect("native Any"),
            InferredType::Any
        );
    }

    #[test]
    fn native_projection_refuses_depth_and_shared_work_with_typed_operands() {
        let cancelled = AtomicBool::new(false);
        let control = PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let site = Span { start: 2, end: 7 };
        let mut deep = Type::None;
        for _ in 0..TYPE_DEPTH + 2 {
            deep = Type::Tuple(Tuple::Concrete(vec![deep]));
        }
        assert!(matches!(
            TypeProjection::new(control).convert(&deep, "deep.py", site),
            Err(CheckerError::NativeTypeProjection {
                cause: PythonTypeProjectionFault::Depth {
                    limit: TYPE_DEPTH,
                    ..
                },
                ..
            })
        ));
        let mut projection = TypeProjection::new(control);
        projection.remaining = 1;
        assert_eq!(
            projection
                .convert(&Type::None, "plain.py", site)
                .expect("last node"),
            InferredType::NoneType
        );
        assert!(matches!(
            projection.convert(&Type::None, "plain.py", site),
            Err(CheckerError::NativeTypeProjection {
                cause: PythonTypeProjectionFault::Work { limit: TYPE_WORK },
                ..
            })
        ));
    }

    #[test]
    fn native_projection_checkpoints_and_preserves_tuple_order() {
        let cancelled = AtomicBool::new(false);
        let control = PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let site = Span { start: 0, end: 1 };
        let ty = Type::Tuple(Tuple::Concrete(vec![
            Type::None,
            Type::Tuple(Tuple::Concrete(vec![Type::None])),
        ]));
        assert_eq!(
            TypeProjection::new(control)
                .convert(&ty, "tuple.py", site)
                .expect("bounded type"),
            InferredType::Tuple(
                vec![
                    InferredType::NoneType,
                    InferredType::Tuple(vec![InferredType::NoneType].into_boxed_slice())
                ]
                .into_boxed_slice()
            )
        );
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            TypeProjection::new(control).convert(&ty, "tuple.py", site),
            Err(CheckerError::Cancelled { .. })
        ));
    }
}
