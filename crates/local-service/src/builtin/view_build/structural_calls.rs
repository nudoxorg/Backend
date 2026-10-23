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
        if let Some(rest) = project_alias_rest(specifier) {
            for root in PROJECT_ALIAS_ROOTS {
                let base = if root.is_empty() {
                    rest.to_owned()
                } else {
                    format!("{root}/{rest}")
                };
                self.push_candidate(&base, &mut out);
            }
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
    key: [u8; 32],
    path: &'a str,
    language: backend_engine::SourceLanguage,
    declarations: Cow<'a, [backend_compile::SourceDeclaration]>,
}

#[derive(Default)]
struct FileDeclarations<'a> {
    require_exact_imports: bool,
    scope_parents: BTreeMap<backend_compile::SourceBinding, Option<backend_compile::SourceBinding>>,
    parameter_names: BTreeMap<backend_compile::SourceBinding, BTreeSet<String>>,
    unknown_parameters: BTreeSet<backend_compile::SourceBinding>,
    scoped_names:
        BTreeMap<&'a str, BTreeMap<Option<backend_compile::SourceBinding>, ScopedCallName>>,
    callable: BTreeMap<&'a str, BTreeSet<DeclarationId>>,
    methods: BTreeMap<&'a str, BTreeMap<&'a str, BTreeSet<DeclarationId>>>,
    nominal: BTreeSet<&'a str>,
}

#[derive(Default)]
struct ScopedCallName {
    bindings: usize,
    targets: BTreeSet<DeclarationId>,
    import: Option<backend_compile::SourceBinding>,
}

enum LocalCallTarget {
    Unbound,
    Bound(DeclarationId),
    Imported(backend_compile::SourceBinding),
    Unresolved,
}

impl FileDeclarations<'_> {
    fn parameter_shadows(&self, caller: backend_compile::SourceBinding, name: &str) -> bool {
        let mut scope = Some(caller);
        for _ in 0..256 {
            let Some(binding) = scope else { return false; };
            if self.unknown_parameters.contains(&binding)
                || self.parameter_names.get(&binding).is_some_and(|names| names.contains(name)) {
                return true;
            }
            // A nearer captured local/import binding cuts off outer parameters.
            if self.scoped_names.get(name).is_some_and(|scopes| scopes.contains_key(&scope)) {
                return false;
            }
            let Some(parent) = self.scope_parents.get(&binding) else { return true; };
            scope = *parent;
        }
        true
    }

    /// The nearest captured lexical scope that binds this name wins. An
    /// observed noncallable/ambiguous binding shadows farther declarations and
    /// imports. This remains syntactic evidence, not checker resolution.
    fn scoped_target(&self, caller: backend_compile::SourceBinding, name: &str) -> LocalCallTarget {
        let Some(scopes) = self.scoped_names.get(name) else {
            return LocalCallTarget::Unbound;
        };
        let mut scope = Some(caller);
        for _ in 0..256 {
            if let Some(bound) = scopes.get(&scope) {
                if bound.bindings == 1 {
                    if let Some(binding) = bound.import {
                        return LocalCallTarget::Imported(binding);
                    }
                }
                return match (bound.bindings, unique_target(bound.targets.iter().copied())) {
                    (1, Some(target)) => LocalCallTarget::Bound(target),
                    _ => LocalCallTarget::Unresolved,
                };
            }
            let Some(binding) = scope else {
                return LocalCallTarget::Unbound;
            };
            let Some(parent) = self.scope_parents.get(&binding) else {
                return LocalCallTarget::Unresolved;
            };
            scope = *parent;
        }
        LocalCallTarget::Unresolved
    }
}

/// Prejoined import targets, including empty resolutions. Its lifetime is one
/// exact file in one source projection: additions and manifest changes cannot
/// reuse either successful or negative module resolutions.
#[derive(Default)]
struct ImportedCalls {
    #[cfg(test)]
    resolved_specifiers: usize,
    // Name-only resolution belongs to legacy imports. Exact imports must be
    // selected through the nearest captured lexical binding first.
    unqualified: BTreeMap<String, BTreeSet<DeclarationId>>,
    exact_unqualified: BTreeMap<backend_compile::SourceBinding, BTreeSet<DeclarationId>>,
    exact_members: BTreeMap<backend_compile::SourceBinding, BTreeMap<String, BTreeSet<DeclarationId>>>,
    qualified: BTreeMap<String, BTreeMap<String, BTreeSet<DeclarationId>>>,
    classes: BTreeMap<String, BTreeMap<String, BTreeSet<DeclarationId>>>,
}

