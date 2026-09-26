//! Outgoing dependency edges declared by indexed local project manifests.

use super::{
    ManifestText, cargo_dependency_facts, element_text, read_local_manifest, read_manifest_bytes,
    strip_xml_comments,
};
use backend_engine::{PackageDependencyRecord, PackageReference};
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencySourceFacts, PackageDependencyTarget, ProductText, RegistryEcosystem,
    admit_dependency_rows, collapse_dependency_rows, dependency_optional,
};
use std::path::Path;

pub(super) fn local_dependency_facts(
    project_root: &Path,
) -> Result<Option<PackageDependencySourceFacts>, String> {
    if project_root.join("Cargo.toml").is_file() {
        return cargo_dependency_facts(project_root);
    }
    let Some(identity) = read_local_manifest(project_root)? else {
        return Ok(None);
    };
    let source = identity.record.coordinate;
    let facts = if project_root.join("package.json").is_file() {
        npm_dependency_facts(project_root, &source)?
    } else if project_root.join("pyproject.toml").is_file() {
        python_dependency_facts(project_root, &source)?
    } else if project_root.join("go.mod").is_file() {
        go_dependency_facts(project_root, &source)?
    } else if project_root.join("pom.xml").is_file() {
        maven_dependency_facts(project_root, &source)?
    } else {
        nuget_dependency_facts(project_root, &source)?
    };
    Ok(Some((source, facts)))
}

fn manifest_frontier(project_root: &Path) -> [u8; 32] {
    *blake3::hash(project_root.as_os_str().as_encoded_bytes()).as_bytes()
}

fn dependency_row(
    source: &PackageReference,
    ecosystem: RegistryEcosystem,
    name: &str,
    requirement: &str,
    scope: DependencyScope,
    declared_optional: bool,
    bytes: &[u8],
    frontier: [u8; 32],
) -> Result<PackageDependencyRecord, String> {
    let target = PackageDependencyTarget::new(ecosystem, name, requirement, None)
        .map_err(|error| format!("admit local dependency {name}: {error}"))?;
    Ok(PackageDependencyRecord::new(
        source.clone(),
        target,
        scope,
        dependency_optional(scope, declared_optional),
        DependencyEvidence {
            authority: DependencyAuthority::LocalManifest,
            frontier,
            provenance: *blake3::hash(bytes).as_bytes(),
        },
    ))
}

fn known_facts(
    rows: Vec<PackageDependencyRecord>,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    admit_dependency_rows(collapse_dependency_rows(rows))
        .map(DependencyFacts::Known)
        .map_err(|error| format!("local manifest dependency rows are invalid: {error}"))
}

fn npm_dependency_facts(
    project_root: &Path,
    source: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    let path = project_root.join("package.json");
    let bytes = read_manifest_bytes(&path)?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|error| format!("parse local manifest {}: {error}", path.display()))?;
    let Some(object) = value.as_object() else {
        return Err(format!(
            "local manifest {} is not a JSON object",
            path.display()
        ));
    };
    let frontier = manifest_frontier(project_root);
    let mut rows = Vec::new();
    for (field, scope) in [
        ("dependencies", DependencyScope::Runtime),
        ("optionalDependencies", DependencyScope::Optional),
        ("peerDependencies", DependencyScope::Peer),
        ("devDependencies", DependencyScope::Development),
    ] {
        let Some(raw) = object.get(field) else {
            continue;
        };
        let Some(values) = raw.as_object() else {
            return Err(format!(
                "local manifest {} {field} is not an object",
                path.display()
            ));
        };
        let declared_optional = matches!(scope, DependencyScope::Optional);
        for (name, requirement) in values {
            let Some(requirement) = requirement.as_str() else {
                return Err(format!(
                    "local manifest {} dependency {name} has a non-string requirement",
                    path.display()
                ));
            };
            rows.push(dependency_row(
                source,
                RegistryEcosystem::Npm,
                name,
                requirement,
                scope,
                declared_optional,
                &bytes,
                frontier,
            )?);
        }
    }
    known_facts(rows)
}

