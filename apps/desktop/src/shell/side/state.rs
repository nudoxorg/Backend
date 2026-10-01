//! State on rows (`v6/cohesion/COHESION.md`, "The sidebar: primitives", 4):
//! glyphs at a row's right edge, so nothing needs opening to know what
//! matters.
//!
//! - mint: how often your code uses it;
//! - amber: it changes in the release being read;
//! - coral: it is gone or deprecated there (a deprecation the index
//!   recorded shows wherever it is);
//! - quiet ink: its member count, when nothing else applies.
//!
//! A module rolls its items' state up. A row whose state is not known shows
//! no glyph: an unread count is never drawn as zero.
//!
//! [`StateBook`] is the one place that answers "what does this item carry".
//! Today it is filled from facet's release data (`facet::data::release`:
//! toml and smallvec, with this workspace's uses of each) and from what the
//! index recorded on the declaration itself; a package outside the release
//! data has no uses and no changes to show, and says nothing.

use facet::data::release::{Crate, What};
use gpui::SharedString;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// One of your crates, by the name its folder gives it (`desktop`,
/// `local-service`): whose uses a package's items carry.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorkspaceCrate(SharedString);

impl WorkspaceCrate {
    /// A crate of yours by name.
    pub(crate) fn new(name: impl Into<SharedString>) -> Self {
        Self(name.into())
    }

    /// The name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The name, shared.
    pub(crate) fn shared(&self) -> SharedString {
        self.0.clone()
    }
}

impl fmt::Display for WorkspaceCrate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How often one of your crates uses one item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Usage {
    /// The crate.
    pub by: WorkspaceCrate,
    /// In how many places.
    pub uses: u32,
}

/// How far one of your crates reaches into a package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Reach {
    /// The crate.
    pub by: WorkspaceCrate,
    /// In how many places it uses the package.
    pub uses: u32,
    /// Of how many distinct items.
    pub items: usize,
}

/// Why an item is coral.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Gone {
    /// It is not in the release being read.
    Removed,
    /// It still is, and is deprecated.
    Deprecated,
}

impl Gone {
    /// The word at the row's edge.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Removed => "gone",
            Self::Deprecated => "deprecated",
        }
    }
}

/// What the release being read does to an item.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Change {
    /// Nothing, or nothing is being compared.
    #[default]
    Still,
    /// This many items change (one for an item; a module counts its own).
    Changes(u32),
    /// Gone or deprecated.
    Gone(Gone),
}

/// What one row carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RowState {
    /// How often your code uses it (`None`: not known).
    pub uses: Option<u32>,
    /// What the release being read does to it.
    pub change: Change,
    /// How many members it has (`None`: it has no members to count).
    pub members: Option<u32>,
}

/// One glyph at a row's edge, in the order they are drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Glyph {
    /// Coral words.
    Gone(Gone),
    /// Amber, with how many.
    Changed(u32),
    /// Mint, with how many uses.
    Used(u32),
    /// Quiet ink.
    Members(u32),
}

impl RowState {
    /// The glyphs to draw: what changes (coral words or an amber count),
    /// then your uses, and the member count only when neither is there.
    pub(crate) fn glyphs(&self) -> impl Iterator<Item = Glyph> {
        let change = match self.change {
            Change::Still => None,
            Change::Changes(count) => Some(Glyph::Changed(count)),
            Change::Gone(gone) => Some(Glyph::Gone(gone)),
        };
        let used = self.uses.filter(|uses| *uses > 0).map(Glyph::Used);
        let members = self
            .members
            .filter(|_| change.is_none() && used.is_none())
            .map(Glyph::Members);
        [change, used, members].into_iter().flatten()
    }

    /// Whether there is anything to draw.
    pub(crate) fn is_quiet(&self) -> bool {
        self.glyphs().next().is_none()
    }
}

/// A module's state, rolled up from its items.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Rollup {
    uses: u32,
    changed: u32,
}

