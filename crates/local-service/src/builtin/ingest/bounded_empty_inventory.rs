//! Linux descriptor-relative revalidation of an already captured empty inventory.
//!
//! This is one component of admission, not an independently sufficient proof:
//! the caller must bind the capture, policy, ambient inputs and revision fence.
use super::{
    file_system_revision_from_metadata, CompilerWorkspaceEntry, CompilerWorkspaceEntryKind,
    FileSystemRevision, ProjectRoot,
};
use backend_version::retirement_budget::RetirementBudget;
use rustix::fs::{openat, Mode, OFlags, RawDir, CWD};
use std::{
    io::Read,
    mem::MaybeUninit,
    os::fd::{AsFd, AsRawFd},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_DEPTH: usize = 32;
const MAX_PATH: usize = 4096;
const MAX_HOST_ANCESTORS: usize = backend_engine::application::TYPE_SCRIPT_PROJECT_ANCESTOR_LIMIT;
// Keep in sync with the normal TypeScript host's MAX_PACKAGE_MANIFEST_BYTES.
const MAX_PACKAGE_MANIFEST_BYTES: usize = 64 * 1024;

/// Metadata for the intentionally narrow, unfiltered empty-tree subset:
/// one root package manifest and at most one empty `src` directory.
pub(super) struct StrictEmptyInventory {
    pub(super) root: FileSystemRevision,
    pub(super) package: FileSystemRevision,
    pub(super) src: Option<FileSystemRevision>,
}

/// Discover this subset directly from a directory descriptor, without an
/// ignore walker or heap allocation. Any additional entry declines admission.
pub(super) fn capture_strict_empty(
    root: &ProjectRoot,
    cancelled: &AtomicBool,
) -> Result<StrictEmptyInventory, &'static str> {
    if cancelled.load(Ordering::Acquire) {
        return Err("empty scan cancelled");
    }
    let before = file_system_revision_from_metadata(
        &root.directory.metadata().map_err(|_| "stat scan root")?,
    );
    let mut package = None;
    let mut src = None;
    let mut buffer = [MaybeUninit::<u8>::uninit(); 8192];
    let mut entries = RawDir::new(&root.directory, &mut buffer);
    while let Some(entry) = entries.next() {
        if cancelled.load(Ordering::Acquire) {
            return Err("empty scan cancelled");
        }
        let entry = entry.map_err(|_| "read scan root")?;
        let name = entry.file_name();
        match name.to_bytes() {
            b"." | b".." => {}
            b"package.json" if package.is_none() => {
                let held = openat(
                    &root.directory,
                    name,
                    OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| "open package identity")?;
                let file = std::fs::File::from(held);
                let metadata = file.metadata().map_err(|_| "stat package identity")?;
                if !metadata.is_file() || metadata.len() > MAX_PACKAGE_MANIFEST_BYTES as u64 {
                    return Err("unsupported package manifest");
                }
                package = Some(file_system_revision_from_metadata(&metadata));
            }
            b"src" if src.is_none() => {
                let held = openat(
                    &root.directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| "open source directory")?;
                let directory = std::fs::File::from(held);
                let first = file_system_revision_from_metadata(
                    &directory.metadata().map_err(|_| "stat source directory")?,
                );
                let mut child_buffer = [MaybeUninit::<u8>::uninit(); 8192];
                let mut children = RawDir::new(&directory, &mut child_buffer);
                while let Some(child) = children.next() {
                    if cancelled.load(Ordering::Acquire) {
                        return Err("empty scan cancelled");
                    }
                    let child = child.map_err(|_| "read source directory")?;
                    if !matches!(child.file_name().to_bytes(), b"." | b"..") {
                        return Err("source directory is not empty");
                    }
                }
                let last = file_system_revision_from_metadata(
                    &directory
                        .metadata()
                        .map_err(|_| "restat source directory")?,
                );
                if first != last {
                    return Err("source directory changed");
                }
                src = Some(first);
            }
            _ => return Err("unsupported empty scan entry"),
        }
    }
    let after = file_system_revision_from_metadata(
        &root.directory.metadata().map_err(|_| "restat scan root")?,
    );
    if before != after || cancelled.load(Ordering::Acquire) {
        return Err("scan root changed");
    }
    Ok(StrictEmptyInventory {
        root: before,
        package: package.ok_or("package manifest absent")?,
        src,
    })
}

