//! Typed source-to-document row projection.

use super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, product_source_file_key};
use std::collections::{BTreeMap, BTreeSet};

fn declaration_symbol(
    coordinate: &str,
    kind: DeclarationKind,
    signature: &str,
    occurrences: &mut BTreeMap<RowId, u32>,
) -> RowId {
    let canonical = RowId::Symbol(backend_engine::symbol_key(coordinate));
    let occurrence = occurrences.entry(canonical).or_default();
    let symbol = if *occurrence == 0 {
        canonical
    } else {
        RowId::Symbol(backend_engine::symbol_key(&format!(
            "{coordinate}\0{}\0{signature}\0{occurrence}",
            kind.name()
        )))
    };
    *occurrence = occurrence.saturating_add(1);
    symbol
}

fn indexed_row_capacity(sources: &IndexedSources) -> Result<usize, BuiltinModelError> {
    let count = sources
        .files
        .iter()
        .try_fold(sources.projects.len(), |count, (_, record)| {
            let declarations = record
                .file_fields()
                .map_or(0, |fields| fields.declarations.len());
            count.checked_add(declarations)
        })
        .and_then(|count| count.checked_add(sources.facts.len()))
        .ok_or_else(|| BuiltinModelError("workspace view row count overflow".to_owned()))?;
    if count > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace source declarations exceed the rebuild row bound".to_owned(),
        ));
    }
    Ok(count)
}

