//! A language-agnostic tokenizer for declaration signatures.
//! Signatures arrive as one opaque string per declaration, in seven different
//! languages, and a reader needs the type names in them to be clickable.
//! This splits a signature into typed runs and marks which runs name a type.
//!
//! The tokenizer is deliberately shallow. It does not parse any language; it
//! recognises identifier runs, punctuation, strings and numbers, and then
//! classifies identifiers by three signals every one of the seven languages
//! shares: a shared keyword set, the position of `:` and `->`, and the case
//! convention for type names. A wrong guess costs one mis-tinted word; a
//! parser per language would cost seven parsers. Resolution to a real
//! declaration is separate and always best-effort: an unresolved type token is
//! drawn as text, never as a link that goes nowhere.

use backend_library::SymbolKey;

/// What a run of signature text is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    /// A language keyword: `fn`, `pub`, `class`, `func`, `def`, `static`.
    Keyword,
    /// The declared name of the thing this signature declares.
    Name,
    /// A type name.
    Type,
    /// A parameter or field name.
    Parameter,
    /// A string or numeric literal.
    Literal,
    /// A lifetime, generic marker, or attribute sigil.
    Marker,
    /// Brackets, commas, arrows, and other separators.
    Punctuation,
    /// Whitespace.
    Space,
    /// An identifier this pass could not classify.
    Plain,
}

/// Whether a type token points at something the shelf actually holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Resolved {
    /// The token resolves to a declaration that can be opened.
    Declaration(SymbolKey),
    /// The token names a type this shelf has no declaration for.
    Unresolved,
    /// The token is not a type and was never looked up.
    NotApplicable,
}

/// One run of signature text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    text: String,
    kind: TokenKind,
    resolved: Resolved,
}

impl Token {
    /// Returns the exact text of this run.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns what kind of run this is.
    pub(crate) const fn kind(&self) -> TokenKind {
        self.kind
    }

    /// Returns whether this run links to a declaration.
    pub(crate) const fn resolved(&self) -> Resolved {
        self.resolved
    }

    /// Returns the declaration this run opens, when it has one.
    pub(crate) const fn target(&self) -> Option<SymbolKey> {
        match self.resolved {
            Resolved::Declaration(symbol) => Some(symbol),
            Resolved::Unresolved | Resolved::NotApplicable => None,
        }
    }
}

/// A tokenized signature.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Signature {
    tokens: Vec<Token>,
    text: String,
}

impl Signature {
    /// Tokenizes one signature string.
    pub(crate) fn parse(text: &str, declared_name: &str) -> Self {
        let runs = split_runs(text);
        let tokens = classify(runs, declared_name);
        Self {
            tokens,
            text: text.to_owned(),
        }
    }

    /// Resolves every type token through a lookup over declaration names.
    pub(crate) fn resolve_with(mut self, lookup: &impl Fn(&str) -> Option<SymbolKey>) -> Self {
        for token in &mut self.tokens {
            if token.kind == TokenKind::Type {
                token.resolved = lookup(&token.text)
                    .map_or(Resolved::Unresolved, Resolved::Declaration);
            }
        }
        self
    }