/// A strict no-workspace-marker ancestor view for one canonical home boundary.
/// This does not prove that a compiler host actually selected that boundary;
/// the caller must bind it to the host capability and revalidate at grant.
pub(crate) struct AbsentAncestorWorkspaces {
    root: std::path::PathBuf,
    home: Option<std::path::PathBuf>,
    home_revision: Option<FileSystemRevision>,
    revisions: Vec<FileSystemRevision>,
}

impl AbsentAncestorWorkspaces {
    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }
    pub(crate) fn home(&self) -> Option<&std::path::Path> {
        self.home.as_deref()
    }

    pub(crate) fn validate(&self, cancelled: &AtomicBool) -> Result<(), &'static str> {
        match (self.home.as_deref(), self.home_revision) {
            (None, None) => {}
            (Some(home), Some(expected)) if canonical_directory_revision(home)? == expected => {}
            _ => return Err("home boundary changed"),
        }
        let mut index = 0usize;
        if let Some(home) = self.home.as_deref() {
            for ancestor in self.root.ancestors().take(MAX_HOST_ANCESTORS).skip(1) {
                if ancestor == home {
                    break;
                }
                let expected = self
                    .revisions
                    .get(index)
                    .ok_or("ancestor inventory incomplete")?;
                let observed = absent_workspace_markers(ancestor, cancelled)?;
                if &observed != expected {
                    return Err("ancestor workspace changed");
                }
                index = index.checked_add(1).ok_or("ancestor count overflow")?;
            }
        }
        if index != self.revisions.len() {
            return Err("ancestor inventory changed");
        }
        if let (Some(home), Some(expected)) = (self.home.as_deref(), self.home_revision) {
            if canonical_directory_revision(home)? != expected {
                return Err("home boundary changed");
            }
        }
        Ok(())
    }
}

// A normalized absolute spelling whose components open with NOFOLLOW has the
// same path as normal canonicalization, without allocating an unbounded
// canonicalize result. Symlinked homes and noncanonical spellings decline.
pub(super) fn canonical_directory_revision(
    path: &std::path::Path,
) -> Result<FileSystemRevision, &'static str> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() || bytes.len() > MAX_PATH || bytes[0] != b'/' || bytes.contains(&0) {
        return Err("unsupported canonical directory");
    }
    if bytes != b"/"
        && bytes[1..]
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err("noncanonical directory spelling");
    }
    let directory = open_root(path, path)?.directory;
    Ok(file_system_revision_from_metadata(
        &directory
            .metadata()
            .map_err(|_| "stat canonical directory")?,
    ))
}

fn absent_workspace_markers(
    path: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<FileSystemRevision, &'static str> {
    use rustix::fs::AtFlags;
    if cancelled.load(Ordering::Acquire) {
        return Err("ancestor workspace cancelled");
    }
    let directory = open_root(path, path)?.directory;
    let before =
        file_system_revision_from_metadata(&directory.metadata().map_err(|_| "stat ancestor")?);
    for name in [c"package.json", c"pnpm-workspace.yaml"] {
        match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => {}
            _ => return Err("ancestor workspace marker present or inaccessible"),
        }
    }
    let after =
        file_system_revision_from_metadata(&directory.metadata().map_err(|_| "restat ancestor")?);
    if before != after || cancelled.load(Ordering::Acquire) {
        return Err("ancestor workspace changed");
    }
    Ok(before)
}

/// Capture parent directories that the normal host would probe when no
/// workspace marker exists. A present marker declines this narrow route.
pub(super) fn capture_absent_ancestor_workspaces(
    root: &std::path::Path,
    home: Option<&std::path::Path>,
    budget: &RetirementBudget,
    bookkeeping: usize,
    cancelled: &AtomicBool,
) -> Result<backend_version::retirement_budget::Metered<AbsentAncestorWorkspaces>, &'static str> {
    if !root.is_absolute()
        || root.as_os_str().as_encoded_bytes().len() > MAX_PATH
        || home.is_some_and(|path| {
            !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_PATH
        })
    {
        return Err("unsupported workspace boundary");
    }
    let count = home.map_or(0, |home| {
        root.ancestors()
            .take(MAX_HOST_ANCESTORS)
            .skip(1)
            .take_while(|path| *path != home)
            .count()
    });
    let charge = std::mem::size_of::<AbsentAncestorWorkspaces>()
        .checked_add(root.as_os_str().as_encoded_bytes().len())
        .and_then(|n| {
            n.checked_add(home.map_or(0, |path| path.as_os_str().as_encoded_bytes().len()))
        })
        .and_then(|n| n.checked_add(count.checked_mul(std::mem::size_of::<FileSystemRevision>())?))
        .and_then(|n| n.checked_add(bookkeeping.checked_mul(3)?))
        .ok_or("ancestor workspace allocation overflow")?;
    let reservation = budget
        .reserve(charge)
        .map_err(|_| "ancestor workspace budget")?;
    let home_revision = home.map(canonical_directory_revision).transpose()?;
    let mut revisions = Vec::with_capacity(count);
    if let Some(home) = home {
        for ancestor in root.ancestors().take(MAX_HOST_ANCESTORS).skip(1) {
            if ancestor == home {
                break;
            }
            revisions.push(absent_workspace_markers(ancestor, cancelled)?);
        }
    }
    let fence = reservation.meter(AbsentAncestorWorkspaces {
        root: root.to_owned(),
        home: home.map(std::path::Path::to_owned),
        home_revision,
        revisions,
    });
    fence.get().validate(cancelled)?;
    Ok(fence)
}

