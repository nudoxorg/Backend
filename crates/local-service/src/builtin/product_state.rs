//! Durable typed owner for follows, projects, and the shared session tree.

use super::discovery_search::{
    DiscoveryPackageSearchGroup, DiscoverySearchCursor, DiscoverySearchIndex,
    DiscoverySearchRequest, DiscoverySearchSource, ForgeSearchDocument,
    ForgeSourcePinSearchDocument, ForgeSourcePinSearchKey, LineageKey, LineageSearchSource,
    LocalDeclarationSearchIndex, ReleaseMatchScope as DiscoveryReleaseMatchScope,
    SearchContinuation, SearchMatchEvidence, SearchPage, SearchResultCount,
};
use super::forge_gateway::{
    ForgePackageDetail, ForgePackagePin as ForgePackageViewPin, find_package_versions,
    project_package_details,
};
use crate::discovery::{DISCOVERY_FRESHNESS_MILLIS, DiscoveryStore};
use backend_engine::{
    ForgeFact, ForgeSearchRecord, PackageDependencySourceFacts, ProductTreeNodeId as TreeNodeId,
    RowId, SurfaceCommand, SurfaceReply, ViewRoot,
};
use backend_library::{
    AdvisoryPackageDto, CommandMutation, DeclarationRecord, DependencyFacts, DependentSources,
    ForgeDiscoveryCandidate, ForgeManifestRecord, ForgeRepositoryMetadataRecord, Fragment,
    IndexSearchCursor, IndexSearchPage, IndexSearchResultCount,
    PackageCoordinate as ProductPackageCoordinate, PackageDependencyLookup,
    PackageDependencyRecord, PackageGraphIndex, PackageGraphSourceAuthority, PackageGraphSourceKey,
    PackageReference, ProductText, ProjectId, ProjectName, ProjectRecord, ProjectSelector,
    RegistryDiscoveryCandidate, RegistryDiscoveryCompleteness, RegistryDiscoveryFreshness,
    RegistryDiscoveryMetadata, RegistryDiscoveryStanding, RegistryDownloadCount, RegistryEcosystem,
    RegistryEvidenceFacet, RegistryFactAvailability, RegistryMetadata, RegistryNativeMetadata,
    RegistryPackageRecord, RegistryPackageSearchGroup, RegistryReleaseMatchScope,
    RegistryReleaseStanding, RegistrySearchGroupKind, RegistrySearchHit, RegistrySearchRelease,
    ReleaseRecord, Row, SemanticVersionRecord, SubscriptionRecord, TreeNodeRecord, TreeOpener,
    TreeSubject, command_spec,
};
use backend_platform::durable;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

#[path = "product_state/catalog_search.rs"]
mod catalog_search;

const STATE_VERSION: u16 = 1;
const MAX_PROJECTS: usize = 256;
const MAX_PROJECT_MEMBERS: usize = 1024;
const MAX_SUBSCRIPTIONS: usize = 4096;
const MAX_TREE_NODES: usize = 2048;
const INDEX_SEARCH_CURSOR_VERSION: u8 = 4;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexSearchCursorState {
    version: u8,
    normalized_query: String,
    snapshot: [u8; 32],
    /// All pages produced from one cursor chain use one evaluation instant so
    /// freshness facets cannot change midway through pagination.
    evaluated_at_millis: u64,
    acquired: Option<SearchContinuation>,
    discovery: Option<DiscoverySearchCursor>,
    forge_source_pins: Option<SearchContinuation>,
    local: Option<SearchContinuation>,
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
    discovery_search: Option<DiscoverySearchIndex>,
    local_declaration_search: LocalDeclarationSearchIndex,
}