impl Rollup {
    /// Adds one item's state.
    pub(crate) fn add(&mut self, state: &RowState) {
        self.uses = self.uses.saturating_add(state.uses.unwrap_or(0));
        self.changed = self.changed.saturating_add(match state.change {
            Change::Still => 0,
            Change::Changes(count) => count,
            Change::Gone(_) => 1,
        });
    }

    /// The rolled-up state of a module with `members` children.
    pub(crate) fn finish(self, members: usize) -> RowState {
        RowState {
            uses: (self.uses > 0).then_some(self.uses),
            change: if self.changed > 0 {
                Change::Changes(self.changed)
            } else {
                Change::Still
            },
            members: u32::try_from(members).ok().filter(|members| *members > 0),
        }
    }
}

/// What became of an item between the pin and the release being read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Move {
    /// Its signature (or shape) changed.
    Changed,
    /// Gone or deprecated.
    Gone(Gone),
}

/// One item's move, with the two signatures the release data recorded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Movement {
    /// What became of it.
    pub kind: Move,
    /// Its signature at the pin.
    pub before: Option<SharedString>,
    /// Its signature in the release being read.
    pub after: Option<SharedString>,
}

/// The two releases being compared, as the release data spells them, and
/// how many items moved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Compared {
    /// The pin.
    pub from: SharedString,
    /// The release being read.
    pub to: SharedString,
    /// How many items change (respelled ones do not count: they never
    /// alarm you).
    pub changed: usize,
}

/// The paths that name one item: an item re-exported under another path, or
/// moved between releases, is still one item, so what your code uses and what
/// a release changes are looked up under one spelling of it.
#[derive(Clone, Debug, Default)]
struct Spellings(HashMap<SharedString, SharedString>);

impl Spellings {
    /// The spellings the release data records for `releases`.
    fn of(krate: &Crate, releases: &[&str]) -> Self {
        let mut spellings = Self::default();
        for release in releases {
            for (alias, declared) in krate.aliases.get(*release).into_iter().flatten() {
                spellings.join(alias, declared);
            }
        }
        spellings
    }

    /// The one spelling every spelling of `path` is looked up under.
    fn root<'a>(&'a self, path: &'a str) -> &'a str {
        let mut at = path;
        while let Some(next) = self.0.get(at) {
            at = next.as_ref();
        }
        at
    }

    /// `a` and `b` name one item: the lesser spelling stands for both.
    fn join(&mut self, a: &str, b: &str) {
        let (a, b) = (self.root(a).to_owned(), self.root(b).to_owned());
        if a != b {
            let (low, high) = if a < b { (a, b) } else { (b, a) };
            self.0.insert(high.into(), low.into());
        }
    }
}

/// What each item of one package carries, by its path
/// (`toml::value::Value::as_str`).
#[derive(Clone, Debug, Default)]
pub(crate) struct StateBook {
    /// The crate's name as a path root (`toml`, `serde_json`).
    root: SharedString,
    /// Item path -> your crate -> how many places it uses the item.
    uses: HashMap<SharedString, BTreeMap<WorkspaceCrate, u32>>,
    /// Item path -> what the release being read does to it.
    moved: HashMap<SharedString, Movement>,
    /// Which paths are one item.
    spellings: Spellings,
    compared: Option<Compared>,
}

