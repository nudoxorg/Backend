//! Immutable source-scoped module and call projection. Resolution is performed
//! once per distinct import, never once per call or declaration candidate.

use super::*;
use std::borrow::Cow;

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
        if is_plain_identifier(specifier) {
            self.push_suffix(&format!("{specifier}/__init__.py"), &mut out);
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

/// The callables and types one file declares, by name.
#[derive(Default)]
struct FileDeclarations<'a> {
    /// Functions that belong to no type: module-level and nested functions.
    free: BTreeMap<&'a str, BTreeSet<DeclarationId>>,
    /// Methods and associated functions, by the type they belong to.
    methods: BTreeMap<&'a str, BTreeMap<&'a str, BTreeSet<DeclarationId>>>,
    nominal: BTreeSet<&'a str>,
}

/// What the whole package declares, which is what the name match may lean on
/// when a receiver names no type: a method bound through an unknown receiver
/// must be the only callable of its name in the package.
#[derive(Default)]
struct PackageNames<'a> {
    /// Every callable of a name, and whether it is a method.
    callables: BTreeMap<&'a str, BTreeMap<DeclarationId, bool>>,
    /// The methods of each type of the package, by the type's name.
    methods: BTreeMap<&'a str, BTreeMap<&'a str, BTreeSet<DeclarationId>>>,
    nominal: BTreeSet<&'a str>,
}

/// Receivers that name the enclosing type itself.
const SELF_RECEIVERS: &[&str] = &["self", "Self", "this", "cls", "super", "base"];

/// Method names every standard library or framework defines on many types. A
/// receiver of unknown type calling one of these is far more likely to call
/// the library's method than the only project method of that name, so the
/// name match leaves them alone. Compared without case or underscores.
const UBIQUITOUS_METHOD_NAMES: &[&str] = &[
    "add", "append", "as_mut", "as_ref", "as_str", "borrow", "call", "clear", "clone", "close",
    "cmp", "collect", "contains", "copy", "decode", "default", "delete", "deref", "done", "drop",
    "encode", "endswith", "eq", "error", "expect", "extend", "filter", "find", "flush", "fmt",
    "foreach", "format", "from", "get", "hash", "includes", "index", "insert", "into", "is_empty",
    "items", "iter", "join", "keys", "len", "less", "lock", "lower", "map", "new", "next", "open",
    "partial_cmp", "pop", "push", "read", "remove", "replace", "run", "send", "set", "sort",
    "split", "startswith", "string", "strip", "swap", "to_owned", "to_string", "tostring", "trim",
    "unlock", "unwrap", "update", "upper", "values", "wait", "write",
];

fn is_ubiquitous_method_name(name: &str) -> bool {
    let normalized = name
        .bytes()
        .filter(|byte| *byte != b'_')
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    UBIQUITOUS_METHOD_NAMES
        .iter()
        .any(|known| known.bytes().filter(|byte| *byte != b'_').eq(normalized.iter().copied()))
}

/// Languages where a bare `name()` inside a type calls that type's own method.
fn bare_calls_reach_methods(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|extension| extension.to_str()),
        Some(
            "java"
                | "cs"
                | "kt"
                | "scala"
                | "swift"
                | "php"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "cxx"
                | "hh"
                | "hpp"
                | "hxx"
                | "m"
                | "mm"
        )
    )
}

/// A name written like a type (`Vec`, `HashMap`, `Entry`) and not like a
/// module, variable or function.
fn written_like_a_type(name: &str) -> bool {
    name.bytes().next().is_some_and(|byte| byte.is_ascii_uppercase())
}