struct ProcBinding {
    directory: std::os::fd::OwnedFd,
    process_id: u32,
}
static OWN_PROC_FD: std::sync::OnceLock<ProcBinding> = std::sync::OnceLock::new();

pub(super) fn startup_proc_binding_valid() -> bool {
    OWN_PROC_FD
        .get()
        .is_some_and(|binding| binding.process_id == std::process::id())
}

/// Capture the owner's procfs descriptor directory before discovery. This
/// relies on the trusted kernel and service mount namespace, not on protection
/// from a privileged administrator changing that namespace.
pub(super) fn install_startup_proc_binding() -> Result<(), &'static str> {
    if let Some(binding) = OWN_PROC_FD.get() {
        return if binding.process_id == std::process::id() {
            Ok(())
        } else {
            Err("proc binding belongs to another process")
        };
    }
    // Resolve the kernel's `self` entry within a verified procfs root, then
    // open `<self-pid>/fd` beneath that root without symlinks or mount
    // crossings. Checking only the f_type of `/proc/self/fd` would permit a
    // bind-mounted fd directory from a different process.
    let proc_root = openat(
        CWD,
        c"/proc",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "open proc root")?;
    let fs = rustix::fs::fstatfs(&proc_root).map_err(|_| "stat proc root")?;
    if fs.f_type != rustix::fs::PROC_SUPER_MAGIC {
        return Err("proc root is not procfs");
    }
    let mut pid = [0u8; 24];
    let pid_len = rustix::fs::readlinkat_raw(&proc_root, c"self", &mut pid[..20])
        .map_err(|_| "read proc self")?;
    if pid_len == 0 || pid_len >= 20 || !pid[..pid_len].iter().all(u8::is_ascii_digit) {
        return Err("invalid proc self identity");
    }
    pid[pid_len..pid_len + 3].copy_from_slice(b"/fd");
    let pid_fd = std::ffi::CStr::from_bytes_with_nul(&pid[..pid_len + 4])
        .map_err(|_| "invalid proc descriptor path")?;
    let proc_dir = rustix::fs::openat2(
        &proc_root,
        pid_fd,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        rustix::fs::ResolveFlags::BENEATH
            | rustix::fs::ResolveFlags::NO_XDEV
            | rustix::fs::ResolveFlags::NO_SYMLINKS,
    )
    .map_err(|_| "open own descriptor directory")?;
    OWN_PROC_FD
        .set(ProcBinding {
            directory: proc_dir,
            process_id: std::process::id(),
        })
        .map_err(|_| "proc binding already installed")
}

