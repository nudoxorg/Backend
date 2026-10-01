//! The page plan: what a symbol page says, compiled once from its inputs.
//!
//! [`compile`] is a pure function from a [`Source`] to a [`PagePlan`]. Both
//! are plain data: no gpui types and no shared pointers, and every enum has
//! explicit discriminants, so the store can hash a source, cache a plan by
//! content and persist it (W-Instant owns the key, the cache and the
//! schedule). The plan is width-independent; laying it out and stroking it
//! belong to [`super::page`].
//!
//! One grammar for every language (DIRECTION.md §4): a choice is a fork, a
//! record a bracket, a callable a pipe, a contract a socket. What a language
//! changes is only how an idiom is spelled, never the shape.

use crate::semantics::types::{Nowhere, Piece, Scope};

/// Bump whenever [`PagePlan`] or what [`compile`] writes into it changes.
pub const PLAN_SCHEMA: u32 = 4;

// ------------------------------------------------------------------ inputs

/// The language a declaration was written in.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Lang {
    /// Rust.
    Rust = 0,
    /// TypeScript or JavaScript.
    TypeScript = 1,
    /// Go.
    Go = 2,
    /// Python.
    Python = 3,
    /// Java.
    Java = 4,
    /// C#.
    CSharp = 5,
    /// C or C++.
    Cpp = 6,
    /// Anything else.
    Other = 7,
}

impl Lang {
    /// The language for a name as the index spells it (`rust`, `typescript`).
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "rust" => Self::Rust,
            "typescript" | "javascript" | "tsx" | "ts" | "js" => Self::TypeScript,
            "go" => Self::Go,
            "python" => Self::Python,
            "java" => Self::Java,
            "c#" | "csharp" => Self::CSharp,
            "c" | "c++" | "cpp" => Self::Cpp,
            _ => Self::Other,
        }
    }
}

/// A declaration's kind, as its page needs it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum DeclKind {
    /// A struct (Rust, Go, C#, C++).
    Struct = 0,
    /// A class.
    Class = 1,
    /// An interface (TypeScript, Java, C#, Go).
    Interface = 2,
    /// A trait or protocol.
    Trait = 3,
    /// An enum.
    Enum = 4,
    /// A union.
    Union = 5,
    /// A type alias, or a Go named type.
    Alias = 6,
    /// A free function.
    Function = 7,
    /// A method.
    Method = 8,
    /// A constant or static.
    Constant = 9,
    /// A module or namespace.
    Module = 10,
    /// A field, property or member.
    Field = 11,
    /// An enum's variant.
    Variant = 12,
    /// Something else, or unreported.
    Other = 13,
}

/// Whether and how a declaration is deprecated.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Deprecated {
    /// The release it began in, as the source says.
    pub since: Option<String>,
    /// The author's note.
    pub note: Option<String>,
}

/// One member of a declaration, as the index records it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceMember {
    /// Its name.
    pub name: String,
    /// Its kind.
    pub kind: DeclKind,
    /// Its recorded declaration text (`pub date: Option<Date>`,
    /// `readonly inclusive?: boolean`, `Name string`), when captured.
    pub signature: Option<String>,
    /// The first sentence of its docs.
    pub summary: Option<String>,
    /// Its deprecation, when the source marks one.
    pub deprecated: Option<Deprecated>,
    /// What a method does to the value it is called on.
    pub effect: Effect,
    /// Its address, for the page's doors (hover, peek, open).
    pub link: Option<String>,
    /// What an implementor owes for it, when it belongs to a contract.
    pub owes: Owes,
}

/// What an implementor of a contract owes for one of its members, as the
/// producer observed it (Rust's bodiless trait method, `abstract`,
/// `@abstractmethod`, a Go interface method; `m?()`; a default method).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Owes {
    /// Not said.
    #[default]
    Unknown = 0,
    /// Implementors must write it: a notch.
    Required = 1,
    /// Implementors may write it: a dotted notch.
    Optional = 2,
    /// Implementors get it: a tab.
    Provided = 3,
}

/// What a method does to the value it is called on (the index's receiver
/// groups): the fork reads an accessor off it, Getting one reads a maker.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Effect {
    /// Not a method, or unreported.
    #[default]
    None = 0,
    /// Borrows it: reads only.
    Reads = 1,
    /// Borrows it mutably: changes it.
    Changes = 2,
    /// Takes it: uses it up.
    UsesUp = 3,
    /// Makes one (a constructor, a conversion into it).
    Makes = 4,
}

/// A route to one, as the caller already knows it (the world's recipes).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceRail {
    /// What you start from, in words.
    pub from: Ty,
    /// The step's verb (`parse`, `from`, `NewFlagSet`).
    pub verb: String,
    /// The step can fail.
    pub fails: bool,
    /// The step may give nothing.
    pub maybe: bool,
    /// The case it lands on, when the maker's body says so.
    pub lands: Option<String>,
    /// The call as code.
    pub code: Option<String>,
    /// The step's address.
    pub link: Option<String>,
}

