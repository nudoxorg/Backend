//! A crate's `Cargo.toml` as the folio reads it: who wrote it, where its
//! repository is, what edition it is, whether it has a build script or is a
//! procedural macro, its features and what each one switches on. A port of
//! the design board's extractor (`extract_trust.py`, `manifest_fields`, and
//! `extract_more.py`, `feature_graph`), with the same rules.

use facet::folio::state::{Build, Library};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use toml::{Table, Value};

/// Whether a feature is on before anyone asks.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Enabled {
    /// Reachable from `default`.
    ByDefault,
    /// Only when asked for.
    #[default]
    OnRequest,
}

/// Whether a dependency must be there.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Need {
    /// Always built.
    Required,
    /// Built when a feature (or a consumer) asks.
    Optional,
}

/// What builds with a dependency.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Stage {
    /// Your code links it.
    Runtime,
    /// Only a build script needs it.
    Build,
}

/// Whether a dependency's own default features are on.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefaultFeatures {
    /// They are.
    On,
    /// `default-features = false`.
    Off,
}

/// Where a dependency comes from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Origin {
    /// A registry release: weight beneath you.
    Registry,
    /// A `path` dependency: another crate of the same project, which is
    /// yours rather than weight beneath you.
    Path,
}

/// What a feature switches on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeatureNode {
    /// Other features it turns on, in manifest order.
    pub enables: Vec<String>,
    /// Optional dependencies it turns on (manifest keys).
    pub deps: Vec<String>,
    /// Whether it is on by default.
    pub enabled: Enabled,
}

/// One declared dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declared {
    /// The manifest key.
    pub key: String,
    /// The package it names (`package = "..."`, else the key).
    pub package: String,
    /// The version requirement as written (`*` when none).
    pub req: String,
    /// Whether it must be there (optional if any table says so).
    pub need: Need,
    /// What builds with it (a build script's only, when every table says so).
    pub stage: Stage,
    /// The `[target.*]` tables it is declared under (`None`: everywhere).
    pub targets: Vec<Option<String>>,
    /// Whether its own default features are on.
    pub default_features: DefaultFeatures,
    /// Where it comes from.
    pub origin: Origin,
}

impl Declared {
    /// Whether it is compiled on this machine's target.
    #[must_use]
    pub fn on_host(&self) -> bool {
        self.targets.iter().any(|t| t.as_deref().is_none_or(host_matches))
    }
}

/// Whether a `[target.X.dependencies]` key (`cfg(...)` or a triple) holds on
/// this machine. Anything it does not understand holds, so a dependency is
/// never hidden by a guess.
#[must_use]
pub fn host_matches(target: &str) -> bool {
    let target = target.trim();
    if let Some(inner) = target.strip_prefix("cfg(").and_then(|t| t.strip_suffix(')')) {
        return cfg_holds(inner.trim());
    }
    // A bare triple: it holds when it names this machine's architecture and
    // operating system; a triple that names neither is not understood.
    let os = if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS };
    let known = ["windows", "linux", "darwin", "android", "wasm", "freebsd"];
    if known.iter().any(|k| target.contains(k)) {
        return target.contains(os) && target.starts_with(std::env::consts::ARCH);
    }
    true
}

fn cfg_holds(expr: &str) -> bool {
    let expr = expr.trim();
    for (name, all) in [("all", true), ("any", false)] {
        if let Some(inner) = expr.strip_prefix(name).map(str::trim_start).and_then(|t| t.strip_prefix('(')).and_then(|t| t.strip_suffix(')')) {
            let parts = split_args(inner);
            return if all { parts.iter().all(|p| cfg_holds(p)) } else { parts.iter().any(|p| cfg_holds(p)) };
        }
    }
    if let Some(inner) = expr.strip_prefix("not").map(str::trim_start).and_then(|t| t.strip_prefix('(')).and_then(|t| t.strip_suffix(')')) {
        return !cfg_holds(inner);
    }
    match expr.split_once('=') {
        Some((key, value)) => {
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "target_os" => value == std::env::consts::OS,
                "target_family" => value == std::env::consts::FAMILY,
                "target_arch" => value == std::env::consts::ARCH,
                "target_pointer_width" => value == if cfg!(target_pointer_width = "64") { "64" } else { "32" },
                "target_endian" => value == if cfg!(target_endian = "little") { "little" } else { "big" },
                "target_vendor" => value == if cfg!(target_os = "macos") { "apple" } else { "unknown" },
                _ => true,
            }
        }
        None => match expr {
            "unix" => cfg!(unix),
            "windows" => cfg!(windows),
            "debug_assertions" => true,
            _ => true,
        },
    }
}

