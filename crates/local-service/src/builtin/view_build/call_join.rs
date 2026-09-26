use super::super::{BuiltinModelError, IndexedSources};
use super::structural::resolve_specifier_paths;
use super::{compiled_source_path, semantic_symbol};
use backend_engine::PackageKey;
use backend_semantic::ir::{
    DeclarationIdentity, ExternalId, ExternalTarget, ForeignTargetOrigin, ItemKind,
    SemanticCoreReader, SemanticImageView, SemanticReader,
};
use backend_engine::application::DocumentationSession;
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
        ItemKind::Record
            | ItemKind::Enum
            | ItemKind::Trait
            | ItemKind::Alias
            | ItemKind::Module
    )
}

pub(crate) struct ProjectCallableIndex {
    by_path_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
    by_owner_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
    mention_by_path_name_kind: BTreeMap<(String, String, ItemKind), Vec<DeclarationIdentity>>,
    field_by_owner_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
}

impl ProjectCallableIndex {
    pub(crate) fn build_from_bytes(images: &[&[u8]]) -> Result<Self, BuiltinModelError> {
        let mut by_path_name = BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        let mut by_owner_name = BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        let mut mention_by_path_name_kind =
            BTreeMap::<(String, String, ItemKind), Vec<DeclarationIdentity>>::new();
        let mut field_by_owner_name =
            BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        for bytes in images {
            let image = SemanticImageView::reopen(bytes).map_err(|error| {
                BuiltinModelError(format!("reopen semantic graph image: {error}"))
            })?;
            let path = compiled_source_path(&image)?;
            let session = DocumentationSession::new(&image);
            for entity in session.canonical_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read semantic graph callable: {error}"))
                })?;
                let name = std::str::from_utf8(entity.name).map_err(|_| {
                    BuiltinModelError("semantic graph callable name is not UTF-8".to_owned())
                })?;
                let identity = entity.entity.version.identity();
                if semantic_callable(entity.entity.kind) {
                    by_path_name
                        .entry((path.clone(), name.to_owned()))
                        .or_default()
                        .push(identity);
                    if let Some((immediate, chain)) =
                        owner_chain_keys(&session, &image, entity.entity.id)?
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
                if entity.entity.kind == ItemKind::Field {
                    if let Some((immediate, chain)) =
                        owner_chain_keys(&session, &image, entity.entity.id)?
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
            }
        }
        Ok(Self {
            by_path_name,
            by_owner_name,
            mention_by_path_name_kind,
            field_by_owner_name,
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
            if let Some(identities) = self
                .mention_by_path_name_kind
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

    pub(crate) fn resolve_owner(&self, namespace: &str, display: &str) -> Option<DeclarationIdentity> {
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

    fn owner_matches(
        &self,
        namespace: &str,
        display: &str,
    ) -> Option<Vec<DeclarationIdentity>> {
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
}

pub(crate) fn owner_chain_keys(
    session: &DocumentationSession<'_, SemanticImageView<'_>>,
    image: &SemanticImageView<'_>,
    function: backend_semantic::ir::EntityId,
) -> Result<Option<(String, String)>, BuiltinModelError> {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = session
        .entity(function)
        .map_err(|error| BuiltinModelError(format!("read semantic graph callable parent: {error}")))?
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
        let parent = session
            .entity(parent_id)
            .map_err(|error| {
                BuiltinModelError(format!("read semantic graph callable ancestor: {error}"))
            })?;
        let name_atom = image
            .atom(parent.entity.name)
            .ok_or_else(|| {
                BuiltinModelError("semantic graph ancestor name atom is missing".to_owned())
            })?;
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

pub(crate) fn foreign_dotted_module_specifier<'a>(path: &'a str, display: &'a str) -> Option<&'a str> {
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

pub(crate) fn foreign_package_call_retarget(
    image: &SemanticImageView<'_>,
    external: ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    callable_index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
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
    let path = std::str::from_utf8(path_atom).map_err(|_| {
        BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned())
    })?;
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(callable_index.resolve(&resolved_paths, display))
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
    let namespace = std::str::from_utf8(namespace_atom).map_err(|_| {
        BuiltinModelError("semantic graph namespace is not UTF-8".to_owned())
    })?;
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
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
    let namespace = std::str::from_utf8(namespace_atom).map_err(|_| {
        BuiltinModelError("semantic graph namespace is not UTF-8".to_owned())
    })?;
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
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
    let path = std::str::from_utf8(path_atom).map_err(|_| {
        BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned())
    })?;
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
    let specifier = foreign_dotted_module_specifier(path, display).unwrap_or(package);
    let resolved_paths = resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(index.resolve_mention(&resolved_paths, display, kind))
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
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
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
        backend_semantic::ir::LinkKind::TypeReference
            | backend_semantic::ir::LinkKind::Imports
    ) {
        return Ok(None);
    }
    let identity = if let Some(identity) = foreign_package_mention_retarget(
        image,
        external,
        caller_path,
        project_paths,
        index,
    )? {
        Some(identity)
    } else if matches!(link_kind, backend_semantic::ir::LinkKind::TypeReference) {
        foreign_qualified_type_mention_retarget(image, external, index)?
    } else {
        None
    };
    Ok(identity.filter(|candidate| published.contains(candidate)))
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
    let path = std::str::from_utf8(path_atom).map_err(|_| {
        BuiltinModelError("semantic graph foreign path is not UTF-8".to_owned())
    })?;
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
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
    let identity = if let Some(identity) = foreign_package_field_retarget(
        image,
        external,
        caller_path,
        project_paths,
        index,
    )? {
        Some(identity)
    } else {
        foreign_namespace_field_retarget(image, external, index)?
    };
    Ok(identity.filter(|candidate| published.contains(candidate)))
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
    let identity = if let Some(identity) = foreign_package_call_retarget(
        image,
        external,
        caller_path,
        project_paths,
        index,
    )? {
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
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
    if display.is_empty() {
        return Ok(None);
    }
    Ok(Some(display.to_owned()))
}
