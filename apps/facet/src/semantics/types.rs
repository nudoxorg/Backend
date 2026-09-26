//! Types spelled for people.
//!
//! A signature's types arrive as source text (the index stores them that way
//! today). [`parse`] turns Rust type syntax into a [`TypeExpr`], a small
//! language-neutral tree: a named type applied to arguments, a borrow, a
//! pointer, a list, a fixed array, a tuple, a function, "any type meeting
//! these bounds", an associated type, "never". The other front-ends (Python,
//! TypeScript, Go, Java, C#, C++) produce the same tree from their own
//! syntax; nothing below [`parse`] knows which language it came from except
//! the vocabulary table ([`Vocabulary`]).
//!
//! [`Scope::spell`] writes a tree in plain words: `maybe X`, `list of X`,
//! `set of X`, `map K → V`, `X or fails with E`, `shared X`, `locked X`,
//! `text`, `path`, `its Value`, generics as variables, and every other named
//! type as a link [`Target`]. The result keeps its exact source text so ⌥
//! can spell it beside the words.

use gpui::SharedString;
use std::collections::HashSet;
use std::fmt;

/// A node's index in the world (the same number as `graph::model::NodeId`).
pub type NodeId = u32;

// ------------------------------------------------------------------ the tree

/// A type expression, independent of the language it was written in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeExpr {
    /// A named type applied to arguments: `a::b::Name<A, B>`. `Self`, a
    /// primitive and a generic parameter are all `Named` until a [`Scope`]
    /// says otherwise.
    Named {
        /// Path segments as written.
        path: Vec<String>,
        /// Type arguments (lifetimes dropped).
        args: Vec<TypeExpr>,
    },
    /// An associated type bound in an argument list: `Item = T`.
    Binding {
        /// The associated name.
        name: String,
        /// Its value.
        ty: Box<TypeExpr>,
    },
    /// A type projected out of another: `Self::Value`, `D::Error`,
    /// `<T as Trait>::Output`.
    Assoc {
        /// What it is projected from.
        base: Box<TypeExpr>,
        /// The trait named in `<T as Trait>`, if any.
        via: Option<Box<TypeExpr>>,
        /// The associated name.
        name: String,
    },
    /// A borrow: `&T`, `&'a mut T`.
    Ref {
        /// `&mut`.
        mutable: bool,
        /// The borrowed type.
        inner: Box<TypeExpr>,
    },
    /// A raw pointer: `*const T`, `*mut T`.
    Ptr {
        /// `*mut`.
        mutable: bool,
        /// The pointee.
        inner: Box<TypeExpr>,
    },
    /// A list of unknown length: `[T]`.
    Slice(Box<TypeExpr>),
    /// A list of fixed length: `[T; N]`.
    Array {
        /// The element.
        inner: Box<TypeExpr>,
        /// The length, as written.
        len: String,
    },
    /// A tuple; the empty tuple is "nothing".
    Tuple(Vec<TypeExpr>),
    /// A function: `Fn(A) -> B`, `fn(A) -> B`, `FnMut(A)`.
    Func {
        /// Parameter types.
        params: Vec<TypeExpr>,
        /// The result (`None`: nothing).
        ret: Option<Box<TypeExpr>>,
    },
    /// Any type meeting every bound: `dyn A + B`, `impl A + B`.
    Any(Vec<TypeExpr>),
    /// Never returns: `!`.
    Never,
    /// Left to inference: `_`.
    Infer,
}

impl TypeExpr {
    /// A plain named type with no arguments.
    #[must_use]
    pub fn name(name: &str) -> Self {
        Self::Named {
            path: name.split("::").map(str::to_owned).collect(),
            args: Vec::new(),
        }
    }

    /// The last path segment of a named type.
    #[must_use]
    pub fn last(&self) -> Option<&str> {
        match self {
            Self::Named { path, .. } => path.last().map(String::as_str),
            _ => None,
        }
    }

