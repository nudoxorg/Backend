//! The page model's data: what the anatomy elements draw and the graph's
//! prism lays out. Pure data with no world attached, so the elements build
//! without the world and a test can write one by hand. The builders live in
//! `relations` and `page`.

use super::caps::Cap;
use super::members::Receiver;
use super::types::{NodeId, Piece, Spelled};
use super::usage::Excerpt;
use crate::graph::Kind;
use gpui::SharedString;

/// Which side of the symbol a group sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// What it comes from.
    Left,
    /// What it is: the capability line, not a direction.
    Is,
    /// What it goes into.
    Right,
}

/// How a group relates to the symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Word {
    /// Traits it implements, derived or written.
    Is,
    /// A type's parts (fields, payloads).
    MadeOf,
    /// Callables that give one back.
    MadeBy,
    /// Types that implement a trait.
    ImplementedBy,
    /// Callables that take one.
    TakenBy,
    /// Fields and payloads of that type.
    HeldBy,
    /// Callers of its members.
    CallsIt,
    /// Everything else that refers to it.
    UsedBy,
    /// A callable's inputs.
    Takes,
    /// A callable's callers.
    CalledFrom,
    /// A callable's outputs.
    Gives,
    /// What a callable calls.
    Calls,
}

impl Word {
    /// The group's words, as the page and the graph print them.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::MadeOf => "made of",
            Self::MadeBy => "made by",
            Self::ImplementedBy => "implemented by",
            Self::TakenBy => "taken by",
            Self::HeldBy => "held by",
            Self::CallsIt => "calls it",
            Self::UsedBy => "used by",
            Self::Takes => "takes",
            Self::CalledFrom => "called from",
            Self::Gives => "gives",
            Self::Calls => "calls",
        }
    }
}

/// How an `is` entry arrives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Note {
    /// An impl written by hand.
    Written,
    /// `#[derive(..)]`.
    Derived,
    /// Given through another trait (`ToString` via `Display`).
    Via(SharedString),
}

impl Note {
    /// The note's words.
    #[must_use]
    pub fn text(&self) -> SharedString {
        match self {
            Self::Written => SharedString::new_static("written"),
            Self::Derived => SharedString::new_static("derived"),
            Self::Via(from) => SharedString::from(format!("via {from}")),
        }
    }
}

/// One related thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The symbol, when the world holds it.
    pub node: Option<NodeId>,
    /// Text in place of the symbol's name (the joined derives, a foreign trait).
    pub text: Option<SharedString>,
    /// How it arrives (the `is` group only).
    pub note: Option<Note>,
    /// The derives this entry stands for (the joined-derives entry only).
    pub caps: Vec<SharedString>,
}

impl Entry {
    /// An entry for a symbol.
    #[must_use]
    pub const fn node(node: NodeId) -> Self {
        Self { node: Some(node), text: None, note: None, caps: Vec::new() }
    }
}

/// A named group of related things.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// How they relate.
    pub word: Word,
    /// Which side they sit on.
    pub side: Side,
    /// The things, yours first, then by importance.
    pub entries: Vec<Entry>,
}

/// One row of a prism column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrismRow {
    /// The symbol, when the world holds it.
    pub node: Option<NodeId>,
    /// Its kind (the row's mark).
    pub kind: Option<Kind>,
    /// The name as shown (`Page::new`, `Page.relations`).
    pub text: SharedString,
    /// The quiet note beside it: another package's short name, or the module
    /// path when the same name appears twice.
    pub note: Option<SharedString>,
}

/// One named group in a prism column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// The group's word.
    pub word: Word,
    /// The first rows.
    pub rows: Vec<PrismRow>,
    /// How many more there are ("and N more").
    pub more: usize,
}

/// A symbol's anatomy.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// One of several variants.
    Fork(Fork),
    /// Fields held together.
    Holds(Holds),
    /// Inputs to an output.
    Pipe(Pipe),
    /// What an implementor writes and what it gets.
    Contract(Contract),
    /// Another name for a type.
    Alias(Spelled),
    /// A fixed value of a type.
    Constant(Spelled),
    /// Nothing to draw (a macro, a module).
    None,
}

/// A variant's payload.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    /// No payload.
    Unit,
    /// Positional parts (`Typed(SemanticLinkKind, RelationDirection)`).
    Tuple(Vec<Spelled>),
    /// Named parts.
    Record(Vec<(SharedString, Spelled)>),
}

/// One branch of a fork.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    /// The variant.
    pub node: NodeId,
    /// Its name.
    pub name: SharedString,
    /// What it carries.
    pub payload: Payload,
    /// Its one sentence.
    pub doc: Option<SharedString>,
}

/// One of: a sum type.
#[derive(Clone, Debug, PartialEq)]
pub struct Fork {
    /// `#[non_exhaustive]`: "and more may come".
    pub open: bool,
    /// The branches, in declaration order.
    pub branches: Vec<Branch>,
}

impl Fork {
    /// The heading.
    #[must_use]
    pub const fn heading(&self) -> &'static str {
        if self.open { "one of, and more may come" } else { "one of" }
    }
}