/// What one file imports, joined once against the project's modules.
///
/// An import that leaves the project (or names nothing callable) is kept as
/// opaque: a call through it is a call into a foreign type or module, never
/// into a same-named project declaration.
///
/// Its lifetime is one exact file in one source projection: additions and
/// manifest changes cannot reuse either successful or negative module
/// resolutions.
#[derive(Default)]
struct ImportedCalls {
    #[cfg(test)]
    resolved_specifiers: usize,
    /// `from m import f [as g]`: a binding that is a project function.
    functions: BTreeMap<String, BTreeSet<DeclarationId>>,
    /// `import m [as q]`: a module binding and the functions it exposes.
    modules: BTreeMap<String, BTreeMap<String, BTreeSet<DeclarationId>>>,
    /// `from m import C [as D]`: a project type binding and its methods.
    classes: BTreeMap<String, BTreeMap<String, BTreeSet<DeclarationId>>>,
    /// Bindings from outside the project, or of something not callable.
    opaque: BTreeSet<String>,
}

/// What a call resolves against: the caller's file, the package, and where
/// the caller itself sits.
struct CallContext<'a, 'b> {
    local: &'b FileDeclarations<'a>,
    package: &'b PackageNames<'a>,
    /// The type or enclosing definition the calling declaration is inside.
    container: Option<&'a str>,
    /// Whether a bare `name()` can be a call of the container's own method.
    implicit_receiver: bool,
}

impl CallContext<'_, '_> {
    fn own_method(&self, name: &str) -> Option<&BTreeSet<DeclarationId>> {
        self.local.methods.get(self.container?)?.get(name)
    }

