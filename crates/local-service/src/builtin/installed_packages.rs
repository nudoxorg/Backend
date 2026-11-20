//! Bounded project-local installation observations. Never executes package code
//! or promotes these files to registry or compiler publication authority.
use backend_engine::{RowId, ViewRoot};
use backend_library::{InstalledPackageFacts, InstalledPackageFile, PackageReference, ProductText};
use std::{
    fs,
    io::Read as _,
    path::{Component, Path, PathBuf},
};

const MAX_FILE: u64 = 4 * 1024 * 1024;

/// Looks only beneath indexed project roots and their immediate package folders.
/// All identity comes from installation metadata; a requested version never
/// supplies missing identity or overrides a mismatching installed version.
pub(super) fn find(
    view: &ViewRoot,
    package: &PackageReference,
    all_versions: bool,
) -> Result<Vec<InstalledPackageFacts>, String> {
    let PackageReference::Purl(wanted) = package else {
        return Ok(Vec::new());
    };
    let Some((kind, rest)) = wanted
        .as_str()
        .strip_prefix("pkg:")
        .and_then(|s| s.split_once('/'))
    else {
        return Ok(Vec::new());
    };
    let Some((name, _)) = rest.rsplit_once('@') else {
        return Ok(Vec::new());
    };
    if !matches!(kind, "npm" | "pypi") {
        return Ok(Vec::new());
    }
    let mut facts = Vec::new();
    let mut roots = Vec::new();
    for row in view
        .row_refs()
        .filter(|row| matches!(row.id, RowId::Package(_)))
    {
        let root = Path::new(&row.label);
        if root.is_absolute() && root.is_dir() && !roots.contains(&root.to_path_buf()) {
            roots.push(root.to_path_buf());
        }
    }
    roots.truncate(64);
    for root in roots {
        let mut locations = vec![root.clone()];
        for entry in sorted_dirs(&root, 64) {
            if !entry
                .file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with('.'))
            {
                locations.push(entry);
            }
        }
        for location in locations {
            if kind == "npm" {
                // npm PURLs percent-encode the scope marker.
                let local_name = name.replace("%40", "@");
                if local_name.split('/').any(|part| {
                    part.is_empty() || part == "." || part == ".." || part.contains('\\')
                }) {
                    continue;
                }
                let relative = location
                    .strip_prefix(&root)
                    .unwrap()
                    .join("node_modules")
                    .join(local_name);
                if let Some(found) = npm(&root, &relative)? {
                    retain(&mut facts, found, wanted, all_versions);
                }
            } else {
                for environment in [".venv", "venv"] {
                    let environment = location.join(environment);
                    let mut sites = vec![environment.join("Lib/site-packages")];
                    for lib in sorted_dirs(&environment.join("lib"), 16) {
                        sites.push(lib.join("site-packages"));
                    }
                    for site in sites {
                        let Ok(site_relative) = site.strip_prefix(&root) else {
                            continue;
                        };
                        for entry in sorted_dirs(&site, 1024) {
                            let Some(filename) = entry.file_name().and_then(|s| s.to_str()) else {
                                continue;
                            };
                            if !filename.ends_with(".dist-info")
                                || !filename
                                    .to_ascii_lowercase()
                                    .replace('_', "-")
                                    .starts_with(&format!("{name}-"))
                            {
                                continue;
                            }
                            let relative = site_relative.join(filename);
                            if let Some(found) = python(&root, &relative, name)? {
                                retain(&mut facts, found, wanted, all_versions);
                            }
                        }
                    }
                }
            }
            if facts.len() >= 16 {
                return Ok(facts);
            }
        }
    }
    Ok(facts)
}

fn retain(
    facts: &mut Vec<InstalledPackageFacts>,
    found: InstalledPackageFacts,
    wanted: &backend_library::PackageCoordinate,
    all_versions: bool,
) {
    let found_identity = found.package.as_str().replace("%40", "@");
    let requested_identity = wanted.as_str().replace("%40", "@");
    let same = if all_versions {
        found_identity.rsplit_once('@').map(|s| s.0)
            == requested_identity.rsplit_once('@').map(|s| s.0)
    } else {
        found_identity == requested_identity
    };
    if same && !facts.iter().any(|f| f == &found) {
        facts.push(found);
    }
}

