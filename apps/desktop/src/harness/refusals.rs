//! What the harness remembers between boots about roots the owner refused to
//! index, and how it classifies what the owner said.
//!
//! Asking a refused root again costs minutes per root for the same answer,
//! so a refusal is written to the state directory and not asked twice. It is
//! only the same question when the same things decide the answer:
//!
//! - the owner: the semantic compiler is linked into this executable, so the
//!   executable is the owner's identity ([`OwnerBuild`]);
//! - the toolchain environment: the owner finds its compilers through the
//!   `NUDOX_*` variables `development.sh` sets ([`ToolchainEnv`]); a run
//!   without them refuses every root with `Toolchain { … }`, which says
//!   nothing about a run with them;
//! - the resolved Cargo authority: its effective home, source roots and
//!   registry indexes select the package source trees ([`CargoAuthorityKey`]);
//! - the root's own bytes ([`root_digest`]).
//!
//! A record made under one of those is never read back under another.

use backend_client::ClientError;
use backend_library::RowState;
use backend_local_service::ProtocolError;
use crate::host::registry::CargoAuthorityKey;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The record, under the state directory.
const RECORD_FILE: &str = "harness-refusals.tsv";
/// The first line of the record: its format, so another format is not read.
const RECORD_HEADER: &str = "nudox-harness-refusals 3";
/// The record an earlier harness wrote, keyed by the source tree only (it
/// replayed a refusal made without the toolchain environment as a fact).
const RETIRED_RECORD_FILE: &str = "harness-failed-roots.tsv";

/// The bytes' digest, in hex.
fn hex(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref()).iter().fold(String::new(), |mut out, byte| {
        out.push_str(&format!("{byte:02x}"));
        out
    })
}

/// This executable: the owner is linked into it, so its length and
/// modification time say which owner is asking.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct OwnerBuild {
    length: u64,
    modified_ns: u128,
}

impl OwnerBuild {
    pub(super) fn of_current_exe() -> Self {
        let modified = |metadata: &std::fs::Metadata| {
            metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|since| since.as_nanos())
        };
        std::env::current_exe()
            .and_then(std::fs::metadata)
            .map(|metadata| Self { length: metadata.len(), modified_ns: modified(&metadata).unwrap_or_default() })
            .unwrap_or_default()
    }
}

/// The variables the owner finds its compilers through: every `NUDOX_*` (the
/// tool paths, the JDK, the corpora) and `LIBCLANG_PATH`, minus the
/// harness's own (`NUDOX_HARNESS_*` names where this run keeps its state;
/// `NUDOX_REVIEW_*` only turns on diagnostics). Sorted, so the same
/// environment is the same value.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ToolchainEnv(Vec<(String, String)>);

impl ToolchainEnv {
    fn read_by_the_owner(name: &str) -> bool {
        name == "LIBCLANG_PATH" || (name.starts_with("NUDOX_") && !name.starts_with("NUDOX_HARNESS_") && !name.starts_with("NUDOX_REVIEW_"))
    }

    pub(super) fn of_process() -> Self {
        Self::from_pairs(std::env::vars_os().filter_map(|(name, value)| Some((name.into_string().ok()?, value.to_string_lossy().into_owned()))))
    }

    fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut wanted = pairs.into_iter().filter(|(name, _)| Self::read_by_the_owner(name)).collect::<Vec<_>>();
        wanted.sort();
        Self(wanted)
    }
}

/// Everything that decides an owner's answer other than the root's bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Environment(String);

impl Environment {
    pub(super) fn of_process(authority: &CargoAuthorityKey) -> Self {
        Self::of(OwnerBuild::of_current_exe(), &ToolchainEnv::of_process(), authority.as_str())
    }

    fn of(build: OwnerBuild, toolchain: &ToolchainEnv, authority: &str) -> Self {
        let mut text = format!("owner {} {}\ncargo-authority={authority}\n", build.length, build.modified_ns);
        for (name, value) in &toolchain.0 {
            text.push_str(&format!("{name}={value}\n"));
        }
        Self(hex(text))
    }
}