pub(super) fn rows_for_indexed_sources(
    initial: &ViewRoot,
    sources: IndexedSources,
) -> Result<Vec<Row>, BuiltinModelError> {
    let mut rows = Vec::with_capacity(indexed_row_capacity(&sources)?);
    let mut selected_files = BTreeSet::new();
    let mut selected_facts = BTreeSet::new();
    let mut symbol_occurrences = BTreeMap::new();
    for (project_key, project) in &sources.projects {
        rows.push(Row::new(
            RowId::Package(project.package),
            initial.basis(),
            &project.label,
        ));
        for key in project.files.iter().copied() {
            if !selected_files.insert((*project_key, key)) {
                return Err(BuiltinModelError(
                    "project file frontier contains an overlapping file key".to_owned(),
                ));
            }
        }
        for key in project.facts.iter().copied() {
            if !selected_facts.insert((*project_key, key)) {
                return Err(BuiltinModelError(
                    "project fact frontier contains an overlapping fact key".to_owned(),
                ));
            }
        }
    }
    let mut dependency_sources = BTreeMap::new();
    let mut graph_facts = Vec::new();
    for (fact_key, record) in sources.facts {
        match record {
            backend_engine::ProductSourceRecord::DependencySource {
                project,
                coordinate,
            } => {
                if backend_engine::product_dependency_source_key(project, &coordinate) != fact_key
                    || !sources.projects.contains_key(&project)
                    || !selected_facts.remove(&(project, fact_key))
                    || dependency_sources
                        .insert(fact_key, (project, coordinate))
                        .is_some()
                {
                    return Err(BuiltinModelError(
                        "dependency source is missing its owner or canonical identity".to_owned(),
                    ));
                }
            }
            record => graph_facts.push((fact_key, record)),
        }
    }
    for (fact_key, record) in graph_facts {
        let (project_key, row) = match record {
            backend_engine::ProductSourceRecord::Package {
                project,
                manifest_path,
                name,
                version,
                description,
                license,
                repository,
            } => {
                let owner = sources.projects.get(&project).ok_or_else(|| {
                    BuiltinModelError("package fact refers to a missing project".to_owned())
                })?;
                if backend_engine::product_package_key(project, &manifest_path) != fact_key {
                    return Err(BuiltinModelError(
                        "package fact does not match its canonical identity".to_owned(),
                    ));
                }
                let coordinate = format!("{}::package::{manifest_path}", owner.label);
                let mut document = vec![Fragment::Text(format!("{name} {version}"))];
                if !description.is_empty() {
                    document.push(Fragment::Text(description.to_string()));
                }
                if !license.is_empty() {
                    document.push(Fragment::Text(format!("License: {license}")));
                }
                if !repository.is_empty() {
                    document.push(Fragment::Text(format!("Repository: {repository}")));
                }
                (
                    project,
                    Some(
                        Row::in_package(
                            RowId::Symbol(backend_engine::symbol_key(&coordinate)),
                            initial.basis(),
                            owner.package,
                            name.to_string(),
                        )
                        .with_signature(format!("package {version}"))
                        .with_document(document),
                    ),
                )
            }
            backend_engine::ProductSourceRecord::Dependency {
                project,
                package,
                alias,
                name,
                requirement,
                kind,
                target,
                source,
                optional,
                default_features,
                features,
            } => {
                let owner = sources.projects.get(&project).ok_or_else(|| {
                    BuiltinModelError("dependency edge refers to a missing project".to_owned())
                })?;
                if backend_engine::product_dependency_key(project, package, &alias, kind, &target)
                    != fact_key
                {
                    return Err(BuiltinModelError(
                        "dependency edge does not match its canonical identity".to_owned(),
                    ));
                }
                let coordinate = format!("{}::dependency::{alias}", owner.label);
                let mut details = vec![format!("{name} {requirement}")];
                if let Some(source) = source {
                    let (source_project, coordinate) =
                        dependency_sources.get(&source).ok_or_else(|| {
                            BuiltinModelError(
                                "dependency edge refers to a missing source fact".to_owned(),
                            )
                        })?;
                    if *source_project != project {
                        return Err(BuiltinModelError(
                            "dependency source belongs to a different project".to_owned(),
                        ));
                    }
                    details.push(coordinate.to_string());
                }
                if !target.is_empty() {
                    details.push(target.to_string());
                }
                if optional {
                    details.push("optional".to_owned());
                }
                if !default_features {
                    details.push("default-features = false".to_owned());
                }
                if !features.is_empty() {
                    details.push(format!("features: {}", features.join(", ")));
                }
                (
                    project,
                    Some(
                        Row::in_package(
                            RowId::Symbol(backend_engine::symbol_key(&coordinate)),
                            initial.basis(),
                            owner.package,
                            alias.to_string(),
                        )
                        .with_signature(format!("dependency {}", kind_label(kind)))
                        .with_document(vec![Fragment::Text(details.join(" · "))]),
                    ),
                )
            }
            backend_engine::ProductSourceRecord::DependencySource { .. } => unreachable!(),
            backend_engine::ProductSourceRecord::Project { .. }
            | backend_engine::ProductSourceRecord::File { .. } => {
                return Err(BuiltinModelError(
                    "expected a package graph fact".to_owned(),
                ));
            }
        };
        if !selected_facts.remove(&(project_key, fact_key)) {
            return Err(BuiltinModelError(
                "package graph fact is outside its project's canonical frontier".to_owned(),
            ));
        }
        if let Some(row) = row {
            rows.push(row);
        }
    }
    for (file_key, record) in sources.files {
        let file = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("expected a source file record".to_owned()))?;
        let project_key = file.project;
        let path = file.path;
        let language = file.language;
        let declarations = file.declarations;
        let project = sources.projects.get(&project_key).ok_or_else(|| {
            BuiltinModelError("source file refers to a missing project record".to_owned())
        })?;
        if product_source_file_key(project_key, path) != file_key
            || project.files.binary_search(&file_key).is_err()
            || !selected_files.remove(&(project_key, file_key))
        {
            return Err(BuiltinModelError(
                "source file is outside its project's canonical frontier".to_owned(),
            ));
        }
        let module_coordinate = format!("{}::{path}", project.label);
        let module = backend_engine::symbol_key(&module_coordinate);
        for declaration in declarations.iter() {
            let coordinate = if declaration.kind() == DeclarationKind::Module {
                module_coordinate.clone()
            } else {
                format!(
                    "{}::{path}:{}::{}",
                    project.label,
                    declaration.line(),
                    declaration.name()
                )
            };
            let symbol = declaration_symbol(
                &coordinate,
                declaration.kind(),
                declaration.signature(),
                &mut symbol_occurrences,
            );
            let mut document = Vec::with_capacity(1);
            if declaration.kind() == DeclarationKind::Module {
                document.push(Fragment::Text(format!(
                    "{} source · {path}",
                    language.name()
                )));
            } else {
                let prose = if declaration.documentation().is_empty() {
                    format!(
                        "{} in {path}:{}",
                        declaration.kind_name(),
                        declaration.line()
                    )
                } else {
                    declaration.documentation().to_owned()
                };
                document.push(Fragment::Text(prose));
            }
            let mut row = Row::in_package(symbol, initial.basis(), project.package, coordinate)
                .with_document(document)
                .with_signature(declaration.signature())
                .with_kind(declaration.kind())
                .with_source(declaration.location().clone());
            if symbol != RowId::Symbol(module) {
                row = row.with_parent(module);
            }
            rows.push(row);
        }
    }
    if !selected_files.is_empty() {
        return Err(BuiltinModelError(
            "project frontier refers to a missing source file".to_owned(),
        ));
    }
    if !selected_facts.is_empty() {
        return Err(BuiltinModelError(
            "project frontier refers to a missing package graph fact".to_owned(),
        ));
    }
    Ok(rows)
}

const fn kind_label(kind: backend_engine::ProductDependencyKind) -> &'static str {
    match kind {
        backend_engine::ProductDependencyKind::Normal => "normal",
        backend_engine::ProductDependencyKind::Build => "build",
        backend_engine::ProductDependencyKind::Development => "dev",
    }
}
