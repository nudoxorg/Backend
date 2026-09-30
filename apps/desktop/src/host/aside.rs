//! Where an index an earlier build wrote was set aside.
//!
//! The owner's thread learns it while it starts (`DesktopHost::state_set_aside`),
//! long before any window state can hold it. It is recorded here once, and the
//! window takes it when the owner answers, to say so and to index the shelf's
//! projects again. Taking it empties it: a later restart of the owner within
//! the same run does not say it a second time.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

static ASIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Notes that the owner set an earlier build's index aside, at `moved_to`.
pub(crate) fn record(moved_to: &Path) {
    *ASIDE.lock().unwrap_or_else(PoisonError::into_inner) = Some(moved_to.to_path_buf());
}

/// Takes what was recorded, once.
pub(crate) fn take() -> Option<PathBuf> {
    ASIDE.lock().unwrap_or_else(PoisonError::into_inner).take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_aside_index_is_told_once() {
        assert_eq!(take(), None, "nothing was set aside");
        record(Path::new("/data/from-another-build"));
        assert_eq!(take(), Some(PathBuf::from("/data/from-another-build")));
        assert_eq!(take(), None, "and it is not told a second time");
    }
}
