//! Types in plain words, for every language the page reads.
//!
//! One tolerant parser reads Rust (`Vec<T>`, `&'a str`, `impl Iterator<Item
//! = T>`), TypeScript (`string[]`, `Promise<T>`, `A | B`, object literals)
//! and Python (`list[str]`, `Optional[X]`, `X | None`) into one tree; one
//! table turns the tree into the words the page says first ("text", "a list
//! of Value", "a map of text to Value", "a count"). What is written follows,
//! quieter, and only when it says more than the word.

use super::super::view::{Origin, Ty};
use super::text::{last_segment, squash};

/// A type expression, whatever language wrote it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum T {
    /// A named type applied to arguments.
    Name { path: Vec<String>, args: Vec<T> },
    /// A borrow.
    Ref { mutable: bool, inner: Box<T> },
    /// A list of unknown or fixed length.
    Slice(Box<T>),
    /// A tuple; the empty one is nothing.
    Tuple(Vec<T>),
    /// A function.
    Func { params: Vec<T>, ret: Option<Box<T>> },
    /// Any type meeting these bounds.
    Any(Vec<T>),
    /// One of several.
    Union(Vec<T>),
    /// A literal (`"email"`, `3`).
    Lit(String),
    /// An object type: `(name, type, optional)`.
    Object(Vec<(String, T, bool)>),
    /// Not said, or not readable.
    Infer,
}

impl T {
    fn named(name: &str) -> Self {
        Self::Name { path: vec![name.to_owned()], args: Vec::new() }
    }

    /// The last path segment of a named type.
    pub(super) fn last(&self) -> Option<&str> {
        match self {
            Self::Name { path, .. } => path.last().map(String::as_str),
            Self::Ref { inner, .. } => inner.last(),
            _ => None,
        }
    }

    /// Whether `name` appears anywhere in the type.
    pub(super) fn mentions(&self, name: &str) -> bool {
        match self {
            Self::Name { path, args } => path.first().is_some_and(|first| first == name) || path.last().is_some_and(|last| last == name) || args.iter().any(|arg| arg.mentions(name)),
            Self::Ref { inner, .. } | Self::Slice(inner) => inner.mentions(name),
            Self::Tuple(items) | Self::Any(items) | Self::Union(items) => items.iter().any(|item| item.mentions(name)),
            Self::Func { params, ret } => params.iter().any(|p| p.mentions(name)) || ret.as_ref().is_some_and(|r| r.mentions(name)),
            Self::Object(fields) => fields.iter().any(|(_, ty, _)| ty.mentions(name)),
            Self::Lit(_) | Self::Infer => false,
        }
    }
}

/// How a language spells its types back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Spelling {
    /// `&str`, `Vec<T>`, `[T]`, `impl A + B`.
    Rust,
    /// `string`, `T[]`, `A | B`.
    Script,
    /// `str`, `list[T]`, `A | B`.
    Python,
    /// `string`, `[]T`, `map[K]V`.
    Go,
}

impl From<super::super::view::Lang> for Spelling {
    fn from(lang: super::super::view::Lang) -> Self {
        use super::super::view::Lang;
        match lang {
            Lang::Python => Self::Python,
            Lang::Go => Self::Go,
            Lang::JavaScript | Lang::TypeScript => Self::Script,
            _ => Self::Rust,
        }
    }
}