/// Everything [`compile`] reads. The store hashes exactly this.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Source {
    /// The declaration's name.
    pub name: String,
    /// The type a method belongs to (`FlagSet` for `FlagSet.Parse`).
    pub owner: Option<String>,
    /// Its kind.
    pub kind: DeclKind,
    /// The language it was written in.
    pub lang: Lang,
    /// Its package's name.
    pub package: String,
    /// Its package's version, when released.
    pub version: Option<String>,
    /// The module path words (`toml::value`, `z.core`).
    pub module: String,
    /// The recorded declaration text, when captured.
    pub signature: Option<String>,
    /// The author's first sentence (markup), when the docs have one.
    pub lede: Option<String>,
    /// Its deprecation, when the source marks one.
    pub deprecated: Option<Deprecated>,
    /// What it is made of: fields, variants, members, in declaration order.
    pub made_of: Vec<SourceMember>,
    /// What it extends or embeds, by name (`$ZodIssueBase`).
    pub extends: Vec<String>,
    /// The sections the page has content for, in any order, with what their
    /// stubs count.
    pub sections: Vec<SectionFacts>,
    /// Its methods (the index's receiver groups), in ledger order.
    pub does: Vec<SourceMember>,
    /// Routes to one the caller already knows (the world's recipes). Empty:
    /// [`compile`] reads them off `does`'s makers.
    pub rails: Vec<SourceRail>,
    /// The oldest release it is known in.
    pub since: Option<String>,
    /// Whether it is in one of your own packages.
    pub yours: bool,
    /// What the docs say can go wrong (`# Errors`, `# Panics`, `Raises:`),
    /// on the declaration or one of its members.
    pub failures: Vec<SourceFailure>,
    /// Who uses it, as the caller counted it.
    pub uses: Uses,
    /// Who does it (a contract's implementors), as the caller counted them.
    pub doers: Doers,
    /// What your own code does with it (relations back to us).
    pub reach: super::reach::Reach,
    /// What the traits it implements let it do.
    pub caps: Vec<Cap>,
    /// The names beside it in its module, in outline order (current included).
    pub siblings: Vec<Sibling>,
    /// What the siblings share (`toml::value`).
    pub scope: String,
    /// What it was at each release the index has read.
    pub history: super::history::History,
}

/// What a trait makes a type able to do, as a mark.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum CapGlyph {
    /// Two squares: it copies.
    Copy = 0,
    /// Two bars: it compares.
    Eq = 1,
    /// An angle: it orders.
    Ord = 2,
    /// A hash: it can be a key.
    Hash = 3,
    /// A dot in a ring: it has a default.
    Default = 4,
    /// Lines of text: it prints.
    Print = 5,
    /// Braces: it debug-prints.
    Debug = 6,
    /// A box with an arrow out: any format can write it.
    Ser = 7,
    /// A box with an arrow in: any format can read one.
    De = 8,
    /// Lines and an arrow: you loop over it.
    Iter = 9,
    /// A crossed square: it is an error.
    Error = 10,
    /// An arrow: it crosses threads.
    Send = 11,
    /// Brackets: it indexes.
    Index = 12,
}

/// A capability: one word for what a trait lets it do.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Cap {
    /// Its mark.
    pub glyph: CapGlyph,
    /// The word (`prints`, `serializes`).
    pub word: String,
    /// What the word means.
    pub tip: String,
}

impl Cap {
    /// The capability a trait named `name` gives, when it is one of the
    /// ones worth a mark (the board's `CAN` table).
    #[must_use]
    pub fn of_trait(name: &str) -> Option<Self> {
        let (glyph, word, tip) = match name {
            "Clone" => (
                CapGlyph::Copy,
                "clones",
                "Clone: .clone() gives an owned copy.",
            ),
            "Copy" => (
                CapGlyph::Copy,
                "copies",
                "Copy: assigning it copies, no move.",
            ),
            "PartialEq" => (CapGlyph::Eq, "compares", "PartialEq: == works on it."),
            "PartialOrd" => (CapGlyph::Ord, "orders", "PartialOrd: < and > work on it."),
            "Hash" => (CapGlyph::Hash, "hashes", "Hash: it can be a map key."),
            "Default" => (
                CapGlyph::Default,
                "has a default",
                "Default: T::default() gives one.",
            ),
            "Display" => (CapGlyph::Print, "prints", "Display: it prints with {}."),
            "Debug" => (
                CapGlyph::Debug,
                "debug-prints",
                "Debug: it prints with {:?}.",
            ),
            "Serialize" => (
                CapGlyph::Ser,
                "serializes",
                "Serialize: any serde format can write it.",
            ),
            "Deserialize" => (
                CapGlyph::De,
                "deserializes",
                "Deserialize: any serde format can read one.",
            ),
            "Iterator" => (CapGlyph::Iter, "iterates", "Iterator: you loop over it."),
            "IntoIterator" => (CapGlyph::Iter, "loops", "IntoIterator: for x in it works."),
            "Error" => (
                CapGlyph::Error,
                "is an error",
                "Error: it can be a ? error.",
            ),
            "Send" => (
                CapGlyph::Send,
                "crosses threads",
                "Send: it can move to another thread.",
            ),
            "Sync" => (
                CapGlyph::Send,
                "shared across threads",
                "Sync: threads can share a reference to it.",
            ),
            "Index" => (CapGlyph::Index, "indexes", "Index: v[i] works on it."),
            _ => return None,
        };
        Some(Self {
            glyph,
            word: word.to_owned(),
            tip: tip.to_owned(),
        })
    }

    /// The capabilities of the traits named in `traits`, each word once, in
    /// the order the traits are given.
    #[must_use]
    pub fn of_traits<'a>(traits: impl IntoIterator<Item = &'a str>) -> Vec<Self> {
        let mut out: Vec<Self> = Vec::new();
        for name in traits {
            if let Some(cap) = Self::of_trait(name)
                && !out.iter().any(|known| known.word == cap.word)
            {
                out.push(cap);
            }
        }
        out
    }
}

/// One name beside the symbol in its module: a chip on the sibling strip.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Sibling {
    /// Its name.
    pub name: String,
    /// Its kind.
    pub kind: DeclKind,
    /// Its address, for its door.
    pub link: Option<String>,
    /// It is the symbol the page is about.
    pub current: bool,
}

/// A contract's implementors: how many, how many yours, the first few.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Doers {
    /// How many.
    pub count: u32,
    /// How many are yours.
    pub yours: u32,
    /// The first few, each with its address and whether it is yours.
    pub names: Vec<(String, Option<String>, bool)>,
    /// Computed by the type checker (Go's implicit satisfaction), not
    /// declared.
    pub computed: bool,
    /// How sure the count is.
    pub tier: Tier,
}

/// One way it can go wrong, as its docs say.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceFailure {
    /// How.
    pub verb: DropVerb,
    /// The member it is said of, when not the declaration itself.
    pub member: Option<String>,
    /// The docs' words.
    pub words: String,
}

/// What the page knows about one of its sections.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SectionFacts {
    /// Which section.
    pub id: SectionId,
    /// How many things it relates (makers, members, users), when counted.
    pub count: Option<u32>,
    /// How many of those are yours.
    pub yours: Option<u32>,
    /// How sure the count is.
    pub tier: Tier,
}