    /// The type with every named type whose path matches `from` replaced by
    /// `to` (alias expansion).
    #[must_use]
    pub fn substitute(&self, from: &str, to: &TypeExpr) -> Self {
        let sub = |t: &TypeExpr| Box::new(t.substitute(from, to));
        match self {
            Self::Named { path, args } if path.len() == 1 && path[0] == from && args.is_empty() => to.clone(),
            Self::Named { path, args } => Self::Named {
                path: path.clone(),
                args: args.iter().map(|a| a.substitute(from, to)).collect(),
            },
            Self::Binding { name, ty } => Self::Binding { name: name.clone(), ty: sub(ty) },
            Self::Assoc { base, via, name } => Self::Assoc {
                base: sub(base),
                via: via.as_ref().map(|v| sub(v)),
                name: name.clone(),
            },
            Self::Ref { mutable, inner } => Self::Ref { mutable: *mutable, inner: sub(inner) },
            Self::Ptr { mutable, inner } => Self::Ptr { mutable: *mutable, inner: sub(inner) },
            Self::Slice(inner) => Self::Slice(sub(inner)),
            Self::Array { inner, len } => Self::Array { inner: sub(inner), len: len.clone() },
            Self::Tuple(items) => Self::Tuple(items.iter().map(|t| t.substitute(from, to)).collect()),
            Self::Func { params, ret } => Self::Func {
                params: params.iter().map(|t| t.substitute(from, to)).collect(),
                ret: ret.as_ref().map(|r| sub(r)),
            },
            Self::Any(bounds) => Self::Any(bounds.iter().map(|t| t.substitute(from, to)).collect()),
            Self::Never | Self::Infer => self.clone(),
        }
    }
}

/// Prints the tree back as Rust source (lifetimes dropped).
impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn list(f: &mut fmt::Formatter<'_>, items: &[TypeExpr], sep: &str) -> fmt::Result {
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    f.write_str(sep)?;
                }
                write!(f, "{item}")?;
            }
            Ok(())
        }
        match self {
            Self::Named { path, args } => {
                f.write_str(&path.join("::"))?;
                if !args.is_empty() {
                    f.write_str("<")?;
                    list(f, args, ", ")?;
                    f.write_str(">")?;
                }
                Ok(())
            }
            Self::Binding { name, ty } => write!(f, "{name} = {ty}"),
            Self::Assoc { base, via: Some(via), name } => write!(f, "<{base} as {via}>::{name}"),
            Self::Assoc { base, via: None, name } => write!(f, "{base}::{name}"),
            Self::Ref { mutable, inner } => write!(f, "&{}{inner}", if *mutable { "mut " } else { "" }),
            Self::Ptr { mutable, inner } => write!(f, "*{} {inner}", if *mutable { "mut" } else { "const" }),
            Self::Slice(inner) => write!(f, "[{inner}]"),
            Self::Array { inner, len } => write!(f, "[{inner}; {len}]"),
            Self::Tuple(items) => {
                f.write_str("(")?;
                list(f, items, ", ")?;
                if items.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }
            Self::Func { params, ret } => {
                f.write_str("fn(")?;
                list(f, params, ", ")?;
                f.write_str(")")?;
                if let Some(ret) = ret {
                    write!(f, " -> {ret}")?;
                }
                Ok(())
            }
            Self::Any(bounds) => {
                f.write_str("impl ")?;
                list(f, bounds, " + ")
            }
            Self::Never => f.write_str("!"),
            Self::Infer => f.write_str("_"),
        }
    }
}

// ------------------------------------------------------------------ lexing

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Lifetime,
    Number(String),
    Arrow,
    Path,
    Punct(char),
}

fn lex(text: &str) -> Vec<Tok> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut k = 0;
    let word = |c: char| c.is_alphanumeric() || c == '_';
    while k < chars.len() {
        let c = chars[k];
        if c.is_whitespace() {
            k += 1;
        } else if c == '\'' {
            k += 1;
            while k < chars.len() && word(chars[k]) {
                k += 1;
            }
            out.push(Tok::Lifetime);
        } else if c == '"' {
            // A string literal (`extern "C"`): skipped.
            k += 1;
            while k < chars.len() && chars[k] != '"' {
                k += 1;
            }
            k += 1;
        } else if c.is_ascii_digit() {
            let start = k;
            while k < chars.len() && word(chars[k]) {
                k += 1;
            }
            out.push(Tok::Number(chars[start..k].iter().collect()));
        } else if word(c) {
            let start = k;
            while k < chars.len() && word(chars[k]) {
                k += 1;
            }
            out.push(Tok::Ident(chars[start..k].iter().collect()));
        } else if c == '-' && chars.get(k + 1) == Some(&'>') {
            out.push(Tok::Arrow);
            k += 2;
        } else if c == ':' && chars.get(k + 1) == Some(&':') {
            out.push(Tok::Path);
            k += 2;
        } else {
            out.push(Tok::Punct(c));
            k += 1;
        }
    }
    out
}

