//! What a shelf row says about a project beyond its readiness.
//!
//! [`backend_present::ShelfEntry`] carries the facts the engine published: the
//! identity, the readiness, and the per-language declaration counts. A shelf
//! row in a window shows two more things, and both are read off the identity
//! rather than fetched: whether the project is a folder on this machine or a
//! pinned registry package, and the one-word badge that says which.
//!
//! It also holds the merge that turns "what the engine published" into "what
//! the reader asked for", because a window has state the engine does not: an
//! index request that has been accepted and has produced no rows yet still
//! belongs on the shelf, marked as exactly that.

use backend_present::{Identity, Readiness, Shelf, ShelfEntry};

/// What a shelf row can actually offer a reader right now.
///
/// This is not the engine's readiness re-spelled; it is readiness *and* what
/// was published, resolved into one statement. The engine can commit a package
/// row as ready while that package has contributed no declarations — a pinned
/// coordinate it accepted and then found nothing in — and a row that drew a ✓
/// beside "0 declarations" would be telling the reader two different things at
/// once. A project with nothing in it is not finished; it is empty, and this
/// window says so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Standing {
    /// Declarations are published and readable.
    Readable,
    /// The engine calls this ready and published nothing under it.
    Empty,
    /// Rows are arriving.
    Indexing,
    /// Accepted, with no rows yet.
    Requested,
    /// Refused; the entry carries the fault.
    Failed,
}

/// Returns what one shelf row can offer, readiness and row count together.
pub(crate) fn standing(entry: &ShelfEntry) -> Standing {
    match entry.readiness() {
        Readiness::Ready if entry.declarations().get() == 0 => Standing::Empty,
        Readiness::Ready => Standing::Readable,
        Readiness::Indexing { .. } => Standing::Indexing,
        Readiness::Requested => Standing::Requested,
        Readiness::Failed { .. } => Standing::Failed,
    }
}

/// Returns the sentence one shelf row prints under its name.
pub(crate) fn summary(entry: &ShelfEntry) -> String {
    let declarations = entry.declarations().get();
    match standing(entry) {
        Standing::Readable => format!(
            "{declarations} declarations · {} languages",
            entry.languages().len()
        ),
        Standing::Empty => "ready · nothing published under it".to_owned(),
        Standing::Indexing => match entry.readiness() {
            Readiness::Indexing { rows } => format!("indexing · {} so far", rows.get()),
            _ => "indexing".to_owned(),
        },
        Standing::Requested => "requested · waiting for the first rows".to_owned(),
        Standing::Failed => "failed".to_owned(),
    }
}

/// Returns whether a project is a folder on this host.
pub(crate) fn is_local(identity: &Identity) -> bool {
    !identity.coordinate().as_str().starts_with("pkg:")
}

/// Returns what this platform calls the machine the reader is sitting at.
///
/// "Choose a folder on this Mac…" is the right sentence on exactly one of the
/// three platforms this application builds for, and it is the first sentence a
/// Windows reader meets in the add form. The noun is read off the build target
/// rather than written into each string.
pub(crate) const fn this_machine() -> &'static str {
    if cfg!(target_os = "macos") {
        "this Mac"
    } else if cfg!(target_os = "windows") {
        "this PC"
    } else {
        "this computer"
    }
}

/// Returns an example absolute project path in this platform's spelling.
///
/// The add field's placeholder is the only statement of what a local project
/// looks like, and `/path/to/project` is not a shape a Windows reader can
/// copy.
pub(crate) const fn example_path() -> &'static str {
    if cfg!(target_os = "windows") {
        r"C:\path\to\project"
    } else {
        "/path/to/project"
    }
}

/// Returns the provenance badge: `local`, or the pinned package version.
pub(crate) fn badge(identity: &Identity) -> String {
    if is_local(identity) {
        return "local".to_owned();
    }
    identity
        .coordinate()
        .as_str()
        .rsplit_once('@')
        .map_or_else(|| "package".to_owned(), |(_, version)| version.to_owned())
}

/// Returns the entry for one exact coordinate.
pub(crate) fn find<'shelf>(shelf: &'shelf Shelf, coordinate: &str) -> Option<&'shelf ShelfEntry> {
    shelf
        .entries()
        .iter()
        .find(|entry| entry.identity().coordinate().as_str() == coordinate)
}

/// Adds a row for every accepted request the published shelf has not caught up with.
pub(crate) fn with_requested(shelf: &Shelf, coordinates: &[String]) -> Shelf {
    let mut entries = shelf.entries().to_vec();
    for coordinate in coordinates {
        let known = entries
            .iter()
            .any(|entry| entry.identity().coordinate().as_str() == coordinate);
        if known {
            continue;
        }
        entries.push(ShelfEntry::new(
            Identity::parse(coordinate),
            Readiness::Requested,
        ));
    }
    sorted(shelf, entries)
}

/// Replaces one project's readiness with the failure a job observed.
pub(crate) fn with_failure(
    shelf: &Shelf,
    coordinate: &str,
    fault: &backend_present::Fault,
) -> Shelf {
    let entries = shelf
        .entries()
        .iter()
        .map(|entry| {
            if entry.identity().coordinate().as_str() == coordinate {
                ShelfEntry::new(
                    entry.identity().clone(),
                    Readiness::Failed {
                        fault: fault.clone(),
                    },
                )
                .with_languages(entry.languages().to_vec())
            } else {
                entry.clone()
            }
        })
        .collect();
    sorted(shelf, entries)
}

fn sorted(shelf: &Shelf, mut entries: Vec<ShelfEntry>) -> Shelf {
    entries.sort_by(|left, right| left.identity().name().cmp(right.identity().name()));
    Shelf::new(shelf.revision(), entries)
}