fn split_args(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut from = 0;
    for (at, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(inner[from..at].trim());
                from = at + 1;
            }
            _ => {}
        }
    }
    let tail = inner[from..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// What a manifest says.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Manifest {
    /// The package name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Its one-line description.
    pub description: Option<String>,
    /// Its SPDX licence expression.
    pub license: Option<String>,
    /// Authors, with addresses removed.
    pub authors: Vec<String>,
    /// Its repository.
    pub repository: Option<String>,
    /// Its homepage.
    pub homepage: Option<String>,
    /// Its documentation.
    pub documentation: Option<String>,
    /// Its edition.
    pub edition: Option<String>,
    /// The oldest compiler it supports.
    pub rust_version: Option<String>,
    /// Registry categories.
    pub categories: Vec<String>,
    /// Registry keywords.
    pub keywords: Vec<String>,
    /// The library file, relative to the crate root.
    pub lib: String,
    /// Whether it is a procedural macro.
    pub library: Library,
    /// Whether building it runs a build script.
    pub build: Build,
    /// Features turned on by default (the `default` list).
    pub default: Vec<String>,
    /// Every feature name, then every optional dependency that is an
    /// implicit feature.
    pub features: Vec<String>,
    /// What each feature switches on.
    pub graph: Vec<(String, FeatureNode)>,
    /// Optional dependencies the default features switch on.
    pub default_active: Vec<String>,
    /// Dependencies declared (never dev), in manifest order.
    pub dependencies: Vec<Declared>,
}

fn string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

/// The `[workspace.package]` table above `dir`, within four levels.
fn workspace_package(dir: &Path) -> Table {
    let mut at = dir.to_path_buf();
    for _ in 0..4 {
        if !at.pop() {
            break;
        }
        if let Ok(text) = fs::read_to_string(at.join("Cargo.toml"))
            && let Ok(table) = text.parse::<Table>()
            && let Some(workspace) = table.get("workspace").and_then(Value::as_table)
        {
            return workspace.get("package").and_then(Value::as_table).cloned().unwrap_or_default();
        }
    }
    Table::new()
}

/// `[package] field`, following `field.workspace = true` to the workspace.
fn field(package: &Table, name: &str, dir: &Path, workspace: &mut Option<Table>) -> Option<Value> {
    let value = package.get(name)?;
    if value.as_table().is_some_and(|t| t.get("workspace").and_then(Value::as_bool) == Some(true)) {
        let workspace = workspace.get_or_insert_with(|| workspace_package(dir));
        return workspace.get(name).cloned();
    }
    Some(value.clone())
}

fn clean_author(author: &str) -> String {
    // "Name <mail>" and an unterminated "Name <mail", then bare addresses.
    let mut out = String::new();
    let mut in_angle = false;
    for ch in author.chars() {
        match ch {
            '<' => in_angle = true,
            '>' => in_angle = false,
            _ if !in_angle => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().filter(|word| !word.contains('@')).collect::<Vec<_>>().join(" ")
}

/// Every dependency table: normal, dev and build, and the same under each `[target.*]`.
fn dependency_tables(manifest: &Table) -> Vec<(&'static str, Option<String>, &Table)> {
    fn tables<'a>(table: &'a Table, target: Option<&str>, out: &mut Vec<(&'static str, Option<String>, &'a Table)>) {
        for (kind, keys) in [("normal", ["dependencies", ""]), ("dev", ["dev-dependencies", "dev_dependencies"]), ("build", ["build-dependencies", "build_dependencies"])] {
            for key in keys.iter().filter(|k| !k.is_empty()) {
                if let Some(t) = table.get(*key).and_then(Value::as_table) {
                    out.push((kind, target.map(str::to_owned), t));
                }
            }
        }
    }
    let mut out = Vec::new();
    tables(manifest, None, &mut out);
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (name, target) in targets {
            if let Some(target) = target.as_table() {
                tables(target, Some(name), &mut out);
            }
        }
    }
    out
}