    /// Returns the typed runs in order.
    pub(crate) fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Returns the exact original text, which is what gets copied.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns whether this signature has any content.
    pub(crate) fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Returns a single-line preview clipped to a character budget.
    pub(crate) fn preview(&self, budget: usize) -> String {
        let flattened: String = self
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if flattened.chars().count() <= budget {
            return flattened;
        }
        let kept: String = flattened.chars().take(budget.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Keywords shared across the seven supported languages.
const KEYWORDS: [&str; 62] = [
    "abstract", "async", "await", "case", "class", "const", "constructor", "def", "default",
    "delegate", "dyn", "elif", "else", "enum", "event", "export", "extends", "extern", "final",
    "fn", "for", "friend", "func", "function", "get", "impl", "implements", "import", "in",
    "inline", "interface", "internal", "let", "map", "mod", "mut", "namespace", "new",
    "operator", "override", "package", "private", "protected", "public", "pub", "readonly",
    "record", "ref", "return", "sealed", "set", "static", "struct", "template", "trait", "type",
    "typedef", "union", "unsafe", "using", "var", "where",
];

/// Primitive and near-universal type names.
const PRIMITIVES: [&str; 34] = [
    "bool", "boolean", "byte", "char", "decimal", "double", "float", "f32", "f64", "i8", "i16",
    "i32", "i64", "i128", "int", "int8", "int16", "int32", "int64", "isize", "long", "object",
    "rune", "short", "str", "string", "u8", "u16", "u32", "u64", "u128", "uint", "usize", "void",
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Run {
    text: String,
    kind: TokenKind,
}

fn split_runs(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(head) = characters.next() {
        let run = if head.is_whitespace() {
            take_while(head, &mut characters, TokenKind::Space, char::is_whitespace)
        } else if head == '"' || head == '\'' {
            take_literal(head, &mut characters)
        } else if head.is_ascii_digit() {
            take_while(head, &mut characters, TokenKind::Literal, |next| {
                next.is_ascii_alphanumeric() || next == '.' || next == '_'
            })
        } else if is_identifier_start(head) {
            take_while(head, &mut characters, TokenKind::Plain, is_identifier_part)
        } else {
            Run {
                text: head.to_string(),
                kind: TokenKind::Punctuation,
            }
        };
        runs.push(run);
    }
    runs
}

fn take_while(
    head: char,
    characters: &mut core::iter::Peekable<core::str::Chars<'_>>,
    kind: TokenKind,
    accept: impl Fn(char) -> bool,
) -> Run {
    let mut text = head.to_string();
    while let Some(next) = characters.peek().copied() {
        if !accept(next) {
            break;
        }
        text.push(next);
        let _ = characters.next();
    }
    Run { text, kind }
}

fn take_literal(quote: char, characters: &mut core::iter::Peekable<core::str::Chars<'_>>) -> Run {
    let mut text = quote.to_string();
    for next in characters.by_ref() {
        text.push(next);
        if next == quote {
            break;
        }
    }
    Run {
        text,
        kind: TokenKind::Literal,
    }
}

const fn is_identifier_start(value: char) -> bool {
    value.is_ascii_alphabetic() || value == '_' || value == '$'
}

const fn is_identifier_part(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_' || value == '$'
}

fn classify(runs: Vec<Run>, declared_name: &str) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(runs.len());
    let mut named = false;
    for (at, run) in runs.iter().enumerate() {
        let kind = classify_run(&runs, at, run, declared_name, &mut named);
        tokens.push(Token {
            text: run.text.clone(),
            kind,
            resolved: Resolved::NotApplicable,
        });
    }
    tokens
}

fn classify_run(
    runs: &[Run],
    at: usize,
    run: &Run,
    declared_name: &str,
    named: &mut bool,
) -> TokenKind {
    if run.kind != TokenKind::Plain {
        return run.kind;
    }
    if KEYWORDS.contains(&run.text.as_str()) {
        return TokenKind::Keyword;
    }
    if !*named && !declared_name.is_empty() && run.text == declared_name {
        *named = true;
        return TokenKind::Name;
    }
    if PRIMITIVES.contains(&run.text.as_str()) {
        return TokenKind::Type;
    }
    if follows_type_position(runs, at) {
        return TokenKind::Type;
    }
    if precedes_annotation(runs, at) {
        return TokenKind::Parameter;
    }
    if starts_upper(&run.text) {
        return TokenKind::Type;
    }
    TokenKind::Plain
}

/// Returns whether the run at `at` sits where a type is written.
fn follows_type_position(runs: &[Run], at: usize) -> bool {
    matches!(
        previous_symbol(runs, at).as_deref(),
        Some(":" | ">" | "&" | "*")
    ) || matches!(previous_two(runs, at).as_deref(), Some("->" | "=>" | "::"))
}

/// Returns whether the run at `at` is followed by a type annotation.
fn precedes_annotation(runs: &[Run], at: usize) -> bool {
    matches!(next_symbol(runs, at).as_deref(), Some(":"))
}

fn previous_symbol(runs: &[Run], at: usize) -> Option<String> {
    runs.get(..at)?
        .iter()
        .rev()
        .find(|run| run.kind != TokenKind::Space)
        .map(|run| run.text.clone())
}

fn previous_two(runs: &[Run], at: usize) -> Option<String> {
    let significant: Vec<&Run> = runs
        .get(..at)?
        .iter()
        .rev()
        .filter(|run| run.kind != TokenKind::Space)
        .take(2)
        .collect();
    let (first, second) = (significant.first()?, significant.get(1)?);
    Some(format!("{}{}", second.text, first.text))
}

fn next_symbol(runs: &[Run], at: usize) -> Option<String> {
    runs.get(at.saturating_add(1)..)?
        .iter()
        .find(|run| run.kind != TokenKind::Space)
        .map(|run| run.text.clone())
}

fn starts_upper(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_uppercase)
}
