//! States: the end of one journey as the start of another.
//!
//! `start from install frontends/rust/fixtures/toml_pin` needs the machine
//! as that part leaves it: a real install, compiled by the real owner. It is
//! materialized ONCE, by running the recipe's plan from clean on the one live
//! machine directory, and kept under a key:
//!
//! - the recipe's expanded steps (their words and kinds, not their line
//!   numbers: a comment added to a part does not re-run it);
//! - the harness binary's build id (Mach-O `LC_UUID`, else the executable's
//!   SHA-256): a new build is a new product, so its states are made again;
//! - the toolchain the owner compiles with (`NUDOX_*`, `LIBCLANG_PATH`,
//!   `CARGO_HOME`, `CARGO_NET_OFFLINE`);
//! - the bytes of every input the recipe names: each project tree (its
//!   `Cargo.lock` pins every registry crate by checksum) and each crate
//!   archive from the local cargo cache.
//!
//! A key that changes is a new directory, so a stale state is never used;
//! it is re-run. A state is never hand-made or copied from an unrelated run:
//! the only way into `STATES/<key>/root` is a PASSing run of its recipe, whose
//! REPORT and strip sit beside it. A journey that starts from it gets a clone
//! of it at the live path the recipe ran at, so every absolute path the owner
//! and `desktop-state.json` recorded is the same one.

use super::parts::Input;
use super::plan::Plan;
use sha2::{Digest as _, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// A state's key and the words it was made of.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateKey {
    /// The SHA-256 of `lines`, hex.
    pub hex: String,
    /// What went in, one record per line (`KEY.txt`).
    pub lines: Vec<String>,
}

impl StateKey {
    /// The first 16 hex digits: the directory name.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.hex[..16]
    }
}

/// The key of the state `plan` (a recipe's materializing plan) leaves.
#[must_use]
pub fn key(plan: &Plan, inputs: &[(Input, String)], build: &str, toolchain: &[(String, String)]) -> StateKey {
    let mut lines = vec!["journey-state v1".to_owned(), format!("recipe: {}", plan.name)];
    lines.extend(plan.steps.iter().map(|step| format!("step {} :: {:?}", step.text, step.kind)));
    lines.push(format!("size: {}x{}", plan.size.0, plan.size.1));
    lines.push(format!("build: {build}"));
    lines.extend(toolchain.iter().map(|(name, value)| format!("env: {name}={value}")));
    lines.extend(inputs.iter().map(|(input, hash)| match input {
        Input::Tree { rel, .. } => format!("input tree {rel}: {hash}"),
        Input::Crate(release) => format!("input crate {}@{}: {hash}", release.name, release.version),
    }));
    let hex = hex(&Sha256::digest(lines.join("\n").as_bytes()));
    StateKey { hex, lines }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The environment the owner compiles with, sorted.
#[must_use]
pub fn toolchain() -> Vec<(String, String)> {
    let mut vars = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.to_string_lossy().into_owned())))
        .filter(|(name, _)| {
            name.starts_with("NUDOX_") || matches!(name.as_str(), "LIBCLANG_PATH" | "CARGO_HOME" | "CARGO_NET_OFFLINE")
        })
        .filter(|(name, _)| !name.starts_with("NUDOX_HARNESS") && !name.starts_with("NUDOX_TRACE") && !name.starts_with("NUDOX_REVIEW"))
        .collect::<Vec<_>>();
    vars.sort();
    vars
}

/// This executable's build id: `macho-uuid:<hex>` when the binary carries an
/// `LC_UUID`, else `sha256:<hex>` of its bytes.
///
/// # Errors
/// The executable cannot be found or read.
pub fn build_id() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|error| format!("this executable: {error}"))?;
    // The load commands sit right after the header: the first MiB holds them.
    let mut head = Vec::with_capacity(1 << 20);
    std::fs::File::open(&exe)
        .and_then(|file| file.take(1 << 20).read_to_end(&mut head))
        .map_err(|error| format!("{}: {error}", exe.display()))?;
    match macho_uuid(&head) {
        Some(uuid) => Ok(format!("macho-uuid:{}", hex(&uuid))),
        None => file_hash(&exe).map(|digest| format!("sha256:{digest}")),
    }
}

