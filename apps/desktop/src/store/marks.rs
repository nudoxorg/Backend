//! What the reader has marked and where they have been: pins and recents.
//! Both persist beside the workspace, one line per entry, written atomically.
//! Neither is a fact about the engine; both are facts about this reader.
//!
//! Preferences are a fixed set of switches and stay `Copy`; marks are lists
//! that grow, so they live in their own file with the same discipline — a
//! hand-rolled `key = value` codec, a sibling temporary renamed into place,
//! unknown lines ignored, a corrupt file degrading to nothing marked rather
//! than to a window that will not open.
//!
//! A recent entry is a subject the reader opened on purpose, with what it
//! takes to open it again: a project by its coordinate, a package by its
//! pinned URL, a declaration by its exact coordinate, which the live root
//! resolves back to a key when it is opened. The list is newest first and
//! bounded, because "recently" means a screenful.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::persist;

/// File name of the marks file inside the workspace data directory.
pub(crate) const MARKS_FILE: &str = "desktop.marks";

/// Largest marks file this build will read.
const MAX_BYTES: u64 = 256 * 1024;

/// How many recent subjects are remembered.
pub(crate) const RECENT: usize = 24;

/// How many pins are kept.
const PINS: usize = 64;

/// One subject the reader opened, as it can be opened again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Recent {
    /// A project on the shelf.
    Project {
        /// Exact project coordinate.
        coordinate: String,
    },
    /// A registry package page.
    Package {
        /// Pinned package URL.
        coordinate: String,
    },
    /// A declaration page.
    Declaration {
        /// Exact producer coordinate.
        coordinate: String,
    },
}

impl Recent {
    /// Returns the coordinate this entry is keyed by.
    pub(crate) fn coordinate(&self) -> &str {
        match self {
            Self::Project { coordinate }
            | Self::Package { coordinate }
            | Self::Declaration { coordinate } => coordinate,
        }
    }

    /// Returns the word a list draws before the name.
    pub(crate) const fn noun(&self) -> &'static str {
        match self {
            Self::Project { .. } => "project",
            Self::Package { .. } => "package",
            Self::Declaration { .. } => "declaration",
        }
    }

    fn encode(&self) -> String {
        match self {
            Self::Project { coordinate } => format!("project {coordinate}"),
            Self::Package { coordinate } => format!("package {coordinate}"),
            Self::Declaration { coordinate } => format!("symbol {coordinate}"),
        }
    }

    fn decode(value: &str) -> Option<Self> {
        let (kind, rest) = value.split_once(' ')?;
        match kind {
            "project" => Some(Self::Project {
                coordinate: rest.to_owned(),
            }),
            "package" => Some(Self::Package {
                coordinate: rest.to_owned(),
            }),
            "symbol" => Some(Self::Declaration {
                coordinate: rest.to_owned(),
            }),
            _ => None,
        }
    }
}

/// Everything the reader has marked.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Marks {
    pinned: Vec<String>,
    recent: Vec<Recent>,
}

impl Marks {
    /// Returns the pinned coordinates, in the order they were pinned.
    pub(crate) fn pinned(&self) -> &[String] {
        &self.pinned
    }

    /// Returns the recently opened subjects, newest first.
    pub(crate) fn recent(&self) -> &[Recent] {
        &self.recent
    }

    /// Returns whether one coordinate is pinned.
    pub(crate) fn is_pinned(&self, coordinate: &str) -> bool {
        self.pinned.iter().any(|held| held == coordinate)
    }

    /// Pins a coordinate, or unpins it when it already is; returns the new state.
    pub(crate) fn toggle_pin(&mut self, coordinate: &str) -> bool {
        if self.is_pinned(coordinate) {
            self.pinned.retain(|held| held != coordinate);
            return false;
        }
        self.pinned.push(coordinate.to_owned());
        while self.pinned.len() > PINS {
            self.pinned.remove(0);
        }
        true
    }

    /// Forgets a pin, for a project that has left the shelf.
    pub(crate) fn unpin(&mut self, coordinate: &str) {
        self.pinned.retain(|held| held != coordinate);
    }

    /// Records one subject as the most recently opened.
    pub(crate) fn remember(&mut self, entry: Recent) {
        self.recent.retain(|held| held != &entry);
        self.recent.insert(0, entry);
        self.recent.truncate(RECENT);
    }

    /// Forgets every recent entry under one project root.
    pub(crate) fn forget_project(&mut self, root: &str) {
        self.recent
            .retain(|held| !held.coordinate().starts_with(root));
    }

    /// Encodes the marks as the exact file text.
    pub(crate) fn encode(&self) -> String {
        let mut text = String::with_capacity(64 * (self.pinned.len() + self.recent.len()));
        for pin in &self.pinned {
            let _ = writeln!(text, "pin = {pin}");
        }
        for entry in &self.recent {
            let _ = writeln!(text, "recent = {}", entry.encode());
        }
        text
    }

    /// Decodes marks, ignoring anything malformed.
    pub(crate) fn decode(text: &str) -> Self {
        let mut marks = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "pin" if !value.trim().is_empty() => marks.pinned.push(value.trim().to_owned()),
                "recent" => {
                    if let Some(entry) = Recent::decode(value.trim()) {
                        marks.recent.push(entry);
                    }
                }
                _ => {}
            }
        }
        marks.pinned.dedup();
        marks.recent.truncate(RECENT);
        marks
    }
}

