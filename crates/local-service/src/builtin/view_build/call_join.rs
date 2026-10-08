use super::super::{BuiltinModelError, IndexedSources};
use super::compiled_source_path;
use super::structural::resolve_specifier_paths;
use backend_engine::PackageKey;
use backend_engine::application::DocumentationSession;
use backend_semantic::ir::{
    DeclarationIdentity, ExternalId, ExternalTarget, ForeignTargetOrigin, ImageProvenance,
    ItemKind, PYTHON_NATIVE_SOURCE_ECOSYSTEM, PythonSourceCoordinate, SemanticCoreReader,
    SemanticImageView, SemanticReader, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM, TypeScriptSourceCoordinate,
    python_program_identity, typescript_program_identity,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn semantic_callable(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Function)
}

pub(crate) fn semantic_mentionable(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Record
            | ItemKind::Enum
            | ItemKind::Trait
            | ItemKind::Alias
            | ItemKind::Module
            | ItemKind::Field
    )
}

pub(crate) fn semantic_qualified_mentionable(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Record | ItemKind::Enum | ItemKind::Trait | ItemKind::Alias | ItemKind::Module
    )
}

pub(crate) struct ProjectCallableIndex {
    by_path_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
    by_owner_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
    mention_by_path_name_kind: BTreeMap<(String, String, ItemKind), Vec<DeclarationIdentity>>,
    field_by_owner_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
    value_by_owner_name_kind: BTreeMap<(String, String, ItemKind), Vec<DeclarationIdentity>>,
    tsz_source_coordinates:
        BTreeMap<(String, [u8; 32], u32, u32, ItemKind), Vec<DeclarationIdentity>>,
    tsz_program_identity: Option<[u8; 32]>,
    python_program_identity: Option<[u8; 32]>,
}

impl ProjectCallableIndex {
    pub(crate) fn build_from_bytes(images: &[&[u8]]) -> Result<Self, BuiltinModelError> {
        let mut opened = Vec::with_capacity(images.len());
        for bytes in images {
            opened.push(SemanticImageView::reopen(bytes).map_err(|error| {
                BuiltinModelError(format!("reopen semantic graph image: {error}"))
            })?);
        }
        let views = opened.iter().collect::<Vec<_>>();
        Self::build_from_views(&views)
    }