fn declarations(manifest: &Table) -> Vec<Declared> {
    let workspace_deps = |dir_manifest: &Table, key: &str| -> Option<Value> {
        dir_manifest.get("workspace").and_then(Value::as_table)?.get("dependencies").and_then(Value::as_table)?.get(key).cloned()
    };
    let mut out: Vec<Declared> = Vec::new();
    for (kind, target, table) in dependency_tables(manifest) {
        if kind == "dev" {
            continue;
        }
        for (key, spec) in table {
            let mut spec: Table = match spec {
                Value::Table(t) => t.clone(),
                Value::String(v) => Table::from_iter([("version".to_owned(), Value::String(v.clone()))]),
                _ => Table::new(),
            };
            if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
                let mut base: Table = match workspace_deps(manifest, key) {
                    Some(Value::Table(t)) => t,
                    Some(Value::String(v)) => Table::from_iter([("version".to_owned(), Value::String(v))]),
                    _ => Table::new(),
                };
                for (k, v) in &spec {
                    if k != "workspace" {
                        base.insert(k.clone(), v.clone());
                    }
                }
                spec = base;
            }
            let need = if spec.get("optional").and_then(Value::as_bool) == Some(true) { Need::Optional } else { Need::Required };
            let origin = if spec.get("path").is_some() { Origin::Path } else { Origin::Registry };
            let default_features = if spec.get("default-features").or_else(|| spec.get("default_features")).and_then(Value::as_bool) == Some(false) { DefaultFeatures::Off } else { DefaultFeatures::On };
            let stage = if kind == "build" { Stage::Build } else { Stage::Runtime };
            let package = string(spec.get("package")).unwrap_or_else(|| key.clone());
            let req = string(spec.get("version")).unwrap_or_else(|| "*".to_owned());
            if let Some(found) = out.iter_mut().find(|d| &d.key == key) {
                if need == Need::Optional {
                    found.need = Need::Optional;
                }
                if stage == Stage::Runtime {
                    found.stage = Stage::Runtime;
                }
                if !found.targets.contains(&target) {
                    found.targets.push(target.clone());
                }
            } else {
                out.push(Declared { key: key.clone(), package, req, need, stage, targets: vec![target.clone()], default_features, origin });
            }
        }
    }
    out
}

/// The feature graph of `[features]` and the optional dependencies (see the
/// board's docstring: `enables`, `deps`, `default`), and the names in order.
fn graph(manifest: &Table, decls: &[Declared]) -> (Vec<String>, Vec<String>, Vec<(String, FeatureNode)>, Vec<String>) {
    let feats: Vec<(String, Vec<String>)> = manifest
        .get("features")
        .and_then(Value::as_table)
        .map(|t| t.iter().filter_map(|(k, v)| v.as_array().map(|list| (k.clone(), list.iter().filter_map(Value::as_str).map(str::to_owned).collect()))).collect())
        .unwrap_or_default();
    let feature = |name: &str| feats.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let optional: Vec<&str> = decls.iter().filter(|d| d.need == Need::Optional).map(|d| d.key.as_str()).collect();
    let referenced: Vec<&str> = feats.iter().flat_map(|(_, v)| v.iter()).filter_map(|e| e.strip_prefix("dep:")).collect();
    let implicit: Vec<&str> = optional.iter().copied().filter(|k| feature(k).is_none() && !referenced.contains(k)).collect();
    let mut nodes: Vec<(String, FeatureNode)> = Vec::new();
    for (name, entries) in &feats {
        if name == "default" {
            continue;
        }
        let mut node = FeatureNode::default();
        for entry in entries {
            if let Some(dep) = entry.strip_prefix("dep:") {
                if !node.deps.iter().any(|d| d == dep) {
                    node.deps.push(dep.to_owned());
                }
            } else if let Some((dep, _)) = entry.split_once('/') {
                // `x?/feat` is weak and turns nothing on; `x/feat` turns `x` on when optional.
                if !dep.ends_with('?') && optional.contains(&dep) && !node.deps.iter().any(|d| d == dep) {
                    node.deps.push(dep.to_owned());
                }
            } else if feature(entry).is_some() {
                if entry != "default" && !node.enables.contains(entry) {
                    node.enables.push(entry.clone());
                }
            } else if optional.contains(&entry.as_str()) && !node.deps.contains(entry) {
                node.deps.push(entry.clone());
            }
        }
        nodes.push((name.clone(), node));
    }
    for key in &implicit {
        nodes.push(((*key).to_owned(), FeatureNode { enables: Vec::new(), deps: vec![(*key).to_owned()], enabled: Enabled::OnRequest }));
    }
    // The default set: reachable from `default` through `enables`; an
    // implicit feature is on when its optional dependency is activated.
    let mut on: Vec<String> = Vec::new();
    let mut active: Vec<String> = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::from(["default".to_owned()]);
    while let Some(name) = queue.pop_front() {
        for entry in feature(&name).into_iter().flatten() {
            if let Some(dep) = entry.strip_prefix("dep:") {
                active.push(dep.to_owned());
            } else if let Some((dep, _)) = entry.split_once('/') {
                if optional.contains(&dep) {
                    active.push(dep.to_owned());
                }
            } else if feature(entry).is_some() {
                if entry != "default" && !on.contains(entry) {
                    on.push(entry.clone());
                    queue.push_back(entry.clone());
                }
            } else if optional.contains(&entry.as_str()) {
                active.push(entry.clone());
            }
        }
    }
    for name in &on {
        if let Some((_, node)) = nodes.iter_mut().find(|(n, _)| n == name) {
            node.enabled = Enabled::ByDefault;
            active.extend(node.deps.iter().cloned());
        }
    }
    for key in &implicit {
        if active.iter().any(|a| a == key)
            && let Some((_, node)) = nodes.iter_mut().find(|(n, _)| n == key)
        {
            node.enabled = Enabled::ByDefault;
        }
    }
    let default = feature("default").cloned().unwrap_or_default();
    let mut names: Vec<String> = feats.iter().map(|(k, _)| k.clone()).filter(|k| k != "default").collect();
    names.extend(implicit.iter().map(|k| (*k).to_owned()));
    active.sort();
    active.dedup();
    (default, names, nodes, active)
}

