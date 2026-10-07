//! Immutable source-scoped module and call projection. Resolution is performed
//! once per distinct import, never once per call or declaration candidate.

use super::*;
use std::borrow::Cow;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FileId(usize);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeclarationId(usize);

/// Keys borrow components from the admitted paths. Every suffix starts on a
/// path-component boundary, so `pkg/module.py` never matches `otherpkg/module.py`.
struct ModuleIndex<'a> {
    paths: Vec<&'a str>,
    exact: BTreeMap<&'a str, FileId>,
    stems: BTreeMap<&'a str, Vec<FileId>>,
    suffixes: BTreeMap<&'a str, Vec<FileId>>,
    go_packages: BTreeMap<&'a str, Vec<FileId>>,
}

fn component_suffixes(path: &str) -> impl Iterator<Item = &str> {
    std::iter::once(path).chain(path.match_indices('/').map(|(at, _)| &path[at + 1..]))
}

impl<'a> ModuleIndex<'a> {
    fn new(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut index = Self {
            paths: paths.into_iter().collect(),
            exact: BTreeMap::new(),
            stems: BTreeMap::new(),
            suffixes: BTreeMap::new(),
            go_packages: BTreeMap::new(),
        };
        for (at, &path) in index.paths.iter().enumerate() {
            let id = FileId(at);
            index.exact.insert(path, id);
            if let Some(stem) = Path::new(path).file_stem().and_then(|stem| stem.to_str()) {
                index.stems.entry(stem).or_default().push(id);
            }
            for suffix in component_suffixes(path) {
                index.suffixes.entry(suffix).or_default().push(id);
            }
            if path.ends_with(".go") {
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                for suffix in component_suffixes(parent) {
                    index.go_packages.entry(suffix).or_default().push(id);
                }
            }
        }
        index
    }

    fn push_exact(&self, path: &str, out: &mut BTreeSet<FileId>) {
        if let Some(&id) = self.exact.get(path) {
            out.insert(id);
        }
    }

    fn push_suffix(&self, path: &str, out: &mut BTreeSet<FileId>) {
        if let Some(ids) = self.suffixes.get(path) {
            out.extend(ids);
        }
    }

    fn push_candidate(&self, path: &str, out: &mut BTreeSet<FileId>) {
        let path = path.replace('\\', "/");
        self.push_exact(&path, out);
        for extension in SOURCE_EXTENSIONS.iter().chain(INDEX_EXTENSIONS) {
            self.push_exact(&format!("{path}{extension}"), out);
        }
        self.push_exact(&format!("{path}/mod.rs"), out);
    }

    fn resolve(&self, specifier: &str, caller_path: &str) -> BTreeSet<FileId> {
        let caller_dir = Path::new(caller_path).parent().unwrap_or(Path::new(""));
        let mut out = BTreeSet::new();
        if specifier.starts_with("./") || specifier.starts_with("../") {
            self.push_candidate(
                &normalize_relative_path(caller_dir, specifier).to_string_lossy(),
                &mut out,
            );
            return out;
        }
        if specifier.contains("::") {
            let mut remainder = specifier;
            let mut base = caller_dir;
            while remainder.starts_with("super::") {
                remainder = remainder.trim_start_matches("super::");
                base = base.parent().unwrap_or(Path::new(""));
            }
            if let Some(stripped) = remainder.strip_prefix("crate::") {
                remainder = stripped;
                base = Path::new("src");
            } else if let Some(stripped) = remainder.strip_prefix("self::") {
                remainder = stripped;
            }
            let path = remainder.replace("::", "/");
            self.push_candidate(&format!("src/{path}"), &mut out);
            self.push_candidate(&format!("src/{path}/mod"), &mut out);
            if !base.as_os_str().is_empty() {
                self.push_candidate(&base.join(&path).to_string_lossy(), &mut out);
                self.push_candidate(&base.join(&path).join("mod").to_string_lossy(), &mut out);
            }
            return out;
        }
        for extension in SOURCE_EXTENSIONS {
            let candidate = caller_dir.join(format!("{specifier}{extension}"));
            self.push_exact(&candidate.to_string_lossy().replace('\\', "/"), &mut out);
        }
        if let Some(ids) = self.stems.get(specifier) {
            out.extend(ids);
        }
        if is_dotted_module_specifier(specifier) {
            let module = specifier.replace('.', "/");
            self.push_suffix(&format!("{module}.py"), &mut out);
            self.push_suffix(&format!("{module}/__init__.py"), &mut out);
        }
        if is_go_import_specifier(specifier) {
            for suffix in component_suffixes(specifier) {
                if let Some(ids) = self.go_packages.get(suffix) {
                    out.extend(ids);
                    break;
                }
            }
        }
        if is_rust_module_path_specifier(specifier) {
            self.push_suffix(&format!("{specifier}.rs"), &mut out);
            self.push_suffix(&format!("{specifier}/mod.rs"), &mut out);
        }
        if is_c_family_path_specifier(specifier) {
            self.push_suffix(specifier, &mut out);
        }
        out
    }
}