    /// Indexes declarations on views that are already open.
    pub(crate) fn build_from_views(
        images: &[&SemanticImageView<'_>],
    ) -> Result<Self, BuiltinModelError> {
        let mut by_path_name = BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        let mut by_owner_name = BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        let mut mention_by_path_name_kind =
            BTreeMap::<(String, String, ItemKind), Vec<DeclarationIdentity>>::new();
        let mut field_by_owner_name = BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        let mut value_by_owner_name_kind =
            BTreeMap::<(String, String, ItemKind), Vec<DeclarationIdentity>>::new();
        let mut tsz_source_coordinates =
            BTreeMap::<(String, [u8; 32], u32, u32, ItemKind), Vec<DeclarationIdentity>>::new();
        let mut source_manifest = Vec::with_capacity(images.len());
        let mut python_source_manifest = Vec::new();
        let mut manifest_complete = true;
        let mut python_manifest_complete = true;
        for image in images {
            let image = *image;
            let path = compiled_source_path(image)?;
            let source = match image.image_facts().provenance {
                ImageProvenance::Captured { source, recipe, .. } => {
                    if matches!(
                        recipe.profile,
                        backend_semantic::vocabulary::LanguageProfile::Python(_)
                    ) {
                        python_source_manifest.push((path.clone(), *source.identity));
                    }
                    *source.identity
                }
                ImageProvenance::Unavailable => {
                    manifest_complete = false;
                    python_manifest_complete = false;
                    continue;
                }
            };
            source_manifest.push((path, source));
        }
        let python_program_identity = python_manifest_complete
            .then(|| python_program_identity(&python_source_manifest))
            .flatten();
        let tsz_program_identity = manifest_complete
            .then(|| typescript_program_identity(&source_manifest))
            .flatten();
        for image in images {
            let image = *image;
            let path = compiled_source_path(image)?;
            let source_identity = match image.image_facts().provenance {
                ImageProvenance::Captured { source, .. } => Some(*source.identity),
                ImageProvenance::Unavailable => None,
            };
            let session = DocumentationSession::new(image);
            for entity in session.canonical_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read semantic graph callable: {error}"))
                })?;
                let identity = entity.entity.version.identity();
                if let (Some(source_identity), Some(source_span)) =
                    (source_identity, entity.entity.source)
                {
                    let source_path = image.atom(source_span.file()).ok_or_else(|| {
                        BuiltinModelError("semantic graph source path atom is missing".to_owned())
                    })?;
                    if source_path == path.as_bytes() {
                        tsz_source_coordinates
                            .entry((
                                path.clone(),
                                source_identity,
                                source_span.start(),
                                source_span.end(),
                                entity.entity.kind,
                            ))
                            .or_default()
                            .push(identity);
                    }
                }
                // Anonymous callables participate in exact source-span joins,
                // but their structural anchors are never identifier spellings.
                let Some(name) = entity.name.named_bytes() else {
                    continue;
                };
                let name = std::str::from_utf8(name).map_err(|_| {
                    BuiltinModelError("semantic graph callable name is not UTF-8".to_owned())
                })?;
                if semantic_callable(entity.entity.kind) {
                    by_path_name
                        .entry((path.clone(), name.to_owned()))
                        .or_default()
                        .push(identity);
                    if let Some((immediate, chain)) = owner_chain_keys(&session, entity.entity.id)?
                    {
                        by_owner_name
                            .entry((immediate.clone(), name.to_owned()))
                            .or_default()
                            .push(identity);
                        if chain != immediate {
                            by_owner_name
                                .entry((chain, name.to_owned()))
                                .or_default()
                                .push(identity);
                        }
                    }
                }
                if semantic_mentionable(entity.entity.kind) {
                    mention_by_path_name_kind
                        .entry((path.clone(), name.to_owned(), entity.entity.kind))
                        .or_default()
                        .push(identity);
                }
                if matches!(
                    entity.entity.kind,
                    ItemKind::Constant | ItemKind::Static | ItemKind::Variant
                ) {
                    mention_by_path_name_kind
                        .entry((path.clone(), name.to_owned(), entity.entity.kind))
                        .or_default()
                        .push(identity);
                }
                if entity.entity.kind == ItemKind::Field {
                    if let Some((immediate, chain)) = owner_chain_keys(&session, entity.entity.id)?
                    {
                        field_by_owner_name
                            .entry((immediate.clone(), name.to_owned()))
                            .or_default()
                            .push(identity);
                        if chain != immediate {
                            field_by_owner_name
                                .entry((chain, name.to_owned()))
                                .or_default()
                                .push(identity);
                        }
                    }
                }
                if matches!(
                    entity.entity.kind,
                    ItemKind::Constant | ItemKind::Static | ItemKind::Variant
                ) {
                    if let Some((immediate, chain)) = owner_chain_keys(&session, entity.entity.id)?
                    {
                        value_by_owner_name_kind
                            .entry((immediate.clone(), name.to_owned(), entity.entity.kind))
                            .or_default()
                            .push(identity);
                        if chain != immediate {
                            value_by_owner_name_kind
                                .entry((chain, name.to_owned(), entity.entity.kind))
                                .or_default()
                                .push(identity);
                        }
                    }
                }
            }
        }
        Ok(Self {
            by_path_name,
            by_owner_name,
            mention_by_path_name_kind,
            field_by_owner_name,
            value_by_owner_name_kind,
            tsz_source_coordinates,
            tsz_program_identity,
            python_program_identity,
        })
    }

    pub(crate) fn resolve(
        &self,
        resolved_paths: &BTreeSet<String>,
        display: &str,
    ) -> Option<DeclarationIdentity> {
        let mut matches = Vec::new();
        for path in resolved_paths {
            if let Some(identities) = self.by_path_name.get(&(path.clone(), display.to_owned())) {
                matches.extend(identities);
            }
        }
        matches.sort();
        matches.dedup();
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }

    pub(crate) fn resolve_mention(
        &self,
        resolved_paths: &BTreeSet<String>,
        display: &str,
        kind: ItemKind,
    ) -> Option<DeclarationIdentity> {
        let mut matches = Vec::new();
        for path in resolved_paths {
            if let Some(identities) =
                self.mention_by_path_name_kind
                    .get(&(path.clone(), display.to_owned(), kind))
            {
                matches.extend(identities);
            }
        }
        matches.sort();
        matches.dedup();
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }

    fn resolve_tsz_source_coordinate(
        &self,
        coordinate: TypeScriptSourceCoordinate<'_>,
        kind: ItemKind,
    ) -> Option<DeclarationIdentity> {
        self.resolve_source_coordinate(coordinate, kind, self.tsz_program_identity)
    }

    fn resolve_source_coordinate(
        &self,
        coordinate: TypeScriptSourceCoordinate<'_>,
        kind: ItemKind,
        program: Option<[u8; 32]>,
    ) -> Option<DeclarationIdentity> {
        if program != Some(coordinate.program) {
            return None;
        }
        let matches = self.tsz_source_coordinates.get(&(
            coordinate.path.to_owned(),
            coordinate.source,
            coordinate.declaration_start,
            coordinate.declaration_end,
            kind,
        ))?;
        let mut unique = matches.clone();
        unique.sort();
        unique.dedup();
        (unique.len() == 1).then(|| unique[0])
    }

    pub(crate) fn resolve_import_mention(
        &self,
        resolved_paths: &BTreeSet<String>,
        display: &str,
    ) -> Option<DeclarationIdentity> {
        if display.is_empty() {
            return None;
        }
        let mut matches = Vec::new();
        for path in resolved_paths {
            for kind in [
                ItemKind::Record,
                ItemKind::Enum,
                ItemKind::Trait,
                ItemKind::Alias,
                ItemKind::Module,
            ] {
                if let Some(identities) =
                    self.mention_by_path_name_kind
                        .get(&(path.clone(), display.to_owned(), kind))
                {
                    matches.extend(identities);
                }
            }
            if let Some(identities) = self.by_path_name.get(&(path.clone(), display.to_owned())) {
                matches.extend(identities);
            }
        }
        matches.sort();
        matches.dedup();
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }

    pub(crate) fn resolve_qualified_mention(
        &self,
        display: &str,
        kind: ItemKind,
    ) -> Option<DeclarationIdentity> {
        if display.is_empty() || !semantic_qualified_mentionable(kind) {
            return None;
        }
        let mut matches = Vec::new();
        for ((_, name, item_kind), identities) in &self.mention_by_path_name_kind {
            if name == display && *item_kind == kind {
                matches.extend(identities);
            }
        }
        matches.sort();
        matches.dedup();
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }

    pub(crate) fn resolve_owner(
        &self,
        namespace: &str,
        display: &str,
    ) -> Option<DeclarationIdentity> {
        if namespace.is_empty() || display.is_empty() {
            return None;
        }
        match self.owner_matches(namespace, display) {
            Some(matches) if matches.len() == 1 => matches.into_iter().next(),
            Some(_) => None,
            None => {
                let stripped = strip_type_arguments(namespace)?;
                if stripped == namespace {
                    None
                } else {
                    match self.owner_matches(&stripped, display) {
                        Some(matches) if matches.len() == 1 => matches.into_iter().next(),
                        _ => None,
                    }
                }
            }
        }
    }

    pub(crate) fn resolve_field_owner(
        &self,
        namespace: &str,
        display: &str,
    ) -> Option<DeclarationIdentity> {
        if namespace.is_empty() || display.is_empty() {
            return None;
        }
        match self.field_owner_matches(namespace, display) {
            Some(matches) if matches.len() == 1 => matches.into_iter().next(),
            Some(_) => None,
            None => {
                let stripped = strip_type_arguments(namespace)?;
                if stripped == namespace {
                    None
                } else {
                    match self.field_owner_matches(&stripped, display) {
                        Some(matches) if matches.len() == 1 => matches.into_iter().next(),
                        _ => None,
                    }
                }
            }
        }
    }

    pub(crate) fn resolve_value_owner(
        &self,
        namespace: &str,
        display: &str,
        kind: ItemKind,
    ) -> Option<DeclarationIdentity> {
        if namespace.is_empty() || display.is_empty() {
            return None;
        }
        match self.value_owner_matches(namespace, display, kind) {
            Some(matches) if matches.len() == 1 => matches.into_iter().next(),
            Some(_) => None,
            None => {
                let stripped = strip_type_arguments(namespace)?;
                if stripped == namespace {
                    None
                } else {
                    match self.value_owner_matches(&stripped, display, kind) {
                        Some(matches) if matches.len() == 1 => matches.into_iter().next(),
                        _ => None,
                    }
                }
            }
        }
    }

    fn owner_matches(&self, namespace: &str, display: &str) -> Option<Vec<DeclarationIdentity>> {
        let mut matches = self
            .by_owner_name
            .get(&(namespace.to_owned(), display.to_owned()))?
            .clone();
        matches.sort();
        matches.dedup();
        if matches.is_empty() {
            None
        } else {
            Some(matches)
        }
    }

    fn field_owner_matches(
        &self,
        namespace: &str,
        display: &str,
    ) -> Option<Vec<DeclarationIdentity>> {
        let mut matches = self
            .field_by_owner_name
            .get(&(namespace.to_owned(), display.to_owned()))?
            .clone();
        matches.sort();
        matches.dedup();
        if matches.is_empty() {
            None
        } else {
            Some(matches)
        }
    }

    fn value_owner_matches(
        &self,
        namespace: &str,
        display: &str,
        kind: ItemKind,
    ) -> Option<Vec<DeclarationIdentity>> {
        let mut matches = self
            .value_by_owner_name_kind
            .get(&(namespace.to_owned(), display.to_owned(), kind))?
            .clone();
        matches.sort();
        matches.dedup();
        if matches.is_empty() {
            None
        } else {
            Some(matches)
        }
    }
}

