//! What the simple symbol page says, as plain data: no gpui types, so the
//! derivation ([`super::derive`]) is pure and its tests run in milliseconds,
//! and the renderer ([`super::page`]) only lays out what is here.
//!
//! The shapes follow the design board's page model (`data/page6/*.json`):
//! a header, the call (ports, then how it can end), docs, "if it fails",
//! "what it is", "what you can do with it", "in your workspace", and the
//! context rail.

use std::collections::BTreeSet;
use std::ops::Range;

pub use super::facts::Implementors;

/// The language a declaration is written in, as the page tags it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Lang {
    /// Rust.
    Rust,
    /// Python.
    Python,
    /// JavaScript.
    JavaScript,
    /// TypeScript.
    TypeScript,
    /// Go.
    Go,
    /// Java.
    Java,
    /// C#.
    CSharp,
    /// C or C++.
    Cpp,
    /// Anything else.
    Other,
}

impl Lang {
    /// The language for a name as the index spells it. TypeScript and
    /// JavaScript arrive under one name; `javascript` names the latter, and a
    /// signature with no annotation tells the two apart otherwise.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "rust" => Self::Rust,
            "python" => Self::Python,
            "javascript" | "js" | "jsx" => Self::JavaScript,
            "typescript" | "ts" | "tsx" => Self::TypeScript,
            "go" => Self::Go,
            "java" => Self::Java,
            "c#" | "csharp" => Self::CSharp,
            "c" | "c++" | "cpp" => Self::Cpp,
            _ => Self::Other,
        }
    }

    /// The tag on the header (`Rust`, `TypeScript`).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::CSharp => "C#",
            Self::Cpp => "C++",
            Self::Other => "",
        }
    }

    /// Whether the language declares no types of its own (what it shows is
    /// read from docs and code, and says so).
    #[must_use]
    pub const fn undeclared(self) -> bool {
        matches!(self, Self::Python | Self::JavaScript)
    }
}

/// What a declaration is, as the header's mark and word say.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Kind {
    /// A free function.
    Function,
    /// A method.
    Method,
    /// An enum, or a union of literals.
    Enum,
    /// A struct, class or record.
    Struct,
    /// A trait, protocol or interface.
    Trait,
    /// A type alias or a named type.
    Alias,
    /// A constant.
    Constant,
    /// A module.
    Module,
    /// Anything else.
    Other,
}

impl Kind {
    /// The kind word above the name.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Enum => "enum",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Alias => "type",
            Self::Constant => "constant",
            Self::Module => "module",
            Self::Other => "item",
        }
    }

    /// Whether it is called.
    #[must_use]
    pub const fn callable(self) -> bool {
        matches!(self, Self::Function | Self::Method)
    }
}

/// Where a type's spelling came from.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Origin {
    /// Written in the declaration.
    #[default]
    Declared,
    /// Read from the docs (an untyped language).
    Docs,
    /// Read from the code around it (a default value, a use).
    Code,
}

impl Origin {
    /// Whether the type is not declared: it is drawn with a dotted underline.
    #[must_use]
    pub const fn dotted(self) -> bool {
        !matches!(self, Self::Declared)
    }
}

/// A type: the plain word first, what is written after it, quieter.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Ty {
    /// The plain words (`text`, `a list of Value`, `a count`).
    pub word: String,
    /// The type as written (`&'a str`), when it says more than the word.
    pub written: Option<String>,
    /// The address of the declaration the word names, when there is one.
    pub link: Option<String>,
    /// The generic parameter this is (`T`): drawn as a violet pill.
    pub generic: Option<String>,
    /// Where the spelling came from.
    pub origin: Origin,
    /// It holds more of itself (an enum case that nests).
    pub loops: bool,
}

impl Ty {
    /// A declared type that is just a word.
    #[must_use]
    pub fn plain(word: impl Into<String>) -> Self {
        Self {
            word: word.into(),
            ..Self::default()
        }
    }

    /// The same type, read rather than declared.
    #[must_use]
    pub fn read_from(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }
}

/// How an input joins the rail.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Joint {
    /// A filled dot.
    Required,
    /// A hollow dot.
    Optional,
    /// A double dot: any number.
    Rest,
}