/// One held field.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The field.
    pub node: NodeId,
    /// Its name (`None` for a positional field).
    pub name: Option<SharedString>,
    /// Its type.
    pub ty: Spelled,
    /// Its one sentence.
    pub doc: Option<SharedString>,
    /// Visible outside its module.
    pub public: bool,
}

/// All of: a record.
#[derive(Clone, Debug, PartialEq)]
pub struct Holds {
    /// The fields, in declaration order.
    pub fields: Vec<Field>,
}

impl Holds {
    /// How many fields are private.
    #[must_use]
    pub fn private(&self) -> usize {
        self.fields.iter().filter(|f| !f.public).count()
    }

    /// The heading: `holds`, `holds, 2 private`, `holds, all private`,
    /// `holds nothing — a marker`.
    #[must_use]
    pub fn heading(&self) -> String {
        let private = self.private();
        match (self.fields.len(), private) {
            (0, _) => "holds nothing — a marker".to_owned(),
            (n, p) if p == n => "holds, all private".to_owned(),
            (_, 0) => "holds".to_owned(),
            (_, p) => format!("holds, {p} private"),
        }
    }
}

/// One input of a pipe.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    /// The parameter's name, or the receiver's words (`reads it`).
    pub name: SharedString,
    /// Its type (`None` for the receiver).
    pub ty: Option<Spelled>,
}

/// A generic parameter and its sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Where {
    /// The parameter.
    pub name: SharedString,
    /// `is any Deserialize`.
    pub sentence: Vec<Piece>,
    /// The bounds as written (⌥).
    pub source: SharedString,
}

/// Takes → gives.
#[derive(Clone, Debug, PartialEq)]
pub struct Pipe {
    /// The receiver first (if any), then the parameters.
    pub inputs: Vec<Input>,
    /// The success output (`None`: nothing).
    pub output: Option<Spelled>,
    /// The failure exit: `Some(None)` fails with an unnamed error.
    pub fails: Option<Option<Spelled>>,
    /// Generic parameters as sentences.
    pub wheres: Vec<Where>,
    /// Qualifier words (`waits (async)`, `must use`).
    pub flags: Vec<&'static str>,
}

/// A callable's signature in words: `(inputs) → output`.
#[derive(Clone, Debug, PartialEq)]
pub struct SigLine {
    /// Parameter types (no receiver).
    pub params: Vec<Spelled>,
    /// The result.
    pub ret: Option<Spelled>,
}

/// A member row: a name, its signature, its one sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberRow {
    /// The member.
    pub node: NodeId,
    /// Its name.
    pub name: SharedString,
    /// Its kind.
    pub kind: Kind,
    /// Its signature in words.
    pub sig: SigLine,
    /// Its one sentence.
    pub doc: Option<SharedString>,
}

/// Look-alikes folded into one row.
#[derive(Clone, Debug, PartialEq)]
pub struct FoldRow {
    /// The shared prefix (`visit_`).
    pub prefix: SharedString,
    /// Every folded member, in order.
    pub members: Vec<NodeId>,
    /// Their names without the prefix (`bool`, `i8`, …).
    pub suffixes: Vec<SharedString>,
    /// The distinct first-parameter types, in order.
    pub inputs: Vec<Spelled>,
    /// The shared result.
    pub ret: Option<Spelled>,
}

impl FoldRow {
    /// How many inputs a row shows before "and N more".
    pub const SHOWN: usize = 4;
}

/// A row in a member list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// One member.
    One(MemberRow),
    /// Look-alikes.
    Fold(FoldRow),
}

/// You write / you get.
#[derive(Clone, Debug, PartialEq)]
pub struct Contract {
    /// Required members.
    pub write: Vec<Row>,
    /// Provided members.
    pub get: Vec<Row>,
    /// How many types in the world implement it.
    pub implementors: usize,
}

/// A Does group.
#[derive(Clone, Debug, PartialEq)]
pub struct DoesGroup {
    /// What the members do to it.
    pub receiver: Receiver,
    /// The rows.
    pub rows: Vec<Row>,
}

/// Members that arrive through one trait.
#[derive(Clone, Debug, PartialEq)]
pub struct Through {
    /// The trait's name.
    pub trait_name: SharedString,
    /// The members.
    pub members: Vec<(NodeId, SharedString)>,
}

/// The Does section.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Does {
    /// Own members grouped by receiver.
    pub groups: Vec<DoesGroup>,
    /// Trait-provided members grouped by trait.
    pub through: Vec<Through>,
}

impl Does {
    /// Nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.through.is_empty()
    }
}

/// The hero's facts line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// `enum`, `method`.
    pub kind: &'static str,
    /// `present::glyph`.
    pub place: SharedString,
    /// Distinct places that use it.
    pub used_in: usize,
    /// How many of them are yours.
    pub yours: usize,
}

/// One use of the symbol in a caller's body.
#[derive(Clone, Debug, PartialEq)]
pub struct Use {
    /// The caller.
    pub caller: NodeId,
    /// The caller's package, short.
    pub package: SharedString,
    /// The file's name.
    pub file: SharedString,
    /// The statement.
    pub excerpt: Excerpt,
}