impl T {
    /// The type written back the way `spelling` writes it: the quiet text
    /// beside its plain words. Lifetimes and paths are not kept.
    pub(super) fn spelled(&self, spelling: Spelling) -> String {
        let list = |items: &[Self]| items.iter().map(|item| item.spelled(spelling)).collect::<Vec<_>>().join(", ");
        match self {
            Self::Name { path, args } => {
                let head = path.join(if spelling == Spelling::Rust { "::" } else { "." });
                match (args.is_empty(), spelling) {
                    (true, _) => head,
                    (false, Spelling::Python | Spelling::Go) => format!("{head}[{}]", list(args)),
                    (false, _) => format!("{head}<{}>", list(args)),
                }
            }
            Self::Ref { mutable, inner } => match (spelling, mutable) {
                (Spelling::Rust, true) => format!("&mut {}", inner.spelled(spelling)),
                (Spelling::Rust, false) => format!("&{}", inner.spelled(spelling)),
                _ => inner.spelled(spelling),
            },
            Self::Slice(inner) => match spelling {
                Spelling::Rust => format!("[{}]", inner.spelled(spelling)),
                Spelling::Script => format!("{}[]", inner.spelled(spelling)),
                Spelling::Python => format!("list[{}]", inner.spelled(spelling)),
                Spelling::Go => format!("[]{}", inner.spelled(spelling)),
            },
            Self::Tuple(items) => format!("({})", list(items)),
            Self::Func { params, ret } => {
                let ret = ret.as_ref().map(|r| r.spelled(spelling));
                match (spelling, ret) {
                    (Spelling::Rust, Some(ret)) => format!("fn({}) -> {ret}", list(params)),
                    (Spelling::Rust, None) => format!("fn({})", list(params)),
                    (_, Some(ret)) => format!("({}) => {ret}", list(params)),
                    (_, None) => format!("({}) => void", list(params)),
                }
            }
            Self::Any(bounds) => {
                let joined = bounds.iter().map(|b| b.spelled(spelling)).collect::<Vec<_>>().join(" + ");
                if spelling == Spelling::Rust { format!("impl {joined}") } else { joined }
            }
            Self::Union(items) => items.iter().map(|item| item.spelled(spelling)).collect::<Vec<_>>().join(" | "),
            Self::Lit(text) => text.clone(),
            Self::Object(fields) => {
                let inner = fields.iter().map(|(name, ty, optional)| format!("{name}{}: {}", if *optional { "?" } else { "" }, ty.spelled(spelling))).collect::<Vec<_>>().join(", ");
                format!("{{ {inner} }}")
            }
            Self::Infer => "_".to_owned(),
        }
    }
}

// ------------------------------------------------------------------ parsing

struct Parser {
    chars: Vec<char>,
    at: usize,
}

/// Parses a type written in Rust, TypeScript or Python syntax.
pub(super) fn parse(text: &str) -> T {
    let mut parser = Parser { chars: text.chars().collect(), at: 0 };
    let ty = parser.union();
    ty.unwrap_or(T::Infer)
}

