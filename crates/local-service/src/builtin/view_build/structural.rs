use super::super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use super::identity::{declaration_coordinate, declaration_symbol};
use super::semantic_profile_is_complete;
use backend_compile::Container;
use backend_engine::{DeclarationKind, RowId, RowIdentityPreimage};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

pub(super) fn duplicate_declaration_coordinates(
    containment: &FileContainment<'_>,
    declarations: &[backend_compile::SourceDeclaration],
) -> BTreeSet<String> {
    let mut counts = BTreeMap::<String, u32>::new();
    for declaration in declarations {
        let coordinate = containment.coordinate(declaration);
        let count = counts.entry(coordinate).or_default();
        *count = count.saturating_add(1);
    }
    counts
        .into_iter()
        .filter_map(|(coordinate, count)| (count > 1).then_some(coordinate))
        .collect()
}
pub(super) fn is_file_module(declaration: &backend_compile::SourceDeclaration, path: &str) -> bool {
    declaration.kind() == DeclarationKind::Module
        && declaration.line() == 1
        && std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem == declaration.name())
}
const fn declares_a_type(kind: DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Struct
            | DeclarationKind::Enum
            | DeclarationKind::Union
            | DeclarationKind::Class
            | DeclarationKind::Trait
            | DeclarationKind::Interface
            | DeclarationKind::Type
    )
}
type TypeIdentity = ([u8; 32], String);

/// The file and line one declaration was declared at.
type DeclarationSite = (String, u32);

#[derive(Default)]
pub(super) struct ProjectTypeIndex {
    by_name: BTreeMap<TypeIdentity, DeclarationSite>,
}

impl ProjectTypeIndex {
    pub(super) fn of(sources: &IndexedSources) -> Self {
        let mut index = Self::default();
        for (_, record) in &sources.files {
            let Some(file) = record.file_fields() else {
                continue;
            };
            for declaration in file.declarations.iter() {
                if !declares_a_type(declaration.kind()) {
                    continue;
                }
                let entry = (file.path.to_owned(), declaration.line());
                index
                    .by_name
                    .entry((file.project, declaration.name().to_owned()))
                    .and_modify(|held| {
                        if entry < *held {
                            *held = entry.clone();
                        }
                    })
                    .or_insert(entry);
            }
        }
        index
    }

    fn resolve(&self, project: [u8; 32], type_name: &str) -> Option<(&str, u32)> {
        self.by_name
            .get(&(project, type_name.to_owned()))
            .map(|(path, line)| (path.as_str(), *line))
    }
}

/// One structural declaration retained for an emitting source file.
///
/// The coordinate, parent coordinate, occurrence-disambiguated identity, and
/// identity preimage are prepared once. Both the product-row and Trustfall
/// producers borrow this exact entry, so they cannot disagree about which
/// duplicate declaration owns an edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StructuralDeclaration {
    pub(super) coordinate: String,
    pub(super) parent: Option<String>,
    pub(super) id: RowId,
    pub(super) identity_preimage: Option<RowIdentityPreimage>,
    pub(super) is_file_module: bool,
}

/// Structural declarations retained for one file that remains in the
/// structural lane. Semantic-complete files intentionally have no entry: a
/// structural parent may never resolve to an identity that will be
/// suppressed from the product or query corpus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StructuralFilePlan {
    project: [u8; 32],
    package: backend_engine::PackageKey,
    module_coordinate: String,
    pub(super) declarations: Box<[StructuralDeclaration]>,
}

/// The emitted structural graph for one projection.
///
/// A tags query can select two declarations at one coordinate (for example a
/// C `struct` and its `typedef`, or two frontend tags for the same namespace).
/// [`declaration_symbol`] deliberately gives every such declaration its own
/// checked identity preimage. Parentage cannot hash the bare coordinate in
/// that case: there is no row with that unsuffixed identity. This table keeps
/// every disambiguated row and resolves a parent to one of those retained
/// identities, preferring a declaration kind that can own members and then a
/// stable kind/id order. No declaration is discarded.
#[derive(Default)]
pub(super) struct StructuralProjectionPlan {
    files: BTreeMap<[u8; 32], StructuralFilePlan>,
    pub(super) by_coordinate: BTreeMap<([u8; 32], String), Vec<StructuralSymbol>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StructuralSymbol {
    pub(super) id: RowId,
    pub(super) kind: DeclarationKind,
}

/// A parent target is either a retained declaration or the package row. The
/// latter is the final bounded fallback for a malformed/incomplete file
/// module; product rows represent it through their package relation field,
/// while Trustfall facts can point at the already-emitted package fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StructuralParent {
    Symbol(RowId),
    Package(backend_engine::PackageKey),
}

