//! A project's dependency tree, read from Cargo and the advisory authority.
//!
//! Cargo is the authority for the current target's active dependency graph:
//! `cargo metadata --filter-platform <host>`. Cargo.lock rows outside that
//! graph can also be disabled by feature selection, so the difference is not
//! called "other platforms." When Cargo cannot answer (not installed, no
//! network for a missing download, a stale lockfile under `--locked`), the
//! tree is read from `Cargo.lock` alone and says so.
//!
//! The Cargo half is cached against the bytes of `Cargo.lock` and every
//! member manifest, so a tree read costs one Cargo run per change to the
//! project, not one per read. Advisories are observed on every read, so a
//! refresh shows at once.

use backend_library::browse::{
    LockedInactiveCoverage, LockfileGraphCoverage, ProjectTree, TreeInput, TreeSource, build_tree,
    lockfile_input, metadata_input,
};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Largest `cargo metadata` document admitted.
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Largest `Cargo.lock` admitted.
const MAX_LOCKFILE_BYTES: u64 = 16 * 1024 * 1024;
/// How long Cargo may take to answer.
const CARGO_DEADLINE: Duration = Duration::from_secs(90);

/// The last tree input read from Cargo, and the file bytes it was read from.
#[derive(Default)]
pub(super) struct BrowseCache {
    entry: Option<CacheEntry>,
}

struct CacheEntry {
    /// The project directory Cargo resolves from (see [`workspace_root`]).
    workspace: PathBuf,
    witness: [u8; 32],
    watched: Vec<PathBuf>,
    input: TreeInput,
}

impl BrowseCache {
    /// Reads the tree of the Cargo project containing `root`.
    pub(super) fn project_tree(
        &mut self,
        root: &Path,
        authority: Option<&backend_engine::advisory::AdvisoryAuthority>,
    ) -> Result<ProjectTree, String> {
        if !root.is_absolute() {
            return Err("project-tree needs an absolute project directory".to_owned());
        }
        let input = self.input(root)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let empty = backend_engine::advisory::AdvisoryAuthority::new(0);
        let authority = authority.unwrap_or(&empty);
        let observe = |name: &str, version: &str| {
            let package = backend_engine::advisory::normalize_package("cargo", name)
                .unwrap_or_else(|_| backend_engine::advisory::PackageIdentity {
                    ecosystem: "cargo".to_owned(),
                    name: name.to_owned(),
                    canonical_purl: None,
                });
            authority.observe(&package, version, false, false, now, false)
        };
        Ok(build_tree(&input, &observe))
    }

    fn input(&mut self, root: &Path) -> Result<TreeInput, String> {
        let workspace = workspace_root(root).ok_or_else(|| {
            format!(
                "{} is not inside a Cargo project (no Cargo.toml above it)",
                root.display()
            )
        })?;
        // The same project, and none of the files it was read from moved.
        // (A nested workspace is another project: compare the resolved
        // directory, never a path prefix.)
        if let Some(entry) = &self.entry
            && entry.workspace == workspace
            && witness(&entry.watched) == entry.witness
        {
            return Ok(entry.input.clone());
        }
        let (input, watched) = read_project(&workspace)?;
        self.entry = Some(CacheEntry {
            workspace,
            witness: witness(&watched),
            watched,
            input: input.clone(),
        });
        Ok(input)
    }
}

