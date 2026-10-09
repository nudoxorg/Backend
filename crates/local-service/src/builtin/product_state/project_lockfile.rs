//! Filesystem adapter for the shared typed lockfile inventory.

use backend_engine::project_lockfile::{LockedMember, LockfileFormat, MAX_PROJECT_LOCKFILE_BYTES};
use backend_library::{PackageReference, ProductText, ProjectLockfileMembership};
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn read(
    path: &Path,
) -> Result<(Box<[PackageReference]>, ProjectLockfileMembership), String> {
    if !path.is_absolute() {
        return Err("lockfile path must be absolute".to_owned());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("unsupported lockfile name")?;
    let format = LockfileFormat::from_basename(name).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or("lockfile has no parent directory")?
        .canonicalize()
        .map_err(|error| format!("resolve lockfile parent: {error}"))?;
    let directory =
        backend_platform::directory::DirectoryCapability::open_read_only_source(&parent)
            .map_err(|error| format!("open lockfile parent: {error}"))?;
    let bytes = backend_platform::durable::read_regular_bounded_stable_at(
        &directory,
        name,
        MAX_PROJECT_LOCKFILE_BYTES,
    )
    .map_err(|error| format!("read lockfile: {error}"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "lockfile is not UTF-8")?;
    let inventory = format
        .parse(text)
        .map_err(|error| format!("{name}: {error}"))?;
    resolve(inventory, &parent, &directory)
}

pub(super) fn resolve(
    inventory: backend_engine::project_lockfile::LockfileInventory,
    parent: &Path,
    directory: &backend_platform::directory::DirectoryCapability,
) -> Result<(Box<[PackageReference]>, ProjectLockfileMembership), String> {
    directory
        .verify_path(parent)
        .map_err(|error| format!("lockfile parent changed: {error}"))?;
    let mut members = BTreeSet::new();
    let mut unresolved = inventory.unresolved.into_vec();
    for member in inventory.members {
        members.insert(match member {
            LockedMember::Package(package) => package,
            LockedMember::Workspace(relative) => {
                let root = match parent.join(&relative).canonicalize() {
                    Ok(root) => root,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        unresolved.push(unresolved_local(&relative, "locked local package is absent; add or restore its manifest path")?);
                        continue;
                    }
                    Err(error) => return Err(format!("resolve locked local package: {error}")),
                };
                if !root.starts_with(parent) {
                    return Err(
                        "locked local package escapes the lockfile parent through a symlink"
                            .to_owned(),
                    );
                }
                // Require a real supported manifest; a versionless workspace
                // link must not become an invented registry release.
                if super::super::local_manifest::read_local_manifest(&root)?.is_none() {
                    unresolved.push(unresolved_local(&relative, "locked local package has no statically pinned manifest identity; add its local path explicitly")?);
                    continue;
                }
                let label = root
                    .to_str()
                    .ok_or("locked local package path is not UTF-8")?;
                PackageReference::Local(ProductText::new(label).map_err(|error| error.to_string())?)
            }
        });
    }
    // Path-based manifest discovery shares the held parent observation. These
    // fences detect replacement; they do not lock a hostile mutable namespace.
    directory
        .verify_path(parent)
        .map_err(|error| format!("lockfile parent changed: {error}"))?;
    let coverage = if unresolved.is_empty() {
        ProjectLockfileMembership::Complete
    } else {
        ProjectLockfileMembership::Partial {
            unresolved: unresolved.into_boxed_slice(),
        }
    };
    Ok((members.into_iter().collect(), coverage))
}

fn unresolved_local(
    path: &str,
    reason: &'static str,
) -> Result<backend_library::ProjectUnresolvedLockMember, String> {
    let path = ProductText::new(path).map_err(|error| error.to_string())?;
    Ok(backend_library::ProjectUnresolvedLockMember {
        name: path.clone(),
        version: None,
        source: Some(path),
        reason: ProductText::from_static(reason),
    })
}