impl ProductState {
    pub(super) fn workspace_path(&self) -> Result<&Path, String> {
        self.path
            .parent()
            .ok_or_else(|| "product state path has no parent workspace".to_owned())
    }

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
        Ok(Self {
            path,
            state,
            discovery_search: None,
            local_declaration_search: LocalDeclarationSearchIndex::default(),
        })
    }

    pub(super) fn execute(
        &mut self,
        command: SurfaceCommand,
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        catalog_index: &CatalogLookupIndex,
        dependency_facts: &[PackageDependencySourceFacts],
        dependency_index: &PackageGraphIndex,
        workspace: Option<&Path>,
    ) -> Result<SurfaceReply, String> {
        self.execute_with_discovery(
            command,
            view,
            catalog,
            catalog_index,
            dependency_facts,
            dependency_index,
            workspace,
            None,
        )
    }

    pub(super) fn execute_with_discovery(
        &mut self,
        command: SurfaceCommand,
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        catalog_index: &CatalogLookupIndex,
        dependency_facts: &[PackageDependencySourceFacts],
        dependency_index: &PackageGraphIndex,
        workspace: Option<&Path>,
        discovery: Option<&DiscoveryStore>,
    ) -> Result<SurfaceReply, String> {
        self.execute_with_discovery_and_forge(
            command,
            view,
            catalog,
            catalog_index,
            dependency_facts,
            dependency_index,
            workspace,
            discovery,
            &[],
        )
    }

    pub(super) fn execute_with_discovery_and_forge(
        &mut self,
        command: SurfaceCommand,
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        catalog_index: &CatalogLookupIndex,
        dependency_facts: &[PackageDependencySourceFacts],
        dependency_index: &PackageGraphIndex,
        workspace: Option<&Path>,
        discovery: Option<&DiscoveryStore>,
        forge_records: &[ForgeSearchRecord],
    ) -> Result<SurfaceReply, String> {
        self.execute_with_discovery_and_forge_snapshot(
            command,
            view,
            catalog,
            catalog_index,
            dependency_facts,
            dependency_index,
            workspace,
            discovery,
            CatalogLookupIndex::snapshot_for_catalog(catalog),
            forge_records,
        )
    }

    pub(super) fn execute_with_discovery_and_forge_snapshot(
        &mut self,
        command: SurfaceCommand,
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        catalog_index: &CatalogLookupIndex,
        dependency_facts: &[PackageDependencySourceFacts],
        dependency_index: &PackageGraphIndex,
        workspace: Option<&Path>,
        discovery: Option<&DiscoveryStore>,
        selected_catalog_snapshot: [u8; 32],
        forge_records: &[ForgeSearchRecord],
    ) -> Result<SurfaceReply, String> {
        command.admit().map_err(|error| error.to_string())?;
        match command_spec(command.id()).mutation {
            CommandMutation::Read => {
                let (reply, changed) = self.execute_admitted(
                    command,
                    view,
                    catalog,
                    catalog_index,
                    dependency_facts,
                    dependency_index,
                    workspace,
                    discovery,
                    selected_catalog_snapshot,
                    forge_records,
                )?;
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
                    discovery_search: None,
                    local_declaration_search: LocalDeclarationSearchIndex::default(),
                };
                let (reply, changed) = pending.execute_admitted(
                    command,
                    view,
                    catalog,
                    catalog_index,
                    dependency_facts,
                    dependency_index,
                    workspace,
                    discovery,
                    selected_catalog_snapshot,
                    forge_records,
                )?;
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
        catalog_index: &CatalogLookupIndex,
        dependency_facts: &[PackageDependencySourceFacts],
        dependency_index: &PackageGraphIndex,
        workspace: Option<&Path>,
        discovery: Option<&DiscoveryStore>,
        selected_catalog_snapshot: [u8; 32],
        forge_records: &[ForgeSearchRecord],
    ) -> Result<(SurfaceReply, bool), String> {
        let (reply, changed) = match command {
            SurfaceCommand::Advisory {
                package,
                override_evidence,
            } => {
                let advisory = catalog_index
                    .first_coordinate(catalog, &package)?
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
                    catalog_index,
                    query.as_ref(),
                    limit,
                )?),
                false,
            ),
            SurfaceCommand::IndexSearch {
                query,
                limit,
                cursor,
            } => {
                let mut forge_documents = Vec::new();
                let mut forge_source_pin_documents = Vec::new();
                for record in forge_records {
                    forge_documents.extend(ForgeSearchDocument::from_search_record(record)?);
                    forge_source_pin_documents
                        .extend(ForgeSourcePinSearchDocument::from_search_record(record)?);
                }
                if discovery.is_some()
                    || !forge_documents.is_empty()
                    || !forge_source_pin_documents.is_empty()
                    || self.discovery_search.is_some()
                {
                    if self.discovery_search.is_none() {
                        self.discovery_search = Some(match discovery {
                            Some(store) => DiscoverySearchIndex::open_with_forge_and_source_pins(
                                store,
                                &forge_documents,
                                &forge_source_pin_documents,
                            )?,
                            None => DiscoverySearchIndex::open_forge_only_with_source_pins(
                                &forge_documents,
                                &forge_source_pin_documents,
                            )?,
                        });
                    }
                    let discovery_search = self
                        .discovery_search
                        .as_mut()
                        .ok_or_else(|| "discovery search index was not initialized".to_owned())?;
                    match discovery {
                        Some(store) => discovery_search.sync_with_forge_and_source_pins(
                            store,
                            &forge_documents,
                            &forge_source_pin_documents,
                        )?,
                        None => discovery_search.sync_forge_only_and_source_pins(
                            &forge_documents,
                            &forge_source_pin_documents,
                        )?,
                    }
                }
                self.local_declaration_search.sync(view)?;
                let page = index_search_page_with_discovery_snapshot(
                    view,
                    catalog,
                    catalog_index,
                    &query,
                    limit,
                    cursor.as_ref(),
                    discovery,
                    self.discovery_search.as_ref(),
                    &self.local_declaration_search,
                    selected_catalog_snapshot,
                    forge_records,
                )?;
                (SurfaceReply::IndexSearchPage(page), false)
            }
            SurfaceCommand::Package { package } => {
                let forge_details = match &package {
                    PackageReference::Purl(coordinate) => {
                        find_package_versions(forge_records, coordinate)
                            .map_err(|error| format!("find forge package version: {error:?}"))?
                            .iter()
                            .map(product_forge_package_detail)
                            .collect::<Result<Vec<_>, _>>()?
                    }
                    PackageReference::Local(_) => Vec::new(),
                };
                if forge_details.is_empty() {
                    (
                        SurfaceReply::Package(package_page(
                            view,
                            catalog,
                            catalog_index,
                            &package,
                        )?),
                        false,
                    )
                } else {
                    let registry = packages(catalog, catalog_index, &package)?;
                    (
                        SurfaceReply::PackageDetails {
                            registry,
                            forge: forge_details.into_boxed_slice(),
                        },
                        false,
                    )
                }
            }
            SurfaceCommand::ForgeAdd { .. } | SurfaceCommand::ForgeReference { .. } => {
                return Err(
                    "forge acquisition authority is not configured in this owner".to_owned(),
                );
            }
            SurfaceCommand::Dependencies { package } => (
                SurfaceReply::Dependencies(dependencies(
                    dependency_facts,
                    dependency_index,
                    &package,
                )?),
                false,
            ),
            SurfaceCommand::PackageVersions { package } => (
                SurfaceReply::PackageVersions(
                    package_versions(view, catalog, catalog_index, &package, workspace)?.rows,
                ),
                false,
            ),
            SurfaceCommand::SemanticVersions { .. }
            | SurfaceCommand::SelectSemanticVersion { .. } => {
                return Err(
                    "semantic version history requires compiler publication authority".to_owned(),
                );
            }
            SurfaceCommand::PackageProfile { package } => (
                profile(view, catalog, catalog_index, &package, workspace)?,
                false,
            ),
            SurfaceCommand::Dependents { package } => (
                SurfaceReply::Dependents(dependents(
                    catalog,
                    catalog_index,
                    dependency_facts,
                    dependency_index,
                    &package,
                )?),
                false,
            ),
            SurfaceCommand::Owner { owner } => {
                let workspace = self.workspace_path()?;
                (
                    SurfaceReply::Owner(owner_page(
                        view,
                        catalog,
                        catalog_index,
                        workspace,
                        &owner,
                    )?),
                    false,
                )
            }
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
                SurfaceReply::Releases(self.releases(
                    view,
                    catalog,
                    catalog_index,
                    mark_seen,
                    workspace,
                )?),
                mark_seen,
            ),
            SurfaceCommand::Projects => (
                SurfaceReply::Projects(enrich_projects(&self.state.projects, view, workspace)?),
                false,
            ),
            SurfaceCommand::ProjectCreate { name, lockfile } => (
                SurfaceReply::ProjectCreated(enrich_project(
                    self.create_project(name, lockfile)?,
                    view,
                    workspace,
                )?),
                true,
            ),
            SurfaceCommand::ProjectDelete { project } => (
                SurfaceReply::ProjectDeleted(self.delete_project(&project)?),
                true,
            ),
            SurfaceCommand::ProjectAdd { project, package } => (
                SurfaceReply::ProjectAdded(enrich_project(
                    self.change_member(&project, package, true)?,
                    view,
                    workspace,
                )?),
                true,
            ),
            SurfaceCommand::ProjectRemove { project, package } => (
                SurfaceReply::ProjectRemoved(enrich_project(
                    self.change_member(&project, package, false)?,
                    view,
                    workspace,
                )?),
                true,
            ),
            SurfaceCommand::ProjectSync { project } => (
                SurfaceReply::ProjectSynced(enrich_project(
                    self.sync_project(&project)?,
                    view,
                    workspace,
                )?),
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
            // The command adapter answers these before product state is asked.
            SurfaceCommand::ProjectTree { .. } | SurfaceCommand::AdvisoryRefresh => {
                return Err("the command adapter owns project-tree and advisory-refresh".to_owned());
            }
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
        view: &ViewRoot,
        catalog: &[RegistryPackageRecord],
        catalog_index: &CatalogLookupIndex,
        mark_seen: bool,
        workspace: Option<&Path>,
    ) -> Result<Box<[ReleaseRecord]>, String> {
        let mut result = Vec::new();
        for subscription in &mut self.state.subscriptions {
            let indexed = indexed_package_records(view, &subscription.package, workspace)?;
            let mut matches = if indexed.is_empty() {
                catalog_index
                    .records_for(catalog, &subscription.package, false)?
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                indexed
            };
            sort_registry_versions(&mut matches);
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
        Ok(result.into_boxed_slice())
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
            member_manifest_names: Box::new([]),
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

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read(view: &ViewRoot, locators: &[ProductText]) -> Result<Box<[DeclarationRecord]>, String> {
    locators
        .iter()
        .map(|locator| {
            let row = view.row_by_label(locator.as_str());
            Ok(DeclarationRecord {
                label: locator.clone(),
                stable_id: row.map(|value| match value.id {
                    RowId::Package(id) => id.to_bytes(),
                    RowId::Symbol(id) => id.to_bytes(),
                    RowId::Object(id) => id.to_bytes(),
                }),
                signature: row
                    .and_then(|value| value.excerpt.text().or(value.signature.as_deref()))
                    .map(ProductText::new)
                    .transpose()
                    .map_err(|e| e.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Vec::into_boxed_slice)
}

fn owner_page(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    workspace: &Path,
    query: &ProductText,
) -> Result<RegistryMetadata<Box<[RegistryPackageRecord]>>, String> {
    let needle = query.as_str();
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    for row in view.row_refs() {
        if !matches!(row.id, RowId::Symbol(_)) {
            continue;
        }
        let (name, path) = declaration_identity(row)?;
        if name != needle {
            continue;
        }
        let project_root = row
            .label
            .split_once("::")
            .map(|(project, _)| project)
            .ok_or_else(|| format!("declaration row {} has no owning project", row.label))?;
        let source_root =
            super::local_manifest::indexed_package_source_root(project_root, workspace)?;
        let package_record = super::local_manifest::local_registry_record(&source_root)?;
        if !seen.insert(package_record.coordinate.as_str().to_owned()) {
            continue;
        }
        let file = RegistryPackageRecord {
            coordinate: PackageReference::Local(
                ProductText::new(format!("{project_root}::{path}"))
                    .map_err(|error| error.to_string())?,
            ),
            ecosystem: package_record.ecosystem,
            name: ProductText::new(path.clone()).map_err(|error| error.to_string())?,
            version: ProductText::new(path).map_err(|error| error.to_string())?,
            bytes: 0,
            standing: RegistryReleaseStanding::Available,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
            facts_version: [0; 32],
            authority: None,
            native_metadata_version: package_record.native_metadata_version,
            native_metadata: package_record.native_metadata.clone(),
            forge_sources: Box::new([]),
            advisory: AdvisoryPackageDto::unknown(),
        };
        records.push(package_record);
        if seen.insert(file.coordinate.as_str().to_owned()) {
            records.push(file);
        }
    }
    if records.is_empty() {
        let registry = catalog_index
            .records_named(catalog, needle)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if registry.is_empty() {
            return Ok(RegistryMetadata::NotRecorded(
                ProductText::new(format!(
                    "no indexed declaration or registry publisher named {}",
                    needle
                ))
                .map_err(|error| error.to_string())?,
            ));
        }
        return Ok(RegistryMetadata::Recorded(registry.into_boxed_slice()));
    }
    Ok(RegistryMetadata::Recorded(records.into_boxed_slice()))
}

fn explore_page(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: Option<&ProductText>,
    limit: u16,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    if let Some(query_text) = query
        && let Some(project_root) = indexed_project_for_query(view, query_text.as_str())
    {
        return indexed_explore_page(view, &project_root, query_text.as_str(), limit);
    }
    let registry = catalog_page(catalog, catalog_index, query, limit)?;
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

fn package_label(view: &ViewRoot, label: &str) -> Option<String> {
    match view.row_by_label(label) {
        Some(row) if matches!(row.id, RowId::Package(_)) => Some(row.label.clone()),
        Some(_) => view.row_refs().find_map(|row| {
            (matches!(row.id, RowId::Package(_)) && row.label == label).then(|| row.label.clone())
        }),
        None => None,
    }
}

fn symbol_row<'a>(view: &'a ViewRoot, label: &str) -> Option<&'a Row> {
    match view.row_by_label(label) {
        Some(row) if matches!(row.id, RowId::Symbol(_)) => Some(row),
        Some(_) => view
            .row_refs()
            .find(|row| matches!(row.id, RowId::Symbol(_)) && row.label == label),
        None => None,
    }
}

fn indexed_project_for_query(view: &ViewRoot, query: &str) -> Option<String> {
    if let Some(label) = package_label(view, query) {
        return Some(label);
    }
    view.row_refs().find_map(|row| {
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
    for row in view.row_refs() {
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
        authority: None,
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
            let row = symbol_row(view, coordinate)
                .ok_or_else(|| format!("declaration {coordinate} is not indexed"))?;
            let (name, path) = declaration_identity(row)?;
            Ok((
                TreeSubject::Declaration(
                    ProductText::new(row.label.clone()).map_err(|error| error.to_string())?,
                ),
                ProductText::new(format!("{name} · {path}")).map_err(|error| error.to_string())?,
            ))
        }
        other => Ok((
            other.clone(),
            title.unwrap_or_else(|| subject_title(&other)),
        )),
    }
}

fn index_search_page(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: Option<&ProductText>,
    limit: u16,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    let mut records = catalog_page(catalog, catalog_index, query, limit)?.into_vec();
    let needle = query.map_or("", ProductText::as_str);
    for row in view.row_refs() {
        if !matches!(row.id, RowId::Symbol(_)) || !row_matches_index_query(row, needle) {
            continue;
        }
        records.push(declaration_explore_record(row)?);
        if records.len() >= usize::from(limit) {
            break;
        }
    }
    Ok(records.into_boxed_slice())
}

fn index_search_page_with_discovery(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: &ProductText,
    limit: u16,
    cursor: Option<&IndexSearchCursor>,
    discovery: Option<&DiscoveryStore>,
    discovery_search: Option<&DiscoverySearchIndex>,
    local_search: &LocalDeclarationSearchIndex,
    forge_records: &[ForgeSearchRecord],
) -> Result<IndexSearchPage, String> {
    index_search_page_with_discovery_snapshot(
        view,
        catalog,
        catalog_index,
        query,
        limit,
        cursor,
        discovery,
        discovery_search,
        local_search,
        CatalogLookupIndex::snapshot_for_catalog(catalog),
        forge_records,
    )
}

fn index_search_page_with_discovery_snapshot(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: &ProductText,
    limit: u16,
    cursor: Option<&IndexSearchCursor>,
    discovery: Option<&DiscoveryStore>,
    discovery_search: Option<&DiscoverySearchIndex>,
    local_search: &LocalDeclarationSearchIndex,
    selected_catalog_snapshot: [u8; 32],
    forge_records: &[ForgeSearchRecord],
) -> Result<IndexSearchPage, String> {
    let mut work = IndexSearchWork::default();
    let page = index_search_page_with_discovery_measured_snapshot(
        view,
        catalog,
        catalog_index,
        query,
        limit,
        cursor,
        discovery,
        discovery_search,
        local_search,
        selected_catalog_snapshot,
        forge_records,
        &mut work,
    )?;
    let _measured_postings = work.total_postings();
    Ok(page)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct IndexSearchWork {
    acquired_lineage_postings: usize,
    acquired_facet_postings: usize,
    discovery_postings: usize,
    discovery_documents_visited: usize,
    discovery_facet_releases_examined: usize,
    forge_source_pin_postings: usize,
    local_postings: usize,
}

impl IndexSearchWork {
    fn total_postings(self) -> usize {
        self.acquired_lineage_postings
            .saturating_add(self.acquired_facet_postings)
            .saturating_add(self.discovery_postings)
            .saturating_add(self.discovery_documents_visited)
            .saturating_add(self.discovery_facet_releases_examined)
            .saturating_add(self.forge_source_pin_postings)
            .saturating_add(self.local_postings)
    }
}

fn index_search_page_with_discovery_measured(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: &ProductText,
    limit: u16,
    cursor: Option<&IndexSearchCursor>,
    discovery: Option<&DiscoveryStore>,
    discovery_search: Option<&DiscoverySearchIndex>,
    local_search: &LocalDeclarationSearchIndex,
    forge_records: &[ForgeSearchRecord],
    work: &mut IndexSearchWork,
) -> Result<IndexSearchPage, String> {
    index_search_page_with_discovery_measured_snapshot(
        view,
        catalog,
        catalog_index,
        query,
        limit,
        cursor,
        discovery,
        discovery_search,
        local_search,
        CatalogLookupIndex::snapshot_for_catalog(catalog),
        forge_records,
        work,
    )
}

fn index_search_page_with_discovery_measured_snapshot(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: &ProductText,
    limit: u16,
    cursor: Option<&IndexSearchCursor>,
    discovery: Option<&DiscoveryStore>,
    discovery_search: Option<&DiscoverySearchIndex>,
    local_search: &LocalDeclarationSearchIndex,
    selected_catalog_snapshot: [u8; 32],
    forge_records: &[ForgeSearchRecord],
    work: &mut IndexSearchWork,
) -> Result<IndexSearchPage, String> {
    *work = IndexSearchWork::default();
    let snapshot = index_search_snapshot(
        view,
        catalog_index,
        selected_catalog_snapshot,
        discovery,
        discovery_search,
    );
    let normalized_query = unicode_lowercase(query.as_str());
    let mut cursor_state = match cursor {
        Some(cursor) => {
            let bytes = URL_SAFE_NO_PAD
                .decode(cursor.as_str())
                .map_err(|_| "index search cursor is malformed".to_owned())?;
            if bytes.len() > 48 * 1024 {
                return Err("index search cursor exceeds its encoded bound".to_owned());
            }
            let state: IndexSearchCursorState = serde_json::from_slice(&bytes)
                .map_err(|_| "index search cursor is malformed".to_owned())?;
            if state.version != INDEX_SEARCH_CURSOR_VERSION
                || state.normalized_query != normalized_query
                || state.snapshot != snapshot
                || state.evaluated_at_millis > current_epoch_millis()
                || state
                    .acquired
                    .as_ref()
                    .is_some_and(|cursor| cursor.admit().is_err())
                || state
                    .discovery
                    .as_ref()
                    .is_some_and(|cursor| cursor.admit().is_err())
                || state
                    .forge_source_pins
                    .as_ref()
                    .is_some_and(|cursor| cursor.admit().is_err())
                || state
                    .local
                    .as_ref()
                    .is_some_and(|cursor| cursor.admit().is_err())
            {
                return Err(
                    "index search cursor does not match this query or selected snapshot".to_owned(),
                );
            }
            state
        }
        None => IndexSearchCursorState {
            version: INDEX_SEARCH_CURSOR_VERSION,
            normalized_query: normalized_query.clone(),
            snapshot,
            evaluated_at_millis: current_epoch_millis(),
            acquired: None,
            discovery: None,
            forge_source_pins: None,
            local: None,
        },
    };
    let evaluated_at_millis = cursor_state.evaluated_at_millis;

    let limit = usize::from(limit);
    if limit == 0 {
        return Ok(IndexSearchPage {
            snapshot,
            evaluated_at_millis,
            hits: Box::new([]),
            next_cursor: None,
            result_count: IndexSearchResultCount::Exact(0),
        });
    }
    // Each plane returns its top `limit` groups and an exact continuation.
    // That is sufficient for a global top-`limit` merge: a candidate after a
    // plane's first `limit` cannot outrank all of that plane's earlier keys.
    // The cursor stored below advances only through groups actually selected
    // by the global merge.
    let acquired_page = catalog_index.lineage_page_after(
        catalog,
        query.as_str(),
        limit,
        cursor_state.acquired.as_ref(),
    )?;
    work.acquired_lineage_postings = acquired_page.posting_candidates;
    let request = DiscoverySearchRequest {
        text: query.as_str(),
        ecosystem: None,
    };
    let discovery_page = match discovery_search {
        Some(index) => {
            Some(index.search_groups_after(request, limit, cursor_state.discovery.as_ref())?)
        }
        None => None,
    };
    if let Some(page) = &discovery_page {
        work.discovery_postings = page.posting_candidates;
        work.discovery_documents_visited = page.index_documents_visited;
        work.discovery_facet_releases_examined = page.facet_releases_examined;
    }
    let forge_source_pin_page = discovery_search
        .map(|index| {
            index.source_pin_page_after(
                query.as_str(),
                limit,
                cursor_state.forge_source_pins.as_ref(),
            )
        })
        .transpose()?;
    work.forge_source_pin_postings = forge_source_pin_page
        .as_ref()
        .map_or(0, |page| page.posting_candidates);
    let local_page = local_search.page_after(query.as_str(), limit, cursor_state.local.as_ref())?;
    work.local_postings = local_page.posting_candidates;
    let mut ranked = Vec::<MergedPageSearchCandidate>::new();
    for search_hit in &acquired_page.hits {
        let key = &search_hit.key;
        let (releases, more_releases, facet_postings, release_match_scope) =
            catalog_index.matching_lineage_releases(catalog, key, query.as_str())?;
        work.acquired_facet_postings = work.acquired_facet_postings.saturating_add(facet_postings);
        let first_record = releases
            .first()
            .ok_or_else(|| "acquired lineage posting has no matching release facet".to_owned())?;
        let lineage = match &first_record.coordinate {
            PackageReference::Purl(coordinate) => {
                registry_search_lineage(coordinate, first_record.ecosystem)
            }
            PackageReference::Local(_) => {
                return Err("acquired lineage contains a local package coordinate".to_owned());
            }
        };
        let source = match &key.source {
            LineageSearchSource::Acquired { source, .. } => source.unwrap_or([0; 32]),
            LineageSearchSource::Discovery(_) => {
                return Err("acquired lineage has a discovery source key".to_owned());
            }
        };
        let group = RegistryPackageSearchGroup {
            kind: RegistrySearchGroupKind::Acquired,
            source,
            ecosystem: key.ecosystem,
            lineage: ProductText::new(lineage).map_err(|error| error.to_string())?,
            releases: releases
                .into_iter()
                .map(RegistrySearchRelease::Acquired)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            release_match_scope,
            more_releases,
        };
        group
            .admit()
            .map_err(|error| format!("acquired package group failed admission: {error:?}"))?;
        ranked.push(MergedPageSearchCandidate {
            candidate: PageSearchCandidate {
                evidence: search_hit.evidence,
                hit: RegistrySearchHit::PackageGroup(group),
                plane: SearchPlane::Acquired,
            },
            page_order: super::discovery_search::lineage_sort_key(key),
            continuation: SearchPlaneContinuation::Acquired(search_hit.continuation_after.clone()),
        });
    }
    let mut forge_records_by_coordinate = BTreeMap::new();
    for record in forge_records {
        forge_records_by_coordinate
            .entry(record.coordinate.clone())
            .or_insert(record);
    }
    let forge_records_by_pin_source = forge_records
        .iter()
        .map(|record| {
            (
                (record.coordinate.clone(), record.resolution.commit.clone()),
                record,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(page) = discovery_page.as_ref() {
        for group in &page.groups {
            let Some((product_group, evidence, _tie)) = product_discovery_group(
                group,
                discovery,
                &forge_records_by_coordinate,
                evaluated_at_millis,
            )?
            else {
                return Err("discovery lineage has no hydratable source-backed releases".to_owned());
            };
            ranked.push(MergedPageSearchCandidate {
                candidate: PageSearchCandidate {
                    evidence,
                    hit: RegistrySearchHit::PackageGroup(product_group),
                    plane: SearchPlane::Discovered,
                },
                page_order: super::discovery_search::lineage_sort_key(&group.key),
                continuation: SearchPlaneContinuation::Discovered(group.continuation_after.clone()),
            });
        }
    }
    let mut forge_details_by_pin = BTreeMap::new();
    if let Some(page) = forge_source_pin_page.as_ref() {
        for search_hit in &page.hits {
            let key = &search_hit.key;
            let detail_key = (
                key.source.clone(),
                key.ecosystem,
                key.manifest_path.clone(),
                key.resolved_commit.clone(),
            );
            if !forge_details_by_pin.contains_key(&detail_key) {
                let record = forge_records_by_pin_source
                    .get(&(key.source.clone(), key.resolved_commit.clone()))
                    .ok_or_else(|| {
                        "forge source-pin posting has no matching source record".to_owned()
                    })?;
                for detail in project_package_details(std::slice::from_ref(*record))
                    .map_err(|error| format!("project forge package details: {error:?}"))?
                {
                    if matches!(&detail.pin, ForgePackageViewPin::PinnedRevision { .. }) {
                        forge_details_by_pin.insert(
                            (
                                detail.source.clone(),
                                detail.manifest.ecosystem,
                                detail.manifest.path.to_string(),
                                detail.resolution.commit.clone(),
                            ),
                            detail,
                        );
                    }
                }
            }
            let detail = forge_details_by_pin.get(&detail_key).ok_or_else(|| {
                "forge source-pin posting has no matching authoritative manifest".to_owned()
            })?;
            let detail = product_forge_package_detail(detail)?;
            ranked.push(MergedPageSearchCandidate {
                candidate: PageSearchCandidate {
                    evidence: search_hit.evidence,
                    hit: RegistrySearchHit::ForgeSourcePin(detail),
                    plane: SearchPlane::ForgeSourcePin,
                },
                page_order: super::discovery_search::forge_source_pin_sort_key(key),
                continuation: SearchPlaneContinuation::ForgeSourcePin(
                    search_hit.continuation_after.clone(),
                ),
            });
        }
    }
    for search_hit in &local_page.hits {
        let row = view.row_ref(search_hit.key).ok_or_else(|| {
            "local search page refers to a row outside its view snapshot".to_owned()
        })?;
        if !matches!(row.id, RowId::Symbol(_)) {
            return Err("local search page contains a non-declaration row".to_owned());
        }
        ranked.push(MergedPageSearchCandidate {
            candidate: PageSearchCandidate {
                evidence: search_hit.evidence,
                hit: RegistrySearchHit::LocalDeclaration(declaration_explore_record(row)?),
                plane: SearchPlane::LocalDeclaration,
            },
            page_order: format!(
                "{}\u{1f}{}",
                unicode_lowercase(row.label.as_str()),
                search_hit.key.stable_key()
            ),
            continuation: SearchPlaneContinuation::Local(search_hit.continuation_after.clone()),
        });
    }
    ranked.sort_by(|left, right| {
        left.candidate
            .evidence
            .rank()
            .cmp(&right.candidate.evidence.rank())
            .then_with(|| left.page_order.cmp(&right.page_order))
            .then_with(|| left.candidate.plane.cmp(&right.candidate.plane))
    });

    let has_more = ranked.len() > limit
        || acquired_page.next_cursor.is_some()
        || discovery_page
            .as_ref()
            .is_some_and(|page| page.next_cursor.is_some())
        || forge_source_pin_page
            .as_ref()
            .is_some_and(|page| page.next_cursor.is_some())
        || local_page.next_cursor.is_some();
    ranked.truncate(limit);
    for candidate in &ranked {
        match &candidate.continuation {
            SearchPlaneContinuation::Acquired(continuation) => {
                cursor_state.acquired = Some(continuation.clone());
            }
            SearchPlaneContinuation::Discovered(continuation) => {
                cursor_state.discovery = Some(continuation.clone());
            }
            SearchPlaneContinuation::ForgeSourcePin(continuation) => {
                cursor_state.forge_source_pins = Some(continuation.clone());
            }
            SearchPlaneContinuation::Local(continuation) => {
                cursor_state.local = Some(continuation.clone());
            }
        }
    }

    let result_count = if has_more {
        IndexSearchResultCount::AtLeast(to_count(limit.saturating_add(1)))
    } else {
        match (
            acquired_page.result_count,
            discovery_page
                .as_ref()
                .map_or(SearchResultCount::Exact(0), |page| page.result_count),
            forge_source_pin_page
                .as_ref()
                .map_or(SearchResultCount::Exact(0), |page| page.result_count),
            local_page.result_count,
        ) {
            (
                SearchResultCount::Exact(acquired),
                SearchResultCount::Exact(discovered),
                SearchResultCount::Exact(forge_source_pins),
                SearchResultCount::Exact(local),
            ) => IndexSearchResultCount::Exact(to_count(
                acquired
                    .saturating_add(discovered)
                    .saturating_add(forge_source_pins)
                    .saturating_add(local),
            )),
            _ => IndexSearchResultCount::Unknown,
        }
    };
    let next_cursor = if has_more {
        let bytes = serde_json::to_vec(&cursor_state)
            .map_err(|error| format!("encode index search cursor: {error}"))?;
        Some(
            IndexSearchCursor::new(URL_SAFE_NO_PAD.encode(bytes)).map_err(|error| {
                format!("index search cursor exceeds product bounds: {error:?}")
            })?,
        )
    } else {
        None
    };
    Ok(IndexSearchPage {
        snapshot,
        evaluated_at_millis,
        hits: ranked
            .into_iter()
            .map(|candidate| candidate.candidate.hit)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        next_cursor,
        result_count,
    })
}

fn index_search_snapshot(
    view: &ViewRoot,
    catalog_index: &CatalogLookupIndex,
    selected_catalog_snapshot: [u8; 32],
    discovery: Option<&DiscoveryStore>,
    discovery_search: Option<&DiscoverySearchIndex>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.index-search.snapshot.v2\0");
    hasher.update(&catalog_index.snapshot());
    hasher.update(&selected_catalog_snapshot);
    hasher.update(view.root().as_bytes());
    if let Some(discovery) = discovery {
        hasher.update(&discovery.search_revision().to_le_bytes());
        hasher.update(&discovery.observation_revision().to_le_bytes());
    } else {
        hasher.update(&[0; 16]);
    }
    if let Some(index) = discovery_search {
        hasher.update(&index.snapshot_root());
    } else {
        hasher.update(&[0; 32]);
    }
    *hasher.finalize().as_bytes()
}

fn to_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

struct PageSearchCandidate {
    evidence: SearchMatchEvidence,
    hit: RegistrySearchHit,
    plane: SearchPlane,
}

enum SearchPlaneContinuation {
    Acquired(SearchContinuation),
    Discovered(DiscoverySearchCursor),
    ForgeSourcePin(SearchContinuation),
    Local(SearchContinuation),
}

struct MergedPageSearchCandidate {
    candidate: PageSearchCandidate,
    /// Plane-local stable ordering key. This is exactly the key used by that
    /// plane's Tantivy pagination so a merge checkpoint never skips a hit.
    page_order: String,
    continuation: SearchPlaneContinuation,
}

fn registry_search_lineage(
    coordinate: &ProductPackageCoordinate,
    ecosystem: RegistryEcosystem,
) -> String {
    let path = coordinate.lineage_name();
    if matches!(ecosystem, RegistryEcosystem::Maven | RegistryEcosystem::Cpp) {
        path.rsplit_once('/').map_or_else(
            || path.to_owned(),
            |(namespace, name)| format!("{namespace}:{name}"),
        )
    } else {
        path.to_owned()
    }
}

fn product_discovery_group(
    group: &DiscoveryPackageSearchGroup,
    discovery: Option<&DiscoveryStore>,
    forge_records: &BTreeMap<backend_engine::ForgeCoordinate, &ForgeSearchRecord>,
    now: u64,
) -> Result<
    Option<(
        RegistryPackageSearchGroup,
        SearchMatchEvidence,
        SearchStableTie,
    )>,
    String,
> {
    let mut releases = Vec::with_capacity(group.matched_releases.len());
    let mut first_tie = None;
    let kind = match &group.source {
        DiscoverySearchSource::Registry(source) => {
            let Some(store) = discovery else {
                return Ok(None);
            };
            for key in &group.matched_releases {
                let Some(fact) = store.fact(*source, key.coordinate.as_str()) else {
                    continue;
                };
                let candidate = registry_discovery_candidate(store, *source, fact, now)?;
                if first_tie.is_none() {
                    first_tie = Some(SearchStableTie::discovered(
                        candidate.coordinate.as_str(),
                        candidate.source,
                        candidate.proof,
                    ));
                }
                releases.push(RegistrySearchRelease::Discovered(candidate));
            }
            RegistrySearchGroupKind::Discovered
        }
        DiscoverySearchSource::Forge(source) => {
            for key in &group.matched_releases {
                let Some(record) = forge_records.get(&source.coordinate) else {
                    continue;
                };
                let Some(candidate) = forge_discovery_candidate(
                    record,
                    key.manifest_path.as_deref(),
                    key.coordinate.as_str(),
                    source.coordinate.identity(),
                )?
                else {
                    continue;
                };
                if first_tie.is_none() {
                    first_tie = Some(SearchStableTie::forge(
                        candidate.coordinate.as_str(),
                        candidate.source,
                        candidate.forge_coordinate.as_str(),
                        candidate.manifest.path.as_str(),
                    ));
                }
                releases.push(RegistrySearchRelease::ForgeDiscovered(candidate));
            }
            RegistrySearchGroupKind::Forge
        }
    };
    let Some(tie) = first_tie else {
        return Ok(None);
    };
    let lineage = match releases.first() {
        Some(RegistrySearchRelease::Acquired(_)) => unreachable!(),
        Some(RegistrySearchRelease::Discovered(candidate)) => {
            registry_search_lineage(&candidate.coordinate, group.ecosystem)
        }
        Some(RegistrySearchRelease::ForgeDiscovered(candidate)) => {
            registry_search_lineage(&candidate.coordinate, group.ecosystem)
        }
        None => return Ok(None),
    };
    let product_group = RegistryPackageSearchGroup {
        kind,
        source: group.source.id(),
        ecosystem: group.ecosystem,
        lineage: ProductText::new(lineage).map_err(|error| error.to_string())?,
        releases: releases.into_boxed_slice(),
        release_match_scope: match group.release_match_scope {
            DiscoveryReleaseMatchScope::ReleaseMatches => RegistryReleaseMatchScope::ReleaseMatches,
            DiscoveryReleaseMatchScope::LineageMetadataOnly => {
                RegistryReleaseMatchScope::LineageMetadataOnly
            }
        },
        more_releases: group.more_releases,
    };
    product_group
        .admit()
        .map_err(|error| format!("discovery package group failed admission: {error:?}"))?;
    Ok(Some((product_group, group.evidence, tie)))
}

fn product_forge_package_detail(
    detail: &ForgePackageDetail,
) -> Result<backend_library::ForgePackageDetailRecord, String> {
    fn fact<T, U>(
        value: &ForgeFact<T>,
        map: impl FnOnce(&T) -> Result<U, String>,
    ) -> Result<backend_library::ForgeFact<U>, String> {
        match value {
            ForgeFact::Recorded(value) => map(value).map(backend_library::ForgeFact::Recorded),
            ForgeFact::Unavailable(reason) => ProductText::new(reason.as_str().to_owned())
                .map(backend_library::ForgeFact::Unavailable)
                .map_err(|error| error.to_string()),
        }
    }

    let manifest = backend_library::ForgePackageManifestDetail {
        path: ProductText::new(detail.manifest.path.to_string())
            .map_err(|error| error.to_string())?,
        ecosystem: detail.manifest.ecosystem,
        name: fact(&detail.manifest.name, |value| Ok(value.clone()))?,
        version: fact(&detail.manifest.version, |value| Ok(value.clone()))?,
        dependencies: detail.manifest.dependencies.clone(),
    };
    let metadata = backend_library::ForgeRepositoryMetadataRecord {
        owner: fact(&detail.repository_metadata.owner, |value| Ok(value.clone()))?,
        description: fact(&detail.repository_metadata.description, |value| {
            Ok(value.clone())
        })?,
        license: fact(&detail.repository_metadata.license, |value| {
            Ok(value.clone())
        })?,
        readme: fact(
            &detail.repository_metadata.readme,
            |value| Ok(value.clone()),
        )?,
        topics: fact(
            &detail.repository_metadata.topics,
            |value| Ok(value.clone()),
        )?,
        stars: fact(&detail.repository_metadata.stars, |value| Ok(*value))?,
        forks: fact(&detail.repository_metadata.forks, |value| Ok(*value))?,
    };
    let pin = match &detail.pin {
        ForgePackageViewPin::PackageVersion { coordinate } => {
            backend_library::ForgePackagePin::PackageVersion {
                coordinate: coordinate.clone(),
            }
        }
        ForgePackageViewPin::PinnedRevision {
            requested_revision,
            resolved_commit,
        } => backend_library::ForgePackagePin::PinnedRevision {
            requested_revision: requested_revision.clone(),
            resolved_commit: resolved_commit.clone(),
        },
    };
    Ok(backend_library::ForgePackageDetailRecord {
        source: detail.source.clone(),
        source_id: detail.source.identity(),
        resolved_commit: detail.resolution.commit.clone(),
        resolved_tree: detail.resolution.tree.clone(),
        package_coordinate: detail.package_coordinate.clone(),
        pin,
        manifest,
        metadata,
        registry: backend_library::ForgePackageRegistryEvidence {
            yanked: detail.registry.yanked.clone(),
            downloads: detail.registry.downloads.clone(),
            advisory: detail.registry.advisory.clone(),
        },
    })
}

fn registry_discovery_candidate(
    store: &DiscoveryStore,
    source: backend_engine::registry::DiscoverySourceIdentity,
    fact: &backend_engine::registry::DiscoveryFact,
    now: u64,
) -> Result<RegistryDiscoveryCandidate, String> {
    let observed_at = fact.observed_at.as_unix_millis();
    let valid_until = observed_at.saturating_add(DISCOVERY_FRESHNESS_MILLIS);
    let completeness = match store.completeness(source) {
        Some(backend_engine::registry::DiscoveryCompleteness::CompleteThroughCursor) => {
            RegistryDiscoveryCompleteness::CompleteThroughCursor
        }
        Some(backend_engine::registry::DiscoveryCompleteness::Windowed) => {
            RegistryDiscoveryCompleteness::Windowed
        }
        Some(backend_engine::registry::DiscoveryCompleteness::Unsupported) => {
            RegistryDiscoveryCompleteness::Unsupported
        }
        Some(backend_engine::registry::DiscoveryCompleteness::Incomplete) | None => {
            RegistryDiscoveryCompleteness::Incomplete
        }
    };
    let caught_up = store.is_caught_up(source);
    let is_incomplete_backfill =
        completeness == RegistryDiscoveryCompleteness::CompleteThroughCursor && !caught_up;
    let historical = store.is_historical(source) || is_incomplete_backfill;
    let freshness = if store.is_unavailable(source) {
        RegistryDiscoveryFreshness::Unavailable {
            observed_at_millis: observed_at,
            historical,
        }
    } else if historical {
        RegistryDiscoveryFreshness::Historical {
            observed_at_millis: observed_at,
        }
    } else if now > valid_until {
        RegistryDiscoveryFreshness::Expired {
            observed_at_millis: observed_at,
            valid_until_millis: valid_until,
        }
    } else {
        RegistryDiscoveryFreshness::Current {
            observed_at_millis: observed_at,
            valid_until_millis: valid_until,
        }
    };
    let standing = match fact.standing {
        backend_engine::registry::DiscoveryStanding::Published => {
            RegistryDiscoveryStanding::Published
        }
        backend_engine::registry::DiscoveryStanding::Yanked => RegistryDiscoveryStanding::Yanked,
        backend_engine::registry::DiscoveryStanding::Withdrawn => {
            RegistryDiscoveryStanding::Withdrawn
        }
        backend_engine::registry::DiscoveryStanding::RecipeAvailable => {
            RegistryDiscoveryStanding::RecipeAvailable
        }
    };
    Ok(RegistryDiscoveryCandidate {
        source: source.id(),
        coordinate: fact.coordinate.clone(),
        standing,
        completeness,
        caught_up,
        freshness,
        proof: fact.proof,
        metadata: registry_discovery_metadata(&fact.metadata)?,
    })
}

fn index_search_with_discovery(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: &ProductText,
    limit: u16,
    discovery: Option<&DiscoveryStore>,
    discovery_search: &DiscoverySearchIndex,
    local_search: &LocalDeclarationSearchIndex,
    forge_records: &[ForgeSearchRecord],
) -> Result<Box<[RegistrySearchHit]>, String> {
    let now = discovery_now();
    let limit = usize::from(limit);
    let mut ranked_hits = Vec::new();
    let mut forge_records_by_coordinate = BTreeMap::new();
    for record in forge_records {
        forge_records_by_coordinate
            .entry(record.coordinate.clone())
            .or_insert(record);
    }
    let forge_records_by_pin_source = forge_records
        .iter()
        .map(|record| {
            (
                (record.coordinate.clone(), record.resolution.commit.clone()),
                record,
            )
        })
        .collect::<BTreeMap<_, _>>();

    // Every projection returns its own top `limit` under the same evidence
    // order and a stable plane-local tie key. Any omitted item already has at
    // least `limit` better rows in its plane, so it cannot enter the global
    // top page. The typed evidence is retained through the merge.
    let acquired_page = catalog_index.search_page(catalog, query.as_str(), limit)?;
    for search_hit in acquired_page.hits {
        let record = catalog
            .get(search_hit.key)
            .ok_or_else(|| "catalog search ordinal is outside its revision".to_owned())?;
        ranked_hits.push(RankedRegistrySearchHit {
            evidence: search_hit.evidence,
            tie: SearchStableTie::acquired(search_hit.key, record),
            hit: RegistrySearchHit::Acquired(record.clone()),
        });
    }

    let local_page = local_search.page(query.as_str(), limit)?;
    for search_hit in local_page.hits {
        let row_id = search_hit.key;
        let Some(row) = view.row_ref(row_id) else {
            continue;
        };
        if matches!(row.id, RowId::Symbol(_)) {
            ranked_hits.push(RankedRegistrySearchHit {
                evidence: search_hit.evidence,
                tie: SearchStableTie::local(row_id, row.label.as_str()),
                hit: RegistrySearchHit::LocalDeclaration(declaration_explore_record(row)?),
            });
        }
    }
    let request = DiscoverySearchRequest {
        text: query.as_str(),
        ecosystem: None,
    };
    let discovery_page = match discovery {
        Some(store) => discovery_search.search_with_store(store, request, limit)?,
        None => discovery_search.search(request, limit)?,
    };
    for search_hit in discovery_page.hits {
        let key = search_hit.key;
        match &key.source {
            DiscoverySearchSource::Registry(source) => {
                let Some(discovery) = discovery else {
                    continue;
                };
                let Some(fact) = discovery.fact(*source, key.coordinate.as_str()) else {
                    continue;
                };
                // Freshness belongs to this fact's observation, not a newer
                // page watermark for a different package.
                let observed_at = fact.observed_at.as_unix_millis();
                let valid_until = observed_at.saturating_add(DISCOVERY_FRESHNESS_MILLIS);
                let completeness = match discovery.completeness(*source) {
                    Some(
                        backend_engine::registry::DiscoveryCompleteness::CompleteThroughCursor,
                    ) => RegistryDiscoveryCompleteness::CompleteThroughCursor,
                    Some(backend_engine::registry::DiscoveryCompleteness::Windowed) => {
                        RegistryDiscoveryCompleteness::Windowed
                    }
                    Some(backend_engine::registry::DiscoveryCompleteness::Unsupported) => {
                        RegistryDiscoveryCompleteness::Unsupported
                    }
                    Some(backend_engine::registry::DiscoveryCompleteness::Incomplete) | None => {
                        RegistryDiscoveryCompleteness::Incomplete
                    }
                };
                let caught_up = discovery.is_caught_up(*source);
                let is_incomplete_backfill = completeness
                    == RegistryDiscoveryCompleteness::CompleteThroughCursor
                    && !caught_up;
                let historical = discovery.is_historical(*source) || is_incomplete_backfill;
                let freshness = if discovery.is_unavailable(*source) {
                    RegistryDiscoveryFreshness::Unavailable {
                        observed_at_millis: observed_at,
                        historical,
                    }
                } else if historical {
                    RegistryDiscoveryFreshness::Historical {
                        observed_at_millis: observed_at,
                    }
                } else if now > valid_until {
                    RegistryDiscoveryFreshness::Expired {
                        observed_at_millis: observed_at,
                        valid_until_millis: valid_until,
                    }
                } else {
                    RegistryDiscoveryFreshness::Current {
                        observed_at_millis: observed_at,
                        valid_until_millis: valid_until,
                    }
                };
                let standing = match fact.standing {
                    backend_engine::registry::DiscoveryStanding::Published => {
                        RegistryDiscoveryStanding::Published
                    }
                    backend_engine::registry::DiscoveryStanding::Yanked => {
                        RegistryDiscoveryStanding::Yanked
                    }
                    backend_engine::registry::DiscoveryStanding::Withdrawn => {
                        RegistryDiscoveryStanding::Withdrawn
                    }
                    backend_engine::registry::DiscoveryStanding::RecipeAvailable => {
                        RegistryDiscoveryStanding::RecipeAvailable
                    }
                };
                let candidate = RegistryDiscoveryCandidate {
                    source: source.id(),
                    coordinate: fact.coordinate.clone(),
                    standing,
                    completeness,
                    caught_up,
                    freshness,
                    proof: fact.proof,
                    metadata: registry_discovery_metadata(&fact.metadata)?,
                };
                ranked_hits.push(RankedRegistrySearchHit {
                    evidence: search_hit.evidence,
                    tie: SearchStableTie::discovered(
                        candidate.coordinate.as_str(),
                        candidate.source,
                        candidate.proof,
                    ),
                    hit: RegistrySearchHit::Discovered(candidate),
                });
            }
            DiscoverySearchSource::Forge(source) => {
                let Some(record) = forge_records_by_coordinate.get(&source.coordinate) else {
                    continue;
                };
                let Some(candidate) = forge_discovery_candidate(
                    record,
                    key.manifest_path.as_deref(),
                    key.coordinate.as_str(),
                    source.coordinate.identity(),
                )?
                else {
                    continue;
                };
                ranked_hits.push(RankedRegistrySearchHit {
                    evidence: search_hit.evidence,
                    tie: SearchStableTie::forge(
                        candidate.coordinate.as_str(),
                        candidate.source,
                        candidate.forge_coordinate.as_str(),
                        candidate.manifest.path.as_str(),
                    ),
                    hit: RegistrySearchHit::ForgeDiscovered(candidate),
                });
            }
        }
    }
    let source_pin_page = discovery_search.source_pin_page_after(query.as_str(), limit, None)?;
    if !source_pin_page.hits.is_empty() {
        let mut details = BTreeMap::new();
        for search_hit in source_pin_page.hits {
            let key = search_hit.key;
            let detail_key = (
                key.source.clone(),
                key.ecosystem,
                key.manifest_path.clone(),
                key.resolved_commit.clone(),
            );
            if !details.contains_key(&detail_key) {
                let record = forge_records_by_pin_source
                    .get(&(key.source.clone(), key.resolved_commit.clone()))
                    .ok_or_else(|| {
                        "forge source-pin posting has no matching source record".to_owned()
                    })?;
                for detail in project_package_details(std::slice::from_ref(*record))
                    .map_err(|error| format!("project forge package details: {error:?}"))?
                {
                    if matches!(&detail.pin, ForgePackageViewPin::PinnedRevision { .. }) {
                        details.insert(
                            (
                                detail.source.clone(),
                                detail.manifest.ecosystem,
                                detail.manifest.path.to_string(),
                                detail.resolution.commit.clone(),
                            ),
                            detail,
                        );
                    }
                }
            }
            let detail = details.get(&detail_key).ok_or_else(|| {
                "forge source-pin posting has no matching authoritative manifest".to_owned()
            })?;
            ranked_hits.push(RankedRegistrySearchHit {
                evidence: search_hit.evidence,
                tie: SearchStableTie::forge_source_pin(&key),
                hit: RegistrySearchHit::ForgeSourcePin(product_forge_package_detail(detail)?),
            });
        }
    }
    ranked_hits.sort_by(|left, right| {
        left.evidence
            .rank()
            .cmp(&right.evidence.rank())
            .then_with(|| left.tie.cmp(&right.tie))
    });
    Ok(ranked_hits
        .into_iter()
        .take(limit)
        .map(|ranked| ranked.hit)
        .collect::<Vec<_>>()
        .into_boxed_slice())
}

fn registry_discovery_metadata(
    metadata: &backend_engine::registry::DiscoveryMetadata,
) -> Result<backend_library::RegistryDiscoveryMetadata, String> {
    use backend_engine::registry::DiscoveryFacet;
    use backend_library::{RegistryDiscoveryAdvisory, RegistryEvidenceFacet};

    let advisories = match &metadata.advisories {
        DiscoveryFacet::Known(values) => {
            let mut records = Vec::with_capacity(values.len());
            for value in values {
                records.push(RegistryDiscoveryAdvisory {
                    id: discovery_product_text(&value.id)?,
                    aliases: value
                        .aliases
                        .iter()
                        .map(|alias| discovery_product_text(alias))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_boxed_slice(),
                    summary: discovery_product_facet(&value.summary, |text| {
                        discovery_product_text(text)
                    })?,
                    severity: discovery_product_facet(&value.severity, |text| {
                        discovery_product_text(text)
                    })?,
                    fixed_in: discovery_product_facet(&value.fixed_in, |versions| {
                        versions
                            .iter()
                            .map(|version| discovery_product_text(version))
                            .collect::<Result<Vec<_>, _>>()
                            .map(Vec::into_boxed_slice)
                    })?,
                });
            }
            RegistryEvidenceFacet::Known(records.into_boxed_slice())
        }
        DiscoveryFacet::Absent => RegistryEvidenceFacet::Absent,
        DiscoveryFacet::Unknown => RegistryEvidenceFacet::Unknown,
    };
    Ok(backend_library::RegistryDiscoveryMetadata {
        downloads: discovery_product_facet(&metadata.downloads, |value| Ok(*value))?,
        advisories,
        yanked: discovery_product_facet(&metadata.yanked, |value| Ok(*value))?,
    })
}

fn discovery_product_facet<T, U>(
    facet: &backend_engine::registry::DiscoveryFacet<T>,
    map: impl FnOnce(&T) -> Result<U, String>,
) -> Result<backend_library::RegistryEvidenceFacet<U>, String> {
    use backend_engine::registry::DiscoveryFacet;
    use backend_library::RegistryEvidenceFacet;

    match facet {
        DiscoveryFacet::Known(value) => map(value).map(RegistryEvidenceFacet::Known),
        DiscoveryFacet::Absent => Ok(RegistryEvidenceFacet::Absent),
        DiscoveryFacet::Unknown => Ok(RegistryEvidenceFacet::Unknown),
    }
}

fn discovery_product_text(value: &str) -> Result<ProductText, String> {
    ProductText::new(value.to_owned())
        .map_err(|error| format!("registry discovery text violates product bounds: {error}"))
}

fn forge_discovery_candidate(
    record: &ForgeSearchRecord,
    manifest_path: Option<&str>,
    expected_coordinate: &str,
    source: [u8; 32],
) -> Result<Option<ForgeDiscoveryCandidate>, String> {
    let Some(manifest) = record
        .manifests
        .iter()
        .find(|manifest| manifest_path.map_or(false, |path| manifest.path.as_ref() == path))
    else {
        return Ok(None);
    };
    let Some(name) = (match &manifest.name {
        ForgeFact::Recorded(value) => Some(value.as_str()),
        ForgeFact::Unavailable(_) => None,
    }) else {
        return Ok(None);
    };
    let version = match &manifest.version {
        ForgeFact::Recorded(value) => value.as_str().to_owned(),
        ForgeFact::Unavailable(_) => return Ok(None),
    };
    let package_name = if manifest.ecosystem == RegistryEcosystem::Maven {
        name.replace(':', "/")
    } else {
        name.to_owned()
    };
    let coordinate = ProductPackageCoordinate::parse(format!(
        "pkg:{}/{}@{}",
        manifest.ecosystem.package_type().as_str(),
        package_name,
        version
    ))
    .map_err(|error| format!("forge manifest package coordinate is invalid: {error:?}"))?;
    if coordinate.as_str() != expected_coordinate {
        return Ok(None);
    }

    fn text_fact<T, U>(
        value: &ForgeFact<T>,
        map: impl FnOnce(&T) -> Result<U, String>,
    ) -> Result<backend_library::ForgeFact<U>, String> {
        match value {
            ForgeFact::Recorded(value) => map(value).map(backend_library::ForgeFact::Recorded),
            ForgeFact::Unavailable(reason) => ProductText::new(reason.as_str())
                .map(backend_library::ForgeFact::Unavailable)
                .map_err(|error| error.to_string()),
        }
    }
    let metadata = ForgeRepositoryMetadataRecord {
        owner: text_fact(&record.metadata.owner, |value| Ok(value.clone()))?,
        description: text_fact(&record.metadata.description, |value| Ok(value.clone()))?,
        license: text_fact(&record.metadata.license, |value| Ok(value.clone()))?,
        readme: text_fact(&record.metadata.readme, |value| Ok(value.clone()))?,
        topics: text_fact(&record.metadata.topics, |value| Ok(value.clone()))?,
        stars: text_fact(&record.metadata.stars, |value| Ok(*value))?,
        forks: text_fact(&record.metadata.forks, |value| Ok(*value))?,
    };
    let manifest = ForgeManifestRecord {
        path: ProductText::new(manifest.path.as_ref()).map_err(|error| error.to_string())?,
        ecosystem: ProductText::new(manifest.ecosystem.as_str())
            .map_err(|error| error.to_string())?,
        name: Some(ProductText::new(name).map_err(|error| error.to_string())?),
        version: Some(ProductText::new(&version).map_err(|error| error.to_string())?),
        dependencies: manifest.dependencies.clone(),
    };
    let commit = ProductText::new(&record.resolution.commit.as_hex())
        .map(backend_library::ForgeFact::Recorded)
        .map_err(|error| error.to_string())?;
    Ok(Some(ForgeDiscoveryCandidate {
        source,
        coordinate,
        forge_coordinate: ProductText::new(record.coordinate.canonical())
            .map_err(|error| error.to_string())?,
        commit,
        manifest,
        metadata,
    }))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SearchPlane {
    Acquired,
    Discovered,
    ForgeSourcePin,
    LocalDeclaration,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SearchStableTie {
    plane: SearchPlane,
    normalized_coordinate: String,
    source: [u8; 32],
    facts: [u8; 32],
    row_identity: String,
}

impl SearchStableTie {
    fn acquired(ordinal: usize, record: &RegistryPackageRecord) -> Self {
        Self {
            plane: SearchPlane::Acquired,
            normalized_coordinate: unicode_lowercase(record.coordinate.as_str()),
            source: record
                .authority
                .map_or([0; 32], |authority| authority.source),
            facts: record.facts_version,
            row_identity: format!("{ordinal:020}"),
        }
    }

    fn discovered(coordinate: &str, source: [u8; 32], proof: [u8; 32]) -> Self {
        Self {
            plane: SearchPlane::Discovered,
            normalized_coordinate: unicode_lowercase(coordinate),
            source,
            facts: proof,
            row_identity: String::new(),
        }
    }

    fn forge(coordinate: &str, source: [u8; 32], forge: &str, manifest: &str) -> Self {
        Self {
            plane: SearchPlane::Discovered,
            normalized_coordinate: unicode_lowercase(coordinate),
            source,
            facts: [0; 32],
            row_identity: format!("{forge}\0{manifest}"),
        }
    }

    fn forge_source_pin(key: &ForgeSourcePinSearchKey) -> Self {
        Self {
            plane: SearchPlane::ForgeSourcePin,
            normalized_coordinate: unicode_lowercase(&key.source.canonical()),
            source: key.source.identity(),
            facts: [0; 32],
            row_identity: format!(
                "{}\0{}\0{}",
                key.ecosystem.as_str(),
                key.manifest_path,
                key.resolved_commit.as_hex()
            ),
        }
    }

    fn local(row_id: RowId, coordinate: &str) -> Self {
        Self {
            plane: SearchPlane::LocalDeclaration,
            normalized_coordinate: unicode_lowercase(coordinate),
            source: [0; 32],
            facts: [0; 32],
            row_identity: row_id.stable_key(),
        }
    }
}

struct RankedRegistrySearchHit {
    evidence: SearchMatchEvidence,
    tie: SearchStableTie,
    hit: RegistrySearchHit,
}

fn unicode_lowercase(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

fn discovery_now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn row_matches_index_query(row: &Row, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let filter = query.to_ascii_lowercase();
    if row.label.to_ascii_lowercase().contains(&filter) {
        return true;
    }
    if row
        .label
        .rsplit("::")
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().contains(&filter))
    {
        return true;
    }
    if row
        .signature
        .as_deref()
        .is_some_and(|signature| signature.to_ascii_lowercase().contains(&filter))
    {
        return true;
    }
    if row
        .excerpt
        .text()
        .is_some_and(|excerpt| excerpt.to_ascii_lowercase().contains(&filter))
    {
        return true;
    }
    row.document.iter().any(|fragment| {
        let text = match fragment {
            Fragment::Text(value) | Fragment::Code(value) => value.as_str(),
            Fragment::Link { label, .. } => label.as_str(),
            Fragment::Break => return false,
        };
        text.to_ascii_lowercase().contains(&filter)
    })
}

pub(crate) fn indexed_semantic_versions(
    _view: &ViewRoot,
    _package: &PackageReference,
    _workspace: Option<&Path>,
) -> Result<Box<[SemanticVersionRecord]>, String> {
    // Manifest metadata cannot stand in for a selected compiler generation.
    Ok(Box::new([]))
}

fn indexed_package_records(
    view: &ViewRoot,
    package: &PackageReference,
    workspace: Option<&Path>,
) -> Result<Vec<RegistryPackageRecord>, String> {
    let mut records = Vec::new();
    for row in view.row_refs() {
        if !matches!(row.id, RowId::Package(_)) {
            continue;
        }
        if Path::new(&row.label).is_dir() {
            // An unreadable sibling can only fail this query when it names
            // this exact local path; otherwise it is skipped, unproven.
            let requested = matches!(
                package,
                PackageReference::Local(label) if label.as_str() == row.label.as_str()
            );
            let project_root = Path::new(&row.label);
            let manifest = match super::local_manifest::read_local_manifest(project_root) {
                Ok(Some(manifest)) => manifest,
                Ok(None) => {
                    if requested {
                        return Err(format!(
                            "indexed project {} has no supported manifest",
                            project_root.display()
                        ));
                    }
                    continue;
                }
                Err(error) => {
                    if requested {
                        return Err(error);
                    }
                    continue;
                }
            };
            if package_matches(package, &manifest.record) {
                records.push(manifest.record);
            }
            continue;
        }
        if !row.label.starts_with("pkg:") {
            continue;
        }
        let source = PackageReference::parse(&row.label).map_err(|error| error.to_string())?;
        if !package_reference_matches(package, &source) {
            continue;
        }
        let root = registry_project_root(view, &row.label, workspace)?;
        records.push(super::local_manifest::local_registry_record(&root)?);
    }
    Ok(records)
}

fn registry_project_root(
    view: &ViewRoot,
    project_label: &str,
    workspace: Option<&Path>,
) -> Result<PathBuf, String> {
    let expected = PackageReference::parse(project_label).map_err(|error| error.to_string())?;
    let prefix = format!("{project_label}::");
    for row in view.row_refs() {
        if !matches!(row.id, RowId::Symbol(_)) || !row.label.starts_with(&prefix) {
            continue;
        }
        if let Some(location) = row.source.captured() {
            let mut dir = Path::new(location.path());
            while let Some(parent) = dir.parent() {
                if let Some(manifest) = super::local_manifest::read_local_manifest(dir)? {
                    if package_reference_matches(&expected, &manifest.record.coordinate) {
                        return Ok(dir.to_path_buf());
                    }
                }
                dir = parent;
            }
        }
    }
    if let Some(workspace) = workspace {
        return registry_staging_root(workspace, &expected);
    }
    Err(format!(
        "indexed package {project_label} has no captured source path"
    ))
}

fn registry_staging_root(workspace: &Path, package: &PackageReference) -> Result<PathBuf, String> {
    let version = match package {
        PackageReference::Purl(coordinate) => coordinate.version(),
        PackageReference::Local(_) => "",
    };
    let staging = workspace.join("registry-staging");
    if !staging.is_dir() {
        return Err(format!(
            "registry staging is absent for {}",
            package.as_str()
        ));
    }
    for entry in fs::read_dir(&staging).map_err(|error| error.to_string())? {
        let path = entry
            .map_err(|error| error.to_string())?
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !path.is_dir()
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        if let Some(root) =
            super::local_manifest::staged_manifest_root(&path, version, package.as_str())?
        {
            return Ok(root);
        }
    }
    Err(format!(
        "indexed package {} has no staged registry manifest",
        package.as_str()
    ))
}

fn catalog_page(
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    query: Option<&ProductText>,
    limit: u16,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    catalog_index.page(catalog, query, limit)
}
fn package_reference_matches(requested: &PackageReference, candidate: &PackageReference) -> bool {
    match (requested, candidate) {
        (PackageReference::Purl(requested), PackageReference::Purl(candidate)) => {
            requested == candidate
        }
        (PackageReference::Local(requested), PackageReference::Local(candidate)) => {
            requested == candidate
        }
        (PackageReference::Purl(_), PackageReference::Local(_))
        | (PackageReference::Local(_), PackageReference::Purl(_)) => false,
    }
}

fn package_matches(package: &PackageReference, row: &RegistryPackageRecord) -> bool {
    package_reference_matches(package, &row.coordinate)
}

fn enrich_projects(
    projects: &[ProjectRecord],
    view: &ViewRoot,
    workspace: Option<&Path>,
) -> Result<Box<[ProjectRecord]>, String> {
    projects
        .iter()
        .cloned()
        .map(|project| enrich_project(project, view, workspace))
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

fn enrich_project(
    project: ProjectRecord,
    view: &ViewRoot,
    workspace: Option<&Path>,
) -> Result<ProjectRecord, String> {
    let member_manifest_names = project
        .members
        .iter()
        .map(|member| member_manifest_name(member, view, workspace))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    Ok(ProjectRecord {
        member_manifest_names,
        ..project
    })
}

fn member_manifest_name(
    member: &PackageReference,
    view: &ViewRoot,
    workspace: Option<&Path>,
) -> Result<ProductText, String> {
    match member {
        PackageReference::Local(label) => {
            let project_root = Path::new(label.as_str());
            if !project_root.is_dir() {
                return Err(format!(
                    "project member {} is not a local manifest path",
                    label.as_str()
                ));
            }
            let manifest = super::local_manifest::require_local_manifest(project_root)?;
            Ok(manifest.record.name)
        }
        PackageReference::Purl(_) => {
            let records = indexed_package_records(view, member, workspace)?;
            let record = records
                .first()
                .ok_or_else(|| format!("project member {} is not indexed", member.as_str()))?;
            Ok(record.name.clone())
        }
    }
}
/// Exact catalog maps plus a Tantivy-backed, replaceable search projection.
///
/// Registry facts remain authoritative. This view is built once for each
/// selected catalog revision and rebuilt from those facts after a cold start.
pub(super) struct CatalogLookupIndex {
    by_coordinate: BTreeMap<PackageReference, Vec<usize>>,
    by_name: BTreeMap<String, Vec<usize>>,
    search: Result<catalog_search::CatalogSearchIndex, String>,
    catalog_len: usize,
    snapshot: [u8; 32],
}

impl CatalogLookupIndex {
    pub(super) fn from_catalog(catalog: &[RegistryPackageRecord]) -> Self {
        let mut by_coordinate: BTreeMap<PackageReference, Vec<usize>> = BTreeMap::new();
        let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, record) in catalog.iter().enumerate() {
            by_coordinate
                .entry(record.coordinate.clone())
                .or_default()
                .push(index);
            by_name
                .entry(record.name.as_str().to_owned())
                .or_default()
                .push(index);
        }
        Self {
            by_coordinate,
            by_name,
            search: catalog_search::CatalogSearchIndex::build(catalog)
                .map_err(|error| format!("build Tantivy catalog projection: {error}")),
            catalog_len: catalog.len(),
            snapshot: Self::snapshot_for_catalog(catalog),
        }
    }

    /// Content identity of the current selected rows, including mutable
    /// freshness overlays. This remains separate from a reusable search index
    /// snapshot when only those overlays change.
    pub(super) fn snapshot_for_catalog(catalog: &[RegistryPackageRecord]) -> [u8; 32] {
        let mut snapshot = blake3::Hasher::new();
        snapshot.update(b"nudox.registry-catalog.selected-rows.v1\0");
        snapshot.update(&(catalog.len() as u64).to_le_bytes());
        for record in catalog {
            let encoded =
                serde_json::to_vec(record).unwrap_or_else(|_| format!("{record:?}").into_bytes());
            snapshot.update(&(encoded.len() as u64).to_le_bytes());
            snapshot.update(&encoded);
        }
        *snapshot.finalize().as_bytes()
    }

    fn snapshot(&self) -> [u8; 32] {
        self.snapshot
    }

    #[cfg(test)]
    pub(super) fn search_identity(&self) -> Option<usize> {
        self.search
            .as_ref()
            .ok()
            .map(|search| std::ptr::from_ref(search) as usize)
    }

    fn page(
        &self,
        catalog: &[RegistryPackageRecord],
        query: Option<&ProductText>,
        limit: u16,
    ) -> Result<Box<[RegistryPackageRecord]>, String> {
        if self.catalog_len != catalog.len() {
            return Err("catalog lookup index does not match the catalog".to_owned());
        }
        let limit = usize::from(limit);
        if limit == 0 {
            return Ok(Box::new([]));
        }
        let needle = query.map_or("", ProductText::as_str);
        if needle.is_empty() {
            return Ok(catalog
                .iter()
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
                .into_boxed_slice());
        }
        let page = self.search_page(catalog, needle, limit)?;
        page.hits
            .into_iter()
            .map(|hit| {
                catalog
                    .get(hit.key)
                    .cloned()
                    .ok_or_else(|| "catalog search ordinal is outside its revision".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Vec::into_boxed_slice)
    }

    /// Searches the resident postings for one query against their catalog rows.
    pub(super) fn search_page(
        &self,
        catalog: &[RegistryPackageRecord],
        query: &str,
        limit: usize,
    ) -> Result<SearchPage<usize>, String> {
        if self.catalog_len != catalog.len() {
            return Err("catalog lookup index does not match the catalog".to_owned());
        }
        self.search
            .as_ref()
            .map_err(Clone::clone)?
            .page(catalog, query, limit)
    }

    fn search_page_after(
        &self,
        catalog: &[RegistryPackageRecord],
        query: &str,
        limit: usize,
        continuation: Option<&super::discovery_search::SearchContinuation>,
    ) -> Result<SearchPage<usize>, String> {
        if self.catalog_len != catalog.len() {
            return Err("catalog lookup index does not match the catalog".to_owned());
        }
        self.search
            .as_ref()
            .map_err(Clone::clone)?
            .page_after(catalog, query, limit, continuation)
    }

    fn lineage_page_after(
        &self,
        catalog: &[RegistryPackageRecord],
        query: &str,
        limit: usize,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<LineageKey>, String> {
        if self.catalog_len != catalog.len() {
            return Err("catalog lookup index does not match the catalog".to_owned());
        }
        self.search
            .as_ref()
            .map_err(Clone::clone)?
            .lineage_page_after(catalog, query, limit, continuation)
    }

    fn matching_lineage_releases(
        &self,
        catalog: &[RegistryPackageRecord],
        key: &LineageKey,
        query: &str,
    ) -> Result<
        (
            Vec<RegistryPackageRecord>,
            bool,
            usize,
            RegistryReleaseMatchScope,
        ),
        String,
    > {
        if self.catalog_len != catalog.len() {
            return Err("catalog lookup index does not match the catalog".to_owned());
        }
        self.search
            .as_ref()
            .map_err(Clone::clone)?
            .matching_lineage_releases(catalog, key, query)
    }

    pub(super) fn first_coordinate<'a>(
        &self,
        catalog: &'a [RegistryPackageRecord],
        package: &PackageReference,
    ) -> Result<Option<&'a RegistryPackageRecord>, String> {
        let Some(indexes) = self.by_coordinate.get(package) else {
            return Ok(None);
        };
        if indexes.len() != 1 {
            return Err(format!(
                "package coordinate {} has multiple registry authorities",
                package.as_str()
            ));
        }
        let index = indexes[0];
        catalog
            .get(index)
            .map(Some)
            .ok_or_else(|| "catalog lookup index does not match the catalog".to_owned())
    }

    fn records_named<'a>(
        &self,
        catalog: &'a [RegistryPackageRecord],
        name: &str,
    ) -> Result<Vec<&'a RegistryPackageRecord>, String> {
        let Some(indexes) = self.by_name.get(name) else {
            return Ok(Vec::new());
        };
        self.records_at(catalog, indexes)
    }

    /// Rows whose typed coordinate matches `package`.
    ///
    /// `include_lineage` also accepts the purl lineage name, which is how
    /// version lookup finds every release of one package.
    pub(super) fn records_for<'a>(
        &self,
        catalog: &'a [RegistryPackageRecord],
        package: &PackageReference,
        include_lineage: bool,
    ) -> Result<Vec<&'a RegistryPackageRecord>, String> {
        let mut positions = Vec::new();
        if let Some(indexes) = self.by_coordinate.get(package) {
            positions.extend(indexes.iter().copied());
        }
        match package {
            PackageReference::Purl(coordinate) => {
                // A version-specific PURL resolves only by typed coordinate.
                // Same-name rows may belong to another coordinate or authority.
                if include_lineage
                    && let Some(indexes) = self.by_name.get(coordinate.lineage_name())
                {
                    let ecosystem = coordinate.package_type().registry();
                    positions.extend(indexes.iter().copied().filter(|index| {
                        catalog
                            .get(*index)
                            .is_some_and(|record| ecosystem == Some(record.ecosystem))
                    }));
                }
            }
            PackageReference::Local(name) => {
                if let Some(indexes) = self.by_name.get(name.as_str()) {
                    positions.extend(indexes.iter().copied());
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        self.records_at(catalog, &positions)
    }

    fn records_for_sources<'a>(
        &self,
        catalog: &'a [RegistryPackageRecord],
        sources: &BTreeSet<PackageGraphSourceKey>,
    ) -> Result<Vec<&'a RegistryPackageRecord>, String> {
        let mut positions = Vec::new();
        for source in sources {
            let PackageGraphSourceAuthority::Registry(authority) = source.authority else {
                continue;
            };
            if let Some(indexes) = self.by_coordinate.get(&source.coordinate) {
                positions.extend(indexes.iter().copied().filter(|index| {
                    catalog.get(*index).is_some_and(|record| {
                        record.authority.is_some_and(|record_authority| {
                            record_authority.source == authority.as_bytes()
                        })
                    })
                }));
            }
        }
        positions.sort_unstable();
        positions.dedup();
        self.records_at(catalog, &positions)
    }

    fn records_at<'a>(
        &self,
        catalog: &'a [RegistryPackageRecord],
        positions: &[usize],
    ) -> Result<Vec<&'a RegistryPackageRecord>, String> {
        let mut records = Vec::with_capacity(positions.len());
        for index in positions {
            let record = catalog
                .get(*index)
                .ok_or_else(|| "catalog lookup index does not match the catalog".to_owned())?;
            records.push(record);
        }
        Ok(records)
    }
}

fn packages(
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    package: &PackageReference,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    Ok(catalog_index
        .records_for(catalog, package, false)?
        .into_iter()
        .cloned()
        .collect::<Vec<_>>()
        .into_boxed_slice())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionOrderState {
    EcosystemKnown,
    Unknown,
}

struct OrderedRegistryVersions {
    rows: Box<[RegistryPackageRecord]>,
    order: VersionOrderState,
}

/// Sorts releases newest-first only when the ecosystem's version grammar can
/// order every row. Unknown sets keep a stable coordinate order and remain
/// distinguishable to callers that select a single latest release.
fn sort_registry_versions(rows: &mut [RegistryPackageRecord]) -> VersionOrderState {
    sort_registry_versions_with(rows, normalized_version_key)
}

fn sort_registry_versions_with(
    rows: &mut [RegistryPackageRecord],
    mut normalize: impl FnMut(
        RegistryEcosystem,
        &str,
    ) -> Option<backend_engine::advisory::NormalizedVersion>,
) -> VersionOrderState {
    let Some(first) = rows.first() else {
        return VersionOrderState::EcosystemKnown;
    };
    let ecosystem = first.ecosystem;
    let keys = rows
        .iter()
        .map(|row| {
            if row.ecosystem == ecosystem {
                normalize(ecosystem, row.version.as_str())
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>();
    let Some(keys) = keys else {
        rows.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));
        return VersionOrderState::Unknown;
    };

    let mut order = keys.into_iter().enumerate().collect::<Vec<_>>();
    order.sort_by(|(left_index, left_key), (right_index, right_key)| {
        right_key.cmp(left_key).then_with(|| {
            rows[*left_index]
                .coordinate
                .cmp(&rows[*right_index].coordinate)
        })
    });
    let source_order = order
        .into_iter()
        .map(|(source_index, _)| source_index)
        .collect::<Vec<_>>();
    reorder_by_source_index(rows, &source_order);
    VersionOrderState::EcosystemKnown
}

fn reorder_by_source_index<T>(rows: &mut [T], source_order: &[usize]) {
    debug_assert_eq!(rows.len(), source_order.len());
    let mut current_origin = (0..rows.len()).collect::<Vec<_>>();
    let mut position_by_origin = (0..rows.len()).collect::<Vec<_>>();
    for (destination, desired_origin) in source_order.iter().copied().enumerate() {
        let current_position = position_by_origin[desired_origin];
        if current_position == destination {
            continue;
        }
        let displaced_origin = current_origin[destination];
        rows.swap(destination, current_position);
        current_origin.swap(destination, current_position);
        position_by_origin[desired_origin] = destination;
        position_by_origin[displaced_origin] = current_position;
    }
}

fn normalized_version_key(
    ecosystem: RegistryEcosystem,
    version: &str,
) -> Option<backend_engine::advisory::NormalizedVersion> {
    let syntax = match ecosystem {
        RegistryEcosystem::Cargo | RegistryEcosystem::Npm | RegistryEcosystem::Nuget => {
            backend_engine::advisory::VersionSyntax::Semver
        }
        RegistryEcosystem::Golang => backend_engine::advisory::VersionSyntax::Go,
        RegistryEcosystem::Pypi => {
            // The shared PEP 440 key omits local-version ordering, so those
            // releases must remain explicitly unordered here.
            if version.contains('+') {
                return None;
            }
            backend_engine::advisory::VersionSyntax::Pep440
        }
        RegistryEcosystem::Maven => backend_engine::advisory::VersionSyntax::Maven,
        RegistryEcosystem::Cpp => backend_engine::advisory::VersionSyntax::Conan,
    };
    backend_engine::advisory::normalize_version(syntax, version).ok()
}

fn package_page(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    package: &PackageReference,
) -> Result<Box<[RegistryPackageRecord]>, String> {
    let records = packages(catalog, catalog_index, package)?;
    if !records.is_empty() {
        return Ok(records);
    }
    if let PackageReference::Local(label) = package {
        let project_root = Path::new(label.as_str());
        if project_root.is_dir()
            && let Some(manifest) = super::local_manifest::read_local_manifest(project_root)?
        {
            return Ok(Box::new([manifest.record]));
        }
    }
    let PackageReference::Purl(_) = package else {
        return Err(format!(
            "package {} is not recorded in the registry catalog",
            package.as_str()
        ));
    };
    for row in view.row_refs() {
        if !matches!(row.id, RowId::Package(_)) {
            continue;
        }
        let project_root = Path::new(&row.label);
        if !project_root.is_dir() {
            continue;
        }
        let Some(manifest) = super::local_manifest::read_local_manifest(project_root)? else {
            continue;
        };
        if &manifest.record.coordinate == package {
            return Ok(Box::new([manifest.record]));
        }
    }
    Err(format!(
        "package {} does not match any indexed local manifest",
        package.as_str()
    ))
}
fn versions(
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    package: &PackageReference,
) -> Result<OrderedRegistryVersions, String> {
    let mut rows = packages(catalog, catalog_index, package)?.into_vec();
    let order = sort_registry_versions(&mut rows);
    Ok(OrderedRegistryVersions {
        rows: rows.into_boxed_slice(),
        order,
    })
}

fn indexed_package_coordinates(
    view: &ViewRoot,
    workspace: &Path,
) -> Result<BTreeSet<String>, String> {
    let mut coordinates = BTreeSet::new();
    for row in view.row_refs() {
        if !matches!(row.id, RowId::Package(_)) {
            continue;
        }
        if row.label.starts_with("pkg:") {
            coordinates.insert(row.label.clone());
            continue;
        }
        // This enumerates every indexed coordinate; there is no single
        // package being asked about here, so no row can ever be "the one
        // requested". A row that cannot supply its own coordinate (an
        // unresolved source root, or a project with no supported manifest)
        // must not blank out every other indexed project's coordinate.
        let Ok(source_root) =
            super::local_manifest::indexed_package_source_root(&row.label, workspace)
        else {
            continue;
        };
        let Ok(Some(manifest)) = super::local_manifest::read_local_manifest(&source_root) else {
            continue;
        };
        coordinates.insert(manifest.record.coordinate.as_str().to_owned());
    }
    Ok(coordinates)
}

#[cfg_attr(not(test), allow(dead_code))]
fn version_matches(package: &PackageReference, row: &RegistryPackageRecord) -> bool {
    if package_matches(package, row) {
        return true;
    }
    match package {
        PackageReference::Purl(query) => {
            query.package_type().registry() == Some(row.ecosystem)
                && row.name.as_str() == query.lineage_name()
        }
        PackageReference::Local(_) => false,
    }
}

fn indexed_catalog_versions(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    workspace: &Path,
    package: &PackageReference,
) -> Result<Vec<RegistryPackageRecord>, String> {
    let indexed = indexed_package_coordinates(view, workspace)?;
    Ok(catalog_index
        .records_for(catalog, package, true)?
        .into_iter()
        .filter(|row| indexed.contains(row.coordinate.as_str()))
        .cloned()
        .collect())
}

fn package_versions(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    package: &PackageReference,
    workspace: Option<&Path>,
) -> Result<OrderedRegistryVersions, String> {
    let indexed = indexed_package_records(view, package, workspace)?;
    let mut rows = if !indexed.is_empty() {
        indexed
    } else if let Some(workspace) = workspace {
        let from_catalog =
            indexed_catalog_versions(view, catalog, catalog_index, workspace, package)?;
        if from_catalog.is_empty() {
            return versions(catalog, catalog_index, package);
        }
        from_catalog
    } else {
        return versions(catalog, catalog_index, package);
    };
    let order = sort_registry_versions(&mut rows);
    Ok(OrderedRegistryVersions {
        rows: rows.into_boxed_slice(),
        order,
    })
}

fn profile(
    view: &ViewRoot,
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    package: &PackageReference,
    workspace: Option<&Path>,
) -> Result<SurfaceReply, String> {
    let ordered = package_versions(view, catalog, catalog_index, package, workspace)?;
    let latest = latest_available_version(&ordered);
    let candidate_authority = latest
        .as_ref()
        .and_then(|row| row.authority)
        .or_else(|| withheld_candidate_authority(&ordered));
    Ok(SurfaceReply::PackageProfile {
        latest,
        versions: ordered.rows.len() as u64,
        candidate_authority,
    })
}

fn latest_available_version(ordered: &OrderedRegistryVersions) -> Option<RegistryPackageRecord> {
    if ordered.order == VersionOrderState::Unknown {
        return None;
    }
    let mut position = 0;
    while position < ordered.rows.len() {
        let row = &ordered.rows[position];
        let mut end = position + 1;
        while end < ordered.rows.len() && ordered.rows[end].coordinate == row.coordinate {
            end += 1;
        }
        // More than one source claims the exact coordinate. Keep every claim
        // available in package/version replies, but do not select one source's
        // standing as the unique current release.
        if end - position > 1 {
            return None;
        }
        if !has_current_standing_authority(row)
            || row.advisory.freshness == backend_engine::advisory::FreshnessState::Stale
            || row.downloads == RegistryDownloadCount::Unavailable(RegistryFactAvailability::Stale)
        {
            return None;
        }
        if row.standing == RegistryReleaseStanding::Available {
            return Some(row.clone());
        }
        position = end;
    }
    None
}

fn withheld_candidate_authority(
    ordered: &OrderedRegistryVersions,
) -> Option<backend_engine::RegistryPackageFactAuthority> {
    if ordered.order == VersionOrderState::Unknown {
        return None;
    }
    let row = ordered.rows.first()?;
    if ordered
        .rows
        .get(1)
        .is_some_and(|next| next.coordinate == row.coordinate)
    {
        return None;
    }
    row.authority
}

fn has_current_standing_authority(row: &RegistryPackageRecord) -> bool {
    let Some(authority) = row.authority else {
        return false;
    };
    if authority.facts_version != row.facts_version
        || authority.standing != backend_engine::RegistryPackageFactCompleteness::Complete
    {
        return false;
    }
    matches!(
        authority.release_facts_freshness,
        backend_engine::RegistryPackageFactFreshness::Current {
            valid_until_millis,
            ..
        } if valid_until_millis > current_epoch_millis()
    )
}

fn current_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
fn dependencies(
    facts: &[PackageDependencySourceFacts],
    index: &PackageGraphIndex,
    package: &PackageReference,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, String> {
    match index.dependencies(facts, package) {
        PackageDependencyLookup::Exact { source, facts } => {
            if source.authority == PackageGraphSourceAuthority::Unattributed {
                return Ok(DependencyFacts::Unavailable(
                    ProductText::new(format!(
                        "dependency facts for {} have no recorded source authority",
                        package.as_str()
                    ))
                    .map_err(|error| error.to_string())?,
                ));
            }
            return Ok(facts.clone());
        }
        PackageDependencyLookup::Ambiguous(keys) => {
            let authorities = keys
                .iter()
                .map(|key| format!("{:?}", key.authority))
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(DependencyFacts::Unavailable(
                ProductText::new(format!(
                    "dependency facts for {} are published by multiple authorities; select one of: {authorities}",
                    package.as_str()
                ))
                .map_err(|error| error.to_string())?,
            ));
        }
        PackageDependencyLookup::Missing => {}
    }
    if let Some(value) = local_manifest_dependencies(facts, index, package)? {
        return Ok(value);
    }
    Ok(DependencyFacts::Unavailable(
        ProductText::new(format!(
            "dependency facts are unavailable for {} because the package is not recorded",
            package.as_str()
        ))
        .map_err(|error| error.to_string())?,
    ))
}

/// Answers a local directory from the canonical manifest reader.
///
/// Indexed facts are keyed by the manifest coordinate. A path query resolves
/// that coordinate and reuses the resident rows. A path the index has not
/// seen yet is read through the same parser.
fn local_manifest_dependencies(
    facts: &[PackageDependencySourceFacts],
    index: &PackageGraphIndex,
    package: &PackageReference,
) -> Result<Option<DependencyFacts<Box<[PackageDependencyRecord]>>>, String> {
    let PackageReference::Local(label) = package else {
        return Ok(None);
    };
    let root = Path::new(label.as_str());
    if !root.is_dir() {
        return Ok(None);
    }
    if let Some(manifest) = super::local_manifest::read_local_manifest(root)?
        && let Some(value) = index.dependencies_for_source(
            facts,
            &super::local_manifest::dependency_source_key(root, manifest.record.coordinate.clone()),
        )
    {
        return Ok(Some(value.clone()));
    }
    Ok(super::local_manifest::local_dependency_facts(root)?.map(|(_, state)| state))
}

fn dependents(
    catalog: &[RegistryPackageRecord],
    catalog_index: &CatalogLookupIndex,
    facts: &[PackageDependencySourceFacts],
    index: &PackageGraphIndex,
    package: &PackageReference,
) -> Result<RegistryMetadata<Box<[RegistryPackageRecord]>>, String> {
    if facts.is_empty() {
        return Ok(RegistryMetadata::NotRecorded(
            ProductText::new("the configured feed does not record dependency metadata")
                .map_err(|error| error.to_string())?,
        ));
    }
    let (sources, gap) = match index.dependent_sources(facts, package) {
        DependentSources::NotPurl => {
            return Ok(RegistryMetadata::NotRecorded(
                ProductText::new("reverse dependency lookup requires a pinned package URL")
                    .map_err(|error| error.to_string())?,
            ));
        }
        DependentSources::Matched { sources, gap } => (sources, gap),
    };
    if sources.is_empty()
        && let Some(reason) = gap
    {
        return Ok(RegistryMetadata::NotRecorded(reason));
    }
    let mut records = catalog_index
        .records_for_sources(catalog, &sources)?
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for source in sources {
        if matches!(source.authority, PackageGraphSourceAuthority::Local(_)) {
            records.push(local_manifest_registry_record(&source.coordinate)?);
        }
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
        authority: None,
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
            rows.insert(format!(
                "pkg:cargo/{name}@{}",
                value.trim_matches(['\"', '\'', ','])
            ));
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
    use backend_engine::registry::{
        DiscoveryBatch, DiscoveryCompleteness, DiscoveryCursor, DiscoveryFact, DiscoveryObservedAt,
        DiscoverySourceIdentity, DiscoveryStanding, RegistryEndpoint,
    };
    use backend_engine::{
        AdvisoryPackageDto, DependencyAuthority, DependencyEvidence, DependencyFacts,
        DependencyScope, PackageDependencyRecord, PackageDependencyTarget, PackageReference,
        RegistryDownloadCount, RegistryEcosystem, RegistryFactAvailability, RegistryMetadata,
        RegistryPackageRecord, RegistryReleaseStanding,
    };
    use backend_library::RegistryNativeMetadata;
    use std::collections::BTreeSet;
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

    fn registry_row(coordinate: &str, name: &str) -> RegistryPackageRecord {
        let coordinate = PackageReference::parse(coordinate).expect("coordinate");
        let (ecosystem, version) = match &coordinate {
            PackageReference::Purl(coordinate) => (
                coordinate
                    .package_type()
                    .registry()
                    .expect("registry package type"),
                coordinate.version().to_owned(),
            ),
            PackageReference::Local(_) => (RegistryEcosystem::Cargo, "1.0.0".to_owned()),
        };
        let native_metadata = RegistryNativeMetadata::unavailable(ecosystem, "catalog test");
        RegistryPackageRecord {
            coordinate,
            ecosystem,
            name: ProductText::new(name).expect("name"),
            version: ProductText::new(version.as_str()).expect("version"),
            bytes: 0,
            standing: RegistryReleaseStanding::Available,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
            facts_version: [0; 32],
            authority: None,
            native_metadata_version: native_metadata.identity().expect("identity"),
            native_metadata,
            forge_sources: Box::new([]),
            advisory: AdvisoryPackageDto::unknown(),
        }
    }

    fn current_test_authority(
        row: &RegistryPackageRecord,
    ) -> backend_engine::RegistryPackageFactAuthority {
        let now = current_epoch_millis();
        backend_engine::RegistryPackageFactAuthority {
            source: [1; 32],
            source_facts_root: [2; 32],
            source_provenance: [3; 32],
            facts_version: row.facts_version,
            advisory_facts_version: [4; 32],
            selection_version: [5; 32],
            standing: backend_engine::RegistryPackageFactCompleteness::Complete,
            downloads: backend_engine::RegistryPackageFactCompleteness::Complete,
            advisories: backend_engine::RegistryPackageFactCompleteness::Unknown,
            release_facts_freshness: backend_engine::RegistryPackageFactFreshness::Current {
                observed_at_millis: now.saturating_sub(1),
                valid_until_millis: now.saturating_add(60_000),
                proof: backend_engine::RegistryPackageFactProof::AcquisitionReceipt {
                    receipt: [6; 32],
                    snapshot: [7; 32],
                },
            },
            advisory_freshness: row.advisory.freshness,
        }
    }

    #[test]
    fn catalog_release_order_uses_numeric_semver_components() {
        let mut rows = vec![
            registry_row("pkg:cargo/demo@1.9.0", "demo"),
            registry_row("pkg:cargo/demo@1.10.0", "demo"),
        ];
        assert_eq!(
            sort_registry_versions(&mut rows),
            VersionOrderState::EcosystemKnown
        );
        assert_eq!(rows[0].version.as_str(), "1.10.0");
        assert_eq!(rows[1].version.as_str(), "1.9.0");

        let mut python = vec![
            registry_row("pkg:pypi/demo@1.9", "demo"),
            registry_row("pkg:pypi/demo@1.10", "demo"),
        ];
        assert_eq!(
            sort_registry_versions(&mut python),
            VersionOrderState::EcosystemKnown
        );
        assert_eq!(python[0].version.as_str(), "1.10");
        assert_eq!(python[1].version.as_str(), "1.9");

        let mut maven = vec![
            registry_row("pkg:maven/org.example/demo@1.9", "demo"),
            registry_row("pkg:maven/org.example/demo@1.10", "demo"),
        ];
        assert_eq!(
            sort_registry_versions(&mut maven),
            VersionOrderState::EcosystemKnown
        );
        assert_eq!(maven[0].version.as_str(), "1.10");
        assert_eq!(maven[1].version.as_str(), "1.9");
    }

    #[test]
    fn pypi_local_versions_without_ordering_are_reported_unknown() {
        let mut rows = vec![
            registry_row("pkg:pypi/demo@1.0+cpu", "demo"),
            registry_row("pkg:pypi/demo@1.0+gpu", "demo"),
        ];
        let order = sort_registry_versions(&mut rows);
        assert_eq!(order, VersionOrderState::Unknown);
        assert_eq!(
            latest_available_version(&OrderedRegistryVersions {
                rows: rows.into_boxed_slice(),
                order,
            }),
            None
        );
    }

    #[test]
    fn catalog_release_order_parses_each_version_once_for_a_large_history() {
        let version_count = 512_usize;
        let mut rows = (0..version_count)
            .rev()
            .map(|minor| registry_row(&format!("pkg:cargo/demo@1.{minor}.0"), "demo"))
            .collect::<Vec<_>>();
        let parse_count = std::cell::Cell::new(0);

        assert_eq!(
            sort_registry_versions_with(&mut rows, |ecosystem, version| {
                parse_count.set(parse_count.get() + 1);
                normalized_version_key(ecosystem, version)
            }),
            VersionOrderState::EcosystemKnown
        );
        assert_eq!(parse_count.get(), version_count);
        assert_eq!(
            rows.first().map(|row| row.version.as_str()),
            Some("1.511.0")
        );
        assert_eq!(rows.last().map(|row| row.version.as_str()), Some("1.0.0"));
        assert!(rows.windows(2).all(|pair| {
            let left = pair[0]
                .version
                .as_str()
                .split('.')
                .nth(1)
                .and_then(|minor| minor.parse::<usize>().ok());
            let right = pair[1]
                .version
                .as_str()
                .split('.')
                .nth(1)
                .and_then(|minor| minor.parse::<usize>().ok());
            matches!((left, right), (Some(left), Some(right)) if left >= right)
        }));
    }

    #[test]
    fn package_profile_moves_past_a_yanked_release_and_stops_on_stale_facts() {
        let mut older = registry_row("pkg:cargo/demo@1.9.0", "demo");
        older.authority = Some(current_test_authority(&older));
        let mut latest = registry_row("pkg:cargo/demo@1.10.0", "demo");
        latest.authority = Some(current_test_authority(&latest));
        let ordered = |rows: Vec<RegistryPackageRecord>| {
            let mut rows = rows;
            let order = sort_registry_versions(&mut rows);
            OrderedRegistryVersions {
                rows: rows.into_boxed_slice(),
                order,
            }
        };
        let rows = ordered(vec![older.clone(), latest.clone()]);
        assert_eq!(
            latest_available_version(&rows).map(|record| record.version),
            Some(ProductText::new("1.10.0").expect("version"))
        );

        latest.standing = RegistryReleaseStanding::Yanked;
        let rows = ordered(vec![older.clone(), latest.clone()]);
        assert_eq!(
            latest_available_version(&rows).map(|record| record.version),
            Some(ProductText::new("1.9.0").expect("version"))
        );

        latest.standing = RegistryReleaseStanding::Available;
        latest.downloads = RegistryDownloadCount::Unavailable(RegistryFactAvailability::Stale);
        let rows = ordered(vec![older.clone(), latest.clone()]);
        assert_eq!(latest_available_version(&rows), None);

        latest.downloads =
            RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported);
        latest.advisory.freshness = backend_engine::advisory::FreshnessState::Stale;
        let latest_authority = latest.authority.as_mut().expect("authority");
        latest_authority.advisory_freshness = backend_engine::advisory::FreshnessState::Stale;
        let rows = ordered(vec![older, latest]);
        assert_eq!(latest_available_version(&rows), None);

        let mut unknown = registry_row("pkg:cargo/demo@1.10.0", "demo");
        unknown.authority = None;
        let rows = ordered(vec![registry_row("pkg:cargo/demo@1.9.0", "demo"), unknown]);
        assert_eq!(latest_available_version(&rows), None);
    }

    #[test]
    fn lineage_search_does_not_mix_ecosystems_with_the_same_name() {
        let catalog = [
            registry_row("pkg:cargo/shared@1.9.0", "shared"),
            registry_row("pkg:npm/shared@1.10.0", "shared"),
        ];
        let query = PackageReference::parse("pkg:cargo/shared@9.9.9").expect("query");
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let rows = index
            .records_for(&catalog, &query, true)
            .expect("lineage lookup");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ecosystem, RegistryEcosystem::Cargo);
        assert_eq!(rows[0].version.as_str(), "1.9.0");
    }

    #[test]
    fn manifest_facts_do_not_create_fake_semantic_generations() {
        let package = PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package");
        assert!(
            indexed_semantic_versions(&view(), &package, None)
                .expect("no compiler publication")
                .is_empty()
        );
    }

    fn dependency_edge(
        source: &str,
        scope: DependencyScope,
        frontier: u8,
    ) -> (
        PackageGraphSourceKey,
        DependencyFacts<Box<[PackageDependencyRecord]>>,
    ) {
        let source = PackageReference::parse(source).expect("source");
        let source_authority = PackageGraphSourceAuthority::Registry(
            backend_library::RegistryAuthorityId::from_configured_source([1; 32]),
        );
        let target = PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("target");
        let row = PackageDependencyRecord::new_with_source_authority(
            source.clone(),
            source_authority,
            PackageDependencyTarget::new(
                RegistryEcosystem::Cargo,
                "target-lib",
                "^1",
                Some(target),
            )
            .expect("target"),
            scope,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [frontier; 32],
                provenance: [frontier + 1; 32],
            },
        );
        (
            PackageGraphSourceKey::new(source, source_authority),
            DependencyFacts::Known(vec![row].into_boxed_slice()),
        )
    }

    #[test]
    fn dependents_counts_only_runtime_and_optional_scopes() {
        let target = PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("target");
        let mut catalog = [
            registry_row("pkg:cargo/runtime-src@1.0.0", "runtime-src"),
            registry_row("pkg:cargo/dev-src@1.0.0", "dev-src"),
            registry_row("pkg:cargo/build-src@1.0.0", "build-src"),
            registry_row("pkg:cargo/peer-src@1.0.0", "peer-src"),
            registry_row("pkg:cargo/optional-src@1.0.0", "optional-src"),
        ];
        for row in &mut catalog {
            row.authority = Some(current_test_authority(row));
        }
        let facts = [
            dependency_edge("pkg:cargo/runtime-src@1.0.0", DependencyScope::Runtime, 1),
            dependency_edge("pkg:cargo/dev-src@1.0.0", DependencyScope::Development, 2),
            dependency_edge("pkg:cargo/build-src@1.0.0", DependencyScope::Build, 3),
            dependency_edge("pkg:cargo/peer-src@1.0.0", DependencyScope::Peer, 4),
            dependency_edge("pkg:cargo/optional-src@1.0.0", DependencyScope::Optional, 5),
        ];
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);
        let index = PackageGraphIndex::from_facts(&facts);
        let result =
            dependents(&catalog, &catalog_index, &facts, &index, &target).expect("dependents");
        let RegistryMetadata::Recorded(rows) = result else {
            panic!("expected recorded dependents");
        };
        let coordinates = rows
            .iter()
            .map(|row| row.coordinate.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            coordinates,
            BTreeSet::from([
                "pkg:cargo/runtime-src@1.0.0",
                "pkg:cargo/optional-src@1.0.0",
            ])
        );
    }

    fn clone_records(rows: Vec<&RegistryPackageRecord>) -> Vec<RegistryPackageRecord> {
        rows.into_iter().cloned().collect()
    }

    #[test]
    fn catalog_lookup_preserves_order_duplicates_and_lineage() {
        let mut first = registry_row("pkg:cargo/serde@1.0.0", "serde");
        first.version = ProductText::new("1.0.0").expect("version");
        let mut second = registry_row("pkg:cargo/serde@2.0.0", "serde");
        second.version = ProductText::new("2.0.0").expect("version");
        let mut named_like_coordinate =
            registry_row("pkg:cargo/other@1.0.0", "pkg:cargo/serde@1.0.0");
        named_like_coordinate.version = ProductText::new("9.0.0").expect("version");
        let catalog = [
            registry_row("pkg:cargo/zzz@1.0.0", "zzz"),
            first,
            named_like_coordinate,
            second,
        ];
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let exact = PackageReference::parse("pkg:cargo/serde@1.0.0").expect("exact");
        let exact_rows = clone_records(index.records_for(&catalog, &exact, false).expect("exact"));
        let scanned = catalog
            .iter()
            .filter(|row| package_matches(&exact, row))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(exact_rows, scanned);
        assert_eq!(
            exact_rows
                .iter()
                .map(|row| row.coordinate.as_str())
                .collect::<Vec<_>>(),
            ["pkg:cargo/serde@1.0.0"]
        );
        let lineage = PackageReference::parse("pkg:cargo/serde@9.9.9").expect("lineage");
        let lineage_rows = clone_records(
            index
                .records_for(&catalog, &lineage, true)
                .expect("lineage"),
        );
        let scanned_lineage = catalog
            .iter()
            .filter(|row| version_matches(&lineage, row))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(lineage_rows, scanned_lineage);
        assert_eq!(
            lineage_rows
                .iter()
                .map(|row| row.version.as_str())
                .collect::<Vec<_>>(),
            ["1.0.0", "2.0.0"]
        );
        let named = clone_records(index.records_named(&catalog, "serde").expect("named"));
        assert_eq!(named.len(), 2);
        assert_eq!(named[0].version.as_str(), "1.0.0");
        let first_hit = index.first_coordinate(&catalog, &exact).expect("first");
        assert_eq!(first_hit.map(|row| row.version.as_str()), Some("1.0.0"));
        let stale = CatalogLookupIndex::from_catalog(&catalog);
        let error = stale
            .records_named(&catalog[..1], "serde")
            .expect_err("stale catalog");
        assert!(error.contains("does not match the catalog"));
    }

    #[test]
    fn catalog_name_lookup_skips_unrelated_rows() {
        const ROWS: usize = 8_192;
        const HITS: usize = 8;
        const SAMPLES: usize = 9;
        let catalog: Vec<_> = (0..ROWS)
            .map(|index| {
                if index >= ROWS - HITS {
                    let version = index - (ROWS - HITS);
                    registry_row(&format!("pkg:cargo/target-lib@{version}.0.0"), "target-lib")
                } else {
                    registry_row(&format!("pkg:cargo/pkg-{index:04}@1.0.0"), "pkg")
                }
            })
            .collect();
        let package = PackageReference::parse("pkg:cargo/target-lib@9.9.9").expect("package");
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let scan = || {
            catalog
                .iter()
                .filter(|row| version_matches(&package, row))
                .cloned()
                .collect::<Vec<_>>()
        };
        let lookup = || {
            index
                .records_for(&catalog, &package, true)
                .expect("lookup")
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(scan(), lookup());
        assert_eq!(lookup().len(), HITS);
        for _ in 0..2 {
            std::hint::black_box(scan());
            std::hint::black_box(lookup());
        }
        let mut scan_samples = Vec::with_capacity(SAMPLES);
        let mut lookup_samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            std::hint::black_box(scan());
            scan_samples.push(started.elapsed().as_nanos());
            let started = std::time::Instant::now();
            std::hint::black_box(lookup());
            lookup_samples.push(started.elapsed().as_nanos());
        }
        scan_samples.sort_unstable();
        lookup_samples.sort_unstable();
        let scan_median = scan_samples[SAMPLES / 2];
        let lookup_median = lookup_samples[SAMPLES / 2];
        eprintln!(
            "catalog_name_index rows={ROWS} hits={HITS} scan_median_ns={scan_median} \
             lookup_median_ns={lookup_median}"
        );
        let _ = (lookup_median, scan_median);
    }

    fn scanned_catalog_page(
        catalog: &[RegistryPackageRecord],
        query: Option<&ProductText>,
        limit: u16,
    ) -> Vec<RegistryPackageRecord> {
        let needle = query.map_or("", ProductText::as_str).to_lowercase();
        if needle.is_empty() {
            return catalog.iter().take(usize::from(limit)).cloned().collect();
        }
        let mut ranked = catalog
            .iter()
            .enumerate()
            .filter_map(|(position, row)| {
                let coordinate = unicode_lowercase(row.coordinate.as_str());
                let name = unicode_lowercase(row.name.as_str());
                let rank = if coordinate == needle {
                    0
                } else if name == needle {
                    1
                } else if coordinate.starts_with(&needle) {
                    2
                } else if name.starts_with(&needle) {
                    3
                } else if coordinate.contains(&needle) || name.contains(&needle) {
                    4
                } else {
                    return None;
                };
                Some((
                    rank,
                    coordinate,
                    row.authority.map_or([0; 32], |authority| authority.source),
                    row.facts_version,
                    position,
                    row,
                ))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
                .then_with(|| left.4.cmp(&right.4))
        });
        ranked
            .into_iter()
            .take(usize::from(limit))
            .map(|(_, _, _, _, _, row)| row.clone())
            .collect()
    }

    #[test]
    fn catalog_substring_page_matches_the_linear_scan() {
        let catalog = [
            registry_row("pkg:cargo/SeRde@1.0.0", "SeRde"),
            registry_row("pkg:cargo/aaa-aaa@1.0.0", "aaa"),
            registry_row("pkg:cargo/other@1.0.0", "other"),
            registry_row("pkg:cargo/serde@2.0.0", "serde"),
        ];
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let queries = [
            None,
            Some("pkg"),
            Some("se"),
            Some("serde"),
            Some("SERDE"),
            Some("RDe"),
            Some("aaa"),
            Some("a"),
            Some("missing-token"),
        ];
        for query in queries {
            let text = query.map(|value| ProductText::new(value).expect("query"));
            for limit in [0_u16, 1, 2, 8] {
                let indexed =
                    catalog_page(&catalog, &index, text.as_ref(), limit).expect("indexed page");
                let scanned = scanned_catalog_page(&catalog, text.as_ref(), limit);
                assert_eq!(
                    indexed.as_ref(),
                    scanned.as_slice(),
                    "{query:?} limit={limit}"
                );
            }
        }
        let stale = CatalogLookupIndex::from_catalog(&catalog);
        assert!(
            stale
                .page(&catalog[..1], None, 1)
                .expect_err("stale catalog")
                .contains("does not match the catalog")
        );
    }

    #[test]
    fn catalog_search_matches_linear_oracle_after_mutations_and_reopen() {
        let cargo = PackageReference::parse("pkg:cargo/shared@1.0.0").expect("cargo");
        let npm = PackageReference::parse("pkg:npm/shared@1.0.0").expect("npm");
        let unicode = PackageReference::parse("pkg:cargo/unicode@1.0.0").expect("unicode");
        let mut rows = BTreeMap::from([
            (
                cargo.clone(),
                registry_row("pkg:cargo/shared@1.0.0", "shared"),
            ),
            (npm.clone(), registry_row("pkg:npm/shared@1.0.0", "shared")),
            (unicode.clone(), {
                let mut record = registry_row("pkg:cargo/unicode@1.0.0", "unicode");
                record.name = ProductText::new("Café Straße").expect("unicode name");
                record
            }),
        ]);

        let queries = [
            "shared",
            "pkg:npm/shared@1.0.0",
            "SHARED",
            "CAFÉ",
            "ßE",
            "🚀",
            "pkg:cargo",
            "not-present",
        ];
        let compare_revision = |catalog: &[RegistryPackageRecord]| {
            let index = CatalogLookupIndex::from_catalog(catalog);
            for query in queries {
                let query = ProductText::new(query).expect("query");
                for limit in [1, 3, 64] {
                    let actual = catalog_page(catalog, &index, Some(&query), limit)
                        .expect("indexed results");
                    let expected = scanned_catalog_page(catalog, Some(&query), limit);
                    assert_eq!(
                        actual.as_ref(),
                        expected.as_slice(),
                        "{} limit={limit}",
                        query.as_str()
                    );

                    // A cold reopen has no Tantivy state. Rebuild once from the
                    // ordered authoritative rows and require identical results.
                    let reopened = CatalogLookupIndex::from_catalog(catalog);
                    let after_reopen =
                        catalog_page(catalog, &reopened, Some(&query), limit).expect("reopen");
                    assert_eq!(actual.as_ref(), after_reopen.as_ref());
                }
            }
        };

        let catalog = rows.values().cloned().collect::<Vec<_>>();
        compare_revision(&catalog);

        // Replacing mutable facts retains the same searchable terms while the
        // search hit must carry the replacement row and its historical status.
        let mut replacement = registry_row("pkg:cargo/shared@1.0.0", "shared");
        replacement.standing = RegistryReleaseStanding::Yanked;
        replacement.downloads = RegistryDownloadCount::Unavailable(RegistryFactAvailability::Stale);
        replacement.authority = Some(backend_engine::RegistryPackageFactAuthority {
            source: [1; 32],
            source_facts_root: [2; 32],
            source_provenance: [3; 32],
            facts_version: replacement.facts_version,
            advisory_facts_version: [4; 32],
            selection_version: [5; 32],
            standing: backend_engine::RegistryPackageFactCompleteness::Complete,
            downloads: backend_engine::RegistryPackageFactCompleteness::Partial,
            advisories: backend_engine::RegistryPackageFactCompleteness::Unknown,
            release_facts_freshness: backend_engine::RegistryPackageFactFreshness::Historical,
            advisory_freshness: backend_engine::advisory::FreshnessState::Stale,
        });
        rows.insert(cargo, replacement);
        let catalog = rows.values().cloned().collect::<Vec<_>>();
        compare_revision(&catalog);

        let inserted = registry_row("pkg:cargo/rocket@1.0.0", "naïve 🚀");
        rows.insert(inserted.coordinate.clone(), inserted);
        let catalog = rows.values().cloned().collect::<Vec<_>>();
        compare_revision(&catalog);

        rows.remove(&npm);
        rows.remove(&unicode);
        let catalog = rows.values().cloned().collect::<Vec<_>>();
        compare_revision(&catalog);
        assert!(
            catalog_page(
                &catalog,
                &CatalogLookupIndex::from_catalog(&catalog),
                Some(&ProductText::new("pkg:npm/shared@1.0.0").expect("deleted query")),
                8,
            )
            .expect("deleted query")
            .is_empty()
        );
    }

    #[test]
    fn freshness_overlay_reuses_tantivy_index_and_returns_current_rows() {
        let mut historical = registry_row("pkg:cargo/shared@1.0.0", "shared");
        historical.authority = Some(backend_engine::RegistryPackageFactAuthority {
            source: [1; 32],
            source_facts_root: [2; 32],
            source_provenance: [3; 32],
            facts_version: historical.facts_version,
            advisory_facts_version: [4; 32],
            selection_version: [5; 32],
            standing: backend_engine::RegistryPackageFactCompleteness::Complete,
            downloads: backend_engine::RegistryPackageFactCompleteness::Complete,
            advisories: backend_engine::RegistryPackageFactCompleteness::Unknown,
            release_facts_freshness: backend_engine::RegistryPackageFactFreshness::Current {
                observed_at_millis: 10,
                valid_until_millis: 100,
                proof: backend_engine::RegistryPackageFactProof::AcquisitionReceipt {
                    receipt: [6; 32],
                    snapshot: [7; 32],
                },
            },
            advisory_freshness: backend_engine::advisory::FreshnessState::Fresh,
        });
        let source = std::sync::Arc::new(CatalogLookupIndex::from_catalog(&[historical.clone()]));
        let original_identity = source.search_identity().expect("search index");

        historical.downloads = RegistryDownloadCount::Unavailable(RegistryFactAvailability::Stale);
        historical.advisory.freshness = backend_engine::advisory::FreshnessState::Stale;
        historical
            .authority
            .as_mut()
            .expect("authority")
            .release_facts_freshness = backend_engine::RegistryPackageFactFreshness::Historical;
        historical
            .authority
            .as_mut()
            .expect("authority")
            .advisory_freshness = backend_engine::advisory::FreshnessState::Stale;
        let overlay = std::sync::Arc::clone(&source);
        assert_eq!(overlay.search_identity(), Some(original_identity));
        let query = ProductText::new("shared").expect("query");
        assert_eq!(
            catalog_page(&[historical.clone()], &overlay, Some(&query), 1).expect("overlay page")
                [0],
            historical
        );

        let structural_change = [historical, registry_row("pkg:cargo/other@1.0.0", "other")];
        let rebuilt = CatalogLookupIndex::from_catalog(&structural_change);
        assert_ne!(rebuilt.search_identity(), Some(original_identity));
    }

    #[test]
    fn catalog_search_indexes_native_and_advisory_facets_with_standing() {
        let mut row = registry_row("pkg:cargo/native-demo@1.0.0", "native-demo");
        row.standing = RegistryReleaseStanding::Yanked;
        row.native_metadata = backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest([9; 32]),
            details: backend_library::RegistryNativeDetails::Cargo(
                backend_library::RegistryCargoMetadata {
                    artifacts: Box::new([]),
                    features: Box::new([backend_library::RegistryNativeFeature {
                        name: "needle-native-feature".to_owned(),
                        members: Box::new(["dep:needle-dependency".to_owned()]),
                    }]),
                },
            ),
        };
        row.native_metadata_version = row.native_metadata.identity().expect("metadata identity");
        row.advisory.coverage = backend_library::AdvisoryCoverage::Complete;
        row.advisory.freshness = backend_library::FreshnessState::Fresh;
        row.advisory.advisories = Box::new([backend_library::AdvisorySurfaceDto {
            schema: 1,
            canonical_id: "CVE-2099-0042".to_owned(),
            source_ids: Box::new([]),
            aliases: Box::new(["GHSA-needle".to_owned()]),
            statuses: Box::new([backend_library::AdvisoryStatus::Vulnerable]),
            categories: Box::new([backend_library::AdvisoryCategory::Vulnerability]),
            affected: Box::new([]),
            fixed_ranges: Box::new([">=2.0.0".to_owned()]),
            severity: backend_library::SeverityLevel::Critical,
            malware: backend_engine::advisory::MalwareCoverage::NotCovered,
            coverage: backend_library::AdvisoryCoverage::Complete,
            freshness: backend_library::FreshnessState::Fresh,
            withdrawn: None,
            yanked: true,
            unlisted: false,
            summary: Some("needle-advisory-summary".to_owned()),
        }]);

        let catalog = [row.clone()];
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let native = index
            .search_page(&catalog, "needle-native-feature", 8)
            .expect("native search");
        assert_eq!(native.hits.len(), 1);
        assert_eq!(native.hits[0].evidence, SearchMatchEvidence::KeywordTerms);
        let advisory = index
            .search_page(&catalog, "needle-advisory-summary", 8)
            .expect("advisory search");
        assert_eq!(advisory.hits.len(), 1);
        assert_eq!(
            advisory.hits[0].evidence,
            SearchMatchEvidence::AdvisoryTerms
        );
        assert_eq!(
            advisory.hits[0].standing,
            super::super::discovery_search::SearchStandingEvidence::Yanked
        );
        assert_eq!(
            catalog[advisory.hits[0].key].standing,
            RegistryReleaseStanding::Yanked
        );
    }

    #[test]
    fn split_release_metadata_returns_source_backed_representatives_with_typed_scope() {
        let mut first = registry_row("pkg:cargo/split-facet@1.0.0", "split-facet");
        first.native_metadata = backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest([0x21; 32]),
            details: backend_library::RegistryNativeDetails::Cargo(
                backend_library::RegistryCargoMetadata {
                    artifacts: Box::new([]),
                    features: Box::new([backend_library::RegistryNativeFeature {
                        name: "alpha".to_owned(),
                        members: Box::new([]),
                    }]),
                },
            ),
        };
        first.native_metadata_version = first
            .native_metadata
            .identity()
            .expect("first metadata identity");
        let mut first_authority = current_test_authority(&first);
        first_authority.source = [0x72; 32];
        first.authority = Some(first_authority);

        let mut second = registry_row("pkg:cargo/split-facet@2.0.0", "split-facet");
        second.native_metadata = backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest([0x22; 32]),
            details: backend_library::RegistryNativeDetails::Cargo(
                backend_library::RegistryCargoMetadata {
                    artifacts: Box::new([]),
                    features: Box::new([backend_library::RegistryNativeFeature {
                        name: "beta".to_owned(),
                        members: Box::new([]),
                    }]),
                },
            ),
        };
        second.native_metadata_version = second
            .native_metadata
            .identity()
            .expect("second metadata identity");
        let mut second_authority = current_test_authority(&second);
        second_authority.source = [0x72; 32];
        second.authority = Some(second_authority);

        let catalog = [first, second];
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);
        let view = view();
        let mut local_index = LocalDeclarationSearchIndex::default();
        local_index.sync(&view).expect("empty local index");
        let query = ProductText::new("alpha beta").expect("query");
        let page = index_search_page_with_discovery(
            &view,
            &catalog,
            &catalog_index,
            &query,
            8,
            None,
            None,
            None,
            &local_index,
            &[],
        )
        .expect("aggregate lineage match remains usable");

        let Some(RegistrySearchHit::PackageGroup(group)) = page.hits.first() else {
            panic!("the aggregate metadata query returns the package lineage");
        };
        assert_eq!(group.lineage.as_str(), "split-facet");
        assert_eq!(group.source, [0x72; 32]);
        assert_eq!(group.releases.len(), 2);
        assert_eq!(
            group.release_match_scope,
            RegistryReleaseMatchScope::LineageMetadataOnly
        );
        assert!(!group.more_releases);
        let release_coordinates = group
            .releases
            .iter()
            .map(|release| match release {
                RegistrySearchRelease::Acquired(record) => {
                    assert!(
                        record
                            .authority
                            .is_some_and(|authority| authority.source == [0x72; 32])
                    );
                    record.coordinate.as_str()
                }
                _ => panic!("the acquired lineage contains acquired release facts"),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            release_coordinates,
            BTreeSet::from(["pkg:cargo/split-facet@1.0.0", "pkg:cargo/split-facet@2.0.0",])
        );
        let encoded = serde_json::to_value(group).expect("typed scope serializes");
        assert_eq!(encoded["release_match_scope"], "lineage-metadata-only");
    }

    #[test]
    fn merged_lineage_pages_do_not_let_releases_crowd_sources_and_reject_stale_cursors() {
        let root = fixture("index-search-lineage-pages");
        let path = root.join("discovery.journal");
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
            .expect("Cargo discovery source");
        let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
        let observed = DiscoveryObservedAt::from_unix_millis(discovery_now());
        let mut discovery = DiscoveryStore::open(path).expect("discovery journal");
        let discovery_coordinate =
            backend_engine::ProductPackageCoordinate::parse("pkg:cargo/needle@1.0.0")
                .expect("discovered coordinate");
        let discovery_cursor = DiscoveryCursor::new(b"1".to_vec()).expect("cursor");
        discovery
            .commit(DiscoveryBatch {
                source,
                previous_cursor: DiscoveryCursor::default(),
                next_cursor: discovery_cursor.clone(),
                source_high_watermark: discovery_cursor,
                caught_up: true,
                observed_at: observed,
                completeness: DiscoveryCompleteness::Windowed,
                facts: vec![DiscoveryFact {
                    source,
                    coordinate: discovery_coordinate,
                    standing: DiscoveryStanding::Yanked,
                    observed_at: observed,
                    source_event_time: None,
                    proof: [0x51; 32],
                    metadata: backend_engine::registry::DiscoveryMetadata::default(),
                }],
                package_retractions: Vec::new(),
            })
            .expect("persist source-only claim");
        let discovery_index = DiscoverySearchIndex::open(&discovery).expect("search index");

        // Eighteen acquired releases share the exact same canonical lineage
        // and source authority as a single source-only claim. The expected
        // result has three distinct planes: the discovered claim, one local
        // declaration, and one acquired lineage with a bounded release facet.
        let catalog = (1..=18)
            .map(|major| {
                let mut row = registry_row(
                    &format!("pkg:cargo/needle@{major}.0.0"),
                    "acquired native label",
                );
                row.advisory.coverage = backend_library::AdvisoryCoverage::Complete;
                row.advisory.freshness = backend_library::FreshnessState::Fresh;
                row.advisory.advisories = Box::new([backend_library::AdvisorySurfaceDto {
                    schema: 1,
                    canonical_id: format!("CVE-2099-{major:04}"),
                    source_ids: Box::new([]),
                    aliases: Box::new([]),
                    statuses: Box::new([backend_library::AdvisoryStatus::Vulnerable]),
                    categories: Box::new([backend_library::AdvisoryCategory::Vulnerability]),
                    affected: Box::new([]),
                    fixed_ranges: Box::new([]),
                    severity: backend_library::SeverityLevel::Moderate,
                    malware: backend_engine::advisory::MalwareCoverage::NotCovered,
                    coverage: backend_library::AdvisoryCoverage::Complete,
                    freshness: backend_library::FreshnessState::Fresh,
                    withdrawn: None,
                    yanked: false,
                    unlisted: false,
                    summary: Some("needle advisory evidence".to_owned()),
                }]);
                let mut authority = current_test_authority(&row);
                authority.source = [0x35; 32];
                row.authority = Some(authority);
                row
            })
            .collect::<Vec<_>>();
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);

        let (empty_view, _) = super::super::initial_view().expect("initial view");
        let local_label = "pkg:local::src/lib.rs::needle-local";
        let local_row = Row::new(
            RowId::Symbol(backend_engine::symbol_key(local_label)),
            empty_view.basis(),
            local_label,
        );
        let prepared = empty_view
            .prepare(
                backend_engine::ViewDelta::Upsert { row: local_row },
                super::super::test_builtin_view_capability().expect("view capability"),
            )
            .expect("prepare local declaration");
        let (view, _) = empty_view
            .commit(prepared)
            .expect("commit local declaration");
        let mut local_index = LocalDeclarationSearchIndex::default();
        local_index.sync(&view).expect("local declaration index");

        let query = ProductText::new("needle").expect("query");
        let mut cursor = None;
        let mut first_cursor = None;
        let mut identities = Vec::new();
        let mut page_times = Vec::new();
        let mut acquired_group_work = None;
        let mut discovery_page_work = None;
        for _ in 0..4 {
            let mut work = IndexSearchWork::default();
            let page = index_search_page_with_discovery_measured(
                &view,
                &catalog,
                &catalog_index,
                &query,
                1,
                cursor.as_ref(),
                Some(&discovery),
                Some(&discovery_index),
                &local_index,
                &[],
                &mut work,
            )
            .expect("merged page");
            assert_eq!(page.snapshot.len(), 32);
            page_times.push(page.evaluated_at_millis);
            assert_eq!(page.hits.len(), 1);
            match &page.hits[0] {
                RegistrySearchHit::PackageGroup(group)
                    if group.kind == backend_library::RegistrySearchGroupKind::Discovered =>
                {
                    assert_eq!(group.lineage.as_str(), "needle");
                    assert_eq!(group.source, source.id());
                    assert_eq!(
                        group.release_match_scope,
                        RegistryReleaseMatchScope::ReleaseMatches
                    );
                    assert!(matches!(
                        group.releases.first(),
                        Some(RegistrySearchRelease::Discovered(candidate))
                            if candidate.standing == RegistryDiscoveryStanding::Yanked
                    ));
                    discovery_page_work = Some((
                        work.discovery_documents_visited,
                        work.discovery_facet_releases_examined,
                    ));
                    identities.push("discovered");
                }
                RegistrySearchHit::LocalDeclaration(record) => {
                    assert_eq!(record.name.as_str(), "needle-local");
                    identities.push("local");
                }
                RegistrySearchHit::PackageGroup(group)
                    if group.kind == backend_library::RegistrySearchGroupKind::Acquired =>
                {
                    assert_eq!(group.lineage.as_str(), "needle");
                    assert_eq!(group.source, [0x35; 32]);
                    assert_eq!(
                        group.release_match_scope,
                        RegistryReleaseMatchScope::ReleaseMatches
                    );
                    assert_eq!(group.releases.len(), 16);
                    assert!(group.more_releases);
                    assert!(group.releases.iter().all(|release| matches!(
                        release,
                        RegistrySearchRelease::Acquired(record)
                            if record.authority.is_some_and(|authority| authority.source == [0x35; 32])
                    )));
                    assert!(group.releases.iter().any(|release| matches!(
                        release,
                        RegistrySearchRelease::Acquired(record)
                            if record.coordinate.as_str() == "pkg:cargo/needle@1.0.0"
                    )));
                    acquired_group_work =
                        Some((work.acquired_lineage_postings, work.acquired_facet_postings));
                    identities.push("acquired");
                }
                hit => panic!("unexpected merged search hit: {hit:?}"),
            }
            cursor = page.next_cursor;
            if first_cursor.is_none() {
                first_cursor = cursor.clone();
            }
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(identities, ["discovered", "acquired", "local"]);
        assert!(page_times.windows(2).all(|pair| pair[0] == pair[1]));
        let (lineage_postings, release_postings) =
            acquired_group_work.expect("acquired lineage was measured");
        assert!(lineage_postings <= 2);
        assert_eq!(release_postings, 17);
        let (discovery_documents, discovery_releases) =
            discovery_page_work.expect("discovery lineage was measured");
        assert!(discovery_documents <= 2);
        assert!(discovery_releases <= 16);
        assert!(first_cursor.is_some());

        // A cursor is tied to the full selected catalog digest. Changing one
        // row's standing/advisory payload cannot resume against stale facets.
        let mut replacement = catalog.clone();
        replacement[0].standing = RegistryReleaseStanding::Yanked;
        let replacement_index = CatalogLookupIndex::from_catalog(&replacement);
        let stale = index_search_page_with_discovery(
            &view,
            &replacement,
            &replacement_index,
            &query,
            1,
            first_cursor.as_ref(),
            Some(&discovery),
            Some(&discovery_index),
            &local_index,
            &[],
        )
        .expect_err("changed release evidence invalidates the cursor");
        assert!(stale.contains("selected snapshot"));

        drop(discovery);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mutable_observation_tick_invalidates_cursor_with_reused_catalog_index() {
        let mut catalog = vec![
            registry_row("pkg:cargo/needle-alpha@1.0.0", "needle-alpha"),
            registry_row("pkg:cargo/needle-beta@1.0.0", "needle-beta"),
        ];
        for row in &mut catalog {
            row.authority = Some(current_test_authority(row));
        }
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);
        let search_identity = catalog_index.search_identity();
        let view = view();
        let mut local_index = LocalDeclarationSearchIndex::default();
        local_index.sync(&view).expect("empty local index");
        let query = ProductText::new("needle").expect("query");

        let first = index_search_page_with_discovery(
            &view,
            &catalog,
            &catalog_index,
            &query,
            1,
            None,
            None,
            None,
            &local_index,
            &[],
        )
        .expect("first page");
        let cursor = first.next_cursor.expect("second page exists");

        // The catalog postings remain structurally reusable, but one row's
        // mutable registry proof has advanced. A public cursor must bind this
        // selected row state separately from the reusable Tantivy snapshot.
        let mut refreshed = catalog.clone();
        let authority = refreshed[0].authority.as_mut().expect("registry authority");
        let backend_engine::RegistryPackageFactFreshness::Current {
            observed_at_millis,
            valid_until_millis,
            proof,
        } = authority.release_facts_freshness
        else {
            panic!("test authority is current");
        };
        authority.release_facts_freshness = backend_engine::RegistryPackageFactFreshness::Current {
            observed_at_millis: observed_at_millis.saturating_add(1),
            valid_until_millis: valid_until_millis.saturating_add(1),
            proof,
        };

        let stale = index_search_page_with_discovery(
            &view,
            &refreshed,
            &catalog_index,
            &query,
            1,
            Some(&cursor),
            None,
            None,
            &local_index,
            &[],
        )
        .expect_err("a freshness tick invalidates the old cursor");
        assert!(stale.contains("selected snapshot"));
        assert_eq!(catalog_index.search_identity(), search_identity);
    }

    #[test]
    fn acquired_keyset_deep_pages_have_bounded_posting_work_and_match_oracle() {
        const LINEAGES: usize = 128;
        let catalog = (0..LINEAGES)
            .map(|index| {
                registry_row(
                    &format!("pkg:cargo/needle-{index:03}@1.0.0"),
                    &format!("needle-{index:03}"),
                )
            })
            .collect::<Vec<_>>();
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);
        let view = view();
        let mut local_index = LocalDeclarationSearchIndex::default();
        local_index.sync(&view).expect("empty local index");
        let query = ProductText::new("needle").expect("query");
        let mut expected = (0..LINEAGES)
            .map(|index| format!("needle-{index:03}"))
            .collect::<Vec<_>>();
        expected.sort();

        let mut cursor = None;
        let mut actual = Vec::new();
        let mut deep_work = None;
        for page_number in 0..LINEAGES {
            let mut work = IndexSearchWork::default();
            let page = index_search_page_with_discovery_measured(
                &view,
                &catalog,
                &catalog_index,
                &query,
                1,
                cursor.as_ref(),
                None,
                None,
                &local_index,
                &[],
                &mut work,
            )
            .expect("keyset page");
            let Some(RegistrySearchHit::PackageGroup(group)) = page.hits.first() else {
                panic!("acquired page must yield one lineage group");
            };
            assert_eq!(
                group.kind,
                backend_library::RegistrySearchGroupKind::Acquired
            );
            actual.push(group.lineage.as_str().to_owned());
            if page_number == 96 {
                deep_work = Some(work);
            }
            cursor = page.next_cursor;
        }
        assert_eq!(actual, expected);
        assert_eq!(actual.iter().collect::<BTreeSet<_>>().len(), actual.len());
        let deep_work = deep_work.expect("deep page was measured");
        assert!(deep_work.acquired_lineage_postings <= 2);
        assert!(deep_work.acquired_facet_postings <= 17);
        assert_eq!(deep_work.discovery_postings, 0);
        assert_eq!(deep_work.discovery_documents_visited, 0);
        assert_eq!(deep_work.discovery_facet_releases_examined, 0);
        assert_eq!(deep_work.local_postings, 0);
    }

    #[test]
    fn discovery_exact_name_outranks_acquired_advisory_match_and_keeps_yank() {
        let root = fixture("search-global-evidence");
        let path = root.join("discovery.journal");
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
            .expect("Cargo discovery source");
        let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
        let observed = DiscoveryObservedAt::from_unix_millis(discovery_now());
        let coordinate =
            backend_engine::ProductPackageCoordinate::parse("pkg:cargo/rare-needle@2.0.0")
                .expect("discovered coordinate");
        let cursor = |value: &str| DiscoveryCursor::new(value.as_bytes().to_vec()).expect("cursor");
        let mut discovery = DiscoveryStore::open(path.clone()).expect("discovery journal");
        discovery
            .commit(DiscoveryBatch {
                source,
                previous_cursor: DiscoveryCursor::default(),
                next_cursor: cursor("1"),
                source_high_watermark: cursor("1"),
                caught_up: true,
                observed_at: observed,
                completeness: DiscoveryCompleteness::CompleteThroughCursor,
                facts: vec![DiscoveryFact {
                    source,
                    coordinate,
                    standing: DiscoveryStanding::Yanked,
                    observed_at: observed,
                    source_event_time: None,
                    proof: [7; 32],
                    metadata: backend_engine::registry::DiscoveryMetadata::default(),
                }],
                package_retractions: Vec::new(),
            })
            .expect("commit source claim");

        let mut acquired = registry_row("pkg:cargo/native-demo@1.0.0", "native-demo");
        acquired.standing = RegistryReleaseStanding::Yanked;
        acquired.advisory.coverage = backend_library::AdvisoryCoverage::Complete;
        acquired.advisory.freshness = backend_library::FreshnessState::Fresh;
        acquired.advisory.advisories = Box::new([backend_library::AdvisorySurfaceDto {
            schema: 1,
            canonical_id: "CVE-2099-0042".to_owned(),
            source_ids: Box::new([]),
            aliases: Box::new([]),
            statuses: Box::new([backend_library::AdvisoryStatus::Vulnerable]),
            categories: Box::new([backend_library::AdvisoryCategory::Vulnerability]),
            affected: Box::new([]),
            fixed_ranges: Box::new([]),
            severity: backend_library::SeverityLevel::High,
            malware: backend_engine::advisory::MalwareCoverage::NotCovered,
            coverage: backend_library::AdvisoryCoverage::Complete,
            freshness: backend_library::FreshnessState::Fresh,
            withdrawn: None,
            yanked: true,
            unlisted: false,
            summary: Some("rare-needle in a source advisory".to_owned()),
        }]);
        let catalog = [acquired];
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);
        let discovery_index = DiscoverySearchIndex::open(&discovery).expect("discovery index");
        let query = ProductText::new("rare-needle").expect("query");

        let acquired_page = catalog_index
            .search_page(&catalog, query.as_str(), 8)
            .expect("acquired facet page");
        assert_eq!(
            acquired_page.hits[0].evidence,
            SearchMatchEvidence::AdvisoryTerms
        );
        assert_eq!(
            acquired_page.hits[0].standing,
            super::super::discovery_search::SearchStandingEvidence::Yanked
        );
        let discovery_page = discovery_index
            .search_with_store(
                &discovery,
                DiscoverySearchRequest {
                    text: query.as_str(),
                    ecosystem: None,
                },
                8,
            )
            .expect("discovery page");
        assert_eq!(
            discovery_page.hits[0].evidence,
            SearchMatchEvidence::ExactName
        );
        assert_eq!(
            discovery_page.hits[0].standing,
            super::super::discovery_search::SearchStandingEvidence::Yanked
        );

        let view = view();
        let mut local_search = LocalDeclarationSearchIndex::default();
        local_search.sync(&view).expect("empty local index");
        let merged = index_search_with_discovery(
            &view,
            &catalog,
            &catalog_index,
            &query,
            8,
            Some(&discovery),
            &discovery_index,
            &local_search,
            &[],
        )
        .expect("merged search");
        assert!(matches!(
            &merged[0],
            RegistrySearchHit::Discovered(candidate)
                if candidate.coordinate.as_str() == "pkg:cargo/rare-needle@2.0.0"
                    && candidate.standing == RegistryDiscoveryStanding::Yanked
        ));
        assert!(matches!(
            &merged[1],
            RegistrySearchHit::Acquired(record)
                if record.coordinate.as_str() == "pkg:cargo/native-demo@1.0.0"
                    && record.standing == RegistryReleaseStanding::Yanked
        ));
        drop(discovery);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_source_refresh_does_not_refresh_untouched_package_facts() {
        let root = fixture("discovery-fact-freshness");
        let path = root.join("discovery.journal");
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
            .expect("Cargo discovery source");
        let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
        let now = discovery_now();
        let old_observation = now.saturating_sub(DISCOVERY_FRESHNESS_MILLIS + 1);
        let old = DiscoveryObservedAt::from_unix_millis(old_observation);
        let current = DiscoveryObservedAt::from_unix_millis(now);
        let cursor = |value: &str| DiscoveryCursor::new(value.as_bytes().to_vec()).expect("cursor");
        let fact = |name: &str, observed_at| DiscoveryFact {
            source,
            coordinate: backend_engine::ProductPackageCoordinate::parse(format!(
                "pkg:cargo/{name}@1.0.0"
            ))
            .expect("coordinate"),
            standing: DiscoveryStanding::Published,
            observed_at,
            source_event_time: None,
            proof: [1; 32],
            metadata: backend_engine::registry::DiscoveryMetadata::default(),
        };
        let batch = |previous_cursor: DiscoveryCursor,
                     next_cursor: DiscoveryCursor,
                     observed_at: DiscoveryObservedAt,
                     facts: Vec<DiscoveryFact>| {
            DiscoveryBatch {
                source,
                previous_cursor,
                next_cursor: next_cursor.clone(),
                source_high_watermark: next_cursor,
                caught_up: true,
                observed_at,
                completeness: DiscoveryCompleteness::CompleteThroughCursor,
                facts,
                package_retractions: Vec::new(),
            }
        };

        let mut discovery = DiscoveryStore::open(path.clone()).expect("open discovery journal");
        discovery
            .commit(batch(
                DiscoveryCursor::default(),
                cursor("1"),
                old,
                vec![fact("alpha-widget", old), fact("beta-widget", old)],
            ))
            .expect("persist first complete observation");
        discovery
            .commit(batch(
                cursor("1"),
                cursor("2"),
                current,
                vec![fact("alpha-widget", current)],
            ))
            .expect("persist bounded refresh of alpha only");
        assert_eq!(
            discovery.latest_observed_at(source),
            Some(now),
            "the source watermark advances with the latest page"
        );

        let search = DiscoverySearchIndex::open(&discovery).expect("search projection");
        let view = view();
        let mut local_search = LocalDeclarationSearchIndex::default();
        local_search
            .sync(&view)
            .expect("empty local declaration index");
        let query = ProductText::new("widget").expect("query");
        let hits = index_search_with_discovery(
            &view,
            &[],
            &CatalogLookupIndex::from_catalog(&[]),
            &query,
            16,
            Some(&discovery),
            &search,
            &local_search,
            &[],
        )
        .expect("combined search");
        let freshness = hits
            .iter()
            .flat_map(|hit| match hit {
                RegistrySearchHit::Discovered(candidate) => {
                    vec![(candidate.coordinate.as_str(), candidate.freshness)]
                }
                RegistrySearchHit::PackageGroup(group) => group
                    .releases
                    .iter()
                    .filter_map(|release| match release {
                        RegistrySearchRelease::Discovered(candidate) => {
                            Some((candidate.coordinate.as_str(), candidate.freshness))
                        }
                        RegistrySearchRelease::Acquired(_)
                        | RegistrySearchRelease::ForgeDiscovered(_) => None,
                    })
                    .collect(),
                RegistrySearchHit::Acquired(_)
                | RegistrySearchHit::ForgeDiscovered(_)
                | RegistrySearchHit::ForgeSourcePin(_)
                | RegistrySearchHit::LocalDeclaration(_) => Vec::new(),
            })
            .collect::<BTreeMap<_, _>>();
        assert!(matches!(
            freshness.get("pkg:cargo/alpha-widget@1.0.0"),
            Some(RegistryDiscoveryFreshness::Current { observed_at_millis, .. })
                if *observed_at_millis == now
        ));
        assert!(matches!(
            freshness.get("pkg:cargo/beta-widget@1.0.0"),
            Some(RegistryDiscoveryFreshness::Expired { observed_at_millis, .. })
                if *observed_at_millis == old_observation
        ));
        drop(discovery);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn forge_only_production_search_returns_source_attributed_manifest_facts() {
        let coordinate = backend_engine::ForgeCoordinate::new(
            "https://github.com/acme/widget-repo",
            backend_engine::ForgeRevision::Tag(
                backend_engine::ForgeRefName::new("v1.0.0").expect("tag"),
            ),
            None::<String>,
        )
        .expect("forge coordinate");
        let resolution = backend_engine::ForgeResolution::for_coordinate(
            &coordinate,
            backend_engine::ForgeObjectId::parse("0123456789abcdef0123456789abcdef01234567")
                .expect("commit"),
            Some(
                backend_engine::ForgeObjectId::parse("abcdefabcdefabcdefabcdefabcdefabcdefabcd")
                    .expect("tree"),
            ),
            "pinned-archive-fixture",
        )
        .expect("resolution");
        let package =
            PackageReference::parse("pkg:cargo/widget@1.0.0").expect("fixture package coordinate");
        let dependency = PackageDependencyRecord::new_with_source_authority(
            package.clone(),
            PackageGraphSourceAuthority::Forge(coordinate.identity()),
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1.0", None)
                .expect("manifest dependency"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::ForgeManifest,
                frontier: [7; 32],
                provenance: [8; 32],
            },
        );
        let forge_record = ForgeSearchRecord {
            coordinate: coordinate.clone(),
            resolution,
            archive: [9; 32],
            metadata: backend_engine::ForgeRepositoryMetadata {
                owner: ForgeFact::Recorded(ProductText::new("acme").expect("owner")),
                description: ForgeFact::Recorded(
                    ProductText::new("small source archive tools").expect("description"),
                ),
                license: ForgeFact::Recorded(ProductText::new("MIT").expect("license")),
                readme: ForgeFact::Recorded(
                    ProductText::new("bounded README fixture describes widget source")
                        .expect("readme"),
                ),
                topics: ForgeFact::Recorded(Box::new([])),
                stars: ForgeFact::Unavailable(
                    backend_engine::ForgeUnavailableReason::AuthorityOmitted,
                ),
                forks: ForgeFact::Unavailable(
                    backend_engine::ForgeUnavailableReason::AuthorityOmitted,
                ),
            },
            manifests: vec![
                backend_engine::ForgePackageManifest {
                    path: "Cargo.toml".into(),
                    ecosystem: RegistryEcosystem::Cargo,
                    name: ForgeFact::Recorded(ProductText::new("widget").expect("package name")),
                    version: ForgeFact::Recorded(ProductText::new("1.0.0").expect("version")),
                    dependencies: DependencyFacts::Known(vec![dependency].into_boxed_slice()),
                },
                backend_engine::ForgePackageManifest {
                    path: "examples/source-pin/Cargo.toml".into(),
                    ecosystem: RegistryEcosystem::Cargo,
                    name: ForgeFact::Recorded(
                        ProductText::new("widget-source-pin").expect("source-pin name"),
                    ),
                    version: ForgeFact::Unavailable(
                        backend_engine::ForgeUnavailableReason::AuthorityOmitted,
                    ),
                    dependencies: DependencyFacts::Known(Box::default()),
                },
            ]
            .into_boxed_slice(),
        };
        let documents = ForgeSearchDocument::from_search_record(&forge_record)
            .expect("forge search projection");
        assert_eq!(documents.len(), 1);
        let source_pin_documents = ForgeSourcePinSearchDocument::from_search_record(&forge_record)
            .expect("forge source-pin projection");
        assert_eq!(source_pin_documents.len(), 1);
        assert_eq!(
            documents[0].metadata.downloads,
            backend_engine::registry::DiscoveryFacet::Unknown
        );
        assert_eq!(
            documents[0].metadata.yanked,
            backend_engine::registry::DiscoveryFacet::Unknown
        );
        assert_eq!(
            documents[0].metadata.advisories,
            backend_engine::registry::DiscoveryFacet::Unknown
        );
        let index = DiscoverySearchIndex::open_forge_only_with_source_pins(
            &documents,
            &source_pin_documents,
        )
        .expect("forge-only search index");
        let view = view();
        let mut local_search = LocalDeclarationSearchIndex::default();
        local_search.sync(&view).expect("empty local search index");
        let search = |text: &str| {
            let query = ProductText::new(text).expect("query");
            index_search_with_discovery(
                &view,
                &[],
                &CatalogLookupIndex::from_catalog(&[]),
                &query,
                8,
                None,
                &index,
                &local_search,
                std::slice::from_ref(&forge_record),
            )
            .expect("combined production search")
        };

        for query in ["widget-repo", "serde", "MIT", "bounded README fixture"] {
            assert!(matches!(
                search(query).first(),
                Some(RegistrySearchHit::ForgeDiscovered(candidate))
                    if candidate.forge_coordinate.as_str() == coordinate.canonical()
                        && candidate.coordinate.as_str() == "pkg:cargo/widget@1.0.0"
                        && candidate.commit == backend_library::ForgeFact::Recorded(
                            ProductText::new("0123456789abcdef0123456789abcdef01234567").expect("commit text")
                        )
                        && candidate.manifest.path.as_str() == "Cargo.toml"
                        && matches!(&candidate.manifest.dependencies, DependencyFacts::Known(rows) if rows.len() == 1)
                        && matches!(&candidate.metadata.stars, backend_library::ForgeFact::Unavailable(_))
            ));
        }
        let source_pin_hits = search("widget-source-pin");
        let Some(RegistrySearchHit::ForgeSourcePin(pin)) = source_pin_hits.first() else {
            panic!("unversioned manifest should remain discoverable as a source pin");
        };
        assert!(matches!(
            &pin.pin,
            backend_library::ForgePackagePin::PinnedRevision { resolved_commit, .. }
                if resolved_commit.as_hex() == "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(pin.package_coordinate.is_none());
        assert_eq!(pin.manifest.path.as_str(), "examples/source-pin/Cargo.toml");
        assert_eq!(pin.registry.yanked, RegistryEvidenceFacet::Unknown);
        assert!(matches!(
            &pin.registry.downloads,
            RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unknown)
        ));
        let encoded = serde_json::to_vec(&RegistrySearchHit::ForgeSourcePin(pin.clone()))
            .expect("serialize typed source pin");
        let decoded: RegistrySearchHit =
            serde_json::from_slice(&encoded).expect("decode typed source pin");
        assert_eq!(decoded, RegistrySearchHit::ForgeSourcePin(pin.clone()));

        let exact_query = ProductText::new("widget-source-pin").expect("source-pin query");
        let page = index_search_page_with_discovery(
            &view,
            &[],
            &CatalogLookupIndex::from_catalog(&[]),
            &exact_query,
            8,
            None,
            None,
            Some(&index),
            &local_search,
            std::slice::from_ref(&forge_record),
        )
        .expect("typed source-pin page");
        assert_eq!(page.hits.len(), 1);
        assert!(matches!(
            &page.hits[0],
            RegistrySearchHit::ForgeSourcePin(detail)
                if detail.package_coordinate.is_none()
                    && detail.manifest.path.as_str() == "examples/source-pin/Cargo.toml"
        ));
        let reply = SurfaceReply::IndexSearchPage(page);
        reply
            .admit(reply.id())
            .expect("admit source-pin search reply");
        let wire = serde_json::to_vec(&reply).expect("serialize search reply");
        assert_eq!(
            serde_json::from_slice::<SurfaceReply>(&wire).expect("decode search reply"),
            reply
        );
    }

    #[test]
    fn catalog_substring_page_skips_unrelated_coordinates() {
        const ROWS: usize = 8_192;
        const SAMPLES: usize = 9;
        let catalog: Vec<_> = (0..ROWS)
            .map(|index| {
                if index + 1 == ROWS {
                    registry_row("pkg:cargo/zzq9needle@1.0.0", "zzq9needle")
                } else {
                    registry_row(&format!("pkg:cargo/pkg-{index:04}@1.0.0"), "pkg")
                }
            })
            .collect();
        let query = ProductText::new("zzq9needle").expect("query");
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let scan = || scanned_catalog_page(&catalog, Some(&query), 4);
        let lookup = || {
            catalog_page(&catalog, &index, Some(&query), 4)
                .expect("page")
                .into_vec()
        };
        assert_eq!(scan(), lookup());
        assert_eq!(lookup().len(), 1);
        assert_eq!(
            lookup()[0].coordinate.as_str(),
            "pkg:cargo/zzq9needle@1.0.0"
        );
        for _ in 0..2 {
            std::hint::black_box(scan());
            std::hint::black_box(lookup());
        }
        let mut scan_samples = Vec::with_capacity(SAMPLES);
        let mut lookup_samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            std::hint::black_box(scan());
            scan_samples.push(started.elapsed().as_nanos());
            let started = std::time::Instant::now();
            std::hint::black_box(lookup());
            lookup_samples.push(started.elapsed().as_nanos());
        }
        scan_samples.sort_unstable();
        lookup_samples.sort_unstable();
        let scan_median = scan_samples[SAMPLES / 2];
        let lookup_median = lookup_samples[SAMPLES / 2];
        eprintln!(
            "catalog_substring_page rows={ROWS} scan_median_ns={scan_median} \
             lookup_median_ns={lookup_median}"
        );
        // Timings are emitted for manual comparisons, never used as a CI gate.
        let _ = (lookup_median, scan_median);
    }

    #[test]
    #[ignore = "manual diagnostic; run with --ignored --nocapture to compare build, query, scan, and payload size"]
    fn manual_catalog_search_measurement_10k_rows() {
        const ROWS: usize = 12_288;
        const SAMPLES: usize = 101;
        let catalog: Vec<_> = (0..ROWS)
            .map(|index| {
                if index + 1 == ROWS {
                    registry_row("pkg:cargo/zzq9needle@1.0.0", "zzq9needle")
                } else {
                    registry_row(
                        &format!("pkg:cargo/pkg-{index:05}@1.0.0"),
                        &format!("package-{index:05}"),
                    )
                }
            })
            .collect();
        let query = ProductText::new("zzq9needle").expect("query");
        let serialized_rows_bytes = serde_json::to_vec(&catalog)
            .expect("serialize authoritative catalog")
            .len();

        let started = std::time::Instant::now();
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let cold_build_ns = started.elapsed().as_nanos();
        assert!(
            index.search_identity().is_some(),
            "Tantivy index must build"
        );

        let scan = || scanned_catalog_page(&catalog, Some(&query), 8);
        let lookup = || {
            catalog_page(&catalog, &index, Some(&query), 8)
                .expect("indexed page")
                .into_vec()
        };
        assert_eq!(scan(), lookup(), "index must match the linear oracle");
        for _ in 0..4 {
            std::hint::black_box(scan());
            std::hint::black_box(lookup());
        }

        let mut scan_samples = Vec::with_capacity(SAMPLES);
        let mut lookup_samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            std::hint::black_box(scan());
            scan_samples.push(started.elapsed().as_nanos());
            let started = std::time::Instant::now();
            std::hint::black_box(lookup());
            lookup_samples.push(started.elapsed().as_nanos());
        }
        scan_samples.sort_unstable();
        lookup_samples.sort_unstable();
        let percentile =
            |samples: &[u128], percentile: usize| samples[(samples.len() - 1) * percentile / 100];
        eprintln!(
            "manual_catalog_search rows={ROWS} serialized_rows_bytes={serialized_rows_bytes} \
             cold_build_ns={cold_build_ns} scan_p50_ns={} scan_p95_ns={} \
             indexed_p50_ns={} indexed_p95_ns={}",
            percentile(&scan_samples, 50),
            percentile(&scan_samples, 95),
            percentile(&lookup_samples, 50),
            percentile(&lookup_samples, 95),
        );
    }

    #[test]
    fn committed_state_reopens_at_the_exact_epoch() {
        let root = fixture("reopen");
        let path = root.join("product-state.json");
        let mut state = ProductState::open(path.clone()).expect("open empty state");
        let reply = state
            .execute(
                create("Nudox"),
                &view(),
                &[],
                &CatalogLookupIndex::from_catalog(&[]),
                &[],
                &PackageGraphIndex::from_facts(&[]),
                None,
            )
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
            .execute(
                create("Canonical"),
                &view(),
                &[],
                &CatalogLookupIndex::from_catalog(&[]),
                &[],
                &PackageGraphIndex::from_facts(&[]),
                None,
            )
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
            discovery_search: None,
            local_declaration_search: LocalDeclarationSearchIndex::default(),
        };
        let before = state.state.clone();
        assert!(
            state
                .execute(
                    create("Unpublished"),
                    &view(),
                    &[],
                    &CatalogLookupIndex::from_catalog(&[]),
                    &[],
                    &PackageGraphIndex::from_facts(&[]),
                    None,
                )
                .is_err()
        );
        assert_eq!(state.state, before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_local_path_reuses_indexed_manifest_dependencies() {
        let root = fixture("local-deps");
        fs::write(
            root.join("Cargo.toml"),
            "\
[package]
name = \"demo\"
version = \"1.0.0\"
edition = \"2021\"

[dependencies]
serde = \"1\"
",
        )
        .expect("manifest");
        let package = PackageReference::parse(root.to_str().expect("utf8 path")).expect("local");
        let empty = PackageGraphIndex::from_facts(&[]);
        let parsed = dependencies(&[], &empty, &package).expect("parsed");
        let DependencyFacts::Known(rows) = &parsed else {
            panic!("parsed dependencies should be known");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "serde");
        assert_eq!(rows[0].target.requirement.as_str(), "1");

        let coordinate = PackageReference::parse("pkg:cargo/demo@1.0.0").expect("purl");
        let local_source =
            super::super::local_manifest::dependency_source_key(&root, coordinate.clone());
        let PackageGraphSourceAuthority::Local(local_frontier) = local_source.authority else {
            panic!("local manifest source should carry a local authority");
        };
        let resident = PackageDependencyRecord::new_with_source_authority(
            coordinate.clone(),
            local_source.authority,
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "from-index", None)
                .expect("target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::LocalManifest,
                frontier: local_frontier,
                provenance: [8; 32],
            },
        );
        let facts = [(
            local_source,
            DependencyFacts::Known(vec![resident].into_boxed_slice()),
        )];
        let index = PackageGraphIndex::from_facts(&facts);
        let reused = dependencies(&facts, &index, &package).expect("reused");
        let DependencyFacts::Known(rows) = &reused else {
            panic!("indexed dependencies should be known");
        };
        assert_eq!(rows[0].target.requirement.as_str(), "from-index");

        let unattributed = PackageDependencyRecord::new(
            coordinate.clone(),
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "unbound", None)
                .expect("target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [10; 32],
                provenance: [11; 32],
            },
        );
        let unbound_facts = [(
            PackageGraphSourceKey::unattributed(coordinate.clone()),
            DependencyFacts::Known(vec![unattributed].into_boxed_slice()),
        )];
        let unbound_index = PackageGraphIndex::from_facts(&unbound_facts);
        assert!(matches!(
            dependencies(&unbound_facts, &unbound_index, &coordinate).expect("unbound"),
            DependencyFacts::Unavailable(_)
        ));

        let missing = PackageReference::parse("pkg:cargo/absent@1.0.0").expect("purl");
        let unavailable = dependencies(&[], &empty, &missing).expect("missing");
        assert!(matches!(unavailable, DependencyFacts::Unavailable(_)));
        let file = root.join("not-a-directory");
        fs::write(&file, "x").expect("file");
        let file_ref = PackageReference::parse(file.to_str().expect("utf8 path")).expect("file");
        let file_deps = dependencies(&[], &empty, &file_ref).expect("file deps");
        assert!(matches!(file_deps, DependencyFacts::Unavailable(_)));

        let mut body = String::from(
            "\
[package]
name = \"demo\"
version = \"1.0.0\"
edition = \"2021\"

[dependencies]
",
        );
        for index in 0..1_024 {
            body.push_str(&format!("dep{index} = \"1\"\n"));
        }
        fs::write(root.join("Cargo.toml"), body).expect("wide manifest");
        let mut parsed_ns = Vec::with_capacity(9);
        let mut reused_ns = Vec::with_capacity(9);
        for _ in 0..2 {
            let _ = dependencies(&[], &empty, &package).expect("warmup parse");
            let _ = dependencies(&facts, &index, &package).expect("warmup reuse");
        }
        for _ in 0..9 {
            let started = std::time::Instant::now();
            let parsed = dependencies(&[], &empty, &package).expect("wide parse");
            parsed_ns.push(started.elapsed().as_nanos());
            let DependencyFacts::Known(rows) = parsed else {
                panic!("wide parse should be known");
            };
            assert_eq!(rows.len(), 1_024);
            let started = std::time::Instant::now();
            let reused = dependencies(&facts, &index, &package).expect("wide reuse");
            reused_ns.push(started.elapsed().as_nanos());
            let DependencyFacts::Known(rows) = reused else {
                panic!("wide reuse should be known");
            };
            assert_eq!(rows[0].target.requirement.as_str(), "from-index");
        }
        parsed_ns.sort_unstable();
        reused_ns.sort_unstable();
        eprintln!(
            "local dependency parse median {} ns; indexed coordinate reuse median {} ns",
            parsed_ns[parsed_ns.len() / 2],
            reused_ns[reused_ns.len() / 2]
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Builds a view whose `RowId::Package` rows are indexed local directories,
    /// one per `project_root`, mirroring how a non-registry (path-indexed)
    /// project is published: `row.label` is the directory path itself.
    fn package_view(recipe: &[u8], project_roots: &[&Path]) -> ViewRoot {
        let root = backend_engine::view_state_root(&[]);
        let basis = backend_engine::Basis::new(root, backend_engine::object_version(b"source"));
        let rows = project_roots
            .iter()
            .map(|project_root| {
                let label = project_root.to_str().expect("utf8 fixture path").to_owned();
                Row::new(
                    RowId::Package(backend_engine::package_key(&label)),
                    basis,
                    label,
                )
            })
            .collect();
        ViewRoot::new_incomplete(
            backend_engine::view_key(recipe),
            basis,
            backend_engine::Frontier::new(basis.branch, basis.log, basis.schema, root, 0),
            rows,
            Vec::new(),
        )
        .expect("view with package rows")
    }

    fn write_toml_manifest(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "\
[package]
name = \"toml\"
version = \"0.8.23\"
edition = \"2021\"
",
        )
        .expect("real manifest");
    }

    #[test]
    fn indexed_semantic_versions_never_invents_a_generation_from_a_manifest() {
        let poisoned_root = fixture("no-manifest-sibling-versions");
        let real_root = fixture("toml-real-versions");
        write_toml_manifest(&real_root);

        let view = package_view(
            b"indexed-semantic-versions-poisoned-sibling",
            &[&poisoned_root, &real_root],
        );
        let package = PackageReference::parse("pkg:cargo/toml@0.8.23").expect("query package");

        let versions = indexed_semantic_versions(&view, &package, None)
            .expect("manifest-only projects cannot create a semantic generation");
        assert!(versions.is_empty(), "no compiler generation was selected");

        let _ = fs::remove_dir_all(poisoned_root);
        let _ = fs::remove_dir_all(real_root);
    }

    #[test]
    fn indexed_package_records_skips_a_manifest_less_sibling() {
        let poisoned_root = fixture("no-manifest-sibling-records");
        let real_root = fixture("toml-real-records");
        write_toml_manifest(&real_root);

        let view = package_view(
            b"indexed-package-records-poisoned-sibling",
            &[&poisoned_root, &real_root],
        );
        let package = PackageReference::parse("pkg:cargo/toml@0.8.23").expect("query package");

        let records = indexed_package_records(&view, &package, None).expect(
            "a sibling project with no supported manifest must not fail an unrelated \
             package's records query",
        );

        assert_eq!(
            records.len(),
            1,
            "expected only the real toml package; the poisoned sibling must be skipped"
        );
        assert_eq!(records[0].name.as_str(), "toml");
        assert_eq!(records[0].version.as_str(), "0.8.23");
        assert_eq!(records[0].coordinate.as_str(), "pkg:cargo/toml@0.8.23");

        let _ = fs::remove_dir_all(poisoned_root);
        let _ = fs::remove_dir_all(real_root);
    }

    #[test]
    fn indexed_package_coordinates_skips_a_manifest_less_sibling() {
        let poisoned_root = fixture("no-manifest-sibling-coordinates");
        let real_root = fixture("toml-real-coordinates");
        write_toml_manifest(&real_root);

        let view = package_view(
            b"indexed-package-coordinates-poisoned-sibling",
            &[&poisoned_root, &real_root],
        );
        let workspace = fixture("workspace-coordinates");

        let coordinates = indexed_package_coordinates(&view, &workspace).expect(
            "a sibling project with no supported manifest must not fail coordinate enumeration",
        );

        assert_eq!(
            coordinates,
            BTreeSet::from(["pkg:cargo/toml@0.8.23".to_owned()])
        );

        let _ = fs::remove_dir_all(poisoned_root);
        let _ = fs::remove_dir_all(real_root);
        let _ = fs::remove_dir_all(workspace);
    }
}