/// The `LC_UUID` of a thin 64-bit little-endian Mach-O image.
fn macho_uuid(bytes: &[u8]) -> Option<[u8; 16]> {
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_UUID: u32 = 0x1b;
    let word = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)) };
    if word(0)? != MH_MAGIC_64 {
        return None;
    }
    let commands = word(16)?;
    let mut at = 32_usize;
    for _ in 0..commands {
        let (command, size) = (word(at)?, usize::try_from(word(at + 4)?).ok()?);
        if command == LC_UUID {
            return bytes.get(at + 8..at + 24)?.try_into().ok();
        }
        if size < 8 {
            return None;
        }
        at = at.checked_add(size)?;
    }
    None
}

/// The SHA-256 of what an input holds.
///
/// # Errors
/// The input cannot be read (a crate release not in the cache).
pub fn input_hash(input: &Input) -> Result<String, String> {
    match input {
        Input::Tree { abs, .. } => tree_hash(abs),
        Input::Crate(release) => {
            let archive = cached_archive(&release.stem());
            match archive {
                Some(path) => file_hash(&path),
                None => tree_hash(&super::parts::registry_source(release)?),
            }
        }
    }
}

fn cached_archive(stem: &str) -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))?;
    let mut found = std::fs::read_dir(home.join("registry/cache"))
        .ok()?
        .filter_map(|index| Some(index.ok()?.path().join(format!("{stem}.crate"))))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    found.sort();
    found.pop()
}

fn file_hash(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Every file under `root` (build output and VCS metadata aside), by
/// relative path and content, in sorted order.
///
/// # Errors
/// A directory or file that cannot be read.
pub fn tree_hash(root: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (relative, digest) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update([0]);
    }
    Ok(hex(&hasher.finalize()))
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        if matches!(name.to_str(), Some("target" | ".git" | "node_modules" | ".DS_Store")) {
            continue;
        }
        let kind = entry.file_type().map_err(|error| format!("{}: {error}", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
        if kind.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            files.push((relative, format!("link:{}", target.display())));
        } else if kind.is_dir() {
            walk(root, &path, files)?;
        } else {
            files.push((relative, file_hash(&path)?));
        }
    }
    Ok(())
}

/// Where machines live: the one live directory every production journey
/// runs in, and the cache of materialized states.
#[derive(Clone, Debug)]
pub struct Store {
    /// `.local/harness/w-journey-clean-live`: a user root (`workspace/`,
    /// `starter/`), the Finder launch's layout (`host::paths::ambient_paths`).
    pub live: PathBuf,
    /// `.local/harness/w-journey-clean-states`.
    pub states: PathBuf,
}

impl Store {
    /// The journey lane's own directories.
    #[must_use]
    pub fn lane() -> Self {
        let harness = super::super::repo().join(".local/harness");
        Self { live: harness.join("w-journey-clean-live"), states: harness.join("w-journey-clean-states") }
    }

    /// The directory of the state under `key`.
    #[must_use]
    pub fn state(&self, key: &StateKey) -> PathBuf {
        self.states.join(key.short())
    }

    /// Whether the state under `key` was materialized by a passing run.
    #[must_use]
    pub fn ready(&self, key: &StateKey) -> bool {
        self.state(key).join("READY").is_file()
    }

    /// An empty live machine: nothing from any earlier run.
    ///
    /// # Errors
    /// The old live directory cannot be removed or the new one made.
    pub fn wipe_live(&self) -> Result<(), String> {
        if self.live.exists() {
            std::fs::remove_dir_all(&self.live).map_err(|error| format!("{}: {error}", self.live.display()))?;
        }
        private_dir(&self.live)
    }