impl Parser {
    fn skip(&mut self) {
        while self.chars.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip();
        self.chars.get(self.at).copied()
    }

    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> Option<String> {
        self.skip();
        let start = self.at;
        while self.chars.get(self.at).is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '$')) {
            self.at += 1;
        }
        (self.at > start).then(|| self.chars[start..self.at].iter().collect())
    }

    fn union(&mut self) -> Option<T> {
        self.eat('|');
        let first = self.postfix()?;
        let mut all = vec![first];
        while self.peek() == Some('|') {
            self.at += 1;
            all.push(self.postfix()?);
        }
        Some(if all.len() == 1 { all.remove(0) } else { T::Union(all) })
    }

    fn postfix(&mut self) -> Option<T> {
        let mut ty = self.atom()?;
        loop {
            match self.peek() {
                // `T[]`
                Some('[') if self.chars.get(self.at + 1) == Some(&']') => {
                    self.at += 2;
                    ty = T::Slice(Box::new(ty));
                }
                // TypeScript's optional marker on a member type.
                Some('?') => self.at += 1,
                _ => return Some(ty),
            }
        }
    }

    fn args(&mut self, close: char) -> Vec<T> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Some(c) if c == close => {
                    self.at += 1;
                    return out;
                }
                Some(',') => self.at += 1,
                Some('\'') => {
                    // A lifetime argument.
                    self.at += 1;
                    self.word();
                }
                Some('.') => self.at += 1,
                None => return out,
                _ => {
                    // `Item = T` binds an associated type: keep the value.
                    let before = self.at;
                    let named = self.word();
                    if named.is_some() && self.peek() == Some('=') && self.chars.get(self.at + 1) != Some(&'>') {
                        self.at += 1;
                    } else {
                        self.at = before;
                    }
                    match self.union() {
                        Some(ty) => out.push(ty),
                        None => {
                            self.at += 1;
                        }
                    }
                }
            }
        }
    }

    fn atom(&mut self) -> Option<T> {
        match self.peek()? {
            '&' => {
                self.at += 1;
                if self.peek() == Some('\'') {
                    self.at += 1;
                    self.word();
                }
                let before = self.at;
                let mutable = self.word().as_deref() == Some("mut");
                if !mutable {
                    self.at = before;
                }
                Some(T::Ref { mutable, inner: Box::new(self.postfix()?) })
            }
            '*' => {
                self.at += 1;
                self.word();
                self.postfix()
            }
            '(' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    match self.peek() {
                        Some(')') => {
                            self.at += 1;
                            break;
                        }
                        Some(',') => self.at += 1,
                        None => break,
                        _ => {
                            // `name: Type` in a function type keeps the type.
                            let before = self.at;
                            let named = self.word();
                            if named.is_some() && self.peek() == Some(':') {
                                self.at += 1;
                            } else {
                                self.at = before;
                            }
                            match self.union() {
                                Some(ty) => items.push(ty),
                                None => self.at += 1,
                            }
                        }
                    }
                }
                if self.peek() == Some('=') && self.chars.get(self.at + 1) == Some(&'>') {
                    self.at += 2;
                    let ret = self.union();
                    return Some(T::Func { params: items, ret: ret.map(Box::new) });
                }
                Some(T::Tuple(items))
            }
            '[' => {
                self.at += 1;
                // Go's `[]T`.
                if self.peek() == Some(']') {
                    self.at += 1;
                    return Some(T::Slice(Box::new(self.postfix()?)));
                }
                let mut items = self.args(']');
                if items.len() == 1 {
                    Some(T::Slice(Box::new(items.remove(0))))
                } else {
                    Some(T::Tuple(items))
                }
            }
            '{' => {
                self.at += 1;
                let mut fields = Vec::new();
                loop {
                    match self.peek() {
                        Some('}') => {
                            self.at += 1;
                            break;
                        }
                        Some(',' | ';') => self.at += 1,
                        None => break,
                        _ => {
                            let readonly = self.at;
                            let mut name = self.word();
                            if name.as_deref() == Some("readonly") {
                                name = self.word();
                            } else {
                                self.at = readonly;
                                name = self.word();
                            }
                            let Some(name) = name else {
                                self.at += 1;
                                continue;
                            };
                            let optional = self.eat('?');
                            let ty = if self.eat(':') { self.union().unwrap_or(T::Infer) } else { T::Infer };
                            fields.push((name, ty, optional));
                        }
                    }
                }
                Some(T::Object(fields))
            }
            quote @ ('"' | '\'' | '`') => {
                self.at += 1;
                let start = self.at;
                while self.chars.get(self.at).is_some_and(|c| *c != quote) {
                    self.at += 1;
                }
                let text: String = self.chars[start..self.at.min(self.chars.len())].iter().collect();
                self.at += 1;
                Some(T::Lit(format!("\"{text}\"")))
            }
            digit if digit.is_ascii_digit() => {
                let start = self.at;
                while self.chars.get(self.at).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '.') {
                    self.at += 1;
                }
                Some(T::Lit(self.chars[start..self.at].iter().collect()))
            }
            '!' => {
                self.at += 1;
                Some(T::named("never"))
            }
            '_' if !self.chars.get(self.at + 1).is_some_and(|c| c.is_alphanumeric() || *c == '_') => {
                self.at += 1;
                Some(T::Infer)
            }
            _ => self.named(),
        }
    }

    fn named(&mut self) -> Option<T> {
        let first = self.word()?;
        match first.as_str() {
            "impl" | "dyn" => {
                let mut bounds = Vec::new();
                loop {
                    if self.peek() == Some('\'') {
                        self.at += 1;
                        self.word();
                    } else if let Some(bound) = self.postfix() {
                        bounds.push(bound);
                    } else {
                        break;
                    }
                    if !self.eat('+') {
                        break;
                    }
                }
                return Some(T::Any(bounds));
            }
            "mut" | "const" | "readonly" | "unique" | "keyof" | "typeof" | "asserts" => return self.postfix(),
            "async" => return self.postfix(),
            _ => {}
        }
        let mut path = vec![first];
        while matches!(self.peek(), Some(':' | '.')) {
            let sep = self.chars[self.at];
            if sep == ':' && self.chars.get(self.at + 1) != Some(&':') {
                break;
            }
            if sep == '.' && self.chars.get(self.at + 1) == Some(&'.') {
                break;
            }
            self.at += if sep == ':' { 2 } else { 1 };
            match self.word() {
                Some(next) => path.push(next),
                None => break,
            }
        }
        let name = path.last().map_or("", String::as_str).to_owned();
        let mut args = Vec::new();
        // Go's `map[K]V`.
        if name == "map" && self.chars.get(self.at) == Some(&'[') {
            self.at += 1;
            let mut key = self.args(']');
            let value = self.postfix().unwrap_or(T::Infer);
            key.push(value);
            return Some(T::Name { path, args: key });
        }
        match self.chars.get(self.at).copied() {
            Some('<') => {
                self.at += 1;
                args = self.args('>');
            }
            Some('[') if self.chars.get(self.at + 1) != Some(&']') => {
                self.at += 1;
                args = self.args(']');
            }
            Some('(') if matches!(name.as_str(), "Fn" | "FnMut" | "FnOnce" | "fn" | "Callable" | "Function") => {
                self.at += 1;
                let params = self.args(')');
                let mut ret = None;
                if self.peek() == Some('-') && self.chars.get(self.at + 1) == Some(&'>') {
                    self.at += 2;
                    ret = self.postfix().map(Box::new);
                }
                return Some(T::Func { params, ret });
            }
            _ => {}
        }
        Some(T::Name { path, args })
    }
}