// ------------------------------------------------------------------ parsing

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn peek_at(&self, ahead: usize) -> Option<&Tok> {
        self.toks.get(self.at + ahead)
    }

    fn is(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Punct(c))
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == w)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.is(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn done(&self) -> bool {
        self.at >= self.toks.len()
    }

    /// Skips a balanced `<…>` (a `for<'a>` binder).
    fn skip_angles(&mut self) {
        if !self.eat('<') {
            return;
        }
        let mut depth = 1;
        while let Some(tok) = self.peek().cloned() {
            self.at += 1;
            match tok {
                Tok::Punct('<') => depth += 1,
                Tok::Punct('>') => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn ty(&mut self) -> TypeExpr {
        let Some(tok) = self.peek().cloned() else {
            return TypeExpr::Tuple(Vec::new());
        };
        self.at += 1;
        match tok {
            Tok::Punct('&') => {
                while matches!(self.peek(), Some(Tok::Lifetime)) {
                    self.at += 1;
                }
                let mutable = self.eat_word("mut");
                TypeExpr::Ref { mutable, inner: Box::new(self.ty()) }
            }
            Tok::Punct('*') => {
                let mutable = self.eat_word("mut");
                if !mutable {
                    self.eat_word("const");
                }
                TypeExpr::Ptr { mutable, inner: Box::new(self.ty()) }
            }
            Tok::Punct('(') => {
                let (items, trailing) = self.list(')');
                if items.len() == 1 && !trailing {
                    items.into_iter().next().unwrap_or(TypeExpr::Tuple(Vec::new()))
                } else {
                    TypeExpr::Tuple(items)
                }
            }
            Tok::Punct('[') => {
                let inner = self.ty();
                if self.eat(';') {
                    let mut len = String::new();
                    while let Some(tok) = self.peek().cloned() {
                        if tok == Tok::Punct(']') {
                            break;
                        }
                        self.at += 1;
                        match tok {
                            Tok::Ident(s) | Tok::Number(s) => len.push_str(&s),
                            Tok::Path => len.push_str("::"),
                            Tok::Punct(c) => len.push(c),
                            Tok::Arrow => len.push_str("->"),
                            Tok::Lifetime => {}
                        }
                    }
                    self.eat(']');
                    TypeExpr::Array { inner: Box::new(inner), len }
                } else {
                    self.eat(']');
                    TypeExpr::Slice(Box::new(inner))
                }
            }
            Tok::Punct('!') => TypeExpr::Never,
            Tok::Punct('<') => {
                // A qualified path: `<T as Trait>::Name`.
                let base = self.ty();
                let via = if self.eat_word("as") { Some(Box::new(self.ty())) } else { None };
                self.eat('>');
                let mut expr = base;
                let mut via = via;
                while self.peek() == Some(&Tok::Path) {
                    self.at += 1;
                    let Some(Tok::Ident(name)) = self.peek().cloned() else { break };
                    self.at += 1;
                    expr = TypeExpr::Assoc { base: Box::new(expr), via: via.take(), name };
                }
                expr
            }
            Tok::Lifetime => self.ty(),
            Tok::Number(n) => TypeExpr::Named { path: vec![n], args: Vec::new() },
            Tok::Ident(word) => match word.as_str() {
                "dyn" | "impl" => TypeExpr::Any(self.bounds()),
                "mut" | "const" | "unsafe" | "extern" => self.ty(),
                "for" if self.is('<') => {
                    self.skip_angles();
                    self.ty()
                }
                "fn" if self.is('(') => {
                    self.at += 1;
                    self.func()
                }
                "_" => TypeExpr::Infer,
                _ => self.path(word),
            },
            Tok::Arrow | Tok::Path | Tok::Punct(_) => self.ty(),
        }
    }

    /// `(params) [-> ret]` after the opening parenthesis.
    fn func(&mut self) -> TypeExpr {
        let (params, _) = self.list(')');
        let ret = if self.peek() == Some(&Tok::Arrow) {
            self.at += 1;
            Some(Box::new(self.ty()))
        } else {
            None
        };
        TypeExpr::Func { params, ret }
    }

    /// `+`-separated bounds (lifetimes and `?Sized` dropped).
    fn bounds(&mut self) -> Vec<TypeExpr> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Some(Tok::Lifetime) => self.at += 1,
                Some(Tok::Punct('?')) => {
                    self.at += 1;
                    let _ = self.ty();
                }
                Some(Tok::Ident(w)) if w == "for" => {
                    self.at += 1;
                    self.skip_angles();
                    out.push(self.ty());
                }
                Some(_) => out.push(self.ty()),
                None => break,
            }
            if !self.eat('+') {
                break;
            }
        }
        out
    }

    /// Items until `close`; tolerant of junk. Returns whether a trailing
    /// comma closed the list.
    fn list(&mut self, close: char) -> (Vec<TypeExpr>, bool) {
        let mut out = Vec::new();
        let mut trailing = false;
        while !self.done() && !self.is(close) {
            trailing = false;
            out.push(self.ty());
            if self.eat(',') {
                trailing = true;
            } else if !self.is(close) {
                while !self.done() && !self.is(close) && !self.is(',') {
                    self.at += 1;
                }
                if self.eat(',') {
                    trailing = true;
                }
            }
        }
        self.eat(close);
        (out, trailing)
    }

    /// `<…>` generic arguments (lifetimes dropped, `Name = T` kept as a binding).
    fn args(&mut self) -> Vec<TypeExpr> {
        let mut out = Vec::new();
        if !self.eat('<') {
            return out;
        }
        while !self.done() && !self.is('>') {
            if matches!(self.peek(), Some(Tok::Lifetime)) {
                self.at += 1;
                self.eat(',');
                continue;
            }
            // `Name = T` or `Name: Bound`
            if let (Some(Tok::Ident(name)), Some(Tok::Punct('='))) = (self.peek().cloned(), self.peek_at(1)) {
                self.at += 2;
                out.push(TypeExpr::Binding { name, ty: Box::new(self.ty()) });
            } else if let (Some(Tok::Ident(_)), Some(Tok::Punct(':'))) = (self.peek(), self.peek_at(1)) {
                self.at += 2;
                let _ = self.bounds();
            } else {
                out.push(self.ty());
            }
            if !self.eat(',') && !self.is('>') {
                // Junk: skip to the next separator.
                while !self.done() && !self.is('>') && !self.is(',') {
                    self.at += 1;
                }
                self.eat(',');
            }
        }
        self.eat('>');
        out
    }

    fn path(&mut self, first: String) -> TypeExpr {
        let mut path = vec![first];
        let mut args = Vec::new();
        loop {
            if self.is('<') {
                args = self.args();
            }
            if self.peek() == Some(&Tok::Path) {
                match self.peek_at(1).cloned() {
                    Some(Tok::Punct('<')) => {
                        // A turbofish.
                        self.at += 1;
                        continue;
                    }
                    Some(Tok::Ident(next)) => {
                        self.at += 2;
                        if args.is_empty() {
                            path.push(next);
                        } else {
                            // `Foo<T>::Bar`: a projection out of an applied type.
                            let base = TypeExpr::Named { path: std::mem::take(&mut path), args: std::mem::take(&mut args) };
                            return self.project(base, next);
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            break;
        }
        let last = path.last().map(String::as_str).unwrap_or_default();
        if matches!(last, "Fn" | "FnMut" | "FnOnce") && self.is('(') {
            self.at += 1;
            return self.func();
        }
        if path.len() > 1 && path[0] == "Self" {
            let name = path[1..].join("::");
            return TypeExpr::Assoc { base: Box::new(TypeExpr::name("Self")), via: None, name };
        }
        TypeExpr::Named { path, args }
    }

    fn project(&mut self, base: TypeExpr, name: String) -> TypeExpr {
        let mut expr = TypeExpr::Assoc { base: Box::new(base), via: None, name };
        while self.peek() == Some(&Tok::Path) {
            let Some(Tok::Ident(next)) = self.peek_at(1).cloned() else { break };
            self.at += 2;
            expr = TypeExpr::Assoc { base: Box::new(expr), via: None, name: next };
        }
        expr
    }
}

/// Parses one Rust type. Never fails: unreadable tokens are skipped and an
/// empty text is the empty tuple.
#[must_use]
pub fn parse(text: &str) -> TypeExpr {
    Parser { toks: lex(text), at: 0 }.ty()
}

/// Parses a comma-separated list of types (a tuple variant's payload).
#[must_use]
pub fn parse_list(text: &str) -> Vec<TypeExpr> {
    let mut parser = Parser { toks: lex(text), at: 0 };
    let mut out = Vec::new();
    while !parser.done() {
        out.push(parser.ty());
        if !parser.eat(',') {
            while !parser.done() && !parser.is(',') {
                parser.at += 1;
            }
            parser.eat(',');
        }
    }
    out
}

/// Splits `text` at top-level occurrences of `sep` (outside `<>`, `()` and
/// `[]`), trimming each part and dropping empty ones.
#[must_use]
pub fn split_top(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(at, c)) in bytes.iter().enumerate() {
        match c {
            '<' | '(' | '[' => depth += 1,
            // `->` is not a closing angle.
            '>' if k > 0 && bytes[k - 1].1 == '-' => {}
            '>' | ')' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(text[start..at].trim().to_owned());
                start = at + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(text[start..].trim().to_owned());
    out.retain(|s| !s.is_empty());
    out
}

/// A record payload `name: Type, other: Type` as `(name, type)` pairs; a
/// part without a name is a positional field.
#[must_use]
pub fn parse_fields(text: &str) -> Vec<(Option<String>, TypeExpr)> {
    split_top(text, ',')
        .into_iter()
        .map(|part| match name_colon(&part) {
            Some(colon) => (
                Some(part[..colon].trim().trim_start_matches("pub ").trim().to_owned()),
                parse(&part[colon + 1..]),
            ),
            None => (None, parse(&part)),
        })
        .collect()
}

/// The index of the first `:` that is not part of `::`.
#[must_use]
pub fn name_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find(|&k| {
        bytes[k] == b':' && bytes.get(k + 1) != Some(&b':') && (k == 0 || bytes[k - 1] != b':')
    })
}

// ------------------------------------------------------------------ spelling

/// Where a named type leads.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A symbol in the world.
    Node(NodeId),
    /// A path the world does not hold (the shell asks the index).
    Path(SharedString),
}

/// One piece of a type spelled in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    /// Plain words in the UI face: `maybe`, `list of`, `or fails with`.
    Word(SharedString),
    /// Quiet punctuation: `×`, `→`, `‹`, `›`, `, `.
    Punct(SharedString),
    /// A generic variable: italic, never a link.
    Var(SharedString),
    /// An associated type's name (after `its`): italic.
    Assoc(SharedString),
    /// A named type: a link.
    Name {
        /// The name as shown (the last path segment).
        text: SharedString,
        /// Where it leads.
        target: Target,
    },
    /// A primitive (`bool`, `u8`) or a name that is not a type: quiet mono.
    Prim(SharedString),
    /// One space.
    Space,
}

impl Piece {
    /// The piece's text.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Word(s) | Self::Punct(s) | Self::Var(s) | Self::Assoc(s) | Self::Prim(s) => s,
            Self::Name { text, .. } => text,
            Self::Space => " ",
        }
    }
}

