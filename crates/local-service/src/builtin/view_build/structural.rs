use super::super::{
    BuiltinModelError, IndexedSources, MAX_REBUILD_PACKAGES, check_search_preparation,
};
use super::identity::{declaration_coordinate, declaration_symbol};
use super::semantic_profile_is_complete;
use backend_compile::{Container, SourceBinding, SourceBindingIdentity};
use backend_engine::{DeclarationKind, RowId, RowIdentityPreimage};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn duplicate_declaration_coordinates(
    sources: &IndexedSources,
    record: &super::super::ProductSourceRecord,
    containment: &FileContainment<'_>,
    cancelled: Option<&AtomicBool>,
) -> Result<BTreeSet<String>, BuiltinModelError> {
    let mut counts = BTreeMap::<String, u32>::new();
    visit_complete_declarations(sources, record, |declaration| {
        check_search_preparation(cancelled, "structural-duplicate")?;
        let coordinate = containment.coordinate(declaration);
        let count = counts.entry(coordinate).or_default();
        *count = count.saturating_add(1);
        Ok(())
    })?;
    Ok(counts
        .into_iter()
        .filter_map(|(coordinate, count)| (count > 1).then_some(coordinate))
        .collect())
}

/// Visits one file's exact complete declarations without hydrating its page
/// tree or the project's facts relation. Legacy compact rows are accepted
/// only when their retention marker proves that the row itself is complete.
pub(super) fn visit_complete_declarations(
    sources: &IndexedSources,
    record: &super::super::ProductSourceRecord,
    mut visit: impl FnMut(&backend_compile::SourceDeclaration) -> Result<(), BuiltinModelError>,
) -> Result<(), BuiltinModelError> {
    let file = record.file_fields().ok_or_else(|| {
        BuiltinModelError("complete source facts requested for a non-file row".to_owned())
    })?;
    if let Some(snapshot) = &sources.source_snapshot {
        match snapshot
            .admit_complete_file_facts(record)
            .map_err(|error| BuiltinModelError(format!("admit complete source facts: {error}")))?
        {
            Some(backend_engine::ProductSourceFileFactsAdmission::Unavailable(_reason)) => {
                // The row retains the reason; there is no extracted declaration to visit.
            }
            Some(backend_engine::ProductSourceFileFactsAdmission::InlineComplete(inline)) => {
                for declaration in inline.declarations() {
                    visit(declaration)?;
                }
            }
            Some(backend_engine::ProductSourceFileFactsAdmission::PagedVerified(mut paged)) => {
                paged
                    .visit_pages(|page| {
                        for index in 0..page.len() {
                            let declaration = page
                                .declaration(index)
                                .ok_or_else(|| {
                                    "verified source-facts page item is missing".to_owned()
                                })?
                                .to_owned()?;
                            visit(&declaration).map_err(|error| error.to_string())?;
                        }
                        Ok(())
                    })
                    .map_err(|error| {
                        BuiltinModelError(format!("visit complete source-facts pages: {error}"))
                    })?;
            }
            None => {
                for declaration in file.declarations.iter() {
                    visit(declaration)?;
                }
            }
        }
        return Ok(());
    }
    if file
        .admitted_unavailable_reason()
        .map_err(BuiltinModelError)?
        .is_some()
    {
        return Ok(());
    }
    if !file.retention.is_complete() {
        return Err(BuiltinModelError(
            "compact source row is incomplete and has no selected complete facts relation"
                .to_owned(),
        ));
    }
    for declaration in file.declarations.iter() {
        visit(declaration)?;
    }
    Ok(())
}

/// Collects one source file's complete declarations for a consumer that must
/// retain them while projecting that file's semantic image. The selected
/// relation is paged and authenticated first; only this single file is
/// materialized, so a project-wide semantic refresh never hydrates every
/// declaration page at once.
pub(super) fn complete_declarations_for_file(
    sources: &IndexedSources,
    record: &super::super::ProductSourceRecord,
) -> Result<Vec<backend_compile::SourceDeclaration>, BuiltinModelError> {
    complete_declarations_for_file_controlled(sources, record, None)
}