/// Hashes the files a tree was read from; a missing file hashes as absent.
fn witness(files: &[PathBuf]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for file in files {
        hasher.update(file.as_os_str().as_encoded_bytes());
        match std::fs::read(file) {
            Ok(bytes) => {
                hasher.update(&[1]);
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            Err(_) => {
                hasher.update(&[0]);
            }
        }
    }
    *hasher.finalize().as_bytes()
}

/// The tree input of the project at `workspace`, and the files it depends on.
fn read_project(workspace: &Path) -> Result<(TreeInput, Vec<PathBuf>), String> {
    let workspace = workspace.to_path_buf();
    let lockfile = read_bounded(&workspace.join("Cargo.lock"));
    match cargo_metadata(&workspace) {
        Ok((metadata, host)) => {
            let input = metadata_input(&metadata, &host, lockfile.as_deref())
                .map_err(|error| error.to_string())?;
            let mut watched = vec![
                PathBuf::from(&input.root).join("Cargo.lock"),
                PathBuf::from(&input.root).join("Cargo.toml"),
            ];
            watched.extend(member_manifests(&metadata));
            Ok((input, watched))
        }
        Err(reason) => {
            let lockfile = lockfile.ok_or_else(|| {
                format!("{reason}; and {} has no Cargo.lock", workspace.display())
            })?;
            let patched = patched_names(&workspace.join("Cargo.toml"));
            let input = lockfile_input(&lockfile, &workspace.to_string_lossy(), &patched, &reason)
                .map_err(|error| error.to_string())?;
            Ok((
                input,
                vec![workspace.join("Cargo.lock"), workspace.join("Cargo.toml")],
            ))
        }
    }
}

/// The nearest directory at or above `root` whose `Cargo.toml` declares
/// `[workspace]`, else the nearest with a `Cargo.toml`: where Cargo itself
/// would resolve the project.
fn workspace_root(root: &Path) -> Option<PathBuf> {
    let mut nearest = None;
    for directory in root.ancestors() {
        let manifest = directory.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        if nearest.is_none() {
            nearest = Some(directory.to_path_buf());
        }
        if std::fs::read_to_string(&manifest)
            .is_ok_and(|text| text.lines().any(|line| line.trim() == "[workspace]"))
        {
            return Some(directory.to_path_buf());
        }
    }
    nearest
}

fn read_bounded(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_LOCKFILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The names `[patch.<registry>]` replaces with a path.
fn patched_names(manifest: &Path) -> BTreeSet<String> {
    let Some(text) = read_bounded(manifest) else {
        return BTreeSet::new();
    };
    let Ok(document) = text.parse::<toml::Value>() else {
        return BTreeSet::new();
    };
    document
        .get("patch")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|registries| registries.values())
        .filter_map(toml::Value::as_table)
        .flat_map(|table| table.keys().cloned())
        .collect()
}

fn member_manifests(metadata: &[u8]) -> Vec<PathBuf> {
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(metadata) else {
        return Vec::new();
    };
    let members = root
        .get("workspace_members")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    root.get("packages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|package| {
            package
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| members.contains(id))
        })
        .filter_map(|package| {
            package
                .get("manifest_path")
                .and_then(serde_json::Value::as_str)
        })
        .map(PathBuf::from)
        .collect()
}

/// Runs `cargo metadata` for this host; returns the document and the host triple.
fn cargo_metadata(workspace: &Path) -> Result<(Vec<u8>, String), String> {
    let cargo = cargo_program().ok_or_else(|| "cargo was not found".to_owned())?;
    let version = run(&cargo, workspace, &["-vV"], 64 * 1024)?;
    let host = String::from_utf8_lossy(&version)
        .lines()
        .find_map(|line| {
            line.strip_prefix("host: ")
                .map(str::trim)
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| "cargo -vV named no host".to_owned())?;
    let metadata = run(
        &cargo,
        workspace,
        &[
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            &host,
        ],
        MAX_METADATA_BYTES,
    )?;
    Ok((metadata, host))
}

/// `NUDOX_CARGO`, else `cargo` beside `NUDOX_RUSTC`, else the first `cargo`
/// on `PATH` or in the usual install places.
fn cargo_program() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("NUDOX_CARGO") {
        return Some(PathBuf::from(explicit));
    }
    let executable = |path: PathBuf| path.is_file().then_some(path);
    if let Some(rustc) = std::env::var_os("NUDOX_RUSTC")
        && let Some(found) = Path::new(&rustc)
            .parent()
            .map(|bin| bin.join("cargo"))
            .and_then(executable)
    {
        return Some(found);
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("cargo"))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".cargo/bin/cargo"));
        candidates.push(home.join(".nix-profile/bin/cargo"));
        if let Some(user) = home.file_name() {
            candidates.push(
                Path::new("/etc/profiles/per-user")
                    .join(user)
                    .join("bin/cargo"),
            );
        }
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/cargo"));
    candidates.push(PathBuf::from("/usr/local/bin/cargo"));
    candidates.push(PathBuf::from("/run/current-system/sw/bin/cargo"));
    candidates.into_iter().find_map(executable)
}