// ------------------------------------------------------------------ words

/// What the words are written against.
pub(super) struct Cx<'a> {
    /// The generic parameters of the declaration.
    pub generics: &'a [String],
    /// The type a method belongs to.
    pub owner: Option<&'a str>,
    /// The address a type name links to.
    pub link: &'a dyn Fn(&str) -> Option<String>,
    /// Where the spelling came from.
    pub origin: Origin,
}

/// A primitive's plain word.
fn primitive(name: &str) -> Option<&'static str> {
    Some(match name {
        "str" | "String" | "string" | "Cow" | "OsString" | "OsStr" | "AnyStr" => "text",
        "bool" | "boolean" | "Boolean" => "yes or no",
        "char" => "a character",
        "f32" | "f64" | "number" | "float" | "double" => "a number",
        "u8" => "a byte",
        "u16" | "u32" | "u64" | "u128" | "usize" | "uint" => "a count",
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "int" | "integer" => "an integer",
        "bigint" | "BigInt" => "a big integer",
        "Duration" | "timedelta" => "a duration",
        "Path" | "PathBuf" | "PurePath" => "a path",
        "any" | "unknown" | "Any" | "object" => "anything",
        "bytes" | "bytearray" | "Buffer" | "Uint8Array" => "bytes",
        "void" | "None" | "undefined" | "null" | "never" | "NoReturn" => "nothing",
        _ => return None,
    })
}

fn is_list(name: &str) -> bool {
    matches!(
        name,
        "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "IndexSet" | "SmallVec" | "ArrayVec" | "list" | "List" | "set" | "Set" | "frozenset" | "Array" | "ReadonlyArray" | "Sequence" | "tuple" | "Tuple" | "Iterable" | "Collection" | "Deque"
    )
}

fn is_map(name: &str) -> bool {
    matches!(name, "HashMap" | "BTreeMap" | "Map" | "map" | "IndexMap" | "dict" | "Dict" | "Record" | "Mapping" | "MutableMapping" | "OrderedDict" | "defaultdict")
}

fn is_wrapper(name: &str) -> bool {
    matches!(name, "Box" | "Rc" | "Arc" | "RefCell" | "Cell" | "Mutex" | "RwLock" | "Pin" | "Readonly" | "Partial" | "Required" | "Final" | "ClassVar" | "Annotated" | "NonNull")
}

fn is_maybe(name: &str) -> bool {
    matches!(name, "Option" | "Optional" | "Maybe")
}

/// What a wrapper type adds to what it holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Wrap {
    /// It may hold nothing.
    Maybe,
    /// It answers later.
    Later,
    /// It gives many.
    Many,
    /// It can fail.
    Fails,
}