// ------------------------------------------------------------------ the plan

/// A kind's family: its hue and its shape.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Fam {
    /// Modules and packages: slate.
    Namespace = 0,
    /// Types: teal.
    Type = 1,
    /// Contracts: violet.
    Contract = 2,
    /// Callables: periwinkle.
    Callable = 3,
    /// Values: neutral ink.
    Value = 4,
}

/// How a type reads in plain words: one token at a time.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum TokKind {
    /// Plain words (`maybe`, `list of`, `or any text`).
    Word = 0,
    /// A plain value (`text`, `integer`, `bool`): a stone.
    Prim = 1,
    /// A named type: a link, marked by its family.
    Named = 2,
    /// A generic variable.
    Var = 3,
    /// A literal value (`"too_small"`).
    Lit = 4,
    /// Quiet punctuation (`→`, `·`).
    Punct = 5,
}

/// One token of a type.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Tok {
    /// What it is.
    pub kind: TokKind,
    /// What it says.
    pub text: String,
    /// A named token's family.
    pub fam: Fam,
}

/// How a type's glyph is drawn around its head token.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Wrap {
    /// As is.
    Plain = 0,
    /// Dotted and hollow: maybe.
    Maybe = 1,
    /// Stacked: a list of it.
    List = 2,
}

/// A type in plain words, with its exact spelling for ⌥.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Ty {
    /// The words.
    pub toks: Vec<Tok>,
    /// The type as written.
    pub exact: String,
}

impl Ty {
    /// The words as one string.
    #[must_use]
    pub fn plain(&self) -> String {
        let mut out = String::new();
        for (k, tok) in self.toks.iter().enumerate() {
            if k > 0 {
                out.push(' ');
            }
            out.push_str(&tok.text);
        }
        out
    }

    /// How the glyph wraps the head.
    #[must_use]
    pub fn wrap(&self) -> Wrap {
        match self.toks.first() {
            Some(tok) if tok.kind == TokKind::Word && tok.text == "maybe" => Wrap::Maybe,
            Some(tok) if tok.kind == TokKind::Word && tok.text == "list of" => Wrap::List,
            _ => Wrap::Plain,
        }
    }

    /// The token the glyph draws: the first that is not a word or punctuation.
    #[must_use]
    pub fn head(&self) -> Option<&Tok> {
        self.toks
            .iter()
            .find(|tok| !matches!(tok.kind, TokKind::Word | TokKind::Punct))
    }
}

/// A mark in the hero's line: each fact its own component.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Mark {
    /// The oldest release it is known in.
    Since(String) = 0,
    /// Deprecated: struck, with a card.
    Deprecated(Deprecated) = 1,
    /// It is in one of your own packages.
    Yours = 2,
}

/// The top of the page.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Hero {
    /// Its kind.
    pub kind: DeclKind,
    /// Its family (the gem's hue).
    pub fam: Fam,
    /// Its name.
    pub name: String,
    /// The type a method belongs to, drawn dim before the name.
    pub owner: Option<String>,
    /// The author's first sentence. No doc, no lede: the page never speaks
    /// in the author's voice.
    pub lede: Option<String>,
    /// Facts as components, only those the chrome does not already say (the
    /// gem says the kind, the jump bar the path, ⌘. the file).
    pub marks: Vec<Mark>,
    /// The recorded declaration text: read into badges, never printed.
    pub signature: Option<String>,
    /// The language it was written in.
    pub lang: Lang,
    /// The module path words (`toml::value`), the kind line's place.
    pub module: String,
    /// What its traits let it do.
    pub caps: Vec<Cap>,
}

/// A generic parameter, said in words.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Generic {
    /// Its name.
    pub name: String,
    /// What it defaults to, in words (`anything`).
    pub default: Option<Ty>,
}

/// One rung of a record's bracket.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Rung {
    /// The field's name.
    pub name: String,
    /// Its type in plain words.
    pub ty: Ty,
    /// Optional: the rung is dotted and its square hollow.
    pub optional: bool,
    /// Read-only (said once for the record when every rung is).
    pub readonly: bool,
    /// Its doc sentence: never in the row at rest; the margin at ≥ 1440, or
    /// the hover.
    pub doc: Option<String>,
    /// Deprecated: struck.
    pub deprecated: bool,
}

/// A record: a bracket of rungs.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Record {
    /// The fields the index shows, in order.
    pub rungs: Vec<Rung>,
    /// How many more are private (unexported): a count, never names.
    pub private: u32,
    /// Its generic parameters.
    pub generics: Vec<Generic>,
    /// What it extends, by name.
    pub extends: Vec<String>,
    /// Whether every rung is read-only (said once, in the count line).
    pub all_readonly: bool,
}

/// The drawing at the centre of the page.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Spec {
    /// Nothing drawn yet for this shape (the page keeps its indexed body).
    None = 0,
    /// A record: a bracket.
    Record(Record) = 1,
    /// A choice: a fork.
    Choice(Choice) = 2,
    /// A callable: a pipe.
    Callable(Callable) = 3,
    /// A contract: a socket.
    Contract(Contract) = 4,
}

/// One member of a contract: a notch to write or a tab you get.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Slot {
    /// Its name.
    pub name: String,
    /// What it gives back, in words.
    pub gives: Option<Ty>,
    /// It can fail (a `Result`, Go's trailing `error`, `throws`).
    pub fails: bool,
    /// Implementors may leave it out.
    pub optional: bool,
    /// Its doc sentence: only on hover.
    pub doc: Option<String>,
    /// Its address.
    pub link: Option<String>,
}

/// A contract: a socket whose left edge is the spine.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Contract {
    /// What an implementor writes: notches.
    pub write: Vec<Slot>,
    /// What an implementor gets: tabs.
    pub get: Vec<Slot>,
    /// Members the producer did not say either of.
    pub other: Vec<Slot>,
    /// Fields it holds (a TypeScript interface's properties).
    pub held: Vec<Rung>,
    /// Who does it: the plug.
    pub doers: Doers,
}