/// A type in plain words, with its exact source for ⌥.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Spelled {
    /// The words.
    pub pieces: Vec<Piece>,
    /// The type as written.
    pub source: SharedString,
}

impl Spelled {
    /// The words as one string.
    #[must_use]
    pub fn plain(&self) -> String {
        self.pieces.iter().map(Piece::text).collect()
    }

    /// Every link target, in order.
    #[must_use]
    pub fn targets(&self) -> Vec<&Target> {
        self.pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Name { target, .. } => Some(target),
                _ => None,
            })
            .collect()
    }

    /// Words only (no source), e.g. `nothing`.
    #[must_use]
    pub fn words(words: &str) -> Self {
        Self { pieces: vec![Piece::Word(SharedString::from(words.to_owned()))], source: SharedString::default() }
    }
}

/// What the plain-word vocabulary makes of a named type's last segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Meaning {
    /// `maybe X`.
    Maybe,
    /// `list of X`.
    List,
    /// `set of X`.
    Set,
    /// `map K → V`.
    Map,
    /// `X or fails with E`.
    Fallible,
    /// Transparent: spelled as its argument (`Box<T>` is `T`).
    Through,
    /// `shared X`.
    Shared,
    /// `changeable X`.
    Changeable,
    /// `locked X`.
    Locked,
    /// `text`.
    Text,
    /// `path`.
    PathWord,
    /// `a marker for X` (zero-sized type markers).
    Marker,
    /// A primitive: quiet, never a link.
    Primitive,
}