/// The wrapper `name` is when it is applied to `arity` arguments. The names
/// a user's own type is likely to reuse (`Task`, `Stream`, `Iter`) count
/// only when they wrap something, so a plain `Task` stays a type.
fn wrapper(name: &str, arity: usize) -> Option<Wrap> {
    let wraps = arity > 0;
    match name {
        "Result" | "Fallible" => Some(Wrap::Fails),
        "Option" | "Optional" | "Maybe" if wraps => Some(Wrap::Maybe),
        "Promise" | "PromiseLike" | "Future" | "Awaitable" | "Coroutine" | "CoroutineType" | "BoxFuture" => Some(Wrap::Later),
        "Task" | "ValueTask" | "Deferred" if wraps => Some(Wrap::Later),
        "Iterator" | "IntoIterator" | "Generator" | "AsyncGenerator" | "AsyncIterator" | "AsyncIterable" | "IterableIterator" | "ExactSizeIterator" | "DoubleEndedIterator" => Some(Wrap::Many),
        "Stream" | "Observable" | "Iter" if wraps => Some(Wrap::Many),
        _ => None,
    }
}

fn is_later(name: &str) -> bool {
    wrapper(name, 1) == Some(Wrap::Later)
}

fn is_many(name: &str) -> bool {
    wrapper(name, 1) == Some(Wrap::Many)
}

fn is_none_word(t: &T) -> bool {
    matches!(t.last(), Some("None" | "null" | "undefined" | "void")) || matches!(t, T::Tuple(items) if items.is_empty())
}

/// Whether, and how, a type can fail.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) enum Fails {
    /// It cannot.
    #[default]
    Never,
    /// It can, and the type does not say with what (`Result<T>` of a crate's own alias).
    Untyped,
    /// It can, with this error type.
    With(T),
}

impl Fails {
    /// Whether it can fail at all.
    pub(super) const fn can(&self) -> bool {
        !matches!(self, Self::Never)
    }
}

/// What a type says once its wrappers are peeled off.
#[derive(Clone, Debug, Default)]
pub(super) struct Peeled {
    /// The inner type, when there is one.
    pub inner: Option<T>,
    /// It may be nothing.
    pub maybe: bool,
    /// It answers later.
    pub later: bool,
    /// It gives many.
    pub many: bool,
    /// It can fail.
    pub fails: Fails,
    /// The type as it stood when the last wrapper came off, borrow and all
    /// (`&str` for `Option<&str>`); none when nothing was wrapped.
    pub held: Option<T>,
}

/// Peels `Result`, `Option`, `Promise` and iterator wrappers, outermost
/// first, into what the type gives and the ways it can end.
pub(super) fn peel(t: &T) -> Peeled {
    let mut out = Peeled::default();
    let mut current = t.clone();
    for _ in 0..6 {
        match &current {
            T::Name { path, args } => {
                let name = path.last().map_or("", String::as_str);
                match wrapper(name, args.len()) {
                    Some(Wrap::Fails) => {
                        out.fails = args.get(1).cloned().map_or(Fails::Untyped, Fails::With);
                        current = args.first().cloned().unwrap_or(T::Tuple(Vec::new()));
                    }
                    Some(Wrap::Maybe) => {
                        out.maybe = true;
                        current = args[0].clone();
                    }
                    Some(Wrap::Later) => {
                        out.later = true;
                        current = args.first().cloned().unwrap_or(T::Tuple(Vec::new()));
                    }
                    Some(Wrap::Many) => {
                        out.many = true;
                        current = args.first().cloned().unwrap_or(T::Infer);
                    }
                    None if is_wrapper(name) && args.len() == 1 && matches!(name, "Pin" | "Box") && matches!(&args[0], T::Any(_)) => current = args[0].clone(),
                    None => break,
                }
                out.held = Some(current.clone());
            }
            T::Any(bounds) => {
                // `impl Iterator<Item = T>` / `impl Future<Output = T>`.
                let Some(T::Name { path, args }) = bounds.first() else { break };
                let name = path.last().map_or("", String::as_str);
                match wrapper(name, args.len().max(1)) {
                    Some(Wrap::Many) => {
                        out.many = true;
                        current = args.first().cloned().unwrap_or(T::Infer);
                    }
                    Some(Wrap::Later) => {
                        out.later = true;
                        current = args.first().cloned().unwrap_or(T::Tuple(Vec::new()));
                    }
                    _ => break,
                }
                out.held = Some(current.clone());
            }
            T::Union(items) if items.iter().any(is_none_word) && items.len() > 1 => {
                out.maybe = true;
                let rest: Vec<T> = items.iter().filter(|item| !is_none_word(item)).cloned().collect();
                current = if rest.len() == 1 { rest[0].clone() } else { T::Union(rest) };
                out.held = Some(current.clone());
            }
            T::Ref { inner, .. } => current = (**inner).clone(),
            _ => break,
        }
    }
    out.inner = Some(current);
    out
}