impl StructuralProjectionPlan {
    pub(super) fn of(
        sources: &IndexedSources,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    ) -> Result<Self, BuiltinModelError> {
        let types = ProjectTypeIndex::of(sources);
        let mut plan = Self::default();
        for (file_key, record) in &sources.files {
            let Some(file) = record.file_fields() else {
                continue;
            };
            let project = sources.projects.get(&file.project).ok_or_else(|| {
                BuiltinModelError("structural source refers to a missing project".to_owned())
            })?;
            if semantic_profile_is_complete(complete, Some(project.package), file.path)? {
                continue;
            }
            let containment =
                FileContainment::new(&project.label, file.path, file.project, file.declarations);
            let duplicate_coordinates =
                duplicate_declaration_coordinates(&containment, file.declarations);
            let mut occurrences = BTreeMap::new();
            let mut declarations = Vec::with_capacity(file.declarations.len());
            for declaration in file.declarations.iter() {
                let coordinate = containment.coordinate(declaration);
                let (id, identity_preimage) = declaration_symbol(
                    &coordinate,
                    declaration.kind(),
                    declaration.signature(),
                    duplicate_coordinates.contains(&coordinate),
                    &mut occurrences,
                );
                let identity_preimage = identity_preimage
                    .map(RowIdentityPreimage::try_from)
                    .transpose()
                    .map_err(|error| {
                        BuiltinModelError(format!("structural row identity preimage: {error}"))
                    })?;
                let parent = containment.parent_coordinate(declaration, &types);
                plan.by_coordinate
                    .entry((file.project, coordinate.clone()))
                    .or_default()
                    .push(StructuralSymbol {
                        id,
                        kind: declaration.kind(),
                    });
                declarations.push(StructuralDeclaration {
                    coordinate,
                    parent,
                    id,
                    identity_preimage,
                    is_file_module: is_file_module(declaration, file.path),
                });
            }
            plan.files.insert(
                *file_key,
                StructuralFilePlan {
                    project: file.project,
                    package: project.package,
                    module_coordinate: containment.module_coordinate,
                    declarations: declarations.into_boxed_slice(),
                },
            );
        }
        for symbols in plan.by_coordinate.values_mut() {
            symbols.sort_unstable_by_key(|symbol| {
                (structural_parent_rank(symbol.kind), symbol.kind, symbol.id)
            });
        }
        Ok(plan)
    }

    pub(super) fn file(&self, file_key: [u8; 32]) -> Option<&StructuralFilePlan> {
        self.files.get(&file_key)
    }

    pub(super) fn declaration(
        &self,
        file_key: [u8; 32],
        index: usize,
    ) -> Result<&StructuralDeclaration, BuiltinModelError> {
        self.file(file_key)
            .and_then(|file| file.declarations.get(index))
            .ok_or_else(|| {
                BuiltinModelError(
                    "structural declaration plan is out of sync with source".to_owned(),
                )
            })
    }

    pub(super) fn parent_id(
        &self,
        file_key: [u8; 32],
        coordinate: &str,
    ) -> Result<StructuralParent, BuiltinModelError> {
        let file = self.file(file_key).ok_or_else(|| {
            BuiltinModelError("structural parent requested for a suppressed source file".to_owned())
        })?;
        if let Some(symbol) = self
            .by_coordinate
            .get(&(file.project, coordinate.to_owned()))
            .and_then(|symbols| symbols.first())
        {
            return Ok(StructuralParent::Symbol(symbol.id));
        }
        if let Some(symbol) = self
            .by_coordinate
            .get(&(file.project, file.module_coordinate.clone()))
            .and_then(|symbols| symbols.first())
        {
            return Ok(StructuralParent::Symbol(symbol.id));
        }
        Ok(StructuralParent::Package(file.package))
    }
}

/// Orders duplicate declarations for parent resolution without dropping any
/// of their identities. Concrete type declarations are preferred over aliases
/// and other tags because they are the declaration that structurally owns
/// fields and methods; ties remain deterministic by the closed kind and row
/// identity.
pub(super) const fn structural_parent_rank(kind: DeclarationKind) -> u8 {
    match kind {
        DeclarationKind::Struct
        | DeclarationKind::Enum
        | DeclarationKind::Class
        | DeclarationKind::Interface
        | DeclarationKind::Trait
        | DeclarationKind::Union => 0,
        DeclarationKind::Type => 1,
        _ => 2,
    }
}

/// Resolves the coordinates of one file's declarations and of their parents.
///
/// The frontend states containment structurally - a name and a line, or a
/// type name to look up - because only a row projection knows what a row's
/// coordinate is. Turning that into a parent coordinate is therefore done
/// here, once, for both the row path and the query-fact path.
pub(super) struct FileContainment<'a> {
    label: &'a str,
    path: &'a str,
    project: [u8; 32],
    module_coordinate: String,
    local_types: BTreeMap<&'a str, u32>,
}

impl<'a> FileContainment<'a> {
    fn new(
        label: &'a str,
        path: &'a str,
        project: [u8; 32],
        declarations: &'a [backend_compile::SourceDeclaration],
    ) -> Self {
        let mut local_types = BTreeMap::new();
        for declaration in declarations {
            if !declares_a_type(declaration.kind()) {
                continue;
            }
            local_types
                .entry(declaration.name())
                .and_modify(|line: &mut u32| *line = (*line).min(declaration.line()))
                .or_insert(declaration.line());
        }
        Self {
            label,
            path,
            project,
            module_coordinate: format!("{label}::{path}"),
            local_types,
        }
    }

    /// Returns the coordinate a declaration's own row is addressed by.
    fn coordinate(&self, declaration: &backend_compile::SourceDeclaration) -> String {
        if is_file_module(declaration, self.path) {
            return self.module_coordinate.clone();
        }
        declaration_coordinate(
            self.label,
            self.path,
            declaration.line(),
            declaration.name(),
        )
    }

    /// Returns the coordinate of the row a declaration hangs under.
    ///
    /// Every unresolved containment falls back to the file module rather than
    /// to no parent at all: a declaration that vanished from every outline
    /// would be worse than one shown at file level.
    fn parent_coordinate(
        &self,
        declaration: &backend_compile::SourceDeclaration,
        types: &ProjectTypeIndex,
    ) -> Option<String> {
        if is_file_module(declaration, self.path) {
            return None;
        }
        Some(match declaration.container() {
            backend_compile::Container::Module => self.module_coordinate.clone(),
            backend_compile::Container::Enclosing { name, line } => {
                declaration_coordinate(self.label, self.path, line.get(), name)
            }
            backend_compile::Container::Attached { type_name } => self
                .attached_coordinate(type_name, types)
                .unwrap_or_else(|| self.module_coordinate.clone()),
        })
    }