/// A language's plain-word vocabulary: what its standard names mean.
pub trait Vocabulary {
    /// The meaning of a named type, given its full path (as written).
    fn meaning(&self, path: &[String]) -> Option<Meaning>;
}

/// Rust's standard names.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rust;

impl Vocabulary for Rust {
    fn meaning(&self, path: &[String]) -> Option<Meaning> {
        let last = path.last()?.as_str();
        Some(match last {
            "Option" => Meaning::Maybe,
            "Vec" | "VecDeque" | "SmallVec" | "LinkedList" | "BinaryHeap" => Meaning::List,
            "HashSet" | "BTreeSet" | "IndexSet" => Meaning::Set,
            "HashMap" | "BTreeMap" | "IndexMap" => Meaning::Map,
            "Result" => Meaning::Fallible,
            "Box" | "Cow" | "Pin" => Meaning::Through,
            "Rc" | "Arc" => Meaning::Shared,
            "RefCell" | "Cell" => Meaning::Changeable,
            "Mutex" | "RwLock" => Meaning::Locked,
            "String" | "str" | "OsStr" | "OsString" | "CStr" | "CString" => Meaning::Text,
            "PathBuf" | "Path" => Meaning::PathWord,
            "PhantomData" => Meaning::Marker,
            "bool" | "char" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32"
            | "i64" | "i128" | "isize" | "f32" | "f64" => Meaning::Primitive,
            _ => return None,
        })
    }
}