/// A digest of a root's files: each one's path (relative to the root), length
/// and modification time. `target`, `.git` and `node_modules` are not the
/// root's source. A root that is not there digests as such.
fn root_digest(root: &Path) -> String {
    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Ok(kind) = entry.file_type() else { continue };
            let relative = prefix.join(&name);
            if kind.is_dir() {
                if !matches!(name.to_str(), Some("target" | ".git" | "node_modules")) {
                    walk(&entry.path(), &relative, out);
                }
            } else if let Ok(metadata) = entry.metadata() {
                let modified = metadata.modified().ok().and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |since| since.as_nanos());
                out.push(format!("{} {} {modified}", relative.display(), metadata.len()));
            }
        }
    }
    if !root.exists() {
        return hex("missing");
    }
    let mut files = Vec::new();
    walk(root, Path::new(""), &mut files);
    files.sort();
    hex(files.join("\n"))
}

/// The refusals recorded for this state directory that still hold: same
/// owner, toolchain, Cargo authority and root bytes.
pub(super) struct Refusals {
    path: PathBuf,
    /// This run's key for each root.
    keys: BTreeMap<PathBuf, String>,
    /// The roots refused before under exactly that key, with the owner's words.
    known: BTreeMap<PathBuf, String>,
}

impl Refusals {
    /// Reads `state`'s record for `roots` under this process's owner, toolchain
    /// and resolved Cargo authority.
    pub(super) fn load(state: &Path, roots: &[PathBuf], authority: &CargoAuthorityKey) -> Self {
        Self::load_under(state, roots, &Environment::of_process(authority))
    }

    fn load_under(state: &Path, roots: &[PathBuf], environment: &Environment) -> Self {
        let keys = roots.iter().map(|root| (root.clone(), hex(format!("{}\n{}\n{}", environment.0, root.display(), root_digest(root))))).collect::<BTreeMap<_, _>>();
        let path = state.join(RECORD_FILE);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines = text.lines();
        let known = if lines.next() == Some(RECORD_HEADER) {
            lines
                .filter_map(|line| {
                    let mut fields = line.splitn(3, '\t');
                    let (root, key, reason) = (PathBuf::from(fields.next()?), fields.next()?, fields.next()?);
                    (keys.get(&root).map(String::as_str) == Some(key)).then(|| (root, reason.to_owned()))
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        Self { path, keys, known }
    }

    /// What the owner said to `root` before, when the same question would be
    /// asked again.
    pub(super) fn recorded(&self, root: &Path) -> Option<&str> {
        self.known.get(root).map(String::as_str)
    }

    /// Writes the record for `refused`, whole or not at all (a file another
    /// process is reading is never half written), and retires the record an
    /// earlier harness wrote. Nothing refused: no record.
    pub(super) fn save(&self, refused: &BTreeMap<PathBuf, String>) {
        let _ = std::fs::remove_file(self.path.with_file_name(RETIRED_RECORD_FILE));
        if refused.is_empty() {
            let _ = std::fs::remove_file(&self.path);
            return;
        }
        let mut text = format!("{RECORD_HEADER}\n");
        for (root, reason) in refused {
            let Some(key) = self.keys.get(root) else { continue };
            text.push_str(&format!("{}\t{key}\t{}\n", root.display(), reason.replace(['\n', '\t', '\r'], " ")));
        }
        let scratch = self.path.with_extension(format!("tmp-{}", std::process::id()));
        if std::fs::write(&scratch, text).is_ok() {
            let _ = std::fs::rename(&scratch, &self.path);
        }
    }
}

/// What the owner answered to an index request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Reply {
    /// It took the request.
    Accepted,
    /// It ran the request and said no (a compile failure, say), in its words.
    Refused(String),
}

/// What to do about an error an index request came back with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Fate {
    /// The owner said no: that is an answer, recorded and not asked again.
    Refused(String),
    /// The connection or the endpoint failed: not an answer. Ask again on a
    /// new connection; never record it.
    Retry,
    /// Anything else: the boot cannot trust the owner's reply.
    Fatal,
}

/// The owner's own "no". A command it ran and rejected arrives as the words
/// `command execution failed: …` (`ProtocolError::CommandExecution`, sent as
/// `ClientError::Protocol`): an index compilation refusal arrives this way.
/// Typed application/query failures do not prove the index request was run.
pub(super) fn fate(error: &ClientError) -> Fate {
    let ran_and_failed = ProtocolError::CommandExecution(String::new()).to_string();
    match error {
        ClientError::Protocol(words) if words.starts_with(&ran_and_failed) => Fate::Refused(format!("protocol: {words}")),
        ClientError::Io(_) | ClientError::Disconnected(_) | ClientError::Transport(_) => Fate::Retry,
        _ => Fate::Fatal,
    }
}

/// The owner's row for a root: its state and the prose the row carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub(super) state: RowState,
    pub(super) words: String,
}

