//! The symbol's history: what it was at each release the index has read.
//!
//! Plain data. A release is a bar on the instrument (tall when it broke
//! callers, short for a patch, coral when it was yanked); the releases the
//! index has read wear a cap that says what the symbol was there, against the
//! release you pin: the same, different (and how), not here yet, gone.
//! Releases nobody read have no cap: the page never guesses what it was.

/// What the symbol was at a release, against the pinned one.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum Was {
    /// Nobody read that release: nothing is said.
    #[default]
    Unread,
    /// The release you pin.
    Pinned,
    /// The same as at the pin.
    Same,
    /// Different, and how (`only its lifetimes differ`, `changed`).
    Differs(String),
    /// Not here yet: the release is older than the symbol.
    NotYet,
    /// Gone by that release.
    Gone,
}

/// How much a release moved the crate's API.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Weight {
    /// A patch.
    #[default]
    Patch = 0,
    /// A minor.
    Minor = 1,
    /// A major (or a breaking minor before 1.0).
    Major = 2,
}

/// One release of the symbol's package.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Release {
    /// The version as people read it (`0.8.23`).
    pub version: String,
    /// When it was published (ISO 8601, `2025-01-13`), or empty when the
    /// registry did not say.
    pub at: String,
    /// Yanked from the registry.
    pub yanked: bool,
    /// How much it moved the API.
    pub weight: Weight,
    /// What the symbol was there.
    pub was: Was,
}

/// The symbol across the releases of its package, oldest first.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct History {
    /// Every release with a date.
    pub releases: Vec<Release>,
}

impl History {
    /// Whether there is a history to draw.
    #[must_use]
    pub fn drawn(&self) -> bool {
        self.releases
            .iter()
            .filter(|release| !release.at.is_empty())
            .count()
            >= 2
    }

    /// The releases the index has read.
    #[must_use]
    pub fn read(&self) -> impl Iterator<Item = &Release> {
        self.releases
            .iter()
            .filter(|release| release.was != Was::Unread)
    }

    /// The caption: what the read releases say together.
    #[must_use]
    pub fn caption(&self) -> String {
        let read: Vec<&Release> = self.read().collect();
        let differs = read
            .iter()
            .filter(|release| matches!(release.was, Was::Differs(_)))
            .count();
        if read.len() <= 1 {
            "only your pin is on disk: other releases not read".to_owned()
        } else if differs > 0 {
            format!("different at {differs} of {} releases read", read.len())
        } else if let Some(release) = read.iter().rev().find(|release| release.was == Was::NotYet) {
            format!("arrived after {}", release.version)
        } else if read.iter().any(|release| release.was == Was::Gone) {
            "gone from later releases".to_owned()
        } else {
            format!("the same at all {} releases read", read.len())
        }
    }

    /// The words a scrubbed release says: `0.5.11 · 2022-01-12 · only its
    /// lifetimes differ`.
    #[must_use]
    pub fn label(&self, index: usize) -> String {
        let Some(release) = self.releases.get(index) else {
            return String::new();
        };
        let mut parts = vec![release.version.clone()];
        if !release.at.is_empty() {
            parts.push(release.at.clone());
        }
        match &release.was {
            Was::Unread => parts.push("not read".to_owned()),
            Was::Pinned => parts.push("your pin".to_owned()),
            Was::Same => parts.push("the same".to_owned()),
            Was::Differs(how) => parts.push(how.clone()),
            Was::NotYet => parts.push("not here yet".to_owned()),
            Was::Gone => parts.push("gone".to_owned()),
        }
        if release.yanked {
            parts.push("yanked".to_owned());
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, at: &str, was: Was) -> Release {
        Release {
            version: version.to_owned(),
            at: at.to_owned(),
            was,
            ..Release::default()
        }
    }

    #[test]
    fn a_history_of_reads_says_how_the_symbol_moved() {
        let mut history = History {
            releases: vec![
                release("0.5.11", "2022-01-12", Was::Same),
                release(
                    "0.6.0",
                    "2023-01-01",
                    Was::Differs("its signature changed".to_owned()),
                ),
                release("0.8.23", "2025-01-13", Was::Pinned),
            ],
        };
        assert_eq!(history.caption(), "different at 1 of 3 releases read");
        assert_eq!(
            history.label(1),
            "0.6.0 · 2023-01-01 · its signature changed"
        );
        history.releases[1].was = Was::Same;
        assert_eq!(history.caption(), "the same at all 3 releases read");
    }

    #[test]
    fn a_history_nobody_read_says_so_and_is_not_drawn_without_dates() {
        let history = History {
            releases: vec![
                release("0.1.0", "2020-01-01", Was::Unread),
                release("0.2.0", "2021-01-01", Was::Pinned),
            ],
        };
        assert_eq!(
            history.caption(),
            "only your pin is on disk: other releases not read"
        );
        assert!(history.drawn());
        assert!(!History::default().drawn());
    }
}
