//! One bounded static Python packaging extraction path for local and forge sources.

use crate::python_requirement::{PythonRequirementParseError, parse_python_requirement};
use backend_frontend_python::metadata::{PackagingLiteral, PackagingSyntax, packaging_literals};
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageGraphSourceAuthority,
    PackageReference, ProductText, PythonDependencyDeclaration, PythonMetadataEvidence,
    PythonMetadataFact, PythonProjectMetadata, RegistryEcosystem, admit_dependency_rows,
    collapse_dependency_rows,
};
use std::collections::BTreeMap;

/// Packaging documents are bounded before parsing, including attribute sources.
pub const MAX_PYTHON_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// True for any supported Python packaging manifest at a project root.
#[must_use]
pub fn is_python_manifest(path: &str) -> bool {
    matches!(path, "pyproject.toml" | "setup.cfg" | "setup.py")
}

/// Extracts static package declarations from an admitted relative-path reader.
///
/// The reader must enforce its source boundary (including symlink containment).
/// This adapter never reads absolute or parent-relative paths, imports Python,
/// executes setup.py, or substitutes a commit for a dynamic package version.
///
/// # Errors
/// Returns bounded document/grammar/read errors. Absence is `Ok(None)`;
/// advertised dynamic fields retain their own typed evidence.
pub fn extract_python_project(
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<Option<PythonProjectMetadata>, String> {
    let mut documents = BTreeMap::new();
    for path in ["pyproject.toml", "setup.cfg", "setup.py"] {
        if let Some(bytes) = bounded_read(&mut read, path)? {
            documents.insert(path, bytes);
        }
    }
    let pyproject = documents
        .get("pyproject.toml")
        .map(|bytes| {
            toml::from_str::<toml::Value>(text(bytes)?)
                .map_err(|error| format!("parse pyproject.toml: {error}"))
        })
        .transpose()?;
    if let (Some(bytes), Some(root)) = (documents.get("pyproject.toml"), pyproject.as_ref()) {
        if let Some(project) = root.get("project") {
            let project = project
                .as_table()
                .ok_or("pyproject.toml project is not a table")?;
            let evidence = evidence("pyproject.toml", bytes);
            let mut metadata = baseline("pyproject.toml", evidence.clone());
            metadata.name = toml_string(project, "name", &evidence)?;
            metadata.version = toml_string(project, "version", &evidence)?;
            metadata.description = toml_string(project, "description", &evidence)?;
            metadata.requires_python = toml_string(project, "requires-python", &evidence)?;
            if let Some(urls) = project.get("urls") {
                let urls = urls
                    .as_table()
                    .ok_or("pyproject.toml project.urls is not a table")?;
                for (key, value) in urls {
                    let value = value
                        .as_str()
                        .ok_or("pyproject.toml project URL is not a string")?;
                    set_url(&mut metadata, key, value, &evidence);
                }
            }
            let mut declarations = Vec::new();
            if let Some(values) = project.get("dependencies") {
                toml_requirements(values, DependencyScope::Runtime, None, &mut declarations)?;
            }
            if let Some(extras) = project.get("optional-dependencies") {
                let extras = extras
                    .as_table()
                    .ok_or("pyproject.toml optional-dependencies is not a table")?;
                for (group, values) in extras {
                    toml_requirements(
                        values,
                        DependencyScope::Optional,
                        Some(group),
                        &mut declarations,
                    )?;
                }
            }
            declarations.extend(pyproject_build_requirements(root)?);
            metadata.dependencies = recorded(declarations, &evidence);
            if let Some(dynamic) = project.get("dynamic") {
                let dynamic = dynamic
                    .as_array()
                    .ok_or("pyproject.toml project.dynamic is not an array")?;
                for value in dynamic {
                    let field = value
                        .as_str()
                        .ok_or("pyproject.toml dynamic field is not a string")?;
                    match field {
                        "version" => {
                            if project.contains_key("version") {
                                return Err(
                                    "pyproject.toml version is both static and dynamic".to_owned()
                                );
                            }
                            metadata.version =
                                dynamic_fact("pyproject.toml dynamic version", &evidence);
                            if let Some(attr) = root
                                .get("tool")
                                .and_then(|v| v.get("setuptools"))
                                .and_then(|v| v.get("dynamic"))
                                .and_then(|v| v.get("version"))
                                .and_then(|v| v.get("attr"))
                                .and_then(toml::Value::as_str)
                            {
                                resolve_attr(&mut metadata, attr, &evidence, &mut read)?;
                            }
                        }
                        "dependencies" | "optional-dependencies" => {
                            if project.contains_key(field) {
                                return Err(format!(
                                    "pyproject.toml {field} is both static and dynamic"
                                ));
                            }
                            mark_incomplete_dependencies(
                                &mut metadata,
                                "pyproject.toml advertises dynamic dependencies",
                                &evidence,
                            );
                        }
                        "description" => {
                            metadata.description = dynamic_fact("dynamic description", &evidence)
                        }
                        "requires-python" => {
                            metadata.requires_python =
                                dynamic_fact("dynamic interpreter requirement", &evidence)
                        }
                        "urls" => {
                            metadata.homepage = dynamic_fact("dynamic URLs", &evidence);
                            metadata.documentation = dynamic_fact("dynamic URLs", &evidence);
                            metadata.repository = dynamic_fact("dynamic URLs", &evidence);
                        }
                        _ => {}
                    }
                }
            }
            metadata.admit().map_err(|error| error.to_string())?;
            return Ok(Some(metadata));
        }
    }
    let supplementary_build = match (documents.get("pyproject.toml"), pyproject.as_ref()) {
        (Some(bytes), Some(root)) if root.get("build-system").is_some() => Some((
            evidence("pyproject.toml", bytes),
            pyproject_build_requirements(root)?,
        )),
        _ => None,
    };
    if let Some(bytes) = documents.get("setup.cfg") {
        let options = ini_options(text(bytes)?)?;
        if options.keys().any(|(section, _)| {
            section == "metadata" || section == "options" || section.starts_with("options.")
        }) {
            let evidence = evidence("setup.cfg", bytes);
            let mut metadata = baseline("setup.cfg", evidence.clone());
            metadata.name = ini_string(&options, "metadata", "name", &evidence);
            metadata.version = ini_string(&options, "metadata", "version", &evidence);
            metadata.description = ini_string(&options, "metadata", "description", &evidence);
            metadata.homepage = ini_string(&options, "metadata", "url", &evidence);
            metadata.requires_python =
                ini_string(&options, "options", "python_requires", &evidence);
            if let Some(version) = options.get(&("metadata".to_owned(), "version".to_owned())) {
                if let Some(attr) = version.strip_prefix("attr:") {
                    resolve_attr(&mut metadata, attr.trim(), &evidence, &mut read)?;
                }
            }
            if let Some(urls) = options.get(&("metadata".to_owned(), "project_urls".to_owned())) {
                for line in urls.lines().filter(|line| !line.trim().is_empty()) {
                    let (key, value) = line
                        .split_once('=')
                        .ok_or("setup.cfg project URL omits '='")?;
                    set_url(&mut metadata, key.trim(), value.trim(), &evidence);
                }
            }
            let mut declarations = Vec::new();
            let mut dynamic_dependencies = false;
            for ((section, key), value) in &options {
                let scope_and_group = match (section.as_str(), key.as_str()) {
                    ("options", "install_requires") => Some((DependencyScope::Runtime, None)),
                    ("options", "setup_requires") => Some((DependencyScope::Build, None)),
                    ("options", "tests_require") => Some((DependencyScope::Development, None)),
                    ("options.extras_require", group) => {
                        Some((DependencyScope::Optional, Some(group)))
                    }
                    ("options", "extras_require") => {
                        dynamic_dependencies = true;
                        None
                    }
                    _ => None,
                };
                if let Some((scope, group)) = scope_and_group {
                    if is_dynamic_value(value) {
                        dynamic_dependencies = true;
                    } else {
                        for requirement in value
                            .lines()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                        {
                            declarations.push(declaration(requirement, scope, group)?);
                        }
                    }
                }
            }
            // Missing options is not proof that setup.py adds no dependencies.
            metadata.dependencies = if dynamic_dependencies {
                partial_dependencies(
                    declarations,
                    "setup.cfg contains dynamic dependency declarations",
                    &evidence,
                )
            } else if options.contains_key(&("options".to_owned(), "install_requires".to_owned())) {
                recorded(declarations, &evidence)
            } else if !declarations.is_empty() {
                partial_dependencies(
                    declarations,
                    "setup.cfg declares extra/build/test dependencies but omits runtime dependencies",
                    &evidence,
                )
            } else {
                PythonMetadataFact::Omitted {
                    evidence: vec![evidence.clone()],
                }
            };
            if let Some(setup_bytes) = documents.get("setup.py") {
                let setup_evidence = self::evidence("setup.py", setup_bytes);
                metadata.evidence.push(setup_evidence.clone());
                match packaging_literals(setup_bytes).map_err(str::to_owned)? {
                    PackagingSyntax::Dynamic => {
                        mark_dynamic_metadata(
                            &mut metadata,
                            "setup.py can override setup.cfg metadata",
                            &setup_evidence,
                        );
                        mark_incomplete_dependencies(
                            &mut metadata,
                            "setup.py can override setup.cfg dependencies",
                            &setup_evidence,
                        );
                    }
                    PackagingSyntax::Literal {
                        setup: Some(fields),
                        ..
                    } => {
                        for (key, field) in [
                            ("name", &mut metadata.name),
                            ("version", &mut metadata.version),
                            ("description", &mut metadata.description),
                            ("url", &mut metadata.homepage),
                            ("python_requires", &mut metadata.requires_python),
                        ] {
                            if fields.contains_key(key) {
                                *field = setup_string(&fields, key, &setup_evidence)?;
                            }
                        }
                        if [
                            "install_requires",
                            "extras_require",
                            "setup_requires",
                            "tests_require",
                        ]
                        .iter()
                        .any(|key| fields.contains_key(*key))
                        {
                            mark_incomplete_dependencies(
                                &mut metadata,
                                "setup.py overrides setup.cfg dependency declarations; combined dependency projection is unavailable",
                                &setup_evidence,
                            );
                        }
                    }
                    PackagingSyntax::Literal { setup: None, .. } => {
                        mark_dynamic_metadata(
                            &mut metadata,
                            "setup.py has no static setuptools setup call",
                            &setup_evidence,
                        );
                        mark_incomplete_dependencies(
                            &mut metadata,
                            "setup.py has no static setuptools setup call",
                            &setup_evidence,
                        );
                    }
                }
            }
            append_build_observations(&mut metadata, supplementary_build.as_ref());
            metadata.admit().map_err(|error| error.to_string())?;
            return Ok(Some(metadata));
        }
    }
    if let Some(bytes) = documents.get("setup.py") {
        let evidence = evidence("setup.py", bytes);
        let mut metadata = baseline("setup.py", evidence.clone());
        match packaging_literals(bytes).map_err(str::to_owned)? {
            PackagingSyntax::Dynamic => {
                mark_dynamic_metadata(
                    &mut metadata,
                    "setup.py requires executable metadata",
                    &evidence,
                );
                metadata.dependencies =
                    dynamic_fact("setup.py requires executable metadata", &evidence);
            }
            PackagingSyntax::Literal {
                setup: Some(fields),
                ..
            } => {
                metadata.name = setup_string(&fields, "name", &evidence)?;
                metadata.version = setup_string(&fields, "version", &evidence)?;
                metadata.description = setup_string(&fields, "description", &evidence)?;
                metadata.homepage = setup_string(&fields, "url", &evidence)?;
                metadata.requires_python = setup_string(&fields, "python_requires", &evidence)?;
                if let Some(urls) = fields.get("project_urls") {
                    let PackagingLiteral::Mapping(urls) = urls else {
                        return Err("setup.py project_urls is not a literal mapping".to_owned());
                    };
                    for (key, value) in urls {
                        let PackagingLiteral::String(value) = value else {
                            return Err("setup.py project URL is not a string".to_owned());
                        };
                        set_url(&mut metadata, key, value, &evidence);
                    }
                }
                let mut declarations = Vec::new();
                for (key, scope) in [
                    ("install_requires", DependencyScope::Runtime),
                    ("setup_requires", DependencyScope::Build),
                    ("tests_require", DependencyScope::Development),
                ] {
                    if let Some(values) = fields.get(key) {
                        literal_requirements(values, scope, None, &mut declarations)?;
                    }
                }
                if let Some(extras) = fields.get("extras_require") {
                    let PackagingLiteral::Mapping(extras) = extras else {
                        return Err("setup.py extras_require is not a literal mapping".to_owned());
                    };
                    for (group, values) in extras {
                        literal_requirements(
                            values,
                            DependencyScope::Optional,
                            Some(group),
                            &mut declarations,
                        )?;
                    }
                }
                metadata.dependencies = recorded(declarations, &evidence);
            }
            PackagingSyntax::Literal { setup: None, .. } => {}
        }
        append_build_observations(&mut metadata, supplementary_build.as_ref());
        metadata.admit().map_err(|error| error.to_string())?;
        return Ok(Some(metadata));
    }
    Ok(None)
}

fn pyproject_build_requirements(
    root: &toml::Value,
) -> Result<Vec<PythonDependencyDeclaration>, String> {
    let mut declarations = Vec::new();
    if let Some(build_system) = root.get("build-system") {
        let build_system = build_system
            .as_table()
            .ok_or("pyproject.toml build-system is not a table")?;
        if let Some(requires) = build_system.get("requires") {
            toml_requirements(requires, DependencyScope::Build, None, &mut declarations)?;
        }
    }
    Ok(declarations)
}

fn append_build_observations(
    metadata: &mut PythonProjectMetadata,
    supplementary: Option<&(PythonMetadataEvidence, Vec<PythonDependencyDeclaration>)>,
) {
    let Some((source, declarations)) = supplementary else {
        return;
    };
    metadata.evidence.push(source.clone());
    match &mut metadata.dependencies {
        PythonMetadataFact::Recorded { value, evidence }
        | PythonMetadataFact::Partial {
            value, evidence, ..
        } => {
            value.extend(declarations.iter().cloned());
            evidence.push(source.clone());
        }
        PythonMetadataFact::Dynamic { reason, evidence } if !declarations.is_empty() => {
            let mut combined = evidence.clone();
            combined.push(source.clone());
            metadata.dependencies = PythonMetadataFact::Partial {
                value: declarations.clone(),
                reason: reason.clone(),
                evidence: combined,
            };
        }
        PythonMetadataFact::Omitted { evidence } if !declarations.is_empty() => {
            let mut combined = evidence.clone();
            combined.push(source.clone());
            metadata.dependencies = PythonMetadataFact::Partial {
                value: declarations.clone(),
                reason: "runtime declarations are omitted while pyproject.toml declares build dependencies".to_owned(),
                evidence: combined,
            };
        }
        PythonMetadataFact::Dynamic { evidence, .. } | PythonMetadataFact::Omitted { evidence } => {
            evidence.push(source.clone());
        }
    }
}

fn mark_dynamic_metadata(
    metadata: &mut PythonProjectMetadata,
    reason: &str,
    evidence: &PythonMetadataEvidence,
) {
    for field in [
        &mut metadata.name,
        &mut metadata.version,
        &mut metadata.description,
        &mut metadata.homepage,
        &mut metadata.documentation,
        &mut metadata.repository,
        &mut metadata.requires_python,
    ] {
        *field = dynamic_fact(reason, evidence);
    }
}

fn bounded_read(
    read: &mut impl FnMut(&str) -> Result<Option<Vec<u8>>, String>,
    path: &str,
) -> Result<Option<Vec<u8>>, String> {
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("unsafe Python metadata source path".to_owned());
    }
    let bytes = read(path)?;
    if bytes
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_PYTHON_METADATA_BYTES)
    {
        return Err(format!("Python metadata {path} exceeds bounds"));
    }
    Ok(bytes)
}