/// Resolves names against a world.
pub trait Resolve {
    /// Where a named type (full path as written) leads, if the world holds it.
    fn named(&self, path: &[String]) -> Option<Target>;

    /// The expansion of a type alias with `arity` arguments at `path`
    /// (`serde_json::Result<T>` is `Result<T, Error>`), with its parameter
    /// names. The expansion's names resolve where the alias lives.
    fn alias(&self, path: &[String], arity: usize) -> Option<(Vec<String>, TypeExpr)> {
        let _ = (path, arity);
        None
    }
}

/// Resolves nothing: every named type becomes a [`Target::Path`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Nowhere;

impl Resolve for Nowhere {
    fn named(&self, _path: &[String]) -> Option<Target> {
        None
    }
}

/// Everything spelling needs to know about where a type was written.
pub struct Scope<'a> {
    /// Generic parameters in scope (never links).
    pub generics: HashSet<String>,
    /// What `Self` names: its target and name.
    pub owner: Option<(Target, SharedString)>,
    /// Name resolution.
    pub resolve: &'a dyn Resolve,
    /// The language's standard names.
    pub vocabulary: &'a dyn Vocabulary,
}

/// Standard library aliases of `Result` whose error is fixed: `fmt::Result`
/// is `Result<(), fmt::Error>`, `io::Result<T>` is `Result<T, io::Error>`.
fn std_alias(path: &[String], args: &[TypeExpr]) -> Option<(TypeExpr, TypeExpr)> {
    if path.len() < 2 || path.last()? != "Result" {
        return None;
    }
    let module = path[path.len() - 2].as_str();
    let err = TypeExpr::Named { path: vec![module.to_owned(), "Error".to_owned()], args: Vec::new() };
    match (module, args) {
        ("fmt", []) => Some((TypeExpr::Tuple(Vec::new()), err)),
        ("io", [ok]) => Some((ok.clone(), err)),
        _ => None,
    }
}

impl<'a> Scope<'a> {
    /// A scope with no generics and no owner.
    #[must_use]
    pub fn new(resolve: &'a dyn Resolve) -> Self {
        Self { generics: HashSet::new(), owner: None, resolve, vocabulary: &Rust }
    }

    /// Adds generic parameters.
    #[must_use]
    pub fn generics<I: IntoIterator<Item = S>, S: Into<String>>(mut self, names: I) -> Self {
        self.generics.extend(names.into_iter().map(Into::into));
        self
    }

    /// Names what `Self` is.
    #[must_use]
    pub fn owner(mut self, target: Target, name: impl Into<SharedString>) -> Self {
        self.owner = Some((target, name.into()));
        self
    }

    /// Whether a bare name is a generic variable here.
    #[must_use]
    pub fn is_var(&self, name: &str) -> bool {
        self.generics.contains(name)
            || (name.len() == 1 && name.chars().all(|c| c.is_ascii_uppercase()))
    }

    /// Parses and spells `source`.
    #[must_use]
    pub fn spell_text(&self, source: &str) -> Spelled {
        let expr = parse(source);
        Spelled { pieces: self.pieces(&expr), source: SharedString::from(source.trim().to_owned()) }
    }

    /// Spells a tree (its source is the tree printed back).
    #[must_use]
    pub fn spell(&self, expr: &TypeExpr) -> Spelled {
        Spelled { pieces: self.pieces(expr), source: SharedString::from(expr.to_string()) }
    }