pub(crate) fn owner_chain_keys(
    session: &DocumentationSession<'_, SemanticImageView<'_>>,
    function: backend_semantic::ir::EntityId,
) -> Result<Option<(String, String)>, BuiltinModelError> {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = session
        .entity(function)
        .map_err(|error| {
            BuiltinModelError(format!("read semantic graph callable parent: {error}"))
        })?
        .entity
        .parent;
    let mut steps = 0usize;
    while let Some(parent_id) = current {
        if steps >= 16 {
            return Ok(None);
        }
        if !seen.insert(parent_id) {
            return Ok(None);
        }
        let parent = session.entity(parent_id).map_err(|error| {
            BuiltinModelError(format!("read semantic graph callable ancestor: {error}"))
        })?;
        let Some(name_atom) = parent.name.named_bytes() else {
            return Ok(None);
        };
        let name = std::str::from_utf8(name_atom).map_err(|_| {
            BuiltinModelError("semantic graph ancestor name is not UTF-8".to_owned())
        })?;
        if name.is_empty() {
            return Ok(None);
        }
        names.push(name.to_owned());
        current = parent.entity.parent;
        steps += 1;
    }
    if names.is_empty() {
        return Ok(None);
    }
    names.reverse();
    let immediate = names.last().cloned().ok_or_else(|| {
        BuiltinModelError("semantic graph owner chain is unexpectedly empty".to_owned())
    })?;
    let chain = names.join(".");
    Ok(Some((immediate, chain)))
}

pub(crate) fn strip_type_arguments(namespace: &str) -> Option<String> {
    let mut out = String::new();
    let bytes = namespace.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            let mut depth = 1usize;
            index += 1;
            while index < bytes.len() && depth > 0 {
                match bytes[index] {
                    b'<' => depth += 1,
                    b'>' => depth -= 1,
                    _ => {}
                }
                index += 1;
            }
            if depth != 0 {
                return None;
            }
        } else if bytes[index] == b'>' {
            return None;
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    Some(out)
}

pub(crate) fn foreign_dotted_module_specifier<'a>(
    path: &'a str,
    display: &'a str,
) -> Option<&'a str> {
    if path == display {
        return None;
    }
    if path.is_empty()
        || path.starts_with('.')
        || path.contains('/')
        || path.contains('\\')
        || path.contains("::")
        || !path.contains('.')
    {
        return None;
    }
    if path.split('.').any(|segment| segment.is_empty()) {
        return None;
    }
    Some(path)
}

/// Joins a native target only when its producer-specific coordinate names one unique
/// declaration span inside the same complete source manifest. The universe
/// discriminator is deliberately separate from npm/package and namespace
/// resolution; this path never falls back to a display-name match.
fn foreign_native_source_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    expected_kind: ItemKind,
    callable_index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Universe { ecosystem } = foreign.origin else {
        return Ok(None);
    };
    if foreign.kind != Some(expected_kind) {
        return Ok(None);
    }
    let ecosystem = image
        .atom(ecosystem)
        .ok_or_else(|| BuiltinModelError("semantic graph ecosystem atom is missing".to_owned()))?;
    if ecosystem != TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes()
        && ecosystem != PYTHON_NATIVE_SOURCE_ECOSYSTEM.as_bytes()
    {
        return Ok(None);
    }
    let path = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let path = std::str::from_utf8(path)
        .map_err(|_| BuiltinModelError("semantic graph path is not UTF-8".to_owned()))?;
    if ecosystem == TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes() {
        let Some(coordinate) = TypeScriptSourceCoordinate::decode(path) else {
            return Ok(None);
        };
        Ok(callable_index.resolve_tsz_source_coordinate(coordinate, expected_kind))
    } else if ecosystem == PYTHON_NATIVE_SOURCE_ECOSYSTEM.as_bytes() {
        let Some(coordinate) = PythonSourceCoordinate::decode(path) else {
            return Ok(None);
        };
        Ok(callable_index.resolve_source_coordinate(
            coordinate.0,
            expected_kind,
            callable_index.python_program_identity,
        ))
    } else {
        Ok(None)
    }
}

pub(crate) fn foreign_package_call_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    callable_index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Function, callable_index)?
    {
        return Ok(Some(identity));
    }
    if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Record, callable_index)?
    {
        return Ok(Some(identity));
    }
    if matches!(image.image_facts().provenance,
        ImageProvenance::Captured { recipe, .. }
            if matches!(recipe.profile, backend_semantic::vocabulary::LanguageProfile::Python(_)))
    {
        // Unresolved/external Python imports cannot borrow same-named local declarations.
        return Ok(None);
    }
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    if foreign.kind == Some(ItemKind::Record)
        && matches!(
            image.image_facts().provenance,
            backend_semantic::ir::ImageProvenance::Captured { recipe, .. }
                if matches!(recipe.profile, backend_semantic::vocabulary::LanguageProfile::Python(_))
        )
    {
        // Python's primary class binding carries its qualified declaration
        // name. The constructor remains a distinct function declaration.
        let Some(module) = python_record_module_specifier(path, display) else {
            return Ok(None);
        };
        let resolved_paths = resolve_specifier_paths(module, caller_path, project_paths);
        return Ok(callable_index.resolve_mention(&resolved_paths, display, ItemKind::Record));
    }
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(callable_index.resolve(&resolved_paths, display))
}

fn python_record_module_specifier<'a>(path: &'a str, display: &'a str) -> Option<&'a str> {
    let path = foreign_dotted_module_specifier(path, display)?;
    let (prefix, terminal) = path.rsplit_once('.')?;
    if terminal == display {
        Some(prefix)
    } else {
        // Older Python package bindings carry the module path directly.
        Some(path)
    }
}

