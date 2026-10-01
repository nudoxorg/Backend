//! What a sidebar list is made of: rows. A row says what it shows (a mark,
//! a name, quiet words, the state glyphs at its right edge) and what it
//! does when chosen ([`Do`]: a closed enum, not a closure, so a test can
//! assert on it and the view has one place that interprets it).

use super::lens::Lens;
use super::scope::Scope;
use super::state::{RowState, WorkspaceCrate};
use crate::core::LocalProjectId;
use crate::model::pages::{PackageRef, PageKey, SymbolRef};
use crate::navigation::{ReleaseId, Route, SettingsPage};
use facet::icons::{Icon, Kind};
use gpui::SharedString;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

/// The trailing fold that holds a package's test-only modules.
pub(crate) const TESTS_ROW: &str = "shelf-tests";

/// The families the package intro sorts the API into: what the page's own
/// module regions do not say.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum KindGroup {
    /// Structs, enums, classes, unions, type aliases.
    Types,
    /// Traits and interfaces.
    Contracts,
    /// Functions, methods, constructors.
    Functions,
    /// Macros.
    Macros,
    /// Constants, variables, fields, properties, variants.
    Values,
}

impl KindGroup {
    /// Every group, in the order the list shows them.
    pub(crate) const ALL: [Self; 5] = [
        Self::Types,
        Self::Contracts,
        Self::Functions,
        Self::Macros,
        Self::Values,
    ];

    /// The group's words.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Types => "Types",
            Self::Contracts => "Contracts",
            Self::Functions => "Functions",
            Self::Macros => "Macros",
            Self::Values => "Values",
        }
    }

    /// The mark the group's row wears.
    pub(crate) const fn mark(self) -> Kind {
        match self {
            Self::Types => Kind::Struct,
            Self::Contracts => Kind::Trait,
            Self::Functions => Kind::Function,
            Self::Macros => Kind::Macro,
            Self::Values => Kind::Constant,
        }
    }

    /// The group a declaration kind belongs to; modules, imports and the
    /// unknown are in none.
    pub(crate) const fn of(kind: Option<backend_library::DeclarationKind>) -> Option<Self> {
        use backend_library::DeclarationKind as K;
        match kind {
            Some(K::Struct | K::Class | K::Enum | K::Union | K::Type) => Some(Self::Types),
            Some(K::Trait | K::Interface) => Some(Self::Contracts),
            Some(K::Function | K::Method | K::Constructor) => Some(Self::Functions),
            Some(K::Macro) => Some(Self::Macros),
            Some(K::Constant | K::Variable | K::Field | K::Property | K::Variant) => {
                Some(Self::Values)
            }
            Some(K::Module | K::Import | K::Unknown) | None => None,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Types => "types",
            Self::Contracts => "contracts",
            Self::Functions => "functions",
            Self::Macros => "macros",
            Self::Values => "values",
        }
    }
}

/// Which list of the package intro an item is listed in: the same item can
/// be in two, and each listing is its own row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Section {
    /// Items your code uses.
    Yours,
    /// Items that change in the release being read.
    Changes,
    /// Items of one kind family.
    Kind(KindGroup),
}

impl Section {
    const fn key(self) -> &'static str {
        match self {
            Self::Yours => "yours",
            Self::Changes => "changes",
            Self::Kind(group) => group.key(),
        }
    }
}

/// Which row it is. Also the keyboard's id for it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RowId {
    /// A node of the outline, in the outline.
    Node(SymbolRef),
    /// An item listed in a section of the package intro.
    Listed(Section, SymbolRef),
    /// A kind family's group on the package intro.
    Group(KindGroup),
    /// The fold that holds test-only modules.
    Tests,
    /// One of your projects (by the spelling of its folder).
    Project(Arc<str>),
    /// A package in the library.
    Package(PackageRef),
    /// One settings page.
    Setting(SettingsPage),
    /// A release.
    Release(Arc<str>),
    /// One dependency, by name.
    Dependency(Arc<str>),
    /// One dependent package.
    Dependent(PackageRef),
    /// One of your crates.
    Crate(WorkspaceCrate),
    /// "*query* in the whole library".
    Widen,
}