/// Whether the owner has listed its rows yet: a snapshot with no row for a
/// root means "not loaded" until the owner has listed rows at all (or has
/// been given long enough to).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Listing {
    /// The catalogue may still be loading.
    Awaited,
    /// The owner has answered with its rows; a root it lists no row for has none.
    Settled,
}

/// Where a fixture root stands while the index settles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Standing {
    /// No row yet, or the row is still loading.
    Pending,
    /// The row's content is available and the owner took the index request.
    Ready,
    /// The owner refused the refresh and serves what it kept: its prior
    /// generation. Pages are real; they are not fresh.
    Preserved(String),
    /// Nothing to serve: the owner refused the root and holds no content for
    /// it, or recorded its row as failed. The words are the owner's.
    Failed(String),
}

/// A root's standing from what the index request answered and the owner's
/// row for it.
///
/// A refusal decides only once the owner has answered for the row: a row
/// that is still loading, or not listed while the catalogue may still be
/// loading, is `Pending`. A Ready row wins over a refused refresh (the owner
/// keeps the prior generation and serves it) and is `Preserved`, so the
/// capture can say what it is a capture of.
pub(super) fn standing(reply: &Reply, row: Option<&Row>, listing: Listing) -> Standing {
    let refusal = match reply {
        Reply::Refused(words) => Some(words.as_str()),
        Reply::Accepted => None,
    };
    match (row.map(|row| (row.state, row.words.as_str())), refusal) {
        (Some((RowState::Ready, _)), None) => Standing::Ready,
        (Some((RowState::Ready, _)), Some(words)) => Standing::Preserved(words.to_owned()),
        (Some((RowState::Loading, _)), _) => Standing::Pending,
        (Some((RowState::Failed, "")), refusal) => {
            Standing::Failed(refusal.unwrap_or("the owner recorded this root's row as Failed and gave no reason").to_owned())
        }
        (Some((RowState::Failed, words)), _) => Standing::Failed(words.to_owned()),
        (None, Some(words)) if listing == Listing::Settled => Standing::Failed(words.to_owned()),
        (None, _) => Standing::Pending,
    }
}