/// What a call does to the value it is made on.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Effect {
    /// Ink: it reads it.
    Reads,
    /// Amber: it changes it.
    Changes,
    /// Peri: it uses it up.
    UsesUp,
}

impl Effect {
    /// The words beside the receiver.
    #[must_use]
    pub const fn says(self) -> &'static str {
        match self {
            Self::Reads => "reads it",
            Self::Changes => "changes it",
            Self::UsesUp => "uses it up",
        }
    }
}

/// The value a method is called on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Receiver {
    /// What the call does to it, when the language says.
    pub effect: Option<Effect>,
    /// Its type.
    pub ty: Ty,
}

/// One outcome an option can change: what the call gives becomes another
/// shape (`nothrow`: "or fails" becomes "or nothing").
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Change {
    /// A failure becomes nothing.
    FailsToNone,
    /// One value becomes many.
    OneToMany,
    /// Nothing becomes a failure.
    NoneToFails,
}

/// One row of an options object.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OptionRow {
    /// Its name.
    pub name: String,
    /// Its type.
    pub ty: Ty,
    /// What it does, one line.
    pub note: String,
    /// The outcome it changes, when it does.
    pub change: Option<Change>,
}

/// One input on the rail.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Port {
    /// Its name.
    pub name: String,
    /// How it joins.
    pub joint: Joint,
    /// Its type.
    pub ty: Ty,
    /// `= default`, as written.
    pub default: Option<String>,
    /// One line about it, from the docs.
    pub note: Option<String>,
    /// An options object, collapsed until asked.
    pub options: Vec<OptionRow>,
}

/// What a call gives.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Gives {
    /// The type, when it gives anything.
    pub ty: Option<Ty>,
    /// It gives each of many (an iterator, a stream, a generator).
    pub many: bool,
    /// What the generic output is, in words ("you choose it").
    pub role: Option<String>,
}

/// The verb of the failure row, in the language's own word.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FailWord {
    /// Rust: "or fails".
    Fails,
    /// Python: "or raises".
    Raises,
    /// JavaScript, TypeScript, Java, C#: "or throws".
    Throws,
    /// A promise: "or rejects".
    Rejects,
    /// Go: "or returns" an error.
    Returns,
}

impl FailWord {
    /// The label on the rail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fails => "or fails",
            Self::Raises => "or raises",
            Self::Throws => "or throws",
            Self::Rejects => "or rejects",
            Self::Returns => "or returns",
        }
    }
}

/// How a call can fail.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Failure {
    /// The verb.
    pub word: FailWord,
    /// The error type.
    pub ty: Ty,
    /// One line: when.
    pub when: String,
}

/// A call: what goes in, and how it can end.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Call {
    /// The value it is called on.
    pub receiver: Option<Receiver>,
    /// Its inputs, in order.
    pub ports: Vec<Port>,
    /// It answers later (a Promise, a future).
    pub later: Option<String>,
    /// What it gives.
    pub gives: Gives,
    /// When it gives nothing, in words; `None` when it always gives.
    pub none: Option<String>,
    /// How it fails.
    pub fails: Option<Failure>,
}

impl Call {
    /// The outcome glyphs a row can wear, in order.
    #[must_use]
    pub fn outcomes(&self) -> Outcomes {
        Outcomes {
            fails: self.fails.is_some(),
            none: self.none.is_some(),
            later: self.later.is_some(),
            many: self.gives.many,
        }
    }
}

/// Which outcome shapes a callable has (the glyphs on rows and siblings).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Outcomes {
    /// It can fail.
    pub fails: bool,
    /// It can give nothing.
    pub none: bool,
    /// It answers later.
    pub later: bool,
    /// It gives many.
    pub many: bool,
}

impl Outcomes {
    /// Whether none is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.fails || self.none || self.later || self.many)
    }
}

/// How a generic parameter relates to the call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Role {
    /// It only appears in what comes out: you choose it.
    Choose,
    /// It appears both in and out: the same kind you gave.
    Through,
    /// It only appears in what goes in: any that fits.
    Needs,
}