    /// The live machine becomes a copy of the state under `key`.
    ///
    /// # Errors
    /// The state is not ready, or the copy fails.
    pub fn restore(&self, key: &StateKey) -> Result<(), String> {
        if !self.ready(key) {
            return Err(format!("state {} is not materialized", key.short()));
        }
        if self.live.exists() {
            std::fs::remove_dir_all(&self.live).map_err(|error| format!("{}: {error}", self.live.display()))?;
        }
        clone_tree(&self.state(key).join("root"), &self.live)
    }

    /// Keeps the live machine (after a passing recipe run) as the state
    /// under `key`, with the run's evidence beside it. `READY` is written
    /// last: a state is ready only once everything else is in place.
    ///
    /// # Errors
    /// The copy or a write fails.
    pub fn keep(&self, key: &StateKey) -> Result<PathBuf, String> {
        let dir = self.state(key);
        let root = dir.join("root");
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|error| format!("{}: {error}", root.display()))?;
        }
        private_dir(&dir)?;
        clone_tree(&self.live, &root)?;
        std::fs::write(dir.join("KEY.txt"), key.lines.join("\n") + "\n").map_err(|error| format!("{}: {error}", dir.display()))?;
        std::fs::write(dir.join("READY"), format!("{}\n", key.hex)).map_err(|error| format!("{}: {error}", dir.display()))?;
        Ok(dir)
    }

    /// Where the run that materializes the state under `key` writes its
    /// evidence (REPORT, strip), beside the state. Any earlier attempt's
    /// `READY` is removed first: only a passing run writes it again.
    ///
    /// # Errors
    /// The directory cannot be made.
    pub fn evidence(&self, key: &StateKey) -> Result<PathBuf, String> {
        let dir = self.state(key);
        let ready = dir.join("READY");
        if ready.exists() {
            std::fs::remove_file(&ready).map_err(|error| format!("{}: {error}", ready.display()))?;
        }
        let run = dir.join("run");
        private_dir(&run)?;
        Ok(run)
    }

    /// Holds the live machine for this process: a second harness process
    /// running a production journey at the same time is refused, not raced.
    ///
    /// # Errors
    /// Another process holds it.
    pub fn lock(&self) -> Result<std::fs::File, String> {
        let path = self.live.with_extension("lock");
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        file.try_lock().map_err(|error| format!("{} is held by another journey run ({error}); one harness process at a time", path.display()))?;
        Ok(file)
    }
}

