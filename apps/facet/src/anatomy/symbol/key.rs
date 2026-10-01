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
    /// The package picker's chip: where its menu opens.
    Picker,
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
            Self::Picker => "picker-spot",
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

/// What one text or element of a part is: `s6-port-2-name` is a port's `Name`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Slot {
    #[doc = "`across`"]
    Across,
    #[doc = "`all`"]
    All,
    #[doc = "`all-words`"]
    AllWords,
    #[doc = "`arrow`"]
    Arrow,
    #[doc = "`aside`"]
    Aside,
    #[doc = "`aside-loop`"]
    AsideLoop,
    #[doc = "`bound`"]
    Bound,
    #[doc = "`card`"]
    Card,
    #[doc = "`caret`"]
    Caret,
    #[doc = "`chooses`"]
    Chooses,
    #[doc = "`click`"]
    Click,
    #[doc = "`code`"]
    Code,
    #[doc = "`count`"]
    Count,
    #[doc = "`default`"]
    Default,
    #[doc = "`differs`"]
    Differs,
    #[doc = "`doc`"]
    Doc,
    #[doc = "`door`"]
    Door,
    #[doc = "`elsewhere`"]
    Elsewhere,
    #[doc = "`elsewhere-head`"]
    ElsewhereHead,
    #[doc = "`empty`"]
    Empty,
    #[doc = "`err`"]
    Err,
    #[doc = "`expand`"]
    Expand,
    #[doc = "`fill`"]
    Fill,
    #[doc = "`fill-clear`"]
    FillClear,
    #[doc = "`fill-clear-words`"]
    FillClearWords,
    #[doc = "`fill-words`"]
    FillWords,
    #[doc = "`flow`"]
    Flow,
    #[doc = "`foot`"]
    Foot,
    #[doc = "`gen`"]
    Gen,
    #[doc = "`group`"]
    Group,
    #[doc = "`head`"]
    Head,
    #[doc = "`holds`"]
    Holds,
    #[doc = "`how`"]
    How,
    #[doc = "`implementors`"]
    Implementors,
    #[doc = "`kind`"]
    Kind,
    #[doc = "`label`"]
    Label,
    #[doc = "`line`"]
    Line,
    #[doc = "`main`"]
    Main,
    #[doc = "`means`"]
    Means,
    #[doc = "`more`"]
    More,
    #[doc = "`more-words`"]
    MoreWords,
    #[doc = "`must`"]
    Must,
    #[doc = "`name`"]
    Name,
    #[doc = "`none`"]
    None,
    #[doc = "`note`"]
    Note,
    #[doc = "`nothing`"]
    Nothing,
    #[doc = "`open`"]
    Open,
    #[doc = "`optional`"]
    Optional,
    #[doc = "`options`"]
    Options,
    #[doc = "`options-words`"]
    OptionsWords,
    #[doc = "`packages`"]
    Packages,
    #[doc = "`pill`"]
    Pill,
    #[doc = "`place`"]
    Place,
    #[doc = "`places`"]
    Places,
    #[doc = "`role`"]
    Role,
    #[doc = "`run`"]
    Run,
    #[doc = "`says`"]
    Says,
    #[doc = "`sep`"]
    Sep,
    #[doc = "`sig`"]
    Sig,
    #[doc = "`source`"]
    Source,
    #[doc = "`sub`"]
    Sub,
    #[doc = "`takes`"]
    Takes,
    #[doc = "`tests`"]
    Tests,
    #[doc = "`title`"]
    Title,
    #[doc = "`ty`"]
    Ty,
    #[doc = "`type`"]
    Type,
    #[doc = "`unroll`"]
    Unroll,
    #[doc = "`when`"]
    When,
    #[doc = "`which`"]
    Which,
    #[doc = "`word`"]
    Word,
    #[doc = "`words`"]
    Words,
    #[doc = "`written`"]
    Written,
    #[doc = "`yours`"]
    Yours,
}

impl Slot {
    const fn word(self) -> &'static str {
        match self {
            Self::Across => "across",
            Self::All => "all",
            Self::AllWords => "all-words",
            Self::Arrow => "arrow",
            Self::Aside => "aside",
            Self::AsideLoop => "aside-loop",
            Self::Bound => "bound",
            Self::Card => "card",
            Self::Caret => "caret",
            Self::Chooses => "chooses",
            Self::Click => "click",
            Self::Code => "code",
            Self::Count => "count",
            Self::Default => "default",
            Self::Differs => "differs",
            Self::Doc => "doc",
            Self::Door => "door",
            Self::Elsewhere => "elsewhere",
            Self::ElsewhereHead => "elsewhere-head",
            Self::Empty => "empty",
            Self::Err => "err",
            Self::Expand => "expand",
            Self::Fill => "fill",
            Self::FillClear => "fill-clear",
            Self::FillClearWords => "fill-clear-words",
            Self::FillWords => "fill-words",
            Self::Flow => "flow",
            Self::Foot => "foot",
            Self::Gen => "gen",
            Self::Group => "group",
            Self::Head => "head",
            Self::Holds => "holds",
            Self::How => "how",
            Self::Implementors => "implementors",
            Self::Kind => "kind",
            Self::Label => "label",
            Self::Line => "line",
            Self::Main => "main",
            Self::Means => "means",
            Self::More => "more",
            Self::MoreWords => "more-words",
            Self::Must => "must",
            Self::Name => "name",
            Self::None => "none",
            Self::Note => "note",
            Self::Nothing => "nothing",
            Self::Open => "open",
            Self::Optional => "optional",
            Self::Options => "options",
            Self::OptionsWords => "options-words",
            Self::Packages => "packages",
            Self::Pill => "pill",
            Self::Place => "place",
            Self::Places => "places",
            Self::Role => "role",
            Self::Run => "run",
            Self::Says => "says",
            Self::Sep => "sep",
            Self::Sig => "sig",
            Self::Source => "source",
            Self::Sub => "sub",
            Self::Takes => "takes",
            Self::Tests => "tests",
            Self::Title => "title",
            Self::Ty => "ty",
            Self::Type => "type",
            Self::Unroll => "unroll",
            Self::When => "when",
            Self::Which => "which",
            Self::Word => "word",
            Self::Words => "words",
            Self::Written => "written",
            Self::Yours => "yours",
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
    pub fn field(&self, slot: Slot) -> Self {
        Self(format!("{}-{}", self.0, slot.word()))
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

/// A port's name: the options object a fold opens belongs to it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PortName(String);

impl PortName {
    /// The port called `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// A case's name: the row a fold opens belongs to it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CaseName(String);

impl CaseName {
    /// The case called `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// A fold (a disclosure that unrolls in place).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FoldKey {
    /// An options object.
    Options(PortName),
    /// An example, by its place in the docs.
    Example(usize),
    /// The docs past the second block.
    MoreDocs,
    /// A method group's rows past the sixth.
    MoreMethods(super::view::Do),
    /// A case's or field's extra line.
    More(CaseName),
}