fn sorted_dirs(root: &Path, maximum: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in entries.take(maximum + 1).flatten() {
        if dirs.len() >= maximum {
            return Vec::new();
        }
        if entry.file_type().is_ok_and(|ty| ty.is_dir()) {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    dirs
}

#[cfg(unix)]
fn read(root: &Path, relative: &Path) -> Result<Option<(Vec<u8>, InstalledPackageFile)>, String> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let parts = relative
        .components()
        .map(|part| match part {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(parts) = parts else { return Ok(None) };
    if parts.is_empty() {
        return Ok(None);
    }
    let root = match open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let mut parent = None;
    for part in &parts[..parts.len() - 1] {
        let descriptor = match openat(
            parent.as_ref().unwrap_or(&root),
            *part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(_) => return Ok(None),
        };
        parent = Some(descriptor);
    }
    let descriptor = match openat(
        parent.as_ref().unwrap_or(&root),
        parts[parts.len() - 1],
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(_) => return Ok(None),
    };
    let file = fs::File::from(descriptor);
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_FILE {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Ok(None);
    }
    let Ok(source) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    let mut end = source.len().min(backend_library::MAX_PRODUCT_TEXT_BYTES);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let window = source[..end].trim();
    if window.is_empty() {
        return Ok(None);
    }
    let offset = source[..end].len() - source[..end].trim_start().len();
    let observed = InstalledPackageFile {
        path: ProductText::new(relative.to_string_lossy()).map_err(|e| e.to_string())?,
        digest: *blake3::hash(&bytes).as_bytes(),
        bytes: bytes.len() as u64,
        offset: offset as u64,
        excerpt: ProductText::new(window).map_err(|e| e.to_string())?,
        truncated: offset > 0 || window.len() < source.len(),
    };
    Ok(Some((bytes, observed)))
}

#[cfg(not(unix))]
fn read(_root: &Path, _relative: &Path) -> Result<Option<(Vec<u8>, InstalledPackageFile)>, String> {
    Ok(None)
}

/// Native syntax locates a useful declaration window in API files; leading
/// imports must not consume the entire API excerpt for a large Python module.
fn read_api(root: &Path, relative: &Path) -> Result<Option<InstalledPackageFile>, String> {
    let Some((bytes, mut observed)) = read(root, relative)? else {
        return Ok(None);
    };
    let frontend = if relative.extension().and_then(|s| s.to_str()) == Some("py") {
        backend_frontend_python::syntax_frontend()
    } else {
        backend_frontend_typescript::syntax_frontend()
    };
    let Ok(frontend) = frontend else {
        return Ok(Some(observed));
    };
    let Ok(analysis) = frontend.analyze(relative, &bytes) else {
        return Ok(Some(observed));
    };
    let Some(declaration) = analysis.declarations().iter().find(|declaration| {
        matches!(
            declaration.kind(),
            backend_compile::DeclarationKind::Class
                | backend_compile::DeclarationKind::Function
                | backend_compile::DeclarationKind::Interface
                | backend_compile::DeclarationKind::Type
                | backend_compile::DeclarationKind::Enum
        )
    }) else {
        return Ok(Some(observed));
    };
    let source = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let start = source
        .split_inclusive('\n')
        .take(declaration.line().saturating_sub(1) as usize)
        .map(str::len)
        .sum::<usize>();
    let mut end = source
        .len()
        .min(start.saturating_add(backend_library::MAX_PRODUCT_TEXT_BYTES));
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let window = source[start..end].trim();
    if !window.is_empty() {
        let leading = source[start..end].len() - source[start..end].trim_start().len();
        observed.offset = (start + leading) as u64;
        observed.excerpt = ProductText::new(window).map_err(|e| e.to_string())?;
        observed.truncated = observed.offset > 0 || window.len() < source.len();
    }
    Ok(Some(observed))
}

fn npm(root: &Path, relative: &Path) -> Result<Option<InstalledPackageFacts>, String> {
    let Some((bytes, manifest)) = read(root, &relative.join("package.json"))? else {
        return Ok(None);
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(name) = value.get("name").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let Some(version) = value.get("version").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let package = match backend_library::PackageCoordinate::parse(format!(
        "pkg:npm/{}@{version}",
        name.replace('@', "%40")
    )) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let mut requirements = Vec::new();
    for field in ["dependencies", "peerDependencies", "optionalDependencies"] {
        if let Some(entries) = value.get(field).and_then(|v| v.as_object()) {
            for (name, requirement) in entries {
                if requirements.len() >= 128 {
                    return Err(
                        "installed package dependency metadata exceeds 128 declarations".to_owned(),
                    );
                }
                if let Some(requirement) = requirement.as_str() {
                    requirements.push(
                        ProductText::new(format!("{field}: {name} {requirement}"))
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
        }
    }
    let mut engines = Vec::new();
    if let Some(entries) = value.get("engines").and_then(|v| v.as_object()) {
        for (name, requirement) in entries {
            if engines.len() >= 16 {
                return Err("installed package engine metadata exceeds bound".to_owned());
            }
            if let Some(requirement) = requirement.as_str() {
                engines.push(
                    ProductText::new(format!("{name} {requirement}")).map_err(|e| e.to_string())?,
                );
            }
        }
    }
    let mut files = vec![manifest];
    for filename in ["README.md", "readme.md", "README.rst", "README"] {
        if let Some((_, file)) = read(root, &relative.join(filename))? {
            files.push(file);
            break;
        }
    }
    let mut entrypoints = Vec::new();
    for field in ["types", "typings", "module", "main"] {
        if let Some(path) = value.get(field).and_then(|v| v.as_str()) {
            entrypoints.push(path.to_owned());
        }
    }
    entrypoints.extend(["index.d.ts".to_owned(), "index.js".to_owned()]);
    for path in entrypoints {
        if files.len() >= 8 {
            break;
        }
        let path = relative.join(path.strip_prefix("./").unwrap_or(&path));
        if let Some(file) = read_api(root, &path)? {
            if !files.iter().any(|f| f.path == file.path) {
                files.push(file);
            }
        }
    }
    Ok(Some(InstalledPackageFacts {
        project: ProductText::new(root.to_string_lossy()).map_err(|e| e.to_string())?,
        package,
        files: files.into_boxed_slice(),
        requirements: requirements.into_boxed_slice(),
        engines: engines.into_boxed_slice(),
    }))
}

fn python(
    root: &Path,
    relative: &Path,
    wanted: &str,
) -> Result<Option<InstalledPackageFacts>, String> {
    let Some((bytes, metadata)) = read(root, &relative.join("METADATA"))? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let mut name = None;
    let mut version = None;
    let mut requirements = Vec::new();
    let mut engines = Vec::new();
    for line in text.lines().take_while(|line| !line.is_empty()) {
        if let Some(value) = line.strip_prefix("Name: ") {
            name = Some(value);
        }
        if let Some(value) = line.strip_prefix("Version: ") {
            version = Some(value);
        }
        if let Some(value) = line.strip_prefix("Requires-Dist: ") {
            if requirements.len() >= 128 {
                return Err("installed Python dependencies exceed bound".to_owned());
            }
            requirements.push(ProductText::new(value).map_err(|e| e.to_string())?);
        }
        if let Some(value) = line.strip_prefix("Requires-Python: ") {
            if engines.len() >= 16 {
                return Err("installed Python runtime metadata exceeds bound".to_owned());
            }
            engines.push(ProductText::new(format!("python {value}")).map_err(|e| e.to_string())?);
        }
    }
    let (Some(name), Some(version)) = (name, version) else {
        return Ok(None);
    };
    let name = name.to_ascii_lowercase().replace(['_', '.'], "-");
    if name != wanted {
        return Ok(None);
    }
    let package =
        match backend_library::PackageCoordinate::parse(format!("pkg:pypi/{name}@{version}")) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
    let mut files = vec![metadata];
    let site = relative.parent().unwrap();
    for filename in [
        format!("{}/__init__.py", name.replace('-', "_")),
        format!("{}.py", name.replace('-', "_")),
    ] {
        if let Some(file) = read_api(root, &site.join(filename))? {
            files.push(file);
        }
    }
    // RECORD supplies actual package source paths without guessing class/module names.
    if let Some((record, _)) = read(root, &relative.join("RECORD"))? {
        if let Ok(record) = std::str::from_utf8(&record) {
            for line in record.lines().take(4096) {
                let Some(path) = line.split(',').next().filter(|s| s.ends_with(".py")) else {
                    continue;
                };
                if files.len() >= 8 {
                    break;
                }
                if let Some(file) = read_api(root, &site.join(path))? {
                    if !files.iter().any(|f| f.path == file.path) {
                        files.push(file);
                    }
                }
            }
        }
    }
    Ok(Some(InstalledPackageFacts {
        project: ProductText::new(root.to_string_lossy()).map_err(|e| e.to_string())?,
        package,
        files: files.into_boxed_slice(),
        requirements: requirements.into_boxed_slice(),
        engines: engines.into_boxed_slice(),
    }))
}