    /// If `expr` is fallible (`Result<T, E>`, an alias of one, `fmt::Result`),
    /// its success and failure types.
    #[must_use]
    pub fn fallible(&self, expr: &TypeExpr) -> Option<(TypeExpr, Option<TypeExpr>)> {
        let TypeExpr::Named { path, args } = expr else { return None };
        if self.vocabulary.meaning(path) != Some(Meaning::Fallible) {
            return None;
        }
        if args.len() >= 2 {
            return Some((args[0].clone(), Some(args[1].clone())));
        }
        if let Some((ok, err)) = std_alias(path, args) {
            return Some((ok, Some(err)));
        }
        if let Some((params, body)) = self.resolve.alias(path, args.len()) {
            let mut body = body;
            for (param, arg) in params.iter().zip(args) {
                body = body.substitute(param, arg);
            }
            if let TypeExpr::Named { path: inner, args: full } = &body
                && self.vocabulary.meaning(inner) == Some(Meaning::Fallible)
                && full.len() >= 2
            {
                return Some((full[0].clone(), Some(full[1].clone())));
            }
        }
        Some((args.first().cloned().unwrap_or(TypeExpr::Tuple(Vec::new())), None))
    }

    fn word(out: &mut Vec<Piece>, w: &str) {
        out.push(Piece::Word(SharedString::from(w.to_owned())));
    }

    fn punct(out: &mut Vec<Piece>, p: &'static str) {
        out.push(Piece::Punct(SharedString::new_static(p)));
    }

    /// The plain words for a tree.
    #[must_use]
    pub fn pieces(&self, expr: &TypeExpr) -> Vec<Piece> {
        let mut out = Vec::new();
        self.put(expr, &mut out);
        out
    }

    fn put(&self, expr: &TypeExpr, out: &mut Vec<Piece>) {
        match expr {
            TypeExpr::Ref { mutable, inner } => {
                if *mutable {
                    Self::word(out, "mutable");
                    out.push(Piece::Space);
                }
                self.put(inner, out);
            }
            TypeExpr::Ptr { inner, .. } => {
                Self::word(out, "pointer to");
                out.push(Piece::Space);
                self.put(inner, out);
            }
            TypeExpr::Tuple(items) if items.is_empty() => Self::word(out, "nothing"),
            TypeExpr::Tuple(items) => {
                for (k, item) in items.iter().enumerate() {
                    if k > 0 {
                        out.push(Piece::Space);
                        Self::punct(out, "×");
                        out.push(Piece::Space);
                    }
                    self.put(item, out);
                }
            }
            TypeExpr::Slice(inner) => {
                Self::word(out, "list of");
                out.push(Piece::Space);
                self.put(inner, out);
            }
            TypeExpr::Array { inner, len } => {
                Self::word(out, &format!("{len} ×"));
                out.push(Piece::Space);
                self.put(inner, out);
            }
            TypeExpr::Never => Self::word(out, "never returns"),
            TypeExpr::Infer => out.push(Piece::Prim(SharedString::new_static("_"))),
            TypeExpr::Binding { ty, .. } => self.put(ty, out),
            TypeExpr::Any(bounds) => {
                // Auto traits say nothing a reader needs beside a real bound
                // (⌥ still spells them).
                let auto = |b: &TypeExpr| matches!(b.last(), Some("Send" | "Sync" | "Unpin"));
                let kept: Vec<&TypeExpr> = if bounds.iter().any(|b| !auto(b)) {
                    bounds.iter().filter(|b| !auto(b)).collect()
                } else {
                    bounds.iter().collect()
                };
                // A lone function bound reads as the function itself.
                if let [only @ TypeExpr::Func { .. }] = kept.as_slice() {
                    self.put(only, out);
                    return;
                }
                Self::word(out, "any");
                out.push(Piece::Space);
                if kept.is_empty() {
                    Self::word(out, "type");
                }
                for (k, bound) in kept.iter().enumerate() {
                    if k > 0 {
                        out.push(Piece::Space);
                        Self::word(out, "and");
                        out.push(Piece::Space);
                    }
                    self.put(bound, out);
                }
            }
            TypeExpr::Func { params, ret } => {
                Self::word(out, "a function of");
                out.push(Piece::Space);
                if params.is_empty() {
                    Self::word(out, "nothing");
                }
                for (k, param) in params.iter().enumerate() {
                    if k > 0 {
                        Self::punct(out, ", ");
                    }
                    self.put(param, out);
                }
                if let Some(ret) = ret {
                    out.push(Piece::Space);
                    Self::punct(out, "→");
                    out.push(Piece::Space);
                    self.put(ret, out);
                }
            }
            TypeExpr::Assoc { base, name, .. } => self.assoc(base, name, out),
            TypeExpr::Named { path, args } => self.named(path, args, out),
        }
    }