pub(crate) fn foreign_namespace_call_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    callable_index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Namespace { namespace, .. } = foreign.origin else {
        return Ok(None);
    };
    let namespace_atom = image
        .atom(namespace)
        .ok_or_else(|| BuiltinModelError("semantic graph namespace atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let namespace = std::str::from_utf8(namespace_atom)
        .map_err(|_| BuiltinModelError("semantic graph namespace is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    Ok(callable_index.resolve_owner(namespace, display))
}

pub(crate) fn foreign_namespace_field_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Namespace { namespace, .. } = foreign.origin else {
        return Ok(None);
    };
    if foreign.kind != Some(ItemKind::Field) {
        return Ok(None);
    }
    let namespace_atom = image
        .atom(namespace)
        .ok_or_else(|| BuiltinModelError("semantic graph namespace atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let namespace = std::str::from_utf8(namespace_atom)
        .map_err(|_| BuiltinModelError("semantic graph namespace is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    Ok(index.resolve_field_owner(namespace, display))
}

pub(crate) fn project_paths_for_package(
    sources: &IndexedSources,
    package: PackageKey,
) -> BTreeSet<String> {
    let Some(project_key) = sources
        .projects
        .values()
        .find(|project| project.package == package)
        .map(|project| project.package.to_bytes())
    else {
        return BTreeSet::new();
    };
    let mut paths = BTreeSet::new();
    for (_, record) in &sources.files {
        let Some(file) = record.file_fields() else {
            continue;
        };
        if file.project == project_key {
            paths.insert(file.path.to_owned());
        }
    }
    paths
}

pub(crate) fn foreign_package_mention_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    let Some(kind) = foreign.kind else {
        return Ok(None);
    };
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(index.resolve_mention(&resolved_paths, display, kind))
}

pub(crate) fn foreign_package_import_mention_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    if !matches!(foreign.kind, None | Some(ItemKind::Reexport)) {
        return Ok(None);
    }
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(index.resolve_import_mention(&resolved_paths, display))
}

pub(crate) fn foreign_qualified_type_mention_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    match foreign.origin {
        ForeignTargetOrigin::Universe { .. } | ForeignTargetOrigin::Namespace { .. } => {}
        ForeignTargetOrigin::Package { .. } | ForeignTargetOrigin::Unspecified { .. } => {
            return Ok(None);
        }
    };
    let Some(kind) = foreign.kind else {
        return Ok(None);
    };
    if !semantic_qualified_mentionable(kind) {
        return Ok(None);
    }
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    if display.is_empty() {
        return Ok(None);
    }
    Ok(index.resolve_qualified_mention(display, kind))
}

pub(crate) fn join_project_mention(
    image: &SemanticImageView<'_>,
    link_kind: backend_semantic::ir::LinkKind,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
    published: &BTreeSet<DeclarationIdentity>,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    if !matches!(
        link_kind,
        backend_semantic::ir::LinkKind::TypeReference | backend_semantic::ir::LinkKind::Imports
    ) {
        return Ok(None);
    }
    let identity = if let Some(identity) =
        foreign_package_mention_retarget(image, external, caller_path, project_paths, index)?
    {
        Some(identity)
    } else if let Some(identity) =
        foreign_package_import_mention_retarget(image, external, caller_path, project_paths, index)?
    {
        Some(identity)
    } else if matches!(link_kind, backend_semantic::ir::LinkKind::TypeReference) {
        foreign_qualified_type_mention_retarget(image, external, index)?
    } else {
        None
    };
    Ok(identity.filter(|candidate| published.contains(candidate)))
}

pub(crate) fn foreign_package_field_static_constant_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    if foreign.kind != Some(ItemKind::Field) {
        return Ok(None);
    }
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    let static_match = index.resolve_mention(&resolved_paths, display, ItemKind::Static);
    let constant_match = index.resolve_mention(&resolved_paths, display, ItemKind::Constant);
    match (static_match, constant_match) {
        (Some(identity), None) | (None, Some(identity)) => Ok(Some(identity)),
        _ => Ok(None),
    }
}

pub(crate) fn foreign_package_field_function_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    if foreign.kind != Some(ItemKind::Field) {
        return Ok(None);
    }
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    let static_match = index.resolve_mention(&resolved_paths, display, ItemKind::Static);
    let constant_match = index.resolve_mention(&resolved_paths, display, ItemKind::Constant);
    match (static_match, constant_match) {
        (Some(_), _) | (_, Some(_)) => Ok(None),
        _ => Ok(index.resolve(&resolved_paths, display)),
    }
}

pub(crate) fn foreign_package_field_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    if foreign.kind != Some(ItemKind::Field) {
        return Ok(None);
    }
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(index.resolve_mention(&resolved_paths, display, ItemKind::Field))
}

pub(crate) fn join_project_field(
    image: &SemanticImageView<'_>,
    link_kind: backend_semantic::ir::LinkKind,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
    published: &BTreeSet<DeclarationIdentity>,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    if !matches!(link_kind, backend_semantic::ir::LinkKind::Reads) {
        return Ok(None);
    }
    if !native_python_coordinate_selected(image, external, project_paths)? {
        return Ok(None);
    }
    let identity = if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Field, index)?
    {
        Some(identity)
    } else if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Function, index)?
    {
        Some(identity)
    } else if let Some(identity) =
        foreign_package_field_retarget(image, external, caller_path, project_paths, index)?
    {
        Some(identity)
    } else if let Some(identity) = foreign_package_field_static_constant_retarget(
        image,
        external,
        caller_path,
        project_paths,
        index,
    )? {
        Some(identity)
    } else if let Some(identity) =
        foreign_package_field_function_retarget(image, external, caller_path, project_paths, index)?
    {
        Some(identity)
    } else {
        foreign_namespace_field_retarget(image, external, index)?
    };
    Ok(identity.filter(|candidate| published.contains(candidate)))
}

pub(crate) fn foreign_namespace_value_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Namespace { namespace, .. } = foreign.origin else {
        return Ok(None);
    };
    let Some(kind) = foreign.kind else {
        return Ok(None);
    };
    if !matches!(
        kind,
        ItemKind::Constant | ItemKind::Static | ItemKind::Variant
    ) {
        return Ok(None);
    }
    let namespace_atom = image
        .atom(namespace)
        .ok_or_else(|| BuiltinModelError("semantic graph namespace atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let namespace = std::str::from_utf8(namespace_atom)
        .map_err(|_| BuiltinModelError("semantic graph namespace is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    Ok(index.resolve_value_owner(namespace, display, kind))
}

pub(crate) fn foreign_package_value_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    let Some(kind) = foreign.kind else {
        return Ok(None);
    };
    if !matches!(
        kind,
        ItemKind::Constant | ItemKind::Static | ItemKind::Variant
    ) {
        return Ok(None);
    }
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(index.resolve_mention(&resolved_paths, display, kind))
}

pub(crate) fn foreign_package_function_value_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    if foreign.kind != Some(ItemKind::Function) {
        return Ok(None);
    }
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let path_atom = image
        .atom(foreign.path)
        .ok_or_else(|| BuiltinModelError("semantic graph path atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let package = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let path = std::str::from_utf8(path_atom)
        .map_err(|_| BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(index.resolve(&resolved_paths, display))
}

pub(crate) fn join_project_value(
    image: &SemanticImageView<'_>,
    link_kind: backend_semantic::ir::LinkKind,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
    published: &BTreeSet<DeclarationIdentity>,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    if !matches!(link_kind, backend_semantic::ir::LinkKind::Reads) {
        return Ok(None);
    }
    if !native_python_coordinate_selected(image, external, project_paths)? {
        return Ok(None);
    }
    let identity = if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Static, index)?
    {
        Some(identity)
    } else if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Function, index)?
    {
        Some(identity)
    } else if let Some(identity) =
        foreign_package_value_retarget(image, external, caller_path, project_paths, index)?
    {
        Some(identity)
    } else if let Some(identity) = foreign_namespace_value_retarget(image, external, index)? {
        Some(identity)
    } else {
        foreign_package_function_value_retarget(image, external, caller_path, project_paths, index)?
    };
    Ok(identity.filter(|candidate| published.contains(candidate)))
}

fn native_python_coordinate_selected(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    project_paths: &BTreeSet<String>,
) -> Result<bool, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(true);
    };
    let ForeignTargetOrigin::Universe { ecosystem } = foreign.origin else {
        return Ok(true);
    };
    if image.atom(ecosystem) != Some(PYTHON_NATIVE_SOURCE_ECOSYSTEM.as_bytes()) {
        return Ok(true);
    }
    let encoded = image
        .atom(foreign.path)
        .and_then(|path| std::str::from_utf8(path).ok());
    Ok(encoded
        .and_then(PythonSourceCoordinate::decode)
        .is_some_and(|coordinate| project_paths.contains(coordinate.0.path)))
}