fn python_dependency_facts(
    project_root: &Path,
    source: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    let path = project_root.join("pyproject.toml");
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let root = toml::from_str::<toml::Value>(text)
        .map_err(|error| format!("parse local manifest {}: {error}", path.display()))?;
    let Some(project) = root.get("project").and_then(toml::Value::as_table) else {
        return known_facts(Vec::new());
    };
    let frontier = manifest_frontier(project_root);
    let mut rows = Vec::new();
    if let Some(dependencies) = project.get("dependencies") {
        let Some(dependencies) = dependencies.as_array() else {
            return Err(format!(
                "local manifest {} project.dependencies is not an array",
                path.display()
            ));
        };
        for requirement in dependencies {
            let Some(requirement) = requirement.as_str() else {
                return Err(format!(
                    "local manifest {} has a non-string project dependency",
                    path.display()
                ));
            };
            let name = pep508_name(requirement)?;
            let extra = pep508_extra(requirement);
            let scope = if extra {
                DependencyScope::Optional
            } else {
                DependencyScope::Runtime
            };
            rows.push(dependency_row(
                source,
                RegistryEcosystem::Pypi,
                name,
                requirement,
                scope,
                extra,
                &bytes,
                frontier,
            )?);
        }
    }
    if let Some(extras) = project.get("optional-dependencies") {
        let Some(extras) = extras.as_table() else {
            return Err(format!(
                "local manifest {} optional-dependencies is not a table",
                path.display()
            ));
        };
        for (group, values) in extras {
            let Some(values) = values.as_array() else {
                return Err(format!(
                    "local manifest {} optional dependency group {group} is not an array",
                    path.display()
                ));
            };
            for requirement in values {
                let Some(requirement) = requirement.as_str() else {
                    return Err(format!(
                        "local manifest {} optional dependency group {group} has a non-string requirement",
                        path.display()
                    ));
                };
                let name = pep508_name(requirement)?;
                rows.push(dependency_row(
                    source,
                    RegistryEcosystem::Pypi,
                    name,
                    requirement,
                    DependencyScope::Optional,
                    true,
                    &bytes,
                    frontier,
                )?);
            }
        }
    }
    known_facts(rows)
}

fn pep508_name(requirement: &str) -> Result<&str, String> {
    let end = requirement
        .find(|character: char| {
            character.is_whitespace()
                || matches!(character, '[' | ';' | '<' | '>' | '=' | '!' | '~' | '@')
        })
        .unwrap_or(requirement.len());
    let Some(name) = requirement.get(..end) else {
        return Err(format!(
            "local manifest requirement {requirement} has no package name"
        ));
    };
    if name.is_empty()
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!(
            "local manifest requirement {requirement} has no package name"
        ));
    }
    Ok(name)
}