/// Runs one bounded, deadline-limited command and returns its stdout.
fn run(
    program: &Path,
    directory: &Path,
    arguments: &[&str],
    maximum: usize,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    // Cargo runs `rustc` to learn the host's targets and cfgs, found through
    // `RUSTC` or `PATH`. A Finder launch's `PATH` holds no `rustc`, and a
    // metadata read that cannot run it fell back to the bare lockfile (every
    // pinned package, including those no build here compiles): name the
    // compiler that ships with this cargo.
    if std::env::var_os("RUSTC").is_none()
        && let Some(rustc) = program
            .parent()
            .map(|bin| bin.join("rustc"))
            .filter(|rustc| rustc.is_file())
    {
        command.env("RUSTC", rustc);
    }
    let mut child = command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cargo could not start: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cargo has no stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "cargo has no stderr".to_owned())?;
    let limit = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(limit).read_to_end(&mut bytes).map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > CARGO_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "cargo {} took longer than {}s",
                    arguments[0],
                    CARGO_DEADLINE.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("cargo {} failed: {error}", arguments[0])),
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "cargo's output reader panicked".to_owned())?
        .map_err(|error| format!("reading cargo's output: {error}"))?;
    let errors = errors.join().unwrap_or_default();
    if bytes.len() > maximum {
        return Err(format!(
            "cargo {} wrote more than {maximum} bytes",
            arguments[0]
        ));
    }
    if !status.success() {
        let first = String::from_utf8_lossy(&errors)
            .lines()
            .find(|line| line.starts_with("error"))
            .map_or_else(|| format!("exit status {status}"), ToOwned::to_owned);
        return Err(format!("cargo {} failed: {first}", arguments[0]));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_root_is_the_outermost_workspace_manifest() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let repository = repository.canonicalize().expect("repository");
        let found = workspace_root(&repository.join("crates/present")).expect("workspace");
        assert_eq!(found, repository);
        let patched = patched_names(&repository.join("Cargo.toml"));
        assert!(patched.contains("gpui-ce"), "{patched:?}");
    }

    #[test]
    fn a_directory_outside_any_cargo_project_says_so() {
        let error = BrowseCache::default()
            .project_tree(Path::new("/"), None)
            .expect_err("no project at the root");
        assert!(error.contains("is not inside a Cargo project"), "{error}");
    }

    #[test]
    fn an_untouched_workspace_is_cached_but_lockfile_and_manifest_changes_each_invalidate_it() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-browse-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        std::fs::create_dir_all(scratch.0.join("src")).expect("project dir");
        let manifest = scratch.0.join("Cargo.toml");
        let lockfile = scratch.0.join("Cargo.lock");
        let write_project = |version: &str| {
            std::fs::write(
                &manifest,
                format!(
                    "[package]\nname = \"gapfix\"\nversion = \"{version}\"\nedition = \"2021\"\n"
                ),
            )
            .expect("manifest");
            std::fs::write(
                &lockfile,
                format!(
                    "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"gapfix\"\nversion = \"{version}\"\n"
                ),
            )
            .expect("lockfile");
        };
        write_project("0.1.0");
        std::fs::write(scratch.0.join("src/lib.rs"), "").expect("lib.rs");
        let root = scratch.0.canonicalize().expect("canonical root");

        // Plant a cache entry whose witness matches the files exactly as
        // they stand right now, but whose `input` is a sentinel no real read
        // of this project would ever produce (a real read's `root` is this
        // absolute path, never the literal string below). If an untouched
        // read is served from the cache, it must come back exactly as
        // planted — proving `read_project` (and so Cargo) was never called
        // again. This is the half of the seam a mutation that always
        // recomputes (never caches) cannot pass.
        let watched = vec![root.join("Cargo.lock"), root.join("Cargo.toml")];
        let sentinel = TreeInput {
            source: TreeSource::Lockfile {
                reason: "planted by the test, never a real read".to_owned(),
                coverage: LockfileGraphCoverage::Complete,
                workspace_membership:
                    backend_library::browse::LockfileWorkspaceMembership::Unknown,
            },
            root: "sentinel-root".to_owned(),
            packages: Vec::new(),
            edges: Vec::new(),
            locked_inactive: 0,
            locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
        };
        let mut cache = BrowseCache::default();
        cache.entry = Some(CacheEntry {
            workspace: root.clone(),
            witness: witness(&watched),
            watched: watched.clone(),
            input: sentinel.clone(),
        });

        let untouched = cache.project_tree(&root, None).expect("untouched read");
        assert_eq!(
            untouched.root, "sentinel-root",
            "an untouched workspace must be served from the cache, not recomputed: {untouched:?}"
        );

        // A lockfile-only edit invalidates the cached Cargo resolution. Its
        // manifest stays byte-for-byte identical, so watching only manifests
        // cannot pass this assertion.
        let initial_lock = std::fs::read_to_string(&lockfile).expect("initial lockfile");
        std::fs::write(
            &lockfile,
            format!("{initial_lock}\n# lockfile changed alone\n"),
        )
        .expect("changed lockfile");
        let touched = cache.project_tree(&root, None).expect("lockfile-only read");
        assert_ne!(
            touched.root, "sentinel-root",
            "a changed lockfile alone must invalidate the cache and force a real read"
        );

        // Replant the sentinel against the new lockfile, then change only
        // Cargo.toml. Both inputs to Cargo's answer have independent guards.
        cache.entry = Some(CacheEntry {
            workspace: root.clone(),
            witness: witness(&watched),
            watched,
            input: sentinel,
        });
        std::fs::write(
            &manifest,
            "[package]\nname = \"gapfix\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
        )
        .expect("changed manifest");
        let touched = cache.project_tree(&root, None).expect("manifest-only read");
        assert_ne!(
            touched.root, "sentinel-root",
            "a changed manifest alone must force a real read"
        );
    }
}
