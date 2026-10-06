//! Exact typed answers and definitions from one committed Pyrefly 1.2 State.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
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

use super::project::{
    DefinitionTarget, PythonProjectControl, PythonProjectSource, PythonTypeProjectionFault,
    checkpoint, project_error,
};
use super::{
    CheckerError, CheckerReport, Inference, InferenceSite, InferredType, SymbolOutcome,
    SymbolResolution,
};
use crate::legacy::{AnnotationPosition, DeclarationKind, ModuleFacts, Span};

pub(super) fn analyze(
    mirror: &Path,
    original_root: &Path,
    package: &str,
    sources: &[PythonProjectSource<'_>],
    syntax: &BTreeMap<&str, ModuleFacts>,
    profile: backend_semantic::vocabulary::PythonVersion,
    control: PythonProjectControl<'_>,
) -> Result<(BTreeMap<Box<str>, CheckerReport>, [u8; 32]), CheckerError> {
    checkpoint(control)?;
    let minor = super::profile_tag(profile)
        .split('.')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| project_error("", "invalid selected native Python version"))?;
    let (finder, configuration_fingerprint) = captured_finder(
        mirror,
        original_root,
        sources,
        NativeVersion::new(3, minor, 0),
        control,
    )?;
    let handles = sources
        .iter()
        .map(|source| {
            let path = ModulePath::filesystem(mirror.join(source.relative_path));
            let config =
                finder.python_file(ModuleNameWithKind::guaranteed(ModuleName::unknown()), &path);
            config.handle_from_module_path(path)
        })
        .collect::<Vec<_>>();
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
    let selected = sources
        .iter()
        .map(|source| (source.relative_path, source.source))
        .collect::<BTreeMap<_, _>>();
    let mut modules = BTreeMap::new();
    let mut projection = TypeProjection::new(control);
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
        let facts = &syntax[source.relative_path];
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
                if !facts.annotations.iter().any(|annotation| {
                    annotation.position == AnnotationPosition::Return
                        && facts
                            .declarations
                            .iter()
                            .filter(|candidate| {
                                candidate.kind == DeclarationKind::Function
                                    && candidate.span.start <= annotation.span.start
                                    && annotation.span.end <= candidate.span.end
                            })
                            .min_by_key(|candidate| candidate.span.end - candidate.span.start)
                            .is_some_and(|owner| owner.name_span == declaration.name_span)
                }) {
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
            let expression_range = read.get_ast(handle).and_then(|ast| {
                Ast::locate_node(&ast, TextSize::new(position))
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
                let Some(target_source) = selected.get(path) else {
                    continue;
                };
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
                let matches = syntax[path]
                    .declarations
                    .iter()
                    .filter(|target| target.name_span == span)
                    .collect::<Vec<_>>();
                let [target] = matches.as_slice() else {
                    continue;
                };
                let mut enclosing = syntax[path]
                    .declarations
                    .iter()
                    .filter(|candidate| {
                        matches!(
                            candidate.kind,
                            DeclarationKind::Class | DeclarationKind::Function
                        ) && candidate.name_span != target.name_span
                            && candidate.span.start <= target.span.start
                            && target.span.end <= candidate.span.end
                    })
                    .collect::<Vec<_>>();
                enclosing.sort_by_key(|candidate| {
                    (candidate.span.start, std::cmp::Reverse(candidate.span.end))
                });
                let mut scopes = enclosing
                    .iter()
                    .map(|candidate| candidate.name.as_str())
                    .collect::<Vec<_>>();
                scopes.push(target.name.as_str());
                let target = DefinitionTarget {
                    relative_path: path.into(),
                    package: package.into(),
                    name_span: span,
                    qualified_name: if is_binding {
                        class_name.clone().expect("native ClassDef name")
                    } else {
                        format!("{}.{}", definition.module.name(), scopes.join("."))
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
        modules.insert(
            source.relative_path.into(),
            CheckerReport {
                inferences: inferences.into_boxed_slice(),
                imports: Box::new([]),
                symbols: symbols.into_boxed_slice(),
            },
        );
    }
    checkpoint(control)?;
    Ok((modules, configuration_fingerprint))
}

/// No ancestor discovery, interpreter/site-package probing, or external loader
/// roots are admitted. All filesystem module searches remain in the finite
/// private tree; standard/third-party bundled stubs are immutable producer data.
fn captured_finder(
    mirror: &Path,
    original_root: &Path,
    sources: &[PythonProjectSource<'_>],
    version: NativeVersion,
    control: PythonProjectControl<'_>,
) -> Result<(ConfigFinder, [u8; 32]), CheckerError> {
    fn rebase(path: &mut PathBuf, mirror: &Path, original: &Path) -> Result<(), CheckerError> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(CheckerError::UncapturedDependency { path: path.clone() });
        }
        if path.starts_with(mirror) {
            return Ok(());
        }
        if let Ok(relative) = path.strip_prefix(original) {
            *path = mirror.join(relative);
            return Ok(());
        }
        Err(CheckerError::UncapturedDependency { path: path.clone() })
    }
    fn configure(
        mut config: ConfigFile,
        mirror: &Path,
        original: &Path,
        version: NativeVersion,
    ) -> Result<ArcId<ConfigFile>, CheckerError> {
        if let Some(path) = &config.typeshed_path {
            return Err(CheckerError::UncapturedDependency { path: path.clone() });
        }
        if let Some(path) = &config.baseline {
            return Err(CheckerError::UncapturedDependency { path: path.clone() });
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
            .chain(config.import_root.iter_mut())
        {
            rebase(path, mirror, original)?;
        }
        // Explicit site paths may only refer to captured mirror members. An
        // empty explicit list prevents native typings/ interpreter discovery.
        if let Some(paths) = &mut config.python_environment.site_package_path {
            for path in paths {
                rebase(path, mirror, original)?;
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
        config.preset = Some(Preset::Off);
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
        mirror,
        original_root,
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
                return Err(project_error(
                    &path.to_string_lossy(),
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
            let priority = match config.source {
                ConfigSource::File(_) | ConfigSource::FailedParse(_) => 0,
                ConfigSource::PythonToolMarker(_) => 1,
                _ => 2,
            };
            loaded.insert(
                path,
                (priority, configure(config, mirror, original_root, version)?),
            );
        }
    }
    let mut effective = BTreeMap::new();
    let mut scope_identity = blake3::Hasher::new();
    scope_identity.update(b"compiler.python.effective-config-scope.v1\0");
    scope_identity.update(b"root-isolated;native-priority;checked-unannotated;checked-returns;no-interpreter;no-fallback;no-ignore;no-index;classdef+ctor;depth=64;work=262144\0");
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
    let before_root = mirror.to_path_buf();
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
    visited: usize,
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
) {
    pending.push(TypeStep::Build(container, children.len()));
    pending.extend(
        children
            .iter()
            .rev()
            .map(|child| TypeStep::Visit(child, depth + 1)),
    );
}

impl<'control> TypeProjection<'control> {
    fn new(control: PythonProjectControl<'control>) -> Self {
        Self {
            control,
            visited: 0,
        }
    }

    fn convert(&mut self, ty: &Type, path: &str, site: Span) -> Result<InferredType, CheckerError> {
        let refusal = |cause| CheckerError::NativeTypeProjection {
            path: path.into(),
            site,
            cause,
        };
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
                        TypeContainer::Union => InferredType::Union(children),
                        TypeContainer::Tuple => InferredType::Tuple(children),
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
                    if self.visited == TYPE_WORK {
                        return Err(refusal(PythonTypeProjectionFault::Work {
                            limit: TYPE_WORK,
                        }));
                    }
                    self.visited += 1;
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
                            Lit::Enum(_) => InferredType::Any,
                        }),
                        Type::LiteralString(_) => Some(InferredType::Str),
                        Type::Union(union) => {
                            queue_children(
                                &mut pending,
                                TypeContainer::Union,
                                &union.members,
                                depth,
                            );
                            None
                        }
                        Type::Tuple(Tuple::Concrete(elements)) => {
                            queue_children(&mut pending, TypeContainer::Tuple, elements, depth);
                            None
                        }
                        Type::Annotated(inner, _) | Type::Unpack(inner) => {
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
                                );
                                None
                            } else if class.is_builtin("dict") && class.targs().len() == 2 {
                                queue_children(
                                    &mut pending,
                                    TypeContainer::Dict,
                                    class.targs().as_slice(),
                                    depth,
                                );
                                None
                            } else if class.is_builtin("dict") {
                                Some(InferredType::Dict(None))
                            } else {
                                Some(InferredType::Named(
                                    class.qname().module_qualified_name().into_boxed_str(),
                                ))
                            }
                        }
                        _ => Some(InferredType::Any),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

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
        projection.visited = TYPE_WORK - 1;
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
            deadline: std::time::Instant::now() + Duration::from_secs(5),
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
            InferredType::Tuple(vec![
                InferredType::NoneType,
                InferredType::Tuple(vec![InferredType::NoneType])
            ])
        );
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            TypeProjection::new(control).convert(&ty, "tuple.py", site),
            Err(CheckerError::Cancelled { .. })
        ));
    }
}
