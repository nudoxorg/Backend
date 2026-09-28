//! Discovered oracles for untrusted wire decoders.
//!
//! `cargo test -p backend-fuzz` replays each `targets/<id>/corpus` under a
//! fixed budget. Always-on runs are `.#continuous-fuzz`, built with
//! `--cfg fuzzing` and `--cfg fuzzing_libfuzzer`. CI passes neither cfg.

#![forbid(unsafe_code)]

use std::fs::DirEntry;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[path = "laws.rs"]
mod laws;
mod discovered {
    include!(concat!(env!("OUT_DIR"), "/dispatch.rs"));
}

pub use laws::{LawsRegression, retained_law_cases};

/// Iterations used when the binary is the bounded engine.
pub const BOUNDED_ITERATIONS: usize = 16;
/// Wall-clock cap for the bounded engine.
pub const BOUNDED_MILLIS: u64 = 150;

/// One harness law was violated.
///
/// Bolero's `IntoResult` only keeps the failure text when the error
/// implements [`std::error::Error`]. A `String` becomes a compile error, and
/// a bare `false` drops the reason.
#[derive(Debug)]
pub struct OracleFailure {
    message: String,
}

impl OracleFailure {
    /// Records one law violation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for OracleFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OracleFailure {}

/// Oracle invoked by the bounded engine, libFuzzer, and seed replay.
pub type Oracle = fn(&[u8]) -> Result<(), OracleFailure>;

/// Whether a buffer was admitted by a grammar under test.
///
/// Rejection is a successful oracle result. Callers that need to prove a seed
/// was refused must match this verdict; `Result::Ok` alone does not mean the
/// decoder accepted the bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Every grammar refused the buffer.
    Rejected,
    /// At least one grammar admitted the buffer and the fixpoint laws held.
    Accepted,
}

/// Classifier used by seed tests. Law failures stay in the error channel.
pub type Judge = fn(&[u8]) -> Result<Verdict, OracleFailure>;

/// One discovered harness.
///
/// `build.rs` fills this table from `targets/<id>/`. The flake reads the same
/// directories, so a new id appears in Rust and in `.#continuous-fuzz` together.
pub struct Target {
    /// Directory name and flake attr key.
    pub name: &'static str,
    /// Byte cap shared with `targets/<id>/max_len`.
    pub max_len: usize,
    /// Bolero entry. Clean rejection is success.
    pub exercise: Oracle,
    /// Seed classifier. Clean rejection is [`Verdict::Rejected`].
    pub judge: Judge,
    /// Fresh encode of the committed `canonical` seed.
    pub canonical: fn() -> Result<Vec<u8>, String>,
}

/// Harnesses discovered under `tests/fuzz/targets`, sorted by id.
#[must_use]
pub fn targets() -> &'static [Target] {
    discovered::TARGETS
}

/// Returns the harness whose directory name is `name`.
#[must_use]
pub fn target(name: &str) -> Option<&'static Target> {
    targets().iter().find(|candidate| candidate.name == name)
}

/// Parses the decimal integer stored in `targets/<id>/max_len`.
///
/// Trailing ASCII whitespace is ignored so Nix's `removeSuffix` of a newline
/// and this const parser accept the same file.
///
/// # Panics
///
/// Panics during const evaluation when `text` is empty, overflows `usize`,
/// or contains a non-digit before the trailing whitespace.
#[must_use]
pub const fn decimal_usize(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut value = 0usize;
    let mut digits = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' || byte == b'\r' || byte == b' ' || byte == b'\t' {
            break;
        }
        assert!(byte.is_ascii_digit(), "max_len must be a decimal integer");
        let digit = (byte - b'0') as usize;
        assert!(
            value <= (usize::MAX - digit) / 10,
            "max_len overflows usize"
        );
        value = value * 10 + digit;
        digits += 1;
        index += 1;
    }
    assert!(digits > 0, "max_len is empty");
    while index < bytes.len() {
        let byte = bytes[index];
        assert!(
            byte == b'\n' || byte == b'\r' || byte == b' ' || byte == b'\t',
            "max_len has trailing junk"
        );
        index += 1;
    }
    value
}