/// `an integer` as many: `integers`; a named type or `text` stays as it is.
fn many_of(word: &str) -> String {
    if word == "a function" {
        return "functions".to_owned();
    }
    match word.strip_prefix("a ").or_else(|| word.strip_prefix("an ")) {
        Some(rest) if !rest.contains(' ') && !word.starts_with("a list") => format!("{rest}s"),
        Some(rest) if rest == "big integer" || rest == "character" => format!("{rest}s"),
        _ => word.to_owned(),
    }
}

/// The plain words of a type.
pub(super) fn word(t: &T, cx: &Cx<'_>) -> String {
    match t {
        T::Ref { inner, .. } => word(inner, cx),
        T::Infer => "anything".to_owned(),
        T::Lit(text) => text.clone(),
        T::Tuple(items) if items.is_empty() => "nothing".to_owned(),
        T::Tuple(items) => items.iter().map(|item| word(item, cx)).collect::<Vec<_>>().join(" and "),
        T::Slice(inner) => {
            if matches!(&**inner, T::Name { path, .. } if path.last().is_some_and(|last| last == "u8")) {
                "bytes".to_owned()
            } else {
                format!("a list of {}", many_of(&word(inner, cx)))
            }
        }
        T::Func { .. } => "a function".to_owned(),
        T::Any(bounds) => bounds
            .first()
            .map_or_else(|| "anything".to_owned(), |bound| match bound {
                T::Name { path, .. } => format!("any {}", path.last().map_or("", String::as_str)),
                other => word(other, cx),
            }),
        T::Union(items) => {
            let mut words: Vec<String> = Vec::new();
            for item in items.iter().filter(|item| !is_none_word(item)) {
                let w = word(item, cx);
                if !words.contains(&w) {
                    words.push(w);
                }
            }
            words.join(" or ")
        }
        T::Object(_) => "options".to_owned(),
        T::Name { path, args } => {
            let name = path.last().map_or("", String::as_str);
            if cx.generics.iter().any(|g| g == name) && path.len() == 1 {
                return name.to_owned();
            }
            if name == "Self" && path.len() == 1 {
                return cx.owner.map_or_else(|| "this type".to_owned(), ToOwned::to_owned);
            }
            if name == "Vec" && matches!(args.first(), Some(T::Name { path, .. }) if path.last().is_some_and(|last| last == "u8")) {
                return "bytes".to_owned();
            }
            if let Some(prim) = primitive(name).filter(|_| args.is_empty() || name == "Cow") {
                return prim.to_owned();
            }
            if is_list(name) && !args.is_empty() {
                return format!("a list of {}", many_of(&word(&args[0], cx)));
            }
            if is_map(name) && args.len() >= 2 {
                return format!("a map of {} to {}", word(&args[0], cx), many_of(&word(&args[1], cx)));
            }
            if (is_wrapper(name) || is_maybe(name) || is_later(name) || is_many(name)) && !args.is_empty() {
                return word(&args[args.len() - 1], cx);
            }
            if name == "Result" {
                return args.first().map_or_else(|| "nothing".to_owned(), |ok| word(ok, cx));
            }
            name.to_owned()
        }
    }
}

/// The address the type names, when its head is a linked declaration.
fn link_of(t: &T, cx: &Cx<'_>) -> Option<String> {
    match t {
        T::Ref { inner, .. } => link_of(inner, cx),
        T::Name { path, args } => {
            let name = path.last()?;
            if primitive(name).is_some() || is_list(name) || is_map(name) {
                return None;
            }
            if name == "Self" {
                return cx.owner.and_then(|owner| (cx.link)(owner));
            }
            if is_wrapper(name) || is_maybe(name) || is_later(name) || is_many(name) || name == "Result" {
                return args.last().and_then(|arg| link_of(arg, cx));
            }
            (cx.link)(name)
        }
        T::Slice(inner) => link_of(inner, cx),
        _ => None,
    }
}

