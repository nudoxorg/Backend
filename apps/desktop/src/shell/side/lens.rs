//! The four lenses on a scope (`v6/cohesion/COHESION.md`, "The sidebar:
//! primitives", 2): Contents · Versions · Rests on · Used by. One tab strip;
//! each lens is a list with the same row grammar, and choosing one changes
//! the list, never the page. The chord to a lens is `G` then its letter.

/// One lens on the scope the sidebar shows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum Lens {
    /// The outline (or, where the reader lists the modules itself, the API
    /// by what matters to you).
    #[default]
    Contents,
    /// Releases, newest first: the pin in mint, the one being read in
    /// periwinkle.
    Versions,
    /// Dependencies.
    RestsOn,
    /// Dependents: your crates first.
    UsedBy,
}

impl Lens {
    /// Every lens, in strip order.
    pub(crate) const ALL: [Self; 4] = [Self::Contents, Self::Versions, Self::RestsOn, Self::UsedBy];

    /// The tab's words.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Contents => "Contents",
            Self::Versions => "Versions",
            Self::RestsOn => "Rests on",
            Self::UsedBy => "Used by",
        }
    }

    /// The tab's element id, and what its rows' ids carry.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Contents => "contents",
            Self::Versions => "versions",
            Self::RestsOn => "rests-on",
            Self::UsedBy => "used-by",
        }
    }

    /// The letter that follows `G` (Go to): `G C`, `G V`, `G R`, `G U`.
    pub(crate) const fn chord(self) -> char {
        match self {
            Self::Contents => 'c',
            Self::Versions => 'v',
            Self::RestsOn => 'r',
            Self::UsedBy => 'u',
        }
    }

    /// The lens a chord's second letter names.
    pub(crate) fn from_chord(letter: char) -> Option<Self> {
        let letter = letter.to_ascii_lowercase();
        Self::ALL.into_iter().find(|lens| lens.chord() == letter)
    }
}

/// How much each lens holds (`None`: not known, or the lens means nothing at
/// this scope). The strip shows the count of the lens you are on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    /// Names in the outline.
    pub contents: Option<usize>,
    /// Releases.
    pub versions: Option<usize>,
    /// Dependencies.
    pub rests_on: Option<usize>,
    /// Your crates and the dependents.
    pub used_by: Option<usize>,
}

impl Counts {
    /// What `lens` holds.
    pub(crate) const fn of(&self, lens: Lens) -> Option<usize> {
        match lens {
            Lens::Contents => self.contents,
            Lens::Versions => self.versions,
            Lens::RestsOn => self.rests_on,
            Lens::UsedBy => self.used_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lens_has_its_own_chord_letter_and_element_id() {
        let letters: Vec<char> = Lens::ALL.iter().map(|lens| lens.chord()).collect();
        assert_eq!(
            letters,
            ['c', 'v', 'r', 'u'],
            "G C / G V / G R / G U, in strip order"
        );
        for lens in Lens::ALL {
            assert_eq!(Lens::from_chord(lens.chord()), Some(lens));
            assert_eq!(
                Lens::from_chord(lens.chord().to_ascii_uppercase()),
                Some(lens),
                "shifted letters chord too"
            );
        }
        assert_eq!(Lens::from_chord('x'), None);
        let keys: std::collections::HashSet<_> = Lens::ALL.iter().map(|lens| lens.key()).collect();
        assert_eq!(keys.len(), Lens::ALL.len());
    }

    #[test]
    fn counts_are_read_by_lens() {
        let counts = Counts {
            contents: Some(29),
            versions: Some(128),
            rests_on: None,
            used_by: Some(4),
        };
        assert_eq!(
            Lens::ALL.map(|lens| counts.of(lens)),
            [Some(29), Some(128), None, Some(4)]
        );
    }
}