impl Role {
    /// The role in words.
    #[must_use]
    pub const fn says(self) -> &'static str {
        match self {
            Self::Choose => "you choose it",
            Self::Through => "the same kind you gave",
            Self::Needs => "any that fits",
        }
    }
}

/// One bound on a generic: what it must be.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Bound {
    /// The bound as named (`Deserialize`).
    pub name: String,
    /// What it means, in words.
    pub means: String,
}

/// A generic parameter: its role, its bounds and what the workspace picks.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Generic {
    /// Its name (`T`).
    pub name: String,
    /// How it relates to the call.
    pub role: Role,
    /// One sentence about it.
    pub says: String,
    /// What it must be.
    pub bounds: Vec<Bound>,
    /// Where the bounds were read from.
    pub origin: Origin,
}

/// One doc block, in markup (`` `code` ``, `**strong**`, `[links](..)`).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Block {
    /// A paragraph.
    Para(String),
    /// A heading.
    Head(String),
    /// A list item.
    Item(String),
    /// An example, as code.
    Code(String),
}

/// The docs after the lede, without the errors section.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Docs {
    /// What is shown, in order.
    pub blocks: Vec<Block>,
}

/// One kind an error tells you.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ErrorKind {
    /// Its name (`Syntax`).
    pub name: String,
    /// Its first doc line.
    pub doc: String,
    /// Why it cannot happen here, when it cannot.
    pub impossible: Option<String>,
}

/// "If it fails": the error, when, and what kinds it has.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Fails {
    /// The error type.
    pub ty: Ty,
    /// The full "when", in markup.
    pub when: String,
    /// What kinds the error has.
    pub kinds: Vec<ErrorKind>,
    /// How you tell them apart (`Error::classify() → Category`).
    pub tells: Option<String>,
}

/// One case of an enum.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Case {
    /// Its name.
    pub name: String,
    /// What it holds, in order (none: "nothing inside").
    pub holds: Vec<Ty>,
    /// One line about it.
    pub doc: String,
    /// More, shown when the row is opened.
    pub more: Option<String>,
    /// Its address, for a door.
    pub link: Option<String>,
    /// How many of your places match on it or build it.
    pub yours: Yours,
}

/// What your places do with one case, field or method: how many make it,
/// match on it, read it and change it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Yours {
    /// Places that build or call it.
    pub made: u32,
    /// Places that match on it.
    pub matched: u32,
    /// Places that read it.
    pub read: u32,
    /// Places that change it.
    pub changed: u32,
    /// Places that reach it in some other way.
    pub other: u32,
}

impl Yours {
    /// Every place.
    #[must_use]
    pub const fn total(self) -> u32 {
        self.made + self.matched + self.read + self.changed + self.other
    }

    /// Counts one place that does `verb`.
    pub fn count(&mut self, verb: Verb) {
        match verb {
            Verb::Makes | Verb::Calls => self.made += 1,
            Verb::Matches => self.matched += 1,
            Verb::Reads => self.read += 1,
            Verb::Changes | Verb::UsesUp => self.changed += 1,
            _ => self.other += 1,
        }
    }

    /// The words for a row: `you read it · 5`, `you match it · 4 · build it · 2`;
    /// none when no place reaches it.
    #[must_use]
    pub fn says(self) -> Option<String> {
        let parts: Vec<String> = [
            (self.matched, "match"),
            (self.made, "build"),
            (self.read, "read"),
            (self.changed, "change"),
            (self.other, "use"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{word} it · {n}"))
        .collect();
        (!parts.is_empty()).then(|| format!("you {}", parts.join(" · ")))
    }
}

/// What your places do with one member of the symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tally {
    /// The member's name.
    pub member: String,
    /// What the places do with it.
    pub yours: Yours,
}

/// One field of a struct.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Field {
    /// Its name.
    pub name: String,
    /// Its type.
    pub ty: Ty,
    /// One line about it.
    pub doc: String,
    /// More, shown when the row is opened.
    pub more: Option<String>,
    /// Its address, for a door.
    pub link: Option<String>,
    /// What your places do with it.
    pub yours: Yours,
}