/// Returns the marks path beside one workspace data directory.
pub(crate) fn path_in(data: &Path) -> PathBuf {
    data.join(MARKS_FILE)
}

/// Reads marks, falling back to nothing marked for any read or parse failure.
pub(crate) fn load(data: &Path) -> Marks {
    load_with_diagnostic(data).marks
}

/// Typed reason a marks file was not admitted exactly as written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarksDiagnostic {
    /// No file exists yet; an empty shelf is the expected first-run state.
    Missing,
    /// The file exceeded the bounded reader budget.
    Oversized,
    /// The file could not be read as UTF-8.
    Unreadable,
    /// At least one line was malformed or could not be admitted.
    Malformed,
}

/// Marks plus a bounded, machine-checkable startup observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoadedMarks {
    /// Value installed in the shell.
    pub(crate) marks: Marks,
    /// Why the reader used defaults or discarded a line, when applicable.
    pub(crate) diagnostic: Option<MarksDiagnostic>,
}

/// Reads marks while retaining a typed recovery observation.
pub(crate) fn load_with_diagnostic(data: &Path) -> LoadedMarks {
    let path = path_in(data);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedMarks {
                marks: Marks::default(),
                diagnostic: Some(MarksDiagnostic::Missing),
            };
        }
        Err(_) => {
            return LoadedMarks {
                marks: Marks::default(),
                diagnostic: Some(MarksDiagnostic::Unreadable),
            };
        }
    };
    if metadata.len() > MAX_BYTES {
        return LoadedMarks {
            marks: Marks::default(),
            diagnostic: Some(MarksDiagnostic::Oversized),
        };
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let marks = Marks::decode(&text);
            let malformed = text.lines().any(|line| {
                let Some((key, value)) = line.split_once('=') else {
                    return !line.trim().is_empty();
                };
                match key.trim() {
                    "pin" => value.trim().is_empty(),
                    "recent" => Recent::decode(value.trim()).is_none(),
                    _ => false,
                }
            });
            LoadedMarks {
                marks,
                diagnostic: malformed.then_some(MarksDiagnostic::Malformed),
            }
        }
        Err(_) => LoadedMarks {
            marks: Marks::default(),
            diagnostic: Some(MarksDiagnostic::Unreadable),
        },
    }
}

/// Writes marks atomically beside the workspace data directory.
pub(crate) fn save(data: &Path, marks: &Marks) -> Result<(), std::io::Error> {
    let path = path_in(data);
    persist::atomic_write(&path, marks.encode().as_bytes())
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn data_directory(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nudox-marks-{label}-{}-{}",
            std::process::id(),
            FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("marks fixture");
        root
    }

    #[test]
    fn marks_survive_a_cold_restart_in_the_same_order() {
        let root = data_directory("restart");
        let mut expected = Marks::default();
        expected.toggle_pin("/workspace");
        expected.remember(Recent::Project {
            coordinate: "/workspace".to_owned(),
        });
        expected.remember(Recent::Package {
            coordinate: "pkg:cargo/demo@1.0.0".to_owned(),
        });
        save(&root, &expected).expect("save marks");
        assert_eq!(load(&root), expected);
        fs::remove_dir_all(root).expect("remove marks fixture");
    }

    #[test]
    fn concurrent_mark_saves_never_publish_a_torn_line() {
        let root = Arc::new(data_directory("concurrent"));
        let mut writers = Vec::new();
        for index in 0..16_u8 {
            let root = Arc::clone(&root);
            writers.push(thread::spawn(move || {
                let mut marks = Marks::default();
                marks.toggle_pin(&format!("/workspace/{index}"));
                save(&root, &marks).expect("save concurrent marks");
            }));
        }
        for writer in writers {
            writer.join().expect("mark writer");
        }
        let loaded = load(&root);
        assert_eq!(loaded.pinned().len(), 1, "a torn marks file was admitted");
        assert!(loaded.pinned()[0].starts_with("/workspace/"));
        fs::remove_dir_all(&*root).expect("remove marks fixture");
    }

    #[test]
    fn truncated_marks_retain_valid_lines_with_a_typed_diagnostic() {
        let root = data_directory("corrupt");
        fs::write(path_in(&root), "pin = /workspace\nrecent = project").expect("truncated marks");
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.marks.pinned(), &[String::from("/workspace")]);
        assert!(loaded.marks.recent().is_empty());
        assert_eq!(loaded.diagnostic, Some(MarksDiagnostic::Malformed));
        fs::remove_dir_all(root).expect("remove marks fixture");
    }

    #[test]
    fn oversized_marks_are_not_partially_admitted() {
        let root = data_directory("oversized");
        fs::write(
            path_in(&root),
            vec![b'x'; (MAX_BYTES as usize).saturating_add(1)],
        )
        .expect("oversized marks");
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.marks, Marks::default());
        assert_eq!(loaded.diagnostic, Some(MarksDiagnostic::Oversized));
        fs::remove_dir_all(root).expect("remove marks fixture");
    }
}