impl StateBook {
    /// Nothing is known.
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// The state the release data holds for the crate `root`, pinned at
    /// `from`. With `to` set (a release being read that is not the pin) it
    /// also holds what moved between the two.
    pub(crate) fn from_release(krate: &Crate, root: &str, from: &str, to: Option<&str>) -> Self {
        let read = to.unwrap_or(from);
        let mut book = Self {
            root: root.replace('-', "_").into(),
            spellings: Spellings::of(krate, &[from, read]),
            ..Self::default()
        };
        for site in &krate.uses {
            let path: SharedString = book.spellings.root(&site.path).to_owned().into();
            let Some(workspace_crate) = site.file.split('/').nth(1) else {
                continue;
            };
            let count = book
                .uses
                .entry(path)
                .or_default()
                .entry(WorkspaceCrate::new(workspace_crate.to_owned()))
                .or_insert(0);
            *count += 1;
        }
        if let Some(to) = to.filter(|to| *to != from) {
            for change in krate.changes(from, to) {
                if change.respelled() {
                    continue;
                }
                let moved = match change.what {
                    What::Removed => Move::Gone(Gone::Removed),
                    What::Deprecated => Move::Gone(Gone::Deprecated),
                    What::Added => continue,
                    What::Changed
                    | What::Renamed
                    | What::FieldAdded
                    | What::FieldRemoved
                    | What::VariantAdded
                    | What::VariantRemoved => Move::Changed,
                };
                let path: SharedString = book.spellings.root(&change.path).to_owned().into();
                book.moved.entry(path).or_insert(Movement {
                    kind: moved,
                    before: change.before.clone(),
                    after: change.after.clone(),
                });
            }
            book.compared = Some(Compared {
                from: from.to_owned().into(),
                to: to.to_owned().into(),
                changed: book.moved.len(),
            });
        }
        book
    }

    /// Whether the book knows nothing about any item.
    pub(crate) fn is_empty(&self) -> bool {
        self.uses.is_empty() && self.moved.is_empty()
    }

    /// The comparison the changes come from, when there is one.
    pub(crate) const fn compared(&self) -> Option<&Compared> {
        self.compared.as_ref()
    }

    /// The path of an item whose names from the crate root down are `names`.
    pub(crate) fn path_of<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> SharedString {
        let mut path = self.root.to_string();
        for name in names {
            path.push_str("::");
            path.push_str(name);
        }
        path.into()
    }

    /// What the book knows of the item at `path`: its uses and what the
    /// release being read does to it.
    pub(crate) fn state_of(&self, path: &str) -> RowState {
        let path = self.spellings.root(path);
        let uses = self
            .uses
            .get(path)
            .map(|by| by.values().sum::<u32>())
            .filter(|uses| *uses > 0);
        let change = match self.moved.get(path).map(|movement| movement.kind) {
            None => Change::Still,
            Some(Move::Changed) => Change::Changes(1),
            Some(Move::Gone(gone)) => Change::Gone(gone),
        };
        RowState {
            uses,
            change,
            members: None,
        }
    }

    /// What became of the item at `path` in the release being read.
    pub(crate) fn movement(&self, path: &str) -> Option<&Movement> {
        self.moved.get(self.spellings.root(path))
    }

    /// How many places `by` uses the item at `path`.
    pub(crate) fn uses_of(&self, path: &str, by: &WorkspaceCrate) -> u32 {
        self.uses
            .get(self.spellings.root(path))
            .and_then(|crates| crates.get(by))
            .copied()
            .unwrap_or(0)
    }

