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
pub const PLAN_SCHEMA: u32 = 1;

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
            if k > 0 { out.push(' '); }
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
        self.toks.iter().find(|tok| !matches!(tok.kind, TokKind::Word | TokKind::Punct))
    }
}

/// A mark in the hero's line: each fact its own component.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Mark {
    /// The package and its version.
    Package {
        /// The package.
        name: String,
        /// Its version.
        version: Option<String>,
    } = 0,
    /// Where it is declared.
    Path {
        /// The module path.
        text: String,
    } = 1,
    /// Deprecated: struck, with a card.
    Deprecated(Deprecated) = 2,
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
    /// Facts as components.
    pub marks: Vec<Mark>,
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
}

/// A section's identity: the anchor id's first half, and its order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SectionId {
    /// The specimen.
    Spec = 0,
    /// Getting one (or Calling it, or Doing it).
    Getting = 1,
    /// What it does.
    Does = 2,
    /// What can go wrong.
    Fails = 3,
    /// Who uses it.
    Uses = 4,
    /// What changed.
    Changed = 5,
    /// Its own words.
    Words = 6,
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Tier {
    /// Compiler-resolved: solid.
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
}

// ------------------------------------------------------------------ compile

/// The page plan for `source`. Pure: the same source always gives the same
/// plan, and nothing here reads the clock, the world or the window.
#[must_use]
pub fn compile(source: &Source) -> PagePlan {
    let spec = specimen(source);
    let mut facts = source.sections.clone();
    facts.sort_by_key(|facts| facts.id);
    facts.dedup_by_key(|facts| facts.id);
    let sections = facts.iter().filter(|facts| facts.id != SectionId::Spec).map(|facts| section(source, facts)).collect();
    PagePlan { hero: hero(source, &spec), spec, sections }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn section(source: &Source, facts: &SectionFacts) -> Section {
    let callable = matches!(source.kind, DeclKind::Function | DeclKind::Method);
    let contract = matches!(source.kind, DeclKind::Trait) || (source.kind == DeclKind::Interface && source.made_of.iter().any(|m| m.kind != DeclKind::Field));
    let (title, dir) = match facts.id {
        SectionId::Spec => ("", Dir::None),
        SectionId::Getting if callable => ("Calling it", Dir::In),
        SectionId::Getting if contract => ("Doing it", Dir::In),
        SectionId::Getting => ("Getting one", Dir::In),
        SectionId::Does => ("What it does", Dir::Out),
        SectionId::Fails => ("What can go wrong", if callable { Dir::Out } else { Dir::None }),
        SectionId::Uses => ("Who uses it", Dir::Out),
        SectionId::Changed => ("What changed", Dir::None),
        SectionId::Words => ("Its own words", Dir::None),
    };
    let count = facts.count.filter(|n| *n > 0).map(|n| match facts.id {
        SectionId::Getting if callable => plural(n, "caller", "callers"),
        SectionId::Getting => plural(n, "way", "ways"),
        SectionId::Does => plural(n, "operation", "operations"),
        SectionId::Fails => plural(n, "kind", "kinds"),
        SectionId::Uses => plural(n, "place", "places"),
        _ => n.to_string(),
    });
    let yours = facts.yours.filter(|n| *n > 0).map(|n| format!("{n} yours"));
    Section { id: facts.id, title: title.to_owned(), dir, count, yours, tier: facts.tier }
}

fn fam_of(kind: DeclKind) -> Fam {
    match kind {
        DeclKind::Struct | DeclKind::Class | DeclKind::Enum | DeclKind::Union | DeclKind::Alias => Fam::Type,
        DeclKind::Interface | DeclKind::Trait => Fam::Contract,
        DeclKind::Function | DeclKind::Method => Fam::Callable,
        DeclKind::Constant | DeclKind::Field | DeclKind::Variant => Fam::Value,
        DeclKind::Module | DeclKind::Other => Fam::Namespace,
    }
}

fn hero(source: &Source, spec: &Spec) -> Hero {
    // An interface that only holds data is a record in plain words: it takes
    // the type's hue, not the contract's.
    let fam = match spec { Spec::Record(_) => Fam::Type, Spec::None => fam_of(source.kind) };
    let mut marks = vec![Mark::Package { name: source.package.clone(), version: source.version.clone() }];
    if !source.module.is_empty() && source.module != source.package {
        marks.push(Mark::Path { text: source.module.clone() });
    }
    if let Some(deprecated) = &source.deprecated { marks.push(Mark::Deprecated(deprecated.clone())); }
    Hero {
        kind: source.kind,
        fam,
        name: source.name.clone(),
        owner: source.owner.clone(),
        lede: source.lede.clone().filter(|lede| !lede.trim().is_empty()),
        marks,
    }
}

fn specimen(source: &Source) -> Spec {
    let fields = source.made_of.iter().filter(|member| member.kind == DeclKind::Field).collect::<Vec<_>>();
    let shaped_as_record = match source.kind {
        DeclKind::Struct | DeclKind::Class => true,
        // An interface of properties only is a record; one with methods is a contract.
        DeclKind::Interface | DeclKind::Trait => !fields.is_empty() && fields.len() == source.made_of.len(),
        _ => false,
    };
    if !shaped_as_record || fields.is_empty() { return Spec::None; }
    let mut rungs = Vec::new();
    let mut private = 0_u32;
    for member in fields {
        let Some(field) = field(member, source.lang) else { continue };
        if field.private { private += 1; continue; }
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
fn field(member: &SourceMember, lang: Lang) -> Option<Field> {
    let text = member.signature.as_deref().unwrap_or("").trim();
    let text = strip_comment(text, lang);
    match lang {
        Lang::Rust => {
            let (vis, rest) = rust_visibility(text);
            let ty = rest.split_once(':').map_or(rest, |(_, ty)| ty).trim().trim_end_matches(',');
            let optional = ty.starts_with("Option<");
            Some(Field { ty: words(ty, Lang::Rust), optional, readonly: false, private: !vis && !text.is_empty() })
        }
        Lang::TypeScript => {
            let mut rest = text;
            let mut readonly = false;
            loop {
                let word = rest.split_whitespace().next().unwrap_or("");
                match word {
                    "readonly" => { readonly = true; rest = rest[word.len()..].trim_start(); }
                    "public" | "declare" | "static" | "abstract" | "override" => rest = rest[word.len()..].trim_start(),
                    "private" | "protected" => return Some(Field { ty: Ty::default(), optional: false, readonly, private: true }),
                    _ => break,
                }
            }
            let colon = rest.find(':');
            let head = colon.map_or(rest, |at| &rest[..at]).trim();
            let optional = head.ends_with('?');
            let ty = colon.map_or("", |at| &rest[at + 1..]).trim().trim_end_matches([';', ',']).trim();
            Some(Field { ty: words(ty, Lang::TypeScript), optional, readonly, private: head.starts_with('#') })
        }
        Lang::Go => {
            // `Name Type`, or a bare embedded type.
            let rest = text.trim_start_matches(member.name.as_str()).trim();
            let ty = rest.split('`').next().unwrap_or(rest).trim();
            let private = member.name.chars().next().is_some_and(char::is_lowercase);
            Some(Field { ty: words(if ty.is_empty() { &member.name } else { ty }, Lang::Go), optional: false, readonly: false, private })
        }
        _ => {
            let ty = text.split_once(':').map_or("", |(_, ty)| ty).trim();
            Some(Field { ty: words(ty, lang), optional: false, readonly: false, private: false })
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
    if let Some(rest) = text.strip_prefix("pub ") { return (true, rest.trim()); }
    (false, text)
}

fn generics(source: &Source) -> Vec<Generic> {
    let Some(signature) = source.signature.as_deref() else { return Vec::new() };
    let Some(at) = signature.find(source.name.as_str()) else { return Vec::new() };
    let after = &signature[at + source.name.len()..];
    let Some(inner) = after.strip_prefix('<') else { return Vec::new() };
    let mut depth = 1_i32;
    let mut end = None;
    for (k, ch) in inner.char_indices() {
        match ch { '<' => depth += 1, '>' => { depth -= 1; if depth == 0 { end = Some(k); break; } } _ => {} }
    }
    let Some(end) = end else { return Vec::new() };
    crate::semantics::types::split_top(&inner[..end], ',').into_iter().filter_map(|part| {
        let part = part.trim();
        if part.starts_with('\'') || part.is_empty() { return None; }
        let (head, default) = part.split_once('=').map_or((part, None), |(h, d)| (h.trim(), Some(d.trim())));
        let name = head.split([':', ' ']).next().unwrap_or(head).trim().to_owned();
        Some(Generic { name, default: default.map(|d| words(d, source.lang)) })
    }).collect()
}

// ------------------------------------------------------------------ types in words

fn tok(kind: TokKind, text: impl Into<String>) -> Tok {
    Tok { kind, text: text.into(), fam: Fam::Type }
}

fn named(text: impl Into<String>) -> Tok {
    Tok { kind: TokKind::Named, text: text.into(), fam: Fam::Type }
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
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => "integer",
        "f32" | "f64" => "float",
        "bool" => "bool",
        "char" => "character",
        _ => return None,
    })
}

fn rust_words(ty: &str) -> Vec<Tok> {
    if ty.is_empty() { return Vec::new(); }
    let scope = Scope::new(&Nowhere);
    let spelled = scope.spell_text(ty);
    let mut out = Vec::new();
    for piece in &spelled.pieces {
        match piece {
            Piece::Space => {}
            Piece::Word(w) => {
                if w.as_ref() == "text" { out.push(tok(TokKind::Prim, "text")); } else { out.push(tok(TokKind::Word, w.to_string())); }
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
        if let Some(q) = quote { cur.push(ch); if ch == q { quote = None; } continue; }
        match ch {
            '"' | '\'' | '`' => { quote = Some(ch); cur.push(ch); }
            '<' | '(' | '[' | '{' => { depth += 1; cur.push(ch); }
            '>' | ')' | ']' | '}' => { depth -= 1; cur.push(ch); }
            '|' if depth == 0 => { out.push(cur.trim().to_owned()); cur.clear(); }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_owned()); }
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
    if ty.is_empty() { return Vec::new(); }
    let parts = split_union(ty);
    if parts.len() > 1 {
        let mut out = Vec::new();
        let literals = parts.iter().filter(|p| p.starts_with(['"', '\''])).collect::<Vec<_>>();
        // `(string & {})` keeps a literal union open; the index sometimes cuts
        // the recorded text at its brace, so its prefix counts too.
        let is_open = |p: &str| p.replace(' ', "").starts_with("(string&");
        let open = parts.iter().any(|p| is_open(p));
        let rest = parts.iter().filter(|p| !p.starts_with(['"', '\'']) && !is_open(p)).collect::<Vec<_>>();
        if !literals.is_empty() {
            out.push(tok(TokKind::Word, "one of"));
            for (k, lit) in literals.iter().enumerate() {
                if k > 0 { out.push(tok(TokKind::Punct, "·")); }
                out.push(tok(TokKind::Lit, lit.replace('\'', "\"")));
            }
        }
        for part in rest {
            if !out.is_empty() { out.push(tok(TokKind::Word, "or")); }
            out.extend(ts_words(part));
        }
        if open { out.push(tok(TokKind::Word, "or any text")); }
        return out;
    }
    if ty.starts_with(['"', '\'']) { return vec![tok(TokKind::Lit, ty.replace('\'', "\""))]; }
    if let Some(inner) = ty.strip_suffix("[]") {
        let inner = inner.trim().trim_start_matches('(').trim_end_matches(')');
        let mut out = vec![tok(TokKind::Word, "list of")];
        out.extend(ts_words(inner));
        return out;
    }
    if let Some(p) = ts_prim(ty) { return vec![tok(TokKind::Prim, p)]; }
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
    if last.len() <= 6 && matches!(last, "T" | "U" | "K" | "V" | "Input" | "Output") { return vec![tok(TokKind::Var, last)]; }
    vec![named(last.to_owned())]
}

fn go_prim(name: &str) -> Option<&'static str> {
    Some(match name {
        "string" => "text",
        "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32" | "uint64" | "uintptr" | "rune" => "integer",
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
    if ty.is_empty() { return Vec::new(); }
    if let Some(inner) = ty.strip_prefix("[]") {
        let mut out = vec![tok(TokKind::Word, "list of")];
        out.extend(go_words(inner));
        return out;
    }
    if let Some(inner) = ty.strip_prefix('*') { return go_words(inner); }
    if let Some(rest) = ty.strip_prefix("map[") {
        let mut depth = 1_i32;
        let mut close = None;
        for (k, ch) in rest.char_indices() {
            match ch { '[' => depth += 1, ']' => { depth -= 1; if depth == 0 { close = Some(k); break; } } _ => {} }
        }
        if let Some(close) = close {
            let mut out = vec![tok(TokKind::Word, "map")];
            out.extend(go_words(&rest[..close]));
            out.push(tok(TokKind::Punct, "→"));
            out.extend(go_words(&rest[close + 1..]));
            return out;
        }
    }
    if ty.starts_with("func") { return vec![tok(TokKind::Word, "callback")]; }
    if let Some(p) = go_prim(ty) { return vec![tok(TokKind::Prim, p)]; }
    vec![named(ty.rsplit('.').next().unwrap_or(ty).to_owned())]
}

#[cfg(test)]
mod tests;