impl ImportedCalls {
    fn join(
        imports: impl Iterator<Item = (ImportBinding, Option<backend_compile::SourceBinding>)>,
        path: &str,
        modules: &ModuleIndex<'_>,
        files: &[FileDeclarations<'_>],
    ) -> Self {
        let mut joined = Self::default();
        let mut resolved = BTreeMap::<String, BTreeSet<FileId>>::new();
        for (import, binding) in imports {
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
                            if let Some(binding) = binding {
                                joined.exact_members.entry(binding).or_default()
                                    .entry(name.to_owned()).or_default().extend(targets);
                            }
                        }
                    }
                    ImportBindingKind::Value { exported, .. } => {
                        if file.nominal.contains(exported.as_str()) {
                            if let Some(methods) = file.methods.get(exported.as_str()) {
                                for (&name, targets) in methods {
                                    joined
                                        .classes
                                        .entry(import.local.clone())
                                        .or_default()
                                        .entry(name.to_owned())
                                        .or_default()
                                        .extend(targets);
                                    if let Some(binding) = binding {
                                        joined.exact_members.entry(binding).or_default()
                                            .entry(name.to_owned()).or_default().extend(targets);
                                    }
                                }
                            }
                        } else if let Some(targets) = file.callable.get(exported.as_str()) {
                            if let Some(binding) = binding {
                                joined
                                    .exact_unqualified
                                    .entry(binding)
                                    .or_default()
                                    .extend(targets);
                            } else {
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
        }
        #[cfg(test)]
        {
            joined.resolved_specifiers = resolved.len();
        }
        joined
    }

    fn target(
        &self,
        local: &FileDeclarations<'_>,
        call: &CallSite,
        exact_caller: Option<backend_compile::SourceBinding>,
    ) -> Option<DeclarationId> {
        let written_binding = match &call.receiver {
            CallReceiver::Bare => Some(call.name.as_str()),
            CallReceiver::Path { root, .. } => Some(root.as_str()),
            CallReceiver::Expression { .. } => None,
        };
        if let (Some(caller), Some(name)) = (exact_caller, written_binding)
            && local.parameter_shadows(caller, name) { return None; }
        match &call.receiver {
            CallReceiver::Bare => {
                if let Some(caller) = exact_caller {
                    match local.scoped_target(caller, &call.name) {
                        LocalCallTarget::Bound(target) => return Some(target),
                        LocalCallTarget::Imported(binding) => {
                            return unique_target(
                                self.exact_unqualified.get(&binding)?.iter().copied(),
                            );
                        }
                        LocalCallTarget::Unresolved => return None,
                        LocalCallTarget::Unbound => {}
                    }
                } else if let Some(targets) = local.callable.get(call.name.as_str()) {
                    // A legacy file has no exact scope proof. Methods are not
                    // free functions merely because they share this name.
                    let methods = local
                        .methods
                        .values()
                        .filter_map(|names| names.get(call.name.as_str()))
                        .flatten()
                        .copied()
                        .collect::<BTreeSet<_>>();
                    return unique_target(
                        targets
                            .iter()
                            .copied()
                            .filter(|target| !methods.contains(target)),
                    );
                }
                if local.require_exact_imports { return None; }
                unique_target(self.unqualified.get(&call.name)?.iter().copied())
            }
            CallReceiver::Path { root, depth: 1, .. } => {
                if local.require_exact_imports {
                    if let Some(caller) = exact_caller
                        && let LocalCallTarget::Imported(binding) = local.scoped_target(caller, root) {
                        return unique_target(self.exact_members.get(&binding)?
                            .get(&call.name)?.iter().copied());
                    }
                } else if let Some(names) = self.qualified.get(root).or_else(|| self.classes.get(root)) {
                    return unique_target(names.get(&call.name)?.iter().copied());
                }
                // An exact written type name can select its own associated
                // method. Arbitrary receivers never inherit that type by name.
                if local.nominal.contains(root.as_str()) {
                    return unique_target(
                        local
                            .methods
                            .get(root.as_str())?
                            .get(call.name.as_str())?
                            .iter()
                            .copied(),
                    );
                }
                None
            }
            CallReceiver::Path { .. } | CallReceiver::Expression { .. } => None,
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

/// Exact binding key belongs to one admitted source file/version. The cached
/// projection's workspace root supplies the version witness for every key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct StructuralCallBinding {
    pub(crate) file: [u8; 32],
    pub(crate) binding: backend_compile::SourceBinding,
}

/// Call endpoints retain their authority precision. Exact row IDs come from
/// the shared structural identity plan; legacy coordinates retain the older
/// synthetic first-label/last-label presentation policy.
#[derive(Clone, Debug)]
pub(crate) enum StructuralCallEndpoint {
    Binding {
        source: StructuralCallBinding,
        row: RowId,
        location: backend_compile::SourceLocation,
        semantic_site_unique: bool,
        name: String,
    },
    LegacyCoordinate(String),
}

// Project metadata shares the semantic query projection budget, not the
// 256-row bound on a single product reply.
const MAX_STRUCTURAL_REFERENCE_METADATA_ITEMS: usize = super::super::MAX_SEMANTIC_QUERY_ROWS;
const MAX_STRUCTURAL_REFERENCE_PARENT_DEPTH: usize = 256;
const MAX_STRUCTURAL_REFERENCE_CONTAINING_OWNERS: usize = MAX_STRUCTURAL_REFERENCE_PARENT_DEPTH;

/// Exact captured owners from the same selected file facts as the call graph.
/// Graph edges retain enclosing callable context; reference occurrences use
/// the nearest declaration, including variable initializers.
pub(crate) struct StructuralReferenceOwners {
    retained: usize,
    limit: usize,
    files: BTreeMap<[u8; 32], ReferenceFileOwners>,
    calls: BTreeMap<(StructuralCallBinding, StructuralCallBinding), CapturedCallSpans>,
}

struct CapturedCallSpans {
    excerpt_digest: [u8; 32],
    spans: BTreeSet<(usize, usize)>,
}

struct ReferenceOwner {
    capture: backend_compile::SourceBindingCapture,
    parent: Option<backend_compile::SourceBinding>,
    callable: bool,
    endpoint: StructuralCallEndpoint,
}

/// A start-sorted interval index with subtree maximum ends. Queries prune
/// irrelevant ranges and enumerate only containing owners, rightmost first.
/// Both storage and tree height are bounded by the admitted metadata count.
#[derive(Default)]
struct ReferenceFileOwners {
    owners: Vec<ReferenceOwner>,
    by_binding: BTreeMap<backend_compile::SourceBinding, usize>,
    leaf_base: usize,
    max_end: Vec<u32>,
}

impl ReferenceFileOwners {
    fn finish(&mut self) -> Result<(), BuiltinModelError> {
        if self.owners.len() > MAX_STRUCTURAL_REFERENCE_METADATA_ITEMS {
            return Err(BuiltinModelError(
                "structural reference interval index exceeds its owner bound".to_owned(),
            ));
        }
        self.owners.sort_by_key(|owner| {
            let (start, end) = owner.capture.owner();
            (start, std::cmp::Reverse(end), owner.capture.binding())
        });
        self.by_binding.clear();
        self.leaf_base = self.owners.len().max(1).next_power_of_two();
        let nodes = self.leaf_base * 2;
        let mut max_end = Vec::new();
        max_end.try_reserve_exact(nodes).map_err(|_| {
            BuiltinModelError("structural reference interval index allocation failed".to_owned())
        })?;
        max_end.resize(nodes, 0);
        for (at, owner) in self.owners.iter().enumerate() {
            if self
                .by_binding
                .insert(owner.capture.binding(), at)
                .is_some()
            {
                return Err(BuiltinModelError(
                    "structural reference interval index repeats a binding".to_owned(),
                ));
            }
            max_end[self.leaf_base + at] = owner.capture.owner().1;
        }
        for node in (1..self.leaf_base).rev() {
            max_end[node] = max_end[node * 2].max(max_end[node * 2 + 1]);
        }
        self.max_end = max_end;
        Ok(())
    }

    fn visit_containing(
        &self,
        node: usize,
        range: (usize, usize),
        upper: usize,
        end: u32,
        visit_node: &mut impl FnMut(),
        visit_owner: &mut impl FnMut(usize) -> Result<bool, BuiltinModelError>,
    ) -> Result<bool, BuiltinModelError> {
        visit_node();
        if range.0 >= upper || self.max_end[node] < end {
            return Ok(true);
        }
        if range.1 - range.0 == 1 {
            return visit_owner(range.0);
        }
        let middle = range.0 + (range.1 - range.0) / 2;
        if !self.visit_containing(
            node * 2 + 1,
            (middle, range.1),
            upper,
            end,
            visit_node,
            visit_owner,
        )? {
            return Ok(false);
        }
        self.visit_containing(
            node * 2,
            (range.0, middle),
            upper,
            end,
            visit_node,
            visit_owner,
        )
    }

    fn nearest(&self, span: (u32, u32)) -> Result<Option<&ReferenceOwner>, BuiltinModelError> {
        self.nearest_counted(span, &mut || {})
    }

    fn nearest_counted(
        &self,
        span: (u32, u32),
        visit_node: &mut impl FnMut(),
    ) -> Result<Option<&ReferenceOwner>, BuiltinModelError> {
        if span.0 >= span.1 || self.owners.is_empty() {
            return Ok(None);
        }
        let upper = self
            .owners
            .partition_point(|owner| owner.capture.owner().0 <= span.0);
        let mut nearest = None;
        let mut previous: Option<usize> = None;
        let mut count = 0;
        let complete = self.visit_containing(
            1,
            (0, self.leaf_base),
            upper,
            span.1,
            visit_node,
            &mut |at| {
                count += 1;
                if count > MAX_STRUCTURAL_REFERENCE_CONTAINING_OWNERS {
                    return Err(BuiltinModelError(
                        "structural reference containment lookup exceeds its owner bound"
                            .to_owned(),
                    ));
                }
                let owner = &self.owners[at];
                // Descending starts and ascending ends must form a strict chain.
                // Equal/crossing extents are ambiguous, including outer captures.
                if previous
                    .is_some_and(|held| !owner.capture.strictly_contains(self.owners[held].capture))
                {
                    return Ok(false);
                }
                nearest.get_or_insert(at);
                previous = Some(at);
                Ok(true)
            },
        )?;
        Ok(if complete {
            nearest.map(|at| &self.owners[at])
        } else {
            None
        })
    }
}

impl StructuralReferenceOwners {
    fn new(limit: usize) -> Self {
        Self {
            retained: 0,
            limit,
            files: BTreeMap::new(),
            calls: BTreeMap::new(),
        }
    }

    fn reserve_item(&mut self) -> Result<(), BuiltinModelError> {
        if self.retained >= self.limit {
            return Err(BuiltinModelError(
                "structural reference metadata exceeds the bounded result contract".to_owned(),
            ));
        }
        self.retained += 1;
        Ok(())
    }

    fn record_call(
        &mut self,
        caller: StructuralCallBinding,
        callee: StructuralCallBinding,
        excerpt_digest: [u8; 32],
        span: (usize, usize),
    ) -> Result<(), BuiltinModelError> {
        let key = (caller, callee);
        if self
            .calls
            .get(&key)
            .is_some_and(|calls| calls.spans.contains(&span))
        {
            return Ok(());
        }
        self.reserve_item()?;
        self.calls
            .entry(key)
            .or_insert_with(|| CapturedCallSpans {
                excerpt_digest,
                spans: BTreeSet::new(),
            })
            .spans
            .insert(span);
        Ok(())
    }

    /// `None` retains legacy placement; `Some(None)` refuses offsets whose
    /// captured excerpt is no longer the mapped row's exact written basis.
    pub(super) fn call_spans(
        &self,
        caller: &StructuralCallEndpoint,
        callee: &StructuralCallEndpoint,
        excerpt: &str,
    ) -> Option<Option<&BTreeSet<(usize, usize)>>> {
        let (
            StructuralCallEndpoint::Binding { source: caller, .. },
            StructuralCallEndpoint::Binding { source: callee, .. },
        ) = (caller, callee)
        else {
            return None;
        };
        let calls = self.calls.get(&(*caller, *callee))?;
        Some(
            (blake3::hash(excerpt.as_bytes()).as_bytes() == &calls.excerpt_digest)
                .then_some(&calls.spans),
        )
    }

    pub(super) fn site<'a>(
        &'a self,
        caller: &'a StructuralCallEndpoint,
        span: &backend_engine::SemanticSourceSpan,
    ) -> Result<Option<&'a StructuralCallEndpoint>, BuiltinModelError> {
        let StructuralCallEndpoint::Binding {
            source, location, ..
        } = caller
        else {
            return Ok(Some(caller));
        };
        if location.path() != span.file.as_str() {
            return Ok(None);
        }
        let Some(index) = self.files.get(&source.file) else {
            // Other languages retain their existing reference projection.
            return Ok(Some(caller));
        };
        let Some(nearest) = index.nearest((span.start, span.end))? else {
            return Ok(None);
        };
        let mut current = nearest;
        for _ in 0..MAX_STRUCTURAL_REFERENCE_PARENT_DEPTH {
            if current.capture.binding() == source.binding {
                return Ok(Some(&nearest.endpoint));
            }
            // A nested callable supplies its own edge and resolution scope.
            // Never borrow the enclosing callable's callee resolution for it.
            if current.callable {
                return Ok(None);
            }
            let Some(parent) = current.parent else {
                return Ok(None);
            };
            let Some(&at) = index.by_binding.get(&parent) else {
                return Ok(None);
            };
            current = &index.owners[at];
        }
        Ok(None)
    }
}

pub(super) struct ProjectCalls {
    #[cfg(test)]
    resolved_specifiers: usize,
    #[cfg(test)]
    coordinates: Vec<String>,
    endpoints: Vec<StructuralCallEndpoint>,
    reference_owners: Option<StructuralReferenceOwners>,
    by_binding: BTreeMap<StructuralCallBinding, DeclarationId>,
    by_row: BTreeMap<RowId, DeclarationId>,
    by_coordinate: BTreeMap<String, DeclarationId>,
    outgoing: BTreeMap<DeclarationId, BTreeSet<DeclarationId>>,
    incoming: BTreeMap<DeclarationId, BTreeSet<DeclarationId>>,
}

impl ProjectCalls {
    fn build(
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<Self, BuiltinModelError> {
        Self::build_with_references(sources, package, None)
    }

    fn build_with_references(
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
        reference_limit: Option<usize>,
    ) -> Result<Self, BuiltinModelError> {
        let _project = sources
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
        for (key, record) in &sources.files {
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
                key: *key,
                path: file.path,
                language: file.language,
                declarations,
            });
        }
        let selected = files.iter().map(|file| file.key).collect::<BTreeSet<_>>();
        let plan = StructuralProjectionPlan::of_files(sources, &BTreeSet::new(), &selected)?;
        let modules = ModuleIndex::new(files.iter().map(|file| file.path));
        let mut graph = Self {
            #[cfg(test)]
            resolved_specifiers: 0,
            #[cfg(test)]
            coordinates: Vec::new(),
            endpoints: Vec::new(),
            reference_owners: reference_limit.map(StructuralReferenceOwners::new),
            by_binding: BTreeMap::new(),
            by_row: BTreeMap::new(),
            by_coordinate: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
        };
        let mut declarations = Vec::with_capacity(files.len());
        let mut callers = Vec::with_capacity(files.len());
        let ts_parameters = files.iter().any(|file| file.language == backend_engine::SourceLanguage::TypeScript)
            .then(backend_frontend_typescript::syntax_frontend).transpose()
            .map_err(|error| BuiltinModelError(format!("structural parameter grammar: {error}")))?;
        for file in &files {
            let mut lookup = FileDeclarations {
                require_exact_imports: file.language == backend_engine::SourceLanguage::TypeScript,
                ..FileDeclarations::default()
            };
            let mut file_callers = Vec::new();
            let binding_names = file
                .declarations
                .iter()
                .filter_map(|owner| match owner.binding_identity() {
                    backend_compile::SourceBindingIdentity::Exact(binding) => {
                        Some((binding.binding(), owner.name()))
                    }
                    backend_compile::SourceBindingIdentity::Legacy => None,
                })
                .collect::<BTreeMap<_, _>>();
            for (index, declaration) in file.declarations.iter().enumerate() {
                if declares_a_nominal_type(declaration.kind()) {
                    lookup.nominal.insert(declaration.name());
                }
                let exact_scope = match (declaration.binding_identity(), declaration.container()) {
                    (
                        backend_compile::SourceBindingIdentity::Exact(capture),
                        Container::Binding { binding },
                    ) => Some((capture.binding(), Some(*binding))),
                    (backend_compile::SourceBindingIdentity::Exact(capture), Container::Module) => {
                        Some((capture.binding(), None))
                    }
                    _ => None,
                };
                if let Some((binding, scope)) = exact_scope {
                    lookup.scope_parents.insert(binding, scope);
                }
                // TypeScript members are property keys, not lexical bindings.
                // Keep their scope ancestry and member indexes, but do not let
                // a parameter type's property mask an imported bare identifier.
                let lexical_scope = exact_scope.filter(|_| {
                    file.language != backend_engine::SourceLanguage::TypeScript
                        || !matches!(declaration.kind(),
                            backend_compile::DeclarationKind::Method
                                | backend_compile::DeclarationKind::Field
                                | backend_compile::DeclarationKind::Property)
                });
                if let Some((binding, scope)) = lexical_scope {
                    let name = lookup
                        .scoped_names
                        .entry(declaration.name())
                        .or_default()
                        .entry(scope)
                        .or_default();
                    name.bindings += 1;
                    if parse_import_binding(declaration).is_some() {
                        name.import = Some(binding);
                    }
                }
                if let Some(owners) = graph.reference_owners.as_mut()
                    && file.language == backend_engine::SourceLanguage::TypeScript
                    && let backend_compile::SourceBindingIdentity::Exact(capture) =
                        declaration.binding_identity()
                    && let Some((_, parent)) = exact_scope
                {
                    let prepared = plan.declaration(file.key, index)?;
                    owners.reserve_item()?;
                    owners
                        .files
                        .entry(file.key)
                        .or_default()
                        .owners
                        .push(ReferenceOwner {
                            capture,
                            parent,
                            callable: structural_callable(declaration.kind()),
                            endpoint: StructuralCallEndpoint::Binding {
                                source: StructuralCallBinding {
                                    file: file.key,
                                    binding: capture.binding(),
                                },
                                row: prepared.id,
                                location: declaration.location().clone(),
                                semantic_site_unique: prepared.identity_preimage.is_none(),
                                name: declaration.name().to_owned(),
                            },
                        });
                }
                if !structural_callable(declaration.kind()) {
                    continue;
                }
                if file.language == backend_engine::SourceLanguage::TypeScript
                    && let Some((binding, _)) = exact_scope {
                    let parameters = if declaration.source_excerpt().extent()
                        == Some(backend_compile::SourceExcerptExtent::Complete) {
                        declaration.source_excerpt().text().and_then(|excerpt|
                            ts_parameters.as_ref()?.callable_parameter_names(std::path::Path::new(file.path), declaration.kind(), excerpt).ok().flatten())
                    } else { None };
                    if let Some(names) = parameters { lookup.parameter_names.insert(binding, names); }
                    else { lookup.unknown_parameters.insert(binding); }
                }
                let prepared = plan.declaration(file.key, index)?;
                let id = match declaration.binding_identity() {
                    backend_compile::SourceBindingIdentity::Exact(capture) => {
                        let source = StructuralCallBinding {
                            file: file.key,
                            binding: capture.binding(),
                        };
                        if let Some(&id) = graph.by_binding.get(&source) {
                            id
                        } else {
                            let id = DeclarationId(graph.endpoints.len());
                            graph.endpoints.push(StructuralCallEndpoint::Binding {
                                source,
                                row: prepared.id,
                                location: declaration.location().clone(),
                                semantic_site_unique: prepared.identity_preimage.is_none(),
                                name: declaration.name().to_owned(),
                            });
                            graph.by_binding.insert(source, id);
                            graph.by_row.insert(prepared.id, id);
                            #[cfg(test)]
                            graph.coordinates.push(prepared.coordinate.clone());
                            id
                        }
                    }
                    backend_compile::SourceBindingIdentity::Legacy => {
                        if let Some(&id) = graph.by_coordinate.get(&prepared.coordinate) {
                            id
                        } else {
                            let id = DeclarationId(graph.endpoints.len());
                            graph
                                .endpoints
                                .push(StructuralCallEndpoint::LegacyCoordinate(
                                    prepared.coordinate.clone(),
                                ));
                            graph.by_coordinate.insert(prepared.coordinate.clone(), id);
                            #[cfg(test)]
                            graph.coordinates.push(prepared.coordinate.clone());
                            id
                        }
                    }
                };
                if let Some((_, scope)) = lexical_scope {
                    lookup
                        .scoped_names
                        .entry(declaration.name())
                        .or_default()
                        .entry(scope)
                        .or_default()
                        .targets
                        .insert(id);
                }
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
                    Container::Binding { binding } => {
                        if let Some(&name) = binding_names.get(binding) {
                            lookup
                                .methods
                                .entry(name)
                                .or_default()
                                .entry(declaration.name())
                                .or_default()
                                .insert(id);
                        }
                    }
                    Container::Module => {}
                }
                if let Some(excerpt) = declaration.source_excerpt().text() {
                    file_callers.push((id, excerpt, exact_scope.map(|(binding, _)| binding)));
                }
            }
            declarations.push(lookup);
            callers.push(file_callers);
        }
        if let Some(owners) = graph.reference_owners.as_mut() {
            for index in owners.files.values_mut() {
                index.finish()?;
            }
        }
        for (at, file) in files.iter().enumerate() {
            let imported = ImportedCalls::join(
                file.declarations.iter().filter_map(|declaration| {
                    let import = parse_import_binding(declaration)?;
                    let binding = match declaration.binding_identity() {
                        backend_compile::SourceBindingIdentity::Exact(capture) => {
                            Some(capture.binding())
                        }
                        backend_compile::SourceBindingIdentity::Legacy => None,
                    };
                    Some((import, binding))
                }),
                file.path,
                &modules,
                &declarations,
            );
            #[cfg(test)]
            {
                graph.resolved_specifiers += imported.resolved_specifiers;
            }
            for &(caller, excerpt, exact_caller) in &callers[at] {
                let excerpt_digest = if graph.reference_owners.is_some()
                    && file.language == backend_engine::SourceLanguage::TypeScript
                {
                    *blake3::hash(excerpt.as_bytes()).as_bytes()
                } else {
                    [0; 32]
                };
                for call in structural_excerpt_call_sites(excerpt) {
                    let Some(callee) = imported.target(&declarations[at], &call, exact_caller)
                    else {
                        continue;
                    };
                    if caller == callee {
                        continue;
                    }
                    if let Some(owners) = graph.reference_owners.as_mut()
                        && let (
                            StructuralCallEndpoint::Binding { source: caller, .. },
                            StructuralCallEndpoint::Binding { source: callee, .. },
                        ) = (&graph.endpoints[caller.0], &graph.endpoints[callee.0])
                        && owners.files.contains_key(&caller.file)
                    {
                        owners.record_call(*caller, *callee, excerpt_digest, call.span)?;
                    }
                    graph.outgoing.entry(caller).or_default().insert(callee);
                    graph.incoming.entry(callee).or_default().insert(caller);
                }
            }
        }
        Ok(graph)
    }

