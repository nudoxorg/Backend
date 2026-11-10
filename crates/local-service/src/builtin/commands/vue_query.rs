//! Read-only Vue syntax queries. This module is not registered until the
//! workspace owner validates the selected Vue relation and replay path.
use super::super::{BuiltinModelError, BuiltinWorkspaceRelation};
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{vue_inventory_relation, vue_inventory_root_pointer_key},
};
use backend_frontend_vue::{
    VueInventoryRecord, VueInventoryRelation, VueInventoryRootSchema, vue_selection_key,
};
use backend_library::vue_query::*;
use backend_version::{Relation, Schema, SchemaIdentity};

fn error(message: impl std::fmt::Display) -> BuiltinModelError {
    BuiltinModelError(format!("Vue query: {message}"))
}
type RelationHandle = backend_engine::workspace::WorkspaceRelationHandle<VueInventoryRelation>;
struct Selected {
    relation: RelationHandle,
    project: [u8; 32],
    root: [u8; 32],
    manifest: [u8; 32],
    closure: [u8; 32],
    source_root: String,
    basis: [u8; 32],
    unavailable: u32,
    truncated: bool,
}
fn select(
    snapshot: &WorkspaceSnapshot,
    package: &str,
) -> Result<Option<Selected>, BuiltinModelError> {
    let project = backend_library::package_key(package).to_bytes();
    let source = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(error)?;
    if !source
        .lookup(&project)
        .map_err(error)?
        .as_ref()
        .and_then(|r| r.project_fields())
        .is_some_and(|p| p.label == package)
    {
        return Ok(None);
    }
    let Some(relation) = vue_inventory_relation(snapshot).map_err(error)? else {
        return Ok(None);
    };
    let schema = SchemaIdentity::new(
        VueInventoryRootSchema::DOMAIN,
        VueInventoryRootSchema::TYPE,
        VueInventoryRootSchema::VERSION,
    );
    let pointer_key = vue_inventory_root_pointer_key();
    let pointer = snapshot
        .closure()
        .control_manifest()
        .objects()
        .iter()
        .find(|o| o.schema() == schema && o.key() == &pointer_key)
        .ok_or_else(|| error("selected Vue pointer missing"))?;
    if !snapshot
        .closure()
        .contains(pointer.id())
        .map_err(|e| error(format!("{e:?}")))?
    {
        return Err(error("Vue pointer outside closure"));
    }
    let root_object = relation.root_object().map_err(error)?;
    if !snapshot
        .closure()
        .contains(root_object.id())
        .map_err(|e| error(format!("{e:?}")))?
    {
        return Err(error("Vue root outside closure"));
    }
    let key = vue_selection_key(project);
    let Some(selection) = relation.lookup(&key).map_err(error)? else {
        return Ok(None);
    };
    if selection.key() != key {
        return Err(error("Vue selection key differs"));
    }
    let VueInventoryRecord::Selection {
        project: owner,
        source_root,
        basis,
        producer,
        unavailable,
        walk_truncated,
        ..
    } = selection
    else {
        return Err(error("Vue selection row has wrong kind"));
    };
    if producer != backend_frontend_vue::support_fingerprint() {
        return Err(error("Vue producer differs"));
    }
    if owner != project {
        return Err(error("Vue selection owner differs"));
    }
    Ok(Some(Selected {
        root: *relation.root().as_bytes(),
        manifest: *snapshot.root().as_bytes(),
        closure: *snapshot.closure().binding().closure().as_bytes(),
        relation,
        project,
        source_root,
        basis,
        unavailable,
        truncated: walk_truncated,
    }))
}
fn coverage() -> VueCoverage {
    VueCoverage {
        script: VueCapability::Partial,
        template: VueCapability::Unavailable,
        style: VueCapability::Unavailable,
    }
}
fn empty(snapshot: &WorkspaceSnapshot, project: [u8; 32]) -> VueQueryPage {
    VueQueryPage {
        manifest_root: *snapshot.root().as_bytes(),
        vue_root: [0; 32],
        closure: *snapshot.closure().binding().closure().as_bytes(),
        project,
        unavailable_hosts: 0,
        stale_hosts: 0,
        removed_hosts: 0,
        selection_policy: VueSelectionPolicy::Unknown,
        walk_truncated: false,
        coverage: VueCoverage {
            script: VueCapability::Unavailable,
            template: VueCapability::Unavailable,
            style: VueCapability::Unavailable,
        },
        hits: Vec::new(),
        terminal: VueQueryTerminal::Unavailable,
    }
}
fn cursor(
    selected: &Selected,
    request: &VueQueryRequest,
    host: [u8; 32],
    next_site: u32,
    secret: &[u8; 32],
) -> Result<VueQueryCursor, BuiltinModelError> {
    VueQueryCursor::issue(
        VueCursorClaims {
            manifest_root: selected.manifest,
            closure: selected.closure,
            vue_root: selected.root,
            project: selected.project,
            role: request.role,
            name: request.name.clone(),
            limit: request.limit,
            scan_rows: request.scan_rows,
            source_bytes: request.source_bytes,
            output_bytes: request.output_bytes,
            after_host: host,
            next_site,
        },
        secret,
    )
    .map_err(error)
}
/// Execute a bounded host-local syntax query against an owner-selected snapshot.
pub(super) fn query(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    request: &VueQueryRequest,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<VueQueryPage, BuiltinModelError> {
    use backend_frontend_vue::ExecutionCheckpoint as _;
    let budget = backend_frontend_vue::ProjectExecutionBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(10),
        cancelled,
        5_000_000,
    );
    budget
        .checkpoint(1)
        .map_err(|_| error("Vue query stopped"))?;
    request.admit().map_err(error)?;
    let project = backend_library::package_key(request.package.as_str()).to_bytes();
    let Some(selected) = select(snapshot, request.package.as_str())? else {
        if request.continuation.is_some() {
            return Err(error("continuation expired"));
        }
        return Ok(empty(snapshot, project));
    };
    let mut after = None;
    let mut resume = None;
    if let Some(token) = &request.continuation {
        let claims = token.verify(secret).map_err(error)?;
        if claims.manifest_root != selected.manifest
            || claims.closure != selected.closure
            || claims.vue_root != selected.root
            || claims.project != selected.project
            || claims.name != request.name
            || claims.role != request.role
            || claims.limit != request.limit
            || claims.scan_rows != request.scan_rows
            || claims.source_bytes != request.source_bytes
            || claims.output_bytes != request.output_bytes
        {
            return Err(error("continuation expired or mismatched"));
        }
        if claims.next_site > 0 {
            resume = Some((claims.after_host, (claims.next_site - 1) as usize));
        } else {
            after = Some(claims.after_host);
        }
    }
    let mut result = VueQueryPage {
        manifest_root: selected.manifest,
        vue_root: selected.root,
        closure: selected.closure,
        project,
        unavailable_hosts: selected.unavailable,
        stale_hosts: 0,
        removed_hosts: 0,
        selection_policy: VueSelectionPolicy::Unknown,
        walk_truncated: selected.truncated,
        coverage: coverage(),
        hits: Vec::new(),
        terminal: VueQueryTerminal::Complete,
    };
    let mut rows = 0u32;
    let mut row_bytes = 0usize;
    let mut source_bytes = 0usize;
    let mut output_work = serde_json::to_vec(&result).map_err(error)?.len();
    loop {
        budget
            .checkpoint(1)
            .map_err(|_| error("Vue query stopped"))?;
        if rows == request.scan_rows {
            result.terminal = VueQueryTerminal::More {
                continuation: cursor(
                    &selected,
                    request,
                    after.ok_or_else(|| error("scan budget made no progress"))?,
                    0,
                    secret,
                )?,
            };
            break;
        }
        let (key, row, start) = if let Some((key, start)) = resume.take() {
            let row = selected
                .relation
                .lookup(&key)
                .map_err(error)?
                .ok_or_else(|| error("cursor host missing"))?;
            (key, row, start)
        } else {
            let page = selected.relation.page(after.as_ref(), 1).map_err(error)?;
            let Some((key, row)) = page.entries().first() else {
                break;
            };
            (*key, row.clone(), 0)
        };
        rows = rows
            .checked_add(1)
            .ok_or_else(|| error("row work overflow"))?;
        if row.key() != key {
            return Err(error("Vue row key differs"));
        }
        let mut encoded = Vec::new();
        VueInventoryRelation::encode_value(&row, &mut encoded);
        let next_bytes = row_bytes
            .checked_add(encoded.len())
            .ok_or_else(|| error("Vue scan byte overflow"))?;
        if next_bytes > 64 * 1024 * 1024 {
            result.terminal = VueQueryTerminal::More {
                continuation: cursor(&selected, request, key, 1, secret)?,
            };
            break;
        }
        row_bytes = next_bytes;
        after = Some(key);
        let VueInventoryRecord::Host(host) = row else {
            continue;
        };
        let (owner, path) = host.owner();
        if owner != selected.project {
            continue;
        }
        if host.analysis_basis() != selected.basis {
            return Err(error("Vue host basis differs"));
        }
        let size = host.byte_length();
        if source_bytes
            .checked_add(size)
            .is_none_or(|v| v > request.source_bytes as usize)
        {
            result.terminal = VueQueryTerminal::More {
                continuation: cursor(
                    &selected,
                    request,
                    key,
                    u32::try_from(start + 1).map_err(error)?,
                    secret,
                )?,
            };
            break;
        }
        source_bytes += size;
        let bytes = match super::super::ingest::read_stylo_selected_source(
            std::path::Path::new(&selected.source_root),
            path,
            size,
        ) {
            Ok(bytes) => bytes,
            Err(super::super::ingest::SelectedSourceReadError::Vanished) => {
                result.removed_hosts = result.removed_hosts.saturating_add(1);
                continue;
            }
            Err(super::super::ingest::SelectedSourceReadError::TooLarge) => {
                result.stale_hosts = result.stale_hosts.saturating_add(1);
                continue;
            }
            Err(super::super::ingest::SelectedSourceReadError::Invalid) => {
                return Err(error("invalid confined Vue source path"));
            }
            Err(_) => {
                result.unavailable_hosts = result.unavailable_hosts.saturating_add(1);
                continue;
            }
        };
        if !host.matches_host_bytes(&bytes) {
            result.stale_hosts = result.stale_hosts.saturating_add(1);
            continue;
        }
        let verified = host
            .verify_host_with_checkpoint(
                &bytes,
                key,
                selected.project,
                path,
                selected.basis,
                &budget,
            )
            .map_err(|_| error("Vue inventory failed host replay"))?;
        let count = match request.role {
            VueQueryRole::Declaration => verified.declaration_count(),
            VueQueryRole::LocalCall => verified.call_count(),
        };
        if start > count {
            return Err(error("cursor site out of range"));
        }
        for index in start..count {
            let (site, matched) = match request.role {
                VueQueryRole::Declaration => {
                    let d = verified
                        .declaration(index)
                        .ok_or_else(|| error("invalid declaration range"))?;
                    let matched = d.name.text() == request.name;
                    (d.name, matched)
                }
                VueQueryRole::LocalCall => {
                    let c = verified
                        .call(index)
                        .ok_or_else(|| error("invalid call range"))?;
                    let matched = c.target.text() == request.name;
                    (c.site, matched)
                }
            };
            if !matched {
                continue;
            }
            let source = VueSourceReference::issue(
                VueSourceClaims {
                    manifest_root: selected.manifest,
                    closure: selected.closure,
                    vue_root: selected.root,
                    project,
                    host: key,
                    role: request.role,
                    site: u32::try_from(index).map_err(error)?,
                },
                secret,
            )
            .map_err(error)?;
            let hit = VueQueryHit {
                selection_policy: VueSelectionPolicy::Unknown,
                name: site.text().to_owned(),
                path: path.to_owned(),
                source,
            };
            let hit_bytes = serde_json::to_vec(&hit).map_err(error)?.len();
            if result.hits.len() >= request.limit as usize
                || output_work.saturating_add(hit_bytes).saturating_add(4096)
                    > request.output_bytes as usize
            {
                result.terminal = VueQueryTerminal::More {
                    continuation: cursor(
                        &selected,
                        request,
                        key,
                        u32::try_from(index + 1).map_err(error)?,
                        secret,
                    )?,
                };
                budget
                    .checkpoint(1)
                    .map_err(|_| error("Vue query stopped"))?;
                return Ok(result);
            }
            output_work = output_work
                .checked_add(hit_bytes)
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| error("output work overflow"))?;
            result.hits.push(hit);
        }
    }
    budget
        .checkpoint(1)
        .map_err(|_| error("Vue query stopped"))?;
    Ok(result)
}
/// Reopen and verify one owner-sealed Vue source locator.
pub(super) fn read_source(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    package: &str,
    reference: &VueSourceReference,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<VueSourceRead, BuiltinModelError> {
    let budget = backend_frontend_vue::ProjectExecutionBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(10),
        cancelled,
        5_000_000,
    );
    use backend_frontend_vue::ExecutionCheckpoint as _;
    budget
        .checkpoint(1)
        .map_err(|_| error("Vue source read stopped"))?;
    let claims = reference.verify(secret).map_err(error)?;
    let Some(selected) = select(snapshot, package)? else {
        return Ok(VueSourceRead::Expired);
    };
    if claims.project != selected.project {
        return Ok(VueSourceRead::Foreign);
    }
    if claims.manifest_root != selected.manifest
        || claims.closure != selected.closure
        || claims.vue_root != selected.root
    {
        return Ok(VueSourceRead::Expired);
    }
    let Some(row) = selected.relation.lookup(&claims.host).map_err(error)? else {
        return Ok(VueSourceRead::Expired);
    };
    if row.key() != claims.host {
        return Err(error("Vue source host key differs"));
    }
    let VueInventoryRecord::Host(host) = row else {
        return Ok(VueSourceRead::Expired);
    };
    let (owner, path) = host.owner();
    if owner != selected.project || host.analysis_basis() != selected.basis {
        return Err(error("Vue source owner or basis differs"));
    }
    budget
        .checkpoint(1)
        .map_err(|_| error("Vue source read stopped"))?;
    let bytes = match super::super::ingest::read_stylo_selected_source(
        std::path::Path::new(&selected.source_root),
        path,
        host.byte_length(),
    ) {
        Ok(bytes) => bytes,
        Err(super::super::ingest::SelectedSourceReadError::Vanished) => {
            return Ok(VueSourceRead::SourceRemoved);
        }
        Err(super::super::ingest::SelectedSourceReadError::TooLarge) => {
            return Ok(VueSourceRead::SourceChanged);
        }
        Err(super::super::ingest::SelectedSourceReadError::Invalid) => {
            return Err(error("invalid confined Vue source path"));
        }
        Err(_) => return Ok(VueSourceRead::BytesNotRetained),
    };
    if !host.matches_host_bytes(&bytes) {
        return Ok(VueSourceRead::SourceChanged);
    }
    let verified = host
        .verify_host_with_checkpoint(
            &bytes,
            claims.host,
            selected.project,
            path,
            selected.basis,
            &budget,
        )
        .map_err(|_| error("Vue inventory failed host replay"))?;
    let extent = match claims.role {
        VueQueryRole::Declaration => verified.declaration(claims.site as usize).map(|d| d.name),
        VueQueryRole::LocalCall => verified.call(claims.site as usize).map(|c| c.site),
    };
    let Some(extent) = extent else {
        return Ok(VueSourceRead::Expired);
    };
    let range = extent.byte_range();
    let line = bytes[..range.start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        .saturating_add(1);
    Ok(VueSourceRead::Available {
        selection_policy: VueSelectionPolicy::Unknown,
        reference: reference.clone(),
        served_root: selected.source_root,
        path: path.to_owned(),
        start: range.start as u64,
        end: range.end as u64,
        line_1: u32::try_from(line).map_err(error)?,
        text: extent.text().to_owned(),
    })
}