fn reopen_held_regular(
    held: &std::fs::File,
    expected: FileSystemRevision,
) -> Result<std::fs::File, &'static str> {
    let proc_binding = OWN_PROC_FD
        .get()
        .ok_or("startup proc binding unavailable")?;
    if proc_binding.process_id != std::process::id() {
        return Err("startup proc binding belongs to another process");
    }
    // The file descriptor remains held until the procfs link is opened; a
    // fixed stack buffer avoids allocating while formatting its decimal name.
    let mut name = [0u8; 32];
    let mut value = u32::try_from(held.as_raw_fd()).map_err(|_| "invalid descriptor")?;
    let mut start = name.len() - 1;
    loop {
        start -= 1;
        name[start] = b'0' + u8::try_from(value % 10).map_err(|_| "invalid descriptor")?;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let name = std::ffi::CStr::from_bytes_with_nul(&name[start..])
        .map_err(|_| "invalid descriptor name")?;
    let readable = openat(
        &proc_binding.directory,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "open package manifest bytes")?;
    let readable = std::fs::File::from(readable);
    let metadata = readable.metadata().map_err(|_| "stat reopened file")?;
    if !metadata.is_file() || file_system_revision_from_metadata(&metadata) != expected {
        return Err("reopened file identity changed");
    }
    Ok(readable)
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct EmptyConfigState {
    named: FileSystemRevision,
    target: FileSystemRevision,
}

/// Witness one missing or genuinely empty regular Git config. No nonempty
/// config bytes are intentionally consumed. Ordinary symlinks are followed
/// through O_PATH and both link and target identities are rechecked; magic
/// links, special files, unreadable paths, and changed identities decline.
pub(super) fn empty_config_state(
    path: &std::path::Path,
    budget: &RetirementBudget,
    bookkeeping: usize,
) -> Result<Option<EmptyConfigState>, &'static str> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if !path.is_absolute() || bytes.len() > MAX_PATH || bytes.contains(&0) {
        return Err("unsupported Git config path");
    }
    let charge = bytes
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_add(bookkeeping))
        .ok_or("Git config path overflow")?;
    let _reservation = budget
        .reserve(charge)
        .map_err(|_| "Git config path budget")?;
    let named = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None)
        }
        Err(_) => return Err("Git config unavailable"),
    };
    if !(named.is_file() || named.file_type().is_symlink()) || (named.is_file() && named.len() != 0)
    {
        return Err("Git config is not empty regular file");
    }
    let named_revision = file_system_revision_from_metadata(&named);
    let open_target = || {
        rustix::fs::openat2(
            CWD,
            path,
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            rustix::fs::ResolveFlags::NO_MAGICLINKS,
        )
        .map(std::fs::File::from)
        .map_err(|_| "open Git config identity")
    };
    let held = open_target()?;
    let metadata = held.metadata().map_err(|_| "stat Git config")?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err("Git config is not empty regular file");
    }
    // Only admit ordinary ext-family files here. Pseudo-files can report a
    // zero st_size while producing content, including sensitive process data.
    // The kernel and service mount namespace are the owner's trust boundary.
    if rustix::fs::fstatfs(&held)
        .map_err(|_| "stat Git config filesystem")?
        .f_type
        != 0xef53
    {
        return Err("unsupported Git config filesystem");
    }
    let expected = file_system_revision_from_metadata(&metadata);
    let mut readable = reopen_held_regular(&held, expected)?;
    let mut byte = [0u8; 1];
    if readable.read(&mut byte).map_err(|_| "read Git config")? != 0
        || file_system_revision_from_metadata(
            &readable.metadata().map_err(|_| "restat Git config")?,
        ) != expected
    {
        return Err("Git config is not stably empty");
    }
    let named_after = std::fs::symlink_metadata(path).map_err(|_| "Git config moved")?;
    if file_system_revision_from_metadata(&named_after) != named_revision {
        return Err("Git config changed");
    }
    let current_target = open_target()?;
    let current = current_target
        .metadata()
        .map_err(|_| "restat Git config target")?;
    if !current.is_file() || file_system_revision_from_metadata(&current) != expected {
        return Err("Git config target changed");
    }
    Ok(Some(EmptyConfigState {
        named: named_revision,
        target: expected,
    }))
}