    /// The method `name` when it is the only callable of that name in the
    /// whole package, and not a name the standard library defines widely.
    fn only_method_of_that_name(&self, name: &str) -> Option<DeclarationId> {
        let candidates = self.package.callables.get(name)?;
        let mut candidates = candidates.iter();
        let (&id, &method) = candidates.next()?;
        (candidates.next().is_none() && method && !is_ubiquitous_method_name(name)).then_some(id)
    }
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
            let mut matched = false;
            for id in paths.iter().copied() {
                let file = &files[id.0];
                match &import.kind {
                    ImportBindingKind::Qualifier { .. } => {
                        matched = true;
                        let qualified = joined.modules.entry(import.local.clone()).or_default();
                        for (&name, targets) in &file.free {
                            qualified
                                .entry(name.to_owned())
                                .or_default()
                                .extend(targets);
                        }
                    }
                    ImportBindingKind::Value { exported, .. } => {
                        if file.nominal.contains(exported.as_str()) {
                            matched = true;
                            let methods = joined.classes.entry(import.local.clone()).or_default();
                            for (&name, targets) in
                                file.methods.get(exported.as_str()).into_iter().flatten()
                            {
                                methods.entry(name.to_owned()).or_default().extend(targets);
                            }
                        } else if let Some(targets) = file.free.get(exported.as_str()) {
                            matched = true;
                            joined
                                .functions
                                .entry(import.local.clone())
                                .or_default()
                                .extend(targets);
                        }
                    }
                }
            }
            if !matched {
                joined.opaque.insert(import.local);
            }
        }
        #[cfg(test)]
        {
            joined.resolved_specifiers = resolved.len();
        }
        joined
    }

    fn is_import(&self, local: &str) -> bool {
        self.functions.contains_key(local)
            || self.modules.contains_key(local)
            || self.classes.contains_key(local)
            || self.opaque.contains(local)
    }

    /// The one declaration a call names, or none.
    ///
    /// A call resolves only from what its text proves: the file's own
    /// functions, what the file imports, and the type or module its receiver
    /// is written as. A call through a receiver of unknown type binds only
    /// to the one method of that name the whole package declares, and never
    /// through a receiver that is foreign by its own spelling (an import
    /// from outside the project, a literal, a type no project file declares).
    fn target(&self, context: &CallContext<'_, '_>, call: &CallSite) -> Option<DeclarationId> {
        let name = call.name.as_str();
        match &call.receiver {
            CallReceiver::Bare => {
                if let Some(targets) = context.local.free.get(name) {
                    return unique_target(targets.iter().copied());
                }
                if context.implicit_receiver
                    && let Some(targets) = context.own_method(name)
                {
                    return unique_target(targets.iter().copied());
                }
                unique_target(self.functions.get(name)?.iter().copied())
            }
            CallReceiver::Expression { literal: true } => None,
            CallReceiver::Expression { literal: false } => context.only_method_of_that_name(name),
            CallReceiver::Path { root, last, depth } => {
                let root = root.as_str();
                // `Type::name()`, however the path to the type is written: the
                // methods the package declares on a type of that name.
                if (*depth > 1 || !SELF_RECEIVERS.contains(&root))
                    && written_like_a_type(last)
                    && context.package.nominal.contains(last.as_str())
                    && !self.is_import(root)
                    && !context.local.nominal.contains(root)
                    && let Some(targets) = context
                        .package
                        .methods
                        .get(last.as_str())
                        .and_then(|methods| methods.get(name))
                {
                    return unique_target(targets.iter().copied());
                }
                if *depth > 1 {
                    // `a.b.name()`: a field or a submodule path; only the
                    // receiver being foreign is known.
                    return (!self.is_import(root))
                        .then(|| context.only_method_of_that_name(name))
                        .flatten();
                }
                if SELF_RECEIVERS.contains(&root) {
                    if let Some(targets) = context.own_method(name) {
                        return unique_target(targets.iter().copied());
                    }
                    return context.only_method_of_that_name(name);
                }
                if let Some(names) = self.modules.get(root) {
                    return unique_target(names.get(name)?.iter().copied());
                }
                if let Some(names) = self.classes.get(root) {
                    return unique_target(names.get(name)?.iter().copied());
                }
                if self.is_import(root) {
                    return None;
                }
                if context.local.nominal.contains(root) {
                    return unique_target(
                        context.local.methods.get(root)?.get(name)?.iter().copied(),
                    );
                }
                if written_like_a_type(root) && !context.package.nominal.contains(root) {
                    return None;
                }
                context.only_method_of_that_name(name)
            }
        }
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
    /// The file each declaration is written in, indexed like `coordinates`.
    declaration_files: Vec<FileId>,
    file_paths: Vec<String>,
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
            declaration_files: Vec::new(),
            file_paths: files.iter().map(|file| file.path.to_owned()).collect(),
            by_coordinate: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
        };
        let mut package_names = PackageNames::default();
        let mut declarations = Vec::with_capacity(files.len());
        let mut callers = Vec::with_capacity(files.len());
        for (at, file) in files.iter().enumerate() {
            let containment =
                FileContainment::new(&project.label, file.path, project_key, &file.declarations);
            let mut lookup = FileDeclarations::default();
            // Types first, so a method written inside a class body is told
            // from a function nested in another function.
            for declaration in file.declarations.iter() {
                if declares_a_nominal_type(declaration.kind()) {
                    lookup.nominal.insert(declaration.name());
                    package_names.nominal.insert(declaration.name());
                }
            }
            let mut file_callers = Vec::new();
            for declaration in file.declarations.iter() {
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
                        graph.declaration_files.push(FileId(at));
                        id
                    });
                let container = match declaration.container() {
                    Container::Enclosing { name, .. } | Container::Attached { type_name: name } => {
                        Some(name.as_str())
                    }
                    Container::Module => None,
                };
                let method = matches!(
                    declaration.kind(),
                    DeclarationKind::Method | DeclarationKind::Constructor
                ) || matches!(declaration.container(), Container::Attached { .. })
                    || container.is_some_and(|name| lookup.nominal.contains(name));
                package_names
                    .callables
                    .entry(declaration.name())
                    .or_default()
                    .insert(id, method);
                match container {
                    Some(name) if method => {
                        package_names
                            .methods
                            .entry(name)
                            .or_default()
                            .entry(declaration.name())
                            .or_default()
                            .insert(id);
                        lookup
                            .methods
                            .entry(name)
                            .or_default()
                            .entry(declaration.name())
                            .or_default()
                            .insert(id);
                    }
                    _ => {
                        lookup
                            .free
                            .entry(declaration.name())
                            .or_default()
                            .insert(id);
                    }
                }
                if let Some(excerpt) = declaration.source_excerpt().text() {
                    file_callers.push((id, container, excerpt));
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
            let implicit_receiver = bare_calls_reach_methods(file.path);
            for &(caller, container, excerpt) in &callers[at] {
                let context = CallContext {
                    local: &declarations[at],
                    package: &package_names,
                    container,
                    implicit_receiver,
                };
                for call in structural_excerpt_call_sites(excerpt) {
                    let Some(callee) = imported.target(&context, &call) else {
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

    /// Every name-matched caller and callee coordinate pair.
    #[cfg(test)]
    pub(super) fn coordinate_pairs(&self) -> Vec<(String, String)> {
        self.coordinate_pairs_outside(&BTreeSet::new())
    }

    /// The name-matched pairs whose caller is written in a file the compiler
    /// did not cover. A file the compiler covers has its calls from resolved
    /// symbols; a guess from a name never stands beside them.
    pub(super) fn coordinate_pairs_outside(
        &self,
        covered: &BTreeSet<String>,
    ) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for (&caller, targets) in &self.outgoing {
            let caller_path = &self.file_paths[self.declaration_files[caller.0].0];
            if covered.contains(caller_path) {
                continue;
            }
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
    key: Option<SourceKey>,
    calls: ProjectCalls,
}

/// One bounded source projection owned by one command adapter. The checked
/// workspace root binds the manifest and complete file facts; fixture inputs
/// without that authenticated root are rebuilt on every request.
#[derive(Default)]
pub(crate) struct StructuralCallResidence {
    current: Option<CachedCalls>,
    #[cfg(test)]
    builds: usize,
}

impl StructuralCallResidence {
    pub(super) fn project_calls(
        &mut self,
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<&ProjectCalls, BuiltinModelError> {
        let key = sources.source_snapshot.as_ref().map(|source| SourceKey {
            workspace: source.workspace_root(),
            package,
        });
        let hit = key.is_some()
            && self
                .current
                .as_ref()
                .is_some_and(|current| current.key == key);
        if !hit {
            // Finish admission before replacing the previous exact projection.
            let calls = ProjectCalls::build(sources, package)?;
            self.current = Some(CachedCalls { key, calls });
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        Ok(&self
            .current
            .as_ref()
            .expect("projection admitted above")
            .calls)
    }

    /// The name-matched pairs whose caller is written outside `covered`, the
    /// files the compiler already answers for.
    pub(crate) fn coordinate_pairs_outside(
        &mut self,
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
        covered: &BTreeSet<String>,
    ) -> Result<Vec<(String, String)>, BuiltinModelError> {
        Ok(self
            .project_calls(sources, package)?
            .coordinate_pairs_outside(covered))
    }

    #[cfg(test)]
    pub(crate) fn coordinate_pairs(
        &mut self,
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<Vec<(String, String)>, BuiltinModelError> {
        Ok(self.project_calls(sources, package)?.coordinate_pairs())
    }

    #[cfg(test)]
    pub(crate) fn build_count(&self) -> usize {
        self.builds
    }
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
            StructuralCallResidence::default()
                .project_calls(&missing, package)
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
            StructuralCallResidence::default()
                .project_calls(&added, package)
                .map_err(|e| e.to_string())?
                .coordinate_pairs(),
            vec![(
                "fixture::one/caller.ts:2::caller".to_owned(),
                "fixture::one/target.ts:1::validate".to_owned()
            )]
        );
        Ok(())
    }

    /// The name-matched caller and callee names of source files, as
    /// `(caller, callee)` pairs of `name` strings.
    fn matched_names(
        files: &[(&str, backend_engine::SourceLanguage, &str)],
    ) -> Result<Vec<(String, String)>, String> {
        let mut analysed = Vec::new();
        for (path, language, text) in files {
            analysed.push((*path, *language, analyze_source(*language, path, text)?));
        }
        let (sources, package) = cross_file_sources(&analysed)?;
        let calls = ProjectCalls::build(&sources, package).map_err(|e| e.to_string())?;
        let name = |coordinate: String| coordinate.rsplit("::").next().unwrap_or("").to_owned();
        Ok(calls
            .coordinate_pairs()
            .into_iter()
            .map(|(caller, callee)| (name(caller), name(callee)))
            .collect())
    }

    #[test]
    fn a_receiver_is_read_whole_and_never_by_the_word_near_it() {
        let receiver = |excerpt: &str| {
            let at = excerpt.rfind("name(").expect("call");
            structural_call_receiver(excerpt, at)
        };
        assert_eq!(receiver("fn f() { name(); }"), CallReceiver::Bare);
        assert_eq!(
            receiver("fn f() { a.name(); }"),
            CallReceiver::Path { root: "a".to_owned(), last: "a".to_owned(), depth: 1 }
        );
        assert_eq!(
            receiver("fn f() { self.field.name(); }"),
            CallReceiver::Path { root: "self".to_owned(), last: "field".to_owned(), depth: 2 }
        );
        assert_eq!(
            receiver("fn f() { Type::name(); }"),
            CallReceiver::Path { root: "Type".to_owned(), last: "Type".to_owned(), depth: 1 }
        );
        assert_eq!(
            receiver("fn f() { a?.name(); }"),
            CallReceiver::Path { root: "a".to_owned(), last: "a".to_owned(), depth: 1 }
        );
        // The word before a call's closing parenthesis is not its receiver.
        assert_eq!(
            receiver("fn f() { foo(x).name(); }"),
            CallReceiver::Expression { literal: false }
        );
        assert_eq!(
            receiver("fn f() { xs[0].name(); }"),
            CallReceiver::Expression { literal: false }
        );
        assert_eq!(
            receiver("fn f() { make().inner.name(); }"),
            CallReceiver::Expression { literal: false }
        );
        assert_eq!(
            receiver("fn f() { \"text\".name(); }"),
            CallReceiver::Expression { literal: true }
        );
        assert_eq!(
            receiver("fn f() { 1.name(); }"),
            CallReceiver::Expression { literal: true }
        );
    }

    #[test]
    fn a_std_method_name_never_binds_to_the_one_project_method_of_that_name() -> Result<(), String> {
        use backend_engine::SourceLanguage::Rust;
        let pairs = matched_names(&[(
            "src/host.rs",
            Rust,
            "pub struct Host;\nimpl Host {\n    pub fn to_owned(&self) -> Host { Host }\n}\npub fn take(text: &str) -> String {\n    text.trim().to_owned()\n}\npub fn again(text: &str) -> String {\n    text.to_owned()\n}\n",
        )])?;
        assert!(pairs.is_empty(), "{pairs:?}");
        Ok(())
    }

    #[test]
    fn a_method_name_the_package_declares_twice_binds_nothing() -> Result<(), String> {
        use backend_engine::SourceLanguage::Rust;
        let pairs = matched_names(&[(
            "src/rows.rs",
            Rust,
            "pub struct Rows;\nimpl Rows {\n    pub fn release(&self) {}\n}\npub struct Stmt;\nimpl Stmt {\n    pub fn release(&self) {}\n}\npub fn drain(rows: &Rows) {\n    rows.release();\n}\n",
        )])?;
        assert!(pairs.is_empty(), "{pairs:?}");
        Ok(())
    }

    #[test]
    fn the_one_method_of_its_name_binds_through_a_receiver_of_unknown_type() -> Result<(), String> {
        use backend_engine::SourceLanguage::Rust;
        let pairs = matched_names(&[
            (
                "src/stmt.rs",
                Rust,
                "pub struct Stmt;\nimpl Stmt {\n    pub fn rebind_parameters(&self) {}\n}\n",
            ),
            (
                "src/run.rs",
                Rust,
                "pub fn run(stmt: &crate::stmt::Stmt) {\n    stmt.rebind_parameters();\n}\n",
            ),
        ])?;
        assert_eq!(pairs, vec![("run".to_owned(), "rebind_parameters".to_owned())]);
        Ok(())
    }

    #[test]
    fn a_literal_or_a_foreign_receiver_binds_nothing() -> Result<(), String> {
        use backend_engine::SourceLanguage::Rust;
        let pairs = matched_names(&[(
            "src/lib.rs",
            Rust,
            "pub struct Registry;\nimpl Registry {\n    pub fn rebind_parameters(&self) {}\n}\npub fn run() {\n    \"text\".rebind_parameters();\n    String::rebind_parameters();\n}\n",
        )])?;
        assert!(pairs.is_empty(), "{pairs:?}");
        Ok(())
    }

    #[test]
    fn a_qualified_call_through_a_foreign_module_never_binds_a_same_named_function(
    ) -> Result<(), String> {
        use backend_engine::SourceLanguage::Python;
        let pairs = matched_names(&[(
            "noxfile.py",
            Python,
            "import nox\n\ndef session(*args, **kwargs):\n    pass\n\ndef _session(fn):\n    return nox.session(python=3)(fn)\n",
        )])?;
        assert!(pairs.is_empty(), "{pairs:?}");
        Ok(())
    }

    #[test]
    fn a_bare_call_reaches_a_function_and_never_a_method_of_that_name() -> Result<(), String> {
        use backend_engine::SourceLanguage::Python;
        let pairs = matched_names(&[(
            "app.py",
            Python,
            "class Engine:\n    def render(self):\n        pass\n\ndef page():\n    render()\n",
        )])?;
        assert!(pairs.is_empty(), "{pairs:?}");
        Ok(())
    }

    #[test]
    fn self_and_a_local_type_reach_their_own_methods_even_when_the_name_repeats() -> Result<(), String> {
        use backend_engine::SourceLanguage::Rust;
        let pairs = matched_names(&[(
            "src/lib.rs",
            Rust,
            "pub struct A;\nimpl A {\n    pub fn open(&self) {}\n    pub fn use_it(&self) {\n        self.open();\n    }\n}\npub struct B;\nimpl B {\n    pub fn open(&self) {}\n}\npub fn go() {\n    B::open(&B);\n}\n",
        )])?;
        assert!(
            pairs.contains(&("use_it".to_owned(), "open".to_owned())),
            "{pairs:?}"
        );
        assert!(pairs.contains(&("go".to_owned(), "open".to_owned())), "{pairs:?}");
        assert_eq!(pairs.len(), 2, "{pairs:?}");
        Ok(())
    }

    #[test]
    fn a_file_the_compiler_covers_gives_no_name_matched_caller() -> Result<(), String> {
        use backend_engine::SourceLanguage::Rust;
        let helper = analyze_source(Rust, "src/helper.rs", "pub fn helper() {}\n")?;
        let covered = analyze_source(
            Rust,
            "src/covered.rs",
            "use crate::helper::helper;\npub fn covered() { helper(); }\n",
        )?;
        let open = analyze_source(
            Rust,
            "src/open.rs",
            "use crate::helper::helper;\npub fn open() { helper(); }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("src/helper.rs", Rust, helper),
            ("src/covered.rs", Rust, covered),
            ("src/open.rs", Rust, open),
        ])?;
        let calls = ProjectCalls::build(&sources, package).map_err(|e| e.to_string())?;
        assert_eq!(calls.coordinate_pairs().len(), 2);
        let outside = calls.coordinate_pairs_outside(&BTreeSet::from(["src/covered.rs".to_owned()]));
        assert_eq!(
            outside,
            vec![(
                "fixture::src/open.rs:2::open".to_owned(),
                "fixture::src/helper.rs:1::helper".to_owned()
            )]
        );
        Ok(())
    }
}