/// The wait is over when no root is still pending.
pub(super) fn settled(standings: &[Standing]) -> bool {
    !standings.is_empty() && standings.iter().all(|standing| *standing != Standing::Pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::CommandFailure;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory that goes when the test does, whether it passes or panics.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(what: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!("nudox-refusals-{what}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }

        fn root(&self, name: &str, source: &str) -> PathBuf {
            let root = self.0.join(name);
            std::fs::create_dir_all(root.join("src")).expect("root dir");
            std::fs::write(root.join("src/lib.rs"), source).expect("source");
            root
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pairs(list: &[(&str, &str)]) -> ToolchainEnv {
        ToolchainEnv::from_pairs(list.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())))
    }

    const DEV_AUTHORITY: &str = "cargo-home=/cargo; registry-indices=/cargo/registry/index";

    fn dev_environment() -> Environment {
        Environment::of(
            OwnerBuild { length: 100, modified_ns: 7 },
            &pairs(&[("NUDOX_RUSTC", "/nix/store/x/bin/rustc"), ("NUDOX_GO", "/nix/store/y/bin/go"), ("LIBCLANG_PATH", "/nix/store/z/lib")]),
            DEV_AUTHORITY,
        )
    }

    const WORDS: &str = "protocol: command execution failed: package semantic compilation failed";

    #[test]
    fn a_refusal_is_read_back_under_the_same_owner_environment_and_root_and_only_then() {
        let scratch = Scratch::new("readback");
        let root = scratch.root("present", "fn a() {}");
        let refused = BTreeMap::from([(root.clone(), format!("{WORDS}\n\twith a tab"))]);
        Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).save(&refused);
        let read = Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment());
        assert_eq!(read.recorded(&root), Some(format!("{WORDS}  with a tab").as_str()), "one line per root, whatever the owner's words held");
        // Another owner (a rebuilt binary) asks again.
        let rebuilt = Environment::of(
            OwnerBuild { length: 101, modified_ns: 7 },
            &pairs(&[("NUDOX_RUSTC", "/nix/store/x/bin/rustc"), ("NUDOX_GO", "/nix/store/y/bin/go"), ("LIBCLANG_PATH", "/nix/store/z/lib")]),
            DEV_AUTHORITY,
        );
        assert_eq!(Refusals::load_under(&scratch.0, &[root.clone()], &rebuilt).recorded(&root), None);
        // Another root content asks again; an untouched one does not.
        let other = scratch.root("runtime", "fn b() {}");
        Refusals::load_under(&scratch.0, &[root.clone(), other.clone()], &dev_environment()).save(&BTreeMap::from([(root.clone(), WORDS.to_owned()), (other.clone(), WORDS.to_owned())]));
        std::fs::write(root.join("src/lib.rs"), "fn a() { changed() }").expect("edit");
        let after = Refusals::load_under(&scratch.0, &[root.clone(), other.clone()], &dev_environment());
        assert_eq!(after.recorded(&root), None, "the root's own bytes changed");
        assert_eq!(after.recorded(&other), Some(WORDS), "the other root's did not");
    }

    /// A run without the toolchain environment refuses every root with
    /// `Toolchain { … }`. Recorded under the same key as a run with it, that
    /// refusal was replayed as a fact for every later run in the dev shell
    /// (13 of 13 roots never indexed again). The environment is in the key.
    #[test]
    fn a_run_without_the_toolchain_environment_never_poisons_a_later_run() {
        let scratch = Scratch::new("poison");
        let root = scratch.root("present", "fn a() {}");
        let bare = Environment::of(OwnerBuild { length: 100, modified_ns: 7 }, &pairs(&[("HOME", "/Users/x"), ("PATH", "/usr/bin")]), DEV_AUTHORITY);
        let toolchain = "protocol: command execution failed: … Toolchain { stage: LowerIr, selected: Rustc, configured: None }";
        Refusals::load_under(&scratch.0, &[root.clone()], &bare).save(&BTreeMap::from([(root.clone(), toolchain.to_owned())]));
        // The dev shell (the same executable, the toolchain variables set) is a different question.
        assert_eq!(Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).recorded(&root), None);
        // And the other way: what the dev shell was told is not replayed to a bare run.
        Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).save(&BTreeMap::from([(root.clone(), WORDS.to_owned())]));
        assert_eq!(Refusals::load_under(&scratch.0, &[root.clone()], &bare).recorded(&root), None);
        assert_eq!(Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).recorded(&root), Some(WORDS));
    }

    #[test]
    fn only_the_variables_the_owner_reads_are_part_of_the_environment() {
        let env = pairs(&[
            ("PATH", "/usr/bin"),
            ("NUDOX_RUSTC", "/r"),
            ("NUDOX_HARNESS_STATE", "/state/a"),
            ("NUDOX_REVIEW_DIAGNOSTICS", "1"),
            ("HOME", "/Users/x"),
            ("LIBCLANG_PATH", "/l"),
        ]);
        assert_eq!(env.0, [("LIBCLANG_PATH".to_owned(), "/l".to_owned()), ("NUDOX_RUSTC".to_owned(), "/r".to_owned())]);
        // Where this run keeps its state is not a toolchain: a cloned state dir keeps its record.
        assert_eq!(
            Environment::of(OwnerBuild::default(), &pairs(&[("NUDOX_RUSTC", "/r"), ("NUDOX_HARNESS_STATE", "/state/a")]), DEV_AUTHORITY),
            Environment::of(OwnerBuild::default(), &pairs(&[("NUDOX_RUSTC", "/r"), ("NUDOX_HARNESS_STATE", "/state/b")]), DEV_AUTHORITY)
        );
        assert_ne!(
            dev_environment(),
            Environment::of(OwnerBuild { length: 100, modified_ns: 7 }, &ToolchainEnv::default(), DEV_AUTHORITY)
        );
        assert_ne!(
            dev_environment(),
            Environment::of(
                OwnerBuild { length: 100, modified_ns: 7 },
                &pairs(&[("NUDOX_RUSTC", "/nix/store/x/bin/rustc"), ("NUDOX_GO", "/nix/store/y/bin/go"), ("LIBCLANG_PATH", "/nix/store/z/lib")]),
                "cargo-home=/different; registry-indices=/different/registry/index",
            ),
            "a different effective Cargo authority asks again",
        );
    }

    #[test]
    fn the_record_is_written_whole_and_a_torn_or_foreign_one_is_not_read() {
        let scratch = Scratch::new("whole");
        let root = scratch.root("present", "fn a() {}");
        let record = scratch.0.join(RECORD_FILE);
        Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).save(&BTreeMap::from([(root.clone(), WORDS.to_owned())]));
        let leftovers = std::fs::read_dir(&scratch.0).expect("dir").flatten().filter(|entry| entry.file_name().to_string_lossy().contains("tmp-")).count();
        assert_eq!(leftovers, 0, "the temporary file was renamed into place");
        // A record cut off mid-line is not trusted for that root; another format's header is not read at all.
        let text = std::fs::read_to_string(&record).expect("record");
        let torn = text.trim_end().rsplit_once('\t').expect("root, key and reason").0;
        std::fs::write(&record, torn).expect("tear it");
        assert_eq!(Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).recorded(&root), None);
        std::fs::write(&record, text.replacen(RECORD_HEADER, "some-other-format 1", 1)).expect("foreign header");
        assert_eq!(Refusals::load_under(&scratch.0, &[root.clone()], &dev_environment()).recorded(&root), None);
        // Nothing refused: no record. The record an earlier harness wrote is retired with it.
        std::fs::write(scratch.0.join(RETIRED_RECORD_FILE), "owner-source:1:0\n").expect("retired record");
        Refusals::load_under(&scratch.0, &[root], &dev_environment()).save(&BTreeMap::new());
        assert!(!record.exists() && !scratch.0.join(RETIRED_RECORD_FILE).exists());
    }

    #[test]
    fn only_the_owners_own_no_is_an_answer_and_a_dead_connection_is_asked_again() {
        let ran = ProtocolError::CommandExecution("local semantic compilation failed".to_owned()).to_string();
        assert_eq!(fate(&ClientError::Protocol(ran.clone())), Fate::Refused(format!("protocol: {ran}")));
        for failure in [CommandFailure::MutationRequiresOwner, CommandFailure::NotFound, CommandFailure::InvalidQuery("bad query".to_owned())] {
            assert_eq!(fate(&ClientError::CommandFailed(failure)), Fate::Fatal, "a typed command/query rejection is not an index refusal");
        }
        for dead in [ClientError::Io("Connection refused".to_owned()), ClientError::Disconnected(std::io::ErrorKind::BrokenPipe)] {
            assert_eq!(fate(&dead), Fate::Retry, "{dead}");
        }
        // A frame the client could not admit is neither an answer nor a dead line.
        assert_eq!(fate(&ClientError::Protocol("unknown continuation token".to_owned())), Fate::Fatal);
        assert_eq!(fate(&ClientError::IncoherentView), Fate::Fatal);
    }

    fn row(state: RowState, words: &str) -> Row {
        Row { state, words: words.to_owned() }
    }

    fn refused() -> Reply {
        Reply::Refused("local semantic compilation failed".to_owned())
    }

    #[test]
    fn a_refusal_decides_only_after_the_owner_has_answered_for_the_row() {
        // Loading, or not listed while the catalogue may still be loading: the owner may yet serve it.
        assert_eq!(standing(&refused(), Some(&row(RowState::Loading, "")), Listing::Settled), Standing::Pending);
        assert_eq!(standing(&refused(), Some(&row(RowState::Loading, "")), Listing::Awaited), Standing::Pending);
        assert_eq!(standing(&refused(), None, Listing::Awaited), Standing::Pending);
        assert_eq!(standing(&Reply::Accepted, None, Listing::Settled), Standing::Pending);
        // The owner listed its rows and has none for this root: nothing to serve.
        assert_eq!(standing(&refused(), None, Listing::Settled), Standing::Failed("local semantic compilation failed".to_owned()));
    }

    #[test]
    fn a_ready_row_under_a_refused_refresh_is_preserved_data_and_says_so() {
        assert_eq!(standing(&Reply::Accepted, Some(&row(RowState::Ready, "")), Listing::Settled), Standing::Ready);
        assert_eq!(
            standing(&refused(), Some(&row(RowState::Ready, "")), Listing::Settled),
            Standing::Preserved("local semantic compilation failed".to_owned()),
            "the owner serves the prior generation: real pages, not fresh ones"
        );
    }

    #[test]
    fn a_failed_row_is_failed_in_the_owners_words_or_the_refusals() {
        assert_eq!(standing(&Reply::Accepted, Some(&row(RowState::Failed, "assemble.rs: LowerIr")), Listing::Settled), Standing::Failed("assemble.rs: LowerIr".to_owned()));
        assert_eq!(standing(&refused(), Some(&row(RowState::Failed, "")), Listing::Settled), Standing::Failed("local semantic compilation failed".to_owned()));
        assert!(matches!(standing(&Reply::Accepted, Some(&row(RowState::Failed, "")), Listing::Settled), Standing::Failed(words) if !words.is_empty()), "a failed row that carries no prose still says why");
    }

    #[test]
    fn the_wait_settles_when_no_root_is_pending_and_only_then() {
        let ready = Standing::Ready;
        let preserved = Standing::Preserved("refused".to_owned());
        let failed = Standing::Failed("no".to_owned());
        assert!(settled(&[ready.clone(), preserved.clone(), failed.clone()]));
        assert!(!settled(&[ready, preserved, failed, Standing::Pending]));
        assert!(!settled(&[]), "an empty fixture list cannot settle vacuously");
    }

    #[test]
    fn a_root_that_is_not_there_and_a_root_that_changed_digest_differently() {
        let scratch = Scratch::new("digest");
        let root = scratch.root("present", "fn a() {}");
        let before = root_digest(&root);
        assert_eq!(before, root_digest(&root), "the same bytes, the same digest");
        std::fs::write(root.join("src/lib.rs"), "fn a() { more() }").expect("edit");
        assert_ne!(before, root_digest(&root));
        std::fs::create_dir_all(root.join("target")).expect("target");
        std::fs::write(root.join("target/big.rlib"), "artefact").expect("artefact");
        let with_target = root_digest(&root);
        std::fs::write(root.join("target/big.rlib"), "another artefact").expect("artefact");
        assert_eq!(with_target, root_digest(&root), "a build directory is not the root's source");
        assert_ne!(root_digest(&scratch.0.join("absent")), root_digest(&root));
    }
}