/// Drives one oracle.
///
/// The libFuzzer build ignores iteration caps and does not return. Every
/// other build refuses `BOLERO_RANDOM_ITERATIONS`, `BOLERO_RANDOM_TEST_TIME_MS`,
/// and `BOLERO_RANDOM_MAX_LEN`. Bolero applies those variables before
/// `with_iterations`, and `std::env::remove_var` is unsafe on this toolchain,
/// so the harness fails closed instead of inheriting an overnight budget.
pub fn drive(max_len: usize, exercise: Oracle) {
    if cfg!(fuzzing_libfuzzer) {
        bolero::check!().with_max_len(max_len).for_each(exercise);
        return;
    }
    if let Some(name) = inherited_random_budget() {
        eprintln!(
            "{name} is set. Bolero would honor it over the {BOUNDED_ITERATIONS}-iteration harness cap, and this crate does not unset environment variables. Unset {name}. Always-on runs use .#continuous-fuzz.bins.<id>."
        );
        std::process::exit(2);
    }
    bolero::check!()
        .with_iterations(BOUNDED_ITERATIONS)
        .with_test_time(Duration::from_millis(BOUNDED_MILLIS))
        .with_max_len(max_len)
        .for_each(exercise);
}

/// Returns the first bolero random-budget variable that would override the cap.
#[must_use]
pub fn inherited_random_budget() -> Option<&'static str> {
    const KEYS: [&str; 3] = [
        "BOLERO_RANDOM_ITERATIONS",
        "BOLERO_RANDOM_TEST_TIME_MS",
        "BOLERO_RANDOM_MAX_LEN",
    ];
    KEYS.into_iter().find(|key| std::env::var_os(key).is_some())
}

/// Replays every regular file in `dir` through `exercise`.
///
/// Dotfiles and directories are skipped. Symlinks are a hard error so a
/// corpus entry cannot alias a secret or a file outside the directory.
///
/// # Errors
///
/// Returns the first oracle failure, or an error when the directory is
/// missing, empty, or contains a symlink or an oversized seed.
pub fn replay_seeds(dir: &Path, max_len: usize, exercise: Oracle) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("seed directory {}: {error}", dir.display()))?;
    let mut seen = 0usize;
    for entry in entries {
        let entry = entry.map_err(|error| format!("seed directory entry: {error}"))?;
        let Some(path) = regular_seed(&entry)? else {
            continue;
        };
        exercise_file(&path, max_len, exercise)?;
        seen += 1;
    }
    if seen == 0 {
        return Err(format!(
            "seed directory {} has no regular files",
            dir.display()
        ));
    }
    Ok(())
}