/// An input to a callable: a port.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Port {
    /// Its name (`s`, `arguments`); empty when the language leaves it out.
    pub name: String,
    /// Its type in words.
    pub ty: Ty,
}

/// What a method is called on: it enters the pipe from above.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Recv {
    /// Its name (`self`, `f`).
    pub name: String,
    /// Its type in words.
    pub ty: Ty,
    /// What the call does to it.
    pub effect: Effect,
}

/// How a callable can fail.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum DropVerb {
    /// `Result<T, E>`: "or fails with E".
    FailsWith = 0,
    /// Go's trailing `error`: "or returns error".
    Returns = 1,
    /// `throws`, `raise`: "throws E".
    Throws = 2,
    /// `panic!`: "panics".
    Panics = 3,
}

impl DropVerb {
    /// The words the drop says.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::FailsWith => "or fails with",
            Self::Returns => "or returns",
            Self::Throws => "throws",
            Self::Panics => "panics",
        }
    }
}

/// One way out that is not the answer: a coral drop.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Drop {
    /// How.
    pub verb: DropVerb,
    /// With what, when it says.
    pub ty: Option<Ty>,
}

/// A callable: a pipe.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Callable {
    /// What it is called on.
    pub receiver: Option<Recv>,
    /// Its inputs, in order.
    pub ports: Vec<Port>,
    /// What it gives back (`None`: nothing).
    pub gives: Option<Ty>,
    /// How it can fail.
    pub drops: Vec<Drop>,
    /// Qualifier words (`waits (async)`, `must use`).
    pub flags: Vec<String>,
}

/// One branch of What can go wrong.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FailBranch {
    /// How.
    pub verb: DropVerb,
    /// The member it is said of (`value[key]`), when not the page itself.
    pub subject: Option<String>,
    /// With what.
    pub ty: Option<Ty>,
    /// The docs' words, when they say.
    pub words: Option<String>,
}

/// One real call site.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Site {
    /// The declaration the use is in.
    pub caller: String,
    /// Where (`manifest.rs:404`, `cobra · command.go`).
    pub place: String,
    /// The line itself, when known.
    pub code: Option<String>,
    /// The name's byte range in `code`, underlined.
    pub mark: Option<(u32, u32)>,
    /// In your own code.
    pub yours: bool,
    /// The caller's address.
    pub link: Option<String>,
}

/// One kind of use: a verb, how many, how many yours, the first names.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct VerbRow {
    /// The verb (`called by`, `taken by`).
    pub verb: String,
    /// How many.
    pub count: u32,
    /// How many are yours.
    pub yours: u32,
    /// The first few, each with its address and whether it is yours.
    pub names: Vec<(String, Option<String>, bool)>,
    /// In how many packages.
    pub packages: u32,
}

/// Who uses it.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Uses {
    /// Real call sites, yours first, three at most shown.
    pub sites: Vec<Site>,
    /// The verb rows.
    pub rows: Vec<VerbRow>,
}

/// How a case is spelled.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum CaseKind {
    /// A variant's name (`String`, `Typed`).
    Name = 0,
    /// A literal (`"email"`): a stone.
    Literal = 1,
    /// A named constant with a printed value (Go's iota block).
    Constant = 2,
    /// A type (`string | number`): the type in words.
    Type = 3,
}

/// An accessor that reads one case: it sits on that case's tine.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Accessor {
    /// Its name (`as_str`).
    pub name: String,
    /// It hands the case out to change (`as_table_mut`).
    pub changes: bool,
    /// Its address.
    pub link: Option<String>,
}

/// One case of a choice: a tine.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Case {
    /// Its name as spelled.
    pub name: String,
    /// How it is spelled.
    pub kind: CaseKind,
    /// What it carries, positional parts in words.
    pub carries: Vec<Ty>,
    /// Named parts (a struct variant).
    pub fields: Vec<Rung>,
    /// Its printed value (an iota position, an explicit discriminant).
    pub value: Option<String>,
    /// Its doc sentence: only on hover, never at rest.
    pub doc: Option<String>,
    /// Accessors that read it.
    pub accessors: Vec<Accessor>,
    /// Deprecated: struck.
    pub deprecated: bool,
    /// Its address.
    pub link: Option<String>,
}

/// A choice: a fork whose trunk is the spine.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Choice {
    /// The cases, in declaration order.
    pub cases: Vec<Case>,
    /// Open: the trunk runs on, dashed, to what else it admits (`any int`,
    /// `any text`, `more may come`).
    pub open: Option<String>,
    /// Fields every case holds (a discriminated union's base), on the trunk
    /// before it splits.
    pub shared: Vec<Rung>,
    /// The field that tells the cases apart (`code`).
    pub told_by: Option<String>,
    /// What every case is underneath (Go's `int`).
    pub each: Option<Ty>,
}

/// One route to one: what you have, a step, what it lands on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Rail {
    /// What you start from, in words.
    pub from: Ty,
    /// The step's verb.
    pub verb: String,
    /// The step can fail: a coral `?`.
    pub fails: bool,
    /// The step may give nothing: a dotted rail.
    pub maybe: bool,
    /// The case it lands on, when known.
    pub lands: Option<String>,
    /// The call as code (the first rail prints it).
    pub code: Option<String>,
    /// The step's address.
    pub link: Option<String>,
}

/// A section's identity: the anchor id's first half, and its order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SectionId {
    /// The specimen.
    Spec = 0,
    /// Your code and it: the relations back to us.
    Yours = 1,
    /// Getting one (or Calling it, or Doing it).
    Getting = 2,
    /// What it does.
    Does = 3,
    /// What can go wrong.
    Fails = 4,
    /// Who uses it.
    Uses = 5,
    /// What changed.
    Changed = 6,
    /// Its own words.
    Words = 7,
}

/// Which margin a section's relations run to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Dir {
    /// No stub.
    None = 0,
    /// From the left margin: things it comes from.
    In = 1,
    /// To the right margin: where it goes.
    Out = 2,
}

/// How sure a count is: its stroke.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Tier {
    /// Compiler-resolved: solid.
    #[default]
    Compiler = 0,
    /// Syntax only: solid, quieter.
    Syntax = 1,
    /// Matched by name or path: dashed.
    Name = 2,
}