fn private_dir(path: &Path) -> Result<(), String> {
    super::super::private_dir(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// A copy-on-write clone where the filesystem has one (APFS), else a copy.
fn clone_tree(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        private_dir(parent)?;
    }
    // The system `cp` (a dev shell's GNU `cp` has no `-c`): on APFS, `-c`
    // clones every file copy-on-write, so a state costs no space until used.
    let (cp, flags) = if cfg!(target_os = "macos") { ("/bin/cp", "-Rc") } else { ("cp", "-a") };
    let status = std::process::Command::new(cp)
        .arg(flags)
        .arg(from)
        .arg(to)
        .status()
        .map_err(|error| format!("{cp} {flags} {} {}: {error}", from.display(), to.display()))?;
    if status.success() { Ok(()) } else { Err(format!("{cp} {flags} {} {}: {status}", from.display(), to.display())) }
}

#[cfg(test)]
mod tests {
    use super::{key, macho_uuid, tree_hash};
    use crate::harness::journey::parts::{Input, Parts};
    use crate::harness::journey::plan::{Plan, Start};
    use std::path::Path;

    fn recipe(parts: &Parts, text: &str) -> Plan {
        let plan = Plan::parse("J", Path::new("J.journey"), text, parts).expect("parses");
        let Start::From(recipe) = plan.start else { panic!("a state") };
        recipe.plan(parts, plan.size).expect("expands")
    }

    #[test]
    fn a_state_key_moves_with_its_steps_arguments_build_toolchain_and_inputs_not_its_line_numbers() {
        let mut parts = Parts::default();
        parts.add_file(Path::new("a.part"), "part open WHAT:word\nkey cmd-k\ncheck {WHAT}\n  text \"{WHAT}\"\n").expect("parts");
        let plan = recipe(&parts, "start from open toml\ncheck x\n");
        let tree = Input::Tree { rel: "p".to_owned(), abs: "/p".into() };
        let env = vec![("NUDOX_RUSTC".to_owned(), "/nix/a/rustc".to_owned())];
        let base = key(&plan, &[(tree.clone(), "h1".to_owned())], "macho-uuid:1", &env);
        assert_eq!(base, key(&plan, &[(tree.clone(), "h1".to_owned())], "macho-uuid:1", &env), "the same inputs give the same key");
        assert_eq!(base.hex.len(), 64);
        for (why, other) in [
            ("an argument", key(&recipe(&parts, "start from open serde\ncheck x\n"), &[(tree.clone(), "h1".to_owned())], "macho-uuid:1", &env)),
            ("an input's bytes", key(&plan, &[(tree.clone(), "h2".to_owned())], "macho-uuid:1", &env)),
            ("the build", key(&plan, &[(tree.clone(), "h1".to_owned())], "macho-uuid:2", &env)),
            ("the toolchain", key(&plan, &[(tree.clone(), "h1".to_owned())], "macho-uuid:1", &[("NUDOX_RUSTC".to_owned(), "/nix/b/rustc".to_owned())])),
        ] {
            assert_ne!(base.hex, other.hex, "changing {why} must re-run the state");
        }
        let mut edited = Parts::default();
        edited.add_file(Path::new("a.part"), "part open WHAT:word\nkey cmd-k\ncheck {WHAT}\n  text \"{WHAT}\"\n  absent \"x\"\n").expect("parts");
        assert_ne!(base.hex, key(&recipe(&edited, "start from open toml\ncheck x\n"), &[(tree.clone(), "h1".to_owned())], "macho-uuid:1", &env).hex, "a part's body is in the key");
        let mut commented = Parts::default();
        commented.add_file(Path::new("a.part"), "# a comment moves every line\n\npart open WHAT:word\nkey cmd-k\ncheck {WHAT}\n  text \"{WHAT}\"\n").expect("parts");
        assert_eq!(base.hex, key(&recipe(&commented, "start from open toml\ncheck x\n"), &[(tree, "h1".to_owned())], "macho-uuid:1", &env).hex, "line numbers are not");
    }

    #[test]
    fn a_tree_hash_reads_contents_and_skips_build_output() {
        let root = std::env::temp_dir().join(format!("nudox-tree-hash-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).expect("dirs");
        std::fs::create_dir_all(root.join("target/debug")).expect("dirs");
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}").expect("write");
        let first = tree_hash(&root).expect("hash");
        std::fs::write(root.join("target/debug/out"), "noise").expect("write");
        assert_eq!(first, tree_hash(&root).expect("hash"), "build output is not an input");
        std::fs::write(root.join("src/lib.rs"), "pub fn b() {}").expect("write");
        assert_ne!(first, tree_hash(&root).expect("hash"), "a source edit is");
        std::fs::remove_dir_all(&root).expect("clean up the test's own dir");
    }

    #[test]
    fn a_macho_uuid_is_read_from_its_load_command() {
        let mut image = vec![0_u8; 32];
        image[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        image[16..20].copy_from_slice(&2_u32.to_le_bytes());
        // LC_SEGMENT_64-ish filler of 16 bytes, then LC_UUID.
        image.extend_from_slice(&0x19_u32.to_le_bytes());
        image.extend_from_slice(&16_u32.to_le_bytes());
        image.extend_from_slice(&[0; 8]);
        image.extend_from_slice(&0x1b_u32.to_le_bytes());
        image.extend_from_slice(&24_u32.to_le_bytes());
        image.extend_from_slice(&[7; 16]);
        assert_eq!(macho_uuid(&image), Some([7; 16]));
        assert_eq!(macho_uuid(b"\x7fELF not a mach-o image at all"), None);
    }
}
