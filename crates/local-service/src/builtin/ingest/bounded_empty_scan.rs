//! Prospectively metered producer for a narrowly shaped empty native scan.
//!
//! This produces source-scan evidence, not compiler authority or a Project
//! after-image. The caller must construct the normal Project row and pair the
//! walk with the real compiler host before any retirement can be granted.
use super::{
    bounded_ambient, bounded_empty_inventory, AmbientIgnoreWitness, CompilerFileRevision,
    CompilerRevisionFence, CompilerWorkspaceEntry, CompilerWorkspaceEntryKind, EmptyNativeTsWalk,
    Path, PathBuf, SourceAdmissionPolicy,
};
use backend_version::{
    retirement_budget::{Metered, RetirementBudget},
    ObjectKeyHasher, Schema, SchemaIdentity,
};
use std::{
    mem::size_of,
    sync::{atomic::AtomicBool, Arc},
};

/// A complete scan of the explicitly supported tiny empty-tree subset.
/// Its nested walk remains charged to the caller's ledger for its lifetime.
pub(crate) struct BoundedEmptyNativeScan {
    walk: Metered<EmptyNativeTsWalk>,
    source_policy: [u8; 32],
}

impl BoundedEmptyNativeScan {
    pub(crate) fn walk(&self) -> &Metered<EmptyNativeTsWalk> {
        &self.walk
    }
    pub(crate) fn source_version(&self) -> [u8; 32] {
        self.walk.get().project_source_version()
    }
    pub(crate) fn source_policy(&self) -> [u8; 32] {
        self.source_policy
    }
    /// The normal scan observes zero unsupported CSS, SQL, and Vue files.
    pub(crate) fn source_selection_counts(&self) -> (u64, u64, u64) {
        (0, 0, 0)
    }
}