/// A section head.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Section {
    /// Which.
    pub id: SectionId,
    /// Its heading.
    pub title: String,
    /// Which margin its stub runs to.
    pub dir: Dir,
    /// The count its stub carries (`59 places · 7 yours`), if any.
    pub count: Option<String>,
    /// The yours part of the count, for mint.
    pub yours: Option<String>,
    /// Its tier.
    pub tier: Tier,
}

/// A page's plan.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PagePlan {
    /// The top.
    pub hero: Hero,
    /// The drawing.
    pub spec: Spec,
    /// The sections, in reading order.
    pub sections: Vec<Section>,
    /// Getting one's rails, best first.
    pub getting: Vec<Rail>,
    /// What can go wrong, one branch per way.
    pub fails: Vec<FailBranch>,
    /// Who uses it.
    pub uses: Uses,
    /// Your code and it.
    pub reach: super::reach::Reach,
    /// The sibling strip.
    pub strip: Vec<Sibling>,
    /// What the strip's names share.
    pub scope: String,
    /// Its history across the releases.
    pub history: super::history::History,
}

/// An anchor's identity: a section and an index into its plan data, so it
/// is the same at every width.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnchorId {
    /// The section.
    pub section: SectionId,
    /// What inside it.
    pub part: Part,
}

/// What an anchor marks inside its section.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Part {
    /// The hero gem.
    Gem = 0,
    /// The title's box.
    Title = 1,
    /// The section's heading.
    Head = 2,
    /// The count line.
    Count = 3,
    /// The n-th rung (or tine, port, notch).
    Row(u16) = 4,
    /// The private count.
    Private = 5,
    /// The spine, from the gem to the last section head.
    Spine = 6,
    /// A section's stub into its margin.
    Stub = 7,
    /// A section's rule.
    Rule = 8,
    /// A section's body, below its head.
    Body = 9,
    /// The n-th rail.
    Rail(u16) = 10,
    /// The n-th shared field on a fork's trunk.
    Shared(u16) = 11,
    /// A fold's "and N more".
    More = 12,
    /// An open choice's dashed end.
    Open = 13,
    /// Where Getting one's rails meet: the terminal bar.
    Terminal = 14,
    /// A pipe's plate.
    Plate = 15,
    /// What a pipe is called on.
    Receiver = 16,
    /// The n-th drop below a pipe.
    Drop(u16) = 17,
    /// What a pipe gives back.
    Gives = 18,
    /// The n-th implementor on a socket's plug.
    Doer(u16) = 19,
}

// ------------------------------------------------------------------ compile

/// The page plan for `source`. Pure: the same source always gives the same
/// plan, and nothing here reads the clock, the world or the window.
#[must_use]
pub fn compile(source: &Source) -> PagePlan {
    let spec = specimen(source);
    let mut facts = source.sections.clone();
    // Your code and it: the section is there whenever anybody read your
    // code's use of the symbol (the page says "nothing read" when it is not
    // known, never that nothing uses it).
    let usable = matches!(
        source.kind,
        DeclKind::Struct
            | DeclKind::Class
            | DeclKind::Interface
            | DeclKind::Trait
            | DeclKind::Enum
            | DeclKind::Union
            | DeclKind::Alias
            | DeclKind::Function
            | DeclKind::Method
            | DeclKind::Constant
    );
    if (source.reach.worth_a_section() || usable)
        && !facts.iter().any(|facts| facts.id == SectionId::Yours)
    {
        facts.push(SectionFacts {
            id: SectionId::Yours,
            count: source.reach.read().then(|| source.reach.total()),
            yours: source
                .reach
                .read()
                .then(|| u32::try_from(source.reach.yours.len()).ok())
                .flatten(),
            tier: if source.reach.basis == super::reach::Basis::Resolved {
                Tier::Compiler
            } else {
                Tier::Name
            },
        });
    }
    // What can go wrong counts its branches on its stub (the pipe's drops
    // alone say themselves: the section is there when the docs say more).
    let fails = fails(source, &spec);
    for facts in facts
        .iter_mut()
        .filter(|facts| facts.id == SectionId::Fails && facts.count.is_none())
    {
        facts.count = u32::try_from(fails.len()).ok();
    }
    facts.sort_by_key(|facts| facts.id);
    facts.dedup_by_key(|facts| facts.id);
    // What it does counts what it shows: accessors that moved onto their
    // tines are counted there, not twice.
    if let Spec::Choice(choice) = &spec {
        let on_tines = choice
            .cases
            .iter()
            .map(|case| case.accessors.len())
            .sum::<usize>();
        for facts in facts.iter_mut().filter(|facts| facts.id == SectionId::Does) {
            facts.count = facts
                .count
                .map(|n| n.saturating_sub(u32::try_from(on_tines).unwrap_or(u32::MAX)));
        }
    }
    let sections = facts
        .iter()
        .filter(|facts| facts.id != SectionId::Spec)
        .map(|facts| section(source, facts))
        .collect();
    // The reach draws each crate's real lines: Who uses it keeps only its
    // verb rows then, so a call site is never said twice.
    let mut uses = source.uses.clone();
    if source
        .reach
        .yours
        .iter()
        .chain(&source.reach.others)
        .any(|used| !used.lines.is_empty())
    {
        uses.sites.clear();
    }
    PagePlan {
        hero: hero(source, &spec),
        spec,
        sections,
        getting: rails::rails(source),
        fails,
        uses,
        reach: source.reach.clone(),
        strip: source.siblings.clone(),
        scope: source.scope.clone(),
        history: source.history.clone(),
    }
}