/// One method a trait's implementor writes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Owed {
    /// Its name.
    pub name: String,
    /// What it takes, in words.
    pub takes: Ty,
    /// One line about it.
    pub doc: String,
    /// Its outcome glyphs.
    pub outcomes: Outcomes,
    /// Its address, for a door.
    pub link: Option<String>,
}

/// What a type is made of.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Shape {
    /// Exactly one of these: a fork.
    OneOf(Vec<Case>),
    /// All of these together: a bracket.
    Holds {
        /// The public fields.
        fields: Vec<Field>,
        /// How many are private.
        hidden: usize,
    },
    /// What you write to implement a trait.
    Write {
        /// The required methods.
        rows: Vec<Owed>,
        /// How far it is implemented, when counted.
        implementors: Option<Implementors>,
    },
}

/// What a method does with the value: the group it is listed under.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Do {
    /// Makes one.
    Makes,
    /// Reads it.
    Reads,
    /// Changes it.
    Changes,
    /// Uses it up.
    UsesUp,
}

impl Do {
    /// The group heading.
    #[must_use]
    pub const fn head(self) -> &'static str {
        match self {
            Self::Makes => "Makes one",
            Self::Reads => "Reads it",
            Self::Changes => "Changes it",
            Self::UsesUp => "Uses it up",
        }
    }
}

/// One method row: `name (inputs) → output` and its outcome glyphs.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Row {
    /// Its name.
    pub name: String,
    /// What it takes, in words (`From` collapsed lists what it converts from).
    pub takes: Vec<String>,
    /// What it gives, in words.
    pub gives: Option<String>,
    /// Its outcome glyphs.
    pub outcomes: Outcomes,
    /// Its first doc line.
    pub doc: String,
    /// Its address, for a hop.
    pub link: Option<String>,
    /// How many places of yours call it.
    pub yours: u32,
    /// The other names your places call it by (`parse` is `from_str`).
    pub also: Vec<String>,
}

/// Methods that share a verb.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Group {
    /// What they do.
    pub verb: Do,
    /// The rows, yours first.
    pub rows: Vec<Row>,
}

/// One sibling in "Next to it".
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Sibling {
    /// Its name.
    pub name: String,
    /// Its mark.
    pub kind: Kind,
    /// The word that differs (`from bytes`).
    pub differs: String,
    /// Its outcome glyphs, when read.
    pub outcomes: Outcomes,
    /// Its address, for a hop.
    pub link: Option<String>,
}

/// A trait the type implements, as a small chip.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Cap {
    /// The trait as named.
    pub name: String,
    /// What it lets you do, in words; the name itself when unknown.
    pub word: String,
    /// Which mark it wears.
    pub mark: CapMark,
    /// Derived rather than written by hand.
    pub derived: bool,
}

/// The marks of the capabilities the board names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CapMark {
    /// Two squares: it copies.
    Copy,
    /// Two bars: it compares.
    Eq,
    /// A hash: it can be a map key.
    Hash,
    /// Braces: it prints for debugging.
    Debug,
    /// Lines: it prints.
    Print,
    /// A dot in a ring: it has a default.
    Default,
    /// An arrow out of a box: serde can write it.
    Ser,
    /// An arrow into a box: serde can read one.
    De,
    /// Text and an arrow: it parses from text.
    FromStr,
    /// Brackets: `v[i]` works.
    Index,
    /// A box: it reads serde data itself.
    Reader,
    /// A crossed box: it is an error.
    Error,
    /// An order angle.
    Order,
    /// A loop: you can loop over it.
    Iter,
    /// A plain trait box: an unknown trait, shown by its name.
    Other,
}

/// One release on disk, for "Across releases".
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Release {
    /// The version.
    pub version: String,
    /// The one you pin.
    pub pinned: bool,
    /// It differs from the pinned one.
    pub differs: bool,
}

/// The rail's source entry.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceAt {
    /// The file, relative to its package.
    pub file: String,
    /// The line.
    pub line: u32,
    /// Where to open it (the file's absolute path), when it is on disk.
    pub open: Option<String>,
    /// `serde_json 1.0.151`.
    pub package: String,
    /// It is the release you pin.
    pub pinned: bool,
}

