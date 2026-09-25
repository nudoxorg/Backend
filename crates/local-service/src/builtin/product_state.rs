//! Durable typed owner for follows, projects, and the shared session tree.

use backend_engine::{
    DeclarationRecord, DependencyFacts, ForgeManifestRecord, ForgePackageFact, ForgePackageRecord,
    ForgeRepositoryMetadataRecord, PackageDependencyRecord, PackageDependencySourceFacts,
    PackageReference, ProductText, ProductTreeNodeId as TreeNodeId, ProjectId, ProjectName,
    ProjectRecord, ProjectSelector, RegistryMetadata, RegistryPackageRecord, ReleaseRecord, RowId,
    SubscriptionRecord, SurfaceCommand, SurfaceReply, TreeNodeRecord, TreeOpener, TreeSubject,
    ViewRoot,
};
use backend_library::{
    AdvisoryPackageDto, CommandMutation, RegistryDownloadCount, RegistryEcosystem,
    RegistryFactAvailability, RegistryNativeMetadata, RegistryReleaseStanding, Row, command_spec,
};
use backend_platform::durable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

const STATE_VERSION: u16 = 1;
const MAX_PROJECTS: usize = 256;
const MAX_PROJECT_MEMBERS: usize = 1024;
const MAX_SUBSCRIPTIONS: usize = 4096;
const MAX_TREE_NODES: usize = 2048;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredState {
    version: u16,
    epoch: u64,
    next_project: NonZeroU64,
    next_node: NonZeroU64,
    projects: Vec<ProjectRecord>,
    subscriptions: Vec<SubscriptionRecord>,
    tree: Vec<TreeNodeRecord>,
    active: Option<TreeNodeId>,
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            epoch: 0,
            next_project: NonZeroU64::MIN,
            next_node: NonZeroU64::MIN,
            projects: Vec::new(),
            subscriptions: Vec::new(),
            tree: Vec::new(),
            active: None,
        }
    }
}

pub(super) struct ProductState {
    path: PathBuf,
    state: StoredState,
}

impl ProductState {
    pub(super) fn open(path: PathBuf) -> Result<Self, String> {
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("decode product state: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoredState::default(),
            Err(error) => return Err(format!("read product state: {error}")),
        };
        if state.version != STATE_VERSION
            || state.projects.len() > MAX_PROJECTS
            || state.subscriptions.len() > MAX_SUBSCRIPTIONS
            || state.tree.len() > MAX_TREE_NODES
        {
            return Err("product state violates its version or cardinality bounds".to_owned());
        }
        Ok(Self { path, state })
    }

    pub(super) fn execute(
        &mut self,
        command: SurfaceCommand,
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        dependency_facts: &[PackageDependencySourceFacts],
    ) -> Result<SurfaceReply, String> {
        command.admit().map_err(|error| error.to_string())?;
        match command_spec(command.id()).mutation {
            CommandMutation::Read => {
                let (reply, changed) =
                    self.execute_admitted(command, view, catalog, dependency_facts)?;
                if changed {
                    return Err("read command attempted to mutate product state".to_owned());
                }
                reply.admit(reply.id()).map_err(|error| error.to_string())?;
                Ok(reply)
            }
            CommandMutation::Write => {
                let mut pending = Self {
                    path: self.path.clone(),
                    state: self.state.clone(),
                };
                let (reply, changed) =
                    pending.execute_admitted(command, view, catalog, dependency_facts)?;
                reply.admit(reply.id()).map_err(|error| error.to_string())?;
                if changed {
                    pending.commit()?;
                    self.state = pending.state;
                }
                Ok(reply)
            }
        }
    }