fn text(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|_| "Python packaging metadata is not UTF-8".to_owned())
}

fn evidence(path: &str, bytes: &[u8]) -> PythonMetadataEvidence {
    PythonMetadataEvidence {
        path: path.to_owned(),
        digest: *blake3::hash(bytes).as_bytes(),
        bytes: bytes.len() as u64,
    }
}

fn baseline(path: &str, evidence: PythonMetadataEvidence) -> PythonProjectMetadata {
    let omitted = PythonMetadataFact::Omitted {
        evidence: vec![evidence.clone()],
    };
    PythonProjectMetadata {
        manifest_path: path.to_owned(),
        evidence: vec![evidence.clone()],
        name: omitted.clone(),
        version: omitted.clone(),
        description: omitted.clone(),
        homepage: omitted.clone(),
        documentation: omitted.clone(),
        repository: omitted.clone(),
        requires_python: omitted,
        dependencies: PythonMetadataFact::Omitted {
            evidence: vec![evidence],
        },
    }
}

fn mark_incomplete_dependencies(
    metadata: &mut PythonProjectMetadata,
    reason: &str,
    source: &PythonMetadataEvidence,
) {
    let Some(declarations) = metadata
        .dependencies
        .declared()
        .filter(|rows| !rows.is_empty())
    else {
        metadata.dependencies = dynamic_fact(reason, source);
        return;
    };
    metadata.dependencies = PythonMetadataFact::Partial {
        value: declarations.clone(),
        reason: reason.to_owned(),
        // Keep original declaration and overriding-source provenance distinct.
        evidence: metadata.evidence.clone(),
    };
}