/// What can go wrong: a callable's drops, each with what its docs say,
/// then whatever else the docs say fails or panics.
fn fails(source: &Source, spec: &Spec) -> Vec<FailBranch> {
    let mut said = source.failures.iter().map(Some).collect::<Vec<_>>();
    let mut take = |verbs: &[DropVerb]| {
        said.iter_mut()
            .find(|failure| {
                failure.is_some_and(|failure| {
                    failure.member.is_none() && verbs.contains(&failure.verb)
                })
            })
            .and_then(Option::take)
            .map(|failure| failure.words.clone())
    };
    let mut out = Vec::new();
    if let Spec::Callable(callable) = spec {
        for drop in &callable.drops {
            let words = match drop.verb {
                DropVerb::Panics => take(&[DropVerb::Panics]),
                _ => take(&[DropVerb::FailsWith, DropVerb::Returns, DropVerb::Throws]),
            };
            out.push(FailBranch {
                verb: drop.verb,
                subject: None,
                ty: drop.ty.clone(),
                words,
            });
        }
    }
    for failure in said.into_iter().flatten() {
        out.push(FailBranch {
            verb: failure.verb,
            subject: failure.member.clone(),
            ty: None,
            words: Some(failure.words.clone()),
        });
    }
    out
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn section(source: &Source, facts: &SectionFacts) -> Section {
    let callable = matches!(source.kind, DeclKind::Function | DeclKind::Method);
    let contract = matches!(source.kind, DeclKind::Trait)
        || (source.kind == DeclKind::Interface
            && source.made_of.iter().any(|m| m.kind != DeclKind::Field));
    let (title, dir) = match facts.id {
        SectionId::Spec => ("", Dir::None),
        SectionId::Yours => ("Your code and it", Dir::In),
        SectionId::Getting if callable => ("Calling it", Dir::In),
        SectionId::Getting if contract => ("Doing it", Dir::In),
        SectionId::Getting => ("Getting one", Dir::In),
        SectionId::Does => ("What it does", Dir::Out),
        SectionId::Fails => (
            "What can go wrong",
            if callable { Dir::Out } else { Dir::None },
        ),
        SectionId::Uses => ("Who uses it", Dir::Out),
        SectionId::Changed => ("What changed", Dir::None),
        SectionId::Words => ("Its own words", Dir::None),
    };
    let count = facts.count.filter(|n| *n > 0).map(|n| match facts.id {
        SectionId::Getting if callable => plural(n, "caller", "callers"),
        SectionId::Getting => plural(n, "way", "ways"),
        SectionId::Does => plural(n, "operation", "operations"),
        SectionId::Fails => plural(n, "way", "ways"),
        SectionId::Uses | SectionId::Yours => plural(n, "place", "places"),
        _ => n.to_string(),
    });
    let yours = facts.yours.filter(|n| *n > 0).map(|n| {
        if facts.id == SectionId::Yours {
            plural(n, "crate", "crates")
        } else {
            format!("{n} yours")
        }
    });
    Section {
        id: facts.id,
        title: title.to_owned(),
        dir,
        count,
        yours,
        tier: facts.tier,
    }
}

fn fam_of(kind: DeclKind) -> Fam {
    match kind {
        DeclKind::Struct | DeclKind::Class | DeclKind::Enum | DeclKind::Union | DeclKind::Alias => {
            Fam::Type
        }
        DeclKind::Interface | DeclKind::Trait => Fam::Contract,
        DeclKind::Function | DeclKind::Method => Fam::Callable,
        DeclKind::Constant | DeclKind::Field | DeclKind::Variant => Fam::Value,
        DeclKind::Module | DeclKind::Other => Fam::Namespace,
    }
}

fn hero(source: &Source, spec: &Spec) -> Hero {
    // An interface that only holds data is a record in plain words: it takes
    // the type's hue, not the contract's.
    let fam = match spec {
        Spec::Record(_) | Spec::Choice(_) => Fam::Type,
        Spec::Callable(_) => Fam::Callable,
        Spec::Contract(_) => Fam::Contract,
        Spec::None => fam_of(source.kind),
    };
    let mut marks = Vec::new();
    if let Some(since) = &source.since {
        marks.push(Mark::Since(since.clone()));
    }
    if let Some(deprecated) = &source.deprecated {
        marks.push(Mark::Deprecated(deprecated.clone()));
    }
    if source.yours {
        marks.push(Mark::Yours);
    }
    Hero {
        kind: source.kind,
        fam,
        name: source.name.clone(),
        owner: source.owner.clone(),
        lede: source.lede.clone().filter(|lede| !lede.trim().is_empty()),
        marks,
        signature: source.signature.clone(),
        lang: source.lang,
        module: source.module.clone(),
        caps: source.caps.clone(),
    }
}

fn specimen(source: &Source) -> Spec {
    if let Some(choice) = choice::choice(source) {
        return Spec::Choice(choice);
    }
    if matches!(source.kind, DeclKind::Function | DeclKind::Method)
        && let Some(callable) = callable::callable(source)
    {
        return Spec::Callable(callable);
    }
    if let Some(contract) = contract::contract(source) {
        return Spec::Contract(contract);
    }
    let fields = source
        .made_of
        .iter()
        .filter(|member| member.kind == DeclKind::Field)
        .collect::<Vec<_>>();
    let shaped_as_record = match source.kind {
        DeclKind::Struct | DeclKind::Class => true,
        // An interface of properties only is a record; one with methods is a contract.
        DeclKind::Interface | DeclKind::Trait => {
            !fields.is_empty() && fields.len() == source.made_of.len()
        }
        _ => false,
    };
    if !shaped_as_record || fields.is_empty() {
        return Spec::None;
    }
    let mut rungs = Vec::new();
    let mut private = 0_u32;
    for member in fields {
        let Some(field) = field(member, source.lang) else {
            continue;
        };
        if field.private {
            private += 1;
            continue;
        }
        rungs.push(Rung {
            name: member.name.clone(),
            ty: field.ty,
            optional: field.optional,
            readonly: field.readonly,
            doc: member.summary.clone().filter(|doc| !doc.trim().is_empty()),
            deprecated: member.deprecated.is_some(),
        });
    }
    let all_readonly = !rungs.is_empty() && rungs.iter().all(|rung| rung.readonly);
    Spec::Record(Record {
        rungs,
        private,
        generics: generics(source),
        extends: source.extends.clone(),
        all_readonly,
    })
}

// ------------------------------------------------------------------ fields

struct Field {
    ty: Ty,
    optional: bool,
    readonly: bool,
    private: bool,
}

/// A field's recorded text read in its own language.
pub(super) fn field_of(member: &SourceMember, lang: Lang) -> Option<(Ty, bool, bool)> {
    field(member, lang)
        .filter(|field| !field.private)
        .map(|field| (field.ty, field.optional, field.readonly))
}

fn field(member: &SourceMember, lang: Lang) -> Option<Field> {
    let text = member.signature.as_deref().unwrap_or("").trim();
    let text = strip_comment(text, lang);
    match lang {
        Lang::Rust => {
            let (vis, rest) = rust_visibility(text);
            let ty = rest
                .split_once(':')
                .map_or(rest, |(_, ty)| ty)
                .trim()
                .trim_end_matches(',');
            let optional = ty.starts_with("Option<");
            Some(Field {
                ty: words(ty, Lang::Rust),
                optional,
                readonly: false,
                private: !vis && !text.is_empty(),
            })
        }
        Lang::TypeScript => {
            let mut rest = text;
            let mut readonly = false;
            loop {
                let word = rest.split_whitespace().next().unwrap_or("");
                match word {
                    "readonly" => {
                        readonly = true;
                        rest = rest[word.len()..].trim_start();
                    }
                    "public" | "declare" | "static" | "abstract" | "override" => {
                        rest = rest[word.len()..].trim_start()
                    }
                    "private" | "protected" => {
                        return Some(Field {
                            ty: Ty::default(),
                            optional: false,
                            readonly,
                            private: true,
                        });
                    }
                    _ => break,
                }
            }
            let colon = rest.find(':');
            let head = colon.map_or(rest, |at| &rest[..at]).trim();
            let optional = head.ends_with('?');
            let ty = colon
                .map_or("", |at| &rest[at + 1..])
                .trim()
                .trim_end_matches([';', ','])
                .trim();
            Some(Field {
                ty: words(ty, Lang::TypeScript),
                optional,
                readonly,
                private: head.starts_with('#'),
            })
        }
        Lang::Go => {
            // `Name Type`, or a bare embedded type.
            let rest = text.trim_start_matches(member.name.as_str()).trim();
            let ty = rest.split('`').next().unwrap_or(rest).trim();
            let private = member.name.chars().next().is_some_and(char::is_lowercase);
            Some(Field {
                ty: words(if ty.is_empty() { &member.name } else { ty }, Lang::Go),
                optional: false,
                readonly: false,
                private,
            })
        }
        _ => {
            let ty = text.split_once(':').map_or("", |(_, ty)| ty).trim();
            Some(Field {
                ty: words(ty, lang),
                optional: false,
                readonly: false,
                private: false,
            })
        }
    }
}

fn strip_comment(text: &str, lang: Lang) -> &str {
    match lang {
        Lang::Go | Lang::Rust | Lang::TypeScript => text.split("//").next().unwrap_or(text).trim(),
        _ => text,
    }
}

fn rust_visibility(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix("pub(") {
        // `pub(crate)`: visible here, not to you.
        let close = rest.find(')').map_or(0, |at| at + 1);
        return (false, rest[close..].trim());
    }
    if let Some(rest) = text.strip_prefix("pub ") {
        return (true, rest.trim());
    }
    (false, text)
}

fn generics(source: &Source) -> Vec<Generic> {
    let Some(signature) = source.signature.as_deref() else {
        return Vec::new();
    };
    let Some(at) = signature.find(source.name.as_str()) else {
        return Vec::new();
    };
    let after = &signature[at + source.name.len()..];
    let Some(inner) = after.strip_prefix('<') else {
        return Vec::new();
    };
    let mut depth = 1_i32;
    let mut end = None;
    for (k, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(k);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else { return Vec::new() };
    crate::semantics::types::split_top(&inner[..end], ',')
        .into_iter()
        .filter_map(|part| {
            let part = part.trim();
            if part.starts_with('\'') || part.is_empty() {
                return None;
            }
            let (head, default) = part
                .split_once('=')
                .map_or((part, None), |(h, d)| (h.trim(), Some(d.trim())));
            let name = head
                .split([':', ' '])
                .next()
                .unwrap_or(head)
                .trim()
                .to_owned();
            Some(Generic {
                name,
                default: default.map(|d| words(d, source.lang)),
            })
        })
        .collect()
}

// ------------------------------------------------------------------ types in words

fn tok(kind: TokKind, text: impl Into<String>) -> Tok {
    Tok {
        kind,
        text: text.into(),
        fam: Fam::Type,
    }
}

fn named(text: impl Into<String>) -> Tok {
    Tok {
        kind: TokKind::Named,
        text: text.into(),
        fam: Fam::Type,
    }
}

/// A type as written in `lang`, in plain words.
#[must_use]
pub fn words(ty: &str, lang: Lang) -> Ty {
    let exact = ty.trim().to_owned();
    let toks = match lang {
        Lang::Rust => rust_words(&exact),
        Lang::TypeScript => ts_words(&exact),
        Lang::Go => go_words(&exact),
        _ => vec![named(exact.clone())],
    };
    Ty { toks, exact }
}

fn plain_number(name: &str) -> Option<&'static str> {
    Some(match name {
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128"
        | "isize" => "integer",
        "f32" | "f64" => "float",
        "bool" => "bool",
        "char" => "character",
        _ => return None,
    })
}

fn rust_words(ty: &str) -> Vec<Tok> {
    if ty.is_empty() {
        return Vec::new();
    }
    let scope = Scope::new(&Nowhere);
    toks_of(&scope.spell_text(ty).pieces)
}

/// A type the semantics layer already spelled (a world recipe's port), in
/// the plan's words.
#[must_use]
pub fn spelled(pieces: &[Piece], exact: &str) -> Ty {
    Ty {
        toks: toks_of(pieces),
        exact: exact.trim().to_owned(),
    }
}

fn toks_of(pieces: &[Piece]) -> Vec<Tok> {
    let mut out = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Space => {}
            Piece::Word(w) => {
                if w.as_ref() == "text" {
                    out.push(tok(TokKind::Prim, "text"));
                } else {
                    out.push(tok(TokKind::Word, w.to_string()));
                }
            }
            Piece::Punct(p) => out.push(tok(TokKind::Punct, p.to_string())),
            Piece::Var(v) | Piece::Assoc(v) => out.push(tok(TokKind::Var, v.to_string())),
            Piece::Name { text, .. } => out.push(named(text.to_string())),
            Piece::Prim(p) => out.push(tok(TokKind::Prim, plain_number(p).unwrap_or(p).to_owned())),
        }
    }
    out
}