    fn execute_admitted(
        &mut self,
        command: SurfaceCommand,
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        dependency_facts: &[PackageDependencySourceFacts],
    ) -> Result<(SurfaceReply, bool), String> {
        let (reply, changed) = match command {
            SurfaceCommand::Advisory {
                package,
                override_evidence,
            } => {
                let advisory = catalog
                    .iter()
                    .find(|record| record.coordinate == package)
                    .map(|record| record.advisory.clone())
                    .unwrap_or_else(backend_engine::AdvisoryPackageDto::unknown);
                let advisory = override_evidence.map_or(advisory.clone(), |evidence| {
                    advisory.with_override(evidence, unix_seconds())
                });
                (SurfaceReply::Advisory(advisory), false)
            }
            SurfaceCommand::Read { locators } => {
                (SurfaceReply::Read(read(view, &locators)?), false)
            }
            SurfaceCommand::Diff { from, to } => {
                let _ = (from, to);
                return Err("semantic diff requires compiler publication authority".to_owned());
            }
            SurfaceCommand::References { .. } => {
                return Err("references require compiler publication authority".to_owned());
            }
            SurfaceCommand::Explore { query, limit } => (
                SurfaceReply::Explored(explore_page(
                    view,
                    catalog,
                    query.as_ref(),
                    limit,
                )?),
                false,
            ),
            SurfaceCommand::IndexSearch { query, limit } => (
                SurfaceReply::IndexSearch(catalog_page(catalog, Some(&query), limit)),
                false,
            ),
            SurfaceCommand::Package { package } => {
                (SurfaceReply::Package(package_page(view, catalog, &package)?), false)
            }
            SurfaceCommand::ForgeAdd { coordinate } => (
                SurfaceReply::ForgePackageAdded(forge_unavailable(&coordinate)),
                false,
            ),
            SurfaceCommand::ForgeReference { coordinate } => (
                SurfaceReply::ForgePackageReferenced(forge_unavailable(&coordinate)),
                false,
            ),
            SurfaceCommand::Dependencies { package } => (
                SurfaceReply::Dependencies(dependencies(dependency_facts, &package)?),
                false,
            ),
            SurfaceCommand::PackageVersions { package } => (
                SurfaceReply::PackageVersions(versions(catalog, &package)),
                false,
            ),
            SurfaceCommand::SemanticVersions { .. }
            | SurfaceCommand::SelectSemanticVersion { .. } => {
                return Err(
                    "semantic version history requires compiler publication authority".to_owned(),
                );
            }
            SurfaceCommand::PackageProfile { package } => (profile(catalog, &package), false),
            SurfaceCommand::Dependents { package } => (
                SurfaceReply::Dependents(dependents(catalog, dependency_facts, &package)?),
                false,
            ),
            SurfaceCommand::Owner { owner } => (
                SurfaceReply::Owner(RegistryMetadata::NotRecorded(
                    ProductText::new(format!(
                        "the configured feed does not record publisher facts for {}",
                        owner.as_str()
                    ))
                    .map_err(|e| e.to_string())?,
                )),
                false,
            ),
            SurfaceCommand::Subscribe { package, project } => (
                SurfaceReply::Subscribed(self.subscribe(package, project.as_ref())?),
                true,
            ),
            SurfaceCommand::Unsubscribe { package } => {
                (SurfaceReply::Unsubscribed(self.unsubscribe(&package)), true)
            }
            SurfaceCommand::Subscriptions => (
                SurfaceReply::Subscriptions(self.state.subscriptions.clone().into_boxed_slice()),
                false,
            ),
            SurfaceCommand::Releases { mark_seen } => (
                SurfaceReply::Releases(self.releases(catalog, mark_seen)),
                mark_seen,
            ),
            SurfaceCommand::Projects => (
                SurfaceReply::Projects(self.state.projects.clone().into_boxed_slice()),
                false,
            ),
            SurfaceCommand::ProjectCreate { name, lockfile } => (
                SurfaceReply::ProjectCreated(self.create_project(name, lockfile)?),
                true,
            ),
            SurfaceCommand::ProjectDelete { project } => (
                SurfaceReply::ProjectDeleted(self.delete_project(&project)?),
                true,
            ),
            SurfaceCommand::ProjectAdd { project, package } => (
                SurfaceReply::ProjectAdded(self.change_member(&project, package, true)?),
                true,
            ),
            SurfaceCommand::ProjectRemove { project, package } => (
                SurfaceReply::ProjectRemoved(self.change_member(&project, package, false)?),
                true,
            ),
            SurfaceCommand::ProjectSync { project } => (
                SurfaceReply::ProjectSynced(self.sync_project(&project)?),
                true,
            ),
            SurfaceCommand::Tree => (SurfaceReply::Tree(self.current_tree()), false),
            SurfaceCommand::TreeOpen {
                subject,
                parent,
                title,
                opener,
            } => (
                SurfaceReply::TreeOpened(self.open_tree(view, subject, parent, title, opener)?),
                true,
            ),
            SurfaceCommand::TreeClose { node, branch } => (
                SurfaceReply::TreeClosed(self.close_tree(node, branch)?),
                true,
            ),
        };
        Ok((reply, changed))
    }