    pub(super) fn endpoint_pairs(
        &self,
    ) -> impl Iterator<Item = (&StructuralCallEndpoint, &StructuralCallEndpoint)> {
        self.outgoing.iter().flat_map(move |(&caller, targets)| {
            targets
                .iter()
                .map(move |&callee| (&self.endpoints[caller.0], &self.endpoints[callee.0]))
        })
    }

    pub(super) fn neighborhood_for_row<'a>(
        &'a self,
        row: RowId,
        legacy_coordinate: &str,
        include_incoming: bool,
    ) -> impl Iterator<Item = (&'a StructuralCallEndpoint, &'a StructuralCallEndpoint)> {
        let source = self
            .by_row
            .get(&row)
            .or_else(|| self.by_coordinate.get(legacy_coordinate))
            .copied();
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
        outgoing
            .chain(incoming)
            .map(|(caller, callee)| (&self.endpoints[caller.0], &self.endpoints[callee.0]))
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
    pub(crate) fn coordinate_pairs(
        &mut self,
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<Vec<(String, String)>, BuiltinModelError> {
        Ok(self.project_calls(sources, package)?.coordinate_pairs())
    }

    /// Borrows endpoints from the immutable projection certified by its
    /// selected workspace/source root. Holding this borrow prevents replacement.
    pub(crate) fn endpoint_pairs<'a>(
        &'a mut self,
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<Vec<(&'a StructuralCallEndpoint, &'a StructuralCallEndpoint)>, BuiltinModelError>
    {
        Ok(self
            .project_calls(sources, package)?
            .endpoint_pairs()
            .collect())
    }

    pub(crate) fn reference_projection<'a>(
        &'a mut self,
        sources: &IndexedSources,
        package: backend_engine::PackageKey,
    ) -> Result<
        (
            Vec<(&'a StructuralCallEndpoint, &'a StructuralCallEndpoint)>,
            &'a StructuralReferenceOwners,
        ),
        BuiltinModelError,
    > {
        // Graph-only requests never retain every occurrence. References upgrade
        // the exact cached projection once, admitting all metadata before swap.
        let key = sources.source_snapshot.as_ref().map(|source| SourceKey {
            workspace: source.workspace_root(),
            package,
        });
        let hit = key.is_some()
            && self.current.as_ref().is_some_and(|current| {
                current.key == key && current.calls.reference_owners.is_some()
            });
        if !hit {
            let calls = ProjectCalls::build_with_references(
                sources,
                package,
                Some(MAX_STRUCTURAL_REFERENCE_METADATA_ITEMS),
            )?;
            self.current = Some(CachedCalls { key, calls });
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        let calls = &self
            .current
            .as_ref()
            .expect("projection admitted above")
            .calls;
        Ok((
            calls.endpoint_pairs().collect(),
            calls
                .reference_owners
                .as_ref()
                .expect("reference metadata admitted above"),
        ))
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
    fn large_parsed_file_reference_lookup_prunes_unrelated_owners() -> Result<(), String> {
        use backend_engine::SourceLanguage::TypeScript;
        let body = (0..4096)
            .map(|at| format!("const item{at} = target();\n"))
            .collect::<String>();
        let text = format!("function target() {{}}\nfunction caller() {{\n{body}}}\n");
        let declarations = analyze_source(TypeScript, "large.ts", &text)?;
        // Real files of this size arrive through complete paged facts. Do not
        // force thousands of captures into the legacy single-row test helper.
        let mut owners = StructuralReferenceOwners::new(MAX_STRUCTURAL_REFERENCE_METADATA_ITEMS);
        let mut index = ReferenceFileOwners::default();
        for declaration in declarations.iter() {
            let backend_compile::SourceBindingIdentity::Exact(capture) =
                declaration.binding_identity()
            else {
                continue;
            };
            let parent = match declaration.container() {
                Container::Binding { binding } => Some(*binding),
                Container::Module => None,
                _ => return Err("expected exact parsed ownership".to_owned()),
            };
            owners.reserve_item().map_err(|error| error.to_string())?;
            index.owners.push(ReferenceOwner {
                capture,
                parent,
                callable: structural_callable(declaration.kind()),
                endpoint: StructuralCallEndpoint::Binding {
                    source: StructuralCallBinding {
                        file: [3; 32],
                        binding: capture.binding(),
                    },
                    row: RowId::Symbol(backend_engine::symbol_key(&format!(
                        "large-owner-{}",
                        capture.name().0
                    ))),
                    location: declaration.location().clone(),
                    semantic_site_unique: true,
                    name: declaration.name().to_owned(),
                },
            });
        }
        index.finish().map_err(|error| error.to_string())?;
        owners.files.insert([3; 32], index);
        let index = owners.files.get(&[3; 32]).ok_or("file index")?;
        assert_eq!(index.owners.len(), 4098);
        assert_eq!(index.by_binding.len(), index.owners.len());
        let caller = &index.owners.iter().find(|owner|
            matches!(&owner.endpoint, StructuralCallEndpoint::Binding { name, .. } if name == "caller")).ok_or("caller")?.endpoint;
        let calls = text
            .match_indices("target()")
            .skip(1)
            .map(|(start, _)| (start as u32, start as u32 + 6))
            .collect::<Vec<_>>();
        for at in [0, 2047, 4095] {
            let mut visited = 0;
            let owner = index
                .nearest_counted(calls[at], &mut || visited += 1)
                .map_err(|error| error.to_string())?
                .ok_or("nearest variable")?;
            assert!(
                visited <= 128,
                "a shallow occurrence must not scan4098 owners: visited{visited} index nodes"
            );
            let StructuralCallEndpoint::Binding { name, .. } = &owner.endpoint else {
                return Err("exact owner".to_owned());
            };
            assert_eq!(name, &format!("item{at}"));
            let bound = *index
                .by_binding
                .get(&owner.capture.binding())
                .ok_or("binding index")?;
            assert!(std::ptr::eq(&index.owners[bound], owner));
            let span = backend_engine::SemanticSourceSpan {
                file: backend_engine::ProductText::new("large.ts")
                    .map_err(|error| error.to_string())?,
                start: calls[at].0,
                end: calls[at].1,
            };
            assert!(std::ptr::eq(
                owners
                    .site(caller, &span)
                    .map_err(|error| error.to_string())?
                    .ok_or("indexed parent traversal")?,
                &owner.endpoint
            ));
        }
        Ok(())
    }

    // Private interval data controls deliberately include malformed overlaps;
    // they do not construct a compiler capability or pretend to parse a file.
    fn interval_owner(start: u32, end: u32, parent: Option<u32>, callable: bool) -> ReferenceOwner {
        let capture = backend_compile::SourceBindingCapture::new(start, end, start, start + 1)
            .expect("nonempty test capture");
        ReferenceOwner {
            capture,
            parent: parent.and_then(|start| backend_compile::SourceBinding::new(start, start + 1)),
            callable,
            endpoint: StructuralCallEndpoint::Binding {
                source: StructuralCallBinding {
                    file: [7; 32],
                    binding: capture.binding(),
                },
                row: RowId::Symbol(backend_engine::symbol_key(&format!("owner{start}"))),
                location: backend_compile::SourceLocation::new("owners.ts", 1)
                    .expect("test location"),
                semantic_site_unique: true,
                name: "x".to_owned(),
            },
        }
    }

    #[test]
    fn indexed_reference_lookup_preserves_ambiguity_and_bounded_nesting() -> Result<(), String> {
        let mut overlap = ReferenceFileOwners::default();
        overlap.owners = vec![
            interval_owner(0, 30, None, false),
            interval_owner(10, 40, None, false),
        ];
        overlap.finish().map_err(|error| error.to_string())?;
        assert!(
            overlap
                .nearest((20, 21))
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert_eq!(
            overlap
                .nearest((25, 35))
                .map_err(|error| error.to_string())?
                .map(|owner| owner.capture.owner()),
            Some((10, 40)),
            "an owner must contain the whole span"
        );
        assert_eq!(
            overlap
                .nearest((2, 3))
                .map_err(|error| error.to_string())?
                .map(|owner| owner.capture.owner()),
            Some((0, 30))
        );
        let mut equal = ReferenceFileOwners::default();
        let mut duplicate = interval_owner(0, 30, None, false);
        duplicate.capture =
            backend_compile::SourceBindingCapture::new(0, 30, 1, 2).ok_or("second binding")?;
        if let StructuralCallEndpoint::Binding { source, row, .. } = &mut duplicate.endpoint {
            source.binding = duplicate.capture.binding();
            *row = RowId::Symbol(backend_engine::symbol_key("equal-owner-second"));
        }
        equal.owners = vec![interval_owner(0, 30, None, false), duplicate];
        equal.finish().map_err(|error| error.to_string())?;
        assert!(
            equal
                .nearest((20, 21))
                .map_err(|error| error.to_string())?
                .is_none()
        );

        let nested = |count: u32| -> Result<ReferenceFileOwners, String> {
            let mut index = ReferenceFileOwners::default();
            index.owners = (0..count)
                .map(|at| interval_owner(at, count * 2 - at, at.checked_sub(1), at == 0))
                .collect();
            index.finish().map_err(|error| error.to_string())?;
            Ok(index)
        };
        let boundary = nested(256)?;
        let caller = boundary
            .owners
            .first()
            .ok_or("root owner")?
            .endpoint
            .clone();
        let mut owners = StructuralReferenceOwners::new(MAX_STRUCTURAL_REFERENCE_METADATA_ITEMS);
        owners.files.insert([7; 32], boundary);
        let span = backend_engine::SemanticSourceSpan {
            file: backend_engine::ProductText::new("owners.ts")
                .map_err(|error| error.to_string())?,
            start: 256,
            end: 257,
        };
        assert!(
            owners
                .site(&caller, &span)
                .map_err(|error| error.to_string())?
                .is_some(),
            "the256-owner/parent boundary remains admissible"
        );
        let deep = nested(2048)?;
        let mut visited = 0;
        let error = match deep.nearest_counted((2048, 2049), &mut || visited += 1) {
            Ok(_) => return Err("uncertified deep lookup must explicitly refuse".to_owned()),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "structural reference containment lookup exceeds its owner bound"
        );
        assert!(
            visited <= 1024,
            "adversarial nesting must stop before scanning2048 owners: visited{visited} nodes"
        );
        Ok(())
    }

    #[test]
    fn reference_metadata_is_optional_and_refuses_overflow_without_truncation() -> Result<(), String>
    {
        use backend_engine::SourceLanguage::TypeScript;
        let declarations = analyze_source(
            TypeScript,
            "calls.ts",
            "function target() {} function caller() { target(); target(); target(); }",
        )?;
        let (sources, package) = cross_file_sources(&[("calls.ts", TypeScript, declarations)])?;
        let graph = ProjectCalls::build(&sources, package).map_err(|error| error.to_string())?;
        assert_eq!(graph.coordinate_pairs().len(), 1);
        assert!(
            graph.reference_owners.is_none(),
            "graph-only queries do not retain occurrence metadata"
        );
        let references = ProjectCalls::build_with_references(&sources, package, Some(5))
            .map_err(|error| error.to_string())?;
        assert_eq!(references.coordinate_pairs(), graph.coordinate_pairs());
        let owners = references
            .reference_owners
            .as_ref()
            .ok_or("reference metadata")?;
        assert_eq!(
            owners.retained, 5,
            "two captured owners plus three distinct written calls"
        );
        assert_eq!(
            owners
                .calls
                .values()
                .map(|calls| calls.spans.len())
                .sum::<usize>(),
            3
        );
        let error = match ProjectCalls::build_with_references(&sources, package, Some(4)) {
            Ok(_) => {
                return Err("overflow must refuse the complete reference projection".to_owned());
            }
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "structural reference metadata exceeds the bounded result contract"
        );
        assert_eq!(
            graph.coordinate_pairs().len(),
            1,
            "reference overflow cannot alter a valid enclosing graph"
        );
        // A project cache must admit more occurrences than one reply page,
        // even though each individual reference response remains bounded.
        let body = "target();".repeat(backend_engine::MAX_PRODUCT_ROWS + 1);
        let text = format!("function target() {{}} function caller() {{{body}}}");
        let declarations = analyze_source(TypeScript, "many.ts", &text)?;
        let (many_sources, package) = cross_file_sources(&[("many.ts", TypeScript, declarations)])?;
        let many = ProjectCalls::build_with_references(
            &many_sources,
            package,
            Some(MAX_STRUCTURAL_REFERENCE_METADATA_ITEMS),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(many.coordinate_pairs().len(), 1);
        assert_eq!(
            many.reference_owners
                .as_ref()
                .ok_or("large reference metadata")?
                .retained,
            2 + backend_engine::MAX_PRODUCT_ROWS + 1
        );
        Ok(())
    }

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
            ("one/target.ts", TypeScript, target.clone()),
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
        for (source, imported, local) in [
            (
                "import { validate as check } from './target';\nexport function caller() { const check = 0; check(); }\n",
                false,
                false,
            ),
            (
                "import { validate as check } from './target';\nexport function caller() { function check() {} check(); }\n",
                false,
                true,
            ),
            (
                "import { validate as check } from './target';\nfunction hidden() { const check = 0; }\nexport function caller() { check(); }\n",
                true,
                false,
            ),
        ] {
            let caller = analyze_source(TypeScript, "one/caller.ts", source)?;
            let (sources, package) = cross_file_sources(&[
                ("one/caller.ts", TypeScript, caller),
                ("one/target.ts", TypeScript, target.clone()),
            ])?;
            let pairs = ProjectCalls::build(&sources, package)
                .map_err(|e| e.to_string())?
                .coordinate_pairs();
            assert_eq!(
                pairs
                    .iter()
                    .any(|(_, target)| target == "fixture::one/target.ts:1::validate"),
                imported,
                "only the nearest exact lexical binding can select the import: {source}"
            );
            assert_eq!(
                pairs.iter().any(|(_, target)| target.ends_with("::check")),
                local,
                "a local callable stays distinct from its imported namesake: {source}"
            );
            if !imported && !local {
                assert!(pairs.is_empty(), "noncallable local shadow: {source}");
            }
        }
        Ok(())
    }
    #[test]
    fn a_method_name_alone_never_binds_an_unknown_receiver() {
        let mut local = FileDeclarations::default();
        local.nominal.insert("Only");
        local
            .callable
            .entry("custom_name")
            .or_default()
            .insert(DeclarationId(0));
        local
            .methods
            .entry("Only")
            .or_default()
            .entry("custom_name")
            .or_default()
            .insert(DeclarationId(0));
        let imported = ImportedCalls::default();
        for excerpt in [
            "fn caller() { other.custom_name(); }",
            "fn caller() { factory(x).custom_name(); }",
            "fn caller() { items[0].custom_name(); }",
            "fn caller() { nested.value.custom_name(); }",
            "fn caller() { custom_name(); }",
            "fn caller() { self.custom_name(); }",
        ] {
            let calls = structural_excerpt_call_sites(excerpt);
            let selected = calls
                .iter()
                .find(|call| call.name == "custom_name")
                .expect("written call");
            assert_eq!(imported.target(&local, selected, None), None, "{excerpt}");
        }
        let calls = structural_excerpt_call_sites("fn caller() { Only::custom_name(); }");
        let selected = calls
            .iter()
            .find(|call| call.name == "custom_name")
            .expect("qualified method");
        assert_eq!(
            imported.target(&local, selected, None),
            Some(DeclarationId(0))
        );
    }

    #[test]
    fn an_imported_type_method_stays_bound_to_that_type_alias() {
        let local = FileDeclarations::default();
        let mut imported = ImportedCalls::default();
        imported
            .classes
            .entry("First".to_owned())
            .or_default()
            .entry("close".to_owned())
            .or_default()
            .insert(DeclarationId(0));
        imported
            .classes
            .entry("Second".to_owned())
            .or_default()
            .entry("close".to_owned())
            .or_default()
            .insert(DeclarationId(1));
        for (receiver, expected) in [
            ("First", Some(DeclarationId(0))),
            ("Second", Some(DeclarationId(1))),
            ("unknown", None),
        ] {
            let calls =
                structural_excerpt_call_sites(&format!("fn caller() {{ {receiver}.close(); }}"));
            let selected = calls
                .iter()
                .find(|call| call.name == "close")
                .expect("written close");
            assert_eq!(imported.target(&local, selected, None), expected);
        }
    }
}