/// Requires one flat file name to exist and to exercise the named outcome.
///
/// # Errors
///
/// Returns when the name is not a single path segment, or when the seed is
/// missing, oversized, a symlink, or has the wrong outcome.
pub fn require_seed(
    dir: &Path,
    name: &str,
    max_len: usize,
    expect_accepted: bool,
    judge: Judge,
) -> Result<(), String> {
    if !is_flat_seed_name(name) {
        return Err(format!(
            "seed name {name} must be a single path segment without '.' or '..'"
        ));
    }
    let path = dir.join(name);
    let bytes = read_regular(&path, max_len)?;
    let wanted = if expect_accepted {
        Verdict::Accepted
    } else {
        Verdict::Rejected
    };
    match judge(&bytes) {
        Ok(verdict) if verdict == wanted => Ok(()),
        Ok(Verdict::Accepted) => Err(format!(
            "{} was accepted; this seed must be rejected",
            path.display()
        )),
        Ok(Verdict::Rejected) => Err(format!(
            "{} was rejected; this seed must be accepted",
            path.display()
        )),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Renders bytes as lowercase hex so a drifted seed can be replaced exactly.
#[must_use]
pub fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn is_flat_seed_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

fn regular_seed(entry: &DirEntry) -> Result<Option<PathBuf>, String> {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    if name.starts_with('.') {
        return Ok(None);
    }
    let file_type = entry
        .file_type()
        .map_err(|error| format!("seed type for {name}: {error}"))?;
    if file_type.is_symlink() {
        return Err(format!(
            "{} is a symlink; corpus entries must be regular files",
            entry.path().display()
        ));
    }
    if !file_type.is_file() {
        return Ok(None);
    }
    Ok(Some(entry.path()))
}

fn read_regular(path: &Path, max_len: usize) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("required seed {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    if metadata.len() > max_len as u64 {
        return Err(format!(
            "{} is {} bytes; the harness cap is {max_len}",
            path.display(),
            metadata.len()
        ));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    if bytes.len() > max_len {
        return Err(format!(
            "{} is {} bytes; the harness cap is {max_len}",
            path.display(),
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn exercise_file(path: &Path, max_len: usize, exercise: Oracle) -> Result<(), String> {
    let bytes = read_regular(path, max_len)?;
    exercise(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_dir() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn corpus_of(target: &Target) -> PathBuf {
        manifest_dir()
            .join("targets")
            .join(target.name)
            .join("corpus")
    }

    #[test]
    fn every_target_directory_is_discovered() -> Result<(), String> {
        let root = manifest_dir().join("targets");
        let mut dirs = Vec::new();
        let entries = std::fs::read_dir(&root)
            .map_err(|error| format!("reading {}: {error}", root.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("target entry: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("target type: {error}"))?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            dirs.push(name.into_owned());
        }
        dirs.sort();
        let found: Vec<&str> = targets().iter().map(|target| target.name).collect();
        if dirs != found {
            return Err(format!("discovered {found:?} but directories are {dirs:?}"));
        }
        Ok(())
    }

    #[test]
    fn discovered_seeds_cover_reject_and_accept() -> Result<(), String> {
        if targets().is_empty() {
            return Err("no fuzz targets were discovered".to_string());
        }
        for target in targets() {
            let dir = corpus_of(target);
            require_seed(&dir, "empty", target.max_len, false, target.judge)?;
            require_seed(&dir, "bad_magic", target.max_len, false, target.judge)?;
            let stored = std::fs::read(dir.join("canonical"))
                .map_err(|error| format!("{} canonical seed: {error}", target.name))?;
            let fresh = (target.canonical)()?;
            if stored != fresh {
                return Err(format!(
                    "{} canonical drifted from a fresh encode (stored {} bytes, fresh {} bytes, fresh hex {})",
                    target.name,
                    stored.len(),
                    fresh.len(),
                    hex_bytes(&fresh)
                ));
            }
            require_seed(&dir, "canonical", target.max_len, true, target.judge)?;
            replay_seeds(&dir, target.max_len, target.exercise)?;
        }
        Ok(())
    }

    #[test]
    fn bounded_engine_has_no_inherited_budget() -> Result<(), String> {
        match inherited_random_budget() {
            None => Ok(()),
            Some(name) => Err(format!(
                "{name} is set; cargo test -p backend-fuzz must not inherit a bolero random budget"
            )),
        }
    }

    /// The bounded engine is a smoke cap, not evidence that the grammar is
    /// free of bugs. Acceptance is the committed canonical seed.
    #[cfg(not(fuzzing_libfuzzer))]
    #[test]
    fn bounded_wire_stays_inside_the_cap() {
        for target in targets() {
            drive(target.max_len, target.exercise);
        }
    }

    #[test]
    fn retained_law_cases_are_not_byte_seeds() -> Result<(), String> {
        let path = manifest_dir().join("../laws/proptest-regressions/lib.txt");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        let cases = retained_law_cases(&text);
        if cases.is_empty() {
            return Err(
                "tests/laws/proptest-regressions/lib.txt has no retained cc seeds".to_string(),
            );
        }
        for case in &cases {
            if case.shrink.contains("bytes") {
                return Err(format!(
                    "law seed {} shrinks to bytes; copy that minimized input into targets/<id>/corpus/<flat-name> instead of treating the hash as a seed",
                    case.fingerprint
                ));
            }
        }
        for target in targets() {
            let corpus = std::fs::read_dir(corpus_of(target))
                .map_err(|error| format!("{} corpus: {error}", target.name))?;
            for entry in corpus {
                let entry = entry.map_err(|error| format!("corpus entry: {error}"))?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.contains(&cases[0].fingerprint[..16]) {
                    return Err(format!(
                        "{name} looks like a proptest fingerprint; those hashes are not decoder inputs"
                    ));
                }
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replay_rejects_symlink_seeds() -> Result<(), String> {
        let Some(target) = targets().first() else {
            return Err("no fuzz targets were discovered".to_string());
        };
        let root =
            std::env::temp_dir().join(format!("backend-fuzz-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("temp corpus {}: {error}", root.display()))?;
        let real = root.join("real");
        std::fs::write(&real, b"XXXX")
            .map_err(|error| format!("temp seed {}: {error}", real.display()))?;
        std::os::unix::fs::symlink(&real, root.join("alias"))
            .map_err(|error| format!("temp symlink: {error}"))?;
        let replayed = replay_seeds(&root, target.max_len, target.exercise);
        if let Err(error) = std::fs::remove_dir_all(&root) {
            return Err(format!(
                "cleanup {}: {error}; replay result was {replayed:?}",
                root.display()
            ));
        }
        match replayed {
            Err(error) if error.contains("symlink") => Ok(()),
            Err(error) => Err(format!("expected a symlink rejection, got {error}")),
            Ok(()) => Err("symlink corpus must not replay".to_string()),
        }
    }

    #[test]
    fn seed_names_stay_inside_the_corpus_directory() -> Result<(), String> {
        let Some(target) = targets().first() else {
            return Err("no fuzz targets were discovered".to_string());
        };
        let dir = corpus_of(target);
        match require_seed(&dir, "../Cargo.toml", 64, false, target.judge) {
            Err(error) if error.contains("single path segment") => Ok(()),
            Err(error) => Err(format!("unexpected rejection: {error}")),
            Ok(()) => Err("parent traversal must be rejected before any read".to_string()),
        }
    }
}