    fn commit(&mut self) -> Result<(), String> {
        self.state.epoch = self
            .state
            .epoch
            .checked_add(1)
            .ok_or("product epoch overflow")?;
        let bytes = serde_json::to_vec(&self.state).map_err(|error| error.to_string())?;
        durable::write_atomic(&self.path, &bytes).map_err(|error| error.to_string())
    }

    fn project_index(&self, selector: &ProjectSelector) -> Option<usize> {
        self.state
            .projects
            .iter()
            .position(|project| match selector {
                ProjectSelector::Id(id) => project.id == *id,
                ProjectSelector::Name(name) => project.name == *name,
            })
    }

    fn subscribe(
        &mut self,
        package: PackageReference,
        selector: Option<&ProjectSelector>,
    ) -> Result<SubscriptionRecord, String> {
        let project = selector
            .map(|value| {
                self.project_index(value)
                    .map(|index| self.state.projects[index].id)
                    .ok_or("unknown project")
            })
            .transpose()?;
        if let Some(row) = self
            .state
            .subscriptions
            .iter_mut()
            .find(|row| row.package == package)
        {
            row.project = project;
            return Ok(row.clone());
        }
        if self.state.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err("subscription store is full".to_owned());
        }
        let row = SubscriptionRecord {
            package,
            project,
            seen: None,
        };
        self.state.subscriptions.push(row.clone());
        Ok(row)
    }

    fn unsubscribe(&mut self, package: &PackageReference) -> bool {
        let before = self.state.subscriptions.len();
        self.state
            .subscriptions
            .retain(|row| &row.package != package);
        before != self.state.subscriptions.len()
    }

    fn releases(
        &mut self,
        catalog: &[RegistryPackageRecord],
        mark_seen: bool,
    ) -> Box<[ReleaseRecord]> {
        let mut result = Vec::new();
        for subscription in &mut self.state.subscriptions {
            let mut matches = catalog
                .iter()
                .filter(|row| package_matches(&subscription.package, row))
                .collect::<Vec<_>>();
            matches.sort_by(|a, b| b.version.cmp(&a.version));
            for row in &matches {
                if subscription.seen.as_ref() != Some(&row.version) {
                    result.push(ReleaseRecord {
                        package: subscription.package.clone(),
                        version: row.version.clone(),
                        seen: mark_seen,
                    });
                }
            }
            if mark_seen && let Some(latest) = matches.first() {
                subscription.seen = Some(latest.version.clone());
            }
        }
        if result.len() > backend_engine::MAX_PRODUCT_ROWS {
            result.truncate(backend_engine::MAX_PRODUCT_ROWS);
        }
        result.into_boxed_slice()
    }

    fn create_project(
        &mut self,
        name: ProjectName,
        lockfile: Option<ProductText>,
    ) -> Result<ProjectRecord, String> {
        if self
            .state
            .projects
            .iter()
            .any(|project| project.name == name)
        {
            return Err("project name already exists".to_owned());
        }
        if self.state.projects.len() >= MAX_PROJECTS {
            return Err("project store is full".to_owned());
        }
        if let Some(path) = lockfile.as_ref() {
            validate_lockfile(Path::new(path.as_str()))?;
        }
        let id = ProjectId::new(self.state.next_project);
        self.state.next_project =
            NonZeroU64::new(id.get().checked_add(1).ok_or("project identity overflow")?)
                .ok_or("project identity overflow")?;
        let project = ProjectRecord {
            id,
            name,
            lockfile,
            members: Box::new([]),
        };
        self.state.projects.push(project.clone());
        Ok(project)
    }

    fn delete_project(&mut self, selector: &ProjectSelector) -> Result<ProjectId, String> {
        let index = self.project_index(selector).ok_or("unknown project")?;
        let id = self.state.projects.remove(index).id;
        for subscription in &mut self.state.subscriptions {
            if subscription.project == Some(id) {
                subscription.project = None;
            }
        }
        Ok(id)
    }

    fn change_member(
        &mut self,
        selector: &ProjectSelector,
        package: PackageReference,
        add: bool,
    ) -> Result<ProjectRecord, String> {
        let index = self.project_index(selector).ok_or("unknown project")?;
        let project = &mut self.state.projects[index];
        let mut members = project.members.to_vec();
        if add && !members.contains(&package) {
            if members.len() >= MAX_PROJECT_MEMBERS {
                return Err("project member store is full".to_owned());
            }
            members.push(package);
        } else if !add {
            members.retain(|member| member != &package);
        }
        members.sort();
        project.members = members.into_boxed_slice();
        Ok(project.clone())
    }

    fn sync_project(&mut self, selector: &ProjectSelector) -> Result<ProjectRecord, String> {
        let index = self.project_index(selector).ok_or("unknown project")?;
        let path = self.state.projects[index]
            .lockfile
            .as_ref()
            .ok_or("project has no lockfile")?;
        self.state.projects[index].members = parse_lockfile(Path::new(path.as_str()))?;
        Ok(self.state.projects[index].clone())
    }

    fn current_tree(&self) -> Box<[TreeNodeRecord]> {
        self.state
            .tree
            .iter()
            .cloned()
            .map(|mut row| {
                row.active = Some(row.id) == self.state.active;
                row
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    fn open_tree(
        &mut self,
        view: &ViewRoot,
        subject: TreeSubject,
        parent: Option<TreeNodeId>,
        title: Option<ProductText>,
        opener: TreeOpener,
    ) -> Result<TreeNodeRecord, String> {
        let (subject, title) = resolve_tree_subject(view, subject, title)?;
        if parent.is_some_and(|parent| !self.state.tree.iter().any(|row| row.id == parent)) {
            return Err("tree parent does not exist".to_owned());
        }
        if let Some(index) = self
            .state
            .tree
            .iter()
            .position(|row| row.subject == subject && row.parent == parent)
        {
            self.state.active = Some(self.state.tree[index].id);
            self.state.tree[index].active = true;
            return Ok(self.state.tree[index].clone());
        }
        if self.state.tree.len() >= MAX_TREE_NODES {
            return Err("session tree is full".to_owned());
        }
        let id = TreeNodeId::new(self.state.next_node);
        self.state.next_node =
            NonZeroU64::new(id.get().checked_add(1).ok_or("tree identity overflow")?)
                .ok_or("tree identity overflow")?;
        for row in &mut self.state.tree {
            row.active = false;
        }
        let row = TreeNodeRecord {
            id,
            parent,
            subject,
            title,
            opener,
            active: true,
        };
        self.state.active = Some(id);
        self.state.tree.push(row.clone());
        Ok(row)
    }

    fn close_tree(&mut self, node: TreeNodeId, branch: bool) -> Result<u64, String> {
        if !self.state.tree.iter().any(|row| row.id == node) {
            return Err("tree node does not exist".to_owned());
        }
        let mut removed = BTreeSet::from([node]);
        if branch {
            loop {
                let before = removed.len();
                for row in &self.state.tree {
                    if row.parent.is_some_and(|p| removed.contains(&p)) {
                        removed.insert(row.id);
                    }
                }
                if before == removed.len() {
                    break;
                }
            }
        }
        let count = u64::try_from(removed.len()).map_err(|_| "tree close count overflow")?;
        self.state.tree.retain(|row| !removed.contains(&row.id));
        if self.state.active.is_some_and(|id| removed.contains(&id)) {
            self.state.active = None;
        }
        Ok(count)
    }
}

fn forge_unavailable(coordinate: &ProductText) -> ForgePackageRecord {
    let reason =
        ProductText::from_static("forge acquisition authority is not configured in this owner");
    let unavailable_text = || ForgePackageFact::<ProductText>::Unavailable(reason.clone());
    let unavailable_topics = || ForgePackageFact::<Box<[ProductText]>>::Unavailable(reason.clone());
    let unavailable_number = || ForgePackageFact::<u64>::Unavailable(reason.clone());
    let owner = ProductText::from_static("unknown");
    let repository = ProductText::from_static("unknown");
    let unavailable_metadata = ForgeRepositoryMetadataRecord {
        owner: unavailable_text(),
        description: unavailable_text(),
        license: unavailable_text(),
        readme: unavailable_text(),
        topics: unavailable_topics(),
        stars: unavailable_number(),
        forks: unavailable_number(),
    };
    ForgePackageRecord {
        coordinate: coordinate.clone(),
        provider: ProductText::from_static("unknown"),
        owner,
        repository,
        revision: coordinate.clone(),
        subdir: None,
        commit: unavailable_text(),
        tree: unavailable_text(),
        metadata: unavailable_metadata,
        manifests: Box::new([] as [ForgeManifestRecord; 0]),
        source: unavailable_text(),
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read(view: &ViewRoot, locators: &[ProductText]) -> Result<Box<[DeclarationRecord]>, String> {
    locators
        .iter()
        .map(|locator| {
            let row = view.rows().iter().find(|row| row.label == locator.as_str());
            Ok(DeclarationRecord {
                label: locator.clone(),
                stable_id: row.map(|value| match value.id {
                    RowId::Package(id) => id.to_bytes(),
                    RowId::Symbol(id) => id.to_bytes(),
                    RowId::Object(id) => id.to_bytes(),
                }),
                signature: row
                    .and_then(|value| {
                        value
                            .excerpt
                            .text()
                            .or(value.signature.as_deref())
                    })
                    .map(ProductText::new)
                    .transpose()
                    .map_err(|e| e.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Vec::into_boxed_slice)
}

fn explore_page(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    query: Option<&ProductText>,
    limit: u16,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    if let Some(query_text) = query
        && let Some(project_root) = indexed_project_for_query(view, query_text.as_str())
    {
        return indexed_explore_page(view, &project_root, query_text.as_str(), limit);
    }
    let registry = catalog_page(catalog, query, limit);
    if !registry.is_empty() {
        return Ok(registry);
    }
    if let Some(query_text) = query
        && let Some(project_root) = indexed_project_for_query(view, query_text.as_str())
    {
        return indexed_explore_page(view, &project_root, query_text.as_str(), limit);
    }
    Ok(registry)
}

fn indexed_project_for_query(view: &ViewRoot, query: &str) -> Option<String> {
    for row in view.rows() {
        if matches!(row.id, RowId::Package(_)) && row.label == query {
            return Some(row.label.clone());
        }
    }
    view.rows().iter().find_map(|row| {
        row.label
            .split_once("::")
            .and_then(|(project, _)| (project == query).then(|| project.to_owned()))
    })
}

fn indexed_explore_page(
    view: &ViewRoot,
    project_root: &str,
    query: &str,
    limit: u16,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    let prefix = format!("{project_root}::");
    let filter = query.to_ascii_lowercase();
    let mut records = Vec::new();
    for row in view.rows() {
        if !matches!(row.id, RowId::Symbol(_)) || !row.label.starts_with(&prefix) {
            continue;
        }
        if query != project_root
            && !row.label.to_ascii_lowercase().contains(&filter)
            && !row
                .label
                .rsplit("::")
                .next()
                .is_some_and(|name| name.to_ascii_lowercase().contains(&filter))
        {
            continue;
        }
        records.push(declaration_explore_record(row)?);
        if records.len() >= usize::from(limit) {
            break;
        }
    }
    Ok(records.into_boxed_slice())
}

fn declaration_identity(row: &Row) -> Result<(String, String), String> {
    let name = row
        .label
        .rsplit("::")
        .next()
        .ok_or_else(|| format!("declaration row {} has no symbol name", row.label))?;
    let path = row
        .source
        .captured()
        .map(|location| location.path().to_owned())
        .or_else(|| {
            row.label
                .split_once("::")
                .and_then(|(_, rest)| rest.rsplit_once("::"))
                .map(|(path, _)| path)
                .map(|path| {
                    path.rsplit_once(':')
                        .map_or(path, |(path_without_line, _)| path_without_line)
                        .to_owned()
                })
        })
        .ok_or_else(|| format!("declaration row {} has no source path", row.label))?;
    Ok((name.to_owned(), path))
}

fn declaration_explore_record(row: &Row) -> Result<RegistryPackageRecord, String> {
    let (name, path) = declaration_identity(row)?;
    Ok(RegistryPackageRecord {
        coordinate: PackageReference::Local(
            ProductText::new(row.label.clone()).map_err(|error| error.to_string())?,
        ),
        ecosystem: RegistryEcosystem::Cargo,
        name: ProductText::new(name).map_err(|error| error.to_string())?,
        version: ProductText::new(path).map_err(|error| error.to_string())?,
        bytes: 0,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
        facts_version: [0; 32],
        native_metadata_version: RegistryNativeMetadata::unavailable(
            RegistryEcosystem::Cargo,
            "indexed declaration",
        )
        .identity()
        .map_err(|error| error.to_string())?,
        native_metadata: RegistryNativeMetadata::unavailable(
            RegistryEcosystem::Cargo,
            "indexed declaration",
        ),
        forge_sources: Box::new([]),
        advisory: AdvisoryPackageDto::unknown(),
    })
}

fn resolve_tree_subject(
    view: &ViewRoot,
    subject: TreeSubject,
    title: Option<ProductText>,
) -> Result<(TreeSubject, ProductText), String> {
    match subject {
        TreeSubject::Declaration(text) => {
            let coordinate = text.as_str();
            let row = view
                .rows()
                .iter()
                .find(|row| matches!(row.id, RowId::Symbol(_)) && row.label == coordinate)
                .ok_or_else(|| format!("declaration {coordinate} is not indexed"))?;
            let (name, path) = declaration_identity(row)?;
            Ok((
                TreeSubject::Declaration(
                    ProductText::new(row.label.clone()).map_err(|error| error.to_string())?,
                ),
                ProductText::new(format!("{name} · {path}"))
                    .map_err(|error| error.to_string())?,
            ))
        }
        other => Ok((
            other.clone(),
            title.unwrap_or_else(|| subject_title(&other)),
        )),
    }
}

fn catalog_page(
    catalog: &[RegistryPackageRecord],
    query: Option<&ProductText>,
    limit: u16,
) -> Box<[RegistryPackageRecord]> {
    let needle = query.map_or("", ProductText::as_str).to_ascii_lowercase();
    catalog
        .iter()
        .filter(|row| {
            needle.is_empty()
                || row
                    .coordinate
                    .as_str()
                    .to_ascii_lowercase()
                    .contains(&needle)
        })
        .take(usize::from(limit))
        .cloned()
        .collect::<Vec<_>>()
        .into_boxed_slice()
}
fn package_matches(package: &PackageReference, row: &RegistryPackageRecord) -> bool {
    &row.coordinate == package || row.name.as_str() == package.as_str()
}
fn packages(
    catalog: &[RegistryPackageRecord],
    package: &PackageReference,
) -> Box<[RegistryPackageRecord]> {
    catalog
        .iter()
        .filter(|row| package_matches(package, row))
        .cloned()
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn package_page(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    package: &PackageReference,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    let records = packages(catalog, package);
    if !records.is_empty() {
        return Ok(records);
    }
    let PackageReference::Purl(_) = package else {
        return Err(format!(
            "package {} is not recorded in the registry catalog",
            package.as_str()
        ));
    };
    for row in view.rows() {
        if !matches!(row.id, RowId::Package(_)) {
            continue;
        }
        let project_root = Path::new(&row.label);
        let (_, _, manifest) = super::local_manifest::cargo_package_identity(project_root)?;
        if &manifest == package {
            return Ok(Box::new([super::local_manifest::cargo_registry_record(
                project_root,
            )?]));
        }
    }
    Err(format!(
        "package {} does not match any indexed local manifest",
        package.as_str()
    ))
}
fn versions(
    catalog: &[RegistryPackageRecord],
    package: &PackageReference,
) -> Box<[RegistryPackageRecord]> {
    let mut rows = packages(catalog, package).into_vec();
    rows.sort_by(|a, b| b.version.cmp(&a.version));
    rows.into_boxed_slice()
}
fn profile(catalog: &[RegistryPackageRecord], package: &PackageReference) -> SurfaceReply {
    let rows = versions(catalog, package);
    SurfaceReply::PackageProfile {
        latest: rows.first().cloned(),
        versions: rows.len() as u64,
    }
}
fn dependencies(
    facts: &[PackageDependencySourceFacts],
    package: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    if let Some((_, value)) = facts
        .iter()
        .find(|(source, _)| source == package || source.as_str() == package.as_str())
    {
        return Ok(value.clone());
    }
    Ok(DependencyFacts::Unavailable(
        ProductText::new(format!(
            "dependency facts are unavailable for {} because the package is not recorded",
            package.as_str()
        ))
        .map_err(|error| error.to_string())?,
    ))
}

fn dependents(
    catalog: &[RegistryPackageRecord],
    facts: &[PackageDependencySourceFacts],
    package: &PackageReference,
) -> Result<RegistryMetadata<Box<[RegistryPackageRecord]>>, String> {
    if facts.is_empty() {
        return Ok(RegistryMetadata::NotRecorded(
            ProductText::new("the configured feed does not record dependency metadata")
                .map_err(|error| error.to_string())?,
        ));
    }
    let PackageReference::Purl(target) = package else {
        return Ok(RegistryMetadata::NotRecorded(
            ProductText::new("reverse dependency lookup requires a pinned package URL")
                .map_err(|error| error.to_string())?,
        ));
    };
    let ecosystem = target.package_type().registry();
    let mut sources = BTreeSet::new();
    let mut saw_unknown = None;
    for (source, value) in facts {
        match value {
            DependencyFacts::Known(rows) => {
                if rows.iter().any(|row| {
                    Some(row.target.ecosystem) == ecosystem
                        && row.target.name.as_str() == target.lineage_name()
                        && row
                            .target
                            .resolved
                            .as_ref()
                            .is_none_or(|resolved| resolved.as_str() == target.as_str())
                }) {
                    sources.insert(source.clone());
                }
            }
            DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
                saw_unknown.get_or_insert(reason.clone());
            }
        }
    }
    if sources.is_empty() {
        if let Some(reason) = saw_unknown {
            return Ok(RegistryMetadata::NotRecorded(reason));
        }
    }
    let mut records = catalog
        .iter()
        .filter(|record| sources.contains(&record.coordinate))
        .cloned()
        .collect::<Vec<_>>();
    for source in sources {
        if records.iter().any(|record| record.coordinate == source) {
            continue;
        }
        records.push(local_manifest_registry_record(&source)?);
    }
    Ok(RegistryMetadata::Recorded(records.into_boxed_slice()))
}

fn local_manifest_registry_record(
    source: &PackageReference,
) -> Result<RegistryPackageRecord, String> {
    let PackageReference::Purl(coordinate) = source else {
        return Err(format!(
            "local manifest dependents require a pinned package URL, not {}",
            source.as_str()
        ));
    };
    let ecosystem = coordinate.package_type().registry().ok_or_else(|| {
        format!(
            "local manifest dependents require a registry ecosystem for {}",
            source.as_str()
        )
    })?;
    let native_metadata = RegistryNativeMetadata::unavailable(ecosystem, "local manifest");
    Ok(RegistryPackageRecord {
        coordinate: source.clone(),
        ecosystem,
        name: ProductText::new(coordinate.lineage_name()).map_err(|error| error.to_string())?,
        version: ProductText::new(coordinate.version()).map_err(|error| error.to_string())?,
        bytes: 0,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
        facts_version: [0; 32],
        native_metadata_version: native_metadata
            .identity()
            .map_err(|error| error.to_string())?,
        native_metadata,
        forge_sources: Box::new([]),
        advisory: AdvisoryPackageDto::unknown(),
    })
}
fn subject_title(subject: &TreeSubject) -> ProductText {
    let text = match subject {
        TreeSubject::Package(v) => v.as_str(),
        TreeSubject::Declaration(v)
        | TreeSubject::Search(v)
        | TreeSubject::Owner(v)
        | TreeSubject::Explore(Some(v)) => v.as_str(),
        TreeSubject::Explore(None) => "Explore",
    };
    ProductText::new(text).unwrap_or_else(|_| ProductText::from_static("Untitled"))
}

fn validate_lockfile(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("lockfile path must be absolute".to_owned());
    }
    match path.file_name().and_then(|name| name.to_str()) {
        Some(
            "Cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "uv.lock"
            | "poetry.lock" | "requirements.txt" | "go.mod" | "pom.xml" | "packages.lock.json"
            | "conan.lock" | "vcpkg.json",
        ) => Ok(()),
        _ => Err("unsupported lockfile name".to_owned()),
    }
}

fn parse_lockfile(path: &Path) -> Result<Box<[PackageReference]>, String> {
    validate_lockfile(path)?;
    let text = fs::read_to_string(path).map_err(|error| format!("read lockfile: {error}"))?;
    if text.len() > 8 * 1024 * 1024 {
        return Err("lockfile exceeds byte bound".to_owned());
    }
    let mut rows = BTreeSet::new();
    let mut name = None;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches(['\"', '\'', ',']).to_owned());
        } else if let Some(value) = line.strip_prefix("version = ")
            && let Some(name) = name.take()
        {
            rows.insert(format!("{name}@{}", value.trim_matches(['\"', '\'', ','])));
        } else if let Some((name, version)) = line.split_once("==") {
            rows.insert(format!("{name}@{version}"));
        } else if let Some(value) = line.strip_prefix("require ")
            && let Some((name, version)) = value.split_once(char::is_whitespace)
        {
            rows.insert(format!("{name}@{version}"));
        }
    }
    if rows.len() > MAX_PROJECT_MEMBERS {
        return Err("lockfile exceeds project member bound".to_owned());
    }
    rows.into_iter()
        .map(PackageReference::parse)
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-product-state-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    fn view() -> ViewRoot {
        let root = backend_engine::view_state_root(&[]);
        let basis = backend_engine::Basis::new(root, backend_engine::object_version(b"source"));
        ViewRoot::new_incomplete(
            backend_engine::view_key(b"product-state-test"),
            basis,
            backend_engine::Frontier::new(basis.branch, basis.log, basis.schema, root, 0),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty view")
    }

    fn create(name: &str) -> SurfaceCommand {
        SurfaceCommand::ProjectCreate {
            name: ProjectName::new(name).expect("project name"),
            lockfile: None,
        }
    }

    #[test]
    fn committed_state_reopens_at_the_exact_epoch() {
        let root = fixture("reopen");
        let path = root.join("product-state.json");
        let mut state = ProductState::open(path.clone()).expect("open empty state");
        let reply = state
            .execute(create("Nudox"), &view(), &[], &[])
            .expect("create project");
        assert!(matches!(reply, SurfaceReply::ProjectCreated(_)));
        assert_eq!(state.state.epoch, 1);
        drop(state);

        let reopened = ProductState::open(path).expect("reopen committed state");
        assert_eq!(reopened.state.epoch, 1);
        assert_eq!(reopened.state.projects.len(), 1);
        assert_eq!(reopened.state.projects[0].name.as_str(), "Nudox");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_sibling_never_supersedes_the_canonical_state() {
        let root = fixture("interrupted");
        let path = root.join("product-state.json");
        let mut state = ProductState::open(path.clone()).expect("open empty state");
        state
            .execute(create("Canonical"), &view(), &[], &[])
            .expect("create project");
        fs::write(root.join(".product-state.json.9.9.tmp"), b"partial")
            .expect("interrupted sibling");

        let reopened = ProductState::open(path).expect("reopen canonical state");
        assert_eq!(reopened.state.epoch, 1);
        assert_eq!(reopened.state.projects[0].name.as_str(), "Canonical");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejected_bytes_remain_untouched_for_diagnosis() {
        let root = fixture("rejected");
        let path = root.join("product-state.json");
        let bytes = br#"{"version":99,"epoch":0}"#;
        fs::write(&path, bytes).expect("write incompatible state");
        assert!(ProductState::open(path.clone()).is_err());
        assert_eq!(fs::read(path).expect("read rejected bytes"), bytes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_publication_cannot_advance_memory() {
        let root = fixture("rollback");
        let destination = root.join("occupied");
        fs::create_dir(&destination).expect("occupied destination");
        let mut state = ProductState {
            path: destination,
            state: StoredState::default(),
        };
        let before = state.state.clone();
        assert!(
            state
                .execute(create("Unpublished"), &view(), &[], &[])
                .is_err()
        );
        assert_eq!(state.state, before);
        let _ = fs::remove_dir_all(root);
    }
}