/// The context beside the page.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rail {
    /// Where it is defined.
    pub source: Option<SourceAt>,
    /// What is next to it.
    pub siblings: Vec<Sibling>,
    /// What it can do.
    pub can: Vec<Cap>,
    /// Its releases on disk.
    pub releases: Vec<Release>,
    /// What changed between them, in words, when the page can say.
    pub across: Option<String>,
    /// How we know its types, for an untyped language.
    pub how: Option<String>,
}

/// The header.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Head {
    /// What it is.
    pub kind: Kind,
    /// The path words (`serde_json`, `value`).
    pub path: Vec<String>,
    /// Its language.
    pub lang: Lang,
    /// Its name.
    pub name: String,
    /// The first doc sentence, in markup.
    pub lede: Option<String>,
}

/// Everything the page says without the workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    /// The header.
    pub head: Head,
    /// The call, for a callable.
    pub call: Option<Call>,
    /// Its generic parameters.
    pub generics: Vec<Generic>,
    /// The docs.
    pub docs: Docs,
    /// If it fails.
    pub fails: Option<Fails>,
    /// What it is.
    pub shape: Option<Shape>,
    /// What you can do with it.
    pub verbs: Vec<Group>,
    /// The rail.
    pub rail: Rail,
}

/// The twelve verbs of "In your workspace": what a place does with it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Verb {
    /// Calls it.
    Calls,
    /// Makes one.
    Makes,
    /// Reads it.
    Reads,
    /// Changes it.
    Changes,
    /// Uses it up.
    UsesUp,
    /// Matches on it.
    Matches,
    /// Holds one.
    Holds,
    /// Derives it.
    Derives,
    /// Implements it.
    Implements,
    /// Asks for it (a bound).
    AsksFor,
    /// Names it.
    Names,
    /// Imports it.
    Imports,
}

impl Verb {
    /// Every verb, in chip order.
    pub const ALL: [Self; 12] = [
        Self::Calls,
        Self::Makes,
        Self::Reads,
        Self::Changes,
        Self::UsesUp,
        Self::Matches,
        Self::Holds,
        Self::Derives,
        Self::Implements,
        Self::AsksFor,
        Self::Names,
        Self::Imports,
    ];

    /// The chip's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Makes => "makes",
            Self::Reads => "reads",
            Self::Changes => "changes",
            Self::UsesUp => "uses up",
            Self::Matches => "matches",
            Self::Holds => "holds",
            Self::Derives => "derives",
            Self::Implements => "implements",
            Self::AsksFor => "asks for",
            Self::Names => "names",
            Self::Imports => "imports",
        }
    }
}

/// Where a use sits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Ctx {
    /// In code that ships.
    Code,
    /// In a test.
    Test,
}

/// One place a workspace package names the symbol.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Use {
    /// The package.
    pub package: String,
    /// The file, relative to the package (shown left-truncated).
    pub file: String,
    /// The file's absolute path, to open it.
    pub path: String,
    /// The line, one-based.
    pub line: u32,
    /// The line of code.
    pub text: String,
    /// The name to mark in the line, as a byte range of `text`.
    pub mark: Option<Range<usize>>,
    /// What the place does.
    pub verb: Verb,
    /// The member of the symbol it reaches, when it is one.
    pub member: Option<String>,
    /// Test or code.
    pub ctx: Ctx,
    /// What a generic is at this call (`Metadata`), when it can be read.
    pub fill: Option<String>,
    /// Matched by name rather than resolved.
    pub approx: bool,
}

/// One package's uses, for the picker and the rail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageUse {
    /// The package.
    pub package: String,
    /// How many places.
    pub count: usize,
    /// The verbs it uses, in chip order.
    pub verbs: BTreeSet<Verb>,
}

/// What one generic is chosen as in the workspace.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Fill {
    /// The type (`Metadata`).
    pub ty: String,
    /// The packages that choose it, most places first.
    pub packages: Vec<String>,
    /// How many places.
    pub places: usize,
}

/// Every place the workspace names the symbol, with what is derived from
/// them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Uses {
    /// The places, in the order the index gave them.
    pub all: Vec<Use>,
    /// The workspace has no source in the symbol's language: what the places are.
    pub elsewhere: Option<String>,
}