impl RowId {
    /// The id as the keyboard, the probe and a click's motion key spell it.
    /// Outline rows are spelled as their declaration's coordinate.
    pub(crate) fn key(&self) -> SharedString {
        match self {
            Self::Node(symbol) => symbol.as_str().to_owned().into(),
            Self::Listed(section, symbol) => {
                format!("shelf-{}:{}", section.key(), symbol.as_str()).into()
            }
            Self::Group(group) => format!("shelf-kind-{}", group.key()).into(),
            Self::Tests => TESTS_ROW.into(),
            Self::Project(path) => format!("project-{path}").into(),
            Self::Package(package) => format!("package-{package}").into(),
            Self::Setting(page) => format!("settings-{}", page.as_str()).into(),
            Self::Release(version) => format!("shelf-release-{version}").into(),
            Self::Dependency(name) => format!("shelf-dep-{name}").into(),
            Self::Dependent(package) => format!("shelf-dependent-{package}").into(),
            Self::Crate(name) => format!("shelf-crate-{name}").into(),
            Self::Widen => "shelf-widen".into(),
        }
    }
}

/// What a row does when it is chosen (↵, a click, a hint).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Do {
    /// Nothing: an honest line, or a place with nowhere to go yet.
    Nothing,
    /// Moves the reader.
    Go(Route),
    /// Opens or closes a group in place.
    Fold(RowId),
    /// Changes the lens (the reader does not move).
    Lens(Lens),
    /// Reads the book at a release (`None`: the one you pin).
    Release(Option<ReleaseId>),
    /// Makes a project the active one.
    Project(LocalProjectId),
    /// Opens a settings page.
    Settings(SettingsPage),
    /// Narrows Contents to what one of your crates uses.
    Via(WorkspaceCrate),
    /// Widens the narrowing query to the whole library (Find).
    Widen(crate::model::pages::SearchQuery),
}

/// Whether a group is open. A two-variant enum, not a bool, so a call site
/// says which it means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fold {
    /// Its children are listed beneath it.
    Open,
    /// Its children are not listed.
    Shut,
}

impl Fold {
    /// Open when `open`.
    pub(crate) const fn of(open: bool) -> Self {
        if open { Self::Open } else { Self::Shut }
    }
}

/// A release's mark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReleaseMark {
    /// The one you pin: mint.
    Pin,
    /// The one being read: periwinkle.
    Reading,
    /// Any other: quiet.
    Other,
}

/// The mark at a row's left.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mark {
    /// A declaration's kind.
    Kind(Kind),
    /// A UI icon.
    Icon(Icon),
    /// A release.
    Release(ReleaseMark),
}

/// What sits at a row's right edge.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Trailing {
    /// Nothing.
    Nothing,
    /// Quiet words: a requirement, a version, "your pin".
    Words(SharedString),
    /// The state glyphs: uses in mint, changes in amber, gone in coral, the
    /// member count in quiet ink.
    State(RowState),
}

/// One row you can stand on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// Which row.
    pub id: RowId,
    /// [`RowId::key`], spelled once.
    pub key: SharedString,
    /// How far in the list it is nested.
    pub depth: u8,
    /// Its mark.
    pub mark: Mark,
    /// Its name.
    pub name: SharedString,
    /// Which characters of the name the narrowing query matched.
    pub hit: Option<Range<usize>>,
    /// Quiet words after the name: the module of an item listed away from
    /// its module.
    pub sub: Option<SharedString>,
    /// The row for the page the reader is on.
    pub current: bool,
    /// `Some` for a group: whether it is open.
    pub fold: Option<Fold>,
    /// What choosing it does.
    pub does: Do,
    /// The scope `→` (or a double-click) hoists the sidebar into.
    pub hoists: Option<Scope>,
    /// The page hovering it warms.
    pub warm: Option<PageKey>,
    /// The declaration S peels to source and a click hands its name to.
    pub source: Option<SymbolRef>,
    /// The item's path in the release data (`toml::value::Value`), when
    /// there is release data for its package: what the peek asks the state
    /// book about.
    pub path: Option<SharedString>,
    /// What sits at its right edge.
    pub trailing: Trailing,
    /// Drawn quieter: not indexed, or an honest "nothing here".
    pub dim: bool,
}

