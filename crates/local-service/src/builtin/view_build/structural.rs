use super::super::{BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES};
use super::identity::{declaration_coordinate, declaration_symbol};
use super::semantic_profile_is_complete;
use backend_engine::{DeclarationKind, RowId, RowIdentityPreimage};
use std::collections::{BTreeMap, BTreeSet};

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