    /// Every crate of yours that uses the item at `path`, with how often,
    /// the busiest first.
    pub(crate) fn users_of(&self, path: &str) -> Vec<Usage> {
        let mut users: Vec<Usage> = self
            .uses
            .get(self.spellings.root(path))
            .into_iter()
            .flatten()
            .map(|(by, uses)| Usage {
                by: by.clone(),
                uses: *uses,
            })
            .collect();
        users.sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| a.by.cmp(&b.by)));
        users
    }

    /// Your crates that use this package, the busiest first.
    pub(crate) fn crates(&self) -> Vec<Reach> {
        let mut by: BTreeMap<&WorkspaceCrate, (u32, usize)> = BTreeMap::new();
        for crates in self.uses.values() {
            for (name, uses) in crates {
                let entry = by.entry(name).or_default();
                entry.0 += uses;
                entry.1 += 1;
            }
        }
        let mut list: Vec<Reach> = by
            .into_iter()
            .map(|(by, (uses, items))| Reach {
                by: by.clone(),
                uses,
                items,
            })
            .collect();
        list.sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| a.by.cmp(&b.by)));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet::data::release::{Change as Diff, ReleaseDiff, Severity, UseSite};

    fn site(path: &str, file: &str, line: u32) -> UseSite {
        UseSite {
            path: path.into(),
            file: file.into(),
            line,
            text: "x".into(),
        }
    }

    fn diff(path: &str, what: What, before: &str, after: &str) -> Diff {
        Diff {
            path: path.into(),
            what,
            severity: Severity::Breaking,
            before: Some(before.into()),
            after: Some(after.into()),
        }
    }

    /// toml 0.8.23 -> 1.1.6, with the uses this workspace makes of it.
    fn toml() -> Crate {
        Crate {
            name: "toml".into(),
            pinned: "0.8.23".into(),
            uses: vec![
                site("toml::value::Value", "apps/desktop/src/a.rs", 1),
                site("toml::value::Value", "apps/desktop/src/b.rs", 2),
                site("toml::value::Value", "crates/engine/src/c.rs", 3),
                site("toml::de::from_str", "apps/desktop/src/a.rs", 9),
            ],
            diffs: vec![ReleaseDiff {
                from: "0.8.23".into(),
                to: "1.1.6".into(),
                changes: vec![
                    diff(
                        "toml::de::from_str",
                        What::Changed,
                        "pub fn from_str<T>(s: &str) -> Result<T, Error>",
                        "pub fn from_str<T>(s: &str, extra: bool) -> Result<T, Error>",
                    ),
                    // Only the spelling of a lifetime moved: it never alarms you.
                    diff(
                        "toml::value::Value::as_str",
                        What::Changed,
                        "pub fn as_str(&self) -> Option<&str>",
                        "pub fn as_str<'a>(&'a self) -> Option<&'a str>",
                    ),
                    diff(
                        "toml::map::Entry",
                        What::Removed,
                        "pub enum Entry",
                        "pub enum Entry",
                    ),
                    diff(
                        "toml::ser::Serializer",
                        What::Deprecated,
                        "pub struct Serializer",
                        "pub struct Serializer",
                    ),
                    diff(
                        "toml::de::Fresh",
                        What::Added,
                        "pub struct Fresh",
                        "pub struct Fresh",
                    ),
                ],
                semver_slip: false,
            }],
            ..Crate::default()
        }
    }

    #[test]
    fn uses_are_counted_per_crate_of_yours_and_rolled_into_the_item() {
        let book = StateBook::from_release(&toml(), "toml", "0.8.23", None);
        assert_eq!(book.state_of("toml::value::Value").uses, Some(3));
        let by = |name: &str, uses: u32| Usage {
            by: WorkspaceCrate::new(name.to_owned()),
            uses,
        };
        assert_eq!(
            book.users_of("toml::value::Value"),
            [by("desktop", 2), by("engine", 1)],
            "the busiest crate first"
        );
        assert_eq!(
            book.uses_of("toml::value::Value", &WorkspaceCrate::new("engine")),
            1
        );
        assert_eq!(
            book.uses_of("toml::value::Value", &WorkspaceCrate::new("advisory")),
            0
        );
        assert_eq!(
            book.state_of("toml::value::Nothing"),
            RowState::default(),
            "an item nobody uses carries no state"
        );
        let reach = |name: &str, uses: u32, items: usize| Reach {
            by: WorkspaceCrate::new(name.to_owned()),
            uses,
            items,
        };
        assert_eq!(
            book.crates(),
            [reach("desktop", 3, 2), reach("engine", 1, 1)]
        );
        assert_eq!(book.compared(), None, "at the pin nothing is compared");
    }

    #[test]
    fn reading_another_release_marks_what_changes_and_what_is_gone_and_leaves_a_respelling_alone() {
        let book = StateBook::from_release(&toml(), "toml", "0.8.23", Some("1.1.6"));
        assert_eq!(
            book.state_of("toml::de::from_str").change,
            Change::Changes(1),
            "amber"
        );
        assert_eq!(
            book.state_of("toml::map::Entry").change,
            Change::Gone(Gone::Removed),
            "coral"
        );
        assert_eq!(
            book.state_of("toml::ser::Serializer").change,
            Change::Gone(Gone::Deprecated)
        );
        assert_eq!(
            book.state_of("toml::value::Value::as_str").change,
            Change::Still,
            "a lifetime spelled out is not a change"
        );
        assert_eq!(
            book.state_of("toml::de::Fresh").change,
            Change::Still,
            "what is new is not in the pin's outline"
        );
        assert_eq!(
            book.compared(),
            Some(&Compared {
                from: "0.8.23".into(),
                to: "1.1.6".into(),
                changed: 3
            })
        );
    }

    #[test]
    fn the_glyphs_read_left_to_right_as_change_then_uses_and_members_only_when_nothing_else_speaks()
    {
        let both = RowState {
            uses: Some(20),
            change: Change::Changes(4),
            members: Some(9),
        };
        assert_eq!(
            both.glyphs().collect::<Vec<_>>(),
            [Glyph::Changed(4), Glyph::Used(20)]
        );
        let gone = RowState {
            uses: Some(2),
            change: Change::Gone(Gone::Removed),
            members: None,
        };
        assert_eq!(
            gone.glyphs().collect::<Vec<_>>(),
            [Glyph::Gone(Gone::Removed), Glyph::Used(2)]
        );
        let quiet = RowState {
            uses: None,
            change: Change::Still,
            members: Some(34),
        };
        assert_eq!(quiet.glyphs().collect::<Vec<_>>(), [Glyph::Members(34)]);
        assert!(
            RowState::default().is_quiet(),
            "an unread count is not drawn as zero"
        );
        assert!(
            RowState {
                uses: Some(0),
                ..RowState::default()
            }
            .is_quiet(),
            "and zero uses says nothing either"
        );
    }

    #[test]
    fn an_item_moved_between_releases_is_one_item_under_either_of_its_paths() {
        use std::collections::HashMap as Map;
        let mut krate = toml();
        krate.uses.push(site(
            "toml::de::Deserializer::new",
            "apps/desktop/src/d.rs",
            7,
        ));
        krate.diffs[0].changes.push(diff(
            "toml::Deserializer::new",
            What::Deprecated,
            "pub fn new()",
            "pub fn new()",
        ));
        krate.aliases = Map::from([
            (
                "0.8.23".into(),
                Map::from([(
                    "toml::Deserializer::new".into(),
                    "toml::de::Deserializer::new".into(),
                )]),
            ),
            (
                "1.1.6".into(),
                Map::from([(
                    "toml::de::Deserializer::new".into(),
                    "toml::Deserializer::new".into(),
                )]),
            ),
        ]);
        let book = StateBook::from_release(&krate, "toml", "0.8.23", Some("1.1.6"));
        for spelling in ["toml::de::Deserializer::new", "toml::Deserializer::new"] {
            let state = book.state_of(spelling);
            assert_eq!(state.uses, Some(1), "your use is found under {spelling}");
            assert_eq!(
                state.change,
                Change::Gone(Gone::Deprecated),
                "and so is the release's word about it: {spelling}"
            );
        }
        assert_eq!(book.users_of("toml::Deserializer::new").len(), 1);
    }

    #[test]
    fn a_module_rolls_its_items_up() {
        let mut rollup = Rollup::default();
        rollup.add(&RowState {
            uses: Some(20),
            change: Change::Changes(1),
            members: None,
        });
        rollup.add(&RowState {
            uses: Some(5),
            change: Change::Gone(Gone::Removed),
            members: None,
        });
        rollup.add(&RowState::default());
        assert_eq!(
            rollup.finish(3),
            RowState {
                uses: Some(25),
                change: Change::Changes(2),
                members: Some(3)
            }
        );
        assert_eq!(
            Rollup::default().finish(0),
            RowState::default(),
            "an empty module is quiet"
        );
    }

    #[test]
    fn an_item_path_starts_at_the_crate_root_with_dashes_as_underscores() {
        let book = StateBook::from_release(&Crate::default(), "serde-json", "1.0.0", None);
        assert_eq!(
            book.path_of(["value", "Value", "as_str"]).as_ref(),
            "serde_json::value::Value::as_str"
        );
    }
}