/// Construct the same source version as the normal retained-native scan when
/// the unfiltered tree contains only `package.json` and optionally an empty
/// `src` directory, every ambient ignore input is absent, and the startup
/// discovery policy remains bound. All other trees decline this narrow route.
pub(super) fn capture(
    root: &Path,
    project: [u8; 32],
    source_policy: SourceAdmissionPolicy,
    cancellation: &Arc<AtomicBool>,
    budget: &RetirementBudget,
) -> Result<Metered<BoundedEmptyNativeScan>, &'static str> {
    if !bounded_empty_inventory::startup_proc_binding_valid() {
        return Err("empty scan startup proc binding unavailable");
    }
    if SourceAdmissionPolicy::startup() != Some(source_policy) {
        return Err("empty scan source policy is not startup bound");
    }
    let root_text = root.to_str().ok_or("empty scan root is not UTF-8")?;
    // `PackageKey::from_value` allocates a canonical preimage. Stream the same
    // package schema framing so no allocation occurs before admission.
    type PackageSchema = backend_library::canonical::PackageSchema;
    let schema = SchemaIdentity::new(
        PackageSchema::DOMAIN,
        PackageSchema::TYPE,
        PackageSchema::VERSION,
    );
    let mut key = ObjectKeyHasher::new(schema, root_text.len())
        .map_err(|_| "empty scan project identity overflow")?;
    key.update(root_text.as_bytes())
        .map_err(|_| "empty scan project identity overflow")?;
    if key
        .finish_key::<PackageSchema>()
        .map_err(|_| "empty scan project identity overflow")?
        .to_bytes()
        != project
    {
        return Err("empty scan project does not bind root");
    }
    let root_revision = bounded_empty_inventory::canonical_directory_revision(root)?;
    let capability = bounded_empty_inventory::open_root(root, root)?;
    let census = bounded_empty_inventory::capture_strict_empty(&capability, cancellation)?;
    if root_revision != census.root {
        return Err("empty scan root changed");
    }
    drop(capability);
    let count = 2usize + usize::from(census.src.is_some());
    let dirs = 1usize + usize::from(census.src.is_some());
    // Each term corresponds to a real retained allocation or owned inline
    // value. The 64-byte allowance is the owner's per-allocation bookkeeping
    // policy, applied separately; no guessed global scan allowance is used.
    let mut charge = size_of::<EmptyNativeTsWalk>();
    fn allocation(total: &mut usize, requested: usize) -> Result<(), &'static str> {
        *total = total
            .checked_add(requested)
            .and_then(|n| n.checked_add(64))
            .ok_or("empty scan allocation overflow")?;
        Ok(())
    }
    for _ in 0..3 {
        allocation(&mut charge, root.as_os_str().as_encoded_bytes().len())?;
    }
    allocation(
        &mut charge,
        dirs.checked_mul(size_of::<(PathBuf, Option<super::FileSystemRevision>)>())
            .ok_or("empty scan overflow")?,
    )?;
    allocation(&mut charge, size_of::<CompilerFileRevision>())?;
    allocation(&mut charge, "package.json".len())?;
    allocation(
        &mut charge,
        dirs.checked_mul(size_of::<PathBuf>())
            .ok_or("empty scan overflow")?,
    )?;
    allocation(
        &mut charge,
        count
            .checked_mul(size_of::<CompilerWorkspaceEntry>())
            .ok_or("empty scan overflow")?,
    )?;
    allocation(&mut charge, "package.json".len())?;
    if census.src.is_some() {
        for _ in 0..3 {
            allocation(&mut charge, "src".len())?;
        }
    }
    allocation(
        &mut charge,
        size_of::<AtomicBool>() + 2 * size_of::<usize>(),
    )?;
    let outer = budget
        .reserve(size_of::<BoundedEmptyNativeScan>())
        .map_err(|_| "empty scan budget")?;
    let reservation = budget.reserve(charge).map_err(|_| "empty scan budget")?;
    let mut fence_directories = Vec::with_capacity(dirs);
    fence_directories.push((PathBuf::new(), Some(census.root)));
    if let Some(src) = census.src {
        fence_directories.push((PathBuf::from("src"), Some(src)));
    }
    let fence = CompilerRevisionFence {
        requested_root: root.to_owned(),
        root: root.to_owned(),
        directories: fence_directories,
        files: vec![CompilerFileRevision {
            relative_path: PathBuf::from("package.json"),
            metadata: Some(census.package),
        }],
        typescript_reuse: Vec::new(),
    };
    let mut directories = Vec::with_capacity(dirs);
    directories.push(PathBuf::new());
    if census.src.is_some() {
        directories.push(PathBuf::from("src"));
    }
    let mut entries = Vec::with_capacity(count);
    entries.push(CompilerWorkspaceEntry {
        path: String::new(),
        kind: CompilerWorkspaceEntryKind::Directory,
        byte_length: None,
        revision: census.root,
    });
    entries.push(CompilerWorkspaceEntry {
        path: "package.json".to_owned(),
        kind: CompilerWorkspaceEntryKind::File,
        byte_length: Some(census.package.length),
        revision: census.package,
    });
    if let Some(src) = census.src {
        entries.push(CompilerWorkspaceEntry {
            path: "src".to_owned(),
            kind: CompilerWorkspaceEntryKind::Directory,
            byte_length: None,
            revision: src,
        });
    }
    let ambient_digest = bounded_ambient::hash_absent_startup_policy_paths(
        root,
        &directories,
        budget,
        64,
        cancellation,
    )?;
    let mut options_len = Some(0usize);
    if !backend_library::visit_startup_bound_source_policy_identity(|bytes| {
        options_len = options_len.and_then(|len| len.checked_add(bytes.len()));
    }) {
        return Err("startup discovery identity unavailable");
    }
    let options_len = options_len.ok_or("discovery identity length overflow")?;
    let mut discovery = blake3::Hasher::new();
    discovery.update(b"nudox.native-typescript-fixed-source-selection-local-incomplete.v1\0");
    discovery.update(
        &(u64::try_from(options_len).map_err(|_| "discovery identity length overflow")?)
            .to_be_bytes(),
    );
    if !backend_library::visit_startup_bound_source_policy_identity(|bytes| {
        discovery.update(bytes);
    }) {
        return Err("startup discovery identity unavailable");
    }
    discovery.update(&source_policy.identity());
    discovery.update(&0u64.to_be_bytes());
    let discovery = discovery.finalize();
    let mut empty_sources = blake3::Hasher::new();
    empty_sources.update(b"nudox.native-typescript-source-set.v1\0");
    empty_sources.update(&0u64.to_be_bytes());
    let mut source = blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    source.update(b"nudox.native-typescript-source-set.v1\0");
    source.update(empty_sources.finalize().as_bytes());
    source.update(b"nudox.native-typescript-input-basis.v2\0");
    source.update(discovery.as_bytes());
    source.update(&[1]);
    source.update(&ambient_digest);
    // No source languages are relevant, so normal config completeness is false
    // and it retains no TypeScript configuration rows, even with package.json.
    source.update(&[0]);
    source.update(&0u64.to_be_bytes());
    let version = *source.finalize().as_bytes();
    let walk = EmptyNativeTsWalk {
        project,
        root: root.to_owned(),
        project_source_version: version,
        fence,
        ambient: AmbientIgnoreWitness {
            digest: ambient_digest,
            directories,
            startup_policy_bound: true,
        },
        require_absent_ambient: true,
        entries,
        cancellation: Arc::clone(cancellation),
    };
    let walk = reservation.meter(walk);
    walk.get().revalidate_bounded_inventory(budget)?;
    Ok(outer.meter(BoundedEmptyNativeScan {
        walk,
        source_policy: source_policy.identity(),
    }))
}