pub(crate) fn join_project_call(
    image: &SemanticImageView<'_>,
    link_kind: backend_semantic::ir::LinkKind,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
    published: &BTreeSet<DeclarationIdentity>,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    if !matches!(
        link_kind,
        backend_semantic::ir::LinkKind::Calls | backend_semantic::ir::LinkKind::MethodCall
    ) {
        return Ok(None);
    }
    if !native_python_coordinate_selected(image, external, project_paths)? {
        return Ok(None);
    }
    // Native TSZ/Python calls carry a program- and declaration-bound source
    // coordinate, so admit that proof before the package/namespace retargets.
    // A malformed or stale native coordinate deliberately does not fall back to
    // display-name matching: neither of the other retargeters accepts its
    // `Universe` origin.
    let identity = if let Some(identity) =
        foreign_native_source_retarget(image, external, ItemKind::Function, index)?
    {
        Some(identity)
    } else if let Some(identity) =
        foreign_package_call_retarget(image, external, caller_path, project_paths, index)?
    {
        Some(identity)
    } else {
        foreign_namespace_call_retarget(image, external, index)?
    };
    Ok(identity.filter(|candidate| published.contains(candidate)))
}

pub(crate) fn foreign_display_name(
    image: &SemanticImageView<'_>,
    external: ExternalId,
) -> Result<Option<String>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let display = std::str::from_utf8(display_atom)
        .map_err(|_| BuiltinModelError("semantic graph display name is not UTF-8".to_owned()))?;
    if display.is_empty() {
        return Ok(None);
    }
    Ok(Some(display.to_owned()))
}

#[cfg(test)]
mod tsz_source_coordinate_tests {
    use super::{
        ProjectCallableIndex, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM, join_project_call,
        join_project_field, typescript_program_identity,
    };
    use backend_semantic::ir::{
        BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
        EntityAuthorityFacts, EntityVersion, ExternalDeclarationIdentity, ExternalTarget,
        FactAvailability, ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin,
        IrBuilder, ItemKind, LanguageProfile, LinkKind, OccurrenceAuthorityFacts,
        ParentageAuthority, SemanticImageView, SemanticReader, SourceIdentity, SourceSpan,
        TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget, TypeScriptSource,
        TypeScriptSourceCoordinate, VariantAvailability, VariantFingerprint, Visibility,
        encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{CompileRecipeFact, NativeTool, PackageUrl, Stage};
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use std::collections::{BTreeMap, BTreeSet};

    fn fixture_version(identity: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
            core_payload: CorePayloadHash::from_raw([identity; 16]),
        }
    }