/// A type as the page draws it: the word, what is written, a link, and the
/// generic it is.
pub(super) fn ty_of(t: &T, written: &str, cx: &Cx<'_>) -> Ty {
    let word_text = word(t, cx);
    let generic = match t {
        T::Name { path, args } if path.len() == 1 && args.is_empty() && cx.generics.contains(&path[0]) => Some(path[0].clone()),
        T::Ref { inner, .. } => match &**inner {
            T::Name { path, args } if path.len() == 1 && args.is_empty() && cx.generics.contains(&path[0]) => Some(path[0].clone()),
            _ => None,
        },
        _ => None,
    };
    let written = squash(written);
    let written = (!written.is_empty() && written != word_text && generic.is_none()).then_some(written);
    Ty { word: word_text, written, link: link_of(t, cx), generic, origin: cx.origin, loops: false }
}

/// The name of the type at the head, for `Self`-like checks.
pub(super) fn head(text: &str) -> String {
    last_segment(text.trim_start_matches(['&', '*']).trim_start_matches("mut ").split(['<', '[', '(']).next().unwrap_or("")).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx<'a>(generics: &'a [String], link: &'a dyn Fn(&str) -> Option<String>) -> Cx<'a> {
        Cx { generics, owner: Some("Value"), link, origin: Origin::Declared }
    }

    fn says(text: &str) -> String {
        let generics = vec!["T".to_owned()];
        let link = |name: &str| (name == "Value").then(|| "addr:Value".to_owned());
        word(&parse(text), &cx(&generics, &link))
    }

    #[test]
    fn rust_types_read_in_plain_words() {
        assert_eq!(says("&'a str"), "text");
        assert_eq!(says("String"), "text");
        assert_eq!(says("Vec<Value>"), "a list of Value");
        assert_eq!(says("Map<String, Value>"), "a map of text to Value");
        assert_eq!(says("&[u8]"), "bytes");
        assert_eq!(says("usize"), "a count");
        assert_eq!(says("i64"), "an integer");
        assert_eq!(says("f64"), "a number");
        assert_eq!(says("bool"), "yes or no");
        assert_eq!(says("Box<dyn Error + Send>"), "any Error");
        assert_eq!(says("Self"), "Value");
        assert_eq!(says("T"), "T");
        assert_eq!(says("BTreeMap<S, V>"), "a map of S to V");
        assert_eq!(says("Cow<'a, str>"), "text");
    }

    #[test]
    fn typescript_and_python_annotations_read_the_same_way() {
        assert_eq!(says("string[]"), "a list of text");
        assert_eq!(says("Array<number>"), "a list of numbers");
        assert_eq!(says("Record<string, boolean>"), "a map of text to yes or no");
        assert_eq!(says("list[str]"), "a list of text");
        assert_eq!(says("dict[str, int]"), "a map of text to integers");
        assert_eq!(says("str | bytes"), "text or bytes");
        assert_eq!(says("(a: string) => void"), "a function");
        assert_eq!(says("unknown"), "anything");
    }

    #[test]
    fn wrappers_peel_into_outcomes() {
        let out = peel(&parse("Result<Option<Value>, Error>"));
        assert!(out.maybe && out.fails.can());
        assert_eq!(out.inner.and_then(|t| t.last().map(str::to_owned)).as_deref(), Some("Value"));
        let later = peel(&parse("Promise<string>"));
        assert!(later.later);
        let many = peel(&parse("impl Iterator<Item = &'a Value>"));
        assert!(many.many);
        assert_eq!(many.inner.and_then(|t| t.last().map(str::to_owned)).as_deref(), Some("Value"));
        let union = peel(&parse("Match | None"));
        assert!(union.maybe);
        let t = peel(&parse("Result<T>"));
        assert_eq!(t.fails, Fails::Untyped);
    }

    #[test]
    fn object_literals_keep_their_optional_fields() {
        let T::Object(fields) = parse("{ nothrow?: boolean, path: string }") else { panic!("an object") };
        assert_eq!(fields.len(), 2);
        assert!(fields[0].2 && !fields[1].2);
    }
}