    fn assoc(&self, base: &TypeExpr, name: &str, out: &mut Vec<Piece>) {
        let shown = SharedString::from(name.rsplit("::").next().unwrap_or(name).to_owned());
        if matches!(base, TypeExpr::Named { path, args } if args.is_empty() && path.len() == 1 && path[0] == "Self") {
            Self::word(out, "its");
            out.push(Piece::Space);
            out.push(Piece::Assoc(shown));
            return;
        }
        self.put(base, out);
        Self::word(out, "’s");
        out.push(Piece::Space);
        out.push(Piece::Assoc(shown));
    }

    fn named(&self, path: &[String], args: &[TypeExpr], out: &mut Vec<Piece>) {
        let Some(last) = path.last() else { return };
        // A generic parameter, or a projection out of one (`D::Error`).
        if self.is_var(&path[0]) && !self.owner_named(&path[0]) {
            out.push(Piece::Var(SharedString::from(path[0].clone())));
            if path.len() > 1 {
                Self::word(out, "’s");
                out.push(Piece::Space);
                out.push(Piece::Assoc(SharedString::from(path[1..].join("::"))));
            }
            return;
        }
        match last.as_str() {
            "Self" if path.len() == 1 => {
                match &self.owner {
                    Some((target, name)) => out.push(Piece::Name { text: name.clone(), target: target.clone() }),
                    None => out.push(Piece::Prim(SharedString::new_static("Self"))),
                }
                return;
            }
            "self" if path.len() == 1 => {
                Self::word(out, "it");
                return;
            }
            _ => {}
        }
        let arg = |k: usize, out: &mut Vec<Piece>| match args.get(k) {
            Some(a) => self.put(a, out),
            None => Self::word(out, "anything"),
        };
        match self.vocabulary.meaning(path) {
            Some(Meaning::Maybe) => {
                Self::word(out, "maybe");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::List) => {
                Self::word(out, "list of");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::Set) => {
                Self::word(out, "set of");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::Map) => {
                Self::word(out, "map");
                out.push(Piece::Space);
                arg(0, out);
                out.push(Piece::Space);
                Self::punct(out, "→");
                out.push(Piece::Space);
                arg(1, out);
            }
            Some(Meaning::Fallible) => {
                let expr = TypeExpr::Named { path: path.to_vec(), args: args.to_vec() };
                let (ok, err) = self.fallible(&expr).unwrap_or((TypeExpr::Tuple(Vec::new()), None));
                self.put(&ok, out);
                out.push(Piece::Space);
                Self::word(out, "or fails with");
                out.push(Piece::Space);
                match err {
                    Some(err) => self.put(&err, out),
                    None => Self::word(out, "an error"),
                }
            }
            Some(Meaning::Through) => match args.iter().find(|a| !matches!(a, TypeExpr::Binding { .. })) {
                Some(a) => self.put(a, out),
                None => self.link(path, out),
            },
            Some(Meaning::Shared) => {
                Self::word(out, "shared");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::Changeable) => {
                Self::word(out, "changeable");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::Locked) => {
                Self::word(out, "locked");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::Text) => Self::word(out, "text"),
            Some(Meaning::PathWord) => Self::word(out, "path"),
            Some(Meaning::Marker) => {
                Self::word(out, "a marker for");
                out.push(Piece::Space);
                arg(0, out);
            }
            Some(Meaning::Primitive) => out.push(Piece::Prim(SharedString::from(last.clone()))),
            None if last.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) => {
                out.push(Piece::Prim(SharedString::from(last.clone())));
            }
            None => {
                self.link(path, out);
                if !args.is_empty() {
                    Self::punct(out, "‹");
                    for (k, a) in args.iter().enumerate() {
                        if k > 0 {
                            Self::punct(out, ", ");
                        }
                        self.put(a, out);
                    }
                    Self::punct(out, "›");
                }
            }
        }
    }

    fn owner_named(&self, name: &str) -> bool {
        self.owner.as_ref().is_some_and(|(_, owner)| owner.as_ref() == name)
    }

    fn link(&self, path: &[String], out: &mut Vec<Piece>) {
        let text = SharedString::from(path.last().cloned().unwrap_or_default());
        let target = self
            .resolve
            .named(path)
            .unwrap_or_else(|| Target::Path(SharedString::from(path.join("::"))));
        out.push(Piece::Name { text, target });
    }
}