/// Whether the places that only import the symbol are listed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportsListed {
    /// They are.
    Shown,
    /// They are left out.
    Hidden,
}

/// Whether the places in tests are listed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestsListed {
    /// They are.
    Included,
    /// They are left out.
    Left,
}

/// What a list of places leaves out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Listed {
    /// The imports.
    pub imports: ImportsListed,
    /// The tests.
    pub tests: TestsListed,
}

impl Listed {
    /// What the rail counts: the places that use it, in code or in tests.
    pub const USES: Self = Self {
        imports: ImportsListed::Hidden,
        tests: TestsListed::Included,
    };

    /// Whether `site` is listed.
    #[must_use]
    pub fn lists(self, site: &Use) -> bool {
        (self.imports == ImportsListed::Shown || site.verb != Verb::Imports)
            && (self.tests == TestsListed::Included || site.ctx != Ctx::Test)
    }
}

impl Uses {
    /// The packages, most places first, counting what `listed` lists.
    #[must_use]
    pub fn packages(&self, listed: Listed) -> Vec<PackageUse> {
        let mut out: Vec<PackageUse> = Vec::new();
        for site in &self.all {
            if !listed.lists(site) {
                continue;
            }
            match out.iter_mut().find(|entry| entry.package == site.package) {
                Some(entry) => {
                    entry.count += 1;
                    entry.verbs.insert(site.verb);
                }
                None => out.push(PackageUse {
                    package: site.package.clone(),
                    count: 1,
                    verbs: BTreeSet::from([site.verb]),
                }),
            }
        }
        out.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.package.cmp(&b.package))
        });
        out
    }

    /// What `generic`-typed places choose, most places first.
    #[must_use]
    pub fn fills(&self, tests: TestsListed) -> Vec<Fill> {
        let mut out: Vec<Fill> = Vec::new();
        for site in &self.all {
            let Some(fill) = &site.fill else { continue };
            if tests == TestsListed::Left && site.ctx == Ctx::Test {
                continue;
            }
            match out.iter_mut().find(|entry| entry.ty == *fill) {
                Some(entry) => {
                    entry.places += 1;
                    if !entry.packages.contains(&site.package) {
                        entry.packages.push(site.package.clone());
                    }
                }
                None => out.push(Fill {
                    ty: fill.clone(),
                    packages: vec![site.package.clone()],
                    places: 1,
                }),
            }
        }
        for fill in &mut out {
            fill.packages.sort();
        }
        out.sort_by(|a, b| b.places.cmp(&a.places).then_with(|| a.ty.cmp(&b.ty)));
        out
    }

    /// What the places do with each member, most places first.
    #[must_use]
    pub fn tally(&self) -> Vec<Tally> {
        let mut out: Vec<Tally> = Vec::new();
        for site in &self.all {
            let Some(member) = &site.member else { continue };
            let at = match out.iter().position(|tally| tally.member == *member) {
                Some(at) => at,
                None => {
                    out.push(Tally {
                        member: member.clone(),
                        yours: Yours::default(),
                    });
                    out.len() - 1
                }
            };
            out[at].yours.count(site.verb);
        }
        out.sort_by(|a, b| {
            b.yours
                .total()
                .cmp(&a.yours.total())
                .then_with(|| a.member.cmp(&b.member))
        });
        out
    }
}

impl Implementors {
    /// The line under a trait's socket: "8,322 types implement it across 343
    /// crates on this machine, 83% of them by derive."
    #[must_use]
    pub fn says(self) -> String {
        let thousands = |n: u32| {
            let digits = n.to_string();
            let mut out = String::new();
            for (at, digit) in digits.chars().enumerate() {
                if at > 0 && (digits.len() - at) % 3 == 0 {
                    out.push(',');
                }
                out.push(digit);
            }
            out
        };
        let types = if self.total == 1 {
            "type implements"
        } else {
            "types implement"
        };
        let mut line = format!(
            "{} {types} it across {} crate{} on this machine",
            thousands(self.total),
            thousands(self.crates),
            if self.crates == 1 { "" } else { "s" }
        );
        if let Some(derived) = self.derived {
            line.push_str(&format!(", {derived}% of them by derive"));
        }
        line.push('.');
        line
    }
}