fn split_union(text: &str) -> Vec<String> {
    let (mut depth, mut cur, mut out) = (0_i32, String::new(), Vec::new());
    let mut quote = None;
    for ch in text.chars() {
        if let Some(q) = quote {
            cur.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '<' | '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            '>' | ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            '|' if depth == 0 => {
                out.push(cur.trim().to_owned());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out.into_iter().filter(|part| !part.is_empty()).collect()
}

fn ts_prim(name: &str) -> Option<&'static str> {
    Some(match name {
        "string" => "text",
        "number" => "number",
        "bigint" => "big integer",
        "boolean" => "bool",
        "unknown" | "any" => "anything",
        "void" | "undefined" => "nothing",
        "null" => "null",
        "symbol" => "symbol",
        "PropertyKey" => "key",
        "never" => "never",
        _ => return None,
    })
}

fn ts_words(ty: &str) -> Vec<Tok> {
    let ty = ty.trim();
    if ty.is_empty() {
        return Vec::new();
    }
    let parts = split_union(ty);
    if parts.len() > 1 {
        let mut out = Vec::new();
        let literals = parts
            .iter()
            .filter(|p| p.starts_with(['"', '\'']))
            .collect::<Vec<_>>();
        // `(string & {})` keeps a literal union open; the index sometimes cuts
        // the recorded text at its brace, so its prefix counts too.
        let is_open = |p: &str| p.replace(' ', "").starts_with("(string&");
        let open = parts.iter().any(|p| is_open(p));
        let rest = parts
            .iter()
            .filter(|p| !p.starts_with(['"', '\'']) && !is_open(p))
            .collect::<Vec<_>>();
        if !literals.is_empty() {
            out.push(tok(TokKind::Word, "one of"));
            for (k, lit) in literals.iter().enumerate() {
                if k > 0 {
                    out.push(tok(TokKind::Punct, "·"));
                }
                out.push(tok(TokKind::Lit, lit.replace('\'', "\"")));
            }
        }
        for part in rest {
            if !out.is_empty() {
                out.push(tok(TokKind::Word, "or"));
            }
            out.extend(ts_words(part));
        }
        if open {
            out.push(tok(TokKind::Word, "or any text"));
        }
        return out;
    }
    if ty.starts_with(['"', '\'']) {
        return vec![tok(TokKind::Lit, ty.replace('\'', "\""))];
    }
    if let Some(inner) = ty.strip_suffix("[]") {
        let inner = inner.trim().trim_start_matches('(').trim_end_matches(')');
        let mut out = vec![tok(TokKind::Word, "list of")];
        out.extend(ts_words(inner));
        return out;
    }
    if let Some(p) = ts_prim(ty) {
        return vec![tok(TokKind::Prim, p)];
    }
    if let Some(open) = ty.find('<').filter(|_| ty.ends_with('>')) {
        let head = ty[..open].rsplit('.').next().unwrap_or(&ty[..open]);
        let args = crate::semantics::types::split_top(&ty[open + 1..ty.len() - 1], ',');
        match head {
            "Record" if args.len() == 2 => {
                let mut out = vec![tok(TokKind::Word, "map")];
                out.extend(ts_words(&args[0]));
                out.push(tok(TokKind::Punct, "→"));
                out.extend(ts_words(&args[1]));
                return out;
            }
            "Array" | "ReadonlyArray" if args.len() == 1 => {
                let mut out = vec![tok(TokKind::Word, "list of")];
                out.extend(ts_words(&args[0]));
                return out;
            }
            "Promise" if args.len() == 1 => {
                let mut out = vec![tok(TokKind::Word, "later")];
                out.extend(ts_words(&args[0]));
                return out;
            }
            _ => return vec![named(head.to_owned())],
        }
    }
    let last = ty.rsplit('.').next().unwrap_or(ty);
    if last.len() <= 6 && matches!(last, "T" | "U" | "K" | "V" | "Input" | "Output") {
        return vec![tok(TokKind::Var, last)];
    }
    vec![named(last.to_owned())]
}

fn go_prim(name: &str) -> Option<&'static str> {
    Some(match name {
        "string" => "text",
        "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32"
        | "uint64" | "uintptr" | "rune" => "integer",
        "float32" | "float64" => "float",
        "bool" => "bool",
        "byte" => "byte",
        "error" => "error",
        "any" | "interface{}" => "anything",
        _ => return None,
    })
}

fn go_words(ty: &str) -> Vec<Tok> {
    let ty = ty.trim();
    if ty.is_empty() {
        return Vec::new();
    }
    if let Some(inner) = ty.strip_prefix("[]") {
        let mut out = vec![tok(TokKind::Word, "list of")];
        out.extend(go_words(inner));
        return out;
    }
    if let Some(inner) = ty.strip_prefix('*') {
        return go_words(inner);
    }
    if let Some(rest) = ty.strip_prefix("map[") {
        let mut depth = 1_i32;
        let mut close = None;
        for (k, ch) in rest.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(close) = close {
            let mut out = vec![tok(TokKind::Word, "map")];
            out.extend(go_words(&rest[..close]));
            out.push(tok(TokKind::Punct, "→"));
            out.extend(go_words(&rest[close + 1..]));
            return out;
        }
    }
    if ty.starts_with("func") {
        return vec![tok(TokKind::Word, "callback")];
    }
    if let Some(p) = go_prim(ty) {
        return vec![tok(TokKind::Prim, p)];
    }
    vec![named(ty.rsplit('.').next().unwrap_or(ty).to_owned())]
}

mod callable;
mod choice;
mod contract;
mod rails;
#[cfg(test)]
mod tests;