impl Manifest {
    /// What building this crate compiles on this machine: dependencies that
    /// are not optional (or that its default features switch on), for this
    /// target, never the ones only tests use.
    #[must_use]
    pub fn compiled(&self, defaults: DefaultFeatures) -> Vec<&Declared> {
        self.dependencies
            .iter()
            .filter(|d| d.on_host() && (d.need == Need::Required || (defaults == DefaultFeatures::On && self.default_active.contains(&d.key))))
            .collect()
    }
}

/// Reads `dir/Cargo.toml`.
#[must_use]
pub fn read(dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let manifest: Table = text.parse().ok()?;
    let package = manifest.get("package").and_then(Value::as_table).cloned().unwrap_or_default();
    let mut workspace: Option<Table> = None;
    let mut get = |name: &str| field(&package, name, dir, &mut workspace);
    let authors: Vec<String> = get("authors")
        .and_then(|v| v.as_array().map(|list| list.iter().filter_map(Value::as_str).map(clean_author).filter(|a| !a.is_empty()).take(4).collect()))
        .unwrap_or_default();
    let listed = |value: Option<Value>| value.map(|v| strings(Some(&v))).unwrap_or_default();
    let (categories, keywords) = (listed(get("categories")), listed(get("keywords")));
    let edition = get("edition").and_then(|v| v.as_str().map(str::to_owned)).or_else(|| Some("2015".to_owned()));
    let lib = manifest.get("lib").and_then(Value::as_table);
    let build = package.get("build");
    let decls = declarations(&manifest);
    let (default, features, graph, default_active) = graph(&manifest, &decls);
    Some(Manifest {
        name: string(package.get("name")).unwrap_or_default(),
        version: get("version").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
        description: get("description").and_then(|v| string(Some(&v))),
        license: get("license").and_then(|v| string(Some(&v))),
        authors,
        repository: get("repository").and_then(|v| string(Some(&v))),
        homepage: get("homepage").and_then(|v| string(Some(&v))),
        documentation: get("documentation").and_then(|v| string(Some(&v))),
        edition,
        rust_version: get("rust-version").and_then(|v| v.as_str().map(str::to_owned)),
        categories,
        keywords,
        lib: lib.and_then(|l| string(l.get("path"))).unwrap_or_else(|| "src/lib.rs".to_owned()),
        library: if lib.is_some_and(|l| l.get("proc-macro").or_else(|| l.get("proc_macro")).and_then(Value::as_bool) == Some(true)) { Library::ProcMacro } else { Library::Plain },
        build: if (matches!(build, Some(Value::String(s)) if !s.is_empty()) || dir.join("build.rs").is_file()) && build.and_then(Value::as_bool) != Some(false) { Build::Script } else { Build::Plain },
        default,
        features,
        graph,
        default_active,
        dependencies: decls,
    })
}
