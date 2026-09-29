//! What the shell reads about a declaration, as the plain input of
//! [`derive::compile`](super::derive::compile): text and small enums, no
//! index types. The desktop fills it from the index's page; a test fills it
//! from a real signature.

use super::view::{Block, Kind, Lang};

/// What a documentation section is about (the conventions' names).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SectionKind {
    /// How it fails: `# Errors`, `@throws`, `Raises:`.
    Errors,
    /// When it panics.
    Panics,
    /// What the caller must uphold.
    Safety,
    /// Worked examples.
    Examples,
    /// What it gives back.
    Returns,
    /// What each parameter means.
    Parameters,
    /// Why not to use it.
    Deprecated,
    /// Anything else.
    Other,
}

/// One section of the docs.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Section {
    /// What it is about.
    pub kind: SectionKind,
    /// The prose that belongs to no entry, in markup.
    pub body: String,
    /// Named entries (`@param cmd …`, `ValueError: …`): subject, then prose.
    pub entries: Vec<(String, String)>,
}

/// What a method does to the value it is called on.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Receives {
    /// Not said.
    #[default]
    Unknown,
    /// Reads `&self`.
    Reads,
    /// Changes `&mut self`.
    Changes,
    /// Uses up `self`.
    UsesUp,
    /// No receiver: makes one or stands alone.
    Makes,
}

/// What an implementor owes for a member of a contract.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Owes {
    /// Not said.
    #[default]
    Unknown,
    /// It must write it.
    Required,
    /// It may write it.
    Optional,
    /// It gets it.
    Provided,
}

/// A member of a declaration: a variant, a field, a method.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Member {
    /// Its name.
    pub name: String,
    /// Its recorded declaration text.
    pub signature: Option<String>,
    /// The first sentence of its docs.
    pub summary: Option<String>,
    /// The second paragraph of its docs.
    pub more: Option<String>,
    /// What a method does to the value it is called on.
    pub receives: Receives,
    /// What an implementor owes for it.
    pub owes: Owes,
    /// Its address, for a door.
    pub link: Option<String>,
    /// Its own `# Errors` section, when it has one.
    pub errors: Option<String>,
}

/// One name beside the declaration in its module.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Beside {
    /// Its name.
    pub name: String,
    /// Its kind.
    pub kind: Kind,
    /// Its recorded declaration text, when read.
    pub signature: Option<String>,
    /// Its address.
    pub link: Option<String>,
}

/// Where the source is.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Site {
    /// The file, relative to the package.
    pub file: String,
    /// The line.
    pub line: u32,
    /// Where to open it: the file's absolute path.
    pub open: Option<String>,
}

/// Everything [`compile`](super::derive::compile) reads.
#[derive(Clone, Debug)]
pub struct Facts {
    /// The declaration's name.
    pub name: String,
    /// The type a method belongs to.
    pub owner: Option<String>,
    /// Its kind.
    pub kind: Kind,
    /// Its language.
    pub lang: Lang,
    /// Its package's display name.
    pub package: String,
    /// Its package's version, when released.
    pub version: Option<String>,
    /// It is the release you pin.
    pub pinned: bool,
    /// The path words above the name (`serde_json`, `value`).
    pub path: Vec<String>,
    /// The recorded declaration text.
    pub signature: Option<String>,
    /// The type names that carry an address, as the signature's tokens link
    /// them: `(Value, address)`.
    pub links: Vec<(String, String)>,
    /// The docs as blocks, in markup: the first paragraph is the lede.
    pub docs: Vec<Block>,
    /// The docs' conventional sections.
    pub sections: Vec<Section>,
    /// Where the source is.
    pub site: Option<Site>,
    /// Fields and variants.
    pub made_of: Vec<Member>,
    /// Methods, functions, constructors.
    pub does: Vec<Member>,
    /// The traits it implements, by name, and whether each is derived.
    pub implements: Vec<(String, bool)>,
    /// How many types implement it, for a trait.
    pub implementors: Option<u32>,
    /// The names beside it in its module, in outline order.
    pub beside: Vec<Beside>,
    /// The releases on disk: `(version, differs from the pinned one)`.
    pub releases: Vec<(String, bool)>,
    /// What changed between the releases, in words.
    pub across: Option<String>,
    /// The kinds the error type tells you, when its page is read: name and
    /// first doc line.
    pub error_kinds: Vec<(String, String)>,
    /// How you tell them apart (`Error::classify() → Category`).
    pub error_tells: Option<String>,
}

impl Facts {
    /// Facts for a declaration, everything else empty.
    #[must_use]
    pub fn new(name: &str, kind: Kind, lang: Lang, package: &str) -> Self {
        Self {
            name: name.to_owned(),
            owner: None,
            kind,
            lang,
            package: package.to_owned(),
            version: None,
            pinned: true,
            path: vec![package.to_owned()],
            signature: None,
            links: Vec::new(),
            docs: Vec::new(),
            sections: Vec::new(),
            site: None,
            made_of: Vec::new(),
            does: Vec::new(),
            implements: Vec::new(),
            implementors: None,
            beside: Vec::new(),
            releases: Vec::new(),
            across: None,
            error_kinds: Vec::new(),
            error_tells: None,
        }
    }

    /// The link for a type name, when the signature's tokens carry one.
    #[must_use]
    pub fn link(&self, name: &str) -> Option<&str> {
        self.links.iter().find(|(known, _)| known == name).map(|(_, address)| address.as_str())
    }
}