    fn attached_coordinate(&self, type_name: &str, types: &ProjectTypeIndex) -> Option<String> {
        if let Some(line) = self.local_types.get(type_name) {
            return Some(declaration_coordinate(
                self.label, self.path, *line, type_name,
            ));
        }
        let (path, line) = types.resolve(self.project, type_name)?;
        Some(declaration_coordinate(self.label, path, line, type_name))
    }
}
pub(super) fn projected_source_capacity(
    sources: &IndexedSources,
    complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
) -> Result<usize, BuiltinModelError> {
    let count = sources
        .files
        .iter()
        .try_fold(sources.projects.len(), |count, (_, record)| {
            let rows = match record.file_fields() {
                Some(fields)
                    if semantic_profile_is_complete(
                        complete,
                        sources
                            .projects
                            .get(&fields.project)
                            .map(|project| project.package),
                        fields.path,
                    )? =>
                {
                    0
                }
                Some(fields) => fields.declarations.len(),
                None => 0,
            };
            count
                .checked_add(rows)
                .ok_or_else(|| BuiltinModelError("workspace view row count overflow".to_owned()))
        })?;
    if count > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace source declarations exceed the rebuild row bound".to_owned(),
        ));
    }
    Ok(count)
}

/// Current compiled-source paths per (project relation key, semantic profile).
pub(super) type ProfileSourcePaths =
    BTreeMap<([u8; 32], backend_semantic::vocabulary::LanguageProfile), BTreeSet<String>>;

/// The persisted semantic source content identity of every current file that
/// states one, per (project relation key, semantic profile).
///
/// A file scanned before identities were persisted - or a file whose bytes
/// could not be read - is absent from the inner map, so identity comparison
/// is trusted only when it covers the profile's whole path set.
pub(super) type ProfileSourceIdentities = BTreeMap<
    ([u8; 32], backend_semantic::vocabulary::LanguageProfile),
    BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>,
>;

pub(super) fn profile_source_paths(
    sources: &IndexedSources,
) -> Result<ProfileSourcePaths, BuiltinModelError> {
    let mut paths = BTreeMap::new();
    for record in &sources.files {
        let Some(file) = record.1.file_fields() else {
            continue;
        };
        let Some(profile) = super::super::ingest::source_profile(std::path::Path::new(file.path))
            .map_err(BuiltinModelError)?
        else {
            continue;
        };
        paths
            .entry((file.project, profile))
            .or_insert_with(BTreeSet::new)
            .insert(file.path.to_owned());
    }
    Ok(paths)
}

/// Maps every (project, semantic profile) pair to each current file's
/// persisted `SourceFactDomain` content identity.
pub(super) fn profile_source_identities(
    sources: &IndexedSources,
) -> Result<ProfileSourceIdentities, BuiltinModelError> {
    let mut identities = BTreeMap::new();
    for record in &sources.files {
        let Some(file) = record.1.file_fields() else {
            continue;
        };
        let Some(identity) = file.source_identity else {
            continue;
        };
        let Some(profile) = super::super::ingest::source_profile(std::path::Path::new(file.path))
            .map_err(BuiltinModelError)?
        else {
            continue;
        };
        identities
            .entry((file.project, profile))
            .or_insert_with(BTreeMap::new)
            .insert(file.path.to_owned(), identity);
    }
    Ok(identities)
}

fn structural_callable(kind: DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Function | DeclarationKind::Method | DeclarationKind::Constructor
    )
}

fn structural_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn structural_ident_boundary_before(excerpt: &str, at: usize) -> bool {
    at == 0 || !structural_ident_byte(excerpt.as_bytes()[at - 1])
}

fn structural_call_open_paren(excerpt: &str, after_name: usize) -> bool {
    excerpt[after_name..]
        .chars()
        .next()
        .is_some_and(|ch| ch == '(')
}

fn structural_fn_declarator_before(excerpt: &str, name_at: usize) -> bool {
    let prefix = excerpt[..name_at].trim_end();
    for keyword in ["fn", "def", "function"] {
        if prefix.ends_with(keyword) && prefix.len() >= keyword.len() {
            let keyword_at = prefix.len() - keyword.len();
            if keyword_at == 0 || !structural_ident_byte(prefix.as_bytes()[keyword_at - 1]) {
                return true;
            }
        }
    }
    false
}