fn partial_dependencies(
    value: Vec<PythonDependencyDeclaration>,
    reason: &str,
    evidence: &PythonMetadataEvidence,
) -> PythonMetadataFact<Vec<PythonDependencyDeclaration>> {
    if value.is_empty() {
        dynamic_fact(reason, evidence)
    } else {
        PythonMetadataFact::Partial {
            value,
            reason: reason.to_owned(),
            evidence: vec![evidence.clone()],
        }
    }
}

fn recorded<T>(value: T, evidence: &PythonMetadataEvidence) -> PythonMetadataFact<T> {
    PythonMetadataFact::Recorded {
        value,
        evidence: vec![evidence.clone()],
    }
}

fn dynamic_fact<T>(reason: &str, evidence: &PythonMetadataEvidence) -> PythonMetadataFact<T> {
    PythonMetadataFact::Dynamic {
        reason: reason
            .chars()
            .scan(0, |bytes, character| {
                *bytes += character.len_utf8();
                (*bytes <= 1024).then_some(character)
            })
            .collect(),
        evidence: vec![evidence.clone()],
    }
}

fn string_fact(value: &str, evidence: &PythonMetadataEvidence) -> PythonMetadataFact<String> {
    if is_dynamic_value(value) {
        dynamic_fact(value, evidence)
    } else {
        recorded(value.to_owned(), evidence)
    }
}

fn is_dynamic_value(value: &str) -> bool {
    value.starts_with("attr:")
        || value.starts_with("file:")
        || value.contains("${")
        || value.contains("%(")
}

fn toml_string(
    table: &toml::Table,
    field: &str,
    evidence: &PythonMetadataEvidence,
) -> Result<PythonMetadataFact<String>, String> {
    match table.get(field) {
        Some(value) => value
            .as_str()
            .map(|value| string_fact(value, evidence))
            .ok_or_else(|| format!("pyproject.toml {field} is not a string")),
        None => Ok(PythonMetadataFact::Omitted {
            evidence: vec![evidence.clone()],
        }),
    }
}

fn setup_string(
    fields: &BTreeMap<String, PackagingLiteral>,
    key: &str,
    evidence: &PythonMetadataEvidence,
) -> Result<PythonMetadataFact<String>, String> {
    match fields.get(key) {
        Some(PackagingLiteral::String(value)) => Ok(string_fact(value, evidence)),
        Some(_) => Err(format!("setup.py {key} is not a literal string")),
        None => Ok(PythonMetadataFact::Omitted {
            evidence: vec![evidence.clone()],
        }),
    }
}

type IniOptions = BTreeMap<(String, String), String>;

fn ini_options(input: &str) -> Result<IniOptions, String> {
    let mut options: IniOptions = BTreeMap::new();
    let mut sections = std::collections::BTreeSet::new();
    let mut section = String::new();
    let mut pending: Option<(String, String)> = None;
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let value = trimmed[1..trimmed.len() - 1].trim();
            if value.is_empty() || value.contains(['[', ']']) {
                return Err("invalid setup.cfg section".to_owned());
            }
            if !sections.insert(value.to_owned()) {
                return Err("duplicate setup.cfg section".to_owned());
            }
            // ConfigParser sections are case sensitive; only the recognized
            // setuptools spellings select metadata/options semantics.
            section = value.to_owned();
            pending = None;
            continue;
        }
        if line.starts_with([' ', '\t']) {
            let key = pending
                .as_ref()
                .ok_or("setup.cfg continuation has no option")?;
            let value = options
                .get_mut(key)
                .ok_or("setup.cfg continuation has no value")?;
            if !value.is_empty() {
                value.push('\n');
            }
            value.push_str(trimmed);
            continue;
        }
        if section.is_empty() {
            return Err("setup.cfg option has no section".to_owned());
        }
        let (key, value) = line
            .split_once('=')
            .or_else(|| line.split_once(':'))
            .ok_or("invalid setup.cfg option")?;
        let key = key.trim();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            return Err("invalid setup.cfg key".to_owned());
        }
        let option = if section == "options.extras_require" {
            key.to_owned()
        } else {
            key.replace('-', "_").to_ascii_lowercase()
        };
        let key = (section.clone(), option);
        if options
            .insert(key.clone(), value.trim().to_owned())
            .is_some()
        {
            return Err("duplicate setup.cfg option".to_owned());
        }
        pending = Some(key);
    }
    Ok(options)
}