impl Item {
    /// A row with nothing but a mark and a name, doing `does`.
    pub(crate) fn new(
        id: RowId,
        depth: u8,
        mark: Mark,
        name: impl Into<SharedString>,
        does: Do,
    ) -> Self {
        Self {
            key: id.key(),
            id,
            depth,
            mark,
            name: name.into(),
            hit: None,
            sub: None,
            current: false,
            fold: None,
            does,
            hoists: None,
            warm: None,
            source: None,
            path: None,
            trailing: Trailing::Nothing,
            dim: false,
        }
    }

    /// Whether the keyboard can stand on it.
    pub(crate) fn is_target(&self) -> bool {
        self.does != Do::Nothing
    }
}

/// A heading over a run of rows: "YOURS 7".
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Heading {
    /// Its words.
    pub words: SharedString,
    /// How many rows it heads.
    pub count: Option<usize>,
}

/// One line of the list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// A row you can stand on.
    Item(Item),
    /// A heading (not a target).
    Heading(Heading),
    /// One quiet, honest line: a gap, or "nothing here".
    Note(SharedString),
}

impl Row {
    /// A heading with a count.
    pub(crate) fn heading(words: impl Into<SharedString>, count: usize) -> Self {
        Self::Heading(Heading {
            words: words.into(),
            count: Some(count),
        })
    }

    /// The item, when this line is one.
    pub(crate) const fn item(&self) -> Option<&Item> {
        match self {
            Self::Item(item) => Some(item),
            Self::Heading(_) | Self::Note(_) => None,
        }
    }
}

/// The groups the person opened or closed by hand: a group is open when
/// that differs from whether it is open on its own (it holds the page you
/// are on, or a match).
#[derive(Clone, Debug, Default)]
pub(crate) struct Folds(HashSet<RowId>);

impl Folds {
    /// Whether the group `id` is open, given whether it is by default.
    pub(crate) fn is_open(&self, id: &RowId, by_default: bool) -> bool {
        by_default != self.0.contains(id)
    }

    /// Flips a group.
    pub(crate) fn flip(&mut self, id: RowId) {
        if !self.0.remove(&id) {
            self.0.insert(id);
        }
    }

    /// Forgets every flip (a new place starts with only its own open).
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_kind_belongs_to_one_family_or_none() {
        use backend_library::DeclarationKind as K;
        assert_eq!(KindGroup::of(Some(K::Struct)), Some(KindGroup::Types));
        assert_eq!(
            KindGroup::of(Some(K::Interface)),
            Some(KindGroup::Contracts)
        );
        assert_eq!(KindGroup::of(Some(K::Method)), Some(KindGroup::Functions));
        assert_eq!(KindGroup::of(Some(K::Macro)), Some(KindGroup::Macros));
        assert_eq!(KindGroup::of(Some(K::Constant)), Some(KindGroup::Values));
        assert_eq!(
            KindGroup::of(Some(K::Module)),
            None,
            "modules are the page's own regions, not a family"
        );
        assert_eq!(KindGroup::of(None), None);
    }

    #[test]
    fn every_row_id_spells_a_distinct_key_and_an_outline_row_is_its_coordinate() {
        let symbol = SymbolRef::new("pkg:cargo/x@1::a.rs:3::Item").expect("symbol");
        let ids = [
            RowId::Node(symbol.clone()),
            RowId::Listed(Section::Yours, symbol.clone()),
            RowId::Listed(Section::Kind(KindGroup::Types), symbol.clone()),
            RowId::Group(KindGroup::Types),
            RowId::Tests,
            RowId::Release("1.0.0".into()),
            RowId::Dependency("x".into()),
            RowId::Crate(WorkspaceCrate::new("x")),
            RowId::Widen,
        ];
        let keys: HashSet<_> = ids.iter().map(RowId::key).collect();
        assert_eq!(keys.len(), ids.len(), "one key per row: {keys:?}");
        assert_eq!(
            RowId::Node(symbol.clone()).key().as_ref(),
            symbol.as_str(),
            "a probe reads the address of an outline row from its key"
        );
        assert_eq!(RowId::Tests.key().as_ref(), TESTS_ROW);
    }
}