/// Returns the first `{` that opens the declaration body, skipping comments
/// and string/char literals so a signature like `fn parse_config(` is never
/// scanned as a call site.
fn structural_body_start(excerpt: &str) -> Option<usize> {
    let bytes = excerpt.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = index.saturating_add(2).min(bytes.len());
            }
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == b'"' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'\'' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == b'\'' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'{' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// Returns whether `excerpt` contains a syntactic call to `callee`.
///
/// Only the declaration body is scanned. Occurrences inside line or block
/// comments, string literals, and char literals are ignored, and a function
/// declarator such as `fn parse_config(` is never treated as a call.
pub(super) fn structural_excerpt_calls(excerpt: &str, callee: &str) -> bool {
    if callee.is_empty() {
        return false;
    }
    let scan_from = structural_body_start(excerpt).unwrap_or(0);
    let body = &excerpt[scan_from..];
    let callee_len = callee.len();
    let mut index = 0;
    while index < body.len() {
        match body.as_bytes()[index] {
            b'/' if body.as_bytes().get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < body.len() && body.as_bytes()[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if body.as_bytes().get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < body.len()
                    && !(body.as_bytes()[index] == b'*' && body.as_bytes()[index + 1] == b'/')
                {
                    index += 1;
                }
                index = index.saturating_add(2).min(body.len());
            }
            b'"' => {
                index += 1;
                while index < body.len() {
                    if body.as_bytes()[index] == b'\\' {
                        index = index.saturating_add(2).min(body.len());
                        continue;
                    }
                    if body.as_bytes()[index] == b'"' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'\'' => {
                index += 1;
                while index < body.len() {
                    if body.as_bytes()[index] == b'\\' {
                        index = index.saturating_add(2).min(body.len());
                        continue;
                    }
                    if body.as_bytes()[index] == b'\'' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            _ if body[index..].starts_with(callee)
                && structural_ident_boundary_before(body, index)
                && structural_call_open_paren(body, index + callee_len)
                && !structural_fn_declarator_before(body, index) =>
            {
                return true;
            }
            _ => index += 1,
        }
    }
    false
}

struct CallSite {
    name: String,
    qualifier: Option<String>,
}

enum ImportBindingKind {
    Value { specifier: String, exported: String },
    Qualifier { specifier: String },
}

struct ImportBinding {
    local: String,
    kind: ImportBindingKind,
}

struct IndexedProjectFile {
    path: String,
    declarations: Vec<backend_compile::SourceDeclaration>,
}

fn parse_import_binding(declaration: &backend_compile::SourceDeclaration) -> Option<ImportBinding> {
    if declaration.kind() != DeclarationKind::Import {
        return None;
    }
    let lines = declaration.signature().split('\n').collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    match lines[0] {
        "value" if lines.len() == 3 => Some(ImportBinding {
            local: declaration.name().to_owned(),
            kind: ImportBindingKind::Value {
                specifier: lines[1].to_owned(),
                exported: lines[2].to_owned(),
            },
        }),
        "qualifier" if lines.len() == 2 => Some(ImportBinding {
            local: declaration.name().to_owned(),
            kind: ImportBindingKind::Qualifier {
                specifier: lines[1].to_owned(),
            },
        }),
        _ => None,
    }
}

fn identifier_immediately_before_end(prefix: &str) -> Option<String> {
    let mut end = prefix.len();
    while end > 0 && !structural_ident_byte(prefix.as_bytes()[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && structural_ident_byte(prefix.as_bytes()[start - 1]) {
        start -= 1;
    }
    let qualifier = &prefix[start..end];
    (!qualifier.is_empty()).then(|| qualifier.to_owned())
}

fn structural_call_qualifier(excerpt: &str, name_at: usize) -> Option<String> {
    let before = excerpt[..name_at].trim_end();
    if before.ends_with("::") {
        return identifier_immediately_before_end(before[..before.len() - 2].trim_end());
    }
    if before.ends_with('.') {
        return identifier_immediately_before_end(before[..before.len() - 1].trim_end());
    }
    None
}

fn structural_excerpt_call_sites(excerpt: &str) -> Vec<CallSite> {
    let scan_from = structural_body_start(excerpt).unwrap_or(0);
    let body = &excerpt[scan_from..];
    let mut sites = Vec::new();
    let mut index = 0;
    while index < body.len() {
        match body.as_bytes()[index] {
            b'/' if body.as_bytes().get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < body.len() && body.as_bytes()[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if body.as_bytes().get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < body.len()
                    && !(body.as_bytes()[index] == b'*' && body.as_bytes()[index + 1] == b'/')
                {
                    index += 1;
                }
                index = index.saturating_add(2).min(body.len());
            }
            b'"' => {
                index += 1;
                while index < body.len() {
                    if body.as_bytes()[index] == b'\\' {
                        index = index.saturating_add(2).min(body.len());
                        continue;
                    }
                    if body.as_bytes()[index] == b'"' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'\'' => {
                index += 1;
                while index < body.len() {
                    if body.as_bytes()[index] == b'\\' {
                        index = index.saturating_add(2).min(body.len());
                        continue;
                    }
                    if body.as_bytes()[index] == b'\'' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            _ => {
                if let Some(name) = identifier_at(body, index) {
                    let name_at = index;
                    let after_name = index + name.len();
                    if structural_ident_boundary_before(body, index)
                        && structural_call_open_paren(body, after_name)
                        && !structural_fn_declarator_before(body, index)
                    {
                        let absolute_at = scan_from + name_at;
                        sites.push(CallSite {
                            name: name.to_owned(),
                            qualifier: structural_call_qualifier(excerpt, absolute_at),
                        });
                        index = after_name;
                        continue;
                    }
                }
                index += 1;
            }
        }
    }
    sites
}

fn identifier_at(body: &str, at: usize) -> Option<&str> {
    if at >= body.len() || !structural_ident_byte(body.as_bytes()[at]) {
        return None;
    }
    let mut end = at + 1;
    while end < body.len() && structural_ident_byte(body.as_bytes()[end]) {
        end += 1;
    }
    body.get(at..end)
}

const SOURCE_EXTENSIONS: &[&str] = &[
    "",
    ".ts",
    ".tsx",
    ".js",
    ".jsx",
    ".mts",
    ".cts",
    ".mjs",
    ".cjs",
    ".py",
    ".rs",
];

const INDEX_EXTENSIONS: &[&str] = &[
    "/index.ts",
    "/index.tsx",
    "/index.js",
    "/index.jsx",
    "/index.mts",
    "/index.cts",
    "/index.mjs",
    "/index.cjs",
];

fn normalize_relative_path(base: &Path, specifier: &str) -> PathBuf {
    let mut parts = base
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect::<Vec<_>>();
    for component in Path::new(specifier).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                parts.push(component);
            }
        }
    }
    parts.iter().collect()
}

fn push_resolved_candidate(
    base: &str,
    project_paths: &BTreeSet<String>,
    out: &mut BTreeSet<String>,
) {
    let normalized = base.replace('\\', "/");
    if project_paths.contains(&normalized) {
        out.insert(normalized.clone());
    }
    for extension in SOURCE_EXTENSIONS {
        let candidate = format!("{normalized}{extension}");
        if project_paths.contains(&candidate) {
            out.insert(candidate);
        }
    }
    for extension in INDEX_EXTENSIONS {
        let candidate = format!("{normalized}{extension}");
        if project_paths.contains(&candidate) {
            out.insert(candidate);
        }
    }
    let rs_mod = format!("{normalized}/mod.rs");
    if project_paths.contains(&rs_mod) {
        out.insert(rs_mod);
    }
}

fn resolve_rust_specifier(
    specifier: &str,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
) -> BTreeSet<String> {
    let caller_dir = Path::new(caller_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    let mut remainder = specifier;
    let mut base = caller_dir.clone();
    while remainder.starts_with("super::") {
        remainder = remainder.trim_start_matches("super::");
        base = base.parent().unwrap_or(Path::new("")).to_path_buf();
    }
    if let Some(stripped) = remainder.strip_prefix("crate::") {
        remainder = stripped;
        base = PathBuf::from("src");
    } else if let Some(stripped) = remainder.strip_prefix("self::") {
        remainder = stripped;
    }
    let path = remainder.replace("::", "/");
    let mut resolved = BTreeSet::new();
    push_resolved_candidate(&format!("src/{path}"), project_paths, &mut resolved);
    push_resolved_candidate(&format!("src/{path}/mod"), project_paths, &mut resolved);
    if !base.as_os_str().is_empty() {
        push_resolved_candidate(
            base.join(&path).to_string_lossy().as_ref(),
            project_paths,
            &mut resolved,
        );
        push_resolved_candidate(
            base.join(&path).join("mod").to_string_lossy().as_ref(),
            project_paths,
            &mut resolved,
        );
    }
    resolved
}

fn is_dotted_module_specifier(specifier: &str) -> bool {
    !specifier.is_empty()
        && !specifier.starts_with('.')
        && specifier.contains('.')
        && !specifier.contains('/')
        && !specifier.contains('\\')
        && specifier.split('.').all(|segment| !segment.is_empty())
}

fn push_dotted_module_paths(
    specifier: &str,
    project_paths: &BTreeSet<String>,
    resolved: &mut BTreeSet<String>,
) {
    if !is_dotted_module_specifier(specifier) {
        return;
    }
    let slashed = specifier.replace('.', "/");
    let py_module = format!("{slashed}.py");
    let py_init = format!("{slashed}/__init__.py");
    for path in project_paths {
        if path == &py_module
            || path.ends_with(&format!("/{py_module}"))
            || path == &py_init
            || path.ends_with(&format!("/{py_init}"))
        {
            resolved.insert(path.clone());
        }
    }
}

fn is_go_import_specifier(specifier: &str) -> bool {
    !specifier.is_empty()
        && !specifier.starts_with('.')
        && specifier.contains('/')
        && !specifier.contains("::")
        && !specifier.split('/').any(|segment| segment.is_empty())
}

fn go_import_parent_matches_suffix(parent: &str, suffix: &str) -> bool {
    parent == suffix || parent.ends_with(&format!("/{suffix}"))
}

fn push_go_import_paths(
    specifier: &str,
    project_paths: &BTreeSet<String>,
    resolved: &mut BTreeSet<String>,
) {
    if !is_go_import_specifier(specifier) {
        return;
    }
    let segments = specifier.split('/').collect::<Vec<_>>();
    for suffix_len in (1..=segments.len()).rev() {
        let suffix = segments[segments.len() - suffix_len..].join("/");
        let mut matched = false;
        for path in project_paths {
            if !path.ends_with(".go") {
                continue;
            }
            let parent = Path::new(path)
                .parent()
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if go_import_parent_matches_suffix(&parent, &suffix) {
                resolved.insert(path.clone());
                matched = true;
            }
        }
        if matched {
            return;
        }
    }
}

pub(crate) fn resolve_specifier_paths(
    specifier: &str,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
) -> BTreeSet<String> {
    if specifier.starts_with("./") || specifier.starts_with("../") {
        let caller_dir = Path::new(caller_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(""));
        let normalized = normalize_relative_path(&caller_dir, specifier)
            .to_string_lossy()
            .replace('\\', "/");
        let mut resolved = BTreeSet::new();
        push_resolved_candidate(&normalized, project_paths, &mut resolved);
        return resolved;
    }
    if specifier.contains("::")
        || specifier.starts_with("crate::")
        || specifier.starts_with("self::")
        || specifier.starts_with("super::")
    {
        return resolve_rust_specifier(specifier, caller_path, project_paths);
    }
    let caller_dir = Path::new(caller_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    let mut resolved = BTreeSet::new();
    for extension in SOURCE_EXTENSIONS {
        let candidate = caller_dir
            .join(format!("{specifier}{extension}"))
            .to_string_lossy()
            .replace('\\', "/");
        if project_paths.contains(&candidate) {
            resolved.insert(candidate);
        }
    }
    for path in project_paths {
        if Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            == Some(specifier)
        {
            resolved.insert(path.clone());
        }
    }
    push_dotted_module_paths(specifier, project_paths, &mut resolved);
    push_go_import_paths(specifier, project_paths, &mut resolved);
    push_rust_module_paths(specifier, project_paths, &mut resolved);
    resolved
}

fn is_rust_module_path_specifier(specifier: &str) -> bool {
    !specifier.is_empty()
        && !specifier.starts_with('.')
        && specifier.contains('/')
        && !specifier.contains("::")
        && !specifier.split('/').any(str::is_empty)
}

fn push_rust_module_paths(
    specifier: &str,
    project_paths: &BTreeSet<String>,
    resolved: &mut BTreeSet<String>,
) {
    if !is_rust_module_path_specifier(specifier) {
        return;
    }
    let rs_file = format!("{specifier}.rs");
    let mod_rs = format!("{specifier}/mod.rs");
    for path in project_paths {
        if path == &rs_file || path.ends_with(&format!("/{rs_file}")) {
            resolved.insert(path.clone());
        }
        if path == &mod_rs || path.ends_with(&format!("/{mod_rs}")) {
            resolved.insert(path.clone());
        }
    }
}

fn declares_a_nominal_type(kind: DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Class
            | DeclarationKind::Struct
            | DeclarationKind::Enum
            | DeclarationKind::Interface
            | DeclarationKind::Trait
            | DeclarationKind::Type
    )
}

fn declaration_matches_type_name(declaration: &backend_compile::SourceDeclaration, type_name: &str) -> bool {
    declares_a_nominal_type(declaration.kind()) && declaration.name() == type_name
}

fn declaration_matches_callable_on_type(
    declaration: &backend_compile::SourceDeclaration,
    type_name: &str,
    call_name: &str,
) -> bool {
    if !structural_callable(declaration.kind()) || declaration.name() != call_name {
        return false;
    }
    match declaration.container() {
        Container::Enclosing { name, .. } | Container::Attached { type_name: name } => {
            name == type_name
        }
        Container::Module => false,
    }
}

fn same_file_callable_coordinates(
    declarations: &[backend_compile::SourceDeclaration],
    containment: &FileContainment<'_>,
    call_name: &str,
) -> BTreeSet<String> {
    declarations
        .iter()
        .filter(|declaration| {
            structural_callable(declaration.kind()) && declaration.name() == call_name
        })
        .map(|declaration| containment.coordinate(declaration))
        .collect()
}

fn declaration_coordinate_in_file(
    label: &str,
    file: &IndexedProjectFile,
    project_key: [u8; 32],
    declaration: &backend_compile::SourceDeclaration,
) -> String {
    let containment = FileContainment::new(
        label,
        &file.path,
        project_key,
        &file.declarations,
    );
    containment.coordinate(declaration)
}

fn cross_file_callable_coordinates(
    label: &str,
    project_key: [u8; 32],
    caller_path: &str,
    imports: &[ImportBinding],
    call: &CallSite,
    project_paths: &BTreeSet<String>,
    files_by_path: &BTreeMap<String, &IndexedProjectFile>,
) -> BTreeSet<String> {
    let mut candidates = BTreeSet::new();
    for import in imports {
        match &import.kind {
            ImportBindingKind::Qualifier { specifier } => {
                if call.qualifier.as_deref() != Some(import.local.as_str()) {
                    continue;
                }
                let resolved_paths =
                    resolve_specifier_paths(specifier, caller_path, project_paths);
                for path in resolved_paths {
                    let Some(file) = files_by_path.get(&path) else {
                        continue;
                    };
                    for declaration in &file.declarations {
                        if structural_callable(declaration.kind())
                            && declaration.name() == call.name
                        {
                            candidates.insert(declaration_coordinate_in_file(
                                label,
                                file,
                                project_key,
                                declaration,
                            ));
                        }
                    }
                }
            }
            ImportBindingKind::Value { specifier, exported } => {
                let resolved_paths =
                    resolve_specifier_paths(specifier, caller_path, project_paths);
                for path in resolved_paths {
                    let Some(file) = files_by_path.get(&path) else {
                        continue;
                    };
                    let exported_is_type = file.declarations.iter().any(|declaration| {
                        declaration_matches_type_name(declaration, exported)
                    });
                    if exported_is_type {
                        for declaration in &file.declarations {
                            if declaration_matches_callable_on_type(
                                declaration,
                                exported,
                                &call.name,
                            ) {
                                candidates.insert(declaration_coordinate_in_file(
                                    label,
                                    file,
                                    project_key,
                                    declaration,
                                ));
                            }
                        }
                    } else if call.qualifier.is_none() && import.local == call.name {
                        for declaration in &file.declarations {
                            if structural_callable(declaration.kind())
                                && declaration.name() == exported
                            {
                                candidates.insert(declaration_coordinate_in_file(
                                    label,
                                    file,
                                    project_key,
                                    declaration,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    candidates
}

fn resolve_call_targets(
    label: &str,
    project_key: [u8; 32],
    caller_file: &IndexedProjectFile,
    imports: &[ImportBinding],
    call: &CallSite,
    project_paths: &BTreeSet<String>,
    files_by_path: &BTreeMap<String, &IndexedProjectFile>,
) -> BTreeSet<String> {
    let caller_containment = FileContainment::new(
        label,
        &caller_file.path,
        project_key,
        &caller_file.declarations,
    );
    let same_file = same_file_callable_coordinates(
        &caller_file.declarations,
        &caller_containment,
        &call.name,
    );
    if !same_file.is_empty() {
        return same_file;
    }
    cross_file_callable_coordinates(
        label,
        project_key,
        &caller_file.path,
        imports,
        call,
        project_paths,
        files_by_path,
    )
}

/// Same-file and import-resolved call coordinate pairs inferred from bounded
/// declaration excerpts.
pub(crate) fn structural_call_coordinate_pairs(
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
) -> Result<Vec<(String, String)>, BuiltinModelError> {
    let project = sources
        .projects
        .values()
        .find(|project| project.package == package)
        .ok_or_else(|| {
            BuiltinModelError(
                "structural call graph package is absent from indexed sources".to_owned(),
            )
        })?;
    let project_key = project.package.to_bytes();
    let mut project_paths = BTreeSet::new();
    let mut static_files = Vec::new();
    for (_, record) in &sources.files {
        let file = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("expected structural source file".to_owned()))?;
        if file.project != project_key {
            continue;
        }
        project_paths.insert(file.path.to_owned());
        static_files.push(IndexedProjectFile {
            path: file.path.to_owned(),
            declarations: file.declarations.to_vec(),
        });
    }
    let mut files_by_path = BTreeMap::new();
    for file in &static_files {
        files_by_path.insert(file.path.clone(), file);
    }
    let mut pairs = BTreeSet::new();
    for file in &static_files {
        let imports = file
            .declarations
            .iter()
            .filter_map(parse_import_binding)
            .collect::<Vec<_>>();
        for caller in file.declarations.iter() {
            if !structural_callable(caller.kind()) {
                continue;
            }
            let Some(excerpt) = caller.source_excerpt().text() else {
                continue;
            };
            let caller_containment = FileContainment::new(
                &project.label,
                &file.path,
                project_key,
                &file.declarations,
            );
            let caller_coordinate = caller_containment.coordinate(caller);
            for call in structural_excerpt_call_sites(excerpt) {
                let targets = resolve_call_targets(
                    &project.label,
                    project_key,
                    file,
                    &imports,
                    &call,
                    &project_paths,
                    &files_by_path,
                );
                if targets.len() != 1 {
                    continue;
                }
                let callee_coordinate = targets.into_iter().next().expect("exactly one target");
                if callee_coordinate == caller_coordinate {
                    continue;
                }
                pairs.insert((caller_coordinate.clone(), callee_coordinate));
            }
        }
    }
    Ok(pairs.into_iter().collect())
}

struct ParsedDeclarationCoordinate {
    path: String,
    line: u32,
    name: String,
}

fn parsed_declaration_coordinate(coordinate: &str) -> Option<ParsedDeclarationCoordinate> {
    let (prefix, name) = coordinate.rsplit_once("::")?;
    if name.is_empty() {
        return None;
    }
    let (path_prefix, line_text) = prefix.rsplit_once(':')?;
    let line = line_text.parse().ok()?;
    let path = path_prefix
        .split_once("::")
        .map_or(path_prefix, |(_, path)| path);
    if path.is_empty() {
        return None;
    }
    Some(ParsedDeclarationCoordinate {
        path: path.to_owned(),
        line,
        name: name.to_owned(),
    })
}

/// Resolves one structural declaration coordinate against the published view.
///
/// A row whose label is exactly the coordinate is preferred. Otherwise a
/// semantic-shaped row is matched by captured source path, line, and name.
pub(crate) fn view_row_for_structural_coordinate(
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    coordinate: &str,
) -> Option<RowId> {
    let mut semantic_matches = Vec::new();
    for row in view.rows() {
        if row.package != Some(package) {
            continue;
        }
        if row.label == coordinate {
            return Some(row.id);
        }
        let Some(parsed) = parsed_declaration_coordinate(coordinate) else {
            continue;
        };
        let Some(location) = row.source.captured() else {
            continue;
        };
        if location.path() != parsed.path || location.start_line() != parsed.line {
            continue;
        }
        if row.label.ends_with(&format!("::{}", parsed.name)) || row.label == parsed.name {
            semantic_matches.push(row.id);
        }
    }
    if semantic_matches.len() == 1 {
        semantic_matches.pop()
    } else {
        None
    }
}

/// Maps structural call coordinate pairs onto view rows, skipping pairs whose
/// endpoints are absent or ambiguous in the published view.
pub(crate) fn structural_call_graph_relations_mapped(
    view: &backend_engine::ViewRoot,
    pairs: &[(String, String)],
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Vec<backend_engine::GraphRelation> {
    let mut relations = BTreeSet::new();
    for (caller_coordinate, callee_coordinate) in pairs {
        let Some(caller_id) = view_row_for_structural_coordinate(view, package, caller_coordinate)
        else {
            continue;
        };
        let Some(callee_id) = view_row_for_structural_coordinate(view, package, callee_coordinate)
        else {
            continue;
        };
        relations.insert(backend_engine::GraphRelation::new(
            caller_id,
            callee_id,
            backend_library::SemanticLinkKind::Calls,
        ));
    }
    relations
        .into_iter()
        .filter(|relation| {
            if include_incoming {
                relation.from == source_id || relation.to == source_id
            } else {
                relation.from == source_id
            }
        })
        .collect()
}

/// Same-file and import-resolved call edges inferred from bounded declaration
/// excerpts when no complete semantic publication supplies compiler-proven
/// `Calls` links.
pub(crate) fn structural_call_graph_relations(
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Result<Option<Vec<backend_engine::GraphRelation>>, BuiltinModelError> {
    let _source_row = view.row(source_id).ok_or_else(|| {
        BuiltinModelError("structural call graph source is absent from the view".to_owned())
    })?;
    let mut coordinate_ids = BTreeMap::<String, RowId>::new();
    for row in view.rows() {
        if row.package == Some(package) {
            coordinate_ids.insert(row.label.clone(), row.id);
        }
    }
    let mut relations = BTreeSet::new();
    for (caller_coordinate, callee_coordinate) in structural_call_coordinate_pairs(sources, package)?
    {
        let caller_id = coordinate_ids.get(&caller_coordinate).ok_or_else(|| {
            BuiltinModelError(
                "structural call graph caller is absent from the published view".to_owned(),
            )
        })?;
        let callee_id = coordinate_ids.get(&callee_coordinate).ok_or_else(|| {
            BuiltinModelError(
                "structural call graph callee is absent from the published view".to_owned(),
            )
        })?;
        relations.insert(backend_engine::GraphRelation::new(
            *caller_id,
            *callee_id,
            backend_library::SemanticLinkKind::Calls,
        ));
    }
    let relations = relations
        .into_iter()
        .filter(|relation| {
            if include_incoming {
                relation.from == source_id || relation.to == source_id
            } else {
                relation.from == source_id
            }
        })
        .collect::<Vec<_>>();
    if relations.is_empty() {
        return Ok(None);
    }
    if relations.len() > usize::from(backend_engine::QueryLimit::MAX) {
        return Err(BuiltinModelError(
            "structural call graph exceeds the bounded result contract".to_owned(),
        ));
    }
    Ok(Some(relations))
}

/// Incoming call sites for one declaration when semantic references are absent.
pub(crate) fn structural_reference_facts(
    view: &backend_engine::ViewRoot,
    sources: &super::super::IndexedSources,
    target: &str,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let target_row = view
        .rows()
        .iter()
        .find(|row| row.label == target)
        .ok_or_else(|| {
            BuiltinModelError("structural references target is absent from the view".to_owned())
        })?;
    let backend_engine::RowId::Symbol(target_symbol) = target_row.id else {
        return Err(BuiltinModelError(
            "structural references target is not a declaration row".to_owned(),
        ));
    };
    let Some(package) = target_row.package else {
        return Err(BuiltinModelError(
            "structural references target is not attributed to a package".to_owned(),
        ));
    };
    let Some(relations) =
        structural_call_graph_relations(view, sources, package, target_row.id, true)?
    else {
        return Ok(Vec::new());
    };
    let target_name = target
        .rsplit("::")
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            BuiltinModelError("structural references target has no declaration name".to_owned())
        })?;
    let target_identity = structural_symbol_identity(target_symbol);
    let mut facts = Vec::new();
    for relation in relations {
        if relation.to != target_row.id
            || relation.relation != backend_library::SemanticLinkKind::Calls
        {
            continue;
        }
        let site_row = view.row(relation.from).ok_or_else(|| {
            BuiltinModelError("structural references site is absent from the view".to_owned())
        })?;
        let backend_engine::RowId::Symbol(site_symbol) = site_row.id else {
            return Err(BuiltinModelError(
                "structural references site is not a declaration row".to_owned(),
            ));
        };
        let (start, end) = site_row
            .excerpt
            .text()
            .and_then(|excerpt| structural_call_span(excerpt, target_name))
            .map(|(start, end)| {
                (
                    u32::try_from(start).unwrap_or(u32::MAX),
                    u32::try_from(end).unwrap_or(u32::MAX),
                )
            })
            .unwrap_or((0, target_name.len().min(u32::MAX as usize) as u32));
        let source = match site_row.source.captured() {
            Some(location) => Some(backend_engine::SemanticSourceSpan {
                file: backend_engine::ProductText::new(location.path())
                    .map_err(|error| {
                        BuiltinModelError(format!("structural references path: {error:?}"))
                    })?,
                start,
                end,
            }),
            None => None,
        };
        facts.push(backend_engine::ReferenceFact {
            site: site_symbol,
            target: backend_engine::SemanticLinkTarget::Local {
                declaration: target_identity,
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_engine::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Syntactic,
                source,
            },
        });
        if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
            return Err(BuiltinModelError(
                "structural references exceed the bounded result contract".to_owned(),
            ));
        }
    }
    Ok(facts)
}

pub(crate) fn structural_symbol_identity(
    symbol: backend_engine::SymbolKey,
) -> backend_engine::SemanticDeclarationIdentity {
    let bytes = symbol.as_bytes();
    let mut family = [0_u8; 16];
    let mut variant = [0_u8; 16];
    family[..bytes.len().min(16)].copy_from_slice(&bytes[..bytes.len().min(16)]);
    if bytes.len() > 16 {
        variant[..16].copy_from_slice(&bytes[bytes.len() - 16..]);
    }
    backend_engine::SemanticDeclarationIdentity { family, variant }
}

pub(crate) fn structural_call_span(excerpt: &str, callee: &str) -> Option<(usize, usize)> {
    let scan_from = structural_body_start(excerpt).unwrap_or(0);
    let body = &excerpt[scan_from..];
    let needle = format!("{callee}(");
    let relative = body.find(&needle)?;
    let start = scan_from + relative;
    Some((start, start + needle.len() - 1))
}