fn ini_string(
    options: &IniOptions,
    section: &str,
    key: &str,
    evidence: &PythonMetadataEvidence,
) -> PythonMetadataFact<String> {
    options
        .get(&(section.to_owned(), key.to_owned()))
        .map_or_else(
            || PythonMetadataFact::Omitted {
                evidence: vec![evidence.clone()],
            },
            |value| string_fact(value, evidence),
        )
}

fn set_url(
    metadata: &mut PythonProjectMetadata,
    key: &str,
    value: &str,
    evidence: &PythonMetadataEvidence,
) {
    match key.to_ascii_lowercase().as_str() {
        "homepage" | "home" => metadata.homepage = string_fact(value, evidence),
        "documentation" | "docs" => metadata.documentation = string_fact(value, evidence),
        "repository" | "source" | "source code" | "github" => {
            metadata.repository = string_fact(value, evidence)
        }
        _ => {}
    }
}

fn resolve_attr(
    metadata: &mut PythonProjectMetadata,
    attr: &str,
    declaration_evidence: &PythonMetadataEvidence,
    read: &mut impl FnMut(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<(), String> {
    metadata.version = dynamic_fact(
        &format!("unresolved static version attribute: {attr}"),
        declaration_evidence,
    );
    let parts: Vec<_> = attr.split('.').collect();
    if parts.len() < 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.chars().enumerate().all(|(index, c)| {
                    c == '_' || c.is_ascii_alphabetic() || (index > 0 && c.is_ascii_digit())
                })
        })
    {
        return Ok(());
    }
    let binding = parts[parts.len() - 1];
    let module_path = parts[..parts.len() - 1].join("/");
    let candidates = [
        format!("{module_path}.py"),
        format!("{module_path}/__init__.py"),
        format!("src/{module_path}.py"),
        format!("src/{module_path}/__init__.py"),
    ];
    let mut found = Vec::new();
    for path in &candidates {
        if let Some(bytes) = bounded_read(read, path)? {
            let evidence = evidence(path, &bytes);
            metadata.evidence.push(evidence.clone());
            found.push((bytes, evidence));
        }
    }
    if found.len() != 1 {
        return Ok(());
    }
    if let Some((bytes, evidence)) = found.pop() {
        if let Ok(PackagingSyntax::Literal {
            bindings,
            setup: None,
        }) = packaging_literals(&bytes)
        {
            if let Some(PackagingLiteral::String(value)) = bindings.get(binding) {
                if !value.is_empty() && !is_dynamic_value(value) {
                    metadata.version = PythonMetadataFact::Recorded {
                        value: value.clone(),
                        evidence: vec![declaration_evidence.clone(), evidence],
                    };
                }
            }
        }
    }
    Ok(())
}

fn toml_requirements(
    values: &toml::Value,
    scope: DependencyScope,
    group: Option<&str>,
    output: &mut Vec<PythonDependencyDeclaration>,
) -> Result<(), String> {
    let values = values
        .as_array()
        .ok_or("Python dependency declarations are not an array")?;
    for value in values {
        let value = value
            .as_str()
            .ok_or("Python dependency declaration is not a string")?;
        output.push(declaration(value, scope, group)?);
    }
    Ok(())
}

fn literal_requirements(
    values: &PackagingLiteral,
    scope: DependencyScope,
    group: Option<&str>,
    output: &mut Vec<PythonDependencyDeclaration>,
) -> Result<(), String> {
    let PackagingLiteral::Sequence(values) = values else {
        return Err("setup.py dependencies are not a literal sequence".to_owned());
    };
    for value in values {
        let PackagingLiteral::String(value) = value else {
            return Err("setup.py dependency is not a string".to_owned());
        };
        output.push(declaration(value, scope, group)?);
    }
    Ok(())
}

fn declaration(
    value: &str,
    scope: DependencyScope,
    group: Option<&str>,
) -> Result<PythonDependencyDeclaration, String> {
    if value.is_empty() || value.len() > 8192 || value.contains(['\n', '\r']) {
        return Err("invalid Python dependency declaration".to_owned());
    }
    Ok(PythonDependencyDeclaration {
        requirement: value.to_owned(),
        scope,
        group: group.map(str::to_owned),
    })
}

/// Projects original declarations into the shared dependency graph vocabulary.
///
/// Resolution, source authority and capability remain separate from extraction.
#[must_use]
pub fn python_dependency_facts(
    metadata: &PythonProjectMetadata,
    source: &PackageReference,
    authority: PackageGraphSourceAuthority,
    evidence_authority: DependencyAuthority,
    frontier: [u8; 32],
) -> DependencyFacts<Box<[PackageDependencyRecord]>> {
    let Some(declarations) = metadata.dependencies.recorded() else {
        return DependencyFacts::Unknown(ProductText::from_static(
            "Python dependency collection is incomplete, dynamic or not declared; inspect Python metadata evidence",
        ));
    };
    if declarations.len() > backend_library::MAX_PRODUCT_ROWS {
        return DependencyFacts::Unavailable(ProductText::from_static(
            "Python dependency declarations exceed bounds",
        ));
    }
    if declarations.is_empty() {
        return DependencyFacts::Known(Box::default());
    }

    let provenance = python_metadata_digest(metadata);
    let mut rows = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let parsed = match parse_python_requirement(&declaration.requirement) {
            Ok(parsed) => parsed,
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded) => {
                return DependencyFacts::Unknown(ProductText::from_static(
                    "Python dependency graph unavailable: a requirement exceeded the bounded PEP 508 parser complexity budget",
                ));
            }
            Err(_) => {
                // Never publish a partial graph: all rows are committed together
                // only after every complete source declaration parses.
                return DependencyFacts::Unknown(ProductText::from_static(
                    "Python dependency graph unavailable: at least one complete PEP 508 requirement is invalid or has unsupported marker semantics",
                ));
            }
        };
        let Ok(target) = PackageDependencyTarget::new(
            RegistryEcosystem::Pypi,
            parsed.package_name,
            declaration.requirement.clone(),
            None,
        ) else {
            return DependencyFacts::Unknown(ProductText::from_static(
                "Python dependency graph unavailable: a parsed requirement target exceeds graph bounds",
            ));
        };
        let optional = declaration.scope == DependencyScope::Optional || parsed.optional_by_extra;
        rows.push(PackageDependencyRecord::new_with_source_authority(
            source.clone(),
            authority,
            target,
            declaration.scope,
            optional,
            DependencyEvidence {
                authority: evidence_authority,
                frontier,
                provenance,
            },
        ));
    }
    // Different extra groups retain separate declarations in metadata, but
    // can project to the same complete graph fact. Canonicalize at this
    // boundary so repeated declarations cannot poison the entire graph.
    // Distinct requirements, scopes and evidence remain distinct edges.
    match admit_dependency_rows(collapse_dependency_rows(rows)) {
        Ok(rows) => DependencyFacts::Known(rows),
        Err(_) => DependencyFacts::Unavailable(ProductText::from_static(
            "Python dependency graph unavailable: projected declarations failed graph admission",
        )),
    }
}