struct FileFacts<'a> {
    path: &'a str,
    declarations: Cow<'a, [backend_compile::SourceDeclaration]>,
}

#[derive(Default)]
struct FileDeclarations<'a> {
    callable: BTreeMap<&'a str, BTreeSet<DeclarationId>>,
    methods: BTreeMap<&'a str, BTreeMap<&'a str, BTreeSet<DeclarationId>>>,
    nominal: BTreeSet<&'a str>,
}

/// Prejoined import targets, including empty resolutions. Its lifetime is one
/// exact file in one source projection: additions and manifest changes cannot
/// reuse either successful or negative module resolutions.
#[derive(Default)]
struct ImportedCalls {
    #[cfg(test)]
    resolved_specifiers: usize,
    unqualified: BTreeMap<String, BTreeSet<DeclarationId>>,
    qualified: BTreeMap<String, BTreeMap<String, BTreeSet<DeclarationId>>>,
    methods: BTreeMap<String, BTreeSet<DeclarationId>>,
}

impl ImportedCalls {
    fn join(
        imports: impl Iterator<Item = ImportBinding>,
        path: &str,
        modules: &ModuleIndex<'_>,
        files: &[FileDeclarations<'_>],
    ) -> Self {
        let mut joined = Self::default();
        let mut resolved = BTreeMap::<String, BTreeSet<FileId>>::new();
        for import in imports {
            let specifier = match &import.kind {
                ImportBindingKind::Qualifier { specifier }
                | ImportBindingKind::Value { specifier, .. } => specifier,
            };
            let paths = resolved
                .entry(specifier.clone())
                .or_insert_with(|| modules.resolve(specifier, path));
            for id in paths.iter().copied() {
                let file = &files[id.0];
                match &import.kind {
                    ImportBindingKind::Qualifier { .. } => {
                        let qualified = joined.qualified.entry(import.local.clone()).or_default();
                        for (&name, targets) in &file.callable {
                            qualified
                                .entry(name.to_owned())
                                .or_default()
                                .extend(targets);
                        }
                    }
                    ImportBindingKind::Value { exported, .. } => {
                        if file.nominal.contains(exported.as_str()) {
                            if let Some(methods) = file.methods.get(exported.as_str()) {
                                for (&name, targets) in methods {
                                    joined
                                        .methods
                                        .entry(name.to_owned())
                                        .or_default()
                                        .extend(targets);
                                }
                            }
                        } else if let Some(targets) = file.callable.get(exported.as_str()) {
                            joined
                                .unqualified
                                .entry(import.local.clone())
                                .or_default()
                                .extend(targets);
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        {
            joined.resolved_specifiers = resolved.len();
        }
        joined
    }

    fn target(&self, local: &FileDeclarations<'_>, call: &CallSite) -> Option<DeclarationId> {
        if let Some(targets) = local.callable.get(call.name.as_str()) {
            return unique_target(targets.iter().copied());
        }
        let methods = self.methods.get(&call.name).into_iter().flatten().copied();
        let imported = match &call.qualifier {
            Some(qualifier) => self
                .qualified
                .get(qualifier)
                .and_then(|names| names.get(&call.name)),
            None => self.unqualified.get(&call.name),
        };
        unique_target(methods.chain(imported.into_iter().flatten().copied()))
    }
}

/// Candidate union can distinguish absence, one identity and ambiguity without
/// allocating a set for every call. Repeated candidates with the same interned
/// coordinate remain one identity, as in the original coordinate-set join.
fn unique_target(mut targets: impl Iterator<Item = DeclarationId>) -> Option<DeclarationId> {
    let first = targets.next()?;
    targets.all(|target| target == first).then_some(first)
}

pub(super) struct ProjectCalls {
    #[cfg(test)]
    resolved_specifiers: usize,
    coordinates: Vec<String>,
    by_coordinate: BTreeMap<String, DeclarationId>,
    outgoing: BTreeMap<DeclarationId, BTreeSet<DeclarationId>>,
    incoming: BTreeMap<DeclarationId, BTreeSet<DeclarationId>>,
}

impl ProjectCalls {
    fn build(
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<Self, BuiltinModelError> {
        let project = sources
            .projects
            .values()
            .find(|project| project.package == package)
            .ok_or_else(|| {
                BuiltinModelError(
                    "structural call graph package is absent from indexed sources".to_owned(),
                )
            })?;
        let project_key = package.to_bytes();
        let mut files = Vec::new();
        for (_, record) in &sources.files {
            let file = record
                .file_fields()
                .ok_or_else(|| BuiltinModelError("expected structural source file".to_owned()))?;
            if file.project != project_key {
                continue;
            }
            // Legacy inline facts can be borrowed. Paged complete declarations
            // are admitted against the same selected closure, never silently
            // projected from an overflow row's compact prefix.
            let declarations = if sources.source_snapshot.is_none() && file.retention.is_complete()
            {
                Cow::Borrowed(file.declarations.as_ref())
            } else {
                Cow::Owned(complete_declarations_for_file(sources, record)?)
            };
            files.push(FileFacts {
                path: file.path,
                declarations,
            });
        }
        let modules = ModuleIndex::new(files.iter().map(|file| file.path));
        let mut graph = Self {
            #[cfg(test)]
            resolved_specifiers: 0,
            coordinates: Vec::new(),
            by_coordinate: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
        };
        let mut declarations = Vec::with_capacity(files.len());
        let mut callers = Vec::with_capacity(files.len());
        for file in &files {
            let containment =
                FileContainment::new(&project.label, file.path, project_key, &file.declarations);
            let mut lookup = FileDeclarations::default();
            let mut file_callers = Vec::new();
            for declaration in file.declarations.iter() {
                if declares_a_nominal_type(declaration.kind()) {
                    lookup.nominal.insert(declaration.name());
                }
                if !structural_callable(declaration.kind()) {
                    continue;
                }
                let coordinate = containment.coordinate(declaration);
                let id = *graph
                    .by_coordinate
                    .entry(coordinate.clone())
                    .or_insert_with(|| {
                        let id = DeclarationId(graph.coordinates.len());
                        graph.coordinates.push(coordinate);
                        id
                    });
                lookup
                    .callable
                    .entry(declaration.name())
                    .or_default()
                    .insert(id);
                match declaration.container() {
                    Container::Enclosing { name, .. } | Container::Attached { type_name: name } => {
                        lookup
                            .methods
                            .entry(name)
                            .or_default()
                            .entry(declaration.name())
                            .or_default()
                            .insert(id);
                    }
                    Container::Module => {}
                }
                if let Some(excerpt) = declaration.source_excerpt().text() {
                    file_callers.push((id, excerpt));
                }
            }
            declarations.push(lookup);
            callers.push(file_callers);
        }
        for (at, file) in files.iter().enumerate() {
            let imported = ImportedCalls::join(
                file.declarations.iter().filter_map(parse_import_binding),
                file.path,
                &modules,
                &declarations,
            );
            #[cfg(test)]
            {
                graph.resolved_specifiers += imported.resolved_specifiers;
            }
            for &(caller, excerpt) in &callers[at] {
                for call in structural_excerpt_call_sites(excerpt) {
                    let Some(callee) = imported.target(&declarations[at], &call) else {
                        continue;
                    };
                    if caller == callee {
                        continue;
                    }
                    graph.outgoing.entry(caller).or_default().insert(callee);
                    graph.incoming.entry(callee).or_default().insert(caller);
                }
            }
        }
        Ok(graph)
    }

    pub(super) fn coordinate_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for (&caller, targets) in &self.outgoing {
            for &callee in targets {
                pairs.push((
                    self.coordinates[caller.0].clone(),
                    self.coordinates[callee.0].clone(),
                ));
            }
        }
        pairs.sort_unstable();
        pairs
    }

    pub(super) fn neighborhood<'a>(
        &'a self,
        coordinate: &str,
        include_incoming: bool,
    ) -> impl Iterator<Item = (&'a str, &'a str)> {
        let source = self.by_coordinate.get(coordinate).copied();
        let outgoing = source.into_iter().flat_map(move |source| {
            self.outgoing
                .get(&source)
                .into_iter()
                .flatten()
                .map(move |target| (source, *target))
        });
        let incoming = source
            .filter(|_| include_incoming)
            .into_iter()
            .flat_map(move |source| {
                self.incoming
                    .get(&source)
                    .into_iter()
                    .flatten()
                    .map(move |caller| (*caller, source))
            });
        outgoing.chain(incoming).map(|(caller, callee)| {
            (
                self.coordinates[caller.0].as_str(),
                self.coordinates[callee.0].as_str(),
            )
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct SourceKey {
    workspace: backend_engine::WorkspaceRoot,
    package: backend_engine::PackageKey,
}

struct CachedCalls {
    key: SourceKey,
    calls: Arc<ProjectCalls>,
}
static CALLS: OnceLock<Mutex<Option<CachedCalls>>> = OnceLock::new();

/// One retained project, keyed by the checked workspace root (including its
/// manifest and complete file-facts relation) and package. Fixture sources
/// without an authenticated selected root bypass residence entirely.
pub(super) fn project_calls(
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
) -> Result<Arc<ProjectCalls>, BuiltinModelError> {
    let key = sources.source_snapshot.as_ref().map(|source| SourceKey {
        workspace: source.workspace_root(),
        package,
    });
    let cache = CALLS.get_or_init(|| Mutex::new(None));
    if let Some(key) = key {
        let resident = cache
            .lock()
            .map_err(|_| BuiltinModelError("structural calls residence is poisoned".to_owned()))?;
        if let Some(resident) = resident.as_ref().filter(|resident| resident.key == key) {
            return Ok(Arc::clone(&resident.calls));
        }
    }
    let calls = Arc::new(ProjectCalls::build(sources, package)?);
    if let Some(key) = key {
        *cache.lock().map_err(|_| {
            BuiltinModelError("structural calls residence is poisoned".to_owned())
        })? = Some(CachedCalls {
            key,
            calls: Arc::clone(&calls),
        });
    }
    Ok(calls)
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{analyze_source, cross_file_sources};
    use super::*;

    #[test]
    fn indexed_module_resolution_preserves_paths_and_ambiguity() {
        let paths = [
            "root_a/mealie/routes/validators/validators.py",
            "root_b/mealie/routes/validators/validators.py",
            "root_a/mealie/routes/validators/__init__.py",
            "othermealie/routes/validators/validators.py",
            "src/local.py",
            "src/view.ts",
            "src/view/index.ts",
            "src/mod.rs",
            "src/view.rs",
            "src/view/mod.rs",
            "one/go/pkg/a.go",
            "two/go/pkg/b.go",
            "two/otherpkg/c.go",
            "include/lib/header.h",
            "vendor/lib/header.h",
            "decoyheader.h",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        let index = ModuleIndex::new(paths.iter().map(String::as_str));
        for caller in [
            "src/caller.py",
            "root_a/mealie/routes/caller.py",
            "caller.ts",
        ] {
            for specifier in [
                "mealie.routes.validators.validators",
                "mealie.routes.validators",
                "validators",
                "./view",
                "../view",
                "./local",
                "crate::view",
                "self::view",
                "super::view",
                "view",
                "github.com/go/pkg",
                "go/pkg",
                "lib/header.h",
                "absent.module",
                "./absent",
            ] {
                let actual = index
                    .resolve(specifier, caller)
                    .into_iter()
                    .map(|id| index.paths[id.0].to_owned())
                    .collect::<BTreeSet<_>>();
                assert_eq!(
                    actual,
                    resolve_specifier_paths(specifier, caller, &paths),
                    "{caller}: {specifier}"
                );
            }
        }
        assert_eq!(
            index
                .resolve("mealie.routes.validators.validators", "src/caller.py")
                .len(),
            2,
            "separate roots remain ambiguous; suffix component boundaries exclude othermealie"
        );
    }

    #[test]
    fn imported_calls_resolve_once_and_keep_alias_namespace_ambiguity() -> Result<(), String> {
        use backend_engine::SourceLanguage::Python;
        let callee = analyze_source(
            Python,
            "mealie/routes/validators/validators.py",
            "def validate_recipe():\n    pass\n",
        )?;
        let mut body = String::from(
            "from mealie.routes.validators.validators import validate_recipe as validate\nimport mealie.routes.validators.validators as rules\nfrom missing.module import unused\n",
        );
        for at in 0..120 {
            body.push_str(&format!(
                "def caller_{at}():\n    validate()\n    rules.validate_recipe()\n    validate()\n"
            ));
        }
        let caller = analyze_source(Python, "mealie/routes/recipes.py", &body)?;
        let (sources, package) = cross_file_sources(&[
            (
                "mealie/routes/validators/validators.py",
                Python,
                callee.clone(),
            ),
            ("mealie/routes/recipes.py", Python, caller.clone()),
        ])?;
        let calls = ProjectCalls::build(&sources, package).map_err(|e| e.to_string())?;
        assert_eq!(
            calls.resolved_specifiers, 2,
            "one lookup for the repeated alias/namespace specifier plus one negative result"
        );
        let pairs = calls.coordinate_pairs();
        assert_eq!(pairs.len(), 120);
        assert!(pairs.iter().all(|(_, target)| target
            == "fixture::mealie/routes/validators/validators.py:1::validate_recipe"));
        assert_eq!(
            calls
                .neighborhood(
                    "fixture::mealie/routes/validators/validators.py:1::validate_recipe",
                    true
                )
                .count(),
            120
        );
        assert_eq!(
            calls
                .neighborhood(
                    "fixture::mealie/routes/validators/validators.py:1::validate_recipe",
                    false
                )
                .count(),
            0
        );
        let (ambiguous, package) = cross_file_sources(&[
            (
                "mealie/routes/validators/validators.py",
                Python,
                callee.clone(),
            ),
            (
                "second/mealie/routes/validators/validators.py",
                Python,
                callee,
            ),
            ("mealie/routes/recipes.py", Python, caller),
        ])?;
        assert!(
            ProjectCalls::build(&ambiguous, package)
                .map_err(|e| e.to_string())?
                .coordinate_pairs()
                .is_empty(),
            "adding a same-name module creates ambiguity, never a cross-root edge"
        );
        println!(
            "structural_call_work_counts: callers=120, repeated_call_sites=360, imported_specifier_resolutions={}, emitted_pairs={}",
            calls.resolved_specifiers,
            pairs.len()
        );
        Ok(())
    }

    #[test]
    fn relative_alias_negative_resolution_rebuilds_after_module_addition() -> Result<(), String> {
        use backend_engine::SourceLanguage::TypeScript;
        let caller = analyze_source(
            TypeScript,
            "one/caller.ts",
            "import { validate as check } from './target';\nexport function caller() { check(); }\n",
        )?;
        let target = analyze_source(
            TypeScript,
            "one/target.ts",
            "export function validate() {}\n",
        )?;
        let decoy = analyze_source(
            TypeScript,
            "two/target.ts",
            "export function validate() {}\n",
        )?;
        let (missing, package) = cross_file_sources(&[
            ("one/caller.ts", TypeScript, caller.clone()),
            ("two/target.ts", TypeScript, decoy.clone()),
        ])?;
        assert!(
            project_calls(&missing, package)
                .map_err(|e| e.to_string())?
                .coordinate_pairs()
                .is_empty()
        );
        let (added, package) = cross_file_sources(&[
            ("one/caller.ts", TypeScript, caller),
            ("two/target.ts", TypeScript, decoy),
            ("one/target.ts", TypeScript, target),
        ])?;
        assert_eq!(
            project_calls(&added, package)
                .map_err(|e| e.to_string())?
                .coordinate_pairs(),
            vec![(
                "fixture::one/caller.ts:2::caller".to_owned(),
                "fixture::one/target.ts:1::validate".to_owned()
            )]
        );
        Ok(())
    }
}