/// Read a captured root package manifest through its already-opened inode.
/// The caller must bind these bytes to the compiler's actual host input and
/// revalidate the full inventory around the operation.
pub(super) fn read_root_package(
    root: &ProjectRoot,
    expected: &[CompilerWorkspaceEntry],
    budget: &RetirementBudget,
    bookkeeping: usize,
    cancelled: &AtomicBool,
) -> Result<Option<backend_version::retirement_budget::Metered<Vec<u8>>>, &'static str> {
    let index = match expected.binary_search_by(|entry| entry.path.as_str().cmp("package.json")) {
        Ok(index) => index,
        Err(_) => return Ok(None),
    };
    let row = &expected[index];
    if row.kind != CompilerWorkspaceEntryKind::File {
        return Err("package manifest is not a file");
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("package manifest cancelled");
    }
    let held = openat(
        &root.directory,
        c"package.json",
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "open package manifest identity")?;
    let held = std::fs::File::from(held);
    let meta = held.metadata().map_err(|_| "stat package manifest")?;
    if !meta.is_file() || file_system_revision_from_metadata(&meta) != row.revision {
        return Err("package manifest changed");
    }
    let length = usize::try_from(meta.len()).map_err(|_| "package manifest length overflow")?;
    if length > MAX_PACKAGE_MANIFEST_BYTES || row.byte_length != Some(meta.len()) {
        return Err("package manifest too large");
    }
    let mut readable = reopen_held_regular(&held, row.revision)?;
    let charge = length
        .checked_add(bookkeeping)
        .ok_or("package manifest allocation overflow")?;
    let reservation = budget
        .reserve(charge)
        .map_err(|_| "package manifest allocation budget")?;
    let mut bytes = vec![0u8; length];
    readable
        .read_exact(&mut bytes)
        .map_err(|_| "read package manifest")?;
    let mut extra = [0u8; 1];
    if readable
        .read(&mut extra)
        .map_err(|_| "read package manifest")?
        != 0
        || file_system_revision_from_metadata(
            &readable.metadata().map_err(|_| "restat package bytes")?,
        ) != row.revision
        || cancelled.load(Ordering::Acquire)
    {
        return Err("package manifest changed during read");
    }
    Ok(Some(reservation.meter(bytes)))
}

/// Open an absolute canonical spelling one component at a time. Reject a
/// different requested spelling instead of resolving links or allocating a
/// canonicalized path under the admission.
pub(super) fn open_root(
    requested: &std::path::Path,
    canonical: &std::path::Path,
) -> Result<ProjectRoot, &'static str> {
    use std::path::Component;
    if requested != canonical || !canonical.is_absolute() {
        return Err("unsupported root spelling");
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(CWD, c"/", flags, Mode::empty()).map_err(|_| "open root")?;
    for component in canonical.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => name.as_encoded_bytes(),
            _ => return Err("unsupported root component"),
        };
        if name.len() > MAX_PATH || name.contains(&0) {
            return Err("unsupported root component");
        }
        let mut nul = [0u8; MAX_PATH + 1];
        nul[..name.len()].copy_from_slice(name);
        let cstr = std::ffi::CStr::from_bytes_with_nul(&nul[..name.len() + 1])
            .map_err(|_| "invalid root component")?;
        directory =
            openat(&directory, cstr, flags, Mode::empty()).map_err(|_| "open root component")?;
    }
    Ok(ProjectRoot {
        directory: std::fs::File::from(directory),
    })
}

/// Revalidate every unfiltered directory entry without following links or
/// retaining path copies. An overlong path, excessive depth, unusual filename,
/// special file, or failed read declines the narrow admission.
pub(super) fn validate(
    root: &ProjectRoot,
    expected: &[CompilerWorkspaceEntry],
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<(), &'static str> {
    if expected.is_empty() || expected.windows(2).any(|w| w[0].path >= w[1].path) {
        return Err("empty inventory is not sorted and unique");
    }
    let mut path = [0u8; MAX_PATH];
    let mut seen = 0usize;
    let bitmap_bytes = expected
        .len()
        .checked_add(7)
        .ok_or("inventory bitmap overflow")?
        / 8;
    let charge = bitmap_bytes
        .checked_add(allocation_bookkeeping_bytes)
        .ok_or("inventory bitmap overflow")?;
    let _reservation = budget
        .reserve(charge)
        .map_err(|_| "inventory bitmap budget")?;
    let mut bitmap = vec![0u8; bitmap_bytes];
    visit(
        root.directory.as_fd(),
        &mut path,
        0,
        0,
        expected,
        &mut seen,
        &mut bitmap,
        cancelled,
    )?;
    if seen != expected.len() {
        return Err("empty inventory entry count changed");
    }
    Ok(())
}