/// Content identity of every source document consumed by the shared extraction.
#[must_use]
pub fn python_metadata_digest(metadata: &PythonProjectMetadata) -> [u8; 32] {
    metadata.digest()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(files: &[(&str, &[u8])]) -> Result<Option<PythonProjectMetadata>, String> {
        extract_python_project(|path| {
            Ok(files
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, bytes)| bytes.to_vec()))
        })
    }

    #[test]
    fn unchanged_official_httpie_metadata_and_requirements_are_evidenced()
    -> Result<(), &'static str> {
        let setup_cfg = include_bytes!("../tests/fixtures/httpie-5b604c37/setup.cfg");
        let setup_py = include_bytes!("../tests/fixtures/httpie-5b604c37/setup.py");
        let version = include_bytes!("../tests/fixtures/httpie-5b604c37/httpie/__init__.py");
        let metadata = extract(&[
            ("setup.cfg", setup_cfg),
            ("setup.py", setup_py),
            ("httpie/__init__.py", version),
        ])
        .expect("HTTPie packaging")
        .expect("HTTPie metadata");
        assert_eq!(metadata.name.recorded().map(String::as_str), Some("httpie"));
        assert_eq!(
            metadata.version.recorded().map(String::as_str),
            Some("3.2.4")
        );
        assert_eq!(
            metadata.documentation.recorded().map(String::as_str),
            Some("https://httpie.io/docs")
        );
        assert_eq!(
            metadata.repository.recorded().map(String::as_str),
            Some("https://github.com/httpie/cli")
        );
        assert!(
            metadata
                .description
                .recorded()
                .expect("description")
                .contains("command-line HTTP client")
        );
        let PythonMetadataFact::Recorded { evidence, .. } = &metadata.version else {
            panic!("literal source evidence");
        };
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[1].path, "httpie/__init__.py");
        assert_eq!(evidence[1].digest, *blake3::hash(version).as_bytes());
        assert!(
            metadata
                .evidence
                .iter()
                .any(|source| source.path == "setup.py")
        );
        let declarations = metadata
            .dependencies
            .recorded()
            .expect("declared dependencies");
        assert_eq!(
            declarations
                .iter()
                .filter(|dep| dep.scope == DependencyScope::Runtime)
                .count(),
            11
        );
        assert!(
            declarations
                .iter()
                .any(|dep| dep.requirement == "requests[socks] >=2.22.0")
        );
        assert!(
            declarations
                .iter()
                .any(|dep| dep.requirement == "colorama>=0.2.4; sys_platform==\"win32\"")
        );
        assert!(
            declarations
                .iter()
                .any(|dep| dep.group.as_deref() == Some("test"))
        );
        let source = PackageReference::parse("pkg:pypi/httpie@3.2.4").expect("coordinate");
        let facts = python_dependency_facts(
            &metadata,
            &source,
            PackageGraphSourceAuthority::Local([7; 32]),
            DependencyAuthority::LocalManifest,
            [7; 32],
        );
        let rows = match facts {
            DependencyFacts::Known(rows) => rows,
            _ => return Err("all HTTPie requirements are valid PEP 508 declarations"),
        };
        // Five requirements occur in both HTTPie's dev and test extras. The
        // metadata preserves both groups; their identical graph edges occur once.
        assert_eq!(declarations.len(), 32);
        for (group, count) in [("dev", 16), ("test", 5)] {
            assert_eq!(
                declarations
                    .iter()
                    .filter(|row| row.group.as_deref() == Some(group))
                    .count(),
                count
            );
        }
        assert_eq!(rows.len(), 27);
        assert_eq!(
            rows.iter().filter(|row| row.scope == DependencyScope::Optional).count(),
            16
        );
        assert!(
            rows.iter()
                .filter(|row| row.scope == DependencyScope::Optional)
                .all(|row| row.optional)
        );
        assert_eq!(
            admit_dependency_rows(rows.to_vec()).expect("canonical rows"),
            rows
        );
        backend_library::CheckedPackageGraphFacts::new(vec![(
            backend_library::PackageGraphSourceKey::new(
                source,
                PackageGraphSourceAuthority::Local([7; 32]),
            ),
            DependencyFacts::Known(rows.clone()),
        )])
        .expect("actual HTTPie facts must admit into the package graph");
        assert_eq!(
            rows.iter()
                .filter(|row| row.scope == DependencyScope::Runtime)
                .count(),
            11
        );
        let requests = rows
            .iter()
            .find(|row| row.target.requirement.as_str() == "requests[socks] >=2.22.0")
            .ok_or("missing exact HTTPie declaration")?;
        assert_eq!(requests.target.name.as_str(), "requests");
        Ok(())
    }

    #[test]
    fn repeated_python_groups_preserve_metadata_and_distinct_graph_edges() {
        let metadata = extract(&[(
            "setup.cfg",
            b"[metadata]\nname=grouped\nversion=1\n[options]\ninstall_requires=requests>=2\nsetup_requires=requests>=2\n[options.extras_require]\ndev=\n requests>=2\n requests>=3\ntest=\n requests>=2\n requests>=3\n",
        )])
        .expect("static packaging")
        .expect("package metadata");
        let declarations = metadata.dependencies.recorded().expect("all groups");
        assert_eq!(declarations.len(), 6);
        for group in ["dev", "test"] {
            assert_eq!(
                declarations
                    .iter()
                    .filter(|row| row.group.as_deref() == Some(group))
                    .count(),
                2
            );
        }
        let source = PackageReference::parse("pkg:pypi/grouped@1").expect("source");
        let facts = python_dependency_facts(
            &metadata,
            &source,
            PackageGraphSourceAuthority::Local([9; 32]),
            DependencyAuthority::LocalManifest,
            [9; 32],
        );
        let DependencyFacts::Known(rows) = &facts else {
            panic!("complete graph facts");
        };
        assert_eq!(rows.len(), 4);
        for (scope, count) in [
            (DependencyScope::Runtime, 1),
            (DependencyScope::Build, 1),
            (DependencyScope::Optional, 2),
        ] {
            assert_eq!(rows.iter().filter(|row| row.scope == scope).count(), count);
        }
        for requirement in ["requests>=2", "requests>=3"] {
            assert!(
                rows.iter()
                    .any(|row| row.target.requirement.as_str() == requirement)
            );
        }
        backend_library::CheckedPackageGraphFacts::new(vec![(
            backend_library::PackageGraphSourceKey::new(
                source,
                PackageGraphSourceAuthority::Local([9; 32]),
            ),
            facts,
        )])
        .expect("deduplicated graph admits without weakening identity checks");
        assert_eq!(
            metadata.dependencies.recorded().expect("retained metadata").len(),
            6
        );
    }

    #[test]
    fn supporting_pyproject_build_rows_and_evidence_survive_cfg_setup_selection()
    -> Result<(), &'static str> {
        let pyproject = b"[build-system]\nrequires=['wheel>=1']\n";
        for (path, bytes) in [
            ("setup.cfg", b"[metadata]\nname=sample\nversion=1\n[options]\ninstall_requires=requests\n".as_slice()),
            ("setup.py", b"from setuptools import setup\nsetup(name='sample', version='1', install_requires=['requests'])\n".as_slice()),
        ] {
            let metadata = extract(&[("pyproject.toml", pyproject), (path, bytes)])
                .map_err(|_| "static supporting build document must extract")?
                .ok_or("selected cfg/setup metadata")?;
            assert_eq!(metadata.manifest_path, path);
            let PythonMetadataFact::Recorded { value, evidence } = &metadata.dependencies else {
                return Err("complete literal collections must remain recorded");
            };
            assert_eq!(value.len(), 2);
            assert!(value.iter().any(|row| row.scope == DependencyScope::Runtime && row.requirement == "requests"));
            assert!(value.iter().any(|row| row.scope == DependencyScope::Build && row.requirement == "wheel>=1"));
            let witness = metadata.evidence.iter().find(|row| row.path == "pyproject.toml")
                .ok_or("supporting document byte evidence")?;
            assert_eq!(witness.digest, *blake3::hash(pyproject).as_bytes());
            assert_eq!(witness.bytes, pyproject.len() as u64);
            assert!(evidence.contains(witness));
            let changed = extract(&[("pyproject.toml", b"[build-system]\nrequires=['wheel>=2']\n"), (path, bytes)])
                .map_err(|_| "changed supporting document")?
                .ok_or("changed selected metadata")?;
            assert_ne!(metadata.digest(), changed.digest());
        }
        for (path, bytes) in [
            (
                "setup.cfg",
                b"[metadata]\nname=sample\nversion=1\n".as_slice(),
            ),
            (
                "setup.py",
                b"import os\nfrom setuptools import setup\nsetup(name=os.getenv('NAME'))\n"
                    .as_slice(),
            ),
        ] {
            let metadata = extract(&[("pyproject.toml", pyproject), (path, bytes)])
                .map_err(|_| "incomplete collection must retain supporting declarations")?
                .ok_or("incomplete metadata")?;
            let PythonMetadataFact::Partial { value, .. } = &metadata.dependencies else {
                return Err("supporting build rows cannot establish runtime completeness");
            };
            assert_eq!(value.len(), 1);
            assert_eq!(value[0].scope, DependencyScope::Build);
        }
        assert!(
            extract(&[
                ("pyproject.toml", b"[build-system]\nrequires=42\n"),
                ("setup.cfg", b"[metadata]\nname=sample\nversion=1\n")
            ])
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn cfg_rejects_duplicate_sections_and_preserves_section_and_extra_spelling()
    -> Result<(), &'static str> {
        for cfg in [
            "[metadata]\nname=sample\n[metadata]\nversion=1\n",
            "[options]\ninstall_requires=requests\n[options]\npython_requires=>=3\n",
            "[unrecognized]\na=1\n[unrecognized]\nb=2\n",
        ] {
            let error = extract(&[("setup.cfg", cfg.as_bytes())])
                .err()
                .ok_or("duplicate section must fail")?;
            assert!(error.contains("duplicate setup.cfg section"));
        }
        let cfg = b"[metadata]\nname=sample\nversion=1\n[Metadata]\nversion=99\n[options]\ninstall_requires=requests\n[options.extras_require]\nDocs_Test=pytest\n";
        let metadata = extract(&[("setup.cfg", cfg)])
            .map_err(|_| "distinct case-sensitive sections")?
            .ok_or("metadata")?;
        assert_eq!(metadata.version.recorded().map(String::as_str), Some("1"));
        let rows = metadata
            .dependencies
            .recorded()
            .ok_or("static dependencies")?;
        assert!(
            rows.iter()
                .any(|row| row.group.as_deref() == Some("Docs_Test"))
        );
        Ok(())
    }

    #[test]
    fn dynamic_pep621_dependency_fields_preserve_static_subsets() {
        for project in [
            "dependencies=['requests>=2']\ndynamic=['optional-dependencies']",
            "optional-dependencies.docs=['sphinx>=7']\ndynamic=['dependencies']",
        ] {
            let text = format!(
                "[project]\nname='sample'\nversion='1'\n{project}\n[build-system]\nrequires=['wheel']\n"
            );
            let metadata = extract(&[("pyproject.toml", text.as_bytes())])
                .expect("metadata")
                .expect("project");
            assert!(matches!(
                metadata.dependencies,
                PythonMetadataFact::Partial { .. }
            ));
            let rows = metadata
                .dependencies
                .declared()
                .expect("observed declarations");
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().any(|row| row.scope == DependencyScope::Build));
            assert!(rows.iter().any(|row| row.requirement != "wheel"));
            metadata.admit().expect("bounded subset");
        }
    }

    #[test]
    fn cfg_options_and_literal_setup_identity_are_extracted_together() {
        let metadata = extract(&[
            ("setup.cfg", b"[options]\ninstall_requires=requests>=2\n[options.extras_require]\ntest=pytest>=8\n"),
            ("setup.py", b"from setuptools import setup\nsetup(name='sample', version='1')\n"),
        ]).expect("metadata").expect("combined cfg/setup");
        assert_eq!(metadata.name.recorded().map(String::as_str), Some("sample"));
        let rows = metadata
            .dependencies
            .recorded()
            .expect("cfg dependency declarations");
        assert_eq!(rows.len(), 2);
        assert_eq!(metadata.evidence.len(), 2);
        assert!(
            rows.iter().any(
                |row| row.requirement == "requests>=2" && row.scope == DependencyScope::Runtime
            )
        );
        assert!(rows.iter().any(|row| row.requirement == "pytest>=8" && row.scope == DependencyScope::Optional));
    }

    #[test]
    fn cfg_preserves_incomplete_extra_build_and_test_declarations() {
        for (section, declaration_line, scope) in [
            (
                "options.extras_require",
                "docs = sphinx>=7",
                DependencyScope::Optional,
            ),
            (
                "options",
                "setup_requires = wheel>=1",
                DependencyScope::Build,
            ),
            (
                "options",
                "tests_require = pytest>=8",
                DependencyScope::Development,
            ),
        ] {
            let text =
                format!("[metadata]\nname=sample\nversion=1\n[{section}]\n{declaration_line}\n");
            let metadata = extract(&[("setup.cfg", text.as_bytes())])
                .expect("metadata")
                .expect("cfg");
            assert!(matches!(
                metadata.dependencies,
                PythonMetadataFact::Partial { .. }
            ));
            assert!(
                metadata.dependencies.recorded().is_none(),
                "incomplete collection"
            );
            let rows = metadata.dependencies.declared().expect("observed subset");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].scope, scope);
            metadata.admit().expect("admitted partial declaration");
            let source = PackageReference::parse("pkg:pypi/sample@1").expect("coordinate");
            assert!(
                matches!(
                    python_dependency_facts(
                        &metadata,
                        &source,
                        PackageGraphSourceAuthority::Local([7; 32]),
                        DependencyAuthority::LocalManifest,
                        [7; 32]
                    ),
                    DependencyFacts::Unknown(_)
                ),
                "partial declarations must not claim complete graph"
            );
        }
        let metadata = extract(&[("setup.cfg", b"[metadata]\nname=sample\nversion=1\n[options]\ninstall_requires = attr: sample.dependencies\ntests_require = pytest>=8\n")])
            .expect("metadata").expect("cfg");
        assert!(matches!(
            metadata.dependencies,
            PythonMetadataFact::Partial { .. }
        ));
        assert_eq!(
            metadata
                .dependencies
                .declared()
                .expect("retained static test")[0]
                .requirement,
            "pytest>=8"
        );
    }

    #[test]
    fn malformed_pep508_names_never_become_known_dependency_targets() -> Result<(), &'static str> {
        let source = PackageReference::parse("pkg:pypi/sample@1").expect("coordinate");
        for requirement in [
            "../evil>=1",
            "_evil>=1",
            "evil/escape>=1",
            "evil\\escape>=1",
            "evil.$bad>=1",
            "évil>=1",
            "evil->=1",
        ] {
            let text =
                format!("[project]\nname='sample'\nversion='1'\ndependencies=['{requirement}']\n");
            let metadata = extract(&[("pyproject.toml", text.as_bytes())])
                .expect("literal declaration")
                .expect("project");
            assert!(
                matches!(
                    python_dependency_facts(
                        &metadata,
                        &source,
                        PackageGraphSourceAuthority::Local([7; 32]),
                        DependencyAuthority::LocalManifest,
                        [7; 32]
                    ),
                    DependencyFacts::Unknown(_)
                ),
                "malformed requirement admitted: {requirement}"
            );
        }
        for (requirement, expected_name) in [
            ("requests[socks]>=2", "requests"),
            ("typing_extensions>=4", "typing-extensions"),
            ("zope.interface>=6", "zope-interface"),
            ("A-B>=1", "a-b"),
        ] {
            let text =
                format!("[project]\nname='sample'\nversion='1'\ndependencies=['{requirement}']\n");
            let metadata = extract(&[("pyproject.toml", text.as_bytes())])
                .expect("metadata")
                .expect("project");
            let rows = match python_dependency_facts(
                &metadata,
                &source,
                PackageGraphSourceAuthority::Local([7; 32]),
                DependencyAuthority::LocalManifest,
                [7; 32],
            ) {
                DependencyFacts::Known(rows) => rows,
                _ => return Err("valid PEP 508 requirement was not projected"),
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].target.requirement.as_str(), requirement);
            assert_eq!(rows[0].target.name.as_str(), expected_name);
        }
        Ok(())
    }

    #[test]
    fn cfg_keeps_comparator_commas_and_all_dependency_scopes() {
        let metadata = extract(&[(
            "setup.cfg",
            br#"[metadata]
name=legacy
version=1.0
[options]
install_requires=
 requests[socks]>=2,<3; python_version >= '3.10'
setup_requires=wheel
[options.extras_require]
test=
 pytest>=8
"#,
        )])
        .expect("cfg")
        .expect("metadata");
        let deps = metadata
            .dependencies
            .recorded()
            .expect("static requirements");
        assert_eq!(deps.len(), 3);
        assert_eq!(
            deps[0].requirement,
            "requests[socks]>=2,<3; python_version >= '3.10'"
        );
        assert!(deps.iter().any(|dep| dep.scope == DependencyScope::Build));
        assert!(
            deps.iter().any(|dep| dep.scope == DependencyScope::Optional
                && dep.group.as_deref() == Some("test"))
        );
    }

    #[test]
    fn missing_cfg_dependencies_and_unresolved_versions_are_explicit() {
        let metadata = extract(&[(
            "setup.cfg",
            b"[metadata]\nname=legacy\nversion=attr: unknown.__version__\n",
        )])
        .expect("cfg")
        .expect("metadata");
        assert!(matches!(
            metadata.version,
            PythonMetadataFact::Dynamic { .. }
        ));
        assert!(matches!(
            metadata.dependencies,
            PythonMetadataFact::Omitted { .. }
        ));
        assert!(metadata.version.recorded().is_none());
    }

    #[test]
    fn dynamic_pyproject_fields_never_become_known_empty_or_fake_version() {
        let metadata = extract(&[(
            "pyproject.toml",
            br#"[project]
name = "dynamic"
dynamic = ["version", "dependencies"]
"#,
        )])
        .expect("pyproject")
        .expect("metadata");
        assert!(matches!(
            metadata.version,
            PythonMetadataFact::Dynamic { .. }
        ));
        assert!(matches!(
            metadata.dependencies,
            PythonMetadataFact::Dynamic { .. }
        ));
        assert!(metadata.version.recorded().is_none());
    }

    #[test]
    fn tool_only_pyproject_allows_cfg_identity_and_static_setup_identity() {
        let metadata = extract(&[
            ("pyproject.toml", b"[tool.ruff]\nline-length=100\n"),
            (
                "setup.cfg",
                b"[metadata]\nname=legacy\nversion=1\n[options]\ninstall_requires=\n",
            ),
        ])
        .expect("cfg fallback")
        .expect("metadata");
        assert_eq!(metadata.manifest_path, "setup.cfg");
        assert!(
            metadata
                .dependencies
                .recorded()
                .expect("explicit empty declaration")
                .is_empty()
        );
        let setup = extract(&[("setup.py", b"from setuptools import setup\nVERSION='2.0'\nsetup(name='legacy', version=VERSION, install_requires=['requests>=2,<3'], project_urls={'Docs':'https://example.test/docs'})\n")]).expect("literal setup").expect("metadata");
        assert_eq!(setup.version.recorded().map(String::as_str), Some("2.0"));
        assert_eq!(
            setup.documentation.recorded().map(String::as_str),
            Some("https://example.test/docs")
        );
    }

    #[test]
    fn attr_paths_cannot_escape_and_ambiguous_sources_are_dynamic() {
        for attr in [
            "../../outside.__version__",
            "/tmp/outside.__version__",
            "p.__version__()",
            "p.💥",
        ] {
            let cfg = format!("[metadata]\nname=test\nversion=attr: {attr}\n");
            let metadata = extract_python_project(|path| match path {
                "setup.cfg" => Ok(Some(cfg.as_bytes().to_vec())),
                "setup.py" | "pyproject.toml" => Ok(None),
                _ => panic!("unsafe attribute read: {path}"),
            })
            .expect("typed invalid attribute")
            .expect("metadata");
            assert!(matches!(
                metadata.version,
                PythonMetadataFact::Dynamic { .. }
            ));
        }
        let metadata = extract(&[
            (
                "setup.cfg",
                b"[metadata]\nname=x\nversion=attr: p.__version__\n",
            ),
            ("p.py", b"__version__='1'"),
            ("p/__init__.py", b"__version__='2'"),
        ])
        .expect("ambiguous source")
        .expect("metadata");
        assert!(matches!(
            metadata.version,
            PythonMetadataFact::Dynamic { .. }
        ));
        assert_eq!(metadata.evidence.len(), 3);
    }

    #[test]
    fn executable_source_and_cfg_overrides_are_incomplete() {
        for version in [
            "__version__=calculate()",
            "if True:\n __version__='1'",
            "__version__='1'\n__version__='2'",
            "__version__='1'\nexec('arbitrary')",
        ] {
            let metadata = extract(&[
                (
                    "setup.cfg",
                    b"[metadata]\nname=x\nversion=attr: p.__version__\n",
                ),
                ("p.py", version.as_bytes()),
            ])
            .expect("safe dynamic attr")
            .expect("metadata");
            assert!(matches!(
                metadata.version,
                PythonMetadataFact::Dynamic { .. }
            ));
        }
        let metadata = extract(&[
            (
                "setup.cfg",
                b"[metadata]\nname=x\nversion=1\n[options]\ninstall_requires=requests\n",
            ),
            (
                "setup.py",
                b"from setuptools import setup\nsetup(version='2', install_requires=['other'])\n",
            ),
        ])
        .expect("explicit override")
        .expect("metadata");
        assert_eq!(metadata.version.recorded().map(String::as_str), Some("2"));
        assert!(matches!(
            metadata.dependencies,
            PythonMetadataFact::Partial { .. }
        ));
        assert_eq!(
            metadata
                .dependencies
                .declared()
                .expect("original cfg declaration")[0]
                .requirement,
            "requests"
        );
        let metadata = extract(&[("setup.py", b"import os\nos.system('never execute')\n")])
            .expect("dynamic setup")
            .expect("metadata");
        assert!(matches!(metadata.name, PythonMetadataFact::Dynamic { .. }));
    }

    #[test]
    fn malformed_duplicate_and_conflicting_fields_are_rejected() {
        for bytes in [
            b"[metadata]\nname=x\nname=y\n".as_slice(),
            b"[metadata]\nname=x\nversion=1\n[options]\nnot an option\n",
        ] {
            assert!(extract(&[("setup.cfg", bytes)]).is_err());
        }
        for text in [
            "[project]\nname=42\n",
            "[project]\nname='x'\nversion='1'\ndynamic=['version']\n",
            "[project]\nname='x'\ndependencies=['ok',42]\n",
        ] {
            assert!(extract(&[("pyproject.toml", text.as_bytes())]).is_err());
        }
        assert!(extract(&[("setup.py", b"setup(")]).is_err());
        assert!(extract(&[("setup.cfg", &[0xff])]).is_err());
        assert!(extract(&[("setup.cfg", &vec![b'x'; MAX_PYTHON_METADATA_BYTES + 1])]).is_err());
    }

    #[test]
    fn over_budget_requirements_keep_declarations_and_report_complexity() -> Result<(), &'static str>
    {
        let requirement = format!(
            "requests; {}",
            vec!["python_version >= '3.8'"; 14].join(" and ")
        );
        let text =
            format!("[project]\nname='sample'\nversion='1'\ndependencies=[\"{requirement}\"]\n");
        let metadata = extract(&[("pyproject.toml", text.as_bytes())])
            .map_err(|_| "static declaration must extract")?
            .ok_or("metadata must be present")?;
        let PythonMetadataFact::Recorded { value, .. } = &metadata.dependencies else {
            return Err("source declarations must remain recorded");
        };
        assert_eq!(value[0].requirement, requirement);
        let source = PackageReference::parse("pkg:pypi/sample@1").map_err(|_| "coordinate")?;
        let DependencyFacts::Unknown(reason) = python_dependency_facts(
            &metadata,
            &source,
            PackageGraphSourceAuthority::Local([7; 32]),
            DependencyAuthority::LocalManifest,
            [7; 32],
        ) else {
            return Err("complexity refusal must withhold the whole graph");
        };
        assert!(reason.as_str().contains("complexity budget"));
        Ok(())
    }

    #[test]
    fn version_source_changes_change_the_shared_metadata_frontier() {
        let cfg = b"[metadata]\nname=x\nversion=attr: p.__version__\n";
        let first = extract(&[("setup.cfg", cfg), ("p.py", b"__version__='1'")])
            .expect("first")
            .expect("metadata");
        let second = extract(&[("setup.cfg", cfg), ("p.py", b"__version__='2'")])
            .expect("second")
            .expect("metadata");
        assert_ne!(
            python_metadata_digest(&first),
            python_metadata_digest(&second)
        );
        assert_eq!(second.version.recorded().map(String::as_str), Some("2"));
    }

    #[test]
    fn raw_markers_and_garbage_requirements_cannot_claim_known_graph_or_optional_scope()
    -> Result<(), &'static str> {
        let source = PackageReference::parse("pkg:pypi/sample@1").expect("source");
        for requirement in [
            "requests garbage",
            "requests ???",
            "requests; python_version < '3' and python_version >= '4'",
        ] {
            let text =
                format!("[project]\nname='sample'\nversion='1'\ndependencies=[{requirement:?}]\n");
            let metadata = extract(&[("pyproject.toml", text.as_bytes())])
                .expect("metadata")
                .expect("project");
            assert!(
                matches!(
                    python_dependency_facts(
                        &metadata,
                        &source,
                        PackageGraphSourceAuthority::Local([7; 32]),
                        DependencyAuthority::LocalManifest,
                        [7; 32]
                    ),
                    DependencyFacts::Unknown(_)
                ),
                "unparsed or unsatisfiable requirement cannot claim known graph"
            );
        }

        for (requirement, expected_optional) in [
            ("requests; extra != 'docs'", false),
            ("requests; extra == ''", false),
            ("requests; extra == 'docs'", true),
            (
                "requests; extra == 'docs' and python_version >= '3.10'",
                true,
            ),
            (
                "requests; extra == 'docs' or python_version >= '3.10'",
                false,
            ),
            ("requests; platform_machine == 'é💥extra'", false),
        ] {
            let text =
                format!("[project]\nname='sample'\nversion='1'\ndependencies=[{requirement:?}]\n");
            let metadata = extract(&[("pyproject.toml", text.as_bytes())])
                .expect("metadata")
                .expect("project");
            let declared = &metadata
                .dependencies
                .recorded()
                .expect("original literal declarations")[0];
            assert_eq!(declared.requirement, requirement);
            assert_eq!(
                declared.scope,
                DependencyScope::Runtime,
                "scope follows declaring field, never marker-token presence"
            );
            let rows = match python_dependency_facts(
                &metadata,
                &source,
                PackageGraphSourceAuthority::Local([7; 32]),
                DependencyAuthority::LocalManifest,
                [7; 32],
            ) {
                DependencyFacts::Known(rows) => rows,
                _ => return Err("valid PEP 508 marker failed to produce facts"),
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].target.requirement.as_str(), requirement);
            assert_eq!(rows[0].scope, DependencyScope::Runtime);
            assert_eq!(rows[0].optional, expected_optional);
        }

        for requirement in [
            "sample-wheel @ https://example.org/packages/sample-wheel-1.0.whl",
            "requests[socks]>=2,<3; python_version >= '3.10'",
        ] {
            let text =
                format!("[project]\nname='sample'\nversion='1'\ndependencies=[{requirement:?}]\n");
            let metadata = extract(&[("pyproject.toml", text.as_bytes())])
                .expect("metadata")
                .expect("project");
            let rows = match python_dependency_facts(
                &metadata,
                &source,
                PackageGraphSourceAuthority::Local([7; 32]),
                DependencyAuthority::LocalManifest,
                [7; 32],
            ) {
                DependencyFacts::Known(rows) => rows,
                _ => return Err("valid URL or extra requirement failed to project"),
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].target.requirement.as_str(), requirement);
        }

        let mixed = extract(&[(
            "pyproject.toml",
            b"[project]\nname='sample'\nversion='1'\ndependencies=['requests>=2', 'invalid requirement garbage']\n",
        )])
        .expect("metadata")
        .expect("project");
        assert!(
            matches!(
                python_dependency_facts(
                    &mixed,
                    &source,
                    PackageGraphSourceAuthority::Local([7; 32]),
                    DependencyAuthority::LocalManifest,
                    [7; 32]
                ),
                DependencyFacts::Unknown(_)
            ),
            "one invalid declaration must prevent a partial Known graph"
        );
        Ok(())
    }
}
