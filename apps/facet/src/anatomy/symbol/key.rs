//! What identifies a part of the page: which part, which one of them, which
//! of its texts. The probe ledger, the keyboard walk and the folds all key
//! on these, so nothing is named by a bare string.

use gpui::{ElementId, SharedString};

/// A section of the page.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Sec {
    /// The header.
    Head,
    /// The call.
    Call,
    /// The docs.
    Docs,
    /// If it fails.
    Fails,
    /// What it is.
    Shape,
    /// What you can do with it.
    Verbs,
    /// In your workspace.
    Uses,
    /// The context rail.
    Rail,
}

impl Sec {
    fn word(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Call => "call",
            Self::Docs => "docs",
            Self::Fails => "fails",
            Self::Shape => "shape",
            Self::Verbs => "verbs",
            Self::Uses => "uses",
            Self::Rail => "rail",
        }
    }
}

/// A kind of part.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Part {
    /// A whole section.
    Sec(Sec),
    /// The header's kind word.
    Kind,
    /// A path segment.
    Path,
    /// The language tag.
    Lang,
    /// The lede.
    Lede,
    /// The receiver row.
    Recv,
    /// An input port.
    Port,
    /// An option of an options object.
    Opt,
    /// The rail's `later` row.
    Later,
    /// The rail's `gives` row.
    Gives,
    /// The rail's `or nothing` row.
    Nothing,
    /// The rail's `or fails` row.
    Fail,
    /// A docs block.
    Doc,
    /// An example.
    Example,
    /// A kind of error.
    ErrKind,
    /// An enum case.
    Case,
    /// A struct field.
    Field,
    /// A method a trait's implementor writes.
    Owed,
    /// A method group.
    Group,
    /// A method row.
    Method,
    /// A section head.
    Head,
    /// A verb chip.
    Chip,
    /// The package picker.
    Picker,
    /// A picker menu row.
    Menu,
    /// A package group in the list.
    Package,
    /// A place row.
    Place,
    /// A rail block.
    Block,
    /// A rail package row.
    RailPackage,
    /// A rail sibling row.
    Sibling,
    /// A capability chip.
    Cap,
    /// A hover card.
    Card,
    /// A fold's line.
    Fold,
}

impl Part {
    fn word(self) -> &'static str {
        match self {
            Self::Sec(sec) => sec.word(),
            Self::Kind => "kind",
            Self::Path => "path",
            Self::Lang => "lang",
            Self::Lede => "lede",
            Self::Recv => "recv",
            Self::Port => "port",
            Self::Opt => "opt",
            Self::Later => "later",
            Self::Gives => "gives",
            Self::Nothing => "none",
            Self::Fail => "fail",
            Self::Doc => "doc",
            Self::Example => "example",
            Self::ErrKind => "errkind",
            Self::Case => "case",
            Self::Field => "field",
            Self::Owed => "owed",
            Self::Group => "group",
            Self::Method => "method",
            Self::Head => "head",
            Self::Chip => "chip",
            Self::Picker => "picker",
            Self::Menu => "menu",
            Self::Package => "pkg",
            Self::Place => "place",
            Self::Block => "block",
            Self::RailPackage => "rpkg",
            Self::Sibling => "sib",
            Self::Cap => "cap",
            Self::Card => "card",
            Self::Fold => "fold",
        }
    }
}

/// One identified part: `s6-port-2-name`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Key(String);

impl Key {
    /// The part itself.
    #[must_use]
    pub fn of(part: Part) -> Self {
        Self(format!("s6-{}", part.word()))
    }

    /// The `n`th of them.
    #[must_use]
    pub fn at(&self, n: usize) -> Self {
        Self(format!("{}-{n}", self.0))
    }

    /// One of its texts or elements.
    #[must_use]
    pub fn field(&self, name: &'static str) -> Self {
        Self(format!("{}-{name}", self.0))
    }

    /// One of them by name (a generic `T`, a package).
    #[must_use]
    pub fn named(&self, name: &str) -> Self {
        Self(format!("{}-{name}", self.0))
    }

    /// The same part under a second index (a row of a group).
    #[must_use]
    pub fn then(&self, part: Part, n: usize) -> Self {
        Self(format!("{}-{}-{n}", self.0, part.word()))
    }

    /// As an element id.
    #[must_use]
    pub fn id(&self) -> ElementId {
        ElementId::Name(SharedString::from(self.0.clone()))
    }

    /// As shared text.
    #[must_use]
    pub fn text(&self) -> SharedString {
        SharedString::from(self.0.clone())
    }
}

impl From<&Key> for ElementId {
    fn from(key: &Key) -> Self {
        key.id()
    }
}

impl From<Key> for ElementId {
    fn from(key: Key) -> Self {
        key.id()
    }
}

impl From<Key> for SharedString {
    fn from(key: Key) -> Self {
        key.text()
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A fold (a disclosure that unrolls in place).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FoldKey {
    /// An options object.
    Options(String),
    /// An example, by its place in the docs.
    Example(usize),
    /// The docs past the second block.
    MoreDocs,
    /// A method group's rows past the sixth.
    MoreMethods(super::view::Do),
    /// A case's or field's extra line.
    More(String),
}