    fn identity(bytes: u8) -> DeclarationIdentity {
        DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([bytes; 16]),
            variant: VariantFingerprint::from_raw([bytes.wrapping_add(1); 16]),
        }
    }

    fn index(program: [u8; 32], source: [u8; 32]) -> ProjectCallableIndex {
        let target = identity(19);
        ProjectCallableIndex {
            by_path_name: BTreeMap::new(),
            by_owner_name: BTreeMap::new(),
            mention_by_path_name_kind: BTreeMap::new(),
            field_by_owner_name: BTreeMap::new(),
            value_by_owner_name_kind: BTreeMap::new(),
            tsz_source_coordinates: BTreeMap::from([(
                (
                    "src/service.ts".to_owned(),
                    source,
                    41,
                    82,
                    ItemKind::Function,
                ),
                vec![target],
            )]),
            tsz_program_identity: Some(program),
            python_program_identity: None,
        }
    }

    fn coordinate(
        program: [u8; 32],
        source: [u8; 32],
        declaration_start: u32,
        declaration_end: u32,
    ) -> TypeScriptSourceCoordinate<'static> {
        TypeScriptSourceCoordinate {
            program,
            source,
            path: "src/service.ts",
            declaration_start,
            declaration_end,
            name_start: 54,
        }
    }

    fn source_image(
        path: &str,
        source: &str,
        identity_byte: u8,
        target: Option<(String, u32, u32, u32, [u8; 32], [u8; 32], u32, u32)>,
    ) -> Result<(Vec<u8>, DeclarationIdentity), String> {
        source_image_with_capture(path, source, identity_byte, target, true)
    }

    pub(super) fn source_image_with_capture(
        path: &str,
        source: &str,
        identity_byte: u8,
        target: Option<(String, u32, u32, u32, [u8; 32], [u8; 32], u32, u32)>,
        captured: bool,
    ) -> Result<(Vec<u8>, DeclarationIdentity), String> {
        let source_identity =
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
        let source_facts = SourceIdentity {
            identity: source_identity,
            byte_len: u32::try_from(source.len()).map_err(|error| error.to_string())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            source_facts.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"tsz-source-join-fixture"),
        );
        let package = PackageUrl::parse("pkg:npm/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture package coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        if captured {
            builder
                .set_image_provenance_for_package(source_facts, recipe, &package, path)
                .map_err(|error| error.to_string())?;
        }
        let path_atom = builder
            .intern_atom(path.as_bytes())
            .map_err(|error| error.to_string())?;
        let is_caller = target.is_some();
        let method = if let Some((
            target_path,
            start,
            end,
            name_start,
            program,
            target_source,
            site_start,
            site_end,
        )) = target.as_ref()
        {
            let encoded = TypeScriptSourceCoordinate {
                program: *program,
                source: *target_source,
                path: target_path,
                declaration_start: *start,
                declaration_end: *end,
                name_start: *name_start,
            }
            .encode()
            .ok_or_else(|| "fixture TSZ coordinate was invalid".to_owned())?;
            let ecosystem = builder
                .intern_atom(TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes())
                .map_err(|error| error.to_string())?;
            let path = builder
                .intern_atom(encoded.as_bytes())
                .map_err(|error| error.to_string())?;
            let display = builder
                .intern_atom(b"getHello")
                .map_err(|error| error.to_string())?;
            let external = builder
                .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                    identity: ExternalDeclarationIdentity {
                        foreign: ForeignDeclarationId::from_raw([identity_byte; 16]),
                        variant: VariantAvailability::Unavailable,
                    },
                    origin: ForeignTargetOrigin::Universe { ecosystem },
                    path,
                    display,
                    kind: Some(ItemKind::Function),
                }))
                .map_err(|error| error.to_string())?;
            let link = TreeLinkInput {
                from: TreeEntityId::new(0),
                target: TreeLinkTarget::External(external),
                kind: LinkKind::MethodCall,
                confidence: Confidence::Compiler,
                authority: OccurrenceAuthorityFacts {
                    source: FactAvailability::Captured,
                },
                source: SourceSpan::new(path_atom, *site_start, *site_end),
            };
            Some(link)
        } else {
            None
        };
        let root_identity = fixture_version(identity_byte).identity();
        let root_authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let method_source = if is_caller {
            let (start, end) = target
                .as_ref()
                .map(|(_, _, _, _, _, _, start, end)| (*start, *end))
                .ok_or_else(|| "caller fixture is missing its call source span".to_owned())?;
            SourceSpan::new(path_atom, start, end)
        } else {
            let start = u32::try_from(
                source
                    .find("getHello(): string")
                    .ok_or_else(|| "service fixture is missing its method".to_owned())?,
            )
            .map_err(|error| error.to_string())?;
            let end = u32::try_from(
                source
                    .find("getHello(): string { return ''; }")
                    .ok_or_else(|| "service fixture is missing its method end".to_owned())?
                    + "getHello(): string { return ''; }".len(),
            )
            .map_err(|error| error.to_string())?;
            SourceSpan::new(path_atom, start, end)
        }
        .ok_or_else(|| "fixture method source span is invalid".to_owned())?;
        let root_members = if is_caller {
            Vec::new()
        } else {
            vec![TreeEntityId::new(1)]
        };
        let root = TreeItemInput {
            name: if is_caller { b"caller" } else { b"AppService" },
            anonymous_callable_anchor: None,
            kind: if is_caller {
                ItemKind::Function
            } else {
                ItemKind::Record
            },
            visibility: Visibility::Public,
            authority: root_authority,
            parent: None,
            semantic_type: None,
            members: &root_members,
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(path_atom, 0, source_facts.byte_len),
            extension: None,
        };
        let items = if is_caller {
            vec![root]
        } else {
            let method_authority = EntityAuthorityFacts {
                parentage: ParentageAuthority::Bound(root_identity),
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            };
            vec![
                root,
                TreeItemInput {
                    name: b"getHello",
                    anonymous_callable_anchor: None,
                    kind: ItemKind::Function,
                    visibility: Visibility::Public,
                    authority: method_authority,
                    parent: Some(TreeEntityId::new(0)),
                    semantic_type: None,
                    members: &[],
                    docs: &[],
                    attributes: &[],
                    source: Some(method_source),
                    extension: None,
                },
            ]
        };
        let versions = if is_caller {
            vec![fixture_version(identity_byte)]
        } else {
            vec![
                fixture_version(identity_byte),
                fixture_version(identity_byte.wrapping_add(1)),
            ]
        };
        let links = method.map_or_else(Vec::new, |link| {
            let mut reference = link;
            reference.kind = LinkKind::Reads;
            vec![link, reference]
        });
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        let entity_identity = if is_caller {
            fixture_version(identity_byte).identity()
        } else {
            fixture_version(identity_byte.wrapping_add(1)).identity()
        };
        Ok((bytes, entity_identity))
    }

    #[test]
    fn native_tsz_join_requires_exact_program_source_span_and_kind() {
        let program = [4; 32];
        let source = [8; 32];
        let index = index(program, source);
        let target = identity(19);
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate(program, source, 41, 82),
                ItemKind::Function
            ),
            Some(target)
        );
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate([5; 32], source, 41, 82),
                ItemKind::Function
            ),
            None,
            "same path and spelling in another TSZ program must stay external"
        );
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate(program, [9; 32], 41, 82),
                ItemKind::Function
            ),
            None,
            "edited source must not reuse an old target coordinate"
        );
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate(program, source, 40, 82),
                ItemKind::Function
            ),
            None,
            "a neighboring overload declaration must not capture this target"
        );
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate(program, source, 41, 82),
                ItemKind::Field
            ),
            None,
            "member ownership and declaration kind are part of the join"
        );
    }

    #[test]
    fn duplicate_exact_source_targets_refuse_the_join() {
        let program = [4; 32];
        let source = [8; 32];
        let mut index = index(program, source);
        index
            .tsz_source_coordinates
            .get_mut(&(
                "src/service.ts".to_owned(),
                source,
                41,
                82,
                ItemKind::Function,
            ))
            .expect("the exact target is indexed")
            .push(identity(23));
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate(program, source, 41, 82),
                ItemKind::Function
            ),
            None,
            "ambiguous exact declaration coordinates stay external"
        );
    }

    #[test]
    fn source_coordinate_stays_unresolved_when_program_scope_is_unassigned() {
        let mut index = index([4; 32], [8; 32]);
        index.tsz_program_identity = None;
        assert_eq!(
            index.resolve_tsz_source_coordinate(
                coordinate([4; 32], [8; 32], 41, 82),
                ItemKind::Function
            ),
            None,
            "an incomplete source manifest cannot claim a program-bound reference"
        );
    }

    #[test]
    fn native_tsz_source_call_retargets_to_exact_captured_declaration() -> Result<(), String> {
        let service = "export class AppService { getHello(): string { return ''; } }\n";
        let caller = "import { AppService } from './app.service.js';\nconst service = new AppService();\nservice.getHello();\n";
        let declaration_start = u32::try_from(
            service
                .find("getHello(): string")
                .ok_or_else(|| "service method should be present".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        let declaration_end = u32::try_from(
            service
                .find("getHello(): string { return ''; }")
                .ok_or_else(|| "service method body should be present".to_owned())?
                + "getHello(): string { return ''; }".len(),
        )
        .map_err(|error| error.to_string())?;
        let call_start = u32::try_from(
            caller
                .find("service.getHello()")
                .ok_or_else(|| "caller method call should be present".to_owned())?
                + "service.".len(),
        )
        .map_err(|error| error.to_string())?;
        let call_end =
            call_start + u32::try_from("getHello".len()).map_err(|error| error.to_string())?;
        let program = typescript_program_identity(&[
            (
                "src/app.controller.ts".to_owned(),
                *ContentId::<SourceFactDomain>::from_canonical_bytes(caller.as_bytes()),
            ),
            (
                "src/app.service.ts".to_owned(),
                *ContentId::<SourceFactDomain>::from_canonical_bytes(service.as_bytes()),
            ),
        ])
        .ok_or_else(|| "fixture project identity should be complete".to_owned())?;
        let (service_bytes, service_identity) =
            source_image("src/app.service.ts", service, 3, None)?;
        let (caller_bytes, caller_identity) = source_image(
            "src/app.controller.ts",
            caller,
            8,
            Some((
                "src/app.service.ts".to_owned(),
                declaration_start,
                declaration_end,
                declaration_start,
                program,
                *ContentId::<SourceFactDomain>::from_canonical_bytes(service.as_bytes()),
                call_start,
                call_end,
            )),
        )?;
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let external_call = caller_image
            .links_from(backend_semantic::ir::EntityId::new(0))
            .find_map(|(_, link)| match (link.kind, link.target) {
                (LinkKind::MethodCall, backend_semantic::ir::LinkTarget::External(external)) => {
                    Some(external)
                }
                _ => None,
            })
            .ok_or_else(|| "caller image should retain its TSZ call target".to_owned())?;
        let external_reference = caller_image
            .links_from(backend_semantic::ir::EntityId::new(0))
            .find_map(|(_, link)| match (link.kind, link.target) {
                (LinkKind::Reads, backend_semantic::ir::LinkTarget::External(external)) => {
                    Some(external)
                }
                _ => None,
            })
            .ok_or_else(|| "caller image should retain its TSZ reference target".to_owned())?;
        let published = BTreeSet::from([service_identity, caller_identity]);
        let paths = BTreeSet::from([
            "src/app.controller.ts".to_owned(),
            "src/app.service.ts".to_owned(),
        ]);
        let joined = join_project_call(
            &caller_image,
            LinkKind::MethodCall,
            external_call,
            "src/app.controller.ts",
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(joined, Some(service_identity));
        let referenced = join_project_field(
            &caller_image,
            LinkKind::Reads,
            external_reference,
            "src/app.controller.ts",
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(referenced, Some(service_identity));

        let edited_service = service.replace("return ''", "return 'edited'");
        let (edited_service_bytes, edited_identity) =
            source_image("src/app.service.ts", &edited_service, 3, None)?;
        let edited_images = [&edited_service_bytes[..], &caller_bytes[..]];
        let edited_index = ProjectCallableIndex::build_from_bytes(&edited_images)
            .map_err(|error| error.to_string())?;
        let edited_published = BTreeSet::from([edited_identity, caller_identity]);
        let stale = join_project_call(
            &caller_image,
            LinkKind::MethodCall,
            external_call,
            "src/app.controller.ts",
            &paths,
            &edited_index,
            &edited_published,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            stale, None,
            "a body-only source edit invalidates the previous program-bound target"
        );

        let edited_declaration_start = declaration_start;
        let edited_method = "getHello(): string { return 'edited'; }";
        let edited_declaration_end = u32::try_from(
            edited_service
                .find(edited_method)
                .ok_or_else(|| "edited service method should be present".to_owned())?
                + edited_method.len(),
        )
        .map_err(|error| error.to_string())?;
        let edited_program = typescript_program_identity(&[
            (
                "src/app.service.ts".to_owned(),
                *ContentId::<SourceFactDomain>::from_canonical_bytes(edited_service.as_bytes()),
            ),
            (
                "src/app.controller.ts".to_owned(),
                *ContentId::<SourceFactDomain>::from_canonical_bytes(caller.as_bytes()),
            ),
        ])
        .ok_or_else(|| "edited source project identity should be complete".to_owned())?;
        let (current_caller_bytes, current_caller_identity) = source_image(
            "src/app.controller.ts",
            caller,
            12,
            Some((
                "src/app.service.ts".to_owned(),
                edited_declaration_start,
                edited_declaration_end,
                declaration_start,
                edited_program,
                *ContentId::<SourceFactDomain>::from_canonical_bytes(edited_service.as_bytes()),
                call_start,
                call_end,
            )),
        )?;
        let current_images = [&edited_service_bytes[..], &current_caller_bytes[..]];
        let current_index = ProjectCallableIndex::build_from_bytes(&current_images)
            .map_err(|error| error.to_string())?;
        let current_published = BTreeSet::from([edited_identity, current_caller_identity]);
        let current_image =
            SemanticImageView::reopen(&current_caller_bytes).map_err(|error| error.to_string())?;
        let current_call = current_image
            .links_from(backend_semantic::ir::EntityId::new(0))
            .find_map(|(_, link)| match (link.kind, link.target) {
                (LinkKind::MethodCall, backend_semantic::ir::LinkTarget::External(external)) => {
                    Some(external)
                }
                _ => None,
            })
            .ok_or_else(|| "current caller image should retain its exact call target".to_owned())?;
        assert_eq!(
            join_project_call(
                &current_image,
                LinkKind::MethodCall,
                current_call,
                "src/app.controller.ts",
                &paths,
                &current_index,
                &current_published,
            )
            .map_err(|error| error.to_string())?,
            Some(edited_identity),
            "a fresh source coordinate joins to the current edited declaration"
        );
        Ok(())
    }
}

#[cfg(test)]
mod python_native_call_tests {
    use super::*;
    use backend_engine::application::{
        LocalCompilerHost, LocalHostDiscovery, LocalHostEnvironment, LocalHostVariable,
        OwnedPackageSource, OwnedPackageSourceSet,
    };
    use backend_library::interface::{
        CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
    };
    use backend_semantic::ir::{Confidence, LinkKind, LinkTarget};
    use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
    use std::{ffi::OsString, fs, path::PathBuf};

    #[derive(Clone)]
    struct NativePythonEnvironment(PathBuf);

    impl LocalHostEnvironment for NativePythonEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
            (variable == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
        }

        fn search_path(&self) -> Option<OsString> {
            Some(OsString::new())
        }
    }

    #[test]
    fn python_record_module_path_retains_exact_symbol_boundary() {
        assert_eq!(
            python_record_module_specifier("requests.models.Request", "Request"),
            Some("requests.models")
        );
        assert_eq!(
            python_record_module_specifier("requests.models", "Request"),
            Some("requests.models")
        );
        assert_eq!(python_record_module_specifier("Request", "Request"), None);
        assert_eq!(
            python_record_module_specifier("requests..Request", "Request"),
            None
        );
        assert_eq!(
            python_record_module_specifier("requests/models.Request", "Request"),
            None
        );
    }

    #[test]
    fn native_python_record_call_joins_class_without_retargeting_constructor() {
        let root = tempfile::tempdir().expect("isolated native producer fixture");
        let package_root = root.path().join("project");
        fs::create_dir_all(package_root.join("src/requests")).expect("package source root");
        let modules = [
            ("src/requests/__init__.py", ""),
            (
                "src/requests/models.py",
                "class Request:\n    def __init__(self):\n        self.label = 'request'\n",
            ),
            (
                "src/requests/sessions.py",
                "from .models import Request\n\ndef make():\n    # UTF-8: café\n    return Request()\n",
            ),
        ];
        for (path, source) in &modules {
            fs::write(package_root.join(path), source).expect("actual module bytes");
        }
        let client = LocalCompilerHost::new(
            NativePythonEnvironment(root.path().join("compiler")),
            LocalHostDiscovery::ExplicitOnly,
        )
        .open()
        .expect("compiled native producer with empty PATH and no configured tools");
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(98),
                profile: LanguageProfile::Python(PythonVersion::Python314),
                stage: Stage::LowerIr,
            },
            PackageUrl::try_from("pkg:pypi/requests@1.0.0".to_owned())
                .expect("exact package coordinate"),
        )
        .expect("native Python compilation profile");
        let sources = modules
            .iter()
            .map(|(path, source)| OwnedPackageSource::new(path, source).expect("captured source"))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let staged = client
            .compile_package_sources_staged(
                OwnedPackageSourceSet::new(request, package_root, sources)
                    .expect("complete actual source frontier"),
            )
            .expect("native Python authority and canonical lowering");
        let images = (0..staged.artifacts().len())
            .map(|ordinal| {
                SemanticImageView::reopen(
                    staged
                        .semantic_output_object(ordinal)
                        .expect("canonical semantic artifact")
                        .bytes(),
                )
                .expect("actual canonical image")
            })
            .collect::<Vec<_>>();
        let views = images.iter().collect::<Vec<_>>();
        let index =
            ProjectCallableIndex::build_from_views(&views).expect("actual declaration index");
        let paths = modules.iter().map(|(path, _)| path.to_string()).collect();
        let model_paths = BTreeSet::from(["src/requests/models.py".to_owned()]);
        let class = index
            .resolve_mention(&model_paths, "Request", ItemKind::Record)
            .expect("primary native class declaration");
        let constructor = index
            .resolve(&model_paths, "__init__")
            .expect("distinct native constructor declaration");
        assert_ne!(class, constructor);
        assert_eq!(index.resolve(&model_paths, "Request"), None);
        let published = BTreeSet::from([class, constructor]);
        let caller = images
            .iter()
            .find(|image| compiled_source_path(image).unwrap() == "src/requests/sessions.py")
            .expect("actual caller image");
        let expected_start = modules[2].1.rfind("Request()").unwrap() as u32;
        let mut observed_calls = 0;
        for (_, link) in caller.canonical_links() {
            let LinkTarget::External(external) = link.target else {
                continue;
            };
            let Some(ExternalTarget::Foreign(foreign)) = caller.external(external) else {
                continue;
            };
            if link.kind != LinkKind::Calls || foreign.kind != Some(ItemKind::Record) {
                continue;
            }
            let coordinate = caller
                .atom(foreign.path)
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(PythonSourceCoordinate::decode)
                .expect("native selected-source coordinate");
            assert_eq!(coordinate.0.path, "src/requests/models.py");
            assert!(matches!(foreign.origin,
                ForeignTargetOrigin::Universe { ecosystem }
                    if caller.atom(ecosystem) == Some(PYTHON_NATIVE_SOURCE_ECOSYSTEM.as_bytes())));
            assert_eq!(caller.atom(foreign.display), Some(b"Request".as_slice()));
            assert_eq!(link.confidence, Confidence::Compiler);
            let source = link.source.expect("compiler-observed call byte range");
            assert_eq!(
                (source.start(), source.end()),
                (expected_start, expected_start + 7)
            );
            let constructor_only = BTreeSet::from([constructor]);
            let absent_paths = BTreeSet::new();
            let join = |paths, published| {
                join_project_call(
                    caller,
                    link.kind,
                    external,
                    "src/requests/sessions.py",
                    paths,
                    &index,
                    published,
                )
                .expect("exact compiler call join")
            };
            assert_eq!(join(&paths, &published), Some(class));
            assert_eq!(join(&paths, &constructor_only), None);
            assert_eq!(join(&absent_paths, &published), None);
            observed_calls += 1;
        }
        assert_eq!(observed_calls, 1, "native class binding must be present");
    }

    #[test]
    fn native_python_relative_function_calls_join_exact_sources_and_reject_unproven_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let package_root = root.path().join("project");
        fs::create_dir_all(package_root.join("core/api"))?;
        let modules = [
            ("core/__init__.py", ""),
            ("core/api/__init__.py", ""),
            (
                "core/utils.py",
                "def generate_s3_authorization_headers(key):\n    return 'other module'\n",
            ),
            (
                "core/api/utils.py",
                "# UTF-8: 🐍\ndef generate_s3_authorization_headers(key):\n    return key\n",
            ),
            (
                "core/api/viewsets.py",
                "from . import utils as helper\nfrom .utils import generate_s3_authorization_headers as generate\nfrom external import generate_s3_authorization_headers as external\n\ndef generate_s3_authorization_headers(key):\n    return 'same name in caller'\n\ndef qualified(key):\n    return helper.generate_s3_authorization_headers(key)\n\ndef direct(key):\n    return generate(key)\n\ndef shadowed(helper, key):\n    return helper.generate_s3_authorization_headers(key)\n\ndef shadowed_local(generate_s3_authorization_headers, key):\n    return generate_s3_authorization_headers(key)\n\ndef unselected(key):\n    return external(key)\n",
            ),
        ];
        for (path, source) in modules {
            fs::write(package_root.join(path), source)?;
        }
        let client = LocalCompilerHost::new(
            NativePythonEnvironment(root.path().join("compiler")),
            LocalHostDiscovery::ExplicitOnly,
        )
        .open()?;
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(99),
                profile: LanguageProfile::Python(PythonVersion::Python314),
                stage: Stage::LowerIr,
            },
            PackageUrl::try_from("pkg:pypi/docs@1.0.0".to_owned())
                .map_err(|error| format!("fixture package coordinate rejected: {error:?}"))?,
        )
        .map_err(|error| format!("fixture package profile rejected: {error:?}"))?;
        let sources = modules
            .iter()
            .map(|(path, source)| OwnedPackageSource::new(path, source))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
            request,
            package_root,
            sources,
        )?)?;
        let images = (0..staged.artifacts().len())
            .map(|ordinal| {
                let bytes = staged
                    .semantic_output_object(ordinal)
                    .ok_or("missing semantic output")?
                    .bytes();
                Ok(SemanticImageView::reopen(bytes)?)
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let views = images.iter().collect::<Vec<_>>();
        let python_only = ProjectCallableIndex::build_from_views(&views)?;
        // A captured unrelated language may have partial declaration authority;
        // its recipe still proves that it is outside the Python source frontier.
        let ts_source = "class AppService { getHello(): string { return ''; } }";
        let (ts_bytes, _) = super::tsz_source_coordinate_tests::source_image_with_capture(
            "src/service.ts",
            ts_source,
            71,
            None,
            true,
        )?;
        let ts_image = SemanticImageView::reopen(&ts_bytes)?;
        let mut mixed = views.clone();
        mixed.push(&ts_image);
        let index = ProjectCallableIndex::build_from_views(&mixed)?;
        assert_eq!(
            index.python_program_identity,
            python_only.python_program_identity
        );
        assert!(index.python_program_identity.is_some());
        // Unavailable provenance has neither a source path nor a language
        // recipe. The index must refuse it rather than guess from a suffix.
        let (unavailable_bytes, _) = super::tsz_source_coordinate_tests::source_image_with_capture(
            "src/service.ts",
            ts_source,
            72,
            None,
            false,
        )?;
        let unavailable_image = SemanticImageView::reopen(&unavailable_bytes)?;
        let mut unproven = views.clone();
        unproven.push(&unavailable_image);
        assert!(ProjectCallableIndex::build_from_views(&unproven).is_err());
        let paths = modules
            .iter()
            .map(|(path, _)| (*path).to_owned())
            .collect::<BTreeSet<_>>();
        let exact_path = BTreeSet::from(["core/api/utils.py".to_owned()]);
        let target = index
            .resolve(&exact_path, "generate_s3_authorization_headers")
            .ok_or("target absent")?;
        let other = index
            .resolve(
                &BTreeSet::from(["core/utils.py".to_owned()]),
                "generate_s3_authorization_headers",
            )
            .ok_or("same-name control absent")?;
        assert_ne!(target, other);
        let published = BTreeSet::from([target, other]);
        let caller = images
            .iter()
            .find(|image| {
                compiled_source_path(image).ok().as_deref() == Some("core/api/viewsets.py")
            })
            .ok_or("caller absent")?;
        let shadow_start = modules[4].1.find("def shadowed").ok_or("shadow control")? as u32;
        let mut joined = 0;
        let mut unresolved = 0;
        let mut stale = ProjectCallableIndex::build_from_views(&views)?;
        stale.python_program_identity = Some([0; 32]);
        for (_, link) in caller.canonical_links() {
            if !matches!(link.kind, LinkKind::Calls | LinkKind::MethodCall) {
                continue;
            }
            let LinkTarget::External(external) = link.target else {
                return Err("native unresolved call borrowed a local identity".into());
            };
            let source = link.source.ok_or("call source absence")?;
            let result = join_project_call(
                caller,
                link.kind,
                external,
                "core/api/viewsets.py",
                &paths,
                &index,
                &published,
            )?;
            if source.start() < shadow_start {
                assert_eq!(result, Some(target));
                assert_eq!(link.confidence, Confidence::Compiler);
                assert_eq!(
                    join_project_call(
                        caller,
                        link.kind,
                        external,
                        "core/api/viewsets.py",
                        &BTreeSet::new(),
                        &index,
                        &published
                    )?,
                    None
                );
                assert_eq!(
                    join_project_call(
                        caller,
                        link.kind,
                        external,
                        "core/api/viewsets.py",
                        &paths,
                        &stale,
                        &published
                    )?,
                    None
                );
                assert_eq!(
                    join_project_call(
                        caller,
                        link.kind,
                        external,
                        "core/api/viewsets.py",
                        &paths,
                        &index,
                        &BTreeSet::from([other])
                    )?,
                    None
                );
                joined += 1;
            } else {
                assert_eq!(
                    result, None,
                    "shadowed and unselected external targets cannot borrow same-name local authority"
                );
                unresolved += 1;
            }
        }
        assert_eq!(joined, 2);
        assert_eq!(unresolved, 3);
        Ok(())
    }
}