fn is_source(name: &str) -> bool {
    let Some(extension) = name.rsplit_once('.').map(|(_, ext)| ext) else {
        return false;
    };
    ["ts", "mts", "cts", "js", "mjs", "cjs", "tsx", "jsx"]
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

fn visit<F: AsFd>(
    directory: F,
    path: &mut [u8; MAX_PATH],
    length: usize,
    depth: usize,
    expected: &[CompilerWorkspaceEntry],
    seen: &mut usize,
    bitmap: &mut [u8],
    cancelled: &AtomicBool,
) -> Result<(), &'static str> {
    if cancelled.load(Ordering::Acquire) {
        return Err("empty inventory cancelled");
    }
    if depth > MAX_DEPTH {
        return Err("empty inventory depth exceeded");
    }
    let relative = std::str::from_utf8(&path[..length]).map_err(|_| "non-UTF-8 inventory path")?;
    let index = expected
        .binary_search_by(|e| e.path.as_str().cmp(relative))
        .map_err(|_| "unexpected inventory directory")?;
    let row = &expected[index];
    let mask = 1u8 << (index % 8);
    if bitmap[index / 8] & mask != 0 {
        return Err("duplicate inventory entry");
    }
    bitmap[index / 8] |= mask;
    let before = rustix::fs::fstat(&directory).map_err(|_| "stat inventory directory")?;
    // `File` metadata uses the same representation as the original capture.
    let dir_file =
        std::fs::File::from(rustix::io::dup(&directory).map_err(|_| "duplicate directory")?);
    let metadata = dir_file
        .metadata()
        .map_err(|_| "stat inventory directory")?;
    if row.kind != CompilerWorkspaceEntryKind::Directory
        || row.revision != file_system_revision_from_metadata(&metadata)
    {
        return Err("inventory directory changed");
    }
    *seen = seen.checked_add(1).ok_or("inventory count overflow")?;
    let mut buffer = [MaybeUninit::<u8>::uninit(); 8192];
    let mut entries = RawDir::new(&directory, &mut buffer);
    while let Some(entry) = entries.next() {
        if cancelled.load(Ordering::Acquire) {
            return Err("empty inventory cancelled");
        }
        let entry = entry.map_err(|_| "read inventory directory")?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        // The existing workspace identity accepts only normalized Unicode.
        // Restrict this first proof to printable ASCII so byte identity and
        // that normalization agree without an allocating normalization pass.
        if bytes.is_empty()
            || bytes.iter().any(|byte| {
                !byte.is_ascii() || *byte < 0x20 || *byte == 0x7f || matches!(*byte, b'/' | b'\\')
            })
        {
            return Err("invalid inventory name");
        }
        let separator = usize::from(length != 0);
        let end = length
            .checked_add(separator)
            .and_then(|n| n.checked_add(bytes.len()))
            .ok_or("inventory path overflow")?;
        if end > path.len() {
            return Err("inventory path too long");
        }
        if separator != 0 {
            path[length] = b'/';
        }
        path[length + separator..end].copy_from_slice(bytes);
        let relative = std::str::from_utf8(&path[..end]).map_err(|_| "non-UTF-8 inventory path")?;
        let index = expected
            .binary_search_by(|e| e.path.as_str().cmp(relative))
            .map_err(|_| "unexpected inventory entry")?;
        let row = &expected[index];
        // O_PATH inspects files without opening a device or FIFO if the name
        // is concurrently replaced. Directories need a readable descriptor
        // for getdents, and DIRECTORY excludes special files at open time.
        let flags = if row.kind == CompilerWorkspaceEntryKind::Directory {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        } else {
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        let child =
            openat(&directory, name, flags, Mode::empty()).map_err(|_| "open inventory entry")?;
        let file = std::fs::File::from(child);
        let metadata = file.metadata().map_err(|_| "stat inventory entry")?;
        if file_system_revision_from_metadata(&metadata) != row.revision {
            return Err("inventory entry changed");
        }
        match row.kind {
            CompilerWorkspaceEntryKind::Directory if metadata.is_dir() => {
                visit(
                    &file,
                    path,
                    end,
                    depth + 1,
                    expected,
                    seen,
                    bitmap,
                    cancelled,
                )?;
            }
            CompilerWorkspaceEntryKind::File if metadata.is_file() && !is_source(relative) => {
                let mask = 1u8 << (index % 8);
                if bitmap[index / 8] & mask != 0 {
                    return Err("duplicate inventory entry");
                }
                bitmap[index / 8] |= mask;
                if row.byte_length != Some(metadata.len()) {
                    return Err("inventory file length changed");
                }
                *seen = seen.checked_add(1).ok_or("inventory count overflow")?;
            }
            _ => return Err("unsupported or source inventory entry"),
        }
    }
    let after = rustix::fs::fstat(&directory).map_err(|_| "restat inventory directory")?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_mtime != after.st_mtime
        || before.st_mtime_nsec != after.st_mtime_nsec
        || before.st_ctime != after.st_ctime
        || before.st_ctime_nsec != after.st_ctime_nsec
    {
        return Err("inventory directory changed during enumeration");
    }
    Ok(())
}