fn pep508_extra(requirement: &str) -> bool {
    let Some((_, marker)) = requirement.split_once(';') else {
        return false;
    };
    let bytes = marker.as_bytes();
    let mut index = 0;
    let mut quoted = false;
    let mut quote = b'"';
    while index < bytes.len() {
        let Some(byte) = bytes.get(index).copied() else {
            break;
        };
        if quoted {
            if byte == quote {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            quoted = true;
            quote = byte;
            index += 1;
            continue;
        }
        if marker[index..].starts_with("extra") {
            let before = index == 0
                || bytes
                    .get(index - 1)
                    .is_none_or(|previous| !is_marker_identifier_byte(*previous));
            let after = index + 5;
            let after_boundary = bytes
                .get(after)
                .is_none_or(|next| !is_marker_identifier_byte(*next));
            if before && after_boundary {
                let rest = marker[after..].trim_start();
                if rest.starts_with("===")
                    || rest.starts_with("==")
                    || rest.starts_with("!=")
                    || rest.starts_with("~=")
                    || rest.starts_with("<=")
                    || rest.starts_with(">=")
                    || rest.starts_with('<')
                    || rest.starts_with('>')
                    || rest.starts_with("in")
                    || rest.starts_with("not")
                {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn is_marker_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn go_dependency_facts(
    project_root: &Path,
    source: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    let path = project_root.join("go.mod");
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let frontier = manifest_frontier(project_root);
    let mut rows = Vec::new();
    for (module, version, indirect) in go_requires(text)? {
        let scope = if indirect {
            DependencyScope::Development
        } else {
            DependencyScope::Runtime
        };
        rows.push(dependency_row(
            source,
            RegistryEcosystem::Golang,
            &module,
            &version,
            scope,
            false,
            &bytes,
            frontier,
        )?);
    }
    known_facts(rows)
}

fn go_requires(text: &str) -> Result<Vec<(String, String, bool)>, String> {
    let mut requires = Vec::new();
    let mut in_require = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let (code, comment) = line.split_once("//").map_or((line, ""), |parts| parts);
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        if in_require {
            if code == ")" {
                in_require = false;
                continue;
            }
            requires.push(go_require_fields(code, comment)?);
            continue;
        }
        if code == "require (" || code == "require(" {
            in_require = true;
            continue;
        }
        if let Some(rest) = code.strip_prefix("require ") {
            requires.push(go_require_fields(rest, comment)?);
        }
    }
    if in_require {
        return Err("go.mod require block is not closed".to_owned());
    }
    Ok(requires)
}

fn go_require_fields(line: &str, comment: &str) -> Result<(String, String, bool), String> {
    let mut fields = line.split_ascii_whitespace();
    let module = fields
        .next()
        .ok_or_else(|| "go.mod require omits a module path".to_owned())?;
    let version = fields
        .next()
        .ok_or_else(|| format!("go.mod require {module} omits a version"))?;
    if fields.next().is_some() || module.contains('"') || !go_version_literal(version) {
        return Err(format!(
            "go.mod require {module} is not a literal module version"
        ));
    }
    Ok((
        module.to_owned(),
        version.to_owned(),
        comment.trim() == "indirect",
    ))
}

fn go_version_literal(version: &str) -> bool {
    let Some(rest) = version.strip_prefix('v') else {
        return false;
    };
    if rest.is_empty() || rest.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    let core = rest.split_once('+').map_or(rest, |(core, _)| core);
    let core = core.split_once('-').map_or(core, |(core, _)| core);
    let mut numbers = core.split('.');
    let Some(major) = numbers.next() else {
        return false;
    };
    let Some(minor) = numbers.next() else {
        return false;
    };
    let Some(patch) = numbers.next() else {
        return false;
    };
    numbers.next().is_none()
        && !major.is_empty()
        && !minor.is_empty()
        && !patch.is_empty()
        && major.parse::<u64>().is_ok()
        && minor.parse::<u64>().is_ok()
        && patch.parse::<u64>().is_ok()
}

fn maven_dependency_facts(
    project_root: &Path,
    source: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    let path = project_root.join("pom.xml");
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let document = strip_xml_comments(text)?;
    let frontier = manifest_frontier(project_root);
    let mut rows = Vec::new();
    for body in maven_dependency_bodies(&document)? {
        let group = match element_text(body, "groupId")? {
            ManifestText::Literal(value) => value,
            ManifestText::Dynamic => {
                return Ok(unavailable(
                    "Maven POM dependency group is not a literal requirement",
                )?);
            }
            ManifestText::Missing => {
                return Err(format!(
                    "local manifest {} dependency omits groupId",
                    path.display()
                ));
            }
        };
        let artifact = match element_text(body, "artifactId")? {
            ManifestText::Literal(value) => value,
            ManifestText::Dynamic => {
                return Ok(unavailable(
                    "Maven POM dependency artifact is not a literal requirement",
                )?);
            }
            ManifestText::Missing => {
                return Err(format!(
                    "local manifest {} dependency omits artifactId",
                    path.display()
                ));
            }
        };
        let requirement = match element_text(body, "version")? {
            ManifestText::Literal(value) => value,
            ManifestText::Dynamic => {
                return Ok(unavailable(
                    "Maven POM dependency version is not a literal requirement",
                )?);
            }
            ManifestText::Missing => {
                return Ok(unavailable(
                    "Maven POM dependency omits its version requirement",
                )?);
            }
        };
        let scope = match element_text(body, "scope")? {
            ManifestText::Missing => DependencyScope::Runtime,
            ManifestText::Literal("test") => DependencyScope::Development,
            ManifestText::Literal("provided" | "system") => DependencyScope::Build,
            ManifestText::Literal("runtime" | "compile") => DependencyScope::Runtime,
            ManifestText::Literal(other) => {
                return Err(format!(
                    "local manifest {} declares unsupported maven dependency scope {other}",
                    path.display()
                ));
            }
            ManifestText::Dynamic => {
                return Err(format!(
                    "local manifest {} maven dependency scope is not literal",
                    path.display()
                ));
            }
        };
        let optional = match element_text(body, "optional")? {
            ManifestText::Missing | ManifestText::Literal("false" | "") => false,
            ManifestText::Literal("true") => true,
            ManifestText::Literal(_) | ManifestText::Dynamic => {
                return Err(format!(
                    "local manifest {} has a non-boolean maven optional flag",
                    path.display()
                ));
            }
        };
        let scope = if optional {
            DependencyScope::Optional
        } else {
            scope
        };
        rows.push(dependency_row(
            source,
            RegistryEcosystem::Maven,
            &format!("{group}:{artifact}"),
            requirement,
            scope,
            optional,
            &bytes,
            frontier,
        )?);
    }
    known_facts(rows)
}

fn unavailable(reason: &str) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    Ok(DependencyFacts::Unavailable(
        ProductText::new(reason).map_err(|error| error.to_string())?,
    ))
}

struct XmlTag<'document> {
    name: &'document str,
    start: usize,
    body_end: usize,
    closing: bool,
    self_closing: bool,
    inner: &'document str,
}

fn next_tag<'document>(
    document: &'document str,
    mut from: usize,
) -> Result<Option<XmlTag<'document>>, String> {
    loop {
        let Some(relative) = document.get(from..).and_then(|rest| rest.find('<')) else {
            return Ok(None);
        };
        let start = from + relative;
        let Some(rest) = document.get(start + 1..) else {
            return Err("local manifest has a truncated XML tag".to_owned());
        };
        if rest.starts_with('!') || rest.starts_with('?') {
            let Some(end) = rest.find('>') else {
                return Err("local manifest has an unclosed XML declaration".to_owned());
            };
            from = start + 1 + end + 1;
            continue;
        }
        let closing = rest.starts_with('/');
        let name_offset = usize::from(closing);
        let Some(name_region) = rest.get(name_offset..) else {
            return Err("local manifest has an empty XML tag".to_owned());
        };
        let name_len = name_region
            .bytes()
            .position(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>'))
            .unwrap_or(name_region.len());
        if name_len == 0 {
            return Err("local manifest has an empty XML tag".to_owned());
        }
        let Some(name) = name_region.get(..name_len) else {
            return Err("local manifest has an empty XML tag".to_owned());
        };
        let Some(end) = rest.find('>') else {
            return Err(format!("local manifest {name} tag is not closed"));
        };
        let Some(raw) = rest.get(..end) else {
            return Err(format!("local manifest {name} tag is truncated"));
        };
        let self_closing = raw.ends_with('/');
        let inner = raw
            .get(name_offset + name_len..)
            .unwrap_or("")
            .trim()
            .trim_end_matches('/')
            .trim();
        return Ok(Some(XmlTag {
            name,
            start,
            body_end: start + 1 + end + 1,
            closing,
            self_closing,
            inner,
        }));
    }
}

fn maven_dependency_bodies(document: &str) -> Result<Vec<&str>, String> {
    let mut stack = Vec::new();
    let mut bodies = Vec::new();
    let mut cursor = 0;
    let mut capture_start = None;
    while let Some(tag) = next_tag(document, cursor)? {
        cursor = tag.body_end;
        if tag.closing {
            if stack.pop() != Some(tag.name) {
                return Err(format!("local manifest closes {} out of order", tag.name));
            }
            if tag.name == "dependency"
                && let Some(start) = capture_start.take()
            {
                let Some(body) = document.get(start..tag.start) else {
                    return Err("local manifest maven dependency is truncated".to_owned());
                };
                bodies.push(body);
            }
            continue;
        }
        if tag.self_closing {
            if tag.name == "dependency" && project_dependencies(&stack) {
                bodies.push("");
            }
            continue;
        }
        if tag.name == "dependency" && project_dependencies(&stack) {
            if capture_start.is_some() {
                return Err("local manifest nests maven dependency elements".to_owned());
            }
            capture_start = Some(tag.body_end);
        }
        stack.push(tag.name);
    }
    if capture_start.is_some() || !stack.is_empty() {
        return Err("local manifest has an unclosed XML element".to_owned());
    }
    Ok(bodies)
}

fn project_dependencies(stack: &[&str]) -> bool {
    stack.last() == Some(&"dependencies")
        && !stack.iter().any(|name| {
            matches!(
                *name,
                "dependencyManagement"
                    | "build"
                    | "profiles"
                    | "profile"
                    | "plugin"
                    | "plugins"
                    | "reporting"
            )
        })
}

fn nuget_dependency_facts(
    project_root: &Path,
    source: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    let path = nuspec_path(project_root)?;
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let document = strip_xml_comments(text)?;
    let frontier = manifest_frontier(project_root);
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    let mut stack = Vec::new();
    let mut cursor = 0;
    while let Some(tag) = next_tag(&document, cursor)? {
        cursor = tag.body_end;
        if tag.closing {
            if stack.pop() != Some(tag.name) {
                return Err(format!("local manifest closes {} out of order", tag.name));
            }
            continue;
        }
        if tag.name == "dependency" && stack.iter().any(|name| *name == "dependencies") {
            let id = xml_attribute(tag.inner, "id")?.ok_or_else(|| {
                format!(
                    "local manifest {} nuget dependency omits id",
                    path.display()
                )
            })?;
            let version = xml_attribute(tag.inner, "version")?.ok_or_else(|| {
                format!(
                    "local manifest {} nuget dependency {id} omits a version",
                    path.display()
                )
            })?;
            if version.contains("${") || id.contains("${") {
                return Ok(unavailable(
                    "NuGet nuspec dependency identity is not a literal requirement",
                )?);
            }
            if let Some((seen_name, seen_version)) = seen
                .iter()
                .find(|(seen_name, _): &&(String, String)| seen_name.eq_ignore_ascii_case(id))
            {
                if seen_version != version {
                    return Err(format!(
                        "local manifest repeats nuget dependency {seen_name} with a different range"
                    ));
                }
            } else {
                seen.push((id.to_owned(), version.to_owned()));
                rows.push(dependency_row(
                    source,
                    RegistryEcosystem::Nuget,
                    id,
                    version,
                    DependencyScope::Runtime,
                    false,
                    &bytes,
                    frontier,
                )?);
            }
        }
        if !tag.self_closing {
            stack.push(tag.name);
        }
    }
    if !stack.is_empty() {
        return Err("local manifest has an unclosed XML element".to_owned());
    }
    known_facts(rows)
}

fn nuspec_path(project_root: &Path) -> Result<std::path::PathBuf, String> {
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(project_root)
        .map_err(|error| format!("read indexed project {}: {error}", project_root.display()))?
    {
        let path = entry.map_err(|error| error.to_string())?.path();
        let is_nuspec = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nuspec"));
        if is_nuspec && path.is_file() {
            specs.push(path);
        }
    }
    specs.sort();
    match specs.as_slice() {
        [path] => Ok(path.clone()),
        _ => Err(format!(
            "indexed project {} has no single nuspec",
            project_root.display()
        )),
    }
}

fn xml_attribute<'document>(
    inner: &'document str,
    name: &str,
) -> Result<Option<&'document str>, String> {
    let mut rest = inner.trim();
    while !rest.is_empty() {
        let Some((key, after_key)) = rest.split_once('=') else {
            return Err("local manifest attribute is missing a value".to_owned());
        };
        let key = key.trim();
        let after_key = after_key.trim_start();
        let Some(quote) = after_key.chars().next() else {
            return Err("local manifest attribute value is missing".to_owned());
        };
        if quote != '"' && quote != '\'' {
            return Err("local manifest attribute value is not quoted".to_owned());
        }
        let Some(value_and_rest) = after_key.get(quote.len_utf8()..) else {
            return Err("local manifest attribute value is missing".to_owned());
        };
        let Some(end) = value_and_rest.find(quote) else {
            return Err("local manifest attribute value is not closed".to_owned());
        };
        let Some(value) = value_and_rest.get(..end) else {
            return Err("local manifest attribute value is truncated".to_owned());
        };
        if key == name {
            return Ok(Some(value));
        }
        rest = value_and_rest
            .get(end + quote.len_utf8()..)
            .unwrap_or("")
            .trim_start();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::super::local_dependency_facts;
    use backend_library::{DependencyFacts, DependencyScope};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-local-dependencies-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    fn write(path: &Path, bytes: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, bytes).expect("fixture manifest");
    }

    fn known<'a>(
        facts: &'a DependencyFacts<Box<[backend_engine::PackageDependencyRecord]>>,
    ) -> &'a [backend_engine::PackageDependencyRecord] {
        match facts {
            DependencyFacts::Known(rows) => rows,
            DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
                panic!("expected known dependencies, got {}", reason.as_str())
            }
        }
    }

    #[test]
    fn local_dependency_npm_python_and_cargo_edges() {
        let root = fixture("npm-python-cargo");
        let npm = root.join("npm");
        write(
            &npm.join("package.json"),
            r#"{"name":"left-pad","version":"1.3.0","dependencies":{"left-pad":"^1.0.0","react":"^18.0.0"},"optionalDependencies":{"left-pad":"^2.0.0","fsevents":"^2.0.0"},"peerDependencies":{"react-dom":"^18.0.0"},"devDependencies":{"jest":"^29.0.0"}}"#,
        );
        let (source, facts) = local_dependency_facts(&npm).expect("npm").expect("facts");
        assert_eq!(source.as_str(), "pkg:npm/left-pad@1.3.0");
        let rows = known(&facts);
        let left_pad = rows
            .iter()
            .find(|row| row.target.name.as_str() == "left-pad")
            .expect("left-pad");
        assert_eq!(left_pad.scope, DependencyScope::Runtime);
        assert_eq!(left_pad.target.requirement.as_str(), "^1.0.0");
        assert!(!left_pad.optional);
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "fsevents")
                .expect("optional")
                .scope,
            DependencyScope::Optional
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "react-dom")
                .expect("peer")
                .scope,
            DependencyScope::Peer
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "jest")
                .expect("dev")
                .scope,
            DependencyScope::Development
        );

        let python = root.join("python");
        write(
            &python.join("pyproject.toml"),
            "[project]\nname = \"attrs\"\nversion = \"24.2.0\"\ndependencies = [\"requests>=2\", \"packaging>=21; extra == \\\"dev\\\"\"]\n\n[project.optional-dependencies]\ntest = [\"pytest>=7\"]\n",
        );
        let (_, facts) = local_dependency_facts(&python)
            .expect("python")
            .expect("facts");
        let rows = known(&facts);
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "requests")
                .expect("requests")
                .scope,
            DependencyScope::Runtime
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "packaging")
                .expect("extra")
                .scope,
            DependencyScope::Optional
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "pytest")
                .expect("pytest")
                .scope,
            DependencyScope::Optional
        );

        let cargo = root.join("cargo");
        write(
            &cargo.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1\"\n",
        );
        write(
            &cargo.join("package.json"),
            r#"{"name":"not-cargo","version":"9.9.9","dependencies":{"left-pad":"1.0.0"}}"#,
        );
        let (source, facts) = local_dependency_facts(&cargo)
            .expect("cargo")
            .expect("facts");
        assert_eq!(source.as_str(), "pkg:cargo/demo@1.0.0");
        let rows = known(&facts);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "serde");
        assert_eq!(rows[0].scope, DependencyScope::Runtime);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_dependency_go_require_ignores_replace_and_marks_indirect() {
        let root = fixture("go");
        let module = root.join("errors@v0.9.1");
        write(
            &module.join("go.mod"),
            "module github.com/pkg/errors\n\ngo 1.22\n\nrequire (\n\tgolang.org/x/sys v0.1.0\n\tgolang.org/x/text v0.2.0 // indirect\n)\n\nreplace golang.org/x/sys => ./sys\n",
        );
        let (source, facts) = local_dependency_facts(&module).expect("go").expect("facts");
        assert_eq!(source.as_str(), "pkg:golang/github.com/pkg/errors@v0.9.1");
        let rows = known(&facts);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "golang.org/x/sys")
                .expect("sys")
                .scope,
            DependencyScope::Runtime
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "golang.org/x/text")
                .expect("text")
                .scope,
            DependencyScope::Development
        );
        let bare = root.join("content-hash");
        write(
            &bare.join("go.mod"),
            "module github.com/pkg/errors\n\nrequire golang.org/x/sys v0.1.0\n",
        );
        assert!(local_dependency_facts(&bare).expect("bare").is_none());
        write(
            &module.join("go.mod"),
            "module github.com/pkg/errors\n\nrequire golang.org/x/sys not-a-version\n",
        );
        assert!(local_dependency_facts(&module).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_dependency_maven_skips_managed_and_rejects_import_scope() {
        let root = fixture("maven");
        let project = root.join("demo");
        write(
            &project.join("pom.xml"),
            r#"<project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencyManagement>
                <dependencies>
                  <dependency>
                    <groupId>com.google.guava</groupId>
                    <artifactId>guava-parent</artifactId>
                    <version>1.0.0</version>
                  </dependency>
                </dependencies>
              </dependencyManagement>
              <dependencies>
                <dependency>
                  <groupId>junit</groupId>
                  <artifactId>junit</artifactId>
                  <version>4.13.2</version>
                  <scope>test</scope>
                </dependency>
                <dependency>
                  <groupId>com.google.guava</groupId>
                  <artifactId>guava</artifactId>
                  <version>33.0.0-jre</version>
                </dependency>
              </dependencies>
            </project>"#,
        );
        let (_, facts) = local_dependency_facts(&project)
            .expect("maven")
            .expect("facts");
        let rows = known(&facts);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.target.name.as_str() != "com.google.guava:guava-parent")
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "junit:junit")
                .expect("junit")
                .scope,
            DependencyScope::Development
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.target.name.as_str() == "com.google.guava:guava")
                .expect("guava")
                .scope,
            DependencyScope::Runtime
        );

        write(
            &project.join("pom.xml"),
            r#"<project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencies>
                <dependency>
                  <groupId>com.example</groupId>
                  <artifactId>demo</artifactId>
                  <version>${revision}</version>
                </dependency>
              </dependencies>
            </project>"#,
        );
        let (_, facts) = local_dependency_facts(&project)
            .expect("dynamic")
            .expect("facts");
        assert!(matches!(facts, DependencyFacts::Unavailable(_)));

        write(
            &project.join("pom.xml"),
            r#"<project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencies>
                <dependency>
                  <groupId>org.springframework.boot</groupId>
                  <artifactId>spring-boot-dependencies</artifactId>
                  <version>3.2.0</version>
                  <scope>import</scope>
                </dependency>
              </dependencies>
            </project>"#,
        );
        assert!(local_dependency_facts(&project).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_dependency_nuget_collapses_repeated_ranges_and_rejects_a_second_range() {
        let root = fixture("nuget");
        let project = root.join("demo");
        write(
            &project.join("Demo.nuspec"),
            r#"<package><metadata><id>Demo</id><version>1.0.0</version><dependencies><group targetFramework="net8.0"><dependency id="Newtonsoft.Json" version="13.0.1" /></group><group targetFramework="netstandard2.0"><dependency id="newtonsoft.json" version="13.0.1" /></group></dependencies></metadata></package>"#,
        );
        let (source, facts) = local_dependency_facts(&project)
            .expect("nuget")
            .expect("facts");
        assert_eq!(source.as_str(), "pkg:nuget/Demo@1.0.0");
        let rows = known(&facts);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "Newtonsoft.Json");
        assert_eq!(rows[0].target.requirement.as_str(), "13.0.1");
        assert_eq!(rows[0].scope, DependencyScope::Runtime);

        write(
            &project.join("Demo.nuspec"),
            r#"<package><metadata><id>Demo</id><version>1.0.0</version><dependencies><group targetFramework="net8.0"><dependency id="Newtonsoft.Json" version="13.0.1" /></group><group targetFramework="netstandard2.0"><dependency id="Newtonsoft.Json" version="12.0.3" /></group></dependencies></metadata></package>"#,
        );
        assert!(local_dependency_facts(&project).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