pub(super) fn complete_declarations_for_file_controlled(
    sources: &IndexedSources,
    record: &super::super::ProductSourceRecord,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<backend_compile::SourceDeclaration>, BuiltinModelError> {
    check_search_preparation(cancelled, "materialize-source-declarations")?;
    let copy = |input: &[backend_compile::SourceDeclaration]| {
        let mut declarations = Vec::with_capacity(input.len());
        for declaration in input {
            check_search_preparation(cancelled, "materialize-declaration")?;
            declarations.push(declaration.clone());
        }
        Ok(declarations)
    };
    let file = record.file_fields().ok_or_else(|| {
        BuiltinModelError("complete source facts requested for a non-file row".to_owned())
    })?;
    if let Some(snapshot) = &sources.source_snapshot {
        match snapshot
            .admit_complete_file_facts(record)
            .map_err(|error| BuiltinModelError(format!("admit complete source facts: {error}")))?
        {
            Some(backend_engine::ProductSourceFileFactsAdmission::Unavailable(_reason)) => {
                return Ok(Vec::new());
            }
            Some(backend_engine::ProductSourceFileFactsAdmission::InlineComplete(inline)) => {
                return copy(inline.declarations());
            }
            Some(backend_engine::ProductSourceFileFactsAdmission::PagedVerified(mut paged)) => {
                let expected_count = usize::try_from(paged.declaration_count()).map_err(|_| {
                    BuiltinModelError("source facts declaration count exceeds usize".to_owned())
                })?;
                let mut declarations = Vec::with_capacity(expected_count);
                paged
                    .visit_pages(|page| {
                        check_search_preparation(cancelled, "materialize-page")
                            .map_err(|error| error.to_string())?;
                        for index in 0..page.len() {
                            check_search_preparation(cancelled, "materialize-declaration")
                                .map_err(|error| error.to_string())?;
                            let declaration = page
                                .declaration(index)
                                .ok_or_else(|| {
                                    "verified source-facts page item is missing".to_owned()
                                })?
                                .to_owned()?;
                            declarations.push(declaration);
                        }
                        Ok(())
                    })
                    .map_err(|error| {
                        BuiltinModelError(format!("visit complete source-facts pages: {error}"))
                    })?;
                if declarations.len() != expected_count {
                    return Err(BuiltinModelError(
                        "complete source-facts page count changed during visitation".to_owned(),
                    ));
                }
                return Ok(declarations);
            }
            None => return copy(&file.declarations),
        }
    }
    if file
        .admitted_unavailable_reason()
        .map_err(BuiltinModelError)?
        .is_some()
    {
        return Ok(Vec::new());
    }
    if !file.retention.is_complete() {
        return Err(BuiltinModelError(
            "compact source row is incomplete and has no selected complete facts relation"
                .to_owned(),
        ));
    }
    copy(&file.declarations)
}

pub(super) fn is_file_module(declaration: &backend_compile::SourceDeclaration, path: &str) -> bool {
    declaration.kind() == DeclarationKind::Module
        && declaration.line() == 1
        && Path::new(path)
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
    fn of(
        sources: &IndexedSources,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, BuiltinModelError> {
        check_search_preparation(cancelled, "structural-types")?;
        let mut index = Self::default();
        for (_, record) in &sources.files {
            check_search_preparation(cancelled, "structural-types-file")?;
            let Some(file) = record.file_fields() else {
                continue;
            };
            visit_complete_declarations(sources, record, |declaration| {
                check_search_preparation(cancelled, "structural-type")?;
                if !declares_a_type(declaration.kind()) {
                    return Ok(());
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
                Ok(())
            })?;
        }
        Ok(index)
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
    pub(super) parent_binding: Option<SourceBinding>,
    binding_capture: Option<backend_compile::SourceBindingCapture>,
    pub(super) id: RowId,
    pub(super) identity_preimage: Option<RowIdentityPreimage>,
    pub(super) is_file_module: bool,
    /// The exact source binding is represented by its compiler declaration.
    pub(super) compiler_twin: bool,
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
    by_binding: BTreeMap<([u8; 32], SourceBinding), (RowId, backend_compile::SourceBindingCapture)>,
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
        Self::plan(sources, complete, None, None)
    }

    pub(super) fn of_controlled(
        sources: &IndexedSources,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, BuiltinModelError> {
        Self::plan(sources, complete, None, cancelled)
    }

    /// Plans structural rows for `only` these file keys.
    ///
    /// The type index still walks every file. An attached parent declared in
    /// an untouched file keeps that file's coordinate; the row builder resolves
    /// the coordinate against the resident view.
    pub(super) fn of_files(
        sources: &IndexedSources,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
        only: &BTreeSet<[u8; 32]>,
    ) -> Result<Self, BuiltinModelError> {
        Self::plan(sources, complete, Some(only), None)
    }

    fn plan(
        sources: &IndexedSources,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
        only: Option<&BTreeSet<[u8; 32]>>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, BuiltinModelError> {
        let types = ProjectTypeIndex::of(sources, cancelled)?;
        let mut plan = Self::default();
        for (file_key, record) in &sources.files {
            check_search_preparation(cancelled, "structural-plan-file")?;
            if only.is_some_and(|keys| !keys.contains(file_key)) {
                continue;
            }
            let Some(file) = record.file_fields() else {
                continue;
            };
            let project = sources.projects.get(&file.project).ok_or_else(|| {
                BuiltinModelError("structural source refers to a missing project".to_owned())
            })?;
            if semantic_profile_is_complete(
                complete,
                Some(project.package),
                file.path,
                file.python_version,
                file.go_version,
            )? {
                continue;
            }
            let containment =
                FileContainment::for_file(sources, record, &project.label, cancelled)?;
            let duplicate_coordinates =
                duplicate_declaration_coordinates(sources, record, &containment, cancelled)?;
            let mut occurrences = BTreeMap::new();
            let mut declarations = Vec::new();
            visit_complete_declarations(sources, record, |declaration| {
                check_search_preparation(cancelled, "structural-plan-declaration")?;
                declaration
                    .admit_binding_parentage()
                    .map_err(BuiltinModelError)?;
                let coordinate = containment.coordinate(declaration);
                let (id, identity_preimage) = if duplicate_coordinates.contains(&coordinate)
                    && let SourceBindingIdentity::Exact(binding) = declaration.binding_identity()
                {
                    let (name_start, name_end) = binding.name();
                    let preimage = format!("{coordinate}\0binding-v1\0{name_start}:{name_end}");
                    (
                        RowId::Symbol(backend_engine::symbol_key(&preimage)),
                        Some(preimage),
                    )
                } else {
                    declaration_symbol(
                        &coordinate,
                        declaration.kind(),
                        declaration.signature(),
                        duplicate_coordinates.contains(&coordinate),
                        &mut occurrences,
                    )
                };
                if let SourceBindingIdentity::Exact(binding) = declaration.binding_identity()
                    && plan
                        .by_binding
                        .insert((*file_key, binding.binding()), (id, binding))
                        .is_some()
                {
                    return Err(BuiltinModelError(
                        "source repeats an exact binding identity".to_owned(),
                    ));
                }
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
                    binding_capture: match declaration.binding_identity() {
                        SourceBindingIdentity::Exact(capture) => Some(capture),
                        _ => None,
                    },
                    parent_binding: match declaration.container() {
                        Container::Binding { binding } => Some(*binding),
                        _ => None,
                    },
                    id,
                    identity_preimage,
                    is_file_module: is_file_module(declaration, file.path),
                    compiler_twin: false,
                });
                Ok(())
            })?;
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
        if let Some(only) = only {
            for key in only {
                if !plan.files.contains_key(key) {
                    return Err(BuiltinModelError(
                        "structural file splice omitted a requested source".to_owned(),
                    ));
                }
            }
        }
        for (file_key, file) in &plan.files {
            for declaration in &file.declarations {
                check_search_preparation(cancelled, "structural-parent")?;
                if let Some(binding) = declaration.parent_binding {
                    let (parent, parent_capture) =
                        plan.by_binding.get(&(*file_key, binding)).ok_or_else(|| {
                            BuiltinModelError(
                                "exact structural parent binding is absent".to_owned(),
                            )
                        })?;
                    let child = declaration.binding_capture.ok_or_else(|| {
                        BuiltinModelError("exact parent lacks child evidence".to_owned())
                    })?;
                    if *parent == declaration.id || !parent_capture.strictly_contains(child) {
                        return Err(BuiltinModelError(
                            "exact parent does not strictly contain its child owner".to_owned(),
                        ));
                    }
                }
            }
        }
        for symbols in plan.by_coordinate.values_mut() {
            check_search_preparation(cancelled, "structural-sort")?;
            if cancelled.is_some() {
                super::super::query::controlled_sort::sort_unstable_in_place_checked(
                    symbols,
                    |symbol| (structural_parent_rank(symbol.kind), symbol.kind, symbol.id),
                    &mut || check_search_preparation(cancelled, "structural-sort-step"),
                )?;
            } else {
                symbols.sort_unstable_by_key(|symbol| {
                    (structural_parent_rank(symbol.kind), symbol.kind, symbol.id)
                });
            }
        }
        check_search_preparation(cancelled, "structural-plan-complete")?;
        Ok(plan)
    }

    pub(super) fn file(&self, file_key: [u8; 32]) -> Option<&StructuralFilePlan> {
        self.files.get(&file_key)
    }

    /// Assigns compiler-owned identity only to authenticated exact twins.
    /// Parents resolve through the same remapped tables as rows and facts.
    pub(super) fn unify_exact_sites(
        &mut self,
        sources: &IndexedSources,
        exact: &super::exact_sites::ExactDeclarationSites,
    ) -> Result<(), BuiltinModelError> {
        self.unify_exact_sites_controlled(sources, exact, None)
    }

    pub(super) fn unify_exact_sites_controlled(
        &mut self,
        sources: &IndexedSources,
        exact: &super::exact_sites::ExactDeclarationSites,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), BuiltinModelError> {
        check_search_preparation(cancelled, "structural-unify")?;
        let mut remapped = BTreeMap::new();
        for (file_key, record) in &sources.files {
            check_search_preparation(cancelled, "structural-unify-file")?;
            let Some(file) = record.file_fields() else {
                continue;
            };
            let Some(plan) = self.files.get_mut(file_key) else {
                continue;
            };
            for declaration in &mut plan.declarations {
                check_search_preparation(cancelled, "structural-unify-declaration")?;
                let Some(capture) = declaration.binding_capture else {
                    continue;
                };
                let Some(id) = exact.get(file.project, file.path, capture.binding()) else {
                    continue;
                };
                if !matches!(id, RowId::Symbol(_)) {
                    return Err(BuiltinModelError(
                        "exact compiler site is not a declaration".to_owned(),
                    ));
                }
                remapped.insert(declaration.id, id);
                declaration.id = id;
                declaration.identity_preimage = None;
                declaration.compiler_twin = true;
            }
        }
        for symbols in self.by_coordinate.values_mut() {
            for symbol in symbols {
                check_search_preparation(cancelled, "structural-unify-symbol")?;
                if let Some(id) = remapped.get(&symbol.id) {
                    symbol.id = *id;
                }
            }
        }
        for (id, _) in self.by_binding.values_mut() {
            check_search_preparation(cancelled, "structural-unify-binding")?;
            if let Some(mapped) = remapped.get(id) {
                *id = *mapped;
            }
        }
        Ok(())
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

    /// Resolves an exact lexical target only within its owning file. Missing
    /// targets are a capture/storage contract error, never a name fallback.
    pub(super) fn binding_parent_id(
        &self,
        file_key: [u8; 32],
        binding: SourceBinding,
    ) -> Result<RowId, BuiltinModelError> {
        self.by_binding
            .get(&(file_key, binding))
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                BuiltinModelError("exact structural parent binding is absent".to_owned())
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

    /// The retained symbol at this coordinate, with no module fallback.
    ///
    /// [`Self::parent_id`] substitutes the file module when the coordinate was
    /// not planned. A one-file splice must not use that fallback for a parent
    /// that still lives in another file.
    pub(super) fn symbol_for_coordinate(
        &self,
        project: [u8; 32],
        coordinate: &str,
    ) -> Option<RowId> {
        self.by_coordinate
            .get(&(project, coordinate.to_owned()))
            .and_then(|symbols| symbols.first())
            .map(|symbol| symbol.id)
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
    local_types: BTreeMap<String, u32>,
}

impl<'a> FileContainment<'a> {
    fn new(
        label: &'a str,
        path: &'a str,
        project: [u8; 32],
        declarations: &[backend_compile::SourceDeclaration],
    ) -> Self {
        let mut local_types = BTreeMap::new();
        for declaration in declarations {
            if !declares_a_type(declaration.kind()) {
                continue;
            }
            local_types
                .entry(declaration.name().to_owned())
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

    fn for_file(
        sources: &IndexedSources,
        record: &'a super::super::ProductSourceRecord,
        label: &'a str,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, BuiltinModelError> {
        check_search_preparation(cancelled, "structural-containment")?;
        let file = record.file_fields().ok_or_else(|| {
            BuiltinModelError("structural source row changed type during admission".to_owned())
        })?;
        let mut local_types = BTreeMap::<String, u32>::new();
        visit_complete_declarations(sources, record, |declaration| {
            check_search_preparation(cancelled, "structural-containment-declaration")?;
            if declares_a_type(declaration.kind()) {
                local_types
                    .entry(declaration.name().to_owned())
                    .and_modify(|line| *line = (*line).min(declaration.line()))
                    .or_insert(declaration.line());
            }
            Ok(())
        })?;
        Ok(Self {
            label,
            path: file.path,
            project: file.project,
            module_coordinate: format!("{label}::{}", file.path),
            local_types,
        })
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
        if matches!(declaration.container(), Container::Binding { .. }) {
            return None;
        }
        Some(match declaration.container() {
            Container::Module => self.module_coordinate.clone(),
            Container::Binding { .. } => {
                unreachable!("exact targets resolve through binding identities")
            }
            Container::Enclosing { name, line } => {
                declaration_coordinate(self.label, self.path, line.get(), name)
            }
            Container::Attached { type_name } => self
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
    let mut count = sources.projects.len();
    for (_, record) in &sources.files {
        let Some(fields) = record.file_fields() else {
            continue;
        };
        if semantic_profile_is_complete(
            complete,
            sources
                .projects
                .get(&fields.project)
                .map(|project| project.package),
            fields.path,
            fields.python_version,
            fields.go_version,
        )? {
            continue;
        }
        visit_complete_declarations(sources, record, |_| {
            count = count
                .checked_add(1)
                .ok_or_else(|| BuiltinModelError("workspace view row count overflow".to_owned()))?;
            Ok(())
        })?;
    }
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
        let Some(profile) =
            super::super::ingest::source_record_profile(file).map_err(BuiltinModelError)?
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
        let Some(profile) =
            super::super::ingest::source_record_profile(file).map_err(BuiltinModelError)?
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

/// Fresh native profile identities admitted from the same selected source
/// closure. Refused or missing frontiers are deliberately absent from both
/// outputs, so callers cannot use a path-set-only fallback for C/C++.
pub(super) struct NativeFrontierFreshness {
    pub(super) complete: BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    pub(super) identities: ProfileSourceIdentities,
}

pub(super) fn native_frontier_freshness(
    sources: &IndexedSources,
    current_paths: &ProfileSourcePaths,
) -> NativeFrontierFreshness {
    let mut complete = BTreeSet::new();
    let mut identities = ProfileSourceIdentities::new();
    let mut rows = BTreeMap::new();
    for (_, record) in &sources.files {
        if let Some(file) = record.file_fields() {
            rows.insert((file.project, file.path.to_owned()), record);
        }
    }
    let Some(snapshot) = &sources.source_snapshot else {
        return NativeFrontierFreshness {
            complete,
            identities,
        };
    };
    for ((project, profile), paths) in current_paths {
        if !matches!(
            profile,
            backend_semantic::vocabulary::LanguageProfile::C(_)
                | backend_semantic::vocabulary::LanguageProfile::Cxx(_)
                | backend_semantic::vocabulary::LanguageProfile::CSharp(_)
        ) {
            continue;
        }
        let Ok(Some(frontier)) = snapshot.admit_native_frontier(*project, *profile, paths) else {
            continue;
        };
        if !frontier.is_compilable() {
            continue;
        }
        let mut profile_identities = BTreeMap::new();
        let mut row_bindings_valid = true;
        for (path, disposition) in frontier.dispositions() {
            let Some(record) = rows.get(&(*project, path.clone())) else {
                row_bindings_valid = false;
                break;
            };
            let Some(file) = record.file_fields() else {
                row_bindings_valid = false;
                break;
            };
            if file.project != *project
                || file.path != path
                || file.language != profile.language()
                || super::super::ingest::source_profile(std::path::Path::new(path))
                    .ok()
                    .flatten()
                    != Some(*profile)
            {
                row_bindings_valid = false;
                break;
            }
            match disposition {
                backend_engine::builtin::NativePathDisposition::Structural {
                    content,
                    source_identity,
                } => {
                    if file.content_version != *content
                        || file.source_identity != Some(*source_identity)
                        || file.content_version == [0; 32]
                        || !matches!(file.admitted_unavailable_reason(), Ok(None))
                    {
                        row_bindings_valid = false;
                        break;
                    }
                }
                backend_engine::builtin::NativePathDisposition::ClangMalformed {
                    source_identity,
                    content,
                    ..
                } => {
                    if file.content_version != [0; 32]
                        || file.source_identity.is_some()
                        || file.admitted_unavailable_reason().ok().flatten()
                            != Some(backend_engine::SourceUnavailableReason::Unparsed)
                    {
                        row_bindings_valid = false;
                        break;
                    }
                    let Ok(native_source) = snapshot.verified_native_source(&frontier, path) else {
                        row_bindings_valid = false;
                        break;
                    };
                    if native_source.content_digest() != *content
                        || native_source.source_identity() != *source_identity
                        || native_source.project() != *project
                        || native_source.profile() != *profile
                        || native_source.relative_path() != path
                    {
                        row_bindings_valid = false;
                        break;
                    }
                    // Keep one bounded source buffer live at a time. The
                    // selected relation has authenticated the bytes; the
                    // semantic worker reconstructs them again for compilation.
                    profile_identities.insert(path.clone(), *source_identity);
                }
                backend_engine::builtin::NativePathDisposition::CSharpMalformed {
                    source_identity,
                    content,
                    ..
                } => {
                    if profile.language() != backend_engine::SourceLanguage::CSharp
                        || file.content_version != [0; 32]
                        || file.source_identity.is_some()
                        || file.admitted_unavailable_reason().ok().flatten()
                            != Some(backend_engine::SourceUnavailableReason::Unparsed)
                    {
                        row_bindings_valid = false;
                        break;
                    }
                    let Ok(native_source) = snapshot.verified_native_csharp_source(&frontier, path)
                    else {
                        row_bindings_valid = false;
                        break;
                    };
                    if native_source.content_digest() != *content
                        || native_source.source_identity() != *source_identity
                        || native_source.project() != *project
                        || native_source.profile() != *profile
                        || native_source.relative_path() != path
                    {
                        row_bindings_valid = false;
                        break;
                    }
                    profile_identities.insert(path.clone(), *source_identity);
                }
                backend_engine::builtin::NativePathDisposition::Refused(_) => {
                    row_bindings_valid = false;
                    break;
                }
            }
        }
        if row_bindings_valid {
            complete.insert((*project, *profile));
            if !profile_identities.is_empty() {
                identities.insert((*project, *profile), profile_identities);
            }
        }
    }
    NativeFrontierFreshness {
        complete,
        identities,
    }
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

/// How a call's receiver was written. The name-match heuristic has no types,
/// so the written shape is all it can reason from.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CallReceiver {
    /// `name(...)`: no receiver at all.
    Bare,
    /// `a.name(...)`, `a.b.name(...)` or `A::name(...)`: a plain path of
    /// identifiers written directly before the separator.
    Path {
        /// First identifier of the path.
        root: String,
        /// The identifier directly before the separator: the receiver itself,
        /// the type of `a::Type::name()`.
        last: String,
        /// Number of identifiers in the path.
        depth: usize,
    },
    /// The receiver is an expression, not a name: `f(x).name()`,
    /// `xs[0].name()`, `"text".name()`, `1.name()`.
    Expression {
        /// A string, character or number literal: always a builtin type.
        literal: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallSite {
    name: String,
    receiver: CallReceiver,
}

enum ImportBindingKind {
    Value { specifier: String, exported: String },
    Qualifier { specifier: String },
}

struct ImportBinding {
    local: String,
    kind: ImportBindingKind,
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

/// The identifier that ends exactly at `end` in `text`, as `(start, name)`.
fn identifier_ending_at(text: &str, end: usize) -> Option<(usize, &str)> {
    let mut start = end;
    while start > 0 && structural_ident_byte(text.as_bytes()[start - 1]) {
        start -= 1;
    }
    (start < end).then(|| (start, &text[start..end]))
}

/// How the call whose name starts at `name_at` was written to its receiver:
/// the whole receiver is read, never just the last word near it, so a call
/// on the result of another call (`foo(x).bar()`) is an expression receiver
/// and never takes `x` for a qualifier.
fn structural_call_receiver(excerpt: &str, name_at: usize) -> CallReceiver {
    let before = excerpt[..name_at].trim_end();
    let mut rest = if let Some(rest) = before.strip_suffix("::") {
        rest
    } else if let Some(rest) = before.strip_suffix('.') {
        // `a?.b()` and `a!.b()` still read `a`.
        rest.trim_end().trim_end_matches(['?', '!'])
    } else {
        return CallReceiver::Bare;
    };
    rest = rest.trim_end();
    let Some(last) = rest.bytes().last() else {
        return CallReceiver::Expression { literal: false };
    };
    if matches!(last, b'"' | b'\'' | b'`') {
        return CallReceiver::Expression { literal: true };
    }
    if !structural_ident_byte(last) {
        return CallReceiver::Expression { literal: false };
    }
    let mut depth = 0;
    let mut root: &str;
    let mut last = "";
    let mut end = rest.len();
    loop {
        let Some((start, segment)) = identifier_ending_at(rest, end) else {
            return CallReceiver::Expression { literal: false };
        };
        if depth == 0 {
            last = segment;
        }
        depth += 1;
        root = segment;
        let head = rest[..start].trim_end();
        if let Some(head) = head.strip_suffix("::").or_else(|| head.strip_suffix('.')) {
            let head = head.trim_end();
            if head.bytes().last().is_some_and(structural_ident_byte) {
                end = head.len();
                rest = head;
                continue;
            }
            // `f(x).field.name()`: the path hangs off an expression.
            return CallReceiver::Expression { literal: false };
        }
        break;
    }
    if root
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        return CallReceiver::Expression { literal: true };
    }
    CallReceiver::Path {
        root: root.to_owned(),
        last: last.to_owned(),
        depth,
    }
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
                            receiver: structural_call_receiver(excerpt, absolute_at),
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
    "", ".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs", ".py", ".rs",
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

/// A bare name that can be the name of a Python package: `inflect`, never
/// `a.b`, `a/b`, `a::b` or a path.
fn is_plain_identifier(specifier: &str) -> bool {
    specifier
        .bytes()
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && specifier
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

/// The `__init__.py` of the package a bare name imports: `import inflect`
/// and `from inflect import x` name `inflect/__init__.py`.
fn push_package_init_paths(
    specifier: &str,
    project_paths: &BTreeSet<String>,
    resolved: &mut BTreeSet<String>,
) {
    if !is_plain_identifier(specifier) {
        return;
    }
    let init = format!("{specifier}/__init__.py");
    for path in project_paths {
        if path == &init || path.ends_with(&format!("/{init}")) {
            resolved.insert(path.clone());
        }
    }
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

/// The directories a `@/` or `~/` import alias commonly names (a bundler or
/// `tsconfig` `paths` entry such as `"@/*": ["./src/*"]`), tried in order.
const PROJECT_ALIAS_ROOTS: &[&str] = &["src", ""];

/// What follows the `@/` or `~/` of a project import alias.
fn project_alias_rest(specifier: &str) -> Option<&str> {
    specifier
        .strip_prefix("@/")
        .or_else(|| specifier.strip_prefix("~/"))
        .filter(|rest| !rest.is_empty() && !rest.starts_with('/'))
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
    if let Some(rest) = project_alias_rest(specifier) {
        let mut resolved = BTreeSet::new();
        for root in PROJECT_ALIAS_ROOTS {
            let base = if root.is_empty() {
                rest.to_owned()
            } else {
                format!("{root}/{rest}")
            };
            push_resolved_candidate(&base, project_paths, &mut resolved);
        }
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
        if Path::new(path).file_stem().and_then(|stem| stem.to_str()) == Some(specifier) {
            resolved.insert(path.clone());
        }
    }
    push_dotted_module_paths(specifier, project_paths, &mut resolved);
    push_package_init_paths(specifier, project_paths, &mut resolved);
    push_go_import_paths(specifier, project_paths, &mut resolved);
    push_rust_module_paths(specifier, project_paths, &mut resolved);
    push_c_family_paths(specifier, project_paths, &mut resolved);
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

fn is_c_family_path_specifier(specifier: &str) -> bool {
    if specifier.is_empty()
        || specifier.starts_with('.')
        || !specifier.contains('/')
        || specifier.contains("::")
        || specifier.split('/').any(str::is_empty)
    {
        return false;
    }
    const SUFFIXES: [&str; 8] = [".c", ".h", ".cc", ".cpp", ".cxx", ".hh", ".hpp", ".hxx"];
    SUFFIXES.iter().any(|suffix| specifier.ends_with(suffix))
}

fn push_c_family_paths(
    specifier: &str,
    project_paths: &BTreeSet<String>,
    resolved: &mut BTreeSet<String>,
) {
    if !is_c_family_path_specifier(specifier) {
        return;
    }
    for path in project_paths {
        if path == specifier || path.ends_with(&format!("/{specifier}")) {
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

#[path = "structural_calls.rs"]
mod calls;
pub(crate) use calls::{StructuralCallEndpoint, StructuralCallResidence};

/// Same-file and import-resolved pairs from the exact selected source facts.
/// Graph and references share the immutable typed projection behind this view.
#[cfg(test)]
pub(crate) fn structural_call_coordinate_pairs(
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
) -> Result<Vec<(String, String)>, BuiltinModelError> {
    StructuralCallResidence::default().coordinate_pairs(sources, package)
}

struct ParsedDeclarationCoordinate {
    path: String,
    line: u32,
    name: String,
}

fn label_names_declaration(label: &str, name: &str) -> bool {
    label == name
        || label
            .strip_suffix(name)
            .is_some_and(|prefix| prefix.ends_with("::"))
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
    if let Some(id) = view.first_package_label(package, coordinate) {
        return Some(id);
    }
    let Some(parsed) = parsed_declaration_coordinate(coordinate) else {
        return None;
    };
    let mut semantic_matches = Vec::new();
    for row in view.row_refs() {
        if row.package != Some(package) {
            continue;
        }
        let Some(location) = row.source.captured() else {
            continue;
        };
        if location.path() != parsed.path || location.start_line() != parsed.line {
            continue;
        }
        if label_names_declaration(&row.label, &parsed.name) {
            semantic_matches.push(row.id);
        }
    }
    if semantic_matches.len() == 1 {
        semantic_matches.pop()
    } else {
        None
    }
}

/// Borrowed row lookup shared by mapped graph and reference projection.
/// Exact binding endpoints never resolve through a display-label map. Only a
/// uniquely matching source site can retarget an absent structural row onto a
/// semantic-shaped row; same-name/line ambiguity remains unresolved.
pub(crate) struct StructuralCallRows<'a> {
    view: &'a backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    first_label: BTreeMap<&'a str, RowId>,
    by_source: BTreeMap<(&'a str, u32), Vec<&'a backend_engine::Row>>,
}

impl<'a> StructuralCallRows<'a> {
    pub(crate) fn new(
        view: &'a backend_engine::ViewRoot,
        package: backend_engine::PackageKey,
    ) -> Self {
        let mut rows = Self {
            view,
            package,
            first_label: BTreeMap::new(),
            by_source: BTreeMap::new(),
        };
        for row in view.row_refs().filter(|row| row.package == Some(package)) {
            rows.first_label.entry(&row.label).or_insert(row.id);
            if let Some(location) = row.source.captured() {
                rows.by_source
                    .entry((location.path(), location.start_line()))
                    .or_default()
                    .push(row);
            }
        }
        rows
    }

    pub(crate) fn mapped(&self, endpoint: &StructuralCallEndpoint) -> Option<RowId> {
        match endpoint {
            StructuralCallEndpoint::Binding {
                source,
                row,
                location,
                semantic_site_unique,
                name,
            } => {
                if backend_engine::product_source_file_key(self.package.to_bytes(), location.path())
                    != source.file
                {
                    return None;
                }
                if let Some(row) = self
                    .view
                    .row_ref(*row)
                    .filter(|row| row.package == Some(self.package))
                {
                    return Some(row.id);
                }
                if !*semantic_site_unique {
                    return None;
                }
                self.unique_source(location.path(), location.start_line(), name)
            }
            StructuralCallEndpoint::LegacyCoordinate(coordinate) => {
                if let Some(&id) = self.first_label.get(coordinate.as_str()) {
                    return Some(id);
                }
                let parsed = parsed_declaration_coordinate(coordinate)?;
                self.unique_source(&parsed.path, parsed.line, &parsed.name)
            }
        }
    }

    fn unique_source(&self, path: &str, line: u32, name: &str) -> Option<RowId> {
        let candidates = self.by_source.get(&(path, line))?;
        let mut matches = candidates
            .iter()
            .filter(|row| label_names_declaration(&row.label, name));
        let id = matches.next()?.id;
        matches.next().is_none().then_some(id)
    }
}

fn resident_call_endpoint(
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    endpoint: &StructuralCallEndpoint,
) -> Option<RowId> {
    match endpoint {
        StructuralCallEndpoint::Binding { row, .. } => view
            .row_ref(*row)
            .filter(|row| row.package == Some(package))
            .map(|row| row.id),
        StructuralCallEndpoint::LegacyCoordinate(coordinate) => {
            view.last_package_label(package, coordinate)
        }
    }
}

/// Projects borrowed exact/legacy endpoints from one admitted source version.
pub(crate) fn structural_call_endpoint_relations_mapped(
    view: &backend_engine::ViewRoot,
    pairs: &[(&StructuralCallEndpoint, &StructuralCallEndpoint)],
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Vec<backend_engine::GraphRelation> {
    let rows = StructuralCallRows::new(view, package);
    let mut relations = BTreeSet::new();
    for &(caller, callee) in pairs {
        let (Some(caller), Some(callee)) = (rows.mapped(caller), rows.mapped(callee)) else {
            continue;
        };
        relations.insert(
            backend_engine::GraphRelation::new(
                caller,
                callee,
                backend_library::SemanticLinkKind::Calls,
            )
            .with_confidence(backend_library::SemanticConfidence::Syntactic),
        );
    }
    relations
        .into_iter()
        .filter(|relation| {
            relation.from == source_id || (include_incoming && relation.to == source_id)
        })
        .collect()
}

/// Explicit legacy adapter for synthetic coordinate-only controls. Production
/// graph and reference paths borrow the typed binding endpoints above.
#[cfg(test)]
pub(crate) fn structural_call_graph_relations_mapped(
    view: &backend_engine::ViewRoot,
    pairs: &[(String, String)],
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Vec<backend_engine::GraphRelation> {
    let endpoints = pairs
        .iter()
        .map(|(caller, callee)| {
            (
                StructuralCallEndpoint::LegacyCoordinate(caller.clone()),
                StructuralCallEndpoint::LegacyCoordinate(callee.clone()),
            )
        })
        .collect::<Vec<_>>();
    let borrowed = endpoints
        .iter()
        .map(|(caller, callee)| (caller, callee))
        .collect::<Vec<_>>();
    structural_call_endpoint_relations_mapped(view, &borrowed, package, source_id, include_incoming)
}

/// Same-file and import-resolved call edges inferred from bounded declaration
/// excerpts when no complete semantic publication supplies compiler-proven
/// `Calls` links.
#[cfg(test)]
pub(crate) fn structural_call_graph_relations(
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Result<Option<Vec<backend_engine::GraphRelation>>, BuiltinModelError> {
    structural_call_graph_relations_resident(
        &mut StructuralCallResidence::default(),
        view,
        sources,
        package,
        source_id,
        include_incoming,
    )
}

pub(crate) fn structural_call_graph_relations_resident(
    residence: &mut StructuralCallResidence,
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Result<Option<Vec<backend_engine::GraphRelation>>, BuiltinModelError> {
    if view.row_ref(source_id).is_none() {
        return Err(BuiltinModelError(
            "structural call graph source is absent from the view".to_owned(),
        ));
    }
    let calls = residence.project_calls(sources, package)?;
    let source = view.row_ref(source_id).expect("source row checked above");
    let mut relations = BTreeSet::new();
    // Only materialize the requested neighborhood. Incoming edges use the
    // reverse adjacency built with the same exact source projection.
    for (caller, callee) in calls.neighborhood_for_row(source_id, &source.label, include_incoming) {
        let caller_id = resident_call_endpoint(view, package, caller).ok_or_else(|| {
            BuiltinModelError(
                "structural call graph caller is absent or ambiguous in the published view"
                    .to_owned(),
            )
        })?;
        let callee_id = resident_call_endpoint(view, package, callee).ok_or_else(|| {
            BuiltinModelError(
                "structural call graph callee is absent or ambiguous in the published view"
                    .to_owned(),
            )
        })?;
        if caller_id != source_id && (!include_incoming || callee_id != source_id) {
            continue;
        }
        relations.insert(
            backend_engine::GraphRelation::new(
                caller_id,
                callee_id,
                backend_library::SemanticLinkKind::Calls,
            )
            .with_confidence(backend_library::SemanticConfidence::Syntactic),
        );
    }
    let relations = relations.into_iter().collect::<Vec<_>>();
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
#[cfg(test)]
pub(crate) fn structural_reference_facts(
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    target: &str,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    structural_reference_facts_resident(
        &mut StructuralCallResidence::default(),
        view,
        sources,
        target,
    )
}

pub(crate) fn structural_reference_facts_resident(
    residence: &mut StructuralCallResidence,
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    target: &str,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let (target_id, target_symbol, package) = {
        let target_row = view.row_by_label(target).ok_or_else(|| {
            BuiltinModelError("structural references target is absent from the view".to_owned())
        })?;
        let RowId::Symbol(target_symbol) = target_row.id else {
            return Err(BuiltinModelError(
                "structural references target is not a declaration row".to_owned(),
            ));
        };
        let Some(package) = target_row.package else {
            return Err(BuiltinModelError(
                "structural references target is not attributed to a package".to_owned(),
            ));
        };
        (target_row.id, target_symbol, package)
    };
    let Some(relations) = structural_call_graph_relations_resident(
        residence, view, sources, package, target_id, true,
    )?
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
    let root = sources
        .projects
        .get(&package.to_bytes())
        .map(|project| PathBuf::from(&project.label));
    let mut files = BTreeMap::new();
    let mut facts = Vec::new();
    for relation in relations {
        if relation.to != target_id || relation.relation != backend_library::SemanticLinkKind::Calls
        {
            continue;
        }
        let site_row = view.row_ref(relation.from).ok_or_else(|| {
            BuiltinModelError("structural references site is absent from the view".to_owned())
        })?;
        let RowId::Symbol(site_symbol) = site_row.id else {
            return Err(BuiltinModelError(
                "structural references site is not a declaration row".to_owned(),
            ));
        };
        // The place is where the call is in the file; a call the text
        // does not show (only in a comment, or the file changed since) has
        // no place rather than a made-up one.
        let source = match (
            site_row.source.captured(),
            site_row.excerpt.text(),
            root.as_deref(),
        ) {
            (Some(location), Some(excerpt), Some(root)) => {
                structural_call_span(excerpt, target_name)
                    .and_then(|span| {
                        structural_file_span(root, location, excerpt, span, &mut files)
                    })
                    .map(|(start, end)| {
                        Ok::<_, BuiltinModelError>(backend_engine::SemanticSourceSpan {
                            file: backend_engine::ProductText::new(location.path()).map_err(
                                |error| {
                                    BuiltinModelError(format!(
                                        "structural references path: {error:?}"
                                    ))
                                },
                            )?,
                            start,
                            end,
                        })
                    })
                    .transpose()?
            }
            _ => None,
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

/// Where `callee` is called in a declaration's captured text: the first
/// `callee(` in its body that is code, as byte offsets in `excerpt`. A
/// comment, a doc comment, a string or a character literal never counts, and
/// the name must stand on its own (`as_str(` is not found in `has_str(`).
pub(crate) fn structural_call_span(excerpt: &str, callee: &str) -> Option<(usize, usize)> {
    structural_name_spans(excerpt, callee, true)
        .into_iter()
        .next()
}

/// Every place `name` is written as code in a declaration's captured text, as
/// byte offsets in `excerpt`: with `calls_only`, those followed by `(`. The
/// same text rules as [`structural_call_span`] apply to each.
pub(crate) fn structural_name_spans(
    excerpt: &str,
    name: &str,
    calls_only: bool,
) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    if name.is_empty() {
        return spans;
    }
    let bytes = excerpt.as_bytes();
    let needle = name.as_bytes();
    let identifier = |byte: u8| byte == b'_' || byte.is_ascii_alphanumeric() || byte >= 0x80;
    let mut index = structural_body_start(excerpt).unwrap_or(0);
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
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
                    match bytes[index] {
                        b'\\' => index = index.saturating_add(2).min(bytes.len()),
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            // A character literal (`'a'`, `'\n'`); a lifetime (`'a`) has no
            // closing quote within two bytes and is read as code.
            b'\''
                if bytes.get(index + 2) == Some(&b'\'')
                    || (bytes.get(index + 1) == Some(&b'\\')
                        && bytes.get(index + 3) == Some(&b'\'')) =>
            {
                index += if bytes.get(index + 1) == Some(&b'\\') {
                    4
                } else {
                    3
                };
            }
            _ if bytes[index..].starts_with(needle)
                && (index == 0 || !identifier(bytes[index - 1]))
                && bytes
                    .get(index + needle.len())
                    .is_none_or(|next| !identifier(*next))
                && (!calls_only || bytes.get(index + needle.len()) == Some(&b'(')) =>
            {
                spans.push((index, index + needle.len()));
                index += needle.len();
            }
            _ => index += 1,
        }
    }
    spans
}

/// The longest text of one use's line a reference carries.
const MAX_PLACE_CHARS: usize = 160;
/// The largest source file read to place uses.
const MAX_PLACE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// One line of code as a reference shows it: trimmed, and cut when it is long.
pub(crate) fn bounded_line(line: &str) -> String {
    let mut text = String::new();
    for (count, character) in line.trim().chars().enumerate() {
        if count == MAX_PLACE_CHARS {
            text.push('…');
            break;
        }
        text.push(character);
    }
    text
}

/// One source file split into lines, for placing byte spans in it.
struct PlacedFile {
    text: String,
    line_starts: Vec<usize>,
}

impl PlacedFile {
    fn read(path: &Path) -> Option<Self> {
        if std::fs::metadata(path).ok()?.len() > MAX_PLACE_FILE_BYTES {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Some(Self { text, line_starts })
    }

    /// The one-based line holding byte `at` and that line's trimmed text.
    fn place(&self, at: usize) -> Option<(u32, String)> {
        if at > self.text.len() || !self.text.is_char_boundary(at) {
            return None;
        }
        let index = self.line_starts.partition_point(|start| *start <= at) - 1;
        let start = self.line_starts[index];
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |next| next - 1);
        let text = bounded_line(&self.text[start..end]);
        if text.is_empty() {
            return None;
        }
        Some((u32::try_from(index + 1).ok()?, text))
    }
}

/// An optional display placement read from current source bytes.
/// Captured reference facts retain their original byte-span evidence.
struct ReferencePlacement {
    line: u32,
    text: backend_engine::ProductText,
}

/// Projects one optional display placement per input fact, in input order.
/// Missing, unreadable or oversized source files and invalid offsets yield no
/// placement. The source text is bounded before it enters the display value.
fn place_reference_facts(
    root: Option<&Path>,
    facts: &[backend_engine::ReferenceFact],
) -> Vec<Option<ReferencePlacement>> {
    let Some(root) = root else {
        return facts.iter().map(|_| None).collect();
    };
    let mut files = BTreeMap::<String, Option<PlacedFile>>::new();
    facts
        .iter()
        .map(|fact| {
            let span = fact.evidence.source.as_ref()?;
            let file = files
                .entry(span.file.as_str().to_owned())
                .or_insert_with(|| PlacedFile::read(&root.join(span.file.as_str())))
                .as_ref()?;
            let start = usize::try_from(span.start).ok()?;
            let (line, text) = file.place(start)?;
            let text = backend_engine::ProductText::new(text).ok()?;
            Some(ReferencePlacement { line, text })
        })
        .collect()
}

/// The file bytes of `span`, a span in the captured text of a declaration
/// that starts on `location`'s line: [`backend_engine::SemanticSourceSpan`]
/// is in bytes of the file, not of the excerpt. `None` when the file under
/// `root` no longer holds the indexed text there: no place is better than a
/// place on the wrong bytes.
pub(crate) fn structural_file_span(
    root: &Path,
    location: &backend_compile::SourceLocation,
    excerpt: &str,
    span: (usize, usize),
    files: &mut BTreeMap<String, Option<String>>,
) -> Option<(u32, u32)> {
    let text = files
        .entry(location.path().to_owned())
        .or_insert_with(|| std::fs::read_to_string(root.join(location.path())).ok())
        .as_deref()?;
    let line = usize::try_from(location.start_line())
        .ok()?
        .checked_sub(1)?;
    let line_start = if line == 0 {
        0
    } else {
        text.match_indices('\n')
            .nth(line - 1)
            .map(|(at, _)| at + 1)?
    };
    let line_text = &text[line_start
        ..text[line_start..]
            .find('\n')
            .map_or(text.len(), |end| line_start + end)];
    let first_line = excerpt.split('\n').next()?;
    let declaration = line_start + line_text.find(first_line)?;
    let covered = excerpt.get(..span.1)?;
    if text.get(declaration..declaration + covered.len())? != covered {
        return None;
    }
    Some((
        u32::try_from(declaration + span.0).ok()?,
        u32::try_from(declaration + span.1).ok()?,
    ))
}

#[cfg(test)]
mod call_span_tests {
    use super::{
        MAX_PLACE_CHARS, place_reference_facts, structural_call_span, structural_file_span,
    };
    use std::collections::BTreeMap;

    #[test]
    fn a_call_in_a_comment_a_doc_comment_or_a_string_is_not_a_use() {
        let excerpt = "fn retain(&mut self) {\n    // as_str(key) was here\n    /* as_str( */\n    let s = \"as_str(\";\n    let c = '(';\n    has_str(x);\n    self.map.retain(|key, value| keep(key.as_str(), value));\n}";
        let (start, end) = structural_call_span(excerpt, "as_str").expect("the call in code");
        assert_eq!(&excerpt[start..end], "as_str");
        assert!(
            excerpt[..start].ends_with("key."),
            "the call, not a comment, a string or has_str: {}",
            &excerpt[start.saturating_sub(10)..end]
        );
        assert_eq!(
            structural_call_span("fn f() {\n    /// as_str(x)\n    // as_str(y)\n}", "as_str"),
            None,
            "only comments: no use"
        );
    }

    #[test]
    fn a_span_is_in_bytes_of_the_file_and_none_when_the_file_changed() {
        let root = std::env::temp_dir().join(format!("f-data-span-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).expect("root");
        let file = "// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n\nimpl Map {\n    pub fn retain(&mut self) {\n        self.map.retain(|key, value| keep(key.as_str(), value));\n    }\n}\n";
        std::fs::write(root.join("src/map.rs"), file).expect("file");
        let excerpt = "pub fn retain(&mut self) {\n        self.map.retain(|key, value| keep(key.as_str(), value));\n    }";
        let location = backend_compile::SourceLocation::new("src/map.rs", 5).expect("location");
        let span = structural_call_span(excerpt, "as_str").expect("the call");
        let (start, end) =
            structural_file_span(&root, &location, excerpt, span, &mut BTreeMap::new())
                .expect("placed");
        let (start, end) = (start as usize, end as usize);
        assert_eq!(&file[start..end], "as_str", "the file's own bytes");
        assert_eq!(
            file[..start].matches('\n').count() + 1,
            6,
            "on the call's line, not in the license header"
        );
        std::fs::write(
            root.join("src/map.rs"),
            file.replace("key.as_str()", "key.to_str()"),
        )
        .expect("edit");
        assert_eq!(
            structural_file_span(&root, &location, excerpt, span, &mut BTreeMap::new()),
            None,
            "a file that no longer holds the indexed text places nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn fact_at(path: &str, start: u32, end: u32) -> backend_engine::ReferenceFact {
        backend_engine::ReferenceFact {
            site: backend_engine::symbol_key("pkg::caller"),
            target: backend_engine::SemanticLinkTarget::Local {
                declaration: backend_engine::SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_engine::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Compiler,
                source: Some(backend_engine::SemanticSourceSpan {
                    file: backend_engine::ProductText::new(path).expect("path"),
                    start,
                    end,
                }),
            },
        }
    }

    #[test]
    fn a_use_carries_the_line_it_is_written_on_and_that_lines_code() {
        let root = std::env::temp_dir().join(format!("f-place-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let file = "import os\n\ndef run():\n    # not here: enclose(x)\n    value = enclose(\n        \"a\",\n    )\n    return value\n";
        std::fs::write(root.join("a.py"), file).expect("file");
        std::fs::write(root.join("unicode.py"), "π = enclose(x)\n").expect("UTF-8 file");
        let at = file.rfind("enclose").expect("call");
        let facts = vec![
            fact_at("a.py", at as u32, (at + 7) as u32),
            fact_at("missing.py", 0, 3),
            fact_at("a.py", 9_999, 10_000),
            fact_at("unicode.py", 1, 2),
        ];
        let original = facts.clone();
        let placements = place_reference_facts(Some(&root), &facts);
        assert_eq!(facts, original);
        assert_eq!(placements.len(), facts.len());
        let place = placements[0].as_ref().expect("placed");
        assert_eq!(place.line, 5);
        assert_eq!(place.text.as_str(), "value = enclose(");
        assert!(placements[1].is_none(), "a missing file places nothing");
        assert!(
            placements[2].is_none(),
            "a span outside the file places nothing"
        );
        assert!(
            placements[3].is_none(),
            "an offset inside a UTF-8 character places nothing"
        );
        let without_root = place_reference_facts(None, &facts);
        assert_eq!(without_root.len(), facts.len());
        assert!(without_root.iter().all(Option::is_none));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_text_of_a_long_line_is_bounded() {
        let root = std::env::temp_dir().join(format!("f-place-long-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let line = format!("x = call({})", "a".repeat(400));
        std::fs::write(root.join("b.py"), format!("{line}\n")).expect("file");
        let facts = vec![fact_at("b.py", 4, 8)];
        let placements = place_reference_facts(Some(&root), &facts);
        let text = placements[0].as_ref().expect("placed").text.as_str();
        assert_eq!(text.chars().count(), MAX_PLACE_CHARS + 1);
        assert!(text.ends_with('…'));
        let _ = std::fs::remove_dir_all(&root);
    }
}
