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

/// Returns whether a project is a folder on this host.
pub(crate) fn is_local(identity: &Identity) -> bool {
    !identity.coordinate().as_str().starts_with("pkg:")
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
    fault: backend_present::Fault,
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
