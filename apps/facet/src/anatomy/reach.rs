//! Relations back to us: what your own code does with the symbol on the page
//! (the v6 board's "Your code and it").
//!
//! Plain data like the rest of the plan (no gpui types, explicit
//! discriminants, hashed by content). The caller reads it from whatever knows
//! how your crates use the symbol (the pinned world's caller edges and the
//! statements mined from their files, the index's own use sites) and says
//! how it counted. [`Basis::Unknown`] is a first-class answer: the page says
//! that nothing was read, never that nothing uses it.

use super::plan::Effect;

/// How the counts were made: how sure the page is of them.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Basis {
    /// Nothing was read: the page says so.
    #[default]
    Unknown = 0,
    /// Caller declarations the compiler resolved to it.
    Resolved = 1,
    /// A path scan of the crates' files: real lines, approximate counts.
    Scanned = 2,
}

/// One line of a crate's code that names the symbol.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Line {
    /// The file, as the crate names it (`src/harness.rs`).
    pub file: String,
    /// The one-based line.
    pub line: u32,
    /// The statement's line, as written (cropped by the page).
    pub text: String,
    /// The name's byte range inside `text`, underlined.
    pub mark: Option<(u32, u32)>,
    /// The member of the symbol it reaches (`as_str`), when it names one.
    pub member: Option<String>,
    /// The declaration the line is in (`package_facts`), when known.
    pub caller: Option<String>,
    /// That declaration's address, for its door.
    pub link: Option<String>,
}

/// One crate's use of the symbol.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CrateUse {
    /// The crate (`desktop`).
    pub name: String,
    /// How many places name it.
    pub count: u32,
    /// The members it reaches, most first (`as_str` 8).
    pub members: Vec<(String, u32)>,
    /// The lines kept, most telling first.
    pub lines: Vec<Line>,
}

impl CrateUse {
    /// How many places were counted whose line was not kept.
    #[must_use]
    pub fn unkept(&self) -> u32 {
        self.count.saturating_sub(u32::try_from(self.lines.len()).unwrap_or(u32::MAX))
    }

    /// The count for `member`, when this crate reaches it.
    #[must_use]
    pub fn reaches(&self, member: &str) -> Option<u32> {
        self.members.iter().find(|(name, _)| name == member).map(|(_, n)| *n)
    }
}

/// A member of a type that your code reaches: what the reach bar's segment
/// says about it when scrubbed.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Reached {
    /// Its name.
    pub name: String,
    /// What it does to the value.
    pub effect: Effect,
    /// What it gives, in plain words (`maybe Table`), when it gives something.
    pub gives: Option<String>,
}

/// Where a sibling was found.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Scope {
    /// In the symbol's own module.
    #[default]
    Module = 0,
    /// In its package.
    Package = 1,
}

/// A crate that does not name the symbol but names its siblings.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Instead {
    /// The crate.
    pub name: String,
    /// The siblings it names (three at most).
    pub uses: Vec<String>,
    /// Where they are.
    pub scope: Scope,
}

/// One segment of the reach bar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    /// What it scrubs to: a member or a crate (`as_table`, `desktop`); empty
    /// for the symbol itself.
    pub id: String,
    /// What it says.
    pub label: String,
    /// How many places.
    pub count: u32,
    /// What it stands for.
    pub kind: SegmentKind,
}

/// What a reach bar segment stands for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum SegmentKind {
    /// A member your code reaches through the type.
    Member = 0,
    /// The type itself, named without reaching a member.
    Itself = 1,
    /// A crate.
    Crate = 2,
}

/// Your code and the symbol.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Reach {
    /// Your crates that name it, most first.
    pub yours: Vec<CrateUse>,
    /// Your crates that name its siblings but not it.
    pub instead: Vec<Instead>,
    /// Other packages that name it, most lines first.
    pub others: Vec<CrateUse>,
    /// The members of it your code reaches (types).
    pub reached: Vec<Reached>,
    /// How it was counted.
    pub basis: Basis,
    /// One quiet line on what is approximate.
    pub note: Option<String>,
}

impl Reach {
    /// Whether anybody read your code's use of it.
    #[must_use]
    pub fn read(&self) -> bool {
        self.basis != Basis::Unknown
    }

    /// Places your code names it.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.yours.iter().map(|used| used.count).sum()
    }

    /// Whether there is anything to say: a crate, or a crate that names its
    /// siblings, or the honest "nothing read".
    #[must_use]
    pub fn worth_a_section(&self) -> bool {
        self.read() || !self.yours.is_empty() || !self.others.is_empty()
    }

    /// The reach bar: for a type, the members your code reaches through it
    /// (then the type itself); otherwise the crates that name it.
    #[must_use]
    pub fn segments(&self, itself: &str) -> Vec<Segment> {
        let mut by: Vec<(String, u32)> = Vec::new();
        for used in &self.yours {
            for (member, n) in &used.members {
                match by.iter_mut().find(|(name, _)| name == member) {
                    Some((_, total)) => *total += n,
                    None => by.push((member.clone(), *n)),
                }
            }
        }
        if by.is_empty() {
            return self
                .yours
                .iter()
                .map(|used| Segment { id: used.name.clone(), label: used.name.clone(), count: used.count, kind: SegmentKind::Crate })
                .collect();
        }
        by.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let named: u32 = by.iter().map(|(_, n)| n).sum();
        let mut out: Vec<Segment> = by.into_iter().map(|(name, count)| Segment { id: name.clone(), label: name, count, kind: SegmentKind::Member }).collect();
        let bare = self.total().saturating_sub(named);
        if bare > 0 {
            out.push(Segment { id: String::new(), label: format!("{itself} itself"), count: bare, kind: SegmentKind::Itself });
        }
        out
    }

    /// What `member` is, when the reach bar can say.
    #[must_use]
    pub fn reached_member(&self, member: &str) -> Option<&Reached> {
        self.reached.iter().find(|reached| reached.name == member)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn used(name: &str, count: u32, members: &[(&str, u32)]) -> CrateUse {
        CrateUse { name: name.to_owned(), count, members: members.iter().map(|(m, n)| ((*m).to_owned(), *n)).collect(), lines: Vec::new() }
    }

    #[test]
    fn the_bar_is_the_members_reached_then_the_type_itself() {
        let reach = Reach {
            yours: vec![used("desktop", 43, &[("as_str", 8), ("as_table", 2)]), used("engine", 15, &[("as_str", 6), ("as_table", 3)])],
            basis: Basis::Scanned,
            ..Reach::default()
        };
        let segments = reach.segments("Value");
        let read: Vec<_> = segments.iter().map(|s| (s.label.as_str(), s.count)).collect();
        assert_eq!(read, vec![("as_str", 14), ("as_table", 5), ("Value itself", 39)]);
    }

    #[test]
    fn a_function_is_a_bar_of_crates() {
        let reach = Reach { yours: vec![used("engine", 5, &[]), used("desktop", 9, &[])], basis: Basis::Resolved, ..Reach::default() };
        let segments = reach.segments("from_str");
        assert!(segments.iter().all(|s| s.kind == SegmentKind::Crate));
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn nothing_read_is_not_none_used() {
        assert!(!Reach::default().read());
        assert!(!Reach::default().worth_a_section());
    }
}
